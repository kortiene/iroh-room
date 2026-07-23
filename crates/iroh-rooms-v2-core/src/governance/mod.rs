//! The governance layer: record model, authorization, deterministic fold, fork
//! detection, state roots, and checkpoints (spec §6.3–§6.5 / #147–#150).
//!
//! This module is pure: it ingests already-verified records and produces
//! deterministic state. Signature/canonical verification happens at the
//! [`crate::signed`] boundary; the fold trusts records it receives from callers
//! that have run `decode_verified`.

pub mod approval;
pub mod authz;
pub mod checkpoint;
pub mod fold;
pub mod fork;
/// The normative v2 governance-log foundation (#134 §7.1–§7.3, issue #147):
/// `GenesisConfig`, the entry/approval records, the closed §7.3 operation
/// registry, the six-component state + state-root record, and pure apply
/// functions. Additive to the candidate scaffolding above; uses the frozen
/// #146 domains and `CommunityId`/`GovernanceId` names.
pub mod log;
pub mod model;
pub mod state_root;

pub use approval::{ApprovalBody, SignedApproval};
pub use authz::{
    authorize_admin_only, authorize_content_body, authorize_governance_entry, is_active_with_role,
    role_of, ApprovalSet,
};
pub use checkpoint::{
    decode_verified as decode_checkpoint, snapshot_hash, validate_against_state, CheckpointBody,
    SignedCheckpoint,
};
pub use fold::{approval_id, entry_id, FoldItem, FoldOutcome, GovernanceFold};
pub use fork::{
    decode_verified as decode_fork_resolution, detect as detect_fork, ForkResolutionBody,
    ForkResolveAction, SignedForkResolution,
};
pub use model::{
    ApprovalPolicy, ForkEvidence, GovernanceAction, GovernanceEntryBody, GovernanceState,
    MemberRecord, MemberStatus, Role, SignedGovernanceEntry, SCHEMA_VERSION,
};
pub use state_root::{
    canonical_state_bytes, canonical_state_value, compute as compute_state_root,
    verify as verify_state_root,
};
