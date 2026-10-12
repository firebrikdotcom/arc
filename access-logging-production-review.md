# Access-log production review and operations

Reviewed 2026-09-12–13 in `security/access-log-review-20260912`. Scope: read-audit
wrappers, stores, failure policies, privacy, and runtime wiring. This is a source
review plus focused verification, not a production deployment certification.

**Original review conclusion (superseded for the opt-in SQLite sink by the
[implementation update below](#durable-implementation-update--2026-09-14)):**
the read-audit interface exists, but the default application has
no access-log persistence. A deployment requiring durable read audit needs an
explicit sink and coverage design before it can rely on this subsystem.

## Evidence-backed findings

Paths below are repository-relative; line numbers refer to this review's source.

| Finding | Impact and evidence | Disposition |
|---|---|---|
| High: default logger discards every valid entry | `crates/arc-web/src/commands/serve.rs:194` constructs `NoOpAccessLogger` unconditionally. `crates/arc-core/src/access_log.rs`, `NoOpAccessLogger::log_access`, constructs then discards the entry and returns success. PHI/PCI fail-closed handling cannot detect that discard. | Open architecture decision: sink, acknowledgement boundary, and required-startup behavior. |
| High: persistence is absent across all shipped stores | Workspace search for `impl AccessLogger` finds only NoOp, feature-gated Recording, and failing test doubles. SQLite/Postgres event stores implement write-event storage; `arc-es-nats` implements `EventBus`, not `AccessLogger`. No access-log schema or pipeline exists in `migrations/` or `config/`. | Do not advertise read-log durability from DB or NATS settings. No sink added by this review. |
| High: disclosure coverage is opt-in and incomplete | `crates/arc-app/src/http/controllers/api_controller.rs:178` instruments profile. `crates/arc-app/src/http/controllers/admin_controller.rs:79` renders session profile data without an access logger; `crates/arc-auth-admin/src/lib.rs:230` and its users list also render identity data without this hook. Repository call-site search finds no other production `record_read` caller. | Application owners must classify and inventory all disclosure surfaces. This review does not choose new audit scope or collect more personal data. |
| Fixed: response discipline was bypassable | `crates/arc-web/src/helpers/access_log.rs:77` and `:104` are now private; `AccessLogged` no longer derives Deserialize (`:97`). Previously callers could unwrap or manufacture the supposedly logged response. | Four compile-fail doctests cover the intended API boundary. This cannot force callers to wrap raw data or stop reusing an already logged response. |
| Fixed: diagnostics could disclose payloads or sink details | Wrapper Debug implementations now omit data (`crates/arc-web/src/helpers/access_log.rs:198`); failure branches emit only a bounded error category (`:165`, `:175`). Previously derived Debug exposed data and `%e` printed arbitrary sink strings. | Regression tests cover safe categories and wrapper output. Sink-owned diagnostics remain the sink author's responsibility. |
| Medium: no timeout or retry contract | `crates/arc-web/src/helpers/access_log.rs:158` directly awaits the sink. A pending future blocks either policy; fail-open applies only after an error. Validation errors and persistence errors share the same policy. | Decide deadlines, cancellation semantics, and retry/idempotency at the sink boundary; do not silently detach a write task. |
| Medium: metadata trust/minimization is undefined | `crates/arc-web/src/helpers/access_log.rs:115` copies forwarded-address context and User-Agent, leaves session ID absent; `:132` accepts a caller UUID. `AccessLogEntry::new` validates three nonblank strings only. | Decide trusted ingress, field limits, pseudonymization, and required correlation/session attribution. No raw session tokens should enter logs. |
| Medium: operational evidence is missing | No durable access store implies no shipped access-log restore, retention, archive, redrive, tamper verification, health gate, or recovery monitoring. The helper emits per-failure diagnostics but tracks no recovery transitions or counters. | Operator requirements below remain deployment gates, not delivered features. |

The accepted [routing ADR](architecture-decisions/0001-benthos-only-event-routing.md)
covers write events on `events.>`. It does not make access records durable.
[ADR 0002](architecture-decisions/0002-framework-upgrade-contract.md) defines
framework/application ownership; applications can inject an `Arc<dyn AccessLogger>`
through `ArcAppBuilder::register_data` (`crates/arc-web/src/lib.rs:237`). Runtime
registration occurs after the default (`commands/serve.rs:246`, `:256`). This is
an available injection seam, not a shipped persistent implementation.

## Durability boundary to decide

`AccessLogEntry` supplies an ID and wall-clock timestamp but no signature, hash
chain, durable receipt, transaction, flush, or replay protocol. Test Recording
storage is process memory with no cap and disappears on restart. NoOp loses the
record immediately. Neither is suitable evidence of persisted access.

For a future sink, document precisely what `Ok(())` acknowledges: process memory,
a local durable journal, a committed DB transaction, or replicated broker storage.
Specify tolerable loss and recovery time, encryption/key ownership, capacity and
backpressure, restricted writer/reader roles, and how deletion or alteration is
detected. Generate a stable access ID once per logical attempt if retries must be
deduplicated; calling `AccessLogEntry::new` again generates a different UUID.

Logging precedes response serialization and network delivery. A record may exist
even if serialization or delivery fails. Conversely, existing fail-open policies
allow data release after a logging failure. Do not interpret records as confirmed
human views or assert exactly-once delivery. Assess clocks and correlation across
instances rather than treating timestamps as a globally ordered sequence.

## Operator runbook

Before relying on access auditing:

1. Record the approved disclosure inventory and classifications, including HTML,
   list/export and cached paths. Decide how denied attempts and multi-resource
   responses are represented; current profile 401/404 paths create no access record.
2. Identify the actual `AccessLogger` trait object for each served route. Confirm
   the application supplies a durable sink through `register_data`, with no nearer
   scope replacing it. Neither application health nor `EVENT_BUS=nats` proves this.
3. Use synthetic data to read an instrumented endpoint. Locate the record in the
   intended sink with actor, resource, fields, purpose, ID, timestamp and correlation;
   confirm response values and credentials are absent. Repeat across instances.
4. Inject sink rejection and unavailability. Confirm PHI/PCI return 503 without
   the payload and assess whether PII/other fail-open behavior is approved. Test a
   sink that stalls, not only one that returns an error.
5. Crash/restart at the documented acknowledgement boundary; verify persisted
   records and duplicates. Exercise restore into an isolated environment and
   reconcile against synthetic expected IDs. Test any redrive mechanism explicitly.
6. Configure restricted sink access and diagnostic handling. Collect safe counters
   for attempted/succeeded/failed writes, latency, backlog and loss where supported.
   Alert on unexpected record gaps and verify recovery. These metrics and recovery
   signals are requirements for a custom sink, not current built-in telemetry.

During an incident, identify affected instances/routes and the time window. Inspect
safe failure categories and the sink's own restricted health metrics. Under NoOp,
there is no persisted backlog to recover; never claim write events reconstruct
who read data. Under a custom sink, follow its documented receipt/retry protocol.
Do not downgrade classifications or turn on raw payload/error logging to restore
service. After recovery, reconcile available receipts and record any unrecoverable
gap without inventing missing accesses.

Retention duration, access-review responsibilities, permitted identifiers,
geographic storage, erasure/legal-hold conflicts, and treatment of backups must be
decided by the organization. This review sets no retention period and makes no
compliance claim. Apply approved policies consistently to primary storage, replicas,
archives, incident exports and backups; test their enforcement before relying on it.

## Verification scope

Focused Rust tests run through a uniquely named Docker Compose project, with the
checkout mounted read-only and build output in `/tmp`. No DB schema changes or
new external services were introduced. Tests prove wrapper and helper behavior;
they cannot prove durable storage that does not exist. The rendered HTTP error response is tested; `AppError` currently inherits Actix's
default `status_code()` (500) even when its custom `error_response()` emits 503.
Consumers inspecting that trait method directly should not infer the wire status.
This pre-existing general error-API inconsistency is outside the bounded wrapper fix.
Exact commands and results
are recorded in `/tmp/nineties-access-log-review-report.md` for this lane.

## Durable implementation update — 2026-09-14

The original review above describes the pre-implementation state. Arc now ships
`arc_es_sqlite::SqliteAccessLogger`, selected by the shared web runtime. It uses a
**separate SQLite journal**, independently of `DATABASE_DRIVER` and `EVENT_BUS`.
The original persistence findings are addressed for this opt-in configuration;
coverage and deployment policy findings remain open.

```dotenv
ACCESS_LOG_DRIVER=sqlite
ACCESS_LOG_REQUIRED=true
ACCESS_LOG_SQLITE_PATH=/var/lib/arc-audit/access.db
ACCESS_LOG_BUSY_TIMEOUT_MS=1000
ACCESS_LOG_INCLUDE_SOURCE_IP=false
ACCESS_LOG_INCLUDE_USER_AGENT=false
```

Defaults are `ACCESS_LOG_DRIVER=noop`, `ACCESS_LOG_REQUIRED=false`, a 1000 ms
SQLite busy timeout, and both optional metadata flags false. Boolean values must
be exactly `true` or `false`. Required mode rejects noop; unknown drivers, missing
paths, invalid settings and sink initialization failures abort startup. There is
no fallback from SQLite to noop. Application `register_data` and nearer Actix
scope overrides still take precedence: operators must verify the actual route's
logger. Required mode guards the built-in selection, not arbitrary plugin code.

Provision a dedicated persistent local directory before startup. Relative paths
resolve against the process working directory; Windows drive-letter filesystem
paths are accepted. Empty paths, `:memory:`, `file:` SQLite URIs and paths containing
query parameters are rejected. Do not use an ephemeral container layer or an
unreviewed network filesystem. New Unix journal files use mode 0600; existing
file permissions, directory ownership, Windows ACLs, encryption and backups are
operator responsibilities. The journal is not encrypted or tamper-evident.

### Acknowledgement, migration and failures

`Ok(())` means the INSERT completed in SQLite autocommit with `synchronous=FULL`
and the journal is initialized in DELETE journal mode. This is a local database
commit, subject to filesystem/hardware fsync guarantees, not replicated storage.
The embedded idempotent schema migration lives under
`crates/arc-es-sqlite/migrations/access_log/0001_create_access_log/`; it creates
`access_log(access_id, timestamp_utc_us, entry_json)` and a timestamp index.
Startup reapplies the creation statements and checks the required columns.
The accompanying down migration deletes the journal table and is never run
implicitly. This separate schema is not installed in the domain event database.

Each logger and its clones admit one blocking operation at a time; overload
returns an error immediately without creating a queue. SQLite lock waits use
`ACCESS_LOG_BUSY_TIMEOUT_MS` (1–30000). This is a database busy deadline, not a
hard deadline on OS disk I/O or Tokio scheduling. Admitted operations run on the
blocking pool and may finish after caller cancellation. There are no automatic
retries: each call receives a new UUID, and records denote attempted disclosure,
not confirmed delivery or a human view. An error can have an uncertain outcome
under filesystem failure; callers must not assume exactly-once semantics.

Validation, overload, lock and disk errors propagate to `record_read`: PHI/PCI
return 503 without the response payload; other classifications, including PII,
retain the existing fail-open warning behavior. Sink errors use bounded generic
messages without SQL, paths or payloads. Startup reports whether durable logging
is enabled. Custom stalled sinks remain subject to the original wrapper gap.

### Privacy and retention hooks

The stored JSON contains only the existing access-entry structure. Session IDs
are always removed. Source IP and User-Agent are omitted unless explicitly
enabled; neither forwarded addresses nor client headers become trustworthy by
being persisted. Caller-owned actor/resource identifiers and field names must be
opaque/minimized and must never contain tokens or response values. Serialized
entries larger than 16384 bytes are rejected. No additional disclosure surfaces
are instrumented: the built-in API profile is covered; HTML/admin, generated
resources, exports, denied attempts and cached paths still require application
inventory and explicit instrumentation.

`SqliteAccessLogger::purge_before(cutoff_utc_us, limit)` is an explicit operator
hook, deleting at most 1–10000 rows strictly older than the supplied UTC
microsecond cutoff. No scheduler or default duration is installed. This logical
delete does not securely erase free pages, snapshots or backups. Retention
periods, legal holds, approved identifiers, encryption, reader permissions,
archival, backup erasure and restore/reconciliation procedures remain deployment
owned. Use a separate maintenance process with the approved cutoff and bounded
batches; avoid competing with critical read traffic.

### Reproducible checks

Focused tests: `cargo test --locked -p arc-es-sqlite access_log` and
`cargo test --locked -p arc-web --test durable_access_http`, run in Docker Compose.
They cover repeat migration/reopen, exact metadata, privacy flags, bounded
retention, lock and disk failure, overload, recovery and PHI non-disclosure.
The synthetic `durable-audit-fixture` example is test-only and exposes inspection
and fault-injection endpoints; never deploy it. Browser tests in
`tests/durable-audit/` exercise real Chromium navigation, lock failure and a
receipt compared after restarting the server process. Run the `before restart`
test, restart the fixture on the same journal, then run `after restart`.

Run both browser phases with `bash tests/durable-audit/run-browser.sh` after
`npm ci` and installing Chromium. It builds the backend only through Compose,
uses a lane-specific cache, checks the synthetic process marker, restarts the
fixture, and removes its labeled containers on exit. Override
`ARC_AUDIT_TEST_IMAGE`, `ARC_AUDIT_TEST_CACHE` and `ARC_AUDIT_TEST_PROJECT` when
needed. The runner builds its verifier from the checked-in Dockerfile (Rust 1.90
and native SQLite/OpenSSL dependencies) and uses Cargo `--locked`; no private
prebuilt image is required. The default cache is the checkout’s `target/verification-cache` on disk.
Port 18784 must be free. Test receipts are written to
`/tmp/arc-nineties-durable-audit-receipt.json` (override
`ARC_AUDIT_RECEIPT` for another lane).
