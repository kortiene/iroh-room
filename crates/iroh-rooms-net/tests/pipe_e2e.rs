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
//! | P11 | Two pipes exposed; per-pipe session count/info distinguish the connected pipe from the idle one, and closing one pipe decrements only its count | issue #86 |
//! | P12 | The connector's stream budget saturates: forward #`N+1` is refused with a visible `PipeOutcome::Saturated` and an immediately-closed local socket, never accepted-then-starved | scale guardrails |

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
    AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, PipeAuditSink, PipeDenyCause,
    PipeOutcome, TracingAudit, PIPE_ALPN, PIPE_MAX_CONCURRENT_FORWARDS,
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

/// Poll until `pipe_id`'s per-pipe live-session count reads `want` (issue #86).
async fn wait_sessions_for(node: &Node, pipe_id: [u8; 16], want: usize) {
    tokio::time::timeout(WAIT, async {
        while node.live_pipe_sessions_for(pipe_id) != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {want} live sessions for the pipe, got {}",
            node.live_pipe_sessions_for(pipe_id)
        )
    });
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

// ---------------------------------------------------------------------------
// Shared helpers for HTTP-server tests (P7–P9)
// ---------------------------------------------------------------------------

/// A minimal hand-rolled HTTP/1.1 server on loopback. Reads until `\r\n\r\n`
/// to consume the request, then replies with a fixed body. Returns its address
/// and a counter of accepted TCP connections (so tests can assert zero-bytes-
/// forwarded denial paths, spec §5.6 WI-6).
async fn spawn_http_server(body: &'static [u8]) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind http server");
    let addr = listener.local_addr().expect("http server addr");
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = count.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            count2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                // Drain the request line + headers, ignoring the body.
                let mut buf = [0u8; 4096];
                let mut filled = 0;
                loop {
                    let n = sock.read(&mut buf[filled..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    filled += n;
                    if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if filled >= buf.len() {
                        break;
                    }
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                // Drop sock → EOF on the client side.
            });
        }
    });
    (addr, count)
}

// ---------------------------------------------------------------------------
// RecordingAudit — captures reject/teardown calls (P10)
// ---------------------------------------------------------------------------

/// A `PipeAuditSink` that records every `connect_rejected` call for assertion
/// in tests that verify the "locally logged" acceptance criterion (spec AC3 /
/// IR-0108 §4.3).
#[derive(Default)]
struct RecordingAudit {
    rejects: std::sync::Mutex<Vec<PipeDenyCause>>,
}

impl PipeAuditSink for RecordingAudit {
    fn opened(&self, _pipe_id: &[u8; 16], _allowed: usize) {}
    fn closed(&self, _pipe_id: &[u8; 16], _reason: &str) {}
    fn connect_accepted(&self, _device: EndpointId, _pipe_id: &[u8; 16]) {}
    fn connect_rejected(
        &self,
        _device: EndpointId,
        _pipe_id: Option<&[u8; 16]>,
        cause: PipeDenyCause,
    ) {
        self.rejects.lock().unwrap().push(cause);
    }
    fn torndown(&self, _device: EndpointId, _pipe_id: &[u8; 16], _cause: PipeDenyCause) {}
}

