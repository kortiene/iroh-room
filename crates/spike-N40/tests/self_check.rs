//! The deterministic loopback self-check (spec §9 / §6.1) — the CI-runnable
//! part of #145. Spawns an N=5 loopback cluster through the **shipping**
//! `iroh_rooms_net::Node`, asserts convergence, nonzero `frames_sent`,
//! nonzero accepted counts, readable `outbound_queue_depths`, a mini rebind,
//! and JSON / Markdown rendering without panic.
//!
//! Every assertion is timeout-bounded (R4: a wiring bug fails fast instead
//! of hanging CI). The full N=40 matrix is a separate manual run committed
//! as `results/results.md`; this test only proves the harness builds and the
//! loopback harness works at the CI-feasible scale (spec §6.6 / §9).
//!
//! Each test holds a live cluster of `Node` handles (transport + engine +
//! pump per node), which exceeds clippy's `large_futures` threshold. The
//! threshold is not actionable without splitting the cluster across tasks,
//! which would obscure the spec's per-test control flow, so it is suppressed
//! at the module level.

#![allow(clippy::large_futures, clippy::too_many_lines)]

use std::time::Duration;

use anyhow::Result;
use iroh_rooms_core::event::ids::EventId;
use iroh_rooms_core::event::signed::event_id_from_bytes;
use iroh_rooms_net::{PeerConnState, GOSSIP_BOOTSTRAP_SEEDS};
use spike_n40::cluster::{ConnectMode, HarnessCluster};
use spike_n40::metrics::{classify_cascade, cluster_metrics, counter_baseline, CascadeWindow};
use spike_n40::report::{results_md, CascadeVerdict, MatrixRow, ScenarioConfig};
use spike_n40::rss::process_rss_bytes;
use spike_n40::workload::Workload;

const N: usize = 5;
const SEED_BASE: u64 = 0x5C40_5500;
const READY: Duration = Duration::from_secs(15);
const HOLD: Duration = Duration::from_secs(10);
const RECONNECT: Duration = Duration::from_secs(15);

