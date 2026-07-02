//! The minimal full-mesh backend (spec §7.2).
//!
//! Deliberately minimal for parity with the gossip prototype — the *shipping*
//! carrier (`iroh-rooms-net`, IR-0005) already exists and is **not** modified;
//! this is the comparison twin for the head-to-head latency + complexity
//! numbers and a self-contained admission demonstration. Reconnect/backoff and
//! the production admission gate are already proven in `iroh-rooms-net`'s
//! `tests/loopback.rs` (T2 admission-before-bytes, T4 reconnect); this spike
//! cites that evidence rather than re-implementing a dial-with-backoff loop
//! (spec §4 / §7.2 "cross-reference, do not re-verify").
//!
//! - One `iroh::Endpoint` + `Router` on [`SPIKE_EVENT_ALPN`] (a **spike-only**
//!   ALPN, distinct from the shipping `/iroh-rooms/event/1`, so this backend
//!   can never be mistaken for the real event plane).
//! - The accept-gate authorizes the QUIC/TLS-proven `Connection::remote_id()`
//!   against a fixed allowlist and **closes before `accept_bi()`** for a
//!   non-member (admission-before-bytes, AC3).
//! - Every member pair gets exactly one bidi stream (node `i` dials node `j`
//!   for `j > i`); each frame is a verbatim `WireEvent` byte string, deduped by
//!   the recomputed `event_id`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use iroh::endpoint::{
    presets, ApplicationClose, Connection, ConnectionError, RecvStream, SendStream, VarInt,
};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_rooms_core::event::signed::event_id_from_bytes;
use iroh_rooms_core::event::{EventId, WireEvent};
use iroh_rooms_core::sync::SyncMessage;
use tokio::sync::mpsc;

use crate::{BackendEvent, BackendKind, TransportBackend, WireBytes};

/// The spike-only ALPN for the minimal mesh backend.
pub const SPIKE_EVENT_ALPN: &[u8] = b"/iroh-rooms/spike-event/1";

/// Close code for a deliberate admission reject (distinct from a normal
/// application close, mirrors `iroh-rooms-net::handler::REJECT_CODE`).
const REJECT_CODE: VarInt = VarInt::from_u32(0x5350_4b01); // "SPK\x01"
/// Maximum accepted frame body length (parity with the shipping carrier's
/// 1 MiB guard).
const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Frame tag: the body is a verbatim `WireEvent` byte string.
const TAG_EVENT: u8 = 0;
/// Frame tag: the body is a canonical-CBOR-encoded `SyncMessage` (the
/// Residual-13 admin-tip-carrier probe, spec §7.7). A second frame kind on the
/// same link — never a second mechanism — so the mesh admin-tip prototype is
/// "just another frame on a connection we already hold" (ADR-1 §4).
const TAG_CONTROL: u8 = 1;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    allowed: HashSet<EndpointId>,
    received: Mutex<BTreeSet<EventId>>,
    events: Mutex<Vec<BackendEvent>>,
    outbound: Mutex<HashMap<EndpointId, mpsc::UnboundedSender<Vec<u8>>>>,
    /// `SyncMessage`s received via a [`TAG_CONTROL`] frame (§7.7 admin-tip
    /// probe only; the workload path never uses this).
    control: Mutex<Vec<SyncMessage>>,
}

impl Inner {
    /// Recompute the `event_id` of a raw frame and record it — never trust an
    /// advisory id off the wire (Event Protocol §4/§6).
    fn record_frame(&self, bytes: &[u8]) {
        if let Ok(wire) = WireEvent::decode(bytes) {
            let id = event_id_from_bytes(&wire.signed);
            lock(&self.received).insert(id);
        }
    }
}

/// A minimal full-mesh node.
pub struct MeshNode {
    inner: Arc<Inner>,
    endpoint: Endpoint,
    router: Router,
}

