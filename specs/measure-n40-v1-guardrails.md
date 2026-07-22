# Spec: Measure where v1 breaks at N=40

**Issue:** #145 — `[SPIKE] spike-N40: measure where v1 breaks at N=40`  
**Labels:** `type/spike` `area/transport` `priority/p1` `risk/low`  
**Owning area:** Transport spike harness; new throwaway workspace member `crates/spike-N40/`  
**Status:** Planning only. Do not implement in the same change that creates this spec.

---

## 1. Summary

Create a throwaway spike crate, `crates/spike-N40/`, that measures the current v1 full-mesh event transport with the post-`b0622ec` guardrails in place at N=5/10/20/40 loopback nodes. The spike answers the decision-record question that remains open after the old pre-guardrail N=25 failure: did bounded queues, quiescent ticks, early dedup/batching, and the #136 dial-stomp fix turn the failure mode from silent collapse into recoverable queue-close/reconnect churn, and at what room-wide event rate does that cascade begin?

The spike must not build a gossip overlay and must not modify shipping crates. It should mirror the existing throwaway spike posture used by `crates/spike-transport` and `crates/spike-nat`: `publish = false`, added to the workspace so CI proves it builds, with a deterministic loopback self-check and a `NOTES.md` decision memo containing the measured results and GO/NO-GO rubric for the #154 gossip-overlay decision.

---

## 2. Repository context read

Relevant current state:

