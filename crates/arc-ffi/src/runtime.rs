use crate::FfiError;
use std::{future::Future, sync::Arc};
use tokio::runtime::{Handle, Runtime};

/// Long-lived owned runtime. Dropping a local owner never blocks an async caller.
pub struct FfiRuntime {
    runtime: Option<Runtime>,
    pub(crate) handle: Handle,
}

impl FfiRuntime {
    /// Create one worker with Tokio's I/O and time drivers enabled.
    pub fn new() -> Result<Arc<Self>, FfiError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|_| FfiError::RuntimeUnavailable.logged("runtime_creation"))?;
        Ok(Arc::new(Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
        }))
    }

    /// Spawn on the owned runtime; cancellation of this wait detaches the task.
    pub async fn run<T, F>(&self, future: F) -> Result<T, FfiError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.handle
            .spawn(future)
            .await
            .map_err(operation_join_error)
    }
}

pub(crate) fn operation_join_error(error: tokio::task::JoinError) -> FfiError {
    if error.is_panic() {
        FfiError::OperationPanicked.logged("operation")
    } else {
        FfiError::TaskCancelled.logged("operation")
    }
}

impl Drop for FfiRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_join_has_a_fixed_error() {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        assert_eq!(
            operation_join_error(task.await.unwrap_err()),
            FfiError::TaskCancelled
        );
    }
}
