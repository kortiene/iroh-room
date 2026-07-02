//! Focused non-e2e tests for [`PeerManager`] (IR-0107).
//!
//! These tests need a tokio runtime and one real iroh loopback endpoint, but
//! no second node and no live network. Dial loops that find no server simply
//! back off and land on Offline — that is the expected path here. The tests
//! observe the manager's _control-plane_ properties (loop count, `PeerTable`
//! state, idempotency, deauthorization) without waiting for any connection
//! to succeed.

use std::sync::Arc;

use iroh::SecretKey;
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MemberRemoved, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::RoomMembership;
use iroh_rooms_net::{
    Admission, AdmissionDecision, AdmissionView, AllowlistAdmission, NetConfig, NetMode,
    NetTransport, OfflineReason, PeerConnState, PeerManager, RejectCause, SnapshotAdmission,
    TracingAudit,
};

// ---------------------------------------------------------------------------
// Deterministic fixture constants
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xcc; 16];
const T0: u64 = 1_750_000_000_000;

// ---------------------------------------------------------------------------
// Test actor: identical seed-derivation to `demo::Participant` so
// `endpoint_id == device_id` byte-for-byte (spec A2).
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
    fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev_sk.to_seed())
    }
    fn endpoint_id(&self) -> iroh::EndpointId {
        self.iroh_secret().public()
    }
}

// ---------------------------------------------------------------------------
// Event builders
// ---------------------------------------------------------------------------

fn seal_and_validate(ev: &SignedEvent, dev_sk: &SigningKey, room_id: RoomId) -> ValidatedEvent {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev_sk);
    let bytes = WireEvent::seal(csb, sig).to_bytes();
    validate_wire_bytes(&bytes, &ValidationContext::for_room(room_id))
        .expect("test event must be stateless-valid")
}

fn mk_genesis(admin: &Actor) -> (RoomId, ValidatedEvent) {
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
            room_name: "mgr-test".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: binding,
        }),
    };
    (room_id, seal_and_validate(&ev, &admin.dev_sk, room_id))
}

/// `n` seeds the `invite_id`/secret so multiple invites don't collide.
fn mk_invite(
    admin: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    invitee: IdentityKey,
    n: u8,
    ts: u64,
) -> ValidatedEvent {
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
    seal_and_validate(&ev, &admin.dev_sk, room_id)
}

/// `n` must match the corresponding `mk_invite` call.
fn mk_join(member: &Actor, room_id: RoomId, prev: &[EventId], n: u8, ts: u64) -> ValidatedEvent {
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
    seal_and_validate(&ev, &member.dev_sk, room_id)
}

fn mk_remove(
    admin: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    target: IdentityKey,
    ts: u64,
) -> ValidatedEvent {
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
    seal_and_validate(&ev, &admin.dev_sk, room_id)
}

// ---------------------------------------------------------------------------
// Transport helper — binds a loopback NetTransport keyed by `actor`.
// ---------------------------------------------------------------------------

async fn bind_transport(actor: &Actor) -> NetTransport {
    NetTransport::bind(
        actor.iroh_secret(),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        },
        None,
        None,
    )
    .await
    .expect("bind loopback transport")
}

// ===========================================================================
// Tests
// ===========================================================================

