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

**Both required environments measured** (2026-07-03/04; 37 per-run JSONs, table in
`results.md`, findings in [`../NOTES.md`](../NOTES.md) §6):

- **S1** home-broadband NAT ↔ Hetzner cloud (non-symmetric) — 18 runs + 5
  `--settle 30` reconciliation runs (issue #43).
- **S2** iPhone cellular Personal Hotspot (carrier CGNAT — the likely-symmetric
  environment) ↔ {Hetzner cloud, home-broadband NAT} — 14 runs.

Establishment and relay reachability pass across both environments, both
directions (incl. inbound-to-CGNAT); a direct path is Active on every established
run (nat-probe labels it `mixed` because iroh 1.0.1 keeps the relay as a warm
standby even at `--settle 30` — a classifier label, corroborated as a real direct
path by the #43 SDK-daemon data point). Soft residuals: a larger-sample cellular
forced-relay **throughput** re-measure (the 256 KiB samples read below the
≥1 Mbit/s target over the mobile uplink) and the home-NAT→CGNAT reverse leg. CI
proves the harness builds and its loopback self-check passes; it **cannot** prove
NAT traversal.

Additional refresh:

- **S3** operator-local ↔ `demo1` cloud (2026-07-07) — 4 runs, both
  directions, natural + relay-only. Establishment passed in all rows; natural
  rows settled `mixed`; relay-only rows measured 4.1 Mbit/s BtoA and
  1.3 Mbit/s AtoB. VPN/shared-LAN status was not independently verified by the
  tool, so S3 is a current refresh and not a replacement for S1/S2.
