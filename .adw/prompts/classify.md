---
description: Classify a work item into a change type for the phased ADW pipeline
argument-hint: "<work-item-id> <work-item-context>"
---
Classify work item #$1 into a single change type for branch naming and commit prefixing.

Work item context (title, body, labels):

$ARGUMENTS

## Instructions

- Choose exactly one `issue_class` from: `feat`, `fix`, `docs`, `chore`, `ci`, `test`, `refactor`.
  - `feat` — new user-visible feature or capability.
  - `fix` — bug fix or correctness change.
  - `docs` — documentation-only change.
  - `chore` — maintenance, deps, tooling with no user-visible behavior change.
  - `ci` — CI/build/workflow change.
  - `test` — test-only change.
  - `refactor` — internal restructuring with no behavior change.
- Prefer the narrowest truthful classification.
- If labels and body disagree, classify by the actual requested change.
