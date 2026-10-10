#!/usr/bin/env bash
# Ensures the canonical Docsify guide and roadmap retain current release and routing facts.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$VERSION" ]; then
  echo "Unable to read workspace version from Cargo.toml." >&2
  exit 1
fi

fail=0

require() {
  local pattern="$1" path="$2" description="$3"
  if ! grep -Fqr -- "$pattern" "$path"; then
    echo "STALE DOCUMENTATION: $description ($path must contain: $pattern)" >&2
    fail=1
  fi
}

forbid() {
  local pattern="$1" path="$2" description="$3"
  if grep -Fqr -- "$pattern" "$path"; then
    echo "STALE DOCUMENTATION: $description ($path must not contain: $pattern)" >&2
    fail=1
  fi
}

require "**Current development version:** $VERSION" progress.md "roadmap development version"
PUBLISHED_VERSION="$(sed -n 's/^\*\*Latest published version:\*\* \([0-9][0-9.]*\)$/\1/p' progress.md)"
if [[ ! "$PUBLISHED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid latest published version in progress.md" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$VERSION" "$PUBLISHED_VERSION" | sort -V | tail -1)" != "$VERSION" ]]; then
  echo "Published version cannot exceed the workspace version" >&2
  exit 1
fi
require "The latest published package version is **$PUBLISHED_VERSION**" docs/packages.md "package publication status"
require "Source version **$VERSION**" docs/packages.md "package source version"
require "arc-core = \"$PUBLISHED_VERSION\"" docs/packages.md "published dependency example"
require "arc-web = \"$PUBLISHED_VERSION\"" docs/packages.md "published dependency example"
require "The routing generator, config" docs/packages.md "implemented routing overview"
require "routing integration suite publishes through" docs/event-handlers.md "routing integration coverage"
require "[x] NATS → Benthos → Arc-owned projection integration suite." progress.md "completed routing integration roadmap item"
require "[x] Generated-pipeline freshness and Redpanda Connect lint CI gates." progress.md "completed routing CI roadmap item"

# The completed routing claims must continue to point at real implementation
# artifacts, rather than merely agree with each other.
require "async fn benthos_routes_nats_event_to_arc_projection_handler" crates/arc-app/tests/benthos_projection_routing.rs "Benthos projection integration test"
require "input:" config/benthos/generated/events.yaml "generated Benthos pipeline"
require "make benthos-config-check" .github/workflows/ci.yml "Benthos freshness CI gate"

forbid "NATS publishing and Benthos (Redpanda Connect) event routing are still being completed" docs "obsolete routing work-in-progress claim"
forbid "The remaining routing integration test should publish" docs "obsolete routing test claim"
forbid "Benthos (Redpanda Connect) routing remain work in progress" progress.md "obsolete roadmap routing claim"

if [ "$fail" -ne 0 ]; then
  exit 1
fi

echo "check-documentation-freshness: OK — v$VERSION documentation and routing status match source."
