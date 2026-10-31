# Changelog

## Unreleased

## 0.9.0 — 2026-10-31

Published to crates.io. `arc-stack` and `arc-ffi` are published for the first time.

### Breaking changes (Phase 2 security hardening)

Details and migration steps: [Upgrading](docs/upgrading.md#phase-2-security-hardening-090--compatibility-notes).

- The `/ws` handshake fails closed unless the app configures an origin
  allowlist and a principal provider ([Upgrading](docs/upgrading.md#phase-2-security-hardening-090--compatibility-notes)).
- Room subscriptions outside `user:<id>` are silently dropped
  ([Upgrading](docs/upgrading.md#phase-2-security-hardening-090--compatibility-notes)).
- Postgres `events.payload` is rewritten from JSONB to JSON at startup
  ([Upgrading](docs/upgrading.md#phase-2-security-hardening-090--compatibility-notes)).
- Event timestamps are stored at whole-second precision
  ([Upgrading](docs/upgrading.md#phase-2-security-hardening-090--compatibility-notes)).

### Fixed

- `arc new` no longer lets inherited `GIT_DIR` and related variables (for example under a Git hook) redirect `git init` to the caller's repository.

### Security

- Update `actix-http` to 3.20.0 (zstd 0.14.3, zstd-safe 8.2.0) to fix RUSTSEC-2026-0391 and RUSTSEC-2026-0403 in the locked workspace and `cargo install --locked` CLI; downstream apps resolve their own lock.
- Repair the Cargo.lock and package-lock.json dependency audit findings.

### Added and changed

- Phase 2 security hardening: WebSocket origin and identity authorization, and
  projection event-origin controls (see `docs/phase-2-security-hardening.md`).

- Add `arc new --android`, `arc doctor --android`, and
  `arc generate resource --ffi` for native Android app scaffolding. Generated
  projects depend on the published `arc-core`, `arc-stack`, and `arc-ffi`
  crates pinned to the CLI's own version; `--arc-path PATH` or `ARC_PATH`
  opts into a local Arc checkout for framework development.

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
  see [Upgrading](docs/upgrading.md).
