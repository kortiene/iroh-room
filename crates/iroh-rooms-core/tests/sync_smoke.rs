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
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
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

/// Recompute the dedup `EventId` of a raw wire frame the same way the validator
/// does (decode transport, hash the canonical signed bytes) — used by the
/// `take_ingested` tests to identify which `StoredEvent` came out of a drain.
fn frame_event_id(bytes: &[u8]) -> EventId {
    let wire = WireEvent::decode(bytes).expect("decode wire frame");
    signed::event_id_from_bytes(&wire.signed)
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
// Security — an unconfirmable admin tip expires and FAILS OPEN, loudly (fix 2)
// ---------------------------------------------------------------------------

// A fabricated higher admin tip that can never be backfilled is expired by the
// attempt budget so it cannot pin the node fail-closed forever (spec §13). That
// expiry *fails the removal-sensitive gate open* — a security-relevant
// transition — so it must not be silent: it surfaces a CRITICAL
// `admin_tip_expired` decision plus a counter, mirroring how #119 surfaces
// store_degraded.
#[test]
fn stale_admin_tip_expiry_fails_open_and_records_critical() {
    let built = build_log(0, true);
    // A tiny attempt budget so the fabricated tip reaches expiry in a few ticks
    // (the default value is unchanged; only this test drives a small bound).
    let cfg = SyncConfig {
        max_unconfirmed_tip_attempts: 2,
        ..SyncConfig::default()
    };
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, cfg).expect("engine");

    // Seed everything except the removal: the node holds carol as an active
    // member and believes its admin view is complete.
    for f in &built.events[..built.events.len() - 1] {
        engine.publish(f).expect("seed prefix");
    }
    assert_eq!(engine.completeness(), Completeness::Complete);
    assert!(engine.snapshot().is_active(&built.carol.identity()));

    // A peer advertises a higher admin tip whose event can never be backfilled
    // (a fabricated id this node will never hold).
    let fake_tip = EventId::from_bytes([0x77; 32]);
    let fake_seq = 999;
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((fake_tip, fake_seq)),
        },
    );

    // While the tip is suspect the node fails CLOSED on carol (correct posture).
    assert_eq!(engine.completeness(), Completeness::AdminViewSuspect);
    assert!(
        engine
            .fail_closed_subjects()
            .contains(&built.carol.identity()),
        "carol is fail-closed while the tip is unconfirmed"
    );
    assert_eq!(engine.counters().suspect_tip_expired, 0);
    assert!(
        !engine
            .trust_decisions()
            .iter()
            .any(|d| d.code == "admin_tip_expired"),
        "no fail-open recorded before the budget is spent"
    );

    // Tick the attempt budget down: 2 -> 1 -> 0 -> expire on the third tick. The
    // injected `now_ms` is advisory only (never gates the expiry), so the exact
    // values do not matter, only that three ticks elapse.
    for t in (T0 + 1..).take(3) {
        let _ = engine.on_tick(t);
    }

    // The fail-OPEN is now loud: the suspicion cleared (carol is trusted again
    // despite the removal we never confirmed), and the transition is surfaced.
    assert_eq!(
        engine.completeness(),
        Completeness::Complete,
        "the attempt budget expired the unconfirmed suspicion"
    );
    assert!(
        !engine
            .fail_closed_subjects()
            .contains(&built.carol.identity()),
        "expiry fails OPEN: carol is trusted again despite the unconfirmed removal"
    );
    assert_eq!(engine.counters().suspect_tip_expired, 1);
    let decisions = engine.trust_decisions();
    assert!(
        decisions.iter().any(|d| d.code == "admin_tip_expired"
            && d.severity == Severity::Critical
            && d.admin_seq == fake_seq
            && d.event_ids == vec![fake_tip]),
        "the fail-open must surface a CRITICAL admin_tip_expired naming the tip; got {decisions:?}"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("admin_tip_unconfirmed")),
        "the expiry must be logged"
    );
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

