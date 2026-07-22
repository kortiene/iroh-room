//! The connector ("connect") side of the Live Pipe Plane (spec §6.5.2).
//!
//! [`connect`] binds a **loopback-only** local TCP listener (`127.0.0.1:<port>`,
//! never `0.0.0.0` — D6), dials the owner once over the pipe ALPN (one reused QUIC
//! connection, D4), and per accepted local TCP connection opens a bidi stream,
//! writes the [`PipeHello`], reads the owner's 1-byte gate reply, and on accept
//! splices local TCP ↔ QUIC. A reject reply (or an owner-side connection drop)
//! surfaces a [`PipeOutcome::Denied`] and closes the local socket — the connector
//! never forwards a byte the owner refused.
//!
//! Because all of a connector's forwards multiplex over that one connection, they
//! share one QUIC stream budget ([`PIPE_MAX_CONCURRENT_FORWARDS`]). The connector
//! owns that budget explicitly rather than letting the transport enforce it silently:
//! see [`PipeOutcome::Saturated`].

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

use super::error::PipeError;
use super::hello::{PipeHello, PIPE_ACCEPT};
use super::splice::splice;

/// How many local TCP connections one connector forwards concurrently over its single
/// reused QUIC connection (D4).
///
/// Each forward consumes exactly one bidi stream held open for the life of the splice,
/// so this is the connector's share of the connection's stream budget. It keeps the
/// 100 the scale run measured as the plane's real working capacity, but it is now a
/// *chosen* number the connector enforces itself rather than a transport default it
/// blocks against: over-budget connections are refused with [`PipeOutcome::Saturated`]
/// instead of parking in `open_bi()`.
///
/// It must stay strictly below [`MAX_CONCURRENT_BIDI_STREAMS`] so that this budget, not
/// QUIC's, is the binding constraint — see that constant for why the gap is required
/// rather than cosmetic.
pub const PIPE_MAX_CONCURRENT_FORWARDS: usize = 100;

