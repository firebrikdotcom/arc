//! Authentication and authorization seams for WebSocket connections.
//!
//! The runtime deliberately knows neither the authentication plugin nor its
//! session format. Plugins provide a verified principal, and the server's
//! room policy decides which rooms that principal may join.

use actix_session::Session;
use actix_web::HttpRequest;
use async_trait::async_trait;
use std::{
    net::{Ipv4Addr, Ipv6Addr},
    sync::Arc,
};

/// Stable, verified identity attached to an upgraded WebSocket connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSocketPrincipal {
    user_id: String,
}

impl WebSocketPrincipal {
    pub fn new(user_id: impl Into<String>) -> Option<Self> {
        let user_id = user_id.into();
        (!user_id.trim().is_empty()).then_some(Self { user_id })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

/// Why a provider could not resolve the request's authenticated principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalResolutionError {
    /// Authentication state could not be checked because required state was
    /// unavailable. The handshake must fail with 503.
    Unavailable,
}

/// Resolves only an identity verified against the authentication system's
/// authoritative current state. Cached cookie fields are not proof.
#[async_trait(?Send)]
pub trait WebSocketPrincipalProvider: Send + Sync {
    async fn resolve(
        &self,
        request: &HttpRequest,
        session: &Session,
    ) -> Result<Option<WebSocketPrincipal>, PrincipalResolutionError>;
}

/// Authorization policy for a verified principal attempting to join a room.
pub trait WebSocketRoomPolicy: Send + Sync {
    fn permits(&self, principal: &WebSocketPrincipal, room: &str) -> bool;
}

/// Default policy: a user may join only the room named `user:<their-id>`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OwnUserRoomPolicy;

impl WebSocketRoomPolicy for OwnUserRoomPolicy {
    fn permits(&self, principal: &WebSocketPrincipal, room: &str) -> bool {
        room == format!("user:{}", principal.user_id())
    }
}

/// Exact HTTP(S) origins permitted to initiate WebSocket handshakes.
///
/// An empty allowlist denies every origin. Raw syntax is checked before
/// normalization so path and whitespace normalization cannot admit malformed input.
#[derive(Clone, Debug, Default)]
pub struct WebSocketOriginAllowlist {
    origins: Vec<String>,
}

impl WebSocketOriginAllowlist {
    pub fn new<I, S>(origins: I) -> Result<Self, &'static str>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = Vec::new();
        for value in origins {
            let value = value.as_ref();
            let origin = parse_origin(value).ok_or("invalid WebSocket origin allowlist entry")?;
            if !normalized.contains(&origin) {
                normalized.push(origin);
            }
        }
        Ok(Self {
            origins: normalized,
        })
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// Return whether exactly one syntactically valid Origin header is allowed.
    pub fn allows_headers<I, S>(&self, values: I) -> bool
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        let mut values = values;
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let value = value.as_ref();
        if value.trim() != value {
            return false;
        }
        parse_origin(value).is_some_and(|origin| self.origins.contains(&origin))
    }
}

fn parse_origin(value: &str) -> Option<String> {
    if value.is_empty() || value.eq_ignore_ascii_case("null") {
        return None;
    }
    if !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == 0x7f)
    {
        return None;
    }
    let (scheme, authority) = value.split_once("://")?;
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else {
        return None;
    };
    if authority.is_empty()
        || authority
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'?' | b'#' | b'@' | b'\\' | b'%'))
    {
        return None;
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let ipv6 = bracketed[..close].parse::<Ipv6Addr>().ok()?;
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(parse_port(suffix.strip_prefix(':')?)?)
        };
        (format!("[{}]", ipv6), port)
    } else {
        let mut parts = authority.split(':');
        let host = parts.next()?;
        let port = match (parts.next(), parts.next()) {
            (None, None) => None,
            (Some(raw_port), None) => Some(parse_port(raw_port)?),
            _ => return None, // IPv6 literals must be bracketed.
        };
        (normalize_host(host)?, port)
    };
    let effective_port = port.unwrap_or(if scheme == "http" { 80 } else { 443 });
    Some(format!("{scheme}://{host}:{effective_port}"))
}

