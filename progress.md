# Arc Project Progress

**Last updated:** 2026-09-13
**Current development version:** 0.8.7
**Latest published version:** 0.8.6
**Status:** v0.8.7 integrates the four security workstreams, validated locally with
workspace tests, live storage/routing integration, browser E2E and static checks.
Package publication remains at v0.8.6, which is released successfully. The reusable framework, standalone application
generator, authentication plugins, Benthos routing/generation, and generated-app UI are
implemented and validated. Remaining roadmap work focuses on security hardening, extensibility,
observability, and developer experience.

This is the canonical project-status and roadmap tracker. Current source code and accepted ADRs
take precedence when a claim here becomes stale.

## Input security lane — verified 2026-09-13

- Generated resource aggregates now reject names over 1024 UTF-8 bytes on create and
  rename, preserving accepted text verbatim and retaining blank-name validation.
- Generated browser pagination saturates adversarial page offsets rather than
  overflowing or wrapping. Collection storage reads remain unbounded.
- Verified through isolated Docker Compose: 10 CLI tests, 5 generated-app tests,
  workspace/generated formatting, CLI/generated Clippy with warnings denied, and
  HTTP checks for oversized writes, unchanged projected state after rejection,
  escaped HTML-like names, and the maximum page number.

## Executive Summary

Arc has evolved from a traditional Actix/Diesel MVC starter into a 13-crate, event-sourced Rust
workspace (12 publishable framework/plugin packages plus the `arc` application):

- `arc-core`: event sourcing, aggregates, commands, projections, read models, audit, access,
  session, snapshot, and integrity primitives.
- `arc-es-sqlite`: SQLite event, read-model, snapshot, and session storage.
- `arc-es-postgres`: Postgres event, read-model, and snapshot storage.
- `arc-es-nats`: NATS JetStream event publishing and stream provisioning.
- `arc-web`: reusable Actix runtime, middleware, helpers, server wiring, and WebSockets.
- `arc-web-cli`: published `arc` generator command that creates standalone applications.
- `arc-auth-*`: opt-in identity, session, admin UI, JWT, RBAC, and persistence plugins.
- `arc-app` (package `arc`): thin application containing the User domain, controllers, routes,
  validation, templates, assets, and seeders.

The physical `arc-web` / thin `arc-app` split, multi-aggregate registration, and aggregate-scoped
stream identity are committed. The default event path is the in-process bus. In distributed mode,
`arc-es-nats` publishes persisted events and Benthos (Redpanda Connect) is the implemented,
tested, sole durable router. The removed `arc-worker` is historical.

## Current Priorities

1. **Add generated-app upgrade assistance**
   - Define a CLI workflow that reports the generated template version and applicable migrations.
   - Detect framework-owned generated-file drift without overwriting application-owned code.
   - Exercise the ADR 0002 upgrade path against a generated application such as `arc-test-1`.

2. **Complete the generated-app developer workflow**
   - `arc new <name>` now scaffolds a standalone application, migrations, `.env.example`, and
     Makefile without requiring a framework checkout.
   - Document and exercise the upgrade path after generated application customization.
   - Exercise upgrades against a generated application such as `arc-test-1`.

3. **Keep user documentation canonical and current**
   - The `docsify-docs/` user guide now covers application anatomy, workflows, endpoints,
     resources, UI, configuration, testing, packages, `web::Data`, and the command bus.
   - `docsify-docs/` is the canonical user-facing source.
   - CI checks release-version references and implemented routing claims against source.

4. **Harden CI and audits**
   - Keep NATS/Benthos integration coverage reliable in CI.
   - Consolidate duplicated/fragile `nats-server` installation and startup logic.
   - Complete the intended `make audit` workflow.

### Most recently completed

- v0.8.6 was tagged and its Release workflow completed successfully.
- Benthos routing and handler-manifest generation are implemented: generated pipelines are checked
  for freshness, linted in CI, and covered by NATS → Benthos → Arc-owned projection integration
  tests. Benthos has no direct Arc database writes.
- Canonical Docsify documentation and the roadmap now track v0.8.6; CI has a documentation
  freshness guard for release-version and routing-status claims.
- The generated-app Instrument Panel foundation landed: Focused and Dense layouts, semantic CSS
  tokens, shared Tera components, truthful dashboard states, browser/session protection, CSRF,
  Argon2 production bootstrap credentials, revocable JWT issuance, JWT-protected resource APIs,
  and `arc generate resource <name> --ui` projection-backed browser CRUD scaffolding.
