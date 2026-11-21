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
//! - to commit, the writer stops admitting readers and gives the running
//!   ones a short grace to finish (`PENDING`, `EXCLUSIVE`); readers that
//!   arrive meanwhile are all admitted when the commit ends, before the next
//!   one can begin.
//!
//! So nobody is overtaken indefinitely: a writer waits for the writers that
//! arrived before it, a read for at most one commit. The gate is shared by
//! everything in the process that uses the same file, whichever store, pool
//! or spelling of the path it came from: it is found through the file name
//! SQLite reports for the connection.
//!
//! The gate only orders callers; it never replaces SQLite's locks, and where
//! it cannot know what SQLite will do it steps aside:
//!
//! - The wait for running readers is bounded ([`DRAIN_GRACE`]). A reader may
//!   itself be waiting inside SQLite for this writer, which takes the
//!   exclusive lock early when its transaction outgrows the page cache; and
//!   a long read is not worth queueing behind. After the grace the writer
//!   commits and SQLite arbitrates, as it would without the gate.
//! - In WAL mode readers and writers do not block each other, so readers are
//!   not gated and commits do not wait for them; writers still take turns.
//!   The journal mode is read once, by a store's first operation. A database
//!   switched to WAL afterwards keeps its readers gated until the store is
//!   reopened (slower, not wrong); SQLite does not let a database leave WAL
//!   while other connections are open.
//! - Only this crate's stores take part. Another process, or a connection
//!   opened outside these stores, still competes through SQLite's busy
//!   handler, so the busy timeout stays the bound for that contention. Time a
//!   writer spent queued behind attempts that such a lock refused is taken
//!   off the busy timeout of its own attempt at the write lock, which it
//!   always makes; its commit keeps the full timeout.
//! - In-memory and temporary databases have no file name and are not gated.
//! - A permit is taken on the blocking thread and released on it; it is never
//!   held across an `await`. An operation nested inside another on the same
//!   thread passes without queueing.
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::sql_types::{Nullable, Text};
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

/// How long a commit waits for this process's running reads before it goes
/// ahead and leaves the rest to SQLite. Store reads take well under this; a
/// read still running after it is either long, or stuck behind this very
/// writer inside SQLite.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_millis(20);

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

    /// False if `grace` passed first.
    fn wait_for(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.granted.load(Ordering::Acquire) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            std::thread::park_timeout(left);
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
    /// When the holder of the write slot got it.
    attempt_since: Option<Instant>,
    /// Total length of the attempts SQLite refused as locked. Attempts hold
    /// the slot one at a time, so this is time, not overlapping intervals.
    refused: Duration,
}

impl State {
    /// What a writer arriving at `now` notes, to learn at its turn how long
    /// it queued behind refused attempts (see [`State::queued_behind_refusals`]).
    /// The part of the running attempt that is already over is not its wait.
    fn refusals_before(&self, now: Instant) -> Duration {
        let running = self
            .attempt_since
            .map(|since| now.saturating_duration_since(since));
        self.refused + running.unwrap_or_default()
    }

    /// The time a writer that noted `before` on arrival has since spent
    /// queued behind attempts refused as locked. Never more than it waited.
    fn queued_behind_refusals(&self, before: Duration) -> Duration {
        self.refused.saturating_sub(before)
    }

    fn record_refusal(&mut self, now: Instant) {
        if let Some(since) = self.attempt_since {
            self.refused += now.saturating_duration_since(since);
        }
    }
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

    /// Returns how long the caller queued behind attempts refused as locked.
    fn begin_write(&self) -> Duration {
        let (waiter, before) = {
            let mut state = self.state();
            let now = Instant::now();
            if !state.writer {
                state.writer = true;
                state.attempt_since = Some(now);
                return Duration::ZERO;
            }
            let waiter = Waiter::new();
            state.writers.push_back(waiter.clone());
            (waiter, state.refusals_before(now))
        };
        waiter.wait();
        self.state().queued_behind_refusals(before)
    }

