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

## Concurrency

SQLite admits one writer per database file, and in the default rollback-journal mode a commit
also excludes readers. Connections built by the stores wait up to 5 seconds for the file
(`busy_timeout`); pools passed to `with_pool` keep the caller's setting. Journal mode is left as
the database has it (WAL is not enabled by this crate).

SQLite's wait is a poll and is not fair: under sustained contention one connection can lose
every retry and fail with `database is locked` although nobody held the file for long. The
stores therefore queue among themselves, per database file, before they enter SQLite:

- writers take turns first-in, first-out, each for one `BEGIN IMMEDIATE` transaction;
- reads run together, and beside a writer until it commits;
- a commit waits for the reads already running, and reads that arrive during it all run before
  the next commit begins.

This covers `SqliteEventStore`, `SqliteReadModelStore`, `SqliteSessionStore` and
`SqliteAccessLogger`, across all stores and pools in the process that open the same file,
whatever spelling of the path they were given. The queue is held on the blocking thread for one
operation and never across an `await`.

Limits:

- Fairness is per process. Another process, or a connection opened outside these stores on the
  same file, still competes through SQLite's busy timeout, and an operation can fail with
  `database is locked` when such a lock outlasts it. A writer that queued behind one refused
  this way for the whole timeout fails with the same error rather than waiting a second timeout.
- A queued operation waits for those ahead of it without a deadline of its own; each of those is
  bounded by its busy timeout. A long read delays the commits behind it, as it would inside
  SQLite.
- In-memory and temporary databases have no file and are not queued.
- Two hard links to one database file count as two files (SQLite does not support that either).

## Stability

This crate is pre-1.0 and follows `arc-core`'s compatibility line. Breaking changes may ship in
minor versions until `1.0`.

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
