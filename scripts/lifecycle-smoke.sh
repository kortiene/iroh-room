#!/usr/bin/env bash
# Bash 3.2 (stock macOS) and newer. No associative arrays, mapfile, ${var,,},
# ${var^^}, &>>, namerefs, or empty-array expansion under `set -u`.
set -euo pipefail

# Install, upgrade, roll back, and uninstall one published Iroh Rooms archive
# against the previous candidate, and record the result.
#
# Usage:
#   scripts/lifecycle-smoke.sh <prior-tag> <candidate-tag> <triple> \
#     [--candidate-binary PATH] [--out DIR]
#
# Tags match ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ and must differ.
# <triple> is x86_64-apple-darwin, aarch64-apple-darwin,
# x86_64-unknown-linux-gnu, or aarch64-unknown-linux-gnu.
#
# Downloads use curl or gh only. rc.6 and later are verified with
# scripts/verify-release-artifacts.sh (full). v0.1.0-rc.1 through
# v0.1.0-rc.5 are checksum-only: the per-archive .sha256 must pass, and that
# mode is recorded as prior_verification=checksum-only. The full verifier
# exits 1 on those tags by design, so it is not the install gate for them.
# A non-zero verification exit never installs.
#
# --candidate-binary PATH is local dev mode. The record class is unreleased.
# The flag is refused (exit 2) when GITHUB_ACTIONS=true.
#
# Class rule:
#   unreleased  --candidate-binary is set. Local dev only. Refused when
#               GITHUB_ACTIONS=true.
#   evidence    GitHub Actions and GITHUB_REF=refs/heads/main, without
#               --candidate-binary.
#   probe       any other GitHub Actions run.
#   local       a published-asset run outside GitHub Actions.
#
# The temp HOME is mktemp -d "${TMPDIR:-/tmp}/lifecycle.XXXXXX", then pwd -P
# (macOS /var vs /private/var). On Linux, XDG_DATA_HOME is set explicitly.
# Identity material stays in that temp directory. This script does not call
# the macOS security tool or any keychain API. Both an explicit
# IROH_ROOMS_HOME and the platform-default data directory are exercised.
# Data is removed only by deleting that temp directory.
#
# Records: <out>/lifecycle-<triple>.json is the source.
# <out>/lifecycle-<triple>.md is rendered from that JSON.
# Default --out is the current directory.
#
# Network steps (verify prior, and verify candidate when it is a published
# archive) retry once. Both attempts are recorded. A failed attempt is never
# marked passed. Exit 0 only when every step passes.
#
# Refs #265 #293

REPO="kortiene/iroh-room"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFIER="${ROOT}/scripts/verify-release-artifacts.sh"

CLASS_RULE="unreleased: --candidate-binary is set (local dev only; refused when GITHUB_ACTIONS=true). evidence: GitHub Actions and GITHUB_REF is refs/heads/main, without --candidate-binary. probe: any other GitHub Actions run. local: a published-asset run outside GitHub Actions."

PRIOR_TAG=""
CANDIDATE_TAG=""
TRIPLE=""
CANDIDATE_BINARY=""
OUT_DIR=""
WORK=""
REAL_HOME=""
PREFIX=""
ROOM_ID=""
BACKUP_ARCHIVE=""
FAILED_STEP=""
RESULT=""
LIFECYCLE_STARTED=0

PRIOR_VERIFICATION=""
CANDIDATE_VERIFICATION=""
PRIOR_ASSET_DIR=""
CANDIDATE_ASSET_DIR=""
PRIOR_ARCHIVE=""
CANDIDATE_ARCHIVE=""
PRIOR_PEELED=""
CANDIDATE_PEELED=""

PRIOR_MANIFEST_COMMIT=""
PRIOR_ARCHIVE_COMMIT12=""
PRIOR_COMMIT_MATCH=""
PRIOR_VERSION_EXPECTED=""
PRIOR_VERSION_ACTUAL=""
PRIOR_VERSION_MATCH=""
CANDIDATE_MANIFEST_COMMIT=""
CANDIDATE_ARCHIVE_COMMIT12=""
CANDIDATE_COMMIT_MATCH=""
CANDIDATE_VERSION_EXPECTED=""
CANDIDATE_VERSION_ACTUAL=""
CANDIDATE_VERSION_MATCH=""

GLIBC_MAX_SYMBOL=""
GLIBC_METHOD=""
LDD_VERSION=""
USER_VERSION_BEFORE=""
USER_VERSION_AFTER=""
SIMULATED=""
DATA_DIRS=""
GH_VERSION=""
COSIGN_VERSION=""
RUNNER_HOSTED=""
STEPS_JSONL=""
ARCHIVES_JSONL=""

usage() {
  cat <<'EOF'
Usage:
  scripts/lifecycle-smoke.sh <prior-tag> <candidate-tag> <triple> \
    [--candidate-binary PATH] [--out DIR]

Run an install, upgrade, rollback, and uninstall smoke against two published
tags, and write lifecycle-<triple>.json plus a markdown record rendered from
that JSON.

  <prior-tag>      Previous release tag, for example v0.1.0-rc.5
  <candidate-tag>  Candidate release tag, for example v0.1.0-rc.6
  <triple>         x86_64-apple-darwin, aarch64-apple-darwin,
                   x86_64-unknown-linux-gnu, or aarch64-unknown-linux-gnu

Options:
  --candidate-binary PATH   Install this local binary as the candidate.
                            Class is unreleased. Refused when GITHUB_ACTIONS=true.
  --out DIR                 Directory for the JSON and markdown records.
                            Default: the current directory.
  -h, --help                Show this help.

Class rule: unreleased when --candidate-binary is set (local dev only; refused
on GitHub Actions); evidence only on GitHub Actions with GITHUB_REF=refs/heads/main
and no --candidate-binary; probe for any other GitHub Actions run; local for a
published-asset run outside GitHub Actions.

v0.1.0-rc.1 through v0.1.0-rc.5 are verified checksum-only (.sha256). rc.6 and
later are verified with scripts/verify-release-artifacts.sh. Exit 0 only when
every step passes.
EOF
}

die() {
  local code="$1"
  shift
  printf 'error: %s\n' "$*" >&2
  exit "$code"
}

class_of() {
  local candidate_binary="${1:-}"
  if [[ -n "$candidate_binary" ]]; then
    printf '%s\n' "unreleased"
    return 0
  fi
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    if [[ "${GITHUB_REF:-}" == "refs/heads/main" ]]; then
      printf '%s\n' "evidence"
    else
      printf '%s\n' "probe"
    fi
    return 0
  fi
  printf '%s\n' "local"
}

