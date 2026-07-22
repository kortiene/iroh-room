//! Restart-durability integration tests for [IR-0201] harden-recent-history-sync.
//!
//! Proves the AC5 matrix (park / fail-closed / trust-audit / rate-limit /
//! convergence-after-restart) over the [`SimNet`] deterministic harness and
//! direct [`SyncEngine`] manipulation.  Every test uses only the public API;
//! none require real network or async.
//!
//! Restart is modelled via [`SimNet::restart`], which drops the engine's
//! in-memory session state and re-opens over the same store — identical to
//! what a real process restart does (spec D9 / AC5).
//!
//! Coverage map (spec §8.1 AC5 / §8.2 / §8.3):
//!
//! | Test | Spec claim |
//! |------|------------|
//! | `park_survives_restart` | AC5(i) — park + missing-parent retry survive restart |
//! | `fail_closed_posture_survives_restart` | AC5(ii) — `AdminViewSuspect` re-armed before first message |
//! | `trust_audit_survives_restart` | AC5(iii) — `trust_decisions()` non-empty after restart |
//! | `rate_limit_not_refilled_by_restart` | AC5(iv) — amplification budget not reset by restart |
//! | `restart_with_in_flight_state_then_converges` | §8.2 — full convergence after SimNet restart |
//! | `checkpoint_idempotency_park_upsert_twice_is_noop` | §8.3 — upsert idempotency |
//! | `restart_determinism_two_independent_engines_yield_identical_state` | §8.3 — determinism |
//! | `corrupt_parked_wire_is_dropped_on_restore_without_panic` | §D5/R3 — corrupt row dropped |
//! | `invalid_frame_rejected_and_logged_without_tracing` | AC3 — reject without tracing subscriber |
//! | `shuffled_delivery_after_restart_converges` | §D9+§8.4 — restart + shuffled delivery combined |
//! | `chat_window_bound_prevents_overflow_events_from_syncing` | AC1 — bound enforced when events exceed window |

#![cfg(feature = "sync")]
#![allow(clippy::similar_names, clippy::too_many_lines)]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MessageText, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::event::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::store::{EventStore, ParkedRow, SyncStateRow};
use iroh_rooms_core::sync::sim::SimNet;
use iroh_rooms_core::sync::{Completeness, PeerId, SyncConfig, SyncEngine, SyncMessage};

// ── fixtures ─────────────────────────────────────────────────────────────────

const NONCE: [u8; 16] = [0xCC; 16];
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

/// A structured room log with individually-named frames for selective delivery.
struct Built {
    room: RoomId,
    /// All frames in causal order (genesis → carol join → optional chat).
    events: Vec<Vec<u8>>,
    /// Named frames for targeted delivery in restart scenarios.
    genesis: Vec<u8>,
    join_bob: Vec<u8>,
    alice: Principal,
    #[allow(dead_code)]
    bob: Principal,
}

