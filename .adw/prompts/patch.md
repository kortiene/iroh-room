---
description: Resolve blocking review findings in the phased ADW pipeline
argument-hint: "<blocker-findings-and-context>"
---
A self-review found blocking issues in the current implementation. Resolve them.

Blocking findings and context:

$ARGUMENTS

## Instructions

- Address every blocking finding above with the smallest correct change.
- Only fix the listed blockers. Do not act on tech-debt or skippable findings unless doing so is necessary to resolve a blocker.
- Do not start unrelated work or broad rewrites.
- Keep tests meaningful: fix root causes, do not weaken assertions, delete coverage, or mask failures.
- Preserve documented project constraints, especially security, privacy, data-handling, compatibility, and operational guarantees.
- Do not run git or forge CLI commands inside the ADW phased pipeline; the orchestrator owns those operations.

After editing, report which blockers were resolved, which remain, and what verification was run.