tag_ok() {
  [[ "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]]
}

is_checksum_only_tag() {
  case "$1" in
    v0.1.0-rc.1 | v0.1.0-rc.2 | v0.1.0-rc.3 | v0.1.0-rc.4 | v0.1.0-rc.5)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

now_utc() {
  date -u '+%Y-%m-%dT%H:%M:%SZ'
}

now_et() {
  TZ=America/New_York date '+%Y-%m-%dT%H:%M:%S%z'
}

json_string() {
  python3 -c 'import json,sys; sys.stdout.write(json.dumps(sys.argv[1]))' "$1"
}

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{ print $1 }'
  else
    shasum -a 256 "$file" | awk '{ print $1 }'
  fi
}

check_sha256() {
  local sumfile="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$sumfile"
  else
    shasum -a 256 -c "$sumfile"
  fi
}

file_mode() {
  local mode=""
  mode="$(stat -c '%a' "$1" 2>/dev/null || true)"
  if [[ -n "$mode" ]]; then
    printf '%s\n' "$mode"
    return 0
  fi
  stat -f '%OLp' "$1"
}

sqlite_scalar() {
  local db="$1"
  local sql="$2"
  if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$db" "$sql"
    return $?
  fi
  python3 -c 'import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
row = c.execute(sys.argv[2]).fetchone()
c.close()
if row is not None and row[0] is not None:
    sys.stdout.write(str(row[0]))
' "$db" "$sql"
}

sqlite_exec() {
  local db="$1"
  local sql="$2"
  if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$db" "$sql"
    return $?
  fi
  python3 -c 'import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.executescript(sys.argv[2])
c.commit()
c.close()
' "$db" "$sql"
}

checkpoint_db() {
  local db="$1"
  if [[ ! -f "$db" ]]; then
    return 0
  fi
  sqlite_exec "$db" "PRAGMA wal_checkpoint(TRUNCATE);" || return $?
}

iroh_rooms_running() {
  local comm=""
  if command -v pgrep >/dev/null 2>&1; then
    pgrep -x iroh-rooms >/dev/null 2>&1
    return $?
  fi
  while IFS= read -r comm; do
    if [[ "$comm" == "iroh-rooms" ]]; then
      return 0
    fi
  done < <(ps -ax -o comm= 2>/dev/null || true)
  return 1
}

physical_dir() {
  (cd "$1" && pwd -P)
}

read_manifest_field() {
  local file="$1"
  local key="$2"
  awk -F ': ' -v key="$key" '$1 == key { print $2; exit }' "$file"
}

append_attempt() {
  local existing="$1"
  local n="$2"
  local code="$3"
  local et="$4"
  local utc="$5"
  local item
  item="$(printf '{"n":%s,"exit_code":%s,"et":%s,"utc":%s}' \
    "$n" "$code" "$(json_string "$et")" "$(json_string "$utc")")"
  if [[ -z "$existing" || "$existing" == "[]" ]]; then
    printf '[%s]' "$item"
    return 0
  fi
  printf '%s,%s]' "${existing%]}" "$item"
}

record_step_line() {
  python3 -c 'import json,sys
attempts = json.loads(sys.argv[7])
json.dump({
    "name": sys.argv[1],
    "exit_code": int(sys.argv[2]),
    "started_et": sys.argv[3],
    "started_utc": sys.argv[4],
    "ended_et": sys.argv[5],
    "ended_utc": sys.argv[6],
    "attempts": attempts,
}, sys.stdout)
sys.stdout.write("\n")' "$@" >>"$STEPS_JSONL"
}

record_archive() {
  python3 -c 'import json,sys
json.dump({
    "role": sys.argv[1],
    "tag": sys.argv[2],
    "name": sys.argv[3],
    "sha256": sys.argv[4],
    "verification": sys.argv[5],
}, sys.stdout)
sys.stdout.write("\n")' "$@" >>"$ARCHIVES_JSONL"
}

run_step() {
  local name="$1"
  local mode="$2"
  shift 2
  if [[ -n "$FAILED_STEP" ]]; then
    return 0
  fi
  printf '== %s ==\n' "$name" >&2
  local started_et started_utc ended_et ended_utc
  local code=0 n=1 et utc attempts=""
  started_et="$(now_et)"
  started_utc="$(now_utc)"
  et="$(now_et)"
  utc="$(now_utc)"
  "$@" || code=$?
  attempts="$(append_attempt "$attempts" "$n" "$code" "$et" "$utc")"
  if [[ "$code" -ne 0 && "$mode" == "retry" ]]; then
    printf 'retry %s (attempt 1 exit %s)\n' "$name" "$code" >&2
    n=2
    et="$(now_et)"
    utc="$(now_utc)"
    code=0
    "$@" || code=$?
    attempts="$(append_attempt "$attempts" "$n" "$code" "$et" "$utc")"
  fi
  ended_et="$(now_et)"
  ended_utc="$(now_utc)"
  record_step_line "$name" "$code" "$started_et" "$started_utc" "$ended_et" "$ended_utc" "$attempts"
  if [[ "$code" -ne 0 ]]; then
    FAILED_STEP="$name"
    RESULT="FAIL (${name})"
    printf 'FAIL (%s) exit %s\n' "$name" "$code" >&2
    return 1
  fi
  printf 'ok %s\n' "$name" >&2
  return 0
}

peel_with_gh() {
  local tag="$1"
  local ref_type="" ref_sha="" peeled_type="" sha=""
  ref_type="$(gh api "repos/${REPO}/git/ref/tags/${tag}" --jq .object.type)" || return 1
  ref_sha="$(gh api "repos/${REPO}/git/ref/tags/${tag}" --jq .object.sha)" || return 1
  case "$ref_type" in
    commit)
      sha="$ref_sha"
      ;;
    tag)
      peeled_type="$(gh api "repos/${REPO}/git/tags/${ref_sha}" --jq .object.type)" || return 1
      sha="$(gh api "repos/${REPO}/git/tags/${ref_sha}" --jq .object.sha)" || return 1
      if [[ "$peeled_type" != "commit" ]]; then
        printf 'error: annotated tag %s peels to %s, not a commit\n' "$tag" "$peeled_type" >&2
        return 1
      fi
      ;;
    *)
      printf 'error: refs/tags/%s points at %s\n' "$tag" "$ref_type" >&2
      return 1
      ;;
  esac
  printf '%s\n' "$sha"
}

peel_with_git() {
  local tag="$1"
  local remote="https://github.com/${REPO}.git"
  local obj="" ref="" sha="" peeled="" listing=""
  listing="$(git ls-remote "$remote" "refs/tags/${tag}^{}" "refs/tags/${tag}")" || return $?
  while IFS="$(printf '\t')" read -r obj ref; do
    case "$ref" in
      "refs/tags/${tag}^{}")
        peeled="$obj"
        ;;
      "refs/tags/${tag}")
        sha="$obj"
        ;;
    esac
  done <<< "$listing"
  if [[ -n "$peeled" ]]; then
    sha="$peeled"
  fi
  if [[ -z "$sha" ]]; then
    printf 'error: git ls-remote did not find refs/tags/%s\n' "$tag" >&2
    return 1
  fi
  printf '%s\n' "$sha"
}

resolve_tag_commit() {
  local tag="$1"
  local out_var="$2"
  local sha=""
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    if sha="$(peel_with_gh "$tag")"; then
      :
    elif sha="$(peel_with_git "$tag")"; then
      printf 'note: peeled %s with git ls-remote after gh api failed\n' "$tag" >&2
    else
      printf 'error: could not peel refs/tags/%s\n' "$tag" >&2
      return 1
    fi
  else
    sha="$(peel_with_git "$tag")" || return 1
  fi
  if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    printf 'error: refs/tags/%s did not peel to a 40-hex commit (got %s)\n' "$tag" "$sha" >&2
    return 1
  fi
  printf -v "$out_var" '%s' "$sha"
}

download_with_curl() {
  local tag="$1"
  local dest="$2"
  local version="${tag#v}"
  local listing="${dest}/release.json"
  local only=0
  local names="" name=""
  if is_checksum_only_tag "$tag"; then
    only=1
  fi
  curl -fsSL \
    -H "Accept: application/vnd.github+json" \
    -o "$listing" \
    "https://api.github.com/repos/${REPO}/releases/tags/${tag}" || return $?
  names="$(python3 -c 'import json, sys
tag, triple, checksum_only, path = sys.argv[1:5]
data = json.load(open(path))
version = tag[1:]
prefix = "iroh-rooms-%s-%s-" % (version, triple)
want = []
for asset in data.get("assets", []):
    name = asset["name"]
    if name.startswith(prefix):
        want.append(name)
    elif checksum_only != "1" and name in ("SHA256SUMS", "SHA256SUMS.sigstore.json"):
        want.append(name)
sys.stdout.write("\n".join(want))
' "$tag" "$TRIPLE" "$only" "$listing")" || return $?
  rm -f "$listing"
  if [[ -z "$names" ]]; then
    printf 'error: release %s has no assets for %s\n' "$tag" "$TRIPLE" >&2
    return 1
  fi
  while IFS= read -r name; do
    if [[ -z "$name" ]]; then
      continue
    fi
    curl -fsSL -o "${dest}/${name}" \
      "https://github.com/${REPO}/releases/download/${tag}/${name}" || return $?
  done <<< "$names"
}

