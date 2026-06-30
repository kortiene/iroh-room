//! The custom ALPN the Room Event Plane mesh speaks (`PHASE-0-SPIKE.md` ADR-1).
//!
//! Every event-transport connection negotiates this exact byte string at the
//! TLS/QUIC layer; a peer that does not offer it never reaches the
//! [`EventProtocolHandler`](crate::handler::EventProtocolHandler) accept-gate.
//! The string is a **wire contract** shared with every other implementation of
//! the protocol, so it is asserted byte-for-byte in tests — a silent typo is an
//! interop break, not a local bug.

/// ALPN protocol identifier for direct full-mesh QUIC event transport (ADR-1).
///
/// Registered on the shared `Router` alongside the future blob/pipe ALPNs ("one
/// `Endpoint`, multiple `.accept()` chains").
pub const EVENT_ALPN: &[u8] = b"/iroh-rooms/event/1";

#[cfg(test)]
mod tests {
    use super::EVENT_ALPN;

    #[test]
    fn alpn_is_the_exact_wire_contract() {
        // A typo here is a silent interop break with every other peer, so pin the
        // exact bytes (not just a prefix/length).
        assert_eq!(EVENT_ALPN, b"/iroh-rooms/event/1");
        assert_eq!(EVENT_ALPN.len(), 19);
    }
}
