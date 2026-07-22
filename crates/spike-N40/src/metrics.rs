//! Metrics sampling and cascade classification (spec §6.6 / §6.7 / D4).
//!
//! Sampling pulls from the same public seams the shipping `Node` exposes for
//! its CLI / observability surface — `peer_states`, `peer_entries`,
//! `outbound_queue_depths`, `counters`, plus the [`crate::RecordingAudit`]'s
//! reconnect/saturation deltas. Task counts are by-construction / peer-entry
//! estimates (spec §6.6 / risk 3): `dial_loop_tasks = N - 1` by construction,
//! `writer_tasks_est = reader_tasks_est = connected_peers`.
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

use iroh_rooms_core::sync::SyncCounters;
use iroh_rooms_net::PeerConnState;

use crate::cluster::HarnessCluster;
use crate::report::{CascadeVerdict, ClusterMetrics, NodeMetrics};
use crate::rss::process_rss_bytes;
use crate::{AuditSnapshot, RecordingAudit};

/// One per-node metrics snapshot at the end of a measurement window.
///
/// Async because it drains the per-node counters through the pump boundary
/// (`Node::counters` is routed through the single-owner pump).
pub async fn node_metrics(cluster: &HarnessCluster, node_index: usize) -> NodeMetrics {
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

    let counters = hn.node.counters().await.unwrap_or_default();
    let SyncCounters {
        accepted,
        parked,
        backfill_requests,
        early_duplicates,
        frames_sent,
        ..
    } = counters;

    NodeMetrics {
        node_index,
        connected_peers: connected,
        expected_peers: expected,
        // By construction: this node dials every other node (D2), so the dial
        // loop count is exactly N - 1 whether or not every loop has connected.
        dial_loop_tasks: expected,
        writer_tasks_est: connected,
        reader_tasks_est: connected,
        outbound_queue_bytes_sum: bytes_sum,
        outbound_queue_bytes_max: bytes_max,
        accepted,
        frames_sent,
        parked,
        backfill_requests,
        early_duplicates,
    }
}

/// Snapshot the entire cluster's metrics at the current instant, including
/// process RSS (`rss_per_node_est = (rss - baseline) / N`) and the reconnect
/// churn derived from `audit` since `baseline` over `window_secs`.
pub async fn cluster_metrics(
    cluster: &HarnessCluster,
    baseline_rss: u64,
    audit_baseline: &AuditSnapshot,
    window_secs: u64,
) -> ClusterMetrics {
    let n = cluster.nodes.len();
    let mut node_metrics_vec = Vec::with_capacity(n);
    for i in 0..n {
        node_metrics_vec.push(node_metrics(cluster, i).await);
    }

    let process_rss = process_rss_bytes().unwrap_or(0);
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

    ClusterMetrics {
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
    }
}

/// The number of post-warmup reconnect transitions recorded by `audit` since
/// `baseline` (a `connected` event arriving after the bring-up counts as a
/// reconnect; spec D4 trigger 2). Returns `connected + disconnected` events
/// since baseline so the per-second rate reflects both halves of the churn.
fn reconnect_events_since(audit: &RecordingAudit, baseline: &AuditSnapshot) -> u64 {
    audit.connected_since(baseline) + audit.disconnected_since(baseline)
}

