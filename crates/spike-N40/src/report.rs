//! The structured, machine-readable measurement record (spec §6.2 / §7).
//!
//! [`ScenarioResult`] is the measurement contract: one (N × rate × mode)
//! scenario emits exactly one of these, serialized to one JSON object so a
//! re-run is directly diffable. [`results_md`] renders the rolled-up matrix
//! table that drops verbatim into `NOTES.md` and `results/results.md`.

use serde::{Deserialize, Serialize};

/// Which scenario produced a [`ScenarioResult`] (spec §6.2). Serialized as a
/// lowercase label matching the CLI subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScenarioKind {
    /// Quiescent observation window after readiness (no publishes).
    Idle,
    /// Steady load at a fixed room-wide event rate.
    Load,
    /// Threshold-bracketing sweep (optional).
    Sweep,
    /// Rebind / NAT-drop convergence probe at N=40.
    Rebind,
}

impl ScenarioKind {
    /// The lowercase label used in the results table and JSON.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Load => "load",
            Self::Sweep => "sweep",
            Self::Rebind => "rebind",
        }
    }
}

/// The (N × rate × window) configuration for one scenario run (spec §6.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioConfig {
    /// Number of in-process loopback transport nodes (over-cap; NOT a
    /// product-supported active-member room — see `NOTES.md` caveat).
    pub n: usize,
    /// Room-wide publish rate in events/sec (`None` for idle / rebind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_events_per_sec: Option<f64>,
    /// Pre-measurement warmup window (excluded from reconnect/sec).
    pub warmup_secs: u64,
    /// Measurement window length.
    pub measure_secs: u64,
    /// The deterministic seed base this scenario's principals were derived
    /// from (so a re-run is byte-reproducible).
    pub seed_base: u64,
}

/// One node's metrics snapshot at the end of a scenario window (spec §6.2 /
/// §6.6). `task_count_is_estimated` (held at the cluster level) flags that
/// `dial_loop_tasks` / `writer_tasks_est` / `reader_tasks_est` are by-
/// construction / peer-entry estimates, not measured Tokio task handles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    /// Index of this node in the cluster.
    pub node_index: usize,
    /// Connected peer count this node reports.
    pub connected_peers: usize,
    /// Expected peer count (`N - 1`).
    pub expected_peers: usize,
    /// Dial loops this node is running. By construction = `N - 1` for the
    /// full mesh (D2).
    pub dial_loop_tasks: usize,
    /// Estimated writer tasks (= live connected peers; one writer per link).
    pub writer_tasks_est: usize,
    /// Estimated reader tasks (= live connected peers; one reader per link).
    pub reader_tasks_est: usize,
    /// Sum of per-peer outbound queue depths (bytes) on this node.
    pub outbound_queue_bytes_sum: usize,
    /// Max per-peer outbound queue depth (bytes) on this node.
    pub outbound_queue_bytes_max: usize,
    /// Engine `accepted` counter for this node.
    pub accepted: u64,
    /// Engine `frames_sent` counter for this node.
    pub frames_sent: u64,
    /// Engine `parked` counter for this node.
    pub parked: u64,
    /// Engine `backfill_requests` counter for this node.
    pub backfill_requests: u64,
    /// Engine `early_duplicates` counter for this node.
    pub early_duplicates: u64,
}

/// The whole-cluster rollup for one sample (spec §6.2 / §6.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMetrics {
    /// Total process RSS at this sample, in bytes.
    pub process_rss_bytes: u64,
    /// Derived per-node RSS estimate: `(process_rss - baseline_rss) / N`.
    pub rss_per_node_est_bytes: u64,
    /// Sum of every node's `connected_peers` (directed count; one entry per
    /// live link, counted from each side).
    pub total_connected_peer_entries: usize,
    /// Expected directed peer entries (`N * (N - 1)`).
    pub expected_connected_peer_entries: usize,
    /// Reconnect churn per second, computed over the load window AFTER
    /// warmup (D4 trigger 2).
    pub reconnects_per_sec: f64,
    /// Cluster-wide count of `transport.queue.saturated` audit events.
    pub queue_saturations: usize,
    /// Minimum per-node `accepted` across the cluster.
    pub accepted_min: u64,
    /// Maximum per-node `accepted` across the cluster.
    pub accepted_max: u64,
    /// Minimum per-node `frames_sent` across the cluster.
    pub frames_sent_min: u64,
    /// Maximum per-node `frames_sent` across the cluster.
    pub frames_sent_max: u64,
    /// True when `dial_loop_tasks` / `writer_tasks_est` / `reader_tasks_est`
    /// are by-construction or peer-entry estimates rather than exact task
    /// handle counts (always true for this spike — spec §6.6 / risk 3).
    pub task_count_is_estimated: bool,
    /// Per-node metrics, index-aligned with the cluster.
    pub nodes: Vec<NodeMetrics>,
}

