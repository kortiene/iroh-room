#!/usr/bin/env bash
set -euo pipefail

# Release-readiness gate for a developer preview build (issue #41 / IR-0306).
#
# This is a *manual*, release-time gate — it is not run by `verify.yml` and it
# does not change `scripts/verify.sh`. It exists to make "the preview is ready"
# a real exit code instead of a checked box: see RELEASE-READINESS.md.
#
# Usage:
#   scripts/release-readiness.sh                # full P0 gate (deterministic + online)
#   scripts/release-readiness.sh --skip-online  # deterministic tier only, for local
#                                                # iteration; NEVER produces a READY verdict

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SKIP_ONLINE=0
if [[ "${1:-}" == "--skip-online" ]]; then
  SKIP_ONLINE=1
elif [[ $# -gt 0 ]]; then
  echo "usage: $0 [--skip-online]" >&2
  exit 2
fi

# P0 — gated online tiers (loopback only; no relay, no external tools). Each
# entry here MUST be byte-identical to a row in RELEASE-READINESS.md's
# "P0 — gated online tiers (loopback)" table — crates/iroh-rooms-cli/tests/
# release_readiness_docs.rs asserts the two sets match exactly so they cannot
# drift apart. `facade_e2e.rs` is intentionally absent: its tests are not
# `#[ignore]`-gated and already run inside the deterministic `--all-features`
# tier below.
ONLINE_TIERS=(
  "cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1"
  "cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1"
  "cargo test -p iroh-rooms-cli --test pipe_cli -- --ignored --test-threads=1"
  "cargo test -p iroh-rooms-cli --test agent_e2e -- --ignored --test-threads=1"
  "cargo test -p iroh-rooms --features experimental --test example_agent_e2e -- --ignored --test-threads=1"
  "cargo test -p iroh-rooms-cli --test error_taxonomy_e2e --test diagnostics_cli -- --ignored --test-threads=1"
)

CHECK_NAMES=()
CHECK_STATUS=()

record() {
  CHECK_NAMES+=("$1")
  CHECK_STATUS+=("$2")
}

echo "== P0 — deterministic tier (scripts/verify.sh) =="
if scripts/verify.sh; then
  echo "-- verify.sh: PASS"
  record "verify.sh" "PASS"
else
  echo "-- verify.sh: FAIL"
  record "verify.sh" "FAIL"
fi
echo

if [[ "$SKIP_ONLINE" -eq 1 ]]; then
  echo "== P0 — gated online tiers (loopback) =="
  echo "-- SKIPPED (--skip-online)"
  echo
  echo "== Summary =="
  for i in "${!CHECK_NAMES[@]}"; do
    printf '%-90s %s\n' "${CHECK_NAMES[$i]}" "${CHECK_STATUS[$i]}"
  done
  echo
  echo "release-readiness: ONLINE TIER SKIPPED — NOT release-ready"
  exit 1
fi

echo "== P0 — gated online tiers (loopback) =="
for cmd in "${ONLINE_TIERS[@]}"; do
  echo "-- $cmd"
  if (eval "$cmd"); then
    record "$cmd" "PASS"
  else
    record "$cmd" "FAIL"
  fi
done
echo

echo "== P1 — tracked, non-blocking =="
RESULTS_MD="crates/spike-nat/results/results.md"
if [[ -f "$RESULTS_MD" ]] && ! grep -q "pending manual two-host run" "$RESULTS_MD"; then
  echo "gate-a: measured results present in $RESULTS_MD"
else
  echo "gate-a: PENDING — no measured two-host run yet; record this status in RELEASE-READINESS.md Sign-off (Gate A never blocks a preview, but must be explicitly acknowledged)"
fi
echo

echo "== Summary =="
FAILED_NAMES=()
for i in "${!CHECK_NAMES[@]}"; do
  printf '%-90s %s\n' "${CHECK_NAMES[$i]}" "${CHECK_STATUS[$i]}"
  if [[ "${CHECK_STATUS[$i]}" == "FAIL" ]]; then
    FAILED_NAMES+=("${CHECK_NAMES[$i]}")
  fi
done
echo

if [[ ${#FAILED_NAMES[@]} -eq 0 ]]; then
  echo "release-readiness: READY"
  exit 0
else
  joined="$(printf '%s; ' "${FAILED_NAMES[@]}")"
  echo "release-readiness: NOT READY (${#FAILED_NAMES[@]} P0 checks failing: ${joined%; })"
  exit 1
fi
