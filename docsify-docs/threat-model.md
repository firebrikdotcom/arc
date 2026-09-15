# Threat model

Reviewed 2026-09-12 against source revision `c6cc0d7736dfabccb9434890b58983aa545e5a8e`
(v0.8.6). Source links below are pinned to that revision; reassess changed controls before each
release. This is a source review, not a penetration test or a claim of legal compliance.
Existing guides' regulatory labels describe design intent, not certification.
Use the [release security checklist](release-security-checklist.md) to record deployment evidence.

## Coverage update (2026-09-13)

See [executable coverage](security-test-coverage.md) for verified controls and explicit
open-gap reproductions. TM-03 missing-jti and missing-store paths are fixed in this
branch and tested over HTTP; the source-pinned assessment below describes the
original reviewed revision. Other risks remain open.

## Scope and assets

Covers the reusable framework, thin app, optional auth plugins, generated apps, SQLite/Postgres,
and the NATS → Benthos → external-handler topology. The deployed network, secret management,
backup system, handler implementations and operator permissions are unknown and must be verified.

| Asset | Why it matters | Source evidence |
|---|---|---|
| Password hashes, identities, roles and browser cookies | Account takeover and privilege escalation | [Session cache](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/helpers/session.rs), [plugin identity store](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-auth-db/src/lib.rs) |
| JWT signing key, session records, cookie key, integrity key and projection bearer token | Each grants a different impersonation or forgery capability; keep separate | [Runtime wiring](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/commands/serve.rs#L140), [configuration](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/helpers/config.rs) |
| Event payloads and audit metadata | Authoritative business history, potentially personal data and credential hashes | [Event](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-core/src/event.rs), [User events](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-app/src/domain/user/events.rs) |
| Snapshots, projections and identity tables | Drive reads, reconstruction and authentication; corruption can alter decisions | [Command bus](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-core/src/command_bus.rs), [User projector](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-app/src/domain/user/projector.rs) |
| JetStream messages, DLQ payloads, logs and backups | Additional copies of sensitive history; availability and confidentiality obligations remain | [Publisher](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-es-nats/src/lib.rs), [generated pipeline](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/config/benthos/generated/events.yaml) |
| Plugins, generator templates, dependencies and release credentials | Trusted executable supply chain affects every generated application | [Plugin interface](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/lib.rs#L157), [CLI](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-cli/src/resource.rs), [release workflow](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/.github/workflows/release.yml) |

## Actors and trust boundaries

Consider anonymous Internet clients, authenticated users attempting cross-user access, stolen
cookies/tokens, compromised handlers, malicious dependencies/plugins, database writers without
application keys, and operators with infrastructure or key access. An application-host or key
compromise exceeds the protection offered by application-level HMACs.

| Boundary | Data and authority crossing it | Required trust decision |
|---|---|---|
| Browser/API → edge → Arc | Forms, cookies, bearer tokens, forwarded IP headers | Authenticate each route; authorize each object/action; constrain proxy headers, request sizes and origins |
| Arc → persistence | Commands, events, snapshots, identity and session writes | Least-privilege DB access; protect backups and the separate SQLite session DB used with Postgres |
| Arc → NATS → Benthos | Full serialized events | Authenticate broker clients, encrypt transport and scope subject permissions; message shape is not proof of origin |
| Benthos → Arc projection endpoint / external handler | Event body and delivery credentials | Treat handlers as separate principals; restrict destinations and prevent projection forgery |
| Plugins/templates → application runtime | Routes, shared state, setup hooks, generated authorization | Plugins execute trusted in-process code; generated applications need their own security review |
| Build/release/operator → deployment | Binaries, assets, config, secrets and migrations | Review provenance, configuration, rollback and recovery; CI success is not production validation |

[ADR 0001](architecture-decisions/0001-benthos-only-event-routing.md) assigns routing to Benthos
and prohibits direct Arc database outputs. [ADR 0002](architecture-decisions/0002-framework-upgrade-contract.md)
defines framework versus application ownership. Current code takes precedence over older planning text.

## Browser-session mitigation update (2026-09-14)

The optional auth plugins now validate durable browser handles on protected HTTP
requests. Role/active/password changes, logout and detected idle expiry revoke
handles; replay remains denied after roles/accounts are restored. Store outages
fail closed. See [configuration and boundaries](auth-plugins.md#browser-session-invalidation)
and [coverage](security-test-coverage.md). TM-01/04 still require HTTPS, cookie scope,
object/tenant authorization and review of application routes outside these plugins.
The source-pinned observations below describe the earlier reviewed snapshot.

## Threats, controls and residual risks

The following are source-observed controls and open risks, not completed mitigations. **High**
means a release decision is required wherever the affected surface is exposed; it is not a CVSS score.

| ID / priority | Threat and observed control | Remaining action / owner |
|---|---|---|
| TM-01 High | Cookie theft/replay: runtime uses `CookieSessionStore`, HttpOnly, configurable SameSite and Secure when `APP_ENV=production`. [Wiring](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/commands/serve.rs#L140) | Operator: require HTTPS, correct environment and narrow cookie domain. App owner: test replay after logout/password change/disable; cached cookies are not the JWT revocation store. |
| TM-02 High | CSRF: helpers generate session-bound tokens and compare them without content-dependent early exit. Protection depends on handlers calling validation. [Helpers](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/helpers/csrf.rs) | App owner: enumerate every state-changing browser route, including plugin forms, and prove missing/wrong-token requests have no effects. SameSite alone is insufficient evidence. |
| TM-03 High | JWT helpers validate signed expiry-bearing claims; registered `jti` values are checked and store errors reject requests. However, missing-store wiring skips revocation, and the no-`jti` branch forwards even with legacy grandfathering disabled. [Middleware](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/http/middlewares/jwt_middleware.rs#L128), [claims](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/helpers/jwt.rs#L79) | Framework owner: make missing-store and missing-`jti` paths fail closed and add negative tests before relying on strict revocation. Keep this release blocker open until fixed or the affected API is disabled. |
| TM-04 High | Privilege escalation: plugins supply identity and explicit role middleware; browser identities/roles are cached. [Session plugin](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-auth-session/src/lib.rs#L20), [RBAC](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-auth-rbac/src/lib.rs#L85) | App owner: enforce object/tenant authorization in addition to roles; test role removal, inactive users, stale cookies and JWT identity resolution. Do not infer per-object isolation from successful authentication. |
| TM-05 High | Plugin compromise: `ArcPlugin` registers builder state/routes and ordered setup hooks. These are ordinary in-process Rust capabilities. [Interface and registration](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/lib.rs#L157) | Maintainer: review dependencies, setup/migrations and route precedence. No sandbox or failure isolation is established by this interface. `arc-auth-db` identity/role writes are conventional DB writes, outside the domain event log. |
| TM-06 High | History tampering: optional HMAC chaining signs event identity/type/payload/sequence/timestamp; audit metadata is explicitly excluded. [Canonical bytes](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-core/src/integrity.rs#L112), [SQLite store](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-es-sqlite/src/lib.rs), [Postgres store](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-es-postgres/src/lib.rs#L357) | Operator: enable and protect the key; test signed reads on the chosen backend. Audit metadata, snapshots, rollback/truncation and a compromised signing key need separate protection and independently retained checkpoints/backups. Key IDs do not establish a complete rotation procedure. |
| TM-07 High | Projection forgery: internal endpoint rejects missing config/token and non-User aggregates, then passes the submitted event to the engine. It does not look up the persisted event or verify an HMAC. [Endpoint](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-app/src/http/controllers/internal_projection_controller.rs#L21) | Operator: isolate endpoint and credential. Framework owner: consider persisted-event verification. A stolen token can submit fabricated User events and alter authentication read models. |
| TM-08 High | Lost/stale/duplicate effects: command bus appends before publishing; these are separate operations. Projector idempotency is a contract, not an automatic guarantee. [Dispatch](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-core/src/command_bus.rs#L332), [projector contract](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-core/src/projection.rs#L170) | Operator/app owner: exercise publish failure after append, replay, duplicates and reordered updates. Establish reconciliation; do not claim atomic append/publish or exactly-once side effects. |
| TM-09 High | Event disclosure and broker injection: publisher sends events to JetStream; generated pipeline validates envelope shape and has an in-memory five-minute dedupe cache. The checked-in generated pipeline currently falls through to stdout and has no projection handler output. [Pipeline](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/config/benthos/generated/events.yaml), [generator](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/scripts/generate-benthos-config.mjs) | Operator: deploy reviewed manifests, verify actual outputs, use NATS authentication/TLS/subject ACLs and restrict DLQ access. Shape checks and dedupe do not authenticate senders. Restarts/TTL expiry permit duplicates; stdout can disclose complete payloads. |
| TM-10 High | Generated-app exposure: API middleware is inserted only for the selected JWT mode. Resource API template writes use `CommandContext::for_actor("anonymous")` and reads list unrestricted rows. [Generator](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-cli/src/resource.rs#L460), [API template](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-cli/templates/resource/api.rs.tpl#L22) | App owner: inspect emitted routes for each selected plugin/auth mode, bind audit actors to verified identity, and implement object access/field filtering. Do not assume all generated APIs are private or audit-attributed. |
| TM-11 High | WebSocket disclosure: `/ws` is mounted outside admin auth; handler accepts an optional user and client-selected room subscriptions. [Routes](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-app/src/routes.rs#L141), [connection](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/websocket/connection.rs) | App owner: disable sensitive broadcasts until handshake/origin and room authorization are verified, including anonymous and cross-user subscriptions. |
| TM-12 Medium | Abuse/XSS/data leakage: in-memory rate limiter keys on reported client IP; templates and static routes remain app-owned. [Limiter](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/http/middlewares/rate_limit_middleware.rs#L60), [static handler](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-app/src/routes.rs#L19) | Operator/app owner: constrain forwarded headers, test multi-instance limits, escaped rendering, traversal, errors, sensitive caching and request/body limits. Security headers/CSP remain roadmap work; test the actual edge response. |
| TM-13 High when audit persistence is required | Access-log types and failure policies exist, but runtime constructs `NoOpAccessLogger`. [Runtime](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/commands/serve.rs#L192), [read discipline](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/crates/arc-web/src/helpers/access_log.rs) | App owner/operator: wire and verify a persistent sink, access controls, retention and failure behavior. A successful read does not prove an audit record was retained. |
| TM-14 High | Deployment/supply-chain compromise: Compose has development environment, example credentials, host-published service ports and mutable image tags. Dockerfile uses old resource paths and permits frontend build failure with `|| true`. [Compose](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/docker-compose.yml), [Dockerfile](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/Dockerfile) | Release owner: validate the actual image and assets; require secret replacement, private service networking, least privilege and tested recovery. Do not treat the reference Compose file as a hardened deployment. |

## Verification limits and maintenance

Existing tests are useful evidence for individual controls, not proof that every deployment wires
those controls correctly. See [testing](testing.md), [browser tests](e2e.md),
[session revocation](session-revocation.md), [integrity](integrity-chain.md) and
[DLQ/redrive](benthos-dlq-redrive.md). The checklist distinguishes existing commands from manual
adversarial cases still required. Revisit this model for auth/plugin changes, new event fields,
new handlers, generator changes, key changes or altered deployment boundaries.

Unresolved production decisions: public routes and data classification; tenant/object policy;
acceptable stale-session window; broker/handler identity; audit retention; key rotation;
recovery objectives; dependency exception owners and expiry. Assign owners before release.
