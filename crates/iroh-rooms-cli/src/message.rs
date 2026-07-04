//! Signed text messaging: the orchestration behind
//! `iroh-rooms room send <ROOM_ID> <MESSAGE>` and
//! `iroh-rooms room tail <ROOM_ID>` (spec IR-0105 D3–D10).
//!
//! These are the first **online** CLI commands — the first that leave the local
//! filesystem and talk to another machine over the landed full-mesh QUIC carrier
//! ([`iroh_rooms_net`]). They stay thin orchestrators over landed primitives, the
//! siblings of [`crate::room`] and [`crate::invite`]:
//!
//! * `send` is **offline-first, online-best-effort** (D3): it builds + self-checks
//!   the `message.text`, then — when the room has other active members — brings up
//!   an ephemeral [`Node`], dials them, and lets the engine's `publish` persist and
//!   fan the frame out to connected peers. The frame is **always** persisted
//!   locally (the guaranteed core); live delivery is best-effort with no queue and
//!   no guaranteed offline delivery (PRD §14).
//! * `tail` is the long-running receiver/session (D4): it brings up a [`Node`],
//!   accepts inbound `message.text` frames (validated/deduped/persisted by the
//!   landed engine), and renders the timeline in canonical `(lamport, event_id)`
//!   order until interrupted.
//!
//! No new crypto and no new validation rule: the message-correctness acceptance
//! criteria (signed-by-device-key, duplicate-ignored, invalid-signature-rejected,
//! non-member-rejected, deterministic-order) are all satisfied by the
//! conformance-tested core/engine this module drives (spec §12.1).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::constants::{MAX_MESSAGE_BODY_BYTES, MAX_PREV_EVENTS, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey};
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::{Ingest, MembershipSnapshot, Role, RoomMembership, Status};
// The offline authoring half of `send`/`send_agent_status` goes through the SDK
// façade (spec IR-0301 §5.4); the online engine/transport imports above stay
// direct `core`/`net` deps (the optional online-path migration).
use iroh_rooms::events::{build_agent_status, build_message_text};
use iroh_rooms_core::store::{EventStore, StoredEvent};
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    Admission, AdmissionView, AllowlistAdmission, BlobServeConfig, ConnEvent,
    JoinBootstrapAdmission, NetConfig, NetMode, Node, PathType, PeerConnState, PeerEntry,
    PeerManager, SnapshotAdmission, DEFAULT_TICK,
};

use crate::display::{self, display_names, iso8601_utc, short_id};
use crate::error::CodedResultExt;
use crate::{audit, clock, identity};

/// The single event-store database file under the data-directory home (spec D3).
pub(crate) const DB_FILE: &str = "rooms.db";
/// Accepted `--format` values (spec §5; the §7 content enum). `None` ⇒ omit
/// (defaults to `plain` on read).
const MESSAGE_FORMATS: &[&str] = &["plain", "markdown"];
/// Default historical rows rendered by `room tail` on startup (spec §6).
pub const DEFAULT_TAIL_LIMIT: u32 = 200;
/// Default best-effort connect timeout for `room send` (spec §5/§7).
pub const DEFAULT_SEND_TIMEOUT: &str = "5s";
/// Poll interval for the `room tail` display loop (spec D6; ≈200 ms is negligible
/// against a single `SQLite` reader, §8).
const TAIL_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Grace period after `publish` so the per-peer writer queues flush before the
/// ephemeral `send` node tears down (spec §5 step 5 / R3).
const FLUSH_GRACE: Duration = Duration::from_millis(300);

/// The result of a `room send`, for the caller to present.
pub struct SendSummary {
    /// The authored message's event id.
    pub event_id: EventId,
    /// The room the message belongs to.
    pub room_id: RoomId,
    /// The author's identity (`sender_id`).
    pub sender_id: IdentityKey,
    /// Number of connected peers the frame was pushed to (possibly zero).
    pub delivered: usize,
    /// Number of active peers we tried to reach.
    pub attempted: usize,
}

