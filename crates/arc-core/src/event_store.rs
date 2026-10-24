//! # Event Store Module
//!
//! Defines the [`EventStore`] trait for persisting and retrieving events.
//!
//! ## Design Principles
//!
//! - **Append-only**: events can only be added, never modified or deleted
//! - **Optimistic concurrency**: version-based conflict detection
//! - **Stream-based**: events can be loaded by aggregate or streamed globally
//! - **Audited**: every event must carry valid [`AuditMetadata`](crate::audit::AuditMetadata)
//!   when appended (HIPAA §164.312(b))
//! - **Pluggable**: multiple implementations (SQLite, Postgres, in-memory)
//!
//! ## HIPAA defense-in-depth
//!
//! `EventStore::append` MUST call `event.audit.validate()?` for each event
//! before persisting. The `CommandBus` validates first, but the store is the
//! durable boundary — it must not trust upstream.

use crate::audit::AuditError;
use crate::event::Event;
#[cfg(test)]
use crate::event::NewEvent;
use crate::integrity::IntegrityError;
use crate::snapshot::Snapshot;
use async_trait::async_trait;
use thiserror::Error;

/// Version check strategy for optimistic concurrency control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCheck {
    /// First event for this aggregate (expected version is 0)
    New,
    /// Require aggregate to be at this exact version
    Expected(i64),
    /// Automatically load and use current version (use sparingly)
    Auto,
}

impl VersionCheck {
    pub fn version(&self) -> Option<i64> {
        match self {
            VersionCheck::New => Some(0),
            VersionCheck::Expected(v) => Some(*v),
            VersionCheck::Auto => None,
        }
    }
}

/// Errors that can occur during event store operations.
#[derive(Debug, Error)]
pub enum EventStoreError {
    /// Optimistic concurrency conflict.
    #[error("Concurrency conflict: expected version {expected}, but aggregate is at version {actual} (aggregate_id: {aggregate_id})")]
    ConcurrencyConflict {
        aggregate_id: String,
        expected: i64,
        actual: i64,
    },

    #[error("Aggregate not found: {aggregate_id}")]
    AggregateNotFound { aggregate_id: String },

    #[error(
        "Invalid event sequence: expected {expected}, got {actual} (aggregate_id: {aggregate_id})"
    )]
    InvalidSequence {
        aggregate_id: String,
        expected: i64,
        actual: i64,
    },

    /// One or more events in an `append` batch had invalid audit metadata.
    /// Defense-in-depth: the bus should have caught this first.
    #[error(
        "Audit metadata validation failed for event {event_index} (aggregate_id: {aggregate_id}): {source}"
    )]
    InvalidAudit {
        aggregate_id: String,
        event_index: usize,
        #[source]
        source: AuditError,
    },

    /// Stored event signatures are missing or do not match the recomputed
    /// integrity chain.
    #[error("Integrity validation failed: {source}")]
    Integrity {
        #[from]
        source: IntegrityError,
    },

    #[error("Database error: {message}")]
    DatabaseError { message: String },

    #[error("Serialization error: {message}")]
    SerializationError { message: String },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// The store does not implement an optional capability such as snapshot
    /// persistence or exact-event lookup.
    #[error("operation is not supported by this store")]
    Unsupported,

    #[error("Event store error: {message}")]
    Other { message: String },
}

impl EventStoreError {
    pub fn database(message: impl Into<String>) -> Self {
        EventStoreError::DatabaseError {
            message: message.into(),
        }
    }

