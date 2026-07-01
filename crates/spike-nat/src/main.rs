//! `nat-probe` — the IR-0012 Gate-A real-NAT hole-punching measurement harness.
//!
//! Two subcommands, matching `net-smoke`'s ergonomics so an operator already
//! familiar with it is at home:
//!
//! ```text
//! nat-probe listen [--relay-only] [--loopback] [--seed <N>]
//! nat-probe dial <ENDPOINT_ID> [--addr <IP:PORT>] [--relay-only] [--loopback]
//!                              [--ping <N>] [--xfer <BYTES>] [--seed <N>]
//!                              [--scenario <label>] [--direction AtoB|BtoA]
//!                              [--nat-a <spec>] [--nat-b <spec>]
//!                              [--run-at <UTC>] [--git-sha <sha>] [--notes <text>]
//!                              [--json <path>]
//! ```
//!
//! `listen` stands up a minimal `iroh::Endpoint` on the n0 stack (DNS discovery +
//! default relay), serves the echo protocol, and prints its `EndpointId`. `dial`
//! connects **purely by `EndpointId`** (discovery resolves the path), measures
//! TTFB / RTT / throughput, reads the settled path type off iroh (§6.2), and emits
//! a [`ProbeResult`] — human summary on stdout, JSON on `--json`.
//!
//! `--loopback` is the offline self-check (relay disabled, dial by `--addr`); it
//! proves the harness works but is **NOT Gate A** (spec §10 step 6). `--relay-only`
//! suppresses direct paths for a controlled relay measurement (spec §6.4).

use std::path::Path;
use std::str::FromStr;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use spike_nat::probe::{
    build_endpoint, dial_and_measure, secret_from_seed, DialParams, EndpointOpts, Measurement,
    ProbeListener, RelayStack,
};
use spike_nat::report::{Direction, NatInfo, PathType, ProbeResult, IROH_VERSION};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("listen") => run_listen(&args).await,
        Some("dial") => run_dial(&args).await,
        _ => {
            eprintln!("{USAGE}");
            bail!("unrecognized command");
        }
    }
}

const USAGE: &str = "\
usage:
  nat-probe listen [--relay-only] [--loopback] [--seed <N>]
  nat-probe dial <ENDPOINT_ID> [--addr <IP:PORT>] [--relay-only] [--loopback]
                 [--ping <N>] [--xfer <BYTES>] [--seed <N>]
                 [--scenario <label>] [--direction AtoB|BtoA]
                 [--nat-a <k=v;...>] [--nat-b <k=v;...>]
                 [--run-at <UTC>] [--git-sha <sha>] [--notes <text>] [--json <path>]";

/// Bind an echo listener and serve forever (Ctrl-C to stop).
async fn run_listen(args: &[String]) -> Result<()> {
    let opts = endpoint_opts(args);
    let secret = identity(args);
    let listener = ProbeListener::spawn(secret, opts).await?;

    println!("== nat-probe listener ==");
    println!("stack       : {}", stack_label(opts));
    println!("endpoint id : {}", listener.id());
    match listener.dial_addr() {
        Ok(addr) => print_addr_hints(&addr),
        Err(err) => println!("addr hints  : (unavailable: {err})"),
    }
    if let Some(relay) = listener.home_relay() {
        println!("home relay  : {relay}");
    }
    println!("alpn        : /iroh-rooms/nat-probe/1  (echo; no room data — spec §8)");
    println!("serving echo; dial this id from the other host. Ctrl-C to stop.");

    // Keep the router alive and serving until the process is interrupted.
    std::future::pending::<()>().await;
    Ok(())
}