- Clean-room minimal/UI generation now compiles, tests, formats, passes Clippy, rejects anonymous
  API access, issues a registered bearer token, and completes authenticated API CRUD.
- Arc 0.4.2 was published and externally verified; `arc-web-cli` can now generate optional JSON
  CRUD APIs with `arc generate resource <name> --api`.
- Arc 0.4.1 was published and externally verified; it adds the first-class aggregate/resource
  generator to `arc-web-cli`.
- Arc 0.4.0 was published and externally verified using a crates.io-installed CLI and a
  crates.io-only generated application.
- The lockfile was refreshed from yanked `spin 0.9.8` to `spin 0.9.9`.
- `arc generate resource <name>` now creates and registers aggregate, command, event, projector,
  migration, and test files with collision protection and 0.4.0 generated-app compatibility.
- `arc generate resource <name> --api` now adds registered JSON create/list/get/update/delete
  endpoints; writes use `CommandBus`, reads use projections, and the API remains optional.

## Validation

### Latest local validation

- 2026-09-08: the v0.8.6 Release workflow completed successfully.
- 2026-09-08: CI, Security, and the Release workflow dry run were manually validated before the
  v0.8.6 release.
- 2026-08-08: all six Arc 0.5.1 packages were published; a crates.io-installed CLI generated a
  fresh empty UI app and Product API/UI resource, both compiling with warnings denied.
- 2026-08-08: all six Arc 0.5.0 packages were published in dependency order; a crates.io-installed
  CLI generated a fresh Instrument Panel application and Product `--api --ui` resource whose
  tests, formatting, and all-target Clippy checks passed using only public packages.
- 2026-08-07: all six Arc 0.4.2 packages were published in dependency order; a crates.io-installed
  CLI generated a fresh CRUD-enabled Product resource whose live create/get/update/delete flow,
  three stored events, tests, formatting, and Clippy checks passed using only public packages.
- 2026-08-07: clean-room minimal and UI apps generated optional Product CRUD APIs; runtime HTTP
  create/list/get/update/delete, projection updates, and three-event persistence checks passed,
  along with workspace tests, formatting, and all-target/all-feature Clippy.
- 2026-08-07: all six Arc 0.4.1 packages were published in dependency order; a crates.io-installed
  CLI created a fresh UI application and Product resource that migrated, tested, formatted, and
  passed Clippy using only public 0.4.1 packages.
- 2026-08-07: workspace tests and all-target/all-feature Clippy passed; clean-room minimal and UI
  applications generated a Product resource, migrated, compiled, tested, and passed Clippy.
- 2026-08-06: Arc 0.4.0 was published in dependency order, and a fresh application generated by
  the crates.io-installed CLI compiled using only public 0.4.0 packages.
- 2026-07-31: Arc 0.4.0 formatting, all-target/all-feature Clippy, full workspace tests, doctests,
  NATS/Benthos routing integration, and clean-room minimal/UI scaffold checks passed.
- 2026-07-31: a sibling `arc-test-1` application compiled against local Arc 0.4.0 packages with
  no duplicate Arc versions.
- 2026-07-30: Arc 0.3.0 was published in dependency order, and a fresh application generated by
  the crates.io-installed CLI compiled using only public packages.
- 2026-07-28: `make doctor`, formatting, all-target/all-feature Clippy, and the frontend build
  passed.
- 2026-07-28: `cargo test --workspace --all-features` passed, including live NATS publishing and
  Benthos projection-routing integration coverage.
- 2026-07-28: publication dry-runs passed for `arc-core`, `arc-es-sqlite`, `arc-es-postgres`,
  `arc-es-nats`, and `arc-web`.

### Earlier infrastructure validation

- 2026-06-16: Postgres live adapter validation passed against the Compose database.
- 2026-06-16: Postgres application startup reached schema initialization and projection rebuild.
- 2026-06-17: User snapshot policy and core snapshot mechanics passed.
- 2026-06-18: Benthos generation, contract tests, config freshness, and lint checks passed.
- 2026-06-21: Publishable metadata was prepared for the four core/storage/event crates;
  `arc-core` passed a publish dry-run.
- 2026-07-04: `make doctor` / `make arc-check` generated-file drift checks landed.