download_assets() {
  local tag="$1"
  local dest="$2"
  local version="${tag#v}"
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    if is_checksum_only_tag "$tag"; then
      gh release download "$tag" --repo "$REPO" --dir "$dest" \
        --pattern "iroh-rooms-${version}-${TRIPLE}-*" || return $?
    else
      gh release download "$tag" --repo "$REPO" --dir "$dest" \
        --pattern "iroh-rooms-${version}-${TRIPLE}-*" \
        --pattern "SHA256SUMS*" || return $?
    fi
    return 0
  fi
  if command -v curl >/dev/null 2>&1; then
    download_with_curl "$tag" "$dest" || return $?
    return 0
  fi
  printf 'error: need gh or curl to download release assets\n' >&2
  return 1
}

pick_archive() {
  local dest="$1"
  local tag="$2"
  local version="${tag#v}"
  local prefix="iroh-rooms-${version}-${TRIPLE}-"
  local path="" name="" rest="" suffix="" chosen=""
  local count=0
  local nullglob_was=0
  if shopt -q nullglob; then
    nullglob_was=1
  fi
  shopt -s nullglob
  for path in "$dest"/"$prefix"*.tar.gz; do
    name="$(basename "$path")"
    rest="${name#"$prefix"}"
    suffix="${rest%.tar.gz}"
    if [[ "$rest" == "${suffix}.tar.gz" && "$suffix" =~ ^[0-9a-f]{12}$ ]]; then
      count=$((count + 1))
      chosen="$name"
    fi
  done
  if [[ "$nullglob_was" -eq 0 ]]; then
    shopt -u nullglob
  fi
  if [[ "$count" -ne 1 ]]; then
    printf 'error: expected one archive for %s %s, found %s\n' "$tag" "$TRIPLE" "$count" >&2
    return 1
  fi
  printf '%s\n' "$chosen"
}

verify_published() {
  local role="$1"
  local tag="$2"
  local dest="${WORK}/assets-${role}"
  local peeled="" archive="" sum="" verification=""
  rm -rf "$dest" || return 1
  mkdir -p "$dest" || return 1
  resolve_tag_commit "$tag" peeled || return 1
  download_assets "$tag" "$dest" || return 1
  archive="$(pick_archive "$dest" "$tag")" || return 1
  sum="$(sha256_file "${dest}/${archive}")" || return 1
  if is_checksum_only_tag "$tag"; then
    if [[ ! -f "${dest}/${archive}.sha256" ]]; then
      printf 'error: missing %s.sha256\n' "$archive" >&2
      return 1
    fi
    (cd "$dest" && check_sha256 "${archive}.sha256") || return 1
    verification="checksum-only"
  else
    "$VERIFIER" --dir "$dest" --commit "$peeled" "$tag" "$TRIPLE" || return 1
    verification="full"
  fi
  record_archive "$role" "$tag" "$archive" "$sum" "$verification" || return 1
  if [[ "$role" == "prior" ]]; then
    PRIOR_VERIFICATION="$verification"
    PRIOR_ASSET_DIR="$dest"
    PRIOR_ARCHIVE="$archive"
    PRIOR_PEELED="$peeled"
  else
    CANDIDATE_VERIFICATION="$verification"
    CANDIDATE_ASSET_DIR="$dest"
    CANDIDATE_ARCHIVE="$archive"
    CANDIDATE_PEELED="$peeled"
  fi
}

verify_candidate_impl() {
  if [[ -n "$CANDIDATE_BINARY" ]]; then
    if [[ ! -f "$CANDIDATE_BINARY" ]]; then
      printf 'error: --candidate-binary is not a file: %s\n' "$CANDIDATE_BINARY" >&2
      return 1
    fi
    CANDIDATE_VERIFICATION="unreleased"
    CANDIDATE_COMMIT_MATCH="n/a"
    CANDIDATE_VERSION_MATCH="n/a"
    return 0
  fi
    verify_published "candidate" "$CANDIDATE_TAG" || return 1
}

verify_prior_impl() {
  verify_published "prior" "$PRIOR_TAG" || return 1
}

install_binary() {
  local src="$1"
  local dest="$2"
  local dest_dir tmp
  dest_dir="$(dirname "$dest")"
  mkdir -p "$dest_dir" || return 1
  tmp="${dest}.tmp.$$"
  install -m 0755 "$src" "$tmp" || return 1
  mv "$tmp" "$dest" || return 1
}

assert_release_identity() {
  local role="$1"
  local manifest="$2"
  local archive="$3"
  local peeled="$4"
  local bin="$5"
  local version="" commit="" actual="" expected="" commit12="" peeled12=""
  version="$(read_manifest_field "$manifest" "version")"
  commit="$(read_manifest_field "$manifest" "commit")"
  commit12="${archive%.tar.gz}"
  commit12="${commit12##*-}"
  peeled12="${peeled:0:12}"
  actual="$("$bin" --version | tr -d '\r')" || return 1
  expected="iroh-rooms ${version}"
  if [[ "$commit" != "$commit12" || "$peeled12" != "$commit12" ]]; then
    printf 'error: commit mismatch for %s: manifest %s archive %s peeled %s\n' \
      "$archive" "$commit" "$commit12" "$peeled" >&2
    return 1
  fi
  if [[ "$actual" != "$expected" ]]; then
    printf 'error: --version [%s] does not match manifest [%s]\n' "$actual" "$expected" >&2
    return 1
  fi
  if [[ "$role" == "prior" ]]; then
    PRIOR_MANIFEST_COMMIT="$commit"
    PRIOR_ARCHIVE_COMMIT12="$commit12"
    PRIOR_COMMIT_MATCH="true"
    PRIOR_VERSION_EXPECTED="$expected"
    PRIOR_VERSION_ACTUAL="$actual"
    PRIOR_VERSION_MATCH="true"
  else
    CANDIDATE_MANIFEST_COMMIT="$commit"
    CANDIDATE_ARCHIVE_COMMIT12="$commit12"
    CANDIDATE_COMMIT_MATCH="true"
    CANDIDATE_VERSION_EXPECTED="$expected"
    CANDIDATE_VERSION_ACTUAL="$actual"
    CANDIDATE_VERSION_MATCH="true"
  fi
}

install_from_archive() {
  local role="$1"
  local asset_dir="$2"
  local archive="$3"
  local peeled="$4"
  local dest_bin="$5"
  local extract="${WORK}/extract-${role}"
  local inner=""
  rm -rf "$extract" || return 1
  mkdir -p "$extract" || return 1
  tar -xzf "${asset_dir}/${archive}" -C "$extract" || return 1
  inner="${extract}/${archive%.tar.gz}"
  if [[ ! -f "${inner}/iroh-rooms" || ! -f "${inner}/RELEASE-MANIFEST.txt" ]]; then
    printf 'error: %s is missing iroh-rooms or RELEASE-MANIFEST.txt\n' "$archive" >&2
    return 1
  fi
  install_binary "${inner}/iroh-rooms" "$dest_bin" || return 1
  assert_release_identity "$role" "${inner}/RELEASE-MANIFEST.txt" "$archive" "$peeled" "$dest_bin" || return 1
}

