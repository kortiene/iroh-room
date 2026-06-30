//! Focused verification of the bounded recent-sync engine (IR-0007).
//!
//! These smoke tests drive the deterministic [`SimNet`] harness to prove the four
//! issue acceptance criteria + the headline security property end-to-end:
//!
//! * **AC1/AC4** — an offline peer reconnects and converges to the expected set,
//!   asserted via the [`SyncDigest`] oracle.
//! * **AC2** — the membership/admin sub-DAG is fully reconciled even when chat is
//!   bounded to a tiny window.
//! * **AC3** — a child delivered before its parent is buffered + backfilled, not
//!   rejected.
//! * **Security** — a node with a stale admin tip fails closed on the affected
//!   subject, then recovers after catch-up.
//! * **Test plan** — shuffled delivery converges across many seeds.
//!
//! The exhaustive `tests/sync_convergence.rs` matrix (§8) lands in a separate test
//! phase; this file is the implementation-phase correctness gate.

#![cfg(feature = "sync")]
#![allow(clippy::similar_names)]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, MemberInvited, MemberJoined, MemberRemoved, MessageText, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::Status;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::sim::SimNet;
use iroh_rooms_core::sync::{
    Completeness, PeerId, Severity, SyncConfig, SyncEngine, SyncMessage, Window,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xab; 16];
const T0: u64 = 1_750_000_000_000;

const NODE_A: PeerId = PeerId::from_bytes([0xA1; 32]);
const NODE_B: PeerId = PeerId::from_bytes([0xB2; 32]);

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

struct Principal {
    id: SigningKey,
    dev: SigningKey,
}

impl Principal {
    fn new(seed: u8) -> Self {
        Self {
            id: sk(seed),
            dev: sk(seed.wrapping_add(0x80)),
        }
    }
    fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }
    fn device(&self) -> DeviceKey {
        self.dev.device_key()
    }
}

fn wire(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// A built room log: causally-ordered wire frames + the cast for assertions.
struct Built {
    room: RoomId,
    /// Causally-ordered wire frames; if `with_removal`, the removal is **last**.
    events: Vec<Vec<u8>>,
    alice: Principal,
    bob: Principal,
    carol: Principal,
}

/// Build `genesis → invite_bob → join_bob → invite_carol → join_carol →
/// {n_chat bob messages} → [remove_carol]`.
///
/// Admin events (alice) cite the prior admin event so `admin_seq` flows; chat is
/// authored by bob (a member) and parented on his join, so it is **chat-class**
/// (windowable) and causally shallow (each message is an independent sibling).
#[allow(clippy::too_many_lines)] // fully-specified fixture events inline for clarity
fn build_log(n_chat: u32, with_removal: bool) -> Built {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut events = Vec::new();
    let mut t = T0;

    // genesis
    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: iroh_rooms_core::event::content::EventType::RoomCreated,
        created_at: t,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Sync Smoke".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
        }),
    };
    let gid = genesis.event_id();
    events.push(wire(&genesis, &alice.dev));

    // invite_bob
    t += 1;
    let inv_bob_id = [0x01; 16];
    let inv_bob_sec = [0x41; 16];
    let inv_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: iroh_rooms_core::event::content::EventType::MemberInvited,
        created_at: t,
        prev_events: vec![gid],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_bob_id,
            capability_hash: capability_hash(&room, &inv_bob_id, &inv_bob_sec),
            role: "member".to_owned(),
            invitee_key: bob.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let inv_bob_eid = inv_bob.event_id();
    events.push(wire(&inv_bob, &alice.dev));

    // join_bob
    t += 1;
    let join_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: bob.identity(),
        device_id: bob.device(),
        event_type: iroh_rooms_core::event::content::EventType::MemberJoined,
        created_at: t,
        prev_events: vec![inv_bob_eid],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_bob_id,
            capability_secret: inv_bob_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &bob.id, bob.device()),
            display_name: None,
        }),
    };
    let join_bob_eid = join_bob.event_id();
    events.push(wire(&join_bob, &bob.dev));

    // invite_carol (cites the prior admin event so admin_seq flows)
    t += 1;
    let inv_carol_id = [0x02; 16];
    let inv_carol_sec = [0x42; 16];
    let inv_carol = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: iroh_rooms_core::event::content::EventType::MemberInvited,
        created_at: t,
        prev_events: vec![inv_bob_eid],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_carol_id,
            capability_hash: capability_hash(&room, &inv_carol_id, &inv_carol_sec),
            role: "member".to_owned(),
            invitee_key: carol.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let inv_carol_eid = inv_carol.event_id();
    events.push(wire(&inv_carol, &alice.dev));

    // join_carol
    t += 1;
    let join_carol = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: carol.identity(),
        device_id: carol.device(),
        event_type: iroh_rooms_core::event::content::EventType::MemberJoined,
        created_at: t,
        prev_events: vec![inv_carol_eid],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_carol_id,
            capability_secret: inv_carol_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &carol.id, carol.device()),
            display_name: None,
        }),
    };
    let join_carol_eid = join_carol.event_id();
    events.push(wire(&join_carol, &carol.dev));

    // chat (bob), each parented on his join → chat-class siblings
    for i in 0..n_chat {
        t += 1;
        let msg = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: bob.identity(),
            device_id: bob.device(),
            event_type: iroh_rooms_core::event::content::EventType::MessageText,
            created_at: t,
            prev_events: vec![join_bob_eid],
            content: Content::MessageText(MessageText {
                body: format!("message {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        events.push(wire(&msg, &bob.dev));
    }

    // remove_carol (alice), cites her prior admin event + carol's join → admin tip
    if with_removal {
        t += 1;
        let remove = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: iroh_rooms_core::event::content::EventType::MemberRemoved,
            created_at: t,
            prev_events: vec![join_carol_eid, inv_carol_eid],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: carol.identity(),
                removed_by: alice.identity(),
                reason: None,
                device_binding: None,
            }),
        };
        events.push(wire(&remove, &alice.dev));
    }

    Built {
        room,
        events,
        alice,
        bob,
        carol,
    }
}

