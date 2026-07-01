//! The structured, machine-readable results record — the Gate-E-memo artifact
//! (spec §5 / §6.5).
//!
//! [`ProbeResult`] is the measurement contract: every scenario × direction ×
//! path-mode run emits exactly one of these, serialized to one JSON object
//! (schema = the spec §5 field table) so two operators produce comparable rows
//! and a later re-run is directly diffable. The rolled-up [`results_md`] renders
//! the scenario × direction × path-type table the spike's Day-1 deliverable calls
//! for, and drops verbatim into the `crates/iroh-rooms-net/NOTES.md` Gate-A table.
//!
//! Nothing here reads a clock or makes a networking decision: `run_at_utc` is an
//! operator-supplied provenance string stamped at emit time (spec §5 / A6), and
//! the measured durations come from the monotonic `Instant` timing in
//! [`crate::probe`]. This module only shapes and renders.

use serde::{Deserialize, Serialize};

/// The pinned iroh line the probe measures (spec §4 / assumption 3). Recorded as
/// provenance in every result so a row is unambiguously tied to the stack tested.
pub const IROH_VERSION: &str = "1.0.1";

/// Which endpoint dialed. NAT symmetry is **directional** (a pair can hole-punch
/// one way and fall back to relay the other), so both directions are always run
/// and recorded separately (spec §5 / §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Direction {
    /// Endpoint A dialed endpoint B.
    AtoB,
    /// Endpoint B dialed endpoint A.
    BtoA,
}

impl Direction {
    /// The wire/label form used in JSON and the Markdown table.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AtoB => "AtoB",
            Self::BtoA => "BtoA",
        }
    }
}

impl std::str::FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "AtoB" | "atob" | "a-to-b" => Ok(Self::AtoB),
            "BtoA" | "btoa" | "b-to-a" => Ok(Self::BtoA),
            other => Err(format!("invalid direction {other:?} (want AtoB|BtoA)")),
        }
    }
}

/// The path type a connection settled on — **read from iroh's `remote_info` active
/// address set, never inferred from latency** (spec §6.2 / risk #2). In iroh 1.0.1
/// there is no `ConnectionType` watcher; the equivalent signal is which transport
/// addresses are *actively in use* for the remote (see [`crate::probe`] and
/// `NOTES.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathType {
    /// A direct, hole-punched UDP path (an active IP transport address).
    Direct,
    /// A relay-only path (an active relay transport address, no active direct one).
    Relay,
    /// Direct and relay both active — a transitional state; record the *settled*
    /// value after the observation window (spec §6.2).
    Mixed,
    /// No usable path resolved (neither direct nor relay) — a NO-GO signal (spec §9).
    None,
}

impl PathType {
    /// The lowercase label used in JSON and the Markdown table.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
            Self::Mixed => "mixed",
            Self::None => "none",
        }
    }

    /// Whether a direct, hole-punched path was achieved (`path_type == direct`).
    #[must_use]
    pub fn is_hole_punched(self) -> bool {
        matches!(self, Self::Direct)
    }
}

/// Per-endpoint NAT characterization (spec §5 / §6.7). Enough to name the NAT
/// class and justify the "≥1 likely-symmetric" coverage; operator-supplied, since
/// only a human knows the ISP and whether a VPN was off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatInfo {
    /// `full-cone` | `restricted` | `port-restricted` | `symmetric` | `cgnat` |
    /// `unknown`.
    pub kind: String,
    /// The ISP / carrier, if known (e.g. `Comcast`, `Verizon LTE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isp: Option<String>,
    /// `wifi` | `ethernet` | `lte` | … .
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_type: Option<String>,
    /// How the class was determined: `stun` | `iroh-net-report` |
    /// `inferred-from-holepunch-result` | `operator-knowledge`.
    pub observed_via: String,
}

impl Default for NatInfo {
    fn default() -> Self {
        Self {
            kind: "unknown".to_owned(),
            isp: None,
            network_type: None,
            observed_via: "operator-knowledge".to_owned(),
        }
    }
}

