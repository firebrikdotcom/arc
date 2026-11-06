# Your first Android app

This guide creates a native Android application that uses Arc through UniFFI.
It does not add a web server or copy the Android prototype into your project.
The generated project depends on the published `arc-core`, `arc-stack`, and
`arc-ffi` crates, pinned to the same version as your `arc` CLI (0.9.0,
published to crates.io).

## Before you start

Use Linux. Android builds need the Rust toolchain
and Android Rust targets, `cargo-ndk`, JDK 17 or newer, Android SDK platform 36,
build-tools 35.0.0, NDK 27.0.12077973, `adb`, and the Android emulator tools.
Start an Android Virtual Device before the emulator test. `arc doctor --android`
checks these tools without installing anything or downloading packages. It
reports missing items and gives the relevant installation command. A running
emulator is needed for emulator tests, not for generating the project.

The generated `rust/Cargo.toml` pins each direct Arc crate to the CLI's exact
version, for example `arc-ffi = { version = "=0.9.0" }`, so upgrading Arc in an
existing project is a deliberate edit of those three lines.

## Create, build, and test

Run these commands in order from a working directory where you want the new
project. The build and emulator steps were verified with JDK 21, NDK 27, and the
`emulator-5580` Medium_Phone_API_36.1 virtual device.

```bash
cargo install arc-web-cli --version 0.9.0 --locked
arc doctor --android
arc new smoke-notes --android --no-git
cd smoke-notes
arc generate resource Note --ffi
arc generate resource Tag --ffi
make bindings
make test
make build
make e2e-emulator E2E_DEVICE=emulator-5580
```

`cargo install` installs the `arc` CLI from crates.io. `arc new` creates the
Android project and its manifest. `arc generate resource` adds a native resource; resource names use
the same validation and normalization rules as Arc's other resource
generators. `make bindings` generates Kotlin bindings for the app and
`arc-ffi`. `make test` runs the Rust and JVM unit tests. `make build` builds Rust
libraries for both Android ABIs and assembles the debug APK. In the verified
resource project, the debug APK was about 36 MB; an empty project APK was about
28 MB. These are observed reference sizes, not size guarantees.

`make e2e-emulator` instruments an Android emulator only. Its default device is
`emulator-5580`; the command checks that the selected device is a ready
emulator before Gradle runs and refuses physical devices. To select another
emulator, set `E2E_DEVICE` to its serial. The instrumented tests use the app's
real `app.sqlite`, and Gradle uninstalls the app after the run, so e2e erases
the app and all of its data on the selected emulator. If that emulator's app
data matters, deploy to a different device or emulator than the one e2e uses.

`make deploy` is separate from emulator testing. It requires an explicit
connected device serial and performs an in-place install, launch and
package/version check against the `versionName` and `versionCode` in
`app/build.gradle.kts`. To deploy, run `make deploy DEVICE=<serial>` only for a
device you intend to update. Deployment does not run instrumentation, uninstall
the app, or clear its data. Do not use `adb uninstall` or clear application
storage as part of an update.

## Developing against a local Arc checkout

Framework contributors can build a generated project against an Arc source
checkout instead of crates.io. Pass `--arc-path PATH` to `arc new`, or set
`ARC_PATH`; the explicit option takes precedence. The generator verifies the
checkout's `arc-core`, `arc-stack`, and `arc-ffi` manifests before writing
anything, then emits absolute path dependencies with the checkout's version.
Keep that checkout available while building. `--arc-path` is valid only with
`--android`.

```bash
cargo install --path /path/to/arc-checkout/crates/arc-cli --locked --force
arc new smoke-notes --android --arc-path /path/to/arc-checkout --no-git
```

## What the generator owns

The project includes Rust and Kotlin source, Gradle files and wrapper, a root
Makefile, and `.arc/android.json`. The manifest identifies the project and
records a SHA-256 hash of each resource's files and registration blocks as Arc
wrote them. Resource ownership is checked against those recorded hashes, never
against what a newer CLI's templates would render, so upgrading the CLI does
not block adding resources. The skeleton `files` hashes are only a baseline
recorded by `arc new`: nothing reads or refreshes them, and shared files such
as `ffi.rs` or `ResourceCatalog.kt` diverge from them by design once resource
blocks are inserted.

Adding a resource writes only its own new files, its blocks at the named
markers in shared files, and the manifest. You can edit another resource's
files or blocks: that does not block generating a new resource, and Arc never
overwrites those edits. Repeating an unchanged resource request is a no-op.
Repeating it after any of its files or blocks were edited or removed is a
conflict that names each changed file, and nothing is written. A target that
already exists without a registration, a missing or duplicated marker, or an
invalid manifest is also a conflict. There is no force option.

Manifests written before hashes were recorded (`format_version` 1) still work.
Their skeleton files have no recorded hashes and are treated as unknown
ownership: new resources can be added, and those files are never rewritten.
The manifest is upgraded to `format_version` 2 the next time a resource is
added.

The Rust project has its own lockfile, as does the host-only UniFFI bindgen
tool. The first Cargo command can create a missing lockfile; later commands
use locked dependencies. Keep both lockfiles under version control.

An empty project does not create a database. When its first resource opens the
native core, Arc initializes the SQLite schema and recorded migrations. The
database lives in the app's private files directory. Schema migrations run
once and atomically; rollback SQL is not run automatically. Resource reads
use Arc's read model and writes use its command bus. If a write result is
uncertain after an error, check the stored state before trying again.

For framework-level native adapter details, see [Native Adapters](ffi.md).
For the overall application creation path, see [Getting started](getting-started.md).


The repository acceptance script can also exercise the scaffold. Its default
mode runs host checks and builds without emulator instrumentation;
`ARC_ANDROID_E2E=1 E2E_DEVICE=emulator-5580 bash scripts/check-android-scaffold.sh`
adds emulator tests and an in-place reinstall persistence check. Physical device
serials are refused in that mode.
