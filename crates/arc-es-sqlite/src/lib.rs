//! # Arc ES SQLite
//!
//! SQLite implementation of the [`EventStore`] trait from `arc-core`.
//!
//! Persists [`AuditMetadata`] inline alongside each event (see HIPAA-1 in
//! the historical production-risk audit). `append` calls
//! [`validate_audit_batch`]
//! before any write — defense-in-depth against an upstream that forgot to
//! stamp.

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
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager};
use diesel::sqlite::SqliteConnection;
use std::sync::Arc;
use uuid::Uuid;

// Re-export for convenience
pub use arc_core::{Deserialize, Serialize};

pub mod access_log;
pub use access_log::{AccessLogPrivacy, SqliteAccessLogger};

pub mod session;
pub use session::SqliteSessionStore;

pub mod read_model_store;
pub use read_model_store::SqliteReadModelStore;

#[cfg(test)]
mod test_support;

/// Database row used for inserting events.
#[derive(Debug, Insertable, Clone)]
#[diesel(table_name = events)]
struct NewEventRecord {
    pub event_id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub payload: String,
    pub timestamp: i64,
    pub actor_id: String,
    pub actor_session_id: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub timestamp_utc_us: i64,
    pub causation_id: Option<String>,
    pub correlation_id: String,
    pub integrity_signature: Option<String>,
    pub integrity_key_id: Option<String>,
}

#[derive(Debug, Queryable, Clone)]
struct EventRecord {
    #[allow(dead_code)]
    pub id: Option<i32>,
    pub event_id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub payload: String,
    pub timestamp: i64,
    pub actor_id: String,
    pub actor_session_id: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub timestamp_utc_us: i64,
    pub causation_id: Option<String>,
    pub correlation_id: String,
    pub integrity_signature: Option<String>,
    pub integrity_key_id: Option<String>,
}

impl NewEventRecord {
    fn from_event(
        event: &Event,
        integrity_signature: Option<String>,
        integrity_key_id: Option<String>,
    ) -> Result<Self, EventStoreError> {
        // sequence and timestamp are i64 end-to-end now — no truncation.
        let timestamp_seconds: i64 = (event.timestamp / 1000) as i64;
        Ok(NewEventRecord {
            event_id: event.event_id.to_string(),
            aggregate_type: event.aggregate_type.clone(),
            aggregate_id: event.aggregate_id.clone(),
            sequence: event.sequence,
            event_type: event.event_type.clone(),
            payload: serde_json::to_string(&event.payload)
                .map_err(|e| EventStoreError::serialization(e.to_string()))?,
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
        })
    }
}

impl EventRecord {
    fn to_event(&self) -> Result<Event, EventStoreError> {
        let event_id = Uuid::parse_str(&self.event_id)
            .map_err(|e| EventStoreError::serialization(format!("Invalid UUID: {}", e)))?;

        let payload: serde_json::Value = serde_json::from_str(&self.payload)
            .map_err(|e| EventStoreError::serialization(e.to_string()))?;

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
            payload,
            audit,
            timestamp: (self.timestamp as u64) * 1000,
        })
    }
}

/// Database row used for upserting snapshots. `state` holds the aggregate's
/// serialized JSON; `created_at` is milliseconds since epoch (same unit as the
/// core `Snapshot`), stored as i64 to match the events table's `timestamp`.
#[derive(Debug, Insertable, Clone)]
#[diesel(table_name = snapshots)]
struct NewSnapshotRecord {
    pub aggregate_id: String,
    pub aggregate_type: String,
    pub version: i64,
    pub state: String,
    pub created_at: i64,
}

#[derive(Debug, Queryable, Clone)]
struct SnapshotRecord {
    pub aggregate_id: String,
    pub aggregate_type: String,
    pub version: i64,
    pub state: String,
    pub created_at: i64,
}

impl NewSnapshotRecord {
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, EventStoreError> {
        Ok(NewSnapshotRecord {
            aggregate_id: snapshot.aggregate_id.clone(),
            aggregate_type: snapshot.aggregate_type.clone(),
            version: snapshot.version,
            state: serde_json::to_string(&snapshot.state)
                .map_err(|e| EventStoreError::serialization(e.to_string()))?,
            created_at: snapshot.created_at as i64,
        })
    }
}

impl SnapshotRecord {
    fn to_snapshot(&self) -> Result<Snapshot, EventStoreError> {
        let state: serde_json::Value = serde_json::from_str(&self.state)
            .map_err(|e| EventStoreError::serialization(e.to_string()))?;
        Ok(Snapshot {
            aggregate_id: self.aggregate_id.clone(),
            aggregate_type: self.aggregate_type.clone(),
            version: self.version,
            state,
            created_at: self.created_at as u64,
        })
    }
}

