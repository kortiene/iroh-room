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

tmp="$(mktemp -d "${TMPDIR:-/tmp}/lifecycle-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

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
"$SCRIPT" v0.1.0-rc.6 v0.1.0-rc.6 x86_64-unknown-linux-gnu >"${tmp}/same.out" 2>"${tmp}/same.err" || same_code=$?
if [[ "$same_code" -eq 0 ]]; then
  fail "identical tags were accepted"
fi
if ! grep -F -q 'must differ' "${tmp}/same.err"; then
  fail "identical-tag error missing"
fi
ok "rejects identical tags (exit ${same_code})"

bad_code=0
"$SCRIPT" not-a-tag v0.1.0-rc.6 x86_64-unknown-linux-gnu >/dev/null 2>"${tmp}/bad.err" || bad_code=$?
if [[ "$bad_code" -eq 0 ]]; then
  fail "bad tag was accepted"
fi
ok "rejects a bad tag (exit ${bad_code})"

# shellcheck disable=SC1090,SC1091
source "$SCRIPT"

if [[ "$(class_of "")" != "local" ]]; then
  fail "plain local class"
fi
PROBE=1
if [[ "$(class_of "")" != "local" ]]; then
  fail "--probe raised a local run"
fi
PROBE=0
GITHUB_ACTIONS=true
GITHUB_REF=refs/heads/main
if [[ "$(class_of "")" != "probe" ]]; then
  fail "partial Actions context was not probe"
fi
GITHUB_EVENT_NAME=workflow_dispatch
RUNNER_ENVIRONMENT=github-hosted
GITHUB_REPOSITORY=kortiene/iroh-room
GITHUB_WORKFLOW_REF="kortiene/iroh-room/.github/workflows/lifecycle.yml@refs/heads/main"
if [[ "$(class_of "")" != "evidence" ]]; then
  fail "full dispatch context was not evidence"
fi
PROBE=1
if [[ "$(class_of "")" != "probe" ]]; then
  fail "--probe did not lower evidence"
fi
PROBE=0
if [[ "$(class_of "/tmp/local-binary")" != "unreleased" ]]; then
  fail "candidate binary was not unreleased"
fi
GITHUB_REF=refs/pull/1/merge
if [[ "$(class_of "")" != "probe" ]]; then
  fail "non-main Actions ref was not probe"
fi
unset GITHUB_ACTIONS
unset GITHUB_REF
unset GITHUB_EVENT_NAME
unset RUNNER_ENVIRONMENT
unset GITHUB_REPOSITORY
unset GITHUB_WORKFLOW_REF
PROBE=0
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
: >"$STEPS_JSONL"
step_name=""
for step_name in \
  verify_candidate verify_prior install_prior create_data backup \
  upgrade write_after_upgrade rollback_a rollback_b uninstall; do
  record_step_line "$step_name" 0 \
    "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" \
    "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z" "[]"
done
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
if ! grep -F -q 'local / probe / unreleased: developer or diagnostic run; not GA evidence for #265, #293 or #347' "$record"; then
  fail "class rule sentence missing from the record"
fi
grade="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["evidence_grade"])' "$json")"
if [[ "$grade" != "False" ]]; then
  fail "local evidence_grade is ${grade}"
fi
evidence_out="${tmp}/evidence"
mkdir -p "$evidence_out"
OUT_DIR="$evidence_out"
GITHUB_ACTIONS=true
GITHUB_REF=refs/heads/main
GITHUB_EVENT_NAME=workflow_dispatch
RUNNER_ENVIRONMENT=github-hosted
GITHUB_REPOSITORY=kortiene/iroh-room
GITHUB_WORKFLOW_REF="kortiene/iroh-room/.github/workflows/lifecycle.yml@refs/heads/main"
PROBE=0
CANDIDATE_BINARY=""
RESULT=PASS
write_record
evidence_pair="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["class"]); print(d["evidence_grade"]); print(d["github_workflow_ref"])' "${evidence_out}/lifecycle-${TRIPLE}.json")"
if [[ "$evidence_pair" != "$(printf 'evidence\nTrue\nkortiene/iroh-room/.github/workflows/lifecycle.yml@refs/heads/main')" ]]; then
  fail "evidence record was: ${evidence_pair}"
