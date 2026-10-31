# ADR 0003 — Share one event-sourced stack across UI adapters

- Status: **Accepted; implementation in progress**
- Date: 2026-10-30
- Deciders: Arc framework architecture
- Related: [ADR 0002](0002-framework-upgrade-contract.md), `progress.md`, and the shared-stack handoff

## Context

`ArcAppBuilder` currently builds a private runtime for each aggregate registration. That works for a single web adapter, but it prevents an application from building its event-sourced stack once and sharing it with other interfaces such as an FFI adapter. If web and FFI each build their own stack, writes and projections can diverge, particularly when both use the same SQLite file.

The relevant facts in this worktree are:

| Fact | Evidence |
|---|---|
| `EsStack<A>` exposes a command bus and shared handles to its read-model store and projection engine. | `crates/arc-stack/src/es_stack.rs:24-32` |
| The current `build` function chooses its database driver from the environment and returns an `EsStack<A>`. | `crates/arc-stack/src/es_stack.rs:147-172` |
| An `ArcAppBuilder` registration captures a call to the private runtime builder. | `crates/arc-web/src/lib.rs:117-129` |
| The existing registration creates a new aggregate registration and its bus at serve time. | `crates/arc-web/src/lib.rs:194-206`; `crates/arc-web/src/commands/serve.rs:67-75,115-123` |
| The current runtime wraps its owned command bus with `web::Data::new` and shares the store and projection engine through `web::Data::from`. | `crates/arc-web/src/commands/serve.rs:42-57` |
| The current in-process server subscribes projections and rebuilds them during startup. | `crates/arc-web/src/commands/serve.rs:84-107` |
| `CommandBus<A>` does not implement `Clone`; sharing a bus requires an `Arc`. | `crates/arc-core/src/command_bus.rs:195-201` |
| Builder projectors and snapshot policy are stored on the most recent aggregate registration; the builder already rejects configuration before registration. | `crates/arc-web/src/lib.rs:109-115,209-220,277-288` |
| The framework workspace is at version 0.8.11 and follows lockstep versioning for reusable crates. | `Cargo.toml:20`; `docs/publishing-crates.md:89-94` |

## Decision

The application owns one stack for each aggregate. UI adapters use that stack and contain only interface-specific translation. A deployment may select the adapter it needs; adapters sharing a process use the same command bus and projection engine.

The original app-specific handoff decisions below are retained as dated
history. U-1 (2026-10-30) supersedes their mobile web/both requirements: the
Android app is FFI-only, while web UIs are for web deployments. Q-14 and Q-15
continue to govern the shared framework crates and sibling adapters.

- **D-A — One stack:** Build one stack per aggregate per process and let the application own it.
- **D-B — App service boundary:** Note operations, typed errors, and other domain-facing application operations live in `NotesCore`. Adapters translate requests and responses; they do not dispatch directly to the bus.
- **D-C — Adapter features:** The app has separate `web` and `ffi` Cargo features. Adapter dependencies are enabled only with their feature, subject to the T4 dependency finding below.
- **D-D — FFI design:** The Android adapter uses UniFFI, Compose, a ViewModel, and a repository. Kotlin lists recordings; playback uses `MediaPlayer`.

The oracle decisions for this framework change are also settled:

- **Q-1:** The consumer app experiment targets `feature/phase2-security-hardening` at `c83fc2e`, which contains the required 0.8.11 baseline.
- **Q-2:** Add `SharedStack<A>` containing an `Arc<CommandBus<A>>` plus the existing shared read-model-store and projection-engine handles. Convert an `EsStack<A>` into it and make it cloneable without `A: Clone`. Add `register_aggregate_with_stack::<A>(stack) -> Self`. Reject any subsequent projector or snapshot-policy configuration for that registration immediately with a descriptive panic, including an empty projector list and `snapshot_policy(None)`. The caller owns projection rebuilding.
- **Q-3:** Preserve all existing public signatures, including the public `EsStack` fields and `es_stack::build`. Keep `BuiltAggregateRuntime` crate-private. The new runtime path accepts the shared bus and injects it with `web::Data::from`.
- **Q-4:** Add `es_stack::build_with_driver` with the signature specified below. Existing `build` delegates using `DatabaseDriver::from_env()` so current behavior is preserved.
- **Q-5:** The app experiment uses path dependencies to the matching local `arc-web` and `arc-core` sources, avoiding duplicate incompatible core types. The intended release target is 0.9.0 (the additions stay additive) after validation; recheck registry availability before any publication. Publication is not authorized by this ADR.
- **Q-6:** App-side work uses its dedicated `feature/shared-stack-ui-adapters` worktree based on app master at `a29ac07`; preserve the existing app checkout.
- **Q-7:** The app's FFI-only dependency check requires a headless assembly seam. Keep one implementation rather than copying stack assembly into the app; preserve current Arc public paths with re-exports.

