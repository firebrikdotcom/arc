//! # Read Model Store
//!
//! Backend-agnostic persistence layer for projection read models.
//!
//! The [`ReadModelStore`] trait abstracts over storage backends (SQLite, Postgres,
//! dqlite, in-memory) using a *typed* operation surface. Earlier iterations
//! exposed `execute(sql, params)` and `query(sql, params)`, which leaked the
//! SQL dialect of whichever backend was wired in — projectors written against
//! the SQLite store would silently bind to SQLite syntax. The current trait
//! describes intent (upsert this row keyed by id, find rows where field=value)
//! and lets each backend translate to its own dialect.
//!
//! ## Operations
//!
//! - [`upsert`](ReadModelStore::upsert) — version-gated insert-or-replace by
//!   primary key. The gate (`existing.version < incoming.version`) makes
//!   projectors idempotent under duplicate or out-of-order delivery.
//! - [`delete`](ReadModelStore::delete) — remove a row by primary key.
//! - [`get`](ReadModelStore::get) — fetch one row by primary key.
//! - [`find_by`](ReadModelStore::find_by) — fetch rows where a single field
//!   equals a value (covers email lookups, secondary index reads).
//! - [`list`](ReadModelStore::list) — explicit internal all-rows read.
//! - [`collection`](ReadModelStore::collection) — validated, bounded collection page.
//! - [`truncate`](ReadModelStore::truncate) — wipe a table during projection
//!   rebuild.
//!
//! ## Implementations
//!
//! - [`InMemoryReadModelStore`] — built-in, for testing and ephemeral state.
//! - `SqliteReadModelStore` (in `arc-es-sqlite`) — production SQLite.
//! - Postgres / dqlite backends — planned.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;

/// A row stored or returned by a read model query, represented as a JSON object.
pub type Row = serde_json::Value;

/// Errors from read model store operations.
#[derive(Debug, Error)]
pub enum ReadModelError {
    /// Write operation failed.
    #[error("Read model write failed: {message}")]
    WriteFailed { message: String },

    /// Query operation failed.
    #[error("Read model query failed: {message}")]
    QueryFailed { message: String },

    /// Schema or truncate operation failed.
    #[error("Read model schema operation failed: {message}")]
    SchemaFailed { message: String },

    /// Other error.
    #[error("Read model error: {message}")]
    Other { message: String },
}

impl ReadModelError {
    pub fn write_failed(message: impl Into<String>) -> Self {
        ReadModelError::WriteFailed {
            message: message.into(),
        }
    }

    pub fn query_failed(message: impl Into<String>) -> Self {
        ReadModelError::QueryFailed {
            message: message.into(),
        }
    }

