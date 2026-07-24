//! The normative #134 §7.4 five-rule authorization predicate for a
//! post-genesis governance entry (issue #148).
//!
//! `validate_governance_entry(prev_state, entry)` evaluates, in fixed order:
//!
//! 1. the predecessor is a previously validated state and its committed
//!    `state_root` equals a fresh root computation (and the candidate targets
//!    the same community);
//! 2. the candidate has the exact next sequence number and exact predecessor
//!    id;
//! 3. the operation is structurally and semantically valid when applied to
//!    state `n-1`;
//! 4. the union of the verified entry signer and verified approval signers
//!    contains at least the old state's threshold of distinct old-state
//!    administrators;
//! 5. deterministic apply to state `n-1` produces the candidate's declared
//!    post-state root.
//!
//! Authorization always reads the *old* state (spec D6): an `admin.set`
//! proposal is authorized under the old administrator set and old threshold.
//! The proposed set/threshold become authoritative only after
//! [`validate_and_apply_governance_entry`] returns the next accepted
//! snapshot — a successful call to the unit-returning
//! [`validate_governance_entry`] never mutates `prev_state`.
//!
//! This module is pure: no wall-clock, network, store, async, logging, or
//! randomness. Out of scope (deferred to #149): detecting two quorum-valid
//! entries sharing a predecessor, branch selection, and recovery-threshold
//! authorization (`fork.resolve` is authorized like every other ordinary
//! operation, under the current admin threshold, until #149 replaces that
//! rule deliberately).

use std::collections::BTreeSet;

use crate::error::Reject;
use crate::ids::{GovernanceId, PrincipalId, StateRoot};

use super::genesis::{verify_genesis, GenesisConfig, GenesisSignature};
use super::model::AdministratorState;
use super::records::{entry_id, VerifiedGovernanceEntry};
use super::state::{
    apply, check_chain_link, compute_state_root, verify_state_root, GovernanceState,
};

/// Public alias for the stable rejection taxonomy (issue #148: no new public
/// error variant is needed; the existing [`Reject`] codes already cover every
/// rule-isolated rejection in the §7.4 table).
pub type RejectionReason = Reject;

/// The local cursor into an accepted governance log (issue #148 D3): either
/// the verified-genesis boundary, or the last accepted entry's sequence + id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceTip {
    /// No post-genesis entry has been accepted yet.
    Genesis,
    /// The last accepted entry.
    Entry {
        /// The accepted entry's sequence number.
        seq: u64,
        /// The accepted entry's id (the expected `prev` for the next entry).
        id: GovernanceId,
    },
}

/// An accepted governance state snapshot together with its local validation
/// cursor (issue #148 D3).
///
/// Opaque: the only ways to construct one are [`validated_genesis_state`] and
/// a successful [`validate_and_apply_governance_entry`]. This closes the trust
/// gap a bare [`GovernanceState`] has — it carries neither the predecessor
/// sequence nor the predecessor entry id, so rule 2 (exact chain link) could
/// not otherwise be checked from the caller-supplied predecessor alone.
///
/// The `committed_state_root`/`tip` here are local validation evidence, not a
/// seventh governance state-root component: the committed root remains the
/// exact frozen six-component root over [`GovernanceState`] (spec §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedGovernanceState {
    state: GovernanceState,
    tip: GovernanceTip,
    committed_state_root: StateRoot,
}

impl ValidatedGovernanceState {
    /// The accepted governance state.
    #[must_use]
    pub fn state(&self) -> &GovernanceState {
        &self.state
    }

    /// The local validation cursor (genesis, or the last accepted entry).
    #[must_use]
    pub fn tip(&self) -> GovernanceTip {
        self.tip
    }

    /// The committed state root this snapshot was accepted at.
    #[must_use]
    pub fn committed_state_root(&self) -> &StateRoot {
        &self.committed_state_root
    }
}

/// Build the accepted genesis snapshot from a verified genesis config (issue
/// #148 D3). This is the only way to obtain a [`ValidatedGovernanceState`]
/// without a prior successful [`validate_and_apply_governance_entry`].
///
/// # Errors
/// Any error from [`verify_genesis`] (spec §5.1 — malformed genesis config or
/// an unmet admin-signature threshold).
pub fn validated_genesis_state(
    config: &GenesisConfig,
    signatures: &[GenesisSignature],
) -> Result<ValidatedGovernanceState, Reject> {
    let community_id = verify_genesis(config, signatures)?;
    let state = GovernanceState::from_genesis(config, community_id);
    let committed_state_root = compute_state_root(&state);
    Ok(ValidatedGovernanceState {
        state,
        tip: GovernanceTip::Genesis,
        committed_state_root,
    })
}

