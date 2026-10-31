//! Native adapter infrastructure. Applications own schema, replay and domain exports.
mod key;
mod registry;
mod runtime;

pub use key::CoreKey;
pub use registry::{CoreRegistry, FfiCore};
pub use runtime::FfiRuntime;

use arc_core::{aggregate::Aggregate, command_bus::SnapshotPolicy};
use arc_stack::{config::DatabaseDriver, ProjectorReg, SharedStack};

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

/// Safe infrastructure categories; database inputs and panic payloads never cross FFI.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum FfiError {
    #[error("invalid database path")]
    InvalidDatabasePath,
    #[error("invalid configuration identity")]
    InvalidConfiguration,
    #[error("core configuration conflicts with an existing registration")]
    ConfigurationConflict,
    #[error("native runtime unavailable")]
    RuntimeUnavailable,
    #[error("schema initialization failed")]
    SchemaInitializationFailed,
    #[error("stack initialization failed")]
    StackInitializationFailed,
    #[error("projection rebuild failed")]
    ProjectionRebuildFailed,
    #[error("core initialization panicked")]
    InitializationPanicked,
    #[error("core operation panicked")]
    OperationPanicked,
    #[error("native task cancelled")]
    TaskCancelled,
}

impl FfiError {
    pub(crate) fn logged(self, stage: &'static str) -> Self {
        tracing::error!(stage, category = %self, "native adapter failure");
        self
    }
}

/// Delegate assembly to arc-stack. The caller prepares schema and chooses replay.
pub async fn build_stack<A: Aggregate>(
    driver: DatabaseDriver,
    database_url: &str,
    projectors: Vec<ProjectorReg>,
    snapshot_policy: Option<SnapshotPolicy>,
) -> Result<SharedStack<A>, FfiError> {
    arc_stack::es_stack::build_with_driver(driver, database_url, projectors, snapshot_policy)
        .await
        .map(Into::into)
        .map_err(|error| {
            // Log only a fixed category; messages may carry paths, DSNs or SQL.
            tracing::error!(
                cause = stack_failure_cause(error.as_ref()),
                "stack initialization cause"
            );
            FfiError::StackInitializationFailed.logged("stack_initialization")
        })
}

fn stack_failure_cause(error: &(dyn std::error::Error + 'static)) -> &'static str {
    use arc_core::{
        event_bus::EventBusError, event_store::EventStoreError, read_model_store::ReadModelError,
    };
    if error.is::<EventStoreError>() {
        "event_store"
    } else if error.is::<ReadModelError>() {
        "read_model_store"
    } else if error.is::<EventBusError>() {
        "event_bus"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::stack_failure_cause;
    use arc_core::{event_store::EventStoreError, read_model_store::ReadModelError};

    #[test]
    fn stack_failure_cause_is_a_fixed_category() {
        let store: Box<dyn std::error::Error> = Box::new(EventStoreError::AggregateNotFound {
            aggregate_id: "/private/path".into(),
        });
        let read: Box<dyn std::error::Error> =
            Box::new(ReadModelError::other("postgres://secret@host/db"));
        let other: Box<dyn std::error::Error> = "SELECT secret".into();
        assert_eq!(stack_failure_cause(store.as_ref()), "event_store");
        assert_eq!(stack_failure_cause(read.as_ref()), "read_model_store");
        assert_eq!(stack_failure_cause(other.as_ref()), "other");
    }
}