#[test]
fn want_events_cannot_serve_an_id_from_another_room_in_the_shared_store() {
    let built = build_log(0, false);
    let foreign_admin = Principal::new(0x55);
    let foreign_nonce = [0xcd; 16];
    let foreign_room = signed::derive_room_id(&foreign_admin.identity(), &foreign_nonce, T0 + 100);
    let foreign_genesis = SignedEvent {
        schema_version: 1,
        room_id: foreign_room,
        sender_id: foreign_admin.identity(),
        device_id: foreign_admin.device(),
        event_type: iroh_rooms_core::event::content::EventType::RoomCreated,
        created_at: T0 + 100,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Foreign Room".to_owned(),
            room_nonce: foreign_nonce,
            admins: vec![foreign_admin.identity()],
            device_binding: DeviceBinding::create(
                &foreign_room,
                &foreign_admin.id,
                foreign_admin.device(),
            ),
        }),
    };
    let foreign_frame = wire(&foreign_genesis, &foreign_admin.dev);
    let foreign_validated =
        validate_wire_bytes(&foreign_frame, &ValidationContext::for_room(foreign_room))
            .expect("foreign genesis validates");
    let foreign_id = foreign_validated.event_id;

    let mut store = EventStore::open_in_memory().expect("store");
    store.insert(&foreign_validated).expect("seed foreign room");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed local room");
    }

    let outs = engine.on_message(
        NODE_A,
        SyncMessage::WantEvents {
            room_id: built.room,
            ids: vec![foreign_id],
        },
    );

    assert!(
        !outs.iter().any(|out| matches!(
            &out.msg,
            SyncMessage::Events { frames, .. } if !frames.is_empty()
        )),
        "a room-scoped engine must never serve foreign-room bytes"
    );
    assert!(
        outs.iter().any(|out| matches!(
            &out.msg,
            SyncMessage::NotFound { room_id, ids }
                if room_id == &built.room && ids == &vec![foreign_id]
        )),
        "the foreign id is indistinguishable from an unknown id in this room"
    );

    // A foreign row must not satisfy an in-room causal dependency either. The
    // otherwise-valid local message is parked and requests its opaque parent;
    // it is never treated as causally complete merely because another room has
    // a row with that id in the shared database.
    let cross_room_parent = SignedEvent {
        schema_version: 1,
        room_id: built.room,
        sender_id: built.bob.identity(),
        device_id: built.bob.device(),
        event_type: iroh_rooms_core::event::content::EventType::MessageText,
        created_at: T0 + 200,
        prev_events: vec![foreign_id],
        content: Content::MessageText(MessageText {
            body: "must remain parked".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let _ = engine.on_connect(NODE_A);
    let backfill = engine.ingest_frame(NODE_A, &wire(&cross_room_parent, &built.bob.dev));
    assert_eq!(engine.parked_len(), 1);
    assert!(
        backfill.iter().any(|out| matches!(
            &out.msg,
            SyncMessage::WantEvents { room_id, ids }
                if room_id == &built.room && ids.contains(&foreign_id)
        )),
        "the foreign row must remain a missing parent in the local room"
    );

    // A peer-advertised admin tip is also room-scoped. A row for the same id
    // in another room cannot clear the fail-closed suspect state.
    let local_seq = engine
        .digest()
        .expect("local digest")
        .admin_tip
        .expect("local admin tip")
        .1;
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((foreign_id, local_seq + 1)),
        },
    );
    assert_eq!(engine.completeness(), Completeness::AdminViewSuspect);
}

// ---------------------------------------------------------------------------
// IR-0105 AC2 — duplicate message.text delivery is silently ignored
// ---------------------------------------------------------------------------

