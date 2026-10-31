# Native adapters with arc-ffi

`arc-ffi` adds a headless async bridge and reusable core registry above `arc-stack`.
It owns runtime lifetime and infrastructure errors. Your application owns the
aggregate, projectors, schema/migrations, validation, records and exported objects.
`arc-web` and `arc-ffi` are independent sibling adapters. Existing web consumers
need no changes. This source addition is not a published release or an app migration.

## 0.9.0 SQLite contention behavior (candidate)

The 0.9.0 `arc-es-sqlite` change applies a 5-second busy timeout to every new
and replacement connection in both stores. Appends acquire an `IMMEDIATE`
transaction before reading the current version. Different-stream writers wait
for the SQLite write lock instead of failing immediately; a lock held longer
than 5 seconds can still produce a database error. Same-stream optimistic
version checks still return typed concurrency conflicts. Journal mode is
unchanged; WAL is not enabled. This observable latency/contention behavior
change is implemented in the unpublished 0.9.0 candidate; see
[Upgrading](upgrading.md).

## Dependencies and features

```toml
[dependencies]
arc-core = "0.8.11"
arc-stack = { version = "0.8.11", default-features = false }
arc-ffi = { version = "0.8.11", features = ["sqlite"] }
uniffi = { version = "=0.32.2", default-features = false }
```

Use matching local paths while testing the unpublished addition. All Arc crates
remain at the current workspace version until a coordinated release bump.

| Feature | Effect |
|---|---|
| default | `sqlite` + `uniffi` |
| no defaults | Generic Rust runtime, registry, keys and errors; no storage backend |
| sqlite | Forwards `arc-stack/sqlite` |
| postgres | Forwards `arc-stack/postgres` |
| uniffi | Error component/scaffolding with exact UniFFI 0.32.2 |

A selected disabled backend returns `StackInitializationFailed`. Normal/build
transitive dependencies contain no `arc-web`, authentication crates, Actix or
Tera. The library has no direct storage implementation, binding compiler or
UniFFI CLI/Tokio-runtime feature. A separate host bindgen tool uses the exact
same UniFFI version with `cli,cargo-metadata`.

## Rust API and initialization

The crate root exports `FfiRuntime`, `FfiCore<A>`, `CoreRegistry`, `CoreKey`,
`FfiError` and `build_stack`. The exact callable shapes are:

```rust,ignore
FfiRuntime::new() -> Result<Arc<FfiRuntime>, FfiError>
FfiRuntime::run<T, F>(&self, future: F) -> Result<T, FfiError>
// F: Future<Output=T> + Send + 'static, T: Send + 'static
CoreRegistry::process() -> Result<Arc<CoreRegistry>, FfiError>
CoreRegistry::new(runtime: Arc<FfiRuntime>) -> Arc<CoreRegistry>
CoreRegistry::open<A, F, Fut>(self: &Arc<Self>, key: CoreKey,
    configuration_id: String, initialize: F) -> Result<Arc<FfiCore<A>>, FfiError>
// async; A: Aggregate + 'static; F: FnOnce(CoreKey)->Fut + Send + 'static
// Fut: Future<Output=Result<SharedStack<A>, FfiError>> + Send + 'static
FfiCore::stack(&self) -> SharedStack<A>
FfiCore::run<T, E, F, Fut>(&self, operation: F) -> Result<T, E>
// async; F: FnOnce(SharedStack<A>)->Fut + Send + 'static
// Fut: Future<Output=Result<T,E>> + Send + 'static
// T: Send + 'static; E: From<FfiError> + Send + 'static
build_stack<A: Aggregate>(driver: DatabaseDriver, database_url: &str,
    projectors: Vec<ProjectorReg>, snapshot_policy: Option<SnapshotPolicy>)
    -> Result<SharedStack<A>, FfiError> // async
CoreKey::sqlite_file(path: PathBuf) -> Result<CoreKey, FfiError>
CoreKey::sqlite_path(&self) -> Option<&Path>
CoreKey::postgres(namespace: String) -> Result<CoreKey, FfiError>
```

