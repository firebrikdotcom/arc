# Published Packages

The latest published package version is **0.8.11**. Source version **0.8.11** gives
the generated admin layout a collapsible navigation rail (`public/workbench.js`) and
makes the repository app use the same Workbench design. Install this source release
directly from its tag:

```bash
cargo install --git https://github.com/firebrikdotcom/arc --tag v0.8.11 arc-web-cli --locked --force
```

All twelve matching 0.8.11 packages are published to crates.io. The newly added
`arc-stack` and `arc-ffi` packages are source-only 0.9.0 candidates and are not
published; no release has been authorized. Generated
applications resolve the same framework and authentication-plugin versions.
Version 0.8.10 adds
command-specific, read-only CLI help, and all twelve 0.8.10 packages are published
to crates.io.
Version 0.8.9 includes
the merged security workstreams and publishes all twelve Arc crates under
`MIT OR Apache-2.0`, with the repository set to
<https://github.com/firebrikdotcom/arc>. Crates.io publication for this release is
performed manually in dependency order. Tagging also triggers the GitHub release,
prebuilt binaries and documentation site workflows.

Generated applications use published crates; they do not depend on an Arc repository checkout.

| Package | Purpose |
|---|---|
| `arc-web-cli` | Installs the `arc` generator command |
| `arc-core` | Commands, aggregates, events, command bus, projections, read models |
| `arc-stack` | Headless stack assembly (source addition, unpublished) |
| `arc-ffi` | Async native adapter runtime and shared core registry (source addition, unpublished; see [Native Adapters](ffi.md)) |
| `arc-web` | Actix runtime, server builder, middleware, and application wiring |
| `arc-es-sqlite` | SQLite event, read-model, snapshot, and session stores |
| `arc-es-postgres` | Optional Postgres stores |
| `arc-es-nats` | Optional NATS JetStream event publishing for Arc's distributed topology |
| `arc-auth-core` | Identity and authorization contracts |
| `arc-auth-db` | SQLite identity and role provider |
| `arc-auth-session` | Browser session authentication protocol and guard |
| `arc-auth-admin` | Sign-in, profile, password, and user-management UI |
| `arc-auth-jwt` | API token issuance and JWT guard |
| `arc-auth-rbac` | Role-based authorization policy and middleware |

> The in-process event bus remains the zero-dependency default and is read-after-write
> consistent. For distributed delivery, select `EVENT_BUS=nats`; Benthos (Redpanda Connect)
> consumes `events.>` and routes generated handler pipelines. The routing generator, config
> freshness check, lint gate, and projection-routing integration coverage are implemented.

## Installation model

The CLI writes normal Cargo dependencies:

```toml
[dependencies]
arc-core = "0.8.11"
arc-web = "0.8.11"
```

You may create the same application manually, but then you must also supply the environment bootstrap, migrations, aggregate, routes, and runtime entry point that the CLI normally generates.

## Versioning

Keep Arc packages on the same version unless release notes explicitly say otherwise. Review dependency updates like any framework upgrade and run:

```bash
make setup
make check
make test
```

Before publishing a workspace release, run `make publish-check`. It packages the
selected crates together, then compiles their tests and an external consumer using
only extracted archives for local Arc dependencies. This supports versions that
are not yet on crates.io. Publication still requires a dry run and upload for each
crate in dependency order.

Authentication packages and their installation bundles are documented in
[Authentication Plugins](auth-plugins.md).