### Historical app-side decisions for T2–T6 (web/both portions superseded by U-1)

The following D-A through D-D choices described the earlier dual-adapter app
plan and are historical where they conflict with U-1:

- **Q-8 — Compose plan (historical):** The earlier plan retained UniFFI, Kotlin recording listing, platform `MediaPlayer`, Material 3, and a mobile web adapter.
- **Q-9 — Variants (superseded):** The proposed Android `web`, `compose`, and `both` flavors and simultaneous mobile adapters are not active requirements.
- **Q-10 — Features (superseded for the app):** The proposed app-level web/ffi feature matrix is not active; Android is FFI-only. Framework-level adapter separation remains.
- **Q-11 — Runtime (historical):** The earlier two-adapter `NotesCore` acceptance is superseded; Q-15 specifies reusable arc-ffi infrastructure, with app migration separate.
- **Q-12 — Continuity (still applicable):** A future app migration must preserve application identity and stored data; this does not authorize installation or data changes.
- **Q-13 — Emulator gate (superseded):** Multi-flavor Android E2E is not an active requirement for this framework work.

**U-1 — Mobile adapter choice (2026-10-30).** The Android app is FFI-only;
web UIs are for web deployments. This supersedes Q-8 through Q-13 wherever
they require a mobile web adapter, WebView, loopback server, or web/both flavor.
Those portions remain dated history, not active requirements.

**U-2 — Native adapter crate (2026-10-30).** Create `arc-ffi` as the UniFFI/native
counterpart to `arc-web`, over `arc-stack`, without web dependencies. Q-15 below
specifies its API and runtime contract.

**C-3 — Supplied stack and EVENT_BUS (2026-10-31).** `register_aggregate_with_stack` is new API, so rejecting `EVENT_BUS=nats` (or any non-in-process bus) for a supplied stack is not a compatibility break. Startup fails fast with a descriptive error naming the aggregate and the variable. A NATS-capable shared stack is a follow-up.

### Specification of the change

**Q-14 — Headless stack crate.** The stack assembly API is ultimately owned by a new headless `arc-stack` crate (`arc_stack`), above core/storage and below `arc-web`. It has no dependency on `arc-web`, auth, Actix, or Tera. The T1 API is implemented first, then extracted without changing its agreed signatures. The shared handle has public fields so the application can retain and use the same bus outside the HTTP adapter, and so the existing stack handles remain directly available:

```rust
pub struct SharedStack<A: Aggregate> {
    pub command_bus: Arc<CommandBus<A>>,
    pub read_model_store: Arc<dyn ReadModelStore>,
    pub projection_engine: Arc<ProjectionEngine>,
}
```

It provides `SharedStack::new(command_bus, read_model_store, projection_engine)` with those field types; `From<EsStack<A>>`, which moves the store and engine handles and wraps the owned bus in `Arc`; and `Clone` implemented without an `A: Clone` bound. Clones refer to the same command bus and shared handles.

`arc_stack::es_stack` owns `EsStack`, `SharedStack`, `EsStores`, and the stack assembly functions. `ProjectorReg` moves to the `arc_stack` crate root with its fields and constructor unchanged. `DatabaseDriver` and its existing methods, plus the integrity environment readers, move to `arc_stack::config`. `SharedStack` is also re-exported at the `arc_stack` root. The existing web builder adds:

```rust
pub fn register_aggregate_with_stack<A: Aggregate + 'static>(
    self,
    stack: SharedStack<A>,
) -> Self;
```

The method registers the supplied handles as the aggregate runtime. It retains duplicate-aggregate validation consistent with `register_aggregate::<A>()`. Builder calls that configure projectors after this registration panic immediately, even when given an empty vector. Calling `snapshot_policy(None)` after it also panics: the attempted configuration itself is invalid for a supplied, already-configured stack. The panic explains that the supplied stack owns its projectors and snapshot policy. Projector or snapshot configuration before any aggregate registration retains the existing missing-registration failure. This tracking must distinguish “the setter was called with `None`” from “no policy setter was called.”

A supplied stack is in-process only (C-3). When `EVENT_BUS` selects anything other than the in-process bus, startup fails at the point that reads `EVENT_BUS` for registrations, before the server binds, with an `InvalidInput` error naming the aggregate and the variable. The supplied stack and `DATABASE_URL`/`DATABASE_DRIVER` must name the same database, because the health check and the SQLite session store still read those variables.

The supplied-stack path does not call `ProjectionEngine::rebuild_all()` on startup. Rebuilding is the caller's responsibility because the caller owns stack initialization and lifecycle. The existing `register_aggregate::<A>()` startup path, including its in-process projection subscription and rebuild behavior, remains unchanged.

