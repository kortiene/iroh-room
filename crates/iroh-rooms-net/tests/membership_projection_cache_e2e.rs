//! End-to-end coverage for the cached membership projection (issue #142).
//!
//! These tests cross the full managed-session stack —
//!   engine fold → ALPN transport → receiver `ingest_frame` →
//!   `refresh_membership_projection_if_needed` → cached `MembershipSnapshot` →
//!   `RoomReconciler::maybe_reconcile` → `PeerManager::reconcile` / admission —
//! and assert the issue #142 acceptance criteria through the engine's
//! `membership_projection_recomputes` counter (issue #142 §7), which is the
//! instrumentation the spec requires ("verified by a counter or
//! instrumentation").
//!
//! The engine-level counter behavior is already pinned by the in-crate
//! `engine_tests.rs` suite (local `publish` path). The unique value of these
//! tests is proving the same invariants hold on the **wire-ingest path** — when
//! the event arrives over the real loopback ALPN via `ingest_frame` → `deliver`
//! → `refresh_membership_projection_if_needed` — and that the cached snapshot
//! served to the net read paths (`PeerManager::reconcile`, admission) stays
//! correct without a per-content-event recompute.
//!
//! Coverage (issue #142 acceptance criteria):
//! * **AC1** — `content_events_over_wire_do_not_recompute_membership_projection`:
//!   accepted `message.text` and `file.shared` frames delivered over the ALPN do
//!   NOT bump `membership_projection_recomputes` on the receiver, while `accepted`
//!   does increment (proving the frame really arrived and was folded).
//! * **AC2** — `member_removed_over_wire_recomputes_and_propagates_to_peer_manager`:
//!   a `member.removed` bumps the counter on both the publisher (local publish →
//!   cache refresh drives `PeerManager::reconcile` → removed peer lands
//!   `Unauthorized`/`Deauthorized`) and the receiver (over-the-wire ingest →
//!   cache refresh), proving acceptance criteria 2 + 4 together.
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, FileShared, MemberInvited, MemberJoined, MemberRemoved,
    MessageText, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
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
const NONCE: [u8; 16] = [0x42; 16];
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
            room_name: "cached-proj-e2e".to_owned(),
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

/// Build a `member.removed` event authored by `admin` targeting `target`.
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

/// Build a `message.text` event citing `prev`.
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
            body: format!("cached-proj-msg-{n}"),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    (ev.event_id(), wire_bytes(&ev, &sender.dev_sk))
}

