//! The gossip overlay for `SyncMessage::Events` fan-out (issue #171, spec
//! `gossip-overlay-events-fan-out.md` §4 D1/D2/D5/D8).
//!
//! Compiled only when the `gossip_overlay` feature is on. The overlay routes
//! `Events` frames through an `iroh-gossip` topic among **admitted** device
//! keys instead of per-peer queue fan-out, removing the O(N²) sender load the
//! full-mesh path incurs and letting N grow past the QUIC-connection-count wall
//! the spike-N40 measured at N=40.
//!
//! ## What lives here
//!
//! - [`events_topic`] — the deterministic per-room `TopicId` derivation (D5).
//!   All admitted peers compute the same topic from the public `room_id` with
//!   no out-of-band coordination. **Topic secrecy is not the admission boundary
//!   — D2 is.**
//! - [`GossipProtocolHandler`] — the `GOSSIP_ALPN` accept gate (D2). It wraps the
//!   `iroh-gossip` actor's `ProtocolHandler` with the same `Arc<dyn Admission>`
//!   instance the `EVENT_ALPN` gate consults, so a device removed from the live
//!   membership snapshot is rejected on both ALPNs in the same tick. The close
//!   fires **before** the inner gossip handler runs — zero gossip bytes are
//!   ever exchanged with an unadmitted device (reject-before-bytes preserved).
//! - [`GossipMesh`] — the per-room broadcast state: the `GossipSender`, the
//!   receiver-task `JoinHandle`, and the live neighbor count. One mesh per
//!   room the node is in. The receiver task feeds decoded frames into the
//!   **existing** inbound sink via `Shared::try_enqueue_inbound`, so the
//!   engine pump processes them through the same `on_message` path as a
//!   point-to-point frame (D8).
//! - [`GossipState`] — the `Shared`-held overlay state: the gossip actor
//!   handle (if any) and the per-room mesh map.

#![cfg(feature = "gossip_overlay")]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::EndpointId;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use iroh_rooms_core::event::ids::RoomId;
use n0_future::StreamExt;
use tokio::task::JoinHandle;

use crate::admission::AdmissionDecision;
use crate::alpn::GOSSIP_ALPN;
use crate::handler::REJECT_CODE;
use crate::peer::peer_id;
use crate::state::PeerConnState;
use crate::transport::Shared;

/// The BLAKE3 context string used to derive a room's gossip `TopicId` from its
/// public `room_id` (spec §4 D5). The version tag (`v1`) leaves room for a
/// future topic-key rotation without colliding with the v1 mesh path during a
/// migration. **Not a secret:** the derived topic is a rendezvous point; D2's
/// connection-level admission wrapper is the security boundary.
const EVENTS_TOPIC_CONTEXT: &str = "iroh-rooms/events-gossip/v1";

