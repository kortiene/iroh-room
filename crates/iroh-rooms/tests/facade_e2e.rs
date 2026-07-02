//! Online-runtime end-to-end test for the SDK façade (issue #36 / IR-0301).
//!
//! Every existing test in this crate (`stable_surface.rs`, `experimental_surface.rs`)
//! is deliberately offline — they prove a façade re-export *path* resolves, not
//! that the online runtime it names actually works. But the issue's stated reason
//! to ship this SDK is so "a third-party Rust program … can drive a room without
//! shelling out to the `iroh-rooms` binary" (spec §1) — a claim about crossing a
//! real network boundary that the offline suite structurally cannot exercise.
//!
//! This file closes that gap: it wires two [`Node`](session::Node)s over real
//! loopback QUIC using **only** `iroh_rooms::{identity, room, events,
//! experimental::{session, store, sync}}` imports — no direct
//! `iroh_rooms_core`/`iroh_rooms_net` dependency anywhere below — and drives the
//! same create → invite → join → converge scenario as
//! `iroh-rooms-net/tests/join_e2e.rs`, plus a `message.text` round trip, entirely
//! through the façade. `iroh::{SecretKey, EndpointId}` are still imported
//! directly: the façade does not (yet) wrap the raw transport identity/dial
//! primitives (spec OQ5 — ergonomic wrappers are an explicit follow-up), and
//! `examples/03_invite_and_join.rs` does the same; every protocol-shaped type
//! (event authoring, validation, the membership fold, the sync engine, the
//! store, the node) comes from `iroh_rooms` alone.
//!
//! Runs unmarked in CI: two in-process nodes on `NetMode::Loopback` with
//! OS-assigned ports, no external process and no port contention, mirroring
//! `join_e2e.rs`/`message_e2e.rs`'s own (non-`#[ignore]`) tier. `scripts/verify.sh`'s
//! `--all-features` run exercises it.

#![cfg(feature = "experimental")]

use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_rooms::events::{
    build_message_text, capability_hash, validate_wire_bytes, Content, EventId, EventType,
    ValidationContext,
};
use iroh_rooms::experimental::session::{
    AllowlistAdmission, JoinBootstrapAdmission, NetConfig, NetMode, Node, TracingAudit,
    DEFAULT_TICK,
};
use iroh_rooms::experimental::store::EventStore;
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
use iroh_rooms::identity::{DeviceBinding, DeviceKey, IdentityKey, SigningKey};
use iroh_rooms::room::{
    build_member_invited, build_member_joined, build_room_created, derive_room_id, RoomId,
};

/// Loopback-wait budget — generous so transient scheduling hiccups don't flake
/// (mirrors the Node-API e2e suites in `iroh-rooms-net`).
const WAIT: Duration = Duration::from_secs(15);

const NONCE: [u8; 16] = [0xf1; 16];
const T0: u64 = 1_750_000_000_000;
const INV_ID: [u8; 16] = [0x01u8; 16];
const INV_SECRET: [u8; 16] = [0x42u8; 16];

/// A deterministic (identity, device) key pair, seeded from a single byte.
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

    fn device_key(&self) -> DeviceKey {
        self.dev.device_key()
    }

    /// iroh transport secret for this principal's device (== the device signing
    /// key, spec A2). Not wrapped by the façade — see the module doc.
    fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev.to_seed())
    }

    /// Authenticated transport identity == `device_key` bytes (spec A2).
    fn endpoint_id(&self) -> EndpointId {
        self.iroh_secret().public()
    }
}

/// Everything the tests need from a freshly authored two-event admin room log.
struct RoomSetup {
    room_id: RoomId,
    genesis_id: EventId,
    invite_id: EventId,
    /// Wire bytes to seed the admin's engine: `[genesis, invite]`.
    log: Vec<Vec<u8>>,
}

/// Author the admin's room (genesis + a single `member.invited` for `joiner`)
/// using only `iroh_rooms::{room, events}` builders + validation.
fn build_admin_room(admin: &Principal, joiner_identity: IdentityKey) -> RoomSetup {
    let room_id = derive_room_id(&admin.identity(), &NONCE, T0);
    let ctx = ValidationContext::for_room(room_id);

    let genesis = build_room_created(&admin.id, &admin.dev, "Facade E2E Room", &NONCE, T0);
    let genesis_id = validate_wire_bytes(&genesis.to_bytes(), &ctx)
        .expect("genesis validates through the facade")
        .event_id;

    let cap_hash = capability_hash(&room_id, &INV_ID, &INV_SECRET);
    let invite = build_member_invited(
        &admin.id,
        &admin.dev,
        &room_id,
        &INV_ID,
        &cap_hash,
        "member",
        &joiner_identity,
        None,
        None,
        &[genesis_id],
        T0 + 1,
    );
    let invite_id = validate_wire_bytes(&invite.to_bytes(), &ctx)
        .expect("invite validates through the facade")
        .event_id;

    RoomSetup {
        room_id,
        genesis_id,
        invite_id,
        log: vec![genesis.to_bytes(), invite.to_bytes()],
    }
}

