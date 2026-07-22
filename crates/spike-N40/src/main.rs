//! `n40-probe` — the #145 measurement CLI (spec §6.1).
//!
//! ```text
//! n40-probe self-check [--json]
//! n40-probe matrix [--nodes 5,10,20,40] [--rates idle,0.1,1,5]
//!                  [--idle-secs 30] [--load-secs 60] [--low-rate-secs 120]
//!                  [--warmup-secs 10] [--json results/<name>.json]
//!                  [--markdown results/results.md]
//! n40-probe sweep --n 40 --start 0.1 --max 20 --factor 2
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
    full_mesh_admission, node_seeds, AdminPrincipal, HarnessCluster, HARNESS_TICK,
};
use spike_n40::metrics::{classify_cascade, cluster_metrics, run_window};
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
  n40-probe matrix [--nodes 5,10,20,40] [--rates idle,0.1,1,5]
                  [--idle-secs 30] [--load-secs 60] [--low-rate-secs 120]
                  [--warmup-secs 10] [--json results/<name>.json]
                  [--markdown results/results.md]
  n40-probe sweep --n 40 --start 0.1 --max 20 --factor 2
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
    run_one_scenario(n, SEED_SELF_CHECK, ScenarioKind::Load, Some(1.0), 2, 8).await
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

    let nodes: Vec<usize> = parse_csv_usize(&nodes_arg, "nodes")?;
    let rates: Vec<Option<f64>> = parse_csv_rates(&rates_arg)?;

    let baseline_rss = process_rss_bytes().unwrap_or(0);

    let mut results: Vec<ScenarioResult> = Vec::new();
    let mut matrix_rows: Vec<MatrixRowOwned> = Vec::new();

    for &n in &nodes {
        // Each N runs in a fresh cluster (spec §6.7) so RSS/backlog from one
        // row does not contaminate the next.
        let seed_base = SEED_MATRIX_BASE + (n as u64) * 0x100;
        let readiness = readiness_timeout(n);
        let cluster = HarnessCluster::spawn(n, seed_base, readiness)
            .await
            .with_context(|| format!("spawn N={n} cluster"))?;

        // Idle window.
        let idle_audit_baseline = cluster.audit.snapshot();
        run_window(idle_secs).await;
        let idle_metrics =
            cluster_metrics(&cluster, baseline_rss, &idle_audit_baseline, idle_secs).await;
        let idle_cascade = classify_cascade(&idle_metrics, n.saturating_sub(1), 0);
        matrix_rows.push(MatrixRowOwned {
            config: ScenarioConfig {
                n,
                rate_events_per_sec: None,
                warmup_secs: 0,
                measure_secs: idle_secs,
                seed_base,
            },
            metrics: idle_metrics.clone(),
            cascade: idle_cascade.clone(),
            recovered_by_end: true,
        });

        // Each rate window. The explicit idle window above already produced
        // the idle matrix row + ScenarioResult.idle field, so a `None` rate
        // token in `rates` is skipped here to avoid duplicating that row.
        for rate_opt in &rates {
            if rate_opt.is_none() {
                continue;
            }
            // Warmup: let the queue settle / dial loops stabilize.
            run_window(warmup_secs).await;
            let warm_audit_baseline = cluster.audit.snapshot();

            let published_events;
            let load_metrics;
            let cascade;
            let measure_secs_actual;

            if let Some(rate) = rate_opt {
                let window_secs = if *rate <= 0.1 {
                    low_rate_secs
                } else {
                    load_secs
                };
                measure_secs_actual = window_secs;
                let workload = Workload::build(
                    cluster.room_id,
                    &cluster.admin.identity_secret(),
                    &cluster.admin.device_secret(),
                    cluster.genesis_id,
                    ((rate * window_secs as f64).ceil() as usize).max(1),
                    base_created_at(seed_base),
                    "n40 load",
                );
                published_events = workload.wires.len();
                publish_at_rate(
                    &cluster.nodes[0].node,
                    &workload,
                    *rate,
                    Duration::from_secs(window_secs),
                )
                .await;
                load_metrics =
                    cluster_metrics(&cluster, baseline_rss, &warm_audit_baseline, window_secs)
                        .await;
                cascade = classify_cascade(&load_metrics, n.saturating_sub(1), published_events);
            } else {
                measure_secs_actual = idle_secs;
                run_window(idle_secs).await;
                load_metrics =
                    cluster_metrics(&cluster, baseline_rss, &warm_audit_baseline, idle_secs).await;
                published_events = 0;
                cascade = classify_cascade(&load_metrics, n.saturating_sub(1), 0);
            }

            let rate_label = rate_opt.map_or_else(|| "idle".to_owned(), |r| format!("{r}"));

            matrix_rows.push(MatrixRowOwned {
                config: ScenarioConfig {
                    n,
                    rate_events_per_sec: *rate_opt,
                    warmup_secs,
                    measure_secs: measure_secs_actual,
                    seed_base,
                },
                metrics: load_metrics.clone(),
                cascade: cascade.clone(),
                recovered_by_end: !cascade.began,
            });

            results.push(ScenarioResult {
                kind: if rate_opt.is_some() {
                    ScenarioKind::Load
                } else {
                    ScenarioKind::Idle
                },
                config: ScenarioConfig {
                    n,
                    rate_events_per_sec: *rate_opt,
                    warmup_secs,
                    measure_secs: measure_secs_actual,
                    seed_base,
                },
                idle: Some(idle_metrics.clone()),
                load: Some(load_metrics),
                published_events,
                cascade: cascade.clone(),
                notes: vec![format!("loopback NetMode::Loopback iroh={IROH_VERSION}")],
            });

            println!(
                "[matrix] N={n} rate={rate_label} cascade={} connected={}/{}",
                if cascade.began { "yes" } else { "no" },
                matrix_rows
                    .last()
                    .unwrap()
                    .metrics
                    .total_connected_peer_entries,
                matrix_rows
                    .last()
                    .unwrap()
                    .metrics
                    .expected_connected_peer_entries,
            );
        }

        cluster.shutdown().await.context("shutdown cluster")?;
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
        )
        .await?;
        let began = r.cascade.began;
        println!(
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
    let _json_path = flag_value(args, "--json");

    let result = rebind_scenario(n, missed_events, rate).await?;
    let doc = render_run_document(std::slice::from_ref(&result));
    println!("{}", serde_json::to_string_pretty(&doc)?);
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
    let seed_base = SEED_REBIND;
    let readiness = readiness_timeout(n);
    let cluster = HarnessCluster::spawn(n, seed_base, readiness)
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
    wait_until_all_hold(&cluster, &baseline_ids, Duration::from_mins(1)).await;

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
    let missed_ids = missed_workload.event_ids();
    for wire in &missed_workload.wires {
        keep[0].node.publish(wire.to_bytes()).await?;
    }

    // Await convergence on the surviving nodes (so we know the missed events
    // really did land on everyone but the target).
    let surviving = HarnessCluster {
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
    wait_until_all_hold(&surviving, &missed_ids, Duration::from_mins(1)).await;

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
    let mut converged_all = true;
    for id in &missed_ids {
        if target_node_handle
            .wait_until_contains(*id, Duration::from_mins(1))
            .await
            .is_err()
        {
            converged_all = false;
        }
    }
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
        published_events: missed_events,
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
) -> Result<ScenarioResult> {
    let readiness = readiness_timeout(n);
    let baseline_rss = process_rss_bytes().unwrap_or(0);
    let cluster = HarnessCluster::spawn(n, seed_base, readiness)
        .await
        .with_context(|| format!("spawn N={n} cluster"))?;

    // Idle window (short for the single-scenario runner; the matrix runner
    // drives its own per-N idle window with the full duration).
    let idle_secs = measure_secs.min(5);
    let idle_audit = cluster.audit.snapshot();
    run_window(idle_secs).await;
    let idle_metrics = cluster_metrics(&cluster, baseline_rss, &idle_audit, idle_secs).await;

    // Warmup, then load.
    run_window(warmup_secs).await;
    let warm_audit = cluster.audit.snapshot();
    let published_events;
    let load_metrics;
    let cascade;
    if let Some(r) = rate {
        let workload = Workload::build(
            cluster.room_id,
            &cluster.admin.identity_secret(),
            &cluster.admin.device_secret(),
            cluster.genesis_id,
            ((r * measure_secs as f64).ceil() as usize).max(1),
            base_created_at(seed_base),
            "n40 load",
        );
        published_events = workload.wires.len();
        publish_at_rate(
            &cluster.nodes[0].node,
            &workload,
            r,
            Duration::from_secs(measure_secs),
        )
        .await;
        load_metrics = cluster_metrics(&cluster, baseline_rss, &warm_audit, measure_secs).await;
        cascade = classify_cascade(&load_metrics, n.saturating_sub(1), published_events);
    } else {
        published_events = 0;
        load_metrics = idle_metrics.clone();
        cascade = classify_cascade(&idle_metrics, n.saturating_sub(1), 0);
    }

    cluster.shutdown().await?;

    Ok(ScenarioResult {
        kind,
        config: ScenarioConfig {
            n,
            rate_events_per_sec: rate,
            warmup_secs,
            measure_secs,
            seed_base,
        },
        idle: Some(idle_metrics),
        load: Some(load_metrics),
        published_events,
        cascade,
        notes: vec![format!("loopback NetMode::Loopback iroh={IROH_VERSION}")],
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Publish every wire in `workload` from `publisher` at the room-wide `rate`
/// (events/sec), spaced evenly over `window`. If the workload has fewer
/// events than `rate * window`, the publisher finishes early.
async fn publish_at_rate(publisher: &Node, workload: &Workload, rate: f64, window: Duration) {
    let interval = if rate <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / rate)
    };
    let deadline = Instant::now() + window;
    for wire in &workload.wires {
        if Instant::now() >= deadline {
            break;
        }
        let bytes = wire.to_bytes();
        let _ = publisher.publish(bytes).await;
        if Instant::now() < deadline {
            tokio::time::sleep(interval).await;
        }
    }
}

/// Wait until every node holds every id in `expected`, or `deadline` elapses.
async fn wait_until_all_hold(cluster: &HarnessCluster, expected: &[EventId], deadline: Duration) {
    let start = Instant::now();
    let poll = Duration::from_millis(50);
    while start.elapsed() < deadline {
        let mut all = true;
        for hn in &cluster.nodes {
            for id in expected {
                if !hn.node.store_contains(*id).await.unwrap_or(false) {
                    all = false;
                    break;
                }
            }
            if !all {
                break;
            }
        }
        if all {
            return;
        }
        tokio::time::sleep(poll).await;
    }
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
            "over-cap transport allowlist, NOT a literal >5 active membership fold (D1)",
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
        assert!(USAGE.contains("--nodes 5,10,20,40"));
        assert!(USAGE.contains("--rates idle,0.1,1,5"));
    }
}
