//! Resource-owned native adapter; schema and stack assembly run on the registry runtime.
use super::{aggregate::{{Type}}Aggregate, commands::{{Type}}Command, projector::{{{Type}}Projector, {{CONSTANT}}_VIEW}};
use arc_core::{command_bus::{CommandBusError, CommandContext}, event_store::EventStoreError};
use arc_ffi::{CoreKey, CoreRegistry, FfiCore, FfiError};
use arc_stack::{config::DatabaseDriver, ProjectorReg};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, uniffi::Record)]
pub struct {{Type}}Record {
    pub id: String,
    pub name: String,
    pub version: i64,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum {{Type}}ResourceError {
    #[error("name cannot be empty")]
    EmptyName,
    #[error("name must not exceed 1024 UTF-8 bytes")]
    NameTooLong,
    #[error("{{module}} not found")]
    NotFound,
    #[error("{{module}} already exists")]
    AlreadyExists,
    #[error("{{module}} changed concurrently; refresh before editing")]
    Conflict,
    #[error("storage operation failed")]
    Storage,
    #[error("the change was committed but delivery failed; refresh before making another change")]
    CommittedDeliveryFailed,
    #[error("native infrastructure failure: {category}")]
    Infrastructure { category: String },
}

impl From<FfiError> for {{Type}}ResourceError {
    fn from(error: FfiError) -> Self { Self::Infrastructure { category: error.to_string() } }
}

impl From<CommandBusError> for {{Type}}ResourceError {
    fn from(error: CommandBusError) -> Self {
        match error {
            CommandBusError::HandleFailed { message, .. } => match message.as_str() {
                "name cannot be empty" => Self::EmptyName,
                "name must not exceed 1024 UTF-8 bytes" => Self::NameTooLong,
                "{{module}} not found" => Self::NotFound,
                "{{module}} already exists" => Self::AlreadyExists,
                _ => Self::Storage,
            },
            CommandBusError::AppendFailed { source: EventStoreError::ConcurrencyConflict { .. }, .. } => Self::Conflict,
            CommandBusError::PublishFailed { .. } => Self::CommittedDeliveryFailed,
            _ => Self::Storage,
        }
    }
}

pub async fn open(registry: Arc<CoreRegistry>, path: String) -> Result<Arc<FfiCore<{{Type}}Aggregate>>, FfiError> {
    let key = CoreKey::sqlite_file(path.into())?;
    // Schema additions are idempotent and run once per process registration. The
    // identity describes the stable aggregate/projector/snapshot configuration.
    registry.open(key, "{{crate-name}}:{{module}}:schema-v1:{{Type}}Projector:no-snapshots".into(), initialize).await
}

async fn initialize(key: CoreKey) -> Result<arc_stack::SharedStack<{{Type}}Aggregate>, FfiError> {
    let path = key.sqlite_path().and_then(|p| p.to_str()).ok_or(FfiError::InvalidDatabasePath)?;
    crate::schema::initialize(path).await?;
    arc_ffi::build_stack::<{{Type}}Aggregate>(DatabaseDriver::Sqlite, path,
        vec![ProjectorReg::new({{Type}}Projector, {{CONSTANT}}_VIEW)], None).await
}

pub async fn list(core: &Arc<FfiCore<{{Type}}Aggregate>>) -> Result<Vec<{{Type}}Record>, {{Type}}ResourceError> {
    core.run(|stack| async move {
        let rows = stack.read_model_store.list({{CONSTANT}}_VIEW).await.map_err(|_| {{Type}}ResourceError::Storage)?;
        rows.into_iter().map(|row| serde_json::from_value(row).map_err(|_| {{Type}}ResourceError::Storage)).collect()
    }).await
}

pub async fn get(core: &Arc<FfiCore<{{Type}}Aggregate>>, id: String) -> Result<{{Type}}Record, {{Type}}ResourceError> {
    core.run(move |stack| async move {
        let row = stack.read_model_store.get({{CONSTANT}}_VIEW, &id).await
            .map_err(|_| {{Type}}ResourceError::Storage)?.ok_or({{Type}}ResourceError::NotFound)?;
        serde_json::from_value(row).map_err(|_| {{Type}}ResourceError::Storage)
    }).await
}

pub async fn create(core: &Arc<FfiCore<{{Type}}Aggregate>>, name: String) -> Result<String, {{Type}}ResourceError> {
    core.run(move |stack| async move {
        let id = uuid::Uuid::new_v4().to_string();
        stack.command_bus.dispatch({{Type}}Command::Create { id: id.clone(), name }, CommandContext::for_actor("native"))
            .await.map_err({{Type}}ResourceError::from)?;
        Ok(id)
    }).await
}

pub async fn rename(core: &Arc<FfiCore<{{Type}}Aggregate>>, id: String, name: String) -> Result<(), {{Type}}ResourceError> {
    core.run(move |stack| async move {
        stack.command_bus.dispatch({{Type}}Command::Rename { id, name }, CommandContext::for_actor("native"))
            .await.map_err({{Type}}ResourceError::from)?;
        Ok(())
    }).await
}

pub async fn delete(core: &Arc<FfiCore<{{Type}}Aggregate>>, id: String) -> Result<(), {{Type}}ResourceError> {
    core.run(move |stack| async move {
        stack.command_bus.dispatch({{Type}}Command::Delete { id }, CommandContext::for_actor("native"))
            .await.map_err({{Type}}ResourceError::from)?;
        Ok(())
    }).await
}
