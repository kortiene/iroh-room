//! The #134 §8.1 projected member record and the projection over it (issue
//! #151, spec `v2-member-projection-sorted-merkle-map.md` §3.1–§3.2).
//!
//! [`ProjectedMemberRecord`] is the canonical §8.1 protocol record: one exact
//! deterministic-CBOR schema with closed field names and types (spec D3). Its
//! canonical bytes are the leaf preimage of [`super::sorted::SortedMerkleMap`].
//!
//! [`MemberMapProjection`] wraps the sorted map and owns the record → canonical
//! byte conversion. It supports full-build construction and the incremental
//! insert/replace/remove surface required by the issue's acceptance.
//!
//! The closed record schema contains `member_id`, `status`, `roles`,
//! `active_devices`, `grant_seq`, conditional `revoke_seq`, and optional
//! `profile`. Roles sort by their canonical wire text; devices sort by raw key.
//! Genesis members use `grant_seq = 0`, while post-genesis transitions use the
//! authenticated governance sequence.

use std::collections::BTreeSet;

use crate::cbor::CborValue;
use crate::error::Reject;
use crate::governance::log::{
    CommittedGovernanceTransition, CommittedGovernanceTransitionKind, DeviceStatus,
    GovernanceState, GovernanceTip, ValidatedGovernanceState,
};
use crate::ids::{CommunityId, DeviceId, MerkleRoot, PrincipalId, LEN};

use super::sorted::{
    empty_member_root, member_leaf_hash, verify_inclusion as verify_sorted, InclusionProof,
    SortedMerkleMap,
};

/// A member's status in the projected record (spec §8.1). Two states only;
/// revoked members are retained as tombstones (spec §3.1 #3 / D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectedStatus {
    /// Active member.
    Active,
    /// Revoked member (tombstoned; not physically removed from the committed map).
    Revoked,
}

impl ProjectedStatus {
    /// The canonical wire string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }

    fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "active" => Ok(Self::Active),
            "revoked" => Ok(Self::Revoked),
            _ => Err(Reject::InvalidContent),
        }
    }
}

/// A canonical role label (spec §8.1 role set). Closed to the existing
/// [`crate::governance::log::model::Role`] wire strings; tracked as text at the
/// canonical encoding boundary.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct RoleLabel(String);

impl RoleLabel {
    /// Construct from a canonical role wire string.
    #[must_use]
    pub fn new(s: &str) -> Self {
        Self(s.to_owned())
    }

    fn validate(&self) -> Result<(), Reject> {
        crate::governance::log::model::Role::parse(&self.0).map(|_| ())
    }

    /// Borrow the canonical role text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<crate::governance::log::model::Role> for RoleLabel {
    fn from(role: crate::governance::log::model::Role) -> Self {
        Self::new(role.as_str())
    }
}

/// An active device binding (spec §8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveDevice {
    /// The device public key.
    pub device_id: DeviceId,
    /// Opaque canonical binding metadata.
    pub binding: Vec<u8>,
}

impl ActiveDevice {
    /// Construct an active-device entry with empty binding metadata.
    #[must_use]
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            binding: Vec::new(),
        }
    }
}

/// The #134 §8.1 projected member record (spec D3). The canonical-CBOR bytes
/// of this record are the Merkle-leaf preimage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMemberRecord {
    /// The identity public key (the map sort key).
    pub member_id: PrincipalId,
    /// Active or revoked.
    pub status: ProjectedStatus,
    /// Canonical, non-empty (when active) role set, sorted + unique.
    pub roles: Vec<RoleLabel>,
    /// Active device bindings, sorted by raw `device_id`, unique.
    pub active_devices: Vec<ActiveDevice>,
    /// Grant sequence number. `0` for genesis admins (D4); post-genesis uses
    /// the accepted governance entry's `seq`.
    pub grant_seq: u64,
    /// Revocation sequence. `Some` iff status is revoked.
    pub revoke_seq: Option<u64>,
    /// Optional profile reference. `None` is omitted from the canonical bytes.
    pub profile: Option<Vec<u8>>,
}

