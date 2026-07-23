//! Metrics sampling and cascade classification (spec §6.6 / §6.7 / D4).
//!
//! Sampling pulls from the same public seams the shipping `Node` exposes for
//! its CLI / observability surface — `peer_states`, `peer_entries`,
//! `outbound_queue_depths`, `counters`, plus the [`crate::RecordingAudit`]'s
//! reconnect/saturation deltas. Task counts are observability / peer-entry
//! estimates (spec §6.6 / risk 3): `dial_loop_tasks` comes from the node handle,
//! while `writer_tasks_est = reader_tasks_est = connected_peers`.
//!
//! # Casts
//!
//! Several computations in this module intentionally cast between `u64` /
//! `usize` / `f64` for per-second rate and ratio computations on
//! measurement-only values that are far below any precision boundary that
//! would matter for the cascade verdict. The relevant pedantic cast lints
//! are suppressed at the module level for that reason.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::fmt::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh_rooms_core::sync::SyncCounters;
use iroh_rooms_net::PeerConnState;

use crate::cluster::HarnessCluster;
use crate::report::{CascadeVerdict, ClusterMetrics, NodeMetrics};
use crate::rss::process_rss_bytes;
use crate::{AuditSnapshot, RecordingAudit};

/// Per-node counter values captured at the beginning of a measurement
/// window. Reported counters are deltas from this baseline, so earlier
/// matrix rows and the room genesis cannot mask delivery loss in a later
/// window.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeCounterBaseline {
    accepted: u64,
    frames_sent: u64,
    parked: u64,
    backfill_requests: u64,
    early_duplicates: u64,
}

/// One five-second D4 sample and the number of events successfully published
/// during that exact sample window.
#[derive(Debug, Clone)]
pub struct CascadeWindow {
    pub metrics: ClusterMetrics,
    pub published_events: usize,
    pub duration_secs: u64,
}

/// Capture the per-node counter baseline for a new scenario or sample window.
///
/// # Errors
///
/// Returns an error when any node pump cannot answer its counter query.
pub async fn counter_baseline(cluster: &HarnessCluster) -> Result<Vec<NodeCounterBaseline>> {
    let mut baselines = Vec::with_capacity(cluster.nodes.len());
    for hn in &cluster.nodes {
        let counters = hn
            .node
            .counters()
            .await
            .with_context(|| format!("read counters for node {}", hn.index))?;
        baselines.push(NodeCounterBaseline {
            accepted: counters.accepted,
            frames_sent: counters.frames_sent,
            parked: counters.parked,
            backfill_requests: counters.backfill_requests,
            early_duplicates: counters.early_duplicates,
        });
    }
    Ok(baselines)
}

/// One per-node metrics snapshot at the end of a measurement window.
///
/// Async because it drains the per-node counters through the pump boundary
/// (`Node::counters` is routed through the single-owner pump).
///
/// # Errors
///
/// Returns an error when the node pump cannot answer its counter query.
pub async fn node_metrics(
    cluster: &HarnessCluster,
    node_index: usize,
    baseline: &NodeCounterBaseline,
) -> Result<NodeMetrics> {
    let hn = &cluster.nodes[node_index];
    let n = cluster.nodes.len();
    let expected = n.saturating_sub(1);

    let states = hn.node.peer_states();
    let connected = states
        .iter()
        .filter(|(_, s)| *s == PeerConnState::Connected)
        .count();

    let queue_depths = hn.node.outbound_queue_depths();
    let bytes_sum: usize = queue_depths.iter().map(|(_, d)| d).sum();
    let bytes_max: usize = queue_depths.iter().map(|(_, d)| *d).max().unwrap_or(0);

    let counters = hn
        .node
        .counters()
        .await
        .with_context(|| format!("read counters for node {node_index}"))?;
    let SyncCounters {
        accepted,
        parked,
        backfill_requests,
        early_duplicates,
        frames_sent,
        ..
    } = counters;

    Ok(NodeMetrics {
        node_index,
        connected_peers: connected,
        expected_peers: expected,
        dial_loop_tasks: hn.node.dial_count(),
        writer_tasks_est: connected,
        reader_tasks_est: connected,
        outbound_queue_bytes_sum: bytes_sum,
        outbound_queue_bytes_max: bytes_max,
        accepted: accepted.saturating_sub(baseline.accepted),
        frames_sent: frames_sent.saturating_sub(baseline.frames_sent),
        parked: parked.saturating_sub(baseline.parked),
        backfill_requests: backfill_requests.saturating_sub(baseline.backfill_requests),
        early_duplicates: early_duplicates.saturating_sub(baseline.early_duplicates),
    })
}