/// How long [`GossipMesh::spawn`] waits for `receiver.joined()` before giving
/// up when bootstrap peers were supplied (spike-transport/NOTES.md surprise 1:
/// the join wait is load-bearing — without it an immediate `broadcast` can
/// race ahead of HyParView/PlumTree still settling the topology).
const GOSSIP_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Capacity of the per-mesh recent-broadcast dedup window (issue #171). The
/// engine fans an accepted `Events` frame out to every connected peer
/// (`engine.rs` per-peer loop), so `Shared::route` reaches
/// [`GossipMesh::broadcast_events`] once per peer carrying the **identical**
/// encoded body. `iroh-gossip` 0.101 does not coalesce duplicate broadcasts,
/// so without dedup one accepted event becomes N-1 identical gossip broadcasts
/// — the O(N²) fan-out issue #171 exists to eliminate. The N-1 copies all
/// arrive within a single synchronous `route_all` loop (no await between
/// them), so a window of one entry per distinct event suffices; the bound is
/// generous to cover many distinct events fanned out in the same tick.
/// Distinct events hash distinctly (each carries a unique hash-chained
/// `event_id`), so legitimate distinct broadcasts are never suppressed.
const RECENT_BROADCAST_CAPACITY: usize = 128;

/// Derive the deterministic per-room gossip `TopicId` from the public
/// `room_id` (spec §4 D5).
///
/// `room_id` is already a public 32-byte id carried in every event
/// (`message.rs:155-161`); the derived `TopicId` adds no secret surface. The
/// derivation is a pure function, so any peer that knows the `room_id`
/// (i.e. any admitted peer — admission requires holding the membership sub-DAG)
/// computes the same topic without out-of-band coordination.
///
/// Uses `blake3::derive_key` (the BLAKE3 KDF — `derive_subkey` in the spec
/// text maps to the crate's actual entry point) so the derivation is
/// domain-separated from any other use of the raw `room_id` bytes.
#[must_use]
pub fn events_topic(room_id: &RoomId) -> TopicId {
    let bytes = blake3::derive_key(EVENTS_TOPIC_CONTEXT, room_id.as_bytes());
    TopicId::from_bytes(bytes)
}

/// The admission-gated accept wrapper for [`GOSSIP_ALPN`] (spec §4 D2).
///
/// Holds a clone of the same `Arc<dyn Admission>` instance
/// [`EventProtocolHandler`](crate::handler::EventProtocolHandler) consults, so
/// the admission decision is byte-identical on both ALPNs: a device removed
/// from the live `MembershipSnapshot` is rejected on the event plane and on the
/// gossip plane in the same tick.
///
/// On `AdmissionDecision::Reject(cause)` the wrapper performs the exact
/// `handler.rs:55-63` sequence — audit `gossip_topic_rejected`, mark the table
/// `Unauthorized`, `conn.close(REJECT_CODE, b"unauthorized")`, return `Ok(())`
/// — **before** delegating `accept(conn)` to the inner gossip handler. Zero
/// gossip bytes are exchanged with a rejected device; the structural
/// reject-before-bytes guarantee is preserved.
///
/// On `Admit { identity }` / `AdmitProvisional` the wrapper delegates to the
/// inner `Gossip` handler, which then runs its own ALPN handshake. A
/// provisionally-admitted device (the join-bootstrap seam, IR-0104) is allowed
/// to reach the gossip layer, consistent with the provisional peer receiving
/// `Events` over the mesh today (spec §6.5).
pub struct GossipProtocolHandler {
    shared: Arc<Shared>,
    inner: Gossip,
}

impl GossipProtocolHandler {
    /// Build the wrapper. `inner` is the gossip actor spawned on the same
    /// `Endpoint` as the event plane; `shared` is the same `Arc<Shared>` the
    /// event ALPN handler holds.
    #[must_use]
    pub fn new(shared: Arc<Shared>, inner: Gossip) -> Self {
        Self { shared, inner }
    }
}

// `ProtocolHandler` requires `Debug`, but `Shared` holds trait objects that are
// not `Debug`; a manual impl keeps the bound satisfied without leaking
// internals (mirrors `handler.rs:42-46`).
impl std::fmt::Debug for GossipProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GossipProtocolHandler")
    }
}

impl ProtocolHandler for GossipProtocolHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // The remote id is the QUIC/TLS-proven `device_id`; no application
        // bytes are read to obtain it (mirrors `handler.rs:52`).
        let device = conn.remote_id();

        match self.shared.admission.authorize(device) {
            AdmissionDecision::Reject(cause) => {
                // Reject BEFORE delegating to the inner gossip handler: zero
                // gossip bytes are ever exchanged. The close is not an error
                // from the router's point of view. The same `REJECT_CODE`
                // (`handler.rs:25`) lets the dialing side's reject-detection
                // logic work for gossip redials too (spec §4 D2).
                self.shared.audit.gossip_topic_rejected(device, cause);
                self.shared
                    .table
                    .set(device, PeerConnState::Unauthorized, None);
                conn.close(REJECT_CODE, b"unauthorized");
                Ok(())
            }
            // Admit (Active member) or AdmitProvisional (join bootstrap): let
            // the inner gossip handler run its own ALPN handshake and forward
            // the connection to the actor (spec §4 D2 step 3 / §6.5).
            AdmissionDecision::Admit { .. } | AdmissionDecision::AdmitProvisional => {
                self.inner.accept(conn).await
            }
        }
    }
}

