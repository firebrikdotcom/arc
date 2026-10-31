# Publishing Arc Crates

Arc's reusable crates are published in dependency order:

1. `arc-core`
2. `arc-auth-core`
3. `arc-es-sqlite`, `arc-es-postgres`, and `arc-es-nats`
4. `arc-stack`
5. `arc-web` and `arc-ffi` (independent siblings)
6. `arc-auth-db`, `arc-auth-session`, and `arc-auth-jwt`
7. `arc-auth-admin`
8. `arc-auth-rbac` (its packaged tests depend on `arc-auth-admin`)
9. `arc-web-cli`

The storage and NATS crates depend on `arc-core` by version, with a local `path` retained for
workspace development. Publish or make `arc-core` available in the registry before running
`cargo publish --dry-run` for the adapter crates.

## Preflight

Run the package isolation gate first. It packages every reusable crate,
compiles each packaged crate's tests, and compiles `arc-stack`, `arc-web`, and the `arc-ffi` concrete native fixture from temporary
consumers outside the workspace:

```bash
make publish-check
```

This gate is required because `cargo publish --dry-run` can accidentally see
workspace-root files that will not exist when a downstream project downloads
the crate.

Arc is dual licensed under MIT or Apache-2.0, set once in the workspace
`license` field. Cargo packages only files inside a crate's directory, so each
published crate carries `LICENSE-MIT` and `LICENSE-APACHE` symlinks to the
root files; Cargo copies their contents into the archive. Give a new publishable
crate the same two symlinks, and confirm them with
`cargo package --list -p <crate>`.

For a new lockstep version, crates.io requires each dependency to exist before
it will package the next crate. Run the same gate and publish sequentially:

```bash
make publish-check PUBLISH_CRATES=arc-core
cargo publish -p arc-core

make publish-check PUBLISH_CRATES=arc-auth-core
cargo publish -p arc-auth-core

make publish-check PUBLISH_CRATES="arc-es-sqlite arc-es-postgres arc-es-nats"
cargo publish -p arc-es-sqlite
cargo publish -p arc-es-postgres
cargo publish -p arc-es-nats

make publish-check PUBLISH_CRATES=arc-stack
cargo publish -p arc-stack

make publish-check PUBLISH_CRATES=arc-ffi
cargo publish -p arc-ffi

make publish-check PUBLISH_CRATES=arc-web
cargo publish -p arc-web

make publish-check PUBLISH_CRATES="arc-auth-db arc-auth-session arc-auth-jwt"
cargo publish -p arc-auth-db
cargo publish -p arc-auth-session
cargo publish -p arc-auth-jwt

make publish-check PUBLISH_CRATES=arc-auth-admin
cargo publish -p arc-auth-admin

make publish-check PUBLISH_CRATES=arc-auth-rbac
cargo publish -p arc-auth-rbac

make publish-check PUBLISH_CRATES=arc-web-cli
cargo publish -p arc-web-cli
```

Publish in dependency order:

```bash
cargo publish -p arc-core --dry-run
cargo publish -p arc-auth-core --dry-run
cargo publish -p arc-es-sqlite --dry-run
cargo publish -p arc-es-postgres --dry-run
cargo publish -p arc-es-nats --dry-run
cargo publish -p arc-stack --dry-run
cargo publish -p arc-ffi --dry-run
cargo publish -p arc-web --dry-run
cargo publish -p arc-web-cli --dry-run
```

If network access is unavailable, `cargo package -p arc-core --allow-dirty --offline --no-verify`
can still validate the `arc-core` package archive. Adapter crates need registry resolution for
`arc-core`, so offline package checks will fail until `arc-core` is present in the local registry
cache.

## Public API Policy

All thirteen reusable crates are pre-1.0 and versioned in lockstep. Within a
compatible minor release line:

- Keep `arc-core` traits source-compatible unless there is a documented migration.
- Treat the event envelope, `EventStore`, `ReadModelStore`, `EventBus`, `Aggregate`, and projection
  traits as public API.
- Treat handler manifest semantics and Benthos envelope fields as integration API.
- Prefer additive changes for read-model, event, audit, session, and integrity structs.
- Document breaking changes in release notes and bump the minor version before `1.0`.

Do not publish adapter crates with direct database-writing Benthos behavior. Benthos remains the
routing/delivery layer; Arc-owned code owns projection writes.

`arc-stack` and `arc-ffi` were first published in 0.9.0, published to crates.io with the
other twelve packages; the release inventory is now fourteen packages that move together.
The native adapter requires exact UniFFI 0.32.2; host binding generation is separate
from its shipped dependencies. See [Native Adapters](ffi.md) for runtime, registry,
feature isolation and the external Kotlin/Swift component smoke.
