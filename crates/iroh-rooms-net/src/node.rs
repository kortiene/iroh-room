//! [`Node`] — a thin runtime that pairs a [`NetTransport`] with the landed,
//! sans-IO [`SyncEngine`] and pumps them (spec OQ-3 "a thin `run()` in the net
//! crate", with the CLI free to drive manually later).
//!
//! The engine is single-owner: one **pump task** owns it and is the only thing
//! that touches it, translating async I/O events into the engine's synchronous
//! entry points and routing the [`Outgoing`]s it returns —
//!
//! * a [`ConnEvent`] reaching `Connected` → `engine.on_connect` (handshake) —
//!   deferred for an unproven provisional peer until its capability proof
//!   verifies or its join lands (issue #121); leaving `Connected` →
//!   `engine.on_disconnect`,
//! * an inbound frame → `engine.ingest_frame`,
//! * a periodic tick → `engine.on_tick` (anti-entropy re-pull),
//! * a [`Cmd`] (publish / query / shutdown) from a [`Node`] handle.
//!
//! Callers interact through the `Node` handle (publish, query the store, observe
//! connection state); the pump keeps the engine accessible without ever sharing a
//! `&mut SyncEngine` across tasks.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::constants::{MAX_SHARED_FILE_BYTES, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{Content, PipeOpened};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::{build_pipe_closed, build_pipe_opened};
use iroh_rooms_core::membership::{
    active_member_warning_crossed, MembershipSnapshot, MAX_ACTIVE_MEMBERS,
};
use iroh_rooms_core::store::StoredEvent;
use iroh_rooms_core::sync::{
    Completeness, Outgoing, PeerId, SyncCounters, SyncEngine, SyncMessage,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::admission::{Admission, AdmissionView};
use crate::audit::AuditSink;
use crate::blob::{self, BlobAclView, BlobError, BlobImport, BlobStore, FetchOutcome};
use crate::manager::PeerManager;
use crate::peer::peer_id;
use crate::pipe::alpn::{PIPE_ALPN, PIPE_ALPN_STR};
use crate::pipe::registry::is_loopback_target;
use crate::pipe::runtime::PipeQueryMsg;
use crate::pipe::{
    connector, new_pipe_id, watcher, PipeAuditSink, PipeDenyCause, PipeError, PipeForwarder,
    PipeHandlerState, PipeProtocolHandler, PipeQuery, PipeRegistry, PipeSessionInfo, PipeSessions,
    TracingPipeAudit,
};
use crate::state::{ConnEvent, PeerConnState, PeerEntry};
use crate::transport::{InboundReceiver, NetConfig, NetTransport, Shared};

/// The extra input a managed room session needs to also serve the blobs it holds
/// over the `iroh-blobs` ALPN (IR-0204 spec §5.3). Opt-in: `None` on
/// [`Node::spawn_room`] keeps the session a pure event-plane member (unchanged
/// behavior); `room tail` is the canonical "provider stays online" surface that
/// supplies this.
#[derive(Debug, Clone)]
pub struct BlobServeConfig {
    /// The durable local blob store directory (`<home>/blobs/`).
    pub blobs_dir: PathBuf,
}

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
    /// The engine's bounded drop/cap log (spec IR-0110 §5.8) — lets a long-running
    /// session (`room tail`) surface per-frame `reject.<code>` entries without a
    /// `tracing` subscriber. See [`Node::logs`].
    Logs(oneshot::Sender<Vec<String>>),
    /// Force an immediate peer-manager reconcile + admission refresh (a test hook;
    /// a no-op for a node with no room session). See [`Node::reconcile_now`].
    Reconcile(oneshot::Sender<()>),
    /// Read the engine's [`SyncCounters`] (issue #142 — lets e2e tests assert the
    /// cached-membership-projection recompute counter across the pump boundary).
    /// Routed through the pump so the engine stays single-owner, like
    /// [`Cmd::Snapshot`] / [`Cmd::Completeness`].
    Counters(oneshot::Sender<SyncCounters>),
    Shutdown(oneshot::Sender<()>),
}

/// The extra inputs a managed room session needs (spec §4.3/§4.4).
struct RoomConfig {
    /// Operator `--peer` addresses, used by the manager to resolve each device.
    addr_hints: Vec<EndpointAddr>,
    /// The live admission cell the pump refreshes each fold change; the session's
    /// admission gate reads it on the accept hot path.
    admission_cell: Arc<Mutex<AdmissionView>>,
    /// Present only when this session also serves blobs (IR-0204 spec §5.3).
    blob: Option<BlobServeConfig>,
}

/// The pump-owned reconciler that keeps the dial set and the admission cell in step
/// with the live membership fold (spec §4.3 — snapshot-diff on the existing tick).
struct RoomReconciler {
    manager: Arc<PeerManager>,
    cell: Arc<Mutex<AdmissionView>>,
    /// The last admission view we reconciled against — the cheap fold-change
    /// detector. `None` forces the next reconcile (initial + `reconcile_now`).
    last: Option<AdmissionView>,
    /// Present only for a blob-serving session; the live cell the blob gate reads
    /// on its accept/request hot path (IR-0204 spec §5.3).
    blob_acl_cell: Option<Arc<Mutex<BlobAclView>>>,
    /// The last referenced-hash set folded into `blob_acl_cell`. Tracked
    /// **independently** of `last` (the admission fold-change detector): a new
    /// `file.shared` is a content event that never changes `AdmissionView`, so
    /// gating this refresh on `last` would starve Gate 2 of newly-shared files
    /// (memory: membership-snapshot equality is vacuous over content events).
    last_referenced: Option<BTreeSet<[u8; 32]>>,
    /// The local audit sink — the seam for the approach-to-ceiling warning
    /// (issue #144). Held on the reconciler because it already observes every
    /// live fold and the warning is independent of the `last` admission-change
    /// detector (a 4 → 4 stay-at-threshold must not re-warn).
    audit: Arc<dyn AuditSink>,
    /// Last observed active-member count — the per-reconciler state that turns
    /// the threshold check into a one-shot-per-crossing signal instead of
    /// per-tick spam. `None` until the first `maybe_reconcile` reads a
    /// snapshot (spec §4 D3 / OQ-1: a node that starts at/above threshold does
    /// **not** emit an initial warning — `room members --status` is the
    /// current-state surface for that).
    last_active_member_count: Option<usize>,
}

