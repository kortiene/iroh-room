//! The custom ALPN the Live Pipe Plane speaks (spec §6.3).
//!
//! Like [`EVENT_ALPN`](crate::alpn::EVENT_ALPN), this is the **second** `.accept()`
//! chain on the shared `Router` ("one `Endpoint`, multiple ALPNs"). It is a wire
//! contract shared with every other implementation and with the value stored in
//! `pipe.opened.content.alpn`, so it is asserted byte-for-byte in tests — a silent
//! typo is an interop break, not a local bug.

use iroh::endpoint::VarInt;

/// ALPN protocol identifier for the authenticated TCP-over-QUIC pipe (spec §6.3).
/// Matches `pipe.opened.content.alpn`.
pub const PIPE_ALPN: &[u8] = b"/iroh-rooms/pipe/1";

/// The same ALPN as a `&str`, for the `pipe.opened.content.alpn` text field the
/// pure core builder takes (a single source of truth, pinned equal in a test).
pub const PIPE_ALPN_STR: &str = "/iroh-rooms/pipe/1";

/// Application close code used when the pipe accept-gate rejects a remote endpoint
/// at **stage 1** (the device resolves to no Active member), before any bidi
/// stream is accepted. Distinct from the event plane's
/// [`REJECT_CODE`](crate::handler::REJECT_CODE) so a dialer can tell which plane
/// refused it, and from a normal close (0).
pub const PIPE_REJECT_CODE: VarInt = VarInt::from_u32(0x5049_5001); // "PIP\x01"

/// Application close code used by the owner's teardown watcher when it severs a
/// live session whose authorization was revoked (removal / close / expiry). The
/// connector observes this code and stops forwarding.
pub const PIPE_TEARDOWN_CODE: VarInt = VarInt::from_u32(0x5049_5002); // "PIP\x02"

#[cfg(test)]
mod tests {
    use super::{PIPE_ALPN, PIPE_REJECT_CODE, PIPE_TEARDOWN_CODE};
    use iroh::endpoint::VarInt;

    #[test]
    fn pipe_alpn_is_the_exact_wire_contract() {
        // A typo here is a silent interop break with every other peer and with the
        // value embedded in pipe.opened.content.alpn, so pin the exact bytes.
        assert_eq!(PIPE_ALPN, b"/iroh-rooms/pipe/1");
        assert_eq!(PIPE_ALPN.len(), 18);
    }

    #[test]
    fn pipe_alpn_str_matches_the_byte_contract() {
        // The `&str` form the core builder takes must equal the wire bytes.
        assert_eq!(super::PIPE_ALPN_STR.as_bytes(), PIPE_ALPN);
    }

    #[test]
    fn pipe_close_codes_are_stable_and_distinct() {
        // The dialer/connector classifies refusals by close code; a silent change
        // breaks that. Pin them and assert they differ from each other and from a
        // normal close (0) and the event-plane reject code.
        assert_eq!(PIPE_REJECT_CODE, VarInt::from_u32(0x5049_5001));
        assert_eq!(PIPE_TEARDOWN_CODE, VarInt::from_u32(0x5049_5002));
        assert_ne!(PIPE_REJECT_CODE, PIPE_TEARDOWN_CODE);
        assert_ne!(PIPE_REJECT_CODE, VarInt::from_u32(0));
        assert_ne!(PIPE_REJECT_CODE, crate::handler::REJECT_CODE);
    }
}
