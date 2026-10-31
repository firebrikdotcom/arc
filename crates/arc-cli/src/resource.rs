use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DOMAIN_MARKER: &str = "// arc:domain-modules";
const IMPORT_MARKER: &str = "// arc:resource-imports";
const REGISTRATION_MARKER: &str = "        // arc:resource-registrations";
const API_ROUTE_MARKER: &str = "    // arc:api-routes";

pub struct NewResource {
    pub name: String,
    pub root: PathBuf,
    pub api: bool,
    pub ui: bool,
    pub api_auth: Option<String>,
    pub ui_auth: Option<String>,
    pub roles: Vec<String>,
}

#[derive(Debug)]
pub struct CreatedResource {
    pub type_name: String,
    pub files: Vec<PathBuf>,
    pub api_path: Option<String>,
}

struct Names {
    type_name: String,
    module: String,
    constant: String,
    view: String,
}

struct GeneratedFile {
    relative: PathBuf,
    contents: String,
}

pub fn create_resource(resource: &NewResource) -> Result<CreatedResource, String> {
    let names = parse_name(&resource.name)?;
    if let Some(manifest) = crate::android::read_manifest(&resource.root)? {
        return create_android_resource(resource, &names, manifest);
    }
    validate_project(&resource.root)?;
    validate_auth(resource)?;

    let domain_path = resource.root.join("src/domain.rs");
    let main_path = resource.root.join("src/main.rs");
    let domain = read_domain(&domain_path)?;
    let main = read_main(&main_path)?;

    let migration_version = next_migration_version(&resource.root.join("migrations"))?;
    if resource.ui && !resource.root.join("src/ui.rs").is_file() {
        return Err("resource UI requires an application created with `arc new --ui`".to_string());
    }
    let manifest = fs::read_to_string(resource.root.join("Cargo.toml"))
        .map_err(|error| format!("could not read application manifest: {error}"))?;
    let effective_ui_auth = if resource.ui && manifest.contains("arc-auth-session") {
        Some("session")
    } else {
        resource.ui_auth.as_deref()
    };
    let files = generated_files(
        &names,
        migration_version,
        resource.api,
        resource.ui,
        resource.api_auth.as_deref(),
        effective_ui_auth,
        &resource.roles,
    );
    let collisions = files
        .iter()
        .map(|file| resource.root.join(&file.relative))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        return Err(format!(
            "Arc will not overwrite existing resource files: {}",
            collisions
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let module_line = format!("pub mod {};", names.module);
    if domain.lines().any(|line| line.trim() == module_line) {
        return Err(format!(
            "resource module `{}` is already registered",
            names.module
        ));
    }

    let import = format!(
        "use crate::domain::{}::aggregate::{}Aggregate;\nuse crate::domain::{}::projector::{{{}Projector, {}_VIEW}};",
        names.module, names.type_name, names.module, names.type_name, names.constant
    );
    let registration = format!(
        "        .register_aggregate::<{}Aggregate>()\n        .register_projector({}Projector, {}_VIEW){}",
        names.type_name, names.type_name, names.constant,
        if resource.ui { format!("\n        .register_ui(crate::domain::{}::ui::contribution())", names.module) } else { String::new() }
    );
    let updated_domain = domain.replace(DOMAIN_MARKER, &format!("{DOMAIN_MARKER}\n{module_line}"));
    let updated_main = main
        .replace(IMPORT_MARKER, &format!("{IMPORT_MARKER}\n{import}"))
        .replace(
            REGISTRATION_MARKER,
            &format!("{registration}\n{REGISTRATION_MARKER}"),
        );

    for file in &files {
        let path = resource.root.join(&file.relative);
        let parent = path.parent().expect("generated files always have a parent");
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create `{}`: {error}", parent.display()))?;
        fs::write(&path, &file.contents)
            .map_err(|error| format!("could not write `{}`: {error}", path.display()))?;
    }
    fs::write(&domain_path, updated_domain)
        .map_err(|error| format!("could not update `{}`: {error}", domain_path.display()))?;
    fs::write(&main_path, updated_main)
        .map_err(|error| format!("could not update `{}`: {error}", main_path.display()))?;
    if resource.api {
        register_api_route(&resource.root, &names)?;
    }
    if resource.ui {
        register_ui_route(&resource.root, &names)?;
    }
    format_project(&resource.root)?;

    Ok(CreatedResource {
        type_name: names.type_name,
        files: files.into_iter().map(|file| file.relative).collect(),
        api_path: resource.api.then(|| format!("/api/{}", names.view)),
    })
}

fn format_project(root: &Path) -> Result<(), String> {
    let status = Command::new("cargo")
        .arg("fmt")
        .current_dir(root)
        .status()
        .map_err(|e| format!("could not run cargo fmt: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("cargo fmt failed for generated resource".into())
    }
}

fn validate_auth(resource: &NewResource) -> Result<(), String> {
    if resource
        .api_auth
        .as_deref()
        .is_some_and(|v| v != "none" && v != "jwt")
    {
        return Err("--api-auth must be jwt or none".into());
    }
    if resource
        .ui_auth
        .as_deref()
        .is_some_and(|v| v != "none" && v != "session")
    {
        return Err("--ui-auth must be session or none".into());
    }
    if resource.api_auth.as_deref() == Some("jwt") && !resource.api {
        return Err("--api-auth jwt requires --api".into());
    }
    if resource.ui_auth.as_deref() == Some("session") && !resource.ui {
        return Err("--ui-auth session requires --ui".into());
    }
    if !resource.roles.is_empty()
        && resource.api_auth.as_deref() != Some("jwt")
        && resource.ui_auth.as_deref() != Some("session")
    {
        return Err("--roles requires an authenticated resource".into());
    }
    let manifest =
        fs::read_to_string(resource.root.join("Cargo.toml")).map_err(|e| e.to_string())?;
    if resource.api_auth.as_deref() == Some("jwt") && !manifest.contains("arc-auth-jwt") {
        return Err("install JWT capability first: arc plugin add auth-db-jwt".into());
    }
    if resource.ui_auth.as_deref() == Some("session") && !manifest.contains("arc-auth-session") {
        return Err("install session capability first: arc plugin add auth-db-session".into());
    }
    if !resource.roles.is_empty() && !manifest.contains("arc-auth-rbac") {
        return Err("install RBAC capability first: arc plugin add auth-rbac".into());
    }
    Ok(())
}

fn register_ui_route(root: &Path, names: &Names) -> Result<(), String> {
    let path = root.join("src/routes.rs");
    let mut contents = read_file(&path)?;
    let marker = "    // arc:browser-resource-routes";
    if !contents.contains(marker) {
        let anchor = "    cfg.service(health)";
        if !contents.contains(anchor) {
            return Err(format!(
                "`{}` has no route configuration anchor",
                path.display()
            ));
        }
        contents = contents.replace(anchor, &format!("{marker}\n{anchor}"));
    }
    let registration = format!("    crate::domain::{}::ui::config(cfg);", names.module);
    contents = contents.replace(marker, &format!("{registration}\n{marker}"));
    fs::write(&path, contents)
        .map_err(|error| format!("could not update `{}`: {error}", path.display()))?;

    Ok(())
}

fn register_api_route(root: &Path, names: &Names) -> Result<(), String> {
    let path = root.join("src/routes.rs");
    let mut contents = read_file(&path)?;
    if !contents.contains(API_ROUTE_MARKER) {
        let closing = contents.rfind('}').ok_or_else(|| {
            format!(
                "`{}` has no route configuration closing brace",
                path.display()
            )
        })?;
        contents.insert_str(closing, &format!("    {API_ROUTE_MARKER}\n"));
    }
    let registration = format!("    crate::domain::{}::api::config(_cfg);", names.module);
    if contents
        .lines()
        .any(|line| line.trim() == registration.trim())
    {
        return Err(format!(
            "resource API `{}` is already registered",
            names.module
        ));
    }
    contents = contents.replace(
        API_ROUTE_MARKER,
        &format!("{registration}\n{API_ROUTE_MARKER}"),
    );
    fs::write(&path, contents)
        .map_err(|error| format!("could not update `{}`: {error}", path.display()))
}

fn validate_project(root: &Path) -> Result<(), String> {
    for relative in ["Cargo.toml", "src/main.rs", "src/domain.rs", "migrations"] {
        let path = root.join(relative);
        if !path.exists() {
            return Err(format!(
                "`{}` is not an Arc application root (missing `{relative}`)",
                root.display()
            ));
        }
    }
    Ok(())
}

fn read_file(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))
}

fn read_domain(path: &Path) -> Result<String, String> {
    let contents = read_file(path)?;
    if contents.contains(DOMAIN_MARKER) {
        return Ok(contents);
    }
    Ok(format!("{contents}\n\n{DOMAIN_MARKER}\n"))
}

fn read_main(path: &Path) -> Result<String, String> {
    let mut contents = read_file(path)?;
    if !contents.contains(IMPORT_MARKER) {
        let anchor = "use crate::domain::AppAggregate;";
        if !contents.contains(anchor) {
            return Err(format!(
                "`{}` has no Arc resource import marker or generated AppAggregate import",
                path.display()
            ));
        }
        contents = contents.replace(anchor, &format!("{anchor}\n{IMPORT_MARKER}"));
    }
    if !contents.contains(REGISTRATION_MARKER) {
        let anchor = "        .register_aggregate::<AppAggregate>()";
        if !contents.contains(anchor) {
            return Err(format!(
                "`{}` has no Arc resource registration marker or generated AppAggregate registration",
                path.display()
            ));
        }
        contents = contents.replace(anchor, &format!("{anchor}\n{REGISTRATION_MARKER}"));
    }
    Ok(contents)
}

fn parse_name(input: &str) -> Result<Names, String> {
    if input.is_empty()
        || !input
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(
            "resource name must contain only ASCII letters, numbers, hyphens, or underscores"
                .to_string(),
        );
    }

    let mut words = Vec::<String>::new();
    let mut current = String::new();
    for character in input.chars() {
        if character == '-' || character == '_' {
            if !current.is_empty() {
                words.push(current);
                current = String::new();
            }
        } else if character.is_ascii_uppercase()
            && current
                .chars()
                .last()
                .is_some_and(|last| last.is_ascii_lowercase())
        {
            words.push(current);
            current = character.to_ascii_lowercase().to_string();
        } else {
            current.push(character.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.is_empty()
        || !words[0]
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return Err("resource name must start with an ASCII letter".to_string());
    }

    let module = words.join("_");
    if is_rust_keyword(&module) {
        return Err(format!(
            "resource name `{input}` is a reserved Rust keyword"
        ));
    }
    let type_name = words
        .iter()
        .map(|word| {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                .unwrap_or_default()
        })
        .collect::<String>();

    Ok(Names {
        type_name,
        constant: pluralize(&module).to_ascii_uppercase(),
        view: pluralize(&module),
        module,
    })
}

fn pluralize(name: &str) -> String {
    if name.ends_with('y')
        && name
            .chars()
            .rev()
            .nth(1)
            .is_some_and(|character| !matches!(character, 'a' | 'e' | 'i' | 'o' | 'u'))
    {
        format!("{}ies", &name[..name.len() - 1])
    } else if name.ends_with('s')
        || name.ends_with('x')
        || name.ends_with('z')
        || name.ends_with("ch")
        || name.ends_with("sh")
    {
        format!("{name}es")
    } else {
        format!("{name}s")
    }
}

fn is_rust_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
    )
}