/// The cascade-start verdict for one (N × rate) load run (spec §4 D4). A
/// cascade is deemed to have begun when ANY of the four triggers fires during
/// the steady load window; `began` is the disjunction.
///
/// The five booleans are the spec contract (D4 trigger 1-4 plus the
/// disjunctive `began`); they are kept as plain fields rather than a bitflag
/// so the JSON artifact is self-describing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct CascadeVerdict {
    /// D4.1: at least one `transport.queue.saturated` audit event occurred.
    pub trigger_queue_saturated: bool,
    /// D4.2: reconnect churn > 1.0/sec for >=2 consecutive 5s windows.
    pub trigger_reconnect_churn: bool,
    /// D4.3: connectedness stayed below 95% of expected for >=10s.
    pub trigger_low_connectedness: bool,
    /// D4.4: accepted delivery < 95% of expected for >=2 windows and did not
    /// recover by end of run.
    pub trigger_low_delivery: bool,
    /// Disjunction of the four triggers — the cascade began this run.
    pub began: bool,
    /// Free-text note the harness may attach (which trigger, sample windows,
    /// partial-recovery evidence).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl CascadeVerdict {
    /// Build the verdict from individual trigger booleans. `began` is the
    /// disjunction; an empty trigger set yields `began == false`.
    ///
    /// The five booleans are the spec contract; suppressing the lint rather
    /// than collapsing to a struct keeps the JSON artifact readable.
    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn from_triggers(
        queue_saturated: bool,
        reconnect_churn: bool,
        low_connectedness: bool,
        low_delivery: bool,
        note: String,
    ) -> Self {
        let began = queue_saturated || reconnect_churn || low_connectedness || low_delivery;
        Self {
            trigger_queue_saturated: queue_saturated,
            trigger_reconnect_churn: reconnect_churn,
            trigger_low_connectedness: low_connectedness,
            trigger_low_delivery: low_delivery,
            began,
            note,
        }
    }
}

/// One scenario run's full record (spec §6.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Which scenario produced this result.
    pub kind: ScenarioKind,
    /// The scenario configuration.
    pub config: ScenarioConfig,
    /// Idle-window metrics (present for `Idle` / `Load`; absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle: Option<ClusterMetrics>,
    /// Load-window metrics (present for `Load` / `Sweep`; absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<ClusterMetrics>,
    /// Number of events published by the admin (node 0) this scenario.
    pub published_events: usize,
    /// The cascade verdict for this run (always present; `began == false`
    /// for runs that did not cross the threshold).
    pub cascade: CascadeVerdict,
    /// Free-text notes / caveats the harness attaches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The "survives?" label for one matrix row (spec §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Survives {
    /// No cascade triggers, expected connectedness reached/restored,
    /// delivery converged.
    Yes,
    /// Transient reconnect/saturation occurred but delivery recovered by end.
    Degraded,
    /// Cascade trigger persisted, delivery did not recover, or run timed out.
    No,
}

impl Survives {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::Degraded => "degraded",
            Self::No => "no",
        }
    }

    /// Classify the `survives?` outcome from a cascade verdict plus a
    /// connectedness / delivery boolean (spec §7.2).
    ///
    /// - `cascade.began && !recovered_by_end` ⇒ `No`
    /// - `cascade.began && recovered_by_end` ⇒ `Degraded`
    /// - `!cascade.began` ⇒ `Yes`
    #[must_use]
    pub fn classify(cascade: &CascadeVerdict, recovered_by_end: bool) -> Self {
        if !cascade.began {
            Self::Yes
        } else if recovered_by_end {
            Self::Degraded
        } else {
            Self::No
        }
    }
}

