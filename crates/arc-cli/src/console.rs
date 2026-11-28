//! `arc console`: an [irust](https://github.com/sigmaSd/IRust) REPL prepared
//! for the Arc application in the current directory.
//!
//! Generated applications are binary-only crates, so a REPL cannot depend on
//! them directly. Instead the console generates, under the application's
//! `target/arc-console/`, a small Cargo workspace that irust is pointed at:
//!
//! * `irust/irust_prelude/` is the crate irust imports into every session
//!   (`IRUST_DATA_DIR` makes irust use it). It depends on the application's
//!   own `[dependencies]` at the application's versions, re-exports them,
//!   adds a few helpers (`load_env`, `block_on`, `rows`, `users`, ...), and
//!   compiles the application's top-level modules from `src/` in place.
//! * `Cargo.toml` makes that crate and irust's temporary host crates
//!   (`IRUST_TEMP_DIR`) one workspace, so the copied `Cargo.lock` pins every
//!   dependency to the version the application builds with, and the shared
//!   target directory reuses the application's compiled dependencies.
//!
//! Nothing outside `target/` is written: the application's sources, `.env`
//! and database are only read.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toml::{Table, Value};

/// Directory, relative to the application root, that holds generated files.
pub(crate) const CONSOLE_DIR: &str = "target/arc-console";
const PRELUDE_DIR: &str = "irust/irust_prelude";
const HOST_PREFIX: &str = "irust_host_repl_";
/// Cargo reads a workspace member glob that matches nothing as a literal
/// path, so one placeholder member always matches the host crate glob.
const PLACEHOLDER: &str = "irust_repls/irust_host_repl_arc_console_placeholder";

pub(crate) struct ConsoleOptions {
    /// Compile the application's own `src/` modules into the session.
    pub app_modules: bool,
    /// Extra arguments passed to irust after the generated startup file.
    pub irust_args: Vec<String>,
}

/// What the console needs to know about the application.
#[derive(Debug)]
pub(crate) struct App {
    pub root: PathBuf,
    pub name: String,
    pub edition: String,
    /// The application's `[dependencies]`, with path dependencies made absolute.
    pub dependencies: Table,
    pub crates: HelperCrates,
    /// `[patch]` and `[profile]` tables to repeat at the console workspace root.
    pub patch: Option<Value>,
    pub profile: Option<Value>,
    /// Top-level modules declared in `src/main.rs`.
    pub modules: Vec<String>,
    /// Aggregates (and their projectors) registered on the builder in
    /// `src/main.rs`, for the write helpers.
    pub registrations: Vec<Registration>,
    /// Top-level `use` declarations of `src/main.rs`, so registered type and
    /// projector expressions resolve in the generated registry.
    pub uses: Vec<String>,
}

/// One `.register_aggregate::<A>()` call and the projectors chained after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Registration {
    pub aggregate: String,
    /// Each entry is a Rust expression of type `Vec<ProjectorReg>`-compatible
    /// item: `Projector(..)` for `register_projector`, `Many(..)` for
    /// `register_projectors`.
    pub projectors: Vec<ProjectorExpr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectorExpr {
    /// `register_projector(projector, view)`.
    One { projector: String, view: String },
    /// `register_projectors(list)`.
    Many(String),
}

/// Helper units of the generated prelude. Each one may be dropped if it does
/// not compile against the application's dependency versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Helper {
    Stores,
    Identity,
    /// The replay-guarded `dispatch` write helpers.
    Writes,
    /// The aggregate/projector registry read from `src/main.rs`. When it is
    /// not selected an empty registry is generated in its place, so the write
    /// helpers keep compiling.
    Registry,
}

impl Helper {
    const ALL: [Helper; 4] = [
        Helper::Stores,
        Helper::Identity,
        Helper::Writes,
        Helper::Registry,
    ];

    fn module(self) -> &'static str {
        match self {
            Helper::Stores => "arc_console_stores",
            Helper::Identity => "arc_console_identity",
            Helper::Writes => "arc_console_writes",
            Helper::Registry => "arc_console_registry",
        }
    }
}

/// The modules compiled into one attempt at building the prelude.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    helpers: Vec<Helper>,
    modules: Vec<String>,
}

pub(crate) fn run(root: &Path, options: &ConsoleOptions) -> Result<(), String> {
    let app = read_app(root)?;
    // Find irust before generating or building anything.
    let irust = find_program("irust", std::env::var_os("PATH")).ok_or(
        "irust was not found on PATH; install it with `cargo install irust`, then run `arc console` again",
    )?;
    let target_dir = target_dir(&app.root);
    let console_dir = app.root.join(CONSOLE_DIR);
    let mut selection = Selection {
        helpers: helpers_for(&app),
        modules: if options.app_modules {
            app.modules.clone()
        } else {
            Vec::new()
        },
    };
    if selection.modules.is_empty() {
        // The registry names application types.
        selection
            .helpers
            .retain(|helper| *helper != Helper::Registry);
    }
    write_workspace(&app, &console_dir)?;
    eprintln!(
        "Preparing the console crate for {} in {}",
        app.name,
        console_dir.display()
    );
    loop {
        write_prelude(&app, &console_dir, &selection)?;
        match build_prelude(&console_dir, &target_dir)? {
            BuildOutcome::Success => break,
            BuildOutcome::Failed(errors) => {
                let dropped = attribute_errors(&app, &console_dir, &selection, &errors);
                if dropped.helpers.is_empty() && dropped.modules.is_empty() {
                    for error in &errors {
                        eprint!("{}", error.rendered);
                    }
                    return Err(
                        "the console crate did not build; see the compiler output above".into(),
                    );
                }
                for module in &dropped.modules {
                    eprintln!(
                        "warning: skipping application module `{module}`: it does not compile outside the application binary ({})",
                        first_message(&errors, &app.root.join("src").join(module))
                    );
                }
                for helper in &dropped.helpers {
                    eprintln!(
                        "warning: skipping console helpers `{}`: they do not compile against this application's Arc version",
                        helper.module()
                    );
                }
                selection.modules.retain(|m| !dropped.modules.contains(m));
                selection.helpers.retain(|h| !dropped.helpers.contains(h));
            }
        }
    }
    print_banner(&app, &selection);
    let startup = console_dir.join("startup.rs");
    let session = session_id();
    let status = Command::new(&irust)
        .arg(&startup)
        .args(&options.irust_args)
        .current_dir(&app.root)
        .envs(irust_env(&console_dir, &target_dir, &session))
        .status()
        .map_err(|error| format!("could not start {}: {error}", irust.display()));
    // The write journal only guards this session's replays.
    let _ = fs::remove_file(journal_path(&console_dir, &session));
    let _ = fs::remove_dir(console_dir.join("sessions"));
    let status = status?;
    remove_host_artifacts(&target_dir);
    if status.success() {
        Ok(())
    } else {
        Err(format!("irust exited with {status}"))
    }
}

/// The environment irust runs with: its prelude and temporary host crates
/// live in the console workspace, and builds share the application's target.
/// `ARC_CONSOLE_SESSION` and `ARC_CONSOLE_DIR` scope the write helpers' replay
/// journal to this launch.
pub(crate) fn irust_env(
    console_dir: &Path,
    target_dir: &Path,
    session: &str,
) -> Vec<(&'static str, OsString)> {
    vec![
        ("IRUST_DATA_DIR", console_dir.as_os_str().to_owned()),
        ("IRUST_TEMP_DIR", console_dir.as_os_str().to_owned()),
        ("CARGO_TARGET_DIR", target_dir.as_os_str().to_owned()),
        ("ARC_CONSOLE_SESSION", session.into()),
        ("ARC_CONSOLE_DIR", console_dir.as_os_str().to_owned()),
    ]
}

