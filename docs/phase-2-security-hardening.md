# Phase 2 Security Hardening Delivery Contract

This document tracks the Phase 2 security-hardening milestone. `progress.md` remains
the canonical project status and roadmap.

## Scope

Phase 2 has two unchecked roadmap bullets and three remaining deliverables:

1. Authorize WebSocket handshakes and room delivery.
2. Verify persisted event origin before internal projection delivery.
3. Approve and verify the deployment audit policy for the named application and
   deployment.

The first two are framework implementation work. The deployment audit policy is
an organization/operator decision and cannot be completed by selecting generic
legal defaults.

## Delivery and PR order

The integration branch is `feature/phase2-security-hardening`, based on the
current `origin/master` (`9a25bf5`). The eventual milestone PR targets `master`.

| Order | PR | Target | Completion condition |
| --- | --- | --- | --- |
| 1 | Repair dependency security gates | `feature/phase2-security-hardening` | Repair root causes behind compatible Rust/npm dependency advisories and verify the dependency-review setting. Keep all checks enabled and audit strength unchanged; no skipped checks or weakened audit. Required CI is green. |
| 2 | WebSocket connection and room authorization | `feature/phase2-security-hardening` | Authenticated, origin-checked connections; denied room joins create no membership; cross-user delivery tests pass. |
| 3 | Persisted projection-event origin validation | `feature/phase2-security-hardening` | Only an exact event found in the authoritative typed store is projected; forged, absent, or modified events cause no mutation. |
| 4 | Security documentation reconciliation | `feature/phase2-security-hardening` | Technical writer updates the threat model, coverage matrix, release checklist, relevant operations guide, and canonical roadmap from verified behavior. |
| 5 | Deployment audit policy record | `feature/phase2-security-hardening` | Named owner approves disclosure inventory, identifiers, retention/legal holds, sink access/encryption, archive, backup restore and erasure; deployment evidence is recorded. |
| 6 | Phase 2 milestone PR | `master` | Combined verification, independent review, and required CI/Security checks pass on the final pushed head, and the approved deployment-policy record is present before milestone closure. The milestone is not ready until CI is green. A draft PR may open earlier while policy remains pending. |

Feature PRs include their focused tests and documentation changes. If both
features touch shared security coverage, the second PR rebases on the first
before merge. The documentation PR follows the verified implementation. No
release, crate publication, production deployment, or secret change is included.

## Acceptance

### WebSocket authorization

- Invalid, missing, expired, or revoked handshake identity fails closed.
- A cached session identity alone is not treated as a validated credential for
  plugin-backed sessions.
- Browser origins are checked against an explicit allowlist; missing, `null`,
  malformed, and unapproved origins are denied by default.
- Actual allowed origins and trusted proxy behavior come from deployment
  configuration; this framework change does not invent production values.
- Only the verified user's own room is allowed by the default policy. Unknown
  rooms are denied; applications may explicitly supply a different policy.
- Authorization occurs before membership changes, including direct actor
  messages. Unknown connection IDs create no membership.
- Anonymous and cross-user clients receive no private-room or user-targeted
  payloads. Unsubscribe and disconnect remove membership.
- Existing socket revocation remains a TM-11 residual risk outside this bounded
  change unless separately implemented and verified; handshake checks must not
  be described as continuous authorization.

### Projection event origin

- An additive typed `load_event(aggregate_type, aggregate_id, sequence)` lookup
  returns `Result<Option<Event>>`; adapters without exact lookup support return
  `Unsupported` and fail closed without scanning an unbounded stream.
- Internal delivery requires complete equality with the authoritative persisted
  event, then projects the stored event instance.
- Missing, modified, cross-aggregate, unavailable, or integrity-failing events
  produce no projection mutation.
- Compare every envelope field, including audit metadata. Authenticate before
  store lookup and return generic errors that do not expose event payloads or
  backend diagnostics.
- Trusted in-process processing and rebuild behavior remain intact.
- Benthos integration fixtures append genuine events before publishing them;
  injected unpersisted events are rejected.
- Cover bounded exact lookup, missing events, aggregate collisions, malformed
  storage, configured integrity failures, and safe duplicate delivery.
- Replay/order risk after physical deletion remains tracked separately under
  TM-08 and is not claimed fixed by origin validation.

### Deployment audit policy

This remains **pending owner input**. Do not choose a legal retention period,
identifier scope, disclosure inventory, legal-hold precedence, access roles, or
backup/erasure policy on the owner's behalf. Completion requires a named
application/deployment, accountable owner, approved policy, and evidence for the
actual sink, route, access/encryption, retention, archival, restore and erasure
behavior. The [conversation plan](deployment-audit-policy-plan.md) records the
ordered owner decisions and candidate evidence checks; it does not approve
policy. Until then, Phase 2 is not fully closed.

## Verification gates

- Focused unit, HTTP, browser and storage/routing tests cover the acceptance
  criteria without opt-in security assertions or prerequisite skips.
- Formatting, all-feature Clippy, workspace tests, docs build, and the harness
  verification/review gates pass on the combined tree.
- Applicable GitHub CI and Security jobs pass on the final pushed milestone PR
  head. Pending or historical runs do not satisfy this gate.