/// Build `genesis(alice) → invite_bob → join_bob → invite_carol → join_carol
/// → {n_chat bob messages}`.
///
/// Admin events (alice) cite the prior admin event so `admin_seq` flows.
/// Chat is authored by bob (a member) parented on his join — chat-class.
#[allow(clippy::too_many_lines)]
fn build_log(n_chat: u32) -> Built {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut events: Vec<Vec<u8>> = Vec::new();
    let mut t = T0;

    // genesis (alice, admin_seq = 0)
    let genesis_ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::RoomCreated,
        created_at: t,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Restart Test".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
        }),
    };
    let gid = genesis_ev.event_id();
    let genesis_bytes = wire_bytes(&genesis_ev, &alice.dev);
    events.push(genesis_bytes.clone());

    // invite_bob (alice, admin_seq = 1; cites genesis)
    t += 1;
    let inv_bob_id = [0x01_u8; 16];
    let inv_bob_sec = [0x41_u8; 16];
    let invite_bob_ev = SignedEvent {
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
    let inv_bob_eid = invite_bob_ev.event_id();
    let invite_bob_bytes = wire_bytes(&invite_bob_ev, &alice.dev);
    events.push(invite_bob_bytes);

    // join_bob (bob; cites invite_bob)
    t += 1;
    let join_bob_ev = SignedEvent {
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
    let join_bob_eid = join_bob_ev.event_id();
    let join_bob_bytes = wire_bytes(&join_bob_ev, &bob.dev);
    events.push(join_bob_bytes.clone());

    // invite_carol (alice, admin_seq = 2; cites invite_bob)
    t += 1;
    let inv_carol_id = [0x02_u8; 16];
    let inv_carol_sec = [0x42_u8; 16];
    let invite_carol_ev = SignedEvent {
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
    let inv_carol_eid = invite_carol_ev.event_id();
    events.push(wire_bytes(&invite_carol_ev, &alice.dev));

    // join_carol (carol; cites invite_carol)
    t += 1;
    let join_carol_ev = SignedEvent {
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
    events.push(wire_bytes(&join_carol_ev, &carol.dev));

    // chat from bob (siblings of join_bob — chat-class)
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
                body: format!("msg {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        events.push(wire_bytes(&msg, &bob.dev));
    }

    Built {
        room,
        events,
        genesis: genesis_bytes,
        join_bob: join_bob_bytes,
        alice,
        bob,
    }
}

fn default_config() -> SyncConfig {
    SyncConfig::default()
}

/// Publish a slice of wire frames into an engine; panic on any validation error.
fn seed_engine(engine: &mut SyncEngine, frames: &[Vec<u8>]) {
    for f in frames {
        engine.publish(f).expect("publish");
    }
}

// ── AC5(i): park survives restart ────────────────────────────────────────────

/// A causally-incomplete frame parked in B is persisted to `sync_parked` and
/// restored on `open`, so a process restart does not lose in-flight buffering.
/// After restart the restored park re-issues `WantEvents` on the next
/// `on_connect`, the missing parent arrives, the child is woken, and both
/// peers converge (spec §6.3 / AC5 / D9).
#[test]
fn park_survives_restart() {
    let built = build_log(0);
    let cfg = default_config();

    // Engine A: holds the full log.
    let mut engine_a =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine_a, &built.events);

    // Engine B: genesis only; receives join_bob whose parent (invite_bob) is
    // missing.  MemberJoined always passes signer_plausible (spec §6.2), so it
    // is parked, not silently dropped at the pre-gate.
    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    engine_b.publish(&built.genesis).unwrap();
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);

    assert_eq!(
        engine_b.parked_len(),
        1,
        "join_bob must be parked before restart"
    );
    assert_eq!(
        engine_b.counters().parked,
        1,
        "parked counter before restart"
    );

    // Simulate a process restart: put in SimNet and call restart() which drops
    // the engine and re-opens over the same store (same in-memory SQLite conn).
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_A, engine_a);
    net.add_peer(NODE_B, engine_b);
    net.restart(NODE_B).unwrap();

    assert_eq!(
        net.engine(NODE_B).parked_len(),
        1,
        "park must survive the restart (AC5-i)"
    );
    assert_eq!(
        net.engine(NODE_B).counters().parked_restored,
        1,
        "parked_restored counter must reflect the restored frame"
    );

    // Reconnect via SimNet: B re-issues WantEvents for invite_bob on connect,
    // A serves it, join_bob is woken and accepted, both peers converge.
    net.connect_all();
    net.run_to_quiescence();
    net.assert_converged(&[NODE_A, NODE_B]);
}

// ── AC5(ii): fail-closed posture survives restart ────────────────────────────

/// A raised `AdminViewSuspect` from an advertised higher admin tip is persisted
/// to `sync_state.suspect_tip_*` and re-arms `completeness()` on `open` —
/// **before** any new message is received. A reboot must never be a way to
/// clear a fail-closed removal-sensitive gate (spec §1.1 / D3 / AC5-ii).
#[test]
fn fail_closed_posture_survives_restart() {
    let built = build_log(0); // alice (admin) + bob + carol (both Active)
    let cfg = default_config();

    // Engine B: full log; alice's admin_seq tip is 2 (invite_carol).
    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine_b, &built.events);
    assert_eq!(engine_b.completeness(), Completeness::Complete);
    assert!(engine_b.fail_closed_subjects().is_empty());

    // A peer advertises a fake admin tip at seq = 999 > 2.  This arms
    // suspect_tip (persisted) and drives completeness → AdminViewSuspect.
    // on_message does not require the sender to be a connected peer.
    let fake_tip = EventId::from_bytes([0xFF; 32]);
    let _ = engine_b.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((fake_tip, 999)),
        },
    );

    assert_eq!(engine_b.completeness(), Completeness::AdminViewSuspect);
    assert!(
        !engine_b.fail_closed_subjects().is_empty(),
        "bob and carol must be fail-closed before restart"
    );

    // Restart B via SimNet.
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_B, engine_b);
    net.restart(NODE_B).unwrap();

    // IMMEDIATELY after open — before any new message — the posture must hold.
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::AdminViewSuspect,
        "AdminViewSuspect must survive restart (anti fail-open, AC5-ii)"
    );
    assert!(
        !net.engine(NODE_B).fail_closed_subjects().is_empty(),
        "fail-closed subjects must be non-empty immediately after restart"
    );
    assert_eq!(
        net.engine(NODE_B).counters().suspicion_restored,
        1,
        "suspicion_restored counter must be set"
    );
}