/// A fresh identifier for one `arc console` launch.
fn session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos}-{}", std::process::id())
}

/// The write journal of a session (see `console_guard.rs`).
pub(crate) fn journal_path(console_dir: &Path, session: &str) -> PathBuf {
    console_dir
        .join("sessions")
        .join(format!("{session}.jsonl"))
}

fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR").filter(|value| !value.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                root.join(dir)
            }
        }
        None => root.join("target"),
    }
}

pub(crate) fn find_program(name: &str, path: Option<OsString>) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&path?)
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Reads and validates the application at `root`.
pub(crate) fn read_app(root: &Path) -> Result<App, String> {
    let not_an_app = || {
        format!(
            "no Arc application found in {}; run `arc console` from the root of an application created by `arc new`",
            fs::canonicalize(root)
                .unwrap_or_else(|_| root.to_path_buf())
                .display()
        )
    };
    let manifest_path = root.join("Cargo.toml");
    let main_path = root.join("src/main.rs");
    if !manifest_path.is_file() || !main_path.is_file() {
        return Err(not_an_app());
    }
    let manifest: Table = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?
        .parse()
        .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
    let package = manifest
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(not_an_app)?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    if !dependencies.contains_key("arc-web") {
        return Err(not_an_app());
    }
    let root = fs::canonicalize(root)
        .map_err(|error| format!("could not resolve {}: {error}", root.display()))?;
    let main = fs::read_to_string(&main_path)
        .map_err(|error| format!("could not read {}: {error}", main_path.display()))?;
    let (dependencies, crates) = console_dependencies(&dependencies, &root);
    Ok(App {
        name: package
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("application")
            .to_string(),
        edition: package
            .get("edition")
            .and_then(Value::as_str)
            .unwrap_or("2021")
            .to_string(),
        dependencies,
        crates,
        patch: manifest.get("patch").map(|patch| absolutize(patch, &root)),
        profile: manifest.get("profile").cloned(),
        modules: top_level_modules(&main)
            .into_iter()
            .filter(|module| module_file(&root, module).is_some())
            .collect(),
        registrations: registrations(&main),
        uses: top_level_uses(&main),
        root,
    })
}

fn module_file(root: &Path, module: &str) -> Option<PathBuf> {
    [format!("src/{module}.rs"), format!("src/{module}/mod.rs")]
        .into_iter()
        .map(|path| root.join(path))
        .find(|path| path.is_file())
}

