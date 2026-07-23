//! End-to-end trust-boundary lifecycle coverage for the v2 crypto core (issue
//! #140 acceptance: the full record lifecycle that the per-module unit tests each
//! exercise in isolation).
//!
//! The v2 core is pure (no network/store/async), so its meaningful "system
//! boundary" is the single trust boundary described in `lib.rs` / spec §4 D2: a
//! logical body → canonical signed bytes (CSB) → domain-separated signature →
//! id derivation → envelope → deterministic fold → state root → member Merkle
//! projection → signed checkpoint → authorization of content. These tests stitch
//! that whole pipeline together and assert the cross-cutting guarantees the
//! module tests leave open:
//!
//! - **sync/offline convergence**: two peers that fold the *same* record set in
//!   *different* arrival orders reach byte-identical roots, and a checkpoint one
//!   peer signs validates against the other peer's independently-folded state
//!   (spec §4 D4 / §11 reliability).
//! - **auth lifecycle**: a principal's write privilege tracks the folded
//!   membership projection — an added member may author content, a removed one
//!   may not, a stranger never could (spec §4 D5).
//! - **persistence/commitment boundary**: a signed checkpoint's state root,
//!   member root, snapshot hash, and unresolved-fork commitment all recompute
//!   from the folded state and reject on any tamper (spec §4 / §11 / #150).
//! - **fork lifecycle**: a same-author equivocation fails the author's
//!   authorization closed, is hash-visible in the checkpoint, and unblocks only
//!   through an authorized `fork.resolve` (spec §4 D6 / #149).
//! - **tamper detection**: each trust-boundary layer (CSB, id, signature,
//!   signer) rejects a forged envelope with its typed code (spec §4 D2 step 5).
//!
//! Everything is deterministic and seed-derived (spec §11: vectors use non-secret
//! seeds only); no entropy, network, or store is touched.

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::CborValue;
use iroh_rooms_v2_core::content::{validate_body, ContentEventBody, ContentKind};
use iroh_rooms_v2_core::governance::{
    authorize_content_body, authorize_governance_entry, entry_id, validate_against_state,
    CheckpointBody, ForkResolutionBody, ForkResolveAction, GovernanceAction, GovernanceEntryBody,
    GovernanceFold, Role, SignedCheckpoint, SCHEMA_VERSION,
};
use iroh_rooms_v2_core::ids::{DeviceId, GovernanceEntryId, MemberId, RoomId, LEN};
use iroh_rooms_v2_core::keys::SigningKey;
use iroh_rooms_v2_core::member::project;
use iroh_rooms_v2_core::signed::{self, Envelope};
use iroh_rooms_v2_core::Reject;

const ROOM_BYTES: [u8; LEN] = [0x70; LEN];

fn room() -> RoomId {
    RoomId::from_bytes(ROOM_BYTES)
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; LEN])
}

/// A genesis `InitRoom` entry authored+administered by `admin`.
fn genesis(admin: &SigningKey) -> GovernanceEntryBody {
    GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq: 1,
        parent: None,
        epoch: 1_000,
        action: GovernanceAction::InitRoom {
            admin: admin.member_id(),
            admin_device: admin.device_id(),
            room_name: "e2e-room".to_owned(),
        },
    }
}

/// An `AddMember` entry authored by `admin`, extending `parent` at `seq`.
fn add_member(
    admin: &SigningKey,
    seq: u64,
    parent: GovernanceEntryId,
    member: MemberId,
    device: DeviceId,
    role: Role,
) -> GovernanceEntryBody {
    GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq,
        parent: Some(parent),
        epoch: 1_000 + seq,
        action: GovernanceAction::AddMember {
            member,
            device,
            role,
        },
    }
}

/// A `RemoveMember` entry authored by `admin`, extending `parent` at `seq`.
fn remove_member(
    admin: &SigningKey,
    seq: u64,
    parent: GovernanceEntryId,
    member: MemberId,
) -> GovernanceEntryBody {
    GovernanceEntryBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: admin.member_id(),
        seq,
        parent: Some(parent),
        epoch: 1_000 + seq,
        action: GovernanceAction::RemoveMember { member },
    }
}