impl MeshNode {
    /// Bind a loopback mesh node whose accept-gate admits exactly `allowed`
    /// device ids — a fixed peer set for this comparison, not a live
    /// membership fold (spec §2 non-goals: ordering/auth-as-state belongs to
    /// the log layer, not the transport).
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn spawn(secret: SecretKey, allowed: HashSet<EndpointId>) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .context("bind loopback mesh endpoint")?;
        let inner = Arc::new(Inner {
            allowed,
            received: Mutex::new(BTreeSet::new()),
            events: Mutex::new(Vec::new()),
            outbound: Mutex::new(HashMap::new()),
            control: Mutex::new(Vec::new()),
        });
        let router = Router::builder(endpoint.clone())
            .accept(
                SPIKE_EVENT_ALPN,
                MeshHandler {
                    inner: inner.clone(),
                },
            )
            .spawn();
        Ok(Self {
            inner,
            endpoint,
            router,
        })
    }

    /// This node's authenticated identity.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// A loopback dial address for this node.
    ///
    /// # Errors
    /// Returns an error if the endpoint has no bound UDP socket.
    pub fn addr(&self) -> Result<EndpointAddr> {
        loopback_addr(&self.endpoint)
    }

    /// Dial `addr` and register the resulting bidi link (both dial-all
    /// bring-up and the admission-probe interloper use this path).
    ///
    /// # Errors
    /// Returns an error if the dial or `open_bi()` fails.
    pub async fn dial(&self, addr: EndpointAddr) -> Result<()> {
        let conn = self
            .endpoint
            .connect(addr, SPIKE_EVENT_ALPN)
            .await
            .context("connect")?;
        let (send, recv) = conn.open_bi().await.context("open_bi")?;
        register(&self.inner, conn.remote_id(), send, recv);
        Ok(())
    }

    /// Gracefully stop this node.
    ///
    /// # Errors
    /// Returns an error if the router shutdown fails to join.
    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.context("router shutdown")?;
        Ok(())
    }

    /// Send a `SyncMessage` (e.g. `AdminTip`) to `peer` as a tagged control
    /// frame on the already-established bidi link (the mesh side of the
    /// Residual-13 admin-tip-carrier probe, spec §7.7) — no new mechanism, the
    /// same connection the event workload uses.
    ///
    /// # Errors
    /// Returns an error if no link to `peer` is currently registered.
    pub fn send_control(&self, peer: EndpointId, msg: &SyncMessage) -> Result<()> {
        let mut framed = Vec::with_capacity(1 + 64);
        framed.push(TAG_CONTROL);
        framed.extend_from_slice(&msg.encode());
        let guard = lock(&self.inner.outbound);
        let tx = guard
            .get(&peer)
            .with_context(|| format!("no link registered to peer {peer}"))?;
        tx.send(framed)
            .map_err(|_| anyhow::anyhow!("peer {peer} writer task already ended"))
    }

    /// Drain every `SyncMessage` received via a control frame since the last
    /// call (§7.7 admin-tip probe).
    #[must_use]
    pub fn drain_control(&self) -> Vec<SyncMessage> {
        std::mem::take(&mut lock(&self.inner.control))
    }
}

#[async_trait]
impl TransportBackend for MeshNode {
    fn kind(&self) -> BackendKind {
        BackendKind::Mesh
    }

    async fn publish(&self, wire: WireBytes) -> anyhow::Result<()> {
        // A node holds what it authors, not only what a peer sends it (the
        // equality oracle needs every node, including the publisher, to
        // converge on the same set).
        self.inner.record_frame(&wire);
        let mut framed = Vec::with_capacity(1 + wire.len());
        framed.push(TAG_EVENT);
        framed.extend_from_slice(&wire);
        let queues: Vec<_> = lock(&self.inner.outbound).values().cloned().collect();
        for tx in queues {
            // Unbounded, non-blocking: a send error only means that peer's
            // writer task already ended (link down).
            let _ = tx.send(framed.clone());
        }
        Ok(())
    }

    fn received_ids(&self) -> BTreeSet<EventId> {
        lock(&self.inner.received).clone()
    }

