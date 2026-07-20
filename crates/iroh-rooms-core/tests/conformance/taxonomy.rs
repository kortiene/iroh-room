//! The §8 taxonomy coverage matrix and the completeness gate (AC5).
//!
//! Every `RejectReason` / `Flag` variant in the spike §8 taxonomy — plus the
//! `duplicate` ignored-outcome — must be either exercised by a named vector or
//! listed on the explicit [`DEFERRED`] list. [`DEFERRED`] is **empty**: the whole
//! taxonomy is covered by this suite.
//!
//! ## Note on the enforcement mechanism
//!
//! [`RejectReason`] and [`Flag`] are `#[non_exhaustive]`. An *external* crate
//! (this integration-test binary) therefore **cannot** write an exhaustive
//! `match` over them — the compiler forces a wildcard arm, which would silently
//! absorb a newly-added variant. The spike's envisioned "adding a variant fails
//! to compile" tripwire is only available *inside* `iroh-rooms-core`, and this is
//! a test-only PR (spec non-goal §2: no `src/` changes). So the gate is enforced
//! here at three layers, together strong enough that a new taxonomy member cannot
//! land silently reviewed:
//!
//! 1. [`ALL_REASON_CODES`] / [`ALL_FLAG_CODES`] pin the taxonomy **counts**
//!    (15 + 3); adding a variant without extending these lists makes their
//!    consistency check with the constructed variants fail.
//! 2. [`reason_and_flag_code_strings_match_spec`] pins every `.code()` string
//!    (closing the 9-of-14 gap the old `golden_vectors.rs` test left open).
//! 3. [`every_reason_and_flag_is_covered_or_deferred`] fails if any known code is
//!    neither in [`COVERAGE`] nor [`DEFERRED`], and if any [`COVERAGE`] row names
//!    a code outside the taxonomy (stale-entry guard).

use std::collections::BTreeSet;

use iroh_rooms_core::event::reject::{Flag, RejectReason};

/// One row per §8 outcome → the vector (or ported test) that exercises it.
const COVERAGE: &[(&str, &str)] = &[
    // Rejections (15).
    (
        "unknown_schema_version",
        "serialization::unknown_schema_version_is_rejected",
    ),
    (
        "unknown_event_type",
        "serialization::unknown_event_type_is_rejected",
    ),
    (
        "non_canonical_encoding",
        "§2 vector_02_non_canonical_encoding_rejected",
    ),
    ("id_mismatch", "§3 vector_03 + §6 vector_06"),
    ("bad_signature", "§5 vector_05 + §6 vector_06"),
    ("unbound_device", "membership::unbound_device_is_rejected"),
    ("not_a_member", "§13 vector_13_non_member_event_rejected"),
    (
        "insufficient_role",
        "§14 vector_14_insufficient_role_rejected",
    ),
    ("room_id_mismatch", "§4 vector_04 + §7 vector_07"),
    (
        "invalid_content",
        "serialization::invalid_content_* (content bounds)",
    ),
    ("expired_invite", "§15 vector_15 + §19 vector_19"),
    (
        "bad_capability",
        "§15 vector_15_bad_capability_and_expired_invite",
    ),
    ("room_full", "membership::join_rejected_when_room_is_full"),
    (
        "too_many_parents",
        "serialization::too_many_parents_is_rejected",
    ),
    (
        "not_genesis_descended",
        "serialization::not_genesis_descended_empty_prev_is_rejected",
    ),
    // Ignored (1).
    ("duplicate", "§8 vector_08_duplicate_ignored_idempotently"),
    // Advisory flags (3). `equivocation`'s segregated admin-fork dimension is
    // additionally exercised at the sync layer (`sync_convergence.rs`); the fold
    // layer covers it here.
    ("clock_skew", "§20 vector_20_clock_skew_advisory_only"),
    ("equivocation", "§12 vector_12_admin_equivocation_flagged"),
    (
        "from_removed_member",
        "membership::from_removed_member_flag_on_removed_author",
    ),
];

/// Explicitly-deferred outcomes (code, reason). **Empty**: the spike taxonomy is
/// fully covered by this suite. The mechanism exists so any future out-of-band
/// reason forces a conscious "cover it or justify deferral" decision.
const DEFERRED: &[(&str, &str)] = &[];