fn fresh_engine(room: RoomId, config: SyncConfig) -> SyncEngine {
    let store = EventStore::open_in_memory().expect("in-memory store");
    SyncEngine::open(store, room, config).expect("open engine")
}

/// Seed a peer's engine with a slice of the log (in causal order) via `publish`.
fn seed(net: &mut SimNet, peer: PeerId, frames: &[Vec<u8>]) {
    for f in frames {
        net.engine_mut(peer).publish(f).expect("seed publish");
    }
}

// ---------------------------------------------------------------------------
// AC1 / AC4 — offline peer reconnects and converges to the expected set
// ---------------------------------------------------------------------------

#[test]
fn offline_peer_reconnects_and_converges() {
    let built = build_log(4, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // Node A holds the whole room; Node B is an empty peer reconnecting.
    seed(&mut net, NODE_A, &built.events);

    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    // Full set equality (the window covers all four chat events).
    net.assert_converged(&[NODE_A, NODE_B]);

    let snap = net.engine(NODE_B).snapshot();
    assert_eq!(snap.admin(), Some(&built.alice.identity()));
    assert!(snap.is_active(&built.bob.identity()), "bob active on B");
    assert_eq!(
        snap.status(&built.carol.identity()),
        Some(Status::Removed),
        "carol removed on B after catch-up"
    );
    assert_eq!(net.engine(NODE_B).completeness(), Completeness::Complete);
}

// ---------------------------------------------------------------------------
// AC2 — membership fully reconciled even when chat is bounded
// ---------------------------------------------------------------------------

#[test]
fn tiny_chat_window_reconciles_membership_but_bounds_chat() {
    let built = build_log(6, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    // Node B asks for only the last two chat events on connect.
    let tight = SyncConfig {
        chat_window_default: 2,
        ..SyncConfig::default()
    };
    net.add_peer(NODE_B, fresh_engine(built.room, tight));

    seed(&mut net, NODE_A, &built.events);
    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    // The never-windowed authorization sub-DAG + snapshot are exactly equal...
    net.assert_membership_converged(&[NODE_A, NODE_B]);

    // ...while the chat sets differ by exactly the windowed amount.
    let a = net.engine(NODE_A).digest().unwrap();
    let b = net.engine(NODE_B).digest().unwrap();
    let a_membership = net.engine(NODE_A).membership_event_ids().unwrap();
    let b_membership = net.engine(NODE_B).membership_event_ids().unwrap();
    let a_chat = a.event_ids.len() - a_membership.len();
    let b_chat = b.event_ids.len() - b_membership.len();
    assert_eq!(a_chat, 6, "A holds all six chat events");
    assert_eq!(b_chat, 2, "B holds exactly the two-event window");
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "carol removal reconciled despite the tiny chat window"
    );
}

// ---------------------------------------------------------------------------
// AC3 — missing parent buffered + backfilled, not rejected
// ---------------------------------------------------------------------------

#[test]
fn child_before_parent_is_buffered_then_backfilled() {
    let built = build_log(0, false);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));
    seed(&mut net, NODE_A, &built.events);

    net.connect(NODE_A, NODE_B);

    // Deliver carol's join (index 4) to B before any of its ancestors arrive.
    let join_carol = built.events[4].clone();
    net.deliver_raw(NODE_B, NODE_A, &join_carol);

    // It must be buffered (not rejected) and trigger a backfill request.
    assert_eq!(
        net.engine(NODE_B).parked_len(),
        1,
        "join is parked, not dropped"
    );
    assert!(
        net.engine(NODE_B).counters().backfill_requests >= 1,
        "a WantEvents backfill was emitted for the missing parent"
    );

    net.run_to_quiescence();

    // The parent chain backfilled, the park drained, and carol joined.
    assert_eq!(net.engine(NODE_B).parked_len(), 0, "park fully drained");
    assert!(
        net.engine(NODE_B)
            .snapshot()
            .is_active(&built.carol.identity()),
        "carol active after backfill"
    );
    net.assert_membership_converged(&[NODE_A, NODE_B]);
}