install_prior_impl() {
  if [[ -z "$PRIOR_ASSET_DIR" || -z "$PRIOR_ARCHIVE" || -z "$PRIOR_PEELED" ]]; then
    printf 'error: prior archive was not verified; refusing to install\n' >&2
    return 1
  fi
  if [[ "$PRIOR_VERIFICATION" != "checksum-only" && "$PRIOR_VERIFICATION" != "full" ]]; then
    printf 'error: prior verification did not pass; refusing to install\n' >&2
    return 1
  fi
  install_from_archive "prior" "$PRIOR_ASSET_DIR" "$PRIOR_ARCHIVE" "$PRIOR_PEELED" \
    "${PREFIX}/bin/iroh-rooms" || return 1
}

run_rooms() {
  local stdout="$1"
  local stderr="$2"
  shift 2
  local code=0
  "$PREFIX/bin/iroh-rooms" "$@" >"$stdout" 2>"$stderr" || code=$?
  if [[ "$code" -ne 0 ]]; then
    printf 'error: iroh-rooms exited %s\n' "$code" >&2
    cat "$stderr" >&2
    return "$code"
  fi
}

capture_reads() {
  local dest="$1"
  mkdir -p "$dest" || return 1
  run_rooms "${dest}/identity.show" "${dest}/identity.show.err" identity show || return 1
  run_rooms "${dest}/members" "${dest}/members.err" room members "$ROOM_ID" || return 1
  run_rooms "${dest}/tail" "${dest}/tail.err" room tail --offline "$ROOM_ID" || return 1
}

cover_platform_default() {
  local expected="" actual_phys="" expected_phys=""
  case "$(uname -s)" in
    Darwin)
      expected="${HOME}/Library/Application Support/iroh-rooms"
      ;;
    Linux)
      expected="${XDG_DATA_HOME}/iroh-rooms"
      ;;
    *)
      printf 'error: platform-default data dir is not defined for %s\n' "$(uname -s)" >&2
      return 1
      ;;
  esac
  env -u IROH_ROOMS_HOME "$PREFIX/bin/iroh-rooms" identity create --name lifecycle-platform-default \
    >"${WORK}/platform-default.out" 2>"${WORK}/platform-default.err" || {
    printf 'error: platform-default identity create failed\n' >&2
    cat "${WORK}/platform-default.err" >&2
    return 1
  }
  if [[ ! -f "${expected}/identity.json" ]]; then
    printf 'error: platform-default identity was not written to %s\n' "$expected" >&2
    return 1
  fi
  actual_phys="$(physical_dir "$expected")" || return 1
  expected_phys="$(physical_dir "$(dirname "$expected")")/$(basename "$expected")" || return 1
  if [[ "$actual_phys" != "$expected_phys" ]]; then
    printf 'error: platform-default path %s != physical %s\n' "$expected_phys" "$actual_phys" >&2
    return 1
  fi
}

create_data_impl() {
  local create_out=""
  cover_platform_default || return 1
  export IROH_ROOMS_HOME="${HOME}/.local/share/iroh-rooms-beta"
  mkdir -p "$(dirname "$IROH_ROOMS_HOME")"
  run_rooms "${WORK}/identity-create.out" "${WORK}/identity-create.err" \
    identity create --name lifecycle-smoke || return 1
  create_out="$("$PREFIX/bin/iroh-rooms" room create lifecycle-room)" || return 1
  ROOM_ID="$(printf '%s\n' "$create_out" | awk -F ': ' '/^room_id: / { print $2; exit }')"
  if [[ ! "$ROOM_ID" =~ ^blake3:[0-9a-f]+$ ]]; then
    printf 'error: could not read room_id from room create\n' >&2
    return 1
  fi
  run_rooms "${WORK}/send-1.out" "${WORK}/send-1.err" \
    room send "$ROOM_ID" "lifecycle message one" || return 1
  run_rooms "${WORK}/send-2.out" "${WORK}/send-2.err" \
    room send "$ROOM_ID" "lifecycle message two" || return 1
  capture_reads "${WORK}/snap-pre" || return 1
  if ! grep -F -q "lifecycle message one" "${WORK}/snap-pre/tail"; then
    printf 'error: offline tail is missing the first message\n' >&2
    return 1
  fi
  if ! grep -F -q "lifecycle message two" "${WORK}/snap-pre/tail"; then
    printf 'error: offline tail is missing the second message\n' >&2
    return 1
  fi
  USER_VERSION_BEFORE="$(sqlite_scalar "${IROH_ROOMS_HOME}/rooms.db" "PRAGMA user_version;")"
  USER_VERSION_BEFORE="${USER_VERSION_BEFORE//$'\r'/}"
  if [[ ! "$USER_VERSION_BEFORE" =~ ^[0-9]+$ ]]; then
    printf 'error: could not read user_version\n' >&2
    return 1
  fi
  DATA_DIRS="IROH_ROOMS_HOME,platform-default"
}

# docs/operations/install-uninstall.md:252-260
# One function. The tar invocation is the runbook snippet. Preconditions:
# no iroh-rooms process and no rooms.db-wal. umask 077, archive mode 600.
# On macOS, COPYFILE_DISABLE=1 and the archive must not contain ._* entries.
documented_backup() {
  local before_list="" created="" f="" mode=""
  if iroh_rooms_running; then
    printf 'error: an iroh-rooms process is running; backup requires it to be stopped\n' >&2
    return 1
  fi
  if [[ -e "${IROH_ROOMS_HOME}/rooms.db-wal" ]]; then
    printf 'error: %s exists; refusing to back up a store with a live WAL\n' \
      "${IROH_ROOMS_HOME}/rooms.db-wal" >&2
    return 1
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    export COPYFILE_DISABLE=1
  fi
  before_list="$(ls -1 iroh-rooms-backup-*.tar.gz 2>/dev/null || true)"

  # docs/operations/install-uninstall.md:252-260
  export IROH_ROOMS_HOME="${IROH_ROOMS_HOME:-$HOME/.local/share/iroh-rooms-beta}"
  # Written to the current working directory. The archive contains
  # identity.secret, so create it owner-only — a default umask would leave it
  # group- and world-readable.
  ( umask 077
    tar -C "$(dirname "$IROH_ROOMS_HOME")" \
      -czf "iroh-rooms-backup-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" \
      "$(basename "$IROH_ROOMS_HOME")" ) || return 1

  created=""
  for f in iroh-rooms-backup-*.tar.gz; do
    if [[ ! -f "$f" ]]; then
      continue
    fi
    case $'\n'"${before_list}"$'\n' in
      *$'\n'"${f}"$'\n'*) ;;
      *) created="$f" ;;
    esac
  done
  if [[ -z "$created" ]]; then
    printf 'error: documented backup did not create an archive\n' >&2
    return 1
  fi
  mode="$(file_mode "$created")"
  if [[ "$mode" != "600" ]]; then
    printf 'error: backup archive mode is %s, expected 600\n' "$mode" >&2
    return 1
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    if tar -tzf "$created" | grep -E '(^|/)\._' >/dev/null; then
      printf 'error: backup contains AppleDouble ._* entries\n' >&2
      return 1
    fi
  fi
  BACKUP_ARCHIVE="$(pwd -P)/${created}"
  printf 'backup archive mode %s\n' "$mode" >&2
}

backup_impl() {
  local db="${IROH_ROOMS_HOME}/rooms.db"
  local prev=""
  if [[ ! -f "$db" ]]; then
    printf 'error: %s is missing; nothing to back up\n' "$db" >&2
    return 1
  fi
  checkpoint_db "$db" || return 1
  if [[ -e "${db}-wal" ]]; then
    printf 'error: %s still exists after checkpoint\n' "${db}-wal" >&2
    return 1
  fi
  mkdir -p "${WORK}/backup-cwd"
  prev="$(pwd)"
  cd "${WORK}/backup-cwd" || return 1
  documented_backup || {
    cd "$prev" || true
    return 1
  }
  cd "$prev" || return 1
  if [[ ! -f "$BACKUP_ARCHIVE" ]]; then
    printf 'error: backup archive path was not recorded\n' >&2
    return 1
  fi
}

