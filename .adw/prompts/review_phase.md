---
description: Review the working-tree implementation in the phased ADW pipeline
argument-hint: "<spec-file-or-empty> <work-item-and-change-context>"
---
Review the implementation currently in the working tree for this change. There is no change request yet — review the staged and uncommitted changes against the work item and, if one was created, the spec.

Spec file, if any: $1

Work item and change context:

${@:2}

## What to do

1. Understand the change.
   - Inspect the working-tree diff against the base branch and read changed files in context.
   - Read the work item, acceptance criteria, and spec (`$1`) when provided.
   - Read relevant repository docs and tests.

2. Review as a strict pre-merge reviewer.
   - correctness and edge cases
   - security/privacy/data handling
   - API/data model compatibility
   - maintainability and scope control
   - test quality and meaningful assertions
   - documentation and operational impact

3. Classify findings.
   - `blocker`: must be fixed before merge
   - `tech_debt`: valid but not blocking this change
   - `skippable`: optional/nit/context

4. Author release artifacts if useful.
   - If the change is merge-ready or nearly merge-ready, write a clear commit message and change-request body to the artifact paths provided by the ADW footer.
   - Do not run git or forge CLI commands; the orchestrator owns those operations.

Be concise but rigorous. Avoid generic feedback; every blocker should be actionable.