// ---------------------------------------------------------------------------
// Security — stale admin tip fails closed, then recovers
// ---------------------------------------------------------------------------

#[test]
fn stale_admin_tip_fails_closed_then_recovers() {
    let built = build_log(0, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // A has the full log (incl. the removal); B has everything *except* it.
    seed(&mut net, NODE_A, &built.events);
    let prefix = &built.events[..built.events.len() - 1];
    seed(&mut net, NODE_B, prefix);

    // Before learning the higher tip, B believes carol is still active.
    assert_eq!(net.engine(NODE_B).completeness(), Completeness::Complete);
    assert!(net
        .engine(NODE_B)
        .snapshot()
        .is_active(&built.carol.identity()));

    // B receives A's higher admin tip while still partitioned (no link).
    let a_tip = net.engine(NODE_A).digest().unwrap().admin_tip;
    let _ = net.engine_mut(NODE_B).on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: a_tip,
        },
    );

    // B now suspects its admin view is incomplete and fails closed on carol.
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::AdminViewSuspect
    );
    assert!(
        net.engine(NODE_B)
            .fail_closed_subjects()
            .contains(&built.carol.identity()),
        "carol is fail-closed while the removal is unconfirmed"
    );

    // After reconnect + catch-up the gap closes and the verdict follows the
    // converged snapshot.
    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();
    assert_eq!(net.engine(NODE_B).completeness(), Completeness::Complete);
    assert!(
        net.engine(NODE_B).fail_closed_subjects().is_empty(),
        "fail-closed set clears after catch-up"
    );
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed)
    );
    net.assert_converged(&[NODE_A, NODE_B]);
}

// ---------------------------------------------------------------------------
// Test plan — shuffled delivery converges deterministically across seeds
// ---------------------------------------------------------------------------

#[test]
fn shuffled_delivery_converges_across_seeds() {
    for seed_val in [1_u64, 2, 3, 7, 42, 9999] {
        let built = build_log(4, true);
        let mut net = SimNet::new(built.room);
        net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
        net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));
        seed(&mut net, NODE_A, &built.events);

        net.connect(NODE_A, NODE_B);
        net.shuffle(seed_val);
        net.run_to_quiescence();

        net.assert_converged(&[NODE_A, NODE_B]);
        assert_eq!(
            net.engine(NODE_B)
                .snapshot()
                .status(&built.carol.identity()),
            Some(Status::Removed),
            "seed {seed_val}: carol removed"
        );
    }
}

// ---------------------------------------------------------------------------
// Determinism guard — a CRITICAL equivocation alert is raised on an admin fork
// ---------------------------------------------------------------------------

