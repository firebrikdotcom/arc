use actix_web::{web, HttpResponse};
use arc_core::{
    aggregate::{Aggregate, Command},
    command_bus::{CommandBus, CommandContext, SnapshotPolicy},
    event::{Event, NewEvent},
    projection::{ProjectionError, ProjectionResult, Projector},
    read_model_store::{ReadModelStore, Upsert},
};
use arc_web::{
    helpers::{
        config::DatabaseDriver,
        es_stack::{self, SharedStack},
    },
    ArcApp, ProjectorReg,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "support/env_guard.rs"]
mod env_guard;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct Item {
    version: i64,
}
struct Create(String);
impl Command for Create {
    fn aggregate_id(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, thiserror::Error)]
#[error("item error")]
struct ItemError;
#[async_trait]
impl Aggregate for Item {
    type Command = Create;
    type Event = ();
    type Error = ItemError;
    fn aggregate_type() -> &'static str {
        "Item"
    }
    fn version(&self) -> i64 {
        self.version
    }
    async fn handle(&self, command: Create) -> Result<Vec<Event>, ItemError> {
        Ok(vec![Event::new(NewEvent {
            aggregate_type: "Item",
            aggregate_id: command.0,
            sequence: self.version + 1,
            event_type: "Created",
            payload: json!({}),
        })])
    }
    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
    }
}
struct Items;
#[async_trait]
impl Projector for Items {
    fn name(&self) -> &str {
        "items"
    }
    fn handles(&self) -> Vec<String> {
        vec!["Created".into()]
    }
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        store
            .upsert(Upsert::new(
                "items",
                &event.aggregate_id,
                json!({"id":event.aggregate_id,"version":event.sequence}),
            ))
            .await
            .map_err(|e| ProjectionError::other(e.to_string()))
    }
}
async fn read(id: web::Path<String>, store: web::Data<dyn ReadModelStore>) -> HttpResponse {
    match store.get("items", &id).await.unwrap() {
        Some(row) => HttpResponse::Ok().json(row),
        None => HttpResponse::NotFound().finish(),
    }
}
async fn write(id: web::Path<String>, bus: web::Data<CommandBus<Item>>) -> HttpResponse {
    bus.dispatch(Create(id.into_inner()), CommandContext::for_actor("test"))
        .await
        .unwrap();
    HttpResponse::Created().finish()
}
async fn request(port: u16, method: &str, id: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream.write_all(format!("{method} /items/{id} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

async fn create_stack(path: &std::path::Path) -> SharedStack<Item> {
    let url = path.to_str().unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
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

    let stack: SharedStack<Item> = es_stack::build_with_driver(
        DatabaseDriver::Sqlite,
        url,
        vec![ProjectorReg::new(Items, "items")],
        None,
    )
    .await
    .unwrap()
    .into();
    stack.projection_engine.rebuild_all().await.unwrap();
    stack
}

#[actix_web::test]
async fn shared_stack_observes_writes_from_both_sides() {
    let _guard = ENV_LOCK.lock().await;
    let path = std::env::temp_dir().join(format!("arc-shared-{}.sqlite", uuid::Uuid::new_v4()));
    let url = path.to_str().unwrap();
    let stack = create_stack(&path).await;
    std::env::set_var("DATABASE_URL", url);
    std::env::set_var(
        "SECRET_KEY",
        "0123456789012345678901234567890123456789012345678901234567890123",
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let builder = ArcApp::builder()
        .register_aggregate_with_stack(stack.clone())
        .register_routes(|cfg| {
            cfg.route("/items/{id}", web::get().to(read))
                .route("/items/{id}", web::post().to(write));
        });
    let server = actix_web::rt::spawn(async move { builder.serve("127.0.0.1".into(), port).await });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    stack
        .command_bus
        .dispatch(Create("outside".into()), CommandContext::for_actor("test"))
        .await
        .unwrap();
    let response = request(port, "GET", "outside").await;
    assert!(
        response.contains("200 OK") && response.contains("outside"),
        "{response}"
    );
    let response = request(port, "POST", "inside").await;
    assert!(response.contains("201 Created"), "{response}");
    assert!(stack
        .read_model_store
        .get("items", "inside")
        .await
        .unwrap()
        .is_some());
    server.abort();
    drop(stack);
    let _ = std::fs::remove_file(path);
}
#[test]
#[should_panic(
    expected = "projectors and snapshot policy cannot be configured on a supplied stack"
)]
fn supplied_stack_rejects_projectors() {
    let store = Arc::new(arc_core::read_model_store::InMemoryReadModelStore::new());
    let event_store = arc_core::event_store::InMemoryEventStore::new();
    let stack = SharedStack {
        command_bus: Arc::new(CommandBus::<Item>::new(
            Box::new(event_store.clone()),
            Box::new(arc_core::event_bus::InProcessEventBus::new()),
        )),
        read_model_store: store,
        projection_engine: Arc::new(arc_core::projection::ProjectionEngine::new(Box::new(
            event_store,
        ))),
    };
    let _ = ArcApp::builder()
        .register_aggregate_with_stack(stack)
        .register_projectors(vec![]);
}

#[test]
#[should_panic(
    expected = "projectors and snapshot policy cannot be configured on a supplied stack"
)]
fn supplied_stack_rejects_snapshot_policy() {
    let store = Arc::new(arc_core::read_model_store::InMemoryReadModelStore::new());
    let event_store = arc_core::event_store::InMemoryEventStore::new();
    let stack = SharedStack {
        command_bus: Arc::new(CommandBus::<Item>::new(
            Box::new(event_store.clone()),
            Box::new(arc_core::event_bus::InProcessEventBus::new()),
        )),
        read_model_store: store,
        projection_engine: Arc::new(arc_core::projection::ProjectionEngine::new(Box::new(
            event_store,
        ))),
    };
    let _ = ArcApp::builder()
        .register_aggregate_with_stack(stack)
        .snapshot_policy(Some(SnapshotPolicy::EveryNEvents(1)));
}

