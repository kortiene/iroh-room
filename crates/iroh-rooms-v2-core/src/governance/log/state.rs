//! The pure governance [`GovernanceState`] (six components), the six-component
//! [`GovernanceStateRootRecord`], state-root computation, and the pure
//! `apply(old_state, op) -> new_state` functions for every §7.3 operation
//! (spec §6.2, §6.3, §7, issue #147).
//!
//! All apply functions are total over structurally valid payloads: they never
//! read a wall clock, randomness, storage, network, or global state. Identical
//! inputs produce byte-identical state roots.

use std::collections::BTreeMap;

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::ids::CommunityId;
use crate::ids::{PrincipalId, ReplicaId, StateRoot, StreamId};

use super::genesis::GenesisConfig;
use super::model::{
    AdministratorState, CommunityPolicy, DeviceRecord, DeviceStatus, MemberRecord, MemberStatus,
    RecoveryState, ReplicaRecord, ReplicaStatus, Role, StreamRecord,
};
use super::operation::{
    AdminSet, DeviceGrant, DeviceRevoke, GovernanceOperationPayload, InviteRevoke, MemberGrant,
    MemberRevoke, MigrationAccept, PolicySet, RecoverySet, ReplicaSet, StreamArchive, StreamCreate,
    StreamPolicySet,
};
use super::records::GovernanceEntryBody;

// ----------------------------------------------------------------------------
// The six-component state (spec §6.2 / §7.1).
// ----------------------------------------------------------------------------

/// The pure governance state (spec §6.2). Exactly six components, in the
/// fixed §7.1 order. The state-root record commits to all six.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceState {
    /// The community this state belongs to.
    pub community_id: CommunityId,
    /// §7.1 component 1: administrators + threshold.
    pub administrators: AdministratorState,
    /// §7.1 component 2: recovery configuration.
    pub recovery: RecoveryState,
    /// §7.1 component 3: replicas, keyed by [`ReplicaId`].
    pub replicas: BTreeMap<ReplicaId, ReplicaRecord>,
    /// §7.1 component 4: members/devices/roles, keyed by principal id.
    pub members: BTreeMap<PrincipalId, MemberRecord>,
    /// §7.1 component 5: stream manifest, keyed by [`StreamId`].
    pub streams: BTreeMap<StreamId, StreamRecord>,
    /// §7.1 component 6: community policy (revoked invites, fork markers,
    /// migrations).
    pub policy: CommunityPolicy,
}

impl GovernanceState {
    /// Build the initial post-genesis state from a verified genesis config +
    /// its derived community id (spec §6.2 / D3).
    #[must_use]
    pub fn from_genesis(config: &GenesisConfig, community_id: CommunityId) -> Self {
        let mut members = BTreeMap::new();
        for admin in &config.administrators {
            members.insert(
                *admin,
                MemberRecord {
                    member_id: *admin,
                    role: Role::Admin,
                    status: MemberStatus::Active,
                    devices: BTreeMap::new(),
                },
            );
        }
        let mut replicas = BTreeMap::new();
        for desc in &config.replicas {
            replicas.insert(
                desc.replica_id,
                ReplicaRecord {
                    descriptor: desc.clone(),
                    status: ReplicaStatus::Active,
                },
            );
        }
        Self {
            community_id,
            administrators: AdministratorState {
                administrators: config.administrators.clone(),
                threshold: config.admin_threshold,
            },
            recovery: RecoveryState {
                config: config.recovery.clone(),
            },
            replicas,
            members,
            streams: BTreeMap::new(),
            policy: config.community_policy.clone(),
        }
    }

    /// An empty state for `community_id` (used by tests before genesis apply).
    #[must_use]
    pub fn empty(community_id: CommunityId) -> Self {
        Self {
            community_id,
            administrators: AdministratorState {
                administrators: Vec::new(),
                threshold: 0,
            },
            recovery: RecoveryState {
                config: super::model::RecoveryConfig::empty(),
            },
            replicas: BTreeMap::new(),
            members: BTreeMap::new(),
            streams: BTreeMap::new(),
            policy: CommunityPolicy::empty(),
        }
    }
}

// ----------------------------------------------------------------------------
// State-root record + computation (spec §7).
// ----------------------------------------------------------------------------

/// The fixed-order six-component state-root record (spec §7.1 / D7).
///
/// Canonicalized as a CBOR array of six byte strings in the exact §7.1 order
/// for direct correspondence to the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernanceStateRootRecord {
    /// Component 1: administrators.
    pub administrators_root: [u8; 32],
    /// Component 2: recovery.
    pub recovery_root: [u8; 32],
    /// Component 3: replicas.
    pub replicas_root: [u8; 32],
    /// Component 4: members/devices/roles.
    pub members_devices_roles_root: [u8; 32],
    /// Component 5: stream manifest.
    pub stream_manifest_root: [u8; 32],
    /// Component 6: community policy.
    pub community_policy_root: [u8; 32],
}

impl GovernanceStateRootRecord {
    /// Canonical CBOR array of the six component roots (spec D7: fixed-order
    /// array).
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Array(vec![
            CborValue::Bytes(self.administrators_root.to_vec()),
            CborValue::Bytes(self.recovery_root.to_vec()),
            CborValue::Bytes(self.replicas_root.to_vec()),
            CborValue::Bytes(self.members_devices_roles_root.to_vec()),
            CborValue::Bytes(self.stream_manifest_root.to_vec()),
            CborValue::Bytes(self.community_policy_root.to_vec()),
        ])
    }
}

/// The §7.1 component labels (used in the per-component preimage so a hash
/// valid for one component cannot replay as another).
pub const COMPONENT_LABELS: &[&str] = &[
    "administrators",
    "recovery",
    "replicas",
    "members_devices_roles",
    "stream_manifest",
    "community_policy",
];