// ---------------------------------------------------------------------------
// P7 (§15.7 AC1/AC4/AC6) — authorized member issues a real HTTP GET and reads
// the exact body back through the pipe (HTTP-server variant of P1, spec §5.6 WI-6)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p7_http_authorized_member_reads_exact_body() {
    let (room, log, alice, bob, _carol) = build_room();
    let (http_addr, http_count) = spawn_http_server(b"hello").await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            http_addr,
            "http",
            "localhost:http",
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

    // Issue a real HTTP/1.1 GET and read back the full response.
    let raw = tokio::time::timeout(WAIT, async {
        let mut client = TcpStream::connect(local).await.expect("connect local");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("send request");
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.expect("read response");
        buf
    })
    .await
    .expect("http round-trip within budget");

    assert!(
        raw.ends_with(b"hello"),
        "AC1/AC6: authorized HTTP GET must receive the exact body through the pipe; \
         got {} bytes: {:?}",
        raw.len(),
        &raw[raw.len().saturating_sub(16)..]
    );
    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Forwarded),
        "AC1: connector must report Forwarded"
    );
    assert!(
        http_count.load(Ordering::SeqCst) >= 1,
        "AC4: owner must have connected to the HTTP server"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P8 (§15.7 AC5) — non-allowlisted member's HTTP GET is denied; zero bytes
// reach the HTTP server (spec §5.6 WI-6 unauthorized-connect scenario)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p8_http_non_allowlisted_denied_zero_bytes_to_server() {
    let (room, log, alice, bob, carol) = build_room();
    let (http_addr, http_count) = spawn_http_server(b"hello").await;

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
            http_addr,
            "http",
            "localhost:http",
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

    // Carol issues an HTTP GET; the gate denies it before forwarding any bytes.
    let mut client = TcpStream::connect(local).await.expect("connect local");
    let _ = client
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await;

    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Denied),
        "AC5: non-allowlisted HTTP GET must be denied at the per-pipe gate"
    );
    assert_eq!(
        http_count.load(Ordering::SeqCst),
        0,
        "AC5: zero bytes must reach the HTTP server for a denied connector"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    carol_node.shutdown().await.expect("shutdown carol");
}

// ---------------------------------------------------------------------------
// P9 (§15.7 AC7/AC8) — clean close publishes `pipe.closed` on the log and
// subsequent connects are denied (spec §5.6 WI-6 clean-close scenario)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p9_clean_close_emits_pipe_closed_on_log() {
    use iroh_rooms_core::event::content::Content;
    use iroh_rooms_core::event::signed::SignedEvent;

    let (room, log, alice, bob, _carol) = build_room();
    let (http_addr, _http_count) = spawn_http_server(b"ok").await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    let pipe_id = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            http_addr,
            "http",
            "localhost:http",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe");
    wait_pipe_opened(&bob_node, pipe_id).await;

    // Establish a live HTTP session so there is something to tear down. The local
    // client is kept open (not dropped) until after the session count is confirmed
    // — the owner deregisters a session when its spliced local socket closes
    // (`handler.rs`), so an already-dropped client would race `wait_sessions` below
    // (see the same hazard documented in `p11_per_pipe_session_attribution`).
    let mut forwarder = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("bob connects");
    let local = forwarder.local_addr();
    let mut client = TcpStream::connect(local).await.expect("connect local");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("send request");
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(WAIT, client.read_to_end(&mut buf)).await;
    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Forwarded),
        "P9 setup: live HTTP session must report Forwarded"
    );
    wait_sessions(&alice_node, 1).await;
    drop(client);

    // Owner closes: publishes pipe.closed{closed} and tears the session down.
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

    // The pipe.closed event must appear on the owner's log (AC7 / AC8).
    // pipe_close() completes only after publish() returns (the pump has ingested
    // the event), so a single room_tail query is sufficient.
    let tail = alice_node.room_tail(50).await.expect("room_tail");
    let has_closed = tail.iter().any(|se| {
        SignedEvent::decode(&se.wire.signed)
            .ok()
            .is_some_and(|ev| matches!(ev.content, Content::PipeClosed(_)))
    });
    assert!(
        has_closed,
        "AC7/AC8: pipe.closed must appear on the log after pipe_close (issue AC4)"
    );

    // A fresh connect must be denied (the pipe is closed on the owner's registry).
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
        "AC7/AC8: a connect after pipe.close must be denied"
    );

    forwarder.shutdown();
    reconnect.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P10 (issue AC3) — owner-side audit sink records connect_rejected for an
