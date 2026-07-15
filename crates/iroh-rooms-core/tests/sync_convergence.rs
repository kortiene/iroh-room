//! Exhaustive §8 convergence matrix for the bounded recent-sync engine (IR-0007).
//!
//! Covers the scenarios that `sync_smoke.rs` explicitly defers to this file:
//!
//! * **§8.2 / §9 vector** — multi-level DAG delivered in fully reversed causal order
//! * **§8.2** — new events published mid-sync reach a latecomer
//! * **§8.2** — membership backfill across invite+join+remove sequence (offline peer)
//! * **§8.2 / §13 vector** — non-member flood: `DoS` amplification guard
//! * **§8.2** — idempotency: 1000× replay does not change state
//! * **§8.4 guard #1** — same event set → byte-identical `SyncDigest` across 20 shuffle seeds
//! * **§8.4 guard #2** — identical engines → identical `on_tick` output (no hidden nondeterminism)
//! * **Five-peer full mesh** — the ≤5-peer MVP room target (spec §2 / A2)
//! * **Never-windowed invariant** — `WantMembership` never returns chat-class events
//! * **Response-cap chunking** — small `response_max_frames` cap + eventual convergence via retry

#![cfg(feature = "sync")]
#![allow(clippy::similar_names, clippy::too_many_lines)]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MemberRemoved, MessageText,
    RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::Status;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::sim::SimNet;
use iroh_rooms_core::sync::{Completeness, PeerId, SyncConfig, SyncEngine, SyncMessage};

// ---------------------------------------------------------------------------
// Shared fixtures (duplicated from sync_smoke; Rust integration-test binaries
// cannot import from each other)
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xAB; 16];
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

fn wire_bytes(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// A built room log: causally-ordered wire frames + the cast for assertions.
struct Built {
    room: RoomId,
    /// Causally-ordered wire frames; removal is last when `with_removal`.
    events: Vec<Vec<u8>>,
    #[allow(dead_code)]
    alice: Principal,
    bob: Principal,
    carol: Principal,
}

/// Build `genesis → invite_bob → join_bob → invite_carol → join_carol →
/// {n_chat bob messages} → [remove_carol]`.
///
/// Admin events (alice) cite the prior admin event so `admin_seq` flows; chat is
/// authored by bob and parented on his join, so it is chat-class (windowable).
fn build_log(n_chat: u32, with_removal: bool) -> Built {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut events = Vec::new();
    let mut t = T0;

    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::RoomCreated,
        created_at: t,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Convergence Tests".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
        }),
    };
    let gid = genesis.event_id();
    events.push(wire_bytes(&genesis, &alice.dev));

    t += 1;
    let inv_bob_id = [0x01; 16];
    let inv_bob_sec = [0x41; 16];
    let inv_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::MemberInvited,
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
    events.push(wire_bytes(&inv_bob, &alice.dev));

    t += 1;
    let join_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: bob.identity(),
        device_id: bob.device(),
        event_type: EventType::MemberJoined,
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
    events.push(wire_bytes(&join_bob, &bob.dev));

    t += 1;
    let inv_carol_id = [0x02; 16];
    let inv_carol_sec = [0x42; 16];
    let inv_carol = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::MemberInvited,
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
    events.push(wire_bytes(&inv_carol, &alice.dev));

    t += 1;
    let join_carol = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: carol.identity(),
        device_id: carol.device(),
        event_type: EventType::MemberJoined,
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
    events.push(wire_bytes(&join_carol, &carol.dev));

    for i in 0..n_chat {
        t += 1;
        let msg = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: bob.identity(),
            device_id: bob.device(),
            event_type: EventType::MessageText,
            created_at: t,
            prev_events: vec![join_bob_eid],
            content: Content::MessageText(MessageText {
                body: format!("conv msg {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        events.push(wire_bytes(&msg, &bob.dev));
    }

    if with_removal {
        t += 1;
        let remove = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::MemberRemoved,
            created_at: t,
            prev_events: vec![join_carol_eid, inv_carol_eid],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: carol.identity(),
                removed_by: alice.identity(),
                reason: None,
                device_binding: None,
            }),
        };
        events.push(wire_bytes(&remove, &alice.dev));
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

/// Seed a peer's engine directly (no `SimNet` queue, no fan-out) with a slice of the
/// log in causal order via `publish`. Used for pre-populating a peer before
/// connecting the mesh.
fn seed(net: &mut SimNet, peer: PeerId, frames: &[Vec<u8>]) {
    for f in frames {
        net.engine_mut(peer).publish(f).expect("seed publish");
    }
}

// ---------------------------------------------------------------------------
// §8.2 / §9 vector — multi-level DAG delivered in fully reversed causal order
// ---------------------------------------------------------------------------

/// §9 protocol test vector: every event in the causal chain is delivered to B in
/// reverse order. B starts with genesis (so alice is plausible as admin in the
/// §6.2 signer pre-check). Events 1–5 are each buffered when they arrive; when
/// `inv_bob` — the one event whose parent (genesis) is already in B's fold —
/// arrives last, `wake_park`'s cascade resolves the entire DAG in one pass.
#[test]
fn multi_level_dag_fully_reversed_converges() {
    let built = build_log(0, true);
    // layout: [genesis(0), inv_bob(1), join_bob(2), inv_carol(3), join_carol(4), remove_carol(5)]
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    seed(&mut net, NODE_A, &built.events);
    // B seeds only genesis so alice is known as admin before the reversed delivery.
    seed(&mut net, NODE_B, &built.events[..1]);

    // Connect so WantEvents backfill responses can be served if needed.
    net.connect(NODE_A, NODE_B);

    // Deliver events 1..=5 in fully reversed order: each is buffered (its parents
    // are missing at delivery time). The cascade in wake_park resolves everything
    // when inv_bob — whose parent is genesis — is delivered last.
    for frame in built.events[1..].iter().rev() {
        net.deliver_raw(NODE_B, NODE_A, frame);
    }

    net.run_to_quiescence();

    assert_eq!(
        net.engine(NODE_B).parked_len(),
        0,
        "all parked events resolved after reversed delivery"
    );
    net.assert_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "remove_carol resolved via cascade from fully-reversed delivery (§9 vector)"
    );
}

