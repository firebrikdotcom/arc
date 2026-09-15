//! # Postgres Read Model Store
//!
//! Implementation of [`ReadModelStore`] backed by an [`sqlx::PgPool`]. Mirrors
//! the SQLite store's contract: every projection table has the standard
//! projection shape `(id TEXT PK, version BIGINT, data JSONB)`, and writes are
//! version-gated so replay and at-least-once delivery converge.
//!
//! ## Idempotency
//!
//! [`upsert`](ReadModelStore::upsert) translates to
//!
//! ```sql
//! INSERT INTO {table} (id, version, data) VALUES ($1, $2, $3)
//! ON CONFLICT (id) DO UPDATE
//!    SET version = EXCLUDED.version, data = EXCLUDED.data
//!  WHERE {table}.version < EXCLUDED.version
//! ```
//!
//! Applying an older or equal version never regresses state.
//!
//! ## Queries
//!
//! [`find_by`](ReadModelStore::find_by) uses Postgres' `data ->> $field`
//! text-extraction operator. `IS NOT DISTINCT FROM` gives null-safe equality.
//!
//! ## Identifiers
//!
//! sqlx binds values, not identifiers. Table names are spliced into SQL, so
//! they are validated against `[A-Za-z0-9_]` first (`check_ident`). Field
//! names are bound as the `->>` operand and so are parameterized, but are still
//! validated for parity with the SQLite store.

use arc_core::read_model_store::{
    CollectionOrder, CollectionPage, CollectionQuery, ReadModelError, ReadModelResult,
    ReadModelStore, Row, Upsert,
};
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row as _;

/// DDL for the `users_view` projection table. Idempotent.
const USERS_VIEW_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users_view (
    id      TEXT   NOT NULL PRIMARY KEY,
    version BIGINT NOT NULL,
    data    JSONB  NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_view_email
    ON users_view ((data ->> 'email'));
"#;

/// Postgres implementation of [`ReadModelStore`].
#[derive(Clone)]
pub struct PostgresReadModelStore {
    pool: PgPool,
}

impl PostgresReadModelStore {
    /// Build a new store from a database URL, creating a small pool.
    pub async fn new(database_url: &str) -> ReadModelResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(|e| {
                ReadModelError::other(format!("Failed to build read-model pool: {}", e))
            })?;
        Ok(PostgresReadModelStore { pool })
    }

    /// Build a store from an existing pool. Lets tests share one pool with the
    /// event store against the same database.
    pub fn with_pool(pool: PgPool) -> Self {
        PostgresReadModelStore { pool }
    }

    /// Borrow the underlying pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create the `users_view` projection table and its email index if absent.
    /// Idempotent.
    pub async fn initialize_schema(&self) -> ReadModelResult<()> {
        sqlx::raw_sql(USERS_VIEW_SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| ReadModelError::schema_failed(e.to_string()))?;
        Ok(())
    }
}

/// Validate a table or column name against an allow-list of characters before
/// splicing it into a SQL string. Guards against caller-supplied identifiers
/// carrying anything other than `[A-Za-z0-9_]`.
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

/// Map a JSON primitive to the text Postgres' `->>` operator returns for that
/// value, so `find_by` can compare against an extracted field.
fn find_by_text(value: &serde_json::Value) -> ReadModelResult<Option<String>> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s.clone())),
        serde_json::Value::Number(n) => Ok(Some(n.to_string())),
        serde_json::Value::Bool(b) => Ok(Some(if *b { "true".into() } else { "false".into() })),
        other => Err(ReadModelError::query_failed(format!(
            "find_by only accepts primitive values, got: {other}"
        ))),
    }
}

#[async_trait]
impl ReadModelStore for PostgresReadModelStore {
    async fn upsert(&self, op: Upsert) -> ReadModelResult<()> {
        check_ident("table name", &op.table)?;
        let version = extract_version(&op.row)?;

        let sql = format!(
            "INSERT INTO {table} (id, version, data) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version, data = EXCLUDED.data \
             WHERE {table}.version < EXCLUDED.version",
            table = op.table
        );

        sqlx::query(&sql)
            .bind(&op.key)
            .bind(version)
            .bind(&op.row)
            .execute(&self.pool)
            .await
            .map_err(|e| ReadModelError::write_failed(e.to_string()))?;
        Ok(())
    }

