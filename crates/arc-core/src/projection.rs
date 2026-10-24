//! # Projection Module
//!
//! Three-trait architecture for building read models from event streams:
//!
//! - **[`Projector`]** — stateless event handler (the "machine"). Contains the pure
//!   logic for transforming events into read model writes.
//! - **[`Projection`]** — composed read model unit (the "output"). Ties a projector
//!   to its storage backend.
//! - **[`ReadModelStore`]** — persistence
//!   layer for projections. Backend-agnostic storage (defined in `read_model_store` module).
//!
//! ## Design Principles
//!
//! - **Separation of concerns**: Handler logic (projector) is separate from storage
//!   (read model store) and orchestration (projection engine)
//! - **Stateless projectors**: Projectors take `&self`, not `&mut self`. All mutable
//!   state lives in the `ReadModelStore` via interior mutability.
//! - **Rebuildable**: Projections can be rebuilt from scratch by replaying events
//! - **Idempotent**: Handling the same event multiple times should be safe
//! - **Composable**: One projector per read model concern; swap backends freely
//!
//! ## Example
//!
//! ```rust,ignore
//! use arc_core::projection::{Projector, Projection, ProjectionUnit, ProjectionEngine};
//! use arc_core::read_model_store::{ReadModelStore, InMemoryReadModelStore};
//! use arc_core::event::Event;
//! use std::sync::Arc;
//!
//! struct UserListProjector;
//!
//! #[async_trait]
//! impl Projector for UserListProjector {
//!     fn name(&self) -> &str { "UserList" }
//!
//!     fn handles(&self) -> Vec<String> {
//!         vec!["UserCreated".to_string(), "ProfileUpdated".to_string()]
//!     }
//!
//!     async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
//!         match event.event_type.as_str() {
//!             "UserCreated" => {
//!                 store.upsert(Upsert::new("users_view", &event.aggregate_id, event.payload.clone())).await
//!                     .map_err(|e| ProjectionError::handle_failed("UserList", &event.event_type, &event.event_id.to_string(), e.to_string()))?;
//!             }
//!             _ => {}
//!         }
//!         Ok(())
//!     }
//! }
//!
//! // Compose: projector + store = projection
//! let store = Arc::new(InMemoryReadModelStore::new());
//! let projection = ProjectionUnit::new(Box::new(UserListProjector), store, "users_view");
//!
//! // Register with engine
//! let mut engine = ProjectionEngine::new(event_store);
//! engine.register(Box::new(projection));
//! engine.process(&event).await?;
//! ```

use crate::event::Event;
#[cfg(test)]
use crate::event::NewEvent;
use crate::event_bus::EventHandler;
use crate::event_store::EventStore;
use crate::read_model_store::ReadModelStore;
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during projection operations.
#[derive(Debug, Error)]
pub enum ProjectionError {
    /// Error handling an event
    #[error(
        "Projection '{name}' failed to handle event {event_type} (event_id: {event_id}): {message}"
    )]
    HandleFailed {
        name: String,
        event_type: String,
        event_id: String,
        message: String,
    },

    /// Error clearing projection state
    #[error("Projection '{name}' failed to clear: {message}")]
    ClearFailed { name: String, message: String },

    /// Error rebuilding projection
    #[error("Projection '{name}' failed to rebuild: {message}")]
    RebuildFailed { name: String, message: String },

    /// Event store error during rebuild
    #[error("Failed to load events for rebuild: {0}")]
    EventStoreError(String),

    /// Read model store error
    #[error("Read model store error in projection '{name}': {message}")]
    ReadModelError { name: String, message: String },

    /// Other errors
    #[error("Projection error: {message}")]
    Other { message: String },
}

impl ProjectionError {
    /// Create a handle failed error.
    pub fn handle_failed(
        name: impl Into<String>,
        event_type: impl Into<String>,
        event_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ProjectionError::HandleFailed {
            name: name.into(),
            event_type: event_type.into(),
            event_id: event_id.into(),
            message: message.into(),
        }
    }

