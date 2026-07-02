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
//!
//! **This is an implementation crate.** The supported SDK entry point is the
//! `iroh-rooms` façade (issue #36 / IR-0301), which re-exports a curated,
//! documented, stability-tiered subset of this crate's surface. Depend on
//! this crate directly only for advanced use; every item here may change
//! without the façade's `0.x` stability policy.

pub mod event;

/// The out-of-band room invite ticket (IR-0103): the copy-pasteable, versioned,
/// checksummed secret carrier that travels alongside an on-log `member.invited`
/// event. Reuses the landed [`event::cbor`] codec for its body and re-derives the
/// capability hash via [`event::capability_hash`], so the joiner side can decode a
/// ticket without duplicating either. See spec `key-bound-invite-ticket-generation.md`.
pub mod ticket;

/// The deterministic membership fold and authorization layer (IR-0008): the
/// second stateful layer of the Room Event Plane, downstream of the stateless
/// [`event`] validator. Turns a set of
/// [`ValidatedEvent`](event::ValidatedEvent)s into a per-event ancestor-stable
/// log-validity verdict and a convergent
/// [`MembershipSnapshot`](membership::MembershipSnapshot) that the pipe/blob
/// access decisions consult. Pure in-memory, no `store` dependency (spec D1).
/// See `PHASE-0-SPIKE.md` Membership & Ordering §3/§5/§6/§7.
pub mod membership;

/// The local `SQLite` event store (IR-0004): idempotent persistence of validated
/// events, derived query indexes, and a deterministic rebuild. Behind the `store`
/// cargo feature so validate-only consumers keep a lean dependency tree.
#[cfg(feature = "store")]
pub mod store;

/// The bounded recent-sync engine (IR-0007): a deterministic, sans-IO protocol
/// state machine that moves opaque signed [`WireEvent`](event::WireEvent)s between
/// peers and reconciles them over the landed [`store`] + [`membership`] fold —
/// pull missing events by id, pull bounded recent chat, always reconcile the
/// never-windowed membership sub-DAG + admin chain, and exchange admin tips with a
/// fail-closed incompleteness detector. Ships with an in-memory simulation harness
/// ([`sync::sim`]) that proves arrival-order-independent convergence (Spike Plan
/// Day 6 / Gate D). Behind the `sync` cargo feature (which enables `store`).
/// See `PHASE-0-SPIKE.md` ADR-2 / Membership & Ordering §0/§4 and the spec
/// `bounded-recent-sync-prototype.md`.
#[cfg(feature = "sync")]
pub mod sync;

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
