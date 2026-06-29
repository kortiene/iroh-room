---
description: Assess project feature completeness against product docs, backlog, code, and work items
argument-hint: "[focus area or milestone target]"
---
Think hard and perform a thorough repository assessment of this project.

Optional focus area or milestone target from me: $ARGUMENTS

Read all files needed to evaluate how close the project is to being feature-complete relative to its stated product vision, requirements, backlog, milestones, and open work items. Do not assume the stack, product domain, or delivery stage until you verify them from the repository.

Start by reviewing, when present:

- README / getting-started documentation
- product requirements, PRDs, RFCs, ADRs, roadmap, backlog, specs, or issue tracker exports
- architecture and security documentation
- source directories and package manifests
- tests, CI configuration, deploy configuration, and operational runbooks
- recent specs or work-item context relevant to the requested focus area

Classify repository areas as:

- complete and shippable
- partially implemented
- planned but not implemented
- undocumented / ambiguous
- risky or blocked

Produce a concise, evidence-backed assessment with:

1. Executive summary
2. Feature-completeness score by major capability area
3. Evidence from files and paths
4. Critical gaps and blockers
5. Security/privacy/operational risks
6. Recommended milestone or issue breakdown
7. Next 3–5 highest-leverage actions

Clearly separate confirmed facts from reasonable inferences. Prefer actionable findings over generic commentary.