    /// Create a clear failed error.
    pub fn clear_failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        ProjectionError::ClearFailed {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Create a rebuild failed error.
    pub fn rebuild_failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        ProjectionError::RebuildFailed {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Create a read model error.
    pub fn read_model_error(name: impl Into<String>, message: impl Into<String>) -> Self {
        ProjectionError::ReadModelError {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Create a generic error.
    pub fn other(message: impl Into<String>) -> Self {
        ProjectionError::Other {
            message: message.into(),
        }
    }
}

/// Result type for projection operations.
pub type ProjectionResult<T> = Result<T, ProjectionError>;

/// Failure reasons for events received through an external projection
/// delivery boundary.
#[derive(Debug, Error)]
pub enum ExternalProjectionError {
    /// The event identity does not exist in the authoritative event store.
    #[error("event is not present in the authoritative event store")]
    MissingEvent,

    /// The submitted event differs from the event persisted under its identity.
    #[error("submitted event differs from the authoritative persisted event")]
    EventMismatch,

    /// The store could not verify or retrieve the authoritative event.
    #[error("authoritative event lookup failed: {0}")]
    EventStore(String),

    /// A registered projection failed after the stored event was verified.
    #[error("projection failed: {0}")]
    Projection(String),
}

// ---------------------------------------------------------------------------
// Projector trait — the stateless event handler
// ---------------------------------------------------------------------------

/// A projector contains the pure event-handling logic for building a read model.
///
/// Projectors are stateless — they receive events and translate them into write
/// operations against a [`ReadModelStore`]. They do not own the store or the
/// read model state.
///
/// # Design
///
/// - **Stateless**: all state lives in the `ReadModelStore`
/// - **Deterministic**: same events + empty store = same read model
/// - **Composable**: one projector per read model concern
/// - **`&self`**: safe to share across threads
///
/// # Idempotency
///
/// `apply()` should be idempotent — handling the same event twice must produce
/// the same result. Use UPSERT, check event_id, or make operations naturally
/// idempotent (SET vs INCREMENT).
///
/// # Example
///
/// ```rust,ignore
/// struct UserListProjector;
///
/// #[async_trait]
/// impl Projector for UserListProjector {
///     fn name(&self) -> &str { "UserList" }
///
///     fn handles(&self) -> Vec<String> {
///         vec!["UserCreated".to_string()]
///     }
///
///     async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
///         store.upsert(Upsert::new("users_view", &event.aggregate_id, event.payload.clone())).await
///             .map_err(|e| ProjectionError::handle_failed(
///                 "UserList", &event.event_type, &event.event_id.to_string(), e.to_string()
///             ))?;
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Projector: Send + Sync {
    /// Unique name identifying this projector.
    ///
    /// Used for logging, monitoring, position tracking, and rebuild targeting.
    fn name(&self) -> &str;

    /// Event types this projector handles.
    ///
    /// Only events whose `event_type` is in this list will be passed to `apply()`.
    fn handles(&self) -> Vec<String>;

    /// Apply a single event to the read model via the store.
    ///
    /// This method should be idempotent: applying the same event twice
    /// must produce the same result.
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()>;

    /// Initialize the read model schema (CREATE TABLE IF NOT EXISTS, etc.).
    ///
    /// Called once when the projector is first registered and before rebuilds.
    /// Default implementation does nothing (for stores that don't need schema setup).
    async fn init(&self, _store: &dyn ReadModelStore) -> ProjectionResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Projection trait — the composed read model unit
// ---------------------------------------------------------------------------

/// A projection is the composed unit of a projector + its read model store.
///
/// It represents a complete, self-contained read model: the logic that transforms
/// events into state, paired with the storage where that state lives.
///
/// Most users don't implement this trait directly. Instead, implement [`Projector`]
/// and compose it with a [`ReadModelStore`] via [`ProjectionUnit`].
///
/// # `&self` not `&mut self`
///
/// All methods take `&self`. Mutable state lives in the `ReadModelStore`, which
/// handles interior mutability via connection pools, `Mutex`, etc.
#[async_trait]
pub trait Projection: Send + Sync {
    /// Projection name (delegates to the projector).
    fn name(&self) -> &str;

    /// Event types this projection handles (delegates to the projector).
    fn handles(&self) -> Vec<String>;

    /// Handle a single event by applying it through the projector to the store.
    async fn handle(&self, event: &Event) -> ProjectionResult<()>;

    /// Clear all read model state for this projection.
    async fn clear(&self) -> ProjectionResult<()>;

    /// Rebuild from a set of events: clear, then replay matching events.
    async fn rebuild(&self, events: Vec<Event>) -> ProjectionResult<()> {
        self.clear().await?;
        for event in events {
            if self.handles().contains(&event.event_type) {
                self.handle(&event).await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ProjectionUnit — standard composition glue
// ---------------------------------------------------------------------------

/// Standard composition of a [`Projector`] and a [`ReadModelStore`].
///
/// This is the typical way to create a [`Projection`]: provide the event-handling
/// logic (projector) and the storage backend (store), and `ProjectionUnit` wires
/// them together.
///
/// # Example
///
/// ```rust,ignore
/// let projector = Box::new(UserListProjector);
/// let store: Arc<dyn ReadModelStore> = Arc::new(SqliteReadModelStore::new(pool));
/// let projection = ProjectionUnit::new(projector, store, "users_view");
/// engine.register(Box::new(projection));
/// ```
pub struct ProjectionUnit {
    projector: Box<dyn Projector>,
    store: Arc<dyn ReadModelStore>,
    /// Table/collection name used for `clear()` (truncate target).
    table: String,
}

impl ProjectionUnit {
    /// Create a new projection unit.
    ///
    /// # Arguments
    ///
    /// - `projector`: The stateless event handler
    /// - `store`: The read model storage backend
    /// - `table`: Table/collection name to truncate on `clear()`
    pub fn new(
        projector: Box<dyn Projector>,
        store: Arc<dyn ReadModelStore>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            projector,
            store,
            table: table.into(),
        }
    }
}

#[async_trait]
impl Projection for ProjectionUnit {
    fn name(&self) -> &str {
        self.projector.name()
    }

    fn handles(&self) -> Vec<String> {
        self.projector.handles()
    }

    async fn handle(&self, event: &Event) -> ProjectionResult<()> {
        self.projector.apply(event, self.store.as_ref()).await
    }

    async fn clear(&self) -> ProjectionResult<()> {
        self.store
            .truncate(&self.table)
            .await
            .map_err(|e| ProjectionError::clear_failed(self.projector.name(), e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// ProjectionEngine — orchestrates multiple projections
// ---------------------------------------------------------------------------

/// Engine for managing multiple projections.
///
/// The `ProjectionEngine`:
/// - Registers fully composed [`Projection`] instances
/// - Routes events to interested projections
/// - Rebuilds projections from the event store
/// - Provides convenience registration via [`register_projector`](Self::register_projector)
///
/// # Example
///
/// ```rust,ignore
/// let event_store = Box::new(sqlite_event_store);
/// let mut engine = ProjectionEngine::new(event_store);
///
/// // Option 1: register a pre-composed projection
/// engine.register(Box::new(projection_unit));
///
/// // Option 2: convenience — register projector + store directly
/// engine.register_projector(Box::new(UserListProjector), store.clone(), "users_view");
///
/// // Process events
/// engine.process(&event).await?;
///
/// // Rebuild all projections from event store
/// engine.rebuild_all().await?;
/// ```
pub struct ProjectionEngine {
    projections: Vec<Box<dyn Projection>>,
    event_store: Box<dyn EventStore>,
}

impl ProjectionEngine {
    /// Create a new projection engine.
    pub fn new(event_store: Box<dyn EventStore>) -> Self {
        Self {
            projections: Vec::new(),
            event_store,
        }
    }

    /// Register a fully composed projection.
    pub fn register(&mut self, projection: Box<dyn Projection>) {
        tracing::info!("Registering projection: {}", projection.name());
        self.projections.push(projection);
    }

    /// Convenience: register a projector + store as a [`ProjectionUnit`].
    pub fn register_projector(
        &mut self,
        projector: Box<dyn Projector>,
        store: Arc<dyn ReadModelStore>,
        table: impl Into<String>,
    ) {
        let unit = ProjectionUnit::new(projector, store, table);
        self.register(Box::new(unit));
    }

    /// Process a single event through all interested projections.
    ///
    /// Routes the event to projections whose `handles()` includes the event type.
    pub async fn process(&self, event: &Event) -> ProjectionResult<()> {
        for projection in &self.projections {
            if projection.handles().contains(&event.event_type) {
                tracing::debug!(
                    "Processing event {} ({}) in projection {}",
                    event.event_type,
                    event.event_id,
                    projection.name()
                );

                projection.handle(event).await.map_err(|e| {
                    ProjectionError::handle_failed(
                        projection.name(),
                        &event.event_type,
                        event.event_id.to_string(),
                        e.to_string(),
                    )
                })?;
            }
        }
        Ok(())
    }

    /// Validate an externally delivered event against the exact persisted
    /// stream event, then project the authoritative stored value.
    ///
    /// Unlike [`Self::process`], this method is intended for untrusted delivery
    /// boundaries. Stores without bounded `load_event` support fail closed.
    pub async fn process_external_delivery(
        &self,
        submitted: &Event,
    ) -> Result<(), ExternalProjectionError> {
        let stored = self
            .event_store
            .load_event(
                &submitted.aggregate_type,
                &submitted.aggregate_id,
                submitted.sequence,
            )
            .await
            .map_err(|error| ExternalProjectionError::EventStore(error.to_string()))?
            .ok_or(ExternalProjectionError::MissingEvent)?;

        if stored != *submitted {
            return Err(ExternalProjectionError::EventMismatch);
        }

        self.process(&stored)
            .await
            .map_err(|error| ExternalProjectionError::Projection(error.to_string()))
    }

    /// Process multiple events in sequence.
    pub async fn process_batch(&self, events: Vec<Event>) -> ProjectionResult<()> {
        for event in events {
            self.process(&event).await?;
        }
        Ok(())
    }

    /// Rebuild all registered projections from the event store.
    pub async fn rebuild_all(&self) -> ProjectionResult<()> {
        tracing::info!("Rebuilding all projections");

        let events = self
            .event_store
            .stream_all(0)
            .await
            .map_err(|e| ProjectionError::EventStoreError(e.to_string()))?;

        tracing::info!("Loaded {} events for rebuild", events.len());

        for projection in &self.projections {
            tracing::info!("Rebuilding projection: {}", projection.name());

            projection
                .rebuild(events.clone())
                .await
                .map_err(|e| ProjectionError::rebuild_failed(projection.name(), e.to_string()))?;

            tracing::info!("Rebuilt projection: {}", projection.name());
        }

        Ok(())
    }

    /// Rebuild a specific projection by name.
    pub async fn rebuild_projection(&self, name: &str) -> ProjectionResult<()> {
        tracing::info!("Rebuilding projection: {}", name);

        let projection = self
            .projections
            .iter()
            .find(|p| p.name() == name)
            .ok_or_else(|| ProjectionError::other(format!("Projection not found: {}", name)))?;

        let events = self
            .event_store
            .stream_all(0)
            .await
            .map_err(|e| ProjectionError::EventStoreError(e.to_string()))?;

        projection
            .rebuild(events)
            .await
            .map_err(|e| ProjectionError::rebuild_failed(name, e.to_string()))?;

        tracing::info!("Rebuilt projection: {}", name);
        Ok(())
    }

    /// Get number of registered projections.
    pub fn projection_count(&self) -> usize {
        self.projections.len()
    }

    /// Get names of all registered projections.
    pub fn projection_names(&self) -> Vec<String> {
        self.projections
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    /// Union of every event type any registered projection handles. Used by
    /// [`ProjectionEngineHandler`] to declare its `handles()` set when
    /// subscribing to an [`EventBus`](crate::event_bus::EventBus).
    pub fn all_handled_event_types(&self) -> Vec<String> {
        let mut all: Vec<String> = self.projections.iter().flat_map(|p| p.handles()).collect();
        all.sort();
        all.dedup();
        all
    }
}

// ---------------------------------------------------------------------------
// EventBus adapter — drive the engine from an in-process bus
// ---------------------------------------------------------------------------

/// Adapter that lets a [`ProjectionEngine`] subscribe to an
/// [`EventBus`](crate::event_bus::EventBus). Wraps the engine in an
/// [`EventHandler`] that routes every relevant event through
/// [`ProjectionEngine::process`]. The engine stays accessible from outside
/// (e.g. for `rebuild_all`) via the same [`Arc`].
///
/// Lives in `arc-core` because the adapter needs nothing app-specific —
/// any aggregate's projector can be driven through it.
pub struct ProjectionEngineHandler {
    engine: Arc<ProjectionEngine>,
    handles: Vec<String>,
}

impl ProjectionEngineHandler {
    pub fn new(engine: Arc<ProjectionEngine>) -> Self {
        let handles = engine.all_handled_event_types();
        Self { engine, handles }
    }
}

#[async_trait]
impl EventHandler for ProjectionEngineHandler {
    fn handles(&self) -> Vec<String> {
        self.handles.clone()
    }

    async fn handle(&self, event: &Event) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.engine
            .process(event)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::{EventStore, EventStoreResult, VersionCheck};
    use crate::read_model_store::InMemoryReadModelStore;
    use std::sync::{Arc, Mutex};

    // -----------------------------------------------------------------------
    // Mock event store (unchanged — needed for ProjectionEngine)
    // -----------------------------------------------------------------------

    struct MockEventStore {
        events: Arc<Mutex<Vec<Event>>>,
    }

    impl MockEventStore {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn add_event(&self, event: Event) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[async_trait]
    impl EventStore for MockEventStore {
        async fn append(
            &self,
            _aggregate_id: &str,
            _version_check: VersionCheck,
            events: Vec<Event>,
        ) -> EventStoreResult<()> {
            self.events.lock().unwrap().extend(events);
            Ok(())
        }

        async fn load(&self, aggregate_id: &str) -> EventStoreResult<Vec<Event>> {
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .cloned()
                .collect())
        }

        async fn load_from(
            &self,
            aggregate_id: &str,
            from_sequence: i64,
        ) -> EventStoreResult<Vec<Event>> {
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id && e.sequence >= from_sequence)
                .cloned()
                .collect())
        }

        async fn stream_all(&self, _from_position: i64) -> EventStoreResult<Vec<Event>> {
            Ok(self.events.lock().unwrap().clone())
        }

        async fn get_version(&self, aggregate_id: &str) -> EventStoreResult<i64> {
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .map(|e| e.sequence)
                .max()
                .unwrap_or(0))
        }
    }

    // -----------------------------------------------------------------------
    // Mock projector — stateless, writes to ReadModelStore
    // -----------------------------------------------------------------------

    struct MockProjector {
        name: String,
        handles_types: Vec<String>,
    }

    impl MockProjector {
        fn new(name: &str, handles: Vec<String>) -> Self {
            Self {
                name: name.to_string(),
                handles_types: handles,
            }
        }
    }

    #[async_trait]
    impl Projector for MockProjector {
        fn name(&self) -> &str {
            &self.name
        }

        fn handles(&self) -> Vec<String> {
            self.handles_types.clone()
        }

        async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
            use crate::read_model_store::Upsert;
            store
                .upsert(Upsert::new(
                    "test_table",
                    event.event_id.to_string(),
                    serde_json::json!({
                        "id": event.event_id.to_string(),
                        "event_type": event.event_type,
                        "version": event.sequence,
                    }),
                ))
                .await
                .map_err(|e| {
                    ProjectionError::handle_failed(
                        &self.name,
                        &event.event_type,
                        event.event_id.to_string(),
                        e.to_string(),
                    )
                })?;
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Helper to build a projection from mock projector + in-memory store
    // -----------------------------------------------------------------------

    fn make_projection(
        name: &str,
        handles: Vec<String>,
        store: Arc<InMemoryReadModelStore>,
    ) -> Box<ProjectionUnit> {
        Box::new(ProjectionUnit::new(
            Box::new(MockProjector::new(name, handles)),
            store,
            "test_table",
        ))
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_projection_engine_new() {
        let store = Box::new(MockEventStore::new());
        let engine = ProjectionEngine::new(store);
        assert_eq!(engine.projection_count(), 0);
    }

    #[tokio::test]
    async fn test_register_projection() {
        let store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(store);

        let rm_store = Arc::new(InMemoryReadModelStore::new());
        let projection = make_projection("Test", vec!["TestEvent".to_string()], rm_store);
        engine.register(projection);

        assert_eq!(engine.projection_count(), 1);
        assert_eq!(engine.projection_names(), vec!["Test"]);
    }

    #[tokio::test]
    async fn test_process_event() {
        let store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(store);

        let rm_store = Arc::new(InMemoryReadModelStore::new());
        let projection = make_projection("Test", vec!["UserCreated".to_string()], rm_store.clone());
        engine.register(projection);

        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: serde_json::json!({"name": "Alice"}),
        });

        engine.process(&event).await.unwrap();

        assert_eq!(rm_store.get_rows("test_table").len(), 1);
    }

    #[tokio::test]
    async fn test_projection_filtering() {
        let store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(store);

        let rm_store = Arc::new(InMemoryReadModelStore::new());
        let projection = make_projection("Test", vec!["UserCreated".to_string()], rm_store.clone());
        engine.register(projection);

        // Event that should be handled
        let event1 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: serde_json::json!({}),
        });
        engine.process(&event1).await.unwrap();

        // Event that should be filtered out
        let event2 = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 2,
            event_type: "UserDeleted",
            payload: serde_json::json!({}),
        });
        engine.process(&event2).await.unwrap();

        assert_eq!(rm_store.get_rows("test_table").len(), 1);
    }

    #[tokio::test]
    async fn test_rebuild_all() {
        let event_store = MockEventStore::new();

        event_store.add_event(Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: serde_json::json!({}),
        }));
        event_store.add_event(Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-2",
            sequence: 1,
            event_type: "UserCreated",
            payload: serde_json::json!({}),
        }));

        let mut engine = ProjectionEngine::new(Box::new(event_store));

        let rm_store = Arc::new(InMemoryReadModelStore::new());
        let projection = make_projection("Test", vec!["UserCreated".to_string()], rm_store.clone());
        engine.register(projection);

        engine.rebuild_all().await.unwrap();

        assert_eq!(rm_store.get_rows("test_table").len(), 2);
    }

    #[tokio::test]
    async fn test_multiple_projections() {
        let store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(store);

        let rm_store1 = Arc::new(InMemoryReadModelStore::new());
        let rm_store2 = Arc::new(InMemoryReadModelStore::new());

        let proj1 = make_projection(
            "Projection1",
            vec!["UserCreated".to_string()],
            rm_store1.clone(),
        );
        let proj2 = make_projection(
            "Projection2",
            vec!["UserCreated".to_string(), "UserDeleted".to_string()],
            rm_store2.clone(),
        );

        engine.register(proj1);
        engine.register(proj2);

        let event = Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-1",
            sequence: 1,
            event_type: "UserCreated",
            payload: serde_json::json!({}),
        });
        engine.process(&event).await.unwrap();

        assert_eq!(rm_store1.get_rows("test_table").len(), 1);
        assert_eq!(rm_store2.get_rows("test_table").len(), 1);
    }