## Completed Foundations

### Event-sourced domain

- [x] `Event`, `EventStore`, `Aggregate`, `Command`, and `CommandBus` foundations.
- [x] Optimistic concurrency and audit validation.
- [x] User aggregate commands and event-sourced write path.
- [x] Projection-backed `users_view`.
- [x] Typed `ReadModelStore` operations.
- [x] Deterministic projection replay from the event log.
- [x] Cookie sign-in reads the projection; profile/password mutations use `CommandBus`.
- [x] Legacy mutable `users` and `user_email_index` tables removed.
- [x] User snapshots at configurable intervals (`USER_SNAPSHOT_INTERVAL_EVENTS`, default 50).
- [x] Multiple aggregate types can be registered in one `ArcApp` server, each with its own typed
  `CommandBus<A>`, projectors, and snapshot policy.
- [x] Event construction uses the named-field `NewEvent` parameter struct instead of unclear
  positional arguments.

### Storage

- [x] SQLite event, read-model, snapshot, and JWT-session stores.
- [x] Postgres event, read-model, and snapshot stores.
- [x] `DATABASE_DRIVER=sqlite|postgres` application selection.
- [x] Sequence/timestamp storage widened to `i64`.
- [x] Event integrity signatures and verification for SQLite and Postgres.
- [x] Event streams and snapshots are keyed by `(aggregate_type, aggregate_id)`, preventing
  collisions when different aggregate types use the same instance ID.
- [x] SQLite migration and idempotent Postgres schema upgrade cover aggregate-scoped identity.

### Distributed eventing and routing

The in-process event bus remains the zero-dependency, read-after-write-consistent default.
Distributed event delivery is implemented and tested: `arc-es-nats` publishes persisted events,
and Benthos owns durable routing, retries, dedupe, dead-lettering, and HTTP/NATS delivery.

- [x] NATS JetStream publishing through `arc-es-nats`.
- [x] Subject naming: `events.<aggregate_type>.<event_type>` in snake_case.
- [x] Idempotent event-stream and `ARC_DLQ` provisioning.
- [x] Benthos selected as the sole durable routing layer in ADR 0001.
- [x] Removed the bespoke Rust `arc-worker` consumer.
- [x] Base and generated Benthos pipelines.
- [x] Handler-manifest compiler with unknown-key and unsupported-value rejection.
- [x] HTTP and NATS delivery; SQL/database delivery prohibited.
- [x] Envelope validation, retry semantics, dedupe, and enriched `x_arc_dlq` metadata.
- [x] Arc-owned HTTP projection endpoint and routing integration coverage.
- [x] DLQ/redrive operator guide.
- [x] NATS → Benthos → Arc-owned projection integration suite.
- [x] Generated-pipeline freshness and Redpanda Connect lint CI gates.

### Security foundations

- [x] Event `AuditMetadata`, validated by command bus and event stores.
- [x] Access logging primitives, sensitivity classification, and failure policies.
- [x] `Sensitive<T>` → `AccessLogged<T>` response discipline for audited reads.
- [x] Session/cookie authentication for browser and admin routes.
- [x] JWT bearer authentication and server-side revocation for API routes.
- [x] CSRF protection for HTML forms.
- [x] Rate limiting.
- [x] Admin idle timeout using projection-backed `SessionUser`.
- [x] HMAC-SHA256 event integrity chain.

### Framework readiness

- [x] ADR 0002 defines ownership, SemVer, public API, and upgrades.
- [x] `arc-web` / thin `arc-app` boundary implemented and committed on `master`.
- [x] `make doctor` checks generated Benthos freshness and structural ownership.
- [x] User-facing upgrade guide.
- [x] Commit the current framework split.
- [x] Validate publication packages for all reusable crates in dependency order.
- [x] Publish all versioned 0.3.0 `arc-*` crates, including `arc-web` and `arc-web-cli`.
- [x] Ship `arc new` at version 0.3.0.
- [x] Support multiple aggregates in one application server.
- [x] Verify generated applications outside the workspace with local release packages.
- [x] Publish the lockstep 0.4.0 release.
- [x] Add aggregate/resource generation to `arc-web-cli`.
- [ ] Extend generated-file checks to Diesel schema and frontend output where practical.

### Delivery and operations