fn next_migration_version(migrations: &Path) -> Result<String, String> {
    let entries = fs::read_dir(migrations)
        .map_err(|error| format!("could not read `{}`: {error}", migrations.display()))?;
    let max = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.split('_').next()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Ok(format!("{:014}", max + 1))
}

fn generated_files(
    names: &Names,
    migration_version: String,
    api: bool,
    ui: bool,
    api_auth: Option<&str>,
    ui_auth: Option<&str>,
    roles: &[String],
) -> Vec<GeneratedFile> {
    render_files(
        names,
        &migration_version,
        api,
        ui,
        api_auth,
        ui_auth,
        roles,
        Path::new(""),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_files(
    names: &Names,
    migration_version: &str,
    api: bool,
    ui: bool,
    api_auth: Option<&str>,
    ui_auth: Option<&str>,
    roles: &[String],
    prefix: &Path,
) -> Vec<GeneratedFile> {
    let base = prefix.join(format!("src/domain/{}", names.module));
    let migration = prefix.join(format!(
        "migrations/{}_{}_view",
        migration_version, names.view
    ));
    let replacements = |template: &str| {
        let role_wrap = format!(
            ".wrap(RequireRoles::new(&[{}]))",
            roles
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        let rendered = template
            .replace("{{Type}}", &names.type_name)
            .replace("{{module}}", &names.module)
            .replace("{{view}}", &names.view)
            .replace("{{CONSTANT}}", &names.constant)
            .replace("{{api-module}}", if api { "pub mod api;\n" } else { "" })
            .replace("{{ui-module}}", if ui { "pub mod ui;\n" } else { "" });
        let rendered = rendered
            .replace(
                "{{api-auth-import}}",
                if api_auth == Some("jwt") {
                    "use arc_auth_jwt::RequireJwt;\n"
                } else {
                    ""
                },
            )
            .replace(
                "{{api-auth-wrap}}",
                if api_auth == Some("jwt") {
                    ".wrap(RequireJwt)"
                } else {
                    ""
                },
            )
            .replace(
                "{{api-role-import}}",
                if !roles.is_empty() && api_auth == Some("jwt") {
                    "use arc_auth_rbac::RequireRoles;\n"
                } else {
                    ""
                },
            )
            .replace(
                "{{api-role-wrap}}",
                if !roles.is_empty() && api_auth == Some("jwt") {
                    &role_wrap
                } else {
                    ""
                },
            )
            .replace(
                "{{ui-auth-import}}",
                if ui_auth == Some("session") {
                    "use arc_auth_session::RequireSession;\nuse arc_web::http::middlewares::idle_timeout_middleware::IdleTimeoutMiddleware;\n"
                } else {
                    ""
                },
            )
            .replace(
                "{{ui-auth-wrap}}",
                if ui_auth == Some("session") {
                    ".wrap(RequireSession).wrap(IdleTimeoutMiddleware::from_env())"
                } else {
                    ""
                },
            )
            .replace(
                "{{ui-role-import}}",
                if !roles.is_empty() && ui_auth == Some("session") {
                    "use arc_auth_rbac::RequireRoles;\n"
                } else {
                    ""
                },
            )
            .replace(
                "{{ui-role-wrap}}",
                if !roles.is_empty() && ui_auth == Some("session") {
                    &role_wrap
                } else {
                    ""
                },
            )
            .replace("{{required-roles}}", &roles.join(","));
        format!("{}\n", rendered.trim_end())
    };
    let mut templates = vec![
        (
            base.join("mod.rs"),
            include_str!("../templates/resource/mod.rs.tpl"),
        ),
        (
            base.join("commands.rs"),
            include_str!("../templates/resource/commands.rs.tpl"),
        ),
        (
            base.join("events.rs"),
            include_str!("../templates/resource/events.rs.tpl"),
        ),
        (
            base.join("aggregate.rs"),
            include_str!("../templates/resource/aggregate.rs.tpl"),
        ),
        (
            base.join("projector.rs"),
            include_str!("../templates/resource/projector.rs.tpl"),
        ),
        (
            migration.join("up.sql"),
            include_str!("../templates/resource/up.sql.tpl"),
        ),
        (
            migration.join("down.sql"),
            include_str!("../templates/resource/down.sql.tpl"),
        ),
    ];
    if api {
        templates.push((
            base.join("api.rs"),
            include_str!("../templates/resource/api.rs.tpl"),
        ));
    }
    if ui {
        templates.push((
            base.join("ui.rs"),
            include_str!("../templates/resource/ui.rs.tpl"),
        ));
        templates.push((
            PathBuf::from(format!(
                "resources/views/resources/{}/collection.html",
                names.module
            )),
            include_str!("../templates/resource/collection.html.tpl"),
        ));
        templates.push((
            PathBuf::from(format!(
                "resources/views/resources/{}/detail.html",
                names.module
            )),
            include_str!("../templates/resource/detail.html.tpl"),
        ));
        templates.push((
            PathBuf::from(format!(
                "resources/views/resources/{}/form.html",
                names.module
            )),
            include_str!("../templates/resource/form.html.tpl"),
        ));
    }
    templates
        .into_iter()
        .map(|(relative, template)| GeneratedFile {
            relative,
            contents: replacements(template),
        })
        .collect()
}

// Generation-time identities: SHA-256 of each owned file and block as written. They are
// never compared with a fresh render, so later template changes do not affect existing
// registrations. Only these blocks and files are managed; surrounding code is free.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidRegistration {
    name: String,
    module: String,
    type_name: String,
    migration: String,
    files: std::collections::BTreeMap<String, String>,
    blocks: std::collections::BTreeMap<String, String>,
}

type AndroidBlocks = Vec<(String, &'static str, String)>;

// UniFFI's Kotlin oracle uses UpperCamelCase for exported record names.
// Resource names are ASCII; retain the preceding letter's case across digits
// and split acronym runs before the capital that begins a lowercase word.
fn kotlin_record_name(type_name: &str) -> String {
    let input = format!("{type_name}Record");
    let bytes = input.as_bytes();
    let mut starts = vec![0];
    let mut previous_upper = None;
    for index in 0..bytes.len().saturating_sub(1) {
        let current = bytes[index];
        let next = bytes[index + 1];
        let current_upper = if current.is_ascii_uppercase() {
            Some(true)
        } else if current.is_ascii_lowercase() {
            Some(false)
        } else {
            previous_upper
        };
        if current_upper == Some(false) && next.is_ascii_uppercase() {
            starts.push(index + 1);
            previous_upper = None;
        } else if previous_upper == Some(true)
            && current.is_ascii_uppercase()
            && next.is_ascii_lowercase()
        {
            starts.push(index);
            previous_upper = None;
        } else {
            previous_upper = current_upper;
        }
    }
    starts.push(bytes.len());
    starts
        .windows(2)
        .map(|range| {
            let word = &input[range[0]..range[1]];
            word[..1].to_ascii_uppercase() + &word[1..].to_ascii_lowercase()
        })
        .collect()
}

fn android_render(
    template: &str,
    names: &Names,
    manifest: &crate::android::AndroidManifest,
) -> String {
    let lower_type = names.type_name[..1].to_ascii_lowercase() + &names.type_name[1..];
    let plural_method = names
        .view
        .split('_')
        .enumerate()
        .map(|(i, w)| {
            if i == 0 {
                w.to_owned()
            } else {
                w[..1].to_ascii_uppercase() + &w[1..]
            }
        })
        .collect::<String>();
    template
        .replace("{{RecordType}}", &kotlin_record_name(&names.type_name))
        .replace("{{lowerType}}", &lower_type)
        .replace(
            "{{pluralMethod}}",
            &(plural_method[..1].to_ascii_uppercase() + &plural_method[1..]),
        )
        .replace("{{Type}}", &names.type_name)
        .replace("{{module}}", &names.module)
        .replace("{{view}}", &names.view)
        .replace("{{CONSTANT}}", &names.constant)
        .replace("{{application-id}}", &manifest.application_id)
        .replace("{{crate-name}}", &manifest.crate_name)
}

fn android_files(
    names: &Names,
    migration: &str,
    manifest: &crate::android::AndroidManifest,
) -> Vec<GeneratedFile> {
    let mut files = render_files(
        names,
        migration,
        false,
        false,
        None,
        None,
        &[],
        Path::new("rust"),
    );
    files
        .iter_mut()
        .find(|f| f.relative.ends_with("mod.rs"))
        .unwrap()
        .contents
        .push_str("pub mod ffi;\n");
    let package = manifest.application_id.replace('.', "/");
    for (path, template) in [
        (
            format!("rust/src/domain/{}/ffi.rs", names.module),
            include_str!("../templates/android-resource/ffi.rs.tpl"),
        ),
        (
            format!("rust/tests/{}.rs", names.module),
            include_str!("../templates/android-resource/test.rs.tpl"),
        ),
        (
            format!(
                "app/src/main/java/{package}/{}/{}Repository.kt",
                names.module, names.type_name
            ),
            include_str!("../templates/android-resource/Repository.kt.tpl"),
        ),
        (
            format!(
                "app/src/main/java/{package}/{}/{}ViewModel.kt",
                names.module, names.type_name
            ),
            include_str!("../templates/android-resource/ViewModel.kt.tpl"),
        ),
        (
            format!(
                "app/src/main/java/{package}/{}/{}Screen.kt",
                names.module, names.type_name
            ),
            include_str!("../templates/android-resource/Screen.kt.tpl"),
        ),
        (
            format!(
                "app/src/test/java/{package}/{}/{}ViewModelTest.kt",
                names.module, names.type_name
            ),
            include_str!("../templates/android-resource/ViewModelTest.kt.tpl"),
        ),
        (
            format!(
                "app/src/androidTest/java/{package}/{}/{}FlowTest.kt",
                names.module, names.type_name
            ),
            include_str!("../templates/android-resource/FlowTest.kt.tpl"),
        ),
    ] {
        files.push(GeneratedFile {
            relative: path.into(),
            contents: android_render(template, names, manifest),
        });
    }
    files
}

fn android_blocks(
    names: &Names,
    migration: &str,
    manifest: &crate::android::AndroidManifest,
) -> AndroidBlocks {
    let t = &names.type_name;
    let m = &names.module;
    let v = &names.view;
    let app = &manifest.application_id;
    vec![
        ("rust/src/domain.rs".into(), "domain-modules", format!("pub mod {m};")),
        ("rust/src/ffi.rs".into(), "ffi-imports", format!("use crate::domain::{m}::{{aggregate::{t}Aggregate, ffi::{{{t}Record, {t}ResourceError}}}};")),
        ("rust/src/ffi.rs".into(), "ffi-fields", format!("    pub(crate) {m}: Arc<arc_ffi::FfiCore<{t}Aggregate>>,")),
        ("rust/src/ffi.rs".into(), "ffi-open-initializers", format!("        let {m} = crate::domain::{m}::ffi::open(arc_ffi::CoreRegistry::process()?, path.clone()).await?;")),
        ("rust/src/ffi.rs".into(), "ffi-open-fields", format!("            {m},")),
        ("rust/src/ffi.rs".into(), "ffi-methods", format!(r#"    pub async fn list_{v}(&self) -> Result<Vec<{t}Record>, {t}ResourceError> {{
        crate::domain::{m}::ffi::list(&self.{m}).await
    }}
    pub async fn get_{m}(&self, id: String) -> Result<{t}Record, {t}ResourceError> {{
        crate::domain::{m}::ffi::get(&self.{m}, id).await
    }}
    pub async fn create_{m}(&self, name: String) -> Result<String, {t}ResourceError> {{
        crate::domain::{m}::ffi::create(&self.{m}, name).await
    }}
    pub async fn rename_{m}(&self, id: String, name: String) -> Result<(), {t}ResourceError> {{
        crate::domain::{m}::ffi::rename(&self.{m}, id, name).await
    }}
    pub async fn delete_{m}(&self, id: String) -> Result<(), {t}ResourceError> {{
        crate::domain::{m}::ffi::delete(&self.{m}, id).await
    }}"#)),
        ("rust/src/schema.rs".into(), "schema-migrations", format!("    (\"{migration}_{v}_view\", include_str!(\"../migrations/{migration}_{v}_view/up.sql\")),")),
        (format!("app/src/main/java/{}/ResourceCatalog.kt", app.replace('.', "/")), "catalog-imports", format!("import {app}.{m}.{t}Screen")),
        (format!("app/src/main/java/{}/ResourceCatalog.kt", app.replace('.', "/")), "catalog-entries", format!("    ResourceEntry(\"{m}\", \"{t}\") {{ back -> {t}Screen(onBack = back) }},")),
    ].into_iter().map(|(path, marker, body)| {
        // Whole-line delimiters permit checking exact ownership without touching
        // handwritten text or other resource blocks in a shared file.
        let block = format!("// arc:begin {m} {marker}\n{body}\n// arc:end {m} {marker}\n");
        (path, marker, block)
    }).collect()
}

fn android_conflict(paths: impl IntoIterator<Item = String>) -> String {
    format!(
        "Android resource conflict: {}",
        paths.into_iter().collect::<Vec<_>>().join(", ")
    )
}

fn check_android_marker(source: &str, path: &str, marker: &str) -> Result<(), String> {
    let token = format!("// arc:{marker}");
    if source.lines().filter(|line| line.trim() == token).count() != 1 {
        return Err(android_conflict([format!(
            "{path} (expected exactly one {token})"
        )]));
    }
    Ok(())
}

fn create_android_resource(
    resource: &NewResource,
    names: &Names,
    mut manifest: crate::android::AndroidManifest,
) -> Result<CreatedResource, String> {
    if resource.api
        || resource.ui
        || resource.api_auth.is_some()
        || resource.ui_auth.is_some()
        || !resource.roles.is_empty()
    {
        return Err(
            "--ffi cannot be combined with --api, --ui, --api-auth, --ui-auth, or --roles".into(),
        );
    }
    // Kotlin and UniFFI names occupy namespaces beyond the Rust module namespace.
    if matches!(
        names.module.as_str(),
        "native_core"
            | "resource_catalog"
            | "main_activity"
            | "when"
            | "object"
            | "in"
            | "is"
            | "fun"
            | "val"
            | "var"
            | "by"
            | "constructor"
            | "init"
            | "companion"
            | "interface"
            | "package"
            | "typeof"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "import"
            | "class"
            | "this"
            | "null"
            | "typealias"
    ) || matches!(
        names.type_name.as_str(),
        "String" | "Any" | "Unit" | "Nothing" | "NativeCore"
    ) {
        return Err(android_conflict([format!(
            "reserved native resource name {}",
            names.type_name
        )]));
    }
    let root = &resource.root;
    android_safe_path(root, Path::new(crate::android::MANIFEST_PATH))?;
    let manifest_conflict = || android_conflict([crate::android::MANIFEST_PATH.into()]);
    let legacy = manifest.format_version == 1;
    let mut records = Vec::<AndroidRegistration>::new();
    let mut occupied = std::collections::BTreeSet::new();
    let mut conflicts = Vec::new();
    // Only the manifest structure is validated for every registration. Other resources'
    // files are never written while adding this one, so their customizations are kept.
    for value in &manifest.resources {
        let mut registration: AndroidRegistration =
            serde_json::from_value(value.clone()).map_err(|_| manifest_conflict())?;
        let registered_names = parse_name(&registration.name).map_err(|_| manifest_conflict())?;
        if registered_names.module != registration.module
            || registered_names.type_name != registration.type_name
            || registration.migration.len() != 14
            || !registration.migration.bytes().all(|b| b.is_ascii_digit())
            || !occupied.insert(registration.module.clone())
            || !registration.files.keys().all(|path| android_relative(path))
            || !registration.blocks.keys().all(|key| {
                key.split_once('#')
                    .is_some_and(|(path, marker)| android_relative(path) && !marker.is_empty())
            })
        {
            return Err(manifest_conflict());
        }
        if legacy {
            // Format 1 recorded whole generated bodies; their hashes are the same identity.
            for identity in registration
                .files
                .values_mut()
                .chain(registration.blocks.values_mut())
            {
                *identity = crate::android::content_hash(identity.as_bytes());
            }
        } else if !registration
            .files
            .values()
            .chain(registration.blocks.values())
            .all(|identity| crate::android::is_content_hash(identity))
        {
            return Err(manifest_conflict());
        }
        records.push(registration);
    }
    // Validate the shared seam even for an exact repeat.
    let blocks = android_blocks(names, "00000000000001", &manifest);
    let mut shared = std::collections::BTreeMap::<String, String>::new();
    for (path, marker, _) in &blocks {
        android_safe_path(root, Path::new(path))?;
        let source = read_file(&root.join(path))?;
        check_android_marker(&source, path, marker)?;
        shared.insert(path.clone(), source);
    }
    if let Some(record) = records.iter().find(|r| {
        r.module == names.module
            || r.type_name.eq_ignore_ascii_case(&names.type_name)
            || parse_name(&r.name).is_ok_and(|n| n.view == names.view)
    }) {
        if record.name == resource.name && record.module == names.module {
            // An exact repeat is a no-op only while everything it owns is unchanged.
            for (path, identity) in &record.files {
                android_safe_path(root, Path::new(path))?;
                let target = root.join(path);
                if !fs::symlink_metadata(&target).is_ok_and(|m| m.is_file())
                    || fs::read(&target)
                        .map(|bytes| crate::android::content_hash(&bytes))
                        .ok()
                        .as_ref()
                        != Some(identity)
                {
                    conflicts.push(path.clone());
                }
            }
            for (key, identity) in &record.blocks {
                let (path, marker) = key.split_once('#').unwrap();
                android_safe_path(root, Path::new(path))?;
                let source = fs::read_to_string(root.join(path)).unwrap_or_default();
                if owned_block(&source, &record.module, marker)
                    .map(|block| crate::android::content_hash(block.as_bytes()))
                    .as_ref()
                    != Some(identity)
                {
                    conflicts.push(format!("{path} ({} {marker} block)", record.module));
                }
            }
            if !conflicts.is_empty() {
                return Err(android_conflict(conflicts));
            }
            return Ok(CreatedResource {
                type_name: names.type_name.clone(),
                files: vec![],
                api_path: None,
            });
        }
        return Err(android_conflict([format!(
            "normalized resource {}",
            names.module
        )]));
    }
    let migration = next_migration_version(&root.join("rust/migrations"))?;
    let files = android_files(names, &migration, &manifest);
    let blocks = android_blocks(names, &migration, &manifest);
    let domain_dir = root.join(format!("rust/src/domain/{}", names.module));
    if fs::symlink_metadata(&domain_dir).is_ok() {
        conflicts.push(domain_dir.display().to_string());
    }
    let kotlin_dir = root.join(format!(
        "app/src/main/java/{}/{}",
        manifest.application_id.replace('.', "/"),
        names.module
    ));
    if fs::symlink_metadata(&kotlin_dir).is_ok() {
        conflicts.push(kotlin_dir.display().to_string());
    }
    for file in &files {
        if fs::symlink_metadata(root.join(&file.relative)).is_ok() {
            conflicts.push(file.relative.display().to_string());
        }
    }
    for (path, marker, _) in &blocks {
        let source = shared.get(path).unwrap();
        if source.contains(&format!("// arc:begin {} ", names.module))
            || (marker == &"domain-modules"
                && source
                    .lines()
                    .any(|l| l.trim() == format!("pub mod {};", names.module)))
        {
            conflicts.push(path.clone());
        }
    }
    if !conflicts.is_empty() {
        return Err(android_conflict(conflicts));
    }
    let mut identities = std::collections::BTreeMap::new();
    for (path, marker, block) in blocks {
        let source = shared.get_mut(&path).unwrap();
        let token = format!("// arc:{marker}");
        // Insert at the start of the marker line, preserving its indentation.
        let at = source
            .split_inclusive('\n')
            .scan(0, |offset, line| {
                let at = *offset;
                *offset += line.len();
                Some((at, line))
            })
            .find(|(_, line)| line.trim() == token)
            .unwrap()
            .0;
        source.insert_str(at, &block);
        identities.insert(
            format!("{path}#{marker}"),
            crate::android::content_hash(block.as_bytes()),
        );
    }
    records.push(AndroidRegistration {
        name: resource.name.clone(),
        module: names.module.clone(),
        type_name: names.type_name.clone(),
        migration,
        files: files
            .iter()
            .map(|f| {
                (
                    f.relative.to_string_lossy().into_owned(),
                    crate::android::content_hash(f.contents.as_bytes()),
                )
            })
            .collect(),
        blocks: identities,
    });
    // Format 1 manifests are upgraded in the same atomic write; skeleton files stay
    // without identities (unknown ownership) because none were recorded for them.
    manifest.format_version = crate::android::MANIFEST_FORMAT;
    manifest.resources = records
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let created = files.iter().map(|f| f.relative.clone()).collect();
    let mut writes = files;
    writes.extend(
        shared
            .into_iter()
            .map(|(relative, contents)| GeneratedFile {
                relative: relative.into(),
                contents,
            }),
    );
    writes.push(GeneratedFile {
        relative: crate::android::MANIFEST_PATH.into(),
        contents: serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n",
    });
    atomic_android_write(root, &writes)?;
    Ok(CreatedResource {
        type_name: names.type_name.clone(),
        files: created,
        api_path: None,
    })
}

fn android_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// The whole-line `arc:begin`..`arc:end` block a resource owns in a shared file.
fn owned_block<'a>(source: &'a str, module: &str, marker: &str) -> Option<&'a str> {
    let begin = format!("// arc:begin {module} {marker}\n");
    let end = format!("// arc:end {module} {marker}\n");
    if source.matches(&begin).count() != 1 || source.matches(&end).count() != 1 {
        return None;
    }
    let start = source.find(&begin)?;
    let stop = source.find(&end)? + end.len();
    (start < stop && (start == 0 || source[..start].ends_with('\n'))).then(|| &source[start..stop])
}

fn android_safe_path(root: &Path, relative: &Path) -> Result<(), String> {
    let path = root.join(relative);
    let mut parent = Some(path.as_path());
    while let Some(p) = parent {
        if p == root {
            break;
        }
        if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(android_conflict([p.display().to_string()]));
        }
        parent = p.parent();
    }
    Ok(())
}

