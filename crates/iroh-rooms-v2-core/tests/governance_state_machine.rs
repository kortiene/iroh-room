//! Focused audit coverage for the governance state machine's authorization and
//! fork-handling paths (issue #140 acceptance: "independent audit of the
//! governance state machine and fork handling"; spec §7.4 / D5 / D6).
//!
//! These exercise gaps the in-module unit tests leave open:
//!
//! - the `ApprovalPolicy::MOfN` quorum branch of the authorization engine
//!   (`authz`), which the module tests never reach (they only cover
//!   `AdminAlone`);
//! - the `fork.resolve` `Reject` / `Supersede` actions and evidence-length
//!   validation (the module tests only cover `Accept`);
//! - a `fork.resolve` clearing unresolved fork evidence through the deterministic
//!   fold so the author's authorization unblocks (spec D6).
//!
//! All coverage is deterministic, seed-derived, and non-e2e.

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::governance::{
    authorize_governance_entry, decode_fork_resolution, detect_fork, entry_id, ApprovalPolicy,
    ForkResolutionBody, ForkResolveAction, GovernanceAction, GovernanceEntryBody, GovernanceFold,
    GovernanceState, MemberRecord, MemberStatus, Role, SCHEMA_VERSION,
};
use iroh_rooms_v2_core::ids::{DeviceId, GovernanceEntryId, MemberId, RoomId, LEN};
use iroh_rooms_v2_core::keys::SigningKey;
use iroh_rooms_v2_core::signed;
use iroh_rooms_v2_core::Reject;

const ROOM: [u8; LEN] = [0x70; LEN];

fn room() -> RoomId {
    RoomId::from_bytes(ROOM)
}

fn member(seed: u8) -> MemberId {
    MemberId::from_bytes([seed; LEN])
}

fn admin_record(id: MemberId) -> MemberRecord {
    MemberRecord {
        member_id: id,
        role: Role::Admin,
        status: MemberStatus::Active,
        devices: vec![DeviceId::from_bytes([0; LEN])],
        governance_cursor: GovernanceEntryId::from_bytes([0; LEN]),
    }
}

fn add_member_entry(author: MemberId) -> GovernanceEntryBody {
    GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author,
        seq: 2,
        parent: Some(GovernanceEntryId::from_bytes([0; LEN])),
        epoch: 10,
        action: GovernanceAction::AddMember {
            member: member(0xc0),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    }
}

/// A state under an `m_of_n` quorum policy with a distinct admin and approver set.
fn m_of_n_state(m: u64, approvers: Vec<MemberId>) -> GovernanceState {
    let admin = member(0xa0);
    let mut state = GovernanceState::empty(room());
    state.admin = Some(admin);
    state.members.insert(admin, admin_record(admin));
    state.policy = ApprovalPolicy::MOfN { m, approvers };
    state
}

// ---------------------------------------------------------------------------
// ApprovalPolicy::MOfN authorization (authz.rs quorum branch — untested)
// ---------------------------------------------------------------------------

#[test]
fn m_of_n_signer_in_set_counts_toward_quorum() {
    // signer is one of two approvers; m == 1, so the signer alone satisfies it.
    let signer = member(0xb1);
    let state = m_of_n_state(1, vec![signer, member(0xb2)]);
    let entry = add_member_entry(signer);
    assert!(authorize_governance_entry(&state, &entry, &[]).is_ok());
}