/// Old-state administrator invariants that must hold before threshold
/// counting can authorize anything (spec §4.2 rule 4 / D5): non-empty, sorted
/// unique, and `1 <= threshold <= administrators.len()`. A malformed old state
/// fails closed — it must never accidentally authorize by, e.g., treating a
/// zero threshold as "always satisfied".
fn old_administrator_invariants_hold(admins: &AdministratorState) -> bool {
    if admins.administrators.is_empty() || admins.threshold == 0 {
        return false;
    }
    let mut sorted = admins.administrators.clone();
    sorted.sort();
    sorted.dedup();
    if sorted != admins.administrators {
        return false;
    }
    match u16::try_from(admins.administrators.len()) {
        Ok(count) => admins.threshold <= count,
        Err(_) => false,
    }
}

/// Rule 4: the union of the verified entry signer and verified approval
/// signers must contain at least `admins.threshold` distinct members of
/// `admins.administrators` (issue #148 D5).
///
/// Uses a `BTreeSet` union so a signer that is also an approver counts once,
/// approval order never matters, and identities outside the old admin set
/// (including principals proposed by *this* entry's own `admin.set`) never
/// contribute — rule 4 reads only the supplied old `AdministratorState`.
///
/// # Errors
/// Returns [`Reject::InsufficientAuthorization`] if the old admin invariants
/// are violated (fail closed) or fewer than `threshold` distinct old admins
/// signed/approved.
fn verify_old_admin_threshold(
    admins: &AdministratorState,
    entry: &VerifiedGovernanceEntry,
) -> Result<(), Reject> {
    if !old_administrator_invariants_hold(admins) {
        return Err(Reject::InsufficientAuthorization);
    }
    let admin_set: BTreeSet<PrincipalId> = admins.administrators.iter().copied().collect();

    let mut signers: BTreeSet<PrincipalId> = BTreeSet::new();
    signers.insert(entry.signer());
    for approval in entry.approvals() {
        signers.insert(approval.approver);
    }

    let distinct_old_admin_signers = signers.intersection(&admin_set).count();
    // `admins.threshold` is `u16`; a `usize` count trivially fits for any
    // reachable admin-set size, but compare via checked conversion so an
    // adversarial (already-rejected-above) size can never panic.
    let meets_threshold =
        u16::try_from(distinct_old_admin_signers).map_or(true, |count| count >= admins.threshold); // more signers than fit in u16 ⇒ trivially met
    if meets_threshold {
        Ok(())
    } else {
        Err(Reject::InsufficientAuthorization)
    }
}

