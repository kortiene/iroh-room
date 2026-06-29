//! Core library for Iroh Rooms.
//!
//! This crate will own the Room Event Plane, persistence interfaces, and shared
//! domain types. The first implementation milestone is the signed event model
//! described in `PHASE-0-SPIKE.md`.

/// Current crate-level protocol placeholder.
///
/// The real protocol version will be introduced with the event-core issue once
/// the canonical serialization and test vectors are implemented.
pub const PROTOCOL_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::PROTOCOL_VERSION;

    #[test]
    fn exposes_initial_protocol_version() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