/// Send a signed `message.text` to `room_id`: build it, self-validate, persist it
/// locally (the guarantee), then best-effort push it to connected peers (D3).
///
/// # Errors
/// Fails — leaving the store untouched on every pre-persist path — if the body or
/// any option is invalid (validated before any IO), if no local identity exists, if
/// the room is unknown, if the caller is not an active member, on a store error, or
/// — as an internal-bug guard — if the freshly built message fails self-validation.
/// A failure to reach peers is **not** an error (availability model): it is
/// reported, exit 0.
#[allow(clippy::too_many_arguments)] // one linear orchestration; each arg is a distinct CLI input
#[allow(clippy::too_many_lines)] // a single offline-then-online flow; splitting hurts readability
pub async fn send(
    home: &Path,
    room_id: &RoomId,
    body: &str,
    format: Option<&str>,
    reply_to: Option<&str>,
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<SendSummary> {
    // ---- Pre-IO argument validation (a bad invocation writes nothing). ----
    validate_body(body)?;
    let format = validate_format(format).coded(crate::error::ErrorCode::InvalidArgument)?;
    let in_reply_to = reply_to.map(parse_event_id).transpose()?;
    let peer_addrs = parse_peers(peers)?;

    // Load the signing secrets (also re-checks them against the public profile).
    let secret = identity::SecretKeys::load(home)?;
    let sender_id = secret.identity.identity_key();

    // ---- Fold the persisted log: confirm the room exists and we are Active. ----
    let db_path = home.join(DB_FILE);
    let mut store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (mut membership, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&sender_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
            "you are not an active member of room {room_id}; only an active member can send \
             messages (this identity is {sender_id})"
        );
    }

    // ---- prev_events = current room heads, bounded per §6 (D8). ----
    let heads = select_heads(&store, room_id)?;

    // ---- Build + self-validate. We do NOT persist here: the engine's `publish`
    // path persists (InsertOutcome::Inserted) and fans out, and a duplicate insert
    // would suppress that fan-out. A final guaranteed insert below covers the case
    // where the live push never runs (spec §4.1 / D9). ----
    let created_at = clock::now_ms();
    let wire = build_message_text(
        &secret.identity,
        &secret.device,
        room_id,
        body,
        format,
        in_reply_to,
        &[],
        &heads,
        created_at,
    );
    let wire_bytes = wire.to_bytes();
    let ctx = ValidationContext::for_room(*room_id);
    let validated = validate_wire_bytes(&wire_bytes, &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built message.text failed validation ({})",
                reason.code()
            )
        })
        .coded(crate::error::ErrorCode::Internal)?;
    let event_id = validated.event_id;
    match membership.ingest(validated.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => crate::bail_coded!(
            crate::error::ErrorCode::Internal,
            "internal error: freshly built message.text was rejected by the fold ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => {
            crate::bail_coded!(
                crate::error::ErrorCode::Internal,
                "internal error: freshly built message.text is causally incomplete"
            )
        }
    }

    // ---- Plan the dial set: active members' devices minus our own (D5/D7). ----
    let self_device = endpoint_id_of(secret.device.device_key())?;
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);
    let attempted = dial_set.len();

    let delivered = if dial_set.is_empty() {
        // No other active member to reach: persist locally only (the guarantee).
        store
            .insert(&validated)
            .with_context(|| format!("could not persist message to {}", db_path.display()))?;
        0
    } else {
        // Best-effort live push: the engine's `publish` persists AND fans out.
        let mode = net_mode(loopback);
        let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
        let admission = build_admission(&snapshot);
        let delivered = match run_push(
            store, room_id, secret_key, admission, dial_set, timeout, mode, wire_bytes,
        )
        .await
        {
            Ok(n) => n,
            Err(err) => {
                eprintln!("warning: live delivery unavailable: {err:#}");
                0
            }
        };
        // Guarantee local persistence regardless of the push outcome (idempotent):
        // a duplicate if `publish` already stored it, an insert otherwise.
        let mut store = EventStore::open(&db_path)
            .with_context(|| format!("could not reopen event store at {}", db_path.display()))?;
        store
            .insert(&validated)
            .with_context(|| format!("could not persist message to {}", db_path.display()))?;
        delivered
    };

    Ok(SendSummary {
        event_id,
        room_id: *room_id,
        sender_id,
        delivered,
        attempted,
    })
}

/// The result of an `agent status`, for the caller to present (parallel to
/// [`SendSummary`]).
pub struct StatusSummary {
    /// The authored status event's id.
    pub event_id: EventId,
    /// The room the status belongs to.
    pub room_id: RoomId,
    /// The author's identity (`sender_id`).
    pub sender_id: IdentityKey,
    /// Number of connected peers the frame was pushed to (possibly zero).
    pub delivered: usize,
    /// Number of active peers we tried to reach.
    pub attempted: usize,
}

/// Send a signed `agent.status` to `room_id`: build it, self-validate, persist it
/// locally (the guarantee), then best-effort push it to connected peers. Mirrors
/// [`send`] with the `agent.status` builder substituted (spec IR-0208 D4); see
/// `send` for the availability model and error semantics.
///
/// # Errors
/// Fails — leaving the store untouched on every pre-persist path — if no local
/// identity exists, if the room is unknown, if the caller is not an active
/// member, on a store error, or — as an internal-bug guard — if the freshly built
/// status fails self-validation. A failure to reach peers is **not** an error: it
/// is reported, exit 0. Caller-facing field validation (`status`/`message` caps,
/// `progress > 100`, artifact handles) happens upstream in `agent::status`.
#[allow(clippy::too_many_arguments)] // mirrors send; each arg is a distinct CLI input
pub async fn send_agent_status(
    home: &Path,
    room_id: &RoomId,
    status: &str,
    message: Option<&str>,
    progress_pct: Option<u64>,
    related_artifact_ids: &[[u8; SHORT_ID_LEN]],
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<StatusSummary> {
    let peer_addrs = parse_peers(peers)?;

    // Load the signing secrets (also re-checks them against the public profile).
    let secret = identity::SecretKeys::load(home)?;
    let sender_id = secret.identity.identity_key();

    // ---- Fold the persisted log: confirm the room exists and we are Active. ----
    let db_path = home.join(DB_FILE);
    let mut store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (mut membership, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&sender_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
            "you are not an active member of room {room_id}; only an active member can post a \
             status (this identity is {sender_id})"
        );
    }

    // ---- prev_events = current room heads, bounded per §6 (D8). ----
    let heads = select_heads(&store, room_id)?;

    // ---- Build + self-validate. We do NOT persist here: the engine's `publish`
    // path persists (InsertOutcome::Inserted) and fans out, and a duplicate insert
    // would suppress that fan-out. A final guaranteed insert below covers the case
    // where the live push never runs (mirrors `send`). ----
    let created_at = clock::now_ms();
    let wire = build_agent_status(
        &secret.identity,
        &secret.device,
        room_id,
        status,
        message,
        related_artifact_ids,
        progress_pct,
        &heads,
        created_at,
    );
    let wire_bytes = wire.to_bytes();
    let ctx = ValidationContext::for_room(*room_id);
    let validated = validate_wire_bytes(&wire_bytes, &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built agent.status failed validation ({})",
                reason.code()
            )
        })
        .coded(crate::error::ErrorCode::Internal)?;
    let event_id = validated.event_id;
    match membership.ingest(validated.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => crate::bail_coded!(
            crate::error::ErrorCode::Internal,
            "internal error: freshly built agent.status was rejected by the fold ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => {
            crate::bail_coded!(
                crate::error::ErrorCode::Internal,
                "internal error: freshly built agent.status is causally incomplete"
            )
        }
    }

    // ---- Plan the dial set: active members' devices minus our own (D5/D7). ----
    let self_device = endpoint_id_of(secret.device.device_key())?;
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);
    let attempted = dial_set.len();

    let delivered = if dial_set.is_empty() {
        // No other active member to reach: persist locally only (the guarantee).
        store
            .insert(&validated)
            .with_context(|| format!("could not persist status to {}", db_path.display()))?;
        0
    } else {
        // Best-effort live push: the engine's `publish` persists AND fans out.
        let mode = net_mode(loopback);
        let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
        let admission = build_admission(&snapshot);
        let delivered = match run_push(
            store, room_id, secret_key, admission, dial_set, timeout, mode, wire_bytes,
        )
        .await
        {
            Ok(n) => n,
            Err(err) => {
                eprintln!("warning: live delivery unavailable: {err:#}");
                0
            }
        };
        // Guarantee local persistence regardless of the push outcome (idempotent):
        // a duplicate if `publish` already stored it, an insert otherwise.
        let mut store = EventStore::open(&db_path)
            .with_context(|| format!("could not reopen event store at {}", db_path.display()))?;
        store
            .insert(&validated)
            .with_context(|| format!("could not persist status to {}", db_path.display()))?;
        delivered
    };

    Ok(StatusSummary {
        event_id,
        room_id: *room_id,
        sender_id,
        delivered,
        attempted,
    })
}