`run`, `open` and `build_stack` are async. `build_stack` only delegates to
`arc_stack::es_stack::build_with_driver`; it never creates another wiring recipe.
Only the driver is explicit: the SQLite event store still reads
`EVENT_INTEGRITY_KEY` and `EVENT_INTEGRITY_KEY_ID` from the process environment.
A native host that does not set them before `build_stack` stores unsigned events.
When `build_stack` fails it logs a fixed cause category (`event_store`,
`read_model_store`, `event_bus` or `other`) before returning
`StackInitializationFailed`.
The initialization closure executes on the registry runtime:

```rust,ignore
let key = CoreKey::sqlite_file(database_path)?; // filesystem work off the UI thread
let core = CoreRegistry::process()?.open::<MyAggregate, _, _>(
    key, "schema-v3-projectors-v2-snapshots-50".into(),
    |key| async move {
        let path = key.sqlite_path().unwrap().to_str().unwrap();
        prepare_schema(path).await.map_err(|_| FfiError::SchemaInitializationFailed)?;
        let stack = build_stack(DatabaseDriver::Sqlite, path, projectors(), policy()).await?;
        // Only if this application's startup policy explicitly requests replay:
        stack.projection_engine.rebuild_all().await
            .map_err(|_| FfiError::ProjectionRebuildFailed)?;
        Ok(stack)
    },
).await?;
let result = core.run(|stack| async move {
    my_domain_operation(stack.command_bus, stack.read_model_store).await
}).await?;
```

There is no implicit replay. Prepare schema and choose replay before returning
readiness. Initialization side effects must be idempotent: an ordinary failure
or panic unblocks all current waiters and permits a later explicit retry with
the same configuration. The closure must never await opening its own
aggregate/key, including recursive initialization from the same thread.

## Identity and retention

Keys are opaque, cloneable, equal and hashable. SQLite keys canonicalize an
existing file, including symlinks. For a new file they canonicalize the existing
parent and append a valid filename. Missing parents, directories, unresolved
symlinks, non-UTF8 paths, names containing `?` or `#`, names starting with
`file:` or `sqlite:`, and exactly `:memory:` are rejected. Other colons, such as
Windows drive letters, are literal path characters. Resolve keys
synchronously off the UI thread; callers must not replace/delete a database
file or change symlink targets while its core is registered.

Postgres keys require a nonempty stable nonsecret app-assigned namespace. Capture
the DSN separately in the callback. Use the same namespace for all aliases of
one database: the registry does not infer URL equivalence. Every open requires
a nonempty stable nonsecret `configuration_id` covering schema, projectors and
snapshot policy. A different ID returns `ConfigurationConflict` while the
entry's core is initializing or ready; it never silently reconfigures a core.
After a failed initialization no core exists, so the next explicit open may use
a corrected ID without restarting the process.

`process()` retains one registry/runtime per linked app library. Entries are
keyed by aggregate `TypeId` plus database key, so distinct aggregate types do not
share a core. Repeated/concurrent opens reuse the same successful `Arc`, bus and
engine, including wrapper/activity recreation. Short map locks never span user
code or await. `new(runtime)` deliberately creates an isolated registry for tests
or other hosts. This is neither a cross-process lock nor a registry shared by
separately loaded native libraries, and it cannot prevent unmanaged app stacks.

## Runtime, errors and cancellation

The owned multithread Tokio runtime uses one worker and enables I/O/time drivers.
Both the operation closure and its future execute on it, even when called from
another Tokio runtime or a foreign callback. Domain errors pass through unchanged;
apps choose their own `CommandBusError` policy and implement `From<FfiError>`.
There is no synchronous `block_on` API and no implicit UniFFI Tokio selector.
One worker does not serialize async mutations: backend contention behavior and
application retry/serialization policy remain the existing store/app contract.

Keep production owners process-lived. Test/local owners use nonblocking runtime
teardown so dropping from an async callback is safe; there is no graceful shutdown
guarantee, close or eviction API. A submitted core operation retains its runtime
until completion. Cancelling a caller detaches the operation; cancelling the
first opener does not cancel initialization. No automatic retry follows a
cancelled wait. Re-read authoritative state before resubmitting a mutation.

