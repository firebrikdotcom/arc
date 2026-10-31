#!/usr/bin/env bash
# Compile independent downstream consumers so feature unification cannot hide a broken facade.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
for variant in default no-default postgres all; do
  mkdir -p "$dir/$variant/src"
  case "$variant" in
    default) web="arc-web = { path = \"$root/crates/arc-web\" }" ;;
    no-default) web="arc-web = { path = \"$root/crates/arc-web\", default-features = false }" ;;
    postgres) web="arc-web = { path = \"$root/crates/arc-web\", default-features = false, features = [\"postgres\"] }" ;;
    all) web="arc-web = { path = \"$root/crates/arc-web\", features = [\"sqlite\", \"postgres\", \"nats\", \"test-utils\"] }" ;;
  esac
  cat > "$dir/$variant/Cargo.toml" <<MANIFEST
[package]
name = "stack-compat-$variant"
version = "0.1.0"
edition = "2021"
[dependencies]
$web
arc-stack = { path = "$root/crates/arc-stack", default-features = false }
arc-core = { path = "$root/crates/arc-core" }
MANIFEST
  cat > "$dir/$variant/src/main.rs" <<'RS'
use arc_core::{aggregate::Aggregate, command_bus::SnapshotPolicy, projection::Projector};
use arc_web::{ProjectorReg, helpers::{config::DatabaseDriver, es_stack}};
#[allow(dead_code)]
fn compatibility<A: Aggregate, P: Projector + 'static>(projector: Box<dyn Projector>, concrete: P, old: es_stack::SharedStack<A>) {
    let _: arc_stack::ProjectorReg = ProjectorReg::new(concrete, "view");
    let _: arc_stack::SharedStack<A> = old.clone();
    let reg: arc_stack::ProjectorReg = ProjectorReg { projector, view: "view".into() };
    let _: ProjectorReg = reg;
    let _: arc_stack::config::DatabaseDriver = DatabaseDriver::Sqlite;
    let _: fn() -> Option<Vec<u8>> = arc_web::helpers::config::event_integrity_key;
    let _: fn() -> String = arc_web::helpers::config::event_integrity_key_id;
}
#[allow(dead_code)]
async fn signatures<A: Aggregate>(url: &str, regs: Vec<ProjectorReg>, policy: Option<SnapshotPolicy>) -> Result<es_stack::EsStack<A>, Box<dyn std::error::Error>> {
    let _: es_stack::EsStores = es_stack::build_stores(DatabaseDriver::Sqlite, url).await?;
    let _: es_stack::EsStack<A> = es_stack::build(url, Vec::new(), None).await?;
    es_stack::build_with_driver(DatabaseDriver::Sqlite, url, regs, policy).await
}
fn main() { let _ = compatibility::<Dummy>; }
#[derive(Default)] struct Dummy;
impl arc_core::aggregate::Aggregate for Dummy {
    type Command = Cmd;
    type Event = ();
    type Error = std::io::Error;
    fn aggregate_type() -> &'static str { "dummy" }
    fn version(&self) -> i64 { 0 }
    fn handle(&self, _: Cmd) -> impl std::future::Future<Output = Result<Vec<arc_core::event::Event>, std::io::Error>> + Send { async { Ok(vec![]) } }
    fn apply(&mut self, _: &arc_core::event::Event) {}
}
struct Cmd;
impl arc_core::aggregate::Command for Cmd { fn aggregate_id(&self) -> &str { "id" } }
RS
  # The generic function body is checked without needing a concrete aggregate implementation.
  python3 - "$dir/$variant/src/main.rs" <<'PY'
import pathlib,sys
p=pathlib.Path(sys.argv[1]); s=p.read_text(); s=s[:s.index('fn main()')]+ 'fn main() {}\n'; p.write_text(s)
PY
  echo "Checking compatibility: $variant"
  CARGO_TARGET_DIR="$root/target" cargo check --manifest-path "$dir/$variant/Cargo.toml" --quiet
done