/// Compute a component root: `BLAKE3(GOVERNANCE_STATE || cbor({label, value}))`
/// (spec D7 / §7.2).
#[must_use]
pub fn component_root(label: &str, value: &CborValue) -> [u8; 32] {
    let preimage = CborValue::Map(vec![
        ("label".to_owned(), CborValue::Text(label.to_owned())),
        ("value".to_owned(), value.clone()),
    ]);
    domain::blake3_domain(domain::GOVERNANCE_STATE, &crate::cbor::encode(&preimage))
}

// --- Per-component canonicalizers (spec §7.1) --------------------------------

fn administrators_component(state: &GovernanceState) -> CborValue {
    state.administrators.to_cbor()
}

fn recovery_component(state: &GovernanceState) -> CborValue {
    state.recovery.to_cbor()
}

fn replicas_component(state: &GovernanceState) -> CborValue {
    CborValue::Array(
        state
            .replicas
            .values()
            .map(ReplicaRecord::to_cbor)
            .collect(),
    )
}

fn members_devices_roles_component(state: &GovernanceState) -> CborValue {
    CborValue::Array(state.members.values().map(MemberRecord::to_cbor).collect())
}

fn stream_manifest_component(state: &GovernanceState) -> CborValue {
    CborValue::Array(state.streams.values().map(StreamRecord::to_cbor).collect())
}

fn community_policy_component(state: &GovernanceState) -> CborValue {
    state.policy.to_cbor()
}

/// Compute the six-component state-root record for `state` (spec §7.1).
#[must_use]
pub fn governance_state_root_record(state: &GovernanceState) -> GovernanceStateRootRecord {
    GovernanceStateRootRecord {
        administrators_root: component_root("administrators", &administrators_component(state)),
        recovery_root: component_root("recovery", &recovery_component(state)),
        replicas_root: component_root("replicas", &replicas_component(state)),
        members_devices_roles_root: component_root(
            "members_devices_roles",
            &members_devices_roles_component(state),
        ),
        stream_manifest_root: component_root("stream_manifest", &stream_manifest_component(state)),
        community_policy_root: component_root(
            "community_policy",
            &community_policy_component(state),
        ),
    }
}

/// Compute the final state root:
/// `BLAKE3(GOVERNANCE_STATE || cbor(GovernanceStateRootRecord))` (spec §7.2).
#[must_use]
pub fn compute_state_root(state: &GovernanceState) -> StateRoot {
    let record = governance_state_root_record(state);
    StateRoot::from_bytes(domain::blake3_domain(
        domain::GOVERNANCE_STATE,
        &crate::cbor::encode(&record.to_cbor()),
    ))
}

/// Recompute the state root and compare to a supplied root (spec §7.3).
///
/// # Errors
/// Returns [`Reject::StateRootMismatch`] if the supplied root differs.
pub fn verify_state_root(state: &GovernanceState, expected: &StateRoot) -> Result<(), Reject> {
    if &compute_state_root(state) == expected {
        Ok(())
    } else {
        Err(Reject::StateRootMismatch)
    }
}

// ----------------------------------------------------------------------------
// Apply functions (spec §6.3 table) — one pure transition per operation.
// ----------------------------------------------------------------------------

/// Apply a typed operation to `old`, returning the new state (spec §6.4).
///
/// Clone-and-return is acceptable for this pure core (spec §6.4 impl notes).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] for structurally invalid transitions
/// (e.g. revoking a device on a non-existent member).
pub fn apply(
    old: &GovernanceState,
    op: &GovernanceOperationPayload,
) -> Result<GovernanceState, Reject> {
    match op {
        GovernanceOperationPayload::MemberGrant(p) => apply_member_grant(old, p),
        GovernanceOperationPayload::MemberRevoke(p) => apply_member_revoke(old, p),
        GovernanceOperationPayload::DeviceGrant(p) => apply_device_grant(old, p),
        GovernanceOperationPayload::DeviceRevoke(p) => apply_device_revoke(old, p),
        GovernanceOperationPayload::AdminSet(p) => apply_admin_set(old, p),
        GovernanceOperationPayload::RecoverySet(p) => apply_recovery_set(old, p),
        GovernanceOperationPayload::ReplicaSet(p) => apply_replica_set(old, p),
        GovernanceOperationPayload::StreamCreate(p) => apply_stream_create(old, p),
        GovernanceOperationPayload::StreamPolicySet(p) => apply_stream_policy_set(old, p),
        GovernanceOperationPayload::StreamArchive(p) => apply_stream_archive(old, p),
        GovernanceOperationPayload::InviteRevoke(p) => apply_invite_revoke(old, p),
        GovernanceOperationPayload::PolicySet(p) => apply_policy_set(old, p),
        GovernanceOperationPayload::ForkResolve(p) => apply_fork_resolve(old, p),
        GovernanceOperationPayload::MigrationAccept(p) => apply_migration_accept(old, p),
    }
}

/// `member.grant`: insert or reactivate a member; set role (spec §6.3).
///
/// # Errors
/// This transition is total over structurally valid payloads; it does not fail.
pub fn apply_member_grant(
    old: &GovernanceState,
    p: &MemberGrant,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    next.members
        .entry(p.member_id)
        .and_modify(|m| {
            m.role = p.role;
            m.status = MemberStatus::Active;
        })
        .or_insert_with(|| MemberRecord {
            member_id: p.member_id,
            role: p.role,
            status: MemberStatus::Active,
            devices: BTreeMap::new(),
        });
    Ok(next)
}

/// `member.revoke`: mark member revoked (tombstoned for deterministic replay).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the member is not present.
pub fn apply_member_revoke(
    old: &GovernanceState,
    p: &MemberRevoke,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    match next.members.get_mut(&p.member_id) {
        Some(m) => m.status = MemberStatus::Revoked,
        None => return Err(Reject::InvalidContent),
    }
    Ok(next)
}

