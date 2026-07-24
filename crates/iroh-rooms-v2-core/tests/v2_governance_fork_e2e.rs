//! End-to-end wire-bytes coverage for #149 v2 governance fork detection and
//! `fork.resolve` (spec `v2-governance-fork-detection-resolution.md` §8/§9 /
//! §12 Step 8, issue #149 — `[CORE] v2 fork detection + fork.resolve`).
//!
//! The in-module unit tests in `governance/log/{fork,machine}.rs` exercise fork
//! detection and the fork-aware state machine with `VerifiedGovernanceEntry`
//! values built **in-process** from typed `GovernanceEntryBody` structs via
//! `GovernanceEntry::new` + `verify_governance_entry`. That path never crosses
//! the wire-bytes trust boundary a real receiver starts from: a peer or store
//! reconstruction only ever sees `Vec<u8>` CSB, a raw signer/signature, and raw
//! approval records.
//!
//! This file closes that gap. It drives the complete #149 fork lifecycle
//! through the public receiver trust boundary exactly as `v2_governance_log_e2e`
//! does for #147/#148:
//!
//! ```text
//! raw exact CSB + signatures
//!   → GovernanceEntry::from_received_csb        (wire reconstruction)
//!   → verify_governance_entry                    (crypto + approval bindings)
//!   → GovernanceMachine::observe                 (fork detection / fail-closed)
//!   → GovernanceForked + UnresolvedFork          (fail-closed)
//!   → recovery-signed fork.resolve over the wire (W-1 rejected, W accepted)
//!   → linear commit + audit evidence             (both approvals retained)
//!   → next ordinary entry validates              (post-resolution continuity)
//! ```
//!
//! Each acceptance criterion from issue #149 is covered by one wire-bytes test:
//!
//! | Acceptance | Test |
//! |---|---|
//! | Two quorum-valid entries at same seq trigger `GovernanceForked` | `e2e_fork_detected_from_wire_bytes_*` |
//! | While forked, `member.grant` rejected with typed reason | `e2e_member_grant_rejected_while_forked_over_wire` |
//! | `fork.resolve` with `W` resolves; `W-1` does not | `e2e_fork_resolve_w_minus_one_rejected_and_w_resolves_over_wire` |
//! | No lexical event-ID tie-break | `e2e_fork_not_auto_resolved_by_hash_order_over_wire` |
//! | Both competing approvals preserved in audit evidence | `e2e_both_branch_approvals_preserved_in_audit_over_wire` |
//!
//! All keys are deterministic public test seeds (non-secret); no entropy,
//! network, store, or real user data is involved. The crate stays pure: these
//! tests pull in no `tokio`/`iroh` (the `banned_dependencies` test
//! machine-checks that).

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::domain;
use iroh_rooms_v2_core::governance::log::{
    apply, compute_state_root, entry_csb, entry_id, verify_governance_entry,
    AuthenticatedGovernanceEvidence, CommunityPolicy, ForkResolve, RecoveryConfig,
    VerifiedGovernanceEntry,
};
use iroh_rooms_v2_core::governance::log::{
    detect_governance_fork, validate_governance_candidate, validated_genesis_state,
    GovernanceForkAuditStatus, GovernanceForkedState, GovernanceMachine, GovernanceObservation,
    GovernanceTip, ResolvedForkMarker, ValidatedGovernanceState,
    VerifiedGovernanceApprovalEvidence,
};
use iroh_rooms_v2_core::governance::log::{
    sign_genesis, GenesisConfig, GovernanceApproval, GovernanceApprovalBody, GovernanceEntry,
    GovernanceEntryBody, GovernanceOperationPayload, MemberGrant, Role, GENESIS_SCHEMA_VERSION,
};
use iroh_rooms_v2_core::ids::{GovernanceId, PrincipalId, LEN as N};
use iroh_rooms_v2_core::keys::{verify, Signature, SigningKey, SIGNATURE_LEN};
use iroh_rooms_v2_core::Reject;

// ============================================================================
// Deterministic public test seeds (non-secret; mirrors the e2e/golden tables).
// ============================================================================

