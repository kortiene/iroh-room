---
description: Resolve failing repository checks reported by the phased ADW test gate
argument-hint: "<failing-output-and-context>"
---
The repository's test or verification gate is failing. Fix the failures.

Failing output and context (truncated):

$ARGUMENTS

## Instructions

- Investigate the failures and make the smallest correct change that fixes them.
- Fix the root cause in code, tests, config, or docs as appropriate.
- Do not weaken meaningful assertions, skip tests, remove coverage, or mask failures just to make the gate pass.
- Stay within the scope of the current change; do not start unrelated work.
- Preserve documented project constraints, especially security, privacy, data-handling, compatibility, and operational guarantees.
- Do not run git or forge CLI commands inside the ADW phased pipeline; the orchestrator owns those operations.

Run the failing gate again if safe and available. Report what failed, what changed, what was verified, and any remaining risk.