/// Spawn an admin `Node` pre-seeded with `log`, admitting first-time joiners
/// provisionally (mirrors `iroh-rooms-net/tests/join_e2e.rs::spawn_admin_node`,
/// but wired entirely from `experimental::{session, store, sync}` re-exports).
async fn spawn_admin_node(admin: &Principal, room_id: RoomId, log: &[Vec<u8>]) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory admin store");
    let mut engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("admin engine");
    for ev in log {
        engine.publish(ev).expect("seed admin event");
    }
    let admission = JoinBootstrapAdmission::new(AllowlistAdmission::new(), true);
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn(
        admin.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn admin node through the facade")
}

/// Spawn a joiner `Node` with an empty store that admits (only) the admin.
async fn spawn_joiner_node(joiner: &Principal, admin: &Principal, room_id: RoomId) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory joiner store");
    let engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("joiner engine");
    let admission = AllowlistAdmission::new()
        .bind_device(admin.endpoint_id(), admin.identity())
        .set_active(admin.identity());
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn(
        joiner.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn joiner node through the facade")
}

/// Poll `node.snapshot()` (façade `Node::snapshot`) until `identity` is Active or
/// `deadline` elapses.
async fn wait_active(node: &Node, identity: &IdentityKey, deadline: Duration) -> bool {
    tokio::time::timeout(deadline, async {
        loop {
            if let Ok(snap) = node.snapshot().await {
                if snap.is_active(identity) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok()
}

/// Drive a joiner from an empty store to `Active` against a pre-seeded admin,
/// entirely through façade calls: connect, pull the membership sub-DAG, author +
/// publish `member.joined`, and wait for the admin to observe it. Returns both
/// nodes so callers can layer further façade-driven activity on the converged
/// room (e.g. message propagation) before shutting down.
async fn converge(admin: &Principal, joiner: &Principal, setup: &RoomSetup) -> (Node, Node) {
    let admin_node = spawn_admin_node(admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(joiner, admin, setup.room_id).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    joiner_node
        .wait_until_contains(setup.genesis_id, WAIT)
        .await
        .expect("joiner pulled genesis via the facade Node API");
    joiner_node
        .wait_until_contains(setup.invite_id, WAIT)
        .await
        .expect("joiner pulled invite via the facade Node API");

    let heads = joiner_node.heads().await.expect("joiner heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET,
        "member",
        binding,
        Some("Joiner"),
        &heads,
        T0 + 1_000,
    );
    let join_id: EventId = join_wire.id.parse().expect("valid join event_id");
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined through the facade Node");
    admin_node
        .wait_until_contains(join_id, WAIT)
        .await
        .expect("admin received the join over the facade transport");

    (admin_node, joiner_node)
}

/// The whole online SDK surface, driven with **only** `iroh_rooms` façade
/// imports (plus the raw `iroh` transport identity primitives the façade does
/// not yet wrap): two [`Node`]s converge over real loopback QUIC on a
/// two-member room. This is the "third-party Rust program drives a room
/// without the binary" claim the issue makes (§1) — proven, not just asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_converge_to_active_through_the_facade_alone() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);
    let setup = build_admin_room(&admin, joiner.identity());

    let (admin_node, joiner_node) = converge(&admin, &joiner, &setup).await;

    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "admin's facade-driven snapshot must show the joiner Active"
    );
    assert!(
        wait_active(&joiner_node, &joiner.identity(), WAIT).await,
        "joiner's own facade-driven snapshot must show itself Active"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

/// After convergence, a `message.text` authored via
/// `iroh_rooms::events::build_message_text` and published on the admin's
/// façade `Node` must reach the joiner's `room_tail` with the correct body —
/// proving the stable events façade and the experimental online-runtime
/// façade interoperate end to end, not just that each resolves in isolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_propagates_through_the_facade_after_convergence() {
    let admin = Principal::new(0x02);
    let joiner = Principal::new(0x20);
    let setup = build_admin_room(&admin, joiner.identity());

    let (admin_node, joiner_node) = converge(&admin, &joiner, &setup).await;
    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "the room must converge before the message round trip"
    );

    let admin_heads = admin_node.heads().await.expect("admin heads");
    let body = "hello from the facade e2e test";
    let message = build_message_text(
        &admin.id,
        &admin.dev,
        &setup.room_id,
        body,
        Some("plain"),
        None,
        &[],
        &admin_heads,
        T0 + 2_000,
    );
    let message_id: EventId = message.id.parse().expect("valid message event_id");
    admin_node
        .publish(message.to_bytes())
        .await
        .expect("publish message.text through the facade Node");

    joiner_node
        .wait_until_contains(message_id, WAIT)
        .await
        .expect("joiner received the message.text over the facade transport");

    let tail = joiner_node.room_tail(100).await.expect("joiner room_tail");
    let ctx = ValidationContext::for_room(setup.room_id);
    let stored = tail
        .iter()
        .find(|ev| ev.event_id == message_id)
        .expect("the published message.text must appear in the joiner's room_tail");
    assert_eq!(stored.event_type, EventType::MessageText);

    let validated = validate_wire_bytes(&stored.wire.to_bytes(), &ctx)
        .expect("the stored wire bytes must still validate through the facade");
    let Content::MessageText(m) = validated.event.content else {
        panic!(
            "expected MessageText content, got {:?}",
            validated.event.content
        );
    };
    assert_eq!(
        m.body, body,
        "the message body must survive the facade round trip unchanged"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}
