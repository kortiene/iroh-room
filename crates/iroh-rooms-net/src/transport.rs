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
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, VarInt};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_rooms_core::sync::{Outgoing, PeerId, SyncTransport};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::admission::Admission;
use crate::alpn::EVENT_ALPN;
use crate::audit::AuditSink;
use crate::handler::EventProtocolHandler;
use crate::peer::{dial_loop, peer_id};
use crate::pipe::alpn::PIPE_ALPN;
use crate::pipe::PipeProtocolHandler;
use crate::state::{ConnEvent, OfflineReason, PeerConnState, PeerEntry, PeerTable};

/// Normal application close code for a locally-initiated disconnect (distinct from
/// [`crate::handler::REJECT_CODE`], which means "unauthorized").
const LOCAL_CLOSE_CODE: VarInt = VarInt::from_u32(0);

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
    /// Broadcast backlog for the [`ConnEvent`] stream before a slow observer lags.
    pub conn_event_capacity: usize,
    /// Ring capacity of the `Node::room_events` broadcast (issue #83). Lossy on
    /// lag exactly like `conn_event_capacity`; a slow subscriber gets `Lagged`.
    pub room_event_capacity: usize,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: NetMode::Loopback,
            conn_event_capacity: 256,
            room_event_capacity: 256,
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

/// Shared transport state: the identity/authorizer/audit + the per-peer routing
/// tables + the inbound sink. Cloned (as `Arc<Shared>`) into the accept handler
/// and every connection task so they observe one consistent view.
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
    outbound: Mutex<HashMap<EndpointId, mpsc::UnboundedSender<Vec<u8>>>>,
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
    /// The single inbound sink feeding the engine driver.
    pub(crate) inbound_tx: mpsc::UnboundedSender<Inbound>,
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
    /// Register a peer's outbound frame queue (replaces any prior queue for the
    /// device — last writer wins on a double-connect, spec OQ-4).
    pub(crate) fn register_outbound(&self, device: EndpointId, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device, tx);
    }

    /// Record a live connection handle for the device.
    pub(crate) fn register_connection(&self, device: EndpointId, conn: Connection) {
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(device, conn);
    }

    /// Drop a peer's outbound queue + connection handle (on disconnect). Dropping
    /// the queue's sender ends that peer's writer task.
    pub(crate) fn unregister(&self, device: EndpointId) {
        self.outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&device);
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

    /// Route one engine [`Outgoing`] to its peer's writer queue. Non-blocking;
    /// **dropped** if the peer has no live writer (offline / unauthorized) — never
    /// buffered to a non-member, and the engine re-pulls on reconnect (spec §6 /
    /// the [`SyncTransport`] contract).
    pub(crate) fn route(&self, out: &Outgoing) {
        let Ok(device) = EndpointId::from_bytes(out.peer.as_bytes()) else {
            tracing::warn!("route: outgoing peer id is not a valid endpoint id; dropping");
            return;
        };
        let body = out.msg.encode();
        let guard = self
            .outbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tx) = guard.get(&device) {
            // Unbounded, non-blocking: a send error only means the writer is gone.
            let _ = tx.send(body);
        }
        // No live writer ⇒ peer offline ⇒ drop (engine re-pulls on reconnect).
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
        tx: mpsc::UnboundedSender<Vec<u8>>,
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
        self.register_outbound(device, tx);
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
}

