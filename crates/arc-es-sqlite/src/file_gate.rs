//! Fair in-process admission to one SQLite database file.
//!
//! SQLite arbitrates a database file by polling: a connection that finds the
//! file locked sleeps and retries, and whichever polls first after a commit
//! wins. That is not fair. Under sustained contention one caller can lose
//! every retry until its busy timeout expires and report `database is
//! locked`, although nobody ever held the file for long. It happens to
//! writers racing each other for the write lock, and, in rollback-journal
//! mode, to a reader polling against one commit after another.
//!
//! The stores of this crate therefore follow SQLite's own protocol among
//! themselves before they enter it, with queues instead of polls:
//!
//! - one writer at a time holds the *write slot*, granted first-in,
//!   first-out, from before `BEGIN IMMEDIATE` until after commit or rollback
//!   (SQLite's `RESERVED` lock);
//! - readers run together, also beside a writer that has not reached commit
//!   (`SHARED`);
//! - to commit, the writer stops admitting readers and waits for the running
//!   ones to finish (`PENDING`, `EXCLUSIVE`); readers that arrive meanwhile
//!   are all admitted when the commit ends, before the next one can begin.
//!
//! So nobody is overtaken indefinitely: a writer waits for the writers that
//! arrived before it, a commit for the reads already running, a read for at
//! most one commit. Within the process the stores never meet inside SQLite's
//! lock protocol at all. The gate is shared by everything in the process that
//! uses the same file, whichever store, pool or spelling of the path it came
//! from: it is found through the file name SQLite reports for the connection.
//!
//! Limits, by design:
//!
//! - Only this crate's stores take part. Another process, or a connection
//!   opened outside these stores, still competes through SQLite's busy
//!   handler, so the busy timeout stays the bound for that contention.
//! - In-memory and temporary databases have no file name and are not gated.
//! - A permit is taken on the blocking thread and released on it; it is never
//!   held across an `await`. An operation nested inside another on the same
//!   thread passes without queueing.
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::sql_types::{Integer, Nullable, Text};
use diesel::SqliteConnection;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::Thread;
use std::time::{Duration, Instant};

/// SQLite's text for `SQLITE_BUSY`, as Diesel reports it.
pub(crate) const LOCKED: &str = "database is locked";

struct Waiter {
    thread: Thread,
    granted: AtomicBool,
}

impl Waiter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            thread: std::thread::current(),
            granted: AtomicBool::new(false),
        })
    }

    /// `park` may wake spuriously; only the flag grants.
    fn wait(&self) {
        while !self.granted.load(Ordering::Acquire) {
            std::thread::park();
        }
    }

    fn grant(&self) {
        self.granted.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

#[derive(Default)]
struct State {
    /// The write slot is taken.
    writer: bool,
    /// Writers waiting for the slot, in arrival order.
    writers: VecDeque<Arc<Waiter>>,
    /// Reads running.
    readers: usize,
    /// The writer is committing, or waiting to: no new reads start.
    committing: bool,
    /// The writer waiting for the running reads to finish.
    draining: Option<Arc<Waiter>>,
    /// Reads waiting for the commit to end.
    parked: Vec<Arc<Waiter>>,
    /// Start and end of the latest write that SQLite refused as locked.
    refused: Option<(Instant, Instant)>,
}

/// The queues described in the module documentation. `std::sync` locks
/// promise no order, and a barging lock would only move the starvation here.
#[derive(Default)]
struct Gate {
    state: Mutex<State>,
}

impl Gate {
    /// The critical sections below cannot panic, and a holder that panics
    /// releases on unwind, so there is nothing a poisoned flag could protect:
    /// ignore it rather than fail every later caller.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin_read(&self) {
        let waiter = {
            let mut state = self.state();
            if !state.committing {
                state.readers += 1;
                return;
            }
            let waiter = Waiter::new();
            state.parked.push(waiter.clone());
            waiter
        };
        waiter.wait();
    }

    fn end_read(&self) {
        let mut state = self.state();
        state.readers -= 1;
        if state.readers == 0 {
            if let Some(writer) = state.draining.take() {
                writer.grant();
            }
        }
    }

    /// Returns when the caller started queueing, if it had to.
    fn begin_write(&self) -> Option<Instant> {
        let queued = Instant::now();
        let waiter = {
            let mut state = self.state();
            if !state.writer {
                state.writer = true;
                return None;
            }
            let waiter = Waiter::new();
            state.writers.push_back(waiter.clone());
            waiter
        };
        waiter.wait();
        Some(queued)
    }

    /// For the holder of the write slot, before it commits.
    fn exclude_readers(&self) {
        let waiter = {
            let mut state = self.state();
            state.committing = true;
            if state.readers == 0 {
                return;
            }
            let waiter = Waiter::new();
            state.draining = Some(waiter.clone());
            waiter
        };
        waiter.wait();
    }

    fn end_write(&self) {
        let mut state = self.state();
        state.committing = false;
        // Every read that waited out this commit runs before the next one.
        state.readers += state.parked.len();
        for reader in state.parked.drain(..) {
            reader.grant();
        }
        // The slot passes on directly, in arrival order.
        match state.writers.pop_front() {
            Some(next) => next.grant(),
            None => state.writer = false,
        }
    }
}

