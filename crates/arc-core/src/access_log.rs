//! # Access Logger
//!
//! Generic read-side audit logger. Where `EventStore` records *writes* with
//! [`AuditMetadata`](crate::audit::AuditMetadata), `AccessLogger` records
//! *reads* of sensitive data. These primitives do not establish compliance or
//! provide durable storage by themselves.
//!
//! Reads do not go through the event store. Controllers must explicitly
//! invoke [`AccessLogger::log_access`] before returning data classified as
//! anything beyond [`Sensitivity::Public`].
//!
//! ## Why generic, not PHI-specific
//!
//! The mechanism is the same regardless of regime: log who looked at what,
//! when, and why. [`Sensitivity`] classifies data so a downstream sink can
//! apply deployment-specific routing, separate categories, drop
//! [`Sensitivity::Public`] reads, and so on.
//!
//! ## Lifecycle
//!
//! 1. A read controller resolves the actor (typically the JWT-bound aggregate UUID).
//! 2. Builds an [`AccessedResource`] describing what's about to be returned.
//! 3. Calls `logger.log_access(actor, resource, purpose).await`.
//! 4. Returns the data to the client.
//!
//! Default implementations in tests and non-regulated apps use
//! [`NoOpAccessLogger`] which validates inputs but discards them. Real
//! deployments can select the dedicated `arc-es-sqlite::SqliteAccessLogger`.
//! Coverage remains opt-in at each disclosure boundary.

use crate::audit::now_us;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Errors emitted by [`AccessLogger`] implementations.
#[derive(Debug, Error)]
pub enum AccessLogError {
    #[error("access log validation failed: {0}")]
    Validation(String),
    #[error("access log sink failed: {0}")]
    Sink(String),
}

/// Identity of the entity performing the read.
///
/// `aggregate_id` is the framework-level handle (UUID, `"system"`,
/// `"anonymous"`, or `"legacy-pre-hipaa"` — the same vocabulary as
/// [`AuditMetadata::actor_id`](crate::audit::AuditMetadata::actor_id)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub actor_id: String,
    pub session_id: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
}

impl Identity {
    pub fn new(actor_id: impl Into<String>) -> Self {
        Self {
            actor_id: actor_id.into(),
            session_id: None,
            source_ip: None,
            user_agent: None,
        }
    }
}

/// What controllers should do when an [`AccessLogger`] sink fails.
///
/// Framework defaults: PHI and PCI refuse the response (HTTP 503); other
/// classifications, including PII, warn and allow the response. These are
/// implementation defaults, not a statement of regulatory requirements.
/// See [`FailurePolicy::for_sensitivity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    FailHard,
    FailOpenWarn,
}

impl FailurePolicy {
    /// Default per regime: PHI and PCI fail hard; everything else logs and continues.
    pub fn for_sensitivity(s: Sensitivity) -> Self {
        match s {
            Sensitivity::Phi | Sensitivity::Pci => FailurePolicy::FailHard,
            _ => FailurePolicy::FailOpenWarn,
        }
    }
}

/// Sensitivity tag used for failure policy and sink routing.
///
/// Audit sinks use this to decide retention, routing, and whether to record
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Protected Health Information (HIPAA §164).
    Phi,
    /// Payment card industry data (PCI-DSS).
    Pci,
    /// Personally identifiable information (GDPR/CCPA).
    Pii,
    /// Trade secrets / internal-only documents.
    Confidential,
    /// Non-public but unregulated business data.
    Internal,
    /// Anything intentionally public (logged for completeness; sinks may downsample).
    Public,
}

impl Sensitivity {
    /// True when reads should be auditable in regulated deployments. Sinks
    /// MAY drop [`Sensitivity::Public`] events to control volume.
    pub fn is_regulated(self) -> bool {
        !matches!(self, Sensitivity::Public)
    }
}

/// Reason the read happened. Maps to HIPAA "purpose of use" categories but is
/// applicable to any regime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurposeOfUse {
    Treatment,
    Payment,
    Operations,
    Emergency,
    UserInitiated,
    AuditReview,
    Other,
}

/// Description of the resource being read.
///
/// `kind` and `identifier` together name the row(s); `fields` enumerates the
/// columns the response will expose; `sensitivity` triggers regime-specific
/// routing in the sink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessedResource {
    pub kind: String,
    pub identifier: String,
    pub fields: Vec<String>,
    pub sensitivity: Sensitivity,
}

