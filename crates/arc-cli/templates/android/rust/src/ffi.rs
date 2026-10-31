use std::sync::Arc;
use arc_ffi::FfiError;
// arc:ffi-imports

#[derive(uniffi::Object)]
pub struct NativeCore {
    // arc:ffi-fields
}

#[uniffi::export]
impl NativeCore {
    #[uniffi::constructor]
    pub async fn open(path: String) -> Result<Arc<Self>, FfiError> {
        // The empty chooser has no database; resource initializers use the registry runtime.
        let _ = &path;
        // arc:ffi-open-initializers
        Ok(Arc::new(Self {
            // arc:ffi-open-fields
        }))
    }
    // arc:ffi-methods
}
