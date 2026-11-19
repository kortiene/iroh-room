#!/usr/bin/env bash
# Local checks for the release run pin in scripts/verify-release-artifacts.sh.
# Bash 3.2 compatible. No network. The gh and cosign commands on PATH are
# stubs: gh enforces a subject-digest match, and both stubs otherwise succeed
# so the script's own pin decoder is what accepts or rejects the bundle.
# shellcheck disable=SC2016
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${ROOT}/scripts/verify-release-artifacts.sh"

COMMIT="bf353b78f63b08ff5e3f425273aa3e0983167679"
SHORT="${COMMIT:0:12}"
TRIPLE="x86_64-unknown-linux-gnu"
GOOD_RUN="43835416665"
GOOD_ATTEMPT="1"
GOOD_URL="https://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/${GOOD_ATTEMPT}"
BAD_RUN_URL="https://github.com/kortiene/iroh-room/actions/runs/43791394134/attempts/1"

fail() {
  printf 'not ok %s\n' "$*" >&2
  exit 1
}

ok() {
  printf 'ok %s\n' "$*"
}

if [[ ! -x "$SRC" ]]; then
  fail "verify-release-artifacts.sh is not executable"
fi
if ! command -v python3 >/dev/null 2>&1; then
  fail "python3 is required"
fi

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/verify-release-artifacts-test.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT

make_bundle() {
  local dest="$1"
  local invocation="$2"
  local name="$3"
  local digest="$4"
  local mode="$5"
  python3 - "$dest" "$invocation" "$name" "$digest" "$mode" <<'PY'
import base64, json, sys
dest, invocation, name, digest, mode = sys.argv[1:6]
if mode == "empty":
    payload = ""
elif mode == "nonjson":
    payload = base64.b64encode(b"not-json").decode("ascii")
elif mode == "missing":
    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": name, "digest": {"sha256": digest}}],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {"runDetails": {"metadata": {}}},
    }
    payload = base64.b64encode(json.dumps(statement).encode("utf-8")).decode("ascii")
else:
    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": name, "digest": {"sha256": digest}}],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {"runDetails": {"metadata": {"invocationId": invocation}}},
    }
    payload = base64.b64encode(json.dumps(statement).encode("utf-8")).decode("ascii")
bundle = {
    "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
    "dsseEnvelope": {
        "payloadType": "application/vnd.in-toto+json",
        "payload": payload,
        "signatures": [],
    },
}
with open(dest, "w", encoding="utf-8") as handle:
    json.dump(bundle, handle)
PY
}

