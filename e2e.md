# Browser E2E Tests

Playwright drives a real Chromium browser against a real `arc` server
binary, with audit-trail verification routed through a backend-agnostic diag
endpoint (so it keeps working when Step 5 swaps SQLite for Postgres).

## What gets tested

| Spec | Surface |
|------|---------|
| `tests/e2e/specs/api-register.spec.ts` | `POST /api/v1/register` → audit fields land (anonymous actor, `User-Agent`, `X-Correlation-Id`) |
| `tests/e2e/specs/api-profile-flow.spec.ts` | Register → login → JWT → PATCH → GET → DELETE → 404, with audit `actor_id` transition |
| `tests/e2e/specs/ui-signin.spec.ts` | `/signin` HTML form: CSRF token extraction, cookie session, redirect to `/admin`, bad-password rejection, CSRF tampering |
| `tests/e2e/specs/security-assets.spec.ts` | Security headers preserve Vite assets, caching/304, Turbo and Toastify |
| `tests/e2e/specs/ui-signout.spec.ts` | `/signout` clears the session and `/admin` bounces to `/signin` |

## Backend-agnostic audit verification

The diag endpoint at `/__diag__/events/{aggregate_id}` is mounted **only when
`APP_ENV=e2e`** (see `crates/arc-app/src/routes.rs`). It reads through
the `EventStore` trait — exactly the same code path as `CommandBus` — so it
returns identical results whether the store is SQLite, Postgres, or
in-memory. Production builds never expose it.

The Playwright `audit` fixture wraps that endpoint:

```ts
const events = await audit.events(aggregateId);
expect(events[0].audit.actor_id).toBe('anonymous');
```

No DB driver in the test runner. `better-sqlite3` is intentionally **not** a
dependency.

## Running

```bash
# One-time
make e2e-install        # installs @playwright/test + Chromium

# Every run
make e2e                # build, run, tear down
make e2e-headed         # same, with a visible browser
make e2e-report         # open the latest HTML report
```

Run backend builds and tests inside the project Docker Compose environment.
The launcher honors `CARGO_TARGET_DIR` when locating the built `arc` binary.
Set `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to use a system Chromium installation
(for example `/usr/bin/chromium` in a verification container); otherwise Playwright
uses its downloaded browser. Video recording also requires Playwright’s FFmpeg
(`npx playwright install ffmpeg`).

The `e2e` target shells through to `playwright.config.ts`, which spawns the
binary via `tests/e2e/global-setup.ts`. That setup script:

1. Loads `.env.e2e` (deterministic secrets, port 18080, `DATABASE_URL=database/database-e2e.sqlite`)
2. `cargo build --bin arc` (skipped if `E2E_SKIP_BUILD=1`)
3. `npm run build` for Vite assets
4. Deletes any stale `database/database-e2e.sqlite{,-shm,-wal}`
5. `arc migrate` then `arc seed` → `jekyll@example.com` / `password`
6. Spawns `arc serve` and waits for both `GET /health` and `GET /__diag__/health`
7. Records PID + port in `.e2e-state.json`

`global-teardown.ts` SIGTERMs the PID, deletes the DB files, and removes
the state file. If the test run dies mid-flight, run `make e2e` again — the
fresh setup overwrites whatever was left behind.

## Adding a spec

1. Drop a `*.spec.ts` file under `tests/e2e/specs/`.
2. Import `test`, `expect`, and any helpers from `../fixtures`.
3. Use `request` for API calls, `page` for browser interactions, `audit` for
   event-stream assertions, `uniqueEmail()` to avoid email collisions.

```ts
import { test, expect, aggregateIdFromRegister } from '../fixtures';

test('something', async ({ request, audit, uniqueEmail }) => {
  const id = await aggregateIdFromRegister(request, uniqueEmail());
  const events = await audit.events(id);
  expect(events).toHaveLength(1);
});
```

## Forensics

When a test fails:

- HTML report: `make e2e-report` (or `npx playwright show-report`)
- Trace viewer for retried tests: `npx playwright show-trace test-results/<spec>-<test>/trace.zip`
- Video on retain-on-failure: `test-results/<spec>-<test>/video.webm`
- Server logs: by default the spawned server inherits stdout/stderr — they
  print directly into the terminal that ran `make e2e`

## Why one worker

Single Playwright worker = single server = single DB file. The framework
team can scale up to file-per-worker when test count justifies the
complexity of port allocation, parallel `cargo run` invocations, and per-DB
seeding. Until then, serial is fast enough (the suite runs in seconds) and
debugging is dramatically simpler.

## CI

A minimal GHA workflow lives in `.github/workflows/e2e.yml` (add when
needed). It needs:

- Rust toolchain + `~/.cargo`/`target` cache
- Node 20 + `~/.npm` cache
- `npx playwright install --with-deps chromium`
- `make e2e`
- Upload `playwright-report/` and `test-results/` on failure
