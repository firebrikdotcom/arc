//! Android scaffold ownership and non-mutating toolchain diagnosis.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MANIFEST_PATH: &str = ".arc/android.json";
const CHECKOUT_ERROR: &str =
    "ARC_PATH must identify a matching Arc checkout containing arc-core, arc-stack, and arc-ffi";

#[derive(Debug, Serialize, Deserialize)]
pub struct AndroidManifest {
    pub format_version: u32,
    pub target: String,
    pub name: String,
    pub crate_name: String,
    pub application_id: String,
    pub resources: Vec<serde_json::Value>,
}

pub fn read_manifest(root: &Path) -> Result<Option<AndroidManifest>, String> {
    let path = root.join(MANIFEST_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let manifest: AndroidManifest = serde_json::from_str(&contents)
        .map_err(|error| format!("invalid {}: {error}", path.display()))?;
    let (crate_name, application_id) =
        identifiers(&manifest.name).map_err(|_| format!("invalid {}", path.display()))?;
    if manifest.format_version != 1
        || manifest.target != "android"
        || manifest.crate_name != crate_name
        || manifest.application_id != application_id
    {
        return Err(format!(
            "invalid {}: unsupported format or inconsistent identifiers",
            path.display()
        ));
    }
    Ok(Some(manifest))
}

pub fn identifiers(name: &str) -> Result<(String, String), String> {
    crate::scaffold::validate_name(name)?;
    let stem = name.to_ascii_lowercase().replace('-', "_");
    Ok((
        format!("{stem}_core"),
        format!("com.example.arc.app_{stem}"),
    ))
}

fn checkout(
    explicit: Option<&Path>,
    environment: Option<PathBuf>,
) -> Result<(PathBuf, String), String> {
    let root = explicit.map(Path::to_path_buf).or(environment).ok_or("Android scaffolding requires a local Arc checkout: pass --arc-path PATH or set ARC_PATH (0.8.12 is not published)")?;
    let root = root.canonicalize().map_err(|_| CHECKOUT_ERROR)?;
    let read = |path: &Path| -> Result<toml::Value, String> {
        toml::from_str(&fs::read_to_string(path).map_err(|_| CHECKOUT_ERROR.to_string())?)
            .map_err(|_| CHECKOUT_ERROR.to_string())
    };
    let workspace = read(&root.join("Cargo.toml"))?;
    let version = workspace
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .ok_or(CHECKOUT_ERROR)?;
    for name in ["arc-core", "arc-stack", "arc-ffi"] {
        let manifest = read(&root.join(format!("crates/{name}/Cargo.toml")))?;
        let package = manifest.get("package").ok_or(CHECKOUT_ERROR)?;
        let v = package.get("version").ok_or(CHECKOUT_ERROR)?;
        if package.get("name").and_then(|n| n.as_str()) != Some(name)
            || !(v.as_str() == Some(version)
                || v.get("workspace").and_then(|v| v.as_bool()) == Some(true))
        {
            return Err(CHECKOUT_ERROR.into());
        }
    }
    Ok((root, version.into()))
}

// Replace template tokens once so literal token-like text in a checkout path is preserved.
fn render(template: &str, replacements: &BTreeMap<&str, String>) -> String {
    let mut result = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find("}}") else {
            break;
        };
        let end = end + 2;
        let token = &remaining[..end];
        result.push_str(replacements.get(token).map(String::as_str).unwrap_or(token));
        remaining = &remaining[end..];
    }
    result.push_str(remaining);
    result
}

