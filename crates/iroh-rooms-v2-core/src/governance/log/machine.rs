//! The fork-aware governance state machine (spec §8 / §9, issue #149).
//!
//! Layers fork detection and recovery on top of the #147/#148 pure governance
//! log. The machine consumes already-crypto-verified [`VerifiedGovernanceEntry`]
//! records and produces deterministic state transitions:
//!
//! - **Linear**: a single accepted tip. An ordinary entry that extends the tip
//!   advances linearly; a duplicate id is a no-op `Duplicate`; a second
//!   authorization-valid entry at the same sequence on a divergent branch
//!   atomically enters `GovernanceForked`.
//! - **`GovernanceForked`**: more than one authorization-valid branch head is
//!   retained. Every ordinary operation fails closed with
//!   [`Reject::UnresolvedFork`] before admin quorum or operation application is
//!   consulted (spec §5.3). Only a recovery-threshold-authorized `fork.resolve`
//!   can leave this state.
//!
//! The machine is pure and deterministic: decisions depend only on
//! authenticated input records and retained validated state, never arrival time,
//! wall clock, randomness, or map iteration order (spec §5.2 item 12). A failed
//! transition leaves the entire previous machine state unchanged (spec §5.2
//! item 5). No lexical/timestamp/arrival-order winner is ever selected (spec
//! §5.6); branch ids are sorted only for canonical representation.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Reject;
use crate::ids::{CommunityId, GovernanceId, PrincipalId, StateRoot};

use super::authz::{
    validate_governance_candidate, GovernanceTip, ValidatedGovernanceCandidate,
    ValidatedGovernanceState,
};
use super::fork::{GovernanceBranchEvidence, GovernanceForkEvidence};
use super::model::{RecoveryConfig, ResolvedForkMarker};
use super::operation::{GovernanceOperationKind, GovernanceOperationPayload};
use super::records::{AuthenticatedGovernanceEvidence, VerifiedGovernanceEntry};
use super::state::compute_state_root;

// ----------------------------------------------------------------------------
// Lineage (spec §6.4). Pure in-memory protocol state used to resolve a common
// ancestor when merging prevalidated branches. A runtime may rebuild it from
// authenticated records.
// ----------------------------------------------------------------------------

/// A validated lineage node: the snapshot + authenticated evidence at one
/// accepted entry id.
#[derive(Clone, Debug)]
struct LineageNode {
    seq: u64,
    prev: Option<GovernanceId>,
    state: ValidatedGovernanceState,
    evidence: AuthenticatedGovernanceEvidence,
}

/// Retained validated ancestry sufficient to identify a common ancestor when
/// merging prevalidated branches (spec §6.4 / §4.3).
#[derive(Clone, Debug)]
pub struct GovernanceLineage {
    genesis: ValidatedGovernanceState,
    nodes: BTreeMap<GovernanceId, LineageNode>,
}

impl GovernanceLineage {
    fn new(genesis: ValidatedGovernanceState) -> Self {
        Self {
            genesis,
            nodes: BTreeMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        id: GovernanceId,
        seq: u64,
        prev: Option<GovernanceId>,
        state: ValidatedGovernanceState,
        evidence: AuthenticatedGovernanceEvidence,
    ) {
        self.nodes.insert(
            id,
            LineageNode {
                seq,
                prev,
                state,
                evidence,
            },
        );
    }

    fn get(&self, id: &GovernanceId) -> Option<&LineageNode> {
        self.nodes.get(id)
    }

    /// The ancestor chain of `tip`, inclusive, walking back to genesis:
    /// `[tip, prev(tip), ..., <genesis cursor>]`. The genesis cursor appears as
    /// `(None, &genesis)` at the tail.
    fn ancestor_chain(&self, tip: GovernanceId) -> Vec<(Option<GovernanceId>, &LineageNode)> {
        let mut chain = Vec::new();
        let mut cur = Some(tip);
        while let Some(id) = cur {
            match self.nodes.get(&id) {
                Some(node) => {
                    cur = node.prev;
                    chain.push((Some(id), node));
                }
                None => break,
            }
        }
        chain
    }

    /// Resolve the most recent common ancestor of two tips from the retained
    /// lineage (spec §7 step 6). Returns its tip + committed state root. Falls
    /// back to the genesis cursor when no deeper shared node exists.
    fn common_ancestor(
        &self,
        tip_a: GovernanceId,
        tip_b: GovernanceId,
    ) -> (GovernanceTip, StateRoot) {
        // Collect a's ancestor ids (inclusive of tip_a).
        let chain_a = self.ancestor_chain(tip_a);
        let ids_a: BTreeSet<Option<GovernanceId>> = chain_a.iter().map(|(id, _)| *id).collect();
        // Walk b from the tip backward; the first ancestor of b that is also an
        // ancestor of a is the most recent common ancestor.
        for (id, node) in self.ancestor_chain(tip_b) {
            if ids_a.contains(&id) {
                return match id {
                    Some(_) => (node.state.tip(), *node.state.committed_state_root()),
                    None => (self.genesis.tip(), *self.genesis.committed_state_root()),
                };
            }
        }
        // Both chains include the genesis cursor tail, so this is unreachable
        // in practice; fall back to genesis defensively.
        (self.genesis.tip(), *self.genesis.committed_state_root())
    }
}

// ----------------------------------------------------------------------------
// Audit record (spec §11).
// ----------------------------------------------------------------------------

/// The status of a fork incident in the audit record (spec §11.1).
///
/// `Resolved` carries the full resolution evidence; `Unresolved` carries none.
/// The variant size difference is acceptable for this pure-core audit record
/// (the bulk is heap-allocated and these records are not on a hot path).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum GovernanceForkAuditStatus {
    /// The fork is unresolved; ordinary governance fails closed.
    Unresolved,
    /// The fork was resolved by a recovery-authorized `fork.resolve`. Losing
    /// branch evidence and approvals remain preserved (spec §5.2 item 9 /
    /// §11.2).
    Resolved {
        /// The authenticated resolution entry evidence (exact CSB + signatures).
        resolution: AuthenticatedGovernanceEvidence,
        /// The selected branch head.
        selected_head: GovernanceId,
        /// The validated state root for the selected head.
        selected_state_root: StateRoot,
        /// The distinct eligible recovery signers that authorized the resolution.
        eligible_recovery_signers: Vec<PrincipalId>,
        /// The new linear tip id (the resolution entry id).
        resulting_tip: GovernanceId,
        /// The post-resolution state root.
        resulting_state_root: StateRoot,
    },
}

/// An immutable fork incident audit record (spec §11.1). Preserves every
/// competing branch's authenticated evidence (exact entry/approval CSBs +
/// signatures) before and after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceForkAuditRecord {
    /// The community this fork belongs to.
    pub community_id: CommunityId,
    /// The last common uncontested ancestor's tip.
    pub stable_tip: GovernanceTip,
    /// The last common ancestor's committed state root.
    pub stable_state_root: StateRoot,
    /// Every known competing branch head's authenticated evidence.
    pub branches: Vec<GovernanceBranchEvidence>,
    /// The incident status (unresolved or resolved).
    pub status: GovernanceForkAuditStatus,
}

// ----------------------------------------------------------------------------
// Machine state (spec §6.4).
// ----------------------------------------------------------------------------

