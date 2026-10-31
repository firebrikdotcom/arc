#!/usr/bin/env bash
#
# check-roadmap-claims.sh
#
# Asserts that every progress.md item marked complete has its
# concrete code artifact present in the current tree. Exits non-zero if any
# asserted artifact is missing. Run from anywhere; resolves the repo root
# relative to this script.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail=0

# need_file PATH DESC
need_file() {
  if [ ! -f "$1" ]; then
    echo "MISSING FILE: $1  ($2)" >&2
    fail=1
  fi
}

# need_dir PATH DESC
need_dir() {
  if [ ! -d "$1" ]; then
    echo "MISSING DIR: $1  ($2)" >&2
    fail=1
  fi
}

# need_grep PATTERN FILE DESC
need_grep() {
  if ! grep -qE "$1" "$2" 2>/dev/null; then
    echo "MISSING SYMBOL: /$1/ not found in $2  ($3)" >&2
    fail=1
  fi
}

# ---------------------------------------------------------------------------
# Snapshot — interface + SQLite persistence DONE (CommandBus wiring still pending)
# progress.md completed foundations, current priorities, and strategic architecture
# ---------------------------------------------------------------------------
need_file "crates/arc-core/src/snapshot.rs" "Snapshot struct"
need_grep "struct Snapshot" "crates/arc-core/src/snapshot.rs" "Snapshot struct"
need_grep "fn save_snapshot" "crates/arc-core/src/event_store.rs" "EventStore::save_snapshot"
need_grep "fn load_snapshot" "crates/arc-core/src/event_store.rs" "EventStore::load_snapshot"
need_grep "fn to_snapshot"   "crates/arc-core/src/aggregate.rs"   "Aggregate::to_snapshot"
need_grep "fn from_snapshot" "crates/arc-core/src/aggregate.rs"   "Aggregate::from_snapshot"
need_grep "save_snapshot"    "crates/arc-es-sqlite/src/lib.rs"    "arc-es-sqlite snapshot impl"
need_dir  "migrations/2026-05-31-000001_create_snapshots" "snapshots migration"

# ---------------------------------------------------------------------------
# Phase 1.3 — MVC -> ES migration COMPLETE
# ---------------------------------------------------------------------------
need_file "crates/arc-app/src/domain/user/projector.rs" "UserProjector"
need_grep "UserProjector"           "crates/arc-app/src/domain/user/projector.rs" "UserProjector struct"
need_grep "USERS_VIEW|users_view"   "crates/arc-app/src/domain/user/projector.rs" "users_view read model"
need_grep "UserProjector"           "crates/arc-app/src/main.rs"                  "UserProjector registered by app"
need_grep "read_model_store"        "crates/arc-web/src/commands/serve.rs"        "projection store wired by framework"
need_dir  "migrations/2026-05-08-000001_drop_legacy_users" "legacy users dropped"

# ---------------------------------------------------------------------------
# Phase 7.1 CI + Phase 4.3 clippy — DONE
# ---------------------------------------------------------------------------
need_file ".github/workflows/ci.yml" "CI workflow"
need_grep "cargo clippy"        ".github/workflows/ci.yml" "clippy in CI"
need_grep "cargo fmt"           ".github/workflows/ci.yml" "fmt check in CI"
need_grep "cargo test"          ".github/workflows/ci.yml" "tests in CI"
need_grep "npm run build"       ".github/workflows/ci.yml" "frontend build in CI"
need_grep "nats-server-.+linux-amd64" ".github/workflows/ci.yml" "NATS server release binary install"
need_file ".github/workflows/security.yml" "Security workflow"
need_grep "cargo audit"         ".github/workflows/security.yml" "cargo audit"

# ---------------------------------------------------------------------------
# HIPAA-1..5 — progress.md completed-foundation claims (assert artifacts)
# ---------------------------------------------------------------------------
need_grep "struct AuditMetadata"     "crates/arc-core/src/audit.rs"       "HIPAA-1 AuditMetadata"
need_dir  "migrations/2026-04-21-000002_add_hipaa_audit" "HIPAA-1 audit migration"
need_grep "trait AccessLogger"       "crates/arc-core/src/access_log.rs"  "HIPAA-2 AccessLogger"
need_grep "FailHard|FailOpenWarn"    "crates/arc-core/src/access_log.rs"  "HIPAA-2a FailurePolicy"
need_grep "for_sensitivity"          "crates/arc-core/src/access_log.rs"  "HIPAA-2a for_sensitivity"
need_file "crates/arc-web/src/http/middlewares/idle_timeout_middleware.rs" "HIPAA-3 idle timeout"
need_grep "SESSION_IDLE_TIMEOUT_SECS" "crates/arc-web/src/http/middlewares/idle_timeout_middleware.rs" "HIPAA-3 env knob"
need_grep "get_session_user"     "crates/arc-web/src/http/middlewares/idle_timeout_middleware.rs" "HIPAA-3 post-cutover session user"
need_grep "trait SessionStore"       "crates/arc-core/src/session.rs"     "HIPAA-4 SessionStore"
need_grep "SqliteSessionStore"       "crates/arc-es-sqlite/src/session.rs" "HIPAA-4 SQLite session store"
need_grep "jti"                      "crates/arc-web/src/helpers/jwt.rs"  "HIPAA-4 jti claim"
need_dir  "migrations/2026-04-26-000002_create_jwt_sessions" "HIPAA-4 sessions migration"
need_grep "trait IntegrityChain"     "crates/arc-core/src/integrity.rs"   "HIPAA-5 IntegrityChain"
need_grep "HmacSha256Chain"          "crates/arc-core/src/integrity.rs"   "HIPAA-5 HMAC impl"
need_grep "fn verify_chain"          "crates/arc-core/src/integrity.rs"   "HIPAA-5 verify_chain"

# ---------------------------------------------------------------------------
# Production Risks (closed) + Infra
# ---------------------------------------------------------------------------
need_dir  "migrations/2026-04-26-000001_widen_event_int_columns" "i64 widening migration"
need_grep "test_sequence_above_i32_max_roundtrips_without_truncation" "crates/arc-es-sqlite/src/lib.rs" "i64 regression test"
need_grep "fn upsert"  "crates/arc-core/src/read_model_store.rs" "typed ReadModelStore::upsert"
need_grep "fn find_by" "crates/arc-core/src/read_model_store.rs" "typed ReadModelStore::find_by"
need_file "Dockerfile"          "Dockerfile present"
need_file "docker-compose.yml"  "compose present"

# ---------------------------------------------------------------------------
# Drift checks added after arc-status-audit
# ---------------------------------------------------------------------------
if [ -f ".github/workflows/tests.yml" ]; then
  echo "STALE WORKFLOW: .github/workflows/tests.yml should not duplicate ci.yml" >&2
  fail=1
fi

if grep -R "apt-get install .*nats-server" .github/workflows 2>/dev/null; then
  echo "BROKEN CI ASSUMPTION: use upstream NATS release binary, not apt nats-server" >&2
  fail=1
fi

need_grep "cargo test --workspace --all-features" "Makefile" "make test matches CI feature/workspace coverage"
need_grep "cargo clippy --workspace --all-targets --all-features -- -D warnings" "Makefile" "make lint matches CI"

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "check-roadmap-claims: FAILED — one or more claimed artifacts are missing." >&2
  exit 1
fi

echo "check-roadmap-claims: OK — all claimed artifacts present."
