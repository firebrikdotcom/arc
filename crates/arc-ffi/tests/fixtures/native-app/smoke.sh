#!/usr/bin/env bash
# Build/generate from the final app cdylib; no UniFFI tooling in runtime dependencies.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../../../.." && pwd)"
fixture="$root/crates/arc-ffi/tests/fixtures/native-app"
export CARGO_BUILD_JOBS=4
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
out="$CARGO_TARGET_DIR/ffi-native"
mkdir -p "$out/generated"
cargo build --manifest-path "$fixture/Cargo.toml"
cargo build --manifest-path "$fixture/bindgen/Cargo.toml"
case "$(uname -s)" in
  Darwin) library="$CARGO_TARGET_DIR/debug/libarc_ffi_native_app.dylib" ;;
  Linux) library="$CARGO_TARGET_DIR/debug/libarc_ffi_native_app.so" ;;
  *) echo 'Unsupported native smoke host' >&2; exit 1 ;;
esac
"$CARGO_TARGET_DIR/debug/arc-ffi-fixture-bindgen" generate --library "$library" --language kotlin --language swift --out-dir "$out/generated" --no-format
for file in uniffi/arc_ffi/arc_ffi.kt uniffi/arc_ffi_native_app/arc_ffi_native_app.kt arc_ffi.swift arc_ffi_native_app.swift; do
  test -s "$out/generated/$file" || { echo "Missing generated component: $file" >&2; exit 1; }
done
# Use explicitly supplied compiler/runtime jars, or existing Gradle cache artifacts.
if [[ -z "${KOTLIN_RUNTIME_CLASSPATH:-}" || -z "${KOTLIN_CLASSPATH:-}" ]]; then
  eval "$(python3 - <<'PY'
from pathlib import Path
import os, shlex
cache = Path.home()/'.gradle/caches/modules-2/files-2.1'
def jar(group, name, version):
    matches = sorted((cache/group/name/version).glob('*/*.jar'))
    if not matches: raise SystemExit(f'Missing {name} {version}; supply KOTLIN_CLASSPATH/KOTLIN_RUNTIME_CLASSPATH')
    return str(matches[0])
stdlib = jar('org.jetbrains.kotlin','kotlin-stdlib','2.0.21')
coroutines = jar('org.jetbrains.kotlinx','kotlinx-coroutines-core-jvm','1.9.0')
annotations = jar('org.jetbrains','annotations','24.0.1')
runtime = [stdlib, coroutines, jar('net.java.dev.jna','jna','5.12.1'), annotations]
compiler = [jar('org.jetbrains.kotlin','kotlin-compiler-embeddable','2.0.21'), stdlib,
    jar('org.jetbrains.kotlin','kotlin-reflect','1.6.10'),
    jar('org.jetbrains.kotlin','kotlin-script-runtime','2.0.21'),
    jar('org.jetbrains.intellij.deps','trove4j','1.0.20200330'), coroutines, annotations]
for key, values in [('KOTLIN_CLASSPATH',compiler),('KOTLIN_RUNTIME_CLASSPATH',runtime)]:
    print(f'export {key}={shlex.quote(os.environ.get(key) or ":".join(values))}')
PY
)"
fi
: "${KOTLIN_CLASSPATH:?Supply Kotlin compiler jars}"
: "${KOTLIN_RUNTIME_CLASSPATH:?Supply Kotlin runtime jars}"
java -cp "$KOTLIN_CLASSPATH" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler \
  -no-stdlib -no-reflect -classpath "$KOTLIN_RUNTIME_CLASSPATH" \
  "$out/generated/uniffi/arc_ffi/arc_ffi.kt" "$out/generated/uniffi/arc_ffi_native_app/arc_ffi_native_app.kt" "$fixture/Smoke.kt" -d "$out/smoke.jar"
database_dir=$(mktemp -d)
trap 'rm -rf "$database_dir"' EXIT
java -Djna.library.path="$CARGO_TARGET_DIR/debug" -cp "$out/smoke.jar:$KOTLIN_RUNTIME_CLASSPATH" SmokeKt "$database_dir/smoke.sqlite"
if ! command -v swiftc >/dev/null; then
  echo 'Swift generation passed; Swift compile/run pending (no configured Swift compiler).'
else
  echo 'Swift sources generated; compile/run requires configured component modules on the Swift host.'
fi