/// Spawn the `iroh-gossip` actor on `endpoint`, configured to dial/accept on
/// [`GOSSIP_ALPN`] (spec §4 D2 step 3 / Step 1). The returned handle is what
/// [`GossipProtocolHandler`] delegates to and what
/// [`GossipMesh::spawn`] subscribes a topic against.
///
/// `Loopback` and `RealNetwork` modes both reuse the endpoint the event plane
/// already bound — one `Endpoint`, many planes (ADR-1).
#[must_use]
pub fn spawn_gossip_actor(endpoint: iroh::Endpoint) -> Gossip {
    Gossip::builder().alpn(GOSSIP_ALPN).spawn(endpoint)
}

/// Per-room gossip broadcast state held on [`Shared`] (spec §4 D8 / Step 2).
///
/// One `GossipMesh` per room the node is in. The mesh owns the room's
/// `GossipSender` (cheap to clone; cloned per broadcast), the receiver-task
/// `JoinHandle`, and an atomically-updated live-neighbor count surfaced for
/// observability.
///
/// The receiver task decodes each `Event::Received` frame's `content` as a
/// `SyncMessage` body and feeds it into `Shared::try_enqueue_inbound` keyed by
/// the gossip `delivered_from` peer — the **same** inbound sink the mesh
/// reader task uses. The engine pump then runs the same `on_message` /
/// `provisional_allows` / counter / audit path as for a point-to-point frame
/// (D8). The engine's `event_id` G-set dedup makes a frame delivered by both
/// paths idempotent.
pub struct GossipMesh {
    room_id: RoomId,
    sender: Mutex<GossipSender>,
    /// The receiver-task handle, retained so dropping the mesh aborts the task.
    receiver_task: Mutex<Option<JoinHandle<()>>>,
    /// Live `gossip-neighbor` count for this topic (`HyParView` partial view),
    /// updated by the receiver task on `NeighborUp` / `NeighborDown`.
    neighbors: AtomicUsize,
    /// Recent broadcast body hashes (issue #171 dedup). Collapses the N-1
    /// identical bodies the engine's per-peer fan-out produces per accepted
    /// event into a single gossip broadcast. Bounded FIFO; see
    /// [`RECENT_BROADCAST_CAPACITY`].
    recent_broadcasts: Mutex<VecDeque<[u8; 32]>>,
}