#[test]
fn m_of_n_insufficient_quorum_denied() {
    // m == 2 but the only contributing principal is the signer → 1 < 2.
    let signer = member(0xb1);
    let state = m_of_n_state(2, vec![signer, member(0xb2), member(0xb3)]);
    let entry = add_member_entry(signer);
    assert_eq!(
        authorize_governance_entry(&state, &entry, &[]).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

#[test]
fn m_of_n_external_approvals_reach_quorum() {
    // signer not in the approver set; two external approvals from listed
    // approvers reach m == 2.
    let signer = member(0xf0);
    let ap1 = member(0xb1);
    let ap2 = member(0xb2);
    let state = m_of_n_state(2, vec![ap1, ap2]);
    let entry = add_member_entry(signer);
    let approvals = [approval_from(ap1), approval_from(ap2)];
    assert!(authorize_governance_entry(&state, &entry, &approvals).is_ok());
}

#[test]
fn m_of_n_approvals_from_non_approvers_do_not_count() {
    // Approvals come from principals that are NOT in the approver set → ignored.
    let signer = member(0xf0);
    let listed = member(0xb1);
    let state = m_of_n_state(1, vec![listed]);
    let entry = add_member_entry(signer);
    let outsider_approvals = [approval_from(member(0x01)), approval_from(member(0x02))];
    assert_eq!(
        authorize_governance_entry(&state, &entry, &outsider_approvals).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

#[test]
fn m_of_n_duplicate_approvals_from_same_approver_count_once() {
    // Two approval records from the same listed approver must not double-count:
    // the inner loop `break`s after the first match. m == 2 stays unmet.
    let signer = member(0xf0);
    let ap1 = member(0xb1);
    let state = m_of_n_state(2, vec![ap1, member(0xb2)]);
    let entry = add_member_entry(signer);
    let dupes = [approval_from(ap1), approval_from(ap1)];
    assert_eq!(
        authorize_governance_entry(&state, &entry, &dupes).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

/// A minimal approval body from `approver` (only the `approver` field is
/// consulted by the quorum engine).
fn approval_from(approver: MemberId) -> iroh_rooms_v2_core::governance::ApprovalBody {
    iroh_rooms_v2_core::governance::ApprovalBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        entry_id: GovernanceEntryId::from_bytes([0; LEN]),
        approver,
        proposed_state_root: None,
        epoch: 0,
    }
}

// ---------------------------------------------------------------------------
// fork.resolve action coverage (Reject / Supersede / evidence length)
// ---------------------------------------------------------------------------

fn resolution_body(signer: MemberId, action: ForkResolveAction) -> ForkResolutionBody {
    let e0 = GovernanceEntryId::from_bytes([10; LEN]);
    let e1 = GovernanceEntryId::from_bytes([20; LEN]);
    ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer,
        evidence: [e0, e1],
        action,
        epoch: 5,
    }
}

#[test]
fn fork_resolution_reject_action_round_trips() {
    let key = SigningKey::from_seed(&[0x7b; LEN]);
    let body = resolution_body(key.member_id(), ForkResolveAction::Reject);
    let env = signed::seal(&body, &key);
    assert_eq!(decode_fork_resolution(&env).unwrap(), body);
}

#[test]
fn fork_resolution_accept_action_round_trips() {
    let key = SigningKey::from_seed(&[0x7c; LEN]);
    let winner = GovernanceEntryId::from_bytes([10; LEN]);
    let body = resolution_body(key.member_id(), ForkResolveAction::Accept { winner });
    let env = signed::seal(&body, &key);
    let decoded = decode_fork_resolution(&env).unwrap();
    assert_eq!(decoded, body);
    match decoded.action {
        ForkResolveAction::Accept { winner: w } => assert_eq!(w, winner),
        ForkResolveAction::Reject => panic!("expected accept"),
    }
}

#[test]
fn fork_resolution_with_wrong_evidence_count_rejected() {
    // Hand-build a CBOR body whose `evidence` array has one element, not two,
    // then seal + verify: decoding must reject with InvalidForkResolution.
    use iroh_rooms_v2_core::cbor::CborValue;
    let key = SigningKey::from_seed(&[0x7d; LEN]);
    let bad = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(SCHEMA_VERSION)),
        ("room_id".to_owned(), CborValue::Bytes(ROOM.to_vec())),
        (
            "signer".to_owned(),
            CborValue::Bytes(key.member_id().as_bytes().to_vec()),
        ),
        (
            "evidence".to_owned(),
            CborValue::Array(vec![CborValue::Bytes([1u8; LEN].to_vec())]),
        ),
        ("epoch".to_owned(), CborValue::Uint(1)),
        (
            "action".to_owned(),
            CborValue::Map(vec![(
                "type".to_owned(),
                CborValue::Text("reject".to_owned()),
            )]),
        ),
    ]);
    let csb = iroh_rooms_v2_core::cbor::encode(&bad);
    let sig = key.sign(&iroh_rooms_v2_core::domain::signing_message(
        iroh_rooms_v2_core::domain::FORK_RESOLVE_SIGN,
        &csb,
    ));
    let id = iroh_rooms_v2_core::ids::SnapshotHash::from_bytes(
        iroh_rooms_v2_core::domain::blake3_domain(
            iroh_rooms_v2_core::domain::FORK_RESOLVE_ID,
            &csb,
        ),
    );
    let env = iroh_rooms_v2_core::signed::Envelope {
        id,
        signed: csb,
        sig,
        signer: key.member_id(),
    };
    assert_eq!(
        decode_fork_resolution(&env).err(),
        Some(Reject::InvalidForkResolution)
    );
}

// ---------------------------------------------------------------------------
// Fork resolution through the fold clears unresolved evidence (spec D6)
// ---------------------------------------------------------------------------

#[test]
fn resolution_clears_matching_fork_evidence_in_fold() {
    let admin = SigningKey::from_seed(&[0xa0; LEN]);
    let g = GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 1,
        parent: None,
        epoch: 1_000,
        action: GovernanceAction::InitRoom {
            admin: admin.member_id(),
            admin_device: admin.device_id(),
            room_name: "r".to_owned(),
        },
    };
    let gid = entry_id(&g);
    // Two admin entries at the same seq/parent → fork.
    let mk_add = |member_seed: u8| GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 1_001,
        action: GovernanceAction::AddMember {
            member: member(member_seed),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let a = mk_add(0x01);
    let b = mk_add(0x02);

    // Without a resolution, the author has an unresolved fork.
    let unresolved = GovernanceFold::new()
        .entry(g.clone())
        .entry(a.clone())
        .entry(b.clone())
        .finish()
        .unwrap();
    assert!(
        unresolved.state.has_unresolved_fork(&admin.member_id()),
        "a same-seq fork must leave unresolved evidence"
    );

    // The evidence pair is the two conflicting entry ids in ascending order.
    let mut pair = [entry_id(&a), entry_id(&b)];
    pair.sort();
    let resolution = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence: pair,
        action: ForkResolveAction::Reject,
        epoch: 2,
    };

    let resolved = GovernanceFold::new()
        .entry(g)
        .entry(a)
        .entry(b)
        .resolution(resolution)
        .finish()
        .unwrap();
    assert!(
        !resolved.state.has_unresolved_fork(&admin.member_id()),
        "a matching fork.resolve must clear the unresolved evidence (spec D6)"
    );
    assert!(
        resolved.state.forks.iter().all(|f| f.resolved),
        "all fork evidence must be marked resolved"
    );
}

