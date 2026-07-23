//! The pure authorization engine over a folded [`GovernanceState`] (spec §4 D5,
//! §7.4 / #148).
//!
//! The engine is **default deny**: every unknown or unsupported action rejects
//! with [`Reject::InsufficientAuthorization`]. It consults only the supplied
//! folded state plus the approvals carried with the entry — never a wall clock or
//! an external store (spec D5). Unresolved fork state on the author fails closed
//! with [`Reject::UnresolvedFork`] (spec D6).

use crate::error::Reject;
use crate::governance::approval::ApprovalBody;
use crate::governance::model::{
    ApprovalPolicy, GovernanceAction, GovernanceEntryBody, GovernanceState, MemberStatus, Role,
};

/// The set of approvals supplied alongside a governance entry to the fold.
pub type ApprovalSet<'a> = &'a [ApprovalBody];

/// Authorize a governance entry against a folded state + its approval set (spec
/// §7.4 / D5).
///
/// # Errors
/// - [`Reject::UnresolvedFork`] — the author has unresolved fork evidence (D6).
/// - [`Reject::InsufficientAuthorization`] — the signer/role/approval policy is
///   not satisfied; also the default-deny outcome for any unknown action.
/// - [`Reject::InvalidContent`] — a structurally invalid action (e.g. removing
///   the immutable admin).
pub fn authorize_governance_entry(
    state: &GovernanceState,
    entry: &GovernanceEntryBody,
    approvals: ApprovalSet<'_>,
) -> Result<(), Reject> {
    // D6: fail closed on any unresolved fork attributed to this author.
    if state.has_unresolved_fork(&entry.author) {
        return Err(Reject::UnresolvedFork);
    }

    match &entry.action {
        // Genesis self-authorizes: the first entry establishes the admin. The
        // fold enforces "first entry" + "author == admin"; here we only check the
        // admin field is well-formed and matches the author.
        GovernanceAction::InitRoom { admin, .. } => {
            if admin != &entry.author {
                return Err(Reject::InsufficientAuthorization);
            }
            Ok(())
        }
        // Admin-gated membership writes (single-admin MVP; OQ-3).
        GovernanceAction::AddMember { .. }
        | GovernanceAction::RemoveMember { .. }
        | GovernanceAction::SetRole { .. }
        | GovernanceAction::RotateDevice { .. }
        | GovernanceAction::SetPolicy { .. } => {
            // The immutable admin cannot be removed (spec §11 invariant).
            if let GovernanceAction::RemoveMember { member } = &entry.action {
                if state.admin.as_ref() == Some(member) {
                    return Err(Reject::InvalidContent);
                }
            }
            authorize_admin_or_policy(state, &entry.author, approvals)
        }
    }
}

/// Resolve the active policy for the room and check the signer + approvals.
fn authorize_admin_or_policy(
    state: &GovernanceState,
    signer: &crate::MemberId,
    approvals: ApprovalSet<'_>,
) -> Result<(), Reject> {
    match &state.policy {
        ApprovalPolicy::AdminAlone => {
            if state.admin.as_ref() == Some(signer) {
                Ok(())
            } else {
                Err(Reject::InsufficientAuthorization)
            }
        }
        ApprovalPolicy::MOfN { m, approvers } => {
            // The signer may also be an approver (self-approval allowed iff in set).
            let mut count: u64 = 0;
            for approver in approvers {
                if approver == signer {
                    count += 1;
                }
                for a in approvals {
                    if &a.approver == approver {
                        count += 1;
                        break;
                    }
                }
            }
            if count >= *m {
                Ok(())
            } else {
                Err(Reject::InsufficientAuthorization)
            }
        }
    }
}