#[test]
fn admin_fork_raises_critical_equivocation() {
    // Two distinct removals at the same admin_seq: a self-fork by the admin.
    let built = build_log(0, false);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    seed(&mut net, NODE_A, &built.events);

    // Re-derive the two parents the removal cited and forge two distinct removals
    // (different `reason`) at the same admin_seq.
    let inv_carol_eid = SignedEvent::decode(&WireEvent::decode(&built.events[3]).unwrap().signed)
        .unwrap()
        .event_id();
    let join_carol_eid = SignedEvent::decode(&WireEvent::decode(&built.events[4]).unwrap().signed)
        .unwrap()
        .event_id();

    let mk_removal = |reason: &str| {
        let ev = SignedEvent {
            schema_version: 1,
            room_id: built.room,
            sender_id: built.alice.identity(),
            device_id: built.alice.device(),
            event_type: iroh_rooms_core::event::content::EventType::MemberRemoved,
            created_at: T0 + 100,
            prev_events: vec![join_carol_eid, inv_carol_eid],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: built.carol.identity(),
                removed_by: built.alice.identity(),
                reason: Some(reason.to_owned()),
                device_binding: None,
            }),
        };
        wire(&ev, &built.alice.dev)
    };

    let fork_a = mk_removal("a");
    let fork_b = mk_removal("b");

    // A ingests both branches; the second at the same admin_seq trips the detector.
    net.deliver_raw(NODE_A, NODE_A, &fork_a);
    net.deliver_raw(NODE_A, NODE_A, &fork_b);

    assert_eq!(
        net.engine(NODE_A).completeness(),
        Completeness::AdminForkDetected
    );
    let decisions = net.engine(NODE_A).trust_decisions();
    assert!(
        decisions
            .iter()
            .any(|d| d.code == "equivocation" && d.severity == Severity::Critical),
        "a CRITICAL equivocation trust decision is recorded"
    );
}

// ---------------------------------------------------------------------------
// Fan-out — three-peer mesh converges via relay through middle peer
// ---------------------------------------------------------------------------

#[test]
fn three_peer_fan_out_converges() {
    const NODE_C: PeerId = PeerId::from_bytes([0xC3; 32]);

    let built = build_log(3, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_C, fresh_engine(built.room, SyncConfig::default()));

    // Only A starts with the full log; B and C are empty latecomers.
    seed(&mut net, NODE_A, &built.events);

    net.connect_all();
    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B, NODE_C]);
    for peer in [NODE_B, NODE_C] {
        assert_eq!(net.engine(peer).completeness(), Completeness::Complete);
        assert_eq!(
            net.engine(peer).snapshot().status(&built.carol.identity()),
            Some(Status::Removed),
            "carol removed at {peer}"
        );
    }
}

// ---------------------------------------------------------------------------
// Reconnect after missed events — partial-log peer fills the gap
// ---------------------------------------------------------------------------

#[test]
fn partial_log_peer_gets_missed_events_on_reconnect() {
    let built = build_log(4, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // A holds the full log. B holds the 5 membership events but missed the
    // 4 chat events and the removal — simulating a peer that was offline while
    // those events accumulated.
    seed(&mut net, NODE_A, &built.events);
    seed(&mut net, NODE_B, &built.events[..5]);

    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "B caught up with the removal after reconnect"
    );
    assert_eq!(net.engine(NODE_B).completeness(), Completeness::Complete);
}

// ---------------------------------------------------------------------------
// Anti-amplification — signer pre-check drops chat from an unknown key
// ---------------------------------------------------------------------------

