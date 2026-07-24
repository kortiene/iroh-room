//! Pure fork detection over authenticated, authorization-valid governance
//! candidates (spec §7 / §4.3, issue #149).
//!
//! A **fork** is two (or more) independently authorization-valid entries that
//! occupy the same governance sequence on divergent branches within one
//! community (spec §4.3). Detection is a side-effect-free predicate over
//! [`ValidatedGovernanceCandidate`] proofs; it never picks a winner. Branch
//! records are sorted by raw head id only for canonical representation —
//! sorted position has no authorization meaning (spec §5.6: no lexical
//! tie-break).
//!
//! This module is pure: no wall-clock, network, store, async, logging, or
//! randomness. It does not call the older candidate `governance::fork::detect`
//! implementation (spec §2.1).

use crate::error::Reject;
use crate::ids::{CommunityId, GovernanceId, StateRoot};

use super::authz::{GovernanceTip, ValidatedGovernanceCandidate};
use super::records::AuthenticatedGovernanceEvidence;

// ----------------------------------------------------------------------------
// Evidence types (spec §6.3). Trusted construction only: these are built from
// already-validated candidates, so unauthenticated bytes can never reach fork
// evidence.
// ----------------------------------------------------------------------------

/// Authenticated evidence for one branch head retained for fork audit (spec
/// §6.3 / §5.7). Carries the exact-CSB-derived head id, the chain position,
/// the committed state root, and the full authenticated entry/approval
/// evidence (exact CSBs + signatures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceBranchEvidence {
    /// The exact-CSB-derived entry id of this branch's head.
    pub head: GovernanceId,
    /// The head's sequence number.
    pub seq: u64,
    /// The head's predecessor id (`None` only at `seq == 1`).
    pub predecessor: Option<GovernanceId>,
    /// The validated state root this branch head commits to.
    pub state_root: StateRoot,
    /// The full authenticated entry + approval evidence (exact CSBs +
    /// signatures), preserved verbatim for audit (spec §5.7 / §11.2).
    pub entry: AuthenticatedGovernanceEvidence,
}

/// Canonical fork evidence (spec §6.3 / §4.4). Proves that more than one
/// authorization-valid state exists for one governance position.
///
/// `branches` is sorted by raw head id only for canonical representation; it
/// contains at least two unique branches. `stable_tip` / `stable_state_root`
/// identify the last common, uncontested ancestor — the recovery authority
/// source (spec §4.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceForkEvidence {
    /// The community this fork belongs to.
    pub community_id: CommunityId,
    /// The last common uncontested ancestor's tip (recovery authority source).
    pub stable_tip: GovernanceTip,
    /// The last common ancestor's committed state root.
    pub stable_state_root: StateRoot,
    /// Every known competing branch head, sorted ascending by raw head id
    /// (canonical representation only; never a winner selection — spec §5.6).
    pub branches: Vec<GovernanceBranchEvidence>,
}

impl GovernanceForkEvidence {
    /// The known branch head ids, in canonical (ascending) order.
    #[must_use]
    pub fn head_ids(&self) -> Vec<GovernanceId> {
        self.branches.iter().map(|b| b.head).collect()
    }

    /// The number of known competing branches (always `>= 2`).
    #[must_use]
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }
}

/// Build a branch evidence record from a validated candidate.
fn branch_from_candidate(candidate: &ValidatedGovernanceCandidate) -> GovernanceBranchEvidence {
    GovernanceBranchEvidence {
        head: candidate.entry_id(),
        seq: candidate.seq(),
        predecessor: candidate.prev(),
        state_root: candidate.resulting_state_root(),
        entry: candidate.evidence().clone(),
    }
}