/// The accepted linear state (spec §6.4).
#[derive(Debug, Clone)]
pub struct LinearGovernanceState {
    accepted: ValidatedGovernanceState,
    lineage: GovernanceLineage,
    audit: Vec<GovernanceForkAuditRecord>,
}

impl LinearGovernanceState {
    /// The accepted snapshot.
    #[must_use]
    pub fn accepted(&self) -> &ValidatedGovernanceState {
        &self.accepted
    }

    /// The retained lineage.
    #[must_use]
    pub fn lineage(&self) -> &GovernanceLineage {
        &self.lineage
    }

    /// Prior fork incident audit records.
    #[must_use]
    pub fn audit(&self) -> &[GovernanceForkAuditRecord] {
        &self.audit
    }
}

/// The unresolved fork state (spec §6.4 / §4.4). Retains the last common
/// ancestor (recovery authority source), every validated branch head, the
/// canonical fork evidence, prior audit incidents, and the full lineage.
#[derive(Debug, Clone)]
pub struct GovernanceForkedState {
    pub(super) stable: ValidatedGovernanceState,
    pub(super) branches: BTreeMap<GovernanceId, ValidatedGovernanceState>,
    pub(super) evidence: GovernanceForkEvidence,
    pub(super) prior_audit: Vec<GovernanceForkAuditRecord>,
    pub(super) lineage: GovernanceLineage,
}

impl GovernanceForkedState {
    /// The last common uncontested ancestor snapshot (recovery authority source).
    #[must_use]
    pub fn stable(&self) -> &ValidatedGovernanceState {
        &self.stable
    }

    /// The known branch head ids (canonical ascending order).
    #[must_use]
    pub fn head_ids(&self) -> Vec<GovernanceId> {
        self.branches.keys().copied().collect()
    }

    /// The validated tip state for a branch head, if known.
    #[must_use]
    pub fn branch(&self, head: &GovernanceId) -> Option<&ValidatedGovernanceState> {
        self.branches.get(head)
    }

    /// The canonical fork evidence.
    #[must_use]
    pub fn evidence(&self) -> &GovernanceForkEvidence {
        &self.evidence
    }

    /// Prior audit incidents.
    #[must_use]
    pub fn prior_audit(&self) -> &[GovernanceForkAuditRecord] {
        &self.prior_audit
    }
}

/// The fork-aware machine state (spec §6.4).
///
/// The two variants differ in size (the forked variant carries retained branch
/// evidence + lineage). The machine is held by value and not on a hot path;
/// both variants' bulk data is heap-allocated behind `Vec`/`BTreeMap` handles,
/// so the stack-size difference is bounded and acceptable for this pure core.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum GovernanceMachineState {
    /// A single accepted tip; ordinary governance proceeds linearly.
    Linear(LinearGovernanceState),
    /// More than one authorization-valid branch head; ordinary governance
    /// fails closed pending a recovery-authorized `fork.resolve`.
    GovernanceForked(GovernanceForkedState),
}

/// A lightweight observation outcome (spec §6.5). The machine holds the
/// authoritative updated state; this descriptor reports what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceObservation {
    /// An ordinary entry extended the linear tip (or resolved a fork into a new
    /// linear tip).
    Advanced {
        /// The new accepted tip.
        tip: GovernanceTip,
        /// The new committed state root.
        state_root: StateRoot,
    },
    /// A second authorization-valid branch was observed; the machine is now
    /// `GovernanceForked`. Carries the typed fork evidence (spec §4.4).
    ForkDetected {
        /// The canonical fork evidence (both competing records + approvals).
        evidence: GovernanceForkEvidence,
    },
    /// An already-known exact entry id was re-observed (no state change).
    Duplicate {
        /// The known entry id.
        id: GovernanceId,
    },
}

// ----------------------------------------------------------------------------
// GovernanceMachine
// ----------------------------------------------------------------------------

/// The fork-aware governance state machine (spec §8 / §9).
///
/// Construct it from a validated genesis snapshot via [`Self::from_genesis`],
/// then feed crypto-verified entries through [`Self::observe`]. The machine is
/// deterministic and pure; a failed observation leaves its state byte-for-byte
/// unchanged.
#[derive(Debug, Clone)]
pub struct GovernanceMachine {
    state: GovernanceMachineState,
}

impl GovernanceMachine {
    /// Build a machine whose initial linear state is the validated genesis
    /// snapshot.
    #[must_use]
    pub fn from_genesis(genesis: ValidatedGovernanceState) -> Self {
        let lineage = GovernanceLineage::new(genesis.clone());
        Self {
            state: GovernanceMachineState::Linear(LinearGovernanceState {
                accepted: genesis,
                lineage,
                audit: Vec::new(),
            }),
        }
    }

    /// The current machine state.
    #[must_use]
    pub fn state(&self) -> &GovernanceMachineState {
        &self.state
    }

    /// The current accepted snapshot, if linear.
    #[must_use]
    pub fn accepted(&self) -> Option<&ValidatedGovernanceState> {
        match &self.state {
            GovernanceMachineState::Linear(linear) => Some(linear.accepted()),
            GovernanceMachineState::GovernanceForked(_) => None,
        }
    }

    /// The current forked state, if forked.
    #[must_use]
    pub fn forked(&self) -> Option<&GovernanceForkedState> {
        match &self.state {
            GovernanceMachineState::Linear(_) => None,
            GovernanceMachineState::GovernanceForked(forked) => Some(forked),
        }
    }

    /// The audit record chain (resolved-prior incidents + the current
    /// unresolved incident, if any).
    #[must_use]
    pub fn audit(&self) -> Vec<GovernanceForkAuditRecord> {
        match &self.state {
            GovernanceMachineState::Linear(linear) => linear.audit.clone(),
            GovernanceMachineState::GovernanceForked(forked) => {
                let mut out = forked.prior_audit.clone();
                out.push(GovernanceForkAuditRecord {
                    community_id: forked.evidence.community_id,
                    stable_tip: forked.evidence.stable_tip,
                    stable_state_root: forked.evidence.stable_state_root,
                    branches: forked.evidence.branches.clone(),
                    status: GovernanceForkAuditStatus::Unresolved,
                });
                out
            }
        }
    }

    /// Observe a crypto-verified governance entry (spec §8).
    ///
    /// In `Linear`: validates the entry against its declared predecessor,
    /// advances the tip, reports a `Duplicate`, or atomically enters
    /// `GovernanceForked` on a conflicting valid branch.
    ///
    /// In `GovernanceForked`: every ordinary operation fails closed with
    /// [`Reject::UnresolvedFork`] before admin quorum or application (spec
    /// §5.3). A `fork.resolve` entry is routed to the dedicated recovery
    /// validator (§9).
    ///
    /// # Errors
    /// - [`Reject::UnresolvedFork`] — ordinary operation while forked.
    /// - [`Reject::MissingDependency`] — declared predecessor not in lineage.
    /// - [`Reject::InvalidForkResolution`] — malformed/stale/inconsistent
    ///   `fork.resolve` semantics or resolution while not forked.
    /// - [`Reject::StateRootMismatch`] — selected or post-resolution root
    ///   mismatch.
    /// - [`Reject::InsufficientAuthorization`] — fewer than `W` eligible
    ///   recovery signers.
    /// - Any [`Reject`] from the #148 five-rule predicate for ordinary entries.
    pub fn observe(
        &mut self,
        entry: &VerifiedGovernanceEntry,
    ) -> Result<GovernanceObservation, Reject> {
        // Build the successor state WITHOUT mutating `self`; only assign on
        // success. A failed observation (early `?` return) leaves `self`
        // byte-for-byte unchanged (spec §5.2 item 5).
        let (new_state, observation) = match &self.state {
            GovernanceMachineState::Linear(linear) => Self::observe_linear(linear, entry)?,
            GovernanceMachineState::GovernanceForked(forked) => {
                Self::observe_forked(forked, entry)?
            }
        };
        self.state = new_state;
        Ok(observation)
    }

