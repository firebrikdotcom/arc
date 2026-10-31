#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
project="${ARC_AUDIT_TEST_PROJECT:-arc-test-durable-browser}"
compose=(docker compose -p "$project" -f tests/durable-audit/compose.yml)
receipt_dir=""
cleanup() { "${compose[@]}" down --remove-orphans; if [ -n "$receipt_dir" ]; then rm -rf "$receipt_dir"; fi; }
trap cleanup EXIT
trap "exit 130" INT
trap "exit 143" TERM
if [ -z "${ARC_AUDIT_RECEIPT:-}" ]; then
  receipt_dir="$(mktemp -d)"
  export ARC_AUDIT_RECEIPT="$receipt_dir/receipt.json"
fi
"${compose[@]}" build verify
"${compose[@]}" run --rm verify cargo build --locked -p arc-web --example durable-audit-fixture
start() {
  "${compose[@]}" run -d --service-ports --name "${project}-fixture" verify /cache/target/debug/examples/durable-audit-fixture
  for attempt in {1..60}; do
    if curl -fsS http://127.0.0.1:18784/health | grep -q '^durable-audit-fixture:'; then return; fi
    sleep 1
  done
  "${compose[@]}" logs
  return 1
}
start
node_modules/.bin/playwright test -c tests/durable-audit/browser.config.mjs --grep 'before restart'
"${compose[@]}" down --remove-orphans
start
node_modules/.bin/playwright test -c tests/durable-audit/browser.config.mjs --grep 'after restart'
