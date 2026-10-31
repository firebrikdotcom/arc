use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

mod android;
mod plugin;
mod resource;
mod scaffold;

use resource::{create_resource, NewResource};
use scaffold::{create_project, NewProject};

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args: Vec<_> = args.collect();
    // Resolve help before consuming positional names or invoking any command.
    // This also handles help following an option that normally takes a value.
    if print_requested_help(&args)? {
        return Ok(());
    }
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("new") => {
            let name = args
                .next()
                .filter(|name| !name.starts_with('-'))
                .ok_or_else(|| {
                    "usage: arc new <name> [--ui] [--api] [--android] [--arc-path PATH] [--no-git]"
                        .to_string()
                })?;
            let mut ui = false;
            let mut api = false;
            let mut git = true;
            let mut android = false;
            let mut arc_path = None;

            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--ui" => ui = true,
                    "--api" => api = true,
                    "--no-git" => git = false,
                    "--android" => android = true,
                    "--arc-path" => {
                        let path = args
                            .next()
                            .filter(|value| !value.is_empty() && !value.starts_with('-'))
                            .ok_or("--arc-path requires a directory")?;
                        arc_path = Some(PathBuf::from(path));
                    }
                    unknown => return Err(format!("unknown option `{unknown}`")),
                }
            }

            if android && (ui || api) {
                return Err("--android cannot be combined with --ui or --api".into());
            }
            if !android && arc_path.is_some() {
                return Err("--arc-path requires --android".into());
            }
            if android {
                let path =
                    android::create_project(&name, &PathBuf::from("."), arc_path.as_deref(), git)?;
                println!("Created {name} at {}\n\n  cd {name}\n  make bindings\n  make test\n  make build", path.display());
                return Ok(());
            }
            let project = NewProject {
                name,
                destination: PathBuf::from("."),
                ui,
                api,
                git,
            };
            let path = create_project(&project)?;
            println!();
            println!("Created {} at {}", project.name, path.display());
            println!();
            println!("  cd {}", project.name);
            println!("  make setup");
            println!("  make dev");
            Ok(())
        }
        Some("generate") => {
            let kind = args.next().ok_or_else(|| {
                "usage: arc generate resource <name> [--ffi] [--api] [--ui] [--api-auth jwt|none] [--ui-auth session|none] [--roles LIST] (alias: aggregate; --ffi is incompatible with --api, --ui, --api-auth, --ui-auth, and --roles)".to_string()
            })?;
            if kind != "resource" && kind != "aggregate" {
                return Err(format!(
                    "unknown generator `{kind}`; expected `resource` or `aggregate`"
                ));
            }
            let name = args.next().ok_or_else(|| {
                "usage: arc generate resource <name> [--ffi] [--api] [--ui] [--api-auth jwt|none] [--ui-auth session|none] [--roles LIST] (alias: aggregate; --ffi is incompatible with --api, --ui, --api-auth, --ui-auth, and --roles)".to_string()
            })?;
            if name.starts_with('-') {
                return Err("resource name is required; run `arc generate resource --help`".into());
            }
            if args.as_slice().iter().any(|arg| arg == "--ffi")
                && args.as_slice().iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "--api" | "--ui" | "--api-auth" | "--ui-auth" | "--roles"
                    )
                })
            {
                return Err(
                    "--ffi cannot be combined with --api, --ui, --api-auth, --ui-auth, or --roles"
                        .into(),
                );
            }
            let mut api = false;
            let mut ui = false;
            let mut api_auth = None;
            let mut ui_auth = None;
            let mut roles = Vec::new();
            let mut ffi = false;
            let mut web_option = false;
            while let Some(argument) = args.next() {
                if matches!(
                    argument.as_str(),
                    "--api" | "--ui" | "--api-auth" | "--ui-auth" | "--roles"
                ) {
                    web_option = true;
                }
                match argument.as_str() {
                    "--ffi" => ffi = true,
                    "--api" => api = true,
                    "--ui" => ui = true,
                    "--api-auth" => {
                        api_auth = Some(args.next().ok_or("--api-auth requires jwt or none")?)
                    }
                    "--ui-auth" => {
                        ui_auth = Some(args.next().ok_or("--ui-auth requires session or none")?)
                    }
                    "--roles" => {
                        roles = args
                            .next()
                            .ok_or("--roles requires a comma-separated value")?
                            .split(',')
                            .map(str::to_owned)
                            .collect()
                    }
                    _ => return Err(format!("unknown option `{argument}`")),
                }
            }

            if ffi && web_option {
                return Err(
                    "--ffi cannot be combined with --api, --ui, --api-auth, --ui-auth, or --roles"
                        .into(),
                );
            }
            let android_project = android::read_manifest(std::path::Path::new("."))?;
            if ffi && android_project.is_none() {
                return Err(
                    "--ffi requires an Arc Android project; run arc new <name> --android".into(),
                );
            }
            if !ffi && android_project.is_some() {
                return Err("Android resources require --ffi".into());
            }
            let resource = NewResource {
                name,
                root: PathBuf::from("."),
                api,
                ui,
                api_auth,
                ui_auth,
                roles,
            };
            let created = create_resource(&resource)?;
            println!();
            println!("Generated {} resource:", created.type_name);
            for path in created.files {
                println!("  {}", path.display());
            }
            println!();
            if ffi {
                println!("Run `make test build`; schema is applied when the resource core opens.");
            } else if let Some(api_path) = created.api_path {
                println!("Run `make migrate`, then use the CRUD API at {api_path}.");
            } else {
                println!(
                    "Run `make migrate` and add routes that dispatch {}Command.",
                    created.type_name
                );
            }
            Ok(())
        }
        Some("doctor") => {
            if args.next().as_deref() != Some("--android") {
                return Err("usage: arc doctor --android".into());
            }
            if let Some(argument) = args.next() {
                return Err(format!("unknown option `{argument}`"));
            }
            android::doctor()
        }
        Some("plugin") => {
            let operation = args.next();
            if operation.as_deref() == Some("migrate-auth-ui") {
                if let Some(argument) = args.next() {
                    return Err(format!("unexpected argument `{argument}`"));
                }
                plugin::migrate_auth_ui(PathBuf::from("."))?;
                println!("Auth UI host migration complete.");
                return Ok(());
            }
            if operation.as_deref() != Some("add") {
                return Err("usage: arc plugin <add NAME|migrate-auth-ui>".into());
            }
            let name = args.next().ok_or("plugin name is required")?;
            if name.starts_with('-') {
                return Err("plugin name is required; run `arc plugin add --help`".into());
            }
            if let Some(argument) = args.next() {
                return Err(format!("unexpected argument `{argument}`"));
            }
            plugin::add_plugin(PathBuf::from("."), &name)?;
            println!("Installed {name}.");
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("arc {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(command) => Err(format!(
            "unknown command `{command}`; run `arc --help` for usage"
        )),
    }
}