// ---------------------------------------------------------------------------
// §8.2 — new events published mid-sync reach the latecomer
// ---------------------------------------------------------------------------

/// A latecomer (B) connects to A while A holds only the 5 membership events.
/// A then publishes 5 chat events while the sync handshake is mid-flight.
/// The fan-out from A reaches B through the active link; B converges on all
/// events after quiescence.
#[test]
fn new_events_published_during_active_sync_reach_latecomer() {
    let built = build_log(5, false); // indices 0..4 = membership, 5..9 = chat
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // A starts with the 5 membership events only.
    seed(&mut net, NODE_A, &built.events[..5]);

    net.connect(NODE_A, NODE_B);

    // Partially drain — some handshake frames delivered but sync is not complete.
    for _ in 0..4 {
        net.step();
    }

    // A publishes the 5 chat events while the sync is mid-flight; these fan out to B.
    for frame in &built.events[5..] {
        net.publish(NODE_A, frame).expect("mid-sync publish");
    }

    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B]);
    assert!(
        net.engine(NODE_B)
            .snapshot()
            .is_active(&built.bob.identity()),
        "bob active on B after mid-sync publish convergence"
    );
}

// ---------------------------------------------------------------------------
// §8.2 — membership backfill across invite+join+remove sequence
// ---------------------------------------------------------------------------

/// B is offline while A processes the full `invite_bob → join_bob → invite_carol
/// → join_carol → remove_carol` sequence. After reconnect, `WantMembership` fully
/// reconciles the membership sub-DAG — no chat window applied.
#[test]
fn membership_backfill_across_invite_join_remove() {
    let built = build_log(0, true);
    // layout: genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4) remove_carol(5)
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // Both start with genesis only; then B goes offline.
    seed(&mut net, NODE_A, &built.events[..1]);
    seed(&mut net, NODE_B, &built.events[..1]);

    // A processes the full membership sequence while B is offline.
    seed(&mut net, NODE_A, &built.events[1..]);

    // B reconnects; the handshake issues WantMembership { have: [genesis_id] }.
    // A responds with the 5 missing membership events in causal order.
    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    net.assert_membership_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "carol's removal reconciled via WantMembership backfill"
    );
    assert!(
        net.engine(NODE_B)
            .snapshot()
            .is_active(&built.bob.identity()),
        "bob active after membership backfill"
    );
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::Complete,
        "completeness is Complete after full backfill"
    );
}

// ---------------------------------------------------------------------------
// §8.2 / §13 vector — non-member flood: DoS amplification guard
// ---------------------------------------------------------------------------

