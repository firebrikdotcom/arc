# AGENTS.md

## Feedback Log

- **Keep deployment audit policy open while technical Phase 2 proceeds.** (2026-10-23) Prepare a document that structures the owner conversation about deployment audit policy; do not invent defaults or mark approval. Continue the technical Phase 2 fixes while that decision remains pending.

- **Documentation and roadmap must identify v0.8.6 as released, and describe Benthos routing/generation as implemented and tested; CI must prevent this documentation drift.** (2026-09-08)
- **Tag and push the validated CI/CD repair as the next patch release.** (2026-09-08) Use a versioned release tag after the manual CI, Security, and Release dry-run validations have passed.
- **Manually trigger GitHub CI and Security to confirm repaired workflows; treat Release separately because dispatching it creates a real release.** (2026-09-08) Run non-publishing checks safely on master, and require a deliberate version/tag decision before a release dispatch.
- **Historical failed GitHub workflow runs should be clearly distinguished from the repaired current pipeline.** (2026-09-08) Verify whether any active workflow still fails; old runs retain their original conclusion and cannot be rewritten by a later workflow fix.
- **Repair the GitHub CI/CD pipeline after releasing the admin breadcrumb work.** (2026-09-07) Diagnose current GitHub failures from their logs and validate the fix before publishing it.
- **The generated admin needs accessible breadcrumbs across its nested pages.** (2026-09-07) Confirm the page hierarchy before implementation; the root crumb is Home.
- **Admin breadcrumbs must name the `/admin` root “Home,” not “Overview.”** (2026-09-07) The dashboard is the hierarchy root; nested admin trails begin with Home.
- **Do not call a refreshed sample environment ready until the intended process, not a stale server on the same port, has been identified and browser-verified.** (2026-09-07) A health check alone can hit an old process and falsely validate a release.
- **Arc’s generated admin UI must visibly confirm successful actions, beginning with profile save; establish layered visual feedback tests and release the complete fix.** (2026-09-07) Use persistent, accessible success/error feedback and verify it in the generated sample app.
- **Arc’s generated UI is missing feedback and needs a deliberate solution.** (2026-09-07) Treat visible user feedback as a product gap; determine whether it concerns validation, success/error notices, loading state, or design-review input before implementing.
- **Release framework fixes fully for beta verification: commit, tag, push, publish, update the CLI, then recreate the test app from scratch.** (2026-09-07) Do not rely on a local-path workaround when validating a framework fix intended for generated applications.
- **Arc development bootstrapping and CLI registration must accept user-chosen passwords without a production-length minimum.** (2026-09-07) Keep production-strength guidance, but do not block manual local/admin setup for a short development password.
- **Arc is still beta; do not prioritize generated-app upgrade assistance or legacy compatibility.** (2026-09-07) Prefer forward product work unless compatibility work is explicitly requested.

## Purpose

Operational guide for agents working in this repository. Keep changes aligned with the current Rust workspace, event-sourced architecture, and roadmap.

## Current State

- Rust workspace with active crates including:
  - `crates/arc-core`: event sourcing primitives, aggregates, command bus, event bus traits, projections, read-model traits, audit/access/session/integrity primitives.
  - `crates/arc-es-sqlite`: SQLite event store, read-model store, snapshot persistence, and JWT session revocation store.
  - `crates/arc-es-postgres`: Postgres event store, read-model store, and snapshot persistence (self-initializing schema).
  - `crates/arc-es-nats`: NATS JetStream `EventBus` implementation (the publish side).
  - `crates/arc-stack`: headless event-sourced stack assembly shared by adapters.
  - `crates/arc-ffi`: native async runtime and shared core registry for UniFFI consumers.
  - `crates/arc-app`: Actix Web application, Tera templates, Vite assets, auth, API/admin routes, user domain wiring.
- Backend framework: Actix Web.
- Write model: command handlers and aggregates through `CommandBus`.
- Event store/read model: SQLite by default; Postgres available via `DATABASE_DRIVER=postgres`.
- Server-side rendering: Tera templates in `crates/arc-app/src/resources/views/`.
- Frontend assets: Vite, Stimulus, Turbo, Toastify. Styling is the single Workbench design: `crates/arc-app/src/resources/css/styles.css` imports `crates/arc-cli/templates/public/styles.css`, and Vite copies `crates/arc-cli/templates/public/workbench.js` (collapsible rail) to `dist/`. Change the design there so `arc new --ui` apps and the repository app stay identical.
- Auth modes:
  - Session/cookie auth for HTML/admin routes.
  - JWT bearer auth for `/api/*`.