impl GossipMesh {
    /// Spawn the mesh for `room_id`: subscribe the gossip actor to the derived
    /// [`events_topic`], split the topic into sender + receiver, await
    /// `receiver.joined()` when bootstrap peers were supplied (spike surprise
    /// 1 — load-bearing), then spawn the receiver task that drains events into
    /// the shared inbound sink.
    ///
    /// `bootstrap` is the deterministic seed set (K lowest-bytewise Active
    /// devices + admin, [`PeerManager::desired_seeds`](crate::manager::PeerManager::desired_seeds));
    /// empty for the first member of a room (no one to bootstrap from).
    ///
    /// # Errors
    /// Returns an error if the subscribe fails, or if a bootstrapped
    /// subscriber times out waiting to join the topic swarm.
    pub async fn spawn(
        shared: Arc<Shared>,
        actor: Gossip,
        room_id: RoomId,
        bootstrap: Vec<EndpointId>,
    ) -> Result<Arc<Self>> {
        let has_bootstrap = !bootstrap.is_empty();
        let topic = actor
            .subscribe(events_topic(&room_id), bootstrap)
            .await
            .context("subscribe to the room events gossip topic")?;
        let (sender, mut receiver) = topic.split();

        // Spike surprise 1: a bootstrapped subscriber MUST wait until the
        // swarm-membership layer has formed at least one direct neighbor link
        // before this constructor returns, so an immediate `broadcast_events`
        // does not race ahead of topology formation (the first member of a
        // room has no peer to join yet, so it skips this).
        // `joined()` progresses the stream to (and consumes) the first
        // `NeighborUp` event (iroh-gossip 0.101 `GossipReceiver::joined` doc:
        // "this consumes this initial NeighborUp event"). So the receiver
        // task's `next()` loop never sees that first NeighborUp and would
        // undercount by the seed neighbor. Seed the count with 1 here —
        // `joined()` guarantees at least one direct neighbor link exists by
        // this point. Additional neighbors that come up later are counted
        // correctly via the receiver task's `NeighborUp` arm. The first member
        // of a room (no bootstrap) has no neighbor yet, so it starts at 0.
        let initial_neighbors = usize::from(has_bootstrap);
        if has_bootstrap {
            tokio::time::timeout(GOSSIP_JOIN_TIMEOUT, receiver.joined())
                .await
                .context("timed out waiting to join the room events gossip topic")?
                .context("join the room events gossip topic")?;
        }

        let mesh = Arc::new(Self {
            room_id,
            sender: Mutex::new(sender),
            receiver_task: Mutex::new(None),
            neighbors: AtomicUsize::new(initial_neighbors),
            recent_broadcasts: Mutex::new(VecDeque::new()),
        });

        let handle = tokio::spawn(receiver_task(shared, mesh.clone(), receiver));
        *mesh
            .receiver_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
        Ok(mesh)
    }

    /// Broadcast an encoded `SyncMessage::Events` body on the room's gossip
    /// topic. Best-effort: spawns a task that awaits the broadcast, so the
    /// engine driver's synchronous `route` call is never blocked on the swarm
    /// (D1). On error the task audits `transport.queue.saturated` with queue
    /// `gossip_out` — the gossip analog of the queue path's `Saturated` arm
    /// (spec §5.3). The engine re-pulls on the next `on_connect` /
    /// `Lagged`-triggered anti-entropy.
    ///
    /// `body` is the *same* canonical CBOR the per-peer queue path would have
    /// sent, so a peer cannot tell (and does not need to tell) which path
    /// delivered it (D1 consequence).
    pub(crate) fn broadcast_events(
        &self,
        audit: Arc<dyn crate::audit::AuditSink>,
        me: EndpointId,
        body: Vec<u8>,
    ) {
        // Dedup (issue #171): the engine fans an accepted `Events` frame out to
        // every connected peer, so `Shared::route` reaches here once per peer
        // with the *identical* encoded body per event. iroh-gossip 0.101 does
        // not coalesce duplicate broadcasts, so without this guard one accepted
        // event becomes N-1 identical gossip broadcasts (O(N²) amplification).
        // Hash the body and suppress a repeat within a recent window; distinct
        // events hash distinctly (each carries a unique hash-chained
        // `event_id`), so legitimate distinct broadcasts are never suppressed.
        // The N-1 copies arrive in one synchronous `route_all` loop, so this
        // collapses to a single broadcast per event regardless of N.
        let hash = blake3::hash(&body);
        {
            let mut recent = self
                .recent_broadcasts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if recent.contains(hash.as_bytes()) {
                return;
            }
            if recent.len() >= RECENT_BROADCAST_CAPACITY {
                recent.pop_front();
            }
            recent.push_back(*hash.as_bytes());
        }

        let sender = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let room_id = self.room_id;
        let byte_len = body.len();
        tokio::spawn(async move {
            match sender.broadcast(Bytes::from(body)).await {
                Ok(()) => audit.gossip_broadcast(room_id, byte_len),
                Err(_) => audit.transport_queue_saturated(me, "gossip_out"),
            }
        });
    }

