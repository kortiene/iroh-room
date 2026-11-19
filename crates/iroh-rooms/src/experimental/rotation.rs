//! **Experimental (unstable API).** Content-key rotation on member removal
//! (spec `specs/content-key-rotation.md`).
//!
//! The rotation lifecycle is implemented in the protocol and the sync engine,
//! but v0.1 GA ships room content as plaintext, encrypted in transit only
//! (ADR-0014 OD-1, decided 2026-11-15; roadmap amendment RB-1.2). No shipped
//! CLI command triggers rotation or content encryption. The builders are
//! re-exported here — not from the stable [`room`](crate::room) module — so a
//! façade consumer can name the same functions `iroh-rooms-core` already
//! exposes without a direct core dependency. The published stable surface
//! does not promise rotation. End-to-end content encryption is the v0.2
//! headline. `RoomKey` stays a type of `iroh-rooms-crypto`; these signatures
//! name it the same way `iroh-rooms-core` does.

pub use iroh_rooms_core::event::{
    build_content_encrypted, build_key_distribution_content, build_member_key_distribution,
    build_member_removed_with_rotation, open_encrypted_content, DistributionError, SealError,
    UnreadableReason,
};
