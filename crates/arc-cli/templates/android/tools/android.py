#!/usr/bin/env python3
"""Stage-scoped local build tooling. Never downloads SDKs or accepts licenses."""
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
os.chdir(ROOT)
os.environ.setdefault("CARGO_BUILD_JOBS", "4")
LIB = "{{crate-name}}"
PACKAGE = "{{application-id}}"
NDK = "27.0.12077973"
GRADLE = "gradlew.bat" if os.name == "nt" else "./gradlew"


def fail(message):
    raise SystemExit("ERROR: " + message)


def run(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def output(args):
    return run(args, capture_output=True, text=True).stdout.strip()


def host_tools():
    for name in ("cargo", "rustc", "rustup"):
        if not shutil.which(name):
            fail("cargo/rustc/rustup not found; install the Rust toolchain with rustup")
        run([name, "--version"], stdout=subprocess.DEVNULL)


def lock(manifest):
    if not manifest.with_name("Cargo.lock").exists():
        run(["cargo", "generate-lockfile", "--manifest-path", manifest])


def metadata(manifest):
    lock(manifest)
    return json.loads(output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1", "--manifest-path", manifest]))


def bindings():
    host_tools()
    host = next(line.split(": ", 1)[1] for line in output(["rustc", "-vV"]).splitlines() if line.startswith("host: "))
    manifest = ROOT / "rust/Cargo.toml"
    target = Path(metadata(manifest)["target_directory"])
    tool_manifest = ROOT / "tools/uniffi-bindgen/Cargo.toml"
    tool_target = Path(metadata(tool_manifest)["target_directory"])
    run(["cargo", "build", "--locked", "--manifest-path", manifest, "--lib", "--target", host])
    run(["cargo", "build", "--locked", "--release", "--manifest-path", tool_manifest, "--target", host])
    extension = ".dylib" if platform.system() == "Darwin" else ".dll" if os.name == "nt" else ".so"
    library = target / host / "debug" / (("" if os.name == "nt" else "lib") + LIB + extension)
    bindgen = tool_target / host / "release" / ("uniffi-bindgen.exe" if os.name == "nt" else "uniffi-bindgen")
    destination = ROOT / "app/build/generated/uniffi"
    # Generate each component with its own namespace and the SAME library name.
    # --crate filters output after discovery, retaining external-type metadata.
    for crate in (LIB, "arc_ffi"):
        run([bindgen, "generate", "--no-format", "--library", library, "--crate", crate,
             "--config", ROOT / "uniffi-bindgen.toml", "--language", "kotlin", "--out-dir", destination], cwd=ROOT / "rust")


def sdk_tools(native=False):
    sdk = Path(os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT") or Path.home() / "Android/Sdk")
    if not sdk.is_dir():
        fail("Android SDK not found; set ANDROID_HOME to your SDK directory")
    os.environ["ANDROID_HOME"] = str(sdk)
    if not (sdk / "platforms/android-36/android.jar").is_file():
        fail('Android SDK platform 36 missing; run sdkmanager "platforms;android-36"')
    if not (sdk / "build-tools/35.0.0/aapt2").is_file():
        fail('Android build-tools 35.0.0 missing; run sdkmanager "build-tools;35.0.0"')
    java_home = os.environ.get("JAVA_HOME")
    java = str(Path(java_home) / "bin/java") if java_home else "java"
    javac = str(Path(java_home) / "bin/javac") if java_home else "javac"
    for tool in (java, javac):
        try:
            process = run([tool, "-version"], capture_output=True, text=True)
            version = process.stdout + process.stderr
            if java_major(version) < 17:
                fail("JDK 17 or newer not found; set JAVA_HOME to a JDK 17+ installation")
        except (FileNotFoundError, subprocess.CalledProcessError):
            fail("JDK 17 or newer not found; set JAVA_HOME to a JDK 17+ installation")
    if native:
        host_tools()
        if not shutil.which("cargo-ndk"):
            fail("cargo-ndk not found; run cargo install cargo-ndk --locked")
        run(["cargo", "ndk", "--version"], stdout=subprocess.DEVNULL)
        ndk = Path(os.environ.get("ANDROID_NDK_HOME") or sdk / "ndk" / NDK)
        properties = ndk / "source.properties"
        values = dict(line.split("=", 1) for line in properties.read_text().splitlines() if "=" in line) if properties.is_file() else {}
        if not any(key.strip() == "Pkg.Revision" and value.strip() == NDK for key, value in values.items()):
            fail('Android NDK 27.0.12077973 missing or mismatched; run sdkmanager "ndk;27.0.12077973" and set ANDROID_NDK_HOME')
        os.environ["ANDROID_NDK_HOME"] = str(ndk)
        installed = output(["rustup", "target", "list", "--installed"]).splitlines()
        missing = sorted(set(("aarch64-linux-android", "x86_64-linux-android")) - set(installed))
        if missing:
            fail("Rust Android targets missing: " + ", ".join(missing) + "; run rustup target add aarch64-linux-android x86_64-linux-android")


def java_major(version):
    match = re.search(r'(?<!\d)(\d+(?:\.\d+)*)(?!\d)', version)
    if not match:
        return 0
    parts = [int(part) for part in match.group(1).split(".")]
    return parts[1] if parts[0] == 1 and len(parts) > 1 else parts[0]


def emulator_tool(sdk):
    candidate = sdk / "emulator" / ("emulator.exe" if os.name == "nt" else "emulator")
    if candidate.is_file():
        return candidate
    return shutil.which("emulator")


def rust():
    sdk_tools(native=True)
    lock(ROOT / "rust/Cargo.toml")
    jni = ROOT / "app/build/rustJniLibs"
    if jni.exists():
        shutil.rmtree(jni)
    run(["cargo", "ndk", "-t", "arm64-v8a", "-t", "x86_64", "-o", jni, "build", "--release", "--locked"], cwd=ROOT / "rust")


def gradle(task):
    sdk_tools()
    run([GRADLE, "--console=plain", task])


def device_ready(serial, emulator=False):
    refusal = "e2e-emulator requires a running Android emulator; physical devices are forbidden"
    if emulator and not serial.startswith("emulator-"):
        fail(refusal)
    if emulator:
        sdk = Path(os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT") or Path.home() / "Android/Sdk")
        if not emulator_tool(sdk):
            fail("Android emulator tool not found; install Android SDK emulator and add it to PATH")
    if not shutil.which("adb"):
        fail("adb not found; install Android SDK platform-tools and add them to PATH")
    try:
        if output(["adb", "-s", serial, "get-state"]) != "device":
            fail(refusal if emulator else "selected DEVICE is not ready")
        if emulator and (output(["adb", "-s", serial, "shell", "getprop", "ro.kernel.qemu"]) != "1" or output(["adb", "-s", serial, "shell", "getprop", "sys.boot_completed"]) != "1"):
            fail(refusal)
    except subprocess.CalledProcessError:
        fail(refusal if emulator else "selected DEVICE is not ready")


def build():
    bindings()
    rust()
    gradle(":app:assembleDebug")


def main():
    stage = sys.argv[1]
    if stage == "bindings":
        bindings()
    elif stage == "rust":
        rust()
    elif stage == "assemble":
        gradle(":app:assembleDebug")
    elif stage == "test":
        host_tools()
        sdk_tools()
        lock(ROOT / "rust/Cargo.toml")
        run(["cargo", "test", "--locked", "--manifest-path", "rust/Cargo.toml"])
        gradle(":app:testDebugUnitTest")
    elif stage == "e2e":
        serial = os.environ.get("E2E_DEVICE") or "emulator-5580"
        device_ready(serial, emulator=True)  # Before any build or Gradle invocation.
        os.environ["ANDROID_SERIAL"] = serial
        build()
        gradle(":app:connectedDebugAndroidTest")
    elif stage == "deploy":
        serial = os.environ.get("DEVICE")
        if not serial:
            fail("DEVICE is required; use make deploy DEVICE=<adb-serial>")
        device_ready(serial)
        build()
        run(["adb", "-s", serial, "install", "-r", "app/build/outputs/apk/debug/app-debug.apk"])
        run(["adb", "-s", serial, "shell", "am", "start", "-W", "-n", PACKAGE + "/.MainActivity"])
        info = output(["adb", "-s", serial, "shell", "dumpsys", "package", PACKAGE])
        if "versionName=0.1.0" not in info or "versionCode=1 " not in info:
            fail("installed package/version verification failed")
        print(PACKAGE + " versionName=0.1.0 versionCode=1 verified")
    else:
        fail("unknown build stage")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode)
