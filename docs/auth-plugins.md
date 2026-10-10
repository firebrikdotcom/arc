# Authentication Plugins

Arc authentication is optional and composable. Generated applications install capabilities with
`arc plugin add`; `arc-web` supplies shared middleware and extension contracts but does not choose
an identity provider or authorization policy.

## Install authentication

Run plugin commands from a generated application root:

```bash
arc plugin add auth-db-session
arc plugin add auth-db-jwt
make setup
```

The bundles are idempotent and may be combined:

| Bundle | Installed capabilities |
|---|---|
| `auth-db-session` | `auth-db`, `auth-session`, `auth-admin`, `auth-rbac` |
| `auth-db-jwt` | `auth-db`, `auth-jwt`, `auth-rbac` |

Individual capability names also work with `arc plugin add`.

On an empty identity database, interactive `make setup` prompts for the first administrator's name,
email, and password. For noninteractive setup, provide `ARC_SETUP_ADMIN_NAME`,
`ARC_SETUP_ADMIN_EMAIL`, and `ARC_SETUP_ADMIN_PASSWORD`.

## Protect a generated resource

Install authentication before generating the resource:

```bash
arc generate resource Product --api --ui \
  --api-auth jwt --ui-auth session --roles admin,user
make migrate
```

- `--api` generates JSON CRUD endpoints.
- `--api-auth jwt` requires a valid bearer token.
- `--ui` generates browser collection, detail, create, and edit pages.
- `--ui-auth session` requires a signed-in browser session.
- `--roles admin,user` accepts an active identity with either role.

When `arc-auth-session` is installed, generated browser resources are session-protected
automatically. API authentication remains explicit: installing `arc-auth-jwt` does not wrap every
`/api` route.

## `arc-auth-core`

Provider-neutral contracts with no routes, schema, or UI:

- `Identity` stores ID, name, email, active state, and role names.
- `IdentityStore` authenticates and manages identities.
- `AuthorizationPolicy` decides whether an identity is authorized.
- `AuthError` supplies stable authentication/store failure categories.

Implement these contracts for custom identity providers or authorization policies.

## `arc-auth-db`

The SQLite identity provider owns conventional `users`, `roles`, and `user_roles` tables. This
plugin-owned schema is separate from event-sourced application aggregates.

| Variable | Purpose |
|---|---|
| `DATABASE_DRIVER` | Must currently be `sqlite`. |
| `DATABASE_URL` | SQLite identity database path. |
| `ARC_SETUP_ADMIN_NAME` | Noninteractive first-admin name. |
| `ARC_SETUP_ADMIN_EMAIL` | Noninteractive first-admin email. |
| `ARC_SETUP_ADMIN_PASSWORD` | Noninteractive password. |

Passwords use Argon2. Arc does not impose a length rule, so local development bootstrap and CLI
registration can use a deliberately simple password; choose a strong unique password for any
deployed application. Emails are normalized and unique. The final active administrator cannot be
deactivated or stripped of the `admin` role.

## `arc-auth-session`

The browser protocol caches the role-bearing identity and Arc's standard `SessionUser`.
`RequireSession` redirects unauthenticated requests to `/signin`. Configure inactivity with
`SESSION_IDLE_TIMEOUT_SECS` (default `900`); optional cookie settings include `SESSION_DOMAIN` and
`SESSION_SAME_SITE`.

The session plugin owns protocol and middleware, not pages. `arc-auth-admin` supplies the UI.

## `arc-auth-admin`

This plugin contributes sign-in, profile, password, and user-management pages to the application's
UI host.

| Route | Access |
|---|---|
| `GET/POST /signin` | Public; POST requires CSRF. |
| `POST /signout` | CSRF-protected. |
| `GET/POST /admin/profile` | Session plus idle timeout. |
| `POST /admin/profile/password` | Session, idle timeout, and CSRF. |
| `/admin/users/*` | Session, idle timeout, and `admin` role. |

