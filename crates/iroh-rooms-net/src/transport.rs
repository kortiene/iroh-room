//! [`NetTransport`] — the full-mesh direct-QUIC carrier and the concrete
//! [`SyncTransport`] the landed [`SyncEngine`](iroh_rooms_core::sync::SyncEngine)
//! drives (spec §4.7 / §4.8 / G1 / G6).
//!
//! It owns the one `iroh::Endpoint` (keyed by the node's `device_id` secret) and
//! the `Router` carrying [`EVENT_ALPN`](crate::alpn::EVENT_ALPN), plus the shared
//! state ([`Shared`]) every connection task reads and mutates. The engine sees a
//! plain, synchronous, best-effort [`SyncTransport`]: [`send`](SyncTransport::send)
//! is a non-blocking enqueue onto the destination peer's writer queue (dropped if
//! the peer is offline — the engine re-pulls on reconnect), and
//! [`peers`](SyncTransport::peers) is the set of `Connected` authenticated devices.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
#[cfg(feature = "gossip_overlay")]
use std::sync::Weak;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, QuicTransportConfig, VarInt};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_rooms_core::sync::{Outgoing, PeerId, SyncTransport};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::admission::Admission;
use crate::alpn::EVENT_ALPN;
use crate::audit::AuditSink;
use crate::handler::EventProtocolHandler;
use crate::peer::{dial_loop, peer_id};
use crate::pipe::alpn::PIPE_ALPN;
use crate::pipe::PipeProtocolHandler;
use crate::queue::{
    self, classify_inbound_bytes, classify_sync_message, BytePriorityQueue, BytePriorityReceiver,
    PushError, DEFAULT_PER_PEER_QUEUE_BYTES, DEFAULT_PER_STREAM_QUEUE_BYTES,
};
use crate::state::{ConnEvent, OfflineReason, PeerConnState, PeerEntry, PeerTable};

/// Normal application close code for a locally-initiated disconnect (distinct from
/// [`crate::handler::REJECT_CODE`], which means "unauthorized").
const LOCAL_CLOSE_CODE: VarInt = VarInt::from_u32(0);

/// How many bidirectional QUIC streams a remote endpoint may have open toward us on
/// one connection, set **explicitly** on the endpoint's `QuicTransportConfig` instead
/// of inherited from quinn's default.
///
/// Until now no `TransportConfig` existed anywhere in this workspace, so quinn's
/// default of 100 applied by accident. That default was the first ceiling the Live
/// Pipe Plane hit, and it hit it invisibly: a scale run on real hardware (25 nodes,
/// real QUIC) drove 150 concurrent forwarded connections through one connector;
/// exactly 100 succeeded (`echo_accepts=100`, `owner_sessions=100`) and the other 50
/// parked indefinitely inside `open_bi()` — no error, no timeout, and no
/// [`PipeOutcome`](crate::pipe::PipeOutcome) of any variant. FD peak in that run was
/// 518 for 100 live sessions, so FD exhaustion cannot bind before this limit does.
///
/// The value is deliberately set **above** the connector's forwarding budget
/// ([`PIPE_MAX_CONCURRENT_FORWARDS`](crate::pipe::PIPE_MAX_CONCURRENT_FORWARDS) = 100,
/// which stays at the measured number). The 28-stream headroom is not slack, it is
/// required for correctness of the guard: quinn issues stream credit in batches, so
/// with the two numbers equal a peer sitting at the limit gets **no** new credit when
/// a single stream retires, and the next `open_bi()` blocks even though the connector's
/// own accounting says a slot is free. Measured on the P12 loopback fixture: with the
/// limit equal to the budget, readmission after retiring one forward never happened
/// inside 90 s (every attempt hit the connector's open deadline); with this headroom it
/// happens in ~1 ms. Headroom keeps the connector's own budget — the one that refuses
/// visibly — the binding constraint, which is the whole point.
///
/// This is also the bound on `PipeProtocolHandler::accept`'s per-stream task spawn:
/// `accept_bi()` cannot yield more than this many live streams per connection, so the
/// owner-side spawn is capped by the same setting rather than being unbounded.
pub const MAX_CONCURRENT_BIDI_STREAMS: u32 = 128;

/// The explicit [`QuicTransportConfig`] every endpoint binds with — see
/// [`MAX_CONCURRENT_BIDI_STREAMS`]. Every other QUIC parameter keeps its default.
fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_CONCURRENT_BIDI_STREAMS))
        .build()
}

/// Which iroh stack the endpoint binds (spec §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetMode {
    /// Loopback / CI: `presets::Minimal` + `RelayMode::Disabled`. Fully offline
    /// over `127.0.0.1`; deterministic enough for CI (spec D9 / A4).
    Loopback,
    /// Real network (Gate A): the default n0 preset with DNS discovery + relay, so
    /// dial-by-`EndpointId` works across NATs (spec §7.3).
    RealNetwork,
}

/// Whether this crate was compiled with the diagnostic relay-only transport
/// seam. The feature is off by default and is intended only for controlled
/// verification binaries, never ordinary application builds.
pub const RELAY_ONLY_TEST_BUILD: bool = cfg!(feature = "relay-only-test");

/// Construction parameters for a [`NetTransport`].
#[derive(Debug, Clone, Copy)]
pub struct NetConfig {
    /// Loopback vs real-network endpoint stack.
    pub mode: NetMode,
    /// Optional fixed socket for [`NetMode::Loopback`]. `None` preserves the
    /// existing OS-assigned loopback port behavior; the spike-N40 gossip harness
    /// uses a fixed value so every node's address hint is known before spawning a
    /// managed room session.
    pub loopback_bind_addr: Option<SocketAddr>,
    /// Broadcast backlog for the [`ConnEvent`] stream before a slow observer lags.
    pub conn_event_capacity: usize,
    /// Ring capacity of the `Node::room_events` broadcast (issue #83). Lossy on
    /// lag exactly like `conn_event_capacity`; a slow subscriber gets `Lagged`.
    pub room_event_capacity: usize,
    /// Per-peer queued-byte cap on the **inbound** event-plane path (issue #141
    /// / `#134 §12.3` — 8 MiB default). Charges encoded `SyncMessage` body
    /// bytes only; the 4-byte length prefix and allocator overhead are not
    /// counted. When exhausted the offending frame is dropped, the link is
    /// closed, and `transport.queue.saturated` is audited (spec D1 / D6 / §7).
    pub inbound_peer_queue_bytes: usize,
    /// Per-peer queued-byte cap on the **outbound** event-plane path (issue
    /// #141 — 8 MiB default). Same units and recovery shape as
    /// [`Self::inbound_peer_queue_bytes`].
    pub outbound_peer_queue_bytes: usize,
    /// Per-subscribed-stream queued-byte cap (issue #141 / `#134 §12.3` — 2 MiB
    /// default). V1 carries exactly one logical event ALPN stream per peer, so
    /// this is the per-peer content/reconciliation bucket (spec D4):
    /// `Events`/`WantRecentChat`/`WantEvents`/`NotFound` frames charge it
    /// **and** the per-peer cap; governance / checkpoint / session control
    /// (`AdminTip` / `WantMembership` / `Heads` / `ProveCapability`) charge the
    /// per-peer cap only, so they can skip ahead of a saturated content
    /// backlog (spec D5).
    pub stream_queue_bytes: usize,
    /// Bounded capacity of the pipe-plane read-query control channel
    /// ([`PipeQuery`](crate::pipe::PipeQuery)) that the engine pump drains
    /// (issue #141 — bounded so no network-derived pipe-ALPN request can drive
    /// an unbounded control queue). Default is `MAX_CONCURRENT_BIDI_STREAMS`,
    /// matching the per-connection bidi-stream ceiling the endpoint itself
    /// enforces; pipe queries are bounded by accept-task spawn anyway, so this
    /// is defense-in-depth, not the binding constraint.
    pub pipe_query_capacity: usize,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: NetMode::Loopback,
            loopback_bind_addr: None,
            conn_event_capacity: 256,
            room_event_capacity: 256,
            inbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
            outbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
            stream_queue_bytes: DEFAULT_PER_STREAM_QUEUE_BYTES,
            pipe_query_capacity: MAX_CONCURRENT_BIDI_STREAMS as usize,
        }
    }
}

/// A raw inbound frame surfaced from a peer's reader task to the engine driver.
/// The bytes are **un-decoded** — the engine validates them (spec §4.3 / §6).
#[derive(Debug, Clone)]
pub struct Inbound {
    /// The authenticated source device (== engine `device_id`).
    pub peer: PeerId,
    /// Verbatim frame body (a canonical-CBOR `SyncMessage`).
    pub bytes: Vec<u8>,
}

/// The byte-bounded, priority-aware outbound frame queue for one peer (issue
/// #141). Cloned (cheaply — one `Arc`) into `Shared::route` for each engine
/// `Outgoing`; the per-peer writer task owns the consumer.
///
/// `depth()` reports **queued body bytes**, not frame count — the unit changed
/// from frames to bytes in issue #141 (spec D1, acceptance: "`OutboundQueue`
/// enforces bytes-in-queue, not frame-count; `depth()` reports bytes").
/// Recovery on true budget exhaustion is unchanged: drop the frame, audit
/// `transport.queue.saturated` with queue `outbound`, and close the link (§7).
#[derive(Clone)]
pub struct OutboundQueue {
    queue: BytePriorityQueue,
}

/// The consumer side of an [`OutboundQueue`], owned by the per-peer writer
/// task. Each `recv` yields the next encoded body in priority order
/// (`governance > checkpoint > content > blob-hints`, spec D3).
pub(crate) struct OutboundReceiver {
    rx: BytePriorityReceiver,
}

impl OutboundReceiver {
    /// Await the next encoded body in priority order. Returns `None` once the
    /// queue is closed and drained (peer unregistered / link torn down).
    pub(crate) async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await.map(|f| f.body)
    }

    /// Non-blocking pop of the next encoded body, or `None` if empty. Used by
    /// synchronous tests; the writer task uses [`Self::recv`].
    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.rx.try_recv().map(|f| f.body)
    }
}

impl OutboundQueue {
    /// Construct a producer/consumer pair with the given byte caps. The peer
    /// is supplied per-push (a per-peer queue only ever passes one peer id, so
    /// its internal budget map has at most one entry — spec D2 / D4).
    pub(crate) fn new(peer_cap: usize, stream_cap: usize) -> (Self, OutboundReceiver) {
        let (queue, rx) = BytePriorityQueue::channel(peer_cap, stream_cap);
        (Self { queue }, OutboundReceiver { rx })
    }

    /// Try to enqueue `body` for `peer`, classified by `family`. Non-blocking.
    /// Charges `body.len()` against the per-peer cap (and the per-stream cap
    /// when `family.charges_stream_budget()`).
    pub(crate) fn try_push(
        &self,
        peer: PeerId,
        body: Vec<u8>,
        family: queue::QueueFamily,
    ) -> Result<(), PushError> {
        self.queue.try_push(peer, body, family)
    }

    /// Current queued body bytes for this peer. **Bytes, not frames** (spec
    /// D1 / acceptance). Surfaced through `Node::outbound_queue_depths` and the
    /// verbose CLI `outbound_depth=<N>` diagnostic.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.queue.depth_bytes()
    }

    /// Close the queue: future pushes return [`PushError::Closed`], and the
    /// writer task drains remaining frames then observes `None` from `recv`.
    /// Called from `Shared::unregister` so a torn-down link's writer task does
    /// not outlive the queue (spec §9 reliability).
    pub(crate) fn close(&self) {
        self.queue.close();
    }
}

