//! Database identity provider for Arc: `users`, `roles`, `user_roles` and
//! `browser_sessions`, bootstrapped by [`DbIdentityPlugin`] setup.
//!
//! SQLite is always available. PostgreSQL is available with the `postgres`
//! cargo feature; the backend is chosen from the database URL
//! (`postgres://` or `postgresql://` selects PostgreSQL, anything else is a
//! SQLite path).
use arc_auth_core::{AuthError, CollectionPage, CollectionQuery, Identity, IdentityStore};
use arc_web::{ArcAppBuilder, ArcPlugin, PluginSetupContext};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use async_trait::async_trait;
use diesel::{connection::SimpleConnection, prelude::*, sql_query};
use rand::rngs::OsRng;
use std::{
    borrow::Cow,
    io::{self, IsTerminal, Write},
    sync::Arc,
};
use uuid::Uuid;

const IDENTITY_ROLES_MIGRATION: &str =
    include_str!("../migrations/90000000000000_identity_roles/up.sql");
const BROWSER_SESSIONS_MIGRATION: &str =
    include_str!("../migrations/90000000000001_browser_sessions/up.sql");

/// PostgreSQL DDL for the `users`, `roles` and `user_roles` tables (and the
/// seeded `admin`/`user` roles). [`DbIdentityPlugin`] setup applies it; it is
/// exported for applications that manage their Postgres schema themselves.
pub const POSTGRES_IDENTITY_ROLES_MIGRATION: &str =
    include_str!("../migrations/postgres/90000000000000_identity_roles/up.sql");
/// PostgreSQL DDL for the `browser_sessions` table.
pub const POSTGRES_BROWSER_SESSIONS_MIGRATION: &str =
    include_str!("../migrations/postgres/90000000000001_browser_sessions/up.sql");

/// Serializes concurrent Postgres schema setup (arbitrary, stable key).
#[cfg(feature = "postgres")]
const POSTGRES_SETUP_LOCK: i64 = 0x6172_635f_6175_7468; // "arc_auth"

/// Storage backend selected from a database URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Sqlite,
    Postgres,
}
impl Backend {
    fn from_url(url: &str) -> Self {
        let url = url.trim_start();
        let scheme = url.split_once("://").map(|(scheme, _)| scheme);
        match scheme.map(str::to_ascii_lowercase).as_deref() {
            Some("postgres" | "postgresql") => Backend::Postgres,
            _ => Backend::Sqlite,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Backend::Sqlite => "sqlite",
            Backend::Postgres => "postgres",
        }
    }
}

/// Checks that `DATABASE_DRIVER` (when set) agrees with the URL's backend.
fn check_driver(driver: Option<&str>, backend: Backend) -> io::Result<()> {
    let Some(driver) = driver else {
        return Ok(());
    };
    let named = match driver.trim().to_ascii_lowercase().as_str() {
        "" | "sqlite" => Backend::Sqlite,
        "postgres" | "postgresql" | "pg" => Backend::Postgres,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("arc-auth-db supports DATABASE_DRIVER=sqlite or postgres, not `{other}`"),
            ))
        }
    };
    if named != backend {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "DATABASE_DRIVER={driver} does not match the {} database URL given to arc-auth-db",
                backend.as_str()
            ),
        ));
    }
    Ok(())
}

/// Connection to whichever backend the URL names.
enum AnyConnection {
    Sqlite(SqliteConnection),
    #[cfg(feature = "postgres")]
    Postgres(diesel::PgConnection),
}
impl AnyConnection {
    fn establish(url: &str) -> Result<Self, String> {
        match Backend::from_url(url) {
            Backend::Sqlite => SqliteConnection::establish(url)
                .map(Self::Sqlite)
                .map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Backend::Postgres => diesel::PgConnection::establish(url)
                .map(Self::Postgres)
                .map_err(|e| e.to_string()),
            #[cfg(not(feature = "postgres"))]
            Backend::Postgres => Err(
                "PostgreSQL database URLs require arc-auth-db's `postgres` cargo feature".into(),
            ),
        }
    }
    fn install_schema(&mut self) -> QueryResult<()> {
        match self {
            Self::Sqlite(c) => c.install_schema(),
            #[cfg(feature = "postgres")]
            Self::Postgres(c) => c.install_schema(),
        }
    }
    #[cfg(test)]
    fn batch_execute(&mut self, sql: &str) -> QueryResult<()> {
        match self {
            Self::Sqlite(c) => c.batch_execute(sql),
            #[cfg(feature = "postgres")]
            Self::Postgres(c) => c.batch_execute(sql),
        }
    }
}

