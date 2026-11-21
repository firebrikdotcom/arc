use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::SqliteConnection;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) fn migrate(connection: &mut SqliteConnection) {
    connection
        .batch_execute(include_str!("../tests/fixtures/schema.sql"))
        .expect("create SQLite test schema");
}

/// Writers of this process committing one after another on the file at
/// `path`, each holding it exclusively for `hold`, as a commit does while it
/// syncs to a slow disk. They queue for the file as the stores' writers do.
/// The hold is a sleep, so what a test sees does not depend on the disk.
pub(crate) struct SlowCommits {
    done: Arc<AtomicBool>,
    writers: Vec<JoinHandle<usize>>,
}

impl SlowCommits {
    pub(crate) fn start(path: &str, writers: usize, hold: Duration) -> Self {
        SqliteConnection::establish(path)
            .unwrap()
            .batch_execute("CREATE TABLE IF NOT EXISTS slow_commits (n INTEGER)")
            .unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writers = (0..writers)
            .map(|_| {
                let (path, done) = (path.to_string(), done.clone());
                std::thread::spawn(move || {
                    let mut conn = SqliteConnection::establish(&path).unwrap();
                    conn.batch_execute("PRAGMA busy_timeout = 5000").unwrap();
                    let gate = crate::file_gate::FileGate::new();
                    let mut commits = 0;
                    while !done.load(Ordering::SeqCst) {
                        // The stores' commit window: the write slot is held
                        // and new reads are held back.
                        let permit = gate.write_on(&mut conn);
                        permit.exclude_readers();
                        conn.batch_execute("BEGIN EXCLUSIVE; INSERT INTO slow_commits VALUES (1);")
                            .unwrap();
                        std::thread::sleep(hold);
                        conn.batch_execute("COMMIT").unwrap();
                        drop(permit);
                        commits += 1;
                    }
                    commits
                })
            })
            .collect();
        Self { done, writers }
    }

    /// Stop the writers and return how many commits they made.
    pub(crate) fn stop(mut self) -> usize {
        self.halt()
            .into_iter()
            .map(|commits| commits.unwrap())
            .sum()
    }

    fn halt(&mut self) -> Vec<std::thread::Result<usize>> {
        self.done.store(true, Ordering::SeqCst);
        std::mem::take(&mut self.writers)
            .into_iter()
            .map(JoinHandle::join)
            .collect()
    }
}

/// A test that fails part way stops the writers before its file goes.
impl Drop for SlowCommits {
    fn drop(&mut self) {
        self.halt();
    }
}
