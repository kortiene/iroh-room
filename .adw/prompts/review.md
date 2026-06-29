---
description: Review a change request or working-tree change and comment when useful
argument-hint: "<change-request-or-number> [spec-file]"
---
Review this change, which may have been produced by an implementation phase:

Change request or target: $1
Spec file, if provided: $2
Extra context/notes from me (may be empty): ${@:3}

Do not modify code unless explicitly asked. Focus on review quality, correctness, security, scope control, and actionable feedback. If the environment provides a change request and commenting is appropriate, comment only when the feedback is clear and useful.

Workflow:

1. Validate inputs.
   - If the target is missing, ask for a change request, branch, diff, or working-tree target.
   - If a spec file is provided, read it completely and review against it.

2. Read repository context before reviewing.
   - product/architecture/security docs when relevant
   - changed files and surrounding code
   - tests and CI/check output when available

3. Review for:
   - correctness and edge cases
   - security/privacy/data handling
   - maintainability and scope control
   - test adequacy
   - backwards compatibility and migration risk
   - documentation impact

4. Produce prioritized findings.
   - `blocker`: must fix before merge
   - `tech_debt`: should fix soon but not necessarily blocking
   - `skippable`: optional/nit/context

Keep findings concrete, reproducible, and path-specific where possible.
