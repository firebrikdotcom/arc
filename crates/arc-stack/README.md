# arc-stack

`arc-stack` owns headless event-sourced stack construction shared by Arc UI
adapters. It exposes `EsStack`, `SharedStack`, `ProjectorReg`, driver
configuration, and storage/stack builders without depending on Actix, Tera,
`arc-web`, or authentication crates.

The `sqlite` feature is enabled by default; `postgres` enables Postgres stores.
Both driver variants remain available in the API, and selecting a backend that
was not compiled in returns an error. See [ADR 0003](../../docs/architecture-decisions/0003-shared-stack-ui-adapters.md)
and [package documentation](../../docs/packages.md) for compatibility and
release status.
