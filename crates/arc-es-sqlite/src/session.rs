//! SQLite-backed [`SessionStore`] (HIPAA-4).
//!
//! Backend-agnostic per the workspace memory: the trait lives in
//! `arc-core`; this is one of several implementations. Postgres and Redis
//! variants are slot-in replacements.

use arc_core::session::{SessionRecord, SessionStore, SessionStoreError};
use async_trait::async_trait;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager};
use diesel::sqlite::SqliteConnection;
use std::sync::Arc;
use uuid::Uuid;

mod schema {
    diesel::table! {
        jwt_sessions (jti) {
            jti -> Text,
            actor_id -> Text,
            created_at_us -> BigInt,
            expires_at_us -> BigInt,
            revoked_at_us -> Nullable<BigInt>,
        }
    }
}

use schema::jwt_sessions;

#[derive(Debug, Insertable, Clone)]
#[diesel(table_name = jwt_sessions)]
struct NewSessionRow {
    jti: String,
    actor_id: String,
    created_at_us: i64,
    expires_at_us: i64,
    revoked_at_us: Option<i64>,
}

#[derive(Debug, Queryable, Clone)]
struct SessionRow {
    jti: String,
    actor_id: String,
    created_at_us: i64,
    expires_at_us: i64,
    revoked_at_us: Option<i64>,
}

impl SessionRow {
    fn into_record(self) -> Result<SessionRecord, SessionStoreError> {
        let jti = Uuid::parse_str(&self.jti)
            .map_err(|e| SessionStoreError::Sink(format!("malformed jti UUID in DB row: {e}")))?;
        Ok(SessionRecord {
            jti,
            actor_id: self.actor_id,
            created_at_us: self.created_at_us,
            expires_at_us: self.expires_at_us,
            revoked_at_us: self.revoked_at_us,
        })
    }
}

type Pool = r2d2::Pool<ConnectionManager<SqliteConnection>>;

/// Durable JWT session store backed by SQLite.
#[derive(Clone)]
pub struct SqliteSessionStore {
    pool: Arc<Pool>,
    gate: Arc<crate::file_gate::FileGate>,
}

impl SqliteSessionStore {
    pub async fn new(database_url: &str) -> Result<Self, SessionStoreError> {
        let manager = ConnectionManager::<SqliteConnection>::new(database_url);
        let pool = Pool::builder()
            .max_size(10)
            .build(manager)
            .map_err(|e| SessionStoreError::Sink(format!("failed to create pool: {e}")))?;
        Ok(Self {
            pool: Arc::new(pool),
            gate: crate::file_gate::FileGate::new(),
        })
    }

    pub fn with_pool(pool: Pool) -> Self {
        Self {
            pool: Arc::new(pool),
            gate: crate::file_gate::FileGate::new(),
        }
    }
}

/// Run a write as one transaction, after queueing fairly behind this
/// process's other writers of the same database file.
async fn run_write<F, T>(
    pool: Arc<Pool>,
    gate: Arc<crate::file_gate::FileGate>,
    f: F,
) -> Result<T, SessionStoreError>
where
    F: FnOnce(&mut SqliteConnection) -> Result<T, SessionStoreError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking(move || {
        let permit = gate
            .write(&pool)
            .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
        let mut conn = pool
            .get()
            .map_err(|e| SessionStoreError::Sink(format!("conn: {e}")))?;
        let result =
            crate::connection::write_transaction(&mut conn, &permit, SessionStoreError::Sink, f);
        permit.finish(&result);
        result
    })
    .await
}

async fn run_blocking<F, T>(f: F) -> Result<T, SessionStoreError>
where
    F: FnOnce() -> Result<T, SessionStoreError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SessionStoreError::Sink(format!("join error: {e}")))?
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn record_session(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        if record.actor_id.trim().is_empty() {
            return Err(SessionStoreError::Validation("actor_id empty".into()));
        }
        if record.expires_at_us <= record.created_at_us {
            return Err(SessionStoreError::Validation(
                "expires_at_us must be > created_at_us".into(),
            ));
        }

        let row = NewSessionRow {
            jti: record.jti.to_string(),
            actor_id: record.actor_id,
            created_at_us: record.created_at_us,
            expires_at_us: record.expires_at_us,
            revoked_at_us: record.revoked_at_us,
        };
        let pool = self.pool.clone();
        let gate = self.gate.clone();

        run_write(pool, gate, move |conn| {
            diesel::insert_into(jwt_sessions::table)
                .values(&row)
                .execute(conn)
                .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn is_valid(&self, jti: Uuid, now_us: i64) -> Result<bool, SessionStoreError> {
        let key = jti.to_string();
        let pool = self.pool.clone();
        let gate = self.gate.clone();

        run_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|e| SessionStoreError::Sink(format!("conn: {e}")))?;
            let _reading = gate.read(&mut conn);
            let row: Option<SessionRow> = jwt_sessions::table
                .filter(jwt_sessions::jti.eq(&key))
                .first(&mut conn)
                .optional()
                .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
            Ok(match row {
                Some(r) => {
                    let rec = r.into_record()?;
                    rec.is_valid_at(now_us)
                }
                None => false,
            })
        })
        .await
    }

    async fn revoke(&self, jti: Uuid, now_us: i64) -> Result<(), SessionStoreError> {
        let key = jti.to_string();
        let pool = self.pool.clone();
        let gate = self.gate.clone();

        run_write(pool, gate, move |conn| {
            let n = diesel::update(jwt_sessions::table.filter(jwt_sessions::jti.eq(&key)))
                .set(jwt_sessions::revoked_at_us.eq(Some(now_us)))
                .execute(conn)
                .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
            if n == 0 {
                Err(SessionStoreError::NotFound(jti))
            } else {
                Ok(())
            }
        })
        .await
    }

    async fn revoke_all_for_actor(
        &self,
        actor_id: &str,
        now_us: i64,
    ) -> Result<usize, SessionStoreError> {
        let key = actor_id.to_string();
        let pool = self.pool.clone();
        let gate = self.gate.clone();

        run_write(pool, gate, move |conn| {
            let n = diesel::update(
                jwt_sessions::table
                    .filter(jwt_sessions::actor_id.eq(&key))
                    .filter(jwt_sessions::revoked_at_us.is_null()),
            )
            .set(jwt_sessions::revoked_at_us.eq(Some(now_us)))
            .execute(conn)
            .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
            Ok(n)
        })
        .await
    }

    async fn prune_expired(&self, now_us: i64) -> Result<usize, SessionStoreError> {
        let pool = self.pool.clone();
        let gate = self.gate.clone();

        run_write(pool, gate, move |conn| {
            let n =
                diesel::delete(jwt_sessions::table.filter(jwt_sessions::expires_at_us.le(now_us)))
                    .execute(conn)
                    .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
            Ok(n)
        })
        .await
    }
}

