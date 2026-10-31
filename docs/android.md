# Your first Android app

This guide creates a native Android application that uses Arc through UniFFI.
It does not add a web server or copy the Android prototype into your project.
The generated project uses local Arc crate paths until an authorized 0.8.12
release is published.

## Before you start

Use Linux with a matching Arc checkout. Android builds need the Rust toolchain
and Android Rust targets, `cargo-ndk`, JDK 17 or newer, Android SDK platform 36,
build-tools 35.0.0, NDK 27.0.12077973, `adb`, and the Android emulator tools.
Start an Android Virtual Device before the emulator test. `arc doctor --android`
checks these tools without installing anything or downloading packages. It
reports missing items and gives the relevant installation command. A running
emulator is needed for emulator tests, not for generating the project.

The project generator also needs the Arc source checkout. Set `ARC_PATH` to
that checkout, or pass `--arc-path PATH` to `arc new`; the explicit option takes
precedence. It verifies `arc-core`, `arc-stack`, and `arc-ffi` before writing
the project. Generated manifests retain absolute paths to that checkout, so
keep it available while building.

## Create, build, and test

Run these commands in order from a working directory where you want the new
project. This sequence was verified with JDK 21, NDK 27, and the
`emulator-5580` Medium_Phone_API_36.1 virtual device.

```bash
export ARC_PATH=/path/to/arc-checkout
cargo install --path "$ARC_PATH/crates/arc-cli" --locked --force
arc doctor --android
arc new smoke-notes --android --arc-path "$ARC_PATH" --no-git
cd smoke-notes
arc generate resource Note --ffi
arc generate resource Tag --ffi
make bindings
make test
make build
make e2e-emulator E2E_DEVICE=emulator-5580
```

`cargo install` uses the local CLI source. It does not install an unpublished
CLI version from crates.io. `arc new` creates the Android project and its
manifest. `arc generate resource` adds a native resource; resource names use
the same validation and normalization rules as Arc's other resource
generators. `make bindings` generates Kotlin bindings for the app and
`arc-ffi`. `make test` runs the Rust and JVM unit tests. `make build` builds Rust
libraries for both Android ABIs and assembles the debug APK. In the verified
resource project, the debug APK was about 36 MB; an empty project APK was about
28 MB. These are observed reference sizes, not size guarantees.

`make e2e-emulator` instruments an Android emulator only. Its default device is
`emulator-5580`; the command checks that the selected device is a ready
emulator before Gradle runs and refuses physical devices. To select another
emulator, set `E2E_DEVICE` to its serial.

`make deploy` is separate from emulator testing. It requires an explicit
connected device serial and performs an in-place install, launch and
package/version check. To deploy, run `make deploy DEVICE=<serial>` only for a
device you intend to update. Deployment does not run instrumentation, uninstall
the app, or clear its data. Do not use `adb uninstall` or clear application
storage as part of an update.

## What the generator owns

The project includes Rust and Kotlin source, Gradle files and wrapper, a root
Makefile, and `.arc/android.json`. The manifest identifies the project and
records generated resources and file ownership. Generated files can be
updated only when their recorded contents still match. An unchanged repeated
resource request is a no-op; missing, edited, or unregistered generated
content is a conflict. There is no force option, and handwritten changes are
not overwritten.

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