measure_glibc() {
  local bin="$1"
  local symbol=""
  if command -v ldd >/dev/null 2>&1; then
    LDD_VERSION="$(ldd --version 2>&1 | head -n 1 || true)"
  else
    LDD_VERSION="n/a"
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    GLIBC_MAX_SYMBOL="n/a (Mach-O)"
    GLIBC_METHOD="n/a (Mach-O)"
    return 0
  fi
  if command -v readelf >/dev/null 2>&1; then
    symbol="$(readelf -V "$bin" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1 || true)"
    if [[ -n "$symbol" ]]; then
      GLIBC_MAX_SYMBOL="$symbol"
      GLIBC_METHOD="readelf -V \"\$bin\" | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1"
      return 0
    fi
  fi
  symbol="$(grep -ao 'GLIBC_[0-9][0-9.]*' "$bin" | sort -uV | tail -1 || true)"
  GLIBC_METHOD="grep -ao 'GLIBC_[0-9][0-9.]*' \"\$bin\" | sort -uV | tail -1"
  if [[ -z "$symbol" ]]; then
    printf 'error: could not read a GLIBC symbol from the candidate binary\n' >&2
    return 1
  fi
  GLIBC_MAX_SYMBOL="$symbol"
}

cmp_snapshot() {
  local label="$1"
  local before="$2"
  local after="$3"
  if cmp -s "$before" "$after"; then
    return 0
  fi
  printf 'error: %s changed\n' "$label" >&2
  diff -u "$before" "$after" >&2 || true
  return 1
}

require_integrity_ok() {
  local db="$1"
  local integrity=""
  integrity="$(sqlite_scalar "$db" "PRAGMA integrity_check;")"
  integrity="${integrity//$'\r'/}"
  if [[ "$integrity" != "ok" ]]; then
    printf 'error: PRAGMA integrity_check returned [%s]\n' "$integrity" >&2
    return 1
  fi
}

upgrade_impl() {
  if [[ ! -x "${PREFIX}/bin/iroh-rooms" ]]; then
    printf 'error: prior binary is not installed\n' >&2
    return 1
  fi
  mv "${PREFIX}/bin/iroh-rooms" "${PREFIX}/bin/iroh-rooms.prev" || return 1
  if [[ -n "$CANDIDATE_BINARY" ]]; then
    install_binary "$CANDIDATE_BINARY" "${PREFIX}/bin/iroh-rooms" || return 1
    CANDIDATE_VERSION_ACTUAL="$("${PREFIX}/bin/iroh-rooms" --version | tr -d '\r')" || return 1
    CANDIDATE_VERSION_MATCH="n/a"
    CANDIDATE_COMMIT_MATCH="n/a"
  else
    if [[ -z "$CANDIDATE_ASSET_DIR" || -z "$CANDIDATE_ARCHIVE" || -z "$CANDIDATE_PEELED" ]]; then
      printf 'error: candidate archive was not verified; refusing to install\n' >&2
      return 1
    fi
    if [[ "$CANDIDATE_VERIFICATION" != "full" && "$CANDIDATE_VERIFICATION" != "checksum-only" ]]; then
      printf 'error: candidate verification did not pass; refusing to install\n' >&2
      return 1
    fi
    install_from_archive "candidate" "$CANDIDATE_ASSET_DIR" "$CANDIDATE_ARCHIVE" \
      "$CANDIDATE_PEELED" "${PREFIX}/bin/iroh-rooms" || return 1
  fi
  measure_glibc "${PREFIX}/bin/iroh-rooms" || return 1
  capture_reads "${WORK}/snap-post" || return 1
  cmp_snapshot "identity show" "${WORK}/snap-pre/identity.show" "${WORK}/snap-post/identity.show" || return 1
  cmp_snapshot "room members" "${WORK}/snap-pre/members" "${WORK}/snap-post/members" || return 1
  cmp_snapshot "room tail --offline" "${WORK}/snap-pre/tail" "${WORK}/snap-post/tail" || return 1
  require_integrity_ok "${IROH_ROOMS_HOME}/rooms.db" || return 1
  USER_VERSION_AFTER="$(sqlite_scalar "${IROH_ROOMS_HOME}/rooms.db" "PRAGMA user_version;")"
  USER_VERSION_AFTER="${USER_VERSION_AFTER//$'\r'/}"
  if [[ ! "$USER_VERSION_AFTER" =~ ^[0-9]+$ ]]; then
    printf 'error: could not read user_version after upgrade\n' >&2
    return 1
  fi
}

write_after_upgrade_impl() {
  run_rooms "${WORK}/send-after.out" "${WORK}/send-after.err" \
    room send "$ROOM_ID" "lifecycle message after upgrade" || return 1
  run_rooms "${WORK}/tail-after.out" "${WORK}/tail-after.err" \
    room tail --offline "$ROOM_ID" || return 1
  if ! grep -F -q "lifecycle message after upgrade" "${WORK}/tail-after.out"; then
    printf 'error: the post-upgrade message was not stored\n' >&2
    return 1
  fi
}

rollback_a_impl() {
  local actual=""
  if [[ ! -x "${PREFIX}/bin/iroh-rooms.prev" ]]; then
    printf 'error: iroh-rooms.prev is missing\n' >&2
    return 1
  fi
  install_binary "${PREFIX}/bin/iroh-rooms.prev" "${PREFIX}/bin/iroh-rooms" || return 1
  actual="$("${PREFIX}/bin/iroh-rooms" --version | tr -d '\r')" || return 1
  if [[ "$actual" != "$PRIOR_VERSION_ACTUAL" ]]; then
    printf 'error: rollback (a) --version [%s] != prior [%s]\n' "$actual" "$PRIOR_VERSION_ACTUAL" >&2
    return 1
  fi
  capture_reads "${WORK}/snap-rollback-a" || return 1
  cmp_snapshot "identity show" "${WORK}/snap-pre/identity.show" "${WORK}/snap-rollback-a/identity.show" || return 1
  cmp_snapshot "room members" "${WORK}/snap-pre/members" "${WORK}/snap-rollback-a/members" || return 1
  if ! grep -F -q "lifecycle message after upgrade" "${WORK}/snap-rollback-a/tail"; then
    printf 'error: rollback (a) restored data; the post-upgrade message is gone\n' >&2
    return 1
  fi
  require_integrity_ok "${IROH_ROOMS_HOME}/rooms.db" || return 1
}

# docs/operations/install-uninstall.md:300-315
# Reinstall the prior binary, move the data directory aside, and untar the
# pre-upgrade backup. The binary install uses a temp name and mv.
restore_documented() {
  local previous="${PREFIX}/bin/iroh-rooms.prev"
  # docs/operations/install-uninstall.md:300-315
  # Stop all iroh-rooms commands first.
  rm -f "${HOME}/.local/bin/iroh-rooms" || return 1
  install_binary "$previous" "${HOME}/.local/bin/iroh-rooms" || return 1
  mv "$IROH_ROOMS_HOME" "${IROH_ROOMS_HOME}.before-rollback" || return 1
  tar -xzf "$BACKUP_ARCHIVE" -C "$(dirname "$IROH_ROOMS_HOME")" || return 1
  run_rooms "${WORK}/restored-version.out" "${WORK}/restored-version.err" --version || return 1
  run_rooms "${WORK}/restored-identity.out" "${WORK}/restored-identity.err" identity show || return 1
}

