//! End-to-end coverage for the frozen #134 §6.3 v2 identifiers across the full
//! signed-record trust boundary (issue #146 acceptance: §6.4 "signature/ID
//! mismatch").
//!
//! `tests/identifiers.rs` pins each identifier's *derivation* in isolation
//! (preimage -> digest -> display -> parse) and `tests/signed_records_golden.rs`
//! §15 exercises the full cross-boundary pipeline for the *legacy* candidate
//! types (`GovernanceEntryId`, `ContentEventId`, ...). Neither covers the
//! frozen §6.3 newtypes (`CommunityId`, `GovernanceId`, `StreamId`, `EventId`,
//! `CheckpointId`, `ReplicaId`) end to end through the real CBOR + BLAKE3 +
//! Ed25519 primitives. This file closes that gap.
//!
//! Per spec D2 (documented at `domain.rs`): the same frozen §6.2 domain string
//! serves double duty — as the BLAKE3 id-derivation prefix AND the Ed25519
//! signing-message prefix. So a v2 record over a frozen identifier is:
//!
//! ```text
//!   CSB = canonical_cbor(descriptor)
//!   id  = BLAKE3(DOMAIN || CSB)        // typed v2 identifier
//!   sig = Ed25519_sign(secret, DOMAIN || CSB)
//! ```
//!
//! These tests cross the canonical-CBOR-serialize <-> BLAKE3-id <->
//! Ed25519-signature <-> strict-verify boundaries, which no single isolated
//! unit test can cover, and pin:
//!
//! 1. **Full round-trip from raw wire/storage bytes** — reconstruct
//!    `{id, csb, sig, signer}` exactly as a receiver pulls it off the
//!    wire/storage, then run the canonical verifier: CSB decodes canonically,
//!    `BLAKE3(domain || CSB)` equals the envelope id, the signature verifies
//!    under the frozen domain, and the decoded body re-encodes to the verbatim
//!    received CSB.
//! 2. **§6.4 signature/ID mismatch** — a single CSB byte flip is rejected
//!    (`IdMismatch` when still canonical, `NonCanonicalEncoding` when not); a
//!    single signature byte flip is rejected (`BadSignature`).
//! 3. **Cross-domain replay isolation through real Ed25519** — a signature
//!    valid under one frozen §6.2 domain does NOT verify under another frozen
//!    domain, end to end through `keys::verify` (the §6.2 fence has teeth at
//!    the signature layer, not just the hash layer).
//!
//! All keys are deterministic public test seeds (non-secret); no entropy,
//! network, store, or real user data is involved. The crate stays pure: these
//! tests pull in no `tokio`/`iroh`/`iroh-blobs` (the `banned_dependencies`
//! test already machine-checks that).

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::{self, CborValue};
use iroh_rooms_v2_core::domain::{self, ALL_V2_DOMAINS};
use iroh_rooms_v2_core::ids::{
    CheckpointId, CommunityId, EventId, GovernanceId, ReplicaId, StreamId, LEN,
};
use iroh_rooms_v2_core::keys::{self, Signature, SigningKey};
use iroh_rooms_v2_core::MemberId;

// ============================================================================
// Frozen golden preimages — mirror of `golden/v2-identifiers.json` and
// `tests/identifiers.rs`. ANY change here requires a schema-version bump.
// ============================================================================

const COMMUNITY_PREIMAGE_HEX: &str = "a2646e616d6570676f6c64656e2d636f6d6d756e6974796565706f636801";
const COMMUNITY_DIGEST_HEX: &str =
    "e593db90754f6d24cdfc13f59c0b2a709d541aecf5d62c05d8fd14bc9e6f5a4a";

const GOVERNANCE_PREIMAGE_HEX: &str =
    "a36373657101646b696e6469696e69745f726f6f6d6565706f63681903e8";
const GOVERNANCE_DIGEST_HEX: &str =
    "518ce110d11f63da61cc335705e2063e0185f50ed20a5b5398925defb5bff8d9";

const STREAM_PREIMAGE_HEX: &str = "a26373657101646e616d656d676f6c64656e2d73747265616d";
const STREAM_DIGEST_HEX: &str = "78559e86ff09920b1d07cc4dcaa28d9b613d08df6c4f96d8b6e417d6fdf8ec60";