    /// For the holder of the write slot, before it commits: stop admitting
    /// readers and give the running ones `grace` to finish.
    fn exclude_readers(&self, grace: Duration) {
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
        if !waiter.wait_for(grace) {
            self.state().draining = None;
        }
    }

    fn end_write(&self) {
        let mut state = self.state();
        state.committing = false;
        state.draining = None;
        // Every read that waited out this commit runs before the next one.
        state.readers += state.parked.len();
        for reader in state.parked.drain(..) {
            reader.grant();
        }
        // The slot passes on directly, in arrival order.
        match state.writers.pop_front() {
            Some(next) => {
                state.attempt_since = Some(Instant::now());
                next.grant();
            }
            None => {
                state.writer = false;
                state.attempt_since = None;
            }
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
    /// False in WAL mode, where readers and writers do not block each other.
    readers_gated: bool,
}

#[derive(QueryableByName)]
struct Attached {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Nullable<Text>)]
    file: Option<String>,
}

#[derive(QueryableByName)]
struct JournalMode {
    #[diesel(sql_type = Text)]
    journal_mode: String,
}

/// Ask the connection which file it uses and how. SQLite reports an absolute
/// name with symbolic links resolved, whatever spelling, relative path or
/// `file:` URI opened it, and an empty name for in-memory and temporary
/// databases. `Err` means the question could not be asked; `Ok(None)` means
/// no file.
///
/// The file name needs no lock, but the journal mode does: on a new
/// connection it is the first statement that loads the schema, which takes a
/// shared lock on the file. Asked outside the queue, it would poll for that
/// lock against one commit after another and could lose every time until the
/// busy timeout, so it is asked as a read of the file, under the permit
/// returned with the answer.
fn describe(conn: &mut SqliteConnection) -> QueryResult<Option<(Target, ReadPermit)>> {
    let file = diesel::sql_query("PRAGMA database_list")
        .load::<Attached>(conn)?
        .into_iter()
        .find(|database| database.name == "main")
        .and_then(|database| database.file)
        .filter(|file| !file.is_empty());
    let Some(file) = file else {
        return Ok(None);
    };
    let file = PathBuf::from(file);
    let gate = gate_for(std::fs::canonicalize(&file).unwrap_or(file));
    let probing = admit_reader(&gate);
    let journal_mode = diesel::sql_query("PRAGMA journal_mode")
        .get_result::<JournalMode>(conn)?
        .journal_mode;
    let target = Target {
        gate,
        readers_gated: !journal_mode.eq_ignore_ascii_case("wal"),
    };
    Ok(Some((target, probing)))
}

type ConnectionPool = Pool<ConnectionManager<SqliteConnection>>;