/// Seal a checkpoint committing to a folded outcome, carrying the outcome's
/// actual unresolved fork evidence (spec #150 / §11).
fn checkpoint_for(
    outcome: &iroh_rooms_v2_core::governance::FoldOutcome,
    admin: &SigningKey,
    seq: u64,
) -> SignedCheckpoint {
    let unresolved: Vec<[GovernanceEntryId; 2]> = outcome
        .state
        .forks
        .iter()
        .filter(|f| !f.resolved)
        .map(|f| f.conflicting)
        .collect();
    let body = CheckpointBody {
        schema_version: SCHEMA_VERSION,
        room_id: outcome.room_id,
        state_root: outcome.state_root,
        member_root: outcome.member_root,
        governance_tip: None,
        unresolved_forks: unresolved,
        epoch: 2_000 + seq,
        seq,
    };
    signed::seal(&body, admin)
}

// ===========================================================================
// Full room lifecycle: build → seal → verify → fold → project → checkpoint,
// proven convergent across two independently-ordered folds (sync determinism).
// ===========================================================================

#[test]
fn full_room_lifecycle_converges_across_shuffled_arrival_order() {
    let admin = key(0xa0);
    let x = key(0xb0); // becomes an active member
    let y = key(0xb1); // added then removed

    let g = genesis(&admin);
    let gid = entry_id(&g);
    let add_x = add_member(&admin, 2, gid, x.member_id(), x.device_id(), Role::Member);
    let xid = entry_id(&add_x);
    let add_y = add_member(&admin, 3, xid, y.member_id(), y.device_id(), Role::Member);
    let yid = entry_id(&add_y);
    let rem_y = remove_member(&admin, 4, yid, y.member_id());

    // --- Peer A: records arrive in chain order. ---
    // Each envelope is sealed AND end-to-end verified (CSB → id → signature →
    // body) before being trusted — the full D2 trust boundary.
    let sealed_a: Vec<Envelope<GovernanceEntryId>> =
        [g.clone(), add_x.clone(), add_y.clone(), rem_y.clone()]
            .into_iter()
            .map(|b| {
                let env = signed::seal(&b, &admin);
                // Verify exact received bytes (never a re-serialization).
                signed::verify_envelope::<GovernanceEntryBody>(&env).unwrap();
                env
            })
            .collect();
    let outcome_a = GovernanceFold::new()
        .entries_from([g.clone(), add_x.clone(), add_y.clone(), rem_y.clone()])
        .finish()
        .unwrap();

    // --- Peer B: the SAME set, delivered out of order (remove before genesis). ---
    let outcome_b = GovernanceFold::new()
        .entries_from([rem_y.clone(), add_y.clone(), add_x.clone(), g.clone()])
        .finish()
        .unwrap();

    // Convergence: byte-identical roots regardless of arrival order (D4).
    assert_eq!(
        outcome_a.state_root, outcome_b.state_root,
        "state root must be arrival-order independent"
    );
    assert_eq!(
        outcome_a.member_root, outcome_b.member_root,
        "member root must be arrival-order independent"
    );
    // Sanity: 4 sealed, 4 accepted, no unresolved forks in a clean chain.
    assert_eq!(sealed_a.len(), 4);
    assert!(outcome_a.unresolved_forks.is_empty());

    // Membership projection reflects the accepted state, not insertion order.
    assert!(outcome_a.state.is_active(&admin.member_id()));
    assert!(outcome_a.state.is_active(&x.member_id()));
    assert!(
        !outcome_a.state.is_active(&y.member_id()),
        "removed member is inactive"
    );

    // --- Persistence/commitment boundary (spec #150 / §11). ---
    // A checkpoint peer A signs must validate against peer B's independently
    // folded state — recomputing state root, member root, snapshot hash, and the
    // unresolved-fork commitment.
    let cp = checkpoint_for(&outcome_a, &admin, 1);
    let decoded = validate_against_state(&cp, &outcome_b.state).expect("checkpoint validates");
    assert_eq!(decoded.state_root, outcome_b.state_root);
    assert_eq!(decoded.member_root, outcome_b.member_root);
    assert!(decoded.unresolved_forks.is_empty());

    // --- Member Merkle projection: inclusion/exclusion proofs verify without
    // access to the full state (spec §6.5 / D7). ---
    let (_root, proj) = project(&outcome_a.state);
    assert_eq!(proj.root, outcome_a.member_root);
    // X is present.
    let inc_x = proj
        .map
        .prove_inclusion(x.member_id().as_bytes())
        .expect("x is a projected member");
    inc_x
        .verify(&proj.root, true)
        .expect("inclusion proof verifies against root");
    // Y was removed → absent from the projection.
    let exc_y = proj.map.prove_exclusion(y.member_id().as_bytes());
    assert!(exc_y.leaf.is_none(), "removed member has no leaf");
    exc_y
        .verify(&proj.root, false)
        .expect("exclusion proof verifies against root");
}