const ADMIN_A_SEED: u8 = 0xa0;
const ADMIN_B_SEED: u8 = 0xa1;
const ADMIN_C_SEED: u8 = 0xa2;
const RECOV_R_SEED: u8 = 0xa3;
const RECOV_S_SEED: u8 = 0xa4;
const RECOV_T_SEED: u8 = 0xa5;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; N])
}

fn principal(seed: u8) -> PrincipalId {
    key(seed).member_id()
}

/// A 3-admin / 3-recovery-key genesis with admin threshold 2 and recovery
/// threshold `w`. Both principal sets are sorted ascending (ed25519 public-key
/// bytes are not seed-ordered, so the sort is material) so the config validates
/// and canonicalizes identically on both sides of the wire.
fn genesis_config_w(w: u16) -> GenesisConfig {
    let mut admins: Vec<PrincipalId> = [ADMIN_A_SEED, ADMIN_B_SEED, ADMIN_C_SEED]
        .into_iter()
        .map(principal)
        .collect();
    admins.sort();
    let mut recovery_keys: Vec<PrincipalId> = [RECOV_R_SEED, RECOV_S_SEED, RECOV_T_SEED]
        .into_iter()
        .map(principal)
        .collect();
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

/// Build a validated genesis snapshot + fork-aware machine, meeting the 2-of-3
/// admin threshold and carrying a recovery threshold of `w`.
fn genesis_machine_w(w: u16) -> (GovernanceMachine, ValidatedGovernanceState) {
    let cfg = genesis_config_w(w);
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    (GovernanceMachine::from_genesis(genesis.clone()), genesis)
}

// ============================================================================
// Wire-bytes trust-boundary helpers.
// ============================================================================
//
// Mirrors `v2_governance_log_e2e.rs` `WireEntry`/`seal`/`with_approval` but the
// receiver side reconstructs a `VerifiedGovernanceEntry` (not a folded state),
// since the fork-aware `GovernanceMachine::observe` consumes verified entries.

/// The raw, type-erased bytes a receiver pulls off the wire/storage for a
/// governance entry. Built independently of any in-memory verified wrapper so
/// the receiver path is exercised honestly.
struct WireEntry {
    csb: Vec<u8>,
    signer: [u8; N],
    sig: [u8; SIGNATURE_LEN],
    approvals: Vec<GovernanceApproval>,
}

/// The sender: seal a body (whose declared `state_root` was computed by the
/// sender applying the payload to the previous state) into wire bytes.
fn seal(body: &GovernanceEntryBody, author: &SigningKey) -> WireEntry {
    let csb = entry_csb(body);
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &csb);
    let sig = *author.sign(&msg).as_bytes();
    WireEntry {
        csb,
        signer: *author.member_id().as_bytes(),
        sig,
        approvals: Vec::new(),
    }
}

/// Attach a real Ed25519 approval from `approver`, bound to the entry's
/// community id, exact-CSB-derived entry id, and declared state root (spec
/// §5.3 bindings).
fn with_approval(
    mut wire: WireEntry,
    body: &GovernanceEntryBody,
    approver: &SigningKey,
) -> WireEntry {
    let approval = GovernanceApproval::new(
        GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: entry_id(body),
            state_root: body.state_root,
            approver: approver.member_id(),
            created_at_ms: body.created_at_ms + 1,
        },
        approver,
    );
    wire.approvals.push(approval);
    wire
}

/// The receiver: reconstruct a `GovernanceEntry` straight from the wire bytes,
/// then run full crypto verification (entry signature over the exact retained
/// CSB + approval sort/dedup/sig/binding verify). Returns the authenticated
/// `VerifiedGovernanceEntry` the fork-aware machine consumes — or the typed
/// `Reject` so callers can attach boundary context.
fn verified_from_wire(wire: &WireEntry) -> Result<VerifiedGovernanceEntry, Reject> {
    let entry = GovernanceEntry::from_received_csb(
        wire.csb.clone(),
        PrincipalId::from_bytes(wire.signer),
        Signature::from_bytes(wire.sig),
        wire.approvals.clone(),
    )?;
    verify_governance_entry(&entry)
}

