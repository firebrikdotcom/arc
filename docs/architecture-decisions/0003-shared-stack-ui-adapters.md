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
| `EsStack<A>` exposes a command bus and shared handles to its read-model store and projection engine. | `crates/arc-web/src/helpers/es_stack.rs:22-28` |
| The current `build` function chooses its database driver from the environment and returns an `EsStack<A>`. | `crates/arc-web/src/helpers/es_stack.rs:104-112` |
| An `ArcAppBuilder` registration captures a call to the private runtime builder. | `crates/arc-web/src/lib.rs:117-129` |
| The existing registration creates a new aggregate registration and its bus at serve time. | `crates/arc-web/src/lib.rs:194-206`; `crates/arc-web/src/commands/serve.rs:67-75,115-123` |
| The current runtime wraps its owned command bus with `web::Data::new` and shares the store and projection engine through `web::Data::from`. | `crates/arc-web/src/commands/serve.rs:42-57` |
| The current in-process server subscribes projections and rebuilds them during startup. | `crates/arc-web/src/commands/serve.rs:84-107` |
| `CommandBus<A>` does not implement `Clone`; sharing a bus requires an `Arc`. | `crates/arc-core/src/command_bus.rs:195-201` |
| Builder projectors and snapshot policy are stored on the most recent aggregate registration; the builder already rejects configuration before registration. | `crates/arc-web/src/lib.rs:109-115,209-220,277-288` |
| The framework workspace is at version 0.8.11 and follows lockstep versioning for reusable crates. | `Cargo.toml:20`; `docs/publishing-crates.md:89-94` |

## Decision

The application owns one stack for each aggregate. UI adapters use that stack and contain only interface-specific translation. Web and FFI may be enabled together; neither adapter creates a second command bus or projection engine.

The handoff decisions are settled as follows:

- **D-A — One stack:** Build one stack per aggregate per process and let the application own it.
- **D-B — App service boundary:** Note operations, typed errors, and other domain-facing application operations live in `NotesCore`. Adapters translate requests and responses; they do not dispatch directly to the bus.
- **D-C — Adapter features:** The app has separate `web` and `ffi` Cargo features. Adapter dependencies are enabled only with their feature, subject to the T4 dependency finding below.
- **D-D — FFI design:** The Android adapter uses UniFFI, Compose, a ViewModel, and a repository. Kotlin lists recordings; playback uses `MediaPlayer`.

The oracle decisions for this framework change are also settled:

- **Q-1:** The consumer app experiment targets `feature/phase2-security-hardening` at `c83fc2e`, which contains the required 0.8.11 baseline.
- **Q-2:** Add `SharedStack<A>` containing an `Arc<CommandBus<A>>` plus the existing shared read-model-store and projection-engine handles. Convert an `EsStack<A>` into it and make it cloneable without `A: Clone`. Add `register_aggregate_with_stack::<A>(stack) -> Self`. Reject any subsequent projector or snapshot-policy configuration for that registration immediately with a descriptive panic, including an empty projector list and `snapshot_policy(None)`. The caller owns projection rebuilding.
- **Q-3:** Preserve all existing public signatures, including the public `EsStack` fields and `es_stack::build`. Keep `BuiltAggregateRuntime` crate-private. The new runtime path accepts the shared bus and injects it with `web::Data::from`.
- **Q-4:** Add `es_stack::build_with_driver` with the signature specified below. Existing `build` delegates using `DatabaseDriver::from_env()` so current behavior is preserved.
- **Q-5:** The app experiment uses path dependencies to the matching local `arc-web` and `arc-core` sources, avoiding duplicate incompatible core types. The intended additive release target is 0.8.12 after validation; recheck registry availability before any publication. Publication is not authorized by this ADR.
- **Q-6:** App-side work uses its dedicated `feature/shared-stack-ui-adapters` worktree based on app master at `a29ac07`; preserve the existing app checkout.
- **Q-7:** The app's FFI-only dependency check requires a headless assembly seam. Keep one implementation rather than copying stack assembly into the app; preserve current Arc public paths with re-exports.

The app-side decisions for T2–T6 are:

- **Q-8 — Compose plan:** The newer handoff governs: retain UniFFI, Kotlin recording listing, platform `MediaPlayer`, Material 3 with the Arc palette, and the web adapter and its existing navigation and recording behavior. Keep plan steps 1 and 3–7, adapt step 4 to Q-9, replace the copied stack builder with shared assembly, and use feature/source-set/manifest isolation plus documentation and size measurements for the remaining work.
- **Q-9 — Variants:** Provide `web`, `compose`, and `both` flavors mapped to Cargo `web`, `ffi`, and `web,ffi`; default Makefile builds/deploys to `both`. Both adapters run concurrently over one `NotesCore`; a remembered Web/Compose selector changes the visible UI without restarting them. Only web/both include internet, loopback, and WebView resources.
- **Q-10 — Features:** Default Cargo features are `web` and `ffi`; make adapter dependencies optional and gate server/JNI and UniFFI code. Keep core, Tokio, and core storage dependencies unconditional. Bindgen is host tooling; Compose dependencies and generated bindings belong to compose/both. Explicitly check core-only, web, ffi, and combined builds.
- **Q-11 — Runtime:** `NotesCore` owns one long-lived multithread Tokio runtime, its handle, and one stack. Initialize schema, stack, and caller-owned projection rebuild once. Both adapters reuse the same `Arc<NotesCore>` by canonical database path. Async methods spawn on that runtime and await results; map join failures to `CoreError`. Do not block inside async/UI callbacks or retry writes after a cancelled wait.
- **Q-12 — Continuity:** All flavors retain the same application ID, signing identity, compatible version codes, database/schema, recording paths, markers, preferences, and stored session keys. Replace variants in place with `adb install -r`; never uninstall or clear data.
- **Q-13 — Emulator gate:** T5 includes a bounded E2E runner pinned to `emulator-5580`, with explicit serials, per-flavor reports, adapter cross-read/write, recording and recovery coverage, and Q-12's install sequence. Refuse other devices and never run phone tests or unscoped `connectedAndroidTest`; missing tooling or unavailable capture is reported as unverified.

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