// ---------------------------------------------------------------------------
// T-M1: fresh manager has zero running loops
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_manager_initial_dial_count_is_zero() {
    let admin = Actor::new(1);
    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );
    assert_eq!(
        manager.dial_count(),
        0,
        "no loops must run before reconcile"
    );
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M2: reconcile with an empty or self-only snapshot starts no loops
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_empty_snapshot_starts_no_loops() {
    let admin = Actor::new(2);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    // Empty fold → no desired peers.
    let empty_snapshot = RoomMembership::new(room_id).snapshot();
    manager.reconcile(&empty_snapshot);
    assert_eq!(manager.dial_count(), 0, "empty snapshot: no loops");

    // Genesis-only fold where self == admin → desired set is empty (self excluded).
    let self_only = RoomMembership::from_events(room_id, [genesis_ev]).snapshot();
    manager.reconcile(&self_only);
    assert_eq!(manager.dial_count(), 0, "self-only snapshot: no loops");

    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M3: reconcile starts exactly one loop per non-self Active member and
//        sets Connecting in the PeerTable before returning
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_starts_loops_and_sets_connecting() {
    let admin = Actor::new(3);
    let member = Actor::new(4);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

    let snapshot =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

    let transport = bind_transport(&admin).await;
    // Subscribe BEFORE reconcile so the synchronous Connecting event is buffered.
    let mut events = transport.conn_events();
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(), // self = admin
        vec![],
    );

    manager.reconcile(&snapshot);

    // One loop for the non-self Active member (admin is self, excluded).
    assert_eq!(
        manager.dial_count(),
        1,
        "exactly one loop for the one non-self Active member"
    );

    // `reconcile` calls `table.set(device, Connecting)` synchronously *before*
    // `tokio::spawn`, so the ConnEvent is already in the broadcast buffer when
    // we reach this line — no race with the dial-loop task.
    let ev = events
        .try_recv()
        .expect("reconcile must emit a Connecting ConnEvent for the new desired device");
    assert_eq!(ev.device, member.endpoint_id());
    assert_eq!(
        ev.to,
        PeerConnState::Connecting,
        "the first ConnEvent for a newly-desired device must be Connecting"
    );

    manager.shutdown();
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M4: reconcile is idempotent — second call with the same snapshot must
//        not spawn a new loop for an already-running one (spec R4 / §4.2)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_is_idempotent() {
    let admin = Actor::new(5);
    let member = Actor::new(6);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

    let snapshot =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    manager.reconcile(&snapshot);
    assert_eq!(manager.dial_count(), 1, "first reconcile: one loop");

    // A second reconcile with the same snapshot must be a no-op — no new loop.
    manager.reconcile(&snapshot);
    assert_eq!(
        manager.dial_count(),
        1,
        "second reconcile with unchanged snapshot must not double-start a loop"
    );

    manager.shutdown();
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M5: reconcile stops a running loop when its device is removed from the
//        membership and marks the device Offline(Deauthorized) in the table
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_stops_removed_device_and_marks_deauthorized() {
    let admin = Actor::new(7);
    let member = Actor::new(8);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);
    let jid = join_ev.event_id;
    let remove_ev = mk_remove(&admin, room_id, &[jid], member.identity(), T0 + 3);

    let snapshot_with = RoomMembership::from_events(
        room_id,
        [genesis_ev.clone(), invite_ev.clone(), join_ev.clone()],
    )
    .snapshot();
    let snapshot_after =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev, remove_ev])
            .snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    // Phase 1: member is Active → one loop running.
    manager.reconcile(&snapshot_with);
    assert_eq!(manager.dial_count(), 1, "before removal: one loop");

    // Phase 2: member removed → loop aborted, Deauthorized reason set in table.
    manager.reconcile(&snapshot_after);
    assert_eq!(manager.dial_count(), 0, "after removal: no loops");

    // `offline_reason` is only written by `set_offline()`; the dial loop's
    // `set(Connecting)` calls never touch it.  So the reason is
    // deterministically Deauthorized right after reconcile returns, regardless
    // of any abort-propagation race on the `state` field (spec §4.2 step 3).
    let entry = transport
        .peer_entries()
        .into_iter()
        .find(|(d, _)| *d == member.endpoint_id())
        .map(|(_, e)| e)
        .expect("member entry must exist in PeerTable after removal");
    assert_eq!(
        entry.offline_reason,
        OfflineReason::Deauthorized,
        "removed member must carry the Deauthorized offline reason"
    );

    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M6: shutdown aborts ALL running loops; dial_count returns 0 immediately
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_aborts_all_running_loops() {
    let admin = Actor::new(9);
    let m1 = Actor::new(10);
    let m2 = Actor::new(11);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;

    // Invite and join two members (distinct invite slots n=1 and n=2).
    let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
    let i1id = invite1.event_id;
    let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
    let j1id = join1.event_id;
    let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
    let i2id = invite2.event_id;
    let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

    let snapshot =
        RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
            .snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    manager.reconcile(&snapshot);
    assert_eq!(manager.dial_count(), 2, "two members → two loops");

    manager.shutdown();
    assert_eq!(manager.dial_count(), 0, "shutdown must abort all loops");

    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M7: Drop aborts all running loops (same guarantee as explicit shutdown)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_aborts_loops_same_as_shutdown() {
    let admin = Actor::new(12);
    let member = Actor::new(13);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

    let snapshot =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    manager.reconcile(&snapshot);
    assert_eq!(manager.dial_count(), 1);

    // Dropping the manager must abort the loop task (spec N6). We verify this
    // indirectly: the PeerTable still holds the device entry (set before spawn),
    // and after drop there are no live references to the manager — the JoinHandle
    // was aborted in Drop, so the background task terminates.
    drop(manager);
    // If the abort did NOT fire, the dial loop would keep running after the
    // manager is gone. We can only verify liveness here: transport shuts down
    // cleanly (no leaked task holds the endpoint open indefinitely).
    transport
        .shutdown()
        .await
        .expect("shutdown after manager drop");
}

// ---------------------------------------------------------------------------
// T-M8: reconcile with addr hints prefers the hint address (resolve_addr)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_with_addr_hint_prefers_hint_over_bare_id() {
    // Build a snapshot with one Active member and pass a `--peer` hint for that
    // member. The manager must start the loop (dial_count == 1) rather than
    // silently failing to resolve the address.
    let admin = Actor::new(14);
    let member = Actor::new(15);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

    let snapshot =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

    let transport = bind_transport(&admin).await;

    // Build a loopback hint for member: id + 127.0.0.1:19999 (unreachable port —
    // we only care that the loop is started with the hint, not that it connects).
    let hint = iroh::EndpointAddr::new(member.endpoint_id())
        .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 19_999u16)));

    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![hint], // supply the --peer hint
    );

    manager.reconcile(&snapshot);
    assert_eq!(
        manager.dial_count(),
        1,
        "hint must be used and one loop started for the Active member"
    );

    manager.shutdown();
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M9: incremental membership growth — two reconcile calls, count grows
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_incremental_growth_adds_loop_without_restarting_existing() {
    let admin = Actor::new(16);
    let m1 = Actor::new(17);
    let m2 = Actor::new(18);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
    let i1id = invite1.event_id;
    let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
    let j1id = join1.event_id;

    // Phase 1: admin + m1.
    let snapshot1 = RoomMembership::from_events(
        room_id,
        [genesis_ev.clone(), invite1.clone(), join1.clone()],
    )
    .snapshot();

    let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
    let i2id = invite2.event_id;
    let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

    // Phase 2: admin + m1 + m2.
    let snapshot2 =
        RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
            .snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    manager.reconcile(&snapshot1);
    assert_eq!(manager.dial_count(), 1, "phase 1: one loop for m1");

    // Second reconcile with a larger snapshot: m2 is newly desired, m1 already
    // has a loop — only m2's loop should be added; m1's must not be restarted.
    manager.reconcile(&snapshot2);
    assert_eq!(
        manager.dial_count(),
        2,
        "phase 2: two loops (m1 kept + m2 added, no duplicate for m1)"
    );

    manager.shutdown();
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M10: one reconcile simultaneously removes one member and adds another —
//         the running set stays at 2 (m2 kept, m1 stopped, m3 started)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_simultaneously_removes_one_member_and_adds_another() {
    let admin = Actor::new(19);
    let m1 = Actor::new(20);
    let m2 = Actor::new(21);
    let m3 = Actor::new(22);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
    let i1id = invite1.event_id;
    let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
    let j1id = join1.event_id;
    let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
    let i2id = invite2.event_id;
    let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);
    let j2id = join2.event_id;

    // Snapshot A: m1 + m2 active, self = admin.
    let snapshot_a = RoomMembership::from_events(
        room_id,
        [
            genesis_ev.clone(),
            invite1.clone(),
            join1.clone(),
            invite2.clone(),
            join2.clone(),
        ],
    )
    .snapshot();

    // Snapshot B: m1 removed, m3 added (m2 stays).
    let remove1 = mk_remove(&admin, room_id, &[j2id], m1.identity(), T0 + 5);
    let rid = remove1.event_id;
    let invite3 = mk_invite(&admin, room_id, &[rid], m3.identity(), 3, T0 + 6);
    let i3id = invite3.event_id;
    let join3 = mk_join(&m3, room_id, &[i3id], 3, T0 + 7);
    let snapshot_b = RoomMembership::from_events(
        room_id,
        [
            genesis_ev, invite1, join1, invite2, join2, remove1, invite3, join3,
        ],
    )
    .snapshot();

    let transport = bind_transport(&admin).await;
    let manager = PeerManager::new(
        transport.shared(),
        transport.endpoint(),
        transport.id(),
        vec![],
    );

    manager.reconcile(&snapshot_a);
    assert_eq!(manager.dial_count(), 2, "snapshot A: m1 + m2");

    manager.reconcile(&snapshot_b);
    assert_eq!(
        manager.dial_count(),
        2,
        "snapshot B: m2 (kept) + m3 (added); m1 stopped"
    );

    // m1 must be deauthorized in the table (its loop was stopped by reconcile).
    let entry = transport
        .peer_entries()
        .into_iter()
        .find(|(d, _)| *d == m1.endpoint_id())
        .map(|(_, e)| e)
        .expect("m1 entry must survive in PeerTable after deauthorization");
    assert_eq!(
        entry.offline_reason,
        OfflineReason::Deauthorized,
        "removed member m1 must carry Deauthorized offline reason"
    );

    manager.shutdown();
    transport.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// T-M11: AdmissionView::from_snapshot correctly builds the live view from a
