use arc_core::audit::AuditMetadata;
use arc_core::event::Event;
#[cfg(test)]
use arc_core::event::NewEvent;
use arc_core::event_store::{
    validate_audit_batch, EventStore, EventStoreError, EventStoreResult, VersionCheck,
};
use arc_core::integrity::{EventSignature, HmacSha256Chain, IntegrityChain, IntegrityError};
use arc_core::snapshot::Snapshot;
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

// Re-export for convenience, matching arc-es-sqlite.
pub use arc_core::{Deserialize, Serialize};

pub mod read_model_store;
pub use read_model_store::PostgresReadModelStore;

/// DDL for the append-only event log. Idempotent.
const EVENTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    id BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSON NOT NULL,
    "timestamp" BIGINT NOT NULL,
    actor_id TEXT NOT NULL DEFAULT 'legacy-pre-hipaa',
    actor_session_id TEXT,
    source_ip TEXT,
    user_agent TEXT,
    timestamp_utc_us BIGINT NOT NULL DEFAULT 0,
    causation_id TEXT,
    correlation_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    integrity_signature TEXT,
    integrity_key_id TEXT,
    UNIQUE(aggregate_type, aggregate_id, sequence)
);
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'events_aggregate_id_sequence_key'
    ) THEN
        ALTER TABLE events
            DROP CONSTRAINT events_aggregate_id_sequence_key;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'events_aggregate_type_aggregate_id_sequence_key'
    ) THEN
        ALTER TABLE events
            ADD CONSTRAINT events_aggregate_type_aggregate_id_sequence_key
            UNIQUE (aggregate_type, aggregate_id, sequence);
    END IF;
END $$;
DROP INDEX IF EXISTS idx_events_aggregate;
CREATE INDEX idx_events_aggregate
    ON events(aggregate_type, aggregate_id, sequence);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events("timestamp");
CREATE INDEX IF NOT EXISTS idx_events_actor_id ON events(actor_id);
CREATE INDEX IF NOT EXISTS idx_events_correlation_id ON events(correlation_id);
"#;

/// Forward migration for databases created before event payloads retained JSON
/// text. The type check makes this safe to run on both fresh and upgraded DBs.
const EVENTS_PAYLOAD_JSON_MIGRATION: &str =
    include_str!("../migrations/20261024_000001_event_payload_json.sql");

/// DDL for the snapshot table. Idempotent.
const SNAPSHOTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS snapshots (
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    state JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (aggregate_type, aggregate_id)
);
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'snapshots'::regclass
          AND contype = 'p'
          AND pg_get_constraintdef(oid) = 'PRIMARY KEY (aggregate_id)'
    ) THEN
        ALTER TABLE snapshots DROP CONSTRAINT snapshots_pkey;
        ALTER TABLE snapshots
            ADD CONSTRAINT snapshots_pkey
            PRIMARY KEY (aggregate_type, aggregate_id);
    END IF;
END $$;
"#;

/// Plain, DB-free representation of an event row. Splitting conversion out from
/// the query layer lets the row<->`Event` mapping be unit-tested without a live
/// Postgres server.
#[derive(Debug, Clone, PartialEq)]
struct EventRow {
    event_id: String,
    aggregate_type: String,
    aggregate_id: String,
    sequence: i64,
    event_type: String,
    payload: serde_json::Value,
    timestamp: i64,
    actor_id: String,
    actor_session_id: Option<String>,
    source_ip: Option<String>,
    user_agent: Option<String>,
    timestamp_utc_us: i64,
    causation_id: Option<String>,
    correlation_id: String,
    integrity_signature: Option<String>,
    integrity_key_id: Option<String>,
}

impl EventRow {
    fn from_event(
        event: &Event,
        integrity_signature: Option<String>,
        integrity_key_id: Option<String>,
    ) -> EventRow {
        // Stored in seconds to match the SQLite store's `timestamp` column unit.
        let timestamp_seconds: i64 = (event.timestamp / 1000) as i64;
        EventRow {
            event_id: event.event_id.to_string(),
            aggregate_type: event.aggregate_type.clone(),
            aggregate_id: event.aggregate_id.clone(),
            sequence: event.sequence,
            event_type: event.event_type.clone(),
            payload: event.payload.clone(),
            timestamp: timestamp_seconds,
            actor_id: event.audit.actor_id.clone(),
            actor_session_id: event.audit.actor_session_id.clone(),
            source_ip: event.audit.source_ip.clone(),
            user_agent: event.audit.user_agent.clone(),
            timestamp_utc_us: event.audit.timestamp_utc_us,
            causation_id: event.audit.causation_id.map(|u| u.to_string()),
            correlation_id: event.audit.correlation_id.to_string(),
            integrity_signature,
            integrity_key_id,
        }
    }

    fn to_event(&self) -> EventStoreResult<Event> {
        let event_id = Uuid::parse_str(&self.event_id)
            .map_err(|e| EventStoreError::serialization(format!("Invalid UUID: {}", e)))?;

        let causation_id = match self.causation_id.as_deref() {
            Some(s) => Some(Uuid::parse_str(s).map_err(|e| {
                EventStoreError::serialization(format!("Invalid causation UUID: {}", e))
            })?),
            None => None,
        };

        let correlation_id = Uuid::parse_str(&self.correlation_id).map_err(|e| {
            EventStoreError::serialization(format!("Invalid correlation UUID: {}", e))
        })?;

        let audit = AuditMetadata {
            actor_id: self.actor_id.clone(),
            actor_session_id: self.actor_session_id.clone(),
            source_ip: self.source_ip.clone(),
            user_agent: self.user_agent.clone(),
            timestamp_utc_us: self.timestamp_utc_us,
            causation_id,
            correlation_id,
        };

        Ok(Event {
            event_id,
            aggregate_type: self.aggregate_type.clone(),
            aggregate_id: self.aggregate_id.clone(),
            sequence: self.sequence,
            event_type: self.event_type.clone(),
            payload: self.payload.clone(),
            audit,
            timestamp: (self.timestamp as u64) * 1000,
        })
    }

    fn from_pg_row(row: &sqlx::postgres::PgRow) -> EventStoreResult<EventRow> {
        let map = |e: sqlx::Error| EventStoreError::database(e.to_string());
        Ok(EventRow {
            event_id: row.try_get("event_id").map_err(map)?,
            aggregate_type: row.try_get("aggregate_type").map_err(map)?,
            aggregate_id: row.try_get("aggregate_id").map_err(map)?,
            sequence: row.try_get("sequence").map_err(map)?,
            event_type: row.try_get("event_type").map_err(map)?,
            payload: serde_json::from_str(&row.try_get::<String, _>("payload_text").map_err(map)?)
                .map_err(|e| EventStoreError::serialization(e.to_string()))?,
            timestamp: row.try_get("timestamp").map_err(map)?,
            actor_id: row.try_get("actor_id").map_err(map)?,
            actor_session_id: row.try_get("actor_session_id").map_err(map)?,
            source_ip: row.try_get("source_ip").map_err(map)?,
            user_agent: row.try_get("user_agent").map_err(map)?,
            timestamp_utc_us: row.try_get("timestamp_utc_us").map_err(map)?,
            causation_id: row.try_get("causation_id").map_err(map)?,
            correlation_id: row.try_get("correlation_id").map_err(map)?,
            integrity_signature: row.try_get("integrity_signature").map_err(map)?,
            integrity_key_id: row.try_get("integrity_key_id").map_err(map)?,
        })
    }
}

/// A check that runs inside every write transaction of a
/// [`PostgresEventStore`], installed with
/// [`PostgresEventStore::with_write_guard`].
///
/// A guard lets an application make each event-store write conditional on
/// database state that Arc does not own, atomically with the write itself. A
/// typical use is a fencing check: the guard reads an application-owned epoch
/// row `FOR SHARE` and refuses the write when this process is no longer the
/// current writer, while the row lock keeps the epoch from advancing until the
/// write has committed.
///
/// # When it runs
///
/// The guard runs exactly once per write transaction:
///
/// - once per `append`, `append_to` or `append_to_with_publication` call,
///   regardless of how many events the batch holds;
/// - once per `acknowledge_publication` call;
/// - once per `save_snapshot` call.
///
/// It is **not** called for an empty append batch, for a batch rejected by
/// audit-metadata validation (both return before any transaction is opened),
/// for any read (`load*`, `stream_all`, `get_*version`, `pending_publications`,
/// `load_snapshot*`), or for `initialize_schema`.
///
/// # Transaction semantics
///
/// `check` receives the connection of the write transaction and is the first
/// statement after `BEGIN`, before Arc reads the stream version. Therefore:
///
/// - locks the guard takes are held until Arc commits or rolls back the write;
/// - anything the guard writes commits or rolls back together with the write;
/// - a guard failure takes precedence over
///   [`EventStoreError::ConcurrencyConflict`] and
///   [`EventStoreError::InvalidSequence`], which are detected later.
///
/// When `check` returns `Err`, Arc rolls the transaction back, persists
/// nothing, and returns the guard's error to the caller unchanged.
///
/// With a guard installed, `acknowledge_publication` and `save_snapshot` run
/// in an explicit transaction (guard, statement, commit) instead of a single
/// auto-commit statement. Without a guard they are unchanged.
///
/// # Rules for implementations
///
/// - Do not commit or roll back the transaction, and do not issue
///   `SET TRANSACTION`.
/// - Do not leave session state changed (for example `SET` without `LOCAL`,
///   session advisory locks, or temporary objects): the connection returns to
///   the pool afterwards.
/// - Avoid returning [`EventStoreError::ConcurrencyConflict`]; callers may
///   treat it as a retryable optimistic-concurrency failure.
/// - The guard runs under the pooled connection's own timeouts
///   (`statement_timeout`, `lock_timeout`, ...); Arc adds none. A guard that
///   blocks holds a pooled connection and delays the write.
///
/// # Scope
///
/// This is a PostgreSQL-only extension. The `arc-core` `EventStore` trait and
/// `arc-es-sqlite` are unchanged, and `PostgresReadModelStore` is not guarded.
#[async_trait]
pub trait WriteGuard: Send + Sync + 'static {
    /// Decide whether the write transaction on `conn` may proceed.
    async fn check(&self, conn: &mut sqlx::PgConnection) -> EventStoreResult<()>;
}