/// Render a single matrix row from a (config, load-or-idle metrics, verdict)
/// triple (spec §7.2). The `survives?` cell is derived from `cascade` and
/// `recovered_by_end`.
///
/// Columns:
/// `N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB |
///  dial loops/node | writer+reader tasks/node est | connected entries |
///  accepted min/max | frames_sent min/max | queue saturations |
///  reconnects/sec | cascade?`
#[must_use]
pub fn matrix_row(
    config: &ScenarioConfig,
    metrics: &ClusterMetrics,
    cascade: &CascadeVerdict,
    recovered_by_end: bool,
) -> String {
    let n = config.n;
    let rate = config
        .rate_events_per_sec
        .map_or_else(|| "idle".to_owned(), |r| format!("{r}"));
    let mode = if config.rate_events_per_sec.is_some() {
        "load"
    } else {
        "idle"
    };
    let survives = Survives::classify(cascade, recovered_by_end).label();
    let rss_total_mib = bytes_to_mib(metrics.process_rss_bytes);
    let rss_per_node_mib = bytes_to_mib(metrics.rss_per_node_est_bytes);
    // Per-node averages — every node has the same expected `N - 1` dial loops
    // by construction, so the mean is exact; writer/reader are estimated.
    let dial_loops_per_node = (metrics
        .nodes
        .iter()
        .map(|m| m.dial_loop_tasks)
        .sum::<usize>())
    .checked_div(n)
    .unwrap_or(0);
    let writer_reader_per_node = (metrics
        .nodes
        .iter()
        .map(|m| m.writer_tasks_est + m.reader_tasks_est)
        .sum::<usize>())
    .checked_div(n)
    .unwrap_or(0);
    let cascade_label = if cascade.began { "yes" } else { "no" };
    format!(
        "| {n} | {rate} | {mode} | {survives} | {rss_total_mib} | {rss_per_node_mib} | \
         {dial_loops_per_node} | {writer_reader_per_node} | \
         {connected}/{expected} | {acc_min}/{acc_max} | {fs_min}/{fs_max} | \
         {saturations} | {reconnects:.2} | {cascade_label} |",
        connected = metrics.total_connected_peer_entries,
        expected = metrics.expected_connected_peer_entries,
        acc_min = metrics.accepted_min,
        acc_max = metrics.accepted_max,
        fs_min = metrics.frames_sent_min,
        fs_max = metrics.frames_sent_max,
        saturations = metrics.queue_saturations,
        reconnects = metrics.reconnects_per_sec,
    )
}

/// Render the full matrix `results.md` table from a slice of
/// `(ScenarioResult, recovered_by_end, is_load)` rows (spec §7.2).
///
/// Each input row contributes exactly one matrix row. `is_load == true`
/// prefers the scenario's `load` metrics; `false` prefers `idle`.
#[must_use]
pub fn results_md(rows: &[MatrixRow]) -> String {
    let mut out = String::new();
    out.push_str("# `spike-N40` matrix results\n\n");
    out.push_str(
        "Rendered from `n40-probe matrix` (loopback `NetMode::Loopback`, no relay/discovery). \
         Regenerate with the command documented in `crates/spike-N40/results/README.md`.\n\n",
    );
    out.push_str(
        "| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | \
         dial loops/node | writer+reader tasks/node est | connected entries | \
         accepted min/max | frames_sent min/max | queue saturations | \
         reconnects/sec | cascade? |\n",
    );
    out.push_str("|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|\n");
    for row in rows {
        let metrics = row.metrics;
        out.push_str(&matrix_row(
            &row.config,
            metrics,
            &row.cascade,
            row.recovered_by_end,
        ));
        out.push('\n');
    }
    out
}

/// One input row for [`results_md`]: the metrics to render, the cascade
/// verdict, and the `recovered_by_end` flag used to derive the `survives?`
/// cell (spec §7.2).
#[derive(Debug, Clone)]
pub struct MatrixRow<'a> {
    pub config: ScenarioConfig,
    pub metrics: &'a ClusterMetrics,
    pub cascade: CascadeVerdict,
    pub recovered_by_end: bool,
}

