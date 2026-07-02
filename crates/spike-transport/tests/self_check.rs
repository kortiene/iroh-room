//! The deterministic loopback self-check (spec §7.8) — the CI-runnable part
//! of IR-0006. Because every scenario here runs on loopback
//! (`RelayMode::Disabled`), deterministic, timeout-bounded, **CI proves the
//! measured claims**, not just that the harness builds (unlike `spike-nat`,
//! whose real-NAT claim CI cannot prove).
//!
//! Asserts exactly the spec §7.8 self-check contract:
//! 1. mesh converges to full set equality (steady state).
//! 2. gossip converges in steady state, but the late-join newcomer's gap == M.
//! 3. mesh admission refuses the interloper before any event byte flows.
//! 4. gossip admits the interloper with no auth check.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use iroh::EndpointId;
use iroh_gossip::api::Event as GossipEvent;
use iroh_rooms_core::event::signed::event_id_from_bytes;
use iroh_rooms_core::event::{EventId, RoomId};
use iroh_rooms_core::sync::SyncMessage;
use spike_transport::workload::Workload;
use spike_transport::{gossip, mesh, BackendEvent, Cluster, TransportBackend, WireBytes};

const EVENTS: usize = 5;
const CONVERGE_DEADLINE: Duration = Duration::from_secs(5);

