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

SQLite admits one writer per database file. In the default rollback-journal mode a commit also
excludes readers; in WAL mode readers and writers do not block each other. Connections built by
the stores wait up to 5 seconds for the file (`busy_timeout`); pools passed to `with_pool` keep
the caller's setting. Journal mode is left as the database has it (this crate does not enable
WAL).

SQLite's wait is a poll and is not fair: under sustained contention one connection can lose
every retry and fail with `database is locked` although nobody held the file for long. The
stores therefore queue among themselves, per database file, before they enter SQLite:

- writers take turns first-in, first-out, each for one `BEGIN IMMEDIATE` transaction;
- in rollback-journal mode, reads run together and beside a writer until it commits; a commit
  gives the reads already running 20 ms to finish, and reads that arrive during it all run
  before the next commit begins;
- in WAL mode reads are not queued and commits do not wait for them.

This covers `SqliteEventStore`, `SqliteReadModelStore`, `SqliteSessionStore` and
`SqliteAccessLogger` (which only writes), across all stores and pools in the process that open
the same file, whatever spelling of the path they were given. The queue is held on the blocking
thread for one operation and never across an `await`.

Limits:

- Fairness is per process. Another process, or a connection opened outside these stores on the
  same file, still competes through SQLite's busy timeout, and an operation can fail with
  `database is locked` when such a lock outlasts it. A writer always makes its own attempt; the
  time it spent queued behind attempts that such a lock refused is taken off the busy timeout
  of that attempt's wait for the write lock, so queueing does not multiply the wait. Its
  commit keeps the full timeout.
- Connections outside these stores on the same file (`arc-auth-db`, an application's own pool,
  `arc-web`'s `get_connection`) get fewer turns under a write-heavy load on a fast file system
  than they did before the queue: store writers hand the write slot to each other directly, so
  the file is seldom free when an outside connection polls for it. Measured on tmpfs with four
  store writers against one outside writer for 8 seconds: 56 outside writes instead of 231 with
  a 5-second busy timeout; with 500 ms, 62 writes and 10 `database is locked` instead of 335
  and 1. Give such connections a generous busy timeout, or keep heavy writers off a shared
  file.
- When an outside lock is held longer than the busy timeout, each store writer is held to its
  own timeout, so more callers can see `database is locked` than before the queue (10 of 10
  instead of 2 of 10 were measured for a 700 ms lock with a 300 ms timeout).
- The queue orders callers and leaves the locking to SQLite. A read that is still running after
  the 20 ms (a long read, or one waiting for a transaction that outgrew the page cache and
  holds the file exclusively) is arbitrated by SQLite's busy timeout as before.
- A queued writer waits for the writers ahead of it without a deadline of its own; each of
  those is bounded by its busy timeout.
- The journal mode is read once, by a store's first operation. A database switched to WAL
  while stores are open keeps its reads queued until the stores are reopened.
- In-memory and temporary databases have no file and are not queued.
- Two hard links to one database file count as two files (SQLite does not support that either).

## Stability

This crate is pre-1.0 and follows `arc-core`'s compatibility line. Breaking changes may ship in
minor versions until `1.0`.

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
