# `spike-N40` gossip-overlay matrix results

Rendered from `n40-probe matrix --connect-mode gossip` (loopback
`NetMode::Loopback`, no relay/discovery). Regenerate with the command
documented in [`README.md`](README.md).

> **Status: measured 2026-07-30** on `dgx-spark` (Ubuntu 24.04.4 LTS, aarch64,
> 20 cores, 121 GiB RAM), **release profile** (`cargo build --release -p
> spike-n40 --bin n40-probe`), single-process loopback — no relay, no real NAT.
> Harness: `n40-probe matrix --connect-mode gossip --nodes 5,40 --rates
> idle,1,5`, with the structured run document committed as
> [`2026-07-30-gossip-matrix.json`](2026-07-30-gossip-matrix.json). The
> documented single-invocation form **cannot complete on this harness** (see the
> port caveats below), so the six legs were run one `n40-probe` process per
> `(N, rate)` cell and their six `ScenarioResult` records concatenated into that
> JSON unmodified. **Both N=5 and N=40 hold at idle and fail at every non-zero
> event rate**: the room-events gossip topic does not stay joined once the admin
> begins publishing, so `accepted` collapses to 1 on every non-authoring node.
> No queue saturation and no reconnect-churn trigger fired in any row — the
> cascade verdict in all four load rows is `connectedness_below_95pct_for_10s` +
> `delivery_below_95pct_for_2_windows`.

Caveats that apply to every row (spec §4 D1 / D3 / §6.6 / §13 risk 3,5,6):

- Gossip mode spawns managed room sessions over a real active-membership fold
  (genesis + invite/join per node) and lets `PeerManager::desired_seeds` +
  `iroh-gossip` form the bounded seed topology, rather than dialing every
  ordered pair. `connected entries` are therefore `connected / (N × K)` warm
  seed links, not `N × (N - 1)` full-mesh entries.
- `rss_per_node_est` is derived from process RSS / N, not a true per-process
  measurement.
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
| 5 | 1 | load | no | 39 | 7 | 0 | 0 | 1/15 | 1/60 | 2/4 | 0 | 0.08 | yes |
| 5 | 5 | load | no | 46 | 8 | 0 | 0 | 1/15 | 1/300 | 2/4 | 0 | 0.12 | yes |
| 40 | idle | idle | yes | 229 | 6 | 3 | 15 | 300/120 | 0/0 | 0/0 | 0 | 5.67 | no |
| 40 | 1 | load | no | 233 | 6 | 0 | 1 | 39/120 | 1/60 | 3/222 | 0 | 1.80 | yes |
| 40 | 5 | load | no | 243 | 6 | 0 | 1 | 38/120 | 1/300 | 3/683 | 0 | 1.80 | yes |

## What the run shows

- **Idle holds at both sizes.** N=40 idle reaches 300/120 warm seed entries (the
  `GOSSIP_BOOTSTRAP_SEEDS = 3` floor is a per-node minimum, not a cap) at 229
  MiB total RSS — ~6 MiB per node — with no cascade trigger. That is a real
  advance over the full-mesh path recorded in [`../NOTES.md`](../NOTES.md)
  §4–§5, where N=40 could not form a mesh at all.
- **Every load rate fails, at N=5 as well as N=40.** Each load leg first reaches
  its full idle topology during the pre-load idle sub-window (18/15 at N=5,
  300/120 at N=40) and then collapses once node 0 begins publishing: connected
  entries fall to 1/15 (N=5) and 38–39/120 (N=40), and every non-authoring node
  ends the window with `accepted = 1` against 60 or 300 published events. The
  harness logs the mechanism as repeated `reason="gossip.mesh.spawn_failed"
  error=timed out waiting to join the room events gossip topic` — 44 / 33
  occurrences on the N=5 legs and 337 / 263 on the N=40 legs — against
  `GOSSIP_JOIN_TIMEOUT` (`gossip.rs:69`, 5 s).
- **Not a queue-pressure failure.** `queue saturations` is 0 in every row and
  per-node `outbound_queue_bytes_sum` / `_max` stay at 0, so the byte-bounded
  queue guardrails are not the limiting factor here: the overlay loses topic
  membership, it does not back up. Nothing in this matrix reproduces the
  pre-`b0622ec` backlog collapse, and nothing in it shows a reconnect storm.
- **Not a profile or host artifact.** A debug-profile run of the same six legs
  on the same host returned the same six verdicts (idle `yes`, every load row
  `no`) with the same collapse signature, and a `--connect-mode full-mesh`
  control at N=5 / 1 event/s on the same release binary delivered 60/60 to all
  five nodes at 20/20 connected with no cascade
  (`| 5 | 1 | load | yes | 49 | 9 | 4 | 8 | 20/20 | 60/60 | 180/240 | 0 | 0.00 | no |`).
  The collapse is specific to the gossip overlay path.

**Consequence for the 40-member cap.** These rows do not support
`MAX_ACTIVE_MEMBERS = 40` over the gossip overlay. N=40 idle is evidence that
the bounded topology forms and that its memory footprint is tractable; no row
here shows a 40-member room actually carrying traffic, and the N=5 gossip rows
fail the same way, so the defect is not N=40-specific. Until the topic-join
failure is fixed and this matrix is re-run, 40-member support is unproven for
message delivery and must be caveated as such.
