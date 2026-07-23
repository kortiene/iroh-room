//! Deterministic member projection from folded governance state + the sparse
//! Merkle map over member leaves (spec §6.5 / §4 D7 / #151).
//!
//! Projection is computed **only** from accepted governance state, in
//! deterministic member-id order (bytewise over raw public-key bytes). Each
//! [`MemberLeaf`] is canonically encoded and inserted into a [`MerkleMap`]; the
//! map root is the `member_root` committed to by the state root and checkpoints.
//!
//! Inclusion/exclusion proofs verify without access to the full state.

use crate::cbor::CborValue;
use crate::governance::model::{GovernanceState, MemberStatus};
use crate::ids::{MemberId, MerkleRoot};
use crate::member::merkle::{leaf_hash, map_key, value_hash, MerkleMap};

// Re-export the map root newtype path used by callers.
pub use crate::ids::MerkleRoot as MemberRoot;
use crate::member::merkle::Hash;

/// A projected member leaf (spec §6.5). Canonical-CBOR encoded for hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberLeaf {
    /// The member principal id.
    pub member_id: MemberId,
    /// The member's status.
    pub status: MemberStatus,
    /// The member's role.
    pub role: crate::governance::model::Role,
    /// The device key(s) bound to this member (OQ-2 single-key model: one).
    pub device_keys: Vec<crate::ids::DeviceId>,
    /// The governance entry id that last touched this member.
    pub governance_cursor: crate::ids::GovernanceEntryId,
}

impl MemberLeaf {
    /// Canonical-CBOR encoding of this leaf (the hashed value).
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "member_id".to_owned(),
                CborValue::Bytes(self.member_id.as_bytes().to_vec()),
            ),
            (
                "status".to_owned(),
                CborValue::Text(status_str(self.status).to_owned()),
            ),
            (
                "role".to_owned(),
                CborValue::Text(self.role.as_str().to_owned()),
            ),
            (
                "device_keys".to_owned(),
                CborValue::Array(
                    self.device_keys
                        .iter()
                        .map(|d| CborValue::Bytes(d.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            (
                "governance_cursor".to_owned(),
                CborValue::Bytes(self.governance_cursor.as_bytes().to_vec()),
            ),
        ])
    }
}

/// The full member projection + its Merkle root (spec §6.5).
#[derive(Debug, Clone)]
pub struct MemberProjection {
    /// Members in deterministic member-id order.
    pub members: Vec<MemberLeaf>,
    /// The sparse Merkle-map root over the members.
    pub root: MerkleRoot,
    /// The backing map (for proof generation).
    pub map: MerkleMap,
}

/// Project the member set from accepted governance state (spec §6.5 / #151).
///
/// Removed members are excluded from the projection (they carry no privileges
/// and the spec §6.5 requires the root to change on any semantically relevant
/// member-state change; including removed members would mask re-adds). Active
/// and invited members are included.
#[must_use]
pub fn project(state: &GovernanceState) -> (MerkleRoot, MemberProjection) {
    let mut members: Vec<MemberLeaf> = state
        .members
        .values()
        .filter(|m| m.status != MemberStatus::Removed)
        .map(|m| MemberLeaf {
            member_id: m.member_id,
            status: m.status,
            role: m.role,
            device_keys: m.devices.clone(),
            governance_cursor: m.governance_cursor,
        })
        .collect();
    // Deterministic order by raw member-id bytes (spec §6.5).
    members.sort_by_key(|m| *m.member_id.as_bytes());

    let mut map = MerkleMap::new();
    for leaf in &members {
        let key = map_key(leaf.member_id.as_bytes());
        let vh = value_hash(&leaf.to_cbor());
        let lh = leaf_hash(&key, &vh);
        map.insert_hash(key, lh);
    }
    let root = map.root();
    (root, MemberProjection { members, root, map })
}

fn status_str(s: MemberStatus) -> &'static str {
    match s {
        MemberStatus::Active => "active",
        MemberStatus::Invited => "invited",
        MemberStatus::Removed => "removed",
    }
}

