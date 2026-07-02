//! `transport-probe` — the IR-0006 Day-4 gossip-vs-full-mesh comparison CLI.
//!
//! ```text
//! transport-probe compare --n <2..5> [--events <M>] [--json]
//! transport-probe late-join --backend <mesh|gossip> --n <N> --events <M>
//! transport-probe admission --backend <mesh|gossip>
//! transport-probe admin-tip
//! ```
//!
//! `compare` stands up both a mesh and a gossip cluster of `--n` loopback
//! nodes, drives the shared workload through each, and prints the comparison
//! table (spec §7.8/§7.9). `late-join` is the AC2 gap probe. `admission` is
//! the AC3 probe. `admin-tip` is the Residual Open Decision 13 probe.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use iroh::EndpointId;
use iroh_rooms_core::event::signed::event_id_from_bytes;
use iroh_rooms_core::event::{EventId, RoomId};
use iroh_rooms_core::sync::SyncMessage;
use spike_transport::report::{ComparisonResult, PropagationStats};
use spike_transport::workload::Workload;
use spike_transport::{gossip, mesh, BackendEvent, BackendKind, Cluster, TransportBackend};

const IROH_VERSION: &str = "1.0.1";
const IROH_GOSSIP_VERSION: &str = "0.101.0";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("compare") => run_compare(&args).await,
        Some("late-join") => run_late_join(&args).await,
        Some("admission") => run_admission(&args).await,
        Some("admin-tip") => run_admin_tip().await,
        _ => {
            eprintln!("{USAGE}");
            bail!("unrecognized command");
        }
    }
}

const USAGE: &str = "\
usage:
  transport-probe compare --n <2..5> [--events <M>] [--json]
  transport-probe late-join --backend <mesh|gossip> --n <N> --events <M>
  transport-probe admission --backend <mesh|gossip>
  transport-probe admin-tip";

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

async fn run_compare(args: &[String]) -> Result<()> {
    let n: usize = flag_value(args, "--n")
        .unwrap_or_else(|| "3".to_owned())
        .parse()
        .context("--n must be an integer")?;
    if !(2..=5).contains(&n) {
        bail!("--n must be in 2..=5 (spec scope, PRD §17.1.13)");
    }
    let m: usize = flag_value(args, "--events")
        .map(|s| s.parse())
        .transpose()
        .context("--events must be an integer")?
        .unwrap_or(10);
    let json = has_flag(args, "--json");

    let mesh_result = compare_mesh(n, m).await?;
    let gossip_result = compare_gossip(n, m).await?;
    let results = vec![mesh_result, gossip_result];

    if json {
        for r in &results {
            println!("{}", r.to_json().context("serialize result")?);
        }
    } else {
        println!("== transport-probe compare: n={n} events={m} ==");
        print!("{}", spike_transport::report::results_md(&results));
    }
    Ok(())
}

async fn compare_mesh(n: usize, m: usize) -> Result<ComparisonResult> {
    let nodes = mesh::spawn_full_mesh(n, 0x1000).await?;
    let cluster = Cluster::new(nodes.clone());
    let workload = Workload::build(m);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();

    let mut samples = Vec::with_capacity(workload.wires.len());
    let run_start = Instant::now();
    for wire in &workload.wires {
        let id = event_id_from_bytes(&wire.signed);
        let t0 = Instant::now();
        cluster.publish_from(0, wire.to_bytes()).await?;
        let mut single = BTreeSet::new();
        single.insert(id);
        cluster
            .await_convergence(&single, Duration::from_secs(5))
            .await;
        samples.push(duration_ms(t0.elapsed()));
    }
    let fanout_completion_ms = Some(duration_ms(run_start.elapsed()));

    let final_sets = cluster
        .await_convergence(&expected, Duration::from_secs(2))
        .await;
    let converged = final_sets.iter().all(|s| expected.is_subset(s));
    let set_delta = set_delta(&expected, &final_sets);
    let lagged_events = count_lagged(&nodes);

    Ok(ComparisonResult {
        n,
        backend: BackendKind::Mesh,
        events_published: workload.wires.len(),
        converged,
        set_delta,
        propagation: Some(PropagationStats::from_samples(
            samples,
            fanout_completion_ms,
        )),
        late_join_gap: None,
        admission_enforced: None,
        interloper_received: None,
        lagged_events,
        backend_loc: mesh::loc(),
        zerox_deps_added: 0,
        iroh_gossip_version: None,
        iroh_version: IROH_VERSION.to_owned(),
        run_note: "steady-state fan-out".to_owned(),
    })
}

