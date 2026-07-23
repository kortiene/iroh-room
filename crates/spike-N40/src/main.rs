//! `n40-probe` — the #145 measurement CLI (spec §6.1).
//!
//! ```text
//! n40-probe self-check [--json]
//! n40-probe matrix [--connect-mode full-mesh|gossip]
//!                  [--nodes 5,10,20,40] [--rates idle,0.1,1,5]
//!                  [--idle-secs 30] [--load-secs 60] [--low-rate-secs 120]
//!                  [--warmup-secs 10] [--json results/<name>.json]
//!                  [--markdown results/results.md]
//! n40-probe sweep --n 40 --start 0.1 --max 20 --factor 2
//!                 [--connect-mode full-mesh|gossip]
//!                 [--load-secs 60] [--warmup-secs 10]
//! n40-probe rebind --n 40 [--missed-events 10] [--rate 0.1]
//!                   [--json results/rebind.json]
//! ```
//!
//! `self-check` is the small N=5 run suitable for CI; `matrix` is the
//! required acceptance matrix; `sweep` is optional threshold bracketing;
//! `rebind` is the N=40 rebind/NAT-drop convergence probe. Markdown is the
//! default output; `--json` emits one structured `ScenarioResult` document.
//!
//! # Casts and large futures
//!
//! The publish-rate math (`(rate * window_secs).ceil() as usize`) and the
//! sweep seed (`u64::from(rate.to_bits() as u32)`) intentionally accept the
//! precision/truncation trade-offs of float ↔ integer casts on small,
//! human-scale measurement values. Several async entry points hold enough
//! live state (a full cluster of `Node` handles) that their futures exceed
//! clippy's `large_futures` threshold; the threshold is not actionable
//! without splitting the harness into smaller pieces, which would obscure
//! the spec's control flow.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use iroh_rooms_core::event::ids::EventId;
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{NetConfig, NetMode, Node, PeerConnState};
use serde_json::json;
use spike_n40::cluster::{
    full_mesh_admission, node_seeds, AdminPrincipal, ConnectMode, HarnessCluster, HARNESS_TICK,
};
use spike_n40::metrics::{
    classify_cascade, cluster_metrics, counter_baseline, recovered_by_end, run_window,
    CascadeWindow,
};
use spike_n40::report::{
    results_md, CascadeVerdict, ClusterMetrics, MatrixRow, ScenarioConfig, ScenarioKind,
    ScenarioResult,
};
use spike_n40::rss::process_rss_bytes;
use spike_n40::workload::Workload;

const IROH_VERSION: &str = "1.0.1";

/// Seed bases for the four subcommands, kept distinct so two scenarios in
/// one process never collide on room id / genesis id.
const SEED_SELF_CHECK: u64 = 0x5C40_0000;
const SEED_MATRIX_BASE: u64 = 0x5C40_0100;
const SEED_SWEEP_BASE: u64 = 0x5C40_0200;
const SEED_REBIND: u64 = 0x5C40_0300;
const DEFAULT_MATRIX_NODES: &str = "5,10,20,40";
const DEFAULT_MATRIX_RATES: &str = "idle,0.1,1,5";
const CASCADE_SAMPLE_SECS: u64 = 5;
const REBIND_CONVERGENCE_BUDGET: Duration = Duration::from_mins(1);

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
        Some("self-check") => run_self_check(&args).await,
        Some("matrix") => run_matrix(&args).await,
        Some("sweep") => run_sweep(&args).await,
        Some("rebind") => run_rebind(&args).await,
        _ => {
            eprintln!("{USAGE}");
            bail!("unrecognized command");
        }
    }
}

const USAGE: &str = "\
usage:
  n40-probe self-check [--json]
  n40-probe matrix [--connect-mode full-mesh|gossip]
                  [--nodes 5,10,20,40] [--rates idle,0.1,1,5]
                  [--idle-secs 30] [--load-secs 60] [--low-rate-secs 120]
                  [--warmup-secs 10] [--json results/<name>.json]
                  [--markdown results/results.md]
  n40-probe sweep --n 40 --start 0.1 --max 20 --factor 2
                 [--connect-mode full-mesh|gossip]
                 [--load-secs 60] [--warmup-secs 10]
  n40-probe rebind --n 40 [--missed-events 10] [--rate 0.1]
                   [--json results/rebind.json]";

// ---------------------------------------------------------------------------
// self-check
// ---------------------------------------------------------------------------