- Realtime: WebSocket support lives in `arc-web`; the reference app no longer mounts `/ws`.
- Distributed event lane: `arc-es-nats` publishes persisted events to NATS JetStream; **Benthos (Redpanda Connect)** is the single routing and event-handler delivery layer that consumes `events.>`. See `docs/architecture-decisions/0001-benthos-only-event-routing.md` and `docs/event-handlers.md`. (The earlier `arc-worker` consumer crate has been removed; treat any reference to it as historical.)

Important: older planning docs can lag behind code. If docs conflict, prefer current source and the source-of-truth order below.

## Repository Map

- `Cargo.toml`: workspace members and shared dependency versions.
- `crates/arc-core/src/`: ES interfaces and framework primitives.
- `crates/arc-es-sqlite/src/`: SQLite implementations.
- `crates/arc-es-nats/src/`: NATS JetStream event bus (publish side).
- `crates/arc-es-postgres/src/`: Postgres event/read-model/snapshot stores.
- `crates/arc-app/src/main.rs`: app entrypoint and command dispatch.
- `crates/arc-app/src/commands/`: serve, migrate, seed, develop commands.
- `crates/arc-app/src/routes.rs`: HTML/API/admin scopes, static asset serving, middleware wiring.
- `crates/arc-app/src/http/controllers/`: HTTP handlers.
- `crates/arc-app/src/http/middlewares/`: auth, JWT, idle timeout, rate limiting.
- `crates/arc-app/src/helpers/`: sessions, CSRF, templates, forms, and JWT; shared stack assembly is owned by `arc-stack` and `arc-web` retains compatibility re-exports.
- `crates/arc-app/src/domain/user/`: User aggregate, commands, projector.
- `crates/arc-app/src/resources/`: CSS, JS, images, Tera views.
- `config/benthos/`: Benthos (Redpanda Connect) routing pipeline(s) that consume `events.>`.
- `config/handlers/`: event-handler manifests (planned) compiled into Benthos pipelines.
- `migrations/`: Diesel SQL migrations.
- `docs/`: canonical user-facing documentation rendered by Docsify. Every user-visible
  framework, CLI, plugin, configuration, setup, or workflow change must update this docs set.
- `AGENTS.md`: operational rules for agents; retain and update alongside architectural changes.
- `progress.md`: canonical roadmap/status record; retain and keep current.
- `progress.md`: canonical project status, remaining work, and roadmap.
- `todo-audit.md`: audit follow-up checklist when present.

## Preferred Commands

- Setup: `make setup`
- Start dev mode: `make dev`
- Run server only: `make serve`
- Run migrations: `make migrate`
- Seed database: `make seed`
- Full DB setup: `make db-setup`
- Tests: `make test`
- Fast compile check: `make check`
- Lint: `make lint`
- Format: `make format`
- Upgradeability drift guard: `make doctor` (alias: `make arc-check`)
- Frontend build: `make frontend-build`

`make test` and `make lint` should match CI-level workspace/all-features coverage.

## Architecture Rules

### Request Handling

- Public HTML routes and admin HTML routes are server-rendered with Tera.
- Admin routes live under `/admin` and require session auth.
- API routes live under `/api`; protected API routes use JWT middleware.
- HTML form POSTs must preserve CSRF protection.

### Event Sourcing

- Writes should go through `CommandBus` and aggregate commands.
- Exception: opt-in `arc-auth-db` identity and role infrastructure uses conventional `users`,
  `roles`, and `user_roles` tables owned by that plugin. This does not change event-sourcing rules
  for application domain resources or the built-in `arc-app` User aggregate.
- Read models are projection outputs, not authoritative write state.
- `users_view` is maintained by `UserProjector`.
- Event log remains the source of truth.
- Snapshot infrastructure is active: `UserAggregate` serializes/restores snapshots and the app command-bus wiring uses `USER_SNAPSHOT_INTERVAL_EVENTS` (default 50), so user writes create best-effort snapshots at a configurable interval.

### Event Bus / Routing

