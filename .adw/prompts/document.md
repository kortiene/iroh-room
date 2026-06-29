---
description: Update project documentation after a reviewed implementation (phased ADW)
argument-hint: "<change-summary-and-files>"
---
Update the repository's documentation to reflect the implemented, reviewed change.

Change summary, files changed, and context:

$ARGUMENTS

## Scope and boundary

This is the standalone documentation pass, distinct from inline documentation already made during implementation.

- The implementation phase should have handled code-local comments, public API docstrings, and usage text tightly coupled to the changed code.
- Here, update broader prose that benefits from seeing the finished change: README, docs, architecture notes, developer guides, changelog, migration notes, API docs, or runbooks when relevant.
- Do not create noisy documentation for internal-only changes that do not affect users, operators, or maintainers.
- Do not invent product behavior, deployment assumptions, or compatibility promises that are not supported by the code.

## Workflow

1. Inspect the changed files and existing docs.
2. Identify docs that are stale, missing, or misleading because of this change.
3. Update only the documentation that should ship with the change.
4. Keep language precise, maintainable, and project-neutral.
5. Preserve documented domain, privacy, security, and operational constraints found in the repo.

## Output expectations

- Make the documentation edits directly.
- Summarize what changed and why.
- If no documentation update is warranted, say so clearly and explain the reason.