#[test]
fn signer_precheck_drops_chat_from_unknown_member() {
    // build_log(1, false): events[5] is a chat message authored by bob.
    let built = build_log(1, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    // Seed only genesis so the snapshot contains alice (admin) but not bob.
    engine.publish(&built.events[0]).expect("seed genesis");

    // Bob's chat message is a sibling of join_bob; deliver it before join_bob
    // so the fold returns Buffered. Bob is not admin and not in the snapshot,
    // so the signer pre-check must drop it — not park it.
    let chat = &built.events[built.events.len() - 1];
    let _ = engine.ingest_frame(NODE_A, chat);

    assert_eq!(
        engine.parked_len(),
        0,
        "chat from unknown signer is not parked"
    );
    assert_eq!(
        engine.counters().signer_dropped,
        1,
        "signer_dropped counter incremented"
    );
}

// ---------------------------------------------------------------------------
// Protocol safety — foreign-room frame is silently dropped
// ---------------------------------------------------------------------------

#[test]
fn foreign_room_message_is_silently_dropped() {
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    let foreign = RoomId::from_bytes([0xFF; 32]);
    let msg = SyncMessage::WantMembership {
        room_id: foreign,
        have: vec![],
    };
    let outs = engine.on_message(NODE_A, msg);

    assert!(
        outs.is_empty(),
        "foreign-room message must produce no output"
    );
    assert!(
        engine.logs().iter().any(|l| l.contains("foreign room")),
        "foreign-room drop must be logged"
    );
}

// ---------------------------------------------------------------------------
// Idempotency — duplicate frame must not trigger a re-broadcast storm
// ---------------------------------------------------------------------------

#[test]
fn duplicate_frame_does_not_trigger_rebroadcast() {
    let built = build_log(3, false);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    seed(&mut net, NODE_A, &built.events);
    seed(&mut net, NODE_B, &built.events);

    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    // Both fully synced. Deliver a frame A already holds as if it came from B.
    let accepted_before = net.engine(NODE_A).counters().accepted;
    let outs = net
        .engine_mut(NODE_A)
        .ingest_frame(NODE_B, &built.events[0]);

    let rebroadcast: Vec<_> = outs
        .iter()
        .filter(|o| matches!(&o.msg, SyncMessage::Events { .. }))
        .collect();
    assert!(
        rebroadcast.is_empty(),
        "duplicate frame must not produce Events fan-out"
    );
    assert_eq!(
        net.engine(NODE_A).counters().accepted,
        accepted_before,
        "no new events accepted on duplicate delivery"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — per-author park cap evicts the oldest frame
// ---------------------------------------------------------------------------

#[test]
fn park_per_author_cap_evicts_oldest() {
    let built = build_log(0, true);
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4) remove_carol(5)
    let tight = SyncConfig {
        max_parked_per_author: 1,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, tight).expect("engine");

    // Seed genesis only; alice is admin, inv_bob_eid is unknown to the store.
    engine.publish(&built.events[0]).expect("seed genesis");

    // First alice orphan (invite_carol, parent=inv_bob_eid missing) → parked.
    let _ = engine.ingest_frame(NODE_A, &built.events[3]);
    assert_eq!(engine.parked_len(), 1);
    assert_eq!(engine.counters().park_evicted, 0);

    // Second alice orphan (remove_carol, parents missing) → per-author cap
    // evicts the first; park stays at 1.
    let _ = engine.ingest_frame(NODE_A, &built.events[5]);
    assert_eq!(engine.parked_len(), 1, "park held at per-author cap");
    assert_eq!(engine.counters().park_evicted, 1, "oldest evicted");
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("max_parked_per_author")),
        "eviction must be logged"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — token bucket suppresses the second backfill request
// ---------------------------------------------------------------------------

#[test]
fn backfill_token_exhausted_suppresses_request() {
    let built = build_log(0, true);
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4) remove_carol(5)
    let tight = SyncConfig {
        backfill_tokens_per_author: 1,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, tight).expect("engine");

    engine.publish(&built.events[0]).expect("seed genesis");

    // First alice orphan (invite_carol): depth=0, signer ok → park + take token.
    let _ = engine.ingest_frame(NODE_A, &built.events[3]);
    assert_eq!(engine.parked_len(), 1);

    // Second alice orphan (remove_carol): token bucket empty → suppressed.
    let _ = engine.ingest_frame(NODE_A, &built.events[5]);
    assert!(
        engine.counters().backfill_rate_limited >= 1,
        "second backfill must be suppressed when token is exhausted"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("backfill_rate_limited")),
        "suppression must be logged"
    );
}

// ---------------------------------------------------------------------------
// Connectivity — disconnect removes peer from fan-out
// ---------------------------------------------------------------------------