- `InProcessEventBus` is the default local synchronous path (`EVENT_BUS=inprocess`); it drives projections in the writer process and is read-after-write consistent.
- `arc-es-nats` publishes persisted events to JetStream when `EVENT_BUS=nats` is selected. The writer's responsibility ends at append + publish.
- **Benthos (Redpanda Connect)** is the single durable consumer of `events.>` in distributed mode: it owns routing, filtering, dedupe, retries, dead-lettering, and handler delivery. There is no Rust consumer of `events.>`.
- Benthos must never write directly to Arc databases. Projection writes must run through Arc-owned code paths (for example an internal HTTP projection endpoint/service that uses `Projector`/`ProjectionEngine`/`ReadModelStore`).
- Event handlers are external to Arc. Add one with a handler manifest (`config/handlers/<name>.yaml`) that the generator compiles into a Benthos pipeline — never by editing a Rust crate. See `docs/event-handlers.md`.
- NATS-backed tests spawn a local `nats-server -js`; CI must provision a real `nats-server` binary.

### Auth Rules

- Session auth is the primary path for browser/admin pages.
- JWT auth is the path for API clients.
- Do not weaken one auth flow while changing the other.
- `set_session_user()` stores a projection-backed `SessionUser` under the `"user"` session key; avoid adding unnecessary DB reads to authenticated HTML flows.
- Idle timeout must read `SessionUser`, not the retired `"user_id"` key.
- `arc-auth-session` caches both its role-bearing identity and the standard `SessionUser`; admin
  middleware must include idle-timeout enforcement.

### Database Rules

- SQLite implementations live in `arc-es-sqlite`; Postgres implementations live in `arc-es-postgres`.
- Schema changes require a Diesel migration and affected seeder/test updates (Postgres self-initializes its schema in `build_stores`).
- Auth capability schema is plugin-owned; each supported database driver must have an explicit
  migration path, and unsupported drivers must fail clearly during setup.
- New storage backends go behind the existing `EventStore` and `ReadModelStore` traits instead of changing domain code.

### Frontend Rules

- Tera templates are the rendering layer.
- Asset references are injected through template helpers and the Vite manifest.
- Public assets are served from `/public/*` via `dist/`.
- Preserve hashed asset caching behavior in `crates/arc-app/src/routes.rs`.

### Testing Rules

- Existing tests rely on SQLite and some use serial execution.
- Auth/profile tests can touch migrations, seeders, session middleware, CSRF behavior, and projections.
- If you change auth, forms, migrations, projection behavior, or session behavior, update or add focused tests.
- If you change NATS publishing behavior, cover publish acks, subject naming (`events.<aggregate_type>.<event_type>`, snake_case), and event serialization. Routing/consumer behavior lives in Benthos pipelines (`config/benthos/`), validated with `benthos lint` and routing integration tests. Projection integration tests should prove Benthos calls Arc-owned projection code; do not test or introduce Benthos SQL/database writes.
- Docker-backed tests must use project-scoped names and labels, for example `arc-test-*` plus `arc.project=arc`, and must remove their containers on every success, skip, timeout, and failure path. Never leave anonymous NATS/Benthos test containers running.

## Current Priorities

### Immediate

- Land `config/benthos/` routing pipelines and the handler-manifest → Benthos-config generator (`make benthos-config`), plus a `benthos lint` CI gate. See `docs/architecture-decisions/0001-benthos-only-event-routing.md`.
- Reconcile high-level docs that still describe the old MVC-only layout.

### Near-Term

- Keep `docs/` aligned with the current generated application and published packages.
- Improve audit checks for stale paths and CI assumptions.
- Tighten controller/service/domain boundaries as more aggregates are added.

### Strategic

- Introduce plugin/hook system.
- Make core/storage crates publishable.
- Broaden the Benthos routing plane (more HTTP/NATS handler manifests, richer DLQ/redrive tooling) without adding database-writing Benthos outputs.

## Known Gaps And Risks

- High-level docs still contain historical MVC-era wording.
- Duplicate/old planning documents can be mistaken for current truth.
- NATS integration tests require a `nats-server` binary to exercise live JetStream behavior; otherwise they skip.

## Change Guidance

- Prefer small, reversible changes unless the task explicitly requires restructuring.
- For auth/security work, verify session, CSRF, JWT, and audit behavior together.
- For template work, verify Tera rendering and asset manifest assumptions.
- For schema work, update migrations, seeders, and tests in one pass.
- Keep all maintained project documentation, including ADRs and engineering guides, under
  `docs/`. Do not create a second documentation tree.
- For roadmap/documentation work, update the source-of-truth docs instead of adding another planning document.
- Do not commit unless the user explicitly asks or approves.

## Source Of Truth Order

When sources disagree, use this order:

1. Current source code in `crates/` and `migrations/`
2. Accepted ADRs under `docs/architecture-decisions/`
3. Current documentation in `docs/`
4. `progress.md`
5. Root `AGENTS.md`