/// Authorize a content event body against a folded governance state (spec §6.5
/// / D5 / #152 §6). Returns the authorization verdict for the author; body-only
/// validation lives in [`crate::content`].
///
/// # Errors
/// - [`Reject::UnresolvedFork`] — the author has unresolved fork evidence.
/// - [`Reject::InsufficientAuthorization`] — the author is not an active member.
pub fn authorize_content_body(
    state: &GovernanceState,
    author: &crate::MemberId,
) -> Result<(), Reject> {
    if state.has_unresolved_fork(author) {
        return Err(Reject::UnresolvedFork);
    }
    if state.is_active(author) {
        Ok(())
    } else {
        Err(Reject::InsufficientAuthorization)
    }
}

/// Authorize an admin-only moderation action (content tombstone / block), used
/// by the content layer's cross-field checks (spec #158 §6). The signer must be
/// the room admin.
///
/// # Errors
/// Returns [`Reject::InsufficientAuthorization`] if the signer is not the admin.
pub fn authorize_admin_only(
    state: &GovernanceState,
    signer: &crate::MemberId,
) -> Result<(), Reject> {
    if state.has_unresolved_fork(signer) {
        return Err(Reject::UnresolvedFork);
    }
    if state.admin.as_ref() == Some(signer) {
        Ok(())
    } else {
        Err(Reject::InsufficientAuthorization)
    }
}

/// Authorize a `fork.resolve` record against a folded state (spec §4 D6 / #149).
///
/// A fork resolution mutates authorization-relevant state (it clears equivocation
/// evidence and drops the losing branch), so it MUST be authorized: only the
/// immutable room admin may resolve a fork. Unlike other admin-gated actions this
/// deliberately does **not** fail closed on the signer's own unresolved fork —
/// the admin resolving *their own* equivocation is the intended path, and
/// blocking it would make forks unrecoverable.
///
/// # Errors
/// - [`Reject::InsufficientAuthorization`] — the signer is not the room admin, or
///   the room has no admin yet (pre-genesis).
pub fn authorize_fork_resolution(
    state: &GovernanceState,
    signer: &crate::MemberId,
) -> Result<(), Reject> {
    if state.admin.as_ref() == Some(signer) {
        Ok(())
    } else {
        Err(Reject::InsufficientAuthorization)
    }
}

/// Convenience: the effective role of a member in a state (`None` if absent).
#[must_use]
pub fn role_of(state: &GovernanceState, member: &crate::MemberId) -> Option<Role> {
    state.member(member).map(|m| m.role)
}

