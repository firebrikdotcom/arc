#!/usr/bin/env bash
#
# Arc upgradeability drift guard.
#
# ADR 0002 defines machine-owned/generated files as safe to regenerate and
# unsafe to hand-edit. This script enforces the currently mechanical part of
# that boundary and is intentionally small enough to run often.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail=0

check_generated_banner() {
  local file="$1"
  local generated_by="$2"
  local regenerate_hint="$3"

  if [ ! -f "$file" ]; then
    echo "MISSING GENERATED FILE: $file" >&2
    fail=1
    return
  fi

  if ! sed -n '1,3p' "$file" | grep -Fq "$generated_by"; then
    echo "MISSING GENERATED BANNER: $file must name $generated_by" >&2
    fail=1
  fi

  if ! sed -n '1,3p' "$file" | grep -Fq "$regenerate_hint"; then
    echo "MISSING REGENERATION HINT: $file must mention $regenerate_hint" >&2
    fail=1
  fi
}

forbid_path() {
  # Fail if a path that must not exist is present, per ADR 0002 ownership.
  local path="$1"
  local why="$2"
  if [ -e "$path" ]; then
    echo "OWNERSHIP VIOLATION: $path must not exist — $why" >&2
    fail=1
  fi
}

echo "arc-check: checking framework/app ownership boundary (ADR 0002)..."

# The framework crate must stay generic over the aggregate: no user-owned
# domain, services, or validation may live in arc-web.
forbid_path "crates/arc-ffi/src/domain" "user domain is app-owned"
forbid_path "crates/arc-ffi/src/services" "application services are app-owned"
forbid_path "crates/arc-ffi/src/validation" "application validation is app-owned"
forbid_path "crates/arc-ffi/src/helpers/es_stack.rs" "assembly belongs to arc-stack"
if rg -n 'CommandBus::new|ProjectionEngine::new|InProcessEventBus|SqliteEventStore|PostgresEventStore|fn build_stores' crates/arc-ffi/src --glob '*.rs'; then
  echo "OWNERSHIP VIOLATION: arc-ffi must delegate stack assembly to arc-stack." >&2
  fail=1
fi
forbid_path "crates/arc-stack/src/domain" "user domain is app-owned"
forbid_path "crates/arc-stack/src/services" "application services are app-owned"
forbid_path "crates/arc-stack/src/validation" "application validation is app-owned"
forbid_path "crates/arc-web/src/domain" "user domain is app-owned; arc-web is generic over the aggregate"
forbid_path "crates/arc-web/src/services" "application services are app-owned"
forbid_path "crates/arc-web/src/validation" "application validation is app-owned"

# The Arc-owned runtime must not reappear inside the thin app (no re-fork of
# framework code, and no duplication of the modules that moved to arc-web).
forbid_path "crates/arc-app/src/http/middlewares" "framework middleware lives in arc-web"
forbid_path "crates/arc-app/src/websocket" "framework websocket transport lives in arc-web"
forbid_path "crates/arc-app/src/helpers/es_stack.rs" "the event-sourced stack lives in arc-stack"
forbid_path "crates/arc-app/src/helpers/session.rs" "session helpers live in arc-web"
forbid_path "crates/arc-app/src/commands/serve.rs" "the serve bootstrap lives in arc-web"

echo "arc-check: checking publishable-crate file boundaries..."

if find crates/arc-auth-session -type f \( -name '*.html' -o -name '*.tera' \) | grep -q .; then
  echo "OWNERSHIP VIOLATION: arc-auth-session must not ship browser templates." >&2
  fail=1
fi
if rg -n 'tera' crates/arc-auth-session/Cargo.toml; then
  echo "OWNERSHIP VIOLATION: arc-auth-session must remain independent of Tera." >&2
  fail=1
fi

publishable_crates=(
  crates/arc-core
  crates/arc-stack
  crates/arc-ffi
  crates/arc-es-sqlite
  crates/arc-es-postgres
  crates/arc-es-nats
  crates/arc-web
)

if rg -n 'embed_migrations!\(\)' "${publishable_crates[@]}" --glob '*.rs'; then
  echo "PACKAGE VIOLATION: publishable crates must use an explicit crate-local migration path." >&2
  fail=1