#[test]
fn resolution_with_nonmatching_evidence_leaves_fork_unresolved() {
    let admin = SigningKey::from_seed(&[0xa0; LEN]);
    let g = GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 1,
        parent: None,
        epoch: 1_000,
        action: GovernanceAction::InitRoom {
            admin: admin.member_id(),
            admin_device: admin.device_id(),
            room_name: "r".to_owned(),
        },
    };
    let gid = entry_id(&g);
    let mk_add = |member_seed: u8| GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 1_001,
        action: GovernanceAction::AddMember {
            member: member(member_seed),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let a = mk_add(0x01);
    let b = mk_add(0x02);
    // A resolution for an unrelated fork must NOT clear this author's evidence.
    let unrelated = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence: [
            GovernanceEntryId::from_bytes([0xdd; LEN]),
            GovernanceEntryId::from_bytes([0xee; LEN]),
        ],
        action: ForkResolveAction::Reject,
        epoch: 2,
    };
    let outcome = GovernanceFold::new()
        .entry(g)
        .entry(a)
        .entry(b)
        .resolution(unrelated)
        .finish()
        .unwrap();
    assert!(
        outcome.state.has_unresolved_fork(&admin.member_id()),
        "a resolution for different evidence must leave the fork unresolved"
    );
}

// ---------------------------------------------------------------------------
// Blocker regressions: authorized-only resolution, winner-selection state
// effect, and unknown-key rejection (spec D6 / D8 / §11).
// ---------------------------------------------------------------------------

