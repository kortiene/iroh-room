#!/usr/bin/env bash
set -euo pipefail

# Build a versioned Production Beta CLI archive and SHA-256 checksum.
#
# Usage:
#   scripts/build-release-artifacts.sh
#   scripts/build-release-artifacts.sh --version 0.1.0-beta.1
#   scripts/build-release-artifacts.sh --allow-dirty
#
# The script refuses a dirty worktree by default because release artifacts must
# be traceable to a commit. Use --allow-dirty only for local dry-runs; never for
# a Production Beta sign-off.

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ALLOW_DIRTY=0
VERSION=""

usage() {
  cat <<'EOF'
Usage:
  scripts/build-release-artifacts.sh
  scripts/build-release-artifacts.sh --version 0.1.0-beta.1
  scripts/build-release-artifacts.sh --allow-dirty

Build a versioned Production Beta CLI archive and SHA-256 checksum. The script
refuses a dirty worktree by default because release artifacts must be traceable
to a commit. Use --allow-dirty only for local dry-runs.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --allow-dirty)
      ALLOW_DIRTY=1
      shift
      ;;
    --version)
      if [[ $# -lt 2 ]]; then
        echo "error: --version requires a value" >&2
        exit 2
      fi
      VERSION="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "usage: $0 [--version VERSION] [--allow-dirty]" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  VERSION="$(awk -F ' *= *' '
    /^\[package\]/ { in_package = 1; next }
    /^\[/ && in_package { exit }
    in_package && $1 == "version" {
      gsub(/"/, "", $2)
      print $2
      exit
    }
  ' crates/iroh-rooms-cli/Cargo.toml)"
fi

if [[ -z "$VERSION" ]]; then
  echo "error: could not derive version from crates/iroh-rooms-cli/Cargo.toml" >&2
  exit 1
fi

if [[ ! "$VERSION" =~ ^[A-Za-z0-9._+-]+$ ]]; then
  echo "error: unsafe version string: $VERSION" >&2
  exit 1
fi

COMMIT="$(git rev-parse --short=12 HEAD)"
DIRTY_SUFFIX=""
if [[ -n "$(git status --porcelain)" ]]; then
  if [[ "$ALLOW_DIRTY" -ne 1 ]]; then
    echo "error: worktree is dirty; commit or stash changes before building release artifacts" >&2
    echo "       use --allow-dirty only for local dry-runs" >&2
    exit 1
  fi
  DIRTY_SUFFIX="-dirty"
fi

HOST="$(rustc -vV | awk '/^host: / { print $2 }')"
if [[ -z "$HOST" ]]; then
  echo "error: could not determine rustc host triple" >&2
  exit 1
fi

DIST_DIR="target/release-artifacts"
STAGE_ROOT="$DIST_DIR/stage"
STAGE_NAME="iroh-rooms-${VERSION}-${HOST}-${COMMIT}${DIRTY_SUFFIX}"
STAGE_DIR="$STAGE_ROOT/$STAGE_NAME"
ARCHIVE="$DIST_DIR/${STAGE_NAME}.tar.gz"

echo "-- building iroh-rooms release binary"
cargo build -p iroh-rooms-cli --release

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"
install -m 0755 target/release/iroh-rooms "$STAGE_DIR/iroh-rooms"

cat > "$STAGE_DIR/RELEASE-MANIFEST.txt" <<EOF
name: iroh-rooms
version: ${VERSION}
commit: ${COMMIT}${DIRTY_SUFFIX}
host: ${HOST}
source: $(git config --get remote.origin.url || echo unknown)
built_at_utc: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
binary: iroh-rooms

This archive is checksummed by the adjacent .sha256 file. Production Beta
artifacts are not project-signed unless the release notes explicitly say so.
EOF

mkdir -p "$DIST_DIR"
rm -f "$ARCHIVE" "$ARCHIVE.sha256"
tar -C "$STAGE_ROOT" -czf "$ARCHIVE" "$STAGE_NAME"

if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$ARCHIVE" > "$ARCHIVE.sha256"
elif command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
else
  echo "error: need shasum or sha256sum to write artifact checksum" >&2
  exit 1
fi

echo
echo "release artifact: $ARCHIVE"
echo "checksum:         $ARCHIVE.sha256"
cat "$ARCHIVE.sha256"
