use arc_auth_core::{AuthError, Identity, IdentityStore};
use arc_web::{ArcAppBuilder, ArcPlugin, PluginSetupContext};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use async_trait::async_trait;
use diesel::{connection::SimpleConnection, prelude::*, sql_query};
use rand::rngs::OsRng;
use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
};
use uuid::Uuid;

const IDENTITY_ROLES_MIGRATION: &str =
    include_str!("../migrations/90000000000000_identity_roles/up.sql");

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
    fn connect(&self) -> Result<SqliteConnection, AuthError> {
        SqliteConnection::establish(&self.database_url).map_err(|e| AuthError::Store(e.to_string()))
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
fn roles(connection: &mut SqliteConnection, id: &str) -> Result<Vec<String>, AuthError> {
    sql_query("SELECT roles.name AS name FROM roles JOIN user_roles ON user_roles.role_id = roles.id WHERE user_roles.user_id = ? ORDER BY roles.name").bind::<diesel::sql_types::Text,_>(id).load::<RoleRow>(connection).map(|rows|rows.into_iter().map(|r|r.name).collect()).map_err(|e|AuthError::Store(e.to_string()))
}
fn identity(connection: &mut SqliteConnection, row: UserRow) -> Result<Identity, AuthError> {
    let assigned = roles(connection, &row.id)?;
    Ok(Identity {
        id: row.id,
        name: row.name,
        email: row.email,
        active: row.active != 0,
        roles: assigned,
    })
}
fn get_row(connection: &mut SqliteConnection, id: &str) -> Result<Option<UserRow>, AuthError> {
    sql_query("SELECT id,name,email,password_hash,active FROM users WHERE id = ?")
        .bind::<diesel::sql_types::Text, _>(id)
        .get_result::<UserRow>(connection)
        .optional()
        .map_err(|e| AuthError::Store(e.to_string()))
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
        let mut c = self.connect()?;
        c.immediate_transaction::<_, BrowserStoreError, _>(|c| {
            let row = sql_query("SELECT id,name,email,password_hash,active FROM users WHERE email = ? COLLATE NOCASE AND active = 1")
                .bind::<diesel::sql_types::Text,_>(email.trim()).get_result::<UserRow>(c).optional()?.ok_or(AuthError::InvalidCredentials)?;
            let parsed = PasswordHash::new(&row.password_hash).map_err(|_| AuthError::InvalidCredentials)?;
            Argon2::default().verify_password(password.as_bytes(), &parsed).map_err(|_| AuthError::InvalidCredentials)?;
            let user = identity(c, row)?;
            let id = Uuid::new_v4().to_string();
            sql_query("DELETE FROM browser_sessions WHERE expires_at <= ?").bind::<diesel::sql_types::BigInt,_>(now_us()).execute(c)?;
            sql_query("INSERT INTO browser_sessions (id,user_id,expires_at) VALUES (?,?,?)")
                .bind::<diesel::sql_types::Text,_>(&id).bind::<diesel::sql_types::Text,_>(&user.id)
                .bind::<diesel::sql_types::BigInt,_>(now_us() + i64::from(ttl_seconds) * 1_000_000).execute(c)?;
            Ok((user, id))
        }).map_err(|e| e.0)
    }
    async fn browser_identity(&self, session_id: &str) -> Result<Option<Identity>, AuthError> {
        let mut c = self.connect()?;
        c.transaction::<_, BrowserStoreError, _>(|c| {
            let row = sql_query("SELECT users.id,name,email,password_hash,active FROM users JOIN browser_sessions ON users.id=browser_sessions.user_id WHERE browser_sessions.id=? AND expires_at>? AND active=1")
                .bind::<diesel::sql_types::Text,_>(session_id).bind::<diesel::sql_types::BigInt,_>(now_us()).get_result::<UserRow>(c).optional()?;
            Ok(row.map(|row| identity(c, row)).transpose()?)
        }).map_err(|e| e.0)
    }
    async fn revoke_browser_session(&self, session_id: &str) -> Result<(), AuthError> {
        sql_query("DELETE FROM browser_sessions WHERE id=?")
            .bind::<diesel::sql_types::Text, _>(session_id)
            .execute(&mut self.connect()?)
            .map_err(|e| AuthError::Store(e.to_string()))?;
        Ok(())
    }

    async fn authenticate(&self, email: &str, password: &str) -> Result<Identity, AuthError> {
        let mut c = self.connect()?;
        let row=sql_query("SELECT id,name,email,password_hash,active FROM users WHERE email = ? COLLATE NOCASE AND active = 1").bind::<diesel::sql_types::Text,_>(email.trim()).get_result::<UserRow>(&mut c).optional().map_err(|e|AuthError::Store(e.to_string()))?.ok_or(AuthError::InvalidCredentials)?;
        let parsed =
            PasswordHash::new(&row.password_hash).map_err(|_| AuthError::InvalidCredentials)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| AuthError::InvalidCredentials)?;
        identity(&mut c, row)
    }
    async fn get(&self, id: &str) -> Result<Option<Identity>, AuthError> {
        let mut c = self.connect()?;
        get_row(&mut c, id)?
            .map(|row| identity(&mut c, row))
            .transpose()
    }
    async fn list(&self) -> Result<Vec<Identity>, AuthError> {
        let mut c = self.connect()?;
        let rows = sql_query("SELECT id,name,email,password_hash,active FROM users ORDER BY email")
            .load::<UserRow>(&mut c)
            .map_err(|e| AuthError::Store(e.to_string()))?;
        rows.into_iter().map(|row| identity(&mut c, row)).collect()
    }
    async fn has_users(&self) -> Result<bool, AuthError> {
        let mut c = self.connect()?;
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type=diesel::sql_types::BigInt)]
            count: i64,
        }
        Ok(sql_query("SELECT COUNT(*) AS count FROM users")
            .get_result::<Count>(&mut c)
            .map_err(|e| AuthError::Store(e.to_string()))?
            .count
            > 0)
    }
    async fn create_user(
        &self,
        name: &str,
        email: &str,
        password: &str,
        assigned: &[String],
    ) -> Result<Identity, AuthError> {
        let email = validate(name, email)?;
        let mut c = self.connect()?;
        let id = Uuid::new_v4().to_string();
        let password_hash = hash(password)?;
        let now = now_us();
        c.transaction::<_,diesel::result::Error,_>(|c|{sql_query("INSERT INTO users (id,name,email,password_hash,active,created_at,updated_at) VALUES (?,?,?,?,1,?,?)").bind::<diesel::sql_types::Text,_>(&id).bind::<diesel::sql_types::Text,_>(name.trim()).bind::<diesel::sql_types::Text,_>(&email).bind::<diesel::sql_types::Text,_>(&password_hash).bind::<diesel::sql_types::BigInt,_>(now).bind::<diesel::sql_types::BigInt,_>(now).execute(c)?;for role in assigned{sql_query("INSERT INTO user_roles (user_id,role_id) SELECT ?,id FROM roles WHERE name = ?").bind::<diesel::sql_types::Text,_>(&id).bind::<diesel::sql_types::Text,_>(role).execute(c)?;}Ok(())}).map_err(|e|if matches!(e,diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::UniqueViolation,_)){AuthError::DuplicateEmail}else{AuthError::Store(e.to_string())})?;
        self.get(&id).await?.ok_or(AuthError::NotFound)
    }
    async fn update_profile(
        &self,
        id: &str,
        name: &str,
        email: &str,
    ) -> Result<Identity, AuthError> {
        let email = validate(name, email)?;
        let mut c = self.connect()?;
        sql_query("UPDATE users SET name=?,email=?,updated_at=? WHERE id=?")
            .bind::<diesel::sql_types::Text, _>(name.trim())
            .bind::<diesel::sql_types::Text, _>(&email)
            .bind::<diesel::sql_types::BigInt, _>(now_us())
            .bind::<diesel::sql_types::Text, _>(id)
            .execute(&mut c)
            .map_err(|e| AuthError::Store(e.to_string()))?;
        self.get(id).await?.ok_or(AuthError::NotFound)
    }
    async fn change_password(&self, id: &str, password: &str) -> Result<(), AuthError> {
        let mut c = self.connect()?;
        let password_hash = hash(password)?;
        c.transaction::<_, diesel::result::Error, _>(|c| {
            sql_query("UPDATE users SET password_hash=?,updated_at=? WHERE id=?")
                .bind::<diesel::sql_types::Text, _>(&password_hash)
                .bind::<diesel::sql_types::BigInt, _>(now_us())
                .bind::<diesel::sql_types::Text, _>(id)
                .execute(c)?;
            sql_query("DELETE FROM browser_sessions WHERE user_id=?")
                .bind::<diesel::sql_types::Text, _>(id)
                .execute(c)?;
            Ok(())
        })
        .map_err(|e| AuthError::Store(e.to_string()))?;
        Ok(())
    }
    async fn set_roles(&self, id: &str, assigned: &[String]) -> Result<Identity, AuthError> {
        let mut c = self.connect()?;
        if !assigned.iter().any(|role| role == "admin") {
            ensure_not_final_admin(&mut c, id)?;
        }
        c.transaction::<_, diesel::result::Error, _>(|c| {
            sql_query("DELETE FROM browser_sessions WHERE user_id=?")
                .bind::<diesel::sql_types::Text, _>(id)
                .execute(c)?;
            sql_query("DELETE FROM user_roles WHERE user_id=?")
                .bind::<diesel::sql_types::Text, _>(id)
                .execute(c)?;
            for role in assigned {
                sql_query(
                    "INSERT INTO user_roles (user_id,role_id) SELECT ?,id FROM roles WHERE name=?",
                )
                .bind::<diesel::sql_types::Text, _>(id)
                .bind::<diesel::sql_types::Text, _>(role)
                .execute(c)?;
            }
            Ok(())
        })
        .map_err(|e| AuthError::Store(e.to_string()))?;
        self.get(id).await?.ok_or(AuthError::NotFound)
    }
    async fn set_active(&self, id: &str, active: bool) -> Result<Identity, AuthError> {
        let mut c = self.connect()?;
        if !active {
            ensure_not_final_admin(&mut c, id)?;
        }
        let changed = c
            .transaction::<_, diesel::result::Error, _>(|c| {
                let changed = sql_query("UPDATE users SET active=?,updated_at=? WHERE id=?")
                    .bind::<diesel::sql_types::Integer, _>(i32::from(active))
                    .bind::<diesel::sql_types::BigInt, _>(now_us())
                    .bind::<diesel::sql_types::Text, _>(id)
                    .execute(c)?;
                sql_query("DELETE FROM browser_sessions WHERE user_id=?")
                    .bind::<diesel::sql_types::Text, _>(id)
                    .execute(c)?;
                Ok(changed)
            })
            .map_err(|e| AuthError::Store(e.to_string()))?;
        if changed == 0 {
            return Err(AuthError::NotFound);
        }
        self.get(id).await?.ok_or(AuthError::NotFound)
    }
}