/// Shared transport state: the identity/authorizer/audit + the per-peer routing
/// tables + the byte-bounded inbound sink. Cloned (as `Arc<Shared>`) into the
/// accept handler and every connection task so they observe one consistent view
/// (issue #141: inbound + outbound event-plane paths now use byte-bounded
/// priority queues, not frame-count `mpsc`).
pub struct Shared {
    /// This node's authenticated identity (`endpoint.id() == device_id`).
    pub(crate) me: EndpointId,
    /// The connect-accept authorizer (§5).
    pub(crate) admission: Arc<dyn Admission>,
    /// The local audit sink (PRD §13.2 / §16.3).
    pub(crate) audit: Arc<dyn AuditSink>,
    /// The observable per-peer connection-state table (§4.5).
    pub(crate) table: PeerTable,
    /// Per-peer outbound frame queues (one writer task each).
    outbound: Mutex<HashMap<EndpointId, OutboundQueue>>,
    /// Configured per-peer outbound queue byte cap (issue #141).
    outbound_peer_queue_bytes: usize,
    /// Configured per-stream outbound queue byte cap (issue #141 / spec D4).
    stream_queue_bytes: usize,
    /// Live connection handles (so a local disconnect can close a specific link).
    connections: Mutex<HashMap<EndpointId, Connection>>,
    /// Devices admitted **provisionally** for the join bootstrap (IR-0104,
    /// Approach A): a not-yet-Active invitee allowed to pull the membership sub-DAG
    /// and push its `member.joined`, but served nothing else. The engine driver
    /// reads this to restrict service (membership-only) and clears it on
    /// upgrade-on-learn; the accept handler clears it on disconnect.
    provisional: Mutex<HashSet<EndpointId>>,
    /// Provisional devices that have **proven invite possession** (issue #112): a
    /// join-bootstrap dialer whose [`ProveCapability`](iroh_rooms_core::sync::SyncMessage::ProveCapability)
    /// matched an on-log invite. Only these are served the never-windowed membership
    /// **closure** (which since #111 can carry chat ancestry) or entered into the
    /// engine's fan-out set (issue #121 — an unproven peer's `on_connect` is
    /// deferred, so it receives no live push and no tip/head advertisements).
    /// Cleared with the provisional mark (on upgrade-on-learn or disconnect).
    capability_proven: Mutex<HashSet<EndpointId>>,
    /// Monotonic per-device connection generation (issue #126). Each accepted or
    /// dialed link is stamped with a fresh generation when it registers, and it
    /// performs its own teardown **only if it is still the current generation**
    /// for the device ([`teardown_if_current`](Self::teardown_if_current)). This
    /// closes a double-connect TOCTOU: without it, a superseded link's late close
    /// would clear a successor link's provisional/proven marks, drop its writer,
    /// and flip it `Offline` — the join-bootstrap gate bypass tracked in #126.
    /// Never reset or pruned per device (an ABA reuse of a stamp would let a
    /// long-superseded link's teardown clobber a same-numbered successor); one
    /// `u64` per distinct device seen, bounded like the other per-device tables.
    generations: Mutex<HashMap<EndpointId, u64>>,
    /// The byte-bounded, multi-peer inbound sink feeding the engine driver
    /// (issue #141). Each reader task charges encoded body bytes against both
    /// the per-peer and per-stream caps; classification is by `SyncMessage`
    /// variant only — never by decoded payload (spec D1 / §9).
    pub(crate) inbound: BytePriorityQueue,
    /// The gossip overlay state (issue #171 / spec §4 D1). Present only when
    /// the `gossip_overlay` feature is on. `inert()` (no actor, no meshes) for
    /// test fixtures; `with_actor(...)` for the production bind path. The mesh
    /// map is keyed by `RoomId` so `route`'s `Events` branch can find a room's
    /// broadcast mesh in O(1).
    #[cfg(feature = "gossip_overlay")]
    pub(crate) gossip_state: crate::gossip::GossipState,
    /// Weak back-reference used only to start bounded on-demand event links for
    /// pull/query traffic. Weak avoids `Shared <-> PeerManager` ownership cycles.
    #[cfg(feature = "gossip_overlay")]
    peer_manager: Mutex<Option<Weak<crate::manager::PeerManager>>>,
}

/// The consumer side of the inbound byte-priority queue, owned by the engine
/// driver pump ([`crate::node::Node`]). Yields one [`Inbound`] per popped frame
/// in priority order. Built by [`NetTransport::bind`]; taken once via
/// [`NetTransport::take_inbound`].
pub struct InboundReceiver {
    rx: BytePriorityReceiver,
}

impl InboundReceiver {
    /// Await the next inbound frame in priority order (governance, then
    /// checkpoint, then content, then blob-hints — spec D3). Returns `None`
    /// once the queue is closed and drained (transport shutdown).
    pub async fn recv(&mut self) -> Option<Inbound> {
        self.rx.recv().await.map(|f| Inbound {
            peer: f.peer,
            bytes: f.body,
        })
    }
}

/// The terminal peer-table state a guarded teardown writes (issue #126).
#[derive(Clone, Copy)]
pub(crate) enum LinkTeardown {
    /// An observable offline with a reason (redialable, or terminal by reason).
    Offline(OfflineReason),
    /// The remote refused us mid-stream (stable reject close): mark `Unauthorized`.
    Unauthorized,
}

impl Shared {
    /// Install the managed-room dial owner after both `Shared` and
    /// `PeerManager` have been placed in `Arc`s.
    #[cfg(feature = "gossip_overlay")]
    pub(crate) fn set_peer_manager(&self, manager: Weak<crate::manager::PeerManager>) {
        *self
            .peer_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(manager);
    }

    /// Register a peer's outbound frame queue (replaces any prior queue for the
    /// device — last writer wins on a double-connect, spec OQ-4). Closing the
    /// superseded queue wakes its writer task so it does not outlive the link
    /// it belonged to (issue #141: dropping the producer handle alone no longer
    /// ends the writer, since the consumer shares the queue's `Arc` state).
    pub(crate) fn register_outbound(&self, device: EndpointId, queue: OutboundQueue) {
        let prev = self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device, queue);
        if let Some(prev) = prev {
            prev.close();
        }
    }

    /// Record a live connection handle for the device.
    pub(crate) fn register_connection(&self, device: EndpointId, conn: Connection) {
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device, conn);
    }

    /// Drop a peer's outbound queue + connection handle (on disconnect).
    /// Closing the queue wakes its writer task so it drains remaining frames
    /// and exits cleanly (issue #141: the writer no longer observes an
    /// `mpsc::Sender` drop — it observes the queue's closed+drained state).
    pub(crate) fn unregister(&self, device: EndpointId) {
        let prev = self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
        if let Some(queue) = prev {
            queue.close();
        }
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
    }

    /// Locally close the connection to `device` (used by `disconnect_peer` and the
    /// reconnect test). The owning dial/accept task then observes `closed()`.
    pub(crate) fn close_connection(&self, device: EndpointId) {
        if let Some(conn) = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&device)
        {
            conn.close(LOCAL_CLOSE_CODE, b"local-disconnect");
        }
    }

    /// Route one engine [`Outgoing`] to its peer's byte-bounded writer queue.
    /// Non-blocking; **dropped** if the peer has no live writer (offline /
    /// unauthorized). The frame is classified by `SyncMessage` variant (spec
    /// D3), charged `body.len()` against the per-peer cap (and per-stream cap
    /// when applicable, spec D4), and — on true budget exhaustion — dropped,
    /// audited `transport.queue.saturated` with queue `outbound`, and the link
    /// is closed so reconnect/backfill becomes the recovery path (spec §6 /
    /// §7). A closed queue (peer torn down) is a silent drop, matching the
    /// prior `mpsc::Sender::Closed` path (issue #141).
    pub(crate) fn route(&self, out: &Outgoing) {
        // Gossip overlay fan-out (issue #171 / spec §4 D1 + open question 4):
        // when the feature is on AND the message is `SyncMessage::Events` AND a
        // gossip mesh is installed for the room, broadcast the encoded body on
        // the room's gossip topic. This is **additive** with the per-peer
        // queue path below — Events is also delivered to the destination
        // peer's queue when a live writer exists. The engine's `event_id`
        // G-set dedup makes a frame delivered by both paths idempotent, so
        // dual-path is correct (spec open question 4 explicitly leaves the
        // gossip-only vs dual-path choice to Phase A; dual-path is chosen here
        // for minimum regression risk: the loopback endpoint lacks the
        // address-discovery service `iroh-gossip`'s dialer needs, so a
        // gossip-only path would silently drop Events in loopback tests).
        //
        // At production scale (N>5, real-network mode with DNS discovery) the
        // gossip path dominates Events delivery; the per-peer queue path is
        // the fallback / coexisting channel. The K-bounded seed selector
        // (PeerManager::desired_seeds) bounds the warm-link count regardless.
        // A future Phase B revisit can switch to gossip-only once the
        // loopback discovery gap is closed.
        //
        // Pull/query variants stay on the queue path only — they rely on
        // per-link FIFO that gossip's epidemic delivery does not provide
        // (spec §4 D1 consequence).
        #[cfg(feature = "gossip_overlay")]
        {
            if let iroh_rooms_core::sync::SyncMessage::Events { room_id, .. } = &out.msg {
                if let Some(mesh) = self.gossip_state.mesh_for(room_id) {
                    mesh.broadcast_events(self.audit.clone(), self.me, out.msg.encode());
                }
                // Fall through to the per-peer queue path: dual-path delivery.
                // If no mesh is installed (loopback / early startup), the
                // queue path carries Events alone — exactly the pre-overlay
                // behavior, so existing tests pass under both feature
                // configurations.
            }
        }

        let Ok(device) = EndpointId::from_bytes(out.peer.as_bytes()) else {
            tracing::warn!("route: outgoing peer id is not a valid endpoint id; dropping");
            return;
        };
        // Classify before encoding: the family decides priority + which cap
        // bucket the body charges. Reads only the variant tag (spec D3 / §9).
        let family = classify_sync_message(&out.msg);
        let body = out.msg.encode();
        let queue = self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&device)
            .cloned();
        if let Some(queue) = queue {
            match queue.try_push(out.peer, body, family) {
                // `PushError::Closed` ⇒ peer's writer is gone; the prior
                // `mpsc::Closed` path was likewise a silent drop, so collapse
                // the two no-op arms (clippy::match_same_arms).
                Ok(()) | Err(PushError::Closed | PushError::Empty) => {}
                Err(PushError::Saturated) => {
                    self.audit.transport_queue_saturated(device, "outbound");
                    self.close_connection(device);
                }
            }
        } else {
            #[cfg(feature = "gossip_overlay")]
            if let Some(manager) = self
                .peer_manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(Weak::upgrade)
            {
                manager.route_on_demand(out.clone());
            }
        }
        // No live writer ⇒ peer offline ⇒ drop (engine re-pulls on reconnect).
    }

    /// Enqueue a raw inbound frame from `peer` onto the byte-bounded,
    /// priority-aware inbound sink (issue #141). The reader task supplies
    /// verbatim bytes; classification is by `SyncMessage` variant only, with
    /// an undecodable body mapping to `BlobHints` (lowest priority, still
    /// bounded) so the engine pump can decode again and log the drop as before
    /// (spec §5 step 2 / §6.1.6).
    ///
    /// `Empty` ⇒ the reader drops the malformed frame and continues;
    /// `Saturated` ⇒ the reader audits `transport.queue.saturated` with queue
    /// `inbound` and closes the link; `Closed` ⇒ the pump is gone and the
    /// reader exits silently. Same shape as the prior `mpsc::try_send` arms.
    pub(crate) fn try_enqueue_inbound(
        &self,
        peer: PeerId,
        bytes: Vec<u8>,
    ) -> Result<(), PushError> {
        let family = classify_inbound_bytes(&bytes);
        self.inbound.try_push(peer, bytes, family)
    }

    /// Close the inbound sink (transport shutdown): reader tasks observe
    /// `PushError::Closed` on the next push and exit; the pump drains and then
    /// observes `None` from [`InboundReceiver::recv`].
    pub(crate) fn close_inbound(&self) {
        self.inbound.close();
    }

    /// Register a new live link for `device` and return its connection generation
    /// (issue #126). Atomically, under the generations lock: assign the next
    /// generation, set the provisional mark when `provisional` (IR-0104, Approach A
    /// — the join-bootstrap seam needs it visible before the very first inbound
    /// frame), and install the outbound queue (last-writer-wins on a double-connect,
    /// spec OQ-4). Folding the generation bump and the provisional mark into one
    /// critical section is what makes the teardown guard sound: a superseded link's
    /// [`teardown_if_current`](Self::teardown_if_current) also takes the generations
    /// lock, so it can never interleave *between* a successor's mark and its
    /// generation bump and wrongly clear that mark. The connection handle is
    /// registered separately by the caller — it feeds only `close_connection`, not
    /// the gate decision, and its guarded removal on teardown keeps it consistent.
    pub(crate) fn register_link(
        &self,
        device: EndpointId,
        queue: OutboundQueue,
        provisional: bool,
    ) -> u64 {
        let mut generations = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = generations
            .get(&device)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        generations.insert(device, generation);
        if provisional {
            self.provisional
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(device);
        }
        self.register_outbound(device, queue);
        generation
    }

    /// Bump `device`'s connection generation without registering a new link, so any
    /// currently-live link's own [`teardown_if_current`](Self::teardown_if_current)
    /// becomes a no-op (issue #126 follow-up). Used by the manager's deauthorize
    /// path: it performs its own forced, unconditional teardown (`unregister` +
    /// `Offline{Deauthorized}`), and invalidating first means the accept task's
    /// later close — woken by `close_connection` — cannot overwrite that terminal
    /// `Deauthorized` state with a generic `LinkDropped`. Like every generation
    /// move this only ever *increases* the counter (never resets), so a future
    /// re-connect cannot collide with a stale link's pending teardown.
    pub(crate) fn invalidate_link(&self, device: EndpointId) {
        let mut generations = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bumped = generations
            .get(&device)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        generations.insert(device, bumped);
    }

    /// Tear down `device`'s link state — but **only if `generation` is still the
    /// current generation** for the device (issue #126). A superseded link's late
    /// close is then a total no-op: it never clears a successor link's
    /// provisional/proven marks, never drops its writer or connection handle, and
    /// never flips it away from `Connected`. Returns whether the teardown ran, so
    /// the caller gates its disconnect audit on actually having owned the link.
    ///
    /// The whole check-and-clear runs under the generations lock, so a concurrent
    /// [`register_link`](Self::register_link) (which also holds it) fully
    /// serializes against this — no window exists in which a fresh mark is dropped.
    pub(crate) fn teardown_if_current(
        &self,
        device: EndpointId,
        generation: u64,
        exit: LinkTeardown,
    ) -> bool {
        let generations = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if generations.get(&device).copied() != Some(generation) {
            return false; // superseded by a newer link — leave its state intact
        }
        self.provisional
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
        self.capability_proven
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
        self.unregister(device);
        match exit {
            LinkTeardown::Offline(reason) => self.table.set_offline(device, reason, None),
            LinkTeardown::Unauthorized => {
                self.table.set(device, PeerConnState::Unauthorized, None);
            }
        }
        true
    }

    /// Try to record `device`'s state as [`Connecting`](PeerConnState::Connecting),
    /// but only if **no live link is currently registered for it** (issue #136).
    ///
    /// The dial loop calls this at the top of each iteration so a stale-address
    /// redial does not stomp a [`Connected`](PeerConnState::Connected) entry a
    /// newer inbound accept established while this loop was backing off. After a
    /// peer rebinds (new UDP port, same device key) the remote's stale-address
    /// dial loop keeps dialing the dead address; without this guard every
    /// iteration's `Connecting` set would briefly flip a demonstrably-live flow's
    /// state away from `Connected`. Returns whether the state was recorded, so the
    /// caller can reason about ownership (though it currently makes no decisions
    /// on the return value — the next dial attempt is correct either way).
    ///
    /// "Live link" is read from the per-device outbound-queue map, which is
    /// populated by [`register_link`](Self::register_link) (under the generations
    /// lock) for both inbound accepts and outbound dials, and cleared by
    /// [`unregister`](Self::unregister) on teardown — so an entry there means a
    /// link has registered its writer and can carry data. The check-and-set runs
    /// under the generations lock (the same critical section
    /// [`register_link`](Self::register_link) takes), so a concurrent link
    /// registration fully serializes against this: it cannot slip its queue
    /// installation between our observation and our table write.
    pub(crate) fn set_connecting_if_no_link(&self, device: EndpointId) -> bool {
        let _guard = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&device)
        {
            return false;
        }
        self.table.set(device, PeerConnState::Connecting, None);
        true
    }

    /// Try to move `device` to [`Offline`](PeerConnState::Offline) carrying
    /// `reason`, but only if **no live link is currently registered for it**
    /// (issue #136). Returns whether the offline was recorded, so the caller can
    /// gate its disconnect audit on actually owning the state transition.
    ///
    /// The dial loop's failed-connect path uses this so a stale-address dial does
    /// not overwrite a live link's `Connected` state: after a peer rebinds, the
    /// remote's stale-address dial loop redials the dead address and fails on
    /// every backoff tick; without this guard each failure would stomp the table
    /// entry of a device whose newer inbound link is alive and carrying data —
    /// the post-rebind lifecycle defect tracked in #136. The provisional-backoff
    /// and stream-open-failure arms use it for the same reason.
    ///
    /// Like [`set_connecting_if_no_link`](Self::set_connecting_if_no_link), the
    /// check-and-set runs under the generations lock so a concurrent
    /// [`register_link`](Self::register_link) serializes against it.
    pub(crate) fn set_offline_if_no_link(&self, device: EndpointId, reason: OfflineReason) -> bool {
        let _guard = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&device)
        {
            return false;
        }
        self.table.set_offline(device, reason, None);
        true
    }

    /// Clear a device's provisional mark — on upgrade-on-learn (its join was
    /// accepted, so it is now a full member). Also drops any capability-proven
    /// mark, so a re-connect must re-prove (issue #112). Disconnect teardown goes
    /// through [`teardown_if_current`](Self::teardown_if_current) instead, so a
    /// superseded link cannot clear a live successor's mark (issue #126).
    pub(crate) fn clear_provisional(&self, device: EndpointId) {
        self.provisional
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
        self.capability_proven
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
    }

    /// Whether `device` is currently a provisional join-bootstrap peer (served the
    /// membership sub-DAG only).
    #[must_use]
    pub(crate) fn is_provisional(&self, device: EndpointId) -> bool {
        self.provisional
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&device)
    }

    /// Record that a provisional `device` has proven invite possession (issue
    /// #112): its [`ProveCapability`](iroh_rooms_core::sync::SyncMessage::ProveCapability)
    /// matched an on-log invite, so it may now pull the membership closure.
    pub(crate) fn mark_capability_proven(&self, device: EndpointId) {
        self.capability_proven
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device);
    }

    /// Whether a provisional `device` has proven invite possession (issue #112).
    #[must_use]
    pub(crate) fn is_capability_proven(&self, device: EndpointId) -> bool {
        self.capability_proven
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&device)
    }

    /// The `Connected` authenticated devices, as engine [`PeerId`]s.
    pub(crate) fn connected_peers(&self) -> Vec<PeerId> {
        self.table
            .connected_devices()
            .into_iter()
            .map(peer_id)
            .collect()
    }

    /// Configured per-peer outbound byte cap (issue #141). Used by
    /// [`peer::register_connection`] to construct each peer's
    /// [`OutboundQueue`].
    pub(crate) const fn outbound_peer_queue_bytes(&self) -> usize {
        self.outbound_peer_queue_bytes
    }

    /// Configured per-stream outbound byte cap (issue #141 / spec D4).
    pub(crate) const fn stream_queue_bytes(&self) -> usize {
        self.stream_queue_bytes
    }

    /// Point-in-time per-peer **queued body bytes** (issue #141: unit is now
    /// bytes, not frames — spec D1 / acceptance). Surfaced through
    /// [`NetTransport::outbound_queue_depths`] and the verbose CLI
    /// `outbound_depth=<N>` diagnostic.
    pub fn outbound_queue_depths(&self) -> Vec<(EndpointId, usize)> {
        self.outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(device, queue)| (*device, queue.depth()))
            .collect()
    }
}

