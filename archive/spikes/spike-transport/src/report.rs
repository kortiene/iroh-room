//! The structured, machine-readable comparison record (spec §7.6/§7.9).
//!
//! [`ComparisonResult`] is the measurement contract: one scenario × backend
//! (× N, where meaningful) run emits exactly one of these, serialized to one
//! JSON object so a re-run is directly diffable. [`results_md`] renders the
//! rolled-up backend × N × dimension table that drops verbatim into the
//! decision memo (`NOTES.md`) and can be pasted into issue #10.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::BackendKind;

/// Propagation-latency summary for one steady-state fan-out run (spec §6
/// latency dimension): wall time from `publish` on node 0 to that `event_id`
/// appearing in node k's received set, for every k≠0.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PropagationStats {
    /// Fastest single-node arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<u64>,
    /// Median single-node arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_ms: Option<u64>,
    /// Slowest single-node arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
    /// Wall time until *every* node held the event (fan-out completion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout_completion_ms: Option<u64>,
}

impl PropagationStats {
    /// Summarize a set of per-node arrival latencies (milliseconds since
    /// publish) into min/median/max, nearest-rank.
    #[must_use]
    pub fn from_samples(mut samples: Vec<u64>, fanout_completion_ms: Option<u64>) -> Self {
        if samples.is_empty() {
            return Self {
                fanout_completion_ms,
                ..Self::default()
            };
        }
        samples.sort_unstable();
        let min_ms = samples.first().copied();
        let max_ms = samples.last().copied();
        let median_ms = Some(samples[samples.len() / 2]);
        Self {
            min_ms,
            median_ms,
            max_ms,
            fanout_completion_ms,
        }
    }
}

/// One measured comparison run: exactly the spec §7.6 field table, serialized
/// to one JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    /// Cluster size for this run.
    pub n: usize,
    /// Which backend.
    pub backend: BackendKind,
    /// Number of events broadcast in this run.
    pub events_published: usize,
    /// Whether every node's `received_ids()` converged to the full published
    /// set within the run's deadline.
    pub converged: bool,
    /// Missing ids per node index (named `blake3:<hex>` strings), empty when
    /// `converged`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_delta: BTreeMap<usize, Vec<String>>,
    /// Propagation-latency summary (steady-state fan-out scenario only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagation: Option<PropagationStats>,
    /// Late-join scenario: count of the M pre-join events the newcomer
    /// received over the transport alone (AC2; expected 0 for gossip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_join_gap: Option<usize>,
    /// Admission scenario: whether the backend refused the interloper before
    /// any event byte flowed (AC3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_enforced: Option<bool>,
    /// Admission scenario (gossip only): whether the interloper was admitted
    /// and could receive/publish with no auth check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interloper_received: Option<bool>,
    /// Count of `BackendEvent::Lagged` signals observed in this run.
    pub lagged_events: usize,
    /// Lines of backend implementation code (`mesh.rs` or `gossip.rs`),
    /// the complexity-dimension proxy (spec §6).
    pub backend_loc: usize,
    /// Count of 0.x crates this backend pulls onto the transport path.
    pub zerox_deps_added: usize,
    /// The pinned `iroh-gossip` version (gossip runs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iroh_gossip_version: Option<String>,
    /// The pinned `iroh` version under test.
    pub iroh_version: String,
    /// Free text: scenario label, anomalies, caveats.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_note: String,
}

impl ComparisonResult {
    /// Serialize to pretty JSON (so a re-run's diff is readable).
    ///
    /// # Errors
    /// Returns an error if serialization fails (should not happen for this
    /// plain data struct).
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// One row of the rolled-up `results.md` table. Missing measurements
    /// render as `—`.
    #[must_use]
    pub fn md_row(&self) -> String {
        let prop = self.propagation.unwrap_or_default();
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            self.backend.label(),
            self.n,
            self.events_published,
            yes_no(self.converged),
            opt_u64(prop.min_ms),
            opt_u64(prop.median_ms),
            opt_u64(prop.max_ms),
            opt_u64(prop.fanout_completion_ms),
            opt_usize(self.late_join_gap),
            opt_bool(self.admission_enforced),
            opt_bool(self.interloper_received),
            self.lagged_events,
        )
    }
}

