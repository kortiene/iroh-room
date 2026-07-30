//! Shared sub-types for the v2 governance log model (#134 §7, issue #147).
//!
//! These are the deterministic record/config types used by the genesis config,
//! the §7.3 operation payloads, and the six-component [`super::state`]
//! projection. Each type canonicalizes to closed deterministic CBOR
//! ([`crate::cbor`]) so the state-root computation is byte-deterministic.

use std::collections::{BTreeMap, BTreeSet};

use crate::cbor::CborValue;
use crate::error::Reject;
use crate::ids::{DeviceId, GovernanceId, PrincipalId, ReplicaId, StateRoot, StreamId};

// ----------------------------------------------------------------------------
// Enums (role + statuses). Encoded as canonical text strings.
// ----------------------------------------------------------------------------

/// A member's role (spec §7.3 `member.grant` / `device.grant`). `Ord` models
/// privilege: admin is most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// A read/write member.
    Member,
    /// A privileged agent principal.
    Agent,
    /// A community administrator.
    Admin,
}

impl Role {
    /// The canonical wire string for this role.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
            Self::Admin => "admin",
        }
    }

    /// Parse a role from its canonical wire string.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for an unknown role string.
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "member" => Ok(Self::Member),
            "agent" => Ok(Self::Agent),
            "admin" => Ok(Self::Admin),
            _ => Err(Reject::InvalidContent),
        }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Text(self.as_str().to_owned())
    }
}

/// A member's lifecycle status (spec §7.3 `member.grant`/`member.revoke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberStatus {
    /// Active member.
    Active,
    /// Revoked/inactive (tombstoned for deterministic replay).
    Revoked,
}

impl MemberStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }

    /// Parse a member status from its canonical wire string.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for an unknown status string.
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "active" => Ok(Self::Active),
            "revoked" => Ok(Self::Revoked),
            _ => Err(Reject::InvalidContent),
        }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Text(self.as_str().to_owned())
    }
}

/// A device's lifecycle status (spec §7.3 `device.grant`/`device.revoke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceStatus {
    /// Active device.
    Active,
    /// Revoked device (tombstoned).
    Revoked,
}

impl DeviceStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }

    /// Parse a device status from its canonical wire string.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for an unknown status string.
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "active" => Ok(Self::Active),
            "revoked" => Ok(Self::Revoked),
            _ => Err(Reject::InvalidContent),
        }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Text(self.as_str().to_owned())
    }
}

/// A replica's lifecycle status (spec §7.3 `replica.set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReplicaStatus {
    /// Active replica.
    Active,
    /// Disabled replica (kept in the manifest for deterministic replay).
    Disabled,
}

impl ReplicaStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    /// Parse a replica status from its canonical wire string.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for an unknown status string.
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            _ => Err(Reject::InvalidContent),
        }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Text(self.as_str().to_owned())
    }
}

// ----------------------------------------------------------------------------
// Config types (also used as state components / payload fields).
// ----------------------------------------------------------------------------

/// The recovery configuration (spec §7.3 `recovery.set`). A threshold of the
/// listed recovery principals may authorize recovery actions (#148).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// Required recovery-principal count.
    pub threshold: u16,
    /// Recovery principal set (canonicalized to sorted unique order).
    pub recovery_keys: Vec<PrincipalId>,
}

impl RecoveryConfig {
    /// An empty recovery config (no recovery principals).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            threshold: 0,
            recovery_keys: Vec::new(),
        }
    }

    /// Canonicalize in place: sort + dedup the recovery-key set.
    pub fn canonicalize(&mut self) {
        self.recovery_keys.sort();
        self.recovery_keys.dedup();
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "threshold".to_owned(),
                CborValue::Uint(u64::from(self.threshold)),
            ),
            (
                "recovery_keys".to_owned(),
                CborValue::Array(
                    self.recovery_keys
                        .iter()
                        .map(|p| CborValue::Bytes(p.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
        ])
    }

    /// Decode from canonical CBOR, enforcing the closed schema (spec D8).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the value is not a closed-schema
    /// map or a field has the wrong shape/width.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        super::reject_unknown_keys(
            entries,
            &["threshold", "recovery_keys"],
            Reject::InvalidContent,
        )?;
        let threshold = super::read_u16_field(entries, "threshold")?;
        let recovery_keys = super::read_principal_array(entries, "recovery_keys")?;
        Ok(Self {
            threshold,
            recovery_keys,
        })
    }
}

