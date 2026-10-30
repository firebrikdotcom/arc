use crate::{CoreKey, FfiError, FfiRuntime};
use arc_core::aggregate::Aggregate;
use arc_stack::SharedStack;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::watch;

/// Shared infrastructure core; monomorphic application wrappers own FFI exports.
pub struct FfiCore<A: Aggregate + 'static> {
    runtime: Arc<FfiRuntime>,
    stack: SharedStack<A>,
}

impl<A: Aggregate + 'static> FfiCore<A> {
    pub fn stack(&self) -> SharedStack<A> {
        self.stack.clone()
    }

    /// Execute the closure and its future on the core runtime, preserving domain errors.
    pub async fn run<T, E, F, Fut>(&self, operation: F) -> Result<T, E>
    where
        F: FnOnce(SharedStack<A>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: From<FfiError> + Send + 'static,
    {
        let owner = self.runtime.clone();
        let stack = self.stack();
        self.runtime
            .run(async move {
                let _owner = owner;
                operation(stack).await
            })
            .await
            .map_err(E::from)?
    }
}

type Outcome<A> = Option<Result<Arc<FfiCore<A>>, FfiError>>;
enum State<A: Aggregate + 'static> {
    Idle,
    Running(watch::Sender<Outcome<A>>),
    Ready(Arc<FfiCore<A>>),
}
struct Entry<A: Aggregate + 'static> {
    configuration: String,
    state: Mutex<State<A>>,
}
type Entries = HashMap<(TypeId, CoreKey), Arc<dyn Any + Send + Sync>>;

/// One retained core per aggregate/database in this registry. No cross-process lock.
pub struct CoreRegistry {
    runtime: Arc<FfiRuntime>,
    entries: Mutex<Entries>,
}

impl CoreRegistry {
    /// Singleton for this linked component. Separately loaded libraries are independent.
    pub fn process() -> Result<Arc<Self>, FfiError> {
        static PROCESS: OnceLock<Mutex<Option<Arc<CoreRegistry>>>> = OnceLock::new();
        let mut process = PROCESS.get_or_init(|| Mutex::new(None)).lock().unwrap();
        if let Some(registry) = process.as_ref() {
            return Ok(registry.clone());
        }
        let registry = Self::new(FfiRuntime::new()?);
        *process = Some(registry.clone());
        Ok(registry)
    }

    pub fn new(runtime: Arc<FfiRuntime>) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            entries: Mutex::new(HashMap::new()),
        })
    }

    /// Initialize once on the owned runtime. Failed attempts are retryable explicitly.
    /// Callbacks must be idempotent and must never await opening their own key.
    pub async fn open<A, F, Fut>(
        self: &Arc<Self>,
        key: CoreKey,
        configuration_id: String,
        initialize: F,
    ) -> Result<Arc<FfiCore<A>>, FfiError>
    where
        A: Aggregate + 'static,
        F: FnOnce(CoreKey) -> Fut + Send + 'static,
        Fut: Future<Output = Result<SharedStack<A>, FfiError>> + Send + 'static,
    {
        if configuration_id.trim().is_empty() {
            return Err(FfiError::InvalidConfiguration.logged("core_registration"));
        }
        let entry = {
            let mut entries = self.entries.lock().unwrap();
            entries
                .entry((TypeId::of::<A>(), key.clone()))
                .or_insert_with(|| {
                    Arc::new(Entry::<A> {
                        configuration: configuration_id.clone(),
                        state: Mutex::new(State::Idle),
                    })
                })
                .clone()
                .downcast::<Entry<A>>()
                .expect("aggregate TypeId matches entry")
        };
        if entry.configuration != configuration_id {
            return Err(FfiError::ConfigurationConflict.logged("core_registration"));
        }
        let mut receiver = {
            let mut state = entry.state.lock().unwrap();
            match &*state {
                State::Ready(core) => return Ok(core.clone()),
                State::Running(sender) => sender.subscribe(),
                State::Idle => {
                    let (sender, receiver) = watch::channel(None);
                    *state = State::Running(sender.clone());
                    let entry = entry.clone();
                    let runtime = self.runtime.clone();
                    // Supervisor and initializer are detached from the opener. User code
                    // executes only in the inner task, whose panic cannot strand the cell.
                    self.runtime.handle.spawn(async move {
                        let initialized = runtime
                            .handle
                            .spawn(async move { initialize(key).await })
                            .await;
                        let result = match initialized {
                            Ok(Ok(stack)) => Ok(Arc::new(FfiCore { runtime, stack })),
                            Ok(Err(error)) => Err(error.logged("core_initialization")),
                            Err(error) if error.is_panic() => {
                                Err(FfiError::InitializationPanicked.logged("core_initialization"))
                            }
                            Err(_) => Err(FfiError::TaskCancelled.logged("core_initialization")),
                        };
                        let mut state = entry.state.lock().unwrap();
                        *state = match &result {
                            Ok(core) => State::Ready(core.clone()),
                            Err(_) => State::Idle,
                        };
                        sender.send_replace(Some(result));
                    });
                    receiver
                }
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| FfiError::TaskCancelled.logged("core_wait"))?;
        }
    }
}
