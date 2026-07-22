//! End-to-end blob-plane serve + fetch (IR-0204 / issue #29 — Node-level test),
//! the always-green backstop the spec names in §8: real `iroh-blobs` connections
//! drive the two-gate ACL (`spawn_blob_gate`) and the verified fetch client
//! (`fetch_blob`) that a unit test cannot synthesize without a live provider.
//!
//! ```text
//! Alice (spawn_room, BlobServeConfig)  --serve-->  iroh-blobs ALPN, two-gate ACL
//! Bob   (Node::fetch_file)             --dial-->   verified transfer + BLAKE3 recheck
//! ```
//!
//! Coverage (issue #29 ACs + Test Plan: "valid fetch, wrong hash, unavailable
//! provider, and unauthorized peer"):
//! * **AC1/AC4** — `authorized_active_member_fetches_and_verifies_blob`: an active
//!   member fetches a blob an online provider holds; the transfer completes and the
//!   receiver's independent BLAKE3 recompute equals the declared hash.
//! * **AC2** — `declared_hash_mismatch_is_rejected`: a `declared_hash` that
//!   disagrees with the bytes actually served yields `HashMismatch`, never
//!   `Fetched` (the `fetch_hash`/`declared_hash` split makes this deterministic
//!   against a content-addressed store, spec §5.4/R4).
//! * **AC3 (Gate 1)** — `non_member_peer_is_denied_at_connect_gate`: a peer who
//!   never joined the room is denied at the connect gate before any bytes move.
//! * **AC3 (Gate 2)** — `active_member_is_denied_an_unreferenced_hash`: an active
//!   member is still denied a hash the provider holds but no `file.shared`
//!   references — per-hash gating is independent of node admission (spec §5.5.
//!   decision-matrix row 4).
//! * **AC5** — `offline_provider_is_reported_unavailable_within_timeout`: no
//!   provider listening at the dialed address yields `Unavailable` within the
//!   bounded per-attempt timeout — no hang, no panic.
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI).
//! Membership + `file.shared` events are seeded via core event builders, and the
//! provider's durable blob store is seeded via `BlobStore` before the serving
//! `Node` opens it (the `FsStore` exclusive-lock invariant: one open per process).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{EndpointId, SecretKey};
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
    AdmissionView, AllowlistAdmission, BlobServeConfig, BlobStore, FetchOutcome, NetConfig,
    NetMode, Node, SnapshotAdmission, TracingAudit, DEFAULT_TICK,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xb1; 16];
const T0: u64 = 1_750_000_200_000;
/// Bound for a real transfer/connect against a live loopback provider.
const FETCH_WAIT: Duration = Duration::from_secs(5);
/// Bound for the "nobody is listening" case — kept short so the suite stays fast;
/// loopback connect failures resolve well under this on CI (spec R7).
const UNAVAILABLE_WAIT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Fixtures (mirrors file_e2e.rs's Principal / build_two_member_room)
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
            room_name: "Blob E2E Room".to_owned(),
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
    let store = BlobStore::open(dir).await.expect("open blob store to seed");
    let path = write_file(dir, name, content);
    let import = store.import_path(&path).await.expect("import fixture blob");
    store.close().await.expect("close seeded blob store");
    import.hash
}

/// A `file.shared` frame from `author`, parented on `prev`, referencing `hash`.
#[allow(clippy::too_many_arguments)] // one fixture-building seam; each arg is a distinct field
fn file_shared(
    author: &Principal,
    room: RoomId,
    prev: EventId,
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
        &[prev],
        created_at,
    )
}