fi

if rg -n 'embed_migrations!\("\.\.' "${publishable_crates[@]}" --glob '*.rs'; then
  echo "PACKAGE VIOLATION: publishable crates cannot embed migrations from outside their crate." >&2
  fail=1
fi

echo "arc-check: checking stack facade and headless dependencies..."
facade=$(sed '/^[[:space:]]*\/\//d' crates/arc-web/src/helpers/es_stack.rs | tr -d '[:space:]')
if [ "$facade" != 'pubusearc_stack::es_stack::{apply_snapshot_policy,build,build_stores,build_with_driver,EsStack,EsStores,SharedStack,};' ]; then
  echo "OWNERSHIP VIOLATION: arc-web es_stack must only re-export arc-stack items." >&2
  fail=1
fi
if ! dependency_tree=$(cargo tree -p arc-stack --no-default-features --features sqlite --edges normal,build --prefix none 2>&1); then
  echo "PACKAGE VIOLATION: cannot inspect headless dependency tree: $dependency_tree" >&2
  fail=1
elif printf '%s\n' "$dependency_tree" | grep -E '^(arc-web|actix[^ ]*|tera) v' >/dev/null; then
  echo "PACKAGE VIOLATION: arc-stack has a web dependency." >&2
  fail=1
fi

# Resolve outside the workspace so sibling web feature unification cannot mask
# a transitive dependency leak in any shipped native feature configuration.
ffi_consumer=$(mktemp -d)
trap 'rm -rf "$ffi_consumer"' EXIT
mkdir -p "$ffi_consumer/src"
printf 'fn main() {}\n' > "$ffi_consumer/src/main.rs"
for variant in default none sqlite postgres all; do
  case "$variant" in
    default) features='' ;;
    none) features=', default-features = false' ;;
    sqlite|postgres) features=", default-features = false, features = [\"$variant\"]" ;;
    all) features=', features = ["sqlite", "postgres", "uniffi"]' ;;
  esac
  cat > "$ffi_consumer/Cargo.toml" <<MANIFEST
[package]
name = "arc-ffi-boundary-probe"
version = "0.0.0"
edition = "2021"
[dependencies]
arc-ffi = { path = "$ROOT/crates/arc-ffi"$features }
MANIFEST
  if ! dependency_tree=$(cargo tree --manifest-path "$ffi_consumer/Cargo.toml" --edges normal,build --prefix none 2>&1); then
    echo "PACKAGE VIOLATION: cannot inspect arc-ffi $variant dependency tree: $dependency_tree" >&2
    fail=1
  elif printf '%s\n' "$dependency_tree" | grep -E '^(arc-web|arc-auth[^ ]*|actix[^ ]*|tera) v' >/dev/null; then
    echo "PACKAGE VIOLATION: arc-ffi $variant has a web/auth dependency." >&2
    fail=1
  fi
done
rm -rf "$ffi_consumer"
trap - EXIT

echo "arc-check: checking generated-file ownership boundary..."

check_generated_banner \
  "config/benthos/generated/events.yaml" \
  "Generated by scripts/generate-benthos-config.mjs" \
  "make benthos-config"

echo "arc-check: checking canonical Docsify documentation..."

for file in \
  docs/index.html \
  docs/_sidebar.md \
  docs/auth-plugins.md; do
  if [ ! -f "$file" ]; then
    echo "MISSING CANONICAL DOC: $file" >&2
    fail=1
  fi
done

if ! rg -q '\]\(auth-plugins\.md\)' docs/_sidebar.md; then
  echo "DOC NAVIGATION VIOLATION: Docsify sidebar must link auth-plugins.md" >&2
  fail=1
fi

for package in arc-auth-core arc-auth-db arc-auth-session arc-auth-admin arc-auth-jwt arc-auth-rbac; do
  if ! rg -q "${package}" docs/auth-plugins.md; then
    echo "AUTH DOC VIOLATION: docs/auth-plugins.md must document ${package}" >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "arc-check: FAILED - ownership, package, generated-file, or documentation checks failed." >&2
  exit 1
fi

npm run benthos:config:check

echo "arc-check: OK - generated files are fresh and carry ownership banners."