The web runtime accepts `Arc<CommandBus<A>>` and constructs `web::Data<CommandBus<A>>` from that Arc. No second bus is created. The supplied read-model store and projection engine continue to be installed as Actix data using their existing shared handles. `arc-web` keeps its existing `helpers::es_stack` module as a re-export facade for every prior public item and the T1 additions; `arc_web::ProjectorReg` and `arc_web::helpers::config::{DatabaseDriver,event_integrity_key,event_integrity_key_id}` likewise re-export the same underlying items. Preserve every SharedStack export path introduced by T1.

`arc-stack` uses `default = ["sqlite"]`, `sqlite = ["dep:arc-es-sqlite"]`, and `postgres = ["dep:arc-es-postgres"]`; core is unconditional and storage dependencies are optional. It has no HTTP or NATS feature. Keep `DatabaseDriver` variants and assembly signatures available under every feature set, returning an explicit error if the selected backend is disabled. `arc-web` depends on `arc-stack` unconditionally with default features off and SQLite enabled; retain its existing default SQLite, marker, NATS, and test-utils features, and forward its PostgreSQL feature to `arc-stack/postgres`. This keeps SQLite support in legacy `arc-web` no-default-features builds. The app uses matching `arc-stack` with SQLite unconditionally; its `arc-web` dependency stays web-only and `NotesCore` imports assembly from `arc-stack`.

The stack helper adds this exact signature:

```rust
pub async fn build_with_driver<A: Aggregate>(
    driver: DatabaseDriver,
    database_url: &str,
    projectors: Vec<ProjectorReg>,
    snapshot_policy: Option<SnapshotPolicy>,
) -> Result<EsStack<A>, Box<dyn std::error::Error>>
```

`es_stack::build` keeps its current signature and delegates to `build_with_driver(DatabaseDriver::from_env(), database_url, projectors, snapshot_policy)`. This allows embedders to select the driver explicitly without changing existing callers.

Unchanged public items include `register_aggregate::<A>()`, `EsStack<A>` and its public fields, and `es_stack::build`. `arc-web` preserves the old module and root import paths as re-exports rather than wrappers or duplicate implementations.

## Backward compatibility

**Q-14 compatibility and release contract.** The extraction is additive and compatible: existing consumers retain the same `arc_web::helpers::es_stack` public items, `arc_web::ProjectorReg`, and `arc_web::helpers::config::{DatabaseDriver,event_integrity_key,event_integrity_key_id}` as re-exports of the same underlying definitions. Keep `ArcAppBuilder`, its private runtime and Actix injection, HTTP APIs, NATS serving, environment names/defaults/error behavior, integrity behavior, snapshot policy, and caller-owned projection replay unchanged. Do not introduce wrapper types, newtypes, or deprecations. Consumers using either default or no-default `arc-web` features retain the complete HTTP API; no schema, migration, credential, or app-copy change is part of the extraction.

The shared-stack extraction itself is compatible and additive; it ships in 0.9.0 because Phase 2 includes breaking changes, not because of this extraction. Use the current workspace version during local experimentation; bump all published crates together only for a separately authorized release. The publish order is `arc-core`, `arc-auth-core`, the storage and NATS crates, `arc-stack`, `arc-web`, then the remaining auth packages and CLI in their established order. Add `arc-stack` to workspace membership, package metadata/license files, package verification inventory, and maintained publishing documentation/counts. Publication and credentials remain outside this decision's authorization; do not assume the release workflow's bare `cargo publish` observes this order.

## Consequences

### Positive

- Web and FFI can dispatch through one command bus and observe the same event and projection state.
- Application code can choose a database driver explicitly when constructing its stack.
- Existing users retain the builder and helper behavior they already use.

### Negative / costs

- The application must own stack construction, initialization order, and projection rebuilding when it supplies a stack.
- Invalid attempts to add builder-owned projectors or snapshot policy after supplying a stack fail by panic at configuration time.
- `arc-stack` needs feature-specific backend errors and isolated dependency tests to ensure the headless boundary remains real.

### Neutral

- `EsStack<A>` remains the existing construction result. `SharedStack<A>` is the opt-in bridge for sharing it with the web runtime.
- T1 changes framework code; the app's `NotesCore`, features, and UI adapters are later tasks in the consumer repository.

## Rollout plan

Historical rollout mapping (T2–T6 portions requiring mobile web/both are
superseded by U-1; the remaining app migration is FFI-only):

| Decision | Rollout application |
|---|---|
| Q-8 | Historical Compose/mobile-web plan; superseded by U-1 where it requires mobile web UI. |
| Q-9 | Historical: T5 proposed web, compose, and both flavors; superseded by U-1. |
| Q-10 | Historical app feature/flavor matrix; superseded by FFI-only U-1. |
| Q-11 | Historical app runtime plan; the app migration remains separate and FFI-only. |
| Q-12 | Data continuity remains a requirement for any future app migration; no installation is authorized here. |
| Q-13 | Historical multi-flavor E2E plan; superseded by FFI-only app migration scope. |

