//! The closed #134 §7.3 governance-operation registry (issue #147).
//!
//! Exactly fifteen operations are registered. Unknown wire strings are
//! rejected with [`Reject::UnknownRecordKind`] (never silently ignored), and
//! every payload is decoded through a closed schema (unknown keys →
//! [`Reject::InvalidContent`]). Each payload struct round-trips through
//! canonical CBOR so it can be embedded in a signed [`super::records`]
//! `GovernanceEntryBody`.

use crate::cbor::CborValue;
use crate::error::Reject;
use crate::ids::{DeviceId, GovernanceId, PrincipalId, StateRoot, StreamId};

use super::model::{
    CommunityPolicy, RecoveryConfig, ReplicaDescriptor, ReplicaStatus, Role, StreamPolicy,
};

/// The closed set of §7.3 operation discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GovernanceOperationKind {
    /// `member.grant`
    MemberGrant,
    /// `member.revoke`
    MemberRevoke,
    /// `member.role_set`
    MemberRoleSet,
    /// `device.grant`
    DeviceGrant,
    /// `device.revoke`
    DeviceRevoke,
    /// `admin.set`
    AdminSet,
    /// `recovery.set`
    RecoverySet,
    /// `replica.set`
    ReplicaSet,
    /// `stream.create`
    StreamCreate,
    /// `stream.policy_set`
    StreamPolicySet,
    /// `stream.archive`
    StreamArchive,
    /// `invite.revoke`
    InviteRevoke,
    /// `policy.set`
    PolicySet,
    /// `fork.resolve`
    ForkResolve,
    /// `migration.accept`
    MigrationAccept,
}

impl GovernanceOperationKind {
    /// All registered kinds in registry order.
    pub const ALL: &[Self] = &[
        Self::MemberGrant,
        Self::MemberRevoke,
        Self::MemberRoleSet,
        Self::DeviceGrant,
        Self::DeviceRevoke,
        Self::AdminSet,
        Self::RecoverySet,
        Self::ReplicaSet,
        Self::StreamCreate,
        Self::StreamPolicySet,
        Self::StreamArchive,
        Self::InviteRevoke,
        Self::PolicySet,
        Self::ForkResolve,
        Self::MigrationAccept,
    ];

    /// The canonical §7.3 wire string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemberGrant => "member.grant",
            Self::MemberRevoke => "member.revoke",
            Self::MemberRoleSet => "member.role_set",
            Self::DeviceGrant => "device.grant",
            Self::DeviceRevoke => "device.revoke",
            Self::AdminSet => "admin.set",
            Self::RecoverySet => "recovery.set",
            Self::ReplicaSet => "replica.set",
            Self::StreamCreate => "stream.create",
            Self::StreamPolicySet => "stream.policy_set",
            Self::StreamArchive => "stream.archive",
            Self::InviteRevoke => "invite.revoke",
            Self::PolicySet => "policy.set",
            Self::ForkResolve => "fork.resolve",
            Self::MigrationAccept => "migration.accept",
        }
    }

    /// Parse a discriminant from its canonical wire string.
    ///
    /// # Errors
    /// Returns [`Reject::UnknownRecordKind`] for any string outside the closed
    /// §7.3 registry (spec §7.3 / D8: unknown operations MUST be rejected, not
    /// ignored).
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "member.grant" => Ok(Self::MemberGrant),
            "member.revoke" => Ok(Self::MemberRevoke),
            "member.role_set" => Ok(Self::MemberRoleSet),
            "device.grant" => Ok(Self::DeviceGrant),
            "device.revoke" => Ok(Self::DeviceRevoke),
            "admin.set" => Ok(Self::AdminSet),
            "recovery.set" => Ok(Self::RecoverySet),
            "replica.set" => Ok(Self::ReplicaSet),
            "stream.create" => Ok(Self::StreamCreate),
            "stream.policy_set" => Ok(Self::StreamPolicySet),
            "stream.archive" => Ok(Self::StreamArchive),
            "invite.revoke" => Ok(Self::InviteRevoke),
            "policy.set" => Ok(Self::PolicySet),
            "fork.resolve" => Ok(Self::ForkResolve),
            "migration.accept" => Ok(Self::MigrationAccept),
            _ => Err(Reject::UnknownRecordKind),
        }
    }
}

