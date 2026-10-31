# {{project-name}}

An FFI-only Arc/Compose app. An empty project opens a resource chooser without creating a database.

The generated direct Arc dependencies use absolute paths to the explicitly selected local checkout.
Keep that checkout available until a matching published release is available. Generation does not download tools.

Install Rust, cargo-ndk, the aarch64-linux-android and x86_64-linux-android Rust targets,
JDK 17, Android SDK 36, build-tools 35.0.0, NDK 27.0.12077973, platform-tools and emulator.
Set ANDROID_HOME (or ANDROID_SDK_ROOT), JAVA_HOME and optionally ANDROID_NDK_HOME.
Run `arc doctor --android` for actionable diagnostics; licenses must be accepted manually.

```sh
arc generate resource Note --ffi
make bindings
make test
make build
make e2e-emulator E2E_DEVICE=emulator-5580
make deploy DEVICE=emulator-5580
```

`bindings` needs host Rust only. `test` adds Java and SDK; `build` adds NDK and Android targets.
The first make invocation creates missing rust/Cargo.lock and tools/uniffi-bindgen/Cargo.lock;
commit both lockfiles. Subsequent Cargo commands use --locked. Bindgen is host-only.
Rust and generated Kotlin components load the same {{crate-name}} native library.

Resources apply embedded SQL migrations once at core open under an IMMEDIATE transaction.
The app database is filesDir/app.sqlite. Existing data is retained; down migrations never run automatically.
The managed .arc/android.json manifest and named registration markers support resource generation.
Preserve generated resource ownership; customized/missing files and marker conflicts must be resolved manually.

Instrumentation is emulator-only and may alter its app test data. Physical devices are refused.
`deploy` requires an explicit authorized connected serial, installs with -r, launches and verifies the version;
it never connects automatically, clears data, uninstalls or runs instrumentation.