/// Snapshot the entire cluster's metrics at the current instant, including
/// process RSS (`rss_per_node_est = (rss - baseline) / N`) and the reconnect
/// churn derived from `audit` since `baseline` over `window_secs`.
///
/// # Errors
///
/// Returns an error when a node counter query or process RSS sample fails, or
/// when the supplied counter baselines do not match the cluster size.
pub async fn cluster_metrics(
    cluster: &HarnessCluster,
    baseline_rss: u64,
    audit_baseline: &AuditSnapshot,
    counter_baselines: &[NodeCounterBaseline],
    window_secs: u64,
) -> Result<ClusterMetrics> {
    let n = cluster.nodes.len();
    if counter_baselines.len() != n {
        anyhow::bail!(
            "counter baseline length {} does not match cluster size {n}",
            counter_baselines.len()
        );
    }
    let mut node_metrics_vec = Vec::with_capacity(n);
    for (i, baseline) in counter_baselines.iter().enumerate() {
        node_metrics_vec.push(node_metrics(cluster, i, baseline).await?);
    }

    let process_rss = process_rss_bytes().context("sample process RSS")?;
    let per_node_est = if n == 0 {
        0
    } else {
        process_rss.saturating_sub(baseline_rss) / n as u64
    };

    let total_connected: usize = node_metrics_vec.iter().map(|m| m.connected_peers).sum();
    let expected_total: usize = cluster.expected_connected_peer_entries();

    let reconnects = reconnect_events_since(cluster.audit.as_ref(), audit_baseline);
    let reconnects_per_sec = if window_secs == 0 {
        0.0
    } else {
        reconnects as f64 / window_secs as f64
    };

    let queue_saturations = cluster
        .audit
        .queue_saturations()
        .saturating_sub(audit_baseline.saturations) as usize;

    let accepted_vals: Vec<u64> = node_metrics_vec.iter().map(|m| m.accepted).collect();
    let frames_sent_vals: Vec<u64> = node_metrics_vec.iter().map(|m| m.frames_sent).collect();

    let accepted_min = *accepted_vals.iter().min().unwrap_or(&0);
    let accepted_max = *accepted_vals.iter().max().unwrap_or(&0);
    let frames_sent_min = *frames_sent_vals.iter().min().unwrap_or(&0);
    let frames_sent_max = *frames_sent_vals.iter().max().unwrap_or(&0);

    Ok(ClusterMetrics {
        process_rss_bytes: process_rss,
        rss_per_node_est_bytes: per_node_est,
        total_connected_peer_entries: total_connected,
        expected_connected_peer_entries: expected_total,
        reconnects_per_sec,
        queue_saturations,
        accepted_min,
        accepted_max,
        frames_sent_min,
        frames_sent_max,
        task_count_is_estimated: true,
        nodes: node_metrics_vec,
    })
}

/// The number of post-warmup reconnect transitions recorded by `audit` since
/// `baseline` (a `connected` event arriving after the bring-up counts as a
/// reconnect; spec D4 trigger 2). Returns `connected + disconnected` events
/// since baseline so the per-second rate reflects both halves of the churn.
fn reconnect_events_since(audit: &RecordingAudit, baseline: &AuditSnapshot) -> u64 {
    audit.connected_since(baseline) + audit.disconnected_since(baseline)
}

/// Whether the final snapshot has at least 95% connectedness and delivery.
#[must_use]
pub fn recovered_by_end(metrics: &ClusterMetrics, published_events: usize) -> bool {
    connectedness_is_healthy(metrics) && delivery_is_healthy(metrics, published_events)
}

fn connectedness_is_healthy(metrics: &ClusterMetrics) -> bool {
    let expected = metrics.expected_connected_peer_entries;
    expected == 0 || (metrics.total_connected_peer_entries as f64 / expected as f64) >= 0.95
}

fn delivery_is_healthy(metrics: &ClusterMetrics, published_events: usize) -> bool {
    published_events == 0 || (metrics.accepted_min as f64 / published_events as f64) >= 0.95
}

