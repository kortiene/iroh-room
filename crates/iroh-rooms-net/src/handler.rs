//! The accept-gate protocol handler (`PHASE-0-SPIKE.md` ADR-1 native admission;
//! the issue's security note; spec §4.6 / G1 / G3).
//!
//! [`EventProtocolHandler`] is registered on the shared `Router` for
//! [`EVENT_ALPN`](crate::alpn::EVENT_ALPN). Its [`accept`](ProtocolHandler::accept)
//! resolves the QUIC/TLS-authenticated `device_id` (`Connection::remote_id()`) and
//! **closes the connection before ever calling `accept_bi()`** when admission
//! fails — so an unauthorized peer's first event byte is never read, never
//! surfaced to the inbound sink, never reaches the engine or store (AC2). This is
//! a *structural* guarantee, not a runtime check after the fact.

use std::sync::Arc;

use iroh::endpoint::{Connection, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};

use crate::admission::AdmissionDecision;
use crate::peer::register_connection;
use crate::state::PeerConnState;
use crate::transport::Shared;

/// Application close code used when admission rejects a remote endpoint. A stable
/// code lets the *dialing* side distinguish a deliberate `Unauthorized` rejection
/// from a generic transport drop (feeds the dialer's PRD §16.3 state).
pub const REJECT_CODE: VarInt = VarInt::from_u32(0x5245_4a01); // "REJ\x01"

/// The accept-side gate for the event ALPN.
pub struct EventProtocolHandler {
    shared: Arc<Shared>,
}

impl EventProtocolHandler {
    /// Build a handler over the shared transport state.
    #[must_use]
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

// `ProtocolHandler` requires `Debug`, but `Shared` holds trait objects that are
// not `Debug`; a manual impl keeps the bound satisfied without leaking internals.
impl std::fmt::Debug for EventProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventProtocolHandler")
    }
}

impl ProtocolHandler for EventProtocolHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // The remote id is the QUIC/TLS-proven `device_id`; no application bytes
        // are read to obtain it (ADR-1 "admission is a property of the transport").
        let device = conn.remote_id();

        match self.shared.admission.authorize(device) {
            AdmissionDecision::Reject(cause) => {
                // Reject BEFORE accept_bi(): zero event bytes are ever read. The
                // close is not an error from the router's point of view.
                self.shared.audit.rejected(device, cause);
                self.shared
                    .table
                    .set(device, PeerConnState::Unauthorized, None);
                conn.close(REJECT_CODE, b"unauthorized");
                Ok(())
            }
            AdmissionDecision::Admit { identity } => {
                self.shared.audit.accepted(device, &identity);
                // Only now — for an admitted member — do we accept the stream.
                let (send, recv) = conn.accept_bi().await?;
                register_connection(self.shared.clone(), device, conn.clone(), send, recv);
                self.shared
                    .table
                    .set(device, PeerConnState::Connected, Some(identity));
                self.shared.audit.connected(device);

                // The accept future owns the connection: keep it (and its streams)
                // alive until it closes, then surface the disconnect.
                conn.closed().await;
                self.shared.unregister(device);
                self.shared.table.set(device, PeerConnState::Offline, None);
                self.shared.audit.disconnected(device);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::REJECT_CODE;
    use iroh::endpoint::VarInt;

    #[test]
    fn reject_code_is_stable_wire_constant() {
        // A changed REJECT_CODE silently breaks the dialer's ability to distinguish
        // "unauthorized" from a generic network drop (spec §4.6 / §4.7). Pin it
        // so any accidental change is caught immediately.
        assert_eq!(REJECT_CODE, VarInt::from_u32(0x5245_4a01));
    }

    #[test]
    fn reject_code_differs_from_normal_application_close() {
        // The dialer identifies a deliberate admission reject by comparing the
        // close code to REJECT_CODE. If it equalled the normal-close code (0),
        // every roster-driven disconnect would look like an Unauthorized rejection.
        let normal_close = VarInt::from_u32(0); // LOCAL_CLOSE_CODE in transport.rs
        assert_ne!(
            REJECT_CODE, normal_close,
            "REJECT_CODE must be non-zero so it is distinguishable from a normal close"
        );
    }
}
