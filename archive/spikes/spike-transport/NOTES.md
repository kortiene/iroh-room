# IR-0006 Day-4 Gossip-vs-Full-Mesh Transport Comparison — Findings & Decision Memo

This is the written findings deliverable required by **AC4** and spec §9
(`specs/measure-gossip-vs-full-mesh-transport.md`). It records the
`iroh-gossip 0.101` API reconciliation, the measured comparison table, a
per-dimension verdict against each ADR-1 claim, the **D1 decision**, the
Residual Open Decision 13 resolution, and the surprises found while building
the harness.

> Status: throwaway-grade spike (mirrors `spike-nat` / `spike-blobs`). It
> measures whether ADR-1 holds in code; it does **not** modify the shipping
> crates (`iroh-rooms-core`, `iroh-rooms-cli`, `iroh-rooms-net`) and is not a
> dependency of any of them. Every scenario runs on deterministic loopback
> (`RelayMode::Disabled`) — **CI proves the measured claims**, not just that
> the harness builds (`tests/self_check.rs`; unlike `spike-nat`, whose
> real-NAT claim CI cannot prove).

---

## 1. `iroh-gossip` 0.101 API reconciliation (spec §7.4 — done first)

Confirmed in-tree against the pinned `iroh = "=1.0.1"`, resolved from
crates.io: `iroh-gossip = "=0.101.0"` builds cleanly against `iroh 1.0.1`
with **zero API drift** in the surface this spike uses (`cargo tree -p
spike-transport -i iroh-gossip` shows exactly one edge, `spike-transport ->
iroh-gossip`, confirming isolation from the shipping tree). This confirms
the `PHASE-0-SPIKE.md` pinned-version table's `iroh-gossip = 0.101.0` recon
was correct — it did **not** need to be revised, unlike the `iroh-blobs`
recon in `spike-blobs/NOTES.md` §1, which found a stale `0.97` note.

| Recon expectation (spec §7.3 / ADR-1 sketch) | Reality on `iroh-gossip 0.101.0` | What we do |
|---|---|---|
| `Gossip` construction needs the `Endpoint` / a `Router` `.accept()` on the gossip ALPN | Confirmed: `Gossip::builder().spawn(endpoint)` returns a `Gossip` handle that also implements `ProtocolHandler`; register it with `Router::builder(endpoint).accept(GOSSIP_ALPN, gossip.clone())`. | `gossip.rs::GossipNode::spawn`. |
| `subscribe(TopicId, bootstrap_peers)` → `GossipTopic` → split into `GossipSender`/`GossipReceiver` | Confirmed, in `iroh_gossip::api`. `GossipTopic::split()` consumes `self` and returns `(GossipSender, GossipReceiver)`. | `gossip.rs::GossipNode::spawn`, `subscribe_liveness`. |
| `Event` enum: `Received { content, delivered_from }`, `Lagged`, `NeighborUp`/`NeighborDown` | Confirmed, `iroh_gossip::api::Event`. `Received` is a tuple variant wrapping a `Message { content: Bytes, .. }`, not a struct variant — matched as `Event::Received(msg)`, `msg.content`. | `gossip.rs::receiver_task`. |
| (not in the original sketch) join-readiness signal | `GossipReceiver::joined(&mut self) -> Result<(), ApiError>` progresses the event stream to the first `Event::NeighborUp` and is the documented way to know a bootstrapped subscriber has actually formed a swarm link. **Load-bearing finding, §6 below.** | `gossip.rs::GossipNode::spawn`, `subscribe_liveness`. |

**Isolation confirmed:** `cargo tree -i iroh-gossip` (workspace-wide) shows
`iroh-gossip` depended on by `spike-transport` only. `cargo tree -i <pkg>`
for every crate `iroh-gossip` pulls transitively (`irpc`, `n0-future`,
`tokio-util`, `rand`, `hex`, `indexmap`, `derive_more`, `postcard`,
`iroh-metrics`, `n0-error`) shows every one of them **already** in the
workspace's resolved graph via `iroh` and/or `iroh-blobs` (the shipping
`iroh-rooms-net` / `spike-blobs` dependency). The **only** crate `iroh-gossip`
adds that is wholly new to the workspace's dependency graph is
`futures-concurrency v7.7.1`. This nuances (without reversing) the "0.x
churn" argument for D1 — see §4.

---

## 2. The measured comparison table (AC1/AC2/AC3)

Full table in [`results/results.md`](results/results.md). Summary:

- **Steady-state fan-out, N=2..5:** both backends converge to full
  `BTreeSet<EventId>` equality at every N, on loopback, with propagation
  latency indistinguishable within run-to-run noise (mesh: 15–18 ms per-event
  arrival across all N; gossip: 15–17 ms). Zero `Lagged` events on either
  backend at this size and workload (11 events, N≤5).
