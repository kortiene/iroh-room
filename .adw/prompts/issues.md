---
description: Implement multiple work items sequentially using /issue semantics
argument-hint: "<work-item-id-or-range> [work-item-id-or-range ...] [-- notes]"
---
Implement multiple work items sequentially for this repository, end to end, using `/issue` semantics for each item.

Work item selectors and shared notes:

$ARGUMENTS

`/issues` is a batch orchestrator. Process items one at a time, in normalized order, and return to a clean, updated base branch between items. Do not parallelize. Do not combine multiple work items into one branch or change request unless explicitly asked and the items genuinely require a shared implementation.

Workflow:

1. Parse and normalize selectors.
   - Accept single numeric IDs, e.g. `12`.
   - Accept inclusive hyphen ranges, e.g. `12-14` expands to `12, 13, 14`.
   - Accept inclusive dot ranges, e.g. `12..14` expands to `12, 13, 14`.
   - Treat content after `--` as shared notes for every item.

2. For each item, run the equivalent of `/issue` end to end.

3. Preserve isolation.
   - One work item → one branch/change request unless explicitly directed otherwise.
   - Do not carry uncommitted changes from one item into the next.
   - Stop on blockers instead of cascading failures into later items.

4. Produce a final batch report with completed items, skipped/blocked items, links or identifiers if available, and follow-up risks.
