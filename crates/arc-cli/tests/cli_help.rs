use std::{collections::BTreeMap, fs, path::Path, process::Command};

struct Sandbox {
    base: std::path::PathBuf,
    work: std::path::PathBuf,
    cli: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("arc-help-{}-{nonce}", std::process::id()));
        let work = path.join("work");
        fs::create_dir_all(&work).unwrap();
        let target = path.join("target");
        // arc-app also has a binary named `arc`. Build this package into its
        // own target directory so workspace tests cannot run the app instead.
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
            base: path,
            work,
            cli: target
                .join("debug")
                .join(format!("arc{}", std::env::consts::EXE_SUFFIX)),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}

fn snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            result.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                if path.is_dir() {
                    Vec::new()
                } else {
                    fs::read(&path).unwrap()
                },
            );
            if path.is_dir() {
                visit(root, &path, result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn every_command_help_is_specific_and_read_only() {
    let sandbox = Sandbox::new();
    let output = Command::new(&sandbox.cli)
        .args(["new", "sample", "--ui", "--no-git"])
        .current_dir(&sandbox.work)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let app = sandbox.work.join("sample");
    let before = snapshot(&sandbox.work);
    let cases: &[(&[&str], &str)] = &[
        (&["generate", "resource"], "arc generate resource <name>"),
        (&["generate", "aggregate"], "arc generate aggregate <name>"),
        (&["new"], "arc new <name>"),
        (&["generate"], "arc generate <command>"),
        (&["plugin"], "arc plugin <command>"),
        (&["plugin", "add"], "arc plugin add <name>"),
        (&["plugin", "migrate-auth-ui"], "arc plugin migrate-auth-ui"),
        (&["new", "demo", "--ui"], "arc new <name>"),
        (&["new", "demo", "--android"], "arc new <name>"),
        (
            &["generate", "resource", "Note", "--ffi"],
            "arc generate resource <name>",
        ),
        (&["doctor", "--android"], "arc doctor"),
        (
            &["generate", "resource", "Product", "--api"],
            "arc generate resource <name>",
        ),
        (
            &["generate", "aggregate", "Product", "--ui-auth"],
            "arc generate aggregate <name>",
        ),
        (
            &["generate", "resource", "Product", "--roles"],
            "arc generate resource <name>",
        ),
        (
            &["generate", "resource", "Product", "--api-auth"],
            "arc generate resource <name>",
        ),
        (
            &["plugin", "add", "auth-db-session"],
            "arc plugin add <name>",
        ),
        (&[], "Arc application generator"),
    ];
    for (command, expected) in cases {
        for flag in ["--help", "-h"] {
            for cwd in [&app, &sandbox.work] {
                let output = Command::new(&sandbox.cli)
                    .args(*command)
                    .arg(flag)
                    .current_dir(cwd)
                    .output()
                    .unwrap();
                assert!(
                    snapshot(&sandbox.work) == before,
                    "{command:?} {flag} modified files"
                );
                assert!(output.status.success(), "{command:?} {flag}: {output:?}");
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains(expected), "{command:?} {flag}: {stdout}");
                assert!(stdout.contains("Usage:"), "{stdout}");
                assert!(output.stderr.is_empty(), "{output:?}");
            }
        }
    }
    for (command, required_lines) in [
        (vec!["--help"], vec!["arc doctor --android"]),
        (
            vec!["generate", "--help"],
            vec![
                "--ffi",
                "--api",
                "--ui",
                "--api-auth",
                "--ui-auth",
                "--roles",
                "incompatible",
            ],
        ),
        (
            vec!["generate", "resource", "--help"],
            vec![
                "--ffi",
                "--api",
                "--ui",
                "--api-auth",
                "--ui-auth",
                "--roles",
                "Incompatible",
            ],
        ),
        (
            vec!["doctor", "--help"],
            vec!["arc doctor --android", "--android"],
        ),
    ] {
        let output = Command::new(&sandbox.cli)
            .args(&command)
            .current_dir(&app)
            .output()
            .unwrap();
        assert!(output.status.success(), "{command:?}: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in required_lines {
            assert!(
                stdout.contains(line),
                "{command:?} missing {line:?}: {stdout}"
            );
        }
    }
    for (command, expected) in [
        (vec!["new", "demo", "--android"], "arc new <name>"),
        (
            vec!["generate", "resource", "Note", "--ffi"],
            "arc generate resource <name>",
        ),
    ] {
        for position in 1..=command.len() {
            for flag in ["--help", "-h"] {
                let mut args = command.clone();
                args.insert(position, flag);
                let output = Command::new(&sandbox.cli)
                    .args(&args)
                    .current_dir(&app)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{args:?}: {output:?}");
                let expected_at_position = if command[0] == "generate" && position == 1 {
                    "arc generate <command>"
                } else {
                    expected
                };
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains(expected_at_position),
                    "{args:?}: {output:?}"
                );
                assert_eq!(snapshot(&sandbox.work), before, "{args:?} modified files");
            }
        }
    }
    for (args, expected) in [
        (vec!["new", "--help", "demo"], "arc new <name>"),
        (
            vec!["generate", "resource", "-h", "Product"],
            "arc generate resource <name>",
        ),
        (
            vec!["plugin", "add", "--help", "auth-db"],
            "arc plugin add <name>",
        ),
    ] {
        let output = Command::new(&sandbox.cli)
            .args(&args)
            .current_dir(&app)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        assert!(snapshot(&sandbox.work) == before, "{args:?} modified files");
    }
}