/// §13 / §8.2 anti-amplification scenario: a non-member sends 50 validly-signed
/// `MessageText` frames citing a phantom unknown parent. Every frame is dropped at
/// the §6.2 signer pre-check — none enter the park, none trigger a backfill
/// fan-out. The park bound is never approached.
#[test]
fn non_member_flood_is_dropped_and_park_stays_bounded() {
    const N_FLOOD: u64 = 50;

    let built = build_log(0, false);
    let evil = Principal::new(0xEE); // not invited, not in the membership snapshot

    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    // Seed genesis so alice is known as admin; evil is demonstrably not in the snapshot.
    engine.publish(&built.events[0]).expect("genesis");

    // A plausible-looking but absent parent id.
    let fake_parent = EventId::from_bytes([0xFA; 32]);

    for i in 0..N_FLOOD {
        let ev = SignedEvent {
            schema_version: 1,
            room_id: built.room,
            sender_id: evil.identity(),
            device_id: evil.device(),
            event_type: EventType::MessageText,
            created_at: T0 + i + 1,
            prev_events: vec![fake_parent],
            content: Content::MessageText(MessageText {
                body: format!("flood {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        let frame = wire_bytes(&ev, &evil.dev);
        let _ = engine.ingest_frame(NODE_A, &frame);
    }

    assert_eq!(
        engine.parked_len(),
        0,
        "non-member frames must not enter the park (§13 vector)"
    );
    // The frames are dropped *before* the fold ingests them, so the fold's node
    // map never grows: only genesis is tracked. This is the actual unbounded-growth
    // guard — asserting park_len alone would mask a leak that retains junk in the
    // fold forever (spec §6.2 step 1 / D5: "dropped early, never parked").
    assert_eq!(
        engine.fold_tracked_len(),
        1,
        "non-member junk must never enter the fold (only genesis is tracked)"
    );
    assert_eq!(
        engine.counters().signer_dropped,
        N_FLOOD,
        "all {N_FLOOD} flood frames dropped at signer pre-check"
    );
    assert_eq!(
        engine.counters().backfill_requests,
        0,
        "no backfill requests emitted for non-member junk"
    );
}

// ---------------------------------------------------------------------------
// §8.2 — idempotency: 1000× replay does not change state
// ---------------------------------------------------------------------------

/// §8.2 duplicate-idempotency vector (exhaustive form): every accepted frame is
/// replayed 1000 times. The `SyncDigest` is byte-identical to the post-seed state,
/// no fan-out storm occurs, and the duplicate counter accounts for every replay.
#[test]
fn idempotency_1000x_does_not_change_state() {
    let built = build_log(3, true);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("initial seed");
    }

    let digest_before = engine.digest().expect("digest before replays");
    let accepted_before = engine.counters().accepted;
    let n_events = u64::try_from(built.events.len()).expect("event count fits u64");

    for _ in 0..1000u32 {
        for frame in &built.events {
            let outs = engine.ingest_frame(NODE_A, frame);
            let fan_out: Vec<_> = outs
                .iter()
                .filter(|o| matches!(&o.msg, SyncMessage::Events { .. }))
                .collect();
            assert!(
                fan_out.is_empty(),
                "duplicate frame must not produce Events fan-out"
            );
        }
    }

    assert_eq!(
        engine.digest().expect("digest after replays"),
        digest_before,
        "digest must be byte-identical after 1000× replay"
    );
    assert_eq!(
        engine.counters().accepted,
        accepted_before,
        "no new events accepted on 1000× duplicate delivery"
    );
    assert_eq!(
        engine.counters().duplicates,
        n_events * 1000,
        "exactly n_events × 1000 duplicates recorded"
    );
}

// ---------------------------------------------------------------------------
// §8.4 guard #1 — same event set → byte-identical SyncDigest across 20 seeds
// ---------------------------------------------------------------------------

/// §8.4 determinism guard: the same validated event set, received in 20 different
/// random orderings, must produce a byte-identical `SyncDigest` on every peer that
/// holds it — the §0 same-set theorem end-to-end through the sync engine.
#[test]
fn same_set_across_many_shuffle_seeds_yields_identical_digest() {
    let built = build_log(4, true);

    // Reference: causal-order seed into a standalone engine.
    let ref_store = EventStore::open_in_memory().expect("ref store");
    let mut ref_engine =
        SyncEngine::open(ref_store, built.room, SyncConfig::default()).expect("ref engine");
    for frame in &built.events {
        ref_engine.publish(frame).expect("ref seed");
    }
    let reference = ref_engine.digest().expect("reference digest");

    // Fibonacci-derived seeds for diverse orderings without Math.random / wall clock.
    let seeds: [u64; 20] = [
        1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987, 1597, 2584, 4181, 6765, 10946,
    ];

    for seed_val in seeds {
        let mut net = SimNet::new(built.room);
        net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
        net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));
        seed(&mut net, NODE_A, &built.events);

        net.connect(NODE_A, NODE_B);
        net.shuffle(seed_val);
        net.run_to_quiescence();

        let digest_b = net.engine(NODE_B).digest().expect("digest");
        assert_eq!(
            digest_b, reference,
            "seed {seed_val}: digest must be byte-identical to causal-order reference"
        );
    }
}

// ---------------------------------------------------------------------------
// §8.4 guard #2 — identical engines → identical on_tick output
// ---------------------------------------------------------------------------

/// §8.4 determinism guard: two engines built from the same initial state and
/// connected to the same peer produce byte-for-byte identical `on_tick` outputs.
/// Confirms no hidden nondeterminism (`HashMap` iteration, PRNG, system state).
#[test]
fn on_tick_is_deterministic_across_identical_engines() {
    let built = build_log(2, false);

    let store1 = EventStore::open_in_memory().expect("store1");
    let mut engine1 = SyncEngine::open(store1, built.room, SyncConfig::default()).expect("engine1");
    let store2 = EventStore::open_in_memory().expect("store2");
    let mut engine2 = SyncEngine::open(store2, built.room, SyncConfig::default()).expect("engine2");

    for frame in &built.events {
        engine1.publish(frame).expect("seed1");
        engine2.publish(frame).expect("seed2");
    }

    // Connect both to NODE_A (add to the fan-out set).
    let _ = engine1.on_connect(NODE_A);
    let _ = engine2.on_connect(NODE_A);

    // Both are in identical state — on_tick must produce byte-identical Outgoing.
    let outs1 = engine1.on_tick(42_000);
    let outs2 = engine2.on_tick(42_000);

    assert_eq!(
        outs1, outs2,
        "identical engines must produce byte-identical on_tick output"
    );
    assert_eq!(
        engine1.digest().expect("digest1"),
        engine2.digest().expect("digest2"),
        "identical engines must have identical SyncDigests"
    );
}

// ---------------------------------------------------------------------------
// Five-peer full mesh — the ≤5-peer MVP room target (spec §2 / A2)
// ---------------------------------------------------------------------------

/// Five peers form a full mesh; only PEERS[0] starts with the full log. After
/// `connect_all` + `run_to_quiescence`, every peer must hold an identical
/// `SyncDigest` and `Completeness::Complete`.
#[test]
fn five_peer_full_mesh_converges() {
    const PEERS: [PeerId; 5] = [
        PeerId::from_bytes([0xF1; 32]),
        PeerId::from_bytes([0xF2; 32]),
        PeerId::from_bytes([0xF3; 32]),
        PeerId::from_bytes([0xF4; 32]),
        PeerId::from_bytes([0xF5; 32]),
    ];

    let built = build_log(5, true);
    let mut net = SimNet::new(built.room);

    for peer in PEERS {
        net.add_peer(peer, fresh_engine(built.room, SyncConfig::default()));
    }

    // Only PEERS[0] holds the full log; the rest are empty latecomers.
    seed(&mut net, PEERS[0], &built.events);

    net.connect_all();
    net.run_to_quiescence();

    net.assert_converged(&PEERS);
    for peer in PEERS {
        assert_eq!(
            net.engine(peer).completeness(),
            Completeness::Complete,
            "completeness must be Complete at peer {peer}"
        );
        assert_eq!(
            net.engine(peer).snapshot().status(&built.carol.identity()),
            Some(Status::Removed),
            "carol removed at peer {peer}"
        );
        assert!(
            net.engine(peer).snapshot().is_active(&built.bob.identity()),
            "bob active at peer {peer}"
        );
    }
}

// ---------------------------------------------------------------------------
// Never-windowed invariant — WantMembership must not return chat-class events
// ---------------------------------------------------------------------------

/// The §0/§4.1 never-windowed invariant at the message level: `WantMembership`
/// serves the authorization class **causally closed**. In this log the closure
/// equals the bare class — chat is a leaf, never a `prev_events` ancestor of a
/// membership event — so the response is exactly the 6 class events (genesis,
/// `inv_bob`, `join_bob`, `inv_carol`, `join_carol`, `remove_carol`) from a log
/// of 11, and no chat-class `MessageText` appears. Chat that IS membership
/// ancestry rides along by design; see
/// `want_membership_closure_includes_chat_ancestry`.
#[test]
fn want_membership_response_contains_only_auth_class_events() {
    let built = build_log(5, true); // 5 membership + 5 chat + 1 removal = 11 events
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");

    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    let outs = engine.on_message(
        NODE_A,
        SyncMessage::WantMembership {
            room_id: built.room,
            have: vec![], // request the complete authorization-class set
        },
    );

    let events_msgs: Vec<_> = outs
        .iter()
        .filter(|o| matches!(&o.msg, SyncMessage::Events { .. }))
        .collect();
    assert_eq!(
        events_msgs.len(),
        1,
        "WantMembership must produce exactly one Events response"
    );

    if let SyncMessage::Events { frames, .. } = &events_msgs[0].msg {
        // Authorization-class: genesis(RoomCreated) + inv_bob(MemberInvited) +
        // join_bob(MemberJoined) + inv_carol(MemberInvited) + join_carol(MemberJoined)
        // + remove_carol(MemberRemoved) = 6 events.
        assert_eq!(
            frames.len(),
            6,
            "WantMembership must return exactly the 6 authorization-class events; got {}",
            frames.len()
        );
        for (i, frame) in frames.iter().enumerate() {
            let wire_ev = WireEvent::decode(frame).expect("valid wire event");
            let signed_ev = SignedEvent::decode(&wire_ev.signed).expect("valid signed event");
            assert!(
                !matches!(signed_ev.event_type, EventType::MessageText),
                "frame {i}: WantMembership must never return MessageText (chat-class) events"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Join-after-history regression — an invite minted after chat must bootstrap
// ---------------------------------------------------------------------------

/// Build the production shape that used to deadlock joins: `genesis → inv_bob →
/// join_bob → chat_1 … chat_n (a LINEAR bob-authored chain, each message citing
/// the previous) → inv_carol citing the chat tip`. This is what `room invite`
/// mints once a conversation has started (`prev_events = current DAG heads`).
/// Carol has not joined; the log is what the admin holds when her bootstrap
/// begins.
fn build_late_invite_log(n_chat: u32) -> Built {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut events = Vec::new();
    let mut t = T0;

    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::RoomCreated,
        created_at: t,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Late Invite Tests".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
        }),
    };
    let gid = genesis.event_id();
    events.push(wire_bytes(&genesis, &alice.dev));

    t += 1;
    let inv_bob_id = [0x01; 16];
    let inv_bob_sec = [0x41; 16];
    let inv_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::MemberInvited,
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
    events.push(wire_bytes(&inv_bob, &alice.dev));

    t += 1;
    let join_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: bob.identity(),
        device_id: bob.device(),
        event_type: EventType::MemberJoined,
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
    let mut tip = join_bob.event_id();
    events.push(wire_bytes(&join_bob, &bob.dev));

    // The conversation: a linear chain so the invite's ancestry is n_chat deep.
    for i in 0..n_chat {
        t += 1;
        let msg = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: bob.identity(),
            device_id: bob.device(),
            event_type: EventType::MessageText,
            created_at: t,
            prev_events: vec![tip],
            content: Content::MessageText(MessageText {
                body: format!("late-invite msg {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        tip = msg.event_id();
        events.push(wire_bytes(&msg, &bob.dev));
    }

    // The invite minted AFTER the conversation, citing the chat tip — the
    // production `prev_events = store.heads(room_id)` shape.
    t += 1;
    let inv_carol_id = [0x02; 16];
    let inv_carol_sec = [0x42; 16];
    let inv_carol = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: vec![tip],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_carol_id,
            capability_hash: capability_hash(&room, &inv_carol_id, &inv_carol_sec),
            role: "member".to_owned(),
            invitee_key: carol.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    events.push(wire_bytes(&inv_carol, &alice.dev));

    Built {
        room,
        events,
        alice,
        bob,
        carol,
    }
}

/// The join-after-history regression ("no one can join a room once a
/// conversation has started"). An invite minted after chat cites chat heads; a
/// bootstrapping joiner pulls ONLY via `WantMembership` — the admin's join
/// bootstrap drops a provisional peer's `WantEvents`/`WantRecentChat`
/// (`provisional_allows`, iroh-rooms-net) — so the membership response must be
/// causally closed or the invite can never classify. The 70-deep linear chain
/// also exceeds `max_backfill_depth` (64): even where a by-id chase is allowed,
/// it could not have recovered this ancestry, so the closure is load-bearing.
#[test]
fn late_invite_after_conversation_bootstraps_without_backfill() {
    let built = build_late_invite_log(70);
    let mut admin = fresh_engine(built.room, SyncConfig::default());
    for frame in &built.events {
        admin.publish(frame).expect("seed admin");
    }
    let mut joiner = fresh_engine(built.room, SyncConfig::default());

    // Drive the handshake by hand, modeling the admin-side provisional filter:
    // only `WantMembership` from the joiner reaches the admin.
    let only_want_membership = |outs: Vec<iroh_rooms_core::sync::Outgoing>| -> Vec<SyncMessage> {
        outs.into_iter()
            .map(|o| o.msg)
            .filter(|m| matches!(m, SyncMessage::WantMembership { .. }))
            .collect()
    };

    let mut pending = only_want_membership(joiner.on_connect(NODE_A));
    let mut rounds = 0;
    while joiner.snapshot().status(&built.carol.identity()).is_none() {
        rounds += 1;
        assert!(
            rounds <= 10,
            "bootstrap did not resolve the late invite within 10 pull rounds"
        );
        let mut responses = Vec::new();
        for msg in pending {
            responses.extend(admin.on_message(NODE_B, msg));
        }
        for o in responses {
            // The joiner's reactions (WantEvents, …) are dropped: a provisional
            // peer gets no backfill.
            let _ = joiner.on_message(NODE_A, o.msg);
        }
        pending = only_want_membership(joiner.on_tick(T0));
    }

    assert_eq!(
        joiner.snapshot().status(&built.carol.identity()),
        Some(Status::Invited),
        "the late invite must classify from the membership pull alone"
    );
    assert_eq!(
        joiner.parked_len(),
        0,
        "a causally-closed response parks nothing"
    );
    assert_eq!(
        joiner.counters().phantom_depth_dropped,
        0,
        "no depth-gate drops: the closure arrives parent-before-child"
    );
    let joiner_ids = joiner.digest().expect("joiner digest").event_ids;
    let admin_ids = admin.digest().expect("admin digest").event_ids;
    assert_eq!(
        joiner_ids, admin_ids,
        "every event here is membership ancestry, so the joiner converges fully"
    );
}

/// The truncated-closure progress invariant: when the membership closure is
/// LARGER than the responder's `response_max_frames`, every capped response is a
/// causally-closed prefix the joiner fold-accepts in full, and — because the
/// joiner's `have` claims every id it holds, not just its own class closure —
/// each round's pull shrinks the delta until the late invite lands. (Claiming
/// only the requester-side closure livelocks here: served chat ancestry is not
/// yet an ancestor of any held class event, so the responder re-serves the same
/// truncated prefix forever and the joiner freezes below the cap boundary.)
#[test]
fn truncated_closure_pull_makes_progress_every_round() {
    let built = build_late_invite_log(20); // closure: 3 membership + 20 chat + invite = 24
    let tight = SyncConfig {
        response_max_frames: 8, // force ceil(24/8) = 3 pull rounds
        ..SyncConfig::default()
    };
    let mut admin = fresh_engine(built.room, tight);
    for frame in &built.events {
        admin.publish(frame).expect("seed admin");
    }
    let mut joiner = fresh_engine(built.room, SyncConfig::default());

    let only_want_membership = |outs: Vec<iroh_rooms_core::sync::Outgoing>| -> Vec<SyncMessage> {
        outs.into_iter()
            .map(|o| o.msg)
            .filter(|m| matches!(m, SyncMessage::WantMembership { .. }))
            .collect()
    };

    let mut pending = only_want_membership(joiner.on_connect(NODE_A));
    let mut rounds = 0;
    while joiner.snapshot().status(&built.carol.identity()).is_none() {
        rounds += 1;
        assert!(
            rounds <= 6,
            "truncated closure pull must converge in ~ceil(24/8) rounds, not livelock"
        );
        let mut responses = Vec::new();
        for msg in pending {
            responses.extend(admin.on_message(NODE_B, msg));
        }
        for o in responses {
            let _ = joiner.on_message(NODE_A, o.msg);
        }
        pending = only_want_membership(joiner.on_tick(T0));
    }

    assert_eq!(
        joiner.snapshot().status(&built.carol.identity()),
        Some(Status::Invited),
        "the late invite lands once the truncated rounds cover the closure"
    );
    assert_eq!(joiner.parked_len(), 0, "each capped prefix accepts in full");
    let joiner_ids = joiner.digest().expect("joiner digest").event_ids;
    let admin_ids = admin.digest().expect("admin digest").event_ids;
    assert_eq!(
        joiner_ids, admin_ids,
        "full convergence across capped rounds"
    );
}

/// The closure at the message level: chat events that are structural ancestors
/// of a membership event ARE served by `WantMembership`, parent-before-child,
/// and a fresh engine ingesting the response in order accepts every frame
/// without parking (the fold's readiness rule is satisfied as frames land).
#[test]
fn want_membership_closure_includes_chat_ancestry() {
    let built = build_late_invite_log(3);
    // Log: genesis, inv_bob, join_bob, 3 chat, inv_carol = 7 events, all of them
    // in the closure (the chat chain is inv_carol's ancestry).
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }

    let outs = engine.on_message(
        NODE_A,
        SyncMessage::WantMembership {
            room_id: built.room,
            have: vec![],
        },
    );
    let frames: Vec<Vec<u8>> = outs
        .into_iter()
        .filter_map(|o| match o.msg {
            SyncMessage::Events { frames, .. } => Some(frames),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        frames.len(),
        7,
        "the closure serves the class AND its chat ancestry; got {}",
        frames.len()
    );
    let chat_served = frames
        .iter()
        .filter(|f| {
            let wire_ev = WireEvent::decode(f).expect("valid wire event");
            let signed_ev = SignedEvent::decode(&wire_ev.signed).expect("valid signed event");
            matches!(signed_ev.event_type, EventType::MessageText)
        })
        .count();
    assert_eq!(
        chat_served, 3,
        "the 3 chat ancestors of the late invite must ride along"
    );

    // A fresh engine ingesting the response in served order accepts everything.
    let mut fresh = fresh_engine(built.room, SyncConfig::default());
    fresh.on_connect(NODE_B);
    for frame in &frames {
        fresh.ingest_frame(NODE_B, frame);
    }
    assert_eq!(fresh.parked_len(), 0, "served order is parent-before-child");
    assert_eq!(
        fresh.snapshot().status(&built.carol.identity()),
        Some(Status::Invited),
        "the late invite classifies immediately from the closure"
    );
}

// ---------------------------------------------------------------------------
// Response-cap chunking — small response_max_frames + eventual convergence
// ---------------------------------------------------------------------------

/// When the responder's `response_max_frames` cap (3) is smaller than the
/// 6 authorization-class events to serve, the requester's `on_tick` anti-entropy
/// re-pull closes the gap using the delta `have` list. Full convergence is
/// eventually reached; the cap is logged.
#[test]
fn response_max_frames_cap_recovers_via_subsequent_pull() {
    let built = build_log(0, true);
    // 6 authorization-class events: genesis, inv_bob, join_bob,
    // inv_carol, join_carol, remove_carol.

    let tight = SyncConfig {
        response_max_frames: 3, // less than the 6 auth-class events
        ..SyncConfig::default()
    };

    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, tight));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    seed(&mut net, NODE_A, &built.events);

    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    // Despite the per-response cap, B converges via repeated anti-entropy pulls.
    net.assert_membership_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "carol removed on B despite response_max_frames cap of 3"
    );
    assert!(
        net.engine(NODE_A)
            .logs()
            .iter()
            .any(|l| l.contains("response_max_frames")),
        "A must log the response_max_frames cap event"
    );
}

// ---------------------------------------------------------------------------
// §8.2 reconnect — partitioned peer catches up on events published while offline
// ---------------------------------------------------------------------------

/// Both peers start with the 5 membership events and sync. Then a partition drops
/// B offline while A publishes 3 chat events and the removal. After reconnect B
/// must converge on all events — the engine retains its park across disconnect and
/// the reconnect handshake re-pulls any delta.
#[test]
fn partitioned_peer_catches_up_on_events_published_while_offline() {
    let built = build_log(3, true);
    // events[0..5] = membership; events[5..8] = chat; events[8] = removal
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // Both start with the 5 membership events and converge.
    seed(&mut net, NODE_A, &built.events[..5]);
    seed(&mut net, NODE_B, &built.events[..5]);
    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();
    net.assert_membership_converged(&[NODE_A, NODE_B]);

    // Partition: B goes offline.
    net.partition(&[NODE_A], &[NODE_B]);

    // While B is offline, A receives the 3 chat events and the removal.
    for frame in &built.events[5..] {
        net.engine_mut(NODE_A)
            .publish(frame)
            .expect("A publish during partition");
    }

    // B reconnects and must catch up on the missed events.
    net.reconnect(NODE_A, NODE_B);
    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B)
            .snapshot()
            .status(&built.carol.identity()),
        Some(Status::Removed),
        "carol removed on B after reconnect following partitioned publish"
    );
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::Complete,
        "completeness Complete after full catch-up"
    );
}