// ----------------------------------------------------------------------------
// Payload structs (spec §6.3 apply table).
// ----------------------------------------------------------------------------

/// `member.grant`: insert/reactivate a member with roles and an optional profile reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberGrant {
    /// The member to grant.
    pub member_id: PrincipalId,
    /// Canonical sorted, duplicate-free granted role set.
    pub roles: Vec<Role>,
    /// Optional opaque profile reference with no authorization meaning.
    pub profile: Option<Vec<u8>>,
}

/// `member.revoke`: mark a member revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRevoke {
    /// The member to revoke.
    pub member_id: PrincipalId,
}

/// `member.role_set`: replace an active member's role set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRoleSet {
    /// The member whose roles are replaced.
    pub member_id: PrincipalId,
    /// Canonical sorted, duplicate-free role set.
    pub roles: Vec<Role>,
}

/// `device.grant`: add a device to a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGrant {
    /// The owning member.
    pub member_id: PrincipalId,
    /// The device to grant.
    pub device_id: DeviceId,
    /// Opaque canonical binding metadata.
    pub binding: Vec<u8>,
}

/// `device.revoke`: revoke a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRevoke {
    /// The owning member.
    pub member_id: PrincipalId,
    /// The device to revoke.
    pub device_id: DeviceId,
}

/// `admin.set`: replace the administrator set + threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSet {
    /// The new administrator set (decoded into sorted unique order).
    pub administrators: Vec<PrincipalId>,
    /// The new threshold.
    pub threshold: u16,
}

/// `recovery.set`: replace the recovery configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySet {
    /// The new recovery configuration.
    pub recovery: RecoveryConfig,
}

/// `replica.set`: upsert/disable a replica record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaSet {
    /// The replica descriptor.
    pub replica: ReplicaDescriptor,
    /// The target status.
    pub status: ReplicaStatus,
}

/// `stream.create`: create a new stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCreate {
    /// The new stream id.
    pub stream_id: StreamId,
    /// The initial stream policy.
    pub policy: StreamPolicy,
    /// Signed creation time (advisory).
    pub created_at_ms: u64,
}

/// `stream.policy_set`: replace a stream's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPolicySet {
    /// The target stream id.
    pub stream_id: StreamId,
    /// The new policy.
    pub policy: StreamPolicy,
}

/// `stream.archive`: archive/unarchive a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamArchive {
    /// The target stream id.
    pub stream_id: StreamId,
    /// The target archived flag.
    pub archived: bool,
}

/// `invite.revoke`: revoke an invite commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRevoke {
    /// The invite commitment/id being revoked.
    pub invite_id: [u8; 32],
}

/// `policy.set`: replace the community policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySet {
    /// The new community policy.
    pub policy: CommunityPolicy,
}

/// `migration.accept`: record a migration acceptance marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationAccept {
    /// The migration id/version being accepted.
    pub migration_id: [u8; 32],
}

/// `fork.resolve`: recovery-authorized resolution of a governance fork (spec
/// §6.6 / #134 §7.5, issue #149).
///
/// The signed operation payload. Canonical rules (spec §6.6):
/// - `branch_heads.len() >= 2`, sorted ascending by raw id, no duplicates;
/// - `selected_head` occurs exactly once in `branch_heads`;
/// - `created_at_ms` is signed advisory data, never checked against a clock.
///
/// Selection is always the explicit `selected_head` signed by at least `W`
/// recovery principals. Lexical/timestamp/arrival order is never a tie-break
/// (spec §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkResolve {
    /// Every known competing branch head, sorted ascending by raw id.
    pub branch_heads: Vec<GovernanceId>,
    /// The selected branch head (must be in `branch_heads`).
    pub selected_head: GovernanceId,
    /// The already-validated state root for `selected_head`.
    pub selected_state_root: StateRoot,
    /// Signed creation time (advisory; never a wall clock).
    pub created_at_ms: u64,
}