- `README.md` describes the product as a local-first private room runtime and explicitly documents the small-room contract: `MAX_ACTIVE_MEMBERS = 5`, hard `RejectReason::RoomFull` on the 6th active join, near-cap warning/status output, and the pre-guardrail N=25 collapse from `PHASE-0-SPIKE.md`.
- `PHASE-0-SPIKE.md:38-49` records the old failure: at N=25, pre-guardrail, idle transport showed `frames_sent=0`, `accepted=0`, 661 MB inbound backlog, and under load 22 published events yielded `accepted=0` with 55,222 queued frames while connectivity still looked healthy.
- `PHASE-0-GO-NO-GO.md` and `crates/spike-transport/NOTES.md` ratify ADR-1 for N≤5: full-mesh direct QUIC remains the load-bearing Room Event Plane; gossip is parked unless larger-room measurements justify revisiting it.
- `crates/spike-transport/` is the closest spike template: `Cargo.toml` with `publish = false`, `src/{lib.rs,main.rs,...}`, `tests/self_check.rs`, `results/`, and `NOTES.md`.
- `crates/spike-nat/` is the second spike template for runbook/NOTES posture and CI loopback self-check.
- Root `Cargo.toml` already lists throwaway spike crates in the workspace so `scripts/verify.sh` covers them.
- `CONTRIBUTING.md` and `scripts/verify.sh` define the local gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets --all-features`, SDK doctests, and SDK examples.
- `crates/iroh-rooms-core/src/membership/model.rs` defines `MAX_ACTIVE_MEMBERS = 5`; the fold enforces the cap. This is a blocker for a literal active-membership N=40 test without a test-only override in core.
- `crates/iroh-rooms-net/src/queue.rs`, `peer.rs`, and `transport.rs` contain the guardrails this spike must exercise: byte-bounded priority queues, close-on-full/audit behavior, dial-with-backoff, and #136 guarded state transitions that avoid stale dial loops stomping live rebound links.
- `crates/iroh-rooms-net/src/node.rs` exposes public measurement seams: `Node::connect_to`, `disconnect_peer`, `peer_states`, `peer_entries`, `outbound_queue_depths`, `conn_events`, `publish`, `store_contains`, `counters`, and `shutdown`.
- `crates/iroh-rooms-core/src/sync/engine.rs` exposes `SyncCounters` with `accepted`, `frames_sent`, `parked`, `backfill_requests`, `early_duplicates`, `store_insert_batches`, and related post-guardrail counters.

---

## 3. Goals and non-goals

### Goals

1. Add `crates/spike-N40/` as an isolated throwaway workspace member, `publish = false`, with no dependency from shipping crates.
2. Stand up N=5/10/20/40 in-process loopback nodes and drive a configurable room-wide publish rate.
3. Exercise the current v1 event transport guardrails as closely as possible without modifying shipping crates.
4. Measure and report, for N=5/10/20/40 × `{idle, 0.1, 1, 5 events/s}`:
   - process RSS and derived per-node RSS estimate at idle and under load;
   - per-node task counts for dial loops, writer tasks, and reader tasks;
   - accepted event counters and frames-sent counters;
   - outbound queue depths in bytes;
   - queue saturation events and link closes;
   - reconnect churn per second;
   - connectedness/convergence status.
5. Identify the first configured rate, or optional sweep rate, where the queue-close-on-full cascade begins.
6. Measure N=40 convergence time after a rebind / NAT-drop simulation.
7. Produce `crates/spike-N40/NOTES.md` with a decision memo answering:
   - Does N=40 idle survive?
   - At what rate does the cascade begin?
   - Is gossip warranted now, or does v1-with-guardrails hold to approximately 40?
8. Include a #154 decision-input block ready for the orchestrator/human to reference from #154. This phase must not run `gh` or comment on GitHub.

### Non-goals

- Do not build the gossip overlay (#154).
- Do not change production/shipping crates (`iroh-rooms-core`, `iroh-rooms-net`, `iroh-rooms-cli`, `iroh-rooms`).
- Do not raise or make configurable the product `MAX_ACTIVE_MEMBERS = 5` invariant in shipping code.
- Do not change wire formats, `SyncMessage`, admission semantics, queue semantics, or CLI behavior.
- Do not add deployment automation for `demo1`/`demo2`/`demo3`. Optional manual infra runs may use those hosts later, but the required harness is loopback.
- Do not perform Git/GitHub operations in this phase.

---

## 4. Key design decisions

### D1 — Use a spike-local over-cap harness instead of modifying `MAX_ACTIVE_MEMBERS`

The issue asks for a multi-node loopback harness that "lifts `MAX_ACTIVE_MEMBERS` via a test-only override," while also saying the spike has no dependencies and does not modify shipping crates. In the current code, `MAX_ACTIVE_MEMBERS` is a shipping-core constant and the membership fold enforces it.

Recommended implementation for this spike:

- Do **not** alter `iroh-rooms-core`.
- Construct a spike-local harness that uses `Node::spawn` with an `AllowlistAdmission` admitting all N endpoint devices, and explicitly calls `Node::connect_to` to form the over-cap transport mesh.
- Seed each node's `SyncEngine` with a single-admin room genesis, then publish load events from the admin node only. This keeps event validation/fold behavior real while avoiding >5 active membership joins.
- Treat N as "transport participants" / "over-cap devices" in reports, not as a product-supported active-member room.

This exercises the post-guardrail event transport paths (`NetTransport`, `peer::dial_loop`, byte queues, pump, `SyncEngine` counters) without changing production code. It is slightly less faithful than a true active-membership N=40 room because non-admin nodes are transport-admitted by the spike allowlist rather than by a >5 membership snapshot. `NOTES.md` must state this caveat plainly.

If maintainers decide the result must use a literal >5 `MembershipSnapshot`, that requires a separate, explicitly-approved test-only override in `iroh-rooms-core`; that would be out of scope for this issue as written.

### D2 — Match managed v1 dial pressure: every node dials every other node

Current `PeerManager::desired_devices` wants every active member device except self, so managed sessions start a dial loop from each node to each other node. The spike should mirror that pressure:

- For every ordered pair `(i, j)` where `i != j`, call `nodes[i].connect_to(nodes[j].endpoint_addr())`.
- Await a stabilization condition before measurements: each node sees all `N-1` peers in `PeerConnState::Connected`, or the readiness timeout expires and the run records the partial connectedness.
- Count per-node dial loops as `N-1` expected loops. Writer/reader task counts are observed/estimated from connected peer entries: one writer and one reader per live peer link registered by this node.

This intentionally stresses dial-loop churn and duplicate/crossed connections in the same shape as the v1 manager.

### D3 — Per-node RSS is an in-process estimate, not an OS RSS measurement

All nodes run in one process, so Linux cannot report exact RSS per node. The spike must report:

- total process RSS from `/proc/self/statm` at baseline, after cluster spawn, after idle stabilization, and during load;
- derived per-node RSS estimate: `(process_rss - baseline_rss) / N`;
- optional incremental spawn deltas if nodes are created sequentially.

Tables should label this as `rss_per_node_est`, not true per-process RSS. If exact per-node RSS becomes required, that is a separate multiprocess harness.

### D4 — Cascade start is defined by observable recovery signals, not just delivery failure

A queue-close-on-full cascade begins at the first rate where any of these hold during the steady load window:

1. At least one `transport.queue.saturated` audit event occurs.
2. Reconnect churn exceeds `1.0 reconnect/sec` cluster-wide for at least two consecutive 5-second sample windows after initial warmup.
3. Connectedness stays below 95% of expected peer entries for at least 10 seconds.
4. Accepted-event delivery falls below 95% of expected recipients for two consecutive sample windows and does not recover by the end of the run.

The primary matrix only has rates `{0.1, 1, 5}` events/s. If none of those rates crosses the cascade threshold, the memo records "not observed up to 5 events/s" and may run an optional `sweep` to bracket the true threshold above 5.

### D5 — Event rate is room-wide and admin-authored

Use a room-wide publish rate, not per-node rate:

- `0.1 events/s` means one event every 10 seconds across the whole cluster.
- `1 events/s` means one event per second across the whole cluster.
- `5 events/s` means five events per second across the whole cluster.

To avoid requiring >5 active authors, all load events are `message.text` events authored by the genesis admin on node 0, with a linear `prev_events` chain. The harness should still measure room-wide fanout and backfill behavior because every accepted event is synchronized through the mesh.

### D6 — Keep CI self-check small; keep the full matrix manual/ignored

CI should prove that the crate builds and the loopback harness works without running the resource-heavy N=40 matrix:

- `tests/self_check.rs` should run N=5 with a short low-rate burst and assert convergence, nonzero `frames_sent`, nonzero accepted counts, valid JSON/Markdown rendering, and a successful mini rebind.
- The full N=5/10/20/40 × rate matrix should be run manually via the `n40-probe matrix` binary and committed as results artifacts.
- Any `#[ignore]` full-matrix test is optional; if added, document how to run it.

