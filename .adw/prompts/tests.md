---
description: Add or improve focused non-e2e tests for a spec, change request, or working tree
argument-hint: "[spec-file|change-request-or-number|notes]"
---
Add or improve focused non-e2e test coverage for this target:

$ARGUMENTS

Use this command for unit tests, deterministic integration tests that do not require external services, argument/output tests, policy/protocol/schema tests, and negative/security regression tests. Do not add tests that require live infrastructure or hardware here; use the e2e phase for cross-boundary scenarios.

Workflow:

1. Understand the testing target.
   - If the argument is a spec file path, read it completely and identify behavior that should be covered.
   - If the argument is a change request, work item, or notes, use the provided context and repository files.
   - If no argument is provided, inspect the current working tree and ask for clarification only if the target is genuinely unclear.

2. Read repository context before editing.
   - existing tests and test helpers
   - package manifests and test commands
   - relevant source modules
   - documented project constraints and acceptance criteria

3. Add the smallest meaningful tests.
   - Prefer deterministic, isolated coverage.
   - Cover regressions and edge cases, not only happy paths.
   - Do not snapshot broad unstable output unless the project already uses that pattern.

4. Run the relevant test command if discoverable and safe.

Report tests added, commands run, failures fixed or remaining, and any risk not covered.
