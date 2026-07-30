//! End-to-end coverage for the #147 v2 governance-log lifecycle, and the #148
//! authorization boundary layered on top of it, across every trust boundary
//! the pure core exposes (issues #147/#148, specs
//! `v2-governance-log-entry-approval-state-root.md` §7.1–§7.3 / §12 and
//! `v2-governance-authorization-rules.md` §7.4).
//!
//! The in-module unit tests in `governance/log/*` exercise each piece
//! (`GenesisConfig`, each `apply_*`, `verify_entry_full`,
//! `validate_governance_entry`, `compute_state_root`) in isolation with
//! in-memory structs. `v2_identifiers_e2e.rs` covers the frozen §6.3
//! *identifier* derivations, and `governance_state_machine.rs` covers the
//! **candidate** (legacy `InitRoom`/`AddMember`) state machine. None of those
//! drives the **normative #147/#148 governance log** end to end through the
//! real CBOR ↔ BLAKE3-id ↔ Ed25519 ↔ authorization ↔ state-root boundaries,
//! starting from nothing but raw wire bytes as a public-API consumer would.
//!
//! This file closes that gap. It models the complete receiver-side lifecycle a
//! peer or store reconstruction would perform:
//!
//! 1. **Genesis bootstrap** — a `GenesisConfig` canonicalizes to CSB, derives a
//!    non-recursive `CommunityId` under `domain::COMMUNITY`, and verifies a
//!    real multi-admin Ed25519 threshold (met / not-met / duplicate-signer).
//! 2. **Multi-entry log fold from raw wire bytes** — a sequence of governance
//!    entries (each crossing several components) is *sealed by the sender* into
//!    `{csb, signer, sig, approvals}` and then *reconstructed + verified by the
//!    receiver* from those bytes: canonical decode → entry-signature verify →
//!    approval sort/dedup/signature/binding verify → chain-link check → pure
//!    apply → state-root recompute → declared-root compare.
//! 3. **Golden final state root** — the post-fold root is byte-pinned so a
//!    silent hash/domain/order/encoding change is caught (spec §12 golden
//!    vector).
//! 4. **Negative e2e contracts** — an unknown operation kind is rejected at the
//!    wire-decode boundary (not silently ignored, §7.3), and a declared-root
//!    mismatch is rejected through the full `apply_verified_entry` pipeline
//!    (§7.3), as is a foreign-community entry (§6.4 isolation).
//! 5. **Per-operation registry** — every one of the fifteen §7.3 operations
//!    folds from wire bytes through the full pipeline with a state-root-visible
//!    transition.
//! 6. **Authorization boundary (#148 §7.4)** — the same wire-bytes receiver
//!    pattern driven through `verify_governance_entry` +
//!    `validate_and_apply_governance_entry` instead of the non-authorizing
//!    `apply_verified_entry`: a multi-entry `ValidatedGovernanceState` chain,
//!    a cryptographically valid but under-threshold entry rejected only by the
//!    authorization boundary (not by crypto or root checks), and the D6
//!    admin-set invariant (old quorum authorizes; new quorum effective only
//!    post-commit) proven from wire bytes rather than in-process structs.
//! 7. **Verbatim-CSB trust boundary (#178)** — a normalizing `admin.set`
//!    (unsorted/duplicate `administrators` array) reconstructs from altered
//!    wire bytes that decode to the same typed body, and is verified against
//!    the *received* bytes: a signature valid only over the normalized bytes
//!    is rejected as `BadSignature` before authorization/state application,
//!    while an exact signature over the normalizing bytes is rejected
//!    post-signature as `NonCanonicalEncoding`. Positive folds prove the
//!    threaded next-link identity equals the exact wire-CSB-derived id.
//!
//! All keys are deterministic public test seeds (non-secret); no entropy,
//! network, store, or real user data is involved. The crate stays pure: these
//! tests pull in no `tokio`/`iroh` (the `banned_dependencies` test
//! machine-checks that).

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::{self, CborValue};
use iroh_rooms_v2_core::domain;
use iroh_rooms_v2_core::governance::log::{
    apply, apply_entry, apply_verified_entry, check_chain_link, compute_state_root,
    decode_entry_csb, derive_community_id, entry_csb, entry_id, entry_id_from_csb,
    genesis_config_csb, sign_genesis, validate_and_apply_governance_entry, validated_genesis_state,
    verify_entry_crypto, verify_entry_full, verify_genesis, verify_governance_entry, GenesisConfig,
    GovernanceApproval, GovernanceApprovalBody, GovernanceEntry, GovernanceEntryBody,
    GovernanceOperationKind, GovernanceOperationPayload, GovernanceState, GovernanceTip,
    MemberStatus, ValidatedGovernanceState, GENESIS_SCHEMA_VERSION,
};
use iroh_rooms_v2_core::governance::log::{
    AdminSet, CommunityPolicy, DeviceGrant, DeviceRevoke, ForkResolve, InviteRevoke, MemberGrant,
    MemberRevoke, MemberRoleSet, MigrationAccept, PolicySet, RecoveryConfig, RecoverySet,
    ReplicaDescriptor, ReplicaSet, ReplicaStatus, Role, StreamArchive, StreamCreate, StreamPolicy,
    StreamPolicySet,
};
use iroh_rooms_v2_core::ids::{
    CommunityId, DeviceId, GovernanceId, PrincipalId, ReplicaId, StateRoot, StreamId, LEN as N,
};
use iroh_rooms_v2_core::keys::{Signature, SigningKey, SIGNATURE_LEN};
use iroh_rooms_v2_core::Reject;

// ============================================================================
// Deterministic public test seeds (non-secret; mirrors golden/README.md table).
// ============================================================================

const ADMIN_A_SEED: u8 = 0xa0;
const ADMIN_B_SEED: u8 = 0xa1;
const ADMIN_C_SEED: u8 = 0xa2;
const APPROVER_SEED: u8 = 0xc0;
/// The member being granted/revoked through the log.
const MEMBER_SEED: u8 = 0xb0;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; N])
}

fn principal(seed: u8) -> PrincipalId {
    key(seed).member_id()
}

/// A 3-admin genesis config with threshold 2. Administrators are sorted
/// ascending (ed25519 public-key bytes are not seed-ordered, so the sort is
/// material) so the config validates and canonicalizes identically on both
/// sides of the wire.
fn genesis_config() -> GenesisConfig {
    let mut admins: Vec<PrincipalId> = [ADMIN_A_SEED, ADMIN_B_SEED, ADMIN_C_SEED]
        .into_iter()
        .map(principal)
        .collect();
    admins.sort();
    // Genesis bootstrap also seeds the replicas + recovery components so the
    // fold starts from a non-trivial state-root commitment.
    let replica = ReplicaDescriptor {
        replica_id: ReplicaId::from_bytes([0x11; N]),
        endpoint: vec![0xfe],
        capability: 2,
    };
    GenesisConfig {
        schema_version: GENESIS_SCHEMA_VERSION,
        created_at_ms: 1_000,
        genesis_nonce: [0xab; N],
        admin_threshold: 2,
        administrators: admins,
        recovery: RecoveryConfig {
            threshold: 1,
            recovery_keys: vec![principal(0xa3)],
        },
        replicas: vec![replica],
        community_policy: CommunityPolicy::empty(),
    }
}

// ============================================================================
// §1 Genesis bootstrap: CSB → CommunityId → multi-sig threshold verification.
// ============================================================================

#[test]
fn e2e_genesis_signs_and_verifies_under_threshold() {
    let cfg = genesis_config();
    // Threshold is 2; sign with two distinct admins.
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let cid = verify_genesis(&cfg, &sigs).expect("threshold met verifies");

    // The derived community id is the non-recursive BLAKE3 over the genesis CSB
    // under domain::COMMUNITY — recomputed straight from the preimage bytes,
    // independent of the verify path.
    let csb = genesis_config_csb(&cfg);
    let recomputed = CommunityId::from_bytes(domain::blake3_domain(domain::COMMUNITY, &csb));
    assert_eq!(
        cid, recomputed,
        "community id must match an independent recompute"
    );

    // The genesis preimage carries no community_id (non-recursive derivation,
    // spec D3) — proven straight from the wire CSB.
    let value = cbor::decode_canonical(&csb).unwrap();
    let entries = value.as_map().unwrap();
    assert!(
        !entries.iter().any(|(k, _)| k == "community_id"),
        "genesis CSB must not contain community_id (spec D3)"
    );
}

#[test]
fn e2e_genesis_below_threshold_rejected() {
    let cfg = genesis_config();
    // Only one admin signs; threshold is 2.
    let sigs = [sign_genesis(&cfg, &key(ADMIN_A_SEED))];
    assert_eq!(
        verify_genesis(&cfg, &sigs).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

#[test]
fn e2e_genesis_duplicate_admin_does_not_double_count() {
    let cfg = genesis_config();
    // The same admin signs twice — must reject (not reach threshold by
    // double-counting), per spec D6 / §9.
    let dup = sign_genesis(&cfg, &key(ADMIN_A_SEED));
    assert_eq!(
        verify_genesis(&cfg, &[dup.clone(), dup]).err(),
        Some(Reject::InvalidApproval)
    );
}

#[test]
fn e2e_genesis_threshold_round_trips_through_canonical_cbor() {
    // The genesis config survives a canonical-CBOR encode/decode round trip
    // without changing its derived community id (the bytes a peer would store).
    let cfg = genesis_config();
    let csb = genesis_config_csb(&cfg);
    let decoded = GenesisConfig::from_canonical(&cbor::decode_canonical(&csb).unwrap()).unwrap();
    assert_eq!(decoded, cfg);
    assert_eq!(derive_community_id(&decoded), derive_community_id(&cfg));
}

// ============================================================================
// §2 Multi-entry log fold reconstructed from raw wire bytes.
// ============================================================================

/// The raw, type-erased bytes a receiver pulls off the wire/storage for a
/// governance entry. Built independently of the in-memory body so the receiver
/// path is exercised honestly (mirrors `v2_identifiers_e2e.rs` `WireRecord`).
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
/// community id, entry id, and declared state root (spec §5.3 bindings).
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
/// then run the full verification + fold pipeline. Returns the new state and
/// the folded entry's exact-CSB id (the `prev` for the next link), or the
/// typed `Reject` so callers can attach boundary context.
fn receive_and_fold(
    old: &GovernanceState,
    wire: &WireEntry,
    expected_prev: Option<GovernanceId>,
    expected_seq: u64,
) -> Result<(GovernanceState, GovernanceId), Reject> {
    // Reassemble the wire record straight from the received bytes — the trust
    // boundary. `from_received_csb` canonical-decodes the typed body and
    // retains the supplied CSB byte-for-byte (issue #178).
    let entry = GovernanceEntry::from_received_csb(
        wire.csb.clone(),
        PrincipalId::from_bytes(wire.signer),
        Signature::from_bytes(wire.sig),
        wire.approvals.clone(),
    )?;
    // Independent decode of the wire CSB, to cross-check the verified body
    // against an honest receiver path (the construction decode and this
    // decode must agree).
    let wire_body = decode_entry_csb(&wire.csb)?;
    // Full pipeline: entry crypto over retained CSB + approval sort/dedup/
    // sig/binding verify. Use the verified wrapper so the next chain link
    // binds to the authenticated exact-CSB identity (issue #178).
    let verified = verify_governance_entry(&entry)?;
    let body = verified.body();
    assert_eq!(
        body, &wire_body,
        "verified body must equal the independently decoded wire body"
    );
    // Chain invariant (spec D5): seq/prev link (contiguous seq, review thread #2).
    check_chain_link(body, expected_prev, expected_seq)?;
    // Pure apply + declared-root recompute + compare (spec §7.3).
    let new = apply_verified_entry(old, body)?;
    Ok((new, verified.id()))
}

/// Sender + receiver for one entry: compute the declared root by applying the
/// payload to the previous state, seal the body into wire bytes (with a real
/// approval), then receive and fold. Centralized so the multi-entry tests stay
/// readable.
fn fold_one(
    old: &GovernanceState,
    payload: &GovernanceOperationPayload,
    seq: u64,
    prev: Option<GovernanceId>,
    author: &SigningKey,
    approver: &SigningKey,
    cid: CommunityId,
) -> (GovernanceState, GovernanceId) {
    // Sender computes the declared root by applying the op to the prior state.
    let applied = apply_entry(old, seq, payload).expect("payload applies to prior state");
    let declared = compute_state_root(&applied);
    let body = GovernanceEntryBody {
        community_id: cid,
        seq,
        prev,
        created_at_ms: 1_000 + seq,
        kind: payload.kind(),
        payload: payload.clone(),
        state_root: declared,
    };
    // Seal + attach an approval bound to the entry, then hand the raw bytes to
    // the receiver pipeline.
    let wire = with_approval(seal(&body, author), &body, approver);
    receive_and_fold(old, &wire, prev, seq).expect("fold must succeed for a well-formed entry")
}

/// The full #147 governance-log lifecycle: genesis → five entries crossing
/// three state components, each reconstructed from wire bytes and folded. This
/// is the central e2e contract — no single unit test spans genesis threshold,
/// multi-entry crypto verification, chain threading, and state-root
/// recomputation together.
#[test]
fn e2e_full_governance_log_fold_from_wire_bytes() {
    let cfg = genesis_config();
    // Bootstrap: verify threshold and derive the community id.
    let cid = verify_genesis(
        &cfg,
        &[
            sign_genesis(&cfg, &key(ADMIN_A_SEED)),
            sign_genesis(&cfg, &key(ADMIN_B_SEED)),
        ],
    )
    .expect("genesis threshold met");
    let mut state = GovernanceState::from_genesis(&cfg, cid);
    let author = key(ADMIN_A_SEED);
    let approver = key(APPROVER_SEED);
    let genesis_root = compute_state_root(&state);

    // Entry 1: member.grant — component 4 (members/devices/roles).
    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    let (next, prev1) = fold_one(&state, &payload, 1, None, &author, &approver, cid);
    assert!(next.members.contains_key(&principal(MEMBER_SEED)));
    state = next;

    // Entry 2: device.grant — component 4.
    let dev = DeviceId::from_bytes([0xd0; N]);
    let payload = GovernanceOperationPayload::DeviceGrant(DeviceGrant {
        member_id: principal(MEMBER_SEED),
        device_id: dev,
        binding: Vec::new(),
    });
    let (next, prev2) = fold_one(&state, &payload, 2, Some(prev1), &author, &approver, cid);
    assert!(next
        .members
        .get(&principal(MEMBER_SEED))
        .unwrap()
        .devices
        .contains_key(&dev));
    state = next;

    // Entry 3: stream.create — component 5 (stream manifest).
    let sid = StreamId::from_bytes([0x22; N]);
    let payload = GovernanceOperationPayload::StreamCreate(StreamCreate {
        stream_id: sid,
        policy: StreamPolicy::default_policy(),
        created_at_ms: 2_000,
    });
    let (next, prev3) = fold_one(&state, &payload, 3, Some(prev2), &author, &approver, cid);
    assert!(next.streams.contains_key(&sid));
    state = next;

    // Entry 4: invite.revoke — component 6 (community policy).
    let payload = GovernanceOperationPayload::InviteRevoke(InviteRevoke {
        invite_id: [0x33; N],
    });
    let (next, prev4) = fold_one(&state, &payload, 4, Some(prev3), &author, &approver, cid);
    assert!(next.policy.revoked_invites.contains(&[0x33; N]));
    state = next;

    // Entry 5: member.revoke — component 4, tombstoning the entry-1 member.
    let payload = GovernanceOperationPayload::MemberRevoke(MemberRevoke {
        member_id: principal(MEMBER_SEED),
    });
    let (next, _) = fold_one(&state, &payload, 5, Some(prev4), &author, &approver, cid);
    assert_eq!(
        next.members.get(&principal(MEMBER_SEED)).unwrap().status,
        MemberStatus::Revoked
    );

    // The folded log must commit to different state than genesis alone.
    assert_ne!(
        compute_state_root(&next),
        genesis_root,
        "the folded log must commit to different state than genesis alone"
    );
}

// ============================================================================
// §3 Golden final state root — byte-pinned regression anchor (spec §12).
// ============================================================================

/// Apply the same deterministic genesis→five-entry fold and pin the exact final
/// state root. A change to the hash algorithm, the `GOVERNANCE_STATE` domain,
/// the six-component order, or the canonical-CBOR encoding would otherwise slip
/// past the self-consistency checks (recompute == declared) while silently
/// changing every root on the wire.
#[test]
fn e2e_final_state_root_is_byte_pinned() {
    let cfg = genesis_config();
    let cid = verify_genesis(
        &cfg,
        &[
            sign_genesis(&cfg, &key(ADMIN_A_SEED)),
            sign_genesis(&cfg, &key(ADMIN_B_SEED)),
        ],
    )
    .unwrap();
    let mut state = GovernanceState::from_genesis(&cfg, cid);
    let author = key(ADMIN_A_SEED);
    let approver = key(APPROVER_SEED);

    let steps: [GovernanceOperationPayload; 5] = [
        GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(MEMBER_SEED),
            roles: vec![Role::Member],
            profile: None,
        }),
        GovernanceOperationPayload::DeviceGrant(DeviceGrant {
            member_id: principal(MEMBER_SEED),
            device_id: DeviceId::from_bytes([0xd0; N]),
            binding: Vec::new(),
        }),
        GovernanceOperationPayload::StreamCreate(StreamCreate {
            stream_id: StreamId::from_bytes([0x22; N]),
            policy: StreamPolicy::default_policy(),
            created_at_ms: 2_000,
        }),
        GovernanceOperationPayload::InviteRevoke(InviteRevoke {
            invite_id: [0x33; N],
        }),
        GovernanceOperationPayload::MemberRevoke(MemberRevoke {
            member_id: principal(MEMBER_SEED),
        }),
    ];
    let mut prev: Option<GovernanceId> = None;
    for (i, payload) in steps.iter().enumerate() {
        let seq = u64::try_from(i + 1).unwrap();
        let (next, entry_prev) = fold_one(&state, payload, seq, prev, &author, &approver, cid);
        state = next;
        prev = Some(entry_prev);
    }

    assert_eq!(
        compute_state_root(&state).as_bytes(),
        &[
            0x10, 0x3d, 0x5c, 0x8a, 0xc5, 0x1e, 0x75, 0x4f, 0xd7, 0x32, 0x01, 0x4c, 0xa6, 0xed,
            0xe1, 0x36, 0x24, 0x10, 0xf0, 0x43, 0xeb, 0x1b, 0xde, 0x4b, 0x2c, 0x1d, 0x55, 0x03,
            0xc9, 0x87, 0xa5, 0x72,
        ],
        "final state root drifted from the golden vector; recompute and update \
         this constant deliberately"
    );
}

// ============================================================================
// §4 Negative e2e: unknown operation rejected at the wire-decode boundary.
// ============================================================================

/// An entry whose wire `kind` is outside the closed §7.3 registry is rejected
/// when the receiver decodes the CSB — never silently ignored (spec §7.3). The
/// injected CSB is a *real signed wire record* (signed by a real Ed25519 key
/// over those exact bytes), so this proves the registry gate fires on received
/// bytes before any signature work could make the record look acceptable.
#[test]
fn e2e_unknown_operation_kind_rejected_at_decode_boundary() {
    let cfg = genesis_config();
    let cid = derive_community_id(&cfg);
    let state = GovernanceState::from_genesis(&cfg, cid);
    let author = key(ADMIN_A_SEED);

    // Build a well-formed entry body CSB, then inject a bogus `kind`.
    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    let applied = apply(&state, &payload).unwrap();
    let mut value = GovernanceEntryBody {
        community_id: cid,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: GovernanceOperationKind::MemberGrant,
        payload: payload.clone(),
        state_root: compute_state_root(&applied),
    }
    .to_cbor();
    if let CborValue::Map(ref mut entries) = value {
        for (k, v) in entries.iter_mut() {
            if k == "kind" {
                // A v1 candidate alias that must NOT validate as a v2 op.
                *v = CborValue::Text("add_member".to_owned());
            }
        }
    }
    let csb = cbor::encode(&value);

    // The sender legitimately signs these exact (bogus-kind) bytes — a peer
    // could receive a validly-signed record whose kind is not in the registry.
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &csb);
    let _sig = author.sign(&msg); // signing succeeds; rejection is structural

    // The receiver decodes the received CSB and must reject the unknown kind
    // before any state or signature-dependent work (§7.3: rejected, not
    // ignored). A parser that accepted-then-ignored would fail this assertion.
    assert_eq!(
        decode_entry_csb(&csb).err(),
        Some(Reject::UnknownRecordKind),
        "an unknown operation kind must be rejected at decode"
    );

    // The injected bytes are otherwise canonical CBOR, so the failure isolates
    // to the registry, not to malformed encoding.
    assert!(
        cbor::decode_canonical(&csb).is_ok(),
        "the injected record must be canonical CBOR; only the kind is unknown"
    );
}

// ============================================================================
// §5 Negative e2e: declared state-root mismatch rejected through full pipeline.
// ============================================================================

/// An entry whose declared `state_root` disagrees with the recomputed root is
/// rejected by `apply_verified_entry` — the §7.3 fail-closed check, exercised
/// through the receiver pipeline (not just a direct `verify_state_root` call).
#[test]
fn e2e_declared_state_root_mismatch_rejected_in_fold() {
    let cfg = genesis_config();
    let cid = derive_community_id(&cfg);
    let state = GovernanceState::from_genesis(&cfg, cid);
    let author = key(ADMIN_A_SEED);

    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    // A body signed over a deliberately-wrong declared root. The signature is
    // valid for THIS body (root included in the signed CSB), so the entry
    // verifies cryptically; only the declared-root check fails downstream.
    let wrong_root = StateRoot::from_bytes([0xff; N]);
    let body = GovernanceEntryBody {
        community_id: cid,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: payload.kind(),
        payload: payload.clone(),
        state_root: wrong_root,
    };
    let entry = GovernanceEntry::new(body, &author, Vec::new());
    let verified = verify_entry_full(&entry).expect("crypto verifies; only the root is wrong");
    assert_eq!(
        apply_verified_entry(&state, &verified).err(),
        Some(Reject::StateRootMismatch),
        "a declared-root mismatch must fail closed through the fold"
    );

    // A separately-signed entry carrying the correct declared root folds
    // successfully — proving the rejection above was the root check, not crypto.
    let correct_root = compute_state_root(&apply(&state, &payload).unwrap());
    let fixed_body = GovernanceEntryBody {
        community_id: cid,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: payload.kind(),
        payload,
        state_root: correct_root,
    };
    let fixed = GovernanceEntry::new(fixed_body, &author, Vec::new());
    let verified = verify_entry_full(&fixed).expect("crypto verifies");
    apply_verified_entry(&state, &verified).expect("matching declared root folds");
}

// ============================================================================
// §6 Negative e2e: foreign-community entry rejected by the fold.
// ============================================================================

/// An entry that decodes and verifies cryptographically but references a
/// different `community_id` than the state being folded is rejected by
/// `apply_verified_entry` (spec §6.4 isolation).
#[test]
fn e2e_foreign_community_entry_rejected_in_fold() {
    let cfg = genesis_config();
    let cid = derive_community_id(&cfg);
    let state = GovernanceState::from_genesis(&cfg, cid);
    let author = key(ADMIN_A_SEED);

    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    let correct_root = compute_state_root(&apply(&state, &payload).unwrap());
    let body = GovernanceEntryBody {
        community_id: CommunityId::from_bytes([0x99; N]), // foreign community
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: payload.kind(),
        payload,
        // Root is computed for THIS community's state, so it would not match;
        // the community-id guard must fire first regardless.
        state_root: correct_root,
    };
    let entry = GovernanceEntry::new(body, &author, Vec::new());
    let verified = verify_entry_full(&entry).expect("crypto verifies");
    assert_eq!(
        apply_verified_entry(&state, &verified).err(),
        Some(Reject::InvalidContent),
        "an entry for a foreign community must not fold into this state"
    );
}

// ============================================================================
// §7 Operation-registry e2e: every §7.3 op folds from wire bytes.
// ============================================================================

/// Every registered §7.3 operation, sealed into wire bytes by the sender and
/// reconstructed + folded by the receiver, produces a state-root-visible
/// transition. This is the acceptance item "each §7.3 operation has a pure
/// apply function", proven across the full wire↔verify↔apply↔root boundary
/// rather than via an in-memory `apply_*` call.
#[allow(clippy::too_many_lines)] // one case per §7.3 operation (15 total)
#[test]
fn e2e_every_registered_operation_folds_from_wire_bytes() {
    let cfg = genesis_config();
    let cid = derive_community_id(&cfg);
    let author = key(ADMIN_A_SEED);
    let approver = key(APPROVER_SEED);

    let setup = GovernanceState::from_genesis(&cfg, cid);
    let with_member = apply(
        &setup,
        &GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: principal(MEMBER_SEED),
            roles: vec![Role::Member],
            profile: None,
        }),
    )
    .unwrap();
    let with_device = apply(
        &with_member,
        &GovernanceOperationPayload::DeviceGrant(DeviceGrant {
            member_id: principal(MEMBER_SEED),
            device_id: DeviceId::from_bytes([0x45; N]),
            binding: Vec::new(),
        }),
    )
    .unwrap();
    let with_stream = apply(
        &setup,
        &GovernanceOperationPayload::StreamCreate(StreamCreate {
            stream_id: StreamId::from_bytes([0x44; N]),
            policy: StreamPolicy::default_policy(),
            created_at_ms: 3_000,
        }),
    )
    .unwrap();

    // A non-empty replacement community policy (setup's policy is empty).
    let mut nonempty_policy = CommunityPolicy::empty();
    nonempty_policy.migrations.insert([0x53; N]);

    // Each (label, payload, prior-state) triple; the prior state is chosen so
    // the transition is structurally valid AND materially changes the root.
    let cases: [(&str, GovernanceOperationPayload, GovernanceState); 15] = [
        (
            "member.grant",
            GovernanceOperationPayload::MemberGrant(MemberGrant {
                member_id: principal(0x31),
                roles: vec![Role::Member],
                profile: None,
            }),
            setup.clone(),
        ),
        (
            "member.revoke",
            GovernanceOperationPayload::MemberRevoke(MemberRevoke {
                member_id: principal(MEMBER_SEED),
            }),
            with_member.clone(),
        ),
        (
            "member.role_set",
            GovernanceOperationPayload::MemberRoleSet(MemberRoleSet {
                member_id: principal(MEMBER_SEED),
                roles: vec![Role::Agent, Role::Member],
            }),
            with_member.clone(),
        ),
        (
            "device.grant",
            GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                member_id: principal(MEMBER_SEED),
                device_id: DeviceId::from_bytes([0x46; N]),
                binding: Vec::new(),
            }),
            with_member.clone(),
        ),
        (
            "device.revoke",
            GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
                member_id: principal(MEMBER_SEED),
                device_id: DeviceId::from_bytes([0x45; N]),
            }),
            with_device.clone(),
        ),
        (
            "admin.set",
            GovernanceOperationPayload::AdminSet(AdminSet {
                administrators: cfg.administrators.clone(),
                threshold: 1, // setup threshold is 2 → material change
            }),
            setup.clone(),
        ),
        (
            "recovery.set",
            GovernanceOperationPayload::RecoverySet(RecoverySet {
                recovery: RecoveryConfig {
                    threshold: 2,
                    recovery_keys: vec![principal(0xa3), principal(0xa4)],
                },
            }),
            setup.clone(),
        ),
        (
            "replica.set",
            GovernanceOperationPayload::ReplicaSet(ReplicaSet {
                replica: ReplicaDescriptor {
                    replica_id: ReplicaId::from_bytes([0x50; N]),
                    endpoint: vec![0x01],
                    capability: 1,
                },
                status: ReplicaStatus::Active,
            }),
            setup.clone(),
        ),
        (
            "stream.create",
            GovernanceOperationPayload::StreamCreate(StreamCreate {
                stream_id: StreamId::from_bytes([0x51; N]),
                policy: StreamPolicy::default_policy(),
                created_at_ms: 4_000,
            }),
            setup.clone(),
        ),
        (
            "stream.policy_set",
            GovernanceOperationPayload::StreamPolicySet(StreamPolicySet {
                stream_id: StreamId::from_bytes([0x44; N]),
                policy: StreamPolicy { access: 5 },
            }),
            with_stream.clone(),
        ),
        (
            "stream.archive",
            GovernanceOperationPayload::StreamArchive(StreamArchive {
                stream_id: StreamId::from_bytes([0x44; N]),
                archived: true,
            }),
            with_stream.clone(),
        ),
        (
            "invite.revoke",
            GovernanceOperationPayload::InviteRevoke(InviteRevoke {
                invite_id: [0x52; N],
            }),
            setup.clone(),
        ),
        (
            "policy.set",
            GovernanceOperationPayload::PolicySet(PolicySet {
                policy: nonempty_policy,
            }),
            setup.clone(),
        ),
        (
            "fork.resolve",
            GovernanceOperationPayload::ForkResolve(ForkResolve {
                branch_heads: vec![
                    GovernanceId::from_bytes([0x60; N]),
                    GovernanceId::from_bytes([0x61; N]),
                ],
                selected_head: GovernanceId::from_bytes([0x60; N]),
                selected_state_root: StateRoot::from_bytes([0xee; N]),
                created_at_ms: 5_000,
            }),
            setup.clone(),
        ),
        (
            "migration.accept",
            GovernanceOperationPayload::MigrationAccept(MigrationAccept {
                migration_id: [0x70; N],
            }),
            setup.clone(),
        ),
    ];
    assert_eq!(cases.len(), 15, "§7.3 freezes exactly 15 operations");

    for (label, payload, prior) in cases {
        let seq = prior.accepted_seq.checked_add(1).unwrap();
        let declared_root = compute_state_root(&apply_entry(&prior, seq, &payload).unwrap());
        let expected_prev = if seq == 1 {
            None
        } else {
            Some(GovernanceId::from_bytes([0x80; N]))
        };
        let body = GovernanceEntryBody {
            community_id: cid,
            seq,
            prev: expected_prev,
            created_at_ms: 9_000,
            kind: payload.kind(),
            payload: payload.clone(),
            state_root: declared_root,
        };
        let wire = with_approval(seal(&body, &author), &body, &approver);
        let (new_state, new_id) = receive_and_fold(&prior, &wire, expected_prev, seq)
            .unwrap_or_else(|e| panic!("fold failed for `{label}`: {e:?}"));
        // The transition must be state-root-visible (no silent no-op), and the
        // folded entry id must recompute from the wire CSB.
        assert_ne!(
            compute_state_root(&new_state),
            compute_state_root(&prior),
            "`{label}` must change the state root"
        );
        assert_eq!(new_id, entry_id(&body), "`{label}` entry id must recompute");
        // The threaded identity must equal the exact-CSB id recomputed straight
        // from the received wire bytes (#178), not just the body-derived id.
        assert_eq!(
            new_id,
            entry_id_from_csb(&wire.csb),
            "`{label}` threaded id must equal the exact wire-CSB-derived id"
        );
    }
}

// ============================================================================
// §8 Authorization-boundary e2e (issue #148): the accepted-state pipeline.
// ============================================================================
//
// §§1-7 above drive every entry through `apply_verified_entry`, which folds
// any cryptographically valid, root-consistent entry regardless of who signed
// it (spec §7.3 — not an authorization boundary; see `verify_entry_full`'s doc
// comment). This section instead drives the #148 receiver pipeline:
//
//   canonical decode → verify_governance_entry (crypto + approval bindings)
//   → validate_and_apply_governance_entry (five-rule authorization) → commit
//
// against `ValidatedGovernanceState`, reconstructing every entry from raw wire
// bytes exactly as `receive_and_fold` does above (a `WireEntry`, not an
// in-memory `VerifiedGovernanceEntry`). This is the boundary the in-crate
// `governance/log/authz.rs` unit/property tests do not cross: those build
// verified entries in-process from typed bodies within the crate that defines
// the (otherwise unforgeable) wrapper types, while here — from an external
// integration-test crate using only the public API — a receiver only ever
// starts from `Vec<u8>` CSB, a raw signer/signature, and raw approval records.

/// The receiver: reconstruct a `GovernanceEntry` from wire bytes, then run it
/// through the full #148 pipeline (crypto verify → five-rule authorize)
/// against an accepted predecessor snapshot.
fn receive_and_authorize(
    prev: &ValidatedGovernanceState,
    wire: &WireEntry,
) -> Result<ValidatedGovernanceState, Reject> {
    // Reassemble the wire record straight from the received bytes — the trust
    // boundary. `from_received_csb` retains the supplied CSB byte-for-byte
    // (issue #178).
    let entry = GovernanceEntry::from_received_csb(
        wire.csb.clone(),
        PrincipalId::from_bytes(wire.signer),
        Signature::from_bytes(wire.sig),
        wire.approvals.clone(),
    )?;
    let verified = verify_governance_entry(&entry)?;
    validate_and_apply_governance_entry(prev, &verified)
}

