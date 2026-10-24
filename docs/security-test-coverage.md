# Security test coverage

Original coverage verified 2026-09-13 in `security/threat-model-20260912`; browser-session
regressions added 2026-09-14 in `security/session-invalidation-20260914`. This matrix supplements the
[threat model](threat-model.md). Passing a test named **GAP** confirms a reproducible
weakness; it does not certify a mitigation or approve a release.

| Threat | Executable coverage | Result and limits |
|---|---|---|
| TM-03 JWT | `crates/arc-web/tests/security_http.rs::jwt_http_security_contract` | Real TCP HTTP: valid token; absent jti/store; revoked/unknown session; malformed, expired and wrong-key tokens; explicit legacy opt-in. Denials assert zero downstream calls and no protected body. Missing-jti/store defects reproduced before fix, then fixed (401/503). |
| TM-04 object access | `crates/arc-app/src/http/controllers/profile_security_test.rs` | Real profile handler through JWT: caller cannot select another user's profile or obtain password fields. This covers the profile endpoint, not all application resources or tenants. |
| TM-01/04 browser identities | `tests/security/browser.spec.mjs` | Chromium with real session/RBAC middleware and SQLite identities: fresh JWT roles reject role removal/disable; browser handles are revoked even after role/account restoration. Saved logout and idle-expiry cookies cannot replay; independent sessions remain valid; store outages deny access and recover. |
| TM-01/04 store and middleware | `arc-auth-db::browser_tests::migration_upgrade_and_durable_invalidation`; `arc-auth-session::tests::cached_identity_cannot_authorize_without_a_validated_handle` | SQLite upgrade/idempotence, persisted handles across store instances, role/active/password invalidation, logout isolation and expiry; cached-only cookies and missing store wiring deny access. |
| TM-07 projection | `crates/arc-app/tests/projection_security_http.rs`; `ProjectionEngine::process_external_delivery` | Real HTTP rejects missing/wrong credentials without mutation. Delivery now requires the exact event from bounded typed lookup in the authoritative store; absent or modified envelopes are rejected before projection. Tests cover a forged aggregate ID, payload, timestamp, and audit metadata with no mutation, then a valid persisted event and idempotent duplicate delivery. Stores without exact lookup support fail closed. |
| TM-07 PostgreSQL event payloads | `arc-es-postgres` event schema and forward migration | Event payloads use `JSON` to preserve serialized numeric representations; read-model `JSONB` stays unchanged. The idempotent forward migration and legacy signed-row upgrade passed the PostgreSQL 16 suite (26/26), including integrity off/on, numeric round-trips, idempotency, tamper rejection/no mutation, and numeric command→publish→external projection. `bash tests/bounded-reads/run.sh` also exited 0 with PostgreSQL-enabled Rust tests, generated fixture 5/5, and browser workflow. Local evidence only; no external migration application is claimed. Historic signatures invalidated by prior normalization stay fail-closed and are never resealed. |
| TM-08 delivery | `arc-core::command_bus::tests::publish_failure_preserves_event_for_explicit_recovery` | Unit fault injection proves append survives publish failure and explicit replay preserves event ID without another append. It is not automatic outbox recovery or crash durability. |
| TM-08/09 distributed routing | `crates/arc-app/tests/benthos_projection_routing.rs` | Existing real NATS → Redpanda Connect → Arc projection test passed with actual binaries, without prerequisite skips. Broker crash/restart recovery remains untested by this check. |
| TM-11 WebSockets | `tests/security/browser.spec.mjs` | The current browser suite checks handshake origin and authenticated identity, rejects cached-only identity, verifies own-user room membership, and confirms user/room broadcasts stay isolated. The authorization tests assert the secure behavior directly; they are not expected-failure probes. Revocation after a socket has already upgraded remains a known limitation. See [WebSocket security](websocket-security.md). |
| TM-13 access audit | `crates/arc-web/tests/security_http.rs::audit_http_failures_and_stalled_sink_gap` | Real TCP with controlled sink: PHI/PCI errors return 503 without payload; PII fails open; stalled PHI request stays pending until released. The 100ms observation is not a production timeout policy. |
| TM-13 durability | SQLite sink unit tests, `durable_access_http`, `tests/durable-audit/` browser fixture | Dedicated commit persistence, reopen, lock/disk PHI rejection, overload and bounded retention hook. Browser process-restart receipts are verified separately. Deployment backup restore, disclosure inventory and retention policies remain open. |