#[test]
#[should_panic(
    expected = "projectors and snapshot policy cannot be configured on a supplied stack"
)]
fn supplied_stack_rejects_empty_snapshot_policy() {
    let store = Arc::new(arc_core::read_model_store::InMemoryReadModelStore::new());
    let event_store = arc_core::event_store::InMemoryEventStore::new();
    let stack = SharedStack {
        command_bus: Arc::new(CommandBus::<Item>::new(
            Box::new(event_store.clone()),
            Box::new(arc_core::event_bus::InProcessEventBus::new()),
        )),
        read_model_store: store,
        projection_engine: Arc::new(arc_core::projection::ProjectionEngine::new(Box::new(
            event_store,
        ))),
    };
    let _ = ArcApp::builder()
        .register_aggregate_with_stack(stack)
        .snapshot_policy(None);
}

#[actix_web::test]
async fn shared_stack_clones_share_bus_and_store() {
    let path =
        std::env::temp_dir().join(format!("arc-shared-clone-{}.sqlite", uuid::Uuid::new_v4()));
    let stack = create_stack(&path).await;
    let other = stack.clone();

    stack
        .command_bus
        .dispatch(
            Create("from-clone-a".into()),
            CommandContext::for_actor("test"),
        )
        .await
        .unwrap();

    assert!(other
        .read_model_store
        .get("items", "from-clone-a")
        .await
        .unwrap()
        .is_some());

    drop(stack);
    drop(other);
    let _ = std::fs::remove_file(path);
}

#[actix_web::test]
async fn build_with_driver_uses_explicit_driver_even_if_env_differs() {
    let _guard = ENV_LOCK.lock().await;
    let prev_driver = std::env::var_os("DATABASE_DRIVER");
    std::env::set_var("DATABASE_DRIVER", "postgres");

    let path =
        std::env::temp_dir().join(format!("arc-shared-driver-{}.sqlite", uuid::Uuid::new_v4()));
    let stack = create_stack(&path).await;
    drop(stack);

    if let Some(value) = prev_driver {
        std::env::set_var("DATABASE_DRIVER", value);
    } else {
        std::env::remove_var("DATABASE_DRIVER");
    }
    let _ = std::fs::remove_file(path);
}

#[actix_web::test]
async fn supplied_stack_rejects_non_in_process_event_bus() {
    let _guard = ENV_LOCK.lock().await;
    let path =
        std::env::temp_dir().join(format!("arc-shared-nats-{}.sqlite", uuid::Uuid::new_v4()));
    let stack = create_stack(&path).await;
    let _env = env_guard::EnvGuard::new(&[
        ("EVENT_BUS", Some("nats")),
        ("DATABASE_URL", path.to_str()),
        (
            "SECRET_KEY",
            Some("0123456789012345678901234567890123456789012345678901234567890123"),
        ),
    ]);
    let error = ArcApp::builder()
        .register_aggregate_with_stack(stack)
        .register_routes(|cfg| {
            cfg.route("/items/{id}", web::get().to(read));
        })
        .serve("127.0.0.1".into(), 0)
        .await
        .expect_err("a supplied stack must not start under EVENT_BUS=nats");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    let message = error.to_string();
    assert!(
        message.contains("aggregate Item") && message.contains("EVENT_BUS"),
        "{message}"
    );
    let _ = std::fs::remove_file(path);
}
