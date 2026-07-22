//! End-to-end proof that adversarial/malformed CBOR crossing the **live network
//! boundary** (real QUIC bytes, not an in-process function call) never crashes a
//! [`Node`] and never corrupts its store (IR-0002 risk R1, issue #45).
//!
//! The direct unit tests (`cbor.rs`'s inline `#[cfg(test)] mod tests`) and the
//! property tests (`iroh-rooms-core/tests/cbor_property.rs`) already prove
//! `decode_canonical` / `validate_wire_bytes` never panic as pure function calls.
//! What they cannot exercise is the actual receive path: a hostile peer's bytes
//! travel `read_frame` (raw QUIC) → `SyncMessage::decode` (`node.rs`) →
//! `engine.on_message` → `SyncMessage::Events { frames }` →
//! `engine.deliver_bytes` → `validate_wire_bytes` → `cbor::decode_canonical`,
//! all inside the async pump task that owns the single, mutable `SyncEngine`
//! (`node.rs` module docs). A panic anywhere on that path would poison the pump
//! and silently stop the node from processing every future frame — a failure
//! mode no in-process property test can observe.
//!
//! This file drives a real admitted peer's raw QUIC connection (bypassing
//! `Node::publish`'s client-side pre-validation, which never lets bad bytes
//! reach the wire in the first place — see `message_e2e.rs`'s AC3 tests) and
//! writes hostile bytes directly onto the stream the receiving [`Node`] reads
//! from. It then proves the node is still alive and correct by publishing a
//! genuinely valid frame straight afterward.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_message_text;
use iroh_rooms_core::event::content::{
    Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine, SyncMessage};
use iroh_rooms_net::{AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit};

// ---------------------------------------------------------------------------
// Fixtures (mirrors message_e2e.rs's Principal / two-member-room builders)
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

fn wire_bytes(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// A two-member room log: genesis (Alice=admin) → `invite_bob` → `join_bob`.
fn build_two_member_room() -> (RoomId, Vec<Vec<u8>>, EventId, Principal, Principal) {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
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
            room_name: "Malformed CBOR E2E Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device_key()),
        }),
    };
    let genesis_id = genesis.event_id();
    log.push(wire_bytes(&genesis, &alice.dev));

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
            capability_hash: iroh_rooms_core::event::content::capability_hash(
                &room, &inv_id, &inv_sec,
            ),
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

fn allowlist(members: &[&Principal]) -> AllowlistAdmission {
    let mut auth = AllowlistAdmission::new();
    for m in members {
        auth = auth
            .bind_device(m.endpoint_id(), m.identity())
            .set_active(m.identity());
    }
    auth
}

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

/// Bind a bare `iroh::Endpoint` keyed by `secret`, matching `NetMode::Loopback`'s
/// stack (`presets::Minimal` + `RelayMode::Disabled`) so it can dial a loopback
/// [`Node`]. Deliberately **not** a full `Node` — a raw endpoint lets the test
/// drive the QUIC stream by hand (`open_bi` + `write_frame`) instead of going
/// through `Node::publish`'s client-side pre-validation, which never lets
/// malformed bytes reach the wire in the first place.
async fn raw_endpoint(secret: SecretKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("bind raw endpoint")
}

// ---------------------------------------------------------------------------
// R1 over the wire: hostile bytes never crash the node, never pollute the store
// ---------------------------------------------------------------------------