/// Print a [`StatusSummary`] as labeled, script-friendly lines (mirrors
/// [`print_send`]).
pub fn print_status(summary: &StatusSummary) {
    println!("status: {}", summary.event_id);
    println!("room:   {}", summary.room_id);
    println!("from:   {}", summary.sender_id);
    println!("stored: yes");
    if summary.delivered == 0 {
        if summary.attempted == 0 {
            println!("delivered: 0 (no other members to reach — stored locally only)");
        } else {
            println!("delivered: 0 (no peers online — stored locally only)");
        }
    } else {
        println!("delivered: {} connected peer(s)", summary.delivered);
    }
}

/// Print a [`SendSummary`] as labeled, script-friendly lines (spec §5 step 6).
pub fn print_send(summary: &SendSummary) {
    println!("sent: {}", summary.event_id);
    println!("room: {}", summary.room_id);
    println!("from: {}", summary.sender_id);
    println!("stored: yes");
    if summary.delivered == 0 {
        if summary.attempted == 0 {
            println!("delivered: 0 (no other members to reach — stored locally only)");
        } else {
            println!("delivered: 0 (no peers online — stored locally only)");
        }
    } else {
        println!("delivered: {} connected peer(s)", summary.delivered);
    }
}

/// Stream the room timeline, receiving and displaying signed messages live until
/// interrupted (Ctrl-C). Brings up a [`Node`], dials the room's other active
/// members, and renders newly-arrived `message.text` rows in canonical order (D4).
///
/// When `accept_joins` is set **and** the caller is the room admin with at least one
/// open invite, the node hosts joins (IR-0104): its admission gate admits a
/// not-yet-`Active` invitee *provisionally* so the invitee can pull the membership
/// sub-DAG and push its `member.joined`. A non-admin caller, or a quiescent room with
/// no open invites, ignores the flag and admits no strangers.
///
/// # Errors
/// Fails before bring-up if no local identity exists, the room is unknown, the
/// caller is not an active member, an option is invalid, or the store / node cannot
/// be opened.
pub async fn tail(
    home: &Path,
    room_id: &RoomId,
    peers: &[String],
    limit: u32,
    accept_joins: bool,
    loopback: bool,
    verbose: bool,
) -> Result<()> {
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();

    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
            "you are not an active member of room {room_id}; only an active member can tail it \
             (this identity is {self_id})"
        );
    }

    // Resolve display names from local `member.joined` events (D10), before the
    // store is handed to the engine.
    let display_names = display_names(&store, room_id)?;

    // Build the live admission gate (IR-0107): a `SnapshotAdmission` reading a cell
    // the node's pump refreshes on every fold change, so a member removed mid-session
    // stops being admitted within a tick (AC2). Seed the cell with the opening
    // snapshot so the accept gate is correct before the first tick. With
    // `--accept-joins` + a real open-invite window, wrap it in the provisional
    // join-bootstrap overlay (IR-0104 Approach A).
    let host_joins = accept_joins && hosting_joins_effective(&snapshot, &self_id);
    if accept_joins {
        report_accept_joins(&snapshot, &self_id, host_joins);
    }
    let admission_cell = Arc::new(Mutex::new(AdmissionView::from_snapshot(&snapshot, &[])));
    let admission: Arc<dyn Admission> = Arc::new(JoinBootstrapAdmission::new(
        SnapshotAdmission::new(admission_cell.clone()),
        host_joins,
    ));

    // Hand the store to the engine and bring up the managed node. The peer manager
    // derives + maintains the dial set from the live snapshot (AC1); we pass the
    // `--peer` hints for deterministic loopback/LAN addressing and do **not** dial
    // explicitly — the manager owns dialing.
    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    // `room tail` is the canonical "provider stays online" surface (IR-0204 spec
    // §5.3 / §6.6): opt this session into serving the blobs it holds. The blob
    // gate's accepted/rejected decisions ride the same `AuditSink` handed to the
    // node; `StderrAudit` renders the `blob.serve.*` vocabulary alongside the
    // taxonomy lines — otherwise those lines would be silently dropped (the CLI
    // installs no `tracing` subscriber).
    let blobs_dir = home.join(crate::file::BLOBS_DIR);
    let node = Node::spawn_room(
        secret_key,
        admission,
        Arc::new(audit::StderrAudit),
        engine,
        cfg,
        DEFAULT_TICK,
        peer_addrs,
        admission_cell,
        Some(BlobServeConfig { blobs_dir }),
    )
    .await
    .context("could not bring up the network node")?;

    // Announce our dialable address so a second terminal can pass it as --peer.
    match node.endpoint_addr() {
        Ok(addr) => {
            println!("listening: {}", render_endpoint_addr(&addr));
            println!("tip: share this address with the other peer via --peer");
        }
        Err(err) => {
            eprintln!("warning: could not determine a dialable address yet: {err}");
        }
    }
    println!("room: {room_id}");
    if verbose {
        print_diagnostics(&node).await;
    }

    // ---- Display loop: poll the timeline + surface the §16.3 connection panel. ----
    let mut seen: BTreeSet<EventId> = BTreeSet::new();
    let mut conn_rx = node.conn_events();
    let mut ticker = tokio::time::interval(TAIL_POLL_INTERVAL);
    // How many of the engine's bounded `logs()` entries have already been rendered
    // (spec IR-0110 AC1/§5.8): this is the receive-path surface where the specific
    // `reject.<code>` distinguishing `bad_signature` from `not_a_member` becomes
    // observable per event.
    let mut logs_seen = 0usize;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            res = &mut ctrl_c => {
                if let Err(err) = res {
                    eprintln!("warning: could not listen for Ctrl-C ({err}); shutting down");
                }
                break;
            }
            conn = conn_rx.recv() => {
                // On a lagged/closed stream just keep going: the roster summary on the
                // next transition re-syncs the human view.
                if let Ok(ev) = conn {
                    print_conn_transition(&node, ev);
                }
            }
            _ = ticker.tick() => {
                match node.room_tail(limit).await {
                    Ok(events) => print_new_messages(&events, &mut seen, &display_names, &snapshot),
                    Err(err) => eprintln!("warning: could not read the timeline: {err}"),
                }
                if let Ok(logs) = node.logs().await {
                    print_new_reject_warnings(&logs, &mut logs_seen);
                }
            }
        }
    }

    node.shutdown()
        .await
        .context("could not shut down cleanly")?;
    Ok(())
}