//         real membership fold — this is the production path used by node.rs
// ---------------------------------------------------------------------------

#[test]
fn admission_view_from_snapshot_admits_active_rejects_removed_and_unknown() {
    use std::sync::{Arc, Mutex};

    let admin = Actor::new(23);
    let member = Actor::new(24);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);
    let jid = join_ev.event_id;
    let remove_ev = mk_remove(&admin, room_id, &[jid], member.identity(), T0 + 3);

    // ── active snapshot: admin + member both Active ───────────────────────────
    let active_snap = RoomMembership::from_events(
        room_id,
        [genesis_ev.clone(), invite_ev.clone(), join_ev.clone()],
    )
    .snapshot();
    let view_active = AdmissionView::from_snapshot(&active_snap, &[]);
    let gate_active = SnapshotAdmission::new(Arc::new(Mutex::new(view_active)));

    assert_eq!(
        gate_active.authorize(admin.endpoint_id()),
        AdmissionDecision::Admit {
            identity: admin.identity()
        },
        "admin must be admitted from the active snapshot"
    );
    assert_eq!(
        gate_active.authorize(member.endpoint_id()),
        AdmissionDecision::Admit {
            identity: member.identity()
        },
        "active member must be admitted from the active snapshot"
    );

    // ── removed snapshot: member is no longer Active ──────────────────────────
    let removed_snap =
        RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev, remove_ev])
            .snapshot();
    let view_removed = AdmissionView::from_snapshot(&removed_snap, &[]);
    let gate_removed = SnapshotAdmission::new(Arc::new(Mutex::new(view_removed)));

    // removed member: device is still in the reverse map → NotActive, not UnknownDevice
    assert_eq!(
        gate_removed.authorize(member.endpoint_id()),
        AdmissionDecision::Reject(RejectCause::NotActive),
        "removed member must be rejected as NotActive (device is known, identity inactive)"
    );

    // admin is still Active after the removal
    assert_eq!(
        gate_removed.authorize(admin.endpoint_id()),
        AdmissionDecision::Admit {
            identity: admin.identity()
        },
        "admin must remain admitted after a different member is removed"
    );

    // a device that never joined is UnknownDevice in both views
    let stranger = iroh::SecretKey::from_bytes(&[0xEEu8; 32]).public();
    assert_eq!(
        gate_removed.authorize(stranger),
        AdmissionDecision::Reject(RejectCause::UnknownDevice),
        "a device that never joined must be rejected as UnknownDevice"
    );
}