const EVENT_PREIMAGE_HEX: &str =
    "a3637365710164626f64796f68656c6c6f20676f6c64656e207632646b696e646c6d6573736167652e74657874";
const EVENT_DIGEST_HEX: &str = "14ff4a5b86dbd7f13b6e40b6892eb14eeeeb55f3f721120647e2c1b4da10077c";

const CHECKPOINT_PREIMAGE_HEX: &str = "a263736571016565706f63681907d0";
const CHECKPOINT_GOV_DIGEST_HEX: &str =
    "9b14d6abe5645f6f3476a384ee9679f3ac0411c0dcb586a8a3345a0505aef138";
const CHECKPOINT_STREAM_DIGEST_HEX: &str =
    "c18eef16358a6058cf702314deeca21da045126589c520ac54a0601333f3a331";

const REPLICA_PREIMAGE_HEX: &str = "a26373657101646b696e64677265706c696361";
const REPLICA_DIGEST_HEX: &str = "0e389a8f70f6c6035cad205ca4f3bd680b7ea3e394a3b342f412b8bfe52944b0";

// Deterministic seed-derived signing key for the v2 principal (non-secret).
const SIGNER_SEED: u8 = 0xa0;

// ============================================================================
// Helpers
// ============================================================================

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("frozen fixture hex must be valid lowercase")
}

fn signer() -> SigningKey {
    SigningKey::from_seed(&[SIGNER_SEED; LEN])
}

/// The raw, type-erased bytes a receiver pulls off the wire/storage for a v2
/// record. Built here from frozen hex so it is independent of any in-process
/// builder — the same way a peer or a store reconstruction would see it.
struct WireRecord {
    id: [u8; LEN],
    csb: Vec<u8>,
    sig: [u8; keys::SIGNATURE_LEN],
    signer: [u8; LEN],
}

/// Build a wire record straight from a frozen CSB + frozen domain, signing with
/// the deterministic principal. This is the *sender/storage-writer* path.
fn seal_from_csb(csb: &[u8], domain_ctx: &[u8], secret: &SigningKey) -> WireRecord {
    let id = domain::blake3_domain(domain_ctx, csb);
    let msg = domain::signing_message(domain_ctx, csb);
    WireRecord {
        id,
        csb: csb.to_vec(),
        sig: *secret.sign(&msg).as_bytes(),
        signer: *secret.member_id().as_bytes(),
    }
}

/// A failure class observed by the full v2 verifier, mirroring the `Reject`
/// taxonomy the typed `signed::verify_envelope` surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    NonCanonical,
    IdMismatch,
    BadSignature,
}

/// The complete receiver-side v2 record verifier, assembled from the public
/// low-level primitives. Runs every boundary in order:
///   1. CSB decodes canonically;
///   2. `BLAKE3(domain || CSB)` equals the envelope id (§6.4 ID mismatch);
///   3. the Ed25519 signature over `domain || CSB` verifies under the signer
///      (§6.4 signature mismatch).
///
/// Returns the decoded canonical value on success so callers can additionally
/// assert verbatim-CSB preservation.
fn verify_v2_record(rec: &WireRecord, domain_ctx: &[u8]) -> Result<CborValue, Fault> {
    let value = cbor::decode_canonical(&rec.csb).map_err(|_| Fault::NonCanonical)?;
    let recomputed = domain::blake3_domain(domain_ctx, &rec.csb);
    if recomputed != rec.id {
        return Err(Fault::IdMismatch);
    }
    let msg = domain::signing_message(domain_ctx, &rec.csb);
    keys::verify(
        &MemberId::from_bytes(rec.signer),
        &msg,
        &Signature::from_bytes(rec.sig),
    )
    .map_err(|_| Fault::BadSignature)?;
    Ok(value)
}

// ============================================================================
// §1 Full round-trip from raw wire/storage bytes for every frozen v2 id family.
// ============================================================================