/// A replica descriptor (spec §7.3 `replica.set`, genesis bootstrap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaDescriptor {
    /// The replica identifier.
    pub replica_id: ReplicaId,
    /// Opaque endpoint bytes (transport-specific; #151 owns the shape).
    pub endpoint: Vec<u8>,
    /// Opaque capability byte.
    pub capability: u8,
}

impl ReplicaDescriptor {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "replica_id".to_owned(),
                CborValue::Bytes(self.replica_id.as_bytes().to_vec()),
            ),
            (
                "endpoint".to_owned(),
                CborValue::Bytes(self.endpoint.clone()),
            ),
            (
                "capability".to_owned(),
                CborValue::Uint(u64::from(self.capability)),
            ),
        ])
    }

    /// Decode from canonical CBOR, enforcing the closed schema (spec D8).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the value is not a closed-schema
    /// map or a field has the wrong shape/width.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        super::reject_unknown_keys(
            entries,
            &["replica_id", "endpoint", "capability"],
            Reject::InvalidContent,
        )?;
        let replica_id = super::read_replica_field(entries, "replica_id")?;
        let endpoint = super::read_bytes_field(entries, "endpoint")?.to_owned();
        let capability = super::read_u8_field(entries, "capability")?;
        Ok(Self {
            replica_id,
            endpoint,
            capability,
        })
    }
}

/// A content-stream policy (spec §7.3 `stream.create`/`stream.policy_set`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPolicy {
    /// Opaque access-level byte (interpreted by #152).
    pub access: u8,
}

impl StreamPolicy {
    /// The default stream policy (access level 0).
    #[must_use]
    pub fn default_policy() -> Self {
        Self { access: 0 }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![(
            "access".to_owned(),
            CborValue::Uint(u64::from(self.access)),
        )])
    }

    /// Decode from canonical CBOR, enforcing the closed schema (spec D8).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the value is not a closed-schema
    /// map or a field has the wrong shape/width.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        super::reject_unknown_keys(entries, &["access"], Reject::InvalidContent)?;
        let access = super::read_u8_field(entries, "access")?;
        Ok(Self { access })
    }
}

/// A deterministic state-visible `fork.resolve` marker (spec §6.6 / §7.3,
/// issue #149). Committed under [`CommunityPolicy::fork_markers`] so a
/// successful resolution is state-root-visible without carrying full audit
/// evidence into the six-component root.
///
/// The marker commits to the resolution facts: every named competing branch
/// head, the selected head, and the selected state root. It deliberately
/// omits a `resolution_entry` field: that id is derived from the entry's CSB,
/// which includes the post-marker state root, so including it here would
/// create a self-referential derivation (spec §6.6). The resolution entry id
/// is retained only in the outer audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedForkMarker {
    /// Every known competing branch head at resolution time, sorted ascending
    /// by raw id (canonical representation only; sorted position is never
    /// authorization — spec §5.6).
    pub branch_heads: Vec<GovernanceId>,
    /// The selected branch head (must occur exactly once in `branch_heads`).
    pub selected_head: GovernanceId,
    /// The already-validated state root for `selected_head`.
    pub selected_state_root: StateRoot,
    /// Signed creation time (advisory; never a wall clock).
    pub created_at_ms: u64,
}

impl ResolvedForkMarker {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "branch_heads".to_owned(),
                CborValue::Array(
                    self.branch_heads
                        .iter()
                        .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            (
                "selected_head".to_owned(),
                CborValue::Bytes(self.selected_head.as_bytes().to_vec()),
            ),
            (
                "selected_state_root".to_owned(),
                CborValue::Bytes(self.selected_state_root.as_bytes().to_vec()),
            ),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
        ])
    }

    /// Decode from canonical CBOR, enforcing the closed schema and the
    /// canonical branch-head invariants (sorted ascending, unique, at least
    /// two, and `selected_head` present exactly once).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for a non-closed schema, a malformed
    /// field, or a non-canonical branch-head set.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        super::reject_unknown_keys(
            entries,
            &[
                "branch_heads",
                "selected_head",
                "selected_state_root",
                "created_at_ms",
            ],
            Reject::InvalidContent,
        )?;
        let (branch_heads, selected_head) =
            super::read_canonical_branch_set(entries, Reject::InvalidContent)?;
        let selected_state_root = super::read_state_root_field(entries, "selected_state_root")?;
        let created_at_ms = super::read_uint_field(entries, "created_at_ms")?;
        Ok(Self {
            branch_heads,
            selected_head,
            selected_state_root,
            created_at_ms,
        })
    }
}