pub fn create_project(
    name: &str,
    destination: &Path,
    arc_path: Option<&Path>,
    git: bool,
) -> Result<PathBuf, String> {
    let (crate_name, application_id) = identifiers(name)?;
    let (checkout, version) = checkout(arc_path, std::env::var_os("ARC_PATH").map(PathBuf::from))?;
    let root = destination.join(name);
    if root.exists() {
        return Err(format!(
            "destination `{}` already exists; Arc will not overwrite it",
            root.display()
        ));
    }
    let manifest = AndroidManifest {
        format_version: 1,
        target: "android".into(),
        name: name.into(),
        crate_name: crate_name.clone(),
        application_id: application_id.clone(),
        resources: vec![],
    };
    let mut replacements = BTreeMap::from([
        ("{{project-name}}", name.to_owned()),
        ("{{crate-name}}", crate_name),
        ("{{application-id}}", application_id.clone()),
        ("{{package-path}}", application_id.replace('.', "/")),
    ]);
    for (key, krate) in [
        ("{{arc-core-path}}", "arc-core"),
        ("{{arc-stack-path}}", "arc-stack"),
        ("{{arc-ffi-path}}", "arc-ffi"),
    ] {
        let path = checkout.join("crates").join(krate);
        let path = path.to_str().ok_or(CHECKOUT_ERROR)?;
        replacements.insert(key, toml::Value::String(path.into()).to_string());
    }
    replacements.insert("{{arc-version}}", version);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let result = (|| {
        for (path, bytes) in FILES {
            let render = |text: &str| render(text, &replacements);
            let path = root.join(render(path));
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let contents = if path.extension().is_some_and(|e| e == "jar") {
                bytes.to_vec()
            } else {
                render(std::str::from_utf8(bytes).map_err(|e| e.to_string())?).into_bytes()
            };
            fs::write(&path, contents)
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        }
        fs::create_dir_all(root.join(".arc")).map_err(|e| e.to_string())?;
        fs::write(
            root.join(MANIFEST_PATH),
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("gradlew"), fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    if git {
        crate::scaffold::initialize_git(&root)?;
    }
    Ok(root)
}

trait Probe {
    fn env(&self, key: &str) -> Option<String>;
    fn file(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> String;
    fn command(&self, program: &str, args: &[&str]) -> Option<String>;
}
struct SystemProbe;
impl Probe for SystemProbe {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
    fn file(&self, path: &Path) -> bool {
        path.exists()
    }
    fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }
    fn command(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status.success().then(|| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

fn diagnostics(probe: &impl Probe) -> Vec<String> {
    let mut result = vec![];
    if ["cargo", "rustc", "rustup"]
        .iter()
        .any(|tool| probe.command(tool, &["--version"]).is_none())
    {
        result.push(
            "ERROR: cargo/rustc/rustup not found; install the Rust toolchain with rustup".into(),
        );
    }
    if probe.command("cargo", &["ndk", "--version"]).is_none() {
        result.push("ERROR: cargo-ndk not found; run cargo install cargo-ndk --locked".into());
    }
    let sdk = probe
        .env("ANDROID_HOME")
        .or_else(|| probe.env("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(probe.env("HOME").unwrap_or_default()).join("Android/Sdk")
        });
    if !probe.file(&sdk) {
        result.push("ERROR: Android SDK not found; set ANDROID_HOME to your SDK directory".into());
    }
    if !probe.file(&sdk.join("platforms/android-36/android.jar")) {
        result.push(
            "ERROR: Android SDK platform 36 missing; run sdkmanager \"platforms;android-36\""
                .into(),
        );
    }
    if !probe.file(&sdk.join("build-tools/35.0.0/aapt2")) {
        result.push(
            "ERROR: Android build-tools 35.0.0 missing; run sdkmanager \"build-tools;35.0.0\""
                .into(),
        );
    }
    let ndk = probe
        .env("ANDROID_NDK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| sdk.join("ndk/27.0.12077973"));
    if !probe
        .read(&ndk.join("source.properties"))
        .lines()
        .any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                key.trim() == "Pkg.Revision" && value.trim() == "27.0.12077973"
            })
        })
        || !probe.file(&ndk.join("build/cmake/android.toolchain.cmake"))
    {
        result.push("ERROR: Android NDK 27.0.12077973 missing or mismatched; run sdkmanager \"ndk;27.0.12077973\" and set ANDROID_NDK_HOME".into());
    }
    let installed = probe
        .command("rustup", &["target", "list", "--installed"])
        .unwrap_or_default();
    let missing: Vec<_> = ["aarch64-linux-android", "x86_64-linux-android"]
        .into_iter()
        .filter(|target| !installed.lines().any(|line| line.trim() == *target))
        .collect();
    if !missing.is_empty() {
        result.push(format!("ERROR: Rust Android targets missing: {}; run rustup target add aarch64-linux-android x86_64-linux-android", missing.join(", ")));
    }
    let java = probe
        .env("JAVA_HOME")
        .map(|home| {
            PathBuf::from(home)
                .join("bin/java")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "java".into());
    let javac = probe
        .env("JAVA_HOME")
        .map(|home| {
            PathBuf::from(home)
                .join("bin/javac")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "javac".into());
    if !probe
        .command(&java, &["-version"])
        .as_deref()
        .and_then(java_major)
        .is_some_and(|major| major >= 17)
        || !probe
            .command(&javac, &["-version"])
            .as_deref()
            .and_then(java_major)
            .is_some_and(|major| major >= 17)
    {
        result.push(
            "ERROR: JDK 17 or newer not found; set JAVA_HOME to a JDK 17+ installation".into(),
        );
    }
    if probe.command("adb", &["version"]).is_none() {
        result.push(
            "ERROR: adb not found; install Android SDK platform-tools and add them to PATH".into(),
        );
    }
    let emulator = sdk.join("emulator/emulator");
    let emulator_version = if probe.file(&emulator) {
        emulator
            .to_str()
            .and_then(|path| probe.command(path, &["-version"]))
    } else {
        None
    }
    .or_else(|| probe.command("emulator", &["-version"]));
    if emulator_version.is_none() {
        result.push("ERROR: Android emulator tool not found; install Android SDK emulator and add it to PATH".into());
    }
    let device = probe
        .env("E2E_DEVICE")
        .unwrap_or_else(|| "emulator-5580".into());
    if !device.starts_with("emulator-")
        || probe
            .command("adb", &["-s", &device, "get-state"])
            .as_deref()
            .map(str::trim)
            != Some("device")
        || probe
            .command(
                "adb",
                &["-s", &device, "shell", "getprop", "ro.kernel.qemu"],
            )
            .as_deref()
            .map(str::trim)
            != Some("1")
    {
        result.push(format!(
            "WARN: {device} is not ready; start an AVD before make e2e-emulator or set E2E_DEVICE"
        ));
    }
    result
}

fn java_major(output: &str) -> Option<u32> {
    output.split_whitespace().find_map(|part| {
        let token = part.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.');
        let mut components = token.split('.');
        let first = components.next()?.parse::<u32>().ok()?;
        if first == 1 {
            components.next()?.parse().ok()
        } else {
            Some(first)
        }
    })
}

pub fn doctor() -> Result<(), String> {
    let messages = diagnostics(&SystemProbe);
    for message in &messages {
        println!("{message}");
    }
    if messages.iter().any(|m| m.starts_with("ERROR:")) {
        Err("Android prerequisites are missing; see diagnostics above".into())
    } else {
        println!("Android prerequisites ready.");
        Ok(())
    }
}

// Template inventory follows. All files are embedded, including the binary wrapper jar.
const FILES: &[(&str, &[u8])] = &[
    (
        ".gitignore",
        include_bytes!("../templates/android/.gitignore"),
    ),
    ("Makefile", include_bytes!("../templates/android/Makefile")),
    (
        "README.md",
        include_bytes!("../templates/android/README.md"),
    ),
    (
        "app/build.gradle.kts",
        include_bytes!("../templates/android/app/build.gradle.kts"),
    ),
    (
        "app/src/androidTest/java/{{package-path}}/ChooserTest.kt",
        include_bytes!(
            "../templates/android/app/src/androidTest/java/{{package-path}}/ChooserTest.kt"
        ),
    ),
    (
        "app/src/main/AndroidManifest.xml",
        include_bytes!("../templates/android/app/src/main/AndroidManifest.xml"),
    ),
    (
        "app/src/main/java/{{package-path}}/MainActivity.kt",
        include_bytes!("../templates/android/app/src/main/java/{{package-path}}/MainActivity.kt"),
    ),
    (
        "app/src/main/java/{{package-path}}/ResourceCatalog.kt",
        include_bytes!(
            "../templates/android/app/src/main/java/{{package-path}}/ResourceCatalog.kt"
        ),
    ),
    (
        "app/src/test/java/{{package-path}}/ResourceCatalogTest.kt",
        include_bytes!(
            "../templates/android/app/src/test/java/{{package-path}}/ResourceCatalogTest.kt"
        ),
    ),
    (
        "build.gradle.kts",
        include_bytes!("../templates/android/build.gradle.kts"),
    ),
    (
        "gradle/wrapper/gradle-wrapper.jar",
        include_bytes!("../templates/android/gradle/wrapper/gradle-wrapper.jar"),
    ),
    (
        "gradle/wrapper/gradle-wrapper.properties",
        include_bytes!("../templates/android/gradle/wrapper/gradle-wrapper.properties"),
    ),
    (
        "gradle.properties",
        include_bytes!("../templates/android/gradle.properties"),
    ),
    ("gradlew", include_bytes!("../templates/android/gradlew")),
    (
        "gradlew.bat",
        include_bytes!("../templates/android/gradlew.bat"),
    ),
    (
        "rust/Cargo.toml",
        include_bytes!("../templates/android/rust/Cargo.toml"),
    ),
    (
        "rust/migrations/00000000000000_arc_base/down.sql",
        include_bytes!("../templates/android/rust/migrations/00000000000000_arc_base/down.sql"),
    ),
    (
        "rust/migrations/00000000000000_arc_base/up.sql",
        include_bytes!("../templates/android/rust/migrations/00000000000000_arc_base/up.sql"),
    ),
    (
        "rust/src/domain.rs",
        include_bytes!("../templates/android/rust/src/domain.rs"),
    ),
    (
        "rust/src/ffi.rs",
        include_bytes!("../templates/android/rust/src/ffi.rs"),
    ),
    (
        "rust/src/lib.rs",
        include_bytes!("../templates/android/rust/src/lib.rs"),
    ),
    (
        "rust/src/schema.rs",
        include_bytes!("../templates/android/rust/src/schema.rs"),
    ),
    (
        "rust/tests/empty.rs",
        include_bytes!("../templates/android/rust/tests/empty.rs"),
    ),
    (
        "settings.gradle.kts",
        include_bytes!("../templates/android/settings.gradle.kts"),
    ),
    (
        "tools/android.py",
        include_bytes!("../templates/android/tools/android.py"),
    ),
    (
        "tools/uniffi-bindgen/Cargo.toml",
        include_bytes!("../templates/android/tools/uniffi-bindgen/Cargo.toml"),
    ),
    (
        "tools/uniffi-bindgen/src/main.rs",
        include_bytes!("../templates/android/tools/uniffi-bindgen/src/main.rs"),
    ),
    (
        "uniffi-bindgen.toml",
        include_bytes!("../templates/android/uniffi-bindgen.toml"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "arc-android-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_checkout(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.8.11\"\n",
        )
        .unwrap();
        for krate in ["arc-core", "arc-stack", "arc-ffi"] {
            let dir = root.join("crates").join(krate);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("Cargo.toml"),
                format!("[package]\nname = \"{krate}\"\nversion.workspace = true\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn template_values_are_never_recursively_interpolated() {
        let replacements = BTreeMap::from([
            ("{{path}}", "/checkout/{{name}}".into()),
            ("{{name}}", "app".into()),
        ]);
        assert_eq!(
            render("{{path}}/{{name}}", &replacements),
            "/checkout/{{name}}/app"
        );
    }

    #[test]
    fn identifiers_handle_case_hyphens_and_keywords() {
        for (name, stem) in [
            ("My-Notes", "my_notes"),
            ("class", "class"),
            ("type", "type"),
            ("SELF", "self"),
        ] {
            assert_eq!(
                identifiers(name).unwrap(),
                (
                    format!("{stem}_core"),
                    format!("com.example.arc.app_{stem}")
                )
            );
        }
        for name in ["", "../bad", "9name", "bad_name", "a/b"] {
            assert!(identifiers(name).is_err());
        }
    }

    #[test]
    fn checkout_precedence_validation_and_no_implicit_fallback() {
        let temp = Temp::new();
        fake_checkout(&temp.0);
        assert!(checkout(None, None)
            .unwrap_err()
            .contains("pass --arc-path"));
        assert_eq!(
            checkout(Some(&temp.0), Some("/missing".into())).unwrap().0,
            temp.0.canonicalize().unwrap()
        );
        assert!(checkout(Some(Path::new("/missing")), Some(temp.0.clone())).is_err());
        assert!(checkout(None, Some(temp.0.clone())).is_ok());
        fs::write(
            temp.0.join("crates/arc-ffi/Cargo.toml"),
            "[package]\nname = \"arc-ffi\"\nversion = \"9.0.0\"",
        )
        .unwrap();
        assert_eq!(
            checkout(None, Some(temp.0.clone())).unwrap_err(),
            CHECKOUT_ERROR
        );
    }

    #[test]
    fn renders_complete_empty_project_with_escaped_paths_and_unique_markers() {
        let temp = Temp::new();
        let checkout = temp.0.join("Arc space \"quoted\" \\path {{project-name}}");
        fake_checkout(&checkout);
        let root = create_project("My-Notes", &temp.0, Some(&checkout), false).unwrap();
        let manifest = read_manifest(&root).unwrap().unwrap();
        assert_eq!(manifest.crate_name, "my_notes_core");
        assert!(manifest.resources.is_empty());
        let cargo: toml::Value =
            toml::from_str(&fs::read_to_string(root.join("rust/Cargo.toml")).unwrap()).unwrap();
        for name in ["arc-core", "arc-stack", "arc-ffi"] {
            assert_eq!(
                cargo["dependencies"][name]["path"].as_str().unwrap(),
                checkout.join("crates").join(name).to_str().unwrap()
            );
            assert_eq!(
                cargo["dependencies"][name]["version"].as_str(),
                Some("0.8.11")
            );
        }
        let kotlin = root.join("app/src/main/java/com/example/arc/app_my_notes/ResourceCatalog.kt");
        assert!(fs::read_to_string(&kotlin)
            .unwrap()
            .contains("No resources yet"));
        for (path, markers) in [
            ("rust/src/domain.rs", vec!["// arc:domain-modules"]),
            (
                "rust/src/ffi.rs",
                vec![
                    "// arc:ffi-imports",
                    "// arc:ffi-fields",
                    "// arc:ffi-open-initializers",
                    "// arc:ffi-open-fields",
                    "// arc:ffi-methods",
                ],
            ),
            ("rust/src/schema.rs", vec!["// arc:schema-migrations"]),
            (
                "app/src/main/java/com/example/arc/app_my_notes/ResourceCatalog.kt",
                vec!["// arc:catalog-imports", "// arc:catalog-entries"],
            ),
        ] {
            let source = fs::read_to_string(root.join(path)).unwrap();
            for marker in markers {
                assert_eq!(source.matches(marker).count(), 1, "{path}: {marker}");
            }
        }
        assert!(
            root.join("gradle/wrapper/gradle-wrapper.jar")
                .metadata()
                .unwrap()
                .len()
                > 10000
        );
        assert!(!root.join("rust/Cargo.lock").exists());
        let android = fs::read_to_string(root.join("app/src/main/AndroidManifest.xml")).unwrap();
        assert!(!android.contains("permission"));
        assert!(!cargo["dependencies"]
            .as_table()
            .unwrap()
            .contains_key("arc-web"));
        assert!(create_project("My-Notes", &temp.0, Some(&checkout), false)
            .unwrap_err()
            .contains("will not overwrite"));
        fs::write(root.join(MANIFEST_PATH), "{}").unwrap();
        assert!(read_manifest(&root).is_err());
    }

    #[test]
    fn invalid_checkout_never_creates_destination() {
        let temp = Temp::new();
        assert!(create_project("Untouched", &temp.0, Some(&temp.0), false).is_err());
        assert!(!temp.0.join("Untouched").exists());
    }

    #[derive(Default)]
    struct Fixture {
        env: BTreeMap<String, String>,
        files: BTreeMap<PathBuf, String>,
        commands: BTreeMap<String, String>,
    }
    impl Probe for Fixture {
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }
        fn file(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn read(&self, path: &Path) -> String {
            self.files.get(path).cloned().unwrap_or_default()
        }
        fn command(&self, program: &str, args: &[&str]) -> Option<String> {
            self.commands
                .get(&format!("{program} {}", args.join(" ")))
                .cloned()
        }
    }
    fn ready() -> Fixture {
        let mut f = Fixture::default();
        f.env.insert("ANDROID_HOME".into(), "/sdk".into());
        for path in [
            "/sdk",
            "/sdk/platforms/android-36/android.jar",
            "/sdk/build-tools/35.0.0/aapt2",
            "/sdk/ndk/27.0.12077973/build/cmake/android.toolchain.cmake",
        ] {
            f.files.insert(path.into(), String::new());
        }
        f.files.insert(
            "/sdk/ndk/27.0.12077973/source.properties".into(),
            "Pkg.Revision = 27.0.12077973\n".into(),
        );
        f.commands
            .insert("cargo ndk --version".into(), "cargo-ndk 4.1.2".into());
        for tool in ["cargo", "rustc", "rustup"] {
            f.commands
                .insert(format!("{tool} --version"), "installed".into());
        }
        for tool in ["java", "javac"] {
            f.commands
                .insert(format!("{tool} -version"), "version \"17.0.18\"".into());
        }
        f.commands.insert(
            "rustup target list --installed".into(),
            "aarch64-linux-android\nx86_64-linux-android\n".into(),
        );
        f.commands.insert(
            "adb version".into(),
            "Android Debug Bridge version 1.0.41".into(),
        );
        f.commands.insert(
            "emulator -version".into(),
            "Android emulator version".into(),
        );
        f.commands
            .insert("adb -s emulator-5580 get-state".into(), "device\n".into());
        f.commands.insert(
            "adb -s emulator-5580 shell getprop ro.kernel.qemu".into(),
            "1\n".into(),
        );
        f
    }
    #[test]
    fn doctor_reports_all_missing_tools_without_real_commands() {
        let messages = diagnostics(&Fixture::default());
        assert_eq!(
            messages.iter().filter(|m| m.starts_with("ERROR:")).count(),
            10
        );
        for expected in [
            "cargo/rustc/rustup",
            "cargo-ndk",
            "Android SDK not found",
            "platform 36",
            "build-tools 35.0.0",
            "NDK 27.0.12077973",
            "aarch64-linux-android, x86_64-linux-android",
            "JDK 17 or newer",
            "adb not found",
            "emulator tool",
        ] {
            assert!(
                messages.iter().any(|m| m.contains(expected)),
                "{expected}: {messages:?}"
            );
        }
    }

    #[test]
    fn java_major_parses_legacy_and_modern_version_output() {
        for (output, expected) in [
            ("java version \"1.8.0_402\"", 8),
            ("javac 11.0.22", 11),
            ("openjdk version \"17.0.18\"", 17),
            ("openjdk version \"21.0.11\" 2026-04-21", 21),
        ] {
            assert_eq!(java_major(output), Some(expected), "{output}");
        }
        assert_eq!(java_major("not a Java version"), None);
    }

    #[test]
    fn doctor_accepts_jdk_21_and_rejects_jdk_11() {
        let mut f = ready();
        for tool in ["java", "javac"] {
            f.commands.insert(
                format!("{tool} -version"),
                "openjdk version \"21.0.11\" 2026-04-21".into(),
            );
        }
        assert!(diagnostics(&f).is_empty());
        for tool in ["java", "javac"] {
            f.commands
                .insert(format!("{tool} -version"), "javac 11.0.22".into());
        }
        assert!(diagnostics(&f).iter().any(|message| {
            message == "ERROR: JDK 17 or newer not found; set JAVA_HOME to a JDK 17+ installation"
        }));
    }
    #[test]
    fn doctor_success_emulator_warning_and_java_ndk_versions() {
        let mut f = ready();
        assert!(diagnostics(&f).is_empty());
        f.commands.remove("adb -s emulator-5580 get-state");
        assert_eq!(diagnostics(&f).len(), 1);
        assert!(diagnostics(&f)[0].starts_with("WARN:"));
        f.commands
            .insert("java -version".into(), "version \"21.0.1\"".into());
        f.files.insert(
            "/sdk/ndk/27.0.12077973/source.properties".into(),
            "Pkg.Revision = 28.0.0".into(),
        );
        assert_eq!(
            diagnostics(&f)
                .iter()
                .filter(|m| m.starts_with("ERROR:"))
                .count(),
            1
        );
    }
    #[test]
    fn doctor_honors_sdk_ndk_java_and_device_overrides() {
        let mut f = ready();
        f.env.remove("ANDROID_HOME");
        f.env.insert("ANDROID_SDK_ROOT".into(), "/sdk".into());
        f.env
            .insert("ANDROID_NDK_HOME".into(), "/custom-ndk".into());
        f.files.insert(
            "/custom-ndk/source.properties".into(),
            "Pkg.Revision = 27.0.12077973".into(),
        );
        f.files.insert(
            "/custom-ndk/build/cmake/android.toolchain.cmake".into(),
            String::new(),
        );
        f.env.insert("JAVA_HOME".into(), "/jdk".into());
        f.commands
            .insert("/jdk/bin/java -version".into(), "version \"17.0.1\"".into());
        f.commands
            .insert("/jdk/bin/javac -version".into(), "javac 17.0.1".into());
        f.env.insert("E2E_DEVICE".into(), "emulator-5590".into());
        assert_eq!(diagnostics(&f), vec!["WARN: emulator-5590 is not ready; start an AVD before make e2e-emulator or set E2E_DEVICE"]);
    }

    #[test]
    fn doctor_finds_emulator_in_sdk_before_path_fallback() {
        let mut f = ready();
        f.commands.remove("emulator -version");
        f.files
            .insert("/sdk/emulator/emulator".into(), String::new());
        f.commands.insert(
            "/sdk/emulator/emulator -version".into(),
            "Android emulator version".into(),
        );
        assert!(diagnostics(&f).is_empty());

        f.commands.remove("/sdk/emulator/emulator -version");
        assert!(diagnostics(&f)
            .iter()
            .any(|message| message.contains("emulator tool")));
        f.commands.insert(
            "emulator -version".into(),
            "Android emulator version".into(),
        );
        assert!(diagnostics(&f).is_empty());
    }
}
