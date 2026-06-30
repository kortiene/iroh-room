//! End-to-end Live Pipe Plane suite — the acceptance-criteria oracle for IR-0010
//! (spec `live-tcp-pipe-path.md` §8). Two/three in-process [`Node`]s on
//! `NetMode::Loopback` (`RelayMode::Disabled`, deterministic CI), membership seeded
//! via core event builders (Alice admin/owner, Bob allowed, Carol Active but not
//! allowed, Mallory non-member), and a trivial in-test echo TCP server as the
//! owner's loopback forward target. Every await is `tokio::time::timeout`-bounded so
//! a wiring bug fails fast instead of hanging CI.
//!
//! | # | Scenario | AC |
//! |---|---|---|
//! | P1 | Bob (allowed) round-trips bytes through the pipe to the echo server | AC1 |
//! | P2 | Carol (Active, not allowed) is denied `not_allowed`; zero bytes forwarded | AC2 |
//! | P3 | Mallory (non-member) is rejected at stage 1 before any hello is read | AC3 |
//! | P4a | Owner `close` while Bob forwards tears the session down; reconnect denied | AC4 |
//! | P4b | Owner node shutdown while Bob forwards drops the live session | AC4 |
//! | P5 | Admin removes Bob; the owner tears the active session down within ≤1 tick | AC5 |
//! | P6 | An expired pipe is denied `expired` (the one wall-clock consultation) | §5 |

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MemberRemoved, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::pipe::PipeHello;
use iroh_rooms_net::{
    AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, PipeOutcome, TracingAudit,
    PIPE_ALPN,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const NONCE: [u8; 16] = [0xcd; 16];
const T0: u64 = 1_750_000_000_000;
const WAIT: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// Principals + room-log fixtures (mirrors tests/message_e2e.rs)
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
    fn device_key(&self) -> DeviceKey {
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

/// Append a genesis(admin) → invite → join pair for `member` onto `log`, parented on
/// `prev`. Returns the join event id (the new head).
fn invite_and_join(
    log: &mut Vec<Vec<u8>>,
    room: RoomId,
    admin: &Principal,
    member: &Principal,
    prev: EventId,
    invite_seed: u8,
    created_at: u64,
) -> EventId {
    let inv_id = [invite_seed; 16];
    let inv_sec = [invite_seed.wrapping_add(0x40); 16];
    let invite = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MemberInvited,
        created_at,
        prev_events: vec![prev],
        content: Content::MemberInvited(MemberInvited {
            invite_id: inv_id,
            capability_hash: capability_hash(&room, &inv_id, &inv_sec),
            role: "member".to_owned(),
            invitee_key: member.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite_id = invite.event_id();
    log.push(wire_bytes(&invite, &admin.dev));

    let join = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: member.identity(),
        device_id: member.device_key(),
        event_type: EventType::MemberJoined,
        created_at: created_at + 1,
        prev_events: vec![invite_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: inv_id,
            capability_secret: inv_sec,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &member.id, member.device_key()),
            display_name: None,
        }),
    };
    let join_id = join.event_id();
    log.push(wire_bytes(&join, &member.dev));
    join_id
}

/// A room with Alice (admin/owner), Bob, and Carol all Active. Returns
/// `(room, log, alice, bob, carol)`.
fn build_room() -> (RoomId, Vec<Vec<u8>>, Principal, Principal, Principal) {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
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
            room_name: "Pipe E2E".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device_key()),
        }),
    };
    let genesis_id = genesis.event_id();
    log.push(wire_bytes(&genesis, &alice.dev));

    let bob_join = invite_and_join(&mut log, room, &alice, &bob, genesis_id, 0x01, T0 + 1);
    let _carol_join = invite_and_join(&mut log, room, &alice, &carol, bob_join, 0x02, T0 + 10);

    (room, log, alice, bob, carol)
}

