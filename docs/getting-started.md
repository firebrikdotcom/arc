# Create Your First Arc App

Create a self-contained application without cloning the Arc repository.

## Requirements

- Stable Rust
- `make`
- SQLite development libraries

## 1. Install the generator

```bash
cargo install arc-web-cli
```

The package installs the `arc` command.

## 2. Create a project

Minimal server:

```bash
arc new my-app
```

Server-rendered UI with Tera views and browser assets:

```bash
arc new my-app --ui
```

Arc initializes a Git repository by default without creating a commit. Pass `--no-git` to skip it.

## 3. Set up and run

```bash
cd my-app
make setup
make dev
```

`make setup` creates `.env` with a random local secret, creates the SQLite database, and runs the generated application's migrations. It is idempotent and preserves an existing `.env`.

Open:

- Minimal and UI health check: <http://127.0.0.1:8080/health>
- UI home page: <http://127.0.0.1:8080/home>

Stop the server with `Ctrl+C`.

## 4. Add authentication

For browser sessions, administration pages, JWT API tokens, and RBAC:

```bash
arc plugin add auth-db-session
arc plugin add auth-db-jwt
make setup
```

The first setup prompts for administrator credentials. See
[Authentication Plugins](auth-plugins.md) for noninteractive variables and capability details.

## 5. Generate your first resource

From the generated application root:

```bash
arc generate resource Product --api --ui --api-auth jwt
make migrate
make test
```

Arc creates and registers a Product aggregate, commands, events, projector, read-model migration,
focused tests, JWT-protected JSON CRUD API, and session-protected browser pages. It refuses to
overwrite an existing resource. Continue with [Build an Event-Sourced Resource](resources.md).

## Port 8080 is busy

Edit `.env`:

```dotenv
APP_PORT=8081
```

Run `make dev` again and use port 8081.

## Useful generated commands

```bash
make setup
make dev
make serve
make migrate
make check
make test
```

Clone the Arc repository only when contributing to the framework itself.