// ---------------------------------------------------------------------------
// Security — an unverified admin-tip advertisement cannot forge a fork
// ---------------------------------------------------------------------------

/// §7 / D6 adversarial vector (the test gap honest-tip fork tests leave open): a
/// peer advertises a *fabricated* admin tip id at an `admin_seq` the victim
/// genuinely holds. The detector sources fork state only from held-and-validated
/// admin events, so the bogus advertisement neither collides into a fork nor
/// forges a CRITICAL `equivocation` against the honest admin. (Genuine held-branch
/// fork detection is covered by `sync_smoke::admin_fork_raises_critical_equivocation`.)
#[test]
fn fabricated_admin_tip_at_held_seq_does_not_forge_a_fork() {
    let built = build_log(0, true); // genesis..join_carol + the real removal
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, built.room, SyncConfig::default()).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }
    assert_eq!(engine.completeness(), Completeness::Complete);

    // The admin_seq of the removal we genuinely hold.
    let (_real_id, held_seq) = engine
        .digest()
        .expect("digest")
        .admin_tip
        .expect("admin tip present");

    // A malicious peer advertises a *different* id at that same held admin_seq.
    let forged = EventId::from_bytes([0xFA; 32]);
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((forged, held_seq)),
        },
    );

    assert_eq!(
        engine.completeness(),
        Completeness::Complete,
        "an unverified advertisement must not forge an admin fork"
    );
    assert!(
        engine
            .trust_decisions()
            .iter()
            .all(|d| d.code != "equivocation"),
        "no equivocation is recorded from a fabricated advertised tip"
    );
}