/// Build an admin-authored `member.removed` for `subject`, parented on `prev`.
fn build_member_removed(
    room: RoomId,
    admin: &Principal,
    subject: &Principal,
    prev: Vec<EventId>,
    created_at: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MemberRemoved,
        created_at,
        prev_events: prev,
        content: Content::MemberRemoved(MemberRemoved {
            member_id: subject.identity(),
            removed_by: admin.identity(),
            reason: Some("revoked".to_owned()),
            device_binding: None,
        }),
    };
    wire_bytes(&ev, &admin.dev)
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

async fn spawn_node(
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
        TICK,
    )
    .await
    .expect("spawn loopback node")
}

// ---------------------------------------------------------------------------
// Echo server + small await helpers
// ---------------------------------------------------------------------------

/// A loopback echo TCP server. Returns its address and a counter of accepted
/// connections (so a test can prove the owner never connected to it on a denial).
async fn spawn_echo_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind echo server");
    let addr = listener.local_addr().expect("echo addr");
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = count.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            count2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, count)
}

/// Connect the event plane between two nodes (both directions reach `Connected`).
async fn connect_event_plane(a: &Node, a_p: &Principal, b: &Node, b_p: &Principal) {
    b.connect_to(a.endpoint_addr().expect("addr"));
    b.wait_for_state(a_p.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("event-plane connect (b→a)");
    a.wait_for_state(b_p.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("event-plane connect (a sees b)");
}

async fn wait_pipe_opened(node: &Node, pipe_id: [u8; 16]) {
    tokio::time::timeout(WAIT, async {
        loop {
            if node.pipe_opened(pipe_id).await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("pipe.opened synced to connector");
}

async fn wait_sessions(node: &Node, want: usize) {
    tokio::time::timeout(WAIT, async {
        while node.live_pipe_sessions() != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {want} live sessions, got {}",
            node.live_pipe_sessions()
        )
    });
}

fn loopback_addr(node: &Node) -> EndpointAddr {
    node.endpoint_addr().expect("owner endpoint addr")
}

// ---------------------------------------------------------------------------
// P1 (AC1) — authorized member round-trips bytes through the pipe
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_authorized_member_forwards_to_local_service() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    // Alice exposes the echo server, allowing only Bob.
    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");

    wait_pipe_opened(&bob_node, pipe_id).await;

    // Bob connects through the pipe and round-trips bytes.
    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects pipe");
    let local = forwarder.local_addr();

    let echoed = tokio::time::timeout(WAIT, async {
        let mut client = TcpStream::connect(local).await.expect("connect local");
        client.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.expect("read echo");
        buf
    })
    .await
    .expect("round-trip within budget");
    assert_eq!(
        &echoed, b"ping",
        "AC1: bytes must echo back through the pipe"
    );

    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Forwarded),
        "AC1: the connector must report a forwarded stream"
    );
    assert!(
        echo_count.load(Ordering::SeqCst) >= 1,
        "AC1: the owner must have connected to the echo server"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P2 (AC2) — Active member not in allowed_members is denied; zero bytes forwarded
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2_non_allowlisted_member_is_denied() {
    let (room, log, alice, bob, carol) = build_room();
    let (echo, echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(
        alice.iroh_secret(),
        allowlist(&[&alice, &bob, &carol]),
        room,
        &log,
    )
    .await;
    let carol_node = spawn_node(
        carol.iroh_secret(),
        allowlist(&[&alice, &bob, &carol]),
        room,
        &log,
    )
    .await;
    connect_event_plane(&alice_node, &alice, &carol_node, &carol).await;

    // The pipe allows only Bob — Carol is Active but not in allowed_members.
    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&carol_node, pipe_id).await;

    let mut forwarder = carol_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("carol dials pipe");
    let local = forwarder.local_addr();

    // Carol connects a local client; the per-pipe gate denies the stream.
    let mut client = TcpStream::connect(local).await.expect("connect local");
    let _ = client.write_all(b"ping").await;

    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Denied),
        "AC2: a non-allowlisted member must be denied at the per-pipe gate"
    );
    // The denied local socket is closed: a read yields no echo bytes — either a
    // clean EOF (0) or a reset, depending on the platform. Never the "ping" echo.
    let mut buf = [0u8; 4];
    let read = tokio::time::timeout(WAIT, client.read(&mut buf))
        .await
        .expect("read completes");
    match read {
        Ok(0) | Err(_) => {} // EOF or reset — denied, no bytes forwarded
        Ok(n) => assert_ne!(
            &buf[..n],
            b"ping",
            "AC2: a denied stream must never echo forwarded bytes"
        ),
    }
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        0,
        "AC2: the owner must never connect to the echo server for a denied member"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    carol_node.shutdown().await.expect("shutdown carol");
}

// ---------------------------------------------------------------------------
// P3 (AC3) — non-member is rejected at stage 1 before any hello is read
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p3_non_member_is_rejected_at_stage_one() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, echo_count) = spawn_echo_server().await;
    let mallory = Principal::new(0x99); // not in the membership log

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    // Mallory's node needs no membership; it only raw-dials the pipe ALPN.
    let mallory_node = spawn_node(mallory.iroh_secret(), allowlist(&[&mallory]), room, &[]).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");

    // Mallory raw-dials the pipe ALPN; Alice's snapshot does not know this device,
    // so the handler closes the connection BEFORE accept_bi (stage 1).
    let conn = tokio::time::timeout(
        WAIT,
        mallory_node
            .endpoint()
            .connect(loopback_addr(&alice_node), PIPE_ALPN),
    )
    .await
    .expect("dial within budget")
    .expect("QUIC handshake succeeds (rejection happens at the app gate)");

    // The owner closes the connection; an attempt to open a stream + read a gate
    // reply must therefore fail — no reply is ever produced.
    let reply = tokio::time::timeout(WAIT, async {
        let (mut send, mut recv) = conn.open_bi().await?;
        PipeHello::new(pipe_id).write_to(&mut send).await?;
        let mut byte = [0u8; 1];
        recv.read_exact(&mut byte).await?;
        anyhow::Ok(byte[0])
    })
    .await
    .expect("the stage-1 reject resolves quickly");
    assert!(
        reply.is_err(),
        "AC3: a non-member must never receive a gate reply (rejected before accept_bi)"
    );
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        0,
        "AC3: a non-member must forward zero bytes to the echo server"
    );

    alice_node.shutdown().await.expect("shutdown alice");
    mallory_node.shutdown().await.expect("shutdown mallory");
}