/// Render a rolled-up `results.md` table from a slice of runs — the backend ×
/// N × dimension table (spec §7.9).
#[must_use]
pub fn results_md(results: &[ComparisonResult]) -> String {
    let mut out = String::new();
    out.push_str(
        "| backend | n | events | converged | prop min (ms) | prop median (ms) | prop max (ms) | fanout completion (ms) | late-join gap | admission enforced | interloper received | lagged events |\n",
    );
    out.push_str(
        "|---------|---|--------|-----------|---------------|-------------------|---------------|------------------------|---------------|---------------------|----------------------|---------------|\n",
    );
    for r in results {
        out.push_str(&r.md_row());
        out.push('\n');
    }
    out
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

fn opt_u64(v: Option<u64>) -> String {
    v.map_or_else(|| "—".to_owned(), |n| n.to_string())
}

fn opt_usize(v: Option<usize>) -> String {
    v.map_or_else(|| "—".to_owned(), |n| n.to_string())
}

fn opt_bool(v: Option<bool>) -> String {
    v.map_or_else(|| "—".to_owned(), |b| yes_no(b).to_owned())
}

#[cfg(test)]
mod tests {
    use super::{results_md, ComparisonResult, PropagationStats};
    use crate::BackendKind;

    fn sample() -> ComparisonResult {
        ComparisonResult {
            n: 3,
            backend: BackendKind::Mesh,
            events_published: 5,
            converged: true,
            set_delta: std::collections::BTreeMap::new(),
            propagation: Some(PropagationStats {
                min_ms: Some(1),
                median_ms: Some(2),
                max_ms: Some(4),
                fanout_completion_ms: Some(4),
            }),
            late_join_gap: None,
            admission_enforced: Some(true),
            interloper_received: None,
            lagged_events: 0,
            backend_loc: 200,
            zerox_deps_added: 0,
            iroh_gossip_version: None,
            iroh_version: "1.0.1".to_owned(),
            run_note: "steady-state fan-out".to_owned(),
        }
    }

    #[test]
    fn propagation_stats_from_samples_computes_min_median_max() {
        let stats = PropagationStats::from_samples(vec![30, 10, 20], Some(35));
        assert_eq!(stats.min_ms, Some(10));
        assert_eq!(stats.median_ms, Some(20));
        assert_eq!(stats.max_ms, Some(30));
        assert_eq!(stats.fanout_completion_ms, Some(35));
    }

    #[test]
    fn propagation_stats_from_empty_samples_is_none_but_keeps_fanout() {
        let stats = PropagationStats::from_samples(vec![], Some(12));
        assert!(stats.min_ms.is_none());
        assert_eq!(stats.fanout_completion_ms, Some(12));
    }

    #[test]
    fn propagation_stats_single_sample_is_min_median_and_max() {
        let stats = PropagationStats::from_samples(vec![7], None);
        assert_eq!(stats.min_ms, Some(7));
        assert_eq!(stats.median_ms, Some(7));
        assert_eq!(stats.max_ms, Some(7));
    }

    #[test]
    fn propagation_stats_even_length_median_is_upper_nearest_rank() {
        // Unsorted input, even length: min/max still correct, and the
        // nearest-rank median is `samples[len / 2]` after sorting (index 2 of
        // [10,20,30,40] = 30), never an interpolated average.
        let stats = PropagationStats::from_samples(vec![40, 10, 30, 20], None);
        assert_eq!(stats.min_ms, Some(10));
        assert_eq!(stats.max_ms, Some(40));
        assert_eq!(stats.median_ms, Some(30));
    }

    #[test]
    fn json_round_trips() {
        let r = sample();
        let json = r.to_json().expect("serialize");
        let back: ComparisonResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.n, r.n);
        assert_eq!(back.backend, r.backend);
        assert_eq!(back.converged, r.converged);
    }

    #[test]
    fn results_md_has_header_separator_and_one_row_per_result() {
        let table = results_md(&[sample(), sample()]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 4, "header + separator + 2 rows: {table}");
        assert!(lines[0].starts_with("| backend |"));
        assert!(lines[1].starts_with("|---"));
    }

    #[test]
    fn results_md_with_no_rows_is_header_and_separator_only() {
        let table = results_md(&[]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 2, "an empty run set still emits the schema");
        assert!(lines[0].starts_with("| backend |"));
        assert!(lines[1].starts_with("|---"));
    }

    #[test]
    fn md_row_renders_missing_as_dash() {
        let r = sample();
        let row = r.md_row();
        assert!(
            row.contains(" — "),
            "missing late_join_gap must render as —: {row}"
        );
    }

    #[test]
    fn md_row_renders_every_populated_measurement() {
        // A fully-measured gossip row: label, counts, converged flag,
        // propagation quartet, late-join gap, and the two admission booleans
        // (Some(false) -> "no", Some(true) -> "yes") must all render in order.
        let r = ComparisonResult {
            n: 5,
            backend: BackendKind::Gossip,
            events_published: 11,
            converged: true,
            set_delta: std::collections::BTreeMap::new(),
            propagation: Some(PropagationStats {
                min_ms: Some(1),
                median_ms: Some(3),
                max_ms: Some(9),
                fanout_completion_ms: Some(12),
            }),
            late_join_gap: Some(11),
            admission_enforced: Some(false),
            interloper_received: Some(true),
            lagged_events: 2,
            backend_loc: 0,
            zerox_deps_added: 1,
            iroh_gossip_version: None,
            iroh_version: "1.0.1".to_owned(),
            run_note: String::new(),
        };
        let row = r.md_row();
        assert!(
            row.starts_with("| gossip | 5 | 11 | yes |"),
            "label/n/events/converged prefix: {row}"
        );
        assert!(
            row.contains("| 1 | 3 | 9 | 12 |"),
            "propagation min/median/max/fanout: {row}"
        );
        assert!(
            row.contains("| 11 | no | yes | 2 |"),
            "gap / admission_enforced / interloper_received / lagged: {row}"
        );
    }

    #[test]
    fn sparse_result_omits_optional_keys_and_still_round_trips() {
        // A gossip late-join run carries no propagation/admission data and an
        // empty delta: `skip_serializing_if` must elide those keys, and the
        // `serde(default)` attrs must rehydrate them on the way back.
        let r = ComparisonResult {
            n: 4,
            backend: BackendKind::Gossip,
            events_published: 6,
            converged: false,
            set_delta: std::collections::BTreeMap::new(),
            propagation: None,
            late_join_gap: Some(6),
            admission_enforced: None,
            interloper_received: None,
            lagged_events: 0,
            backend_loc: 120,
            zerox_deps_added: 1,
            iroh_gossip_version: Some("0.101.0".to_owned()),
            iroh_version: "1.0.1".to_owned(),
            run_note: String::new(),
        };
        let json = r.to_json().expect("serialize");
        assert!(
            !json.contains("propagation"),
            "None propagation must be omitted: {json}"
        );
        assert!(
            !json.contains("set_delta"),
            "empty set_delta must be omitted: {json}"
        );
        assert!(
            !json.contains("admission_enforced"),
            "None admission_enforced must be omitted: {json}"
        );
        assert!(
            !json.contains("run_note"),
            "empty run_note must be omitted: {json}"
        );

        let back: ComparisonResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.late_join_gap, Some(6));
        assert!(back.propagation.is_none());
        assert!(back.set_delta.is_empty());
        assert_eq!(back.iroh_gossip_version.as_deref(), Some("0.101.0"));
    }
}