fn ensure_not_final_admin(c: &mut SqliteConnection, id: &str) -> Result<(), AuthError> {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type=diesel::sql_types::BigInt)]
        count: i64,
    }
    let target = sql_query("SELECT COUNT(*) AS count FROM users JOIN user_roles ON users.id=user_roles.user_id JOIN roles ON roles.id=user_roles.role_id WHERE users.id=? AND users.active=1 AND roles.name='admin'")
        .bind::<diesel::sql_types::Text,_>(id).get_result::<Count>(c).map_err(|e|AuthError::Store(e.to_string()))?.count;
    if target == 0 {
        return Ok(());
    }
    let admins = sql_query("SELECT COUNT(DISTINCT users.id) AS count FROM users JOIN user_roles ON users.id=user_roles.user_id JOIN roles ON roles.id=user_roles.role_id WHERE users.active=1 AND roles.name='admin'")
        .get_result::<Count>(c).map_err(|e|AuthError::Store(e.to_string()))?.count;
    if admins <= 1 {
        Err(AuthError::InvalidInput(
            "the final active administrator cannot be deactivated or lose the admin role".into(),
        ))
    } else {
        Ok(())
    }
}

pub struct DbIdentityPlugin {
    store: Arc<dyn IdentityStore>,
}
impl DbIdentityPlugin {
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
        let driver = std::env::var("DATABASE_DRIVER").unwrap_or_else(|_| "sqlite".into());
        if driver != "sqlite" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("arc-auth-db currently supports DATABASE_DRIVER=sqlite, not `{driver}`"),
            ));
        }
        let mut c = SqliteConnection::establish(context.database_url).map_err(io::Error::other)?;
        c.batch_execute(IDENTITY_ROLES_MIGRATION)
            .map_err(io::Error::other)?;
        c.batch_execute(include_str!(
            "../migrations/90000000000001_browser_sessions/up.sql"
        ))
        .map_err(io::Error::other)?;
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
    use super::{hash, validate};

    #[test]
    fn permits_short_passwords_for_local_bootstrap() {
        assert_eq!(
            validate("Admin", "admin@example.test").unwrap(),
            "admin@example.test"
        );
        assert!(hash("password").is_ok());
    }
}

#[cfg(test)]
mod browser_tests {
    use super::*;
    struct Database(String);
    impl Drop for Database {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    #[tokio::test]
    async fn migration_upgrade_and_durable_invalidation() {
        let db = Database(format!(
            "/tmp/arc-nineties-session-{}.sqlite",
            Uuid::new_v4()
        ));
        let store = DbIdentityStore::new(&db.0);
        let mut c = store.connect().unwrap();
        c.batch_execute(IDENTITY_ROLES_MIGRATION).unwrap();
        let user = store
            .create_user("User", "user@example.test", "password", &["user".into()])
            .await
            .unwrap();
        for _ in 0..2 {
            c.batch_execute(include_str!(
                "../migrations/90000000000001_browser_sessions/up.sql"
            ))
            .unwrap();
        }
        assert_eq!(store.get(&user.id).await.unwrap(), Some(user.clone()));
        for change in ["roles", "active", "password"] {
            let (_, first) = store
                .authenticate_browser("user@example.test", "password", 86400)
                .await
                .unwrap();
            let (_, second) = store
                .authenticate_browser("user@example.test", "password", 86400)
                .await
                .unwrap();
            assert!(DbIdentityStore::new(&db.0)
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
}