// ── AC5(iii): trust-decision audit survives restart ──────────────────────────

/// A trust decision recorded in-memory is also appended to `trust_decisions`
/// in the store and restored on `open`, so a reboot cannot erase a CRITICAL
/// admin-fork alert or an `admin_view_suspect` warning (spec §D6 / AC5-iii).
#[test]
fn trust_audit_survives_restart() {
    let built = build_log(0);
    let cfg = default_config();

    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine_b, &built.events);

    // Arm a fake higher tip → records an `admin_view_suspect` trust decision.
    let fake_tip = EventId::from_bytes([0xEE; 32]);
    let _ = engine_b.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((fake_tip, 50)),
        },
    );
    assert!(
        !engine_b.trust_decisions().is_empty(),
        "a trust decision must be recorded before restart"
    );
    let code_before = engine_b.trust_decisions()[0].code;

    // Restart.
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_B, engine_b);
    net.restart(NODE_B).unwrap();

    assert!(
        !net.engine(NODE_B).trust_decisions().is_empty(),
        "trust_decisions() must survive restart (AC5-iii)"
    );
    assert_eq!(
        net.engine(NODE_B).trust_decisions()[0].code,
        code_before,
        "trust decision code must be identical after restore"
    );
    assert!(
        net.engine(NODE_B).counters().trust_restored >= 1,
        "trust_restored counter must reflect the loaded decisions"
    );
}

// ── AC5(iv): rate-limiter tokens not refilled by restart ─────────────────────

/// Backfill token buckets are persisted to `sync_backfill_tokens` and restored
/// as-is on `open` — **not** refilled by the restart — so a crash-loop cannot
/// reset the §4 anti-amplification budget (spec §1.3 / R4 / AC5-iv).
#[test]
fn rate_limit_not_refilled_by_restart() {
    let built = build_log(0);
    // Minimal budget: 1 token per author, 0 refill per tick.
    let cfg = SyncConfig {
        backfill_tokens_per_author: 1,
        backfill_refill_per_tick: 0,
        ..SyncConfig::default()
    };

    // Engine B: genesis only; no peers connected.
    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    engine_b.publish(&built.genesis).unwrap();

    // Deliver join_bob (cites invite_bob, which is missing) → parked.
    // The engine takes one backfill token for bob's author bucket (1→0).
    // With no peers connected the WantEvents has nowhere to go, but the token
    // IS consumed by the park machinery.
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);
    assert_eq!(
        engine_b.parked_len(),
        1,
        "join_bob must be parked before restart"
    );

    // Restart B via SimNet.
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_B, engine_b);
    net.restart(NODE_B).unwrap();

    assert_eq!(
        net.engine(NODE_B).counters().tokens_restored,
        1,
        "one author token bucket must be restored"
    );

    // Trigger `retry_restored_park` via `on_connect`.  The function guards on
    // `self.peers.is_empty()`, so it needs at least one connected peer.
    // We call it directly (bypassing SimNet link bookkeeping) since we only
    // care about the token-bucket counter, not the actual routed frames.
    // The restored token bucket for bob's author is 0 (not refilled by the
    // restart), so `take_backfill_token` returns false → rate-limited.
    let _ = net.engine_mut(NODE_B).on_connect(NODE_A);

    assert!(
        net.engine(NODE_B).counters().backfill_rate_limited >= 1,
        "backfill must be rate-limited after restart: tokens must not be refilled (AC5-iv)"
    );
    assert_eq!(
        net.engine(NODE_B).counters().backfill_requests,
        0,
        "no successful backfill request expected when token is exhausted"
    );
}

