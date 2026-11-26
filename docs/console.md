# Application Console

`arc console` opens an [irust](https://github.com/sigmaSd/IRust) REPL prepared
for the Arc application in the current directory, so you can inspect its data
and call Arc and application code interactively, the way `artisan tinker` is
used in Laravel.

```bash
cd my-app
arc console
```

```text
In: users().len()
Out: 1
In: users().iter().map(|u| (u.email.clone(), u.roles.clone())).collect::<Vec<_>>()
Out: [("admin@example.com", ["admin"])]
```

## Requirements

irust is an external tool and is not installed with Arc:

```bash
cargo install irust
```

`arc console` looks for `irust` on `PATH` and stops with that install command
when it is missing. Run it from the root of an application created by
`arc new`; anywhere else it refuses without writing anything. Android projects
(`arc new --android`) are not supported.

## Usage

```bash
arc console [--no-app-modules] [-- IRUST_ARGS...]
```

| Option | Effect |
| --- | --- |
| `--no-app-modules` | Do not compile the application's own `src/` modules into the session. |
| `-- IRUST_ARGS...` | Pass the remaining arguments to irust, for example `-- --bare-repl`. |
| `-h`, `--help` | Show help. |

The first launch builds the console crate. Because it uses the application's
`Cargo.lock` and target directory, the dependencies the application has
already compiled (with `make dev` or `cargo build`) are reused; only the small
console crate is compiled. A launch on an application that has never been
built compiles its dependencies once.

## What the session has

irust imports the generated `irust_prelude` crate into every session:

- **The application's dependencies** at the application's locked versions,
  re-exported under their crate names: `arc_core`, `arc_web`, `arc_auth_db`,
  `diesel`, `serde_json`, and so on. `[dev-dependencies]` and optional
  dependencies are not included.
- **The application's `.env`**, loaded at the start of every evaluation (the
  generated startup file calls `load_env()`). Variables already set in your
  shell win, as in the application's own `main`. The working directory is the
  application root, so the default `DATABASE_URL=database/database.sqlite`
  resolves to the application's database.
- **The application's modules**: every top-level `mod name;` in `src/main.rs`
  is compiled in place from `src/`, so `domain::...`, `routes::...` and the
  rest can be named. A module that only compiles inside the application binary
  (for example one that uses an item defined in `main.rs` itself) is skipped
  with a warning, and the session starts without it.
- **Helpers**:

| Helper | Returns |
| --- | --- |
| `load_env()` | Loads `.env` (already called for you). |
| `env(key)` | An environment variable after loading `.env`. |
| `database_url()` | `DATABASE_URL`, defaulting to `database/database.sqlite`. |
| `block_on(future)` | Runs an async Arc API to completion on a shared Tokio runtime. |
| `stores()` | The event store and read-model store, opened from `DATABASE_DRIVER` and `DATABASE_URL` like the server does. |
| `read_models()` | The read-model store (`Arc<dyn ReadModelStore>`). |
| `rows(table)` | Every row of a read-model table, for example `rows("products_view")`. |
| `event_store()` | The event store (`Box<dyn EventStore>`). |
| `events(aggregate_id)` | The events of one aggregate, oldest first. |
| `identities()` | The arc-auth-db `DbIdentityStore` (only with arc-auth-db). |
| `users()` | Every arc-auth-db user with roles (only with arc-auth-db). |

The `ReadModelStore`, `EventStore`, `IdentityStore`, `Identity`, `Event` and
`Row` types are in scope, so their methods can be called directly.

## Worked example: list the users

After `arc plugin add auth-db-session` and `make setup`, the users live in the
plugin's `users` table. In the console:

```text
In: users().len()
Out: 1
In: users().into_iter().map(|u| u.email).collect::<Vec<_>>()
Out: ["admin@example.com"]
In: block_on(identities().get("<user id>"))
Out: Ok(Some(Identity { id: "...", name: "Admin", email: "admin@example.com", active: true, roles: ["admin"] }))
```

Read models work the same way, and application types can be used directly:

```text
In: rows("products_view").len()
Out: 3
In: block_on(read_models().get("products_view", "<id>"))
Out: Ok(Some(Object {...}))
```

## Generated files

Everything the console writes lives in `target/arc-console/` inside the
application:

| Path | Purpose |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | A workspace holding the console crate and irust's host crates; the lockfile is a copy of the application's. |
| `irust/irust_prelude/` | The generated prelude crate (`IRUST_DATA_DIR`). It links, read-only, to the application's top-level folders so `include_dir!("$CARGO_MANIFEST_DIR/...")` still resolves. |
| `irust_repls/` | irust's temporary host crates (`IRUST_TEMP_DIR`). |
| `startup.rs` | The startup file irust loads (`load_env();`). |

These files are regenerated on every launch. Builds use the application's
target directory (`target/`, or `CARGO_TARGET_DIR`); irust's per-session host
binaries are removed from it when the session ends. The application's source,
`.env` and database are never modified while the session is prepared. Delete
`target/arc-console/` at any time to reset the console. Your irust
configuration and history are used unchanged.

## Limits

- **Every evaluation reruns the session.** irust compiles and runs all the
  statements entered so far each time you evaluate something. A statement that
  writes (dispatching a command, `create_user`, ...) runs again on every later
  evaluation. Do writes as a single expression without a trailing `;`, or
  remove the statement with `:pop`. `:reset` clears every statement, including
  the startup `load_env();` (the helpers still load `.env` themselves; call
  `load_env()` again before reading raw `std::env::var` values).
- **What you can do is what the application exposes.** Private items of
  application modules are not reachable, and items defined in `src/main.rs`
  itself (for example the `builder()` function or `MIGRATIONS`) are not
  available. Modules that depend on them are skipped.
- Opening `stores()` or `identities()` connects to the real database the
  application uses. Writes through them are real writes; Arc store
  constructors may also create missing Arc tables, as the server does on start.
- `env!("CARGO_PKG_NAME")` inside application modules evaluates to
  `irust_prelude`, and modules that need the application's `build.rs` output
  are skipped.
- `[target.'cfg(...)'.dependencies]` sections are not copied.
- Run one console per application at a time; sessions share
  `target/arc-console/`. A session killed abruptly can leave its host crate in
  `irust_repls/`; it is harmless and is removed by deleting the folder.
- In terminals that report a zero window size (some automation harnesses),
  irust itself can crash on start. A normal terminal, or `-- --bare-repl` for
  scripted use, avoids it.
