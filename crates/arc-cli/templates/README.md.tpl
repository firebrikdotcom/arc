# {{project-name}}

Generated with Arc.

```sh
make setup
make dev
```

Open <http://127.0.0.1:8080/health>. UI applications serve the Focused home at `/home` and the
session-protected Dense Workbench at `/admin`. Change `APP_PORT` in `.env` if port 8080 is busy.

On a fresh database, `make setup` prompts for the first administrator's name, email, and password.
Noninteractive environments must provide `ARC_SETUP_ADMIN_NAME`, `ARC_SETUP_ADMIN_EMAIL`, and
`ARC_SETUP_ADMIN_PASSWORD` for that first run. These are one-shot setup inputs: they are never
written to `.env`, and rerunning setup never resets credentials. `POST /api/session` exchanges the
same projection-backed credentials for a revocable JWT; APIs generated with `--api-auth jwt`
require it.

The administrator Profile page updates name/email through User commands. Password changes verify
the current password and revoke existing API sessions.

## Generate a resource

Create and register an event-sourced aggregate, commands, events, projector, read-model migration,
and focused tests:

```sh
arc generate resource Product --api --ui --api-auth jwt
make migrate
```

`--api` adds create/list/get/update/delete JSON endpoints; `--api-auth jwt` protects them. In a
project created with `arc new --ui`, `--ui` adds session-protected collection, detail, create, and
edit pages when the session plugin is installed. Browser writes retain CSRF protection and dispatch
commands; reads use the resource projection. Generation refuses to overwrite an existing resource.
`aggregate` is an alias for `resource`.

See the [Arc authentication plugin reference](https://github.com/lotharthesavior/arc/blob/master/docs/auth-plugins.md)
for capability ownership, configuration, routes, and authorization boundaries.
