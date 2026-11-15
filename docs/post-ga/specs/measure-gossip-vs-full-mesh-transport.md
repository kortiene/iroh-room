# Measure Gossip vs Full-Mesh Transport (ADR-1 confirmation) — IR-0006 / #10

**Status:** Landed. Implemented as `crates/spike-transport` (`transport-probe`);
findings and the decision memo are in `crates/spike-transport/NOTES.md`. ADR-1
is ratified — see `PHASE-0-SPIKE.md`.
**Type:** spike / decision (`type/spike`, `type/decision`, `area/transport`, `priority/p1`, `risk/medium`).
**Parent:** #1 (Phase 0 epic).
**Depends on:** #6 / IR-0002 (canonical signed event model — landed), #9 / IR-0005
(full-mesh QUIC event transport — landed).

---

## 1. Summary

`PHASE-0-SPIKE.md` **ADR-1** already *recommends* full-mesh direct QUIC over a custom
ALPN for the Room Event Plane and *rejects* `iroh-gossip` for the load-bearing log, with
gossip parked as an optional best-effort liveness / admin-tip carrier. That
recommendation was made on **adversarial design review, not measured code** (Residual
Risk 12: "D1/D2 are recommended, not measured-yet"). Spike Plan **Day 4** is the task that
must *"Build both, minimally … Decision must cite measured numbers."*

This spike builds a throwaway `crates/spike-transport` harness that stands up **both**
backends behind one trait, drives the **same signed `WireEvent` payloads** through each
over N=2..5 in-process nodes, and measures the five ADR-1 comparison dimensions —
**latency, reconnect behavior, late-join behavior, auth/admission model, and
implementation complexity**. It confirms (or disproves) each load-bearing ADR-1 claim in
code and produces a **decision memo** that either ratifies ADR-1 or flips it, and — as a
by-product — resolves **Residual Open Decision 13** (the admin-tip advertisement carrier:
mesh pull RPC vs. optional gossip liveness channel vs. both).

The expected outcome is that ADR-1 **holds** (the landed full-mesh carrier already proves
the mesh side; see §4), but the deliverable is the *measured evidence and written memo*,
not a re-litigation of the design. This document plans that work; it does **not** implement
it and changes no shipping code.

---

## 2. Goal and non-goals

### Goal

- Provide a minimal, apples-to-apples comparison of full-mesh direct QUIC and
  `iroh-gossip` as Room Event Plane transports at N=2..5, on measured local evidence.
- Confirm the specific ADR-1 claims that were asserted but not yet measured:
  - mesh gives **native authenticated admission** (reject unknown `EndpointId` before any
    event byte); gossip's open topic gives **none**;
  - gossip has **no history** — a late joiner receives nothing sent before it subscribed
    (quantify the gap the sync layer must fill);
  - at N≤5 gossip's epidemic fan-out / partial-view membership **buys nothing** over a full
    mesh, while costing ordering, auth, history, and a 0.x dependency.
- Evaluate gossip **only** as an optional liveness / admin-tip advertisement carrier
  (off the critical path) and answer Residual Open Decision 13.
- Emit a **decision memo** ratifying or flipping ADR-1, linked from the repo docs, in a
  form that can be pasted into issue #10.

### Non-goals

- **Not** a real-NAT measurement. Latency/reconnect/late-join are measured on **loopback**
  in-process nodes (`RelayMode::Disabled`), which satisfies the AC "measured or simulated
  locally." Real-NAT hole-punching is **Gate A** (IR-0012 / `crates/spike-nat`), a separate
  owed run; this spike does not touch it.
- **Not** a new sync/ordering/history implementation. Ordering, dedup, causal fold, and
  backfill are the landed `iroh-rooms-core` layers' job (ADR-2) and are transport-agnostic;
  this spike measures only what the *transport* does and does not provide.
- **Not** shipping code. The gossip backend is throwaway and must never enter the shipping
  dependency tree (§5). The landed full-mesh carrier (`iroh-rooms-net`) is **not modified**.
- **Not** a large-swarm evaluation. Scope is fixed at N≤5 (PRD §17.1.13); the O(n²) mesh
  ceiling is an accepted, documented trade, not something this spike re-opens.

---

## 3. Traceability