/// Postgres implementation of [`EventStore`].
#[derive(Clone)]
pub struct PostgresEventStore {
    pool: PgPool,
    integrity: Option<Arc<IntegrityConfig>>,
    write_guard: Option<Arc<dyn WriteGuard>>,
}

struct IntegrityConfig {
    chain: Arc<dyn IntegrityChain>,
    key_id: String,
}

impl PostgresEventStore {
    /// Install a [`WriteGuard`] that runs first inside every write transaction
    /// (appends, publication acknowledgements and snapshot saves). Chains after
    /// any constructor; calling it again replaces the previous guard. Clones of
    /// the store share the same guard.
    pub fn with_write_guard(mut self, guard: Arc<dyn WriteGuard>) -> Self {
        self.write_guard = Some(guard);
        self
    }

    /// Begin a write transaction and run the write guard, if any, as its first
    /// statement. A guard error rolls the transaction back and is returned
    /// unchanged.
    async fn begin_write(&self) -> EventStoreResult<sqlx::Transaction<'static, sqlx::Postgres>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        if let Some(guard) = self.write_guard.as_ref() {
            if let Err(error) = guard.check(&mut tx).await {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
        Ok(tx)
    }

    async fn append_internal(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        version_check: VersionCheck,
        new_events: Vec<Event>,
        queue_publication: bool,
    ) -> EventStoreResult<()> {
        if new_events.is_empty() {
            return Ok(());
        }

        // Defense-in-depth: reject any event with invalid audit before touching the DB.
        validate_audit_batch(aggregate_id, &new_events)?;

        let mut tx = self.begin_write().await?;

        let current_version: i64 = sqlx::query(
            "SELECT COALESCE(MAX(sequence), 0) AS v FROM events
             WHERE aggregate_type = $1 AND aggregate_id = $2",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?
        .try_get("v")
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        if let Some(expected) = version_check.version() {
            if current_version != expected {
                return Err(EventStoreError::ConcurrencyConflict {
                    aggregate_id: aggregate_id.to_string(),
                    expected,
                    actual: current_version,
                });
            }
        }

        for (expected_sequence, event) in (current_version + 1..).zip(new_events.iter()) {
            if event.sequence != expected_sequence {
                return Err(EventStoreError::InvalidSequence {
                    aggregate_id: aggregate_id.to_string(),
                    expected: expected_sequence,
                    actual: event.sequence,
                });
            }
        }

        let mut previous_signature = if self.integrity.is_some() {
            self.previous_signature_for_aggregate(
                &mut tx,
                Some(aggregate_type),
                aggregate_id,
                current_version + 1,
            )
            .await?
        } else {
            EventSignature::genesis()
        };

        for event in &new_events {
            let mut signature_str = None;
            let mut key_id_str = None;

            if let Some(integrity) = self.integrity.as_ref() {
                // Sign based on row-seconds timestamp parity with SQLite.
                let timestamp_seconds = (event.timestamp / 1000) as i64;
                let mut persisted_event = event.clone();
                persisted_event.timestamp = (timestamp_seconds as u64) * 1000;

                let signature = integrity
                    .chain
                    .sign_event(&previous_signature, &persisted_event)
                    .map_err(EventStoreError::from)?;
                previous_signature = signature.clone();
                signature_str = Some(signature.0);
                key_id_str = Some(integrity.key_id.clone());
            }

            let row = EventRow::from_event(event, signature_str, key_id_str);
            sqlx::query(
                r#"INSERT INTO events
                    (event_id, aggregate_type, aggregate_id, sequence, event_type, payload,
                     "timestamp", actor_id, actor_session_id, source_ip, user_agent,
                     timestamp_utc_us, causation_id, correlation_id,
                     integrity_signature, integrity_key_id)
                   VALUES ($1, $2, $3, $4, $5, $6::text::json, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"#,
            )
            .bind(&row.event_id)
            .bind(&row.aggregate_type)
            .bind(&row.aggregate_id)
            .bind(row.sequence)
            .bind(&row.event_type)
            .bind(serde_json::to_string(&row.payload).map_err(|e| {
                EventStoreError::serialization(format!("Failed to serialize event payload: {e}"))
            })?)
            .bind(row.timestamp)
            .bind(&row.actor_id)
            .bind(&row.actor_session_id)
            .bind(&row.source_ip)
            .bind(&row.user_agent)
            .bind(row.timestamp_utc_us)
            .bind(&row.causation_id)
            .bind(&row.correlation_id)
            .bind(&row.integrity_signature)
            .bind(&row.integrity_key_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
            if queue_publication {
                sqlx::query("INSERT INTO event_publications (event_id) VALUES ($1)")
                    .bind(event.event_id.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| EventStoreError::database(e.to_string()))?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(())
    }

    /// Build a store from a Postgres connection URL, creating a small pool.
    pub async fn new(database_url: &str) -> EventStoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(|e| EventStoreError::database(format!("Failed to create pool: {}", e)))?;
        Ok(PostgresEventStore {
            pool,
            integrity: None,
            write_guard: None,
        })
    }

    /// Build a store from a Postgres connection URL with an integrity key.
    pub async fn new_with_integrity_key(
        database_url: &str,
        key: impl Into<Vec<u8>>,
        key_id: impl Into<String>,
    ) -> EventStoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(|e| EventStoreError::database(format!("Failed to create pool: {}", e)))?;
        Ok(PostgresEventStore {
            pool,
            integrity: Some(Arc::new(IntegrityConfig {
                chain: Arc::new(HmacSha256Chain::new(key).map_err(EventStoreError::from)?),
                key_id: key_id.into(),
            })),
            write_guard: None,
        })
    }

    /// Build a store from an existing pool. Lets tests share one pool with the
    /// read-model store against the same database.
    pub fn with_pool(pool: PgPool) -> Self {
        PostgresEventStore {
            pool,
            integrity: None,
            write_guard: None,
        }
    }

    /// Build a store from an existing pool and an integrity key.
    pub fn with_pool_and_integrity_key(
        pool: PgPool,
        key: impl Into<Vec<u8>>,
        key_id: impl Into<String>,
    ) -> EventStoreResult<Self> {
        Ok(PostgresEventStore {
            pool,
            integrity: Some(Arc::new(IntegrityConfig {
                chain: Arc::new(HmacSha256Chain::new(key).map_err(EventStoreError::from)?),
                key_id: key_id.into(),
            })),
            write_guard: None,
        })
    }

    /// Borrow the underlying pool (e.g. to construct a read-model store that
    /// shares the same connections).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create the `events` and `snapshots` tables and their indexes if absent.
    /// Idempotent; safe to call on every startup.
    pub async fn initialize_schema(&self) -> EventStoreResult<()> {
        sqlx::raw_sql(EVENTS_SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        sqlx::raw_sql(EVENTS_PAYLOAD_JSON_MIGRATION)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS event_publications (event_id TEXT PRIMARY KEY REFERENCES events(event_id))")
            .execute(&self.pool).await.map_err(|e| EventStoreError::database(e.to_string()))?;
        sqlx::raw_sql(SNAPSHOTS_SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(())
    }

    async fn required_signature(
        &self,
        row: &EventRow,
        aggregate_id: &str,
        sequence: i64,
    ) -> EventStoreResult<EventSignature> {
        let _key_id = row.integrity_key_id.as_ref().ok_or_else(|| {
            EventStoreError::from(IntegrityError::BrokenAt {
                aggregate_id: aggregate_id.to_string(),
                sequence,
            })
        })?;

        row.integrity_signature
            .as_ref()
            .map(|s| EventSignature(s.clone()))
            .ok_or_else(|| {
                EventStoreError::from(IntegrityError::BrokenAt {
                    aggregate_id: aggregate_id.to_string(),
                    sequence,
                })
            })
    }

    async fn previous_signature_for_aggregate(
        &self,
        executor: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        aggregate_type: Option<&str>,
        aggregate_id: &str,
        before_sequence: i64,
    ) -> EventStoreResult<EventSignature> {
        if before_sequence <= 1 {
            return Ok(EventSignature::genesis());
        }

        let row = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events
             WHERE ($1::text IS NULL OR aggregate_type = $1)
               AND aggregate_id = $2 AND sequence < $3
             ORDER BY sequence DESC LIMIT 1",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(before_sequence)
        .fetch_optional(&mut **executor)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        match row {
            Some(r) => {
                let event_row = EventRow::from_pg_row(&r)?;
                self.required_signature(&event_row, aggregate_id, event_row.sequence)
                    .await
            }
            None => Ok(EventSignature::genesis()),
        }
    }

    async fn verify_integrity_rows(
        &self,
        integrity: &IntegrityConfig,
        rows: &[EventRow],
        previous_signature: EventSignature,
    ) -> EventStoreResult<Vec<Event>> {
        let mut previous = previous_signature;
        let mut events = Vec::with_capacity(rows.len());

        for row in rows {
            let event = row.to_event()?;
            let expected = integrity.chain.sign_event(&previous, &event)?;
            let claimed = self
                .required_signature(row, &event.aggregate_id, event.sequence)
                .await?;

            if expected != claimed {
                return Err(EventStoreError::from(IntegrityError::BrokenAt {
                    aggregate_id: event.aggregate_id,
                    sequence: event.sequence,
                }));
            }

            previous = claimed;
            events.push(event);
        }

        Ok(events)
    }

    async fn verify_stream_integrity_rows(
        &self,
        integrity: &IntegrityConfig,
        rows: &[EventRow],
    ) -> EventStoreResult<Vec<Event>> {
        use std::collections::HashMap;

        let mut previous_by_aggregate: HashMap<(String, String), EventSignature> = HashMap::new();
        let mut events = Vec::with_capacity(rows.len());

        for row in rows {
            let event = row.to_event()?;
            let stream = (event.aggregate_type.clone(), event.aggregate_id.clone());
            let previous = match previous_by_aggregate.get(&stream) {
                Some(sig) => sig.clone(),
                None => {
                    self.previous_signature_no_tx(
                        Some(&event.aggregate_type),
                        &event.aggregate_id,
                        event.sequence,
                    )
                    .await?
                }
            };

            let expected = integrity.chain.sign_event(&previous, &event)?;
            let claimed = self
                .required_signature(row, &event.aggregate_id, event.sequence)
                .await?;

            if expected != claimed {
                return Err(EventStoreError::from(IntegrityError::BrokenAt {
                    aggregate_id: event.aggregate_id,
                    sequence: event.sequence,
                }));
            }

            previous_by_aggregate.insert(stream, claimed);
            events.push(event);
        }

        Ok(events)
    }

    async fn previous_signature_no_tx(
        &self,
        aggregate_type: Option<&str>,
        aggregate_id: &str,
        before_sequence: i64,
    ) -> EventStoreResult<EventSignature> {
        if before_sequence <= 1 {
            return Ok(EventSignature::genesis());
        }

        let row = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events
             WHERE ($1::text IS NULL OR aggregate_type = $1)
               AND aggregate_id = $2 AND sequence < $3
             ORDER BY sequence DESC LIMIT 1",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(before_sequence)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        match row {
            Some(r) => {
                let event_row = EventRow::from_pg_row(&r)?;
                self.required_signature(&event_row, aggregate_id, event_row.sequence)
                    .await
            }
            None => Ok(EventSignature::genesis()),
        }
    }
}

#[async_trait]
impl EventStore for PostgresEventStore {
    async fn append(
        &self,
        aggregate_id: &str,
        version_check: VersionCheck,
        new_events: Vec<Event>,
    ) -> EventStoreResult<()> {
        let aggregate_type = new_events
            .first()
            .map(|event| event.aggregate_type.clone())
            .unwrap_or_default();
        self.append_to(&aggregate_type, aggregate_id, version_check, new_events)
            .await
    }

    fn supports_durable_publication(&self) -> bool {
        true
    }

    async fn append_to(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        version_check: VersionCheck,
        new_events: Vec<Event>,
    ) -> EventStoreResult<()> {
        self.append_internal(
            aggregate_type,
            aggregate_id,
            version_check,
            new_events,
            false,
        )
        .await
    }

    async fn append_to_with_publication(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        version_check: VersionCheck,
        new_events: Vec<Event>,
    ) -> EventStoreResult<()> {
        self.append_internal(
            aggregate_type,
            aggregate_id,
            version_check,
            new_events,
            true,
        )
        .await
    }

    async fn pending_publications(
        &self,
        aggregate_type: &str,
        limit: usize,
    ) -> EventStoreResult<Vec<Event>> {
        let identities = sqlx::query("SELECT e.aggregate_type, e.aggregate_id, e.sequence FROM event_publications p JOIN events e ON e.event_id = p.event_id WHERE e.aggregate_type = $1 ORDER BY e.id LIMIT $2")
            .bind(aggregate_type).bind(limit.min(1000) as i64)
            .fetch_all(&self.pool).await.map_err(|e| EventStoreError::database(e.to_string()))?;
        let mut pending = Vec::with_capacity(identities.len());
        for identity in identities {
            let aggregate_type: String = identity
                .try_get("aggregate_type")
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            let aggregate_id: String = identity
                .try_get("aggregate_id")
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            let sequence: i64 = identity
                .try_get("sequence")
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            let event = self
                .load_event(&aggregate_type, &aggregate_id, sequence)
                .await?
                .ok_or_else(|| {
                    EventStoreError::other("queued event is missing from authoritative store")
                })?;
            pending.push(event);
        }
        Ok(pending)
    }

    async fn acknowledge_publication(&self, event_id: Uuid) -> EventStoreResult<()> {
        let query = sqlx::query("DELETE FROM event_publications WHERE event_id = $1")
            .bind(event_id.to_string());
        if self.write_guard.is_none() {
            query
                .execute(&self.pool)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            return Ok(());
        }
        let mut tx = self.begin_write().await?;
        query
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(())
    }

    async fn load(&self, aggregate_id: &str) -> EventStoreResult<Vec<Event>> {
        self.load_from(aggregate_id, 1).await
    }

    async fn load_stream(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<Vec<Event>> {
        self.load_stream_from(aggregate_type, aggregate_id, 1).await
    }

    async fn load_event(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        sequence: i64,
    ) -> EventStoreResult<Option<Event>> {
        // The target event and the predecessor signature must come from one
        // database snapshot so integrity verification cannot mix revisions.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;

        let row = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events
             WHERE aggregate_type = $1 AND aggregate_id = $2 AND sequence = $3",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(sequence)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        let event = match row {
            Some(row) => {
                let event_row = EventRow::from_pg_row(&row)?;
                match self.integrity.as_ref() {
                    Some(integrity) => {
                        let previous = self
                            .previous_signature_for_aggregate(
                                &mut tx,
                                Some(aggregate_type),
                                aggregate_id,
                                sequence,
                            )
                            .await?;
                        self.verify_integrity_rows(integrity, &[event_row], previous)
                            .await?
                            .pop()
                    }
                    None => Some(event_row.to_event()?),
                }
            }
            None => None,
        };

        tx.commit()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(event)
    }

    async fn load_from(
        &self,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events WHERE aggregate_id = $1 AND sequence >= $2 ORDER BY sequence ASC",
        )
        .bind(aggregate_id)
        .bind(from_sequence)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        let event_rows: Vec<EventRow> = rows
            .iter()
            .map(EventRow::from_pg_row)
            .collect::<EventStoreResult<_>>()?;

        match self.integrity.as_ref() {
            Some(integrity) => {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|e| EventStoreError::database(e.to_string()))?;
                let previous = self
                    .previous_signature_for_aggregate(&mut tx, None, aggregate_id, from_sequence)
                    .await?;
                self.verify_integrity_rows(integrity, &event_rows, previous)
                    .await
            }
            None => event_rows
                .iter()
                .map(|r| r.to_event())
                .collect::<EventStoreResult<_>>(),
        }
    }

    async fn load_stream_from(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events
             WHERE aggregate_type = $1 AND aggregate_id = $2 AND sequence >= $3
             ORDER BY sequence ASC",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(from_sequence)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        let event_rows: Vec<EventRow> = rows
            .iter()
            .map(EventRow::from_pg_row)
            .collect::<EventStoreResult<_>>()?;

        match self.integrity.as_ref() {
            Some(integrity) => {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|e| EventStoreError::database(e.to_string()))?;
                let previous = self
                    .previous_signature_for_aggregate(
                        &mut tx,
                        Some(aggregate_type),
                        aggregate_id,
                        from_sequence,
                    )
                    .await?;
                self.verify_integrity_rows(integrity, &event_rows, previous)
                    .await
            }
            None => event_rows
                .iter()
                .map(|row| row.to_event())
                .collect::<EventStoreResult<_>>(),
        }
    }

    async fn stream_all(&self, from_position: i64) -> EventStoreResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT *, payload::text AS payload_text FROM events WHERE id >= $1 ORDER BY id ASC",
        )
        .bind(from_position)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        let event_rows: Vec<EventRow> = rows
            .iter()
            .map(EventRow::from_pg_row)
            .collect::<EventStoreResult<_>>()?;

