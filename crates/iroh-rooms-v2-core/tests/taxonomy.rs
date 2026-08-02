//! Taxonomy-completeness guard (spec §9 / §10): every public rejection code in
//! [`iroh_rooms_v2_core::Reject`] must be reachable and named here, so a new code
//! cannot land without a covering reference (the v1 §10 tripwire discipline).
//!
//! This file is the canonical completeness gate. It closes TWO loops:
//! - **Naming** — `EXPECTED` ↔ `error::all_codes()` ↔ the enum agree (`every_
//!   rejection_code_is_named_and_consistent`), and codes are stable `snake_case`.
//! - **Reachability** — every code is actually constructed by some public path
//!   in `src/` (so it is genuinely exercisable), or is an explicit member of
//!   `BLOCKED_CODES` (`every_code_is_reachable_or_explicitly_blocked`). This
//!   catches a code that is named but never wired up.
//!
//! The frozen byte-exact *exercise* vectors for every reachable code live in
//! `signed_records_golden.rs` (#153); this file asserts the taxonomy itself is
//! real and closed, not that every code is re-exercised here.

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

/// Codes declared in the taxonomy but emitted by NO public path today. These are
/// the only codes that may be absent from the crate's construction sites; the
/// reachability test below proves every other code is actually wired up. MUST
/// stay in sync with `BLOCKED_CODES` in `signed_records_golden.rs` (both are
/// checked, so they cannot drift silently).
const BLOCKED_CODES: &[&str] = &["wrong_domain"];

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

/// Reachability-completeness gate (spec §9 / §10): every named code must be
/// constructed by some public path in `src/` — so it is genuinely exercisable
/// by a #153 vector — unless it is an explicit `BLOCKED_CODES` entry (declared
/// but unreachable, with the gap itself asserted). This catches a code that is
/// named in `EXPECTED` / `all_codes()` but never actually wired up, closing the
/// gap that a naming-only guard leaves open.
#[test]
fn every_code_is_reachable_or_explicitly_blocked() {
    for code in EXPECTED.iter().map(|(_, c)| *c) {
        let variant = code_to_variant(code);
        let constructed = variant_constructed_in_src(&variant);
        if BLOCKED_CODES.contains(&code) {
            assert!(
                !constructed,
                "{variant} ({code}) is listed in BLOCKED_CODES but is now constructed in src/ — \
                 remove it from BLOCKED_CODES (and here) and add a real vector"
            );
        } else {
            assert!(
                constructed,
                "{variant} ({code}) is not in BLOCKED_CODES but is never constructed in src/ — \
                 either wire it to a public path or add it to BLOCKED_CODES"
            );
        }
    }
    // Every blocked code must be a known taxonomy code (no phantom blocks).
    for &code in BLOCKED_CODES {
        assert!(
            EXPECTED.iter().any(|(_, c)| c == &code),
            "BLOCKED_CODES lists {code:?}, which is not a known taxonomy code"
        );
    }
}

/// Convert a reject `code` (`snake_case`) to its enum variant name (`CamelCase`),
/// e.g. `wrong_domain` → `WrongDomain`.
fn code_to_variant(code: &str) -> String {
    code.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Heuristic reachability check: scan the crate's `src/` for a construction site
/// `Reject::<Variant>` outside `error.rs` (the declaration site). Returns true if
/// any exists. Mirrors the `code_is_constructed_in_lib` discipline in
/// `signed_records_golden.rs`; intentionally conservative.
fn variant_constructed_in_src(variant: &str) -> bool {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = std::path::Path::new(manifest).join("src");
    walk_and_find(&src, &format!("Reject::{variant}"))
}

fn walk_and_find(dir: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if walk_and_find(&path, needle) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_none_or(|n| n != "error.rs")
        {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text.contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}
