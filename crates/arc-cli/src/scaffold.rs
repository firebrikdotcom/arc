use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ARC_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct NewProject {
    pub name: String,
    pub destination: PathBuf,
    pub ui: bool,
    pub api: bool,
    pub git: bool,
}

struct TemplateFile {
    path: &'static str,
    contents: &'static str,
    ui_only: bool,
}

const FILES: &[TemplateFile] = &[
    TemplateFile {
        path: "Cargo.toml",
        contents: include_str!("../templates/Cargo.toml.tpl"),
        ui_only: false,
    },
    TemplateFile {
        path: "Makefile",
        contents: include_str!("../templates/Makefile"),
        ui_only: false,
    },
    TemplateFile {
        path: ".env.example",
        contents: include_str!("../templates/env.example.tpl"),
        ui_only: false,
    },
    TemplateFile {
        path: ".gitignore",
        contents: include_str!("../templates/gitignore.tpl"),
        ui_only: false,
    },
    TemplateFile {
        path: "README.md",
        contents: include_str!("../templates/README.md.tpl"),
        ui_only: false,
    },
    TemplateFile {
        path: "src/main.rs",
        contents: include_str!("../templates/src/main.rs"),
        ui_only: false,
    },
    TemplateFile {
        path: "src/domain.rs",
        contents: include_str!("../templates/src/domain.rs"),
        ui_only: false,
    },
    TemplateFile {
        path: "src/routes.rs",
        contents: include_str!("../templates/src/routes.rs.tpl"),
        ui_only: false,
    },
    TemplateFile {
        path: "src/ui.rs",
        contents: include_str!("../templates/src/ui.rs"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/home.html",
        contents: include_str!("../templates/resources/views/home.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/layouts/public.html",
        contents: include_str!("../templates/resources/views/layouts/public.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/layouts/admin.html",
        contents: include_str!("../templates/resources/views/layouts/admin.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/components/ui.html",
        contents: include_str!("../templates/resources/views/components/ui.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/admin/dashboard.html",
        contents: include_str!("../templates/resources/views/admin/dashboard.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/errors/403.html",
        contents: include_str!("../templates/resources/views/errors/403.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/errors/404.html",
        contents: include_str!("../templates/resources/views/errors/404.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "resources/views/errors/500.html",
        contents: include_str!("../templates/resources/views/errors/500.html"),
        ui_only: true,
    },
    TemplateFile {
        path: "public/styles.css",
        contents: include_str!("../templates/public/styles.css"),
        ui_only: true,
    },
    TemplateFile {
        path: "public/workbench.js",
        contents: include_str!("../templates/public/workbench.js"),
        ui_only: true,
    },
    TemplateFile {
        path: "migrations/00000000000000_arc_base/up.sql",
        contents: include_str!("../templates/migrations/arc_base/up.sql"),
        ui_only: false,
    },
    TemplateFile {
        path: "migrations/00000000000000_arc_base/down.sql",
        contents: include_str!("../templates/migrations/arc_base/down.sql"),
        ui_only: false,
    },
];

pub fn create_project(project: &NewProject) -> Result<PathBuf, String> {
    validate_name(&project.name)?;
    let root = project.destination.join(&project.name);
    if root.exists() {
        return Err(format!(
            "destination `{}` already exists; Arc will not overwrite it",
            root.display()
        ));
    }

    fs::create_dir_all(&root)
        .map_err(|error| format!("could not create `{}`: {error}", root.display()))?;

    let result = write_project(project, &root);
    if result.is_err() {
        let _ = fs::remove_dir_all(&root);
    }
    result?;

    if project.git {
        initialize_git(&root)?;
    }

    Ok(root)
}

fn write_project(project: &NewProject, root: &Path) -> Result<(), String> {
    for file in FILES {
        if file.ui_only && !project.ui {
            continue;
        }
        let path = root.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create `{}`: {error}", parent.display()))?;
        }
        let contents = render(file.contents, project);
        fs::write(&path, contents)
            .map_err(|error| format!("could not write `{}`: {error}", path.display()))?;
    }
    Ok(())
}

fn render(template: &str, project: &NewProject) -> String {
    let _api_requested = project.api;
    let rendered = template
        .replace("{{project-name}}", &project.name)
        .replace("{{crate-name}}", &project.name.replace('-', "_"))
        .replace("{{arc-version}}", ARC_VERSION)
        .replace(
            "// {{ui-module}}\n",
            if project.ui { "mod ui;\n" } else { "" },
        )
        .replace(
            "        // {{ui-host-registration}}",
            if project.ui { "        // arc:ui-host-registration\n        .register_ui_host(ui::host())\n        .register_ui(ui::contribution())" } else { "" },
        )
        .replace(
            "{{ui-routes}}",
            if project.ui {
                ".configure(crate::ui::config).service(actix_files::Files::new(\"/public\", \"public\"))"
            } else {
                ""
            },
        );

    match std::env::var("ARC_CLI_TEST_LOCAL_ROOT") {
        Ok(root) => rendered
            .replace(
                &format!("arc-core = \"{ARC_VERSION}\""),
                &format!("arc-core = {{ path = \"{root}/crates/arc-core\" }}"),
            )
            .replace(
                &format!("arc-web = \"{ARC_VERSION}\""),
                &format!("arc-web = {{ path = \"{root}/crates/arc-web\" }}"),
            ),
        Err(_) => rendered,
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid_first = chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    let valid_rest = chars.all(|character| character.is_ascii_alphanumeric() || character == '-');
    if !valid_first || !valid_rest {
        return Err(
            "project name must start with an ASCII letter and contain only letters, numbers, or hyphens"
                .to_string(),
        );
    }
    Ok(())
}

/// Variables that make Git pick a repository other than the working directory, or inject
/// per-invocation config from a parent `git -c` / hook. User and system config
/// (`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM`) and `GIT_CEILING_DIRECTORIES` are kept on purpose:
/// they cannot redirect `git init` to another repository.
const GIT_REPOSITORY_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Builds a `git` command that cannot be redirected to a caller's repository.
/// `inherited` simulates variables of the parent environment (used by tests).
fn isolated_git_command(root: &Path, inherited: &[(&str, &Path)]) -> Command {
    let mut command = Command::new("git");
    for (name, value) in inherited {
        command.env(name, value);
    }
    for name in GIT_REPOSITORY_ENV {
        command.env_remove(name);
    }
    // `GIT_CONFIG_COUNT` pairs with numbered GIT_CONFIG_KEY_n / GIT_CONFIG_VALUE_n entries.
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str() {
            if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
                command.env_remove(name);
            }
        }
    }
    command.current_dir(root);
    command
}

pub(crate) fn initialize_git(root: &Path) -> Result<(), String> {
    git_init(root, &[])
}

fn git_init(root: &Path, inherited: &[(&str, &Path)]) -> Result<(), String> {
    let status = isolated_git_command(root, inherited)
        .args(["init", "--quiet"])
        .status()
        .map_err(|error| format!("could not run `git init`: {error}; retry with --no-git"))?;
    if !status.success() {
        return Err("`git init` failed; retry with --no-git".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("arc-cli-test-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn git_init_ignores_inherited_repository_environment() {
        struct Cleanup(Vec<PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
        let outer = temp_root();
        let project = temp_root().with_extension("project");
        let _cleanup = Cleanup(vec![outer.clone(), project.clone()]);
        fs::create_dir_all(&outer).unwrap();
        fs::create_dir_all(&project).unwrap();
        assert!(isolated_git_command(&outer, &[])
            .args(["init", "--quiet"])
            .status()
            .unwrap()
            .success());
        let outer_git = outer.join(".git");
        git_init(&project, &[("GIT_DIR", outer_git.as_path())]).unwrap();
        assert!(project.join(".git").is_dir());
        let bare = isolated_git_command(&outer, &[])
            .args(["config", "--get", "core.bare"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&bare.stdout).trim(), "false");
    }

    #[test]
    fn creates_minimal_project_without_ui_files() {
        let destination = temp_root();
        let project = NewProject {
            api: false,
            name: "hello-arc".to_string(),
            destination: destination.clone(),
            ui: false,
            git: false,
        };
        let root = create_project(&project).unwrap();
        assert!(root.join("src/main.rs").is_file());
        assert!(root
            .join("migrations/00000000000000_arc_base/up.sql")
            .is_file());
        assert!(!root.join("src/ui.rs").exists());
        assert!(fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .contains(&format!("arc-web = \"{ARC_VERSION}\"")));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn ui_flag_adds_web_files_and_routes() {
        let destination = temp_root();
        let project = NewProject {
            api: false,
            name: "hello-ui".to_string(),
            destination: destination.clone(),
            ui: true,
            git: false,
        };
        let root = create_project(&project).unwrap();
        assert!(root.join("src/ui.rs").is_file());
        assert!(root.join("resources/views/home.html").is_file());
        assert!(root.join("resources/views/layouts/admin.html").is_file());
        assert!(root.join("resources/views/errors/500.html").is_file());
        assert!(root.join("public/workbench.js").is_file());
        assert!(!root.join("resources/views/auth/signin.html").exists());
        assert!(!root.join("src/domain/user/aggregate.rs").exists());
        assert!(!root.join("resources/views/admin/settings.html").exists());
        let routes = fs::read_to_string(root.join("src/routes.rs")).unwrap();
        assert!(routes.contains("crate::ui::config"));
        assert!(routes.contains("fn api_config(_cfg: &mut web::ServiceConfig)"));
        assert!(!routes.contains("JwtMiddleware"));
        let ui_source = fs::read_to_string(root.join("src/ui.rs")).unwrap();
        assert!(ui_source.contains("pub fn host()->UiHost"));
        assert!(ui_source.contains("HOST_TEMPLATES"));
        let mut tera = tera::Tera::default();
        tera.add_raw_templates([
            (
                "home.html",
                fs::read_to_string(root.join("resources/views/home.html")).unwrap(),
            ),
            (
                "layouts/public.html",
                fs::read_to_string(root.join("resources/views/layouts/public.html")).unwrap(),
            ),
            (
                "layouts/admin.html",
                fs::read_to_string(root.join("resources/views/layouts/admin.html")).unwrap(),
            ),
            (
                "components/ui.html",
                fs::read_to_string(root.join("resources/views/components/ui.html")).unwrap(),
            ),
            (
                "admin/dashboard.html",
                fs::read_to_string(root.join("resources/views/admin/dashboard.html")).unwrap(),
            ),
        ])
        .expect("generated templates must resolve inheritance regardless of declaration order");
        assert!(
            !fs::read_to_string(root.join("resources/views/layouts/admin.html"))
                .unwrap()
                .contains("/admin/settings")
        );
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn refuses_to_overwrite_existing_destination() {
        let destination = temp_root();
        fs::create_dir_all(destination.join("taken")).unwrap();
        let project = NewProject {
            api: false,
            name: "taken".to_string(),
            destination: destination.clone(),
            ui: false,
            git: false,
        };
        assert!(create_project(&project)
            .unwrap_err()
            .contains("will not overwrite"));
        fs::remove_dir_all(destination).unwrap();
    }
}