## Repeatable normal checks

From the repository root, install frontend dependencies with `npm ci` and provide
Docker Compose and host Chromium (`CHROMIUM_PATH` can override `/usr/bin/chromium`).
Run `bash tests/security/run.sh`. It builds/tests backend code only in Compose and
runs Chromium against the localhost-only, explicitly opted-in fixture. Set
`ARC_SECURITY_PORT` to a free port if 18764 is occupied. Its trap removes the scoped
container and network on exit. Build output remains under `target/security-coverage`.
The fixture has deliberately privileged test controls: never deploy it.

The runner does not provision NATS/Redpanda Connect. To repeat the distributed check,
provide `nats-server`, `node` and `redpanda-connect` inside your Compose verifier and run
`cargo test --locked -p arc --test benthos_projection_routing -- --nocapture` there.
Inspect output: the existing test can skip when tools are missing. This review used
NATS 2.14.1 and the Redpanda Connect container binary and observed actual delivery.

## Historical gap probes (2026-09-14)

At the time of the 2026-09-14 v0.8.8 integration, these were separate,
intentionally failing probes for controls that had not yet been implemented.
The dated results below are retained as historical evidence; they do not
describe the current WebSocket assertions. The current browser suite exercises
the implemented handshake and room controls as normal passing criteria. TM-07
projection-origin validation is separately implemented and covered by the HTTP
tests recorded in the current matrix.
The commands run for that historical probe (retained for context, not as current
test instructions) were:

```sh
ARC_SECURITY_CONTRACTS=1 npx playwright test --config tests/security/browser.config.mjs
```

Inside the verifier run:

```sh
ARC_SECURITY_CONTRACTS=1 cargo test --locked -p arc --test projection_security_http
```

Observed then: five browser failures (cached role, disabled identity, saved logout
cookie, anonymous socket, cross-user room) and one forged-projection failure.
The socket failures were later replaced by passing positive handshake and room
authorization checks. This historical run must not be used to describe the current
WebSocket behavior or to infer that other gaps were fixed.

## Verification evidence and scope

All-feature tests for `arc-core`, `arc-web`, and `arc-auth-rbac` passed.
Core tests: 122 passed; runtime unit tests: 27 passed; core doctests: 16 passed,
12 previously ignored. New HTTP matrix tests, projection and profile tests passed.
Chromium: five passed in normal mode. All-target/all-feature Clippy for `arc-core`,
`arc-web`, `arc-auth-rbac` and `arc` passed with warnings denied. The real routing test
passed. Full workspace CI, Postgres deployment, cross-browser coverage and all
threat-model mitigations were not claimed. Test helpers are crate-local so published
crate tests do not depend on workspace-relative helper paths.

The checked-in runner was syntax/configuration checked; its component commands were
executed in the scoped verifier, rather than rerunning the entire runner on a cold image.

## Master integration validation — v0.8.7 (2026-09-13)

The four security workstreams are merged into `master`. Verification runs in a
project-scoped Docker Compose environment with Rust 1.90, PostgreSQL 16, a real
NATS server and Redpanda Connect, and Chromium. Workspace artifacts were rebuilt
before the combined test run.

- `cargo test --locked --workspace --all-features`: 294 passed; 12 existing
  documentation examples remain ignored. The Postgres test database was configured,
  and the NATS/Benthos routing prerequisites were present.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  and `cargo fmt --all -- --check`: passed.
- Library documentation with `RUSTDOCFLAGS='-D warnings'`, Docsify freshness,
  the architecture drift guard, Benthos generator tests/freshness/lint, and the
  Vite production build: passed.
- Application Playwright suite: 14 passed, including the Vite assets, immutable
  caching/304 responses, Turbo navigation and Toastify regression.
- Fresh minimal and UI scaffolds: five generated tests each, formatting, Clippy,
  setup idempotence, API persistence, auth/CSRF, browser CRUD and header enforcement
  passed through `scripts/check-arc-scaffold.sh`.
- Threat-model Chromium suite: five passed in characterization mode. The separate
  desired-security assertions above continue to expose unresolved controls.

Older API fixtures now register their JWT sessions before asserting authenticated
profile and audit behavior. The E2E launcher honors `CARGO_TARGET_DIR` and an
optional system Chromium executable. This is local integration evidence; it does
not claim remote CI, package publication, or resolution of all threat-model risks.

