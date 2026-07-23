//! End-to-end tests for the managed-room `PeerManager` path (IR-0107).
//!
//! These tests cross the full managed-session stack:
//!   engine fold → `PeerManager.reconcile` → `dial_loop` → QUIC connect →
//!   `EventProtocolHandler` → `SnapshotAdmission` → `PeerTable`
//!
//! Unlike the focused non-e2e `manager.rs` tests (which observe control-plane
//! properties with a single node and no real network connections), every test
//! here runs two in-process loopback nodes and verifies observable behaviour
//! at the system boundary.
//!
//! Coverage (issue #22 acceptance criteria):
//! * **AC1** — `managed_room_auto_dials_active_member`: known peers are dialed
//!   automatically from the live membership fold with no explicit
//!   `connect_to()` call; the link carries events end-to-end.
//! * **AC2** — `managed_room_unknown_inbound_rejected_by_snapshot_admission`:
//!   unknown inbound endpoints are rejected before any event byte;
//!   the live `SnapshotAdmission` (not a static list) enforces this.
//! * **AC3** — `managed_room_removal_stops_loop_and_marks_deauthorized`:
//!   after a `member.removed` is folded, the `PeerManager` stops the
//!   removed peer's loop and the `PeerTable` shows `Deauthorized`;
//!   subsequent connection attempts from the removed device are
//!   rejected with `Unauthorized` by the now-live `SnapshotAdmission`.
//! * **AC4** — `managed_room_reconnect_delivers_no_duplicates`: a stopped peer
//!   restarted through its `PeerManager` re-converges to the identical
//!   `(event_count, head set, ordered timeline)` as the still-live peers,
//!   applying only the events it missed — reconnect delivers no duplicates.
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MemberRemoved, MessageText,
    RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AdmissionView, AllowlistAdmission, NetConfig, NetMode, Node, OfflineReason, PeerConnState,
    SnapshotAdmission, TracingAudit, DEFAULT_TICK,
};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Deterministic room nonce — distinct from every other test file.
const NONCE: [u8; 16] = [0xdd; 16];
/// Base timestamp (advisory wall-clock anchor).
const T0: u64 = 1_750_000_000_000;
/// Per-step timeout — loopback connects in well under this.
const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Test actor — deterministic key pair seeded from one byte (spec A2).
// ---------------------------------------------------------------------------

struct Actor {
    id_sk: SigningKey,
    dev_sk: SigningKey,
}

impl Actor {
    fn new(seed: u8) -> Self {
        Self {
            id_sk: SigningKey::from_seed(&[seed; 32]),
            dev_sk: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
        }
    }

    fn identity(&self) -> IdentityKey {
        self.id_sk.identity_key()
    }

    fn device(&self) -> DeviceKey {
        self.dev_sk.device_key()
    }

    fn iroh_secret(&self) -> iroh::SecretKey {
        iroh::SecretKey::from_bytes(&self.dev_sk.to_seed())
    }

    /// The iroh `EndpointId` — equal byte-for-byte to `device()` (spec A2).
    fn endpoint_id(&self) -> iroh::EndpointId {
        self.iroh_secret().public()
    }
}

// ---------------------------------------------------------------------------
// Event builders
// ---------------------------------------------------------------------------

/// Seal a `SignedEvent` into verbatim wire bytes.
fn wire_bytes(ev: &SignedEvent, dev_sk: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev_sk);
    WireEvent::seal(csb, sig).to_bytes()
}

/// Build a `room.created` event; returns (`room_id`, `genesis_event_id`, `wire_bytes`).
fn mk_genesis(admin: &Actor) -> (RoomId, EventId, Vec<u8>) {
    let room_id = signed::derive_room_id(&admin.identity(), &NONCE, T0);
    let binding = DeviceBinding::create(&room_id, &admin.id_sk, admin.device());
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "mgr-e2e".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: binding,
        }),
    };
    (room_id, ev.event_id(), wire_bytes(&ev, &admin.dev_sk))
}