mod schema {
    diesel::table! {
        events (id) {
            id -> Nullable<Integer>,
            event_id -> Text,
            aggregate_type -> Text,
            aggregate_id -> Text,
            sequence -> BigInt,
            event_type -> Text,
            payload -> Text,
            timestamp -> BigInt,
            actor_id -> Text,
            actor_session_id -> Nullable<Text>,
            source_ip -> Nullable<Text>,
            user_agent -> Nullable<Text>,
            timestamp_utc_us -> BigInt,
            causation_id -> Nullable<Text>,
            correlation_id -> Text,
            integrity_signature -> Nullable<Text>,
            integrity_key_id -> Nullable<Text>,
        }
    }

    diesel::table! {
        snapshots (aggregate_type, aggregate_id) {
            aggregate_id -> Text,
            aggregate_type -> Text,
            version -> BigInt,
            state -> Text,
            created_at -> BigInt,
        }
    }
}

use schema::{events, snapshots};

type Pool = r2d2::Pool<ConnectionManager<SqliteConnection>>;

/// SQLite implementation of EventStore.
#[derive(Clone)]
pub struct SqliteEventStore {
    pool: Arc<Pool>,
    integrity: Option<Arc<IntegrityConfig>>,
}

struct IntegrityConfig {
    chain: Arc<dyn IntegrityChain>,
    key_id: String,
}

impl SqliteEventStore {
    pub async fn new(database_url: &str) -> EventStoreResult<Self> {
        let manager = ConnectionManager::<SqliteConnection>::new(database_url);
        let pool = Pool::builder()
            .max_size(10)
            .build(manager)
            .map_err(|e| EventStoreError::database(format!("Failed to create pool: {}", e)))?;

        Ok(SqliteEventStore {
            pool: Arc::new(pool),
            integrity: None,
        })
    }

    pub async fn new_with_integrity_key(
        database_url: &str,
        key: impl Into<Vec<u8>>,
        key_id: impl Into<String>,
    ) -> EventStoreResult<Self> {
        let mut store = Self::new(database_url).await?;
        store.integrity = Some(Arc::new(IntegrityConfig {
            chain: Arc::new(HmacSha256Chain::new(key).map_err(EventStoreError::from)?),
            key_id: key_id.into(),
        }));
        Ok(store)
    }

    pub fn with_pool(pool: Pool) -> Self {
        SqliteEventStore {
            pool: Arc::new(pool),
            integrity: None,
        }
    }

    pub fn with_pool_and_integrity_key(
        pool: Pool,
        key: impl Into<Vec<u8>>,
        key_id: impl Into<String>,
    ) -> EventStoreResult<Self> {
        Ok(SqliteEventStore {
            pool: Arc::new(pool),
            integrity: Some(Arc::new(IntegrityConfig {
                chain: Arc::new(HmacSha256Chain::new(key).map_err(EventStoreError::from)?),
                key_id: key_id.into(),
            })),
        })
    }
}

fn required_signature(
    record: &EventRecord,
    aggregate_id: &str,
    sequence: i64,
) -> EventStoreResult<EventSignature> {
    let _key_id = record.integrity_key_id.as_ref().ok_or_else(|| {
        EventStoreError::from(IntegrityError::BrokenAt {
            aggregate_id: aggregate_id.to_string(),
            sequence,
        })
    })?;

    record
        .integrity_signature
        .as_ref()
        .map(|s| EventSignature(s.clone()))
        .ok_or_else(|| {
            EventStoreError::from(IntegrityError::BrokenAt {
                aggregate_id: aggregate_id.to_string(),
                sequence,
            })
        })
}