#[test]
fn disconnect_stops_fanout_to_peer() {
    let built = build_log(1, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    // Seed membership (no chat yet).
    for frame in &built.events[..5] {
        engine.publish(frame).expect("seed");
    }

    // Connect NODE_A so it enters the fan-out set, then immediately disconnect.
    let _ = engine.on_connect(NODE_A);
    engine.on_disconnect(NODE_A);

    // Publish a chat event — no fan-out should target the disconnected peer.
    let outs = engine
        .publish(&built.events[built.events.len() - 1])
        .expect("publish chat");
    let to_a: Vec<_> = outs.iter().filter(|o| o.peer == NODE_A).collect();
    assert!(to_a.is_empty(), "no fan-out to disconnected peer");
}

// ---------------------------------------------------------------------------
// Protocol correctness — on_connect emits the four required handshake messages
// ---------------------------------------------------------------------------

#[test]
fn on_connect_emits_four_handshake_messages() {
    let built = build_log(2, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    let outs = engine.on_connect(NODE_A);

    assert_eq!(outs.len(), 4, "exactly four handshake messages");
    assert!(
        outs.iter().all(|o| o.peer == NODE_A),
        "all messages addressed to the new peer"
    );
    assert!(
        outs.iter()
            .any(|o| matches!(&o.msg, SyncMessage::AdminTip { .. })),
        "handshake must include AdminTip"
    );
    assert!(
        outs.iter()
            .any(|o| matches!(&o.msg, SyncMessage::Heads { .. })),
        "handshake must include Heads"
    );
    assert!(
        outs.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantMembership { .. })),
        "handshake must include WantMembership"
    );
    assert!(
        outs.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantRecentChat { .. })),
        "handshake must include WantRecentChat"
    );
}

// ---------------------------------------------------------------------------
// Anti-entropy — on_tick re-issues membership and chat pulls to connected peers
// ---------------------------------------------------------------------------

#[test]
fn tick_reissues_membership_and_chat_pulls() {
    let built = build_log(2, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    let _ = engine.on_connect(NODE_A);
    let tick_outs = engine.on_tick(1_000);

    let to_a: Vec<_> = tick_outs.iter().filter(|o| o.peer == NODE_A).collect();
    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::AdminTip { .. })),
        "tick must re-advertise AdminTip"
    );
    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantMembership { .. })),
        "tick must re-issue WantMembership anti-entropy pull"
    );
    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantRecentChat { .. })),
        "tick must re-issue WantRecentChat anti-entropy pull"
    );
}

// ---------------------------------------------------------------------------
// Config gate — SyncEngine::open propagates invalid-config errors
// ---------------------------------------------------------------------------

#[test]
fn open_rejects_invalid_config() {
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let bad = SyncConfig {
        max_parked_total: 0,
        ..SyncConfig::default()
    };
    let Err(e) = SyncEngine::open(store, built.room, bad) else {
        panic!("SyncEngine::open must reject a zero park cap");
    };
    assert!(
        e.to_string().contains("park_cap_zero"),
        "error code must name the violated bound"
    );
}

// ---------------------------------------------------------------------------
// Responder — WantEvents for unknown ids returns NotFound
// ---------------------------------------------------------------------------

