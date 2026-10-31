# ADR 0004 — Native Android application scaffolding

- Status: **Accepted**
- Date: 2026-10-30
- Deciders: Arc framework architecture (U-4, Q-17, Q-17a)
- Related: [ADR 0003: Shared Stack UI Adapters](0003-shared-stack-ui-adapters.md), [Your first Android app](../android.md), `progress.md`

## Context

Arc developers should be able to start a native Android application from the
Arc CLI, add a resource, build its UniFFI bindings and APK, and test or deploy
it using documented Make targets. They should not need to copy files from the
prototype app or guess which local Arc checkout supplies unpublished crates.
The generated project must use the framework's native FFI path, preserve
application-owned data, and clearly distinguish emulator testing from an
explicit device install.

Until a separately authorized 0.8.12 release is published, the Android
framework crates are not available from crates.io. The generator therefore
needs an explicit local checkout and must record the dependency paths that it
uses. Empty scaffolds also need to remain usable without opening a database
that has no resources to store.

## Decision

Add an Android target to the existing CLI without changing existing web
commands or their defaults:

- `arc new <name> --android [--arc-path PATH] [--no-git]` creates a native,
  FFI-only Android project.
- `arc generate resource <name> --ffi` (including the existing `aggregate`
  alias) adds a native resource to an Android project.
- `arc doctor --android` reports Android build prerequisites without installing
  tools or downloading packages.
- `--android` cannot be combined with `--ui` or `--api`; `--arc-path` is valid
  only with `--android`; `--ffi` cannot be combined with web UI/API or auth
  options and requires an Android project. Android resources require `--ffi`.
  Invalid requests fail before writing files. Existing web behavior, aliases,
  defaults, help handling and diagnostics remain unchanged.

### Specification

**Local dependencies and project identity.** Until the separately authorized
release, creation requires `--arc-path PATH` or `ARC_PATH`; the explicit flag
takes precedence. The path is canonicalized and safely represented in TOML.
Before writing, the CLI verifies that it identifies a matching checkout with
`arc-core`, `arc-stack` and `arc-ffi` manifests at one workspace version. The
generated Rust manifest records absolute path dependencies and the actual
checkout version. It does not guess a sibling directory, use a fictional
0.8.12 registry version, add a git dependency, publish crates, or rewrite
existing paths if `ARC_PATH` later changes.

The generated Rust package and library name is `S_core`, where `S` is the
lowercase project name with hyphens replaced by underscores. The Android
namespace and application ID are `com.example.arc.app_S`, substituting `S`
literally; the `app_` prefix keeps normalized name segments away from language
keywords. Existing project-name validation applies. This milestone has no
package-name override.

**Generated layout and toolchain.** The project contains a root `Makefile`,
`README.md`, `.gitignore`, managed `.arc/android.json` manifest, Gradle build
files and checked-in wrapper (scripts and jar), `app/`, `rust/`, and a
host-only `tools/uniffi-bindgen/` workspace. The manifest uses
`format_version=2` and records the Android target, original and normalized
names, application ID, SHA-256 hashes of the generated skeleton files, and
registered resources. Format 1 manifests, which stored whole file bodies and
no skeleton hashes, remain readable and are upgraded when a resource is added. Rust is a standalone
workspace with `rlib` and `cdylib` outputs, domain modules, migrations and
tests. Kotlin application sources use the generated package. Rust JNI output
and generated UniFFI bindings stay under `app/build/`.

The reference pins Gradle 8.13, Android Gradle Plugin 8.10.1, Kotlin/Compose
plugin 2.1.0, SDK 36, build tools 35.0.0, min SDK 30, JDK 17 or newer with Java/JVM target 17, NDK
27.0.12077973, and `arm64-v8a` plus `x86_64`. UniFFI is exactly 0.32.2: the
Android library disables default features and CLI support; the separate host
bindgen tool uses the CLI and cargo-metadata features. The project is
FFI-only: no web server, WebView, `INTERNET` or microphone permission, audio
recording, or prototype identity/device default is generated.

**Make targets and safety.** `make bindings` builds the host library and
bindgen tool, then generates both the application and `arc-ffi` Kotlin
components from that library. It resolves Cargo's actual target directory and
host library extension instead of assuming a Linux path. `make rust` builds
both Android ABIs with `cargo ndk`; `make build` runs Rust, bindings and the
debug APK build. `make test` runs bindings, host Rust tests and JVM unit tests.
`make e2e-emulator` builds and instruments only the selected emulator, default
`emulator-5580`; it validates readiness and `ro.kernel.qemu=1` before Gradle
and refuses physical devices. `make deploy DEVICE=<serial>` requires an
explicit connected serial, installs with `adb -s DEVICE install -r`, launches
and verifies the package/version. It does not uninstall, clear data, run
instrumentation, connect a device automatically, or default to a phone.

