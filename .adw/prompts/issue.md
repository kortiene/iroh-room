---
description: Implement a work item end-to-end using plan/implement/tests/e2e/review phases
argument-hint: "<work-item-id> [notes]"
---
Implement work item #$1 for this repository, end to end.

Extra context/notes from me (may be empty): ${@:2}

`/issue` is the full delivery pipeline. Reuse the project prompt templates below as phase contracts, but do not stop after any individual phase unless there is a real blocker:

- planning/spec creation when warranted
- disciplined implementation
- focused non-e2e test coverage
- conditional end-to-end coverage
- local self-review before shipping
- documentation updates when user-visible, operational, or developer-facing behavior changes

Read the relevant prompt files before starting. Apply their workflows inline as phases of this `/issue` run; do not merely tell the user to run them separately.

The orchestrator owns all git and forge operations (branch, commit, push, change request, CI watch, merge). Do not run git or forge CLI commands yourself inside the ADW phased pipeline. Your job is to implement, test, self-review, and document the working tree change, then hand off a clean, verified result and a clear report for the orchestrator to ship.

Preserve the product, security, privacy, and operational constraints documented in this repository. If the work item conflicts with those constraints, stop and report the conflict.
