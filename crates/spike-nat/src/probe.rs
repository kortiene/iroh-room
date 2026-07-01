//! The `nat-probe` substrate harness: endpoint bring-up, the echo protocol,
//! dial-and-measure, and the path-type settle-and-sample classification
//! (spec §6.1 / §6.2 / §6.3).
//!
//! This is a bare `iroh::Endpoint` echo probe — deliberately *not* the shipping
//! carrier — so Gate A tests the **iroh substrate itself** (does hole-punching
//! work on real NATs) isolated from our ALPN/framing (spec §4, Day-1 "minimal
//! `iroh::Endpoint` … open one bidi stream; echo bytes").
//!
//! ## Path-type classification (the load-bearing instrumentation, spec §6.2)
//!
//! Direct-vs-relay MUST be read from iroh, never inferred from latency. iroh
//! **1.0.1 has no `ConnectionType` watcher** (the name the recon expected — it
//! drifted, exactly as the spec warned). The equivalent signal on this pin is
//! [`Endpoint::remote_info`]: the set of transport addresses iroh is *actively
//! using* for the remote. An active [`TransportAddr::Ip`] means a direct,
//! hole-punched path; an active [`TransportAddr::Relay`] with no active IP means
//! relay-only; both active is the transitional `mixed`. See `NOTES.md` §2.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use iroh::endpoint::{
    presets, Connection, RecvStream, RemoteInfo, SendStream, TransportAddrUsage, VarInt,
};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};

use crate::report::PathType;

/// The dedicated probe ALPN. A distinct protocol from the shipping event/pipe
/// ALPNs so the probe never touches room admission, membership, or data (spec §8):
/// it echoes synthetic bytes between iroh-authenticated identities and nothing more.
pub const NAT_PROBE_ALPN: &[u8] = b"/iroh-rooms/nat-probe/1";

/// First byte the dialer sends to trigger the first echo (the TTFB marker).
const MARKER_BYTE: u8 = 0x01;
/// Byte used for each RTT ping and the throughput payload.
const PING_BYTE: u8 = 0x02;
/// Chunk size for the echo server's read/echo loop and the dialer's drain.
const IO_CHUNK: usize = 64 * 1024;
/// How often to re-sample the path type inside the settle window (spec §6.2).
const SETTLE_POLL: Duration = Duration::from_millis(250);
/// Normal application close code for a clean, locally-initiated disconnect.
const CLOSE_OK: VarInt = VarInt::from_u32(0);

/// Which iroh stack the endpoint binds (spec §4.8 / §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayStack {
    /// Loopback / offline self-check: `presets::Minimal` + `RelayMode::Disabled`.
    /// Direct over `127.0.0.1`, no relay, no discovery. **This is NOT Gate A** — it
    /// only proves the harness builds, dials, echoes, and emits a well-formed
    /// result (spec §10 step 6).
    Loopback,
    /// Real network (Gate A): `presets::N0` — n0 DNS discovery + default relay, so
    /// dial-by-`EndpointId` works across NATs (spec §6.1 / §7).
    RealNetwork,
}

/// How to build a probe endpoint.
#[derive(Debug, Clone, Copy)]
pub struct EndpointOpts {
    /// Loopback vs real-network stack.
    pub stack: RelayStack,
    /// Force relay: suppress direct UDP paths via [`clear_ip_transports`] so all
    /// traffic is pinned to the relay (spec §6.4 forced-relay mode). Only
    /// meaningful with [`RelayStack::RealNetwork`]; ignored on loopback (which has
    /// no relay to pin to). Both endpoints must set this for a clean relay-only
    /// measurement.
    ///
    /// [`clear_ip_transports`]: iroh::endpoint::Builder::clear_ip_transports
    pub relay_only: bool,
}

impl EndpointOpts {
    /// A real-network probe endpoint on the default n0 stack.
    #[must_use]
    pub fn real() -> Self {
        Self {
            stack: RelayStack::RealNetwork,
            relay_only: false,
        }
    }

    /// A real-network probe endpoint with direct paths suppressed (`--relay-only`).
    #[must_use]
    pub fn real_relay_only() -> Self {
        Self {
            stack: RelayStack::RealNetwork,
            relay_only: true,
        }
    }