// ── §8.2: convergence after SimNet restart ────────────────────────────────────

/// End-to-end: an engine with in-flight state (a parked frame) is restarted
/// via `SimNet::restart()`, then reconnected; the restored park re-issues its
/// backfill on the first `on_connect`, the missing parent is delivered, the
/// child is woken, and both peers converge to an identical digest.
///
/// This proves restart durability composes with the `PeerManager` reconnect
/// guarantee (spec D9 / §8.2 real-QUIC restart scenario at the `SimNet` layer).
#[test]
fn restart_with_in_flight_state_then_converges() {
    let built = build_log(2); // genesis + bob/carol members + 2 chat msgs
    let cfg = default_config();

    let mut net = SimNet::new(built.room);

    // A holds the full log.
    let mut engine_a =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine_a, &built.events);
    net.add_peer(NODE_A, engine_a);

    // B starts with genesis only; manually receive join_bob (parent missing) → parks.
    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    engine_b.publish(&built.genesis).unwrap();
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);
    assert_eq!(
        engine_b.parked_len(),
        1,
        "pre-restart: join_bob must be parked"
    );
    net.add_peer(NODE_B, engine_b);

    // Process restart via SimNet: drops + re-opens over the same store.
    net.restart(NODE_B).unwrap();

    assert_eq!(
        net.engine(NODE_B).counters().parked_restored,
        1,
        "park must be restored after SimNet restart"
    );

    // Connect and drive to quiescence: B re-issues WantEvents for invite_bob,
    // A serves it, join_bob wakes, everything converges.
    net.connect_all();
    net.run_to_quiescence();
    net.assert_converged(&[NODE_A, NODE_B]);
}

// ── §8.3: checkpoint idempotency ─────────────────────────────────────────────

/// Delivering the same frame twice must not grow the park beyond one entry
/// (the `ON CONFLICT … DO UPDATE` upsert is idempotent), and after restart
/// exactly one frame is restored (no double-counting).
#[test]
fn checkpoint_idempotency_park_upsert_twice_is_noop() {
    let built = build_log(0);
    let cfg = default_config();

    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    engine_b.publish(&built.genesis).unwrap();

    // First delivery → parks join_bob, upserts `sync_parked`.
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);
    assert_eq!(
        engine_b.parked_len(),
        1,
        "first delivery must park the frame"
    );

    // Second delivery of the exact same bytes → duplicate — must be a no-op.
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);
    assert_eq!(
        engine_b.parked_len(),
        1,
        "duplicate delivery must not grow the park"
    );
    assert_eq!(
        engine_b.counters().parked,
        1,
        "parked counter must not double-count a duplicate"
    );

    // Restart → exactly one row is restored.
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_B, engine_b);
    net.restart(NODE_B).unwrap();

    assert_eq!(
        net.engine(NODE_B).parked_len(),
        1,
        "exactly one parked frame must be restored (upsert idempotency)"
    );
    assert_eq!(
        net.engine(NODE_B).counters().parked_restored,
        1,
        "parked_restored must be 1, not 2"
    );
}

// ── §8.3: restart determinism ────────────────────────────────────────────────

/// The same mutate→restart sequence yields byte-identical restored state
/// across two independent engines (BTreeMap/BTreeSet discipline; `park_seq` and
/// trust.seq are monotone, never clock-derived — spec §8.3 restart
/// determinism guard).
#[test]
fn restart_determinism_two_independent_engines_yield_identical_state() {
    let built = build_log(0);
    let cfg = default_config();

    let fake_tip = EventId::from_bytes([0xAB; 32]);

    let run = || -> (usize, Completeness, bool, u64) {
        let mut e =
            SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
        e.publish(&built.genesis).unwrap();
        // Arm suspicion.
        let _ = e.on_message(
            NODE_A,
            SyncMessage::AdminTip {
                room_id: built.room,
                tip: Some((fake_tip, 77)),
            },
        );
        // Park join_bob.
        let _ = e.ingest_frame(NODE_A, &built.join_bob);

        // Restart via SimNet.
        let mut net = SimNet::new(built.room);
        net.add_peer(NODE_B, e);
        net.restart(NODE_B).unwrap();

        (
            net.engine(NODE_B).parked_len(),
            net.engine(NODE_B).completeness(),
            !net.engine(NODE_B).fail_closed_subjects().is_empty(),
            net.engine(NODE_B).counters().trust_restored,
        )
    };

    let (parked1, comp1, fc1, trust1) = run();
    let (parked2, comp2, fc2, trust2) = run();

    assert_eq!(parked1, parked2, "parked count must be deterministic");
    assert_eq!(comp1, comp2, "completeness must be deterministic");
    assert_eq!(fc1, fc2, "fail_closed presence must be deterministic");
    assert_eq!(trust1, trust2, "trust_restored count must be deterministic");
}