/// Top-level `mod name;` declarations in `main.rs`, skipping test-only and
/// inline modules.
pub(crate) fn top_level_modules(main: &str) -> Vec<String> {
    let mut modules = Vec::new();
    let mut test_only = false;
    let mut depth = 0usize;
    for line in main.lines() {
        let trimmed = line.trim();
        if depth == 0 {
            if trimmed.starts_with("#[cfg(test)]") {
                test_only = true;
                continue;
            }
            let declaration = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            if let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
                .map(str::trim)
            {
                if !test_only && is_identifier(name) && !modules.iter().any(|m| m == name) {
                    modules.push(name.to_string());
                }
            }
            if !trimmed.starts_with("#[") && !trimmed.is_empty() {
                test_only = false;
            }
        }
        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    modules
}

/// Top-level `use` declarations of `main.rs`, each as one string.
pub(crate) fn top_level_uses(main: &str) -> Vec<String> {
    let mut uses = Vec::new();
    let mut current: Option<String> = None;
    let mut depth = 0usize;
    for line in main.lines() {
        let trimmed = line.trim();
        if let Some(statement) = current.as_mut() {
            statement.push(' ');
            statement.push_str(trimmed);
        } else if depth == 0 && (trimmed.starts_with("use ") || trimmed.starts_with("pub use ")) {
            current = Some(trimmed.to_string());
        }
        if current
            .as_ref()
            .is_some_and(|statement| statement.ends_with(';'))
        {
            uses.extend(current.take());
        }
        if current.is_none() {
            for character in line.chars() {
                match character {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }
    uses
}

/// The `.register_aggregate::<A>()` calls in `main.rs`, each with the
/// `.register_projector(..)` / `.register_projectors(..)` calls that follow it
/// (the builder attaches projectors to the most recently registered
/// aggregate). Aggregates registered with a caller-built stack, or by plugins,
/// are not listed.
pub(crate) fn registrations(main: &str) -> Vec<Registration> {
    // Drop whole-line comments (the generators' `// arc:...` markers); string
    // literals such as `"postgres://..."` stay intact.
    let code: String = main
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    const AGGREGATE: &str = ".register_aggregate";
    let mut found: Vec<Registration> = Vec::new();
    let mut rest = code.as_str();
    while let Some(start) = rest.find(AGGREGATE) {
        let after = &rest[start + AGGREGATE.len()..];
        rest = after;
        let Some(generic) = after.trim_start().strip_prefix("::<") else {
            continue; // `register_aggregate_with_stack` or not a turbofish
        };
        let Some((aggregate, tail)) = split_closing(generic, '<', '>') else {
            continue;
        };
        let Some(tail) = tail.trim_start().strip_prefix("()") else {
            continue;
        };
        let aggregate = aggregate.split_whitespace().collect::<String>();
        let segment = tail.find(AGGREGATE).map_or(tail, |end| &tail[..end]);
        let mut projectors = Vec::new();
        let mut scan = segment;
        while let Some(call) = scan.find(".register_projector") {
            let after = &scan[call + ".register_projector".len()..];
            let (many, after) = match after.strip_prefix('s') {
                Some(after) => (true, after),
                None => (false, after),
            };
            scan = after;
            let Some(args) = after.trim_start().strip_prefix('(') else {
                continue;
            };
            let Some((args, tail)) = split_closing(args, '(', ')') else {
                break;
            };
            scan = tail;
            if many {
                projectors.push(ProjectorExpr::Many(args.trim().to_string()));
            } else if let Some((projector, view)) = split_top_level_comma(args) {
                projectors.push(ProjectorExpr::One {
                    projector: projector.trim().to_string(),
                    view: view.trim().trim_end_matches(',').trim().to_string(),
                });
            }
        }
        if !found.iter().any(|known| known.aggregate == aggregate) {
            found.push(Registration {
                aggregate,
                projectors,
            });
        }
    }
    found
}

/// Splits `text` at the delimiter closing an already-opened `open`, returning
/// the enclosed text and what follows the closing delimiter.
fn split_closing(text: &str, open: char, close: char) -> Option<(&str, &str)> {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[..index], &text[index + close.len_utf8()..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits call arguments at the first comma outside brackets and strings.
fn split_top_level_comma(args: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in args.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => return Some((&args[..index], &args[index + 1..])),
            _ => {}
        }
    }
    None
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Crate names the generated helpers use, as the prelude manifest declares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HelperCrates {
    pub tokio: String,
    pub dotenvy: String,
    pub serde_json: String,
    pub auth_core: Option<String>,
}

/// The application's dependencies as the prelude declares them, plus what
/// the console helpers need. Cargo refuses one package under two names, so a
/// helper dependency the application already has is reused (with any extra
/// features added) instead of being declared again.
fn console_dependencies(dependencies: &Table, root: &Path) -> (Table, HelperCrates) {
    let mut result = Table::new();
    for (name, spec) in dependencies {
        if let Some(table) = spec.as_table() {
            // Optional dependencies follow application features and workspace
            // inheritance needs a workspace the console does not have.
            if table.get("optional").and_then(Value::as_bool) == Some(true)
                || table.get("workspace").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
        }
        result.insert(name.clone(), absolutize(spec, root));
    }
    let tokio = ensure_dependency(&mut result, "tokio", "1".into(), &["rt-multi-thread"]);
    let dotenvy = ensure_dependency(&mut result, "dotenvy", "0.15".into(), &[]);
    let serde_json = ensure_dependency(&mut result, "serde_json", "1".into(), &[]);
    // The identity helpers name the `IdentityStore` trait, which lives in
    // arc-auth-core: depend on it exactly as the application depends on
    // arc-auth-db.
    let auth_core = result.get("arc-auth-db").cloned().map(|spec| {
        let core = match spec {
            Value::Table(mut table) => {
                for key in [
                    "features",
                    "default-features",
                    "default_features",
                    "package",
                ] {
                    table.remove(key);
                }
                if let Some(path) = table.get("path").and_then(Value::as_str) {
                    let sibling = Path::new(path).with_file_name("arc-auth-core");
                    table.insert("path".into(), sibling.display().to_string().into());
                }
                Value::Table(table)
            }
            other => other,
        };
        ensure_dependency(&mut result, "arc-auth-core", core, &[])
    });
    (
        result,
        HelperCrates {
            tokio,
            dotenvy,
            serde_json,
            auth_core,
        },
    )
}

/// Returns the crate name under which `package` is available, declaring it
/// with `spec` when the application does not depend on it.
fn ensure_dependency(deps: &mut Table, package: &str, spec: Value, features: &[&str]) -> String {
    let existing = deps
        .iter()
        .find(|(key, spec)| {
            spec.get("package")
                .and_then(Value::as_str)
                .unwrap_or(key.as_str())
                == package
        })
        .map(|(key, _)| key.clone());
    let key = existing.unwrap_or_else(|| {
        deps.insert(package.to_string(), spec);
        package.to_string()
    });
    if !features.is_empty() {
        let entry = deps
            .get_mut(&key)
            .expect("dependency was just found or added");
        if let Value::String(version) = entry {
            let mut table = Table::new();
            table.insert("version".into(), version.clone().into());
            *entry = Value::Table(table);
        }
        if let Value::Table(table) = entry {
            let list = table
                .entry("features")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(list) = list {
                for feature in features {
                    let feature = Value::from(*feature);
                    if !list.contains(&feature) {
                        list.push(feature);
                    }
                }
            }
        }
    }
    crate_name(&key)
}

/// Rewrites every relative `path = "..."` inside `value` against `root`.
fn absolutize(value: &Value, root: &Path) -> Value {
    match value {
        Value::Table(table) => Value::Table(
            table
                .iter()
                .map(|(key, value)| {
                    let value = match (key.as_str(), value) {
                        ("path", Value::String(path)) if Path::new(path).is_relative() => {
                            Value::String(root.join(path).display().to_string())
                        }
                        _ => absolutize(value, root),
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn helpers_for(app: &App) -> Vec<Helper> {
    let mut helpers = Vec::new();
    if app.dependencies.contains_key("arc-web") && app.dependencies.contains_key("arc-core") {
        helpers.push(Helper::Stores);
    }
    if app.dependencies.contains_key("arc-auth-db") && app.crates.auth_core.is_some() {
        helpers.push(Helper::Identity);
    }
    if app.dependencies.contains_key("arc-web") && app.dependencies.contains_key("arc-core") {
        helpers.push(Helper::Writes);
        if !app.registrations.is_empty() {
            helpers.push(Helper::Registry);
        }
    }
    helpers
}

/// The Rust name a dependency is imported under.
fn crate_name(key: &str) -> String {
    key.replace('-', "_")
}

pub(crate) fn workspace_manifest(app: &App) -> String {
    let resolver = if app.edition.as_str() >= "2024" {
        "3"
    } else {
        "2"
    };
    let mut workspace = Table::new();
    workspace.insert("resolver".into(), resolver.into());
    workspace.insert(
        "members".into(),
        Value::Array(vec![
            PRELUDE_DIR.into(),
            format!("irust_repls/{HOST_PREFIX}*").into(),
        ]),
    );
    let mut manifest = Table::new();
    manifest.insert("workspace".into(), Value::Table(workspace));
    if let Some(patch) = &app.patch {
        manifest.insert("patch".into(), patch.clone());
    }
    if let Some(profile) = &app.profile {
        manifest.insert("profile".into(), profile.clone());
    }
    format!(
        "# Generated by `arc console` for {}; regenerated on every launch.\n{}",
        app.name,
        toml::to_string(&manifest).expect("workspace manifest serializes")
    )
}

pub(crate) fn prelude_manifest(app: &App) -> String {
    let mut package = Table::new();
    package.insert("name".into(), "irust_prelude".into());
    package.insert("version".into(), "0.1.0".into());
    package.insert("edition".into(), app.edition.clone().into());
    package.insert("publish".into(), false.into());
    // The prelude links to the application's top-level entries (see
    // `link_app_entries`); never pick up its tests, examples or build script.
    for key in [
        "autobins",
        "autoexamples",
        "autotests",
        "autobenches",
        "build",
    ] {
        package.insert(key.into(), false.into());
    }
    let section = |name: &str, table: Table| {
        let mut wrapper = Table::new();
        wrapper.insert(name.into(), Value::Table(table));
        toml::to_string(&wrapper).expect("prelude manifest serializes")
    };
    format!(
        "# Generated by `arc console` for {}; regenerated on every launch.\n{}\n{}",
        app.name,
        section("package", package),
        section("dependencies", app.dependencies.clone())
    )
}

fn prelude_source(app: &App, selection: &Selection) -> String {
    let mut source = format!(
        "//! Generated by `arc console` for {}; regenerated on every launch.\n\
         //! irust imports this crate into every session with `use irust_prelude::*;`.\n\
         #![allow(warnings)]\n#![allow(clippy::all)]\n\n\
         // The application's dependencies, at the application's versions.\n",
        app.name
    );
    for key in app.dependencies.keys() {
        let name = crate_name(key);
        if selection.modules.contains(&name) {
            continue;
        }
        source.push_str(&format!("pub use ::{name};\n"));
    }
    source.push_str("\npub mod arc_console;\npub use arc_console::*;\n");
    for helper in &selection.helpers {
        let module = helper.module();
        if *helper == Helper::Registry {
            continue; // declared below, with the write helpers
        }
        source.push_str(&format!("pub mod {module};\npub use {module}::*;\n"));
    }
    if selection.helpers.contains(&Helper::Writes) {
        source.push_str("mod arc_console_guard;\nmod arc_console_registry;\n");
    }
    if !selection.modules.is_empty() {
        let src = app.root.join("src").display().to_string();
        source.push_str(&format!(
            "\n// The application's own modules, compiled in place from {src}.\n\
             #[path = {src:?}]\nmod arc_app {{\n"
        ));
        for module in &selection.modules {
            source.push_str(&format!("    pub mod {module};\n"));
        }
        source.push_str(&format!(
            "}}\npub use arc_app::{{{}}};\n",
            selection.modules.join(", ")
        ));
    }
    source
}

const CONSOLE_HELPERS: &str = r#"//! Helpers available in every `arc console` session.
use std::future::Future;
use std::sync::{Once, OnceLock};

/// Loads the application's `.env` from the current directory (the
/// application root) once per evaluation. Variables already set win, as in
/// the application's own `main`. The console's startup file calls it.
pub fn load_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ::__DOTENVY__::dotenv();
    });
}

/// Reads an environment variable after loading `.env`.
pub fn env(key: &str) -> Option<String> {
    load_env();
    std::env::var(key).ok()
}

/// `DATABASE_URL`, defaulting like generated applications do.
pub fn database_url() -> String {
    env("DATABASE_URL").unwrap_or_else(|| "database/database.sqlite".to_string())
}

/// A Tokio runtime shared by everything in one evaluation.
pub fn runtime() -> &'static ::__TOKIO__::runtime::Runtime {
    static RUNTIME: OnceLock<::__TOKIO__::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        ::__TOKIO__::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("arc console could not start a Tokio runtime")
    })
}

/// Runs a future to completion, so async Arc APIs can be used from the REPL:
/// `block_on(read_models().list("posts_view"))`.
pub fn block_on<F: Future>(future: F) -> F::Output {
    load_env();
    runtime().block_on(future)
}
"#;

const STORES_HELPERS: &str = r#"//! Handles to the application's event store and read models, opened from
//! `DATABASE_DRIVER` and `DATABASE_URL` the way the server opens them.
use super::arc_console::{block_on, database_url};
use std::sync::Arc;

pub use ::arc_core::event::Event;
pub use ::arc_core::event_store::EventStore;
pub use ::arc_core::read_model_store::{ReadModelStore, Row};
pub use ::arc_web::helpers::config::DatabaseDriver;
pub use ::arc_web::helpers::es_stack::{build_stores, EsStores};

/// Opens the application's stores.
pub fn stores() -> EsStores {
    block_on(build_stores(DatabaseDriver::from_env(), &database_url()))
        .unwrap_or_else(|error| panic!("could not open the application's stores: {error}"))
}

/// The projection read-model store.
pub fn read_models() -> Arc<dyn ReadModelStore> {
    stores().read_model_store
}

/// The event store (the source of truth).
pub fn event_store() -> Box<dyn EventStore> {
    stores().command_event_store
}

/// Every row of a read-model table, for example `rows("posts_view")`.
pub fn rows(table: &str) -> Vec<Row> {
    block_on(read_models().list(table))
        .unwrap_or_else(|error| panic!("could not list {table}: {error}"))
}

/// The events of one aggregate, oldest first.
pub fn events(aggregate_id: &str) -> Vec<Event> {
    block_on(event_store().load(aggregate_id))
        .unwrap_or_else(|error| panic!("could not load events for {aggregate_id}: {error}"))
}
"#;

const IDENTITY_HELPERS: &str = r#"//! The arc-auth-db identity store (`users`, `roles` and `user_roles`).
use super::arc_console::{block_on, database_url};

pub use ::arc_auth_db::DbIdentityStore;
pub use ::__AUTH_CORE__::{Identity, IdentityStore};

/// The identity store the application's `DbIdentityPlugin` uses.
pub fn identities() -> DbIdentityStore {
    DbIdentityStore::new(database_url())
}

/// Every user, with roles.
pub fn users() -> Vec<Identity> {
    block_on(identities().list()).unwrap_or_else(|error| panic!("could not list users: {error}"))
}
"#;

const WRITES_HELPERS: &str = r#"//! Writes through the application's `CommandBus`, the way the server makes
//! them: an in-process event bus drives the registered projectors, so read
//! models reflect each write at once. Every write is replay-guarded (see
//! `arc_console_guard`): irust reruns earlier statements on each evaluation,
//! and a statement that already wrote returns its first result instead of
//! writing again.
use super::arc_console::{block_on, database_url, env};
use super::arc_console_guard::{Journal, Outcome, Site};
use super::arc_console_registry::registered_projectors;

pub use ::arc_core::aggregate::{Aggregate, Command};
pub use ::arc_web::ProjectorReg;

/// Dispatches `command` to aggregate `A` with the projectors the application
/// registers for `A` in `src/main.rs`, for example
/// `dispatch::<domain::post::aggregate::PostAggregate>(PostCommand::Create { .. })`.
/// Returns the events the command produced.
#[track_caller]
pub fn dispatch<A: Aggregate + 'static>(
    command: A::Command,
) -> Result<Vec<::arc_core::event::Event>, String> {
    let site = Site::of(std::panic::Location::caller());
    match registered_projectors::<A>() {
        Some(projectors) => guarded::<A>(&site, command, projectors),
        None => Err(format!(
            "aggregate {} is not registered with `.register_aggregate::<..>()` in src/main.rs (or its registration does not compile in the console); use dispatch_with::<A>(command, projectors) to name its projectors",
            A::aggregate_type()
        )),
    }
}

/// Like [`dispatch`], with an explicit projector list, for aggregates the
/// console cannot read from `src/main.rs`:
/// `dispatch_with::<A>(command, vec![ProjectorReg::new(MyProjector, MY_VIEW)])`.
#[track_caller]
pub fn dispatch_with<A: Aggregate + 'static>(
    command: A::Command,
    projectors: Vec<ProjectorReg>,
) -> Result<Vec<::arc_core::event::Event>, String> {
    let site = Site::of(std::panic::Location::caller());
    guarded::<A>(&site, command, projectors)
}

fn guarded<A: Aggregate + 'static>(
    site: &Site,
    command: A::Command,
    projectors: Vec<ProjectorReg>,
) -> Result<Vec<::arc_core::event::Event>, String> {
    let bus = env("EVENT_BUS").unwrap_or_default().to_ascii_lowercase();
    if bus == "nats" {
        return Err("EVENT_BUS=nats: console writes use the in-process event bus and would not reach NATS/Benthos; run the write through the application instead".into());
    }
    let journal = Journal::current()?;
    let aggregate_id = command.aggregate_id().to_string();
    let label = A::aggregate_type();
    let (outcome, result) = journal.guard(site, label, || {
        let events = block_on(async {
            let stack = ::arc_web::helpers::es_stack::build_with_driver::<A>(
                ::arc_web::helpers::config::DatabaseDriver::from_env(),
                &database_url(),
                projectors,
                None,
            )
            .await
            .map_err(|error| format!("could not open the application's stores: {error}"))?;
            let mut context = ::arc_core::command_bus::CommandContext::system();
            context.session_id = Some(format!("arc-console-{}", journal.session));
            context.user_agent = Some("arc console".to_string());
            stack
                .command_bus
                .dispatch(command, context)
                .await
                .map_err(|error| match error {
                    ::arc_core::command_bus::CommandBusError::PublishFailed { .. } => format!(
                        "{error} (the events were appended to the event store; only publishing them to the projectors failed, so the write is not retried)"
                    ),
                    other => other.to_string(),
                })
        })?;
        ::__SERDE_JSON__::to_value(&events).map_err(|error| error.to_string())
    });
    let events: Vec<::arc_core::event::Event> = match result {
        Ok(value) => ::__SERDE_JSON__::from_value(value).map_err(|error| error.to_string())?,
        Err(error) => return Err(error),
    };
    if outcome != Outcome::Replayed {
        eprintln!(
            "arc console: dispatched to {label} {aggregate_id}: {} event(s) written",
            events.len()
        );
    }
    Ok(events)
}
"#;

/// The guard's source, shared with this crate's unit tests.
const GUARD_SOURCE: &str = include_str!("console_guard.rs");

// Compiled here only so its unit tests run without irust.
#[cfg(test)]
#[allow(dead_code)]
#[path = "console_guard.rs"]
mod guard;

/// The aggregate/projector registry the write helpers use. With `enabled`
/// false (no application modules, nothing registered, or the registry did not
/// compile), it knows no aggregate.
fn registry_source(app: &App, enabled: bool) -> String {
    let mut source = String::from(
        "//! Aggregates and projectors registered in the application's `src/main.rs`.\n",
    );
    if !enabled {
        source.push_str(
            "pub fn registered_projectors<A: 'static>() -> Option<Vec<::arc_web::ProjectorReg>> {\n    None\n}\n",
        );
        return source;
    }
    source.push_str("// `use` declarations of src/main.rs.\n");
    for declaration in &app.uses {
        source.push_str(declaration);
        source.push('\n');
    }
    source.push_str(
        "\npub fn registered_projectors<A: 'static>() -> Option<Vec<::arc_web::ProjectorReg>> {\n    let aggregate = ::std::any::TypeId::of::<A>();\n",
    );
    for registration in &app.registrations {
        source.push_str(&format!(
            "    if aggregate == ::std::any::TypeId::of::<{}>() {{\n        let mut projectors: Vec<::arc_web::ProjectorReg> = Vec::new();\n",
            registration.aggregate
        ));
        for projector in &registration.projectors {
            match projector {
                ProjectorExpr::One { projector, view } => source.push_str(&format!(
                    "        projectors.push(::arc_web::ProjectorReg::new({projector}, {view}));\n"
                )),
                ProjectorExpr::Many(list) => {
                    source.push_str(&format!("        projectors.extend({list});\n"))
                }
            }
        }
        source.push_str("        return Some(projectors);\n    }\n");
    }
    source.push_str("    None\n}\n");
    source
}

/// Statements irust runs at the top of every evaluation.
const STARTUP: &str = "load_env();\n";

fn write(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(()); // Keep the mtime so Cargo does not rebuild needlessly.
    }
    fs::write(path, contents)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// Writes the console workspace manifest, lockfile and startup file.
pub(crate) fn write_workspace(app: &App, console_dir: &Path) -> Result<(), String> {
    write(&console_dir.join("Cargo.toml"), &workspace_manifest(app))?;
    write(&console_dir.join("startup.rs"), STARTUP)?;
    let placeholder = console_dir.join(PLACEHOLDER);
    write(
        &placeholder.join("Cargo.toml"),
        "[package]\nname = \"arc_console_placeholder\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n",
    )?;
    write(&placeholder.join("src/lib.rs"), "")?;
    let lock = app.root.join("Cargo.lock");
    if lock.is_file() {
        let contents = fs::read_to_string(&lock)
            .map_err(|error| format!("could not read {}: {error}", lock.display()))?;
        write(&console_dir.join("Cargo.lock"), &contents)?;
    }
    // irust deletes its host crate on exit; a crashed session can leave one
    // without a manifest, which would break the workspace member glob.
    if let Ok(entries) = fs::read_dir(console_dir.join("irust_repls")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_host = entry.file_name().to_string_lossy().starts_with(HOST_PREFIX);
            if is_host && path.is_dir() && !path.join("Cargo.toml").is_file() {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
    Ok(())
}

fn write_prelude(app: &App, console_dir: &Path, selection: &Selection) -> Result<(), String> {
    let prelude = console_dir.join(PRELUDE_DIR);
    let src = prelude.join("src");
    write(&prelude.join("Cargo.toml"), &prelude_manifest(app))?;
    write(&src.join("lib.rs"), &prelude_source(app, selection))?;
    let fill = |source: &str| {
        source
            .replace("__TOKIO__", &app.crates.tokio)
            .replace("__DOTENVY__", &app.crates.dotenvy)
            .replace("__SERDE_JSON__", &app.crates.serde_json)
            .replace(
                "__AUTH_CORE__",
                app.crates.auth_core.as_deref().unwrap_or("arc_auth_core"),
            )
    };
    write(&src.join("arc_console.rs"), &fill(CONSOLE_HELPERS))?;
    link_app_entries(&app.root, &prelude)?;
    let writes = selection.helpers.contains(&Helper::Writes);
    for helper in Helper::ALL {
        let path = src.join(format!("{}.rs", helper.module()));
        let contents = match helper {
            _ if helper == Helper::Registry && writes => {
                registry_source(app, selection.helpers.contains(&Helper::Registry))
            }
            _ if !selection.helpers.contains(&helper) => {
                let _ = fs::remove_file(path);
                continue;
            }
            Helper::Stores => fill(STORES_HELPERS),
            Helper::Identity => fill(IDENTITY_HELPERS),
            Helper::Writes => fill(WRITES_HELPERS),
            Helper::Registry => unreachable!("the registry is written with the write helpers"),
        };
        write(&path, &contents)?;
    }
    let guard = src.join("arc_console_guard.rs");
    if writes {
        write(
            &guard,
            &GUARD_SOURCE.replace(
                "::serde_json as json",
                &format!("::{} as json", app.crates.serde_json),
            ),
        )?;
    } else {
        let _ = fs::remove_file(guard);
    }
    Ok(())
}

/// Entries of the application root the prelude never links to.
#[cfg(unix)]
const UNLINKED: &[&str] = &["src", "target", "Cargo.toml", "Cargo.lock", "build.rs"];

/// Links the application's top-level files and directories (`resources`,
/// `migrations`, `docs`, ...) into the prelude crate, so application modules
/// that embed files relative to `CARGO_MANIFEST_DIR` (for example
/// `include_dir!("$CARGO_MANIFEST_DIR/docs")`) compile in the console too.
/// The links are only read by the compiler.
#[cfg(unix)]
fn link_app_entries(root: &Path, prelude: &Path) -> Result<(), String> {
    let entries = fs::read_dir(prelude)
        .map_err(|error| format!("could not read {}: {error}", prelude.display()))?;
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    let entries = fs::read_dir(root)
        .map_err(|error| format!("could not read {}: {error}", root.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || UNLINKED.contains(&name_str.as_ref()) {
            continue;
        }
        let link = prelude.join(&name);
        if link.exists() {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), &link)
            .map_err(|error| format!("could not link {}: {error}", link.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn link_app_entries(_root: &Path, _prelude: &Path) -> Result<(), String> {
    Ok(())
}

enum BuildOutcome {
    Success,
    Failed(Vec<CompilerError>),
}

#[derive(Debug)]
struct CompilerError {
    message: String,
    rendered: String,
    files: Vec<PathBuf>,
}

fn build_prelude(console_dir: &Path, target_dir: &Path) -> Result<BuildOutcome, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(&cargo)
        .args(["build", "--message-format=json", "-p", "irust_prelude"])
        .current_dir(console_dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("could not run {}: {error}", Path::new(&cargo).display()))?;
    if output.status.success() {
        return Ok(BuildOutcome::Success);
    }
    Ok(BuildOutcome::Failed(parse_errors(
        &String::from_utf8_lossy(&output.stdout),
        &console_dir.join(PRELUDE_DIR),
    )))
}

/// Compiler errors reported for the prelude crate in Cargo's JSON messages.
fn parse_errors(stdout: &str, prelude_dir: &Path) -> Vec<CompilerError> {
    let mut errors = Vec::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-message"
            || message["target"]["name"] != "irust_prelude"
            || message["message"]["level"] != "error"
        {
            continue;
        }
        let diagnostic = &message["message"];
        let files = diagnostic["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|span| span["is_primary"] == true)
            .filter_map(|span| span["file_name"].as_str())
            .map(|file| {
                let file = PathBuf::from(file);
                if file.is_absolute() {
                    file
                } else {
                    prelude_dir.join(file)
                }
            })
            .collect();
        errors.push(CompilerError {
            message: diagnostic["message"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            rendered: diagnostic["rendered"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            files,
        });
    }
    errors
}

/// The modules and helpers whose files the errors point into.
fn attribute_errors(
    app: &App,
    console_dir: &Path,
    selection: &Selection,
    errors: &[CompilerError],
) -> Selection {
    let src = app.root.join("src");
    let prelude_src = console_dir.join(PRELUDE_DIR).join("src");
    let mut dropped = Selection {
        helpers: Vec::new(),
        modules: Vec::new(),
    };
    for file in errors.iter().flat_map(|error| &error.files) {
        if let Ok(relative) = file.strip_prefix(&src) {
            let Some(first) = relative.components().next() else {
                continue;
            };
            let first = first.as_os_str().to_string_lossy();
            let module = first.strip_suffix(".rs").unwrap_or(&first).to_string();
            if selection.modules.contains(&module) && !dropped.modules.contains(&module) {
                dropped.modules.push(module);
            }
        } else if let Ok(relative) = file.strip_prefix(&prelude_src) {
            let mut owner = selection
                .helpers
                .iter()
                .copied()
                .find(|helper| relative == Path::new(&format!("{}.rs", helper.module())));
            // The guard and the empty registry exist only for the write helpers.
            if owner.is_none()
                && (relative == Path::new("arc_console_guard.rs")
                    || relative == Path::new("arc_console_registry.rs"))
                && selection.helpers.contains(&Helper::Writes)
            {
                owner = Some(Helper::Writes);
            }
            if let Some(helper) = owner {
                if !dropped.helpers.contains(&helper) {
                    dropped.helpers.push(helper);
                }
            }
        }
    }
    dropped
}

fn first_message(errors: &[CompilerError], module_path: &Path) -> String {
    let file = module_path.with_extension("rs");
    errors
        .iter()
        .find(|error| {
            error
                .files
                .iter()
                .any(|f| f.starts_with(module_path) || f == &file)
        })
        .map(|error| error.message.clone())
        .unwrap_or_else(|| "compile error".into())
}

fn print_banner(app: &App, selection: &Selection) {
    let mut helpers = vec![
        "load_env()",
        "env(key)",
        "database_url()",
        "block_on(future)",
    ];
    if selection.helpers.contains(&Helper::Stores) {
        helpers.extend([
            "stores()",
            "read_models()",
            "rows(table)",
            "event_store()",
            "events(id)",
        ]);
    }
    if selection.helpers.contains(&Helper::Identity) {
        helpers.extend(["identities()", "users()"]);
    }
    if selection.helpers.contains(&Helper::Writes) {
        helpers.extend([
            "dispatch::<Aggregate>(command)",
            "dispatch_with::<Aggregate>(command, projectors)",
        ]);
    }
    eprintln!("Starting irust for {} ({})", app.name, app.root.display());
    eprintln!("  helpers: {}", helpers.join(", "));
    if selection.modules.is_empty() {
        eprintln!("  application modules: none");
    } else {
        eprintln!("  application modules: {}", selection.modules.join(", "));
    }
    eprintln!("  note: every evaluation reruns the statements entered before it");
    if selection.helpers.contains(&Helper::Writes) {
        eprintln!("        dispatch writes once per session; reruns return the first result");
    }
}

/// Removes the per-session host binaries irust built into the shared target
/// directory. Only files named after irust's host crates are touched.
fn remove_host_artifacts(target_dir: &Path) {
    for profile in ["debug", "release"] {
        let profile = target_dir.join(profile);
        for dir in [
            profile.clone(),
            profile.join("deps"),
            profile.join(".fingerprint"),
            profile.join("incremental"),
        ] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let name = name.strip_prefix("lib").unwrap_or(&name);
                if !name.starts_with(HOST_PREFIX) {
                    continue;
                }
                let path = entry.path();
                let _ = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "arc-console-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = r#"[package]
name = "shop"
version = "0.1.0"
edition = "2021"

[dependencies]
arc-core = "=0.9.4"
arc-web = "=0.9.4"
arc-auth-db = { version = "=0.9.4", features = ["x"] }
local-lib = { path = "../local-lib" }
maybe = { version = "1", optional = true }
diesel = { version = "2.2", features = ["sqlite"] }

[dev-dependencies]
ureq = "2"

[patch.crates-io]
arc-core = { path = "vendor/arc-core" }
"#;

    fn app(dir: &Path) -> App {
        fs::create_dir_all(dir.join("src/domain")).unwrap();
        fs::write(dir.join("Cargo.toml"), MANIFEST).unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "mod domain;\nmod routes;\n#[cfg(test)]\nmod tests;\nmod missing;\nfn main() {}\n",
        )
        .unwrap();
        fs::write(dir.join("src/domain.rs"), "").unwrap();
        fs::write(dir.join("src/routes.rs"), "").unwrap();
        fs::write(dir.join("Cargo.lock"), "# lock\n").unwrap();
        fs::create_dir_all(dir.join("migrations")).unwrap();
        read_app(dir).unwrap()
    }

    #[test]
    fn module_declarations_skip_inline_test_and_nested_modules() {
        let main = "use x;\nmod domain;\npub mod ui;\npub(crate) mod seed;\n\
                    #[cfg(test)]\nmod tests;\nmod inline {\n    mod nested;\n}\n\
                    #[allow(dead_code)]\nmod limits;\nmod domain;\n";
        assert_eq!(top_level_modules(main), ["domain", "ui", "seed", "limits"]);
    }

    const MAIN: &str = r#"use arc_web::{ArcApp, ArcAppBuilder};

use crate::domain::AppAggregate;
// arc:resource-imports
use crate::domain::post::aggregate::PostAggregate;
use crate::domain::post::projector::{
    PostProjector, POSTS_VIEW,
};

mod domain;

fn builder() -> ArcAppBuilder {
    use crate::inner::Hidden;
    ArcApp::builder()
        .register_aggregate::<AppAggregate>()
        // .register_projector(Commented, OUT)
        .register_ui(ui::contribution())
        .register_aggregate::<PostAggregate>()
        .register_projector(PostProjector, POSTS_VIEW)
        .register_projector(other::Counter::new("a,b"), "counts_view")
        .register_aggregate::< Order<Item> >()
        .register_projectors(vec![ProjectorReg::new(OrderProjector, ORDERS_VIEW)])
        .register_aggregate_with_stack::<Custom>(stack)
        .register_projector(NotForCustom, "x")
        .register_routes(routes::config)
}
"#;

    #[test]
    fn registrations_follow_the_builder_chain() {
        let found = registrations(MAIN);
        assert_eq!(
            found,
            [
                Registration {
                    aggregate: "AppAggregate".into(),
                    projectors: Vec::new(),
                },
                Registration {
                    aggregate: "PostAggregate".into(),
                    projectors: vec![
                        ProjectorExpr::One {
                            projector: "PostProjector".into(),
                            view: "POSTS_VIEW".into(),
                        },
                        ProjectorExpr::One {
                            projector: "other::Counter::new(\"a,b\")".into(),
                            view: "\"counts_view\"".into(),
                        },
                    ],
                },
                Registration {
                    aggregate: "Order<Item>".into(),
                    projectors: vec![ProjectorExpr::Many(
                        "vec![ProjectorReg::new(OrderProjector, ORDERS_VIEW)]".into()
                    )],
                },
            ]
        );
        assert_eq!(
            top_level_uses(MAIN),
            [
                "use arc_web::{ArcApp, ArcAppBuilder};",
                "use crate::domain::AppAggregate;",
                "use crate::domain::post::aggregate::PostAggregate;",
                "use crate::domain::post::projector::{ PostProjector, POSTS_VIEW, };",
            ]
        );
    }

    #[test]
    fn registry_names_registered_projectors() {
        let dir = TempDir::new("registry");
        let mut app = app(&dir.0);
        app.registrations = registrations(MAIN);
        app.uses = top_level_uses(MAIN);
        let source = registry_source(&app, true);
        for line in [
            "use crate::domain::post::aggregate::PostAggregate;",
            "    if aggregate == ::std::any::TypeId::of::<PostAggregate>() {",
            "        projectors.push(::arc_web::ProjectorReg::new(PostProjector, POSTS_VIEW));",
            "        projectors.extend(vec![ProjectorReg::new(OrderProjector, ORDERS_VIEW)]);",
            "    if aggregate == ::std::any::TypeId::of::<AppAggregate>() {",
        ] {
            assert!(source.contains(line), "missing {line:?} in\n{source}");
        }
        assert!(!source.contains("Custom") && !source.contains("Commented"));
        let empty = registry_source(&app, false);
        assert!(empty.contains("None") && !empty.contains("TypeId"));
        assert!(!empty.contains("use crate"));
        assert!(helpers_for(&app).contains(&Helper::Registry));
    }

    #[test]
    fn sessions_are_scoped_by_the_launch_environment() {
        let env = irust_env(
            Path::new("/app/target/arc-console"),
            Path::new("/t"),
            "42-7",
        );
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("ARC_CONSOLE_SESSION"), Some("42-7".into()));
        assert_eq!(
            get("ARC_CONSOLE_DIR"),
            Some("/app/target/arc-console".into())
        );
        assert_eq!(
            journal_path(Path::new("/c"), "42-7"),
            Path::new("/c/sessions/42-7.jsonl")
        );
        let (a, b) = (session_id(), session_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_digit() || c == '-'));
        // The guard compiled into the console uses the application's crate.
        assert!(GUARD_SOURCE.contains("use ::serde_json as json;"));
    }

    #[test]
    fn refuses_directories_that_are_not_arc_applications() {
        let dir = TempDir::new("refuse");
        let error = read_app(&dir.0).unwrap_err();
        assert!(error.contains("no Arc application found"), "{error}");
        fs::create_dir_all(dir.0.join("src")).unwrap();
        fs::write(dir.0.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(
            dir.0.join("Cargo.toml"),
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        assert!(read_app(&dir.0)
            .unwrap_err()
            .contains("no Arc application found"));
    }

    #[test]
    fn reads_dependencies_modules_and_paths() {
        let dir = TempDir::new("read");
        let app = app(&dir.0);
        let root = fs::canonicalize(&dir.0).unwrap();
        assert_eq!(app.root, root);
        assert_eq!(app.modules, ["domain", "routes"]);
        let deps = &app.dependencies;
        assert!(!deps.contains_key("maybe"), "optional deps are skipped");
        assert_eq!(
            deps["local-lib"]["path"].as_str().unwrap(),
            root.join("../local-lib").display().to_string()
        );
        assert_eq!(
            deps["tokio"]["features"].as_array().unwrap(),
            &vec![Value::from("rt-multi-thread")]
        );
        assert_eq!(deps["dotenvy"].as_str(), Some("0.15"));
        let core = deps["arc-auth-core"].as_table().unwrap();
        assert_eq!(core["version"].as_str(), Some("=0.9.4"));
        assert!(!core.contains_key("features"));
        assert_eq!(
            app.crates,
            HelperCrates {
                tokio: "tokio".into(),
                dotenvy: "dotenvy".into(),
                serde_json: "serde_json".into(),
                auth_core: Some("arc_auth_core".into()),
            }
        );
        assert_eq!(
            helpers_for(&app),
            [Helper::Stores, Helper::Identity, Helper::Writes],
            "no aggregate is registered, so there is no registry"
        );
    }

    #[test]
    fn path_arc_auth_db_pulls_sibling_arc_auth_core() {
        let mut deps = Table::new();
        deps.insert("arc-web".into(), "=1".into());
        let mut db = Table::new();
        db.insert("path".into(), "/arc/crates/arc-auth-db".into());
        deps.insert("arc-auth-db".into(), Value::Table(db));
        let mut tokio = Table::new();
        tokio.insert("package".into(), "tokio".into());
        tokio.insert("version".into(), "1".into());
        tokio.insert("features".into(), Value::Array(vec!["macros".into()]));
        deps.insert("rt".into(), Value::Table(tokio));
        deps.insert(
            "dotenv-crate".into(),
            Value::Table({
                let mut t = Table::new();
                t.insert("package".into(), "dotenvy".into());
                t.insert("version".into(), "0.15".into());
                t
            }),
        );
        let (deps, crates) = console_dependencies(&deps, Path::new("/app"));
        assert_eq!(
            deps["arc-auth-core"]["path"].as_str(),
            Some("/arc/crates/arc-auth-core")
        );
        // Renamed application dependencies are reused, never declared twice.
        assert_eq!(crates.tokio, "rt");
        assert_eq!(crates.dotenvy, "dotenv_crate");
        assert!(!deps.contains_key("tokio") && !deps.contains_key("dotenvy"));
        assert_eq!(
            deps["rt"]["features"].as_array().unwrap(),
            &vec![Value::from("macros"), Value::from("rt-multi-thread")]
        );
    }

    #[test]
    fn generated_manifests_pin_the_application() {
        let dir = TempDir::new("manifests");
        let app = app(&dir.0);
        let workspace: Table = workspace_manifest(&app).parse().unwrap();
        assert_eq!(
            workspace["workspace"]["members"].as_array().unwrap(),
            &vec![
                Value::from("irust/irust_prelude"),
                Value::from("irust_repls/irust_host_repl_*")
            ]
        );
        assert_eq!(workspace["workspace"]["resolver"].as_str(), Some("2"));
        assert_eq!(
            workspace["patch"]["crates-io"]["arc-core"]["path"].as_str(),
            Some(
                app.root
                    .join("vendor/arc-core")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        let prelude: Table = prelude_manifest(&app).parse().unwrap();
        assert_eq!(prelude["package"]["name"].as_str(), Some("irust_prelude"));
        assert_eq!(prelude["package"]["edition"].as_str(), Some("2021"));
        assert_eq!(prelude["dependencies"]["arc-web"].as_str(), Some("=0.9.4"));
        assert!(!prelude.contains_key("dev-dependencies"));
    }

    #[test]
    fn prelude_reexports_dependencies_and_includes_app_modules() {
        let dir = TempDir::new("source");
        let app = app(&dir.0);
        let selection = Selection {
            helpers: vec![Helper::Stores],
            modules: vec!["domain".into(), "routes".into()],
        };
        let source = prelude_source(&app, &selection);
        for line in [
            "pub use ::arc_web;",
            "pub use ::arc_auth_db;",
            "pub use ::local_lib;",
            "pub mod arc_console;",
            "pub mod arc_console_stores;",
            &format!("#[path = {:?}]", app.root.join("src").display().to_string()),
            "    pub mod domain;",
            "pub use arc_app::{domain, routes};",
        ] {
            assert!(source.contains(line), "missing {line:?} in\n{source}");
        }
        assert!(!source.contains("arc_console_identity"));
        assert!(!source.contains("arc_console_guard"));
        let writes = prelude_source(
            &app,
            &Selection {
                helpers: vec![Helper::Writes, Helper::Registry],
                modules: vec!["domain".into()],
            },
        );
        for line in [
            "pub mod arc_console_writes;\npub use arc_console_writes::*;",
            "mod arc_console_guard;\nmod arc_console_registry;",
        ] {
            assert!(writes.contains(line), "missing {line:?} in\n{writes}");
        }
        assert!(!writes.contains("pub use arc_console_registry"));
        let bare = prelude_source(
            &app,
            &Selection {
                helpers: Vec::new(),
                modules: Vec::new(),
            },
        );
        assert!(!bare.contains("mod arc_app"));
    }

    #[test]
    fn preparation_writes_only_under_target() {
        let dir = TempDir::new("write");
        let app = app(&dir.0);
        let before: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        let console = app.root.join(CONSOLE_DIR);
        fs::create_dir_all(console.join("irust_repls/irust_host_repl_stale")).unwrap();
        fs::create_dir_all(console.join("irust_repls/irust_host_repl_live/src")).unwrap();
        fs::write(
            console.join("irust_repls/irust_host_repl_live/Cargo.toml"),
            "",
        )
        .unwrap();
        write_workspace(&app, &console).unwrap();
        let selection = Selection {
            helpers: helpers_for(&app),
            modules: app.modules.clone(),
        };
        write_prelude(&app, &console, &selection).unwrap();
        assert_eq!(
            fs::read_to_string(console.join("Cargo.lock")).unwrap(),
            "# lock\n"
        );
        assert_eq!(
            fs::read_to_string(console.join("startup.rs")).unwrap(),
            STARTUP
        );
        assert!(console.join(PLACEHOLDER).join("Cargo.toml").is_file());
        for file in [
            "irust/irust_prelude/Cargo.toml",
            "irust/irust_prelude/src/lib.rs",
            "irust/irust_prelude/src/arc_console.rs",
            "irust/irust_prelude/src/arc_console_stores.rs",
            "irust/irust_prelude/src/arc_console_identity.rs",
            "irust/irust_prelude/src/arc_console_writes.rs",
            "irust/irust_prelude/src/arc_console_guard.rs",
            "irust/irust_prelude/src/arc_console_registry.rs",
        ] {
            assert!(console.join(file).is_file(), "{file}");
        }
        let prelude_src = console.join("irust/irust_prelude/src");
        let writes = fs::read_to_string(prelude_src.join("arc_console_writes.rs")).unwrap();
        assert!(writes.contains("::serde_json::to_value") && !writes.contains("__"));
        // Nothing is registered in this application: the registry is empty.
        let registry = fs::read_to_string(prelude_src.join("arc_console_registry.rs")).unwrap();
        assert!(registry.contains("None") && !registry.contains("TypeId"));
        assert!(!console.join("irust_repls/irust_host_repl_stale").exists());
        #[cfg(unix)]
        {
            let prelude = console.join(PRELUDE_DIR);
            assert!(fs::symlink_metadata(prelude.join("migrations"))
                .unwrap()
                .is_symlink());
            assert!(!prelude.join("target").exists(), "target is never linked");
            assert!(!fs::symlink_metadata(prelude.join("src"))
                .unwrap()
                .is_symlink());
            assert!(!fs::symlink_metadata(prelude.join("Cargo.toml"))
                .unwrap()
                .is_symlink());
        }
        assert!(console.join("irust_repls/irust_host_repl_live").exists());
        // Dropping a helper removes its file.
        write_prelude(
            &app,
            &console,
            &Selection {
                helpers: vec![Helper::Stores],
                modules: Vec::new(),
            },
        )
        .unwrap();
        assert!(!console
            .join("irust/irust_prelude/src/arc_console_identity.rs")
            .exists());
        for file in [
            "arc_console_writes.rs",
            "arc_console_guard.rs",
            "arc_console_registry.rs",
        ] {
            assert!(!prelude_src.join(file).exists(), "{file}");
        }
        let mut after: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        after.retain(|path| !path.ends_with("target"));
        assert_eq!(before.len(), after.len());
        assert_eq!(
            fs::read_to_string(dir.0.join("Cargo.toml")).unwrap(),
            MANIFEST
        );
    }

    #[test]
    fn compiler_errors_are_attributed_to_modules_and_helpers() {
        let dir = TempDir::new("errors");
        let app = app(&dir.0);
        let console = app.root.join(CONSOLE_DIR);
        let src = app.root.join("src");
        let message = |target: &str, file: &str| {
            serde_json::json!({
                "reason": "compiler-message",
                "target": {"name": target},
                "message": {
                    "level": "error",
                    "message": "cannot find value `MIGRATIONS` in the crate root",
                    "rendered": "error: rendered\n",
                    "spans": [{"file_name": file, "is_primary": true}],
                },
            })
            .to_string()
        };
        let stdout = [
            message(
                "irust_prelude",
                &src.join("routes.rs").display().to_string(),
            ),
            message(
                "irust_prelude",
                &src.join("domain/post/ui.rs").display().to_string(),
            ),
            message("irust_prelude", "src/arc_console_identity.rs"),
            message("irust_prelude", "src/arc_console_guard.rs"),
            message("other", &src.join("domain.rs").display().to_string()),
            "not json".into(),
        ]
        .join("\n");
        let errors = parse_errors(&stdout, &console.join(PRELUDE_DIR));
        assert_eq!(errors.len(), 4);
        let selection = Selection {
            helpers: vec![Helper::Stores, Helper::Identity, Helper::Writes],
            modules: vec!["domain".into(), "routes".into()],
        };
        let dropped = attribute_errors(&app, &console, &selection, &errors);
        assert_eq!(dropped.modules, ["routes", "domain"]);
        // The guard belongs to the write helpers.
        assert_eq!(dropped.helpers, [Helper::Identity, Helper::Writes]);
        // A registry that does not compile is dropped on its own, keeping writes.
        let registry = parse_errors(
            &message("irust_prelude", "src/arc_console_registry.rs"),
            &console.join(PRELUDE_DIR),
        );
        let with_registry = Selection {
            helpers: vec![Helper::Writes, Helper::Registry],
            modules: Vec::new(),
        };
        assert_eq!(
            attribute_errors(&app, &console, &with_registry, &registry).helpers,
            [Helper::Registry]
        );
        assert_eq!(
            first_message(&errors, &src.join("routes")),
            "cannot find value `MIGRATIONS` in the crate root"
        );
    }

    #[test]
    fn finds_programs_on_path_and_cleans_host_artifacts() {
        let dir = TempDir::new("path");
        assert!(find_program("irust", Some(dir.0.clone().into_os_string())).is_none());
        assert!(find_program("irust", None).is_none());
        let target = dir.0.join("target");
        let deps = target.join("debug/deps");
        fs::create_dir_all(deps.join("irust_host_repl_abc-dir")).unwrap();
        fs::write(target.join("debug/irust_host_repl_abc"), "").unwrap();
        fs::write(deps.join("irust_host_repl_abc-123"), "").unwrap();
        fs::write(deps.join("libirust_host_repl_abc-123.rmeta"), "").unwrap();
        fs::write(deps.join("libirust_prelude-1.rlib"), "").unwrap();
        fs::write(deps.join("app-123"), "").unwrap();
        remove_host_artifacts(&target);
        assert!(!target.join("debug/irust_host_repl_abc").exists());
        assert!(!deps.join("irust_host_repl_abc-123").exists());
        assert!(!deps.join("irust_host_repl_abc-dir").exists());
        assert!(!deps.join("libirust_host_repl_abc-123.rmeta").exists());
        assert!(deps.join("libirust_prelude-1.rlib").exists());
        assert!(deps.join("app-123").exists());
    }
}
