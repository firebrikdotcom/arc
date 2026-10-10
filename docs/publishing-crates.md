# Publishing Arc Crates

Arc's reusable crates are published in dependency order:

1. `arc-core`
2. `arc-es-sqlite`
3. `arc-es-postgres`
4. `arc-es-nats`
5. `arc-web`
6. `arc-auth-core`
7. `arc-auth-db`, `arc-auth-session`, `arc-auth-jwt`, and `arc-auth-rbac`
8. `arc-auth-admin`
9. `arc-web-cli`

The storage and NATS crates depend on `arc-core` by version, with a local `path` retained for
workspace development. Publish or make `arc-core` available in the registry before running
`cargo publish --dry-run` for the adapter crates.

## Preflight

Run the package isolation gate first. It packages every reusable crate,
compiles each packaged crate's tests, and compiles `arc-web` from a temporary
consumer outside the workspace:

```bash
make publish-check
```

This gate is required because `cargo publish --dry-run` can accidentally see
workspace-root files that will not exist when a downstream project downloads
the crate.

For a new lockstep version, crates.io requires each dependency to exist before
it will package the next crate. Run the same gate and publish sequentially:

```bash
make publish-check PUBLISH_CRATES=arc-core
cargo publish -p arc-core

make publish-check PUBLISH_CRATES="arc-es-sqlite arc-es-postgres arc-es-nats"
cargo publish -p arc-es-sqlite
cargo publish -p arc-es-postgres
cargo publish -p arc-es-nats

make publish-check PUBLISH_CRATES=arc-web
cargo publish -p arc-web

make publish-check PUBLISH_CRATES="arc-auth-core arc-auth-db arc-auth-session arc-auth-admin arc-auth-jwt arc-auth-rbac"
cargo publish -p arc-auth-core
cargo publish -p arc-auth-db
cargo publish -p arc-auth-session
cargo publish -p arc-auth-admin
cargo publish -p arc-auth-jwt
cargo publish -p arc-auth-rbac

make publish-check PUBLISH_CRATES=arc-web-cli
cargo publish -p arc-web-cli
```

Publish in dependency order:

```bash
cargo publish -p arc-core --dry-run
cargo publish -p arc-es-sqlite --dry-run
cargo publish -p arc-es-postgres --dry-run
cargo publish -p arc-es-nats --dry-run
cargo publish -p arc-web --dry-run
cargo publish -p arc-web-cli --dry-run
```

If network access is unavailable, `cargo package -p arc-core --allow-dirty --offline --no-verify`
can still validate the `arc-core` package archive. Adapter crates need registry resolution for
`arc-core`, so offline package checks will fail until `arc-core` is present in the local registry
cache.

## Public API Policy

All six reusable crates are pre-1.0 and versioned in lockstep. Within a
compatible minor release line:

- Keep `arc-core` traits source-compatible unless there is a documented migration.
- Treat the event envelope, `EventStore`, `ReadModelStore`, `EventBus`, `Aggregate`, and projection
  traits as public API.
- Treat handler manifest semantics and Benthos envelope fields as integration API.
- Prefer additive changes for read-model, event, audit, session, and integrity structs.
- Document breaking changes in release notes and bump the minor version before `1.0`.

Do not publish adapter crates with direct database-writing Benthos behavior. Benthos remains the
routing/delivery layer; Arc-owned code owns projection writes.
