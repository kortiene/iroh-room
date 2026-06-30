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

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, VarInt};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
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
use crate::state::{ConnEvent, PeerConnState, PeerTable};

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

/// Construction parameters for a [`NetTransport`].
#[derive(Debug, Clone, Copy)]
pub struct NetConfig {
    /// Loopback vs real-network endpoint stack.
    pub mode: NetMode,
    /// Broadcast backlog for the [`ConnEvent`] stream before a slow observer lags.
    pub conn_event_capacity: usize,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: NetMode::Loopback,
            conn_event_capacity: 256,
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
    /// The single inbound sink feeding the engine driver.
    pub(crate) inbound_tx: mpsc::UnboundedSender<Inbound>,
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
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn bind(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        cfg: NetConfig,
        pipe_handler: Option<PipeProtocolHandler>,
    ) -> Result<Self> {
        let endpoint = match cfg.mode {
            NetMode::Loopback => Endpoint::builder(presets::Minimal)
                .secret_key(secret)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .context("bind loopback endpoint")?,
            NetMode::RealNetwork => Endpoint::builder(presets::N0)
                .secret_key(secret)
                .bind()
                .await
                .context("bind real-network endpoint")?,
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

    /// The current state of one device, if known.
    #[must_use]
    pub fn peer_state(&self, device: EndpointId) -> Option<PeerConnState> {
        self.shared.table.state_of(device)
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
    use super::{Inbound, NetConfig, NetMode, Shared};
    use crate::admission::AllowlistAdmission;
    use crate::audit::TracingAudit;
    use crate::state::{PeerConnState, PeerTable};
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::sync::{Outgoing, PeerId, SyncMessage};
    use std::collections::HashMap;
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
