//! Core governance data model: roles, member records, governance actions, fork
//! evidence, and the pure [`GovernanceState`] (spec §6.4, §6.5, §7; #147/#148).
//!
//! # Action-set assumption (OQ-3)
//!
//! `#134 §7` action/threshold text is unavailable (spec §13). This module
//! defines a closed [`GovernanceAction`] set that is general enough to cover the
//! v1 membership surface and the spec's described authorization model, with a
//! default-deny engine in [`super::authz`]:
//!
//! - `InitRoom` — establish the room + its single admin (genesis).
//! - `AddMember` / `RemoveMember` / `SetRole` — admin-gated membership writes.
//! - `SetPolicy` — change the approval policy (admin-gated).
//! - `RotateDevice` — bind a new device key to a principal.
//!
//! Unknown actions are rejected ([`crate::Reject::UnknownRecordKind`]). When
//! `#134` lands, the canonical action set replaces this set verbatim; the
//! authorization engine is table-driven so adding an arm is a localized change.

use std::collections::BTreeMap;

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::ids::{DeviceId, GovernanceEntryId, MemberId, RoomId, LEN};
use crate::signed::{self, opt, Envelope, SignedBody};

/// A member role (spec §6.5 / OQ-3). `Ord` models privilege: admin is most
/// privileged. The least-privilege merge in the fold uses this ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// An invited-but-not-joined or removed member; no write privileges.
    None,
    /// A read/write member.
    Member,
    /// A privileged agent member.
    Agent,
    /// The room administrator (single-admin model; OQ-3/#134 may extend).
    Admin,
}

impl Role {
    /// The wire string for a role.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Member => "member",
            Self::Agent => "agent",
            Self::Admin => "admin",
        }
    }

    /// Parse a role from its wire string.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] for an unknown role string.
    pub fn parse(s: &str) -> Result<Self, Reject> {
        match s {
            "none" => Ok(Self::None),
            "member" => Ok(Self::Member),
            "agent" => Ok(Self::Agent),
            "admin" => Ok(Self::Admin),
            _ => Err(Reject::InvalidContent),
        }
    }
}

/// The live set of approvals required to authorize a governance action (spec
/// §6.4 / OQ-3). Default policy is "admin may act alone" (single-admin MVP);
/// `m_of_n` expresses a quorum once `#134` finalizes thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// The room admin may authorize the action alone (default).
    AdminAlone,
    /// `m` of the `n` listed principals must approve (quorum). Listed principals
    /// are stored in deterministic member-id order.
    MOfN {
        /// Required approval count.
        m: u64,
        /// Eligible approver set (deterministic order).
        approvers: Vec<MemberId>,
    },
}

impl ApprovalPolicy {
    /// The default policy: admin acts alone.
    #[must_use]
    pub const fn default_policy() -> Self {
        Self::AdminAlone
    }
}

/// A member's status (spec §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberStatus {
    /// Removed/inactive; no privileges.
    Removed,
    /// Invited but not yet active.
    Invited,
    /// Active member.
    Active,
}

/// The per-member governance record (a subset of the spec §6.5 `MemberLeaf`;
/// the full projection lives in [`crate::member`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The principal identity.
    pub member_id: MemberId,
    /// The member's current role.
    pub role: Role,
    /// The member's current status.
    pub status: MemberStatus,
    /// The device key(s) bound to this principal (OQ-2 single-key model: one).
    pub devices: Vec<DeviceId>,
    /// The governance entry id that last touched this member (cursor).
    pub governance_cursor: GovernanceEntryId,
}

/// Evidence of a same-author governance fork (spec §4 D6, #149). Two accepted
/// entries from the same author at the same sequence number (or claiming the
/// same parent) constitute a fork.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForkEvidence {
    /// The author of both conflicting entries.
    pub author: MemberId,
    /// The conflicting entry ids (exactly two; ordered).
    pub conflicting: [GovernanceEntryId; 2],
    /// The author sequence number at which the conflict occurred.
    pub seq: u64,
    /// Resolution state: unresolved until an authorized `fork.resolve` lands.
    pub resolved: bool,
}

/// The pure governance state (spec §6.4). A value of this type is the sole input
/// to the authorization engine and the state-root computation; it never reads a
/// wall clock or an external store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceState {
    /// The room this state belongs to.
    pub room_id: RoomId,
    /// The single immutable room admin (`None` before genesis).
    pub admin: Option<MemberId>,
    /// Members keyed by principal id (deterministic order).
    pub members: BTreeMap<MemberId, MemberRecord>,
    /// Per-author governance tip: `(seq, last_entry_id)`.
    pub tips: BTreeMap<MemberId, (u64, GovernanceEntryId)>,
    /// Unresolved fork evidence (spec D6; included in the state root per OQ-4).
    pub forks: Vec<ForkEvidence>,
    /// The current approval policy (spec §6.4 / OQ-3).
    pub policy: ApprovalPolicy,
}

