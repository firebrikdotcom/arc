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
        .map_err(|_| FfiError::StackInitializationFailed.logged("stack_initialization"))
}
