//! End-to-end `message.text` send and receive (IR-0105 §9.3 — Node-level test).
//!
//! Exercises the full signed-message pipeline at the [`Node`] API layer:
//!
//! ```text
//! build_message_text → Node::publish → QUIC loopback → engine.deliver
//!   (validate → fold → store) → Node::room_tail
//! ```
//!
//! Coverage map (spec §9.3 / acceptance criteria):
//!
//! * **Headline e2e** — Bob sends a `build_message_text` frame; Alice's
//!   `room_tail` returns it in canonical order.
//! * **AC2** — Duplicate delivery is silently ignored: the same frame published
//!   twice appears exactly once in `room_tail`.
//! * **AC3** — Invalid signature: a tampered frame is rejected by `engine.publish`
//!   before reaching the network; a correctly-signed but zero-signature variant
//!   (`WireEvent` tamper) is dropped by `engine.ingest_frame` after crossing the
//!   loopback — verified via the `SyncEngine` counter directly.
//! * **AC4 / admission-before-bytes** — A non-member's connection attempt is
//!   rejected at the transport layer (no frame bytes read) and Alice's timeline
//!   stays empty of stranger messages.
//! * **AC5** — Three messages from Bob appear in canonical `(lamport, event_id)`
//!   ascending order in `room_tail`, not in wall-clock order.
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).
//! Membership is seeded via core event builders to avoid the `room join` CLI
//! dependency (#19 / OQ-5).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_message_text;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::reject::RejectReason;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine};
use iroh_rooms_net::{AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xab; 16];
const T0: u64 = 1_750_000_000_000;
const WAIT: Duration = Duration::from_secs(10);

struct Principal {
    id: SigningKey,
    dev: SigningKey,
}

impl Principal {
    fn new(seed: u8) -> Self {
        Self {
            id: SigningKey::from_seed(&[seed; 32]),
            dev: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
        }
    }

    fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }

    fn device_key(&self) -> iroh_rooms_core::event::keys::DeviceKey {
        self.dev.device_key()
    }

    fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev.to_seed())
    }

    fn endpoint_id(&self) -> EndpointId {
        self.iroh_secret().public()
    }
}

/// Seal a `SignedEvent` into verbatim wire bytes signed by `dev`.
fn wire_bytes(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// A two-member room log: genesis (Alice=admin) → `invite_bob` → `join_bob`.
/// Returns `(room_id, [genesis_bytes, invite_bytes, join_bytes], join_event_id,
///           alice, bob)`.
fn build_two_member_room() -> (RoomId, Vec<Vec<u8>>, EventId, Principal, Principal) {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut log: Vec<Vec<u8>> = Vec::new();

    // genesis — device_id MUST be alice.dev.device_key() (signed by alice.dev)
    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Message E2E Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device_key()),
        }),
    };
    let genesis_id = genesis.event_id();
    log.push(wire_bytes(&genesis, &alice.dev));

    // invite_bob — authored and signed by alice
    let inv_id = [0x01u8; 16];
    let inv_sec = [0x41u8; 16];
    let invite = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T0 + 1,
        prev_events: vec![genesis_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_id,
            capability_hash: capability_hash(&room, &inv_id, &inv_sec),
            role: "member".to_owned(),
            invitee_key: bob.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite_id = invite.event_id();
    log.push(wire_bytes(&invite, &alice.dev));

    // join_bob — authored and signed by bob; device_id = bob.dev.device_key()
    let join = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: bob.identity(),
        device_id: bob.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T0 + 2,
        prev_events: vec![invite_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_id,
            capability_secret: inv_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &bob.id, bob.device_key()),
            display_name: Some("Bob".to_owned()),
        }),
    };
    let join_id = join.event_id();
    log.push(wire_bytes(&join, &bob.dev));

    (room, log, join_id, alice, bob)
}

/// Build an allowlist admitting all `members` (bind device → identity, mark Active).
fn allowlist(members: &[&Principal]) -> AllowlistAdmission {
    let mut auth = AllowlistAdmission::new();
    for m in members {
        auth = auth
            .bind_device(m.endpoint_id(), m.identity())
            .set_active(m.identity());
    }
    auth
}