## Master integration validation — v0.8.8 (2026-09-14)

Three further security workstreams — bounded collection reads, the durable SQLite
read-audit journal, and durable browser-session invalidation — were merged one at a
time, each with an explicit merge commit and a full cumulative verification pass in
a project-scoped Docker Compose environment (Rust 1.90, PostgreSQL 16, a real NATS
server and Redpanda Connect, Chromium).

- `cargo test --locked --workspace --all-features`: 301 passed, 0 failed; the 12
  existing documentation examples remain ignored. Postgres was configured and the
  NATS/Benthos routing prerequisites were present, so no prerequisite skips applied.
- `cargo test --locked --workspace --doc`: 20 passed, 12 existing ignored examples.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
  `cargo fmt --all -- --check`, `cargo doc --locked --workspace --no-deps
  --all-features`, and debug and release builds: passed.
- Docsify freshness, the architecture drift guard, Benthos generator
  tests/freshness/lint, the Vite production build and `npm audit`: passed.
- Application Playwright suite: 14 passed. Fresh minimal and UI scaffolds passed
  through `scripts/check-arc-scaffold.sh`.
- Bounded-collections harness: 202 focused storage/doc tests, a freshly generated
  application with its own tests and static checks, and the Chromium API/browser/
  admin navigation suite.
- Durable-audit harness: both real Chromium phases, proving a committed receipt
  survives replacement of the serving process.
- Threat-model Chromium suite: 11 passed at that revision. Nine are positive
  browser/real-TCP session checks; the remaining two were **characterizations of
  open WebSocket gaps at that time** and asserted then-current behavior.
- Dependency audit: exit 0 against the pre-existing workflow exceptions only.

`tests/durable-audit/run-browser.sh` and `tests/security/run.sh` are now required
CI jobs alongside the bounded-collections job, so these regressions are enforced
rather than run by hand.

Behavioral note for non-browser clients: signing in now clears and renews the
session to defeat session fixation, so a CSRF token minted **before**
authentication is no longer valid afterwards. Browsers handle this transparently
because they store the refreshed session cookie. Scripted clients must read the
CSRF token from a page fetched *after* signing in and must persist the updated
session cookie from that response; `scripts/check-arc-scaffold.sh` was corrected
to do so.

At that revision, this was local integration evidence plus the CI gates it installed;
it did not claim package publication or resolution of the then-open room-authorization,
projection-origin, or deployment disclosure/retention items. The later WebSocket
status is recorded below; the other items are not changed by that update.

## WebSocket authorization update (2026-10-24)

The current WebSocket feature adds handshake authentication and origin checks, a
default own-user room policy, and browser coverage for identity isolation and
denied room joins. See [WebSocket security](websocket-security.md) and the
[threat-model mitigation update](threat-model.md#websocket-authorization-mitigation-update-2026-10-24).
Authorization is checked at handshake; an already-upgraded socket is not
revalidated or closed when its identity is revoked or disabled. Treat that as an
open residual risk. The WebSocket harness passed 11 Playwright tests, Rust tests,
Clippy, and build checks locally; this does not claim deployment readiness or
replace required hosted CI.

## Projection-origin validation update (2026-10-24)

The internal projection endpoint now requires an exact event returned by bounded
typed lookup in the authoritative event store, compares the complete envelope,
and projects the stored event. Missing, forged, modified, and unsupported lookup
cases fail closed without read-model mutation. The focused real-HTTP test also
checks valid persisted delivery and idempotent duplicates. See the [projection
boundary implementation](event-handlers.md#projection-delivery) and the current
TM-07 row above. Local verification passed at commit `596cc81`; required hosted
CI and final integration remain separate gates.

## Bounded collection follow-up (2026-09-14)

Storage and generated collection regression coverage is documented in
[bounded collections](bounded-collections.md). Other known-gap characterization
checks remained unchanged at that time; collection hardening did not resolve
session, socket, audit, or projection-origin gaps then. Later WebSocket and
projection-origin status is recorded above.

## Dependency security gate (2026-09-14)

All three 2026-09-14 security workstreams carried the same dependency fix: the
audit detected RUSTSEC-2026-0285 in the existing `rustls` pin, and Cargo.lock now
pins the fixed patch 0.23.45. Cargo Audit passes using only the pre-existing
workflow exceptions (RUSTSEC-2026-0258 and RUSTSEC-2023-0071); no exception was
added for this advisory.
