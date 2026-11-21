//! Shared connection policy for store-owned pools. Injected pools keep their policy.
use diesel::connection::{AnsiTransactionManager, SimpleConnection, TransactionManager};
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Error};
use diesel::SqliteConnection;

/// Maximum SQLite lock wait per statement on every store-owned connection,
/// including replacements. This changes neither journal mode nor durability.
pub(crate) const BUSY_TIMEOUT_MS: u32 = 5_000;

/// The builder of every store-owned pool. r2d2 would test each connection it
/// lends with `SELECT 1`, before the store can queue for the file. On a new
/// connection, including a replacement, that is the first statement, the one
/// that loads the schema under a shared lock on the file: outside the queue
/// it polls for the lock against one commit after another, can lose every
/// time until the busy timeout, and then r2d2 discards the connection and
/// tries another. Without the test the first statement is the store's own,
/// under its turn. The test would catch nothing here: a SQLite connection
/// does not go stale, and Diesel already has r2d2 discard a connection left
/// in a broken transaction.
pub(crate) fn pool_builder() -> diesel::r2d2::Builder<ConnectionManager<SqliteConnection>> {
    diesel::r2d2::Pool::builder().test_on_check_out(false)
}

#[derive(Debug)]
pub(crate) struct BusyTimeout;

impl CustomizeConnection<SqliteConnection, Error> for BusyTimeout {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), Error> {
        conn.batch_execute(&format!("PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}"))
            .map_err(Error::QueryError)
    }
}

/// Clean up without replacing the original append error. Diesel may already
/// have rolled back a failed commit. If rollback fails, mark the connection
/// broken so r2d2 disposes of it rather than lending dirty state to a caller.
pub(crate) fn rollback_append(conn: &mut SqliteConnection) {
    if matches!(
        AnsiTransactionManager::transaction_manager_status_mut(conn).transaction_depth(),
        Ok(None)
    ) {
        return;
    }
    if AnsiTransactionManager::rollback_transaction(conn).is_err() {
        *AnsiTransactionManager::transaction_manager_status_mut(conn) =
            diesel::connection::TransactionManagerStatus::InError;
    }
}

/// Run `body` as one write transaction for the holder of the file's write
/// slot. The writer is reserved up front (`BEGIN IMMEDIATE`): a deferred
/// read-to-write upgrade can fail immediately even with a busy timeout, and
/// while this waits for a writer outside the process the stores' readers
/// carry on. They are held back only for the commit itself. The body or
/// commit error is preserved; COMMIT can fail (for example while an external
/// reader holds a shared lock) and leave a transaction open.
pub(crate) fn write_transaction<T, E>(
    conn: &mut SqliteConnection,
    permit: &crate::file_gate::WritePermit,
    database_error: impl Fn(String) -> E,
    body: impl FnOnce(&mut SqliteConnection) -> Result<T, E>,
) -> Result<T, E> {
    let usual_timeout = spend_busy_timeout(conn, permit.queued_behind_refusals())
        .map_err(|e| database_error(e.to_string()))?;
    let begun = AnsiTransactionManager::begin_transaction_sql(conn, "BEGIN IMMEDIATE");
    // Only the wait for the write lock is shortened: it is the one this
    // writer already sat through in the queue. The body and COMMIT, which
    // may wait for a reader, get the connection's usual timeout back.
    if let Some(timeout) = usual_timeout {
        if let Err(error) = set_busy_timeout(conn, timeout) {
            // A connection that cannot get its timeout back must not be reused.
            rollback_append(conn);
            *AnsiTransactionManager::transaction_manager_status_mut(conn) =
                diesel::connection::TransactionManagerStatus::InError;
            return Err(database_error(error.to_string()));
        }
    }
    let result = begun
        .map_err(|e| database_error(e.to_string()))
        .and_then(|()| body(conn))
        .and_then(|value| {
            permit.exclude_readers();
            AnsiTransactionManager::commit_transaction(conn)
                .map(|()| value)
                .map_err(|e| database_error(e.to_string()))
        });
    if result.is_err() {
        rollback_append(conn);
    }
    result
}

