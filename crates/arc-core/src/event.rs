//! # Event Module
//!
//! Core event type for event sourcing. Events represent immutable facts about
//! things that have happened in the system.
//!
//! ## Audit metadata
//!
//! Every event carries an [`AuditMetadata`] value.
//! Aggregates produce events with `AuditMetadata::pending()`; the
//! `CommandBus::dispatch` implementation overwrites that placeholder with a
//! validated audit struct sourced from the request `CommandContext` before
//! calling `EventStore::append`. Stores reject events whose audit fails
//! validation.
//!
//! ## Design Principles
//!
//! - **Immutable**: Once persisted, events cannot be changed.
//! - **Serializable**: All events can be stored as JSON.
//! - **Self-describing**: Events contain all metadata needed to understand them.
//! - **Ordered**: Events have a sequence number within their aggregate.
//! - **Audited**: Every persisted event carries `who/when/where/why` audit data.

use crate::audit::AuditMetadata;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Core event type representing an immutable domain event.
///
/// # Fields
///
/// - `event_id`: Unique identifier for this specific event occurrence
/// - `aggregate_type`: Type of aggregate this event belongs to (e.g., "User")
/// - `aggregate_id`: ID of the specific aggregate instance
/// - `sequence`: Sequential number within the aggregate stream (starts at 1)
/// - `event_type`: Type of event (e.g., "UserRegistered")
/// - `payload`: Event data as JSON (flexible, evolvable schema)
/// - `audit`: HIPAA audit metadata. `pending()` until the bus stamps it.
/// - `timestamp`: When the event occurred (milliseconds since UNIX epoch)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Unique identifier for this event
    pub event_id: Uuid,

    /// Aggregate type (e.g., "User", "Order")
    pub aggregate_type: String,

    /// Aggregate instance identifier
    pub aggregate_id: String,

    /// Sequence number within the aggregate (starts at 1)
    pub sequence: i64,

    /// Event type (e.g., "UserCreated", "ProfileUpdated")
    /// Convention: Past tense, PascalCase
    pub event_type: String,

    /// Event payload as JSON
    pub payload: serde_json::Value,

    /// HIPAA audit metadata. `AuditMetadata::pending()` until the
    /// `CommandBus` overwrites it before `append`. `EventStore::append`
    /// implementations call `audit.validate()` and reject pending values.
    pub audit: AuditMetadata,

    /// Wall-clock timestamp (milliseconds since UNIX epoch). For HIPAA
    /// audit-quality time, use `audit.timestamp_utc_us` (microsecond precision).
    pub timestamp: u64,
}

/// Named fields required to create a new domain event.
///
/// The string fields accept either borrowed values such as `"Product"` or
/// owned `String` values. `Event::new` converts them into owned strings.
#[derive(Debug, Clone)]
pub struct NewEvent<AggregateType, AggregateId, EventType> {
    /// Aggregate type (for example, `"Product"`).
    pub aggregate_type: AggregateType,

    /// Aggregate instance identifier.
    pub aggregate_id: AggregateId,

    /// Next sequence number in this aggregate's event stream.
    pub sequence: i64,

    /// Event type in past-tense PascalCase (for example, `"ProductCreated"`).
    pub event_type: EventType,

    /// Event-specific data.
    pub payload: serde_json::Value,
}

impl Event {
    /// Create a new event with `audit = AuditMetadata::pending()`.
    ///
    /// Aggregates call this from `handle()` and the `CommandBus` overwrites the
    /// audit field with a request-scoped value before persisting. The
    /// pending placeholder fails store-side validation, so a forgotten stamp
    /// is impossible to commit.
    ///
    /// # Example
    ///
    /// ```rust
    /// use arc_core::event::{Event, NewEvent};
    /// use serde_json::json;
    ///
    /// let event = Event::new(NewEvent {
    ///     aggregate_type: "User",
    ///     aggregate_id: "user-456",
    ///     sequence: 1,
    ///     event_type: "UserCreated",
    ///     payload: json!({ "name": "Bob", "email": "bob@example.com" }),
    /// });
    /// assert!(event.audit.is_pending());
    /// ```
    pub fn new<AggregateType, AggregateId, EventType>(
        event: NewEvent<AggregateType, AggregateId, EventType>,
    ) -> Self
    where
        AggregateType: Into<String>,
        AggregateId: Into<String>,
        EventType: Into<String>,
    {
        Self {
            event_id: Uuid::new_v4(),
            aggregate_type: event.aggregate_type.into(),
            aggregate_id: event.aggregate_id.into(),
            sequence: event.sequence,
            event_type: event.event_type.into(),
            payload: event.payload,
            audit: AuditMetadata::pending(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_millis() as u64,
        }
    }

    /// Replace `audit` with a fully-stamped value. Used by `CommandBus`
    /// before calling `EventStore::append`.
    pub fn with_audit(mut self, audit: AuditMetadata) -> Self {
        self.audit = audit;
        self
    }

    /// Round `timestamp` down to whole seconds, the precision the SQLite and
    /// Postgres stores persist. `CommandBus` and `InMemoryEventStore` apply
    /// this so every store returns the same `timestamp` for the same event.
    pub fn with_stored_timestamp_precision(mut self) -> Self {
        self.timestamp -= self.timestamp % 1_000;
        self
    }

    /// Serialize event to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize event from JSON string.
    pub fn from_json(json_str: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditMetadata;
    use serde_json::json;

    #[test]
    fn test_event_creation() {
        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-123",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({
                "name": "Test User",
                "email": "test@example.com"
            }),
        });

        assert_eq!(event.aggregate_type, "User");
        assert_eq!(event.aggregate_id, "user-123");
        assert_eq!(event.sequence, 1);
        assert_eq!(event.event_type, "UserCreated");
        assert_eq!(event.payload["name"], "Test User");
        assert!(event.timestamp > 0);
        assert!(event.audit.is_pending());
    }

    #[test]
    fn test_with_audit_overwrites_pending() {
        let stamp = AuditMetadata::test_default();
        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        })
        .with_audit(stamp.clone());
        assert!(!event.audit.is_pending());
        assert_eq!(event.audit, stamp);
    }

    #[test]
    fn test_event_serialization_roundtrips_audit() {
        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-789",
            sequence: 3,
            event_type: "ProfileUpdated",
            payload: json!({"name": "X"}),
        })
        .with_audit(AuditMetadata::test_default());

        let json_str = event.to_json().unwrap();
        let deserialized = Event::from_json(&json_str).unwrap();

        assert_eq!(event.event_id, deserialized.event_id);
        assert_eq!(event.audit, deserialized.audit);
    }

    #[test]
    fn test_event_ordering() {
        let event1 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        });
        let event2 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 2,
            event_type: "ProfileUpdated",
            payload: json!({}),
        });

        assert!(event1.sequence < event2.sequence);
    }

    #[test]
    fn test_event_uniqueness() {
        let event1 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        });
        let event2 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: json!({}),
        });
        assert_ne!(event1.event_id, event2.event_id);
    }
}
