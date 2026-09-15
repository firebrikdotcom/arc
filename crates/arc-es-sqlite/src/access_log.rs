//! Dedicated, opt-in SQLite read-audit journal. Success acknowledges a committed
//! SQLite transaction with synchronous=FULL; filesystem/hardware guarantees apply.
use arc_core::access_log::{
    AccessLogEntry, AccessLogError, AccessLogger, AccessedResource, Identity, PurposeOfUse,
};
use async_trait::async_trait;
use diesel::{connection::SimpleConnection, prelude::*};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Semaphore;
use uuid::Uuid;

const MIGRATION: &str = include_str!("../migrations/access_log/0001_create_access_log/up.sql");

/// Optional metadata is omitted by default. Actor/resource identifiers and field
/// names remain caller-owned: pass opaque IDs, never response values or tokens.
#[derive(Clone, Copy, Debug, Default)]
pub struct AccessLogPrivacy {
    pub include_source_ip: bool,
    pub include_user_agent: bool,
}

/// No in-memory fallback, retries, or unbounded task queue. Clones share one write
/// permit; overload rejects immediately. A cancelled caller's admitted blocking
/// operation can still commit; no exactly-once response-delivery claim is made.
#[derive(Clone)]
pub struct SqliteAccessLogger {
    path: Arc<PathBuf>,
    permit: Arc<Semaphore>,
    privacy: AccessLogPrivacy,
    busy_timeout_ms: u32,
}

fn disk_path(name: &str) -> bool {
    !name.is_empty() && name != ":memory:" && !name.starts_with("file:") && !name.contains('?')
}

fn sink_error() -> AccessLogError {
    AccessLogError::Sink("sqlite access journal operation failed".into())
}

impl SqliteAccessLogger {
    /// Open a dedicated on-disk journal and apply its idempotent initial migration.
    /// In-memory databases and SQLite URI parameters are intentionally unsupported.
    pub async fn open(
        path: PathBuf,
        privacy: AccessLogPrivacy,
        busy_timeout_ms: u32,
    ) -> Result<Self, AccessLogError> {
        let name = path.to_str().ok_or_else(sink_error)?;
        if !disk_path(name) || busy_timeout_ms == 0 || busy_timeout_ms > 30_000 {
            return Err(AccessLogError::Validation(
                "access journal requires a disk path and busy timeout in 1..=30000 ms".into(),
            ));
        }
        let logger = Self {
            path: Arc::new(path),
            permit: Arc::new(Semaphore::new(1)),
            privacy,
            busy_timeout_ms,
        };
        logger
            .run(|conn| {
                conn.batch_execute("PRAGMA journal_mode=DELETE;")
                    .map_err(|_| sink_error())?;
                conn.batch_execute(MIGRATION).map_err(|_| sink_error())?;
                conn.batch_execute(
                    "SELECT access_id, timestamp_utc_us, entry_json FROM access_log LIMIT 0;",
                )
                .map_err(|_| sink_error())
            })
            .await?;
        Ok(logger)
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut SqliteConnection) -> Result<T, AccessLogError> + Send + 'static,
    ) -> Result<T, AccessLogError> {
        let permit = self
            .permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccessLogError::Sink("sqlite access journal busy".into()))?;
        let path = self.path.clone();
        let timeout = self.busy_timeout_ms;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path.as_ref()).map_err(|_| sink_error())?;
            let mut conn = SqliteConnection::establish(path.to_str().ok_or_else(sink_error)?)
                .map_err(|_| sink_error())?;
            conn.batch_execute(&format!(
                "PRAGMA busy_timeout={timeout}; PRAGMA synchronous=FULL;"
            ))
            .map_err(|_| sink_error())?;
            operation(&mut conn)
        })
        .await
        .map_err(|_| sink_error())?
    }

    /// Delete at most `limit` entries strictly older than an operator-approved UTC
    /// microsecond cutoff. No automatic retention duration is imposed. Backups and
    /// filesystem free pages are outside this logical deletion hook.
    pub async fn purge_before(
        &self,
        cutoff_utc_us: i64,
        limit: u32,
    ) -> Result<usize, AccessLogError> {
        if limit == 0 || limit > 10_000 {
            return Err(AccessLogError::Validation(
                "purge limit must be 1..=10000".into(),
            ));
        }
        self.run(move |conn| diesel::sql_query("DELETE FROM access_log WHERE access_id IN (SELECT access_id FROM access_log WHERE timestamp_utc_us < ? ORDER BY timestamp_utc_us, access_id LIMIT ?)")
            .bind::<diesel::sql_types::BigInt, _>(cutoff_utc_us)
            .bind::<diesel::sql_types::BigInt, _>(i64::from(limit))
            .execute(conn).map_err(|_| sink_error())).await
    }
}