fn print_requested_help(args: &[String]) -> Result<bool, String> {
    let Some(position) = args
        .iter()
        .position(|arg| matches!(arg.as_str(), "--help" | "-h"))
    else {
        return Ok(false);
    };
    let command = args.first().map(String::as_str);
    let subcommand = args.get(1).filter(|_| position > 1).map(String::as_str);
    match command {
        Some("--help" | "-h") => print_help(),
        Some("new") => print_new_help(),
        Some("doctor") => println!("Diagnose Android prerequisites without installing anything\n\nUsage:\n  arc doctor --android\n\nOptions:\n  --android  Check Rust, SDK, NDK, Java and emulator tooling\n  -h, --help Show help"),
        Some("generate") => match subcommand {
            None => println!("Generate application resources\n\nUsage:\n  arc generate <command>\n\nCommands:\n  resource    Generate an event-sourced CRUD resource\n  aggregate   Alias for resource\n\nOptions:\n  --ffi  Generate native resource adapters in an Arc Android project; incompatible with --api, --ui, --api-auth, --ui-auth, and --roles\n  -h, --help  Show help"),
            Some(kind @ ("resource" | "aggregate")) => print_generate_help(kind),
            Some(kind) => return Err(format!("unknown generator `{kind}`; expected `resource` or `aggregate`")),
        },
        Some("plugin") => match subcommand {
            None => println!("Manage Arc plugins\n\nUsage:\n  arc plugin <command>\n\nCommands:\n  add              Install an authentication plugin\n  migrate-auth-ui  Migrate a generated authentication UI host\n\nOptions:\n  -h, --help  Show help"),
            Some("add") => println!("Install an Arc authentication plugin\n\nUsage:\n  arc plugin add <name>\n\nPlugins:\n  auth-db-session  Browser sessions, administration and RBAC\n  auth-db-jwt      JWT APIs and RBAC\n  auth-db          Database identity provider\n  auth-session     Browser session authentication\n  auth-admin       User and role administration\n  auth-jwt         JWT API authentication\n  auth-rbac        Role-based access control\n\nOptions:\n  -h, --help  Show help\n\nRun from the root of an application created by `arc new`."),
            Some("migrate-auth-ui") => println!("Migrate an untouched generated UI to the authentication UI host\n\nUsage:\n  arc plugin migrate-auth-ui\n\nOptions:\n  -h, --help  Show help\n\nCustomized UI shells are refused with manual migration instructions."),
            Some(operation) => return Err(format!("unknown plugin command `{operation}`")),
        },
        Some("--version" | "-V") => print_help(),
        Some(command) => return Err(format!("unknown command `{command}`; run `arc --help` for usage")),
        None => print_help(),
    }
    Ok(true)
}

