//! [`Node`] — a thin runtime that pairs a [`NetTransport`] with the landed,
//! sans-IO [`SyncEngine`] and pumps them (spec OQ-3 "a thin `run()` in the net
//! crate", with the CLI free to drive manually later).
//!
//! The engine is single-owner: one **pump task** owns it and is the only thing
//! that touches it, translating async I/O events into the engine's synchronous
//! entry points and routing the [`Outgoing`]s it returns —
//!
//! * a [`ConnEvent`] reaching `Connected` → `engine.on_connect` (handshake);
//!   leaving `Connected` → `engine.on_disconnect`,
//! * an inbound frame → `engine.ingest_frame`,
//! * a periodic tick → `engine.on_tick` (anti-entropy re-pull),
//! * a [`Cmd`] (publish / query / shutdown) from a [`Node`] handle.
//!
//! Callers interact through the `Node` handle (publish, query the store, observe
//! connection state); the pump keeps the engine accessible without ever sharing a
//! `&mut SyncEngine` across tasks.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::ids::EventId;
use iroh_rooms_core::membership::MembershipSnapshot;
use iroh_rooms_core::sync::{Completeness, Outgoing, SyncEngine, SyncMessage};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::admission::Admission;
use crate::audit::AuditSink;
use crate::peer::peer_id;
use crate::state::{ConnEvent, PeerConnState};
use crate::transport::{Inbound, NetConfig, NetTransport, Shared};

/// Default anti-entropy tick (drives `on_tick` re-pulls + reconnect catch-up).
pub const DEFAULT_TICK: Duration = Duration::from_millis(250);

/// A command from a [`Node`] handle to its pump task.
enum Cmd {
    Publish(Vec<u8>, oneshot::Sender<Result<(), String>>),
    Contains(EventId, oneshot::Sender<Result<bool, String>>),
    Snapshot(oneshot::Sender<MembershipSnapshot>),
    Completeness(oneshot::Sender<Completeness>),
    Shutdown(oneshot::Sender<()>),
}

/// A running event-transport node: a [`NetTransport`] carrier + an engine pump.
pub struct Node {
    transport: NetTransport,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    pump: JoinHandle<()>,
}

impl Node {
    /// Bind a transport for `secret` and spawn the pump driving `engine` over it.
    /// `tick` is the anti-entropy interval (see [`DEFAULT_TICK`]).
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn spawn(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
    ) -> Result<Self> {
        let mut transport = NetTransport::bind(secret, admission, audit, cfg).await?;
        let inbound_rx = transport
            .take_inbound()
            .ok_or_else(|| anyhow!("inbound receiver already taken"))?;
        let conn_rx = transport.conn_events();
        let shared = transport.shared();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let pump = tokio::spawn(pump(engine, inbound_rx, conn_rx, shared, cmd_rx, tick));

        Ok(Self {
            transport,
            cmd_tx,
            pump,
        })
    }