impl ProjectedMemberRecord {
    /// Build an active genesis admin record (D4 genesis convention).
    #[must_use]
    pub fn genesis_admin(member_id: PrincipalId, role: RoleLabel) -> Self {
        Self {
            member_id,
            status: ProjectedStatus::Active,
            roles: vec![role],
            active_devices: Vec::new(),
            grant_seq: 0,
            revoke_seq: None,
            profile: None,
        }
    }

    /// Canonical-CBOR value of this record (the leaf preimage).
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        let mut entries: Vec<(String, CborValue)> = vec![
            (
                "member_id".to_owned(),
                CborValue::Bytes(self.member_id.as_bytes().to_vec()),
            ),
            (
                "status".to_owned(),
                CborValue::Text(self.status.as_str().to_owned()),
            ),
            (
                "roles".to_owned(),
                CborValue::Array(
                    self.roles
                        .iter()
                        .map(|r| CborValue::Text(r.as_str().to_owned()))
                        .collect(),
                ),
            ),
            (
                "active_devices".to_owned(),
                CborValue::Array(
                    self.active_devices
                        .iter()
                        .map(|d| {
                            CborValue::Map(vec![
                                (
                                    "device_id".to_owned(),
                                    CborValue::Bytes(d.device_id.as_bytes().to_vec()),
                                ),
                                ("binding".to_owned(), CborValue::Bytes(d.binding.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("grant_seq".to_owned(), CborValue::Uint(self.grant_seq)),
        ];
        if let Some(seq) = self.revoke_seq {
            entries.push(("revoke_seq".to_owned(), CborValue::Uint(seq)));
        }
        if let Some(profile) = &self.profile {
            entries.push(("profile".to_owned(), CborValue::Bytes(profile.clone())));
        }
        CborValue::Map(entries)
    }

    /// Canonical-CBOR bytes of this record (the exact leaf preimage).
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        crate::cbor::encode(&self.to_cbor())
    }

    /// Decode canonical record bytes through the strict deterministic-CBOR
    /// trust boundary and validate the complete closed schema.
    ///
    /// # Errors
    /// Returns [`Reject::NonCanonicalEncoding`] for malformed or non-canonical
    /// bytes and [`Reject::InvalidContent`] for schema or semantic violations.
    pub fn from_bytes(input: &[u8]) -> Result<Self, Reject> {
        let value =
            crate::cbor::decode_canonical(input).map_err(|_| Reject::NonCanonicalEncoding)?;
        Self::from_canonical(&value)
    }

    /// The leaf hash `BLAKE3(MEMBER_LEAF || canonical_record)`.
    #[must_use]
    pub fn leaf_hash(&self) -> [u8; LEN] {
        member_leaf_hash(&self.canonical_bytes())
    }

    /// Validate the record invariants (spec §3.1 / D3 additional rules).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] when:
    /// - an active member has an empty role set;
    /// - `revoke_seq` is present while status is active, or absent while revoked;
    /// - roles are duplicate/unsorted;
    /// - active devices are duplicate (by `device_id`) or unsorted.
    pub fn validate(&self) -> Result<(), Reject> {
        // Roles: non-empty when active, sorted + unique.
        if self.status == ProjectedStatus::Active && self.roles.is_empty() {
            return Err(Reject::InvalidContent);
        }
        if !is_sorted_unique(&self.roles) || self.roles.iter().any(|role| role.validate().is_err())
        {
            return Err(Reject::InvalidContent);
        }
        // Devices: sorted + unique by raw device_id.
        let mut prev: Option<&DeviceId> = None;
        for d in &self.active_devices {
            if let Some(p) = prev {
                if p >= &d.device_id {
                    return Err(Reject::InvalidContent);
                }
            }
            prev = Some(&d.device_id);
        }
        // Sequence cross-field rules.
        match (self.status, self.revoke_seq) {
            (ProjectedStatus::Active, Some(_)) | (ProjectedStatus::Revoked, None) => {
                return Err(Reject::InvalidContent);
            }
            (ProjectedStatus::Revoked, Some(seq)) if seq < self.grant_seq => {
                return Err(Reject::InvalidContent);
            }
            _ => {}
        }
        Ok(())
    }

    /// Decode a [`ProjectedMemberRecord`] from canonical CBOR, enforcing the
    /// closed schema and the §3.2 invariants (spec §3.2 #25: canonical decode
    /// first, then closed-schema + semantic checks).
    ///
    /// # Errors
    /// Returns [`Reject::NonCanonicalEncoding`] for non-canonical CBOR or a
    /// wrong field type/width; [`Reject::InvalidContent`] for a closed-schema,
    /// cross-field, or sort/uniqueness violation.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        validate_closed_keys(
            entries,
            &[
                "member_id",
                "status",
                "roles",
                "active_devices",
                "grant_seq",
                "revoke_seq",
                "profile",
            ],
        )?;
        let member_id_bytes = field_bytes(entries, "member_id")?;
        let member_id = PrincipalId::from_bytes(
            <[u8; LEN]>::try_from(member_id_bytes).map_err(|_| Reject::NonCanonicalEncoding)?,
        );
        let status = ProjectedStatus::parse(field_text(entries, "status")?)?;
        let roles = field_array(entries, "roles")?
            .iter()
            .map(|v| {
                v.as_text()
                    .map(RoleLabel::new)
                    .ok_or(Reject::NonCanonicalEncoding)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let devices_val = field_value(entries, "active_devices")?;
        let devices_arr = devices_val.as_array().ok_or(Reject::NonCanonicalEncoding)?;
        let mut active_devices = Vec::with_capacity(devices_arr.len());
        for dval in devices_arr {
            let dent = dval.as_map().ok_or(Reject::NonCanonicalEncoding)?;
            validate_closed_keys(dent, &["device_id", "binding"])?;
            let d_id_bytes = field_bytes(dent, "device_id")?;
            let device_id = DeviceId::from_bytes(
                <[u8; LEN]>::try_from(d_id_bytes).map_err(|_| Reject::NonCanonicalEncoding)?,
            );
            let binding = field_bytes(dent, "binding")?.to_vec();
            active_devices.push(ActiveDevice { device_id, binding });
        }
        let grant_seq = field_uint(entries, "grant_seq")?;
        let revoke_seq = match opt_field(entries, "revoke_seq") {
            Some(v) => Some(v.as_uint().ok_or(Reject::NonCanonicalEncoding)?),
            None => None,
        };
        let profile = match opt_field(entries, "profile") {
            Some(v) => Some(v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?.to_vec()),
            None => None,
        };
        let record = Self {
            member_id,
            status,
            roles,
            active_devices,
            grant_seq,
            revoke_seq,
            profile,
        };
        record.validate()?;
        Ok(record)
    }
}

fn validate_closed_keys(entries: &[(String, CborValue)], allowed: &[&str]) -> Result<(), Reject> {
    let mut seen = BTreeSet::new();
    for (key, _) in entries {
        if !allowed.contains(&key.as_str()) || !seen.insert(key.as_str()) {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

fn opt_field<'a>(entries: &'a [(String, CborValue)], key: &str) -> Option<&'a CborValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn field_value<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a CborValue, Reject> {
    opt_field(entries, key).ok_or(Reject::InvalidContent)
}

fn field_bytes<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a [u8], Reject> {
    field_value(entries, key)?
        .as_bytes()
        .ok_or(Reject::NonCanonicalEncoding)
}

fn field_text<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a str, Reject> {
    field_value(entries, key)?
        .as_text()
        .ok_or(Reject::NonCanonicalEncoding)
}

fn field_array<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Result<&'a [CborValue], Reject> {
    field_value(entries, key)?
        .as_array()
        .ok_or(Reject::NonCanonicalEncoding)
}

fn field_uint(entries: &[(String, CborValue)], key: &str) -> Result<u64, Reject> {
    field_value(entries, key)?
        .as_uint()
        .ok_or(Reject::NonCanonicalEncoding)
}

fn is_sorted_unique(roles: &[RoleLabel]) -> bool {
    let mut set: BTreeSet<&str> = BTreeSet::new();
    let mut prev: Option<&str> = None;
    for r in roles {
        if !set.insert(r.as_str()) {
            return false;
        }
        if let Some(p) = prev {
            if p > r.as_str() {
                return false;
            }
        }
        prev = Some(r.as_str());
    }
    true
}

fn project_governance_member(
    member: &crate::governance::log::MemberRecord,
) -> Result<ProjectedMemberRecord, Reject> {
    let status = match member.status {
        crate::governance::log::MemberStatus::Active => ProjectedStatus::Active,
        crate::governance::log::MemberStatus::Revoked => ProjectedStatus::Revoked,
    };
    let mut roles: Vec<RoleLabel> = member.roles.iter().copied().map(RoleLabel::from).collect();
    roles.sort();
    roles.dedup();
    let active_devices = member
        .devices
        .values()
        .filter(|device| device.status == DeviceStatus::Active)
        .map(|device| ActiveDevice {
            device_id: device.device_id,
            binding: device.binding.clone(),
        })
        .collect();
    let record = ProjectedMemberRecord {
        member_id: member.member_id,
        status,
        roles,
        active_devices,
        grant_seq: member.grant_seq,
        revoke_seq: member.revoke_seq,
        profile: member.profile.clone(),
    };
    record.validate()?;
    Ok(record)
}

// ----------------------------------------------------------------------------
// MemberMapProjection — owns the record ↔ canonical byte conversion and the
// incremental sorted map.
// ----------------------------------------------------------------------------

/// The projected member map: a [`SortedMerkleMap`] keyed by `PrincipalId` over
/// canonical [`ProjectedMemberRecord`] bytes (#134 §8.1 / §8.2).
///
/// The projection retains revoked members as tombstones (spec §3.1 #3 / D5):
/// revocation is a value replacement, not physical removal. Physical removal
/// exists for deterministic tests and the generic add/remove acceptance.
#[derive(Debug, Clone)]
pub struct MemberMapProjection {
    community_id: Option<CommunityId>,
    cursor: Option<GovernanceTip>,
    records: std::collections::BTreeMap<PrincipalId, ProjectedMemberRecord>,
    map: SortedMerkleMap,
}

/// The observable outcome of applying a committed governance transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionUpdate {
    /// The accepted operation did not affect the member projection.
    Unchanged,
    /// One member record changed incrementally.
    Updated {
        /// The changed identity.
        member_id: PrincipalId,
        /// Root before the transition.
        old_root: MerkleRoot,
        /// Root after the transition.
        new_root: MerkleRoot,
    },
    /// A fork resolution synchronized the projection to the selected lineage.
    Synchronized {
        /// Sorted identities whose projected records changed.
        changed_members: Vec<PrincipalId>,
        /// Root before synchronization.
        old_root: MerkleRoot,
        /// Root after synchronization.
        new_root: MerkleRoot,
    },
}

impl MemberMapProjection {
    /// An empty projection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            community_id: None,
            cursor: None,
            records: std::collections::BTreeMap::new(),
            map: SortedMerkleMap::new(),
        }
    }

    /// Build a projection by full-build from an iterator of records (spec D6
    /// recovery/test oracle). Each record is validated before insertion;
    /// duplicate identity keys reject.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] on a structurally invalid record or a
    /// duplicate identity.
    pub fn from_records<I>(records: I) -> Result<Self, Reject>
    where
        I: IntoIterator<Item = ProjectedMemberRecord>,
    {
        let mut records_by_id = std::collections::BTreeMap::new();
        for record in records {
            record.validate()?;
            if records_by_id.insert(record.member_id, record).is_some() {
                return Err(Reject::InvalidContent);
            }
        }
        let canonical: Vec<(PrincipalId, Vec<u8>)> = records_by_id
            .values()
            .map(|record| (record.member_id, record.canonical_bytes()))
            .collect();
        let map = SortedMerkleMap::from_validated_records(canonical)?;
        Ok(Self {
            community_id: None,
            cursor: None,
            records: records_by_id,
            map,
        })
    }

    /// Rebuild a projection from a committed governance snapshot.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the snapshot cannot be represented
    /// as canonical §8.1 member records.
    pub fn from_validated_state(state: &ValidatedGovernanceState) -> Result<Self, Reject> {
        let mut projection = Self::from_governance_state(state.state())?;
        projection.community_id = Some(state.state().community_id);
        projection.cursor = Some(state.tip());
        Ok(projection)
    }

    /// The committed governance cursor maintained by this projection.
    #[must_use]
    pub fn cursor(&self) -> Option<GovernanceTip> {
        self.cursor
    }

    pub(crate) fn from_governance_state(state: &GovernanceState) -> Result<Self, Reject> {
        let records = state
            .members
            .values()
            .map(project_governance_member)
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_records(records)
    }

    /// Apply a transition emitted after the fork-aware governance machine has
    /// atomically committed it.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the transition does not extend this
    /// projection's exact community/cursor or if either committed snapshot is
    /// inconsistent with the maintained projection.
    pub fn apply_committed(
        &mut self,
        transition: &CommittedGovernanceTransition,
    ) -> Result<ProjectionUpdate, Reject> {
        let expected_community = self.community_id.ok_or(Reject::InvalidContent)?;
        if transition.prior().state().community_id != expected_community
            || transition.next().state().community_id != expected_community
            || self.cursor != Some(transition.prior().tip())
            || self.root() != Self::from_governance_state(transition.prior().state())?.root()
        {
            return Err(Reject::InvalidContent);
        }

        let mut candidate = self.clone();
        let update = match transition.kind() {
            CommittedGovernanceTransitionKind::LinearAdvance => {
                let prior_members = &transition.prior().state().members;
                let next_members = &transition.next().state().members;
                let changed: Vec<PrincipalId> = prior_members
                    .keys()
                    .chain(next_members.keys())
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter(|member_id| prior_members.get(member_id) != next_members.get(member_id))
                    .collect();
                if changed.len() > 1 {
                    return Err(Reject::InvalidContent);
                }
                let old_root = candidate.root();
                let update = if let Some(member_id) = changed.first().copied() {
                    let record = next_members
                        .get(&member_id)
                        .map(project_governance_member)
                        .transpose()?;
                    match (candidate.records.contains_key(&member_id), record) {
                        (false, Some(record)) => candidate.insert_new(record)?,
                        (true, Some(record)) => candidate.replace_existing(record)?,
                        (true, None) => {
                            candidate.remove_existing(&member_id)?;
                        }
                        (false, None) => return Err(Reject::InvalidContent),
                    }
                    ProjectionUpdate::Updated {
                        member_id,
                        old_root,
                        new_root: candidate.root(),
                    }
                } else {
                    ProjectionUpdate::Unchanged
                };
                if candidate.root()
                    != Self::from_governance_state(transition.next().state())?.root()
                {
                    return Err(Reject::InvalidContent);
                }
                candidate.cursor = Some(transition.next().tip());
                update
            }
            CommittedGovernanceTransitionKind::ForkResolution => {
                let replacement = Self::from_validated_state(transition.next())?;
                let changed_members = candidate
                    .records
                    .keys()
                    .chain(replacement.records.keys())
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter(|member_id| {
                        candidate.records.get(member_id) != replacement.records.get(member_id)
                    })
                    .collect();
                let update = ProjectionUpdate::Synchronized {
                    changed_members,
                    old_root: candidate.root(),
                    new_root: replacement.root(),
                };
                candidate = replacement;
                update
            }
        };
        *self = candidate;
        Ok(update)
    }

    /// The committed Merkle root.
    #[must_use]
    pub fn root(&self) -> MerkleRoot {
        self.map.root()
    }

    /// The empty-tree root (matches [`MemberMapProjection::root`] when empty).
    #[must_use]
    pub fn empty_root() -> MerkleRoot {
        empty_member_root()
    }

    /// Number of projected records (active + revoked tombstones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the projection is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Borrow a record by identity.
    #[must_use]
    pub fn get(&self, id: &PrincipalId) -> Option<&ProjectedMemberRecord> {
        self.records.get(id)
    }

    /// Borrow all records in raw-identity order.
    #[must_use]
    pub fn records(&self) -> &std::collections::BTreeMap<PrincipalId, ProjectedMemberRecord> {
        &self.records
    }

    /// Borrow the underlying sorted map (for proofs/inspection).
    #[must_use]
    pub fn map(&self) -> &SortedMerkleMap {
        &self.map
    }

    /// Insert a brand-new member record (incremental). Validates, encodes once,
    /// and updates only the affected subtree suffix — no full rebuild.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] on a structurally invalid record or a
    /// duplicate identity.
    pub fn insert_new(&mut self, record: ProjectedMemberRecord) -> Result<(), Reject> {
        record.validate()?;
        let id = record.member_id;
        let canonical = record.canonical_bytes();
        self.map.insert_new(id, canonical)?;
        self.records.insert(id, record);
        Ok(())
    }

    /// Replace an existing member record (incremental). Re-encodes only this
    /// record and recomputes its `O(log n)` path — every other leaf is reused.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] on a structurally invalid record or
    /// an absent identity, or if the replacement's identity differs from the
    /// stored one.
    pub fn replace_existing(&mut self, record: ProjectedMemberRecord) -> Result<(), Reject> {
        record.validate()?;
        if !self.records.contains_key(&record.member_id) {
            return Err(Reject::InvalidContent);
        }
        let id = record.member_id;
        let canonical = record.canonical_bytes();
        self.map.replace_existing(&id, canonical)?;
        self.records.insert(id, record);
        Ok(())
    }

    /// Physically remove a member record (incremental). Returns the removed
    /// record. Generic map deletion; member revocation is a value replacement.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the identity is absent.
    pub fn remove_existing(&mut self, id: &PrincipalId) -> Result<ProjectedMemberRecord, Reject> {
        // Remove from the projection record map first; if the low-level map
        // then rejects (absent identity), restore the record so the operation
        // is atomic (spec §3.4 #38 — no partial mutation escapes).
        let record = self.records.remove(id).ok_or(Reject::InvalidContent)?;
        match self.map.remove_existing(id) {
            Ok(_) => Ok(record),
            Err(e) => {
                self.records.insert(*id, record);
                Err(e)
            }
        }
    }

    /// Build an inclusion proof for `id` (spec §3.5). Returns `None` for an
    /// absent identity — no proof is fabricated.
    #[must_use]
    pub fn prove(&self, id: &PrincipalId) -> Option<InclusionProof> {
        self.map.prove(id)
    }

    /// Verify an inclusion proof for `id` against the current root, binding the
    /// proof to both the requested identity and the stored canonical record
    /// (spec §3.5 #44 / #46).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidMerkleProof`] for any structural fault, an
    /// identity mismatch, a wrong record, or a root mismatch.
    pub fn verify_member_inclusion(
        &self,
        id: &PrincipalId,
        proof: &InclusionProof,
    ) -> Result<(), Reject> {
        let record = self.records.get(id).ok_or(Reject::InvalidMerkleProof)?;
        let canonical = record.canonical_bytes();
        verify_sorted(&self.map.root(), id, &canonical, proof)
    }

    /// Verify a typed record and proof against a caller-supplied public root.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidMerkleProof`] for an identity mismatch, invalid
    /// record, malformed proof, or root mismatch.
    pub fn verify_inclusion(
        root: &MerkleRoot,
        id: &PrincipalId,
        record: &ProjectedMemberRecord,
        proof: &InclusionProof,
    ) -> Result<(), Reject> {
        record.validate().map_err(|_| Reject::InvalidMerkleProof)?;
        if record.member_id != *id {
            return Err(Reject::InvalidMerkleProof);
        }
        verify_sorted(root, id, &record.canonical_bytes(), proof)
    }
}

impl Default for MemberMapProjection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::log::model::Role;
    use crate::governance::log::{
        sign_genesis, validated_genesis_state, GovernanceApproval, GovernanceEntry,
        GovernanceEntryBody, GovernanceMachine, GovernanceOperationPayload, MemberGrant,
        RecoveryConfig, GENESIS_SCHEMA_VERSION,
    };
    use crate::keys::SigningKey;

    fn id(byte: u8) -> PrincipalId {
        PrincipalId::from_bytes([byte; LEN])
    }

    fn active(byte: u8) -> ProjectedMemberRecord {
        ProjectedMemberRecord::genesis_admin(id(byte), RoleLabel::from(Role::Member))
    }

    fn signing_key(byte: u8) -> SigningKey {
        SigningKey::from_seed(&[byte; LEN])
    }

    fn committed_genesis() -> ValidatedGovernanceState {
        let admin = signing_key(0xa0);
        let config = crate::governance::log::GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1,
            genesis_nonce: [0xab; LEN],
            admin_threshold: 1,
            administrators: vec![admin.member_id()],
            recovery: RecoveryConfig::empty(),
            replicas: Vec::new(),
            community_policy: crate::governance::log::CommunityPolicy::empty(),
        };
        let signature = sign_genesis(&config, &admin);
        validated_genesis_state(&config, &[signature]).unwrap()
    }

    fn verified_grant(
        previous: &ValidatedGovernanceState,
        member_id: PrincipalId,
    ) -> crate::governance::log::VerifiedGovernanceEntry {
        let admin = signing_key(0xa0);
        let payload = GovernanceOperationPayload::MemberGrant(MemberGrant {
            member_id,
            roles: vec![Role::Member],
            profile: None,
        });
        let next = crate::governance::log::apply_entry(previous.state(), 1, &payload).unwrap();
        let body = GovernanceEntryBody {
            community_id: previous.state().community_id,
            seq: 1,
            prev: None,
            created_at_ms: 2,
            kind: payload.kind(),
            payload,
            state_root: crate::governance::log::compute_state_root(&next),
        };
        let entry = GovernanceEntry::new(body, &admin, Vec::<GovernanceApproval>::new());
        crate::governance::log::verify_governance_entry(&entry).unwrap()
    }

    #[test]
    fn canonical_bytes_are_independent_of_source_collection_order() {
        let r1 = active(0x01);
        let r2 = ProjectedMemberRecord {
            member_id: r1.member_id,
            status: r1.status,
            roles: r1.roles.clone(),
            active_devices: r1.active_devices.clone(),
            grant_seq: r1.grant_seq,
            revoke_seq: r1.revoke_seq,
            profile: r1.profile.clone(),
        };
        assert_eq!(r1.canonical_bytes(), r2.canonical_bytes());
    }

    #[test]
    fn active_record_with_empty_roles_rejects_validation() {
        let mut r = active(0x01);
        r.roles = Vec::new();
        assert_eq!(r.validate(), Err(Reject::InvalidContent));
    }

    #[test]
    fn revoked_record_must_carry_revoke_seq() {
        let mut r = active(0x01);
        r.status = ProjectedStatus::Revoked;
        r.revoke_seq = None;
        assert_eq!(r.validate(), Err(Reject::InvalidContent));
        r.revoke_seq = Some(r.grant_seq + 5);
        assert!(r.validate().is_ok());
    }

    #[test]
    fn duplicate_device_rejects_validation() {
        let dev = DeviceId::from_bytes([0xd0; LEN]);
        let mut r = active(0x01);
        r.active_devices = vec![ActiveDevice::new(dev), ActiveDevice::new(dev)];
        assert_eq!(r.validate(), Err(Reject::InvalidContent));
    }

    #[test]
    fn projection_incremental_matches_full_build() {
        let mut proj = MemberMapProjection::new();
        let mut oracle: Vec<ProjectedMemberRecord> = Vec::new();
        for b in [0x07u8, 0x02, 0x09, 0x01, 0x05] {
            let rec = active(b);
            proj.insert_new(rec.clone()).unwrap();
            oracle.push(rec);
        }
        let rebuild = MemberMapProjection::from_records(oracle).unwrap();
        assert_eq!(proj.root(), rebuild.root());
    }

    #[test]
    fn projection_proof_verifies_and_rebinds() {
        let mut proj = MemberMapProjection::new();
        proj.insert_new(active(0x01)).unwrap();
        proj.insert_new(active(0x02)).unwrap();
        let proof = proj.prove(&id(0x01)).expect("present");
        proj.verify_member_inclusion(&id(0x01), &proof).unwrap();
        assert_eq!(
            proj.verify_member_inclusion(&id(0xff), &proof),
            Err(Reject::InvalidMerkleProof)
        );
    }

    #[test]
    fn strict_record_decoder_rejects_noncanonical_and_duplicate_fields() {
        let record = active(0x01);
        let bytes = record.canonical_bytes();
        assert_eq!(ProjectedMemberRecord::from_bytes(&bytes).unwrap(), record);
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            ProjectedMemberRecord::from_bytes(&trailing),
            Err(Reject::NonCanonicalEncoding)
        );
        let duplicate = CborValue::Map(vec![
            (
                "member_id".to_owned(),
                CborValue::Bytes(id(0x01).as_bytes().to_vec()),
            ),
            (
                "member_id".to_owned(),
                CborValue::Bytes(id(0x01).as_bytes().to_vec()),
            ),
        ]);
        assert_eq!(
            ProjectedMemberRecord::from_canonical(&duplicate),
            Err(Reject::InvalidContent)
        );
    }

