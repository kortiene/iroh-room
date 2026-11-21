#!/usr/bin/env bash
# Live CLI process tests for `room remove` and `room leave`.
#
# Called by .github/workflows/live-room-membership.yml on every pull request
# and on push to main. The two tests stay #[ignore] so scripts/verify.sh does
# not start them. Exact name filters keep the rest of the two_peer_e2e online
# tier out of this job; that tier remains scripts/release-readiness.sh.
#
# `cargo test` exits 0 when every filter misses. The summary check below turns
# that into a failure, so a renamed test cannot go green with zero runs.
#
# Refs #392

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

tests=(
  admin_removal_reaches_member_and_withholds_new_messages
  member_leave_reaches_admin
)

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

echo "live-room-membership: start $(date -u +%Y-%m-%dT%H:%M:%SZ)"
start=$(date +%s)

# Cold debug builds of iroh-rooms-cli dominate this budget. Each test's own
# waits are 15s (WAIT in two_peer_e2e.rs); 30m is the hang bound.
set +e
timeout 30m \
  stdbuf -oL -eL cargo test -p iroh-rooms-cli --test two_peer_e2e -- \
    --ignored \
    --test-threads=1 \
    --exact \
    "${tests[@]}" \
  2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e

end=$(date +%s)
echo "live-room-membership: wall_seconds=$((end - start)) cargo_status=${status}"

if [[ "$status" -ne 0 ]]; then
  echo "live-room-membership: cargo test failed (status ${status})" >&2
  exit "$status"
fi

if ! grep -Eq '^test result: ok\. 2 passed; 0 failed;' "$log"; then
  echo "live-room-membership: expected exactly 2 passed tests" >&2
  exit 1
fi
