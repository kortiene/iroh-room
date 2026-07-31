# `spike-N40` gossip-overlay matrix results

Rendered from `n40-probe matrix --connect-mode gossip` (loopback
`NetMode::Loopback`, no relay/discovery). Regenerate with the command
documented in [`README.md`](README.md).

> **Status: measured 2026-07-31** on `dgx-spark` (Ubuntu 24.04.4 LTS, aarch64,
> 20 cores, 121 GiB RAM), **release profile**, single-process loopback — no
> relay, no real NAT — with the corrected workload parent (see below). The
> structured run document for the four load legs is committed as
> [`2026-07-31-gossip-matrix.json`](2026-07-31-gossip-matrix.json); the two
> idle rows are carried from the
> [`2026-07-30`](2026-07-30-gossip-matrix.json) run unchanged (idle legs never
> exercise the workload, so the fix cannot affect them). One `n40-probe`
> process per `(N, rate)` cell, as before (the one-shot invocation still
> aborts on the port defect documented in the caveats).
>
> **The 2026-07-30 load rows are superseded: their collapse was a harness
> artifact, not an overlay failure.** The workload's first load event was
> admin-authored and parented on **genesis**, so the store derived
> `admin_seq = 1` for it — colliding with the membership fixture's invite #1
> (also genesis-parented, also `admin_seq = 1`; `store/mod.rs` keys the admin
> chain on *sender == admin* regardless of event type). Two distinct admin
> events at one `admin_seq` is exactly what `AdminForkDetected` exists to
> catch: every node that stored the first load event raised a CRITICAL
> equivocation, fail-closed every non-admin member, tore down links and
> dropped the gossip mesh as a revocation — and every 5 s re-subscribe then
> died against the admin's fail-closed accept gate (the observed
> `gossip.mesh.spawn_failed` storm, 44 ≈ 4 nodes × ~11 retries at the 5 s
> `GOSSIP_JOIN_TIMEOUT` cadence). The fix parents the load chain on the
> fixture's **final membership head** (member-authored, `admin_seq` NULL),
> which is also what real clients do — they parent on current heads, not
> genesis. An unpatched same-day control reproduced the collapse exactly;
> the diagnosis was adversarially verified end to end.
>
> **Corrected result: both sizes pass at 1 event/s; N=40 at 5 events/s fails
> with a real hub-overload cascade** — a different failure mode from the
> superseded rows (zero spawn failures, zero join timeouts, zero
> equivocations) and the genuine scaling limit this harness now measures.

Caveats that apply to every row (spec §4 D1 / D3 / §6.6 / §13 risk 3,5,6):

- Gossip mode spawns managed room sessions over a real active-membership fold
  (genesis + invite/join per node) and lets `PeerManager::desired_seeds` +
  `iroh-gossip` form the bounded seed topology, rather than dialing every
  ordered pair. `connected entries` are therefore `connected / (N × K)` warm
  seed links, not `N × (N - 1)` full-mesh entries.
- `rss_per_node_est` is `(process RSS − pre-spawn baseline RSS) / N`
  (`metrics.rs::cluster_metrics`), not a true per-process measurement — it is
  the cluster's *incremental* RSS spread over N, so it understates what a
  standalone node would report.
- `dial loops/node` is the live warm dial count (bounded by the seed selector),
  read from `Node::dial_count()`.
- **The documented one-shot matrix invocation aborts after its first row.**
  `cluster::loopback_port` derives a fixed port base of `30_000 + (seed_base %
  20_000)` while `matrix_seed(n, row_index)` advances `seed_base` by 1 per row,
  so consecutive rows overlap by `N - 1` ports — and `Node::shutdown()` returns
  before the endpoint socket is released, so row 2 dies on `Address already in
  use (os error 98)`. Reproduced in both debug and release. Each cell below was
  therefore measured in its own process with `--nodes <N> --rates <rate>`, which
  pins every leg to `row_index = 0` (N=5 `seed_base = 1548026112`, N=40
  `seed_base = 1550319872`) rather than to the distinct per-row seeds a single
  invocation would have used.
- **The fixed loopback port range overlaps the kernel ephemeral range.** N=40
  binds 49872–49911, inside this host's `ip_local_port_range` of 32768–60999, so
  an unrelated socket can steal a port mid-run; the N=40 idle leg lost port
  49882 on its first attempt and was re-run. A leg that aborts this way emits no
  row at all, so it cannot be mistaken for a measurement.
- `reconnects/sec` counts `connected` **and** `disconnected` audit transitions
  over the whole window, so initial topology formation inflates it (the N=40
  idle figure of 5.67/s is ≈170 transitions while 300 seed entries come up), and
  it is an aggregate while the D4 rubric samples every 5 s. No row tripped D4
  trigger 2.

| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | dial loops/node | writer+reader tasks/node est | connected entries | accepted min/max | frames_sent min/max | queue saturations | reconnects/sec | cascade? |
|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|
| 5 | idle | idle | yes | 40 | 7 | 3 | 7 | 18/15 | 0/0 | 0/0 | 0 | 0.20 | no |
| 5 | 1 | load | yes | 49 | 9 | 3 | 7 | 18/15 | 60/60 | 120/252 | 0 | 0.00 | no |
| 5 | 5 | load | yes | 52 | 9 | 3 | 7 | 18/15 | 300/300 | 612/1251 | 0 | 0.00 | no |
| 40 | idle | idle | yes | 229 | 6 | 3 | 15 | 300/120 | 0/0 | 0/0 | 0 | 5.67 | no |
| 40 | 1 | load | yes | 275 | 7 | 3 | 15 | 300/120 | 60/60 | 189/2692 | 0 | 0.00 | no |
| 40 | 5 | load | no | 944 | 23 | 3 | 5 | 101/120 | 100/300 | 4054/171538 | 28087 | 775.72 | yes |

## What the run shows

- **Idle holds at both sizes.** N=40 idle reaches 300/120 warm seed entries (the
  `GOSSIP_BOOTSTRAP_SEEDS = 3` floor is a per-node minimum, not a cap) at 229
  MiB total RSS — ~6 MiB per node — with no cascade trigger. That is a real
  advance over the full-mesh path recorded in [`../NOTES.md`](../NOTES.md)
  §4–§5, where N=40 could not form a mesh at all.
- **1 event/s passes cleanly at both sizes.** With the corrected workload
  parent, every node accepts every published event (60/60) at N=5 and N=40,
  connected entries hold at their idle values (18/15 and 300/120), dial loops
  stay at 3, and no cascade trigger fires. `gossip.mesh.spawn_failed`, join
  timeouts, and equivocations are all **zero**. This meets the Step 7
  acceptance thresholds ("no cascade at 1 event/s, connectedness >95%, delivery
  >95%") at both measured sizes — but the AC quantifies over N=10/20/40, and
  N=10/20 have never been run, so Step 7 itself remains **pending**, not
  passed.
- **5 events/s passes at N=5 and fails at N=40 with a hub-overload cascade.**
  The N=40 leg reaches full idle topology, then all four cascade triggers fire
  under load: 28,087 queue saturations, reconnect churn averaging 775.72/s
  (peak 1,769/s), connectedness and delivery both below 95%. The distribution
  is the telling part: **37 of 40 nodes accept the full 300/300** while exactly
  three stall at ~100 (nodes 17, 28, 35 — precisely the high-degree hubs that
  held full 39-peer connectivity in the idle phase, alongside publisher node 0,
  whose `frames_sent` of 171,538 dwarfs the ~4k of the chokers). Leaf nodes
  keep receiving; the hubs drown. The tracing signature is 20,605 iroh-gossip
  `failed to send` warns plus 1,122 event-plane `frame write failed; closing
  stream` — consistent with the cross-plane coupling in which gossip-delivered
  frames charge the same per-peer inbound byte budgets as the event plane, and
  a saturated budget causes the event-plane reader to **close the connection**
  (`crates/iroh-rooms-net/src/peer.rs:113-116`), shredding the mesh around the
  hub. That mechanism is identified from code reading and the failure
  signature; it has not been isolated by a dedicated experiment yet.
- **Not a queue-pressure artifact of the harness.** N=5 at the same 5 events/s
  is clean (300/300, zero saturations), and N=40 at 1 event/s is clean — the
  failure needs both the fan-in degree of N=40 hubs and the higher rate.

**Consequence for the 40-member cap.** The superseded 2026-07-30 conclusion
("the overlay loses topic membership under any load") is withdrawn: that was
the harness manufacturing an admin fork. The corrected matrix meets the
Step 7 thresholds at every **measured** N (the AC's N=10/20 legs remain
unrun, so the AC stays pending) — but a
40-member room at 5 events/s collapses its hub nodes, and 5 events/s is a
realistic busy-room rate. `MAX_ACTIVE_MEMBERS = 40` should stay held until the
hub overload is either fixed (e.g. decoupling gossip delivery from the
event-plane per-peer budgets, or spreading hub degree) or explicitly bounded
in the product (a documented sustained-rate ceiling for large rooms), and this
matrix is re-run clean at 5 events/s. Loopback-only still applies: no relay,
no real NAT, no internet-path evidence for the overlay at any size.