// unauthorized member ("rejected and locally logged", spec §5.6 WI-6 / §4.3)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p10_audit_sink_records_connect_rejected_for_unauthorized_member() {
    let (room, log, alice, bob, carol) = build_room();
    let (echo, _echo_count) = spawn_echo_server().await;

    // Alice's node uses the RecordingAudit sink (the owner-visible path, spec AC3).
    let recording = Arc::new(RecordingAudit::default());
    let alice_node = {
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        for ev in &log {
            engine.publish(ev).expect("seed event");
        }
        let cfg = NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        };
        Node::spawn_with_pipe_audit(
            alice.iroh_secret(),
            Arc::new(allowlist(&[&alice, &bob, &carol])),
            Arc::new(TracingAudit),
            engine,
            cfg,
            TICK,
            recording.clone(),
        )
        .await
        .expect("spawn alice with recording audit")
    };
    let carol_node = spawn_node(
        carol.iroh_secret(),
        allowlist(&[&alice, &bob, &carol]),
        room,
        &log,
    )
    .await;
    connect_event_plane(&alice_node, &alice, &carol_node, &carol).await;

    // Pipe allows only Bob — Carol is Active but not in allowed_members.
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

    // Carol attempts to connect; the gate must deny and call connect_rejected.
    let mut forwarder = carol_node
        .pipe_connect(loopback_addr(&alice_node), pipe_id, 0)
        .await
        .expect("carol dials");
    let _c = TcpStream::connect(forwarder.local_addr())
        .await
        .expect("connect local");
    assert_eq!(
        forwarder.next_outcome().await,
        Some(PipeOutcome::Denied),
        "P10 setup: Carol must be denied"
    );

    // The recording audit sink must have captured a NotAllowed reject (AC3 / §13.2.7).
    let rejects = recording.rejects.lock().unwrap().clone();
    assert!(
        rejects.contains(&PipeDenyCause::NotAllowed),
        "AC3: audit sink must record connect_rejected:not_allowed for an unauthorized member; \
         got: {rejects:?}"
    );

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    carol_node.shutdown().await.expect("shutdown carol");
}

