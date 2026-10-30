use std::env;

/// Default database file path used when DATABASE_URL is not set
pub const DEFAULT_DATABASE_URL: &str = "database/database.sqlite";

/// Default SQLite file backing the JWT session-revocation registry when the
/// primary driver is not SQLite.
pub const DEFAULT_SESSION_DATABASE_URL: &str = "database/sessions.sqlite";

/// Default database connection pool size
pub const DEFAULT_POOL_LIMIT: u32 = 10;

/// Default number of new aggregate events between best-effort user snapshots.
pub const DEFAULT_USER_SNAPSHOT_INTERVAL_EVENTS: i64 = 50;

pub use arc_stack::config::{event_integrity_key, event_integrity_key_id, DatabaseDriver};

/// Get the database URL from environment or use default
pub fn database_url() -> String {
    env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
}

/// SQLite URL backing the JWT session-revocation registry. The registry is
/// SQLite-only today; under a non-SQLite primary driver it keeps its own local
/// SQLite database (overridable via `SESSION_DATABASE_URL`) so session
/// revocation still functions.
pub fn session_store_url(driver: DatabaseDriver) -> String {
    match driver {
        DatabaseDriver::Sqlite => database_url(),
        DatabaseDriver::Postgres => env::var("SESSION_DATABASE_URL")
            .unwrap_or_else(|_| DEFAULT_SESSION_DATABASE_URL.to_string()),
    }
}

/// Number of new events between best-effort aggregate snapshots.
/// Values <= 0 disable user snapshots without changing command correctness.
pub fn user_snapshot_interval_events() -> Option<i64> {
    let interval = env::var("USER_SNAPSHOT_INTERVAL_EVENTS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .parse()
                .expect("USER_SNAPSHOT_INTERVAL_EVENTS must be a number")
        })
        .unwrap_or(DEFAULT_USER_SNAPSHOT_INTERVAL_EVENTS);

    (interval > 0).then_some(interval)
}

/// Get the database pool limit from environment or use default
pub fn database_pool_limit() -> u32 {
    env::var("DATABASE_POOL_LIMIT")
        .unwrap_or_else(|_| DEFAULT_POOL_LIMIT.to_string())
        .parse()
        .expect("DATABASE_POOL_LIMIT must be a number")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn user_snapshot_interval_defaults_to_50() {
        env::remove_var("USER_SNAPSHOT_INTERVAL_EVENTS");
        assert_eq!(
            user_snapshot_interval_events(),
            Some(DEFAULT_USER_SNAPSHOT_INTERVAL_EVENTS)
        );
    }

    #[test]
    #[serial]
    fn user_snapshot_interval_reads_env_override() {
        env::set_var("USER_SNAPSHOT_INTERVAL_EVENTS", "25");
        assert_eq!(user_snapshot_interval_events(), Some(25));
        env::remove_var("USER_SNAPSHOT_INTERVAL_EVENTS");
    }

    #[test]
    #[serial]
    fn user_snapshot_interval_non_positive_disables_snapshots() {
        env::set_var("USER_SNAPSHOT_INTERVAL_EVENTS", "0");
        assert_eq!(user_snapshot_interval_events(), None);
        env::remove_var("USER_SNAPSHOT_INTERVAL_EVENTS");
    }
}
