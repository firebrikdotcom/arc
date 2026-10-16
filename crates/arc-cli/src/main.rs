use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

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
                .ok_or_else(|| "usage: arc new <name> [--ui] [--api] [--no-git]".to_string())?;
            let mut ui = false;
            let mut api = false;
            let mut git = true;

            for argument in args {
                match argument.as_str() {
                    "--ui" => ui = true,
                    "--api" => api = true,
                    "--no-git" => git = false,
                    unknown => return Err(format!("unknown option `{unknown}`")),
                }
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
                "usage: arc generate resource <name> (alias: aggregate)".to_string()
            })?;
            if kind != "resource" && kind != "aggregate" {
                return Err(format!(
                    "unknown generator `{kind}`; expected `resource` or `aggregate`"
                ));
            }
            let name = args.next().ok_or_else(|| {
                "usage: arc generate resource <name> [--api] [--ui] (alias: aggregate)".to_string()
            })?;
            if name.starts_with('-') {
                return Err("resource name is required; run `arc generate resource --help`".into());
            }
            let mut api = false;
            let mut ui = false;
            let mut api_auth = None;
            let mut ui_auth = None;
            let mut roles = Vec::new();
            while let Some(argument) = args.next() {
                match argument.as_str() {
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
            if let Some(api_path) = created.api_path {
                println!("Run `make migrate`, then use the CRUD API at {api_path}.");
            } else {
                println!(
                    "Run `make migrate` and add routes that dispatch {}Command.",
                    created.type_name
                );
            }
            Ok(())
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
        Some("generate") => match subcommand {
            None => println!("Generate application resources\n\nUsage:\n  arc generate <command>\n\nCommands:\n  resource    Generate an event-sourced CRUD resource\n  aggregate   Alias for resource\n\nOptions:\n  -h, --help  Show help"),
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
  arc new <name> [--ui] [--api] [--no-git]
  arc plugin add <name>
  arc plugin migrate-auth-ui
  arc generate resource <name> [--api] [--ui] [--api-auth jwt] [--ui-auth session] [--roles admin,user]
  arc generate aggregate <name> [--api] [--ui]

Options:
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
  arc generate {kind} <name> [--api] [--ui] [--api-auth jwt|none] [--ui-auth session|none] [--roles LIST]

Options:
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
  arc new <name> [--ui] [--api] [--no-git]

Options:
  --ui       Add Tera views and browser assets
  --api      Add a public JSON endpoint
  --no-git   Do not initialize a Git repository
  -h, --help Show help"
    );
}
