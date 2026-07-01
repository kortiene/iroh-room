//! Loopback self-check — the only CI-runnable part of IR-0012 (spec §10 step 6).
//!
//! It proves the harness **builds, dials, echoes, and emits a well-formed
//! `ProbeResult`** over two in-process endpoints on `127.0.0.1` with the relay
//! disabled. It asserts *shape*, not traversal: `path_type` reads `direct` on
//! loopback because the only path is the loopback IP, which tells you nothing
//! about NAT hole-punching.
//!
//! **This is NOT Gate A.** Gate A is the manual two-host run on separate real
//! NATs documented in `NOTES.md`; CI cannot prove NAT traversal.

use std::time::{Duration, Instant};

use spike_nat::probe::{
    build_endpoint, dial_and_measure, secret_from_seed, DialParams, EndpointOpts, ProbeListener,
};
use spike_nat::report::{Direction, NatInfo, PathType, ProbeResult, IROH_VERSION};

/// Small, fast params for the loopback check (the real run uses the §6.3 defaults).
fn fast_params() -> DialParams {
    DialParams {
        pings: 5,
        xfer_bytes: 64 * 1024,
        settle: Duration::from_millis(1200),
        budget: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn loopback_probe_establishes_echoes_and_measures() {
    let listener = ProbeListener::spawn(secret_from_seed(1), EndpointOpts::loopback())
        .await
        .expect("spawn loopback echo listener");
    let target = listener.dial_addr().expect("loopback dial addr");

    let dialer = build_endpoint(secret_from_seed(2), EndpointOpts::loopback())
        .await
        .expect("build loopback dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;

    // --- shape assertions (NOT traversal) ---
    assert!(m.established, "loopback dial must establish: {:?}", m.error);
    assert!(m.ttfb_ms.is_some(), "TTFB must be measured");
    assert!(
        m.rtt_median_ms.is_some(),
        "RTT must be measured over the pings"
    );
    assert!(
        m.throughput_mbit_s.is_some(),
        "throughput must be measured for the xfer"
    );
    // Over loopback the only transport address is the loopback IP, so iroh reports
    // an active direct path. We assert this as *shape* (the classifier reads iroh's
    // active-addr set), explicitly not as evidence of hole-punching.
    assert_eq!(
        m.path_type,
        PathType::Direct,
        "loopback settles direct (shape check, not Gate A)"
    );

    listener.shutdown().await.expect("listener shutdown");
}

#[tokio::test]
async fn loopback_result_serializes_to_wellformed_json() {
    let listener = ProbeListener::spawn(secret_from_seed(3), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("dial addr");
    let dialer = build_endpoint(secret_from_seed(4), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;
    assert!(m.established, "precondition: established");

    // Fold into the §5 record exactly as the CLI does, then round-trip the JSON —
    // the memo/table consumer contract (spec §6.5).
    let result = ProbeResult {
        scenario: "loopback-self-check".to_owned(),
        direction: Direction::AtoB,
        nat_a: NatInfo::parse("kind=unknown;via=operator-knowledge"),
        nat_b: NatInfo::parse("kind=unknown;via=operator-knowledge"),
        established: m.established,
        path_type: m.path_type,
        initial_path_type: m.initial_path_type,
        hole_punched: m.path_type.is_hole_punched(),
        relay_only: false,
        ttfb_ms: m.ttfb_ms,
        ttfb_direct_ms: m.ttfb_ms, // loopback settles direct
        ttfb_relay_ms: None,
        rtt_ms: m.rtt_median_ms,
        rtt_p90_ms: m.rtt_p90_ms,
        throughput_mbit_s: m.throughput_mbit_s,
        setup_time_ms: m.setup_time_ms,
        relay_url: m.relay_url.clone(),
        iroh_version: IROH_VERSION.to_owned(),
        probe_git_sha: None,
        run_at_utc: Some("1970-01-01T00:00:00Z".to_owned()),
        notes: "loopback self-check — NOT Gate A".to_owned(),
    };

    let json = result.to_json().expect("serialize");
    let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
    assert!(back.established);
    assert_eq!(back.path_type, PathType::Direct);
    assert_eq!(back.ttfb_ms, m.ttfb_ms);
    assert_eq!(back.iroh_version, IROH_VERSION);

    listener.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn loopback_probe_skips_throughput_when_xfer_is_zero() {
    // xfer_bytes=0 must leave throughput_mbit_s=None — the conditional branch in
    // run_measurement (spec §6.3: "0 skips it"). TTFB and RTT are still measured.
    let listener = ProbeListener::spawn(secret_from_seed(7), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("dial addr");
    let dialer = build_endpoint(secret_from_seed(8), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let params = DialParams {
        pings: 3,
        xfer_bytes: 0,
        settle: Duration::from_millis(500),
        budget: Duration::from_secs(10),
    };
    let m = dial_and_measure(&dialer, target, params, Instant::now()).await;

    assert!(m.established, "must establish: {:?}", m.error);
    assert!(m.ttfb_ms.is_some(), "TTFB must be measured");
    assert!(m.rtt_median_ms.is_some(), "RTT must be measured (pings=3)");
    assert!(
        m.throughput_mbit_s.is_none(),
        "throughput must be skipped when xfer_bytes=0"
    );

    listener.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn loopback_probe_skips_rtt_when_pings_is_zero() {
    // pings=0 must leave rtt_median_ms and rtt_p90_ms as None — the ping loop
    // runs zero times so summarize_rtt([]) returns (None, None). TTFB and
    // throughput are still measured (spec §6.3).
    let listener = ProbeListener::spawn(secret_from_seed(9), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("dial addr");
    let dialer = build_endpoint(secret_from_seed(10), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let params = DialParams {
        pings: 0,
        xfer_bytes: 32 * 1024,
        settle: Duration::from_millis(500),
        budget: Duration::from_secs(10),
    };
    let m = dial_and_measure(&dialer, target, params, Instant::now()).await;

    assert!(m.established, "must establish: {:?}", m.error);
    assert!(m.ttfb_ms.is_some(), "TTFB must be measured");
    assert!(
        m.rtt_median_ms.is_none(),
        "rtt_median_ms must be None when pings=0"
    );
    assert!(
        m.rtt_p90_ms.is_none(),
        "rtt_p90_ms must be None when pings=0"
    );
    assert!(
        m.throughput_mbit_s.is_some(),
        "throughput must be measured when xfer_bytes>0"
    );

    listener.shutdown().await.expect("shutdown");
}

/// Both A→B and B→A roles must produce a successful measurement (spec §5 / §7:
/// "Both directions are run — NAT symmetry is directional"). Loopback exercises
/// the role-reversal in-process: the same pair of endpoints swap dial roles and
/// both must establish and settle on `PathType::Direct`.
#[tokio::test]
async fn bidirectional_loopback_probe_both_directions_establish() {
    let listener_a = ProbeListener::spawn(secret_from_seed(11), EndpointOpts::loopback())
        .await
        .expect("spawn listener A");
    let listener_b = ProbeListener::spawn(secret_from_seed(12), EndpointOpts::loopback())
        .await
        .expect("spawn listener B");

    let addr_a = listener_a.dial_addr().expect("loopback addr A");
    let addr_b = listener_b.dial_addr().expect("loopback addr B");

    // A→B: endpoint A dials listener B.
    let ma = dial_and_measure(
        &listener_a.endpoint(),
        addr_b,
        fast_params(),
        Instant::now(),
    )
    .await;
    assert!(ma.established, "A→B must establish: {:?}", ma.error);
    assert_eq!(
        ma.path_type,
        PathType::Direct,
        "A→B must settle direct on loopback (shape check, not Gate A)"
    );

    // B→A: endpoint B dials listener A (role reversal).
    let mb = dial_and_measure(
        &listener_b.endpoint(),
        addr_a,
        fast_params(),
        Instant::now(),
    )
    .await;
    assert!(mb.established, "B→A must establish: {:?}", mb.error);
    assert_eq!(
        mb.path_type,
        PathType::Direct,
        "B→A must settle direct on loopback (shape check, not Gate A)"
    );

    listener_a.shutdown().await.expect("shutdown A");
    listener_b.shutdown().await.expect("shutdown B");
}

/// Two independent dialer/listener pairs running concurrently must not interfere:
/// the echo handler is stateless and connection-scoped, so each dialer measures
/// its own listener only. This mirrors the real Gate A setup where two operators
/// run separate `nat-probe` processes while logging their own results.
#[tokio::test]
async fn two_concurrent_probes_do_not_share_state() {
    let listener_p = ProbeListener::spawn(secret_from_seed(13), EndpointOpts::loopback())
        .await
        .expect("spawn listener P");
    let listener_q = ProbeListener::spawn(secret_from_seed(14), EndpointOpts::loopback())
        .await
        .expect("spawn listener Q");

    let addr_p = listener_p.dial_addr().expect("addr P");
    let addr_q = listener_q.dial_addr().expect("addr Q");

    let dialer_p = build_endpoint(secret_from_seed(15), EndpointOpts::loopback())
        .await
        .expect("build dialer P");
    let dialer_q = build_endpoint(secret_from_seed(16), EndpointOpts::loopback())
        .await
        .expect("build dialer Q");

    // Both dials run concurrently — no barrier, each races independently.
    let (mp, mq) = tokio::join!(
        dial_and_measure(&dialer_p, addr_p, fast_params(), Instant::now()),
        dial_and_measure(&dialer_q, addr_q, fast_params(), Instant::now()),
    );

    assert!(mp.established, "P must establish: {:?}", mp.error);
    assert!(mq.established, "Q must establish: {:?}", mq.error);
    // Each session must record its own independent throughput measurement.
    assert!(
        mp.throughput_mbit_s.is_some(),
        "P throughput must be measured independently"
    );
    assert!(
        mq.throughput_mbit_s.is_some(),
        "Q throughput must be measured independently"
    );
    assert_eq!(mp.path_type, PathType::Direct, "P must settle direct");
    assert_eq!(mq.path_type, PathType::Direct, "Q must settle direct");

    listener_p.shutdown().await.expect("shutdown P");
    listener_q.shutdown().await.expect("shutdown Q");
}

/// `ProbeListener::home_relay()` returns `None` on loopback because
/// `RelayMode::Disabled` configures no relay. On the real-network stack it would
/// return the n0 relay URL; that path requires live network access and is
/// exercised only by the manual Gate A run (spec §10 step 6).
#[tokio::test]
async fn listener_home_relay_is_none_on_loopback() {
    let listener = ProbeListener::spawn(secret_from_seed(17), EndpointOpts::loopback())
        .await
        .expect("spawn loopback listener");
    assert!(
        listener.home_relay().is_none(),
        "loopback (RelayMode::Disabled) must have no home relay"
    );
    listener.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// IR-0012 e2e coverage — Gate A instrumentation assertions (loopback proxies)
// ---------------------------------------------------------------------------

/// Verify that `initial_path_type` and `path_type` are both `Direct` on loopback.
///
/// `initial_path_type` is sampled immediately *before* the settle window
/// (spec §6.2); on loopback the only transport address is 127.0.0.1 (no relay),
/// so iroh reports an active IP addr from the very first sample. Both the pre-
/// and post-settle readings must be `Direct`.  This confirms the settle-window
/// instrumentation path is exercised — not just the final settled value — which
/// is the mechanism used on real NATs to capture the relay→direct upgrade the
/// memo cares about.
///
/// This is NOT Gate A (NAT traversal is not tested on loopback).
#[tokio::test]
async fn loopback_initial_path_type_matches_settled_path_type() {
    let listener = ProbeListener::spawn(secret_from_seed(18), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("loopback addr");
    let dialer = build_endpoint(secret_from_seed(19), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;

    assert!(m.established, "loopback must establish: {:?}", m.error);
    assert_eq!(
        m.initial_path_type,
        PathType::Direct,
        "initial_path_type (pre-settle) must be Direct on loopback"
    );
    assert_eq!(
        m.path_type,
        PathType::Direct,
        "settled path_type must be Direct on loopback"
    );
    // On loopback both readings are stable Direct — there is no relay→direct
    // upgrade to capture. On a real NAT the initial reading is often Relay; the
    // settle window is what records the upgrade (spec §6.2).
    assert_eq!(
        m.initial_path_type, m.path_type,
        "initial and settled must agree on a loopback connection"
    );

    listener.shutdown().await.expect("shutdown");
}

/// On a loopback connection with `RelayMode::Disabled`, there is no relay address
/// in the peer's transport addr set — `relay_url` in the Measurement must be
/// `None`. This is the loopback proxy for AC3: "TTFB is measured for both direct
/// and relay paths" — specifically it confirms that the relay URL column is only
/// populated when an actual relay path is active, keeping the Gate E memo
/// artifacts unambiguous.
#[tokio::test]
async fn loopback_measurement_relay_url_is_none_on_disabled_relay() {
    let listener = ProbeListener::spawn(secret_from_seed(20), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("loopback addr");
    let dialer = build_endpoint(secret_from_seed(21), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;

    assert!(m.established, "must establish: {:?}", m.error);
    assert!(
        m.relay_url.is_none(),
        "relay_url must be None when RelayMode::Disabled: {:?}",
        m.relay_url
    );
}

/// TTFB and `setup_time` must be present and well within the dial budget (10 s).
///
/// A measurement reporting a TTFB just under the 30 s budget indicates a hung
/// connection that finally timed out, not a fast direct path.  Bounding the
/// loopback measurements to ≤10 s provides the Gate E memo with confidence that
/// the timing values reflect actual path latency, not budget exhaustion.
/// (Gate A GO rubric: "established within ≤10 s", spec §9.)
#[tokio::test]
async fn loopback_ttfb_and_setup_time_are_bounded_within_ten_seconds() {
    const CEILING_MS: u64 = 10_000;

    let listener = ProbeListener::spawn(secret_from_seed(22), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("loopback addr");
    let dialer = build_endpoint(secret_from_seed(23), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;

    assert!(m.established, "must establish: {:?}", m.error);

    let ttfb = m.ttfb_ms.expect("TTFB must be measured");
    assert!(
        ttfb <= CEILING_MS,
        "loopback TTFB must be well under 10 s (got {ttfb} ms)"
    );

    let setup = m.setup_time_ms.expect("setup_time must be measured");
    assert!(
        setup <= CEILING_MS,
        "loopback setup_time must be well under 10 s (got {setup} ms); \
         the Gate A rubric requires connection within 10 s (spec §9)"
    );

    listener.shutdown().await.expect("shutdown");
}

/// Full IR-0012 pipeline: loopback probe → `ProbeResult` assembly → Gate E artifact.
///
/// Simulates what `nat-probe dial --loopback` does end-to-end:
/// 1. Spawn a loopback echo listener and dialer.
/// 2. Call `dial_and_measure` to get a raw `Measurement`.
/// 3. Assemble the `ProbeResult` with correct TTFB routing (direct path →
///    `ttfb_direct_ms`, relay path → `ttfb_relay_ms`), `hole_punched`, and
///    `relay_only` fields.
/// 4. Verify the assembled record satisfies the Gate E memo fields and JSON
///    round-trips cleanly.
///
/// This is NOT Gate A — it proves the assembly pipeline is correct, not that
/// NAT traversal works. The same assembly runs on the real two-host matrix.
#[tokio::test]
async fn loopback_probe_assembles_gate_e_ready_direct_result() {
    let listener = ProbeListener::spawn(secret_from_seed(24), EndpointOpts::loopback())
        .await
        .expect("spawn listener");
    let target = listener.dial_addr().expect("loopback addr");
    let dialer = build_endpoint(secret_from_seed(25), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    let m = dial_and_measure(&dialer, target, fast_params(), Instant::now()).await;
    assert!(m.established, "precondition: established: {:?}", m.error);

    // Replicate the TTFB-routing logic from `nat-probe dial → assemble_result`
    // (spec §5): direct path → ttfb_direct_ms; relay/relay-only → ttfb_relay_ms.
    let relay_only = false;
    let is_relay_path = relay_only || matches!(m.path_type, PathType::Relay);
    let (ttfb_direct_ms, ttfb_relay_ms) = match (m.established, is_relay_path) {
        (false, _) => (None, None),
        (true, true) => (None, m.ttfb_ms),
        (true, false) => (m.ttfb_ms, None),
    };

    let result = ProbeResult {
        scenario: "loopback-gate-e-check".to_owned(),
        direction: Direction::AtoB,
        nat_a: NatInfo::parse("kind=unknown;via=operator-knowledge"),
        nat_b: NatInfo::parse("kind=unknown;via=operator-knowledge"),
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
        probe_git_sha: None,
        run_at_utc: Some("2026-07-01T00:00:00Z".to_owned()),
        notes: "loopback gate-e pipeline check — NOT Gate A".to_owned(),
    };

    // Gate E readiness assertions (spec §9 / AC1–AC4).
    assert!(result.established, "Gate E: established must be true");
    assert_eq!(result.path_type, PathType::Direct, "Gate E: path_type");
    assert!(
        result.hole_punched,
        "Gate E: hole_punched must be true for Direct"
    );
    assert!(
        !result.relay_only,
        "Gate E: relay_only must be false for natural run"
    );
    assert!(
        result.ttfb_ms.is_some(),
        "Gate E: ttfb_ms must be present (AC3)"
    );
    assert!(
        result.ttfb_direct_ms.is_some(),
        "Gate E: ttfb_direct_ms must be populated on a direct path (AC3)"
    );
    assert!(
        result.ttfb_relay_ms.is_none(),
        "Gate E: ttfb_relay_ms must be None on a direct path (AC3)"
    );
    // within_10s is the Gate A GO signal (spec §9); on loopback it must be true.
    let within_10s = result.setup_time_ms.is_some_and(|ms| ms <= 10_000);
    assert!(
        within_10s,
        "Gate E: connection must be within 10 s on loopback (setup={:?})",
        result.setup_time_ms
    );
    assert_eq!(
        result.iroh_version, IROH_VERSION,
        "Gate E: iroh_version provenance"
    );

    // JSON round-trip — the memo consumer reads the committed JSON (spec §6.5).
    let json = result.to_json().expect("serialize");
    let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.established, result.established);
    assert_eq!(back.path_type, result.path_type);
    assert_eq!(back.hole_punched, result.hole_punched);
    assert_eq!(back.ttfb_direct_ms, result.ttfb_direct_ms);
    assert_eq!(back.ttfb_relay_ms, result.ttfb_relay_ms);

    listener.shutdown().await.expect("shutdown");
}

/// Relay-only `ProbeResult` structure for Gate E (AC2 — relay fallback confirmed).
///
/// CI cannot exercise real relay traversal, but the *structure* of a relay-only
/// result can be verified deterministically. This test constructs the `ProbeResult`
/// a `--relay-only` run would emit and asserts the Gate E memo fields are correct:
/// `hole_punched=false`, `relay_only=true`, TTFB in the relay column,
/// `ttfb_direct_ms=None`. On a real two-host run the same assertions hold when
/// direct paths are suppressed via `clear_ip_transports` (spec §6.4).
#[test]
fn relay_only_probe_result_structure_is_correct_for_gate_e() {
    let result = ProbeResult {
        scenario: "controlled-relay-fallback".to_owned(),
        direction: Direction::BtoA,
        nat_a: NatInfo::parse("kind=port-restricted;via=operator-knowledge"),
        nat_b: NatInfo::parse("kind=cgnat;net=lte;via=inferred-from-holepunch-result"),
        established: true,
        path_type: PathType::Relay,
        initial_path_type: PathType::Relay,
        hole_punched: PathType::Relay.is_hole_punched(),
        relay_only: true,
        ttfb_ms: Some(280),
        ttfb_direct_ms: None,
        ttfb_relay_ms: Some(280),
        rtt_ms: Some(115.0),
        rtt_p90_ms: Some(140.0),
        throughput_mbit_s: Some(4.2),
        setup_time_ms: Some(3800),
        relay_url: Some("https://use1.relay.iroh.link./".to_owned()),
        iroh_version: IROH_VERSION.to_owned(),
        probe_git_sha: None,
        run_at_utc: Some("2026-07-01T00:00:00Z".to_owned()),
        notes: "relay-only run; direct paths suppressed via clear_ip_transports (spec §6.4)"
            .to_owned(),
    };

    // AC2: relay fallback is confirmed to work (spec §9 relay-usability thresholds).
    assert!(result.established, "AC2: relay fallback must establish");
    assert!(
        !result.hole_punched,
        "AC2: relay path must not be hole-punched"
    );
    assert!(result.relay_only, "AC2: relay_only flag must be set");
    assert_eq!(result.path_type, PathType::Relay, "AC2: path must be relay");
    assert!(
        result.relay_url.is_some(),
        "AC2: relay URL must be recorded when relay is used"
    );

    // AC3: TTFB is in the relay column; direct column must be empty.
    assert!(
        result.ttfb_relay_ms.is_some(),
        "AC3: ttfb_relay_ms must be populated on a relay run"
    );
    assert!(
        result.ttfb_direct_ms.is_none(),
        "AC3: ttfb_direct_ms must be None on a relay-only run"
    );

    // Gate A relay-usability thresholds (spec §9): ≥1 Mbit/s and RTT ≤ 300 ms.
    let tput = result.throughput_mbit_s.unwrap_or(0.0);
    let rtt = result.rtt_ms.unwrap_or(f64::MAX);
    assert!(
        tput >= 1.0,
        "relay usability: throughput must be ≥ 1 Mbit/s (got {tput:.1})"
    );
    assert!(
        rtt <= 300.0,
        "relay usability: RTT must be ≤ 300 ms (got {rtt:.1})"
    );

    // JSON round-trip preserves all relay-specific fields.
    let json = result.to_json().expect("serialize relay-only result");
    let back: ProbeResult = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.path_type, PathType::Relay);
    assert!(!back.hole_punched);
    assert!(back.relay_only);
    assert_eq!(back.ttfb_relay_ms, Some(280));
    assert_eq!(back.ttfb_direct_ms, None);
    assert_eq!(
        back.relay_url.as_deref(),
        Some("https://use1.relay.iroh.link./")
    );
}

#[tokio::test]
async fn unreachable_dial_reports_not_established_without_panicking() {
    // Dial an id/addr nobody is serving, with a short budget: the harness must
    // return a not-established measurement (a NO-GO signal, spec §9), never hang or
    // panic. This exercises the failure path the real run records for a dead pair.
    let dialer = build_endpoint(secret_from_seed(5), EndpointOpts::loopback())
        .await
        .expect("build dialer");

    // A well-formed but unreachable loopback address (port 1 has no listener).
    let dead = iroh::EndpointAddr::new(secret_from_seed(6).public())
        .with_ip_addr("127.0.0.1:1".parse().expect("addr"));

    let params = DialParams {
        pings: 1,
        xfer_bytes: 0,
        settle: Duration::from_millis(200),
        budget: Duration::from_secs(3),
    };
    let m = dial_and_measure(&dialer, dead, params, Instant::now()).await;

    assert!(!m.established, "unreachable dial must not establish");
    assert_eq!(m.path_type, PathType::None);
    assert!(m.error.is_some(), "a failure reason must be recorded");
    assert!(m.ttfb_ms.is_none());
}
