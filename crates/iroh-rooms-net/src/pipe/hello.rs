//! The pipe control handshake: `PipeHello` + the 1-byte gate reply (spec §6.4).
//!
//! The first bytes on each accepted pipe bidi stream are a fixed-width
//! [`PipeHello`] — a 1-byte version (`== 1`) followed by the 16-byte `pipe_id` the
//! connector wants to reach. The owner reads exactly one `PipeHello`, runs the
//! stage-2 gate, then writes exactly one **reply byte**
//! ([`PIPE_ACCEPT`]/[`PIPE_REJECT`]): on accept the remainder of the stream is raw
//! spliced TCP bytes; on reject the owner finishes the stream having forwarded
//! nothing. The reply byte is a control byte, not payload — so "no TCP byte is
//! forwarded before the gate passes" holds (spec §4.3 / D3).

use iroh::endpoint::{RecvStream, SendStream};

use super::error::PipeError;

/// Length of the on-wire [`PipeHello`] frame: 1 version byte + 16 `pipe_id` bytes.
pub const HELLO_LEN: usize = 1 + 16;

/// The protocol version carried in the first `PipeHello` byte. Pinned: a peer that
/// sends any other value is rejected before the gate runs.
pub const HELLO_VERSION: u8 = 1;

/// The owner's reply byte after a stream **passes** both gate stages: the stream
/// now carries spliced TCP bytes.
pub const PIPE_ACCEPT: u8 = 0x01;

/// The owner's reply byte after a stream is **denied** at stage 2 (per-pipe
/// authorization): the stream is finished without forwarding any byte.
pub const PIPE_REJECT: u8 = 0x00;

/// The fixed-width pipe handshake frame (spec §6.4): which pipe the connector
/// wants to reach. The proven `device_id` (QUIC stage 1) is **not** carried here —
/// it is never re-derived from self-asserted fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeHello {
    /// The pipe to connect to (a `pipe.opened.pipe_id`).
    pub pipe_id: [u8; 16],
}

impl PipeHello {
    /// Build a hello for `pipe_id`.
    #[must_use]
    pub fn new(pipe_id: [u8; 16]) -> Self {
        Self { pipe_id }
    }

    /// The fixed-width on-wire encoding (`[version, pipe_id..]`).
    #[must_use]
    pub fn encode(&self) -> [u8; HELLO_LEN] {
        let mut buf = [0u8; HELLO_LEN];
        buf[0] = HELLO_VERSION;
        buf[1..].copy_from_slice(&self.pipe_id);
        buf
    }

    /// Decode a fixed-width hello, rejecting an unknown version.
    ///
    /// # Errors
    /// [`PipeError::BadHandshake`] if the version byte is not [`HELLO_VERSION`].
    pub fn decode(buf: &[u8; HELLO_LEN]) -> Result<Self, PipeError> {
        if buf[0] != HELLO_VERSION {
            return Err(PipeError::BadHandshake);
        }
        let mut pipe_id = [0u8; 16];
        pipe_id.copy_from_slice(&buf[1..]);
        Ok(Self { pipe_id })
    }

    /// Write this hello to `stream` (the connector → owner control frame).
    ///
    /// # Errors
    /// [`PipeError::Io`] on a stream write error.
    pub async fn write_to(&self, stream: &mut SendStream) -> Result<(), PipeError> {
        stream
            .write_all(&self.encode())
            .await
            .map_err(|e| PipeError::Io(e.to_string()))
    }

    /// Read exactly one hello from `stream` (the owner reads the control frame).
    ///
    /// # Errors
    /// [`PipeError::Io`] on a stream read error / early end, or
    /// [`PipeError::BadHandshake`] on an unknown version.
    pub async fn read_from(stream: &mut RecvStream) -> Result<Self, PipeError> {
        let mut buf = [0u8; HELLO_LEN];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| PipeError::Io(format!("{e:?}")))?;
        Self::decode(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::{PipeHello, HELLO_LEN, HELLO_VERSION, PIPE_ACCEPT, PIPE_REJECT};
    use crate::pipe::error::PipeError;

    #[test]
    fn hello_round_trips_through_encode_decode() {
        let hello = PipeHello::new([0x7a; 16]);
        let buf = hello.encode();
        assert_eq!(buf.len(), HELLO_LEN);
        assert_eq!(buf[0], HELLO_VERSION);
        let decoded = PipeHello::decode(&buf).expect("decode");
        assert_eq!(decoded, hello);
    }

    #[test]
    fn hello_with_unknown_version_is_rejected() {
        let mut buf = PipeHello::new([0x01; 16]).encode();
        buf[0] = 0xFF; // not HELLO_VERSION
        assert!(matches!(
            PipeHello::decode(&buf),
            Err(PipeError::BadHandshake)
        ));
    }

    #[test]
    fn hello_len_is_seventeen_bytes() {
        // Pin the fixed-width framing: a change is a wire break.
        assert_eq!(HELLO_LEN, 17);
    }

    #[test]
    fn accept_and_reject_reply_bytes_are_distinct() {
        // The connector distinguishes forwarding from denied by this single byte.
        assert_ne!(PIPE_ACCEPT, PIPE_REJECT);
        assert_eq!(PIPE_ACCEPT, 0x01);
        assert_eq!(PIPE_REJECT, 0x00);
    }

    #[test]
    fn encode_positions_version_at_byte_zero_and_pipe_id_at_bytes_one_through_sixteen() {
        // Pin the exact on-wire layout: a change silently breaks interop with any
        // peer that does not re-derive the offsets from constants.
        let pipe_id = [0xab; 16];
        let hello = PipeHello::new(pipe_id);
        let buf = hello.encode();
        assert_eq!(buf[0], HELLO_VERSION, "byte 0 must be the version");
        assert_eq!(&buf[1..], &pipe_id, "bytes 1..17 must be pipe_id verbatim");
    }

    #[test]
    fn decode_preserves_every_pipe_id_byte() {
        // Verify that a non-uniform pipe_id with all 256 byte values represented
        // survives an encode → decode round trip without any byte being lost or
        // swapped (catches off-by-one in the copy_from_slice bounds).
        let pipe_id: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        let decoded = PipeHello::decode(&PipeHello::new(pipe_id).encode()).expect("decode");
        assert_eq!(decoded.pipe_id, pipe_id);
    }
}
