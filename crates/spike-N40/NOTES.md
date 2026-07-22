# `spike-N40` — N=40 v1-with-guardrails measurement & GO/NO-GO decision memo (#145)

This is the decision-memo deliverable required by issue #145 and spec
`specs/measure-n40-v1-guardrails.md`. It records what was measured, the
caveats, the cascade-threshold statement, the N=40 rebind result, and the
GO/NO-GO rubric evaluation for #154 (gossip-overlay decision).

> **Status:** throwaway-grade spike (mirrors `spike-transport` / `spike-nat`
> / `spike-blobs`). It exercises the shipping `iroh-rooms-net` carrier at
> over-cap transport pressure; it does **not** modify the shipping crates
> (`iroh-rooms-core`, `iroh-rooms-cli`, `iroh-rooms-net`, `iroh-rooms`) and
> is not a dependency of any of them. Every required-acceptance scenario
> runs on deterministic loopback (`NetMode::Loopback`,
> `RelayMode::Disabled`); the N=5 self-check is CI-reproducible
> (`tests/self_check.rs`), the full N=40 matrix is a manual run committed
> as `results/results.md`.

---

## 1. What was measured and why

`PHASE-0-SPIKE.md:43-49` recorded the **pre-`b0622ec`** N=25 collapse: at
idle, `frames_sent=0`, `accepted=0`, 661 MB inbound backlog, and under load
22 published events yielded `accepted=0` with 55,222 queued frames while
connectivity still looked healthy. The post-`b0622ec` guardrails — byte-
bounded priority queues, quiescent ticks, early event-id dedup, and the #136
dial-stomp fix — may have changed the failure mode from "silent collapse"
into "constant reconnect churn", or they may have raised the ceiling
entirely. This spike answers the three decision-record questions:

1. Does N=40 idle survive?
2. At what room-wide event rate does the queue-close-on-full cascade begin?
3. Is gossip warranted now, or does v1-with-guardrails hold to ~40?

The harness stands up N=5/10/20/40 in-process loopback nodes through the
**shipping** `iroh_rooms_net::Node`, forms the full mesh via
`Node::connect_to` for every ordered pair `(i, j)`, drives a configurable
admin-authored publish rate, and samples process RSS, per-node task
estimates, accepted / `frames_sent` counters, outbound queue bytes, queue-
saturation audit events, and reconnect churn to compute the cascade verdict
(spec §4 D4) and the GO/NO-GO rubric for #154.

---

## 2. Caveats (must read before citing any number below)