/// The community policy component (spec §7.1 component 6 / §7.3 `policy.set`,
/// `invite.revoke`, `fork.resolve`, `migration.accept`). Revocations, fork
/// markers, and migration acceptances are housed here so the state-root record
/// stays at the six §7.1 components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityPolicy {
    /// Revoked invite commitments (deterministic order via `BTreeSet`).
    pub revoked_invites: BTreeSet<[u8; 32]>,
    /// `fork.resolve` markers (canonicalized to sorted order at encode time).
    /// Append-only history of resolved forks (issue #149).
    pub fork_markers: Vec<ResolvedForkMarker>,
    /// Accepted migration ids (deterministic order via `BTreeSet`).
    pub migrations: BTreeSet<[u8; 32]>,
}

impl CommunityPolicy {
    /// An empty community policy.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            revoked_invites: BTreeSet::new(),
            fork_markers: Vec::new(),
            migrations: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        let mut markers = self.fork_markers.clone();
        markers.sort_by_cached_key(|marker| crate::cbor::encode(&marker.to_cbor()));
        CborValue::Map(vec![
            (
                "revoked_invites".to_owned(),
                CborValue::Array(
                    self.revoked_invites
                        .iter()
                        .map(|id| CborValue::Bytes(id.to_vec()))
                        .collect(),
                ),
            ),
            (
                "fork_markers".to_owned(),
                CborValue::Array(markers.iter().map(ResolvedForkMarker::to_cbor).collect()),
            ),
            (
                "migrations".to_owned(),
                CborValue::Array(
                    self.migrations
                        .iter()
                        .map(|id| CborValue::Bytes(id.to_vec()))
                        .collect(),
                ),
            ),
        ])
    }

    /// Decode from canonical CBOR, enforcing the closed schema (spec D8).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the value is not a closed-schema
    /// map or a field has the wrong shape/width.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        super::reject_unknown_keys(
            entries,
            &["revoked_invites", "fork_markers", "migrations"],
            Reject::InvalidContent,
        )?;
        let revoked_invites = super::read_byte_array_set(entries, "revoked_invites")?;
        let fork_markers = super::read_marker_array(entries, "fork_markers")?;
        let migrations = super::read_byte_array_set(entries, "migrations")?;
        Ok(Self {
            revoked_invites,
            fork_markers,
            migrations,
        })
    }
}

// ----------------------------------------------------------------------------
// State sub-types (the live components folded from accepted governance state).
// ----------------------------------------------------------------------------

/// The administrator component (spec §7.1 component 1 / §7.3 `admin.set`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministratorState {
    /// Administrator principal set (sorted unique).
    pub administrators: Vec<PrincipalId>,
    /// The signature threshold required to authorize admin-gated actions.
    pub threshold: u16,
}

impl AdministratorState {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "administrators".to_owned(),
                CborValue::Array(
                    self.administrators
                        .iter()
                        .map(|p| CborValue::Bytes(p.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            (
                "threshold".to_owned(),
                CborValue::Uint(u64::from(self.threshold)),
            ),
        ])
    }
}

/// The recovery component (spec §7.1 component 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryState {
    /// The current recovery configuration.
    pub config: RecoveryConfig,
}

impl RecoveryState {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        self.config.to_cbor()
    }
}

/// A replica record in the replicas component (spec §7.1 component 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaRecord {
    /// The replica descriptor.
    pub descriptor: ReplicaDescriptor,
    /// The replica's current status.
    pub status: ReplicaStatus,
}

impl ReplicaRecord {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            ("descriptor".to_owned(), self.descriptor.to_cbor()),
            ("status".to_owned(), self.status.to_cbor()),
        ])
    }
}

/// A device record (members/devices/roles component).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device identifier.
    pub device_id: DeviceId,
    /// The device's current status.
    pub status: DeviceStatus,
    /// Canonical device-binding metadata committed by the member projection.
    pub binding: Vec<u8>,
}

