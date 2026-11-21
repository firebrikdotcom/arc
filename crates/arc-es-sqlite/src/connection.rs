//! Shared connection policy for store-owned pools. Injected pools keep their policy.
use diesel::connection::{AnsiTransactionManager, SimpleConnection, TransactionManager};
use diesel::r2d2::{CustomizeConnection, Error};
use diesel::SqliteConnection;

/// Maximum SQLite lock wait per statement on every store-owned connection,
/// including replacements. This changes neither journal mode nor durability.
pub(crate) const BUSY_TIMEOUT_MS: u32 = 5_000;

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
    let result = AnsiTransactionManager::begin_transaction_sql(conn, "BEGIN IMMEDIATE")
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
    if let Some(timeout) = usual_timeout {
        // A connection that cannot get its timeout back must not be reused.
        if set_busy_timeout(conn, timeout).is_err() {
            *AnsiTransactionManager::transaction_manager_status_mut(conn) =
                diesel::connection::TransactionManagerStatus::InError;
        }
    }
    result
}

/// A writer that queued behind attempts refused by a lock outside the gate
/// has already waited `spent` for that lock. Take it off this connection's
/// busy timeout for the writer's own attempt, which it still makes, so that
/// queueing does not multiply the bounded wait. Returns the timeout to put
/// back, if it was changed.
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