impl GovernanceState {
    /// An empty state for `room_id` (pre-genesis).
    #[must_use]
    pub fn empty(room_id: RoomId) -> Self {
        Self {
            room_id,
            admin: None,
            members: BTreeMap::new(),
            tips: BTreeMap::new(),
            forks: Vec::new(),
            policy: ApprovalPolicy::default_policy(),
        }
    }

    /// The author's current sequence number (0 if unseen).
    #[must_use]
    pub fn author_seq(&self, author: &MemberId) -> u64 {
        self.tips.get(author).map_or(0, |(s, _)| *s)
    }

    /// The author's last accepted entry id.
    #[must_use]
    pub fn author_tip(&self, author: &MemberId) -> Option<GovernanceEntryId> {
        self.tips.get(author).map(|(_, id)| *id)
    }

    /// Whether `author` has any unresolved fork.
    #[must_use]
    pub fn has_unresolved_fork(&self, author: &MemberId) -> bool {
        self.forks
            .iter()
            .any(|f| &f.author == author && !f.resolved)
    }

    /// The member record for `member_id`, if present.
    #[must_use]
    pub fn member(&self, member_id: &MemberId) -> Option<&MemberRecord> {
        self.members.get(member_id)
    }

    /// Whether `member_id` is an active member.
    #[must_use]
    pub fn is_active(&self, member_id: &MemberId) -> bool {
        self.members
            .get(member_id)
            .is_some_and(|m| m.status == MemberStatus::Active)
    }
}

// ----------------------------------------------------------------------------
// The governance entry body (spec §6.3 / #147)
// ----------------------------------------------------------------------------

/// A governance action (spec §7 / OQ-3). See module docs for the closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceAction {
    /// Establish the room and its single admin. Valid only as the first entry.
    InitRoom {
        /// The admin principal.
        admin: MemberId,
        /// Admin's initial device key.
        admin_device: DeviceId,
        /// Human-readable room name (capped).
        room_name: String,
    },
    /// Add a member with a role.
    AddMember {
        /// The member to add.
        member: MemberId,
        /// The member's device key.
        device: DeviceId,
        /// The granted role.
        role: Role,
    },
    /// Remove a member.
    RemoveMember {
        /// The member to remove.
        member: MemberId,
    },
    /// Change a member's role.
    SetRole {
        /// The member whose role changes.
        member: MemberId,
        /// The new role.
        role: Role,
    },
    /// Bind a new device key to a principal.
    RotateDevice {
        /// The principal.
        member: MemberId,
        /// The new device key.
        device: DeviceId,
    },
    /// Change the room's approval policy.
    SetPolicy {
        /// The new policy, encoded canonically.
        policy: ApprovalPolicy,
    },
}

impl GovernanceAction {
    /// The wire discriminant string (closed registry; unknown → reject).
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::InitRoom { .. } => "init_room",
            Self::AddMember { .. } => "add_member",
            Self::RemoveMember { .. } => "remove_member",
            Self::SetRole { .. } => "set_role",
            Self::RotateDevice { .. } => "rotate_device",
            Self::SetPolicy { .. } => "set_policy",
        }
    }

    /// Parse the discriminant back, returning `None` for an unknown kind.
    #[must_use]
    pub fn kind_from_str(s: &str) -> Option<&str> {
        match s {
            "init_room" | "add_member" | "remove_member" | "set_role" | "rotate_device"
            | "set_policy" => Some(s),
            _ => None,
        }
    }
}

/// The canonical governance entry body (spec §6.3 / D2). Every field is signed.
///
/// `seq` is the author's per-author sequence number (starts at 1 for the first
/// entry); `parent` is the author's previous entry id (`None` for genesis). Fork
/// detection compares `(author, seq)` and `parent` (spec §4 D6 / #149).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceEntryBody {
    /// Schema version (MUST be `2` for v2; spec D8/#158).
    pub schema_version: u64,
    /// The room this entry belongs to.
    pub room_id: RoomId,
    /// The signing author/principal.
    pub author: MemberId,
    /// The author's sequence number for this entry (1-based).
    pub seq: u64,
    /// The author's previous entry id (`None` for genesis).
    pub parent: Option<GovernanceEntryId>,
    /// The signed epoch (ms since Unix epoch); advisory, never a wall clock.
    pub epoch: u64,
    /// The governance action.
    pub action: GovernanceAction,
}

