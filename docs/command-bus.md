# Command Bus

The command bus coordinates a write: it loads the correct aggregate, sends it the command, saves the resulting events, and starts the configured event and projection flow.

## Where it fits

```text
HTTP route
  → CommandBus<ProductAggregate>
  → ProductAggregate
  → EventStore
  → EventBus
  → Projectors and external handlers
```

The route translates an incoming request into a command. The aggregate decides whether that command is allowed and returns events describing what happened. The command bus coordinates everything around that decision.

## What `dispatch` does

```rust
bus.dispatch(product_command, context).await
```

For one dispatch, the command bus:

1. Uses the command's aggregate ID to load that aggregate's event stream.
2. Rebuilds the aggregate's current state from its snapshot and events.
3. Calls the aggregate's `handle` method with the command.
4. Adds audit information from the command context.
5. Saves the new events with an optimistic version check.
6. Publishes the saved events through the configured event bus.
7. Creates a snapshot when the aggregate's snapshot policy requires one.

If the aggregate rejects the command or saving fails, `dispatch` returns an error.

## Why it exists

Controllers should handle HTTP concerns, while aggregates should enforce business rules. The command bus connects those layers and ensures every write follows the same persistence, concurrency, audit, event-publishing, and snapshot process.

## Why it has an aggregate type

```rust
CommandBus<ProductAggregate>
CommandBus<OrderAggregate>
```

Each bus accepts only the command type belonging to its aggregate. Rust therefore prevents a Product command from being sent to the Order aggregate.

Arc creates one typed command bus for every aggregate registered with the server:

```rust
ArcApp::builder()
    .register_aggregate::<ProductAggregate>()
    .register_aggregate::<OrderAggregate>()
```

Routes receive the bus they need through Actix:

```rust
async fn create_product(
    bus: web::Data<CommandBus<ProductAggregate>>,
) {
    bus.dispatch(product_command, context).await;
}
```

> **Note:** [`web::Data`](project-structure.md#actix-shared-application-data) gives the route access to the shared Product command bus that Arc created when the server started.

## Implementation

The framework implementation is in [`crates/arc-core/src/command_bus.rs`](https://github.com/lotharthesavior/arc/blob/master/crates/arc-core/src/command_bus.rs). Look for `CommandBus<A>` and its `dispatch` method.

Next, follow a complete request through [Application Workflows](workflows.md).