/// Resolve the last common ancestor of two candidates from their carried
/// predecessor snapshots (spec §7 step 6).
///
/// For the direct same-predecessor case (`A.prev == B.prev`) the shared
/// predecessor snapshot is the common ancestor. When the two predecessors
/// diverge at a deeper position, the pure pair predicate cannot prove the
/// ancestry from the candidates alone — the fork-aware state machine, which
/// retains full lineage, resolves it. This helper returns `None` in that case
/// so the caller (machine) falls back to its lineage map rather than guessing
/// an authority set (spec §4.7: never guess).
fn common_ancestor_from_candidates(
    left: &ValidatedGovernanceCandidate,
    right: &ValidatedGovernanceCandidate,
) -> Option<(GovernanceTip, StateRoot)> {
    let lp = left.predecessor();
    let rp = right.predecessor();
    if lp.tip() == rp.tip() {
        // Same immediate predecessor (or both genesis) → it is the common
        // ancestor.
        Some((lp.tip(), *lp.committed_state_root()))
    } else {
        // Different immediate predecessors: the pair predicate carries only
        // one level of ancestry, so it cannot prove a deeper common ancestor.
        // Decline; the state machine resolves this from its retained lineage.
        None
    }
}

/// Pure fork predicate over two validated candidates (spec §7).
///
/// Returns `Some(GovernanceForkEvidence)` iff `left` and `right` are distinct
/// authorization-valid entries at the same sequence in the same community
/// whose common ancestor is resolvable from their carried predecessor
/// snapshots (the direct same-predecessor case). Returns `None` otherwise —
/// including when the two predecessors diverge at a depth the pair predicate
/// cannot prove from the candidates alone; the fork-aware state machine
/// resolves that case from its retained lineage.
///
/// The result is independent of argument order: swapping `left` and `right`
/// yields byte-identical evidence (spec §5.1 item 11).
#[must_use]
pub fn detect_governance_fork(
    left: &ValidatedGovernanceCandidate,
    right: &ValidatedGovernanceCandidate,
) -> Option<GovernanceForkEvidence> {
    // §7 step 1: communities must match.
    if left.community_id() != right.community_id() {
        return None;
    }
    // §7 step 3: identical ids ⇒ same entry (duplicate), not a fork.
    if left.entry_id() == right.entry_id() {
        return None;
    }
    // §7 step 5: same sequence + distinct ids ⇒ conflict. Different sequences
    // are handled by the set/machine form through lineage comparison.
    if left.seq() != right.seq() {
        return None;
    }
    // §7 step 6: resolve the common ancestor. If it cannot be proven from the
    // carried snapshots, decline (the machine resolves it from lineage).
    let (stable_tip, stable_state_root) = common_ancestor_from_candidates(left, right)?;
    // §7 step 7: canonically sort branch records for representation only.
    let mut branches = vec![branch_from_candidate(left), branch_from_candidate(right)];
    branches.sort_by_key(|b| *b.head.as_bytes());
    Some(GovernanceForkEvidence {
        community_id: left.community_id(),
        stable_tip,
        stable_state_root,
        branches,
    })
}