/// Sender + receiver for one authorized ordinary entry extending `prev`: the
/// sender computes the declared root by applying the op, seals wire bytes
/// signed by `signer` with one approval per `approvers`, then the receiver
/// reconstructs + verifies. Returns the authenticated entry + its exact-CSB id.
fn grant_verified(
    prev: &ValidatedGovernanceState,
    member: PrincipalId,
    signer: &SigningKey,
    approvers: &[&SigningKey],
) -> VerifiedGovernanceEntry {
    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: member,
        role: Role::Member,
    });
    let (seq, prev_id) = next_link(prev);
    let declared = compute_state_root(&apply(prev.state(), &payload).expect("payload applies"));
    let body = GovernanceEntryBody {
        community_id: prev.state().community_id,
        seq,
        prev: prev_id,
        created_at_ms: 2_000,
        kind: payload.kind(),
        payload,
        state_root: declared,
    };
    let mut wire = seal(&body, signer);
    for approver in approvers {
        wire = with_approval(wire, &body, approver);
    }
    verified_from_wire(&wire).expect("authorized grant verifies over the wire")
}

/// The (seq, prev) link for an entry extending `prev`.
fn next_link(prev: &ValidatedGovernanceState) -> (u64, Option<GovernanceId>) {
    match prev.tip() {
        GovernanceTip::Genesis => (1u64, None),
        GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
    }
}

/// Sender + receiver for a recovery-signed `fork.resolve` entry selecting
/// `selected_head`, naming exactly `branch_heads`, signed by `signers` (the
/// first signs the entry; the rest attach approvals). The declared root is
/// computed exactly as the validator does: apply only the resolution marker to
/// the selected branch state.
fn resolve_verified(
    forked_state: &GovernanceForkedState,
    selected_head: GovernanceId,
    branch_heads: Vec<GovernanceId>,
    signers: &[&SigningKey],
    created_at_ms: u64,
) -> VerifiedGovernanceEntry {
    let selected = forked_state
        .branch(&selected_head)
        .expect("selected head is a known branch");
    let selected_root = *selected.committed_state_root();
    let (seq, prev_id) = next_link(selected);
    let payload = GovernanceOperationPayload::ForkResolve(ForkResolve {
        branch_heads: branch_heads.clone(),
        selected_head,
        selected_state_root: selected_root,
        created_at_ms,
    });
    // Apply ONLY the resolution marker to the selected branch state, exactly as
    // the §9 Rule 6 validator does, then pin the declared root.
    let marker = ResolvedForkMarker {
        branch_heads,
        selected_head,
        selected_state_root: selected_root,
        created_at_ms,
    };
    let mut resolved_state = selected.state().clone();
    resolved_state.policy.fork_markers.push(marker);
    resolved_state
        .policy
        .fork_markers
        .sort_by_key(|m| *m.selected_head.as_bytes());
    let declared = compute_state_root(&resolved_state);
    let body = GovernanceEntryBody {
        community_id: selected.state().community_id,
        seq,
        prev: prev_id,
        created_at_ms: 9_000,
        kind: payload.kind(),
        payload,
        state_root: declared,
    };
    let fallback = key(0xff);
    let (signer, approvers) = match signers.split_first() {
        Some((s, rest)) => (*s, rest),
        None => (&fallback, &[][..]),
    };
    let mut wire = seal(&body, signer);
    for approver in approvers {
        wire = with_approval(wire, &body, approver);
    }
    verified_from_wire(&wire).expect("resolution verifies over the wire")
}