/// Build a `member.invited` event; `n` seeds `invite_id`/secret to avoid collisions.
fn mk_invite(
    admin: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    invitee: IdentityKey,
    n: u8,
    ts: u64,
) -> (EventId, Vec<u8>) {
    let invite_id = [n; 16];
    let secret = [n.wrapping_add(1); 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device(),
        event_type: EventType::MemberInvited,
        created_at: ts,
        prev_events: prev.to_vec(),
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: cap_hash,
            role: "member".to_owned(),
            invitee_key: invitee,
            expires_at: None,
            invitee_hint: None,
        }),
    };
    (ev.event_id(), wire_bytes(&ev, &admin.dev_sk))
}

/// Build a `member.joined` event; `n` must match the corresponding invite.
fn mk_join(
    member: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    n: u8,
    ts: u64,
) -> (EventId, Vec<u8>) {
    let invite_id = [n; 16];
    let secret = [n.wrapping_add(1); 16];
    let binding = DeviceBinding::create(&room_id, &member.id_sk, member.device());
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: member.identity(),
        device_id: member.device(),
        event_type: EventType::MemberJoined,
        created_at: ts,
        prev_events: prev.to_vec(),
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: secret,
            role: "member".to_owned(),
            device_binding: binding,
            display_name: None,
        }),
    };
    (ev.event_id(), wire_bytes(&ev, &member.dev_sk))
}

/// Build a `member.removed` event.
fn mk_remove(
    admin: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    target: IdentityKey,
    ts: u64,
) -> (EventId, Vec<u8>) {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device(),
        event_type: EventType::MemberRemoved,
        created_at: ts,
        prev_events: prev.to_vec(),
        content: Content::MemberRemoved(MemberRemoved {
            member_id: target,
            removed_by: admin.identity(),
            reason: None,
            device_binding: None,
        }),
    };
    (ev.event_id(), wire_bytes(&ev, &admin.dev_sk))
}

/// Build a `message.text` event.
fn mk_message(sender: &Actor, room_id: RoomId, prev: EventId, n: u8) -> (EventId, Vec<u8>) {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: sender.identity(),
        device_id: sender.device(),
        event_type: EventType::MessageText,
        created_at: T0 + 100 + u64::from(n),
        prev_events: vec![prev],
        content: Content::MessageText(MessageText {
            body: format!("e2e-msg-{n}"),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    (ev.event_id(), wire_bytes(&ev, &sender.dev_sk))
}

// ---------------------------------------------------------------------------
// Node-spawning helpers
// ---------------------------------------------------------------------------

/// Open an in-memory `SyncEngine` for `room_id` and seed it with `events`.
fn make_engine(room_id: RoomId, events: &[Vec<u8>]) -> SyncEngine {
    let store = EventStore::open_in_memory().expect("in-memory store");
    let mut engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("open engine");
    for ev in events {
        engine.publish(ev).expect("seed event");
    }
    engine
}

/// Spawn a **managed-room** node via `Node::spawn_room`.
///
/// The `SnapshotAdmission` cell starts empty; the pump's initial
/// `maybe_reconcile` populates it from the engine's fold and starts the dial
/// loops for all Active non-self peers found in `addr_hints`.
///
/// Returns a boxed future so `Node::spawn_room`'s ~16 KB state machine is not
/// inlined into each caller (clippy `large_futures`).
fn spawn_room_node(
    actor: &Actor,
    engine: SyncEngine,
    addr_hints: Vec<iroh::EndpointAddr>,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let cell = Arc::new(Mutex::new(AdmissionView::empty()));
        let admission = Arc::new(SnapshotAdmission::new(cell.clone()));
        Node::spawn_room(
            actor.iroh_secret(),
            admission,
            Arc::new(TracingAudit),
            engine,
            NetConfig {
                mode: NetMode::Loopback,
                ..NetConfig::default()
            },
            DEFAULT_TICK,
            addr_hints,
            cell,
            None,
        )
        .await
        .expect("spawn_room")
    })
}