// ===========================================================================
// Auth lifecycle: a principal's write privilege tracks the folded membership.
// ===========================================================================

#[test]
fn content_authorization_tracks_membership_lifecycle() {
    let admin = key(0xa0);
    let member_x = key(0xb0);
    let member_y = key(0xb1);

    let g = genesis(&admin);
    let gid = entry_id(&g);
    let add_x = add_member(
        &admin,
        2,
        gid,
        member_x.member_id(),
        member_x.device_id(),
        Role::Member,
    );
    let xid = entry_id(&add_x);
    let add_y = add_member(
        &admin,
        3,
        xid,
        member_y.member_id(),
        member_y.device_id(),
        Role::Member,
    );
    let yid = entry_id(&add_y);
    let rem_y = remove_member(&admin, 4, yid, member_y.member_id());

    let outcome = GovernanceFold::new()
        .entries_from([g, add_x, add_y, rem_y])
        .finish()
        .unwrap();
    let state = &outcome.state;

    // An active member may sign and have a content event accepted (full D2
    // verify + body validation + authorization against the folded state).
    let text = ContentEventBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        author: member_x.member_id(),
        kind: ContentKind::MessageText,
        version: 1,
        stream_id: None,
        body: CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]),
    };
    let env_x = signed::seal(&text, &member_x);
    let decoded_x = iroh_rooms_v2_core::content::body::decode_verified(&env_x).unwrap();
    validate_body(&decoded_x).unwrap();
    authorize_content_body(state, &member_x.member_id()).expect("active member is authorized");

    // A removed member is no longer authorized, even with a well-formed event.
    let env_y = signed::seal(
        &ContentEventBody {
            schema_version: SCHEMA_VERSION,
            room_id: room(),
            author: member_y.member_id(),
            kind: ContentKind::MessageText,
            version: 1,
            stream_id: None,
            body: CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]),
        },
        &member_y,
    );
    iroh_rooms_v2_core::content::body::decode_verified(&env_y).unwrap();
    assert_eq!(
        authorize_content_body(state, &member_y.member_id()).err(),
        Some(Reject::InsufficientAuthorization),
        "removed member must not be authorized"
    );

    // A stranger (never added) is rejected identically.
    let stranger = key(0xee);
    assert_eq!(
        authorize_content_body(state, &stranger.member_id()).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

// ===========================================================================
// Fork lifecycle: equivocation fails closed, is hash-visible, and unblocks only
// via an authorized fork.resolve (spec §4 D6 / #149).
// ===========================================================================

#[test]
fn fork_fails_closed_then_unblocks_through_resolution() {
    let admin = key(0xa0);
    let g = genesis(&admin);
    let gid = entry_id(&g);

    // Two admin entries at the same seq/parent → an equivocation (fork).
    let branch_a = add_member(
        &admin,
        2,
        gid,
        MemberId::from_bytes([0x01; LEN]),
        DeviceId::from_bytes([0; LEN]),
        Role::Member,
    );
    let branch_b = add_member(
        &admin,
        2,
        gid,
        MemberId::from_bytes([0x02; LEN]),
        DeviceId::from_bytes([0; LEN]),
        Role::Member,
    );

    // Unresolved: the fork is detected and the admin is fail-closed.
    let unresolved = GovernanceFold::new()
        .entries_from([g.clone(), branch_a.clone(), branch_b.clone()])
        .finish()
        .unwrap();
    assert!(
        unresolved.state.has_unresolved_fork(&admin.member_id()),
        "equivocation must be detected"
    );
    // The fork is hash-visible in a checkpoint (OQ-4): the unresolved-fork
    // commitment carried by a signed checkpoint matches the state exactly.
    let cp_unresolved = checkpoint_for(&unresolved, &admin, 1);
    let decoded_unresolved =
        validate_against_state(&cp_unresolved, &unresolved.state).expect("checkpoint validates");
    assert_eq!(
        decoded_unresolved.unresolved_forks.len(),
        1,
        "the unresolved fork must be committed to the checkpoint"
    );
    // Authorization of further admin action fails closed while unresolved.
    let next = add_member(
        &admin,
        3,
        entry_id(&branch_a),
        MemberId::from_bytes([0x03; LEN]),
        DeviceId::from_bytes([0; LEN]),
        Role::Member,
    );
    assert_eq!(
        authorize_governance_entry(&unresolved.state, &next, &[]).err(),
        Some(Reject::UnresolvedFork)
    );

    // Resolve the fork: the evidence pair is the two conflicting ids, ascending.
    let mut evidence = [entry_id(&branch_a), entry_id(&branch_b)];
    evidence.sort();
    let resolution = ForkResolutionBody {
        schema_version: SCHEMA_VERSION,
        room_id: room(),
        signer: admin.member_id(),
        evidence,
        action: ForkResolveAction::Reject,
        epoch: 9_000,
    };
    let resolved_env = signed::seal(&resolution, &admin);
    iroh_rooms_v2_core::governance::decode_fork_resolution(&resolved_env).unwrap();

    let resolved = GovernanceFold::new()
        .entries_from([g, branch_a, branch_b])
        .resolution(resolution)
        .finish()
        .unwrap();
    assert!(
        !resolved.state.has_unresolved_fork(&admin.member_id()),
        "an authorized fork.resolve must clear the evidence"
    );

    // After resolution the checkpoint commits to NO unresolved forks, and the
    // state root has moved (the resolved state differs from the unresolved one).
    let cp_resolved = checkpoint_for(&resolved, &admin, 2);
    let decoded_resolved =
        validate_against_state(&cp_resolved, &resolved.state).expect("checkpoint validates");
    assert!(
        decoded_resolved.unresolved_forks.is_empty(),
        "no unresolved fork remains after resolution"
    );
    assert_ne!(
        unresolved.state_root, resolved.state_root,
        "resolution must change the committed state root"
    );
}

// ===========================================================================
// Tamper detection: each trust-boundary layer rejects a forged envelope with its
// typed code (spec §4 D2 step 5).
// ===========================================================================

#[test]
fn forged_envelope_rejected_at_each_trust_boundary() {
    let signer = key(0x11);
    let body = genesis(&signer);

    // A valid envelope verifies end-to-end.
    let env = signed::seal(&body, &signer);
    signed::verify_envelope::<GovernanceEntryBody>(&env).unwrap();

    // (1) CSB tampered → not canonical CBOR (trailing byte).
    let mut bad_csb = env.clone();
    bad_csb.signed.push(0x00);
    assert_eq!(
        signed::verify_envelope::<GovernanceEntryBody>(&bad_csb).err(),
        Some(Reject::NonCanonicalEncoding)
    );

    // (2) Envelope id swapped → does not match BLAKE3(ID_CONTEXT || CSB).
    let mut bad_id = env.clone();
    bad_id.id = GovernanceEntryId::from_bytes([0xff; LEN]);
    assert_eq!(
        signed::verify_envelope::<GovernanceEntryBody>(&bad_id).err(),
        Some(Reject::IdMismatch)
    );

    // (3) Signature re-forged under a different key, original signer retained.
    let mut bad_sig = env.clone();
    let forger = key(0x22);
    bad_sig.sig = signed::sign_csb::<GovernanceEntryBody>(&env.signed, &forger);
    assert_eq!(
        signed::verify_envelope::<GovernanceEntryBody>(&bad_sig).err(),
        Some(Reject::BadSignature)
    );

    // (4) Signer field swapped to a foreign principal (signature no longer
    // verifies under the claimed principal).
    let mut bad_signer = env.clone();
    bad_signer.signer = forger.member_id();
    assert_eq!(
        signed::verify_envelope::<GovernanceEntryBody>(&bad_signer).err(),
        Some(Reject::BadSignature)
    );
}
