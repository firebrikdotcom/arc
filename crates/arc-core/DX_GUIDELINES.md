# Developer Experience Guidelines for Arc Event Sourcing

> Version: 1.0.0
> Target Audience: Library developers implementing arc-core and plugin authors
> Last Updated: 2026-03-01

---

## Table of Contents

1. [Naming Conventions](#naming-conventions)
2. [Error Handling Patterns](#error-handling-patterns)
3. [Example Patterns](#example-patterns)
4. [Common Pitfalls](#common-pitfalls)
5. [IDE Support Considerations](#ide-support-considerations)
6. [API Design Principles](#api-design-principles)
7. [Testing Patterns](#testing-patterns)
8. [Migration and Versioning](#migration-and-versioning)

---

## 1. Naming Conventions

### 1.1 Core Principle: Consistent Terminology

Use domain-driven design terminology consistently throughout the codebase:

| Concept | Type Suffix | Example | Bad Example |
|---------|-------------|---------|-------------|
| Domain Commands | `Command` | `CreateUserCommand` | `UserCreationRequest` |
| Domain Events | `Event` | `UserCreatedEvent` | `UserCreatedMessage` |
| Aggregates | `Aggregate` | `UserAggregate` | `UserEntity`, `UserModel` |
| Projections | `Projection` | `UserListProjection` | `UserView`, `UserReadModel` |
| Event Handlers | `Handler` | `WelcomeEmailHandler` | `WelcomeEmailListener` |
| Errors | `Error` | `UserError`, `EventStoreError` | `UserException` |

### 1.2 Trait Naming

Traits should be nouns describing capabilities, not verbs:

```rust
// ✅ GOOD: Noun-based traits
pub trait EventStore { }
pub trait EventBus { }
pub trait Aggregate { }
pub trait Projection { }
pub trait Command { }

// ❌ BAD: Verb-based traits
pub trait StoreEvents { }
pub trait PublishEvents { }
pub trait HandleCommands { }
```

### 1.3 Method Naming

Methods should follow Rust conventions with clear, action-oriented verbs:

```rust
// ✅ GOOD: Clear verbs, self-documenting
async fn append(&self, aggregate_id: &str, events: Vec<Event>) -> Result<()>;
async fn load(&self, aggregate_id: &str) -> Result<Vec<Event>>;
async fn handle(&self, command: Self::Command) -> Result<Vec<Event>>;
async fn publish(&self, events: Vec<Event>) -> Result<()>;

// ❌ BAD: Ambiguous or inconsistent
async fn add(&self, id: &str, ev: Vec<Event>) -> Result<()>;
async fn get(&self, id: &str) -> Result<Vec<Event>>;
async fn process(&self, cmd: Self::Command) -> Result<Vec<Event>>;
async fn send(&self, events: Vec<Event>) -> Result<()>;
```

### 1.4 Event and Command Naming

Follow domain language exactly - events are past tense, commands are imperative:

```rust
// ✅ GOOD: Clear domain language
pub enum UserCommand {
    CreateUser { id: String, name: String, email: String },
    UpdateProfile { id: String, name: String },
    ChangePassword { id: String, old_password: String, new_password: String },
    DeleteUser { id: String },
}

pub enum UserEvent {
    UserCreated { id: String, name: String, email: String, created_at: DateTime },
    ProfileUpdated { id: String, name: String, updated_at: DateTime },
    PasswordChanged { id: String, changed_at: DateTime },
    UserDeleted { id: String, deleted_at: DateTime },
}

// ❌ BAD: Inconsistent tense, unclear intent
pub enum UserCommand {
    UserCreate { ... },      // Wrong: sounds like an event
    ProfileUpdate { ... },   // Wrong: not imperative
    PasswordSet { ... },     // Ambiguous: set for first time or change?
}

pub enum UserEvent {
    CreateUser { ... },      // Wrong: sounds like a command
    UpdateProfile { ... },   // Wrong: not past tense
}
```

### 1.5 Generic Type Parameters

Use descriptive single-letter conventions consistently:

```rust
// ✅ GOOD: Standard Rust conventions
pub trait Aggregate {
    type Command: Command;     // C for command
    type Event;                // E for event
    type Error: Error;         // E for error (different context)
}

pub struct CommandBus<A: Aggregate> {  // A for aggregate
    event_store: Box<dyn EventStore>,
    event_bus: Box<dyn EventBus>,
    _phantom: PhantomData<A>,
}

// When multiple similar types exist, use descriptive names
pub struct ProjectionEngine<P, E>
where
    P: Projection,
    E: EventStore,
{
    projections: Vec<P>,
    event_store: E,
}
```

---

## 2. Error Handling Patterns

### 2.1 Principle: Be Explicit and Helpful

Every error should tell the developer:
1. **What** went wrong
2. **Why** it went wrong
3. **How** to fix it (when possible)

### 2.2 Error Type Design

Use `thiserror` for domain errors with rich context:

```rust
use thiserror::Error;

// ✅ GOOD: Rich, contextual errors
#[derive(Debug, Error)]
pub enum UserError {
    #[error("User with email '{email}' already exists")]
    AlreadyExists { email: String },

    #[error("User '{id}' not found")]
    NotFound { id: String },

    #[error("Invalid email format: '{email}'. Must contain '@' and valid domain")]
    InvalidEmail { email: String },

    #[error("Password must be at least 8 characters, contain uppercase, lowercase, and number")]
    WeakPassword,

    #[error("Old password is incorrect for user '{id}'")]
    IncorrectPassword { id: String },
}

// ❌ BAD: Generic, unhelpful errors
#[derive(Debug, Error)]
pub enum UserError {
    #[error("User exists")]
    AlreadyExists,

    #[error("Not found")]
    NotFound,

    #[error("Invalid")]
    Invalid,
}
```

### 2.3 Event Store Errors

Event store errors should be categorized by severity and recoverability:

```rust
#[derive(Debug, Error)]
pub enum EventStoreError {
    // Transient errors (retry possible)
    #[error("Database connection lost. Retrying...")]
    ConnectionLost,

    #[error("Database lock timeout after {timeout_ms}ms. Retry or use exponential backoff")]
    LockTimeout { timeout_ms: u64 },

    // Concurrency errors (expected in normal flow)
    #[error(
        "Optimistic concurrency violation for aggregate '{aggregate_id}': \
         expected version {expected}, but current version is {actual}. \
         Reload aggregate and retry command"
    )]
    ConcurrencyViolation {
        aggregate_id: String,
        expected: i64,
        actual: i64,
    },

    // Permanent errors (fix code or data)
    #[error(
        "Event serialization failed for {event_type}: {source}. \
         Check event payload is valid JSON"
    )]
    SerializationFailed {
        event_type: String,
        source: serde_json::Error,
    },

    #[error(
        "Invalid event sequence for aggregate '{aggregate_id}': \
         sequence {sequence} already exists. Events must be unique and sequential"
    )]
    DuplicateSequence {
        aggregate_id: String,
        sequence: i64,
    },
}
```

### 2.4 Error Propagation

Use `?` operator liberally, but add context at boundaries:

```rust
// ✅ GOOD: Add context when crossing boundaries
impl CommandBus<UserAggregate> {
    pub async fn dispatch(&mut self, command: UserCommand) -> Result<Vec<Event>, CommandError> {
        let aggregate_id = command.aggregate_id();

        // Load events - add context if fails
        let events = self.event_store
            .load(aggregate_id)
            .await
            .map_err(|e| CommandError::EventStoreFailure {
                aggregate_id: aggregate_id.to_string(),
                operation: "load",
                source: e,
            })?;

        // ... rest of implementation
    }
}

// ❌ BAD: Silent error propagation loses context
impl CommandBus<UserAggregate> {
    pub async fn dispatch(&mut self, command: UserCommand) -> Result<Vec<Event>, Box<dyn Error>> {
        let aggregate_id = command.aggregate_id();
        let events = self.event_store.load(aggregate_id).await?;  // Lost context!
        // ...
    }
}
```

### 2.5 User-Facing Error Messages

For web controllers, convert domain errors to HTTP responses with actionable messages:

```rust
// ✅ GOOD: Actionable error responses
#[post("/users")]
pub async fn create_user(
    form: web::Form<CreateUserForm>,
    command_bus: web::Data<Arc<Mutex<CommandBus<UserAggregate>>>>,
) -> impl Responder {
    let command = UserCommand::CreateUser {
        id: Uuid::new_v4().to_string(),
        name: form.name.clone(),
        email: form.email.clone(),
        password: form.password.clone(),
    };

    match command_bus.lock().await.dispatch(command).await {
        Ok(_) => HttpResponse::Created().json(json!({
            "success": true,
            "message": "User created successfully"
        })),
        Err(e) => match e.downcast_ref::<UserError>() {
            Some(UserError::AlreadyExists { email }) => {
                HttpResponse::Conflict().json(json!({
                    "error": "USER_ALREADY_EXISTS",
                    "message": format!("A user with email '{}' already exists", email),
                    "field": "email",
                    "suggestion": "Try logging in instead, or use a different email"
                }))
            }
            Some(UserError::InvalidEmail { email }) => {
                HttpResponse::BadRequest().json(json!({
                    "error": "INVALID_EMAIL",
                    "message": format!("'{}' is not a valid email address", email),
                    "field": "email",
                    "suggestion": "Enter a valid email like user@example.com"
                }))
            }
            Some(UserError::WeakPassword) => {
                HttpResponse::BadRequest().json(json!({
                    "error": "WEAK_PASSWORD",
                    "message": "Password does not meet security requirements",
                    "field": "password",
                    "requirements": [
                        "At least 8 characters",
                        "One uppercase letter",
                        "One lowercase letter",
                        "One number"
                    ]
                }))
            }
            _ => HttpResponse::InternalServerError().json(json!({
                "error": "INTERNAL_ERROR",
                "message": "An unexpected error occurred. Please try again later."
            }))
        }
    }
}
```

---

## 3. Example Patterns

### 3.1 Progressive Disclosure: Simple to Complex

Show the simplest working example first, then progressively add complexity.

#### Level 1: Minimal Event Store Usage (No Aggregates)

```rust
// Simple service emitting events directly
use arc_core::{Event, EventStore};

pub async fn create_user_simple(
    id: &str,
    name: &str,
    email: &str,
    event_store: &dyn EventStore,
) -> Result<(), Box<dyn Error>> {
    // Create event
    let event = Event::new(NewEvent {
        aggregate_type: "User",
        aggregate_id: id,
        sequence: 1,
        event_type: // First event
                    "UserCreated",
        payload: json!({
                        "id": id,
                        "name": name,
                        "email": email,
                    }),
    });

    // Append to store
    event_store.append(id, None, vec![event]).await?;

    Ok(())
}
```

#### Level 2: With Aggregate (Domain Logic)

```rust
// Add business logic with aggregates
use arc_core::{Aggregate, Command, Event, EventStore};

pub struct UserAggregate {
    id: Option<String>,
    email: Option<String>,
    created: bool,
    version: i64,
}

impl Aggregate for UserAggregate {
    type Command = UserCommand;
    type Event = UserEvent;
    type Error = UserError;

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            UserCommand::CreateUser { id, name, email, password } => {
                // Business rule: can't create if already exists
                if self.created {
                    return Err(UserError::AlreadyExists { email });
                }

                // Validation
                if !email.contains('@') {
                    return Err(UserError::InvalidEmail { email });
                }

                let password_hash = hash_password(&password)?;

                Ok(vec![Event::new(NewEvent {
                    aggregate_type: "User",
                    aggregate_id: &id,
                    sequence: self.version + 1,
                    event_type: "UserCreated",
                    payload: json!(UserEvent::UserCreated {
                                                id,
                                                name,
                                                email,
                                                password_hash,
                                                created_at: Utc::now(),
                                            }),
                })])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        // Update aggregate state from event
        match event.event_type.as_str() {
            "UserCreated" => {
                let data: UserEvent = serde_json::from_value(event.payload.clone()).unwrap();
                if let UserEvent::UserCreated { id, email, .. } = data {
                    self.id = Some(id);
                    self.email = Some(email);
                    self.created = true;
                    self.version = event.sequence;
                }
            }
            _ => {}
        }
    }
}
```

#### Level 3: Full System (CommandBus + Projections + EventBus)

```rust
// Production-ready setup with all components
use arc_core::{CommandBus, EventBus, EventStore, ProjectionEngine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize components
    let event_store = Box::new(SqliteEventStore::new("events.db").await?);
    let event_bus = Box::new(InProcessEventBus::new());
    let mut projection_engine = ProjectionEngine::new(event_store.clone());

    // Register projections
    projection_engine.register(Box::new(UserListProjection::new()));
    projection_engine.register(Box::new(AuditLogProjection::new()));

    // Subscribe projections to event bus
    event_bus.subscribe(Box::new(projection_engine)).await;

    // Subscribe side effects
    event_bus.subscribe(Box::new(WelcomeEmailHandler::new())).await;
    event_bus.subscribe(Box::new(WebSocketNotifier::new())).await;

    // Create command bus
    let mut command_bus = CommandBus::<UserAggregate>::new(
        event_store.clone(),
        event_bus.clone(),
    );

    // Execute command
    let command = UserCommand::CreateUser {
        id: Uuid::new_v4().to_string(),
        name: "Alice".to_string(),
        email: "alice@example.com".to_string(),
        password: "SecurePass123".to_string(),
    };

    let events = command_bus.dispatch(command).await?;
    println!("Created user with {} events", events.len());

    Ok(())
}
```

### 3.2 Common Use Cases with Complete Examples

#### Example: User Registration Flow

```rust
// Complete example: User registration with validation, events, and projections

// 1. Define domain types
pub enum UserCommand {
    Register {
        email: String,
        password: String,
        name: String,
    },
}

pub enum UserEvent {
    UserRegistered {
        id: String,
        email: String,
        name: String,
        password_hash: String,
        registered_at: DateTime<Utc>,
    },
}

// 2. Implement aggregate
impl Aggregate for UserAggregate {
    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            UserCommand::Register { email, password, name } => {
                // Validation
                if !is_valid_email(&email) {
                    return Err(UserError::InvalidEmail { email });
                }

                if password.len() < 8 {
                    return Err(UserError::WeakPassword);
                }

                // Generate ID
                let id = Uuid::new_v4().to_string();

                // Hash password
                let password_hash = argon2::hash_encoded(
                    password.as_bytes(),
                    &random_salt(),
                    &Config::default(),
                )?;

                // Create event
                Ok(vec![Event::new(NewEvent {
                    aggregate_type: "User",
                    aggregate_id: &id,
                    sequence: 1,
                    event_type: "UserRegistered",
                    payload: json!(UserEvent::UserRegistered {
                                                id: id.clone(),
                                                email,
                                                name,
                                                password_hash,
                                                registered_at: Utc::now(),
                                            }),
                })])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        if let "UserRegistered" = event.event_type.as_str() {
            let data: UserEvent = serde_json::from_value(event.payload.clone()).unwrap();
            if let UserEvent::UserRegistered { id, email, name, .. } = data {
                self.id = Some(id);
                self.email = Some(email);
                self.name = Some(name);
                self.created = true;
                self.version = event.sequence;
            }
        }
    }
}

// 3. Create projection for user list
pub struct UserListProjection {
    pool: diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<SqliteConnection>>,
}

impl Projection for UserListProjection {
    fn name(&self) -> &str {
        "UserList"
    }

    fn handles(&self) -> Vec<String> {
        vec!["UserRegistered".to_string()]
    }

    async fn handle(&mut self, event: &Event) -> Result<(), Box<dyn Error>> {
        let data: UserEvent = serde_json::from_value(event.payload.clone())?;

        if let UserEvent::UserRegistered { id, email, name, registered_at, .. } = data {
            let mut conn = self.pool.get()?;

            diesel::sql_query(
                "INSERT INTO users_view (id, email, name, created_at) VALUES (?1, ?2, ?3, ?4)"
            )
            .bind::<Text, _>(&id)
            .bind::<Text, _>(&email)
            .bind::<Text, _>(&name)
            .bind::<BigInt, _>(registered_at.timestamp())
            .execute(&mut conn)?;
        }

        Ok(())
    }

    async fn clear(&mut self) -> Result<(), Box<dyn Error>> {
        let mut conn = self.pool.get()?;
        diesel::sql_query("DELETE FROM users_view").execute(&mut conn)?;
        Ok(())
    }
}

// 4. Add side effect: welcome email
pub struct WelcomeEmailHandler {
    smtp_client: SmtpClient,
}

impl EventHandler for WelcomeEmailHandler {
    fn handles(&self) -> Vec<String> {
        vec!["UserRegistered".to_string()]
    }

    async fn handle(&self, event: &Event) -> Result<(), Box<dyn Error>> {
        let data: UserEvent = serde_json::from_value(event.payload.clone())?;

        if let UserEvent::UserRegistered { email, name, .. } = data {
            self.smtp_client.send_email(
                &email,
                "Welcome to Arc!",
                &format!("Hello {}, welcome to our platform!", name),
            ).await?;
        }

        Ok(())
    }
}
```

---

## 4. Common Pitfalls

### 4.1 Concurrency Issues

#### Pitfall: Ignoring Optimistic Concurrency

```rust
// ❌ BAD: No version check - concurrent commands can conflict
event_store.append(aggregate_id, None, events).await?;

// ✅ GOOD: Always pass expected version
let current_version = aggregate.version();
event_store.append(aggregate_id, Some(current_version), events).await?;
```

**Why it matters**: Two concurrent commands on the same aggregate can produce conflicting state. Always use optimistic concurrency control.

**How to handle violations**:
```rust
match event_store.append(aggregate_id, Some(current_version), events).await {
    Ok(_) => Ok(()),
    Err(EventStoreError::ConcurrencyViolation { .. }) => {
        // Reload aggregate and retry
        let fresh_aggregate = reload_aggregate(aggregate_id).await?;
        let events = fresh_aggregate.handle(command).await?;
        event_store.append(aggregate_id, Some(fresh_aggregate.version()), events).await?;
        Ok(())
    }
    Err(e) => Err(e),
}
```

### 4.2 Event Versioning

#### Pitfall: Breaking Event Schema Changes

```rust
// ❌ BAD: Removing fields breaks replay
#[derive(Serialize, Deserialize)]
pub struct UserCreated {
    pub id: String,
    pub email: String,
    // Removed: pub name: String,  // Old events have this!
}

// ✅ GOOD: Use Option for removed fields
#[derive(Serialize, Deserialize)]
pub struct UserCreated {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,  // Backward compatible
}
```

**Event versioning strategy**:
```rust
// Version events explicitly
#[derive(Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum UserCreatedEvent {
    #[serde(rename = "v1")]
    V1 {
        id: String,
        email: String,
    },
    #[serde(rename = "v2")]
    V2 {
        id: String,
        email: String,
        name: String,
    },
}

// Upcasting in aggregate apply
fn apply(&mut self, event: &Event) {
    match serde_json::from_value::<UserCreatedEvent>(event.payload.clone()) {
        Ok(UserCreatedEvent::V1 { id, email }) => {
            // Upcast V1 -> V2
            self.id = Some(id);
            self.email = Some(email);
            self.name = Some("Unknown".to_string());  // Default
        }
        Ok(UserCreatedEvent::V2 { id, email, name }) => {
            self.id = Some(id);
            self.email = Some(email);
            self.name = Some(name);
        }
        Err(e) => {
            tracing::error!("Failed to deserialize UserCreated: {}", e);
        }
    }
}
```

### 4.3 Projection Rebuild Performance

#### Pitfall: Rebuilding Large Projections in One Query

```rust
// ❌ BAD: Loads all events into memory at once
pub async fn rebuild(&mut self) -> Result<(), Box<dyn Error>> {
    self.clear().await?;
    let all_events = self.event_store.stream_all(0).await?;  // Could be millions!
    for event in all_events {
        self.handle(&event).await?;
    }
    Ok(())
}

// ✅ GOOD: Stream events in batches
pub async fn rebuild(&mut self) -> Result<(), Box<dyn Error>> {
    self.clear().await?;

    let batch_size = 1000;
    let mut position = 0;

    loop {
        let events = self.event_store.stream_all(position).await?;
        if events.is_empty() {
            break;
        }

        for event in &events {
            self.handle(event).await?;
        }

        position += events.len() as i64;

        // Progress indicator
        tracing::info!("Rebuilt {} events", position);
    }

    Ok(())
}
```

### 4.4 Aggregate Boundary Design

#### Pitfall: Aggregates That Are Too Large

```rust
// ❌ BAD: One aggregate for entire order + line items + payments
pub struct OrderAggregate {
    order_id: String,
    line_items: Vec<LineItem>,      // Could be 100s
    payments: Vec<Payment>,         // Could be many
    shipments: Vec<Shipment>,       // Could be many
    // ... thousands of events to replay!
}

// ✅ GOOD: Separate aggregates with eventual consistency
pub struct OrderAggregate {
    order_id: String,
    status: OrderStatus,
    total: Decimal,
    // References only
    line_item_ids: Vec<String>,
    payment_ids: Vec<String>,
}

pub struct LineItemAggregate {
    id: String,
    order_id: String,  // Reference
    product_id: String,
    quantity: i32,
}

pub struct PaymentAggregate {
    id: String,
    order_id: String,  // Reference
    amount: Decimal,
    status: PaymentStatus,
}
```

**Rule of thumb**: If an aggregate has more than ~100 events, consider splitting it.

### 4.5 Event Payload Design

#### Pitfall: Storing References Instead of Values

```rust
// ❌ BAD: Storing only IDs - loses history if related data changes
pub struct OrderPlaced {
    order_id: String,
    user_id: String,          // What if user is deleted?
    product_ids: Vec<String>, // What if product price changes?
}

// ✅ GOOD: Capture all relevant data at event time
pub struct OrderPlaced {
    order_id: String,
    user_id: String,
    user_email: String,       // Snapshot user data
    user_name: String,
    items: Vec<OrderItem>,    // Snapshot product data
}

#[derive(Serialize, Deserialize)]
pub struct OrderItem {
    product_id: String,
    product_name: String,     // Value at time of order
    price: Decimal,           // Price at time of order
    quantity: i32,
}
```

**Principle**: Events are immutable historical records. Capture all data needed to reconstruct the decision.

---

## 5. IDE Support Considerations

### 5.1 Trait Design for Autocomplete

Use associated types instead of generic parameters when possible for better IDE inference:

```rust
// ✅ GOOD: Associated types - IDE can infer
pub trait Aggregate {
    type Command: Command;
    type Event;
    type Error: Error;

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error>;
}

// ❌ LESS GOOD: Generic parameters require explicit annotation
pub trait Aggregate<C, E, Err>
where
    C: Command,
    Err: Error,
{
    async fn handle(&self, command: C) -> Result<Vec<Event>, Err>;
}
```

### 5.2 Builder Pattern for Complex Types

Provide builders for complex event/command creation:

```rust
// ✅ GOOD: Builder pattern for complex events
pub struct UserCreatedEventBuilder {
    id: Option<String>,
    email: Option<String>,
    name: Option<String>,
    password_hash: Option<String>,
}

impl UserCreatedEventBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    pub fn build(self) -> Result<UserCreatedEvent, ValidationError> {
        Ok(UserCreatedEvent {
            id: self.id.ok_or(ValidationError::MissingField("id"))?,
            email: self.email.ok_or(ValidationError::MissingField("email"))?,
            name: self.name.ok_or(ValidationError::MissingField("name"))?,
            password_hash: self.password_hash.ok_or(ValidationError::MissingField("password_hash"))?,
            created_at: Utc::now(),
        })
    }
}

// Usage with great IDE support
let event = UserCreatedEventBuilder::new()
    .id("user-123")
    .email("user@example.com")
    .name("Alice")
    .build()?;
```

### 5.3 Documentation Comments

Use doc comments with examples that actually compile:

```rust
/// Append events to the event store with optimistic concurrency control.
///
/// # Arguments
///
/// * `aggregate_id` - The unique identifier of the aggregate
/// * `expected_version` - The expected current version (None if new aggregate)
/// * `events` - The events to append
///
/// # Returns
///
/// * `Ok(())` on success
/// * `Err(EventStoreError::ConcurrencyViolation)` if version mismatch
/// * `Err(EventStoreError::SerializationFailed)` if event serialization fails
///
/// # Example
///
/// ```rust
/// use arc_core::{Event, EventStore, SqliteEventStore};
/// use serde_json::json;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let event_store = SqliteEventStore::new("events.db").await?;
///
/// let event = Event::new(NewEvent {
///                 aggregate_type: "User",
///                 aggregate_id: "user-123",
///                 sequence: 1,
///                 event_type: "UserCreated",
///                 payload: json!({"name": "Alice"}),
///             });
///
/// // First event - no version check needed
/// event_store.append("user-123", None, vec![event]).await?;
///
/// // Subsequent events - require version check
/// let event2 = Event::new(NewEvent {
    aggregate_type: "User",
    aggregate_id: "user-123",
    sequence: 2,
    event_type: "ProfileUpdated",
    payload: json!({"name": "Bob"}),
});
/// event_store.append("user-123", Some(1), vec![event2]).await?;
/// # Ok(())
/// # }
/// ```
pub async fn append(
    &self,
    aggregate_id: &str,
    expected_version: Option<i64>,
    events: Vec<Event>,
) -> Result<(), EventStoreError>;
```

### 5.4 Type Aliases for Common Patterns

Reduce boilerplate with well-named type aliases:

```rust
// ✅ GOOD: Type aliases for common patterns
pub type EventResult<T = ()> = Result<T, EventStoreError>;
pub type CommandResult<T = Vec<Event>> = Result<T, Box<dyn Error>>;
pub type ProjectionResult<T = ()> = Result<T, ProjectionError>;

// Usage
async fn append(&self, events: Vec<Event>) -> EventResult {
    // ...
}

async fn handle(&self, command: Self::Command) -> CommandResult {
    // ...
}
```

---

## 6. API Design Principles

### 6.1 Principle of Least Surprise

APIs should behave as developers expect based on their names:

```rust
// ✅ GOOD: Method does exactly what name implies
async fn load(&self, aggregate_id: &str) -> Result<Vec<Event>>;

// ❌ BAD: Method has side effects not implied by name
async fn load(&self, aggregate_id: &str) -> Result<Vec<Event>> {
    // Unexpected: also rebuilds projections!
    self.rebuild_projections().await?;
    // ...
}
```

### 6.2 Make the Right Thing Easy, Wrong Thing Hard

Use the type system to prevent mistakes:

```rust
// ✅ GOOD: Type system prevents using sequence without aggregate_id
pub struct Event {
    event_id: Uuid,
    aggregate_type: String,
    aggregate_id: String,     // Required
    sequence: i64,            // Must match aggregate
    // ...
}

impl Event {
    pub fn new(
        aggregate_type: impl Into<String>,
        aggregate_id: impl Into<String>,  // Can't forget this
        sequence: i64,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        // Constructor enforces relationship
        Self {
            event_id: Uuid::new_v4(),
            aggregate_type: aggregate_type.into(),
            aggregate_id: aggregate_id.into(),
            sequence,
            event_type: event_type.into(),
            payload,
            metadata: json!({}),
            timestamp: SystemTime::now(),
        }
    }
}

// ❌ BAD: Can create invalid states
pub struct Event {
    pub event_id: Uuid,
    pub aggregate_id: Option<String>,  // Optional = can forget
    pub sequence: i64,
    // ...
}
```

### 6.3 Consistent Return Types

Use consistent patterns for similar operations:

```rust
// ✅ GOOD: All store operations return Result<T, EventStoreError>
pub trait EventStore {
    async fn append(&self, events: Vec<Event>) -> Result<(), EventStoreError>;
    async fn load(&self, id: &str) -> Result<Vec<Event>, EventStoreError>;
    async fn stream_all(&self, from: i64) -> Result<Vec<Event>, EventStoreError>;
}

// ❌ BAD: Inconsistent error types
pub trait EventStore {
    async fn append(&self, events: Vec<Event>) -> Result<(), Box<dyn Error>>;
    async fn load(&self, id: &str) -> Result<Vec<Event>, String>;
    async fn stream_all(&self, from: i64) -> Vec<Event>;  // No error handling!
}
```

### 6.4 Provide Both High-Level and Low-Level APIs

```rust
// Low-level: Full control
pub trait EventStore {
    async fn append(&self, aggregate_id: &str, expected_version: Option<i64>, events: Vec<Event>) -> Result<()>;
}

// High-level: Convenience
impl EventStore {
    /// Append a single event without version check (use for first event only)
    pub async fn append_new(&self, aggregate_id: &str, event: Event) -> Result<()> {
        self.append(aggregate_id, None, vec![event]).await
    }

    /// Append events with automatic version detection
    pub async fn append_auto(&self, aggregate_id: &str, events: Vec<Event>) -> Result<()> {
        let current_events = self.load(aggregate_id).await?;
        let current_version = current_events.last().map(|e| e.sequence);
        self.append(aggregate_id, current_version, events).await
    }
}
```

---

## 7. Testing Patterns

### 7.1 Provide Test Utilities

Include test helpers in the library:

```rust
// In arc-core/src/testing.rs
pub mod testing {
    use super::*;

    /// In-memory event store for testing (no DB required)
    pub struct InMemoryEventStore {
        events: Arc<Mutex<HashMap<String, Vec<Event>>>>,
    }

    impl InMemoryEventStore {
        pub fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        /// Get all events for inspection in tests
        pub async fn get_all_events(&self) -> Vec<Event> {
            self.events
                .lock()
                .await
                .values()
                .flatten()
                .cloned()
                .collect()
        }
    }

    /// Test fixture builder
    pub struct AggregateFixture<A: Aggregate> {
        events: Vec<Event>,
        _phantom: PhantomData<A>,
    }

    impl<A: Aggregate> AggregateFixture<A> {
        pub fn new() -> Self {
            Self {
                events: Vec::new(),
                _phantom: PhantomData,
            }
        }

        pub fn given(mut self, event: Event) -> Self {
            self.events.push(event);
            self
        }

        pub async fn when(self, command: A::Command) -> TestResult<A> {
            let aggregate = A::from_events(self.events.clone());
            let result = aggregate.handle(command).await;
            TestResult {
                given: self.events,
                result,
            }
        }
    }

    pub struct TestResult<A: Aggregate> {
        pub given: Vec<Event>,
        pub result: Result<Vec<Event>, A::Error>,
    }

    impl<A: Aggregate> TestResult<A> {
        pub fn then_expect_events(self, expected: Vec<Event>) {
            assert!(self.result.is_ok());
            let actual = self.result.unwrap();
            assert_eq!(actual.len(), expected.len());
            // Deep comparison...
        }

        pub fn then_expect_error<F>(self, check: F)
        where
            F: FnOnce(&A::Error) -> bool
        {
            assert!(self.result.is_err());
            let error = self.result.unwrap_err();
            assert!(check(&error), "Error didn't match expectation");
        }
    }
}
```

### 7.2 Example Test Usage

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use arc_core::testing::*;

    #[tokio::test]
    async fn test_user_creation() {
        // Given: No prior events
        AggregateFixture::<UserAggregate>::new()
            // When: CreateUser command
            .when(UserCommand::CreateUser {
                id: "user-123".to_string(),
                name: "Alice".to_string(),
                email: "alice@example.com".to_string(),
                password: "SecurePass123".to_string(),
            })
            .await
            // Then: UserCreated event
            .then_expect_events(vec![
                Event::new(NewEvent {
                    aggregate_type: "User",
                    aggregate_id: "user-123",
                    sequence: 1,
                    event_type: "UserCreated",
                    payload: json!({
                                                "id": "user-123",
                                                "name": "Alice",
                                                "email": "alice@example.com",
                                            }),
                }),
            ]);
    }

    #[tokio::test]
    async fn test_duplicate_user_creation_fails() {
        // Given: User already created
        AggregateFixture::<UserAggregate>::new()
            .given(Event::new(NewEvent {
                aggregate_type: "User",
                aggregate_id: "user-123",
                sequence: 1,
                event_type: "UserCreated",
                payload: json!({
                                        "id": "user-123",
                                        "email": "alice@example.com",
                                    }),
            }))
            // When: Try to create again
            .when(UserCommand::CreateUser {
                id: "user-123".to_string(),
                name: "Alice".to_string(),
                email: "alice@example.com".to_string(),
                password: "SecurePass123".to_string(),
            })
            .await
            // Then: Error
            .then_expect_error(|e| matches!(e, UserError::AlreadyExists { .. }));
    }
}
```

---

## 8. Migration and Versioning

### 8.1 Semantic Versioning for Events

Track event schema versions explicitly:

```rust
// Event metadata includes version
pub struct Event {
    pub event_id: Uuid,
    pub event_type: String,
    pub event_version: u32,  // Schema version
    pub payload: serde_json::Value,
    // ...
}

// Domain event types are versioned
#[derive(Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum UserCreatedEvent {
    #[serde(rename = "1")]
    V1 { id: String, email: String },

    #[serde(rename = "2")]
    V2 { id: String, email: String, name: String },

    #[serde(rename = "3")]
    V3 { id: String, email: String, name: String, metadata: HashMap<String, String> },
}
```

### 8.2 Migration Strategies

Provide tools for safe event migration:

```rust
pub trait EventMigration: Send + Sync {
    /// From version
    fn from_version(&self) -> u32;

    /// To version
    fn to_version(&self) -> u32;

    /// Migrate event payload
    fn migrate(&self, event: Event) -> Result<Event, MigrationError>;
}

// Example migration
pub struct UserCreatedV1ToV2Migration;

impl EventMigration for UserCreatedV1ToV2Migration {
    fn from_version(&self) -> u32 { 1 }
    fn to_version(&self) -> u32 { 2 }

    fn migrate(&self, mut event: Event) -> Result<Event, MigrationError> {
        let mut payload = event.payload.as_object_mut().ok_or(MigrationError::InvalidPayload)?;

        // Add default name field
        if !payload.contains_key("name") {
            payload.insert("name".to_string(), json!("Unknown"));
        }

        event.event_version = 2;
        Ok(event)
    }
}
```

---

## Summary Checklist

When implementing event sourcing features, ensure:

- [ ] **Naming** follows consistent DDD terminology (Command, Event, Aggregate, Projection)
- [ ] **Errors** are explicit with context and actionable messages
- [ ] **Examples** progress from simple to complex (Level 1, 2, 3)
- [ ] **Documentation** includes working code examples that compile
- [ ] **Concurrency** uses optimistic locking with version checks
- [ ] **Events** are versioned and backward compatible
- [ ] **Projections** rebuild in batches for large event streams
- [ ] **Aggregates** are sized appropriately (< 100 events ideally)
- [ ] **Event payloads** capture values, not just references
- [ ] **Type system** prevents common mistakes at compile time
- [ ] **Test utilities** are provided for easy testing
- [ ] **IDE support** via associated types, builders, and good docs
- [ ] **APIs** follow principle of least surprise
- [ ] **Return types** are consistent across similar operations

---

## References

- Implementation Guide: `/docs/resources.md`
- Architecture Overview: `/docs/command-bus.md`
- Plugin System Plan: `/plugin-system-plan.md`
- Rust API Guidelines: https://rust-lang.github.io/api-guidelines/

---

**Next Steps for Implementers:**

1. Review these guidelines before starting implementation
2. Create issue templates based on common pitfalls section
3. Set up linting rules to enforce naming conventions
4. Build test fixtures following testing patterns
5. Write documentation examples that compile and run
6. Validate error messages are helpful in real usage
7. Get feedback from plugin authors on API ergonomics