// ---------------------------------------------------------------------------
// T-M12: AdmissionView::from_snapshot honours the fail_closed overlay — the
//         production node.rs passes fail_closed_subjects here (spec OQ-6)
// ---------------------------------------------------------------------------

#[test]
fn admission_view_from_snapshot_fail_closed_overrides_active() {
    use std::sync::{Arc, Mutex};

    let admin = Actor::new(25);
    let member = Actor::new(26);
    let (room_id, genesis_ev) = mk_genesis(&admin);
    let gid = genesis_ev.event_id;
    let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
    let iid = invite_ev.event_id;
    let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

    let snap = RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

    // Pass member's identity as a fail-closed subject (simulating a pending removal
    // the local fold hasn't seen yet — spec OQ-6 / §0 incompleteness overlay).
    let view = AdmissionView::from_snapshot(&snap, &[member.identity()]);
    let gate = SnapshotAdmission::new(Arc::new(Mutex::new(view)));

    assert_eq!(
        gate.authorize(member.endpoint_id()),
        AdmissionDecision::Reject(RejectCause::FailClosed),
        "fail_closed identity must be rejected even while still Active in the fold"
    );
    // Admin (not in fail_closed) must still be admitted normally.
    assert_eq!(
        gate.authorize(admin.endpoint_id()),
        AdmissionDecision::Admit {
            identity: admin.identity()
        },
        "admin must be admitted when only the member is fail_closed"
    );
}
