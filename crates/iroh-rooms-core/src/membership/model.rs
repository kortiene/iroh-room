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

    /// Iterate every known member (any status) in deterministic identity order.
    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }
}