impl NatInfo {
    /// Parse a compact `key=value;key=value` description into a [`NatInfo`], e.g.
    /// `kind=cgnat;isp=Verizon LTE;net=lte;via=operator-knowledge`.
    ///
    /// Unknown keys are ignored (forward-compatible); recognized keys are `kind`,
    /// `isp`, `net`/`network_type`, and `via`/`observed_via`. Empty input yields
    /// the default (`kind=unknown`, `via=operator-knowledge`).
    #[must_use]
    pub fn parse(spec: &str) -> Self {
        let mut info = Self::default();
        for pair in spec.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = value.trim().to_owned();
            match key.trim() {
                "kind" => info.kind = value,
                "isp" => info.isp = Some(value),
                "net" | "network_type" => info.network_type = Some(value),
                "via" | "observed_via" => info.observed_via = value,
                _ => {}
            }
        }
        info
    }
}

/// One measured run: exactly the spec §5 field table, serialized to one JSON
/// object. Optional numeric fields are `null` when the corresponding path was not
/// exercised in this run (e.g. `ttfb_relay_ms` on a natural direct run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Human label of the NAT pairing, e.g. `home-broadband ↔ lte-hotspot`.
    pub scenario: String,
    /// Which endpoint dialed.
    pub direction: Direction,
    /// The dialing endpoint's NAT characterization.
    pub nat_a: NatInfo,
    /// The listening endpoint's NAT characterization.
    pub nat_b: NatInfo,
    /// A usable bidi stream carried the first echo byte within the wait budget.
    pub established: bool,
    /// The **settled** path type (spec §6.2), read from iroh, never inferred.
    pub path_type: PathType,
    /// The path type observed **before** the settle window — hole-punching often
    /// starts on relay and upgrades to direct, so the upgrade itself is recorded.
    pub initial_path_type: PathType,
    /// `path_type == direct` — a direct, hole-punched path was achieved.
    pub hole_punched: bool,
    /// Whether direct paths were suppressed for this run (`--relay-only`, spec
    /// §6.4): a controlled relay measurement rather than a natural fallback.
    pub relay_only: bool,
    /// Time from dial start to first application byte received over the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    /// TTFB on a run that settled `direct`. `null` if that path was never achieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_direct_ms: Option<u64>,
    /// TTFB on a run forced/observed as `relay`. `null` if relay never exercised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_relay_ms: Option<u64>,
    /// Median application-level round-trip over the stream (N echo pings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<f64>,
    /// p90 application-level round-trip (the tail the memo cares about).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_p90_ms: Option<f64>,
    /// Sustained one-way throughput of the fixed-size echo transfer, in Mbit/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_mbit_s: Option<f64>,
    /// Wall time from process start to `established` (incl. discovery/relay
    /// resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_time_ms: Option<u64>,
    /// The relay the endpoint homed to / reached the peer through, for relay runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// The pinned iroh line under test.
    pub iroh_version: String,
    /// The probe's git sha, if supplied (env `PROBE_GIT_SHA` / `--git-sha`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_git_sha: Option<String>,
    /// Operator-supplied UTC stamp (never read from the clock in code, spec §5/A6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at_utc: Option<String>,
    /// Free text: retries, anomalies, firewall config, VPN-off confirmation,
    /// resolved remote addrs (redact home IPs per spec §8 before committing).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl ProbeResult {
    /// Serialize to the committed one-object-per-run JSON (pretty-printed so the
    /// diff of a re-run is readable).
    ///
    /// # Errors
    /// Returns an error if serialization fails (should not happen for this plain
    /// data struct).
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// One row of the rolled-up `results.md` table (see [`results_md`] for the
    /// header). Missing measurements render as `—`.
    #[must_use]
    pub fn md_row(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            self.scenario,
            self.direction.label(),
            if self.relay_only {
                "relay-only"
            } else {
                "natural"
            },
            yes_no(self.established),
            self.path_type.label(),
            opt_u64(self.ttfb_ms),
            opt_u64(self.ttfb_direct_ms),
            opt_u64(self.ttfb_relay_ms),
            opt_f64(self.rtt_ms),
            opt_f64(self.throughput_mbit_s),
            opt_u64(self.setup_time_ms),
        )
    }
}

/// Render a rolled-up `results.md` table from a slice of runs — the scenario ×
/// direction × path-type × TTFB × RTT × throughput × setup-time table the spike's
/// Day-1 deliverable calls for (spec §5 / §6.5).
#[must_use]
pub fn results_md(results: &[ProbeResult]) -> String {
    let mut out = String::new();
    out.push_str("| scenario | direction | mode | established | path type | ttfb (ms) | ttfb direct (ms) | ttfb relay (ms) | rtt median (ms) | throughput (Mbit/s) | setup (ms) |\n");
    out.push_str("|----------|-----------|------|-------------|-----------|-----------|------------------|-----------------|-----------------|---------------------|------------|\n");
    for r in results {
        out.push_str(&r.md_row());
        out.push('\n');
    }
    out
}