    fn drain_events(&self) -> Vec<BackendEvent> {
        std::mem::take(&mut lock(&self.inner.events))
    }
}

/// Wire up a live bidi link: register the outbound queue and spawn the reader
/// and writer tasks. Called by both the accept handler (inbound) and
/// [`MeshNode::dial`] (outbound).
fn register(inner: &Arc<Inner>, device: EndpointId, send: SendStream, recv: RecvStream) {
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    lock(&inner.outbound).insert(device, tx);
    tokio::spawn(writer_task(send, rx));
    tokio::spawn(reader_task(inner.clone(), device, recv));
}

async fn writer_task(mut send: SendStream, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(body) = rx.recv().await {
        if write_frame(&mut send, &body).await.is_err() {
            break;
        }
    }
    let _ = send.finish();
}

async fn reader_task(inner: Arc<Inner>, device: EndpointId, mut recv: RecvStream) {
    loop {
        match read_frame(&mut recv).await {
            Ok(Some(bytes)) if bytes.is_empty() => {}
            Ok(Some(bytes)) => match bytes[0] {
                TAG_CONTROL => {
                    if let Ok(msg) = SyncMessage::decode(&bytes[1..]) {
                        lock(&inner.control).push(msg);
                    }
                }
                _ => inner.record_frame(&bytes[1..]),
            },
            Ok(None) | Err(_) => break,
        }
    }
    lock(&inner.outbound).remove(&device);
    lock(&inner.events).push(BackendEvent::LinkDropped);
}

/// The accept-side admission gate for [`SPIKE_EVENT_ALPN`] — resolves the
/// QUIC/TLS-proven `remote_id()` against the fixed allowlist and closes
/// **before** `accept_bi()` for a non-member (AC3).
struct MeshHandler {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MeshHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MeshHandler")
    }
}

impl ProtocolHandler for MeshHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let device = conn.remote_id();
        if !self.inner.allowed.contains(&device) {
            lock(&self.inner.events).push(BackendEvent::AdmissionRejected);
            conn.close(REJECT_CODE, b"unauthorized");
            return Ok(());
        }
        let (send, recv) = conn.accept_bi().await?;
        register(&self.inner, device, send, recv);
        conn.closed().await;
        Ok(())
    }
}

/// Deterministically derive a 32-byte secret from a `u64` seed (reproducible
/// identities, mirrors `spike-nat::probe::secret_from_seed`).
#[must_use]
pub fn secret_from_seed(seed: u64) -> SecretKey {
    let mut bytes = [0u8; 32];
    let s = seed.to_le_bytes();
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&s);
    }
    SecretKey::from_bytes(&bytes)
}

/// Bring up `n` mesh nodes on loopback, forming a full mesh: node `i` dials
/// every node `j > i`, so every pair gets exactly one symmetric bidi link.
///
/// # Errors
/// Returns an error if any bind or dial fails.
pub async fn spawn_full_mesh(n: usize, seed_base: u64) -> Result<Vec<Arc<MeshNode>>> {
    let secrets: Vec<SecretKey> = (0..n)
        .map(|i| secret_from_seed(seed_base + i as u64))
        .collect();
    let allowed: HashSet<EndpointId> = secrets.iter().map(SecretKey::public).collect();

    let mut nodes = Vec::with_capacity(n);
    for secret in secrets {
        nodes.push(Arc::new(MeshNode::spawn(secret, allowed.clone()).await?));
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let addr = nodes[j].addr()?;
            nodes[i].dial(addr).await?;
        }
    }
    Ok(nodes)
}