fi
unset GITHUB_ACTIONS GITHUB_REF GITHUB_EVENT_NAME RUNNER_ENVIRONMENT GITHUB_REPOSITORY GITHUB_WORKFLOW_REF
PROBE=0
OUT_DIR="${tmp}/out"
ok "record renders PASS from JSON"

SIMULATED="identity_secret"
if write_record; then
  fail "secret scan should refuse the record"
fi
if [[ -f "$json" ]]; then
  fail "secret scan left the JSON in place"
fi
ok "secret scan refuses a tainted record"

: >"$STEPS_JSONL"
record_step_line verify_candidate 0 \
  "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" \
  "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z" "[]"
record_step_line verify_prior 0 \
  "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" \
  "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z" "[]"
RESULT=PASS
SIMULATED="store stamped USER_VERSION+1"
partial_out="${tmp}/partial"
mkdir -p "$partial_out"
OUT_DIR="$partial_out"
if write_record; then
  fail "PASS with 2 steps was accepted"
fi
partial_json="${partial_out}/lifecycle-${TRIPLE}.json"
if [[ ! -f "$partial_json" ]]; then
  fail "rejected PASS did not leave a record"
fi
partial_result="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"])' "$partial_json")"
if [[ "$partial_result" == "PASS" ]]; then
  fail "incomplete record result is still PASS"
fi
if [[ "$partial_result" != "FAIL (record)" ]]; then
  fail "incomplete record result is ${partial_result}"
fi
ok "PASS requires exactly 10 successful steps"

if (
  # shellcheck disable=SC1090,SC1091
  source "$SCRIPT"
  set +e
  OUT_DIR="${tmp}/interrupted"
  mkdir -p "$OUT_DIR"
  TRIPLE=x86_64-unknown-linux-gnu
  PRIOR_TAG=v0.1.0-rc.5
  CANDIDATE_TAG=v0.1.0-rc.6
  LIFECYCLE_STARTED=1
  RESULT=""
  FAILED_STEP=""
  WORK=""
  STEPS_JSONL="${OUT_DIR}/steps.jsonl"
  ARCHIVES_JSONL="${OUT_DIR}/archives.jsonl"
  : >"$STEPS_JSONL"
  : >"$ARCHIVES_JSONL"
  record_step_line verify_candidate 0 \
    "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" \
    "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z" "[]"
  record_step_line verify_prior 0 \
    "2026-11-17T14:00:00-0500" "2026-11-17T19:00:00Z" \
    "2026-11-17T14:00:01-0500" "2026-11-17T19:00:01Z" "[]"
  (exit 143)
  on_exit
); then
  fail "interrupted exit was reported as success"
else
  int_code=$?
fi
if [[ "$int_code" -ne 143 ]]; then
  fail "interrupted exit code is ${int_code}, expected 143"
fi
int_json="${tmp}/interrupted/lifecycle-x86_64-unknown-linux-gnu.json"
int_result="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"])' "$int_json")"
if [[ "$int_result" == "PASS" ]]; then
  fail "SIGTERM-style exit recorded PASS"
fi
if [[ "$int_result" != "FAIL (interrupted)" ]]; then
  fail "interrupted result is ${int_result}"
fi
ok "on_exit records FAIL (interrupted) and exits 143"

if (
  # shellcheck disable=SC1090,SC1091
  source "$SCRIPT"
  set +e
  OUT_DIR="${tmp}/unexpected"
  mkdir -p "$OUT_DIR"
  TRIPLE=x86_64-unknown-linux-gnu
  PRIOR_TAG=v0.1.0-rc.5
  CANDIDATE_TAG=v0.1.0-rc.6
  LIFECYCLE_STARTED=1
  RESULT=""
  FAILED_STEP=""
  WORK=""
  STEPS_JSONL="${OUT_DIR}/steps.jsonl"
  ARCHIVES_JSONL="${OUT_DIR}/archives.jsonl"
  : >"$STEPS_JSONL"
  : >"$ARCHIVES_JSONL"
  true
  on_exit
); then
  unexpected_code=0
