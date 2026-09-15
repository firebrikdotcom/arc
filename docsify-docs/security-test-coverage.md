# Security test coverage

Verified 2026-09-13 in `security/threat-model-20260912`. This matrix supplements the
[threat model](threat-model.md). Passing a test named **GAP** confirms a reproducible
weakness; it does not certify a mitigation or approve a release.

| Threat | Executable coverage | Result and limits |
|---|---|---|
| TM-03 JWT | `crates/arc-web/tests/security_http.rs::jwt_http_security_contract` | Real TCP HTTP: valid token; absent jti/store; revoked/unknown session; malformed, expired and wrong-key tokens; explicit legacy opt-in. Denials assert zero downstream calls and no protected body. Missing-jti/store defects reproduced before fix, then fixed (401/503). |
| TM-04 object access | `crates/arc-app/src/http/controllers/profile_security_test.rs` | Real profile handler through JWT: caller cannot select another user's profile or obtain password fields. This covers the profile endpoint, not all application resources or tenants. |
| TM-01/04 browser identities | `tests/security/browser.spec.mjs` | Chromium with real session/RBAC middleware and SQLite identities: fresh JWT roles reject role removal/disable; cached browser identities still accept them (GAP). Idle expiry purges identity; a saved pre-logout cookie remains replayable (GAP). |
| TM-07 projection | `crates/arc-app/tests/projection_security_http.rs` | Real TCP: absent/wrong credentials reject without projection mutation; duplicate event leaves the same row; valid bearer can inject an event absent from the event store (GAP). |
| TM-08 delivery | `arc-core::command_bus::tests::publish_failure_preserves_event_for_explicit_recovery` | Unit fault injection proves append survives publish failure and explicit replay preserves event ID without another append. It is not automatic outbox recovery or crash durability. |
| TM-08/09 distributed routing | `crates/arc-app/tests/benthos_projection_routing.rs` | Existing real NATS → Redpanda Connect → Arc projection test passed with actual binaries, without prerequisite skips. Broker crash/restart recovery remains untested by this check. |
| TM-11 WebSockets | `tests/security/browser.spec.mjs` | Real Chromium WebSockets/Arc actors: user-targeted broadcasts isolate identities, but anonymous private-room subscriptions and cross-user room delivery succeed (GAP). |
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

## Explicit desired-security assertions

These are separate, intentionally failing probes for unresolved controls. With the
fixture running, run:

```sh
ARC_SECURITY_CONTRACTS=1 npx playwright test --config tests/security/browser.config.mjs
```

Inside the verifier run:

```sh
ARC_SECURITY_CONTRACTS=1 cargo test --locked -p arc --test projection_security_http
```

Observed: five browser failures (cached role, disabled identity, saved logout cookie,
anonymous socket, cross-user room) and one forged-projection failure. Normal mode
passes by explicitly asserting and naming the current gaps. No tests silently skip
these risks. Security-policy decisions and implementations remain required before
these desired assertions can become release gates.

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

## Bounded collection follow-up (2026-09-14)

Storage and generated collection regression coverage is documented in
[bounded collections](bounded-collections.md). Other known-gap characterization
checks remain unchanged; collection hardening does not resolve session, socket,
audit, or projection-origin gaps.

The 2026-09-14 dependency audit also detected RUSTSEC-2026-0285 in the
existing rustls pin. Cargo.lock now pins the fixed patch 0.23.45. The audit
passes with the existing workflow exceptions unchanged.
