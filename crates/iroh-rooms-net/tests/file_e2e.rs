//! End-to-end `file.shared` send and receive (IR-0203 / issue #28 — Node-level
//! test), mirroring `message_e2e.rs`'s `message.text` coverage for the same
//! transport pipeline:
//!
//! ```text
//! build_file_shared → Node::publish → QUIC loopback → engine.deliver
//!   (validate → fold → store) → Node::room_tail
//! ```
//!
//! ## Why this file exists
//!
//! Every other `file.shared` test in the workspace stops short of the network
//! boundary: `serialization.rs` conformance vectors call `validate_wire_bytes`
//! directly; `membership_store_e2e.rs`'s `invalid_file_shared_never_persisted_or_listed`
//! seeds a single in-process `EventStore`; `file_cli.rs`'s
//! `shared_file_appears_in_list_after_validation` drives one CLI process against
//! its own room. None of them prove a `file.shared` actually **propagates from one
//! peer to another** over the transport `message.text` already proves in
//! `message_e2e.rs` (issue #28 depends on #20, "signed message send/receive", for
//! exactly this guarantee). This file closes that gap.
//!
//! * **Headline e2e (AC1–AC3)** — Bob shares a file; Alice receives it over
//!   loopback and it appears, validated, in her `room_tail` with every field
//!   intact — `blob_hash`, not bytes, per AC1.
//! * **AC4 — network-adjacent rejection** — a `file.shared` asserting
//!   `size_bytes` over `MAX_SHARED_FILE_BYTES` (the IR-0203 hardening target) is
//!   rejected by `Node::publish` before it ever reaches the wire, and
//!   independently by `SyncEngine::ingest_frame` on the receive path — so a
//!   malicious/buggy peer's claimed file size can never surface in another
//!   node's `room_tail` (the direct input to CLI `file list`).
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).
//! Membership is seeded via core event builders, mirroring `message_e2e.rs`.

use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_file_shared;
use iroh_rooms_core::event::constants::MAX_SHARED_FILE_BYTES;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine};
use iroh_rooms_net::{AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit};

// ---------------------------------------------------------------------------
// Fixtures (ported verbatim from message_e2e.rs so this file stays independent)
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xcd; 16];
const T0: u64 = 1_750_000_100_000;
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
    let alice = Principal::new(0x21);
    let bob = Principal::new(0x30);
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let mut log: Vec<Vec<u8>> = Vec::new();

    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "File E2E Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device_key()),
        }),
    };
    let genesis_id = genesis.event_id();
    log.push(wire_bytes(&genesis, &alice.dev));

    let inv_id = [0x02u8; 16];
    let inv_sec = [0x42u8; 16];
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
async fn spawn_loopback_node(
    secret: SecretKey,
    admission: AllowlistAdmission,
    room: RoomId,
    log: &[Vec<u8>],
) -> Node {
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
}

/// A valid `file.shared` frame from Bob referencing a fixed blob hash, parented
/// on the room heads (`join_id`).
fn bob_file_shared(bob: &Principal, room: RoomId, join_id: EventId, size_bytes: u64) -> WireEvent {
    build_file_shared(
        &bob.id,
        &bob.dev,
        &room,
        [0x30; 16],
        "notes.md",
        "text/markdown",
        size_bytes,
        HashRef::from_bytes([0x99; 32]),
        Some("raw"),
        &[],
        &[join_id],
        T0 + 10,
    )
}

