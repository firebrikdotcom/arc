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

## Stability

This crate is pre-1.0 and follows `arc-core`'s compatibility line. Breaking changes may ship in
minor versions until `1.0`.

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