This is a compatible 0.8.12 candidate, not a 0.9.0 change. Use the current workspace version during local experimentation; bump all published crates together only for a separately authorized release. The publish order is `arc-core`, `arc-auth-core`, the storage and NATS crates, `arc-stack`, `arc-web`, then the remaining auth packages and CLI in their established order. Add `arc-stack` to workspace membership, package metadata/license files, package verification inventory, and maintained publishing documentation/counts. Publication and credentials remain outside this decision's authorization; do not assume the release workflow's bare `cargo publish` observes this order.

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

Tasks run in order, with one commit per task. T1 and T1b belong to the Arc framework repository; T2–T6 belong to the consumer app repository. The app-side decisions map to rollout work as follows:

| Decision | Rollout application |
|---|---|
| Q-8 | T5/T6 follow the newer handoff, retain web behavior and the chosen Compose stack, replace copied assembly with `arc-stack`, and keep the relevant Compose plan steps. |
| Q-9 | T5 builds web, compose, and both flavors; both runs the two adapters over one `NotesCore` and remembers the visible UI choice. |
| Q-10 | T4 tests core-only, web, ffi, and web+ffi with explicit feature selection and flavor-specific generated bindings/resources. |
| Q-11 | T3 implements the single runtime bridge and shared `NotesCore`; T5 proves it through Android UniFFI and concurrent adapter flows. |
| Q-12 | T5 verifies sequential in-place flavor installation and storage/audio continuity without uninstalling or clearing data. |
| Q-13 | T5 runs the bounded E2E suite only on emulator-5580, with per-flavor reports and no phone tests. |

| Task | Repository and change | Owned checks / completion evidence |
|---|---|---|
| T1 | Framework: add `SharedStack`, `register_aggregate_with_stack`, explicit-driver stack construction, bidirectional integration coverage, and API documentation. | `make check && make test && make lint && make doctor`; shared-bus writes are visible to HTTP reads and HTTP writes are visible through the retained stack. Existing registration tests stay green. |
| T1b | Framework: extract the completed assembly API into headless `arc-stack`, preserving `arc-web` re-export paths and feature behavior. | Downstream fixtures compile old imports and signatures under default, no-default, PostgreSQL, and all features; package dependency checks show no HTTP/Actix/Tera dependency through `arc-stack`. Run the prescribed framework checks and `make publish-check`. |
| T2 | App: use matching local `arc-stack`, `arc-web`, and `arc-core` dependencies without duplicate core types. | `cargo tree -p arc-web` resolves the T1 framework source and one matching core type; `NotesCore` can use `arc-stack` while web remains feature-gated. |
| T3 | App: extract note operations into `NotesCore`, which owns one Tokio runtime, handle, and stack per canonical database path. | `cd rust && CARGO_BUILD_JOBS=4 cargo test`; concurrent adapter operations share one `Arc<NotesCore>`, bridge through its runtime, and focused tests cover note operations and typed errors. |
| T4 | App: implement Cargo features and web adapter over `NotesCore`; keep runtime assembly in `arc-stack`, not an app copy. | Explicit no-default core-only, web, ffi, and web+ffi checks pass. FFI-only dependency analysis excludes Actix and Tera; web/both retain the existing adapter behavior. |
| T5 | App: provide `web` → `web`, `compose` → `ffi`, and `both` → `web,ffi` Gradle flavors; default `FLAVOR` to `both`. Add UniFFI bindings, Compose UI, recording state, playback, and a remembered selector with simultaneous adapters in `both`. | Run `make e2e-emulator` pinned to emulator-5580, sequential data-preserving flavor installs, adapter cross-visibility, recording/recovery tests, and per-flavor isolation checks. Only after these checks, deploy the requested flavor to Phone (3) in place; never uninstall or clear data. |
| T6 | App: align README, architecture guide, and Compose plan; explain adding a UI and record measured sizes for all variants. | Documentation matches code and measured web, compose, and both APK/native library sizes. Applicable CI passes on the final head. |

## Risks and open questions

- **Two writers:** If T1 is delayed, two stacks on one SQLite file are only a temporary stopgap; they do not provide live cross-stack visibility and are not the target design.
- **Tokio runtime:** `NotesCore` owns a Tokio runtime. T4 must establish whether Actix and UniFFI can share it or need a bridge.
- **Headless boundary:** T1b must keep one assembly implementation and all legacy `arc-web` paths source-compatible while ensuring `arc-stack` has no HTTP, Actix, Tera, auth, or NATS dependency.
- **Async runtime:** UniFFI and Actix use their own surrounding runtimes; the shared core runtime bridge must be proven on Android and under concurrent adapter use. Cancellation cannot imply a safe write retry.
- **Data continuity:** Preserve the app's `notes.sqlite` filename, `schema.sql` tables, and `recordings/` layout.
- **Release status:** Recheck 0.8.12 availability before publishing. This ADR does not authorize publication.

## Follow-ups

- Add `arc-stack` as the sole headless stack assembly crate and keep the `arc-web` compatibility facades covered by downstream compile fixtures and dependency guards.
- The app currently does not delete recordings when their note is deleted; that existing gap is outside this stack-sharing change.
- Remove the first prototype's unused `respond` and home-page code in the app when T4 reaches the relevant files.

## Implementation record

To be completed when T1 merges.
