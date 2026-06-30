//! [`PipeError`] — the Live Pipe Plane's error taxonomy (spec §6.5).
//!
//! These are link-/setup-level faults (a non-loopback target, a broken handshake,
//! an offline owner). A *denied* connection is **not** a `PipeError`: it is a
//! first-class [`PipeOutcome`](super::connector::PipeOutcome) the connector
//! surfaces, because denial is the expected, security-relevant result of the gate.

use std::net::SocketAddr;

use super::hex16;

/// A Live Pipe Plane setup/transport fault.
#[derive(Debug)]
#[non_exhaustive]
pub enum PipeError {
    /// The owner's forward target is not a loopback address (PRD §13.2.3 / D6). The
    /// prototype refuses to expose a non-loopback target; there is no escape hatch.
    NonLoopbackTarget(SocketAddr),
    /// `allowed_members` was empty (no default-all, PRD §13.2). Caught before any
    /// event is built so the caller gets a friendly pre-IO error.
    EmptyAllowList,
    /// The connector could not resolve a `pipe.opened` for the requested `pipe_id`
    /// (it has not synced the announcement yet — spec R4).
    UnknownPipe([u8; 16]),
    /// The dialable owner address does not match the `owner_endpoint` named in the
    /// signed `pipe.opened` (a redirected / wrong address).
    OwnerEndpointMismatch,
    /// The pipe handshake was malformed (bad version / truncated control frame).
    BadHandshake,
    /// The owner could not be dialed over the pipe ALPN (offline / unreachable).
    OwnerUnreachable(String),
    /// A stream or socket I/O error.
    Io(String),
}

impl core::fmt::Display for PipeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NonLoopbackTarget(addr) => {
                write!(
                    f,
                    "non_loopback_target: {addr} is not a loopback address; the pipe forward \
                     target must be 127.0.0.0/8 or ::1 (PRD §13.2.3)"
                )
            }
            Self::EmptyAllowList => f.write_str(
                "empty_allow_list: a pipe must name at least one allowed member (no default-all)",
            ),
            Self::UnknownPipe(id) => {
                write!(
                    f,
                    "unknown_pipe: no pipe.opened for {} is known locally yet (sync the room first)",
                    hex16(id)
                )
            }
            Self::OwnerEndpointMismatch => f.write_str(
                "owner_endpoint_mismatch: the dialable address does not match the signed \
                 pipe.opened owner_endpoint",
            ),
            Self::BadHandshake => f.write_str("bad_handshake: malformed pipe control frame"),
            Self::OwnerUnreachable(e) => write!(f, "owner_unreachable: {e}"),
            Self::Io(e) => write!(f, "pipe_io_error: {e}"),
        }
    }
}

impl std::error::Error for PipeError {}

#[cfg(test)]
mod tests {
    use super::PipeError;
    use std::net::{Ipv4Addr, SocketAddr};

    #[test]
    fn display_strings_carry_stable_codes() {
        let nl = PipeError::NonLoopbackTarget(SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 22)));
        assert!(nl.to_string().starts_with("non_loopback_target:"));
        assert!(PipeError::EmptyAllowList
            .to_string()
            .starts_with("empty_allow_list:"));
        assert!(PipeError::UnknownPipe([0xab; 16])
            .to_string()
            .starts_with("unknown_pipe:"));
        assert!(PipeError::UnknownPipe([0xab; 16])
            .to_string()
            .contains("ab"));
        assert!(PipeError::OwnerEndpointMismatch
            .to_string()
            .starts_with("owner_endpoint_mismatch:"));
        assert!(PipeError::BadHandshake
            .to_string()
            .starts_with("bad_handshake:"));
    }
}
