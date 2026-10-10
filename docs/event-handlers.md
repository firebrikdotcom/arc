# Guide — Writing event handlers with Benthos

This guide is for **framework users**: you are building an application on Arc and want to *react* to
events (send an email when a user registers, update a search index, call a webhook, or trigger an
Arc-owned projection service). You will do this **without editing any Arc internal crate**. You
write a small handler service and a **handler manifest**; Arc's Benthos routing layer delivers
matching events to it.

> Why Benthos and not a Rust worker? See [ADR 0001](architecture-decisions/0001-benthos-only-event-routing.md). Short
> version: the durable routing plane is **Benthos (Redpanda Connect) only**. The retired
> `arc-worker` is not the path — do not extend it.

## How events flow

```
write → CommandBus → EventStore::append → arc-es-nats publishes
        events.<aggregate_type>.<event_type>   (NATS JetStream, stream EVENTS)
                          │
                          ▼
        Benthos pipeline  input: nats_jetstream (subject events.>)
                          processor: validate envelope · dedupe(event_id) · route
                          output: switch ──► your handler (HTTP / NATS)
                                    └─ fallback ──► dead-letter (dlq.<handler>.<event_type>)
```

- The writer's job ends at *publish*. It does not know your handler exists.
- **Benthos is the only durable consumer** of `events.>`. It owns routing, filtering, dedupe,
  retries, and dead-lettering.
- A handler is just a delivery target. You add one with a manifest; you never touch Rust.
- **Benthos never writes to Arc databases.** Database writes remain inside Arc-owned code paths
  (`ProjectionEngine`, read-model stores, or explicit projection HTTP services).

## 1. The versioned event envelope (the contract)

Every message Benthos delivers to a handler is a JSON **envelope**. The envelope is the stable
contract between Arc and your handler. It wraps the internal `arc-core` `Event` additively, so the
schema can evolve without breaking handlers.

```json
{
  "envelope_version": 1,
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "aggregate_type": "User",
  "aggregate_id": "9b2f...",
  "sequence": 3,
  "event_type": "ProfileUpdated",
  "occurred_at": 1750258882000,
  "subject": "events.user.profile_updated",
  "audit": {
    "actor_id": "9b2f...",
    "actor_session_id": "…",
    "source_ip": "203.0.113.7",
    "user_agent": "…",
    "causation_id": "…",
    "timestamp_utc_us": 1750258882000000
  },
  "payload": { "name": "Ada Lovelace" }
}
```

Field rules:

| Field | Source | Guarantee |
|---|---|---|
| `envelope_version` | Benthos | Integer, starts at `1`. Bumped only for breaking envelope changes; new optional fields do **not** bump it. Handlers MUST tolerate unknown fields. |
| `event_id` | `Event.event_id` | Globally unique per event occurrence. **This is your idempotency / dedupe key.** |
| `aggregate_type` / `aggregate_id` | `Event` | Stream identity. `aggregate_id` is an opaque UUID — never a natural key. |
| `sequence` | `Event.sequence` | Monotonic per `aggregate_id`, starts at 1. Use for per-aggregate ordering and last-writer-wins. |
| `event_type` | `Event.event_type` | Past-tense PascalCase (`UserRegistered`). |
| `occurred_at` | `Event.timestamp` | Milliseconds since the Unix epoch. |
| `subject` | NATS | The JetStream subject, `events.<aggregate_type>.<event_type>` lowercased. |
| `audit` | `Event.audit` | who/when/where/why. Present on every event. Do not log raw PHI from `payload`. |
| `payload` | `Event.payload` | Domain-specific JSON. Schema is owned by the aggregate, versioned by `event_type` evolution (add fields, don't repurpose). |

**Subject naming and PHI:** subjects are derived from `aggregate_type`/`event_type` only — never
embed natural keys or PHI in a subject (`events.patient.12345` leaks). Keep identifiers in the
opaque `aggregate_id`.

## 2. The handler manifest

A handler manifest is a small YAML file you commit under `config/handlers/<name>.yaml`. Arc's
generator turns manifests into the Benthos pipeline config — you do not hand-write Bloblang for the
common cases.

```yaml
# config/handlers/welcome-email.yaml
name: welcome-email              # unique; used for consumer name, DLQ subject, metrics
description: Send a welcome email when a user registers.

# Which events to receive. Matched against the envelope.
subscribe:
  aggregate_types: [User]        # optional filter
  event_types: [UserRegistered]  # required; at least one
  # Optional Bloblang predicate for finer filtering (e.g. by sensitivity):
  # filter: 'this.audit.source_ip.has_prefix("10.")'

# Where to deliver. Exactly one target block.
delivery:
  type: http                     # http | nats
  http:
    url: "http://welcome-email:8090/handle"
    verb: POST
    timeout: 10s
    headers:                       # optional; merged with framework headers
      Authorization: "Bearer ${HANDLER_TOKEN}"
    # Benthos adds idempotency + tracing headers automatically (see §4).

# Idempotency + ordering expectations the handler promises to honor.
idempotency:
  key: event_id                  # dedupe key; event_id is the default and recommended
  ordering: none                 # currently supported: none

# Failure handling.
retry:
  max_attempts: 4                # delivery attempts before dead-lettering
  backoff: exponential           # currently supported: exponential
  initial_interval: 2s
  max_interval: 1m
dead_letter:
  enabled: true                  # on exhaustion, send to dlq.<name>.<event_type>
```

Required keys: `name`, `subscribe.event_types`, `delivery`. Everything else has framework defaults
(idempotency key `event_id`, ordering `none`, 4 attempts exponential backoff, DLQ enabled). The
generator rejects unknown manifest keys and rejects reserved-but-unsupported options instead of
silently ignoring them. Today that means `idempotency.key` must be `event_id`,
`idempotency.ordering` must be `none`, and `retry.backoff` must be `exponential`.

Run `make benthos-config` after adding or editing manifests. The generator reads
`config/handlers/*.yaml` and writes the runtime pipeline to
`config/benthos/generated/events.yaml`; `docker-compose.yml` points Benthos at that generated file.
CI runs `make benthos-config-check` so stale generated config fails fast.

The generated pipeline uses the shared `events.>` JetStream consumer, applies envelope mapping,
`subscribe` filters, and `dedupe`, and routes each matching event to your delivery target with a
retry wrapper plus `fallback` to the DLQ.

## 3. Delivery target types

### `http` — deliver to an HTTP endpoint (most common)

Benthos POSTs the envelope as a JSON body to your service. Your service returns:

- **2xx** → Benthos ACKs the JetStream message (success).
- **non-2xx / timeout** → counts as a failed attempt; retried per `retry`, then dead-lettered.

This is the recommended default: your handler is an ordinary web service in any language, fully
decoupled from Arc.

### `nats` — re-publish to a downstream NATS subject

```yaml
delivery:
  type: nats
  nats:
    subject: "handlers.search-index.user"
```

Use when the consumer is itself a NATS subscriber, or to chain pipelines. Benthos publishes the
(optionally transformed) envelope to the subject; downstream durability is the subscriber's
JetStream consumer.

### Projection delivery — call Arc-owned code, not the database

Benthos must not use `sql_insert`, database DSNs, or any direct database output. Projection writes
belong in Arc-owned code because the `Projector`/`ProjectionEngine`/`ReadModelStore` boundary owns
idempotency, schema, storage-driver differences, audit posture, and future invariants.

For distributed projections, route matching events to an Arc-owned HTTP projection endpoint or
projection service:

```yaml
delivery:
  type: http
  http:
    url: "http://arc-app:8080/internal/projections/users/handle"
    verb: POST
    timeout: 10s
    headers:
      Authorization: "Bearer ${INTERNAL_PROJECTION_TOKEN}"
```

That endpoint/service should authenticate the call, deserialize the envelope, run the relevant
projector through the read-model store, and return 2xx only after the projection write is durable.
In `EVENT_BUS=inprocess` dev mode, projections are still driven synchronously in-process — see §6.

## 4. Idempotency expectations

Delivery is **at-least-once**. The same event can arrive more than once (redelivery after a crash,
a retry after a slow 200, a Benthos restart). **Handlers MUST be idempotent.**

Two layers protect you:

1. **Pipeline dedupe.** Benthos runs a `dedupe` processor keyed on `idempotency.key` (default
   `event_id`) backed by a cache, collapsing obvious duplicates within the cache window.
2. **Handler-side idempotency (required).** The cache window is finite; do not rely on it alone.
   Make the handler naturally idempotent:
   - **HTTP state/projections:** persist a processed-event marker keyed on `event_id`, or update
     the read model only when `sequence > existing` for per-aggregate last-writer-wins. The
     `users_view` projector already uses version checks — keep that logic inside Arc-owned code.
   - **Side effects (email, webhooks):** record processed `event_id`s in a dedupe table and short-
     circuit if already present, or use the target system's idempotency key
     (Benthos sends `Idempotency-Key: <event_id>` and `X-Arc-Event-Sequence: <sequence>` headers on
     HTTP delivery).

**Ordering.** `ordering: none` is the only supported manifest value today. Assume events for one
aggregate can arrive out of order and reconcile using `sequence` (ignore an event whose `sequence`
is not greater than what you've already applied). `ordering: per_aggregate` is reserved for a
future partitioned routing mode; current generator versions reject it so users do not receive a
false ordering guarantee.

## 5. Failure and dead-letter behavior

A delivery **fails** when the target returns non-2xx (HTTP), errors (NATS), or times out.

1. **Retry.** Benthos retries every delivery target per the manifest `retry` block (default: 4
   total attempts, exponential backoff 2s→1m). The JetStream message stays un-ACKed during retries.
2. **Dead-letter on exhaustion.** After `max_attempts`, the envelope is routed to the
   **dead-letter subject** `dlq.<name>.<event_type>` (a durable DLQ stream). The original
   JetStream message is then ACKed so the main stream is **not blocked**.
3. **Poison messages.** Benthos validates the required Arc envelope fields before dedupe and
   routing. Malformed events are enriched with `x_arc_dlq` metadata and routed to
   `dlq.envelope.<event_type>` (or `dlq.envelope.invalid` when the type is missing) instead of
   blocking the stream.
4. **Operate the DLQ.** DLQ subjects are monitored; you redrive after fixing the handler by
   replaying the DLQ stream back onto the handler's input. Nothing is silently dropped. See
   [Benthos DLQ and Redrive](benthos-dlq-redrive.md) for the operator workflow.

Guarantee: **no event is lost and no poison event blocks the stream.** Either the handler succeeds,
or the event lands in a DLQ you can inspect and redrive.

DLQ messages carry the original envelope plus an `x_arc_dlq` object:

```json
{
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "event_type": "UserRegistered",
  "x_arc_dlq": {
    "handler": "welcome-email",
    "reason": "delivery_failed_after_retries",
    "failed_at": "2026-06-18T19:46:55.827647355Z",
    "original_subject": "events.user.user_registered",
    "fingerprint": "..."
  }
}
```

Validation failures use `handler: envelope-validation` and
`reason: envelope_validation_failed`. The `fingerprint` is a hash of the message body at the point
it was dead-lettered; use it for operator correlation, not as a domain identifier.

## 6. Local development workflow

You can build and test a handler end-to-end on your machine.

**Option A — in-process (no Benthos, fastest inner loop).**
Run the app with `EVENT_BUS=inprocess` (the default). Projections are driven synchronously in the
writer and are read-after-write consistent. Use this to develop aggregate/projection *logic*. Note:
external handlers (HTTP/NATS) are **not** exercised in this mode — only in-process projections run.

**Option B — full routing (Benthos + NATS, mirrors production).**

1. Start infra: `docker compose up nats benthos` (NATS JetStream with monitoring on `:8222`;
   Benthos with its HTTP server/metrics on `:4195`).
2. Run the app with `EVENT_BUS=nats` so events publish to JetStream.
3. Drop your manifest in `config/handlers/<name>.yaml` and regenerate the pipeline:
   `make benthos-config`.
4. Run your handler service locally (e.g. the `http` target on `localhost:8090`).
   For Arc-owned projection handlers, set the same `INTERNAL_PROJECTION_TOKEN` for both the app and
   Benthos and use an `Authorization: Bearer ${INTERNAL_PROJECTION_TOKEN}` manifest header.
5. Trigger a write (register a user). Watch the event flow:
   - NATS monitor `:8222` shows messages on `events.user.*`.
   - Benthos metrics `:4195/metrics` show input/output/dedupe/DLQ counts.
   - Your handler receives the envelope; check your projection endpoint/service or side effect.
6. Test failure: make the handler return 500 and confirm retries then a message on
   `dlq.<name>.userregistered`.

**CI.** Pipeline configs are versioned artifacts. CI runs the generator tests,
`make benthos-config-check`, and Redpanda Connect lint against `config/benthos/events.yaml` and
`config/benthos/generated/events.yaml`. The routing integration suite publishes through
`arc-es-nats`, lets Benthos invoke an Arc-owned projection endpoint, and verifies the read model
is updated through Arc's own store. It also covers retry and dead-letter behavior; Benthos never
writes an Arc database directly.

## Checklist for a new handler

- [ ] Handler service returns 2xx only on durable success; non-2xx/timeout otherwise.
- [ ] Handler is idempotent on `event_id` (UPSERT or dedupe table), even though dedupe runs upstream.
- [ ] Manifest committed at `config/handlers/<name>.yaml` with `subscribe.event_types` set.
- [ ] Ordering need declared (`none` vs `per_aggregate`); if `none`, reconcile with `sequence`.
- [ ] `dead_letter.enabled: true` (default), Arc's NATS setup has provisioned `ARC_DLQ` for
      `dlq.>`, and someone owns DLQ redrive.
- [ ] No PHI/natural keys logged from `payload`; subjects stay opaque.
- [ ] You edited **no** Arc internal crate. If you did, you're on the wrong path — re-read §"How events flow".
