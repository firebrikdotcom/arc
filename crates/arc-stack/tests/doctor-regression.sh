#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/scripts" "$dir/bin" "$dir/crates/arc-web/src/helpers" "$dir/crates/arc-app/src/helpers" "$dir/crates/arc-auth-session" "$dir/crates/arc-stack" "$dir/docs" "$dir/config/benthos/generated"
cp "$root/scripts/arc-check.sh" "$dir/scripts/"
cp "$root/crates/arc-web/src/helpers/es_stack.rs" "$dir/crates/arc-web/src/helpers/"
cp "$root/crates/arc-auth-session/Cargo.toml" "$dir/crates/arc-auth-session/"
cp "$root/docs/index.html" "$root/docs/_sidebar.md" "$root/docs/auth-plugins.md" "$dir/docs/"
cp "$root/config/benthos/generated/events.yaml" "$dir/config/benthos/generated/"
for crate in arc-core arc-es-sqlite arc-es-postgres arc-es-nats; do mkdir -p "$dir/crates/$crate"; done
cat > "$dir/bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n' 'arc-stack v0.9.0' "${FAKE_DEPENDENCY:-arc-core v0.9.0}"
SH
printf '#!/usr/bin/env bash\nexit 0\n' > "$dir/bin/npm"
chmod +x "$dir/bin/"*
export PATH="$dir/bin:$PATH"
(cd "$dir" && bash scripts/arc-check.sh >/dev/null)
if (cd "$dir" && FAKE_DEPENDENCY='actix-web v4.0.0' bash scripts/arc-check.sh >/dev/null 2>&1); then echo 'HTTP dependency guard failed' >&2; exit 1; fi
touch "$dir/crates/arc-app/src/helpers/es_stack.rs"
if (cd "$dir" && bash scripts/arc-check.sh >/dev/null 2>&1); then echo 'app copy guard failed' >&2; exit 1; fi
rm "$dir/crates/arc-app/src/helpers/es_stack.rs"
printf '\nfn duplicate_assembly() {}\n' >> "$dir/crates/arc-web/src/helpers/es_stack.rs"
if (cd "$dir" && bash scripts/arc-check.sh >/dev/null 2>&1); then echo 'facade implementation guard failed' >&2; exit 1; fi
echo 'Doctor regression fixtures passed.'