// ---------------------------------------------------------------------------
// Security — a bogus higher admin tip expires instead of pinning fail-closed
// ---------------------------------------------------------------------------

/// §13 / D6 adversarial vector: a peer advertises a fabricated tip far ahead of
/// the real admin chain. The victim fails closed while it tries to catch up (it
/// cannot yet know the tip is fake), but the unverified tip is bounded — after the
/// attempt budget it expires rather than pinning the node in permanent
/// `AdminViewSuspect` / fail-closed (the never-backfillable bogus-tip `DoS`).
#[test]
fn bogus_higher_admin_tip_expires_and_does_not_pin_fail_closed() {
    let built = build_log(0, true);
    let store = EventStore::open_in_memory().expect("store");
    let cfg = SyncConfig {
        max_unconfirmed_tip_attempts: 3,
        ..SyncConfig::default()
    };
    let mut engine = SyncEngine::open(store, built.room, cfg).expect("engine");
    for frame in &built.events {
        engine.publish(frame).expect("seed");
    }
    assert_eq!(engine.completeness(), Completeness::Complete);

    // A peer advertises a fabricated tip far ahead of our real chain.
    let forged = EventId::from_bytes([0xFA; 32]);
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((forged, 9_999)),
        },
    );
    assert_eq!(
        engine.completeness(),
        Completeness::AdminViewSuspect,
        "we fail closed while chasing the (as-yet-unfalsified) higher tip"
    );

    // Tick past the attempt budget: the never-backfillable tip must expire.
    for _ in 0..5 {
        let _ = engine.on_tick(1_000);
    }
    assert_eq!(
        engine.completeness(),
        Completeness::Complete,
        "a fabricated, never-backfillable admin tip must expire, not pin fail-closed forever"
    );
    assert!(
        engine.fail_closed_subjects().is_empty(),
        "fail-closed subjects clear once the bogus tip expires"
    );
}