impl AccessedResource {
    pub fn new(
        kind: impl Into<String>,
        identifier: impl Into<String>,
        sensitivity: Sensitivity,
    ) -> Self {
        Self {
            kind: kind.into(),
            identifier: identifier.into(),
            fields: Vec::new(),
            sensitivity,
        }
    }

    pub fn with_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }
}

/// Single record produced for every successful `log_access` call.
///
/// Sinks serialize this into their target medium; tests inspect it directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessLogEntry {
    pub access_id: Uuid,
    pub actor: Identity,
    pub resource: AccessedResource,
    pub purpose: PurposeOfUse,
    pub timestamp_utc_us: i64,
    pub correlation_id: Option<Uuid>,
}

impl AccessLogEntry {
    /// Construct an entry from caller inputs. Validates and stamps timestamp.
    pub fn new(
        actor: Identity,
        resource: AccessedResource,
        purpose: PurposeOfUse,
        correlation_id: Option<Uuid>,
    ) -> Result<Self, AccessLogError> {
        if actor.actor_id.trim().is_empty() {
            return Err(AccessLogError::Validation(
                "actor_id must be non-empty (use 'anonymous' or 'system' explicitly)".into(),
            ));
        }
        if resource.kind.trim().is_empty() {
            return Err(AccessLogError::Validation(
                "resource.kind must be non-empty".into(),
            ));
        }
        if resource.identifier.trim().is_empty() {
            return Err(AccessLogError::Validation(
                "resource.identifier must be non-empty".into(),
            ));
        }
        Ok(Self {
            access_id: Uuid::new_v4(),
            actor,
            resource,
            purpose,
            timestamp_utc_us: now_us(),
            correlation_id,
        })
    }
}

/// Sink for read-access audit events.
///
/// Implementations:
/// - [`NoOpAccessLogger`] — validates and discards (default in tests, non-PHI apps)
/// - `RecordingAccessLogger` — keeps entries in memory for assertions (test-utils)
///
/// - `arc-es-sqlite::SqliteAccessLogger` — dedicated durable SQLite journal
#[async_trait]
pub trait AccessLogger: Send + Sync {
    /// Log a read. Implementations validate the input, record it (or not),
    /// and return.
    ///
    /// Callers must apply [`FailurePolicy`] before releasing sensitive data.
    /// The web `record_read` helper fails closed for PHI/PCI and warns while
    /// continuing for other classifications. An `Ok(())` is only as durable
    /// as the chosen implementation: the no-op implementation discards data.
    /// Custom sinks must document their acknowledgement boundary, timeout,
    /// cancellation and retry behavior. Error strings may contain private
    /// implementation details; callers should log only a safe error category.
    async fn log_access(
        &self,
        actor: Identity,
        resource: AccessedResource,
        purpose: PurposeOfUse,
        correlation_id: Option<Uuid>,
    ) -> Result<(), AccessLogError>;
}

/// Validates inputs and discards the entry. Default for test apps and any
/// deployment that has not yet wired a real sink.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpAccessLogger;