/// The full-mesh direct-QUIC event-transport adapter.
pub struct NetTransport {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    router: Router,
    inbound_rx: Option<mpsc::UnboundedReceiver<Inbound>>,
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
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn bind(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        cfg: NetConfig,
        pipe_handler: Option<PipeProtocolHandler>,
        blobs_handler: Option<iroh_blobs::BlobsProtocol>,
    ) -> Result<Self> {
        let endpoint = match cfg.mode {
            NetMode::Loopback => Endpoint::builder(presets::Minimal)
                .secret_key(secret)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .context("bind loopback endpoint")?,
            NetMode::RealNetwork => {
                let builder = Endpoint::builder(presets::N0).secret_key(secret);
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
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            me,
            admission,
            audit,
            table: PeerTable::new(cfg.conn_event_capacity),
            outbound: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            provisional: Mutex::new(HashSet::new()),
            capability_proven: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            inbound_tx,
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
        let router = builder.spawn();

        Ok(Self {
            shared,
            endpoint,
            router,
            inbound_rx: Some(inbound_rx),
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

    /// Take the inbound raw-frame receiver (once). The engine driver owns it and
    /// feeds each frame to `SyncEngine::ingest_frame`.
    pub fn take_inbound(&mut self) -> Option<mpsc::UnboundedReceiver<Inbound>> {
        self.inbound_rx.take()
    }

    /// A clone of the shared state, for an external driver that routes engine
    /// outputs without the [`SyncTransport`] `&mut self` borrow.
    #[must_use]
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Gracefully stop: abort dial loops and shut the router down.
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
    use super::{Inbound, LinkTeardown, NetConfig, NetMode, Shared, RELAY_ONLY_TEST_BUILD};
    use crate::admission::AllowlistAdmission;
    use crate::audit::TracingAudit;
    use crate::state::{OfflineReason, PeerConnState, PeerTable};
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::sync::{Outgoing, PeerId, SyncMessage};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// Construct a `Shared` with no connections or queues registered.
    fn make_shared() -> (Arc<Shared>, mpsc::UnboundedReceiver<Inbound>) {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            me: device(0x01),
            admission: Arc::new(AllowlistAdmission::new()),
            audit: Arc::new(TracingAudit),
            table: PeerTable::new(8),
            outbound: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            provisional: Mutex::new(HashSet::new()),
            capability_proven: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            inbound_tx,
        });
        (shared, inbound_rx)
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

    // --- NetConfig defaults ---

    #[test]
    fn net_config_default_mode_is_loopback() {
        assert_eq!(NetConfig::default().mode, NetMode::Loopback);
    }

    #[test]
    fn net_config_default_conn_event_capacity_is_256() {
        assert_eq!(NetConfig::default().conn_event_capacity, 256);
    }

    #[test]
    fn net_config_default_room_event_capacity_is_256() {
        assert_eq!(NetConfig::default().room_event_capacity, 256);
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
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        shared.register_outbound(peer, tx);

        let out = dummy_outgoing(peer);
        let expected = out.msg.encode();
        shared.route(&out);

        let received = rx
            .try_recv()
            .expect("frame must be queued for the registered peer");
        assert_eq!(received, expected);
    }

    // --- Shared::unregister removes the outbound queue ---

    #[test]
    fn route_drops_frame_after_unregister() {
        let (shared, _rx) = make_shared();
        let peer = device(0x04);
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        shared.register_outbound(peer, tx);
        shared.unregister(peer);

        shared.route(&dummy_outgoing(peer));
        assert!(
            rx.try_recv().is_err(),
            "no frame must arrive after the peer is unregistered"
        );
    }

    // --- Issue #126: connection-generation guard on teardown -----------------

    #[test]
    fn register_link_generations_increase_per_device_and_start_fresh_per_device() {
        let (shared, _rx) = make_shared();
        let a = device(0x20);
        let b = device(0x21);
        let (tx, _rx1) = mpsc::unbounded_channel::<Vec<u8>>();

        // Each successive link on the same device gets a strictly greater stamp,
        // so a superseded link can always be distinguished from its successor.
        assert_eq!(shared.register_link(a, tx.clone(), false), 1);
        assert_eq!(shared.register_link(a, tx.clone(), false), 2);
        assert_eq!(shared.register_link(a, tx.clone(), false), 3);
        // A different device starts its own sequence at 1.
        assert_eq!(shared.register_link(b, tx, false), 1);
    }

    #[test]
    fn teardown_if_current_tears_down_only_at_the_current_generation() {
        let (shared, _rx) = make_shared();
        let peer = device(0x22);
        let (tx, _rx0) = mpsc::unbounded_channel::<Vec<u8>>();

        let gen = shared.register_link(peer, tx, true);
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
        let (tx1, mut rx1) = mpsc::unbounded_channel::<Vec<u8>>();
        let (tx2, mut rx2) = mpsc::unbounded_channel::<Vec<u8>>();

        let gen1 = shared.register_link(peer, tx1, true);
        shared.table.set(peer, PeerConnState::Connected, None);
        // conn2 supersedes conn1: fresh generation, its own writer.
        let gen2 = shared.register_link(peer, tx2, true);
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
            rx2.try_recv().is_ok(),
            "conn2's writer must survive conn1's close"
        );
        assert!(
            rx1.try_recv().is_err(),
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
        let (tx, _rx0) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut current = shared.register_link(peer, tx.clone(), true);

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
                let tx = tx.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shared.register_link(peer, tx, true)
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
        let (tx, _rx0) = mpsc::unbounded_channel::<Vec<u8>>();
        let gen = shared.register_link(peer, tx, false);
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
        let (tx, _rx0) = mpsc::unbounded_channel::<Vec<u8>>();

        // An inbound accept link is live at generation `gen`.
        let gen = shared.register_link(peer, tx, false);
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
}