- **Late-join (AC2):** newcomer receives **0/11** pre-join events on
  **both** backends — mesh and gossip alike give zero raw history over the
  transport (confirms the transport-agnostic half of ADR-1's framing, §4).
- **Admission (AC3):** mesh refuses the interloper's connection **before**
  `accept_bi()` (100% enforced, corroborating `iroh-rooms-net` T2); gossip
  admits the interloper with no auth check, and a room member receives the
  interloper's published event (100% "open topic").
- **Admin-tip carrier (Residual 13):** mesh control-frame freshness 18–21 ms;
  gossip liveness-topic freshness 3–6 ms, both on a 2-node loopback probe.

`fanout_completion_ms` (~180–188 ms across every run) is an artifact of
`Cluster::await_convergence`'s fixed 15 ms poll interval × 11 sequential
publishes in the harness's `compare` driver, not a backend cost difference —
it is nearly identical for both backends at every N, which is itself
consistent with "gossip buys nothing at N≤5" (if gossip's PlumTree needed
materially more hops, this number would diverge from mesh's; it does not).

---

## 3. Per-dimension verdict against each ADR-1 claim

| Dimension | ADR-1 claim | Verdict | Evidence |
|---|---|---|---|
| **Propagation latency** | "At N≤5 mesh should be ≤ gossip" (direct 1-hop vs. possibly multi-hop) | **Confirmed, with a caveat.** Mesh and gossip are statistically indistinguishable at N≤5 loopback (both single-digit-ms arrival, both ~180 ms fan-out-completion artifact). Mesh is never *slower*; it is not measurably *faster* either at this size. This is the expected, *confirming* result — ADR-1's claim was "gossip buys nothing," not "gossip is slower," and a near-zero latency gap is exactly that (spec §12 risk 2, "a small/no latency gap is a valid confirming result, not a measurement flaw"). | `results/results.md` §"Steady-state fan-out" |
| **Reconnect behavior** | Mesh redials + re-pulls on the same authenticated link; gossip `Lagged` is a "resync needed" signal with no per-peer link to pull over | **Confirmed, now with a direct mesh measurement in addition to the cross-reference.** Mesh's *production* dial-with-backoff reconnect is proven end-to-end in `iroh-rooms-net::tests::loopback.rs` T4 (`Connected → Offline/Connecting → Connected`, then a post-reconnect event still arrives) — this spike's minimal mesh backend intentionally does not reimplement backoff (spec §7.2 "cross-reference, do not re-verify"). What the minimal backend *does* offer is now directly measured: `tests/self_check.rs::mesh_link_drop_is_observed_and_a_fresh_dial_reconverges` shuts a node down mid-stream, confirms the survivors observe `BackendEvent::LinkDropped`, and confirms a fresh explicit `dial()` under the same identity genuinely re-establishes delivery (a post-redial event is received; a while-down event is not, retroactively — no history over a reconnect either). **New finding:** the redial only worked when issued by the side that dialed *originally* — the reverse direction (the rejoined node dialing back) never delivered a frame across a 5s retry window despite `dial()` returning `Ok`. That is concrete evidence, not just an architectural inference, for why a real reconnect needs a per-peer connection state machine (exactly what `iroh-rooms-net`'s dial-with-backoff loop is) rather than "dial again" — sharpening, not flipping, the complexity-dimension case. Gossip's `Event::Lagged` signal exists and is drained into `BackendEvent::Lagged` by this harness (`gossip.rs::receiver_task`) but was not observed in any steady-state run (0 lagged events at N≤5, 11 events — too small a workload to induce a channel backlog). The qualitative claim (Lagged = drop signal, no addressable peer link) is architecturally confirmed by the API shape itself: `iroh_gossip::api::Event` has no per-peer "retry this connection" affordance, only the broadcast-wide `Lagged` marker. | `iroh-rooms-net/tests/loopback.rs` T4; `tests/self_check.rs::mesh_link_drop_is_observed_and_a_fresh_dial_reconverges`; `gossip.rs::receiver_task` |
| **Late-join history gap** | Gossip newcomer receives **0** of the M pre-join events; mesh newcomer also gets 0 raw but the link trivially carries a backfill pull | **Confirmed exactly as predicted.** Both backends: newcomer receives 0/11. The asymmetry is structural, not something this transport-only measurement can show numerically: the mesh newcomer already holds an authenticated bidi stream to every existing member the instant it dials in (`mesh.rs::MeshNode::dial` registers the same reader/writer machinery used for the event workload — a backfill request is "just another frame," spec §4); the gossip newcomer has **no per-peer connection object at all** to attach a pull request to — only the shared, unauthenticated topic. | `results/results.md` §"Late-join"; `tests/self_check.rs::gossip_late_join_gap_equals_published_count` |
| **Auth / admission model** | Mesh gives native pre-byte authenticated admission; gossip's open topic gives none | **Confirmed exactly as predicted.** Mesh: the interloper's connection is closed by `MeshHandler::accept` before `accept_bi()` is ever called — verified via the close-reason carrying `REJECT_CODE`, not by connection success/failure alone. Gossip: an interloper that knows only the fixed 32-byte `TopicId` and one bootstrap address joins the swarm, publishes a plaintext event, and a room member receives it — no identity check anywhere in the join or publish path. | `results/results.md` §"Admission"; `tests/self_check.rs::mesh_admission_refuses_interloper_before_any_byte` / `gossip_admits_interloper_with_no_auth_check` |
| **Implementation complexity** | Gossip's tiny API is a "false economy" — the app re-implements ordering+history+auth regardless, plus a 0.x dep | **Confirmed, with a sharper number than expected.** Gossip's own backend (`gossip.rs`, 299 LOC) is smaller than mesh's (`mesh.rs`, 405 LOC) — the raw-LOC comparison alone would (wrongly) favor gossip. But mesh's extra ~100 lines *are* the admission gate, the per-peer dial/reconnect bookkeeping, and the length-prefixed frame codec — i.e. exactly the auth + connection-state machinery gossip has no slot for at all, not incidental complexity. Gossip still adds one direct 0.x dependency (`iroh-gossip 0.101`) plus one *wholly new* transitive crate to the workspace graph (`futures-concurrency`) — smaller churn exposure than the pinned-version table implied (§1), but nonzero, and it buys nothing at N≤5 per the other four rows. | `results/results.md` §"Implementation complexity"; §1 above |

**All five ADR-1 claims hold.** None are disproven; the latency claim
resolves to its stronger, intended form ("gossip buys nothing," not "gossip
is slower") rather than a literal numeric win for mesh.

---

## 4. The D1 decision

**Ratify ADR-1: full-mesh direct QUIC remains the Room Event Plane
transport; `iroh-gossip` is not adopted for the load-bearing log.**

Per the Day-4 decision criterion (`PHASE-0-SPIKE.md`: *"gossip wins only if
mesh dial/maintenance proves materially harder than expected"*): it did not.
The minimal mesh backend needed ~100 more lines than gossip's, entirely
attributable to admission control and per-peer connection bookkeeping the
product requires regardless of transport — not incidental dial/maintenance
overhead. Both backends converge to identical event sets at every measured
N with statistically indistinguishable latency; the late-join gap is
identical (0/M) on both, and the auth model — the one dimension with a real,
measured, categorical difference — comes down decisively in mesh's favor
(refuses-before-bytes vs. open-topic-admits-anyone). The dependency-churn
argument survives in weakened form (§1): gossip is not the multi-crate
liability the pinned-version table implied, but it remains a 0.x dependency
this spike would add for a benefit (epidemic fan-out at scale) that does not
exist at N≤5. **No measured surprise crossed the Day-4 flip trigger.**

Gossip is parked exactly as ADR-1 already specified: an optional,
off-critical-path liveness/admin-tip carrier, never the system of record for
the event log.

---

## 5. Residual Open Decision 13 resolution — admin-tip carrier

**Decision: the mesh `SyncMessage::AdminTip` control frame is sufficient for
MVP; gossip's liveness topic is not adopted in this phase.**

Both carriers work (§2): the mesh control frame rides the same authenticated
link the event workload already uses — "no new mechanism," per ADR-1 §4 —
and was observed end-to-end in 18–21 ms on a 2-node loopback probe. The
gossip liveness topic is faster in this small probe (3–6 ms, direct
neighbor push vs. mesh's per-peer send), but:

1. **The freshness gap is immaterial at MVP scale.** Both are one to two
   orders of magnitude under any liveness-detection threshold that matters
   (peers noticing a higher admin tip is a background hygiene signal, not a
   latency-sensitive path — Membership & Ordering §0's fail-closed behavior
   triggers on *detecting* a divergent tip at all, not on shaving
   milliseconds off detection).
2. **Trust posture is asymmetric and matters more than freshness.** A gossip
   `AdminTip` is, by construction, an *unauthenticated hint* on an open
   topic — exactly like the event-log gossip prototype, any node that
   knows the liveness `TopicId` could inject a bogus tip. It can only ever
   trigger a fail-closed re-check on the authenticated mesh path, never be
   trusted on its own (spec §7.7). The mesh control frame, by contrast, rides
   an already-authenticated, already-admission-gated connection — the
   freshness number and the trust guarantee both favor not adding a second,
   weaker-trust carrier for a use case the first carrier already covers.
3. **Adding gossip here means adding it to the dependency tree for exactly
   one narrow purpose**, re-opening the §1 churn question for a marginal
   3–4ms freshness win with no corresponding trust improvement.

**Open Decision 13 moves from "open" to "decided, with measured rationale":
mesh `AdminTip` control frame only, for MVP.** If a future phase needs
faster incompleteness detection across a much larger room (where mesh's
O(n²) per-peer control-frame fanout starts to matter), gossip's
liveness-topic prototype in this spike (`gossip.rs::subscribe_liveness`,
`liveness_topic`) is a ready-made starting point — revisit then, not now.

---

## 6. Surprises

1. **`GossipReceiver::joined()` is load-bearing and easy to miss — found via
   a real, reproducible harness bug.** The first version of
   `gossip::subscribe_liveness` subscribed to the liveness topic and
   returned immediately, without waiting for the swarm-membership layer to
   actually form a neighbor link (unlike `GossipNode::spawn`'s handling of
   the *event* topic, which already had this wait). The `admin-tip` probe
   then broadcast from node A before node B's liveness-topic subscription
   had a neighbor, and the broadcast was silently dropped — the probe timed
   out waiting for an observation that had already been lost, deterministically
   reproducible on every run. Fixed by making `subscribe_liveness` wait on
   `GossipReceiver::joined()` for a bootstrapped subscriber, mirroring
   `GossipNode::spawn`'s existing pattern (`gossip.rs`). **This is itself
   confirming evidence for the ADR-1 complexity claim (§3/§4):** gossip's
   "trivially small" `subscribe`/`broadcast` API hides a topology-formation
   race that a caller must know to wait out — the kind of implicit
   distributed-systems subtlety mesh's connection-oriented model (a
   `dial()` either succeeds or returns an error you can `.await`) does not
   have. (§3 below narrows this: mesh's own reconnect path is not entirely
   free of subtlety either, though of a different kind.)
2. **The dependency-churn picture is better for gossip than the pinned
   table implied, but the LOC picture is worse for the "cheap API" pitch
   than expected.** See §1 and §3's complexity row: `iroh-gossip` reuses
   most of its transitive dependency tree with `iroh`/`iroh-blobs` already
   in the workspace, but the app-side backend needed to build the admission
   and history story gossip does not provide would still cost *more* than
   the ~100 extra lines mesh's backend spent on the same problems, because
   gossip's version would need to reconstruct per-peer identity tracking
   with no connection object to hang it on. Neither surprise flips the
   decision; both sharpen the rationale.
3. **The minimal mesh backend's redial only works in the original dial
   direction — the reverse silently delivers nothing.** Added while closing
   the "reconnect" measurement gap (§3): the first version of
   `tests/self_check.rs::mesh_link_drop_is_observed_and_a_fresh_dial_reconverges`
   had the *rejoining* node re-dial the survivor it was originally dialed
   *by*. `dial()` returned `Ok` every time, but the survivor never received a
   single frame across a 5-second retry window. Reversing the redial to match
   the original direction (the side that dialed the first time dials again)
   fixed it immediately and reproducibly. This was not chased to an iroh-level
   root cause (out of scope for a throwaway spike backend with no per-peer
   connection state machine at all — spec §7.2), but it is real, reproducible
   evidence, not test flakiness: it shows the minimal backend's "just dial
   again" story is direction-sensitive and would need real connection-state
   tracking (which peer dialed whom, and redial from the same role) to be a
   correct general-purpose reconnect — exactly the machinery `iroh-rooms-net`
   already has and this spike deliberately did not rebuild.

None of the three surprises cross the Day-4 flip trigger.

---

## 7. Structure note

This crate follows the spec's file layout (`lib.rs` + `main.rs` + `mesh.rs` +
`gossip.rs` + `workload.rs` + `report.rs`) exactly as specified in §5 — no
undocumented additions. `iroh = "=1.0.1"` matches the shipping carrier;
`iroh-gossip = "=0.101.0"` is the one 0.x crate on this spike's path, isolated
from the shipping dependency tree (confirmed via `cargo tree -i iroh-gossip`,
§1); workspace lints (`unsafe_code = "forbid"`, clippy pedantic) inherited and
clean (`scripts/verify.sh`-equivalent local run: fmt --check, clippy
`--all-targets -- -D warnings`, `cargo test -p spike-transport`, all green,
50/50 tests passing (26 lib unit + 10 binary unit + 5 `tests/cluster.rs` +
9 `tests/self_check.rs`), including the full `tests/self_check.rs` contract).
