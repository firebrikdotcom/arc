# Access logging

`AccessLogger` is an explicit read-audit interface, separate from write-side
[Audit Metadata](audit-metadata.md). **The default runtime discards access-log
records unless configured with `ACCESS_LOG_DRIVER=sqlite`.** The shipped dedicated
SQLite journal acknowledges committed inserts. Selecting Postgres,
NATS, or Benthos does not enable read-audit persistence. These primitives and
classification names do not establish regulatory compliance.

See the [production review and operator runbook](access-logging-production-review.md)
for evidence, deployment gates, and unresolved decisions.

## Read lifecycle and failure behavior

The sample `GET /api/profile` resolves the JWT actor, reads `users_view`, selects
`id`, `name`, and `email`, then awaits `record_read` before constructing its JSON
response. Missing identity returns 401, a missing row returns 404, and a store
failure returns 500 before access logging. This hook describes a planned data
release, not proof that the client received or viewed the response.

`Sensitive<T>` holds the payload without exposing serialization, response, or
public extraction APIs. `record_read` returns `AccessLogged<T>` after applying
failure policy. Its constructor is private and it cannot be deserialized.
Both wrappers redact the payload in `Debug` output. This is an opt-in discipline:
controllers can still return raw values without using either wrapper.

| Wrapped classification | Logger returns an error | Logger returns success |
|---|---|---|
| PHI / PCI | Error diagnostic and HTTP 503; payload withheld | Payload released |
| PII / Confidential / Internal / Public | Warning diagnostic; payload released | Payload released |

The **wrapper's** classification overrides `AccessedResource.sensitivity`. Both
validation and sink errors follow this table. There is no automatic fallback,
retry, timeout, or durable buffer. Async calls are awaited and can stall a read.
These are framework defaults; assess them against the application's requirements.
A successful no-op call releases even PHI/PCI without persisting a record.

## Controller integration

After authenticating, authorizing, and resolving the resource, return the helper's
result directly. For example, inside an async controller returning
`Result<AccessLogged<serde_json::Value>, AppError>`:

```rust
use arc_core::access_log::{AccessedResource, PurposeOfUse, Sensitivity};
use arc_web::helpers::access_log::{record_read, AccessLogged, Sensitive};

let resource = AccessedResource::new("UserProfile", actor_id.clone(), Sensitivity::Pii)
    .with_fields(["id", "name", "email"]);
record_read(
    access_logger.as_ref(),
    &req,
    actor_id,
    resource,
    PurposeOfUse::UserInitiated,
    Sensitive::pii(response_data),
).await
```

Describe fields by name, never by their values. Derive the actor from authenticated
server state, not a caller-supplied identity header. Inventory every disclosure
path, including HTML rendering, exports, lists, cached reads, and WebSockets;
authentication alone does not install this hook. Multi-resource responses need
an explicitly designed audit representation and failure behavior.

The `AccessLogged` responder returns HTTP 200 with the inner JSON unchanged.
Serializing the wrapper directly instead produces its struct representation
(`{"data": ...}`); use the responder or `into_inner()` when the inner shape is needed.
Cloning an already logged wrapper does not log a second access.

## Record contents and privacy

`AccessLogEntry::new` stamps a random UUID and process-clock UTC microseconds.
It validates only nonblank actor ID, resource kind, and resource identifier.
The record contains:

- Actor ID, optional session ID, source address, and User-Agent.
- Resource kind, identifier, field names, and classification.
- Purpose, timestamp, access UUID, and optional correlation UUID.

The web helper leaves session ID absent, reads the address via Actix
`realip_remote_addr()`, copies User-Agent, and accepts a parseable client
`X-Correlation-Id`. Invalid/missing correlation IDs become `None`. This helper
has no trusted-proxy allowlist or metadata length limits. Treat forwarded
addresses and client correlation IDs as untrusted context, never authentication.
Configure ingress to strip/replace forwarded headers and prevent direct access
if their attribution is relied upon.

Payload values are not supplied to the logger, but identifiers, field names,
addresses, and arbitrary strings can themselves disclose sensitive information.
The helper emits only `error_kind=validation|sink` on failure, not sink error
strings, identities, or payloads. Custom sinks must apply their own safe logging.
`Identity`, `AccessedResource`, and `AccessLogEntry` remain serializable and
Debug-printable; do not dump them into general diagnostics.

## Runtime integration

`arc-web/src/commands/serve.rs` selects the logger at startup. Set
`ACCESS_LOG_DRIVER=sqlite`, `ACCESS_LOG_REQUIRED=true` and
`ACCESS_LOG_SQLITE_PATH` to a dedicated persistent disk file. Invalid or unavailable
configured sinks abort startup. See the [operations guide](access-logging-production-review.md)
for migration, acknowledgement, metadata flags, lock bounds and retention hooks.
An application can supply its own implementation through the existing builder
state registration seam, using the **trait-object type**:

```rust
let logger: std::sync::Arc<dyn arc_core::access_log::AccessLogger> =
    std::sync::Arc::new(my_durable_logger);
let app = arc_web::ArcApp::builder().register_data(logger);
// Register application aggregates/routes/capabilities, then run the builder.
```

`register_data` wraps the Arc in Actix `web::Data`. Registered data is applied
after the default logger, replacing the same type at application scope. Registering
only `Arc<MyLogger>` does not satisfy `web::Data<dyn AccessLogger>`. A nearer route
scope can override application state; verify the actual served route. The required
flag guards built-in startup selection, not application or plugin overrides.

`NoOpAccessLogger` validates then discards. `RecordingAccessLogger`, available with
`arc-core/test-utils`, keeps records in an unbounded in-memory vector for tests;
it is not an operational store. Event-store integrity signatures and Benthos
write-event delivery do not cover these records.

## Verification and API change

This review makes `Sensitive::into_parts` and `AccessLogged::new` private and removes
`AccessLogged` deserialization. Callers using those bypasses must move to
`record_read`. Compile-fail doctests protect these boundaries and the lack of
`Sensitive` serialization. Unit tests cover the six classification defaults,
metadata capture, response shape, and diagnostic redaction. Existing sample
profile tests cover logged success, missing records, and failure behavior.

For sink implementations, add crash/restart, outage, slow-sink, cancellation,
duplicate retry, and backup/restore tests before asserting persistence guarantees.