/// Whether a member is active with at least the given role.
#[must_use]
pub fn is_active_with_role(state: &GovernanceState, member: &crate::MemberId, role: Role) -> bool {
    state
        .member(member)
        .is_some_and(|m| m.status == MemberStatus::Active && m.role >= role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::model::{
        GovernanceAction, GovernanceEntryBody, MemberRecord, MemberStatus,
    };
    use crate::ids::{DeviceId, GovernanceEntryId, MemberId, RoomId, LEN};
    use crate::MemberId as P;

    fn empty_state() -> GovernanceState {
        GovernanceState::empty(RoomId::from_bytes([0x70; LEN]))
    }

    fn init_entry(author: MemberId) -> GovernanceEntryBody {
        GovernanceEntryBody {
            schema_version: 2,
            room_id: RoomId::from_bytes([0x70; LEN]),
            author,
            seq: 1,
            parent: None,
            epoch: 1,
            action: GovernanceAction::InitRoom {
                admin: author,
                admin_device: DeviceId::from_bytes(*author.as_bytes()),
                room_name: "r".to_owned(),
            },
        }
    }

    #[test]
    fn genesis_self_authorizes() {
        let state = empty_state();
        let admin = P::from_bytes([0xa0; LEN]);
        let entry = init_entry(admin);
        assert!(authorize_governance_entry(&state, &entry, &[]).is_ok());
    }

    #[test]
    fn genesis_with_mismatched_admin_denied() {
        let state = empty_state();
        let author = P::from_bytes([0xa0; LEN]);
        let other = P::from_bytes([0xa1; LEN]);
        let mut entry = init_entry(author);
        entry.action = GovernanceAction::InitRoom {
            admin: other,
            admin_device: DeviceId::from_bytes([0; LEN]),
            room_name: "r".to_owned(),
        };
        assert_eq!(
            authorize_governance_entry(&state, &entry, &[]).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    #[test]
    fn non_admin_membership_write_denied() {
        // State with an admin but no other members; a non-admin tries AddMember.
        let admin = P::from_bytes([0xa0; LEN]);
        let mut state = empty_state();
        state.admin = Some(admin);
        state.members.insert(
            admin,
            MemberRecord {
                member_id: admin,
                role: Role::Admin,
                status: MemberStatus::Active,
                devices: vec![DeviceId::from_bytes([0; LEN])],
                governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
            },
        );
        let imposter = P::from_bytes([0xb0; LEN]);
        let entry = GovernanceEntryBody {
            schema_version: 2,
            room_id: state.room_id,
            author: imposter,
            seq: 1,
            parent: None,
            epoch: 2,
            action: GovernanceAction::AddMember {
                member: P::from_bytes([0xc0; LEN]),
                device: DeviceId::from_bytes([0; LEN]),
                role: Role::Member,
            },
        };
        assert_eq!(
            authorize_governance_entry(&state, &entry, &[]).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    #[test]
    fn admin_membership_write_authorized() {
        let admin = P::from_bytes([0xa0; LEN]);
        let mut state = empty_state();
        state.admin = Some(admin);
        state.members.insert(
            admin,
            MemberRecord {
                member_id: admin,
                role: Role::Admin,
                status: MemberStatus::Active,
                devices: vec![DeviceId::from_bytes([0; LEN])],
                governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
            },
        );
        let entry = GovernanceEntryBody {
            schema_version: 2,
            room_id: state.room_id,
            author: admin,
            seq: 2,
            parent: Some(GovernanceEntryId::from_bytes([0; LEN])),
            epoch: 2,
            action: GovernanceAction::AddMember {
                member: P::from_bytes([0xc0; LEN]),
                device: DeviceId::from_bytes([0; LEN]),
                role: Role::Member,
            },
        };
        assert!(authorize_governance_entry(&state, &entry, &[]).is_ok());
    }

    #[test]
    fn removing_admin_is_invalid_content() {
        let admin = P::from_bytes([0xa0; LEN]);
        let mut state = empty_state();
        state.admin = Some(admin);
        state.members.insert(
            admin,
            MemberRecord {
                member_id: admin,
                role: Role::Admin,
                status: MemberStatus::Active,
                devices: vec![DeviceId::from_bytes([0; LEN])],
                governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
            },
        );
        let entry = GovernanceEntryBody {
            schema_version: 2,
            room_id: state.room_id,
            author: admin,
            seq: 2,
            parent: None,
            epoch: 3,
            action: GovernanceAction::RemoveMember { member: admin },
        };
        assert_eq!(
            authorize_governance_entry(&state, &entry, &[]).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn unresolved_fork_fails_closed() {
        let admin = P::from_bytes([0xa0; LEN]);
        let mut state = empty_state();
        state.admin = Some(admin);
        state.forks.push(crate::governance::model::ForkEvidence {
            author: admin,
            conflicting: [
                GovernanceEntryId::from_bytes([1; LEN]),
                GovernanceEntryId::from_bytes([2; LEN]),
            ],
            seq: 2,
            resolved: false,
        });
        let entry = GovernanceEntryBody {
            schema_version: 2,
            room_id: state.room_id,
            author: admin,
            seq: 3,
            parent: None,
            epoch: 4,
            action: GovernanceAction::AddMember {
                member: P::from_bytes([0xc0; LEN]),
                device: DeviceId::from_bytes([0; LEN]),
                role: Role::Member,
            },
        };
        assert_eq!(
            authorize_governance_entry(&state, &entry, &[]).err(),
            Some(Reject::UnresolvedFork)
        );
    }

    #[test]
    fn content_author_requires_active_membership() {
        let state = empty_state();
        let nonmember = P::from_bytes([0xd0; LEN]);
        assert_eq!(
            authorize_content_body(&state, &nonmember).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }
}