/// A writer that queued behind attempts refused by a lock outside the gate
/// has already waited `spent` for that lock. Take it off this connection's
/// busy timeout for the writer's own attempt at the write lock, which it
/// still makes, so that queueing does not multiply the bounded wait. Returns
/// the timeout to put back, if it was changed.
fn spend_busy_timeout(
    conn: &mut SqliteConnection,
    spent: std::time::Duration,
) -> diesel::QueryResult<Option<i32>> {
    use diesel::prelude::*;
    #[derive(QueryableByName)]
    struct Timeout {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        timeout: i32,
    }
    if spent.is_zero() {
        return Ok(None);
    }
    let usual = diesel::sql_query("PRAGMA busy_timeout")
        .get_result::<Timeout>(conn)?
        .timeout;
    if usual <= 0 {
        return Ok(None);
    }
    let spent = i32::try_from(spent.as_millis()).unwrap_or(i32::MAX);
    set_busy_timeout(conn, usual.saturating_sub(spent).max(0))?;
    Ok(Some(usual))
}

fn set_busy_timeout(conn: &mut SqliteConnection, milliseconds: i32) -> diesel::QueryResult<()> {
    conn.batch_execute(&format!("PRAGMA busy_timeout = {milliseconds}"))
}

#[cfg(test)]
pub(crate) fn assert_timeout_on_replacement(
    pool: &diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<SqliteConnection>>,
) {
    use diesel::prelude::*;
    #[derive(QueryableByName)]
    struct Timeout {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        timeout: i32,
    }
    let check = |conn: &mut SqliteConnection| {
        let value = diesel::sql_query("PRAGMA busy_timeout")
            .get_result::<Timeout>(conn)
            .unwrap();
        assert_eq!(value.timeout, BUSY_TIMEOUT_MS as i32);
    };
    // Hold all original connections; returning one with an open transaction
    // forces Diesel/r2d2 to discard it and create a replacement.
    let mut held: Vec<_> = (0..10).map(|_| pool.get().unwrap()).collect();
    for conn in &mut held {
        check(conn);
    }
    let mut discarded = held.pop().unwrap();
    discarded
        .batch_execute("PRAGMA busy_timeout = 123")
        .unwrap();
    AnsiTransactionManager::begin_transaction(&mut *discarded).unwrap();
    drop(discarded);
    let mut replacement = pool.get().unwrap();
    check(&mut replacement);
    assert!(
        AnsiTransactionManager::transaction_manager_status_mut(&mut *replacement)
            .transaction_depth()
            .unwrap()
            .is_none()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::InstrumentationEvent;
    use diesel::prelude::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct File(std::path::PathBuf);
    impl File {
        fn new() -> Self {
            let name = format!("arc-connection-{}.db", uuid::Uuid::new_v4());
            let file = Self(std::env::temp_dir().join(name));
            file.connect()
                .batch_execute("CREATE TABLE notes (body TEXT)")
                .unwrap();
            file
        }
        fn connect(&self) -> SqliteConnection {
            SqliteConnection::establish(self.0.to_str().unwrap()).unwrap()
        }
    }
    impl Drop for File {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn busy_timeout(conn: &mut SqliteConnection) -> i32 {
        #[derive(QueryableByName)]
        struct Timeout {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            timeout: i32,
        }
        diesel::sql_query("PRAGMA busy_timeout")
            .get_result::<Timeout>(conn)
            .unwrap()
            .timeout
    }

    /// A connection with a 300 ms busy timeout that records its statements.
    fn recorded(file: &File) -> (SqliteConnection, Arc<Mutex<Vec<String>>>) {
        let mut conn = file.connect();
        set_busy_timeout(&mut conn, 300).unwrap();
        let statements = Arc::new(Mutex::new(Vec::new()));
        let record = statements.clone();
        conn.set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                let query = query.to_string();
                let statement = query.split(" -- ").next().unwrap_or_default();
                record.lock().unwrap().push(statement.to_string());
            }
        });
        (conn, statements)
    }

    #[test]
    fn time_spent_queueing_shortens_only_the_wait_for_the_write_lock() {
        let file = File::new();
        let (mut conn, statements) = recorded(&file);
        // Queued 200 of its 300 ms behind refused attempts: 100 ms are left
        // for BEGIN IMMEDIATE, and the full 300 for the body and COMMIT.
        let permit = crate::file_gate::WritePermit::queued_behind(Duration::from_millis(200));
        let during_body = write_transaction(
            &mut conn,
            &permit,
            |e| e,
            |conn| {
                let timeout = busy_timeout(conn);
                conn.batch_execute("INSERT INTO notes VALUES ('kept')")
                    .map_err(|e| e.to_string())?;
                Ok(timeout)
            },
        )
        .unwrap();
        assert_eq!(during_body, 300);
        assert_eq!(busy_timeout(&mut conn), 300);
        assert_eq!(
            statements.lock().unwrap()[..6],
            [
                "PRAGMA busy_timeout",
                "PRAGMA busy_timeout = 100",
                "BEGIN IMMEDIATE",
                "PRAGMA busy_timeout = 300",
                "PRAGMA busy_timeout",
                "INSERT INTO notes VALUES ('kept')",
            ]
        );

        // A writer that queued its whole timeout still tries once.
        let permit = crate::file_gate::WritePermit::queued_behind(Duration::from_secs(60));
        statements.lock().unwrap().clear();
        write_transaction(&mut conn, &permit, |e| e, |_| Ok(())).unwrap();
        assert_eq!(
            statements.lock().unwrap()[1..3],
            ["PRAGMA busy_timeout = 0", "BEGIN IMMEDIATE"]
        );
        assert_eq!(busy_timeout(&mut conn), 300);
    }

    /// The first statement on a new connection loads the schema, which needs
    /// a shared lock on the file. Outside the queue it polls for that lock
    /// against one commit after another and can lose every time until the
    /// busy timeout; in the queue it waits for one commit. Every connection of
    /// a new store is new, and so is a replacement.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn new_connections_read_in_turn_while_others_commit_nonstop() {
        use arc_core::event_store::EventStore;
        use arc_core::read_model_store::ReadModelStore;
        const HOLD: Duration = Duration::from_millis(100);
        // A few commits; a read that polls lasts the 5-second busy timeout.
        const IN_TURN: Duration = Duration::from_secs(2);
        let file = File::new();
        crate::test_support::migrate(&mut file.connect());
        let url = file.0.to_str().unwrap();
        let commits = crate::test_support::SlowCommits::start(url, 2, HOLD);
        tokio::time::sleep(HOLD * 2).await;

        let events = crate::SqliteEventStore::new(url).await.unwrap();
        let views = crate::SqliteReadModelStore::new(url).await.unwrap();
        let began = Instant::now();
        let mut reads = tokio::task::JoinSet::new();
        // As many at once as a pool has connections, so each takes a new one.
        for _ in 0..10 {
            let (events, views) = (events.clone(), views.clone());
            reads.spawn(async move {
                assert!(events.load("none").await.unwrap().is_empty());
                assert!(views.get("users_view", "none").await.unwrap().is_none());
            });
        }
        while let Some(read) = reads.join_next().await {
            read.unwrap();
        }
        let first = began.elapsed();

        // Returning a connection with an open transaction makes r2d2 discard
        // it; with the others held, the next read gets its replacement.
        let pool = events.pool.clone();
        let held = tokio::task::spawn_blocking(move || {
            let mut held: Vec<_> = (0..10).map(|_| pool.get().unwrap()).collect();
            let mut discarded = held.pop().unwrap();
            AnsiTransactionManager::begin_transaction(&mut *discarded).unwrap();
            drop(discarded);
            held
        })
        .await
        .unwrap();
        let began = Instant::now();
        assert!(events.load("none").await.unwrap().is_empty());
        let replacement = began.elapsed();
        drop(held);

        let made = commits.stop();
        assert!(
            first < IN_TURN,
            "first reads polled for the file: {first:?}"
        );
        assert!(
            replacement < IN_TURN,
            "a replacement connection polled for the file: {replacement:?}"
        );
        assert!(made >= 4, "the commits stopped: {made}");
    }

    #[test]
    fn the_usual_timeout_returns_when_the_write_lock_is_refused() {
        let file = File::new();
        let mut outside = file.connect();
        outside.batch_execute("BEGIN IMMEDIATE").unwrap();
        let (mut conn, _) = recorded(&file);
        let permit = crate::file_gate::WritePermit::queued_behind(Duration::from_secs(60));
        let refused = write_transaction(&mut conn, &permit, |e| e, |_| Ok(())).unwrap_err();
        assert!(refused.contains(crate::file_gate::LOCKED), "{refused}");
        assert_eq!(busy_timeout(&mut conn), 300);
        // The connection is clean and usable once the lock is gone.
        outside.batch_execute("ROLLBACK").unwrap();
        write_transaction(&mut conn, &permit, |e| e, |_| Ok::<_, String>(())).unwrap();

        // A writer that did not queue behind refusals is left alone.
        let (mut conn, statements) = recorded(&file);
        let permit = crate::file_gate::WritePermit::queued_behind(Duration::ZERO);
        write_transaction(&mut conn, &permit, |e| e, |_| Ok::<_, String>(())).unwrap();
        assert_eq!(
            statements.lock().unwrap()[..2],
            ["BEGIN IMMEDIATE", "COMMIT"]
        );
    }
}