impl ForkResolve {
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

    /// Decode + strictly validate a `fork.resolve` payload against the closed
    /// schema and canonical branch-head invariants (spec §6.6 / §9 Rule 2).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidForkResolution`] for a non-closed schema or a
    /// non-canonical branch-head set (fewer than two, unsorted, duplicate, or
    /// `selected_head` not present exactly once).
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidForkResolution)?;
        super::reject_unknown_keys(
            entries,
            &[
                "branch_heads",
                "selected_head",
                "selected_state_root",
                "created_at_ms",
            ],
            Reject::InvalidForkResolution,
        )?;
        let (branch_heads, selected_head) =
            super::read_canonical_branch_set(entries, Reject::InvalidForkResolution)?;
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

/// A typed sum over every registered operation payload (spec §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceOperationPayload {
    /// `member.grant`
    MemberGrant(MemberGrant),
    /// `member.revoke`
    MemberRevoke(MemberRevoke),
    /// `member.role_set`
    MemberRoleSet(MemberRoleSet),
    /// `device.grant`
    DeviceGrant(DeviceGrant),
    /// `device.revoke`
    DeviceRevoke(DeviceRevoke),
    /// `admin.set`
    AdminSet(AdminSet),
    /// `recovery.set`
    RecoverySet(RecoverySet),
    /// `replica.set`
    ReplicaSet(ReplicaSet),
    /// `stream.create`
    StreamCreate(StreamCreate),
    /// `stream.policy_set`
    StreamPolicySet(StreamPolicySet),
    /// `stream.archive`
    StreamArchive(StreamArchive),
    /// `invite.revoke`
    InviteRevoke(InviteRevoke),
    /// `policy.set`
    PolicySet(PolicySet),
    /// `fork.resolve`
    ForkResolve(ForkResolve),
    /// `migration.accept`
    MigrationAccept(MigrationAccept),
}

fn validate_roles(roles: &[Role]) -> Result<(), Reject> {
    if roles.is_empty()
        || roles
            .windows(2)
            .any(|window| window[0].as_str() >= window[1].as_str())
    {
        Err(Reject::InvalidContent)
    } else {
        Ok(())
    }
}

impl GovernanceOperationPayload {
    /// The discriminant of this payload.
    #[must_use]
    pub fn kind(&self) -> GovernanceOperationKind {
        match self {
            Self::MemberGrant(_) => GovernanceOperationKind::MemberGrant,
            Self::MemberRevoke(_) => GovernanceOperationKind::MemberRevoke,
            Self::MemberRoleSet(_) => GovernanceOperationKind::MemberRoleSet,
            Self::DeviceGrant(_) => GovernanceOperationKind::DeviceGrant,
            Self::DeviceRevoke(_) => GovernanceOperationKind::DeviceRevoke,
            Self::AdminSet(_) => GovernanceOperationKind::AdminSet,
            Self::RecoverySet(_) => GovernanceOperationKind::RecoverySet,
            Self::ReplicaSet(_) => GovernanceOperationKind::ReplicaSet,
            Self::StreamCreate(_) => GovernanceOperationKind::StreamCreate,
            Self::StreamPolicySet(_) => GovernanceOperationKind::StreamPolicySet,
            Self::StreamArchive(_) => GovernanceOperationKind::StreamArchive,
            Self::InviteRevoke(_) => GovernanceOperationKind::InviteRevoke,
            Self::PolicySet(_) => GovernanceOperationKind::PolicySet,
            Self::ForkResolve(_) => GovernanceOperationKind::ForkResolve,
            Self::MigrationAccept(_) => GovernanceOperationKind::MigrationAccept,
        }
    }