    /// A loopback probe endpoint (offline self-check; NOT Gate A).
    #[must_use]
    pub fn loopback() -> Self {
        Self {
            stack: RelayStack::Loopback,
            relay_only: false,
        }
    }
}

/// Tunables for one dial-and-measure run.
#[derive(Debug, Clone, Copy)]
pub struct DialParams {
    /// Number of application RTT pings (1 byte up, 1 byte back). Median + p90.
    pub pings: u32,
    /// One-way throughput transfer size in bytes (echoed back). 0 skips it.
    pub xfer_bytes: usize,
    /// Path-type settle window: hole-punching often starts on relay and *upgrades*
    /// to direct, so we observe for this long and record the **settled** value
    /// (spec §6.2). We exit early once a stable `direct` is seen.
    pub settle: Duration,
    /// Per-operation wait budget. Generous, since real discovery, relay resolution
    /// and hole-punch can together take seconds; it only bounds a hang, it is not
    /// the measured number (spec §6.1).
    pub budget: Duration,
}

impl Default for DialParams {
    fn default() -> Self {
        Self {
            pings: 20,
            xfer_bytes: 8 * 1024 * 1024, // 8 MiB (spec §6.3)
            settle: Duration::from_secs(4),
            budget: Duration::from_secs(30),
        }
    }
}

/// The raw measured values of one run, before it is folded (with operator-supplied
/// context) into a [`crate::report::ProbeResult`].
#[derive(Debug, Clone)]
pub struct Measurement {
    /// A usable bidi stream carried the first echo byte within the budget.
    pub established: bool,
    /// Wall time from `process_start` to `established` (incl. discovery/relay).
    pub setup_time_ms: Option<u64>,
    /// Dial start → first application byte received (spec §6.3).
    pub ttfb_ms: Option<u64>,
    /// Median application RTT over `pings` echo round-trips.
    pub rtt_median_ms: Option<f64>,
    /// p90 application RTT.
    pub rtt_p90_ms: Option<f64>,
    /// Sustained one-way throughput of the echo transfer, in Mbit/s.
    pub throughput_mbit_s: Option<f64>,
    /// Path type observed immediately after `established`, before the settle window.
    pub initial_path_type: PathType,
    /// Path type after the settle window — the value recorded as `path_type`.
    pub path_type: PathType,
    /// The relay the peer was reached through / homed to, if any.
    pub relay_url: Option<String>,
    /// The remote's resolved transport addresses (for the operator's *private* log;
    /// may reveal a home IP, so it is NOT auto-committed — see spec §8 redaction).
    pub remote_addrs: Vec<String>,
    /// A one-line failure reason when `established == false`.
    pub error: Option<String>,
}

impl Measurement {
    /// A not-established measurement carrying a failure reason (no path within the
    /// budget → a NO-GO signal for that run, spec §9).
    fn failed(reason: String) -> Self {
        Self {
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
            error: Some(reason),
        }
    }
}

/// A running echo listener on the probe ALPN.
pub struct ProbeListener {
    router: Router,
    opts: EndpointOpts,
}

impl ProbeListener {
    /// Bind a probe endpoint and serve the echo protocol forever (until
    /// [`shutdown`](Self::shutdown)). In real-network mode this waits for the home
    /// relay so the endpoint is immediately discoverable/dialable by id.
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn spawn(secret: SecretKey, opts: EndpointOpts) -> Result<Self> {
        let endpoint = build_endpoint(secret, opts).await?;
        let router = Router::builder(endpoint)
            .accept(NAT_PROBE_ALPN, EchoHandler)
            .spawn();
        Ok(Self { router, opts })
    }

