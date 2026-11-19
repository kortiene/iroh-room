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
//! That check is [`decode_length_prefix`]: a plain synchronous decode of a
//! length-prefixed buffer. [`read_frame`] calls it on the 4-byte prefix before
//! it allocates, then reads exactly the declared number of body bytes. The same
//! function accepts a whole buffer (prefix and body, or a hostile prefix with
//! no body) so it can be fuzzed without a QUIC stream.

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

/// One synchronous look at a length-prefixed buffer.
///
/// [`decode_length_prefix`] never allocates. A declared length above
/// [`MAX_FRAME_BYTES`] is [`FrameError::Oversized`] before any body byte is
/// copied. [`read_frame`] only allocates after this returns an in-range length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedFrame<'a> {
    /// `input` was empty: a clean end at a frame boundary.
    ///
    /// [`read_frame`] maps a clean stream end to `Ok(None)` before calling
    /// [`decode_length_prefix`], so the stream path does not observe this
    /// variant. An empty buffer passed to the synchronous decoder does.
    Empty,
    /// The prefix is in range, but `input` does not yet contain `len` body bytes.
    ///
    /// No body buffer was allocated. [`read_frame`] treats this as "read exactly
    /// `len` more bytes from the stream".
    Incomplete {
        /// Declared body length, already checked against [`MAX_FRAME_BYTES`].
        len: u32,
    },
    /// The first complete frame's body.
    ///
    /// Bytes after this frame are not part of the result. A later frame in the
    /// same buffer is left for the caller; [`read_frame`] never hands a buffer
    /// that contains more than one prefix, because it reads the body from the
    /// stream itself.
    Body(&'a [u8]),
}

/// Decode one length-prefixed frame from `input`.
///
/// This is the synchronous core of [`read_frame`]:
///
/// * empty input → [`DecodedFrame::Empty`] (clean end at a frame boundary)
/// * a short non-empty prefix (1–3 bytes) → [`FrameError::Truncated`]
/// * a declared length above [`MAX_FRAME_BYTES`] → [`FrameError::Oversized`],
///   and the following bytes are not copied
/// * an in-range prefix whose body is not fully present →
///   [`DecodedFrame::Incomplete`]
/// * a complete prefix and body → [`DecodedFrame::Body`] of exactly the
///   declared bytes
///
/// # Errors
/// [`FrameError::Truncated`] when a non-empty buffer does not contain a full
/// 4-byte prefix, or [`FrameError::Oversized`] when the declared length exceeds
/// [`MAX_FRAME_BYTES`]. A short body is [`DecodedFrame::Incomplete`], not an
/// error: unlike [`read_frame`], this function cannot tell "the stream ended"
/// from "more bytes might arrive".
pub fn decode_length_prefix(input: &[u8]) -> Result<DecodedFrame<'_>, FrameError> {
    if input.is_empty() {
        return Ok(DecodedFrame::Empty);
    }
    if input.len() < PREFIX_LEN {
        return Err(FrameError::Truncated);
    }
    let mut raw = [0u8; PREFIX_LEN];
    raw.copy_from_slice(&input[..PREFIX_LEN]);
    let declared = u32::from_be_bytes(raw);
    if declared > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized { declared });
    }
    let len = declared as usize;
    let rest = &input[PREFIX_LEN..];
    if rest.len() < len {
        return Ok(DecodedFrame::Incomplete { len: declared });
    }
    Ok(DecodedFrame::Body(&rest[..len]))
}