| Source | What it requires | Where this spec addresses it |
|---|---|---|
| ADR-1 (`PHASE-0-SPIKE.md` §"Decision Records") | Adopt full-mesh, reject gossip for the log; confirm by measurement | §6–§9 (measure all five dimensions), §9 (memo ratifies/flips) |
| Spike Plan **Day 4** (`PHASE-0-SPIKE.md`) | Build both minimally behind a trait; deliverable `spike-transport` + comparison memo; decision must cite measured numbers | §5, §7, §9, §10 |
| Residual **Open Decision 13** | Admin-tip advertisement carrier: mesh pull RPC vs. gossip vs. both | §7.7, §9 |
| Residual **Risk 12** | D1 is recommended, not measured-yet | Entire spec closes this for D1 (transport) |
| Dependency **#6 / IR-0002** | Signed event model / `WireEvent` / event-id | §7.5 (shared signed payload workload), §7.6 (set-equality oracle) |
| Dependency **#9 / IR-0005** | Full-mesh QUIC carrier | §4 (reuse landed evidence), §7.2 |

---

## 4. Background — what already exists, and why the spike is still needed

- **The full-mesh side is largely already built and proven — on loopback.** `iroh-rooms-net`
  (IR-0005 / #9) is the *shipping* full-mesh carrier: `NetTransport` = an `iroh::Endpoint` +
  `Router` on ALPN `/iroh-rooms/event/1`, admission-before-bytes (a non-member's connection
  is closed **before** `accept_bi()`), a `PeerConnState` trichotomy (connected / offline /
  unauthorized), per-peer bidi framing, and a dial-with-backoff reconnect loop. Its
  `tests/loopback.rs` T1–T9 already demonstrate AC-relevant behavior: **T2** proves
  admission-before-bytes; **T4** proves reconnect. So AC "mesh admission-control confirmed"
  can lean on landed, conformance-tested evidence — the spike re-demonstrates it minimally
  and *cites* IR-0005 rather than re-implementing it.
- **The gossip side does not exist anywhere.** No crate depends on `iroh-gossip`
  (`grep iroh-gossip crates/*/Cargo.toml` → nothing). This is the genuinely new build and
  the real risk-bearing part of the spike.
- **The set-equality oracle already exists.** The sans-IO `SyncEngine` exposes
  `SyncDigest { event_ids: BTreeSet<EventId>, admin_tip, snapshot }` via `digest()`
  (`crates/iroh-rooms-core/src/sync/engine.rs`). "Event set equality" from the issue Test
  Plan is exactly `BTreeSet<EventId>` equality across nodes — the spike collects each node's
  received-id set and compares.
- **The throwaway-spike pattern is established.** `crates/spike-nat` (`nat-probe`, IR-0012)
  and `crates/spike-blobs` (IR-0009) are the template: `publish = false`, does **not**
  inherit the workspace `rust-version`, isolated from the shipping dep tree, a `lib` + a
  `[[bin]]`, a `NOTES.md` findings doc, a `results/` artifact directory, and a deterministic
  loopback self-check that CI runs (CI proves it builds and self-checks; it cannot prove the
  network claim). This spike mirrors that pattern exactly.
- **Ordering/history/auth are the log layer's job, not the transport's — for both options.**
  ADR-1's honest framing: *neither* raw transport provides room-wide ordering or history.
  The difference is that mesh gives an **authenticated** ordered link over which a backfill
  pull is "just another frame on a connection we already hold," whereas gossip gives an
  **unauthenticated** unordered firehose with no per-peer connection state to even attach a
  pull to. The spike measures this difference concretely, it does not re-argue it.

---

## 5. Owning component and where the code lives

**New throwaway crate: `crates/spike-transport/`.** It mirrors `spike-nat` / `spike-blobs`:

```text
crates/spike-transport/
  Cargo.toml            # publish = false; does NOT inherit rust-version; [lints] workspace = true
  src/
    lib.rs              # TransportBackend trait, shared workload, comparison types
    main.rs             # `transport-probe` binary (compare | late-join | admission | admin-tip)
    mesh.rs             # minimal full-mesh backend (Router + ALPN + dial-all + frame)
    gossip.rs           # minimal iroh-gossip backend (TopicId + subscribe + broadcast)
    workload.rs         # deterministic shared signed-WireEvent set (via core builders)
    report.rs           # ComparisonResult + JSON/markdown emitters
  tests/
    self_check.rs       # deterministic loopback self-check (CI-runnable)
  results/
    README.md           # how the artifacts are produced
    results.md          # rolled-up comparison table (filled by a local run)
  NOTES.md              # the decision memo + API reconciliation + findings
```

Hard isolation rules (copied from the spike-nat / spike-blobs precedent):

- **`publish = false`** and it **MUST NOT** be a dependency of `iroh-rooms-core`,
  `iroh-rooms-cli`, or `iroh-rooms-net`. Adding a 0.x crate (`iroh-gossip`) here keeps it
  entirely off the shipping critical path — the whole point of ADR-1's churn argument.
- Add to workspace `members` in the root `Cargo.toml` with a comment matching the existing
  spike entries, so CI builds it and runs its self-check (but cannot prove nothing that
  needs a real network — see §7.8; here everything is loopback so CI proves the *measured*
  claims too).
- Does **not** inherit the workspace `rust-version` (the iroh 1.0 stack MSRV is higher; same
  rationale as the other spikes).

**Dependencies (to reconcile in step 1, §7.4):**

- `iroh = "=1.0.1"` — the exact pin the shipping carrier and other spikes run on.
- `iroh-gossip = "=0.101.0"` — the D1 candidate. **Must be reconciled against `iroh 1.0.1`
  first** (§7.4); this is the highest-risk step.
- `iroh-rooms-core` (path dep, with the `sync` feature) — for `WireEvent`, `EventId`,
  `SyncDigest`, and the public event builders that produce the shared signed workload
  (§7.5). This is a *dev/spike* consumption of core, not core depending on the spike.
- `tokio`, `anyhow`, `serde`, `serde_json`, `tracing`, `tracing-subscriber` — as in
  `spike-nat`.

---

## 6. The measurement contract (dimensions and definitions)

Every metric below is captured per backend and, where meaningful, per N∈{2,3,4,5}. All
runs are in-process loopback nodes with `RelayMode::Disabled` so the numbers are
deterministic and CI-reproducible (the AC's "measured or simulated locally").

| Dimension (ADR-1 / issue Scope) | Definition / how measured | The claim under test |
|---|---|---|
| **Propagation latency** | Wall time from `publish(event)` on node 0 to that `event_id` appearing in node k's received set, for every k≠0, at N=2..5. Report per-N min/median/max and fan-out completion time (time until *all* nodes hold the event). | Mesh is direct 1-hop; gossip may be multi-hop. At N≤5 mesh should be ≤ gossip. |
| **Reconnect behavior** | Drop one node's link mid-stream; publish new events; measure whether/when the event set re-converges after the link is re-established, and by what mechanism. | Mesh redials + re-pulls on the same authenticated link (IR-0005 T4). Gossip: `Event::Lagged` = silent drop → "resync needed" signal, but no per-peer link to pull over. |
| **Late-join behavior** (AC2) | Bring up N-1 nodes, broadcast M events, then subscribe/join the Nth node. Count how many of the M pre-join events the newcomer receives **over the transport alone** (no sync layer). | Gossip newcomer receives **0** of the M (history gap = M); confirm or disprove. Mesh newcomer also gets 0 raw, but the same link trivially carries a backfill pull. |
| **Auth / admission model** (AC3) | (mesh) dial from an unknown / non-member `EndpointId`; confirm the connection is refused **before** any event byte is read. (gossip) have a node that knows only the `TopicId` subscribe; confirm it receives and can publish plaintext events with **no** admission check. | Mesh admission is native and pre-byte; gossip topic is open and unauthenticated. |
| **Implementation complexity** | Lines of backend code, count of 0.x crates pulled onto the path, and a qualitative note on what each backend forces the *app* to re-implement (ordering, history, auth). | Gossip's tiny API is a "false economy" — you re-implement ordering+history+auth regardless, and add a 0.x dep. |
| **Admin-tip carrier** (Residual 13) | Prototype `AdminTip{tip:(event_id, admin_seq)}` advertisement over (a) the mesh `SyncMessage::AdminTip` frame (already implemented) and (b) a gossip liveness topic; compare freshness, cost, and trust posture. | Decide whether gossip adds enough value as an off-path liveness carrier to justify the 0.x dep. |

**The correctness oracle (issue Test Plan).** After each scenario, every node's set of
received `event_id`s is collected as a `BTreeSet<EventId>` (or via `SyncEngine::digest()` if
the events are folded). Two comparisons matter:

1. **Event-set equality** — do all nodes converge to the same set? (Steady state, no drops.)
2. **Observed failure modes** — enumerate exactly what diverges and why: gossip `Lagged`
   drops, late-join gaps, admission rejections, ordering (there is none at the transport
   layer for either). The memo records these verbatim.

Because the *same signed bytes* are fed to both backends (§7.5), any set difference is a
property of the transport, not the payload.

---

## 7. Design

### 7.1 The common `TransportBackend` trait

Both backends sit behind one trait so the harness drives them identically (mirrors the
sans-IO `SyncTransport` seam in core, but this is a spike-local, async, node-owning trait —
it stands up real endpoints):

```rust
/// A minimal event-carrier backend for the N≤5 comparison. Spike-local; NOT the
/// shipping `iroh_rooms_core::sync::SyncTransport`.
#[async_trait]
pub trait TransportBackend {
    /// Human label for the results table ("mesh" | "gossip").
    fn kind(&self) -> BackendKind;

    /// Broadcast one verbatim signed `WireEvent` to the room.
    async fn publish(&self, wire: WireBytes) -> anyhow::Result<()>;

    /// Every `event_id` this node has *received* from peers so far (its set for
    /// the equality oracle). Deduped by id.
    fn received_ids(&self) -> BTreeSet<EventId>;

    /// Backend-observed failure signals since the last drain (gossip `Lagged`,
    /// admission rejections, link drops) — feeds the "observed failure modes" column.
    fn drain_events(&self) -> Vec<BackendEvent>;
}
```

A `Cluster` helper stands up N nodes of a given backend on loopback, wires their addresses
(mesh dials every peer's `EndpointId`; gossip bootstraps every node into one `TopicId`),
and exposes `publish_from(node_i, wire)` + `await_convergence(deadline)`.

### 7.2 Minimal full-mesh backend (`mesh.rs`)

Deliberately minimal for parity with the gossip prototype (the *shipping* carrier already
exists — this is the comparison twin, not a re-ship):

- `iroh::Endpoint` keyed by a seed-derived device secret + a `Router` on ALPN
  `/iroh-rooms/spike-event/1` (a **spike-only** ALPN, distinct from the shipping
  `/iroh-rooms/event/1`, so the spike can never be mistaken for the real plane).
- A `ProtocolHandler::accept` that (for the admission scenario, §7.6/AC3) authorizes the
  QUIC/TLS-proven `Connection::remote_id()` against an allowlist of member `EndpointId`s and
  **closes before `accept_bi()`** for a non-member.
- Dial every other member's `EndpointId`; one length-prefixed bidi stream per peer; each
  frame is a verbatim `WireEvent` byte string. Received frames are deduped by recomputed
  `event_id` into `received_ids`.
- **Cross-reference, do not re-verify:** the reconnect/backoff and the production admission
  gate are already proven in `iroh-rooms-net` (T2/T4). The memo cites that evidence; the
  minimal backend exists for the head-to-head latency + complexity numbers and a
  self-contained admission demonstration.

### 7.3 Minimal gossip backend (`gossip.rs`)

- One `iroh_gossip::Gossip` instance per node; `gossip.subscribe(TopicId, bootstrap_peers)`
  → `GossipTopic` split into `GossipSender` / `GossipReceiver`.
- `publish` = `sender.broadcast(wire_bytes)`; the receiver task consumes
  `Event::Received { content, delivered_from }` (dedup by recomputed `event_id`) and records
  `Event::Lagged` as a `BackendEvent::Lagged` failure signal.
- **No admission:** the topic is joined by any node that knows the 32-byte `TopicId`; the
  admission scenario (§7.6) stands up an *interloper* node that knows only the topic and
  confirms it both receives plaintext and can publish — the decisive "open topic" evidence.
- **No history:** the late-join scenario subscribes the newcomer *after* M broadcasts and
  counts received pre-join events (expected 0).

### 7.4 iroh-gossip 0.101 API reconciliation — DO THIS FIRST (de-risks everything)

Exactly the `spike-nat` "§2 API reconciliation, Step 1" discipline. Before any measurement:

1. Confirm on **crates.io / docs.rs** the exact `iroh-gossip` version that resolves against
   `iroh = 1.0.1` (the pinned table says `=0.101.0` pinned to `iroh ^1`, but this was
   automated recon, **not** verified in-tree — see `PHASE-0-SPIKE.md` "Confirm before
   pinning"). If `0.101.0` does not build against `iroh 1.0.1`, pick the nearest gossip
   version that does and **record the exact pin + why** in `NOTES.md`.
2. Reconcile the real 0.101 API against the ADR-1 sketch: `Gossip` construction (does it
   need the `Endpoint` / a `Router` `.accept` on the gossip ALPN?), `subscribe` signature,
   the `GossipTopic` → `GossipSender`/`GossipReceiver` split, and the `Event` enum
   (`Received { content, delivered_from }`, `Lagged`, `NeighborUp`/`NeighborDown`). Write the
   "recon expectation vs. reality" table (spike-nat §2 format).
3. If gossip cannot be made to build against `iroh 1.0.1` at all, that is itself a **strong
   confirming datapoint for ADR-1** (the 0.x churn / integration-cost argument) — record it
   as such; the spike does not then need a running gossip cluster to reach its decision, but
   MUST document the attempt and the failure precisely.

### 7.5 Shared signed-payload workload (`workload.rs`)

The issue Test Plan requires *"the same signed event payloads"* through both backends. Reuse
`iroh-rooms-core`'s public, deterministic event builders (`build_room_created`,
`build_message_text`, `build_member_*`, etc.) with **seed-derived keys and injected clocks**
(the same technique the core conformance fixtures use) to produce a fixed ordered list of
signed `WireEvent`s — a `room.created` genesis followed by M `message.text` events. The
byte-for-byte identical `WireBytes` are fed to `mesh` and `gossip`, so the equality oracle
compares transports, never payloads. No wall-clock or RNG on the workload path (determinism =
CI-reproducible numbers).

### 7.6 Set-equality and failure-mode oracle (`report.rs`)

- **Convergence check:** after `await_convergence`, assert every node's `received_ids()` set
  equals the published set (steady state) — or record the exact delta and its cause.
- **Failure-mode ledger:** drain each backend's `BackendEvent`s into the results table:
  gossip `Lagged` counts, admission rejections (mesh), interloper acceptances (gossip),
  late-join gap size.
- **`ComparisonResult`** (the structured artifact, one per run): `n`, `backend`,
  `events_published`, `converged` (bool), `set_delta` (missing ids per node),
  `propagation_ms` (min/median/max, fan-out completion), `late_join_gap`,
  `admission_enforced` (bool), `interloper_received` (bool, gossip), `lagged_events`,
  `backend_loc`, `zerox_deps_added`, `iroh_gossip_version`, `iroh_version`, `run_note`.
  Emitted as human summary on stdout and JSON on `--json`, rolled up into
  `results/results.md`.

### 7.7 Admin-tip-as-gossip probe (Residual Open Decision 13)

Prototype admin-tip advertisement two ways and compare, off the critical path:

- **Mesh carrier (already exists):** `SyncMessage::AdminTip { tip: Option<(EventId, u64)> }`
  is already an engine message on the authenticated link. Confirm a node learns a peer's
  higher admin tip over the existing mesh connection (no new mechanism).
- **Gossip carrier (candidate):** broadcast the same `AdminTip` on a dedicated liveness
  topic; measure freshness (how quickly a peer learns of a higher tip) and note the trust
  posture — a gossip `AdminTip` is an *unauthenticated hint* that only triggers a
  fail-closed / backfill on the authenticated path, never a trust input on its own.
- **Decision:** the memo (§9) records whether gossip's liveness advertisement adds enough
  (e.g., faster incompleteness detection when a mesh link is momentarily down) to justify
  putting a 0.x crate into a *later* phase, or whether the mesh `AdminTip` frame alone
  suffices for MVP. Either way, Open Decision 13 moves from "open" to "decided, with
  measured rationale."

### 7.8 Harness / CLI (`transport-probe`) and CI self-check

- `transport-probe compare --n <2..5> [--events <M>] [--json]` — stands up both clusters,
  runs the workload, prints the comparison table.
- `transport-probe late-join --backend <mesh|gossip> --n <N> --events <M>` — the AC2 gap
  probe.
- `transport-probe admission --backend <mesh|gossip>` — the AC3 probe (mesh: interloper
  refused pre-byte; gossip: interloper admitted).
- `transport-probe admin-tip` — the Residual-13 probe.
- **`tests/self_check.rs`** runs the whole comparison on loopback deterministically (fixed N,
  fixed M, seed-derived keys, `RelayMode::Disabled`, every await timeout-bounded) and asserts:
  mesh converges to full set equality; gossip converges in steady state but the late-join
  newcomer's gap == M; mesh admission refuses the interloper; gossip admits it. Because
  everything is loopback and deterministic, **CI proves the measured claims** (unlike
  `spike-nat`, whose real-NAT claim CI cannot prove). Must pass `scripts/verify.sh`
  (fmt + clippy `-D warnings` pedantic + tests) — the real CI gate.

### 7.9 Structured results artifact

`results/results.md` is the rolled-up table (backend × N × dimension) plus a
`results/*.json` per run, matching the `spike-nat/results/` convention. This table drops
verbatim into the decision memo (§9) and can be pasted into issue #10.

---

## 8. Measurement matrix

| Scenario | Backends | N | What it produces |
|---|---|---|---|
| Steady-state fan-out | mesh, gossip | 2,3,4,5 | propagation latency (min/median/max, fan-out completion); set-equality convergence |
| Late join | mesh, gossip | 3 (join a 4th) | pre-join history gap (AC2); confirm gossip newcomer gets 0 of M |
| Reconnect | mesh, gossip | 3 | re-convergence after a mid-stream link drop; mechanism + `Lagged` observation |
| Admission | mesh, gossip | 3 + 1 interloper | mesh refuses unknown `EndpointId` pre-byte (AC3); gossip admits topic-knower |
| Admin-tip carrier | mesh, gossip | 3 | freshness + trust posture of `AdminTip` over each (Residual 13) |
| Complexity | mesh, gossip | — | backend LOC, 0.x deps added, app-side re-implementation burden |

All scenarios run on loopback (`RelayMode::Disabled`), in-process; deterministic.

---

## 9. The decision memo (AC4) and how it closes the issue

Since this ADW phase has **no GitHub access**, the memo lands **in the repo** and is
*linked from docs*; the orchestrator/human can paste it into issue #10. AC4 ("Decision memo
is posted in the issue **or linked from docs**") is satisfied by the doc path.

The memo lives in **`crates/spike-transport/NOTES.md`** (the findings deliverable, mirroring
`spike-nat/NOTES.md` and the `spike-blobs` "COMPLETE" annotation pattern) and MUST contain:

1. **The measured comparison table** (§7.9), with pinned `iroh` + `iroh-gossip` versions.
2. **A per-dimension verdict** against each ADR-1 claim: latency, reconnect, late-join gap,
   admission model, complexity — each marked *confirmed* / *disproven* with the numbers.
3. **The D1 decision** in one paragraph, in the Gate-C form ("mesh | gossip … backed by
   Day-4 measurements"): ratify ADR-1 (full-mesh for the load-bearing log; gossip parked as
   optional liveness) **or** flip it, citing the measured trigger (Day-4 criterion: gossip
   wins only if mesh dial/maintenance proved materially harder than expected).
4. **Residual Open Decision 13 resolution** — the chosen admin-tip carrier + rationale.
5. **Any surprises** that would flip the decision (recorded even if the decision holds).

Then annotate **`PHASE-0-SPIKE.md`** to close the loop (mirrors the Day-8 blob
`> COMPLETE (IR-0009): GATE GO …` annotation):

- Under ADR-1 status and the Day-4 plan line: `> COMPLETE (IR-0006): D1 measured — <verdict>.
  Findings in crates/spike-transport/NOTES.md.`
- Update **Residual Risk 12** (D1 half now measured) and **Open Decision 13** (now decided).
- Optionally note in `README.md`'s status section that the D1 transport decision is
  measurement-closed, and in `crates/iroh-rooms-net/NOTES.md` that the ADR-1 mesh choice is
  now measurement-backed (not just the landed loopback carrier).

**This spec plans the memo; it does not write the decision** — the decision is authored after
the measurements exist, and must cite them.

---

## 10. Implementation steps (executable by another engineer/agent)

1. **Reconcile `iroh-gossip` against `iroh 1.0.1` first (§7.4).** Confirm the exact resolvable
   pin on crates.io/docs.rs; write the recon table in `NOTES.md`. If it cannot build against
   `iroh 1.0.1`, record that as a confirming ADR-1 datapoint and proceed with the mesh-only
   measurements + a documented gossip-integration-cost finding.
2. **Scaffold `crates/spike-transport`** on the `spike-nat` template: `Cargo.toml`
   (`publish = false`, no `rust-version` inherit, `[lints] workspace = true`), `lib` + `[[bin]]
   transport-probe`, `results/`, `NOTES.md`. Add it to workspace `members` with a comment.
   Confirm no shipping crate depends on it.
3. **Implement the shared workload (`workload.rs`)** using core's public event builders with
   seed-derived keys + injected clocks → a fixed ordered `Vec<WireBytes>` (genesis + M
   messages). Unit-test determinism (same bytes every run).
4. **Implement the `TransportBackend` trait + `Cluster` helper (`lib.rs`)** and the two
   backends (`mesh.rs`, `gossip.rs`) per §7.2/§7.3. Keep both minimal and comparable.
5. **Implement the oracle + report types (`report.rs`)**: `received_ids` set-equality,
   `BackendEvent` failure ledger, `ComparisonResult` (JSON + markdown).
6. **Implement `transport-probe`** subcommands (`compare`, `late-join`, `admission`,
   `admin-tip`) per §7.8.
7. **Write `tests/self_check.rs`** — the deterministic loopback assertions (§7.8). Every await
   timeout-bounded; `RelayMode::Disabled`.
8. **Run the measurements locally**, roll up `results/results.md`, and **write the decision
   memo in `NOTES.md`** (§9) with the measured table.
9. **Annotate `PHASE-0-SPIKE.md`** (ADR-1 status, Day-4 line, Residual 12, Open Decision 13)
   and optionally `README.md` / `iroh-rooms-net/NOTES.md` (§9).
10. **`scripts/verify.sh` green** across the workspace (fmt + clippy `-D warnings` pedantic +
    tests, including the new self-check).

Steps 3–10 are the *implementation* issue's work (a follow-up); **this document produces only
the spec.** No production code is modified by this planning task.

---

## 11. Acceptance criteria → evidence mapping

| Issue AC | How this plan satisfies it |
|---|---|
| N=2..5 propagation behavior measured or simulated locally | §6 latency dimension + §8 steady-state matrix, run on in-process loopback nodes; `results/results.md` per-N table; asserted in `tests/self_check.rs`. |
| Gossip late-join history gap confirmed or disproven | §7.3 + §7.8 `late-join` probe + §8 late-join scenario: newcomer subscribes after M broadcasts, received pre-join count reported (expected 0 → gap == M); self-check asserts it. |
| Mesh admission-control behavior confirmed | §7.2 minimal mesh gate (reject unknown `EndpointId` before `accept_bi()`) + §7.8 `admission` probe; corroborated by landed `iroh-rooms-net` T2 (§4). |
| Decision memo posted in issue or linked from docs | §9: memo in `crates/spike-transport/NOTES.md`, linked from `PHASE-0-SPIKE.md` / `README.md`; pasteable into issue #10. (No GitHub access this phase — the doc path satisfies the "or linked from docs" branch.) |
| Test Plan: same signed payloads, compare event-set equality + failure modes | §7.5 shared `WireBytes` workload; §7.6 `BTreeSet<EventId>` equality oracle + `BackendEvent` failure ledger. |

---

## 12. Risks

- **`iroh-gossip 0.101` may not build against `iroh 1.0.1`.** (medium) The pinned-version
  table is unverified automated recon; the whole stack above core is 0.x on a monthly
  breaking cadence. *Mitigation:* §7.4 reconciles it first; a build failure is itself a
  confirming ADR-1 datapoint (record it, proceed mesh-only with a documented integration-cost
  finding). Isolation (`publish = false`, off the shipping tree) bounds the blast radius.
- **Loopback measurements under-represent multi-hop gossip.** (low–medium) At N≤5 on
  loopback, gossip's PlumTree may be effectively 1-hop, hiding fan-out cost. *Mitigation:* the
  claim under test is "gossip buys nothing at N≤5," so a small/no latency gap is a *valid*
  confirming result, not a measurement flaw; the memo states the loopback caveat explicitly
  (real-NAT latency is Gate A's concern, not D1's).
- **Comparing a minimal mesh vs. a minimal gossip skews the complexity dimension** against the
  shipping carrier's real feature set. (low) *Mitigation:* the complexity column measures the
  *minimal* twins for parity, and separately cites the landed `iroh-rooms-net` for the
  production admission/reconnect story (§4). The memo keeps the two clearly labeled.
- **Determinism of async fan-out timing.** (low) Wall-clock latency numbers vary run-to-run.
  *Mitigation:* self-check asserts *convergence + set equality + gap size* (deterministic),
  not latency thresholds; latency is reported as a range for the memo, never gated in CI.
- **Scope creep into re-implementing ordering/history.** (low) *Mitigation:* §2 non-goals fix
  the boundary — the spike measures what the transport does/doesn't provide; the log layer is
  untouched.
- **`verify.sh` clippy-pedantic + fmt gate.** (low) A throwaway spike still must pass the real
  CI gate (`[lints] workspace = true`). *Mitigation:* budgeted in step 10; mirrors how
  `spike-nat`/`spike-blobs` already satisfy it.

---

## 13. Assumptions

- The ADR-1 recommendation is **expected to hold**; this spike produces the measured evidence
  and the memo, and only flips the decision on a measured surprise (Day-4 criterion).
- N≤5 is fixed (PRD §17.1.13); no large-swarm requirement is in scope.
- Local loopback measurement satisfies the AC ("measured **or simulated** locally");
  real-NAT is out of scope here (Gate A / IR-0012).
- `iroh-rooms-core` exposes public, deterministic event builders sufficient to synthesize the
  shared signed `WireEvent` workload with injected keys/clocks (confirmed: `build_room_created`,
  `build_message_text`, `build_member_*` exist and are golden-tested).
- The landed `iroh-rooms-net` loopback evidence (T2 admission, T4 reconnect) is acceptable
  corroboration for the mesh admission/reconnect claims, so the spike need not re-ship them.
- This ADW phase does not commit code or interact with GitHub; the orchestrator handles git/PR
  and any issue posting. The memo is delivered as a repo doc.

---

## 14. Open questions

1. **Minimal mesh vs. wrap the landed carrier.** Should `mesh.rs` be a fresh minimal backend
   (chosen here, for apples-to-apples parity with the gossip prototype) or a thin wrapper over
   `iroh-rooms-net::NetTransport` (measures the *real* mesh but skews complexity)? Default:
   minimal fresh backend + cite the landed carrier. Revisit if the minimal mesh diverges from
   the shipping behavior in a way that matters to the decision.
2. **Exact `iroh-gossip` pin.** Resolved in step 1 (§7.4) against `iroh 1.0.1`; may differ
   from the `=0.101.0` in the `PHASE-0-SPIKE.md` table (unverified recon).
3. **Admin-tip carrier for MVP vs. later phase.** §7.7 measures both; the memo decides whether
   gossip's liveness advertisement is worth a 0.x dep in a *later* phase or whether the mesh
   `AdminTip` frame alone suffices for MVP. (Residual Open Decision 13.)
4. **Reconnect scenario depth.** Is a single mid-stream link drop + re-converge sufficient, or
   should the spike also exercise a longer partition (peer offline through ~M events, then
   rejoin)? The longer case overlaps Gate D convergence work already landed in core; default
   to the single-drop transport-level check and cite Gate D for the deeper convergence proof.
5. **Whether a `results.md` with placeholder numbers should land with the scaffold** (like
   `spike-nat`'s pending table) or only after a local run. Default: land the scaffold + schema
   with a clearly-marked "pending local run" table, fill it in the same PR that writes the memo.

---

## 15. Out of scope

- Real-NAT / hole-punching measurement (Gate A, IR-0012 / `crates/spike-nat`).
- Any change to `iroh-rooms-core`, `iroh-rooms-cli`, or `iroh-rooms-net`.
- New sync/ordering/history/backfill logic (ADR-2 layers, already landed).
- Adopting `iroh-gossip` into the shipping tree, now or in this phase.
- Large-swarm (N≫5) evaluation, sharding, or big-room membership.
- The D2 decision (iroh-docs vs. hand-roll) — that is Day-5 / a separate item.