    pub fn schema_failed(message: impl Into<String>) -> Self {
        ReadModelError::SchemaFailed {
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        ReadModelError::Other {
            message: message.into(),
        }
    }
}

/// Result type for read model store operations.
pub type ReadModelResult<T> = Result<T, ReadModelError>;

/// Validated collection window. SQL backends apply filtering and ordering before
/// LIMIT/OFFSET; at most `limit + 1` rows are fetched to discover the next page.
#[derive(Debug, Clone)]
pub struct CollectionQuery {
    pub limit: u64,
    pub offset: u64,
    pub filter: String,
    pub order: CollectionOrder,
}

#[derive(Debug, Clone, Copy)]
pub enum CollectionOrder {
    Id,
    NameAsc,
    NameDesc,
}

impl CollectionQuery {
    pub const MAX_LIMIT: u64 = 100;
    pub const MAX_OFFSET: u64 = 1_000_000;
    pub fn new(limit: u64, offset: u64) -> ReadModelResult<Self> {
        let query = Self {
            limit,
            offset,
            filter: String::new(),
            order: CollectionOrder::Id,
        };
        query.validate()?;
        Ok(query)
    }
    pub fn for_page(page: u64, limit: u64) -> ReadModelResult<Self> {
        let offset = page
            .max(1)
            .checked_sub(1)
            .and_then(|p| p.checked_mul(limit))
            .ok_or_else(|| ReadModelError::query_failed("page overflow"))?;
        Self::new(limit, offset)
    }
    pub fn validate(&self) -> ReadModelResult<()> {
        if self.limit == 0
            || self.limit > Self::MAX_LIMIT
            || self.offset > Self::MAX_OFFSET
            || self.filter.len() > 1024
            || self.filter.contains('\0')
        {
            return Err(ReadModelError::query_failed(
                "invalid collection window or filter",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct CollectionPage<T = Row> {
    pub rows: Vec<T>,
    pub has_next: bool,
}
impl<T> CollectionPage<T> {
    pub fn from_rows(mut rows: Vec<T>, query: &CollectionQuery) -> Self {
        let has_next = rows.len() > query.limit as usize
            && query
                .offset
                .checked_add(query.limit)
                .is_some_and(|next| next <= CollectionQuery::MAX_OFFSET);
        rows.truncate(query.limit as usize);
        Self { rows, has_next }
    }
}

/// Version-gated upsert command.
///
/// `version` is compared against the existing row's `version` column (if any).
/// The store writes only when the row is absent or the existing version is
/// strictly less than this value. That makes projectors idempotent: replaying
/// the same event sequence twice — or receiving the same event twice through
/// at-least-once delivery — converges to the same final state.
#[derive(Debug, Clone)]
pub struct Upsert {
    /// Logical table / collection name (e.g. `"users_view"`).
    pub table: String,
    /// Primary key value (string-typed; backends serialize as needed).
    pub key: String,
    /// Full row payload as a JSON object. Must include the primary key field
    /// and a `version` field so the store can apply the version gate.
    pub row: Row,
}

impl Upsert {
    pub fn new(table: impl Into<String>, key: impl Into<String>, row: Row) -> Self {
        Self {
            table: table.into(),
            key: key.into(),
            row,
        }
    }
}

/// Backend-agnostic storage for projection read models.
///
/// Implementations must be `Send + Sync`. Multiple projectors may share a
/// store instance (different tables in the same database). Interior
/// mutability (connection pools, `Mutex`, etc.) is expected.
#[async_trait]
pub trait ReadModelStore: Send + Sync {
    /// Insert or update a row, version-gated.
    ///
    /// Writes only if no row exists with the given key, or the existing row's
    /// `version` is strictly less than the incoming row's `version`. Returns
    /// `Ok(())` whether or not the gate let the write through — the caller
    /// (projector) does not need to distinguish "applied" from "skipped as
    /// stale", which matters for replay semantics.
    async fn upsert(&self, op: Upsert) -> ReadModelResult<()>;

    /// Delete a row by primary key. No-op if missing.
    async fn delete(&self, table: &str, key: &str) -> ReadModelResult<()>;

    /// Fetch a single row by primary key.
    async fn get(&self, table: &str, key: &str) -> ReadModelResult<Option<Row>>;

    /// Fetch all rows where `field` equals `value`. Used for secondary-index
    /// lookups such as login-by-email. Backends may or may not have an index
    /// on the field — the trait carries no guarantee.
    async fn find_by(
        &self,
        table: &str,
        field: &str,
        value: &serde_json::Value,
    ) -> ReadModelResult<Vec<Row>>;

    /// Fetch every row in a table. Use `collection` for public collection endpoints.
    async fn list(&self, table: &str) -> ReadModelResult<Vec<Row>>;

    /// Fetch a bounded, deterministic collection window. No load-all fallback.
    async fn collection(
        &self,
        table: &str,
        query: &CollectionQuery,
    ) -> ReadModelResult<CollectionPage>;

    /// Wipe a table. Used during projection rebuild before replay.
    async fn truncate(&self, table: &str) -> ReadModelResult<()>;
}

/// In-memory read model store for testing.
///
/// Stores rows keyed by primary key in a per-table `HashMap`. Implements the
/// version gate exactly the way SQLite does, so tests written against this
/// store exercise the same idempotency semantics as production.
pub struct InMemoryReadModelStore {
    tables: Mutex<HashMap<String, HashMap<String, Row>>>,
}

impl InMemoryReadModelStore {
    pub fn new() -> Self {
        Self {
            tables: Mutex::new(HashMap::new()),
        }
    }

    /// Get all rows in a table (test helper).
    pub fn get_rows(&self, table: &str) -> Vec<Row> {
        self.tables
            .lock()
            .unwrap()
            .get(table)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Total row count across all tables (test helper).
    pub fn total_rows(&self) -> usize {
        self.tables.lock().unwrap().values().map(|m| m.len()).sum()
    }
}

impl Default for InMemoryReadModelStore {
    fn default() -> Self {
        Self::new()
    }
}

fn row_version(row: &Row) -> i64 {
    row.get("version").and_then(|v| v.as_i64()).unwrap_or(0)
}

#[async_trait]
impl ReadModelStore for InMemoryReadModelStore {
    async fn upsert(&self, op: Upsert) -> ReadModelResult<()> {
        let mut tables = self.tables.lock().unwrap();
        let table = tables.entry(op.table).or_default();
        let incoming_version = row_version(&op.row);
        let should_write = match table.get(&op.key) {
            Some(existing) => row_version(existing) < incoming_version,
            None => true,
        };
        if should_write {
            table.insert(op.key, op.row);
        }
        Ok(())
    }

    async fn delete(&self, table: &str, key: &str) -> ReadModelResult<()> {
        if let Some(t) = self.tables.lock().unwrap().get_mut(table) {
            t.remove(key);
        }
        Ok(())
    }

    async fn get(&self, table: &str, key: &str) -> ReadModelResult<Option<Row>> {
        Ok(self
            .tables
            .lock()
            .unwrap()
            .get(table)
            .and_then(|t| t.get(key).cloned()))
    }

    async fn find_by(
        &self,
        table: &str,
        field: &str,
        value: &serde_json::Value,
    ) -> ReadModelResult<Vec<Row>> {
        Ok(self
            .tables
            .lock()
            .unwrap()
            .get(table)
            .map(|t| {
                t.values()
                    .filter(|row| row.get(field) == Some(value))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list(&self, table: &str) -> ReadModelResult<Vec<Row>> {
        Ok(self.get_rows(table))
    }

    async fn collection(
        &self,
        table: &str,
        query: &CollectionQuery,
    ) -> ReadModelResult<CollectionPage> {
        query.validate()?;
        let tables = self.tables.lock().unwrap();
        let needle = query.filter.to_ascii_lowercase();
        let mut rows: Vec<_> = tables
            .get(table)
            .into_iter()
            .flat_map(|t| t.iter())
            .filter(|(_, row)| {
                needle.is_empty()
                    || row["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .collect();
        rows.sort_by(|(aid, a), (bid, b)| {
            let names = a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""));
            match query.order {
                CollectionOrder::Id => aid.cmp(bid),
                CollectionOrder::NameAsc => names.then(aid.cmp(bid)),
                CollectionOrder::NameDesc => names.reverse().then(aid.cmp(bid)),
            }
        });
        Ok(CollectionPage::from_rows(
            rows.into_iter()
                .skip(query.offset as usize)
                .take(query.limit as usize + 1)
                .map(|(_, row)| row.clone())
                .collect(),
            query,
        ))
    }

    async fn truncate(&self, table: &str) -> ReadModelResult<()> {
        self.tables.lock().unwrap().remove(table);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, name: &str, version: i64) -> Row {
        json!({"id": id, "name": name, "version": version})
    }

    #[tokio::test]
    async fn test_upsert_inserts_when_absent() {
        let store = InMemoryReadModelStore::new();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice", 1)))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice");
    }

    #[tokio::test]
    async fn test_upsert_replaces_when_version_advances() {
        let store = InMemoryReadModelStore::new();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice", 1)))
            .await
            .unwrap();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice2", 2)))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice2");
        assert_eq!(got["version"], 2);
    }

    #[tokio::test]
    async fn test_upsert_skips_when_version_stale() {
        // Pinned: replay must not regress newer state.
        let store = InMemoryReadModelStore::new();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice2", 2)))
            .await
            .unwrap();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice1", 1)))
            .await
            .unwrap();

        let got = store.get("users_view", "u1").await.unwrap().unwrap();
        assert_eq!(got["name"], "Alice2");
        assert_eq!(got["version"], 2);
    }

    #[tokio::test]
    async fn test_find_by_returns_matching_rows() {
        let store = InMemoryReadModelStore::new();
        store
            .upsert(Upsert::new(
                "users_view",
                "u1",
                json!({"id": "u1", "email": "a@b.c", "version": 1}),
            ))
            .await
            .unwrap();
        store
            .upsert(Upsert::new(
                "users_view",
                "u2",
                json!({"id": "u2", "email": "x@y.z", "version": 1}),
            ))
            .await
            .unwrap();

        let hits = store
            .find_by("users_view", "email", &json!("a@b.c"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "u1");
    }

    #[tokio::test]
    async fn test_delete_and_truncate() {
        let store = InMemoryReadModelStore::new();
        store
            .upsert(Upsert::new("users_view", "u1", row("u1", "Alice", 1)))
            .await
            .unwrap();
        store
            .upsert(Upsert::new("users_view", "u2", row("u2", "Bob", 1)))
            .await
            .unwrap();

        store.delete("users_view", "u1").await.unwrap();
        assert!(store.get("users_view", "u1").await.unwrap().is_none());
        assert_eq!(store.list("users_view").await.unwrap().len(), 1);

        store.truncate("users_view").await.unwrap();
        assert_eq!(store.total_rows(), 0);
    }
    #[tokio::test]

    async fn bounded_collection_regression() {
        let store = InMemoryReadModelStore::new();
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

    #[test]
    fn collection_windows_validate_before_arithmetic_or_sql() {
        assert_eq!(CollectionQuery::for_page(0, 20).unwrap().offset, 0);
        assert_eq!(CollectionQuery::for_page(2, 20).unwrap().offset, 20);
        for page in [u64::MAX, u64::MAX / 20 + 2, 50_002] {
            assert!(CollectionQuery::for_page(page, 20).is_err());
        }
        let mut query = CollectionQuery::new(100, 1_000_000).unwrap();
        query.filter = "é".repeat(512);
        assert!(query.validate().is_ok());
        query.filter.push('é');
        assert!(query.validate().is_err());
    }
    #[test]
    fn last_permitted_window_does_not_advertise_an_unreachable_page() {
        let query = CollectionQuery::new(1, CollectionQuery::MAX_OFFSET).unwrap();
        let page = CollectionPage::from_rows(vec![1, 2], &query);
        assert_eq!(page.rows, vec![1]);
        assert!(!page.has_next);
        let query = CollectionQuery::new(1, CollectionQuery::MAX_OFFSET - 1).unwrap();
        assert!(CollectionPage::from_rows(vec![1, 2], &query).has_next);
    }
}
