# WebSocket security

`/ws` validates the request origin and resolves a current authenticated user
before attempting the WebSocket upgrade. It rejects missing, `null`, malformed,
multiple, and unapproved `Origin` values with 403. A missing provider or
unavailable identity store returns 503; a missing, expired, revoked, or inactive
browser identity returns 401. The cached `SessionUser` is not authentication
evidence. Raw syntax is checked before normalization; only scheme and host case
and default ports are normalized. Paths, including a trailing slash, are
rejected.

Applications must explicitly register an origin allowlist. The default is
empty, and the runtime does not infer origins from `Host`, forwarded headers, or
`APP_URL`:

```rust
use std::sync::Arc;
use arc_web::{ArcApp, websocket::security::WebSocketOriginAllowlist};

let origins = WebSocketOriginAllowlist::new(["https://app.example.test"])?;
ArcApp::builder()
    .register_data(Arc::new(origins))
    // Register the session-auth plugin and application routes as usual.
    ;
```

The session-auth plugin supplies the neutral `WebSocketPrincipalProvider` seam.
It validates the opaque browser-session handle against the current identity
store and requires the identity to remain active. Authentication happens at
handshake time; revoking or disabling an identity does not currently close an
already upgraded socket (TM-11 residual risk).

The default room policy permits only `user:<verified-user-id>`. The server actor
checks this policy before mutating membership, including for direct actor
messages; unknown connection IDs and unknown room names cannot join. Existing
user-targeted broadcasts, heartbeats, unsubscribe, and disconnect cleanup remain
available. Anonymous/public rooms and native clients without an `Origin` are
policy extensions and are not enabled by this feature.