/// Spawn an N=5 cluster (used by every test below).
async fn spawn() -> Result<HarnessCluster> {
    HarnessCluster::spawn(N, SEED_BASE, READY, ConnectMode::FullMesh).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_cluster_reaches_gossip_readiness() {
    let cluster = HarnessCluster::spawn(N, SEED_BASE + 0x100, READY, ConnectMode::Gossip)
        .await
        .expect("spawn N=5 gossip cluster");
    assert_eq!(cluster.connect_mode, ConnectMode::Gossip);
    let expected = GOSSIP_BOOTSTRAP_SEEDS.min(N - 1);
    let connected = cluster.connected_counts();
    let total: usize = connected.iter().sum();
    assert!(
        total >= expected * N,
        "gossip cluster reached {total}/{} aggregate seed connections: {connected:?}",
        expected * N
    );
    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_cluster_reaches_full_mesh_readiness() {
    let cluster = spawn().await.expect("spawn N=5 cluster");
    let expected = N - 1;
    // Readiness was already awaited inside spawn; sanity-check the result.
    let connected: Vec<usize> = cluster.connected_counts();
    for (i, c) in connected.iter().enumerate() {
        assert_eq!(
            *c, expected,
            "node {i} reached {c}/{expected} connected peers"
        );
    }
    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_admin_publish_fans_out_to_every_node() {
    let cluster = spawn().await.expect("spawn N=5 cluster");

    // Author a short admin-authored burst parented on the genesis.
    let workload = Workload::build(
        cluster.room_id,
        &cluster.admin.identity_secret(),
        &cluster.admin.device_secret(),
        cluster.genesis_id,
        5,
        1_771_000_555_000,
        "n40 self-check",
    );
    let expected_ids: Vec<EventId> = workload.event_ids();
    for wire in &workload.wires {
        cluster.nodes[0]
            .node
            .publish(wire.to_bytes())
            .await
            .expect("publish");
    }

    // Every node must hold every published id within HOLD.
    for hn in &cluster.nodes {
        for id in &expected_ids {
            hn.node
                .wait_until_contains(*id, HOLD)
                .await
                .expect("node receives the published event");
        }
    }
    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_counters_show_frames_sent_and_accepted() {
    let cluster = spawn().await.expect("spawn N=5 cluster");

    let workload = Workload::build(
        cluster.room_id,
        &cluster.admin.identity_secret(),
        &cluster.admin.device_secret(),
        cluster.genesis_id,
        3,
        1_771_000_666_000,
        "n40 self-check",
    );
    let ids = workload.event_ids();
    for wire in &workload.wires {
        cluster.nodes[0]
            .node
            .publish(wire.to_bytes())
            .await
            .expect("publish");
    }
    for hn in &cluster.nodes {
        for id in &ids {
            hn.node.wait_until_contains(*id, HOLD).await.expect("hold");
        }
    }

    // At least one node must have frames_sent > 0 and every node must have
    // accepted >= published (genesis + the message chain).
    let mut any_frames_sent = false;
    for hn in &cluster.nodes {
        let counters = hn.node.counters().await.expect("counters");
        if counters.frames_sent > 0 {
            any_frames_sent = true;
        }
        assert!(
            counters.accepted >= workload.wires.len() as u64,
            "node {} accepted {} < published {}",
            hn.index,
            counters.accepted,
            workload.wires.len()
        );
    }
    assert!(
        any_frames_sent,
        "at least one node must have frames_sent > 0"
    );

    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_outbound_queue_depths_are_readable_and_byte_valued() {
    let cluster = spawn().await.expect("spawn N=5 cluster");

    for hn in &cluster.nodes {
        let depths = hn.node.outbound_queue_depths();
        // Every node should have at least one outbound queue (one per peer).
        assert!(
            !depths.is_empty(),
            "node {} has no outbound queues",
            hn.index
        );
        // Depth is bytes — non-negative integer (always true for usize, but
        // pin the invariant that the API returns a plain usize, not a
        // complex type).
        for (_dev, bytes) in &depths {
            assert!(
                isize::try_from(*bytes).is_ok(),
                "outbound queue depth {bytes} exceeds isize::MAX"
            );
        }
    }
    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_mini_rebound_node_catches_missed_events() {
    // The CI-feasible analog of the spec §6.8 rebind scenario at N=5: shut
    // down the last node, publish a missed event from the admin, respawn the
    // last node under the SAME device secret, dial back in, and assert the
    // rebound node receives the missed event over the mesh.
    let cluster = spawn().await.expect("spawn N=5 cluster");
    let target_index = N - 1;
    let target_seeds = spike_n40::cluster::node_seeds(N, SEED_BASE)[target_index].clone();
    let room_id = cluster.room_id;
    let genesis_id = cluster.genesis_id;
    let admin_identity = cluster.admin.identity;
    let admin_identity_seed = cluster.admin.identity_seed;
    let admin_device_seed = cluster.admin.device_seed;
    let admin_endpoint_id = cluster.admin.endpoint_id;
    let audit = cluster.audit.clone();

    // Move nodes out, shut down the target.
    let mut all_nodes = cluster.nodes;
    let mut keep: Vec<_> = Vec::with_capacity(N - 1);
    let mut target_node = None;
    for hn in all_nodes.drain(..) {
        if hn.index == target_index {
            target_node = Some(hn);
        } else {
            keep.push(hn);
        }
    }
    if let Some(t) = target_node {
        t.shutdown().await.expect("shutdown target");
    }

    // Publish the missed event while the target is offline.
    let admin_identity_secret =
        iroh_rooms_core::event::keys::SigningKey::from_seed(&admin_identity_seed);
    let admin_device_secret =
        iroh_rooms_core::event::keys::SigningKey::from_seed(&admin_device_seed);
    let missed = Workload::build(
        room_id,
        &admin_identity_secret,
        &admin_device_secret,
        genesis_id,
        1,
        1_771_000_777_000,
        "n40 missed",
    );
    let missed_id = event_id_from_bytes(&missed.wires[0].signed);
    keep[0]
        .node
        .publish(missed.wires[0].to_bytes())
        .await
        .expect("publish missed");

    // Wait until every surviving node holds the missed event.
    let surviving = HarnessCluster {
        connect_mode: ConnectMode::FullMesh,
        room_id,
        genesis_id,
        admin: spike_n40::cluster::AdminPrincipal {
            identity: admin_identity,
            identity_seed: admin_identity_seed,
            device_seed: admin_device_seed,
            endpoint_id: admin_endpoint_id,
        },
        nodes: keep,
        audit: audit.clone(),
    };
    for hn in &surviving.nodes {
        hn.node
            .wait_until_contains(missed_id, HOLD)
            .await
            .expect("surviving node holds missed event");
    }

    // Respawn the target with the same secret + empty store.
    let target_store = iroh_rooms_core::store::EventStore::open_in_memory().unwrap();
    let target_engine = iroh_rooms_core::sync::SyncEngine::open(
        target_store,
        room_id,
        iroh_rooms_core::sync::SyncConfig::default(),
    )
    .unwrap();
    let target_admission = std::sync::Arc::new(spike_n40::cluster::full_mesh_admission(
        &spike_n40::cluster::node_seeds(N, SEED_BASE),
    ));
    let target_cfg = iroh_rooms_net::NetConfig {
        mode: iroh_rooms_net::NetMode::Loopback,
        ..Default::default()
    };
    let target_handle = iroh_rooms_net::Node::spawn(
        target_seeds.iroh_secret(),
        target_admission,
        audit.clone(),
        target_engine,
        target_cfg,
        spike_n40::cluster::HARNESS_TICK,
    )
    .await
    .expect("respawn target");

    // Dial back in to every surviving node.
    for hn in &surviving.nodes {
        let addr = hn.node.endpoint_addr().expect("addr");
        target_handle.connect_to(addr);
    }

    // The rebound node must receive the missed event within HOLD.
    target_handle
        .wait_until_contains(missed_id, HOLD)
        .await
        .expect("rebound node catches the missed event");

    let _ = target_handle.shutdown().await;
    surviving.shutdown().await.expect("shutdown surviving");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_metrics_render_markdown_and_json_without_panic() {
    let baseline_rss = process_rss_bytes().expect("capture pre-spawn RSS baseline");
    let cluster = spawn().await.expect("spawn N=5 cluster");
    let audit_baseline = cluster.audit.snapshot();
    let counters = counter_baseline(&cluster).await.expect("counter baseline");
    let metrics = cluster_metrics(&cluster, baseline_rss, &audit_baseline, &counters, 1)
        .await
        .expect("sample cluster metrics");

    // Markdown: render one row and assert it contains the rate cell.
    let config = ScenarioConfig {
        n: N,
        rate_events_per_sec: None,
        warmup_secs: 0,
        measure_secs: 1,
        seed_base: SEED_BASE,
        connect_mode: ConnectMode::FullMesh.label().to_owned(),
    };
    let cascade = classify_cascade(
        &[CascadeWindow {
            metrics: metrics.clone(),
            published_events: 0,
            duration_secs: 1,
        }],
        true,
    );
    let row = MatrixRow {
        config,
        metrics: &metrics,
        cascade: cascade.clone(),
        recovered_by_end: true,
    };
    let table = results_md(std::slice::from_ref(&row));
    assert!(table.contains("| N | rate events/s | mode |"));
    assert!(table.contains("| 5 | idle | idle |"));

    // JSON: serialize a minimal `CascadeVerdict` and assert round-trip.
    let v = CascadeVerdict::from_triggers(false, false, false, false, String::new());
    let json = serde_json::to_string(&v).expect("serialize");
    let back: CascadeVerdict = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.began, v.began);

    // Also assert connectedness reached the expected directed count.
    assert_eq!(
        metrics.expected_connected_peer_entries,
        N * (N - 1),
        "expected_connected_peer_entries must be N*(N-1)"
    );

    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_disconnect_peer_drops_then_redials_and_delivery_recovers() {
    // The lighter `disconnect_peer` drop test (spec §6.8: "Also add a lighter
    // `disconnect_peer` drop test if useful"). Distinct from the respawn-rebind
    // test above: it exercises the in-place link-teardown → dial-with-backoff
    // redial path (the #136-guarded reconnect code) and the D4 trigger-2
    // (reconnect-churn) signal path end-to-end against a real transport cycle —
    // none of the other N=5 tests close a live link mid-session.
    //
    // Contract under test: a locally-closed link is observed by the shared
    // RecordingAudit, the mesh returns to full connectedness within a bounded
    // window, the reconnect churn flows through `cluster_metrics`, and a
    // subsequent publish still fans out to the dropped-then-redialed peer.
    let baseline_rss = process_rss_bytes().expect("capture pre-spawn RSS baseline");
    let cluster = spawn().await.expect("spawn N=5 cluster");
    let target = cluster.nodes[N - 1].endpoint_id;
    let target_identity = cluster.nodes[N - 1].identity;

    // The §16.3 connection-panel seam the cascade connectedness is derived
    // from: peer_entries must report the bound identity + Connected state for
    // the target before we tear the link down.
    let entry_before = cluster.nodes[0]
        .node
        .peer_entries()
        .into_iter()
        .find(|(dev, _)| *dev == target)
        .map(|(_, e)| e);
    assert!(
        matches!(
            entry_before,
            Some(iroh_rooms_net::PeerEntry {
                state: PeerConnState::Connected,
                identity: Some(id),
                ..
            }) if id == target_identity
        ),
        "node 0 must see node {target:?} Connected with a bound identity before disconnect"
    );

    let baseline = cluster.audit.snapshot();
    let counters = counter_baseline(&cluster).await.expect("counter baseline");

    // Locally close node 0's link to the last node. The owning dial/accept
    // task observes the close and records the drop on the shared audit.
    cluster.nodes[0].node.disconnect_peer(target);

    // D4 trigger-2 source: the drop must reach the shared RecordingAudit.
    // Bounded — a wiring bug fails fast instead of hanging.
    tokio::time::timeout(RECONNECT, async {
        loop {
            if cluster.audit.disconnected_since(&baseline) >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("disconnect did not propagate to the audit within {RECONNECT:?}");

    // The dial-with-backoff loop redials the (unchanged) loopback address and
    // the mesh returns to full connectedness (every node back at N-1).
    tokio::time::timeout(RECONNECT, async {
        loop {
            if cluster.connected_counts().iter().all(|c| *c == N - 1) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("mesh did not return to full connectedness within {RECONNECT:?}");

    // The reconnect was recorded (D4 trigger-2 signal path end-to-end).
    assert!(
        cluster.audit.connected_since(&baseline) >= 1,
        "reconnect must be recorded by the audit"
    );

    // The reconnect churn flows through the metrics pipeline (RecordingAudit
    // deltas → cluster_metrics.reconnects_per_sec), the decision-relevant
    // signal for #154.
    let metrics = cluster_metrics(&cluster, baseline_rss, &baseline, &counters, 1)
        .await
        .expect("sample reconnect metrics");
    assert!(
        metrics.reconnects_per_sec > 0.0,
        "cluster_metrics must reflect the reconnect churn; got {}",
        metrics.reconnects_per_sec
    );

    // Delivery recovers: a publish after the reconnect fans out to the
    // dropped-then-redialed peer (the actual end-to-end contract).
    let workload = Workload::build(
        cluster.room_id,
        &cluster.admin.identity_secret(),
        &cluster.admin.device_secret(),
        cluster.genesis_id,
        1,
        1_771_000_888_000,
        "n40 after reconnect",
    );
    let after_id = event_id_from_bytes(&workload.wires[0].signed);
    cluster.nodes[0]
        .node
        .publish(workload.wires[0].to_bytes())
        .await
        .expect("publish after reconnect");
    cluster.nodes[N - 1]
        .node
        .wait_until_contains(after_id, HOLD)
        .await
        .expect("redialed node receives the post-reconnect event");

    cluster.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n5_idle_connectedness_stays_at_full_mesh() {
    // Smoke: idle does not silently lose links (the pre-b0622ec collapse
    // signature). The cluster must hold full mesh for a short idle window.
    let cluster = spawn().await.expect("spawn N=5 cluster");
    tokio::time::sleep(Duration::from_secs(2)).await;
    for (i, hn) in cluster.nodes.iter().enumerate() {
        let connected = hn
            .node
            .peer_states()
            .iter()
            .filter(|(_, s)| *s == PeerConnState::Connected)
            .count();
        assert_eq!(
            connected,
            N - 1,
            "node {i} dropped a link during the idle window ({connected}/{})",
            N - 1
        );
    }
    cluster.shutdown().await.expect("shutdown");
}