fn registry() -> MutexGuard<'static, HashMap<PathBuf, Weak<Gate>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Gate>>>> = OnceLock::new();
    REGISTRY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn gate_for(file: PathBuf) -> Arc<Gate> {
    let mut gates = registry();
    if let Some(gate) = gates.get(&file).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Gate::default());
    gates.insert(file, Arc::downgrade(&gate));
    gate
}

thread_local! {
    /// Gates this thread is inside, so an operation that calls another for
    /// the same file passes straight through instead of waiting for itself.
    static INSIDE: RefCell<Vec<*const Gate>> = const { RefCell::new(Vec::new()) };
}

struct Target {
    gate: Arc<Gate>,
    busy_timeout: Duration,
}

#[derive(QueryableByName)]
struct Attached {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Nullable<Text>)]
    file: Option<String>,
}

#[derive(QueryableByName)]
struct Timeout {
    #[diesel(sql_type = Integer)]
    timeout: i32,
}

/// Ask the connection which file it uses. SQLite reports an absolute name
/// with symbolic links resolved, whatever spelling, relative path or `file:`
/// URI opened it, and an empty name for in-memory and temporary databases.
/// `Err` means the question could not be asked; `Ok(None)` means no file.
fn describe(conn: &mut SqliteConnection) -> QueryResult<Option<Target>> {
    let file = diesel::sql_query("PRAGMA database_list")
        .load::<Attached>(conn)?
        .into_iter()
        .find(|database| database.name == "main")
        .and_then(|database| database.file)
        .filter(|file| !file.is_empty());
    let Some(file) = file else {
        return Ok(None);
    };
    let busy_timeout = diesel::sql_query("PRAGMA busy_timeout")
        .get_result::<Timeout>(conn)?
        .timeout;
    let file = PathBuf::from(file);
    Ok(Some(Target {
        gate: gate_for(std::fs::canonicalize(&file).unwrap_or(file)),
        busy_timeout: Duration::from_millis(busy_timeout.max(0) as u64),
    }))
}

type ConnectionPool = Pool<ConnectionManager<SqliteConnection>>;

/// A store's handle on the gate of its database file. The file is resolved
/// once, by the store's first operation, and kept for the life of the store.
#[derive(Default)]
pub(crate) struct FileGate {
    target: OnceLock<Option<Target>>,
}

/// Returned instead of an attempt when another connection has kept the
/// database locked for this writer's whole busy timeout while it queued.
#[derive(Debug)]
pub(crate) struct ExternallyLocked;

impl Display for ExternallyLocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(LOCKED)
    }
}

