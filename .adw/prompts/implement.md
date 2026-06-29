---
description: Implement a spec file end-to-end
argument-hint: "<spec-file>"
---
Implement the specification in this file end-to-end:

$1

Extra context/notes from me (may be empty): ${@:2}

Do not stop after planning unless the spec is genuinely ambiguous, unsafe, impossible, or blocked by missing information. Read the spec, implement it, test it, and report the result.

Workflow:

1. Read and understand the spec.
   - Read `$1` completely.
   - Treat the spec as the source of truth for scope and acceptance criteria.
   - If the file does not exist, stop and report the missing path.
   - If the spec is ambiguous, make a safe, explicit assumption when possible and proceed. Stop only for real blockers.

2. Read enough repository context.
   - Identify the owning package/module/service.
   - Inspect relevant existing patterns, tests, config, docs, and contracts.
   - Preserve project-specific security, privacy, compliance, performance, and operational constraints documented in the repo.

3. Implement with production-grade discipline.
   - Keep the change scoped to the spec.
   - Prefer clear, maintainable code over cleverness.
   - Add validation, error handling, and observability where relevant.
   - Avoid unrelated refactors.

4. Verify.
   - Add or update focused tests when appropriate.
   - Run the relevant local checks if discoverable and safe.
   - If checks cannot be run, explain exactly why.

Return a concise implementation report: files changed, behavior implemented, tests/checks run, risks, and follow-ups.
