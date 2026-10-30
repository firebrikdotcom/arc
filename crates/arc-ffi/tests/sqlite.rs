#![cfg(feature = "sqlite")]
mod support;
use arc_core::command_bus::CommandContext;
use arc_ffi::{CoreKey, CoreRegistry, FfiError, FfiRuntime};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use support::{Create, Example};
use tokio::sync::oneshot;

#[tokio::test]
async fn shared_bus_projects_immediately_across_os_threads_and_replay_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let key = CoreKey::sqlite_file(dir.path().join("db.sqlite")).unwrap();
    let runtime = FfiRuntime::new().unwrap();
    let registry = CoreRegistry::new(runtime.clone());
    let count = Arc::new(AtomicUsize::new(0));
    let init_count = count.clone();
    let core = registry
        .open::<Example, _, _>(key.clone(), "v1".into(), move |key| {
            support::initialize(key, init_count)
        })
        .await
        .unwrap();
    for n in 0..4 {
        let (core, registry, key) = (core.clone(), registry.clone(), key.clone());
        std::thread::spawn(move || {
            let caller = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            caller.block_on(async {
                let other = registry
                    .open::<Example, _, _>(key, "v1".into(), |_| async { panic!("second stack") })
                    .await
                    .unwrap();
                assert!(Arc::ptr_eq(
                    &core.stack().command_bus,
                    &other.stack().command_bus
                ));
                assert!(Arc::ptr_eq(
                    &core.stack().projection_engine,
                    &other.stack().projection_engine
                ));
                core.run::<_, FfiError, _, _>(move |stack| async move {
                    let id = format!("item-{n}");
                    stack
                        .command_bus
                        .dispatch(Create(id.clone()), CommandContext::system())
                        .await
                        .map_err(|_| FfiError::StackInitializationFailed)?;
                    let row = stack
                        .read_model_store
                        .get("items", &id)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(row["data"], "visible");
                    Ok(())
                })
                .await
                .unwrap();
            });
        })
        .join()
        .unwrap();
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        4,
        "one synchronous subscription"
    );
    // A separate isolated host explicitly requests replay before publishing readiness.
    let replay_registry = CoreRegistry::new(runtime);
    let replays = Arc::new(AtomicUsize::new(0));
    let (counter, replay_counter) = (count.clone(), replays.clone());
    let reopened = replay_registry
        .open::<Example, _, _>(key.clone(), "v1-replay".into(), move |key| async move {
            let stack = support::initialize(key, counter.clone()).await?;
            assert_eq!(
                counter.load(Ordering::SeqCst),
                4,
                "assembly did not implicitly replay"
            );
            stack
                .projection_engine
                .rebuild_all()
                .await
                .map_err(|_| FfiError::ProjectionRebuildFailed)?;
            replay_counter.fetch_add(1, Ordering::SeqCst);
            Ok(stack)
        })
        .await
        .unwrap();
    let again = replay_registry
        .open::<Example, _, _>(key, "v1-replay".into(), |_| async {
            panic!("repeated replay")
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&reopened, &again));
    assert_eq!(replays.load(Ordering::SeqCst), 1);
    assert_eq!(count.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn cancelled_wait_commits_once_without_automatic_retry() {
    let dir = tempfile::tempdir().unwrap();
    let key = CoreKey::sqlite_file(dir.path().join("db.sqlite")).unwrap();
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let counter = Arc::new(AtomicUsize::new(0));
    let init_count = counter.clone();
    let core = registry
        .open::<Example, _, _>(key, "v1".into(), move |key| {
            support::initialize(key, init_count)
        })
        .await
        .unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let (continue_tx, continue_rx) = oneshot::channel();
    let (committed_tx, committed_rx) = oneshot::channel();
    let owner = core.clone();
    let waiter = tokio::spawn(async move {
        owner
            .run::<_, FfiError, _, _>(move |stack| async move {
                started_tx.send(()).unwrap();
                continue_rx.await.unwrap();
                stack
                    .command_bus
                    .dispatch(Create("once".into()), CommandContext::system())
                    .await
                    .unwrap();
                committed_tx.send(()).unwrap();
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    waiter.abort();
    assert!(waiter.await.err().unwrap().is_cancelled());
    continue_tx.send(()).unwrap();
    committed_rx.await.unwrap();
    let row = core
        .run::<_, FfiError, _, _>(|stack| async move {
            Ok(stack
                .read_model_store
                .get("items", "once")
                .await
                .unwrap()
                .unwrap())
        })
        .await
        .unwrap();
    assert_eq!(row["version"], 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