impl RoomReconciler {
    /// Reconcile the dial set / admission cell when the membership-relevant
    /// projection of the fold changed, and — independently — the blob ACL cell
    /// (if this session serves blobs) whenever membership OR the referenced-hash
    /// set changed. Idempotent when neither changed. Called after every
    /// fold-mutating pump step and on each tick.
    fn maybe_reconcile(&mut self, engine: &SyncEngine) {
        let snapshot = engine.snapshot();
        // Approach-to-ceiling warning (issue #144): independent of the admission
        // fold-change detector below — a stay-at-threshold room must not re-warn
        // on every tick, but a below-to-at/above transition must warn exactly
        // once per crossing. Tracked against `last_active_member_count` so the
        // signal is one-shot per crossing regardless of admission diff cadence.
        let active = snapshot.active_member_count();
        if active_member_warning_crossed(self.last_active_member_count, active) {
            let max = MAX_ACTIVE_MEMBERS;
            let remaining = max.saturating_sub(active);
            self.audit
                .active_member_threshold_reached(snapshot.room_id(), active, max, remaining);
        }
        self.last_active_member_count = Some(active);

        let view = AdmissionView::from_snapshot(&snapshot, &engine.fail_closed_subjects());
        let membership_changed = self.last.as_ref() != Some(&view);

        if let Some(blob_cell) = &self.blob_acl_cell {
            // Fail-closed on a store read error: an empty referenced set denies
            // every hash rather than risk serving one it can no longer prove.
            let referenced = engine.file_shared_hashes().unwrap_or_default();
            let hashes_changed = self.last_referenced.as_ref() != Some(&referenced);
            if membership_changed || hashes_changed {
                *blob_cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    BlobAclView::from_snapshot(&snapshot, &referenced);
                self.last_referenced = Some(referenced);
            }
        }

        if !membership_changed {
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
        self.last_referenced = None;
        self.maybe_reconcile(engine);
    }
}

/// The join-bootstrap **capability proof** a dialing invitee presents on connect
/// (issue #112): the invite it holds and the secret proving possession. The pump
/// sends it as a [`SyncMessage::ProveCapability`] to every peer it connects to, and
/// the join-hosting admin verifies it before serving the never-windowed membership
/// closure. Held only for the short bootstrap session; the secret carried here is
/// the same one the join later places on the log, so this leaks nothing new.
///
/// `Debug` is hand-redacted — the `capability_secret` must never reach a log.
#[derive(Clone)]
pub struct BootstrapProof {
    /// Room the proof is scoped to.
    pub room_id: RoomId,
    /// The invite id the dialer holds (`member.invited.invite_id`).
    pub invite_id: [u8; SHORT_ID_LEN],
    /// The capability secret proving possession of that invite.
    pub capability_secret: [u8; SHORT_ID_LEN],
}

impl core::fmt::Debug for BootstrapProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BootstrapProof")
            .field("room_id", &self.room_id)
            .field("invite_id", &hex::encode(self.invite_id))
            .field("capability_secret", &"<redacted>")
            .finish()
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
    /// Local API control channel (publish / tail / snapshot / shutdown). The
    /// only producers are local `Node::publish`-style methods (no peer can
    /// push a `Cmd` directly), so this is intentionally **not** on the
    /// network-derived byte-budget path (#141 grep allowlist: local control,
    /// not a peer frame-body path).
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
    /// The durable blob store, present only when this session serves blobs
    /// (IR-0204 spec §5.3). Closed on [`Node::shutdown`] to flush and release its
    /// exclusive on-disk lock.
    blob_store: Option<BlobStore>,
    /// The blob-plane serve gate's decision loop, aborted on [`Node::shutdown`].
    blob_gate: Option<JoinHandle<()>>,
    /// Broadcasts every event the engine accepts (issue #83 / IR-0307). Lives
    /// here (not `Shared`/`PeerTable`) because room events are engine-scoped,
    /// drained where the engine is driven — not a transport/peer concern.
    room_event_tx: broadcast::Sender<StoredEvent>,
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
        Self::spawn_inner(
            secret, admission, audit, engine, cfg, tick, None, None, None,
        )
        .await
    }

    /// Like [`Node::spawn`] but the node presents a join-bootstrap **capability
    /// proof** (issue #112) to every peer it connects to: the invitee proves it
    /// holds the ticket's invite before the join-hosting admin will serve it the
    /// never-windowed membership closure (which since #111 can carry chat ancestry).
    /// The join CLI uses this so an uninvited dialer that merely knows the room id
    /// and admin address cannot pull room history during the `--accept-joins` window.
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    #[allow(clippy::too_many_arguments)] // one wiring seam; each arg is a distinct input
    pub async fn spawn_join_bootstrap(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
        proof: BootstrapProof,
    ) -> Result<Self> {
        Self::spawn_inner(
            secret,
            admission,
            audit,
            engine,
            cfg,
            tick,
            None,
            None,
            Some(proof),
        )
        .await
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
            None,
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
    /// `blob`, when supplied, turns this session into a blob provider (IR-0204
    /// spec §5.3): it opens the durable store at `blob.blobs_dir`, gates the
    /// `iroh-blobs` ALPN with the two-gate ACL sourced from the live fold + the
    /// room's `file.shared` set, and chains it onto the shared `Router`. `None`
    /// keeps this session a pure event-plane member (unchanged behavior) —
    /// `room tail` is the canonical caller that supplies it.
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind, or if `blob` is supplied
    /// and its store directory cannot be opened.
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
        blob: Option<BlobServeConfig>,
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
                blob,
            }),
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one wiring seam; each arg is a distinct input
    async fn spawn_inner(
        secret: SecretKey,
        admission: Arc<dyn Admission>,
        audit: Arc<dyn AuditSink>,
        engine: SyncEngine,
        cfg: NetConfig,
        tick: Duration,
        room: Option<RoomConfig>,
        pipe_audit: Option<Arc<dyn PipeAuditSink>>,
        bootstrap_proof: Option<BootstrapProof>,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        // A dedicated, **bounded** channel for the Pipe plane's reads against
        // the single-owner engine (handler + watcher), drained by the same pump
        // (spec §6.5 / D5). Bounded in #141 so no network-derived pipe-ALPN
        // request can drive an unbounded control queue: the pipe handler can be
        // reached by a remote peer, and its queries enter this channel. Capacity
        // is `NetConfig::pipe_query_capacity` (default `MAX_CONCURRENT_BIDI_STREAMS`).
        let pipe_query_capacity = cfg.pipe_query_capacity.max(1);
        let (pipe_query_tx, pipe_query_rx) = mpsc::channel::<PipeQueryMsg>(pipe_query_capacity);
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

        // The blob-plane serve gate (IR-0204 spec §5.3): opt-in via a managed room
        // session's `BlobServeConfig`. Built before the transport binds so the
        // gated `BlobsProtocol` handler can be chained onto the same router the
        // event/pipe ALPNs share (one Endpoint, many planes).
        let blob_cfg = room.as_ref().and_then(|r| r.blob.clone());
        let (blobs_handler, blob_store, blob_gate, blob_acl_cell) = match blob_cfg {
            Some(cfg) => {
                let store = BlobStore::open(&cfg.blobs_dir).await.map_err(|e| {
                    anyhow!(
                        "could not open the blob store at {}: {e}",
                        cfg.blobs_dir.display()
                    )
                })?;
                let acl_cell = Arc::new(Mutex::new(BlobAclView::empty()));
                let (events, gate) = blob::spawn_blob_gate(acl_cell.clone(), audit.clone());
                let handler = store.serve_handler(events);
                (Some(handler), Some(store), Some(gate), Some(acl_cell))
            }
            None => (None, None, None, None),
        };

        // Created before `cfg` is consumed by `bind` below (issue #83 / IR-0307).
        let (room_event_tx, _) = broadcast::channel::<StoredEvent>(cfg.room_event_capacity);

        let mut transport = NetTransport::bind(
            secret,
            admission,
            audit,
            cfg,
            Some(pipe_handler),
            blobs_handler,
        )
        .await?;
        let inbound_rx = transport
            .take_inbound()
            .ok_or_else(|| anyhow!("inbound receiver already taken"))?;
        let conn_rx = transport.conn_events();
        let shared = transport.shared();

        // Build the room-scoped manager + admission-refresh reconciler when this is a
        // managed session. The manager is moved (as a clone) into the pump so the
        // single-owner engine drives reconciliation; `Node` keeps a handle to abort
        // its dial loops on shutdown. `audit` is cloned here (before it is moved
        // into `NetTransport::bind` further below) so the reconciler can emit the
        // approach-to-ceiling warning (issue #144) through the same sink the
        // accept path uses.
        let (peer_manager, room_reconciler) = match room {
            Some(RoomConfig {
                addr_hints,
                admission_cell,
                ..
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
                    blob_acl_cell,
                    last_referenced: None,
                    audit: transport.shared().audit.clone(),
                    last_active_member_count: None,
                };
                (Some(manager), Some(reconciler))
            }
            None => (None, None),
        };

        // The per-room gossip mesh (issue #171 / spec §4 D1/D5/Step 2). When
        // the overlay feature is on AND this is a managed room session, spawn
        // the mesh for the engine's `room_id`, bootstrapping from the
        // deterministic seed set the manager computes from the current
        // membership snapshot (D3). The mesh installs itself into
        // `Shared.gossip_state.meshes`, so `Shared::route`'s `Events` branch
        // can find it for broadcast.
        //
        // The mesh spawns in a **background task** (issue #171): `GossipMesh::spawn`
        // awaits `receiver.joined()` under a 5s timeout when bootstrap seeds are
        // present, and loopback endpoints lack the address discovery the dialer
        // needs — so the join waits the full timeout. Awaiting it inline blocked
        // `Node::start` for up to 5s per joining node (~54x slowdown on the e2e
        // suite under `--features gossip_overlay`). Backgrounding it makes
        // `Node::start` return immediately: the mesh installs itself once formed,
        // and `Events` broadcasts issued before then hit `mesh_for == None` and are
        // silently dropped — the documented fallback (same shape as no-mesh / a peer
        // with no live writer), recovered by anti-entropy on the next tick.
        //
        // Failure to spawn the mesh is **non-fatal**: the events plane falls
        // back to silent-drop for `Events` (the same shape as a peer with no
        // live writer), and pull/query variants continue unchanged on the
        // per-peer queue path. Anti-entropy pulls recover any dropped frame on
        // the next tick. We log the error rather than failing the whole node
        // — a partial degradation (events plane down, pulls still working) is
        // more recoverable than a startup crash.
        #[cfg(feature = "gossip_overlay")]
        if peer_manager.is_some() {
            let room_id = *engine.room_id();
            let snapshot = engine.snapshot();
            let bootstrap = PeerManager::desired_seeds(&snapshot, transport.id())
                .into_iter()
                .collect();
            if let Some(actor) = shared.gossip_state.actor().cloned() {
                let shared_for_mesh = shared.clone();
                tokio::spawn(async move {
                    match crate::gossip::GossipMesh::spawn(
                        shared_for_mesh.clone(),
                        actor,
                        room_id,
                        bootstrap,
                    )
                    .await
                    {
                        Ok(mesh) => {
                            shared_for_mesh.gossip_state.install_mesh(room_id, mesh);
                        }
                        Err(err) => {
                            tracing::warn!(
                                reason = "gossip.mesh.spawn_failed",
                                room = %room_id,
                                error = %err,
                                "could not spawn the room events gossip mesh; Events frames will be silently dropped until the next retry"
                            );
                        }
                    }
                });
            }
        }

        let pump = tokio::spawn(pump(
            engine,
            inbound_rx,
            conn_rx,
            shared,
            cmd_rx,
            pipe_query_rx,
            tick,
            room_reconciler,
            room_event_tx.clone(),
            bootstrap_proof,
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
            blob_store,
            blob_gate,
            room_event_tx,
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

    #[must_use]
    pub fn outbound_queue_depths(&self) -> Vec<(EndpointId, usize)> {
        self.transport.outbound_queue_depths()
    }

    /// The total gossip-neighbor count across every per-room mesh this node has
    /// subscribed (issue #171 / spec §5.4). Zero when the `gossip_overlay`
    /// feature is off, no room session is active, or the swarm has not yet
    /// formed a direct neighbor link. Diagnostic only — meant for the CLI's
    /// `room members --status` panel and the spike-N40 harness.
    #[must_use]
    pub fn gossip_neighbor_count(&self) -> usize {
        self.transport.gossip_neighbor_count()
    }

    /// Subscribe to the live [`ConnEvent`] transition stream.
    #[must_use]
    pub fn conn_events(&self) -> broadcast::Receiver<ConnEvent> {
        self.transport.conn_events()
    }

    /// Subscribe to the live stream of events accepted into this room's store —
    /// every event validated + inserted via local publish OR remote sync, emitted
    /// exactly once after insert (issue #83 / IR-0307).
    ///
    /// # Semantics
    /// - **Exactly once per stored event.** A duplicate re-see (same `event_id`)
    ///   is idempotent and never re-emitted.
    /// - **Lossy on lag.** This is a bounded `broadcast` (capacity
    ///   `NetConfig::room_event_capacity`, default 256). A subscriber that falls
    ///   behind receives `RecvError::Lagged(n)` and MUST resync — the events it
    ///   missed are gone from this channel.
    /// - **Not ordered by Lamport.** Emission order follows insertion order at the
    ///   engine choke point. A park-promotion cascade emits the directly-accepted
    ///   trigger first, then its promoted descendants in engine-iteration order —
    ///   NOT causal order. Use `StoredEvent.lamport` if you need a total order.
    ///
    /// # Reconcile recipe (on `Lagged`)
    /// ```ignore
    /// let mut rx = node.room_events();
    /// let mut seen = HashSet::new();
    /// loop {
    ///     match rx.recv().await {
    ///         Ok(ev) => { if seen.insert(ev.event_id) { handle(ev); } }
    ///         Err(RecvError::Lagged(_)) => {
    ///             // Rebuild from the authoritative tail, dedupe against `seen`.
    ///             for ev in node.room_tail(u32::MAX).await? {
    ///                 if seen.insert(ev.event_id) { handle(ev); }
    ///             }
    ///         }
    ///         Err(RecvError::Closed) => break,
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn room_events(&self) -> broadcast::Receiver<StoredEvent> {
        self.room_event_tx.subscribe()
    }

    /// Per-peer live path classification (direct/relay/mixed/none) + relay url (spec
    /// IR-0303 §5.3) — the data behind `room members --status --verbose` / `room
    /// tail --verbose`'s `diag:` block. Diagnostic only, off any hot path.
    pub async fn peer_paths(&self) -> Vec<(EndpointId, crate::diag::PathType, Option<String>)> {
        self.transport.peer_paths().await
    }

    /// This node's home relay url, if any (spec IR-0303 §5.3).
    #[must_use]
    pub fn relay_url(&self) -> Option<String> {
        self.transport.relay_url()
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

    /// The engine's bounded drop/cap log (spec IR-0110 §5.8/§4.4): a running list of
    /// `reject.<code>` / `flag.<code>` entries a long-running session (`room tail`)
    /// can poll to surface the specific per-frame reject/flag code without a
    /// `tracing` subscriber (project memory: *CLI has no tracing subscriber*).
    ///
    /// # Errors
    /// Returns an error if the pump is gone.
    pub async fn logs(&self) -> Result<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Logs(tx))
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

    /// The engine's current [`SyncCounters`] — the Gate-D evidence memo, including
    /// `membership_projection_recomputes` (issue #142). Routed through the pump so
    /// the engine stays single-owner; lets e2e tests assert the cached-membership-
    /// projection recompute behavior across the real transport boundary (the same
    /// way [`Node::snapshot`] / [`Node::completeness`] expose other engine state).
    ///
    /// # Errors
    /// Returns an error if the pump is gone.
    pub async fn counters(&self) -> Result<SyncCounters> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Counters(tx))
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
        // `PipeError` (not collapsed to a string) is preserved on the error path so
        // a caller can `downcast_ref::<PipeError>()` and distinguish, e.g.,
        // `OwnerUnreachable` (offline owner, spec IR-0110 §5.5) from other setup
        // faults.
        let endpoint = self.transport.endpoint();
        let forwarder =
            connector::connect(&endpoint, owner_addr, pipe_id, PIPE_ALPN, local_port).await?;
        Ok(forwarder)
    }

    /// A clone of the underlying iroh [`Endpoint`] (for an out-of-band pipe dial in
    /// tests / tooling that drive the connector directly).
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.transport.endpoint()
    }

    /// The number of live pipe sessions currently being forwarded (observability /
    /// tests for the teardown path). For a per-pipe count, see
    /// [`Node::live_pipe_sessions_for`].
    #[must_use]
    pub fn live_pipe_sessions(&self) -> usize {
        self.pipe_sessions.len()
    }

    /// Count of live forwarding sessions for one exposed `pipe_id` (issue #86).
    ///
    /// Unlike [`Node::live_pipe_sessions`] (node-wide across every exposed pipe),
    /// this attributes sessions to a single pipe, so an owner exposing more than one
    /// pipe can render an accurate per-pipe "connected" indicator. `0` for an
    /// unknown / never-connected pipe.
    #[must_use]
    pub fn live_pipe_sessions_for(&self, pipe_id: [u8; 16]) -> usize {
        self.pipe_sessions.count_for(&pipe_id)
    }

    /// Per-session detail for every live forwarding session this node owns (issue
    /// #86): `(pipe_id, connecting device, since)`. A point-in-time snapshot in
    /// unspecified order — sort by `pipe_id`/`since_ms` for display. Resolve
    /// `device` to a member identity via [`Node::snapshot`] if needed.
    #[must_use]
    pub fn pipe_session_info(&self) -> Vec<PipeSessionInfo> {
        self.pipe_sessions.info()
    }

    /// **Fetch** `hash` from `provider_addr` over the blobs ALPN on this node's
    /// shared endpoint, requiring the assembled bytes' independent BLAKE3-256
    /// equal `declared` (IR-0204 spec §5.4). Bounded by `timeout`; an offline or
    /// denying provider yields a non-[`Fetched`](FetchOutcome::Fetched) outcome,
    /// never a hang. A pure consumer call — this node need not itself serve blobs.
    pub async fn fetch_file(
        &self,
        provider_addr: EndpointAddr,
        hash: [u8; 32],
        declared: [u8; 32],
        timeout: Duration,
    ) -> (FetchOutcome, Option<Bytes>) {
        self.fetch_file_sized(
            provider_addr,
            hash,
            declared,
            MAX_SHARED_FILE_BYTES,
            timeout,
        )
        .await
    }

    /// Fetch a blob while refusing to buffer more than `max_bytes`. CLI callers
    /// pass the signed `file.shared.size_bytes`; generic SDK callers use
    /// [`Node::fetch_file`], which applies the protocol-wide 100 MiB ceiling.
    pub async fn fetch_file_sized(
        &self,
        provider_addr: EndpointAddr,
        hash: [u8; 32],
        declared: [u8; 32],
        max_bytes: u64,
        timeout: Duration,
    ) -> (FetchOutcome, Option<Bytes>) {
        blob::fetch_blob_sized(
            &self.transport.endpoint(),
            provider_addr,
            hash,
            declared,
            max_bytes.min(MAX_SHARED_FILE_BYTES),
            timeout,
        )
        .await
    }

    /// Import a file into the durable store this session already owns (issue #84 /
    /// IR-0308), returning the verified content ref. Unlike the CLI's
    /// open→import→close, this reuses the live session's store handle: no second
    /// `FsStore` open (so no [`BlobError::Locked`]) and **no session cycle** — the
    /// endpoint, engine pump, and every peer link stay up (zero `ConnEvent` churn).
    ///
    /// Pair with `build_file_shared` + [`Node::publish`] to announce the reference;
    /// the serve ACL's Gate 2 begins allowing the new hash on the next fold drive.
    /// Import **before** publish — publishing first would briefly reference a hash
    /// the store doesn't hold yet.
    ///
    /// # Errors
    /// [`BlobError::NotServing`] if this session opened no blob store (spawned
    /// without a `BlobServeConfig`); otherwise the `import_path` errors (`Import`,
    /// `Read`, `HashMismatch`).
    pub async fn blob_import(&self, path: &Path) -> Result<BlobImport, BlobError> {
        self.blob_store
            .as_ref()
            .ok_or(BlobError::NotServing)?
            .import_path(path)
            .await
    }

    /// [`Node::blob_import`] from in-memory bytes — re-provide fetched bytes
    /// in-session so a long-running consumer becomes a provider without restarting.
    /// After a `fetch_file` that returned `(FetchOutcome::Fetched, Some(bytes))`, the
    /// bytes' hash is already the one the `file.shared` references, so once imported
    /// this node serves it immediately (Gate 2 already allows the referenced hash;
    /// no new `file.shared`, no reconcile needed).
    ///
    /// # Errors
    /// [`BlobError::NotServing`] if this session opened no blob store; otherwise the
    /// `import_bytes` errors (`Import`, `HashMismatch`).
    pub async fn blob_import_bytes(&self, bytes: Bytes) -> Result<BlobImport, BlobError> {
        self.blob_store
            .as_ref()
            .ok_or(BlobError::NotServing)?
            .import_bytes(bytes)
            .await
    }

    /// Gracefully stop: drain the pump, stop the pipe watcher, stop serving blobs
    /// (if this session did), and shut the transport's router down.
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
            blob_store,
            blob_gate,
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
        if let Some(gate) = blob_gate {
            gate.abort();
        }
        // `NetTransport::shutdown` calls `Router::shutdown`, which awaits every
        // registered `ProtocolHandler::shutdown` — including `BlobsProtocol`'s,
        // which already flushes and releases the store's exclusive on-disk lock
        // (iroh_blobs `net_protocol::BlobsProtocol::shutdown` calls
        // `store.shutdown()`) before the router closes the endpoint. So the
        // store is already durably closed by the time this returns; do NOT also
        // call `BlobStore::close` here — the store's actor is already gone by
        // then, so a second shutdown fails with a stale-channel error. Just drop
        // the handle.
        transport.shutdown().await?;
        drop(blob_store);
        Ok(())
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
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one wiring seam; each channel/handle is distinct
async fn pump(
    mut engine: SyncEngine,
    mut inbound_rx: InboundReceiver,
    mut conn_rx: broadcast::Receiver<ConnEvent>,
    shared: Arc<Shared>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    mut pipe_query_rx: mpsc::Receiver<PipeQueryMsg>,
    tick: Duration,
    mut room: Option<RoomReconciler>,
    room_event_tx: broadcast::Sender<StoredEvent>,
    bootstrap_proof: Option<BootstrapProof>,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Outbound gate (issue #121): provisional peers whose engine handshake is
    // deferred because they have not yet proven invite possession. A peer in this
    // set never enters the engine's fan-out set, so `store_and_fanout` pushes
    // nothing to it and `on_tick` never advertises the admin tip / heads to it.
    // The deferred `on_connect` runs when its capability proof verifies (below)
    // or when its accepted join upgrades it to a member
    // ([`maybe_upgrade_provisional`]); the entry is dropped on disconnect.
    let mut deferred_handshakes: BTreeSet<PeerId> = BTreeSet::new();

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
                if handle_cmd(&mut engine, &shared, &mut room, cmd, &room_event_tx) {
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
                        // *provisional* peer is served the membership sub-DAG only,
                        // and — since #111 lets that closure carry chat ancestry —
                        // only once it has proven invite possession (issue #112).
                        let device = endpoint_of(inbound.peer);
                        let provisional = device.is_some_and(|d| shared.is_provisional(d));
                        let serve = if provisional {
                            match &msg {
                                // The capability proof is a transport handshake step:
                                // verify it, flip the proven mark, and never forward it
                                // to the deterministic engine (issue #112).
                                SyncMessage::ProveCapability {
                                    room_id,
                                    invite_id,
                                    capability_secret,
                                } => {
                                    if let Some(d) = device {
                                        let ok = room_id == engine.room_id()
                                            && engine.capability_proof_matches(
                                                invite_id,
                                                capability_secret,
                                            );
                                        if ok {
                                            shared.mark_capability_proven(d);
                                            shared.audit.bootstrap_capability_proven(d);
                                            // Deferred handshake (issue #121): if this
                                            // peer reached `Connected` before its proof
                                            // verified, run the engine handshake now —
                                            // only from this point may it receive
                                            // fan-out and advertisements.
                                            if deferred_handshakes.remove(&inbound.peer) {
                                                let outs = engine.on_connect(inbound.peer);
                                                route_all(&shared, outs);
                                            }
                                        } else {
                                            shared.audit.bootstrap_capability_rejected(d);
                                        }
                                    }
                                    false
                                }
                                other => {
                                    let proven = device
                                        .is_some_and(|d| shared.is_capability_proven(d));
                                    let allowed = provisional_allows(other, proven);
                                    if !allowed {
                                        if let Some(d) = device {
                                            shared
                                                .audit
                                                .bootstrap_blocked(d, sync_message_kind(other));
                                        }
                                    }
                                    allowed
                                }
                            }
                        } else {
                            true
                        };
                        if serve {
                            // AC3 observability (spec §D8): surface an engine reject
                            // of this inbound frame through the AuditSink so it is
                            // visible without a tracing subscriber. The reject count
                            // is the delta in the engine's monotonic counter; the
                            // per-frame `reject.<code>` detail is in engine.logs().
                            let rejected_before = engine.counters().rejected;
                            let outs = engine.on_message(inbound.peer, msg);
                            route_all(&shared, outs);
                            let rejected = engine
                                .counters()
                                .rejected
                                .saturating_sub(rejected_before);
                            if rejected > 0 {
                                if let Some(d) = device {
                                    shared.audit.event_rejected(d, rejected);
                                }
                            }
                            // Advisory flags on any events this message accepted
                            // (spec IR-0110 §5.9, e.g. `clock_skew`) — surfaced
                            // per-code, distinct from a rejection.
                            let flags = engine.take_flags();
                            if let Some(d) = device {
                                for code in flags {
                                    shared.audit.event_flagged(d, code);
                                }
                            }
                            // Push-subscription feed (issue #83): peer-sync + any
                            // park-promotion this message triggered.
                            drain_room_events(&mut engine, &room_event_tx);
                            if provisional {
                                // Upgrade-on-learn: if that frame was the join the
                                // fold accepted, the peer is now an Active member —
                                // lift the restriction and record its identity.
                                maybe_upgrade_provisional(
                                    &mut engine,
                                    &shared,
                                    inbound.peer,
                                    &mut deferred_handshakes,
                                );
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
                    Ok(event) => {
                        handle_conn_event(
                            &mut engine,
                            &shared,
                            event,
                            bootstrap_proof.as_ref(),
                            &mut deferred_handshakes,
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "pump: conn-event stream lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {}
                }
            }
            _ = ticker.tick() => {
                let outs = engine.on_tick(now_ms());
                route_all(&shared, outs);
                // Push-subscription feed (issue #83): `on_tick` also drives
                // `wake_park`, so park-promotions surface here.
                drain_room_events(&mut engine, &room_event_tx);
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
    room_event_tx: &broadcast::Sender<StoredEvent>,
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
            // Push-subscription feed (issue #83): own publish + any park-promotion
            // it triggered.
            drain_room_events(engine, room_event_tx);
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
        Cmd::Logs(reply) => {
            let _ = reply.send(engine.logs().to_vec());
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
        Cmd::Counters(reply) => {
            let _ = reply.send(engine.counters());
            false
        }
        Cmd::Shutdown(reply) => {
            let _ = reply.send(());
            true
        }
    }
}

/// Fan out every event the engine accepted since the last drain onto the
/// `room_events` broadcast (issue #83 / IR-0307). A `send` error means no
/// live subscriber — expected and ignored; a lagging subscriber is dropped
/// frames on the receiver, not an error here.
fn drain_room_events(engine: &mut SyncEngine, tx: &broadcast::Sender<StoredEvent>) {
    for ev in engine.take_ingested() {
        let _ = tx.send(ev);
    }
}

/// Translate a connection-state transition into the engine's link entry points.
///
/// For an **unproven provisional** peer the `Connected` handshake is deferred
/// (issue #121): `engine.on_connect` would insert it into the fan-out set, so every
/// event accepted while it stays connected — including live chat published during
/// an open `--accept-joins` window — would be pushed to it, and every tick would
/// advertise the admin tip and heads to it. The peer is parked in `deferred`
/// instead; the handshake runs when its capability proof verifies (the pump's
/// `ProveCapability` arm) or when its accepted join promotes it to a member
/// ([`maybe_upgrade_provisional`]). The proof may be processed *before* this
/// `Connected` transition (the pump's inbound and conn-event channels are not
/// ordered relative to each other): then the proven mark is already set and the
/// handshake runs here, immediately.
fn handle_conn_event(
    engine: &mut SyncEngine,
    shared: &Arc<Shared>,
    event: ConnEvent,
    bootstrap_proof: Option<&BootstrapProof>,
    deferred: &mut BTreeSet<PeerId>,
) {
    let peer = peer_id(event.device);
    match event.to {
        PeerConnState::Connected => {
            // A dialing invitee presents its capability proof (issue #112) *before*
            // the engine's `on_connect` pull, so the admin marks it proven before it
            // processes the `WantMembership` that follows — frames are ordered per
            // link, so the closure serve is never blocked for a genuine joiner.
            if let Some(proof) = bootstrap_proof {
                shared.route(&Outgoing {
                    peer,
                    msg: SyncMessage::ProveCapability {
                        room_id: proof.room_id,
                        invite_id: proof.invite_id,
                        capability_secret: proof.capability_secret,
                    },
                });
            }
            if shared.is_provisional(event.device) && !shared.is_capability_proven(event.device) {
                deferred.insert(peer);
            } else {
                let outs = engine.on_connect(peer);
                route_all(shared, outs);
            }
        }
        PeerConnState::Offline | PeerConnState::Unauthorized => {
            // A deferred handshake dies with the link (a re-connect re-proves).
            deferred.remove(&peer);
            // Only a transition *away from* a live link is a real disconnect; a
            // first-sight Offline/Unauthorized (we never connected) is a no-op.
            // (`on_disconnect` is a plain set-remove, so it is safe for a peer
            // whose handshake was still deferred.)
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
/// `msg`, given whether it has proven invite possession (`capability_proven`, issue
/// #112).
///
/// It may push its `member.joined` ([`Events`], which the fold judges) and the
/// harmless tip/head/not-found advertisements at any time. It may pull the
/// never-windowed membership sub-DAG ([`WantMembership`]) **only after proving
/// capability** — since PR #111 that closure can carry the chat that entered the
/// membership ancestry, so an *unproven* dialer must not receive it. It may **never**
/// pull chat or arbitrary events ([`WantRecentChat`] / [`WantEvents`]) — those would
/// serve room content to a not-yet-member, the privacy regression the spec scopes
/// out. [`ProveCapability`] is intercepted before this gate and never reaches it.
///
/// [`WantMembership`]: SyncMessage::WantMembership
/// [`Events`]: SyncMessage::Events
/// [`WantRecentChat`]: SyncMessage::WantRecentChat
/// [`WantEvents`]: SyncMessage::WantEvents
/// [`ProveCapability`]: SyncMessage::ProveCapability
fn provisional_allows(msg: &SyncMessage, capability_proven: bool) -> bool {
    match msg {
        // The membership closure is served only to a capability-proven peer (#112).
        SyncMessage::WantMembership { .. } => capability_proven,
        // The join push and the harmless advertisements are always allowed.
        SyncMessage::Events { .. }
        | SyncMessage::AdminTip { .. }
        | SyncMessage::Heads { .. }
        | SyncMessage::NotFound { .. } => true,
        // Chat/arbitrary pulls, and any other kind, are refused.
        _ => false,
    }
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
fn maybe_upgrade_provisional(
    engine: &mut SyncEngine,
    shared: &Arc<Shared>,
    peer: PeerId,
    deferred: &mut BTreeSet<PeerId>,
) {
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
    // without one).
    shared
        .table
        .set(device, PeerConnState::Connected, Some(*identity));
    shared.audit.bootstrap_upgraded(device, identity);
    // If the peer's handshake was deferred (issue #121: its join was accepted
    // without a verified capability proof — e.g. a device that already held the
    // membership DAG pushed a valid `member.joined` directly), run it now so the
    // new member enters the fan-out set.
    if deferred.remove(&peer) {
        let outs = engine.on_connect(peer);
        route_all(shared, outs);
    }
}

/// Advisory wall-clock ms for `on_tick` (the engine treats it as advisory only).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Tests for the private [`drain_room_events`] helper (issue #83 / IR-0307) — a
/// real [`SyncEngine`] over `open_in_memory`, but a bare `broadcast::channel`, no
/// endpoint/QUIC/tokio runtime: `try_recv` is sync, so these run as plain `#[test]`.
#[cfg(test)]
mod room_events_pump_tests {
    use iroh_rooms_core::event::ids::{EventId, RoomId};
    use iroh_rooms_core::store::EventStore;
    use iroh_rooms_core::sync::SyncConfig;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::{drain_room_events, StoredEvent, SyncEngine};
    use crate::demo;

    /// A fresh engine seeded with only genesis, plus the host/room/genesis-id
    /// needed to author further admin messages. Lets a test publish in distinct
    /// phases (drain, subscribe, publish more, drain) so the live-tap contract of
    /// `Node::room_events` can be exercised without any network.
    fn engine_and_author() -> (SyncEngine, demo::Participant, RoomId, EventId) {
        let host = demo::Participant::new(0x01);
        let (room, genesis_id, genesis_wire) = demo::genesis(&host);
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        engine.publish(&genesis_wire).expect("publish genesis");
        (engine, host, room, genesis_id)
    }

    /// A fresh engine seeded with a genesis + `n` admin-authored chat messages
    /// (each a distinct sibling of genesis, so every one is directly `Accepted` —
    /// no park/promotion needed to exercise the drain).
    fn engine_with_chain(n: u8) -> SyncEngine {
        let (mut engine, host, room, genesis_id) = engine_and_author();
        for i in 0..n {
            let (_, wire) = demo::admin_message(&host, room, genesis_id, i);
            engine.publish(&wire).expect("publish admin message");
        }
        engine
    }

    #[test]
    fn drain_forwards_in_order_and_is_destructive() {
        let mut engine = engine_with_chain(3);
        let (tx, mut rx) = tokio::sync::broadcast::channel::<StoredEvent>(16);

        drain_room_events(&mut engine, &tx);

        // genesis + 3 admin messages, in acceptance order.
        let mut received = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            received.push(ev);
        }
        assert_eq!(received.len(), 4, "genesis + 3 chat messages, each once");

        // A second drain with nothing newly accepted must forward nothing.
        drain_room_events(&mut engine, &tx);
        assert_eq!(
            rx.try_recv(),
            Err(TryRecvError::Empty),
            "a second drain must be empty (destructive take)"
        );
    }

    #[test]
    fn drain_no_subscriber_does_not_panic() {
        let mut engine = engine_with_chain(1);
        let (tx, rx) = tokio::sync::broadcast::channel::<StoredEvent>(16);
        drop(rx);

        // `tx.send` errors with no live receiver; must be silently ignored (R7).
        drain_room_events(&mut engine, &tx);
    }

    #[test]
    fn drain_lagged_then_recovers() {
        // genesis + 4 admin messages = 5 accepted events.
        let mut engine = engine_with_chain(4);
        let (tx, mut rx) = tokio::sync::broadcast::channel::<StoredEvent>(2);

        drain_room_events(&mut engine, &tx);

        // tokio broadcast does not round capacity: with cap 2 and 5 sent while
        // subscribed from the start, the receiver lags by exactly 3.
        match rx.try_recv() {
            Err(TryRecvError::Lagged(n)) => assert_eq!(n, 3, "exact lag count, not rounded"),
            other => panic!("expected Lagged(3), got {other:?}"),
        }
        // The two most recent events are still there to recover.
        assert!(rx.try_recv().is_ok(), "first surviving event after lag");
        assert!(rx.try_recv().is_ok(), "second surviving event after lag");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn drain_does_not_replay_to_a_late_subscriber() {
        // Live-tap contract (`Node::room_events` doc): a subscriber receives only
        // events drained *after* it subscribes. Events fanned out before it
        // subscribed are gone from this channel — the reason a `Lagged` consumer
        // must rebuild from `room_tail`, and why the e2e subscribes before the
        // first publish. Proven here with no network.
        let (mut engine, host, room, genesis_id) = engine_and_author();
        let (tx, _) = tokio::sync::broadcast::channel::<StoredEvent>(16);

        // Genesis is drained with no live subscriber — discarded, not buffered.
        drain_room_events(&mut engine, &tx);

        // Subscribe, then author + accept one more event and drain it.
        let mut late = tx.subscribe();
        let (msg_id, wire) = demo::admin_message(&host, room, genesis_id, 0);
        engine
            .publish(&wire)
            .expect("publish post-subscribe message");
        drain_room_events(&mut engine, &tx);

        let ev = late
            .try_recv()
            .expect("late subscriber must receive the post-subscribe event");
        assert_eq!(
            ev.event_id, msg_id,
            "a late subscriber sees only events drained after it subscribed"
        );
        assert_eq!(
            late.try_recv(),
            Err(TryRecvError::Empty),
            "genesis (drained before subscribe) must never be replayed"
        );
    }

    #[test]
    fn drain_fans_out_to_every_live_subscriber() {
        // A daemon may open several `room_events()` receivers (e.g. one per UI
        // client); a single drain must deliver its own copy of every event to
        // each live subscriber.
        let mut engine = engine_with_chain(2); // genesis + 2 messages = 3 events
        let (tx, mut rx1) = tokio::sync::broadcast::channel::<StoredEvent>(16);
        let mut rx2 = tx.subscribe();

        drain_room_events(&mut engine, &tx);

        for (label, rx) in [("rx1", &mut rx1), ("rx2", &mut rx2)] {
            let mut n = 0;
            while rx.try_recv().is_ok() {
                n += 1;
            }
            assert_eq!(
                n, 3,
                "{label} must receive its own copy of all three events"
            );
        }
    }
}