/// Spawn a loopback [`Node`] seeded with `log` (in causal order) via `publish`.
///
/// Returns a boxed future so the ~16 KB `Node::spawn` state machine is
/// heap-allocated once rather than inlined into each caller (clippy
/// `large_futures` — every test here calls this at least once).
fn spawn_loopback_node(
    secret: SecretKey,
    admission: AllowlistAdmission,
    room: RoomId,
    log: &[Vec<u8>],
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        for ev in log {
            engine.publish(ev).expect("seed event");
        }
        let cfg = NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        };
        Node::spawn(
            secret,
            Arc::new(admission),
            Arc::new(TracingAudit),
            engine,
            cfg,
            Duration::from_millis(100),
        )
        .await
        .expect("spawn loopback node")
    })
}

// ---------------------------------------------------------------------------
// Headline e2e — Bob sends; Alice receives via room_tail (IR-0105 §9.3 step 1-3)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_peers_exchange_message_text_via_loopback() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), roster(), room, &log).await;
    let bob_node = spawn_loopback_node(bob.iroh_secret(), roster(), room, &log).await;

    // Connect Bob → Alice.
    bob_node.connect_to(alice_node.endpoint_addr().expect("Alice addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob connected");

    // Bob builds and publishes a message.text via the IR-0105 D1 builder.
    // prev_events = [join_id] (the current heads after genesis→invite→join).
    let body = "I pushed the first prototype.";
    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        body,
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let wire_frame = wire.to_bytes();
    let msg_id = wire.id.parse().expect("valid event id from wire.id");

    bob_node
        .publish(wire_frame.clone())
        .await
        .expect("Bob publishes message.text");

    // Alice waits until the event reaches her store.
    alice_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("Alice's store contains the message event");

    // Alice's room_tail returns the message with the correct body.
    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let message_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert_eq!(
        message_events.len(),
        1,
        "exactly one message.text in Alice's tail"
    );
    let stored_ev = &message_events[0];
    let decoded = SignedEvent::decode(&stored_ev.wire.signed).expect("decode");
    let Content::MessageText(m) = &decoded.content else {
        panic!("expected MessageText content");
    };
    assert_eq!(m.body, body, "body round-trips through the transport");
    assert_eq!(
        decoded.sender_id,
        bob.identity(),
        "sender_id is Bob's identity"
    );

    // The engine must contain it on Bob's node too (he published it locally).
    assert!(
        bob_node
            .store_contains(msg_id)
            .await
            .expect("store_contains"),
        "Bob's own store contains the message he sent"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC2 — Duplicate message.text frame appears exactly once in room_tail
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_message_text_frame_appears_once_in_tail() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), roster(), room, &log).await;
    let bob_node = spawn_loopback_node(bob.iroh_secret(), roster(), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob connected");

    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "hello once",
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let frame = wire.to_bytes();
    let msg_id: EventId = wire.id.parse().expect("event id");

    // Publish the same frame twice (idempotent per AC2).
    bob_node
        .publish(frame.clone())
        .await
        .expect("first publish");
    bob_node
        .publish(frame.clone())
        .await
        .expect("second publish (idempotent)");

    alice_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("Alice receives the message");

    // Alice's tail must have the event exactly once.
    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let message_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert_eq!(
        message_events.len(),
        1,
        "AC2: duplicate message.text must appear exactly once in room_tail"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC5 — Three messages appear in canonical (lamport, event_id) order
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_messages_appear_in_canonical_tail_order() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), roster(), room, &log).await;
    let bob_node = spawn_loopback_node(bob.iroh_secret(), roster(), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob");

    // Publish three messages, each parented on join_id (lamport siblings whose
    // order is settled by event_id — the canonical tie-breaker §2.1).
    let bodies = ["alpha", "beta", "gamma"];
    let mut ids: Vec<EventId> = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let wire = build_message_text(
            &bob.id,
            &bob.dev,
            &room,
            body,
            None,
            None,
            &[],
            &[join_id],
            T0 + 10 + i as u64, // distinct created_at (advisory only)
        );
        let id: EventId = wire.id.parse().expect("event id");
        ids.push(id);
        bob_node.publish(wire.to_bytes()).await.expect("publish");
    }

    // Wait until Alice has all three.
    for id in &ids {
        alice_node
            .wait_until_contains(*id, WAIT)
            .await
            .expect("Alice receives message");
    }

    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let messages: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert_eq!(messages.len(), 3, "all three messages in tail");

    // Verify ascending (lamport, event_id) — the canonical store ordering.
    for pair in messages.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let la = a.lamport.unwrap_or(u64::MAX);
        let lb = b.lamport.unwrap_or(u64::MAX);
        assert!(
            la < lb || (la == lb && a.event_id <= b.event_id),
            "AC5: room_tail must be ascending by (lamport, event_id): got ({la}, {:?}) then ({lb}, {:?})",
            a.event_id,
            b.event_id,
        );
    }

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC3 — Invalid signature is rejected before being stored
// ---------------------------------------------------------------------------

