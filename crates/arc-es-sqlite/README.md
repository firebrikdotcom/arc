# arc-es-sqlite

SQLite storage adapters for Arc event sourcing.

This crate implements Arc's `EventStore`, `ReadModelStore`, snapshot persistence, and JWT session
revocation storage on SQLite. It is intended for local development, embedded deployments, tests,
and single-node applications.

## What It Provides

- `SqliteEventStore`: append-only event store with optimistic concurrency.
- `SqliteReadModelStore`: JSON read-model table storage for projections.
- `SqliteSessionStore`: server-side JWT/session revocation store.
- Snapshot persistence through the `EventStore` snapshot methods.
- Optional HMAC integrity-chain enforcement for persisted events.

## Example

```rust
use arc_core::audit::AuditMetadata;
use arc_core::event::{Event, NewEvent};
use arc_core::event_store::{EventStore, VersionCheck};
use arc_es_sqlite::SqliteEventStore;
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let store = SqliteEventStore::new("database/arc_dev.db").await?;
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

Use `SqliteEventStore::new_with_integrity_key` or
`SqliteEventStore::with_pool_and_integrity_key` to sign new events and verify events on load/stream.
Existing unsigned data must be replayed or migrated before enabling integrity verification.

## Stability

This crate is pre-1.0 and follows `arc-core`'s compatibility line. Breaking changes may ship in
minor versions until `1.0`.

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