fn print_help() {
    println!(
        "Arc application generator

Usage:
  arc new <name> [--ui] [--api] [--android] [--arc-path PATH] [--no-git]
  arc plugin add <name>
  arc plugin migrate-auth-ui
  arc doctor --android
  arc generate resource <name> [--api] [--ui] [--api-auth jwt] [--ui-auth session] [--roles admin,user]
  arc generate aggregate <name> [--api] [--ui]

Options:
  --android  Create an FFI-only Compose Android application
  --arc-path PATH  Build against a local Arc checkout instead of crates.io (or ARC_PATH)
  --ui       Add Tera views and browser assets
  --no-git   Do not initialize a Git repository
  -h, --help Show help
  -V, --version Show version"
    );
}

fn print_generate_help(kind: &str) {
    println!(
        "Generate and register an event-sourced aggregate resource

Usage:
  arc generate {kind} <name> [--ffi] [--api] [--ui] [--api-auth jwt|none] [--ui-auth session|none] [--roles LIST]

Options:
  --ffi  Generate native resource adapters in an Arc Android project
          Incompatible with --api, --ui, --api-auth, --ui-auth, and --roles
  --api  Add public JSON CRUD endpoints
  --ui   Add browser CRUD pages (session-protected when auth-session is installed)
  --api-auth jwt      Explicitly protect this API resource
  --ui-auth session   Explicitly protect this browser resource
  --roles LIST        Require any listed role (requires auth)
  -h, --help          Show help

Run this command from the root of an application created by `arc new`."
    );
}

fn print_new_help() {
    println!(
        "Create a self-contained Arc application

Usage:
  arc new <name> [--ui] [--api] [--android] [--arc-path PATH] [--no-git]

Options:
  --android  Create an FFI-only Compose Android application
  --arc-path PATH  Build against a local Arc checkout instead of crates.io (or ARC_PATH)
  --ui       Add Tera views and browser assets
  --api      Add a public JSON endpoint
  --no-git   Do not initialize a Git repository
  -h, --help Show help"
    );
}

#[cfg(test)]
mod android_options_tests {
    use super::*;
    fn invoke(args: &[&str]) -> Result<(), String> {
        run(args.iter().map(|arg| (*arg).into()))
    }
    #[test]
    fn android_options_fail_before_writes() {
        for option in ["--api", "--ui"] {
            assert_eq!(
                invoke(&["new", "untouched", "--android", option]).unwrap_err(),
                "--android cannot be combined with --ui or --api"
            );
        }
        assert_eq!(
            invoke(&["new", "untouched", "--arc-path", "."]).unwrap_err(),
            "--arc-path requires --android"
        );
        for args in [
            vec!["new", "untouched", "--android", "--arc-path"],
            vec!["new", "untouched", "--arc-path", "--no-git"],
        ] {
            assert_eq!(
                invoke(&args).unwrap_err(),
                "--arc-path requires a directory"
            );
        }
        for kind in ["resource", "aggregate"] {
            for option in ["--api", "--ui", "--api-auth", "--ui-auth", "--roles"] {
                let mut args = vec!["generate", kind, "Note", "--ffi", option];
                if matches!(option, "--api-auth" | "--ui-auth" | "--roles") {
                    assert_eq!(invoke(&args).unwrap_err(), "--ffi cannot be combined with --api, --ui, --api-auth, --ui-auth, or --roles");
                    args.push("none");
                }
                assert_eq!(
                    invoke(&args).unwrap_err(),
                    "--ffi cannot be combined with --api, --ui, --api-auth, --ui-auth, or --roles"
                );
            }
            assert_eq!(
                invoke(&["generate", kind, "Note", "--ffi"]).unwrap_err(),
                "--ffi requires an Arc Android project; run arc new <name> --android"
            );
        }
    }
    #[test]
    fn help_precedes_new_android_validation_and_execution() {
        for flag in ["--help", "-h"] {
            for mut args in [
                vec!["new", "--android", "--arc-path"],
                vec!["new", "No-Writes", "--android", "--ui"],
                vec!["generate", "resource", "Note", "--ffi", "--roles"],
                vec!["generate", "aggregate", "Note", "--ffi"],
                vec!["doctor", "--android"],
            ] {
                for index in 1..=args.len() {
                    let mut candidate = args.clone();
                    candidate.insert(index, flag);
                    assert!(invoke(&candidate).is_ok(), "{candidate:?}");
                }
                args.push(flag);
                assert!(invoke(&args).is_ok());
            }
        }
    }
}