/// The full-mesh direct-QUIC event-transport adapter.
pub struct NetTransport {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    router: Router,
    inbound_rx: Option<InboundReceiver>,
    dial_tasks: Mutex<Vec<JoinHandle<()>>>,
    mode: NetMode,
}

impl NetTransport {
    /// Bind an endpoint keyed by `secret` (the node's `device_id` key, so
    /// `endpoint.id() == device_id == EndpointId`, unifying transport / signing /
    /// ACL identity, Membership §1) and spawn the `Router` with the event ALPN
    /// gated by `admission`.
    ///
    /// `blobs_handler`, when supplied, chains the `iroh-blobs` ALPN as a third
    /// `.accept()` on the same router (IR-0204 spec §5.3) — the established
    /// one-endpoint-many-planes pattern already used for `pipe_handler`.
    ///
    /// When the `gossip_overlay` feature is enabled at compile time, a fourth
    /// `.accept()` chain is added for [`GOSSIP_ALPN`](crate::alpn::GOSSIP_ALPN),
    /// wrapped in [`GossipProtocolHandler`](crate::gossip::GossipProtocolHandler)
    /// so the same `Arc<dyn Admission>` instance gates both planes (issue #171
    /// / spec §4 D2). Reject-before-bytes is preserved: an unadmitted device's
    /// `GOSSIP_ALPN` connection is closed at `accept()` time before the inner
    /// gossip handler runs.
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    ///
    /// # Panics
    /// Panics if the gossip actor was not installed on `Shared` before the
    /// `GOSSIP_ALPN` wrapper is built. This is impossible by construction
    /// (the actor is installed on the line just above) — the `expect` is a
    /// construction-order invariant check, not a runtime hazard.
    pub async fn bind(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        cfg: NetConfig,
        pipe_handler: Option<PipeProtocolHandler>,
        blobs_handler: Option<iroh_blobs::BlobsProtocol>,
    ) -> Result<Self> {
        let endpoint = match cfg.mode {
            NetMode::Loopback => {
                let builder = Endpoint::builder(presets::Minimal)
                    .secret_key(secret)
                    .relay_mode(RelayMode::Disabled)
                    .transport_config(transport_config());
                let builder = if let Some(addr) = cfg.loopback_bind_addr {
                    builder
                        .bind_addr(addr)
                        .context("configure loopback bind address")?
                } else {
                    builder
                };
                builder.bind().await.context("bind loopback endpoint")?
            }
            NetMode::RealNetwork => {
                let builder = Endpoint::builder(presets::N0)
                    .secret_key(secret)
                    .transport_config(transport_config());
                // Compile-time diagnostic seam only. Both endpoints in a
                // controlled verification run use a separately built binary,
                // suppressing direct UDP transports so all room, blob, and
                // pipe traffic must traverse the configured relay. With the
                // default feature set this statement is not compiled and the
                // original direct-capable builder is unchanged.
                #[cfg(feature = "relay-only-test")]
                let builder = builder.clear_ip_transports();
                builder.bind().await.context("bind real-network endpoint")?
            }
        };

        let me = endpoint.id();
        let (inbound, inbound_rx) = BytePriorityQueue::channel(
            cfg.inbound_peer_queue_bytes.max(1),
            cfg.stream_queue_bytes.max(1),
        );

        // The gossip overlay actor (issue #171 / spec §4 D2 / Step 1). Spawned
        // on the same endpoint the event plane already bound — one Endpoint,
        // many planes (ADR-1). The actor is held by `Shared.gossip_state`; a
        // clone is taken below to build the admission-gated accept wrapper.
        #[cfg(feature = "gossip_overlay")]
        let gossip_actor = crate::gossip::spawn_gossip_actor(endpoint.clone());

        let shared = Arc::new(Shared {
            me,
            admission,
            audit,
            table: PeerTable::new(cfg.conn_event_capacity),
            outbound: Mutex::new(HashMap::new()),
            outbound_peer_queue_bytes: cfg.outbound_peer_queue_bytes.max(1),
            stream_queue_bytes: cfg.stream_queue_bytes.max(1),
            connections: Mutex::new(HashMap::new()),
            provisional: Mutex::new(HashSet::new()),
            capability_proven: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            inbound,
            #[cfg(feature = "gossip_overlay")]
            gossip_state: crate::gossip::GossipState::with_actor(gossip_actor),
            #[cfg(feature = "gossip_overlay")]
            peer_manager: Mutex::new(None),
        });

        // One Endpoint, one Router serving both planes (ADR-1): the event ALPN is
        // the first `.accept()` chain; the Live Pipe Plane (IR-0010) chains the pipe
        // ALPN as the second when a handler is supplied. The endpoint is cloned so we
        // retain it for dialing (events and pipes both).
        let mut builder = Router::builder(endpoint.clone())
            .accept(EVENT_ALPN, EventProtocolHandler::new(shared.clone()));
        if let Some(pipe_handler) = pipe_handler {
            builder = builder.accept(PIPE_ALPN, pipe_handler);
        }
        if let Some(blobs_handler) = blobs_handler {
            builder = builder.accept(iroh_blobs::ALPN, blobs_handler);
        }
        // The gossip overlay plane (issue #171 / spec §4 D2). The wrapper
        // closes a GOSSIP_ALPN connection at `accept()` time before the inner
        // gossip handler runs whenever admission fails — zero gossip bytes are
        // exchanged with an unadmitted device. The wrapper shares the same
        // `Arc<Shared>` (and so the same `Arc<dyn Admission>` instance) as the
        // event-plane handler, so admission decisions are byte-identical on
        // both ALPNs.
        #[cfg(feature = "gossip_overlay")]
        {
            let handler = crate::gossip::GossipProtocolHandler::new(
                shared.clone(),
                shared
                    .gossip_state
                    .actor()
                    .expect("gossip actor installed just above")
                    .clone(),
            );
            builder = builder.accept(crate::alpn::GOSSIP_ALPN, handler);
        }
        let router = builder.spawn();

        Ok(Self {
            shared,
            endpoint,
            router,
            inbound_rx: Some(InboundReceiver { rx: inbound_rx }),
            dial_tasks: Mutex::new(Vec::new()),
            mode: cfg.mode,
        })
    }

