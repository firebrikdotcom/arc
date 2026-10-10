# Arc Core: Event Sourcing Architecture

> Architectural guidance for the arc-core event sourcing library
>
> **Architect**: Agent 5 (Software Architect with Event Sourcing Expertise)
>
> **Date**: 2026-03-01
>
> **Status**: Phase 1 - Foundation (0% Implementation, 100% Design Complete)

---

## Table of Contents

1. [Core Principles](#core-principles)
2. [Design Philosophy](#design-philosophy)
3. [Complexity Paths](#complexity-paths)
4. [Component Architecture](#component-architecture)
5. [Design Decisions](#design-decisions)
6. [Implementation Guidelines](#implementation-guidelines)
7. [Quality Standards](#quality-standards)
8. [Anti-Patterns](#anti-patterns)

---

## Core Principles

### 1. Events Are the Source of Truth

**Principle**: All state changes must be represented as immutable events in an append-only log.

**Why**:
- Complete audit trail of all changes
- Ability to replay and reconstruct state at any point in time
- Time-travel debugging
- Temporal queries ("what was the state on date X?")
- Bug fixes can be applied by replaying events with corrected logic

**Implementation Requirement**:
- Events must be immutable once written
- Events must contain all information needed to understand what happened
- Events must be serializable (JSON recommended for flexibility)
- Events must include metadata (timestamp, causation_id, correlation_id, user_id)

### 2. Commands Produce Events

**Principle**: All write operations go through commands that are validated and produce events.

**Why**:
- Clear intent (CreateUser vs UserCreated)
- Validation happens before state change
- Business rules enforced consistently
- Failed commands don't corrupt state
- Commands can be rejected, events cannot

**Implementation Requirement**:
- Commands are imperative (CreateUser, UpdateProfile)
- Events are past tense (UserCreated, ProfileUpdated)
- Commands may produce zero events (validation failure)
- Commands may produce multiple events (complex operations)
- Events from one command must be atomic

### 3. Projections Build Read Models

**Principle**: Read models are derived from events, not stored directly.

**Why**:
- Optimized for queries (denormalized)
- Can be rebuilt from scratch at any time
- Multiple read models from same events
- Schema evolution without migration
- Add new projections retroactively

**Implementation Requirement**:
- Three-trait separation: `Projector` (stateless handler), `Projection` (composed unit), `ReadModelStore` (persistence)
- Projectors must be idempotent (can process same event multiple times)
- Projectors must handle events in order (per aggregate)
- Projections must be rebuildable from full event stream
- Projections should handle missing/unknown events gracefully
- All projection methods take `&self`, not `&mut self` — mutable state lives in the `ReadModelStore`

### 4. Headless by Default

**Principle**: The core library must have zero dependencies on web frameworks, databases, or UI libraries.

**Why**:
- Reusable in CLI tools, background workers, tests
- Web layer is optional (plugin)
- Event sourcing logic is independent of delivery mechanism
- Easier testing (no HTTP server required)
- Smaller binaries for non-web use cases

**Implementation Requirement**:
- `arc-core` depends only on: `serde`, `async-trait`, `uuid`, `thiserror`
- No `actix-web`, `tera`, `diesel` in core
- All storage is abstracted via traits
- EventStore trait can be implemented for SQLite, Postgres, in-memory

### 5. Optimistic Concurrency

**Principle**: Aggregates use version numbers to detect conflicting writes.

**Why**:
- Avoids distributed locks
- Prevents lost updates
- Simple to implement
- Works across distributed systems
- Clear error semantics (conflict detected)

**Implementation Requirement**:
- Each event has a sequence number within its aggregate
- `EventStore::append()` takes `expected_version`
- Conflict returns error, not panic
- Retry logic is caller's responsibility

### 6. Complexity is Opt-In

**Principle**: Developers choose between simple (direct events) or complex (full aggregates + CQRS).

**Why**:
- Simple domains don't need ceremony
- Complex domains get strong guarantees
- Progressive complexity as needed
- Both paths use same EventStore and EventBus

**Implementation Requirement**:
- Simple path: Services emit events directly, projections update read models
- Complex path: Commands → Aggregates → Events → Projections
- Both paths must be first-class citizens
- No "this is the wrong way" messaging

---

## Design Philosophy

### Prefer Composition Over Inheritance

**Rationale**: Rust's trait system and composition model align well with event sourcing.

- EventStore is a trait, not a base class
- Projectors, projections, and read model stores are trait objects, not subclasses
- Aggregates compose behavior via methods, not inheritance hierarchies

### Async by Default

**Rationale**: Modern systems need async I/O for scalability.

- All storage operations are async (`async fn`)
- Use `async-trait` for trait async methods
- EventBus can handle both sync and async subscribers
- Projections may be async (e.g., HTTP calls, DB writes)

### Type Safety Without Boilerplate

**Rationale**: Rust's type system prevents errors, but shouldn't require excessive ceremony.

- Use enums for domain events (type-safe, exhaustive matching)
- Use `serde_json::Value` for generic event payloads (flexibility)
- Provide typed wrappers where it matters, raw JSON where it doesn't
- Use `thiserror` for error types (ergonomic, zero-cost)

### Test-Driven Design

**Rationale**: Event sourcing is testable by design, so tests should be easy.

- In-memory EventStore for fast tests
- Test aggregates by: command → events → apply → assert state
- Test projections by: events → projection → query → assert result
- No database required for most tests

---

## Complexity Paths

### Path 1: Simple Event Publishing (Recommended for MVP)

**When to Use**:
- Simple CRUD operations
- No complex domain invariants
- Validation is straightforward
- Single-aggregate operations

**Architecture**:
```
Controller
  ↓
Service (validates input)
  ↓
EventStore::append(event)
  ↓
EventBus::publish(event)
  ↓
Projections update read models
```

**Example**:
```rust
// Service layer
pub async fn create_user(name: String, email: String, password: String) -> Result<(), Error> {
    // Validate
    if !email.contains('@') {
        return Err(Error::InvalidEmail);
    }

    // Create event
    let event = Event::new(NewEvent {
        aggregate_type: "User",
        aggregate_id: &Uuid::new_v4().to_string(),
        sequence: 1,
        event_type: "UserCreated",
        payload: json!({
                        "name": name,
                        "email": email,
                        "password_hash": hash_password(&password),
                    }),
    });

    // Persist and publish
    event_store.append(&event.aggregate_id, None, vec![event.clone()]).await?;
    event_bus.publish(vec![event]).await?;

    Ok(())
}
```

**Pros**:
- Minimal boilerplate
- Fast to implement
- Easy to understand
- Good for 80% of use cases

**Cons**:
- Validation spread across services
- No aggregate state to enforce invariants
- Optimistic concurrency not enforced
- Cross-event logic difficult

### Path 2: Full CQRS with Aggregates (Recommended for Complex Domains)

**When to Use**:
- Complex business rules
- Multi-event workflows
- Domain invariants must be enforced
- Need aggregate state for validation

**Architecture**:
```
Controller
  ↓
CommandBus::dispatch(command)
  ↓
Load aggregate from EventStore
  ↓
Aggregate::handle(command) → validate → produce events
  ↓
EventStore::append(events, expected_version)
  ↓
EventBus::publish(events)
  ↓
Projections update read models
```

**Example**:
```rust
// Aggregate
pub struct UserAggregate {
    id: Option<String>,
    email: Option<String>,
    created: bool,
    version: i64,
}

impl Aggregate for UserAggregate {
    async fn handle(&self, command: UserCommand) -> Result<Vec<Event>, UserError> {
        match command {
            UserCommand::CreateUser { id, name, email, password } => {
                // Invariant: user cannot be created twice
                if self.created {
                    return Err(UserError::AlreadyExists);
                }

                // Validation
                if !email.contains('@') {
                    return Err(UserError::InvalidEmail);
                }

                // Produce event
                Ok(vec![Event::new(NewEvent {
                    aggregate_type: "User",
                    aggregate_id: &id,
                    sequence: self.version + 1,
                    event_type: "UserCreated",
                    payload: json!({ "name": name, "email": email, "password_hash": hash_password(&password) }),
                })])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        match event.event_type.as_str() {
            "UserCreated" => {
                self.id = Some(event.aggregate_id.clone());
                self.email = Some(event.payload["email"].as_str().unwrap().to_string());
                self.created = true;
                self.version = event.sequence;
            }
            _ => {}
        }
    }
}

// Controller
pub async fn create_user(
    command: UserCommand,
    command_bus: &mut CommandBus<UserAggregate>,
) -> Result<(), Error> {
    command_bus.dispatch(command).await?;
    Ok(())
}
```

**Pros**:
- Strong domain invariants
- Aggregate encapsulates business logic
- Optimistic concurrency enforced
- Easy to test (unit test aggregates)
- State reconstruction from events

**Cons**:
- More boilerplate
- Aggregate design requires thought
- Learning curve for CQRS

### Choosing a Path

**Start with Path 1 (Simple) if**:
- You're new to event sourcing
- Your domain is simple (CRUD)
- You need to ship fast
- You can refactor later

**Use Path 2 (Full CQRS) if**:
- Domain has complex invariants
- You need strong consistency guarantees
- Multiple aggregates interact
- You have experience with CQRS/ES

**Hybrid Approach**:
- Use Path 1 for simple aggregates (e.g., User profile)
- Use Path 2 for complex aggregates (e.g., Order with payment, inventory, shipping)
- Both can coexist in the same system

---

## Component Architecture

### Event

**Purpose**: Immutable record of something that happened.

**Key Properties**:
- `event_id: Uuid` - Globally unique identifier
- `aggregate_type: String` - Type of aggregate (e.g., "User", "Order")
- `aggregate_id: String` - Instance ID (e.g., "user-123")
- `sequence: i64` - Version within aggregate (1, 2, 3, ...)
- `event_type: String` - Type of event (e.g., "UserCreated")
- `payload: serde_json::Value` - Event data
- `metadata: serde_json::Value` - Causation, correlation, user_id, etc.
- `timestamp: SystemTime` - When the event occurred

**Design Decisions**:
- Use `String` for aggregate_type and aggregate_id (flexible, no generics needed)
- Use `serde_json::Value` for payload (flexible, schema evolution)
- Use `i64` for sequence (SQLite-friendly, no overflow in practice)
- Use `SystemTime` (not chrono) to avoid extra dependencies

### EventStore Trait

**Purpose**: Abstraction for appending and loading events.

**Key Methods**:
```rust
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append events to the store
    /// Returns error if expected_version doesn't match (optimistic concurrency)
    async fn append(
        &self,
        aggregate_id: &str,
        expected_version: Option<i64>,
        events: Vec<Event>,
    ) -> Result<(), Box<dyn Error>>;

    /// Load all events for an aggregate
    async fn load(&self, aggregate_id: &str) -> Result<Vec<Event>, Box<dyn Error>>;

    /// Load events from a specific sequence number
    async fn load_from(
        &self,
        aggregate_id: &str,
        from_sequence: i64,
    ) -> Result<Vec<Event>, Box<dyn Error>>;

    /// Stream all events (for projections)
    async fn stream_all(
        &self,
        from_position: i64,
    ) -> Result<Vec<Event>, Box<dyn Error>>;
}
```

**Design Decisions**:
- `expected_version` is `Option<i64>`:
  - `None` = don't check (first event or idempotent writes)
  - `Some(v)` = ensure current version matches `v`
- Returns `Vec<Event>` not `Stream<Event>` (simpler for SQLite, can optimize later)
- Errors are `Box<dyn Error>` (flexibility for implementers)

**Implementations**:
- `SQLiteEventStore` - Production use, persistent
- `InMemoryEventStore` - Testing, fast
- `PostgresEventStore` - Future (larger scale)

### EventBus Trait

**Purpose**: Publish events to subscribers.

**Key Methods**:
```rust
#[async_trait]
pub trait EventBus: Send + Sync {
    /// Publish events to all subscribers
    async fn publish(&self, events: Vec<Event>) -> Result<(), Box<dyn Error>>;

    /// Subscribe a handler
    async fn subscribe(&mut self, handler: Box<dyn EventHandler>);
}

#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Handle an event
    async fn handle(&self, event: &Event) -> Result<(), Box<dyn Error>>;

    /// Event types this handler subscribes to
    fn handles(&self) -> Vec<String>;
}
```

**Design Decisions**:
- Subscribers filter by `event_type` (not aggregate_type)
- `publish()` delivers to all handlers synchronously (async internally)
- Errors from one handler don't affect others (log and continue)
- `InProcessEventBus` is default (same process, no broker needed)
- Future: `ChannelEventBus` (async channels), `NatsEventBus` (distributed)

### Projection Architecture (Three-Trait Separation)

**Purpose**: Build read models from events with clear separation of concerns.

The monolithic `Projection` trait has been split into three focused traits:

#### Projector Trait — Stateless Event Handler

**Purpose**: Contains the pure event-handling logic. Stateless — takes `&self`.

```rust
#[async_trait]
pub trait Projector: Send + Sync {
    /// Unique name identifying this projector
    fn name(&self) -> &str;

    /// Event types this projector handles
    fn handles(&self) -> Vec<String>;

    /// Apply a single event to the read model via the store (idempotent)
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()>;

    /// Initialize the read model schema (CREATE TABLE IF NOT EXISTS, etc.)
    async fn init(&self, _store: &dyn ReadModelStore) -> ProjectionResult<()> {
        Ok(())
    }
}
```

#### ReadModelStore Trait — Persistence Layer

**Purpose**: Backend-agnostic storage for projection read models. Defined in `read_model_store.rs`.

```rust
#[async_trait]
pub trait ReadModelStore: Send + Sync {
    /// Execute a write operation (INSERT, UPDATE, DELETE)
    async fn execute(&self, sql: &str, params: Vec<serde_json::Value>) -> ReadModelResult<()>;

    /// Execute a query and return rows
    async fn query(&self, sql: &str, params: Vec<serde_json::Value>) -> ReadModelResult<Vec<Row>>;

    /// Truncate/clear a table or collection
    async fn truncate(&self, table: &str) -> ReadModelResult<()>;
}
```

`InMemoryReadModelStore` is built into `arc-core` for testing and ephemeral projections.
Production backends (SQLite, Postgres, dqlite) live in separate crates.

#### Projection Trait — Composed Read Model Unit

**Purpose**: The assembled unit that ties a projector to its store. Takes `&self`.

```rust
#[async_trait]
pub trait Projection: Send + Sync {
    fn name(&self) -> &str;
    fn handles(&self) -> Vec<String>;
    async fn handle(&self, event: &Event) -> ProjectionResult<()>;
    async fn clear(&self) -> ProjectionResult<()>;
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
```

#### ProjectionUnit — Standard Composition Glue

`ProjectionUnit` is the standard way to compose a `Projector` + `Arc<dyn ReadModelStore>` + table name into a `Projection`:

```rust
pub struct ProjectionUnit {
    projector: Box<dyn Projector>,
    store: Arc<dyn ReadModelStore>,
    table: String,
}
```

It delegates `handle()` to `projector.apply(event, store)` and `clear()` to `store.truncate(table)`.

**Design Decisions**:
- **Three-way split**: Handler logic (projector) is separate from storage (read model store) and orchestration (projection engine)
- **`&self` throughout**: All projection methods take `&self`, not `&mut self`. Mutable state lives in the `ReadModelStore` via interior mutability (connection pools, `Mutex`, etc.)
- **Projectors are stateless**: Same events + empty store = same read model. Safe to share across threads
- **`apply()` must be idempotent**: Use UPSERT, check event_id, or make operations naturally idempotent (SET vs INCREMENT)
- **`init()` for schema setup**: Called once on registration and before rebuilds. Default is no-op
- **`clear()` delegates to store.truncate()**: Clean separation between the "what to clear" and "how to clear"
- **Composable backends**: Swap `InMemoryReadModelStore` for `SqliteReadModelStore` without changing projector logic

**Implementation Example**:
```rust
struct UserListProjector;

#[async_trait]
impl Projector for UserListProjector {
    fn name(&self) -> &str { "UserList" }

    fn handles(&self) -> Vec<String> {
        vec!["UserCreated".to_string(), "ProfileUpdated".to_string()]
    }

    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        match event.event_type.as_str() {
            "UserCreated" => {
                store.execute("INSERT OR REPLACE INTO users_view ...", vec![event.payload.clone()]).await
                    .map_err(|e| ProjectionError::handle_failed("UserList", &event.event_type, &event.event_id.to_string(), e.to_string()))?;
            }
            "ProfileUpdated" => {
                store.execute("UPDATE users_view ...", vec![event.payload.clone()]).await
                    .map_err(|e| ProjectionError::handle_failed("UserList", &event.event_type, &event.event_id.to_string(), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }
}

// Compose: projector + store = projection
let store = Arc::new(InMemoryReadModelStore::new());
let projection = ProjectionUnit::new(Box::new(UserListProjector), store, "users_view");

// Register with engine
let mut engine = ProjectionEngine::new(event_store);
engine.register(Box::new(projection));
```

### Aggregate Trait

**Purpose**: Encapsulate domain logic, validate commands, produce events, apply events to state.

**Key Methods**:
```rust
#[async_trait]
pub trait Aggregate: Send + Sync + Default {
    type Command: Command;
    type Event;
    type Error: Error + Send + Sync + 'static;

    /// Aggregate type name
    fn aggregate_type() -> &'static str;

    /// Current version (sequence number)
    fn version(&self) -> i64;

    /// Handle a command and produce events
    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error>;

    /// Apply an event to update state
    fn apply(&mut self, event: &Event);

    /// Reconstruct aggregate from events
    fn from_events(events: Vec<Event>) -> Self {
        let mut aggregate = Self::default();
        for event in events {
            aggregate.apply(&event);
        }
        aggregate
    }
}
```

**Design Decisions**:
- `handle()` is pure (no side effects, just produce events)
- `apply()` is deterministic (same events = same state)
- `from_events()` has default implementation (fold over events)
- Aggregate state is private (not exposed outside)
- `handle()` inspects `self` (current state) to make decisions

### CommandBus

**Purpose**: Dispatch commands to aggregates, handle concurrency, persist events.

**Key Methods**:
```rust
pub struct CommandBus<A: Aggregate> {
    event_store: Box<dyn EventStore>,
    event_bus: Box<dyn EventBus>,
    _phantom: PhantomData<A>,
}

impl<A: Aggregate> CommandBus<A> {
    pub async fn dispatch(&mut self, command: A::Command) -> Result<Vec<Event>, Box<dyn Error>> {
        let aggregate_id = command.aggregate_id();

        // 1. Load existing events
        let events = self.event_store.load(aggregate_id).await?;
        let current_version = events.last().map(|e| e.sequence).unwrap_or(0);

        // 2. Reconstruct aggregate
        let aggregate = A::from_events(events);

        // 3. Handle command
        let new_events = aggregate.handle(command).await?;

        // 4. Append events (with optimistic concurrency check)
        self.event_store
            .append(aggregate_id, Some(current_version), new_events.clone())
            .await?;

        // 5. Publish events
        self.event_bus.publish(new_events.clone()).await?;

        Ok(new_events)
    }
}
```

**Design Decisions**:
- `dispatch()` coordinates load → handle → append → publish
- Optimistic concurrency enforced (expected_version)
- If concurrency conflict, caller must retry
- Errors from `handle()` don't write events (validation failures are safe)

---

## Design Decisions

### 1. Why SQLite for Event Store?

**Decision**: Use SQLite as the default event store, not Postgres.

**Rationale**:
- Embedded, zero-config, perfect for MVP
- Sufficient for most use cases (millions of events)
- Fast for single-node (no network overhead)
- Easy to backup (single file)
- Diesel already used in project

**Trade-offs**:
- Single-writer (but ES is single-writer per aggregate anyway)
- Not ideal for distributed systems (but local per node with sync layer solves this)
- Limited concurrency (but use connection pool)

**Future**: Add `PostgresEventStore` for larger scale, keep SQLite as default.

### 2. Why JSON for Event Payloads?

**Decision**: Use `serde_json::Value` for event payloads, not typed structs.

**Rationale**:
- Schema evolution (add/remove fields without migration)
- Flexibility (different event types have different shapes)
- Easy to store in SQLite (TEXT column)
- Easy to debug (human-readable)
- Supports nested structures

**Trade-offs**:
- No compile-time type safety for payload
- Requires runtime deserialization

**Mitigation**: Provide typed wrappers in application layer (enum `UserEvent` deserializes from JSON).

### 3. Why `async-trait` Instead of Native Async?

**Decision**: Use `async-trait` crate for trait async methods.

**Rationale**:
- Rust doesn't support `async fn` in traits (yet)
- `async-trait` is stable, widely used, zero runtime cost
- Allows `EventStore`, `Projector`, `Projection`, `ReadModelStore`, etc. to have async methods

**Trade-offs**:
- Slight compile-time overhead (macro expansion)
- Boxing (but negligible for I/O-bound operations)

**Future**: When Rust stabilizes async fn in traits, migrate away from `async-trait`.

### 4. Why In-Process EventBus, Not External Broker?

**Decision**: Default to `InProcessEventBus`, not NATS/Kafka/RabbitMQ.

**Rationale**:
- Simpler (no external dependencies for MVP)
- Faster (no network, no serialization)
- Easier to test
- Sufficient for single-node

**Trade-offs**:
- No distributed pub/sub
- No durability if process crashes before projections run

**Mitigation**:
- Projections can rebuild from EventStore
- For distributed: add `NatsEventBus` or `KafkaEventBus` (opt-in)

### 5. Why Optimistic Concurrency, Not Locks?

**Decision**: Use version-based optimistic concurrency, not distributed locks.

**Rationale**:
- Simpler (no lock manager)
- Faster (no waiting for locks)
- Works across distributed systems
- Clear error semantics (conflict detected)
- Scales better (no lock contention)

**Trade-offs**:
- Caller must handle retries
- Not suitable for extremely high contention (but rare in practice)

**Implementation**: `expected_version` in `EventStore::append()`.

### 6. Why Two Complexity Paths?

**Decision**: Support both simple (direct events) and complex (aggregates) paths.

**Rationale**:
- Most use cases are simple CRUD (Path 1)
- Some use cases need strong invariants (Path 2)
- Forcing full CQRS for everything is overkill
- Both paths use same infrastructure (EventStore, EventBus)

**Trade-offs**:
- Two ways to do things (documentation must be clear)
- Risk of mixing patterns inconsistently

**Mitigation**: Provide clear guidelines on when to use each path.

---

## Implementation Guidelines

### For Agent 1 (Core Library Developer)

**Your Job**: Implement the core traits and types in `arc-core`.

**Priority Order**:
1. `Event` type (Week 1)
2. `EventStore` trait + SQLite implementation (Week 2)
3. `EventBus` trait + InProcessEventBus (Week 3)
4. `Projector` + `Projection` + `ReadModelStore` traits + ProjectionEngine (Week 4)
5. `Aggregate` trait (Week 5)
6. `CommandBus` (Week 6)

**Key Guidelines**:
- No `actix-web`, `tera`, or web dependencies in `arc-core`
- Use `async-trait` for trait async methods
- Use `thiserror` for error types
- Write comprehensive unit tests (target: 95% coverage)
- Provide `InMemoryEventStore` for tests
- Document all public APIs with doc comments

**Testing Strategy**:
- Test Event serialization/deserialization
- Test EventStore append with optimistic concurrency
- Test EventStore load and stream_all
- Test EventBus publish/subscribe
- Test Projection handle and rebuild
- Test Aggregate handle → apply → from_events
- Test CommandBus full flow (load → handle → append → publish)

### For Agent 2 (Domain Implementer)

**Your Job**: Implement the first aggregate (User) in `arc-app`.

**Priority Order**:
1. Define UserCommand enum (CreateUser, UpdateProfile, ChangePassword)
2. Define UserEvent enum (UserCreated, ProfileUpdated, PasswordChanged)
3. Implement UserAggregate (handle, apply, from_events)
4. Implement UserListProjector + compose into ProjectionUnit (handle UserCreated, ProfileUpdated)
5. Write integration tests (command → events → projection → query)

**Key Guidelines**:
- Follow the Full CQRS path (Aggregate + CommandBus)
- Aggregate state is private (not exposed)
- `handle()` validates and produces events (no side effects)
- `apply()` updates state (deterministic)
- Projector is idempotent (can process same event twice)
- Use ReadModelStore implementations for read model persistence

**Testing Strategy**:
- Unit test UserAggregate:
  - Command → events → apply → assert state
  - Invalid command → error
- Integration test full flow:
  - Dispatch command → events persisted → projection updated → query read model

### For Agent 3 (Migration Specialist)

**Your Job**: Migrate existing MVC code to event sourcing (Dual-Write phase).

**Priority Order**:
1. Add events table migration (Diesel)
2. Add users_view table migration (Diesel)
3. Update auth_controller to use CommandBus
4. Keep Diesel writes for safety (dual-write)
5. Monitor consistency (events vs Diesel)
6. Remove Diesel writes after 1 week of stability

**Key Guidelines**:
- Dual-write: Write to both EventStore AND Diesel
- Validate consistency: Events should produce same state as Diesel
- If mismatch: Log error, don't fail request (investigate later)
- Reads still use Diesel (from users_view)
- After migration: Diesel is only in projections (read models)

**Testing Strategy**:
- Test dual-write consistency:
  - Create user → check EventStore has event → check Diesel has row
  - Update user → check events → check Diesel updated
- Test projection updates:
  - Publish event → projection handles → read model updated

### For Agent 4 (QA / Testing)

**Your Job**: Ensure quality and correctness of ES implementation.

**Priority Order**:
1. Unit test coverage (target: 95% for core)
2. Integration tests (EventStore + EventBus + Projections)
3. End-to-end tests (HTTP → Command → Events → Projection → Query)
4. Performance tests (event throughput, projection rebuild time)
5. Concurrency tests (optimistic locking, race conditions)

**Key Guidelines**:
- Test all error paths (validation failures, concurrency conflicts)
- Test projection rebuild (clear → replay → verify state)
- Test aggregate reconstruction (events → from_events → verify state)
- Test optimistic concurrency (two writes to same aggregate)
- Use InMemoryEventStore for fast tests
- Use SQLite for integration tests

**Testing Strategy**:
- Unit tests: Fast, no I/O, test pure logic
- Integration tests: Use real SQLite, test persistence
- E2E tests: Use TestServer (Actix), test full HTTP flow
- Load tests: Use `criterion` for benchmarks

### For Agent 6 (Documentation)

**Your Job**: Document the event sourcing library.

**Priority Order**:
1. API documentation (rustdoc for all public APIs)
2. Tutorials (how to create your first aggregate)
3. Examples (UserAggregate, OrderAggregate)
4. Migration guide (MVC → ES)
5. Troubleshooting (common errors, how to debug)

**Key Guidelines**:
- Explain why, not just what
- Provide code examples for every concept
- Document both complexity paths
- Explain when to use each path
- Include diagrams (mermaid) for flows
- Link to external resources (Martin Fowler's Event Sourcing)

**Documentation Structure**:
```
crates/arc-core/README.md      # Overview, quickstart
crates/arc-core/docs/
  01-concepts.md                    # ES concepts
  02-simple-path.md                 # Path 1 tutorial
  03-full-cqrs.md                   # Path 2 tutorial
  04-projections.md                 # Building read models
  05-testing.md                     # Testing strategies
  06-migration.md                   # MVC → ES migration
  07-troubleshooting.md             # Common errors
  examples/
    user_aggregate.rs
    order_aggregate.rs
    blog_post_aggregate.rs
```

### For Agent 7 (Technical Writer)

**Your Job**: Create user-facing documentation for the main docs.

**Priority Order**:
1. Update `docs/` to reflect ES architecture
2. Keep the user-facing event-sourcing guidance in the canonical Docsify set
3. Update `progress.md` with ES implementation progress
4. Create diagrams (architecture, flows, sequences)

**Key Guidelines**:
- Write for developers new to event sourcing
- Use analogies (event sourcing is like Git for your database)
- Provide concrete examples (e.g., User domain)
- Explain trade-offs (when ES is overkill, when it's essential)
- Link to reference documentation (rustdoc)

---

## Quality Standards

### Code Quality

**Requirements**:
- Zero compiler warnings (`cargo build` must be clean)
- Zero clippy warnings (`cargo clippy` must be clean)
- Formatted with rustfmt (`cargo fmt`)
- All public APIs documented (rustdoc)
- No `unwrap()` or `expect()` in production code (use `?` or `match`)
- No `panic!()` in library code (return `Result`)

**Testing Requirements**:
- Unit test coverage: 95%+ for core library
- Integration test coverage: 80%+ for application
- All error paths tested
- All concurrency scenarios tested (optimistic locking)
- All projections tested (rebuild capability)

**Performance Requirements**:
- Event store write: <5ms p99 (SQLite)
- Event store load: <10ms p99 (1000 events)
- Projection rebuild: <1 min per 100k events
- EventBus publish: <1ms p99 (in-process)

### Architecture Quality

**Requirements**:
- Core library is headless (no web dependencies)
- All storage is abstracted via traits
- No concrete implementations in trait definitions
- Clear separation: Core → Storage → Application → Web
- Dependency direction: Web → App → Core (never reversed)

**Design Patterns**:
- Use traits for abstractions (EventStore, Projector, Projection, ReadModelStore, Aggregate)
- Use enums for domain events (type-safe, exhaustive)
- Use `thiserror` for error types (derive, zero-cost)
- Use `async-trait` for async traits
- Use `PhantomData` for generic markers (CommandBus)

---

## Anti-Patterns

### 1. Mutable Events

**Anti-Pattern**: Modifying events after they're written.

**Why It's Bad**: Breaks audit trail, corrupts state reconstruction.

**Correct Approach**: Events are immutable. To fix a mistake, write a compensating event.

**Example**:
```rust
// WRONG: Mutating event
let mut event = event_store.load("user-123").await?[0];
event.payload["email"] = "corrected@example.com";
event_store.update(event).await?; // NO!

// CORRECT: Compensating event
let correction_event = Event::new(NewEvent {
    aggregate_type: "User",
    aggregate_id: "user-123",
    sequence: 2,
    event_type: "EmailCorrected",
    payload: json!({ "old_email": "wrong@example.com", "new_email": "corrected@example.com" }),
});
event_store.append("user-123", Some(1), vec![correction_event]).await?;
```

### 2. Querying EventStore for Reads

**Anti-Pattern**: Loading events and replaying to answer queries.

**Why It's Bad**: Slow, doesn't scale, defeats the purpose of projections.

**Correct Approach**: Query read models (projections), not events.

**Example**:
```rust
// WRONG: Query events
let events = event_store.load("user-123").await?;
let user = UserAggregate::from_events(events);
let email = user.email(); // Slow!

// CORRECT: Query projection
let user = diesel::sql_query("SELECT * FROM users_view WHERE id = ?")
    .bind::<Text, _>("user-123")
    .get_result::<UserView>(&mut conn)?; // Fast!
```

### 3. Direct DB Writes in Aggregates

**Anti-Pattern**: Aggregates writing to the database directly.

**Why It's Bad**: Violates event sourcing, bypasses EventStore, no audit trail.

**Correct Approach**: Aggregates produce events, projections write to DB.

**Example**:
```rust
// WRONG: Direct write in aggregate
impl UserAggregate {
    async fn handle(&self, command: CreateUser) -> Result<Vec<Event>, Error> {
        diesel::insert_into(users).values(&new_user).execute(&mut conn)?; // NO!
        Ok(vec![event])
    }
}

// CORRECT: Aggregate produces event, projector writes via store
impl UserAggregate {
    async fn handle(&self, command: CreateUser) -> Result<Vec<Event>, Error> {
        Ok(vec![Event::new(NewEvent { ... })])  // Just produce event
    }
}

impl Projector for UserListProjector {
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        if event.event_type == "UserCreated" {
            store.execute("INSERT INTO users_view ...", vec![event.payload.clone()]).await?;
        }
        Ok(())
    }
}
```

### 4. Ignoring Optimistic Concurrency Errors

**Anti-Pattern**: Not handling concurrency conflicts, just retrying blindly.

**Why It's Bad**: Lost updates, data corruption, race conditions.

**Correct Approach**: Detect conflicts, reload state, retry command.

**Example**:
```rust
// WRONG: Ignore conflict
loop {
    match command_bus.dispatch(command).await {
        Ok(_) => break,
        Err(_) => continue, // Retry without reloading state!
    }
}

// CORRECT: Reload and retry
let mut retries = 0;
loop {
    match command_bus.dispatch(command.clone()).await {
        Ok(_) => break,
        Err(e) if is_concurrency_error(&e) && retries < 3 => {
            retries += 1;
            // Command bus will reload state automatically on next dispatch
        }
        Err(e) => return Err(e), // Not a concurrency error, propagate
    }
}
```

### 5. Non-Idempotent Projections

**Anti-Pattern**: Projections that produce different results if events are replayed.

**Why It's Bad**: Rebuilds produce incorrect state, eventual consistency broken.

**Correct Approach**: Projections must be idempotent (same events = same state).

**Example**:
```rust
// WRONG: Non-idempotent (increments counter)
impl Projector for StatsProjector {
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        if event.event_type == "UserCreated" {
            store.execute("UPDATE stats SET user_count = user_count + 1", vec![]).await?;
            // Replaying will over-count!
        }
        Ok(())
    }
}

// CORRECT: Idempotent (set value)
impl Projector for StatsProjector {
    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        if event.event_type == "UserCreated" {
            store.execute(
                "INSERT INTO stats (user_id, ...) VALUES (?, ...) ON CONFLICT DO NOTHING",
                vec![event.payload.clone()],
            ).await?; // Replaying is safe
        }
        Ok(())
    }
}
```

### 6. Synchronous I/O in Event Handlers

**Anti-Pattern**: Blocking I/O in EventBus subscribers.

**Why It's Bad**: Slows down event publishing, blocks other handlers.

**Correct Approach**: Use async I/O, or spawn background task.

**Example**:
```rust
// WRONG: Blocking I/O
impl EventHandler for EmailNotifier {
    async fn handle(&self, event: &Event) -> Result<(), Box<dyn Error>> {
        std::thread::sleep(Duration::from_secs(5)); // Blocks!
        send_email(&event)?; // Blocking SMTP call
        Ok(())
    }
}

// CORRECT: Async I/O or background task
impl EventHandler for EmailNotifier {
    async fn handle(&self, event: &Event) -> Result<(), Box<dyn Error>> {
        let event = event.clone();
        tokio::spawn(async move {
            send_email_async(&event).await; // Non-blocking
        });
        Ok(())
    }
}
```

---

## Summary

**Core Philosophy**:
- Events are the source of truth
- Commands are validated and produce events
- Projections build optimized read models
- Complexity is opt-in (simple or full CQRS)
- Core is headless (web is a plugin)

**Key Design Decisions**:
- SQLite for event store (embedded, zero-config)
- JSON for event payloads (flexible, schema evolution)
- Optimistic concurrency (no distributed locks)
- In-process EventBus (simple, fast, sufficient for single node)
- Two complexity paths (simple and full CQRS)

**Implementation Guidelines**:
- Core library: traits and types, no web dependencies
- Domain: implement aggregates, commands, events
- Application: wire up CommandBus, projections, controllers
- Testing: unit tests (in-memory), integration tests (SQLite), E2E tests (Actix)

**Quality Standards**:
- 95%+ test coverage for core
- Zero warnings (compiler, clippy)
- All public APIs documented
- Performance: <5ms event write, <1min rebuild per 100k events

**Anti-Patterns to Avoid**:
- Mutable events
- Querying EventStore for reads
- Direct DB writes in aggregates
- Ignoring concurrency errors
- Non-idempotent projections
- Blocking I/O in handlers

---

## Next Steps for Agents

### Immediate Actions
1. **Agent 1** (Core): Start with `Event` type and serialization tests
2. **Agent 2** (Domain): Review UserAggregate design, prepare commands/events
3. **Agent 3** (Migration): Create Diesel migrations for events table
4. **Agent 4** (QA): Setup test infrastructure (fixtures, helpers)
5. **Agent 6** (Docs): Start rustdoc for Event type
6. **Agent 7** (Writer): Update architecture docs with ES diagrams

### Weekly Check-ins
- Review completed work for architectural consistency
- Answer design questions
- Validate trade-offs
- Ensure quality standards are met

### Architecture Review Points
- Week 2: Review EventStore trait and SQLite implementation
- Week 4: Review Projector/Projection/ReadModelStore traits and rebuild capability
- Week 6: Review Aggregate trait and CommandBus
- Week 8: Review first aggregate (User) implementation
- Week 10: Review dual-write migration strategy

---

**Questions or concerns?** Escalate to Agent 5 (Software Architect) for guidance.

**This document is living documentation.** Update as design decisions evolve.
