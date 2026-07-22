//! Headline end-to-end coverage for in-session blob import (issue #84 /
//! IR-0308, spec §7.4): the two claims that `tests/blob_import.rs` cannot prove
//! without a real peer on real loopback QUIC.
//!
//! * **AC1** — `share_over_live_session_no_disconnect_peer_fetches`: with a peer
//!   already `Connected` on the event plane, `Node::blob_import` + a `file.shared`
//!   publish produce **zero** `ConnEvent` disconnects, and the peer fetches the
//!   freshly imported blob from the very same, never-cycled session.
//! * **AC2** — `fetched_bytes_reprovided_in_session_serves_third_peer`: after
//!   `Node::fetch_file` returns verified bytes, `Node::blob_import_bytes` makes the
//!   fetcher a provider **without restarting it** — a third member fetches the
//!   blob from the fetcher, even after the origin provider has shut down.
//!
//! Fixtures mirror `tests/blob_e2e.rs` (`Principal` / `build_two_member_room` /
//! `spawn_provider` / `spawn_fetcher`) — each integration-test file in this crate
//! is its own compilation unit, so the pattern is duplicated rather than shared.
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_file_shared;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AdmissionView, AllowlistAdmission, BlobImport, BlobServeConfig, FetchOutcome, NetConfig,
    NetMode, Node, PeerConnState, SnapshotAdmission, TracingAudit, DEFAULT_TICK,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0x84; 16];
const T0: u64 = 1_750_000_400_000;
/// Bound for connection / sync steps over real loopback QUIC.
const WAIT: Duration = Duration::from_secs(10);
/// Bound for a real blob transfer against a live loopback provider.
const FETCH_WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Fixtures (mirrors blob_e2e.rs's Principal / build_two_member_room)
// ---------------------------------------------------------------------------

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

/// A two-member room log: genesis (Alice=admin) -> `invite_bob` -> `join_bob`.
/// Returns `(room_id, [genesis, invite, join] bytes, join_event_id, alice, bob)`.
fn build_two_member_room() -> (RoomId, Vec<Vec<u8>>, EventId, Principal, Principal) {
    let alice = Principal::new(0x61);
    let bob = Principal::new(0x62);
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
            room_name: "Blob Import Live E2E Room".to_owned(),
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

/// A three-member room log, extending [`build_two_member_room`] with Carol.
/// Returns `(room_id, log, carol_join_id, alice, bob, carol)`.
fn build_three_member_room() -> (
    RoomId,
    Vec<Vec<u8>>,
    EventId,
    Principal,
    Principal,
    Principal,
) {
    let (room, mut log, bob_join_id, alice, bob) = build_two_member_room();
    let carol = Principal::new(0x63);

    let inv_id = [0x03u8; 16];
    let inv_sec = [0x43u8; 16];
    let invite = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T0 + 3,
        prev_events: vec![bob_join_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_id,
            capability_hash: capability_hash(&room, &inv_id, &inv_sec),
            role: "member".to_owned(),
            invitee_key: carol.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite_id = invite.event_id();
    log.push(wire_bytes(&invite, &alice.dev));

    let join = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: carol.identity(),
        device_id: carol.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T0 + 4,
        prev_events: vec![invite_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_id,
            capability_secret: inv_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &carol.id, carol.device_key()),
            display_name: Some("Carol".to_owned()),
        }),
    };
    let join_id = join.event_id();
    log.push(wire_bytes(&join, &carol.dev));

    (room, log, join_id, alice, bob, carol)
}

/// Write `bytes` to `<dir>/<name>` and return the path (absolute — `BlobStore`
/// import requires it, per the `blob-add-path-requires-absolute` invariant).
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write fixture file");
    path
}

/// Import `content` into a fresh durable store at `dir`, then close it (release
/// the exclusive `FsStore` lock) so a `Node::spawn_room` can reopen the same dir
/// (the `fsstore-exclusive-lock-needs-shutdown` invariant). Returns the hash.
async fn seed_blob(dir: &Path, name: &str, content: &[u8]) -> [u8; 32] {
    let store = iroh_rooms_net::BlobStore::open(dir)
        .await
        .expect("open blob store to seed");
    let path = write_file(dir, name, content);
    let import = store.import_path(&path).await.expect("import fixture blob");
    store.close().await.expect("close seeded blob store");
    import.hash
}