/// Convert bytes to mebibytes, rounding to the nearest integer.
fn bytes_to_mib(b: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    (b + (MIB / 2)) / MIB
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_node(index: usize, n: usize) -> NodeMetrics {
        NodeMetrics {
            node_index: index,
            connected_peers: n - 1,
            expected_peers: n - 1,
            dial_loop_tasks: n - 1,
            writer_tasks_est: n - 1,
            reader_tasks_est: n - 1,
            outbound_queue_bytes_sum: 0,
            outbound_queue_bytes_max: 0,
            accepted: 10,
            frames_sent: 20,
            parked: 0,
            backfill_requests: 0,
            early_duplicates: 0,
        }
    }

    fn sample_cluster(n: usize, rss_bytes: u64, baseline: u64) -> ClusterMetrics {
        let nodes: Vec<NodeMetrics> = (0..n).map(|i| sample_node(i, n)).collect();
        let per_node_est = if n == 0 {
            0
        } else {
            rss_bytes.saturating_sub(baseline) / n as u64
        };
        ClusterMetrics {
            process_rss_bytes: rss_bytes,
            rss_per_node_est_bytes: per_node_est,
            total_connected_peer_entries: n * (n - 1),
            expected_connected_peer_entries: n * (n - 1),
            reconnects_per_sec: 0.0,
            queue_saturations: 0,
            accepted_min: 10,
            accepted_max: 10,
            frames_sent_min: 20,
            frames_sent_max: 20,
            task_count_is_estimated: true,
            nodes,
        }
    }

    fn sample_config(n: usize, rate: Option<f64>) -> ScenarioConfig {
        ScenarioConfig {
            n,
            rate_events_per_sec: rate,
            warmup_secs: 5,
            measure_secs: 10,
            seed_base: 0x1000,
        }
    }

    fn sample_matrix_row(n: usize, rate: Option<f64>) -> (ScenarioConfig, ClusterMetrics) {
        (
            sample_config(n, rate),
            sample_cluster(n, n as u64 * 10 * 1024 * 1024, 0),
        )
    }

    #[test]
    fn scenario_kind_serde_matches_label_lowercase() {
        for kind in [
            ScenarioKind::Idle,
            ScenarioKind::Load,
            ScenarioKind::Sweep,
            ScenarioKind::Rebind,
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.label()));
            let back: ScenarioKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn cascade_from_triggers_disjunction() {
        let none = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        assert!(!none.began);
        let any = CascadeVerdict::from_triggers(false, true, false, false, "x".to_owned());
        assert!(any.began);
        assert_eq!(any.note, "x");
        let all = CascadeVerdict::from_triggers(true, true, true, true, String::new());
        assert!(all.began);
    }

    #[test]
    fn survives_classifies_no_cascade_as_yes() {
        let v = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        assert_eq!(Survives::classify(&v, false), Survives::Yes);
        assert_eq!(Survives::classify(&v, true), Survives::Yes);
    }

    #[test]
    fn survives_classifies_cascade_recovered_as_degraded() {
        let v = CascadeVerdict::from_triggers(true, false, false, false, String::new());
        assert_eq!(Survives::classify(&v, true), Survives::Degraded);
    }

    #[test]
    fn survives_classifies_cascade_unrecovered_as_no() {
        let v = CascadeVerdict::from_triggers(false, false, true, false, String::new());
        assert_eq!(Survives::classify(&v, false), Survives::No);
    }

    #[test]
    fn matrix_row_renders_idle_row_with_dashes_for_rate() {
        let config = sample_config(5, None);
        let metrics = sample_cluster(5, 50 * 1024 * 1024, 0);
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let row = matrix_row(&config, &metrics, &cascade, true);
        assert!(
            row.starts_with("| 5 | idle | idle | yes |"),
            "idle row prefix: {row}"
        );
        // Accepted and frames_sent min/max cells render.
        assert!(row.contains("| 10/10 |"), "accepted min/max: {row}");
        assert!(row.contains("| 20/20 |"), "frames_sent min/max: {row}");
    }

    #[test]
    fn matrix_row_renders_load_row_with_rate() {
        let config = sample_config(40, Some(5.0));
        let metrics = sample_cluster(40, 500 * 1024 * 1024, 0);
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let row = matrix_row(&config, &metrics, &cascade, true);
        assert!(
            row.starts_with("| 40 | 5 | load | yes |"),
            "load row prefix: {row}"
        );
    }

    #[test]
    fn matrix_row_renders_cascade_and_degraded() {
        let config = sample_config(40, Some(1.0));
        let metrics = sample_cluster(40, 0, 0);
        let cascade = CascadeVerdict::from_triggers(false, true, false, false, "churn".to_owned());
        let row = matrix_row(&config, &metrics, &cascade, true);
        assert!(row.contains("| degraded |"), "degraded label: {row}");
        assert!(row.ends_with("| yes |"), "cascade yes suffix: {row}");
    }

    #[test]
    fn results_md_emits_header_separator_and_one_row_per_input() {
        let config = sample_config(5, Some(1.0));
        let m1 = sample_cluster(5, 10 * 1024 * 1024, 0);
        let m2 = sample_cluster(10, 20 * 1024 * 1024, 0);
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let rows = vec![
            MatrixRow {
                config: config.clone(),
                metrics: &m1,
                cascade: cascade.clone(),
                recovered_by_end: true,
            },
            MatrixRow {
                config: sample_config(10, Some(1.0)),
                metrics: &m2,
                cascade: cascade.clone(),
                recovered_by_end: true,
            },
        ];
        let table = results_md(&rows);
        let lines: Vec<&str> = table.lines().collect();
        // Header + blank + summary line + blank + header row + separator + 2 data rows.
        assert!(lines.len() >= 6, "expected at least 6 lines: {table}");
        assert!(table.contains("| N | rate events/s | mode |"));
        assert!(table.contains("| 5 | 1 | load |"));
        assert!(table.contains("| 10 | 1 | load |"));
    }

    #[test]
    fn scenario_result_round_trips_through_json() {
        let config = sample_config(20, Some(0.1));
        let idle = sample_cluster(20, 100 * 1024 * 1024, 10 * 1024 * 1024);
        let load = sample_cluster(20, 120 * 1024 * 1024, 10 * 1024 * 1024);
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let r = ScenarioResult {
            kind: ScenarioKind::Load,
            config,
            idle: Some(idle),
            load: Some(load.clone()),
            published_events: 12,
            cascade,
            notes: vec!["sample note".to_owned()],
        };
        let json = serde_json::to_string(&r).expect("serialize");
        let back: ScenarioResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.kind, ScenarioKind::Load);
        assert_eq!(back.published_events, 12);
        assert!(back.load.is_some());
        let back_load = back.load.unwrap();
        assert_eq!(back_load.process_rss_bytes, load.process_rss_bytes);
        assert_eq!(back.notes, vec!["sample note".to_owned()]);
    }

    #[test]
    fn results_md_covers_required_n_by_rate_matrix_shape() {
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let inputs = [
            (5, None),
            (5, Some(0.1)),
            (5, Some(1.0)),
            (5, Some(5.0)),
            (10, None),
            (10, Some(0.1)),
            (10, Some(1.0)),
            (10, Some(5.0)),
            (20, None),
            (20, Some(0.1)),
            (20, Some(1.0)),
            (20, Some(5.0)),
            (40, None),
            (40, Some(0.1)),
            (40, Some(1.0)),
            (40, Some(5.0)),
        ];
        let samples: Vec<(ScenarioConfig, ClusterMetrics)> = inputs
            .iter()
            .map(|(n, rate)| sample_matrix_row(*n, *rate))
            .collect();
        let rows: Vec<MatrixRow> = samples
            .iter()
            .map(|(config, metrics)| MatrixRow {
                config: config.clone(),
                metrics,
                cascade: cascade.clone(),
                recovered_by_end: true,
            })
            .collect();

        let table = results_md(&rows);
        let data_rows: Vec<&str> = table
            .lines()
            .filter(|line| line.starts_with("| "))
            .collect();
        assert_eq!(data_rows.len(), 17, "header plus 16 matrix rows: {table}");
        for n in [5, 10, 20, 40] {
            for rate in ["idle", "0.1", "1", "5"] {
                let mode = if rate == "idle" { "idle" } else { "load" };
                let needle = format!("| {n} | {rate} | {mode} |");
                assert!(
                    table.contains(&needle),
                    "missing required matrix cell {needle}: {table}"
                );
            }
        }
    }

    #[test]
    fn scenario_result_omits_none_metrics() {
        let config = sample_config(40, None);
        let cascade = CascadeVerdict::from_triggers(false, false, false, false, String::new());
        let r = ScenarioResult {
            kind: ScenarioKind::Idle,
            config,
            idle: None,
            load: None,
            published_events: 0,
            cascade,
            notes: Vec::new(),
        };
        let json = serde_json::to_string(&r).expect("serialize");
        // Check for the field-name pattern (not just the substring "idle",
        // which appears as the `"kind":"idle"` enum value).
        assert!(
            !json.contains("\"idle\":"),
            "None idle must be omitted: {json}"
        );
        assert!(
            !json.contains("\"load\":"),
            "None load must be omitted: {json}"
        );
        assert!(
            !json.contains("\"notes\""),
            "empty notes must be omitted: {json}"
        );
    }

    #[test]
    fn bytes_to_mib_rounds_to_nearest_mebibyte() {
        assert_eq!(bytes_to_mib(0), 0);
        assert_eq!(bytes_to_mib(1024 * 1024), 1);
        assert_eq!(bytes_to_mib(1024 * 1024 + 524_288), 2);
        assert_eq!(bytes_to_mib(1024 * 1024 + 100), 1);
    }
}
