//! Database selection and event integrity configuration.
use std::env;

/// Storage backend selected at startup behind the `EventStore` /
/// `ReadModelStore` traits. Parsing is always available; constructing the
/// Postgres stores additionally requires the `postgres` cargo feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseDriver {
    Sqlite,
    Postgres,
}

impl DatabaseDriver {
    /// Read `DATABASE_DRIVER` from the environment, defaulting to SQLite.
    pub fn from_env() -> Self {
        Self::parse(&env::var("DATABASE_DRIVER").unwrap_or_else(|_| "sqlite".to_string()))
    }

    /// Parse a driver name. Anything other than a recognized Postgres alias
    /// falls back to SQLite, matching the historical default.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => DatabaseDriver::Postgres,
            _ => DatabaseDriver::Sqlite,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DatabaseDriver::Sqlite => "sqlite",
            DatabaseDriver::Postgres => "postgres",
        }
    }

    /// Whether `DATABASE_URL` names a local filesystem path the startup health
    /// check can stat. Postgres uses a connection string, not a file.
    pub fn is_file_backed(self) -> bool {
        matches!(self, DatabaseDriver::Sqlite)
    }
}

/// Optional HMAC key enabling HIPAA-5 event integrity signing/enforcement.
/// The storage layer validates length and rejects keys shorter than 32 bytes.
pub fn event_integrity_key() -> Option<Vec<u8>> {
    env::var("EVENT_INTEGRITY_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.into_bytes())
}

/// Identifier stored with each event signature so future key rotation can
/// distinguish which key signed a row.
pub fn event_integrity_key_id() -> String {
    env::var("EVENT_INTEGRITY_KEY_ID").unwrap_or_else(|_| "default".to_string())
}