rollback_b_impl() {
  local db="${IROH_ROOMS_HOME}/rooms.db"
  local current="" next="" mode="" hash_before="" hash_after="" code=0
  if [[ ! -w "$IROH_ROOMS_HOME" || ! -w "$db" ]]; then
    printf 'error: data dir is not writable for the simulated rollback\n' >&2
    return 1
  fi
  mode="$(sqlite_scalar "$db" "PRAGMA journal_mode;")"
  mode="$(printf '%s' "$mode" | tr '[:upper:]' '[:lower:]')"
  mode="${mode//$'\r'/}"
  if [[ "$mode" != "wal" ]]; then
    printf 'error: simulated rollback expects a WAL store, journal_mode=%s\n' "$mode" >&2
    return 1
  fi
  current="$(sqlite_scalar "$db" "PRAGMA user_version;")"
  current="${current//$'\r'/}"
  if [[ ! "$current" =~ ^[0-9]+$ ]]; then
    printf 'error: could not read user_version before the stamp\n' >&2
    return 1
  fi
  next=$((current + 1))
  sqlite_exec "$db" "PRAGMA user_version = ${next}; PRAGMA wal_checkpoint(TRUNCATE);" || return $?
  if [[ -e "${db}-wal" ]]; then
    printf 'error: rooms.db-wal still exists after the simulated stamp\n' >&2
    return 1
  fi
  SIMULATED="store stamped USER_VERSION+1"
  hash_before="$(sha256_file "$db")" || return 1
  code=0
  "${PREFIX}/bin/iroh-rooms" room tail --offline "$ROOM_ID" \
    >"${WORK}/rollback-b.stdout" 2>"${WORK}/rollback-b.stderr" || code=$?
  if [[ "$code" -ne 1 ]]; then
    printf 'error: prior binary room tail --offline exited %s, expected 1\n' "$code" >&2
    cat "${WORK}/rollback-b.stderr" >&2
    return 1
  fi
  if ! grep -E -q 'store migration failed: database user_version [0-9]+ is newer than supported [0-9]+' \
    "${WORK}/rollback-b.stderr"; then
    printf 'error: stderr did not match the store migration refusal\n' >&2
    cat "${WORK}/rollback-b.stderr" >&2
    return 1
  fi
  hash_after="$(sha256_file "$db")" || return 1
  if [[ "$hash_before" != "$hash_after" ]]; then
    printf 'error: rooms.db sha256 changed across the refused open\n' >&2
    return 1
  fi
  restore_documented || return 1
  capture_reads "${WORK}/snap-restored" || return 1
  cmp_snapshot "identity show" "${WORK}/snap-pre/identity.show" "${WORK}/snap-restored/identity.show" || return 1
  cmp_snapshot "room members" "${WORK}/snap-pre/members" "${WORK}/snap-restored/members" || return 1
  cmp_snapshot "room tail --offline" "${WORK}/snap-pre/tail" "${WORK}/snap-restored/tail" || return 1
  require_integrity_ok "${IROH_ROOMS_HOME}/rooms.db" || return 1
  if grep -F -q "lifecycle message after upgrade" "${WORK}/snap-restored/tail"; then
    printf 'error: restored tail still contains the post-upgrade message\n' >&2
    return 1
  fi
  rm -rf "${IROH_ROOMS_HOME}.before-rollback"
}

uninstall_impl() {
  local db="${IROH_ROOMS_HOME}/rooms.db"
  local before="" after="" found=""
  before="$(sha256_file "$db")" || return 1
  rm -f "${PREFIX}/bin/iroh-rooms" || return 1
  hash -r
  found="$(command -v iroh-rooms || true)"
  if [[ -n "$found" ]]; then
    printf 'error: command -v iroh-rooms still resolves to %s after uninstall\n' "$found" >&2
    return 1
  fi
  if [[ ! -d "$IROH_ROOMS_HOME" || ! -f "${IROH_ROOMS_HOME}/identity.json" || ! -f "$db" ]]; then
    printf 'error: uninstall removed or damaged the data directory\n' >&2
    return 1
  fi
  after="$(sha256_file "$db")" || return 1
  if [[ "$before" != "$after" ]]; then
    printf 'error: rooms.db changed during uninstall\n' >&2
    return 1
  fi
}

remove_work() {
  if [[ -z "${WORK:-}" || ! -d "$WORK" ]]; then
    return 0
  fi
  case "$WORK" in
    *"/lifecycle."*) ;;
    *)
      printf 'error: refusing to remove unexpected work dir\n' >&2
      return 1
      ;;
  esac
  if [[ ! -f "${WORK}/.lifecycle-smoke-work" ]]; then
    printf 'error: refusing to remove a work dir without its marker\n' >&2
    return 1
  fi
  rm -rf "$WORK"
}