    /// This node's authenticated identity (`device_id == EndpointId`).
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.shared.me
    }

    /// A clone of the underlying iroh [`Endpoint`], so the Live Pipe Plane connector
    /// can dial the owner over [`PIPE_ALPN`](crate::pipe::alpn::PIPE_ALPN) on the
    /// **same** endpoint that serves events (one `Endpoint`, two planes).
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// A dialable address for this node, exchanged out-of-band in lieu of an invite
    /// ticket (spec N3). Loopback mode returns `id + 127.0.0.1:<port>`; real-network
    /// mode returns the endpoint's discovered address (relay + direct hints).
    ///
    /// # Errors
    /// Returns an error if a loopback endpoint has no bound UDP socket.
    pub fn endpoint_addr(&self) -> Result<EndpointAddr> {
        match self.mode {
            NetMode::Loopback => loopback_addr(&self.endpoint),
            NetMode::RealNetwork => Ok(self.endpoint.addr()),
        }
    }

    /// This node's home relay url, if any (spec IR-0303 §5.3) — the first
    /// [`TransportAddr::Relay`] in the local dialable address. Diagnostic only
    /// (§18.1 "clear connection state"); `None` in [`NetMode::Loopback`] (relay is
    /// disabled) or before the endpoint has homed a relay.
    #[must_use]
    pub fn relay_url(&self) -> Option<String> {
        let addr = self.endpoint_addr().ok()?;
        addr.addrs.iter().find_map(|a| match a {
            TransportAddr::Relay(url) => Some(url.to_string()),
            _ => None,
        })
    }

    /// Start (or restart) a dial-with-backoff loop toward `addr`. Returns
    /// immediately; the loop keeps the link alive until the transport is dropped.
    pub fn connect_to(&self, addr: EndpointAddr) {
        let handle = tokio::spawn(dial_loop(self.shared.clone(), self.endpoint.clone(), addr));
        self.dial_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(handle);
    }

    /// Locally close the link to `device` (the reconnect-test / roster-change
    /// hook). A peer with an active dial loop will redial it.
    pub fn disconnect_peer(&self, device: EndpointId) {
        self.shared.close_connection(device);
    }

    /// Point-in-time snapshot of every known device and its [`PeerConnState`].
    #[must_use]
    pub fn peer_states(&self) -> Vec<(EndpointId, PeerConnState)> {
        self.shared.table.snapshot()
    }

    /// Point-in-time snapshot of every known device and its full [`PeerEntry`]
    /// (state + offline reason + bound identity) — the §16.3 CLI connection view.
    #[must_use]
    pub fn peer_entries(&self) -> Vec<(EndpointId, PeerEntry)> {
        self.shared.table.entries()
    }

    #[must_use]
    pub fn outbound_queue_depths(&self) -> Vec<(EndpointId, usize)> {
        self.shared.outbound_queue_depths()
    }

    /// The number of unmanaged dial loops currently running.
    #[must_use]
    pub fn dial_count(&self) -> usize {
        self.dial_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// The total gossip-neighbor count across every per-room mesh this node has
    /// subscribed (issue #171 / spec §5.4). Zero when the `gossip_overlay`
    /// feature is off, no room session is active, or the swarm has not yet
    /// formed a direct neighbor link. Surfaced through `Node::gossip_neighbor_count`
    /// so the CLI / spike harness can read it.
    #[must_use]
    pub fn gossip_neighbor_count(&self) -> usize {
        #[cfg(feature = "gossip_overlay")]
        {
            self.shared.gossip_state.neighbor_count()
        }
        #[cfg(not(feature = "gossip_overlay"))]
        {
            0
        }
    }

    /// The current state of one device, if known.
    #[must_use]
    pub fn peer_state(&self, device: EndpointId) -> Option<PeerConnState> {
        self.shared.table.state_of(device)
    }

    /// Per-peer live path classification (direct/relay/mixed/none) + relay url, read
    /// from iroh's `remote_info` for each known peer (spec IR-0303 §5.3). Diagnostic
    /// only, off any hot path — meant for `--verbose` callers. An `offline`/
    /// `unauthorized` peer has no active transport and honestly classifies as
    /// [`crate::diag::PathType::None`] rather than blocking or guessing.
    pub async fn peer_paths(&self) -> Vec<(EndpointId, crate::diag::PathType, Option<String>)> {
        let mut out = Vec::new();
        for (device, _) in self.peer_entries() {
            let info = self.endpoint.remote_info(device).await;
            let (path_type, relay_url) = crate::diag::classify_remote_info(info.as_ref());
            out.push((device, path_type, relay_url));
        }
        out
    }

    /// Subscribe to the live [`ConnEvent`] transition stream (§4.5).
    #[must_use]
    pub fn conn_events(&self) -> broadcast::Receiver<ConnEvent> {
        self.shared.table.subscribe()
    }

    /// Take the inbound frame receiver (once). The engine driver owns it and
    /// feeds each frame to `SyncEngine::on_message` (issue #141: this is now a
    /// byte-bounded priority receiver, not an `mpsc::Receiver`).
    pub fn take_inbound(&mut self) -> Option<InboundReceiver> {
        self.inbound_rx.take()
    }

    /// A clone of the shared state, for an external driver that routes engine
    /// outputs without the [`SyncTransport`] `&mut self` borrow.
    #[must_use]
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Gracefully stop: abort dial loops, close the inbound sink so reader
    /// tasks observe `PushError::Closed` and exit, and shut the router down
    /// (issue #141: the inbound byte-priority queue is closed explicitly so
    /// any in-flight reader does not pile frames against an unread queue).
    ///
    /// # Errors
    /// Returns an error if the router shutdown task fails to join.
    pub async fn shutdown(self) -> Result<()> {
        for handle in self
            .dial_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            handle.abort();
        }
        self.shared.close_inbound();
        self.router.shutdown().await.context("router shutdown")?;
        Ok(())
    }
}

impl Drop for NetTransport {
    fn drop(&mut self) {
        // Best-effort: abort the dial loops so they do not outlive the transport
        // (graceful shutdown beyond Drop is a non-goal, spec N6).
        if let Ok(mut tasks) = self.dial_tasks.lock() {
            for handle in tasks.drain(..) {
                handle.abort();
            }
        }
    }
}

impl SyncTransport for NetTransport {
    fn peers(&self) -> Vec<PeerId> {
        self.shared.connected_peers()
    }

    fn send(&mut self, out: Outgoing) {
        self.shared.route(&out);
    }
}

/// Build a loopback [`EndpointAddr`] (`id + 127.0.0.1:<bound port>`) for a local
/// endpoint, bypassing relay/DNS discovery (mirrors `spike-blobs`).
fn loopback_addr(endpoint: &Endpoint) -> Result<EndpointAddr> {
    let port = endpoint
        .bound_sockets()
        .into_iter()
        .map(|s| s.port())
        .next()
        .context("endpoint has no bound UDP socket")?;
    let socket = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    Ok(EndpointAddr::new(endpoint.id()).with_ip_addr(socket))
}

#[cfg(test)]
mod tests {
    use super::{
        InboundReceiver, LinkTeardown, NetConfig, NetMode, OutboundQueue, OutboundReceiver, Shared,
        RELAY_ONLY_TEST_BUILD,
    };
    use crate::admission::AllowlistAdmission;
    use crate::audit::TracingAudit;
    use crate::queue::{
        BytePriorityQueue, PushError, DEFAULT_PER_PEER_QUEUE_BYTES, DEFAULT_PER_STREAM_QUEUE_BYTES,
    };
    use crate::state::{OfflineReason, PeerConnState, PeerTable};
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::sync::{Outgoing, PeerId, SyncMessage};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// Construct a `Shared` with no connections or queues registered, backed by
    /// a real byte-priority inbound sink (#141). The returned receiver is the
    /// engine driver's inlet; tests that don't care simply drop it.
    fn make_shared() -> (Arc<Shared>, InboundReceiver) {
        let (inbound, inbound_rx) = BytePriorityQueue::channel(
            DEFAULT_PER_PEER_QUEUE_BYTES,
            DEFAULT_PER_STREAM_QUEUE_BYTES,
        );
        let shared = Arc::new(Shared {
            me: device(0x01),
            admission: Arc::new(AllowlistAdmission::new()),
            audit: Arc::new(TracingAudit),
            table: PeerTable::new(8),
            outbound: Mutex::new(HashMap::new()),
            outbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
            stream_queue_bytes: DEFAULT_PER_STREAM_QUEUE_BYTES,
            connections: Mutex::new(HashMap::new()),
            provisional: Mutex::new(HashSet::new()),
            capability_proven: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            inbound,
            #[cfg(feature = "gossip_overlay")]
            gossip_state: crate::gossip::GossipState::inert(),
            #[cfg(feature = "gossip_overlay")]
            peer_manager: Mutex::new(None),
        });
        (shared, InboundReceiver { rx: inbound_rx })
    }

    /// Build a per-peer outbound queue and its receiver with the default 8 MiB
    /// peer / 2 MiB stream byte caps (#141). Most tests use
    /// [`outbound_pair_with_caps`] to drive saturation with a deliberately
    /// tiny cap.
    fn outbound_pair() -> (OutboundQueue, OutboundReceiver) {
        outbound_pair_with_caps(DEFAULT_PER_PEER_QUEUE_BYTES, DEFAULT_PER_STREAM_QUEUE_BYTES)
    }

    fn outbound_pair_with_caps(
        peer_cap: usize,
        stream_cap: usize,
    ) -> (OutboundQueue, OutboundReceiver) {
        OutboundQueue::new(peer_cap, stream_cap)
    }

    fn dummy_outgoing(peer: EndpointId) -> Outgoing {
        Outgoing {
            peer: PeerId::from_bytes(*peer.as_bytes()),
            msg: SyncMessage::NotFound {
                room_id: RoomId::from_bytes([0xAA; 32]),
                ids: vec![],
            },
        }
    }

    fn admin_tip_outgoing(peer: EndpointId) -> Outgoing {
        Outgoing {
            peer: PeerId::from_bytes(*peer.as_bytes()),
            msg: SyncMessage::AdminTip {
                room_id: RoomId::from_bytes([0xAB; 32]),
                tip: None,
            },
        }
    }

    fn want_membership_outgoing(peer: EndpointId) -> Outgoing {
        Outgoing {
            peer: PeerId::from_bytes(*peer.as_bytes()),
            msg: SyncMessage::WantMembership {
                room_id: RoomId::from_bytes([0xAC; 32]),
                have: vec![],
            },
        }
    }

    /// A `WantRecentChat` frame — used in queue-budget tests as the
    /// Content-priority / stream-charging variant. `Events` is also
    /// Content-priority but, under the `gossip_overlay` feature, routes to the
    /// gossip mesh instead of the per-peer queue (issue #171 / spec §4 D1); a
    /// `WantRecentChat` pull is a pull/query variant that always rides the
    /// queue, so the queue-budget properties hold under both feature
    /// configurations.
    fn want_recent_chat_outgoing(peer: EndpointId) -> Outgoing {
        use iroh_rooms_core::sync::Window;
        Outgoing {
            peer: PeerId::from_bytes(*peer.as_bytes()),
            msg: SyncMessage::WantRecentChat {
                room_id: RoomId::from_bytes([0xAE; 32]),
                window: Window {
                    max_count: 10,
                    since_ms: None,
                },
                have: vec![],
            },
        }
    }

    fn events_outgoing(peer: EndpointId, payload_len: usize) -> Outgoing {
        Outgoing {
            peer: PeerId::from_bytes(*peer.as_bytes()),
            msg: SyncMessage::Events {
                room_id: RoomId::from_bytes([0xAD; 32]),
                frames: vec![vec![0xEE; payload_len]],
            },
        }
    }

    // --- NetConfig defaults ---

    #[test]
    fn net_config_default_mode_is_loopback() {
        assert_eq!(NetConfig::default().mode, NetMode::Loopback);
    }

    #[test]
    fn net_config_default_loopback_bind_addr_is_os_assigned() {
        assert_eq!(NetConfig::default().loopback_bind_addr, None);
    }

    #[test]
    fn net_config_default_conn_event_capacity_is_256() {
        assert_eq!(NetConfig::default().conn_event_capacity, 256);
    }

    #[test]
    fn net_config_default_room_event_capacity_is_256() {
        assert_eq!(NetConfig::default().room_event_capacity, 256);
    }

    /// Issue #141: `NetConfig` now exposes byte-named caps, not frame-count
    /// caps. Defaults pin the §12.3 budget — 8 MiB per peer, 2 MiB per stream —
    /// and the bounded pipe-query capacity defaults to the bidi-stream ceiling.
    #[test]
    fn net_config_defaults_are_byte_budgets_from_section_12_3() {
        let cfg = NetConfig::default();
        assert_eq!(cfg.inbound_peer_queue_bytes, 8 * 1024 * 1024);
        assert_eq!(cfg.outbound_peer_queue_bytes, 8 * 1024 * 1024);
        assert_eq!(cfg.stream_queue_bytes, 2 * 1024 * 1024);
        assert_eq!(
            cfg.pipe_query_capacity,
            super::MAX_CONCURRENT_BIDI_STREAMS as usize
        );
    }

    #[test]
    fn relay_only_diagnostic_constant_matches_the_compile_time_feature() {
        assert_eq!(RELAY_ONLY_TEST_BUILD, cfg!(feature = "relay-only-test"));
    }

    #[test]
    fn net_mode_loopback_and_real_network_are_distinct() {
        assert_ne!(NetMode::Loopback, NetMode::RealNetwork);
        assert_eq!(NetMode::Loopback, NetMode::Loopback);
        assert_eq!(NetMode::RealNetwork, NetMode::RealNetwork);
    }

    // --- Shared::route: silent-drop for peer with no registered queue ---

    #[test]
    fn route_silently_drops_frame_when_peer_has_no_queue() {
        let (shared, _rx) = make_shared();
        // device(0x02) was never registered — route must be a silent no-op.
        shared.route(&dummy_outgoing(device(0x02)));
        // No panic, nothing queued anywhere.
    }

    // --- Shared::route: delivers encoded body to registered outbound queue ---

    #[test]
    fn route_delivers_encoded_frame_to_registered_peer() {
        let (shared, _rx) = make_shared();
        let peer = device(0x03);
        let (queue, mut rx) = outbound_pair();
        shared.register_outbound(peer, queue);

        let out = dummy_outgoing(peer);
        let expected = out.msg.encode();
        shared.route(&out);

        let received = rx
            .try_recv()
            .expect("frame must be queued for the registered peer");
        assert_eq!(received, expected);
    }

    /// Issue #141: a saturated outbound byte budget drops the frame, audits
    /// `transport.queue.saturated`, and closes the link — same recovery shape
    /// as before, now keyed on bytes not frame count. `outbound_queue_depths`
    /// reports bytes (the one frame that landed = its `body.len()`).
    #[test]
    fn route_closes_peer_when_outbound_queue_is_full() {
        let (shared, _rx) = make_shared();
        let peer = device(0x05);
        // Cap both budgets at exactly one body's length: the first frame fills
        // the cap; the second must saturate, audit, and close the link.
        let one_body = dummy_outgoing(peer).msg.encode().len();
        let (queue, _rx) = outbound_pair_with_caps(one_body, one_body);
        shared.register_outbound(peer, queue);
        shared.table.set(peer, PeerConnState::Connected, None);

        shared.route(&dummy_outgoing(peer)); // admitted; depth == one_body bytes
        shared.route(&dummy_outgoing(peer)); // saturates; link closed

        assert_eq!(
            shared.outbound_queue_depths(),
            vec![(peer, one_body)],
            "depth reports the one admitted body's bytes, not a frame count"
        );
    }