impl FileGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Admit a read on `conn`, and keep the permit until the read's last
    /// statement has finished. The reader takes its connection first: one
    /// that still waited for a pooled connection while counted as running
    /// could be waiting for the very writer whose commit waits for it.
    pub(crate) fn read(&self, conn: &mut SqliteConnection) -> ReadPermit {
        if self.target.get().is_none() {
            self.resolve(conn);
        }
        let gate = self.target.get().and_then(|target| target.as_ref());
        let Some(gate) = gate.map(|target| &target.gate).filter(|gate| !inside(gate)) else {
            return ReadPermit(None, PhantomData);
        };
        gate.begin_read();
        enter(gate);
        ReadPermit(Some(gate.clone()), PhantomData)
    }

    /// Queue for the write slot of the database file behind `pool`. Call it
    /// on the blocking thread before taking the connection the write will
    /// use, so a queued writer does not keep a pooled connection from others.
    pub(crate) fn write(&self, pool: &ConnectionPool) -> Result<WritePermit, ExternallyLocked> {
        if self.target.get().is_none() {
            // If no connection can be had, the operation itself reports that.
            // The connection goes back to the pool before queueing.
            if let Ok(mut conn) = pool.get() {
                self.resolve(&mut conn);
            }
        }
        self.admit_writer()
    }

    /// As [`write`](Self::write), for a writer that owns its connection.
    pub(crate) fn write_on(
        &self,
        conn: &mut SqliteConnection,
    ) -> Result<WritePermit, ExternallyLocked> {
        if self.target.get().is_none() {
            self.resolve(conn);
        }
        self.admit_writer()
    }

    /// If the connection cannot be asked, this operation runs ungated and
    /// the next one asks again.
    fn resolve(&self, conn: &mut SqliteConnection) {
        if let Ok(target) = describe(conn) {
            let _ = self.target.set(target);
        }
    }

    fn admit_writer(&self) -> Result<WritePermit, ExternallyLocked> {
        let Some(Some(target)) = self.target.get() else {
            return Ok(WritePermit::ungated());
        };
        if inside(&target.gate) {
            return Ok(WritePermit::ungated());
        }
        if let Some(queued) = target.gate.begin_write() {
            // Queueing must not multiply the bounded wait for a lock held
            // outside this process. If a writer ahead spent its whole busy
            // timeout on such a lock while this one queued behind it, this
            // one has waited out the same lock for as long and reports it,
            // instead of starting a second full timeout of its own.
            let refused = target.gate.state().refused;
            if let Some((start, end)) = refused {
                let waited = end.saturating_duration_since(start.max(queued));
                let bound = target.busy_timeout - target.busy_timeout / 10;
                if end > queued && waited >= bound {
                    target.gate.end_write();
                    return Err(ExternallyLocked);
                }
            }
        }
        enter(&target.gate);
        Ok(WritePermit {
            held: Some((target.gate.clone(), Instant::now())),
            _on_this_thread: PhantomData,
        })
    }
}

fn inside(gate: &Arc<Gate>) -> bool {
    INSIDE.with(|inside| inside.borrow().contains(&Arc::as_ptr(gate)))
}

fn enter(gate: &Arc<Gate>) {
    INSIDE.with(|inside| inside.borrow_mut().push(Arc::as_ptr(gate)));
}

fn leave(gate: &Arc<Gate>) {
    INSIDE.with(|inside| {
        inside
            .borrow_mut()
            .retain(|held| *held != Arc::as_ptr(gate))
    });
}

/// Admission of one read. Dropping it, on any path including a panic, ends
/// the read; it cannot leave the thread that obtained it.
pub(crate) struct ReadPermit(Option<Arc<Gate>>, PhantomData<*const ()>);

impl Drop for ReadPermit {
    fn drop(&mut self) {
        if let Some(gate) = self.0.take() {
            leave(&gate);
            gate.end_read();
        }
    }
}

/// The write slot. Dropping it, on any path including a panic, passes the
/// slot on; it cannot leave the thread that obtained it.
pub(crate) struct WritePermit {
    held: Option<(Arc<Gate>, Instant)>,
    _on_this_thread: PhantomData<*const ()>,
}

impl WritePermit {
    fn ungated() -> Self {
        Self {
            held: None,
            _on_this_thread: PhantomData,
        }
    }

    /// Call before `COMMIT`, or before a statement that commits by itself:
    /// stop admitting this process's readers and wait for the running ones,
    /// so the commit does not wait for them inside SQLite and they do not
    /// poll for the file against it.
    pub(crate) fn exclude_readers(&self) {
        if let Some((gate, _)) = &self.held {
            gate.exclude_readers();
        }
    }