    /// Canonical-CBOR encode this payload (the nested `payload` field of a
    /// [`super::records::GovernanceEntryBody`]).
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn to_cbor(&self) -> CborValue {
        match self {
            Self::MemberGrant(p) => {
                let mut entries = vec![
                    (
                        "member_id".to_owned(),
                        CborValue::Bytes(p.member_id.as_bytes().to_vec()),
                    ),
                    (
                        "roles".to_owned(),
                        CborValue::Array(p.roles.iter().map(Role::to_cbor).collect()),
                    ),
                ];
                if let Some(profile) = &p.profile {
                    entries.push(("profile".to_owned(), CborValue::Bytes(profile.clone())));
                }
                CborValue::Map(entries)
            }
            Self::MemberRevoke(p) => CborValue::Map(vec![(
                "member_id".to_owned(),
                CborValue::Bytes(p.member_id.as_bytes().to_vec()),
            )]),
            Self::MemberRoleSet(p) => CborValue::Map(vec![
                (
                    "member_id".to_owned(),
                    CborValue::Bytes(p.member_id.as_bytes().to_vec()),
                ),
                (
                    "roles".to_owned(),
                    CborValue::Array(p.roles.iter().map(Role::to_cbor).collect()),
                ),
            ]),
            Self::DeviceGrant(p) => CborValue::Map(vec![
                (
                    "member_id".to_owned(),
                    CborValue::Bytes(p.member_id.as_bytes().to_vec()),
                ),
                (
                    "device_id".to_owned(),
                    CborValue::Bytes(p.device_id.as_bytes().to_vec()),
                ),
                ("binding".to_owned(), CborValue::Bytes(p.binding.clone())),
            ]),
            Self::DeviceRevoke(p) => CborValue::Map(vec![
                (
                    "member_id".to_owned(),
                    CborValue::Bytes(p.member_id.as_bytes().to_vec()),
                ),
                (
                    "device_id".to_owned(),
                    CborValue::Bytes(p.device_id.as_bytes().to_vec()),
                ),
            ]),
            Self::AdminSet(p) => CborValue::Map(vec![
                (
                    "administrators".to_owned(),
                    CborValue::Array(
                        p.administrators
                            .iter()
                            .map(|a| CborValue::Bytes(a.as_bytes().to_vec()))
                            .collect(),
                    ),
                ),
                (
                    "threshold".to_owned(),
                    CborValue::Uint(u64::from(p.threshold)),
                ),
            ]),
            Self::RecoverySet(p) => p.recovery.to_cbor(),
            Self::ReplicaSet(p) => CborValue::Map(vec![
                ("replica".to_owned(), p.replica.to_cbor()),
                ("status".to_owned(), p.status.to_cbor()),
            ]),
            Self::StreamCreate(p) => CborValue::Map(vec![
                (
                    "stream_id".to_owned(),
                    CborValue::Bytes(p.stream_id.as_bytes().to_vec()),
                ),
                ("policy".to_owned(), p.policy.to_cbor()),
                ("created_at_ms".to_owned(), CborValue::Uint(p.created_at_ms)),
            ]),
            Self::StreamPolicySet(p) => CborValue::Map(vec![
                (
                    "stream_id".to_owned(),
                    CborValue::Bytes(p.stream_id.as_bytes().to_vec()),
                ),
                ("policy".to_owned(), p.policy.to_cbor()),
            ]),
            Self::StreamArchive(p) => CborValue::Map(vec![
                (
                    "stream_id".to_owned(),
                    CborValue::Bytes(p.stream_id.as_bytes().to_vec()),
                ),
                (
                    "archived".to_owned(),
                    CborValue::Uint(u64::from(p.archived)),
                ),
            ]),
            Self::InviteRevoke(p) => CborValue::Map(vec![(
                "invite_id".to_owned(),
                CborValue::Bytes(p.invite_id.to_vec()),
            )]),
            Self::PolicySet(p) => p.policy.to_cbor(),
            Self::ForkResolve(p) => p.to_cbor(),
            Self::MigrationAccept(p) => CborValue::Map(vec![(
                "migration_id".to_owned(),
                CborValue::Bytes(p.migration_id.to_vec()),
            )]),
        }
    }