/// A `file.shared` frame from `author`, parented on `prev` (current heads),
/// referencing `hash`.
#[allow(clippy::too_many_arguments)] // one fixture-building seam; each arg is a distinct field
fn file_shared_citing(
    author: &Principal,
    room: RoomId,
    prev: &[EventId],
    file_id: [u8; 16],
    name: &str,
    size_bytes: u64,
    hash: [u8; 32],
    created_at: u64,
) -> WireEvent {
    build_file_shared(
        &author.id,
        &author.dev,
        &room,
        file_id,
        name,
        "text/plain",
        size_bytes,
        HashRef::from_bytes(hash),
        Some("raw"),
        &[],
        prev,
        created_at,
    )
}

/// Spawn a **managed room** node that also serves blobs from `blobs_dir` — the
/// `Node` a resident daemon runs (`spawn_room` + `BlobServeConfig`). Forces one
/// reconcile after spawn so the `BlobAclView`/admission cell is populated before
/// the caller relies on it. `addr_hints` lets the caller opt a node into
/// auto-dialing a peer's event-plane connection (empty = accept-only).
///
/// Returns a boxed future so `Node::spawn_room`'s ~16 KB state machine is not
/// inlined into each caller (clippy `large_futures`).
fn spawn_serving_member<'a>(
    actor: &'a Principal,
    room: RoomId,
    log: &'a [Vec<u8>],
    blobs_dir: PathBuf,
    addr_hints: Vec<EndpointAddr>,
) -> Pin<Box<dyn Future<Output = Node> + Send + 'a>> {
    Box::pin(async move {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
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
            addr_hints,
            cell,
            Some(BlobServeConfig { blobs_dir }),
        )
        .await
        .expect("spawn_room with blob serving");
        node.reconcile_now()
            .await
            .expect("force admission/BlobAclView to populate before returning");
        node
    })
}

/// Spawn a **pure fetcher** node (unmanaged, no blob serving): only
/// `Node::fetch_file` is exercised, so no inbound admission is ever tested.
fn spawn_fetcher<'a>(
    actor: &'a Principal,
    room: RoomId,
    log: &'a [Vec<u8>],
) -> Pin<Box<dyn Future<Output = Node> + Send + 'a>> {
    Box::pin(async move {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
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
        .expect("spawn fetcher node")
    })
}

