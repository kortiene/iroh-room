---
description: Add or improve end-to-end tests for cross-boundary project flows
argument-hint: "[spec-file|change-request-or-number|notes]"
---
Add or improve end-to-end test coverage for this target:

$ARGUMENTS

Use this command for heavier scenarios that cross meaningful system boundaries: UI ↔ API, client ↔ server, auth/session lifecycle, async jobs, persistence, sync/offline behavior, external integrations, or deployment/runtime boundaries. Prefer the focused test phase for unit tests and deterministic non-e2e integration tests.

Workflow:

1. Understand the e2e target.
   - If the argument is a spec file path, read it completely and identify the end-to-end behavior that needs coverage.
   - If the argument is a change request, work item, or notes, use the orchestrator-provided context and repository files. Do not run git or forge CLI commands when operating inside the ADW phased pipeline; the orchestrator owns those operations.
   - If no target is clear, inspect the working tree and ask for clarification only if genuinely blocked.

2. Read repository and test infrastructure context before editing.
   - package/build manifests
   - existing test directories and helpers
   - CI or local test commands
   - relevant app/service/module code
   - architecture/security docs when present

3. Add the smallest valuable e2e coverage.
   - Prefer stable, deterministic tests.
   - Avoid tests requiring real third-party services unless the repository already has a safe test harness.
   - Use mocks/fakes only where they preserve the end-to-end contract being tested.

4. Run the relevant test command if discoverable and safe.

Report what was added, how to run it, and any coverage gaps left intentionally open.