    /// Release after the write has committed or rolled back, recording
    /// whether SQLite refused it as locked (see `FileGate::admit_writer`).
    pub(crate) fn finish<T, E: Display>(self, outcome: &Result<T, E>) {
        if let Some((gate, since)) = &self.held {
            let refused = match outcome {
                Ok(_) => None,
                Err(error) if error.to_string().contains(LOCKED) => Some((*since, Instant::now())),
                Err(_) => return,
            };
            gate.state().refused = refused;
        }
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        if let Some((gate, _)) = self.held.take() {
            leave(&gate);
            gate.end_write();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;
    use std::sync::mpsc;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("arc-gate-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn pool(&self) -> ConnectionPool {
            pool(self.0.join("store.db").to_str().unwrap(), 200)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pool(url: &str, busy_timeout_ms: u32) -> ConnectionPool {
        #[derive(Debug)]
        struct Busy(u32);
        impl diesel::r2d2::CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for Busy {
            fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
                conn.batch_execute(&format!("PRAGMA busy_timeout = {}", self.0))
                    .map_err(diesel::r2d2::Error::QueryError)
            }
        }
        Pool::builder()
            .max_size(2)
            .connection_customizer(Box::new(Busy(busy_timeout_ms)))
            .build(ConnectionManager::new(url))
            .unwrap()
    }

    fn gate_of(gate: &FileGate) -> Arc<Gate> {
        gate.target.get().unwrap().as_ref().unwrap().gate.clone()
    }

    /// Spin until the gate's state satisfies `reached`.
    fn until(gate: &Gate, reached: impl Fn(&State) -> bool) {
        while !reached(&gate.state()) {
            std::thread::yield_now();
        }
    }

    #[test]
    fn the_write_slot_is_granted_in_arrival_order() {
        let gate = Arc::new(Gate::default());
        assert!(gate.begin_write().is_none());
        let (order_tx, order_rx) = mpsc::channel();
        let mut threads = Vec::new();
        for n in 0..16 {
            let (waiting, order_tx) = (gate.clone(), order_tx.clone());
            threads.push(std::thread::spawn(move || {
                assert!(waiting.begin_write().is_some());
                order_tx.send(n).unwrap();
                waiting.end_write();
            }));
            // Arrival is the moment the waiter is in the queue.
            until(&gate, |state| state.writers.len() > n);
        }
        gate.end_write();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            order_rx.try_iter().collect::<Vec<_>>(),
            (0..16).collect::<Vec<_>>()
        );
        assert!(!gate.state().writer);
    }

    #[test]
    fn reads_run_beside_a_writer_until_it_commits_and_wait_out_one_commit() {
        let gate = Arc::new(Gate::default());
        // Reads run together, and beside a writer that has not reached commit.
        gate.begin_read();
        assert!(gate.begin_write().is_none());
        gate.begin_read();
        assert_eq!(gate.state().readers, 2);

        // The commit waits for both running reads.
        let committing = gate.clone();
        let (committed_tx, committed_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            committing.exclude_readers();
            committed_tx.send(committing.state().readers).unwrap();
        });
        until(&gate, |state| state.draining.is_some());

        // Reads arriving now wait for the commit to end, however many.
        let late: Vec<_> = (0..3)
            .map(|_| {
                let reading = gate.clone();
                std::thread::spawn(move || {
                    reading.begin_read();
                    let admitted_with = reading.state().readers;
                    std::thread::sleep(Duration::from_millis(50));
                    reading.end_read();
                    admitted_with
                })
            })
            .collect();
        until(&gate, |state| state.parked.len() == 3);
        gate.end_read();
        assert!(committed_rx.try_recv().is_err(), "one read still runs");
        gate.end_read();
        assert_eq!(
            committed_rx.recv().unwrap(),
            0,
            "commit began with no reads"
        );
        writer.join().unwrap();
        assert_eq!(gate.state().readers, 0, "late reads still wait");

        // A second writer is queued for the slot. When the first ends, all
        // waiting reads start at once, and the second writer's commit then
        // waits for them: no read waits out more than one commit.
        let second = gate.clone();
        let (second_tx, second_rx) = mpsc::channel();
        let second_writer = std::thread::spawn(move || {
            assert!(second.begin_write().is_some());
            second.exclude_readers();
            second_tx.send(second.state().readers).unwrap();
            second.end_write();
        });
        until(&gate, |state| state.writers.len() == 1);
        gate.end_write();
        for reader in late {
            assert_eq!(reader.join().unwrap(), 3, "admitted together");
        }
        assert_eq!(second_rx.recv().unwrap(), 0);
        second_writer.join().unwrap();
        let state = gate.state();
        assert!(!state.writer && !state.committing && state.readers == 0);
        assert!(state.writers.is_empty() && state.parked.is_empty());
    }

    #[test]
    fn every_spelling_of_one_file_shares_one_gate() {
        let dir = Dir::new();
        let file = dir.0.join("store.db");
        let plain = file.to_str().unwrap().to_string();
        let dotted = dir.0.join(".").join("store.db");
        let uri = format!("file:{plain}?mode=rwc");
        #[cfg(unix)]
        let linked = {
            let link = dir.0.join("link.db");
            // The link exists before the database file does.
            std::os::unix::fs::symlink(&file, &link).unwrap();
            link
        };
        let gates: Vec<_> = [
            plain.as_str(),
            dotted.to_str().unwrap(),
            uri.as_str(),
            #[cfg(unix)]
            linked.to_str().unwrap(),
        ]
        .iter()
        .map(|url| {
            let gate = FileGate::new();
            drop(gate.write(&pool(url, 100)).unwrap());
            gate
        })
        .collect();
        for gate in &gates {
            assert!(Arc::ptr_eq(&gate_of(gate), &gate_of(&gates[0])));
        }

        // A store that only ever reads finds the same gate.
        let reader = FileGate::new();
        drop(reader.read(&mut pool(&plain, 100).get().unwrap()));
        assert!(Arc::ptr_eq(&gate_of(&reader), &gate_of(&gates[0])));

        let other = FileGate::new();
        drop(
            other
                .write(&pool(dir.0.join("other.db").to_str().unwrap(), 100))
                .unwrap(),
        );
        assert!(!Arc::ptr_eq(&gate_of(&other), &gate_of(&gates[0])));
    }

