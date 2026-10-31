#![allow(dead_code)]
use arc_core::{
    aggregate::{Aggregate, Command},
    event::{Event, NewEvent},
    projection::{ProjectionResult, Projector},
    read_model_store::{ReadModelStore, Upsert},
};
use arc_ffi::{CoreKey, FfiError};
use arc_stack::{config::DatabaseDriver, ProjectorReg, SharedStack};
use async_trait::async_trait;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
pub struct Example {
    pub version: i64,
}
pub struct Create(pub String);
impl Command for Create {
    fn aggregate_id(&self) -> &str {
        &self.0
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
    async fn handle(&self, cmd: Create) -> Result<Vec<Event>, std::io::Error> {
        Ok(vec![Event::new(NewEvent {
            aggregate_type: "Example",
            aggregate_id: &cmd.0,
            sequence: self.version + 1,
            event_type: "Created",
            payload: serde_json::json!({"value": "visible"}),
        })])
    }
    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
    }
}

pub struct Count(pub Arc<AtomicUsize>);
#[async_trait]
impl Projector for Count {
    fn name(&self) -> &str {
        "example"
    }
    fn handles(&self) -> Vec<String> {
        vec!["Created".into()]
    }
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        store.upsert(Upsert::new("items", &event.aggregate_id, serde_json::json!({"id": event.aggregate_id, "version": event.sequence, "data": "visible"}))).await.map_err(|error| arc_core::projection::ProjectionError::ReadModelError { name: self.name().into(), message: error.to_string() })?;
        Ok(())
    }
}

pub async fn initialize(
    key: CoreKey,
    counter: Arc<AtomicUsize>,
) -> Result<SharedStack<Example>, FfiError> {
    let path = key.sqlite_path().unwrap().to_str().unwrap();
    schema(path).await?;
    arc_ffi::build_stack(
        DatabaseDriver::Sqlite,
        path,
        vec![ProjectorReg::new(Count(counter), "items")],
        None,
    )
    .await
}

pub async fn schema(path: &str) -> Result<(), FfiError> {
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .map_err(|_| FfiError::SchemaInitializationFailed)?;
    for query in [
        "CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, aggregate_type TEXT NOT NULL, aggregate_id TEXT NOT NULL, sequence BIGINT NOT NULL, event_type TEXT NOT NULL, payload TEXT NOT NULL, timestamp BIGINT NOT NULL, actor_id TEXT NOT NULL, actor_session_id TEXT, source_ip TEXT, user_agent TEXT, timestamp_utc_us BIGINT NOT NULL, causation_id TEXT, correlation_id TEXT NOT NULL, integrity_signature TEXT, integrity_key_id TEXT, UNIQUE(aggregate_type, aggregate_id, sequence))",
        "CREATE TABLE IF NOT EXISTS snapshots (aggregate_type TEXT NOT NULL, aggregate_id TEXT NOT NULL, version BIGINT NOT NULL, state TEXT NOT NULL, created_at BIGINT NOT NULL, PRIMARY KEY(aggregate_type, aggregate_id))",
        "CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY, data TEXT NOT NULL, version BIGINT NOT NULL)",
    ] {
        sqlx::query(query).execute(&pool).await.map_err(|_| FfiError::SchemaInitializationFailed)?;
    }
    pool.close().await;
    Ok(())
}