---

## 5. Crate layout

Create:

```text
crates/spike-N40/
  Cargo.toml
  NOTES.md
  src/
    lib.rs
    main.rs
    cluster.rs
    workload.rs
    metrics.rs
    report.rs
    rss.rs
  tests/
    self_check.rs
  results/
    README.md
    results.md
```

Recommended package settings:

```toml
[package]
name = "spike-n40"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
publish = false

[lib]
name = "spike_n40"
path = "src/lib.rs"

[[bin]]
name = "n40-probe"
path = "src/main.rs"

[lints]
workspace = true
```

Recommended dependencies:

- `iroh = "=1.0.1"` — same pin as the net crate/spikes.
- `iroh-rooms-core = { path = "../iroh-rooms-core", features = ["sync", "store"] }` if the `store` feature is required directly; otherwise use the same feature set already used by `iroh-rooms-net`.
- `iroh-rooms-net = { path = "../iroh-rooms-net" }` — this is acceptable because the spike depends on shipping crates; shipping crates must not depend on the spike.
- `tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }`
- `anyhow = "1"`
- `serde = { version = "1", features = ["derive"] }`
- `serde_json = "1"`
- `tracing = "0.1"`
- `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }`

Update root `Cargo.toml` workspace members with a comment matching existing spike style. The path should use the requested directory casing, `"crates/spike-N40"`; the package name remains lowercase `spike-n40` and the Rust lib target `spike_n40`.