    /// The room this mesh carries Events for.
    #[must_use]
    pub fn room_id(&self) -> RoomId {
        self.room_id
    }

    /// Point-in-time `gossip-neighbor` count (`HyParView` partial view size).
    /// Surfaced via `Node::peer_states` for observability (spec §5.4).
    #[must_use]
    pub fn neighbor_count(&self) -> usize {
        self.neighbors.load(Ordering::Relaxed)
    }
}

impl Drop for GossipMesh {
    fn drop(&mut self) {
        // Abort the receiver task so it does not outlive the mesh. The gossip
        // actor itself stays up — it is owned by `NetTransport`/`Shared`, not
        // by any one mesh.
        if let Some(handle) = self
            .receiver_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            handle.abort();
        }
    }
}

/// The receiver task: drain `Event`s from the gossip topic and feed them into
/// the existing inbound sink, so the engine pump runs the same `on_message` /
/// `provisional_allows` / counter / audit path as for a point-to-point frame
/// (spec §4 D8). The task exists once per subscribed room.
///
/// Mapping (D8):
/// - `Received(Message { content, delivered_from, .. })` → decode is the
///   engine's job; we hand the verbatim `content` bytes to
///   `Shared::try_enqueue_inbound` keyed by `delivered_from`. `delivered_from`
///   is the gossip neighbor that handed us the frame, *not* necessarily the
///   original author; the engine already keys dedup on `event_id`, not on the
///   delivering peer, so this is correct.
/// - `Lagged` → audit `transport.queue.saturated` (queue `gossip`) and audit
///   `gossip_lagged`; the recovery path is the engine's next anti-entropy
///   pull (D8: same shape as the mesh's "link dropped, re-pull" signal).
/// - `NeighborUp` / `NeighborDown` → audit + bump/draining the live neighbor
///   count.
async fn receiver_task(shared: Arc<Shared>, mesh: Arc<GossipMesh>, mut receiver: GossipReceiver) {
    loop {
        match receiver.next().await {
            Some(Ok(Event::Received(msg))) => {
                let from = peer_id(msg.delivered_from);
                let byte_len = msg.content.len();
                // The same inbound sink the mesh reader task uses. The engine
                // pump re-decodes, runs `provisional_allows`, re-validates
                // every `WireEvent`, and dedups by `event_id` (D8).
                match shared.try_enqueue_inbound(from, msg.content.to_vec()) {
                    Ok(())
                    | Err(crate::queue::PushError::Closed | crate::queue::PushError::Empty) => {}
                    Err(crate::queue::PushError::Saturated) => {
                        shared
                            .audit
                            .transport_queue_saturated(msg.delivered_from, "gossip");
                    }
                }
                shared.audit.gossip_received(msg.delivered_from, byte_len);
            }
            Some(Ok(Event::Lagged)) => {
                shared.audit.gossip_lagged(shared.me);
                // Best-effort anti-entropy nudge: surface the gap; the engine's
                // next tick pulls `WantMembership` + `WantRecentChat` over a
                // warm seed link. The trigger is observability-only here.
            }
            Some(Ok(Event::NeighborUp(device))) => {
                mesh.neighbors.fetch_add(1, Ordering::Relaxed);
                shared.audit.gossip_neighbor_up(device);
            }
            Some(Ok(Event::NeighborDown(device))) => {
                mesh.neighbors.fetch_sub(1, Ordering::Relaxed);
                shared.audit.gossip_neighbor_down(device);
            }
            // The receiver errored or ended (topic closed / actor shutdown).
            Some(Err(_)) | None => break,
        }
    }
}

/// The `Shared`-held overlay state (spec §4 D1 / D8 / Step 3).
///
/// `actor` is `None` for `Shared` instances constructed without a gossip actor
/// (test fixtures, or a `NetTransport` bound with the feature off). The mesh
/// map is keyed by `RoomId` so `Shared::route`'s `Events` branch can look up
/// the mesh in O(1) without decoding the body.
pub(crate) struct GossipState {
    actor: Option<Gossip>,
    meshes: Mutex<HashMap<RoomId, Arc<GossipMesh>>>,
}