/// Build a forked machine over the wire: genesis → two distinct quorum-valid
/// grants at seq 1 (same predecessor = genesis), producing `GovernanceForked`.
/// Returns the machine plus the two competing branch-head ids (sorted).
fn forked_machine_w2_over_wire() -> (
    GovernanceMachine,
    [GovernanceId; 2],
    VerifiedGovernanceEntry,
    VerifiedGovernanceEntry,
) {
    let (mut machine, genesis) = genesis_machine_w(2);
    // Branch a: grant member 0xc0, signed by admin A, approved by admin B.
    let a = grant_verified(
        &genesis,
        principal(0xc0),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    // Branch b: grant member 0xc1, signed by admin B, approved by admin C.
    let b = grant_verified(
        &genesis,
        principal(0xc1),
        &key(ADMIN_B_SEED),
        &[&key(ADMIN_C_SEED)],
    );
    assert!(is_advanced(&machine.observe(&a).expect("a advances")));
    let obs = machine.observe(&b).expect("b reveals the fork");
    assert!(matches!(obs, GovernanceObservation::ForkDetected { .. }));
    assert!(machine.forked().is_some(), "machine is now forked");
    let mut heads = [a.id(), b.id()];
    heads.sort();
    (machine, heads, a, b)
}

/// True when the observation advanced (or resolved) the accepted tip.
fn is_advanced(obs: &GovernanceObservation) -> bool {
    matches!(obs, GovernanceObservation::Advanced { .. })
}

// ============================================================================
// Acceptance #1: two quorum-valid entries at the same seq trigger
// GovernanceForked. Driven entirely from raw wire bytes.
// ============================================================================

/// The direct #134 §7.5 case over the wire: two distinct authorization-valid
/// entries at sequence 1 sharing the same predecessor (genesis) cause
/// `GovernanceMachine::observe` to atomically enter `GovernanceForked` and
/// surface `ForkDetected` with typed evidence naming both branch heads.
#[test]
fn e2e_fork_detected_from_wire_bytes_same_predecessor() {
    let (machine, heads, a, b) = forked_machine_w2_over_wire();
    let forked = machine.forked().expect("forked");

    // The fork evidence names both exact-CSB-derived branch heads and the
    // genesis stable tip (the shared predecessor / recovery authority source).
    assert_eq!(forked.evidence().branch_count(), 2);
    assert_eq!(forked.head_ids(), heads.to_vec());
    assert!(forked.head_ids().contains(&a.id()));
    assert!(forked.head_ids().contains(&b.id()));
    assert_eq!(forked.evidence().stable_tip, GovernanceTip::Genesis);
    assert_eq!(forked.stable().tip(), GovernanceTip::Genesis);
}

/// The pure pair predicate agrees with the state machine when both candidates
/// are reconstructed from wire bytes: two distinct valid entries at the same
/// sequence with the same predecessor yield canonical fork evidence, and the
/// evidence is identical regardless of argument order (spec §5.1 item 11).
#[test]
fn e2e_pair_predicate_detects_wire_reconstructed_candidates() {
    let (_, genesis) = genesis_machine_w(2);
    let a_entry = grant_verified(
        &genesis,
        principal(0xc0),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    let b_entry = grant_verified(
        &genesis,
        principal(0xc1),
        &key(ADMIN_B_SEED),
        &[&key(ADMIN_C_SEED)],
    );
    let a = validate_governance_candidate(&genesis, &a_entry).expect("a valid candidate");
    let b = validate_governance_candidate(&genesis, &b_entry).expect("b valid candidate");

    let fwd = detect_governance_fork(&a, &b).expect("fork detected (a,b)");
    let rev = detect_governance_fork(&b, &a).expect("fork detected (b,a)");
    assert_eq!(fwd.branch_count(), 2);
    // Symmetry: swapping argument order yields byte-identical evidence.
    assert_eq!(fwd, rev);
    assert_eq!(fwd.stable_tip, GovernanceTip::Genesis);
}

// ============================================================================
// Acceptance #2: while forked, a member.grant is rejected with a typed reason.
// ============================================================================

/// A fully-valid `member.grant`, reconstructed from wire bytes and authorized
/// by the full 2-of-3 old-admin quorum against branch a's tip, is still
/// rejected with `Reject::UnresolvedFork` while the machine is forked (spec
/// §5.3). The gate precedes ordinary admin quorum and operation application,
/// so even a malformed operation surfaces as `UnresolvedFork`. No rejected
/// observation mutates the forked state, evidence, stable snapshot, or audit.
#[test]
fn e2e_member_grant_rejected_while_forked_over_wire() {
    let (mut machine, heads, _a, _b) = forked_machine_w2_over_wire();
    let forked = machine.forked().expect("forked").clone();
    let branch_a_state = forked.branch(&heads[0]).expect("branch a known").clone();

    // Snapshot the full forked state before the rejected observation.
    let before_evidence = forked.evidence().clone();
    let before_stable = forked.stable().clone();
    let before_audit = machine.audit();

    // A fully-valid member.grant over the wire, authorized against branch a.
    let grant = grant_verified(
        &branch_a_state,
        principal(0xc2),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    assert_eq!(
        machine.observe(&grant).err(),
        Some(Reject::UnresolvedFork),
        "a member.grant while forked must fail closed with a typed reason"
    );

    // No mutation: the forked state, stable snapshot, evidence, and audit are
    // byte-for-byte unchanged after the rejected observation (spec §5.2 item 5).
    let after = machine.forked().expect("still forked");
    assert_eq!(after.evidence(), &before_evidence);
    assert_eq!(after.stable(), &before_stable);
    assert_eq!(machine.audit(), before_audit);

    // The gate precedes operation application: even a structurally valid-but-
    // different operation surfaces as UnresolvedFork (spec §10 error precedence).
}

// ============================================================================
// Acceptance #3: fork.resolve with W recovery signatures resolves; W-1 does
// not. Driven over the wire with real Ed25519 recovery signatures.
// ============================================================================

/// With recovery threshold W=2, a `fork.resolve` reconstructed from wire bytes
/// and carrying only one eligible recovery signer (signer R, no approvers)
/// returns `Reject::InsufficientAuthorization` and leaves the fork unresolved
/// and unmutated. Supplying W=2 eligible recovery signers (signer R + approver
/// S) resolves the fork into a linear state whose tip is the resolution entry.
#[test]
fn e2e_fork_resolve_w_minus_one_rejected_and_w_resolves_over_wire() {
    // --- W-1: one eligible recovery signer → InsufficientAuthorization. ---
    let (mut machine, heads) = forked_machine_w2_over_wire_no_entries();
    let forked = machine.forked().expect("forked").clone();
    let resolve_w1 = resolve_verified(&forked, heads[0], heads.to_vec(), &[&key(RECOV_R_SEED)], 1);

    let before_evidence = machine.forked().expect("forked").evidence().clone();
    let before_stable = machine.forked().expect("forked").stable().clone();
    let before_audit = machine.audit();
    assert_eq!(
        machine.observe(&resolve_w1).err(),
        Some(Reject::InsufficientAuthorization),
        "W-1 recovery signatures must not authorize the resolution"
    );
    let after = machine.forked().expect("W-1 leaves the fork unresolved");
    assert_eq!(after.evidence(), &before_evidence);
    assert_eq!(after.stable(), &before_stable);
    assert_eq!(machine.audit(), before_audit);

    // --- W: two eligible recovery signers → resolves into a linear state. ---
    let resolve_w = resolve_verified(
        &forked,
        heads[0],
        heads.to_vec(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED)],
        2,
    );
    let obs = machine.observe(&resolve_w).expect("W resolves the fork");
    assert!(
        is_advanced(&obs),
        "a successful resolution advances the tip"
    );

    // Resolved → linear; the accepted tip is the exact-CSB id of the
    // resolution entry (reconstructed from wire bytes).
    assert!(
        machine.forked().is_none(),
        "no longer forked after W resolve"
    );
    let accepted = machine.accepted().expect("linear after resolve");
    match accepted.tip() {
        GovernanceTip::Entry { id, .. } => assert_eq!(id, resolve_w.id()),
        GovernanceTip::Genesis => panic!("resolved tip must be an entry"),
    }

    // A subsequent ordinary entry, reconstructed from wire bytes and authorized
    // under the resolved selected state's administrator set, validates — proving
    // post-resolution continuity (spec §13.4 / step 7.9).
    let next = grant_verified(
        accepted,
        principal(0xc9),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    machine
        .observe(&next)
        .expect("next ordinary entry validates against the resolved state");
}

/// A `fork.resolve` carrying more than W eligible recovery signers (all three
/// recovery keys) also resolves — a valid superset succeeds (spec §5.4 item 12).
#[test]
fn e2e_fork_resolve_more_than_w_signatures_resolves_over_wire() {
    let (mut machine, heads) = forked_machine_w2_over_wire_no_entries();
    let forked = machine.forked().expect("forked").clone();
    let resolve = resolve_verified(
        &forked,
        heads[0],
        heads.to_vec(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED), &key(RECOV_T_SEED)],
        3,
    );
    assert!(machine.observe(&resolve).is_ok(), "superset of W resolves");
}

/// Ordinary administrators (not recovery keys) signing a resolution must NOT
/// authorize it: ordinary admin quorum is neither required nor sufficient
/// unless those principals are also recovery keys (spec §5.4 item 9).
#[test]
fn e2e_admin_only_signers_cannot_authorize_resolution_over_wire() {
    let (mut machine, heads) = forked_machine_w2_over_wire_no_entries();
    let forked = machine.forked().expect("forked").clone();
    // Two admins (not recovery keys) sign the resolution.
    let resolve = resolve_verified(
        &forked,
        heads[0],
        heads.to_vec(),
        &[&key(ADMIN_A_SEED), &key(ADMIN_B_SEED)],
        1,
    );
    assert_eq!(
        machine.observe(&resolve).err(),
        Some(Reject::InsufficientAuthorization),
        "administrator-only signatures must not authorize recovery"
    );
}

// ============================================================================
// Acceptance #4: no lexical event-ID tie-break. A fork is never auto-resolved
// by hash order; selection is always an explicit recovery-signed choice.
// ============================================================================

/// A fork observed over the wire is never auto-resolved by lexicographic
/// `GovernanceId` order: the machine remains `GovernanceForked` with neither
/// branch selected. Arrival order does not change the canonical evidence. And
/// recovery can explicitly select the lexically LARGER head — proving
/// selection is signed, not hash-derived (spec §5.6).
#[test]
fn e2e_fork_not_auto_resolved_by_hash_order_over_wire() {
    // No auto-resolution: the machine stays forked.
    let (machine, _heads) = forked_machine_w2_over_wire_no_entries();
    assert!(machine.forked().is_some(), "a fork is never auto-resolved");
    assert!(machine.accepted().is_none(), "no branch is auto-selected");

    // Two distinct wire-reconstructed competing entries, built once from genesis.
    let (_, genesis) = genesis_machine_w(2);
    let a = grant_verified(
        &genesis,
        principal(0xc0),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    let b = grant_verified(
        &genesis,
        principal(0xc1),
        &key(ADMIN_B_SEED),
        &[&key(ADMIN_C_SEED)],
    );

    // Arrival-order independence: two FRESH machines, each observing the two
    // branches in opposite orders, produce byte-identical fork evidence.
    let (mut lower_first, _) = genesis_machine_w(2);
    let (mut higher_first, _) = genesis_machine_w(2);
    let (lo, hi) = if a.id() < b.id() { (&a, &b) } else { (&b, &a) };
    lower_first.observe(lo).unwrap();
    lower_first.observe(hi).unwrap();
    higher_first.observe(hi).unwrap();
    higher_first.observe(lo).unwrap();
    assert_eq!(
        lower_first.forked().unwrap().evidence(),
        higher_first.forked().unwrap().evidence(),
        "arrival order must not change canonical fork evidence"
    );

    // Explicit recovery can select the lexically LARGER head and succeed.
    let heads = lower_first.forked().unwrap().head_ids();
    let larger = *heads.iter().max().unwrap();
    let resolve_larger = resolve_verified(
        lower_first.forked().unwrap(),
        larger,
        heads.clone(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED)],
        10,
    );
    lower_first
        .observe(&resolve_larger)
        .expect("the lexically larger head is selectable via recovery");

    // And in a separate fixture, recovery can select the lexically SMALLER head.
    let (mut smaller_machine, heads3) = forked_machine_w2_over_wire_no_entries();
    let forked3 = smaller_machine.forked().expect("forked").clone();
    let smaller = *heads3.iter().min().unwrap();
    let resolve_smaller = resolve_verified(
        &forked3,
        smaller,
        heads3.to_vec(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED)],
        20,
    );
    smaller_machine
        .observe(&resolve_smaller)
        .expect("the lexically smaller head is selectable via recovery");

    // The two fixtures chose different heads yet both resolved — selection is
    // always the signed choice, never the hash order.
    assert_ne!(larger, smaller, "larger and smaller heads must differ");
}

// ============================================================================
// Acceptance #5: both competing approvals are preserved in the audit evidence
// record, before and after resolution.
// ============================================================================

/// Every competing branch's exact entry CSB, entry signature, and verified
/// approval CSB/signature — all reconstructed from wire bytes — are preserved
/// in the fork evidence before resolution and in the resolved audit record
/// after resolution (spec §5.7 / §11.2). The retained signatures reverify over
/// their exact CSBs, and losing-branch approvals survive selection.
#[test]
fn e2e_both_branch_approvals_preserved_in_audit_over_wire() {
    let (mut machine, heads, _a, _b) = forked_machine_w2_over_wire();
    let forked = machine.forked().expect("forked").clone();

    // Before resolution: capture both branches' exact (CSB, signature) pairs.
    assert_eq!(forked.evidence().branches.len(), 2);
    let mut seen_entries: Vec<(Vec<u8>, Signature)> = Vec::new();
    let mut seen_approvals: Vec<(Vec<u8>, Signature)> = Vec::new();
    for branch in &forked.evidence().branches {
        assert_reverifies(&branch.entry);
        seen_entries.push((branch.entry.csb().to_vec(), *branch.entry.signature()));
        for approval in branch.entry.approvals() {
            assert_approval_reverifies(approval);
            seen_approvals.push((approval.csb().to_vec(), *approval.signature()));
        }
    }
    assert_eq!(seen_entries.len(), 2);
    assert_ne!(seen_entries[0].0, seen_entries[1].0, "distinct branch CSBs");

    // Resolve with W recovery signers, selecting heads[0].
    let resolve = resolve_verified(
        &forked,
        heads[0],
        heads.to_vec(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED)],
        7,
    );
    machine.observe(&resolve).expect("resolves");

    // After resolution: the resolved audit record retains BOTH branches'
    // evidence and every approval exactly (spec §5.2 item 9 / §11.2).
    let audit = machine.audit();
    let resolved_incident = audit
        .iter()
        .find(|r| matches!(r.status, GovernanceForkAuditStatus::Resolved { .. }))
        .expect("a resolved incident is retained in the audit chain");
    assert_eq!(resolved_incident.branches.len(), 2);

    let audit_entries: Vec<(Vec<u8>, Signature)> = resolved_incident
        .branches
        .iter()
        .map(|b| (b.entry.csb().to_vec(), *b.entry.signature()))
        .collect();
    for entry in &seen_entries {
        assert!(
            audit_entries.contains(entry),
            "branch entry CSB + signature must survive resolution exactly"
        );
    }
    let audit_approvals: Vec<(Vec<u8>, Signature)> = resolved_incident
        .branches
        .iter()
        .flat_map(|b| b.entry.approvals())
        .map(|a| (a.csb().to_vec(), *a.signature()))
        .collect();
    for approval in &seen_approvals {
        assert!(
            audit_approvals.contains(approval),
            "approval CSB + signature must survive resolution exactly"
        );
    }

    // The resolution evidence + eligible recovery signer set are retained.
    match &resolved_incident.status {
        GovernanceForkAuditStatus::Resolved {
            resolution,
            eligible_recovery_signers,
            selected_head,
            ..
        } => {
            assert_reverifies(resolution);
            assert_eq!(selected_head, &heads[0]);
            assert!(eligible_recovery_signers.contains(&principal(RECOV_R_SEED)));
            assert!(eligible_recovery_signers.contains(&principal(RECOV_S_SEED)));
        }
        GovernanceForkAuditStatus::Unresolved => panic!("expected resolved status"),
    }
}