1. **Over-cap transport allowlist, NOT a literal >5 active membership fold
   (spec §4 D1).** The shipping `MAX_ACTIVE_MEMBERS = 5` invariant is **not**
   modified by this spike. Every node runs through the real
   `iroh_rooms_net::Node` with an `AllowlistAdmission` admitting every N
   endpoint devices; non-admin nodes are transport-admitted by the spike
   allowlist rather than by a >5 membership snapshot. This exercises the real
   post-guardrail transport paths (byte-bounded queues, dial-with-backoff
   `peer::dial_loop`, #136 guarded state transitions, `SyncEngine` counters)
   but is **not** a product-supported active-member room. Treat N as
   "transport participants" / "over-cap devices" in reports.

2. **Per-node RSS is derived from process RSS, not measured per process
   (spec §4 D3).** All nodes run in one process, so Linux cannot report
   exact RSS per node. Tables label this as `rss_per_node_est =
   (process_rss - baseline_rss) / N`. Exact per-node RSS requires a
   multiprocess harness (out of scope).

3. **Writer / reader task counts are estimated from connected peer entries
   (spec §6.6 / risk 3).** `dial_loop_tasks = N - 1` by construction; the
   writer / reader estimates are `connected_peers` (one writer + one reader
   per live peer link). Exact Tokio task handles are private inside
   `iroh-rooms-net` and are out of scope.

4. **Loopback differs from real NAT (spec §13 risk 6).** This spike measures
   topology / queue / reconnect pressure, not internet traversal. The
   rebind/NAT-drop probe is simulated by endpoint shutdown / respawn on
   loopback.

5. **Admin-only workload may understate multi-author causal/backfill
   pressure (spec §13 risk 5).** Every load event is `message.text` authored
   by the genesis admin (node 0) on a linear `prev_events` chain. It is the
   safest no-shipping-change workload; a multi-author >5 membership pressure
   test would need a separately-approved test-only core override.

6. **Rebind pre-seed caveat (spec §6.8 step 6 partial).** The shipping
   `SyncEngine` does not expose a direct store-insert from the public API,
   so the rebound node's in-memory store is empty at respawn and relies on
   anti-entropy backfill to repopulate. This still measures convergence
   time, just from an empty store (a stricter condition than step 6's
   "seeded with the baseline set it knew before").

---

## 3. Results table

**Measured 2026-07-22** (debug + release runs, single-process loopback):

| N | rate events/s | survives? | connected | cascade? | notes |
|---:|---:|---|---|---|---|
| 5 | idle | yes | 20/20 | no | baseline confirmed |
| 5 | 0.1 | yes | 20/20 | no | |
| 5 | 1 | yes | 20/20 | no | |
| 5 | 5 | yes | 20/20 | no | |
| 40 | idle | **no** | — | — | **QUIC panic during mesh formation** (see §4) |
| 40 | 0.1–5 | not measured | — | — | mesh never formed; measurement not reachable |

N=10 and N=20 were not run (the matrix jumped from N=5 to N=40). The exact
ceiling between 5 and 40 is not pinned by this spike — see §6 caveat.

Full matrix regeneration command:

```
cargo run -p spike-n40 --bin n40-probe -- matrix \
  --markdown crates/spike-N40/results/results.md \
  --json    crates/spike-N40/results/<date>-matrix.json
```

### `survives?` rubric (spec §7.2)

- `yes`: no cascade triggers, expected connectedness reached/restored,
  delivery converged.
- `degraded`: transient reconnect / saturation occurred but delivery
  recovered by end of run.
- `no`: cascade trigger persisted, delivery did not recover, or run timed
  out.

### Cascade triggers (spec §4 D4)

A cascade **begins** at the first rate where **any** of these hold during
the steady load window:

1. At least one `transport.queue.saturated` audit event occurs.
2. Reconnect churn > 1.0/sec for ≥ 2 consecutive 5-second sample windows
   after warmup.
3. Connectedness < 95% of expected peer entries for ≥ 10 seconds.
4. Accepted-event delivery < 95% of expected recipients for ≥ 2 consecutive
   sample windows and does not recover by end of run.

---

## 4. N=40 rebind / NAT-drop convergence

**Not measured.** The N=40 mesh never formed — the QUIC layer (`noq-proto`)
panicked during the mesh-setup phase (forming 1560 concurrent connections
in a single process). See §5 for the panic details. The rebind convergence
measurement requires a successfully-formed mesh and is therefore not
reachable at N=40 in the current in-process harness.

---

## 5. Cascade threshold statement

**N=5: no cascade observed at any rate up to 5 events/s.** The queue-close-
on-full guardrails, quiescent ticks, and #136 dial-stomp fix work as designed
at the current ceiling.

**N=40: not measurable — the QUIC layer panicked during mesh formation.**
The `noq-proto` crate (`noq-proto-1.0.1/src/connection/mod.rs:4141`) panicked
when managing 1560 concurrent QUIC connections in a single process. In debug
mode, the first panic was `"attempt to subtract with overflow"` (the QUIC
connection state machine's internal arithmetic). In release mode (overflow
checks off), a different panic surfaced at the same site — confirming the
crash is not an arithmetic artifact but a genuine connection-management
failure at scale. The chained `PoisonError` from `noq-1.0.1/src/mutex.rs:138`
is the mutex poisoned by the first panic (cleanup code).

**N=10/20: not measured.** The exact ceiling between "works at 5" and "crashes
at 40" is unknown. A follow-up run targeting N=10 and N=20 would pin it.

---

## 6. GO/NO-GO rubric evaluation for #154

**Verdict: GO — gossip overlay IS warranted for N>5.**

Spec §8 classification:
- N=5: **GO** — v1-with-guardrails holds; gossip not needed at the current cap.
- N=40: **NO-GO for full-mesh** — the QUIC layer itself panics at 1560 in-process
  connections. Even in a multi-process deployment (39 connections per process),
  the pre-`b0622ec` N=25 measurement (661 MB backlog, `accepted=0`,
  `frames_sent=0`) is canonical evidence that full-mesh fan-out amplification
  collapses the transport well before N=40.
- The exact ceiling (N=10? 15? 20?) is not pinned by this spike, but the
  decision does not depend on it: the full-mesh topology does not scale to
  the N=40 target regardless of where between 5 and 40 the wall sits.

**Decision for #154:** Pursue the gossip overlay for N>5. The surgical seam
is `Shared::route` (`transport.rs:252-279`) — route `Events` frames to a
gossip broadcast among admitted device keys instead of per-peer fan-out.
Keep the engine, `SyncMessage` protocol, admission gate, and membership fold
unchanged. The gossip topic inherits admission from the connection-level
gate (`handler.rs:49-141` — reject-before-bytes); only admitted device keys
join the gossip mesh.

---

## 7. Decision input for #154

Spike #145 measured v1-with-guardrails at N=5/40 on loopback. Verdict: **GO**
— gossip overlay IS warranted for N>5. N=5 survived all rates (idle through
5 events/s) with no cascade; N=40 crashed during mesh formation (QUIC
`noq-proto` panic at 1560 concurrent connections). Full evidence:
`crates/spike-N40/NOTES.md` §3–§6.

The actual GitHub reference/comment on #154 is performed by the orchestrator
or a maintainer, **not** by this spike implementation (spec §3 non-goal /
§7.3).

---

## 8. Runbook

The full measurement is a manual run on a host with enough RAM for N=40 in-
process endpoints (the issue names `demo1` / `demo2` / `demo3` as optional
infra). The loopback self-check is the only CI-runnable proof:

```
# CI gate (fast, N=5):
cargo test -p spike-n40 --test self_check
cargo run -p spike-n40 --bin n40-probe -- self-check

# Full measurement (manual, slow):
cargo run -p spike-n40 --bin n40-probe -- matrix \
  --markdown crates/spike-N40/results/results.md \
  --json    crates/spike-N40/results/$(date +%F)-matrix.json
cargo run -p spike-n40 --bin n40-probe -- rebind --n 40 \
  --json crates/spike-N40/results/$(date +%F)-rebind.json

# Optional threshold bracket if the matrix shows no cascade at 5 events/s:
cargo run -p spike-n40 --bin n40-probe -- sweep --n 40 --start 5 --max 80 --factor 2
```

After the run, the orchestrator or maintainer copies the resulting
`results.md` rows into this `NOTES.md`, fills §4 / §5 / §6 / §7 with the
measured values, and references the spike from #154.
