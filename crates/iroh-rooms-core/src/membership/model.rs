//! The membership domain model: per-subject [`Status`] / [`Role`], a resolved
//! [`Member`], and the deterministic fold result [`MembershipSnapshot`]
//! (`PHASE-0-SPIKE.md` Membership & Ordering §3.1/§3.4; spec §4).
//!
//! [`Status`] and [`Role`] are ordered so the two convergence-critical merges are
//! plain lattice operations: status is a `max` (Removed-dominates, §3.4) and role
//! is a `min` (least-privilege, §3.8).

use std::collections::BTreeMap;

use crate::event::ids::RoomId;
use crate::event::keys::{DeviceKey, IdentityKey};

/// The hard active-member cap enforced by the membership fold.
///
/// Default builds stay at 5, the full-mesh-safe bound measured before the gossip
/// overlay. The `large_rooms` feature raises the cap to 40 for binaries that also
/// compile the bounded gossip topology (`iroh-rooms-net/gossip_overlay`), whose
/// spike-N40 run survived N=40 with 100% event delivery and no cascade.
#[cfg(not(feature = "large_rooms"))]
pub const MAX_ACTIVE_MEMBERS: usize = 5;

/// The hard active-member cap enforced by the membership fold.
///
/// Default builds stay at 5, the full-mesh-safe bound measured before the gossip
/// overlay. The `large_rooms` feature raises the cap to 40 for binaries that also
/// compile the bounded gossip topology (`iroh-rooms-net/gossip_overlay`), whose
/// spike-N40 run survived N=40 with 100% event delivery and no cascade.
#[cfg(feature = "large_rooms")]
pub const MAX_ACTIVE_MEMBERS: usize = 40;

/// The soft warning threshold for "approaching the active-member ceiling": one
/// slot below the hard cap (issue #144). Used by live observers
/// (`RoomReconciler`, `room members --status`) to surface a near-cap warning
/// **without** changing authorization or the cap itself. Derived from
/// [`MAX_ACTIVE_MEMBERS`] so it tracks the protocol invariant rather than being
/// configured independently.
pub const ACTIVE_MEMBER_WARNING_THRESHOLD: usize = MAX_ACTIVE_MEMBERS - 1;

/// A subject's current membership status (spike §3.4).
///
/// Ordered so the **Removed-dominates** rule is a `max`:
/// `Invited < Active < Removed`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    /// Has an admin invite in scope but no descending join.
    Invited,
    /// Has a join descending from a still-live invite and no causally-later
    /// departure head.
    Active,
    /// A `member.removed` or `member.left` is among the subject's causal heads.
    /// Removed dominates concurrent Active/Invited contributions.
    Removed,
}

/// A participant role (spike §3.1).
///
/// Ordered **least → most** privileged so the least-privilege attribute merge
/// (§3.8) is a `min`: `Agent < Member < Admin`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role {
    /// An automated agent participant.
    Agent,
    /// An ordinary member.
    Member,
    /// The immutable room admin (the genesis signer).
    Admin,
}

impl Role {
    /// Map a validated role string (`member` | `agent` | `admin`) to a [`Role`].
    /// Any other string defaults to the least-trusted concrete role, [`Role::Member`]
    /// — callers only ever pass strings already enum-validated by the stateless
    /// content parser, so the default is unreachable in practice and fails safe.
    #[must_use]
    pub(crate) fn from_validated_str(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            "agent" => Self::Agent,
            _ => Self::Member,
        }
    }
}

/// The resolved state of one subject after the fold (spec §4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    /// The subject's stable identity (`sender_id`).
    pub identity: IdentityKey,
    /// The device bound to this identity (from the join's / genesis
    /// `device_binding`). `None` for an `Invited`-only subject that never joined.
    pub device: Option<DeviceKey>,
    /// The subject's current membership status.
    pub status: Status,
    /// Resolved by the least-privilege + lowest-`event_id` merge (§3.8).
    pub role: Role,
}

/// The deterministic fold result over a validated event set — the value pipe/blob
/// access decisions consult (spike §5; spec §4).
///
/// A **pure function of the in-scope validated set**: any two peers holding the
/// identical set compute an equal snapshot regardless of arrival order
/// (the §0 same-set convergence guarantee).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MembershipSnapshot {
    room_id: RoomId,
    /// The immutable genesis signer, or `None` if no `room.created` is in scope.
    admin: Option<IdentityKey>,
    /// Per-identity resolved state, in deterministic (bytewise-identity) order.
    members: BTreeMap<IdentityKey, Member>,
    /// QUIC `EndpointId` (device) → bound identity, for §5 identity resolution.
    by_device: BTreeMap<DeviceKey, IdentityKey>,
}

impl MembershipSnapshot {
    /// Assemble a snapshot from already-folded parts. Internal to the membership
    /// layer; outside callers obtain snapshots via
    /// [`RoomMembership::snapshot`](super::RoomMembership::snapshot).
    #[must_use]
    pub(crate) fn new(
        room_id: RoomId,
        admin: Option<IdentityKey>,
        members: BTreeMap<IdentityKey, Member>,
        by_device: BTreeMap<DeviceKey, IdentityKey>,
    ) -> Self {
        Self {
            room_id,
            admin,
            members,
            by_device,
        }
    }

    /// The room this snapshot describes.
    #[must_use]
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// The immutable admin (genesis signer), or `None` if no `room.created` is in
    /// scope yet.
    #[must_use]
    pub fn admin(&self) -> Option<&IdentityKey> {
        self.admin.as_ref()
    }