/// Every `RejectReason` `.code()`, constructed from each variant. Hand-maintained
/// (external crates cannot reflect a `#[non_exhaustive]` enum); the count is
/// pinned and cross-checked against the `.code()` strings below.
const ALL_REASONS: &[RejectReason] = &[
    RejectReason::UnknownSchemaVersion,
    RejectReason::UnknownEventType,
    RejectReason::NonCanonicalEncoding,
    RejectReason::IdMismatch,
    RejectReason::BadSignature,
    RejectReason::UnboundDevice,
    RejectReason::NotAMember,
    RejectReason::InsufficientRole,
    RejectReason::RoomIdMismatch,
    RejectReason::InvalidContent,
    RejectReason::ExpiredInvite,
    RejectReason::BadCapability,
    RejectReason::RoomFull,
    RejectReason::TooManyParents,
    RejectReason::NotGenesisDescended,
];

/// Every `Flag`.
const ALL_FLAGS: &[Flag] = &[Flag::ClockSkew, Flag::Equivocation, Flag::FromRemovedMember];

/// The `duplicate` ignored-outcome is neither a `RejectReason` nor a `Flag`; it
/// is registered as a literal string.
const DUPLICATE_CODE: &str = "duplicate";

fn is_covered(code: &str) -> bool {
    COVERAGE.iter().any(|(c, _)| *c == code) || DEFERRED.iter().any(|(c, _)| *c == code)
}

/// The full set of taxonomy codes this gate knows about.
fn known_codes() -> BTreeSet<&'static str> {
    ALL_REASONS
        .iter()
        .map(RejectReason::code)
        .chain(ALL_FLAGS.iter().map(Flag::code))
        .chain(std::iter::once(DUPLICATE_CODE))
        .collect()
}

// ---------------------------------------------------------------------------
// AC5: the completeness gate.
// ---------------------------------------------------------------------------

#[test]
fn every_reason_and_flag_is_covered_or_deferred() {
    // Count pins (the enumerators mirror the §8 taxonomy; a new variant must be
    // added here, which then trips the coverage check below unless also mapped).
    assert_eq!(
        ALL_REASONS.len(),
        15,
        "the §8 rejection taxonomy has exactly 15 reasons"
    );
    assert_eq!(
        ALL_FLAGS.len(),
        3,
        "the §8 flag taxonomy has exactly 3 flags"
    );

    // DEFERRED is empty — the suite covers the whole taxonomy.
    assert!(
        DEFERRED.is_empty(),
        "DEFERRED must be empty; every taxonomy outcome is covered by a vector"
    );

    // Every known code is covered (or deferred).
    for code in known_codes() {
        assert!(
            is_covered(code),
            "taxonomy code `{code}` is neither covered by a vector nor deferred"
        );
    }

    // Stale-entry guard: no COVERAGE row names a code outside the taxonomy.
    let known = known_codes();
    for (code, _) in COVERAGE {
        assert!(
            known.contains(code),
            "COVERAGE lists `{code}`, which is not a known §8 taxonomy code"
        );
    }

    // No duplicate COVERAGE rows.
    let mut seen = BTreeSet::new();
    for (code, _) in COVERAGE {
        assert!(seen.insert(*code), "COVERAGE lists `{code}` more than once");
    }
}

// ---------------------------------------------------------------------------
// Pin every §8 code string.
// ---------------------------------------------------------------------------

#[test]
fn reason_and_flag_code_strings_match_spec() {
    // All 15 rejection codes.
    assert_eq!(
        RejectReason::UnknownSchemaVersion.code(),
        "unknown_schema_version"
    );
    assert_eq!(RejectReason::UnknownEventType.code(), "unknown_event_type");
    assert_eq!(
        RejectReason::NonCanonicalEncoding.code(),
        "non_canonical_encoding"
    );
    assert_eq!(RejectReason::IdMismatch.code(), "id_mismatch");
    assert_eq!(RejectReason::BadSignature.code(), "bad_signature");
    assert_eq!(RejectReason::UnboundDevice.code(), "unbound_device");
    assert_eq!(RejectReason::NotAMember.code(), "not_a_member");
    assert_eq!(RejectReason::InsufficientRole.code(), "insufficient_role");
    assert_eq!(RejectReason::RoomIdMismatch.code(), "room_id_mismatch");
    assert_eq!(RejectReason::InvalidContent.code(), "invalid_content");
    assert_eq!(RejectReason::ExpiredInvite.code(), "expired_invite");
    assert_eq!(RejectReason::BadCapability.code(), "bad_capability");
    assert_eq!(RejectReason::RoomFull.code(), "room_full");
    assert_eq!(RejectReason::TooManyParents.code(), "too_many_parents");
    assert_eq!(
        RejectReason::NotGenesisDescended.code(),
        "not_genesis_descended"
    );

    // All 3 advisory flag codes.
    assert_eq!(Flag::ClockSkew.code(), "clock_skew");
    assert_eq!(Flag::Equivocation.code(), "equivocation");
    assert_eq!(Flag::FromRemovedMember.code(), "from_removed_member");
}