/// The default self-check shape (N=5, short low-rate burst). Exported so the
/// test harness can drive it without re-parsing CLI args.
///
/// # Errors
///
/// Propagates any cluster spawn / publish / shutdown error.
pub async fn self_check() -> Result<ScenarioResult> {
    let n = 5usize;
    run_one_scenario(
        n,
        SEED_SELF_CHECK,
        ScenarioKind::Load,
        Some(1.0),
        2,
        8,
        ConnectMode::FullMesh,
    )
    .await
}

async fn run_self_check(args: &[String]) -> Result<()> {
    let json = has_flag(args, "--json");
    let result = self_check().await?;
    if json {
        let doc = render_run_document(std::slice::from_ref(&result));
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        println!("== n40-probe self-check: N=5, short low-rate burst ==");
        println!(
            "  cascade began: {}",
            if result.cascade.began { "yes" } else { "no" }
        );
        if let Some(load) = result.load.as_ref() {
            println!(
                "  connected: {}/{}  accepted min/max: {}/{}  frames_sent min/max: {}/{}",
                load.total_connected_peer_entries,
                load.expected_connected_peer_entries,
                load.accepted_min,
                load.accepted_max,
                load.frames_sent_min,
                load.frames_sent_max,
            );
            println!(
                "  queue saturations: {}  reconnects/sec: {:.2}",
                load.queue_saturations, load.reconnects_per_sec,
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// matrix
// ---------------------------------------------------------------------------

async fn run_matrix(args: &[String]) -> Result<()> {
    let nodes_arg = flag_value(args, "--nodes").unwrap_or_else(|| DEFAULT_MATRIX_NODES.to_owned());
    let rates_arg = flag_value(args, "--rates").unwrap_or_else(|| DEFAULT_MATRIX_RATES.to_owned());
    let idle_secs: u64 = flag_value(args, "--idle-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(30);
    let load_secs: u64 = flag_value(args, "--load-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(60);
    let low_rate_secs: u64 = flag_value(args, "--low-rate-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(120);
    let warmup_secs: u64 = flag_value(args, "--warmup-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10);
    let json_path = flag_value(args, "--json");
    let markdown_path = flag_value(args, "--markdown");
    let connect_mode = parse_connect_mode(args)?;

    let nodes: Vec<usize> = parse_csv_usize(&nodes_arg, "nodes")?;
    let rates: Vec<Option<f64>> = parse_csv_rates(&rates_arg)?;

    let mut results: Vec<ScenarioResult> = Vec::new();
    let mut matrix_rows: Vec<MatrixRowOwned> = Vec::new();

    for &n in &nodes {
        for (row_index, rate_opt) in rates.iter().enumerate() {
            let measure_secs = rate_opt.map_or(idle_secs, |rate| {
                if rate <= 0.1 {
                    low_rate_secs
                } else {
                    load_secs
                }
            });
            let seed_base = matrix_seed(n, row_index);
            let kind = if rate_opt.is_some() {
                ScenarioKind::Load
            } else {
                ScenarioKind::Idle
            };
            let row_warmup = if rate_opt.is_some() { warmup_secs } else { 0 };
            let result = run_one_scenario(
                n,
                seed_base,
                kind,
                *rate_opt,
                row_warmup,
                measure_secs,
                connect_mode,
            )
            .await
            .with_context(|| {
                let rate = rate_opt.map_or_else(|| "idle".to_owned(), |r| r.to_string());
                format!("run matrix row N={n} rate={rate}")
            })?;

            let metrics = if rate_opt.is_some() {
                result
                    .load
                    .clone()
                    .context("load row missing load metrics")?
            } else {
                result
                    .idle
                    .clone()
                    .context("idle row missing idle metrics")?
            };
            let recovered = recovered_by_end(&metrics, result.published_events);
            matrix_rows.push(MatrixRowOwned {
                config: result.config.clone(),
                metrics: metrics.clone(),
                cascade: result.cascade.clone(),
                recovered_by_end: recovered,
            });
            let rate_label = rate_opt.map_or_else(|| "idle".to_owned(), |r| r.to_string());
            eprintln!(
                "[matrix] N={n} rate={rate_label} cascade={} connected={}/{}",
                if result.cascade.began { "yes" } else { "no" },
                metrics.total_connected_peer_entries,
                metrics.expected_connected_peer_entries,
            );
            results.push(result);
        }
    }

    // Render markdown.
    let md_rows: Vec<MatrixRow> = matrix_rows
        .iter()
        .map(|r| MatrixRow {
            config: r.config.clone(),
            metrics: &r.metrics,
            cascade: r.cascade.clone(),
            recovered_by_end: r.recovered_by_end,
        })
        .collect();
    let md = results_md(&md_rows);

    if let Some(path) = &markdown_path {
        std::fs::write(path, md.as_bytes()).with_context(|| format!("write markdown to {path}"))?;
    } else {
        print!("{md}");
    }

    if let Some(path) = &json_path {
        let doc = render_run_document(&results);
        let bytes = serde_json::to_vec_pretty(&doc)?;
        std::fs::write(path, bytes).with_context(|| format!("write json to {path}"))?;
    }

    Ok(())
}

/// Owned variant of [`MatrixRow`] so the matrix loop can hold rows past the
/// borrow of the metrics vector.
struct MatrixRowOwned {
    config: ScenarioConfig,
    metrics: ClusterMetrics,
    cascade: CascadeVerdict,
    recovered_by_end: bool,
}

// ---------------------------------------------------------------------------
// sweep (optional threshold bracketing)
// ---------------------------------------------------------------------------

async fn run_sweep(args: &[String]) -> Result<()> {
    let n: usize = flag_value(args, "--n")
        .ok_or_else(|| anyhow::anyhow!("--n is required for sweep"))?
        .parse()?;
    let start: f64 = flag_value(args, "--start")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(0.1);
    let max: f64 = flag_value(args, "--max")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(20.0);
    let factor: f64 = flag_value(args, "--factor")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(2.0);
    let load_secs: u64 = flag_value(args, "--load-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(60);
    let warmup_secs: u64 = flag_value(args, "--warmup-secs")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10);
    let connect_mode = parse_connect_mode(args)?;

    validate_sweep(start, max, factor)?;

    let mut rate = start;
    let mut results: Vec<ScenarioResult> = Vec::new();
    while rate <= max + f64::EPSILON {
        let seed_base = SEED_SWEEP_BASE + u64::from(rate.to_bits() as u32);
        let r = run_one_scenario(
            n,
            seed_base,
            ScenarioKind::Sweep,
            Some(rate),
            warmup_secs,
            load_secs,
            connect_mode,
        )
        .await?;
        let began = r.cascade.began;
        eprintln!(
            "[sweep] N={n} rate={rate:.3} cascade={}",
            if began { "yes" } else { "no" }
        );
        results.push(r);
        if began {
            break;
        }
        rate *= factor;
    }

    let doc = render_run_document(&results);
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// rebind
// ---------------------------------------------------------------------------

async fn run_rebind(args: &[String]) -> Result<()> {
    let n: usize = flag_value(args, "--n")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(40);
    let missed_events: usize = flag_value(args, "--missed-events")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10);
    let rate: f64 = flag_value(args, "--rate")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(0.1);
    let json_path = flag_value(args, "--json");

    let result = rebind_scenario(n, missed_events, rate).await?;
    let doc = render_run_document(std::slice::from_ref(&result));
    let bytes = serde_json::to_vec_pretty(&doc)?;
    if let Some(path) = json_path {
        std::fs::write(&path, bytes).with_context(|| format!("write json to {path}"))?;
    } else {
        println!("{}", String::from_utf8(bytes)?);
    }
    Ok(())
}

/// The N=40 rebind / NAT-drop convergence probe (spec §6.8). Records the
/// convergence time for the rebound node to receive every event published
/// while it was offline, plus the connected-count it reached.
///
/// # Errors
///
/// Propagates any cluster spawn / publish / shutdown error.
pub async fn rebind_scenario(n: usize, missed_events: usize, rate: f64) -> Result<ScenarioResult> {
    if n < 2 {
        bail!("rebind requires at least 2 nodes, got {n}");
    }
    validate_positive_rate(rate, "rate")?;

    let seed_base = SEED_REBIND;
    let readiness = readiness_timeout(n);
    let cluster = HarnessCluster::spawn(n, seed_base, readiness, ConnectMode::FullMesh)
        .await
        .context("spawn rebind cluster")?;

    // Baseline set: publish a small set, wait for every node to hold them.
    let baseline_count = 3usize;
    let baseline_workload = Workload::build(
        cluster.room_id,
        &cluster.admin.identity_secret(),
        &cluster.admin.device_secret(),
        cluster.genesis_id,
        baseline_count,
        base_created_at(seed_base),
        "n40 baseline",
    );
    let baseline_ids = baseline_workload.event_ids();
    for wire in &baseline_workload.wires {
        cluster.nodes[0].node.publish(wire.to_bytes()).await?;
    }
    wait_until_all_hold(&cluster, &baseline_ids, REBIND_CONVERGENCE_BUDGET)
        .await
        .context("baseline events did not converge before rebind")?;

    // Choose target node N-1 (the rebinding node).
    let target_index = n.saturating_sub(1);
    let target_seeds = node_seeds(n, seed_base)[target_index].clone();
    let room_id = cluster.room_id;
    let genesis_id = cluster.genesis_id;
    let admin_identity = cluster.admin.identity;
    let admin_identity_seed = cluster.admin.identity_seed;
    let admin_device_seed = cluster.admin.device_seed;
    let admin_endpoint_id = cluster.admin.endpoint_id;
    let audit = cluster.audit.clone();

    // Move the surviving nodes out, then shut down the target.
    let mut all_nodes = cluster.nodes;
    let mut keep: Vec<_> = Vec::with_capacity(n - 1);
    let mut target_node = None;
    for hn in all_nodes.drain(..) {
        if hn.index == target_index {
            target_node = Some(hn);
        } else {
            keep.push(hn);
        }
    }
    if let Some(t) = target_node {
        t.shutdown().await?;
    }

    // Publish missed_events while the target is offline.
    let admin_identity_secret = SigningKey::from_seed(&admin_identity_seed);
    let admin_device_secret = SigningKey::from_seed(&admin_device_seed);
    let missed_workload = Workload::build(
        room_id,
        &admin_identity_secret,
        &admin_device_secret,
        genesis_id,
        missed_events,
        base_created_at(seed_base) + 1000 * (baseline_count as u64 + 1),
        "n40 missed",
    );
    let published_events = publish_all_at_rate(&keep[0].node, &missed_workload, rate)
        .await
        .context("publish missed rebind events")?;
    let missed_ids: Vec<EventId> = missed_workload
        .event_ids()
        .into_iter()
        .take(published_events)
        .collect();

    // Await convergence on the surviving nodes (so we know the missed events
    // really did land on everyone but the target).
    let surviving = HarnessCluster {
        connect_mode: ConnectMode::FullMesh,
        room_id,
        genesis_id,
        admin: AdminPrincipal {
            identity: admin_identity,
            identity_seed: admin_identity_seed,
            device_seed: admin_device_seed,
            endpoint_id: admin_endpoint_id,
        },
        nodes: keep,
        audit: audit.clone(),
    };
    wait_until_all_hold(&surviving, &missed_ids, REBIND_CONVERGENCE_BUDGET)
        .await
        .context("surviving nodes did not converge before target respawn")?;

    // Respawn the target with the same secret + an empty in-memory store.
    // Spec §6.8 step 6 calls for seeding the target's store with the
    // baseline events it knew before shutdown (NOT the missed events); the
    // shipping SyncEngine does not expose a direct store-insert from the
    // public API, so we instead measure convergence from an empty store,
    // which the anti-entropy backfill still drives — recorded as a caveat.
    let target_store = EventStore::open_in_memory()?;
    let target_engine = SyncEngine::open(target_store, room_id, SyncConfig::default())?;
    let target_admission = std::sync::Arc::new(full_mesh_admission(&node_seeds(n, seed_base)));
    let target_cfg = NetConfig {
        mode: NetMode::Loopback,
        ..Default::default()
    };
    let target_node_handle = Node::spawn(
        target_seeds.iroh_secret(),
        target_admission,
        audit.clone(),
        target_engine,
        target_cfg,
        HARNESS_TICK,
    )
    .await?;

    // Start timer at respawn. Dial every surviving node.
    let t0 = Instant::now();
    for hn in &surviving.nodes {
        let addr = hn.node.endpoint_addr()?;
        target_node_handle.connect_to(addr);
    }

    // Wait until the target holds every missed event id, or the 60s
    // convergence budget elapses.
    let converged_all =
        wait_node_holds_all(&target_node_handle, &missed_ids, REBIND_CONVERGENCE_BUDGET)
            .await
            .context("query rebound target convergence")?;
    let elapsed_ms = t0.elapsed().as_millis();

    let target_connected = target_node_handle
        .peer_states()
        .iter()
        .filter(|(_, s)| *s == PeerConnState::Connected)
        .count();

    let note = format!(
        "rebind N={n}: target converged={} in {elapsed_ms} ms; connected {} / {} peers; \
         pre-seed caveat: target store was empty at respawn (anti-entropy backfill only, \
         no baseline pre-seed — spec §6.8 step 6 partial)",
        converged_all,
        target_connected,
        n - 1,
    );

    // Shut everything down cleanly.
    let _ = target_node_handle.shutdown().await;
    surviving.shutdown().await?;

    let config = ScenarioConfig {
        n,
        rate_events_per_sec: Some(rate),
        warmup_secs: 0,
        measure_secs: 60,
        seed_base,
        connect_mode: ConnectMode::FullMesh.label().to_owned(),
    };
    let cascade = CascadeVerdict::from_triggers(
        false,
        false,
        target_connected < (n - 1),
        !converged_all,
        note.clone(),
    );
    Ok(ScenarioResult {
        kind: ScenarioKind::Rebind,
        config,
        idle: None,
        load: None,
        published_events,
        cascade,
        notes: vec![note],
    })
}

// ---------------------------------------------------------------------------
// shared scenario runner
// ---------------------------------------------------------------------------

/// Run one (N × rate) load scenario in a fresh cluster (spec §6.7). Returns
/// the [`ScenarioResult`] for that single run.
///
/// # Errors
///
/// Propagates any cluster spawn / publish / shutdown error.
pub async fn run_one_scenario(
    n: usize,
    seed_base: u64,
    kind: ScenarioKind,
    rate: Option<f64>,
    warmup_secs: u64,
    measure_secs: u64,
    connect_mode: ConnectMode,
) -> Result<ScenarioResult> {
    if let Some(r) = rate {
        validate_positive_rate(r, "rate")?;
    }
    let readiness = readiness_timeout(n);
    let baseline_rss = process_rss_bytes().context("capture pre-spawn RSS baseline")?;
    let cluster = HarnessCluster::spawn(n, seed_base, readiness, connect_mode)
        .await
        .with_context(|| format!("spawn N={n} cluster"))?;

    let (idle_metrics, load_metrics, published_events, cascade) = if let Some(r) = rate {
        // Capture a short idle baseline in the same fresh row cluster, then
        // warm up before the sampled load window.
        let idle = measure_idle(&cluster, baseline_rss, measure_secs.min(5)).await?;
        run_window(warmup_secs).await;
        let workload = Workload::build(
            cluster.room_id,
            &cluster.admin.identity_secret(),
            &cluster.admin.device_secret(),
            cluster.genesis_id,
            planned_event_count(r, measure_secs),
            base_created_at(seed_base),
            "n40 load",
        );
        let load = measure_load(&cluster, baseline_rss, &workload, r, measure_secs).await?;
        (
            Some(idle.metrics),
            Some(load.metrics),
            load.published_events,
            load.cascade,
        )
    } else {
        run_window(warmup_secs).await;
        let idle = measure_idle(&cluster, baseline_rss, measure_secs).await?;
        (Some(idle.metrics), None, 0, idle.cascade)
    };

    cluster.shutdown().await?;

    Ok(ScenarioResult {
        kind,
        config: ScenarioConfig {
            n,
            rate_events_per_sec: rate,
            warmup_secs,
            measure_secs,
            seed_base,
            connect_mode: connect_mode.label().to_owned(),
        },
        idle: idle_metrics,
        load: load_metrics,
        published_events,
        cascade,
        notes: vec![format!("loopback NetMode::Loopback iroh={IROH_VERSION}")],
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct MeasurementOutcome {
    metrics: ClusterMetrics,
    cascade: CascadeVerdict,
    published_events: usize,
}

async fn measure_idle(
    cluster: &HarnessCluster,
    baseline_rss: u64,
    measure_secs: u64,
) -> Result<MeasurementOutcome> {
    let aggregate_audit = cluster.audit.snapshot();
    let aggregate_counters = counter_baseline(cluster).await?;
    let mut windows = Vec::new();
    let mut elapsed = 0u64;

    while elapsed < measure_secs {
        let sample_secs = CASCADE_SAMPLE_SECS.min(measure_secs - elapsed);
        let sample_audit = cluster.audit.snapshot();
        let sample_counters = counter_baseline(cluster).await?;
        run_window(sample_secs).await;
        let metrics = cluster_metrics(
            cluster,
            baseline_rss,
            &sample_audit,
            &sample_counters,
            sample_secs,
        )
        .await?;
        windows.push(CascadeWindow {
            metrics,
            published_events: 0,
            duration_secs: sample_secs,
        });
        elapsed += sample_secs;
    }

    let metrics = cluster_metrics(
        cluster,
        baseline_rss,
        &aggregate_audit,
        &aggregate_counters,
        measure_secs,
    )
    .await?;
    let recovered = recovered_by_end(&metrics, 0);
    let cascade = classify_cascade(&windows, recovered);
    Ok(MeasurementOutcome {
        metrics,
        cascade,
        published_events: 0,
    })
}

async fn measure_load(
    cluster: &HarnessCluster,
    baseline_rss: u64,
    workload: &Workload,
    rate: f64,
    measure_secs: u64,
) -> Result<MeasurementOutcome> {
    let aggregate_audit = cluster.audit.snapshot();
    let aggregate_counters = counter_baseline(cluster).await?;
    let mut windows = Vec::new();
    let mut elapsed = 0u64;
    let mut published_events = 0usize;

    while elapsed < measure_secs {
        let sample_secs = CASCADE_SAMPLE_SECS.min(measure_secs - elapsed);
        let sample_audit = cluster.audit.snapshot();
        let sample_counters = counter_baseline(cluster).await?;
        let target = planned_event_count(rate, elapsed + sample_secs).min(workload.wires.len());
        let published_this_window = publish_at_rate(
            &cluster.nodes[0].node,
            workload,
            published_events,
            target,
            rate,
            Duration::from_secs(sample_secs),
        )
        .await?;
        published_events += published_this_window;
        let metrics = cluster_metrics(
            cluster,
            baseline_rss,
            &sample_audit,
            &sample_counters,
            sample_secs,
        )
        .await?;
        windows.push(CascadeWindow {
            metrics,
            published_events: published_this_window,
            duration_secs: sample_secs,
        });
        elapsed += sample_secs;
    }

    let metrics = cluster_metrics(
        cluster,
        baseline_rss,
        &aggregate_audit,
        &aggregate_counters,
        measure_secs,
    )
    .await?;
    let recovered = recovered_by_end(&metrics, published_events);
    let cascade = classify_cascade(&windows, recovered);
    Ok(MeasurementOutcome {
        metrics,
        cascade,
        published_events,
    })
}

/// Publish the requested workload range at the room-wide rate, returning the
/// number of successful publishes. A publish error aborts the scenario.
async fn publish_at_rate(
    publisher: &Node,
    workload: &Workload,
    start_index: usize,
    target_index: usize,
    rate: f64,
    window: Duration,
) -> Result<usize> {
    validate_positive_rate(rate, "rate")?;
    let interval = Duration::from_secs_f64(1.0 / rate);
    let begin = Instant::now();
    let deadline = begin + window;
    let mut successful = 0usize;
    for wire in workload
        .wires
        .iter()
        .skip(start_index)
        .take(target_index.saturating_sub(start_index))
    {
        if Instant::now() >= deadline {
            break;
        }
        publisher
            .publish(wire.to_bytes())
            .await
            .context("publish load event")?;
        successful += 1;
        let next_due = begin + interval.mul_f64(successful as f64);
        let wake = next_due.min(deadline);
        if let Some(remaining) = wake.checked_duration_since(Instant::now()) {
            tokio::time::sleep(remaining).await;
        }
    }
    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        tokio::time::sleep(remaining).await;
    }
    Ok(successful)
}

/// Publish an entire workload no faster than the requested room-wide rate.
/// Unlike a bounded measurement window, the rebind setup must publish every
/// event before it can assert survivor convergence.
async fn publish_all_at_rate(publisher: &Node, workload: &Workload, rate: f64) -> Result<usize> {
    validate_positive_rate(rate, "rate")?;
    let interval = Duration::from_secs_f64(1.0 / rate);
    let begin = Instant::now();
    let mut successful = 0usize;

    for (index, wire) in workload.wires.iter().enumerate() {
        let due = begin + interval.mul_f64(index as f64);
        if let Some(remaining) = due.checked_duration_since(Instant::now()) {
            tokio::time::sleep(remaining).await;
        }
        publisher
            .publish(wire.to_bytes())
            .await
            .with_context(|| format!("publish event {index}"))?;
        successful += 1;
    }

    // Include the final event's interval so count / elapsed time matches the
    // configured room-wide rate rather than ending immediately after it.
    let end = begin + interval.mul_f64(successful as f64);
    if let Some(remaining) = end.checked_duration_since(Instant::now()) {
        tokio::time::sleep(remaining).await;
    }
    Ok(successful)
}

/// Wait until every node holds every id in `expected`, or `deadline` elapses.
async fn wait_until_all_hold(
    cluster: &HarnessCluster,
    expected: &[EventId],
    deadline: Duration,
) -> Result<()> {
    let poll = Duration::from_millis(50);
    let wait = async {
        loop {
            let mut all = true;
            for hn in &cluster.nodes {
                for id in expected {
                    if !hn
                        .node
                        .store_contains(*id)
                        .await
                        .with_context(|| format!("query event {id} on node {}", hn.index))?
                    {
                        all = false;
                        break;
                    }
                }
                if !all {
                    break;
                }
            }
            if all {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(poll).await;
        }
    };

    match tokio::time::timeout(deadline, wait).await {
        Ok(result) => result,
        Err(_) => bail!(
            "timed out after {deadline:?} waiting for {} events on {} nodes",
            expected.len(),
            cluster.nodes.len()
        ),
    }
}

/// Poll all expected ids under one wall-clock budget. A timeout is a measured
/// non-convergence result; store-query failures remain scenario errors.
async fn wait_node_holds_all(node: &Node, expected: &[EventId], budget: Duration) -> Result<bool> {
    let poll = Duration::from_millis(50);
    let wait = async {
        loop {
            let mut all = true;
            for id in expected {
                if !node
                    .store_contains(*id)
                    .await
                    .with_context(|| format!("query event {id} on rebound target"))?
                {
                    all = false;
                    break;
                }
            }
            if all {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(poll).await;
        }
    };

    match tokio::time::timeout(budget, wait).await {
        Ok(result) => result.map(|()| true),
        Err(_) => Ok(false),
    }
}

fn validate_positive_rate(rate: f64, label: &str) -> Result<()> {
    if !rate.is_finite() || rate <= 0.0 {
        bail!("--{label} must be finite and greater than zero, got {rate}");
    }
    if Duration::try_from_secs_f64(1.0 / rate).is_err() {
        bail!("--{label} is outside the supported duration range: {rate}");
    }
    Ok(())
}

fn validate_sweep(start: f64, max: f64, factor: f64) -> Result<()> {
    validate_positive_rate(start, "start")?;
    validate_positive_rate(max, "max")?;
    if max < start {
        bail!("--max must be greater than or equal to --start");
    }
    if !factor.is_finite() || factor <= 1.0 {
        bail!("--factor must be finite and greater than 1, got {factor}");
    }
    Ok(())
}

fn planned_event_count(rate: f64, secs: u64) -> usize {
    (rate * secs as f64).ceil() as usize
}

fn matrix_seed(n: usize, row_index: usize) -> u64 {
    SEED_MATRIX_BASE
        .wrapping_add((n as u64).wrapping_mul(0x1_0000))
        .wrapping_add(row_index as u64)
}

/// Readiness timeout scales with N so larger meshes still have time to come
/// up but a wiring bug fails fast.
fn readiness_timeout(n: usize) -> Duration {
    Duration::from_secs((n as u64).max(5) * 2)
}

fn base_created_at(seed_base: u64) -> u64 {
    1_771_000_000_000 + seed_base
}

/// Render the structured per-run JSON document (spec §7.1).
fn render_run_document(results: &[ScenarioResult]) -> serde_json::Value {
    json!({
        "crate_version": env!("CARGO_PKG_VERSION"),
        "binary": "n40-probe",
        "iroh_version": IROH_VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "net_mode": "loopback",
        "relay": "disabled",
        "caveats": [
            "connect_mode records whether links were formed by full-mesh connect_to or managed gossip seeds",
            "per-node RSS is derived from process RSS / N (D3)",
            "writer/reader task counts are estimated from connected peer entries (risk 3)",
        ],
        "results": results,
    })
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

fn parse_connect_mode(args: &[String]) -> Result<ConnectMode> {
    flag_value(args, "--connect-mode")
        .as_deref()
        .map_or(Ok(ConnectMode::FullMesh), ConnectMode::parse)
}

fn parse_csv_usize(s: &str, label: &str) -> Result<Vec<usize>> {
    s.split(',')
        .map(|t| t.trim().parse::<usize>())
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("--{label} must be a comma-separated list of integers"))
}

fn parse_csv_rates(s: &str) -> Result<Vec<Option<f64>>> {
    s.split(',')
        .map(|t| {
            let t = t.trim();
            if t.eq_ignore_ascii_case("idle") {
                Ok(None)
            } else {
                t.parse::<f64>()
                    .map(Some)
                    .with_context(|| format!("rate token {t:?} is not a number or 'idle'"))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_value_returns_token_after_flag() {
        let args: Vec<String> = ["--n", "40", "--rate", "0.1"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(flag_value(&args, "--n").as_deref(), Some("40"));
        assert_eq!(flag_value(&args, "--rate").as_deref(), Some("0.1"));
    }

    #[test]
    fn flag_value_is_none_for_absent_or_dangling() {
        let args: Vec<String> = ["--n", "40"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(flag_value(&args, "--json").is_none());
        let trailing: Vec<String> = ["--n"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(flag_value(&trailing, "--n").is_none());
    }

    #[test]
    fn parse_connect_mode_defaults_to_full_mesh_and_accepts_gossip() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(parse_connect_mode(&empty).unwrap(), ConnectMode::FullMesh);
        let args = vec!["--connect-mode".to_owned(), "gossip".to_owned()];
        assert_eq!(parse_connect_mode(&args).unwrap(), ConnectMode::Gossip);
        let bad = vec!["--connect-mode".to_owned(), "mesh".to_owned()];
        assert!(parse_connect_mode(&bad).is_err());
    }

    #[test]
    fn has_flag_detects_presence() {
        let args: Vec<String> = ["--json"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(has_flag(&args, "--json"));
        assert!(!has_flag(&args, "--markdown"));
    }

    #[test]
    fn parse_csv_usize_parses_integers() {
        assert_eq!(
            parse_csv_usize("5,10,20,40", "nodes").unwrap(),
            vec![5, 10, 20, 40]
        );
        assert!(parse_csv_usize("5,abc", "nodes").is_err());
    }

    #[test]
    fn parse_csv_rates_parses_idle_and_numbers() {
        let r = parse_csv_rates("idle,0.1,1,5").unwrap();
        assert_eq!(r, vec![None, Some(0.1), Some(1.0), Some(5.0)]);
    }

    #[test]
    fn parse_csv_rates_rejects_non_numeric_non_idle() {
        assert!(parse_csv_rates("0.1,fast").is_err());
    }

    #[test]
    fn sweep_validation_rejects_nonterminating_ranges() {
        assert!(validate_sweep(0.1, 20.0, 2.0).is_ok());
        assert!(validate_sweep(0.0, 20.0, 2.0).is_err());
        assert!(validate_sweep(-0.1, 20.0, 2.0).is_err());
        assert!(validate_sweep(2.0, 1.0, 2.0).is_err());
        assert!(validate_sweep(0.1, 20.0, 1.0).is_err());
        assert!(validate_sweep(0.1, 20.0, 0.0).is_err());
        assert!(validate_sweep(f64::NAN, 20.0, 2.0).is_err());
        assert!(validate_sweep(0.1, 20.0, f64::INFINITY).is_err());
    }

    #[test]
    fn matrix_rows_have_distinct_seed_bases() {
        let mut seeds = std::collections::BTreeSet::new();
        for n in [5, 10, 20, 40] {
            for row in 0..4 {
                assert!(seeds.insert(matrix_seed(n, row)));
            }
        }
        assert_eq!(seeds.len(), 16);
    }

    #[test]
    fn planned_event_count_rounds_partial_events_up() {
        assert_eq!(planned_event_count(0.1, 5), 1);
        assert_eq!(planned_event_count(0.1, 10), 1);
        assert_eq!(planned_event_count(1.0, 5), 5);
        assert_eq!(planned_event_count(5.0, 60), 300);
    }

    #[test]
    fn readiness_timeout_scales_with_n() {
        // `readiness_timeout(n) = max(n, 5) * 2` seconds, so any N below 5
        // still allows a 10-second readiness window (a wiring bug fails
        // fast instead of hanging).
        assert_eq!(readiness_timeout(1), Duration::from_secs(10));
        assert_eq!(readiness_timeout(5), Duration::from_secs(10));
        assert_eq!(readiness_timeout(40), Duration::from_secs(80));
    }

    #[test]
    fn render_run_document_has_required_top_level_fields() {
        let doc = render_run_document(&[]);
        assert!(doc.get("crate_version").is_some());
        assert!(doc.get("binary").is_some());
        assert!(doc.get("iroh_version").is_some());
        assert!(doc.get("net_mode").is_some());
        assert!(doc.get("caveats").is_some());
        assert!(doc.get("results").is_some());
    }

    #[test]
    fn seed_bases_are_distinct() {
        // Two scenarios in one process must never collide on room id.
        assert_ne!(SEED_SELF_CHECK, SEED_MATRIX_BASE);
        assert_ne!(SEED_MATRIX_BASE, SEED_SWEEP_BASE);
        assert_ne!(SEED_SWEEP_BASE, SEED_REBIND);
        assert_ne!(SEED_REBIND, SEED_SELF_CHECK);
    }

    #[test]
    fn default_matrix_shape_matches_issue_145_acceptance() {
        assert_eq!(
            parse_csv_usize(DEFAULT_MATRIX_NODES, "nodes").unwrap(),
            vec![5, 10, 20, 40]
        );
        assert_eq!(
            parse_csv_rates(DEFAULT_MATRIX_RATES).unwrap(),
            vec![None, Some(0.1), Some(1.0), Some(5.0)]
        );
        assert!(USAGE.contains("--connect-mode full-mesh|gossip"));
        assert!(USAGE.contains("--nodes 5,10,20,40"));
        assert!(USAGE.contains("--rates idle,0.1,1,5"));
    }
}