#[test]
fn invalid_commands_and_extra_plugin_arguments_do_not_write() {
    let sandbox = Sandbox::new();
    let cases: &[&[&str]] = &[
        &["unknown", "--help"],
        &["generate", "unknown", "--help"],
        &["plugin", "unknown", "-h"],
        &["new"],
        &["new", "--ui"],
        &["generate", "resource"],
        &["generate", "resource", "--api"],
        &["plugin", "add"],
        &["plugin", "add", "auth-db", "unexpected"],
        &["plugin", "migrate-auth-ui", "unexpected"],
    ];
    for args in cases {
        let output = Command::new(&sandbox.cli)
            .args(*args)
            .current_dir(&sandbox.work)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
        assert!(
            snapshot(&sandbox.work).is_empty(),
            "{args:?} modified files"
        );
    }

    for args in [vec!["generate"], vec!["generate", "resource"]] {
        let output = Command::new(&sandbox.cli)
            .args(&args)
            .current_dir(&sandbox.work)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        for option in [
            "--ffi",
            "--api",
            "--ui",
            "--api-auth",
            "--ui-auth",
            "--roles",
        ] {
            assert!(
                stderr.contains(option),
                "{args:?} missing {option}: {stderr}"
            );
        }
        assert!(stderr.contains("incompatible"), "{args:?}: {stderr}");
    }
}

#[test]
fn root_help_and_version_remain_available() {
    let sandbox = Sandbox::new();
    for args in [vec![], vec!["--version"], vec!["-V"]] {
        let output = Command::new(&sandbox.cli)
            .args(&args)
            .current_dir(&sandbox.work)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        if args.is_empty() {
            assert!(stdout.contains("Arc application generator"));
        } else {
            assert_eq!(stdout.trim(), format!("arc {}", env!("CARGO_PKG_VERSION")));
        }
        assert!(snapshot(&sandbox.work).is_empty());
    }
}

#[test]
fn android_option_errors_leave_tree_unchanged() {
    let sandbox = Sandbox::new();
    let cases: &[(&[&str], &str)] = &[
        (&["new", "demo", "--android", "--ui"], "--android"),
        (&["new", "demo", "--android", "--api"], "--android"),
        (&["new", "demo", "--arc-path", "."], "--arc-path"),
        (&["generate", "resource", "Note", "--ffi", "--api"], "--ffi"),
        (&["generate", "resource", "Note", "--ffi", "--ui"], "--ffi"),
        (
            &["generate", "resource", "Note", "--ffi", "--roles"],
            "--ffi",
        ),
        (
            &["generate", "resource", "Note", "--ffi", "--api-auth"],
            "--ffi",
        ),
        (
            &["generate", "resource", "Note", "--ffi", "--ui-auth"],
            "--ffi",
        ),
    ];
    for (args, diagnostic) in cases {
        let before = snapshot(&sandbox.work);
        let output = Command::new(&sandbox.cli)
            .args(*args)
            .current_dir(&sandbox.work)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{args:?}: {output:?}"
        );
        assert_eq!(snapshot(&sandbox.work), before, "{args:?} modified files");
    }
}

#[test]
fn plugin_help_output_is_unchanged() {
    const PLUGIN_ADD_HELP: &str = "Install an Arc authentication plugin

Usage:
  arc plugin add <name>

Plugins:
  auth-db-session  Browser sessions, administration and RBAC
  auth-db-jwt      JWT APIs and RBAC
  auth-db          Database identity provider
  auth-session     Browser session authentication
  auth-admin       User and role administration
  auth-jwt         JWT API authentication
  auth-rbac        Role-based access control

Options:
  -h, --help  Show help

Run from the root of an application created by `arc new`.
";
    let sandbox = Sandbox::new();
    for args in [
        vec!["plugin", "add", "--help"],
        vec!["plugin", "add", "-h"],
        vec!["plugin", "add", "auth-db", "--help"],
    ] {
        let output = Command::new(&sandbox.cli)
            .args(&args)
            .current_dir(&sandbox.work)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, PLUGIN_ADD_HELP, "{args:?}");
        assert!(!stdout.contains("arc doctor"), "{args:?}: {stdout}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}
