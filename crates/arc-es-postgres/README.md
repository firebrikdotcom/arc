# arc-es-postgres

Postgres storage adapters for Arc event sourcing.

This crate implements Arc's `EventStore`, `ReadModelStore`, and snapshot persistence on Postgres.
It is intended for production deployments and multi-process Arc applications.

## What It Provides

- `PostgresEventStore`: append-only event store with optimistic concurrency.
- `PostgresReadModelStore`: JSONB read-model storage for projections. Event-log
  payloads use PostgreSQL `JSON` so numeric JSON text round-trips exactly for
  strict persisted-event equality.
- Self-initializing schema helpers for events, snapshots, and read models.
- An idempotent forward migration converts legacy `events.payload` JSONB to
  JSON with `USING payload::json`; `initialize_schema()` runs it for existing
  databases and fresh schemas converge on JSON. It never rewrites integrity
  signatures. Historical JSONB normalization may already have invalidated some
  signatures; those events fail closed and must not be resealed.
- Optional HMAC integrity-chain enforcement for persisted events.
- Optional `WriteGuard`: an application check that runs inside every event-store
  write transaction (see [Write Guard](#write-guard)).

## Example

```rust
use arc_core::audit::AuditMetadata;
use arc_core::event::{Event, NewEvent};
use arc_core::event_store::{EventStore, VersionCheck};
use arc_es_postgres::PostgresEventStore;
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let store = PostgresEventStore::new("postgres://arc:password@localhost:5432/arc").await?;
store.initialize_schema().await?;

let event = Event::new(NewEvent {
    aggregate_type: "User",
    aggregate_id: "user-123",
    sequence: 1,
    event_type: "UserRegistered",
    payload: json!({ "email": "ada@example.test" }),
})
.with_audit(AuditMetadata::system());

store
    .append("user-123", VersionCheck::New, vec![event])
    .await?;
# Ok(())
# }
```

## Integrity Mode

Use `PostgresEventStore::new_with_integrity_key` or
`PostgresEventStore::with_pool_and_integrity_key` to sign new events and verify events on
load/stream. Existing unsigned data must be replayed or migrated before enabling integrity
verification.

## Write Guard

`PostgresEventStore::with_write_guard` installs a `WriteGuard` that makes every write
conditional on database state your application owns, atomically with the write. It chains
after any constructor; calling it again replaces the guard, and clones share it. Without a
guard the store behaves exactly as before.

```rust
use arc_core::event_store::{EventStoreError, EventStoreResult};
use arc_es_postgres::{PostgresEventStore, WriteGuard};
use async_trait::async_trait;
use std::sync::Arc;

/// Refuses writes once another process has advanced the writer epoch.
struct EpochGuard {
    epoch: i64,
}

#[async_trait]
impl WriteGuard for EpochGuard {
    async fn check(&self, conn: &mut sqlx::PgConnection) -> EventStoreResult<()> {
        // FOR SHARE: concurrent writers do not block each other, but the epoch
        // cannot be advanced until this write has committed or rolled back.
        let current: i64 =
            sqlx::query_scalar("SELECT epoch FROM writer_epoch WHERE id = 1 FOR SHARE")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| EventStoreError::database(e.to_string()))?;
        if current != self.epoch {
            return Err(EventStoreError::other("writer epoch superseded"));
        }
        Ok(())
    }
}

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let store = PostgresEventStore::new("postgres://arc:password@localhost:5432/arc")
    .await?
    .with_write_guard(Arc::new(EpochGuard { epoch: 7 }));
# Ok(())
# }
```

The guard receives a `sqlx_postgres::PgConnection`, which is the type the `sqlx` 0.8 facade
re-exports as `sqlx::PgConnection`; likewise `with_pool` takes the pool `sqlx::PgPool` names.
An application may depend on `sqlx` 0.8 with the `postgres` feature, as above, or on
`sqlx-core` and `sqlx-postgres` directly, which keeps the facade's unused MySQL and SQLite
drivers out of its `Cargo.lock`.

Semantics:

- The guard runs once per write transaction: once per `append`, `append_to` or
  `append_to_with_publication` call regardless of event count, once per
  `acknowledge_publication`, and once per `save_snapshot`.
- It is not called for empty batches or batches rejected by audit validation (no
  transaction is opened), for reads, or for `initialize_schema`.
  `PostgresReadModelStore` is not guarded.
- It is the first statement after `BEGIN` on the write's own connection. Locks it takes are
  held until Arc commits or rolls back, and anything it writes commits or rolls back with
  the write.
- On `Err` nothing is persisted and the guard's error is returned unchanged. A guard
  failure takes precedence over `ConcurrencyConflict` and `InvalidSequence`.
- With a guard installed, publication acknowledgements and snapshot saves run in an
  explicit transaction (guard, statement, commit) rather than one auto-commit statement.
- A guard must not commit, roll back, issue `SET TRANSACTION`, or leave session state
  changed. It should avoid returning `ConcurrencyConflict`, which callers may treat as
  retryable. It runs under the pooled connection's own timeouts; Arc adds none.

## Stability

This crate is pre-1.0 and follows `arc-core`'s compatibility line. Breaking changes may ship in
minor versions until `1.0`.

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