/// Dial a listener, measure the run, and emit a [`ProbeResult`].
async fn run_dial(args: &[String]) -> Result<()> {
    let process_start = Instant::now();

    let host_id = args
        .get(1)
        .filter(|s| !s.starts_with("--"))
        .ok_or_else(|| anyhow!("dial requires <ENDPOINT_ID>\n{USAGE}"))
        .and_then(|s| {
            EndpointId::from_str(s).map_err(|e| anyhow!("invalid endpoint id {s:?}: {e}"))
        })?;

    let opts = endpoint_opts(args);
    if opts.stack == RelayStack::Loopback && flag_value(args, "--addr").is_none() {
        bail!("--loopback dial needs --addr <IP:PORT> (no discovery in loopback mode)");
    }

    let params = dial_params(args)?;
    let secret = identity(args);
    let endpoint = build_endpoint(secret, opts).await?;

    // Dial purely by EndpointId; --addr optionally seeds a direct hint.
    let mut target = EndpointAddr::new(host_id);
    if let Some(addr) = flag_value(args, "--addr") {
        let socket = std::net::SocketAddr::from_str(&addr).context("invalid --addr")?;
        target = target.with_ip_addr(socket);
    }

    let scenario = flag_value(args, "--scenario").unwrap_or_else(|| "unlabeled".to_owned());
    let direction = flag_value(args, "--direction")
        .map(|s| Direction::from_str(&s))
        .transpose()
        .map_err(|e| anyhow!(e))?
        .unwrap_or(Direction::AtoB);

    println!("== nat-probe dialer ==");
    println!("stack       : {}", stack_label(opts));
    println!("endpoint id : {}", endpoint.id());
    println!("dialing host: {host_id}");
    println!("scenario    : {scenario} ({})", direction.label());
    println!("measuring (budget {:?})...", params.budget);

    let m = dial_and_measure(&endpoint, target, params, process_start).await;
    print_measurement_summary(&m);

    let result = assemble_result(&m, args, scenario, direction, opts);
    print_verdict(&result);

    if let Some(path) = flag_value(args, "--json") {
        write_json(&result, &path)?;
        println!("wrote {path}");
    } else {
        println!("\n{}", result.to_json().context("serialize result")?);
    }

    // Close the endpoint gracefully so the relay/socket teardown is clean (avoids
    // iroh's "dropped without calling close" warning on process exit).
    endpoint.close().await;
    Ok(())
}

/// Fold the raw [`Measurement`] plus operator-supplied context into a
/// [`ProbeResult`] (the §5 measurement contract). TTFB is bucketed into the
/// direct/relay column by the settled path type (or forced to relay under
/// `--relay-only`); a failed run keeps its reason in `notes` so the NO-GO is
/// self-documenting.
fn assemble_result(
    m: &Measurement,
    args: &[String],
    scenario: String,
    direction: Direction,
    opts: EndpointOpts,
) -> ProbeResult {
    let relay_only = opts.relay_only;
    let is_relay_path = relay_only || matches!(m.path_type, PathType::Relay);
    let (ttfb_direct_ms, ttfb_relay_ms) = match (m.established, is_relay_path) {
        (false, _) => (None, None),
        (true, true) => (None, m.ttfb_ms),
        (true, false) => (m.ttfb_ms, None),
    };

    let mut notes = flag_value(args, "--notes").unwrap_or_default();
    if let Some(err) = &m.error {
        push_note(&mut notes, &format!("run did not establish: {err}"));
    }
    if m.initial_path_type != m.path_type {
        push_note(
            &mut notes,
            &format!(
                "path upgraded {} -> {} within the settle window",
                m.initial_path_type.label(),
                m.path_type.label()
            ),
        );
    }

    ProbeResult {
        scenario,
        direction,
        nat_a: NatInfo::parse(&flag_value(args, "--nat-a").unwrap_or_default()),
        nat_b: NatInfo::parse(&flag_value(args, "--nat-b").unwrap_or_default()),
        established: m.established,
        path_type: m.path_type,
        initial_path_type: m.initial_path_type,
        hole_punched: m.path_type.is_hole_punched(),
        relay_only,
        ttfb_ms: m.ttfb_ms,
        ttfb_direct_ms,
        ttfb_relay_ms,
        rtt_ms: m.rtt_median_ms,
        rtt_p90_ms: m.rtt_p90_ms,
        throughput_mbit_s: m.throughput_mbit_s,
        setup_time_ms: m.setup_time_ms,
        relay_url: m.relay_url.clone(),
        iroh_version: IROH_VERSION.to_owned(),
        probe_git_sha: flag_value(args, "--git-sha")
            .or_else(|| std::env::var("PROBE_GIT_SHA").ok()),
        run_at_utc: flag_value(args, "--run-at").or_else(|| std::env::var("PROBE_RUN_AT").ok()),
        notes,
    }
}

