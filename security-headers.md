# Security headers and Content Security Policy

The `arc-web` server applies response defaults to every application using
`ArcAppBuilder::serve`, including CLI-generated applications and authentication
plugins. The outer middleware covers HTML, API responses, static assets, redirects,
not-found responses, and responses produced by session/auth/rate-limit middleware.
It preserves response status, cookies, CSRF handling, content types and cache headers.
Existing response headers take precedence, allowing deliberate route-specific policies.
HTML handlers must explicitly return `Content-Type: text/html; charset=utf-8`: do
not rely on browser sniffing. Both generated UI rendering and the demo HTML handlers
set this MIME type. JavaScript and CSS must likewise be served with their correct types.

| Header | Default |
|---|---|
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `Referrer-Policy` | `strict-origin-when-cross-origin` |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` |
| `Content-Security-Policy` | `base-uri 'self'; object-src 'none'; frame-ancestors 'none'; form-action 'self'` |

## Compatible baseline, not an XSS script policy

The enforced baseline prevents foreign base URLs, plugin objects, embedding the app
in frames, and form submissions to other origins. It does **not** restrict script,
style, image, font or connection sources and must not be described as a strict CSP
or comprehensive XSS protection. Continue escaping template output and enforcing
CSRF and authorization on the server.

This is intentional: the bundled demo has inline theme/highlighting scripts and CDN
highlight.js assets; Turbo creates dynamic styles, Toastify changes styles, and Vite
serves built hashed assets from `/public/`. Generated UI templates currently use
local `/public/styles.css` without inline JavaScript. The baseline accommodates both,
as well as same-origin Turbo requests and WebSocket connections. It neither adds
`unsafe-inline` globally nor claims to block inline script execution.

## Application policy configuration

Set these environment variables before starting the server; restart after changes.
Unset optional values rather than assigning empty strings. Empty values and invalid
HTTP header bytes fail startup. Values are validated as HTTP headers, **not** parsed
for CSP syntax or security strength; review and browser-test application policies.

| Variable | Effect |
|---|---|
| `SECURITY_CSP` | Replaces the entire enforced baseline. Retain its containment directives unless deliberately changing them. |
| `SECURITY_CSP_REPORT_ONLY` | Adds an independent, non-enforcing policy for testing stricter rules. Absent by default. |
| `SECURITY_HSTS` | Adds `Strict-Transport-Security` with the supplied value. Absent by default in every environment. |

For a generated app with only same-origin assets, begin testing with:

```dotenv
SECURITY_CSP_REPORT_ONLY="default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"
```

Inspect browser CSP violations while exercising login, logout, profile saves,
validation failures, resource CRUD, Turbo navigation and WebSockets. Report-only
violations are expected for the demo's inline/CDN scripts and Turbo styles. Move inline
scripts into bundled files or implement per-response nonces/hashes before enforcement;
Arc does not currently supply nonce generation or injection. Allow only the exact
required asset origins. With a Vite HMR server, explicitly allow its HTTP origin for
scripts/styles and its WebSocket origin for connections in the development policy;
do not carry those allowances into production. Some browsers require explicit
`ws://`/`wss://` origins even with `connect-src 'self'`.

Once verified, copy the complete policy to `SECURITY_CSP`. An enforced policy and a
report-only policy operate independently. No reporting endpoint is installed by Arc;
configure an application-owned receiver and reporting directives if remote collection
is needed, with appropriate privacy and rate limits. See the
[MDN CSP guide](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/CSP).

Applications intentionally embedded elsewhere must set both CSP `frame-ancestors`
and a compatible route-level `X-Frame-Options` value (or arrange header removal at
the proxy). Cross-origin form/SSO workflows require an explicit `form-action` policy.
Multiple CSP headers added by a proxy are enforced together, not as replacements.

## HTTPS and deployment ownership

HSTS is opt-in: Arc cannot infer a trusted public TLS boundary from `APP_ENV` or
untrusted forwarding headers. Configure it at the TLS proxy, or set
`SECURITY_HSTS="max-age=300"` while validating an HTTPS-only deployment, then raise
the duration after confirming all routes remain available through HTTPS. Do not add
`includeSubDomains` or `preload` until every affected hostname is ready. Local HTTP
continues to work without automatic upgrades. Production session cookies still use
Secure independently of this setting.

COOP/COEP/CORP and legacy `X-XSS-Protection` are not enabled: cross-origin isolation
and browser popup workflows need application-specific decisions. Permissions for
camera, microphone or geolocation can be enabled by an explicit response override.

## Regression checks

Run backend checks inside the project's isolated Docker Compose test environment:

```bash
cargo test -p arc-web security_headers --lib
cargo clippy -p arc-web --all-targets --all-features -- -D warnings
bash scripts/check-arc-scaffold.sh
```

The scaffold check creates fresh minimal and UI consumers against the local framework,
checks generated Rust, exercises auth/CSRF/CRUD, and runs Chromium header/CSP probes.
`scripts/check-security-headers.mjs <generated-app-url>` can also run against that UI
fixture. It checks HTML/API/assets/errors/redirects and blocked base URLs, forms and
framing. Use `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to select a container's Chromium;
otherwise Playwright uses its installed browser. Each parallel lane needs its own
Compose project and container network because scaffold servers use fixed internal
ports. `CARGO_TARGET_DIR` is respected for isolated build caches.

After `npm ci` and `npm run build`, run
`node scripts/check-security-assets.mjs <demo-url>` against the bundled demo running
inside Compose. It checks inline theme initialization, Vite asset loading, cache/304
behavior, Turbo navigation and visible Toastify output under the baseline policy.
These checks do not certify an application's custom CSP, HMR allowlist, SSO or TLS
proxy configuration.
