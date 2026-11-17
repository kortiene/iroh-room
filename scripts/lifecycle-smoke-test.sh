#!/usr/bin/env bash
# Local checks for scripts/lifecycle-smoke.sh. Bash 3.2 compatible.
# Not part of scripts/verify.sh.
# SC2034: assignments below are inputs to functions defined in the sourced script.
# shellcheck disable=SC2034
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${ROOT}/scripts/lifecycle-smoke.sh"

fail() {
  printf 'not ok %s\n' "$*" >&2
  exit 1
}

ok() {
  printf 'ok %s\n' "$*"
}

if [[ ! -x "$SCRIPT" ]]; then
  fail "lifecycle-smoke.sh is not executable"
fi

refusal_out=""
refusal_code=0
refusal_out="$(GITHUB_ACTIONS=true "$SCRIPT" v0.1.0-rc.5 v0.1.0-rc.6 x86_64-unknown-linux-gnu --candidate-binary /no/such/binary 2>&1)" || refusal_code=$?
if [[ "$refusal_code" -eq 0 ]]; then
  fail "GITHUB_ACTIONS --candidate-binary was not refused"
fi
if ! printf '%s\n' "$refusal_out" | grep -F -q 'refusing --candidate-binary because GITHUB_ACTIONS=true'; then
  fail "refusal message was: ${refusal_out}"
fi
ok "refuses --candidate-binary on GitHub Actions (exit ${refusal_code})"

same_code=0
"$SCRIPT" v0.1.0-rc.6 v0.1.0-rc.6 x86_64-unknown-linux-gnu >/tmp/lifecycle-same.out 2>/tmp/lifecycle-same.err || same_code=$?
if [[ "$same_code" -eq 0 ]]; then
  fail "identical tags were accepted"
fi
if ! grep -F -q 'must differ' /tmp/lifecycle-same.err; then
  fail "identical-tag error missing"
fi
rm -f /tmp/lifecycle-same.out /tmp/lifecycle-same.err
ok "rejects identical tags (exit ${same_code})"

bad_code=0
"$SCRIPT" not-a-tag v0.1.0-rc.6 x86_64-unknown-linux-gnu >/dev/null 2>/tmp/lifecycle-bad.err || bad_code=$?
if [[ "$bad_code" -eq 0 ]]; then
  fail "bad tag was accepted"
fi
rm -f /tmp/lifecycle-bad.err
ok "rejects a bad tag (exit ${bad_code})"

# shellcheck disable=SC1090,SC1091
source "$SCRIPT"

if [[ "$(class_of "")" != "local" ]]; then
  fail "plain local class"
fi
# Read by class_of in the sourced script.
# shellcheck disable=SC2034
GITHUB_ACTIONS=true
# shellcheck disable=SC2034
GITHUB_REF=refs/heads/main
if [[ "$(class_of "")" != "evidence" ]]; then
  fail "evidence class"
fi
# shellcheck disable=SC2034
GITHUB_REF=refs/pull/1/merge
if [[ "$(class_of "")" != "probe" ]]; then
  fail "probe class"
fi
if [[ "$(class_of "/tmp/local-binary")" != "unreleased" ]]; then
  fail "unreleased class"
fi
unset GITHUB_ACTIONS
unset GITHUB_REF
ok "class rule"

tag_ok "v0.1.0-rc.5" || fail "rc tag"
tag_ok "v1.2.3" || fail "release tag"
if tag_ok "v0.1.0-beta.1"; then
  fail "beta tag should be rejected"
fi
if tag_ok "0.1.0"; then
  fail "untagged version should be rejected"
fi
is_checksum_only_tag "v0.1.0-rc.5" || fail "rc.5 checksum-only"
if is_checksum_only_tag "v0.1.0-rc.6"; then
  fail "rc.6 is not checksum-only"
fi
ok "tag and checksum-only rules"

if ! grep -F -q 'docs/operations/install-uninstall.md:252-260' "$SCRIPT"; then
  fail "backup citation missing"
fi
if ! grep -F -q 'docs/operations/install-uninstall.md:300-315' "$SCRIPT"; then
  fail "restore citation missing"
fi
# The needle is a literal snippet from the runbook, not a shell expansion.
# shellcheck disable=SC2016
if ! grep -F -q 'tar -C "$(dirname "$IROH_ROOMS_HOME")"' "$SCRIPT"; then
  fail "documented tar command missing"
fi
ok "runbook citations"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/lifecycle-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
data="${tmp}/home/.local/share/iroh-rooms-beta"
mkdir -p "$data"
printf 'marker\n' >"${data}/marker.txt"
HOME="${tmp}/home"
export HOME
export IROH_ROOMS_HOME="$data"
mkdir -p "${tmp}/cwd"
prev="$(pwd)"
cd "${tmp}/cwd"
documented_backup
if [[ ! -f "$BACKUP_ARCHIVE" ]]; then
  fail "backup path was not recorded"