/// Evaluate the cascade verdict (spec §4 D4) for one load window.
///
/// Triggers:
/// 1. `transport.queue.saturated` audit events this window > 0.
/// 2. `reconnects_per_sec > 1.0` sustained (the caller already collapsed the
///    window to a per-second rate; we check the threshold here).
/// 3. Connectedness below 95% of expected.
/// 4. Accepted delivery below 95% of `expected_recipients * published_events`.
#[must_use]
pub fn classify_cascade(
    metrics: &ClusterMetrics,
    expected_recipients_per_event: usize,
    published_events: usize,
) -> CascadeVerdict {
    // D4.1: any queue saturation event this window.
    let trigger_queue_saturated = metrics.queue_saturations > 0;

    // D4.2: reconnect churn > 1.0/sec for the steady window.
    let trigger_reconnect_churn = metrics.reconnects_per_sec > 1.0;

    // D4.3: connectedness < 95% of expected.
    let expected = metrics.expected_connected_peer_entries;
    let trigger_low_connectedness = if expected == 0 {
        false
    } else {
        let ratio = metrics.total_connected_peer_entries as f64 / expected as f64;
        ratio < 0.95
    };

    // D4.4: accepted delivery below 95% of expected (every event should land
    // in `expected_recipients_per_event` stores — every node but the
    // publisher).
    let trigger_low_delivery = if published_events == 0 {
        false
    } else {
        let expected_accepted = expected_recipients_per_event.saturating_mul(published_events);
        if expected_accepted == 0 {
            false
        } else {
            let actual_min_total = metrics.accepted_min;
            // `accepted_min` is per-node; the worst-case node should still
            // have received close to `published_events` events (the admin's
            // own store has them by construction; remote nodes received them
            // through the mesh). Use the min across nodes vs the per-node
            // expected count.
            let per_node_expected = published_events;
            (actual_min_total as f64 / per_node_expected as f64) < 0.95
        }
    };

    let mut note = String::new();
    if trigger_queue_saturated {
        let _ = write!(note, "queue_saturated={} ", metrics.queue_saturations);
    }
    if trigger_reconnect_churn {
        let _ = write!(note, "reconnects={:.2}/s ", metrics.reconnects_per_sec);
    }
    if trigger_low_connectedness {
        let _ = write!(
            note,
            "connected={}/{} ",
            metrics.total_connected_peer_entries, metrics.expected_connected_peer_entries
        );
    }
    if trigger_low_delivery {
        let _ = write!(
            note,
            "accepted_min={} expected_per_node={} ",
            metrics.accepted_min, published_events
        );
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

    #[test]
    fn classify_no_cascade_when_everything_is_healthy() {
        let m = sample_metrics(1560, 1560, 0.0, 0, 10);
        let v = classify_cascade(&m, 39, 10);
        assert!(!v.began);
    }

    #[test]
    fn classify_cascade_on_queue_saturation() {
        let m = sample_metrics(1560, 1560, 0.0, 5, 10);
        let v = classify_cascade(&m, 39, 10);
        assert!(v.trigger_queue_saturated);
        assert!(v.began);
        assert!(v.note.contains("queue_saturated=5"));
    }

    #[test]
    fn classify_cascade_on_reconnect_churn() {
        let m = sample_metrics(1560, 1560, 2.5, 0, 10);
        let v = classify_cascade(&m, 39, 10);
        assert!(v.trigger_reconnect_churn);
        assert!(v.began);
        assert!(v.note.contains("reconnects=2.50/s"));
    }

    #[test]
    fn classify_cascade_on_low_connectedness() {
        // 1400/1560 ≈ 0.897 < 0.95
        let m = sample_metrics(1400, 1560, 0.0, 0, 10);
        let v = classify_cascade(&m, 39, 10);
        assert!(v.trigger_low_connectedness);
        assert!(v.began);
    }

    #[test]
    fn classify_no_low_connectedness_when_at_threshold() {
        // Exactly 0.95 of 1560 = 1482.0 → at-threshold must NOT trigger.
        let m = sample_metrics(1482, 1560, 0.0, 0, 10);
        let v = classify_cascade(&m, 39, 10);
        assert!(!v.trigger_low_connectedness);
    }

    #[test]
    fn classify_cascade_on_low_delivery() {
        // 10 published events, min accepted is 5 → 0.5 < 0.95 ⇒ cascade.
        let m = sample_metrics(1560, 1560, 0.0, 0, 5);
        let v = classify_cascade(&m, 39, 10);
        assert!(v.trigger_low_delivery);
        assert!(v.began);
    }

    #[test]
    fn classify_no_low_delivery_when_zero_published() {
        let m = sample_metrics(1560, 1560, 0.0, 0, 0);
        let v = classify_cascade(&m, 39, 0);
        assert!(!v.trigger_low_delivery);
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