- [x] CI checks formatting, Clippy, tests, and frontend builds.
- [x] CI checks canonical documentation freshness against the workspace version and routing state.
- [x] Security workflow runs dependency auditing.
- [x] Dockerfile and health-checked Compose services for app, Postgres, NATS, and Benthos.
- [x] Browser E2E infrastructure.
- [x] Tera/Vite/Tailwind/Stimulus/Turbo frontend stack.
- [x] Hashed asset caching and `/public/*` serving.

## Documentation Work

### Missing user documentation

- [ ] `docs/tutorials/01-adding-your-first-aggregate.md`
- [ ] `docs/tutorials/02-adding-a-projection.md`
- [x] `docs/guides/getting-started.md`
- [ ] `docs/guides/event-sourcing-concepts.md`
- [ ] `docs/guides/testing-aggregates.md`
- [x] Replace the Docsify user guide with functional app, endpoint, resource, UI, configuration,
  testing, troubleshooting, package, and architecture workflows.
- [x] Document the command bus and Actix `web::Data` in user-facing language.
- [ ] Reconcile the root README's “Adding a New Entity” workflow with current generated-app APIs.

### Reference reconciliation

- [ ] `docs/01-overview.md`: seven-crate workspace and ES-first architecture.
- [ ] `docs/02-architecture.md`: current layer and deployment diagrams.
- [ ] `docs/03-backend.md`: `arc-web` paths and CommandBus write path.
- [ ] `docs/04-frontend.md`: verify actual JS stack and build commands.
- [ ] `docs/05-database.md`: remove legacy mutable User model/table descriptions.
- [ ] `docs/06-testing.md`: event-sourced domain, projection, NATS, and Benthos testing.
- [ ] `docs/07-api-reference.md`: verify routes, auth modes, and response shapes.
- [ ] `docs/08-problems-and-improvements.md`: close or remove resolved findings.
- [ ] `docs/09-event-sourcing-architecture.md`: distinguish current design from proposals.
- [ ] `docs/10-event-sourcing-implementation-guide.md`: replace pre-implementation phases.
- [ ] `docs/11-event-sourcing-api-reference.md`: align traits with current source.
- [ ] Label old QA, implementation, and planning documents as historical.
- [x] Rewrite `docsify-docs/` as the focused user-facing guide.
- [ ] Consolidate deployment so only the maintained Docsify source is presented to users.

## Remaining Product Roadmap

### Phase 2 — Security hardening

Security primitives are implemented; they do not establish legal compliance or production readiness.
The four dispatched security workstreams below are complete, merged into `master`,
and included in the local `v0.8.7` tag (2026-09-13). The source-grounded threat model
continues to track open controls and deployment decisions.

- [x] Security headers and a documented CSP posture: shared runtime defaults, explicit HTML MIME types, configurable enforced/report-only CSP and opt-in HSTS. Verified with focused Rust tests, generated auth/API/browser flows, and Vite/Turbo asset checks; see [Security Headers and CSP](docsify-docs/security-headers.md).
- [x] Input validation and adversarial security coverage for the dispatched scope:
  generated resource names are limited to 1024 UTF-8 bytes, pagination arithmetic
  saturates safely, and rejection/state-preservation/HTML-escaping cases are tested.
  Unit, HTTP and browser threat-model coverage is recorded in the
  [coverage matrix](docsify-docs/security-test-coverage.md).
- [x] Production review of access-log persistence and operations (2026-09-13): see
  [findings and operator runbook](docsify-docs/access-logging-production-review.md).
  Wrapper bypasses and diagnostic disclosure fixed; focused Compose tests and
  all-feature static analysis passed. Durable sink implementation, disclosure
  coverage, and deployment-specific privacy/retention decisions remain open.
- [x] Source-grounded [threat model](docsify-docs/threat-model.md) and actionable
  [release security checklist](docsify-docs/release-security-checklist.md) documented (2026-09-12).
  Documentation is complete; identified risks, release gates and deployment validation remain open.

Remaining security follow-ups:

- [ ] Bound collection storage reads and extend adversarial coverage beyond the completed input-validation scope.
- [ ] Implement durable audit persistence and resolve deployment-specific disclosure, privacy and retention policies.
- [x] Implement durable optional-plugin browser-session invalidation after role, active-state and password changes, logout and detected idle expiry. Old cookies cannot regain access after identity restoration; store failures deny access. See [auth plugins](docsify-docs/auth-plugins.md).
- [ ] Resolve room authorization and projection-origin validation gaps identified by the threat model.

