# ADR 0001 — Benthos-only durable event routing

- Status: **Accepted**
- Date: 2026-06-18
- Deciders: Arc framework architecture (benthos-architect, phase 1)
- Supersedes: the custom `arc-worker` durable-consumer approach (refactor-plan Step 4, original form)
- Related: `progress.md` and [Event Handlers](../event-handlers.md)

## Context

At the time of this decision, Arc persisted every write as an immutable `Event` (`crates/arc-core/src/event.rs`) and
publishes persisted events to NATS JetStream through `arc-es-nats` on the subject
`events.<aggregate_type>.<event_type>` (lowercase). Two delivery topologies exist today:

1. **In-process** (`EVENT_BUS=inprocess`): `serve.rs` subscribes a `ProjectionEngineHandler`
   to the synchronous `InProcessEventBus` and rebuilds projections in the writer process.
   This is read-after-write consistent and correct for single-process development.
2. **Distributed (historical implementation)** (`EVENT_BUS=nats`): the writer only publishes. A separate process,
   `arc-worker` (`crates/arc-worker/src/lib.rs`), creates a durable JetStream pull consumer,
   deserializes each `Event`, and drives `ProjectionEngine::process` to update read models,
   ACK-ing on success and NAK-ing on failure.

`arc-worker` works, but it is the wrong long-term home for the routing plane:

- **It hard-codes the routing graph in Rust.** `events.>` → `ProjectionEngine` → `UserProjector`.
  Adding a handler (email on `UserRegistered`, a search-index updater, a webhook) means editing
  and redeploying a Rust binary. Framework users cannot extend event handling without forking Arc.
- **It only delivers to projections.** There is no HTTP/NATS fan-out, no filtering by sensitivity
  or aggregate type, no enrichment, no transformation — all of which real applications need.
- **Its failure handling is a liability.** A message that fails to *deserialize* is NAK-ed for
  redelivery (`lib.rs:174-181`); a projection error is NAK-ed too (`lib.rs:199-207`). With
  `max_ack_pending = 1` and no max-deliver/DLQ, a poison event blocks the entire stream and
  redelivers forever. There is no dead-letter path.
- **It duplicates infrastructure that already exists.** Durable consumers, retry/backoff,
  dedupe, batching, metrics, and dead-lettering are solved problems in a stream-processing
  runtime. Re-implementing them in bespoke Rust is cost without differentiation.

The framework's stated direction (AGENTS.md "Strategic", roadmap §1.4.1, refactor-plan Step 4)
is to keep the core Rust crates (`arc-core`, `arc-es-*`) **headless** and use a best-of-breed,
declarative tool for the routing plane.

## Decision

**Adopt Benthos (Redpanda Connect) as the single durable routing and handler-delivery layer for
Arc, and remove `arc-worker` from the framework's forward direction.**

Concretely:

1. **`arc-es-nats` remains the only publish path.** The writer's responsibility ends at
   `EventStore::append` + publish to `events.<aggregate_type>.<event_type>`. Nothing downstream
   is the writer's concern.

2. **Benthos is the only durable consumer of `events.>`.** Benthos pipelines
   (`input: nats_jetstream` → `processor: bloblang/dedupe/...` → `output: switch/broker`) own all
   routing, filtering, enrichment, deduplication, transformation, retry, and dead-lettering.

3. **Event handlers are external to Arc.** A handler is an HTTP service or NATS subscriber that
   Benthos delivers to. Framework users add handlers by writing a **handler manifest**
   (a small YAML descriptor) from which the Benthos pipeline config is generated — they never edit
   `arc-worker`, `ProjectionEngine`, or any Rust internal.

4. **Benthos must never write directly to Arc databases.** Projection delivery is a routed
   handler call, not a database output. In distributed mode, Benthos delivers user events to an
   Arc-owned HTTP projection endpoint/service, and that Arc-owned code runs the
   `Projector`/`ProjectionEngine`/`ReadModelStore` path that owns schema, idempotency, storage
   driver behavior, and audit posture. The three-trait model in `arc-core` stays — it remains the
   in-process path and the projection *logic* — but it is no longer driven by `arc-worker` in the
   distributed topology.