/// `device.grant`: add an active device to a member's sorted device set
/// (spec §4.4 / D7 device-binding validity, issue #148).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if:
/// - the member is not present or is not [`MemberStatus::Active`];
/// - the device id is already bound to *any* member in the old state
///   (globally unique ownership) — this covers granting a device already
///   bound to another member, granting an already-active device to the same
///   member (no silent replace), and regranting a revoked device.
pub fn apply_device_grant(
    old: &GovernanceState,
    p: &DeviceGrant,
) -> Result<GovernanceState, Reject> {
    let member = old
        .members
        .get(&p.member_id)
        .ok_or(Reject::InvalidContent)?;
    if member.status != MemberStatus::Active {
        return Err(Reject::InvalidContent);
    }
    if old
        .members
        .values()
        .any(|m| m.devices.contains_key(&p.device_id))
    {
        return Err(Reject::InvalidContent);
    }
    let mut next = old.clone();
    let member = next
        .members
        .get_mut(&p.member_id)
        .ok_or(Reject::InvalidContent)?;
    member.devices.insert(
        p.device_id,
        DeviceRecord {
            device_id: p.device_id,
            status: DeviceStatus::Active,
        },
    );
    Ok(next)
}

/// `device.revoke`: revoke a device (tombstoned) (spec §4.4 / D7 device-binding
/// validity, issue #148).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if:
/// - the member is not present or is not [`MemberStatus::Active`];
/// - the device is absent, bound to another member (so absent under this
///   member), or already [`DeviceStatus::Revoked`].
pub fn apply_device_revoke(
    old: &GovernanceState,
    p: &DeviceRevoke,
) -> Result<GovernanceState, Reject> {
    let member = old
        .members
        .get(&p.member_id)
        .ok_or(Reject::InvalidContent)?;
    if member.status != MemberStatus::Active {
        return Err(Reject::InvalidContent);
    }
    let device = member
        .devices
        .get(&p.device_id)
        .ok_or(Reject::InvalidContent)?;
    if device.status != DeviceStatus::Active {
        return Err(Reject::InvalidContent);
    }
    let mut next = old.clone();
    let member = next
        .members
        .get_mut(&p.member_id)
        .ok_or(Reject::InvalidContent)?;
    let device = member
        .devices
        .get_mut(&p.device_id)
        .ok_or(Reject::InvalidContent)?;
    device.status = DeviceStatus::Revoked;
    Ok(next)
}

/// `admin.set`: replace the administrator set + threshold (spec §6.3).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the administrator set is empty,
/// unsorted, contains duplicates, or the threshold is outside
/// `1..=administrators.len()`.
pub fn apply_admin_set(old: &GovernanceState, p: &AdminSet) -> Result<GovernanceState, Reject> {
    if p.administrators.is_empty() {
        return Err(Reject::InvalidContent);
    }
    let mut admins = p.administrators.clone();
    admins.sort();
    admins.dedup();
    if admins != p.administrators {
        // Caller supplied duplicates or unsorted input.
        return Err(Reject::InvalidContent);
    }
    if p.threshold == 0 {
        return Err(Reject::InvalidContent);
    }
    let admin_count = u16::try_from(admins.len()).map_err(|_| Reject::InvalidContent)?;
    if p.threshold > admin_count {
        return Err(Reject::InvalidContent);
    }
    let mut next = old.clone();
    next.administrators = AdministratorState {
        administrators: admins,
        threshold: p.threshold,
    };
    Ok(next)
}

/// `recovery.set`: replace the recovery component.
///
/// # Errors
/// This transition is total over structurally valid payloads; it does not fail.
pub fn apply_recovery_set(
    old: &GovernanceState,
    p: &RecoverySet,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    let mut cfg = p.recovery.clone();
    cfg.canonicalize();
    next.recovery = RecoveryState { config: cfg };
    Ok(next)
}

/// `replica.set`: upsert / disable a replica record (sorted by `ReplicaId`).
///
/// # Errors
/// This transition is total over structurally valid payloads; it does not fail.
pub fn apply_replica_set(old: &GovernanceState, p: &ReplicaSet) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    next.replicas.insert(
        p.replica.replica_id,
        ReplicaRecord {
            descriptor: p.replica.clone(),
            status: p.status,
        },
    );
    Ok(next)
}

/// `stream.create`: insert a new active stream (duplicate id rejected).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if `stream_id` already exists.
pub fn apply_stream_create(
    old: &GovernanceState,
    p: &StreamCreate,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    if next.streams.contains_key(&p.stream_id) {
        return Err(Reject::InvalidContent);
    }
    next.streams.insert(
        p.stream_id,
        StreamRecord {
            stream_id: p.stream_id,
            policy: p.policy.clone(),
            archived: false,
            created_at_ms: p.created_at_ms,
        },
    );
    Ok(next)
}

/// `stream.policy_set`: replace an existing stream's policy.
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the stream does not exist.
pub fn apply_stream_policy_set(
    old: &GovernanceState,
    p: &StreamPolicySet,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    let stream = next
        .streams
        .get_mut(&p.stream_id)
        .ok_or(Reject::InvalidContent)?;
    stream.policy = p.policy.clone();
    Ok(next)
}

/// `stream.archive`: mark an existing stream archived/unarchived.
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the stream does not exist.
pub fn apply_stream_archive(
    old: &GovernanceState,
    p: &StreamArchive,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    let stream = next
        .streams
        .get_mut(&p.stream_id)
        .ok_or(Reject::InvalidContent)?;
    stream.archived = p.archived;
    Ok(next)
}

/// `invite.revoke`: add an invite to the sorted revoked-invite set.
///
/// # Errors
/// This transition is total over structurally valid payloads; it does not fail.
pub fn apply_invite_revoke(
    old: &GovernanceState,
    p: &InviteRevoke,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    next.policy.revoked_invites.insert(p.invite_id);
    Ok(next)
}