impl DeviceRecord {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "device_id".to_owned(),
                CborValue::Bytes(self.device_id.as_bytes().to_vec()),
            ),
            ("status".to_owned(), self.status.to_cbor()),
            ("binding".to_owned(), CborValue::Bytes(self.binding.clone())),
        ])
    }
}

/// A member record (spec §7.1 component 4: members/devices/roles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The principal identity.
    pub member_id: PrincipalId,
    /// The member's canonical role set.
    pub roles: Vec<Role>,
    /// The member's status.
    pub status: MemberStatus,
    /// Devices bound to this member (deterministic `BTreeMap` order).
    pub devices: BTreeMap<DeviceId, DeviceRecord>,
    /// Sequence of the genesis grant or most recent regrant.
    pub grant_seq: u64,
    /// Sequence of revocation, present exactly while revoked.
    pub revoke_seq: Option<u64>,
    /// Optional opaque profile reference; it has no authorization meaning.
    pub profile: Option<Vec<u8>>,
}

impl MemberRecord {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        let mut roles = self.roles.clone();
        roles.sort_by_key(Role::as_str);
        roles.dedup();
        let mut entries = vec![
            (
                "member_id".to_owned(),
                CborValue::Bytes(self.member_id.as_bytes().to_vec()),
            ),
            (
                "roles".to_owned(),
                CborValue::Array(roles.iter().map(Role::to_cbor).collect()),
            ),
            ("status".to_owned(), self.status.to_cbor()),
            (
                "devices".to_owned(),
                CborValue::Array(self.devices.values().map(DeviceRecord::to_cbor).collect()),
            ),
            ("grant_seq".to_owned(), CborValue::Uint(self.grant_seq)),
        ];
        if let Some(revoke_seq) = self.revoke_seq {
            entries.push(("revoke_seq".to_owned(), CborValue::Uint(revoke_seq)));
        }
        if let Some(profile) = &self.profile {
            entries.push(("profile".to_owned(), CborValue::Bytes(profile.clone())));
        }
        CborValue::Map(entries)
    }
}

/// A content-stream record (spec §7.1 component 5: stream manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecord {
    /// The stream identifier.
    pub stream_id: StreamId,
    /// The stream's policy.
    pub policy: StreamPolicy,
    /// Whether the stream is archived.
    pub archived: bool,
    /// Signed creation time (advisory).
    pub created_at_ms: u64,
}

impl StreamRecord {
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "stream_id".to_owned(),
                CborValue::Bytes(self.stream_id.as_bytes().to_vec()),
            ),
            ("policy".to_owned(), self.policy.to_cbor()),
            (
                "archived".to_owned(),
                CborValue::Uint(u64::from(self.archived)),
            ),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::LEN;

    #[test]
    fn role_round_trip() {
        assert_eq!(Role::parse("admin").unwrap(), Role::Admin);
        assert_eq!(Role::parse("bogus").err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn community_policy_marker_encoding_is_permutation_independent() {
        let selected = GovernanceId::from_bytes([1; LEN]);
        let marker_a = ResolvedForkMarker {
            branch_heads: vec![selected, GovernanceId::from_bytes([2; LEN])],
            selected_head: selected,
            selected_state_root: StateRoot::from_bytes([3; LEN]),
            created_at_ms: 10,
        };
        let marker_b = ResolvedForkMarker {
            branch_heads: vec![selected, GovernanceId::from_bytes([4; LEN])],
            selected_head: selected,
            selected_state_root: StateRoot::from_bytes([5; LEN]),
            created_at_ms: 20,
        };
        let mut first = CommunityPolicy::empty();
        first.fork_markers = vec![marker_a.clone(), marker_b.clone()];
        let mut second = CommunityPolicy::empty();
        second.fork_markers = vec![marker_b, marker_a];
        assert_eq!(first.to_cbor(), second.to_cbor());
    }

    #[test]
    fn recovery_config_canonicalizes_keys() {
        let a = PrincipalId::from_bytes([1; LEN]);
        let b = PrincipalId::from_bytes([2; LEN]);
        let mut cfg = RecoveryConfig {
            threshold: 1,
            recovery_keys: vec![b, a, a],
        };
        cfg.canonicalize();
        assert_eq!(cfg.recovery_keys, vec![a, b]);
    }
}
