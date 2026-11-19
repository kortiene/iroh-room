//! [`PipeProtocolHandler`] — the owner's two-stage accept gate for the pipe ALPN
//! (spec §4.3 / §6.5.1; `PHASE-0-SPIKE.md` Membership §5).
//!
//! Registered as the **second** `.accept()` chain on the shared `Router` (the event
//! ALPN is the first), so one `Endpoint` serves both planes. Its
//! [`accept`](ProtocolHandler::accept) enforces:
//!
//! * **Stage 1 — transport admission** (before any bidi stream): resolve the
//!   QUIC/TLS-proven `device_id` against the **current** snapshot
//!   (`device → identity → Active?`). A non-member / non-Active device is closed
//!   with [`PIPE_REJECT_CODE`](super::alpn::PIPE_REJECT_CODE) **before** `accept_bi()`
//!   — its handshake byte is never read (AC3).
//! * **Stage 2 — per-pipe authorization** (per accepted stream): read the
//!   [`PipeHello`], run the composed [`gate::evaluate`], and only on `Accept` write
//!   the accept reply byte and splice to the loopback target. A reject writes the
//!   reject byte and forwards **nothing** (AC2).
//!
//! Each accepted+gated stream registers a live session in [`PipeSessions`] so the
//! teardown watcher can sever it on revocation (spec §4.5).

use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::EndpointId;
use iroh_rooms_core::event::keys::DeviceKey;
use tokio::net::TcpStream;

use super::alpn::PIPE_REJECT_CODE;
use super::audit::{PipeAuditSink, PipeDenyCause};
use super::gate::{self, PipeGateVerdict};
use super::hello::{PipeHello, PIPE_ACCEPT, PIPE_REJECT};
use super::registry::PipeRegistry;
use super::runtime::PipeQuery;
use super::sessions::PipeSessions;
use super::splice::splice;
use crate::pipe::now_ms;

/// The shared state every pipe accept/stream task reads.
pub(crate) struct PipeHandlerState {
    pub(crate) query: PipeQuery,
    pub(crate) registry: Arc<PipeRegistry>,
    pub(crate) sessions: Arc<PipeSessions>,
    pub(crate) audit: Arc<dyn PipeAuditSink>,
}

/// The accept-side gate for [`PIPE_ALPN`](super::alpn::PIPE_ALPN).
pub struct PipeProtocolHandler {
    state: Arc<PipeHandlerState>,
}

impl PipeProtocolHandler {
    /// Build a handler over the shared pipe state.
    #[must_use]
    pub(crate) fn new(state: Arc<PipeHandlerState>) -> Self {
        Self { state }
    }
}

// `ProtocolHandler` requires `Debug`; the state holds trait objects that are not
// `Debug`, so a manual impl keeps the bound satisfied without leaking internals.
impl std::fmt::Debug for PipeProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PipeProtocolHandler")
    }
}

impl ProtocolHandler for PipeProtocolHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let device_eid = conn.remote_id();
        let device = DeviceKey::from_bytes(*device_eid.as_bytes());

        // --- Stage 1: transport admission against the current snapshot. ---
        if let Some(cause) = stage1_reject(&self.state.query, &device).await {
            self.state.audit.connect_rejected(device_eid, None, cause);
            // Reject BEFORE accept_bi(): zero handshake bytes are read.
            conn.close(PIPE_REJECT_CODE, b"pipe-unauthorized");
            return Ok(());
        }

        // --- Stage 2: gate each bidi stream independently against the current
        // snapshot (revocation between streams is enforced naturally; in-flight
        // streams are covered by the teardown watcher). The loop ends when the
        // connection closes (locally torn down or peer gone). ---
        while let Ok((send, recv)) = conn.accept_bi().await {
            let state = self.state.clone();
            let conn = conn.clone();
            tokio::spawn(handle_stream(state, conn, device_eid, send, recv));
        }
        Ok(())
    }
}

/// Stage-1 admission against the **current** snapshot (§5): resolve the proven
/// `device → identity → Active?`. Returns `Some(cause)` to reject (close before
/// `accept_bi`), `None` to admit. Fail-closed: an unreachable engine denies as
/// `unknown_device`.
async fn stage1_reject(query: &PipeQuery, device: &DeviceKey) -> Option<PipeDenyCause> {
    let Some(snapshot) = query.snapshot().await else {
        return Some(PipeDenyCause::UnknownDevice);
    };
    let Some(identity) = snapshot.identity_of_device(device) else {
        return Some(PipeDenyCause::UnknownDevice);
    };
    if snapshot.is_active(identity) {
        None
    } else {
        Some(PipeDenyCause::NotActive)
    }
}

/// One accepted pipe stream: read the hello, run stage-2, and splice on accept.
async fn handle_stream(
    state: Arc<PipeHandlerState>,
    conn: Connection,
    device_eid: EndpointId,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let hello = match PipeHello::read_from(&mut recv).await {
        Ok(h) => h,
        Err(err) => {
            tracing::debug!(%err, peer = %device_eid, "pipe: malformed hello; dropping stream");
            return;
        }
    };
    let pipe_id = hello.pipe_id;
    let device = DeviceKey::from_bytes(*device_eid.as_bytes());

    let verdict = gate::evaluate(&state.query, &state.registry, &device, pipe_id, now_ms()).await;

    let target = match verdict {
        PipeGateVerdict::Accept(target) => target,
        PipeGateVerdict::Reject(cause) => {
            // Write the reject byte (forwarding nothing) and finish the stream.
            let _ = send.write_all(&[PIPE_REJECT]).await;
            let _ = send.finish();
            state
                .audit
                .connect_rejected(device_eid, Some(&pipe_id), cause);
            return;
        }
    };

    // Connect to the real loopback target. A failure here is an owner-side error,
    // not a gate denial: signal reject so the connector does not hang, and log it.
    let tcp = match TcpStream::connect(target).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, peer = %device_eid, %target, "pipe: target connect failed");
            let _ = send.write_all(&[PIPE_REJECT]).await;
            let _ = send.finish();
            return;
        }
    };

    // Accepted: tell the connector to start forwarding, then splice.
    if send.write_all(&[PIPE_ACCEPT]).await.is_err() {
        return;
    }
    state.audit.connect_accepted(device_eid, &pipe_id);

    let splice_task = tokio::spawn(splice(send, recv, tcp));
    let abort = splice_task.abort_handle();
    let id = state.sessions.register(device_eid, pipe_id, conn, abort);

    // Reap the session bookkeeping when the splice finishes (or is aborted by the
    // teardown watcher) — `deregister` is a no-op if teardown already removed it.
    let sessions = state.sessions.clone();
    tokio::spawn(async move {
        let _ = splice_task.await;
        sessions.deregister(id);
    });
}

#[cfg(test)]
mod tests {
    use super::PipeProtocolHandler;

    #[test]
    fn handler_debug_is_opaque() {
        // The Debug impl must not require the inner trait objects to be Debug.
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<PipeProtocolHandler>();
    }
}