write_stubs() {
  local bin="$1"
  mkdir -p "$bin"
  cat >"${bin}/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "auth" && "${2:-}" == "status" ]]; then
  exit 0
fi

if [[ "${1:-}" == "attestation" && "${2:-}" == "verify" ]]; then
  help=0
  for arg in "$@"; do
    if [[ "$arg" == "--help" ]]; then
      help=1
    fi
  done
  if [[ "$help" -eq 1 ]]; then
    printf '%s\n' "--source-digest --deny-self-hosted-runners"
    exit 0
  fi
  if [[ "${FIXTURE_NO_ATTESTATIONS:-}" == "1" ]]; then
    printf '%s\n' "no attestations found" >&2
    exit 1
  fi
  subject="${3:-}"
  bundle=""
  prev=""
  for arg in "$@"; do
    if [[ "$prev" == "--bundle" ]]; then
      bundle="$arg"
    fi
    prev="$arg"
  done
  if [[ -n "$bundle" ]]; then
    src="$bundle"
  elif [[ "$(basename "$subject")" == "SHA256SUMS" ]]; then
    src="${FIXTURE_SUMS_BUNDLE:?}"
  else
    src="${FIXTURE_ONLINE_ARCHIVE_BUNDLE:?}"
  fi
  online=0
  if [[ -z "$bundle" ]]; then
    online=1
  fi
  python3 - "$subject" "$src" "$online" <<'PY'
import base64, copy, hashlib, json, os, sys
subject, bundle_path, online = sys.argv[1], sys.argv[2], sys.argv[3]
bundle = json.load(open(bundle_path, encoding="utf-8"))
digest = None
statement = None
try:
    payload = bundle["dsseEnvelope"]["payload"]
    if isinstance(payload, str) and payload != "":
        raw = base64.b64decode(payload, validate=True)
        statement = json.loads(raw.decode("utf-8"))
        digest = statement["subject"][0]["digest"]["sha256"]
except Exception:
    digest = None
if isinstance(digest, str):
    file_hash = hashlib.sha256(open(subject, "rb").read()).hexdigest()
    if digest != file_hash:
        sys.stderr.write("subject digest mismatch\n")
        sys.exit(1)
out = [{"attestation": {"bundle": bundle}, "verificationResult": {"statement": {}}}]
second = os.environ.get("FIXTURE_SECOND_INVOCATION", "")
if online == "1" and second and isinstance(statement, dict):
    other = copy.deepcopy(bundle)
    statement["predicate"]["runDetails"]["metadata"]["invocationId"] = second
    other["dsseEnvelope"]["payload"] = base64.b64encode(
        json.dumps(statement).encode("utf-8")
    ).decode("ascii")
    out.append({"attestation": {"bundle": other}, "verificationResult": {"statement": {}}})
json.dump(out, sys.stdout)
sys.stdout.write("\n")
PY
  exit 0
fi

if [[ "${1:-}" == "api" ]]; then
  jq_expr=""
  prev=""
  for arg in "$@"; do
    if [[ "$prev" == "--jq" ]]; then
      jq_expr="$arg"
    fi
    prev="$arg"
  done
  case "$jq_expr" in
    .object.type)
      printf '%s\n' commit
      ;;
    .object.sha)
      printf '%s\n' "${FIXTURE_COMMIT:?}"
      ;;
    *)
      printf 'unexpected jq %s\n' "$jq_expr" >&2
      exit 1
      ;;
  esac
  exit 0
fi

printf 'unexpected gh args: %s\n' "$*" >&2
exit 1
EOF
  cat >"${bin}/cosign" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "version" ]]; then
  printf '%s\n' "cosign version v3.1.4"
  exit 0
fi
if [[ "${1:-}" == "verify-blob" ]]; then
  exit 0
fi
printf 'unexpected cosign args: %s\n' "$*" >&2
exit 1
EOF
  chmod +x "${bin}/gh" "${bin}/cosign"
}

