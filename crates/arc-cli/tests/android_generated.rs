use std::{fs, path::Path, process::Command};

fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(
        root: &Path,
        dir: &Path,
        files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn run(cli: &Path, dir: &Path, args: &[&str]) {
    let output = Command::new(cli)
        .args(args)
        .current_dir(dir)
        .env(
            "ARC_PATH",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_two_resource_project_passes_host_tests() {
    let base = std::env::temp_dir().join(format!(
        "arc-android-host-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&base).unwrap();
    let cli_target = base.join("cli-target");
    let build = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "-p",
            "arc-web-cli",
            "--bin",
            "arc",
            "--target-dir",
        ])
        .arg(&cli_target)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_BUILD_JOBS", "4")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "CLI build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let cli = cli_target
        .join("debug")
        .join(format!("arc{}", std::env::consts::EXE_SUFFIX));
    run(&cli, &base, &["new", "sample", "--android"]);
    let app = base.join("sample");
    run(&cli, &app, &["generate", "resource", "Note", "--ffi"]);
    run(&cli, &app, &["generate", "resource", "Tag", "--ffi"]);
    let before = snapshot(&app);
    run(&cli, &app, &["generate", "resource", "Note", "--ffi"]);
    assert_eq!(before, snapshot(&app));
    // Exercise a database created before the second resource existed, using the
    // generated migrations and ledger rather than a mock schema.
    fs::write(app.join("rust/tests/two_resources.rs"), r#"
use sample_core::{ffi::NativeCore, schema};
use sqlx::{Connection, Executor};
#[tokio::test]
async fn existing_database_gains_second_resource_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite").to_string_lossy().into_owned();
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true);
    let mut db = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
    db.execute("CREATE TABLE arc_schema_migrations (version TEXT PRIMARY KEY)").await.unwrap();
    let mut migrations: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap().map(|e| e.unwrap().path()).collect();
    migrations.sort();
    for migration in migrations.iter().filter(|p| !p.file_name().unwrap().to_string_lossy().contains("tag")) {
        let sql = std::fs::read_to_string(migration.join("up.sql")).unwrap();
        db.execute(sql.as_str()).await.unwrap();
        sqlx::query("INSERT INTO arc_schema_migrations(version) VALUES (?)").bind(migration.file_name().unwrap().to_str().unwrap()).execute(&mut db).await.unwrap();
    }
    db.execute("CREATE TABLE retained_data (value TEXT NOT NULL)").await.unwrap();
    db.execute("INSERT INTO retained_data VALUES ('preserved')").await.unwrap();
    db.close().await.unwrap();
    let core = NativeCore::open(path.clone()).await.unwrap();
    let note = core.create_note("note".into()).await.unwrap();
    let tag = core.create_tag("tag".into()).await.unwrap();
    schema::initialize(&path).await.unwrap();
    schema::initialize(&path).await.unwrap();
    assert_eq!(core.get_note(note).await.unwrap().name, "note");
    assert_eq!(core.get_tag(tag).await.unwrap().name, "tag");
    let mut db = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
    let retained: String = sqlx::query_scalar("SELECT value FROM retained_data").fetch_one(&mut db).await.unwrap();
    assert_eq!(retained, "preserved");
    db.close().await.unwrap();
}
#[tokio::test]
async fn failed_migration_preserves_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("failure.sqlite").to_string_lossy().into_owned();
    schema::initialize(&path).await.unwrap();
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&path);
    let mut db = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
    db.execute("CREATE TABLE retained_data (value TEXT NOT NULL)").await.unwrap();
    db.execute("INSERT INTO retained_data VALUES ('preserved')").await.unwrap();
    db.execute("DELETE FROM arc_schema_migrations WHERE version LIKE '%tags_view'").await.unwrap();
    db.close().await.unwrap();
    assert!(schema::initialize(&path).await.is_err());
    let mut db = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
    let retained: String = sqlx::query_scalar("SELECT value FROM retained_data").fetch_one(&mut db).await.unwrap();
    assert_eq!(retained, "preserved");
    db.close().await.unwrap();
}
"#).unwrap();
    let target = std::env::var_os("ARC_ANDROID_HOST_TARGET")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| base.join("target"));
    let output = Command::new(env!("CARGO"))
        .args(["test", "--manifest-path"])
        .arg(app.join("rust/Cargo.toml"))
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_JOBS", "4")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "generated cargo test failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(base).unwrap();
}