/// Build a genesis + a same-seq/parent fork (branches A and B) authored by the
/// admin. Returns `(admin_key, genesis, branch_a, branch_b)`.
fn forked_setup() -> (
    SigningKey,
    GovernanceEntryBody,
    GovernanceEntryBody,
    GovernanceEntryBody,
) {
    let admin = SigningKey::from_seed(&[0xa0; LEN]);
    let g = GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 1,
        parent: None,
        epoch: 1_000,
        action: GovernanceAction::InitRoom {
            admin: admin.member_id(),
            admin_device: admin.device_id(),
            room_name: "r".to_owned(),
        },
    };
    let gid = entry_id(&g);
    let mk_add = |seed: u8| GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 1_001,
        action: GovernanceAction::AddMember {
            member: member(seed),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let a = mk_add(0x01);
    let b = mk_add(0x02);
    (admin, g, a, b)
}

#[test]
fn resolution_by_non_admin_does_not_clear_fork() {
    // Blocker 1: a resolution authored by someone other than the admin must NOT
    // clear the fork or unblock authorization. Previously any resolution (even
    // one built by nobody) cleared matching evidence.
    let (admin, g, a, b) = forked_setup();
    let mut pair = [entry_id(&a), entry_id(&b)];
    pair.sort();
    let intruder = SigningKey::from_seed(&[0xee; LEN]);
    let forged = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: intruder.member_id(),
        evidence: pair,
        action: ForkResolveAction::Reject,
        epoch: 2,
    };
    let outcome = GovernanceFold::new()
        .entry(g)
        .entry(a)
        .entry(b)
        .resolution(forged)
        .finish()
        .unwrap();
    assert!(
        outcome.state.has_unresolved_fork(&admin.member_id()),
        "an unauthorized resolution must NOT clear the fork (spec D6)"
    );
}

#[test]
fn accept_resolution_keeps_winner_and_drops_loser() {
    // Blocker 2: Accept{winner} must have a state effect — the winner's member is
    // present, the loser's is not — distinguishing it from Reject (drop both).
    let (admin, g, a, b) = forked_setup();
    let mut pair = [entry_id(&a), entry_id(&b)];
    pair.sort();
    let winner_id = entry_id(&a);
    let resolution = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence: pair,
        action: ForkResolveAction::Accept { winner: winner_id },
        epoch: 2,
    };
    let outcome = GovernanceFold::new()
        .entry(g)
        .entry(a)
        .entry(b)
        .resolution(resolution)
        .finish()
        .unwrap();
    assert!(
        !outcome.state.has_unresolved_fork(&admin.member_id()),
        "an authorized Accept resolves the fork"
    );
    // branch A (winner) added member 0x01; branch B (loser) added member 0x02.
    assert!(
        outcome.state.is_active(&member(0x01)),
        "the winning branch's membership mutation must be applied"
    );
    assert!(
        !outcome.state.is_active(&member(0x02)),
        "the losing branch's membership mutation must be rolled back"
    );
}

