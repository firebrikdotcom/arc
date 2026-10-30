//! Monomorphic application export, also used as an external package consumer.
#[path = "../../../support/mod.rs"]
mod support;
use arc_core::command_bus::CommandContext;
use arc_ffi::{CoreKey, CoreRegistry, FfiCore, FfiError};
use std::sync::{atomic::AtomicUsize, Arc};
use support::{Create, Example};
uniffi::setup_scaffolding!();
arc_ffi::uniffi_reexport_scaffolding!();

#[derive(uniffi::Object)]
pub struct NativeCore {
    core: Arc<FfiCore<Example>>,
}
#[uniffi::export]
impl NativeCore {
    #[uniffi::constructor]
    pub async fn open(path: String) -> Result<Arc<Self>, FfiError> {
        let key = CoreKey::sqlite_file(path.into())?;
        let core = CoreRegistry::process()?
            .open(key, "fixture-schema-v1".into(), |key| {
                support::initialize(key, Arc::new(AtomicUsize::new(0)))
            })
            .await?;
        Ok(Arc::new(Self { core }))
    }
    pub async fn create_and_read(&self, id: String) -> Result<String, FfiError> {
        self.core
            .run(move |stack| async move {
                stack
                    .command_bus
                    .dispatch(Create(id.clone()), CommandContext::system())
                    .await
                    .map_err(|_| FfiError::StackInitializationFailed)?;
                let row = stack
                    .read_model_store
                    .get("items", &id)
                    .await
                    .map_err(|_| FfiError::StackInitializationFailed)?
                    .ok_or(FfiError::StackInitializationFailed)?;
                Ok(row["data"].as_str().unwrap().to_owned())
            })
            .await
    }
    pub async fn infrastructure_error(&self) -> Result<(), FfiError> {
        Err(FfiError::ProjectionRebuildFailed)
    }
}
