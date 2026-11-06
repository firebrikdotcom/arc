Pending owner decisions. This document plans the discussion; it does not approve a policy or authorize deployment or secret changes.

# Deployment audit policy conversation plan

The deployment audit policy remains open. This plan structures an owner-led
conversation and records what evidence would be needed later. It sets no legal
classification, retention period, access rule, deployment setting, or completion
claim. Current runtime values below describe implementation defaults only.

## Decision sequence

Discuss one topic per turn. For each topic, present the evidence and unresolved
choices, summarize the proposed answer, and obtain explicit owner approval before
changing its status to `approved`. Keep unanswered items visible. The owner may
reopen an earlier decision; record the change and revisit dependent topics. A
decision is not `verified` until evidence from the named application and
deployment demonstrates it.

| # | Topic and questions to resolve | Record after discussion | Status |
|---|---|---|---|
| 1 | **Scope and accountability:** Which application, environments, and deployments are in scope? Who owns the policy and who approves it? | Named scope, owner, and approver. | unanswered |
| 2 | **Disclosure inventory:** Which reads require evidence across API, HTML/admin, lists, exports, cached responses, and denied attempts? What classifications apply? | Approved inventory and explicit exclusions. | unanswered |
| 3 | **Identifiers and metadata:** Which actor/resource IDs and field names are needed, and for what purposes? Are source IP and User-Agent necessary? | Allowed, minimized, and prohibited fields. | unanswered |
| 4 | **Retention and holds:** What duration applies by category, when does its clock start, and how do holds suspend and resume deletion? | Rules, precedence, and responsible roles. | unanswered |
| 5 | **Storage protections:** Who may write, read, administer, or delete records? What encryption and key-management requirements apply? | Protection requirements and accountable owners. | unanswered |
| 6 | **Archives and backups:** Which copies exist? How are integrity, holds, retention, and erasure reconciled across copies? | Copy inventory, reconciliation, and deletion procedure. | unanswered |
| 7 | **Operational evidence and approval:** What proof is required, who operates the controls, how are gaps handled, and who approves readiness? | Evidence matrix, open issues, and readiness approver. | unanswered |

## Candidate disclosure inventory—not approved audit scope

This list is a prompt for inventory work, not a statement that these surfaces
are currently logged or must be logged. The production review identifies the
built-in API profile read as instrumented and notes that broader coverage needs
application inventory and explicit instrumentation.

| Candidate surface | Questions for the owner and implementation inventory | Status |
|---|---|---|
| API reads | Which routes and fields disclose data? How should multi-resource responses be represented? | unanswered |
| HTML and admin rendering | Which pages render personal or otherwise classified data, including profile pages? | unanswered |
| Lists and search results | Which collections, pagination, filters, and result fields count as a disclosure? | unanswered |
| Exports and downloads | Which generated files or bulk exports are in scope, and at what point is an attempt recorded? | unanswered |
| Cached responses | Which server, proxy, or client cache paths may serve data without the origin handler running? | unanswered |
| Denied attempts | Which authentication, authorization, missing-resource, or policy denials need records? | unanswered |

## Current implementation facts

These are source-backed capabilities and limitations, not policy recommendations.

- The default runtime selects `NoOpAccessLogger`; access records are discarded
  unless the built-in SQLite selection or an application-provided logger is
  configured. The SQLite journal is separate from the event/read-model database
  and independent of `DATABASE_DRIVER` and `EVENT_BUS`.
- Built-in settings currently default to `ACCESS_LOG_DRIVER=noop`,
  `ACCESS_LOG_REQUIRED=false`, a 1000 ms SQLite busy timeout, and source-IP and
  User-Agent capture disabled. These values describe the code; they are not
  approved production settings. Required mode rejects the built-in noop choice
  and startup errors do not fall back to noop, but application registration or
  a nearer route scope can replace the logger.
- The SQLite logger accepts an on-disk path, initializes an idempotent schema,
  and acknowledges after an SQLite autocommit insert with `synchronous=FULL`.
  This is a local commit subject to filesystem and hardware behavior, not
  replication, encryption, or tamper evidence. New Unix journal files use mode
  `0600`; existing permissions, directory ownership, Windows ACLs, encryption,
  and backups are outside this implementation guarantee.
