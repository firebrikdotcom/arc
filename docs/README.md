# Arc

Arc is a Rust framework for building event-sourced Actix Web applications.

Start here:

```bash
cargo install arc-web-cli
arc new my-app --ui
cd my-app
make setup
make dev
```

This documentation covers the application you receive from `arc new`: what is included, where to put your code, and how to build working features with published Arc APIs.

Arc separates headless stack construction (`arc-stack`) from its UI adapters:
`arc-web` serves web deployments and `arc-ffi` supports native UIs. The Android
app is FFI-only; web UIs are for web deployments. These adapter crates are
0.9.0 candidates and are not yet published.

## What Arc gives your application

- An Actix Web server configured through `ArcApp`
- SQLite event, snapshot, read-model, and session storage
- A `CommandBus` that loads aggregates, persists events, and publishes them
- In-process projections with read-after-write consistency
- Cookie-session, rate-limit, compression, tracing, and path middleware
- Application-owned migrations and environment configuration
- Optional Tera page and static assets with `--ui`
- Optional database, session, JWT, admin UI, and RBAC authentication plugins
- Resource generation for event-sourced APIs and browser pages

Authentication is installed explicitly with `arc plugin add`; it is not implied by `--ui` alone.
See [Authentication Plugins](auth-plugins.md).

## Build something

- [Create an application](getting-started.md)
- [Understand every generated source file](project-structure.md)
- [Follow request, domain, view, and form lifecycles](workflows.md)
- [Add an endpoint](endpoints.md)
- [Build an event-sourced resource](resources.md)
- [Install and configure authentication plugins](auth-plugins.md)
- [Add a server-rendered page](ui.md)
- [Test the application](testing.md)
- [Fix common startup problems](troubleshooting.md)

## License

Arc is licensed under either of the MIT license ([LICENSE-MIT](https://github.com/firebrikdotcom/arc/blob/master/LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/firebrikdotcom/arc/blob/master/LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.
