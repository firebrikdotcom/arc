# Changelog

## Unreleased — 0.8.12 candidate

- Add `arc new --android`, `arc doctor --android`, and
  `arc generate resource --ffi` for native Android app scaffolding.

- Add headless `arc-stack` for shared event-sourced stack assembly and retain
  the existing `arc-web` stack/config public paths as re-exports.
- Add `arc-ffi` native adapter infrastructure with an owned async runtime,
  shared core registry, and UniFFI support; Android uses FFI only while web UIs
  are for web deployments.
- Add `ArcAppBuilder::register_aggregate_with_stack` and `SharedStack` so an
  application can share a caller-built event-sourced stack with its HTTP
  handlers. Limits: in-process event bus only (`EVENT_BUS=nats` fails at serve
  with an error naming the aggregate); the caller owns projection rebuild at
  startup; the stack and `DATABASE_URL`/`DATABASE_DRIVER` must name the same
  database.
- SQLite event-store and read-model connections now use a 5-second busy timeout.
  Appends acquire an IMMEDIATE write transaction before reading the current
  version, so competing writers queue instead of failing immediately. A lock
  held longer than 5 seconds can still fail. Journal mode remains unchanged;
  see [Upgrading](docs/upgrading.md). This behavior change is not yet released.
- This candidate has not been published; release 0.8.12 needs authorization.