/// `room members <ROOM_ID> --status`: the recommended human-facing connection view
/// (spec §6.2 point 2 / D6). Brings up an ephemeral managed node, reconciles the
/// dial set from the live snapshot, waits up to `timeout` for links to settle, then
/// prints each member with its membership `role`/`status` **and** live connection
/// state + offline reason + bound device — the §16.3 "distinguish offline peer vs
/// unauthorized peer" view a human reads.
///
/// # Errors
/// Fails before bring-up if no local identity exists, the room is unknown, the caller
/// is not an active member, an option is invalid, or the store / node cannot open.
pub async fn members_status(
    home: &Path,
    room_id: &RoomId,
    peers: &[String],
    timeout: Duration,
    loopback: bool,
    verbose: bool,
) -> Result<()> {
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let self_device = endpoint_id_of(secret.device.device_key())?;

    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
            "you are not an active member of room {room_id}; only an active member can query \
             connection status (this identity is {self_id})"
        );
    }

    // Log-derived left/removed refinement (D5), read before the store is handed to
    // the engine, so a departed member's row reads `left` vs `removed` — the same
    // projection the offline reads use.
    let (removed_ids, left_ids) = display::departure_sets(&store, room_id)?;

    // Live admission cell the pump refreshes; the manager derives the dial set.
    let admission_cell = Arc::new(Mutex::new(AdmissionView::from_snapshot(&snapshot, &[])));
    let admission: Arc<dyn Admission> = Arc::new(SnapshotAdmission::new(admission_cell.clone()));
    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    // A short-lived connection-status query need not serve blobs (spec §6.6).
    let node = Node::spawn_room(
        secret_key,
        admission,
        Arc::new(audit::StderrAudit),
        engine,
        cfg,
        DEFAULT_TICK,
        peer_addrs,
        admission_cell,
        None,
    )
    .await
    .context("could not bring up the network node")?;

    // Wait (bounded) for the desired peers to connect, so the snapshot we print is
    // settled rather than mid-dial. Timeout is expected when a peer is genuinely
    // offline — we then render it as such, which is the whole point.
    let desired: Vec<EndpointId> = PeerManager::desired_devices(&snapshot, self_device)
        .into_iter()
        .collect();
    if !desired.is_empty() {
        let _ = wait_for_any_connected(&node, &desired, timeout).await;
    }

    print_members_status(&node, &snapshot, self_device, &removed_ids, &left_ids);
    if verbose {
        print_diagnostics(&node).await;
    }

    node.shutdown()
        .await
        .context("could not shut down cleanly")?;
    Ok(())
}

/// Print membership × live connection state for `room members --status` (spec §6.2).
/// The `status=` field is the shared D5 display state, so a departed member reads
/// `left` vs `removed` consistently with the offline reads.
fn print_members_status(
    node: &Node,
    snapshot: &MembershipSnapshot,
    self_device: EndpointId,
    removed_ids: &BTreeSet<IdentityKey>,
    left_ids: &BTreeSet<IdentityKey>,
) {
    println!("room: {}", snapshot.room_id());
    match snapshot.admin() {
        Some(admin) => println!("admin: {admin}"),
        None => println!("admin: <none>"),
    }
    let entries: HashMap<EndpointId, PeerEntry> = node.peer_entries().into_iter().collect();
    for m in snapshot.members() {
        let admin_tag = if snapshot.admin() == Some(&m.identity) {
            " (admin)"
        } else {
            ""
        };
        let status = display::member_display_state(m.status, &m.identity, removed_ids, left_ids);
        println!(
            "member: {} role={} status={} conn={}{admin_tag}",
            m.identity,
            role_label(m.role),
            status.as_str(),
            member_conn_field(&entries, m.device, self_device),
        );
    }
    println!("{}", roster_summary(&node.peer_entries()));
}

