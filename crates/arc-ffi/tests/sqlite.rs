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
    const WORKERS: usize = 4;
    const PER_WORKER: usize = 30;
    let start = Arc::new(tokio::sync::Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for n in 0..WORKERS {
        let start = start.clone();
        let (core, registry, key) = (core.clone(), registry.clone(), key.clone());
        workers.push(std::thread::spawn(move || {
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
                core.run::<_, Box<dyn std::error::Error + Send + Sync>, _, _>(
                    move |stack| async move {
                        start.wait().await;
                        for item in 0..PER_WORKER {
                            let id = format!("item-{n}-{item}");
                            stack
                                .command_bus
                                .dispatch(Create(id.clone()), CommandContext::system())
                                .await?;
                            let row = stack
                                .read_model_store
                                .get("items", &id)
                                .await
                                .unwrap()
                                .unwrap();
                            assert_eq!(row["data"], "visible");
                        }
                        Ok(())
                    },
                )
                .await
                .unwrap();
            });
        }));
    }
    // Start every worker before joining; preserve raw dispatch/persistence errors.
    let outcomes: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    for outcome in outcomes {
        outcome.unwrap();
    }
    let db = sqlx::SqlitePool::connect(key.sqlite_path().unwrap().to_str().unwrap())
        .await
        .unwrap();
    for table in ["events", "items"] {
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(total, (WORKERS * PER_WORKER) as i64);
    }
    db.close().await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        WORKERS * PER_WORKER,
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
                WORKERS * PER_WORKER,
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
    assert_eq!(count.load(Ordering::SeqCst), 2 * WORKERS * PER_WORKER);
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

#[tokio::test]
async fn post_commit_projection_lock_failure_requires_replay_without_redispatch() {
    use arc_core::{
        command_bus::CommandBusError,
        event::Event,
        projection::{ProjectionError, ProjectionResult, Projector},
        read_model_store::{ReadModelStore, Upsert},
    };
    use arc_stack::{config::DatabaseDriver, ProjectorReg};
    use std::sync::atomic::AtomicBool;

    struct LockedProjection {
        external: sqlx::SqlitePool,
        once: AtomicBool,
    }
    #[async_trait::async_trait]
    impl Projector for LockedProjection {
        fn name(&self) -> &str {
            "locked-projection"
        }
        fn handles(&self) -> Vec<String> {
            vec!["Created".into()]
        }
        async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
            // This callback runs only after append committed. Hold an independent
            // writer lock until the real projection store exhausts its timeout.
            let mut external = self.external.acquire().await.unwrap();
            let lock = self.once.swap(false, Ordering::SeqCst);
            if lock {
                sqlx::query("BEGIN IMMEDIATE")
                    .execute(&mut *external)
                    .await
                    .unwrap();
            }
            let outcome = store.upsert(Upsert::new("items", &event.aggregate_id,
                serde_json::json!({"id": event.aggregate_id, "version": event.sequence, "data": "visible"}))).await;
            if lock {
                sqlx::query("ROLLBACK")
                    .execute(&mut *external)
                    .await
                    .unwrap();
            }
            outcome.map_err(|error| ProjectionError::ReadModelError {
                name: self.name().into(),
                message: error.to_string(),
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let key = CoreKey::sqlite_file(dir.path().join("db.sqlite")).unwrap();
    support::schema(key.sqlite_path().unwrap().to_str().unwrap())
        .await
        .unwrap();
    let db = sqlx::SqlitePool::connect(key.sqlite_path().unwrap().to_str().unwrap())
        .await
        .unwrap();
    let registry = CoreRegistry::new(FfiRuntime::new().unwrap());
    let external = db.clone();
    let core = registry
        .open::<Example, _, _>(key, "lock-fixture".into(), move |key| async move {
            arc_ffi::build_stack(
                DatabaseDriver::Sqlite,
                key.sqlite_path().unwrap().to_str().unwrap(),
                vec![ProjectorReg::new(
                    LockedProjection {
                        external,
                        once: AtomicBool::new(true),
                    },
                    "items",
                )],
                None,
            )
            .await
        })
        .await
        .unwrap();
    let error = core
        .run::<_, Box<dyn std::error::Error + Send + Sync>, _, _>(|stack| async move {
            stack
                .command_bus
                .dispatch(Create("once".into()), CommandContext::system())
                .await?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CommandBusError>(),
            Some(CommandBusError::PublishFailed { .. })
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("locked"), "{error:?}");
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&db)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        (events, rows),
        (1, 0),
        "append is durable despite publish error"
    );
    core.run::<_, Box<dyn std::error::Error + Send + Sync>, _, _>(|stack| async move {
        stack.projection_engine.rebuild_all().await?;
        assert_eq!(
            stack.read_model_store.get("items", "once").await?.unwrap()["version"],
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(events, 1, "replay never redispatches a committed command");
    db.close().await;
}
