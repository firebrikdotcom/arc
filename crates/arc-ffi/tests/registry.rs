mod support;
use arc_core::{
    aggregate::Aggregate, command_bus::CommandBus, event_bus::InProcessEventBus,
    event_store::InMemoryEventStore, projection::ProjectionEngine,
    read_model_store::InMemoryReadModelStore,
};
use arc_ffi::{CoreKey, CoreRegistry, FfiError, FfiRuntime};
use arc_stack::SharedStack;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use support::Example;
use tokio::sync::{oneshot, Barrier};

fn stack<A: Aggregate>() -> SharedStack<A> {
    SharedStack {
        command_bus: Arc::new(CommandBus::new(
            Box::new(InMemoryEventStore::new()),
            Box::new(InProcessEventBus::new()),
        )),
        read_model_store: Arc::new(InMemoryReadModelStore::new()),
        projection_engine: Arc::new(ProjectionEngine::new(Box::new(InMemoryEventStore::new()))),
    }
}
fn key() -> CoreKey {
    CoreKey::postgres("test-database".into()).unwrap()
}

#[tokio::test]
async fn concurrent_open_retains_identity_and_runs_closure_on_owned_runtime() {
    let runtime = FfiRuntime::new().unwrap();
    let worker = runtime
        .run(async { std::thread::current().id() })
        .await
        .unwrap();
    let registry = CoreRegistry::new(runtime);
    let count = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    let barrier = Arc::new(Barrier::new(13));
    for _ in 0..12 {
        let (registry, count, barrier) = (registry.clone(), count.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            registry
                .open::<Example, _, _>(key(), "schema-v1".into(), move |_| async move {
                    assert_eq!(std::thread::current().id(), worker);
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(stack())
                })
                .await
                .unwrap()
        }));
    }
    barrier.wait().await;
    let first = tasks.remove(0).await.unwrap();
    for task in tasks {
        assert!(Arc::ptr_eq(&first, &task.await.unwrap()));
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let retained = Arc::downgrade(&first);
    let bus = first.stack().command_bus;
    drop(first);
    let reopened = registry
        .open::<Example, _, _>(key(), "schema-v1".into(), |_| async {
            panic!("must not reinitialize")
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&retained.upgrade().unwrap(), &reopened));
    assert!(Arc::ptr_eq(&bus, &reopened.stack().command_bus));
    reopened
        .run::<_, FfiError, _, _>(move |_| {
            assert_eq!(
                std::thread::current().id(),
                worker,
                "closure runs on owned worker"
            );
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .open::<Example, _, _>(key(), "different".into(), |_| async { Ok(stack()) })
            .await,
        Err(FfiError::ConfigurationConflict)
    ));
    assert!(matches!(
        registry
            .open::<Example, _, _>(key(), " ".into(), |_| async { Ok(stack()) })
            .await,
        Err(FfiError::InvalidConfiguration)
    ));
}

#[tokio::test]
async fn failure_panic_and_explicit_retry_never_strand_waiters() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let first = registry
        .open::<Example, _, _>(key(), "v1".into(), |_| async {
            Err(FfiError::SchemaInitializationFailed)
        })
        .await;
    assert!(matches!(first, Err(FfiError::SchemaInitializationFailed)));
    let panicked = registry
        .open::<Example, _, _>(key(), "v1".into(), |_| async {
            panic!("private initialization payload")
        })
        .await;
    assert!(matches!(panicked, Err(FfiError::InitializationPanicked)));
    registry
        .open::<Example, _, _>(key(), "v1".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_initialization_accepts_a_corrected_configuration() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let failed = registry
        .open::<Example, _, _>(key(), "broken".into(), |_| async {
            Err(FfiError::SchemaInitializationFailed)
        })
        .await;
    assert!(matches!(failed, Err(FfiError::SchemaInitializationFailed)));
    let corrected = registry
        .open::<Example, _, _>(key(), "fixed".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .open::<Example, _, _>(key(), "broken".into(), |_| async { Ok(stack()) })
            .await,
        Err(FfiError::ConfigurationConflict)
    ));
    let reopened = registry
        .open::<Example, _, _>(key(), "fixed".into(), |_| async {
            panic!("must not reinitialize")
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&corrected, &reopened));
}