5. **`arc-worker` is rejected as the durable routing layer and removed from the roadmap's core
   path.** See "Rejected" below for the precise status.

The full extension contract — envelope schema, manifest shape, delivery targets, idempotency,
dead-letter behavior, and local workflow — is specified in [Event Handlers](../event-handlers.md).

**Non-negotiable boundary:** handler manifests support HTTP and NATS delivery only. SQL/database
outputs are intentionally rejected by the generator.

## Rejected alternatives

### Rejected: keep `arc-worker` as the primary durable router (status quo)

**Rejected.** `arc-worker` is explicitly **not** the framework's durable routing and handler
delivery layer going forward. Reasons, in order of weight:

- Extending event handling requires editing/redeploying a Rust binary — the opposite of the
  declarative, user-extensible model the framework wants.
- It cannot route, filter, enrich, transform, or fan out to HTTP/NATS targets; it only feeds
  `ProjectionEngine`.
- Its NAK-everything failure model has no dead-letter path and poison-message-blocks-the-stream
  behavior that is unacceptable in production.
- It re-implements retry/dedupe/DLQ/metrics that a stream processor already provides.

**Disposition of the existing crate:** `arc-worker` is removed from the framework's recommended
architecture and from the roadmap's core path. It MAY survive only as an optional, clearly-labeled
thin consumer for narrow, low-latency, tightly-coupled cases that genuinely cannot tolerate a
hop through Benthos. It is never the default, never required for projections, and new handler work
MUST NOT extend it. Treat any continued use as legacy.

### Rejected: in-process bus as the only path

**Rejected for distributed/production.** `InProcessEventBus` blocks the write path on every
handler and offers no durability, redelivery, or fan-out across processes. It is retained as the
zero-dependency single-process development default (`EVENT_BUS=inprocess`) and nothing more.

### Rejected: a new bespoke Rust router ("arc-router")

**Rejected.** Rewriting `arc-worker` with better failure handling still lands us with bespoke
routing code that users must fork to extend, and still re-implements stream-processor primitives.
The whole point is to *stop* owning the routing runtime.

## Consequences

### Positive

- Framework users extend event handling declaratively, with zero changes to Arc internals.
- Routing, filtering, enrichment, dedupe, retry, batching, DLQ, metrics, and tracing come from a
  mature runtime instead of bespoke Rust.
- The core crates stay headless and publishable; the routing plane evolves independently of Rust
  release cycles.
- A real dead-letter path replaces the poison-message redelivery loop.

### Negative / costs

- Benthos becomes an operational dependency in the distributed topology (one more service to run,
  monitor, and version). Mitigated: it is a single static binary / container and is not needed for
  the in-process dev default.
- Pipeline config is now a versioned artifact that must be linted and tested in CI
  (`benthos lint`, config-validation job).
- Team must learn Bloblang and Benthos pipeline structure. Mitigated by the handler guide and a
  manifest→config generator that hides most of it.

### Neutral

- `arc-core`'s `Projector`/`Projection`/`ProjectionEngine` model is unchanged; only its *driver* in
  the distributed topology changes (Benthos output instead of `arc-worker`).
- The wire `Event` shape is unchanged; the versioned envelope (guide §1) wraps it additively so
  existing consumers keep working.

## Implementation status (updated 2026-09-08)

All original implementation follow-ups are complete:

- [x] `config/benthos/` pipelines: JetStream input on `events.>`, dedupe on `event_id`, a
  `switch` output keyed by `aggregate_type`/`event_type`, HTTP/NATS handler delivery, and a DLQ
  output.
- [x] Handler-manifest → Benthos-config generator and a `benthos lint` CI gate.
- [x] Arc-owned projection endpoint/service and integration coverage for publish → Benthos →
  projection HTTP call → read-model update.
- [x] `docker-compose.yml` runs Benthos as the routing service; `arc-worker` is removed.
- [x] The roadmap records `arc-worker` as historical and Benthos as the active durable router.