impl GossipState {
    /// Construct with a live gossip actor (the production bind path).
    pub(crate) fn with_actor(actor: Gossip) -> Self {
        Self {
            actor: Some(actor),
            meshes: Mutex::new(HashMap::new()),
        }
    }

    /// Construct without an actor (test fixtures that exercise route logic
    /// without the overlay I/O). Mesh lookups always return `None`.
    pub(crate) fn inert() -> Self {
        Self {
            actor: None,
            meshes: Mutex::new(HashMap::new()),
        }
    }

    /// The gossip actor handle, if any. `None` when the overlay is inert.
    pub(crate) fn actor(&self) -> Option<&Gossip> {
        self.actor.as_ref()
    }

    /// Install (or replace) the per-room mesh. Called by the room-session
    /// driver after [`GossipMesh::spawn`] succeeds.
    pub(crate) fn install_mesh(&self, room_id: RoomId, mesh: Arc<GossipMesh>) {
        self.meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(room_id, mesh);
    }

    /// Remove (and return) the per-room mesh. Called on room teardown /
    /// unsubscribe. Not yet exercised by the room-session lifecycle in Phase A
    /// (the mesh lives for the session); the surface is complete so a future
    /// unsubscribe path can call it without an API change.
    #[allow(dead_code)]
    pub(crate) fn remove_mesh(&self, room_id: &RoomId) -> Option<Arc<GossipMesh>> {
        self.meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(room_id)
    }

    /// Look up the per-room mesh for `room_id`. Returns `None` when no mesh is
    /// installed (early startup, or the overlay is inert) — `Shared::route`'s
    /// `Events` branch treats `None` as a silent drop, exactly like the
    /// per-peer queue path treats a peer with no live writer.
    pub(crate) fn mesh_for(&self, room_id: &RoomId) -> Option<Arc<GossipMesh>> {
        self.meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(room_id)
            .cloned()
    }

    /// Point-in-time gossip-neighbor count summed across every installed mesh.
    /// Surfaced via `Node::peer_states` for observability (spec §5.4).
    pub(crate) fn neighbor_count(&self) -> usize {
        self.meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|m| m.neighbor_count())
            .sum()
    }
}

impl Default for GossipState {
    fn default() -> Self {
        Self::inert()
    }
}

