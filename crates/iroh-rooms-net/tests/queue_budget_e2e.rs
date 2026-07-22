//! End-to-end coverage for issue #141 — byte-bounded priority queues on the v1
//! event-plane transport (`#134 §12.3`).
//!
//! The queue logic itself (byte accounting, per-peer/per-stream caps, the four
//! priority classes, governance skip-ahead) is pinned by unit tests in
//! `queue.rs` and `transport.rs`. Those tests exercise [`Shared::route`] and
//! [`crate::queue::BytePriorityQueue`] **in isolation** — without the spawned
//! writer/reader tasks, without QUIC, and without the engine pump. What only an
//! end-to-end run can prove is that the byte budgets configured in
//! [`NetConfig`] are actually plumbed through to the **live** per-peer queues
//! (built in `peer::register_connection`), that frames still flow across real
//! QUIC loopback with the priority queues in place on both sides, and that the
//! saturation → `transport.queue.saturated` audit → link-close recovery path
//! fires through the real `writer_task` / `reader_task` (not just a direct
//! `Shared::route` call).
//!
//! Boundary under test: `Node::publish` → engine fan-out → `Shared::route` →
//! **byte-bounded `OutboundQueue`** → `writer_task` → QUIC loopback →
//! `reader_task` → **byte-bounded inbound sink** → engine pump → store. All
//! tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).
//!
//! Acceptance coverage (issue #141):
//! * **AC1 / "depth reports bytes"** — `byte_budgets_carry_message_traffic`:
//!   two nodes configured with deliberately small (non-default) byte caps
//!   exchange a `message.text`; the frame lands in the receiver's store. Proves
//!   `NetConfig.{inbound_peer_queue_bytes, outbound_peer_queue_bytes,
//!   stream_queue_bytes}` reach the live per-peer queues and frames flow.
//! * **AC "drop frame, audit, close link" (outbound)** —
//!   `outbound_saturation_audits_and_closes_live_link`: a sender whose outbound
//!   peer cap is `1` (every encoded body saturates) drives the real
//!   `writer_task` path; the recording audit captures
//!   `transport_queue_saturated(peer, "outbound")`.
//! * **AC "drop frame, audit, close link" (inbound)** —
//!   `inbound_saturation_audits_and_closes_live_link`: a receiver whose inbound
//!   peer cap is `1` drives the real `reader_task` path; the recording audit
//!   captures `transport_queue_saturated(peer, "inbound")`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_message_text;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AllowlistAdmission, AuditSink, NetConfig, NetMode, Node, PeerConnState, RejectCause,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Deterministic room nonce — distinct from every other test file so parallel
/// runs never collide on the derived `RoomId`.
const NONCE: [u8; 16] = [0x71; 16];
/// Base timestamp (advisory wall-clock anchor).
const T0: u64 = 1_777_100_000_000;
/// Generous wait for a loopback round-trip (CI can stall briefly under load).
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

/// Build a two-member room log: genesis (Alice=admin) → Bob's invite → Bob's
/// join.
///
/// Returns `(room_id, [genesis, invite, join], join_event_id, alice, bob)`.
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
            room_name: "Queue Budget E2E".to_owned(),
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

/// Build an allowlist admitting all `members` (bind device → identity, Active).
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
/// `large_futures`).
fn spawn_loopback_node(
    secret: SecretKey,
    admission: AllowlistAdmission,
    audit: Arc<dyn AuditSink>,
    room: RoomId,
    log: &[Vec<u8>],
    cfg: NetConfig,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        for ev in log {
            engine.publish(ev).expect("seed event");
        }
        Node::spawn(
            secret,
            Arc::new(admission),
            audit,
            engine,
            cfg,
            Duration::from_millis(100),
        )
        .await
        .expect("spawn loopback node")
    })
}

// ---------------------------------------------------------------------------
// Recording audit sink — captures `transport.queue.saturated` callbacks.
// ---------------------------------------------------------------------------