/// Assert the complete pipeline succeeds on a record reconstructed from frozen
/// bytes, and that the decoded body re-encodes to the verbatim received CSB.
fn assert_full_round_trip(csb_hex: &str, domain_ctx: &[u8], expected_id_hex: &str) {
    let csb = hx(csb_hex);
    let secret = signer();
    let rec = seal_from_csb(&csb, domain_ctx, &secret);

    // The id the sender stamped equals the frozen golden digest (independent
    // recompute straight from the preimage).
    assert_eq!(hex::encode(rec.id), expected_id_hex);

    // Full receiver verification succeeds on the raw bytes.
    let value = verify_v2_record(&rec, domain_ctx).expect("frozen record must verify e2e");

    // Verbatim-CSB preservation (spec Step 4.4): encode(decode(csb)) == csb.
    assert_eq!(
        cbor::encode(&value),
        csb,
        "decoded body does not re-encode to received CSB"
    );

    // The typed identifier, built from the SAME CSB under the SAME frozen
    // domain, wraps the same digest the verifier checked — the id surface and
    // the verify surface agree byte-for-byte.
    assert_eq!(
        hex::encode(domain::blake3_domain(domain_ctx, &csb)),
        expected_id_hex
    );
}

#[test]
fn e2e_community_record_round_trips_through_frozen_domain() {
    assert_full_round_trip(
        COMMUNITY_PREIMAGE_HEX,
        domain::COMMUNITY,
        COMMUNITY_DIGEST_HEX,
    );
}

#[test]
fn e2e_governance_record_round_trips_through_frozen_domain() {
    assert_full_round_trip(
        GOVERNANCE_PREIMAGE_HEX,
        domain::GOVERNANCE_ENTRY,
        GOVERNANCE_DIGEST_HEX,
    );
}

#[test]
fn e2e_stream_record_round_trips_through_frozen_domain() {
    assert_full_round_trip(
        STREAM_PREIMAGE_HEX,
        domain::CONTENT_EVENT,
        STREAM_DIGEST_HEX,
    );
}

#[test]
fn e2e_event_record_round_trips_through_frozen_domain() {
    assert_full_round_trip(EVENT_PREIMAGE_HEX, domain::CONTENT_EVENT, EVENT_DIGEST_HEX);
}

#[test]
fn e2e_checkpoint_record_round_trips_through_both_frozen_domains() {
    // The same checkpoint descriptor derives distinct ids under its two frozen
    // domains, and BOTH verify end to end through the real Ed25519 path.
    assert_full_round_trip(
        CHECKPOINT_PREIMAGE_HEX,
        domain::GOVERNANCE_CHECKPOINT,
        CHECKPOINT_GOV_DIGEST_HEX,
    );
    assert_full_round_trip(
        CHECKPOINT_PREIMAGE_HEX,
        domain::STREAM_CHECKPOINT,
        CHECKPOINT_STREAM_DIGEST_HEX,
    );
    // The two pinned domains produce distinct wire ids for one descriptor.
    let csb = hx(CHECKPOINT_PREIMAGE_HEX);
    let gov = domain::blake3_domain(domain::GOVERNANCE_CHECKPOINT, &csb);
    let stream = domain::blake3_domain(domain::STREAM_CHECKPOINT, &csb);
    assert_ne!(gov, stream);
}

#[test]
fn e2e_replica_record_round_trips_through_frozen_domain() {
    assert_full_round_trip(
        REPLICA_PREIMAGE_HEX,
        domain::REPLICA_RECEIPT,
        REPLICA_DIGEST_HEX,
    );
}

// ============================================================================
// §2 §6.4 signature / ID mismatch — tampered bytes are rejected.
// ============================================================================

#[test]
fn e2e_tampered_csb_byte_is_rejected() {
    // Flip one byte in the middle of a canonical CSB. The result is still a
    // well-formed canonical map, so the failure isolates to IdMismatch (the
    // recomputed BLAKE3 no longer equals the stamped id).
    let csb = hx(COMMUNITY_PREIMAGE_HEX);
    let secret = signer();
    let mut rec = seal_from_csb(&csb, domain::COMMUNITY, &secret);
    // Flip a payload byte (index 4 is safely inside the map body, not the head).
    rec.csb[4] ^= 0x01;
    assert_eq!(
        verify_v2_record(&rec, domain::COMMUNITY),
        Err(Fault::IdMismatch),
        "a CSB byte flip must surface as IdMismatch, not a silent verify"
    );
}