/// Evaluate the cascade verdict (spec §4 D4) from retained five-second load
/// samples rather than a single end-of-run snapshot.
///
/// Triggers:
/// 1. Any `transport.queue.saturated` audit event in any sample.
/// 2. `reconnects_per_sec > 1.0` for two consecutive full five-second samples.
/// 3. Connectedness below 95% for at least ten consecutive sampled seconds.
/// 4. Per-window accepted delivery below 95% for two consecutive full
///    five-second samples and not recovered by the final aggregate snapshot.
#[must_use]
pub fn classify_cascade(windows: &[CascadeWindow], recovered_at_end: bool) -> CascadeVerdict {
    let trigger_queue_saturated = windows.iter().any(|w| w.metrics.queue_saturations > 0);

    let mut reconnect_streak = 0usize;
    let mut low_connected_secs = 0u64;
    let mut low_delivery_streak = 0usize;
    let mut trigger_reconnect_churn = false;
    let mut trigger_low_connectedness = false;
    let mut saw_sustained_low_delivery = false;

    for window in windows {
        let full_sample = window.duration_secs >= 5;

        if full_sample && window.metrics.reconnects_per_sec > 1.0 {
            reconnect_streak += 1;
        } else {
            reconnect_streak = 0;
        }
        trigger_reconnect_churn |= reconnect_streak >= 2;

        if connectedness_is_healthy(&window.metrics) {
            low_connected_secs = 0;
        } else {
            low_connected_secs = low_connected_secs.saturating_add(window.duration_secs);
        }
        trigger_low_connectedness |= low_connected_secs >= 10;

        if full_sample
            && window.published_events > 0
            && !delivery_is_healthy(&window.metrics, window.published_events)
        {
            low_delivery_streak += 1;
        } else {
            low_delivery_streak = 0;
        }
        saw_sustained_low_delivery |= low_delivery_streak >= 2;
    }

    let trigger_low_delivery = saw_sustained_low_delivery && !recovered_at_end;

    let mut note = String::new();
    if trigger_queue_saturated {
        let count: usize = windows.iter().map(|w| w.metrics.queue_saturations).sum();
        let _ = write!(note, "queue_saturated={count} ");
    }
    if trigger_reconnect_churn {
        let max_rate = windows
            .iter()
            .map(|w| w.metrics.reconnects_per_sec)
            .fold(0.0, f64::max);
        let _ = write!(note, "reconnects_max={max_rate:.2}/s ");
    }
    if trigger_low_connectedness {
        let _ = write!(note, "connectedness_below_95pct_for_10s ");
    }
    if trigger_low_delivery {
        let _ = write!(note, "delivery_below_95pct_for_2_windows ");
    }

    CascadeVerdict::from_triggers(
        trigger_queue_saturated,
        trigger_reconnect_churn,
        trigger_low_connectedness,
        trigger_low_delivery,
        note,
    )
}