/// `policy.set`: replace the community policy (canonicalized).
///
/// The append-only marker sets — `revoked_invites`, `fork_markers`, and
/// `migrations` — are accumulated history, not replaceable policy (review thread
/// #5). They are unioned back in after the replacement so a `policy.set` can
/// extend but never wipe them (e.g. a `policy.set(empty)` can no longer clear the
/// migrations set to re-accept a duplicate `migration.accept`).
///
/// # Errors
/// This transition is total over structurally valid payloads; it does not fail.
pub fn apply_policy_set(old: &GovernanceState, p: &PolicySet) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    let kept_invites = next.policy.revoked_invites.clone();
    let kept_migrations = next.policy.migrations.clone();
    let kept_forks = next.policy.fork_markers.clone();
    next.policy = p.policy.clone();
    next.policy.revoked_invites.extend(kept_invites);
    next.policy.migrations.extend(kept_migrations);
    next.policy.fork_markers.extend(kept_forks);
    next.policy
        .fork_markers
        .sort_by_key(|m| (*m.evidence[0].as_bytes(), *m.evidence[1].as_bytes()));
    Ok(next)
}

/// `fork.resolve`: record a deterministic marker under community policy only
/// (spec D8). #149 owns branch selection and evidence interpretation.
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the evidence pair is not an ascending
/// pair of distinct ids.
pub fn apply_fork_resolve(
    old: &GovernanceState,
    p: &super::model::ForkResolutionMarker,
) -> Result<GovernanceState, Reject> {
    // Evidence must be an ascending pair of distinct ids.
    if p.evidence[0] >= p.evidence[1] {
        return Err(Reject::InvalidContent);
    }
    let mut next = old.clone();
    next.policy.fork_markers.push(p.clone());
    Ok(next)
}

/// `migration.accept`: record a migration acceptance marker (duplicate
/// acceptance rejected — spec §6.3).
///
/// # Errors
/// Returns [`Reject::InvalidContent`] if the migration id was already accepted.
pub fn apply_migration_accept(
    old: &GovernanceState,
    p: &MigrationAccept,
) -> Result<GovernanceState, Reject> {
    let mut next = old.clone();
    if !next.policy.migrations.insert(p.migration_id) {
        return Err(Reject::InvalidContent);
    }
    Ok(next)
}

// ----------------------------------------------------------------------------
// Declared-root check + chain-link validation (spec §7.3 / D5).
// ----------------------------------------------------------------------------

/// Apply a verified entry's operation to `old`, recompute the state root, and
/// compare to the entry's declared `state_root` (spec §7.3 / acceptance).
///
/// **Not an authorization boundary** (issue #148 §4.5): this only checks
/// community/operation-validity/post-root agreement (rules 1's community
/// check, 3, and 5 of the #148 five-rule predicate). It does not check the
/// chain link (rule 2) or any signer/approval threshold (rule 4), so a
/// cryptographically valid but *unauthorized* operation can still apply
/// through this function. Normative callers must use
/// [`super::authz::validate_governance_entry`] /
/// [`super::authz::validate_and_apply_governance_entry`] instead; this
/// low-level function remains only for source compatibility.
///
/// # Errors
/// - [`Reject::InvalidContent`] — `body.community_id` differs from
///   `old.community_id`.
/// - [`Reject::StateRootMismatch`] — recomputed root differs from
///   `body.state_root`.
pub fn apply_verified_entry(
    old: &GovernanceState,
    body: &GovernanceEntryBody,
) -> Result<GovernanceState, Reject> {
    if body.community_id != old.community_id {
        return Err(Reject::InvalidContent);
    }
    let new = apply(old, &body.payload)?;
    verify_state_root(&new, &body.state_root)?;
    Ok(new)
}