    /// This endpoint's authenticated identity (`EndpointId`).
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    /// A clone of the underlying endpoint (for tests that dial it directly).
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.router.endpoint().clone()
    }

    /// A dialable address for this listener. Loopback returns `id + 127.0.0.1:port`
    /// (no discovery); real-network returns the discovered address (relay + direct
    /// hints) — though a real dialer only needs the bare `EndpointId`.
    ///
    /// # Errors
    /// Returns an error if a loopback endpoint has no bound UDP socket.
    pub fn dial_addr(&self) -> Result<EndpointAddr> {
        match self.opts.stack {
            RelayStack::Loopback => loopback_addr(self.router.endpoint()),
            RelayStack::RealNetwork => Ok(self.router.endpoint().addr()),
        }
    }

    /// The relay this endpoint homed to, if any (for the results record).
    #[must_use]
    pub fn home_relay(&self) -> Option<String> {
        relay_url_of(&self.router.endpoint().addr())
    }

    /// Gracefully stop serving.
    ///
    /// # Errors
    /// Returns an error if the router shutdown task fails.
    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.context("router shutdown")?;
        Ok(())
    }
}

/// The echo `ProtocolHandler`: accept one bidi stream and mirror every byte back
/// until the dialer finishes the stream. Stateless and identity-agnostic — it
/// admits any authenticated peer because the probe grants nothing (no room, no
/// data), which is exactly the substrate question Gate A asks.
#[derive(Debug, Clone, Copy)]
struct EchoHandler;

impl ProtocolHandler for EchoHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id();
        let (mut send, mut recv) = conn.accept_bi().await?;
        let mut buf = vec![0u8; IO_CHUNK];
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    if let Err(err) = send.write_all(&buf[..n]).await {
                        tracing::debug!(%peer, %err, "echo: write failed; ending");
                        break;
                    }
                }
                Ok(None) => break, // clean EOF: the dialer finished the stream
                Err(err) => {
                    tracing::debug!(%peer, %err, "echo: read error; ending");
                    break;
                }
            }
        }
        let _ = send.finish();
        Ok(())
    }
}

/// Build a probe endpoint per `opts`. In real-network mode this also waits for the
/// home relay ([`Endpoint::online`]) so the endpoint is discoverable and hole
/// punching can begin; loopback skips it (there is no relay to wait for).
///
/// # Errors
/// Returns an error if the endpoint fails to bind.
pub async fn build_endpoint(secret: SecretKey, opts: EndpointOpts) -> Result<Endpoint> {
    let endpoint = match opts.stack {
        RelayStack::Loopback => Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .context("bind loopback probe endpoint")?,
        RelayStack::RealNetwork => {
            let mut builder = Endpoint::builder(presets::N0).secret_key(secret);
            if opts.relay_only {
                // Suppress direct UDP path discovery so traffic is pinned to the
                // relay — the iroh 1.0.1 knob for a deterministic relay measurement
                // (spec §6.4; verified against source, see NOTES.md §2).
                builder = builder.clear_ip_transports();
            }
            let endpoint = builder
                .bind()
                .await
                .context("bind real-network probe endpoint")?;
            // Wait until the home relay is connected so the endpoint is immediately
            // discoverable/dialable; this warmup counts toward setup_time, not TTFB.
            endpoint.online().await;
            endpoint
        }
    };
    Ok(endpoint)
}

/// Dial `target` on the probe ALPN and measure the run. Never returns an error:
/// a failure to establish (timeout, no path, refused) becomes a not-established
/// [`Measurement`] carrying the reason, so the caller always emits a row (a run
/// with no path is itself a Gate-A finding, spec §9).
///
/// `process_start` is the outer process-start instant, so `setup_time_ms` reflects
/// the spec's "process start → established" definition (incl. discovery/relay),
/// while TTFB is measured from the dial itself.
pub async fn dial_and_measure(
    endpoint: &Endpoint,
    target: EndpointAddr,
    params: DialParams,
    process_start: Instant,
) -> Measurement {
    match run_measurement(endpoint, target, params, process_start).await {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(%err, "probe: run did not establish");
            Measurement::failed(err.to_string())
        }
    }
}

