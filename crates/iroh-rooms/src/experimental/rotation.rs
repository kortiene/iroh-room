//! **Experimental (unstable API).** Content-key rotation on member removal
//! (spec `specs/content-key-rotation.md`).
//!
//! The rotation lifecycle is implemented in the protocol and the sync engine,
//! but v0.1 GA ships room content as plaintext, encrypted in transit only
//! (ADR-0014 OD-1, decided 2026-11-15; roadmap amendment RB-1.2). No shipped
//! CLI or SDK flow triggers rotation, so this builder lives in the
//! experimental tier rather than the stable [`room`](crate::room) module: the
//! published stable surface does not promise rotation. End-to-end content
//! encryption is the v0.2 headline.

pub use iroh_rooms_core::event::content::{
    EncryptedContent, MemberKeyDistribution, WrappedKeyEntry,
};
pub use iroh_rooms_core::event::{build_member_removed_with_rotation, DistributionError};