// ---------------------------------------------------------------------------
// P11 (issue #86 AC1–AC3) — per-pipe live-session attribution: with two pipes
// exposed, the connected pipe and the idle pipe are distinguishable, and
// closing one pipe decrements only its own count (spec
// `per-pipe-live-session-state.md` §7.2, the acceptance oracle for #86).
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)] // one linear two-pipe attribute-then-teardown narrative; splitting fragments it
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p11_per_pipe_session_attribution() {
    let (room, log, alice, bob, _carol) = build_room();
    let (echo_a, _echo_a_count) = spawn_echo_server().await;
    let (echo_b, _echo_b_count) = spawn_echo_server().await;

    let alice_node = spawn_node(alice.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    let bob_node = spawn_node(bob.iroh_secret(), allowlist(&[&alice, &bob]), room, &log).await;
    connect_event_plane(&alice_node, &alice, &bob_node, &bob).await;

    // Alice exposes two independent pipes, both allowing Bob.
    let pipe_a = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo_a,
            "echo-a",
            "localhost:echo-a",
            &[bob.identity()],
            None,
            T0 + 100,
        )
        .await
        .expect("expose pipe A");
    let pipe_b = alice_node
        .pipe_expose(
            &alice.id,
            &alice.dev,
            &room,
            echo_b,
            "echo-b",
            "localhost:echo-b",
            &[bob.identity()],
            None,
            T0 + 101,
        )
        .await
        .expect("expose pipe B");
    assert_ne!(pipe_a, pipe_b, "two exposes must draw distinct pipe ids");

    wait_pipe_opened(&bob_node, pipe_a).await;
    wait_pipe_opened(&bob_node, pipe_b).await;

    // Before any connector attaches, every accessor reads zero for both pipes.
    assert_eq!(alice_node.live_pipe_sessions_for(pipe_a), 0);
    assert_eq!(alice_node.live_pipe_sessions_for(pipe_b), 0);
    assert!(alice_node.pipe_session_info().is_empty());

    // Bob connects through pipe A only and round-trips a byte to force
    // the accept handler to register the session. The local client is kept
    // open (not dropped) — the owner deregisters a session when its spliced
    // local socket closes (`handler.rs`), so an already-dropped client would
    // race the very assertions below.
    let mut fwd_a = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_a, 0)
        .await
        .expect("bob connects pipe A");
    let mut client_a = TcpStream::connect(fwd_a.local_addr())
        .await
        .expect("connect local A");
    client_a.write_all(b"a").await.expect("write A");
    let mut buf_a = [0u8; 1];
    tokio::time::timeout(WAIT, client_a.read_exact(&mut buf_a))
        .await
        .expect("read A")
        .expect("echo A");
    assert_eq!(fwd_a.next_outcome().await, Some(PipeOutcome::Forwarded));
    wait_sessions_for(&alice_node, pipe_a, 1).await;

    // AC1: the connected pipe and the idle pipe are distinguishable; the
    // node-wide total (unchanged, AC4) still just sums them.
    assert_eq!(alice_node.live_pipe_sessions_for(pipe_a), 1);
    assert_eq!(alice_node.live_pipe_sessions_for(pipe_b), 0);
    assert_eq!(alice_node.live_pipe_sessions(), 1);

    // AC2: `pipe_session_info` carries exactly the one live session, attributed
    // to pipe A and Bob's device, with a since_ms timestamp populated.
    let info = alice_node.pipe_session_info();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].pipe_id, pipe_a);
    assert_eq!(info[0].device, bob.endpoint_id());
    assert!(
        info[0].since_ms > 0,
        "since_ms must be a real wall-clock stamp under the real clock"
    );

    // A second connector on pipe B: both per-pipe counts now read 1
    // independently, the node-wide total is 2, and `pipe_session_info`
    // attributes two distinct entries to the two pipes (AC2's two-pipe case).
    let mut fwd_b = bob_node
        .pipe_connect(loopback_addr(&alice_node), pipe_b, 0)
        .await
        .expect("bob connects pipe B");
    let mut client_b = TcpStream::connect(fwd_b.local_addr())
        .await
        .expect("connect local B");
    client_b.write_all(b"b").await.expect("write B");
    let mut buf_b = [0u8; 1];
    tokio::time::timeout(WAIT, client_b.read_exact(&mut buf_b))
        .await
        .expect("read B")
        .expect("echo B");
    assert_eq!(fwd_b.next_outcome().await, Some(PipeOutcome::Forwarded));
    wait_sessions_for(&alice_node, pipe_b, 1).await;

    assert_eq!(alice_node.live_pipe_sessions_for(pipe_a), 1);
    assert_eq!(alice_node.live_pipe_sessions_for(pipe_b), 1);
    assert_eq!(alice_node.live_pipe_sessions(), 2);
    let mut info = alice_node.pipe_session_info();
    info.sort_by_key(|i| i.pipe_id);
    let mut expected_ids = [pipe_a, pipe_b];
    expected_ids.sort_unstable();
    assert_eq!(info.len(), 2);
    assert_eq!([info[0].pipe_id, info[1].pipe_id], expected_ids);
    assert!(info.iter().all(|i| i.device == bob.endpoint_id()));

    // AC3: closing pipe A tears down only its own session; pipe B's count
    // (and its info entry) is untouched.
    alice_node
        .pipe_close(
            &alice.id,
            &alice.dev,
            &room,
            pipe_a,
            Some("closed"),
            T0 + 200,
        )
        .await
        .expect("close pipe A");
    wait_sessions_for(&alice_node, pipe_a, 0).await;

    assert_eq!(alice_node.live_pipe_sessions_for(pipe_a), 0);
    assert_eq!(
        alice_node.live_pipe_sessions_for(pipe_b),
        1,
        "AC3: closing pipe A must not decrement pipe B's count"
    );
    assert_eq!(alice_node.live_pipe_sessions(), 1);
    let info = alice_node.pipe_session_info();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].pipe_id, pipe_b);

    fwd_a.shutdown();
    fwd_b.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}