/// The measurement body (errors bubble to a not-established [`Measurement`]).
async fn run_measurement(
    endpoint: &Endpoint,
    target: EndpointAddr,
    params: DialParams,
    process_start: Instant,
) -> Result<Measurement> {
    let remote = target.id;
    let dial_start = Instant::now();

    let conn = with_budget(params.budget, endpoint.connect(target, NAT_PROBE_ALPN))
        .await
        .context("connect")?
        .context("connect")?;
    let (mut send, mut recv) = with_budget(params.budget, conn.open_bi())
        .await
        .context("open_bi")?
        .context("open_bi")?;

    // TTFB: send the marker, read the first echoed byte back.
    send.write_all(&[MARKER_BYTE])
        .await
        .context("write TTFB marker")?;
    let mut one = [0u8; 1];
    with_budget(params.budget, recv.read_exact(&mut one))
        .await
        .context("TTFB read")?
        .context("TTFB read")?;
    let ttfb = dial_start.elapsed();
    let setup = process_start.elapsed();

    // Initial path sample (before the settle window) — captures a relay start that
    // later upgrades to direct (spec §6.2).
    let (initial_path_type, _) = sample_path(endpoint, remote).await;

    // RTT: `pings` application round-trips over the same stream.
    let mut rtts = Vec::with_capacity(params.pings as usize);
    for _ in 0..params.pings {
        let t = Instant::now();
        send.write_all(&[PING_BYTE]).await.context("write ping")?;
        with_budget(params.budget, recv.read_exact(&mut one))
            .await
            .context("ping read")?
            .context("ping read")?;
        rtts.push(t.elapsed());
    }
    let (rtt_median_ms, rtt_p90_ms) = summarize_rtt(&rtts);

    // Settle-and-sample: observe the path over the window, record the settled value.
    let settled = settle_path(endpoint, remote, params.settle).await;

    // Throughput on the settled path (concurrent write+read to avoid a QUIC
    // flow-control deadlock on the echoed bytes).
    let throughput_mbit_s = if params.xfer_bytes > 0 {
        Some(measure_throughput(&mut send, &mut recv, params.xfer_bytes, params.budget).await?)
    } else {
        None
    };

    // Clean finish.
    let _ = send.finish();
    conn.close(CLOSE_OK, b"probe-done");

    Ok(Measurement {
        established: true,
        setup_time_ms: Some(duration_ms(setup)),
        ttfb_ms: Some(duration_ms(ttfb)),
        rtt_median_ms,
        rtt_p90_ms,
        throughput_mbit_s,
        initial_path_type,
        path_type: settled.path_type,
        relay_url: settled.relay_url,
        remote_addrs: settled.remote_addrs,
        error: None,
    })
}

/// A settled path observation.
struct SettledPath {
    path_type: PathType,
    relay_url: Option<String>,
    remote_addrs: Vec<String>,
}

/// Observe the path over `settle`, returning the **last** reading (spec §6.2). A
/// stable `direct` is terminal-good, so we return as soon as it appears; otherwise
/// we keep re-sampling until the window elapses, so a relay→direct upgrade is
/// captured as the settled value.
async fn settle_path(endpoint: &Endpoint, remote: EndpointId, settle: Duration) -> SettledPath {
    let deadline = Instant::now() + settle;
    let mut last = full_sample(endpoint, remote).await;
    while last.path_type != PathType::Direct && Instant::now() < deadline {
        tokio::time::sleep(SETTLE_POLL).await;
        last = full_sample(endpoint, remote).await;
    }
    last
}

/// One full path sample: classification plus the resolved addr list.
async fn full_sample(endpoint: &Endpoint, remote: EndpointId) -> SettledPath {
    let info = endpoint.remote_info(remote).await;
    let (path_type, relay_url) = classify_remote_info(info.as_ref());
    let remote_addrs = info
        .as_ref()
        .map(|i| {
            i.addrs()
                .map(|a| format!("{} ({:?})", a.addr(), a.usage()))
                .collect()
        })
        .unwrap_or_default();
    SettledPath {
        path_type,
        relay_url,
        remote_addrs,
    }
}

/// Just the path type + relay url (used for the initial sample).
async fn sample_path(endpoint: &Endpoint, remote: EndpointId) -> (PathType, Option<String>) {
    let info = endpoint.remote_info(remote).await;
    classify_remote_info(info.as_ref())
}