/// Build a `file.shared` event citing `prev`. Content event for #142 purposes —
/// must NOT refresh the membership projection, but IS folded + fanned out.
fn mk_file_shared(
    sender: &Actor,
    room_id: RoomId,
    prev: EventId,
    blob_hash: HashRef,
    n: u8,
) -> (EventId, Vec<u8>) {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: sender.identity(),
        device_id: sender.device(),
        event_type: EventType::FileShared,
        created_at: T0 + 200 + u64::from(n),
        prev_events: vec![prev],
        content: Content::FileShared(FileShared {
            file_id: [n; 16],
            name: format!("cached-proj-file-{n}.bin"),
            mime_type: "application/octet-stream".to_owned(),
            size_bytes: 1,
            blob_hash,
            blob_format: None,
            providers: None,
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

/// Spawn a **managed-room** node with an empty addr-hint set (inbound-only
/// reconcile). Used for the publisher in AC1, which does not need to auto-dial.
fn spawn_room_node_inbound(
    actor: &Actor,
    engine: SyncEngine,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    spawn_room_node(actor, engine, Vec::new())
}

// ---------------------------------------------------------------------------
// AC1: content events over the wire do NOT recompute the membership projection
// ---------------------------------------------------------------------------

/// AC1 (issue #142): accepted `message.text` and `file.shared` frames delivered
/// over the real loopback ALPN do NOT bump the receiver's
/// `membership_projection_recomputes` counter, while `accepted` does increment —
/// proving the content event was genuinely folded on the wire-ingest path
/// (`ingest_frame` → `deliver` → `refresh_membership_projection_if_needed`) yet
/// left the fold's membership generation unchanged.
///
/// Topology (deterministic loopback):
/// * `admin` — managed `spawn_room`, seeded with the full fold (genesis +
///   invite + join for carol). Publishes `message.text`.
/// * `carol` — managed `spawn_room`, same fold, admin's `EndpointAddr` as an
///   `addr_hint` so its `PeerManager` auto-dials admin. Publishes `file.shared`.
///
/// Both nodes' `maybe_reconcile` calls `engine.snapshot()` (the cached path) on
/// every received frame; the assertion proves that path is served without a
/// per-content-event recompute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // one cohesive two-content-event narrative; splitting fragments it
async fn content_events_over_wire_do_not_recompute_membership_projection() {
    let admin = Actor::new(71);
    let carol = Actor::new(72);

    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite_id, invite_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], carol.identity(), 1, T0 + 1);
    let (join_id, join_bytes) = mk_join(&carol, room_id, &[invite_id], 1, T0 + 2);

    let fold = [
        genesis_bytes.clone(),
        invite_bytes.clone(),
        join_bytes.clone(),
    ];

    // Admin: managed, inbound-only (no addr hints) — carol dials it.
    let admin_node = spawn_room_node_inbound(&admin, make_engine(room_id, &fold)).await;
    let admin_addr = admin_node.endpoint_addr().expect("admin addr");

    // Carol: managed, auto-dials admin from the fold.
    let carol_node = spawn_room_node(&carol, make_engine(room_id, &fold), vec![admin_addr]).await;

    // Wait for the managed dial to succeed — the link is up before we mutate.
    carol_node
        .wait_for_state(admin.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("carol auto-dials admin and reaches Connected");
    admin_node
        .wait_for_state(carol.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin sees carol as Connected via inbound accept");

    // Baseline counters AFTER convergence. Seeding (genesis+invite+join) bumped
    // the counter exactly 3× on each node via local publish; anti-entropy over
    // the now-up link only re-delivers duplicates, which never bump it (issue
    // #142 D2: duplicates do not advance the membership generation). So these
    // baselines are stable against further sync traffic.
    let admin_before = admin_node.counters().await.expect("admin counters");
    let carol_before = carol_node.counters().await.expect("carol counters");

    // --- message.text: admin publishes, carol receives over the wire --------
    let (msg_id, msg_bytes) = mk_message(&admin, room_id, join_id, 1);
    admin_node
        .publish(msg_bytes)
        .await
        .expect("admin publishes message.text");

    // Carol folds the content frame delivered over the ALPN.
    carol_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("message.text arrives at carol over the wire");

    let carol_after_msg = carol_node
        .counters()
        .await
        .expect("carol counters after msg");
    assert!(
        carol_after_msg.accepted > carol_before.accepted,
        "carol must have folded the message.text frame delivered over the wire"
    );
    assert_eq!(
        carol_after_msg.membership_projection_recomputes, carol_before.membership_projection_recomputes,
        "message.text over the wire must NOT refresh the cached membership projection (issue #142 AC1)"
    );

    // --- file.shared: carol publishes, admin receives over the wire ---------
    // A fresh, deterministic blob hash; only its event-id matters here (we are
    // not serving the blob, only proving the content event does not recompute).
    let blob_hash = HashRef::from_bytes([0xC1; 32]);
    let (file_id, file_bytes) = mk_file_shared(&carol, room_id, msg_id, blob_hash, 1);
    carol_node
        .publish(file_bytes)
        .await
        .expect("carol publishes file.shared");

    admin_node
        .wait_until_contains(file_id, WAIT)
        .await
        .expect("file.shared arrives at admin over the wire");

    let admin_after_file = admin_node
        .counters()
        .await
        .expect("admin counters after file");
    assert!(
        admin_after_file.accepted > admin_before.accepted,
        "admin must have folded the file.shared frame delivered over the wire"
    );
    assert_eq!(
        admin_after_file.membership_projection_recomputes,
        admin_before.membership_projection_recomputes,
        "file.shared over the wire must NOT refresh the cached membership projection (issue #142 AC1)"
    );

    // Sanity: the cached snapshot admin serves to its own PeerManager still
    // reports both active members (content events did not change membership).
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert_eq!(
        admin_snap.active_member_count(),
        2,
        "content events must not change the active member count"
    );
    assert!(
        admin_snap
            .active_members()
            .any(|m| m.identity == carol.identity()),
        "carol must remain Active in admin's cached snapshot after content events"
    );
    assert!(
        admin_snap
            .active_members()
            .any(|m| m.identity == admin.identity()),
        "admin must remain Active in its own cached snapshot after content events"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    carol_node.shutdown().await.expect("shutdown carol");
}

// ---------------------------------------------------------------------------
// AC2: member.removed over the wire recomputes and propagates to PeerManager
// ---------------------------------------------------------------------------

/// AC2 (issue #142): a `member.removed` advances `membership_projection_recomputes`
/// on both (a) the **publisher** — whose cache refresh drives
/// `PeerManager::reconcile` to stop the removed peer's loop and mark it
/// `Deauthorized`/`Unauthorized` (acceptance criterion 4: no behavioral
/// regression in `PeerManager::reconcile` or admission verdicts) — and (b) a
/// **wire receiver** observer, who ingests the membership event over the ALPN
/// and refreshes its own cached projection on the `ingest_frame` → `deliver` →
/// `refresh_membership_projection_if_needed` path.
///
/// Topology (deterministic loopback):
/// * `admin` — managed `spawn_room`; fold = genesis + invite/join for bob and
///   carol; bob's and carol's `EndpointAddr`s as `addr_hint`s. Admin's
///   `PeerManager` auto-dials both. Admin authors + publishes `member.removed`
///   for carol.
/// * `bob` — unmanaged static admission; the **observer** who receives carol's
///   removal over the admin→bob link (kept up because bob is NOT removed). His
///   engine pump folds the frame and his counter is observable.
/// * `carol` — unmanaged static admission; the **target** of the removal. Carol
///   is static so her side does not race admin's `Deauthorized` observation.
///
/// A third (observer) node is required for the wire-receiver assertion because
/// once admin's `PeerManager` deauthorizes carol (the very behavior under
/// test), admin stops fanning events out to her — so carol cannot be the
/// wire-receiver of her own removal. Bob, still authorized, is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // one cohesive publish→propagate→observe narrative
async fn member_removed_over_wire_recomputes_and_propagates_to_peer_manager() {
    let admin = Actor::new(73);
    let bob = Actor::new(74);
    let carol = Actor::new(75);

    // Fold: genesis → invite/join bob → invite/join carol → (all three Active).
    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (invite_bob_id, invite_bob_bytes) =
        mk_invite(&admin, room_id, &[genesis_id], bob.identity(), 1, T0 + 1);
    let (join_bob_id, join_bob_bytes) = mk_join(&bob, room_id, &[invite_bob_id], 1, T0 + 2);
    let (invite_carol_id, invite_carol_bytes) =
        mk_invite(&admin, room_id, &[join_bob_id], carol.identity(), 2, T0 + 3);
    let (join_carol_id, join_carol_bytes) = mk_join(&carol, room_id, &[invite_carol_id], 2, T0 + 4);

    let fold = [
        genesis_bytes.clone(),
        invite_bob_bytes.clone(),
        join_bob_bytes.clone(),
        invite_carol_bytes.clone(),
        join_carol_bytes.clone(),
    ];

    // Static observers first (so admin can be seeded with both addr hints).
    let bob_node = spawn_static_node(&bob, make_engine(room_id, &fold), &[&admin, &carol]).await;
    let carol_node = spawn_static_node(&carol, make_engine(room_id, &fold), &[&admin, &bob]).await;
    let bob_addr = bob_node.endpoint_addr().expect("bob addr");
    let carol_addr = carol_node.endpoint_addr().expect("carol addr");

    // Admin: managed spawn_room with both addr hints. PeerManager derives
    // desired = {bob, carol} and dials both on the initial reconcile.
    let admin_node = spawn_room_node(
        &admin,
        make_engine(room_id, &fold),
        vec![bob_addr, carol_addr],
    )
    .await;

    // Wait for the managed mesh to come up before we mutate.
    admin_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin auto-dials bob and reaches Connected (pre-removal)");
    admin_node
        .wait_for_state(carol.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("admin auto-dials carol and reaches Connected (pre-removal)");

    // Baseline counters after convergence (seeding bumped the counter once per
    // accepted membership event via local publish; anti-entropy over the up
    // links only re-delivers duplicates, which never bump it — issue #142 D2).
    let admin_before = admin_node.counters().await.expect("admin counters");
    let bob_before = bob_node.counters().await.expect("bob counters");

    // Author + publish member.removed for carol. After publish() returns, the
    // pump has already called maybe_reconcile (it runs synchronously before the
    // reply), so the cache is refreshed and the PeerManager has stopped carol's
    // loop and set Deauthorized before the publisher-side assertions below.
    let (remove_id, remove_bytes) =
        mk_remove(&admin, room_id, &[join_carol_id], carol.identity(), T0 + 5);
    admin_node
        .publish(remove_bytes)
        .await
        .expect("admin publishes member.removed");

    // AC2 publisher side: the local membership publish refreshed the cache
    // (counter +1), and that refresh is what drove PeerManager::reconcile to
    // deauthorize carol. Tie the counter to the observable net effect.
    let admin_after = admin_node
        .counters()
        .await
        .expect("admin counters after remove");
    assert_eq!(
        admin_after.membership_projection_recomputes,
        admin_before.membership_projection_recomputes + 1,
        "member.removed must refresh the cached projection on the publisher (issue #142 AC2)"
    );
    let removed_entry = admin_node
        .peer_entries()
        .into_iter()
        .find(|(d, _)| *d == carol.endpoint_id())
        .map(|(_, e)| e)
        .expect("carol entry must persist in PeerTable after deauthorization");
    assert_eq!(
        removed_entry.offline_reason,
        OfflineReason::Deauthorized,
        "carol must carry Deauthorized offline reason in admin's PeerTable \
         (PeerManager::reconcile served from the refreshed cached snapshot)"
    );
    // bob remains desired → still Connected (the reconcile only touched carol).
    assert_eq!(
        admin_node.peer_state(bob.endpoint_id()),
        Some(PeerConnState::Connected),
        "bob must remain Connected after carol's removal (unrelated to the delta)"
    );

    // AC2 wire-receiver side: bob ingests carol's removal over the admin→bob
    // link and his cache refreshes on the ingest path — the unique e2e
    // assertion (the in-crate engine test can only exercise local publish).
    bob_node
        .wait_until_contains(remove_id, WAIT)
        .await
        .expect("member.removed arrives at bob over the wire");
    let bob_after = bob_node
        .counters()
        .await
        .expect("bob counters after remove");
    assert!(
        bob_after.accepted > bob_before.accepted,
        "bob must have folded the member.removed frame delivered over the wire"
    );
    assert_eq!(
        bob_after.membership_projection_recomputes,
        bob_before.membership_projection_recomputes + 1,
        "member.removed over the wire must refresh the receiver's cached projection (issue #142 AC2)"
    );

    // Bob's cached snapshot must now exclude carol — the refresh made the
    // removal immediately visible to his read paths (admission / ACL).
    let bob_snap = bob_node.snapshot().await.expect("bob snapshot");
    assert!(
        !bob_snap
            .active_members()
            .any(|m| m.identity == carol.identity()),
        "carol must no longer be Active in bob's cached snapshot after receiving her removal"
    );
    assert!(
        bob_snap
            .active_members()
            .any(|m| m.identity == bob.identity()),
        "bob must remain Active in his own cached snapshot (he was not removed)"
    );

    // AC4: a new inbound connection from the removed carol is rejected
    // Unauthorized by admin's now-live SnapshotAdmission (the cell the same
    // reconcile refreshed) — no behavioral regression in admission verdicts.
    carol_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    admin_node
        .wait_for_state(carol.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("removed carol must be rejected Unauthorized by the live SnapshotAdmission");

    admin_node.shutdown().await.expect("shutdown admin");
    bob_node.shutdown().await.expect("shutdown bob");
    carol_node.shutdown().await.expect("shutdown carol");
}

// ---------------------------------------------------------------------------
// Static-node spawn helper (mirrors manager_e2e::spawn_static_node)
// ---------------------------------------------------------------------------

/// Spawn an **unmanaged** node with a static `iroh_rooms_net::AllowlistAdmission`
/// admitting `self_actor` and every actor in `peers`. The node still runs a real
/// engine pump, so it receives + folds frames over the wire and its `counters()`
/// are observable — it simply has no `PeerManager` reconciling a dial set.
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