fn parse_port(value: &str) -> Option<u16> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn normalize_host(host: &str) -> Option<String> {
    if host.is_empty() || host.len() > 254 {
        return None;
    }
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return host
            .parse::<Ipv4Addr>()
            .ok()
            .map(|address| address.to_string());
    }

    let domain = host.strip_suffix('.').unwrap_or(host);
    if domain.is_empty()
        || domain.len() > 253
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !label
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric())
                || !label
                    .bytes()
                    .last()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Shared policy state accepted by `WsServer` constructors.
pub type SharedWebSocketRoomPolicy = Arc<dyn WebSocketRoomPolicy>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allowlist_normalizes_scheme_host_and_default_port() {
        let policy =
            WebSocketOriginAllowlist::new(["https://Example.Test"]).expect("valid allowlist");
        for origin in ["https://example.test", "HTTPS://EXAMPLE.TEST:443"] {
            assert!(policy.allows_headers([origin].into_iter()), "{origin}");
        }
        assert!(!policy.allows_headers(["https://example.test:444"].into_iter()));

        let http =
            WebSocketOriginAllowlist::new(["http://example.test:80"]).expect("valid allowlist");
        assert!(http.allows_headers(["http://EXAMPLE.TEST"].into_iter()));
        assert!(!http.allows_headers(["https://example.test"].into_iter()));
    }

    #[test]
    fn origin_allowlist_accepts_only_raw_origin_syntax() {
        let policy =
            WebSocketOriginAllowlist::new(["https://example.test"]).expect("valid allowlist");
        for origin in [
            "",
            "null",
            "https://example.test/",
            "https://example.test/a/..",
            "https://example.test\t",
            "https://example.test\n.evil",
            " https://example.test",
            "https://example.test ",
            "https://example.test\\evil",
            "https://example.test%2f.evil",
            "https://example.test%5c.evil",
            "https://user@example.test",
            "https://example.test?x=1",
            "https://example.test#fragment",
            "https://example.test,https://example.test",
            "https://example.test https://example.test",
            "https://example.test:",
            "https://example.test:abc",
            "https://example.test:65536",
            "https://999.1.1.1",
            "https://[not-ipv6]",
            "https://2001:db8::1",
            "https://éxample.test",
        ] {
            assert!(
                !policy.allows_headers([origin].into_iter()),
                "unexpectedly accepted {origin:?}"
            );
        }
        assert!(!policy.allows_headers(std::iter::empty::<&str>()));
        assert!(
            !policy.allows_headers(["https://example.test", "https://example.test"].into_iter())
        );
        assert!(WebSocketOriginAllowlist::new(["https://example.test/"]).is_err());
    }

    #[test]
    fn origin_allowlist_handles_ipv6_and_keeps_trailing_dot_distinct() {
        let ipv6 =
            WebSocketOriginAllowlist::new(["https://[2001:db8::1]"]).expect("valid IPv6 origin");
        assert!(ipv6.allows_headers(["https://[2001:0db8:0:0:0:0:0:1]:443"].into_iter()));
        assert!(!ipv6.allows_headers(["https://[2001:db8::2]"].into_iter()));
        assert!(WebSocketOriginAllowlist::new(["https://[2001:db8::zz]"]).is_err());

        let plain =
            WebSocketOriginAllowlist::new(["https://example.test"]).expect("valid hostname");
        assert!(!plain.allows_headers(["https://example.test."].into_iter()));
        let rooted = WebSocketOriginAllowlist::new(["https://example.test."])
            .expect("valid absolute hostname");
        assert!(rooted.allows_headers(["https://example.test."].into_iter()));
    }

    #[test]
    fn default_room_policy_is_own_user_only() {
        let alice = WebSocketPrincipal::new("alice").expect("non-empty principal");
        assert!(OwnUserRoomPolicy.permits(&alice, "user:alice"));
        assert!(!OwnUserRoomPolicy.permits(&alice, "user:bob"));
        assert!(!OwnUserRoomPolicy.permits(&alice, "unknown"));
        assert!(!OwnUserRoomPolicy.permits(&alice, "private"));
    }
}