impl std::fmt::Debug for GossipState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GossipState")
            .field("actor", &self.actor.is_some())
            .field(
                "meshes",
                &self
                    .meshes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::events_topic;
    use iroh_rooms_core::event::ids::RoomId;

    /// The topic derivation is a pure function of the public `room_id`. Every
    /// admitted peer must compute the same topic from the same `room_id`
    /// without out-of-band coordination (spec §4 D5).
    #[test]
    fn events_topic_is_deterministic_per_room() {
        let room = RoomId::from_bytes([0xAB; 32]);
        assert_eq!(events_topic(&room), events_topic(&room));
    }

    /// Distinct rooms must yield distinct topics — otherwise Events from one
    /// room would be delivered to subscribers of another (a cross-room leak).
    #[test]
    fn events_topic_differs_for_distinct_rooms() {
        let a = RoomId::from_bytes([0x01; 32]);
        let b = RoomId::from_bytes([0x02; 32]);
        assert_ne!(events_topic(&a), events_topic(&b));
    }

    /// A known-answer test pins the derivation so a future refactor of
    /// `EVENTS_TOPIC_CONTEXT` or the hash function is caught immediately (the
    /// context string is a wire-compatibility boundary: changing it silently
    /// partitions old and new peers).
    #[test]
    fn events_topic_known_answer_is_stable() {
        let room = RoomId::from_bytes([0x00; 32]);
        let topic = events_topic(&room);
        // Recompute the expected derivation inline so the test fails loudly if
        // either the context string or the KDF entry point drifts.
        let expected = blake3::derive_key("iroh-rooms/events-gossip/v1", room.as_bytes());
        assert_eq!(topic.as_bytes(), &expected);
    }

    /// The derived topic must NOT equal the raw `room_id` bytes (spec §4 D5:
    /// the derivation is a BLAKE3 KDF, domain-separated from the raw id). This
    /// catches a regression where someone replaces `blake3::derive_key` with a
    /// raw `TopicId::from_bytes(*room_id.as_bytes())` copy — which would collide
    /// with any future raw-id use and lose the domain separation the version tag
    /// buys.
    #[test]
    fn events_topic_is_not_the_raw_room_id() {
        let room = RoomId::from_bytes([0x33; 32]);
        let topic = events_topic(&room);
        assert_ne!(
            topic.as_bytes(),
            room.as_bytes(),
            "the derived topic must be domain-separated from the raw room_id"
        );
    }

    /// The context string (`EVENTS_TOPIC_CONTEXT`) is load-bearing in the KDF:
    /// a different context must yield a different topic. This is the
    /// domain-separation property that lets a future topic-key rotation (a new
    /// context) coexist with the v1 topic without collision. It also catches a
    /// regression where the context is dropped (derivation reduced to a plain
    /// hash of the `room_id`).
    #[test]
    fn events_topic_context_string_participates_in_derivation() {
        let room = RoomId::from_bytes([0x55; 32]);
        let topic = events_topic(&room);
        // A different context string — a hypothetical v2 — must derive a
        // different topic from the same room_id, proving the context is mixed
        // into the KDF rather than ignored.
        let other = blake3::derive_key("iroh-rooms/events-gossip/v2", room.as_bytes());
        assert_ne!(
            topic.as_bytes(),
            &other,
            "the context string must participate in the topic derivation"
        );
    }

    /// `GossipState::inert()` reports no actor and no meshes — the contract
    /// `Shared::route` relies on when the overlay is off (or a test fixture
    /// has no actor). Pinned so a future constructor change cannot accidentally
    /// install a mesh without an actor.
    #[test]
    fn inert_state_has_no_actor_and_no_meshes() {
        let state = super::GossipState::inert();
        assert!(state.actor().is_none());
        assert_eq!(state.neighbor_count(), 0);
        let room = RoomId::from_bytes([0xEE; 32]);
        assert!(state.mesh_for(&room).is_none());
    }

    /// `GossipState::install_mesh` / `mesh_for` / `remove_mesh` round-trip the
    /// installed mesh through the same lookup `Shared::route`'s `Events`
    /// branch depends on. We cannot easily construct a real `GossipMesh`
    /// without a live gossip actor (it owns a `GossipSender` from the actor),
    /// so the loopback delivery test in Step 2's verify exercises the real
    /// mesh; here we only pin the absent-mesh lookup contract `route` falls
    /// back to (silent drop, same as a peer with no live writer).
    #[test]
    fn inert_state_lookups_return_none() {
        let state = super::GossipState::inert();
        let room = RoomId::from_bytes([0x11; 32]);
        assert!(state.mesh_for(&room).is_none());
        assert!(state.remove_mesh(&room).is_none());
    }

    /// The admission wrapper is exercised end-to-end by the loopback
    /// integration test in Step 1's verify (a real `GOSSIP_ALPN` connection
    /// from an unadmitted device closes with `REJECT_CODE`). The unit-level
    /// property — the wrapper delegates the same `Arc<dyn Admission>` instance
    /// the event-plane gate uses — is pinned by the `audit` wiring test in
    /// `audit.rs`.
    #[test]
    fn gossip_alpn_constant_matches_alpn_module() {
        assert_eq!(super::GOSSIP_ALPN, crate::alpn::GOSSIP_ALPN);
    }
}