/// Read one length-prefixed frame from `stream`.
///
/// Returns `Ok(None)` on a **clean** end-of-stream (the peer finished the stream
/// at a frame boundary). Returns the raw, un-decoded body bytes on success — the
/// caller hands them verbatim to the engine.
///
/// The length check is [`decode_length_prefix`] on the 4-byte prefix. The body
/// is allocated only after that call accepts the length.
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

    // Four prefix bytes: `Oversized`, `Incomplete { len }` for a non-zero
    // in-range length, or `Body(&[])` when the declared length is zero.
    // `read_frame` still performs the zero-length `read_exact` so a reset
    // stream surfaces as `Io` the same way it did before the split.
    let len = match decode_length_prefix(&prefix)? {
        DecodedFrame::Incomplete { len } => len,
        DecodedFrame::Body(_) => 0,
        DecodedFrame::Empty => return Err(FrameError::Truncated),
    };

    let mut body = vec![0u8; len as usize];
    match stream.read_exact(&mut body).await {
        Ok(()) => Ok(Some(body)),
        Err(ReadExactError::FinishedEarly(_)) => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Io(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_length_prefix, DecodedFrame, FrameError, MAX_FRAME_BYTES, PREFIX_LEN};

    #[test]
    fn max_frame_bytes_is_one_mib() {
        // Pin the protocol constant: changing it silently breaks wire compatibility
        // with any peer that accepted frames up to the old limit.
        assert_eq!(MAX_FRAME_BYTES, 1_048_576u32);
        assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    }

    #[test]
    fn frame_cap_matches_the_engine_wire_contract() {
        // The engine bounds every message it emits (`have` claims, byte-budgeted
        // `Events` batches) by its own copy of this cap (issue #113). If the two
        // constants drift, the engine either emits bodies the writer drops or
        // under-fills frames — pin equality so a change must touch both.
        assert_eq!(
            MAX_FRAME_BYTES as usize,
            iroh_rooms_core::sync::MAX_FRAME_BYTES
        );
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

    /// Prefix-only classification [`read_frame`] relies on: the stream path
    /// calls [`decode_length_prefix`] with exactly four bytes.
    #[test]
    fn prefix_only_buffer_matches_what_read_frame_classifies() {
        assert_eq!(
            decode_length_prefix(&0u32.to_be_bytes()).expect("zero length"),
            DecodedFrame::Body(&[])
        );
        assert_eq!(
            decode_length_prefix(&4u32.to_be_bytes()).expect("in-range length"),
            DecodedFrame::Incomplete { len: 4 }
        );
        let err =
            decode_length_prefix(&(MAX_FRAME_BYTES + 1).to_be_bytes()).expect_err("over the cap");
        assert!(
            matches!(err, FrameError::Oversized { declared } if declared == MAX_FRAME_BYTES + 1),
            "expected Oversized, got {err}"
        );
    }

    #[test]
    fn decode_length_prefix_empty_is_clean_end() {
        assert_eq!(
            decode_length_prefix(b"").expect("empty"),
            DecodedFrame::Empty
        );
    }

    #[test]
    fn decode_length_prefix_short_prefix_is_truncated() {
        for n in 1..PREFIX_LEN {
            let err = decode_length_prefix(&vec![0xA5; n]).expect_err("short prefix");
            assert!(
                matches!(err, FrameError::Truncated),
                "len {n}: expected Truncated, got {err}"
            );
        }
    }

    #[test]
    fn decode_length_prefix_round_trips_a_body() {
        let body = b"hello";
        let mut input = Vec::new();
        let len = u32::try_from(body.len()).expect("body fits u32");
        input.extend_from_slice(&len.to_be_bytes());
        input.extend_from_slice(body);
        assert_eq!(
            decode_length_prefix(&input).expect("frame"),
            DecodedFrame::Body(body)
        );
    }

    #[test]
    fn decode_length_prefix_ignores_bytes_after_the_frame() {
        let mut input = Vec::new();
        input.extend_from_slice(&1u32.to_be_bytes());
        input.push(0xAB);
        input.extend_from_slice(b"next-frame");
        assert_eq!(
            decode_length_prefix(&input).expect("first frame"),
            DecodedFrame::Body(&[0xAB])
        );
    }

    #[test]
    fn decode_length_prefix_zero_length_with_trailing_bytes_is_empty_body() {
        let mut input = 0u32.to_be_bytes().to_vec();
        input.extend_from_slice(b"next-frame");
        assert_eq!(
            decode_length_prefix(&input).expect("empty body"),
            DecodedFrame::Body(&[])
        );
    }

    #[test]
    fn decode_length_prefix_short_body_is_incomplete() {
        // Claim 50 bytes, supply 10. Must not allocate the declared length:
        // the result borrows `input` and only reports the declared length.
        let mut input = 50u32.to_be_bytes().to_vec();
        input.extend_from_slice(&[0xAB; 10]);
        assert_eq!(
            decode_length_prefix(&input).expect("short body"),
            DecodedFrame::Incomplete { len: 50 }
        );
    }

    #[test]
    fn decode_length_prefix_exact_cap_without_body_is_incomplete() {
        let prefix = MAX_FRAME_BYTES.to_be_bytes();
        assert_eq!(
            decode_length_prefix(&prefix).expect("at the cap"),
            DecodedFrame::Incomplete {
                len: MAX_FRAME_BYTES
            }
        );
    }

    #[test]
    fn decode_length_prefix_rejects_oversized_prefix_with_a_trailing_payload() {
        let mut input = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
        input.extend_from_slice(b"not-part-of-an-allocated-body");
        let err = decode_length_prefix(&input).expect_err("over the cap");
        assert!(
            matches!(err, FrameError::Oversized { declared } if declared == MAX_FRAME_BYTES + 1),
            "expected Oversized, got {err}"
        );
    }

    #[test]
    fn decode_length_prefix_rejects_u32_max_length() {
        let err = decode_length_prefix(&u32::MAX.to_be_bytes()).expect_err("u32::MAX");
        assert!(
            matches!(err, FrameError::Oversized { declared } if declared == u32::MAX),
            "expected Oversized of u32::MAX, got {err}"
        );
    }
}
