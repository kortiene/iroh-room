//! Typed rejection/error taxonomy (spec §7, issue #146/#147/#148/#149/#150/#152).
//!
//! The crate is pure and never logs directly. Every failure surfaces as a
//! [`Reject`] carrying a stable `.code()` string so downstream CLI/runtime layers
//! can map failures without parsing display text. The taxonomy is
//! `#[non_exhaustive]`: future versions may add codes, so downstream matches must
//! keep a fallback arm.

use core::fmt;

/// A structured rejection from a v2 core operation (spec §7 table).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reject {
    /// CBOR malformed or outside the deterministic profile (spec §7).
    NonCanonicalEncoding,
    /// Unsupported schema/protocol version.
    UnknownVersion,
    /// Record kind not in the closed registry.
    UnknownRecordKind,
    /// Content body kind not in the v2 registry (spec D8 / #152 §6.4 rule).
    UnknownContentKind,
    /// Missing / wrong / extra / out-of-bound body field.
    InvalidContent,
    /// Envelope id does not match the domain-separated hash of signed bytes.
    IdMismatch,
    /// Ed25519 verification failed under the signer/device key.
    BadSignature,
    /// Bytes valid under another signed-record domain but not this one.
    WrongDomain,
    /// Parent/entry/checkpoint dependency not supplied to the pure fold.
    MissingDependency,
    /// Signer/approval set cannot authorize the action.
    InsufficientAuthorization,
    /// Approval references the wrong entry/root, duplicates a signer, has a bad
    /// signature, or is stale.
    InvalidApproval,
    /// Conflicting branches/evidence detected (spec #149).
    ForkDetected,
    /// Operation depends on unresolved fork state and must fail closed.
    UnresolvedFork,
    /// Malformed or unauthorized `fork.resolve`.
    InvalidForkResolution,
    /// Supplied root differs from the recomputed state root.
    StateRootMismatch,
    /// Checkpoint/snapshot hash differs from the recomputed hash.
    SnapshotHashMismatch,
    /// Merkle proof does not verify against the root.
    InvalidMerkleProof,
}

impl Reject {
    /// The stable, machine-readable rejection code (spec §7 "expose
    /// machine-readable `.code()` strings").
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NonCanonicalEncoding => "non_canonical_encoding",
            Self::UnknownVersion => "unknown_version",
            Self::UnknownRecordKind => "unknown_record_kind",
            Self::UnknownContentKind => "unknown_content_kind",
            Self::InvalidContent => "invalid_content",
            Self::IdMismatch => "id_mismatch",
            Self::BadSignature => "bad_signature",
            Self::WrongDomain => "wrong_domain",
            Self::MissingDependency => "missing_dependency",
            Self::InsufficientAuthorization => "insufficient_authorization",
            Self::InvalidApproval => "invalid_approval",
            Self::ForkDetected => "fork_detected",
            Self::UnresolvedFork => "unresolved_fork",
            Self::InvalidForkResolution => "invalid_fork_resolution",
            Self::StateRootMismatch => "state_root_mismatch",
            Self::SnapshotHashMismatch => "snapshot_hash_mismatch",
            Self::InvalidMerkleProof => "invalid_merkle_proof",
        }
    }
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Reject {}

impl From<crate::cbor::CborError> for Reject {
    fn from(_: crate::cbor::CborError) -> Self {
        Self::NonCanonicalEncoding
    }
}

/// The complete set of stable rejection codes, used by the taxonomy-completeness
/// test (spec §9 / §10) so a new code cannot land without a covering vector.
#[must_use]
pub fn all_codes() -> Vec<&'static str> {
    [
        Reject::NonCanonicalEncoding,
        Reject::UnknownVersion,
        Reject::UnknownRecordKind,
        Reject::UnknownContentKind,
        Reject::InvalidContent,
        Reject::IdMismatch,
        Reject::BadSignature,
        Reject::WrongDomain,
        Reject::MissingDependency,
        Reject::InsufficientAuthorization,
        Reject::InvalidApproval,
        Reject::ForkDetected,
        Reject::UnresolvedFork,
        Reject::InvalidForkResolution,
        Reject::StateRootMismatch,
        Reject::SnapshotHashMismatch,
        Reject::InvalidMerkleProof,
    ]
    .iter()
    .map(Reject::code)
    .collect()
}
