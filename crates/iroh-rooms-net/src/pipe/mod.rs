//! The Live Pipe Plane (IR-0010): authenticated TCP-over-QUIC forwarding to an
//! explicitly authorized room peer (spec `live-tcp-pipe-path.md`).
//!
//! This module is the **transport plane** that turns the already-landed pipe data
//! model ([`PipeOpened`](iroh_rooms_core::event::content::PipeOpened) /
//! [`PipeClosed`](iroh_rooms_core::event::content::PipeClosed)) and the pure access
//! predicate
//! [`pipe_connect_allowed`](iroh_rooms_core::membership::pipe_connect_allowed) into
//! a working pipe. It registers a second ALPN ([`PIPE_ALPN`]) on the **shared**
//! `Router` (one `Endpoint`, two planes), enforces the §5 connect gate
//! ([`gate::evaluate`]) at connect-accept time, splices bytes to a loopback target,
//! and tears live sessions down when authorization is revoked
//! ([`watcher`]). The owner/connector orchestration is driven from
//! [`Node`](crate::node::Node).
//!
//! Layering mirrors the event plane: the gate reads the **current**
//! [`MembershipSnapshot`](iroh_rooms_core::membership::MembershipSnapshot) (never an
//! ancestor view), the proven `device_id` is the QUIC `EndpointId` (no app-level
//! identity assertion), and every byte rides the encrypted QUIC/TLS tunnel for free.

use std::time::{SystemTime, UNIX_EPOCH};

pub mod alpn;
pub mod audit;
pub mod connector;
pub mod error;
pub mod gate;
pub mod hello;
pub mod owner;
pub mod registry;
pub mod runtime;
pub mod sessions;
pub mod splice;
pub mod watcher;

mod handler;

pub use alpn::{PIPE_ALPN, PIPE_REJECT_CODE, PIPE_TEARDOWN_CODE};
pub use audit::{PipeAuditSink, PipeDenyCause, TracingPipeAudit};
pub use connector::{PipeForwarder, PipeOutcome};
pub use error::PipeError;
pub use gate::PipeGateVerdict;
pub use hello::PipeHello;
pub use owner::new_pipe_id;
pub use registry::{is_loopback_target, OpenPipe, PipeRegistry};
pub use runtime::{PipeQuery, PipeQueryMsg};
pub use sessions::{LiveSession, PipeSessions};

pub(crate) use handler::{PipeHandlerState, PipeProtocolHandler};

/// Advisory wall-clock ms — the one place the Pipe plane reads a clock, and only to
/// deny on expiry (fail-closed, §5). Mirrors the `Node` pump's `now_ms`.
#[must_use]
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Lowercase hex of a 16-byte id for audit / error lines (the ids are tiny, so a
/// per-call `String` is fine). Shared so the audit and error vocabularies render
/// `pipe_id`s identically.
#[must_use]
pub(crate) fn hex16(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(32);
    for b in id {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::hex16;

    #[test]
    fn hex16_all_zeros_is_thirty_two_zero_chars() {
        assert_eq!(hex16(&[0u8; 16]), "0".repeat(32));
    }

    #[test]
    fn hex16_all_ff_is_ff_repeated() {
        assert_eq!(hex16(&[0xffu8; 16]), "ff".repeat(16));
    }

    #[test]
    fn hex16_is_always_32_lowercase_hex_chars() {
        for b in [0x00u8, 0x0f, 0x10, 0x7f, 0x80, 0xfe, 0xff] {
            let s = hex16(&[b; 16]);
            assert_eq!(
                s.len(),
                32,
                "hex16 must always be 32 chars for byte 0x{b:02x}"
            );
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "hex16 must be lowercase hex for byte 0x{b:02x}"
            );
        }
    }

    #[test]
    fn hex16_known_value() {
        // Hand-computed reference for a non-uniform id.
        let id: [u8; 16] = [
            0x0a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x00, 0xff,
            0x10, 0x20,
        ];
        assert_eq!(hex16(&id), "0abcdef00123456789abcdef00ff1020");
    }
}