---

## 6. Harness design

### 6.1 Public CLI

Binary: `n40-probe`

```text
n40-probe self-check [--json]
n40-probe matrix [--nodes 5,10,20,40] [--rates idle,0.1,1,5]
                 [--idle-secs 30] [--load-secs 60] [--low-rate-secs 120]
                 [--warmup-secs 10] [--json results/<name>.json]
                 [--markdown results/results.md]
n40-probe sweep --n 40 --start 0.1 --max 20 --factor 2
                [--load-secs 60] [--warmup-secs 10]
n40-probe rebind --n 40 [--missed-events 10] [--rate 0.1]
                  [--json results/rebind.json]
```

Behavior:

- `self-check`: small N=5 run suitable for CI and `tests/self_check.rs` parity.
- `matrix`: required acceptance matrix.
- `sweep`: optional threshold bracketing if no cascade is observed at 5 events/s or if the cascade begins below 0.1 and needs finer resolution.
- `rebind`: N=40 rebind/NAT-drop simulation.

The binary should print Markdown by default and JSON with `--json`. JSON should be one structured document per run, not ad-hoc logs.

### 6.2 Data model

Define these core structs in `report.rs` / `metrics.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ScenarioKind {
    Idle,
    Load,
    Sweep,
    Rebind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioConfig {
    pub n: usize,
    pub rate_events_per_sec: Option<f64>,
    pub warmup_secs: u64,
    pub measure_secs: u64,
    pub seed_base: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub node_index: usize,
    pub connected_peers: usize,
    pub expected_peers: usize,
    pub dial_loop_tasks: usize,
    pub writer_tasks_est: usize,
    pub reader_tasks_est: usize,
    pub outbound_queue_bytes_sum: usize,
    pub outbound_queue_bytes_max: usize,
    pub accepted: u64,
    pub frames_sent: u64,
    pub parked: u64,
    pub backfill_requests: u64,
    pub early_duplicates: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMetrics {
    pub process_rss_bytes: u64,
    pub rss_per_node_est_bytes: u64,
    pub total_connected_peer_entries: usize,
    pub expected_connected_peer_entries: usize,
    pub reconnects_per_sec: f64,
    pub queue_saturations: usize,
    pub accepted_min: u64,
    pub accepted_max: u64,
    pub frames_sent_min: u64,
    pub frames_sent_max: u64,
    pub nodes: Vec<NodeMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub kind: ScenarioKind,
    pub config: ScenarioConfig,
    pub idle: Option<ClusterMetrics>,
    pub load: Option<ClusterMetrics>,
    pub published_events: usize,
    pub cascade: CascadeVerdict,
    pub notes: Vec<String>,
}
```

`CascadeVerdict` should include booleans for each trigger in D4 and a final `began: bool`.

### 6.3 Recording audit sink

Implement a spike-local `RecordingAudit` that implements `iroh_rooms_net::AuditSink` and records at least:

- `accepted`, `connected`, `disconnected`, `offline`, `rejected` counts;
- `transport_queue_saturated(device, queue)` events by queue (`inbound`/`outbound`);
- timestamps (`Instant`) for reconnect-rate calculation.

Do not record event bodies, message text, invite secrets, capability secrets, blob data, local filesystem paths, or private keys.

### 6.4 Cluster construction

`cluster.rs` should expose:

```rust
pub struct HarnessCluster {
    pub room_id: RoomId,
    pub nodes: Vec<HarnessNode>,
    pub admin: Principal,
    pub genesis_id: EventId,
}
```

Implementation steps:

1. Deterministically derive N principals from `seed_base` using the same shape as existing tests: identity signing key + device signing key; device seed also produces the `iroh::SecretKey` so `EndpointId == device_id`.
2. Build a single `room.created` genesis event authored by node 0/admin.
3. For each node:
   - open an in-memory `EventStore`;
   - open a `SyncEngine` for the room;
   - publish/seed the genesis into the local engine;
   - build `AllowlistAdmission` that binds every node device to an identity and marks those identities active for transport admission only;
   - spawn `Node::spawn` with `NetConfig { mode: NetMode::Loopback, ..Default::default() }`, a short tick (100–250 ms), and the shared/individual `RecordingAudit`.
4. After all nodes have endpoint addresses, call `connect_to` for every ordered pair `(i, j), i != j`.
5. Await readiness for up to a configurable timeout. Readiness is all nodes reporting `N-1` connected peers; if not reached, record partial readiness and continue only for negative-evidence runs where useful.

Use the same deterministic room nonce/time base across runs only if room IDs cannot collide in one process. Otherwise derive nonce from `seed_base` and scenario label.

### 6.5 Workload generation

`workload.rs` should create signed `message.text` `WireEvent`s from admin node 0:

- Keep a linear `prev_events` chain: first message parent is `genesis_id`; each following message parent is the previous message id.
- Use deterministic timestamps and body text such as `n40 load seq=<k>`.
- Pre-generate event bytes and ids for each scenario when possible so the expected set is known.
- Publish with `Node::publish` at the configured room-wide rate.
- For `idle`, publish no messages after genesis and sample metrics for the idle window.

Recommended durations:

- CI self-check: N=5, 5–10 events, short sleeps, total under ~30s.
- Full matrix default:
  - idle: 30s after readiness;
  - 0.1 events/s: 120s so at least 12 events are generated;
  - 1 events/s: 60s;
  - 5 events/s: 60s.

### 6.6 Metrics sampling

Sampling should occur:

1. at process baseline before node spawn;
2. after all nodes are spawned and connected;
3. after the idle window;
4. every 5 seconds during load;
5. at final load window end;
6. before and after rebind simulation.

Metrics sources:

- RSS: `/proc/self/statm` × page size. Implement Linux-only `rss.rs`; return a clear error if not on Linux.
- Connectedness: `Node::peer_states()` / `peer_entries()`.
- Queue bytes: `Node::outbound_queue_depths()`.
- Counters: `Node::counters().await`.
- Reconnect churn: `Node::conn_events()` streams plus `RecordingAudit` connected/disconnected events. Exclude initial bring-up from reconnect/sec by resetting counters after warmup.
- Saturation: `RecordingAudit::transport_queue_saturated` count.
- Task counts:
  - `dial_loop_tasks = N - 1` per node by construction;
  - `writer_tasks_est = connected_peers`;
  - `reader_tasks_est = connected_peers`;
  - include `task_count_is_estimated: true` in JSON because writer/reader task handles are private inside `iroh-rooms-net`.

If exact task handles are needed, that requires instrumentation in shipping net code and is out of scope.

### 6.7 Matrix run

For each N in `{5,10,20,40}`:

1. Build the cluster.
2. Wait for readiness and record initial task/connectedness metrics.
3. Run idle scenario and record idle metrics.
4. For each rate in `{0.1,1,5}`:
   - reset per-scenario audit/event counters;
   - run a warmup period if needed;
   - publish admin-authored events at the configured room-wide rate;
   - sample every 5 seconds;
   - record final metrics;
   - compute `CascadeVerdict` using D4.
5. Shut down nodes cleanly.

Run each N in a fresh cluster. Do not reuse a cluster across rates unless the code can prove the previous load left no backlog/churn; fresh clusters keep rows comparable.

### 6.8 Rebind / NAT-drop scenario at N=40

Goal: measure convergence after a node disappears and returns with the same device identity on a new loopback port while existing peers may still have stale dial loops.

Recommended simulation:

1. Build N=40 cluster and wait for readiness.
2. Publish a small baseline set and wait until every node contains those ids.
3. Choose target node 39 as the rebinding node.
4. Shut down target node 39. Keep its deterministic principal/secret and remember the event ids it had before shutdown.
5. While node 39 is offline, publish `missed_events` admin messages from node 0.
6. Respawn node 39 with the same `SecretKey`, same room id, and an in-memory store seeded with the baseline events it knew before shutdown (not with the missed events).
7. Have the rebound node dial all existing nodes with their current endpoint addresses. Existing nodes' old outbound dial loops may still be attempting the stale address; this is the #136-shaped condition.
8. Start timer at rebound spawn/dial.
9. Measure time until:
   - node 39 contains every missed event id;
   - node 39 sees enough connected peers to receive backfill (record exact connected count);
   - existing peers do not permanently stomp the live inbound rebound link away from `Connected`.
10. Record timeout/failure if convergence does not happen within 60s.

Also add a lighter `disconnect_peer` drop test if useful, but the respawn-with-same-secret case is the decision-relevant rebind/NAT-drop simulation.

---

## 7. Reporting and artifacts

### 7.1 JSON artifacts

Write JSON artifacts under `crates/spike-N40/results/`, one file per full run, for example:

```text
results/2026-07-22-loopback-matrix.json
results/2026-07-22-loopback-rebind.json
```

Include:

- crate version and binary args;
- OS/platform;
- `iroh` version;
- workspace package versions if available;
- note that the run is loopback `NetMode::Loopback`, no relay/discovery;
- all `ScenarioResult`s;
- caveats from D1/D3/D6.

Do not include hostnames beyond optional operator-supplied labels; do not include secrets.

### 7.2 Markdown table

`results/results.md` must contain at least the required matrix:

| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | dial loops/node | writer+reader tasks/node est | connected entries | accepted min/max | frames_sent min/max | queue saturations | reconnects/sec | cascade? |
|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|

Rows required:

- N=5 idle, 0.1, 1, 5
- N=10 idle, 0.1, 1, 5
- N=20 idle, 0.1, 1, 5
- N=40 idle, 0.1, 1, 5

`survives?` should be `yes`, `degraded`, or `no`:

- `yes`: no cascade triggers, expected connectedness reached/restored, delivery converged.
- `degraded`: transient reconnect/saturation occurred but delivery recovered by end.
- `no`: cascade trigger persisted, delivery did not recover, or run timed out.

### 7.3 `NOTES.md` decision memo

`crates/spike-N40/NOTES.md` should include:

1. What was measured and why.
2. Caveats:
   - over-cap transport allowlist rather than literal >5 active membership fold;
   - per-node RSS is derived from process RSS;
   - writer/reader task counts are estimated from live peer entries unless exact instrumentation is later added.
3. The results table or link to `results/results.md`.
4. Rebind/NAT-drop result at N=40.
5. Cascade threshold statement:
   - first configured rate where cascade begins, or
   - not observed up to 5 events/s, with optional sweep bracket.
6. GO/NO-GO rubric evaluation for #154.
7. A "Decision input for #154" block ready to paste/reference, for example:

```md
### Decision input for #154

Spike #145 measured v1-with-guardrails at N=40 on loopback. Verdict: <GO/NO-GO/CONDITIONAL>. N=40 idle <survived/did not survive>; cascade begins at <rate or not observed ≤5 events/s>; rebind convergence at N=40 was <duration/failure>. Full evidence: `crates/spike-N40/NOTES.md` and `crates/spike-N40/results/results.md`.
```

The actual GitHub reference/comment is performed by the orchestrator or a maintainer, not by this spike implementation.

---

## 8. GO/NO-GO rubric for gossip-overlay decision

Use the measured data to classify #154 as follows.

### GO: v1 holds to approximately 40; gossip not warranted now

All of:

- N=40 idle survives the full idle window with no queue saturation and no persistent reconnect churn.
- N=40 at 0.1 events/s and 1 events/s converges with no cascade triggers.
- N=40 at 5 events/s either converges cleanly or degrades transiently but recovers without sustained reconnect churn.
- Rebind/NAT-drop at N=40 converges within 30s.
- RSS and task-count growth are explainable as O(N²) and do not show unbounded backlog growth during idle.

Decision: keep the hard product cap unless separately changed, but do not build gossip solely for N≈40 transport pressure.

### CONDITIONAL: v1 guardrails avoid silent collapse but N=40 has a low load ceiling

Any of:

- N=40 idle survives, but cascade begins at 1–5 events/s.
- N=40 0.1 events/s survives, but 1 events/s is degraded/no.
- Rebind converges but takes 30–60s or requires inbound rebound links to recover from stale dials.
- Queue close/reconnect is visible and recoverable, not silent, but operator experience at 40 would be poor.

Decision: use this as a real input to #154; gossip may be warranted if product wants N≈40 rooms with event rates above the measured ceiling.

### NO-GO: gossip or a different topology is warranted for N≈40

Any of:

- N=40 idle does not survive.
- Cascade begins at or below 0.1 events/s.
- Failure is still silent: delivery stalls while connectedness appears healthy and no saturation/reconnect signal explains it.
- Rebind/NAT-drop fails to converge within 60s.
- RSS/backlog grows without bound during idle or low load.

Decision: #154 should proceed or the product must keep ≤5 as a strict non-negotiable limit with no N≈40 ambition.

---

## 9. Testing strategy

### Unit tests

- `rss.rs`: parse representative `/proc/self/statm` lines and compute RSS bytes.
- `report.rs`: serialize/deserialize `ScenarioResult`, render Markdown rows, and classify `CascadeVerdict` triggers.
- `workload.rs`: generated message chain has correct parent linkage and deterministic event ids.
- `metrics.rs`: reconnect/sec excludes initial warmup and cascade classification matches D4.

### Integration/self-check tests

`crates/spike-N40/tests/self_check.rs`:

1. Spawn N=5 loopback cluster.
2. Assert readiness reaches all peers within a bounded timeout.
3. Publish a short admin-authored burst.
4. Assert every node contains every published id.
5. Assert at least one node has `frames_sent > 0` and every node has `accepted >= published + genesis` as applicable.
6. Assert `outbound_queue_depths` are readable and byte-valued.
7. Run a mini rebind with N=5 and assert the rebound node catches missed events.
8. Render JSON and Markdown without panic.

Keep the self-check deterministic and fast enough for `cargo test --workspace --all-targets --all-features`.

### Manual/full-matrix verification

After implementation, run:

```bash
cargo run -p spike-n40 --bin n40-probe -- self-check
cargo run -p spike-n40 --bin n40-probe -- matrix --markdown crates/spike-N40/results/results.md --json crates/spike-N40/results/<date>-matrix.json
cargo run -p spike-n40 --bin n40-probe -- rebind --n 40 --json crates/spike-N40/results/<date>-rebind.json
```

Then run the standard gate:

```bash
scripts/verify.sh
```

If the full matrix is too heavy for the local machine, optionally run the same commands on one of the provided hosts (`demo1`, `demo2`, `demo3`) after copying the workspace or using the orchestrator's infra process. The loopback self-check must still pass locally/CI.

---

## 10. Security, privacy, and reliability considerations

- The spike uses deterministic test keys only; never read real user identities, data dirs, invite tickets, or blob stores.
- Audit/report artifacts must not include private keys, capability secrets, invite tickets, message bodies beyond synthetic sequence labels, local paths outside the results path, or raw frame bytes.
- The harness intentionally exceeds the product room-size cap. Every output must label this as unsupported measurement, not a product recommendation.
- The optional demo hosts are root SSH targets supplied by the issue. Do not bake their names into required CI paths or results unless a manual run actually uses them.
- Use bounded timeouts on every readiness/convergence wait. A bad N=40 run must fail with a measured timeout, not hang.
- Cleanly call `Node::shutdown` for every node at the end of each scenario to avoid leaked endpoint tasks affecting later rows.
- Run each matrix row in a fresh process or fresh cluster when possible so RSS/backlog from previous rows does not contaminate the result.