/// Sender + receiver for one *authorized* entry: compute the declared root,
/// seal wire bytes signed by `author` with one approval per `approvers`, then
/// receive + authorize against `prev`. Panics if the entry is not authorized —
/// tests that want the rejection path call `receive_and_authorize` directly.
fn fold_one_authorized(
    prev: &ValidatedGovernanceState,
    payload: &GovernanceOperationPayload,
    author: &SigningKey,
    approvers: &[&SigningKey],
) -> ValidatedGovernanceState {
    let (seq, prev_id) = match prev.tip() {
        GovernanceTip::Genesis => (1u64, None),
        GovernanceTip::Entry { seq, id } => (seq + 1, Some(id)),
    };
    let declared = compute_state_root(&apply(prev.state(), payload).expect("payload applies"));
    let body = GovernanceEntryBody {
        community_id: prev.state().community_id,
        seq,
        prev: prev_id,
        created_at_ms: 1_000 + seq,
        kind: payload.kind(),
        payload: payload.clone(),
        state_root: declared,
    };
    let mut wire = seal(&body, author);
    for approver in approvers {
        wire = with_approval(wire, &body, approver);
    }
    receive_and_authorize(prev, &wire).expect("authorized entry must fold")
}

/// A multi-entry chain, each entry reconstructed from wire bytes and carried
/// through the full #148 authorization pipeline, produces the same accepted
/// state a direct `apply` fold would — proving the accepted-state wrapper
/// (`ValidatedGovernanceState`/`GovernanceTip`) threads correctly across
/// several wire round trips, not just a single call.
#[test]
fn e2e_authorized_pipeline_folds_multi_entry_chain_from_wire_bytes() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    assert_eq!(genesis.tip(), GovernanceTip::Genesis);
    let author = key(ADMIN_A_SEED);
    let approver = key(ADMIN_B_SEED);

    // Entry 1: member.grant, authorized by exactly the 2-of-3 old-admin quorum.
    let grant = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    let after_grant = fold_one_authorized(&genesis, &grant, &author, &[&approver]);
    assert!(after_grant
        .state()
        .members
        .contains_key(&principal(MEMBER_SEED)));
    assert!(matches!(
        after_grant.tip(),
        GovernanceTip::Entry { seq: 1, .. }
    ));

    // Entry 2: device.grant, chained off entry 1's accepted tip.
    let dev = DeviceId::from_bytes([0xd2; N]);
    let grant_dev = GovernanceOperationPayload::DeviceGrant(DeviceGrant {
        member_id: principal(MEMBER_SEED),
        device_id: dev,
        binding: Vec::new(),
    });
    let after_device = fold_one_authorized(&after_grant, &grant_dev, &author, &[&approver]);
    assert!(after_device
        .state()
        .members
        .get(&principal(MEMBER_SEED))
        .unwrap()
        .devices
        .contains_key(&dev));
    assert!(matches!(
        after_device.tip(),
        GovernanceTip::Entry { seq: 2, .. }
    ));

    // The accepted state root matches an independent direct-apply fold over
    // the same two operations, so the wire round trip changed nothing material.
    let mut direct = GovernanceState::from_genesis(&cfg, derive_community_id(&cfg));
    direct = apply(&direct, &grant).unwrap();
    direct = apply(&direct, &grant_dev).unwrap();
    assert_eq!(
        after_device.committed_state_root(),
        &compute_state_root(&direct)
    );

    // The original genesis snapshot was never mutated by either fold.
    assert_eq!(genesis.tip(), GovernanceTip::Genesis);
}

/// An entry that is cryptographically perfect — well-formed CSB, valid entry
/// signature, correct declared root — but carries only one of the two
/// required distinct old-admin signatures still folds successfully through
/// the non-authorizing #147 `apply_verified_entry` pipeline. The #148
/// authorization boundary must reject the same wire bytes, and must leave the
/// accepted predecessor snapshot completely unchanged.
#[test]
fn e2e_insufficient_threshold_entry_rejected_by_full_wire_pipeline() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    let author = key(ADMIN_A_SEED); // one distinct old admin; threshold is 2

    let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(MEMBER_SEED),
        roles: vec![Role::Member],
        profile: None,
    });
    let declared = compute_state_root(&apply(genesis.state(), &payload).unwrap());
    let body = GovernanceEntryBody {
        community_id: genesis.state().community_id,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: payload.kind(),
        payload,
        state_root: declared,
    };
    // No approvals attached: only the signer counts, one short of threshold 2.
    let wire = seal(&body, &author);

    // Sanity: the non-authorizing #147 pipeline folds this record — the
    // rejection below is specifically the #148 authorization boundary, not a
    // crypto or root defect.
    let (_folded, _id) = receive_and_fold(genesis.state(), &wire, None, 1)
        .expect("cryptographically valid entry folds under the non-authorizing pipeline");

    assert_eq!(
        receive_and_authorize(&genesis, &wire).err(),
        Some(Reject::InsufficientAuthorization),
        "one-of-two old-admin signatures must not authorize the entry"
    );

    // The rejected attempt must not have mutated the accepted predecessor.
    assert_eq!(genesis.tip(), GovernanceTip::Genesis);
    assert_eq!(
        genesis.committed_state_root(),
        &compute_state_root(genesis.state())
    );
}

/// The D6 admin-set invariant end to end over the wire: the old 2-of-3 quorum
/// authorizes an `admin.set` that replaces the administrators with a single
/// disjoint new admin; a wire-reconstructed entry signed only by that new
/// admin cannot authorize the transition itself (it is not yet effective, and
/// is an outsider to the old set); and once the transition is accepted, the
/// new admin alone authorizes the next entry from wire bytes while the old
/// quorum — no longer administrators — is rejected.
#[test]
fn e2e_admin_set_transition_old_quorum_then_new_quorum_via_wire_bytes() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    let old_a = key(ADMIN_A_SEED);
    let old_b = key(ADMIN_B_SEED);
    let new_admin = key(0xb5);

    let admin_set = GovernanceOperationPayload::AdminSet(AdminSet {
        administrators: vec![new_admin.member_id()],
        threshold: 1,
    });
    let declared = compute_state_root(&apply(genesis.state(), &admin_set).unwrap());
    let body = GovernanceEntryBody {
        community_id: genesis.state().community_id,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: admin_set.kind(),
        payload: admin_set,
        state_root: declared,
    };

    // The new admin signing alone cannot authorize its own appointment: the
    // proposed set is not yet effective and is an outsider to the old state.
    let new_only_wire = seal(&body, &new_admin);
    assert_eq!(
        receive_and_authorize(&genesis, &new_only_wire).err(),
        Some(Reject::InsufficientAuthorization)
    );

    // The old 2-of-3 quorum authorizes the same wire bytes.
    let old_quorum_wire = with_approval(seal(&body, &old_a), &body, &old_b);
    let committed = receive_and_authorize(&genesis, &old_quorum_wire)
        .expect("old quorum authorizes admin.set over the wire");
    assert_eq!(
        committed.state().administrators.administrators,
        vec![new_admin.member_id()]
    );

    // After commit, the new admin alone authorizes the next entry over the wire.
    let next_payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
        member_id: principal(0xc9),
        roles: vec![Role::Member],
        profile: None,
    });
    let after_next = fold_one_authorized(&committed, &next_payload, &new_admin, &[]);
    assert!(after_next.state().members.contains_key(&principal(0xc9)));

    // ...while the old (now-outsider) quorum cannot authorize a competing
    // next entry against the same committed predecessor.
    let next_declared = compute_state_root(&apply(committed.state(), &next_payload).unwrap());
    let by_old_body = GovernanceEntryBody {
        community_id: committed.state().community_id,
        seq: 2,
        prev: Some(match committed.tip() {
            GovernanceTip::Entry { id, .. } => id,
            GovernanceTip::Genesis => unreachable!("commit always advances the tip"),
        }),
        created_at_ms: 2_001,
        kind: next_payload.kind(),
        payload: next_payload,
        state_root: next_declared,
    };
    let by_old_wire = with_approval(seal(&by_old_body, &old_a), &by_old_body, &old_b);
    assert_eq!(
        receive_and_authorize(&committed, &by_old_wire).err(),
        Some(Reject::InsufficientAuthorization),
        "principals removed from the admin set must not authorize further entries"
    );
}

