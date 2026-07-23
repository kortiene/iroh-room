//! Deterministic governance state-root computation (spec §4 D4 / §6.4 / #147).
//!
//! `state_root = BLAKE3(GOVERNANCE_STATE_ROOT_CONTEXT || canonical_state_bytes)`.
//!
//! The canonical state commits to everything that affects authorization (spec
//! §11): the room id, the admin, the member set (role/status/device/cursor), the
//! per-author governance tips, the unresolved fork evidence (OQ-4: forks are
//! hash-visible so peers cannot silently disagree), the approval policy, and the
//! member Merkle root. For identical input the root is byte-identical and
//! arrival-order-independent.

use crate::cbor::CborValue;
use crate::domain;
use crate::governance::model::{ApprovalPolicy, GovernanceState};
use crate::ids::{MerkleRoot, StateRoot};

/// Compute the governance state root for `state`, given its recomputed member
/// Merkle root (spec §6.4 / #150). The member root is supplied (not recomputed
/// here) to keep this module free of a dependency on the projection layer.
#[must_use]
pub fn compute(state: &GovernanceState, member_root: &MerkleRoot) -> StateRoot {
    StateRoot::from_bytes(domain::blake3_domain(
        domain::GOVERNANCE_STATE_ROOT,
        &canonical_state_bytes(state, member_root),
    ))
}

/// Recompute the state root and compare to a supplied root.
///
/// # Errors
/// Returns [`crate::Reject::StateRootMismatch`] if the supplied root differs.
pub fn verify(
    state: &GovernanceState,
    member_root: &MerkleRoot,
    expected: &StateRoot,
) -> Result<(), crate::Reject> {
    if compute(state, member_root) == *expected {
        Ok(())
    } else {
        Err(crate::Reject::StateRootMismatch)
    }
}

/// Canonical-encode the state (the hashed payload). Public so the checkpoint and
/// snapshot layers can hash a superset consistently.
#[must_use]
pub fn canonical_state_bytes(state: &GovernanceState, member_root: &MerkleRoot) -> Vec<u8> {
    crate::cbor::encode(&canonical_state_value(state, member_root))
}

/// The canonical [`CborValue`] of the state. Fields are emitted in a fixed
/// canonical order.
#[must_use]
pub fn canonical_state_value(state: &GovernanceState, member_root: &MerkleRoot) -> CborValue {
    let admin = match state.admin {
        Some(a) => CborValue::Bytes(a.as_bytes().to_vec()),
        None => CborValue::Array(vec![]), // omit-when-absent → empty array marker
    };
    let members = CborValue::Array(state.members.values().map(member_to_cbor).collect());
    let tips = CborValue::Array(
        state
            .tips
            .iter()
            .map(|(author, (seq, id))| {
                CborValue::Map(vec![
                    (
                        "author".to_owned(),
                        CborValue::Bytes(author.as_bytes().to_vec()),
                    ),
                    ("seq".to_owned(), CborValue::Uint(*seq)),
                    ("tip".to_owned(), CborValue::Bytes(id.as_bytes().to_vec())),
                ])
            })
            .collect(),
    );
    let forks = CborValue::Array(state.forks.iter().map(fork_to_cbor).collect());
    CborValue::Map(vec![
        (
            "room_id".to_owned(),
            CborValue::Bytes(state.room_id.as_bytes().to_vec()),
        ),
        ("admin".to_owned(), admin),
        ("members".to_owned(), members),
        ("tips".to_owned(), tips),
        ("forks".to_owned(), forks),
        ("policy".to_owned(), policy_to_cbor(&state.policy)),
        (
            "member_root".to_owned(),
            CborValue::Bytes(member_root.as_bytes().to_vec()),
        ),
    ])
}

fn member_to_cbor(m: &crate::governance::model::MemberRecord) -> CborValue {
    CborValue::Map(vec![
        (
            "member_id".to_owned(),
            CborValue::Bytes(m.member_id.as_bytes().to_vec()),
        ),
        (
            "role".to_owned(),
            CborValue::Text(m.role.as_str().to_owned()),
        ),
        (
            "status".to_owned(),
            CborValue::Text(
                match m.status {
                    crate::governance::model::MemberStatus::Active => "active",
                    crate::governance::model::MemberStatus::Invited => "invited",
                    crate::governance::model::MemberStatus::Removed => "removed",
                }
                .to_owned(),
            ),
        ),
        (
            "devices".to_owned(),
            CborValue::Array(
                m.devices
                    .iter()
                    .map(|d| CborValue::Bytes(d.as_bytes().to_vec()))
                    .collect(),
            ),
        ),
        (
            "governance_cursor".to_owned(),
            CborValue::Bytes(m.governance_cursor.as_bytes().to_vec()),
        ),
    ])
}

