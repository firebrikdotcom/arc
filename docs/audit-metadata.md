# Audit Metadata (HIPAA-1)

> **Status:** implemented in HIPAA-1 (Step 1 addendum). Required reading before
> writing a controller, an aggregate, or any code that touches `EventStore`.

Every event persisted by the framework carries a typed `AuditMetadata` value.
This is a hard invariant: the store rejects writes whose audit fails
validation. The goal is HIPAA §164.312(b) Audit Controls — *who* changed
something, *when*, *from where*, and *because of what*.

This page covers:

1. The lifecycle of an audit stamp from HTTP request to durable storage
2. The data model and reserved actor identifiers
3. How to wire a new controller correctly
4. How to write tests against audited code
5. The migration story for pre-existing event rows

---

## 1. Lifecycle

The flow:

1. The HTTP request arrives. `audit_context::for_actor(req, actor_id)` (or
   `audit_context::anonymous(req)` for unauthenticated paths like
   `/register`) builds a `CommandContext` from the request — pulling
   `source_ip` from `realip_remote_addr()`, `user_agent` from headers, and
   `correlation_id` from the `X-Correlation-Id` header (or generating one).
2. The controller calls `command_bus.dispatch(cmd, ctx)`. Note the
   **mandatory** second argument — the type system refuses dispatch without
   a context.
3. The bus loads the aggregate, calls `Aggregate::handle()`, and gets back
   `Vec<Event>`. These events have `audit = AuditMetadata::pending()` — the
   aggregate never touches audit data.
4. The bus calls `ctx.to_audit()` which validates and stamps a fresh
   `timestamp_utc_us`. It then `event.with_audit(stamp)` for every produced
   event (all events from one command share one stamp).
5. The bus calls `EventStore::append`. Every store implementation calls
   `validate_audit_batch(...)` at the top — defense in depth. If the bus
   ever forgets to stamp, the store catches it and the write fails with
   `EventStoreError::InvalidAudit`.
6. Stamped events land in the `events` table with seven dedicated audit
   columns. `actor_id` and `correlation_id` are indexed.

### Defense in depth

Two boundaries validate the audit. The bus boundary should catch every real
problem. The storage boundary should never fire — but it does, because
"should never" is not a security guarantee. If a future contributor adds a
direct `event_store.append(...)` bypass (skipping the bus), the store still
refuses unaudited writes.

---

## 2. Data model

```rust
pub struct AuditMetadata {
    pub actor_id:         String,         // required, non-empty
    pub actor_session_id: Option<String>, // pair w/ HIPAA-4 session store
    pub source_ip:        Option<String>,
    pub user_agent:       Option<String>,
    pub timestamp_utc_us: i64,            // microseconds, > 0
    pub causation_id:     Option<Uuid>,   // triggering event_id
    pub correlation_id:   Uuid,           // request-scope id
}
```

### Reserved actor identifiers

| Sentinel              | When |
|-----------------------|------|
| `"system"`            | seeders, cron, migrations, internal jobs |
| `"anonymous"`         | unauthenticated requests (e.g. self-registration) |
| `"legacy-pre-hipaa"`  | rows backfilled by the HIPAA-1 migration |

Use `AuditMetadata::system()` for internal jobs; it synthesizes a fresh
`correlation_id` and never fails validation.

### Validation rules

`AuditMetadata::validate()` rejects:

- A pending placeholder (`AuditError::PendingNotStamped`)
- An empty or whitespace-only `actor_id` (`AuditError::EmptyActorId`)
- A non-positive `timestamp_utc_us` (`AuditError::InvalidTimestamp`)

The validator is intentionally narrow. It does not validate UUID shape (the
type system does) or enforce per-actor rules — those belong to authorization
and policy code, not the audit boundary.

---

## 3. Storage schema

The `events` table gained seven columns in migration
`2026-04-21-000002_add_hipaa_audit`:

```sql
ALTER TABLE events ADD COLUMN actor_id         TEXT NOT NULL DEFAULT 'legacy-pre-hipaa';
ALTER TABLE events ADD COLUMN actor_session_id TEXT;
ALTER TABLE events ADD COLUMN source_ip        TEXT;
ALTER TABLE events ADD COLUMN user_agent       TEXT;
ALTER TABLE events ADD COLUMN timestamp_utc_us BIGINT NOT NULL DEFAULT 0;
ALTER TABLE events ADD COLUMN causation_id     TEXT;
ALTER TABLE events ADD COLUMN correlation_id   TEXT NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000000';

UPDATE events SET timestamp_utc_us = timestamp * 1000000 WHERE timestamp_utc_us = 0;

CREATE INDEX idx_events_actor_id       ON events(actor_id);
CREATE INDEX idx_events_correlation_id ON events(correlation_id);

ALTER TABLE events DROP COLUMN metadata;
```

