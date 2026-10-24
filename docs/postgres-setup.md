# Postgres Setup Guide

## Overview

Arc supports **Postgres** as an alternative storage backend for both events and read models. This is controlled via the `DATABASE_DRIVER` environment variable.

## Configuration

To enable Postgres support:

1. **Build with the `postgres` feature**:
   ```bash
   cargo build --features postgres
   ```

2. **Configure environment variables**:
   ```env
   DATABASE_DRIVER=postgres
   DATABASE_URL=postgres://user:password@localhost:5432/database_name
   ```

## Schema Initialization

Unlike the SQLite implementation which uses Diesel migrations, the Postgres implementation (`arc-es-postgres`) handles its own schema initialization.

The `initialize_schema()` method is called during application startup (in `build_stores`) and creates the following tables if they do not exist:

- `events`: The append-only event log.
- `snapshots`: The aggregate snapshots table.
- `users_view`: The default projection table for users.

This "self-healing" schema boundary ensures that the database is always ready for use without requiring manual migration steps during initial setup or testing.

### Event payload representation for origin validation

The Phase 2 PostgreSQL change stores event payloads as `JSON` rather than
`JSONB`. PostgreSQL `JSONB` normalizes JSON values, including numeric
representations; preserving the original serialized number form is needed for
strict comparison of a delivered event with its persisted authoritative event.
This change applies only to the event log's payload column. Projection
read-model documents remain `JSONB`.

Existing schemas are upgraded with an idempotent forward migration. On
2026-10-24, the focused PostgreSQL 16 suite passed 26/26 tests, covering
integrity disabled/enabled, JSON numeric round-trips, a signed legacy JSONB
upgrade, migration idempotency, tamper rejection without projection mutation,
and numeric command → publish → external projection. The broader
`bash tests/bounded-reads/run.sh` harness exited 0 with PostgreSQL-enabled Rust
tests, a generated fixture (5/5), and the browser workflow. These are local test
results; they do not establish that the migration has been applied to any
external or deployed database. Historical signatures that no longer validate
after prior JSONB normalization remain invalid and fail closed. They are not
resealed or rewritten to make verification pass.

## Live Testing

To run the Postgres-specific tests against a live database, set the `ARC_POSTGRES_TEST_DATABASE_URL` environment variable:

```bash
ARC_POSTGRES_TEST_DATABASE_URL=postgres://user:password@localhost:5432/test_db cargo test -p arc-es-postgres
```

## Using Docker Compose

The project includes a `docker-compose.yml` with a Postgres service. To start it:

```bash
docker compose up -d postgres
```

By default, it is configured to use port **5433** to avoid conflicts with local Postgres instances. You can adjust this in `docker-compose.yml`.

The default connection URL for local development with Docker is:
`postgres://arc:password@localhost:5433/arc_dev`

## Startup Validation

Use the bundled compose services for a local parity check:

```bash
docker compose up -d postgres nats
ARC_POSTGRES_TEST_DATABASE_URL=postgres://arc:password@127.0.0.1:5433/arc_dev cargo test -p arc-es-postgres
DATABASE_DRIVER=postgres DATABASE_URL=postgres://arc:password@127.0.0.1:5433/arc_dev cargo run -p arc --features postgres -- migrate
```

For app startup checks, build with the `postgres` feature and set
`DATABASE_DRIVER=postgres`. The app calls both event-store and read-model
`initialize_schema()` during `build_stores`. Durable event consumption and
routing in distributed mode run in Benthos (Redpanda Connect), not a Rust
process — see
`architecture-decisions/0001-benthos-only-event-routing.md` and
`event-handlers.md`. Benthos must never write directly to the
database; projection writes should go through an Arc-owned handler/service that
uses the configured read-model store. Run the app (or `migrate`) once against
the database before starting the routing layer so the Arc-owned projection path
has the expected schema.

## Production Readiness

- **Optimistic Concurrency**: Postgres implementation uses `UNIQUE(aggregate_id, sequence)` and a version-check query within a transaction to ensure consistency.
- **Idempotent Upserts**: Read-model upserts use `ON CONFLICT (id) DO UPDATE ... WHERE version < EXCLUDED.version` to ensure that replay and out-of-order delivery converge to the correct state.
- **Audit Metadata**: Audit fields (who, when, from where, why) are persisted inline on each event row.
- **Integrity Metadata**: When `EVENT_INTEGRITY_KEY` is configured, signatures and key ids are persisted inline on each event row and verified on load/stream.
