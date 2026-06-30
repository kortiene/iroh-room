//! Core library for Iroh Rooms.
//!
//! This crate owns the Room Event Plane, persistence interfaces, and shared
//! domain types. The first implementation milestone — landed here — is the
//! canonical signed event model described in `PHASE-0-SPIKE.md` (Event Protocol
//! §1–§8): the byte-for-byte trust boundary every other plane rides on.
//!
//! See [`event`] for the public surface: domain newtypes, deterministic-CBOR
//! serialization (canonical signed bytes), BLAKE3-256 event-ID derivation,
//! Ed25519 signing/verification under `device_id`, the [`event::WireEvent`]
//! envelope, strict per-type content validation, and the stateless
//! [`event::validate::validate_wire_bytes`] pipeline.

pub mod event;

/// Current crate-level protocol version.
///
/// Matches the on-wire `schema_version` and `WireEvent.v` for MVP
/// (Event Protocol §2/§3): both are `1`; any other value is rejected.
pub const PROTOCOL_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::PROTOCOL_VERSION;

    #[test]
    fn exposes_initial_protocol_version() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