/// The registered v2 schema version (spec D8 / #158). The shipped v1 protocol
/// still hard-enforces `schema_version == 1`; this constant is for v2 bodies
/// only and does not affect the v1 protocol (P-26/D-9 gate).
pub const SCHEMA_VERSION: u64 = 2;

impl SignedBody for GovernanceEntryBody {
    type Id = GovernanceEntryId;
    const SIGN_CONTEXT: &'static [u8] = domain::GOVERNANCE_ENTRY_SIGN;
    const ID_CONTEXT: &'static [u8] = domain::GOVERNANCE_ENTRY_ID;

    fn to_cbor(&self) -> CborValue {
        let mut entries = vec![
            (
                "schema_version".to_owned(),
                CborValue::Uint(self.schema_version),
            ),
            (
                "room_id".to_owned(),
                CborValue::Bytes(self.room_id.as_bytes().to_vec()),
            ),
            (
                "author".to_owned(),
                CborValue::Bytes(self.author.as_bytes().to_vec()),
            ),
            ("seq".to_owned(), CborValue::Uint(self.seq)),
            ("epoch".to_owned(), CborValue::Uint(self.epoch)),
            (
                "kind".to_owned(),
                CborValue::Text(self.action.kind_str().to_owned()),
            ),
        ];
        if let Some(p) = &self.parent {
            entries.push(("parent".to_owned(), CborValue::Bytes(p.as_bytes().to_vec())));
        }
        // Omit-when-absent: a missing `parent` is genesis (canonical re-encode
        // relies on optionals being absent, not null).
        entries.push(("action".to_owned(), action_to_cbor(&self.action)));
        CborValue::Map(entries)
    }

    fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        reject_unknown_keys(
            entries,
            &[
                "schema_version",
                "room_id",
                "author",
                "seq",
                "epoch",
                "kind",
                "parent",
                "action",
            ],
        )?;
        let schema_version = entries
            .iter()
            .find(|(k, _)| k == "schema_version")
            .and_then(|(_, v)| v.as_uint())
            .ok_or(Reject::NonCanonicalEncoding)?;
        if schema_version != SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let room_id = read_id_field(entries, "room_id")?;
        let author = read_member_field(entries, "author")?;
        let seq = uint_field(entries, "seq")?;
        let epoch = uint_field(entries, "epoch")?;
        let kind = text_field(entries, "kind")?;
        let parent = match opt(entries, "parent") {
            Some(v) => {
                let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
                Some(GovernanceEntryId::from_bytes(arr))
            }
            None => None,
        };
        let action_val = opt(entries, "action").ok_or(Reject::NonCanonicalEncoding)?;
        let action = action_from_cbor(kind, action_val)?;
        Ok(Self {
            schema_version,
            room_id,
            author,
            seq,
            parent,
            epoch,
            action,
        })
    }

    fn id_from_csb(csb: &[u8]) -> Self::Id {
        GovernanceEntryId::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
    }
}

/// A signed governance entry envelope.
pub type SignedGovernanceEntry = Envelope<GovernanceEntryId>;

/// Decode + verify a signed governance entry end-to-end (spec D2 / §6.3).
///
/// # Errors
/// See [`signed::verify_envelope`]; additionally enforces `schema_version == 2`.
pub fn decode_verified(env: &SignedGovernanceEntry) -> Result<GovernanceEntryBody, Reject> {
    signed::verify_envelope::<GovernanceEntryBody>(env)
}

// ---- CBOR field helpers (shared across governance bodies) -------------------

pub(crate) fn read_id_field(entries: &[(String, CborValue)], key: &str) -> Result<RoomId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(RoomId::from_bytes(arr))
}

pub(crate) fn read_member_field(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<MemberId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(MemberId::from_bytes(arr))
}

/// Reject a signed-record body (or nested map) that carries any key outside its
/// exact known set (spec D8 / §11: unknown keys MUST be rejected, never ignored).
///
/// Silently ignoring extra keys is a signature-malleability / parser-differential
/// risk: an attacker can inject keys that verify under one parser but are dropped
/// by another. Every `from_canonical` (and each nested action/policy map) enforces
/// an exact known-key-set through this helper, mirroring the strict `Fields::finish`
/// discipline already used by the content inner body.
pub(crate) fn reject_unknown_keys(
    entries: &[(String, CborValue)],
    allowed: &[&str],
) -> Result<(), Reject> {
    for (k, _) in entries {
        if !allowed.contains(&k.as_str()) {
            return Err(Reject::NonCanonicalEncoding);
        }
    }
    Ok(())
}

