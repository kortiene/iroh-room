//! Taxonomy-completeness guard (spec §9 / §10): every public rejection code in
//! [`iroh_rooms_v2_core::Reject`] must be reachable and named here, so a new code
//! cannot land without a covering reference (the v1 §10 tripwire discipline).
//!
//! This does not exhaustively *exercise* every code path (that is the separate
//! `tests` phase / #153 golden vectors); it asserts the taxonomy is closed and
//! that `all_codes()` agrees with the enum.

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::Reject;

/// Every variant the crate can emit, paired with its stable `.code()` string.
/// Adding a variant without listing it here is a deliberate, reviewable act.
const EXPECTED: &[(Reject, &str)] = &[
    (Reject::NonCanonicalEncoding, "non_canonical_encoding"),
    (Reject::UnknownVersion, "unknown_version"),
    (Reject::UnknownRecordKind, "unknown_record_kind"),
    (Reject::UnknownContentKind, "unknown_content_kind"),
    (Reject::InvalidContent, "invalid_content"),
    (Reject::IdMismatch, "id_mismatch"),
    (Reject::BadSignature, "bad_signature"),
    (Reject::WrongDomain, "wrong_domain"),
    (Reject::MissingDependency, "missing_dependency"),
    (
        Reject::InsufficientAuthorization,
        "insufficient_authorization",
    ),
    (Reject::InvalidApproval, "invalid_approval"),
    (Reject::ForkDetected, "fork_detected"),
    (Reject::UnresolvedFork, "unresolved_fork"),
    (Reject::InvalidForkResolution, "invalid_fork_resolution"),
    (Reject::StateRootMismatch, "state_root_mismatch"),
    (Reject::SnapshotHashMismatch, "snapshot_hash_mismatch"),
    (Reject::InvalidMerkleProof, "invalid_merkle_proof"),
];

#[test]
fn every_rejection_code_is_named_and_consistent() {
    let all = iroh_rooms_v2_core::error::all_codes();
    // Each named code is present in `all_codes()` exactly once.
    for (_, code) in EXPECTED {
        let count = all.iter().filter(|c| **c == *code).count();
        assert_eq!(
            count, 1,
            "code {code:?} must appear exactly once in all_codes()"
        );
    }
    // No unnamed codes slipped in.
    assert_eq!(
        all.len(),
        EXPECTED.len(),
        "all_codes() has {} entries but the taxonomy names {} — a code was added without listing it here",
        all.len(),
        EXPECTED.len()
    );
    // Each variant's `.code()` matches its declared string.
    for (reject, code) in EXPECTED {
        assert_eq!(reject.code(), *code);
    }
}

#[test]
fn codes_are_stable_snake_case_strings() {
    for code in iroh_rooms_v2_core::error::all_codes() {
        assert!(
            code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "code {code:?} must be stable lowercase snake_case"
        );
        assert!(!code.is_empty(), "codes must be non-empty");
    }
}