/// Validate a single chain link: `seq == expected_seq` (no skipped entries),
/// `seq == 1` ⇒ `prev == None`; `seq > 1` ⇒ `prev == expected_prev` (spec D5).
/// Fork detection beyond this single check is #149.
///
/// # Errors
/// Returns [`Reject::InvalidContent`] on a chain-rule violation.
pub fn check_chain_link(
    body: &GovernanceEntryBody,
    expected_prev: Option<crate::ids::GovernanceId>,
    expected_seq: u64,
) -> Result<(), Reject> {
    // The sequence number must be exactly the next expected one (review thread
    // #2): comparing only `prev` previously let a non-contiguous chain (e.g.
    // seq 3 linking back to seq 1) fold with seq 2 silently skipped.
    if body.seq != expected_seq {
        return Err(Reject::InvalidContent);
    }
    if body.seq == 1 {
        if body.prev.is_some() {
            return Err(Reject::InvalidContent);
        }
    } else if body.prev != expected_prev {
        return Err(Reject::InvalidContent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::genesis::{
        derive_community_id, sign_genesis, verify_genesis, GENESIS_SCHEMA_VERSION,
    };
    use super::super::model::{
        ForkResolutionMarker, RecoveryConfig, ReplicaDescriptor, StreamPolicy,
    };
    use super::super::operation::GovernanceOperationKind;
    use super::super::records::{entry_csb, entry_id};
    use super::*;
    use crate::ids::{DeviceId, GovernanceId, LEN as N};
    use crate::keys::SigningKey;

    fn admin_key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; N])
    }

    fn genesis_config() -> GenesisConfig {
        let admin = admin_key(0xa0);
        GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1_000,
            genesis_nonce: [0xab; N],
            admin_threshold: 1,
            administrators: vec![admin.member_id()],
            recovery: RecoveryConfig::empty(),
            replicas: Vec::new(),
            community_policy: CommunityPolicy::empty(),
        }
    }

    fn state() -> GovernanceState {
        let cfg = genesis_config();
        let cid = derive_community_id(&cfg);
        GovernanceState::from_genesis(&cfg, cid)
    }

    fn mid(seed: u8) -> PrincipalId {
        PrincipalId::from_bytes([seed; N])
    }

    // --- Genesis threshold + community id -----------------------------------

    #[test]
    fn genesis_signs_and_verifies_under_threshold() {
        let cfg = genesis_config();
        let admin = admin_key(0xa0);
        let sig = sign_genesis(&cfg, &admin);
        let cid = verify_genesis(&cfg, std::slice::from_ref(&sig)).unwrap();
        assert_eq!(cid, derive_community_id(&cfg));
    }

    // --- State-root recomputation (golden vector property) ------------------

    #[test]
    fn state_root_is_deterministic_for_identical_state() {
        let s = state();
        assert_eq!(compute_state_root(&s), compute_state_root(&s));
    }

    #[test]
    fn state_root_changes_when_membership_changes() {
        let s = state();
        let before = compute_state_root(&s);
        let next = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            },
        )
        .unwrap();
        let after = compute_state_root(&next);
        assert_ne!(before, after);
    }

    #[test]
    fn declared_state_root_mismatch_rejected() {
        let s = state();
        let mut body = GovernanceEntryBody {
            community_id: s.community_id,
            seq: 1,
            prev: None,
            created_at_ms: 1_001,
            kind: GovernanceOperationKind::MemberGrant,
            payload: GovernanceOperationPayload::MemberGrant(MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            }),
            state_root: StateRoot::from_bytes([0xff; N]), // wrong root
        };
        assert_eq!(
            apply_verified_entry(&s, &body).err(),
            Some(Reject::StateRootMismatch)
        );
        // Correct root succeeds.
        let mut next = apply(&s, &body.payload).unwrap();
        body.state_root = compute_state_root(&next);
        next = apply_verified_entry(&s, &body).expect("declared root matches");
        // Use `next` to avoid unused warnings.
        assert!(next.members.contains_key(&mid(0xc0)));
    }

    // --- One apply test per registered operation ----------------------------

    #[test]
    fn apply_member_grant_inserts_member() {
        let s = state();
        let next = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            },
        )
        .unwrap();
        let m = next.members.get(&mid(0xc0)).unwrap();
        assert_eq!(m.role, Role::Member);
        assert_eq!(m.status, MemberStatus::Active);
    }

    #[test]
    fn apply_member_revoke_revokes_member() {
        let s = state();
        let granted = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            },
        )
        .unwrap();
        let next = apply_member_revoke(
            &granted,
            &MemberRevoke {
                member_id: mid(0xc0),
            },
        )
        .unwrap();
        assert_eq!(
            next.members.get(&mid(0xc0)).unwrap().status,
            MemberStatus::Revoked
        );
        // Revoking a non-existent member rejects.
        assert_eq!(
            apply_member_revoke(
                &s,
                &MemberRevoke {
                    member_id: mid(0xee)
                }
            )
            .err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn apply_device_grant_and_revoke() {
        let s = state();
        let admin = admin_key(0xa0).member_id();
        let dev = DeviceId::from_bytes([0xd0; N]);
        let granted = apply_device_grant(
            &s,
            &DeviceGrant {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        assert_eq!(
            granted
                .members
                .get(&admin)
                .unwrap()
                .devices
                .get(&dev)
                .unwrap()
                .status,
            DeviceStatus::Active
        );
        let revoked = apply_device_revoke(
            &granted,
            &DeviceRevoke {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        assert_eq!(
            revoked
                .members
                .get(&admin)
                .unwrap()
                .devices
                .get(&dev)
                .unwrap()
                .status,
            DeviceStatus::Revoked
        );
    }

    // --- Device-binding validity (spec §4.4 / D7, issue #148) --------------

    #[test]
    fn apply_device_grant_rejects_absent_or_revoked_member() {
        let s = state();
        let dev = DeviceId::from_bytes([0xd1; N]);
        // Absent member.
        assert_eq!(
            apply_device_grant(
                &s,
                &DeviceGrant {
                    member_id: mid(0xee),
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Revoked member.
        let with_member = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            },
        )
        .unwrap();
        let revoked_member = apply_member_revoke(
            &with_member,
            &MemberRevoke {
                member_id: mid(0xc0),
            },
        )
        .unwrap();
        assert_eq!(
            apply_device_grant(
                &revoked_member,
                &DeviceGrant {
                    member_id: mid(0xc0),
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn apply_device_grant_rejects_globally_duplicate_device_id() {
        let s = state();
        let admin = admin_key(0xa0).member_id();
        let other_member = mid(0xc0);
        let with_other = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: other_member,
                role: Role::Member,
            },
        )
        .unwrap();
        let dev = DeviceId::from_bytes([0xd2; N]);
        let with_device = apply_device_grant(
            &with_other,
            &DeviceGrant {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        // Cross-member duplicate: the device is already bound to `admin`.
        assert_eq!(
            apply_device_grant(
                &with_device,
                &DeviceGrant {
                    member_id: other_member,
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Same-member active duplicate: re-granting to the same owner.
        assert_eq!(
            apply_device_grant(
                &with_device,
                &DeviceGrant {
                    member_id: admin,
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Regranting a revoked device (even to the original owner) rejects.
        let revoked = apply_device_revoke(
            &with_device,
            &DeviceRevoke {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        assert_eq!(
            apply_device_grant(
                &revoked,
                &DeviceGrant {
                    member_id: admin,
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn apply_device_revoke_rejects_absent_wrong_owner_or_already_revoked() {
        let s = state();
        let admin = admin_key(0xa0).member_id();
        let dev = DeviceId::from_bytes([0xd3; N]);
        let with_device = apply_device_grant(
            &s,
            &DeviceGrant {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        // Absent device.
        assert_eq!(
            apply_device_revoke(
                &s,
                &DeviceRevoke {
                    member_id: admin,
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Wrong owner (device is bound to `admin`, not this member).
        let with_other = apply_member_grant(
            &with_device,
            &MemberGrant {
                member_id: mid(0xc1),
                role: Role::Member,
            },
        )
        .unwrap();
        assert_eq!(
            apply_device_revoke(
                &with_other,
                &DeviceRevoke {
                    member_id: mid(0xc1),
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
        // Already-revoked device.
        let revoked = apply_device_revoke(
            &with_device,
            &DeviceRevoke {
                member_id: admin,
                device_id: dev,
            },
        )
        .unwrap();
        assert_eq!(
            apply_device_revoke(
                &revoked,
                &DeviceRevoke {
                    member_id: admin,
                    device_id: dev,
                },
            )
            .err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn apply_admin_set_replaces_admins() {
        let s = state();
        let a = mid(0xa0);
        let b = mid(0xa1);
        let next = apply_admin_set(
            &s,
            &AdminSet {
                administrators: vec![a, b],
                threshold: 2,
            },
        )
        .unwrap();
        assert_eq!(next.administrators.administrators, vec![a, b]);
        assert_eq!(next.administrators.threshold, 2);
        // Threshold out of range rejects.
        assert!(apply_admin_set(
            &s,
            &AdminSet {
                administrators: vec![a],
                threshold: 2,
            }
        )
        .is_err());
    }

    #[test]
    fn apply_recovery_set_replaces_recovery() {
        let s = state();
        let next = apply_recovery_set(
            &s,
            &RecoverySet {
                recovery: RecoveryConfig {
                    threshold: 2,
                    recovery_keys: vec![mid(0x01), mid(0x02)],
                },
            },
        )
        .unwrap();
        assert_eq!(next.recovery.config.threshold, 2);
    }

    #[test]
    fn apply_replica_set_upserts_replica() {
        let s = state();
        let rid = ReplicaId::from_bytes([0x11; N]);
        let next = apply_replica_set(
            &s,
            &ReplicaSet {
                replica: ReplicaDescriptor {
                    replica_id: rid,
                    endpoint: vec![0xfe],
                    capability: 1,
                },
                status: ReplicaStatus::Active,
            },
        )
        .unwrap();
        assert!(next.replicas.contains_key(&rid));
    }

    #[test]
    fn apply_stream_create_inserts_stream() {
        let s = state();
        let sid = StreamId::from_bytes([0x22; N]);
        let next = apply_stream_create(
            &s,
            &StreamCreate {
                stream_id: sid,
                policy: StreamPolicy::default_policy(),
                created_at_ms: 1_000,
            },
        )
        .unwrap();
        assert!(next.streams.contains_key(&sid));
        // Duplicate stream id rejects.
        assert!(apply_stream_create(
            &next,
            &StreamCreate {
                stream_id: sid,
                policy: StreamPolicy::default_policy(),
                created_at_ms: 1_000,
            },
        )
        .is_err());
    }

    #[test]
    fn apply_stream_policy_set_replaces_policy() {
        let s = state();
        let sid = StreamId::from_bytes([0x22; N]);
        let with_stream = apply_stream_create(
            &s,
            &StreamCreate {
                stream_id: sid,
                policy: StreamPolicy::default_policy(),
                created_at_ms: 1_000,
            },
        )
        .unwrap();
        let next = apply_stream_policy_set(
            &with_stream,
            &StreamPolicySet {
                stream_id: sid,
                policy: StreamPolicy { access: 7 },
            },
        )
        .unwrap();
        assert_eq!(next.streams.get(&sid).unwrap().policy.access, 7);
    }

    #[test]
    fn apply_stream_archive_marks_archived() {
        let s = state();
        let sid = StreamId::from_bytes([0x22; N]);
        let with_stream = apply_stream_create(
            &s,
            &StreamCreate {
                stream_id: sid,
                policy: StreamPolicy::default_policy(),
                created_at_ms: 1_000,
            },
        )
        .unwrap();
        let next = apply_stream_archive(
            &with_stream,
            &StreamArchive {
                stream_id: sid,
                archived: true,
            },
        )
        .unwrap();
        assert!(next.streams.get(&sid).unwrap().archived);
    }

    #[test]
    fn apply_invite_revoke_adds_to_revoked_set() {
        let s = state();
        let next = apply_invite_revoke(
            &s,
            &InviteRevoke {
                invite_id: [0xaa; N],
            },
        )
        .unwrap();
        assert!(next.policy.revoked_invites.contains(&[0xaa; N]));
    }

    #[test]
    fn apply_policy_set_replaces_policy() {
        let s = state();
        let mut new_policy = CommunityPolicy::empty();
        new_policy.migrations.insert([0xbb; N]);
        let next = apply_policy_set(&s, &PolicySet { policy: new_policy }).unwrap();
        assert!(next.policy.migrations.contains(&[0xbb; N]));
    }

    #[test]
    fn apply_fork_resolve_records_marker() {
        let s = state();
        let marker = ForkResolutionMarker {
            evidence: [
                GovernanceId::from_bytes([1; N]),
                GovernanceId::from_bytes([2; N]),
            ],
            decision: 1,
            created_at_ms: 1_000,
        };
        let next = apply_fork_resolve(&s, &marker).unwrap();
        assert_eq!(next.policy.fork_markers.len(), 1);
        // Descending evidence pair rejects.
        let bad = ForkResolutionMarker {
            evidence: [marker.evidence[1], marker.evidence[0]],
            decision: 1,
            created_at_ms: 1_000,
        };
        assert!(apply_fork_resolve(&s, &bad).is_err());
    }

    #[test]
    fn apply_policy_set_preserves_append_only_markers() {
        // Review thread #5: revoked invites, fork markers, and migration
        // acceptances are accumulated history. A policy.set must not wipe them,
        // so it cannot be used to re-accept a duplicate migration.
        let s = state();
        let with_migration = apply_migration_accept(
            &s,
            &MigrationAccept {
                migration_id: [0xcc; N],
            },
        )
        .unwrap();
        let wiped = apply_policy_set(
            &with_migration,
            &PolicySet {
                policy: CommunityPolicy::empty(),
            },
        )
        .unwrap();
        assert!(
            wiped.policy.migrations.contains(&[0xcc; N]),
            "policy.set must not wipe append-only migration markers"
        );
        assert_eq!(
            apply_migration_accept(
                &wiped,
                &MigrationAccept {
                    migration_id: [0xcc; N]
                }
            )
            .err(),
            Some(Reject::InvalidContent),
            "a policy.set must not enable re-accepting a duplicate migration"
        );
    }

    #[test]
    fn apply_migration_accept_records_marker() {
        let s = state();
        let next = apply_migration_accept(
            &s,
            &MigrationAccept {
                migration_id: [0xcc; N],
            },
        )
        .unwrap();
        assert!(next.policy.migrations.contains(&[0xcc; N]));
        // Duplicate acceptance rejects.
        assert_eq!(
            apply_migration_accept(
                &next,
                &MigrationAccept {
                    migration_id: [0xcc; N]
                }
            )
            .err(),
            Some(Reject::InvalidContent)
        );
    }

    // --- Golden vector: full declared-root round trip ----------------------

    #[test]
    fn golden_vector_state_root_matches_declaration() {
        // Apply genesis → member.grant → verify the declared state_root on the
        // signed entry matches the recomputed root. This is the acceptance
        // golden vector (deterministic, synthetic, non-secret).
        let s = state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: mid(0xc0),
            role: Role::Member,
        });
        let new = apply(&s, &payload).unwrap();
        let declared = compute_state_root(&new);
        let body = GovernanceEntryBody {
            community_id: s.community_id,
            seq: 1,
            prev: None,
            created_at_ms: 1_234,
            kind: GovernanceOperationKind::MemberGrant,
            payload,
            state_root: declared,
        };
        // The entry id is stable and derived purely from the body CSB.
        let id = entry_id(&body);
        let recomputed = GovernanceId::from_governance_entry_csb(&entry_csb(&body));
        assert_eq!(id, recomputed);
        // Declared-root check succeeds.
        apply_verified_entry(&s, &body).expect("declared root matches recomputed root");
    }

    #[test]
    fn apply_unknown_payload_kind_is_impossible_at_compile_time() {
        // The closed payload enum makes "unknown kind reaches apply" impossible
        // at the type level. `apply` is total over the enum. The unknown-kind
        // rejection happens earlier, at decode (see records::tests).
        let s = state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: mid(0xc0),
            role: Role::Member,
        });
        assert!(apply(&s, &payload).is_ok());
    }

    // --- Pinned golden byte-vectors (regression anchor) --------------------
    //
    // The `golden_vector_state_root_matches_declaration` test above proves
    // self-consistency (recompute == declared), but a change to the hash
    // algorithm, the `GOVERNANCE_STATE` domain, the six-component order, or the
    // canonical-CBOR encoding would keep that test green while silently
    // changing every root on the wire. These vectors pin the *exact* bytes for
    // the deterministic, synthetic genesis fixture (`genesis_config`) so any
    // such change is caught. Bytes captured from the frozen #146 domains.

    #[test]
    fn golden_genesis_state_root_is_byte_pinned() {
        let s = state();
        assert_eq!(
            compute_state_root(&s).as_bytes(),
            &[
                0xdb, 0x7b, 0xc9, 0xca, 0x74, 0x58, 0xc1, 0x72, 0x73, 0x3f, 0x3a, 0x04, 0x20, 0x08,
                0xc0, 0x1f, 0x8b, 0x1f, 0xe0, 0x00, 0x43, 0xc0, 0xc6, 0x9c, 0x04, 0xa1, 0xc4, 0x28,
                0xb9, 0x09, 0x82, 0xe8,
            ],
            "post-genesis state root drifted; a hash/domain/encoding/order \
             change would break wire compatibility"
        );
    }

    #[test]
    fn golden_member_grant_state_root_is_byte_pinned() {
        let s = state();
        let new = apply_member_grant(
            &s,
            &MemberGrant {
                member_id: mid(0xc0),
                role: Role::Member,
            },
        )
        .unwrap();
        assert_eq!(
            compute_state_root(&new).as_bytes(),
            &[
                0x6d, 0x91, 0xd0, 0xed, 0xb2, 0xbe, 0xa4, 0x8f, 0x70, 0x68, 0xa9, 0xa3, 0xf0, 0x9f,
                0xc5, 0xaa, 0x9d, 0x02, 0xf4, 0x05, 0xbf, 0xd3, 0x44, 0x8c, 0x84, 0x3c, 0xa1, 0xbe,
                0x19, 0xe3, 0x2f, 0x2c,
            ],
            "genesis→member.grant state root drifted from the golden vector"
        );
    }

    // --- Six-component fixed-order commitment (spec §7.1) ------------------

    #[test]
    fn state_root_record_has_six_distinct_component_labels() {
        // Fixed order and count are normative (§7.1). A reorder or a relabel
        // would silently change the committed root; pin both here.
        assert_eq!(COMPONENT_LABELS.len(), 6);
        let unique: std::collections::BTreeSet<_> = COMPONENT_LABELS.iter().collect();
        assert_eq!(unique.len(), 6, "component labels must be distinct");
        assert_eq!(
            COMPONENT_LABELS,
            &[
                "administrators",
                "recovery",
                "replicas",
                "members_devices_roles",
                "stream_manifest",
                "community_policy",
            ]
        );
    }

    #[test]
    fn component_root_is_domain_separated_by_label() {
        // A hash valid for one component must not replay as another: the label
        // is part of the preimage (§7.2 / D7). Same value, different label ⇒
        // different root.
        let value = CborValue::Array(vec![]);
        let a = component_root("administrators", &value);
        let b = component_root("recovery", &value);
        assert_ne!(a, b, "component roots must be label-separated");
    }

    #[test]
    fn each_component_independently_affects_the_state_root() {
        // Perturbing any one of the six components must change the overall root
        // (proves all six are actually committed, not just a subset).
        let base = state();
        let base_root = compute_state_root(&base);

        // Component 4 (members) — grant a member.
        let members = apply_member_grant(
            &base,
            &MemberGrant {
                member_id: mid(0xc1),
                role: Role::Member,
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&members), base_root);

        // Component 3 (replicas).
        let replicas = apply_replica_set(
            &base,
            &ReplicaSet {
                replica: ReplicaDescriptor {
                    replica_id: ReplicaId::from_bytes([0x31; N]),
                    endpoint: vec![0x01],
                    capability: 1,
                },
                status: ReplicaStatus::Active,
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&replicas), base_root);

        // Component 5 (stream manifest).
        let streams = apply_stream_create(
            &base,
            &StreamCreate {
                stream_id: StreamId::from_bytes([0x32; N]),
                policy: StreamPolicy::default_policy(),
                created_at_ms: 1,
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&streams), base_root);

        // Component 6 (community policy).
        let policy = apply_invite_revoke(
            &base,
            &InviteRevoke {
                invite_id: [0x33; N],
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&policy), base_root);

        // Component 1 (administrators).
        let admins = apply_admin_set(
            &base,
            &AdminSet {
                administrators: vec![mid(0x34)],
                threshold: 1,
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&admins), base_root);

        // Component 2 (recovery).
        let recovery = apply_recovery_set(
            &base,
            &RecoverySet {
                recovery: RecoveryConfig {
                    threshold: 1,
                    recovery_keys: vec![mid(0x35)],
                },
            },
        )
        .unwrap();
        assert_ne!(compute_state_root(&recovery), base_root);
    }

    #[test]
    fn verify_state_root_accepts_matching_and_rejects_mismatch() {
        let s = state();
        let root = compute_state_root(&s);
        assert!(verify_state_root(&s, &root).is_ok());
        assert_eq!(
            verify_state_root(&s, &StateRoot::from_bytes([0x00; N])).err(),
            Some(Reject::StateRootMismatch)
        );
    }

    // --- Chain-link validation (spec D5) -----------------------------------

    #[test]
    fn check_chain_link_enforces_seq_prev_invariant() {
        let s = state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: mid(0xc0),
            role: Role::Member,
        });
        let first = GovernanceEntryBody {
            community_id: s.community_id,
            seq: 1,
            prev: None,
            created_at_ms: 1,
            kind: GovernanceOperationKind::MemberGrant,
            payload: payload.clone(),
            state_root: compute_state_root(&apply(&s, &payload).unwrap()),
        };
        // seq == 1 with prev == None is valid (expected_seq == 1).
        assert!(check_chain_link(&first, None, 1).is_ok());
        // seq == 1 with a non-None prev is rejected.
        let mut bad_first = first.clone();
        bad_first.prev = Some(GovernanceId::from_bytes([0x01; N]));
        assert_eq!(
            check_chain_link(&bad_first, None, 1).err(),
            Some(Reject::InvalidContent)
        );

        let prev_id = entry_id(&first);
        let second = GovernanceEntryBody {
            community_id: s.community_id,
            seq: 2,
            prev: Some(prev_id),
            created_at_ms: 2,
            kind: GovernanceOperationKind::MemberGrant,
            payload,
            state_root: first.state_root,
        };
        // seq > 1 with matching prev + expected_seq is valid.
        assert!(check_chain_link(&second, Some(prev_id), 2).is_ok());
        // seq > 1 with wrong prev is rejected.
        assert_eq!(
            check_chain_link(&second, Some(GovernanceId::from_bytes([0xff; N])), 2).err(),
            Some(Reject::InvalidContent)
        );
        // seq > 1 with None expected_prev is rejected.
        assert_eq!(
            check_chain_link(&second, None, 2).err(),
            Some(Reject::InvalidContent)
        );

        // Skipped sequence number (review thread #2): an entry at seq 3 that
        // links back to entry 1 must be rejected even though its `prev` matches
        // entry 1's id — seq 2 was never produced. The old check, which compared
        // only `prev`, accepted this non-contiguous chain.
        let mut skipped = second.clone();
        skipped.seq = 3;
        assert_eq!(
            check_chain_link(&skipped, Some(prev_id), 2).err(),
            Some(Reject::InvalidContent),
            "a non-contiguous seq (3 when 2 is expected) must be rejected"
        );
    }

    // --- Cross-community isolation (spec §6.4 / D3) ------------------------

    #[test]
    fn apply_verified_entry_rejects_foreign_community() {
        let s = state();
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id: mid(0xc0),
            role: Role::Member,
        });
        let new = apply(&s, &payload).unwrap();
        let body = GovernanceEntryBody {
            community_id: CommunityId::from_bytes([0x99; N]), // foreign community
            seq: 1,
            prev: None,
            created_at_ms: 1,
            kind: GovernanceOperationKind::MemberGrant,
            payload,
            state_root: compute_state_root(&new),
        };
        assert_eq!(
            apply_verified_entry(&s, &body).err(),
            Some(Reject::InvalidContent)
        );
    }

    // --- Determinism across apply order (pure-function property) -----------

    #[test]
    fn apply_is_order_sensitive_but_deterministic() {
        // Applying the same sequence of independent ops from identical start
        // states yields byte-identical roots (no hidden nondeterminism).
        let s = state();
        let grant = MemberGrant {
            member_id: mid(0xd1),
            role: Role::Member,
        };
        let a = apply_member_grant(&s, &grant).unwrap();
        let b = apply_member_grant(&s, &grant).unwrap();
        assert_eq!(compute_state_root(&a), compute_state_root(&b));
    }
}