    #[test]
    fn databases_without_a_file_are_not_gated() {
        for url in [":memory:", "", "file::memory:?cache=shared"] {
            let gate = FileGate::new();
            let pool = pool(url, 100);
            assert!(gate.write(&pool).unwrap().held.is_none(), "{url}");
            assert!(gate.read(&mut pool.get().unwrap()).0.is_none(), "{url}");
            assert!(gate.target.get().unwrap().is_none(), "{url}");
        }
    }

    #[test]
    fn an_operation_nested_on_one_thread_passes_and_keeps_the_outer_permit() {
        let dir = Dir::new();
        let pool = dir.pool();
        let (outer_gate, inner_gate) = (FileGate::new(), FileGate::new());

        let outer = outer_gate.write(&pool).unwrap();
        let inner = inner_gate.write(&pool).unwrap();
        assert!(outer.held.is_some() && inner.held.is_none());
        inner.exclude_readers();
        let reading = inner_gate.read(&mut pool.get().unwrap());
        assert!(reading.0.is_none());
        drop((inner, reading));
        let gate = gate_of(&outer_gate);
        assert!(gate.state().writer && !gate.state().committing);
        drop(outer);
        assert!(!gate.state().writer);

        // A write nested in a read cannot wait for its own thread either.
        let outer = outer_gate.read(&mut pool.get().unwrap());
        let inner = inner_gate.write(&pool).unwrap();
        assert!(outer.0.is_some() && inner.held.is_none());
        inner.exclude_readers();
        drop((inner, outer));
        let state = gate.state();
        assert!(!state.writer && state.readers == 0);
    }

    #[test]
    fn a_panicking_holder_releases_its_permit() {
        let dir = Dir::new();
        let pool = dir.pool();
        let gate = FileGate::new();
        for write in [true, false] {
            let (panicking_gate, panicking_pool) = (gate.clone(), pool.clone());
            let panicked = std::thread::spawn(move || {
                let _writing = write.then(|| {
                    let permit = panicking_gate.write(&panicking_pool).unwrap();
                    permit.exclude_readers();
                    permit
                });
                let _reading =
                    (!write).then(|| panicking_gate.read(&mut panicking_pool.get().unwrap()));
                panic!("failed while admitted");
            })
            .join();
            assert!(panicked.is_err());
            let state = gate_of(&gate);
            let state = state.state();
            assert!(!state.writer && !state.committing && state.readers == 0);
        }
        assert!(gate.write(&pool).unwrap().held.is_some());
    }

    #[test]
    fn a_writer_queued_through_a_whole_refused_attempt_reports_the_lock() {
        let dir = Dir::new();
        let pool = dir.pool();
        let file_gate = FileGate::new();
        drop(file_gate.write(&pool).unwrap());
        let gate = gate_of(&file_gate);
        let queue = |file_gate: &Arc<FileGate>| {
            let (file_gate, pool) = (file_gate.clone(), pool.clone());
            let queued = std::thread::spawn(move || file_gate.write(&pool).map(drop));
            until(&gate, |state| !state.writers.is_empty());
            queued
        };

        // Queued for the holder's whole refused attempt: same lock, same wait.
        let holder = file_gate.write(&pool).unwrap();
        let queued = queue(&file_gate);
        std::thread::sleep(Duration::from_millis(200));
        holder.finish(&Err::<(), _>(LOCKED));
        assert!(queued.join().unwrap().is_err());
        assert!(!gate.state().writer);

        // Arrived late in a refused attempt: it gets its own full attempt.
        let holder = file_gate.write(&pool).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let late = queue(&file_gate);
        holder.finish(&Err::<(), _>(LOCKED));
        assert!(late.join().unwrap().is_ok());

        // A later success clears the record; other errors do not set it.
        file_gate.write(&pool).unwrap().finish(&Ok::<(), &str>(()));
        assert!(gate.state().refused.is_none());
        file_gate
            .write(&pool)
            .unwrap()
            .finish(&Err::<(), _>("constraint failed"));
        assert!(gate.state().refused.is_none());
    }
}
