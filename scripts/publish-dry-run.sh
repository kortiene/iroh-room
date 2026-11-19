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

work=""
cleanup() {
  rc=$?
  if [[ -n "$work" ]]; then
    git -C "$root" worktree remove --force --force "$work" >/dev/null 2>&1 || rm -rf "$work"
  fi
  git -C "$root" worktree prune >/dev/null 2>&1 || true
  exit "$rc"
}
trap cleanup EXIT

work="$(mktemp -d "${TMPDIR:-/tmp}/iroh-rooms-publish-dry-run.XXXXXX")"
git worktree add --detach "$work" HEAD

python3 - "$work" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifests = [
    "crates/iroh-rooms-crypto/Cargo.toml",
    "crates/iroh-rooms-core/Cargo.toml",
    "crates/iroh-rooms-net/Cargo.toml",
    "crates/iroh-rooms/Cargo.toml",
]
for rel in manifests:
    path = root / rel
    lines = path.read_text().splitlines(keepends=True)
    kept = [line for line in lines if line.strip() != "publish = false"]
    if len(kept) == len(lines):
        raise SystemExit(f"{rel} has no publish = false line to strip")
    path.write_text("".join(kept))
PY

# The throwaway commit is only in this detached worktree, so the dry run's
# git tree is clean without --allow-dirty. It is deleted with the worktree.
git -C "$work" add \
  crates/iroh-rooms-crypto/Cargo.toml \
  crates/iroh-rooms-core/Cargo.toml \
  crates/iroh-rooms-net/Cargo.toml \
  crates/iroh-rooms/Cargo.toml
git -C "$work" \
  -c user.name="sdk-publish-dry-run" \
  -c user.email="sdk-publish-dry-run@localhost" \
  commit -m "Strip publish = false for the dry run."

cargo publish --dry-run --locked \
  --manifest-path "$work/Cargo.toml" \
  -p iroh-rooms-crypto \
  -p iroh-rooms-core \
  -p iroh-rooms-net \
  -p iroh-rooms