/// Classify direct-vs-relay from the remote's **active** transport addresses — the
/// iroh 1.0.1 equivalent of the (absent) `ConnectionType` watcher (spec §6.2,
/// NOTES.md §2). An active IP addr ⇒ a direct hole-punched path; an active relay
/// addr with no active IP ⇒ relay-only; both ⇒ the transitional `mixed`; none ⇒
/// no usable path. The relay url is returned whenever a relay addr is present
/// (active preferred), so relay runs record where they homed.
pub(crate) fn classify_remote_info(info: Option<&RemoteInfo>) -> (PathType, Option<String>) {
    let Some(info) = info else {
        return (PathType::None, None);
    };
    let mut has_direct = false;
    let mut has_relay = false;
    let mut active_relay: Option<String> = None;
    let mut any_relay: Option<String> = None;
    for addr in info.addrs() {
        let active = matches!(addr.usage(), TransportAddrUsage::Active);
        match addr.addr() {
            TransportAddr::Ip(_) => has_direct |= active,
            TransportAddr::Relay(url) => {
                let url = url.to_string();
                if active {
                    has_relay = true;
                    active_relay.get_or_insert(url.clone());
                }
                any_relay.get_or_insert(url);
            }
            // `Custom` and any future non-exhaustive variant are neither a direct
            // IP path nor a relay path for classification purposes.
            _ => {}
        }
    }
    let path_type = match (has_direct, has_relay) {
        (true, true) => PathType::Mixed,
        (true, false) => PathType::Direct,
        (false, true) => PathType::Relay,
        (false, false) => PathType::None,
    };
    (path_type, active_relay.or(any_relay))
}

/// Stream a fixed payload and drain the echoed copy concurrently, returning
/// throughput in Mbit/s. The write and read run under one [`tokio::join`] so the
/// echoed bytes are drained while we send — writing all N bytes first would stall
/// on QUIC flow control once the echo backpressures.
async fn measure_throughput(
    send: &mut SendStream,
    recv: &mut RecvStream,
    n: usize,
    budget: Duration,
) -> Result<f64> {
    let payload = vec![PING_BYTE; n];
    let start = Instant::now();

    let write = async {
        send.write_all(&payload)
            .await
            .map_err(|e| anyhow!("throughput write: {e}"))
    };
    let read = async {
        let mut buf = vec![0u8; IO_CHUNK];
        let mut remaining = n;
        while remaining > 0 {
            let take = remaining.min(buf.len());
            match recv.read(&mut buf[..take]).await {
                Ok(Some(k)) => remaining -= k,
                Ok(None) => {
                    return Err(anyhow!(
                        "stream closed mid-transfer with {remaining} bytes left"
                    ))
                }
                Err(e) => return Err(anyhow!("throughput read: {e}")),
            }
        }
        Ok(())
    };

    let (w, r) = with_budget(budget, async { tokio::join!(write, read) })
        .await
        .context("throughput transfer")?;
    w?;
    r?;

    Ok(bytes_to_mbit(n, start.elapsed()))
}

/// Deterministically derive a 32-byte secret from a `u64` seed (for `--seed`
/// reproducibility of the probe identity). Genuine iroh-authenticated identity;
/// carries no room membership (spec §6.1 / §8).
#[must_use]
pub fn secret_from_seed(seed: u64) -> SecretKey {
    let mut bytes = [0u8; 32];
    let s = seed.to_le_bytes();
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&s);
    }
    SecretKey::from_bytes(&bytes)
}

/// Wrap a future in the per-op wait budget (spec §6.1).
async fn with_budget<F: std::future::Future>(budget: Duration, fut: F) -> Result<F::Output> {
    tokio::time::timeout(budget, fut)
        .await
        .map_err(|_| anyhow!("timed out after {budget:?}"))
}

/// Median + p90 of the RTT samples, in milliseconds (nearest-rank).
fn summarize_rtt(rtts: &[Duration]) -> (Option<f64>, Option<f64>) {
    if rtts.is_empty() {
        return (None, None);
    }
    let mut ms: Vec<f64> = rtts.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (Some(percentile(&ms, 50)), Some(percentile(&ms, 90)))
}

/// Nearest-rank percentile of a **sorted** slice (`p` in 1..=100).
fn percentile(sorted: &[f64], p: u32) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let len = u64::try_from(sorted.len()).unwrap_or(u64::MAX);
    // rank = ceil(p/100 * N), 1-based, clamped into range.
    let rank = (u64::from(p) * len).div_ceil(100).max(1);
    let idx = usize::try_from(rank - 1)
        .unwrap_or(usize::MAX)
        .min(sorted.len() - 1);
    sorted[idx]
}

