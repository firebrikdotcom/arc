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
The managed .arc/android.json manifest records a SHA-256 hash of each generated file and block,
and named registration markers support resource generation. Editing one resource never blocks adding another,
and Arc never overwrites your edits; repeating a resource whose files you changed is reported as a conflict.

Instrumentation is emulator-only. e2e uses the real app.sqlite and Gradle uninstalls the app afterwards,
so it erases the app and all of its data on the selected emulator; deploy elsewhere when that data matters.
Physical devices are refused.
`deploy` requires an explicit authorized connected serial, installs with -r, launches and verifies the
versionName/versionCode from app/build.gradle.kts;
it never connects automatically, clears data, uninstalls or runs instrumentation.
