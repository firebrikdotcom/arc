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
| `dispatch::<A>(command)` | Dispatches an aggregate command through `CommandBus<A>` with the projectors `src/main.rs` registers for `A`; returns `Result<Vec<Event>, String>`. Replay-guarded (see below). |
| `dispatch_with::<A>(command, projectors)` | The same with an explicit `Vec<ProjectorReg>`, for an aggregate the console cannot find in `src/main.rs`. |

The `ReadModelStore`, `EventStore`, `IdentityStore`, `Identity`, `Event`,
`Row`, `Aggregate`, `Command` and `ProjectorReg` types are in scope, so their
methods can be called directly.

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

## Writing through commands

`dispatch::<A>(command)` changes application state the way the server does:
it opens the application's stores from `DATABASE_DRIVER`/`DATABASE_URL`,
builds a `CommandBus<A>` on an in-process event bus with the projectors that
`src/main.rs` registers for `A` (`.register_aggregate::<A>()` followed by
`.register_projector(..)` or `.register_projectors(..)`), and dispatches the
command. Its events are appended to the event store and projected at once, so
`rows(..)` shows the change immediately. Nothing is written with SQL. Events
are stamped with the `system` actor, the session id `arc-console-<session>`
and the user agent `arc console`.

After `arc generate resource post`:

```text
In: use domain::post::{aggregate::PostAggregate, commands::PostCommand};
In: let created = dispatch::<PostAggregate>(PostCommand::Create { id: "post-1".into(), name: "First".into() });
In: 1 + 1
Out: 2
Err: arc console: dispatched to Post post-1: 1 event(s) written
In: events("post-1").len()
Out: 1
In: created.clone().map(|events| events.len())
Out: Ok(1)
In: rows("posts_view")
Out: [Object {"id": String("post-1"), "name": String("First"), "version": Number(1)}]
```

irust runs a statement ending in `;` for the first time with the next
evaluation, which is why the write is reported under `1 + 1`. irust shows
anything printed to stderr after `Err:`.

The write helpers refuse when:

- `EVENT_BUS=nats`: the console's in-process bus would not publish the events
  to NATS or Benthos. Make that write through the running application.
- `A` is not registered with `.register_aggregate::<A>()` in `src/main.rs`
  (for example an aggregate added by a plugin or one registered with
  `register_aggregate_with_stack`), or that registration did not compile in
  the console. Use `dispatch_with::<A>(command, vec![ProjectorReg::new(MyProjector, MY_VIEW)])`.
- The session was not started by `arc console`.

A failing command (for example `Create` for an id that already exists) returns
`Err` with the aggregate's message, as `CommandBus::dispatch` does.

### Replays write once

irust compiles and runs every kept statement again on each evaluation. The
write helpers keep a journal of the writes made in this session, so the
statement above dispatched once even though it ran again on every later
evaluation:

- **Session.** One `arc console` launch. It sets `ARC_CONSOLE_SESSION` (a new
  id) and `ARC_CONSOLE_DIR`. The journal lives in
  `target/arc-console/sessions/<session>.jsonl` and is deleted when irust exits.
- **Identity of a write.** The aggregate type, the source text of the statement
  that calls the helper, and how many times that same statement text has
  already written in the current evaluation. A loop that dispatches three
  commands is three writes. Two identical statements entered one after the
  other are two writes. Values inside the statement do not matter, so a
  statement that generates a new id on every run (`Uuid::new_v4()`) still
  writes once.
- **Replay.** A statement that already wrote returns its first result,
  including a recorded `Err`, without dispatching. A command that failed is
  never retried silently after a later write would let it succeed.
- **Interrupted writes.** A write is recorded as pending before it is
  dispatched. If an evaluation dies before recording the result, later runs
  of that statement return an error instead of retrying. Check `events(id)`,
  then restart `arc console` to write again.
- **Evaluated expressions.** A write in the expression being evaluated (input
  without a trailing `;`, which irust does not keep) always runs, once, and
  is not journaled. Evaluating it again writes again.

Limits of the guard:

- Re-entering the exact text of a statement that already wrote (for example
  after `:pop` or `:reset`) replays it. The console then prints
  `already wrote earlier in this session` and returns the first result. To
  write again, change the statement, evaluate it as an expression, or restart
  the console.
- The guard protects against irust's replays inside one session. It does not
  coordinate with the running server or with another console.
- The statement text is read from irust's host crate. If it cannot be read,
  the call's line and column identify the write instead.

## Generated files

Everything the console writes lives in `target/arc-console/` inside the
application:

| Path | Purpose |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | A workspace holding the console crate and irust's host crates; the lockfile is a copy of the application's. |
| `irust/irust_prelude/` | The generated prelude crate (`IRUST_DATA_DIR`). It links, read-only, to the application's top-level folders so `include_dir!("$CARGO_MANIFEST_DIR/...")` still resolves. |
| `irust_repls/` | irust's temporary host crates (`IRUST_TEMP_DIR`). |
| `startup.rs` | The startup file irust loads (`load_env();`). |
| `sessions/` | The write journal of the running session (see [Replays write once](#replays-write-once)); removed when the session ends. |

These files are regenerated on every launch. Builds use the application's
target directory (`target/`, or `CARGO_TARGET_DIR`); irust's per-session host
binaries are removed from it when the session ends. The application's source,
`.env` and database are never modified while the session is prepared. Delete
`target/arc-console/` at any time to reset the console. Your irust
configuration and history are used unchanged.

## Limits

- **Every evaluation reruns the session.** irust compiles and runs all the
  statements entered so far each time you evaluate something. `dispatch` and
  `dispatch_with` guard against this (see
  [Replays write once](#replays-write-once)). Other writes, such as
  `block_on(identities().create(..))` or a raw `EventStore::append`, are not
  guarded and run again on every later evaluation: do them as a single
  expression without a trailing `;`, or remove the statement with `:pop`.
  `:reset` clears every statement, including the startup `load_env();` (the
  helpers still load `.env` themselves; call `load_env()` again before reading
  raw `std::env::var` values).
- **What you can do is what the application exposes.** Private items of
  application modules are not reachable, and items defined in `src/main.rs`
  itself (for example the `builder()` function or `MIGRATIONS`) are not
  available. Modules that depend on them are skipped.
- Opening `stores()`, `identities()` or a `dispatch` connects to the real
  database the application uses. Writes through them are real writes; Arc
  store constructors may also create missing Arc tables, as the server does on
  start.
- On Postgres, a generated application's `migrate` command does not create
  resource read-model tables. Create the table before dispatching, or the
  projection fails.
- `arc console` is tested on Linux only. On Windows it does not link the
  application's top-level files into the prelude, so modules that read them at
  compile time (for example with `include_str!`) may be skipped.
- `dispatch` does not apply the snapshot policy (`.snapshot_policy(..)`).
  Snapshots are only a load cache, so the written events are the same.
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