pub(crate) fn uint_field(entries: &[(String, CborValue)], key: &str) -> Result<u64, Reject> {
    signed::field(entries, key)
        .and_then(super::super::cbor::CborValue::as_uint)
        .ok_or(Reject::NonCanonicalEncoding)
}

pub(crate) fn text_field<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Result<&'a str, Reject> {
    signed::field(entries, key)
        .and_then(|v| v.as_text())
        .ok_or(Reject::NonCanonicalEncoding)
}

fn action_to_cbor(action: &GovernanceAction) -> CborValue {
    match action {
        GovernanceAction::InitRoom {
            admin,
            admin_device,
            room_name,
        } => CborValue::Map(vec![
            (
                "admin".to_owned(),
                CborValue::Bytes(admin.as_bytes().to_vec()),
            ),
            (
                "admin_device".to_owned(),
                CborValue::Bytes(admin_device.as_bytes().to_vec()),
            ),
            ("room_name".to_owned(), CborValue::Text(room_name.clone())),
        ]),
        GovernanceAction::AddMember {
            member,
            device,
            role,
        } => CborValue::Map(vec![
            (
                "member".to_owned(),
                CborValue::Bytes(member.as_bytes().to_vec()),
            ),
            (
                "device".to_owned(),
                CborValue::Bytes(device.as_bytes().to_vec()),
            ),
            ("role".to_owned(), CborValue::Text(role.as_str().to_owned())),
        ]),
        GovernanceAction::RemoveMember { member } => CborValue::Map(vec![(
            "member".to_owned(),
            CborValue::Bytes(member.as_bytes().to_vec()),
        )]),
        GovernanceAction::SetRole { member, role } => CborValue::Map(vec![
            (
                "member".to_owned(),
                CborValue::Bytes(member.as_bytes().to_vec()),
            ),
            ("role".to_owned(), CborValue::Text(role.as_str().to_owned())),
        ]),
        GovernanceAction::RotateDevice { member, device } => CborValue::Map(vec![
            (
                "member".to_owned(),
                CborValue::Bytes(member.as_bytes().to_vec()),
            ),
            (
                "device".to_owned(),
                CborValue::Bytes(device.as_bytes().to_vec()),
            ),
        ]),
        GovernanceAction::SetPolicy { policy } => policy_to_cbor(policy),
    }
}

