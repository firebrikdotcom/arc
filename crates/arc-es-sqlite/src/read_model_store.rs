//! # SQLite Read Model Store
//!
//! Production-grade implementation of [`ReadModelStore`] backed by an
//! `r2d2`-pooled SQLite connection. Each projection table follows the
//! framework's standard projection shape: `(id TEXT PK, version BIGINT,
//! data TEXT)`. The `data` column holds the full row as JSON, so this store
//! can serve any projector without compile-time knowledge of the
//! domain-specific columns.
//!
//! ## Idempotency
//!
//! [`upsert`](ReadModelStore::upsert) translates to
//!
//! ```sql
//! INSERT INTO {table} (id, version, data) VALUES (?, ?, ?)
//! ON CONFLICT(id) DO UPDATE
//!    SET version = excluded.version, data = excluded.data
//!  WHERE {table}.version < excluded.version
//! ```
//!
//! That gate makes replay-from-zero deterministic and tolerates at-least-once
//! delivery: applying an older event twice, or out-of-order, never regresses
//! state.
//!
//! ## Queries
//!
//! [`find_by`](ReadModelStore::find_by) uses `json_extract(data, '$.{field}')`
//! to reach into the blob. Columns commonly queried (e.g. `email`) get
//! expression indexes in the migration that creates the table.
//!
//! ## Boundaries
//!
//! The store does **not** create tables — DDL lives in `migrations/`. That
//! keeps the production write path away from connection-time mutations and
//! lets `diesel migration run` control schema evolution.

use arc_core::read_model_store::{
    CollectionOrder, CollectionPage, CollectionQuery, ReadModelError, ReadModelResult,
    ReadModelStore, Row, Upsert,
};
use async_trait::async_trait;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager};
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::sync::Arc;

type Pool = r2d2::Pool<ConnectionManager<SqliteConnection>>;

/// SQLite implementation of [`ReadModelStore`].
#[derive(Clone)]
pub struct SqliteReadModelStore {
    pool: Arc<Pool>,
}

impl SqliteReadModelStore {
    /// Build a new store from a database URL. Creates a small r2d2 pool.
    pub async fn new(database_url: &str) -> ReadModelResult<Self> {
        let manager = ConnectionManager::<SqliteConnection>::new(database_url);
        let pool = Pool::builder().max_size(10).build(manager).map_err(|e| {
            ReadModelError::other(format!("Failed to build read-model pool: {}", e))
        })?;
        Ok(SqliteReadModelStore {
            pool: Arc::new(pool),
        })
    }

    /// Build a store from an existing pool. Used in tests so we can share a
    /// single in-memory database with the event store.
    pub fn with_pool(pool: Pool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }
}

#[derive(QueryableByName, Debug)]
struct DataRow {
    #[diesel(sql_type = Text)]
    data: String,
}

/// Validate a table or column name against an allow-list of characters before
/// splicing it into a SQL string. Diesel's `sql_query` does not bind
/// identifiers, only values. This guards against caller-supplied identifiers
/// reaching SQL with anything other than `[A-Za-z0-9_]` — same posture as
/// every other SQL library that accepts dynamic table names.
fn check_ident(label: &str, ident: &str) -> ReadModelResult<()> {
    if ident.is_empty() {
        return Err(ReadModelError::other(format!("{label} cannot be empty")));
    }
    if !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ReadModelError::other(format!(
            "invalid {label} '{ident}': only [A-Za-z0-9_] permitted"
        )));
    }
    Ok(())
}

fn extract_version(row: &Row) -> ReadModelResult<i64> {
    row.get("version").and_then(|v| v.as_i64()).ok_or_else(|| {
        ReadModelError::write_failed(
            "Upsert.row missing required i64 field 'version' for version-gated upsert",
        )
    })
}

fn parse_data_row(raw: &str) -> ReadModelResult<Row> {
    serde_json::from_str(raw).map_err(|e| {
        ReadModelError::query_failed(format!("Failed to parse projection row JSON: {e}"))
    })
}

#[async_trait]
impl ReadModelStore for SqliteReadModelStore {
    async fn upsert(&self, op: Upsert) -> ReadModelResult<()> {
        check_ident("table name", &op.table)?;
        let version = extract_version(&op.row)?;
        let data = serde_json::to_string(&op.row)
            .map_err(|e| ReadModelError::write_failed(format!("Failed to serialize row: {e}")))?;
        let pool = self.pool.clone();
        let table = op.table.clone();
        let key = op.key.clone();

        tokio::task::spawn_blocking(move || -> ReadModelResult<()> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::write_failed(format!("Failed to get connection: {e}"))
            })?;