`FfiError` has fixed safe unit variants: `InvalidDatabasePath`,
`InvalidConfiguration`, `ConfigurationConflict`, `RuntimeUnavailable`,
`SchemaInitializationFailed`, `StackInitializationFailed`,
`ProjectionRebuildFailed`, `InitializationPanicked`, `OperationPanicked`, and
`TaskCancelled`. It implements Clone/Debug/Eq/PartialEq and thiserror::Error.
Infrastructure logs include stage/category only; errors never expose raw
path, DSN, SQL or panic payloads. Rust's host panic hook is separate from this
error boundary; hosts control their panic logging policy.

## UniFFI application wrapper and bindings

Export a concrete application object containing `Arc<FfiCore<MyAggregate>>`.
Derive `uniffi::Object` and use ordinary async exported methods, as the runnable
fixture does. Do not export generic aggregates, shared stacks or closures.
In the final app cdylib call each exactly once:

```rust,ignore
uniffi::setup_scaffolding!();
arc_ffi::uniffi_reexport_scaffolding!();
```

Generate **both** components from the final app cdylib in library mode, targeting
that same library: app exports and external `arc_ffi` error bindings have distinct
namespaces/packages. Compile the generated error component too; do not copy its
definition or replace the components with one UDL. This follows UniFFI's
[dependency scaffolding](https://mozilla.github.io/uniffi-rs/latest/tutorial/Rust_scaffolding.html#libraries-that-depend-on-uniffi-components)
and [external type](https://mozilla.github.io/uniffi-rs/latest/types/remote_ext_types.html) contracts.

```kotlin
// In a lifecycle-owned coroutine, keep synchronous filesystem key resolution off UI.
val core = withContext(Dispatchers.IO) { NativeCore.open(databasePath) }
val value = core.createAndRead("entry-id")
// Handle uniffi.arc_ffi.FfiException alongside app-owned domain exceptions.
```

```swift
// Keep synchronous key resolution off the main actor.
let core = try await Task.detached {
    try await NativeCore.open(path: databasePath)
}.value
let value = try await core.createAndRead(id: "entry-id")
```

## Verification and packaging

The standalone fixture is `crates/arc-ffi/tests/fixtures/native-app`. It prepares
its own synthetic SQLite schema/projector, exports async create/read and returns
external `FfiError`. Run from the repository root with `CARGO_BUILD_JOBS=4`:

```bash
cargo test -p arc-ffi
cargo test -p arc-ffi --no-default-features
cargo test -p arc-ffi --no-default-features --features sqlite
cargo test -p arc-ffi --no-default-features --features postgres
cargo test -p arc-ffi --all-features
bash crates/arc-ffi/tests/doctor-regression.sh
bash crates/arc-ffi/tests/fixtures/native-app/smoke.sh
make doctor
make publish-check
```

The smoke script builds the real host cdylib, generates Kotlin and Swift from it,
and compiles/runs the JVM smoke with JNA and coroutines. Supply `KOTLIN_CLASSPATH`
(compiler/stdlib/script-runtime/reflect/annotations/trove/coroutines jars),
`KOTLIN_RUNTIME_CLASSPATH` (stdlib/JNA/coroutines) if `kotlinc` is unavailable.
Swift source generation is checked everywhere; Swift compile/run needs a
configured Swift host and remains pending when absent. No Android/iOS device
acceptance is implied by the host smoke. Full workspace checks and final-head CI
remain integration gates.

Package verification compiles extracted archives and the standalone concrete
consumer, with no workspace sources for Arc dependencies. Doctor rejects domain,
services, validation or copied assembly in `arc-ffi`, and checks isolated
normal/build dependency trees for default, no-default, SQLite, Postgres and all
features. Publish after core/storage/`arc-stack`; `arc-web` and `arc-ffi` are
siblings, followed by the existing auth/CLI order. No publishing or credential
change is authorized by this implementation.
