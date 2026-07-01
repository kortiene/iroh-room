# `spike-nat` results — committed run artifacts

This directory holds the durable, machine-readable outputs of the Gate-A
measurement (spec §5 / §6.5). It is the artifact the Gate E go/no-go memo (#15)
consumes.

## Layout

- `<run_at_utc-date>-<scenario>-<direction>[-relay].json` — one
  [`ProbeResult`](../src/report.rs) per run, emitted by
  `nat-probe dial … --json <path>`. Schema = the spec §5 field table.
- `results.md` — the rolled-up scenario × direction × path-type × TTFB × RTT ×
  throughput × setup-time table, pasted verbatim into
  `crates/iroh-rooms-net/NOTES.md` under *Gate A (real-network)*.

## Privacy (spec §8)

Committed JSON contains `EndpointId`s (public keys), relay URLs, ISP/network-type
labels, and timings — **no secrets**. Do **not** commit socket addresses that
reveal a home IP: `nat-probe` deliberately keeps the resolved `remote_info` addr
set on the operator's console (a "private — redact before committing" block) and
out of the JSON. If you paste addrs into `notes` for debugging, redact home IPs
first.

## Status

Pending the manual two-host run (see [`../NOTES.md`](../NOTES.md) runbook). CI
proves the harness builds and its loopback self-check passes; it **cannot** prove
NAT traversal.