Why these two indices: the two queries audit reports actually run are
**"every event by actor X"** and **"every event in request Y."** Other audit
queries (by aggregate, by event_type, by time range) already use existing
indices.

The free-form `metadata` JSON column is removed; typed audit columns
replace it.

---

## 4. Wiring a new controller

```rust
use crate::helpers::audit_context;

#[post("/things/{id}/do-stuff")]
pub async fn do_stuff(
    http_req: HttpRequest,
    path: web::Path<String>,
    command_bus: web::Data<CommandBus<ThingAggregate>>,
) -> impl Responder {
    // Authenticated path: the JWT middleware put the actor's UUID into req extensions.
    let actor_id = match http_req.extensions().get::<String>() {
        Some(id) => id.clone(),
        None => return HttpResponse::Unauthorized().finish(),
    };

    let ctx = audit_context::for_actor(&http_req, actor_id);
    let cmd = ThingCommand::DoStuff { id: path.into_inner() };

    match command_bus.dispatch(cmd, ctx).await {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(e) => AppError::from(e).error_response(),
    }
}
```

Things to *not* do:

- **Do not** call `event_store.append(...)` directly from a controller. The
  bus stamping is what makes audit a hard invariant.
- **Do not** synthesize a `CommandContext::for_actor("system")` from a user
  request to "make the audit pass." That destroys the audit trail. Use the
  authenticated UUID; if there's no user, use `audit_context::anonymous`.
- **Do not** mutate `event.audit` after `dispatch` returns. Treat it as
  immutable. The event ID and audit form the durable record.

---

## 5. Testing audited code

The `arc-core` crate exposes `AuditMetadata::test_default()` and
`InMemoryEventStore` behind the `test-utils` feature flag. Both
`arc-app` and `arc-es-sqlite` enable the feature in their
`[dev-dependencies]`.

### Building stamped events in unit tests

```rust
use arc_core::audit::AuditMetadata;
use arc_core::event::{Event, NewEvent};
use serde_json::json;

let event = Event::new(NewEvent {
    aggregate_type: "User",
    aggregate_id: "u1",
    sequence: 1,
    event_type: "UserCreated",
    payload: json!({}),
})
    .with_audit(AuditMetadata::test_default());
```

### Dispatching commands in unit tests

```rust
use arc_core::command_bus::{CommandBus, CommandContext};
use arc_core::event_store::InMemoryEventStore;

let bus = CommandBus::<MyAgg>::new(
    Box::new(InMemoryEventStore::new()),
    Box::new(InProcessEventBus::new()),
);
bus.dispatch(my_command, CommandContext::for_actor("test-actor")).await?;
```

### Asserting the audit landed

```rust
let events = store.load("u1").await?;
assert_eq!(events[0].audit.actor_id, "alice-uuid");
assert!(!events[0].audit.is_pending());
assert!(events[0].audit.timestamp_utc_us > 0);
```

### HTTP integration tests

The existing `api_controller::tests` module shows the pattern: spin up a
`SqliteEventStore` against `:memory:`, mount the routes, send requests with
`actix_web::test::TestRequest`, and load events back from the store to
verify audit fields. Three tests at the bottom of that module specifically
exercise audit (`test_register_stamps_anonymous_actor_and_user_agent`,
`test_authenticated_update_stamps_aggregate_uuid_as_actor`,
`test_correlation_id_header_propagates_to_event`).

---

## 6. Querying the audit log

A few canonical queries:

```sql
-- Every event a user ever caused (uses idx_events_actor_id):
SELECT event_type, sequence, timestamp_utc_us
FROM events
WHERE actor_id = 'd3a1...';

-- Trace a single request end-to-end across aggregates:
SELECT aggregate_type, aggregate_id, event_type, timestamp_utc_us
FROM events
WHERE correlation_id = '...'
ORDER BY timestamp_utc_us;

-- Find every event caused by a specific upstream event:
SELECT * FROM events WHERE causation_id = 'event-uuid-123';
```

---

## 7. Known limitations

- **Wall-clock timestamps are not monotonic.** Use `sequence` and
  `event_id` for ordering, not `timestamp_utc_us`. Audit timestamps capture
  *when something happened* in human-readable terms, not a serializable
  total order.
- **`InProcessEventBus` subscribers receive full audit including IP/UA.**
  Handlers that log events must avoid leaking these fields to non-compliant
  sinks. A `redacted_for_logging()` helper is on the roadmap.
- **The `legacy-pre-hipaa` sentinel is queryable forever.** Events written
  before HIPAA-1 have actor `legacy-pre-hipaa` and a derived microsecond
  timestamp. They satisfy validation but are clearly distinguishable from
  authentic post-HIPAA audit data.

## See also

  (HIPAA-2 through HIPAA-5)
- `crates/arc-core/src/audit.rs` — the source of truth for the type
  and validation rules
- `migrations/2026-04-21-000002_add_hipaa_audit/up.sql`
