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

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::content::{Content, PipeOpened};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::{build_pipe_closed, build_pipe_opened};
use iroh_rooms_core::membership::MembershipSnapshot;
use iroh_rooms_core::store::StoredEvent;
use iroh_rooms_core::sync::{Completeness, Outgoing, PeerId, SyncEngine, SyncMessage};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::admission::{Admission, AdmissionView};
use crate::audit::AuditSink;
use crate::manager::PeerManager;
use crate::peer::peer_id;
use crate::pipe::alpn::{PIPE_ALPN, PIPE_ALPN_STR};
use crate::pipe::registry::is_loopback_target;
use crate::pipe::runtime::PipeQueryMsg;
use crate::pipe::{
    connector, new_pipe_id, watcher, PipeAuditSink, PipeDenyCause, PipeError, PipeForwarder,
    PipeHandlerState, PipeProtocolHandler, PipeQuery, PipeRegistry, PipeSessions, TracingPipeAudit,
};
use crate::state::{ConnEvent, PeerConnState, PeerEntry};
use crate::transport::{Inbound, NetConfig, NetTransport, Shared};

/// Default anti-entropy tick (drives `on_tick` re-pulls + reconnect catch-up).
pub const DEFAULT_TICK: Duration = Duration::from_millis(250);

/// A command from a [`Node`] handle to its pump task.
enum Cmd {
    Publish(Vec<u8>, oneshot::Sender<Result<(), String>>),
    Contains(EventId, oneshot::Sender<Result<bool, String>>),
    Tail(u32, oneshot::Sender<Result<Vec<StoredEvent>, String>>),
    Heads(oneshot::Sender<Result<Vec<EventId>, String>>),
    Snapshot(oneshot::Sender<MembershipSnapshot>),
    Completeness(oneshot::Sender<Completeness>),
    /// Force an immediate peer-manager reconcile + admission refresh (a test hook;
    /// a no-op for a node with no room session). See [`Node::reconcile_now`].
    Reconcile(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

/// The extra inputs a managed room session needs (spec §4.3/§4.4).
struct RoomConfig {
    /// Operator `--peer` addresses, used by the manager to resolve each device.
    addr_hints: Vec<EndpointAddr>,
    /// The live admission cell the pump refreshes each fold change; the session's
    /// admission gate reads it on the accept hot path.
    admission_cell: Arc<Mutex<AdmissionView>>,
}

/// The pump-owned reconciler that keeps the dial set and the admission cell in step
/// with the live membership fold (spec §4.3 — snapshot-diff on the existing tick).
struct RoomReconciler {
    manager: Arc<PeerManager>,
    cell: Arc<Mutex<AdmissionView>>,
    /// The last admission view we reconciled against — the cheap fold-change
    /// detector. `None` forces the next reconcile (initial + `reconcile_now`).
    last: Option<AdmissionView>,
}

impl RoomReconciler {
    /// Reconcile **only if** the membership-relevant projection of the fold changed
    /// since the last reconcile. Idempotent otherwise (no dial churn, no admission
    /// swap). Called after every fold-mutating pump step and on each tick.
    fn maybe_reconcile(&mut self, engine: &SyncEngine) {
        let snapshot = engine.snapshot();
        let view = AdmissionView::from_snapshot(&snapshot, &engine.fail_closed_subjects());
        if self.last.as_ref() == Some(&view) {
            return; // no membership-relevant change since last reconcile
        }
        // Refresh admission first so the accept gate is never *more* permissive than
        // the dial set, then reconcile the outbound loops against the same snapshot.
        *self
            .cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = view.clone();
        self.manager.reconcile(&snapshot);
        self.last = Some(view);
    }

    /// Force a reconcile regardless of the change detector (the `reconcile_now` test
    /// hook: makes a just-applied membership change take effect without waiting for
    /// the natural fold-change detection).
    fn force_reconcile(&mut self, engine: &SyncEngine) {
        self.last = None;
        self.maybe_reconcile(engine);
    }
}

/// A running event-transport node: a [`NetTransport`] carrier + an engine pump.
///
/// The node also drives the **Live Pipe Plane** (IR-0010): its `Router` serves the
/// pipe ALPN alongside events, the [`PipeRegistry`] holds locally-open pipes, the
/// [`PipeSessions`] table tracks live forwarded sessions, and a teardown
/// [`watcher`](crate::pipe::watcher) severs revoked sessions each tick.
pub struct Node {
    transport: NetTransport,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    pump: JoinHandle<()>,
    /// The room-scoped peer manager, present only for a managed room session (spec
    /// §4.1). `None` for the simple `spawn` path (join bootstrap, pipe, `send`),
    /// which keeps driving dials through [`Node::connect_to`]. Held so
    /// [`Node::shutdown`] can abort its dial loops.
    peer_manager: Option<Arc<PeerManager>>,
    pipe_query: PipeQuery,
    pipe_registry: Arc<PipeRegistry>,
    pipe_sessions: Arc<PipeSessions>,
    pipe_audit: Arc<dyn PipeAuditSink>,
    pipe_watcher: JoinHandle<()>,
}

impl Node {
    /// Bind a transport for `secret` and spawn the pump driving `engine` over it.
    /// `tick` is the anti-entropy interval (see [`DEFAULT_TICK`]).
    ///
    /// This is the **unmanaged** path: the caller drives dialing explicitly through
    /// [`Node::connect_to`] and supplies a fixed `admission` gate. Used by the join
    /// bootstrap (IR-0104), the pipe commands (IR-0010), and short-lived `room send`.
    /// For a long-running room session that must react to membership change, use
    /// [`Node::spawn_room`].
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
        Self::spawn_inner(secret, admission, audit, engine, cfg, tick, None, None).await
    }

    /// Like [`Node::spawn`] but with a caller-supplied Live-Pipe-Plane audit sink
    /// instead of the default [`TracingPipeAudit`].
    ///
    /// The `pipe expose` CLI uses this to surface owner-side reject / teardown /
    /// accept lines directly to the operator's terminal (spec IR-0108 §4.3) — the
    /// default `tracing` sink is dropped because the CLI installs no subscriber.
    /// Every other caller keeps the `TracingPipeAudit` default via [`Node::spawn`].
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    #[allow(clippy::too_many_arguments)] // one wiring seam; each arg is a distinct input
    pub async fn spawn_with_pipe_audit(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
        pipe_audit: Arc<dyn PipeAuditSink>,
    ) -> Result<Self> {
        Self::spawn_inner(
            secret,
            admission,
            audit,
            engine,
            cfg,
            tick,
            None,
            Some(pipe_audit),
        )
        .await
    }

    /// Bind a transport and spawn the pump as a **managed room session** (IR-0107,
    /// spec §4.1–§4.4).
    ///
    /// A [`PeerManager`] derives the outbound dial set from the live membership
    /// snapshot and reconciles it on every fold change; the pump swaps a fresh
    /// [`AdmissionView`] into `admission_cell` in the same place, so the (live)
    /// `admission` gate — expected to be a
    /// [`SnapshotAdmission`](crate::admission::SnapshotAdmission) reading
    /// `admission_cell`, optionally wrapped by
    /// [`JoinBootstrapAdmission`](crate::admission::JoinBootstrapAdmission) — never
    /// drifts from the dial set. The caller does **not** call [`Node::connect_to`];
    /// the manager owns dialing. `addr_hints` are the operator's `--peer` addresses
    /// used to resolve each device deterministically (loopback/LAN).
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    #[allow(clippy::too_many_arguments)] // one wiring seam; each arg is a distinct input
    pub async fn spawn_room(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
        addr_hints: Vec<EndpointAddr>,
        admission_cell: Arc<Mutex<AdmissionView>>,
    ) -> Result<Self> {
        Self::spawn_inner(
            secret,
            admission,
            audit,
            engine,
            cfg,
            tick,
            Some(RoomConfig {
                addr_hints,
                admission_cell,
            }),
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // one wiring seam; each arg is a distinct input
    async fn spawn_inner(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
        room: Option<RoomConfig>,
        pipe_audit: Option<Arc<dyn PipeAuditSink>>,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        // A dedicated channel for the Pipe plane's reads against the single-owner
        // engine (handler + watcher), drained by the same pump (spec §6.5 / D5).
        let (pipe_query_tx, pipe_query_rx) = mpsc::unbounded_channel::<PipeQueryMsg>();
        let pipe_query = PipeQuery::new(pipe_query_tx);

        let pipe_registry = Arc::new(PipeRegistry::new());
        let pipe_sessions = Arc::new(PipeSessions::new());
        // Default to the structured `tracing` sink; an explicit caller sink (the
        // CLI's stderr renderer, spec IR-0108 §4.3) overrides it when supplied.
        let pipe_audit: Arc<dyn PipeAuditSink> =
            pipe_audit.unwrap_or_else(|| Arc::new(TracingPipeAudit));

        // The pipe accept-gate handler, registered as the second ALPN on the shared
        // Router (one Endpoint serves both planes, spec §6.5.1).
        let handler_state = Arc::new(PipeHandlerState {
            query: pipe_query.clone(),
            registry: pipe_registry.clone(),
            sessions: pipe_sessions.clone(),
            audit: pipe_audit.clone(),
        });
        let pipe_handler = PipeProtocolHandler::new(handler_state);

        let mut transport =
            NetTransport::bind(secret, admission, audit, cfg, Some(pipe_handler)).await?;
        let inbound_rx = transport
            .take_inbound()
            .ok_or_else(|| anyhow!("inbound receiver already taken"))?;
        let conn_rx = transport.conn_events();
        let shared = transport.shared();

        // Build the room-scoped manager + admission-refresh reconciler when this is a
        // managed session. The manager is moved (as a clone) into the pump so the
        // single-owner engine drives reconciliation; `Node` keeps a handle to abort
        // its dial loops on shutdown.
        let (peer_manager, room_reconciler) = match room {
            Some(RoomConfig {
                addr_hints,
                admission_cell,
            }) => {
                let manager = Arc::new(PeerManager::new(
                    shared.clone(),
                    transport.endpoint(),
                    transport.id(),
                    addr_hints,
                ));
                let reconciler = RoomReconciler {
                    manager: manager.clone(),
                    cell: admission_cell,
                    last: None,
                };
                (Some(manager), Some(reconciler))
            }
            None => (None, None),
        };

        let pump = tokio::spawn(pump(
            engine,
            inbound_rx,
            conn_rx,
            shared,
            cmd_rx,
            pipe_query_rx,
            tick,
            room_reconciler,
        ));

        // The teardown-on-learn watcher (spec §4.5/D5): re-evaluates every live pipe
        // session each tick and severs any that no longer passes the gate.
        let pipe_watcher = tokio::spawn(watcher::watch(
            pipe_query.clone(),
            pipe_registry.clone(),
            pipe_sessions.clone(),
            pipe_audit.clone(),
            tick,
        ));

        Ok(Self {
            transport,
            cmd_tx,
            pump,
            peer_manager,
            pipe_query,
            pipe_registry,
            pipe_sessions,
            pipe_audit,
            pipe_watcher,
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

    /// Point-in-time snapshot of all known peers' full [`PeerEntry`] (state +
    /// offline reason + bound identity) — the source for the CLI §16.3 connection
    /// panel and `room members --status` (spec §4.5 / §6).
    #[must_use]
    pub fn peer_entries(&self) -> Vec<(EndpointId, PeerEntry)> {
        self.transport.peer_entries()
    }

    /// Subscribe to the live [`ConnEvent`] transition stream.
    #[must_use]
    pub fn conn_events(&self) -> broadcast::Receiver<ConnEvent> {
        self.transport.conn_events()
    }

    /// Force the managed peer manager to reconcile against the **current** fold now,
    /// rather than waiting for the next tick / fold-change detection (spec §5 test
    /// hook). A no-op for an unmanaged node (one spawned via [`Node::spawn`]).
    ///
    /// # Errors
    /// Returns an error if the pump is gone.
    pub async fn reconcile_now(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Reconcile(tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await.map_err(|_| anyhow!("pump dropped the reply"))
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

    /// The current room timeline — the most-recent `limit` events in canonical
    /// `(lamport, event_id)` order — for display. Routed through the pump so the
    /// engine stays single-owner (no second store handle racing its WAL writes).
    ///
    /// # Errors
    /// Returns an error on a store read failure or if the pump is gone.
    pub async fn room_tail(&self, limit: u32) -> Result<Vec<StoredEvent>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Tail(limit, tx))
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

    // ------------------------------------------------------------------
    // Live Pipe Plane (IR-0010)
    // ------------------------------------------------------------------

    /// The current DAG heads of the room — the `prev_events` a freshly authored
    /// event must cite. Routed through the pump so the engine stays single-owner.
    ///
    /// # Errors
    /// Returns an error on a store read failure or if the pump is gone.
    pub async fn heads(&self) -> Result<Vec<EventId>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Heads(tx))
            .map_err(|_| anyhow!("pump task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("pump dropped the reply"))?
            .map_err(|e| anyhow!(e))
    }

    /// The governing `pipe.opened` for `pipe_id` from this node's validated set, or
    /// `None` if it has not synced the announcement yet (spec §6.5.2 / R4).
    pub async fn pipe_opened(&self, pipe_id: [u8; 16]) -> Option<PipeOpened> {
        self.pipe_query.pipe_opened(pipe_id).await
    }

    /// **Expose** a local TCP service as a pipe (spec §6.5.1): validate the loopback
    /// target (D6) and non-empty allow-list (no default-all), draw a CSPRNG
    /// `pipe_id`, register the real target locally, author + publish the signed
    /// `pipe.opened`, and start gating connections. Returns the new `pipe_id`.
    ///
    /// `owner_identity_secret` provides both `sender_id` and `owner_id`;
    /// `owner_device_secret` signs. `created_at` is a caller-injected clock read.
    ///
    /// # Errors
    /// A non-loopback `target`, an empty `allowed_members`, or a publish/heads
    /// failure. Nothing is registered or published on the error paths that precede
    /// the publish.
    #[allow(clippy::too_many_arguments)] // each arg is a distinct signed/registry input
    pub async fn pipe_expose(
        &self,
        owner_identity_secret: &SigningKey,
        owner_device_secret: &SigningKey,
        room_id: &RoomId,
        target: SocketAddr,
        label: &str,
        target_hint: &str,
        allowed_members: &[IdentityKey],
        expires_at: Option<u64>,
        created_at: u64,
    ) -> Result<[u8; 16]> {
        if !is_loopback_target(&target) {
            bail!("{}", PipeError::NonLoopbackTarget(target));
        }
        if allowed_members.is_empty() {
            bail!("{}", PipeError::EmptyAllowList);
        }

        let pipe_id = new_pipe_id();
        let owner_endpoint = owner_device_secret.device_key();
        let heads = self.heads().await?;
        let wire = build_pipe_opened(
            owner_identity_secret,
            owner_device_secret,
            room_id,
            pipe_id,
            &owner_endpoint,
            label,
            target_hint,
            PIPE_ALPN_STR,
            allowed_members,
            expires_at,
            &heads,
            created_at,
        );

        // Register the real loopback target before publishing so a connector that
        // races in on the freshly-synced announcement finds it (the registry
        // re-validates the loopback rule).
        let event = SignedEvent::decode(&wire.signed)
            .map_err(|r| anyhow!("freshly built pipe.opened failed to decode: {r:?}"))?;
        let Content::PipeOpened(opened) = event.content else {
            bail!("freshly built event is not a pipe.opened");
        };
        self.pipe_registry
            .insert(opened, target)
            .map_err(|e| anyhow!("{e}"))?;

        if let Err(err) = self.publish(wire.to_bytes()).await {
            // Roll back the local registration so a failed publish leaves no
            // dangling open pipe.
            self.pipe_registry.remove(&pipe_id);
            return Err(err.context("could not publish pipe.opened"));
        }
        self.pipe_audit.opened(&pipe_id, allowed_members.len());
        Ok(pipe_id)
    }

    /// **Close** a pipe (spec §6.5.1): author + publish a signed `pipe.closed`,
    /// remove the local target, and tear down every live session for it. The signer
    /// must be the pipe owner or the room admin (folded downstream).
    ///
    /// # Errors
    /// A heads/publish failure (the local teardown still runs best-effort after a
    /// successful publish).
    pub async fn pipe_close(
        &self,
        signer_identity_secret: &SigningKey,
        signer_device_secret: &SigningKey,
        room_id: &RoomId,
        pipe_id: [u8; 16],
        reason: Option<&str>,
        created_at: u64,
    ) -> Result<()> {
        let heads = self.heads().await?;
        let wire = build_pipe_closed(
            signer_identity_secret,
            signer_device_secret,
            room_id,
            pipe_id,
            reason,
            &heads,
            created_at,
        );
        self.publish(wire.to_bytes())
            .await
            .map_err(|e| e.context("could not publish pipe.closed"))?;

        self.pipe_registry.remove(&pipe_id);
        for (device, pid) in self.pipe_sessions.teardown_pipe(&pipe_id) {
            self.pipe_audit
                .torndown(device, &pid, PipeDenyCause::Closed);
        }
        self.pipe_audit.closed(&pipe_id, reason.unwrap_or("closed"));
        Ok(())
    }

    /// **Connect** to a pipe (spec §6.5.2): resolve the synced `pipe.opened`, verify
    /// the dialable owner address matches its `owner_endpoint`, bind a loopback
    /// listener on `127.0.0.1:local_port` (`0` ⇒ OS-assigned), and forward each
    /// local TCP connection over the pipe ALPN. Returns a [`PipeForwarder`] carrying
    /// the bound local address.
    ///
    /// # Errors
    /// [`PipeError::UnknownPipe`] if the announcement is not synced yet,
    /// [`PipeError::OwnerEndpointMismatch`] if the address does not match the signed
    /// `owner_endpoint`, or a listener-bind / owner-dial failure.
    pub async fn pipe_connect(
        &self,
        owner_addr: EndpointAddr,
        pipe_id: [u8; 16],
        local_port: u16,
    ) -> Result<PipeForwarder> {
        let Some(opened) = self.pipe_opened(pipe_id).await else {
            bail!("{}", PipeError::UnknownPipe(pipe_id));
        };
        if owner_addr.id.as_bytes() != opened.owner_endpoint.as_bytes() {
            bail!("{}", PipeError::OwnerEndpointMismatch);
        }
        let endpoint = self.transport.endpoint();
        let forwarder = connector::connect(&endpoint, owner_addr, pipe_id, PIPE_ALPN, local_port)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(forwarder)
    }

    /// A clone of the underlying iroh [`Endpoint`] (for an out-of-band pipe dial in
    /// tests / tooling that drive the connector directly).
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.transport.endpoint()
    }

    /// The number of live pipe sessions currently being forwarded (observability /
    /// tests for the teardown path).
    #[must_use]
    pub fn live_pipe_sessions(&self) -> usize {
        self.pipe_sessions.len()
    }

    /// Gracefully stop: drain the pump, stop the pipe watcher, and shut the
    /// transport's router down.
    ///
    /// # Errors
    /// Propagates [`NetTransport::shutdown`].
    pub async fn shutdown(self) -> Result<()> {
        let Self {
            transport,
            cmd_tx,
            pump,
            peer_manager,
            pipe_watcher,
            ..
        } = self;
        let (tx, rx) = oneshot::channel();
        if cmd_tx.send(Cmd::Shutdown(tx)).is_ok() {
            let _ = rx.await;
        }
        // Abort the managed dial loops (if any) so they do not outlive the session.
        if let Some(manager) = peer_manager {
            manager.shutdown();
        }
        pump.abort();
        pipe_watcher.abort();
        transport.shutdown().await
    }
}

/// Serve one Pipe-plane read against the engine (snapshot / governing pipe.opened /
/// pipe.closed-known). Fail-closed: a store/decode error answers `None` / `closed`.
fn serve_pipe_query(engine: &SyncEngine, query: PipeQueryMsg) {
    match query {
        PipeQueryMsg::Snapshot(reply) => {
            let _ = reply.send(engine.snapshot());
        }
        PipeQueryMsg::Opened(pipe_id, reply) => {
            let _ = reply.send(engine.pipe_opened(&pipe_id).ok().flatten());
        }
        PipeQueryMsg::IsClosed(pipe_id, reply) => {
            let _ = reply.send(engine.pipe_is_closed(&pipe_id).unwrap_or(true));
        }
    }
}

/// The single task that owns the engine and routes its outputs.
///
/// For a managed room session (`room` is `Some`) the pump also owns the
/// [`RoomReconciler`]: after every fold-mutating step (publish, inbound message) and
/// on each tick it reconciles the [`PeerManager`] dial set and refreshes the live
/// admission cell against the current snapshot (spec §4.3 — snapshot-diff on the
/// existing tick, so no new membership-change event plumbing is introduced).
#[allow(clippy::too_many_arguments)] // one wiring seam; each channel/handle is distinct
async fn pump(
    mut engine: SyncEngine,
    mut inbound_rx: mpsc::UnboundedReceiver<Inbound>,
    mut conn_rx: broadcast::Receiver<ConnEvent>,
    shared: Arc<Shared>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    mut pipe_query_rx: mpsc::UnboundedReceiver<PipeQueryMsg>,
    tick: Duration,
    mut room: Option<RoomReconciler>,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Establish the initial dial set + admission view from the opening snapshot so a
    // managed session starts dialing its active members immediately (not after the
    // first fold change).
    if let Some(room) = room.as_mut() {
        room.maybe_reconcile(&engine);
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                if handle_cmd(&mut engine, &shared, &mut room, cmd) {
                    break;
                }
            }
            query = pipe_query_rx.recv() => {
                // Pipe-plane reads against the single-owner engine (snapshot /
                // governing pipe.opened / pipe.closed-known). A closed channel just
                // means no pipe plane is active; keep pumping.
                if let Some(query) = query {
                    serve_pipe_query(&engine, query);
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
                        // Join-bootstrap restriction (IR-0104, Approach A): a
                        // *provisional* peer is served the membership sub-DAG only.
                        // Drop any request that would serve it chat/files/arbitrary
                        // events; allow membership pulls and its `member.joined` push.
                        let device = endpoint_of(inbound.peer);
                        let provisional = device.is_some_and(|d| shared.is_provisional(d));
                        if provisional && !provisional_allows(&msg) {
                            if let Some(d) = device {
                                shared.audit.bootstrap_blocked(d, sync_message_kind(&msg));
                            }
                        } else {
                            let outs = engine.on_message(inbound.peer, msg);
                            route_all(&shared, outs);
                            if provisional {
                                // Upgrade-on-learn: if that frame was the join the
                                // fold accepted, the peer is now an Active member —
                                // lift the restriction and record its identity.
                                maybe_upgrade_provisional(&engine, &shared, inbound.peer);
                            }
                            // An inbound message may have advanced membership (a join
                            // landed, a member was removed): reconcile against it.
                            if let Some(room) = room.as_mut() {
                                room.maybe_reconcile(&engine);
                            }
                        }
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
                // The anti-entropy cadence doubles as the roster-reactive reconcile
                // trigger (spec §4.3): bounded ≤1-tick latency to react to a change.
                if let Some(room) = room.as_mut() {
                    room.maybe_reconcile(&engine);
                }
            }
        }
    }
}

/// Apply one command; returns `true` if the pump should stop.
fn handle_cmd(
    engine: &mut SyncEngine,
    shared: &Arc<Shared>,
    room: &mut Option<RoomReconciler>,
    cmd: Cmd,
) -> bool {
    match cmd {
        Cmd::Publish(bytes, reply) => {
            let result = match engine.publish(&bytes) {
                Ok(outs) => {
                    route_all(shared, outs);
                    Ok(())
                }
                Err(err) => Err(err.to_string()),
            };
            // A publish can advance membership (e.g. an admin `member.removed`):
            // reconcile the dial set + admission against the new snapshot.
            if let Some(room) = room.as_mut() {
                room.maybe_reconcile(engine);
            }
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
        Cmd::Tail(limit, reply) => {
            let result = engine.room_tail(limit).map_err(|e| e.to_string());
            let _ = reply.send(result);
            false
        }
        Cmd::Heads(reply) => {
            let result = engine.heads().map_err(|e| e.to_string());
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
        Cmd::Reconcile(reply) => {
            // Force an immediate reconcile against the current fold (a no-op for a
            // node with no room session). Bypasses the change detector so a
            // just-applied membership change takes effect without waiting for a tick.
            if let Some(room) = room.as_mut() {
                room.force_reconcile(engine);
            }
            let _ = reply.send(());
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

/// Map an engine [`PeerId`] back to its transport [`EndpointId`] (same 32
/// `device_id` bytes). `None` only on a malformed id, which never originates from a
/// real connection.
fn endpoint_of(peer: PeerId) -> Option<EndpointId> {
    EndpointId::from_bytes(peer.as_bytes()).ok()
}

/// Whether a provisional join-bootstrap peer (IR-0104, Approach A) may be served
/// `msg`. It may pull the never-windowed membership sub-DAG ([`WantMembership`]) and
/// push its `member.joined` ([`Events`], which the fold judges), plus the harmless
/// tip/head/not-found advertisements. It may **not** pull chat or arbitrary events
/// ([`WantRecentChat`] / [`WantEvents`]) — those would serve room content to a
/// not-yet-member, the privacy regression the spec scopes out.
///
/// [`WantMembership`]: SyncMessage::WantMembership
/// [`Events`]: SyncMessage::Events
/// [`WantRecentChat`]: SyncMessage::WantRecentChat
/// [`WantEvents`]: SyncMessage::WantEvents
fn provisional_allows(msg: &SyncMessage) -> bool {
    matches!(
        msg,
        SyncMessage::WantMembership { .. }
            | SyncMessage::Events { .. }
            | SyncMessage::AdminTip { .. }
            | SyncMessage::Heads { .. }
            | SyncMessage::NotFound { .. }
    )
}

/// A stable, greppable kind string for the `join.bootstrap.blocked` audit line.
fn sync_message_kind(msg: &SyncMessage) -> &'static str {
    match msg {
        SyncMessage::AdminTip { .. } => "admin_tip",
        SyncMessage::Heads { .. } => "heads",
        SyncMessage::WantEvents { .. } => "want_events",
        SyncMessage::WantMembership { .. } => "want_membership",
        SyncMessage::WantRecentChat { .. } => "want_recent_chat",
        SyncMessage::Events { .. } => "events",
        SyncMessage::NotFound { .. } => "not_found",
        // `SyncMessage` is `#[non_exhaustive]`; an unknown future kind is "other".
        _ => "other",
    }
}

/// Upgrade-on-learn (IR-0104, Approach A): once a provisional peer's `member.joined`
/// is accepted by the fold, its device is bound to a now-Active identity. Lift the
/// provisional restriction and record the learned identity so subsequent traffic is
/// served as a normal member. A no-op until the join lands.
fn maybe_upgrade_provisional(engine: &SyncEngine, shared: &Arc<Shared>, peer: PeerId) {
    let snapshot = engine.snapshot();
    let device_key = DeviceKey::from_bytes(*peer.as_bytes());
    let Some(identity) = snapshot.identity_of_device(&device_key) else {
        return;
    };
    if !snapshot.is_active(identity) {
        return;
    }
    let Some(device) = endpoint_of(peer) else {
        return;
    };
    shared.clear_provisional(device);
    // Record the now-known identity on the live Connected entry (it was admitted
    // without one). The peer is already in the engine's fan-out set from its
    // `on_connect`, so no re-handshake is needed.
    shared
        .table
        .set(device, PeerConnState::Connected, Some(*identity));
    shared.audit.bootstrap_upgraded(device, identity);
}

/// Advisory wall-clock ms for `on_tick` (the engine treats it as advisory only).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