/// Convert a byte count transferred in `elapsed` to Mbit/s. Precision loss on the
/// `usize → f64` byte count is irrelevant at Mbit granularity.
#[allow(clippy::cast_precision_loss)]
fn bytes_to_mbit(bytes: usize, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return 0.0;
    }
    (bytes as f64 * 8.0) / secs / 1_000_000.0
}

/// Milliseconds of a duration, saturating (never panics on an implausibly long run).
fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The first relay url in an [`EndpointAddr`], if any.
fn relay_url_of(addr: &EndpointAddr) -> Option<String> {
    addr.addrs.iter().find_map(|a| match a {
        TransportAddr::Relay(url) => Some(url.to_string()),
        _ => None,
    })
}

/// Build a loopback [`EndpointAddr`] (`id + 127.0.0.1:<bound port>`), bypassing
/// relay/DNS discovery (mirrors `iroh-rooms-net` / `spike-blobs`).
fn loopback_addr(endpoint: &Endpoint) -> Result<EndpointAddr> {
    let port = endpoint
        .bound_sockets()
        .into_iter()
        .map(|s| s.port())
        .next()
        .context("endpoint has no bound UDP socket")?;
    let socket = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    Ok(EndpointAddr::new(endpoint.id()).with_ip_addr(socket))
}

#[cfg(test)]
mod tests {
    use super::{
        bytes_to_mbit, classify_remote_info, duration_ms, percentile, secret_from_seed,
        summarize_rtt, DialParams, EndpointOpts, RelayStack, NAT_PROBE_ALPN,
    };
    use crate::report::PathType;
    use std::time::Duration;

    #[test]
    fn probe_alpn_is_the_dedicated_nat_probe_protocol() {
        // A distinct ALPN from the shipping event/pipe protocols keeps the probe off
        // room admission entirely (spec §8). Pin it so a rename is deliberate.
        assert_eq!(NAT_PROBE_ALPN, b"/iroh-rooms/nat-probe/1");
    }

    #[test]
    fn secret_from_seed_is_deterministic_and_seed_sensitive() {
        assert_eq!(
            secret_from_seed(7).public(),
            secret_from_seed(7).public(),
            "same seed must yield the same identity"
        );
        assert_ne!(
            secret_from_seed(1).public(),
            secret_from_seed(2).public(),
            "distinct seeds must yield distinct identities"
        );
    }

