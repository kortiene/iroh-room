//! The custom ALPN the Room Event Plane mesh speaks (`PHASE-0-SPIKE.md` ADR-1).
//!
//! Every event-transport connection negotiates this exact byte string at the
//! TLS/QUIC layer; a peer that does not offer it never reaches the
//! [`EventProtocolHandler`](crate::handler::EventProtocolHandler) accept-gate.
//! The string is a **wire contract** shared with every other implementation of
//! the protocol, so it is asserted byte-for-byte in tests — a silent typo is an
//! interop break, not a local bug.
//!
//! When the `gossip_overlay` feature is enabled, a second ALPN
//! ([`GOSSIP_ALPN`]) carries the `iroh-gossip` swarm traffic that broadcasts
//! `SyncMessage::Events` frames among admitted device keys (issue #171 / spec
//! §4 D2). It is gated by the same connection-level admission wrapper
//! ([`GossipProtocolHandler`](crate::gossip::GossipProtocolHandler)) so the
//! reject-before-bytes guarantee is preserved on both planes.

/// ALPN protocol identifier for direct full-mesh QUIC event transport (ADR-1).
///
/// Registered on the shared `Router` alongside the future blob/pipe ALPNs ("one
/// `Endpoint`, multiple `.accept()` chains").
pub const EVENT_ALPN: &[u8] = b"/iroh-rooms/event/1";

/// ALPN protocol identifier for the gossip overlay's swarm traffic
/// (issue #171 / spec §4 D2). Used **only** when the `gossip_overlay` feature
/// is enabled.
///
/// Distinct from iroh-gossip's default `/iroh-gossip/1` so the room event
/// plane's admission gate (`GossipProtocolHandler`) owns the accept path — a
/// peer that fails admission at the connection level never reaches the gossip
/// layer. The 32-byte `TopicId` (derived from the public `room_id`, spec D5)
/// is a rendezvous point, **not** the admission boundary.
pub const GOSSIP_ALPN: &[u8] = b"/iroh-rooms/gossip/1";

#[cfg(test)]
mod tests {
    use super::{EVENT_ALPN, GOSSIP_ALPN};

    #[test]
    fn alpn_is_the_exact_wire_contract() {
        // A typo here is a silent interop break with every other peer, so pin the
        // exact bytes (not just a prefix/length).
        assert_eq!(EVENT_ALPN, b"/iroh-rooms/event/1");
        assert_eq!(EVENT_ALPN.len(), 19);
    }

    /// The gossip ALPN is a distinct wire contract: a peer that does not offer
    /// it never reaches the gossip accept-gate. Pin the exact bytes so a silent
    /// typo is caught as a test failure, not an interop break (issue #171 /
    /// spec §4 D2 step 1).
    #[test]
    fn gossip_alpn_is_the_exact_wire_contract() {
        assert_eq!(GOSSIP_ALPN, b"/iroh-rooms/gossip/1");
        assert_eq!(GOSSIP_ALPN.len(), 20);
    }

    /// The two room-plane ALPNs must not collide: a peer negotiating one must
    /// not be routed to the other's handler. Same-length distinct bytes is the
    /// minimum; the explicit inequality pins it.
    #[test]
    fn gossip_alpn_differs_from_event_alpn() {
        assert_ne!(EVENT_ALPN, GOSSIP_ALPN);
    }
}