# Sets CASE_DIR. pin_text is the pin file body. archive_mode / sums_mode are
# ok, empty, nonjson, or missing. archive_digest_override replaces only the
# on-disk archive bundle's subject digest; the online archive bundle keeps
# the file's real digest so a digest failure happens at --bundle.
prepare_case() {
  local name="$1"
  local tag="$2"
  local pin_text="$3"
  local archive_invocation="$4"
  local sums_invocation="$5"
  local archive_mode="$6"
  local sums_mode="$7"
  local archive_digest_override="${8:-}"
  local no_attestations="${9:-0}"
  local version="${tag#v}"
  local archive="iroh-rooms-${version}-${TRIPLE}-${SHORT}.tar.gz"
  local case_dir="${tmp_root}/${name}"
  local assets="${case_dir}/assets"
  local fixtures="${case_dir}/fixtures"
  rm -rf "$case_dir"
  mkdir -p "$assets" "$fixtures" "${case_dir}/bin"
  cp "$SRC" "${case_dir}/verify-release-artifacts.sh"
  chmod +x "${case_dir}/verify-release-artifacts.sh"
  printf '%s\n' "$pin_text" >"${case_dir}/release-run-pins.txt"
  printf 'fixture-archive\n' >"${assets}/${archive}"
  python3 - "$assets" "$archive" "$fixtures" "$archive_invocation" "$sums_invocation" \
    "$archive_mode" "$sums_mode" "$archive_digest_override" <<'PY'
import hashlib, json, os, sys
assets, archive, fixtures, archive_invocation, sums_invocation, archive_mode, sums_mode, override = sys.argv[1:9]
archive_path = os.path.join(assets, archive)
archive_hash = hashlib.sha256(open(archive_path, "rb").read()).hexdigest()
line = "%s  %s\n" % (archive_hash, archive)
with open(archive_path + ".sha256", "w", encoding="utf-8") as handle:
    handle.write(line)
sums_path = os.path.join(assets, "SHA256SUMS")
with open(sums_path, "w", encoding="utf-8") as handle:
    handle.write(line)
sums_hash = hashlib.sha256(line.encode("utf-8")).hexdigest()
with open(os.path.join(assets, "SHA256SUMS.sigstore.json"), "w", encoding="utf-8") as handle:
    json.dump(
        {
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "messageSignature": {"signature": "AA=="},
        },
        handle,
    )
open(os.path.join(fixtures, "archive-hash"), "w", encoding="utf-8").write(archive_hash + "\n")
open(os.path.join(fixtures, "sums-hash"), "w", encoding="utf-8").write(sums_hash + "\n")
open(os.path.join(fixtures, "archive-name"), "w", encoding="utf-8").write(archive + "\n")
meta = {
    "archive_hash": archive_hash,
    "sums_hash": sums_hash,
    "archive": archive,
    "archive_invocation": archive_invocation,
    "sums_invocation": sums_invocation,
    "archive_mode": archive_mode,
    "sums_mode": sums_mode,
    "override": override,
}
json.dump(meta, open(os.path.join(fixtures, "meta.json"), "w", encoding="utf-8"))
PY
  local archive_hash sums_hash
  archive_hash="$(tr -d '\n' <"${fixtures}/archive-hash")"
  sums_hash="$(tr -d '\n' <"${fixtures}/sums-hash")"
  local archive_digest="$archive_hash"
  local online_mode="$archive_mode"
  local online_invocation="$archive_invocation"
  if [[ -n "$archive_digest_override" ]]; then
    archive_digest="$archive_digest_override"
    # The online attestation still names the downloaded file. Only the
    # on-disk bundle carries the other digest, so gh --bundle is the failure.
    online_mode="ok"
  fi
  make_bundle "${assets}/${archive}.sigstore.json" "$archive_invocation" "$archive" "$archive_digest" "$archive_mode"
  make_bundle "${fixtures}/online-archive.json" "$online_invocation" "$archive" "$archive_hash" "$online_mode"
  make_bundle "${fixtures}/sums.json" "$sums_invocation" "SHA256SUMS" "$sums_hash" "$sums_mode"
  write_stubs "${case_dir}/bin"
  CASE_DIR="$case_dir"
  CASE_NO_ATTESTATIONS="$no_attestations"
}

run_case() {
  local tag="$1"
  shift
  local out="${CASE_DIR}/out"
  local code=0
  set +e
  PATH="${CASE_DIR}/bin:${PATH}" \
    FIXTURE_COMMIT="$COMMIT" \
    FIXTURE_SUMS_BUNDLE="${CASE_DIR}/fixtures/sums.json" \
    FIXTURE_ONLINE_ARCHIVE_BUNDLE="${CASE_DIR}/fixtures/online-archive.json" \
    FIXTURE_NO_ATTESTATIONS="$CASE_NO_ATTESTATIONS" \
    "${CASE_DIR}/verify-release-artifacts.sh" \
    --dir "${CASE_DIR}/assets" \
    --commit "$COMMIT" \
    "$@" \
    "$tag" \
    "$TRIPLE" >"$out" 2>&1
  code=$?
  set -e
  CASE_CODE="$code"
  CASE_OUT="$(cat "$out")"
}

assert_fail() {
  local label="$1"
  local needle="$2"
  if [[ "$CASE_CODE" -eq 0 ]]; then
    fail "${label}: expected a non-zero exit, got 0. output: ${CASE_OUT}"
  fi
  if ! printf '%s\n' "$CASE_OUT" | grep -F -q -- "$needle"; then
    fail "${label}: missing [${needle}] in output: ${CASE_OUT}"
  fi
  ok "${label} (exit ${CASE_CODE})"
}

assert_absent() {
  local label="$1"
  local needle="$2"
  if printf '%s\n' "$CASE_OUT" | grep -F -q -- "$needle"; then
    fail "${label}: unexpectedly saw [${needle}] in output: ${CASE_OUT}"
  fi
}

assert_ok() {
  local label="$1"
  local needle="$2"
  if [[ "$CASE_CODE" -ne 0 ]]; then
    fail "${label}: expected exit 0, got ${CASE_CODE}. output: ${CASE_OUT}"
  fi
  if ! printf '%s\n' "$CASE_OUT" | grep -F -q -- "$needle"; then
    fail "${label}: missing [${needle}] in output: ${CASE_OUT}"
  fi
  ok "${label}"
}