    fn observe_linear(
        linear: &LinearGovernanceState,
        entry: &VerifiedGovernanceEntry,
    ) -> Result<(GovernanceMachineState, GovernanceObservation), Reject> {
        let body = entry.body();

        // A fork.resolve while linear is unsolicited (spec §9 Rule 1).
        if body.kind == GovernanceOperationKind::ForkResolve {
            return Err(Reject::InvalidForkResolution);
        }

        // Duplicate exact id? (spec §5.1 item 6 / §8.1 step 6).
        if linear.lineage.nodes.contains_key(&entry.id()) {
            return Ok((
                Self::clone_linear_state(linear),
                GovernanceObservation::Duplicate { id: entry.id() },
            ));
        }

        // Locate the declared predecessor (spec §8.1 step 2-3).
        let predecessor = predecessor_for(linear, body.prev, body.seq)?;

        // Validate against the declared predecessor (rules 1-5).
        let candidate = validate_governance_candidate(&predecessor, entry)?;

        // Conflict detection: any retained entry at the same seq with a
        // distinct id (same community) reveals a divergent authorized branch.
        if let Some(conflict_id) =
            conflicting_entry_at_seq(linear, candidate.seq(), entry.id(), body.community_id)
        {
            return Self::enter_forked_from_linear(linear, &candidate, conflict_id);
        }

        // Otherwise: linear advance.
        let new_accepted = candidate.resulting().clone();
        let mut new_lineage = linear.lineage.clone();
        new_lineage.insert(
            entry.id(),
            body.seq,
            body.prev,
            new_accepted.clone(),
            candidate.evidence().clone(),
        );
        let observation = GovernanceObservation::Advanced {
            tip: new_accepted.tip(),
            state_root: *new_accepted.committed_state_root(),
        };
        Ok((
            GovernanceMachineState::Linear(LinearGovernanceState {
                accepted: new_accepted,
                lineage: new_lineage,
                audit: linear.audit.clone(),
            }),
            observation,
        ))
    }

    fn observe_forked(
        forked: &GovernanceForkedState,
        entry: &VerifiedGovernanceEntry,
    ) -> Result<(GovernanceMachineState, GovernanceObservation), Reject> {
        let body = entry.body();
        // §5.3: every ordinary operation fails closed while forked. This gate
        // precedes ordinary admin quorum and operation application, so even a
        // malformed member.grant returns UnresolvedFork (spec §10).
        if body.kind != GovernanceOperationKind::ForkResolve {
            return Err(Reject::UnresolvedFork);
        }
        // §9: dedicated fork.resolve validation + commit.
        Self::validate_and_commit_resolution(forked, entry)
    }

    /// Atomically enter `GovernanceForked` from a linear state given the new
    /// candidate and the conflicting retained entry id (spec §8.1 step 7).
    fn enter_forked_from_linear(
        linear: &LinearGovernanceState,
        new_candidate: &ValidatedGovernanceCandidate,
        conflict_id: GovernanceId,
    ) -> Result<(GovernanceMachineState, GovernanceObservation), Reject> {
        let conflict_node = linear
            .lineage
            .get(&conflict_id)
            .ok_or(Reject::MissingDependency)?;
        let conflict_state = conflict_node.state.clone();
        let conflict_evidence = conflict_node.evidence.clone();

        // Resolve the common ancestor from the full lineage.
        let (stable_tip, stable_root) = linear
            .lineage
            .common_ancestor(new_candidate.entry_id(), conflict_id);

        let new_branch = GovernanceBranchEvidence {
            head: new_candidate.entry_id(),
            seq: new_candidate.seq(),
            predecessor: new_candidate.prev(),
            state_root: new_candidate.resulting_state_root(),
            entry: new_candidate.evidence().clone(),
        };
        let conflict_branch = GovernanceBranchEvidence {
            head: conflict_id,
            seq: conflict_node.seq,
            predecessor: conflict_node.prev,
            state_root: *conflict_state.committed_state_root(),
            entry: conflict_evidence,
        };

        let mut branches = vec![new_branch, conflict_branch];
        branches.sort_by_key(|b| *b.head.as_bytes());
        let evidence = GovernanceForkEvidence {
            community_id: new_candidate.community_id(),
            stable_tip,
            stable_state_root: stable_root,
            branches,
        };

        let mut branch_map: BTreeMap<GovernanceId, ValidatedGovernanceState> = BTreeMap::new();
        branch_map.insert(new_candidate.entry_id(), new_candidate.resulting().clone());
        branch_map.insert(conflict_id, conflict_state);

        let mut new_lineage = linear.lineage.clone();
        new_lineage.insert(
            new_candidate.entry_id(),
            new_candidate.seq(),
            new_candidate.prev(),
            new_candidate.resulting().clone(),
            new_candidate.evidence().clone(),
        );

        let stable_state = match stable_tip {
            GovernanceTip::Genesis => linear.lineage.genesis.clone(),
            GovernanceTip::Entry { id, .. } => new_lineage
                .get(&id)
                .map_or_else(|| linear.lineage.genesis.clone(), |n| n.state.clone()),
        };

        let new_state = GovernanceMachineState::GovernanceForked(GovernanceForkedState {
            stable: stable_state,
            branches: branch_map,
            evidence: evidence.clone(),
            prior_audit: linear.audit.clone(),
            lineage: new_lineage,
        });
        Ok((new_state, GovernanceObservation::ForkDetected { evidence }))
    }

    /// Clone a linear state (used to return an unchanged state for `Duplicate`).
    fn clone_linear_state(linear: &LinearGovernanceState) -> GovernanceMachineState {
        GovernanceMachineState::Linear(LinearGovernanceState {
            accepted: linear.accepted.clone(),
            lineage: linear.lineage.clone(),
            audit: linear.audit.clone(),
        })
    }

