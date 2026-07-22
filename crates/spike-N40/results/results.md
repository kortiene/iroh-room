# `spike-N40` matrix results

Rendered from `n40-probe matrix` (loopback `NetMode::Loopback`,
`RelayMode::Disabled`, `iroh = 1.0.1`). Regenerate with the command
documented in [`README.md`](README.md).

> **Status: schema-only — the full matrix has not yet been run.** The
> CI-runnable proof that the harness builds and the loopback harness works
> is `tests/self_check.rs` at N=5. The full N=5/10/20/40 ×
> {idle, 0.1, 1, 5 events/s} matrix is a manual run on a host with enough
> RAM for N=40 in-process endpoints (the issue names `demo1` / `demo2` /
> `demo3` as optional infra). Until that run is executed and its output
> pasted here, every cell below holds the placeholder `—`.

Caveats that apply to every row (spec §4 D1 / D3 / §6.6 / §13 risk 3,5,6):

- Over-cap transport allowlist (every node runs through the real
  `iroh_rooms_net::Node` with an `AllowlistAdmission` admitting every N
  endpoint devices); **not** a product-supported active-member room —
  `MAX_ACTIVE_MEMBERS = 5` is unchanged in shipping code.
- `rss_per_node_est` is derived from process RSS / N, not a true per-process
  measurement.
- `dial loops/node` is by construction (= `N - 1`); `writer+reader
  tasks/node est` is estimated from live connected peer entries.

| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | dial loops/node | writer+reader tasks/node est | connected entries | accepted min/max | frames_sent min/max | queue saturations | reconnects/sec | cascade? |
|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|
| 5 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 5 | 0.1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 5 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 5 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |
| 10 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 10 | 0.1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 10 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 10 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |
| 20 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 20 | 0.1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 20 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 20 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |
| 40 | idle | idle | — | — | — | — | — | — | — | — | — | — | — |
| 40 | 0.1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 40 | 1 | load | — | — | — | — | — | — | — | — | — | — | — |
| 40 | 5 | load | — | — | — | — | — | — | — | — | — | — | — |

## Column definitions (spec §7.2)

- **survives?** — `yes` / `degraded` / `no` per the rubric in `../NOTES.md`
  §3.
- **rss total MiB** — total process RSS at end of window, from
  `/proc/self/status` (`VmRSS:`).
- **rss/node est MiB** — `(process_rss - baseline_rss) / N`, rounded.
- **dial loops/node** — `N - 1` by construction (D2).
- **writer+reader tasks/node est** — `connected_peers × 2` (one writer + one
  reader per live peer link).
- **connected entries** — `connected / expected` directed peer entries
  (`expected = N × (N - 1)`).
- **accepted min/max** — min/max per-node `SyncCounters::accepted`.
- **frames_sent min/max** — min/max per-node `SyncCounters::frames_sent`.
- **queue saturations** — cluster-wide `transport.queue.saturated` audit
  events this window.
- **reconnects/sec** — `(connected + disconnected events since warmup) /
  window_secs`.
- **cascade?** — `yes` if any of the four D4 triggers fired this window,
  else `no`.