    #[test]
    fn raw_map_rejects_key_record_mismatch_atomically() {
        let record = active(0x01);
        let mut map = SortedMerkleMap::new();
        let before = map.root();
        assert_eq!(
            map.insert_new(id(0x02), record.canonical_bytes()),
            Err(Reject::InvalidContent)
        );
        assert_eq!(map.root(), before);
        assert!(map.is_empty());
    }

    #[test]
    fn committed_linear_transition_updates_projection_once() {
        let genesis = committed_genesis();
        let mut machine = GovernanceMachine::from_genesis(genesis.clone());
        let mut projection = MemberMapProjection::from_validated_state(&genesis).unwrap();
        let member_id = id(0xc0);
        let entry = verified_grant(&genesis, member_id);
        let (_, transition) = machine.observe_committed(&entry).unwrap();
        let transition = transition.unwrap();
        let update = projection.apply_committed(&transition).unwrap();
        assert!(matches!(
            update,
            ProjectionUpdate::Updated {
                member_id: changed,
                ..
            } if changed == member_id
        ));
        assert_eq!(projection.cursor(), Some(transition.next().tip()));
        assert_eq!(
            projection.root(),
            MemberMapProjection::from_validated_state(transition.next())
                .unwrap()
                .root()
        );
        let before = projection.root();
        assert_eq!(
            projection.apply_committed(&transition),
            Err(Reject::InvalidContent)
        );
        assert_eq!(projection.root(), before);
    }
}