else
  unexpected_code=$?
fi
unexpected_result="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"])' "${tmp}/unexpected/lifecycle-x86_64-unknown-linux-gnu.json")"
if [[ "$unexpected_result" == "PASS" ]]; then
  fail "on_exit invented PASS from exit 0"
fi
if [[ "$unexpected_result" != "FAIL (unexpected)" ]]; then
  fail "empty RESULT with exit 0 recorded ${unexpected_result}"
fi
if [[ "$unexpected_code" -ne 0 ]]; then
  fail "successful record write changed exit ${unexpected_code}"
fi
ok "on_exit does not invent PASS"

if (
  # shellcheck disable=SC1090,SC1091
  source "$SCRIPT"
  set +e
  OUT_DIR="${tmp}/write-fail/nested"
  mkdir -p "${tmp}/write-fail"
  : >"${tmp}/write-fail-block"
  OUT_DIR="${tmp}/write-fail-block/out"
  TRIPLE=x86_64-unknown-linux-gnu
  PRIOR_TAG=v0.1.0-rc.5
  CANDIDATE_TAG=v0.1.0-rc.6
  LIFECYCLE_STARTED=1
  RESULT=PASS
  WORK=""
  STEPS_JSONL="${tmp}/write-fail-steps.jsonl"
  ARCHIVES_JSONL="${tmp}/write-fail-archives.jsonl"
  : >"$STEPS_JSONL"
  : >"$ARCHIVES_JSONL"
  true
  on_exit
); then
  fail "failed record write kept exit 0"
else
  write_fail_code=$?
fi
if [[ "$write_fail_code" -eq 0 ]]; then
  fail "failed record write exited 0"
fi
ok "failed record write turns exit 0 into ${write_fail_code}"

if ! grep -F -q "trap 'exit 130' INT" "$SCRIPT"; then
  fail "missing INT trap"
fi
if ! grep -F -q "trap 'exit 143' TERM" "$SCRIPT"; then
  fail "missing TERM trap"
fi
if ! grep -F -q "trap 'exit 129' HUP" "$SCRIPT"; then
  fail "missing HUP trap"
fi
ok "signal traps exit 130, 143, and 129"

sumfile="${tmp}/named.sha256"
printf 'abc  example.tar.gz\n' >"$sumfile"
assert_sha256_names_archive "$sumfile" "example.tar.gz" || fail "sha256 name match"
printf 'abc  other.tar.gz\n' >"$sumfile"
if assert_sha256_names_archive "$sumfile" "example.tar.gz"; then
  fail "sha256 name mismatch was accepted"
fi
ok "sha256 file names the archive"

USER_VERSION_BEFORE=2
USER_VERSION_AFTER=2
note_user_version || fail "equal user_version"
if [[ "$MIGRATED" != "false" ]]; then
  fail "equal versions recorded migrated=${MIGRATED}"
fi
USER_VERSION_AFTER=3
note_user_version || fail "newer user_version"
if [[ "$MIGRATED" != "true" ]]; then
  fail "newer version recorded migrated=${MIGRATED}"
fi
USER_VERSION_AFTER=1
if note_user_version; then
  fail "older user_version was accepted"
fi
ok "user_version migration flag"

saved_path="$PATH"
prefix="${tmp}/uninstall-prefix"
mkdir -p "${prefix}/bin" "${tmp}/other-bin"
printf '#!/bin/sh\n' >"${prefix}/bin/iroh-rooms"
printf '#!/bin/sh\n' >"${tmp}/other-bin/iroh-rooms"
chmod 755 "${prefix}/bin/iroh-rooms" "${tmp}/other-bin/iroh-rooms"
uhome="${tmp}/uninstall-home"
mkdir -p "$uhome"
printf '{}\n' >"${uhome}/identity.json"
printf 'db\n' >"${uhome}/rooms.db"
PREFIX="$prefix"
IROH_ROOMS_HOME="$uhome"
PATH="${prefix}/bin:${tmp}/other-bin:${saved_path}"
hash -r
uninstall_impl || fail "uninstall rejected another iroh-rooms on PATH"
if [[ -e "${prefix}/bin/iroh-rooms" ]]; then
  fail "temp binary still exists"
