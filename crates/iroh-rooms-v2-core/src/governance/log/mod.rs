//! The normative v2 governance-log foundation: `GenesisConfig`, the
//! `GovernanceEntryBody` / `GovernanceApproval` / `GovernanceEntry` record
//! types, the closed #134 §7.3 operation registry, the six-component
//! [`state::GovernanceState`] and [`state::GovernanceStateRootRecord`], and the
//! pure-deterministic `apply(old_state, op) -> new_state` functions (spec
//! `v2-governance-log-entry-approval-state-root.md`, issue #147).
//!
//! This module implements the #134 §7.1–§7.3 governance-log foundation in the
//! pure v2 core crate. It is **additive** to the earlier candidate governance
//! scaffolding in the sibling modules (`super::model`, `super::approval`, …):
//! that scaffolding carries the frozen #153 signed-record golden vectors and
//! remains the candidate path until a deliberate, reviewable migration lands.
//! New normative code MUST use the frozen #146 domains
//! (`domain::GOVERNANCE_ENTRY`, `domain::GOVERNANCE_APPROVAL`,
//! `domain::GOVERNANCE_STATE`) and the #146 names (`CommunityId`,
//! `GovernanceId`), which this module does exclusively.
//!
//! # Scope (spec §3)
//!
//! In scope: genesis threshold verification, the entry/approval records, the
//! §7.3 operation registry with pure apply functions, the six-component
//! state-root record, and unknown-operation rejection. Out of scope (and
//! deferred to later issues): authorization policy (#148), fork handling
//! (#149), checkpoints/snapshots (#150), and any network/replica/storage code.

pub mod genesis;
pub mod model;
pub mod operation;
pub mod records;
pub mod state;

pub use genesis::{
    derive_community_id, genesis_config_csb, sign_genesis, verify_genesis, GenesisConfig,
    GenesisSignature, GENESIS_SCHEMA_VERSION,
};
pub use model::{
    AdministratorState, CommunityPolicy, DeviceRecord, DeviceStatus, ForkResolutionMarker,
    MemberRecord, MemberStatus, RecoveryConfig, RecoveryState, ReplicaDescriptor, ReplicaRecord,
    ReplicaStatus, Role, StreamPolicy, StreamRecord,
};
pub use operation::{
    AdminSet, DeviceGrant, DeviceRevoke, GovernanceOperationKind, GovernanceOperationPayload,
    InviteRevoke, MemberGrant, MemberRevoke, MigrationAccept, PolicySet, RecoverySet, ReplicaSet,
    StreamArchive, StreamCreate, StreamPolicySet,
};
pub use records::{
    approval_csb, approval_id, decode_entry_csb, entry_csb, entry_id, verify_approval_crypto,
    verify_entry_crypto, verify_entry_full, GovernanceApproval, GovernanceApprovalBody,
    GovernanceEntry, GovernanceEntryBody,
};
pub use state::{
    apply, apply_verified_entry, check_chain_link, component_root, compute_state_root,
    governance_state_root_record, verify_state_root, GovernanceState, GovernanceStateRootRecord,
    COMPONENT_LABELS,
};

// ----------------------------------------------------------------------------
// Shared canonical-CBOR field helpers (private to this module).
//
// These mirror the discipline of `signed::field` / `governance::model::*` but
// surface a configurable `Reject` code so decode sites can choose
// `NonCanonicalEncoding` (top-level body shape) vs `InvalidContent` (nested
// payload shape) as the spec's error table recommends.
// ----------------------------------------------------------------------------

use crate::cbor::CborValue;
use crate::error::Reject;
use crate::ids::{
    CommunityId, DeviceId, GovernanceId, PrincipalId, ReplicaId, StateRoot, StreamId, LEN,
};

/// Look up a required map field, returning `None` when absent.
pub(super) fn opt_field<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Option<&'a CborValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Read a required `u64` field.
pub(super) fn read_uint_field(entries: &[(String, CborValue)], key: &str) -> Result<u64, Reject> {
    opt_field(entries, key)
        .and_then(CborValue::as_uint)
        .ok_or(Reject::NonCanonicalEncoding)
}

/// Read a required `u64` field and narrow it to `u8`, rejecting overflow as
/// [`Reject::InvalidContent`] (avoids the `as u8` truncation lint).
pub(super) fn read_u8_field(entries: &[(String, CborValue)], key: &str) -> Result<u8, Reject> {
    u8::try_from(read_uint_field(entries, key)?).map_err(|_| Reject::InvalidContent)
}