    #[test]
    fn route_accepts_governance_when_content_stream_budget_is_full() {
        let (shared, _rx) = make_shared();
        let peer = device(0x06);
        // `WantRecentChat` (Subscription family) is Content-priority and
        // stream-charging — the same queue classification as `Events` — but it
        // is a pull variant that always rides the per-peer queue (issue #171
        // / spec §4 D1: pull/query variants stay on the queue path). Using it
        // instead of `Events` keeps this queue-budget test meaningful under
        // both feature configurations: under `gossip_overlay`, `Events`
        // detours through the gossip mesh and would never reach the queue.
        let content = want_recent_chat_outgoing(peer);
        let content_body_len = content.msg.encode().len();
        let admin = admin_tip_outgoing(peer);
        let admin_body = admin.msg.encode();
        let want_membership = want_membership_outgoing(peer);
        let want_membership_body = want_membership.msg.encode();
        let peer_cap = content_body_len + admin_body.len() + want_membership_body.len();
        let (queue, mut rx) = outbound_pair_with_caps(peer_cap, content_body_len);
        shared.register_outbound(peer, queue);
        shared.table.set(peer, PeerConnState::Connected, None);

        shared.route(&content);
        shared.route(&admin);
        shared.route(&want_membership);
        shared.route(&content);

        assert_eq!(
            shared.outbound_queue_depths(),
            vec![(peer, peer_cap)],
            "the saturated content stream must not consume governance headroom"
        );
        assert_eq!(rx.try_recv(), Some(admin_body));
        assert_eq!(rx.try_recv(), Some(want_membership_body));
        assert_eq!(rx.try_recv(), Some(content.msg.encode()));
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn try_enqueue_inbound_enforces_byte_caps_and_prioritizes_governance() {
        let peer = device(0x0D);
        let p = PeerId::from_bytes(*peer.as_bytes());
        let content = events_outgoing(peer, 64).msg.encode();
        let admin = admin_tip_outgoing(peer).msg.encode();
        let want_membership = want_membership_outgoing(peer).msg.encode();
        let peer_cap = content.len() + admin.len() + want_membership.len();
        let (inbound, inbound_rx) = BytePriorityQueue::channel(peer_cap, content.len());
        let shared = Arc::new(Shared {
            me: device(0x01),
            admission: Arc::new(AllowlistAdmission::new()),
            audit: Arc::new(TracingAudit),
            table: PeerTable::new(8),
            outbound: Mutex::new(HashMap::new()),
            outbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
            stream_queue_bytes: DEFAULT_PER_STREAM_QUEUE_BYTES,
            connections: Mutex::new(HashMap::new()),
            provisional: Mutex::new(HashSet::new()),
            capability_proven: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            inbound,
            #[cfg(feature = "gossip_overlay")]
            gossip_state: crate::gossip::GossipState::inert(),
            #[cfg(feature = "gossip_overlay")]
            peer_manager: Mutex::new(None),
        });
        let mut rx = InboundReceiver { rx: inbound_rx };

        shared
            .try_enqueue_inbound(p, content.clone())
            .expect("content fills the inbound stream cap");
        shared
            .try_enqueue_inbound(p, admin.clone())
            .expect("AdminTip does not charge the saturated stream cap");
        shared
            .try_enqueue_inbound(p, want_membership.clone())
            .expect("WantMembership does not charge the saturated stream cap");
        assert_eq!(
            shared.try_enqueue_inbound(p, content.clone()),
            Err(PushError::Saturated),
            "another content frame exceeds the inbound stream cap"
        );

        assert_eq!(rx.rx.try_recv().map(|f| f.body), Some(admin));
        assert_eq!(rx.rx.try_recv().map(|f| f.body), Some(want_membership));
        assert_eq!(rx.rx.try_recv().map(|f| f.body), Some(content));
        assert!(rx.rx.try_recv().is_none());
    }

    // --- Shared::unregister removes the outbound queue ---

    #[test]
    fn route_drops_frame_after_unregister() {
        let (shared, _rx) = make_shared();
        let peer = device(0x04);
        let (queue, mut rx) = outbound_pair();
        shared.register_outbound(peer, queue);
        shared.unregister(peer);

        shared.route(&dummy_outgoing(peer));
        assert!(
            rx.try_recv().is_none(),
            "no frame must arrive after the peer is unregistered"
        );
    }

    /// Issue #141: a double-connect replaces the outbound queue (spec OQ-4,
    /// last-writer-wins). The superseded queue must be **closed** so its
    /// writer task observes `None` from `recv` and exits — without this, the
    /// Arc-shared queue state stays open and the writer task leaks (blocked
    /// forever on an empty, never-closed queue). Dropping the producer handle
    /// alone does not close the queue (the consumer shares the state).
    #[tokio::test]
    async fn register_outbound_closes_the_superseded_queue_so_its_writer_exits() {
        let (shared, _rx) = make_shared();
        let peer = device(0x07);
        let (queue1, mut rx1) = outbound_pair();
        shared.register_outbound(peer, queue1);
        let (queue2, mut rx2) = outbound_pair();
        shared.register_outbound(peer, queue2); // supersedes queue1

        // The superseded consumer must terminate (return None) on the next
        // `recv`, not block forever.
        let superseded = tokio::time::timeout(std::time::Duration::from_millis(500), rx1.recv())
            .await
            .expect("superseded writer must observe None, not block");
        assert!(
            superseded.is_none(),
            "a superseded queue's writer must terminate, not hang"
        );

        // The successor queue is unaffected and still delivers routed frames.
        shared.route(&dummy_outgoing(peer));
        let delivered = rx2.recv().await.expect("successor queue delivers");
        assert!(!delivered.is_empty());
    }

    // --- Issue #126: connection-generation guard on teardown -----------------

    #[test]
    fn register_link_generations_increase_per_device_and_start_fresh_per_device() {
        let (shared, _rx) = make_shared();
        let a = device(0x20);
        let b = device(0x21);
        // Each successive link on the same device gets a strictly greater stamp,
        // so a superseded link can always be distinguished from its successor.
        assert_eq!(shared.register_link(a, outbound_pair().0, false), 1);
        assert_eq!(shared.register_link(a, outbound_pair().0, false), 2);
        assert_eq!(shared.register_link(a, outbound_pair().0, false), 3);
        // A different device starts its own sequence at 1.
        assert_eq!(shared.register_link(b, outbound_pair().0, false), 1);
    }

    #[test]
    fn teardown_if_current_tears_down_only_at_the_current_generation() {
        let (shared, _rx) = make_shared();
        let peer = device(0x22);
        let gen = shared.register_link(peer, outbound_pair().0, true);
        shared.mark_capability_proven(peer);
        shared.table.set(peer, PeerConnState::Connected, None);

        // A stale generation (there is none newer here, but simulate a mismatch)
        // is a no-op: nothing is cleared.
        assert!(!shared.teardown_if_current(
            peer,
            gen + 1,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));
        assert!(shared.is_provisional(peer));
        assert!(shared.is_capability_proven(peer));
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Connected));