/// Print the measured numbers and the resolved remote addrs (the latter to the
/// operator's console only — do NOT paste home IPs into the committed JSON, spec §8).
fn print_measurement_summary(m: &Measurement) {
    println!("\n-- measurement --");
    println!("established : {}", m.established);
    if !m.established {
        if let Some(err) = &m.error {
            println!("reason      : {err}");
        }
        return;
    }
    println!(
        "path type   : {} (initial {}) {}",
        m.path_type.label(),
        m.initial_path_type.label(),
        if m.path_type.is_hole_punched() {
            "[HOLE-PUNCHED]"
        } else {
            ""
        }
    );
    print_opt("ttfb", m.ttfb_ms, "ms");
    print_opt("setup", m.setup_time_ms, "ms");
    print_opt_f("rtt median", m.rtt_median_ms, "ms");
    print_opt_f("rtt p90", m.rtt_p90_ms, "ms");
    print_opt_f("throughput", m.throughput_mbit_s, "Mbit/s");
    if let Some(relay) = &m.relay_url {
        println!("relay       : {relay}");
    }
    if !m.remote_addrs.is_empty() {
        println!("remote addrs (private — redact before committing, spec §8):");
        for a in &m.remote_addrs {
            println!("  - {a}");
        }
    }
}

/// Print the GO/NO-GO-relevant one-liner for this single run against the spec §9
/// thresholds. (The gate verdict is over the *whole matrix*, not one run — this is
/// just the per-run signal for the operator.)
fn print_verdict(r: &ProbeResult) {
    println!("\n-- per-run signal (full GO/NO-GO is over the matrix, spec §9) --");
    if !r.established {
        println!("NO-PATH: neither direct nor relay established — a NO-GO input (spec §9).");
        return;
    }
    let within_10s = r.setup_time_ms.is_some_and(|ms| ms <= 10_000);
    println!(
        "established={} path={} hole_punched={} within_10s={}",
        r.established,
        r.path_type.label(),
        r.hole_punched,
        within_10s
    );
    if let (Some(rtt), Some(tput)) = (r.rtt_ms, r.throughput_mbit_s) {
        let relay_usable = tput >= 1.0 && rtt <= 300.0;
        if r.relay_only || matches!(r.path_type, PathType::Relay) {
            println!(
                "relay usability (≥1 Mbit/s & RTT ≤300 ms): {} (rtt={rtt:.1} ms, tput={tput:.1} Mbit/s)",
                if relay_usable { "OK" } else { "BELOW THRESHOLD" }
            );
        }
    }
}

// --- endpoint / identity / params from args ---

/// Resolve the endpoint stack from `--loopback` / `--relay-only`.
fn endpoint_opts(args: &[String]) -> EndpointOpts {
    let loopback = has_flag(args, "--loopback");
    let relay_only = has_flag(args, "--relay-only");
    match (loopback, relay_only) {
        (true, _) => EndpointOpts::loopback(),
        (false, true) => EndpointOpts::real_relay_only(),
        (false, false) => EndpointOpts::real(),
    }
}

/// The probe identity: `--seed <N>` for reproducibility, else a fresh random key.
fn identity(args: &[String]) -> SecretKey {
    match flag_value(args, "--seed").and_then(|s| s.parse::<u64>().ok()) {
        Some(seed) => secret_from_seed(seed),
        None => SecretKey::generate(),
    }
}

/// Build [`DialParams`] from the flags, defaulting to the spec §6.3 values.
fn dial_params(args: &[String]) -> Result<DialParams> {
    let mut params = DialParams::default();
    if let Some(p) = flag_value(args, "--ping") {
        params.pings = p.parse().context("--ping must be an integer")?;
    }
    if let Some(x) = flag_value(args, "--xfer") {
        params.xfer_bytes = x.parse().context("--xfer must be a byte count")?;
    }
    Ok(params)
}