async fn compare_gossip(n: usize, m: usize) -> Result<ComparisonResult> {
    let nodes = gossip::spawn_swarm(n, 0x1000).await?;
    let cluster = Cluster::new(nodes.clone());
    let workload = Workload::build(m);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();

    let mut samples = Vec::with_capacity(workload.wires.len());
    let run_start = Instant::now();
    for wire in &workload.wires {
        let id = event_id_from_bytes(&wire.signed);
        let t0 = Instant::now();
        cluster.publish_from(0, wire.to_bytes()).await?;
        let mut single = BTreeSet::new();
        single.insert(id);
        cluster
            .await_convergence(&single, Duration::from_secs(5))
            .await;
        samples.push(duration_ms(t0.elapsed()));
    }
    let fanout_completion_ms = Some(duration_ms(run_start.elapsed()));

    let final_sets = cluster
        .await_convergence(&expected, Duration::from_secs(2))
        .await;
    let converged = final_sets.iter().all(|s| expected.is_subset(s));
    let set_delta = set_delta(&expected, &final_sets);
    let lagged_events = count_lagged(&nodes);

    Ok(ComparisonResult {
        n,
        backend: BackendKind::Gossip,
        events_published: workload.wires.len(),
        converged,
        set_delta,
        propagation: Some(PropagationStats::from_samples(
            samples,
            fanout_completion_ms,
        )),
        late_join_gap: None,
        admission_enforced: None,
        interloper_received: None,
        lagged_events,
        backend_loc: gossip::loc(),
        zerox_deps_added: 1,
        iroh_gossip_version: Some(IROH_GOSSIP_VERSION.to_owned()),
        iroh_version: IROH_VERSION.to_owned(),
        run_note: "steady-state fan-out".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// late-join
// ---------------------------------------------------------------------------

async fn run_late_join(args: &[String]) -> Result<()> {
    let backend = flag_value(args, "--backend").unwrap_or_else(|| "mesh".to_owned());
    let n: usize = flag_value(args, "--n")
        .unwrap_or_else(|| "3".to_owned())
        .parse()
        .context("--n must be an integer")?;
    let m: usize = flag_value(args, "--events")
        .unwrap_or_else(|| "10".to_owned())
        .parse()
        .context("--events must be an integer")?;

    let (total_published, received, gap) = match backend.as_str() {
        "mesh" => late_join_mesh(n, m).await?,
        "gossip" => late_join_gossip(n, m).await?,
        other => bail!("unknown --backend {other:?} (want mesh|gossip)"),
    };
    println!("== transport-probe late-join: backend={backend} n={n} events={m} ==");
    println!(
        "newcomer received {received}/{total_published} pre-join events over the transport alone; gap = {gap}"
    );
    Ok(())
}

/// Returns `(total_published, received_by_newcomer, gap)`.
async fn late_join_mesh(n_pre: usize, m: usize) -> Result<(usize, usize, usize)> {
    let seed_base = 0x2000u64;
    let total = n_pre + 1;
    let allowed: HashSet<EndpointId> = (0..total)
        .map(|i| mesh::secret_from_seed(seed_base + i as u64).public())
        .collect();

    let mut nodes = Vec::with_capacity(n_pre);
    for i in 0..n_pre {
        nodes.push(Arc::new(
            mesh::MeshNode::spawn(
                mesh::secret_from_seed(seed_base + i as u64),
                allowed.clone(),
            )
            .await?,
        ));
    }
    for i in 0..n_pre {
        for j in (i + 1)..n_pre {
            let addr = nodes[j].addr()?;
            nodes[i].dial(addr).await?;
        }
    }

    let workload = Workload::build(m);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();
    for wire in &workload.wires {
        nodes[0].publish(wire.to_bytes()).await?;
    }
    let cluster = Cluster::new(nodes.clone());
    cluster
        .await_convergence(&expected, Duration::from_secs(5))
        .await;

    // The newcomer's identity was pre-provisioned (an invite already knows the
    // device id) but it dials in only *after* every pre-join event was
    // published — the AC2 gap probe.
    let newcomer = mesh::MeshNode::spawn(
        mesh::secret_from_seed(seed_base + n_pre as u64),
        allowed.clone(),
    )
    .await?;
    for existing in &nodes {
        let addr = existing.addr()?;
        newcomer.dial(addr).await?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let newcomer_ids = newcomer.received_ids();
    let received = expected.intersection(&newcomer_ids).count();
    Ok((
        workload.wires.len(),
        received,
        workload.wires.len() - received,
    ))
}

/// Returns `(total_published, received_by_newcomer, gap)`.
async fn late_join_gossip(n_pre: usize, m: usize) -> Result<(usize, usize, usize)> {
    let seed_base = 0x3000u64;
    let nodes = gossip::spawn_swarm(n_pre, seed_base).await?;

    let workload = Workload::build(m);
    let expected: BTreeSet<EventId> = workload.event_ids().into_iter().collect();
    for wire in &workload.wires {
        nodes[0].publish(wire.to_bytes()).await?;
    }
    let cluster = Cluster::new(nodes.clone());
    cluster
        .await_convergence(&expected, Duration::from_secs(5))
        .await;

    // The newcomer subscribes to the same topic only *after* every pre-join
    // event was broadcast — gossip has no history, so this must be 0 received.
    let bootstrap_addr = nodes[0].addr()?;
    let newcomer = gossip::GossipNode::spawn(
        gossip::secret_from_seed(seed_base + n_pre as u64 + 100),
        vec![bootstrap_addr],
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let newcomer_ids = newcomer.received_ids();
    let received = expected.intersection(&newcomer_ids).count();
    Ok((
        workload.wires.len(),
        received,
        workload.wires.len() - received,
    ))
}

// ---------------------------------------------------------------------------
// admission
// ---------------------------------------------------------------------------

async fn run_admission(args: &[String]) -> Result<()> {
    let backend = flag_value(args, "--backend").unwrap_or_else(|| "mesh".to_owned());
    match backend.as_str() {
        "mesh" => {
            let enforced = admission_mesh().await?;
            println!("== transport-probe admission: backend=mesh ==");
            println!("admission enforced (interloper refused pre-byte): {enforced}");
        }
        "gossip" => {
            let admitted = admission_gossip().await?;
            println!("== transport-probe admission: backend=gossip ==");
            println!("interloper admitted with no auth check (received+could publish): {admitted}");
        }
        other => bail!("unknown --backend {other:?} (want mesh|gossip)"),
    }
    Ok(())
}

async fn admission_mesh() -> Result<bool> {
    let seed_base = 0x4000u64;
    let nodes = mesh::spawn_full_mesh(3, seed_base).await?;
    let victim_addr = nodes[0].addr()?;
    let interloper_secret = mesh::secret_from_seed(seed_base + 999);
    match mesh::probe_admission_rejects_interloper(interloper_secret, victim_addr).await {
        Ok(()) => Ok(true),
        Err(err) => {
            tracing::warn!(%err, "admission probe: interloper was NOT refused");
            Ok(false)
        }
    }
}

async fn admission_gossip() -> Result<bool> {
    let seed_base = 0x5000u64;
    let nodes = gossip::spawn_swarm(3, seed_base).await?;
    let bootstrap_addr = nodes[0].addr()?;
    let interloper = gossip::interloper_join(bootstrap_addr, seed_base + 999).await?;

    // The interloper publishes; confirm at least one room member receives it
    // with no authentication check on gossip's part.
    let workload = Workload::build(1);
    let wire = &workload.wires[0];
    let id = event_id_from_bytes(&wire.signed);
    interloper.publish(wire.to_bytes()).await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received_by_room = false;
    while Instant::now() < deadline {
        if nodes.iter().any(|n| n.received_ids().contains(&id)) {
            received_by_room = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    Ok(received_by_room)
}

// ---------------------------------------------------------------------------
// admin-tip (Residual Open Decision 13)
// ---------------------------------------------------------------------------

async fn run_admin_tip() -> Result<()> {
    let mesh_ms = admin_tip_mesh().await?;
    let gossip_ms = admin_tip_gossip().await?;
    println!("== transport-probe admin-tip ==");
    println!("mesh AdminTip freshness (send -> peer observes): {mesh_ms} ms");
    println!("gossip AdminTip freshness (broadcast -> peer observes): {gossip_ms} ms");
    Ok(())
}

fn sample_admin_tip() -> SyncMessage {
    SyncMessage::AdminTip {
        room_id: RoomId::from_bytes([0x77; 32]),
        tip: Some((EventId::from_bytes([0x88; 32]), 42)),
    }
}

async fn admin_tip_mesh() -> Result<u64> {
    let seed_base = 0x6000u64;
    let nodes = mesh::spawn_full_mesh(2, seed_base).await?;
    let (a, b) = (&nodes[0], &nodes[1]);

    let t0 = Instant::now();
    a.send_control(b.id(), &sample_admin_tip())?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if b.drain_control().iter().any(|m| *m == sample_admin_tip()) {
            return Ok(duration_ms(t0.elapsed()));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    bail!("mesh admin-tip control frame was not observed within the deadline")
}

async fn admin_tip_gossip() -> Result<u64> {
    let seed_base = 0x7000u64;
    let node_a = gossip::GossipNode::spawn(gossip::secret_from_seed(seed_base), Vec::new()).await?;
    let addr_a = node_a.addr()?;
    let node_b =
        gossip::GossipNode::spawn(gossip::secret_from_seed(seed_base + 1), vec![addr_a]).await?;

    let (sender_a, _recv_a) = gossip::subscribe_liveness(&node_a.gossip(), Vec::new()).await?;
    let (_sender_b, mut recv_b) =
        gossip::subscribe_liveness(&node_b.gossip(), vec![node_a.id()]).await?;

    let payload = sample_admin_tip().encode();
    let t0 = Instant::now();
    sender_a.broadcast(payload.clone().into()).await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(Ok(iroh_gossip::api::Event::Received(msg))) =
            tokio::time::timeout(Duration::from_millis(50), recv_b.next())
                .await
                .ok()
                .flatten()
        {
            if msg.content.as_ref() == payload.as_slice() {
                return Ok(duration_ms(t0.elapsed()));
            }
        }
    }
    bail!("gossip admin-tip broadcast was not observed within the deadline")
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn set_delta(
    expected: &BTreeSet<EventId>,
    per_node: &[BTreeSet<EventId>],
) -> BTreeMap<usize, Vec<String>> {
    let mut delta = BTreeMap::new();
    for (i, held) in per_node.iter().enumerate() {
        let missing: Vec<String> = expected
            .difference(held)
            .map(EventId::to_named_string)
            .collect();
        if !missing.is_empty() {
            delta.insert(i, missing);
        }
    }
    delta
}

fn count_lagged<B: TransportBackend>(nodes: &[Arc<B>]) -> usize {
    nodes
        .iter()
        .map(|n| {
            n.drain_events()
                .iter()
                .filter(|e| matches!(e, BackendEvent::Lagged))
                .count()
        })
        .sum()
}

fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use iroh_rooms_core::event::EventId;
    use spike_transport::{BackendEvent, BackendKind, TransportBackend, WireBytes};

    use super::{count_lagged, duration_ms, flag_value, has_flag, set_delta};

    fn to_args(items: &[&str]) -> Vec<String> {
        items.iter().copied().map(String::from).collect()
    }

    fn eid(fill: u8) -> EventId {
        EventId::from_bytes([fill; 32])
    }

    #[test]
    fn flag_value_returns_the_token_after_the_flag() {
        let args = to_args(&["compare", "--n", "4", "--events", "10"]);
        assert_eq!(flag_value(&args, "--n").as_deref(), Some("4"));
        assert_eq!(flag_value(&args, "--events").as_deref(), Some("10"));
    }

    #[test]
    fn flag_value_is_none_when_flag_absent_or_dangling() {
        let present = to_args(&["compare", "--n", "3"]);
        assert!(flag_value(&present, "--backend").is_none(), "absent flag");

        // A flag in the final position has no following token to return, so it
        // must be `None` rather than panicking on the missing index.
        let trailing = to_args(&["compare", "--n"]);
        assert!(flag_value(&trailing, "--n").is_none(), "dangling flag");
    }

    #[test]
    fn flag_value_takes_the_first_occurrence() {
        let args = to_args(&["compare", "--n", "2", "--n", "5"]);
        assert_eq!(flag_value(&args, "--n").as_deref(), Some("2"));
    }

    #[test]
    fn has_flag_detects_presence_only() {
        let args = to_args(&["compare", "--json"]);
        assert!(has_flag(&args, "--json"));
        assert!(!has_flag(&args, "--quiet"));
    }

    #[test]
    fn duration_ms_reports_whole_milliseconds() {
        assert_eq!(duration_ms(Duration::from_millis(1500)), 1500);
        assert_eq!(duration_ms(Duration::ZERO), 0);
    }

    #[test]
    fn duration_ms_saturates_instead_of_overflowing() {
        // `Duration::as_millis()` is a u128; a duration whose millisecond count
        // exceeds `u64::MAX` must saturate, never wrap or panic (these values
        // feed the u64 latency fields of `ComparisonResult`).
        assert_eq!(duration_ms(Duration::from_secs(u64::MAX)), u64::MAX);
    }

    #[test]
    fn set_delta_is_empty_when_every_node_holds_the_expected_set() {
        let expected: BTreeSet<EventId> = [eid(1), eid(2)].into_iter().collect();
        let per_node = vec![expected.clone(), expected.clone()];
        assert!(
            set_delta(&expected, &per_node).is_empty(),
            "a fully-converged cluster produces no failure-ledger delta"
        );
    }

    #[test]
    fn set_delta_names_missing_ids_only_for_incomplete_nodes() {
        let expected: BTreeSet<EventId> = [eid(1), eid(2), eid(3)].into_iter().collect();
        let complete = expected.clone();
        let partial: BTreeSet<EventId> = [eid(1)].into_iter().collect();
        let per_node = vec![complete, partial];

        let delta = set_delta(&expected, &per_node);
        assert!(
            !delta.contains_key(&0),
            "node 0 holds the full set, so it must be absent from the delta"
        );
        // `BTreeSet::difference` yields ascending id order, so the named strings
        // are the exact missing ids (eid(2), eid(3)) in that order.
        let missing = delta.get(&1).expect("node 1 is missing ids");
        assert_eq!(
            missing,
            &vec![eid(2).to_named_string(), eid(3).to_named_string()],
            "the gap lists the exact missing ids as blake3-named strings, in id order"
        );
        assert!(
            missing.iter().all(|s| s.starts_with("blake3:")),
            "delta ids render in the named `blake3:<hex>` form: {missing:?}"
        );
    }

    /// A stub backend that replays a fixed failure-signal ledger from
    /// `drain_events()` — enough to exercise `count_lagged`'s filter+sum with
    /// no endpoint. `publish`/`received_ids` are unused by that path.
    struct EventStub(Vec<BackendEvent>);

    #[async_trait]
    impl TransportBackend for EventStub {
        fn kind(&self) -> BackendKind {
            BackendKind::Gossip
        }
        async fn publish(&self, _wire: WireBytes) -> anyhow::Result<()> {
            Ok(())
        }
        fn received_ids(&self) -> BTreeSet<EventId> {
            BTreeSet::new()
        }
        fn drain_events(&self) -> Vec<BackendEvent> {
            self.0.clone()
        }
    }

    #[test]
    fn count_lagged_counts_only_lagged_events_across_all_nodes() {
        let nodes = vec![
            Arc::new(EventStub(vec![
                BackendEvent::Lagged,
                BackendEvent::LinkDropped,
                BackendEvent::Lagged,
            ])),
            Arc::new(EventStub(vec![BackendEvent::AdmissionRejected])),
            Arc::new(EventStub(vec![BackendEvent::Lagged])),
        ];
        // 2 (node 0) + 0 (node 1: no Lagged) + 1 (node 2) = 3.
        assert_eq!(count_lagged(&nodes), 3);
    }

    #[test]
    fn count_lagged_is_zero_without_any_lagged_signal() {
        let nodes = vec![Arc::new(EventStub(vec![
            BackendEvent::AdmissionRejected,
            BackendEvent::InterloperAdmitted,
            BackendEvent::LinkDropped,
        ]))];
        assert_eq!(
            count_lagged(&nodes),
            0,
            "non-Lagged failure signals must not inflate the lagged_events measurement"
        );
    }
}