| Task | Repository and change | Owned checks / completion evidence |
|---|---|---|
| T1 | Framework: add `SharedStack`, `register_aggregate_with_stack`, explicit-driver stack construction, bidirectional integration coverage, and API documentation. | `make check && make test && make lint && make doctor`; shared-bus writes are visible to HTTP reads and HTTP writes are visible through the retained stack. Existing registration tests stay green. |
| T1b | Framework: extract the completed assembly API into headless `arc-stack`, preserving `arc-web` re-export paths and feature behavior. | Downstream fixtures compile old imports and signatures under default, no-default, PostgreSQL, and all features; package dependency checks show no HTTP/Actix/Tera dependency through `arc-stack`. Run the prescribed framework checks and `make publish-check`. |
| T2–T6 | App migration and Android UI work (historical rollout; mobile web/both portions superseded by U-1). | App migration to `arc-stack` + `arc-ffi` remains a separate FFI-only task; see Follow-ups. No app files are part of this implementation record. |

## Risks and open questions

- **Headless boundary:** T1b keeps one assembly implementation and all legacy `arc-web` paths source-compatible while ensuring `arc-stack` has no HTTP, Actix, Tera, auth, or NATS dependency.
- **App migration:** Preserve the app's `notes.sqlite` filename, `schema.sql` tables, application identity, and `recordings/` layout when the separate FFI-only migration is performed.
- **Release status:** Recheck 0.9.0 availability before publishing. This ADR does not authorize publication.

## Follow-ups

- Migrate `arc-android-prototype` to `arc-stack` + `arc-ffi`, preserving its database/schema, recordings, application identity and typed domain behavior; verify FFI-only dependency isolation and emulator data continuity.
- Obtain authorization before a 0.9.0 release; nothing from this candidate has been published.
- The app currently does not delete recordings when their note is deleted; that existing gap is outside this stack-sharing change.
- A NATS-capable supplied stack (C-3), and an explicit integrity-key parameter for `build_with_driver`/`build_stack` instead of the `EVENT_INTEGRITY_KEY` environment variables.

## Implementation record

SQLite contention behavior change for the 0.9.0 candidate (Q-16, approved by
U-3): `arc-es-sqlite` sets a 5000 ms busy timeout on every new or replacement
event-store and read-model connection. Appends use IMMEDIATE transactions to
take the write lock before reading the current version. Journal mode is
unchanged; WAL is not enabled. This changes lock-wait latency: writers queue
while a lock is available within the timeout, and a lock held longer than five
seconds can still fail. A local measurement launched 160 simultaneous appends
from eight barrier-started workers; all appends and projection writes succeeded
in 0.12–0.15 seconds. Same-stream contenders produced exactly one commit and a
typed `ConcurrencyConflict`. Before the fix, four threads making 30 distinct-ID
writes each produced 27 successes and 93 `database is locked` errors. These are
local measurements, not a general throughput guarantee.

Implemented in `84983b0` (arc-ffi), `a271610` (arc-stack), `beb93b5` (QA), and
`50cf543` (caller-built stack for arc-web), following ADR commit `e5516eb`.
`arc-stack` publicly provides `ProjectorReg`, `DatabaseDriver`, `EsStack`,
`SharedStack`, `EsStores`, `build`, `build_with_driver`, `build_stores`, and
`apply_snapshot_policy`; `arc-ffi` provides `FfiRuntime`, `FfiCore`,
`CoreRegistry`, `CoreKey`, `FfiError`, and `build_stack`. Existing public
`arc-web` stack/config paths remain re-exports of those same types and functions.

Checks reported passing by the implementation worktrees: `CARGO_BUILD_JOBS=4
make check`, `make test`, `make lint`, `make doctor`,
`bash scripts/check-publish-packages.sh`, isolated arc-stack downstream
compatibility checks (default, no-default, PostgreSQL, all features), arc-ffi
feature-matrix tests, dependency isolation, doctor regression fixtures, and the
native Kotlin/JVM create/read smoke. The live PostgreSQL schema test was not run
because `ARC_POSTGRES_TEST_DATABASE_URL` was unset. Swift binding generation
passed; Swift compile/run was not run because no Swift compiler was configured.
Android app migration and final pushed-head CI remain pending; nothing was
published.

The shared-stack changes are additive and are a 0.9.0 candidate. The SQLite
contention change is a documented behavior change in that candidate. No persisted schema or
legacy Arc web import path changes. The native Android consumer still needs a
separate migration to `arc-stack` and `arc-ffi`; no release is authorized here.

2026-10-31: the release is 0.9.0 (decision U-9).