/// Spawn a **managed room** node that also serves blobs from `blobs_dir` (the
/// `room tail` surface, spec §5.3): `Node::spawn_room` with a `BlobServeConfig`.
/// Forces one reconcile after spawn so the `BlobAclView` cell is populated
/// (active members + referenced hashes) before the caller's address is handed
/// to a fetcher — otherwise the very first fetch could race the pump's initial
/// (but async) reconcile.
///
/// Returns a boxed future: `Node::spawn_room`'s state machine is ~16 KB, which
/// would otherwise be inlined into each caller and trip clippy `large_futures`.
fn spawn_provider<'a>(
    actor: &'a Principal,
    room: RoomId,
    log: &'a [Vec<u8>],
    blobs_dir: PathBuf,
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
            Vec::new(), // no addr hints needed: this test never relies on event-plane dialing
            cell,
            Some(BlobServeConfig { blobs_dir }),
        )
        .await
        .expect("spawn_room with blob serving");
        node.reconcile_now()
            .await
            .expect("force the BlobAclView cell to populate before returning");
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
// AC1 / AC4 — valid fetch: authorized active member gets verified bytes
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorized_active_member_fetches_and_verifies_blob() {
    let (room, base_log, join_id, alice, bob) = build_two_member_room();

    let blobs_tmp = TempDir::new().unwrap();
    let blobs_dir = blobs_tmp.path().join("blobs");
    let content = b"the quick brown fox jumps over the lazy dog";
    let hash = seed_blob(&blobs_dir, "fox.txt", content).await;

    let mut log = base_log.clone();
    log.push(
        file_shared(
            &alice,
            room,
            join_id,
            [0x11; 16],
            "fox.txt",
            content.len() as u64,
            hash,
            T0 + 10,
        )
        .to_bytes(),
    );

    let alice_node = spawn_provider(&alice, room, &log, blobs_dir).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");
    let bob_node = spawn_fetcher(&bob, room, &log).await;

    let (outcome, bytes) = bob_node
        .fetch_file(alice_addr, hash, hash, FETCH_WAIT)
        .await;

    assert_eq!(
        outcome,
        FetchOutcome::Fetched,
        "an active member fetching a hash the online provider holds and references \
         must succeed (AC1)"
    );
    assert_eq!(
        bytes.expect("Fetched must carry bytes").as_ref(),
        content,
        "the transferred bytes must equal the original file content (AC4)"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// AC2 — a declared hash that disagrees with the served bytes is rejected
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_hash_mismatch_is_rejected() {
    let (room, base_log, join_id, alice, bob) = build_two_member_room();

    let blobs_tmp = TempDir::new().unwrap();
    let blobs_dir = blobs_tmp.path().join("blobs");
    let content = b"integrity matters more than availability";
    let served_hash = seed_blob(&blobs_dir, "notes.txt", content).await;

    let mut log = base_log.clone();
    log.push(
        file_shared(
            &alice,
            room,
            join_id,
            [0x12; 16],
            "notes.txt",
            content.len() as u64,
            served_hash,
            T0 + 10,
        )
        .to_bytes(),
    );

    let alice_node = spawn_provider(&alice, room, &log, blobs_dir).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");
    let bob_node = spawn_fetcher(&bob, room, &log).await;

    // Request the real (referenced, held) hash on the wire, but declare a
    // different one — a `file.shared` that lies about its content (spec §5.4/R4).
    let lying_declared_hash = [0xEE; 32];
    let (outcome, bytes) = bob_node
        .fetch_file(alice_addr, served_hash, lying_declared_hash, FETCH_WAIT)
        .await;

    assert_eq!(
        outcome,
        FetchOutcome::HashMismatch,
        "a declared hash that disagrees with the transferred bytes must never \
         report Fetched (AC2)"
    );
    assert_ne!(
        bytes.map(|b| *blake3::hash(&b).as_bytes()),
        Some(lying_declared_hash),
        "the mismatching content must not equal the falsely declared hash"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// AC3 (Gate 1) — a non-member peer is denied at the connect gate
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_member_peer_is_denied_at_connect_gate() {
    let (room, base_log, join_id, alice, _bob) = build_two_member_room();
    // Charlie never appears in the room log at all — never invited, never joined.
    let charlie = Principal::new(0x63);

    let blobs_tmp = TempDir::new().unwrap();
    let blobs_dir = blobs_tmp.path().join("blobs");
    let content = b"members only";
    let hash = seed_blob(&blobs_dir, "secret.txt", content).await;

    let mut log = base_log.clone();
    log.push(
        file_shared(
            &alice,
            room,
            join_id,
            [0x13; 16],
            "secret.txt",
            content.len() as u64,
            hash,
            T0 + 10,
        )
        .to_bytes(),
    );

    let alice_node = spawn_provider(&alice, room, &log, blobs_dir).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");
    // Charlie's own engine only needs to open the same room; he holds none of its
    // events (he was never admitted) — irrelevant to a Gate 1 dial-side check.
    let charlie_node = spawn_fetcher(&charlie, room, &[]).await;

    let (outcome, bytes) = charlie_node
        .fetch_file(alice_addr, hash, hash, FETCH_WAIT)
        .await;

    assert_eq!(
        outcome,
        FetchOutcome::DeniedAtConnect,
        "a peer who is not an active member of the room must be denied at the \
         connect gate before any hash negotiation (AC3)"
    );
    assert!(
        bytes.is_none(),
        "a connect-denied fetch must transfer zero bytes"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    charlie_node.shutdown().await.expect("shutdown charlie");
}

// ---------------------------------------------------------------------------
// AC3 (Gate 2) — an active member is denied an unreferenced hash
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_member_is_denied_an_unreferenced_hash() {
    let (room, base_log, join_id, alice, bob) = build_two_member_room();

    let blobs_tmp = TempDir::new().unwrap();
    let blobs_dir = blobs_tmp.path().join("blobs");
    // Alice's store holds two blobs; only one is ever referenced by a file.shared.
    let referenced_content = b"this one was shared";
    let unreferenced_content = b"this one was only imported, never shared";
    let referenced_hash = seed_blob(&blobs_dir, "shared.txt", referenced_content).await;
    let unreferenced_hash = seed_blob(&blobs_dir, "private.txt", unreferenced_content).await;

    let mut log = base_log.clone();
    log.push(
        file_shared(
            &alice,
            room,
            join_id,
            [0x14; 16],
            "shared.txt",
            referenced_content.len() as u64,
            referenced_hash,
            T0 + 10,
        )
        .to_bytes(),
    );

    let alice_node = spawn_provider(&alice, room, &log, blobs_dir).await;
    let alice_addr = alice_node.endpoint_addr().expect("alice addr");
    let bob_node = spawn_fetcher(&bob, room, &log).await;

    // Bob is an active member (Gate 1 passes) but the hash he asks for is one
    // Alice's store holds and never referenced by any file.shared (Gate 2 must
    // still deny it — per-hash gating independent of node admission).
    let (outcome, bytes) = bob_node
        .fetch_file(alice_addr, unreferenced_hash, unreferenced_hash, FETCH_WAIT)
        .await;

    assert_eq!(
        outcome,
        FetchOutcome::DeniedPerHash,
        "an active member must still be denied a hash no file.shared references, \
         even though the provider genuinely holds it (AC3, Gate 2)"
    );
    assert!(
        bytes.is_none(),
        "a per-hash-denied fetch must transfer zero bytes"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// AC5 — no provider listening yields an honest Unavailable, never a hang
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_provider_is_reported_unavailable_within_timeout() {
    let (room, base_log, join_id, alice, bob) = build_two_member_room();

    // Alice is a legitimate active member with a file.shared reference, but she
    // (and nobody else) is actually online holding it — the honest-unavailable
    // case (PRD §14): a dial at her identity on a port nobody is listening on.
    let hash = [0xAB; 32];
    let mut log = base_log.clone();
    log.push(
        file_shared(
            &alice,
            room,
            join_id,
            [0x15; 16],
            "ghost.txt",
            4096,
            hash,
            T0 + 10,
        )
        .to_bytes(),
    );

    let bob_node = spawn_fetcher(&bob, room, &log).await;

    let unreachable = iroh::EndpointAddr::new(alice.endpoint_id())
        .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 19_998u16)));

    let started = std::time::Instant::now();
    let (outcome, bytes) = bob_node
        .fetch_file(unreachable, hash, hash, UNAVAILABLE_WAIT)
        .await;

    assert_eq!(
        outcome,
        FetchOutcome::Unavailable,
        "no provider listening at the dialed address must report Unavailable, \
         never Fetched or a hang (AC5)"
    );
    assert!(bytes.is_none(), "an unavailable fetch must carry no bytes");
    assert!(
        started.elapsed() < UNAVAILABLE_WAIT + Duration::from_secs(2),
        "the fetch must fail within the bounded timeout, not hang (AC5): took {:?}",
        started.elapsed()
    );

    bob_node.shutdown().await.expect("shutdown bob");
}
