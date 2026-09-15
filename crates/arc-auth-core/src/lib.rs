use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub email: String,
    pub active: bool,
    pub roles: Vec<String>,
}

impl Identity {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|assigned| assigned == role)
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("identity not found")]
    NotFound,
    #[error("email is already registered")]
    DuplicateEmail,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("identity store unavailable: {0}")]
    Store(String),
}

#[async_trait]
pub trait IdentityStore: Send + Sync {
    /// Issue a durable browser session atomically with credential verification.
    /// Stores without browser-session support fail closed.
    async fn authenticate_browser(
        &self,
        _email: &str,
        _password: &str,
        _ttl_seconds: u32,
    ) -> Result<(Identity, String), AuthError> {
        Err(AuthError::Store(
            "browser sessions are not supported".into(),
        ))
    }
    async fn browser_identity(&self, _session_id: &str) -> Result<Option<Identity>, AuthError> {
        Err(AuthError::Store(
            "browser sessions are not supported".into(),
        ))
    }
    async fn revoke_browser_session(&self, _session_id: &str) -> Result<(), AuthError> {
        Err(AuthError::Store(
            "browser sessions are not supported".into(),
        ))
    }
    async fn authenticate(&self, email: &str, password: &str) -> Result<Identity, AuthError>;
    async fn get(&self, id: &str) -> Result<Option<Identity>, AuthError>;
    async fn list(&self) -> Result<Vec<Identity>, AuthError>;
    async fn has_users(&self) -> Result<bool, AuthError>;
    async fn create_user(
        &self,
        name: &str,
        email: &str,
        password: &str,
        roles: &[String],
    ) -> Result<Identity, AuthError>;
    async fn update_profile(
        &self,
        id: &str,
        name: &str,
        email: &str,
    ) -> Result<Identity, AuthError>;
    async fn change_password(&self, id: &str, password: &str) -> Result<(), AuthError>;
    async fn set_roles(&self, id: &str, roles: &[String]) -> Result<Identity, AuthError>;
    async fn set_active(&self, id: &str, active: bool) -> Result<Identity, AuthError>;
}

pub trait AuthorizationPolicy: Send + Sync {
    fn permits(&self, identity: &Identity, required_roles: &[&str]) -> bool;
}