/// `yes` / `no` for the boolean columns.
fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

/// Render an optional integer measurement, `—` when absent.
fn opt_u64(v: Option<u64>) -> String {
    v.map_or_else(|| "—".to_owned(), |n| n.to_string())
}

/// Render an optional float measurement to one decimal, `—` when absent.
fn opt_f64(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_owned(), |n| format!("{n:.1}"))
}

#[cfg(test)]
mod tests {
    use super::{Direction, NatInfo, PathType, ProbeResult, IROH_VERSION};

    fn sample() -> ProbeResult {
        ProbeResult {
            scenario: "home-broadband ↔ lte-hotspot".to_owned(),
            direction: Direction::AtoB,
            nat_a: NatInfo::parse(
                "kind=port-restricted;isp=Comcast;net=ethernet;via=operator-knowledge",
            ),
            nat_b: NatInfo::parse(
                "kind=cgnat;isp=Verizon LTE;net=lte;via=inferred-from-holepunch-result",
            ),
            established: true,
            path_type: PathType::Direct,
            initial_path_type: PathType::Relay,
            hole_punched: true,
            relay_only: false,
            ttfb_ms: Some(412),
            ttfb_direct_ms: Some(412),
            ttfb_relay_ms: None,
            rtt_ms: Some(38.5),
            rtt_p90_ms: Some(61.0),
            throughput_mbit_s: Some(24.3),
            setup_time_ms: Some(1830),
            relay_url: Some("https://use1.relay.iroh.link./".to_owned()),
            iroh_version: IROH_VERSION.to_owned(),
            probe_git_sha: Some("deadbeef".to_owned()),
            run_at_utc: Some("2026-07-01T12:00:00Z".to_owned()),
            notes: "different networks, VPN off".to_owned(),
        }
    }

    // --- Direction / PathType labels + round-trips ---

    #[test]
    fn direction_label_and_parse_round_trip() {
        for d in [Direction::AtoB, Direction::BtoA] {
            let parsed: Direction = d.label().parse().expect("label parses back");
            assert_eq!(parsed, d);
        }
    }

    #[test]
    fn direction_parse_rejects_garbage() {
        assert!("sideways".parse::<Direction>().is_err());
    }

    #[test]
    fn path_type_hole_punched_only_for_direct() {
        assert!(PathType::Direct.is_hole_punched());
        for p in [PathType::Relay, PathType::Mixed, PathType::None] {
            assert!(
                !p.is_hole_punched(),
                "{} must not count as hole-punched",
                p.label()
            );
        }
    }

    #[test]
    fn path_type_labels_are_lowercase_stable() {
        assert_eq!(PathType::Direct.label(), "direct");
        assert_eq!(PathType::Relay.label(), "relay");
        assert_eq!(PathType::Mixed.label(), "mixed");
        assert_eq!(PathType::None.label(), "none");
    }

    // --- NatInfo::parse ---

    #[test]
    fn nat_info_parse_reads_known_keys_and_ignores_unknown() {
        let n = NatInfo::parse("kind=symmetric;isp=Foo;net=lte;via=stun;bogus=x");
        assert_eq!(n.kind, "symmetric");
        assert_eq!(n.isp.as_deref(), Some("Foo"));
        assert_eq!(n.network_type.as_deref(), Some("lte"));
        assert_eq!(n.observed_via, "stun");
    }

    #[test]
    fn nat_info_parse_empty_is_default() {
        let n = NatInfo::parse("");
        assert_eq!(n.kind, "unknown");
        assert_eq!(n.observed_via, "operator-knowledge");
        assert!(n.isp.is_none());
    }

    // --- JSON round-trips the schema (the memo/table consumer contract) ---