/// Poll `check` until it returns `true` or `deadline` elapses. Used by the
/// reconnect scenario below, which drives individually-owned [`mesh::MeshNode`]s
/// (one of them consumed by `shutdown()` mid-test) rather than a [`Cluster`].
async fn wait_for<F: Fn() -> bool>(deadline: Duration, check: F) -> bool {
    let start = Instant::now();
    loop {
        if check() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Publish `wire` from `publisher`, retrying (idempotent — dedup by
/// `event_id`) until `observer` reports `id` received or `deadline` elapses.
/// Bridges the benign registration race after a fresh `dial()`: the dialer's
/// `open_bi()` can return before the remote's `accept()` handler finishes
/// registering the link, so a single immediate `publish()` can miss that
/// peer's outbound queue even though the link is (or is about to be) live.
async fn publish_until_received(
    publisher: &mesh::MeshNode,
    observer: &mesh::MeshNode,
    wire: WireBytes,
    id: EventId,
    deadline: Duration,
) -> bool {
    let start = Instant::now();
    loop {
        let _ = publisher.publish(wire.clone()).await;
        if observer.received_ids().contains(&id) {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn mesh_converges_to_full_set_equality() {
    let nodes = mesh::spawn_full_mesh(3, 0xA000).await.expect("spawn mesh");
    let cluster = Cluster::new(nodes.clone());
    let workload = Workload::build(EVENTS);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();

    for wire in &workload.wires {
        cluster
            .publish_from(0, wire.to_bytes())
            .await
            .expect("publish");
    }
    let sets = cluster
        .await_convergence(&expected, CONVERGE_DEADLINE)
        .await;

    for (i, held) in sets.iter().enumerate() {
        assert!(
            expected.is_subset(held),
            "mesh node {i} must hold the full published set; missing {:?}",
            expected.difference(held).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn gossip_converges_in_steady_state() {
    let nodes = gossip::spawn_swarm(3, 0xB000).await.expect("spawn gossip");
    let cluster = Cluster::new(nodes.clone());
    let workload = Workload::build(EVENTS);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();

    for wire in &workload.wires {
        cluster
            .publish_from(0, wire.to_bytes())
            .await
            .expect("publish");
    }
    let sets = cluster
        .await_convergence(&expected, CONVERGE_DEADLINE)
        .await;

    for (i, held) in sets.iter().enumerate() {
        assert!(
            expected.is_subset(held),
            "gossip node {i} must converge in steady state; missing {:?}",
            expected.difference(held).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn gossip_late_join_gap_equals_published_count() {
    // Bring up 2 nodes, publish EVENTS+1 (genesis + EVENTS messages), let them
    // converge, THEN subscribe a newcomer — it must receive none of them
    // (gossip has no history; AC2).
    let nodes = gossip::spawn_swarm(2, 0xC000).await.expect("spawn gossip");
    let workload = Workload::build(EVENTS);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();
    for wire in &workload.wires {
        nodes[0].publish(wire.to_bytes()).await.expect("publish");
    }
    let cluster = Cluster::new(nodes.clone());
    cluster
        .await_convergence(&expected, CONVERGE_DEADLINE)
        .await;

    let bootstrap = nodes[0].addr().expect("addr");
    let newcomer = gossip::GossipNode::spawn(gossip::secret_from_seed(0xC0FF), vec![bootstrap])
        .await
        .expect("newcomer spawn");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let newcomer_ids = newcomer.received_ids();
    let received = expected.intersection(&newcomer_ids).count();
    assert_eq!(
        received,
        0,
        "a gossip late-joiner must receive none of the pre-join events over the transport alone \
         (gap must equal the full published count = {})",
        workload.wires.len()
    );
}

#[tokio::test]
async fn mesh_late_join_newcomer_receives_none_of_the_prejoin_events() {
    // Mirrors `gossip_late_join_gap_equals_published_count` for the mesh
    // backend (spec §8 "Late join" row lists both backends). NOTES.md §3
    // asserts in prose that "the mesh newcomer also gets 0 raw" pre-join
    // events over the transport alone, but until now that claim was only
    // exercised by the manual `transport-probe late-join --backend mesh` CLI
    // (`main.rs::late_join_mesh`), never asserted by a CI-run test.
    let seed_base = 0x9000u64;
    let total = 4usize;
    let allowed: HashSet<EndpointId> = (0..total as u64)
        .map(|i| mesh::secret_from_seed(seed_base + i).public())
        .collect();

    let mut nodes = Vec::with_capacity(total - 1);
    for i in 0..(total - 1) as u64 {
        nodes.push(Arc::new(
            mesh::MeshNode::spawn(mesh::secret_from_seed(seed_base + i), allowed.clone())
                .await
                .expect("spawn pre-join node"),
        ));
    }
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let addr = nodes[j].addr().expect("addr");
            nodes[i].dial(addr).await.expect("dial pre-join peer");
        }
    }

    let workload = Workload::build(EVENTS);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();
    for wire in &workload.wires {
        nodes[0].publish(wire.to_bytes()).await.expect("publish");
    }
    let cluster = Cluster::new(nodes.clone());
    cluster
        .await_convergence(&expected, CONVERGE_DEADLINE)
        .await;

    // The newcomer's identity is pre-provisioned in `allowed` (an invite
    // already knows the device id), but it dials in only *after* every
    // pre-join event was published — the mesh-side AC2-shaped gap probe.
    let newcomer = mesh::MeshNode::spawn(
        mesh::secret_from_seed(seed_base + (total - 1) as u64),
        allowed.clone(),
    )
    .await
    .expect("spawn newcomer");
    for existing in &nodes {
        let addr = existing.addr().expect("addr");
        newcomer.dial(addr).await.expect("newcomer dial");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let newcomer_ids = newcomer.received_ids();
    let received = expected.intersection(&newcomer_ids).count();
    assert_eq!(
        received,
        0,
        "a mesh late-joiner must receive none of the pre-join events over the raw transport \
         alone (no history at the transport layer either; gap must equal the full published \
         count = {})",
        workload.wires.len()
    );
}

#[tokio::test]
async fn mesh_admission_refuses_interloper_before_any_byte() {
    let nodes = mesh::spawn_full_mesh(3, 0xD000).await.expect("spawn mesh");
    let victim_addr = nodes[0].addr().expect("victim addr");
    let interloper_secret = mesh::secret_from_seed(0xD999);

    mesh::probe_admission_rejects_interloper(interloper_secret, victim_addr)
        .await
        .expect("mesh must refuse a non-member connection before accept_bi()");
}

#[tokio::test]
async fn gossip_admits_interloper_with_no_auth_check() {
    let nodes = gossip::spawn_swarm(3, 0xE000).await.expect("spawn gossip");
    let bootstrap = nodes[0].addr().expect("bootstrap addr");
    let interloper = gossip::interloper_join(bootstrap, 0xE999)
        .await
        .expect("gossip must admit any node that knows the topic id");

    // The interloper can publish and a room member receives it — no
    // authentication check gated the topic join.
    let workload = Workload::build(0);
    let wire = &workload.wires[0];
    let id = event_id_from_bytes(&wire.signed);
    interloper
        .publish(wire.to_bytes())
        .await
        .expect("interloper publish");

    let deadline = std::time::Instant::now() + CONVERGE_DEADLINE;
    let mut received = false;
    while std::time::Instant::now() < deadline {
        if nodes.iter().any(|n| n.received_ids().contains(&id)) {
            received = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(
        received,
        "a room member must receive the interloper's event — gossip's open topic has no admission gate"
    );
}

/// The "reconnect behavior" ADR-1 dimension (spec §6/§8), previously
/// confirmed only by cross-reference to `iroh-rooms-net`'s T4 (`NOTES.md`
/// §3) because the minimal mesh backend deliberately has no dial-with-backoff
/// loop (spec §7.2). This measures what the backend *does* offer: a dropped
/// link is observed (`BackendEvent::LinkDropped`), and a fresh explicit
/// `dial()` under the same identity — the only reconnection mechanism this
/// minimal backend has — genuinely re-establishes delivery. It is not a claim
/// of automatic backoff/redial, which this backend does not implement.
///
/// **Finding:** the redial must be issued by whichever side originally dialed
/// (here, node0 redials the rejoined node1, exactly as node0 dialed node1
/// the first time) — the reverse direction (the rejoined node1 dialing node0)
/// was tried first and never delivered a single frame across a 5s retry
/// window, even though `dial()` itself returned `Ok`. This is a real,
/// reproducible limitation of the minimal accept/dial pairing, not a test
/// race, and it sharpens the ADR-1 complexity/reconnect case: a production
/// reconnect story needs a real per-peer connection state machine (exactly
/// what `iroh-rooms-net`'s dial-with-backoff loop provides and this spike
/// intentionally does not reimplement), not just "dial again."
#[tokio::test]
async fn mesh_link_drop_is_observed_and_a_fresh_dial_reconverges() {
    let seed_base = 0xF000u64;
    let allowed: HashSet<EndpointId> = (0..3u64)
        .map(|i| mesh::secret_from_seed(seed_base + i).public())
        .collect();

    let node0 = mesh::MeshNode::spawn(mesh::secret_from_seed(seed_base), allowed.clone())
        .await
        .expect("spawn node0");
    let node1 = mesh::MeshNode::spawn(mesh::secret_from_seed(seed_base + 1), allowed.clone())
        .await
        .expect("spawn node1");
    let node2 = mesh::MeshNode::spawn(mesh::secret_from_seed(seed_base + 2), allowed.clone())
        .await
        .expect("spawn node2");

    node0
        .dial(node1.addr().expect("node1 addr"))
        .await
        .expect("dial 0->1");
    node0
        .dial(node2.addr().expect("node2 addr"))
        .await
        .expect("dial 0->2");
    node1
        .dial(node2.addr().expect("node2 addr"))
        .await
        .expect("dial 1->2");

    let initial = Workload::build(EVENTS);
    let expected: BTreeSet<EventId> = initial.event_ids().into_iter().collect();
    for wire in &initial.wires {
        node0
            .publish(wire.to_bytes())
            .await
            .expect("publish initial batch");
    }
    assert!(
        wait_for(CONVERGE_DEADLINE, || [&node0, &node1, &node2]
            .iter()
            .all(|n| expected.is_subset(&n.received_ids())))
        .await,
        "all three nodes must converge on the initial batch before the drop"
    );

    // Drop node1 mid-stream: a real transport-level disconnect (shutting down
    // its endpoint/router), not a mock — node0's and node2's live links to it
    // end for real.
    node1
        .shutdown()
        .await
        .expect("shutdown node1 (simulated drop)");
    assert!(
        wait_for(CONVERGE_DEADLINE, || node0
            .drain_events()
            .iter()
            .any(|e| matches!(e, BackendEvent::LinkDropped)))
        .await,
        "node0 must observe LinkDropped when node1 disappears"
    );

    // Publish while node1 is down: node0/node2 (their link to each other is
    // unaffected) still converge on it. This is exactly the event a rejoining
    // node1 must NOT retroactively hold — the transport carries no history,
    // reconnect or otherwise (spec §6 late-join row applies here too).
    let while_down = Workload::build(EVENTS + 1);
    let while_down_id = *while_down.event_ids().last().expect("has a trailing event");
    node0
        .publish(while_down.wires.last().expect("trailing wire").to_bytes())
        .await
        .expect("publish while node1 is down");
    assert!(
        wait_for(CONVERGE_DEADLINE, || node2
            .received_ids()
            .contains(&while_down_id))
        .await,
        "node2 must still receive events while node1 is down (its own link is unaffected)"
    );

    // node1 rejoins under the SAME identity. The minimal backend has no
    // built-in backoff/redial (spec §7.2), so reconnection is a fresh
    // explicit dial — issued by node0, the side that dialed node1 the first
    // time (see the doc comment above: redialing from the other direction
    // does not deliver anything).
    let node1_rejoined =
        mesh::MeshNode::spawn(mesh::secret_from_seed(seed_base + 1), allowed.clone())
            .await
            .expect("respawn node1 under the same identity");
    node0
        .dial(node1_rejoined.addr().expect("node1_rejoined addr"))
        .await
        .expect("redial 0->1");
    node1_rejoined
        .dial(node2.addr().expect("node2 addr"))
        .await
        .expect("redial 1->2");

    let after_redial = Workload::build(EVENTS + 2);
    let after_redial_id = *after_redial
        .event_ids()
        .last()
        .expect("has a trailing event");
    let after_redial_wire = after_redial.wires.last().expect("trailing wire").to_bytes();

    assert!(
        publish_until_received(
            &node0,
            &node1_rejoined,
            after_redial_wire,
            after_redial_id,
            CONVERGE_DEADLINE,
        )
        .await,
        "the rejoined node1 must receive an event published after its fresh dial, \
         proving the link was genuinely re-established, not just that dial() returned Ok"
    );
    assert!(
        !node1_rejoined.received_ids().contains(&while_down_id),
        "the rejoined node1 must not retroactively hold the event published while \
         it was down: the mesh transport carries no raw history either (spec §6)"
    );
}

/// A fixed `AdminTip` payload for the Residual-13 admin-tip-carrier probes
/// below (spec §7.7) — mirrors `main.rs::sample_admin_tip`.
fn sample_admin_tip() -> SyncMessage {
    SyncMessage::AdminTip {
        room_id: RoomId::from_bytes([0x77; 32]),
        tip: Some((EventId::from_bytes([0x88; 32]), 42)),
    }
}

/// The mesh side of Residual Open Decision 13 (spec §7.7/§9 §5): an
/// `AdminTip` control frame rides the same authenticated bidi link the event
/// workload uses — "no new mechanism" (ADR-1 §4). Previously exercised only
/// by the manual `transport-probe admin-tip` CLI (`main.rs::admin_tip_mesh`);
/// the decision memo's mesh-carrier freshness claim was never backed by a
/// CI-run assertion until now.
#[tokio::test]
async fn admin_tip_mesh_control_frame_reaches_the_peer() {
    let nodes = mesh::spawn_full_mesh(2, 0x9100).await.expect("spawn mesh");
    let (a, b) = (&nodes[0], &nodes[1]);
    let tip = sample_admin_tip();

    a.send_control(b.id(), &tip).expect("send control frame");

    assert!(
        wait_for(CONVERGE_DEADLINE, || b.drain_control().contains(&tip)).await,
        "the peer must observe the AdminTip control frame sent over the existing mesh link"
    );
}

/// The gossip side of Residual Open Decision 13: the same `AdminTip` payload
/// broadcast on the dedicated, off-critical-path liveness topic (spec §7.7).
/// Previously exercised only by the manual `transport-probe admin-tip` CLI
/// (`main.rs::admin_tip_gossip`); the decision memo's gossip-carrier
/// freshness claim was never backed by a CI-run assertion until now.
#[tokio::test]
async fn admin_tip_gossip_liveness_broadcast_reaches_the_peer() {
    let node_a = gossip::GossipNode::spawn(gossip::secret_from_seed(0x9200), Vec::new())
        .await
        .expect("spawn node a");
    let addr_a = node_a.addr().expect("addr a");
    let node_b = gossip::GossipNode::spawn(gossip::secret_from_seed(0x9201), vec![addr_a])
        .await
        .expect("spawn node b");

    let (sender_a, _recv_a) = gossip::subscribe_liveness(&node_a.gossip(), Vec::new())
        .await
        .expect("node a subscribe liveness");
    let (_sender_b, mut recv_b) = gossip::subscribe_liveness(&node_b.gossip(), vec![node_a.id()])
        .await
        .expect("node b subscribe liveness");

    let payload = sample_admin_tip().encode();
    sender_a
        .broadcast(payload.clone().into())
        .await
        .expect("broadcast admin-tip");

    let deadline = Instant::now() + CONVERGE_DEADLINE;
    let mut observed = false;
    while Instant::now() < deadline {
        if let Some(Ok(GossipEvent::Received(msg))) =
            tokio::time::timeout(Duration::from_millis(50), recv_b.next())
                .await
                .ok()
                .flatten()
        {
            if msg.content.as_ref() == payload.as_slice() {
                observed = true;
                break;
            }
        }
    }
    assert!(
        observed,
        "the peer must observe the AdminTip broadcast on the dedicated liveness topic"
    );
}