#[test]
fn want_events_returns_not_found_for_missing_ids() {
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    let phantom = EventId::from_bytes([0xDE; 32]);
    let outs = engine.on_message(
        NODE_A,
        SyncMessage::WantEvents {
            room_id: built.room,
            ids: vec![phantom],
        },
    );

    assert!(
        outs.iter().any(|o| matches!(&o.msg,
            SyncMessage::NotFound { ids, .. } if ids.contains(&phantom)
        )),
        "WantEvents for a missing id must elicit a NotFound response"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — global park cap evicts oldest across authors
// ---------------------------------------------------------------------------

#[test]
fn global_park_cap_evicts_oldest_across_authors() {
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4)
    let built = build_log(0, false);
    // Global cap of 1 with a generous per-author cap so only the total cap fires.
    let config = SyncConfig {
        max_parked_total: 1,
        max_parked_per_author: 64,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, config).expect("engine");
    engine.publish(&built.events[0]).expect("genesis");

    // First orphan: inv_carol (alice, parent=inv_bob_eid missing) → parked.
    let _ = engine.ingest_frame(NODE_A, &built.events[3]);
    assert_eq!(engine.parked_len(), 1);
    assert_eq!(engine.counters().park_evicted, 0);

    // Second orphan: join_carol (carol, MemberJoined → always signer-plausible,
    // parent=inv_carol_eid missing). Global cap is 1, so the oldest frame
    // (inv_carol) is evicted and join_carol takes the single slot.
    let _ = engine.ingest_frame(NODE_A, &built.events[4]);
    assert_eq!(engine.parked_len(), 1, "park held at global cap");
    assert_eq!(
        engine.counters().park_evicted,
        1,
        "oldest evicted by max_parked_total"
    );
    assert!(
        engine.logs().iter().any(|l| l.contains("max_parked_total")),
        "global cap eviction must be logged as max_parked_total"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — phantom-parent depth gate drops deep backfill chains
// ---------------------------------------------------------------------------

#[test]
fn phantom_depth_drop_gates_backfill_chain() {
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4)
    let built = build_log(0, false);
    // max_backfill_depth = 0: the initial orphan (depth 0) passes the check
    // `0 > 0 = false`, but any event registered as a backfill target (depth 1)
    // that is itself still an orphan is dropped as a phantom chain.
    let config = SyncConfig {
        max_backfill_depth: 0,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, config).expect("engine");
    engine.publish(&built.events[0]).expect("genesis");
    let _ = engine.on_connect(NODE_A);

    // Deliver join_carol (index 4, parent=inv_carol_eid missing) → depth 0 passes.
    // The engine parks join_carol and registers inv_carol_eid at backfill depth 1.
    let _ = engine.ingest_frame(NODE_A, &built.events[4]);
    assert_eq!(engine.parked_len(), 1, "join_carol parked at depth 0");
    assert!(
        engine.counters().backfill_requests >= 1,
        "WantEvents emitted for inv_carol"
    );

    // Deliver inv_carol (index 3) in answer to the WantEvents.  inv_carol's id
    // was registered at depth 1 and its own parent (inv_bob_eid) is missing,
    // so `1 > max_backfill_depth(0)` triggers phantom_depth_dropped.
    let _ = engine.ingest_frame(NODE_A, &built.events[3]);
    assert_eq!(
        engine.counters().phantom_depth_dropped,
        1,
        "inv_carol dropped: phantom_parent_depth"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("phantom_parent_depth")),
        "phantom depth drop must be logged"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — tick anti-entropy pulls are independent of backfill tokens
// ---------------------------------------------------------------------------

#[test]
fn tick_anti_entropy_emitted_independently_of_backfill_token_state() {
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4) remove_carol(5)
    //
    // The WantMembership / WantRecentChat anti-entropy pulls emitted on tick are
    // the actual post-exhaustion recovery path: retry_park skips parked events
    // because they are not in the store (only fold-accepted events are stored),
    // so the tick's unconditional membership + chat pulls close the gap.
    let built = build_log(0, true);
    let config = SyncConfig {
        backfill_tokens_per_author: 1,
        backfill_refill_per_tick: 1,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, config).expect("engine");
    engine.publish(&built.events[0]).expect("genesis");
    let _ = engine.on_connect(NODE_A);

    // Exhaust alice's token: first orphan takes it, second is rate-limited.
    let _ = engine.ingest_frame(NODE_A, &built.events[3]); // inv_carol → token taken
    let _ = engine.ingest_frame(NODE_A, &built.events[5]); // remove_carol → rate-limited
    assert_eq!(
        engine.counters().backfill_rate_limited,
        1,
        "second orphan rate-limited"
    );
    let rate_limited_before = engine.counters().backfill_rate_limited;

    // Tick: refills tokens AND unconditionally emits WantMembership + WantRecentChat
    // anti-entropy pulls to every connected peer.  retry_park finds no store entries
    // for parked events (they are held in memory only), so it emits nothing and
    // does not consume the refilled token.
    let tick_outs = engine.on_tick(1_000);
    let to_a: Vec<_> = tick_outs.iter().filter(|o| o.peer == NODE_A).collect();

    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantMembership { .. })),
        "tick must emit WantMembership pull even after token exhaustion"
    );
    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantRecentChat { .. })),
        "tick must emit WantRecentChat pull even after token exhaustion"
    );
    // retry_park finds nothing in the store for the parked events → no new
    // suppressions; the rate-limited counter stays frozen.
    assert_eq!(
        engine.counters().backfill_rate_limited,
        rate_limited_before,
        "retry_park must not add new suppressions when missing_parents is empty"
    );
}

