#!/usr/bin/env bash
set -euo pipefail

# Automated preflight for the Production Readiness Plan.
#
# This script checks evidence that can be verified mechanically. It is not the
# final production sign-off: security review, storage decisions, beta evidence,
# and release-owner acceptance still live in PRODUCTION-READINESS.md.
#
# Usage:
#   scripts/production-readiness.sh
#   scripts/production-readiness.sh --offline-only
#
# The default mode runs scripts/release-readiness.sh first. --offline-only skips
# that expensive gate and reports only document/evidence checks; it must not be
# used to label a build production-ready.

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OFFLINE_ONLY=0
if [[ "${1:-}" == "--offline-only" ]]; then
  OFFLINE_ONLY=1
elif [[ $# -gt 0 ]]; then
  echo "usage: $0 [--offline-only]" >&2
  exit 2
fi

CHECK_NAMES=()
CHECK_STATUS=()
CHECK_NOTES=()

record() {
  CHECK_NAMES+=("$1")
  CHECK_STATUS+=("$2")
  CHECK_NOTES+=("${3:-}")
}

require_file() {
  local path="$1"
  if [[ -f "$path" ]]; then
    record "$path exists" "PASS" ""
  else
    record "$path exists" "FAIL" "missing required file"
  fi
}

require_text() {
  local path="$1"
  local needle="$2"
  local label="$3"
  if [[ -f "$path" ]] && grep -Fq "$needle" "$path"; then
    record "$label" "PASS" ""
  else
    record "$label" "FAIL" "missing text: $needle"
  fi
}

echo "== Production readiness automated preflight =="
echo

if [[ "$OFFLINE_ONLY" -eq 1 ]]; then
  record "release-readiness gate" "SKIP" "--offline-only; not a production-ready verdict"
  record "compatibility fixture gate" "SKIP" "--offline-only; run cargo test before production sign-off"
else
  echo "-- scripts/release-readiness.sh"
  if scripts/release-readiness.sh; then
    record "release-readiness gate" "PASS" ""
  else
    record "release-readiness gate" "FAIL" "scripts/release-readiness.sh did not exit 0"
  fi
  echo

  echo "-- cargo test -p iroh-rooms-core --features store --test compatibility"
  if cargo test -p iroh-rooms-core --features store --test compatibility; then
    record "compatibility fixture gate" "PASS" ""
  else
    record "compatibility fixture gate" "FAIL" "core compatibility fixture test did not exit 0"
  fi
  echo
fi

require_file "PRODUCTION-READINESS.md"
require_file "RELEASE-READINESS.md"
require_file "PHASE-0-GO-NO-GO.md"
require_file "PRD.v0.3.md"
require_file "docs/getting-started.md"
require_file "docs/protocol.md"
require_file "docs/decisions/ADR-0001-local-storage-posture.md"
require_file "docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md"
require_file "docs/decisions/ADR-0003-persistent-audit-posture.md"
require_file "docs/security/threat-model.md"
require_file "docs/operations/data-handling.md"
require_file "docs/operations/release-operations.md"
require_file "docs/operations/install-uninstall.md"
require_file "docs/operations/production-beta-release-notes-template.md"
require_file "docs/compatibility.md"
require_file "crates/iroh-rooms-core/tests/compatibility.rs"
require_file "crates/iroh-rooms-core/tests/fixtures/v1/events.txt"
require_file "crates/iroh-rooms-core/tests/fixtures/v1/store_v1_schema.sql"
require_file "scripts/build-release-artifacts.sh"
require_file ".github/ISSUE_TEMPLATE/bug_report.md"

require_text "PRODUCTION-READINESS.md" "## Production P0 Gates" "production P0 gates documented"
require_text "PRODUCTION-READINESS.md" "## Workstreams" "production workstreams documented"
require_text "PRODUCTION-READINESS.md" "## Initial Backlog" "production backlog documented"
require_text "PRODUCTION-READINESS.md" "P0.3 Threat Model And Security Review" "threat-model gate documented"
require_text "PRODUCTION-READINESS.md" "P0.5 Local Data Handling" "local-data gate documented"
require_text "PRODUCTION-READINESS.md" "P0.7 Compatibility And Migration" "compatibility gate documented"
require_text "PRODUCTION-READINESS.md" "ADR-0001-local-storage-posture.md" "local storage ADR linked from production plan"
require_text "PRODUCTION-READINESS.md" "ADR-0002-invite-revocation-bounded-ticket-risk.md" "invite revocation ADR linked from production plan"
require_text "PRODUCTION-READINESS.md" "ADR-0003-persistent-audit-posture.md" "persistent audit ADR linked from production plan"
require_text "PRODUCTION-READINESS.md" "crates/iroh-rooms-core/tests/fixtures/v1/events.txt" "compatibility fixture linked from production plan"
require_text "PRODUCTION-READINESS.md" "scripts/build-release-artifacts.sh" "release artifact script linked from production plan"
require_text "docs/decisions/ADR-0001-local-storage-posture.md" "trusted local machines" "local storage ADR scopes beta to trusted machines"
require_text "docs/decisions/ADR-0001-local-storage-posture.md" "Production GA remains blocked" "local storage ADR preserves GA blocker"
require_text "docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md" "bounded leaked-ticket model" "invite ADR accepts bounded leaked-ticket model"
require_text "docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md" "Production GA remains blocked" "invite ADR preserves GA revocation blocker"
require_text "docs/decisions/ADR-0003-persistent-audit-posture.md" "Persistent Local Audit" "audit ADR accepts persistent local audit"
require_text "docs/decisions/ADR-0003-persistent-audit-posture.md" "not remote, centrally retained, tamper-evident" "audit ADR scopes audit limitations"
require_text "PRODUCTION-READINESS.md" "audit.ndjson" "persistent audit gate references local audit log"
require_text "docs/operations/data-handling.md" "audit.ndjson" "audit data handling documented"
require_text "docs/operations/data-handling.md" "ADR-0001-local-storage-posture.md" "data handling links storage ADR"
require_text "docs/operations/data-handling.md" "ADR-0003-persistent-audit-posture.md" "data handling links audit ADR"
require_text "docs/operations/data-handling.md" "Suspected Invite Ticket Leak" "ticket leak handling documented"
require_text "crates/iroh-rooms-cli/src/audit.rs" "AUDIT_LOG_FILE" "CLI persistent audit sink present"
require_text "RELEASE-READINESS.md" "Status: DEVELOPER PREVIEW. Not for production." "preview disclaimer remains explicit"
require_text "RELEASE-READINESS.md" "ADR-0001-local-storage-posture.md" "release readiness names storage ADR"
require_text "RELEASE-READINESS.md" "ADR-0002-invite-revocation-bounded-ticket-risk.md" "release readiness names invite ADR"
require_text "RELEASE-READINESS.md" "ADR-0003-persistent-audit-posture.md" "release readiness names audit ADR"
require_text "docs/compatibility.md" "cargo test -p iroh-rooms-core --features store --test compatibility" "compatibility test command documented"
require_text "crates/iroh-rooms-core/tests/fixtures/v1/events.txt" "E_PIPE_CLOSED|pipe.closed" "pipe.closed v1 fixture present"
require_text "crates/iroh-rooms-core/tests/fixtures/v1/events.txt" "E_AGENT_STATUS|agent.status" "agent.status v1 fixture present"
require_text "crates/iroh-rooms-core/tests/fixtures/v1/store_v1_schema.sql" "PRAGMA user_version = 1" "v1 SQLite schema fixture stamped"
require_text "scripts/build-release-artifacts.sh" "shasum -a 256" "release artifact script writes SHA-256 checksums"
require_text "docs/operations/release-operations.md" "scripts/build-release-artifacts.sh --version <VERSION>" "release ops documents artifact build command"
require_text "docs/operations/install-uninstall.md" "Rollback requires" "install docs include rollback procedure"
require_text "docs/operations/install-uninstall.md" 'rm -f "$HOME/.local/bin/iroh-rooms"' "install docs include uninstall command"
require_text "docs/operations/production-beta-release-notes-template.md" "Known Limitations" "release notes template surfaces limitations"
require_text "docs/operations/production-beta-release-notes-template.md" "Artifacts And Checksums" "release notes template includes checksums"
require_text ".github/ISSUE_TEMPLATE/bug_report.md" "full data-directory backups" "bug template blocks raw local backups"
require_text "CONTRIBUTING.md" "Production Readiness" "contributing links production readiness"
require_text "README.md" "Production Readiness" "readme links production readiness"

RESULTS_MD="crates/spike-nat/results/results.md"
if [[ -f "$RESULTS_MD" ]] && ! grep -Fq "pending manual two-host run" "$RESULTS_MD"; then
  record "Gate A measured results present" "PASS" "$RESULTS_MD"
else
  record "Gate A measured results present" "FAIL" "$RESULTS_MD missing or still pending"
fi

if grep -Fq "publish = false" crates/iroh-rooms/Cargo.toml; then
  record "SDK publication status is explicit" "WARN" "crates/iroh-rooms remains publish=false; acceptable for beta only if documented"
else
  record "SDK publication status is explicit" "PASS" "SDK is publishable or publish flag removed"
fi

echo "== Summary =="
FAILED=0
WARNED=0
for i in "${!CHECK_NAMES[@]}"; do
  name="${CHECK_NAMES[$i]}"
  status="${CHECK_STATUS[$i]}"
  note="${CHECK_NOTES[$i]}"
  if [[ -n "$note" ]]; then
    printf '%-55s %-6s %s\n' "$name" "$status" "$note"
  else
    printf '%-55s %-6s\n' "$name" "$status"
  fi
  case "$status" in
    FAIL) FAILED=$((FAILED + 1)) ;;
    WARN) WARNED=$((WARNED + 1)) ;;
    SKIP) WARNED=$((WARNED + 1)) ;;
  esac
done
echo

if [[ "$FAILED" -eq 0 ]]; then
  if [[ "$OFFLINE_ONLY" -eq 1 ]]; then
    echo "production-readiness: OFFLINE PREFLIGHT PASS (${WARNED} warnings/skips; release-readiness not run)"
  else
    echo "production-readiness: AUTOMATED PREFLIGHT PASS (${WARNED} warnings)"
  fi
  echo "manual P0 sign-offs in PRODUCTION-READINESS.md are still required before a production label"
  exit 0
else
  echo "production-readiness: AUTOMATED PREFLIGHT FAIL (${FAILED} failures, ${WARNED} warnings/skips)"
  exit 1
fi
