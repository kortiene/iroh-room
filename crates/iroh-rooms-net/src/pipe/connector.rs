//! The connector ("connect") side of the Live Pipe Plane (spec §6.5.2).
//!
//! [`connect`] binds a **loopback-only** local TCP listener (`127.0.0.1:<port>`,
//! never `0.0.0.0` — D6), dials the owner once over the pipe ALPN (one reused QUIC
//! connection, D4), and per accepted local TCP connection opens a bidi stream,
//! writes the [`PipeHello`], reads the owner's 1-byte gate reply, and on accept
//! splices local TCP ↔ QUIC. A reject reply (or an owner-side connection drop)
//! surfaces a [`PipeOutcome::Denied`] and closes the local socket — the connector
//! never forwards a byte the owner refused.

use std::net::{Ipv4Addr, SocketAddr};

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::error::PipeError;
use super::hello::{PipeHello, PIPE_ACCEPT};
use super::splice::splice;

/// The outcome of one forwarded local TCP connection (for observability / tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeOutcome {
    /// The owner accepted: bytes were spliced end to end.
    Forwarded,
    /// The owner denied the stream at the gate (per-pipe authorization).
    Denied,
    /// The owner connection dropped before/while forwarding (offline / torn down).
    OwnerClosed,
    /// A local error opening or driving the stream.
    Error(String),
}

/// A running connector: a loopback listener forwarding over one QUIC connection to
/// the owner. Dropping it stops accepting and closes the owner connection.
pub struct PipeForwarder {
    local_addr: SocketAddr,
    conn: Connection,
    accept_task: JoinHandle<()>,
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
async fn accept_loop(
    listener: TcpListener,
    conn: Connection,
    pipe_id: [u8; 16],
    outcome_tx: mpsc::UnboundedSender<PipeOutcome>,
) {
    loop {
        match listener.accept().await {
            Ok((tcp, _peer)) => {
                let conn = conn.clone();
                let outcome_tx = outcome_tx.clone();
                tokio::spawn(forward_one(conn, tcp, pipe_id, outcome_tx));
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
async fn forward_one(
    conn: Connection,
    tcp: TcpStream,
    pipe_id: [u8; 16],
    outcome_tx: mpsc::UnboundedSender<PipeOutcome>,
) {
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(pair) => pair,
        Err(err) => {
            let _ = outcome_tx.send(PipeOutcome::OwnerClosed);
            tracing::debug!(%err, "pipe connector: open_bi failed (owner closed?)");
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

    #[test]
    fn pipe_outcome_variants_are_distinct() {
        assert_ne!(PipeOutcome::Forwarded, PipeOutcome::Denied);
        assert_ne!(PipeOutcome::Denied, PipeOutcome::OwnerClosed);
        assert_ne!(
            PipeOutcome::Error("x".into()),
            PipeOutcome::Error("y".into())
        );
    }
}