// ---------------------------------------------------------------------------
// P12 — the connector's concurrent-forward budget saturates visibly
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p12_over_budget_forward_is_refused_not_starved() {
    // The scale run drove 150 concurrent forwards through one connector: 100 got
    // streams and 50 parked forever inside `open_bi()` — no error, no timeout, no
    // outcome, and no `PipeHello`, so the owner could not observe the overload even
    // in principle. This test reproduces the boundary (`PIPE_MAX_CONCURRENT_FORWARDS`
    // live forwards, then one more) and pins the new contract: the excess connection
    // is refused with a typed outcome and its local socket is closed at once. Every
    // await is `WAIT`-bounded, so a regression to the old parking behaviour fails the
    // test instead of hanging CI.
    let (room, log, alice, bob, _carol) = build_room();
    let (echo, echo_count) = spawn_echo_server().await;

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
        .expect("bob connects pipe");
    let local = forwarder.local_addr();

    // Fill the budget exactly. Each client is held open (never dropped) so its permit
    // — and its QUIC stream — stays live; awaiting the `Forwarded` outcome after each
    // connect makes the fill deterministic rather than racing the accept loop.
    let mut clients = Vec::with_capacity(PIPE_MAX_CONCURRENT_FORWARDS);
    for i in 0..PIPE_MAX_CONCURRENT_FORWARDS {
        let client = tokio::time::timeout(WAIT, TcpStream::connect(local))
            .await
            .unwrap_or_else(|_| panic!("connect client {i} within budget"))
            .unwrap_or_else(|e| panic!("connect client {i}: {e}"));
        let outcome = tokio::time::timeout(WAIT, forwarder.next_outcome())
            .await
            .unwrap_or_else(|_| panic!("client {i} produced no outcome"));
        assert_eq!(
            outcome,
            Some(PipeOutcome::Forwarded),
            "client {i} is within the budget and must forward"
        );
        clients.push(client);
    }
    wait_sessions(&alice_node, PIPE_MAX_CONCURRENT_FORWARDS).await;

    // One more than the budget: the connection past the limit.
    let mut excess = tokio::time::timeout(WAIT, TcpStream::connect(local))
        .await
        .expect("local listener still accepts")
        .expect("connect the over-budget client");

    // 1. It is reported. Before this fix there was no outcome of any variant here.
    let outcome = tokio::time::timeout(WAIT, forwarder.next_outcome())
        .await
        .expect("the over-budget forward must surface an outcome, not park");
    assert_eq!(
        outcome,
        Some(PipeOutcome::Saturated),
        "saturation must be its own outcome, distinguishable from a gate denial"
    );

    // 2. Its local socket is closed immediately — the client learns it will not be
    // served instead of holding an accepted connection that never carries a byte.
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(WAIT, excess.read(&mut buf))
        .await
        .expect("the refused local socket must close, not hang")
        .expect("read the refused local socket");
    assert_eq!(n, 0, "a refused connection must see EOF, never be starved");

    // 3. Nothing reached the owner for it: the echo server saw exactly the forwards
    // that were admitted, so the refusal cost no owner-side resources.
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        PIPE_MAX_CONCURRENT_FORWARDS,
        "the over-budget connection must not reach the owner's target"
    );
    assert_eq!(
        alice_node.live_pipe_sessions(),
        PIPE_MAX_CONCURRENT_FORWARDS
    );

    // 4. The budget is a live count, not a fuse: retiring one forward frees a permit
    // and a later connection forwards normally again. This is also what pins the
    // headroom between `PIPE_MAX_CONCURRENT_FORWARDS` and `MAX_CONCURRENT_BIDI_STREAMS`
    // — with the two equal, quinn's batched stream credit means the readmitted forward
    // gets a permit but no stream, and this step never completes. The connector's
    // permit is released when its own splice future returns, which is not synchronised
    // with the owner's session bookkeeping, so retry inside the bound rather than
    // assuming the slot is free the instant the owner's count drops.
    drop(clients.pop().expect("one client to retire"));
    wait_sessions(&alice_node, PIPE_MAX_CONCURRENT_FORWARDS - 1).await;
    let mut retry = tokio::time::timeout(WAIT, async {
        loop {
            let mut client = TcpStream::connect(local)
                .await
                .expect("connect retry client");
            match forwarder.next_outcome().await {
                Some(PipeOutcome::Forwarded) => return client,
                Some(PipeOutcome::Saturated) => {
                    // The freed permit has not landed yet; close and try again.
                    client.shutdown().await.ok();
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                other => panic!("unexpected outcome while awaiting readmission: {other:?}"),
            }
        }
    })
    .await
    .expect("a freed permit must admit a later connection");
    let echoed = tokio::time::timeout(WAIT, async {
        retry.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        retry.read_exact(&mut buf).await.expect("read echo");
        buf
    })
    .await
    .expect("retry round-trip within budget");
    assert_eq!(&echoed, b"ping", "the readmitted connection must forward");

    forwarder.shutdown();
    alice_node.shutdown().await.expect("shutdown alice");
    bob_node.shutdown().await.expect("shutdown bob");
}