/// Read a required `u64` field and narrow it to `u16`, rejecting overflow as
/// [`Reject::InvalidContent`] (avoids the `as u16` truncation lint).
pub(super) fn read_u16_field(entries: &[(String, CborValue)], key: &str) -> Result<u16, Reject> {
    u16::try_from(read_uint_field(entries, key)?).map_err(|_| Reject::InvalidContent)
}

/// Read a required text field.
pub(super) fn read_text_field<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Result<&'a str, Reject> {
    opt_field(entries, key)
        .and_then(CborValue::as_text)
        .ok_or(Reject::NonCanonicalEncoding)
}

/// Read a required byte-string field.
pub(super) fn read_bytes_field<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Result<&'a [u8], Reject> {
    opt_field(entries, key)
        .and_then(CborValue::as_bytes)
        .ok_or(Reject::NonCanonicalEncoding)
}

/// Reject any map key outside the allowed set, surfacing `code` (spec D8).
pub(super) fn reject_unknown_keys(
    entries: &[(String, CborValue)],
    allowed: &[&str],
    code: Reject,
) -> Result<(), Reject> {
    for (k, _) in entries {
        if !allowed.contains(&k.as_str()) {
            return Err(code);
        }
    }
    Ok(())
}

pub(super) fn read_principal_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<PrincipalId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(PrincipalId::from_bytes(arr))
}

pub(super) fn read_device_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<DeviceId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(DeviceId::from_bytes(arr))
}

pub(super) fn read_community_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<CommunityId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(CommunityId::from_bytes(arr))
}

pub(super) fn read_governance_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<GovernanceId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(GovernanceId::from_bytes(arr))
}

pub(super) fn read_replica_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<ReplicaId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(ReplicaId::from_bytes(arr))
}

pub(super) fn read_stream_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<StreamId, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(StreamId::from_bytes(arr))
}

pub(super) fn read_state_root_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<StateRoot, Reject> {
    let bytes = read_bytes_field(entries, key)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(StateRoot::from_bytes(arr))
}

pub(super) fn read_fixed32_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<[u8; LEN], Reject> {
    let bytes = read_bytes_field(entries, key)?;
    <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)
}

pub(super) fn read_principal_array(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<Vec<PrincipalId>, Reject> {
    let arr = opt_field(entries, key)
        .and_then(CborValue::as_array)
        .ok_or(Reject::NonCanonicalEncoding)?;
    arr.iter()
        .map(|v| {
            let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
            let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
            Ok(PrincipalId::from_bytes(arr))
        })
        .collect()
}

pub(super) fn read_replica_array(value: &CborValue) -> Result<Vec<ReplicaDescriptor>, Reject> {
    let arr = value.as_array().ok_or(Reject::NonCanonicalEncoding)?;
    let mut out: Vec<ReplicaDescriptor> = arr
        .iter()
        .map(ReplicaDescriptor::from_canonical)
        .collect::<Result<_, _>>()?;
    // Enforce sorted-by-id + unique (spec §5.1).
    let mut sorted = out.clone();
    sorted.sort_by_key(|r| *r.replica_id.as_bytes());
    sorted.dedup_by_key(|r| *r.replica_id.as_bytes());
    if sorted.len() != out.len() {
        return Err(Reject::NonCanonicalEncoding);
    }
    out.sort_by_key(|r| *r.replica_id.as_bytes());
    Ok(out)
}

pub(super) fn read_byte_array_set(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<std::collections::BTreeSet<[u8; LEN]>, Reject> {
    let arr = opt_field(entries, key)
        .and_then(CborValue::as_array)
        .ok_or(Reject::NonCanonicalEncoding)?;
    let mut set = std::collections::BTreeSet::new();
    for v in arr {
        let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
        let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
        if !set.insert(arr) {
            return Err(Reject::NonCanonicalEncoding);
        }
    }
    Ok(set)
}

pub(super) fn read_governance_id_pair(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<[GovernanceId; 2], Reject> {
    let arr = opt_field(entries, key)
        .and_then(CborValue::as_array)
        .ok_or(Reject::NonCanonicalEncoding)?;
    if arr.len() != 2 {
        return Err(Reject::InvalidContent);
    }
    let mut out = [GovernanceId::from_bytes([0; LEN]); 2];
    for (i, v) in arr.iter().enumerate() {
        let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
        let b = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
        out[i] = GovernanceId::from_bytes(b);
    }
    Ok(out)
}

pub(super) fn read_marker_array(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<Vec<model::ForkResolutionMarker>, Reject> {
    let arr = opt_field(entries, key)
        .and_then(CborValue::as_array)
        .ok_or(Reject::NonCanonicalEncoding)?;
    arr.iter()
        .map(model::ForkResolutionMarker::from_canonical)
        .collect()
}