    /// Decode a payload for a known `kind` from canonical CBOR. The kind and
    /// payload shape must agree.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the payload is not a closed-schema
    /// map for the declared `kind`, or if the kind and payload shapes disagree.
    #[allow(clippy::too_many_lines)] // one match arm per §7.3 operation
    pub fn from_canonical(
        kind: GovernanceOperationKind,
        value: &CborValue,
    ) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::InvalidContent)?;
        Ok(match kind {
            GovernanceOperationKind::MemberGrant => {
                super::reject_unknown_keys(
                    entries,
                    &["member_id", "roles", "profile"],
                    Reject::InvalidContent,
                )?;
                let roles_value =
                    super::opt_field(entries, "roles").ok_or(Reject::InvalidContent)?;
                let roles = roles_value
                    .as_array()
                    .ok_or(Reject::InvalidContent)?
                    .iter()
                    .map(|value| {
                        value
                            .as_text()
                            .ok_or(Reject::InvalidContent)
                            .and_then(Role::parse)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                validate_roles(&roles)?;
                let profile = super::opt_field(entries, "profile")
                    .map(|value| {
                        value
                            .as_bytes()
                            .map(<[u8]>::to_vec)
                            .ok_or(Reject::InvalidContent)
                    })
                    .transpose()?;
                Self::MemberGrant(MemberGrant {
                    member_id: super::read_principal_field(entries, "member_id")?,
                    roles,
                    profile,
                })
            }
            GovernanceOperationKind::MemberRevoke => {
                super::reject_unknown_keys(entries, &["member_id"], Reject::InvalidContent)?;
                Self::MemberRevoke(MemberRevoke {
                    member_id: super::read_principal_field(entries, "member_id")?,
                })
            }
            GovernanceOperationKind::MemberRoleSet => {
                super::reject_unknown_keys(
                    entries,
                    &["member_id", "roles"],
                    Reject::InvalidContent,
                )?;
                let roles_value =
                    super::opt_field(entries, "roles").ok_or(Reject::InvalidContent)?;
                let roles_array = roles_value.as_array().ok_or(Reject::InvalidContent)?;
                let roles = roles_array
                    .iter()
                    .map(|value| {
                        value
                            .as_text()
                            .ok_or(Reject::InvalidContent)
                            .and_then(Role::parse)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                validate_roles(&roles)?;
                Self::MemberRoleSet(MemberRoleSet {
                    member_id: super::read_principal_field(entries, "member_id")?,
                    roles,
                })
            }
            GovernanceOperationKind::DeviceGrant => {
                super::reject_unknown_keys(
                    entries,
                    &["member_id", "device_id", "binding"],
                    Reject::InvalidContent,
                )?;
                Self::DeviceGrant(DeviceGrant {
                    member_id: super::read_principal_field(entries, "member_id")?,
                    device_id: super::read_device_field(entries, "device_id")?,
                    binding: super::read_bytes_field(entries, "binding")?.to_vec(),
                })
            }
            GovernanceOperationKind::DeviceRevoke => {
                super::reject_unknown_keys(
                    entries,
                    &["member_id", "device_id"],
                    Reject::InvalidContent,
                )?;
                Self::DeviceRevoke(DeviceRevoke {
                    member_id: super::read_principal_field(entries, "member_id")?,
                    device_id: super::read_device_field(entries, "device_id")?,
                })
            }
            GovernanceOperationKind::AdminSet => {
                super::reject_unknown_keys(
                    entries,
                    &["administrators", "threshold"],
                    Reject::InvalidContent,
                )?;
                let mut administrators = super::read_principal_array(entries, "administrators")?;
                administrators.sort();
                administrators.dedup();
                let threshold = super::read_u16_field(entries, "threshold")?;
                Self::AdminSet(AdminSet {
                    administrators,
                    threshold,
                })
            }
            GovernanceOperationKind::RecoverySet => Self::RecoverySet(RecoverySet {
                recovery: RecoveryConfig::from_canonical(value)?,
            }),
            GovernanceOperationKind::ReplicaSet => {
                super::reject_unknown_keys(
                    entries,
                    &["replica", "status"],
                    Reject::InvalidContent,
                )?;
                let replica_val =
                    super::opt_field(entries, "replica").ok_or(Reject::InvalidContent)?;
                let status_val =
                    super::opt_field(entries, "status").ok_or(Reject::InvalidContent)?;
                Self::ReplicaSet(ReplicaSet {
                    replica: ReplicaDescriptor::from_canonical(replica_val)?,
                    status: ReplicaStatus::parse(
                        status_val.as_text().ok_or(Reject::InvalidContent)?,
                    )?,
                })
            }
            GovernanceOperationKind::StreamCreate => {
                super::reject_unknown_keys(
                    entries,
                    &["stream_id", "policy", "created_at_ms"],
                    Reject::InvalidContent,
                )?;
                let policy_val =
                    super::opt_field(entries, "policy").ok_or(Reject::InvalidContent)?;
                Self::StreamCreate(StreamCreate {
                    stream_id: super::read_stream_field(entries, "stream_id")?,
                    policy: StreamPolicy::from_canonical(policy_val)?,
                    created_at_ms: super::read_uint_field(entries, "created_at_ms")?,
                })
            }
            GovernanceOperationKind::StreamPolicySet => {
                super::reject_unknown_keys(
                    entries,
                    &["stream_id", "policy"],
                    Reject::InvalidContent,
                )?;
                let policy_val =
                    super::opt_field(entries, "policy").ok_or(Reject::InvalidContent)?;
                Self::StreamPolicySet(StreamPolicySet {
                    stream_id: super::read_stream_field(entries, "stream_id")?,
                    policy: StreamPolicy::from_canonical(policy_val)?,
                })
            }
            GovernanceOperationKind::StreamArchive => {
                super::reject_unknown_keys(
                    entries,
                    &["stream_id", "archived"],
                    Reject::InvalidContent,
                )?;
                // The encoder emits only 0 or 1 (review thread #4); accept no
                // other value so a canonical `archived = 2` is rejected as
                // malformed content instead of silently coerced to `true`.
                let archived_val = super::read_uint_field(entries, "archived")?;
                if archived_val > 1 {
                    return Err(Reject::InvalidContent);
                }
                Self::StreamArchive(StreamArchive {
                    stream_id: super::read_stream_field(entries, "stream_id")?,
                    archived: archived_val == 1,
                })
            }
            GovernanceOperationKind::InviteRevoke => {
                super::reject_unknown_keys(entries, &["invite_id"], Reject::InvalidContent)?;
                Self::InviteRevoke(InviteRevoke {
                    invite_id: super::read_fixed32_field(entries, "invite_id")?,
                })
            }
            GovernanceOperationKind::PolicySet => Self::PolicySet(PolicySet {
                policy: CommunityPolicy::from_canonical(value)?,
            }),
            GovernanceOperationKind::ForkResolve => {
                Self::ForkResolve(ForkResolve::from_canonical(value)?)
            }
            GovernanceOperationKind::MigrationAccept => {
                super::reject_unknown_keys(entries, &["migration_id"], Reject::InvalidContent)?;
                Self::MigrationAccept(MigrationAccept {
                    migration_id: super::read_fixed32_field(entries, "migration_id")?,
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::LEN;

    #[test]
    fn registry_has_exactly_fifteen_operations() {
        assert_eq!(
            GovernanceOperationKind::ALL.len(),
            15,
            "§7.3 freezes exactly 15 ops"
        );
    }

    #[test]
    fn every_kind_round_trips_through_wire_string() {
        for kind in GovernanceOperationKind::ALL {
            let s = kind.as_str();
            assert_eq!(GovernanceOperationKind::parse(s).unwrap(), *kind);
        }
    }

    #[test]
    fn unknown_operation_kind_rejected() {
        // Candidate v1 names must NOT validate as v2 operations (spec §6.1).
        for bogus in [
            "init_room",
            "add_member",
            "set_policy",
            "rotate_device",
            "frobnicate",
        ] {
            assert_eq!(
                GovernanceOperationKind::parse(bogus).err(),
                Some(Reject::UnknownRecordKind),
                "alias `{bogus}` must be rejected"
            );
        }
    }

    #[test]
    fn admin_set_payload_canonicalizes_admins() {
        let a = PrincipalId::from_bytes([1; LEN]);
        let b = PrincipalId::from_bytes([2; LEN]);
        let payload = GovernanceOperationPayload::AdminSet(AdminSet {
            administrators: vec![b, a, a],
            threshold: 1,
        });
        let cbor = payload.to_cbor();
        let back =
            GovernanceOperationPayload::from_canonical(GovernanceOperationKind::AdminSet, &cbor)
                .unwrap();
        match back {
            GovernanceOperationPayload::AdminSet(p) => {
                assert_eq!(p.administrators, vec![a, b]);
                assert_eq!(p.threshold, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn every_payload_round_trips_through_canonical_cbor() {
        use super::super::model::{
            CommunityPolicy, RecoveryConfig, ReplicaDescriptor, ReplicaStatus, Role, StreamPolicy,
        };
        use super::ForkResolve;
        use crate::ids::{DeviceId, GovernanceId, ReplicaId, StreamId};

        let payloads = [
            GovernanceOperationPayload::MemberGrant(MemberGrant {
                member_id: PrincipalId::from_bytes([0x01; LEN]),
                roles: vec![Role::Member],
                profile: None,
            }),
            GovernanceOperationPayload::MemberRevoke(MemberRevoke {
                member_id: PrincipalId::from_bytes([0x02; LEN]),
            }),
            GovernanceOperationPayload::MemberRoleSet(MemberRoleSet {
                member_id: PrincipalId::from_bytes([0x02; LEN]),
                roles: vec![Role::Admin, Role::Agent, Role::Member],
            }),
            GovernanceOperationPayload::DeviceGrant(DeviceGrant {
                member_id: PrincipalId::from_bytes([0x03; LEN]),
                device_id: DeviceId::from_bytes([0x04; LEN]),
                binding: vec![0xaa],
            }),
            GovernanceOperationPayload::DeviceRevoke(DeviceRevoke {
                member_id: PrincipalId::from_bytes([0x05; LEN]),
                device_id: DeviceId::from_bytes([0x06; LEN]),
            }),
            GovernanceOperationPayload::AdminSet(AdminSet {
                administrators: vec![PrincipalId::from_bytes([0x07; LEN])],
                threshold: 1,
            }),
            GovernanceOperationPayload::RecoverySet(RecoverySet {
                recovery: RecoveryConfig {
                    threshold: 1,
                    recovery_keys: vec![PrincipalId::from_bytes([0x08; LEN])],
                },
            }),
            GovernanceOperationPayload::ReplicaSet(ReplicaSet {
                replica: ReplicaDescriptor {
                    replica_id: ReplicaId::from_bytes([0x09; LEN]),
                    endpoint: vec![0xaa],
                    capability: 1,
                },
                status: ReplicaStatus::Active,
            }),
            GovernanceOperationPayload::StreamCreate(StreamCreate {
                stream_id: StreamId::from_bytes([0x0a; LEN]),
                policy: StreamPolicy::default_policy(),
                created_at_ms: 7,
            }),
            GovernanceOperationPayload::StreamPolicySet(StreamPolicySet {
                stream_id: StreamId::from_bytes([0x0b; LEN]),
                policy: StreamPolicy { access: 3 },
            }),
            GovernanceOperationPayload::StreamArchive(StreamArchive {
                stream_id: StreamId::from_bytes([0x0c; LEN]),
                archived: true,
            }),
            GovernanceOperationPayload::InviteRevoke(InviteRevoke {
                invite_id: [0x0d; LEN],
            }),
            GovernanceOperationPayload::PolicySet(PolicySet {
                policy: CommunityPolicy::empty(),
            }),
            GovernanceOperationPayload::ForkResolve(ForkResolve {
                branch_heads: [
                    GovernanceId::from_bytes([0x0e; LEN]),
                    GovernanceId::from_bytes([0x0f; LEN]),
                ]
                .to_vec(),
                selected_head: GovernanceId::from_bytes([0x0e; LEN]),
                selected_state_root: crate::ids::StateRoot::from_bytes([0xee; LEN]),
                created_at_ms: 8,
            }),
            GovernanceOperationPayload::MigrationAccept(MigrationAccept {
                migration_id: [0x10; LEN],
            }),
        ];
        // Every registered kind must be represented exactly once.
        assert_eq!(payloads.len(), GovernanceOperationKind::ALL.len());
        for payload in &payloads {
            let cbor = payload.to_cbor();
            let back = GovernanceOperationPayload::from_canonical(payload.kind(), &cbor).unwrap();
            assert_eq!(&back, payload, "round-trip failed for {:?}", payload.kind());
        }
    }

    #[test]
    fn payload_kind_matches_wire_string_for_every_op() {
        for kind in GovernanceOperationKind::ALL {
            assert_eq!(
                GovernanceOperationKind::parse(kind.as_str()).unwrap(),
                *kind
            );
        }
    }

    #[test]
    fn payload_with_unknown_key_rejected() {
        // A member.grant payload carrying an extra key must be rejected
        // (closed-schema decode, spec D8).
        let cbor = CborValue::Map(vec![
            ("member_id".to_owned(), CborValue::Bytes(vec![0x01; LEN])),
            ("role".to_owned(), CborValue::Text("member".to_owned())),
            ("extra".to_owned(), CborValue::Uint(1)),
        ]);
        assert_eq!(
            GovernanceOperationPayload::from_canonical(GovernanceOperationKind::MemberGrant, &cbor)
                .err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn stream_archive_rejects_non_boolean_flag() {
        // The encoder emits only 0 or 1 for `archived` (review thread #4). A
        // canonical payload carrying `archived = 2` must be rejected as malformed
        // content rather than silently coerced to `true` (which would also
        // normalize to different bytes when re-encoded).
        let cbor = CborValue::Map(vec![
            ("stream_id".to_owned(), CborValue::Bytes(vec![0x0c; LEN])),
            ("archived".to_owned(), CborValue::Uint(2)),
        ]);
        assert_eq!(
            GovernanceOperationPayload::from_canonical(
                GovernanceOperationKind::StreamArchive,
                &cbor,
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Sanity: 0 and 1 still decode.
        for v in [0u64, 1u64] {
            let cbor = CborValue::Map(vec![
                ("stream_id".to_owned(), CborValue::Bytes(vec![0x0c; LEN])),
                ("archived".to_owned(), CborValue::Uint(v)),
            ]);
            assert!(
                GovernanceOperationPayload::from_canonical(
                    GovernanceOperationKind::StreamArchive,
                    &cbor,
                )
                .is_ok(),
                "archived = {v} must still decode"
            );
        }
    }

    #[test]
    fn payload_kind_mismatch_rejected() {
        // Encode a MemberGrant, then attempt to decode it as MemberRevoke.
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: PrincipalId::from_bytes([1; LEN]),
            roles: vec![Role::Member],
            profile: None,
        });
        let cbor = payload.to_cbor();
        assert!(GovernanceOperationPayload::from_canonical(
            GovernanceOperationKind::MemberRevoke,
            &cbor
        )
        .is_err());
    }
}