    /// Validate and commit a `fork.resolve` entry (spec §9, seven rules).
    /// Returns the new linear machine state + observation on success.
    #[allow(clippy::too_many_lines)]
    fn validate_and_commit_resolution(
        forked: &GovernanceForkedState,
        entry: &VerifiedGovernanceEntry,
    ) -> Result<(GovernanceMachineState, GovernanceObservation), Reject> {
        let body = entry.body();
        let GovernanceOperationPayload::ForkResolve(resolve) = &body.payload else {
            return Err(Reject::InvalidForkResolution);
        };
        // Rule 1: community must match the forked community.
        if body.community_id != forked.evidence.community_id {
            return Err(Reject::InvalidForkResolution);
        }

        // Rule 2: canonical complete branch set — exact set equality with all
        // current known heads. The payload decode already enforced canonical
        // (sorted, unique, >=2) branch_heads.
        let known_heads: BTreeSet<GovernanceId> = forked.branches.keys().copied().collect();
        let declared_heads: BTreeSet<GovernanceId> = resolve.branch_heads.iter().copied().collect();
        if declared_heads.len() != resolve.branch_heads.len() {
            return Err(Reject::InvalidForkResolution);
        }
        // Any locally-unknown listed head → MissingDependency (spec §9 Rule 2).
        for declared in &declared_heads {
            if !known_heads.contains(declared) {
                return Err(Reject::MissingDependency);
            }
        }
        // Omitted known head, extra unrelated head, or stale head → invalid.
        if declared_heads != known_heads {
            return Err(Reject::InvalidForkResolution);
        }

        // Rule 3: selected head binding — must be a known head with a matching
        // validated state root.
        let selected_state = forked
            .branches
            .get(&resolve.selected_head)
            .ok_or(Reject::InvalidForkResolution)?;
        if *selected_state.committed_state_root() != resolve.selected_state_root {
            return Err(Reject::StateRootMismatch);
        }
        let (selected_seq, selected_prev_id) = match selected_state.tip() {
            GovernanceTip::Entry { seq, id } => (seq, Some(id)),
            GovernanceTip::Genesis => (0u64, None),
        };

        // Rule 4: resolution chain link — seq == selected_head.seq + 1, prev ==
        // Some(selected_head), checked arithmetic.
        let expected_seq = selected_seq
            .checked_add(1)
            .ok_or(Reject::InvalidForkResolution)?;
        if body.seq != expected_seq || body.prev != selected_prev_id {
            return Err(Reject::InvalidForkResolution);
        }

        // Rule 5: recovery authority from the last common uncontested ancestor.
        let recovery_cfg = &forked.stable.state().recovery.config;
        let eligible = count_eligible_recovery_signers(recovery_cfg, entry)?;

        // Rule 6: deterministic post-resolution root. Apply ONLY the resolution
        // marker to the selected branch state, then require the declared root
        // to match.
        let marker = ResolvedForkMarker {
            branch_heads: resolve.branch_heads.clone(),
            selected_head: resolve.selected_head,
            selected_state_root: resolve.selected_state_root,
            created_at_ms: resolve.created_at_ms,
        };
        let mut resolved_state = selected_state.state().clone();
        resolved_state.policy.fork_markers.push(marker);
        resolved_state
            .policy
            .fork_markers
            .sort_by_key(|m| *m.selected_head.as_bytes());
        let recomputed = compute_state_root(&resolved_state);
        if recomputed != body.state_root {
            return Err(Reject::StateRootMismatch);
        }

        // Rule 7: atomic commit + audit. Build the complete audit record before
        // committing the new linear snapshot.
        let new_accepted = ValidatedGovernanceState::from_parts(
            resolved_state,
            GovernanceTip::Entry {
                seq: body.seq,
                id: entry.id(),
            },
            body.state_root,
        );

        // Build the resolved audit record (preserves all branch evidence +
        // losing approvals — spec §5.2 item 9 / §11.2).
        let mut audit = forked.prior_audit.clone();
        audit.push(GovernanceForkAuditRecord {
            community_id: forked.evidence.community_id,
            stable_tip: forked.evidence.stable_tip,
            stable_state_root: forked.evidence.stable_state_root,
            branches: forked.evidence.branches.clone(),
            status: GovernanceForkAuditStatus::Resolved {
                resolution: entry.authenticated_evidence().clone(),
                selected_head: resolve.selected_head,
                selected_state_root: resolve.selected_state_root,
                eligible_recovery_signers: eligible.iter().copied().collect(),
                resulting_tip: entry.id(),
                resulting_state_root: body.state_root,
            },
        });

        let mut new_lineage = forked.lineage.clone();
        new_lineage.insert(
            entry.id(),
            body.seq,
            body.prev,
            new_accepted.clone(),
            entry.authenticated_evidence().clone(),
        );

        let new_state = GovernanceMachineState::Linear(LinearGovernanceState {
            accepted: new_accepted.clone(),
            lineage: new_lineage,
            audit,
        });
        let observation = GovernanceObservation::Advanced {
            tip: new_accepted.tip(),
            state_root: *new_accepted.committed_state_root(),
        };
        Ok((new_state, observation))
    }
}

// ----------------------------------------------------------------------------
// Recovery-threshold authorization (spec §5.4).
// ----------------------------------------------------------------------------

/// Recovery-config invariants that must hold before threshold counting can
/// authorize a resolution (spec §5.4 items 1-2): non-empty, sorted unique, and
/// `1 <= threshold <= len(R)`. A malformed/disabled config fails closed.
fn recovery_authorization_invariants_hold(config: &RecoveryConfig) -> bool {
    if config.recovery_keys.is_empty() || config.threshold == 0 {
        return false;
    }
    let mut sorted = config.recovery_keys.clone();
    sorted.sort();
    sorted.dedup();
    if sorted != config.recovery_keys {
        return false;
    }
    match u16::try_from(config.recovery_keys.len()) {
        Ok(count) => config.threshold <= count,
        Err(_) => false,
    }
}

/// Count the distinct union of the verified entry signer and verified approval
/// signers intersected with the recovery-key set `R` (spec §5.4 items 3-8).
///
/// The entry signer counts iff in `R`; a signer who also approves counts once;
/// outsider/administrator-only signatures contribute zero; recovery keys
/// installed only on a contested branch are excluded (the caller passes the
/// last common ancestor's config).
///
/// Returns the eligible signer set on success.
///
/// # Errors
/// Returns [`Reject::InsufficientAuthorization`] for a malformed/disabled
/// config or fewer than `W` eligible signers (no administrator fallback — spec
/// §5.4 item 14).
fn count_eligible_recovery_signers(
    config: &RecoveryConfig,
    entry: &VerifiedGovernanceEntry,
) -> Result<BTreeSet<PrincipalId>, Reject> {
    if !recovery_authorization_invariants_hold(config) {
        return Err(Reject::InsufficientAuthorization);
    }
    let recovery_set: BTreeSet<PrincipalId> = config.recovery_keys.iter().copied().collect();
    let mut signers: BTreeSet<PrincipalId> = BTreeSet::new();
    signers.insert(entry.signer());
    for approval in entry.approvals() {
        signers.insert(approval.body().approver);
    }
    let eligible: BTreeSet<PrincipalId> = signers.intersection(&recovery_set).copied().collect();
    if eligible.len() >= usize::from(config.threshold) {
        Ok(eligible)
    } else {
        Err(Reject::InsufficientAuthorization)
    }
}

// ----------------------------------------------------------------------------
// Free helpers (pure; operate on the linear state).
// ----------------------------------------------------------------------------