// ── AC3 observability via engine logs() and counters() ───────────────────────

/// Invalid (non-decodable) frames increment `counters().rejected` and append a
/// stable `reject.<code>` line to `logs()` — the non-tracing observability
/// surface for the CLI-has-no-subscriber constraint (spec D8 / AC3).
#[test]
fn invalid_frame_rejected_and_logged_without_tracing() {
    let built = build_log(0);
    let cfg = default_config();

    let mut engine =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine, &built.events);

    let rejected_before = engine.counters().rejected;

    // Inject obviously-corrupt bytes (not a valid WireEvent).
    let garbage = vec![0xFF_u8; 64];
    let _ = engine.ingest_frame(NODE_A, &garbage);

    assert!(
        engine.counters().rejected > rejected_before,
        "rejected counter must increment on a corrupt frame"
    );
    let has_reject_log = engine.logs().iter().any(|l| l.starts_with("reject."));
    assert!(
        has_reject_log,
        "a reject.<code> line must appear in logs() for AC3 observability"
    );
}

// ── Corrupt park row is dropped on restore without panic ─────────────────────

// ── AC1: offline peer catches up within configured chat window ────────────────

/// AC1: A peer that was offline while N recent events accumulated reconnects via
/// the `WantMembership` + `WantRecentChat` handshake and reaches full event-set
/// equality within the configured `chat_window_default` bound (spec §6.3 /
/// §10.7 / AC1).
///
/// This is the minimal reconnect scenario: A holds the full log, B has an empty
/// store, B connects and drives catch-up via the `on_connect` pull sequence.
#[test]
fn offline_peer_catches_up_within_chat_window() {
    let built = build_log(5); // genesis + bob/carol members + 5 chat messages
    let cfg = default_config();

    let mut net = SimNet::new(built.room);
    net.add_fresh_peer(NODE_A, cfg).unwrap();
    net.add_fresh_peer(NODE_B, cfg).unwrap();

    // A holds the full log; B is "offline" (fresh store, no events).
    seed_engine(net.engine_mut(NODE_A), &built.events);

    // B reconnects and drives catch-up via WantMembership + WantRecentChat.
    net.connect_all();
    net.run_to_quiescence();

    // Full event-set equality must hold after reconnect (AC1).
    net.assert_converged(&[NODE_A, NODE_B]);
}

// ── AC2: membership sub-DAG is complete after sync ───────────────────────────

/// AC2: After sync, the never-windowed authorization-class event set
/// (membership sub-DAG + all admin-authored events) is identical on every peer
/// regardless of any chat window bounds — the unconditional §0 hard invariant
/// (`WantMembership` is never windowed, spec §0 / §4.1 / AC2).
#[test]
fn membership_sub_dag_complete_after_sync() {
    let built = build_log(3); // genesis + bob/carol members + 3 chat messages
    let cfg = default_config();

    let mut net = SimNet::new(built.room);
    net.add_fresh_peer(NODE_A, cfg).unwrap();
    net.add_fresh_peer(NODE_B, cfg).unwrap();

    seed_engine(net.engine_mut(NODE_A), &built.events);

    net.connect_all();
    net.run_to_quiescence();

    // The never-windowed authorization-class set and snapshot must be equal (AC2).
    net.assert_membership_converged(&[NODE_A, NODE_B]);

    // B's snapshot must reflect the full membership chain (admin + members).
    let snap = net.engine(NODE_B).snapshot();
    assert!(
        snap.admin().is_some(),
        "membership snapshot must have an admin after sync (AC2)"
    );
}

