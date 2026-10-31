# arc-ffi

`arc-ffi` is Arc's native adapter infrastructure, built over `arc-stack` and
without web dependencies. It provides an owned async Tokio runtime, a shared
core registry keyed by aggregate and database identity, safe infrastructure
errors, and a thin stack-construction helper for UniFFI-backed native apps.

The application owns domain types, schema and projection initialization, and
its UniFFI objects. A cancelled caller does not cancel or retry an already
submitted operation; re-read authoritative state before resubmitting a write.

See [Native Adapters](../../docs/ffi.md) and [ADR 0003](../../docs/architecture-decisions/0003-shared-stack-ui-adapters.md).
The package is a 0.8.12 candidate and has not been published.