#[async_trait]
impl AccessLogger for SqliteAccessLogger {
    async fn log_access(
        &self,
        mut actor: Identity,
        resource: AccessedResource,
        purpose: PurposeOfUse,
        correlation_id: Option<Uuid>,
    ) -> Result<(), AccessLogError> {
        // Never persist session identifiers: the caller may have supplied a token.
        actor.session_id = None;
        if !self.privacy.include_source_ip {
            actor.source_ip = None;
        }
        if !self.privacy.include_user_agent {
            actor.user_agent = None;
        }
        let entry = AccessLogEntry::new(actor, resource, purpose, correlation_id)?;
        let json = serde_json::to_string(&entry).map_err(|_| sink_error())?;
        if json.len() > 16_384 {
            return Err(AccessLogError::Validation(
                "access journal entry exceeds 16384 bytes".into(),
            ));
        }
        self.run(move |conn| {
            diesel::sql_query(
                "INSERT INTO access_log(access_id, timestamp_utc_us, entry_json) VALUES (?, ?, ?)",
            )
            .bind::<diesel::sql_types::Text, _>(entry.access_id.to_string())
            .bind::<diesel::sql_types::BigInt, _>(entry.timestamp_utc_us)
            .bind::<diesel::sql_types::Text, _>(json)
            .execute(conn)
            .map_err(|_| sink_error())?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_core::access_log::Sensitivity;
    struct Journal(PathBuf);
    impl Journal {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("arc-nineties-audit-{}.db", Uuid::new_v4())))
        }
        async fn open(&self) -> SqliteAccessLogger {
            SqliteAccessLogger::open(self.0.clone(), AccessLogPrivacy::default(), 25)
                .await
                .unwrap()
        }
        fn entries(&self) -> Vec<AccessLogEntry> {
            #[derive(QueryableByName)]
            struct Row {
                #[diesel(sql_type = diesel::sql_types::Text)]
                entry_json: String,
            }
            let mut conn = SqliteConnection::establish(self.0.to_str().unwrap()).unwrap();
            diesel::sql_query("SELECT entry_json FROM access_log ORDER BY timestamp_utc_us")
                .load::<Row>(&mut conn)
                .unwrap()
                .into_iter()
                .map(|r| serde_json::from_str(&r.entry_json).unwrap())
                .collect()
        }
    }
    impl Drop for Journal {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    async fn write(logger: &SqliteAccessLogger) -> Result<(), AccessLogError> {
        let mut actor = Identity::new("opaque-user-id");
        actor.session_id = Some("SECRET-TOKEN".into());
        actor.source_ip = Some("192.0.2.1".into());
        actor.user_agent = Some("test-agent".into());
        logger
            .log_access(
                actor,
                AccessedResource::new("Profile", "opaque-resource-id", Sensitivity::Pii)
                    .with_fields(["name"]),
                PurposeOfUse::UserInitiated,
                Some(Uuid::nil()),
            )
            .await
    }
    #[test]
    fn platform_paths_are_not_sqlite_uris() {
        for path in [
            "/var/lib/arc/audit.db",
            "audit.db",
            r"C:\data\audit.db",
            "C:/data/audit.db",
        ] {
            assert!(disk_path(path));
        }
        for path in [
            "",
            ":memory:",
            "file:audit.db",
            "file::memory:?cache=shared",
        ] {
            assert!(!disk_path(path));
        }
    }
    #[tokio::test]
    async fn committed_entries_survive_reopen_and_retention_is_bounded() {
        let journal = Journal::new();
        let logger = journal.open().await;
        write(&logger).await.unwrap();
        drop(logger);
        let logger = journal.open().await;
        let entries = journal.entries();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.actor, Identity::new("opaque-user-id"));
        assert_eq!(entry.resource.fields, ["name"]);
        assert_eq!(entry.correlation_id, Some(Uuid::nil()));
        assert_eq!(
            logger
                .purge_before(entry.timestamp_utc_us, 10)
                .await
                .unwrap(),
            0
        );
        write(&logger).await.unwrap();
        assert_eq!(logger.purge_before(i64::MAX, 1).await.unwrap(), 1);
        assert_eq!(journal.entries().len(), 1);
        assert!(logger.purge_before(i64::MAX, 0).await.is_err());
    }
    #[tokio::test]
    async fn lock_contention_overload_and_invalid_entries_return_errors() {
        let journal = Journal::new();
        let logger = journal.open().await;
        let mut conn = SqliteConnection::establish(journal.0.to_str().unwrap()).unwrap();
        conn.batch_execute("BEGIN EXCLUSIVE").unwrap();
        assert!(matches!(write(&logger).await, Err(AccessLogError::Sink(_))));
        conn.batch_execute("ROLLBACK").unwrap();
        let permit = logger.permit.clone().acquire_owned().await.unwrap();
        assert!(matches!(write(&logger).await, Err(AccessLogError::Sink(_))));
        drop(permit);
        assert!(logger
            .log_access(
                Identity::new(""),
                AccessedResource::new("Profile", "1", Sensitivity::Pii),
                PurposeOfUse::Other,
                None
            )
            .await
            .is_err());
        assert!(journal.entries().is_empty());
        write(&logger).await.unwrap();
        assert_eq!(journal.entries().len(), 1);
        conn.batch_execute("DROP TABLE access_log").unwrap();
        let err = write(&logger).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "access log sink failed: sqlite access journal operation failed"
        );
    }
    #[tokio::test]
    async fn oversized_metadata_is_rejected_without_persistence() {
        let journal = Journal::new();
        let logger = journal.open().await;
        let result = logger
            .log_access(
                Identity::new("x".repeat(16_384)),
                AccessedResource::new("Record", "1", Sensitivity::Phi),
                PurposeOfUse::Other,
                None,
            )
            .await;
        assert!(matches!(result, Err(AccessLogError::Validation(_))));
        assert!(journal.entries().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&journal.0).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn metadata_is_explicit_and_session_tokens_are_always_omitted() {
        let journal = Journal::new();
        let logger = SqliteAccessLogger::open(
            journal.0.clone(),
            AccessLogPrivacy {
                include_source_ip: true,
                include_user_agent: true,
            },
            25,
        )
        .await
        .unwrap();
        write(&logger).await.unwrap();
        let entries = journal.entries();
        assert_eq!(entries[0].actor.source_ip.as_deref(), Some("192.0.2.1"));
        assert_eq!(entries[0].actor.user_agent.as_deref(), Some("test-agent"));
        assert!(entries[0].actor.session_id.is_none());
    }
}