// ---------------------------------------------------------------------------
// Headline e2e — Bob shares a file; Alice receives it via room_tail (AC1–AC3)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_peers_exchange_file_shared_via_loopback() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(alice.iroh_secret(), roster(), room, &log).await;
    let bob_node = spawn_loopback_node(bob.iroh_secret(), roster(), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("Alice addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob connected");

    let wire = bob_file_shared(&bob, room, join_id, 204_800);
    let wire_frame = wire.to_bytes();
    let file_event_id: EventId = wire.id.parse().expect("valid event id from wire.id");

    bob_node
        .publish(wire_frame)
        .await
        .expect("Bob publishes file.shared");

    // Alice waits until the event reaches her store, then it must appear in her
    // room_tail — the direct input to CLI `file list` — with every field intact.
    alice_node
        .wait_until_contains(file_event_id, WAIT)
        .await
        .expect("Alice's store contains the file.shared event");

    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let file_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::FileShared)
        .collect();
    assert_eq!(
        file_events.len(),
        1,
        "exactly one file.shared in Alice's tail"
    );
    let stored_ev = &file_events[0];
    let decoded = SignedEvent::decode(&stored_ev.wire.signed).expect("decode");
    let Content::FileShared(f) = &decoded.content else {
        panic!("expected FileShared content");
    };
    assert_eq!(f.name, "notes.md", "name round-trips through the transport");
    assert_eq!(f.mime_type, "text/markdown", "mime_type round-trips");
    assert_eq!(f.size_bytes, 204_800, "size_bytes round-trips");
    assert_eq!(
        f.blob_hash,
        HashRef::from_bytes([0x99; 32]),
        "AC1: the event carries the blob hash, not file bytes"
    );
    assert_eq!(
        decoded.sender_id,
        bob.identity(),
        "sender_id is Bob's identity"
    );

    // The engine must contain it on Bob's node too (he published it locally).
    assert!(
        bob_node
            .store_contains(file_event_id)
            .await
            .expect("store_contains"),
        "Bob's own store contains the file.shared he sent"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC4 — an over-cap file.shared is rejected before it reaches the wire
// ---------------------------------------------------------------------------

/// A `file.shared` asserting `size_bytes` over `MAX_SHARED_FILE_BYTES` is caught
/// by `validate_wire_bytes` inside `engine.publish` on the publishing node and
/// never reaches the network — the same stateless-first defence `message_e2e.rs`
/// proves for a tampered signature.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_with_oversized_file_shared_is_rejected_before_network() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let alice_node =
        spawn_loopback_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node =
        spawn_loopback_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");

    // The exact absurd-size vector IR-0203 exists to close: a peer asserting an
    // unbounded file.
    let oversized = bob_file_shared(&bob, room, join_id, u64::MAX);

    let err = bob_node
        .publish(oversized.to_bytes())
        .await
        .expect_err("publishing an over-cap file.shared must fail");
    assert!(
        err.to_string().to_lowercase().contains("invalid"),
        "error must name the invalid frame: {err}"
    );

    // Alice must have received nothing (the frame never reached the wire).
    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let file_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::FileShared)
        .collect();
    assert!(
        file_events.is_empty(),
        "AC4: over-cap file.shared must not appear in Alice's tail"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ---------------------------------------------------------------------------
// AC4 — an over-cap file.shared injected directly into the engine is dropped
// ---------------------------------------------------------------------------

/// Tests the inbound receive-side path directly: a frame that arrives over the
/// wire asserting an over-cap `size_bytes` is caught by `engine.ingest_frame`
/// and never reaches the store — independent of the publish-side guard above,
/// proving the receive boundary itself enforces AC4 (relevant if a peer's
/// engine ever ingests a frame that did not originate from `Node::publish`,
/// e.g. a non-conforming client). Mirrors
/// `message_e2e::engine_ingest_frame_drops_tampered_signature`.
#[test]
fn engine_ingest_frame_drops_oversized_file_shared() {
    let (room, log, join_id, _alice, bob) = build_two_member_room();

    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("engine");
    for ev in &log {
        engine.publish(ev).expect("seed");
    }

    let oversized = bob_file_shared(&bob, room, join_id, MAX_SHARED_FILE_BYTES + 1);

    let peer = PeerId::from_bytes([0xCC; 32]);
    let _ = engine.ingest_frame(peer, &oversized.to_bytes());

    let tail = engine.room_tail(100).expect("room_tail");
    let file_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::FileShared)
        .collect();
    assert!(
        file_events.is_empty(),
        "AC4: engine must drop an over-cap file.shared (not add it to room_tail)"
    );
}
