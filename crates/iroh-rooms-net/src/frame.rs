//! Length-prefixed framing over a per-peer bidirectional QUIC stream
//! (`PHASE-0-SPIKE.md` ADR-1 "frames events with a length prefix"; spec D4).
//!
//! Each stream carries a sequence of frames: a 4-byte big-endian `u32` body
//! length followed by exactly that many body bytes. A body is one canonical-CBOR
//! [`SyncMessage`](iroh_rooms_core::sync::SyncMessage) (`SyncMessage::encode()`),
//! so the transport speaks **one opaque frame type** and reuses the landed,
//! strict, byte-deterministic codec — a live `WireEvent` push is just a
//! `SyncMessage::Events { frames: [wire_bytes] }`.
//!
//! The codec never decodes the body: [`read_frame`] hands the raw bytes
//! **verbatim** to the engine, which validates them (defense in depth — a
//! malformed frame is the engine's logged drop, not a transport crash, spec §6).
//! The only structural check here is the [`MAX_FRAME_BYTES`] guard, which denies
//! a peer that claims an enormous frame before any allocation of that size.

use iroh::endpoint::{RecvStream, SendStream};

/// Maximum accepted body length for a single frame (1 MiB).
///
/// Chosen to comfortably exceed one `WireEvent` plus a bounded `Events` batch for
/// the prototype; revisit when batch sizing is tuned (spec D4). A declared length
/// above this closes the stream rather than attempting the allocation.
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Number of bytes in the big-endian length prefix.
const PREFIX_LEN: usize = 4;

/// A framing-layer failure on a peer stream. These are link-fatal (the caller
/// marks the peer offline and redials); they are never a reason to crash.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
    /// The declared body length exceeded [`MAX_FRAME_BYTES`] (a peer claiming an
    /// oversized frame). The link is closed.
    Oversized {
        /// The length the peer declared.
        declared: u32,
    },
    /// The stream ended part-way through a frame (prefix or body truncated).
    Truncated,
    /// An underlying stream read/write error.
    Io(String),
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Oversized { declared } => {
                write!(
                    f,
                    "oversized_frame: declared {declared} > {MAX_FRAME_BYTES}"
                )
            }
            Self::Truncated => f.write_str("truncated_frame"),
            Self::Io(e) => write!(f, "frame_io_error: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Write one length-prefixed frame (4-byte BE length + body) to `stream`.
///
/// # Errors
/// [`FrameError::Oversized`] if `body` exceeds [`MAX_FRAME_BYTES`] (we refuse to
/// emit a frame a conformant peer would reject), or [`FrameError::Io`] on a
/// stream write error.
pub async fn write_frame(stream: &mut SendStream, body: &[u8]) -> Result<(), FrameError> {
    let len =
        u32::try_from(body.len()).map_err(|_| FrameError::Oversized { declared: u32::MAX })?;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized { declared: len });
    }
    let prefix = len.to_be_bytes();
    stream
        .write_all(&prefix)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    Ok(())
}

/// Read one length-prefixed frame from `stream`.
///
/// Returns `Ok(None)` on a **clean** end-of-stream (the peer finished the stream
/// at a frame boundary). Returns the raw, un-decoded body bytes on success — the
/// caller hands them verbatim to the engine.
///
/// # Errors
/// [`FrameError::Oversized`] if the declared length exceeds [`MAX_FRAME_BYTES`],
/// [`FrameError::Truncated`] if the stream ends mid-frame, or [`FrameError::Io`]
/// on a stream read error.
pub async fn read_frame(stream: &mut RecvStream) -> Result<Option<Vec<u8>>, FrameError> {
    use iroh::endpoint::ReadExactError;

    let mut prefix = [0u8; PREFIX_LEN];
    match stream.read_exact(&mut prefix).await {
        Ok(()) => {}
        // Clean EOF at a frame boundary: the stream finished with nothing buffered.
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        // A partial prefix is a genuinely truncated frame.
        Err(ReadExactError::FinishedEarly(_)) => return Err(FrameError::Truncated),
        Err(e) => return Err(FrameError::Io(e.to_string())),
    }

    let len = u32::from_be_bytes(prefix);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized { declared: len });
    }

    let mut body = vec![0u8; len as usize];
    match stream.read_exact(&mut body).await {
        Ok(()) => Ok(Some(body)),
        Err(ReadExactError::FinishedEarly(_)) => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Io(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameError, MAX_FRAME_BYTES};

    #[test]
    fn max_frame_bytes_is_one_mib() {
        // Pin the protocol constant: changing it silently breaks wire compatibility
        // with any peer that accepted frames up to the old limit.
        assert_eq!(MAX_FRAME_BYTES, 1_048_576u32);
        assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    }

    #[test]
    fn frame_error_display_is_non_empty_and_stable() {
        // Stable display strings appear in logs/audit; verify they are non-empty
        // and contain diagnostic context (length, error type).
        let oversized = FrameError::Oversized {
            declared: 5_000_000,
        }
        .to_string();
        assert!(
            oversized.contains("5000000"),
            "Oversized must include declared len: {oversized}"
        );

        let truncated = FrameError::Truncated.to_string();
        assert!(!truncated.is_empty(), "Truncated display must not be empty");

        let io_err = FrameError::Io("broken pipe".to_owned()).to_string();
        assert!(
            io_err.contains("broken pipe"),
            "Io must include the reason: {io_err}"
        );
    }
}
