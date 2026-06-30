//! Owner-side helpers for the Live Pipe Plane (spec §6.5.1).
//!
//! The expose/close **orchestration** (register target → `build_pipe_opened` →
//! publish; `build_pipe_closed` → publish → teardown) lives on the
//! [`Node`](crate::node::Node), where the engine heads/publish surface is reachable.
//! This module owns the one piece that is purely owner-local: drawing a fresh,
//! unguessable `pipe_id`.

use iroh_rooms_core::event::keys::SigningKey;

/// Draw a fresh 128-bit `pipe_id` from the OS CSPRNG (spec R5).
///
/// Reuses the core key generator (the single audited CSPRNG entry point in the
/// workspace) and takes the first 16 bytes of a throwaway public key, so the net
/// crate adds **no** new RNG dependency. 128 bits makes a collision or a guess
/// cryptographically negligible.
#[must_use]
pub fn new_pipe_id() -> [u8; 16] {
    let key = SigningKey::generate();
    let mut id = [0u8; 16];
    id.copy_from_slice(&key.public_bytes()[..16]);
    id
}

#[cfg(test)]
mod tests {
    use super::new_pipe_id;

    #[test]
    fn pipe_ids_are_distinct_across_draws() {
        // Two CSPRNG draws must not collide (128-bit space).
        let a = new_pipe_id();
        let b = new_pipe_id();
        assert_ne!(a, b);
    }
}