/// Backstop deadline on `open_bi()` for a forward that *did* win a local permit.
///
/// The permit budget is the primary guard and is exact against a peer running this
/// same build. This deadline covers what the local count cannot see — a peer that
/// advertises a smaller stream limit than we assume — so that **no** forwarding path
/// can park indefinitely, which is the failure the scale run observed. It is a
/// liveness backstop, not a latency policy, hence a deliberately loose value.
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The outcome of one forwarded local TCP connection (for observability / tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeOutcome {
    /// The owner accepted: bytes were spliced end to end.
    Forwarded,
    /// The owner denied the stream at the gate (per-pipe authorization).
    Denied,
    /// The owner connection dropped before/while forwarding (offline / torn down).
    OwnerClosed,
    /// The connector's concurrent-forward budget
    /// ([`PIPE_MAX_CONCURRENT_FORWARDS`]) is exhausted: the local socket was closed
    /// **immediately** rather than accepted and starved.
    ///
    /// This is the outcome the plane previously had no way to express. A scale run on
    /// real hardware pushed 150 concurrent forwards through one connector; 100 got
    /// streams and the remaining 50 blocked forever inside `open_bi()`, emitting no
    /// error, no timeout, and no outcome. Because the stream never opened, no
    /// [`PipeHello`] was ever written, so the owner could not log, audit, or even in
    /// principle observe the overload — from the client's side the local TCP
    /// connection was accepted and then simply never carried a byte. Saturation is a
    /// capacity fact, not a gate denial, so it is deliberately distinct from
    /// [`Denied`](Self::Denied): the operator's remedy is a second connector or fewer
    /// concurrent clients, not an authorization change.
    Saturated,
    /// A local error opening or driving the stream.
    Error(String),
}

/// A running connector: a loopback listener forwarding over one QUIC connection to
/// the owner. Dropping it stops accepting and closes the owner connection.
pub struct PipeForwarder {
    local_addr: SocketAddr,
    conn: Connection,
    accept_task: JoinHandle<()>,
    /// Local-loopback outcome reporting for the CLI / tests. Allowlisted under
    /// #141's grep audit (spec D8.4): every producer is a `forward_one` task
    /// that first took a permit from the `PIPE_MAX_CONCURRENT_FORWARDS`
    /// semaphore before it could send, so this queue's high-water mark is
    /// bounded by that semaphore even though the channel itself is unbounded.
    /// No remote peer writes to it directly.
    outcomes: mpsc::UnboundedReceiver<PipeOutcome>,
}

impl PipeForwarder {
    /// The bound local address callers connect to (with the OS-assigned port when
    /// `0` was requested).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Await the next per-connection outcome (tests / a CLI status line). Returns
    /// `None` once the connector has stopped and all outcomes are drained.
    pub async fn next_outcome(&mut self) -> Option<PipeOutcome> {
        self.outcomes.recv().await
    }

    /// Stop forwarding: abort the accept loop and close the owner connection.
    pub fn shutdown(self) {
        self.accept_task.abort();
        self.conn.close(VarInt::from_u32(0), b"connector-shutdown");
    }
}

impl Drop for PipeForwarder {
    fn drop(&mut self) {
        self.accept_task.abort();
        // Best-effort connection close; the owner observes the drop and tears down.
        self.conn.close(VarInt::from_u32(0), b"connector-dropped");
    }
}

/// Bring up a connector: bind `127.0.0.1:local_port`, dial `owner_addr` over `alpn`,
/// and forward each accepted local TCP connection over the pipe.
///
/// # Errors
/// [`PipeError::Io`] if the loopback listener cannot bind, or
/// [`PipeError::OwnerUnreachable`] if the owner cannot be dialed.
pub async fn connect(
    endpoint: &Endpoint,
    owner_addr: EndpointAddr,
    pipe_id: [u8; 16],
    alpn: &[u8],
    local_port: u16,
) -> Result<PipeForwarder, PipeError> {
    // Loopback-only bind (D6): the tunnel mouth stays private to this host.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local_port))
        .await
        .map_err(|e| PipeError::Io(e.to_string()))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| PipeError::Io(e.to_string()))?;

    // One reused QUIC connection to the owner (D4).
    let conn = endpoint
        .connect(owner_addr, alpn)
        .await
        .map_err(|e| PipeError::OwnerUnreachable(e.to_string()))?;

    // Local-loopback outcome queue: see `PipeForwarder::outcomes` for the
    // #141 grep allowlist rationale (bounded by `PIPE_MAX_CONCURRENT_FORWARDS`).
    let (outcome_tx, outcome_rx) = mpsc::unbounded_channel();
    let accept_task = tokio::spawn(accept_loop(listener, conn.clone(), pipe_id, outcome_tx));

    Ok(PipeForwarder {
        local_addr,
        conn,
        accept_task,
        outcomes: outcome_rx,
    })
}

/// Accept local TCP connections forever, forwarding each over a fresh bidi stream.
///
/// Every forward must first take a permit from the connection's stream budget
/// ([`PIPE_MAX_CONCURRENT_FORWARDS`]). `try_acquire_owned` never waits, so an
/// over-budget connection is refused here — before `open_bi()`, the call that used to
/// park — and the local socket is closed by dropping `tcp` on the spot.
async fn accept_loop(
    listener: TcpListener,
    conn: Connection,
    pipe_id: [u8; 16],
    outcome_tx: mpsc::UnboundedSender<PipeOutcome>,
) {
    let budget = Arc::new(Semaphore::new(PIPE_MAX_CONCURRENT_FORWARDS));
    loop {
        match listener.accept().await {
            Ok((tcp, _peer)) => {
                let Ok(permit) = budget.clone().try_acquire_owned() else {
                    // WARN with a stable, greppable reason, matching the pipe audit
                    // vocabulary: saturation is an operational fact worth an alert.
                    tracing::warn!(
                        reason = "pipe.connect.saturated",
                        limit = PIPE_MAX_CONCURRENT_FORWARDS,
                        "pipe connector: concurrent-forward budget exhausted; refusing"
                    );
                    let _ = outcome_tx.send(PipeOutcome::Saturated);
                    drop(tcp); // close the local socket now — never accept-then-starve
                    continue;
                };
                let conn = conn.clone();
                let outcome_tx = outcome_tx.clone();
                tokio::spawn(forward_one(conn, tcp, pipe_id, outcome_tx, permit));
            }
            Err(err) => {
                tracing::debug!(%err, "pipe connector: local accept failed; stopping");
                break;
            }
        }
    }
}