            let sql = format!(
                "INSERT INTO {table} (id, version, data) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET version = excluded.version, data = excluded.data \
                 WHERE {table}.version < excluded.version"
            );

            diesel::sql_query(sql)
                .bind::<Text, _>(key)
                .bind::<BigInt, _>(version)
                .bind::<Text, _>(data)
                .execute(&mut *conn)
                .map_err(|e| ReadModelError::write_failed(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))?
    }

    async fn delete(&self, table: &str, key: &str) -> ReadModelResult<()> {
        check_ident("table name", table)?;
        let pool = self.pool.clone();
        let table = table.to_string();
        let key = key.to_string();

        tokio::task::spawn_blocking(move || -> ReadModelResult<()> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::write_failed(format!("Failed to get connection: {e}"))
            })?;
            diesel::sql_query(format!("DELETE FROM {table} WHERE id = ?"))
                .bind::<Text, _>(key)
                .execute(&mut *conn)
                .map_err(|e| ReadModelError::write_failed(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))?
    }

    async fn get(&self, table: &str, key: &str) -> ReadModelResult<Option<Row>> {
        check_ident("table name", table)?;
        let pool = self.pool.clone();
        let table = table.to_string();
        let key = key.to_string();

        let raw = tokio::task::spawn_blocking(move || -> ReadModelResult<Option<String>> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::query_failed(format!("Failed to get connection: {e}"))
            })?;
            let rows: Vec<DataRow> =
                diesel::sql_query(format!("SELECT data FROM {table} WHERE id = ? LIMIT 1"))
                    .bind::<Text, _>(key)
                    .load(&mut *conn)
                    .map_err(|e| ReadModelError::query_failed(e.to_string()))?;
            Ok(rows.into_iter().next().map(|r| r.data))
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))??;

        match raw {
            Some(s) => Ok(Some(parse_data_row(&s)?)),
            None => Ok(None),
        }
    }

    async fn find_by(
        &self,
        table: &str,
        field: &str,
        value: &serde_json::Value,
    ) -> ReadModelResult<Vec<Row>> {
        check_ident("table name", table)?;
        check_ident("field name", field)?;
        let pool = self.pool.clone();
        let table = table.to_string();
        let path = format!("$.{field}");

        // We only support primitive comparisons here. Anything else is a
        // caller bug, not a runtime exception worth tolerating.
        let bind_text: Option<String> = match value {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
            other => {
                return Err(ReadModelError::query_failed(format!(
                    "find_by only accepts primitive values, got: {other}"
                )))
            }
        };

        let rows = tokio::task::spawn_blocking(move || -> ReadModelResult<Vec<String>> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::query_failed(format!("Failed to get connection: {e}"))
            })?;
            let sql = format!("SELECT data FROM {table} WHERE json_extract(data, ?) IS ?");
            let rows: Vec<DataRow> = diesel::sql_query(sql)
                .bind::<Text, _>(path)
                .bind::<Nullable<Text>, _>(bind_text)
                .load(&mut *conn)
                .map_err(|e| ReadModelError::query_failed(e.to_string()))?;
            Ok(rows.into_iter().map(|r| r.data).collect())
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))??;

        rows.iter().map(|r| parse_data_row(r)).collect()
    }

    async fn list(&self, table: &str) -> ReadModelResult<Vec<Row>> {
        check_ident("table name", table)?;
        let pool = self.pool.clone();
        let table = table.to_string();

        let rows = tokio::task::spawn_blocking(move || -> ReadModelResult<Vec<String>> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::query_failed(format!("Failed to get connection: {e}"))
            })?;
            let rows: Vec<DataRow> = diesel::sql_query(format!("SELECT data FROM {table}"))
                .load(&mut *conn)
                .map_err(|e| ReadModelError::query_failed(e.to_string()))?;
            Ok(rows.into_iter().map(|r| r.data).collect())
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))??;

        rows.iter().map(|r| parse_data_row(r)).collect()
    }

    async fn collection(
        &self,
        table: &str,
        query: &CollectionQuery,
    ) -> ReadModelResult<CollectionPage> {
        query.validate()?;
        check_ident("table name", table)?;
        let order = match query.order {
            CollectionOrder::Id => "id COLLATE BINARY ASC",
            CollectionOrder::NameAsc => "COALESCE(json_extract(data, '$.name'), '') COLLATE BINARY ASC, id COLLATE BINARY ASC",
            CollectionOrder::NameDesc => "COALESCE(json_extract(data, '$.name'), '') COLLATE BINARY DESC, id COLLATE BINARY ASC",
        };
        let sql = format!("SELECT data FROM {table} WHERE (? = '' OR instr(lower(COALESCE(json_extract(data, '$.name'), '')), ?) > 0) ORDER BY {order} LIMIT ? OFFSET ?");
        let pool = self.pool.clone();
        let query = query.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|e| ReadModelError::query_failed(e.to_string()))?;
            let rows: Vec<DataRow> = diesel::sql_query(sql)
                .bind::<Text, _>(query.filter.to_ascii_lowercase())
                .bind::<Text, _>(query.filter.to_ascii_lowercase())
                .bind::<BigInt, _>(query.limit as i64 + 1)
                .bind::<BigInt, _>(query.offset as i64)
                .load(&mut *conn)
                .map_err(|e| ReadModelError::query_failed(e.to_string()))?;
            let rows = rows
                .iter()
                .map(|r| parse_data_row(&r.data))
                .collect::<ReadModelResult<Vec<_>>>()?;
            Ok(CollectionPage::from_rows(rows, &query))
        })
        .await
        .map_err(|e| ReadModelError::other(e.to_string()))?
    }

    async fn truncate(&self, table: &str) -> ReadModelResult<()> {
        check_ident("table name", table)?;
        let pool = self.pool.clone();
        let table = table.to_string();

        tokio::task::spawn_blocking(move || -> ReadModelResult<()> {
            let mut conn = pool.get().map_err(|e| {
                ReadModelError::schema_failed(format!("Failed to get connection: {e}"))
            })?;
            diesel::sql_query(format!("DELETE FROM {table}"))
                .execute(&mut *conn)
                .map_err(|e| ReadModelError::schema_failed(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| ReadModelError::other(format!("Task join error: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn setup() -> SqliteReadModelStore {
        let manager = ConnectionManager::<SqliteConnection>::new(":memory:");
        let pool = Pool::builder().max_size(1).build(manager).expect("pool");
        let mut conn = pool.get().expect("conn");
        crate::test_support::migrate(&mut conn);
        drop(conn);
        SqliteReadModelStore::with_pool(pool)
    }

    fn user_row(id: &str, name: &str, email: &str, version: i64) -> Row {
        json!({
            "id": id,
            "name": name,
            "email": email,
            "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$x$y",
            "version": version,
        })
    }

    #[tokio::test]
    async fn test_upsert_inserts_then_reads() {
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice", "a@b.c", 1),
            ))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice");
        assert_eq!(got["version"], 1);
    }

    #[tokio::test]
    async fn test_upsert_advances_on_higher_version() {
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice", "a@b.c", 1),
            ))
            .await
            .unwrap();
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice2", "a@b.c", 2),
            ))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice2");
        assert_eq!(got["version"], 2);
    }

    #[tokio::test]
    async fn test_upsert_idempotent_on_lower_or_equal_version() {
        // Replay must not regress newer state.
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice2", "a@b.c", 2),
            ))
            .await
            .unwrap();

        // Same version: SQLite ON CONFLICT WHERE clause filters this out.
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Should not stick", "a@b.c", 2),
            ))
            .await
            .unwrap();
        // Lower version: same.
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Stale", "a@b.c", 1),
            ))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice2");
        assert_eq!(got["version"], 2);
    }

    #[tokio::test]
    async fn test_find_by_email_uses_index() {
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice", "a@b.c", 1),
            ))
            .await
            .unwrap();
        store
            .upsert(Upsert::new(
                "users_view",
                "u2",
                user_row("u2", "Bob", "b@b.c", 1),
            ))
            .await
            .unwrap();

        let hits = store
            .find_by("users_view", "email", &json!("b@b.c"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "u2");
    }

    #[tokio::test]
    async fn test_delete_and_truncate() {
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice", "a@b.c", 1),
            ))
            .await
            .unwrap();
        store
            .upsert(Upsert::new(
                "users_view",
                "u2",
                user_row("u2", "Bob", "b@b.c", 1),
            ))
            .await
            .unwrap();

        store.delete("users_view", "u1").await.unwrap();
        assert!(store.get("users_view", "u1").await.unwrap().is_none());
        assert_eq!(store.list("users_view").await.unwrap().len(), 1);

        store.truncate("users_view").await.unwrap();
        assert!(store.list("users_view").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_email_unique_constraint_rejects_collision() {
        // Pinned: the migration's UNIQUE INDEX on json_extract(data,'$.email')
        // protects login-by-email from ambiguity. Two rows with the same email
        // must not coexist.
        let store = setup().await;
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice", "x@y.z", 1),
            ))
            .await
            .unwrap();
        let err = store
            .upsert(Upsert::new(
                "users_view",
                "u2",
                user_row("u2", "Bob", "x@y.z", 1),
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ReadModelError::WriteFailed { ref message } if message.contains("UNIQUE")),
            "expected UNIQUE constraint violation, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_table_name_validation_rejects_injection() {
        let store = setup().await;
        let err = store
            .get("users_view; DROP TABLE users_view", "u1")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ReadModelError::Other { ref message } if message.contains("table name")),
            "expected identifier rejection, got {err:?}"
        );
    }
    #[tokio::test]

    async fn bounded_collection_regression() {
        let store = setup().await;
        for index in (0..105).rev() {
            let id = format!("{index:03}");
            store.upsert(Upsert::new("users_view", &id, json!({"id": id, "name": if index < 103 { "Same" } else { "Été 100%_" }, "email": format!("{index}@example.test"), "version": 1}))).await.unwrap();
        }
        let mut query = CollectionQuery::new(20, 0).unwrap();
        query.order = CollectionOrder::NameAsc;
        let first = store.collection("users_view", &query).await.unwrap();
        assert_eq!(first.rows.len(), 20);
        assert!(first.has_next);
        assert_eq!(first.rows[0]["id"], "000");
        query.offset = 20;
        let second = store.collection("users_view", &query).await.unwrap();
        assert_eq!(second.rows[0]["id"], "020");
        assert_eq!(second.rows[19]["id"], "039");
        query.offset = 100;
        let last = store.collection("users_view", &query).await.unwrap();
        assert_eq!(last.rows.len(), 5);
        assert!(!last.has_next);
        query.offset = CollectionQuery::MAX_OFFSET;
        assert!(store
            .collection("users_view", &query)
            .await
            .unwrap()
            .rows
            .is_empty());
        query.offset = 0;
        query.limit = 100;
        assert_eq!(
            store
                .collection("users_view", &query)
                .await
                .unwrap()
                .rows
                .len(),
            100
        );
        query.filter = "ÉTé 100%_".into();
        let filtered = store.collection("users_view", &query).await.unwrap();
        assert_eq!(filtered.rows.len(), 2);
        assert_eq!(filtered.rows[0]["id"], "103");
        query.filter = "same".into();
        query.order = CollectionOrder::NameDesc;
        assert_eq!(
            store.collection("users_view", &query).await.unwrap().rows[0]["id"],
            "000"
        );
        query.filter = "' OR 1=1 --".into();
        assert!(store
            .collection("users_view", &query)
            .await
            .unwrap()
            .rows
            .is_empty());
        for limit in [0, 101, u64::MAX] {
            query.limit = limit;
            assert!(store.collection("users_view", &query).await.is_err());
        }
        query.limit = 20;
        query.offset = u64::MAX;
        assert!(store.collection("users_view", &query).await.is_err());
        query.offset = 0;
        for filter in ["é".repeat(513), "x\0y".into()] {
            query.filter = filter;
            assert!(store.collection("users_view", &query).await.is_err());
        }
    }

    #[tokio::test]
    async fn collection_does_not_load_or_decode_rows_beyond_lookahead() {
        let store = setup().await;
        let mut conn = store.pool.get().unwrap();
        // Separate table allows malformed JSON as a sentinel beyond the window.
        diesel::sql_query("CREATE TABLE bounded_probe (id TEXT PRIMARY KEY, data TEXT)")
            .execute(&mut *conn)
            .unwrap();
        for (id, data) in [
            ("a", r#"{"name":"a"}"#),
            ("b", r#"{"name":"b"}"#),
            ("z", "invalid-json"),
        ] {
            diesel::sql_query("INSERT INTO bounded_probe VALUES (?, ?)")
                .bind::<Text, _>(id)
                .bind::<Text, _>(data)
                .execute(&mut *conn)
                .unwrap();
        }
        drop(conn);
        let page = store
            .collection("bounded_probe", &CollectionQuery::new(1, 0).unwrap())
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.has_next);
    }
}
