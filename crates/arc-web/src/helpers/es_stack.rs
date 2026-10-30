//! Shared assembly of the event-sourced stack — `EventStore`, in-process
//! `EventBus`, `ReadModelStore`, `ProjectionEngine`, and `CommandBus`.
//!
//! Generic over the domain aggregate `A`: the framework never names a concrete
//! aggregate. Applications supply `A` and their projectors; the runtime server
//! (`commands::serve`) and CLI utilities drive the same wiring.

use crate::helpers::config;
use crate::helpers::config::DatabaseDriver;
use crate::ProjectorReg;
use arc_core::aggregate::Aggregate;
use arc_core::command_bus::{CommandBus, SnapshotPolicy};
use arc_core::event_bus::{EventBus, InProcessEventBus};
use arc_core::event_store::EventStore;
use arc_core::projection::{ProjectionEngine, ProjectionEngineHandler};
use arc_core::read_model_store::ReadModelStore;
use arc_es_sqlite::{SqliteEventStore, SqliteReadModelStore};
use std::sync::Arc;

/// Bundle of constructed components — the parts external code keeps a
/// handle on after wiring.
pub struct EsStack<A: Aggregate> {
    pub command_bus: CommandBus<A>,
    pub read_model_store: Arc<dyn ReadModelStore>,
    /// Held so callers that want to drive `rebuild_all()` can do so. CLI
    /// utilities ignore it; the runtime server keeps a clone.
    #[allow(dead_code)]
    pub projection_engine: Arc<ProjectionEngine>,
}

/// Share one constructed stack with multiple adapters. The caller owns startup
/// projection rebuilding before making the stack available to those adapters.
pub struct SharedStack<A: Aggregate> {
    /// The shared command bus.
    pub command_bus: Arc<CommandBus<A>>,
    /// The shared projection read store.
    pub read_model_store: Arc<dyn ReadModelStore>,
    /// The projection engine, including the caller-owned rebuild operation.
    pub projection_engine: Arc<ProjectionEngine>,
}

impl<A: Aggregate> Clone for SharedStack<A> {
    fn clone(&self) -> Self {
        Self {
            command_bus: self.command_bus.clone(),
            read_model_store: self.read_model_store.clone(),
            projection_engine: self.projection_engine.clone(),
        }
    }
}

impl<A: Aggregate> From<EsStack<A>> for SharedStack<A> {
    fn from(stack: EsStack<A>) -> Self {
        Self {
            command_bus: Arc::new(stack.command_bus),
            read_model_store: stack.read_model_store,
            projection_engine: stack.projection_engine,
        }
    }
}

/// Boxed storage seam shared by every entry point. The command bus and the
/// projection engine each own an independent handle to the same backend so the
/// write path and replay path do not contend on one connection wrapper.
pub struct EsStores {
    pub command_event_store: Box<dyn EventStore>,
    pub projection_event_store: Box<dyn EventStore>,
    pub read_model_store: Arc<dyn ReadModelStore>,
}

/// Construct the event store and read-model store for the selected driver.
/// Domain code never sees the concrete type — only `Box<dyn EventStore>` and
/// `Arc<dyn ReadModelStore>`.
pub async fn build_stores(
    driver: DatabaseDriver,
    database_url: &str,
) -> Result<EsStores, Box<dyn std::error::Error>> {
    match driver {
        DatabaseDriver::Sqlite => build_sqlite_stores(database_url).await,
        DatabaseDriver::Postgres => build_postgres_stores(database_url).await,
    }
}

async fn build_sqlite_stores(database_url: &str) -> Result<EsStores, Box<dyn std::error::Error>> {
    let event_store = build_sqlite_event_store(database_url).await?;
    let read_model_store: Arc<dyn ReadModelStore> =
        Arc::new(SqliteReadModelStore::new(database_url).await?);

    Ok(EsStores {
        command_event_store: Box::new(event_store.clone()),
        projection_event_store: Box::new(event_store),
        read_model_store,
    })
}

async fn build_sqlite_event_store(
    database_url: &str,
) -> Result<SqliteEventStore, Box<dyn std::error::Error>> {
    match config::event_integrity_key() {
        Some(key) => Ok(SqliteEventStore::new_with_integrity_key(
            database_url,
            key,
            config::event_integrity_key_id(),
        )
        .await?),
        None => Ok(SqliteEventStore::new(database_url).await?),
    }
}

#[cfg(feature = "postgres")]
async fn build_postgres_stores(database_url: &str) -> Result<EsStores, Box<dyn std::error::Error>> {
    use arc_es_postgres::{PostgresEventStore, PostgresReadModelStore};

    let event_store = PostgresEventStore::new(database_url).await?;
    event_store.initialize_schema().await?;

    let read_model_store_impl = PostgresReadModelStore::new(database_url).await?;
    read_model_store_impl.initialize_schema().await?;
    let read_model_store: Arc<dyn ReadModelStore> = Arc::new(read_model_store_impl);

    Ok(EsStores {
        command_event_store: Box::new(event_store.clone()),
        projection_event_store: Box::new(event_store),
        read_model_store,
    })
}

#[cfg(not(feature = "postgres"))]
async fn build_postgres_stores(
    _database_url: &str,
) -> Result<EsStores, Box<dyn std::error::Error>> {
    Err("DATABASE_DRIVER=postgres requires building with the `postgres` cargo feature".into())
}

/// Build the in-process stack for the configured driver, registering the
/// supplied projectors against the in-process bus so writes drive their views
/// synchronously. Used by CLI utilities (`migrate --seed`, `seed`) and tests.
pub async fn build<A: Aggregate>(
    database_url: &str,
    projectors: Vec<ProjectorReg>,
    snapshot_policy: Option<SnapshotPolicy>,
) -> Result<EsStack<A>, Box<dyn std::error::Error>> {
    build_with_driver(
        DatabaseDriver::from_env(),
        database_url,
        projectors,
        snapshot_policy,
    )
    .await
}

/// Build an in-process stack using an explicitly selected database driver.
/// The caller is responsible for rebuilding projections when required.
pub async fn build_with_driver<A: Aggregate>(
    driver: DatabaseDriver,
    database_url: &str,
    projectors: Vec<ProjectorReg>,
    snapshot_policy: Option<SnapshotPolicy>,
) -> Result<EsStack<A>, Box<dyn std::error::Error>> {
    let stores = build_stores(driver, database_url).await?;
    let read_model_store = stores.read_model_store.clone();

    let mut engine = ProjectionEngine::new(stores.projection_event_store);
    for reg in projectors {
        engine.register_projector(reg.projector, read_model_store.clone(), reg.view);
    }
    let engine = Arc::new(engine);

    let mut bus = InProcessEventBus::new();
    bus.subscribe(Box::new(ProjectionEngineHandler::new(engine.clone())))
        .await?;

    let command_bus = apply_snapshot_policy(
        CommandBus::<A>::new(stores.command_event_store, Box::new(bus)),
        snapshot_policy,
    );

    Ok(EsStack {
        command_bus,
        read_model_store,
        projection_engine: engine,
    })
}

/// Apply a snapshot policy to a command bus if one is configured.
pub fn apply_snapshot_policy<A: Aggregate>(
    command_bus: CommandBus<A>,
    policy: Option<SnapshotPolicy>,
) -> CommandBus<A> {
    match policy {
        Some(p) => command_bus.with_snapshot_policy(p),
        None => command_bus,
    }
}