/// `(device, queue)` for each saturation callback observed on this node.
type SaturationCall = (EndpointId, &'static str);

#[derive(Default, Clone)]
struct RecordingAudit {
    saturations: Arc<Mutex<Vec<SaturationCall>>>,
}

impl RecordingAudit {
    fn saturations(&self) -> Vec<SaturationCall> {
        self.saturations.lock().expect("audit lock").clone()
    }
}

impl AuditSink for RecordingAudit {
    fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {}
    fn rejected(&self, _device: EndpointId, _cause: RejectCause) {}
    fn connected(&self, _device: EndpointId) {}
    fn disconnected(&self, _device: EndpointId) {}

    fn transport_queue_saturated(&self, device: EndpointId, queue: &'static str) {
        self.saturations
            .lock()
            .expect("audit lock")
            .push((device, queue));
    }
}

/// Poll `audit` until at least one saturation callback lands, or panic on
/// `timeout`. The saturation path runs through real async I/O tasks, so a poll
/// is the deterministic way to observe it (a single point-in-time read races
/// the writer/reader task schedulers).
async fn wait_for_saturation(audit: &RecordingAudit, timeout: Duration) -> Vec<SaturationCall> {
    tokio::time::timeout(timeout, async {
        loop {
            let calls = audit.saturations();
            if !calls.is_empty() {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transport.queue.saturated must fire on the live transport within {timeout:?}")
}

// ===========================================================================
// AC1: configured byte budgets carry message traffic over real QUIC loopback
// ===========================================================================

/// Two nodes configured with deliberately small (non-default) byte caps — well
/// below the 8 MiB / 2 MiB §12.3 defaults — exchange a `message.text` frame
/// across real QUIC loopback. The frame lands in the receiver's validated
/// store.
///
/// This proves the integration that only an end-to-end run can reach:
/// `NetConfig.{inbound_peer_queue_bytes, outbound_peer_queue_bytes,
/// stream_queue_bytes}` flow through `NetTransport::bind` → `Shared` →
/// `peer::register_connection` → the live per-peer `OutboundQueue`, and the
/// byte-priority queues (both outbound, drained by `writer_task`, and inbound,
/// drained by the engine pump) carry real `SyncMessage` traffic without
/// breakage. If the queue wiring were reverted or the caps ignored, the frame
/// would not arrive (or would saturate at the small cap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byte_budgets_carry_message_traffic_over_loopback() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    // Small but non-saturating caps: a message.text frame is ~200-300 encoded
    // bytes, and the on_connect handshake produces a handful of ~100-200-byte
    // frames. 8 KiB peer / 4 KiB stream is far below the §12.3 defaults (so the
    // test exercises the configured, not default, plumbing) yet leaves ample
    // headroom for the handshake + one message.
    let cfg = || NetConfig {
        mode: NetMode::Loopback,
        inbound_peer_queue_bytes: 8 * 1024,
        outbound_peer_queue_bytes: 8 * 1024,
        stream_queue_bytes: 4 * 1024,
        ..NetConfig::default()
    };

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(
        alice.iroh_secret(),
        roster(),
        Arc::new(RecordingAudit::default()),
        room,
        &log,
        cfg(),
    )
    .await;
    let bob_node = spawn_loopback_node(
        bob.iroh_secret(),
        roster(),
        Arc::new(RecordingAudit::default()),
        room,
        &log,
        cfg(),
    )
    .await;

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

    // Bob publishes a message.text (content/publication family — charges both
    // the per-peer and per-stream byte buckets).
    let body = "byte-budgeted queue carries this over the wire";
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
    let msg_id: EventId = wire.id.parse().expect("valid event id from wire.id");
    bob_node
        .publish(wire.to_bytes())
        .await
        .expect("Bob publishes message.text");

    // Alice's store must contain the event — the frame traversed Bob's
    // byte-bounded outbound queue, the QUIC loopback, and Alice's
    // byte-bounded inbound sink.
    alice_node
        .wait_until_contains(msg_id, WAIT)
        .await
        .expect("Alice receives the message through the byte-budgeted transport");

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ===========================================================================
// Outbound saturation → audit + close, through the real writer_task
// ===========================================================================

/// A sender whose outbound per-peer byte cap is `1` cannot fit any encoded
/// `SyncMessage` body (every frame is hundreds of bytes), so the first fan-out
/// — whether from the engine's `on_connect` handshake or from an explicit
/// `publish` — saturates the live `OutboundQueue` inside `Shared::route`,
/// audits `transport.queue.saturated` with queue `outbound`, and closes the
/// link.
///
/// This drives the real `writer_task` / `Shared::route` recovery path that the
/// unit test `route_closes_peer_when_outbound_queue_is_full` covers in
/// isolation: here the queue is the live one built by
/// `peer::register_connection`, the audit fires through the real pump's
/// `on_connect` / `publish` fan-out, and the close hits a real QUIC connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_saturation_audits_and_closes_live_link() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    // Sender (Bob): outbound cap = 1 → every body saturates. Receiver (Alice):
    // default caps so her side of the link is not the thing under test.
    let sender_cfg = NetConfig {
        mode: NetMode::Loopback,
        outbound_peer_queue_bytes: 1,
        ..NetConfig::default()
    };
    let receiver_cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };

    let sender_audit = Arc::new(RecordingAudit::default());
    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(
        alice.iroh_secret(),
        roster(),
        Arc::new(RecordingAudit::default()),
        room,
        &log,
        receiver_cfg,
    )
    .await;
    let bob_node = spawn_loopback_node(
        bob.iroh_secret(),
        roster(),
        sender_audit.clone(),
        room,
        &log,
        sender_cfg,
    )
    .await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("Alice addr"));

    // Publish from Bob to guarantee an outbound fan-out even if the engine's
    // on_connect handshake produced no frames for this room state. The publish
    // succeeds locally (the frame is ingested into Bob's own store); the
    // fan-out to Alice is what saturates.
    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "saturates the byte budget",
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let _ = bob_node.publish(wire.to_bytes()).await;

    // The sender's recording audit must capture an outbound saturation against
    // Alice's device. The dial loop redials on close, so this fires on the
    // first attempt that reaches Connected + fan-out — well within the wait.
    let calls = wait_for_saturation(&sender_audit, WAIT).await;
    let alice_dev = alice.endpoint_id();
    assert!(
        calls
            .iter()
            .any(|(dev, queue)| *dev == alice_dev && *queue == "outbound"),
        "expected transport.queue.saturated outbound against Alice; got {calls:?}"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

// ===========================================================================
// Inbound saturation → audit + close, through the real reader_task
// ===========================================================================

/// A receiver whose inbound per-peer byte cap is `1` cannot fit any encoded
/// `SyncMessage` body, so the first frame the real `reader_task` reads from
/// the QUIC stream saturates the byte-bounded inbound sink inside
/// `Shared::try_enqueue_inbound`, audits `transport.queue.saturated` with queue
/// `inbound`, and closes the link.
///
/// This drives the real `reader_task` recovery path that the unit test
/// `try_enqueue_inbound_enforces_byte_caps_and_prioritizes_governance` covers
/// in isolation: here the inbound sink is the live one built by
/// `NetTransport::bind`, the frame arrives over a real QUIC loopback, and the
/// audit fires inside the spawned reader task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_saturation_audits_and_closes_live_link() {
    let (room, log, join_id, alice, bob) = build_two_member_room();

    // Sender (Bob): default caps so frames actually leave the writer. Receiver
    // (Alice): inbound cap = 1 → the reader_task saturates on the first frame.
    let sender_cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    let receiver_cfg = NetConfig {
        mode: NetMode::Loopback,
        inbound_peer_queue_bytes: 1,
        ..NetConfig::default()
    };

    let receiver_audit = Arc::new(RecordingAudit::default());
    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = spawn_loopback_node(
        alice.iroh_secret(),
        roster(),
        receiver_audit.clone(),
        room,
        &log,
        receiver_cfg,
    )
    .await;
    let bob_node = spawn_loopback_node(
        bob.iroh_secret(),
        roster(),
        Arc::new(RecordingAudit::default()),
        room,
        &log,
        sender_cfg,
    )
    .await;

    bob_node.connect_to(alice_node.endpoint_addr().expect("Alice addr"));
    // Wait for the link to come up so Bob's publish fans out toward a live
    // peer (a silent drop for an offline peer would never reach Alice's
    // reader_task). on_connect already pushes handshake frames, but a Connected
    // wait + publish guarantees an inbound read on Alice regardless of
    // handshake timing.
    let _ = bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await;

    let wire = build_message_text(
        &bob.id,
        &bob.dev,
        &room,
        "saturates the inbound byte budget",
        None,
        None,
        &[],
        &[join_id],
        T0 + 10,
    );
    let _ = bob_node.publish(wire.to_bytes()).await;

    // The receiver's recording audit must capture an inbound saturation against
    // Bob's device. The reader_task closes the link on saturation, so this
    // fires on the first frame Alice reads — well within the wait.
    let calls = wait_for_saturation(&receiver_audit, WAIT).await;
    let bob_dev = bob.endpoint_id();
    assert!(
        calls
            .iter()
            .any(|(dev, queue)| *dev == bob_dev && *queue == "inbound"),
        "expected transport.queue.saturated inbound against Bob; got {calls:?}"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}
