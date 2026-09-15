#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
project="arc-nineties-security-${$}"
compose=(docker compose -p "$project" -f tests/security/compose.yml)
cleanup() { "${compose[@]}" down --remove-orphans; }
trap cleanup EXIT
"${compose[@]}" up -d
"${compose[@]}" exec -T verify sh -c 'timeout 180 sh -c "until command -v cargo-clippy >/dev/null 2>&1 && pkg-config --exists openssl sqlite3; do sleep 1; done"'
"${compose[@]}" exec -T verify cargo test --locked -p arc-core -p arc-web -p arc-auth-rbac -p arc-auth-session -p arc-auth-db -p arc-auth-admin
"${compose[@]}" exec -T verify cargo test --locked -p arc --test projection_security_http
"${compose[@]}" exec -T verify cargo test --locked -p arc --bin arc profile_http_is_bound_to_actor_and_filters_secrets
"${compose[@]}" exec -T verify cargo clippy --locked -p arc-core -p arc-web -p arc-auth-rbac -p arc-auth-session -p arc-auth-db -p arc-auth-admin -p arc --all-targets --all-features -- -D warnings
"${compose[@]}" exec -T verify cargo build --locked -p arc-auth-rbac --example security-fixture
"${compose[@]}" exec -d -e ARC_SECURITY_FIXTURE=1 verify target/security-coverage/debug/examples/security-fixture
export ARC_SECURITY_URL="http://127.0.0.1:${ARC_SECURITY_PORT:-18764}"
for attempt in {1..60}; do
  if curl -fsS "$ARC_SECURITY_URL/health" >/dev/null; then break; fi
  sleep 1
done
curl -fsS "$ARC_SECURITY_URL/health" >/dev/null
npx playwright test --config tests/security/browser.config.mjs