/// Spawn an **unmanaged** node with a static `AllowlistAdmission` admitting
/// `self_actor` and every actor in `peers`.
fn spawn_static_node<'a>(
    self_actor: &'a Actor,
    engine: SyncEngine,
    peers: &'a [&'a Actor],
) -> Pin<Box<dyn Future<Output = Node> + Send + 'a>> {
    Box::pin(async move {
        let mut auth = AllowlistAdmission::new()
            .bind_device(self_actor.endpoint_id(), self_actor.identity())
            .set_active(self_actor.identity());
        for peer in peers {
            auth = auth
                .bind_device(peer.endpoint_id(), peer.identity())
                .set_active(peer.identity());
        }
        Node::spawn(
            self_actor.iroh_secret(),
            Arc::new(auth),
            Arc::new(TracingAudit),
            engine,
            NetConfig {
                mode: NetMode::Loopback,
                ..NetConfig::default()
            },
            DEFAULT_TICK,
        )
        .await
        .expect("spawn_static")
    })
}

// ===========================================================================
// AC1: managed-room auto-dials active members
// ===========================================================================

/// AC1: A `spawn_room` node with a pre-seeded membership fold automatically
/// dials its Active non-self peers through the `PeerManager` — no explicit
/// `connect_to()` is called by the test.
///
/// Setup:
/// * Admin node — static admission, accepts inbound from member.
/// * Member node — `spawn_room` with admin's `EndpointAddr` as an `addr_hint`.
///   Fold (genesis + invite + join) shows admin as Active.
///
/// The pump's initial `maybe_reconcile` derives `desired = {admin}`, starts
/// the dial loop, and the connection is fully established before we assert.
/// A subsequent event published by admin arrives at member over the auto-dialed
/// link, proving the connection carries data (not just a flipped state flag).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_room_auto_dials_active_member() {
    let admin = Actor::new(31);
    let member = Actor::new(32);

    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite_id, invite_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], member.identity(), 1, T0 + 1);
    let (_join_id, join_bytes) = mk_join(&member, room_id, &[invite_id], 1, T0 + 2);

    let events = [
        genesis_bytes.clone(),
        invite_bytes.clone(),
        join_bytes.clone(),
    ];

    // Admin: static admission admitting member; seeded with the full fold so
    // on_connect handshake can serve the membership sub-DAG.
    let admin_node = spawn_static_node(&admin, make_engine(room_id, &events), &[&member]).await;
    let admin_addr = admin_node.endpoint_addr().expect("admin addr");

    // Member: spawn_room with admin's addr hint. The PeerManager derives
    // desired = {admin} from the fold and starts the dial loop immediately.
    let member_node =
        spawn_room_node(&member, make_engine(room_id, &events), vec![admin_addr]).await;

    // No connect_to — the PeerManager dials.
    member_node
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("member auto-dials admin and reaches Connected (AC1)");
    admin_node
        .wait_for_state(member.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin sees member as Connected via inbound accept");

    // Publish a post-genesis event from admin and confirm it crosses the
    // auto-dialed link to member — proving the stream carries data.
    let (msg_id, msg_bytes) = mk_message(&admin, room_id, genesis_id, 1);
    admin_node.publish(msg_bytes).await.expect("admin publish");
    member_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("event arrives at member over the auto-dialed link (AC1 data-flow)");

    // Confirm member's store: the event id the PeerTable reports is the same as
    // what admin published (BLAKE3(wire.signed) id-equality ⇒ byte-equality).
    assert!(
        member_node
            .store_contains(msg_id)
            .await
            .expect("store_contains"),
        "member's validated set must contain the event published by admin"
    );

    // Idempotency: admin's PeerTable entry for member is Connected (from the
    // inbound accept), not just the dialer's outbound side.
    assert_eq!(
        admin_node.peer_state(member.endpoint_id()),
        Some(PeerConnState::Connected),
        "admin's PeerTable must record member as Connected (inbound)"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    member_node.shutdown().await.expect("shutdown member");
}

#[cfg(feature = "gossip_overlay")]
async fn wait_for_gossip_neighbors(node: &Node, expected: usize, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if node.gossip_neighbor_count() == expected {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Review regressions for the managed gossip lifecycle:
///
/// * the first node's initial subscription fails while its seed is unreachable,
///   then a later reconcile tick retries successfully;
/// * the second node reaches that first seed using only its explicit loopback
///   `addr_hint`, proving the hint was registered with iroh's address lookup;
/// * removing the member invalidates the topic epoch and drops the live gossip
///   neighbor on both sides, so the removed peer cannot keep receiving Events.
#[cfg(feature = "gossip_overlay")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gossip_mesh_retries_with_addr_hints_and_tears_down_on_revocation() {
    let admin = Actor::new(0x41);
    let member = Actor::new(0x42);
    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite_id, invite_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], member.identity(), 1, T0 + 1);
    let (join_id, join_bytes) = mk_join(&member, room_id, &[invite_id], 1, T0 + 2);
    let events = [genesis_bytes, invite_bytes, join_bytes];

    // Admin knows the member only by id. Let the initial 5-second gossip join
    // timeout elapse so success below necessarily comes from a retry.
    let admin_node = spawn_room_node(&admin, make_engine(room_id, &events), vec![]).await;
    let admin_addr = admin_node.endpoint_addr().expect("admin addr");
    tokio::time::sleep(Duration::from_millis(5_500)).await;
    assert_eq!(admin_node.gossip_neighbor_count(), 0);

    // Member has the one explicit address hint. The managed Node wiring adds it
    // to iroh's MemoryLookup before subscribing by EndpointId.
    let member_node =
        spawn_room_node(&member, make_engine(room_id, &events), vec![admin_addr]).await;
    assert!(
        wait_for_gossip_neighbors(&admin_node, 1, WAIT).await,
        "admin retry must form a gossip neighbor after member comes online"
    );
    assert!(
        wait_for_gossip_neighbors(&member_node, 1, WAIT).await,
        "member must bootstrap gossip through its explicit address hint"
    );

    let (remove_id, remove_bytes) =
        mk_remove(&admin, room_id, &[join_id], member.identity(), T0 + 3);
    admin_node
        .publish(remove_bytes)
        .await
        .expect("publish member.removed");
    member_node
        .wait_until_contains(remove_id, WAIT)
        .await
        .expect("removed member learns the revocation before link teardown");

    assert!(
        wait_for_gossip_neighbors(&admin_node, 0, WAIT).await,
        "admin must unsubscribe/restart without the removed neighbor"
    );
    assert!(
        wait_for_gossip_neighbors(&member_node, 0, WAIT).await,
        "removed peer's gossip neighbor must be disconnected"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    member_node.shutdown().await.expect("shutdown member");
}

// ===========================================================================
// AC2: unknown inbound rejected by the live SnapshotAdmission
// ===========================================================================

/// AC2: A `spawn_room` node's `SnapshotAdmission` gate — backed by the live
/// membership fold — rejects a stranger's inbound connection before any event
/// byte is read, exactly as `AllowlistAdmission` does in the basic loopback
/// tests but driven by the live fold rather than a static list.
///
/// Setup:
/// * Host node — `spawn_room`; fold has only the host as Active.
/// * Stranger node — unmanaged; dials host explicitly.
///
/// Host's initial `maybe_reconcile` populates the `SnapshotAdmission` cell with
/// the single-member view.  When the stranger dials, `authorize(stranger)` hits
/// `UnknownDevice` → Reject, and the accept handler closes the connection with
/// the stable REJECT code before `accept_bi()` is called.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_room_unknown_inbound_rejected_by_snapshot_admission() {
    let host = Actor::new(33);
    let stranger = Actor::new(34);

    let (room_id, _genesis_id, genesis_bytes) = mk_genesis(&host);

    // Host: spawn_room with just the genesis seeded. After the initial reconcile
    // the SnapshotAdmission cell contains only host as Active.
    let host_node = spawn_room_node(
        &host,
        make_engine(room_id, &[genesis_bytes]),
        vec![], // no outbound hints needed
    )
    .await;
    let host_addr = host_node.endpoint_addr().expect("host addr");

    // Stranger: unmanaged node; the host is in its static admission (so the
    // dialer side of the QUIC handshake completes), but the stranger is NOT in
    // the host's SnapshotAdmission cell.
    let stranger_engine = make_engine(room_id, &[]);
    let stranger_node = spawn_static_node(&stranger, stranger_engine, &[&host]).await;

    let mut host_events = host_node.conn_events();

    // Stranger dials host explicitly (no PeerManager on stranger's side).
    stranger_node.connect_to(host_addr);

    // Host must record the stranger as Unauthorized (SnapshotAdmission rejects
    // the device before accept_bi — the D6 re-point, not a static list).
    host_node
        .wait_for_state(stranger.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("host must record stranger Unauthorized via SnapshotAdmission (AC2)");

    // Structural AC2: host must never have reached Connected for the stranger,
    // so no frame was read from an unauthenticated stream.
    let stranger_id = stranger.endpoint_id();
    let mut saw_connected = false;
    while let Ok(ev) = host_events.try_recv() {
        if ev.device == stranger_id && ev.to == PeerConnState::Connected {
            saw_connected = true;
        }
    }
    assert!(
        !saw_connected,
        "host must never emit Connected for the stranger (reject-before-accept_bi)"
    );

    // Only self (genesis-only fold) is in host's view; stranger is not.
    let host_states: std::collections::HashMap<_, _> =
        host_node.peer_states().into_iter().collect();
    assert_ne!(
        host_states.get(&stranger_id),
        Some(&PeerConnState::Connected),
        "host's PeerTable must not show stranger as Connected"
    );

    host_node.shutdown().await.expect("shutdown host");
    stranger_node.shutdown().await.expect("shutdown stranger");
}

// ===========================================================================
// AC3: member removal stops the dial loop and marks the peer Deauthorized
// ===========================================================================

/// AC3: When the live fold folds a `member.removed` event, the pump's
/// `maybe_reconcile` detects the membership change and the `PeerManager`:
/// 1. Aborts the removed peer's dial loop.
/// 2. Closes the in-flight connection.
/// 3. Writes `Offline(Deauthorized)` to the `PeerTable`.
///
/// After the removal, the now-live `SnapshotAdmission` (cell refreshed by the
/// same reconcile) begins rejecting the removed device as `NotActive`; any
/// subsequent inbound connection attempt from the removed peer is refused with
/// `Unauthorized`, proving the two-phase deauthorization (spec §4.2 / AC3).
///
/// Setup:
/// * Member node — unmanaged; static admission admitting admin; used as the
///   *target* of the dial loop so admin can auto-dial it.
/// * Admin node — `spawn_room`; fold seeded with genesis + invite + join;
///   member's `EndpointAddr` supplied as an `addr_hint`. The `PeerManager`
///   auto-dials member on the initial reconcile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_room_removal_stops_loop_and_marks_deauthorized() {
    let admin = Actor::new(35);
    let member = Actor::new(36);

    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite_id, invite_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], member.identity(), 1, T0 + 1);
    let (join_id, join_bytes) = mk_join(&member, room_id, &[invite_id], 1, T0 + 2);

    // Member: static admission admitting admin (accepts the inbound dial loop).
    let member_node = spawn_static_node(&member, make_engine(room_id, &[]), &[&admin]).await;
    let member_addr = member_node.endpoint_addr().expect("member addr");

    // Admin: spawn_room with full fold seeded and member addr hint.
    // PeerManager derives desired = {member} and starts the dial loop.
    let admin_engine = make_engine(
        room_id,
        &[
            genesis_bytes.clone(),
            invite_bytes.clone(),
            join_bytes.clone(),
        ],
    );
    let admin_node = spawn_room_node(&admin, admin_engine, vec![member_addr.clone()]).await;

    // Wait for the managed dial to succeed — proves the loop ran before we mutate.
    admin_node
        .wait_for_state(member.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin auto-dials member and reaches Connected (pre-removal)");
    member_node
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("member sees admin as Connected (inbound, pre-removal)");

    // Build and publish member.removed. After publish() returns, the pump has
    // already called maybe_reconcile (it runs synchronously before sending the
    // reply), so the manager has stopped member's loop and set the Deauthorized
    // reason before we reach the assert.
    let (_remove_id, remove_bytes) =
        mk_remove(&admin, room_id, &[join_id], member.identity(), T0 + 3);
    admin_node
        .publish(remove_bytes)
        .await
        .expect("admin publishes member.removed");

    // AC3 part 1: PeerTable shows Offline(Deauthorized) for the removed member.
    let removed_entry = admin_node
        .peer_entries()
        .into_iter()
        .find(|(d, _)| *d == member.endpoint_id())
        .map(|(_, e)| e)
        .expect("member entry must persist in PeerTable after deauthorization");
    assert_eq!(
        removed_entry.offline_reason,
        OfflineReason::Deauthorized,
        "removed member must carry Deauthorized offline reason in admin's PeerTable (AC3)"
    );

    // AC3 part 2: the SnapshotAdmission cell was refreshed by the same reconcile.
    // A new inbound connection from the removed member is rejected with Unauthorized.
    member_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    admin_node
        .wait_for_state(member.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("removed member must be rejected Unauthorized by the live SnapshotAdmission (AC3)");

    // Confirm the state transitioned to Unauthorized (not still Offline/Deauthorized).
    assert_eq!(
        admin_node.peer_state(member.endpoint_id()),
        Some(PeerConnState::Unauthorized),
        "post-removal reconnect attempt must land in Unauthorized state (NotActive)"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    member_node.shutdown().await.expect("shutdown member");
}

// ===========================================================================
// AC4: reconnect through the managed PeerManager delivers no duplicates
// ===========================================================================

/// The ordered `event_id` timeline of `node` — the most-recent events in
/// canonical `(lamport, event_id)` order. Two converged nodes must produce the
/// byte-identical list; its length is the store event-count proxy (it excludes
/// causally-incomplete events, exactly as convergence requires).
async fn tail_ids(node: &Node) -> Vec<EventId> {
    node.room_tail(10_000)
        .await
        .expect("room_tail")
        .into_iter()
        .map(|e| e.event_id)
        .collect()
}

/// `node`'s DAG head set, sorted so two converged nodes compare equal regardless
/// of the traversal order `heads()` happens to return.
async fn head_set(node: &Node) -> Vec<EventId> {
    let mut heads = node.heads().await.expect("heads");
    heads.sort();
    heads
}

/// AC4 (spec §8 T-RC — "the headline"): a peer that stops and later restarts
/// **through its `PeerManager`** re-converges to the byte-identical room state as
/// the peers that stayed live, applying only the events it missed while away —
/// reconnect delivers no duplicate application (spec §4.7 / G4).
///
/// Topology (deterministic loopback):
/// * `admin` (A) and `carol` (C) — unmanaged static nodes; A authors the fold and
///   the two hold the live room. A dials C so the A<->C link carries events while
///   the managed peer is away.
/// * `bob` (B) — a **managed** `spawn_room` node whose `PeerManager` auto-dials A
///   and C from the live fold (no explicit `connect_to`). B is the peer we stop
///   and restart; its reconnect is driven entirely by the manager.
///
/// Sequence:
/// 1. Bring the mesh up; A publishes `msg1`/`msg2`; all three converge (state N).
/// 2. **Stop** B (drop its managed node).
/// 3. A publishes `msg3`, C publishes `msg4` (M = 2). They exchange both over the
///    still-live A<->C link; assert A and C advance by exactly M.
/// 4. **Restart** B via `spawn_room`, seeded only with what it held at N
///    (fold + `msg1`/`msg2`). Its manager re-dials A and C and re-pulls the delta.
/// 5. Assert B converges to the identical `(event_count, head set, ordered
///    timeline)` as A and C, that its count advanced by exactly M (only the missed
///    events — no re-application of already-held ones and no duplicate ids), and
///    that A's and C's state is unchanged by B's reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // one cohesive stop→publish→restart narrative; splitting fragments it
async fn managed_room_reconnect_delivers_no_duplicates() {
    // Number of events published while bob is away (asserted +M / no-dup below).
    const M: usize = 2;

    let admin = Actor::new(37);
    let bob = Actor::new(38);
    let carol = Actor::new(39);

    // Fold: genesis + invite/join for both bob and carol → all three Active.
    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite1, invite1_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], bob.identity(), 1, T0 + 1);
    let (join1, join1_bytes) = mk_join(&bob, room_id, &[invite1], 1, T0 + 2);
    let (invite2, invite2_bytes) =
        mk_invite(&admin, room_id, &[join1], carol.identity(), 2, T0 + 3);
    let (join2, join2_bytes) = mk_join(&carol, room_id, &[invite2], 2, T0 + 4);

    let fold: Vec<Vec<u8>> = vec![
        genesis_bytes,
        invite1_bytes,
        join1_bytes,
        invite2_bytes,
        join2_bytes,
    ];

    // Author the message events up front. Each cites the fold tip `join2` — its
    // ancestor view has all three members Active, so a non-admin author (carol)
    // passes the `gate_active_member` check (a message citing genesis would be
    // rejected `NotAMember`, since carol is not yet Active in the genesis view).
    // `join2` is present on every node (seeded), so each message is immediately
    // causally complete. Bytes are cloned into each publish so the originals can
    // re-seed the restarted bob.
    let (msg1_id, msg1_bytes) = mk_message(&admin, room_id, join2, 1);
    let (msg2_id, msg2_bytes) = mk_message(&admin, room_id, join2, 2);
    let (msg3_id, msg3_bytes) = mk_message(&admin, room_id, join2, 3);
    let (msg4_id, msg4_bytes) = mk_message(&carol, room_id, join2, 4);

    // A and C: unmanaged static nodes admitting all three; both seeded with the
    // full fold so an on_connect handshake can serve the membership sub-DAG.
    let admin_node = spawn_static_node(&admin, make_engine(room_id, &fold), &[&bob, &carol]).await;
    let admin_addr = admin_node.endpoint_addr().expect("admin addr");
    let carol_node = spawn_static_node(&carol, make_engine(room_id, &fold), &[&admin, &bob]).await;
    let carol_addr = carol_node.endpoint_addr().expect("carol addr");

    // Keep the two live peers linked so they exchange events while bob is away.
    admin_node.connect_to(carol_addr.clone());
    admin_node
        .wait_for_state(carol.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin<->carol link up");
    carol_node
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("carol registers admin as Connected (inbound) for its fan-out set");

    // Bob: managed node. Its PeerManager derives desired = {admin, carol} from the
    // fold and auto-dials both (hints supplied) — no connect_to in the test.
    let bob_node = spawn_room_node(
        &bob,
        make_engine(room_id, &fold),
        vec![admin_addr.clone(), carol_addr.clone()],
    )
    .await;
    bob_node
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("bob auto-dials admin via the PeerManager");
    bob_node
        .wait_for_state(carol.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("bob auto-dials carol via the PeerManager");

    // Phase 1 — converge on N: admin publishes msg1 + msg2; all three receive them.
    admin_node
        .publish(msg1_bytes.clone())
        .await
        .expect("publish msg1");
    admin_node
        .publish(msg2_bytes.clone())
        .await
        .expect("publish msg2");
    bob_node
        .wait_until_contains(msg1_id, WAIT)
        .await
        .expect("bob has msg1 (state N)");
    bob_node
        .wait_until_contains(msg2_id, WAIT)
        .await
        .expect("bob has msg2 (state N)");
    carol_node
        .wait_until_contains(msg1_id, WAIT)
        .await
        .expect("carol has msg1 (state N)");
    carol_node
        .wait_until_contains(msg2_id, WAIT)
        .await
        .expect("carol has msg2 (state N)");

    // Baseline N — admin's converged timeline length (proves +M and no-dup later).
    let n_count = tail_ids(&admin_node).await.len();

    // Phase 2 — stop bob (drop the managed node entirely).
    bob_node.shutdown().await.expect("shutdown bob");

    // Phase 3 — while bob is away, admin publishes msg3 and carol publishes msg4
    // (M = 2). They exchange both over the still-live admin<->carol link.
    admin_node
        .publish(msg3_bytes.clone())
        .await
        .expect("publish msg3");
    carol_node
        .publish(msg4_bytes.clone())
        .await
        .expect("publish msg4");
    admin_node
        .wait_until_contains(msg4_id, WAIT)
        .await
        .expect("admin gets carol's msg4 over the A<->C link");
    carol_node
        .wait_until_contains(msg3_id, WAIT)
        .await
        .expect("carol gets admin's msg3 over the A<->C link");

    // A and C advanced by exactly M while bob was away (no churn from the absence).
    let admin_tail_before = tail_ids(&admin_node).await;
    let admin_heads_before = head_set(&admin_node).await;
    let carol_tail_before = tail_ids(&carol_node).await;
    let carol_heads_before = head_set(&carol_node).await;
    assert_eq!(
        admin_tail_before.len(),
        n_count + M,
        "admin's store must advance by exactly M while bob is away"
    );
    assert_eq!(
        carol_tail_before.len(),
        n_count + M,
        "carol's store must advance by exactly M while bob is away"
    );

    // Phase 4 — restart bob through spawn_room, seeded ONLY with what it held at N
    // (fold + msg1 + msg2). Its PeerManager re-dials admin and carol and re-pulls
    // the missed delta {msg3, msg4}.
    let mut bob_seed = fold.clone();
    bob_seed.push(msg1_bytes);
    bob_seed.push(msg2_bytes);
    let bob_restarted = spawn_room_node(
        &bob,
        make_engine(room_id, &bob_seed),
        vec![admin_addr, carol_addr],
    )
    .await;
    bob_restarted
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("restarted bob re-dials admin via the PeerManager");
    bob_restarted
        .wait_for_state(carol.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("restarted bob re-dials carol via the PeerManager");

    // Bob must pull exactly the two events it missed (the managed reconnect path).
    bob_restarted
        .wait_until_contains(msg3_id, WAIT)
        .await
        .expect("bob re-pulls msg3 on reconnect");
    bob_restarted
        .wait_until_contains(msg4_id, WAIT)
        .await
        .expect("bob re-pulls msg4 on reconnect");

    // Phase 5 — convergence + no-duplicate assertions.
    let bob_tail = tail_ids(&bob_restarted).await;
    let bob_heads = head_set(&bob_restarted).await;

    // (a) Bob converged to the byte-identical (count, ordered timeline, head set).
    assert_eq!(
        bob_tail, admin_tail_before,
        "restarted bob's ordered timeline must match admin's exactly"
    );
    assert_eq!(
        bob_heads, admin_heads_before,
        "restarted bob's DAG head set must match admin's exactly"
    );

    // (b) Bob's count advanced by exactly M — it applied only the missed events,
    //     never re-applying msg1/msg2 it was seeded with (reconnect != duplicate).
    assert_eq!(
        bob_tail.len(),
        n_count + M,
        "restarted bob must apply exactly the M missed events (no duplicate application)"
    );

    // (c) Each event appears exactly once in bob's timeline (no re-delivery churn).
    let unique: std::collections::HashSet<_> = bob_tail.iter().collect();
    assert_eq!(
        unique.len(),
        bob_tail.len(),
        "bob's timeline must contain no duplicate event ids after reconnect"
    );

    // (d) A's and C's state is unchanged by bob's reconnect — the re-sync delivered
    //     no duplicates to the peers that stayed live.
    assert_eq!(
        tail_ids(&admin_node).await,
        admin_tail_before,
        "admin's timeline must be unchanged by bob's reconnect"
    );
    assert_eq!(
        head_set(&admin_node).await,
        admin_heads_before,
        "admin's head set must be unchanged by bob's reconnect"
    );
    assert_eq!(
        tail_ids(&carol_node).await,
        carol_tail_before,
        "carol's timeline must be unchanged by bob's reconnect"
    );
    assert_eq!(
        head_set(&carol_node).await,
        carol_heads_before,
        "carol's head set must be unchanged by bob's reconnect"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    carol_node.shutdown().await.expect("shutdown carol");
    bob_restarted
        .shutdown()
        .await
        .expect("shutdown bob (restarted)");
}