// ---------------------------------------------------------------------------
// AC1 — share in-session: zero disconnects, peer fetches the new blob
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_over_live_session_no_disconnect_peer_fetches() {
    let (room, log, _join_id, alice, bob) = build_two_member_room();

    let alice_tmp = TempDir::new().unwrap();
    let alice_node = spawn_serving_member(
        &alice,
        room,
        &log,
        alice_tmp.path().join("blobs"),
        Vec::new(),
    )
    .await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");

    let bob_tmp = TempDir::new().unwrap();
    // Bob's PeerManager derives desired = {alice} from the fold and auto-dials her
    // via this addr hint — no explicit connect_to needed.
    let bob_node = spawn_serving_member(
        &bob,
        room,
        &log,
        bob_tmp.path().join("blobs"),
        vec![alice_addr.clone()],
    )
    .await;

    bob_node
        .wait_for_state(alice.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("bob auto-dials alice and reaches Connected");
    alice_node
        .wait_for_state(bob.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("alice observes bob as Connected via inbound accept");

    // Subscribe BEFORE the import so every transition during the share is seen.
    let mut alice_conn_events = alice_node.conn_events();

    // In-session import: no second FsStore open, no session cycle.
    let content = b"the quick brown fox jumps over the lazy dog";
    let src_dir = TempDir::new().unwrap();
    let path = write_file(src_dir.path(), "fox.txt", content);
    let imp: BlobImport = alice_node
        .blob_import(&path)
        .await
        .expect("alice imports through the live session she already owns");

    let heads = alice_node.heads().await.expect("alice heads");
    let wire = file_shared_citing(
        &alice,
        room,
        &heads,
        [0x84; 16],
        "fox.txt",
        content.len() as u64,
        imp.hash,
        T0 + 100,
    );
    let file_shared_id: EventId = wire.id.parse().expect("valid file_shared event_id");
    alice_node
        .publish(wire.to_bytes())
        .await
        .expect("alice publishes file.shared over the live session");

    // Bob — still connected the whole time — receives it over the uninterrupted
    // event link (no session cycle means no reconnect needed to see it).
    bob_node
        .wait_until_contains(file_shared_id, WAIT)
        .await
        .expect("bob receives file.shared over the still-live event connection");

    // AC1: zero ConnEvent disconnects for bob across the whole import + publish.
    let bob_id = bob.endpoint_id();
    let mut saw_drop = false;
    while let Ok(ev) = alice_conn_events.try_recv() {
        if ev.device == bob_id
            && matches!(ev.to, PeerConnState::Offline | PeerConnState::Unauthorized)
        {
            saw_drop = true;
        }
    }
    assert!(
        !saw_drop,
        "blob_import + publish must produce zero ConnEvent disconnects for the connected peer (AC1)"
    );
    assert_eq!(
        alice_node.peer_state(bob_id),
        Some(PeerConnState::Connected),
        "bob must remain Connected on alice's side after the share (AC1)"
    );
    assert_eq!(
        bob_node.peer_state(alice.endpoint_id()),
        Some(PeerConnState::Connected),
        "alice must remain Connected on bob's side after the share (AC1)"
    );

    // Bob fetches the freshly shared blob from the SAME uninterrupted session.
    let (outcome, bytes) = bob_node
        .fetch_file(alice_addr, imp.hash, imp.hash, FETCH_WAIT)
        .await;
    assert_eq!(
        outcome,
        FetchOutcome::Fetched,
        "bob must fetch the freshly imported blob from alice's live, never-cycled session (AC1)"
    );
    assert_eq!(
        bytes.expect("Fetched must carry bytes").as_ref(),
        content,
        "the fetched bytes must equal the imported file content"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// AC2 — fetched bytes re-provided in-session serve a third peer
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetched_bytes_reprovided_in_session_serves_third_peer() {
    let (room, base_log, carol_join_id, alice, bob, carol) = build_three_member_room();

    let alice_tmp = TempDir::new().unwrap();
    let alice_blobs = alice_tmp.path().join("blobs");
    let content = b"fetched bytes re-provided across a live session, spec IR-0308";
    let hash = seed_blob(&alice_blobs, "fox.txt", content).await;

    let mut log = base_log.clone();
    log.push(
        file_shared_citing(
            &alice,
            room,
            &[carol_join_id],
            [0x85; 16],
            "fox.txt",
            content.len() as u64,
            hash,
            T0 + 100,
        )
        .to_bytes(),
    );

    // Alice: origin provider, seeded with the blob + its file.shared reference.
    let alice_node = spawn_serving_member(&alice, room, &log, alice_blobs, Vec::new()).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");

    // Bob: a second serving node with an EMPTY store — it will become a provider
    // only via blob_import_bytes, never a second FsStore open on alice's dir.
    let bob_tmp = TempDir::new().unwrap();
    let bob_node =
        spawn_serving_member(&bob, room, &log, bob_tmp.path().join("blobs"), Vec::new()).await;
    let bob_addr = bob_node.endpoint_addr().expect("bob addr");

    // Carol: a pure fetcher, never a provider.
    let carol_node = spawn_fetcher(&carol, room, &log).await;

    // Step 1: Bob fetches the blob from Alice over the blob plane.
    let (outcome, bytes) = bob_node
        .fetch_file(alice_addr, hash, hash, FETCH_WAIT)
        .await;
    assert_eq!(
        outcome,
        FetchOutcome::Fetched,
        "bob must fetch the blob alice originally provides"
    );
    let bytes = bytes.expect("Fetched must carry bytes");
    assert_eq!(bytes.as_ref(), content);

    // Step 2: Bob re-provides the fetched bytes in-session, without restarting.
    let import: BlobImport = bob_node
        .blob_import_bytes(bytes)
        .await
        .expect("bob re-provides the fetched bytes in-session");
    assert_eq!(
        import.hash, hash,
        "the re-imported hash must equal the fetched blob's hash"
    );

    // Alice can go away entirely — proving bob is a genuine independent provider,
    // not merely relaying while alice happens to still be up.
    alice_node.shutdown().await.expect("shutdown alice");

    // Step 3: Carol fetches the SAME blob from Bob — no restart, no re-import
    // trigger beyond step 2. Gate 1 passes (carol is Active in bob's own fold);
    // Gate 2 passes (bob's fold already carries the synced file.shared); the
    // store has(hash) is true from the re-import.
    let (outcome2, bytes2) = carol_node
        .fetch_file(bob_addr, hash, hash, FETCH_WAIT)
        .await;
    assert_eq!(
        outcome2,
        FetchOutcome::Fetched,
        "carol must fetch the blob bob re-provided in-session, with no restart (AC2)"
    );
    assert_eq!(
        bytes2.expect("Fetched must carry bytes").as_ref(),
        content,
        "the re-provided bytes must equal the original content"
    );

    bob_node.shutdown().await.expect("shutdown bob");
    carol_node.shutdown().await.expect("shutdown carol");
}