fi
mode="$(file_mode "$BACKUP_ARCHIVE")"
if [[ "$mode" != "600" ]]; then
  fail "backup mode is ${mode}, expected 600"
fi
if tar -tzf "$BACKUP_ARCHIVE" | grep -E '(^|/)\._' >/dev/null; then
  fail "backup contains AppleDouble entries"
fi
if ! tar -tzf "$BACKUP_ARCHIVE" | grep -F -q 'marker.txt'; then
  fail "backup is missing the data file"
fi
ok "documented backup is mode 600"

: >"${IROH_ROOMS_HOME}/rooms.db-wal"
if documented_backup; then
  fail "backup should refuse a live WAL"
fi
rm -f "${IROH_ROOMS_HOME}/rooms.db-wal"
ok "backup refuses rooms.db-wal"
cd "$prev"

attempts="$(append_attempt "" 1 0 "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z")"
attempts="$(append_attempt "$attempts" 2 1 "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z")"
python3 -c 'import json,sys; json.loads(sys.argv[1])' "$attempts" || fail "attempt JSON"
ok "attempt JSON"

OUT_DIR="${tmp}/out"
WORK="${tmp}/work"
mkdir -p "$WORK" "$OUT_DIR"
touch "${WORK}/.lifecycle-smoke-work"
STEPS_JSONL="${WORK}/steps.jsonl"
ARCHIVES_JSONL="${WORK}/archives.jsonl"
: >"$STEPS_JSONL"
: >"$ARCHIVES_JSONL"
# The sourced writer reads these names. shellcheck cannot see that.
# shellcheck disable=SC2034
LIFECYCLE_STARTED=1
PRIOR_TAG=v0.1.0-rc.5
CANDIDATE_TAG=v0.1.0-rc.6
TRIPLE=x86_64-unknown-linux-gnu
CANDIDATE_BINARY=""
RESULT=PASS
RUNNER_HOSTED=local
GH_VERSION="gh version test"
COSIGN_VERSION="v3.1.4"
PRIOR_VERIFICATION=checksum-only
CANDIDATE_VERIFICATION=full
GLIBC_MAX_SYMBOL=GLIBC_2.34
GLIBC_METHOD="readelf -V"
LDD_VERSION="ldd test"
USER_VERSION_BEFORE=2
USER_VERSION_AFTER=2
SIMULATED="store stamped USER_VERSION+1"
DATA_DIRS="IROH_ROOMS_HOME,platform-default"
PRIOR_MANIFEST_COMMIT=dc6833cc4690
PRIOR_ARCHIVE_COMMIT12=dc6833cc4690
PRIOR_PEELED=dc6833cc46901234567890123456789012345678
PRIOR_COMMIT_MATCH=true
PRIOR_VERSION_EXPECTED="iroh-rooms 0.1.0-rc.5"
PRIOR_VERSION_ACTUAL="iroh-rooms 0.1.0-rc.5"
PRIOR_VERSION_MATCH=true
CANDIDATE_MANIFEST_COMMIT=bf353b78f63b
CANDIDATE_ARCHIVE_COMMIT12=bf353b78f63b
CANDIDATE_PEELED=bf353b78f63b0123456789012345678901234567
CANDIDATE_COMMIT_MATCH=true
CANDIDATE_VERSION_EXPECTED="iroh-rooms 0.1.0-rc.6"
CANDIDATE_VERSION_ACTUAL="iroh-rooms 0.1.0-rc.6"
CANDIDATE_VERSION_MATCH=true
record_archive prior "$PRIOR_TAG" "iroh-rooms-0.1.0-rc.5-${TRIPLE}-dc6833cc4690.tar.gz" "abc" checksum-only
record_step_line verify_prior 0 "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" "2026-11-17T14:00:02-0500" "2026-11-17T19:00:02Z" "$attempts"
write_record
record="${OUT_DIR}/lifecycle-${TRIPLE}.md"
json="${OUT_DIR}/lifecycle-${TRIPLE}.json"
if [[ ! -f "$record" || ! -f "$json" ]]; then
  fail "record files were not written"
fi
last="$(awk 'NF { line=$0 } END { print line }' "$record")"
if [[ "$last" != "PASS" ]]; then
  fail "record does not end in PASS (got ${last})"
fi
python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$json" || fail "record JSON"
if grep -E -q 'identity_secret|device_secret|roomtkt1' "$json" "$record"; then
  fail "record contains secret-shaped text"
fi
ok "record renders PASS from JSON"

SIMULATED="identity_secret"
if write_record; then
  fail "secret scan should refuse the record"
fi
if [[ -f "$json" ]]; then
  fail "secret scan left the JSON in place"
fi
ok "secret scan refuses a tainted record"

printf 'ok lifecycle-smoke-test\n'