// ============================================================================
// §9 Verbatim-CSB trust boundary (issue #178).
// ============================================================================
//
// The record layer now retains the exact received CSB and verifies signatures
// + identity over those bytes, never a re-serialization of the typed body (see
// `GovernanceEntry::from_received_csb` / `verify_entry_crypto`). The trigger is
// `admin.set`: typed decode sorts and deduplicates `administrators`, so a wire
// record carrying `[B, A]` (or `[A, A, B]`) decodes to the same typed body as
// `[A, B]` but is byte-distinct and hashes to a different `GovernanceId`.
//
// These e2e cases build the altered CSB directly from public APIs (never via
// `entry_csb(decoded_body)`, which would re-derive the bytes under test) and
// drive the full receiver path: `from_received_csb` → `verify_entry_crypto` →
// `verify_governance_entry` → the non-authorizing `receive_and_fold` (#147) and
// the authorizing `receive_and_authorize` (#148). They fence the pre-#178
// weakness where a signature valid only over normalized bytes was accepted.

/// Reorder the `payload.administrators` array inside an entry-body
/// [`CborValue`] to `perm` (touching no other field), then deterministically
/// encode it. Mirrors the private `records.rs` unit-test helper using only
/// public APIs, so the bytes under test are never re-derived from the typed
/// body.
fn altered_admin_set_csb(body: &GovernanceEntryBody, perm: &[usize]) -> Vec<u8> {
    let mut value = body.to_cbor();
    if let CborValue::Map(ref mut entries) = value {
        for (k, v) in entries.iter_mut() {
            if k == "payload" {
                if let CborValue::Map(ref mut payload_entries) = v {
                    for (pk, pv) in payload_entries.iter_mut() {
                        if pk == "administrators" {
                            if let CborValue::Array(ref mut admins) = pv {
                                let original = admins.clone();
                                admins.clear();
                                for &idx in perm {
                                    admins.push(original[idx].clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    cbor::encode(&value)
}

/// A valid `admin.set` entry body over a sorted unique two-admin set `[a, b]`
/// (a < b), changing the genesis threshold 2 → 1 so the transition is
/// state-root-visible. The `administrators` array normalizes during typed
/// decode, so a permuted/duplicated re-encoding decodes to the same typed body
/// — the #178 regression trigger.
fn normalizing_admin_set_body(
    genesis: &ValidatedGovernanceState,
    a: PrincipalId,
    b: PrincipalId,
) -> GovernanceEntryBody {
    let admin_set = GovernanceOperationPayload::AdminSet(AdminSet {
        administrators: vec![a, b],
        threshold: 1,
    });
    let declared = compute_state_root(&apply(genesis.state(), &admin_set).unwrap());
    GovernanceEntryBody {
        community_id: genesis.state().community_id,
        seq: 1,
        prev: None,
        created_at_ms: 1_001,
        kind: admin_set.kind(),
        payload: admin_set,
        state_root: declared,
    }
}

/// The #178 regression fixture, built only from public APIs: a valid
/// `admin.set` body, its canonical (normalized) CSB, and an *altered* received
/// CSB whose permuted `administrators` array is valid deterministic CBOR that
/// normalizes to the same typed body but is byte-distinct.
struct NormalizingFixture {
    body: GovernanceEntryBody,
    normalized_csb: Vec<u8>,
    received_csb: Vec<u8>,
}

fn normalizing_admin_set_fixture(genesis: &ValidatedGovernanceState) -> NormalizingFixture {
    // Deterministic principals A < B (ed25519 public-key bytes are not
    // seed-ordered; sort to guarantee A < B so the typed body is canonical).
    let mut principals = [principal(0x01), principal(0x02)];
    principals.sort();
    let [a, b] = principals;
    let body = normalizing_admin_set_body(genesis, a, b);
    let normalized_csb = entry_csb(&body);
    // Altered CSB: `administrators` permuted to `[B, A]` — valid deterministic
    // CBOR that decodes to the same normalized typed body.
    let received_csb = altered_admin_set_csb(&body, &[1, 0]);
    NormalizingFixture {
        body,
        normalized_csb,
        received_csb,
    }
}

/// A record reconstructed from a *normalizing* received CSB (valid
/// deterministic CBOR that decodes to the same typed body as the canonical
/// encoding) but carrying a signature valid only over the *normalized* bytes
/// must be rejected at the signature check — over the exact received bytes —
/// before authorization or state application. Before #178 the verify path
/// re-derived the CSB from the typed body and so accepted this forgery.
///
/// This drives both receiver pipelines (`receive_and_fold` / #147 and
/// `receive_and_authorize` / #148) and proves: (a) the received bytes are
/// retained verbatim by `from_received_csb`; (b) the rejection is `BadSignature`
/// at entry crypto, before any approval binding or authorization work; (c) the
/// predecessor state is left unchanged.
#[test]
fn e2e_normalizing_admin_set_with_normalized_only_signature_rejected_over_wire() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    let author = key(ADMIN_A_SEED);
    let old_b = key(ADMIN_B_SEED);
    let f = normalizing_admin_set_fixture(&genesis);

    // The altered CSB must be byte-distinct from the normalized CSB, yet both
    // must decode to the same normalized typed body, and their exact-CSB
    // identities must differ.
    assert_ne!(
        f.received_csb, f.normalized_csb,
        "altered CSB must differ from the normalized CSB"
    );
    assert_eq!(
        decode_entry_csb(&f.received_csb).unwrap(),
        decode_entry_csb(&f.normalized_csb).unwrap(),
        "both CSBs must normalize to the same typed body"
    );
    assert_ne!(
        entry_id_from_csb(&f.received_csb),
        entry_id_from_csb(&f.normalized_csb),
        "exact-CSB identities must differ"
    );

    // Seal the body (signing over the NORMALIZED CSB) and attach a valid
    // approval bound to the normalized entry id — i.e. a record that *would*
    // be fully authorized but for the byte-level signature boundary.
    let mut wire = with_approval(seal(&f.body, &author), &f.body, &old_b);
    // The signature is valid only over the normalized bytes; swap in the
    // altered received bytes a forger would put on the wire.
    wire.csb = f.received_csb.clone();

    // (a) Verbatim retention: the receiver constructor keeps the exact bytes.
    let entry = GovernanceEntry::from_received_csb(
        wire.csb.clone(),
        PrincipalId::from_bytes(wire.signer),
        Signature::from_bytes(wire.sig),
        wire.approvals.clone(),
    )
    .expect("altered CSB canonical-decodes");
    assert_eq!(
        entry.csb(),
        wire.csb.as_slice(),
        "from_received_csb must retain the exact received bytes (#178)"
    );

    // (b) Rejection is BadSignature at entry crypto, before any approval /
    // binding / authorization work.
    assert_eq!(
        verify_entry_crypto(&entry).err(),
        Some(Reject::BadSignature),
        "a signature valid only over normalized bytes must not verify over the \
         altered received bytes"
    );
    assert_eq!(
        verify_governance_entry(&entry).err(),
        Some(Reject::BadSignature),
        "the full crypto pipeline must reject at the entry signature check, \
         before approvals"
    );

    // Both receiver pipelines reject identically at the signature boundary —
    // never reaching authorization or state application.
    assert_eq!(
        receive_and_fold(&genesis.state().clone(), &wire, None, 1).err(),
        Some(Reject::BadSignature),
        "the non-authorizing #147 pipeline must reject the forged record"
    );
    assert_eq!(
        receive_and_authorize(&genesis, &wire).err(),
        Some(Reject::BadSignature),
        "the #148 authorization pipeline must reject before authorization"
    );

    // (c) The rejected attempt must not have mutated the accepted predecessor.
    assert_eq!(genesis.tip(), GovernanceTip::Genesis);
    assert_eq!(
        genesis.committed_state_root(),
        &compute_state_root(genesis.state())
    );
}

/// An exact signature over a *normalizing* received CSB passes the signature
/// check but must fail the post-signature semantic-canonicality round-trip,
/// surfacing as `NonCanonicalEncoding` over the full receiver path. This
/// preserves the verbatim decode/re-encode guarantee without inventing an
/// impossible approval-normalization vector (spec §5.6).
#[test]
fn e2e_normalizing_admin_set_signed_over_exact_bytes_is_non_canonical_over_wire() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    let author = key(ADMIN_A_SEED);
    let f = normalizing_admin_set_fixture(&genesis);

    // A signature valid over the EXACT altered bytes — passes the signature
    // check, then fails the post-signature round-trip.
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &f.received_csb);
    let exact_sig = *author.sign(&msg).as_bytes();
    let wire = WireEntry {
        csb: f.received_csb.clone(),
        signer: *author.member_id().as_bytes(),
        sig: exact_sig,
        approvals: Vec::new(),
    };

    let entry = GovernanceEntry::from_received_csb(
        wire.csb.clone(),
        PrincipalId::from_bytes(wire.signer),
        Signature::from_bytes(wire.sig),
        wire.approvals.clone(),
    )
    .expect("altered CSB canonical-decodes");
    assert_eq!(
        entry.csb(),
        f.received_csb.as_slice(),
        "from_received_csb must retain the exact received bytes (#178)"
    );
    assert_eq!(
        verify_entry_crypto(&entry).err(),
        Some(Reject::NonCanonicalEncoding),
        "an exact signature over semantically normalizing bytes must fail the \
         post-signature round-trip"
    );
    assert_eq!(
        verify_governance_entry(&entry).err(),
        Some(Reject::NonCanonicalEncoding),
        "the full crypto pipeline must reject after the signature check"
    );
    assert_eq!(
        receive_and_fold(&genesis.state().clone(), &wire, None, 1).err(),
        Some(Reject::NonCanonicalEncoding),
        "the non-authorizing #147 pipeline must reject the normalizing record"
    );
    assert_eq!(
        receive_and_authorize(&genesis, &wire).err(),
        Some(Reject::NonCanonicalEncoding),
        "the #148 authorization pipeline must reject the normalizing record"
    );
}

/// Positive folds must thread the *exact wire-CSB-derived* identity as the
/// next chain link (spec §8.3 item 6). A normalizing `admin.set` folds cleanly
/// when its CSB is the canonical encoding (not a permuted forgery), and the
/// returned `verified.id()` — used as the next entry's `prev` — equals
/// `entry_id_from_csb` recomputed straight from the received wire bytes, not a
/// body-derived identity.
#[test]
fn e2e_positive_fold_threads_exact_wire_csb_identity_as_next_link() {
    let cfg = genesis_config();
    let sigs = [
        sign_genesis(&cfg, &key(ADMIN_A_SEED)),
        sign_genesis(&cfg, &key(ADMIN_B_SEED)),
    ];
    let genesis = validated_genesis_state(&cfg, &sigs).expect("genesis threshold met");
    let author = key(ADMIN_A_SEED);
    let old_b = key(ADMIN_B_SEED);

    // A canonical (non-altered) admin.set over a sorted unique set folds.
    let mut principals = [principal(0x01), principal(0x02)];
    principals.sort();
    let [a, b] = principals;
    let body = normalizing_admin_set_body(&genesis, a, b);
    let wire = with_approval(seal(&body, &author), &body, &old_b);

    // The identity a receiver derives from the verified record must equal the
    // exact-CSB identity recomputed straight from the wire bytes — and the body
    // is canonical, so the body-derived identity agrees too.
    let expected_id = entry_id_from_csb(&wire.csb);
    assert_eq!(
        expected_id,
        entry_id(&body),
        "for a canonical body the exact-CSB and body-derived identities agree"
    );

    let (_next, folded_id) =
        receive_and_fold(&genesis.state().clone(), &wire, None, 1).expect("canonical fold");
    assert_eq!(
        folded_id, expected_id,
        "the threaded next-link identity must equal the exact wire-CSB-derived id (#178)"
    );

    // And the same identity survives the authorizing #148 pipeline's accepted
    // tip — the value a subsequent entry's `prev` must bind to.
    let committed = receive_and_authorize(&genesis, &wire).expect("authorized fold");
    let committed_id = match committed.tip() {
        GovernanceTip::Entry { id, .. } => id,
        GovernanceTip::Genesis => unreachable!("a folded entry advances the tip"),
    };
    assert_eq!(
        committed_id, expected_id,
        "the accepted tip must carry the exact wire-CSB-derived id (#178)"
    );
}