// ── AC4: duplicate events are silently ignored at the engine level ────────────

/// AC4: Re-delivering an already-accepted frame through `ingest_frame` is a
/// silent no-op: `counters().accepted` must not increase, and the duplicate
/// must be observed by either the early dedup cache (`early_duplicates`,
/// issue #143) or the post-store idempotent path (`duplicates`, spec §6 step
/// 11 / Event Protocol §6 duplicate-idempotency / AC4).
#[test]
fn engine_duplicates_are_silently_ignored() {
    let built = build_log(2); // genesis + members + 2 chat messages
    let cfg = default_config();

    let mut engine =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine, &built.events);

    let accepted_before = engine.counters().accepted;
    let early_before = engine.counters().early_duplicates;
    let dup_before = engine.counters().duplicates;

    // Re-deliver every frame via ingest_frame, simulating a peer re-sending.
    for frame in &built.events {
        let _ = engine.ingest_frame(NODE_A, frame);
    }

    assert_eq!(
        engine.counters().accepted,
        accepted_before,
        "accepted must not increase on duplicate re-delivery (AC4)"
    );
    assert!(
        engine.counters().early_duplicates + engine.counters().duplicates
            > early_before + dup_before,
        "duplicate re-delivery must be observed by the early or post-store path (AC4 / #143)"
    );
}

// ── spec §13 / D3: suspect-tip attempt budget decrements persist across restart

/// spec §13 / D3: The unconfirmed admin-tip attempt budget is decremented on
/// every `on_tick` call and **persisted** to `sync_state.suspect_tip_attempts`,
/// so a restart cannot reset the budget and grant extra catch-up ticks to a
/// fabricated tip. Only the already-decremented remaining budget is restored.
///
/// Proof: with `max_attempts = 4`, after 2 pre-restart ticks the budget is 2.
/// A restart that reset the budget would need 5 more ticks to expire; with
/// the correctly-restored budget of 2 it takes exactly 3 more ticks.
#[test]
fn suspect_tip_attempt_budget_decrements_persist_across_restart() {
    let built = build_log(0);
    let max_attempts: u32 = 4;
    let cfg = SyncConfig {
        max_unconfirmed_tip_attempts: max_attempts,
        ..SyncConfig::default()
    };

    let mut engine =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine, &built.events);

    // Arm a fake tip (budget = max_attempts = 4, persisted).
    let fake_tip = EventId::from_bytes([0x77; 32]);
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: built.room,
            tip: Some((fake_tip, 500)),
        },
    );
    assert_eq!(engine.completeness(), Completeness::AdminViewSuspect);

    // Two ticks: budget decrements 4 → 3 → 2, persisted after each tick.
    let _ = engine.on_tick(1000);
    let _ = engine.on_tick(2000);
    assert_eq!(engine.completeness(), Completeness::AdminViewSuspect);

    // Restart over the same store: the persisted budget of 2 is restored.
    let mut net = SimNet::new(built.room);
    net.add_peer(NODE_B, engine);
    net.restart(NODE_B).unwrap();

    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::AdminViewSuspect,
        "suspicion must survive restart"
    );

    // Three post-restart ticks expire the remaining budget (2 → 1 → 0 → expire).
    let _ = net.engine_mut(NODE_B).on_tick(3000); // 2 → 1
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::AdminViewSuspect,
        "still suspect after first post-restart tick"
    );
    let _ = net.engine_mut(NODE_B).on_tick(4000); // 1 → 0
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::AdminViewSuspect,
        "still suspect after second post-restart tick (attempts reaches 0, not yet expired)"
    );
    let _ = net.engine_mut(NODE_B).on_tick(5000); // 0 → expire
    assert_eq!(
        net.engine(NODE_B).completeness(),
        Completeness::Complete,
        "suspicion must expire after exactly 3 post-restart ticks — budget was NOT reset (spec §13)"
    );
}

// ── OQ-1: chat cursor is advisory — not consumed on engine open ───────────────