#[test]
fn e2e_non_canonical_csb_is_rejected() {
    // Append a trailing byte -> the strict decoder rejects before any id/sig
    // work (the canonical-CBOR rule fires first).
    let csb = hx(GOVERNANCE_PREIMAGE_HEX);
    let secret = signer();
    let mut rec = seal_from_csb(&csb, domain::GOVERNANCE_ENTRY, &secret);
    rec.csb.push(0x00);
    assert_eq!(
        verify_v2_record(&rec, domain::GOVERNANCE_ENTRY),
        Err(Fault::NonCanonical)
    );
}

#[test]
fn e2e_flipped_signature_byte_is_rejected() {
    // CSB and id remain valid; only the signature is corrupted.
    let csb = hx(EVENT_PREIMAGE_HEX);
    let secret = signer();
    let mut rec = seal_from_csb(&csb, domain::CONTENT_EVENT, &secret);
    rec.sig[0] ^= 0x01;
    assert_eq!(
        verify_v2_record(&rec, domain::CONTENT_EVENT),
        Err(Fault::BadSignature)
    );
}

#[test]
fn e2e_wrong_signer_is_rejected() {
    // A signature from a different principal does not verify under the stamped
    // signer (the OQ-2 single-key model: identity == verifying key).
    let csb = hx(REPLICA_PREIMAGE_HEX);
    let impostor = SigningKey::from_seed(&[0xee; LEN]);
    let mut rec = seal_from_csb(&csb, domain::REPLICA_RECEIPT, &impostor);
    // Claim it was signed by the real signer.
    rec.signer = *signer().member_id().as_bytes();
    assert_eq!(
        verify_v2_record(&rec, domain::REPLICA_RECEIPT),
        Err(Fault::BadSignature)
    );
}

// ============================================================================
// §3 Cross-domain replay isolation through real Ed25519 verification.
// ============================================================================

/// A signature valid under one frozen §6.2 domain must NOT verify under a
/// different frozen domain. This is the §6.2 fence proven end to end at the
/// *signature* layer (not just the hash layer, which `identifiers.rs` already
/// pins). We reuse the frozen community preimage as a stand-in record body.
#[test]
fn e2e_signature_not_replayable_across_frozen_domains() {
    let csb = hx(COMMUNITY_PREIMAGE_HEX);
    let secret = signer();

    // Sign legitimately under COMMUNITY.
    let legit = seal_from_csb(&csb, domain::COMMUNITY, &secret);
    assert_eq!(
        verify_v2_record(&legit, domain::COMMUNITY),
        Ok(cbor::decode_canonical(&csb).unwrap()),
        "the legitimately-sealed record verifies under its own frozen domain"
    );

    // Replay under GOVERNANCE_ENTRY: recompute the id under the TARGET domain so
    // the failure isolates to the signature (the signing-message prefix differs).
    let replayed = WireRecord {
        id: domain::blake3_domain(domain::GOVERNANCE_ENTRY, &csb),
        csb: csb.clone(),
        sig: legit.sig,
        signer: legit.signer,
    };
    assert_eq!(
        verify_v2_record(&replayed, domain::GOVERNANCE_ENTRY),
        Err(Fault::BadSignature),
        "a COMMUNITY-domain signature must not verify as a GOVERNANCE_ENTRY signature"
    );
}

/// Pairwise fence: for several frozen domains, a signature bound to domain A
/// fails verification under domain B. Exercises the property across the full
/// frozen set rather than a single hand-picked pair.
#[test]
fn e2e_cross_domain_replay_blocked_across_frozen_set() {
    let csb = hx(STREAM_PREIMAGE_HEX);
    let secret = signer();
    // Domains with a distinct id-derivation so the failure isolates to the
    // signature check (id is recomputed under the target domain below).
    let domains = [
        domain::COMMUNITY,
        domain::GOVERNANCE_ENTRY,
        domain::CONTENT_EVENT,
        domain::REPLICA_RECEIPT,
        domain::GOVERNANCE_CHECKPOINT,
        domain::STREAM_CHECKPOINT,
        domain::MIGRATION,
    ];

    for (i, &sign_domain) in domains.iter().enumerate() {
        let sealed = seal_from_csb(&csb, sign_domain, &secret);
        // Self-verify passes.
        assert_eq!(
            verify_v2_record(&sealed, sign_domain).err(),
            None,
            "record must verify under its own signing domain"
        );
        for (j, &target_domain) in domains.iter().enumerate() {
            if i == j {
                continue;
            }
            // Recompute the id under the TARGET domain so a BadSignature result
            // is attributable solely to the signing-message prefix mismatch.
            let replayed = WireRecord {
                id: domain::blake3_domain(target_domain, &csb),
                csb: csb.clone(),
                sig: sealed.sig,
                signer: sealed.signer,
            };
            assert_eq!(
                verify_v2_record(&replayed, target_domain),
                Err(Fault::BadSignature),
                "signature under {:?} must not verify under {:?}",
                std::str::from_utf8(sign_domain).unwrap(),
                std::str::from_utf8(target_domain).unwrap(),
            );
        }
    }
}

