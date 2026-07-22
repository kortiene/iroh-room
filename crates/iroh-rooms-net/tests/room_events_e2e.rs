//! Two-node end-to-end proof of the push-subscription feed (issue #83 / IR-0307).
//!
//! `message_e2e.rs`/`join_e2e.rs` already prove the transport carries frames;
//! this file proves `Node::room_events()` surfaces them **exactly once**, even
//! when a later-authored event crosses the wire before its causal parent — the
//! headline scenario the issue calls out (a late-arriving low-lamport event
//! must not be silently dropped from the push stream).
//!
//! ## Inducing genuine out-of-order delivery
//!
//! Alice (admin) authors a two-deep admin chain — `invite_carol1` then
//! `invite_carol2` (parented on `invite_carol1`) — entirely offline, while Bob
//! is not yet connected, so nothing is fanned out yet. Only once Bob connects
//! does Alice advertise her heads: `invite_carol2` is the deepest leaf, so Bob
//! requests and receives it *first*. Its parent (`invite_carol1`) is missing,
//! so Bob's engine **parks** it — a genuine wire-level out-of-order arrival, not
//! a simulated one. Alice then backfills `invite_carol1`, which Bob accepts
//! directly and which triggers `wake_park`, promoting the parked
//! `invite_carol2` in the same `on_message` drive.
//!
//! Runs unmarked in CI: two in-process nodes on `NetMode::Loopback`, no
//! external process, mirroring this crate's `message_e2e.rs`/`join_e2e.rs`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit};

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

/// A two-member baseline room log: genesis (Alice=admin) → `invite_bob` →
/// `join_bob`. Both Alice's and Bob's nodes are pre-seeded with this identical
/// log, so connecting them exchanges nothing for it — only the admin chain
/// authored afterward is genuinely novel to Bob.
fn build_baseline() -> (RoomId, Vec<Vec<u8>>, EventId, Principal, Principal) {
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
            room_name: "Room Events E2E".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device_key()),
        }),
    };
    let genesis_id = genesis.event_id();
    log.push(wire_bytes(&genesis, &alice.dev));

    let inv_id = [0x01u8; 16];
    let inv_sec = [0x41u8; 16];
    let invite_bob = SignedEvent {
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
    let invite_bob_id = invite_bob.event_id();
    log.push(wire_bytes(&invite_bob, &alice.dev));

    let join_bob = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: bob.identity(),
        device_id: bob.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T0 + 2,
        prev_events: vec![invite_bob_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_id,
            capability_secret: inv_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &bob.id, bob.device_key()),
            display_name: Some("Bob".to_owned()),
        }),
    };
    log.push(wire_bytes(&join_bob, &bob.dev));

    (room, log, invite_bob_id, alice, bob)
}

