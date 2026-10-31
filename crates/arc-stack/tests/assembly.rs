#![cfg(feature = "sqlite")]
use arc_core::{
    aggregate::{Aggregate, Command},
    command_bus::CommandContext,
    event::{Event, NewEvent},
    projection::{ProjectionResult, Projector},
    read_model_store::ReadModelStore,
};
use arc_stack::{config::DatabaseDriver, es_stack, ProjectorReg};
use async_trait::async_trait;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
struct Example {
    version: i64,
}
struct Create;
impl Command for Create {
    fn aggregate_id(&self) -> &str {
        "example"
    }
}
#[async_trait]
impl Aggregate for Example {
    type Command = Create;
    type Event = ();
    type Error = std::io::Error;
    fn aggregate_type() -> &'static str {
        "Example"
    }
    fn version(&self) -> i64 {
        self.version
    }
    async fn handle(&self, _: Create) -> Result<Vec<Event>, std::io::Error> {
        Ok(vec![Event::new(NewEvent {
            aggregate_type: "Example",
            aggregate_id: "example",
            sequence: self.version + 1,
            event_type: "Created",
            payload: serde_json::json!({}),
        })])
    }
    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
    }
}
struct Count(Arc<AtomicUsize>);
#[async_trait]
impl Projector for Count {
    fn name(&self) -> &str {
        "count"
    }
    fn handles(&self) -> Vec<String> {
        vec!["Created".into()]
    }
    async fn apply(&self, _: &Event, _: &dyn ReadModelStore) -> ProjectionResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[tokio::test]
async fn sqlite_explicit_and_environment_builds_subscribe_once_without_automatic_rebuild() {
    let path = std::env::temp_dir().join(format!("arc-stack-{}.sqlite", uuid::Uuid::new_v4()));
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, aggregate_type TEXT NOT NULL, aggregate_id TEXT NOT NULL, sequence BIGINT NOT NULL, event_type TEXT NOT NULL, payload TEXT NOT NULL, timestamp BIGINT NOT NULL, actor_id TEXT NOT NULL, actor_session_id TEXT, source_ip TEXT, user_agent TEXT, timestamp_utc_us BIGINT NOT NULL, causation_id TEXT, correlation_id TEXT NOT NULL, integrity_signature TEXT, integrity_key_id TEXT, UNIQUE(aggregate_type, aggregate_id, sequence))").execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE snapshots (aggregate_type TEXT NOT NULL, aggregate_id TEXT NOT NULL, version BIGINT NOT NULL, state TEXT NOT NULL, created_at BIGINT NOT NULL, PRIMARY KEY(aggregate_type, aggregate_id))").execute(&pool).await.unwrap();
    sqlx::query(
        "CREATE TABLE items (id TEXT PRIMARY KEY, data TEXT NOT NULL, version BIGINT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let path_str = path.to_str().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let stack = es_stack::build_with_driver::<Example>(
        DatabaseDriver::Sqlite,
        path_str,
        vec![ProjectorReg::new(Count(count.clone()), "items")],
        None,
    )
    .await
    .unwrap();
    stack
        .command_bus
        .dispatch(Create, CommandContext::for_actor("test"))
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(stack);
    std::env::set_var("DATABASE_DRIVER", "sqlite");
    let stack = es_stack::build::<Example>(
        path_str,
        vec![ProjectorReg::new(Count(count.clone()), "items")],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "construction must not replay events"
    );
    stack.projection_engine.rebuild_all().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    drop(stack);
    std::env::remove_var("DATABASE_DRIVER");
    let _ = std::fs::remove_file(path);
}
