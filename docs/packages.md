# Published Packages

The latest published package version is **0.8.6**. Source version **0.8.8** contains
the merged security workstreams. Tagging a release publishes the GitHub release,
its prebuilt binaries and the documentation site, but crates.io publication is
gated on the `PUBLISH_CRATES` repository variable and is currently disabled, so
the published package version stays at 0.8.6. The installation examples below
continue to target published packages.

Generated applications use published crates; they do not depend on an Arc repository checkout.

| Package | Purpose |
|---|---|
| `arc-web-cli` | Installs the `arc` generator command |
| `arc-core` | Commands, aggregates, events, command bus, projections, read models |
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
arc-core = "0.8.6"
arc-web = "0.8.6"
```

You may create the same application manually, but then you must also supply the environment bootstrap, migrations, aggregate, routes, and runtime entry point that the CLI normally generates.

## Versioning

Keep Arc packages on the same version unless release notes explicitly say otherwise. Review dependency updates like any framework upgrade and run:

```bash
make setup
make check
make test
```

Authentication packages and their installation bundles are documented in
[Authentication Plugins](auth-plugins.md).