/// Convenience helper that sleeps for `secs` and returns the elapsed
/// `Duration`. Used by the harness to pace an idle / load window.
pub async fn run_window(secs: u64) -> Duration {
    let start = std::time::Instant::now();
    if secs > 0 {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::ClusterMetrics;

    fn sample_metrics(
        connected: usize,
        expected: usize,
        reconnects_per_sec: f64,
        queue_saturations: usize,
        accepted_min: u64,
    ) -> ClusterMetrics {
        ClusterMetrics {
            process_rss_bytes: 0,
            rss_per_node_est_bytes: 0,
            total_connected_peer_entries: connected,
            expected_connected_peer_entries: expected,
            reconnects_per_sec,
            queue_saturations,
            accepted_min,
            accepted_max: accepted_min,
            frames_sent_min: 0,
            frames_sent_max: 0,
            task_count_is_estimated: true,
            nodes: Vec::new(),
        }
    }

    fn window(metrics: ClusterMetrics, published_events: usize) -> CascadeWindow {
        CascadeWindow {
            metrics,
            published_events,
            duration_secs: 5,
        }
    }

    #[test]
    fn classify_no_cascade_when_everything_is_healthy() {
        let windows = vec![
            window(sample_metrics(1560, 1560, 0.0, 0, 10), 10),
            window(sample_metrics(1560, 1560, 0.0, 0, 10), 10),
        ];
        let v = classify_cascade(&windows, true);
        assert!(!v.began);
    }

    #[test]
    fn classify_cascade_on_queue_saturation() {
        let windows = vec![window(sample_metrics(1560, 1560, 0.0, 5, 10), 10)];
        let v = classify_cascade(&windows, true);
        assert!(v.trigger_queue_saturated);
        assert!(v.began);
        assert!(v.note.contains("queue_saturated=5"));
    }

    #[test]
    fn reconnect_churn_requires_two_consecutive_full_windows() {
        let one_high = vec![window(sample_metrics(1560, 1560, 2.5, 0, 10), 10)];
        assert!(!classify_cascade(&one_high, true).trigger_reconnect_churn);

        let two_high = vec![
            window(sample_metrics(1560, 1560, 2.5, 0, 10), 10),
            window(sample_metrics(1560, 1560, 1.1, 0, 10), 10),
        ];
        let v = classify_cascade(&two_high, true);
        assert!(v.trigger_reconnect_churn);
        assert!(v.began);
        assert!(v.note.contains("reconnects_max=2.50/s"));
    }

    #[test]
    fn low_connectedness_requires_ten_consecutive_seconds() {
        // 1400/1560 ≈ 0.897 < 0.95
        let low = window(sample_metrics(1400, 1560, 0.0, 0, 10), 10);
        assert!(!classify_cascade(std::slice::from_ref(&low), true).trigger_low_connectedness);
        let v = classify_cascade(&[low.clone(), low], true);
        assert!(v.trigger_low_connectedness);
        assert!(v.began);
    }

    #[test]
    fn classify_no_low_connectedness_when_at_threshold() {
        // Exactly 0.95 of 1560 = 1482.0 → at-threshold must NOT trigger.
        let windows = vec![
            window(sample_metrics(1482, 1560, 0.0, 0, 10), 10),
            window(sample_metrics(1482, 1560, 0.0, 0, 10), 10),
        ];
        let v = classify_cascade(&windows, true);
        assert!(!v.trigger_low_connectedness);
    }

    #[test]
    fn low_delivery_requires_two_windows_and_no_final_recovery() {
        let windows = vec![
            window(sample_metrics(1560, 1560, 0.0, 0, 5), 10),
            window(sample_metrics(1560, 1560, 0.0, 0, 4), 10),
        ];
        assert!(!classify_cascade(&windows, true).trigger_low_delivery);
        let v = classify_cascade(&windows, false);
        assert!(v.trigger_low_delivery);
        assert!(v.began);
    }

    #[test]
    fn classify_no_low_delivery_when_zero_published() {
        let windows = vec![
            window(sample_metrics(1560, 1560, 0.0, 0, 0), 0),
            window(sample_metrics(1560, 1560, 0.0, 0, 0), 0),
        ];
        let v = classify_cascade(&windows, false);
        assert!(!v.trigger_low_delivery);
    }

    #[test]
    fn partial_window_does_not_complete_a_two_window_streak() {
        let mut partial = window(sample_metrics(1560, 1560, 2.5, 0, 1), 10);
        partial.duration_secs = 4;
        let full = window(sample_metrics(1560, 1560, 2.5, 0, 1), 10);
        let v = classify_cascade(&[partial, full], false);
        assert!(!v.trigger_reconnect_churn);
        assert!(!v.trigger_low_delivery);
    }

    #[test]
    fn recovery_requires_both_connectedness_and_delivery() {
        assert!(recovered_by_end(&sample_metrics(95, 100, 0.0, 0, 10), 10));
        assert!(!recovered_by_end(&sample_metrics(94, 100, 0.0, 0, 10), 10));
        assert!(!recovered_by_end(&sample_metrics(100, 100, 0.0, 0, 9), 10));
    }

    #[test]
    fn reconnect_events_since_counts_both_halves() {
        use iroh_rooms_net::audit::AuditSink as _;
        fn dev(seed: u8) -> iroh::EndpointId {
            iroh::SecretKey::from_bytes(&[seed; 32]).public()
        }
        let audit = RecordingAudit::new();
        let baseline = audit.snapshot();
        audit.connected(dev(1));
        audit.disconnected(dev(2));
        audit.disconnected(dev(3));
        assert_eq!(
            reconnect_events_since(&audit, &baseline),
            3,
            "1 connected + 2 disconnected = 3 churn events since baseline"
        );
    }
}
