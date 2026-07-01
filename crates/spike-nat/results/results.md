# Gate A — rolled-up results

Rendered by [`report::results_md`](../src/report.rs) from the committed per-run
JSON in this directory. One row per (scenario, direction, path-mode). This table
drops verbatim into `crates/iroh-rooms-net/NOTES.md` under *Gate A
(real-network)* and feeds the Gate E memo (#15).

| scenario | direction | mode | established | path type | ttfb (ms) | ttfb direct (ms) | ttfb relay (ms) | rtt median (ms) | throughput (Mbit/s) | setup (ms) |
|----------|-----------|------|-------------|-----------|-----------|------------------|-----------------|-----------------|---------------------|------------|
| _(pending manual two-host run — see [../NOTES.md](../NOTES.md) runbook)_ | | | | | | | | | | |

> **This table is empty by design in the code deliverable.** Gate A is an
> inherently manual measurement across two physical machines on two different real
> NATs (spec §1 / §12 risk 1). The harness, runbook, GO/NO-GO rubric, and results
> schema are landed and CI-proven; the *numbers* are produced by the operator
> running the matrix (spec §7) and committing the per-run JSON here. Regenerate
> this table from the JSON after the run.
