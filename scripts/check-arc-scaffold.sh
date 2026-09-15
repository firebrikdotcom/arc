#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
consumer_root="$(mktemp -d)"
consumer_target="${CARGO_TARGET_DIR:-$repo_root/target/scaffold-check}"
server_pid=""

cleanup() {
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    case "$consumer_root" in
        /tmp/*) rm -rf "$consumer_root" ;;
        *) echo "Refusing to clean unexpected scaffold path: $consumer_root" >&2 ;;
    esac
}
trap cleanup EXIT

export CARGO_TARGET_DIR="$consumer_target"

generate_and_check() {
    local name="$1"
    shift
    local resource_args=(--api)
    if [[ " $* " == *" --ui "* ]]; then
        resource_args+=(--ui)
    fi

    (
        cd "$consumer_root"
        ARC_CLI_TEST_LOCAL_ROOT="$repo_root" \
            cargo run --quiet --manifest-path "$repo_root/crates/arc-cli/Cargo.toml" -- \
            new "$name" --no-git "$@"
    )

    (
        cd "$consumer_root/$name"
        RUSTFLAGS="-D warnings" cargo check --quiet
        if [[ " $* " == *" --ui "* ]]; then
            ARC_CLI_TEST_LOCAL_ROOT="$repo_root" cargo run --quiet --manifest-path "$repo_root/crates/arc-cli/Cargo.toml" -- plugin add auth-db-session
            ARC_CLI_TEST_LOCAL_ROOT="$repo_root" cargo run --quiet --manifest-path "$repo_root/crates/arc-cli/Cargo.toml" -- plugin add auth-jwt
            ARC_CLI_TEST_LOCAL_ROOT="$repo_root" cargo run --quiet --manifest-path "$repo_root/crates/arc-cli/Cargo.toml" -- generate resource Product "${resource_args[@]}" --api-auth jwt --roles admin
            export ARC_SETUP_ADMIN_NAME="Scaffold Administrator"
            export ARC_SETUP_ADMIN_EMAIL="admin@example.com"
            export ARC_SETUP_ADMIN_PASSWORD="change-me-now"
        else
            cargo run --quiet --manifest-path "$repo_root/crates/arc-cli/Cargo.toml" -- generate resource Product "${resource_args[@]}"
        fi
        cargo run --quiet -- setup
        before="$(sed -n 's/^SECRET_KEY=//p' .env)"
        cargo run --quiet -- setup
        after="$(sed -n 's/^SECRET_KEY=//p' .env)"
        test -n "$before"
        test "$before" = "$after"
        cargo check --quiet
        cargo test --quiet
        cargo fmt --all -- --check
        cargo clippy --quiet --all-targets -- -D warnings
    )
}

generate_and_check arc-scaffold-minimal
generate_and_check arc-scaffold-ui --ui

test ! -e "$consumer_root/arc-scaffold-minimal/src/ui.rs"
test -e "$consumer_root/arc-scaffold-ui/src/ui.rs"
test -e "$consumer_root/arc-scaffold-ui/resources/views/home.html"
test -e "$consumer_root/arc-scaffold-minimal/src/domain/product/aggregate.rs"
test -e "$consumer_root/arc-scaffold-ui/src/domain/product/projector.rs"
test -e "$consumer_root/arc-scaffold-ui/src/domain/product/api.rs"
grep -q 'register_aggregate::<ProductAggregate>()' \
    "$consumer_root/arc-scaffold-minimal/src/main.rs"
grep -q 'register_projector(ProductProjector, PRODUCTS_VIEW)' \
    "$consumer_root/arc-scaffold-ui/src/main.rs"
grep -q 'crate::domain::product::api::config(_cfg);' \
    "$consumer_root/arc-scaffold-ui/src/routes.rs"

pushd "$consumer_root/arc-scaffold-minimal" >/dev/null
sed -i 's/^APP_PORT=.*/APP_PORT=39081/' .env
cargo run --quiet -- serve >server.log 2>&1 &
server_pid=$!
for _ in {1..40}; do
    if curl --silent --fail http://127.0.0.1:39081/health >/dev/null; then
        break
    fi
    sleep 0.25
done

test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    http://127.0.0.1:39081/api/products)" = "200"
curl --silent --fail \
    --request POST \
    --header 'content-type: application/json' \
    --data '{"id":"product-1","name":"Notebook"}' \
    http://127.0.0.1:39081/api/products | grep -q 'Notebook'
curl --silent --fail http://127.0.0.1:39081/api/products/product-1 | grep -q 'Notebook'
curl --silent --fail http://127.0.0.1:39081/api/products | grep -q 'product-1'
curl --silent --fail \
    --request PUT \
    --header 'content-type: application/json' \
    --data '{"name":"Field Notebook"}' \
    http://127.0.0.1:39081/api/products/product-1 | grep -q 'Field Notebook'
curl --silent --fail http://127.0.0.1:39081/api/products/product-1 | grep -q 'Field Notebook'
curl --silent --fail \
    --request DELETE \
    http://127.0.0.1:39081/api/products/product-1 >/dev/null
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    http://127.0.0.1:39081/api/products/product-1)" = "404"
test "$(sqlite3 database/database.sqlite \
    "SELECT COUNT(*) FROM events WHERE aggregate_type = 'Product' AND aggregate_id = 'product-1';")" = "3"
kill "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=""
popd >/dev/null

pushd "$consumer_root/arc-scaffold-ui" >/dev/null
sed -i 's/^APP_PORT=.*/APP_PORT=39082/' .env
cargo run --quiet -- serve >server.log 2>&1 &
server_pid=$!
for _ in {1..40}; do
    if curl --silent --fail http://127.0.0.1:39082/health >/dev/null; then
        break
    fi
    sleep 0.25
done

cookie_jar="$consumer_root/ui-cookies.txt"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    http://127.0.0.1:39082/admin)" = "302"
signin_html="$(curl --silent --fail --cookie-jar "$cookie_jar" http://127.0.0.1:39082/signin)"
grep -q '/public/styles.css' <<<"$signin_html"
grep -q 'class="field"' <<<"$signin_html"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    http://127.0.0.1:39082/public/styles.css)" = "200"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --cookie "$cookie_jar" --request POST --data-urlencode 'csrf_token=invalid' \
    --data-urlencode 'email=admin@example.com' --data-urlencode 'password=wrong' \
    http://127.0.0.1:39082/signin)" = "403"
invalid_cookie_jar="$consumer_root/ui-invalid-cookies.txt"
invalid_html="$(curl --silent --cookie-jar "$invalid_cookie_jar" http://127.0.0.1:39082/signin)"
invalid_csrf="$(sed -n 's/.*name="csrf_token" value="\([^"]*\)".*/\1/p' <<<"$invalid_html")"
invalid_response="$(curl --silent --cookie "$invalid_cookie_jar" --cookie-jar "$invalid_cookie_jar" \
    --request POST --data-urlencode "csrf_token=$invalid_csrf" \
    --data-urlencode 'email=<script>alert(1)</script>@example.com' --data-urlencode 'password=wrong' \
    http://127.0.0.1:39082/signin)"
grep -q '&lt;script&gt;' <<<"$invalid_response"
if grep -q '<script>alert(1)</script>' <<<"$invalid_response"; then exit 1; fi
node "$repo_root/scripts/check-security-headers.mjs" http://127.0.0.1:39082
node "$repo_root/scripts/check-generated-auth-ui.mjs" http://127.0.0.1:39082
csrf_token="$(sed -n 's/.*name="csrf_token" value="\([^"]*\)".*/\1/p' <<<"$signin_html")"
test -n "$csrf_token"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --cookie "$cookie_jar" --cookie-jar "$cookie_jar" \
    --request POST --data-urlencode "csrf_token=$csrf_token" \
    --data-urlencode 'email=admin@example.com' --data-urlencode 'password=change-me-now' \
    http://127.0.0.1:39082/signin)" = "303"
# Persist the refreshed session cookie, as a browser does. Signing in now clears
# and renews the session to defeat session fixation, so the pre-login CSRF token
# no longer survives authentication: this GET is what mints the token for the
# POST below, and it arrives in an updated session cookie. Reading without
# --cookie-jar would discard that cookie and submit a token the stored session
# no longer knows.
new_html="$(curl --silent --fail --cookie "$cookie_jar" --cookie-jar "$cookie_jar" \
    http://127.0.0.1:39082/admin/products/new)"
csrf_token="$(sed -n 's/.*name="csrf_token" value="\([^"]*\)".*/\1/p' <<<"$new_html")"
test -n "$csrf_token"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --cookie "$cookie_jar" --cookie-jar "$cookie_jar" \
    --request POST --data-urlencode "csrf_token=$csrf_token" \
    --data-urlencode 'id=browser-product' --data-urlencode 'name=Browser Notebook' \
    http://127.0.0.1:39082/admin/products/new)" = "303"
curl --silent --fail --cookie "$cookie_jar" \
    http://127.0.0.1:39082/admin/products/browser-product | grep -q 'Browser Notebook'
test "$(curl --silent --output /dev/null --write-out '%{http_code}' --cookie "$cookie_jar" \
    http://127.0.0.1:39082/admin/settings)" = "404"
kill "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=""
popd >/dev/null

echo "Arc scaffold verification passed."
