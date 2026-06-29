---
description: Create a detailed implementation spec without implementing it
argument-hint: "<prompt>"
---
Create a detailed implementation specification for this request:

$ARGUMENTS

Do not implement the requested feature. Only create a planning/spec document.

Workflow:

1. Read enough repository context to make the plan accurate.
   - README and developer docs
   - product requirements, roadmap, backlog, ADRs, or specs when present
   - relevant source packages/modules
   - existing tests, CI, deployment, and operational docs
   - the specific work item context when available

2. Think through the request carefully.
   - owning module/package/service
   - domain model and API/data model impact
   - validation and authorization rules
   - error model and observability
   - security, privacy, reliability, performance, and migration implications
   - test strategy and rollout/rollback plan

3. Create a `specs/` directory if it does not already exist.

4. Write a new Markdown spec file in `specs/`.
   - Derive a short descriptive kebab-case filename from the request.
   - Include implementation steps detailed enough for another engineer/agent to execute.
   - Include acceptance criteria and risks.

5. Do not modify production code.

Return the spec path, summary, key decisions, assumptions, and open questions.