write_record() {
  local json_path="" md_path=""
  if [[ "$LIFECYCLE_STARTED" -ne 1 || -z "$OUT_DIR" ]]; then
    return 0
  fi
  mkdir -p "$OUT_DIR"
  json_path="${OUT_DIR}/lifecycle-${TRIPLE}.json"
  md_path="${OUT_DIR}/lifecycle-${TRIPLE}.md"
  if [[ ! -f "$STEPS_JSONL" ]]; then
    : >"$STEPS_JSONL"
  fi
  if [[ ! -f "$ARCHIVES_JSONL" ]]; then
    : >"$ARCHIVES_JSONL"
  fi
  LS_PRIOR_TAG="$PRIOR_TAG" \
  LS_CANDIDATE_TAG="$CANDIDATE_TAG" \
  LS_TRIPLE="$TRIPLE" \
  LS_CLASS="$(class_of "$CANDIDATE_BINARY")" \
  LS_CLASS_RULE="$CLASS_RULE" \
  LS_RUNNER_HOSTED="$RUNNER_HOSTED" \
  LS_GH_VERSION="$GH_VERSION" \
  LS_COSIGN_VERSION="$COSIGN_VERSION" \
  LS_PRIOR_VERIFICATION="$PRIOR_VERIFICATION" \
  LS_CANDIDATE_VERIFICATION="$CANDIDATE_VERIFICATION" \
  LS_GLIBC_MAX_SYMBOL="$GLIBC_MAX_SYMBOL" \
  LS_GLIBC_METHOD="$GLIBC_METHOD" \
  LS_LDD_VERSION="$LDD_VERSION" \
  LS_USER_VERSION_BEFORE="$USER_VERSION_BEFORE" \
  LS_USER_VERSION_AFTER="$USER_VERSION_AFTER" \
  LS_SIMULATED="$SIMULATED" \
  LS_DATA_DIRS="$DATA_DIRS" \
  LS_RESULT="$RESULT" \
  LS_PRIOR_MANIFEST_COMMIT="$PRIOR_MANIFEST_COMMIT" \
  LS_PRIOR_ARCHIVE_COMMIT12="$PRIOR_ARCHIVE_COMMIT12" \
  LS_PRIOR_PEELED="$PRIOR_PEELED" \
  LS_PRIOR_COMMIT_MATCH="$PRIOR_COMMIT_MATCH" \
  LS_PRIOR_VERSION_EXPECTED="$PRIOR_VERSION_EXPECTED" \
  LS_PRIOR_VERSION_ACTUAL="$PRIOR_VERSION_ACTUAL" \
  LS_PRIOR_VERSION_MATCH="$PRIOR_VERSION_MATCH" \
  LS_CANDIDATE_MANIFEST_COMMIT="$CANDIDATE_MANIFEST_COMMIT" \
  LS_CANDIDATE_ARCHIVE_COMMIT12="$CANDIDATE_ARCHIVE_COMMIT12" \
  LS_CANDIDATE_PEELED="$CANDIDATE_PEELED" \
  LS_CANDIDATE_COMMIT_MATCH="$CANDIDATE_COMMIT_MATCH" \
  LS_CANDIDATE_VERSION_EXPECTED="$CANDIDATE_VERSION_EXPECTED" \
  LS_CANDIDATE_VERSION_ACTUAL="$CANDIDATE_VERSION_ACTUAL" \
  LS_CANDIDATE_VERSION_MATCH="$CANDIDATE_VERSION_MATCH" \
  python3 - "$json_path" "$md_path" "$STEPS_JSONL" "$ARCHIVES_JSONL" <<'PY'
import json
import os
import sys

json_path, md_path, steps_path, archives_path = sys.argv[1:5]


def env(name):
    return os.environ.get(name, "")


def load_lines(path):
    rows = []
    if not path or not os.path.isfile(path):
        return rows
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def maybe_int(value):
    if value.isdigit():
        return int(value)
    return None


def triple(prefix):
    match = env("LS_%s_COMMIT_MATCH" % prefix)
    if match == "":
        return None
    if match == "n/a":
        return {"match": "n/a", "reason": "unreleased local binary"}
    peeled = env("LS_%s_PEELED" % prefix)
    return {
        "manifest_commit": env("LS_%s_MANIFEST_COMMIT" % prefix),
        "archive_commit12": env("LS_%s_ARCHIVE_COMMIT12" % prefix),
        "tag_peeled_commit": peeled,
        "tag_peeled_commit12": peeled[:12],
        "match": match == "true",
    }


def version(prefix):
    match = env("LS_%s_VERSION_MATCH" % prefix)
    if match == "":
        return None
    if match == "n/a":
        return {
            "actual": env("LS_%s_VERSION_ACTUAL" % prefix),
            "match": "n/a",
            "reason": "unreleased local binary",
        }
    return {
        "expected": env("LS_%s_VERSION_EXPECTED" % prefix),
        "actual": env("LS_%s_VERSION_ACTUAL" % prefix),
        "match": match == "true",
    }


data = {
    "schema_version": 1,
    "prior_tag": env("LS_PRIOR_TAG"),
    "candidate_tag": env("LS_CANDIDATE_TAG"),
    "triple": env("LS_TRIPLE"),
    "runner_hosted": env("LS_RUNNER_HOSTED"),
    "gh_version": env("LS_GH_VERSION"),
    "cosign_version": env("LS_COSIGN_VERSION"),
    "prior_verification": env("LS_PRIOR_VERIFICATION") or None,
    "candidate_verification": env("LS_CANDIDATE_VERIFICATION") or None,
    "archives": load_lines(archives_path),
    "commit_triple_match": {},
    "version_match": {},
    "glibc_max_symbol": env("LS_GLIBC_MAX_SYMBOL") or None,
    "glibc_method": env("LS_GLIBC_METHOD") or None,
    "ldd_version": env("LS_LDD_VERSION") or None,
    "class": env("LS_CLASS"),
    "class_rule": env("LS_CLASS_RULE"),
    "steps": load_lines(steps_path),
    "result": env("LS_RESULT") or "FAIL (unexpected)",
}

for src, dst in (
    ("GITHUB_RUN_ID", "github_run_id"),
    ("GITHUB_RUN_ATTEMPT", "github_run_attempt"),
    ("GITHUB_SHA", "github_sha"),
    ("GITHUB_REF", "github_ref"),
    ("ImageOS", "image_os"),
    ("ImageVersion", "image_version"),
):
    value = os.environ.get(src, "")
    if value:
        data[dst] = value

for label, prefix in (("prior", "PRIOR"), ("candidate", "CANDIDATE")):
    block = triple(prefix)
    if block is not None:
        data["commit_triple_match"][label] = block
    block = version(prefix)
    if block is not None:
        data["version_match"][label] = block

before = maybe_int(env("LS_USER_VERSION_BEFORE"))
after = maybe_int(env("LS_USER_VERSION_AFTER"))
if before is not None:
    data["user_version_before"] = before
if after is not None:
    data["user_version_after"] = after
simulated = env("LS_SIMULATED")
if simulated:
    data["simulated"] = simulated
dirs = [part for part in env("LS_DATA_DIRS").split(",") if part]
if dirs:
    data["data_dirs_covered"] = dirs

reject_pass = False
if data["result"] == "PASS":
    steps = data["steps"]
    complete = len(steps) == 10 and all(step.get("exit_code") == 0 for step in steps)
    if not complete:
        data["result"] = "FAIL (record)"
        reject_pass = True

with open(json_path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2)
    handle.write("\n")

with open(json_path, encoding="utf-8") as handle:
    rendered = json.load(handle)


def cell(value):
    text = "" if value is None else str(value)
    return text.replace("|", "\\|").replace("\n", " ")


def attempt_cell(attempts):
    parts = []
    for item in attempts or []:
        parts.append("%s:%s" % (item.get("n"), item.get("exit_code")))
    return " ".join(parts)


lines = []
lines.append("# Lifecycle smoke %s" % rendered.get("triple", ""))
lines.append("")
lines.append("- prior: `%s`" % rendered.get("prior_tag", ""))
lines.append("- candidate: `%s`" % rendered.get("candidate_tag", ""))
lines.append("- class: `%s`" % rendered.get("class", ""))
lines.append("- schema_version: `%s`" % rendered.get("schema_version", ""))
lines.append("")
lines.append(rendered.get("class_rule", ""))
lines.append("")
lines.append("## Runner")
lines.append("")
lines.append("- runner_hosted: `%s`" % rendered.get("runner_hosted", ""))
for key in ("github_run_id", "github_run_attempt", "github_sha", "github_ref", "image_os", "image_version"):
    if key in rendered:
        lines.append("- %s: `%s`" % (key, rendered[key]))
lines.append("- gh_version: `%s`" % rendered.get("gh_version", ""))
lines.append("- cosign_version: `%s`" % rendered.get("cosign_version", ""))
lines.append("")
lines.append("## Verification")
lines.append("")
lines.append("- prior_verification: `%s`" % rendered.get("prior_verification"))
lines.append("- candidate_verification: `%s`" % rendered.get("candidate_verification"))
lines.append("")
lines.append("## Archives")
lines.append("")
lines.append("| role | tag | name | sha256 | verification |")
lines.append("| --- | --- | --- | --- | --- |")
for archive in rendered.get("archives") or []:
    lines.append("| %s | %s | %s | `%s` | %s |" % (
        cell(archive.get("role")),
        cell(archive.get("tag")),
        cell(archive.get("name")),
        cell(archive.get("sha256")),
        cell(archive.get("verification")),
    ))
lines.append("")
lines.append("## Commit triple match")
lines.append("")
lines.append("```json")
lines.append(json.dumps(rendered.get("commit_triple_match") or {}, indent=2))
lines.append("```")
lines.append("")
lines.append("## Version match")
lines.append("")
lines.append("```json")
lines.append(json.dumps(rendered.get("version_match") or {}, indent=2))
lines.append("```")
lines.append("")
lines.append("## glibc")
lines.append("")
lines.append("- glibc_max_symbol: `%s`" % rendered.get("glibc_max_symbol"))
lines.append("- glibc_method: `%s`" % rendered.get("glibc_method"))
lines.append("- ldd_version: `%s`" % rendered.get("ldd_version"))
lines.append("")
lines.append("## Store")
lines.append("")
if "user_version_before" in rendered:
    lines.append("- user_version_before: `%s`" % rendered["user_version_before"])
if "user_version_after" in rendered:
    lines.append("- user_version_after: `%s`" % rendered["user_version_after"])
if "simulated" in rendered:
    lines.append("- simulated: `%s`" % rendered["simulated"])
if rendered.get("data_dirs_covered"):
    lines.append("- data_dirs_covered: `%s`" % ", ".join(rendered["data_dirs_covered"]))
lines.append("")
lines.append("## Steps")
lines.append("")
lines.append("| step | exit | started ET | started UTC | ended ET | ended UTC | attempts |")
lines.append("| --- | --- | --- | --- | --- | --- | --- |")
for step in rendered.get("steps") or []:
    lines.append("| %s | %s | %s | %s | %s | %s | %s |" % (
        cell(step.get("name")),
        cell(step.get("exit_code")),
        cell(step.get("started_et")),
        cell(step.get("started_utc")),
        cell(step.get("ended_et")),
        cell(step.get("ended_utc")),
        cell(attempt_cell(step.get("attempts"))),
    ))
lines.append("")
lines.append(rendered.get("result", ""))
lines.append("")
markdown = "\n".join(lines)

blob = open(json_path, encoding="utf-8").read() + markdown
for needle in ("identity_secret", "device_secret", "roomtkt1"):
    if needle in blob:
        os.remove(json_path)
        sys.stderr.write("refusing to write a record that contains secret material\n")
        sys.exit(1)

with open(md_path, "w", encoding="utf-8") as handle:
    handle.write(markdown)
if reject_pass:
    sys.stderr.write("refusing PASS without exactly 10 successful steps\n")
    sys.exit(1)
PY
}