/// Evaluate the five rules in normative order and return the resulting
/// candidate state (issue #148 D4). Shared by [`validate_governance_entry`]
/// and [`validate_and_apply_governance_entry`] so the two can never disagree
/// about what is authorized.
///
/// No state is committed before rule 5 succeeds; the returned candidate is
/// never derived from `prev_state` being mutated.
fn validate_candidate(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<GovernanceState, Reject> {
    let body = entry.body();
    let old = prev_state.state();

    // Rule 1: predecessor validity — recomputed root must match the
    // committed root, and the candidate must target the same community.
    verify_state_root(old, prev_state.committed_state_root())?;
    if body.community_id != old.community_id {
        return Err(Reject::InvalidContent);
    }

    // Rule 2: exact next sequence number and exact predecessor id.
    let (expected_seq, expected_prev) = match prev_state.tip() {
        GovernanceTip::Genesis => (1u64, None),
        GovernanceTip::Entry { seq, id } => {
            (seq.checked_add(1).ok_or(Reject::InvalidContent)?, Some(id))
        }
    };
    check_chain_link(body, expected_prev, expected_seq)?;

    // Rule 3: the operation must be structurally/semantically valid when
    // applied to state n-1. `apply` never mutates `old` (clone-and-return).
    let candidate = apply(old, &body.payload)?;

    // Rule 4: a threshold of distinct *old*-state administrators signed.
    verify_old_admin_threshold(&old.administrators, entry)?;

    // Rule 5: the candidate's declared post-state root must match the
    // deterministic apply result.
    verify_state_root(&candidate, &body.state_root)?;

    Ok(candidate)
}

/// The #134 §7.4 five-rule authorization predicate (issue #148).
///
/// Pure: evaluates rules 1–5 in fixed order and returns `Ok(())` iff `entry`
/// is authorized to extend `prev_state`. Never mutates or commits state —
/// callers that want the next accepted snapshot must call
/// [`validate_and_apply_governance_entry`].
///
/// # Errors
/// See the module-level rule table; every rejection maps to an existing
/// [`Reject`] variant (no new taxonomy is introduced by this predicate).
pub fn validate_governance_entry(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<(), RejectionReason> {
    validate_candidate(prev_state, entry).map(|_candidate| ())
}

/// Validate `entry` against `prev_state` and, on success, return the next
/// accepted [`ValidatedGovernanceState`] (issue #148 D3/D6).
///
/// This is the only way to advance the accepted-state cursor: a new
/// administrator set proposed by `admin.set` becomes authoritative for
/// *subsequent* calls only through the snapshot this function returns, never
/// through the unit-returning [`validate_governance_entry`] alone.
///
/// # Errors
/// See [`validate_governance_entry`].
pub fn validate_and_apply_governance_entry(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<ValidatedGovernanceState, RejectionReason> {
    let candidate = validate_candidate(prev_state, entry)?;
    let body = entry.body();
    Ok(ValidatedGovernanceState {
        committed_state_root: body.state_root,
        tip: GovernanceTip::Entry {
            seq: body.seq,
            id: entry_id(body),
        },
        state: candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::super::genesis::GENESIS_SCHEMA_VERSION;
    use super::super::model::{CommunityPolicy, DeviceStatus, RecoveryConfig, Role};
    use super::super::operation::{
        AdminSet, DeviceGrant, DeviceRevoke, GovernanceOperationPayload, MemberGrant,
    };
    use super::super::records::{
        GovernanceApproval, GovernanceApprovalBody, GovernanceEntry, GovernanceEntryBody,
        VerifiedGovernanceEntry,
    };
    use super::*;
    use crate::ids::{DeviceId, LEN as N};
    use crate::keys::SigningKey;
    use proptest::prelude::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; N])
    }

    fn principal(seed: u8) -> PrincipalId {
        key(seed).member_id()
    }

    /// A 3-admin genesis (threshold 2), sorted so it validates deterministically.
    fn genesis_config() -> GenesisConfig {
        let mut admins = vec![principal(0xa0), principal(0xa1), principal(0xa2)];
        admins.sort();
        GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1_000,
            genesis_nonce: [0xab; N],
            admin_threshold: 2,
            administrators: admins,
            recovery: RecoveryConfig::empty(),
            replicas: Vec::new(),
            community_policy: CommunityPolicy::empty(),
        }
    }

    fn genesis_state() -> ValidatedGovernanceState {
        let cfg = genesis_config();
        let sigs = [
            super::super::genesis::sign_genesis(&cfg, &key(0xa0)),
            super::super::genesis::sign_genesis(&cfg, &key(0xa1)),
        ];
        validated_genesis_state(&cfg, &sigs).expect("genesis threshold met")
    }

    /// Seal a body + admin signatures into a [`VerifiedGovernanceEntry`],
    /// signing with `author` and attaching one approval per `approvers`.
    fn verified_entry(
        body: GovernanceEntryBody,
        author: &SigningKey,
        approvers: &[&SigningKey],
    ) -> VerifiedGovernanceEntry {
        let approvals = approvers
            .iter()
            .map(|approver| {
                GovernanceApproval::new(
                    GovernanceApprovalBody {
                        community_id: body.community_id,
                        entry_id: entry_id(&body),
                        state_root: body.state_root,
                        approver: approver.member_id(),
                        created_at_ms: body.created_at_ms + 1,
                    },
                    approver,
                )
            })
            .collect();
        let entry = GovernanceEntry::new(body, author, approvals);
        super::super::records::verify_governance_entry(&entry).expect("entry verifies")
    }

    /// A well-formed `member.grant` entry authorized by exactly `W` (2)
    /// distinct old admins: one signer + one approver.
    fn valid_member_grant_entry(prev: &ValidatedGovernanceState) -> VerifiedGovernanceEntry {
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        verified_entry(body, &key(0xa0), &[&key(0xa1)])
    }

    // --- Happy path ----------------------------------------------------

    #[test]
    fn valid_two_of_three_entry_is_authorized() {
        let prev = genesis_state();
        let entry = valid_member_grant_entry(&prev);
        assert!(validate_governance_entry(&prev, &entry).is_ok());

        let next =
            validate_and_apply_governance_entry(&prev, &entry).expect("authorized entry commits");
        assert!(next.state().members.contains_key(&principal(0xc0)));
        assert_eq!(
            next.tip(),
            GovernanceTip::Entry {
                seq: 1,
                id: entry_id(entry.body()),
            }
        );
        // Validation must not mutate the predecessor snapshot.
        assert!(!prev.state().members.contains_key(&principal(0xc0)));
    }

    // --- Rule-isolated negatives (spec §7) ------------------------------

    #[test]
    fn rule1_predecessor_root_mismatch_rejected() {
        let prev = genesis_state();
        let entry = valid_member_grant_entry(&prev);
        // Tamper the committed root only; the wrapper's `state` is untouched
        // so rules 2-5 would otherwise all succeed.
        let tampered = ValidatedGovernanceState {
            state: prev.state().clone(),
            tip: prev.tip(),
            committed_state_root: StateRoot::from_bytes([0xff; N]),
        };
        assert_eq!(
            validate_governance_entry(&tampered, &entry).err(),
            Some(Reject::StateRootMismatch)
        );
    }

    #[test]
    fn rule2_wrong_sequence_rejected() {
        // Canonical decode already enforces `seq == 1 <=> prev == None` (spec
        // §4.2: a later entry with no `prev` is rejected even earlier, at
        // decode), so an authorization-level rule-2 test must use a `seq`
        // that disagrees with the *expected chain position* while still
        // satisfying that body-level invariant: `seq != 1` with a `prev`.
        let prev = genesis_state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 2, // wrong: genesis expects seq == 1
            prev: Some(GovernanceId::from_bytes([0xaa; N])),
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn rule2_wrong_predecessor_id_rejected() {
        // Correct sequence (2), but a predecessor id that does not match the
        // accepted tip.
        let prev = genesis_state();
        let first = valid_member_grant_entry(&prev);
        let committed =
            validate_and_apply_governance_entry(&prev, &first).expect("first entry commits");

        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc1),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(committed.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: committed.state().community_id,
            seq: 2,
            prev: Some(GovernanceId::from_bytes([0xbb; N])), // wrong predecessor id
            created_at_ms: 3_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&committed, &entry).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn rule3_invalid_operation_rejected() {
        let prev = genesis_state();
        // device.revoke for an absent device: apply() rejects before root
        // computation, so the declared root can be anything.
        let payload = GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
            member_id: principal(0xa0),
            device_id: DeviceId::from_bytes([0xd0; N]),
        });
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0x00; N]),
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn rule4_w_minus_one_signatures_rejected() {
        let prev = genesis_state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        // Only the signer (1 distinct old admin); threshold is 2.
        let entry = verified_entry(body, &key(0xa0), &[]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    #[test]
    fn rule5_declared_root_mismatch_rejected() {
        let prev = genesis_state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0xee; N]), // wrong declared root
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::StateRootMismatch)
        );
    }

    // --- Threshold edges (spec §4.3 / acceptance) -----------------------

    #[test]
    fn exactly_w_signatures_accepted_and_superset_accepted() {
        let prev = genesis_state();
        let entry = valid_member_grant_entry(&prev); // exactly W=2
        assert!(validate_governance_entry(&prev, &entry).is_ok());

        // A superset (all three old admins) also accepts.
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc1),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let superset_entry = verified_entry(body, &key(0xa0), &[&key(0xa1), &key(0xa2)]);
        assert!(validate_governance_entry(&prev, &superset_entry).is_ok());
    }

    #[test]
    fn outsider_signer_with_w_admin_approvals_is_authorized() {
        // Assumption (spec §14 #3): the entry signer need not be an admin if
        // `W` distinct old admins supplied verified approvals.
        let prev = genesis_state();
        let outsider = key(0xee);
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &outsider, &[&key(0xa0), &key(0xa1)]);
        assert!(validate_governance_entry(&prev, &entry).is_ok());
    }

    #[test]
    fn outsider_approvals_are_ignored_not_counted() {
        let prev = genesis_state();
        let outsider = key(0xee);
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        // Signer is an old admin (1), one outsider approval (+0) — below W=2.
        let entry = verified_entry(body, &key(0xa0), &[&outsider]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    #[test]
    fn signer_also_approving_counts_once() {
        let prev = genesis_state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        // Signer (0xa0) also supplies an approval for itself: still only 1
        // distinct old admin, below threshold 2.
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa0)]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    // --- Genesis / sequence boundaries -----------------------------------

    #[test]
    fn u64_max_sequence_cannot_authorize_a_next_entry() {
        let prev = genesis_state();
        let at_max = ValidatedGovernanceState {
            state: prev.state().clone(),
            tip: GovernanceTip::Entry {
                seq: u64::MAX,
                id: GovernanceId::from_bytes([0x01; N]),
            },
            committed_state_root: *prev.committed_state_root(),
        };
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(at_max.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: at_max.state().community_id,
            seq: u64::MAX, // any seq is wrong: checked_add(1) on the expected side overflows
            prev: Some(GovernanceId::from_bytes([0x01; N])),
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&at_max, &entry).err(),
            Some(Reject::InvalidContent)
        );
    }

    // --- Admin-set transition: old set authorizes; new set post-commit only
    //     (spec D6, acceptance item 2) -----------------------------------

    #[test]
    fn admin_set_old_quorum_authorizes_new_quorum_effective_only_post_commit() {
        let prev = genesis_state();
        let new_admin = key(0xb0);
        let payload = GovernanceOperationPayload::AdminSet(AdminSet {
            administrators: vec![new_admin.member_id()],
            threshold: 1,
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };

        // The *new* admin signing alone cannot authorize this transition: the
        // proposed set is not yet effective, and it is not in the old set.
        let new_only = verified_entry(body.clone(), &new_admin, &[]);
        assert_eq!(
            validate_governance_entry(&prev, &new_only).err(),
            Some(Reject::InsufficientAuthorization)
        );

        // The *old* quorum (2-of-3 old admins) authorizes it.
        let old_quorum = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        let committed = validate_and_apply_governance_entry(&prev, &old_quorum)
            .expect("old quorum authorizes admin.set");
        assert_eq!(
            committed.state().administrators.administrators,
            vec![new_admin.member_id()]
        );

        // Now the *new* admin alone can authorize the next ordinary entry...
        let next_payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc9),
            role: Role::Member,
        });
        let next_declared = compute_state_root(&apply(committed.state(), &next_payload).unwrap());
        let next_body = GovernanceEntryBody {
            community_id: committed.state().community_id,
            seq: 2,
            prev: Some(match committed.tip() {
                GovernanceTip::Entry { id, .. } => id,
                GovernanceTip::Genesis => unreachable!(),
            }),
            created_at_ms: 3_000,
            kind: next_payload.kind(),
            payload: next_payload,
            state_root: next_declared,
        };
        let by_new_admin = verified_entry(next_body.clone(), &new_admin, &[]);
        assert!(validate_governance_entry(&committed, &by_new_admin).is_ok());

        // ...while the old-only admins (no longer administrators) cannot.
        let by_old_admin = verified_entry(next_body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&committed, &by_old_admin).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    // --- Empty/malformed old-admin fail-closed behavior -------------------

    #[test]
    fn malformed_old_admin_threshold_fails_closed() {
        let prev = genesis_state();
        let mut zero_threshold_state = prev.state().clone();
        zero_threshold_state.administrators.threshold = 0;
        let zero_threshold = ValidatedGovernanceState {
            committed_state_root: compute_state_root(&zero_threshold_state),
            tip: prev.tip(),
            state: zero_threshold_state,
        };
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = compute_state_root(&apply(zero_threshold.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: zero_threshold.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1), &key(0xa2)]);
        assert_eq!(
            validate_governance_entry(&zero_threshold, &entry).err(),
            Some(Reject::InsufficientAuthorization),
            "a zero threshold must fail closed, never trivially authorize"
        );
    }

    // --- Device-binding validity is a rule-3 concern (spec §4.4) ----------

    #[test]
    fn device_grant_and_revoke_transitions_are_rule3_gated() {
        let prev = genesis_state();
        // Grant a device to a non-existent member: rule 3 must reject.
        let payload = GovernanceOperationPayload::DeviceGrant(DeviceGrant {
            member_id: principal(0xc0),
            device_id: DeviceId::from_bytes([0xd0; N]),
        });
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0x00; N]),
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        assert_eq!(
            validate_governance_entry(&prev, &entry).err(),
            Some(Reject::InvalidContent)
        );

        // Granting a device to the (active) admin succeeds end to end.
        let admin = principal(0xa0);
        let payload = GovernanceOperationPayload::DeviceGrant(DeviceGrant {
            member_id: admin,
            device_id: DeviceId::from_bytes([0xd1; N]),
        });
        let declared = compute_state_root(&apply(prev.state(), &payload).unwrap());
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let entry = verified_entry(body, &key(0xa0), &[&key(0xa1)]);
        let next =
            validate_and_apply_governance_entry(&prev, &entry).expect("device.grant authorized");
        assert_eq!(
            next.state()
                .members
                .get(&admin)
                .unwrap()
                .devices
                .get(&DeviceId::from_bytes([0xd1; N]))
                .unwrap()
                .status,
            DeviceStatus::Active
        );
    }

    // --- Property tests (spec §8.2–§8.4, issue #148 acceptance) -----------
    //
    // These generalize the fixed-fixture negatives above over generated
    // admin-set sizes/thresholds and device/member pairs. Every case builds
    // and signs *real* Ed25519 records; a `VerifiedGovernanceEntry` is never
    // forged, so the properties exercise the exact public predicate path.

    /// Build an accepted genesis snapshot from `admin_keys` at `threshold`,
    /// signed by the first `threshold` (distinct) keys. Panics if the keys are
    /// not distinct enough to meet the threshold (the generators guarantee it).
    fn genesis_state_with(admin_keys: &[SigningKey], threshold: u16) -> ValidatedGovernanceState {
        let mut administrators: Vec<PrincipalId> =
            admin_keys.iter().map(SigningKey::member_id).collect();
        administrators.sort();
        administrators.dedup();
        let cfg = GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1_000,
            genesis_nonce: [0xab; N],
            admin_threshold: threshold,
            administrators,
            recovery: RecoveryConfig::empty(),
            replicas: Vec::new(),
            community_policy: CommunityPolicy::empty(),
        };
        let sigs: Vec<_> = admin_keys
            .iter()
            .take(usize::from(threshold))
            .map(|k| super::super::genesis::sign_genesis(&cfg, k))
            .collect();
        validated_genesis_state(&cfg, &sigs).expect("genesis threshold met")
    }

    /// Seal `payload` into a verified entry that extends `prev` with the exact
    /// next chain link (seq/prev derived from the accepted tip) and the
    /// deterministically-declared post-root. When `payload` is rule-3 invalid
    /// (apply fails), a placeholder root is used — rule 3 rejects before the
    /// rule-5 root check is reached.
    fn next_entry(
        prev: &ValidatedGovernanceState,
        payload: GovernanceOperationPayload,
        signer: &SigningKey,
        approvers: &[&SigningKey],
    ) -> VerifiedGovernanceEntry {
        let (seq, prev_id) = match prev.tip() {
            GovernanceTip::Genesis => (1u64, None),
            GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
        };
        let declared = apply(prev.state(), &payload).map_or_else(
            |_| StateRoot::from_bytes([0x00; N]),
            |s| compute_state_root(&s),
        );
        let body = GovernanceEntryBody {
            community_id: prev.state().community_id,
            seq,
            prev: prev_id,
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        verified_entry(body, signer, approvers)
    }

    /// A `member.grant` entry authorized by the distinct old admins in
    /// `admin_signers` (first signs, the rest approve) plus ignored
    /// `outsider_approvers`. When `admin_signers` is empty, `fallback_signer`
    /// (an outsider) signs so zero old admins contribute.
    fn grant_entry_with_signers(
        prev: &ValidatedGovernanceState,
        member: PrincipalId,
        admin_signers: &[&SigningKey],
        outsider_approvers: &[&SigningKey],
        fallback_signer: &SigningKey,
    ) -> VerifiedGovernanceEntry {
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: member,
            role: Role::Member,
        });
        match admin_signers.split_first() {
            Some((first, rest)) => {
                let mut approvers: Vec<&SigningKey> = rest.to_vec();
                approvers.extend_from_slice(outsider_approvers);
                next_entry(prev, payload, first, &approvers)
            }
            None => next_entry(prev, payload, fallback_signer, outsider_approvers),
        }
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 64, ..proptest::test_runner::Config::default()
        })]

        /// Threshold edges (spec §8.2 / acceptance "W-1 rejected, W accepted"):
        /// over generated admin-set sizes and thresholds, `W-1` distinct old
        /// admins are rejected, exactly `W` are accepted, and a full-set
        /// superset is accepted — with outsider signatures present but ignored.
        #[test]
        fn prop_threshold_w_minus_one_rejects_w_accepts(
            n in 1u8..=6,
            w_raw in 1u8..=6,
            outsiders in 0u8..=3,
        ) {
            let w = ((w_raw - 1) % n) + 1; // clamp into 1..=n
            let admin_keys: Vec<SigningKey> = (0..n).map(key).collect();
            let outsider_keys: Vec<SigningKey> = (0..outsiders).map(|i| key(0xf0 + i)).collect();
            let fallback = key(0xfe);
            let prev = genesis_state_with(&admin_keys, u16::from(w));

            let admin_refs: Vec<&SigningKey> = admin_keys.iter().collect();
            let outsider_refs: Vec<&SigningKey> = outsider_keys.iter().collect();
            let member = principal(0xc0);
            let wu = usize::from(w);
            let nu = usize::from(n);

            // Exactly W distinct old admins (+ ignored outsiders) → accepted.
            let e_w =
                grant_entry_with_signers(&prev, member, &admin_refs[..wu], &outsider_refs, &fallback);
            prop_assert!(validate_governance_entry(&prev, &e_w).is_ok());

            // W-1 distinct old admins → rejected; outsiders cannot make up the deficit.
            let e_wm1 = grant_entry_with_signers(
                &prev,
                member,
                &admin_refs[..wu - 1],
                &outsider_refs,
                &fallback,
            );
            prop_assert_eq!(
                validate_governance_entry(&prev, &e_wm1).err(),
                Some(Reject::InsufficientAuthorization)
            );

            // Full-set superset (all N old admins) → accepted.
            let e_n =
                grant_entry_with_signers(&prev, member, &admin_refs[..nu], &outsider_refs, &fallback);
            prop_assert!(validate_governance_entry(&prev, &e_n).is_ok());
        }

        /// Admin-set transition (spec §8.3 / D6 / acceptance "old set
        /// authorizes, new set only post-commit"): with disjoint old/new admin
        /// sets, the old quorum authorizes the `admin.set`, a new-only quorum
        /// cannot, and after commit the authority flips — the new quorum
        /// authorizes the next entry while the (now-outsider) old quorum cannot.
        #[test]
        fn prop_admin_set_disjoint_transition_flips_authority(
            n_old in 2u8..=4,
            w_old_raw in 1u8..=4,
            n_new in 1u8..=4,
            w_new_raw in 1u8..=4,
        ) {
            let w_old = ((w_old_raw - 1) % n_old) + 1;
            let w_new = ((w_new_raw - 1) % n_new) + 1;
            let old_keys: Vec<SigningKey> = (0..n_old).map(|i| key(0x10 + i)).collect();
            let new_keys: Vec<SigningKey> = (0..n_new).map(|i| key(0x80 + i)).collect();
            let prev = genesis_state_with(&old_keys, u16::from(w_old));

            let old_refs: Vec<&SigningKey> = old_keys.iter().collect();
            let new_refs: Vec<&SigningKey> = new_keys.iter().collect();
            let wo = usize::from(w_old);
            let wn = usize::from(w_new);

            let mut new_admins: Vec<PrincipalId> =
                new_keys.iter().map(SigningKey::member_id).collect();
            new_admins.sort();
            new_admins.dedup();
            let admin_set = GovernanceOperationPayload::AdminSet(AdminSet {
                administrators: new_admins.clone(),
                threshold: u16::from(w_new),
            });

            // A new-only quorum cannot authorize the transition: the proposed
            // admins are outsiders to the OLD state that must authorize it.
            let new_only = next_entry(&prev, admin_set.clone(), new_refs[0], &new_refs[1..wn]);
            prop_assert_eq!(
                validate_governance_entry(&prev, &new_only).err(),
                Some(Reject::InsufficientAuthorization)
            );

            // The old quorum authorizes it, and the new set takes effect.
            let old_quorum = next_entry(&prev, admin_set, old_refs[0], &old_refs[1..wo]);
            let committed = validate_and_apply_governance_entry(&prev, &old_quorum)
                .expect("old quorum authorizes admin.set");
            prop_assert_eq!(committed.state().administrators.administrators.clone(), new_admins);
            prop_assert_eq!(committed.state().administrators.threshold, u16::from(w_new));

            // After commit, the new quorum authorizes the next ordinary entry...
            let grant = GovernanceOperationPayload::MemberGrant(MemberGrant {
                member_id: principal(0xc9),
                role: Role::Member,
            });
            let by_new = next_entry(&committed, grant.clone(), new_refs[0], &new_refs[1..wn]);
            prop_assert!(validate_governance_entry(&committed, &by_new).is_ok());

            // ...while the old admins (disjoint from the new set) no longer can.
            let by_old = next_entry(&committed, grant, old_refs[0], &old_refs[1..wo]);
            prop_assert_eq!(
                validate_governance_entry(&committed, &by_old).err(),
                Some(Reject::InsufficientAuthorization)
            );
        }

        /// Device-binding validity (spec §4.4 / §8.4 / acceptance "property
        /// tests cover device-binding validity): over arbitrary device ids and
        /// both admin/non-admin owners, the grant→revoke lifecycle is enforced
        /// through the full predicate — unique ownership, active-status, and
        /// wrong-owner rules all reject as rule-3 `InvalidContent`.
        #[test]
        fn prop_device_binding_lifecycle_through_pipeline(
            dev_seed in 0u8..=255,
            owner_is_admin in any::<bool>(),
        ) {
            let admin_keys = [key(0x10)];
            let prev = genesis_state_with(&admin_keys, 1);
            let admin0 = &admin_keys[0];

            // Grant a distinct active plain member so both an admin owner and a
            // non-admin owner are available.
            let plain = principal(0xc0);
            let base = validate_and_apply_governance_entry(
                &prev,
                &next_entry(
                    &prev,
                    GovernanceOperationPayload::MemberGrant(MemberGrant {
                        member_id: plain,
                        role: Role::Member,
                    }),
                    admin0,
                    &[],
                ),
            )
            .expect("grant plain member");

            let owner = if owner_is_admin { admin0.member_id() } else { plain };
            let wrong_owner = if owner_is_admin { plain } else { admin0.member_id() };
            let absent = principal(0xee);
            let dev = DeviceId::from_bytes([dev_seed; N]);
            let base_root = *base.committed_state_root();

            // 1. grant(owner, dev) is authorized; the predecessor is untouched.
            let granted = validate_and_apply_governance_entry(
                &base,
                &next_entry(
                    &base,
                    GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                        member_id: owner,
                        device_id: dev,
                    }),
                    admin0,
                    &[],
                ),
            )
            .expect("device grant authorized");
            prop_assert_eq!(base.committed_state_root(), &base_root);
            prop_assert_eq!(
                granted
                    .state()
                    .members
                    .get(&owner)
                    .unwrap()
                    .devices
                    .get(&dev)
                    .unwrap()
                    .status,
                DeviceStatus::Active
            );

            // 2. Granting to an absent member is rejected (rule 3).
            let e_absent = next_entry(
                &base,
                GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                    member_id: absent,
                    device_id: dev,
                }),
                admin0,
                &[],
            );
            prop_assert_eq!(
                validate_governance_entry(&base, &e_absent).err(),
                Some(Reject::InvalidContent)
            );

            // 3. Re-granting the same device to the same owner is rejected.
            let e_regrant = next_entry(
                &granted,
                GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                    member_id: owner,
                    device_id: dev,
                }),
                admin0,
                &[],
            );
            prop_assert_eq!(
                validate_governance_entry(&granted, &e_regrant).err(),
                Some(Reject::InvalidContent)
            );

            // 4. Granting the same device to a different owner is rejected
            //    (device ids are globally unique across members).
            let e_cross = next_entry(
                &granted,
                GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                    member_id: wrong_owner,
                    device_id: dev,
                }),
                admin0,
                &[],
            );
            prop_assert_eq!(
                validate_governance_entry(&granted, &e_cross).err(),
                Some(Reject::InvalidContent)
            );

            // 5a. Revoking by a non-owner is rejected.
            let e_revoke_wrong = next_entry(
                &granted,
                GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
                    member_id: wrong_owner,
                    device_id: dev,
                }),
                admin0,
                &[],
            );
            prop_assert_eq!(
                validate_governance_entry(&granted, &e_revoke_wrong).err(),
                Some(Reject::InvalidContent)
            );

            // 5b. Revoking by the owner is authorized.
            let revoked = validate_and_apply_governance_entry(
                &granted,
                &next_entry(
                    &granted,
                    GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
                        member_id: owner,
                        device_id: dev,
                    }),
                    admin0,
                    &[],
                ),
            )
            .expect("owner revoke authorized");
            prop_assert_eq!(
                revoked
                    .state()
                    .members
                    .get(&owner)
                    .unwrap()
                    .devices
                    .get(&dev)
                    .unwrap()
                    .status,
                DeviceStatus::Revoked
            );

            // 6. Revoking an already-revoked device is rejected.
            let e_revoke2 = next_entry(
                &revoked,
                GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
                    member_id: owner,
                    device_id: dev,
                }),
                admin0,
                &[],
            );
            prop_assert_eq!(
                validate_governance_entry(&revoked, &e_revoke2).err(),
                Some(Reject::InvalidContent)
            );
        }
    }
}