    #[tokio::test]
    async fn test_process_batch() {
        let store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(store);

        let rm_store = Arc::new(InMemoryReadModelStore::new());
        let projection = make_projection("Test", vec!["UserCreated".to_string()], rm_store.clone());
        engine.register(projection);

        let events = vec![
            Event::new(NewEvent {
                aggregate_type: "User",
                aggregate_id: "user-1",
                sequence: 1,
                event_type: "UserCreated",
                payload: serde_json::json!({}),
            }),
            Event::new(NewEvent {
                aggregate_type: "User",
                aggregate_id: "user-2",
                sequence: 1,
                event_type: "UserCreated",
                payload: serde_json::json!({}),
            }),
            Event::new(NewEvent {
                aggregate_type: "User",
                aggregate_id: "user-3",
                sequence: 1,
                event_type: "UserCreated",
                payload: serde_json::json!({}),
            }),
        ];

        engine.process_batch(events).await.unwrap();

        assert_eq!(rm_store.get_rows("test_table").len(), 3);
    }

    #[tokio::test]
    async fn test_projector_init_default() {
        let projector = MockProjector::new("Test", vec![]);
        let store = InMemoryReadModelStore::new();
        // Default init() should succeed (no-op)
        projector.init(&store).await.unwrap();
    }

    #[tokio::test]
    async fn test_register_projector_convenience() {
        let event_store = Box::new(MockEventStore::new());
        let mut engine = ProjectionEngine::new(event_store);

        let rm_store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
        engine.register_projector(
            Box::new(MockProjector::new("Convenient", vec!["X".to_string()])),
            rm_store,
            "my_table",
        );

        assert_eq!(engine.projection_count(), 1);
        assert_eq!(engine.projection_names(), vec!["Convenient"]);
    }
}
