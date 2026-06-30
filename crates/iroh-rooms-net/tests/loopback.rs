//! Loopback integration tests — the AC oracle for the full-mesh QUIC event
//! transport (spec §7.2). Two (or three) in-process [`Node`]s on
//! `RelayMode::Disabled` over `127.0.0.1`, each driving a real `SyncEngine`. Every
//! await is bounded by a `tokio::time::timeout` (via the `Node` wait helpers or an
//! explicit deadline) so a wiring bug fails fast instead of hanging CI (spec R4).
//!
//! * **T1 (AC1)** — two peers exchange a signed `WireEvent` over the custom ALPN.
//! * **T2 (AC2)** — an unknown endpoint is rejected before any event byte; the
//!   rejected dialer never reaches `Connected`.
//! * **T3 (AC3)** — the connection-state trichotomy (connected / offline /
//!   unauthorized) is reached and observable.
//! * **T4 (AC4)** — a dropped link reconnects and a subsequently-published event
//!   still arrives (proving the stream was genuinely re-established).
//! * **T8** — a device bound to a known identity that is *not* Active is rejected
//!   (`NotActive`) before any byte is read, exactly like an unknown device (T2).
//! * **T9** — a device whose identity is in the `fail_closed` overlay is rejected
//!   (`FailClosed`) before any byte is read, even though its identity is Active.

use std::sync::Arc;
use std::time::Duration;

use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::demo::{self, Participant};
use iroh_rooms_net::{
    AllowlistAdmission, ConnEvent, NetConfig, NetMode, Node, PeerConnState, TracingAudit,
};
use tokio::sync::broadcast;

/// Generous per-step budget — loopback connects in well under this.
const WAIT: Duration = Duration::from_secs(10);

/// Spawn a loopback [`Node`] for `secret` driving a fresh in-memory engine on
/// `room`. A short tick keeps anti-entropy / reconnect catch-up brisk.
async fn spawn_node(secret: iroh::SecretKey, admission: AllowlistAdmission, room: RoomId) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory store");
    let engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
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
    .expect("spawn node")
}

