//! App-owned schema-on-open. Called inside a typed CoreRegistry initializer.
use arc_ffi::FfiError;
use sqlx::{Connection, Executor};
use std::time::Duration;

const MIGRATIONS: &[(&str, &str)] = &[
    ("00000000000000_arc_base", include_str!("../migrations/00000000000000_arc_base/up.sql")),
    // arc:schema-migrations
];

pub async fn initialize(path: &str) -> Result<(), FfiError> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path).create_if_missing(true).busy_timeout(Duration::from_millis(5000));
    let mut connection = sqlx::SqliteConnection::connect_with(&options).await
        .map_err(|_| FfiError::SchemaInitializationFailed)?;
    let result = apply(&mut connection).await;
    // An explicit close releases the temporary schema connection on every path.
    let closed = connection.close().await.map_err(|_| FfiError::SchemaInitializationFailed);
    result.and(closed)
}

async fn apply(connection: &mut sqlx::SqliteConnection) -> Result<(), FfiError> {
    connection.execute("BEGIN IMMEDIATE").await.map_err(|_| FfiError::SchemaInitializationFailed)?;
    let result = async {
        connection.execute("CREATE TABLE IF NOT EXISTS arc_schema_migrations (version TEXT PRIMARY KEY)").await?;
        for (version, sql) in MIGRATIONS {
            let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM arc_schema_migrations WHERE version = ?")
                .bind(version).fetch_one(&mut *connection).await?;
            if applied == 0 {
                connection.execute(*sql).await?;
                sqlx::query("INSERT INTO arc_schema_migrations (version) VALUES (?)")
                    .bind(version).execute(&mut *connection).await?;
            }
        }
        connection.execute("COMMIT").await?;
        Ok::<(), sqlx::Error>(())
    }.await;
    if result.is_err() { let _ = connection.execute("ROLLBACK").await; }
    result.map_err(|_| FfiError::SchemaInitializationFailed)
}
