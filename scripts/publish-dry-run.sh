#!/usr/bin/env bash
# Dry-run the SDK publish set. This script must not upload crates or log in.
# See docs/sdk-publish.md.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if grep -nE '^[^#]*cargo[[:space:]]+login' "$0" >/dev/null; then
  echo "refusing to run: registry login is not allowed" >&2
  exit 2
fi
if grep -nE '^[^#]*cargo[[:space:]]+publish' "$0" | grep -v -- '--dry-run' >/dev/null; then
  echo "refusing to run: cargo publish must include --dry-run" >&2
  exit 2
fi

crates=(
  crates/iroh-rooms-crypto
  crates/iroh-rooms-core
  crates/iroh-rooms-net
  crates/iroh-rooms
)

status=0
for crate in "${crates[@]}"; do
  echo "===== cargo publish --dry-run: ${crate} ====="
  if cargo publish --dry-run --locked --allow-dirty --manifest-path "${crate}/Cargo.toml"; then
    echo "dry-run exit 0: ${crate}"
  else
    rc=$?
    echo "dry-run exit ${rc}: ${crate}"
    status=1
  fi

  echo "===== cargo package --no-verify: ${crate} ====="
  if cargo package --locked --allow-dirty --no-verify --manifest-path "${crate}/Cargo.toml"; then
    echo "package exit 0: ${crate}"
  else
    rc=$?
    echo "package exit ${rc}: ${crate}"
    status=1
  fi
done

exit "${status}"
