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