#[test]
fn message_text_duplicate_delivery_is_silently_ignored() {
    // build_log(1, false): 5 membership events + 1 MessageText from bob (events[5]).
    let built = build_log(1, false);
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));

    // Seed all events so the first delivery of the chat frame is accepted.
    seed(&mut net, NODE_A, &built.events);

    let chat_frame = built.events.last().expect("has chat event");
    let accepted_before = net.engine(NODE_A).counters().accepted;

    // Deliver the same message.text frame a second time as if from NODE_B.
    let outs = net.engine_mut(NODE_A).ingest_frame(NODE_B, chat_frame);

    let rebroadcast: Vec<_> = outs
        .iter()
        .filter(|o| matches!(&o.msg, SyncMessage::Events { .. }))
        .collect();
    assert!(
        rebroadcast.is_empty(),
        "duplicate message.text must not produce an Events fan-out"
    );
    assert_eq!(
        net.engine(NODE_A).counters().accepted,
        accepted_before,
        "duplicate message.text delivery must not increment the accepted counter"
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

    // fix 2: an evicted-and-never-re-served frame is permanently lost, so the
    // overflow is surfaced (not a silent drop) — a CRITICAL `park_overflow`
    // decision names the evicted event.
    let inv_carol_id = frame_event_id(&built.events[3]);
    let decisions = engine.trust_decisions();
    assert!(
        decisions.iter().any(|d| d.code == "park_overflow"
            && d.severity == Severity::Critical
            && d.event_ids == vec![inv_carol_id]),
        "the max_parked_total eviction must surface a CRITICAL decision; got {decisions:?}"
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

    // fix 2: the drop is a *permanent* loss (unrecoverable through backfill), so
    // it is not silent — a CRITICAL `backfill_depth_exceeded` decision names the
    // lost event, exactly as #119 surfaces store_degraded.
    let inv_carol_id = frame_event_id(&built.events[3]);
    let decisions = engine.trust_decisions();
    assert!(
        decisions.iter().any(|d| d.code == "backfill_depth_exceeded"
            && d.severity == Severity::Critical
            && d.event_ids == vec![inv_carol_id]),
        "the phantom-depth drop must surface a CRITICAL decision; got {decisions:?}"
    );
}

// ---------------------------------------------------------------------------
// Anti-amplification — tick anti-entropy pulls are independent of backfill tokens
// ---------------------------------------------------------------------------

#[test]
fn tick_anti_entropy_emitted_independently_of_backfill_token_state() {
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4) remove_carol(5)
    //
    // On tick the engine unconditionally emits WantMembership / WantRecentChat
    // anti-entropy pulls to every connected peer, regardless of the per-author
    // backfill token state — token exhaustion must never stall the anti-entropy
    // recovery path.
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

    // Tick: refills tokens AND unconditionally emits WantMembership + WantRecentChat
    // anti-entropy pulls to every connected peer — independent of token state.
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
    // retry_park now re-drives the parked frames from their recorded missing sets
    // (issue #114): the one refilled token is spent chasing the first parked
    // frame's missing parent, and the second parked frame is rate-limited again —
    // so the by-id backfill retry actually makes progress rather than silently
    // doing nothing for events that are (correctly) not in the store.
    assert_eq!(
        engine.counters().backfill_rate_limited,
        2,
        "retry_park re-drives parked frames and rate-limits once the refill is spent"
    );
    assert!(
        to_a.iter()
            .any(|o| matches!(&o.msg, SyncMessage::WantEvents { .. })),
        "retry_park re-issues a by-id backfill for a still-missing parent"
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

// ---------------------------------------------------------------------------
// SyncEngine::room_tail passthrough — ordering, limit enforcement, empty store
// ---------------------------------------------------------------------------

#[test]
fn room_tail_returns_all_events_in_order_when_no_limit() {
    // build_log(3, false): genesis + 4 membership events + 3 chat = 8 events.
    let built = build_log(3, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }
    let tail = engine.room_tail(u32::MAX).expect("room_tail must succeed");
    assert_eq!(
        tail.len(),
        built.events.len(),
        "room_tail(MAX) must return all {} seeded events",
        built.events.len()
    );
    // Verify ascending (lamport, event_id) order that store guarantees.
    for pair in tail.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let la = a.lamport.unwrap_or(u64::MAX);
        let lb = b.lamport.unwrap_or(u64::MAX);
        assert!(
            la < lb || (la == lb && a.event_id <= b.event_id),
            "tail must be ascending by (lamport, event_id): ({la}, {:?}) then ({lb}, {:?})",
            a.event_id,
            b.event_id
        );
    }
}

#[test]
fn room_tail_truncates_to_limit() {
    // build_log(4, false): 5 membership + 4 chat = 9 events.
    let built = build_log(4, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }
    // Limit smaller than total: must return exactly `limit` events.
    let tail = engine.room_tail(3).expect("room_tail must succeed");
    assert_eq!(
        tail.len(),
        3,
        "room_tail(3) must return exactly 3 of the 9 events"
    );
    // Limit larger than total: must return all events.
    let tail_all = engine.room_tail(u32::MAX).expect("room_tail must succeed");
    assert_eq!(tail_all.len(), 9, "room_tail(MAX) must return all 9 events");
}

#[test]
fn room_tail_returns_empty_on_fresh_engine() {
    // An engine opened over an empty store must return an empty tail.
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    let tail = engine.room_tail(u32::MAX).expect("room_tail must succeed");
    assert!(
        tail.is_empty(),
        "fresh engine with no seeded events must return an empty tail"
    );
}

// ---------------------------------------------------------------------------
// `take_ingested` — the push-subscription feed (issue #83 / IR-0307)
// ---------------------------------------------------------------------------

#[test]
fn take_ingested_emits_on_own_publish() {
    let built = build_log(2, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    for frame in &built.events {
        engine.publish(frame).expect("seed publish");
    }

    let ingested = engine.take_ingested();
    assert_eq!(
        ingested.len(),
        built.events.len(),
        "every accepted event must appear exactly once"
    );
    // publish order == emission order when nothing parks.
    for (se, frame) in ingested.iter().zip(built.events.iter()) {
        assert_eq!(
            se.event_id,
            frame_event_id(frame),
            "take_ingested must preserve publish order"
        );
    }
}

#[test]
fn take_ingested_skips_duplicates() {
    let built = build_log(0, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    engine.publish(&built.events[0]).expect("publish genesis");
    assert_eq!(engine.take_ingested().len(), 1, "genesis emits once");

    // Re-deliver the already-stored genesis frame: a Duplicate re-see must never
    // reach the emit point (exactly-once).
    engine
        .publish(&built.events[0])
        .expect("re-publish genesis");
    assert!(
        engine.take_ingested().is_empty(),
        "a duplicate re-see must not re-emit"
    );
}

#[test]
fn take_ingested_is_destructive() {
    let built = build_log(0, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    engine.publish(&built.events[0]).expect("publish genesis");
    let first = engine.take_ingested();
    assert_eq!(first.len(), 1, "first drain gets the genesis event");

    engine
        .publish(&built.events[1])
        .expect("publish invite_bob");
    let second = engine.take_ingested();
    assert_eq!(
        second.len(),
        1,
        "second drain must contain only events accepted since the first drain"
    );

    assert!(
        engine.take_ingested().is_empty(),
        "a third drain with nothing new accepted must be empty"
    );
}

#[test]
fn take_ingested_emits_on_park_promotion() {
    let built = build_log(0, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    // Genesis must be pre-published so alice is already a known admin: only then
    // does the admin-authored `inv_carol` (parked below) survive the §6.2
    // signer-plausibility pre-gate instead of being dropped outright.
    engine.publish(&built.events[0]).expect("publish genesis");
    let _ = engine.take_ingested();

    // Deliver inv_carol (admin event, parent = inv_bob) before inv_bob lands: it
    // must park, not drop or insert.
    let inv_carol_id = frame_event_id(&built.events[3]);
    engine
        .publish(&built.events[3])
        .expect("publish inv_carol (parks)");
    assert_eq!(engine.parked_len(), 1, "inv_carol buffered pending inv_bob");
    assert!(
        engine.take_ingested().is_empty(),
        "nothing is inserted while a frame is only parked"
    );

    // Deliver inv_bob: directly accepted (its parent, genesis, is present) and
    // triggers wake_park, which promotes the parked inv_carol in the same drive.
    let inv_bob_id = frame_event_id(&built.events[1]);
    engine.publish(&built.events[1]).expect("publish inv_bob");

    assert_eq!(engine.parked_len(), 0, "park fully drained by wake_park");
    let ingested = engine.take_ingested();
    assert_eq!(
        ingested.len(),
        2,
        "both the trigger and its promoted descendant must appear, exactly once"
    );
    // Set-membership + exactly-once hold; strict causal ordering does not (§6.2).
    let ids: std::collections::BTreeSet<_> = ingested.iter().map(|se| se.event_id).collect();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([inv_bob_id, inv_carol_id]),
        "both events must appear as a set"
    );
    assert_eq!(
        ingested[0].event_id, inv_bob_id,
        "the directly-accepted trigger must be recorded first"
    );
}

#[test]
fn take_ingested_emits_on_peer_sync() {
    // AC-1 second ingest path: the existing `take_ingested_*` tests all drive
    // `publish` (`from = None`); this proves an event arriving over the wire via
    // `ingest_frame` (`from = Some(peer)`) is surfaced too — the daemon's actual
    // motivation, a peer's message landing on the push feed as it is ingested.
    // build_log(1, false): 5 membership events + 1 MessageText from bob (events[5]).
    let built = build_log(1, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    // Seed the 5 membership events via own-publish so bob is an active member,
    // then drain — we assert only on the frame that arrives from a peer next.
    for frame in &built.events[..5] {
        engine.publish(frame).expect("seed membership");
    }
    let _ = engine.take_ingested();

    // Bob's chat arrives over the wire; its parent (join_bob) is already present,
    // so it is directly Accepted → inserted → emitted on the ingest feed.
    let chat = &built.events[5];
    let chat_id = frame_event_id(chat);
    let _ = engine.ingest_frame(NODE_B, chat);

    let ingested = engine.take_ingested();
    assert_eq!(
        ingested.len(),
        1,
        "the peer-delivered event is emitted exactly once"
    );
    assert_eq!(
        ingested[0].event_id, chat_id,
        "take_ingested must surface the event ingested over the wire (peer-sync path)"
    );
}

#[test]
fn take_ingested_skips_peer_delivered_duplicate() {
    // Exactly-once across the peer path: a peer re-delivering an event the store
    // already holds takes the `InsertOutcome::Duplicate` arm, which never reaches
    // the emit point. This is the concrete de-dupe the issue calls out (peers
    // re-advertising), complementing `take_ingested_skips_duplicates` (re-publish).
    let built = build_log(0, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    engine.publish(&built.events[0]).expect("publish genesis");
    engine
        .publish(&built.events[1])
        .expect("publish invite_bob");
    let _ = engine.take_ingested();

    let dups_before = engine.counters().duplicates;
    let _ = engine.ingest_frame(NODE_B, &built.events[1]);

    assert!(
        engine.take_ingested().is_empty(),
        "a peer re-delivering a stored event must not re-emit"
    );
    assert_eq!(
        engine.counters().duplicates,
        dups_before + 1,
        "the re-see took the Duplicate arm (exactly-once for free)"
    );
}

#[test]
fn take_ingested_never_emits_a_dropped_frame() {
    // Security (§10): "no event that fails validation, is parked, or is rejected
    // ever reaches a subscriber." Here bob's chat is delivered with only genesis
    // seeded, so bob is not yet a member and the frame (parent join_bob missing →
    // would_buffer) is dropped at the anti-amplification signer pre-gate — never
    // folded, never stored, and therefore never emitted on the ingest feed.
    // build_log(1, false): events[5] is bob's chat.
    let built = build_log(1, false);
    let mut engine = fresh_engine(built.room, SyncConfig::default());

    engine.publish(&built.events[0]).expect("seed genesis");
    let _ = engine.take_ingested();

    let chat = &built.events[built.events.len() - 1];
    let _ = engine.ingest_frame(NODE_A, chat);

    assert_eq!(
        engine.counters().signer_dropped,
        1,
        "chat from a non-member is dropped at the signer pre-gate"
    );
    assert!(
        engine.take_ingested().is_empty(),
        "a dropped frame must never appear on the ingest feed"
    );
}

// ---------------------------------------------------------------------------
// Issue #112 — the join-bootstrap capability-proof verifier
// ---------------------------------------------------------------------------

#[test]
fn capability_proof_matches_only_a_held_invite_secret() {
    // build_log mints bob's invite with invite_id [0x01; 16] + secret [0x41; 16].
    let built = build_log(0, false);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    engine.publish(&built.events[0]).expect("genesis");
    engine.publish(&built.events[1]).expect("invite");

    let inv_bob_id = [0x01u8; 16];
    let inv_bob_secret = [0x41u8; 16];

    assert!(
        engine.capability_proof_matches(&inv_bob_id, &inv_bob_secret),
        "the correct secret for a held invite must prove possession"
    );
    assert!(
        !engine.capability_proof_matches(&inv_bob_id, &[0xFFu8; 16]),
        "a wrong secret must not prove possession"
    );
    assert!(
        !engine.capability_proof_matches(&[0x09u8; 16], &inv_bob_secret),
        "an unknown invite id must not prove possession"
    );
    // A fresh engine that holds no invite proves nothing.
    let empty_store = EventStore::open_in_memory().expect("store");
    let empty = SyncEngine::open(empty_store, built.room, SyncConfig::default()).expect("engine");
    assert!(
        !empty.capability_proof_matches(&inv_bob_id, &inv_bob_secret),
        "an engine with no on-log invite proves nothing"
    );
}