#[async_trait]
impl AccessLogger for NoOpAccessLogger {
    async fn log_access(
        &self,
        actor: Identity,
        resource: AccessedResource,
        purpose: PurposeOfUse,
        correlation_id: Option<Uuid>,
    ) -> Result<(), AccessLogError> {
        // Build the entry purely for validation side effects.
        let _ = AccessLogEntry::new(actor, resource, purpose, correlation_id)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers — captured behind the `test-utils` feature so downstream
// integration tests can assert against recorded entries.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "test-utils"))]
mod recording {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// In-memory `AccessLogger` that records every successful entry.
    /// Use `entries()` to assert in tests.
    #[derive(Clone, Default)]
    pub struct RecordingAccessLogger {
        entries: Arc<Mutex<Vec<AccessLogEntry>>>,
    }

    impl RecordingAccessLogger {
        pub fn new() -> Self {
            Self::default()
        }

        pub async fn entries(&self) -> Vec<AccessLogEntry> {
            self.entries.lock().await.clone()
        }
    }

    #[async_trait]
    impl AccessLogger for RecordingAccessLogger {
        async fn log_access(
            &self,
            actor: Identity,
            resource: AccessedResource,
            purpose: PurposeOfUse,
            correlation_id: Option<Uuid>,
        ) -> Result<(), AccessLogError> {
            let entry = AccessLogEntry::new(actor, resource, purpose, correlation_id)?;
            self.entries.lock().await.push(entry);
            Ok(())
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use recording::RecordingAccessLogger;

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_actor() -> Identity {
        Identity::new("alice-uuid")
    }

    fn ok_resource() -> AccessedResource {
        AccessedResource::new("UserProfile", "alice-uuid", Sensitivity::Pii)
            .with_fields(["name", "email"])
    }

    #[tokio::test]
    async fn test_noop_logger_accepts_valid_input() {
        let logger = NoOpAccessLogger;
        logger
            .log_access(ok_actor(), ok_resource(), PurposeOfUse::UserInitiated, None)
            .await
            .expect("noop must accept valid input");
    }

    #[tokio::test]
    async fn test_noop_logger_rejects_empty_actor() {
        let logger = NoOpAccessLogger;
        let bad_actor = Identity::new("");
        let err = logger
            .log_access(bad_actor, ok_resource(), PurposeOfUse::UserInitiated, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AccessLogError::Validation(_)));
    }

    #[tokio::test]
    async fn test_noop_logger_rejects_empty_resource_kind() {
        let bad_resource = AccessedResource::new("", "x", Sensitivity::Pii);
        let err = NoOpAccessLogger
            .log_access(ok_actor(), bad_resource, PurposeOfUse::UserInitiated, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AccessLogError::Validation(_)));
    }

    #[tokio::test]
    async fn test_noop_logger_rejects_empty_identifier() {
        let bad_resource = AccessedResource::new("X", "   ", Sensitivity::Pii);
        let err = NoOpAccessLogger
            .log_access(ok_actor(), bad_resource, PurposeOfUse::UserInitiated, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AccessLogError::Validation(_)));
    }

    #[tokio::test]
    async fn test_recording_logger_captures_entries() {
        let logger = RecordingAccessLogger::new();
        let corr = Uuid::new_v4();

        logger
            .log_access(
                ok_actor(),
                ok_resource(),
                PurposeOfUse::Treatment,
                Some(corr),
            )
            .await
            .unwrap();
        logger
            .log_access(
                Identity::new("bob"),
                AccessedResource::new("Order", "ord-1", Sensitivity::Pci),
                PurposeOfUse::Payment,
                None,
            )
            .await
            .unwrap();

        let entries = logger.entries().await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].actor.actor_id, "alice-uuid");
        assert_eq!(entries[0].resource.sensitivity, Sensitivity::Pii);
        assert_eq!(entries[0].purpose, PurposeOfUse::Treatment);
        assert_eq!(entries[0].correlation_id, Some(corr));
        assert!(entries[0].timestamp_utc_us > 0);
        assert_eq!(entries[1].resource.sensitivity, Sensitivity::Pci);
        assert_eq!(entries[1].correlation_id, None);
    }

    #[test]
    fn test_sensitivity_regulated_classification() {
        assert!(Sensitivity::Phi.is_regulated());
        assert!(Sensitivity::Pci.is_regulated());
        assert!(Sensitivity::Pii.is_regulated());
        assert!(Sensitivity::Confidential.is_regulated());
        assert!(Sensitivity::Internal.is_regulated());
        assert!(!Sensitivity::Public.is_regulated());
    }

    #[test]
    fn test_serde_roundtrip() {
        let entry = AccessLogEntry::new(
            ok_actor(),
            ok_resource(),
            PurposeOfUse::Treatment,
            Some(Uuid::new_v4()),
        )
        .unwrap();
        let s = serde_json::to_string(&entry).unwrap();
        let back: AccessLogEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn test_resource_with_fields_chains() {
        let r = AccessedResource::new("Patient", "pat-1", Sensitivity::Phi)
            .with_fields(["vitals", "notes"]);
        assert_eq!(r.fields, vec!["vitals", "notes"]);
    }

    #[test]
    fn test_failure_policy_phi_pci_fail_hard() {
        assert_eq!(
            FailurePolicy::for_sensitivity(Sensitivity::Phi),
            FailurePolicy::FailHard
        );
        assert_eq!(
            FailurePolicy::for_sensitivity(Sensitivity::Pci),
            FailurePolicy::FailHard
        );
    }

    #[test]
    fn test_failure_policy_others_fail_open_warn() {
        for s in [
            Sensitivity::Pii,
            Sensitivity::Confidential,
            Sensitivity::Internal,
            Sensitivity::Public,
        ] {
            assert_eq!(
                FailurePolicy::for_sensitivity(s),
                FailurePolicy::FailOpenWarn,
                "unexpected default policy for {:?}",
                s
            );
        }
    }
}
