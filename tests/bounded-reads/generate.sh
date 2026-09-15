#!/usr/bin/env bash
set -euo pipefail
cd /work
# The application and CLI both name their binary arc; isolate the CLI output.
CARGO_TARGET_DIR=/cache/cli-target cargo build --locked -p arc-web-cli
fixture_parent="$(mktemp -d /cache/bounded-XXXXXX)"
cd "$fixture_parent"
ARC_CLI_TEST_LOCAL_ROOT=/work /cache/cli-target/debug/arc new bounded-fixture --ui --no-git
cd bounded-fixture
ARC_CLI_TEST_LOCAL_ROOT=/work /cache/cli-target/debug/arc plugin add auth-db-session
ARC_CLI_TEST_LOCAL_ROOT=/work /cache/cli-target/debug/arc generate resource Product --api --ui
# New consumers have no lockfile; resolve it once, then verify the locked graph.
cargo generate-lockfile
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
ARC_SETUP_ADMIN_NAME=Administrator ARC_SETUP_ADMIN_EMAIL=admin@example.com ARC_SETUP_ADMIN_PASSWORD=change-me-now cargo run --locked -- setup
sed -i 's/^APP_PORT=.*/APP_PORT=18771/; s/^APP_URL=.*/APP_URL=0.0.0.0/' .env
# The fixture sends hundreds of requests to exercise paging, not throttling.
echo "GLOBAL_RATE_LIMIT_MAX_REQUESTS=10000" >> .env
cargo build --locked
pwd > /cache/bounded-fixture-path