/// Set-form fork detection: group candidates by community, deduplicate identical
/// ids, and detect a fork if more than one distinct valid id occupies the same
/// sequence with a resolvable common ancestor (spec §7 / §5.1 item 14).
///
/// Coalesces a third or later competing branch into the evidence rather than
/// replacing prior evidence. Produces byte-identical canonical evidence for
/// every permutation of the same inputs (spec §5.1 item 11).
///
/// # Errors
/// Returns [`Reject::MissingDependency`] when ancestry required to prove a
/// detected conflict cannot be established from the supplied candidates.
#[allow(clippy::type_complexity)]
pub fn detect_governance_forks<'a>(
    candidates: impl IntoIterator<Item = &'a ValidatedGovernanceCandidate>,
) -> Result<Option<GovernanceForkEvidence>, Reject> {
    // Collect, deduplicate by exact id, and group by community.
    let mut by_id: std::collections::BTreeMap<GovernanceId, &ValidatedGovernanceCandidate> =
        std::collections::BTreeMap::new();
    for candidate in candidates {
        by_id.insert(candidate.entry_id(), candidate);
    }
    if by_id.len() < 2 {
        return Ok(None);
    }
    // Group by community.
    let mut by_community: std::collections::BTreeMap<
        CommunityId,
        Vec<&ValidatedGovernanceCandidate>,
    > = std::collections::BTreeMap::new();
    for candidate in by_id.values() {
        by_community
            .entry(candidate.community_id())
            .or_default()
            .push(candidate);
    }
    for (_, group) in by_community {
        if group.len() < 2 {
            continue;
        }
        // Find the maximal set of candidates at the same sequence.
        let mut by_seq: std::collections::BTreeMap<u64, Vec<&ValidatedGovernanceCandidate>> =
            std::collections::BTreeMap::new();
        for candidate in &group {
            by_seq.entry(candidate.seq()).or_default().push(*candidate);
        }
        for (_, seq_group) in by_seq {
            if seq_group.len() < 2 {
                continue;
            }
            // Build branch evidence for every candidate at this seq.
            let mut branches: Vec<GovernanceBranchEvidence> =
                seq_group.iter().map(|c| branch_from_candidate(c)).collect();
            branches.sort_by_key(|b| *b.head.as_bytes());
            // Resolve the common ancestor across all of them via the first
            // pair that yields one; if none can, the predicate cannot prove
            // ancestry from the supplied set.
            let mut common: Option<(GovernanceTip, StateRoot)> = None;
            for i in 0..seq_group.len() {
                for j in (i + 1)..seq_group.len() {
                    if let Some(c) = common_ancestor_from_candidates(seq_group[i], seq_group[j]) {
                        common = Some(c);
                        break;
                    }
                }
                if common.is_some() {
                    break;
                }
            }
            let (stable_tip, stable_state_root) = common.ok_or(Reject::MissingDependency)?;
            return Ok(Some(GovernanceForkEvidence {
                community_id: group[0].community_id(),
                stable_tip,
                stable_state_root,
                branches,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::log::authz::{
        validate_governance_candidate, validated_genesis_state, ValidatedGovernanceState,
    };
    use crate::governance::log::genesis::{sign_genesis, GenesisConfig, GENESIS_SCHEMA_VERSION};
    use crate::governance::log::model::Role;
    use crate::governance::log::model::{CommunityPolicy, RecoveryConfig};
    use crate::governance::log::operation::{GovernanceOperationPayload, MemberGrant};
    use crate::governance::log::records::{
        entry_id, GovernanceApproval, GovernanceApprovalBody, GovernanceEntry, GovernanceEntryBody,
        VerifiedGovernanceEntry,
    };
    use crate::ids::{CommunityId, GovernanceId, PrincipalId, StateRoot, LEN as N};
    use crate::keys::SigningKey;

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
            sign_genesis(&cfg, &key(0xa0)),
            sign_genesis(&cfg, &key(0xa1)),
        ];
        validated_genesis_state(&cfg, &sigs).expect("genesis threshold met")
    }

    /// Build a verified `member.grant` entry extending `prev`, signed by
    /// `signer` and approved by `approvers` (exactly the W=2 old-admin quorum).
    fn verified_grant(
        prev: &ValidatedGovernanceState,
        member: PrincipalId,
        signer: &SigningKey,
        approvers: &[&SigningKey],
    ) -> VerifiedGovernanceEntry {
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: member,
            role: Role::Member,
        });
        let (seq, prev_id) = match prev.tip() {
            GovernanceTip::Genesis => (1u64, None),
            GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
        };
        let declared = crate::governance::log::state::compute_state_root(
            &crate::governance::log::state::apply(prev.state(), &payload).unwrap(),
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
        let approvals: Vec<GovernanceApproval> = approvers
            .iter()
            .map(|a| {
                GovernanceApproval::new(
                    GovernanceApprovalBody {
                        community_id: body.community_id,
                        entry_id: entry_id(&body),
                        state_root: body.state_root,
                        approver: a.member_id(),
                        created_at_ms: body.created_at_ms + 1,
                    },
                    a,
                )
            })
            .collect();
        let entry = GovernanceEntry::new(body, signer, approvals);
        crate::governance::log::records::verify_governance_entry(&entry).expect("verifies")
    }

    fn candidate(
        prev: &ValidatedGovernanceState,
        member: PrincipalId,
        signer: &SigningKey,
        approvers: &[&SigningKey],
    ) -> ValidatedGovernanceCandidate {
        let entry = verified_grant(prev, member, signer, approvers);
        validate_governance_candidate(prev, &entry).expect("valid candidate")
    }

    #[test]
    fn same_predecessor_distinct_ids_at_same_seq_is_a_fork() {
        let genesis = genesis_state();
        // Two distinct valid grants at seq 1, both extending genesis.
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        let b = candidate(&genesis, principal(0xc1), &key(0xa1), &[&key(0xa2)]);
        let evidence = detect_governance_fork(&a, &b).expect("fork detected");
        assert_eq!(evidence.community_id, genesis.state().community_id);
        assert_eq!(evidence.branch_count(), 2);
        assert_eq!(evidence.stable_tip, GovernanceTip::Genesis);
        // Branch order is canonical (ascending by raw head id), independent of
        // argument order.
        let reversed = detect_governance_fork(&b, &a).expect("fork detected (reversed)");
        assert_eq!(reversed.branches, evidence.branches);
        assert_eq!(reversed.head_ids(), evidence.head_ids());
    }

    #[test]
    fn duplicate_entry_id_is_not_a_fork() {
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        // The same candidate is a duplicate observation, not a fork.
        assert!(detect_governance_fork(&a, &a).is_none());
    }

    #[test]
    fn different_communities_do_not_fork() {
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        // Tamper only the community id of b's predecessor wrapper so the
        // candidate reports a foreign community. We construct b against the
        // same genesis then mutate its carried evidence's community via a
        // separate validation: simpler to validate against a foreign-state
        // wrapper is not possible through the public API, so instead assert
        // the predicate's community guard directly with mismatched candidates
        // built from two different genesis communities.
        let mut foreign_cfg = genesis_config();
        foreign_cfg.genesis_nonce = [0xcd; N];
        let foreign = validated_genesis_state(
            &foreign_cfg,
            &[
                sign_genesis(&foreign_cfg, &key(0xa0)),
                sign_genesis(&foreign_cfg, &key(0xa1)),
            ],
        )
        .expect("foreign genesis");
        assert_ne!(a.community_id(), foreign.state().community_id);
        let b = candidate(&foreign, principal(0xc1), &key(0xa0), &[&key(0xa1)]);
        assert!(detect_governance_fork(&a, &b).is_none());
    }

    #[test]
    fn distinct_valid_entries_with_equal_resulting_roots_still_fork() {
        // Two grants of the SAME member produce equal resulting roots, yet they
        // are distinct authorized decisions and must still fork (spec §5.1
        // item 10). The bodies differ only in `created_at_ms` (advisory, never
        // part of the state root) so the resulting roots are equal while the
        // exact-CSB-derived ids differ.
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        // Build b with a distinct body (different created_at_ms) but the same
        // grant payload → same resulting root, distinct entry id.
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(0xc0),
            role: Role::Member,
        });
        let declared = crate::governance::log::state::compute_state_root(
            &crate::governance::log::state::apply(genesis.state(), &payload).unwrap(),
        );
        let body = GovernanceEntryBody {
            community_id: genesis.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 3_333, // differs from a's 2_000
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let approvals = vec![GovernanceApproval::new(
            GovernanceApprovalBody {
                community_id: body.community_id,
                entry_id: entry_id(&body),
                state_root: body.state_root,
                approver: key(0xa2).member_id(),
                created_at_ms: body.created_at_ms + 1,
            },
            &key(0xa2),
        )];
        let entry = GovernanceEntry::new(body, &key(0xa1), approvals);
        let b_entry =
            crate::governance::log::records::verify_governance_entry(&entry).expect("verifies");
        let b = validate_governance_candidate(&genesis, &b_entry).expect("valid candidate");
        assert_eq!(
            a.resulting_state_root(),
            b.resulting_state_root(),
            "test setup: both grants yield the same root"
        );
        assert_ne!(a.entry_id(), b.entry_id(), "test setup: distinct entry ids");
        let evidence = detect_governance_fork(&a, &b).expect("equal-root fork detected");
        assert_eq!(evidence.branch_count(), 2);
    }

    #[test]
    fn evidence_preserves_both_branch_signatures_and_csbs() {
        // Acceptance #5: both competing approvals/entries are preserved in the
        // audit evidence record (spec §5.7 / §11.2).
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        let b = candidate(&genesis, principal(0xc1), &key(0xa1), &[&key(0xa2)]);
        let evidence = detect_governance_fork(&a, &b).expect("fork detected");
        // Each branch carries the full authenticated evidence: exact entry CSB,
        // entry signature, and every verified approval's CSB + signature.
        assert_eq!(evidence.branches.len(), 2);
        for branch in &evidence.branches {
            assert!(!branch.entry.csb().is_empty());
            // The entry signature bytes are retained.
            assert_eq!(branch.entry.signature().as_bytes().len(), 64);
            // Each branch carries its approval evidence with exact CSB + sig.
            for approval in branch.entry.approvals() {
                assert!(!approval.csb().is_empty());
                assert_eq!(approval.signature().as_bytes().len(), 64);
            }
        }
        // The two branches carry distinct exact entry CSBs.
        assert_ne!(
            evidence.branches[0].entry.csb(),
            evidence.branches[1].entry.csb()
        );
    }

    #[test]
    fn set_form_permutation_independence_and_third_branch() {
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        let b = candidate(&genesis, principal(0xc1), &key(0xa1), &[&key(0xa2)]);
        let fwd = detect_governance_forks(&[a.clone(), b.clone()])
            .unwrap()
            .expect("fork");
        let rev = detect_governance_forks(&[b.clone(), a.clone()])
            .unwrap()
            .expect("fork");
        assert_eq!(fwd, rev);

        // A third distinct valid branch expands (never replaces) the evidence.
        let c = candidate(&genesis, principal(0xc2), &key(0xa2), &[&key(0xa0)]);
        let triple = detect_governance_forks(&[a.clone(), b.clone(), c.clone()])
            .unwrap()
            .expect("three-way fork");
        assert_eq!(triple.branch_count(), 3);
        // The first two branches are still present.
        assert!(triple.head_ids().contains(&a.entry_id()));
        assert!(triple.head_ids().contains(&b.entry_id()));
        assert!(triple.head_ids().contains(&c.entry_id()));
    }

    #[test]
    fn set_form_dedupes_identical_ids() {
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        // Same id twice (a duplicate observation) is deduped, not a fork.
        assert!(detect_governance_forks(&[a.clone(), a]).unwrap().is_none());
    }

    #[test]
    fn no_fork_when_only_one_candidate_is_valid() {
        // The predicate only ever receives validated candidates (construction
        // is opaque). A single candidate can never fork on its own.
        let genesis = genesis_state();
        let a = candidate(&genesis, principal(0xc0), &key(0xa0), &[&key(0xa1)]);
        assert!(detect_governance_forks(std::slice::from_ref(&a))
            .unwrap()
            .is_none());
        // Suppress unused warning for StateRoot/CommunityId/GovernanceId in
        // this test module's import set.
        let _ = CommunityId::from_bytes([0; N]);
        let _ = GovernanceId::from_bytes([0; N]);
        let _ = StateRoot::from_bytes([0; N]);
    }
}