/// Per-backend SQL dialect and transaction behaviour.
trait Dialect: Connection + Sized {
    const POSTGRES: bool;
    /// A transaction that takes the write lock up front where the backend
    /// distinguishes (SQLite `BEGIN IMMEDIATE`).
    fn write_transaction<T, E, F>(&mut self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Self) -> Result<T, E>,
        E: From<diesel::result::Error>;
    /// Idempotently create the identity and browser-session tables.
    fn install_schema(&mut self) -> QueryResult<()>;
}
impl Dialect for SqliteConnection {
    const POSTGRES: bool = false;
    fn write_transaction<T, E, F>(&mut self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Self) -> Result<T, E>,
        E: From<diesel::result::Error>,
    {
        self.immediate_transaction(f)
    }
    fn install_schema(&mut self) -> QueryResult<()> {
        self.batch_execute(IDENTITY_ROLES_MIGRATION)?;
        self.batch_execute(BROWSER_SESSIONS_MIGRATION)
    }
}
#[cfg(feature = "postgres")]
impl Dialect for diesel::PgConnection {
    const POSTGRES: bool = true;
    fn write_transaction<T, E, F>(&mut self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Self) -> Result<T, E>,
        E: From<diesel::result::Error>,
    {
        self.transaction(f)
    }
    fn install_schema(&mut self) -> QueryResult<()> {
        self.transaction(|c| {
            sql_query("SELECT pg_advisory_xact_lock($1)")
                .bind::<diesel::sql_types::BigInt, _>(POSTGRES_SETUP_LOCK)
                .execute(c)?;
            c.batch_execute(POSTGRES_IDENTITY_ROLES_MIGRATION)?;
            c.batch_execute(POSTGRES_BROWSER_SESSIONS_MIGRATION)
        })
    }
}

/// Statement text for `C`: SQLite SQL as written, or the Postgres variant with
/// `?` placeholders renumbered to `$1, $2, ...`.
fn sql<C: Dialect>(sqlite: &'static str, postgres: &'static str) -> Cow<'static, str> {
    if !C::POSTGRES {
        return Cow::Borrowed(sqlite);
    }
    let mut out = String::with_capacity(postgres.len() + 8);
    let mut n = 0;
    for ch in postgres.chars() {
        if ch == '?' {
            n += 1;
            out.push('$');
            out.push_str(&n.to_string());
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}
/// Statement whose text is the same on both backends apart from placeholders.
fn same<C: Dialect>(statement: &'static str) -> Cow<'static, str> {
    sql::<C>(statement, statement)
}

struct BrowserStoreError(AuthError);
impl From<AuthError> for BrowserStoreError {
    fn from(error: AuthError) -> Self {
        Self(error)
    }
}
impl From<diesel::result::Error> for BrowserStoreError {
    fn from(error: diesel::result::Error) -> Self {
        Self(AuthError::Store(error.to_string()))
    }
}

#[derive(Clone)]
pub struct DbIdentityStore {
    database_url: String,
}
impl DbIdentityStore {
    pub fn new(database_url: impl Into<String>) -> Self {
        Self {
            database_url: database_url.into(),
        }
    }
    fn connect(&self) -> Result<AnyConnection, AuthError> {
        AnyConnection::establish(&self.database_url).map_err(AuthError::Store)
    }
}

#[derive(QueryableByName)]
struct UserRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    id: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    email: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    password_hash: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    active: i32,
}
#[derive(QueryableByName)]
struct RoleRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}
#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}
fn validate(name: &str, email: &str) -> Result<String, AuthError> {
    if name.trim().is_empty() {
        return Err(AuthError::InvalidInput("name is required".into()));
    }
    let email = email.trim().to_ascii_lowercase();
    if !email
        .split_once('@')
        .is_some_and(|(l, r)| !l.is_empty() && r.contains('.'))
    {
        return Err(AuthError::InvalidInput("valid email is required".into()));
    }
    Ok(email)
}
fn hash(password: &str) -> Result<String, AuthError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| AuthError::Store(e.to_string()))
}
fn store_error(e: diesel::result::Error) -> AuthError {
    AuthError::Store(e.to_string())
}

