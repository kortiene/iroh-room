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
//! through the façade. `SecretKey`/`EndpointId` come from
//! `iroh_rooms::experimental::session` (issue #87) — re-exported *verbatim*
//! from the pinned `iroh` release, so this test doubles as the type-identity
//! regression guard: it only compiles if the façade's `iroh` is the same
//! crate instance `iroh-rooms-net` uses. The façade does not (yet) *wrap* the
//! raw transport identity/dial primitives in an ergonomic newtype (spec
//! OQ5 — an explicit follow-up); every protocol-shaped type (event authoring,
//! validation, the membership fold, the sync engine, the store, the node)
//! comes from `iroh_rooms` alone.
//!
//! Runs unmarked in CI: two in-process nodes on `NetMode::Loopback` with
//! OS-assigned ports, no external process and no port contention, mirroring
//! `join_e2e.rs`/`message_e2e.rs`'s own (non-`#[ignore]`) tier. `scripts/verify.sh`'s
//! `--all-features` run exercises it.

#![cfg(feature = "experimental")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh_rooms::events::{
    build_message_text, capability_hash, validate_wire_bytes, Content, EventId, EventType,
    ValidationContext,
};
use iroh_rooms::experimental::blob::{BlobImport, BlobStore, FetchOutcome};
use iroh_rooms::experimental::session::{
    AdmissionView, AllowlistAdmission, BlobServeConfig, EndpointId, JoinBootstrapAdmission,
    NetConfig, NetMode, Node, SecretKey, SnapshotAdmission, TracingAudit, DEFAULT_TICK,
};
use iroh_rooms::experimental::store::EventStore;
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
use iroh_rooms::files::{build_file_shared, HashRef};
use iroh_rooms::identity::{DeviceBinding, DeviceKey, IdentityKey, SigningKey};
use iroh_rooms::room::{
    build_member_invited, build_member_joined, build_room_created, derive_room_id, RoomId,
};
use tempfile::TempDir;

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

    // `Endpoint` (issue #87, D3) is the one re-exported transport type
    // `experimental_surface.rs`'s identity guard only proves by compile-time
    // coercion, never by calling it against a live Node. Do that here: a real
    // spawned facade `Node`'s `.endpoint()` must be a working handle onto the
    // *same* transport session `.id()` reports, not merely a type-compatible one.
    assert_eq!(
        admin_node.endpoint().id(),
        admin_node.id(),
        "the facade-reexported Endpoint must be a live handle onto this Node's own transport session"
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

/// After convergence, a `message.text` published on the admin's façade `Node`
/// must arrive on the joiner's `room_events()` push stream — proving the
/// issue #83 / IR-0307 primitive works through the public façade over real
/// loopback QUIC, not only in the offline `drain_room_events` unit tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_events_delivers_published_message_through_the_facade() {
    let admin = Principal::new(0x03);
    let joiner = Principal::new(0x30);
    let setup = build_admin_room(&admin, joiner.identity());

    let (admin_node, joiner_node) = converge(&admin, &joiner, &setup).await;
    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "the room must converge before the push round trip"
    );

    let mut room_events = joiner_node.room_events();

    let admin_heads = admin_node.heads().await.expect("admin heads");
    let body = "hello from the room_events facade test";
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

    let stored = tokio::time::timeout(WAIT, async {
        loop {
            match room_events.recv().await {
                Ok(ev) if ev.event_id == message_id => return ev,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("room_events closed before the message arrived")
                }
            }
        }
    })
    .await
    .expect("joiner's room_events must deliver the peer-synced message.text");

    assert_eq!(stored.event_type, EventType::MessageText);
    let ctx = ValidationContext::for_room(setup.room_id);
    let validated = validate_wire_bytes(&stored.wire.to_bytes(), &ctx)
        .expect("the pushed wire bytes must still validate through the facade");
    let Content::MessageText(m) = validated.event.content else {
        panic!(
            "expected MessageText content, got {:?}",
            validated.event.content
        );
    };
    assert_eq!(
        m.body, body,
        "the message body must survive the room_events push unchanged"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

// ---------------------------------------------------------------------------
// In-session blob import through the facade (issue #84 / IR-0308, spec §7.5)
// ---------------------------------------------------------------------------

/// A three-member room (admin + two members), all Active, plus a `file.shared`
/// authored by admin referencing `hash`/`size_bytes`, entirely through
/// `iroh_rooms::{room, events, files}` facade builders + validation.
struct BlobRoomSetup {
    room_id: RoomId,
    log: Vec<Vec<u8>>,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one linear genesis→invite→join(x2)→file_shared narrative; splitting fragments it
fn build_room_with_shared_file(
    admin: &Principal,
    bob: &Principal,
    carol: &Principal,
    hash: HashRef,
    size_bytes: u64,
) -> BlobRoomSetup {
    const NONCE: [u8; 16] = [0x84; 16];
    const T0: u64 = 1_750_000_400_000;
    const INV_ID_BOB: [u8; 16] = [0x84; 16];
    const INV_SECRET_BOB: [u8; 16] = [0xb4; 16];
    const INV_ID_CAROL: [u8; 16] = [0x85; 16];
    const INV_SECRET_CAROL: [u8; 16] = [0xb5; 16];
    const FILE_ID: [u8; 16] = [0x84; 16];

    let room_id = derive_room_id(&admin.identity(), &NONCE, T0);
    let ctx = ValidationContext::for_room(room_id);

    let genesis = build_room_created(&admin.id, &admin.dev, "Facade Blob Room", &NONCE, T0);
    let genesis_id = validate_wire_bytes(&genesis.to_bytes(), &ctx)
        .expect("genesis validates through the facade")
        .event_id;

    let cap_hash_bob = capability_hash(&room_id, &INV_ID_BOB, &INV_SECRET_BOB);
    let invite_bob = build_member_invited(
        &admin.id,
        &admin.dev,
        &room_id,
        &INV_ID_BOB,
        &cap_hash_bob,
        "member",
        &bob.identity(),
        None,
        None,
        &[genesis_id],
        T0 + 1,
    );
    let invite_bob_id = validate_wire_bytes(&invite_bob.to_bytes(), &ctx)
        .expect("bob's invite validates through the facade")
        .event_id;

    let binding_bob = DeviceBinding::create(&room_id, &bob.id, bob.device_key());
    let join_bob = build_member_joined(
        &bob.id,
        &bob.dev,
        &room_id,
        &INV_ID_BOB,
        &INV_SECRET_BOB,
        "member",
        binding_bob,
        Some("Bob"),
        &[invite_bob_id],
        T0 + 2,
    );
    let join_bob_id = validate_wire_bytes(&join_bob.to_bytes(), &ctx)
        .expect("bob's join validates through the facade")
        .event_id;

    let cap_hash_carol = capability_hash(&room_id, &INV_ID_CAROL, &INV_SECRET_CAROL);
    let invite_carol = build_member_invited(
        &admin.id,
        &admin.dev,
        &room_id,
        &INV_ID_CAROL,
        &cap_hash_carol,
        "member",
        &carol.identity(),
        None,
        None,
        &[join_bob_id],
        T0 + 3,
    );
    let invite_carol_id = validate_wire_bytes(&invite_carol.to_bytes(), &ctx)
        .expect("carol's invite validates through the facade")
        .event_id;

    let binding_carol = DeviceBinding::create(&room_id, &carol.id, carol.device_key());
    let join_carol = build_member_joined(
        &carol.id,
        &carol.dev,
        &room_id,
        &INV_ID_CAROL,
        &INV_SECRET_CAROL,
        "member",
        binding_carol,
        Some("Carol"),
        &[invite_carol_id],
        T0 + 4,
    );
    let join_carol_id = validate_wire_bytes(&join_carol.to_bytes(), &ctx)
        .expect("carol's join validates through the facade")
        .event_id;

    let file_shared = build_file_shared(
        &admin.id,
        &admin.dev,
        &room_id,
        FILE_ID,
        "fox.txt",
        "text/plain",
        size_bytes,
        hash,
        Some("raw"),
        &[],
        &[join_carol_id],
        T0 + 5,
    );
    validate_wire_bytes(&file_shared.to_bytes(), &ctx)
        .expect("file.shared validates through the facade");

    BlobRoomSetup {
        room_id,
        log: vec![
            genesis.to_bytes(),
            invite_bob.to_bytes(),
            join_bob.to_bytes(),
            invite_carol.to_bytes(),
            join_carol.to_bytes(),
            file_shared.to_bytes(),
        ],
    }
}

/// Import `content` into a fresh durable store at `dir` (through the facade's
/// `experimental::blob::BlobStore`), then close it so a later `Node::spawn_room`
/// can reopen the same dir (the exclusive-lock invariant). Returns the hash.
async fn seed_blob(dir: &std::path::Path, name: &str, content: &[u8]) -> [u8; 32] {
    let store = BlobStore::open(dir).await.expect("open blob store to seed");
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write fixture file");
    let import = store.import_path(&path).await.expect("import fixture blob");
    store.close().await.expect("close seeded blob store");
    import.hash
}

/// Spawn a facade-only **serving** node (`Node::spawn_room` + `BlobServeConfig`),
/// seeded with `log`, reconciled once so admission/`BlobAclView` are populated.
async fn spawn_facade_serving_member(
    actor: &Principal,
    room_id: RoomId,
    log: &[Vec<u8>],
    blobs_dir: std::path::PathBuf,
) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory store");
    let mut engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("open engine");
    for ev in log {
        engine.publish(ev).expect("seed event");
    }
    let cell = Arc::new(Mutex::new(AdmissionView::empty()));
    let admission = Arc::new(SnapshotAdmission::new(cell.clone()));
    let node = Node::spawn_room(
        actor.iroh_secret(),
        admission,
        Arc::new(TracingAudit),
        engine,
        NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        },
        DEFAULT_TICK,
        Vec::new(),
        cell,
        Some(BlobServeConfig { blobs_dir }),
    )
    .await
    .expect("spawn_room with blob serving through the facade");
    node.reconcile_now()
        .await
        .expect("force admission/BlobAclView to populate before returning");
    node
}