/// The mesh side of the AC3 admission probe (spec §7.6/§8): connect to
/// `addr` as a device that is **not** in the victim's allowlist and confirm
/// the connection is refused before any event byte could flow either way.
///
/// # Errors
/// Returns an error if the endpoint fails to bind, the dial itself fails
/// outright, or — the actual admission-probe failure mode — the remote does
/// **not** produce the expected admission-reject close (i.e. it incorrectly
/// admitted this unauthorized identity).
pub async fn probe_admission_rejects_interloper(
    interloper_secret: SecretKey,
    victim_addr: EndpointAddr,
) -> Result<()> {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(interloper_secret)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .context("bind interloper endpoint")?;
    let conn = endpoint
        .connect(victim_addr, SPIKE_EVENT_ALPN)
        .await
        .context("interloper connect")?;
    // The victim's accept-gate closes immediately (before accept_bi()); wait
    // for that close and confirm it carries the admission-reject code.
    conn.closed().await;
    let refused = matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. }))
            if error_code == REJECT_CODE
    );
    endpoint.close().await;
    if refused {
        Ok(())
    } else {
        bail!("interloper connection did not observe the expected admission-reject close");
    }
}

/// Write one length-prefixed frame (4-byte BE length + body) to `stream`
/// (minimal parity with `iroh-rooms-net::frame`, inlined here to keep the
/// backend self-contained and comparably minimal to the gossip prototype).
async fn write_frame(stream: &mut SendStream, body: &[u8]) -> Result<()> {
    let len = u32::try_from(body.len()).context("frame body too large to prefix")?;
    if len > MAX_FRAME_BYTES {
        bail!("frame body {len} exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})");
    }
    stream
        .write_all(&len.to_be_bytes())
        .await
        .context("write frame prefix")?;
    stream.write_all(body).await.context("write frame body")?;
    Ok(())
}

/// Read one length-prefixed frame from `stream`. Returns `Ok(None)` on a
/// clean end-of-stream at a frame boundary.
async fn read_frame(stream: &mut RecvStream) -> Result<Option<Vec<u8>>> {
    use iroh::endpoint::ReadExactError;

    let mut prefix = [0u8; 4];
    match stream.read_exact(&mut prefix).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => bail!("frame prefix read failed: {e}"),
    }
    let len = u32::from_be_bytes(prefix);
    if len > MAX_FRAME_BYTES {
        bail!("declared frame body {len} exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})");
    }
    let mut body = vec![0u8; len as usize];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| anyhow::anyhow!("frame body read failed: {e}"))?;
    Ok(Some(body))
}

/// Lines of implementation code in this file (excludes the `#[cfg(test)]`
/// module) — the complexity-dimension proxy (spec §6).
#[must_use]
pub fn loc() -> usize {
    let src = include_str!("mesh.rs");
    src.split("#[cfg(test)]")
        .next()
        .unwrap_or(src)
        .lines()
        .count()
}

/// Build a loopback [`EndpointAddr`] (`id + 127.0.0.1:<bound port>`), bypassing
/// relay/DNS discovery (mirrors `iroh-rooms-net` / `spike-nat`).
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
    use super::secret_from_seed;

    #[test]
    fn secret_from_seed_is_deterministic_and_seed_sensitive() {
        assert_eq!(secret_from_seed(7).public(), secret_from_seed(7).public());
        assert_ne!(secret_from_seed(1).public(), secret_from_seed(2).public());
    }

    #[test]
    fn spike_alpn_is_distinct_from_shipping_alpn() {
        assert_ne!(super::SPIKE_EVENT_ALPN, b"/iroh-rooms/event/1");
        assert_eq!(super::SPIKE_EVENT_ALPN, b"/iroh-rooms/spike-event/1");
    }

    #[test]
    fn loc_is_a_positive_count_bounded_by_the_file_length() {
        // `loc()` is the complexity-dimension input feeding
        // `ComparisonResult.backend_loc`; it must report a positive line count
        // no larger than the whole file (it trims at the first `#[cfg(test)]`),
        // never zero or a panic.
        let total = include_str!("mesh.rs").lines().count();
        let impl_loc = super::loc();
        assert!(impl_loc > 0, "complexity proxy must be a positive count");
        assert!(
            impl_loc <= total,
            "loc() ({impl_loc}) cannot exceed the file's line count ({total})"
        );
    }
}