Missing Rust lockfiles are created once with `cargo generate-lockfile`; later
Cargo commands use `--locked`, and generated-project guidance tells developers
to commit both locks. Generation itself does not download or build.
Prerequisites are checked at the stage that needs them so `make -j` does not
cause overlapping writes to generated bindings or JNI output.

**Resource and data contract.** Resource generation reuses Arc's current
resource normalization, aggregate, commands, events, projector, module and SQL
templates; it does not change web output or copy a separate domain
implementation. Android projects use schema-at-open rather than a new global
`arc migrate` command. The first resource's typed FFI core initializes the
base and generated schema migrations through the registry callback. An empty
chooser has no persistence operations and does not create or open SQLite. The
database is `context.filesDir/app.sqlite`; schema migrations are applied once
in a transaction and `down.sql` is not run automatically.

Generated resources use the shared `arc-ffi` core registry and command bus.
They expose typed CRUD operations and errors; writes are not automatically
retried after an uncertain result. The generated chooser lists registered
resources. Generation preflights names, normalized collisions, files and
registration markers before writing. `.arc/android.json` records generated
ownership as SHA-256 hashes taken at generation time; checks compare files
with those hashes, never with a fresh render, so template changes in later CLI
versions do not block new resources. Adding a resource checks only its own
targets and the shared markers, so edits to another resource's files do not
block it and are never overwritten. Repeating an unchanged registered resource
is a no-op; repeating one whose owned files or blocks changed is a conflict
naming each file. There is no force option.

## Backward compatibility

This is additive CLI behavior. Existing web commands, option combinations,
defaults, aliases, help-before-execution rules, generated web output, and
resource repeat/refusal behavior remain unchanged. Android generation is
explicit; it does not convert an existing web project. The generated project
uses local dependency paths until an authorized release makes registry
dependencies available.

## Consequences

### Positive

- Developers have one documented route from project creation to native CRUD,
  emulator validation and explicit deployment.
- Android uses the same Arc domain and stack APIs through UniFFI rather than a
  copied framework implementation.
- Tool requirements, local framework dependencies and device-safety rules are
  visible before building or installing.

### Costs and risks

- Developers need a matching Arc checkout until the unpublished crates are
  released.
- Android builds require a pinned Rust/NDK/SDK/Java toolchain and can be
  slower than host-only work.
- Generated-file ownership and migration tracking add rules the CLI must
  enforce without damaging handwritten code or existing data.
- Emulator and deployment targets have different safety properties; commands
  and documentation must keep that distinction explicit.

## Rollout

| Task | Scope | Evidence |
|---|---|---|
| T2a | CLI flags, Android doctor, project scaffold and build Makefile. | CLI tests; empty project builds and launches; old CLI tests stay green. |
| T2b | FFI resource generation, schema and Kotlin UI integration. | Two-resource generated project tests; repeat/conflict guarantees; web tests. |
| T2c | Generator acceptance tests and CI wiring. | Clean-room host checks and actual emulator acceptance with captured evidence. |
| T2d | User documentation and roadmap. | `make docs-check`; documented commands exercised against tested CLI and emulator. |

The coordinator merges in dependency order, runs combined mandatory checks and
records CI. No reference-app changes, publication, or device installation are
included in this task.

## Follow-ups

- Revisit registry dependencies only after a separately authorized 0.8.12
  release.
- Keep app migration and any installation of the reference app outside this
  scaffold milestone.

## Implementation record

The coordinator verified the generated workflow on this machine with JDK 21,
NDK 27, and `emulator-5580` (Medium_Phone_API_36.1): `arc new smoke-notes
--android --arc-path <checkout> --no-git`; `arc generate resource Note --ffi`;
`arc generate resource Tag --ffi`; `make bindings`; `make test` (Rust and JVM
tests passed); `make build` (debug APK about 36 MB; empty-project APK about
28 MB); and `make e2e-emulator E2E_DEVICE=emulator-5580` (chooser, Note, and Tag
instrumented flows passed through real FFI).

T2c also verified `scripts/check-android-scaffold.sh` in default mode and with
`ARC_ANDROID_E2E=1 E2E_DEVICE=emulator-5580` (five instrumentation tests and
an in-place reinstall persistence check), the host integration test
`crates/arc-cli/tests/android_generated.rs`, and
`.github/workflows/android-scaffold.yml`. Emulator acceptance refuses physical
serials. The generated CLI help was checked against the built CLI; its command
reference is in [Your first Android app](../android.md).