        match self.integrity.as_ref() {
            Some(integrity) => {
                self.verify_stream_integrity_rows(integrity, &event_rows)
                    .await
            }
            None => event_rows
                .iter()
                .map(|r| r.to_event())
                .collect::<EventStoreResult<_>>(),
        }
    }

    async fn get_version(&self, aggregate_id: &str) -> EventStoreResult<i64> {
        let version: i64 = sqlx::query(
            "SELECT COALESCE(MAX(sequence), 0) AS v FROM events WHERE aggregate_id = $1",
        )
        .bind(aggregate_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?
        .try_get("v")
        .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(version)
    }

    async fn get_stream_version(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<i64> {
        let version: i64 = sqlx::query(
            "SELECT COALESCE(MAX(sequence), 0) AS v FROM events
             WHERE aggregate_type = $1 AND aggregate_id = $2",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?
        .try_get("v")
        .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(version)
    }

    async fn save_snapshot(&self, snapshot: &Snapshot) -> EventStoreResult<()> {
        // One snapshot per aggregate: replace in place rather than accumulating
        // stale versions.
        let query = sqlx::query(
            r#"INSERT INTO snapshots (aggregate_id, aggregate_type, version, state, created_at)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (aggregate_type, aggregate_id) DO UPDATE
                 SET version = EXCLUDED.version,
                     state = EXCLUDED.state,
                     created_at = EXCLUDED.created_at"#,
        )
        .bind(&snapshot.aggregate_id)
        .bind(&snapshot.aggregate_type)
        .bind(snapshot.version)
        .bind(&snapshot.state)
        .bind(snapshot.created_at as i64);
        if self.write_guard.is_none() {
            query
                .execute(&self.pool)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            return Ok(());
        }
        let mut tx = self.begin_write().await?;
        query
            .execute(&mut *tx)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
        Ok(())
    }

    async fn load_snapshot(&self, aggregate_id: &str) -> EventStoreResult<Option<Snapshot>> {
        let row = sqlx::query(
            "SELECT aggregate_id, aggregate_type, version, state, created_at \
             FROM snapshots WHERE aggregate_id = $1",
        )
        .bind(aggregate_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        match row {
            Some(r) => {
                let created_at: i64 = r
                    .try_get("created_at")
                    .map_err(|e| EventStoreError::database(e.to_string()))?;
                Ok(Some(Snapshot {
                    aggregate_id: r
                        .try_get("aggregate_id")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    aggregate_type: r
                        .try_get("aggregate_type")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    version: r
                        .try_get("version")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    state: r
                        .try_get("state")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    created_at: created_at as u64,
                }))
            }
            None => Ok(None),
        }
    }

    async fn load_snapshot_for(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<Option<Snapshot>> {
        let row = sqlx::query(
            "SELECT aggregate_id, aggregate_type, version, state, created_at
             FROM snapshots WHERE aggregate_type = $1 AND aggregate_id = $2",
        )
        .bind(aggregate_type)
        .bind(aggregate_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| EventStoreError::database(e.to_string()))?;

        match row {
            Some(row) => {
                let created_at: i64 = row
                    .try_get("created_at")
                    .map_err(|e| EventStoreError::database(e.to_string()))?;
                Ok(Some(Snapshot {
                    aggregate_id: row
                        .try_get("aggregate_id")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    aggregate_type: row
                        .try_get("aggregate_type")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    version: row
                        .try_get("version")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    state: row
                        .try_get("state")
                        .map_err(|e| EventStoreError::database(e.to_string()))?,
                    created_at: created_at as u64,
                }))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_core::aggregate::{Aggregate, Command};
    use arc_core::audit::AuditMetadata;
    use arc_core::command_bus::{CommandBus, CommandContext};
    use arc_core::event_bus::{EventBus, EventHandler, InProcessEventBus};
    use arc_core::projection::{Projection, ProjectionEngine, ProjectionResult};
    use serde_json::json;
    use sqlx::postgres::PgConnectOptions;
    use std::env;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn setup_test_store() -> Option<PostgresEventStore> {
        let url = env::var("ARC_POSTGRES_TEST_DATABASE_URL").ok()?;
        let store = PostgresEventStore::new(&url).await.unwrap();
        store.initialize_schema().await.unwrap();

        // Clean start for each test
        sqlx::query("TRUNCATE event_publications, events RESTART IDENTITY")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("TRUNCATE snapshots")
            .execute(store.pool())
            .await
            .unwrap();

        Some(store)
    }

    async fn setup_integrity_test_store() -> Option<PostgresEventStore> {
        let url = env::var("ARC_POSTGRES_TEST_DATABASE_URL").ok()?;
        let store = PostgresEventStore::new_with_integrity_key(&url, integrity_key(), "test-key")
            .await
            .unwrap();
        store.initialize_schema().await.unwrap();

        sqlx::query("TRUNCATE event_publications, events RESTART IDENTITY")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("TRUNCATE snapshots")
            .execute(store.pool())
            .await
            .unwrap();

        Some(store)
    }

    fn integrity_key() -> Vec<u8> {
        b"012345678901234567890123456789AB".to_vec()
    }

    fn stamped_event(
        agg_type: &str,
        agg_id: &str,
        sequence: i64,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Event {
        Event::new(NewEvent {
            aggregate_type: agg_type,
            aggregate_id: agg_id,
            sequence,
            event_type,
            payload,
        })
        .with_audit(AuditMetadata::test_default())
    }

    #[derive(Debug)]
    struct NumericCommand {
        aggregate_id: String,
        payload: serde_json::Value,
    }

    impl Command for NumericCommand {
        fn aggregate_id(&self) -> &str {
            &self.aggregate_id
        }
    }

    #[derive(Default)]
    struct NumericAggregate {
        version: i64,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("numeric aggregate error")]
    struct NumericAggregateError;

    #[async_trait]
    impl Aggregate for NumericAggregate {
        type Command = NumericCommand;
        type Event = ();
        type Error = NumericAggregateError;

        fn aggregate_type() -> &'static str {
            "Numeric"
        }

        fn version(&self) -> i64 {
            self.version
        }

        async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
            Ok(vec![Event::new(NewEvent {
                aggregate_type: "Numeric",
                aggregate_id: &command.aggregate_id,
                sequence: self.version + 1,
                event_type: "NumericCommanded",
                payload: command.payload,
            })])
        }

        fn apply(&mut self, event: &Event) {
            self.version = event.sequence;
        }
    }

    struct ApplyCounter(Arc<AtomicUsize>);

    #[async_trait]
    impl Projection for ApplyCounter {
        fn name(&self) -> &str {
            "numeric-apply-counter"
        }

        fn handles(&self) -> Vec<String> {
            vec!["NumericCommanded".to_string()]
        }

        async fn handle(&self, _event: &Event) -> ProjectionResult<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn clear(&self) -> ProjectionResult<()> {
            self.0.store(0, Ordering::SeqCst);
            Ok(())
        }
    }

    struct ExternalProjectionHandler(Arc<ProjectionEngine>);

    #[async_trait]
    impl EventHandler for ExternalProjectionHandler {
        fn handles(&self) -> Vec<String> {
            vec!["NumericCommanded".to_string()]
        }

        async fn handle(
            &self,
            event: &Event,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.0.process_external_delivery(event).await?;
            Ok(())
        }
    }

    #[derive(Default)]
    struct PublicationBus {
        fail_ack: std::sync::atomic::AtomicBool,
        delivered: std::sync::Mutex<Vec<Event>>,
    }
    #[async_trait]
    impl EventBus for PublicationBus {
        async fn publish(&self, events: Vec<Event>) -> arc_core::event_bus::EventBusResult<()> {
            self.delivered.lock().unwrap().extend(events);
            if self.fail_ack.load(Ordering::SeqCst) {
                Err(arc_core::event_bus::EventBusError::other(
                    "injected lost broker ack",
                ))
            } else {
                Ok(())
            }
        }
        async fn subscribe(
            &mut self,
            _: Box<dyn EventHandler>,
        ) -> arc_core::event_bus::EventBusResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn durable_publication_reopens_retains_failed_ack_and_filters_aggregate_type() {
        use arc_core::publication::relay_pending_publications;
        let Some(store) = setup_test_store().await else {
            return;
        };
        let event = stamped_event("User", "recover", 1, "UserCreated", json!({"n":1.0}))
            .with_stored_timestamp_precision();
        store
            .append_to_with_publication("User", "recover", VersionCheck::New, vec![event.clone()])
            .await
            .unwrap();
        let other = stamped_event("Product", "recover", 1, "ProductCreated", json!({}));
        store
            .append_to_with_publication("Product", "recover", VersionCheck::New, vec![other])
            .await
            .unwrap();
        store.pool().close().await;
        drop(store);
        let reopened =
            PostgresEventStore::new(&env::var("ARC_POSTGRES_TEST_DATABASE_URL").unwrap())
                .await
                .unwrap();
        assert!(reopened
            .pending_publications("User", 0)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened.pending_publications("User", 1).await.unwrap(),
            vec![event.clone()]
        );
        let bus = PublicationBus::default();
        bus.fail_ack.store(true, Ordering::SeqCst);
        assert!(relay_pending_publications(&reopened, &bus, "User", 100)
            .await
            .is_err());
        assert_eq!(
            reopened.pending_publications("User", 100).await.unwrap(),
            vec![event.clone()]
        );
        bus.fail_ack.store(false, Ordering::SeqCst);
        assert_eq!(
            relay_pending_publications(&reopened, &bus, "User", 100)
                .await
                .unwrap(),
            1
        );
        assert!(reopened
            .pending_publications("User", 100)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened
                .pending_publications("Product", 100)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(*bus.delivered.lock().unwrap(), vec![event.clone(), event]);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_append_and_load() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let event = stamped_event("User", "u1", 1, "Created", json!({}));
        store
            .append("u1", VersionCheck::New, vec![event])
            .await
            .unwrap();
        let loaded = store.load("u1").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sequence, 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_load_event_uses_exact_stream_identity() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let event = stamped_event("User", "shared-id", 1, "UserCreated", json!({"v": 1}));
        store
            .append_to("User", "shared-id", VersionCheck::New, vec![event.clone()])
            .await
            .unwrap();
        store
            .append_to(
                "Product",
                "shared-id",
                VersionCheck::New,
                vec![stamped_event(
                    "Product",
                    "shared-id",
                    1,
                    "ProductCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();

        let loaded = store.load_event("User", "shared-id", 1).await.unwrap();
        assert_eq!(
            loaded.as_ref().map(|loaded| loaded.event_id),
            Some(event.event_id)
        );
        assert!(store
            .load_event("Order", "shared-id", 1)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .load_event("User", "shared-id", 2)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_load_event_verifies_integrity_signature() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        let mut first = stamped_event("User", "integrity-user", 1, "UserCreated", json!({"v": 1}));
        first.timestamp = 1_700_000_000_000;
        let mut second = stamped_event("User", "integrity-user", 2, "UserUpdated", json!({"v": 2}));
        second.timestamp = 1_700_000_001_000;
        store
            .append_to(
                "User",
                "integrity-user",
                VersionCheck::New,
                vec![first, second.clone()],
            )
            .await
            .unwrap();

        let loaded = store.load_event("User", "integrity-user", 2).await.unwrap();
        assert_eq!(loaded, Some(second));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_json_payload_round_trips_numeric_forms_without_integrity() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"nested":{"numbers":[1e18,1,1.0,0.125,1e-18,-0.0,-9223372036854775808,18446744073709551615]}}"#,
        )
        .unwrap();
        let mut event = stamped_event("Numeric", "json-precision", 1, "NumericEvent", payload);
        event.timestamp = 1_700_000_000_000;
        store
            .append_to(
                "Numeric",
                "json-precision",
                VersionCheck::New,
                vec![event.clone()],
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .load_event("Numeric", "json-precision", 1)
                .await
                .unwrap(),
            Some(event)
        );
        let payload_type: String = sqlx::query_scalar(
            "SELECT t.typname FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_type t ON t.oid = a.atttypid WHERE c.relname = 'events' AND a.attname = 'payload' AND NOT a.attisdropped",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(payload_type, "json");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_json_payload_round_trips_numeric_forms_with_integrity() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"nested":{"numbers":[1e18,1,1.0,0.125,1e-18,-0.0,-9223372036854775808,18446744073709551615]}}"#,
        )
        .unwrap();
        let mut event = stamped_event("Numeric", "signed-precision", 1, "NumericEvent", payload);
        event.timestamp = 1_700_000_000_000;
        store
            .append_to(
                "Numeric",
                "signed-precision",
                VersionCheck::New,
                vec![event.clone()],
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .load_event("Numeric", "signed-precision", 1)
                .await
                .unwrap(),
            Some(event)
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_numeric_command_publish_and_external_projection_round_trip() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let applied = Arc::new(AtomicUsize::new(0));
        let mut engine = ProjectionEngine::new(Box::new(store.clone()));
        engine.register(Box::new(ApplyCounter(applied.clone())));
        let engine = Arc::new(engine);

        let mut event_bus = InProcessEventBus::new();
        event_bus
            .subscribe(Box::new(ExternalProjectionHandler(engine)))
            .await
            .unwrap();
        let command_bus =
            CommandBus::<NumericAggregate>::new(Box::new(store.clone()), Box::new(event_bus));
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"nested":{"numbers":[1e18,1,1.0,0.125,1e-18,-0.0,-9223372036854775808,18446744073709551615]}}"#,
        )
        .unwrap();
        let events = command_bus
            .dispatch(
                NumericCommand {
                    aggregate_id: "numeric-command-publish".into(),
                    payload: payload.clone(),
                },
                CommandContext::for_actor("numeric-test"),
            )
            .await
            .unwrap();

        assert_eq!(events[0].payload, payload);
        assert_eq!(events[0].timestamp % 1_000, 0);
        assert_eq!(
            store
                .load_event("Numeric", "numeric-command-publish", 1)
                .await
                .unwrap(),
            Some(events[0].clone())
        );
        assert_eq!(applied.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_jsonb_upgrade_preserves_signature_and_migration_is_repeatable() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        let mut original = stamped_event(
            "Upgrade",
            "signed-jsonb-upgrade",
            1,
            "Created",
            json!({"safe": 7}),
        );
        original.timestamp = 1_700_000_000_000;
        store
            .append_to(
                "Upgrade",
                "signed-jsonb-upgrade",
                VersionCheck::New,
                vec![original.clone()],
            )
            .await
            .unwrap();
        let signature_before: String = sqlx::query_scalar(
            "SELECT integrity_signature FROM events WHERE aggregate_id = 'signed-jsonb-upgrade'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();

        sqlx::query("ALTER TABLE events ALTER COLUMN payload TYPE JSONB USING payload::jsonb")
            .execute(store.pool())
            .await
            .unwrap();
        store.initialize_schema().await.unwrap();
        let signature_after: String = sqlx::query_scalar(
            "SELECT integrity_signature FROM events WHERE aggregate_id = 'signed-jsonb-upgrade'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(
            signature_after, signature_before,
            "migration must not reseal history"
        );
        assert_eq!(
            store
                .load_event("Upgrade", "signed-jsonb-upgrade", 1)
                .await
                .unwrap(),
            Some(original)
        );

        // Once applied, the startup check is a catalog lookup: the table keeps
        // its storage file, so no rewrite and no long ACCESS EXCLUSIVE lock.
        let relfilenode = "SELECT relfilenode::text FROM pg_class WHERE oid = 'events'::regclass";
        let storage_before: String = sqlx::query_scalar(relfilenode)
            .fetch_one(store.pool())
            .await
            .unwrap();
        store.initialize_schema().await.unwrap();
        let storage_after: String = sqlx::query_scalar(relfilenode)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(
            storage_after, storage_before,
            "an applied payload migration must not rewrite the events table"
        );
        let mut precise = stamped_event(
            "Upgrade",
            "signed-jsonb-upgrade",
            2,
            "Updated",
            serde_json::from_str(r#"{"n":1e18,"f":1.0,"u":18446744073709551615}"#).unwrap(),
        );
        precise.timestamp = 1_700_000_001_000;
        store
            .append_to(
                "Upgrade",
                "signed-jsonb-upgrade",
                VersionCheck::Expected(1),
                vec![precise.clone()],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .load_event("Upgrade", "signed-jsonb-upgrade", 2)
                .await
                .unwrap(),
            Some(precise)
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_jsonb_normalized_signature_fails_closed_without_resealing() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        let mut original = stamped_event(
            "Upgrade",
            "normalized-jsonb-signature",
            1,
            "NumericCreated",
            serde_json::from_str(r#"{"value":1e18}"#).unwrap(),
        );
        original.timestamp = 1_700_000_000_000;
        store
            .append_to(
                "Upgrade",
                "normalized-jsonb-signature",
                VersionCheck::New,
                vec![original],
            )
            .await
            .unwrap();
        let signature_before: String = sqlx::query_scalar(
            "SELECT integrity_signature FROM events WHERE aggregate_id = 'normalized-jsonb-signature'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();

        sqlx::query("ALTER TABLE events ALTER COLUMN payload TYPE JSONB USING payload::jsonb")
            .execute(store.pool())
            .await
            .unwrap();
        store.initialize_schema().await.unwrap();
        let signature_after: String = sqlx::query_scalar(
            "SELECT integrity_signature FROM events WHERE aggregate_id = 'normalized-jsonb-signature'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();

        assert_eq!(
            signature_after, signature_before,
            "migration must never reseal history"
        );
        assert!(matches!(
            store
                .load_event("Upgrade", "normalized-jsonb-signature", 1)
                .await,
            Err(EventStoreError::Integrity { .. })
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_payload_migration_scopes_same_named_tables_by_search_path() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let url = env::var("ARC_POSTGRES_TEST_DATABASE_URL").unwrap();
        let other_schema = format!("arc_jsonb_events_{}", std::process::id());
        sqlx::query(&format!("CREATE SCHEMA {other_schema}"))
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(&format!(
            "CREATE TABLE {other_schema}.events (payload JSONB NOT NULL)"
        ))
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(&format!(
            "INSERT INTO {other_schema}.events (payload) VALUES ('{{\"large\": 1000000000000000000}}'::jsonb)"
        ))
        .execute(store.pool())
        .await
        .unwrap();

        let search_path = format!("public,{other_schema}");
        let options = PgConnectOptions::from_str(&url).unwrap().options([
            ("search_path", search_path.as_str()),
            ("lock_timeout", "250ms"),
        ]);
        let target_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let target_store = PostgresEventStore::with_pool(target_pool);
        target_store.initialize_schema().await.unwrap();
        target_store.initialize_schema().await.unwrap();

        let lock_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let mut lock_tx = lock_pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE public.events IN ACCESS SHARE MODE")
            .execute(&mut *lock_tx)
            .await
            .unwrap();
        // Isolate the migration's idempotent lock behavior from the existing
        // startup index refresh, which independently acquires table locks.
        for _ in 0..2 {
            sqlx::raw_sql(EVENTS_PAYLOAD_JSON_MIGRATION)
                .execute(target_store.pool())
                .await
                .unwrap();
        }
        lock_tx.rollback().await.unwrap();

        let target_type: String = sqlx::query_scalar(
            "SELECT t.typname FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid WHERE a.attrelid = 'public.events'::regclass AND a.attname = 'payload' AND NOT a.attisdropped",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let other_type: String = sqlx::query_scalar(
            &format!("SELECT t.typname FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid WHERE a.attrelid = '{other_schema}.events'::regclass AND a.attname = 'payload' AND NOT a.attisdropped"),
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let other_payload: serde_json::Value =
            sqlx::query_scalar(&format!("SELECT payload FROM {other_schema}.events"))
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(target_type, "json");
        assert_eq!(
            other_type, "jsonb",
            "migration must not alter a same-named table outside its search path target"
        );
        assert_eq!(
            other_payload,
            json!({"large": 1_000_000_000_000_000_000_u64})
        );

        target_store.pool().close().await;
        lock_pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {other_schema} CASCADE"))
            .execute(store.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_numeric_payload_mutation_changes_authoritative_event() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        let mut event = stamped_event(
            "Numeric",
            "numeric-tamper",
            1,
            "NumericEvent",
            serde_json::from_str(r#"{"value":1e18}"#).unwrap(),
        );
        event.timestamp = 1_700_000_000_000;
        store
            .append_to(
                "Numeric",
                "numeric-tamper",
                VersionCheck::New,
                vec![event.clone()],
            )
            .await
            .unwrap();
        sqlx::query(
            "UPDATE events SET payload = '{\"value\":1.0}' WHERE aggregate_id = 'numeric-tamper'",
        )
        .execute(store.pool())
        .await
        .unwrap();

        assert!(matches!(
            store.load_event("Numeric", "numeric-tamper", 1).await,
            Err(EventStoreError::Integrity { .. })
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_numeric_envelope_mismatch_rejects_without_projection_mutation() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        let event = stamped_event(
            "Numeric",
            "numeric-envelope-tamper",
            1,
            "NumericCommanded",
            serde_json::from_str(r#"{"value":1e18}"#).unwrap(),
        );
        store
            .append_to(
                "Numeric",
                "numeric-envelope-tamper",
                VersionCheck::New,
                vec![event.clone()],
            )
            .await
            .unwrap();
        let applied = Arc::new(AtomicUsize::new(0));
        let mut engine = ProjectionEngine::new(Box::new(store));
        engine.register(Box::new(ApplyCounter(applied.clone())));
        let mut modified = event;
        modified.payload = serde_json::from_str(r#"{"value":1.0}"#).unwrap();

        let result = engine.process_external_delivery(&modified).await;
        assert!(matches!(
            result,
            Err(arc_core::projection::ExternalProjectionError::EventMismatch)
        ));
        assert_eq!(applied.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_same_id_is_isolated_by_aggregate_type() {
        let Some(store) = setup_test_store().await else {
            return;
        };
        store
            .append_to(
                "Product",
                "shared-id",
                VersionCheck::New,
                vec![stamped_event(
                    "Product",
                    "shared-id",
                    1,
                    "ProductCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();
        store
            .append_to(
                "Order",
                "shared-id",
                VersionCheck::New,
                vec![stamped_event(
                    "Order",
                    "shared-id",
                    1,
                    "OrderPlaced",
                    json!({}),
                )],
            )
            .await
            .unwrap();

        assert_eq!(
            store.load_stream("Product", "shared-id").await.unwrap()[0].event_type,
            "ProductCreated"
        );
        assert_eq!(
            store.load_stream("Order", "shared-id").await.unwrap()[0].event_type,
            "OrderPlaced"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_integrity_append_persists_signatures() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        store
            .append(
                "signed-1",
                VersionCheck::New,
                vec![
                    stamped_event("User", "signed-1", 1, "Created", json!({})),
                    stamped_event("User", "signed-1", 2, "Updated", json!({})),
                ],
            )
            .await
            .unwrap();

        let rows = sqlx::query(
            "SELECT integrity_signature, integrity_key_id FROM events ORDER BY sequence",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();

        assert_eq!(rows.len(), 2);
        for row in rows {
            let sig: String = row.get("integrity_signature");
            let kid: String = row.get("integrity_key_id");
            assert_eq!(sig.len(), 64);
            assert_eq!(kid, "test-key");
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_integrity_load_rejects_tampered_payload() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        store
            .append(
                "tamper-1",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "tamper-1",
                    1,
                    "Created",
                    json!({"ok": true}),
                )],
            )
            .await
            .unwrap();

        sqlx::query(
            "UPDATE events SET payload = '{\"ok\": false}' WHERE aggregate_id = 'tamper-1'",
        )
        .execute(store.pool())
        .await
        .unwrap();

        let err = store.load("tamper-1").await.unwrap_err();
        assert!(matches!(err, EventStoreError::Integrity { .. }));
        assert!(matches!(
            store.load_event("User", "tamper-1", 1).await,
            Err(EventStoreError::Integrity { .. })
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_integrity_load_rejects_missing_signature() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        store
            .append(
                "missing-sig",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "missing-sig",
                    1,
                    "Created",
                    json!({}),
                )],
            )
            .await
            .unwrap();

        sqlx::query(
            "UPDATE events SET integrity_signature = NULL WHERE aggregate_id = 'missing-sig'",
        )
        .execute(store.pool())
        .await
        .unwrap();

        let err = store.load("missing-sig").await.unwrap_err();
        assert!(matches!(err, EventStoreError::Integrity { .. }));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_integrity_stream_all_verifies_per_aggregate() {
        let Some(store) = setup_integrity_test_store().await else {
            return;
        };
        store
            .append(
                "a",
                VersionCheck::New,
                vec![stamped_event("U", "a", 1, "X", json!({}))],
            )
            .await
            .unwrap();
        store
            .append(
                "b",
                VersionCheck::New,
                vec![stamped_event("U", "b", 1, "X", json!({}))],
            )
            .await
            .unwrap();
        store
            .append(
                "a",
                VersionCheck::Expected(1),
                vec![stamped_event("U", "a", 2, "Y", json!({}))],
            )
            .await
            .unwrap();

        let loaded = store.stream_all(0).await.unwrap();
        assert_eq!(loaded.len(), 3);
    }

    // ---- Write guard -------------------------------------------------------

    const GUARD_SCRATCH: &str = "arc_write_guard_scratch";

    struct CountingGuard(Arc<AtomicUsize>);

    #[async_trait]
    impl WriteGuard for CountingGuard {
        async fn check(&self, _conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Refuses while `refuse` is set; counts every invocation.
    struct SwitchGuard {
        refuse: std::sync::atomic::AtomicBool,
        calls: AtomicUsize,
    }

    impl SwitchGuard {
        fn new(refuse: bool) -> Arc<Self> {
            Arc::new(Self {
                refuse: std::sync::atomic::AtomicBool::new(refuse),
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl WriteGuard for SwitchGuard {
        async fn check(&self, _conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.refuse.load(Ordering::SeqCst) {
                Err(EventStoreError::other("guard refused"))
            } else {
                Ok(())
            }
        }
    }

    /// Records the guard's transaction id and optionally writes a marker row.
    struct TxidGuard {
        txids: std::sync::Mutex<Vec<i64>>,
        insert_marker: bool,
    }

    #[async_trait]
    impl WriteGuard for TxidGuard {
        async fn check(&self, conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
            let txid: i64 = sqlx::query_scalar("SELECT txid_current()")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            self.txids.lock().unwrap().push(txid);
            if self.insert_marker {
                sqlx::query(&format!(
                    "INSERT INTO {GUARD_SCRATCH} (id, epoch) VALUES (99, 0)"
                ))
                .execute(&mut *conn)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            }
            Ok(())
        }
    }

    /// Share-locks the scratch row, signals, then waits to be released.
    struct LockingGuard {
        locked: tokio::sync::mpsc::UnboundedSender<()>,
        release: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
    }

    #[async_trait]
    impl WriteGuard for LockingGuard {
        async fn check(&self, conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
            sqlx::query(&format!(
                "SELECT epoch FROM {GUARD_SCRATCH} WHERE id = 1 FOR SHARE"
            ))
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| EventStoreError::database(e.to_string()))?;
            self.locked.send(()).unwrap();
            self.release.lock().await.recv().await;
            Ok(())
        }
    }

    fn assert_guard_refused(result: EventStoreResult<()>) {
        match result {
            Err(EventStoreError::Other { message }) => assert_eq!(message, "guard refused"),
            other => panic!("expected the guard's own error, got {other:?}"),
        }
    }

    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://arc:arc@127.0.0.1:1/arc_unreachable")
            .unwrap()
    }

    async fn create_guard_scratch(pool: &PgPool) {
        sqlx::raw_sql(&format!(
            "DROP TABLE IF EXISTS {GUARD_SCRATCH};
             CREATE TABLE {GUARD_SCRATCH} (id INT PRIMARY KEY, epoch BIGINT NOT NULL);
             INSERT INTO {GUARD_SCRATCH} (id, epoch) VALUES (1, 1);"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn drop_guard_scratch(pool: &PgPool) {
        sqlx::raw_sql(&format!("DROP TABLE IF EXISTS {GUARD_SCRATCH}"))
            .execute(pool)
            .await
            .unwrap();
    }

    async fn count_rows(pool: &PgPool, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn write_guard_builder_composes_after_constructors_and_clone_shares_guard() {
        let plain = PostgresEventStore::with_pool(lazy_pool());
        assert!(plain.write_guard.is_none());

        let first: Arc<dyn WriteGuard> = Arc::new(CountingGuard(Arc::new(AtomicUsize::new(0))));
        let second: Arc<dyn WriteGuard> = Arc::new(CountingGuard(Arc::new(AtomicUsize::new(0))));

        let store = plain.with_write_guard(first.clone());
        assert!(Arc::ptr_eq(store.write_guard.as_ref().unwrap(), &first));
        assert!(store.integrity.is_none());
        let cloned = store.clone();
        assert!(Arc::ptr_eq(cloned.write_guard.as_ref().unwrap(), &first));

        // A second call replaces the guard.
        let replaced = store.with_write_guard(second.clone());
        assert!(Arc::ptr_eq(replaced.write_guard.as_ref().unwrap(), &second));

        let signed =
            PostgresEventStore::with_pool_and_integrity_key(lazy_pool(), integrity_key(), "k")
                .unwrap();
        assert!(signed.write_guard.is_none());
        let signed = signed.with_write_guard(first.clone());
        assert!(signed.integrity.is_some());
        assert!(Arc::ptr_eq(signed.write_guard.as_ref().unwrap(), &first));
    }

    #[tokio::test]
    async fn write_guard_not_invoked_for_empty_batch_or_invalid_audit() {
        let count = Arc::new(AtomicUsize::new(0));
        let store = PostgresEventStore::with_pool(lazy_pool())
            .with_write_guard(Arc::new(CountingGuard(count.clone())));

        store
            .append("empty", VersionCheck::New, Vec::new())
            .await
            .unwrap();
        store
            .append_to("User", "empty", VersionCheck::New, Vec::new())
            .await
            .unwrap();
        store
            .append_to_with_publication("User", "empty", VersionCheck::New, Vec::new())
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // Built without with_audit: audit stays pending and is rejected before
        // any transaction (the lazy pool is unreachable).
        let unaudited = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "bad",
            sequence: 1,
            event_type: "Created",
            payload: json!({}),
        });
        let err = store
            .append("bad", VersionCheck::New, vec![unaudited])
            .await
            .unwrap_err();
        assert!(matches!(err, EventStoreError::InvalidAudit { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_write_guard_runs_once_per_write_and_never_for_reads() {
        let Some(base) = setup_test_store().await else {
            return;
        };
        let count = Arc::new(AtomicUsize::new(0));
        let store = base
            .clone()
            .with_write_guard(Arc::new(CountingGuard(count.clone())));

        store
            .append(
                "g1",
                VersionCheck::New,
                vec![
                    stamped_event("User", "g1", 1, "Created", json!({})),
                    stamped_event("User", "g1", 2, "Renamed", json!({})),
                ],
            )
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let published = stamped_event("User", "g1", 3, "Published", json!({}));
        store
            .append_to_with_publication(
                "User",
                "g1",
                VersionCheck::Expected(2),
                vec![published.clone()],
            )
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(count_rows(base.pool(), "event_publications").await, 1);

        store
            .acknowledge_publication(published.event_id)
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);

        store
            .save_snapshot(&Snapshot::new("g1", "User", 3, json!({"n": 3})))
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 4);

        // Everything persisted.
        assert_eq!(count_rows(base.pool(), "events").await, 3);
        assert_eq!(count_rows(base.pool(), "event_publications").await, 0);
        assert_eq!(count_rows(base.pool(), "snapshots").await, 1);

        // Reads never invoke the guard.
        assert_eq!(store.load("g1").await.unwrap().len(), 3);
        assert_eq!(store.load_stream("User", "g1").await.unwrap().len(), 3);
        assert!(store.load_event("User", "g1", 2).await.unwrap().is_some());
        assert_eq!(store.load_from("g1", 2).await.unwrap().len(), 2);
        assert_eq!(
            store.load_stream_from("User", "g1", 3).await.unwrap().len(),
            1
        );
        assert_eq!(store.stream_all(0).await.unwrap().len(), 3);
        assert_eq!(store.get_version("g1").await.unwrap(), 3);
        assert_eq!(store.get_stream_version("User", "g1").await.unwrap(), 3);
        assert!(store
            .pending_publications("User", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(store.load_snapshot("g1").await.unwrap().unwrap().version, 3);
        assert_eq!(
            store
                .load_snapshot_for("User", "g1")
                .await
                .unwrap()
                .unwrap()
                .version,
            3
        );
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_write_guard_rejection_persists_nothing_and_returns_guard_error() {
        let Some(base) = setup_test_store().await else {
            return;
        };
        let pool = base.pool().clone();

        // Seed through an unguarded store on the same pool.
        let seeded = stamped_event("User", "r1", 1, "Created", json!({}));
        base.append_to_with_publication("User", "r1", VersionCheck::New, vec![seeded.clone()])
            .await
            .unwrap();
        base.save_snapshot(&Snapshot::new("r1", "User", 1, json!({"v": 1})))
            .await
            .unwrap();

        async fn assert_unchanged(base: &PostgresEventStore, seeded: &Event) {
            assert_eq!(count_rows(base.pool(), "events").await, 1);
            assert_eq!(count_rows(base.pool(), "event_publications").await, 1);
            let pending: String = sqlx::query_scalar("SELECT event_id FROM event_publications")
                .fetch_one(base.pool())
                .await
                .unwrap();
            assert_eq!(pending, seeded.event_id.to_string());
            let snapshot = base.load_snapshot_for("User", "r1").await.unwrap().unwrap();
            assert_eq!(snapshot.version, 1);
            assert_eq!(snapshot.state, json!({"v": 1}));
        }

        let guard = SwitchGuard::new(true);
        let store = PostgresEventStore::with_pool(pool.clone()).with_write_guard(guard.clone());

        assert_guard_refused(
            store
                .append(
                    "r1",
                    VersionCheck::Expected(1),
                    vec![stamped_event("User", "r1", 2, "Renamed", json!({}))],
                )
                .await,
        );
        assert_unchanged(&base, &seeded).await;

        assert_guard_refused(
            store
                .append_to(
                    "User",
                    "r2",
                    VersionCheck::New,
                    vec![stamped_event("User", "r2", 1, "Created", json!({}))],
                )
                .await,
        );
        assert_unchanged(&base, &seeded).await;

        assert_guard_refused(
            store
                .append_to_with_publication(
                    "User",
                    "r3",
                    VersionCheck::New,
                    vec![stamped_event("User", "r3", 1, "Created", json!({}))],
                )
                .await,
        );
        assert_unchanged(&base, &seeded).await;

        assert_guard_refused(store.acknowledge_publication(seeded.event_id).await);
        assert_unchanged(&base, &seeded).await;

        assert_guard_refused(
            store
                .save_snapshot(&Snapshot::new("r1", "User", 2, json!({"v": 2})))
                .await,
        );
        assert_unchanged(&base, &seeded).await;
        assert_eq!(guard.calls.load(Ordering::SeqCst), 5);

        // Guard failure wins over the later concurrency check.
        assert_guard_refused(
            store
                .append(
                    "r1",
                    VersionCheck::Expected(7),
                    vec![stamped_event("User", "r1", 8, "Renamed", json!({}))],
                )
                .await,
        );
        assert_unchanged(&base, &seeded).await;

        // Same for the integrity-enabled store.
        let signed_guard = SwitchGuard::new(true);
        let signed = PostgresEventStore::with_pool_and_integrity_key(pool, integrity_key(), "k")
            .unwrap()
            .with_write_guard(signed_guard.clone());
        assert_guard_refused(
            signed
                .append(
                    "r4",
                    VersionCheck::New,
                    vec![stamped_event("User", "r4", 1, "Created", json!({}))],
                )
                .await,
        );
        assert_eq!(signed_guard.calls.load(Ordering::SeqCst), 1);
        assert_unchanged(&base, &seeded).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_write_guard_shares_the_write_transaction() {
        let Some(base) = setup_test_store().await else {
            return;
        };
        let pool = base.pool().clone();
        create_guard_scratch(&pool).await;

        let guard = Arc::new(TxidGuard {
            txids: std::sync::Mutex::new(Vec::new()),
            insert_marker: false,
        });
        let store = base.clone().with_write_guard(guard.clone());

        store
            .append(
                "t1",
                VersionCheck::New,
                vec![stamped_event("User", "t1", 1, "Created", json!({}))],
            )
            .await
            .unwrap();
        let event_xmin: i64 =
            sqlx::query_scalar("SELECT xmin::text::bigint FROM events WHERE aggregate_id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();

        store
            .save_snapshot(&Snapshot::new("t1", "User", 1, json!({})))
            .await
            .unwrap();
        // Upsert path: the replaced row version is written by the guard's tx too.
        store
            .save_snapshot(&Snapshot::new("t1", "User", 1, json!({"again": true})))
            .await
            .unwrap();
        let snapshot_xmin: i64 = sqlx::query_scalar(
            "SELECT xmin::text::bigint FROM snapshots WHERE aggregate_id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let txids = guard.txids.lock().unwrap().clone();
        assert_eq!(txids.len(), 3);
        // xmin is the 32-bit xid; txid_current() carries the epoch in the high bits.
        assert_eq!(event_xmin, txids[0] & 0xFFFF_FFFF);
        assert_eq!(snapshot_xmin, txids[2] & 0xFFFF_FFFF);
        assert_ne!(txids[0], txids[1]);
        assert_ne!(txids[1], txids[2]);

        // What the guard writes rolls back with a failed write.
        let marking = Arc::new(TxidGuard {
            txids: std::sync::Mutex::new(Vec::new()),
            insert_marker: true,
        });
        let marking_store = base.clone().with_write_guard(marking.clone());
        let err = marking_store
            .append(
                "t1",
                VersionCheck::Expected(5),
                vec![stamped_event("User", "t1", 6, "Renamed", json!({}))],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EventStoreError::ConcurrencyConflict { .. }));
        assert_eq!(marking.txids.lock().unwrap().len(), 1);
        let markers: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {GUARD_SCRATCH} WHERE id = 99"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(markers, 0);

        // ...and commits with a successful one.
        marking_store
            .append(
                "t1",
                VersionCheck::Expected(1),
                vec![stamped_event("User", "t1", 2, "Renamed", json!({}))],
            )
            .await
            .unwrap();
        let markers: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {GUARD_SCRATCH} WHERE id = 99"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(markers, 1);

        drop_guard_scratch(&pool).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_write_guard_lock_is_held_until_commit() {
        use sqlx::Connection;
        use std::time::Duration;

        let Some(base) = setup_test_store().await else {
            return;
        };
        let url = env::var("ARC_POSTGRES_TEST_DATABASE_URL").unwrap();
        let pool = base.pool().clone();
        create_guard_scratch(&pool).await;

        let (locked_tx, mut locked_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel();
        let store = base.clone().with_write_guard(Arc::new(LockingGuard {
            locked: locked_tx,
            release: tokio::sync::Mutex::new(release_rx),
        }));

        let append = tokio::spawn(async move {
            store
                .append(
                    "l1",
                    VersionCheck::New,
                    vec![stamped_event("User", "l1", 1, "Created", json!({}))],
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), locked_rx.recv())
            .await
            .expect("guard never took its lock")
            .unwrap();

        // While the guard's transaction is open, a writer of the row is blocked.
        let mut impatient = sqlx::PgConnection::connect(&url).await.unwrap();
        sqlx::query("SET lock_timeout = '200ms'")
            .execute(&mut impatient)
            .await
            .unwrap();
        let blocked = sqlx::query(&format!(
            "UPDATE {GUARD_SCRATCH} SET epoch = epoch + 1 WHERE id = 1"
        ))
        .execute(&mut impatient)
        .await
        .unwrap_err();
        assert_eq!(
            blocked
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("55P03")
        );
        impatient.close().await.unwrap();
        assert!(!append.is_finished());

        // A patient updater queues behind the guard's lock.
        let mut patient = sqlx::PgConnection::connect(&url).await.unwrap();
        let updater = tokio::spawn(async move {
            let mut tx = patient.begin().await.unwrap();
            sqlx::query(&format!(
                "UPDATE {GUARD_SCRATCH} SET epoch = epoch + 1 WHERE id = 1"
            ))
            .execute(&mut *tx)
            .await
            .unwrap();
            let visible: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE aggregate_id = 'l1'")
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
            tx.commit().await.unwrap();
            patient.close().await.unwrap();
            visible
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !updater.is_finished(),
            "updater was not blocked by the guard"
        );
        assert_eq!(count_rows(&pool, "events").await, 0);

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), append)
            .await
            .expect("append did not finish")
            .unwrap()
            .unwrap();
        // The updater could only proceed after the append committed, so it
        // sees the event.
        let visible = tokio::time::timeout(Duration::from_secs(10), updater)
            .await
            .expect("updater did not finish")
            .unwrap();
        assert_eq!(visible, 1);
        let epoch: i64 =
            sqlx::query_scalar(&format!("SELECT epoch FROM {GUARD_SCRATCH} WHERE id = 1"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(epoch, 2);

        drop_guard_scratch(&pool).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_write_guard_failure_leaves_the_connection_clean() {
        use std::time::Duration;

        let Some(base) = setup_test_store().await else {
            return;
        };
        base.pool().close().await;
        drop(base);

        // One connection: a leaked transaction or lock would stall the next write.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&env::var("ARC_POSTGRES_TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        create_guard_scratch(&pool).await;

        // The refusing guard takes a row lock before failing.
        struct LockThenSwitch(Arc<SwitchGuard>);
        #[async_trait]
        impl WriteGuard for LockThenSwitch {
            async fn check(&self, conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
                sqlx::query(&format!(
                    "SELECT epoch FROM {GUARD_SCRATCH} WHERE id = 1 FOR UPDATE"
                ))
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
                self.0.check(conn).await
            }
        }

        let switch = SwitchGuard::new(true);
        let unguarded = PostgresEventStore::with_pool(pool.clone());
        let guarded = unguarded
            .clone()
            .with_write_guard(Arc::new(LockThenSwitch(switch.clone())));

        assert_guard_refused(
            guarded
                .append(
                    "h1",
                    VersionCheck::New,
                    vec![stamped_event("User", "h1", 1, "Created", json!({}))],
                )
                .await,
        );
        assert_guard_refused(guarded.acknowledge_publication(Uuid::new_v4()).await);
        assert_guard_refused(
            guarded
                .save_snapshot(&Snapshot::new("h1", "User", 1, json!({})))
                .await,
        );

        // The single connection is back, idle and outside any transaction.
        let in_tx: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_locks WHERE pid = pg_backend_pid() AND locktype = 'transactionid'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(in_tx, 0);

        unguarded
            .append(
                "h1",
                VersionCheck::New,
                vec![stamped_event("User", "h1", 1, "Created", json!({}))],
            )
            .await
            .unwrap();
        sqlx::query(&format!(
            "UPDATE {GUARD_SCRATCH} SET epoch = epoch + 1 WHERE id = 1"
        ))
        .execute(&pool)
        .await
        .unwrap();

        switch.refuse.store(false, Ordering::SeqCst);
        guarded
            .append(
                "h1",
                VersionCheck::Expected(1),
                vec![stamped_event("User", "h1", 2, "Renamed", json!({}))],
            )
            .await
            .unwrap();
        guarded
            .save_snapshot(&Snapshot::new("h1", "User", 2, json!({})))
            .await
            .unwrap();
        assert_eq!(unguarded.load("h1").await.unwrap().len(), 2);
        assert_eq!(switch.calls.load(Ordering::SeqCst), 5);

        drop_guard_scratch(&pool).await;
        pool.close().await;
    }
}