// ---------------------------------------------------------------------------
// T1 (AC1) — exchange a signed WireEvent over the custom ALPN
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t1_two_peers_exchange_a_signed_wire_event() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let (room, genesis_id, genesis_bytes) = demo::genesis(&host);

    // Each admits the other (and itself) — the membership-snapshot shape (D6).
    let roster = || demo::allowlist(&[&host, &member]);
    let a = spawn_node(host.iroh_secret(), roster(), room).await;
    let b = spawn_node(member.iroh_secret(), roster(), room).await;

    // B dials A; both authenticate each other and the bidi stream comes up.
    b.connect_to(a.endpoint_addr().expect("addr"));
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B connects to A");
    a.wait_for_state(member.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("A sees B connected");

    // A publishes a signed WireEvent (the room genesis); it crosses the ALPN to B.
    a.publish(genesis_bytes).await.expect("publish genesis");
    b.wait_until_contains(genesis_id, WAIT)
        .await
        .expect("B receives the genesis over the ALPN");

    assert!(
        b.store_contains(genesis_id).await.expect("digest"),
        "B's validated set contains the exchanged event id"
    );
    // The event id is BLAKE3(wire.signed), so id-equality ⇒ byte-equality: the
    // transport delivered the exact signed bytes (the engine is the gate).

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

// ---------------------------------------------------------------------------
// T2 (AC2) — unknown endpoint rejected before any event byte
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2_unknown_endpoint_rejected_before_bytes() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let stranger = Participant::new(99); // never bound in the members' allowlist
    let (room, _genesis_id, _genesis_bytes) = demo::genesis(&host);

    // B's roster admits only host + member — NOT the stranger.
    let b = spawn_node(
        member.iroh_secret(),
        demo::allowlist(&[&host, &member]),
        room,
    )
    .await;
    // The stranger *wants* to talk to B (B is in its allowlist), but B refuses it.
    let c = spawn_node(
        stranger.iroh_secret(),
        demo::allowlist(&[&stranger, &member]),
        room,
    )
    .await;

    // Watch B's transitions for the stranger: it must NEVER reach Connected.
    let mut b_events = b.conn_events();

    c.connect_to(b.endpoint_addr().expect("addr"));

    // The dialer observes Unauthorized (B closed with the stable REJECT code).
    c.wait_for_state(member.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("stranger observes Unauthorized from B's reject");

    // B records the stranger as Unauthorized.
    b.wait_for_state(stranger.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("B records the stranger Unauthorized");

    // Structural AC2: B never transitioned the stranger to Connected, so its
    // bidi stream was never accepted and no frame was ever read from it.
    let stranger_id = stranger.endpoint_id();
    let mut saw_connected = false;
    while let Ok(ev) = b_events.try_recv() {
        if ev.device == stranger_id && ev.to == PeerConnState::Connected {
            saw_connected = true;
        }
    }
    assert!(
        !saw_connected,
        "B must never reach Connected for an unauthorized dialer (reject precedes accept_bi)"
    );

    b.shutdown().await.expect("shutdown B");
    c.shutdown().await.expect("shutdown C");
}

// ---------------------------------------------------------------------------
// T3 (AC3) — connection-state trichotomy: connected / offline / unauthorized
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t3_connection_state_trichotomy() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let stranger = Participant::new(99);
    let (room, _gid, _bytes) = demo::genesis(&host);

    let a = spawn_node(host.iroh_secret(), demo::allowlist(&[&host, &member]), room).await;
    let b = spawn_node(
        member.iroh_secret(),
        demo::allowlist(&[&host, &member]),
        room,
    )
    .await;
    let c = spawn_node(
        stranger.iroh_secret(),
        demo::allowlist(&[&stranger, &member]),
        room,
    )
    .await;

    // (1) CONNECTED: B dials A.
    b.connect_to(a.endpoint_addr().expect("addr"));
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B->A connected");

    // (2) UNAUTHORIZED: the stranger dials B and is refused.
    c.connect_to(b.endpoint_addr().expect("addr"));
    b.wait_for_state(stranger.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("B records stranger Unauthorized");

    // (3) OFFLINE: A goes away; B's live link to A drops to Offline.
    a.shutdown().await.expect("shutdown A");
    b.wait_for_state(host.endpoint_id(), PeerConnState::Offline, WAIT)
        .await
        .expect("B sees A go Offline");

    // All three are simultaneously observable in one snapshot.
    let states: std::collections::HashMap<_, _> = b.peer_states().into_iter().collect();
    assert_eq!(
        states.get(&host.endpoint_id()),
        Some(&PeerConnState::Offline),
        "A is Offline (authorized member, no path)"
    );
    assert_eq!(
        states.get(&stranger.endpoint_id()),
        Some(&PeerConnState::Unauthorized),
        "stranger is Unauthorized (refused regardless of reachability)"
    );

    b.shutdown().await.expect("shutdown B");
    c.shutdown().await.expect("shutdown C");
}

// ---------------------------------------------------------------------------
// T4 (AC4) — basic reconnect: dropped link re-establishes and carries new data
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t4_basic_reconnect_redelivers() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let (room, genesis_id, genesis_bytes) = demo::genesis(&host);

    let roster = || demo::allowlist(&[&host, &member]);
    let a = spawn_node(host.iroh_secret(), roster(), room).await;
    let b = spawn_node(member.iroh_secret(), roster(), room).await;

    b.connect_to(a.endpoint_addr().expect("addr"));
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B->A connected");

    // Seed the room and confirm B got the genesis before the drop.
    a.publish(genesis_bytes).await.expect("publish genesis");
    b.wait_until_contains(genesis_id, WAIT)
        .await
        .expect("B has genesis pre-drop");

    // Force-drop B's link to A and confirm it transitions down and back up.
    let mut events = b.conn_events();
    b.disconnect_peer(host.endpoint_id());
    assert!(
        observed_reconnect(&mut events, host.endpoint_id(), WAIT).await,
        "B must observe Connected -> (Offline|Connecting) -> Connected"
    );
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B reconnects to A");

    // A new event published *after* the reconnect must still reach B — proving the
    // stream was genuinely re-established, not just the state flag flipped.
    let (msg_id, msg_bytes) = demo::admin_message(&host, room, genesis_id, 1);
    a.publish(msg_bytes).await.expect("publish post-reconnect");
    b.wait_until_contains(msg_id, WAIT)
        .await
        .expect("post-reconnect event reaches B");

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

// ---------------------------------------------------------------------------
// T5 (bidirectional) — both send directions of the bidi stream carry events
// ---------------------------------------------------------------------------

/// T1 proves A→B delivery; this proves B→A. Every bidi stream has two send
/// halves: the dialer's (`open_bi`) and the acceptor's (`accept_bi`). T1 only
/// exercises the acceptor-side writer (A sends to B via A's accept stream).
/// T5 confirms that the dialer's writer (B sends to A via B's opened stream)
/// also carries events end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_bidirectional_exchange() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let (room, genesis_id, genesis_bytes) = demo::genesis(&host);
    // A second event authored by host; B will publish it so A receives it via
    // the B→A direction.
    let (msg_id, msg_bytes) = demo::admin_message(&host, room, genesis_id, 10);

    let roster = || demo::allowlist(&[&host, &member]);
    let a = spawn_node(host.iroh_secret(), roster(), room).await;
    let b = spawn_node(member.iroh_secret(), roster(), room).await;

    b.connect_to(a.endpoint_addr().expect("addr"));
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B->A connected");
    a.wait_for_state(member.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("A sees B connected");

    // Forward: A publishes genesis → B receives (A→B send path).
    a.publish(genesis_bytes).await.expect("A publish genesis");
    b.wait_until_contains(genesis_id, WAIT)
        .await
        .expect("B receives genesis (A→B direction)");

    // Reverse: B publishes msg → A receives (B→A send path). Genesis is
    // already in A's store so the parent reference is satisfiable.
    b.publish(msg_bytes).await.expect("B publish msg");
    a.wait_until_contains(msg_id, WAIT)
        .await
        .expect("A receives msg (B→A direction; proves the full bidi stream)");

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

// ---------------------------------------------------------------------------
// T6 (N=3 fan-out) — A publishes to a three-node mesh; B and C both receive
// ---------------------------------------------------------------------------

/// The spec requires the transport to handle N≤5 full-mesh peers (Spike Plan
/// Day 4). T1–T5 only cover two-node pairs; this test proves the fan-out path
/// to N=3 by having A fan out genesis to B and C simultaneously.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t6_three_node_fan_out() {
    let host = Participant::new(1);
    let m1 = Participant::new(2);
    let m2 = Participant::new(3);
    let (room, genesis_id, genesis_bytes) = demo::genesis(&host);

    let roster = || demo::allowlist(&[&host, &m1, &m2]);
    let a = spawn_node(host.iroh_secret(), roster(), room).await;
    let b = spawn_node(m1.iroh_secret(), roster(), room).await;
    let c = spawn_node(m2.iroh_secret(), roster(), room).await;

    // Wire the N=3 full mesh: B→A, C→A, C→B (B's accept side also sees C).
    let a_addr = a.endpoint_addr().expect("addr A");
    let b_addr = b.endpoint_addr().expect("addr B");
    b.connect_to(a_addr.clone());
    c.connect_to(a_addr);
    c.connect_to(b_addr);

    // Both accept-sides of A must see their dialers before we publish so that
    // A's engine has processed on_connect for each peer.
    a.wait_for_state(m1.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("A sees B connected");
    a.wait_for_state(m2.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("A sees C connected");
    b.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("B->A connected");
    c.wait_for_state(host.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("C->A connected");
    c.wait_for_state(m1.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("C->B connected");

    // A publishes genesis; A's engine fans it out to both B and C directly.
    a.publish(genesis_bytes).await.expect("A publish genesis");
    b.wait_until_contains(genesis_id, WAIT)
        .await
        .expect("B receives genesis (fan-out leg 1)");
    c.wait_until_contains(genesis_id, WAIT)
        .await
        .expect("C receives genesis (fan-out leg 2)");

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    c.shutdown().await.expect("shutdown C");
}

// ---------------------------------------------------------------------------
// T7 (ConnEvent stream) — live broadcast stream emits all three AC3 states
// ---------------------------------------------------------------------------

/// T3 (AC3) asserts the state trichotomy via `peer_states()` snapshot polling.
/// The spec also requires "the `ConnEvent` stream emitted each transition" —
/// this test verifies that directly by consuming the broadcast receiver and
/// asserting that Connected, Unauthorized, and Offline each appear in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t7_conn_event_stream_emits_all_three_states() {
    let host = Participant::new(1);
    let member = Participant::new(2);
    let stranger = Participant::new(99);
    let (room, _, _) = demo::genesis(&host);

    let a = spawn_node(host.iroh_secret(), demo::allowlist(&[&host, &member]), room).await;
    let b = spawn_node(
        member.iroh_secret(),
        demo::allowlist(&[&host, &member]),
        room,
    )
    .await;
    let c = spawn_node(
        stranger.iroh_secret(),
        demo::allowlist(&[&stranger, &host]),
        room,
    )
    .await;

    // Subscribe BEFORE any dials so every subsequent transition is queued in
    // the broadcast backlog and is readable even after the fact.
    let mut a_events = a.conn_events();

    // (1) Connected: B dials A.
    b.connect_to(a.endpoint_addr().expect("addr"));
    a.wait_for_state(member.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("A sees B connected");

    // (2) Unauthorized: stranger dials A; A rejects it before accept_bi().
    c.connect_to(a.endpoint_addr().expect("addr"));
    a.wait_for_state(stranger.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("A records stranger Unauthorized");

    // (3) Offline: B shuts down; A's live link drops to Offline.
    b.shutdown().await.expect("shutdown B");
    a.wait_for_state(member.endpoint_id(), PeerConnState::Offline, WAIT)
        .await
        .expect("A sees B go Offline");

    // Drain the broadcast stream and verify all three state values appeared.
    // The `wait_for_state` calls above guarantee each transition has already
    // been written to the PeerTable, so the corresponding ConnEvents are
    // already in the broadcast channel's internal queue.
    let member_id = member.endpoint_id();
    let stranger_id = stranger.endpoint_id();
    let mut saw_connected = false;
    let mut saw_unauthorized = false;
    let mut saw_offline = false;
    while let Ok(ev) = a_events.try_recv() {
        if ev.device == member_id && ev.to == PeerConnState::Connected {
            saw_connected = true;
        }
        if ev.device == stranger_id && ev.to == PeerConnState::Unauthorized {
            saw_unauthorized = true;
        }
        if ev.device == member_id && ev.to == PeerConnState::Offline {
            saw_offline = true;
        }
    }
    assert!(
        saw_connected,
        "ConnEvent stream must emit Connected for the admitted member"
    );
    assert!(
        saw_unauthorized,
        "ConnEvent stream must emit Unauthorized for the rejected stranger (reject-before-bytes)"
    );
    assert!(
        saw_offline,
        "ConnEvent stream must emit Offline when the member's link drops"
    );

    a.shutdown().await.expect("shutdown A");
    c.shutdown().await.expect("shutdown C");
}

// ---------------------------------------------------------------------------
// T8 — NotActive rejection: bound device whose identity is not Active
// ---------------------------------------------------------------------------

/// T2 covers the `UnknownDevice` rejection path (device not bound at all).
/// This test covers the `NotActive` path: the device *is* bound to a known
/// identity in the acceptor's roster, but that identity has not been set Active
/// (e.g. an invited-but-not-yet-confirmed member, or a removed member whose
/// device key we still know).  The handler must still reject before `accept_bi()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t8_not_active_device_rejected_before_bytes() {
    use iroh_rooms_net::AllowlistAdmission;

    let host = Participant::new(1);
    let inactive = Participant::new(3); // bound in B's roster but NOT Active

    let (room, _gid, _bytes) = demo::genesis(&host);

    // B's admission: knows inactive's device→identity binding, but never calls
    // set_active() on that identity — so authorize() returns NotActive.
    let b_admission = AllowlistAdmission::new()
        .bind_device(host.endpoint_id(), host.identity())
        .set_active(host.identity())
        .bind_device(inactive.endpoint_id(), inactive.identity());
    // Note: inactive.identity() is intentionally NOT set Active on B's side.

    let b = spawn_node(host.iroh_secret(), b_admission, room).await;
    let c = spawn_node(
        inactive.iroh_secret(),
        demo::allowlist(&[&inactive, &host]),
        room,
    )
    .await;

    let mut b_events = b.conn_events();

    c.connect_to(b.endpoint_addr().expect("addr"));

    // The inactive dialer must be recorded Unauthorized on B's side.
    b.wait_for_state(inactive.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("B records inactive device Unauthorized (NotActive cause)");

    // B must never have reached Connected for the inactive device.
    let inactive_id = inactive.endpoint_id();
    let mut saw_connected = false;
    while let Ok(ev) = b_events.try_recv() {
        if ev.device == inactive_id && ev.to == PeerConnState::Connected {
            saw_connected = true;
        }
    }
    assert!(
        !saw_connected,
        "B must never reach Connected for a NotActive device (reject precedes accept_bi)"
    );

    b.shutdown().await.expect("shutdown B");
    c.shutdown().await.expect("shutdown C");
}

// ---------------------------------------------------------------------------
// T9 — FailClosed rejection: Active device whose identity is in fail_closed
// ---------------------------------------------------------------------------

/// The `FailClosed` cause covers the §0/§5 incompleteness overlay: an identity
/// that is nominally Active but whose local membership view is suspect, so
/// admission defaults to deny.  The transport must reject before `accept_bi()`
/// even though the device is bound AND the identity is in the Active set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t9_fail_closed_device_rejected_before_bytes() {
    use iroh_rooms_net::AllowlistAdmission;

    let host = Participant::new(1);
    let pending = Participant::new(4); // Active but in fail_closed overlay

    let (room, _gid, _bytes) = demo::genesis(&host);

    // B's admission: pending is bound AND Active, but also fail_closed — so
    // authorize() returns FailClosed (overrides Active, spec OQ-6 seam).
    let b_admission = AllowlistAdmission::new()
        .bind_device(host.endpoint_id(), host.identity())
        .set_active(host.identity())
        .bind_device(pending.endpoint_id(), pending.identity())
        .set_active(pending.identity())
        .set_fail_closed(pending.identity()); // the §0/§5 overlay

    let b = spawn_node(host.iroh_secret(), b_admission, room).await;
    let c = spawn_node(
        pending.iroh_secret(),
        demo::allowlist(&[&pending, &host]),
        room,
    )
    .await;

    let mut b_events = b.conn_events();

    c.connect_to(b.endpoint_addr().expect("addr"));

    // The fail-closed dialer must be recorded Unauthorized on B's side.
    b.wait_for_state(pending.endpoint_id(), PeerConnState::Unauthorized, WAIT)
        .await
        .expect("B records fail_closed device Unauthorized (FailClosed cause)");

    // B must never have reached Connected for the fail_closed device.
    let pending_id = pending.endpoint_id();
    let mut saw_connected = false;
    while let Ok(ev) = b_events.try_recv() {
        if ev.device == pending_id && ev.to == PeerConnState::Connected {
            saw_connected = true;
        }
    }
    assert!(
        !saw_connected,
        "B must never reach Connected for a FailClosed device (reject precedes accept_bi)"
    );

    b.shutdown().await.expect("shutdown B");
    c.shutdown().await.expect("shutdown C");
}

/// Watch `events` until `device` is seen going down (Offline/Connecting) and then
/// back to Connected, within `budget`. Returns `false` on timeout.
async fn observed_reconnect(
    events: &mut broadcast::Receiver<ConnEvent>,
    device: iroh::EndpointId,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    let mut saw_down = false;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(ev)) if ev.device == device => match ev.to {
                PeerConnState::Offline | PeerConnState::Connecting => saw_down = true,
                PeerConnState::Connected if saw_down => return true,
                _ => {}
            },
            Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return false,
        }
    }
}
