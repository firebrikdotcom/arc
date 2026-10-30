#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/scripts" "$dir/bin" "$dir/crates/arc-web/src/helpers" "$dir/crates/arc-app/src/helpers" "$dir/crates/arc-auth-session" "$dir/crates/arc-ffi/src" "$dir/docs" "$dir/config/benthos/generated"
cp "$root/scripts/arc-check.sh" "$dir/scripts/"
cp "$root/crates/arc-web/src/helpers/es_stack.rs" "$dir/crates/arc-web/src/helpers/"
cp "$root/crates/arc-auth-session/Cargo.toml" "$dir/crates/arc-auth-session/"
cp "$root/docs/index.html" "$root/docs/_sidebar.md" "$root/docs/auth-plugins.md" "$dir/docs/"
cp "$root/config/benthos/generated/events.yaml" "$dir/config/benthos/generated/"
for crate in arc-core arc-stack arc-es-sqlite arc-es-postgres arc-es-nats; do mkdir -p "$dir/crates/$crate"; done
cat > "$dir/bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n' 'arc-ffi v0.8.11'
# Leak only through the isolated native consumer, retaining the old stack check.
if [[ "$*" == *--manifest-path* ]]; then printf '%s\n' "${FAKE_DEPENDENCY:-arc-core v0.8.11}"; fi
SH
printf '#!/usr/bin/env bash\nexit 0\n' > "$dir/bin/npm"
chmod +x "$dir/bin/"*
export PATH="$dir/bin:$PATH"
(cd "$dir" && bash scripts/arc-check.sh >/dev/null)
for dependency in 'actix-web v4.0.0' 'tera v1.0.0' 'arc-web v0.8.11' 'arc-auth-core v0.8.11'; do
  if (cd "$dir" && FAKE_DEPENDENCY="$dependency" bash scripts/arc-check.sh >/dev/null 2>&1); then echo "Native dependency guard failed: $dependency" >&2; exit 1; fi
done
for path in domain services validation; do
  mkdir "$dir/crates/arc-ffi/src/$path"
  if (cd "$dir" && bash scripts/arc-check.sh >/dev/null 2>&1); then echo "Native ownership guard failed: $path" >&2; exit 1; fi
  rmdir "$dir/crates/arc-ffi/src/$path"
done
printf 'fn duplicate() { CommandBus::new(); }\n' > "$dir/crates/arc-ffi/src/lib.rs"
if (cd "$dir" && bash scripts/arc-check.sh >/dev/null 2>&1); then echo 'Copied assembly guard failed' >&2; exit 1; fi
echo 'Native doctor regression fixtures passed.'