/// Stage bytes before changing the tree; rename each file and restore originals on
/// any failure. Only newly created directories are removed during rollback.
fn atomic_android_write(root: &Path, writes: &[GeneratedFile]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let mut backups = Vec::new();
    for file in writes {
        let path = root.join(&file.relative);
        // No symlink traversal, including shared parent directories.
        let mut parent = Some(path.as_path());
        while let Some(p) = parent {
            if p == root {
                break;
            }
            if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(android_conflict([p.display().to_string()]));
            }
            parent = p.parent();
        }
        let original = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("could not stage {}: {e}", path.display())),
        };
        backups.push((path, original));
    }
    let stage = root.join(format!(
        ".arc-resource-stage-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let mut directories = Vec::new();
    let mut installed = 0;
    let result: Result<(), String> = (|| {
        for (index, file) in writes.iter().enumerate() {
            fs::write(stage.join(index.to_string()), &file.contents).map_err(|e| e.to_string())?;
        }
        for (index, (path, _)) in backups.iter().enumerate() {
            let mut missing = Vec::new();
            let mut parent = path.parent().unwrap();
            while !parent.exists() {
                missing.push(parent.to_path_buf());
                parent = parent.parent().unwrap();
            }
            for directory in missing.into_iter().rev() {
                fs::create_dir(&directory).map_err(|e| e.to_string())?;
                directories.push(directory);
            }
            fs::rename(stage.join(index.to_string()), path)
                .map_err(|e| format!("could not install {}: {e}", path.display()))?;
            installed += 1;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut rollback_errors = Vec::new();
        for (path, original) in backups[..installed].iter().rev() {
            let restored = match original {
                Some(bytes) => fs::write(path, bytes),
                None => fs::remove_file(path),
            };
            if let Err(e) = restored {
                rollback_errors.push(format!("{}: {e}", path.display()));
            }
        }
        for directory in directories.iter().rev() {
            if let Err(e) = fs::remove_dir(directory) {
                rollback_errors.push(format!("{}: {e}", directory.display()));
            }
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(format!(
            "{error}; rollback errors: {}",
            rollback_errors.join(", ")
        ));
    }
    fs::remove_dir_all(&stage).map_err(|e| format!("could not remove staging directory: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::{create_project, NewProject};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("arc-resource-test-{}-{nonce}", std::process::id()))
    }

    fn project(destination: &Path) -> PathBuf {
        create_project(&NewProject {
            api: false,
            name: "catalog".to_string(),
            destination: destination.to_path_buf(),
            ui: false,
            git: false,
        })
        .unwrap()
    }

    #[test]
    fn generates_and_registers_complete_resource() {
        let destination = temp_root();
        let root = project(&destination);
        let created = create_resource(&NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "OrderItem".to_string(),
            root: root.clone(),
            api: true,
            ui: false,
        })
        .unwrap();

        assert_eq!(created.type_name, "OrderItem");
        assert_eq!(created.files.len(), 8);
        assert!(root.join("src/domain/order_item/aggregate.rs").is_file());
        assert!(root.join("src/domain/order_item/api.rs").is_file());
        assert!(root
            .join("migrations/00000000000001_order_items_view/up.sql")
            .is_file());
        let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(main.contains("register_aggregate::<OrderItemAggregate>()"));
        assert!(main.contains("register_projector(OrderItemProjector, ORDER_ITEMS_VIEW)"));
        let domain = fs::read_to_string(root.join("src/domain.rs")).unwrap();
        assert!(domain.contains("pub mod order_item;"));
        let routes = fs::read_to_string(root.join("src/routes.rs")).unwrap();
        assert!(routes.contains("crate::domain::order_item::api::config(_cfg);"));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn refuses_to_overwrite_an_existing_resource() {
        let destination = temp_root();
        let root = project(&destination);
        let resource = NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "Product".to_string(),
            root: root.clone(),
            api: false,
            ui: false,
        };
        create_resource(&resource).unwrap();
        let before = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(create_resource(&resource)
            .unwrap_err()
            .contains("will not overwrite"));
        assert_eq!(
            before,
            fs::read_to_string(root.join("src/main.rs")).unwrap()
        );
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn upgrades_a_pre_marker_generated_application() {
        let destination = temp_root();
        let root = project(&destination);
        let domain_path = root.join("src/domain.rs");
        let main_path = root.join("src/main.rs");
        let routes_path = root.join("src/routes.rs");
        fs::write(
            &domain_path,
            fs::read_to_string(&domain_path)
                .unwrap()
                .replace(DOMAIN_MARKER, ""),
        )
        .unwrap();
        fs::write(
            &routes_path,
            fs::read_to_string(&routes_path)
                .unwrap()
                .replace(API_ROUTE_MARKER, ""),
        )
        .unwrap();
        fs::write(
            &main_path,
            fs::read_to_string(&main_path)
                .unwrap()
                .replace(IMPORT_MARKER, "")
                .replace(REGISTRATION_MARKER, ""),
        )
        .unwrap();

        create_resource(&NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "Product".to_string(),
            root: root.clone(),
            api: true,
            ui: false,
        })
        .unwrap();

        let main = fs::read_to_string(main_path).unwrap();
        assert!(main.contains(IMPORT_MARKER));
        assert!(main.contains(REGISTRATION_MARKER));
        assert!(main.contains("register_aggregate::<ProductAggregate>()"));
        assert!(fs::read_to_string(routes_path)
            .unwrap()
            .contains("crate::domain::product::api::config(_cfg);"));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn rejects_invalid_names_and_non_arc_directories() {
        assert!(parse_name("3products").is_err());
        assert!(parse_name("type").is_err());
        assert!(create_resource(&NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "Product".to_string(),
            root: temp_root(),
            api: false,
            ui: false,
        })
        .is_err());
    }

    #[test]
    fn generates_public_resource_ui_by_default() {
        let destination = temp_root();
        let root = create_project(&NewProject {
            api: false,
            name: "catalog-ui".to_string(),
            destination: destination.clone(),
            ui: true,
            git: false,
        })
        .unwrap();
        let created = create_resource(&NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "Product".to_string(),
            root: root.clone(),
            api: true,
            ui: true,
        })
        .unwrap();
        assert_eq!(created.files.len(), 12);
        assert!(root.join("src/domain/product/ui.rs").is_file());
        assert!(root
            .join("resources/views/resources/product/collection.html")
            .is_file());
        let routes = fs::read_to_string(root.join("src/routes.rs")).unwrap();
        assert!(routes.contains("crate::domain::product::ui::config(cfg);"));
        let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(main.contains("register_ui(crate::domain::product::ui::contribution())"));
        let ui = fs::read_to_string(root.join("src/domain/product/ui.rs")).unwrap();
        assert!(ui.contains("CommandBus<ProductAggregate>"));
        assert!(!ui.contains("RequireSession"));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn session_capability_automatically_protects_browser_resources() {
        let destination = temp_root();
        let root = create_project(&NewProject {
            api: false,
            name: "secure-ui".into(),
            destination: destination.clone(),
            ui: true,
            git: false,
        })
        .unwrap();
        crate::plugin::add_plugin(root.clone(), "auth-session").unwrap();
        create_resource(&NewResource {
            api_auth: None,
            ui_auth: None,
            roles: vec![],
            name: "Secret".into(),
            root: root.clone(),
            api: false,
            ui: true,
        })
        .unwrap();
        let ui = fs::read_to_string(root.join("src/domain/secret/ui.rs")).unwrap();
        assert!(ui.contains("wrap(RequireSession)"));
        assert!(ui.contains("IdleTimeoutMiddleware::from_env()"));
        fs::remove_dir_all(destination).unwrap();
    }
    #[test]
    fn kotlin_record_names_follow_uniffi_acronym_boundaries() {
        assert_eq!(kotlin_record_name("Note"), "NoteRecord");
        assert_eq!(kotlin_record_name("NOTE"), "NoteRecord");
        assert_eq!(kotlin_record_name("APIKey"), "ApiKeyRecord");
        assert_eq!(kotlin_record_name("Order2D"), "Order2DRecord");
    }

    fn android_project(destination: &Path) -> PathBuf {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        crate::android::create_project("native-catalog", destination, Some(checkout), false)
            .unwrap()
    }

    fn native_resource(root: &Path, name: &str) -> NewResource {
        NewResource {
            name: name.into(),
            root: root.into(),
            api: false,
            ui: false,
            api_auth: None,
            ui_auth: None,
            roles: vec![],
        }
    }

    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            base: &Path,
            path: &Path,
            result: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    result.insert(path.strip_prefix(base).unwrap().to_path_buf(), vec![]);
                    visit(base, &path, result);
                } else {
                    result.insert(
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut result = std::collections::BTreeMap::new();
        visit(root, root, &mut result);
        result
    }

    #[test]
    fn android_two_resources_repeat_and_shared_domain_renderer() {
        let destination = temp_root();
        let root = android_project(&destination);
        let note = native_resource(&root, "Note");
        create_resource(&note).unwrap();
        let first = tree(&root);
        assert!(create_resource(&note).unwrap().files.is_empty());
        assert_eq!(first, tree(&root));
        create_resource(&native_resource(&root, "Tag")).unwrap();
        let second = tree(&root);
        assert!(create_resource(&note).unwrap().files.is_empty());
        assert_eq!(second, tree(&root));
        let names = parse_name("Note").unwrap();
        for file in generated_files(
            &names,
            "00000000000001".into(),
            false,
            false,
            None,
            None,
            &[],
        ) {
            if file.relative.ends_with("mod.rs") {
                continue;
            }
            assert_eq!(
                file.contents,
                read_file(&root.join("rust").join(file.relative)).unwrap()
            );
        }
        let manifest = crate::android::read_manifest(&root).unwrap().unwrap();
        assert_eq!(manifest.resources.len(), 2);
        let ffi = read_file(&root.join("rust/src/ffi.rs")).unwrap();
        assert_eq!(ffi.matches("pub async fn list_notes").count(), 1);
        assert_eq!(ffi.matches("pub async fn list_tags").count(), 1);
        fs::remove_dir_all(destination).unwrap();
    }

    fn edit_manifest(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
        let path = root.join(crate::android::MANIFEST_PATH);
        let mut manifest: serde_json::Value =
            serde_json::from_str(&read_file(&path).unwrap()).unwrap();
        edit(&mut manifest);
        fs::write(path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    }

    fn append(path: &Path, text: &str) {
        let mut contents = fs::read(path).unwrap();
        contents.extend_from_slice(text.as_bytes());
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn android_conflicts_are_whole_tree_no_ops() {
        let destination = temp_root();
        let root = android_project(&destination);
        create_resource(&native_resource(&root, "Note")).unwrap();
        for (path, expected) in [
            ("rust/src/domain/note/ffi.rs", "rust/src/domain/note/ffi.rs"),
            (
                "rust/src/ffi.rs",
                "rust/src/ffi.rs (note ffi-methods block)",
            ),
            (
                "rust/src/schema.rs",
                "rust/src/schema.rs (note schema-migrations block)",
            ),
        ] {
            let path = root.join(path);
            let original = fs::read(&path).unwrap();
            // Break a file identity or an owned block.
            let mut changed = String::from_utf8(original.clone())
                .unwrap()
                .replace("list_notes", "list_custom")
                .replace("00000000000001_notes_view", "00000000000099_notes_view");
            if changed.as_bytes() == original.as_slice() {
                changed.push_str("// customized\n");
            }
            fs::write(&path, changed).unwrap();
            let before = tree(&root);
            let error = create_resource(&native_resource(&root, "Note")).unwrap_err();
            assert_eq!(error, format!("Android resource conflict: {expected}"));
            assert_eq!(before, tree(&root));
            fs::write(&path, original).unwrap();
        }
        let manifest = root.join(crate::android::MANIFEST_PATH);
        let original = fs::read(&manifest).unwrap();
        append(&manifest, "\ncustomized");
        let before = tree(&root);
        assert!(create_resource(&native_resource(&root, "Tag")).is_err());
        assert!(create_resource(&native_resource(&root, "Note")).is_err());
        assert_eq!(before, tree(&root));
        fs::write(&manifest, original).unwrap();
        let path = root.join("rust/src/domain/note/commands.rs");
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let before = tree(&root);
        let error = create_resource(&native_resource(&root, "Note")).unwrap_err();
        assert!(
            error.contains("rust/src/domain/note/commands.rs"),
            "{error}"
        );
        assert_eq!(before, tree(&root));
        fs::write(path, original).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn android_user_edits_in_one_resource_do_not_block_another() {
        let destination = temp_root();
        let root = android_project(&destination);
        create_resource(&native_resource(&root, "Note")).unwrap();
        let screen =
            root.join("app/src/main/java/com/example/arc/app_native_catalog/note/NoteScreen.kt");
        append(&screen, "// customized screen\n");
        let ffi = root.join("rust/src/ffi.rs");
        let edited = read_file(&ffi)
            .unwrap()
            .replace("pub async fn list_notes", "pub async fn list_custom_notes");
        fs::write(&ffi, &edited).unwrap();
        let customized_screen = fs::read(&screen).unwrap();
        create_resource(&native_resource(&root, "Tag")).unwrap();
        assert_eq!(fs::read(&screen).unwrap(), customized_screen);
        let ffi_source = read_file(&ffi).unwrap();
        assert_eq!(
            ffi_source.matches("pub async fn list_custom_notes").count(),
            1
        );
        assert_eq!(ffi_source.matches("pub async fn list_tags").count(), 1);
        let before = tree(&root);
        let error = create_resource(&native_resource(&root, "Note")).unwrap_err();
        assert!(error.contains("note/NoteScreen.kt"), "{error}");
        assert!(
            error.contains("rust/src/ffi.rs (note ffi-methods block)"),
            "{error}"
        );
        assert_eq!(before, tree(&root));
        assert!(create_resource(&native_resource(&root, "Tag"))
            .unwrap()
            .files
            .is_empty());
        assert_eq!(before, tree(&root));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn android_template_changes_do_not_block_new_resources() {
        let destination = temp_root();
        let root = android_project(&destination);
        create_resource(&native_resource(&root, "Note")).unwrap();
        // Simulate output from an older CLI: on-disk content and its recorded identity
        // differ from what the current templates render.
        let repository =
            "app/src/main/java/com/example/arc/app_native_catalog/note/NoteRepository.kt";
        append(&root.join(repository), "// older template\n");
        let older = crate::android::content_hash(&fs::read(root.join(repository)).unwrap());
        let ffi = root.join("rust/src/ffi.rs");
        let source = read_file(&ffi).unwrap();
        let block = owned_block(&source, "note", "ffi-methods").unwrap();
        let older_block = block.replace("pub async fn list_notes", "pub async fn list_all_notes");
        fs::write(&ffi, source.replace(block, &older_block)).unwrap();
        edit_manifest(&root, |manifest| {
            let note = &mut manifest["resources"][0];
            note["files"][repository] = older.into();
            note["blocks"]["rust/src/ffi.rs#ffi-methods"] =
                crate::android::content_hash(older_block.as_bytes()).into();
        });
        let before = tree(&root);
        assert!(create_resource(&native_resource(&root, "Note"))
            .unwrap()
            .files
            .is_empty());
        assert_eq!(before, tree(&root));
        assert!(!create_resource(&native_resource(&root, "Tag"))
            .unwrap()
            .files
            .is_empty());
        assert!(read_file(&ffi)
            .unwrap()
            .contains("pub async fn list_all_notes"));
        let after = tree(&root);
        assert!(create_resource(&native_resource(&root, "Note"))
            .unwrap()
            .files
            .is_empty());
        assert!(create_resource(&native_resource(&root, "Tag"))
            .unwrap()
            .files
            .is_empty());
        assert_eq!(after, tree(&root));
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn android_reads_format_1_manifests() {
        let destination = temp_root();
        let root = android_project(&destination);
        create_resource(&native_resource(&root, "Note")).unwrap();
        // Format 1 recorded whole bodies and no skeleton identities.
        edit_manifest(&root, |manifest| {
            manifest["format_version"] = 1.into();
            manifest.as_object_mut().unwrap().remove("files");
            let note = &mut manifest["resources"][0];
            for (path, identity) in note["files"].as_object_mut().unwrap() {
                *identity = read_file(&root.join(path)).unwrap().into();
            }
            for (key, identity) in note["blocks"].as_object_mut().unwrap() {
                let (path, marker) = key.split_once('#').unwrap();
                let source = read_file(&root.join(path)).unwrap();
                *identity = owned_block(&source, "note", marker).unwrap().into();
            }
        });
        let before = tree(&root);
        assert!(create_resource(&native_resource(&root, "Note"))
            .unwrap()
            .files
            .is_empty());
        assert_eq!(before, tree(&root));
        let commands = root.join("rust/src/domain/note/commands.rs");
        append(&commands, "// customized\n");
        let customized = fs::read(&commands).unwrap();
        let before = tree(&root);
        let error = create_resource(&native_resource(&root, "Note")).unwrap_err();
        assert_eq!(
            error,
            "Android resource conflict: rust/src/domain/note/commands.rs"
        );
        assert_eq!(before, tree(&root));
        create_resource(&native_resource(&root, "Tag")).unwrap();
        assert_eq!(fs::read(&commands).unwrap(), customized);
        let manifest = crate::android::read_manifest(&root).unwrap().unwrap();
        assert_eq!(manifest.format_version, crate::android::MANIFEST_FORMAT);
        assert!(manifest.files.is_none());
        for registration in &manifest.resources {
            for identity in registration["files"]
                .as_object()
                .unwrap()
                .values()
                .chain(registration["blocks"].as_object().unwrap().values())
            {
                assert!(crate::android::is_content_hash(identity.as_str().unwrap()));
            }
        }
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn android_markers_collisions_and_handwritten_content() {
        let destination = temp_root();
        let root = android_project(&destination);
        let path = root.join("rust/src/ffi.rs");
        let original = read_file(&path).unwrap();
        for replacement in ["", "// arc:ffi-fields\n// arc:ffi-fields"] {
            fs::write(&path, original.replace("// arc:ffi-fields", replacement)).unwrap();
            let before = tree(&root);
            assert!(create_resource(&native_resource(&root, "Note")).is_err());
            assert_eq!(before, tree(&root));
        }
        fs::write(&path, original + "\n// handwritten application code\n").unwrap();
        create_resource(&native_resource(&root, "Note")).unwrap();
        assert!(read_file(&path)
            .unwrap()
            .ends_with("// handwritten application code\n"));
        for name in [
            "note",
            "NOTE",
            "when",
            "class",
            "this",
            "null",
            "typealias",
            "NativeCore",
            "String",
        ] {
            let before = tree(&root);
            assert!(
                create_resource(&native_resource(&root, name)).is_err(),
                "{name}"
            );
            assert_eq!(before, tree(&root));
        }
        create_resource(&native_resource(&root, "Category")).unwrap();
        let before = tree(&root);
        assert!(create_resource(&native_resource(&root, "Categorie")).is_err());
        assert_eq!(before, tree(&root));
        fs::create_dir_all(root.join("rust/src/domain/tag")).unwrap();
        let before = tree(&root);
        assert!(create_resource(&native_resource(&root, "Tag")).is_err());
        assert_eq!(before, tree(&root));
        fs::remove_dir_all(destination).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn android_write_failure_restores_files_and_directories() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root();
        fs::create_dir_all(root.join("locked")).unwrap();
        fs::write(root.join("existing"), "original").unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o555)).unwrap();
        let before = tree(&root);
        let result = atomic_android_write(
            &root,
            &[
                GeneratedFile {
                    relative: "existing".into(),
                    contents: "changed".into(),
                },
                GeneratedFile {
                    relative: "new/child".into(),
                    contents: "new".into(),
                },
                GeneratedFile {
                    relative: "locked/refused".into(),
                    contents: "refused".into(),
                },
            ],
        );
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(before, tree(&root));
        fs::remove_dir_all(root).unwrap();
    }
}