fn stack_label(opts: EndpointOpts) -> String {
    match (opts.stack, opts.relay_only) {
        (RelayStack::Loopback, _) => "loopback (RelayMode::Disabled — NOT Gate A)".to_owned(),
        (RelayStack::RealNetwork, false) => "real-network (n0 discovery + relay)".to_owned(),
        (RelayStack::RealNetwork, true) => {
            "real-network, relay-only (direct paths suppressed)".to_owned()
        }
    }
}

// --- small helpers ---

fn write_json(result: &ProbeResult, path: &str) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create results dir {}", parent.display()))?;
        }
    }
    let json = result.to_json().context("serialize result")?;
    std::fs::write(path, json).with_context(|| format!("write {path}"))?;
    Ok(())
}

fn push_note(notes: &mut String, msg: &str) {
    if !notes.is_empty() {
        notes.push_str("; ");
    }
    notes.push_str(msg);
}

fn print_opt(label: &str, v: Option<u64>, unit: &str) {
    if let Some(n) = v {
        println!("{label:<12}: {n} {unit}");
    }
}

fn print_opt_f(label: &str, v: Option<f64>, unit: &str) {
    if let Some(n) = v {
        println!("{label:<12}: {n:.1} {unit}");
    }
}

fn print_addr_hints(addr: &EndpointAddr) {
    if addr.addrs.is_empty() {
        println!("addr hints  : (none yet; discovery/relay may take a moment)");
    } else {
        for ta in &addr.addrs {
            println!("addr hint   : {ta}");
        }
    }
}

/// Read the value following a `--flag`.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Whether a boolean `--flag` is present.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

#[cfg(test)]
mod tests {
    use super::{assemble_result, dial_params, endpoint_opts, flag_value, has_flag, push_note};
    use spike_nat::probe::{Measurement, RelayStack};
    use spike_nat::report::{Direction, PathType};

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    fn established_measurement(path_type: PathType, ttfb_ms: u64) -> Measurement {
        Measurement {
            established: true,
            setup_time_ms: Some(200),
            ttfb_ms: Some(ttfb_ms),
            rtt_median_ms: Some(15.0),
            rtt_p90_ms: Some(22.0),
            throughput_mbit_s: Some(50.0),
            initial_path_type: PathType::Relay,
            path_type,
            relay_url: None,
            remote_addrs: Vec::new(),
            error: None,
        }
    }

    fn failed_measurement() -> Measurement {
        Measurement {
            established: false,
            setup_time_ms: None,
            ttfb_ms: None,
            rtt_median_ms: None,
            rtt_p90_ms: None,
            throughput_mbit_s: None,
            initial_path_type: PathType::None,
            path_type: PathType::None,
            relay_url: None,
            remote_addrs: Vec::new(),
            error: Some("connection refused".to_owned()),
        }
    }

    // --- assemble_result: TTFB bucketing (spec §5 / §6.4) ---

