# Upgrading an Arc application

Arc is an upgradeable framework, not a clone-and-edit starter. The framework
lives in versioned crates you depend on; your application is a thin crate you
own. Upgrades are dependency bumps, not manual re-application of edits.

The normative contract for what Arc owns vs. what you own is
[ADR 0002](architecture-decisions/0002-framework-upgrade-contract.md). This guide is the
practical procedure.

## Ownership in one paragraph

- **Framework (Arc-owned, consumed by version):** `arc-core`, `arc-es-sqlite`,
  `arc-es-postgres`, `arc-es-nats`, and `arc-web`. `arc-web` holds the web/runtime
  machinery — Actix bootstrap, middleware, session/JWT helpers, the websocket
  transport, and the generic event-sourced stack `EsStack<A>`. It is **generic
  over your aggregate** and ships no concrete domain type. Never hand-edit these.
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
   cargo update -p arc-web -p arc-core -p arc-es-sqlite
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

- **SemVer:** `0.x` releases move in lockstep. A patch bump is compatible; a
  minor bump may break the public API surface defined in ADR 0002.
- **Event envelopes** evolve additively within an `envelope_version`.
- **Handler manifests** reject unknown keys at generation time, so a manifest
  that references a removed capability fails fast at `make benthos-config`.

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