fi
found="$(command -v iroh-rooms || true)"
case "$found" in
  "${prefix}/"*) fail "command -v still under PREFIX (${found})" ;;
esac
PATH="$saved_path"
hash -r
ok "uninstall ignores an iroh-rooms outside PREFIX"

SIMULATED=secret_key
RESULT=PASS
OUT_DIR="${tmp}/secret-key"
mkdir -p "$OUT_DIR"
if write_record; then
  fail "secret_key was accepted"
fi
SIMULATED=seed
if write_record; then
  fail "seed was accepted"
fi
ok "secret scan rejects secret_key and seed"

refusal_dir="${tmp}/refusal-forms"
mkdir -p "$refusal_dir"
printf '%s\n' 'error: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2' \
  >"${refusal_dir}/rc.stderr"
accept_newer_store_refusal 1 "${refusal_dir}/rc.stderr" || fail "rc.1-rc.6 form was refused"
printf '%s\n' 'error: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  >"${refusal_dir}/hint.stderr"
accept_newer_store_refusal 1 "${refusal_dir}/hint.stderr" || fail "#409 form was refused"
printf '%s\n' \
  'error[store_version_unsupported]: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  'next: restore the pre-upgrade backup, or retry with the previous binary (see docs/compatibility.md#migration-failure-policy)' \
  >"${refusal_dir}/coded.stderr"
accept_newer_store_refusal 4 "${refusal_dir}/coded.stderr" || fail "coded form was refused"
printf '%s\n' \
  'error[store_version_unsupported]: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  >"${refusal_dir}/coded-exit1.stderr"
if accept_newer_store_refusal 1 "${refusal_dir}/coded-exit1.stderr"; then
  fail "exit 1 with the coded prefix was accepted"
fi
printf '%s\n' \
  'error[store_version_unsupported]: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  >"${refusal_dir}/no-next.stderr"
if accept_newer_store_refusal 4 "${refusal_dir}/no-next.stderr"; then
  fail "exit 4 without next: was accepted"
fi
printf '%s\n' 'error: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2' \
  >"${refusal_dir}/uncoded-exit4.stderr"
if accept_newer_store_refusal 4 "${refusal_dir}/uncoded-exit4.stderr"; then
  fail "exit 4 uncoded was accepted"
fi
# The full coded line plus next: is accepted only at exit 4. The same stderr
# at any other status must be rejected, including exit 1.
printf '%s\n' \
  'error[store_version_unsupported]: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  'next: restore the pre-upgrade backup, or retry with the previous binary (see docs/compatibility.md#migration-failure-policy)' \
  >"${refusal_dir}/coded-next.stderr"
if accept_newer_store_refusal 1 "${refusal_dir}/coded-next.stderr"; then
  fail "exit 1 with the coded line and next: was accepted"
fi
if accept_newer_store_refusal 0 "${refusal_dir}/coded-next.stderr"; then
  fail "exit 0 with the coded line and next: was accepted"
fi
if accept_newer_store_refusal 2 "${refusal_dir}/coded-next.stderr"; then
  fail "exit 2 with the coded line and next: was accepted"
fi
# Uncoded error: line plus a coded line. The uncoded grep matches, so only
# the coded-prefix exclusion can reject exit 1.
printf '%s\n' \
  'error: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2' \
  'error[store_version_unsupported]: could not open event store at /tmp/rooms.db: store migration failed: database user_version 3 is newer than supported 2; restore from backup or retry with the previous binary' \
  >"${refusal_dir}/uncoded-and-coded-exit1.stderr"
if accept_newer_store_refusal 1 "${refusal_dir}/uncoded-and-coded-exit1.stderr"; then
  fail "exit 1 with an uncoded error: line and a coded line was accepted"
fi
ok "newer-store refusal pairs"

printf 'ok lifecycle-smoke-test\n'