    /// The subject's status, or `None` for an identity with **no** membership
    /// events (an unknown subject). Callers **default-deny** unknown subjects
    /// (§5): only a resolvably-`Active` identity is granted access.
    #[must_use]
    pub fn status(&self, id: &IdentityKey) -> Option<Status> {
        self.members.get(id).map(|m| m.status)
    }

    /// Whether `id` is currently `Active` (the single predicate the access planes
    /// rely on; an unknown subject is not active).
    #[must_use]
    pub fn is_active(&self, id: &IdentityKey) -> bool {
        self.status(id) == Some(Status::Active)
    }

    /// The subject's resolved role, or `None` for an unknown subject.
    #[must_use]
    pub fn role(&self, id: &IdentityKey) -> Option<Role> {
        self.members.get(id).map(|m| m.role)
    }

    /// The full resolved [`Member`] record for `id`, if known.
    #[must_use]
    pub fn member(&self, id: &IdentityKey) -> Option<&Member> {
        self.members.get(id)
    }

    /// Resolve a QUIC-authenticated device key to its bound identity (§5).
    #[must_use]
    pub fn identity_of_device(&self, dev: &DeviceKey) -> Option<&IdentityKey> {
        self.by_device.get(dev)
    }

    /// Iterate the currently-`Active` members in deterministic identity order.
    pub fn active_members(&self) -> impl Iterator<Item = &Member> {
        self.members.values().filter(|m| m.status == Status::Active)
    }

    /// Count currently-`Active` members.
    #[must_use]
    pub fn active_member_count(&self) -> usize {
        self.active_members().count()
    }

    /// The hard active-member cap ([`MAX_ACTIVE_MEMBERS`]); the protocol
    /// invariant the fold enforces with `RejectReason::RoomFull`. Exposed as a
    /// snapshot method so status/audit callers do not need to import the
    /// constant separately (issue #144).
    #[must_use]
    pub fn active_member_limit(&self) -> usize {
        MAX_ACTIVE_MEMBERS
    }

    /// Remaining active-member slots before the room hits [`MAX_ACTIVE_MEMBERS`]
    /// (issue #144). Saturates to `0` for an over-cap snapshot (defensive: the
    /// fold should never produce one, but the headroom surface must never
    /// underflow).
    #[must_use]
    pub fn active_member_headroom(&self) -> usize {
        self.active_member_limit()
            .saturating_sub(self.active_member_count())
    }

    /// Iterate every known member (any status) in deterministic identity order.
    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }
}

/// Pure below-to-at/above-threshold crossing detector for the active-member
/// count (issue #144). Returns `true` only on the transition from strictly
/// below [`ACTIVE_MEMBER_WARNING_THRESHOLD`] to at/above it; this is what
/// "one-shot warning per crossing" callers (`RoomReconciler`) consume so a
/// room that stays at the threshold does not emit a warning on every tick.
///
/// * `previous = Some(threshold - 1), current = threshold` → `true`
/// * `previous = Some(threshold), current = threshold` → `false` (no transition)
/// * `previous = Some(threshold), current = MAX_ACTIVE_MEMBERS` → `false`
/// * `previous = Some(threshold - 1), current = MAX_ACTIVE_MEMBERS` → `true`
/// * `previous = Some(MAX_ACTIVE_MEMBERS), current = threshold - 1` → `false`
/// * `previous = None`           , any `current` → `false` (no prior observation;
///   recommended default for `RoomReconciler` startup — see spec §4 D3 / OQ-1)
#[must_use]
pub fn active_member_warning_crossed(previous: Option<usize>, current: usize) -> bool {
    match previous {
        Some(prev) => {
            prev < ACTIVE_MEMBER_WARNING_THRESHOLD && current >= ACTIVE_MEMBER_WARNING_THRESHOLD
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_member_warning_crossed, ACTIVE_MEMBER_WARNING_THRESHOLD, MAX_ACTIVE_MEMBERS,
    };

    #[test]
    fn active_member_cap_matches_feature_flag() {
        #[cfg(not(feature = "large_rooms"))]
        assert_eq!(MAX_ACTIVE_MEMBERS, 5);
        #[cfg(feature = "large_rooms")]
        assert_eq!(MAX_ACTIVE_MEMBERS, 40);
    }

    #[test]
    fn active_member_warning_crossed_is_one_shot_per_below_to_threshold_crossing() {
        assert_eq!(ACTIVE_MEMBER_WARNING_THRESHOLD, MAX_ACTIVE_MEMBERS - 1);
        assert!(active_member_warning_crossed(
            Some(ACTIVE_MEMBER_WARNING_THRESHOLD - 1),
            ACTIVE_MEMBER_WARNING_THRESHOLD
        ));
        assert!(active_member_warning_crossed(
            Some(ACTIVE_MEMBER_WARNING_THRESHOLD - 1),
            MAX_ACTIVE_MEMBERS
        ));
        assert!(!active_member_warning_crossed(
            Some(ACTIVE_MEMBER_WARNING_THRESHOLD),
            ACTIVE_MEMBER_WARNING_THRESHOLD
        ));
        assert!(!active_member_warning_crossed(
            Some(ACTIVE_MEMBER_WARNING_THRESHOLD),
            MAX_ACTIVE_MEMBERS
        ));
        assert!(!active_member_warning_crossed(
            Some(MAX_ACTIVE_MEMBERS),
            ACTIVE_MEMBER_WARNING_THRESHOLD - 1
        ));
        assert!(!active_member_warning_crossed(
            None,
            ACTIVE_MEMBER_WARNING_THRESHOLD
        ));
    }
}