// ---------------------------------------------------------------------------
// P4a (AC4) — owner close tears the live session down; reconnect denied
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p4a_owner_close_tears_down_and_denies_reconnect() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, _echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&bob_node, pipe_id).await;

    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects");
    let local = forwarder.local_addr();

    // Establish a live session: a client connected and forwarding.
    let mut client = TcpStream::connect(local).await.expect("connect");
    client.write_all(b"hi").await.expect("write");
    let mut buf = [0u8; 2];
    tokio::time::timeout(WAIT, client.read_exact(&mut buf))
        .await
        .expect("read")
        .expect("echo");
    assert_eq!(forwarder.next_outcome().await, Some(PipeOutcome::Forwarded));
    wait_sessions(&alice_node, 1).await;

    // Owner closes the pipe: the live session is torn down.
    alice_node
        .pipe_close(
            &alice.id,
            &alice.dev,
            &room,
            pipe_id,
            Some("closed"),
            T0 + 200,
        )
        .await
        .expect("close pipe");
    wait_sessions(&alice_node, 0).await;

    // The live client's connection drops (read returns EOF).
    let mut tail = [0u8; 1];
    let n = tokio::time::timeout(WAIT, client.read(&mut tail))
        .await
        .expect("read completes")
        .unwrap_or(0);
    assert_eq!(
        n, 0,
        "AC4: an open client must drop when the pipe is closed"
    );

    // A fresh connect is denied (the pipe is closed / its target is gone).
    let mut reconnect = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob re-dials");
    let _c = TcpStream::connect(reconnect.local_addr())
        .await
        .expect("connect");
    assert_eq!(
        reconnect.next_outcome().await,
        Some(PipeOutcome::Denied),
        "AC4: a connect after close must be denied"
    );

    forwarder.shutdown();
    reconnect.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P4b (AC4) — owner node shutdown drops the live session
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p4b_owner_shutdown_drops_live_session() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, _echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&bob_node, pipe_id).await;

    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects");
    let mut client = TcpStream::connect(forwarder.local_addr())
        .await
        .expect("connect");
    client.write_all(b"yo").await.expect("write");
    let mut buf = [0u8; 2];
    tokio::time::timeout(WAIT, client.read_exact(&mut buf))
        .await
        .expect("read")
        .expect("echo");
    assert_eq!(forwarder.next_outcome().await, Some(PipeOutcome::Forwarded));

    // Owner node goes away: the connection drops, the client sees EOF.
    alice_node.shutdown().await.expect("shutdown alice");
    let mut tail = [0u8; 1];
    let n = tokio::time::timeout(WAIT, client.read(&mut tail))
        .await
        .expect("read completes")
        .unwrap_or(0);
    assert_eq!(
        n, 0,
        "AC4: the live session drops when the owner node exits"
    );

    forwarder.shutdown();
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P5 (AC5) — revocation-on-learn tears down an active session
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p5_revocation_on_learn_tears_down_active_session() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, _echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&bob_node, pipe_id).await;

    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects");
    let mut client = TcpStream::connect(forwarder.local_addr())
        .await
        .expect("connect");
    client.write_all(b"ok").await.expect("write");
    let mut buf = [0u8; 2];
    tokio::time::timeout(WAIT, client.read_exact(&mut buf))
        .await
        .expect("read")
        .expect("echo");
    assert_eq!(forwarder.next_outcome().await, Some(PipeOutcome::Forwarded));
    wait_sessions(&alice_node, 1).await;

    // The admin removes Bob; the removal reaches the owner (Alice publishes it).
    let heads = alice_node.heads().await.expect("heads");
    let removed = build_member_removed(room, &alice, &bob, heads, T0 + 300);
    alice_node.publish(removed).await.expect("publish removal");

    // Within ≤1 tick the watcher re-evaluates and tears Bob's session down.
    wait_sessions(&alice_node, 0).await;
    let mut tail = [0u8; 1];
    let n = tokio::time::timeout(WAIT, client.read(&mut tail))
        .await
        .expect("read completes")
        .unwrap_or(0);
    assert_eq!(n, 0, "AC5: the active session drops when Bob is removed");

    // A fresh connect is denied (Bob is no longer Active — stage 1 / stage 2).
    let mut reconnect = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob re-dials");
    let _c = TcpStream::connect(reconnect.local_addr())
        .await
        .expect("connect");
    let outcome = reconnect.next_outcome().await;
    assert!(
        matches!(
            outcome,
            Some(PipeOutcome::Denied | PipeOutcome::OwnerClosed)
        ),
        "AC5: a removed member's reconnect must be refused, got {outcome:?}"
    );

    forwarder.shutdown();
    reconnect.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P6 (§5 expiry) — an expired pipe is denied (the one wall-clock consultation)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p6_expired_pipe_is_denied() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, echo_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    // expires_at far in the past (1 ms since epoch) → expired against the wall clock.
    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo,
            "echo",
            "localhost:echo",
            &[bob.identity()],
            Some(1),
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&bob_node, pipe_id).await;

    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects");
    let _c = TcpStream::connect(forwarder.local_addr())
        .await
        .expect("connect");
    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Denied),
        "§5: an expired pipe must be denied"
    );
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        0,
        "§5: an expired pipe forwards nothing"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}