---

## 11. Rollout and rollback

This is a spike-only workspace member.

Rollout:

1. Add `crates/spike-N40/` and root workspace member entry.
2. Add CI-runnable self-check.
3. Commit generated `results/README.md`; commit `results/results.md` and JSON artifacts only after the full measurement is run.
4. Fill `NOTES.md` with the decision memo.

Rollback:

- Remove the root workspace member entry and delete `crates/spike-N40/`.
- No migration or production rollback is needed because no shipping crates or persisted user data are changed.

---

## 12. Acceptance criteria mapping

| Issue acceptance | Spec coverage |
|---|---|
| Spike builds in CI and loopback self-check passes | Workspace member + `tests/self_check.rs` (§5, §9) |
| Results table covers N=5/10/20/40 × `{idle, 0.1, 1, 5 events/s}` | `n40-probe matrix`, `results/results.md` schema (§6.7, §7.2) |
| `NOTES.md` records N=40 idle survival, cascade rate, gossip warranted decision | `NOTES.md` decision memo + GO/NO-GO rubric (§7.3, §8) |
| Spike is referenced from #154 as decision input | `NOTES.md` includes a ready-to-reference #154 block; orchestrator/maintainer performs GitHub action (§7.3) |
| Dependencies none / no shipping crate changes | D1, non-goals, rollback (§3, §4.1, §11) |

---

## 13. Risks

1. **Literal `MAX_ACTIVE_MEMBERS` override conflicts with "no shipping crate changes."** Recommended mitigation is the spike-local over-cap transport allowlist in D1; record caveat in `NOTES.md`.
2. **In-process RSS cannot be truly per-node.** Report total RSS and `rss_per_node_est`; do not overclaim.
3. **Private task handles are not observable.** Report dial-loop counts by construction and writer/reader estimates from connected peer entries; do not overclaim exact Tokio task counts.
4. **N=40 all-directed dial loops may be resource heavy.** Use bounded timeouts, fresh clusters, and optional demo hosts for full matrix runs.
5. **Admin-only workload may understate multi-author causal/backfill pressure.** It is the safest no-shipping-change workload. If maintainers require multi-author >5 membership pressure, that needs a separate approved test-only core override.
6. **Loopback differs from real NAT.** This spike measures topology/queue pressure, not internet traversal. Rebind/NAT-drop is simulated by endpoint shutdown/respawn on loopback.

---

## 14. Assumptions and open questions

### Assumptions

- The decision input needed for #154 is transport/queue/reconnect behavior at N≈40, not a product-supported active-membership room.
- Room-wide event rates are intended, not per-node rates.
- Loopback measurement is sufficient for this spike because the issue explicitly asks for in-process loopback nodes.
- The implementation may depend on shipping crates, but shipping crates must not depend on the spike and must not be modified.

### Open questions

1. Should maintainers approve a true `MAX_ACTIVE_MEMBERS` test-only override in `iroh-rooms-core`, despite the issue's "Out: changes to shipping crates" statement?
2. What exact duration should define "N=40 idle survives" for the final memo: 30s, 5min, or 10min? This spec recommends 30s default for practical matrix runs, with longer optional confirmation if resources allow.
3. Is a 95% delivery/connectedness threshold the right degraded/no boundary, or should the memo require 100% convergence at every rate?
4. Should the optional sweep continue beyond 5 events/s if the required matrix shows no cascade, or is "not observed ≤5 events/s" enough for #154?
5. If the full matrix only runs on `demo1`/`demo2`/`demo3`, what host metadata should be recorded without leaking unnecessary operational details?