/// A store's handle on the gate of its database file. The file and its
/// journal mode are resolved once, by the store's first operation, and kept
/// for the life of the store.
#[derive(Default)]
pub(crate) struct FileGate {
    target: OnceLock<Option<Target>>,
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
            // The read goes on under the permit it was resolved under.
            let probing = self.resolve(conn);
            if probing.0.is_some() {
                return probing;
            }
        }
        let target = self.target.get().and_then(|target| target.as_ref());
        let Some(target) = target.filter(|target| target.readers_gated) else {
            return ReadPermit(None, PhantomData);
        };
        admit_reader(&target.gate)
    }

    /// Queue for the write slot of the database file behind `pool`. Call it
    /// on the blocking thread before taking the connection the write will
    /// use, so a queued writer does not keep a pooled connection from others.
    pub(crate) fn write(&self, pool: &ConnectionPool) -> WritePermit {
        if self.target.get().is_none() {
            // If no connection can be had, the operation itself reports that.
            // The connection goes back to the pool before queueing.
            if let Ok(mut conn) = pool.get() {
                drop(self.resolve(&mut conn));
            }
        }
        self.admit_writer()
    }

    /// As [`write`](Self::write), for a writer that owns its connection.
    pub(crate) fn write_on(&self, conn: &mut SqliteConnection) -> WritePermit {
        if self.target.get().is_none() {
            drop(self.resolve(conn));
        }
        self.admit_writer()
    }

    /// If the connection cannot be asked, this operation runs ungated and
    /// the next one asks again. Returns the permit the question was asked
    /// under while reads of the file are gated; a writer drops it before it
    /// queues for the write slot.
    fn resolve(&self, conn: &mut SqliteConnection) -> ReadPermit {
        match describe(conn) {
            Ok(Some((target, probing))) => {
                let gated = target.readers_gated;
                let _ = self.target.set(Some(target));
                if gated {
                    return probing;
                }
            }
            Ok(None) => {
                let _ = self.target.set(None);
            }
            Err(_) => {}
        }
        ReadPermit(None, PhantomData)
    }

    fn admit_writer(&self) -> WritePermit {
        let Some(Some(target)) = self.target.get() else {
            return WritePermit::ungated();
        };
        if inside(&target.gate) {
            return WritePermit::ungated();
        }
        let queued_behind_refusals = target.gate.begin_write();
        enter(&target.gate);
        WritePermit {
            held: Some(Held {
                gate: target.gate.clone(),
                readers_gated: target.readers_gated,
                queued_behind_refusals,
            }),
            _on_this_thread: PhantomData,
        }
    }
}

