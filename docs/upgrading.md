# Upgrading an Arc application

Arc is an upgradeable framework, not a clone-and-edit starter. The framework
lives in versioned crates you depend on; your application is a thin crate you
own. Upgrades are dependency bumps, not manual re-application of edits.

The normative contract for what Arc owns vs. what you own is
[ADR 0002](architecture-decisions/0002-framework-upgrade-contract.md). This guide is the
practical procedure.

## Ownership in one paragraph

- **Framework (Arc-owned, consumed by version):** `arc-core`, `arc-es-sqlite`,
  `arc-es-postgres`, `arc-es-nats`, `arc-stack`, `arc-web`, and `arc-ffi`.
  `arc-stack` owns headless generic event-sourced stack assembly; `arc-web` is
  the Actix web adapter and keeps its prior stack/config imports as re-exports;
  `arc-ffi` supplies native async runtime and registry infrastructure. The
  Android app uses FFI only; web UIs are for web deployments. Never hand-edit
  these framework crates in an application.
- **Your app (`crates/arc-app`, package `arc`):** your domain aggregates,
  services, validation, controllers, routes, templates, and seeders. You plug
  them into the framework through
  `ArcApp::builder().register_aggregate::<YourAggregate>()`.

## How the app plugs into the framework

`crates/arc-app/src/main.rs` wires your app to the framework via the builder:

```rust
ArcApp::builder()
    .register_aggregate::<UserAggregate>()
    .register_projectors(user_projectors())  // your read-model projectors
    .snapshot_policy(user_snapshot_policy())  // optional
    .register_routes(routes::config)          // your controllers/routes
    .serve(app_url, app_port)
    .await
```

You customize framework surfaces through hooks, never by editing framework
source:

| Surface        | How you customize it                                             |
|----------------|------------------------------------------------------------------|
| Routes         | `register_routes(cfg)` — mount your controllers                  |
| Aggregate      | `builder()` + `register_aggregate::<A>()` + projectors           |
| Snapshots      | `snapshot_policy(..)`                                            |
| Event handlers | Benthos handler manifests under `config/handlers/*.yaml` (ADR 0001) |

## Upgrade procedure

1. **Bump the framework crates** in `crates/arc-app/Cargo.toml` (and the
   workspace) to the new version. Arc releases the `arc-*` crates in lockstep,
   so move them together.
   ```bash
   cargo update -p arc-web -p arc-stack -p arc-ffi -p arc-core -p arc-es-sqlite
   ```
2. **Run migrations** — framework-shipped Diesel migrations are forward-only.
   ```bash
   make migrate
   ```
3. **Regenerate machine-owned files** (Benthos runtime config, `schema.rs`):
   ```bash
   make benthos-config
   diesel print-schema > crates/arc-app/src/schema.rs   # if migrations changed
   ```
4. **Revalidate the ownership boundary and build:**
   ```bash
   make doctor      # ADR 0002 drift guard: framework/app boundary + generated files
   make lint
   make test
   make e2e         # Playwright end-to-end
   ```

If `make doctor` fails, you have either edited an Arc-owned file in place or a
generated file is stale — the message names the offending path. Re-generate or
revert the edit rather than working around it; hand-edits to Arc-owned files
void the upgrade promise.

## Compatibility notes

The `arc-web::helpers::es_stack` and related public config paths remain
source-compatible re-exports of `arc-stack` items. The extraction is additive;
the 0.9.0 candidate has not been published. `arc-ffi` is a native adapter and
does not imply an Android web UI.

The 0.9.0 SQLite behavior change sets a 5-second busy timeout on every new or
replacement event-store and read-model connection. Appends acquire an IMMEDIATE
write transaction before reading the current version, so competing writers
queue instead of failing immediately. A write waits while another connection
holds the lock; it can still fail if the lock remains held longer than 5
seconds, such as during a long transaction or by another process. Journal mode
is unchanged (WAL is not enabled). This changes contention latency and lock
acquisition behavior; it is implemented in the unpublished 0.9.0 candidate.

- **SemVer:** `0.x` releases move in lockstep. A patch bump is compatible; a
  minor bump may break the public API surface defined in ADR 0002.
- **Event envelopes** evolve additively within an `envelope_version`.
- **Handler manifests** reject unknown keys at generation time, so a manifest
  that references a removed capability fails fast at `make benthos-config`.

### Phase 2 security hardening (0.9.0) — compatibility notes

This release changes runtime behaviour. It is 0.9.0 rather than a patch
release because Phase 2 includes these breaking changes (decision U-9). Read
all four items before upgrading.

**1. `/ws` fails closed unless the app configures it.** The `ws_handler` in
`arc-web` now checks every handshake before upgrading:

- A missing, `null`, malformed, repeated, or unlisted `Origin` header gets 403.
  The allowlist is empty by default, and Arc never derives it from `Host`,
  forwarded headers, or `APP_URL`.
- If no `WebSocketPrincipalProvider` is registered, or the identity store
  cannot be reached, the handshake gets 503.
- A missing, expired, revoked, or inactive browser identity gets 401. The
  cached `SessionUser` in the cookie no longer counts as authentication.

An app that serves `/ws` must register an origin allowlist and a principal
provider. The session-auth plugin registers the provider; it checks the
browser's session handle against the identity store (for example the one
`arc-auth-db` registers):

```rust
use std::sync::Arc;
use arc_web::websocket::security::WebSocketOriginAllowlist;

ArcApp::builder()
    .register_data(Arc::new(WebSocketOriginAllowlist::new(["https://app.example.test"])?))
    .register_plugin(arc_auth_session::SessionAuthPlugin)
    // identity-store plugin, aggregates and routes as usual
```

An app that does not need sockets should stop mounting `/ws` and stop opening
a socket from its pages; otherwise browsers retry the failed handshake
forever. The reference app (`crates/arc-app`) does this: its pages subscribe
to no room, and its login keeps no server-side session handle a provider could
check. See [WebSocket security](websocket-security.md).

**2. Room subscriptions outside `user:<id>` are dropped without an error.**
`WsServer::new()` now uses `OwnUserRoomPolicy`: a connection may join only
`user:<its verified user id>`. Any other `subscribe` is ignored and the client
gets no error, so broadcasts to shared rooms simply stop arriving. To allow
other rooms, implement `WebSocketRoomPolicy` and register your own server,
which replaces the default one:

```rust
use arc_web::websocket::server::WsServer;

let server = WsServer::with_room_policy(Arc::new(MyRoomPolicy)).start();
builder.register_data(Arc::new(server))
```

**3. Postgres: `events.payload` is rewritten from JSONB to JSON at startup.**
`PostgresEventStore::initialize_schema()` runs on every app start with
`DATABASE_DRIVER=postgres`, and in `arc migrate`. It now converts the column
so payloads keep their exact JSON text (number formatting and key order),
which signature checks and exact event comparison need. Read-model and
snapshot columns stay JSONB.

- The conversion runs `ALTER TABLE events ALTER COLUMN payload TYPE JSON`. It
  rewrites the whole `events` table and rebuilds its indexes under an
  `ACCESS EXCLUSIVE` lock, so every read and write of `events` waits until it
  finishes. The time grows with the table size; measure it on a copy of
  production data first.
- It runs only while the column is still JSONB. Once converted, startup does a
  catalog lookup and nothing else: no rewrite and no long lock.
- It is one way. Arc does not convert the column back.
- Any index that only works on JSONB (for example a GIN index on
  `events.payload`) makes the conversion fail, and the app then refuses to
  start. Check before upgrading:

  ```sql
  SELECT pg_get_indexdef(indexrelid) FROM pg_index
  WHERE indrelid = 'events'::regclass
    AND pg_get_indexdef(indexrelid) ILIKE '%payload%';
  ```

Recommended procedure: back up the database, schedule a maintenance window,
drop or replace any JSONB-only index on `events.payload`, run `arc migrate`
once from a single instance, and only then start the new version everywhere.
New instances started together against an unconverted table queue on the same
lock.

**4. `CommandBus` truncates `Event.timestamp` to whole seconds.** The SQLite
and Postgres stores have always stored the event timestamp in seconds. Before
this release, `CommandBus` published and returned the millisecond value while
the store kept seconds, so the published event did not match the stored one.
Projection delivery now compares the full event with the stored copy, and that
mismatch would reject valid deliveries. `CommandBus` therefore rounds the
timestamp down to the second before it appends, publishes, or returns events.
`InMemoryEventStore` (the `test-utils` store) applies the same rounding on
append, so tests see what the real stores return.

Effect on consumers: `Event.timestamp` from `dispatch`, from event handlers,
and from published events is always a whole second (a multiple of 1000 ms).
Use `sequence` for ordering, and `audit.timestamp_utc_us` when you need
sub-second time. A producer that calls `EventStore::append` and then publishes
the same events itself must round them with
`Event::with_stored_timestamp_precision()` first, or projection delivery
rejects them.

### 0.3 to 0.4

`Event::new` now takes a named-field `NewEvent` parameter struct. Replace
positional construction:

```rust
Event::new(NewEvent {
    aggregate_type: "Product",
    aggregate_id: product_id,
    sequence: next_sequence,
    event_type: "ProductCreated",
    payload,
})
```

Import both types with `use arc_core::event::{Event, NewEvent};`.
