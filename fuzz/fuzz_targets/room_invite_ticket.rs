#![no_main]

use std::str::FromStr;

use libfuzzer_sys::fuzz_target;

/// Wire constants from `iroh_rooms_core::ticket` (`TICKET_PREFIX`,
/// `TICKET_VERSION`, `TICKET_CHECKSUM_LEN`). Those are private, so the harness
/// repeats them. `from_str` checks the 4-byte BLAKE3 prefix of
/// `version || body` before it parses CBOR.
const TICKET_PREFIX: &str = "roomtkt1";
const TICKET_VERSION: u8 = 1;
const TICKET_CHECKSUM_LEN: usize = 4;

fuzz_target!(|data: &[u8]| {
    // Raw token: prefix, base32, version, and the checksum gate.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = std::hint::black_box(iroh_rooms_core::ticket::RoomInviteTicket::from_str(text));
    }
    // Same bytes as a CBOR body inside a token whose checksum is valid, so
    // `from_str` reaches `decode_canonical` / `from_cbor_body`.
    let wrapped = token_with_valid_checksum(data);
    let _ = std::hint::black_box(iroh_rooms_core::ticket::RoomInviteTicket::from_str(
        &wrapped,
    ));
});

fn token_with_valid_checksum(cbor_body: &[u8]) -> String {
    let mut payload = Vec::with_capacity(1 + cbor_body.len() + TICKET_CHECKSUM_LEN);
    payload.push(TICKET_VERSION);
    payload.extend_from_slice(cbor_body);
    let digest = blake3::hash(&payload);
    payload.extend_from_slice(&digest.as_bytes()[..TICKET_CHECKSUM_LEN]);
    let body = data_encoding::BASE32_NOPAD
        .encode(&payload)
        .to_ascii_lowercase();
    format!("{TICKET_PREFIX}{body}")
}
