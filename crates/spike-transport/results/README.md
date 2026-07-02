# `spike-transport` results — committed run artifacts

This directory holds the durable, machine-readable outputs of the IR-0006
Day-4 gossip-vs-full-mesh comparison (spec §7.9). Unlike `spike-nat`'s Gate A
(a manual two-host run CI cannot prove), every scenario here runs on
deterministic loopback — CI proves the *measured* claims too (`tests/self_check.rs`).

## Layout

- `results.md` — the rolled-up backend × N × dimension table, produced by
  [`report::results_md`](../src/report.rs) and pasted verbatim into
  `NOTES.md`'s decision memo. This is the only run artifact committed here.
- `transport-probe compare --json` emits a
  [`ComparisonResult`](../src/report.rs) per run (schema = the spec §7.6 field
  table) to stdout; no `--json` output is committed to this directory.

## Privacy

Any JSON emitted by `transport-probe --json` contains only synthetic loopback
measurements (propagation timings, convergence booleans, event counts) — no
real identities, no network addresses beyond `127.0.0.1`, no secrets.
