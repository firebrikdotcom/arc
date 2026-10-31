//! Real rollback-journal files, independent connections and coordinated starts.
use arc_core::{
    command_bus::CommandContext,
    event::{Event, NewEvent},
    event_store::{EventStore, EventStoreError, VersionCheck},
    read_model_store::{ReadModelStore, Upsert},
};
use arc_es_sqlite::{SqliteEventStore, SqliteReadModelStore};
use diesel::{connection::SimpleConnection, prelude::*};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Barrier;

struct Database(std::path::PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("arc-sqlite-{}.db", uuid::Uuid::new_v4()));
        let db = Self(path);
        db.connect()
            .batch_execute(include_str!("fixtures/schema.sql"))
            .unwrap();
        db
    }
    fn url(&self) -> &str {
        self.0.to_str().unwrap()
    }
    fn connect(&self) -> SqliteConnection {
        SqliteConnection::establish(self.url()).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn event(id: &str, sequence: i64) -> Event {
    let mut event = Event::new(NewEvent {
        aggregate_type: "Counter",
        aggregate_id: id,
        sequence,
        event_type: "Incremented",
        payload: serde_json::json!({"value": sequence}),
    });
    event.audit = CommandContext::system().to_audit().unwrap();
    event
}
fn row(id: &str, version: i64) -> Upsert {
    Upsert::new(
        "users_view",
        id,
        serde_json::json!({"id": id, "version": version}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_distinct_streams_and_projection_writes_succeed() {
    let db = Database::new();
    let events = SqliteEventStore::new_with_integrity_key(db.url(), vec![7; 32], "test")
        .await
        .unwrap();
    let views = SqliteReadModelStore::new(db.url()).await.unwrap();
    let start = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for worker in 0..8 {
        let (events, views, start) = (events.clone(), views.clone(), start.clone());
        workers.push(tokio::spawn(async move {
            start.wait().await;
            for n in 0..20 {
                let id = format!("{worker}-{n}");
                events
                    .append(&id, VersionCheck::New, vec![event(&id, 1)])
                    .await
                    .unwrap();
                views.upsert(row(&id, 1)).await.unwrap();
                assert_eq!(events.load(&id).await.unwrap().len(), 1);
                assert_eq!(
                    views.get("users_view", &id).await.unwrap().unwrap()["version"],
                    1
                );
            }
        }));
    }
    for worker in workers {
        worker.await.unwrap();
    }
    assert_eq!(views.list("users_view").await.unwrap().len(), 160);
    #[derive(QueryableByName)]
    struct Mode {
        #[diesel(sql_type = diesel::sql_types::Text)]
        journal_mode: String,
    }
    assert_eq!(
        diesel::sql_query("PRAGMA journal_mode")
            .get_result::<Mode>(&mut db.connect())
            .unwrap()
            .journal_mode,
        "delete"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_projection_writes_succeed() {
    let db = Database::new();
    let views = SqliteReadModelStore::new(db.url()).await.unwrap();
    let start = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for worker in 0..8 {
        let (views, start) = (views.clone(), start.clone());
        workers.push(tokio::spawn(async move {
            start.wait().await;
            for version in 1..=20 {
                views
                    .upsert(row(&worker.to_string(), version))
                    .await
                    .unwrap();
            }
        }));
    }
    for worker in workers {
        worker.await.unwrap();
    }
    let rows = views.list("users_view").await.unwrap();
    assert_eq!(rows.len(), 8);
    assert!(rows.iter().all(|row| row["version"] == 20));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_stream_contenders_return_typed_conflicts() {
    let db = Database::new();
    let store = SqliteEventStore::new(db.url()).await.unwrap();
    store
        .append("same", VersionCheck::New, vec![event("same", 1)])
        .await
        .unwrap();
    let start = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let (store, start) = (store.clone(), start.clone());
        workers.push(tokio::spawn(async move {
            start.wait().await;
            store
                .append("same", VersionCheck::Expected(1), vec![event("same", 2)])
                .await
        }));
    }
    let mut commits = 0;
    for worker in workers {
        match worker.await.unwrap() {
            Ok(()) => commits += 1,
            Err(EventStoreError::ConcurrencyConflict {
                expected: 1,
                actual: 2,
                aggregate_id,
            }) => assert_eq!(aggregate_id, "same"),
            other => panic!("expected typed conflict or commit, got {other:?}"),
        }
    }
    assert_eq!(commits, 1);
    assert_eq!(store.load("same").await.unwrap().len(), 2);
}

#[tokio::test]
async fn failed_second_insert_rolls_back_entire_signed_batch_and_pool_recovers() {
    let db = Database::new();
    db.connect().batch_execute("CREATE TRIGGER reject_second BEFORE INSERT ON events WHEN NEW.sequence = 2 BEGIN SELECT RAISE(ABORT, 'injected batch failure'); END").unwrap();
    let store = SqliteEventStore::new_with_integrity_key(db.url(), vec![7; 32], "test")
        .await
        .unwrap();
    let error = store
        .append(
            "batch",
            VersionCheck::New,
            vec![event("batch", 1), event("batch", 2)],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, EventStoreError::DatabaseError { ref message } if message.contains("injected batch failure")),
        "{error:?}"
    );
    assert!(store.load("batch").await.unwrap().is_empty());
    db.connect()
        .batch_execute("DROP TRIGGER reject_second")
        .unwrap();
    store
        .append(
            "batch",
            VersionCheck::New,
            vec![event("batch", 1), event("batch", 2)],
        )
        .await
        .unwrap();
    assert_eq!(store.load("batch").await.unwrap().len(), 2);
}

#[tokio::test]
async fn external_writer_exhausts_bounded_timeout_and_pools_recover() {
    let db = Database::new();
    let store = SqliteEventStore::new(db.url()).await.unwrap();
    let views = SqliteReadModelStore::new(db.url()).await.unwrap();
    let mut external = db.connect();
    external.batch_execute("BEGIN IMMEDIATE").unwrap();
    let start = Instant::now();
    let (append, projection) = tokio::join!(
        store.append("locked", VersionCheck::New, vec![event("locked", 1)]),
        views.upsert(row("locked", 1)),
    );
    let elapsed = start.elapsed();
    assert!(
        matches!(append, Err(EventStoreError::DatabaseError { ref message }) if message.contains("locked")),
        "{append:?}"
    );
    assert!(
        matches!(projection, Err(arc_core::read_model_store::ReadModelError::WriteFailed { ref message }) if message.contains("locked")),
        "{projection:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(4_800),
        "timeout missing: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(9),
        "unbounded wait: {elapsed:?}"
    );
    external.batch_execute("ROLLBACK").unwrap();
    assert!(store.load("locked").await.unwrap().is_empty());
    store
        .append("locked", VersionCheck::New, vec![event("locked", 1)])
        .await
        .unwrap();
    views.upsert(row("locked", 1)).await.unwrap();
}

#[tokio::test]
async fn commit_failure_rolls_back_and_reuses_clean_connection_without_masking_error() {
    use diesel::connection::{AnsiTransactionManager, InstrumentationEvent, TransactionManager};
    use diesel::r2d2::{ConnectionManager, Pool};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let db = Database::new();
    // A one-connection injected pool proves cleanup of the exact failed connection.
    // Its caller-selected short timeout also verifies with_pool preserves policy.
    let pool = Pool::builder()
        .max_size(1)
        .build(ConnectionManager::<SqliteConnection>::new(db.url()))
        .unwrap();
    let (request_tx, request_rx) = mpsc::channel();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let path = db.0.clone();
    let reader = std::thread::spawn(move || {
        let mut conn = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        request_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        conn.batch_execute("BEGIN; SELECT * FROM events;").unwrap();
        locked_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        conn.batch_execute("ROLLBACK").unwrap();
    });
    {
        let mut conn = pool.get().unwrap();
        conn.batch_execute(
            "PRAGMA busy_timeout=100; CREATE TEMP TABLE original_connection (id INTEGER);",
        )
        .unwrap();
        let once = AtomicBool::new(false);
        conn.set_instrumentation(move |event: InstrumentationEvent<'_>| {
            // The writer has already inserted its event, but has not committed.
            // A rollback-journal reader can still take SHARED and block COMMIT.
            if matches!(event, InstrumentationEvent::CommitTransaction { .. })
                && !once.swap(true, Ordering::SeqCst)
            {
                request_tx.send(()).unwrap();
                locked_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        });
    }
    let store = SqliteEventStore::with_pool(pool.clone());
    let outcome = store
        .append("commit", VersionCheck::New, vec![event("commit", 1)])
        .await;
    release_tx.send(()).unwrap();
    reader.join().unwrap();
    assert!(
        matches!(outcome, Err(EventStoreError::DatabaseError { ref message }) if message.contains("locked")),
        "original commit error lost: {outcome:?}"
    );
    {
        let mut conn = pool.get().unwrap();
        assert!(
            AnsiTransactionManager::transaction_manager_status_mut(&mut *conn)
                .transaction_depth()
                .unwrap()
                .is_none()
        );
        conn.batch_execute("INSERT INTO original_connection VALUES (1)")
            .unwrap();
    }
    assert!(store.load("commit").await.unwrap().is_empty());
    store
        .append("commit", VersionCheck::New, vec![event("commit", 1)])
        .await
        .unwrap();
}

#[tokio::test]
async fn automatic_sqlite_rollback_preserves_body_error_and_pool_recovers() {
    use diesel::connection::{AnsiTransactionManager, TransactionManager};
    use diesel::r2d2::{ConnectionManager, Pool};
    let db = Database::new();
    db.connect().batch_execute("CREATE TRIGGER abort_transaction BEFORE INSERT ON events WHEN NEW.sequence = 2 BEGIN SELECT RAISE(ROLLBACK, 'original body failure'); END").unwrap();
    let pool = Pool::builder()
        .max_size(1)
        .build(ConnectionManager::<SqliteConnection>::new(db.url()))
        .unwrap();
    let store = SqliteEventStore::with_pool(pool.clone());
    let error = store
        .append(
            "auto",
            VersionCheck::New,
            vec![event("auto", 1), event("auto", 2)],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, EventStoreError::DatabaseError { ref message } if message.contains("original body failure")),
        "{error:?}"
    );
    {
        let mut conn = pool.get().unwrap();
        assert!(
            AnsiTransactionManager::transaction_manager_status_mut(&mut *conn)
                .transaction_depth()
                .unwrap()
                .is_none()
        );
    }
    assert!(store.load("auto").await.unwrap().is_empty());
    db.connect()
        .batch_execute("DROP TRIGGER abort_transaction")
        .unwrap();
    store
        .append("auto", VersionCheck::New, vec![event("auto", 1)])
        .await
        .unwrap();
}

#[tokio::test]
async fn writer_reservation_is_held_before_version_query() {
    use diesel::connection::InstrumentationEvent;
    use diesel::r2d2::{ConnectionManager, Pool};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let db = Database::new();
    let pool = Pool::builder()
        .max_size(1)
        .build(ConnectionManager::<SqliteConnection>::new(db.url()))
        .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let reservations = Arc::new(AtomicUsize::new(0));
    {
        let mut conn = pool.get().unwrap();
        let mut contender = db.connect();
        let (attempts, reservations) = (attempts.clone(), reservations.clone());
        conn.set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                if query.to_string().contains("max(") {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    // Probe at the exact version-read boundary. A deferred
                    // transaction would still let this independent writer in.
                    match contender.batch_execute("BEGIN IMMEDIATE") {
                        Err(error) => {
                            assert!(error.to_string().contains("locked"), "{error:?}");
                            reservations.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(()) => contender.batch_execute("ROLLBACK").unwrap(),
                    }
                }
            }
        });
    }
    let store = SqliteEventStore::with_pool(pool);
    store
        .append("reserved", VersionCheck::New, vec![event("reserved", 1)])
        .await
        .unwrap();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "version read was instrumented"
    );
    assert_eq!(
        reservations.load(Ordering::SeqCst),
        1,
        "writer must be reserved before version read"
    );
}