/// Render the `--verbose` network-diagnostics block (spec IR-0303 §5.3): the local
/// dialable address + relay url, one `diag: peer …` line per known peer with its
/// live direct/relay/mixed/none path classification (read from iroh's
/// `remote_info`, never inferred from latency), and an aggregate `diag: transport
/// …` summary. stderr-only and additive — the default (non-`--verbose`) output is
/// byte-identical without it (§18.5 "hide networking details unless needed").
///
/// Diagnostic only, never a trust input (mirrors [`OfflineReason`]): an
/// `offline`/`unauthorized` peer has no active transport and honestly renders
/// `path=none` rather than as reachable (§16.4 honesty).
async fn print_diagnostics(node: &Node) {
    let local_addrs = node
        .endpoint_addr()
        .ok()
        .map(|addr| {
            addr.ip_addrs()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty());
    eprintln!(
        "diag: local id={} direct={} relay={}",
        node.id(),
        local_addrs.as_deref().unwrap_or("none"),
        node.relay_url().as_deref().unwrap_or("none"),
    );

    let entries: HashMap<EndpointId, PeerEntry> = node.peer_entries().into_iter().collect();
    let (mut direct, mut relay, mut mixed) = (0u32, 0u32, 0u32);
    let (mut connected, mut offline, mut unauthorized) = (0u32, 0u32, 0u32);
    for (device, path_type, relay_url) in node.peer_paths().await {
        let entry = entries.get(&device);
        let identity = entry
            .and_then(|e| e.identity)
            .map_or_else(|| "unknown".to_owned(), |id| short_id(&id));
        let state_label = entry.map_or("unknown", |e| e.state.label());
        eprintln!(
            "diag: peer {identity} device={} state={state_label} path={} relay={}",
            short_device(&device),
            path_type.label(),
            relay_url.as_deref().unwrap_or("none"),
        );
        match path_type {
            PathType::Direct => direct += 1,
            PathType::Relay => relay += 1,
            PathType::Mixed => mixed += 1,
            PathType::None => {}
        }
        if let Some(entry) = entry {
            match entry.state {
                PeerConnState::Connected => connected += 1,
                PeerConnState::Offline => offline += 1,
                PeerConnState::Unauthorized => unauthorized += 1,
                PeerConnState::Connecting => {}
            }
        }
    }
    eprintln!(
        "diag: transport connected={connected} (direct={direct} relay={relay} mixed={mixed}) \
         offline={offline} unauthorized={unauthorized}"
    );
}

/// The connection field for one member row: `self` for us, `n/a` for an invited-only
/// member with no bound device, else the peer's live state (+ offline reason). A
/// desired-but-never-seen device reads `offline reason=never_dialed`.
fn member_conn_field(
    entries: &HashMap<EndpointId, PeerEntry>,
    device: Option<DeviceKey>,
    self_device: EndpointId,
) -> String {
    let Some(dev) = device else {
        return "n/a".to_owned();
    };
    let Ok(id) = EndpointId::from_bytes(dev.as_bytes()) else {
        return "n/a".to_owned();
    };
    if id == self_device {
        return "self".to_owned();
    }
    match entries.get(&id) {
        Some(entry) if entry.state == PeerConnState::Offline => {
            format!(
                "{} reason={}",
                entry.state.label(),
                entry.offline_reason.label()
            )
        }
        Some(entry) => entry.state.label().to_owned(),
        None => "offline reason=never_dialed".to_owned(),
    }
}

/// Presentation string for a [`Role`] (mirrors `room::role_str`).
fn role_label(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
        Role::Agent => "agent",
    }
}

// ---------------------------------------------------------------------------
// Live-push helper (the ephemeral `room send` node)
// ---------------------------------------------------------------------------