    /// This node's authenticated identity (`device_id == EndpointId`).
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.transport.id()
    }

    /// A dialable address for this node (out-of-band exchange in lieu of a ticket).
    ///
    /// # Errors
    /// Propagates [`NetTransport::endpoint_addr`].
    pub fn endpoint_addr(&self) -> Result<EndpointAddr> {
        self.transport.endpoint_addr()
    }

    /// Start a dial-with-backoff loop toward `addr`.
    pub fn connect_to(&self, addr: EndpointAddr) {
        self.transport.connect_to(addr);
    }

    /// Locally close the link to `device` (reconnect-test / roster-change hook).
    pub fn disconnect_peer(&self, device: EndpointId) {
        self.transport.disconnect_peer(device);
    }

    /// The current [`PeerConnState`] of one device, if known.
    #[must_use]
    pub fn peer_state(&self, device: EndpointId) -> Option<PeerConnState> {
        self.transport.peer_state(device)
    }

    /// Point-in-time snapshot of all known peers' connection states.
    #[must_use]
    pub fn peer_states(&self) -> Vec<(EndpointId, PeerConnState)> {
        self.transport.peer_states()
    }

    /// Subscribe to the live [`ConnEvent`] transition stream.
    #[must_use]
    pub fn conn_events(&self) -> broadcast::Receiver<ConnEvent> {
        self.transport.conn_events()
    }

    /// Publish a locally-authored, stateless-valid `WireEvent` frame: the engine
    /// ingests it and fans it out (as `SyncMessage::Events`) to connected peers.
    ///
    /// # Errors
    /// Returns an error if the frame fails stateless validation or the pump is gone.
    pub async fn publish(&self, wire_bytes: Vec<u8>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Publish(wire_bytes, tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("pump dropped the reply"))?
            .map_err(|e| anyhow!(e))
    }

    /// Whether this node's validated set contains `id` (via the engine digest —
    /// the event id is `BLAKE3(wire.signed)`, so id-equality implies byte-equality).
    ///
    /// # Errors
    /// Returns an error on a store read failure or if the pump is gone.
    pub async fn store_contains(&self, id: EventId) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Contains(id, tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("pump dropped the reply"))?
            .map_err(|e| anyhow!(e))
    }

    /// The current convergent membership snapshot.
    ///
    /// # Errors
    /// Returns an error if the pump is gone.
    pub async fn snapshot(&self) -> Result<MembershipSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Snapshot(tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await.map_err(|_| anyhow!("pump dropped the reply"))
    }

    /// The current admin-completeness verdict.
    ///
    /// # Errors
    /// Returns an error if the pump is gone.
    pub async fn completeness(&self) -> Result<Completeness> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Completeness(tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await.map_err(|_| anyhow!("pump dropped the reply"))
    }

    /// Wait (up to `timeout`) until `device` reaches `want`, polling the table.
    ///
    /// # Errors
    /// Returns an error if the deadline elapses first.
    pub async fn wait_for_state(
        &self,
        device: EndpointId,
        want: PeerConnState,
        timeout: Duration,
    ) -> Result<()> {
        tokio::time::timeout(timeout, async {
            while self.peer_state(device) != Some(want) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for {device} -> {}", want.label()))
    }

    /// Wait (up to `timeout`) until this node's validated set contains `id`.
    ///
    /// # Errors
    /// Returns an error if the deadline elapses first.
    pub async fn wait_until_contains(&self, id: EventId, timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, async {
            loop {
                if self.store_contains(id).await.unwrap_or(false) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for event {id} to reach the store"))
    }

    /// Gracefully stop: drain the pump and shut the transport's router down.
    ///
    /// # Errors
    /// Propagates [`NetTransport::shutdown`].
    pub async fn shutdown(self) -> Result<()> {
        let Self {
            transport,
            cmd_tx,
            pump,
        } = self;
        let (tx, rx) = oneshot::channel();
        if cmd_tx.send(Cmd::Shutdown(tx)).is_ok() {
            let _ = rx.await;
        }
        pump.abort();
        transport.shutdown().await
    }
}

/// The single task that owns the engine and routes its outputs.
async fn pump(
    mut engine: SyncEngine,
    mut inbound_rx: mpsc::UnboundedReceiver<Inbound>,
    mut conn_rx: broadcast::Receiver<ConnEvent>,
    shared: Arc<Shared>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    tick: Duration,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                if handle_cmd(&mut engine, &shared, cmd) {
                    break;
                }
            }
            inbound = inbound_rx.recv() => {
                let Some(inbound) = inbound else { continue };
                // The wire carries canonical-CBOR `SyncMessage` frames (live push is
                // a `SyncMessage::Events`). Decode here — at the engine-driver
                // boundary — and feed `on_message`; an undecodable frame is a logged
                // drop, never a crash (spec §4.3 defense-in-depth).
                match SyncMessage::decode(&inbound.bytes) {
                    Ok(msg) => {
                        let outs = engine.on_message(inbound.peer, msg);
                        route_all(&shared, outs);
                    }
                    Err(err) => {
                        tracing::debug!(%err, peer = %inbound.peer, "pump: dropping undecodable inbound frame");
                    }
                }
            }
            event = conn_rx.recv() => {
                match event {
                    Ok(event) => handle_conn_event(&mut engine, &shared, event),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "pump: conn-event stream lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {}
                }
            }
            _ = ticker.tick() => {
                let outs = engine.on_tick(now_ms());
                route_all(&shared, outs);
            }
        }
    }
}

/// Apply one command; returns `true` if the pump should stop.
fn handle_cmd(engine: &mut SyncEngine, shared: &Arc<Shared>, cmd: Cmd) -> bool {
    match cmd {
        Cmd::Publish(bytes, reply) => {
            let result = match engine.publish(&bytes) {
                Ok(outs) => {
                    route_all(shared, outs);
                    Ok(())
                }
                Err(err) => Err(err.to_string()),
            };
            let _ = reply.send(result);
            false
        }
        Cmd::Contains(id, reply) => {
            let result = engine
                .digest()
                .map(|d| d.event_ids.contains(&id))
                .map_err(|e| e.to_string());
            let _ = reply.send(result);
            false
        }
        Cmd::Snapshot(reply) => {
            let _ = reply.send(engine.snapshot());
            false
        }
        Cmd::Completeness(reply) => {
            let _ = reply.send(engine.completeness());
            false
        }
        Cmd::Shutdown(reply) => {
            let _ = reply.send(());
            true
        }
    }
}

/// Translate a connection-state transition into the engine's link entry points.
fn handle_conn_event(engine: &mut SyncEngine, shared: &Arc<Shared>, event: ConnEvent) {
    let peer = peer_id(event.device);
    match event.to {
        PeerConnState::Connected => {
            let outs = engine.on_connect(peer);
            route_all(shared, outs);
        }
        PeerConnState::Offline | PeerConnState::Unauthorized => {
            // Only a transition *away from* a live link is a real disconnect; a
            // first-sight Offline/Unauthorized (we never connected) is a no-op.
            if event.from == PeerConnState::Connected {
                engine.on_disconnect(peer);
            }
        }
        PeerConnState::Connecting => {}
    }
}

/// Route every engine output to its peer's writer queue (best-effort).
fn route_all(shared: &Arc<Shared>, outs: Vec<Outgoing>) {
    for out in outs {
        shared.route(&out);
    }
}

/// Advisory wall-clock ms for `on_tick` (the engine treats it as advisory only).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