/// Forward one local TCP connection: open a stream, hello, read the gate reply, and
/// splice on accept.
///
/// `_permit` holds this forward's slot in the connector's stream budget for the whole
/// call — including the splice, which is when the bidi stream is actually live — and
/// releases it on every exit path by being dropped.
async fn forward_one(
    conn: Connection,
    tcp: TcpStream,
    pipe_id: [u8; 16],
    outcome_tx: mpsc::UnboundedSender<PipeOutcome>,
    _permit: OwnedSemaphorePermit,
) {
    let (mut send, mut recv) = match tokio::time::timeout(STREAM_OPEN_TIMEOUT, conn.open_bi()).await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(err)) => {
            let _ = outcome_tx.send(PipeOutcome::OwnerClosed);
            tracing::debug!(%err, "pipe connector: open_bi failed (owner closed?)");
            return;
        }
        Err(_elapsed) => {
            // Held a local permit but still got no stream credit: the owner's real
            // limit is below ours. Same operator-visible fact, same outcome.
            tracing::warn!(
                reason = "pipe.connect.saturated",
                limit = PIPE_MAX_CONCURRENT_FORWARDS,
                "pipe connector: no stream credit within the open deadline; refusing"
            );
            let _ = outcome_tx.send(PipeOutcome::Saturated);
            return;
        }
    };

    if let Err(err) = PipeHello::new(pipe_id).write_to(&mut send).await {
        let _ = outcome_tx.send(PipeOutcome::Error(err.to_string()));
        return;
    }

    // Read the owner's 1-byte gate reply. A failed read means the owner closed the
    // stream/connection without accepting (stage-1 reject or teardown).
    let mut reply = [0u8; 1];
    match recv.read_exact(&mut reply).await {
        Ok(()) if reply[0] == PIPE_ACCEPT => {
            let _ = outcome_tx.send(PipeOutcome::Forwarded);
            splice(send, recv, tcp).await;
        }
        Ok(()) => {
            // Explicit reject byte (PIPE_REJECT) — denied at the per-pipe gate.
            let _ = outcome_tx.send(PipeOutcome::Denied);
        }
        Err(err) => {
            tracing::debug!(?err, "pipe connector: no gate reply (owner refused/closed)");
            let _ = outcome_tx.send(PipeOutcome::OwnerClosed);
        }
    }
    // `tcp` is dropped here on every non-forwarding path, closing the local socket.
}

/// Build a loopback [`EndpointAddr`] for an `EndpointId` + `SocketAddr` (the
/// connector dials this owner address out-of-band, as the event plane does).
#[must_use]
pub fn loopback_owner_addr(id: iroh::EndpointId, socket: SocketAddr) -> EndpointAddr {
    EndpointAddr::new(id).with_ip_addr(socket)
}

#[cfg(test)]
mod tests {
    use super::PipeOutcome;
    use crate::pipe::connector::PIPE_MAX_CONCURRENT_FORWARDS;
    use crate::transport::MAX_CONCURRENT_BIDI_STREAMS;

    #[test]
    fn pipe_outcome_variants_are_distinct() {
        assert_ne!(PipeOutcome::Forwarded, PipeOutcome::Denied);
        assert_ne!(PipeOutcome::Denied, PipeOutcome::OwnerClosed);
        assert_ne!(
            PipeOutcome::Error("x".into()),
            PipeOutcome::Error("y".into())
        );
    }

    #[test]
    fn saturated_is_distinct_from_every_other_outcome() {
        // Saturation is a capacity fact, not a gate denial and not an owner-side
        // close; a caller (the CLI status line, an audit parser) must be able to tell
        // "too many concurrent clients" from "you are not authorized".
        assert_ne!(PipeOutcome::Saturated, PipeOutcome::Forwarded);
        assert_ne!(PipeOutcome::Saturated, PipeOutcome::Denied);
        assert_ne!(PipeOutcome::Saturated, PipeOutcome::OwnerClosed);
        assert_ne!(
            PipeOutcome::Saturated,
            PipeOutcome::Error("saturated".into())
        );
    }

    #[test]
    fn forward_budget_stays_below_the_advertised_bidi_stream_limit() {
        // The ordering is load-bearing, not incidental. At or above the transport
        // limit the connector's refusal stops being the binding constraint and
        // `open_bi()` starts parking against QUIC's batched stream credit again —
        // exactly the silent stall this guard exists to eliminate.
        assert!(
            PIPE_MAX_CONCURRENT_FORWARDS < MAX_CONCURRENT_BIDI_STREAMS as usize,
            "the connector budget must leave stream-credit headroom"
        );
    }
}
