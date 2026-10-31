#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_BUILD_JOBS=4
# Build the generated app against this checkout, not the published crates.
export ARC_PATH="$root"
export CARGO_TARGET_DIR="${ARC_ANDROID_SCAFFOLD_TARGET:-$root/target/android-scaffold}"
if [[ ${ARC_ANDROID_E2E:-0} == 1 && ! ${E2E_DEVICE:-emulator-5580} =~ ^emulator-[0-9]+$ ]]; then
  echo 'ERROR: e2e-emulator requires a running Android emulator; physical devices are forbidden' >&2
  exit 1
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo build --locked -p arc-web-cli --bin arc --target-dir "$work/cli-target"
cli="$work/cli-target/debug/arc"
"$cli" doctor --android
(cd "$work" && "$cli" new sample --android)
cd "$work/sample"
"$cli" generate resource Note --ffi
"$cli" generate resource Tag --ffi
make bindings
make test
make build
if cargo tree --locked --manifest-path rust/Cargo.toml --target aarch64-linux-android -e normal,build | grep -E '(^|[[:space:]│├└─])(arc-web|actix-web|tera) v'; then
  echo 'ERROR: Android dependency tree includes web dependencies' >&2
  exit 1
fi
if cargo tree --locked --manifest-path rust/Cargo.toml --target aarch64-linux-android -e features | grep -F 'uniffi feature "cli"'; then
  echo 'ERROR: Android dependency tree includes the UniFFI CLI' >&2
  exit 1
fi
if [[ ${ARC_ANDROID_E2E:-0} == 1 ]]; then
  serial=${E2E_DEVICE:-emulator-5580}
  marker="upgrade-$(date +%s%N)"
  test_dir=app/src/androidTest/java/com/example/arc/app_sample
  mkdir -p "$test_dir"
  cat > "$test_dir/UpgradeTest.kt" <<KOTLIN
package com.example.arc.app_sample
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import android.content.Context
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Test
import org.junit.FixMethodOrder
import org.junit.runners.MethodSorters
import org.junit.runner.RunWith
import uniffi.sample_core.NativeCore
@RunWith(AndroidJUnit4::class)
@FixMethodOrder(MethodSorters.NAME_ASCENDING)
class UpgradeTest {
    private fun core() = runBlocking {
        NativeCore.open(File(ApplicationProvider.getApplicationContext<Context>().filesDir, "app.sqlite").canonicalPath)
    }
    @Test fun aSeed() = runBlocking { core().createNote("$marker"); Unit }
    @Test fun bVerify() = runBlocking {
        val core = core()
        val record = core.listNotes().single { it.name == "$marker" }
        core.deleteNote(record.id)
    }
}
KOTLIN
  make e2e-emulator E2E_DEVICE="$serial"
  adb -s "$serial" install -r app/build/outputs/apk/debug/app-debug.apk
  adb -s "$serial" install -r app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
  runner=com.example.arc.app_sample.test/androidx.test.runner.AndroidJUnitRunner
  check_instrumentation() {
    local output
    output=$(adb -s "$serial" shell am instrument -w -e class "com.example.arc.app_sample.UpgradeTest#$1" "$runner")
    printf '%s\n' "$output"
    grep -Fq 'OK (1 test)' <<<"$output"
  }
  check_instrumentation aSeed
  adb -s "$serial" install -r app/build/outputs/apk/debug/app-debug.apk
  check_instrumentation bVerify
else
  echo 'Android emulator acceptance: NOT RUN (set ARC_ANDROID_E2E=1)'
fi