// ---------------------------------------------------------------------------
// Connectivity — on_tick with no connected peers produces no output
// ---------------------------------------------------------------------------

#[test]
fn on_tick_with_no_peers_returns_empty() {
    let built = build_log(2, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    // No peers are connected: tick must produce no outbound messages.
    let outs = engine.on_tick(1_000);
    assert!(
        outs.is_empty(),
        "tick with no connected peers produces no output"
    );
}

// ---------------------------------------------------------------------------
// Config gate — additional invalid-config codes are propagated
// ---------------------------------------------------------------------------

#[test]
fn open_rejects_fanout_cap_zero() {
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let bad = SyncConfig {
        max_backfill_fanout_ids: 0,
        ..SyncConfig::default()
    };
    let Err(e) = SyncEngine::open(store, built.room, bad) else {
        panic!("SyncEngine::open must reject max_backfill_fanout_ids=0");
    };
    assert!(
        e.to_string().contains("fanout_cap_zero"),
        "error code must be fanout_cap_zero, got: {e}"
    );
}

#[test]
fn open_rejects_chat_window_zero() {
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let bad = SyncConfig {
        chat_window_default: 0,
        ..SyncConfig::default()
    };
    let Err(e) = SyncEngine::open(store, built.room, bad) else {
        panic!("SyncEngine::open must reject chat_window_default=0");
    };
    assert!(
        e.to_string().contains("chat_window_zero"),
        "error code must be chat_window_zero, got: {e}"
    );
}

// ---------------------------------------------------------------------------
// Connectivity — SimNet partition + reconnect converges
// ---------------------------------------------------------------------------

#[test]
fn partition_then_reconnect_converges() {
    let built = build_log(2, true);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // A holds the full log; B is empty.
    seed(&mut net, NODE_A, &built.events);
    net.connect(NODE_A, NODE_B);

    // Drain a few steps (partial sync), then partition before quiescence.
    for _ in 0..3 {
        net.step();
    }
    net.partition(&[NODE_A], &[NODE_B]);

    // In-flight frames for the disconnected peer are dropped.  Re-establish
    // the link and finish: the engine retains its orphan park across disconnect
    // so backfill can complete on reconnect.
    net.reconnect(NODE_A, NODE_B);
    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "B converged on carol's removal after partition+reconnect"
    );
    assert_eq!(net.engine(NODE_B).completeness(), Completeness::Complete);
}

// ---------------------------------------------------------------------------
// Protocol spec — advisory since_ms filter in WantRecentChat (R8 / §2.3)
// ---------------------------------------------------------------------------

#[test]
fn want_recent_chat_since_ms_advisory_filters_events() {
    // build_log(2, false): 5 membership events + 2 chat messages from bob.
    let built = build_log(2, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    // Advisory since_ms = u64::MAX: all events have created_at << MAX,
    // so the time filter excludes every chat event → no Events response.
    let outs_none = engine.on_message(
        NODE_A,
        SyncMessage::WantRecentChat {
            room_id: built.room,
            window: Window {
                max_count: 200,
                since_ms: Some(u64::MAX),
            },
            have: vec![],
        },
    );
    assert!(
        outs_none.is_empty(),
        "advisory since_ms=MAX must exclude all chat events; got: {outs_none:?}"
    );

    // Advisory since_ms = 0: created_at >= 0 is always true → both chat events
    // are returned in one Events message.
    let outs_all = engine.on_message(
        NODE_A,
        SyncMessage::WantRecentChat {
            room_id: built.room,
            window: Window {
                max_count: 200,
                since_ms: Some(0),
            },
            have: vec![],
        },
    );
    let events_msgs: Vec<_> = outs_all
        .iter()
        .filter(|o| matches!(&o.msg, SyncMessage::Events { .. }))
        .collect();
    assert_eq!(events_msgs.len(), 1, "one Events response for since_ms=0");
    if let SyncMessage::Events { frames, .. } = &events_msgs[0].msg {
        assert_eq!(frames.len(), 2, "both chat events returned when since_ms=0");
    }
}
