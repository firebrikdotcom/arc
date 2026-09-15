//! Cookie-session-backed identity for the server-rendered admin UI.
//!
//! Sessions hold a [`SessionUser`] — a lightweight projection-backed POD
//! carrying the `aggregate_id` UUID, name, and email. Reads from
//! `users_view` (Step 2 projection); never touches the retired Diesel
//! `users` table.

use actix_session::Session;
use arc_core::read_model_store::ReadModelStore;
use serde::{Deserialize, Serialize};

/// Session key holding the cached [`SessionUser`].
const SESSION_USER_KEY: &str = "user";

/// Identity stored in the cookie session after sign-in.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionUser {
    /// Aggregate UUID — same value used in JWT `sub` and audit `actor_id`.
    pub id: String,
    pub name: String,
    pub email: String,
}

impl SessionUser {
    /// Build from a `users_view` row, or `None` if the row is missing the
    /// fields the cookie session needs.
    pub fn from_row(row: &serde_json::Value) -> Option<Self> {
        Some(Self {
            id: row.get("id")?.as_str()?.to_string(),
            name: row.get("name")?.as_str()?.to_string(),
            email: row.get("email")?.as_str()?.to_string(),
        })
    }

    /// Refresh from the projection (e.g. after profile update). `view` is the
    /// read-model table the application's user projector maintains.
    pub async fn from_projection(store: &dyn ReadModelStore, view: &str, id: &str) -> Option<Self> {
        let row = store.get(view, id).await.ok().flatten()?;
        Self::from_row(&row)
    }
}

/// `true` when the cookie session carries a [`SessionUser`].
///
/// Trusts the signed/encrypted cookie store — no DB round-trip per request.
pub fn is_authenticated(session: &Session) -> bool {
    session
        .get::<SessionUser>(SESSION_USER_KEY)
        .ok()
        .flatten()
        .is_some()
}

/// Read the cached [`SessionUser`].
pub fn get_session_user(session: &Session) -> Option<SessionUser> {
    session.get::<SessionUser>(SESSION_USER_KEY).ok().flatten()
}

/// Cache identity in the session after a successful sign-in or profile change.
pub fn set_session_user(session: &Session, user: &SessionUser) {
    let _ = session.insert(SESSION_USER_KEY, user.clone());
}

/// Wipe identity (sign-out path).
pub fn clear_session_user(session: &Session) {
    session.remove(SESSION_USER_KEY);
    // Pre-cutover keys — clear in case of session carry-over from old cookies.
    session.remove("user_id");
    session.remove("user_data");
}

/// Retrieves and clears the flash message from the session.
/// Returns a tuple of (message_type, message_text) where type is "success" or "error".
/// Set `is_json` to true for structured JSON messages (used by the sign-in page).
pub fn get_session_message(session: &Session, is_json: bool) -> (String, String) {
    if !is_json {
        let simple_message = (
            "success".to_string(),
            session
                .get::<String>("message")
                .unwrap_or(Some("".to_string()))
                .unwrap_or("".to_string()),
        );

        session.remove("message");

        return simple_message;
    }

    let session_message = session
        .get::<serde_json::Value>("message")
        .unwrap_or(Some(serde_json::json!({})))
        .unwrap_or(serde_json::json!({}));

    if session_message.is_null() {
        session.remove("message");

        return ("success".to_string(), "".to_string());
    }

    if session_message["error"].is_string()
        && !session_message["error"].as_str().unwrap().is_empty()
    {
        session.remove("message");

        return (
            "error".to_string(),
            session_message["error"].as_str().unwrap().to_string(),
        );
    }

    if session_message["success"].is_string()
        && !session_message["success"].as_str().unwrap().is_empty()
    {
        session.remove("message");

        return (
            "success".to_string(),
            session_message["success"].as_str().unwrap().to_string(),
        );
    }

    session.remove("message");

    ("success".to_string(), "".to_string())
}

/// Shared cookie lifetime and absolute browser-handle lifetime. This is an
/// authentication deadline, not an audit/privacy retention policy.
pub fn browser_session_ttl_seconds() -> std::io::Result<u32> {
    let seconds = std::env::var("ARC_BROWSER_SESSION_TTL_SECONDS")
        .unwrap_or_else(|_| "86400".into())
        .parse::<u32>()
        .map_err(|_| std::io::Error::other("invalid browser session lifetime"))?;
    if seconds == 0 {
        return Err(std::io::Error::other(
            "browser session lifetime must be positive",
        ));
    }
    Ok(seconds)
}