It also contributes Profile and Users navigation and the Sign out action. The generated application
owns the surrounding layout and assets. Successful profile, password, and user-management changes
redirect to their destination with a one-time accessible success notice; invalid submissions render
an inline error instead. Admin pages supply a breadcrumb trail to the application-owned layout:
the dashboard is the root, while Profile begins `Home → Profile` and user pages begin `Home → Users`.
Each trail has linked ancestors and one `aria-current="page"` item.

## `arc-auth-jwt`

`POST /api/session` accepts JSON credentials and returns a revocable bearer token:

```json
{"email":"admin@example.com","password":"your-password"}
```

Send the result as `Authorization: Bearer <token>`.

| Variable | Purpose |
|---|---|
| `JWT_SECRET` | Required signing secret; setup initializes it. |
| `JWT_EXPIRY_HOURS` | Token lifetime; generated default is `24`. |
| `JWT_GRANDFATHER_LEGACY` | Opt-in acceptance of legacy tokens without session records. |

Issued token IDs are recorded in the configured `SessionStore`; middleware rejects revoked records.

## `arc-auth-rbac`

`SimpleRbac` permits an active identity when any required role matches exactly. `RequireRoles`
resolves the identity from the browser session or the actor ID installed by JWT middleware. A failed
role check returns `403 Forbidden`.

`ADMIN` and `USER` are convenience names, not a complete role catalog.

## Current policy limitations

Roles are currently database rows assigned to identities. Route requirements are generated into
Rust configuration. Arc does **not yet** load role definitions, permissions, or action policies from
JSON/YAML manifests, and it does not expose commands for managing such policy manifests. The
built-in user-management pages currently require the literal `admin` role.

Authentication plugins therefore provide identity, transport authentication, and the current role
guard; they do not yet make every application action dynamically configurable.

## Browser session invalidation

`arc-auth-session` requires durable browser-session support from its `IdentityStore`.
`arc-auth-db` installs the identity tables first, then the idempotent
`90000000000001_browser_sessions` migration during setup. Existing databases must
run setup before serving the updated plugins. Pre-migration cookies require a new sign-in.
Custom stores must implement `authenticate_browser`, `browser_identity`, and
`revoke_browser_session`; the default implementations deny access.

Each successful login creates a random server-side handle. `RequireSession` checks
that handle and the current active identity on every protected request, then supplies
the validated identity to RBAC. Generated admin and resource routes already use
this guard. Role changes, disable/enable operations and password changes delete all
of the account's browser handles atomically with the identity write. Restoring a
role or re-enabling an account never restores old cookies. Logout revokes only the
current handle before clearing the cookie; other sessions remain valid. It remains
a CSRF-protected POST. Re-authentication rotates the previous handle.

`ARC_BROWSER_SESSION_TTL_SECONDS` configures both the runtime cookie lifetime and
the server-side absolute deadline (positive integer; default 86400 seconds, matching
the existing 24-hour cookie lifetime). Cookie refresh cannot extend the server-side
absolute deadline. `SESSION_IDLE_TIMEOUT_SECS` remains a separate inactivity limit:
activity refreshes it; detected idle expiry revokes the handle before purging the
cookie. A previously saved cookie cannot resurrect that revoked session. A request
already authorized before revocation may finish; subsequent checks deny access.

Store outages fail closed with HTTP 503, including logout and idle revocation;
logout does not claim success before persistence succeeds. Diagnostic events log
only the operation and a fixed message, never credentials, cookies or raw store
errors. Recovery permits still-valid handles to work again. JWT token issuance and
revocation remain separate; JWT RBAC resolves its own actor even when a browser
cookie accompanies the request.

Session rows contain only the random handle, identity ID and expiry. Revocation
deletes rows; login removes expired rows. This is operational authentication state,
not an audit retention policy. Operators still choose deployment timeouts, HTTPS,
cookie scope, database availability/backups and any independent audit retention.
Direct SQL identity mutations must also invalidate affected handles; use the
identity-store methods to preserve the transactional contract. Object/tenant and
WebSocket-room authorization remain application responsibilities. The built-in
projection-backed `arc-app` auth path is separate from these optional plugins.