### Phase 3 — Plugin and hook system

- [ ] Finalize hook extension boundaries against ADR 0002 ownership rules.
- [ ] Define async hook behavior and failure isolation.
- [ ] Implement plugin discovery/loading without requiring framework forks.
- [ ] Provide at least one maintained example plugin.

### Phase 4 — Testing and quality

- [x] Restore the full distributed integration suite locally.
- [ ] Keep the distributed integration suite stable in CI across supported environments.
- [ ] Improve test isolation and external-service lifecycle cleanup.
- [ ] Add explicit coverage targets and reporting.
- [x] Add canonical documentation freshness auditing in CI.
- [ ] Add broader architecture drift auditing.
- [ ] Keep Docker resources project-scoped and reliably cleaned up.

### Phase 5 — Performance and observability

- [ ] Establish reproducible performance baselines and budgets.
- [ ] Add metrics for commands, projections, NATS publishing, Benthos delivery, and DLQ depth.
- [ ] Add distributed tracing/correlation across writer, Benthos, and handlers.
- [ ] Document production logging, alerting, and health-check expectations.

### Phase 6 — PWA

- [ ] Decide whether PWA support belongs in the core framework or an optional template/plugin.
- [ ] Add manifest/installability if accepted.
- [ ] Add safe asset/offline caching compatible with authenticated and Turbo flows.

### Phase 7 — CI/CD and releases

- [x] Core CI and security workflows.
- [ ] Stabilize external NATS/Benthos provisioning.
- [x] Validate crate publishing in dependency order.
- [x] Publish and externally verify the lockstep 0.3.0 packages.
- [x] Publish and externally verify the lockstep 0.4.0 packages.
- [x] Publish and externally verify the lockstep 0.4.1 resource-generator release.
- [x] Publish and externally verify the lockstep 0.4.2 generated-CRUD-API release.
- [x] Tag v0.8.6 and complete its Release workflow successfully.
- [ ] Establish release notes and migration-note enforcement.
- [ ] Exercise the ADR 0002 upgrade path against a generated sample application.

### Phase 8 — Developer experience

- [x] `arc new` with standalone migrations, environment example, Makefile, and optional UI files.
- [x] Aggregate and projection generators with current module paths.
- [x] Complete the standalone getting-started and first-resource Docsify tutorials.
- [x] Add a CLI-driven first aggregate/resource workflow.
- [x] Add optional generated CRUD API routes for immediate resource interaction.
- [ ] Improve actionable diagnostics from `make doctor`.
- [ ] Make crate and framework API documentation publish-ready.

### Phase 9 — Strategic architecture

- [ ] Decide whether Postgres read-model storage should become `arc-rm-postgres`.
- [ ] Broaden handler manifests and DLQ/redrive tooling without database-writing Benthos outputs.
- [ ] Evaluate additional storage backends behind existing traits.
- [ ] Define the 1.0 compatibility and support policy.

## Decisions and Boundaries

- The event log is authoritative; projections are rebuildable read models.
- Writes go through commands and aggregates.
- In-process event delivery is the synchronous, read-after-write-consistent local default.
- NATS is the distributed publish lane when `EVENT_BUS=nats` is selected.
- Benthos is the sole durable consumer/router of `events.>` in distributed mode.
- Benthos never writes Arc databases.
- Event handlers are external and declared through manifests.
- Browser/admin authentication uses sessions; APIs use JWT bearer authentication.
- Framework code belongs in versioned `arc-*` crates; application/domain code belongs in the
  thin app.
- Generated artifacts are not hand-edited.

## Source of Truth

When sources disagree:

1. Current code in `crates/` and `migrations/`
2. Accepted ADRs in `docsify-docs/architecture-decisions/`
3. Current guides in `docsify-docs/`
4. This file
5. Historical plans, implementation notes, and QA reports

## Threat-model coverage — 2026-09-13

Added unit, real HTTP and Chromium security coverage, including explicit open-gap
reproductions and separate desired-security probes. Fixed strict JWT missing-jti and
missing-store bypasses. See [coverage matrix](docsify-docs/security-test-coverage.md).
Durable audit storage, cached-session policy, room authorization and projection
origin validation remain open; passing characterization tests does not close them.