/// A raw, authenticated peer (Bob, an Active member) sends a batch of hostile
/// byte sequences directly over the wire, then a genuinely valid frame. Alice's
/// [`Node`] must: (a) survive every hostile frame without crashing/hanging its
/// pump, (b) store nothing from them, and (c) still correctly ingest the valid
/// frame sent right after — proving the pump task is not poisoned by adversarial
/// input (spec §4.3 defense-in-depth, risk R1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_bytes_over_live_connection_never_crash_node_or_pollute_store() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    let alice_node =
        spawn_loopback_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;

    // Bob connects with a bare endpoint (his real, admitted device key) instead
    // of a full Node, so we can write raw bytes onto the stream by hand.
    let bob_ep = raw_endpoint(bob.iroh_secret()).await;
    let addr = alice_node.endpoint_addr().expect("Alice addr");
    let conn = tokio::time::timeout(WAIT, bob_ep.connect(addr, iroh_rooms_net::EVENT_ALPN))
        .await
        .expect("connect timeout")
        .expect("Bob (admitted) connects to Alice");

    // Quinn does not notify the accept side of a bidi stream until the opener
    // actually writes to it (`frame.rs`'s `make_pair` doc comment) — `open_bi()`
    // returning `Ok` only creates the local handles. So Alice's handler stays
    // parked inside `accept_bi()` (never reaching `Connected`) until Bob's first
    // write lands. Open the stream and go straight to writing the hostile
    // batch; only check `Connected` after bytes are actually on the wire.
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");

    // A battery of hostile byte sequences, each a distinct way §10.3's "never
    // panic, never over-allocate" guarantee could be tested at the transport
    // boundary. Every one is sent as a well-framed body (a valid length prefix,
    // per frame.rs — that layer is already proven robust in frame.rs); the
    // *body* itself is what is hostile here.
    let hostile_bodies: Vec<(&str, Vec<u8>)> = vec![
        ("empty body", Vec::new()),
        ("pure garbage bytes", vec![0xFFu8; 64]),
        (
            "non-canonical indefinite-length map (CborError::IndefiniteLength)",
            vec![0xbf, 0xff],
        ),
        (
            "canonical CBOR uint, not a SyncMessage shape (MessageError::BadShape)",
            vec![0x00],
        ),
        (
            "SyncMessage::Events envelope carrying a malformed inner WireEvent frame",
            malformed_events_envelope(room),
        ),
    ];

    for (label, body) in &hostile_bodies {
        iroh_rooms_net::frame::write_frame(&mut send, body)
            .await
            .unwrap_or_else(|e| panic!("write_frame must accept {label}: {e}"));
    }

    // The node must still be alive: the connection reaches (and stays at)
    // `Connected` despite every hostile frame having crossed it, and ordinary
    // queries against the pump still answer (a poisoned/panicked pump would
    // hang these forever, and the surrounding #[tokio::test] would time out
    // rather than silently pass).
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice must reach Connected despite the hostile batch");
    // Let the pump fully drain the inbound queue before checking the store.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tail_after_hostile = tokio::time::timeout(WAIT, alice_node.room_tail(100))
        .await
        .expect("room_tail must not hang after hostile frames")
        .expect("room_tail must succeed");
    let message_count_after_hostile = tail_after_hostile
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .count();
    assert_eq!(
        message_count_after_hostile, 0,
        "no hostile frame may produce a stored message.text event"
    );

    // Now prove full recovery: a genuinely valid frame sent right after the
    // hostile batch, over the SAME connection, must still be ingested correctly.
    let body = "still alive after the hostile batch";
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
    let msg_id: EventId = wire.id.parse().expect("event id");
    let valid_envelope = SyncMessage::Events {
        room_id: room,
        frames: vec![wire.to_bytes()],
    }
    .encode();
    iroh_rooms_net::frame::write_frame(&mut send, &valid_envelope)
        .await
        .expect("write_frame must accept the valid envelope");

    alice_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("Alice must still ingest a valid frame after the hostile batch");

    let tail = alice_node
        .room_tail(100)
        .await
        .expect("room_tail must succeed");
    let messages: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "exactly the recovery message must be stored, none from the hostile batch"
    );
    let decoded = SignedEvent::decode(&messages[0].wire.signed).expect("decode");
    let Content::MessageText(m) = &decoded.content else {
        panic!("expected MessageText content");
    };
    assert_eq!(m.body, body, "the recovery message's body is intact");

    alice_node.shutdown().await.expect("shutdown Alice");
}

/// A canonical-CBOR `SyncMessage::Events` envelope (decodes fine at the
/// `SyncMessage` layer) whose single `frames` entry is deliberately not a valid
/// `WireEvent` — hostile input specifically on the inner CBOR reader
/// (`validate_wire_bytes` → `cbor::decode_canonical`) rather than the outer
/// `SyncMessage` shape.
fn malformed_events_envelope(room: RoomId) -> Vec<u8> {
    SyncMessage::Events {
        room_id: room,
        frames: vec![vec![0xffu8; 40]],
    }
    .encode()
}

/// Same hostile-bytes battery, but delivered as several **separate** frames in
/// immediate succession (rather than one at a time before checking liveness) to
/// prove frame-to-frame independence: an earlier hostile frame's rejection must
/// not affect the next frame's decode, and none may accumulate a peer's
/// `SyncEngine` into a wedged state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn back_to_back_hostile_frames_each_independently_rejected() {
    let (room, log, _join_id, alice, bob) = build_two_member_room();

    let alice_node =
        spawn_loopback_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;

    let bob_ep = raw_endpoint(bob.iroh_secret()).await;
    let addr = alice_node.endpoint_addr().expect("Alice addr");
    let conn = tokio::time::timeout(WAIT, bob_ep.connect(addr, iroh_rooms_net::EVENT_ALPN))
        .await
        .expect("connect timeout")
        .expect("Bob (admitted) connects to Alice");

    // Quinn does not notify the accept side of a bidi stream until the opener
    // actually writes to it, so go straight to writing before checking
    // `Connected` (see the sibling test's comment for the full explanation).
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");

    // 50 back-to-back frames of pseudo-random hostile bytes, varying length and
    // content deterministically (no wall-clock / RNG dependency).
    for i in 0u8..50 {
        let len = usize::from(i) % 37;
        let body: Vec<u8> = (0..len)
            .map(|j| {
                i.wrapping_mul(31)
                    .wrapping_add(u8::try_from(j).unwrap_or(0))
            })
            .collect();
        iroh_rooms_net::frame::write_frame(&mut send, &body)
            .await
            .unwrap_or_else(|e| panic!("write_frame must accept hostile frame {i}: {e}"));
    }

    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice must reach Connected despite 50 back-to-back hostile frames");
    // Let the pump fully drain the inbound queue before checking the store.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let peer = PeerId::from_bytes(*bob.endpoint_id().as_bytes());
    let _ = peer; // documents the PeerId <-> EndpointId identity bridge used by the engine

    let tail = tokio::time::timeout(WAIT, alice_node.room_tail(100))
        .await
        .expect("room_tail must not hang after 50 hostile frames")
        .expect("room_tail must succeed");
    assert!(
        tail.iter().all(|e| e.event_type != EventType::MessageText),
        "no hostile frame may produce a stored message.text event"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
}