/// Spawn a facade-only **pure fetcher** node (`Node::spawn`, unmanaged).
async fn spawn_facade_fetcher(actor: &Principal, room_id: RoomId, log: &[Vec<u8>]) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory store");
    let mut engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("open engine");
    for ev in log {
        engine.publish(ev).expect("seed event");
    }
    Node::spawn(
        actor.iroh_secret(),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        engine,
        NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        },
        DEFAULT_TICK,
    )
    .await
    .expect("spawn fetcher node through the facade")
}

/// The online tier's in-session import claim (issue #84 / IR-0308), driven with
/// **only** facade imports over real loopback QUIC: two serving nodes, a fetch
/// from the origin, an in-session `blob_import_bytes` re-provide, and a third
/// node fetching the re-provided blob after the origin has shut down — proving
/// `Node::blob_import`/`blob_import_bytes` are reachable and functional through
/// `iroh_rooms::experimental::session::Node` (AC6), not just offline-locked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_import_through_facade_reprovides() {
    let admin = Principal::new(0x84);
    let bob = Principal::new(0x85);
    let carol = Principal::new(0x86);

    let content = b"facade re-provide fixture content, issue #84 / IR-0308";
    let alice_tmp = TempDir::new().expect("alice tmp");
    let alice_blobs = alice_tmp.path().join("blobs");
    let hash = seed_blob(&alice_blobs, "fox.txt", content).await;

    let setup = build_room_with_shared_file(
        &admin,
        &bob,
        &carol,
        HashRef::from_bytes(hash),
        content.len() as u64,
    );

    let alice_node =
        spawn_facade_serving_member(&admin, setup.room_id, &setup.log, alice_blobs).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");

    let bob_tmp = TempDir::new().expect("bob tmp");
    let bob_node = spawn_facade_serving_member(
        &bob,
        setup.room_id,
        &setup.log,
        bob_tmp.path().join("blobs"),
    )
    .await;
    let bob_addr = bob_node.endpoint_addr().expect("bob addr");

    let carol_node = spawn_facade_fetcher(&carol, setup.room_id, &setup.log).await;

    // Bob fetches the blob from Alice over the facade's blob plane.
    let (outcome, bytes) = bob_node.fetch_file(alice_addr, hash, hash, WAIT).await;
    assert_eq!(
        outcome,
        FetchOutcome::Fetched,
        "bob must fetch the blob alice originally provides, through the facade"
    );
    let bytes = bytes.expect("Fetched must carry bytes");
    assert_eq!(bytes.as_ref(), content);

    // Bob re-provides the fetched bytes in-session via the facade's
    // blob_import_bytes — no restart, no second FsStore open.
    let import: BlobImport = bob_node
        .blob_import_bytes(bytes)
        .await
        .expect("bob re-provides the fetched bytes in-session through the facade");
    assert_eq!(import.hash, hash);

    // Alice can go away entirely — bob is a genuine independent provider.
    alice_node.shutdown().await.expect("shutdown alice");

    // Carol fetches the re-provided blob from bob — no restart of bob involved.
    let (outcome2, bytes2) = carol_node.fetch_file(bob_addr, hash, hash, WAIT).await;
    assert_eq!(
        outcome2,
        FetchOutcome::Fetched,
        "carol must fetch the blob bob re-provided in-session through the facade (AC6)"
    );
    assert_eq!(bytes2.expect("Fetched must carry bytes").as_ref(), content);

    bob_node.shutdown().await.expect("shutdown bob");
    carol_node.shutdown().await.expect("shutdown carol");
}