fn fork_to_cbor(f: &crate::governance::model::ForkEvidence) -> CborValue {
    CborValue::Map(vec![
        (
            "author".to_owned(),
            CborValue::Bytes(f.author.as_bytes().to_vec()),
        ),
        (
            "conflicting".to_owned(),
            CborValue::Array(
                f.conflicting
                    .iter()
                    .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                    .collect(),
            ),
        ),
        ("seq".to_owned(), CborValue::Uint(f.seq)),
        (
            "resolved".to_owned(),
            CborValue::Uint(u64::from(f.resolved)),
        ),
    ])
}

fn policy_to_cbor(policy: &ApprovalPolicy) -> CborValue {
    match policy {
        ApprovalPolicy::AdminAlone => CborValue::Map(vec![(
            "type".to_owned(),
            CborValue::Text("admin_alone".to_owned()),
        )]),
        ApprovalPolicy::MOfN { m, approvers } => CborValue::Map(vec![
            ("type".to_owned(), CborValue::Text("m_of_n".to_owned())),
            ("m".to_owned(), CborValue::Uint(*m)),
            (
                "approvers".to_owned(),
                CborValue::Array(
                    approvers
                        .iter()
                        .map(|a| CborValue::Bytes(a.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::model::{MemberRecord, MemberStatus, Role};
    use crate::ids::{DeviceId, GovernanceEntryId, MemberId, RoomId, LEN};
    use std::collections::BTreeMap;

    #[test]
    fn empty_state_root_is_deterministic() {
        let room = RoomId::from_bytes([0x50; LEN]);
        let state = GovernanceState::empty(room);
        let mroot = MerkleRoot::from_bytes([0; LEN]);
        let a = compute(&state, &mroot);
        let b = compute(&state, &mroot);
        assert_eq!(a, b);
    }

    #[test]
    fn state_root_changes_with_membership() {
        let room = RoomId::from_bytes([0x50; LEN]);
        let member = MemberId::from_bytes([0x51; LEN]);
        let mut state = GovernanceState::empty(room);
        let mroot = MerkleRoot::from_bytes([0; LEN]);
        let before = compute(&state, &mroot);

        state.admin = Some(member);
        state.members.insert(
            member,
            MemberRecord {
                member_id: member,
                role: Role::Admin,
                status: MemberStatus::Active,
                devices: vec![DeviceId::from_bytes([0; LEN])],
                governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
            },
        );
        let after = compute(&state, &mroot);
        assert_ne!(
            before, after,
            "membership change must change the state root"
        );
    }

    #[test]
    fn state_root_is_member_order_independent() {
        let room = RoomId::from_bytes([0x50; LEN]);
        let mroot = MerkleRoot::from_bytes([0; LEN]);
        let a = MemberId::from_bytes([0x61; LEN]);
        let b = MemberId::from_bytes([0x62; LEN]);

        // Build two states whose BTreeMaps differ only in insertion order.
        let mut s1 = GovernanceState::empty(room);
        let mut s2 = GovernanceState::empty(room);
        let rec = |id| MemberRecord {
            member_id: id,
            role: Role::Member,
            status: MemberStatus::Active,
            devices: vec![DeviceId::from_bytes([0; LEN])],
            governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
        };
        // BTreeMap ignores insertion order, but assert equivalence explicitly.
        s1.members = BTreeMap::from([(a, rec(a)), (b, rec(b))]);
        s2.members = BTreeMap::from([(b, rec(b)), (a, rec(a))]);
        assert_eq!(compute(&s1, &mroot), compute(&s2, &mroot));
    }

    #[test]
    fn state_root_includes_unresolved_forks() {
        let room = RoomId::from_bytes([0x50; LEN]);
        let mroot = MerkleRoot::from_bytes([0; LEN]);
        let author = MemberId::from_bytes([0x63; LEN]);
        let mut state = GovernanceState::empty(room);
        state.admin = Some(author);
        let before = compute(&state, &mroot);
        state.forks.push(crate::governance::model::ForkEvidence {
            author,
            conflicting: [
                GovernanceEntryId::from_bytes([1; LEN]),
                GovernanceEntryId::from_bytes([2; LEN]),
            ],
            seq: 2,
            resolved: false,
        });
        let after = compute(&state, &mroot);
        assert_ne!(before, after, "fork evidence must be hash-visible (OQ-4)");
    }

    #[test]
    fn verify_rejects_mismatch() {
        let state = GovernanceState::empty(RoomId::from_bytes([0x50; LEN]));
        let mroot = MerkleRoot::from_bytes([0; LEN]);
        let expected = StateRoot::from_bytes([0xff; LEN]);
        assert_eq!(
            verify(&state, &mroot, &expected).err(),
            Some(crate::Reject::StateRootMismatch)
        );
    }
}