/// Admit a read of `gate`'s file, unless this thread is inside it already.
fn admit_reader(gate: &Arc<Gate>) -> ReadPermit {
    if inside(gate) {
        return ReadPermit(None, PhantomData);
    }
    gate.begin_read();
    enter(gate);
    ReadPermit(Some(gate.clone()), PhantomData)
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

struct Held {
    gate: Arc<Gate>,
    readers_gated: bool,
    queued_behind_refusals: Duration,
}

/// The write slot. Dropping it, on any path including a panic, passes the
/// slot on; it cannot leave the thread that obtained it.
pub(crate) struct WritePermit {
    held: Option<Held>,
    _on_this_thread: PhantomData<*const ()>,
}

impl WritePermit {
    fn ungated() -> Self {
        Self {
            held: None,
            _on_this_thread: PhantomData,
        }
    }

    /// A permit on a gate of its own that queued `behind_refusals`.
    #[cfg(test)]
    pub(crate) fn queued_behind(behind_refusals: Duration) -> Self {
        let gate = Arc::new(Gate::default());
        gate.begin_write();
        Self {
            held: Some(Held {
                gate,
                readers_gated: true,
                queued_behind_refusals: behind_refusals,
            }),
            _on_this_thread: PhantomData,
        }
    }

    /// How long this writer queued behind attempts that a lock outside the
    /// gate refused. It has waited out that lock for as long already, so its
    /// own attempt should not wait a whole busy timeout again: queueing must
    /// not multiply the bounded wait.
    pub(crate) fn queued_behind_refusals(&self) -> Duration {
        self.held
            .as_ref()
            .map_or(Duration::ZERO, |held| held.queued_behind_refusals)
    }

    /// Call before `COMMIT`: stop admitting this process's readers and give
    /// the running ones a short grace, so that normally the commit does not
    /// wait for them inside SQLite and they do not poll for the file against
    /// it. Nothing to do in WAL mode.
    pub(crate) fn exclude_readers(&self) {
        if let Some(held) = self.held.as_ref().filter(|held| held.readers_gated) {
            held.gate.exclude_readers(DRAIN_GRACE);
        }
    }

    /// Release after the write has committed or rolled back, recording
    /// whether SQLite refused it as locked.
    pub(crate) fn finish<T, E: Display>(self, outcome: &Result<T, E>) {
        if let (Some(held), Err(error)) = (&self.held, outcome) {
            if error.to_string().contains(LOCKED) {
                held.gate.state().record_refusal(Instant::now());
            }
        }
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            leave(&held.gate);
            held.gate.end_write();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;
    use std::sync::mpsc;

    /// Far longer than any step of these tests: a wait this long is a failure.
    const NEVER: Duration = Duration::from_secs(30);

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("arc-gate-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn pool(&self) -> ConnectionPool {
            pool(self.0.join("store.db").to_str().unwrap())
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pool(url: &str) -> ConnectionPool {
        Pool::builder()
            .max_size(2)
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
        gate.begin_write();
        let (order_tx, order_rx) = mpsc::channel();
        let mut threads = Vec::new();
        for n in 0..16 {
            let (waiting, order_tx) = (gate.clone(), order_tx.clone());
            threads.push(std::thread::spawn(move || {
                waiting.begin_write();
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
        let state = gate.state();
        assert!(!state.writer && state.attempt_since.is_none());
    }

    #[test]
    fn reads_run_beside_a_writer_until_it_commits_and_wait_out_one_commit() {
        let gate = Arc::new(Gate::default());
        // Reads run together, and beside a writer that has not reached commit.
        gate.begin_read();
        gate.begin_write();
        gate.begin_read();
        assert_eq!(gate.state().readers, 2);

        // The commit waits for both running reads.
        let committing = gate.clone();
        let (committed_tx, committed_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            committing.exclude_readers(NEVER);
            committed_tx.send(committing.state().readers).unwrap();
        });
        until(&gate, |state| state.draining.is_some());

        // Reads arriving now wait for the commit to end, however many. Each
        // reports when it runs and keeps running until told to finish.
        let (running_tx, running_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let finish_rx = Arc::new(Mutex::new(finish_rx));
        let late: Vec<_> = (0..3)
            .map(|_| {
                let (reading, running_tx, finish_rx) =
                    (gate.clone(), running_tx.clone(), finish_rx.clone());
                std::thread::spawn(move || {
                    reading.begin_read();
                    running_tx.send(()).unwrap();
                    finish_rx.lock().unwrap().recv_timeout(NEVER).unwrap();
                    reading.end_read();
                })
            })
            .collect();
        until(&gate, |state| state.parked.len() == 3);
        gate.end_read();
        assert!(committed_rx.try_recv().is_err(), "one read still runs");
        gate.end_read();
        assert_eq!(committed_rx.recv_timeout(NEVER).unwrap(), 0);
        writer.join().unwrap();
        assert!(running_rx.try_recv().is_err(), "late reads still wait");

        // A second writer is queued for the slot. When the first ends, all
        // waiting reads run at once, and the second writer's commit then
        // waits for them: no read waits out more than one commit.
        let second = gate.clone();
        let (second_tx, second_rx) = mpsc::channel();
        let second_writer = std::thread::spawn(move || {
            second.begin_write();
            second.exclude_readers(NEVER);
            second_tx.send(second.state().readers).unwrap();
            second.end_write();
        });
        until(&gate, |state| state.writers.len() == 1);
        gate.end_write();
        for _ in 0..3 {
            running_rx.recv_timeout(NEVER).unwrap();
        }
        assert_eq!(gate.state().readers, 3, "all three run together");
        until(&gate, |state| state.draining.is_some());
        assert!(second_rx.try_recv().is_err(), "the commit waits for them");
        for _ in 0..3 {
            finish_tx.send(()).unwrap();
        }
        assert_eq!(second_rx.recv_timeout(NEVER).unwrap(), 0);
        for thread in late.into_iter().chain([second_writer]) {
            thread.join().unwrap();
        }
        let state = gate.state();
        assert!(!state.writer && !state.committing && state.readers == 0);
        assert!(state.writers.is_empty() && state.parked.is_empty());
    }

    #[test]
    fn a_commit_stops_waiting_for_a_read_that_does_not_finish() {
        let gate = Gate::default();
        gate.begin_write();
        // This read never ends on its own: it stands for one that is stuck
        // inside SQLite behind this writer, or is simply long.
        gate.begin_read();
        let grace = Duration::from_millis(30);
        let began = Instant::now();
        gate.exclude_readers(grace);
        assert!(began.elapsed() >= grace);
        {
            let state = gate.state();
            assert!(state.committing && state.draining.is_none() && state.readers == 1);
        }
        // The read ends later without anyone to wake, and the gate is clean.
        gate.end_write();
        gate.end_read();
        let state = gate.state();
        assert!(!state.writer && !state.committing && state.readers == 0);
    }

    #[test]
    fn a_writer_is_charged_only_the_refused_time_it_queued_through() {
        // The arithmetic alone, on chosen instants.
        let start = Instant::now();
        let at = |milliseconds| start + Duration::from_millis(milliseconds);
        let mut state = State {
            writer: true,
            attempt_since: Some(at(0)),
            ..State::default()
        };
        // Two writers arrive during an attempt that is then refused at 300.
        let early = state.refusals_before(at(10));
        let late = state.refusals_before(at(280));
        state.record_refusal(at(300));
        assert_eq!(
            state.queued_behind_refusals(early),
            Duration::from_millis(290)
        );
        assert_eq!(
            state.queued_behind_refusals(late),
            Duration::from_millis(20)
        );

        // The next attempt succeeds: nobody is charged for queueing behind it.
        state.attempt_since = Some(at(300));
        let behind_success = state.refusals_before(at(350));
        state.attempt_since = Some(at(900));
        assert_eq!(state.queued_behind_refusals(behind_success), Duration::ZERO);
        // Not even when a later attempt is refused for less than that wait.
        state.record_refusal(at(940));
        assert_eq!(state.queued_behind_refusals(behind_success), Duration::ZERO);
        // Refusals add up for a writer that queued through several.
        assert_eq!(
            state.queued_behind_refusals(early),
            Duration::from_millis(330)
        );
    }

    #[test]
    fn a_refused_attempt_is_charged_to_the_writers_queued_behind_it() {
        let dir = Dir::new();
        let pool = dir.pool();
        let file_gate = FileGate::new();
        let holder = file_gate.write(&pool);
        assert_eq!(holder.queued_behind_refusals(), Duration::ZERO);
        let gate = gate_of(&file_gate);
        let queue = || {
            let (file_gate, pool) = (file_gate.clone(), pool.clone());
            let queued = gate.state().writers.len();
            let writer = std::thread::spawn(move || {
                let permit = file_gate.write(&pool);
                let charged = permit.queued_behind_refusals();
                permit.finish(&Ok::<(), &str>(()));
                charged
            });
            until(&gate, |state| state.writers.len() > queued);
            writer
        };
        let first = queue();
        std::thread::sleep(Duration::from_millis(20));
        holder.finish(&Err::<(), _>(LOCKED));
        let charged = first.join().unwrap();
        assert!(charged >= Duration::from_millis(20), "{charged:?}");

        // Other errors and successes charge nobody.
        let holder = file_gate.write(&pool);
        let second = queue();
        std::thread::sleep(Duration::from_millis(20));
        holder.finish(&Err::<(), _>("constraint failed"));
        assert_eq!(second.join().unwrap(), Duration::ZERO);
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
            drop(gate.write(&pool(url)));
            gate
        })
        .collect();
        for gate in &gates {
            assert!(Arc::ptr_eq(&gate_of(gate), &gate_of(&gates[0])));
        }

        // A store that only ever reads finds the same gate.
        let reader = FileGate::new();
        drop(reader.read(&mut pool(&plain).get().unwrap()));
        assert!(Arc::ptr_eq(&gate_of(&reader), &gate_of(&gates[0])));

        let other = FileGate::new();
        drop(other.write(&pool(dir.0.join("other.db").to_str().unwrap())));
        assert!(!Arc::ptr_eq(&gate_of(&other), &gate_of(&gates[0])));
    }

    #[test]
    fn databases_without_a_file_are_not_gated() {
        for url in [":memory:", "", "file::memory:?cache=shared"] {
            let gate = FileGate::new();
            let pool = pool(url);
            assert!(gate.write(&pool).held.is_none(), "{url}");
            assert!(gate.read(&mut pool.get().unwrap()).0.is_none(), "{url}");
            assert!(gate.target.get().unwrap().is_none(), "{url}");
        }
    }

    /// On a new connection the journal mode is the first statement to load
    /// the schema, under a shared lock on the file, so it is asked in turn,
    /// as a read: during a commit it waits for the commit to end, and the
    /// read that asked goes on under the same permit. A writer gives the
    /// permit up before it queues for the slot.
    #[test]
    fn the_journal_mode_is_asked_in_turn() {
        use diesel::connection::InstrumentationEvent;
        let dir = Dir::new();
        let pool = dir.pool();
        // Another store's writer is committing.
        let committer = FileGate::new();
        let committing = committer.write(&pool);
        committing.exclude_readers();
        let gate = gate_of(&committer);

        let asked = Arc::new(AtomicBool::new(false));
        let mut conn = pool.get().unwrap();
        let asking = asked.clone();
        conn.set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                if query.to_string().contains("journal_mode") {
                    asking.store(true, Ordering::SeqCst);
                }
            }
        });
        let (read_tx, read_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            let reading = FileGate::new().read(&mut conn);
            read_tx.send(reading.0.is_some()).unwrap();
            finish_rx.recv_timeout(NEVER).unwrap();
        });
        until(&gate, |state| state.parked.len() == 1);
        assert!(!asked.load(Ordering::SeqCst), "asked during the commit");
        drop(committing);
        assert!(read_rx.recv_timeout(NEVER).unwrap(), "the read is gated");
        assert!(asked.load(Ordering::SeqCst));
        assert_eq!(gate.state().readers, 1, "one permit, for both");
        finish_tx.send(()).unwrap();
        reader.join().unwrap();
        assert_eq!(gate.state().readers, 0);

        let writing = FileGate::new().write(&pool);
        let state = gate.state();
        assert!(state.writer && state.readers == 0);
        drop(state);
        drop(writing);
    }

    #[test]
    fn wal_readers_are_not_gated_and_commits_do_not_wait_for_them() {
        let dir = Dir::new();
        let pool = dir.pool();
        pool.get()
            .unwrap()
            .batch_execute("PRAGMA journal_mode = WAL")
            .unwrap();
        let file_gate = FileGate::new();
        let reading = file_gate.read(&mut pool.get().unwrap());
        assert!(reading.0.is_none());
        // Nor is the read that asked the journal mode left counted.
        assert_eq!(gate_of(&file_gate).state().readers, 0);
        // Writers still take turns.
        let writing = file_gate.write(&pool);
        let gate = gate_of(&file_gate);
        assert!(gate.state().writer);
        writing.exclude_readers();
        assert!(!gate.state().committing);
        drop(writing);
        assert!(!gate.state().writer);

        // The same file in rollback-journal mode, for contrast.
        let dir = Dir::new();
        let pool = dir.pool();
        let file_gate = FileGate::new();
        let reading = file_gate.read(&mut pool.get().unwrap());
        assert!(reading.0.is_some());
        drop(reading);
        let writing = file_gate.write(&pool);
        writing.exclude_readers();
        assert!(gate_of(&file_gate).state().committing);
    }

    #[test]
    fn an_operation_nested_on_one_thread_passes_and_keeps_the_outer_permit() {
        let dir = Dir::new();
        let pool = dir.pool();
        let (outer_gate, inner_gate) = (FileGate::new(), FileGate::new());

        let outer = outer_gate.write(&pool);
        let inner = inner_gate.write(&pool);
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
        let inner = inner_gate.write(&pool);
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
                    let permit = panicking_gate.write(&panicking_pool);
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
        assert!(gate.write(&pool).held.is_some());
    }
}