    #[test]
    fn percentile_nearest_rank() {
        let sorted = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert!((percentile(&sorted, 50) - 30.0).abs() < f64::EPSILON);
        assert!((percentile(&sorted, 90) - 50.0).abs() < f64::EPSILON);
        assert!((percentile(&sorted, 100) - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percentile_single_and_empty() {
        assert!((percentile(&[42.0], 90) - 42.0).abs() < f64::EPSILON);
        assert!((percentile(&[], 50) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summarize_rtt_empty_is_none() {
        let (m, p) = summarize_rtt(&[]);
        assert!(m.is_none() && p.is_none());
    }

    #[test]
    fn summarize_rtt_orders_before_percentile() {
        // Deliberately unsorted input; the summary must sort first.
        let samples = [
            Duration::from_millis(50),
            Duration::from_millis(10),
            Duration::from_millis(30),
        ];
        let (median, p90) = summarize_rtt(&samples);
        assert!((median.unwrap() - 30.0).abs() < 1e-6, "median = {median:?}");
        assert!((p90.unwrap() - 50.0).abs() < 1e-6, "p90 = {p90:?}");
    }

    #[test]
    fn bytes_to_mbit_basic_and_zero_time() {
        // 1 MiB in 1 s ≈ 8.389 Mbit/s.
        let mbit = bytes_to_mbit(1024 * 1024, Duration::from_secs(1));
        assert!((mbit - 8.388_608).abs() < 1e-3, "got {mbit}");
        // Zero elapsed must not divide-by-zero.
        assert!((bytes_to_mbit(1000, Duration::ZERO) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn duration_ms_saturates() {
        assert_eq!(duration_ms(Duration::from_millis(1500)), 1500);
        assert_eq!(duration_ms(Duration::ZERO), 0);
    }

    // --- classify_remote_info: the load-bearing instrumentation (spec §6.2) ---

    #[test]
    fn classify_remote_info_none_is_no_path() {
        // `None` from `Endpoint::remote_info` means iroh has no record of the peer —
        // no path, no relay url. This is the NO-GO signal (spec §9).
        let (pt, relay_url) = classify_remote_info(None);
        assert_eq!(pt, PathType::None, "unknown peer must classify as no path");
        assert!(relay_url.is_none(), "unknown peer has no relay url");
    }

    // --- DialParams defaults must match spec §6.3 ---

    #[test]
    fn dial_params_default_matches_spec() {
        // The spec §6.3 mandates these exact values. Pin them so a casual tweak is
        // a deliberate, documented change — not an accidental drift.
        let p = DialParams::default();
        assert_eq!(p.pings, 20, "spec §6.3: 20 pings for the RTT estimate");
        assert_eq!(
            p.xfer_bytes,
            8 * 1024 * 1024,
            "spec §6.3: 8 MiB throughput transfer"
        );
        assert_eq!(
            p.settle,
            Duration::from_secs(4),
            "spec §6.2: 4 s hole-punch upgrade window"
        );
        assert_eq!(
            p.budget,
            Duration::from_secs(30),
            "spec §6.1: 30 s hang budget (bounds a hang, not the measured number)"
        );
    }

    // --- EndpointOpts builder variants ---

    #[test]
    fn endpoint_opts_loopback_has_expected_fields() {
        // Loopback is the offline self-check stack — no relay, no discovery.
        let opts = EndpointOpts::loopback();
        assert_eq!(opts.stack, RelayStack::Loopback);
        assert!(!opts.relay_only, "loopback has no relay to pin to");
    }

    #[test]
    fn endpoint_opts_real_has_expected_fields() {
        let opts = EndpointOpts::real();
        assert_eq!(opts.stack, RelayStack::RealNetwork);
        assert!(!opts.relay_only, "natural run allows direct paths");
    }

    #[test]
    fn endpoint_opts_real_relay_only_suppresses_direct() {
        // relay_only=true triggers clear_ip_transports — the controlled relay
        // measurement knob (spec §6.4). Confirm the builder sets it correctly.
        let opts = EndpointOpts::real_relay_only();
        assert_eq!(opts.stack, RelayStack::RealNetwork);
        assert!(opts.relay_only, "relay_only must suppress direct UDP paths");
    }

    // --- duration_ms saturation on extreme input ---

    #[test]
    fn duration_ms_saturates_on_max_duration() {
        // Duration::MAX overflows u128→u64; the conversion must saturate, not panic.
        let ms = duration_ms(Duration::MAX);
        assert_eq!(ms, u64::MAX, "must saturate not overflow on Duration::MAX");
    }

    // --- percentile edge cases ---

    #[test]
    fn percentile_p1_returns_first_element() {
        // rank = ceil(1/100 * 5) = 1 → idx 0
        let sorted = [1.0_f64, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile(&sorted, 1) - 1.0).abs() < f64::EPSILON);
    }

    // --- summarize_rtt edge cases ---

    #[test]
    fn summarize_rtt_single_sample_median_and_p90_are_equal() {
        // With one sample there is only one rank regardless of percentile.
        let (median, p90) = summarize_rtt(&[Duration::from_millis(42)]);
        assert!((median.unwrap() - 42.0).abs() < f64::EPSILON);
        assert!((p90.unwrap() - 42.0).abs() < f64::EPSILON);
    }

    // --- bytes_to_mbit precision ---

    #[test]
    fn bytes_to_mbit_100_mib_per_second() {
        // 100 MiB in 1 s = 104857600 bytes × 8 / 1_000_000 = 838.8608 Mbit/s.
        let mbit = bytes_to_mbit(100 * 1024 * 1024, Duration::from_secs(1));
        assert!((mbit - 838.860_8).abs() < 0.01, "got {mbit}");
    }
}