PIN_LINE="v0.1.0-rc.6 ${GOOD_RUN} ${GOOD_ATTEMPT}"

prepare_case "match" "v0.1.0-rc.6" "$PIN_LINE" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6"
assert_ok "matching pin passes" "$GOOD_URL"
if ! printf '%s\n' "$CASE_OUT" | grep -F -q "run pin"; then
  fail "matching pin passes: output did not mention the run pin: ${CASE_OUT}"
fi

prepare_case "override" "v0.1.0-rc.6" "v0.1.0-rc.6 11111111111 1" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6" --expected-run "$GOOD_RUN" "$GOOD_ATTEMPT"
assert_fail "expected-run conflicting with a pinned tag is rejected" "does not equal the pin"
assert_absent "expected-run conflicting with a pinned tag is rejected" "run pin: operator-supplied"

prepare_case "flag-matches-pin" "v0.1.0-rc.6" "$PIN_LINE" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6" --expected-run "$GOOD_RUN" "$GOOD_ATTEMPT"
assert_ok "expected-run that equals the pin passes" "$GOOD_URL"
assert_absent "expected-run that equals the pin passes" "run pin: operator-supplied"

prepare_case "bad-run" "v0.1.0-rc.6" "$PIN_LINE" "$BAD_RUN_URL" "$BAD_RUN_URL" ok ok
run_case "v0.1.0-rc.6"
assert_fail "run 43791394134 is rejected" "43791394134"
assert_absent "run 43791394134 is rejected" "verified v0.1.0-rc.6"

prepare_case "bad-attempt" "v0.1.0-rc.6" "$PIN_LINE" \
  "https://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/2" \
  "https://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/2" ok ok
run_case "v0.1.0-rc.6"
assert_fail "right run, wrong attempt is rejected" "attempts/2"

prepare_case "other-repo" "v0.1.0-rc.6" "$PIN_LINE" \
  "https://github.com/other/iroh-room/actions/runs/${GOOD_RUN}/attempts/1" \
  "https://github.com/other/iroh-room/actions/runs/${GOOD_RUN}/attempts/1" ok ok
run_case "v0.1.0-rc.6"
assert_fail "another repo URL with the same run id is rejected" "github.com/other/iroh-room"

prepare_case "slash" "v0.1.0-rc.6" "$PIN_LINE" "${GOOD_URL}/" "${GOOD_URL}/" ok ok
run_case "v0.1.0-rc.6"
assert_fail "trailing slash is rejected" "${GOOD_URL}/"

prepare_case "http" "v0.1.0-rc.6" "$PIN_LINE" \
  "http://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/1" \
  "http://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/1" ok ok
run_case "v0.1.0-rc.6"
assert_fail "http URL is rejected" "http://github.com/kortiene/iroh-room/actions/runs/${GOOD_RUN}/attempts/1"

prepare_case "missing-id" "v0.1.0-rc.6" "$PIN_LINE" "" "" missing missing
run_case "v0.1.0-rc.6"
assert_fail "missing invocationId is rejected" "missing invocationId"

prepare_case "empty" "v0.1.0-rc.6" "$PIN_LINE" "" "" empty empty
run_case "v0.1.0-rc.6"
assert_fail "empty payload is rejected" "empty dsse payload"

prepare_case "nonjson" "v0.1.0-rc.6" "$PIN_LINE" "" "" nonjson nonjson
run_case "v0.1.0-rc.6"
assert_fail "non-JSON payload is rejected" "not JSON"

prepare_case "disagree" "v0.1.0-rc.6" "$PIN_LINE" "$GOOD_URL" "$BAD_RUN_URL" ok ok
run_case "v0.1.0-rc.6"
assert_fail "archive and SHA256SUMS bundles that disagree are rejected" "SHA256SUMS"
assert_fail "archive and SHA256SUMS bundles that disagree are rejected" "43791394134"

prepare_case "digest" "v0.1.0-rc.6" "$PIN_LINE" "$GOOD_URL" "$GOOD_URL" ok ok \
  "0000000000000000000000000000000000000000000000000000000000000000"