// Suppress dead_code on SessionRow fields that exist for Queryable derive only.
#[allow(dead_code)]
fn _force_use(r: &SessionRow) -> &str {
    &r.actor_id
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_store() -> SqliteSessionStore {
        let manager = ConnectionManager::<SqliteConnection>::new(":memory:");
        let pool = Pool::builder().max_size(1).build(manager).expect("pool");
        let mut conn = pool.get().unwrap();
        crate::test_support::migrate(&mut conn);
        drop(conn);
        SqliteSessionStore::with_pool(pool)
    }

    fn record(jti: Uuid, actor: &str) -> SessionRecord {
        let now = 1_700_000_000_000_000;
        SessionRecord {
            jti,
            actor_id: actor.into(),
            created_at_us: now,
            expires_at_us: now + 24 * 3600 * 1_000_000,
            revoked_at_us: None,
        }
    }

    #[tokio::test]
    async fn test_record_then_is_valid() {
        let s = setup_store().await;
        let id = Uuid::new_v4();
        s.record_session(record(id, "alice")).await.unwrap();
        assert!(s.is_valid(id, 1_700_000_000_000_001).await.unwrap());
    }

    #[tokio::test]
    async fn test_revoke_then_invalid() {
        let s = setup_store().await;
        let id = Uuid::new_v4();
        s.record_session(record(id, "alice")).await.unwrap();
        s.revoke(id, 1_700_000_000_000_500).await.unwrap();
        assert!(!s.is_valid(id, 1_700_000_000_000_600).await.unwrap());
    }

    #[tokio::test]
    async fn test_revoke_unknown_returns_not_found() {
        let s = setup_store().await;
        let err = s.revoke(Uuid::new_v4(), 0).await.unwrap_err();
        assert!(matches!(err, SessionStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_revoke_all_for_actor() {
        let s = setup_store().await;
        let a1 = Uuid::new_v4();
        let a2 = Uuid::new_v4();
        let b = Uuid::new_v4();
        s.record_session(record(a1, "alice")).await.unwrap();
        s.record_session(record(a2, "alice")).await.unwrap();
        s.record_session(record(b, "bob")).await.unwrap();

        let now = 1_700_000_000_000_500;
        let n = s.revoke_all_for_actor("alice", now).await.unwrap();
        assert_eq!(n, 2);
        assert!(!s.is_valid(a1, now + 1).await.unwrap());
        assert!(!s.is_valid(a2, now + 1).await.unwrap());
        assert!(s.is_valid(b, now + 1).await.unwrap());
    }

    #[tokio::test]
    async fn test_prune_expired() {
        let s = setup_store().await;
        let live = Uuid::new_v4();
        let dead = Uuid::new_v4();
        let now = 1_700_000_000_000_000;

        s.record_session(SessionRecord {
            jti: live,
            actor_id: "x".into(),
            created_at_us: now,
            expires_at_us: now + 1_000_000,
            revoked_at_us: None,
        })
        .await
        .unwrap();
        s.record_session(SessionRecord {
            jti: dead,
            actor_id: "x".into(),
            created_at_us: now - 2000,
            expires_at_us: now - 1000,
            revoked_at_us: None,
        })
        .await
        .unwrap();

        let n = s.prune_expired(now).await.unwrap();
        assert_eq!(n, 1);
        assert!(s.is_valid(live, now + 1).await.unwrap());
        assert!(!s.is_valid(dead, now + 1).await.unwrap());
    }

    #[tokio::test]
    async fn test_indices_used_for_actor_lookup() {
        let s = setup_store().await;
        let pool = s.pool.clone();
        let plan =
            tokio::task::spawn_blocking(move || -> Result<Vec<String>, SessionStoreError> {
                let mut conn = pool
                    .get()
                    .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
                let plan: Vec<ExplainRow> = diesel::sql_query(
                    "EXPLAIN QUERY PLAN SELECT 1 FROM jwt_sessions WHERE actor_id = 'a'",
                )
                .load::<ExplainRow>(&mut *conn)
                .map_err(|e| SessionStoreError::Sink(e.to_string()))?;
                Ok(plan.into_iter().map(|r| r.detail).collect())
            })
            .await
            .unwrap()
            .unwrap();
        assert!(
            plan.iter().any(|d| d.contains("idx_jwt_sessions_actor_id")),
            "actor_id query did not use index; plan: {plan:?}"
        );
    }

    #[derive(QueryableByName, Debug)]
    struct ExplainRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        #[allow(dead_code)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        #[allow(dead_code)]
        parent: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        #[allow(dead_code)]
        notused: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        detail: String,
    }
}