on_exit() {
  local code=$?
  trap - EXIT
  # Only main sets PASS. A signal or any other exit leaves a failure record.
  if [[ -z "${RESULT:-}" ]]; then
    case "$code" in
      129 | 130 | 143)
        RESULT="FAIL (interrupted)"
        ;;
      *)
        if [[ -n "${FAILED_STEP:-}" ]]; then
          RESULT="FAIL (${FAILED_STEP})"
        else
          RESULT="FAIL (unexpected)"
        fi
        ;;
    esac
  fi
  if ! write_record; then
    if [[ "$code" -eq 0 ]]; then
      code=1
    fi
  fi
  remove_work || true
  exit "$code"
}

prepare_home() {
  local tmp_root=""
  REAL_HOME="${HOME}"
  if [[ -z "${GH_CONFIG_DIR:-}" && -d "${REAL_HOME}/.config/gh" ]]; then
    export GH_CONFIG_DIR="${REAL_HOME}/.config/gh"
  fi
  tmp_root="${TMPDIR:-/tmp}"
  tmp_root="$(physical_dir "$tmp_root")"
  WORK="$(mktemp -d "${tmp_root}/lifecycle.XXXXXX")"
  WORK="$(physical_dir "$WORK")"
  touch "${WORK}/.lifecycle-smoke-work"
  export HOME="$WORK"
  case "$(uname -s)" in
    Linux)
      export XDG_DATA_HOME="${HOME}/.local/share"
      mkdir -p "$XDG_DATA_HOME"
      ;;
  esac
  PREFIX="${HOME}/.local"
  mkdir -p "${PREFIX}/bin"
  export PATH="${PREFIX}/bin:${PATH}"
  unset IROH_ROOMS_HOME
  unset IROH_ROOMS_DOWNLOADS || true
  STEPS_JSONL="${WORK}/steps.jsonl"
  ARCHIVES_JSONL="${WORK}/archives.jsonl"
  : >"$STEPS_JSONL"
  : >"$ARCHIVES_JSONL"
}

tool_versions() {
  if command -v gh >/dev/null 2>&1; then
    GH_VERSION="$(gh version 2>/dev/null | head -n 1 || true)"
  else
    GH_VERSION="not installed"
  fi
  if command -v cosign >/dev/null 2>&1; then
    COSIGN_VERSION="$(cosign version 2>/dev/null | awk '/GitVersion/ { print $2; exit }' || true)"
    if [[ -z "$COSIGN_VERSION" ]]; then
      COSIGN_VERSION="$(cosign version 2>&1 | head -n 1 || true)"
    fi
  else
    COSIGN_VERSION="not installed"
  fi
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    RUNNER_HOSTED="github"
  else
    RUNNER_HOSTED="local"
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --candidate-binary)
        if [[ $# -lt 2 ]]; then
          die 2 "--candidate-binary requires a path"
        fi
        CANDIDATE_BINARY="$2"
        shift 2
        ;;
      --out)
        if [[ $# -lt 2 ]]; then
          die 2 "--out requires a directory"
        fi
        OUT_DIR="$2"
        shift 2
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      --)
        shift
        break
        ;;
      -*)
        die 2 "unknown option: $1"
        ;;
      *)
        if [[ -z "$PRIOR_TAG" ]]; then
          PRIOR_TAG="$1"
        elif [[ -z "$CANDIDATE_TAG" ]]; then
          CANDIDATE_TAG="$1"
        elif [[ -z "$TRIPLE" ]]; then
          TRIPLE="$1"
        else
          die 2 "unexpected argument: $1"
        fi
        shift
        ;;
    esac
  done
  while [[ $# -gt 0 ]]; do
    if [[ -z "$PRIOR_TAG" ]]; then
      PRIOR_TAG="$1"
    elif [[ -z "$CANDIDATE_TAG" ]]; then
      CANDIDATE_TAG="$1"
    elif [[ -z "$TRIPLE" ]]; then
      TRIPLE="$1"
    else
      die 2 "unexpected argument: $1"
    fi
    shift
  done
}

main() {
  local candidate_mode="retry"
  parse_args "$@"
  if [[ -n "$CANDIDATE_BINARY" && "${GITHUB_ACTIONS:-}" == "true" ]]; then
    printf 'error: refusing --candidate-binary because GITHUB_ACTIONS=true. --candidate-binary is local dev mode only (class: unreleased) and must not run on GitHub Actions.\n' >&2
    exit 2
  fi
  if [[ -z "$PRIOR_TAG" || -z "$CANDIDATE_TAG" || -z "$TRIPLE" ]]; then
    usage >&2
    die 2 "expected <prior-tag> <candidate-tag> <triple>"
  fi
  if ! tag_ok "$PRIOR_TAG"; then
    die 2 "prior tag must match ^v[0-9]+.[0-9]+.[0-9]+(-rc.[0-9]+)?$ (got ${PRIOR_TAG})"
  fi
  if ! tag_ok "$CANDIDATE_TAG"; then
    die 2 "candidate tag must match ^v[0-9]+.[0-9]+.[0-9]+(-rc.[0-9]+)?$ (got ${CANDIDATE_TAG})"
  fi
  if [[ "$PRIOR_TAG" == "$CANDIDATE_TAG" ]]; then
    die 2 "prior tag and candidate tag must differ"
  fi
  case "$TRIPLE" in
    x86_64-apple-darwin | aarch64-apple-darwin | x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
      ;;
    *)
      die 2 "triple must be one of x86_64-apple-darwin, aarch64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu"
      ;;
  esac
  if ! command -v python3 >/dev/null 2>&1; then
    die 3 "python3 is required to render the lifecycle record"
  fi
  if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR="$(pwd)"
  fi
  mkdir -p "$OUT_DIR"
  OUT_DIR="$(physical_dir "$OUT_DIR")"
  prepare_home
  tool_versions
  LIFECYCLE_STARTED=1
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap on_exit EXIT
  if [[ -n "$CANDIDATE_BINARY" ]]; then
    candidate_mode="once"
  fi
  run_step verify_candidate "$candidate_mode" verify_candidate_impl
  run_step verify_prior retry verify_prior_impl
  run_step install_prior once install_prior_impl
  run_step create_data once create_data_impl
  run_step backup once backup_impl
  run_step upgrade once upgrade_impl
  run_step write_after_upgrade once write_after_upgrade_impl
  run_step rollback_a once rollback_a_impl
  run_step rollback_b once rollback_b_impl
  run_step uninstall once uninstall_impl
  if [[ -n "$FAILED_STEP" ]]; then
    exit 1
  fi
  RESULT="PASS"
  exit 0
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
