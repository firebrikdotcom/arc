//! App-owned schema-on-open. Called inside a typed CoreRegistry initializer.
use arc_ffi::FfiError;
use diesel::{connection::SimpleConnection, sql_types::{BigInt, Text}, Connection, QueryResult, RunQueryDsl, SqliteConnection};

const MIGRATIONS: &[(&str, &str)] = &[
    ("00000000000000_arc_base", include_str!("../migrations/00000000000000_arc_base/up.sql")),
    // arc:schema-migrations
];

pub async fn initialize(path: &str) -> Result<(), FfiError> {
    let path = path.to_owned();
    // Diesel blocks, so the schema work runs off the async workers like Arc's SQLite stores.
    // A panic in it is resumed here so the registry reports it as a panic.
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => match runtime.spawn_blocking(move || run(&path)).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Err(FfiError::SchemaInitializationFailed),
        },
        // No Tokio runtime is needed: a plain thread does the work for any other executor.
        Err(_) => {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = sender.send(std::panic::catch_unwind(move || run(&path)));
            });
            match receiver.await {
                Ok(Ok(result)) => result,
                Ok(Err(panic)) => std::panic::resume_unwind(panic),
                Err(_) => Err(FfiError::SchemaInitializationFailed),
            }
        }
    }
}

fn run(path: &str) -> Result<(), FfiError> {
    // Opening creates a missing database file.
    let mut connection = SqliteConnection::establish(path).map_err(|_| FfiError::SchemaInitializationFailed)?;
    // Dropping the temporary schema connection closes it on every path.
    apply(&mut connection).map_err(|_| FfiError::SchemaInitializationFailed)
}

fn apply(connection: &mut SqliteConnection) -> QueryResult<()> {
    connection.batch_execute("PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON")?;
    // BEGIN IMMEDIATE; committed on success and rolled back on any failure.
    connection.immediate_transaction(|connection| {
        connection.batch_execute("CREATE TABLE IF NOT EXISTS arc_schema_migrations (version TEXT PRIMARY KEY)")?;
        for (version, sql) in MIGRATIONS {
            let applied: i64 = diesel::dsl::sql::<BigInt>("SELECT COUNT(*) FROM arc_schema_migrations WHERE version = ")
                .bind::<Text, _>(*version).get_result(connection)?;
            if applied == 0 {
                connection.batch_execute(sql)?;
                diesel::sql_query("INSERT INTO arc_schema_migrations (version) VALUES (?)")
                    .bind::<Text, _>(*version).execute(connection)?;
            }
        }
        Ok(())
    })
}