fn policy_to_cbor(policy: &ApprovalPolicy) -> CborValue {
    match policy {
        ApprovalPolicy::AdminAlone => CborValue::Map(vec![(
            "type".to_owned(),
            CborValue::Text("admin_alone".to_owned()),
        )]),
        ApprovalPolicy::MOfN { m, approvers } => CborValue::Map(vec![
            ("type".to_owned(), CborValue::Text("m_of_n".to_owned())),
            ("m".to_owned(), CborValue::Uint(*m)),
            (
                "approvers".to_owned(),
                CborValue::Array(
                    approvers
                        .iter()
                        .map(|a| CborValue::Bytes(a.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
        ]),
    }
}

fn action_from_cbor(kind: &str, value: &CborValue) -> Result<GovernanceAction, Reject> {
    let entries = value.as_map().ok_or(Reject::InvalidContent)?;
    match kind {
        "init_room" => {
            reject_unknown_action_keys(entries, &["admin", "admin_device", "room_name"])?;
            let admin = read_member(entries, "admin")?;
            let admin_device = read_device(entries, "admin_device")?;
            let room_name = text_field(entries, "room_name")?.to_owned();
            // Reject control characters in the room name (sanity, mirrors v1).
            if room_name.chars().any(char::is_control) {
                return Err(Reject::InvalidContent);
            }
            Ok(GovernanceAction::InitRoom {
                admin,
                admin_device,
                room_name,
            })
        }
        "add_member" => {
            reject_unknown_action_keys(entries, &["member", "device", "role"])?;
            let member = read_member(entries, "member")?;
            let device = read_device(entries, "device")?;
            let role = Role::parse(text_field(entries, "role")?)?;
            Ok(GovernanceAction::AddMember {
                member,
                device,
                role,
            })
        }
        "remove_member" => {
            reject_unknown_action_keys(entries, &["member"])?;
            let member = read_member(entries, "member")?;
            Ok(GovernanceAction::RemoveMember { member })
        }
        "set_role" => {
            reject_unknown_action_keys(entries, &["member", "role"])?;
            let member = read_member(entries, "member")?;
            let role = Role::parse(text_field(entries, "role")?)?;
            Ok(GovernanceAction::SetRole { member, role })
        }
        "rotate_device" => {
            reject_unknown_action_keys(entries, &["member", "device"])?;
            let member = read_member(entries, "member")?;
            let device = read_device(entries, "device")?;
            Ok(GovernanceAction::RotateDevice { member, device })
        }
        "set_policy" => Ok(GovernanceAction::SetPolicy {
            policy: policy_from_cbor(entries)?,
        }),
        // Unknown discriminant → closed-registry rejection (spec D8 / §7).
        _ => Err(Reject::UnknownRecordKind),
    }
}

/// Reject a nested action/policy map carrying any key outside its known set.
/// Mirrors [`reject_unknown_keys`] but surfaces [`Reject::InvalidContent`], the
/// error class used for malformed action bodies (spec D8 / §11).
fn reject_unknown_action_keys(
    entries: &[(String, CborValue)],
    allowed: &[&str],
) -> Result<(), Reject> {
    for (k, _) in entries {
        if !allowed.contains(&k.as_str()) {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

fn read_member(entries: &[(String, CborValue)], key: &str) -> Result<MemberId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::InvalidContent)?;
    let bytes = v.as_bytes().ok_or(Reject::InvalidContent)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
    Ok(MemberId::from_bytes(arr))
}

fn read_device(entries: &[(String, CborValue)], key: &str) -> Result<DeviceId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::InvalidContent)?;
    let bytes = v.as_bytes().ok_or(Reject::InvalidContent)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
    Ok(DeviceId::from_bytes(arr))
}

fn policy_from_cbor(entries: &[(String, CborValue)]) -> Result<ApprovalPolicy, Reject> {
    let ty = text_field(entries, "type")?;
    match ty {
        "admin_alone" => {
            if entries.len() != 1 {
                return Err(Reject::InvalidContent);
            }
            Ok(ApprovalPolicy::AdminAlone)
        }
        "m_of_n" => {
            reject_unknown_action_keys(entries, &["type", "m", "approvers"])?;
            let m = uint_field(entries, "m")?;
            let approvers_val = signed::field(entries, "approvers")
                .and_then(|v| v.as_array())
                .ok_or(Reject::InvalidContent)?;
            let mut approvers: Vec<MemberId> = approvers_val
                .iter()
                .map(|v| {
                    let bytes = v.as_bytes().ok_or(Reject::InvalidContent)?;
                    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
                    Ok(MemberId::from_bytes(arr))
                })
                .collect::<Result<_, Reject>>()?;
            // Deterministic order: sort + dedup so identical policy sets hash equal.
            approvers.sort();
            approvers.dedup();
            if m == 0 || usize::try_from(m).map_or(true, |mm| mm > approvers.len()) {
                return Err(Reject::InvalidContent);
            }
            Ok(ApprovalPolicy::MOfN { m, approvers })
        }
        _ => Err(Reject::InvalidContent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RoomId;

    #[test]
    fn entry_body_round_trips() {
        let admin = MemberId::from_bytes([0xa1; LEN]);
        let dev = DeviceId::from_bytes([0xa1; LEN]);
        let body = GovernanceEntryBody {
            schema_version: SCHEMA_VERSION,
            room_id: RoomId::from_bytes([0x1f; LEN]),
            author: admin,
            seq: 1,
            parent: None,
            epoch: 1_000,
            action: GovernanceAction::InitRoom {
                admin,
                admin_device: dev,
                room_name: "room".to_owned(),
            },
        };
        let csb = signed::to_csb(&body);
        let back =
            GovernanceEntryBody::from_canonical(&crate::cbor::decode_canonical(&csb).unwrap())
                .expect("round-trip");
        assert_eq!(back, body);
    }

    #[test]
    fn unknown_action_kind_rejected() {
        let result = action_from_cbor("frobnicate", &CborValue::Map(vec![]));
        assert_eq!(result, Err(Reject::UnknownRecordKind));
    }

    #[test]
    fn role_parse_rejects_unknown() {
        assert_eq!(Role::parse("superuser"), Err(Reject::InvalidContent));
    }

    #[test]
    fn m_of_n_dedups_and_orders_approvers() {
        let a = MemberId::from_bytes([1; LEN]);
        let b = MemberId::from_bytes([2; LEN]);
        let policy = ApprovalPolicy::MOfN {
            m: 2,
            approvers: vec![b, a, a],
        };
        let cbor = policy_to_cbor(&policy);
        let back = policy_from_cbor(cbor.as_map().unwrap()).unwrap();
        match back {
            ApprovalPolicy::MOfN { m, approvers } => {
                assert_eq!(m, 2);
                assert_eq!(approvers, vec![a, b]);
            }
            ApprovalPolicy::AdminAlone => panic!("expected m_of_n"),
        }
    }
}