    async fn delete(&self, table: &str, key: &str) -> ReadModelResult<()> {
        check_ident("table name", table)?;
        sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|e| ReadModelError::write_failed(e.to_string()))?;
        Ok(())
    }

    async fn get(&self, table: &str, key: &str) -> ReadModelResult<Option<Row>> {
        check_ident("table name", table)?;
        let row = sqlx::query(&format!("SELECT data FROM {table} WHERE id = $1 LIMIT 1"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ReadModelError::query_failed(e.to_string()))?;

        match row {
            Some(r) => {
                Ok(Some(r.try_get("data").map_err(|e| {
                    ReadModelError::query_failed(e.to_string())
                })?))
            }
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
        let needle = find_by_text(value)?;

        let rows = sqlx::query(&format!(
            "SELECT data FROM {table} WHERE data ->> $1 IS NOT DISTINCT FROM $2"
        ))
        .bind(field)
        .bind(needle)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ReadModelError::query_failed(e.to_string()))?;

        rows.iter()
            .map(|r| {
                r.try_get("data")
                    .map_err(|e| ReadModelError::query_failed(e.to_string()))
            })
            .collect()
    }

    async fn list(&self, table: &str) -> ReadModelResult<Vec<Row>> {
        check_ident("table name", table)?;
        let rows = sqlx::query(&format!("SELECT data FROM {table}"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ReadModelError::query_failed(e.to_string()))?;

        rows.iter()
            .map(|r| {
                r.try_get("data")
                    .map_err(|e| ReadModelError::query_failed(e.to_string()))
            })
            .collect()
    }

    async fn collection(
        &self,
        table: &str,
        query: &CollectionQuery,
    ) -> ReadModelResult<CollectionPage> {
        query.validate()?;
        check_ident("table name", table)?;
        let order = match query.order {
            CollectionOrder::Id => "id COLLATE \"C\" ASC",
            CollectionOrder::NameAsc => {
                "COALESCE(data ->> 'name', '') COLLATE \"C\" ASC, id COLLATE \"C\" ASC"
            }
            CollectionOrder::NameDesc => {
                "COALESCE(data ->> 'name', '') COLLATE \"C\" DESC, id COLLATE \"C\" ASC"
            }
        };
        let rows = sqlx::query(&format!("SELECT data FROM {table} WHERE strpos(translate(COALESCE(data ->> 'name', ''), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), $1) > 0 ORDER BY {order} LIMIT $2 OFFSET $3"))
            .bind(query.filter.to_ascii_lowercase()).bind(query.limit as i64 + 1).bind(query.offset as i64)
            .fetch_all(&self.pool).await.map_err(|e| ReadModelError::query_failed(e.to_string()))?;
        let rows = rows
            .iter()
            .map(|r| {
                r.try_get("data")
                    .map_err(|e| ReadModelError::query_failed(e.to_string()))
            })
            .collect::<ReadModelResult<Vec<_>>>()?;
        Ok(CollectionPage::from_rows(rows, query))
    }

    async fn truncate(&self, table: &str) -> ReadModelResult<()> {
        check_ident("table name", table)?;
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&self.pool)
            .await
            .map_err(|e| ReadModelError::schema_failed(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_check_ident_accepts_plain_name() {
        check_ident("table name", "users_view").unwrap();
    }

    #[test]
    fn test_check_ident_rejects_injection() {
        let err = check_ident("table name", "users; DROP TABLE users").unwrap_err();
        assert!(
            matches!(err, ReadModelError::Other { ref message } if message.contains("table name")),
            "got {err:?}"
        );
    }

    #[test]
    fn test_check_ident_rejects_empty() {
        assert!(check_ident("field name", "").is_err());
    }

    #[test]
    fn test_extract_version_reads_field() {
        let row = json!({ "id": "u1", "version": 7 });
        assert_eq!(extract_version(&row).unwrap(), 7);
    }

    #[test]
    fn test_extract_version_missing_is_error() {
        let row = json!({ "id": "u1" });
        let err = extract_version(&row).unwrap_err();
        assert!(matches!(err, ReadModelError::WriteFailed { .. }));
    }

    #[test]
    fn test_find_by_text_maps_primitives() {
        assert_eq!(find_by_text(&json!("a@b.c")).unwrap(), Some("a@b.c".into()));
        assert_eq!(find_by_text(&json!(42)).unwrap(), Some("42".into()));
        assert_eq!(find_by_text(&json!(true)).unwrap(), Some("true".into()));
        assert_eq!(find_by_text(&json!(null)).unwrap(), None);
    }

    #[test]
    fn test_find_by_text_rejects_composite() {
        assert!(find_by_text(&json!({ "x": 1 })).is_err());
        assert!(find_by_text(&json!([1, 2])).is_err());
    }

    // ── Live-database tests ──────────────────────────────────────────────────
    // Gated behind ARC_POSTGRES_TEST_DATABASE_URL; see lib.rs for usage.

    async fn live_store() -> Option<PostgresReadModelStore> {
        let url = std::env::var("ARC_POSTGRES_TEST_DATABASE_URL").ok()?;
        let store = PostgresReadModelStore::new(&url).await.expect("connect");
        store.initialize_schema().await.expect("schema");
        sqlx::query("DELETE FROM users_view")
            .execute(store.pool())
            .await
            .expect("clear");
        Some(store)
    }

    fn user_row(id: &str, name: &str, email: &str, version: i64) -> Row {
        json!({ "id": id, "name": name, "email": email, "version": version })
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_live_upsert_and_get() {
        let Some(store) = live_store().await else {
            return;
        };
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
    #[serial_test::serial]
    async fn test_live_upsert_version_gate() {
        let Some(store) = live_store().await else {
            return;
        };
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                user_row("u1", "Alice2", "a@b.c", 2),
            ))
            .await
            .unwrap();
        // Stale write must not regress.
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
    #[serial_test::serial]
    async fn test_live_find_by_email() {
        let Some(store) = live_store().await else {
            return;
        };
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
    #[serial_test::serial]
    async fn test_live_delete_and_truncate() {
        let Some(store) = live_store().await else {
            return;
        };
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
    #[serial_test::serial]
    async fn bounded_collection_regression() {
        let Some(store) = live_store().await else {
            return;
        };
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
}