/// Store operations, instantiated once per backend connection type so each
/// body is checked against a concrete diesel connection.
macro_rules! identity_ops {
    ($module:ident, $conn:ty) => {
        mod $module {
            use super::*;
            type C = $conn;

            const ACTIVE_USER_BY_EMAIL_SQLITE: &str = "SELECT id,name,email,password_hash,active FROM users WHERE email = ? COLLATE NOCASE AND active = 1";
            const ACTIVE_USER_BY_EMAIL_POSTGRES: &str = "SELECT id,name,email,password_hash,active FROM users WHERE lower(email) = lower(?) AND active = 1";

            fn roles(c: &mut C, id: &str) -> Result<Vec<String>, AuthError> {
                sql_query(same::<C>("SELECT roles.name AS name FROM roles JOIN user_roles ON user_roles.role_id = roles.id WHERE user_roles.user_id = ? ORDER BY roles.name"))
                    .bind::<diesel::sql_types::Text, _>(id)
                    .load::<RoleRow>(c)
                    .map(|rows| rows.into_iter().map(|r| r.name).collect())
                    .map_err(store_error)
            }
            fn identity(c: &mut C, row: UserRow) -> Result<Identity, AuthError> {
                let assigned = roles(c, &row.id)?;
                Ok(Identity {
                    id: row.id,
                    name: row.name,
                    email: row.email,
                    active: row.active != 0,
                    roles: assigned,
                })
            }
            fn active_user_by_email(
                c: &mut C,
                email: &str,
            ) -> diesel::QueryResult<Option<UserRow>> {
                sql_query(sql::<C>(
                    ACTIVE_USER_BY_EMAIL_SQLITE,
                    ACTIVE_USER_BY_EMAIL_POSTGRES,
                ))
                .bind::<diesel::sql_types::Text, _>(email.trim())
                .get_result::<UserRow>(c)
                .optional()
            }
            fn verify(row: &UserRow, password: &str) -> Result<(), AuthError> {
                let parsed = PasswordHash::new(&row.password_hash)
                    .map_err(|_| AuthError::InvalidCredentials)?;
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .map_err(|_| AuthError::InvalidCredentials)
            }
            fn delete_user_sessions(c: &mut C, id: &str) -> diesel::QueryResult<usize> {
                sql_query(same::<C>("DELETE FROM browser_sessions WHERE user_id=?"))
                    .bind::<diesel::sql_types::Text, _>(id)
                    .execute(c)
            }
            fn assign_roles(c: &mut C, id: &str, assigned: &[String]) -> diesel::QueryResult<()> {
                for role in assigned {
                    sql_query(same::<C>(
                        "INSERT INTO user_roles (user_id,role_id) SELECT ?,id FROM roles WHERE name = ?",
                    ))
                    .bind::<diesel::sql_types::Text, _>(id)
                    .bind::<diesel::sql_types::Text, _>(role)
                    .execute(c)?;
                }
                Ok(())
            }

            pub(super) fn get(c: &mut C, id: &str) -> Result<Option<Identity>, AuthError> {
                sql_query(same::<C>(
                    "SELECT id,name,email,password_hash,active FROM users WHERE id = ?",
                ))
                .bind::<diesel::sql_types::Text, _>(id)
                .get_result::<UserRow>(c)
                .optional()
                .map_err(store_error)?
                .map(|row| identity(c, row))
                .transpose()
            }
            pub(super) fn authenticate_browser(
                c: &mut C,
                email: &str,
                password: &str,
                ttl_seconds: u32,
            ) -> Result<(Identity, String), AuthError> {
                c.write_transaction::<_, BrowserStoreError, _>(|c| {
                    let row = active_user_by_email(c, email)?.ok_or(AuthError::InvalidCredentials)?;
                    verify(&row, password)?;
                    let user = identity(c, row)?;
                    let id = Uuid::new_v4().to_string();
                    sql_query(same::<C>("DELETE FROM browser_sessions WHERE expires_at <= ?"))
                        .bind::<diesel::sql_types::BigInt, _>(now_us())
                        .execute(c)?;
                    sql_query(same::<C>(
                        "INSERT INTO browser_sessions (id,user_id,expires_at) VALUES (?,?,?)",
                    ))
                    .bind::<diesel::sql_types::Text, _>(&id)
                    .bind::<diesel::sql_types::Text, _>(&user.id)
                    .bind::<diesel::sql_types::BigInt, _>(
                        now_us() + i64::from(ttl_seconds) * 1_000_000,
                    )
                    .execute(c)?;
                    Ok((user, id))
                })
                .map_err(|e| e.0)
            }
            pub(super) fn browser_identity(
                c: &mut C,
                session_id: &str,
            ) -> Result<Option<Identity>, AuthError> {
                c.transaction::<_, BrowserStoreError, _>(|c| {
                    let row = sql_query(same::<C>("SELECT users.id,name,email,password_hash,active FROM users JOIN browser_sessions ON users.id=browser_sessions.user_id WHERE browser_sessions.id=? AND expires_at>? AND active=1"))
                        .bind::<diesel::sql_types::Text, _>(session_id)
                        .bind::<diesel::sql_types::BigInt, _>(now_us())
                        .get_result::<UserRow>(c)
                        .optional()?;
                    Ok(row.map(|row| identity(c, row)).transpose()?)
                })
                .map_err(|e| e.0)
            }
            pub(super) fn revoke_browser_session(
                c: &mut C,
                session_id: &str,
            ) -> Result<(), AuthError> {
                sql_query(same::<C>("DELETE FROM browser_sessions WHERE id=?"))
                    .bind::<diesel::sql_types::Text, _>(session_id)
                    .execute(c)
                    .map_err(store_error)?;
                Ok(())
            }
            pub(super) fn authenticate(
                c: &mut C,
                email: &str,
                password: &str,
            ) -> Result<Identity, AuthError> {
                let row = active_user_by_email(c, email)
                    .map_err(store_error)?
                    .ok_or(AuthError::InvalidCredentials)?;
                verify(&row, password)?;
                identity(c, row)
            }
            pub(super) fn list(c: &mut C) -> Result<Vec<Identity>, AuthError> {
                let rows = sql_query(sql::<C>(
                    "SELECT id,name,email,password_hash,active FROM users ORDER BY email",
                    "SELECT id,name,email,password_hash,active FROM users ORDER BY email COLLATE \"C\"",
                ))
                .load::<UserRow>(c)
                .map_err(store_error)?;
                rows.into_iter().map(|row| identity(c, row)).collect()
            }
            pub(super) fn collection(
                c: &mut C,
                query: &CollectionQuery,
            ) -> Result<CollectionPage<Identity>, AuthError> {
                let filter = query.filter.trim().to_ascii_lowercase();
                let rows = sql_query(sql::<C>(
                    "SELECT id,name,email,'' AS password_hash,active FROM users WHERE instr(lower(name), ?) > 0 OR instr(lower(email), ?) > 0 ORDER BY email COLLATE BINARY, id COLLATE BINARY LIMIT ? OFFSET ?",
                    "SELECT id,name,email,'' AS password_hash,active FROM users WHERE strpos(lower(name), lower(?)) > 0 OR strpos(lower(email), lower(?)) > 0 ORDER BY email COLLATE \"C\", id COLLATE \"C\" LIMIT ? OFFSET ?",
                ))
                .bind::<diesel::sql_types::Text, _>(&filter)
                .bind::<diesel::sql_types::Text, _>(&filter)
                .bind::<diesel::sql_types::BigInt, _>(query.limit as i64 + 1)
                .bind::<diesel::sql_types::BigInt, _>(query.offset as i64)
                .load::<UserRow>(c)
                .map_err(store_error)?;
                let page = CollectionPage::from_rows(rows, query);
                Ok(CollectionPage {
                    rows: page
                        .rows
                        .into_iter()
                        .map(|row| identity(c, row))
                        .collect::<Result<_, _>>()?,
                    has_next: page.has_next,
                })
            }
            pub(super) fn has_users(c: &mut C) -> Result<bool, AuthError> {
                Ok(sql_query("SELECT COUNT(*) AS count FROM users")
                    .get_result::<Count>(c)
                    .map_err(store_error)?
                    .count
                    > 0)
            }
            pub(super) fn create_user(
                c: &mut C,
                name: &str,
                email: &str,
                password: &str,
                assigned: &[String],
            ) -> Result<Identity, AuthError> {
                let email = validate(name, email)?;
                let id = Uuid::new_v4().to_string();
                let password_hash = hash(password)?;
                let now = now_us();
                c.transaction::<_, diesel::result::Error, _>(|c| {
                    sql_query(same::<C>("INSERT INTO users (id,name,email,password_hash,active,created_at,updated_at) VALUES (?,?,?,?,1,?,?)"))
                        .bind::<diesel::sql_types::Text, _>(&id)
                        .bind::<diesel::sql_types::Text, _>(name.trim())
                        .bind::<diesel::sql_types::Text, _>(&email)
                        .bind::<diesel::sql_types::Text, _>(&password_hash)
                        .bind::<diesel::sql_types::BigInt, _>(now)
                        .bind::<diesel::sql_types::BigInt, _>(now)
                        .execute(c)?;
                    assign_roles(c, &id, assigned)
                })
                .map_err(|e| {
                    if matches!(
                        e,
                        diesel::result::Error::DatabaseError(
                            diesel::result::DatabaseErrorKind::UniqueViolation,
                            _
                        )
                    ) {
                        AuthError::DuplicateEmail
                    } else {
                        store_error(e)
                    }
                })?;
                get(c, &id)?.ok_or(AuthError::NotFound)
            }
            pub(super) fn update_profile(
                c: &mut C,
                id: &str,
                name: &str,
                email: &str,
            ) -> Result<Identity, AuthError> {
                let email = validate(name, email)?;
                sql_query(same::<C>("UPDATE users SET name=?,email=?,updated_at=? WHERE id=?"))
                    .bind::<diesel::sql_types::Text, _>(name.trim())
                    .bind::<diesel::sql_types::Text, _>(&email)
                    .bind::<diesel::sql_types::BigInt, _>(now_us())
                    .bind::<diesel::sql_types::Text, _>(id)
                    .execute(c)
                    .map_err(store_error)?;
                get(c, id)?.ok_or(AuthError::NotFound)
            }
            pub(super) fn change_password(
                c: &mut C,
                id: &str,
                password: &str,
            ) -> Result<(), AuthError> {
                let password_hash = hash(password)?;
                c.transaction::<_, diesel::result::Error, _>(|c| {
                    sql_query(same::<C>(
                        "UPDATE users SET password_hash=?,updated_at=? WHERE id=?",
                    ))
                    .bind::<diesel::sql_types::Text, _>(&password_hash)
                    .bind::<diesel::sql_types::BigInt, _>(now_us())
                    .bind::<diesel::sql_types::Text, _>(id)
                    .execute(c)?;
                    delete_user_sessions(c, id)?;
                    Ok(())
                })
                .map_err(store_error)
            }
            pub(super) fn set_roles(
                c: &mut C,
                id: &str,
                assigned: &[String],
            ) -> Result<Identity, AuthError> {
                if !assigned.iter().any(|role| role == "admin") {
                    ensure_not_final_admin(c, id)?;
                }
                c.transaction::<_, diesel::result::Error, _>(|c| {
                    delete_user_sessions(c, id)?;
                    sql_query(same::<C>("DELETE FROM user_roles WHERE user_id=?"))
                        .bind::<diesel::sql_types::Text, _>(id)
                        .execute(c)?;
                    assign_roles(c, id, assigned)
                })
                .map_err(store_error)?;
                get(c, id)?.ok_or(AuthError::NotFound)
            }
            pub(super) fn set_active(
                c: &mut C,
                id: &str,
                active: bool,
            ) -> Result<Identity, AuthError> {
                if !active {
                    ensure_not_final_admin(c, id)?;
                }
                let changed = c
                    .transaction::<_, diesel::result::Error, _>(|c| {
                        let changed =
                            sql_query(same::<C>("UPDATE users SET active=?,updated_at=? WHERE id=?"))
                                .bind::<diesel::sql_types::Integer, _>(i32::from(active))
                                .bind::<diesel::sql_types::BigInt, _>(now_us())
                                .bind::<diesel::sql_types::Text, _>(id)
                                .execute(c)?;
                        delete_user_sessions(c, id)?;
                        Ok(changed)
                    })
                    .map_err(store_error)?;
                if changed == 0 {
                    return Err(AuthError::NotFound);
                }
                get(c, id)?.ok_or(AuthError::NotFound)
            }
            fn ensure_not_final_admin(c: &mut C, id: &str) -> Result<(), AuthError> {
                let target = sql_query(same::<C>("SELECT COUNT(*) AS count FROM users JOIN user_roles ON users.id=user_roles.user_id JOIN roles ON roles.id=user_roles.role_id WHERE users.id=? AND users.active=1 AND roles.name='admin'"))
                    .bind::<diesel::sql_types::Text, _>(id)
                    .get_result::<Count>(c)
                    .map_err(store_error)?
                    .count;
                if target == 0 {
                    return Ok(());
                }
                let admins = sql_query("SELECT COUNT(DISTINCT users.id) AS count FROM users JOIN user_roles ON users.id=user_roles.user_id JOIN roles ON roles.id=user_roles.role_id WHERE users.active=1 AND roles.name='admin'")
                    .get_result::<Count>(c)
                    .map_err(store_error)?
                    .count;
                if admins <= 1 {
                    Err(AuthError::InvalidInput(
                        "the final active administrator cannot be deactivated or lose the admin role"
                            .into(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    };
}
identity_ops!(sqlite_ops, SqliteConnection);
#[cfg(feature = "postgres")]
identity_ops!(postgres_ops, diesel::PgConnection);

/// Connect and run one store operation on the backend the URL selects.
macro_rules! dispatch {
    ($store:expr, $op:ident($($arg:expr),* $(,)?)) => {
        match $store.connect()? {
            AnyConnection::Sqlite(mut c) => sqlite_ops::$op(&mut c $(, $arg)*),
            #[cfg(feature = "postgres")]
            AnyConnection::Postgres(mut c) => postgres_ops::$op(&mut c $(, $arg)*),
        }
    };
}

#[async_trait]
impl IdentityStore for DbIdentityStore {
    async fn authenticate_browser(
        &self,
        email: &str,
        password: &str,
        ttl_seconds: u32,
    ) -> Result<(Identity, String), AuthError> {
        if ttl_seconds == 0 {
            return Err(AuthError::InvalidInput(
                "browser session lifetime must be positive".into(),
            ));
        }
        dispatch!(self, authenticate_browser(email, password, ttl_seconds))
    }
    async fn browser_identity(&self, session_id: &str) -> Result<Option<Identity>, AuthError> {
        dispatch!(self, browser_identity(session_id))
    }
    async fn revoke_browser_session(&self, session_id: &str) -> Result<(), AuthError> {
        dispatch!(self, revoke_browser_session(session_id))
    }
    async fn authenticate(&self, email: &str, password: &str) -> Result<Identity, AuthError> {
        dispatch!(self, authenticate(email, password))
    }
    async fn get(&self, id: &str) -> Result<Option<Identity>, AuthError> {
        dispatch!(self, get(id))
    }
    async fn list(&self) -> Result<Vec<Identity>, AuthError> {
        dispatch!(self, list())
    }
    async fn collection(
        &self,
        query: &CollectionQuery,
    ) -> Result<CollectionPage<Identity>, AuthError> {
        query
            .validate()
            .map_err(|e| AuthError::InvalidInput(e.to_string()))?;
        dispatch!(self, collection(query))
    }
    async fn has_users(&self) -> Result<bool, AuthError> {
        dispatch!(self, has_users())
    }
    async fn create_user(
        &self,
        name: &str,
        email: &str,
        password: &str,
        assigned: &[String],
    ) -> Result<Identity, AuthError> {
        validate(name, email)?;
        dispatch!(self, create_user(name, email, password, assigned))
    }
    async fn update_profile(
        &self,
        id: &str,
        name: &str,
        email: &str,
    ) -> Result<Identity, AuthError> {
        validate(name, email)?;
        dispatch!(self, update_profile(id, name, email))
    }
    async fn change_password(&self, id: &str, password: &str) -> Result<(), AuthError> {
        dispatch!(self, change_password(id, password))
    }
    async fn set_roles(&self, id: &str, assigned: &[String]) -> Result<Identity, AuthError> {
        dispatch!(self, set_roles(id, assigned))
    }
    async fn set_active(&self, id: &str, active: bool) -> Result<Identity, AuthError> {
        dispatch!(self, set_active(id, active))
    }
}

pub struct DbIdentityPlugin {
    store: Arc<dyn IdentityStore>,
}
impl DbIdentityPlugin {
    /// Identity plugin backed by `database_url`: a SQLite path, or (with the
    /// `postgres` feature) a `postgres://` / `postgresql://` connection URL.
    pub fn new(database_url: impl Into<String>) -> Self {
        Self {
            store: Arc::new(DbIdentityStore::new(database_url)),
        }
    }
}
#[async_trait]
impl ArcPlugin for DbIdentityPlugin {
    fn name(&self) -> &'static str {
        "auth-db"
    }
    fn register(&self, builder: ArcAppBuilder) -> ArcAppBuilder {
        builder.register_data(self.store.clone())
    }
    async fn setup(&self, context: &PluginSetupContext<'_>) -> io::Result<()> {
        let backend = Backend::from_url(context.database_url);
        check_driver(std::env::var("DATABASE_DRIVER").ok().as_deref(), backend)?;
        let mut c = AnyConnection::establish(context.database_url).map_err(|e| {
            let kind = if cfg!(feature = "postgres") || backend == Backend::Sqlite {
                io::ErrorKind::Other
            } else {
                io::ErrorKind::Unsupported
            };
            io::Error::new(kind, e)
        })?;
        c.install_schema().map_err(io::Error::other)?;
        if !self.store.has_users().await.map_err(io::Error::other)? {
            let name = bootstrap_value("ARC_SETUP_ADMIN_NAME", "Administrator name", false)?;
            let email = bootstrap_value("ARC_SETUP_ADMIN_EMAIL", "Administrator email", false)?;
            let password =
                bootstrap_value("ARC_SETUP_ADMIN_PASSWORD", "Administrator password", true)?;
            self.store
                .create_user(&name, &email, &password, &["admin".into()])
                .await
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}

fn bootstrap_value(key: &str, label: &str, secret: bool) -> io::Result<String> {
    if let Ok(value) = std::env::var(key) {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "{key} is required for noninteractive first-administrator setup"
        )));
    }
    let value = if secret {
        rpassword::prompt_password(format!("{label}: "))?
    } else {
        print!("{label}: ");
        io::stdout().flush()?;
        let mut value = String::new();
        io::stdin().read_line(&mut value)?;
        value.trim().to_owned()
    };
    if value.trim().is_empty() {
        Err(io::Error::other(format!("{label} cannot be empty")))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RemoveFile(String);
    impl Drop for RemoveFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn permits_short_passwords_for_local_bootstrap() {
        assert_eq!(
            validate("Admin", "admin@example.test").unwrap(),
            "admin@example.test"
        );
        assert!(hash("password").is_ok());
    }

    #[test]
    fn backend_follows_url_scheme_and_driver_must_agree() {
        assert_eq!(
            Backend::from_url("database/database.sqlite"),
            Backend::Sqlite
        );
        assert_eq!(Backend::from_url("file:app.db?mode=rwc"), Backend::Sqlite);
        assert_eq!(Backend::from_url("postgres://u@h/db"), Backend::Postgres);
        assert_eq!(Backend::from_url("PostgreSQL://u@h/db"), Backend::Postgres);
        assert!(check_driver(None, Backend::Postgres).is_ok());
        assert!(check_driver(Some("sqlite"), Backend::Sqlite).is_ok());
        assert!(check_driver(Some("postgres"), Backend::Postgres).is_ok());
        assert!(check_driver(Some("pg"), Backend::Postgres).is_ok());
        assert!(check_driver(Some("postgres"), Backend::Sqlite).is_err());
        assert!(check_driver(Some("sqlite"), Backend::Postgres).is_err());
        assert_eq!(
            check_driver(Some("mysql"), Backend::Sqlite)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn postgres_statements_use_numbered_placeholders() {
        assert_eq!(
            sql::<SqliteConnection>("a=? AND b=?", "x=? AND y=?"),
            "a=? AND b=?"
        );
        #[cfg(feature = "postgres")]
        assert_eq!(
            sql::<diesel::PgConnection>("a=? AND b=?", "x=? AND y=?"),
            "x=$1 AND y=$2"
        );
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn postgres_url_without_feature_reports_the_feature() {
        let store = DbIdentityStore::new("postgres://user@localhost/db");
        let error = store.has_users().await.unwrap_err().to_string();
        assert!(error.contains("postgres"), "{error}");
    }

    /// Shared bounded-collection checks; `c` has the identity schema applied.
    pub(super) async fn exercise_bounded_collection(
        store: &DbIdentityStore,
        c: &mut AnyConnection,
    ) {
        for i in (0..45).rev() {
            c.batch_execute(&format!("INSERT INTO users (id,name,email,password_hash,active,created_at,updated_at) VALUES ('{i:03}','Été 100%_','{i:03}@example.test','unused',1,1,1)"))
                .unwrap();
        }
        let mut query = CollectionQuery::new(20, 0).unwrap();
        query.filter = " ÉTé 100%_ ".into();
        let page = store.collection(&query).await.unwrap();
        assert_eq!(page.rows.len(), 20);
        assert!(page.has_next);
        assert_eq!(page.rows[0].id, "000");
        query.offset = 40;
        let page = store.collection(&query).await.unwrap();
        assert_eq!(page.rows.len(), 5);
        assert!(!page.has_next);
        assert_eq!(page.rows[0].id, "040");
        query.offset = 0;
        query.filter = "003@EXAMPLE.TEST".into();
        assert_eq!(store.collection(&query).await.unwrap().rows[0].id, "003");
        query.filter = "' OR 1=1 --".into();
        assert!(store.collection(&query).await.unwrap().rows.is_empty());
        query.filter = "é".repeat(513);
        assert!(store.collection(&query).await.is_err());
        query.filter.clear();
        for limit in [0, 101, u64::MAX] {
            query.limit = limit;
            assert!(store.collection(&query).await.is_err());
        }
        query.limit = 20;
        query.offset = u64::MAX;
        assert!(store.collection(&query).await.is_err());
    }

    /// Shared browser-session checks: re-applying the session migration keeps
    /// data, and role/active/password changes revoke every session.
    pub(super) async fn exercise_browser_sessions(
        url: &str,
        identity_migration: &str,
        browser_migration: &str,
    ) {
        let store = DbIdentityStore::new(url);
        let mut c = store.connect().unwrap();
        c.batch_execute(identity_migration).unwrap();
        let user = store
            .create_user("User", "user@example.test", "password", &["user".into()])
            .await
            .unwrap();
        for _ in 0..2 {
            c.batch_execute(browser_migration).unwrap();
        }
        assert_eq!(store.get(&user.id).await.unwrap(), Some(user.clone()));
        for change in ["roles", "active", "password"] {
            let (_, first) = store
                .authenticate_browser("user@example.test", "password", 86400)
                .await
                .unwrap();
            let (_, second) = store
                .authenticate_browser("USER@example.test", "password", 86400)
                .await
                .unwrap();
            assert!(DbIdentityStore::new(url)
                .browser_identity(&first)
                .await
                .unwrap()
                .is_some());
            match change {
                "roles" => {
                    store.set_roles(&user.id, &[]).await.unwrap();
                    store.set_roles(&user.id, &["user".into()]).await.unwrap();
                }
                "active" => {
                    store.set_active(&user.id, false).await.unwrap();
                    store.set_active(&user.id, true).await.unwrap();
                }
                _ => {
                    store.change_password(&user.id, "password").await.unwrap();
                }
            }
            assert!(store.browser_identity(&first).await.unwrap().is_none());
            assert!(store.browser_identity(&second).await.unwrap().is_none());
        }
        let (_, first) = store
            .authenticate_browser("user@example.test", "password", 86400)
            .await
            .unwrap();
        let (_, second) = store
            .authenticate_browser("user@example.test", "password", 86400)
            .await
            .unwrap();
        store.revoke_browser_session(&first).await.unwrap();
        store.revoke_browser_session(&first).await.unwrap();
        assert!(store.browser_identity(&first).await.unwrap().is_none());
        assert!(store.browser_identity(&second).await.unwrap().is_some());
        c.batch_execute("UPDATE browser_sessions SET expires_at=0")
            .unwrap();
        assert!(store.browser_identity(&second).await.unwrap().is_none());
        assert!(store
            .authenticate_browser("user@example.test", "password", 0)
            .await
            .is_err());
        assert!(store
            .authenticate_browser("user@example.test", "wrong", 60)
            .await
            .is_err());
        assert!(store.browser_identity("unknown").await.unwrap().is_none());
    }

    /// Shared identity-management checks on an installed schema.
    pub(super) async fn exercise_identity_management(store: &DbIdentityStore) {
        assert!(!store.has_users().await.unwrap());
        let admin = store
            .create_user(
                " Admin ",
                " Admin@Example.Test ",
                "secret",
                &["admin".into()],
            )
            .await
            .unwrap();
        assert_eq!(admin.name, "Admin");
        assert_eq!(admin.email, "admin@example.test");
        assert_eq!(admin.roles, vec!["admin".to_string()]);
        assert!(admin.active);
        assert!(store.has_users().await.unwrap());
        assert!(matches!(
            store
                .create_user("Other", "ADMIN@example.test", "x", &[])
                .await,
            Err(AuthError::DuplicateEmail)
        ));
        let user = store
            .create_user(
                "User",
                "user@example.test",
                "pw",
                &["user".into(), "admin".into()],
            )
            .await
            .unwrap();
        assert_eq!(user.roles, vec!["admin".to_string(), "user".to_string()]);
        assert_eq!(
            store
                .authenticate("ADMIN@example.test", "secret")
                .await
                .unwrap(),
            admin
        );
        assert!(matches!(
            store.authenticate("admin@example.test", "nope").await,
            Err(AuthError::InvalidCredentials)
        ));
        let emails: Vec<_> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.email)
            .collect();
        assert_eq!(emails, ["admin@example.test", "user@example.test"]);
        let renamed = store
            .update_profile(&user.id, "Renamed", "renamed@example.test")
            .await
            .unwrap();
        assert_eq!(renamed.email, "renamed@example.test");
        store.set_roles(&user.id, &["user".into()]).await.unwrap();
        assert!(store.set_active(&admin.id, false).await.is_err());
        assert!(store.set_roles(&admin.id, &[]).await.is_err());
        let inactive = store.set_active(&user.id, false).await.unwrap();
        assert!(!inactive.active);
        assert!(store
            .authenticate("renamed@example.test", "pw")
            .await
            .is_err());
        assert!(matches!(
            store.set_active("missing", true).await,
            Err(AuthError::NotFound)
        ));
        store.change_password(&admin.id, "changed").await.unwrap();
        assert!(store
            .authenticate("admin@example.test", "changed")
            .await
            .is_ok());
        assert_eq!(store.get("missing").await.unwrap(), None);
    }

    fn sqlite_path(label: &str) -> RemoveFile {
        RemoveFile(
            std::env::temp_dir()
                .join(format!("arc-{label}-{}.sqlite", Uuid::new_v4()))
                .to_string_lossy()
                .into_owned(),
        )
    }

    #[tokio::test]
    async fn bounded_identity_collection_filters_and_orders_in_sql() {
        let db = sqlite_path("bounded");
        let store = DbIdentityStore::new(&db.0);
        let mut c = store.connect().unwrap();
        c.batch_execute(IDENTITY_ROLES_MIGRATION).unwrap();
        exercise_bounded_collection(&store, &mut c).await;
    }

    #[tokio::test]
    async fn migration_upgrade_and_durable_invalidation() {
        let db = sqlite_path("session");
        exercise_browser_sessions(&db.0, IDENTITY_ROLES_MIGRATION, BROWSER_SESSIONS_MIGRATION)
            .await;
    }

    #[tokio::test]
    async fn sqlite_identity_management() {
        let db = sqlite_path("identity");
        let store = DbIdentityStore::new(&db.0);
        store.connect().unwrap().install_schema().unwrap();
        exercise_identity_management(&store).await;
    }
}

/// PostgreSQL integration tests. They run only when
/// `ARC_POSTGRES_TEST_DATABASE_URL` names a database the tests may create
/// throwaway schemas in; otherwise they return early.
#[cfg(all(test, feature = "postgres"))]
mod postgres_tests {
    use super::*;

    /// A uniquely named schema, dropped on exit; `url` pins `search_path` to it.
    struct Schema {
        base: String,
        name: String,
        url: String,
    }
    impl Schema {
        fn create() -> Option<Self> {
            let base = std::env::var("ARC_POSTGRES_TEST_DATABASE_URL").ok()?;
            let name = format!("arc_auth_db_test_{}", Uuid::new_v4().simple());
            diesel::PgConnection::establish(&base)
                .expect("connect to ARC_POSTGRES_TEST_DATABASE_URL")
                .batch_execute(&format!("CREATE SCHEMA {name}"))
                .unwrap();
            let separator = if base.contains('?') { '&' } else { '?' };
            let url = format!("{base}{separator}options=-csearch_path%3D{name}");
            Some(Self { base, name, url })
        }
    }
    impl Drop for Schema {
        fn drop(&mut self) {
            if let Ok(mut c) = diesel::PgConnection::establish(&self.base) {
                let _ = c.batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name));
            }
        }
    }

    #[tokio::test]
    async fn postgres_bounded_identity_collection() {
        let Some(schema) = Schema::create() else {
            return;
        };
        let store = DbIdentityStore::new(&schema.url);
        let mut c = store.connect().unwrap();
        assert!(matches!(c, AnyConnection::Postgres(_)));
        c.batch_execute(POSTGRES_IDENTITY_ROLES_MIGRATION).unwrap();
        super::tests::exercise_bounded_collection(&store, &mut c).await;
    }

    #[tokio::test]
    async fn postgres_migration_upgrade_and_durable_invalidation() {
        let Some(schema) = Schema::create() else {
            return;
        };
        super::tests::exercise_browser_sessions(
            &schema.url,
            POSTGRES_IDENTITY_ROLES_MIGRATION,
            POSTGRES_BROWSER_SESSIONS_MIGRATION,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_identity_management() {
        let Some(schema) = Schema::create() else {
            return;
        };
        let store = DbIdentityStore::new(&schema.url);
        store.connect().unwrap().install_schema().unwrap();
        super::tests::exercise_identity_management(&store).await;
    }

    #[tokio::test]
    async fn postgres_plugin_setup_is_idempotent_and_bootstraps_admin() {
        let Some(schema) = Schema::create() else {
            return;
        };
        // Only this test reads these variables.
        std::env::set_var("DATABASE_DRIVER", "postgres");
        std::env::set_var("ARC_SETUP_ADMIN_NAME", "Admin");
        std::env::set_var("ARC_SETUP_ADMIN_EMAIL", "Admin@Example.Test");
        std::env::set_var("ARC_SETUP_ADMIN_PASSWORD", "bootstrap");
        let plugin = DbIdentityPlugin::new(&schema.url);
        let context = PluginSetupContext {
            database_url: &schema.url,
            project_root: std::path::Path::new("."),
        };
        plugin.setup(&context).await.unwrap();
        plugin.setup(&context).await.unwrap();
        let store = DbIdentityStore::new(&schema.url);
        let admin = store
            .authenticate("admin@example.test", "bootstrap")
            .await
            .unwrap();
        assert_eq!(admin.roles, vec!["admin".to_string()]);
        assert_eq!(store.list().await.unwrap().len(), 1);
        std::env::set_var("DATABASE_DRIVER", "sqlite");
        assert!(plugin.setup(&context).await.is_err());
        std::env::remove_var("DATABASE_DRIVER");
    }
}
