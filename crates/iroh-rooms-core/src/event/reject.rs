//! Rejection / flag taxonomy (Event Protocol §8) and the membership trait
//! boundary for the deferred stateful checks.
//!
//! [`RejectReason`] is the single, stable taxonomy for the whole protocol. The
//! variants this *stateless* layer can actually emit are marked **(stateless)**;
//! the rest are **(deferred)** — defined here so the taxonomy is one type, but
//! only produced by the sibling membership/ordering layer. Each variant maps to
//! the exact §8 spelling via [`RejectReason::code`].

use core::fmt;

use super::ids::RoomId;
use super::keys::IdentityKey;

/// A stable reason an event was rejected (dropped, never persisted/re-broadcast).
///
/// `duplicate` is intentionally **absent**: a duplicate is ignored, not an error
/// (Event Protocol §6 step 11 / §8), and is handled by the store layer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectReason {
    /// `schema_version` was not `1`. **(stateless)**
    UnknownSchemaVersion,
    /// `event_type` is not in the §7 registry. **(stateless)**
    UnknownEventType,
    /// The bytes were not canonical deterministic CBOR, or not exactly the eight
    /// signed keys with the correct types, or the outer envelope was malformed.
    /// **(stateless)**
    NonCanonicalEncoding,
    /// The advisory `id` field did not equal the recomputed event id. **(stateless)**
    IdMismatch,
    /// The Ed25519 signature did not verify under `device_id`. **(stateless)**
    BadSignature,
    /// `room_id` did not match (recomputed genesis id, or the processing room).
    /// **(stateless)**
    RoomIdMismatch,
    /// Strict per-type content validation failed (unknown content key, missing
    /// required key, wrong type, length/enum violation, or a bad embedded
    /// `device_binding`). **(stateless)**
    InvalidContent,
    /// `prev_events` had more than [`MAX_PREV_EVENTS`](super::constants::MAX_PREV_EVENTS)
    /// entries. **(stateless)**
    TooManyParents,
    /// A non-genesis event had empty `prev_events`, or `room.created` had
    /// non-empty `prev_events` (the stateless part of the genesis-descent
    /// invariant; full transitive reachability is deferred). **(stateless)**
    NotGenesisDescended,

    /// `sender_id` has no device bound to it in membership state. **(deferred —
    /// emitted by the membership layer.)**
    UnboundDevice,
    /// `sender_id` is not a current member of the room. **(deferred.)**
    NotAMember,
    /// `sender_id`'s role does not permit this `event_type`. **(deferred.)**
    InsufficientRole,
    /// A cited invite was consumed or its `expires_at` passed. **(deferred.)**
    ExpiredInvite,
    /// A join's `capability_secret` did not reproduce the invite's hash, or no
    /// such invite exists in the ancestor view. **(deferred.)**
    BadCapability,
    /// Accepting a join would exceed the supported active-member ceiling.
    /// **(deferred.)**
    RoomFull,
}

impl RejectReason {
    /// The stable string code (exact Event Protocol §8 spelling) for audit logs
    /// and CLI failure-mode distinction.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownSchemaVersion => "unknown_schema_version",
            Self::UnknownEventType => "unknown_event_type",
            Self::NonCanonicalEncoding => "non_canonical_encoding",
            Self::IdMismatch => "id_mismatch",
            Self::BadSignature => "bad_signature",
            Self::RoomIdMismatch => "room_id_mismatch",
            Self::InvalidContent => "invalid_content",
            Self::TooManyParents => "too_many_parents",
            Self::NotGenesisDescended => "not_genesis_descended",
            Self::UnboundDevice => "unbound_device",
            Self::NotAMember => "not_a_member",
            Self::InsufficientRole => "insufficient_role",
            Self::ExpiredInvite => "expired_invite",
            Self::BadCapability => "bad_capability",
            Self::RoomFull => "room_full",
        }
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for RejectReason {}

/// An advisory flag attached to a **successfully validated** event. Flags never
/// change the verdict, the validated set, ordering, or any authorization/expiry
/// decision (Event Protocol §6 step 10 / §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Flag {
    /// `created_at` was more than
    /// [`CLOCK_SKEW_FUTURE_MS`](super::constants::CLOCK_SKEW_FUTURE_MS) ahead of
    /// the supplied local time. **(stateless, advisory.)**
    ClockSkew,
    /// The signer authored two mutually-concurrent events. **(deferred — emitted
    /// by the membership/ordering layer.)**
    Equivocation,
    /// UI attribution: the author was later removed. **(deferred.)**
    FromRemovedMember,
}

impl Flag {
    /// The stable string code (exact Event Protocol §8 spelling).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClockSkew => "clock_skew",
            Self::Equivocation => "equivocation",
            Self::FromRemovedMember => "from_removed_member",
        }
    }
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Boundary for the **deferred** stateful checks (Event Protocol §6 steps 7–8).
///
/// This trait is defined here to freeze the validator's eventual public surface
/// (spec D3 / Open Q5), but **no implementation and no call site exist in this
/// issue** — the stateless validator never consults it. The sibling
/// membership/authorization issue provides the implementation and the wrapping
/// `validate_with_membership` entry point.
pub trait MembershipOracle {
    /// Resolve the device key bound to `sender_id` in `room_id`'s validated
    /// membership state, if any. Returns the raw `device_id` bytes.
    ///
    /// Used for step 7 (device binding from membership state) for the event
    /// types that do **not** carry a self-contained `device_binding`.
    fn bound_device(&self, room_id: &RoomId, sender_id: &IdentityKey) -> Option<[u8; 32]>;

    /// Decide whether `sender_id` is a current member of `room_id` with a role
    /// permitting `event_type` (step 8). `Ok(())` authorizes; `Err` carries the
    /// specific deferred reason (`not_a_member` / `insufficient_role` / …).
    ///
    /// # Errors
    /// Returns the deferred [`RejectReason`] for the failure mode.
    fn authorize(
        &self,
        room_id: &RoomId,
        sender_id: &IdentityKey,
        event_type: &str,
    ) -> Result<(), RejectReason>;
}
