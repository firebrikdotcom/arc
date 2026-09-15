#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
project="arc-nineties-bounded-${$}"
compose_file="${ARC_BOUNDED_COMPOSE_FILE:-tests/bounded-reads/compose.yml}"
compose=(docker compose -p "$project" -f "$compose_file")
cleanup() { "${compose[@]}" down --volumes --remove-orphans; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
"${compose[@]}" up --build -d --wait
"${compose[@]}" exec -T verify cargo test --locked -p arc-core -p arc-es-sqlite -p arc-es-postgres -p arc-auth-db
"${compose[@]}" exec -T verify bash tests/bounded-reads/generate.sh
"${compose[@]}" exec -d verify sh -c 'cd "$(cat /cache/bounded-fixture-path)" && exec /cache/target/debug/bounded-fixture serve'
export ARC_BOUNDED_URL="http://127.0.0.1:${ARC_BOUNDED_PORT:-18771}"
for attempt in {1..60}; do
  if curl -fsS "$ARC_BOUNDED_URL/health" >/dev/null; then break; fi
  sleep 1
done
timeout 300 node tests/bounded-reads/browser.mjs
