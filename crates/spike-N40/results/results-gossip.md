# `spike-N40` gossip-overlay matrix results

Rendered from `n40-probe matrix --connect-mode gossip` (loopback
`NetMode::Loopback`, no relay/discovery). Regenerate with the command
documented in [`README.md`](README.md).

> **Status: schema-only — the gossip matrix has not yet been re-run through the
> committed harness.** The prior gossip numbers in this file came from an
> uncommitted seed-only connect override that predates the `--connect-mode
> gossip` harness path, and the committed harness now derives `dial loops/node`
> from the live dial set (so those old cells are no longer reproducible). The
> CI-runnable proof that gossip mode builds and forms its bounded seed topology
> is `tests/self_check.rs::n5_cluster_reaches_gossip_readiness` at N=5. The full
> N=5/40 × {idle, 1, 5 events/s} gossip matrix is a manual run on a host with
> enough RAM for N=40 in-process endpoints; until it is executed and pasted
> here, every cell below holds the placeholder `—`.

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

| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | dial loops/node | writer+reader tasks/node est | connected entries | accepted min/max | frames_sent min/max | queue saturations | reconnects/sec | cascade? |
|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|
| 5 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 5 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 5 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |
| 40 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 40 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 40 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |
