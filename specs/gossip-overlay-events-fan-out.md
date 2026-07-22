# Gossip Overlay for Events Fan-Out — Raise v1 Ceiling to N>5 (#154 GO decision)

- **Issue:** #171 — `[NET] Gossip overlay for Events fan-out — raise v1 ceiling to N>5 (#154 GO decision)`
- **Parent epic:** Phase 0 / transport scale-out.
- **Labels:** `type/feature`, `area/transport`, `priority/p1`, `risk/high`.
- **Depends on:** #154 (closed GO decision — gossip warranted for N>5), #145 / `crates/spike-N40`
  (measured evidence: N=5 all rates pass; N=40 QUIC panic at 1560 in-process connections;
  pre-`b0622ec` N=25 = 661 MB backlog, `accepted=0`, `frames_sent=0`), `crates/spike-transport`
  (measured gossip-vs-mesh at N≤5; auth axis measured as decisive).
- **Owning crates:** `crates/iroh-rooms-net` (the gossip wrapper, `Shared::route` branch,
  `PeerManager` rearchitecture, ALPN wiring), `crates/iroh-rooms-core` (only `MAX_ACTIVE_MEMBERS`
  raise, last phase only). No shipping change to `crates/iroh-rooms` / `crates/iroh-rooms-cli`
  beyond surfacing new diagnostics.
- **Status:** **Planning / spec only.** Do not implement from this document without a
  follow-up build task. This document changes no production code.

---

## 1. Summary

`#154` closed GO: a gossip overlay **is** warranted for N>5. The evidence is canonical:

- `crates/spike-N40/NOTES.md` §3/§5: N=5 survives idle through 5 events/s with no cascade;
  N=40 panics in the QUIC layer (`noq-proto`) while forming 1560 in-process connections.
- `PHASE-0-SPIKE.md:38-49`: pre-`b0622ec` N=25 collapsed silently — idle `frames_sent=0`,
  `accepted=0`, 661 MB inbound backlog — while every connectivity signal still read healthy.
  Full-mesh fan-out amplification collapses delivery well before N=40.
- `crates/spike-transport/NOTES.md` §3/§4: gossip measured at N=5 was latency-indistinguishable
  from mesh; declined **for N=5 only** because the auth axis (mesh refuses-before-bytes; open
  gossip admits anyone who learns the topic id) was decisive at that scale.

N>5 is now a product goal. The full-mesh topology cannot scale to it. This spec plans the
**surgical fan-out replacement** the #154 decision named: route `SyncMessage::Events` frames
through an `iroh-gossip` topic among admitted device keys instead of per-peer fan-out, while
leaving the engine, the wire protocol, the admission gate, the membership fold, and every
pull/query message variant on the existing point-to-point path.

The non-negotiable constraint — repeated throughout this document — is that the structural
**reject-before-bytes** admission guarantee (`handler.rs:49-141`) must survive the overlay
unchanged. Mechanism: the gossip ALPN is registered behind a wrapper that consults the same
`Shared::admission` gate before `iroh-gossip`'s handler ever sees the connection (D2). A peer
that fails admission at the connection level never reaches the gossip layer.

---

## 2. Repository context read (verified)

| Concern | Current state | Location | Reused / changed by this spec |
|---|---|---|---|
| Engine (sans-IO) | `SyncEngine` returns `Outgoing { peer, msg }`; no I/O | `crates/iroh-rooms-core/src/sync/engine.rs` | **Unchanged.** |
| Wire protocol | `SyncMessage` enum (8 variants); canonical-CBOR; pinned `MAX_FRAME_BYTES = 1 MiB` | `crates/iroh-rooms-core/src/sync/message.rs` | **Unchanged.** Events frames still ride `SyncMessage::Events { room_id, frames: Vec<WireBytes> }`. |
| Transport seam | `Shared::route(&Outgoing)` classifies by variant, encodes, pushes to the destination peer's `BytePriorityQueue`; no live writer ⇒ silent drop | `crates/iroh-rooms-net/src/transport.rs:382-410` | **Surgical branch added (D1).** Events → gossip sender; all other variants → existing per-peer queue. |
| `SyncTransport::send` | delegates to `Shared::route` | `crates/iroh-rooms-net/src/transport.rs:944-946` | Unchanged (calls the branched `route`). |
| Accept gate (reject-before-bytes) | `EventProtocolHandler::accept` closes with `REJECT_CODE` before `accept_bi()` when `Admission::authorize` returns `Reject` | `crates/iroh-rooms-net/src/handler.rs:49-141` | **Unchanged for EVENT_ALPN; cloned for GOSSIP_ALPN (D2).** |
| ALPN registration | One `Endpoint`, one `Router`, `.accept()` chain: `EVENT_ALPN` → `PIPE_ALPN` → `iroh_blobs::ALPN` | `crates/iroh-rooms-net/src/transport.rs:766-778`; ALPN constant `alpn.rs:14` | **Extended: add `GOSSIP_ALPN` accept chain (D2).** |
| PeerManager (warm dial set) | `desired_devices` = every Active member device minus self; `reconcile` starts/stops one `dial_loop` per device | `crates/iroh-rooms-net/src/manager.rs:84-175` | **Rearchitected: dial a seed subset, not every active member (D3).** |
| Active-member ceiling | `MAX_ACTIVE_MEMBERS = 5`; fold enforces `RejectReason::RoomFull`; `ACTIVE_MEMBER_WARNING_THRESHOLD = 4` | `crates/iroh-rooms-core/src/membership/model.rs:14-22` | **Raised last, behind a feature flag, after the overlay is proven (D4/D7).** |
| Provisional join-bootstrap gate | `provisional_allows` permits `Events`/`AdminTip`/`Heads`/`NotFound` to a provisional dialer; `WantMembership` only after capability proven | `crates/iroh-rooms-net/src/node.rs:1558-1571` | Behavior preserved; see §6.4 for the provisional-over-gossip interaction. |
| Connection generation guard (#126) | `register_link` stamps a monotonic generation; `teardown_if_current` no-ops a superseded link | `crates/iroh-rooms-net/src/transport.rs:451-475` | Unchanged. Gossip neighbor up/down is a separate signal (D8). |
| iroh-gossip 0.101 API recon | `Gossip::builder().spawn(endpoint)`; `subscribe(TopicId, bootstrap)` → `GossipTopic::split()` → `(GossipSender, GossipReceiver)`; `Event::Received(Message{content,..})`, `Event::Lagged`, `Event::NeighborUp/Down`; `GossipReceiver::joined()` is load-bearing for topology formation | `crates/spike-transport/src/gossip.rs:82-148`, `crates/spike-transport/NOTES.md` §1, §6 surprise 1 | **Reused verbatim** as the integration template. |
| Open-topic admission weakness (measured) | An interloper that knows only the 32-byte `TopicId` + one bootstrap addr joins and publishes; no identity check anywhere in gossip's join/publish path | `crates/spike-transport/NOTES.md` §3 auth row, §4; `tests/self_check.rs::gossip_admits_interloper_with_no_auth_check` | **Closed by D2** (gossip ALPN admission wrapper). |
| Local gate | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets --all-features` | `CONTRIBUTING.md`, `scripts/verify.sh` | Must stay green at every phase. |

---

## 3. Goals and non-goals

### Goals

1. Integrate `iroh-gossip = "=0.101.0"` (pinned to match `iroh = "=1.0.1"`; proven compatible by
   `crates/spike-transport/NOTES.md` §1) into `crates/iroh-rooms-net` as the **Events fan-out**
   carrier, behind the existing connection-level admission gate.
2. Make `Shared::route` branch on `SyncMessage` variant: `Events` → gossip broadcast; every
   other variant → existing per-peer `BytePriorityQueue` (point-to-point, unchanged).
3. Preserve the structural **reject-before-bytes** admission guarantee for both the event ALPN
   and the gossip ALPN. An unadmitted peer never receives event bytes by either path.
4. Rearchitect `PeerManager` so the warm outbound dial set is a small seed subset (HyParView
   partial-view maintenance grows the overlay), not every active member — the change that lets
   N grow past the QUIC-connection-count wall the spike measured at N=40.
5. Keep the engine, the `SyncMessage` protocol, encoding/decoding, the membership fold, the
   admission trait shape, the audit sink vocabulary (additively extended), and the existing
   pull/query variants byte-for-byte unchanged.
6. Re-run the `crates/spike-N40` matrix at N=10/20/40 with the overlay enabled and record the
   same cascade/connectedness/delivery verdict in `crates/spike-N40/NOTES.md` §3.
7. Only **after** the overlay is proven at the target N, raise `MAX_ACTIVE_MEMBERS` behind a
   feature flag (D4/D7) and update `README.md`, `docs/protocol.md`, `docs/getting-started.md`,
   `docs/operations/data-handling.md`, and the threat model to describe the new ceiling.

### Non-goals

- Do **not** change `SyncMessage` encoding, variant set, or wire contract.
- Do **not** change the engine's `on_message` / `on_connect` / `on_disconnect` shapes; the
  transport continues to deliver `Outgoing { peer, msg }` and the engine stays sans-IO.
- Do **not** route pull/query variants (`WantMembership`, `WantRecentChat`, `WantEvents`,
  `AdminTip`, `Heads`, `NotFound`, `ProveCapability`) over gossip. They stay point-to-point.
- Do **not** raise `MAX_ACTIVE_MEMBERS` in the same change that lands the overlay (D4).
- Do **not** adopt gossip as the system of record for the event log. It is a fan-out carrier
  only; the SQLite store and anti-entropy pulls remain canonical (ADR-2 unchanged).
- Do **not** re-open the ADR-1 decision for N≤5. At N≤5 the overlay may be a no-op (events
  still delivered over the warm mesh links) or a coexisting second path; the spike's N=5
  latency/auth verdict is not re-litigated.
- Do **not** perform Git/GitHub operations from this phase. Branching, PRs, and the `#154` /
  `#171` issue comments are the orchestrator's responsibility.

---

## 4. Key design decisions

### D1 — Surgical seam: `Shared::route` branches on `SyncMessage::Events`

The single load-bearing change in the routing layer. Today `Shared::route`
(`transport.rs:382-410`) classifies the variant for priority, encodes the body, looks up the
destination peer's `BytePriorityQueue`, and pushes (dropping silently if no live writer).

After:

```text
pub(crate) fn route(&self, out: &Outgoing) {
    match &out.msg {
        SyncMessage::Events { room_id, frames } => {
            // Events fan-out: broadcast the encoded SyncMessage::Events body on the
            // room's gossip topic. The receiver task re-decodes and feeds the engine
            // via the existing inbound sink (see D8). The body is the *same* canonical
            // CBOR the point-to-point path would have sent, so a peer cannot tell (and
            // does not need to tell) which path delivered it.
            self.gossip.broadcast_events(out.peer, room_id, out.msg.encode());
        }
        _ => {
            // Unchanged: point-to-point per-peer queue for every pull/query variant.
            // existing body ...
        }
    }
}
```

Consequences:

- **No new variant, no new encoding.** `SyncMessage::Events` continues to encode to the same
  canonical CBOR; the gossip `content` bytes are exactly the bytes the queue path would have
  sent. A receiver decodes one frame and hands it to the engine as `on_message(from, Events{..})`,
  identical to today. The engine's existing dedup (G-set by `event_id`) makes a frame delivered
  by both paths idempotent.
- **Classification and budgeting still happen.** The gossip path still consults
  `classify_sync_message` for the audit/log label; the per-frame `MAX_FRAME_BYTES` cap
  (`sync/message.rs:25`) is unchanged, so a too-large batch is dropped before broadcast (same
  behavior as the queue path's `try_push` → `Saturated`).
- **Per-peer ordering is weakened for Events only.** The mesh's per-link ordered stream gave
  per-peer FIFO for free; gossip's epidemic delivery does not. This is acceptable because the
  engine already canonicalizes order on receipt (`(lamport, event_id)` ordering, `prev_events`
  DAG) and re-validates every frame — Events is **already** an unordered, dedup'd, re-validated
  payload set, not an ordered stream the engine trusts. Pull variants, which do rely on
  per-link ordering (a `WantMembership` response must follow the request on the same link),
  stay on the queue path.

### D2 — Auth preservation: gate `GOSSIP_ALPN` with the same `Admission` trait (non-negotiable)

This is the design's security core and directly answers spike-transport §4's measured weakness
("gossip admits the interloper with no auth check"). iroh-gossip 0.101 has **no authenticated
topics** (the spike and `PHASE-0-SPIKE.md` both note authenticated topics are a future 1.0
trigger), so admission cannot live inside gossip itself. It lives one layer down, at the
connection-accept boundary — exactly where `EventProtocolHandler` already enforces it.

Design:

1. Add a gossip ALPN constant (e.g. `GOSSIP_ALPN: &[u8] = b"/iroh-rooms/gossip/1"`) alongside
   `EVENT_ALPN`, with a byte-equality test mirroring `alpn.rs:24`.
2. Spawn the `iroh_gossip::net::Gossip` instance on the same `Endpoint`:
   `Gossip::builder().spawn(endpoint.clone())`.
3. **Wrap** the gossip handler's `ProtocolHandler` in a thin admission gate `GossipProtocolHandler`
   that holds `Arc<Shared>` and:
   - reads `conn.remote_id()` (the QUIC/TLS-proven `device_id`, identical to `handler.rs:52`),
   - on `AdmissionDecision::Reject(cause)`: audits `rejected`, sets `PeerConnState::Unauthorized`,
     `conn.close(REJECT_CODE, b"unauthorized")`, returns `Ok(())` — **before delegating** to the
     inner gossip handler. Zero gossip bytes are exchanged.
   - on `AdmissionDecision::Admit { identity }` / `AdmitProvisional`: delegates `accept(conn)`
     to the inner `Gossip` handler, which then runs its own ALPN handshake.
4. Register on the same router chain:
   `Router::builder(endpoint).accept(EVENT_ALPN, …).accept(GOSSIP_ALPN, gossip_wrapper).accept(PIPE_ALPN, …).accept(iroh_blobs::ALPN, …)`.

Why this preserves reject-before-bytes structurally (not as a runtime check):

- iroh-gossip's swarm traffic between two endpoints is a QUIC connection on `GOSSIP_ALPN`. The
  wrapper closes that connection at `accept()` time before the inner handler runs, identically
  to `EventProtocolHandler::accept` for `EVENT_ALPN`. The gossip layer never sees, reads, or
  forwards a byte from a rejected device.
- The wrapper reuses `Shared::admission` — the same `Arc<dyn Admission>` instance — so the
  decision is byte-identical to the event-plane decision. A device removed from the live
  `MembershipSnapshot` is rejected on both ALPNs in the same tick.
- `REJECT_CODE` is the same constant (`handler.rs:25`), so the dialing side's existing
  reject-detection logic (which keys on this code) works for gossip redials too.

Rejected alternatives:

- **Receiver-side drop on `delivered_from ∉ admitted`.** Violates reject-before-bytes (bytes
  are received then dropped) and is exactly the open-topic weakness the spike measured. **Out.**
- **Capability-proof at topic join.** iroh-gossip 0.101 has no topic-authorization hook; this
  is the future-1.0 revisit trigger, not available now. **Out.**
- **Topic-id secrecy as the boundary.** The 32-byte `TopicId` cannot be the admission boundary
  (see D5 — it is derived from the public `room_id`). Admission is enforced at the connection
  layer; the `TopicId` is a rendezvous point, not a secret. **Out as the boundary; kept as a
  rendezvous mechanism.**

### D3 — PeerManager dials gossip bootstrap seeds, not every active member

This is the change that lets N grow past the QUIC-connection wall. Today
`PeerManager::desired_devices` (`manager.rs:84-99`) returns every Active member device minus
self, and `reconcile` (`manager.rs:122-175`) runs one `dial_loop` per device — N-1 warm links
per node, O(N²) links room-wide. The spike measured the QUIC layer panicking at 1560
in-process connections (N=40). The rearchitecture bounds the warm-link count by a small
constant K and lets HyParView partial-view membership grow the overlay the rest of the way.

Design:

1. Introduce a configured constant `GOSSIP_BOOTSTRAP_SEEDS: usize` (recommend K=3 for v1;
   configurable, derived from room size heuristics in a later phase). 
2. `PeerManager::desired_devices` becomes two sets:
   - `desired_seeds(snapshot, self_device)` — the K Active-member devices (excluding self)
     selected deterministically: the K lowest-bytewise `EndpointId`s, plus the room admin if
     Active and not self. Determinism matters: every node must compute the same seed set from
     the same snapshot so the seeds are mutually reachable.
   - `desired_links(snapshot, self_device)` — the full Active-member set (the current behavior),
     used only for **on-demand** pull dialing, not for warm `dial_loop`s.
3. `PeerManager::reconcile`:
   - starts/stops warm `dial_loop`s only for `desired_seeds`;
   - subscribes/unsubscribes the room gossip topic as the snapshot's Active set changes (so a
     newly-Active member's device joins the topic and a Removed member's device is unsubscribed
     + its ALPN connection torn down via the existing deauthorize path, `manager.rs:136-154`).
4. Pull/query variants (`WantMembership`, `WantRecentChat`, `WantEvents`) target a specific
   peer. Two cases:
   - **Target is a warm seed** — the frame rides the existing per-peer queue on the warm link.
   - **Target is not a seed** — `Shared::route` triggers an **on-demand dial** over `EVENT_ALPN`
     (the existing `peer::dial_loop` machinery, but spawned lazily and torn down after a
     quiescence timeout) and queues the frame on that transient link. The transient dial is
     bounded by a per-node `MAX_ON_DEMAND_LINKS` (recommend 8 for v1) so the total connection
     count stays `K + MAX_ON_DEMAND_LINKS` per node regardless of N.

This separates two concerns the issue's wording collapses:

- **Overlay maintenance** (gossip bootstrap): K warm links, HyParView grows the rest.
- **Anti-entropy pulls** (engine-driven): on-demand point-to-point, bounded, torn down.

Both ride the same admission gate (D2 for the GOSSIP_ALPN side, the existing
`EventProtocolHandler` for the EVENT_ALPN side).

Lifecycle / risk note (Risk 4): this is a different lifecycle model than today's "every active
member has a warm loop." `on_connect`-driven anti-entropy (`engine.rs:704`) currently assumes
every peer it hears about has a warm link; under D3 the engine is unchanged (it still emits
`Outgoing { peer, msg }`) but the transport decides per-message whether to ride a warm link or
dial on demand. The engine's `peers()` (`SyncTransport::peers`, `transport.rs:940-942`) must
return the union of warm seeds + current gossip neighbors + live on-demand links, so
engine-driven fan-out targets reach real peers. This surface must be pinned with a test.

### D4 — `MAX_ACTIVE_MEMBERS` raise lands LAST, behind a feature flag, in its own change

Risk 3 names this. The cap raise is only meaningful after the overlay is proven at the target N
(AC: spike-N40 re-run passes). Decoupling:

- **Phase A** (this spec, minus the cap): ship the gossip overlay with `MAX_ACTIVE_MEMBERS = 5`
  unchanged. At N≤5 the overlay may be inert (Events still delivered over warm mesh links) or
  coexisting; the cap is not the gate.
- **Phase B** (follow-up change): re-run spike-N40 at N=10/20/40 with the overlay; record
  results in `crates/spike-N40/NOTES.md` §3; if the AC passes, raise the cap.
- **Phase C** (follow-up change): raise `MAX_ACTIVE_MEMBERS` to the target (D7) behind a
  feature flag (e.g. `[features] large_rooms = []` in `iroh-rooms-core`), update
  `ACTIVE_MEMBER_WARNING_THRESHOLD` derivation, and update docs.

Rollback of Phase C is then a one-line revert that does not touch the overlay. Phase A's
overlay can itself be disabled by a feature flag (`[features] gossip_overlay = []` in
`iroh-rooms-net`) that compiles `Shared::route` back to the pure-mesh branch — a clean abort.

### D5 — Gossip topic id is deterministically derived from `room_id` (per-room)

Open question 2. All admitted peers must compute the same `TopicId` without out-of-band
coordination. Derivation:

```text
topic_id = TopicId::from_bytes(
    blake3::derive_subkey(room_id.as_bytes(), b"iroh-rooms/events-gossip/v1")
)
```

(`blake3` is already a direct dep of `iroh-rooms-net`, `Cargo.toml:59`; the same hash the blob
import path cross-checks.)

Consequences:

- `room_id` is already a public 32-byte id carried in every event (`message.rs:155-161`); the
  `TopicId` adds no secret surface. **Topic secrecy is not the admission boundary** — D2 is.
- The derivation is a pure function; any peer that knows the `room_id` (i.e. any admitted peer,
  since admission requires holding the membership sub-DAG) computes the same topic.
- The version tag (`v1`) leaves room for a future topic-key rotation without colliding with the
  v1 mesh path during a migration.

Rejected: per-stream topics (one topic per `stream_id`). Premature for v1; per-room matches
the membership/admission granularity and keeps the topic count = room count. Revisit if
per-room fan-out volume becomes a bottleneck (D6).

### D6 — One gossip topic per room for v1; per-stream is a future optimization

Answers the second half of open question 2. One topic per `room_id`:

- matches the admission boundary (admission is per-room, per-device);
- keeps receiver fan-out simple (one receiver task per room per node);
- matches the existing `Events` frame shape (already scoped by `room_id`).

Per-stream topics are parked as a Phase-5 optimization, triggered only if measurement shows a
single high-volume stream drowning out low-volume ones on the same topic.

### D7 — Target N = 20 for v1; N = 40 as a stretch goal

Answers open question 1. Recommend **N=20** as the v1 target:

- the spike measured mesh dying at N=40; 20 is comfortably above the 5-peer ceiling and below
  the crash threshold, leaving operational headroom;
- HyParView's partial-view size is typically O(log N); at N=20 a partial view of ~5 neighbors
  gives high connectedness with low link pressure;
- the AC's `connectedness >95%` and `delivery >95%` targets are realistic at N=20 on loopback.

N=40 lands as a stretch goal after the overlay is proven at N=20 and the spike-N40 re-run at
N=40 (with the overlay) passes. The cap value itself is set in Phase C (D4), not in Phase A.

### D8 — Map gossip signals onto the existing audit/anti-entropy machinery

iroh-gossip surfaces three load-bearing signals (spike §1):

- `Event::Received(Message { content, delivered_from })` — a frame arrived.
- `Event::Lagged` — the receiver fell behind; messages were dropped.
- `Event::NeighborUp` / `Event::NeighborDown` — swarm membership changed.

Mapping:

- **`Received`**: decode `content` as a `SyncMessage` (the body is the same canonical CBOR the
  sender encoded in D1). Hand the decoded frame to the engine via the **existing inbound
  sink** — `Shared::try_enqueue_inbound(delivered_from_as_peer_id, content_bytes)` — so the
  engine-driver pump processes it through the same `on_message` path, the same
  `provisional_allows` gate, the same counters, and the same audit hooks as a point-to-point
  frame. This keeps the engine sans-IO and the trust boundary (re-validate every `WireEvent`
  on accept) byte-identical. **Critical:** `delivered_from` is the gossip neighbor that handed
  us the frame, *not* necessarily the original author; the engine already keys dedup on
  `event_id`, not on the delivering peer, so this is correct.
- **`Lagged`**: audit `transport.queue.saturated` (queue label `gossip`) and trigger an
  anti-entropy pull — issue `WantMembership` + `WantRecentChat` to a warm seed (D3), exactly
  the recovery path the mesh uses on reconnect. `Lagged` is the gossip equivalent of the mesh's
  "link dropped, re-pull" signal.
- **`NeighborUp/Down`**: audit (`gossip.neighbor_up` / `gossip.neighbor_down`) and feed
  `SyncTransport::peers()` so the engine's fan-out targets reflect live overlay membership.

`GossipReceiver::joined()` is load-bearing (spike surprise 1): the constructor must await it
before returning a subscribed sender, so an immediate `broadcast` does not race ahead of
topology formation. Pin this with a test that broadcasts right after subscribe and asserts
delivery.

### D9 — Late-join history gap is unchanged; gossip is live-fan-out only

The spike measured both backends giving 0/M pre-join events to a newcomer. The existing
anti-entropy pull protocol (`WantMembership` / `WantRecentChat` on `on_connect`) already covers
this and is unchanged. **Gossip is live fan-out only** — it does not replay history, and a
late-joining node pulls the backlog over point-to-point EVENT_ALPN links after admission. Do
not add a gossip-based history replay; that would re-open the ADR-2 (iroh-docs) decision.

---

## 5. Validation, authorization, and error model

### 5.1 Authorization (single source of truth)

`Shared::admission: Arc<dyn Admission>` (`transport.rs:242`) remains the single authorization
point, consulted by **both** ALPN wrappers:

- `EventProtocolHandler::accept` (unchanged, `handler.rs:54`).
- `GossipProtocolHandler::accept` (new, D2) — same `Arc`, same decision.

A device removed from the live `MembershipSnapshot` is rejected on both ALPNs in the same
`reconcile` tick. Production re-point to `SnapshotAdmission` (already in `admission.rs`) is
unchanged.

### 5.2 Validation (defense in depth, unchanged)

Every accepted `Events` frame — whether delivered by gossip or by the queue path — is:

1. re-decoded as a `SyncMessage` at the engine-driver boundary (`node.rs:1233`);
2. each `WireEvent` inside is re-validated by `validate_wire_bytes`
   (`iroh-rooms-core::event::validate`);
3. dedup'd by `event_id` (G-set);
4. folded through the membership lattice (`Status::max`, `Role::min`).

An admitted-but-malicious peer that injects a malformed or unauthorized event over gossip is
rejected by the same path that rejects it over the mesh. **The gossip overlay does not weaken
validation; it widens the delivery surface to admitted peers only.**

### 5.3 Error model

- Per-frame decode failures: logged drop at the engine boundary (`node.rs:1233`), never a
  crash — unchanged.
- Gossip `broadcast` returns `Err`: audit `transport.queue.saturated` (queue `gossip_out`),
  drop the frame (best-effort delivery; the engine re-pulls on the next `on_connect` /
  `Lagged`-triggered anti-entropy). Same shape as `PushError::Saturated` for the queue path
  (`transport.rs:403-406`), minus the link close (gossip has no single link to close).
- Gossip `Lagged`: see D8 — audit + anti-entropy trigger.
- Admission reject on GOSSIP_ALPN: audit `rejected`, `Unauthorized` state, close — same as
  event-plane reject.

### 5.4 Observability (additive)

Extend `AuditSink` (`audit.rs`) with:

- `gossip_broadcast(room_id, bytes)` — a frame was broadcast.
- `gossip_received(device, bytes)` — a frame arrived over gossip (in addition to the existing
  `connected`/`disconnected` events).
- `gossip_lagged(device)` — receiver fell behind.
- `gossip_neighbor_up(device)` / `gossip_neighbor_down(device)`.
- `gossip_topic_rejected(device)` — the GOSSIP_ALPN wrapper rejected a connection (parallel to
  the existing `rejected`).

Extend the engine counters (`SyncCounters`) or add a parallel `GossipCounters`:
`broadcasts`, `received`, `lagged`, `topic_joins`, `topic_rejects`. Surface in
`Node::counters()` (`node.rs`) so the CLI and the spike harness read them.

Extend `Node::peer_states` / `peer_entries` to surface gossip neighbor count alongside the
existing `PeerConnState` trichotomy.

---

## 6. Security, privacy, reliability, performance, migration

### 6.1 Security / auth (the non-negotiable axis)

- **Reject-before-bytes preserved** on both ALPNs (D2). Pinned by a test that mirrors
  `crates/spike-transport/tests/self_check.rs::gossip_admits_interloper_with_no_auth_check` —
  the same probe, run against the *wrapped* gossip handler, must now **fail to join** with a
  `REJECT_CODE` close (AC: "No regression in existing admission-gate tests").
- **Threat model update** (`docs/security/threat-model.md`): add a row for "unadmitted peer
  joins gossip topic" → controlled by D2; note that forwarding visibility (spike §"Forwarding
  visibility") is unchanged because the forwarding set ⊆ admitted set, same as full-mesh.
- **No new trust input.** Gossip `delivered_from` is not trusted for ordering or authorization;
  the engine re-validates every frame.

### 6.2 Privacy

- The forwarding-visibility posture (spike NOTES.md §3, §4) is unchanged: a gossip neighbor
  can read the frames it forwards. Because the neighbor set is restricted to admitted device
  keys (D2), the forwarding set ⊆ admitted set — the same population that could read the frame
  under full-mesh. No regression.
- A room admin's `room_id` is already public (in every event); the derived `TopicId` (D5) adds
  no disclosure.

### 6.3 Reliability

- Gossip is best-effort. A `Lagged` or partitioned peer may miss live frames; the anti-entropy
  pull (D8) is the recovery path. This is the same "engine re-pulls on reconnect" model the
  mesh already uses — the spike measured mesh's redial has the same "may miss while-down
  events" property (`spike-transport/NOTES.md` §6 surprise 3).
- Overlay partition healing is HyParView's job; `Lagged` is the surfacing signal. If
  connectedness drops below 95% (AC threshold), the audit stream shows it before delivery
  collapses — the same observability posture the post-`b0622ec` guardrails enforce.

### 6.4 Performance

- **Fan-out amplification removed.** Today an Events batch of B bytes to N peers costs the
  sender O(N·B) queue bytes and N writer tasks; gossip broadcast costs O(K·B) on the sender
  (K = gossip neighbors, O(log N)) and O(log N) hops end-to-end. This is the headline win the
  spike-N40 collapse predicted.
- **Per-peer queue budgets shrink** for Events: the `outbound_peer_queue_bytes`
  (`transport.rs:250`) is no longer the bottleneck for Events fan-out; it remains the budget
  for pull/query variants. Default cap values (`queue.rs`) need re-measurement at the target N
  but are not in the critical path for fan-out.
- **Connection count** bounded by `K + MAX_ON_DEMAND_LINKS` per node (D3), not O(N).

### 6.5 Provisional join-bootstrap interaction

`provisional_allows` (`node.rs:1558-1571`) already permits `SyncMessage::Events` to a
provisional dialer. Under the overlay, a provisionally-admitted device that subscribes the
gossip topic will receive live `Events` broadcasts — consistent with today's behavior (the
provisional peer may push its `member.joined` and receive `Events`/`AdminTip`/`Heads`/
`NotFound`). The capability-proof gate (`#112`) and the upgrade-on-learn path (`#121`) are
unchanged because they operate at the engine layer, below the transport's routing decision.
**Pin a test**: a provisional dialer receives a gossip `Events` broadcast but its
`WantMembership` is still gated on capability proof.