// ---------------------------------------------------------------------------
// Security — a *genuine* cross-partition fork is still detected via the
// never-windowed membership backfill (held-branch detection, no advertisement)
// ---------------------------------------------------------------------------

/// Confirms the held-only fork detector still catches a *real* admin self-fork
/// across a partition: two distinct removals at the same `admin_seq`, one held by
/// each peer. Neither peer holds both branches initially, but the never-windowed
/// `WantMembership` exchange reconciles both admin-authored branches onto each
/// peer (both are auth-class), so each detects `AdminForkDetected` + a CRITICAL
/// `equivocation` — proving the security fix tightens, not removes, fork detection.
#[test]
fn cross_partition_admin_fork_detected_after_membership_backfill() {
    let built = build_log(0, false);
    // genesis(0) inv_bob(1) join_bob(2) inv_carol(3) join_carol(4)
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
            event_type: EventType::MemberRemoved,
            created_at: T0 + 100,
            prev_events: vec![join_carol_eid, inv_carol_eid],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: built.carol.identity(),
                removed_by: built.alice.identity(),
                reason: Some(reason.to_owned()),
                device_binding: None,
            }),
        };
        wire_bytes(&ev, &built.alice.dev)
    };
    let fork_a = mk_removal("a");
    let fork_b = mk_removal("b");

    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, fresh_engine(built.room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(built.room, SyncConfig::default()));

    // Both hold the full membership; then each holds a *different* fork branch.
    seed(&mut net, NODE_A, &built.events);
    seed(&mut net, NODE_B, &built.events);
    seed(&mut net, NODE_A, &[fork_a]);
    seed(&mut net, NODE_B, &[fork_b]);

    net.connect(NODE_A, NODE_B);
    net.run_to_quiescence();

    for peer in [NODE_A, NODE_B] {
        assert_eq!(
            net.engine(peer).completeness(),
            Completeness::AdminForkDetected,
            "a real cross-partition fork must be detected at {peer} after backfill"
        );
        assert!(
            net.engine(peer)
                .trust_decisions()
                .iter()
                .any(|d| d.code == "equivocation"),
            "a CRITICAL equivocation must be recorded at {peer}"
        );
    }
}