        // The current generation tears everything down.
        assert!(shared.teardown_if_current(
            peer,
            gen,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));
        assert!(!shared.is_provisional(peer));
        assert!(!shared.is_capability_proven(peer));
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Offline));
    }

    #[test]
    fn superseded_provisional_link_teardown_preserves_the_successors_gate() {
        // The #126 leak, reduced to its invariant: an unproven provisional dialer
        // double-connects (conn1 → gen1, conn2 → gen2), then conn1 closes. conn1's
        // teardown must NOT clear the provisional mark (nor drop conn2's writer, nor
        // flip conn2 Offline) — otherwise the pump would serve the still-connected
        // conn2 un-gated.
        let (shared, _rx) = make_shared();
        let peer = device(0x23);
        let (_queue1, mut rx1) = outbound_pair();
        let (queue2, mut rx2) = outbound_pair();

        let gen1 = shared.register_link(peer, outbound_pair().0, true);
        shared.table.set(peer, PeerConnState::Connected, None);
        // conn2 supersedes conn1: fresh generation, its own writer.
        let gen2 = shared.register_link(peer, queue2, true);
        shared.table.set(peer, PeerConnState::Connected, None);
        assert!(gen2 > gen1);

        // conn1 (the superseded link) closes.
        assert!(!shared.teardown_if_current(
            peer,
            gen1,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));

        // The gate for the still-live conn2 is intact.
        assert!(
            shared.is_provisional(peer),
            "superseded conn1 close must leave conn2's provisional mark set"
        );
        assert_eq!(
            shared.table.state_of(peer),
            Some(PeerConnState::Connected),
            "superseded conn1 close must not flip the live conn2 Offline"
        );
        // conn2's writer still delivers; conn1's is gone.
        shared.route(&dummy_outgoing(peer));
        assert!(
            rx2.try_recv().is_some(),
            "conn2's writer must survive conn1's close"
        );
        assert!(
            rx1.try_recv().is_none(),
            "conn1's writer was replaced, not the live one"
        );

        // When conn2 itself finally closes at its own generation, the gate clears.
        assert!(shared.teardown_if_current(
            peer,
            gen2,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));
        assert!(!shared.is_provisional(peer));
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Offline));
    }

    #[test]
    fn concurrent_supersede_and_teardown_never_clears_a_live_gate() {
        use std::sync::Barrier;
        use std::thread;

        // Real threads racing the exact #126 interleaving: a superseded link's
        // close (thread A) against a successor link's registration (thread B). For
        // every interleaving the live successor must remain provisional — if the
        // CAS were not atomic under one lock, A could clear the mark B just set.
        let (shared, _rx) = make_shared();
        let peer = device(0x25);
        let mut current = shared.register_link(peer, outbound_pair().0, true);

        for _ in 0..500 {
            let barrier = Arc::new(Barrier::new(2));
            let old_gen = current;

            let a = {
                let shared = shared.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shared.teardown_if_current(
                        peer,
                        old_gen,
                        LinkTeardown::Offline(OfflineReason::LinkDropped),
                    );
                })
            };
            let b = {
                let shared = shared.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shared.register_link(peer, outbound_pair().0, true)
                })
            };

            a.join().expect("teardown thread");
            let new_gen = b.join().expect("register thread");

            assert!(
                shared.is_provisional(peer),
                "a superseded link's concurrent close cleared the live successor's gate"
            );
            assert!(
                new_gen > old_gen,
                "the successor must get a fresh generation"
            );
            current = new_gen;
        }
    }

    #[test]
    fn teardown_if_current_unauthorized_exit_sets_unauthorized() {
        let (shared, _rx) = make_shared();
        let peer = device(0x24);
        let gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);

        assert!(shared.teardown_if_current(peer, gen, LinkTeardown::Unauthorized));
        assert_eq!(
            shared.table.state_of(peer),
            Some(PeerConnState::Unauthorized)
        );
    }

    #[test]
    fn invalidate_link_makes_a_late_teardown_a_noop_preserving_deauthorized() {
        // The #126 follow-up: the manager's forced deauthorize teardown must win
        // over an inbound accept task's later close. Invalidating the generation
        // first turns that close into a no-op, so `Deauthorized` is not overwritten.
        let (shared, _rx) = make_shared();
        let peer = device(0x26);
        // An inbound accept link is live at generation `gen`.
        let gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);

        // Manager deauthorize: invalidate the generation, then force the terminal
        // state (mirrors `PeerManager::reconcile`'s stop path).
        shared.invalidate_link(peer);
        shared.unregister(peer);
        shared
            .table
            .set_offline(peer, OfflineReason::Deauthorized, None);

        // The accept task's late close, at the now-superseded generation, no-ops.
        assert!(!shared.teardown_if_current(
            peer,
            gen,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));

        // The terminal `Deauthorized` reason survives — not clobbered by LinkDropped.
        let reason = shared
            .table
            .entries()
            .into_iter()
            .find(|(d, _)| *d == peer)
            .map(|(_, e)| e.offline_reason);
        assert_eq!(reason, Some(OfflineReason::Deauthorized));
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Offline));
    }

    // --- Issue #136: live-link guard on the dial loop's stomp paths ------------

    #[test]
    fn set_offline_if_no_link_records_offline_when_no_link_registered() {
        // The benign case: no live link exists, so the dial loop's failed-connect
        // path freely records Offline{Unreachable} — the pre-#136 behavior, still
        // correct for a genuinely unreachable peer.
        let (shared, _rx) = make_shared();
        let peer = device(0x30);
        assert!(
            shared.set_offline_if_no_link(peer, OfflineReason::Unreachable),
            "no live link registered → must record Offline"
        );
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Offline));
        let reason = shared
            .table
            .entries()
            .into_iter()
            .find(|(d, _)| *d == peer)
            .map(|(_, e)| e.offline_reason);
        assert_eq!(reason, Some(OfflineReason::Unreachable));
    }

    #[test]
    fn set_offline_if_no_link_preserves_live_link_state() {
        // The #136 defect, reduced: a live link is registered (an inbound accept
        // established it); the stale-address dial loop's failed-connect path must
        // NOT stomp its `Connected` state to Offline.
        let (shared, _rx) = make_shared();
        let peer = device(0x31);
        // The inbound accept registers a link (outbound queue + connection
        // generation) and flips the peer Connected — exactly what
        // `peer::register_connection` + the accept handler do.
        let _gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);

        assert!(
            !shared.set_offline_if_no_link(peer, OfflineReason::Unreachable),
            "a live link is registered → must NOT record Offline"
        );
        assert_eq!(
            shared.table.state_of(peer),
            Some(PeerConnState::Connected),
            "the live link's Connected state must survive the stale dial's stomp"
        );
    }

    #[test]
    fn set_offline_if_no_link_records_offline_after_link_torn_down() {
        // Once the live link's teardown clears the outbound queue, the guard no
        // longer protects the peer — a subsequent failed-connect may record
        // Offline. This is the steady-state contract: the guard tracks liveness,
        // not history.
        let (shared, _rx) = make_shared();
        let peer = device(0x32);
        let gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);
        assert!(shared.teardown_if_current(
            peer,
            gen,
            LinkTeardown::Offline(OfflineReason::LinkDropped)
        ));

        assert!(
            shared.set_offline_if_no_link(peer, OfflineReason::Unreachable),
            "after teardown cleared the live link, the guard must allow Offline"
        );
        let reason = shared
            .table
            .entries()
            .into_iter()
            .find(|(d, _)| *d == peer)
            .map(|(_, e)| e.offline_reason);
        assert_eq!(reason, Some(OfflineReason::Unreachable));
    }

    #[test]
    fn set_connecting_if_no_link_records_connecting_when_no_link_registered() {
        let (shared, _rx) = make_shared();
        let peer = device(0x33);
        assert!(
            shared.set_connecting_if_no_link(peer),
            "no live link registered → must record Connecting"
        );
        assert_eq!(shared.table.state_of(peer), Some(PeerConnState::Connecting));
    }

    #[test]
    fn set_connecting_if_no_link_preserves_live_link_state() {
        // Symmetric to the Offline guard: the top-of-iteration Connecting set
        // must not stomp a live link's Connected state. Without this, a
        // stale-address dial would briefly flip the peer Connecting on every
        // backoff tick even though an inbound link is carrying data.
        let (shared, _rx) = make_shared();
        let peer = device(0x34);
        let _gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);

        assert!(
            !shared.set_connecting_if_no_link(peer),
            "a live link is registered → must NOT record Connecting"
        );
        assert_eq!(
            shared.table.state_of(peer),
            Some(PeerConnState::Connected),
            "the live link's Connected state must survive the stale dial's Connecting stomp"
        );
    }

    #[test]
    fn set_offline_if_no_link_preserves_live_link_across_repeated_stoms() {
        // The observed #136 signature: the stale-address dial loop retries
        // indefinitely (the never-give-up contract), each failed connect
        // attempting to stomp. Every iteration must leave the live link's
        // Connected state intact, not just the first.
        let (shared, _rx) = make_shared();
        let peer = device(0x35);
        let _gen = shared.register_link(peer, outbound_pair().0, false);
        shared.table.set(peer, PeerConnState::Connected, None);

        for _ in 0..50 {
            assert!(
                !shared.set_offline_if_no_link(peer, OfflineReason::Unreachable),
                "no iteration of the stale dial may stomp a live link"
            );
            assert_eq!(
                shared.table.state_of(peer),
                Some(PeerConnState::Connected),
                "Connected must hold across every backoff tick"
            );
        }
    }

    #[test]
    fn set_offline_if_no_link_and_register_link_serialize_atomically() {
        // Real threads racing the exact #136 interleaving: a stale-address dial's
        // failed-connect (thread A) against an inbound accept's register_link
        // (thread B). For every interleaving, either A's Offline lands BEFORE B
        // registers (so B then flips it Connected — correct), or A's Offline is
        // suppressed because B already registered (so the peer stays Connected —
        // correct). The one outcome that must NEVER happen is A's Offline
        // overwriting an already-established live link — that is the #136 defect.
        use std::sync::Barrier;
        use std::thread;

        let (shared, _rx) = make_shared();
        let peer = device(0x36);

        for _ in 0..500 {
            // Clean slate for this round: no live link, peer Offline.
            shared.unregister(peer);
            shared.table.set(peer, PeerConnState::Offline, None);

            let barrier = Arc::new(Barrier::new(2));
            let a = {
                let shared = shared.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shared.set_offline_if_no_link(peer, OfflineReason::Unreachable);
                })
            };
            let b = {
                let shared = shared.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    // The inbound accept: register the link, then flip Connected.
                    shared.register_link(peer, outbound_pair().0, false);
                    shared.table.set(peer, PeerConnState::Connected, None);
                })
            };

            a.join().expect("set_offline thread");
            b.join().expect("register_link thread");

            // The invariant: if a live link is registered, the table MUST read
            // Connected — never Offline. A stale-address stomp landing AFTER the
            // register would violate this; the generations-lock guard prevents it.
            let has_link = shared
                .outbound_queue_depths()
                .into_iter()
                .any(|(d, _)| d == peer);
            if has_link {
                assert_eq!(
                    shared.table.state_of(peer),
                    Some(PeerConnState::Connected),
                    "a registered live link must never read Offline (#136 invariant)"
                );
            }
        }
    }

    // --- Shared::connected_peers ---

    #[test]
    fn connected_peers_is_empty_on_fresh_shared() {
        let (shared, _rx) = make_shared();
        assert!(shared.connected_peers().is_empty());
    }

    #[test]
    fn connected_peers_returns_only_connected_devices_as_peer_ids() {
        let (shared, _rx) = make_shared();
        let connected = device(0x0A);
        shared.table.set(connected, PeerConnState::Connected, None);
        shared.table.set(device(0x0B), PeerConnState::Offline, None);
        shared
            .table
            .set(device(0x0C), PeerConnState::Unauthorized, None);

        let peers = shared.connected_peers();
        assert_eq!(peers.len(), 1, "only the Connected device should appear");
        // PeerId and EndpointId carry the same 32 device-id bytes (Membership §1).
        assert_eq!(peers[0].as_bytes(), connected.as_bytes());
    }

    #[test]
    fn connected_peers_excludes_connecting_state() {
        let (shared, _rx) = make_shared();
        shared
            .table
            .set(device(0x10), PeerConnState::Connecting, None);
        assert!(
            shared.connected_peers().is_empty(),
            "Connecting is not authenticated; connected_peers must exclude it"
        );
    }

    // --- Issue #171: Shared::route branches Events → gossip broadcast --------
    //
    // The surgical seam (spec §4 D1). When the `gossip_overlay` feature is on,
    // `Events` frames are broadcast on the room's gossip topic instead of
    // per-peer queue fan-out; every other variant stays on the queue path
    // unchanged. The pull/query variants rely on per-link FIFO that gossip's
    // epidemic delivery does not provide.
    //
    // These tests run under both feature configurations:
    // - default (overlay off): `Events` rides the queue path like today; the
    //   branch test confirms a registered peer still receives the frame.
    // - `gossip_overlay`: `Events` is routed through the (absent) gossip mesh
    //   and silently dropped; pull/query variants still land on the queue.
    //   This pins the spec's "no mesh ⇒ silent drop" contract (D1) and the
    //   "pull variants unchanged" contract.

    /// Issue #171 / spec §4 D1 + open question 4: under the gossip overlay,
    /// an `Events` frame is ALSO broadcast on the room's gossip mesh when one
    /// is installed (dual-path delivery); the per-peer queue path is retained
    /// as the fallback / coexisting channel (engine `event_id` dedup makes a
    /// duplicate idempotent). Without the overlay, the legacy per-peer queue
    /// path is the only path. This test pins the queue-side guarantee under
    /// both feature configurations: a registered peer always receives Events
    /// via the queue. The gossip-side delivery is exercised by the loopback
    /// integration test (Step 2 verify).
    #[test]
    fn route_events_branch_respects_gossip_overlay_feature() {
        let (shared, _rx) = make_shared();
        let peer = device(0x40);
        let (queue, mut rx) = outbound_pair();
        shared.register_outbound(peer, queue);

        shared.route(&events_outgoing(peer, 64));

        // The per-peer queue always carries Events under both feature
        // configurations (dual-path under the overlay, queue-only otherwise).
        assert!(
            rx.try_recv().is_some(),
            "Events must ride the per-peer queue under both feature configurations"
        );
    }

    /// Issue #171 / spec §4 D1: pull/query variants stay on the per-peer queue
    /// path regardless of the overlay feature — they rely on per-link FIFO
    /// that gossip cannot provide. Pinned for both `WantMembership`
    /// (governance) and `AdminTip` (governance) so a future variant
    /// reclassification cannot silently move them onto gossip.
    #[test]
    fn route_pull_variants_stay_on_per_peer_queue_under_overlay() {
        let (shared, _rx) = make_shared();
        let peer = device(0x41);
        let (queue, mut rx) = outbound_pair();
        shared.register_outbound(peer, queue);

        let want_membership_body = want_membership_outgoing(peer).msg.encode();
        let admin_tip_body = admin_tip_outgoing(peer).msg.encode();

        shared.route(&want_membership_outgoing(peer));
        shared.route(&admin_tip_outgoing(peer));

        // Pull/query variants always land on the queue, in priority order
        // (governance first). The order itself is exercised by
        // `route_accepts_governance_when_content_stream_budget_is_full`
        // above; here we only assert both bodies arrive.
        let bodies = [rx.try_recv(), rx.try_recv()];
        assert!(bodies.iter().all(std::option::Option::is_some));
        let got: Vec<Vec<u8>> = bodies.into_iter().map(|b| b.expect("queued")).collect();
        assert!(
            got.contains(&want_membership_body),
            "WantMembership must ride the per-peer queue"
        );
        assert!(
            got.contains(&admin_tip_body),
            "AdminTip must ride the per-peer queue"
        );
    }

    // ── Issue #171 / spec §4 D2 + Step 1 verify: GOSSIP_ALPN admission gate ──
    //
    // The non-negotiable reject-before-bytes guarantee, pinned by deterministic
    // loopback QUIC (no external services). An unadmitted device's GOSSIP_ALPN
    // connection must close with `REJECT_CODE` before the inner gossip handler
    // runs — zero gossip bytes are ever exchanged. This is the focused
    // negative/security regression for Risk 1; the route-branch tests above
    // cover the D1 routing seam, and these cover the D2 admission seam.

    #[cfg(feature = "gossip_overlay")]
    mod gossip_admission {
        use super::*;
        use crate::admission::{Admission, AdmissionDecision, AllowlistAdmission, RejectCause};
        use crate::alpn::GOSSIP_ALPN;
        use crate::audit::AuditSink;
        use crate::gossip::{spawn_gossip_actor, GossipProtocolHandler};
        use crate::handler::REJECT_CODE;

        use iroh::endpoint::presets;
        use iroh::endpoint::{ApplicationClose, ConnectionError};
        use iroh::protocol::Router;
        use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
        use iroh_rooms_core::event::keys::IdentityKey;

        use std::net::{Ipv4Addr, SocketAddr};
        use std::time::Duration;

        /// A spy `AuditSink` that records every `gossip_topic_rejected` call so
        /// the reject path can be asserted to have run (and run exactly once).
        #[derive(Default)]
        struct SpyAudit {
            topic_rejected: Mutex<Vec<(EndpointId, RejectCause)>>,
        }

        impl AuditSink for SpyAudit {
            fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {}
            fn rejected(&self, _device: EndpointId, _cause: RejectCause) {}
            fn connected(&self, _device: EndpointId) {}
            fn disconnected(&self, _device: EndpointId) {}
            fn gossip_topic_rejected(&self, device: EndpointId, cause: RejectCause) {
                self.topic_rejected.lock().unwrap().push((device, cause));
            }
        }

        /// Build a minimal `Shared` over `admission` + `audit`, with empty
        /// routing tables and an inert gossip state. Mirrors the parent module's
        /// `make_shared` so the wrapper under test consults the exact same state
        /// shape production uses, without needing a full `NetTransport::bind`.
        fn make_shared_with_gate(
            me: EndpointId,
            admission: Arc<dyn Admission>,
            audit: Arc<dyn AuditSink>,
        ) -> Arc<Shared> {
            let (inbound, _inbound_rx) = BytePriorityQueue::channel(
                DEFAULT_PER_PEER_QUEUE_BYTES,
                DEFAULT_PER_STREAM_QUEUE_BYTES,
            );
            Arc::new(Shared {
                me,
                admission,
                audit,
                table: PeerTable::new(8),
                outbound: Mutex::new(HashMap::new()),
                outbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
                stream_queue_bytes: DEFAULT_PER_STREAM_QUEUE_BYTES,
                connections: Mutex::new(HashMap::new()),
                provisional: Mutex::new(HashSet::new()),
                capability_proven: Mutex::new(HashSet::new()),
                generations: Mutex::new(HashMap::new()),
                inbound,
                gossip_state: crate::gossip::GossipState::inert(),
                peer_manager: Mutex::new(None),
            })
        }

        fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
            let port = ep
                .bound_sockets()
                .into_iter()
                .map(|s| s.port())
                .next()
                .expect("bound UDP socket");
            EndpointAddr::new(ep.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        }

        struct ProvisionalAdmission;

        impl Admission for ProvisionalAdmission {
            fn authorize(&self, _device: EndpointId) -> AdmissionDecision {
                AdmissionDecision::AdmitProvisional
            }
        }

        /// Issue #171 / spec §4 D2 (Risk 1 — non-negotiable): a `GOSSIP_ALPN`
        /// connection from a device the admission gate rejects (here:
        /// `UnknownDevice` — an unbound device) is closed with `REJECT_CODE`
        /// before the inner gossip handler runs. Pinned by (a) the dialer
        /// observing the application close at `REJECT_CODE`, (b) the spy
        /// recording exactly one `gossip_topic_rejected`, and (c) the device
        /// table flipping to `Unauthorized`. The audit + table writes precede
        /// `conn.close()` in the handler, so observing the close guarantees
        /// they have run.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gossip_alpn_rejects_unadmitted_device_before_delegate() {
            let server_sk = SecretKey::from_bytes(&[0x71; 32]);
            let dialer_sk = SecretKey::from_bytes(&[0x72; 32]);
            let server_id = server_sk.public();
            let dialer_id = dialer_sk.public();

            // Admission binds NO devices → the dialer resolves to UnknownDevice.
            let admission: Arc<dyn Admission> = Arc::new(AllowlistAdmission::new());
            let spy = Arc::new(SpyAudit::default());
            let shared =
                make_shared_with_gate(server_id, admission, Arc::clone(&spy) as Arc<dyn AuditSink>);

            let server_ep = Endpoint::builder(presets::Minimal)
                .secret_key(server_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("server bind");
            let server_addr = loopback_addr(&server_ep);

            let gossip = spawn_gossip_actor(server_ep.clone());
            let handler = GossipProtocolHandler::new(Arc::clone(&shared), gossip.clone());
            // Keep the router alive for the test's duration: dropping it stops
            // the accept loop before the reject can be observed.
            let _router = Router::builder(server_ep.clone())
                .accept(GOSSIP_ALPN, handler)
                .spawn();

            let dialer_ep = Endpoint::builder(presets::Minimal)
                .secret_key(dialer_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("dialer bind");

            let conn = dialer_ep
                .connect(server_addr, GOSSIP_ALPN)
                .await
                .expect("dialer connect");

            // The server's accept() rejects and closes before delegating.
            // Observe the application close on the dialer side.
            let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .expect("connection must close (reject-before-bytes)");
            match closed {
                ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. }) => {
                    assert_eq!(
                        error_code, REJECT_CODE,
                        "the GOSSIP_ALPN admission gate must close with REJECT_CODE"
                    );
                }
                other => panic!("expected an ApplicationClosed at REJECT_CODE, got: {other:?}"),
            }

            // The reject path ran exactly once and recorded the dialer + cause.
            let recorded = spy.topic_rejected.lock().unwrap().clone();
            assert_eq!(
                recorded.len(),
                1,
                "admission must audit the GOSSIP_ALPN reject exactly once"
            );
            assert_eq!(
                recorded[0].0, dialer_id,
                "the rejected device is the dialer"
            );
            assert_eq!(
                recorded[0].1,
                RejectCause::UnknownDevice,
                "an unbound device rejects as UnknownDevice"
            );

            // The device table flipped to Unauthorized (mirrors `handler.rs:60`).
            assert_eq!(
                shared.table.state_of(dialer_id),
                Some(PeerConnState::Unauthorized),
                "a rejected GOSSIP_ALPN device must be marked Unauthorized"
            );
        }

        /// Review regression: an open join window may provisionally admit an
        /// unknown device on `EVENT_ALPN`, but gossip has no membership-only
        /// capability protocol. The same verdict must therefore be rejected
        /// before the inner gossip handler sees the connection.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gossip_alpn_rejects_provisional_device_before_delegate() {
            let server_sk = SecretKey::from_bytes(&[0x75; 32]);
            let dialer_sk = SecretKey::from_bytes(&[0x76; 32]);
            let server_id = server_sk.public();
            let dialer_id = dialer_sk.public();
            let spy = Arc::new(SpyAudit::default());
            let shared = make_shared_with_gate(
                server_id,
                Arc::new(ProvisionalAdmission),
                Arc::clone(&spy) as Arc<dyn AuditSink>,
            );

            let server_ep = Endpoint::builder(presets::Minimal)
                .secret_key(server_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("server bind");
            let gossip = spawn_gossip_actor(server_ep.clone());
            let _router = Router::builder(server_ep.clone())
                .accept(
                    GOSSIP_ALPN,
                    GossipProtocolHandler::new(shared.clone(), gossip),
                )
                .spawn();
            let dialer_ep = Endpoint::builder(presets::Minimal)
                .secret_key(dialer_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("dialer bind");
            let conn = dialer_ep
                .connect(loopback_addr(&server_ep), GOSSIP_ALPN)
                .await
                .expect("dialer connect");

            let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .expect("provisional gossip connection must close");
            assert!(matches!(
                closed,
                ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. })
                    if error_code == REJECT_CODE
            ));
            assert_eq!(
                spy.topic_rejected.lock().unwrap().as_slice(),
                &[(dialer_id, RejectCause::UnknownDevice)]
            );
            assert_eq!(
                shared.table.state_of(dialer_id),
                Some(PeerConnState::Unauthorized)
            );
        }

        /// Issue #171 / spec §4 D2: the gossip gate inherits the event plane's
        /// reject-cause vocabulary because both consult the same
        /// `Arc<dyn Admission>`. A device that IS bound but whose identity is
        /// not Active rejects as `NotActive` (not `UnknownDevice`). Pinned so a
        /// future change cannot collapse the §16.3 reject vocabulary on the
        /// gossip path.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gossip_alpn_reject_propagates_the_admission_cause() {
            let server_sk = SecretKey::from_bytes(&[0x73; 32]);
            let dialer_sk = SecretKey::from_bytes(&[0x74; 32]);
            let server_id = server_sk.public();
            let dialer_id = dialer_sk.public();
            let bound_identity = IdentityKey::from_bytes([0xB0; 32]);

            // Bound but NOT Active → NotActive.
            let admission: Arc<dyn Admission> =
                Arc::new(AllowlistAdmission::new().bind_device(dialer_id, bound_identity));
            let spy = Arc::new(SpyAudit::default());
            let shared =
                make_shared_with_gate(server_id, admission, Arc::clone(&spy) as Arc<dyn AuditSink>);

            let server_ep = Endpoint::builder(presets::Minimal)
                .secret_key(server_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("server bind");
            let server_addr = loopback_addr(&server_ep);
            let gossip = spawn_gossip_actor(server_ep.clone());
            let handler = GossipProtocolHandler::new(Arc::clone(&shared), gossip.clone());
            let _router = Router::builder(server_ep.clone())
                .accept(GOSSIP_ALPN, handler)
                .spawn();

            let dialer_ep = Endpoint::builder(presets::Minimal)
                .secret_key(dialer_sk)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("dialer bind");
            let conn = dialer_ep
                .connect(server_addr, GOSSIP_ALPN)
                .await
                .expect("dialer connect");

            let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .expect("connection must close");
            assert!(
                matches!(
                    closed,
                    ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. })
                        if error_code == REJECT_CODE
                ),
                "bound-but-inactive device must still be rejected at REJECT_CODE; got: {closed:?}"
            );

            let recorded = spy.topic_rejected.lock().unwrap().clone();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].0, dialer_id);
            assert_eq!(
                recorded[0].1,
                RejectCause::NotActive,
                "a bound-but-inactive device rejects as NotActive on the gossip plane"
            );
        }
    }

    // ── Issue #171 / spec §4 D1 + Step 2 verify: gossip overlay delivery ──
    //
    // The loopback integration test the spec's Step 2 verify names and the
    // `route_events_branch_respects_gossip_overlay_feature` test above
    // references ("The gossip-side delivery is exercised by the loopback
    // integration test"). Two in-process nodes, each with a real `GossipMesh`
    // installed on a real `Shared`, wired through a `MemoryLookup` so the
    // `iroh-gossip` swarm can form on loopback (loopback has no discovery
    // service — the same seam `spike-transport/src/gossip.rs::spawn` uses).
    // Broadcasting an encoded `SyncMessage::Events` via `Shared::route` must
    // arrive at the receiver's inbound sink as the same canonical CBOR bytes
    // (D1), having crossed the `GOSSIP_ALPN` admission gate on both sides
    // (D2). This crosses the full boundary the focused route tests cannot:
    // `route` → `GossipMesh::broadcast_events` → real GOSSIP_ALPN QUIC →
    // receiver task → `try_enqueue_inbound` → inbound sink.

    #[cfg(feature = "gossip_overlay")]
    mod gossip_overlay_delivery {
        use super::*;
        use crate::admission::{Admission, RejectCause};
        use crate::alpn::GOSSIP_ALPN;
        use crate::audit::AuditSink;
        use crate::gossip::{events_topic, spawn_gossip_actor, GossipMesh, GossipProtocolHandler};
        use iroh_rooms_core::event::keys::IdentityKey;

        use iroh::address_lookup::memory::MemoryLookup;
        use iroh::endpoint::presets;
        use iroh::protocol::Router;
        use iroh::{Endpoint, EndpointAddr, RelayMode};

        use std::net::{Ipv4Addr, SocketAddr};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        /// Per-step budget — loopback gossip swarm forms in well under this.
        const WAIT: Duration = Duration::from_secs(15);

        /// A throwaway identity deterministically derived from the device seed
        /// (admission needs *some* bound identity to mark Active; its value is
        /// irrelevant to the overlay delivery path under test).
        fn identity(seed: u8) -> IdentityKey {
            IdentityKey::from_bytes([seed.wrapping_add(0x40); 32])
        }

        fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
            let port = ep
                .bound_sockets()
                .into_iter()
                .map(|s| s.port())
                .next()
                .expect("bound UDP socket");
            EndpointAddr::new(ep.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        }

        /// A recording `AuditSink` that counts gossip-broadcast outcomes on the
        /// sender: `gossip_broadcast` (the broadcast task completed Ok) and
        /// `transport_queue_saturated("gossip_out")` (it errored). Exactly one
        /// outcome fires per broadcast task that actually ran, so the combined
        /// count is the observable that pins the per-mesh dedup (issue #171):
        /// routing the identical body N times must collapse to one outcome.
        #[derive(Default)]
        struct BroadcastSpy {
            ok: AtomicUsize,
            err: AtomicUsize,
        }

        impl BroadcastSpy {
            fn outcomes(&self) -> usize {
                self.ok.load(Ordering::Relaxed) + self.err.load(Ordering::Relaxed)
            }
        }

        impl AuditSink for BroadcastSpy {
            fn accepted(&self, _: EndpointId, _: &IdentityKey) {}
            fn rejected(&self, _: EndpointId, _: RejectCause) {}
            fn connected(&self, _: EndpointId) {}
            fn disconnected(&self, _: EndpointId) {}
            fn gossip_broadcast(&self, _: RoomId, _: usize) {
                self.ok.fetch_add(1, Ordering::Relaxed);
            }
            fn transport_queue_saturated(&self, _: EndpointId, queue: &'static str) {
                if queue == "gossip_out" {
                    self.err.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        /// One gossip-overlay node on loopback: a real `Endpoint` (with a
        /// `MemoryLookup` pre-loaded with `known_addrs` so the gossip actor can
        /// dial bootstrap `EndpointId`s — loopback has no discovery service,
        /// the same seam `spike-transport/src/gossip.rs::spawn` uses), a real
        /// gossip actor spawned on it, the `GOSSIP_ALPN` admission-gate handler
        /// chained on the router (D2), and a `Shared` whose `gossip_state`
        /// carries the live actor.
        ///
        /// The returned `Router` and `InboundReceiver` must outlive the test
        /// body: dropping the `Router` stops the `GOSSIP_ALPN` accept loop, and
        /// the `InboundReceiver` is the inlet a gossip-delivered frame surfaces
        /// on.
        async fn spawn_overlay_node(
            seed: u8,
            members: &[u8],
            known_addrs: Vec<EndpointAddr>,
        ) -> (Arc<Shared>, InboundReceiver, Endpoint, Router) {
            spawn_overlay_node_with_audit(seed, members, known_addrs, Arc::new(TracingAudit)).await
        }

        /// Same as [`spawn_overlay_node`] but with a caller-supplied audit sink
        /// — used by the dedup test to count broadcasts at the sender (the exact
        /// point the N-1 amplification occurs; iroh-gossip dedups identical
        /// content at delivery, so receiver-side counting cannot surface it).
        async fn spawn_overlay_node_with_audit(
            seed: u8,
            members: &[u8],
            known_addrs: Vec<EndpointAddr>,
            audit: Arc<dyn AuditSink>,
        ) -> (Arc<Shared>, InboundReceiver, Endpoint, Router) {
            let sk = SecretKey::from_bytes(&[seed; 32]);
            let me = sk.public();

            let mut admission = AllowlistAdmission::new();
            for &s in members {
                admission = admission
                    .bind_device(device(s), identity(s))
                    .set_active(identity(s));
            }
            let admission: Arc<dyn Admission> = Arc::new(admission);

            let lookup = MemoryLookup::new();
            for addr in &known_addrs {
                lookup.add_endpoint_info(addr.clone());
            }
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(sk)
                .relay_mode(RelayMode::Disabled)
                .address_lookup(lookup)
                .bind()
                .await
                .expect("bind loopback endpoint");

            let gossip = spawn_gossip_actor(endpoint.clone());

            let (inbound, inbound_rx) = BytePriorityQueue::channel(
                DEFAULT_PER_PEER_QUEUE_BYTES,
                DEFAULT_PER_STREAM_QUEUE_BYTES,
            );
            let shared = Arc::new(Shared {
                me,
                admission,
                audit,
                table: PeerTable::new(8),
                outbound: Mutex::new(HashMap::new()),
                outbound_peer_queue_bytes: DEFAULT_PER_PEER_QUEUE_BYTES,
                stream_queue_bytes: DEFAULT_PER_STREAM_QUEUE_BYTES,
                connections: Mutex::new(HashMap::new()),
                provisional: Mutex::new(HashSet::new()),
                capability_proven: Mutex::new(HashSet::new()),
                generations: Mutex::new(HashMap::new()),
                inbound,
                gossip_state: crate::gossip::GossipState::with_actor(gossip.clone()),
                peer_manager: Mutex::new(None),
            });

            let handler = GossipProtocolHandler::new(shared.clone(), gossip);
            let router = Router::builder(endpoint.clone())
                .accept(GOSSIP_ALPN, handler)
                .spawn();

            (shared, InboundReceiver { rx: inbound_rx }, endpoint, router)
        }

        /// Poll the overlay's gossip-neighbor count until it reaches
        /// `threshold` or `budget` elapses (spec §5.4 — the swarm formed a
        /// direct neighbor link, the precondition for broadcast delivery and
        /// the observability surface `Node::gossip_neighbor_count` exposes).
        async fn wait_for_neighbors(shared: &Shared, threshold: usize, budget: Duration) -> bool {
            let deadline = tokio::time::Instant::now() + budget;
            loop {
                if shared.gossip_state.neighbor_count() >= threshold {
                    return true;
                }
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        /// Issue #171 / spec §4 D1 + Step 2 verify: an encoded
        /// `SyncMessage::Events` body broadcast on the room's gossip topic by
        /// `Shared::route` arrives at the receiver's inbound sink as the same
        /// canonical CBOR bytes — the headline positive-delivery proof for the
        /// gossip overlay.
        ///
        /// The two nodes are admitted to each other (D2: the `GOSSIP_ALPN` gate
        /// lets the swarm connection through on both sides), and the swarm is
        /// given time to form a direct neighbor link before the broadcast
        /// (spike surprise 1: `joined()` is awaited inside `GossipMesh::spawn`).
        /// The delivered bytes are byte-compared against a freshly encoded copy
        /// — D1's "a peer cannot tell which path delivered it" contract.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gossip_overlay_broadcast_delivers_events_to_inbound_sink() {
            let room = RoomId::from_bytes([0xAD; 32]);
            let a_seed: u8 = 0x61;
            let b_seed: u8 = 0x62;

            // Node A: first member, no known peer addrs yet. Both nodes admit
            // each other's device (D2 — the GOSSIP_ALPN connection must clear
            // admission on both sides for the swarm to form).
            let (a_shared, _a_rx, a_ep, _a_router) =
                spawn_overlay_node(a_seed, &[a_seed, b_seed], vec![]).await;
            let a_addr = loopback_addr(&a_ep);
            let a_id = a_ep.id();

            // Node B: knows A's loopback addr so the gossip actor can dial A's
            // EndpointId as a bootstrap seed (loopback has no discovery).
            let (b_shared, mut b_rx, b_ep, _b_router) =
                spawn_overlay_node(b_seed, &[a_seed, b_seed], vec![a_addr]).await;
            let b_id = b_ep.id();

            // Sanity: the derived topic is the same pure function both peers
            // compute from the public room_id (D5) — a mismatch would mean the
            // two meshes subscribe to different topics and never see each other.
            assert_eq!(
                crate::gossip::events_topic(&room),
                events_topic(&room),
                "both peers must derive the same per-room TopicId (D5)"
            );

            // Install the per-room gossip mesh on both nodes. A subscribes with
            // no bootstrap (first member); B bootstraps from A's EndpointId —
            // the deterministic seed set (D3). `GossipMesh::spawn` awaits
            // `receiver.joined()` on B's side before returning (spike surprise
            // 1), so the swarm has a direct neighbor link by the time we
            // broadcast.
            let a_actor = a_shared
                .gossip_state
                .actor()
                .expect("A has a live gossip actor")
                .clone();
            let a_mesh = GossipMesh::spawn(a_shared.clone(), a_actor, room, vec![])
                .await
                .expect("A subscribes the room gossip topic");
            a_shared.gossip_state.install_mesh(room, a_mesh);

            let b_actor = b_shared
                .gossip_state
                .actor()
                .expect("B has a live gossip actor")
                .clone();
            let b_mesh = GossipMesh::spawn(b_shared.clone(), b_actor, room, vec![a_id])
                .await
                .expect("B subscribes + joins the room gossip topic");
            b_shared.gossip_state.install_mesh(room, b_mesh);

            // The swarm formed a direct neighbor link on both sides (D8
            // NeighborUp, surfaced for observability — spec §5.4). This is the
            // precondition for broadcast delivery and pins the
            // `Node::gossip_neighbor_count` surface.
            assert!(
                wait_for_neighbors(&a_shared, 1, WAIT).await,
                "A must report at least one gossip neighbor after B joined"
            );
            assert!(
                wait_for_neighbors(&b_shared, 1, WAIT).await,
                "B must report at least one gossip neighbor after joining"
            );

            // The surgical seam (D1): routing an `Events` frame broadcasts the
            // encoded body on the room's gossip topic. `broadcast_events` is
            // fire-and-forget (spawns a task), so delivery is awaited below.
            let out = Outgoing {
                peer: PeerId::from_bytes(*b_id.as_bytes()),
                msg: SyncMessage::Events {
                    room_id: room,
                    frames: vec![vec![0xEE; 64]],
                },
            };
            let expected_body = out.msg.encode();
            a_shared.route(&out);

            // The receiver task feeds the broadcast content into B's inbound
            // sink as verbatim bytes (D8). The bytes are the same canonical
            // CBOR the per-peer queue path would have sent (D1).
            let delivered = tokio::time::timeout(WAIT, b_rx.recv())
                .await
                .expect("B must receive the gossip broadcast within budget")
                .expect("B's inbound stream must not close");
            assert_eq!(
                delivered.bytes, expected_body,
                "the gossip-delivered bytes must equal the encoded SyncMessage \
                 (D1: same canonical CBOR as the per-peer queue path)"
            );

            // The delivering peer recorded by the inbound sink is A (the gossip
            // neighbor that handed B the frame), as a PeerId carrying A's
            // device-id bytes — the engine keys dedup on event_id, not on the
            // delivering peer, so this is correct (D8).
            assert_eq!(
                delivered.peer.as_bytes(),
                a_id.as_bytes(),
                "delivered_from must be A's device id (B's only gossip neighbor)"
            );
        }

        /// Review regression: both sides of the gossip boundary enforce the
        /// direct event-plane `MAX_FRAME_BYTES` contract. The public sender
        /// refuses an oversized body, and even a bypassed/raw gossip sender
        /// cannot enqueue that body into the receiver's inbound sink.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gossip_overlay_rejects_oversized_bodies_on_send_and_receive() {
            let room = RoomId::from_bytes([0xAF; 32]);
            let a_seed = 0x63;
            let b_seed = 0x64;
            let spy = Arc::new(BroadcastSpy::default());
            let (a_shared, _a_rx, a_ep, _a_router) = spawn_overlay_node_with_audit(
                a_seed,
                &[a_seed, b_seed],
                vec![],
                Arc::clone(&spy) as Arc<dyn AuditSink>,
            )
            .await;
            let (b_shared, mut b_rx, _b_ep, _b_router) =
                spawn_overlay_node(b_seed, &[a_seed, b_seed], vec![loopback_addr(&a_ep)]).await;

            let a_mesh = GossipMesh::spawn(
                a_shared.clone(),
                a_shared.gossip_state.actor().unwrap().clone(),
                room,
                vec![],
            )
            .await
            .expect("A mesh");
            a_shared.gossip_state.install_mesh(room, a_mesh.clone());
            let b_mesh = GossipMesh::spawn(
                b_shared.clone(),
                b_shared.gossip_state.actor().unwrap().clone(),
                room,
                vec![a_ep.id()],
            )
            .await
            .expect("B mesh");
            b_shared.gossip_state.install_mesh(room, b_mesh);
            assert!(wait_for_neighbors(&a_shared, 1, WAIT).await);

            let oversized = vec![0xCD; crate::frame::MAX_FRAME_BYTES as usize + 1];
            a_mesh.broadcast_events(
                Arc::clone(&spy) as Arc<dyn AuditSink>,
                a_ep.id(),
                oversized.clone(),
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(spy.outcomes(), 0, "sender must not call iroh-gossip");

            a_mesh
                .broadcast_unchecked_for_test(oversized)
                .await
                .expect("raw test broadcast reaches receiver guard");
            assert!(
                tokio::time::timeout(Duration::from_millis(300), b_rx.recv())
                    .await
                    .is_err(),
                "receiver must not enqueue an oversized gossip body"
            );
        }

        /// Review regression: the receiver task must not own its parent mesh.
        /// Removing the final map/local handles drops the mesh immediately and
        /// aborts the task instead of leaving a self-sustaining Arc cycle.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn removing_mesh_breaks_receiver_task_ownership() {
            let room = RoomId::from_bytes([0xB0; 32]);
            let (shared, _rx, _ep, _router) = spawn_overlay_node(0x65, &[0x65], vec![]).await;
            let mesh = GossipMesh::spawn(
                shared.clone(),
                shared.gossip_state.actor().unwrap().clone(),
                room,
                vec![],
            )
            .await
            .expect("mesh");
            let weak = Arc::downgrade(&mesh);
            shared.gossip_state.install_mesh(room, mesh.clone());
            drop(mesh);
            drop(shared.gossip_state.remove_mesh(&room));
            tokio::task::yield_now().await;
            assert!(
                weak.upgrade().is_none(),
                "receiver task must not retain its GossipMesh"
            );
        }

        /// Issue #171 dedup: the engine fans an accepted `Events` frame out to
        /// every connected peer (`engine.rs` per-peer loop), so `Shared::route`
        /// reaches `broadcast_events` once per peer carrying the **identical**
        /// encoded body. Without per-mesh dedup this is N-1 identical gossip
        /// broadcasts per event — the O(N²) fan-out issue #171 exists to
        /// eliminate. The headline delivery test above runs at N=2 (one peer),
        /// so it cannot surface the redundancy.
        ///
        /// This pins the dedup by counting the sender's broadcast outcomes via
        /// a recording audit: iroh-gossip dedups identical content at
        /// *delivery*, so counting received copies would not surface the bug —
        /// only the sender-side audit (one outcome per broadcast task that ran)
        /// does. Routing the same body to several distinct peers must collapse
        /// to exactly one outcome.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn route_dedups_identical_events_bodies_to_one_gossip_broadcast() {
            // Number of distinct peers the engine's per-peer fan-out simulates
            // (5 > the N=2 the headline delivery test runs at).
            const FANOUT_PEERS: u8 = 5;
            let room = RoomId::from_bytes([0xAE; 32]);
            let a_seed: u8 = 0x71;
            let b_seed: u8 = 0x72;

            // A carries a recording audit so the dedup is measured at the
            // broadcast source.
            let spy = Arc::new(BroadcastSpy::default());
            let (a_shared, _a_rx, a_ep, _a_router) = spawn_overlay_node_with_audit(
                a_seed,
                &[a_seed, b_seed],
                vec![],
                Arc::clone(&spy) as Arc<dyn AuditSink>,
            )
            .await;
            let a_addr = loopback_addr(&a_ep);
            let a_id = a_ep.id();

            let (b_shared, _b_rx, _b_ep, _b_router) =
                spawn_overlay_node(b_seed, &[a_seed, b_seed], vec![a_addr]).await;

            // Same topology + neighbor wait as the delivery test: both meshes
            // installed, a direct neighbor link formed so A's broadcast reliably
            // completes (and fires its audit).
            let a_actor = a_shared
                .gossip_state
                .actor()
                .expect("A has a live gossip actor")
                .clone();
            let a_mesh = GossipMesh::spawn(a_shared.clone(), a_actor, room, vec![])
                .await
                .expect("A subscribes the room gossip topic");
            a_shared.gossip_state.install_mesh(room, a_mesh);

            let b_actor = b_shared
                .gossip_state
                .actor()
                .expect("B has a live gossip actor")
                .clone();
            let b_mesh = GossipMesh::spawn(b_shared.clone(), b_actor, room, vec![a_id])
                .await
                .expect("B subscribes + joins the room gossip topic");
            b_shared.gossip_state.install_mesh(room, b_mesh);

            assert!(
                wait_for_neighbors(&a_shared, 1, WAIT).await,
                "A must report at least one gossip neighbor after B joined"
            );
            assert!(
                wait_for_neighbors(&b_shared, 1, WAIT).await,
                "B must report at least one gossip neighbor after joining"
            );

            // Simulate the engine's per-peer fan-out: route the SAME Events body
            // to several distinct peers (5 > the N=2 the headline delivery test
            // runs at). All carry byte-identical frames → one body hash, so the
            // per-mesh dedup must collapse them to a single gossip broadcast.
            let frame = vec![0xEE; 64];
            for n in 0..FANOUT_PEERS {
                let peer = PeerId::from_bytes([0xC0 + n; 32]);
                let out = Outgoing {
                    peer,
                    msg: SyncMessage::Events {
                        room_id: room,
                        frames: vec![frame.clone()],
                    },
                };
                a_shared.route(&out);
            }

            // The broadcast task is fire-and-forget, so poll the spy until the
            // outcome count stabilizes (≥1 outcome with no increase for 150ms)
            // or it reaches FANOUT_PEERS (a definite regression — stop early).
            // With the dedup exactly one task runs → one outcome; without it,
            // FANOUT_PEERS tasks run → FANOUT_PEERS outcomes.
            let mut last = 0usize;
            let mut stable_at = tokio::time::Instant::now();
            let deadline = tokio::time::Instant::now() + WAIT;
            loop {
                let total = spy.outcomes();
                if total != last {
                    last = total;
                    stable_at = tokio::time::Instant::now();
                }
                if total >= FANOUT_PEERS as usize {
                    break;
                }
                if last >= 1 && stable_at.elapsed() >= Duration::from_millis(150) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                last, 1,
                "the per-mesh dedup must collapse the {FANOUT_PEERS} identical route calls to one \
                 gossip broadcast (observed {last} broadcast outcomes)",
            );
        }
    }
}
