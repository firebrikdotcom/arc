# Troubleshooting

## `arc-web-cli` cannot be found

Update the crates.io index and install:

```bash
cargo install arc-web-cli
```

Confirm:

```bash
arc --version
```

## Port 8080 is busy

Set another port in `.env`:

```dotenv
APP_PORT=8081
```

Then run `make dev`.

## The database is missing

Run:

```bash
make setup
```

or, if `.env` already exists:

```bash
make migrate
```

## A projector fails with “no such table”

Every SQLite read model needs an application migration:

```sql
CREATE TABLE products_view (
    id TEXT PRIMARY KEY NOT NULL,
    version BIGINT NOT NULL,
    data TEXT NOT NULL
);
```

Add the migration, then run `make migrate`.

## The application exits without serving

The generated entry point reports the cause. Check:

1. `make setup` completed.
2. `.env` contains `APP_URL`, `DATABASE_URL`, and `SECRET_KEY`.
3. `DATABASE_URL` is writable.
4. `APP_PORT` is free.

## Authentication examples are missing

This is intentional rather than hidden: a complete generated authentication feature is not shipped yet. Session infrastructure alone is not authentication.