- Session IDs are always removed before persistence. Source IP and User-Agent
  are omitted unless their respective flags are enabled. Actor/resource IDs and
  field names remain caller-owned. Entries above 16,384 serialized bytes are
  rejected.
- A logger and its clones admit one blocking operation at a time. Overload is
  rejected without a queue; there are no automatic retries. The configured
  SQLite busy timeout bounds lock waiting, not all disk I/O or scheduling. A
  cancelled caller's admitted operation can still commit, and a failed call
  does not establish exactly-once outcome.
- With the wrapper's current classification behavior, PHI/PCI sink failures
  return HTTP 503 without the payload; other classifications, including PII,
  retain fail-open behavior. These are implementation behaviors, not an
  approved classification or deployment policy.
- `purge_before(cutoff_utc_us, limit)` deletes at most 1–10,000 rows strictly
  older than a supplied UTC-microsecond cutoff. No scheduler or default
  retention duration is installed. Logical deletion does not securely erase
  filesystem free pages, snapshots, or backups.

See [Access logging](access-logging.md), the [production review and operations
guide](access-logging-production-review.md), and the implementation in
[`access_log.rs`](../crates/arc-es-sqlite/src/access_log.rs). The operations guide
documents the current knobs, their defaults, and focused verification details.

## Decision and evidence record

Use only these statuses: `unanswered`, `proposed`, `approved`, and `verified`.
An approved answer records who approved it and when. Verification records the
named deployment, evidence location, operator, and result. Keep an issue open if
evidence is missing or conflicts with the approved decision.

| Topic | Status | Proposed answer / approved decision | Owner approval (name/date) | Evidence, operator, result, or gap |
|---|---|---|---|---|
| Scope and accountability | unanswered |  |  |  |
| Disclosure inventory | unanswered |  |  |  |
| Identifiers and metadata | unanswered |  |  |  |
| Retention and holds | unanswered |  |  |  |
| Storage protections | unanswered |  |  |  |
| Archives and backups | unanswered |  |  |  |
| Operational evidence and approval | unanswered |  |  |  |

| Evidence check | Status | Named scope / evidence link | Operator | Result or unresolved issue |
|---|---|---|---|---|
| Persistence and acknowledgement boundary | unanswered |  |  |  |
| Fail-closed and failure-path behavior | unanswered |  |  |  |
| Restore and reconciliation | unanswered |  |  |  |
| Bounded purge and copy reconciliation | unanswered |  |  |  |

## Synthetic verification procedures to scope after decisions

These procedures describe candidate evidence using synthetic records. They do
not authorize changing deployment settings, production data, secrets, or policy.
The eventual operator must first work in an isolated test environment and name
the actual sink and deployment being assessed.

1. **Persistence:** Write uniquely identified synthetic records through the
   actual intended logging path. Check that successful acknowledgement matches
   the documented commit boundary. Restart the test process against the same
   journal and reconcile the expected IDs, fields, duplicates, and missing
   records. Repeat for each relevant application instance or logger override.
2. **Fail-closed behavior:** In the isolated environment, inject synthetic
   logger rejection, lock contention, disk failure, overload, and a stalled
   logger. Check each approved classification's response and whether data is
   withheld. Record HTTP behavior, bounded diagnostics, and any uncertain write
   outcome. Do not infer that the current PHI/PCI behavior settles the policy.
3. **Restore reconciliation:** Restore an isolated copy using the proposed
   recovery procedure. Compare record IDs and counts with the synthetic source
   ledger; check duplicate handling, timestamp boundaries, and any integrity
   evidence available. Record gaps and the operator. Current SQLite code does
   not provide a backup/restore or tamper-verification protocol.
4. **Bounded purge and copies:** With synthetic rows spanning a chosen test
   cutoff, invoke the explicit purge hook in bounded batches. Confirm each batch
   removes no more than its requested limit and only rows strictly before the
   cutoff, then reconcile primary storage with any test archive/backup copies.
   Test holds and copy deletion only after their precedence and procedures are
   explicitly decided. The hook itself does not erase free pages or backups.

The required retention rules, hold behavior, permitted identifiers, production
access roles, encryption and key ownership, archive/backup treatment, and
readiness approver still need owner answers. Do not add instrumentation or
describe the policy as complete until those answers and their deployment
evidence are recorded.
