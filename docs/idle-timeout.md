# Idle Timeout (HIPAA-3)

`IdleTimeoutMiddleware` enforces HIPAA §164.312(a)(2)(iii) Automatic Logoff:
sessions that have been idle longer than the configured limit are purged
and the user is redirected to `/signin?reason=idle`.

## Lifecycle

On every request through a route wrapped with `IdleTimeoutMiddleware`:

1. If the session has no post-cutover `SessionUser` under the `"user"` key,
   pass through unchanged. Stateless API bearers are unaffected — they don't
   carry a session.
2. If `last_active_at` is missing, stamp `now` and continue (first
   authenticated hit after login).
3. If `now - last_active_at > limit`, call `session.purge()` and redirect
   to `/signin?reason=idle`. The user must re-authenticate.
4. Otherwise refresh `last_active_at = now` and continue.

## Configuration

| Variable | Default | Notes |
|---|---|---|
| `SESSION_IDLE_TIMEOUT_SECS` | `900` (15 min) | HHS OCR guidance for PHI systems |

`IdleTimeoutMiddleware::from_env()` reads the variable and falls back to the
default. `IdleTimeoutMiddleware::new(secs)` for explicit values in tests or
custom code.

## Wiring

The `/admin` scope already chains `IdleTimeoutMiddleware` outside
`AuthMiddleware`:

```rust
.service(
    web::scope("/admin")
        .wrap(AuthMiddleware)                       // innermost
        .wrap(IdleTimeoutMiddleware::from_env())    // outer
        .service(admin_controller::dashboard)
        // ...
)
```

Order matters. The idle middleware reads the cached `SessionUser` from the
session's `"user"` key — that slot is populated by the cookie session login
flow before `AuthMiddleware` runs. Wrapping idle on the *outside* means it
sees the session even when `AuthMiddleware` would have redirected; that's the
right semantic — even an "about to be sent to /signin" request gets its idle
status refreshed.

## Notes

- Independent of absolute session lifetime: a 24h cookie that sits idle for
  16 minutes still gets purged.
- Stateless bearers (JWT) are not idle-tracked here. Use HIPAA-4 server-side
  session revocation for breach response on bearer flows.
- The redirect target `/signin?reason=idle` lets the signin page display a
  user-visible "you were logged out for inactivity" message; rendering that
  message is the front-end's job.
