//! Launch contract of `arc console`, proven with fake `irust` and `cargo`
//! executables so the test needs neither irust nor a network.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{fs, io};

struct Sandbox {
    base: PathBuf,
    cli: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("arc-console-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(&base).unwrap();
        let target = base.join("cli-target");
        // arc-app also has a binary named `arc`; build this package's own.
        let output = Command::new(env!("CARGO"))
            .args([
                "build",
                "--offline",
                "--locked",
                "-p",
                "arc-web-cli",
                "--bin",
                "arc",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(output.status.success(), "CLI build failed: {output:?}");
        Self {
            cli: target
                .join("debug")
                .join(format!("arc{}", std::env::consts::EXE_SUFFIX)),
            base,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every file under `root` except `target/`, with its contents.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if relative == Path::new("target") {
                continue;
            }
            if path.is_dir() {
                out.insert(relative, Vec::new());
                visit(root, &path, out)?;
            } else {
                out.insert(relative, fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out).unwrap();
    out
}

fn console(sandbox: &Sandbox, cwd: &Path, path: &str, args: &[&str], cargo: &Path) -> Output {
    Command::new(&sandbox.cli)
        .arg("console")
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("CARGO", cargo)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

#[test]
fn console_prepares_the_application_and_launches_irust() {
    let sandbox = Sandbox::new();
    let work = sandbox.base.join("work");
    fs::create_dir_all(&work).unwrap();
    let output = Command::new(&sandbox.cli)
        .args(["new", "shop", "--ui", "--no-git"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let app = work.join("shop");
    fs::write(app.join(".env"), "DATABASE_URL=database/database.sqlite\n").unwrap();
    fs::create_dir_all(app.join("database")).unwrap();
    fs::write(app.join("database/database.sqlite"), "not really sqlite").unwrap();
    fs::write(app.join("Cargo.lock"), "# application lock\n").unwrap();
    let before = snapshot(&app);

    let bin = sandbox.base.join("bin");
    let log = sandbox.base.join("log");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&log).unwrap();
    // The fake irust records its launch contract.
    script(
        &bin.join("irust"),
        &format!(
            "pwd -P > {log}/irust-cwd\nprintf '%s\\n' \"$@\" > {log}/irust-args\n\
             printf '%s\\n%s\\n%s\\n' \"$IRUST_DATA_DIR\" \"$IRUST_TEMP_DIR\" \"$CARGO_TARGET_DIR\" > {log}/irust-env\n\
             touch \"$CARGO_TARGET_DIR/debug/irust_host_repl_0123\"\n\
             exit \"${{FAKE_IRUST_STATUS:-0}}\"\n",
            log = log.display()
        ),
    );
    // The fake cargo rejects the `routes` module once, as rustc would when an
    // application module names an item that only exists in `main.rs`.
    let cargo = sandbox.base.join("fake-cargo");
    script(
        &cargo,
        &format!(
            "pwd -P >> {log}/cargo-cwd\nprintf '%s ' \"$@\" >> {log}/cargo-args\necho >> {log}/cargo-args\n\
             echo \"$CARGO_TARGET_DIR\" > {log}/cargo-target\nmkdir -p \"$CARGO_TARGET_DIR/debug\"\n\
             if grep -q 'pub mod routes;' irust/irust_prelude/src/lib.rs; then\n\
               root=$(cd ../.. && pwd -P)\n\
               printf '%s\\n' '{{\"reason\":\"compiler-artifact\"}}'\n\
               printf '{{\"reason\":\"compiler-message\",\"target\":{{\"name\":\"irust_prelude\"}},\"message\":{{\"level\":\"error\",\"message\":\"cannot find value `MIGRATIONS`\",\"rendered\":\"error\",\"spans\":[{{\"file_name\":\"%s/src/routes.rs\",\"is_primary\":true}}]}}}}\\n' \"$root\"\n\
               exit 101\nfi\nexit 0\n",
            log = log.display()
        ),
    );
    let path = format!("{}:/usr/bin:/bin", bin.display());

    let output = console(&sandbox, &app, &path, &["--", "--bare-repl"], &cargo);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: skipping application module `routes`")
            && stderr.contains("cannot find value `MIGRATIONS`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("application modules: domain, ui"),
        "{stderr}"
    );
    assert!(
        stderr.contains("users()") || stderr.contains("rows(table)"),
        "{stderr}"
    );

    let root = fs::canonicalize(&app).unwrap();
    let console_dir = root.join("target/arc-console");
    let read = |name: &str| fs::read_to_string(log.join(name)).unwrap();
    assert_eq!(read("irust-cwd").trim(), root.display().to_string());
    assert_eq!(
        read("irust-args"),
        format!(
            "{}\n--bare-repl\n",
            console_dir.join("startup.rs").display()
        )
    );
    assert_eq!(
        read("irust-env"),
        format!(
            "{0}\n{0}\n{1}\n",
            console_dir.display(),
            root.join("target").display()
        )
    );
    // Two builds: with every module, then without the failing one.
    let cargo_args = read("cargo-args");
    assert_eq!(
        cargo_args.lines().collect::<Vec<_>>(),
        ["build --message-format=json -p irust_prelude "; 2]
    );
    assert!(read("cargo-cwd")
        .lines()
        .all(|line| line == console_dir.display().to_string()));
    assert_eq!(
        read("cargo-target").trim(),
        root.join("target").display().to_string()
    );

    let lib = fs::read_to_string(console_dir.join("irust/irust_prelude/src/lib.rs")).unwrap();
    for expected in [
        "pub use ::arc_web;",
        "pub use ::arc_core;",
        "pub mod arc_console;",
        "pub mod arc_console_stores;",
        &format!("#[path = {:?}]", root.join("src").display().to_string()),
        "    pub mod domain;",
        "pub use arc_app::{domain, ui};",
    ] {
        assert!(lib.contains(expected), "missing {expected:?}:\n{lib}");
    }
    assert!(!lib.contains("pub mod routes;"), "{lib}");
    let manifest = fs::read_to_string(console_dir.join("irust/irust_prelude/Cargo.toml")).unwrap();
    assert!(manifest.contains("name = \"irust_prelude\""), "{manifest}");
    assert!(manifest.contains("arc-web = \""), "{manifest}");
    assert_eq!(
        fs::read_to_string(console_dir.join("Cargo.lock")).unwrap(),
        "# application lock\n"
    );
    assert_eq!(
        fs::read_to_string(console_dir.join("startup.rs")).unwrap(),
        "load_env();\n"
    );
    assert!(!root.join("target/debug/irust_host_repl_0123").exists());
    assert_eq!(
        snapshot(&app),
        before,
        "the application outside target/ changed"
    );

    // irust's exit status is reported.
    let output = Command::new(&sandbox.cli)
        .arg("console")
        .current_dir(&app)
        .env("PATH", &path)
        .env("CARGO", &cargo)
        .env("FAKE_IRUST_STATUS", "3")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error: irust exited with"));

    // --no-app-modules compiles no application module.
    let output = console(&sandbox, &app, &path, &["--no-app-modules"], &cargo);
    assert!(output.status.success(), "{output:?}");
    let lib = fs::read_to_string(console_dir.join("irust/irust_prelude/src/lib.rs")).unwrap();
    assert!(!lib.contains("mod arc_app"), "{lib}");
    assert_eq!(snapshot(&app), before);
}

#[test]
fn console_refuses_without_irust_or_outside_an_application() {
    let sandbox = Sandbox::new();
    let work = sandbox.base.join("work");
    fs::create_dir_all(&work).unwrap();
    let output = Command::new(&sandbox.cli)
        .args(["new", "shop", "--no-git"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let app = work.join("shop");
    let before = snapshot(&app);
    let cargo = sandbox.base.join("fake-cargo");
    script(&cargo, "touch cargo-ran\nexit 0\n");
    let empty_bin = sandbox.base.join("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    let path = format!("{}:/usr/bin:/bin", empty_bin.display());

    let output = console(&sandbox, &app, &path, &[], &cargo);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("irust was not found on PATH") && stderr.contains("cargo install irust"),
        "{stderr}"
    );
    assert!(
        !app.join("target").exists(),
        "nothing is generated without irust"
    );
    assert_eq!(snapshot(&app), before);

    let bin = sandbox.base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    script(&bin.join("irust"), "exit 0\n");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let outside = sandbox.base.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let output = console(&sandbox, &outside, &path, &[], &cargo);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no Arc application found"),
        "{output:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    let output = console(&sandbox, &app, &path, &["--bogus"], &cargo);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option `--bogus`"));
    assert!(!app.join("target").exists());
}