    pub fn serialization(message: impl Into<String>) -> Self {
        EventStoreError::SerializationError {
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        EventStoreError::Other {
            message: message.into(),
        }
    }

    pub fn invalid_audit(
        aggregate_id: impl Into<String>,
        event_index: usize,
        source: AuditError,
    ) -> Self {
        EventStoreError::InvalidAudit {
            aggregate_id: aggregate_id.into(),
            event_index,
            source,
        }
    }
}

/// Result type for event store operations.
pub type EventStoreResult<T> = Result<T, EventStoreError>;

/// Helper for store implementations: validate every event's audit before persisting.
/// Returns `Err(EventStoreError::InvalidAudit)` on the first failure.
pub fn validate_audit_batch(aggregate_id: &str, events: &[Event]) -> EventStoreResult<()> {
    for (idx, ev) in events.iter().enumerate() {
        ev.audit
            .validate()
            .map_err(|e| EventStoreError::invalid_audit(aggregate_id, idx, e))?;
    }
    Ok(())
}

/// Trait for event store implementations.
///
/// `append` MUST invoke `validate_audit_batch` before persisting (HIPAA defense
/// in depth). The `CommandBus` also validates upstream — both layers run.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append events to the store for a specific aggregate.
    ///
    /// Implementations must call `validate_audit_batch(aggregate_id, &events)?`
    /// before any persistence work.
    async fn append(
        &self,
        aggregate_id: &str,
        version_check: VersionCheck,
        events: Vec<Event>,
    ) -> EventStoreResult<()>;

    /// Append to the stream identified by aggregate type and instance ID.
    ///
    /// The default preserves compatibility with stores that historically keyed
    /// streams only by instance ID. Multi-aggregate stores override this to
    /// enforce `(aggregate_type, aggregate_id, sequence)` identity.
    async fn append_to(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        version_check: VersionCheck,
        events: Vec<Event>,
    ) -> EventStoreResult<()> {
        let _ = aggregate_type;
        self.append(aggregate_id, version_check, events).await
    }

    async fn load(&self, aggregate_id: &str) -> EventStoreResult<Vec<Event>>;

    async fn load_stream(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<Vec<Event>> {
        let _ = aggregate_type;
        self.load(aggregate_id).await
    }

    /// Load one persisted event by its complete stream identity.
    ///
    /// The default returns [`EventStoreError::Unsupported`] so implementations
    /// that do not provide bounded exact-event lookup fail closed instead of
    /// falling back to an unbounded stream read.
    async fn load_event(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        sequence: i64,
    ) -> EventStoreResult<Option<Event>> {
        let _ = (aggregate_type, aggregate_id, sequence);
        Err(EventStoreError::Unsupported)
    }

    async fn load_from(
        &self,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>>;

    async fn load_stream_from(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> EventStoreResult<Vec<Event>> {
        let _ = aggregate_type;
        self.load_from(aggregate_id, from_sequence).await
    }

    async fn stream_all(&self, from_position: i64) -> EventStoreResult<Vec<Event>>;

    async fn get_version(&self, aggregate_id: &str) -> EventStoreResult<i64>;

    async fn get_stream_version(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<i64> {
        let _ = aggregate_type;
        self.get_version(aggregate_id).await
    }

    /// Persist an aggregate snapshot (upsert by `aggregate_id`).
    ///
    /// Default returns `EventStoreError::Unsupported` so stores that have not
    /// implemented snapshotting compile unchanged and fail loudly if a caller
    /// tries to save one.
    async fn save_snapshot(&self, snapshot: &Snapshot) -> EventStoreResult<()> {
        let _ = snapshot;
        Err(EventStoreError::Unsupported)
    }

    /// Load the latest snapshot for an aggregate, if one exists.
    ///
    /// Default returns `Ok(None)` — safe because the caller then replays the
    /// stream from sequence 0, which is always correct, just slower.
    async fn load_snapshot(&self, aggregate_id: &str) -> EventStoreResult<Option<Snapshot>> {
        let _ = aggregate_id;
        Ok(None)
    }

    async fn load_snapshot_for(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> EventStoreResult<Option<Snapshot>> {
        let _ = aggregate_type;
        self.load_snapshot(aggregate_id).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory implementation, public for downstream test code.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "test-utils"))]
mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// In-memory event store. Available to downstream crates via the
    /// `test-utils` feature flag.
    ///
    /// Validates audit metadata on every append (same contract as production
    /// stores) so behavior matches what real implementations enforce.
    #[derive(Clone, Default)]
    pub struct InMemoryEventStore {
        events: Arc<TokioMutex<Vec<Event>>>,
        snapshots: Arc<TokioMutex<HashMap<(String, String), Snapshot>>>,
    }

    impl InMemoryEventStore {
        pub fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl EventStore for InMemoryEventStore {
        async fn append(
            &self,
            aggregate_id: &str,
            version_check: VersionCheck,
            events: Vec<Event>,
        ) -> EventStoreResult<()> {
            validate_audit_batch(aggregate_id, &events)?;

            let mut store = self.events.lock().await;

            let current_version = store
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .map(|e| e.sequence)
                .max()
                .unwrap_or(0);

            if let Some(expected) = version_check.version() {
                if current_version != expected {
                    return Err(EventStoreError::ConcurrencyConflict {
                        aggregate_id: aggregate_id.to_string(),
                        expected,
                        actual: current_version,
                    });
                }
            }

            store.extend(events);
            Ok(())
        }

        async fn append_to(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
            version_check: VersionCheck,
            events: Vec<Event>,
        ) -> EventStoreResult<()> {
            validate_audit_batch(aggregate_id, &events)?;
            let mut store = self.events.lock().await;
            let current_version = store
                .iter()
                .filter(|event| {
                    event.aggregate_type == aggregate_type && event.aggregate_id == aggregate_id
                })
                .map(|event| event.sequence)
                .max()
                .unwrap_or(0);

            if let Some(expected) = version_check.version() {
                if current_version != expected {
                    return Err(EventStoreError::ConcurrencyConflict {
                        aggregate_id: aggregate_id.to_string(),
                        expected,
                        actual: current_version,
                    });
                }
            }

            store.extend(events);
            Ok(())
        }

        async fn load(&self, aggregate_id: &str) -> EventStoreResult<Vec<Event>> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .cloned()
                .collect())
        }

        async fn load_stream(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
        ) -> EventStoreResult<Vec<Event>> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|event| {
                    event.aggregate_type == aggregate_type && event.aggregate_id == aggregate_id
                })
                .cloned()
                .collect())
        }