run_case "v0.1.0-rc.6"
assert_fail "different digest fails at gh attestation verify" "build-provenance attestation verification failed"
assert_absent "different digest fails at gh attestation verify" "run pin mismatch"
if ! printf '%s\n' "$CASE_OUT" | grep -F -q "subject digest mismatch"; then
  fail "different digest fails at gh attestation verify: gh stub did not report the digest mismatch: ${CASE_OUT}"
fi
ok "different digest is the gh attestation verify failure"

prepare_case "no-pin" "v0.1.0-rc.6" "# no pins yet" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6"
assert_fail "missing pin line is rejected" "no run pin"

prepare_case "malformed" "v0.1.0-rc.6" "v0.1.0-rc.6 not-a-run 1" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6"
assert_fail "malformed pin line is rejected" "malformed run pin"

prepare_case "duplicate" "v0.1.0-rc.6" "$(printf '%s\n%s\n' "$PIN_LINE" "$PIN_LINE")" \
  "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6"
assert_fail "duplicate tag line is rejected" "duplicate run pin"

prepare_case "rc5" "v0.1.0-rc.5" "this line is malformed" "$GOOD_URL" "$GOOD_URL" ok ok "" 1
run_case "v0.1.0-rc.5"
assert_fail "rc.5 skips the run pin" "v0.1.0-rc.1 through v0.1.0-rc.5"
assert_absent "rc.5 skips the run pin" "malformed run pin"
assert_absent "rc.5 skips the run pin" "no run pin"
assert_absent "rc.5 skips the run pin" "run pin mismatch"

prepare_case "stable" "v0.1.0" "# no pin" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0"
assert_fail "v0.1.0 with no pin is rejected" "no run pin"

prepare_case "rc10" "v0.1.0-rc.10" "# no pin" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.10"
assert_fail "v0.1.0-rc.10 with no pin is rejected" "no run pin"

assert_usage() {
  local label="$1"
  local needle="$2"
  shift 2
  local out="" code=0
  set +e
  out="$("$SRC" "$@" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -eq 0 ]]; then
    fail "${label}: expected a non-zero exit, got 0. output: ${out}"
  fi
  if ! printf '%s\n' "$out" | grep -F -q -- "$needle"; then
    fail "${label}: missing [${needle}] in output: ${out}"
  fi
  ok "${label} (exit ${code})"
}

assert_usage "expected-run 0 is rejected" "attempt starting at 1" \
  --expected-run 0 1 v0.1.0 "$TRIPLE"
assert_usage "expected-run abc is rejected" "attempt starting at 1" \
  --expected-run abc 1 v0.1.0 "$TRIPLE"
assert_usage "expected-run attempt 0 is rejected" "attempt starting at 1" \
  --expected-run "$GOOD_RUN" 0 v0.1.0 "$TRIPLE"
assert_usage "expected-run without an attempt is rejected" "requires <run_id> <attempt>" \
  --expected-run "$GOOD_RUN"

export FIXTURE_SECOND_INVOCATION="$BAD_RUN_URL"
prepare_case "two-online" "v0.1.0-rc.6" "$PIN_LINE" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0-rc.6"
unset FIXTURE_SECOND_INVOCATION
assert_fail "online result with one non-matching attestation is rejected" "43791394134"
assert_absent "online result with one non-matching attestation is rejected" "verified v0.1.0-rc.6"

prepare_case "operator" "v0.1.0" "$PIN_LINE" "$GOOD_URL" "$GOOD_URL" ok ok
run_case "v0.1.0" --expected-run "$GOOD_RUN" "$GOOD_ATTEMPT"
assert_ok "operator-supplied pin for an unpinned tag passes" "$GOOD_URL"
operator_line="run pin: operator-supplied (not in release-run-pins.txt)"
if ! printf '%s\n' "$CASE_OUT" | grep -F -q -- "$operator_line"; then
  fail "operator-supplied pin did not print the required line: ${CASE_OUT}"
fi
last_line="$(printf '%s\n' "$CASE_OUT" | sed '/^$/d' | tail -n 1)"
if [[ "$last_line" != "$operator_line" ]]; then
  fail "operator-supplied final line was [${last_line}]"
fi
ok "operator-supplied final line"

printf 'ok verify-release-artifacts-test\n'