#[test]
fn accept_and_reject_produce_different_state() {
    // Blocker 2: 'accept A, drop B' and 'reject both' must NOT produce identical
    // state (the documented winner-selection semantics).
    let (admin, g, a, b) = forked_setup();
    let mut pair = [entry_id(&a), entry_id(&b)];
    pair.sort();
    let accept = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence: pair,
        action: ForkResolveAction::Accept {
            winner: entry_id(&a),
        },
        epoch: 2,
    };
    let reject = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence: pair,
        action: ForkResolveAction::Reject,
        epoch: 2,
    };
    let accepted = GovernanceFold::new()
        .entry(g.clone())
        .entry(a.clone())
        .entry(b.clone())
        .resolution(accept)
        .finish()
        .unwrap();
    let rejected = GovernanceFold::new()
        .entry(g)
        .entry(a)
        .entry(b)
        .resolution(reject)
        .finish()
        .unwrap();
    assert_ne!(
        accepted.state_root, rejected.state_root,
        "accept-winner and reject-both must diverge in state"
    );
    assert!(accepted.state.is_active(&member(0x01)));
    assert!(!rejected.state.is_active(&member(0x01)));
}

#[test]
fn governance_entry_with_unknown_top_level_key_rejected() {
    // Blocker 3: an injected unknown top-level key must be rejected, not ignored
    // (signature-malleability / parser-differential risk).
    use iroh_rooms_v2_core::cbor::CborValue;
    let admin = SigningKey::from_seed(&[0xa0; LEN]);
    let raw = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(SCHEMA_VERSION)),
        ("room_id".to_owned(), CborValue::Bytes(ROOM.to_vec())),
        (
            "author".to_owned(),
            CborValue::Bytes(admin.member_id().as_bytes().to_vec()),
        ),
        ("seq".to_owned(), CborValue::Uint(1)),
        ("epoch".to_owned(), CborValue::Uint(1_000)),
        ("kind".to_owned(), CborValue::Text("init_room".to_owned())),
        (
            "action".to_owned(),
            CborValue::Map(vec![
                (
                    "admin".to_owned(),
                    CborValue::Bytes(admin.member_id().as_bytes().to_vec()),
                ),
                (
                    "admin_device".to_owned(),
                    CborValue::Bytes(admin.device_id().as_bytes().to_vec()),
                ),
                ("room_name".to_owned(), CborValue::Text("r".to_owned())),
            ]),
        ),
        ("zz_unknown".to_owned(), CborValue::Uint(1)),
    ]);
    let csb = iroh_rooms_v2_core::cbor::encode(&raw);
    let sig = admin.sign(&iroh_rooms_v2_core::domain::signing_message(
        iroh_rooms_v2_core::domain::GOVERNANCE_ENTRY_SIGN,
        &csb,
    ));
    let id = GovernanceEntryId::from_bytes(iroh_rooms_v2_core::domain::blake3_domain(
        iroh_rooms_v2_core::domain::GOVERNANCE_ENTRY_ID,
        &csb,
    ));
    let env = iroh_rooms_v2_core::signed::Envelope {
        id,
        signed: csb,
        sig,
        signer: admin.member_id(),
    };
    assert_eq!(
        iroh_rooms_v2_core::governance::model::decode_verified(&env).err(),
        Some(Reject::NonCanonicalEncoding),
        "an injected unknown top-level key must be rejected"
    );
}

// ---------------------------------------------------------------------------
// Fork detection: seq mismatch and identical-id are not forks
// ---------------------------------------------------------------------------

#[test]
fn detect_ignores_identical_id_and_seq_mismatch() {
    let author = member(0x77);
    let id = GovernanceEntryId::from_bytes([1; LEN]);
    // Same id at the same seq is a replay, not a fork.
    assert!(detect_fork(author, id, 3, None, Some((3, id))).is_none());
    // Different id at a different seq is a normal chain position, not a fork.
    let other = GovernanceEntryId::from_bytes([2; LEN]);
    assert!(detect_fork(author, other, 5, None, Some((3, id))).is_none());
}