        async fn load_event(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
            sequence: i64,
        ) -> EventStoreResult<Option<Event>> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .find(|event| {
                    event.aggregate_type == aggregate_type
                        && event.aggregate_id == aggregate_id
                        && event.sequence == sequence
                })
                .cloned())
        }

        async fn load_from(
            &self,
            aggregate_id: &str,
            from_sequence: i64,
        ) -> EventStoreResult<Vec<Event>> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id && e.sequence >= from_sequence)
                .cloned()
                .collect())
        }

        async fn load_stream_from(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
            from_sequence: i64,
        ) -> EventStoreResult<Vec<Event>> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|event| {
                    event.aggregate_type == aggregate_type
                        && event.aggregate_id == aggregate_id
                        && event.sequence >= from_sequence
                })
                .cloned()
                .collect())
        }

        async fn stream_all(&self, from_position: i64) -> EventStoreResult<Vec<Event>> {
            let store = self.events.lock().await;
            Ok(store.iter().skip(from_position as usize).cloned().collect())
        }

        async fn get_version(&self, aggregate_id: &str) -> EventStoreResult<i64> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .map(|e| e.sequence)
                .max()
                .unwrap_or(0))
        }

        async fn get_stream_version(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
        ) -> EventStoreResult<i64> {
            let store = self.events.lock().await;
            Ok(store
                .iter()
                .filter(|event| {
                    event.aggregate_type == aggregate_type && event.aggregate_id == aggregate_id
                })
                .map(|event| event.sequence)
                .max()
                .unwrap_or(0))
        }

        async fn save_snapshot(&self, snapshot: &Snapshot) -> EventStoreResult<()> {
            let mut snapshots = self.snapshots.lock().await;
            snapshots.insert(
                (
                    snapshot.aggregate_type.clone(),
                    snapshot.aggregate_id.clone(),
                ),
                snapshot.clone(),
            );
            Ok(())
        }

        async fn load_snapshot(&self, aggregate_id: &str) -> EventStoreResult<Option<Snapshot>> {
            let snapshots = self.snapshots.lock().await;
            Ok(snapshots
                .iter()
                .find(|((_, id), _)| id == aggregate_id)
                .map(|(_, snapshot)| snapshot.clone()))
        }

        async fn load_snapshot_for(
            &self,
            aggregate_type: &str,
            aggregate_id: &str,
        ) -> EventStoreResult<Option<Snapshot>> {
            let snapshots = self.snapshots.lock().await;
            Ok(snapshots
                .get(&(aggregate_type.to_string(), aggregate_id.to_string()))
                .cloned())
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryEventStore;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditMetadata;
    use crate::event::Event;
    use serde_json::json;

    #[test]
    fn test_version_check_new() {
        assert_eq!(VersionCheck::New.version(), Some(0));
    }

    #[test]
    fn test_version_check_expected() {
        assert_eq!(VersionCheck::Expected(5).version(), Some(5));
    }

    #[test]
    fn test_version_check_auto() {
        assert_eq!(VersionCheck::Auto.version(), None);
    }

    #[test]
    fn test_error_messages() {
        let error = EventStoreError::ConcurrencyConflict {
            aggregate_id: "user-123".to_string(),
            expected: 5,
            actual: 6,
        };
        let msg = error.to_string();
        assert!(msg.contains("expected version 5"));
        assert!(msg.contains("aggregate is at version 6"));
        assert!(msg.contains("user-123"));

        assert!(EventStoreError::database("X").to_string().contains("X"));
        assert!(EventStoreError::serialization("Y")
            .to_string()
            .contains("Y"));
    }

    #[test]
    fn test_validate_audit_batch_rejects_pending() {
        let mut e = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u1",
            sequence: 1,
            event_type: "X",
            payload: json!({}),
        });
        e.audit = AuditMetadata::pending();
        let err = validate_audit_batch("u1", &[e]).unwrap_err();
        assert!(matches!(
            err,
            EventStoreError::InvalidAudit { event_index: 0, .. }
        ));
    }

    #[test]
    fn test_validate_audit_batch_passes_stamped() {
        let e = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u1",
            sequence: 1,
            event_type: "X",
            payload: json!({}),
        })
        .with_audit(AuditMetadata::test_default());
        validate_audit_batch("u1", &[e]).expect("stamped audit must pass");
    }

    #[tokio::test]
    async fn test_in_memory_store_rejects_pending_audit() {
        let store = InMemoryEventStore::new();
        let e = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u1",
            sequence: 1,
            event_type: "X",
            payload: json!({}),
        }); // pending
        let err = store
            .append("u1", VersionCheck::New, vec![e])
            .await
            .unwrap_err();
        assert!(matches!(err, EventStoreError::InvalidAudit { .. }));
    }

    #[tokio::test]
    async fn test_in_memory_store_persists_stamped_event() {
        let store = InMemoryEventStore::new();
        let e = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "u1",
            sequence: 1,
            event_type: "X",
            payload: json!({}),
        })
        .with_audit(AuditMetadata::test_default());
        store
            .append("u1", VersionCheck::New, vec![e.clone()])
            .await
            .unwrap();
        let loaded = store.load("u1").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(store.load_event("User", "u1", 1).await.unwrap(), Some(e));
        assert_eq!(store.load_event("Order", "u1", 1).await.unwrap(), None);
        assert_eq!(store.load_event("User", "u1", 2).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_in_memory_snapshot_save_then_load() {
        let store = InMemoryEventStore::new();
        let snap = Snapshot::new("u1", "User", 3, json!({ "name": "Alice" }));
        store.save_snapshot(&snap).await.unwrap();
        let loaded = store.load_snapshot("u1").await.unwrap();
        assert_eq!(loaded, Some(snap));
    }

    #[tokio::test]
    async fn test_in_memory_load_snapshot_unknown_returns_none() {
        let store = InMemoryEventStore::new();
        assert_eq!(store.load_snapshot("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_in_memory_snapshot_overwrites_on_resave() {
        let store = InMemoryEventStore::new();
        store
            .save_snapshot(&Snapshot::new("u1", "User", 3, json!({ "v": 3 })))
            .await
            .unwrap();
        let newer = Snapshot::new("u1", "User", 9, json!({ "v": 9 }));
        store.save_snapshot(&newer).await.unwrap();
        let loaded = store.load_snapshot("u1").await.unwrap().unwrap();
        assert_eq!(loaded.version, 9);
        assert_eq!(loaded.state["v"], 9);
    }
}