    #[test]
    fn assemble_result_direct_path_puts_ttfb_in_direct_column() {
        let m = established_measurement(PathType::Direct, 120);
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "test".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::loopback(),
        );
        assert_eq!(
            r.ttfb_direct_ms,
            Some(120),
            "direct TTFB must go in ttfb_direct_ms"
        );
        assert_eq!(
            r.ttfb_relay_ms, None,
            "relay column must be None on a direct run"
        );
    }

    #[test]
    fn assemble_result_relay_path_puts_ttfb_in_relay_column() {
        let m = established_measurement(PathType::Relay, 250);
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "test".to_owned(),
            Direction::BtoA,
            super::EndpointOpts::loopback(),
        );
        assert_eq!(
            r.ttfb_relay_ms,
            Some(250),
            "relay TTFB must go in ttfb_relay_ms"
        );
        assert_eq!(
            r.ttfb_direct_ms, None,
            "direct column must be None on a relay run"
        );
    }

    #[test]
    fn assemble_result_relay_only_forces_ttfb_to_relay_column() {
        // With --relay-only the direct path is suppressed; TTFB must always land in
        // the relay column regardless of what path_type iroh reports (spec §6.4).
        let m = established_measurement(PathType::Direct, 80);
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "test".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::real_relay_only(),
        );
        assert_eq!(
            r.ttfb_relay_ms,
            Some(80),
            "relay_only must override path_type for TTFB bucketing"
        );
        assert_eq!(r.ttfb_direct_ms, None);
        assert!(
            r.relay_only,
            "relay_only flag must be carried into the result"
        );
    }

    #[test]
    fn assemble_result_not_established_has_no_ttfb_in_either_column() {
        // A failed run is a NO-GO signal; both TTFB columns must be None so the
        // results table doesn't fabricate a measurement (spec §9).
        let m = failed_measurement();
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "dead-pair".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::loopback(),
        );
        assert!(!r.established);
        assert_eq!(r.ttfb_ms, None);
        assert_eq!(r.ttfb_direct_ms, None);
        assert_eq!(r.ttfb_relay_ms, None);
    }

    #[test]
    fn assemble_result_path_upgrade_appends_note() {
        // When initial_path_type ≠ path_type the assembler records the upgrade in
        // notes (spec §6.2: the upgrade latency is interesting for the memo).
        let m = Measurement {
            established: true,
            setup_time_ms: Some(500),
            ttfb_ms: Some(100),
            rtt_median_ms: Some(20.0),
            rtt_p90_ms: Some(30.0),
            throughput_mbit_s: Some(40.0),
            initial_path_type: PathType::Relay,
            path_type: PathType::Direct,
            relay_url: None,
            remote_addrs: Vec::new(),
            error: None,
        };
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "upgrade-test".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::loopback(),
        );
        assert!(
            r.notes.contains("upgraded"),
            "relay→direct upgrade must appear in notes: {:?}",
            r.notes
        );
    }

    // --- flag_value / has_flag helpers ---

    #[test]
    fn flag_value_finds_next_argument() {
        let args = strs(&[
            "dial",
            "fakeid",
            "--scenario",
            "home-to-lte",
            "--ping",
            "10",
        ]);
        assert_eq!(
            flag_value(&args, "--scenario").as_deref(),
            Some("home-to-lte")
        );
        assert_eq!(flag_value(&args, "--ping").as_deref(), Some("10"));
    }

    #[test]
    fn flag_value_returns_none_for_absent_flag() {
        let args = strs(&["--scenario", "x"]);
        assert!(flag_value(&args, "--ping").is_none());
    }

    #[test]
    fn flag_value_at_end_without_value_returns_none() {
        // `--flag` with no following token must not panic.
        let args = strs(&["--scenario"]);
        assert!(flag_value(&args, "--scenario").is_none());
    }

    #[test]
    fn has_flag_detects_presence_and_absence() {
        let args = strs(&["--relay-only", "--loopback"]);
        assert!(has_flag(&args, "--relay-only"));
        assert!(has_flag(&args, "--loopback"));
        assert!(!has_flag(&args, "--json"));
    }

    // --- push_note helper ---

    #[test]
    fn push_note_first_append_has_no_separator() {
        let mut notes = String::new();
        push_note(&mut notes, "first");
        assert_eq!(notes, "first");
    }

    #[test]
    fn push_note_subsequent_appends_use_semicolon_separator() {
        let mut notes = "first".to_owned();
        push_note(&mut notes, "second");
        assert_eq!(notes, "first; second");
        push_note(&mut notes, "third");
        assert_eq!(notes, "first; second; third");
    }

    // --- endpoint_opts from CLI args ---

    #[test]
    fn endpoint_opts_no_flags_is_real_network() {
        let args = strs(&["listen"]);
        let opts = endpoint_opts(&args);
        assert_eq!(opts.stack, RelayStack::RealNetwork);
        assert!(!opts.relay_only);
    }

    #[test]
    fn endpoint_opts_loopback_flag_is_loopback() {
        let args = strs(&["listen", "--loopback"]);
        let opts = endpoint_opts(&args);
        assert_eq!(opts.stack, RelayStack::Loopback);
    }

    #[test]
    fn endpoint_opts_relay_only_flag_is_real_relay_only() {
        let args = strs(&["listen", "--relay-only"]);
        let opts = endpoint_opts(&args);
        assert_eq!(opts.stack, RelayStack::RealNetwork);
        assert!(opts.relay_only);
    }

    // --- dial_params from CLI args ---

    #[test]
    fn dial_params_overrides_ping_and_xfer() {
        let args = strs(&["dial", "fakeid", "--ping", "5", "--xfer", "131072"]);
        let p = dial_params(&args).expect("parse params");
        assert_eq!(p.pings, 5);
        assert_eq!(p.xfer_bytes, 131_072);
    }

    #[test]
    fn dial_params_invalid_ping_returns_error() {
        let args = strs(&["dial", "fakeid", "--ping", "notanumber"]);
        assert!(
            dial_params(&args).is_err(),
            "--ping with non-integer must error"
        );
    }

    #[test]
    fn dial_params_invalid_xfer_returns_error() {
        let args = strs(&["dial", "fakeid", "--xfer", "notanumber"]);
        assert!(
            dial_params(&args).is_err(),
            "--xfer with non-integer must error"
        );
    }

    // --- assemble_result: PathType::Mixed TTFB bucketing ---

    #[test]
    fn assemble_result_mixed_path_puts_ttfb_in_direct_column() {
        // PathType::Mixed (both direct and relay active — transitional, spec §6.2)
        // is not a relay path. TTFB must land in the direct column: the bucketing
        // logic is `is_relay_path = relay_only || matches!(path_type, Relay)`, and
        // Mixed is not Relay (spec §5 / assemble_result).
        let m = established_measurement(PathType::Mixed, 90);
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid"]),
            "transitional".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::loopback(),
        );
        assert_eq!(
            r.ttfb_direct_ms,
            Some(90),
            "Mixed settled path TTFB must land in the direct column"
        );
        assert_eq!(r.ttfb_relay_ms, None, "relay column must be None for Mixed");
    }

    // --- assemble_result: upgrade-note injection ---

    #[test]
    fn assemble_result_no_upgrade_note_when_initial_equals_settled() {
        // When initial_path_type == path_type (no upgrade), the assembler must NOT
        // inject an "upgraded" note — operator-supplied notes should not be polluted
        // with false upgrade entries (spec §6.2: upgrade note only when they differ).
        let m = Measurement {
            established: true,
            setup_time_ms: Some(100),
            ttfb_ms: Some(50),
            rtt_median_ms: Some(10.0),
            rtt_p90_ms: Some(15.0),
            throughput_mbit_s: Some(30.0),
            initial_path_type: PathType::Direct,
            path_type: PathType::Direct,
            relay_url: None,
            remote_addrs: Vec::new(),
            error: None,
        };
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid", "--notes", "VPN off"]),
            "steady-direct".to_owned(),
            Direction::AtoB,
            super::EndpointOpts::loopback(),
        );
        assert!(
            !r.notes.contains("upgraded"),
            "no upgrade note expected when initial == settled: {:?}",
            r.notes
        );
        assert!(
            r.notes.contains("VPN off"),
            "operator note must be preserved: {:?}",
            r.notes
        );
    }

    #[test]
    fn assemble_result_operator_notes_and_upgrade_note_concatenated_with_separator() {
        // When both an operator note (--notes) and a path upgrade are present, the
        // assembler must join them with "; " via push_note — not concatenate directly
        // or drop either (spec §6.2 + push_note contract). The operator note comes
        // first (it is set from the flag before push_note injects the upgrade).
        let m = Measurement {
            established: true,
            setup_time_ms: Some(300),
            ttfb_ms: Some(200),
            rtt_median_ms: Some(50.0),
            rtt_p90_ms: Some(80.0),
            throughput_mbit_s: Some(5.0),
            initial_path_type: PathType::Relay,
            path_type: PathType::Direct,
            relay_url: None,
            remote_addrs: Vec::new(),
            error: None,
        };
        let r = assemble_result(
            &m,
            &strs(&["dial", "fakeid", "--notes", "mobile hotspot; no VPN"]),
            "upgrade-with-op-note".to_owned(),
            Direction::BtoA,
            super::EndpointOpts::loopback(),
        );
        assert!(
            r.notes.contains("mobile hotspot"),
            "operator note must appear: {:?}",
            r.notes
        );
        assert!(
            r.notes.contains("upgraded"),
            "upgrade note must appear: {:?}",
            r.notes
        );
        assert!(
            r.notes.contains("; "),
            "entries must be separated by '; ': {:?}",
            r.notes
        );
    }
}