/// Bring up an ephemeral node over `store`, dial `dial_set`, wait briefly for at
/// least one link, publish the frame (the engine persists + fans out), grant a
/// short flush grace, and report how many peers were connected at publish time.
///
/// Consumes `store` (moved into the engine) and the wire `frame`.
#[allow(clippy::too_many_arguments)] // distinct carrier inputs; grouping them buys nothing
async fn run_push(
    store: EventStore,
    room_id: &RoomId,
    secret_key: SecretKey,
    admission: AllowlistAdmission,
    dial_set: Vec<EndpointAddr>,
    timeout: Duration,
    mode: NetMode,
    frame: Vec<u8>,
) -> Result<usize> {
    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let cfg = NetConfig {
        mode,
        ..NetConfig::default()
    };
    let node = Node::spawn(
        secret_key,
        Arc::new(admission),
        Arc::new(audit::StderrAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;

    let ids: Vec<EndpointId> = dial_set.iter().map(|a| a.id).collect();
    for addr in dial_set {
        node.connect_to(addr);
    }

    // Wait (bounded by `timeout`) for at least one peer to connect; 0 on timeout.
    let _ = wait_for_any_connected(&node, &ids, timeout).await;

    // The engine ingests (first time ⇒ Inserted) and fans out to connected peers.
    let publish = node
        .publish(frame)
        .await
        .context("could not publish the message frame");

    // Brief grace so the per-peer writer queues flush before we tear down.
    tokio::time::sleep(FLUSH_GRACE).await;
    let delivered = connected_count(&node, &ids);

    let shutdown = node.shutdown().await;
    publish?;
    shutdown.context("could not shut down the network node")?;
    Ok(delivered)
}

/// Wait up to `timeout` for at least one of `ids` to reach `Connected`. Returns the
/// count connected at the moment the first connects, or 0 on timeout.
async fn wait_for_any_connected(node: &Node, ids: &[EndpointId], timeout: Duration) -> usize {
    tokio::time::timeout(timeout, async {
        loop {
            let n = connected_count(node, ids);
            if n > 0 {
                return n;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(0)
}

/// How many of `ids` are currently `Connected`.
fn connected_count(node: &Node, ids: &[EndpointId]) -> usize {
    ids.iter()
        .filter(|id| node.peer_state(**id) == Some(PeerConnState::Connected))
        .count()
}

// ---------------------------------------------------------------------------
// Membership → carrier glue (D5/D7)
// ---------------------------------------------------------------------------

/// Whether a `--accept-joins` request is actually effective: only the room's single
/// immutable admin may host joins, and only while at least one invite is open (a
/// subject still `Invited`). With no open invite the room is quiescent and admits no
/// strangers (IR-0104 Approach A; the "quiescent room admits no strangers" property).
fn hosting_joins_effective(snapshot: &MembershipSnapshot, self_id: &IdentityKey) -> bool {
    snapshot.admin() == Some(self_id) && snapshot.members().any(|m| m.status == Status::Invited)
}

/// Print a short, actionable notice explaining whether `--accept-joins` took effect.
fn report_accept_joins(snapshot: &MembershipSnapshot, self_id: &IdentityKey, host_joins: bool) {
    if host_joins {
        let pending = snapshot
            .members()
            .filter(|m| m.status == Status::Invited)
            .count();
        println!("accepting joins: yes ({pending} open invite(s); provisional bootstrap enabled)");
    } else if snapshot.admin() != Some(self_id) {
        eprintln!("note: --accept-joins ignored — only the room admin can host joins");
    } else {
        eprintln!("note: --accept-joins is a no-op right now — no open invites to bootstrap");
    }
}

/// Build the carrier admission gate from the current membership snapshot (D7):
/// bind every active member's device → identity and mark each Active. This is the
/// production shape the net crate documents (`AllowlistAdmission`).
pub(crate) fn build_admission(snapshot: &MembershipSnapshot) -> AllowlistAdmission {
    let mut auth = AllowlistAdmission::new();
    for m in snapshot.active_members() {
        if let Some(dev) = m.device {
            if let Ok(id) = EndpointId::from_bytes(dev.as_bytes()) {
                auth = auth.bind_device(id, m.identity).set_active(m.identity);
            }
        }
    }
    auth
}

/// The dial set: every active member's device minus our own, addressed by an
/// explicit `--peer` when one matches (deterministic LAN/loopback) else by a bare
/// `EndpointId` resolved through iroh discovery (D5).
///
/// The device **selection** (active-only, self-excluded, deduped) delegates to the
/// net crate's [`PeerManager::desired_devices`] so there is a single implementation
/// shared with the runtime peer manager (IR-0107 §11 step 1); this wrapper only maps
/// each selected device to its `--peer`-hinted or bare address for the callers that
/// still dial explicitly (`room send`, `pipe`).
pub(crate) fn build_dial_set(
    snapshot: &MembershipSnapshot,
    self_device: EndpointId,
    peer_addrs: &[EndpointAddr],
) -> Vec<EndpointAddr> {
    let by_id: BTreeMap<EndpointId, EndpointAddr> =
        peer_addrs.iter().map(|a| (a.id, a.clone())).collect();
    PeerManager::desired_devices(snapshot, self_device)
        .into_iter()
        .map(|id| {
            by_id
                .get(&id)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(id))
        })
        .collect()
}

/// Convert a core [`DeviceKey`] (`device_id`) into an iroh [`EndpointId`]; they are
/// the same raw 32 bytes (Membership §1 / spec A2).
pub(crate) fn endpoint_id_of(dev: DeviceKey) -> Result<EndpointId> {
    EndpointId::from_bytes(dev.as_bytes()).map_err(|err| anyhow!("invalid device id: {err}"))
}

/// Map the loopback flag to a [`NetMode`]: `--loopback` (deterministic CI/LAN
/// tests) vs the default real-network n0 discovery + relay stack (D5).
pub(crate) fn net_mode(loopback: bool) -> NetMode {
    if loopback {
        NetMode::Loopback
    } else {
        NetMode::RealNetwork
    }
}

// ---------------------------------------------------------------------------
// Fold / display helpers
// ---------------------------------------------------------------------------

/// Re-fold a room's persisted log into a membership view, re-validating each stored
/// event through the full §6 pipeline first (mirrors [`crate::room::members`]).
pub(crate) fn fold_room(
    store: &EventStore,
    home: &Path,
    room_id: &RoomId,
) -> Result<(RoomMembership, MembershipSnapshot)> {
    let ids = store
        .room_event_ids(room_id)
        .with_context(|| format!("could not read events for room {room_id}"))?;
    if ids.is_empty() {
        crate::bail_coded!(
            crate::error::ErrorCode::RoomNotFound,
            "no room {} in {}",
            room_id,
            home.display()
        );
    }
    let ctx = ValidationContext::for_room(*room_id);
    let mut validated = Vec::with_capacity(ids.len());
    for id in &ids {
        let stored = store
            .get(id)
            .with_context(|| format!("could not read stored event {id}"))?
            .ok_or_else(|| anyhow!("stored event {id} vanished mid-read"))?;
        let event = validate_wire_bytes(&stored.wire.to_bytes(), &ctx).map_err(|reason| {
            anyhow!("stored event {id} failed re-validation ({})", reason.code())
        })?;
        validated.push(event);
    }
    let membership = RoomMembership::from_events(*room_id, validated);
    let snapshot = membership.snapshot();
    Ok((membership, snapshot))
}

/// Current DAG heads for `prev_events`, truncated deterministically to
/// `MAX_PREV_EVENTS` (identical to the landed `invite.rs` head selection, D8).
pub(crate) fn select_heads(store: &EventStore, room_id: &RoomId) -> Result<Vec<EventId>> {
    let mut heads = store
        .heads(room_id)
        .with_context(|| format!("could not read DAG heads for room {room_id}"))?;
    if heads.len() > MAX_PREV_EVENTS {
        // `heads` is already ascending by event_id; cite the 20 lowest-id heads.
        eprintln!(
            "note: room has {} heads (> {MAX_PREV_EVENTS}); citing the {MAX_PREV_EVENTS} \
             lowest-id heads",
            heads.len()
        );
        heads.truncate(MAX_PREV_EVENTS);
    }
    Ok(heads)
}

/// Print each not-yet-shown `message.text` row in the order `events` arrives
/// (canonical `(lamport, event_id)`), in the D10 identity-first, trust-free format:
/// `[<created_at>] <author>[ (removed)]: <body>`.
fn print_new_messages(
    events: &[StoredEvent],
    seen: &mut BTreeSet<EventId>,
    names: &BTreeMap<IdentityKey, String>,
    snapshot: &MembershipSnapshot,
) {
    for se in events {
        if se.event_type != EventType::MessageText || !seen.insert(se.event_id) {
            continue;
        }
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        let Content::MessageText(m) = &ev.content else {
            continue;
        };
        let author = names
            .get(&ev.sender_id)
            .cloned()
            .unwrap_or_else(|| short_id(&ev.sender_id));
        // `created_at` is advisory/display-only — never used to order (§2.3). The
        // ordering is the store's `(lamport, event_id)`, reflected by `events`.
        let removed = if snapshot.status(&ev.sender_id) == Some(Status::Removed) {
            " (removed)"
        } else {
            ""
        };
        println!(
            "[{}] {author}{removed}: {}",
            iso8601_utc(ev.created_at),
            m.body
        );
    }
}

/// Render every not-yet-seen `reject.<code>` entry in the engine's bounded
/// `logs()` (spec IR-0110 §5.8/AC1) as a pinned `warning[<code>]: …` line — the
/// surface where a receive-path `bad_signature` reject is observable distinctly
/// from a `not_a_member` reject. Other log kinds (`flag.*`, internal drop notes)
/// are ignored here: flags render via the installed [`crate::audit::StderrAudit`]
/// sink instead.
///
/// `logs` is a bounded ring (spec §4.4): under heavy reject volume the oldest
/// entries may already have been evicted by the time this polls, in which case a
/// handful of reject warnings can be missed — an accepted tradeoff for an
/// observability surface, never for a verdict.
fn print_new_reject_warnings(logs: &[String], seen: &mut usize) {
    for line in logs.iter().skip((*seen).min(logs.len())) {
        if let Some(code) = line.strip_prefix("reject.") {
            eprintln!(
                "warning[{code}]: dropped an invalid inbound event; not stored, not re-broadcast"
            );
        }
    }
    *seen = logs.len();
}

/// A short, human-friendly device id: the first 8 chars of the endpoint id.
pub(crate) fn short_device(device: &EndpointId) -> String {
    let s = device.to_string();
    s.get(..8).unwrap_or(&s).to_owned()
}

/// A short, human-scannable prefix of a BLAKE3 hash for a one-line audit record.
pub(crate) fn short_hash(hash: [u8; 32]) -> String {
    hex::encode(hash).chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Connection-state panel (PRD §16.3; spec §6.2/§6.3)
// ---------------------------------------------------------------------------

/// Render one peer's stable, greppable status line (spec §6.2). The offline `reason`
/// is included **only** for an offline peer, so an unauthorized peer never reads as
/// "offline" (§16.4 honesty). Reason/state strings are the pinned
/// [`PeerConnState::label`] / [`OfflineReason::label`] tooling contract.
fn format_peer_line(device: EndpointId, entry: &PeerEntry) -> String {
    let identity = entry
        .identity
        .map_or_else(|| "unknown".to_owned(), |id| short_id(&id));
    let dev = short_device(&device);
    if entry.state == PeerConnState::Offline {
        format!(
            "peer {identity} device={dev} state={} reason={}",
            entry.state.label(),
            entry.offline_reason.label()
        )
    } else {
        format!("peer {identity} device={dev} state={}", entry.state.label())
    }
}

/// A one-line roster summary refreshed on change (spec §6.2). `Connecting` peers are
/// counted as neither connected nor offline yet — they are transient.
fn roster_summary(entries: &[(EndpointId, PeerEntry)]) -> String {
    let (mut connected, mut offline, mut unauthorized) = (0u32, 0u32, 0u32);
    for (_, e) in entries {
        match e.state {
            PeerConnState::Connected => connected += 1,
            PeerConnState::Offline => offline += 1,
            PeerConnState::Unauthorized => unauthorized += 1,
            PeerConnState::Connecting => {}
        }
    }
    format!("peers: {connected} connected, {offline} offline, {unauthorized} unauthorized")
}

/// Print a per-peer transition line + the refreshed roster summary, driven by the
/// live [`ConnEvent`] stream (spec §6.2, the mandatory AC3 surface).
fn print_conn_transition(node: &Node, ev: ConnEvent) {
    let entries = node.peer_entries();
    if let Some((device, entry)) = entries.iter().find(|(d, _)| *d == ev.device) {
        println!("{}", format_peer_line(*device, entry));
    }
    println!("{}", roster_summary(&entries));
}

// ---------------------------------------------------------------------------
// Argument parsing & address serialization
// ---------------------------------------------------------------------------

/// Validate the message body: non-empty and within the §7 byte cap. The protocol
/// allows any UTF-8 body, so control characters are intentionally not rejected.
fn validate_body(body: &str) -> Result<()> {
    if body.is_empty() {
        bail!("message body must not be empty");
    }
    let len = body.len();
    if len > MAX_MESSAGE_BODY_BYTES {
        bail!("message body must be at most {MAX_MESSAGE_BODY_BYTES} bytes (got {len})");
    }
    Ok(())
}

/// Validate the optional `--format` flag against the §7 enum (`plain` | `markdown`).
fn validate_format(format: Option<&str>) -> Result<Option<&str>> {
    match format {
        None => Ok(None),
        Some(f) if MESSAGE_FORMATS.contains(&f) => Ok(Some(f)),
        Some(other) => bail!("unknown --format {other:?}; expected `plain` or `markdown`"),
    }
}

/// Parse the optional `--reply-to` event id (`blake3:<hex>`).
fn parse_event_id(s: &str) -> Result<EventId> {
    s.parse()
        .map_err(|err| anyhow!("invalid --reply-to event id (expected `blake3:<hex>`): {err}"))
}

/// Parse a `--timeout` duration: `<int>{ms|s|m}` (default unit seconds). Rejects
/// empty / non-numeric / overflowing values with an actionable error.
pub fn parse_timeout(spec: &str) -> Result<Duration> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("--timeout must not be empty; use e.g. 5s, 500ms, or 2m");
    }
    let (digits, unit_ms): (&str, u64) = if let Some(rest) = spec.strip_suffix("ms") {
        (rest, 1)
    } else if let Some(rest) = spec.strip_suffix('s') {
        (rest, 1_000)
    } else if let Some(rest) = spec.strip_suffix('m') {
        (rest, 60_000)
    } else {
        (spec, 1_000) // bare number ⇒ seconds
    };
    let digits = digits.trim();
    let value: u64 = digits
        .parse()
        .map_err(|_| anyhow!("--timeout must be a non-negative integer with an optional unit (ms|s|m), e.g. 5s; got {spec:?}"))?;
    value
        .checked_mul(unit_ms)
        .map(Duration::from_millis)
        .ok_or_else(|| anyhow!("--timeout {spec:?} is too large"))
}

/// Parse every `--peer` value into an [`EndpointAddr`].
pub(crate) fn parse_peers(peers: &[String]) -> Result<Vec<EndpointAddr>> {
    peers.iter().map(|s| parse_peer(s)).collect()
}

/// Parse a single `--peer` value: `<ENDPOINT_ID>[@<ip:port>[,<ip:port>...]]`. The
/// id alone relies on discovery; the optional socket addresses make a loopback/LAN
/// dial deterministic (the form `room tail` prints as `listening:`).
fn parse_peer(s: &str) -> Result<EndpointAddr> {
    let s = s.trim();
    let (id_part, addr_part) = match s.split_once('@') {
        Some((id, rest)) => (id, Some(rest)),
        None => (s, None),
    };
    let id = EndpointId::from_str(id_part.trim())
        .map_err(|err| anyhow!("invalid --peer endpoint id {id_part:?}: {err}"))?;
    let mut addr = EndpointAddr::new(id);
    if let Some(rest) = addr_part {
        for sock in rest.split(',') {
            let sock = sock.trim();
            if sock.is_empty() {
                continue;
            }
            let socket = SocketAddr::from_str(sock)
                .map_err(|err| anyhow!("invalid --peer socket address {sock:?}: {err}"))?;
            addr = addr.with_ip_addr(socket);
        }
    }
    Ok(addr)
}

/// Render an [`EndpointAddr`] as the `--peer` wire form
/// `<ENDPOINT_ID>[@<ip:port>,...]` so a second terminal can dial deterministically.
pub(crate) fn render_endpoint_addr(addr: &EndpointAddr) -> String {
    let socks: Vec<String> = addr.ip_addrs().map(ToString::to_string).collect();
    if socks.is_empty() {
        addr.id.to_string()
    } else {
        format!("{}@{}", addr.id, socks.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_event_id, parse_peer, parse_timeout, render_endpoint_addr, validate_body,
        validate_format,
    };
    use iroh_rooms::experimental::session::{EndpointAddr, SecretKey};
    use iroh_rooms_core::event::constants::MAX_MESSAGE_BODY_BYTES;
    use std::time::Duration;

    // ── validate_body ─────────────────────────────────────────────────────────

    #[test]
    fn body_empty_is_rejected() {
        assert!(validate_body("").is_err());
    }

    #[test]
    fn body_at_cap_is_accepted() {
        assert!(validate_body(&"a".repeat(MAX_MESSAGE_BODY_BYTES)).is_ok());
    }

    #[test]
    fn body_over_cap_is_rejected() {
        let err = validate_body(&"a".repeat(MAX_MESSAGE_BODY_BYTES + 1)).unwrap_err();
        assert!(err
            .to_string()
            .contains(&MAX_MESSAGE_BODY_BYTES.to_string()));
    }

    #[test]
    fn body_allows_newlines_and_unicode() {
        assert!(validate_body("hi\nthere — ☕").is_ok());
    }

    // ── validate_format ───────────────────────────────────────────────────────

    #[test]
    fn format_none_is_ok() {
        assert_eq!(validate_format(None).unwrap(), None);
    }

    #[test]
    fn format_plain_and_markdown_are_ok() {
        assert_eq!(validate_format(Some("plain")).unwrap(), Some("plain"));
        assert_eq!(validate_format(Some("markdown")).unwrap(), Some("markdown"));
    }

    #[test]
    fn format_unknown_is_rejected() {
        assert!(validate_format(Some("html")).is_err());
        assert!(validate_format(Some("Plain")).is_err()); // case-sensitive
    }

    // ── parse_event_id ────────────────────────────────────────────────────────

    #[test]
    fn reply_to_requires_blake3_prefix() {
        assert!(parse_event_id(&"ab".repeat(32)).is_err());
        assert!(parse_event_id(&format!("blake3:{}", "ab".repeat(32))).is_ok());
    }

    // ── parse_timeout ─────────────────────────────────────────────────────────

    #[test]
    fn timeout_units_parse() {
        assert_eq!(parse_timeout("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_timeout("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_timeout("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_timeout("3").unwrap(), Duration::from_secs(3)); // bare = seconds
    }

    #[test]
    fn timeout_rejects_garbage() {
        assert!(parse_timeout("").is_err());
        assert!(parse_timeout("soon").is_err());
        assert!(parse_timeout("-5s").is_err());
    }

    // ── parse_peer / render round-trip ────────────────────────────────────────

    fn an_endpoint_id() -> iroh_rooms::experimental::session::EndpointId {
        SecretKey::from_bytes(&[7u8; 32]).public()
    }

    #[test]
    fn peer_id_only_parses() {
        let id = an_endpoint_id();
        let addr = parse_peer(&id.to_string()).unwrap();
        assert_eq!(addr.id, id);
        assert_eq!(addr.ip_addrs().count(), 0);
    }

    #[test]
    fn peer_with_socket_round_trips_through_render() {
        let id = an_endpoint_id();
        let wire = format!("{id}@127.0.0.1:45000");
        let addr = parse_peer(&wire).unwrap();
        assert_eq!(addr.id, id);
        assert_eq!(addr.ip_addrs().count(), 1);
        // render → parse must reproduce the same address.
        let rendered = render_endpoint_addr(&addr);
        let reparsed = parse_peer(&rendered).unwrap();
        assert_eq!(reparsed, addr);
    }

    #[test]
    fn peer_rejects_bad_id_and_socket() {
        assert!(parse_peer("not-an-endpoint-id").is_err());
        assert!(parse_peer(&format!("{}@not-a-socket", an_endpoint_id())).is_err());
    }

    #[test]
    fn render_bare_id_when_no_addrs() {
        let id = an_endpoint_id();
        assert_eq!(render_endpoint_addr(&EndpointAddr::new(id)), id.to_string());
    }

    // `short_id` and `iso8601_utc` moved to `crate::display` (spec IR-0106 D5/D6);
    // their unit tests live alongside them there.
}