### 6.6 Migration / rollout / rollback

- **Phase A** (overlay, cap unchanged): feature-flagged (`gossip_overlay` in `iroh-rooms-net`).
  Default off in the first landing; flipped on after the loopback self-check passes. Rollback =
  flip the flag; `Shared::route` compiles to the pure-mesh branch.
- **Phase B** (spike-N40 re-run at N=10/20/40): documentation-only change to
  `crates/spike-N40/NOTES.md` §3. No production code touched.
- **Phase C** (cap raise): feature-flagged (`large_rooms` in `iroh-rooms-core`). Default off
  until Phase B passes the AC. Rollback = one-line revert; the overlay keeps working at N≤5
  regardless.
- **No wire-format migration.** `SyncMessage` is unchanged; old and new nodes interoperate
  (an old node still receives `Events` over the mesh; a new node receives them over gossip or
  mesh, dedup'd by `event_id`).

---

## 7. Implementation steps (ordered, phased)

Each step is independently testable and revertible. Do not implement from this document
without a follow-up build task; these steps are the input to that task.

### Step 0 — Dependency & wiring prep (no behavior change)

1. Add `iroh-gossip = "=0.101.0"` to `crates/iroh-rooms-net/Cargo.toml` `[dependencies]`.
   Verify the version resolves against `iroh = "=1.0.1"` with `cargo tree -p iroh-rooms-net
   -i iroh-gossip` (must show zero version divergence; cross-ref `spike-transport/NOTES.md`
   §1's identical recon).
2. Add `GOSSIP_ALPN: &[u8] = b"/iroh-rooms/gossip/1"` to `crates/iroh-rooms-net/src/alpn.rs`
   with a byte-equality test mirroring `EVENT_ALPN`'s (`alpn.rs:21-25`).
3. Add the `gossip_overlay` cargo feature to `crates/iroh-rooms-net/Cargo.toml` `[features]`,
   default off. All subsequent steps compile behind `#[cfg(feature = "gossip_overlay")]`.

**Verify:** `cargo build -p iroh-rooms-net --features gossip_overlay`;
`cargo fmt --all --check`; `cargo clippy -p iroh-rooms-net --all-targets -- -D warnings`.

### Step 1 — Admission-gated gossip ALPN wrapper (D2)

1. Add `GossipProtocolHandler { shared: Arc<Shared>, inner: iroh_gossip::net::Gossip }` in a
   new `crates/iroh-rooms-net/src/gossip.rs` module. Implement `ProtocolHandler::accept`:
   read `conn.remote_id()`, consult `shared.admission.authorize`, on `Reject` do the exact
   `handler.rs:55-63` sequence (audit + `Unauthorized` + `conn.close(REJECT_CODE, …)` +
   `return Ok(())`); otherwise delegate `self.inner.accept(conn).await`.
2. Manual `Debug` impl mirroring `handler.rs:42-46` (`Shared` holds non-`Debug` trait objects).
3. Wire into `NetTransport::bind` (`transport.rs:714-778`): when the feature is on, spawn
   `let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());` and chain
   `.accept(GOSSIP_ALPN, GossipProtocolHandler::new(shared.clone(), gossip.clone()))`.
4. Store the `Gossip` handle on `NetTransport` (and `Shared`) for Step 3's subscribe.

**Verify:** new unit test `gossip_alpn_rejects_unadmitted_device_before_delegate` — construct
a `Shared` with an `AllowlistAdmission` that does not bind a target device, attempt a
`GOSSIP_ALPN` connection from that device, assert the connection closes with `REJECT_CODE`
and the inner gossip handler is never called (assert via a spy `AuditSink` that records
`rejected`). Mirror `handler.rs`'s reject tests.

### Step 2 — Per-room gossip topic + receiver task (D5, D6, D8)

1. Add `pub(crate) fn events_topic(room_id: &RoomId) -> TopicId` in `gossip.rs` using the D5
   `blake3::derive_subkey` derivation. Pin with a determinism test.
2. Add `GossipMesh` (per-room state held on `Shared`): the `GossipSender`, the receiver task
   `JoinHandle`, and the live neighbor count. One `GossipMesh` per room the node is in.
3. `GossipMesh::spawn`: `gossip.subscribe(events_topic(room_id), bootstrap_seed_ids).await`,
   `topic.split()`, await `receiver.joined()` (spike surprise 1 — load-bearing), spawn a
   receiver task that loops on `receiver.next()`:
   - `Event::Received(msg)` → `shared.try_enqueue_inbound(peer_id_from(msg.delivered_from),
     msg.content.to_vec())` — same inbound sink as the mesh reader task.
   - `Event::Lagged` → audit `transport.queue.saturated` (queue `gossip`) + trigger
     anti-entropy (emit a `WantMembership` + `WantRecentChat` to a warm seed via
     `Shared::route`; the engine driver may need a new `Shared::trigger_anti_entropy` helper).
   - `Event::NeighborUp/Down` → audit + update the live neighbor count.
4. `GossipMesh::broadcast_events(body: Vec<u8>)`: `self.sender.broadcast(Bytes::from(body))`,
   audit `gossip_broadcast`, handle `Err` per §5.3.

**Verify:** unit tests for topic derivation; a loopback integration test (mirroring
`spike-transport/src/gossip.rs::spawn_swarm`) that broadcasts a frame from node A and asserts
node B's inbound sink receives it.

### Step 3 — `Shared::route` branch (D1)

1. Add `gossip: Mutex<HashMap<RoomId, Arc<GossipMesh>>>` to `Shared` (`transport.rs:238-285`).
2. Branch `Shared::route` (`transport.rs:382-410`): for `SyncMessage::Events { room_id, .. }`,
   look up (or lazily spawn) the room's `GossipMesh` and call `broadcast_events(out.msg.encode())`;
   for all other variants, the existing per-peer queue path, byte-identical.
3. Keep `classify_sync_message` and `MAX_FRAME_BYTES` consulted on both paths.

**Verify:** extend `transport.rs`'s `route_*` unit tests (`transport.rs:1111-1242`) with:
`route_broadcasts_events_via_gossip_when_mesh_present` and
`route_routes_pull_variants_via_per_peer_queue_unchanged`.

### Step 4 — PeerManager rearchitecture (D3)

1. Add `GOSSIP_BOOTSTRAP_SEEDS` and `MAX_ON_DEMAND_LINKS` constants (recommend 3 and 8).
2. Split `PeerManager::desired_devices` into `desired_seeds` (K lowest-bytewise Active
   devices + admin) and `desired_links` (full Active set, for on-demand pulls).
3. `PeerManager::reconcile` starts/stops warm `dial_loop`s only for `desired_seeds`; also
   subscribes/unsubscribes the room `GossipMesh` as the Active set changes.
4. `Shared::route` for pull variants targeting a non-seed peer: trigger an on-demand dial
   (spawn a `dial_loop` with a quiescence-timeout self-teardown), bounded by
   `MAX_ON_DEMAND_LINKS`.
5. `SyncTransport::peers` returns warm seeds ∪ live on-demand links ∪ gossip neighbors.

**Verify:** extend `manager.rs` tests (`manager.rs:382-540`) — `desired_seeds` is
deterministic, K-bounded, includes admin, excludes self; reconcile starts exactly K warm
loops; on-demand dial path is bounded.

### Step 5 — Observability (§5.4)

1. Extend `AuditSink` (`audit.rs`) with the gossip events; implement on `TracingAudit`.
2. Add `GossipCounters` (or extend `SyncCounters`); surface in `Node::counters`.
3. Extend `Node::peer_states` / `peer_entries` with gossip neighbor count.

**Verify:** unit test that a broadcast produces a `gossip_broadcast` audit event and bumps the
counter; a `Lagged` produces `transport.queue.saturated` (queue `gossip`).

### Step 6 — Regression gates (AC: "No regression in #126 or #136 tests")

Run the existing test suites unchanged and green:

- `cargo test -p iroh-rooms-net` (includes `handler.rs`, `manager.rs`, `transport.rs`,
  `node.rs::loopback`, the generation-guard tests for #126, and the dial-stomp tests for #136).
- `cargo test -p iroh-rooms-core` (engine, membership fold, `MAX_ACTIVE_MEMBERS` enforcement).
- The spike-transport N≤5 self-check still passes (`crates/spike-transport/tests/self_check.rs`)
  — the overlay does not change the N≤5 behavior the spike measured.

### Step 7 — Phase B: spike-N40 re-run at N=10/20/40 (AC)

1. Extend `crates/spike-N40` to enable the `gossip_overlay` feature on the `iroh-rooms-net`
   dep, and add a `--overlay {mesh,gossip}` flag to the `matrix` subcommand.
2. Re-run the matrix at N=10/20/40 × {idle, 0.1, 1, 5 events/s} with `--overlay gossip`.
3. Record the results in `crates/spike-N40/NOTES.md` §3 alongside the existing N=5/N=40-mesh
   rows, with the same cascade/connectedness/delivery rubric.
4. The AC passes iff: no cascade at 1 event/s, connectedness >95%, delivery >95% at every N.

### Step 8 — Phase C: cap raise (only after Step 7 passes)

1. Add `large_rooms` feature to `crates/iroh-rooms-core/Cargo.toml`; under the feature, set
   `MAX_ACTIVE_MEMBERS = 20` (D7 target); recompute `ACTIVE_MEMBER_WARNING_THRESHOLD`.
2. Update `README.md` (the "Small rooms only (≤5 members)" paragraph at README.md:102), 
   `docs/protocol.md` (`room_full` row at line 581), `docs/getting-started.md` (line 1106),
   `docs/operations/data-handling.md` (lines 79-80), `docs/security/threat-model.md`.
3. Add a `CHANGELOG.md` entry under `crates/iroh-rooms/CHANGELOG.md`.

**Verify:** `cargo test --workspace --all-targets --all-features`; full
`scripts/verify.sh` green.

---

## 8. Acceptance criteria

Copied verbatim from issue #171, with the spec section that satisfies each:

- [ ] **iroh-gossip integrated; gossip topic restricted to admitted device keys** — Step 0, D2,
      Step 1. Pinned by `gossip_alpn_rejects_unadmitted_device_before_delegate`.
- [ ] **`Shared::route` branches: Events → gossip broadcast; all other variants → per-peer
      queue (unchanged)** — D1, Step 3. Pinned by the two new `route_*` tests.
- [ ] **PeerManager dials gossip bootstrap seeds, not every active member** — D3, Step 4.
      Pinned by the `desired_seeds_*` tests.
- [ ] **`MAX_ACTIVE_MEMBERS` raised (target TBD)** — D4, D7, Step 8 (Phase C, gated).
- [ ] **spike-N40 re-run at N=10/20/40 shows: no cascade at 1 event/s, connectedness >95%,
      delivery >95%** — Step 7 (Phase B).
- [ ] **No regression in existing admission-gate tests (reject-before-bytes still structural)**
      — Step 6 + the new GOSSIP_ALPN reject test (Step 1).
- [ ] **No regression in #126 or #136 tests** — Step 6.

---

## 9. Risks (from the issue, with mitigations)

| # | Risk | Severity | Mitigation in this spec |
|---|---|---|---|
| 1 | Admission-gate regression: a path where an unadmitted peer receives event bytes over gossip | **high** | D2 makes the GOSSIP_ALPN wrapper structurally reject-before-bytes, identical to `EventProtocolHandler`. Pinned by a test mirroring the spike's `gossip_admits_interloper_with_no_auth_check`, inverted. |
| 2 | `iroh-gossip` version compat with `iroh = "=1.0.1"` | medium | Pre-verified by `crates/spike-transport/NOTES.md` §1 (`iroh-gossip = "=0.101.0"` builds against `iroh 1.0.1` with zero API drift). Step 0 re-verifies with `cargo tree`. |
| 3 | `MAX_ACTIVE_MEMBERS` raise lands too early | medium | D4: Phase C is a separate, feature-flagged change, lands only after Phase B's spike-N40 re-run passes the AC. Rollback is one-line. |
| 4 | PeerManager rearchitecture (overlay membership vs full-mesh lifecycle) | medium | D3 separates warm-seed links from on-demand pull links; `SyncTransport::peers` returns the union; engine stays sans-IO. The lifecycle is pinned by tests for both dial sets. |
| 5 (added) | Gossip `Lagged` → silent delivery gap | medium | D8 maps `Lagged` to an audit + anti-entropy pull, the same recovery path the mesh uses on reconnect. |
| 6 (added) | Per-peer FIFO ordering regression for `Events` | low | The engine already canonicalizes order on receipt (`(lamport, event_id)` + `prev_events` DAG) and dedups by `event_id`; `Events` was never an ordered stream the engine trusts. Pull variants, which do rely on per-link FIFO, stay on the queue path (D1). |
| 7 (added) | `GossipReceiver::joined()` race (spike surprise 1) | low | Step 2 awaits `joined()` before returning a subscribed sender; pinned by a "broadcast immediately after subscribe" test. |
| 8 (added) | Forwarding-visibility posture change | low | D2 restricts the gossip neighbor set to admitted device keys; forwarding set ⊆ admitted set, same as full-mesh. No regression; documented in the threat-model update (§6.1). |

---

## 10. Open questions

1. **Target N (D7 answers: 20 for v1, 40 stretch).** Confirm with product before Phase C lands.
   The spec recommends N=20 because it is comfortably above the 5-peer ceiling and below the
   spike's N=40 crash threshold; N=40 follows after the overlay is proven at N=20.
2. **Gossip topic key (D5 answers: `blake3`-derived from `room_id`, per-room).** Confirm the
   derivation tag (`b"iroh-rooms/events-gossip/v1"`) is stable across the v1 series; a future
   rotation needs a new tag and a migration.
3. **Bootstrap peer selection (D3 answers: K=3 lowest-bytewise Active devices + admin,
   reused `resolve_addr + admission gate`).** Confirm K=3 is the right constant for the target
   N; revisit if connectedness <95% at N=20 in Phase B.
4. **Should `Events` fan-out be dual-path (mesh + gossip) at N≤5, or gossip-only?** The spec
   is neutral: the engine's `event_id` dedup makes both correct. Recommend gossip-only when
   the overlay is on (simpler; matches the #154 decision's intent), with the mesh path
   retained behind the `gossip_overlay` feature flag for fallback. Confirm before Phase A
   landing.
5. **`SyncTransport::peers` semantics under D3.** Should it return gossip neighbors that are
   not warm seeds? The spec says yes (so engine fan-out targets reach them), but this widens
   the engine's view of "connected peers." Confirm the engine's `on_disconnect` behavior is
   correct for a gossip neighbor that goes down without a point-to-point link close.

---

## 11. Traceability

| Source | What it requires | Where this spec addresses it |
|---|---|---|
| `#154` (closed GO decision) | Gossip overlay IS warranted for N>5; surgical seam at `Shared::route`; topic inherits admission from the connection-level gate | §1, D1, D2, §4 |
| `#145` / `crates/spike-N40/NOTES.md` | N=5 all rates pass; N=40 QUIC panic at 1560 connections; pre-`b0622ec` N=25 = 661 MB backlog, `accepted=0` | §1, §6.4, Step 7 |
| `crates/spike-transport/NOTES.md` §4 | Auth axis decisive; gossip open-topic admits interloper; mesh refuses-before-bytes | D2 (closes the open-topic weakness), §6.1 |
| `crates/spike-transport/src/gossip.rs` | The iroh-gossip 0.101 API surface to integrate; the `joined()` load-bearing wait | Step 2, D8 |
| `PHASE-0-SPIKE.md` ADR-1 | Full-mesh for N≤5; gossip parked unless larger-room measurements justify revisiting | §3 non-goals (does not re-open N≤5); §4 (the larger-room measurement is now in) |
| `transport.rs:382-410` (`Shared::route`) | The surgical seam | D1, Step 3 |
| `handler.rs:49-141` | The reject-before-bytes admission gate to clone for GOSSIP_ALPN | D2, Step 1 |
| `manager.rs:84-175` | The PeerManager rearchitecture site | D3, Step 4 |
| `membership/model.rs:14` (`MAX_ACTIVE_MEMBERS`) | The cap to raise last | D4, D7, Step 8 |
| `PRD.v0.3.md` §843-846 | "A room with 5 participants remains usable. This is the declared ceiling… measured reality above the ceiling: at N=25 the system does not deliver" | §1, §6.4 (the overlay is what lets the ceiling move) |
| `docs/security/threat-model.md` | T18 (withheld removal delays revocation) and the admission rows | §6.1 (threat-model update in Step 8) |

---

## 12. Out of scope (explicit)

- Per-stream gossip topics (D6 parks this).
- Gossip-based history replay (D9 — would re-open ADR-2).
- Authenticated gossip topics (PHASE-0-SPIKE.md future-1.0 revisit trigger; D2 makes this
  unnecessary for v1).
- Real-NAT measurement at N>5 (the spike-N40 harness is loopback; real-NAT is Gate A,
  `crates/spike-nat`, a separate owed run).
- Multi-process deployment automation for `demo1`/`demo2`/`demo3` (optional infra; not
  required for any AC).
- Git/GitHub operations (orchestrator's responsibility).