/// Resolve the predecessor [`ValidatedGovernanceState`] for an entry declaring
/// `prev` at `seq` (spec §8.1 step 2-3).
fn predecessor_for(
    linear: &LinearGovernanceState,
    prev: Option<GovernanceId>,
    seq: u64,
) -> Result<ValidatedGovernanceState, Reject> {
    match prev {
        None => {
            if seq != 1 {
                return Err(Reject::InvalidContent);
            }
            // seq == 1 validates against the genesis root cursor.
            Ok(linear.lineage.genesis.clone())
        }
        Some(prev_id) => {
            if seq == 1 {
                return Err(Reject::InvalidContent);
            }
            linear
                .lineage
                .get(&prev_id)
                .map(|node| node.state.clone())
                .ok_or(Reject::MissingDependency)
        }
    }
}

/// Find a retained entry id at `seq` with an id distinct from `new_id` in
/// `community` (a conflicting authorized branch) — the fork trigger.
fn conflicting_entry_at_seq(
    linear: &LinearGovernanceState,
    seq: u64,
    new_id: GovernanceId,
    community: CommunityId,
) -> Option<GovernanceId> {
    // The current accepted tip, if at this seq with a distinct id.
    if let GovernanceTip::Entry {
        seq: tip_seq,
        id: tip_id,
    } = linear.accepted.tip()
    {
        if tip_seq == seq && tip_id != new_id && linear.accepted.state().community_id == community {
            return Some(tip_id);
        }
    }
    // Any retained lineage node at this seq with a distinct id in this community.
    for (id, node) in &linear.lineage.nodes {
        if node.seq == seq && *id != new_id && node.state.state().community_id == community {
            return Some(*id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain;
    use crate::governance::log::authz::validated_genesis_state;
    use crate::governance::log::genesis::{sign_genesis, GenesisConfig, GENESIS_SCHEMA_VERSION};
    use crate::governance::log::model::{CommunityPolicy, RecoveryConfig, Role};
    use crate::governance::log::operation::{
        ForkResolve, GovernanceOperationPayload, MemberGrant, MemberRevoke,
    };
    use crate::governance::log::records::{
        entry_id, GovernanceApproval, GovernanceApprovalBody, GovernanceEntry, GovernanceEntryBody,
    };
    use crate::governance::log::state::{apply, compute_state_root};
    use crate::ids::{GovernanceId, PrincipalId, StateRoot, LEN as N};
    use crate::keys::{verify, SigningKey};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; N])
    }
    fn principal(seed: u8) -> PrincipalId {
        key(seed).member_id()
    }

    /// A genesis config with `w` recovery threshold and admin threshold 2.
    fn genesis_config_with_recovery(w: u16) -> GenesisConfig {
        let mut admins = vec![principal(0xa0), principal(0xa1), principal(0xa2)];
        admins.sort();
        let mut recovery_keys = vec![principal(0xa3), principal(0xa4), principal(0xa5)];
        recovery_keys.sort();
        GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1_000,
            genesis_nonce: [0xab; N],
            admin_threshold: 2,
            administrators: admins,
            recovery: RecoveryConfig {
                threshold: w,
                recovery_keys,
            },
            replicas: Vec::new(),
            community_policy: CommunityPolicy::empty(),
        }
    }

    fn genesis_machine(w: u16) -> (GovernanceMachine, GenesisConfig) {
        let cfg = genesis_config_with_recovery(w);
        let sigs = [
            sign_genesis(&cfg, &key(0xa0)),
            sign_genesis(&cfg, &key(0xa1)),
        ];
        let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
        (GovernanceMachine::from_genesis(genesis), cfg)
    }

    /// Build a verified entry extending `prev` with `payload`, signed by
    /// `signer` and approved by `approvers`.
    fn verified_entry(
        prev: &ValidatedGovernanceState,
        payload: GovernanceOperationPayload,
        signer: &SigningKey,
        approvers: &[&SigningKey],
    ) -> VerifiedGovernanceEntry {
        let (seq, prev_id) = match prev.tip() {
            GovernanceTip::Genesis => (1u64, None),
            GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
        };
        let declared = match apply(prev.state(), &payload) {
            Ok(s) => compute_state_root(&s),
            Err(_) => StateRoot::from_bytes([0; N]),
        };
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

    fn grant_payload(member: PrincipalId) -> GovernanceOperationPayload {
        GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: member,
            role: Role::Member,
        })
    }

    fn verify_entry(body: GovernanceEntryBody) -> VerifiedGovernanceEntry {
        let entry = GovernanceEntry::new(body, &key(0xa0), Vec::new());
        crate::governance::log::records::verify_governance_entry(&entry).expect("verifies")
    }

    /// Build a `fork.resolve` entry selecting `selected_head`, naming exactly
    /// `branch_heads`, signed by `signers` (first signs, rest approve).
    fn resolve_entry(
        forked_state: &GovernanceForkedState,
        selected_head: GovernanceId,
        branch_heads: Vec<GovernanceId>,
        signers: &[&SigningKey],
        created_at_ms: u64,
    ) -> VerifiedGovernanceEntry {
        let selected = forked_state
            .branch(&selected_head)
            .expect("selected head known");
        let (seq, prev_id) = match selected.tip() {
            GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
            GovernanceTip::Genesis => (1, None),
        };
        let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
            branch_heads: branch_heads.clone(),
            selected_head,
            selected_state_root: *selected.committed_state_root(),
            created_at_ms,
        });
        let marker = ResolvedForkMarker {
            branch_heads,
            selected_head,
            selected_state_root: *selected.committed_state_root(),
            created_at_ms,
        };
        let mut state = selected.state().clone();
        state.policy.fork_markers.push(marker);
        state
            .policy
            .fork_markers
            .sort_by_key(|m| *m.selected_head.as_bytes());
        let declared = compute_state_root(&state);
        let body = GovernanceEntryBody {
            community_id: selected.state().community_id,
            seq,
            prev: prev_id,
            created_at_ms: 9_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let fallback_signer = key(0xff);
        let fallback_ref = &fallback_signer;
        let empty: &[&SigningKey] = &[];
        let (signer, approvers) = match signers.split_first() {
            Some((s, rest)) => (*s, rest),
            None => (fallback_ref, empty),
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

    // ========================================================================
    // Acceptance #1: fork detection (same + different predecessor).
    // ========================================================================

    #[test]
    fn same_predecessor_sibling_fork_enters_governance_forked() {
        let (mut machine, _) = genesis_machine(2);
        let genesis = machine.accepted().unwrap().clone();
        // Two distinct quorum-valid grants at seq 1, both extending genesis.
        let a = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let b = verified_entry(
            &genesis,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );

        let obs = machine.observe(&a).expect("a advances");
        assert!(matches!(obs, GovernanceObservation::Advanced { .. }));
        let fork_obs = machine.observe(&b).expect("b reveals fork");
        match fork_obs {
            GovernanceObservation::ForkDetected { evidence } => {
                assert_eq!(evidence.branch_count(), 2);
                assert!(evidence.head_ids().contains(&a.id()));
                assert!(evidence.head_ids().contains(&b.id()));
                assert_eq!(evidence.stable_tip, GovernanceTip::Genesis);
            }
            other => panic!("expected ForkDetected, got {other:?}"),
        }
        assert!(machine.forked().is_some(), "machine is now forked");
    }

    #[test]
    fn different_predecessor_candidates_are_detected_as_a_fork() {
        let cfg = genesis_config_with_recovery(2);
        let sigs = [
            sign_genesis(&cfg, &key(0xa0)),
            sign_genesis(&cfg, &key(0xa1)),
        ];
        let genesis = validated_genesis_state(&cfg, &sigs).unwrap();
        let p1 = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let p2 = verified_entry(
            &genesis,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        let p1_state = validate_governance_candidate(&genesis, &p1)
            .unwrap()
            .resulting()
            .clone();
        let p2_state = validate_governance_candidate(&genesis, &p2)
            .unwrap()
            .resulting()
            .clone();
        let q1 = verified_entry(
            &p1_state,
            grant_payload(principal(0xc2)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let q2 = verified_entry(
            &p2_state,
            grant_payload(principal(0xc3)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        let q1_candidate = validate_governance_candidate(&p1_state, &q1).unwrap();
        let q2_candidate = validate_governance_candidate(&p2_state, &q2).unwrap();

        assert_eq!(q1_candidate.seq(), q2_candidate.seq());
        assert_ne!(q1_candidate.prev(), q2_candidate.prev());
        assert_eq!(
            crate::governance::log::fork::detect_governance_forks([&q1_candidate, &q2_candidate,]),
            Err(Reject::MissingDependency),
            "different predecessors require retained lineage to prove their ancestor"
        );

        let mut linear = match GovernanceMachine::from_genesis(genesis).state {
            GovernanceMachineState::Linear(linear) => linear,
            GovernanceMachineState::GovernanceForked(_) => unreachable!(),
        };
        linear.lineage.insert(
            p1.id(),
            p1.body().seq,
            p1.body().prev,
            p1_state,
            p1.authenticated_evidence().clone(),
        );
        linear.lineage.insert(
            p2.id(),
            p2.body().seq,
            p2.body().prev,
            p2_state,
            p2.authenticated_evidence().clone(),
        );
        linear.lineage.insert(
            q1.id(),
            q1.body().seq,
            q1.body().prev,
            q1_candidate.resulting().clone(),
            q1.authenticated_evidence().clone(),
        );
        linear.accepted = q1_candidate.resulting().clone();

        let (state, observation) =
            GovernanceMachine::enter_forked_from_linear(&linear, &q2_candidate, q1.id())
                .expect("retained lineages prove the fork");
        let GovernanceMachineState::GovernanceForked(forked) = state else {
            panic!("expected GovernanceForked");
        };
        assert!(matches!(
            observation,
            GovernanceObservation::ForkDetected { .. }
        ));
        assert_eq!(forked.head_ids(), {
            let mut heads = vec![q1.id(), q2.id()];
            heads.sort();
            heads
        });
        assert_eq!(forked.stable().tip(), GovernanceTip::Genesis);
    }

    // ========================================================================
    // Acceptance #2: fail-closed member.grant while forked.
    // ========================================================================

    #[test]
    fn member_grant_while_forked_is_rejected_with_unresolved_fork() {
        let (mut machine, _) = genesis_machine(2);
        let genesis = machine.accepted().unwrap().clone();
        let a = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let b = verified_entry(
            &genesis,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        machine.observe(&a).unwrap();
        machine.observe(&b).unwrap();
        assert!(machine.forked().is_some());

        // A fully-valid member.grant against branch a's tip is still rejected.
        let branch_a_state = machine.forked().unwrap().branches[&a.id()].clone();
        let grant = verified_entry(
            &branch_a_state,
            grant_payload(principal(0xc2)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let before_evidence = machine.forked().unwrap().evidence().clone();
        let before_stable = machine.forked().unwrap().stable().clone();
        let before_audit = machine.audit();
        let err = machine.observe(&grant).unwrap_err();
        assert_eq!(err, Reject::UnresolvedFork);
        let after = machine
            .forked()
            .expect("ordinary operation cannot resolve fork");
        assert_eq!(after.evidence(), &before_evidence);
        assert_eq!(after.stable(), &before_stable);
        assert_eq!(machine.audit(), before_audit);

        // A *malformed* member.grant is ALSO UnresolvedFork (the gate precedes
        // operation application — spec §10).
        let malformed = verified_entry(
            &branch_a_state,
            GovernanceOperationPayload::MemberRevoke(MemberRevoke {
                member_id: principal(0xee),
            }),
            &key(0xa0),
            &[&key(0xa1)],
        );
        assert_eq!(
            machine.observe(&malformed).unwrap_err(),
            Reject::UnresolvedFork
        );
    }

    // ========================================================================
    // Acceptance #3: fork.resolve with W succeeds; W-1 does not.
    // ========================================================================

    fn forked_machine_w2() -> (GovernanceMachine, [GovernanceId; 2]) {
        // W=2 recovery threshold; 3 recovery keys (0xa3, 0xa4, 0xa5).
        let (mut machine, _) = genesis_machine(2);
        let genesis = machine.accepted().unwrap().clone();
        let a = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let b = verified_entry(
            &genesis,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        machine.observe(&a).unwrap();
        machine.observe(&b).unwrap();
        let mut heads = [a.id(), b.id()];
        heads.sort();
        (machine, heads)
    }

    #[test]
    fn fork_resolve_with_w_minus_one_signatures_is_insufficient() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // W=2; supply only 1 eligible recovery signer (signer 0xa3, no
        // approvers). Must reject with InsufficientAuthorization.
        let resolve = resolve_entry(&forked, heads[0], heads.to_vec(), &[&key(0xa3)], 1);
        let before_evidence = machine.forked().unwrap().evidence().clone();
        let before_stable = machine.forked().unwrap().stable().clone();
        let before_audit = machine.audit();
        let err = machine.observe(&resolve).unwrap_err();
        assert_eq!(err, Reject::InsufficientAuthorization);
        let after = machine.forked().expect("W-1 leaves the fork unresolved");
        assert_eq!(after.evidence(), &before_evidence);
        assert_eq!(after.stable(), &before_stable);
        assert_eq!(machine.audit(), before_audit);
    }

    #[test]
    fn fork_resolve_with_w_signatures_resolves() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // W=2; supply 2 eligible recovery signers (signer 0xa3 + approver 0xa4).
        let resolve = resolve_entry(
            &forked,
            heads[0],
            heads.to_vec(),
            &[&key(0xa3), &key(0xa4)],
            1,
        );
        let obs = machine.observe(&resolve).expect("W resolves");
        assert!(matches!(obs, GovernanceObservation::Advanced { .. }));
        // Resolved → linear; the accepted tip is the resolution entry.
        let accepted = machine.accepted().expect("linear after resolve");
        match accepted.tip() {
            GovernanceTip::Entry { id, .. } => assert_eq!(id, resolve.id()),
            GovernanceTip::Genesis => panic!("resolved tip must be an entry"),
        }
        // A subsequent ordinary entry validates against the resolved selected
        // state (spec §13.4 / step 7.9).
        let next = verified_entry(
            accepted,
            grant_payload(principal(0xc9)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        machine
            .observe(&next)
            .expect("next ordinary entry validates");
    }

    #[test]
    fn fork_resolve_more_than_w_signatures_resolves() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // All 3 recovery keys sign — superset of W=2.
        let resolve = resolve_entry(
            &forked,
            heads[0],
            heads.to_vec(),
            &[&key(0xa3), &key(0xa4), &key(0xa5)],
            1,
        );
        assert!(machine.observe(&resolve).is_ok());
    }

    // ========================================================================
    // Acceptance #4: no lexical tie-break.
    // ========================================================================

    #[test]
    fn fork_is_not_auto_resolved_by_hash_order() {
        let (mut machine, heads) = forked_machine_w2();
        assert!(machine.forked().is_some(), "no auto-resolution");

        let genesis = {
            let (fresh, _) = genesis_machine(2);
            fresh.accepted().unwrap().clone()
        };
        let lower_entry = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let higher_entry = verified_entry(
            &genesis,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        let (lower_entry, higher_entry) = if lower_entry.id() < higher_entry.id() {
            (lower_entry, higher_entry)
        } else {
            (higher_entry, lower_entry)
        };
        let mut lower_first = GovernanceMachine::from_genesis(genesis.clone());
        lower_first.observe(&lower_entry).unwrap();
        lower_first.observe(&higher_entry).unwrap();
        let mut higher_first = GovernanceMachine::from_genesis(genesis);
        higher_first.observe(&higher_entry).unwrap();
        higher_first.observe(&lower_entry).unwrap();
        assert_eq!(
            lower_first.forked().unwrap().evidence(),
            higher_first.forked().unwrap().evidence()
        );
        assert!(lower_first.accepted().is_none());
        assert!(higher_first.accepted().is_none());

        // Selecting the lexically LARGER head must succeed (proving selection
        // is explicit, not hash order).
        let forked = machine.forked().unwrap().clone();
        let larger = *heads.iter().max().unwrap();
        let resolve_larger = resolve_entry(
            &forked,
            larger,
            heads.to_vec(),
            &[&key(0xa3), &key(0xa4)],
            1,
        );
        machine
            .observe(&resolve_larger)
            .expect("larger head selectable");

        // In a separate fixture, select the lexically SMALLER head.
        let (mut machine2, heads2) = forked_machine_w2();
        let forked2 = machine2.forked().unwrap().clone();
        let smaller = *heads2.iter().min().unwrap();
        let resolve_smaller = resolve_entry(
            &forked2,
            smaller,
            heads2.to_vec(),
            &[&key(0xa3), &key(0xa4)],
            2,
        );
        machine2
            .observe(&resolve_smaller)
            .expect("smaller head selectable");
    }

    // ========================================================================
    // Acceptance #5: both competing approvals preserved in audit evidence.
    // ========================================================================

    #[test]
    fn both_branch_approvals_preserved_in_audit_before_and_after_resolution() {
        let (mut machine, heads) = forked_machine_w2();
        // Before resolution: the forked evidence retains both branches' full
        // authenticated evidence (entry CSBs + signatures + approval evidence).
        let forked = machine.forked().unwrap().clone();
        assert_eq!(forked.evidence().branches.len(), 2);
        let mut seen_entries = Vec::new();
        let mut seen_approvals = Vec::new();
        for branch in &forked.evidence().branches {
            let entry_message =
                domain::signing_message(domain::GOVERNANCE_ENTRY, branch.entry.csb());
            verify(
                &branch.entry.signer(),
                &entry_message,
                branch.entry.signature(),
            )
            .expect("retained branch signature verifies over its exact CSB");
            seen_entries.push((branch.entry.csb().to_vec(), *branch.entry.signature()));
            for approval in branch.entry.approvals() {
                let approval_message =
                    domain::signing_message(domain::GOVERNANCE_APPROVAL, approval.csb());
                verify(
                    &approval.body().approver,
                    &approval_message,
                    approval.signature(),
                )
                .expect("retained approval signature verifies over its exact CSB");
                seen_approvals.push((approval.csb().to_vec(), *approval.signature()));
            }
        }
        assert_eq!(seen_entries.len(), 2);
        assert_ne!(seen_entries[0].0, seen_entries[1].0);

        // Resolve with W recovery signers, selecting heads[0].
        let resolve = resolve_entry(
            &forked,
            heads[0],
            heads.to_vec(),
            &[&key(0xa3), &key(0xa4)],
            7,
        );
        machine.observe(&resolve).expect("resolves");

        // After resolution: the audit record preserves BOTH branches' evidence
        // and the resolution signatures (spec §5.2 item 9 / §11.2).
        let audit = machine.audit();
        let resolved_incident = audit
            .iter()
            .find(|r| matches!(r.status, GovernanceForkAuditStatus::Resolved { .. }))
            .expect("a resolved incident is retained");
        assert_eq!(resolved_incident.branches.len(), 2);
        let audit_entries: Vec<(Vec<u8>, crate::keys::Signature)> = resolved_incident
            .branches
            .iter()
            .map(|branch| (branch.entry.csb().to_vec(), *branch.entry.signature()))
            .collect();
        for entry in &seen_entries {
            assert!(
                audit_entries.contains(entry),
                "entry CSB and signature must survive resolution exactly"
            );
        }
        let audit_approvals: Vec<(Vec<u8>, crate::keys::Signature)> = resolved_incident
            .branches
            .iter()
            .flat_map(|branch| branch.entry.approvals())
            .map(|approval| (approval.csb().to_vec(), *approval.signature()))
            .collect();
        for approval in &seen_approvals {
            assert!(
                audit_approvals.contains(approval),
                "approval CSB and signature must survive resolution exactly"
            );
        }
        // The resolution evidence (signatures) is retained.
        match &resolved_incident.status {
            GovernanceForkAuditStatus::Resolved {
                resolution,
                eligible_recovery_signers,
                ..
            } => {
                assert_eq!(resolution.signature().as_bytes().len(), 64);
                assert!(eligible_recovery_signers.contains(&principal(0xa3)));
                assert!(eligible_recovery_signers.contains(&principal(0xa4)));
            }
            GovernanceForkAuditStatus::Unresolved => panic!("expected resolved"),
        }
    }

    // ========================================================================
    // Resolution validation rules (§9).
    // ========================================================================

    #[test]
    fn fork_resolve_while_linear_is_invalid() {
        let (mut machine, _) = genesis_machine(2);
        // The machine is linear → observing a fork.resolve returns
        // InvalidForkResolution (Rule 1).
        let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
            branch_heads: vec![
                GovernanceId::from_bytes([0x01; N]),
                GovernanceId::from_bytes([0x02; N]),
            ],
            selected_head: GovernanceId::from_bytes([0x01; N]),
            selected_state_root: StateRoot::from_bytes([0; N]),
            created_at_ms: 1,
        });
        let body = GovernanceEntryBody {
            community_id: machine.accepted().unwrap().state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 9_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0; N]),
        };
        let verified = verify_entry(body);
        let err = machine.observe(&verified).unwrap_err();
        assert_eq!(err, Reject::InvalidForkResolution);
    }

    #[test]
    fn fork_resolve_with_unknown_declared_head_is_missing_dependency() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // Declare an extra unknown head → MissingDependency.
        let mut bad_heads = heads.to_vec();
        bad_heads.push(GovernanceId::from_bytes([0xee; N]));
        bad_heads.sort();
        let resolve = resolve_entry(&forked, heads[0], bad_heads, &[&key(0xa3), &key(0xa4)], 1);
        let err = machine.observe(&resolve).unwrap_err();
        assert_eq!(err, Reject::MissingDependency);
    }

    #[test]
    fn fork_resolve_omitting_known_head_is_invalid() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // Declare only heads[0] + a filler unknown → MissingDependency fires
        // before set-equality (the unknown is reported first). Use a known-but-
        // omitted scenario: declare heads but swap one for an unknown so the
        // set differs but all-declared-known fails first.
        let mut omitted = heads.to_vec();
        omitted[1] = GovernanceId::from_bytes([0xee; N]); // unknown replacement
        let resolve = resolve_entry(&forked, heads[0], omitted, &[&key(0xa3), &key(0xa4)], 1);
        let err = machine.observe(&resolve).unwrap_err();
        assert_eq!(err, Reject::MissingDependency);
    }

    #[test]
    fn fork_resolve_unknown_selected_head_is_invalid_at_decode() {
        let heads = [
            GovernanceId::from_bytes([0x01; N]),
            GovernanceId::from_bytes([0x02; N]),
        ];
        let foreign = GovernanceId::from_bytes([0xee; N]);
        let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
            branch_heads: heads.to_vec(),
            selected_head: foreign,
            selected_state_root: StateRoot::from_bytes([0; N]),
            created_at_ms: 1,
        });
        let cbor = payload.to_cbor();
        assert_eq!(
            GovernanceOperationPayload::from_canonical(payload.kind(), &cbor).err(),
            Some(Reject::InvalidForkResolution)
        );
    }

    #[test]
    fn fork_resolve_wrong_seq_is_invalid() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        let selected = forked.branch(&heads[0]).unwrap().clone();
        let selected_seq = match selected.tip() {
            GovernanceTip::Entry { seq, .. } => seq,
            GovernanceTip::Genesis => 0,
        };
        let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
            branch_heads: heads.to_vec(),
            selected_head: heads[0],
            selected_state_root: *selected.committed_state_root(),
            created_at_ms: 1,
        });
        let marker = ResolvedForkMarker {
            branch_heads: heads.to_vec(),
            selected_head: heads[0],
            selected_state_root: *selected.committed_state_root(),
            created_at_ms: 1,
        };
        let mut state = selected.state().clone();
        state.policy.fork_markers.push(marker);
        state
            .policy
            .fork_markers
            .sort_by_key(|m| *m.selected_head.as_bytes());
        let declared = compute_state_root(&state);
        let body = GovernanceEntryBody {
            community_id: selected.state().community_id,
            seq: selected_seq + 2, // wrong (should be +1)
            prev: Some(heads[0]),
            created_at_ms: 9_000,
            kind: payload.kind(),
            payload,
            state_root: declared,
        };
        let verified = verify_entry(body);
        let err = machine.observe(&verified).unwrap_err();
        assert_eq!(err, Reject::InvalidForkResolution);
    }

    #[test]
    fn fork_resolve_selected_root_mismatch_is_state_root_mismatch() {
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        let selected = forked.branch(&heads[0]).unwrap().clone();
        let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
            branch_heads: heads.to_vec(),
            selected_head: heads[0],
            selected_state_root: StateRoot::from_bytes([0xff; N]), // wrong
            created_at_ms: 1,
        });
        let (seq, prev_id) = match selected.tip() {
            GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
            GovernanceTip::Genesis => (1, None),
        };
        let body = GovernanceEntryBody {
            community_id: selected.state().community_id,
            seq,
            prev: prev_id,
            created_at_ms: 9_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0xee; N]),
        };
        let verified = verify_entry(body);
        let err = machine.observe(&verified).unwrap_err();
        assert_eq!(err, Reject::StateRootMismatch);
    }

    #[test]
    fn duplicate_entry_id_is_not_a_fork() {
        let (mut machine, _) = genesis_machine(2);
        let genesis = machine.accepted().unwrap().clone();
        let a = verified_entry(
            &genesis,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        machine.observe(&a).unwrap();
        // Re-observe the exact same entry → Duplicate, not a fork.
        let obs = machine.observe(&a).expect("duplicate observed");
        assert!(matches!(obs, GovernanceObservation::Duplicate { .. }));
        assert!(machine.accepted().is_some());
    }

    #[test]
    fn missing_predecessor_returns_missing_dependency() {
        let (mut machine, _) = genesis_machine(2);
        let payload = grant_payload(principal(0xc0));
        let body = GovernanceEntryBody {
            community_id: machine.accepted().unwrap().state().community_id,
            seq: 2,
            prev: Some(GovernanceId::from_bytes([0x99; N])),
            created_at_ms: 2_000,
            kind: payload.kind(),
            payload,
            state_root: StateRoot::from_bytes([0; N]),
        };
        let verified = verify_entry(body);
        let err = machine.observe(&verified).unwrap_err();
        assert_eq!(err, Reject::MissingDependency);
    }

    #[test]
    fn disabled_recovery_config_fails_closed() {
        // W=0 (threshold 0) → config is malformed → fail closed, no admin
        // fallback (spec §5.4 item 14).
        let (mut machine0, _) = genesis_machine(0);
        let genesis0 = machine0.accepted().unwrap().clone();
        let a0 = verified_entry(
            &genesis0,
            grant_payload(principal(0xc0)),
            &key(0xa0),
            &[&key(0xa1)],
        );
        let b0 = verified_entry(
            &genesis0,
            grant_payload(principal(0xc1)),
            &key(0xa1),
            &[&key(0xa2)],
        );
        machine0.observe(&a0).unwrap();
        machine0.observe(&b0).unwrap();
        let forked0 = machine0.forked().unwrap().clone();
        let mut heads0 = [a0.id(), b0.id()];
        heads0.sort();
        let resolve0 = resolve_entry(
            &forked0,
            heads0[0],
            heads0.to_vec(),
            &[&key(0xa3), &key(0xa4)],
            1,
        );
        let err = machine0.observe(&resolve0).unwrap_err();
        assert_eq!(err, Reject::InsufficientAuthorization);
    }

    #[test]
    fn administrator_only_signers_cannot_authorize_resolution() {
        // Admins (0xa0..0xa2) signing a resolution when they are NOT recovery
        // keys must fail: ordinary admin quorum is neither required nor
        // sufficient unless those principals are also recovery keys (spec §5.4
        // item 9). Recovery keys are 0xa3..0xa5.
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // Two admins sign (not recovery keys).
        let resolve = resolve_entry(
            &forked,
            heads[0],
            heads.to_vec(),
            &[&key(0xa0), &key(0xa1)],
            1,
        );
        let err = machine.observe(&resolve).unwrap_err();
        assert_eq!(err, Reject::InsufficientAuthorization);
    }

    #[test]
    fn signer_also_approving_counts_once() {
        // A recovery key that both signs and approves counts once. With W=2
        // and one recovery principal double-presenting, the count must still be
        // 1 → InsufficientAuthorization.
        let (mut machine, heads) = forked_machine_w2();
        let forked = machine.forked().unwrap().clone();
        // 0xa3 signs and also approves itself → still 1 distinct eligible.
        let resolve = resolve_entry(
            &forked,
            heads[0],
            heads.to_vec(),
            &[&key(0xa3), &key(0xa3)],
            1,
        );
        let err = machine.observe(&resolve);
        // Note: duplicate approver is rejected at record construction
        // (InvalidApproval) — so this surfaces as a verification error before
        // reaching the machine. Either way, it must NOT authorize.
        assert!(err.is_err(), "double-presenting signer must not authorize");
    }
}