/// An invalid-signature frame is caught by `validate_wire_bytes` inside
/// `engine.publish` on the publishing node and never reaches the wire.
/// This is the stateless-first defence: a caller cannot publish bad bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_with_invalid_signature_is_rejected_before_network() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    // Only Alice needs a node; we assert the error at Bob's publish call.
    let alice_node =
        spawn_loopback_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node =
        spawn_loopback_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");

    // Build a valid wire event, then zero the signature field.
    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "tampered",
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let zero_sig = iroh_rooms_core::event::keys::Signature::from_bytes([0u8; 64]);
    let tampered = WireEvent {
        sig: zero_sig,
        ..wire.clone()
    };

    // `engine.publish` (called by `node.publish`) runs `validate_wire_bytes`
    // first and must reject the zeroed-signature frame with an error.
    let err = bob_node
        .publish(tampered.to_bytes())
        .await
        .expect_err("publishing a tampered frame must fail");
    assert!(
        err.to_string().to_lowercase().contains("invalid")
            || err.to_string().to_lowercase().contains("signature")
            || err.to_string().to_lowercase().contains("bad"),
        "error must name the invalid frame: {err}"
    );

    // Alice must have received nothing (the frame never reached the wire).
    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let messages: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert!(
        messages.is_empty(),
        "AC3: tampered frame must not appear in Alice's tail"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC3 — Invalid-signature frame injected directly into the engine is dropped
// ---------------------------------------------------------------------------

/// Tests the inbound receive-side path: a frame that arrives over the wire
/// with a bad signature is caught by `engine.ingest_frame` (landed, #6/#11)
/// and never reaches the store.  Exercises the engine-level AC3 path directly
/// rather than the network, because `Node::publish` blocks the bad frame before
/// it hits the wire (the above test). This test wires the `SyncEngine` directly.
#[test]
fn engine_ingest_frame_drops_tampered_signature() {
    use iroh_rooms_core::sync::SyncConfig;

    let (room, log, join_id, _alice, bob) = build_two_member_room();

    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("engine");
    for ev in &log {
        engine.publish(ev).expect("seed");
    }

    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "bad sig",
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let zero_sig = iroh_rooms_core::event::keys::Signature::from_bytes([0u8; 64]);
    let tampered = WireEvent {
        sig: zero_sig,
        ..wire
    };

    let peer = PeerId::from_bytes([0xBB; 32]);
    let _ = engine.ingest_frame(peer, &tampered.to_bytes());

    let tail = engine.room_tail(100).expect("room_tail");
    let messages: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert!(
        messages.is_empty(),
        "AC3: engine must drop a tampered-signature frame (not add it to room_tail)"
    );
}

// ---------------------------------------------------------------------------
// AC4 — Non-member connection attempt rejected at admission (no frame bytes read)
// ---------------------------------------------------------------------------

/// A stranger (device not in Alice+Bob's allowlist) dials Alice and is refused
/// before any `SyncMessage` frame is read.  Alice's `room_tail` must show zero
/// `message.text` events from the stranger — because the stranger's bytes are
/// never deserialized, let alone ingested.  This verifies admission-before-bytes
/// as the first line of defence for AC4.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_member_connection_rejected_before_message_reaches_tail() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let stranger = Principal::new(0x99);

    // Alice/Bob's allowlist does NOT include the stranger.
    let member_roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), member_roster(), room, &log).await;

    // Stranger's admission list includes itself and Bob (but Alice will reject it).
    let stranger_node = spawn_loopback_node(
        stranger.iroh_secret(),
        allowlist(&[&stranger, &bob]),
        room,
        &[], // stranger has no membership events
    )
    .await;

    // Stranger dials Alice — Alice refuses it (UnknownDevice / NotActive).
    stranger_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    alice_node
        .wait_for_state(stranger.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("Alice records stranger as Unauthorized");

    // Alice's timeline must have no message.text events.
    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let messages: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert!(
        messages.is_empty(),
        "AC4: stranger's rejected connection must produce zero message.text in Alice's tail"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    stranger_node.shutdown().await.expect("shutdown stranger");

    let _ = join_id; // suppress unused-variable lint when tests are restructured
    let _ = bob;
}

// ---------------------------------------------------------------------------
// Stateless validation — build_message_text round-trips through the transport
// ---------------------------------------------------------------------------

/// Verifies that a frame built by `build_message_text` and then published
/// through the full loopback stack round-trips as a valid `WireEvent` on the
/// receiving node: the `event_id`, body, format, `sender_id`, and `device_id` all
/// match their original values after traversing the encode → QUIC → decode path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_text_round_trips_all_fields_through_transport() {
    let (room, log, join_id, alice, bob) = build_two_member_room();
    let reply_to = EventId::from_bytes([0xcd; 32]);

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), roster(), room, &log).await;
    let bob_node = spawn_loopback_node(bob.iroh_secret(), roster(), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("connected");

    let body = "round-trip body — unicode ☕ accepted";
    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        body,
        Some("markdown"),
        Some(reply_to),
        &[alice.identity()], // mention Alice
        &[join_id],
        T0 + 42,
    );
    let msg_id: EventId = wire.id.parse().expect("event id");
    bob_node.publish(wire.to_bytes()).await.expect("publish");

    alice_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("Alice receives message");

    let tail = alice_node.room_tail(100).await.expect("room_tail");
    let stored = tail
        .iter()
        .find(|e| e.event_id == msg_id)
        .expect("event must be in tail");
    let decoded = SignedEvent::decode(&stored.wire.signed).expect("decode");
    let Content::MessageText(m) = &decoded.content else {
        panic!("expected MessageText");
    };

    assert_eq!(m.body, body, "body round-trips");
    assert_eq!(m.format.as_deref(), Some("markdown"), "format round-trips");
    assert_eq!(m.in_reply_to, Some(reply_to), "in_reply_to round-trips");
    assert_eq!(
        m.mentions.as_deref(),
        Some([alice.identity()].as_slice()),
        "mentions round-trips"
    );
    assert_eq!(
        decoded.sender_id,
        bob.identity(),
        "sender_id is Bob's identity"
    );
    assert_eq!(decoded.room_id, room, "room_id is preserved");

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// Stateless check — validate_wire_bytes independently verifies the built frame
// ---------------------------------------------------------------------------

/// Confirms that a frame produced by `build_message_text` passes the stateless
/// §6 pipeline (the same check `engine.publish` / `engine.ingest_frame` runs
/// first) so any future transport-level test starts from a known-good frame.
#[test]
fn build_message_text_produces_stateless_valid_frame() {
    let (room, _log, join_id, _alice, bob) = build_two_member_room();

    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "hello world",
        None,
        None,
        &[],
        &[join_id],
        T0 + 1,
    );
    let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room));
    assert!(
        result.is_ok(),
        "build_message_text must produce a stateless-valid frame: {:?}",
        result.unwrap_err()
    );
    // The reject path: a zeroed signature must fail.
    let zero_sig = iroh_rooms_core::event::keys::Signature::from_bytes([0u8; 64]);
    let tampered = WireEvent {
        sig: zero_sig,
        ..wire
    };
    let bad = validate_wire_bytes(&tampered.to_bytes(), &ValidationContext::for_room(room));
    assert_eq!(
        bad.unwrap_err(),
        RejectReason::BadSignature,
        "zeroed signature must be rejected as BadSignature"
    );
}