/// OQ-1: The `sync_state.chat_cursor_*` columns are advisory-only and are
/// **not consumed** on `open` (spec §6.1 comment: "the chat cursor is advisory
/// and intentionally not consumed"). Restoring a persisted chat cursor must not
/// set `suspicion_restored`, affect completeness, or cause any fail-closed
/// subjects.
#[test]
fn chat_cursor_is_advisory_not_consumed_on_open() {
    let built = build_log(0);
    let cfg = default_config();

    // Build a store with genesis + a persisted chat cursor, but NO suspect tip.
    let mut store = EventStore::open_in_memory().unwrap();
    let ctx = ValidationContext::for_room(built.room);
    let genesis_validated = validate_wire_bytes(&built.genesis, &ctx).expect("genesis valid");
    let _ = store.insert(&genesis_validated).unwrap();

    let cursor_event = EventId::from_bytes([0xCC; 32]);
    store
        .save_sync_state(
            &built.room,
            &SyncStateRow {
                chat_cursor: Some((42, cursor_event)),
                suspect_tip: None,
            },
        )
        .unwrap();

    // Opening the engine must not consume the cursor, raise suspicion, or
    // alter completeness or fail-closed state (OQ-1).
    let engine = SyncEngine::open(store, built.room, cfg).unwrap();

    assert_eq!(
        engine.counters().suspicion_restored,
        0,
        "a persisted chat cursor must not raise suspicion_restored (advisory only, OQ-1)"
    );
    assert_eq!(
        engine.completeness(),
        Completeness::Complete,
        "a persisted chat cursor must not affect completeness (OQ-1)"
    );
    assert!(
        engine.fail_closed_subjects().is_empty(),
        "a persisted chat cursor must not cause fail-closed subjects (OQ-1)"
    );
}

// ── AC1 + §8.4: shuffled delivery after restart still converges ───────────────

/// AC1 + §8.4 combined: a peer with a parked frame is restarted, then
/// reconnects to a peer that can serve the missing parent, but the handshake
/// messages arrive in a deterministically-shuffled order. The restored park
/// re-issues `WantEvents` on the first `on_connect`, the shuffled delivery
/// eventually resolves the missing parent, and both peers converge.
///
/// This combines the §D9 restart-durability guarantee with the §8.4
/// shuffled-delivery vector — a scenario not covered by either suite alone.
#[test]
fn shuffled_delivery_after_restart_converges() {
    let built = build_log(3); // genesis + members + 3 chat msgs
    let cfg = default_config();

    let mut net = SimNet::new(built.room);

    // A holds the full log.
    let mut engine_a =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    seed_engine(&mut engine_a, &built.events);
    net.add_peer(NODE_A, engine_a);

    // B starts with genesis only; receives join_bob (parent invite_bob missing) → parks.
    let mut engine_b =
        SyncEngine::open(EventStore::open_in_memory().unwrap(), built.room, cfg).unwrap();
    engine_b.publish(&built.genesis).unwrap();
    let _ = engine_b.ingest_frame(NODE_A, &built.join_bob);
    assert_eq!(engine_b.parked_len(), 1, "join_bob parked before restart");
    net.add_peer(NODE_B, engine_b);

    // Restart B: in-memory session state drops, persisted park is restored.
    net.restart(NODE_B).unwrap();
    assert_eq!(
        net.engine(NODE_B).counters().parked_restored,
        1,
        "park must be restored after restart"
    );

    // Connect and immediately shuffle the handshake queue (seed 0xBEEF).
    // Regardless of arrival order, the engine's anti-entropy + park retry
    // must resolve the missing parent and converge.
    net.connect_all();
    net.shuffle(0xBEEF_DEAD_1234_5678);
    net.run_to_quiescence();

    net.assert_converged(&[NODE_A, NODE_B]);
    assert_eq!(
        net.engine(NODE_B).parked_len(),
        0,
        "no frames still parked after convergence"
    );
}

// ── AC1: chat window bound prevents overflow events from syncing ──────────────

