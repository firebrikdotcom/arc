# Server-Side JWT Sessions (HIPAA-4)

`SessionStore` makes JWTs revocable. Without it, a stolen token stays valid
until natural expiry (typically 24h) and the framework cannot prove a user
is "logged out". HIPAA §164.312(d) requires immediate session revocation
on breach response — this trait is the hook.

Optional browser auth plugins use a separate durable handle table owned by
`arc-auth-db`; see [browser session invalidation](auth-plugins.md#browser-session-invalidation).
Browser logout and role/account changes do not revoke JWT records through this table.

## Flow

Three integration points:

1. **Login** (`POST /api/v1/login`) — `create_token` returns `(token,
   jti)`. The controller calls `session_store.record_session(...)` before
   returning the token. If recording fails, the controller returns **503**
   instead of handing out an unrevocable token.
2. **Per-request validation** — `JwtMiddleware` decodes the token, then
   calls `session_store.is_valid(jti, now_us)`. False → 401. Sink error →
   503 (fail closed; the only safe default for a security control).
3. **Logout** (`POST /api/v1/protected/logout`) — `session_store.revoke(jti)`
   then 204. Subsequent requests with that token return 401.

## Trait surface

```rust
trait SessionStore {
    async fn record_session(&self, record: SessionRecord) -> Result<(), SessionStoreError>;
    async fn is_valid(&self, jti: Uuid, now_us: i64) -> Result<bool, SessionStoreError>;
    async fn revoke(&self, jti: Uuid, now_us: i64) -> Result<(), SessionStoreError>;
    async fn revoke_all_for_actor(&self, actor_id: &str, now_us: i64) -> Result<usize, SessionStoreError>;
    async fn prune_expired(&self, now_us: i64) -> Result<usize, SessionStoreError>;
}
```

Implementations:

- `InMemorySessionStore` — `Arc<Mutex<HashMap>>`. Behind `test-utils`
  feature; usable as a single-node deployment too.
- `SqliteSessionStore` — durable, in `arc-es-sqlite`. Indexed on
  `actor_id` (powers `revoke_all_for_actor`) and `expires_at_us` (powers
  `prune_expired`). PK on `jti` covers `is_valid` / `revoke`.

## Failure semantics

This is the **opposite** of `AccessLogger`'s `FailOpenWarn` default:

| Concern | When the sink fails |
|---|---|
| `AccessLogger::log_access` for `Sensitivity::Pii` and below | warn + continue |
| `AccessLogger::log_access` for `Sensitivity::Phi`/`Pci` | fail hard (HIPAA-2a) |
| `SessionStore::is_valid` | **always** fail closed (503) |
| `SessionStore::record_session` at login | **always** fail closed (503) |
| `SessionStore::revoke` at logout | warn-and-continue (idempotent goal) |

Revocation is a security control — when the store can't prove a token is
*not* revoked, the only sound default is to refuse the request.

## Schema

Migration `2026-04-26-000002_create_jwt_sessions`:

```sql
CREATE TABLE jwt_sessions (
    jti TEXT NOT NULL PRIMARY KEY,
    actor_id TEXT NOT NULL,
    created_at_us BIGINT NOT NULL,
    expires_at_us BIGINT NOT NULL,
    revoked_at_us BIGINT
);
CREATE INDEX idx_jwt_sessions_actor_id ON jwt_sessions(actor_id);
CREATE INDEX idx_jwt_sessions_expires_at ON jwt_sessions(expires_at_us);
```

## Grandfather mode

Tokens minted before HIPAA-4 landed have no `jti`. The middleware refuses
them by default. Set `JWT_GRANDFATHER_LEGACY=true` to accept them during a
rollout window — every accept logs a `tracing::warn!`. Remove the env flag
once existing tokens have churned out.

## Operations

- **Pruning**: call `store.prune_expired(now_us).await` on a schedule.
  Today this is dev-only; production should run a tokio interval task or
  invoke the SQL via cron.
- **Breach response**: `revoke_all_for_actor("alice-uuid")` invalidates
  every active session that user holds. Wire to an admin endpoint when
  one exists; plan currently flags this as a follow-up.
- **Cache layer**: not implemented yet. Per-process LRU is the obvious
  next step — every request hits the store today; the in-memory variant
  is fast enough for single-node, the SQLite variant fast enough for
  ~1k req/s. Bigger deployments add caching alongside Step 5's Postgres
  store.

## Strict middleware wiring

JWT middleware requires a configured `SessionStore`; missing wiring returns 503 before
calling the protected service. Missing `jti` returns 401 by default. The explicit
`JWT_GRANDFATHER_LEGACY` opt-in permits legacy tokens only when store wiring exists.
Revoked or unknown sessions reject with 401; store failures reject with 503. See
[security coverage](security-test-coverage.md) for real HTTP regression cases.
