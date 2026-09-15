# Bounded collections

Generated resource API and browser collections and the authentication user admin
use storage-level pagination. SQLite and Postgres projection stores filter and
order before applying SQL `LIMIT` and `OFFSET`. They fetch at most one extra row
to determine whether a next page exists; they do not load the collection into Rust.

## HTTP contract

- Resource APIs retain the JSON array response. `limit` defaults to 20 and accepts
  1–100; `offset` defaults to zero and accepts 0–1,000,000. `X-Has-Next` is `true`
  or `false`; it becomes false at the last permitted offset even if more rows
  exist. Requests without pagination now return the first page.
- Browser resource and user collections show 20 records per page. `page=0` maps
  to page 1. Invalid numeric input, overflowing pages, or offsets above 1,000,000
  return HTTP 400. Resource sorting accepts `name_asc` (default) or `name_desc`.
- Filters accept at most 1024 UTF-8 bytes and reject NUL. Matching is literal
  substring matching with ASCII case folding; `%` and `_` are ordinary characters.
  Non-ASCII case folding is not performed. Resource filtering matches names;
  user filtering matches trimmed name/email search text.
- Paging links retain the active filter and resource sort. User pagination shows
  the current page and next/previous links without calculating a total count.

Ordering uses the primary key for APIs and name plus ascending primary key for
browser resources. Identity collections order by email then primary key. SQL
backends use binary/C collation for consistent ties. Concurrent writes can shift
offset pages; pagination does not promise a snapshot across requests.

## Store contract and limits

`ReadModelStore::collection` takes a `CollectionQuery` and returns a
`CollectionPage`. `IdentityStore::collection` uses the same window and returns
identities. Custom implementations must implement bounded reads; there is no
load-all default implementation. Each backend validates the query again before
SQL, including when callers modify its public fields.

The existing `list` and `find_by` methods retain their explicit all-results
semantics for internal callers; they should not implement public collection
endpoints. This change bounds rows materialized, not total database work: filters
and sorting may scan rows. Large installations should add appropriate indexes
or introduce cursor pagination for their workload. The auth database plugin still
supports SQLite only; Postgres coverage applies to projection collections.

Regression coverage includes in-memory, SQLite and live Postgres paging, tie
ordering, literal/multibyte filters, invalid windows and result bounds. The SQLite
sentinel test detects decoding outside the requested window plus lookahead.
`tests/bounded-reads/browser.mjs` exercises a freshly generated `bounded-fixture`
with session authentication, resource API/browser pagination and user-admin
navigation in Chromium.

Run `bash tests/bounded-reads/run.sh` with Docker Compose and installed Node /
Playwright dependencies (`npm ci`). Set `CHROMIUM_PATH` for a non-default Chromium
path or `ARC_BOUNDED_PORT` for a different host port. An optional
`ARC_BOUNDED_COMPOSE_FILE` can select a lane-specific Compose file with an
existing verifier image and disk-backed cache; its container paths must remain
`/work` and `/cache`. The harness builds backend
code only in Compose, caps Cargo at two jobs, starts temporary Postgres, generates
a fresh app, verifies its locked dependency graph, and removes its labeled
containers and cache volume on exit. The browser checks the health response's
application name before testing to reject a stale server on the port.

The CI `bounded-collections` job runs this harness and is required by CI Success.
Local verification on 2026-09-14 passed 301 workspace tests (including doc tests),
5 generated-app tests, live Postgres/SQLite regressions, Chromium E2E, locked
Clippy, documentation builds, and debug/optimized release builds. The full
maintained harness also passed in its newly built CI image and cleaned up its
containers and network. Existing ignored documentation examples and
unrelated known-gap tests retain their prior status.