/// Build a two-deep admin chain (`MemberInvited` for two never-redeemed
/// invitees), each citing the *prior admin event* as parent so `admin_seq`
/// flows without forking — mirrors `iroh-rooms-core/tests/sync_smoke.rs`'s
/// `build_log` convention. Neither invite is ever joined; they exist purely as
/// a causally-dependent pair to induce out-of-order delivery.
fn build_admin_chain(
    alice: &Principal,
    room: RoomId,
    parent: EventId,
) -> (EventId, EventId, Vec<u8>, Vec<u8>) {
    let carol1 = Principal::new(0x20).identity();
    let carol2 = Principal::new(0x21).identity();

    let id1 = [0x02u8; 16];
    let sec1 = [0x42u8; 16];
    let invite1 = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T0 + 10,
        prev_events: vec![parent],
        content: Content::MemberInvited(MemberInvited {
            invite_id: id1,
            capability_hash: capability_hash(&room, &id1, &sec1),
            role: "member".to_owned(),
            invitee_key: carol1,
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite1_id = invite1.event_id();
    let invite1_bytes = wire_bytes(&invite1, &alice.dev);

    let id2 = [0x03u8; 16];
    let sec2 = [0x43u8; 16];
    let invite2 = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T0 + 11,
        prev_events: vec![invite1_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id: id2,
            capability_hash: capability_hash(&room, &id2, &sec2),
            role: "member".to_owned(),
            invitee_key: carol2,
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite2_id = invite2.event_id();
    let invite2_bytes = wire_bytes(&invite2, &alice.dev);

    (invite1_id, invite2_id, invite1_bytes, invite2_bytes)
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

fn spawn_loopback_node_with_capacity(
    secret: SecretKey,
    admission: AllowlistAdmission,
    room: RoomId,
    log: &[Vec<u8>],
    room_event_capacity: usize,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        for ev in log {
            engine.publish(ev).expect("seed event");
        }
        let cfg = NetConfig {
            mode: NetMode::Loopback,
            room_event_capacity,
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

fn spawn_loopback_node(
    secret: SecretKey,
    admission: AllowlistAdmission,
    room: RoomId,
    log: &[Vec<u8>],
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    spawn_loopback_node_with_capacity(
        secret,
        admission,
        room,
        log,
        NetConfig::default().room_event_capacity,
    )
}

/// Build `count` chained `MemberInvited` events (never redeemed), each citing
/// the previous as parent so `admin_seq` advances without forking — a burst
/// that's cheap for Alice to author and guaranteed to land in Bob's store as
/// `count` separate inserts (hence `count` separate `room_events` sends).
fn build_admin_invite_burst(
    alice: &Principal,
    room: RoomId,
    mut parent: EventId,
    count: u8,
) -> (Vec<EventId>, Vec<Vec<u8>>) {
    let mut ids = Vec::with_capacity(count as usize);
    let mut bytes = Vec::with_capacity(count as usize);
    for i in 0..count {
        let invitee = Principal::new(0x50 + i).identity();
        let invite_id = [0x50 + i; 16];
        let sec = [0x60 + i; 16];
        let invite = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device_key(),
            event_type: EventType::MemberInvited,
            created_at: T0 + 20 + u64::from(i),
            prev_events: vec![parent],
            content: Content::MemberInvited(MemberInvited {
                invite_id,
                capability_hash: capability_hash(&room, &invite_id, &sec),
                role: "member".to_owned(),
                invitee_key: invitee,
                expires_at: None,
                invitee_hint: None,
            }),
        };
        let id = invite.event_id();
        bytes.push(wire_bytes(&invite, &alice.dev));
        ids.push(id);
        parent = id;
    }
    (ids, bytes)
}

/// AC-1 headline: a two-hop causal chain delivered out of order over real
/// loopback QUIC is emitted **exactly once per event**, with the directly-
/// accepted trigger (`invite_carol1`) recorded before the promoted descendant
/// (`invite_carol2`) it unparked — set + trigger-first, never asserted as a
/// full causal sequence (§6.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_events_two_node_out_of_order() {
    let (room, baseline, invite_bob_id, alice, bob) = build_baseline();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = Box::pin(spawn_loopback_node(
        alice.iroh_secret(),
        roster(),
        room,
        &baseline,
    ))
    .await;
    let bob_node = Box::pin(spawn_loopback_node(
        bob.iroh_secret(),
        roster(),
        room,
        &baseline,
    ))
    .await;

    // Subscribe before anything carol-related exists, so no live-tap events are
    // missed. No `.await` intervenes before this call, so the freshly spawned
    // pump cannot have run a tick yet.
    let mut room_events = bob_node.room_events();

    // Alice authors the two-deep admin chain while Bob is still disconnected:
    // nothing is fanned out yet, both events land only in Alice's own store.
    let (invite1_id, invite2_id, invite1_bytes, invite2_bytes) =
        build_admin_chain(&alice, room, invite_bob_id);
    alice_node
        .publish(invite1_bytes)
        .await
        .expect("alice publishes invite_carol1 (offline, no fanout yet)");
    alice_node
        .publish(invite2_bytes)
        .await
        .expect("alice publishes invite_carol2 (offline, no fanout yet)");

    // Now connect: Alice's advertised heads include invite_carol2 (the deepest
    // leaf) but not invite_carol1 (no longer a head), so Bob requests and
    // receives invite_carol2 first, parks it, backfills invite_carol1, then
    // wake_park promotes invite_carol2 in the same drive.
    bob_node.connect_to(alice_node.endpoint_addr().expect("alice addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob connected");

    bob_node
        .wait_until_contains(invite1_id, WAIT)
        .await
        .expect("Bob's store contains invite_carol1 (backfilled parent)");
    bob_node
        .wait_until_contains(invite2_id, WAIT)
        .await
        .expect("Bob's store contains invite_carol2 (the out-of-order child)");

    // Drain room_events until both carol ids have been observed (or time out).
    // Filter to just these two: the baseline seed events may or may not have
    // already flushed on Bob's first tick before we subscribed, and that race
    // is irrelevant to what this test proves.
    let mut seen: Vec<EventId> = Vec::new();
    tokio::time::timeout(WAIT, async {
        loop {
            match room_events.recv().await {
                Ok(ev) if ev.event_id == invite1_id || ev.event_id == invite2_id => {
                    seen.push(ev.event_id);
                    if seen.len() >= 2 {
                        return;
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("room_events closed before both carol events arrived")
                }
            }
        }
    })
    .await
    .expect("room_events must deliver both carol events");

    // Exactly-once: no duplicate emission for either id (AC-1).
    assert_eq!(
        seen.iter().filter(|id| **id == invite1_id).count(),
        1,
        "invite_carol1 must be emitted exactly once"
    );
    assert_eq!(
        seen.iter().filter(|id| **id == invite2_id).count(),
        1,
        "invite_carol2 must be emitted exactly once"
    );
    // Set-membership holds regardless of order; trigger-first additionally
    // requires the directly-accepted parent before its promoted child (§6.2).
    assert_eq!(
        seen,
        vec![invite1_id, invite2_id],
        "the directly-accepted trigger (invite_carol1) must be emitted before \
         the park-promoted descendant (invite_carol2) it unparked"
    );

    // The set also matches the authoritative store (no silent drop).
    let tail = bob_node
        .room_tail(u32::MAX)
        .await
        .expect("room_tail must succeed");
    assert!(
        tail.iter().any(|e| e.event_id == invite1_id),
        "invite_carol1 must be in room_tail"
    );
    assert!(
        tail.iter().any(|e| e.event_id == invite2_id),
        "invite_carol2 must be in room_tail"
    );

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}

/// AC-2: a subscriber that falls behind observes `RecvError::Lagged`, never
/// silent loss. `node.rs`'s `room_events_pump_tests::drain_lagged_then_recovers`
/// already proves this at the pump level by feeding a bare broadcast channel
/// directly into `drain_room_events` — no `Node`, no transport. This test
/// closes the remaining gap: the same contract holds when the events actually
/// cross real loopback QUIC and are drained by the live pump.
///
/// Bob's `room_event_capacity` is set to 2. Alice authors a 4-deep admin
/// chain and publishes it while connected; Bob's engine inserts all four
/// (proven via `room_tail`), but his `room_events` ring only holds 2, so the
/// two oldest are evicted before the test ever calls `recv()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_events_lagging_subscriber_observes_lagged_then_recovers() {
    const BURST: u8 = 4;

    let (room, baseline, invite_bob_id, alice, bob) = build_baseline();

    let roster = || allowlist(&[&alice, &bob]);
    let alice_node = Box::pin(spawn_loopback_node(
        alice.iroh_secret(),
        roster(),
        room,
        &baseline,
    ))
    .await;
    let bob_node =
        spawn_loopback_node_with_capacity(bob.iroh_secret(), roster(), room, &baseline, 2).await;

    let mut room_events = bob_node.room_events();

    // Bob's own pump drains his pre-seeded baseline (genesis/invite_bob/
    // join_bob) into this same channel on its first tick, at a time that's
    // nondeterministic relative to the subscribe call above. Drain to
    // quiescence first so the deliberate burst below is the only contributor
    // to the lag count the test asserts on.
    tokio::time::sleep(Duration::from_millis(250)).await;
    loop {
        match room_events.try_recv() {
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                panic!("room_events closed while draining baseline noise")
            }
        }
    }

    bob_node.connect_to(alice_node.endpoint_addr().expect("alice addr"));
    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Bob connects to Alice");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("Alice sees Bob connected");

    let (ids, bytes) = build_admin_invite_burst(&alice, room, invite_bob_id, BURST);
    for wire in &bytes {
        alice_node
            .publish(wire.clone())
            .await
            .expect("alice publishes burst event");
    }

    let last_id = *ids.last().expect("burst is non-empty");
    bob_node
        .wait_until_contains(last_id, WAIT)
        .await
        .expect("Bob's store must contain the last burst event despite the tiny push ring");

    // Capacity 2 with 4 sends before any read: tokio broadcast does not round
    // the lag count, so the first recv() must report exactly 2 missed.
    match tokio::time::timeout(WAIT, room_events.recv())
        .await
        .expect("recv must not hang")
    {
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            assert_eq!(n, 2, "exact lag count, not rounded");
        }
        other => panic!("expected Err(Lagged(2)), got {other:?}"),
    }

    // Reconcile recipe (per `Node::room_events` doc): after `Lagged`, resync
    // via `room_tail` — the authoritative store still has every burst event,
    // proving the push channel dropped them silently only from itself, not
    // from the room.
    let tail = bob_node
        .room_tail(u32::MAX)
        .await
        .expect("room_tail must succeed");
    for id in &ids {
        assert!(
            tail.iter().any(|e| e.event_id == *id),
            "burst event {id:?} must survive in room_tail despite the push channel lagging"
        );
    }

    alice_node.shutdown().await.expect("shutdown Alice");
    bob_node.shutdown().await.expect("shutdown Bob");
}