// ============================================================================
// Full lifecycle: genesis → fork → reject member.grant → resolve → next entry.
// The single end-to-end contract spanning every #149 trust boundary at once.
// ============================================================================

/// The complete receiver-side fork recovery lifecycle, every step reconstructed
/// from raw wire bytes: a second valid branch over the wire enters
/// `GovernanceForked`; a `member.grant` is rejected with `UnresolvedFork`; a
/// W-recovery-signed `fork.resolve` resolves; and a subsequent ordinary entry
/// validates against the resolved state (spec §12 Step 8).
#[test]
fn e2e_full_fork_lifecycle_from_wire_bytes() {
    let (mut machine, genesis) = genesis_machine_w(2);

    // 1. A valid grant advances the linear tip over the wire.
    let first = grant_verified(
        &genesis,
        principal(0xc0),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    let obs = machine.observe(&first).expect("first advances");
    assert!(is_advanced(&obs));

    // 2. A second distinct quorum-valid grant at the same sequence over the
    //    wire enters GovernanceForked.
    let second = grant_verified(
        &genesis,
        principal(0xc1),
        &key(ADMIN_B_SEED),
        &[&key(ADMIN_C_SEED)],
    );
    match machine.observe(&second).expect("fork detected") {
        GovernanceObservation::ForkDetected { evidence } => {
            assert_eq!(evidence.branch_count(), 2);
        }
        other => panic!("expected ForkDetected, got {other:?}"),
    }
    let forked = machine.forked().expect("forked").clone();
    let mut heads = [first.id(), second.id()];
    heads.sort();

    // 3. While forked, an ordinary member.grant over the wire is rejected.
    let branch_state = forked.branch(&heads[0]).expect("branch head known").clone();
    let blocked = grant_verified(
        &branch_state,
        principal(0xc2),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    assert_eq!(
        machine.observe(&blocked).err(),
        Some(Reject::UnresolvedFork),
        "ordinary governance fails closed while forked"
    );

    // 4. W-1 recovery signatures over the wire do not resolve.
    let resolve_short =
        resolve_verified(&forked, heads[0], heads.to_vec(), &[&key(RECOV_R_SEED)], 1);
    assert_eq!(
        machine.observe(&resolve_short).err(),
        Some(Reject::InsufficientAuthorization)
    );

    // 5. W recovery signatures over the wire resolve the fork.
    let resolve_ok = resolve_verified(
        &forked,
        heads[0],
        heads.to_vec(),
        &[&key(RECOV_R_SEED), &key(RECOV_S_SEED)],
        5,
    );
    machine.observe(&resolve_ok).expect("W resolves");

    // 6. After resolution, a subsequent ordinary entry validates — the
    //    resolved incident is retained in the audit chain.
    assert!(machine.forked().is_none());
    let accepted = machine.accepted().expect("linear after resolve");
    let next = grant_verified(
        accepted,
        principal(0xd0),
        &key(ADMIN_A_SEED),
        &[&key(ADMIN_B_SEED)],
    );
    machine
        .observe(&next)
        .expect("post-resolution ordinary entry validates");
    assert!(
        machine
            .audit()
            .iter()
            .any(|r| matches!(r.status, GovernanceForkAuditStatus::Resolved { .. })),
        "the resolved incident is retained in the audit chain"
    );
}

// ============================================================================
// Small private assertion helpers (audit evidence reverification over wire CSBs).
// ============================================================================

/// Re-verify a retained branch/resolution entry signature over its exact CSB.
fn assert_reverifies(ev: &AuthenticatedGovernanceEvidence) {
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, ev.csb());
    verify(&ev.signer(), &msg, ev.signature())
        .expect("retained entry signature verifies over its exact CSB");
}

/// Re-verify a retained approval signature over its exact CSB.
fn assert_approval_reverifies(approval: &VerifiedGovernanceApprovalEvidence) {
    let msg = domain::signing_message(domain::GOVERNANCE_APPROVAL, approval.csb());
    verify(&approval.body().approver, &msg, approval.signature())
        .expect("retained approval signature verifies over its exact CSB");
}

// ----------------------------------------------------------------------------
// A second forked-machine builder that does not return the consumed verified
// entries (used by tests that only need the heads + forked state). Keeping the
// 4-tuple builder above for the audit test that needs the entries.
// ----------------------------------------------------------------------------

fn forked_machine_w2_over_wire_no_entries() -> (GovernanceMachine, [GovernanceId; 2]) {
    let (machine, heads, _a, _b) = forked_machine_w2_over_wire();
    (machine, heads)
}