/// Phantom use to keep the `Hash` import meaningful for proof helpers below.
#[allow(dead_code)]
type _Hash = Hash;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::model::{
        GovernanceAction, GovernanceEntryBody, MemberRecord, MemberStatus, Role,
    };
    use crate::ids::{DeviceId, GovernanceEntryId, RoomId, LEN};
    use std::collections::BTreeMap;

    fn state_with(members: Vec<MemberRecord>) -> GovernanceState {
        let mut s = GovernanceState::empty(RoomId::from_bytes([0x40; LEN]));
        s.members = members
            .into_iter()
            .map(|m| (m.member_id, m))
            .collect::<BTreeMap<_, _>>();
        s
    }

    fn rec(id: MemberId, status: MemberStatus, role: Role) -> MemberRecord {
        MemberRecord {
            member_id: id,
            role,
            status,
            devices: vec![DeviceId::from_bytes([0; LEN])],
            governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
        }
    }

    #[test]
    fn projection_excludes_removed_and_orders_by_member_id() {
        let a = MemberId::from_bytes([0x02; LEN]);
        let b = MemberId::from_bytes([0x01; LEN]);
        let removed = MemberId::from_bytes([0x03; LEN]);
        let state = state_with(vec![
            rec(a, MemberStatus::Active, Role::Member),
            rec(b, MemberStatus::Invited, Role::Member),
            rec(removed, MemberStatus::Removed, Role::None),
        ]);
        let (root, proj) = project(&state);
        assert_eq!(proj.members.len(), 2);
        // Sorted by raw bytes: b (0x01) before a (0x02).
        assert_eq!(proj.members[0].member_id, b);
        assert_eq!(proj.members[1].member_id, a);
        // Inclusion proof verifies against the root.
        let proof = proj.map.prove_inclusion(b.as_bytes()).expect("b is set");
        proof.verify(&root, true).expect("inclusion verifies");
    }

    #[test]
    fn member_state_change_changes_root() {
        let a = MemberId::from_bytes([0x02; LEN]);
        let s1 = state_with(vec![rec(a, MemberStatus::Invited, Role::Member)]);
        let s2 = state_with(vec![rec(a, MemberStatus::Active, Role::Member)]);
        assert_ne!(project(&s1).0, project(&s2).0);
    }

    #[test]
    fn projection_is_independent_of_member_map_insertion_order() {
        let a = MemberId::from_bytes([0x02; LEN]);
        let b = MemberId::from_bytes([0x01; LEN]);
        // Two states whose BTreeMaps differ only in insertion order.
        let mut s1 = state_with(vec![rec(a, MemberStatus::Active, Role::Member)]);
        let mut s2 = state_with(vec![rec(b, MemberStatus::Active, Role::Member)]);
        s1.members
            .insert(b, rec(b, MemberStatus::Active, Role::Member));
        s2.members
            .insert(a, rec(a, MemberStatus::Active, Role::Member));
        assert_eq!(project(&s1).0, project(&s2).0);
    }

    #[test]
    fn exclusion_proof_for_absent_member() {
        let a = MemberId::from_bytes([0x02; LEN]);
        let absent = MemberId::from_bytes([0xee; LEN]);
        let state = state_with(vec![rec(a, MemberStatus::Active, Role::Member)]);
        let (root, proj) = project(&state);
        let proof = proj.map.prove_exclusion(absent.as_bytes());
        assert!(proof.leaf.is_none());
        proof.verify(&root, false).expect("exclusion verifies");
    }

    #[test]
    fn full_entry_round_trip_projects_admin() {
        // End-to-end: genesis fold → projection contains the admin as Active.
        use crate::governance::fold::GovernanceFold;
        let room = RoomId::from_bytes([0x40; LEN]);
        let admin_key = crate::keys::SigningKey::from_seed(&[0xa0; LEN]);
        let g = GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: admin_key.member_id(),
            seq: 1,
            parent: None,
            epoch: 1,
            action: GovernanceAction::InitRoom {
                admin: admin_key.member_id(),
                admin_device: admin_key.device_id(),
                room_name: "r".to_owned(),
            },
        };
        let outcome = GovernanceFold::new().entry(g).finish().unwrap();
        let (_root, proj) = project(&outcome.state);
        assert_eq!(proj.members.len(), 1);
        assert_eq!(proj.members[0].status, MemberStatus::Active);
        assert_eq!(proj.members[0].role, Role::Admin);
    }
}