fn verify_integrity_records(
    integrity: &IntegrityConfig,
    records: &[EventRecord],
    previous_signature: EventSignature,
) -> EventStoreResult<Vec<Event>> {
    let mut previous = previous_signature;
    let mut events = Vec::with_capacity(records.len());

    for record in records {
        let event = record.to_event()?;
        let expected = integrity.chain.sign_event(&previous, &event)?;
        let claimed = required_signature(record, &event.aggregate_id, event.sequence)?;

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

fn previous_signature_for_aggregate(
    conn: &mut SqliteConnection,
    aggregate_type: Option<&str>,
    aggregate_id: &str,
    before_sequence: i64,
) -> EventStoreResult<EventSignature> {
    if before_sequence <= 1 {
        return Ok(EventSignature::genesis());
    }

    let mut query = events::table
        .filter(events::aggregate_id.eq(aggregate_id))
        .filter(events::sequence.lt(before_sequence))
        .into_boxed();
    if let Some(aggregate_type) = aggregate_type {
        query = query.filter(events::aggregate_type.eq(aggregate_type));
    }
    let record = query
        .order(events::sequence.desc())
        .first::<EventRecord>(conn)
        .optional()
        .map_err(|e| EventStoreError::database(e.to_string()))?;

    match record {
        Some(record) => required_signature(&record, aggregate_id, record.sequence),
        None => Ok(EventSignature::genesis()),
    }
}

fn verify_stream_integrity_records(
    conn: &mut SqliteConnection,
    integrity: &IntegrityConfig,
    records: &[EventRecord],
) -> EventStoreResult<Vec<Event>> {
    use std::collections::HashMap;

    let mut previous_by_aggregate: HashMap<(String, String), EventSignature> = HashMap::new();
    let mut events = Vec::with_capacity(records.len());

    for record in records {
        let event = record.to_event()?;
        let stream = (event.aggregate_type.clone(), event.aggregate_id.clone());
        let previous = match previous_by_aggregate.get(&stream) {
            Some(sig) => sig.clone(),
            None => previous_signature_for_aggregate(
                conn,
                Some(&event.aggregate_type),
                &event.aggregate_id,
                event.sequence,
            )?,
        };

        let expected = integrity.chain.sign_event(&previous, &event)?;
        let claimed = required_signature(record, &event.aggregate_id, event.sequence)?;

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

#[async_trait]
impl EventStore for SqliteEventStore {
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

    async fn append_to(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        version_check: VersionCheck,
        new_events: Vec<Event>,
    ) -> EventStoreResult<()> {
        if new_events.is_empty() {
            return Ok(());
        }

        // Defense-in-depth: reject any event with invalid audit before touching the DB.
        validate_audit_batch(aggregate_id, &new_events)?;

        let aggregate_type = aggregate_type.to_string();
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();
        let integrity = self.integrity.clone();

        tokio::task::spawn_blocking(move || -> EventStoreResult<()> {
            use diesel::connection::AnsiTransactionManager;
            use diesel::connection::TransactionManager;

            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            AnsiTransactionManager::begin_transaction(&mut *conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            let result = (|| -> EventStoreResult<()> {
                let current_version = events::table
                    .filter(events::aggregate_type.eq(&aggregate_type))
                    .filter(events::aggregate_id.eq(&aggregate_id))
                    .select(diesel::dsl::max(events::sequence))
                    .first::<Option<i64>>(&mut *conn)
                    .map_err(|e| EventStoreError::database(e.to_string()))?
                    .unwrap_or(0);

                if let Some(expected) = version_check.version() {
                    if current_version != expected {
                        return Err(EventStoreError::ConcurrencyConflict {
                            aggregate_id: aggregate_id.clone(),
                            expected,
                            actual: current_version,
                        });
                    }
                }

                for (expected_sequence, event) in (current_version + 1..).zip(new_events.iter()) {
                    if event.sequence != expected_sequence {
                        return Err(EventStoreError::InvalidSequence {
                            aggregate_id: aggregate_id.clone(),
                            expected: expected_sequence,
                            actual: event.sequence,
                        });
                    }
                }

                let mut previous_signature = if integrity.is_some() {
                    previous_signature_for_aggregate(
                        &mut conn,
                        Some(&aggregate_type),
                        &aggregate_id,
                        current_version + 1,
                    )?
                } else {
                    EventSignature::genesis()
                };

                for event in &new_events {
                    let mut record = NewEventRecord::from_event(event, None, None)?;

                    if let Some(integrity) = integrity.as_ref() {
                        let mut persisted_event = event.clone();
                        persisted_event.timestamp = (record.timestamp as u64) * 1000;
                        let signature = integrity
                            .chain
                            .sign_event(&previous_signature, &persisted_event)
                            .map_err(EventStoreError::from)?;
                        previous_signature = signature.clone();
                        record.integrity_signature = Some(signature.0);
                        record.integrity_key_id = Some(integrity.key_id.clone());
                    }

                    diesel::insert_into(events::table)
                        .values(&record)
                        .execute(&mut *conn)
                        .map_err(|e| EventStoreError::database(e.to_string()))?;
                }

                Ok(())
            })();

            match result {
                Ok(_) => {
                    AnsiTransactionManager::commit_transaction(&mut *conn)
                        .map_err(|e| EventStoreError::database(e.to_string()))?;
                    Ok(())
                }
                Err(e) => {
                    let _ = AnsiTransactionManager::rollback_transaction(&mut *conn);
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
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

    async fn load_from(
        &self,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>> {
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();
        let integrity = self.integrity.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let records: Vec<EventRecord> = events::table
                .filter(events::aggregate_id.eq(&aggregate_id))
                .filter(events::sequence.ge(from_sequence))
                .order(events::sequence.asc())
                .load(&mut conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            match integrity.as_ref() {
                Some(integrity) => {
                    let previous = previous_signature_for_aggregate(
                        &mut conn,
                        None,
                        &aggregate_id,
                        from_sequence,
                    )?;
                    verify_integrity_records(integrity, &records, previous)
                }
                None => records.iter().map(|r| r.to_event()).collect(),
            }
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn load_stream_from(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>> {
        let aggregate_type = aggregate_type.to_string();
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();
        let integrity = self.integrity.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let records: Vec<EventRecord> = events::table
                .filter(events::aggregate_type.eq(&aggregate_type))
                .filter(events::aggregate_id.eq(&aggregate_id))
                .filter(events::sequence.ge(from_sequence))
                .order(events::sequence.asc())
                .load(&mut conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            match integrity.as_ref() {
                Some(integrity) => {
                    let previous = previous_signature_for_aggregate(
                        &mut conn,
                        Some(&aggregate_type),
                        &aggregate_id,
                        from_sequence,
                    )?;
                    verify_integrity_records(integrity, &records, previous)
                }
                None => records.iter().map(|record| record.to_event()).collect(),
            }
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn stream_all(&self, from_position: i64) -> EventStoreResult<Vec<Event>> {
        let pool = self.pool.clone();
        let integrity = self.integrity.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let records: Vec<EventRecord> = events::table
                .filter(events::id.ge(from_position as i32))
                .order(events::id.asc())
                .load(&mut conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            match integrity.as_ref() {
                Some(integrity) => verify_stream_integrity_records(&mut conn, integrity, &records),
                None => records.iter().map(|r| r.to_event()).collect(),
            }
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn get_version(&self, aggregate_id: &str) -> EventStoreResult<i64> {
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let version = events::table
                .filter(events::aggregate_id.eq(&aggregate_id))
                .select(diesel::dsl::max(events::sequence))
                .first::<Option<i64>>(&mut conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?
                .unwrap_or(0);

            Ok(version)
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn get_stream_version(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<i64> {
        let aggregate_type = aggregate_type.to_string();
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;
            events::table
                .filter(events::aggregate_type.eq(&aggregate_type))
                .filter(events::aggregate_id.eq(&aggregate_id))
                .select(diesel::dsl::max(events::sequence))
                .first::<Option<i64>>(&mut conn)
                .map(|version| version.unwrap_or(0))
                .map_err(|e| EventStoreError::database(e.to_string()))
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn save_snapshot(&self, snapshot: &Snapshot) -> EventStoreResult<()> {
        let record = NewSnapshotRecord::from_snapshot(snapshot)?;
        let pool = self.pool.clone();

        tokio::task::spawn_blocking(move || -> EventStoreResult<()> {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            // One snapshot per aggregate: replace the stored row in place rather
            // than accumulating stale versions.
            diesel::insert_into(snapshots::table)
                .values(&record)
                .on_conflict((snapshots::aggregate_type, snapshots::aggregate_id))
                .do_update()
                .set((
                    snapshots::version.eq(record.version),
                    snapshots::state.eq(&record.state),
                    snapshots::created_at.eq(record.created_at),
                ))
                .execute(&mut *conn)
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            Ok(())
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn load_snapshot(&self, aggregate_id: &str) -> EventStoreResult<Option<Snapshot>> {
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let record: Option<SnapshotRecord> = snapshots::table
                .filter(snapshots::aggregate_id.eq(&aggregate_id))
                .first::<SnapshotRecord>(&mut conn)
                .optional()
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            record.map(|r| r.to_snapshot()).transpose()
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }

    async fn load_snapshot_for(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<Option<Snapshot>> {
        let aggregate_type = aggregate_type.to_string();
        let aggregate_id = aggregate_id.to_string();
        let pool = self.pool.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                EventStoreError::database(format!("Failed to get connection: {}", e))
            })?;

            let record: Option<SnapshotRecord> = snapshots::table
                .filter(snapshots::aggregate_type.eq(&aggregate_type))
                .filter(snapshots::aggregate_id.eq(&aggregate_id))
                .first::<SnapshotRecord>(&mut conn)
                .optional()
                .map_err(|e| EventStoreError::database(e.to_string()))?;

            record.map(|record| record.to_snapshot()).transpose()
        })
        .await
        .map_err(|e| EventStoreError::other(format!("Task join error: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_core::audit::AuditMetadata;
    use serde_json::json;

    async fn setup_test_store() -> SqliteEventStore {
        let manager = ConnectionManager::<SqliteConnection>::new(":memory:");
        let pool = Pool::builder()
            .max_size(1)
            .build(manager)
            .expect("Failed to create pool");

        let mut conn = pool.get().expect("Failed to get connection");
        crate::test_support::migrate(&mut conn);
        drop(conn);

        SqliteEventStore::with_pool(pool)
    }

    async fn setup_integrity_test_store() -> SqliteEventStore {
        let manager = ConnectionManager::<SqliteConnection>::new(":memory:");
        let pool = Pool::builder()
            .max_size(1)
            .build(manager)
            .expect("Failed to create pool");

        let mut conn = pool.get().expect("Failed to get connection");
        crate::test_support::migrate(&mut conn);
        drop(conn);

        SqliteEventStore::with_pool_and_integrity_key(pool, integrity_key(), "test-key")
            .expect("integrity store")
    }

    fn integrity_key() -> Vec<u8> {
        b"012345678901234567890123456789AB".to_vec()
    }

    /// Helper: build an event with stamped audit.
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

    #[derive(QueryableByName, Debug)]
    struct SignatureRow {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        integrity_signature: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        integrity_key_id: Option<String>,
    }

    #[tokio::test]
    async fn test_append_and_load_single_event() {
        let store = setup_test_store().await;
        let event = stamped_event(
            "User",
            "user-123",
            1,
            "UserCreated",
            json!({ "name": "Alice" }),
        );

        store
            .append("user-123", VersionCheck::New, vec![event.clone()])
            .await
            .unwrap();
        let loaded = store.load("user-123").await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].aggregate_id, "user-123");
        assert_eq!(loaded[0].event_type, "UserCreated");
        assert_eq!(loaded[0].sequence, 1);
        assert_eq!(loaded[0].audit.actor_id, "test");
    }

    #[tokio::test]
    async fn same_instance_id_is_isolated_by_aggregate_type() {
        let store = setup_test_store().await;
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
    async fn test_append_multiple_events() {
        let store = setup_test_store().await;
        let events = vec![
            stamped_event("User", "user-456", 1, "UserCreated", json!({})),
            stamped_event("User", "user-456", 2, "ProfileUpdated", json!({})),
            stamped_event("User", "user-456", 3, "EmailChanged", json!({})),
        ];

        store
            .append("user-456", VersionCheck::New, events)
            .await
            .unwrap();
        let loaded = store.load("user-456").await.unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].sequence, 1);
        assert_eq!(loaded[2].sequence, 3);
    }

    #[tokio::test]
    async fn test_integrity_append_persists_signatures() {
        let store = setup_integrity_test_store().await;
        store
            .append(
                "signed-1",
                VersionCheck::New,
                vec![
                    stamped_event("User", "signed-1", 1, "UserCreated", json!({})),
                    stamped_event("User", "signed-1", 2, "ProfileUpdated", json!({})),
                ],
            )
            .await
            .unwrap();

        let pool = store.pool.clone();
        let rows = tokio::task::spawn_blocking(move || -> EventStoreResult<Vec<SignatureRow>> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(
                "SELECT integrity_signature, integrity_key_id
                 FROM events WHERE aggregate_id = 'signed-1' ORDER BY sequence",
            )
            .load(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.integrity_signature.as_deref().map(str::len), Some(64));
            assert_eq!(row.integrity_key_id.as_deref(), Some("test-key"));
        }
    }

    #[tokio::test]
    async fn test_integrity_load_rejects_tampered_payload() {
        let store = setup_integrity_test_store().await;
        store
            .append(
                "tamper-1",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "tamper-1",
                    1,
                    "UserCreated",
                    json!({"ok": true}),
                )],
            )
            .await
            .unwrap();

        let pool = store.pool.clone();
        tokio::task::spawn_blocking(move || -> EventStoreResult<()> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(
                "UPDATE events SET payload = '{\"ok\": false}' WHERE aggregate_id = 'tamper-1'",
            )
            .execute(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();

        let err = store.load("tamper-1").await.unwrap_err();
        assert!(
            matches!(err, EventStoreError::Integrity { .. }),
            "expected integrity error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_integrity_load_rejects_missing_signature() {
        let store = setup_integrity_test_store().await;
        store
            .append(
                "missing-sig",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "missing-sig",
                    1,
                    "UserCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();

        let pool = store.pool.clone();
        tokio::task::spawn_blocking(move || -> EventStoreResult<()> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(
                "UPDATE events SET integrity_signature = NULL WHERE aggregate_id = 'missing-sig'",
            )
            .execute(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();

        let err = store.load("missing-sig").await.unwrap_err();
        assert!(
            matches!(err, EventStoreError::Integrity { .. }),
            "expected integrity error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_integrity_load_from_uses_previous_signature() {
        let store = setup_integrity_test_store().await;
        store
            .append(
                "load-from-signed",
                VersionCheck::New,
                vec![
                    stamped_event("User", "load-from-signed", 1, "UserCreated", json!({})),
                    stamped_event("User", "load-from-signed", 2, "ProfileUpdated", json!({})),
                ],
            )
            .await
            .unwrap();

        let loaded = store.load_from("load-from-signed", 2).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sequence, 2);
    }

    #[tokio::test]
    async fn test_integrity_stream_all_verifies_per_aggregate() {
        let store = setup_integrity_test_store().await;
        store
            .append(
                "signed-a",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "signed-a",
                    1,
                    "UserCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();
        store
            .append(
                "signed-b",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "signed-b",
                    1,
                    "UserCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();
        store
            .append(
                "signed-a",
                VersionCheck::Expected(1),
                vec![stamped_event(
                    "User",
                    "signed-a",
                    2,
                    "ProfileUpdated",
                    json!({}),
                )],
            )
            .await
            .unwrap();

        let loaded = store.stream_all(0).await.unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[tokio::test]
    async fn test_optimistic_concurrency_control() {
        let store = setup_test_store().await;
        store
            .append(
                "user-789",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "user-789",
                    1,
                    "UserCreated",
                    json!({}),
                )],
            )
            .await
            .unwrap();
        store
            .append(
                "user-789",
                VersionCheck::Expected(1),
                vec![stamped_event(
                    "User",
                    "user-789",
                    2,
                    "ProfileUpdated",
                    json!({}),
                )],
            )
            .await
            .unwrap();
        let result = store
            .append(
                "user-789",
                VersionCheck::Expected(1),
                vec![stamped_event(
                    "User",
                    "user-789",
                    3,
                    "EmailChanged",
                    json!({}),
                )],
            )
            .await;
        assert!(matches!(
            result,
            Err(EventStoreError::ConcurrencyConflict {
                expected: 1,
                actual: 2,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_invalid_sequence() {
        let store = setup_test_store().await;
        let result = store
            .append(
                "user-999",
                VersionCheck::New,
                vec![stamped_event(
                    "User",
                    "user-999",
                    5,
                    "UserCreated",
                    json!({}),
                )],
            )
            .await;
        assert!(matches!(
            result,
            Err(EventStoreError::InvalidSequence {
                expected: 1,
                actual: 5,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_load_from_sequence() {
        let store = setup_test_store().await;
        let events = vec![
            stamped_event("Order", "order-1", 1, "OrderCreated", json!({})),
            stamped_event("Order", "order-1", 2, "ItemAdded", json!({})),
            stamped_event("Order", "order-1", 3, "ItemAdded", json!({})),
            stamped_event("Order", "order-1", 4, "OrderShipped", json!({})),
        ];
        store
            .append("order-1", VersionCheck::New, events)
            .await
            .unwrap();
        let loaded = store.load_from("order-1", 3).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].sequence, 3);
    }

    #[tokio::test]
    async fn test_get_version() {
        let store = setup_test_store().await;
        assert_eq!(store.get_version("nope").await.unwrap(), 0);
        let events = vec![
            stamped_event("User", "u1", 1, "UserCreated", json!({})),
            stamped_event("User", "u1", 2, "ProfileUpdated", json!({})),
            stamped_event("User", "u1", 3, "EmailChanged", json!({})),
        ];
        store.append("u1", VersionCheck::New, events).await.unwrap();
        assert_eq!(store.get_version("u1").await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_stream_all() {
        let store = setup_test_store().await;
        store
            .append(
                "user-1",
                VersionCheck::New,
                vec![
                    stamped_event("User", "user-1", 1, "UserCreated", json!({})),
                    stamped_event("User", "user-1", 2, "ProfileUpdated", json!({})),
                ],
            )
            .await
            .unwrap();
        store
            .append(
                "order-1",
                VersionCheck::New,
                vec![
                    stamped_event("Order", "order-1", 1, "OrderCreated", json!({})),
                    stamped_event("Order", "order-1", 2, "OrderShipped", json!({})),
                ],
            )
            .await
            .unwrap();
        assert_eq!(store.stream_all(0).await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn test_empty_aggregate() {
        let store = setup_test_store().await;
        assert_eq!(store.load("nothing").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_audit_roundtrip_preserves_all_fields() {
        let store = setup_test_store().await;
        let mut audit = AuditMetadata::test_default();
        audit.actor_id = "user-uuid-42".to_string();
        audit.actor_session_id = Some("sess-XYZ".to_string());
        audit.source_ip = Some("10.0.0.42".to_string());
        audit.user_agent = Some("Mozilla/5.0 (test)".to_string());
        audit.causation_id = Some(Uuid::new_v4());
        let expected_corr = audit.correlation_id;
        let expected_caus = audit.causation_id;

        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u-audit",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        })
        .with_audit(audit.clone());

        store
            .append("u-audit", VersionCheck::New, vec![event])
            .await
            .unwrap();
        let loaded = store.load("u-audit").await.unwrap();

        assert_eq!(loaded[0].audit.actor_id, "user-uuid-42");
        assert_eq!(
            loaded[0].audit.actor_session_id.as_deref(),
            Some("sess-XYZ")
        );
        assert_eq!(loaded[0].audit.source_ip.as_deref(), Some("10.0.0.42"));
        assert_eq!(
            loaded[0].audit.user_agent.as_deref(),
            Some("Mozilla/5.0 (test)")
        );
        assert_eq!(loaded[0].audit.correlation_id, expected_corr);
        assert_eq!(loaded[0].audit.causation_id, expected_caus);
        assert!(loaded[0].audit.timestamp_utc_us > 0);
    }

    #[tokio::test]
    async fn test_append_rejects_pending_audit() {
        let store = setup_test_store().await;
        // Built without with_audit — audit stays pending.
        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u-bad",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        });
        let err = store
            .append("u-bad", VersionCheck::New, vec![event])
            .await
            .unwrap_err();
        assert!(matches!(err, EventStoreError::InvalidAudit { .. }));

        // No row should have been written.
        assert_eq!(store.load("u-bad").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_actor_id_index_used() {
        let store = setup_test_store().await;
        let mut a = AuditMetadata::test_default();
        a.actor_id = "alice-uuid".to_string();
        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u1",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        })
        .with_audit(a);
        store
            .append("u1", VersionCheck::New, vec![event])
            .await
            .unwrap();

        // EXPLAIN QUERY PLAN must show an index search on actor_id
        let pool = store.pool.clone();
        let plan = tokio::task::spawn_blocking(move || -> EventStoreResult<Vec<String>> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            let plan: Vec<(i32, i32, i32, String)> = diesel::sql_query(
                "EXPLAIN QUERY PLAN SELECT * FROM events WHERE actor_id = 'alice-uuid'",
            )
            .load::<ExplainRow>(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))?
            .into_iter()
            .map(|r| (r.id, r.parent, r.notused, r.detail))
            .collect();
            Ok(plan.into_iter().map(|(_, _, _, d)| d).collect())
        })
        .await
        .unwrap()
        .unwrap();

        assert!(
            plan.iter().any(|d| d.contains("idx_events_actor_id")),
            "actor_id query did not use index; plan: {:?}",
            plan
        );
    }

    #[derive(QueryableByName, Debug)]
    struct ExplainRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        parent: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        notused: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        detail: String,
    }

    #[tokio::test]
    async fn test_concurrent_appends() {
        let store = setup_test_store().await;
        store
            .append(
                "uc",
                VersionCheck::New,
                vec![stamped_event("User", "uc", 1, "UserCreated", json!({}))],
            )
            .await
            .unwrap();

        let s1 = store.clone();
        let s2 = store.clone();
        let h1 = tokio::spawn(async move {
            s1.append(
                "uc",
                VersionCheck::Expected(1),
                vec![stamped_event("User", "uc", 2, "U1", json!({}))],
            )
            .await
        });
        let h2 = tokio::spawn(async move {
            s2.append(
                "uc",
                VersionCheck::Expected(1),
                vec![stamped_event("User", "uc", 2, "U2", json!({}))],
            )
            .await
        });
        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();
        assert!(r1.is_ok() != r2.is_ok());
    }

    #[tokio::test]
    async fn test_sequence_above_i32_max_roundtrips_without_truncation() {
        // Pre-fix bug: sequence was cast to i32 on insert and read back as i32.
        // A value above i32::MAX (2_147_483_647) would silently overflow.
        // After widening, this round-trips intact.
        let store = setup_test_store().await;
        let pool = store.pool.clone();

        let huge_seq: i64 = (i32::MAX as i64) + 1234;
        let huge_ts: i64 = 9_999_999_999; // year 2286 — would never fit in i32

        let inserted = tokio::task::spawn_blocking(move || -> EventStoreResult<usize> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(format!(
                "INSERT INTO events (event_id, aggregate_type, aggregate_id, sequence,
                    event_type, payload, timestamp,
                    actor_id, timestamp_utc_us, correlation_id)
                 VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'User', 'u-big', {seq},
                    'Event', '{{}}', {ts},
                    'tester', {seq}, '00000000-0000-0000-0000-000000000001')",
                seq = huge_seq,
                ts = huge_ts,
            ))
            .execute(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(inserted, 1);

        let loaded = store.load("u-big").await.expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sequence, huge_seq, "sequence must not truncate");
        // timestamp stored as seconds; round-trip back to milliseconds in Event::timestamp
        assert_eq!(loaded[0].timestamp, (huge_ts as u64) * 1000);

        let v = store.get_version("u-big").await.expect("version");
        assert_eq!(v, huge_seq, "get_version must not truncate either");
    }

    #[tokio::test]
    async fn test_legacy_backfilled_row_roundtrips() {
        // Simulates a row written before HIPAA-1, then backfilled by the migration:
        // actor_id='legacy-pre-hipaa', timestamp_utc_us derived from seconds*1_000_000,
        // correlation_id = nil UUID. The row must load without panicking.
        let store = setup_test_store().await;
        let pool = store.pool.clone();

        let inserted_count = tokio::task::spawn_blocking(move || -> EventStoreResult<usize> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(
                "INSERT INTO events (event_id, aggregate_type, aggregate_id, sequence,
                    event_type, payload, timestamp,
                    actor_id, timestamp_utc_us, correlation_id)
                 VALUES ('11111111-1111-1111-1111-111111111111', 'User', 'u-legacy', 1,
                    'UserCreated', '{}', 1700000000,
                    'legacy-pre-hipaa', 1700000000000000,
                    '00000000-0000-0000-0000-000000000000')",
            )
            .execute(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(inserted_count, 1);

        let loaded = store.load("u-legacy").await.expect("legacy row must load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].audit.actor_id, "legacy-pre-hipaa");
        assert_eq!(loaded[0].audit.timestamp_utc_us, 1_700_000_000_000_000);
        assert_eq!(loaded[0].audit.correlation_id, Uuid::nil());
        assert!(loaded[0].audit.causation_id.is_none());
    }

    #[tokio::test]
    async fn test_load_rejects_malformed_correlation_uuid() {
        let store = setup_test_store().await;
        let pool = store.pool.clone();

        tokio::task::spawn_blocking(move || -> EventStoreResult<()> {
            let mut conn = pool
                .get()
                .map_err(|e| EventStoreError::database(e.to_string()))?;
            diesel::sql_query(
                "INSERT INTO events (event_id, aggregate_type, aggregate_id, sequence,
                    event_type, payload, timestamp,
                    actor_id, timestamp_utc_us, correlation_id)
                 VALUES ('22222222-2222-2222-2222-222222222222', 'User', 'u-bad', 1,
                    'X', '{}', 1700000000,
                    'tester', 1700000000000000,
                    'not-a-uuid')",
            )
            .execute(&mut *conn)
            .map_err(|e| EventStoreError::database(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();

        let err = store.load("u-bad").await.unwrap_err();
        assert!(
            matches!(err, EventStoreError::SerializationError { ref message } if message.contains("Invalid correlation UUID")),
            "expected SerializationError on malformed correlation_id, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_caused_by_chain_roundtrips_through_sqlite() {
        use arc_core::aggregate::{Aggregate, Command};
        use arc_core::command_bus::{CommandBus, CommandContext};
        use arc_core::event::Event as CoreEvent;
        use arc_core::event_bus::InProcessEventBus;

        // Trivial aggregate so we can exercise CommandBus + SqliteEventStore together.
        #[derive(Default)]
        struct Counter {
            v: i64,
        }
        struct Cmd {
            id: String,
        }
        impl Command for Cmd {
            fn aggregate_id(&self) -> &str {
                &self.id
            }
        }
        #[derive(Debug, thiserror::Error)]
        #[error("never")]
        struct Never;
        #[async_trait]
        impl Aggregate for Counter {
            type Command = Cmd;
            type Event = ();
            type Error = Never;
            fn aggregate_type() -> &'static str {
                "Counter"
            }
            fn version(&self) -> i64 {
                self.v
            }
            async fn handle(&self, c: Self::Command) -> Result<Vec<CoreEvent>, Self::Error> {
                Ok(vec![CoreEvent::new(NewEvent {
                    aggregate_type: "Counter",
                    aggregate_id: &c.id,
                    sequence: self.v + 1,
                    event_type: "Incremented",
                    payload: serde_json::json!({}),
                })])
            }
            fn apply(&mut self, e: &CoreEvent) {
                self.v = e.sequence;
            }
        }

        let store = setup_test_store().await;
        let bus =
            CommandBus::<Counter>::new(Box::new(store.clone()), Box::new(InProcessEventBus::new()));

        let first_ctx = CommandContext::for_actor("alice");
        let trigger_corr = first_ctx.correlation_id;
        let triggers = bus
            .dispatch(Cmd { id: "c1".into() }, first_ctx)
            .await
            .unwrap();

        let follow_ctx = CommandContext::caused_by("worker", &triggers[0]);
        let follow_corr = follow_ctx.correlation_id;
        let _ = bus
            .dispatch(Cmd { id: "c2".into() }, follow_ctx)
            .await
            .unwrap();

        // Reload from SQLite — chain must survive the trip
        let loaded_c2 = store.load("c2").await.unwrap();
        assert_eq!(loaded_c2.len(), 1);
        assert_eq!(loaded_c2[0].audit.correlation_id, trigger_corr);
        assert_eq!(loaded_c2[0].audit.correlation_id, follow_corr);
        assert_eq!(loaded_c2[0].audit.causation_id, Some(triggers[0].event_id));
    }

    #[tokio::test]
    async fn test_event_ordering_within_aggregate() {
        let store = setup_test_store().await;
        store
            .append(
                "uo",
                VersionCheck::New,
                vec![
                    stamped_event("User", "uo", 1, "UserCreated", json!({})),
                    stamped_event("User", "uo", 2, "EmailChanged", json!({})),
                ],
            )
            .await
            .unwrap();
        store
            .append(
                "uo",
                VersionCheck::Expected(2),
                vec![
                    stamped_event("User", "uo", 3, "ProfileUpdated", json!({})),
                    stamped_event("User", "uo", 4, "PasswordChanged", json!({})),
                ],
            )
            .await
            .unwrap();
        let loaded = store.load("uo").await.unwrap();
        for (i, e) in loaded.iter().enumerate() {
            assert_eq!(e.sequence, (i + 1) as i64);
        }
    }

    #[tokio::test]
    async fn test_save_then_load_snapshot() {
        let store = setup_test_store().await;
        let snap = Snapshot::new("agg-1", "User", 5, json!({ "name": "Alice" }));
        store.save_snapshot(&snap).await.unwrap();
        let loaded = store.load_snapshot("agg-1").await.unwrap();
        assert_eq!(loaded, Some(snap));
    }

    #[tokio::test]
    async fn test_load_snapshot_unknown_returns_none() {
        let store = setup_test_store().await;
        assert_eq!(store.load_snapshot("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_save_snapshot_upserts_newer_version() {
        let store = setup_test_store().await;
        store
            .save_snapshot(&Snapshot::new("agg-2", "User", 3, json!({ "v": 3 })))
            .await
            .unwrap();
        let newer = Snapshot::new("agg-2", "User", 9, json!({ "v": 9 }));
        store.save_snapshot(&newer).await.unwrap();
        let loaded = store.load_snapshot("agg-2").await.unwrap().unwrap();
        assert_eq!(loaded.version, 9);
        assert_eq!(loaded.state["v"], 9);
    }
}