/// AC1 "within configured bounds": A holds more chat events than
/// `chat_window_default`. After B reconnects, B must receive at most
/// `chat_window_default` chat events, not the full unbounded set.
/// Membership events are always synced regardless of the chat window (AC2).
///
/// Proves the §10.7 count bound at the level of the sync engine: `WantRecentChat`
/// is capped by `chat_window_max`, so B's chat-class event count must be strictly
/// less than A's total and bounded by the window.
///
/// Note: `chat_window_max` caps the total `room_tail` query (not just chat events),
/// so when membership events share the highest lamport they may occupy some window
/// slots. The invariant is `b_chat_count ≤ window`, not `b_chat_count == window`.
#[test]
fn chat_window_bound_prevents_overflow_events_from_syncing() {
    // Use a tiny window so the test log stays small.
    let window: u32 = 3;
    let n_chat: u32 = 12; // well above window so B cannot receive all of them
    let cfg = SyncConfig {
        chat_window_default: window,
        chat_window_max: window,
        ..SyncConfig::default()
    };
    let built = build_log(n_chat); // genesis + members + n_chat chat msgs

    let mut net = SimNet::new(built.room);
    net.add_fresh_peer(NODE_A, cfg).unwrap();
    net.add_fresh_peer(NODE_B, cfg).unwrap();

    // A holds the full log (membership + all n_chat events).
    seed_engine(net.engine_mut(NODE_A), &built.events);

    // B is a fresh empty peer (offline while events accumulated).
    net.connect_all();
    net.run_to_quiescence();

    // The membership sub-DAG (never-windowed) must be complete on B (AC2).
    net.assert_membership_converged(&[NODE_A, NODE_B]);

    // Compute B's chat event count via membership_event_ids() so the assertion
    // is independent of the membership-count hardcode.
    let a_membership_ids = net.engine(NODE_A).membership_event_ids().unwrap();
    let b_membership_ids = net.engine(NODE_B).membership_event_ids().unwrap();
    assert_eq!(
        a_membership_ids, b_membership_ids,
        "B must hold all membership events (AC2 never-windowed invariant)"
    );

    let a_total = net.engine(NODE_A).digest().unwrap().event_ids.len();
    let b_total = net.engine(NODE_B).digest().unwrap().event_ids.len();
    let a_chat_count = a_total - a_membership_ids.len(); // = n_chat
    let b_chat_count = b_total - b_membership_ids.len();

    assert!(
        b_chat_count <= window as usize,
        "B must hold at most window={window} chat events; got {b_chat_count} (AC1 bound)"
    );
    assert!(
        b_chat_count < a_chat_count,
        "B must NOT have received all {a_chat_count} chat events from A (window={window} enforcement)"
    );
}

/// A corrupt or tampered `wire` in `sync_parked` is a logged drop on restore
/// (`reject.park_corrupt`), never a panic — spec D5 / R3.
///
/// Simulated by inserting genesis into a fresh store, directly upserting a
/// `ParkedRow` with deliberately non-decodable wire bytes, then opening a
/// fresh engine over the prepared store.
#[test]
fn corrupt_parked_wire_is_dropped_on_restore_without_panic() {
    let built = build_log(0);
    let cfg = default_config();

    // Open a fresh store and insert genesis so the engine can fold the room.
    let mut store = EventStore::open_in_memory().unwrap();
    let ctx = ValidationContext::for_room(built.room);
    let genesis_validated = validate_wire_bytes(&built.genesis, &ctx).expect("genesis valid");
    let _ = store.insert(&genesis_validated).unwrap();

    // Insert a parked row whose wire bytes are deliberately corrupt.
    let corrupt_id = EventId::from_bytes([0xDE; 32]);
    let missing_parent = EventId::from_bytes([0xAD; 32]);
    store
        .upsert_parked(
            &built.room,
            &ParkedRow {
                event_id: corrupt_id,
                wire: vec![0x00, 0x01, 0x02], // not a valid WireEvent
                author: built.alice.identity(),
                park_seq: 1,
                depth: 0,
                missing: vec![missing_parent],
            },
        )
        .unwrap();

    // Opening the engine must not panic; the corrupt row is dropped and logged.
    let engine = SyncEngine::open(store, built.room, cfg).unwrap();

    assert_eq!(
        engine.parked_len(),
        0,
        "corrupt parked row must be dropped, not loaded"
    );
    assert_eq!(
        engine.counters().park_corrupt_dropped,
        1,
        "park_corrupt_dropped counter must record the drop"
    );
    let has_corrupt_log = engine.logs().iter().any(|l| l.contains("park_corrupt"));
    assert!(
        has_corrupt_log,
        "a park_corrupt log line must be emitted for AC3 observability"
    );
}
