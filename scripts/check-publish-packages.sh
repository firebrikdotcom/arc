#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if (($#)); then
    crates=("$@")
else
    crates=(arc-core arc-auth-core arc-es-sqlite arc-es-postgres arc-es-nats arc-stack arc-web arc-auth-db arc-auth-session arc-auth-jwt arc-auth-admin arc-auth-rbac arc-web-cli)
fi

# Package the release together so Cargo can resolve unpublished workspace versions.
package_args=()
for crate in "${crates[@]}"; do
    package_args+=(-p "$crate")
done
cargo package --allow-dirty --no-verify "${package_args[@]}"

package_overrides=()
for crate in "${crates[@]}"; do

    package_dir="$repo_root/target/package/${crate}-${version}"
    package_archive="$repo_root/target/package/${crate}-${version}.crate"
    if [[ ! -f "$package_archive" ]]; then
        echo "Package archive missing: $package_archive" >&2
        exit 1
    fi

    case "$package_dir" in
        "$repo_root"/target/package/*) rm -rf "$package_dir" ;;
        *)
            echo "Refusing to clean unexpected package path: $package_dir" >&2
            exit 1
            ;;
    esac
    tar -xzf "$package_archive" -C "$repo_root/target/package"

    if [[ ! -d "$package_dir" ]]; then
        echo "Packaged source directory missing: $package_dir" >&2
        exit 1
    fi
    # Verify only extracted archives, never the original workspace sources.
    package_overrides+=(--config "patch.crates-io.${crate}.path=\"${package_dir}\"")
done

export CARGO_TARGET_DIR="$repo_root/target/publish-check"

for crate in "${crates[@]}"; do
    package_dir="$repo_root/target/package/${crate}-${version}"
    echo "Compiling packaged ${crate} tests..."
    cargo test --manifest-path "$package_dir/Cargo.toml" "${package_overrides[@]}" --all-features --no-run
done

if [[ " ${crates[*]} " =~ " arc-stack " ]]; then
    consumer_dir="$(mktemp -d)"
    trap 'rm -rf "$consumer_dir"' EXIT
    mkdir -p "$consumer_dir/src"
    cat >"$consumer_dir/Cargo.toml" <<EOF
[package]
name = "arc-stack-package-consumer"
version = "0.1.0"
edition = "2021"
[dependencies]
arc-stack = { path = "$repo_root/target/package/arc-stack-${version}" }
EOF
    echo 'fn main() { let _ = arc_stack::config::DatabaseDriver::Sqlite; }' >"$consumer_dir/src/main.rs"
    cargo check --manifest-path "$consumer_dir/Cargo.toml" "${package_overrides[@]}"
    tree=$(cargo tree --manifest-path "$consumer_dir/Cargo.toml" "${package_overrides[@]}" --edges normal,build --prefix none)
    if printf '%s\n' "$tree" | grep -E '^(arc-web|actix[^ ]*|tera) v'; then
        echo "Packaged headless stack contains web dependencies" >&2
        exit 1
    fi
    rm -rf "$consumer_dir"
    trap - EXIT
fi

if [[ ! " ${crates[*]} " =~ " arc-web " ]]; then
    echo "Selected publish-package verification passed."
    exit 0
fi

consumer_dir="$(mktemp -d)"
trap 'rm -rf "$consumer_dir"' EXIT

mkdir -p "$consumer_dir/src"
cat >"$consumer_dir/Cargo.toml" <<EOF
[package]
name = "arc-package-consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
arc-web = { path = "$repo_root/target/package/arc-web-${version}", features = ["nats", "postgres"] }
arc-stack = "${version}"
arc-core = "${version}"
EOF

cat >"$consumer_dir/src/main.rs" <<'EOF'
use arc_web::{ArcApp, ProjectorReg, helpers::{config::DatabaseDriver, es_stack}};
use arc_core::{aggregate::Aggregate, projection::Projector};
#[allow(dead_code)]
fn compatibility<A: Aggregate>(stack: es_stack::SharedStack<A>, projector: Box<dyn Projector>) {
    let _: arc_stack::SharedStack<A> = stack;
    let _: arc_stack::ProjectorReg = ProjectorReg { projector, view: "view".into() };
    let _: arc_stack::config::DatabaseDriver = DatabaseDriver::Sqlite;
    let _ = es_stack::build::<A>;
    let _ = es_stack::build_with_driver::<A>;
}

fn main() {
    let _ = ArcApp;
    let _: Option<ProjectorReg> = None;
    let _ = DatabaseDriver::Sqlite;
    let _ = arc_web::helpers::es_stack::build_stores;
}
EOF

echo "Compiling arc-web from a standalone external consumer..."
cargo check --manifest-path "$consumer_dir/Cargo.toml" "${package_overrides[@]}"

echo "Publish-package verification passed."