    #[test]
    fn probe_result_json_round_trips() {
        let r = sample();
        let json = r.to_json().expect("serialize");
        let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.scenario, r.scenario);
        assert_eq!(back.direction, r.direction);
        assert_eq!(back.path_type, r.path_type);
        assert_eq!(back.hole_punched, r.hole_punched);
        assert_eq!(back.ttfb_ms, r.ttfb_ms);
        assert_eq!(back.throughput_mbit_s, r.throughput_mbit_s);
        assert_eq!(back.relay_url, r.relay_url);
    }

    #[test]
    fn probe_result_json_uses_lowercase_path_type() {
        let json = sample().to_json().expect("serialize");
        assert!(json.contains("\"path_type\": \"direct\""), "{json}");
        assert!(json.contains("\"direction\": \"AtoB\""), "{json}");
    }

    #[test]
    fn null_measurements_are_omitted_not_rendered_as_null_strings() {
        let mut r = sample();
        r.ttfb_relay_ms = None;
        let json = r.to_json().expect("serialize");
        // `skip_serializing_if` keeps the JSON compact; the reader treats an absent
        // key as "path not exercised", identical to an explicit null.
        assert!(
            !json.contains("ttfb_relay_ms"),
            "absent field must be omitted: {json}"
        );
    }

    // --- Markdown rendering ---

    #[test]
    fn md_row_renders_all_columns_and_dashes_for_missing() {
        let mut r = sample();
        r.ttfb_relay_ms = None;
        r.throughput_mbit_s = None;
        let row = r.md_row();
        assert!(row.contains("| direct |"), "{row}");
        assert!(row.contains("| natural |"), "{row}");
        // Missing measurements render as an em-dash, not `None` or `null`.
        assert!(
            row.contains(" — "),
            "missing values must render as —: {row}"
        );
        assert!(!row.to_lowercase().contains("none"), "{row}");
    }

    #[test]
    fn results_md_has_header_and_one_row_per_result() {
        let table = super::results_md(&[sample(), sample()]);
        let lines: Vec<&str> = table.lines().collect();
        // header + separator + 2 rows
        assert_eq!(
            lines.len(),
            4,
            "table = header + separator + N rows: {table}"
        );
        assert!(lines[0].starts_with("| scenario |"));
        assert!(lines[1].starts_with("|---"));
    }

    // --- Direction::from_str all accepted aliases ---

    #[test]
    fn direction_parse_all_aliases() {
        // The spec and CLI accept three forms per direction; all must round-trip.
        for (input, expected) in [
            ("AtoB", Direction::AtoB),
            ("atob", Direction::AtoB),
            ("a-to-b", Direction::AtoB),
            ("BtoA", Direction::BtoA),
            ("btoa", Direction::BtoA),
            ("b-to-a", Direction::BtoA),
        ] {
            let got: Direction = input
                .parse()
                .unwrap_or_else(|e| panic!("alias {input:?} should parse: {e}"));
            assert_eq!(got, expected, "alias {input:?}");
        }
    }

    // --- NatInfo::parse coverage ---

    #[test]
    fn nat_info_parse_long_key_aliases_for_network_type_and_observed_via() {
        // `network_type` and `observed_via` are accepted alongside the short `net`
        // and `via` aliases. Forward-compatible: operators can use either form.
        let n = NatInfo::parse("kind=full-cone;network_type=wifi;observed_via=stun");
        assert_eq!(n.kind, "full-cone");
        assert_eq!(n.network_type.as_deref(), Some("wifi"));
        assert_eq!(n.observed_via, "stun");
    }

    #[test]
    fn nat_info_parse_trims_whitespace_around_key_and_value() {
        // Operators copy-pasting from a doc may include extra spaces; the parser
        // must be tolerant: ` kind = symmetric ` → kind="symmetric".
        let n = NatInfo::parse(" kind = symmetric ; isp = Acme ");
        assert_eq!(n.kind, "symmetric");
        assert_eq!(n.isp.as_deref(), Some("Acme"));
    }

    #[test]
    fn nat_info_json_has_expected_field_names() {
        // The memo consumer (Gate E) reads these JSON key names directly from the
        // committed artifacts — they must match the spec §5 schema exactly.
        let n = NatInfo::parse("kind=symmetric;via=stun;isp=Verizon;net=lte");
        let json = serde_json::to_string(&n).expect("serialize NatInfo");
        assert!(json.contains("\"kind\""), "{json}");
        assert!(json.contains("\"observed_via\""), "{json}");
        assert!(json.contains("\"isp\""), "{json}");
        assert!(json.contains("\"network_type\""), "{json}");

        // Optional fields must be omitted (not `null`) when absent — keeps the
        // committed JSON diff-friendly and the schema compact (spec §5).
        let n_none = NatInfo::default();
        let json_none = serde_json::to_string(&n_none).expect("serialize default");
        assert!(
            !json_none.contains("\"isp\""),
            "isp must be omitted when None: {json_none}"
        );
        assert!(
            !json_none.contains("\"network_type\""),
            "network_type must be omitted when None: {json_none}"
        );
    }

    // --- ProbeResult Markdown rendering edge cases ---

    #[test]
    fn probe_result_relay_only_renders_relay_only_mode_label() {
        // A `--relay-only` run (controlled relay measurement, spec §6.4) must show
        // "relay-only" in the mode column, not "natural".
        let mut r = sample();
        r.relay_only = true;
        r.path_type = PathType::Relay;
        let row = r.md_row();
        assert!(
            row.contains("| relay-only |"),
            "forced-relay run must say relay-only in mode column: {row}"
        );
    }

    #[test]
    fn probe_result_not_established_renders_dashes_for_all_metrics() {
        // A NO-GO run (no path) must render "no" for established, "none" for
        // path type, and "—" for every numeric metric — never "None" or "null".
        let r = ProbeResult {
            scenario: "dead-pair".to_owned(),
            direction: Direction::BtoA,
            nat_a: NatInfo::default(),
            nat_b: NatInfo::default(),
            established: false,
            path_type: PathType::None,
            initial_path_type: PathType::None,
            hole_punched: false,
            relay_only: false,
            ttfb_ms: None,
            ttfb_direct_ms: None,
            ttfb_relay_ms: None,
            rtt_ms: None,
            rtt_p90_ms: None,
            throughput_mbit_s: None,
            setup_time_ms: None,
            relay_url: None,
            iroh_version: IROH_VERSION.to_owned(),
            probe_git_sha: None,
            run_at_utc: None,
            notes: String::new(),
        };
        let row = r.md_row();
        assert!(row.contains("| no |"), "established must be no: {row}");
        assert!(row.contains("| none |"), "path type must be none: {row}");
        let dash_count = row.matches(" — ").count();
        assert!(
            dash_count >= 5,
            "at least 5 numeric-metric dashes expected in a no-path row: {row}"
        );
        assert!(
            !row.to_lowercase().contains("null"),
            "must not render 'null': {row}"
        );
    }

    #[test]
    fn results_md_empty_has_only_header_and_separator() {
        let table = super::results_md(&[]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "empty results must yield only header + separator: {table}"
        );
        assert!(lines[0].starts_with("| scenario |"));
        assert!(lines[1].starts_with("|---"));
    }

    // --- hole_punched consistency with path_type ---

    #[test]
    fn hole_punched_is_consistent_with_path_type_in_sample() {
        // The assembler must keep hole_punched = (path_type == Direct) (spec §5).
        let direct = sample(); // sample() uses PathType::Direct
        assert!(
            direct.hole_punched,
            "Direct path must have hole_punched=true"
        );
        assert_eq!(
            direct.hole_punched,
            direct.path_type.is_hole_punched(),
            "hole_punched must mirror PathType::is_hole_punched()"
        );
    }

    // --- iroh_version provenance in JSON ---

    #[test]
    fn probe_result_json_contains_pinned_iroh_version() {
        // iroh_version is the provenance field tying every result to the exact stack
        // tested. The memo consumer reads it; pin the literal so a dependency bump
        // forces a deliberate results-provenance audit.
        let json = sample().to_json().expect("serialize");
        assert!(json.contains("\"iroh_version\""), "{json}");
        assert!(
            json.contains("\"1.0.1\""),
            "pinned version must appear verbatim in JSON: {json}"
        );
    }

    // --- notes field skip_serializing_if = "String::is_empty" ---

    #[test]
    fn probe_result_empty_notes_omitted_from_json() {
        // notes: "" has `skip_serializing_if = "String::is_empty"` — the key must
        // not appear in the committed JSON at all (compact diff, spec §5).
        let mut r = sample();
        r.notes = String::new();
        let json = r.to_json().expect("serialize");
        assert!(
            !json.contains("\"notes\""),
            "empty notes must be omitted from JSON: {json}"
        );
    }

    #[test]
    fn probe_result_nonempty_notes_present_in_json() {
        // Non-empty operator notes (VPN-off confirmation, anomalies) must survive the
        // round-trip intact (spec §5 / §8 redaction guidance).
        let mut r = sample();
        r.notes = "VPN off, different networks, no common LAN".to_owned();
        let json = r.to_json().expect("serialize");
        let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
        assert!(json.contains("\"notes\""), "notes key must appear: {json}");
        assert_eq!(back.notes, r.notes, "notes must round-trip verbatim");
    }

    // --- PathType::Mixed JSON serialization ---

    #[test]
    fn probe_result_mixed_path_type_serializes_as_lowercase_mixed() {
        // PathType::Mixed is the transitional state during a relay→direct upgrade
        // (spec §6.2). It must serialize as lowercase "mixed" (rename_all =
        // "lowercase"), not "Mixed". The memo consumer reads this field as a string.
        let mut r = sample();
        r.path_type = PathType::Mixed;
        r.initial_path_type = PathType::Relay;
        r.hole_punched = PathType::Mixed.is_hole_punched(); // false
        let json = r.to_json().expect("serialize");
        assert!(
            json.contains("\"path_type\": \"mixed\""),
            "Mixed must serialize as lowercase: {json}"
        );
        assert!(
            !json.contains("\"Mixed\""),
            "PascalCase must not appear for path_type: {json}"
        );
        let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.path_type, PathType::Mixed, "Mixed must round-trip");
        assert!(!back.hole_punched, "Mixed is not hole-punched");
    }

    // --- results_md with heterogeneous rows ---

    #[test]
    fn results_md_with_heterogeneous_runs_all_rows_have_equal_column_count() {
        // The rolled-up table must handle a realistic mix of runs — different path
        // types, modes (natural / relay-only), established and not — without
        // producing rows with a mismatched column count. Each row must have the same
        // number of `|` separators as the header (spec §6.5 / memo artifact).
        let direct_run = sample();

        let relay_forced = ProbeResult {
            scenario: "broadband-to-lte".to_owned(),
            direction: Direction::BtoA,
            nat_a: NatInfo::parse("kind=port-restricted;via=operator-knowledge"),
            nat_b: NatInfo::parse("kind=cgnat;net=lte;via=inferred-from-holepunch-result"),
            established: true,
            path_type: PathType::Relay,
            initial_path_type: PathType::Relay,
            hole_punched: false,
            relay_only: true,
            ttfb_ms: Some(310),
            ttfb_direct_ms: None,
            ttfb_relay_ms: Some(310),
            rtt_ms: Some(120.0),
            rtt_p90_ms: Some(145.0),
            throughput_mbit_s: Some(3.2),
            setup_time_ms: Some(4200),
            relay_url: Some("https://euw1.relay.iroh.link./".to_owned()),
            iroh_version: IROH_VERSION.to_owned(),
            probe_git_sha: None,
            run_at_utc: None,
            notes: "relay-only run confirming fallback works (spec §6.4)".to_owned(),
        };

        let no_path = ProbeResult {
            scenario: "symmetric-pair".to_owned(),
            direction: Direction::AtoB,
            nat_a: NatInfo::default(),
            nat_b: NatInfo::default(),
            established: false,
            path_type: PathType::None,
            initial_path_type: PathType::None,
            hole_punched: false,
            relay_only: false,
            ttfb_ms: None,
            ttfb_direct_ms: None,
            ttfb_relay_ms: None,
            rtt_ms: None,
            rtt_p90_ms: None,
            throughput_mbit_s: None,
            setup_time_ms: None,
            relay_url: None,
            iroh_version: IROH_VERSION.to_owned(),
            probe_git_sha: None,
            run_at_utc: None,
            notes: String::new(),
        };

        let table = super::results_md(&[direct_run, relay_forced, no_path]);
        let lines: Vec<&str> = table.lines().collect();

        // header + separator + 3 data rows
        assert_eq!(lines.len(), 5, "expected 5 lines: {table}");

        let header_pipes = lines[0].matches('|').count();
        for row in &lines[2..] {
            let row_pipes = row.matches('|').count();
            assert_eq!(
                row_pipes, header_pipes,
                "row has different pipe count than header: {row}"
            );
        }

        // Mode column must show "relay-only" for the forced run.
        assert!(
            table.contains("relay-only"),
            "relay-only run must appear: {table}"
        );
        // Established column must show "no" for the not-established run.
        assert!(
            table.contains("| no |"),
            "not-established must appear: {table}"
        );
        // All four PathType labels must round-trip through md_row (only "direct",
        // "relay", and "none" appear here — "mixed" is the transitional case).
        assert!(table.contains("| direct |"), "direct must appear: {table}");
        assert!(table.contains("| relay |"), "relay must appear: {table}");
        assert!(table.contains("| none |"), "none must appear: {table}");
    }
}