// ============================================================================
// §4 Cross-boundary consistency — every frozen domain round-trips a real
// signed record, and the frozen typed identifiers agree with the verifier.
// ============================================================================

/// Every frozen §6.2 domain seals + verifies a record end to end, and produces
/// a distinct id for a shared preimage (the §6.2 fence, checked through the
/// complete pipeline rather than `blake3_domain` alone).
#[test]
fn e2e_every_frozen_domain_round_trips_and_separates() {
    let csb = hx(EVENT_PREIMAGE_HEX);
    let secret = signer();
    let mut ids: Vec<[u8; LEN]> = Vec::new();
    for (_name, dom) in ALL_V2_DOMAINS {
        let rec = seal_from_csb(&csb, dom, &secret);
        // Each domain verifies a real signed record end to end.
        verify_v2_record(&rec, dom).expect("every frozen domain must verify a sealed record");
        // And yields a distinct id for the shared preimage.
        assert!(
            !ids.contains(&rec.id),
            "frozen domain {:?} collided with an earlier domain",
            std::str::from_utf8(dom).unwrap()
        );
        ids.push(rec.id);
    }
    assert_eq!(ids.len(), 11, "#134 §6.2 freezes eleven domains");
}

/// The frozen §6.3 typed identifiers and the byte-oriented verifier agree:
/// each typed `derive`/`from_*_csb` helper produces the exact digest the
/// verifier recomputes from the same CSB under the same frozen domain. This is
/// the "recompute exactly from its declared preimage" acceptance, proven
/// across the CBOR<->hash<->typed-id boundary.
#[test]
fn e2e_typed_identifiers_match_verifier_recompute() {
    let community_csb = hx(COMMUNITY_PREIMAGE_HEX);
    let governance_csb = hx(GOVERNANCE_PREIMAGE_HEX);
    let stream_csb = hx(STREAM_PREIMAGE_HEX);
    let event_csb = hx(EVENT_PREIMAGE_HEX);
    let checkpoint_csb = hx(CHECKPOINT_PREIMAGE_HEX);
    let replica_csb = hx(REPLICA_PREIMAGE_HEX);

    assert_eq!(
        CommunityId::derive(&community_csb).as_bytes(),
        &domain::blake3_domain(domain::COMMUNITY, &community_csb)
    );
    assert_eq!(
        GovernanceId::from_governance_entry_csb(&governance_csb).as_bytes(),
        &domain::blake3_domain(domain::GOVERNANCE_ENTRY, &governance_csb)
    );
    assert_eq!(
        StreamId::from_stream_descriptor_csb(&stream_csb).as_bytes(),
        &domain::blake3_domain(domain::CONTENT_EVENT, &stream_csb)
    );
    assert_eq!(
        EventId::from_content_event_csb(&event_csb).as_bytes(),
        &domain::blake3_domain(domain::CONTENT_EVENT, &event_csb)
    );
    assert_eq!(
        CheckpointId::from_governance_checkpoint_csb(&checkpoint_csb).as_bytes(),
        &domain::blake3_domain(domain::GOVERNANCE_CHECKPOINT, &checkpoint_csb)
    );
    assert_eq!(
        CheckpointId::from_stream_checkpoint_csb(&checkpoint_csb).as_bytes(),
        &domain::blake3_domain(domain::STREAM_CHECKPOINT, &checkpoint_csb)
    );
    assert_eq!(
        ReplicaId::from_replica_descriptor_csb(&replica_csb).as_bytes(),
        &domain::blake3_domain(domain::REPLICA_RECEIPT, &replica_csb)
    );
}