#[tokio::test]
async fn cancelled_opener_leaves_initializer_alive_for_other_waiters() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let owner = registry.clone();
    let task = tokio::spawn(async move {
        owner
            .open::<Example, _, _>(key(), "v1".into(), |_| async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(stack())
            })
            .await
    });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    let waiter = registry.open::<Example, _, _>(key(), "v1".into(), |_| async {
        panic!("duplicate initialization")
    });
    release_tx.send(()).unwrap();
    waiter.await.unwrap();
}

#[derive(Default)]
struct Other;
#[async_trait::async_trait]
impl Aggregate for Other {
    type Command = support::Create;
    type Event = ();
    type Error = std::io::Error;
    fn aggregate_type() -> &'static str {
        "Other"
    }
    fn version(&self) -> i64 {
        0
    }
    async fn handle(&self, _: Self::Command) -> Result<Vec<arc_core::event::Event>, Self::Error> {
        Ok(vec![])
    }
    fn apply(&mut self, _: &arc_core::event::Event) {}
}

#[tokio::test]
async fn aggregate_typeid_separates_entries_and_domain_errors_are_preserved() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let a = registry
        .open::<Example, _, _>(key(), "first".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
    let _b = registry
        .open::<Other, _, _>(key(), "second".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
    #[derive(Debug, PartialEq)]
    enum DomainError {
        Infrastructure(FfiError),
        Rejected,
    }
    impl From<FfiError> for DomainError {
        fn from(e: FfiError) -> Self {
            Self::Infrastructure(e)
        }
    }
    assert_eq!(
        a.run(|_| async { Err::<(), _>(DomainError::Rejected) })
            .await,
        Err(DomainError::Rejected)
    );
    assert_eq!(
        a.run::<(), DomainError, _, _>(|_| {
            panic!("closure panic");
            #[allow(unreachable_code)]
            async {
                Ok(())
            }
        })
        .await,
        Err(DomainError::Infrastructure(FfiError::OperationPanicked))
    );
}

#[cfg(not(feature = "sqlite"))]
#[tokio::test]
async fn disabled_backend_returns_safe_stack_error() {
    let result = arc_ffi::build_stack::<Example>(
        arc_stack::config::DatabaseDriver::Sqlite,
        "unused-private-path",
        vec![],
        None,
    )
    .await;
    assert!(matches!(result, Err(FfiError::StackInitializationFailed)));
}

#[tokio::test]
async fn detached_operation_retains_runtime_after_all_external_owners_drop() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let core = registry
        .open::<Example, _, _>(key(), "v1".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (finished_tx, finished_rx) = oneshot::channel();
    let waiter = tokio::spawn(async move {
        core.run::<(), FfiError, _, _>(|_| async move {
            started_tx.send(()).unwrap();
            release_rx.await.unwrap();
            finished_tx.send(()).unwrap();
            Ok(())
        })
        .await
    });
    started_rx.await.unwrap();
    waiter.abort();
    assert!(waiter.await.err().unwrap().is_cancelled());
    drop(registry);
    release_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), finished_rx)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn initialization_panic_wakes_every_waiter_and_next_open_can_retry() {
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let owner = registry.clone();
    let first = tokio::spawn(async move {
        owner
            .open::<Example, _, _>(key(), "v1".into(), |_| async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                panic!("initializer panic");
            })
            .await
    });
    started_rx.await.unwrap();
    let second = registry.open::<Example, _, _>(key(), "v1".into(), |_| async {
        panic!("waiter must not initialize")
    });
    tokio::pin!(second);
    // Poll the second opener into its wait state before releasing initialization.
    tokio::select! {
        biased;
        _ = &mut second => panic!("initialization returned before release"),
        _ = std::future::ready(()) => {}
    }
    release_tx.send(()).unwrap();
    assert!(matches!(
        first.await.unwrap(),
        Err(FfiError::InitializationPanicked)
    ));
    assert!(matches!(
        second.await,
        Err(FfiError::InitializationPanicked)
    ));
    registry
        .open::<Example, _, _>(key(), "v1".into(), |_| async { Ok(stack()) })
        .await
        .unwrap();
}

#[cfg(not(feature = "postgres"))]
#[tokio::test]
async fn disabled_postgres_returns_safe_stack_error() {
    assert!(matches!(
        arc_ffi::build_stack::<Example>(
            arc_stack::config::DatabaseDriver::Postgres,
            "unused-private-dsn",
            vec![],
            None
        )
        .await,
        Err(FfiError::StackInitializationFailed)
    ));
}
