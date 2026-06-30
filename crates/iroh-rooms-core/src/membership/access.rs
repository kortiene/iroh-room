//! Pure access-decision predicates the Blob/Pipe planes call
//! (`PHASE-0-SPIKE.md` Membership & Ordering §5; spec D6/D7).
//!
//! These gate **access**, not log-validity, and therefore consult the **current**
//! [`MembershipSnapshot`] (the fold over the whole local validated set), never an
//! ancestor view (spec D6). A since-removed member's log-valid `file.shared` /
//! `pipe.opened` grants **zero** capabilities. There is no default-all: a pipe is
//! reachable only by the explicit intersection `allowed_members ∩ Active`
//! (PRD §13.2).
//!
//! The QUIC accept-handler wiring, `pipe.closed`-reachability, and
//! tear-down-on-learn live in the planes (which own connection/pipe lifecycle);
//! this module owns the identity-resolution + Active + `allowed_members` + owner
//! predicates only.

use core::fmt;

use crate::event::content::PipeOpened;
use crate::event::ids::HashRef;
use crate::event::keys::{DeviceKey, IdentityKey};

use super::model::MembershipSnapshot;

/// Why an access request was denied. Distinct from the protocol
/// [`RejectReason`](crate::event::RejectReason): that taxonomy classifies
/// *log-validity*; this one classifies *access* (a different boundary, spec D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The connecting device resolves to no known identity.
    UnknownDevice,
    /// The connecting identity is not currently `Active` (removed or unknown).
    NotActive,
    /// The connecting identity is `Active` but not in `allowed_members`.
    NotAllowed,
    /// The pipe owner is not currently `Active`, so the pipe is void.
    OwnerInactive,
    /// The pipe's signed `expires_at` has passed relative to the supplied clock.
    Expired,
    /// The blob is not referenced by a `file.shared` from a currently-`Active`
    /// member.
    Unshared,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::UnknownDevice => "unknown_device",
            Self::NotActive => "not_active",
            Self::NotAllowed => "not_allowed",
            Self::OwnerInactive => "owner_inactive",
            Self::Expired => "expired",
            Self::Unshared => "unshared",
        };
        f.write_str(s)
    }
}

/// The blob-serve decision (spike §5 / vector §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobDecision {
    /// Serve the blob to the connecting device.
    Serve,
    /// Deny, with the specific reason.
    Reject(DenyReason),
}

/// The pipe-connect decision (spike §5 / vector §17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeDecision {
    /// Accept the connection.
    Accept,
    /// Deny, with the specific reason.
    Reject(DenyReason),
}

/// Blob-serve gate (spike §5 / vector §16): the connecting identity must be
/// `Active`, and `blob_hash` must be referenced by a `file.shared` whose authoring
/// identity is also currently `Active`.
///
/// `file_shares` resolves a blob hash to the identity that shared it (the Blob
/// plane supplies it from the validated `file.shared` set). Returning `None`
/// means no in-scope share references the hash.
#[must_use]
pub fn blob_serve_allowed(
    snapshot: &MembershipSnapshot,
    connecting: &DeviceKey,
    blob_hash: &HashRef,
    file_shares: &dyn Fn(&HashRef) -> Option<IdentityKey>,
) -> BlobDecision {
    let Some(identity) = snapshot.identity_of_device(connecting) else {
        return BlobDecision::Reject(DenyReason::UnknownDevice);
    };
    if !snapshot.is_active(identity) {
        return BlobDecision::Reject(DenyReason::NotActive);
    }
    match file_shares(blob_hash) {
        Some(sharer) if snapshot.is_active(&sharer) => BlobDecision::Serve,
        _ => BlobDecision::Reject(DenyReason::Unshared),
    }
}

/// Pipe-connect gate (spike §5 / vector §17). ALL must hold (no default-all,
/// PRD §13.2): the remote identity is `Active`, is in `allowed_members`, the pipe
/// owner is `Active`, and the pipe has not expired. `now_ms` is consulted **only**
/// to deny (fail-closed); with no clock supplied, expiry cannot fire.
///
/// The `pipe.closed`-causally-known check and live tear-down-on-learn belong to
/// the Pipe plane (which owns pipe lifecycle); compose them with this predicate.
#[must_use]
pub fn pipe_connect_allowed(
    snapshot: &MembershipSnapshot,
    connecting: &DeviceKey,
    pipe: &PipeOpened,
    now_ms: Option<u64>,
) -> PipeDecision {
    let Some(identity) = snapshot.identity_of_device(connecting) else {
        return PipeDecision::Reject(DenyReason::UnknownDevice);
    };
    if !snapshot.is_active(identity) {
        return PipeDecision::Reject(DenyReason::NotActive);
    }
    if !pipe.allowed_members.iter().any(|m| m == identity) {
        return PipeDecision::Reject(DenyReason::NotAllowed);
    }
    if !snapshot.is_active(&pipe.owner_id) {
        return PipeDecision::Reject(DenyReason::OwnerInactive);
    }
    if let (Some(expiry), Some(now)) = (pipe.expires_at, now_ms) {
        if now > expiry {
            return PipeDecision::Reject(DenyReason::Expired);
        }
    }
    PipeDecision::Accept
}
