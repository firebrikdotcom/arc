# Release security checklist

Companion to the [threat model](threat-model.md), reviewed 2026-09-12. Copy this checklist into
the release record, with candidate SHA, generated-app version, enabled plugins, DB driver,
event-bus mode, deployment image digest, operator, date and evidence links. Leave items unchecked
until verified. This document records required work; it does not approve a release or certify compliance.

## Release decisions

- [ ] Assign an owner to every applicable TM risk. Record mitigation evidence or a time-limited
  acceptance with rationale and reviewer. Mark a surface not applicable only with proof it is disabled.
- [ ] Resolve TM-03 before claiming strict JWT revocation: missing `jti`, missing store, invalid,
  expired, unknown and revoked tokens must reject without handler side effects; store outage must
  reject. Current source does not meet the missing-`jti`/missing-store conditions.
- [ ] Decide object/tenant authorization and stale-cookie behavior (TM-01/04/10). Test two users
  against each other's IDs for list/get/create/update/delete and role changes.
- [ ] Restrict the projection endpoint and broker credentials (TM-07/09); document how forged
  events are prevented. Do not equate a correct envelope with a persisted, authentic event.
- [ ] Disable sensitive WebSocket delivery until anonymous/cross-user room tests pass (TM-11).
- [ ] If retained access evidence is required, replace and test the no-op logger (TM-13).

## Authentication and browser evidence

- [ ] Inventory public, session, JWT and internal routes from the candidate's actual route table.
  Include every plugin and generated resource; prove anonymous access is rejected where required.
- [ ] Test missing/wrong CSRF tokens on every mutating form, including sign-in, profile, password,
  user/role management and sign-out where applicable; confirm no writes occurred.
- [ ] Verify production cookies in the browser: Secure, HttpOnly, selected SameSite, domain/path,
  idle timeout and expiry. Test replay of a saved cookie after sign-out, password change and disable.
- [ ] Check generated passwords use the intended Argon2 flow; remove bootstrap/demo credentials.
  Development password permissiveness is not a production password policy.
- [ ] Verify TLS, redirects, response headers/CSP, error redaction, escaping and sensitive cache
  behavior at the deployed edge. Test hostile forwarded IP headers and login abuse across replicas.

## Events, plugins and data operations

- [ ] Review plugin code/dependencies/setup migrations and registration order; test least-privilege
  roles. Plugin presence alone does not protect routes or provide tenant isolation.
- [ ] Enable event integrity where required and test payload/signature tampering and unsigned-row
  rejection on the selected backend. Record treatment of unsigned historical rows before enabling it.
- [ ] Protect audit metadata, snapshots and backup/checkpoint copies separately. Rehearse restoration,
  key loss/rotation and rollback detection; never reseal suspect history without investigation.
- [ ] Test append-success/publish-failure recovery, projection rebuilds, duplicates, reordering,
  handler outages, poison events, DLQ retention and redrive. Confirm final state and side effects.
- [ ] Inspect deployed generated Benthos output after `make benthos-config-check`; the reference
  artifact has stdout fallback. Verify intended HTTP/NATS destinations, credential handling,
  payload minimization and absence of SQL/database outputs.
- [ ] Apply broker TLS/authentication and publish/consume/DLQ subject ACLs; keep broker monitoring,
  Benthos metrics and internal projection routes private. Validate denied clients and destinations.
- [ ] Review event/DLQ/log/backup data exposure and retention. Verify Postgres deployments persist
  and protect the separate SQLite JWT-session database, including multi-instance consistency.

## Existing automated checks

Commands below are defined in the [Makefile](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/Makefile),
[package scripts](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/package.json),
[CI](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/.github/workflows/ci.yml)
and [Security workflow](https://github.com/lotharthesavior/arc/blob/c6cc0d7736dfabccb9434890b58983aa545e5a8e/.github/workflows/security.yml).
Record exact invocation, exit status, test counts and skips. Run backend tests and static analysis
inside a Docker Compose verification service with the candidate toolchain and dependencies,
unique `arc-nineties-<lane>` project/container names, `arc.project=nineties` labels, isolated
DB/storage and cleanup on success/failure/timeout. The reference runtime image is not a test service.
Do not start the shared reference stack or reuse another lane's volumes/ports.

| Check | Existing command to run in the appropriate environment | Required evidence |
|---|---|---|
| Canonical docs and structural drift | `make docs-check`; `make doctor` | Both pass; documentation matches source |
| Formatting and static analysis | `make format-check`; `make lint` | Formatting and all-target/all-feature Clippy pass |
| Backend regression | `make test` | Workspace/all-feature tests pass; count service-dependent skips explicitly |
| Focused core security contracts | `cargo test --locked -p arc-core` | Integrity, audit, command and projection tests pass; not a substitute for HTTP tests |
| Projection boundary | `cargo test -p arc --all-features internal_projection_controller` | Auth rejection and Arc-owned projection tests pass |
| Distributed routing | `cargo test -p arc --all-features --test benthos_projection_routing -- --nocapture` | Real NATS/Benthos path exercised; prerequisite skips are not passes |
| Manifest contracts | `npm run benthos:config:test`; `make benthos-config-check` | Generator tests and freshness pass |
| Pipeline syntax | `make benthos-lint` | Installed pinned tool passes; avoid the target's anonymous Docker fallback, use a scoped Compose service instead |
| Generated apps | `make scaffold-check` | Clean-room selected auth/plugin/API/UI combinations; inspect emitted authorization and actor attribution |
| Browser flows and assets | `make frontend-build`; `make e2e` | Intended backend identified, browser auth/CSRF/idle/profile/API revocation verified; run backend build/server in Compose and point browser harness at it |
| Dependencies | `make audit`; `npm audit --audit-level=moderate` | Fresh reports and reviewed exceptions; `make audit` differs from CI's flags |

These commands do not yet prove all manual adversarial cases above. In particular, add failing
regression cases when repairing TM-03; passing existing tests cannot close that finding.
The current Security workflow explicitly ignores `RUSTSEC-2026-0258` and `RUSTSEC-2023-0071` in
`cargo audit --deny warnings`. Reassess exposure, owner and expiry rather than treating ignored
advisories as fixed. CodeQL is configured for JavaScript; its success is not Rust analysis.
Inspect individual required job conclusions, not only the summary job. No current advisory lookup
or external workflow execution is implied by this checklist.

## Deployment and sign-off

- [ ] Build the actual candidate image with required DB/event-bus features, complete assets and
  migrations. Check current Dockerfile paths/build-failure handling before trusting it (TM-14).
- [ ] Replace example secrets; separate cookie/JWT/integrity/projection credentials. Verify
  `APP_ENV=production`, private database/broker ports, non-root runtime, filesystem permissions,
  resource limits and managed secrets. Keep diagnostics (`APP_ENV=e2e`) out of production.
- [ ] Rehearse backup restore, event/projection consistency, session-store recovery and service
  restart. Record recovery objectives and who responds to delivery, integrity or audit failures.
- [ ] Record all remaining failures/skips/unknowns with owners and release impact. Confirm the
  reviewed commit and image match the tested candidate. Obtain release authorization separately;
  completing this checklist does not authorize publishing or deployment.
