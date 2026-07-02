//! MVP event-type registry and **strict** per-type content validation
//! (Event Protocol §7).
//!
//! Each `content` map is parsed into a typed [`Content`] variant. Validation is
//! strict in the trust-boundary sense: every key in the inbound map must be a
//! *known* key for that type, required keys must be present with the correct
//! CBOR type, optional keys are type-checked when present, enum/length bounds are
//! enforced, and **any unknown key is rejected** ([`RejectReason::InvalidContent`]).
//!
//! Per-type rules that need ancestor/membership state (invite liveness,
//! owner-is-active, admin-signer, …) are **deferred**; only the bytes-local
//! checks live here. Cross-field checks against the envelope (e.g.
//! `owner_id == sender_id`) live in [`check_field_rules`]; embedded
//! device-binding verification lives in [`verify_bindings`].

use blake3::Hasher;

use super::binding::DeviceBinding;
use super::cbor::CborValue;
use super::constants::{
    DIGEST_LEN, INVITE_CONTEXT, MAX_FILE_NAME_BYTES, MAX_FILE_PROVIDERS, MAX_MESSAGE_BODY_BYTES,
    MAX_MIME_TYPE_BYTES, MAX_SHARED_FILE_BYTES, PUBLIC_KEY_LEN, SHORT_ID_LEN,
};
use super::ids::{EventId, HashRef, RoomId};
use super::keys::{DeviceKey, IdentityKey};
use super::reject::RejectReason;

/// Roles a participant may hold (Event Protocol §3.1/§7).
const ROLES: &[&str] = &["member", "agent", "admin"];
/// Accepted `message.text` formats.
const MESSAGE_FORMATS: &[&str] = &["plain", "markdown"];
/// Accepted `file.shared` blob formats.
const BLOB_FORMATS: &[&str] = &["raw", "hash_seq"];
/// Accepted `pipe.closed` reasons.
const PIPE_CLOSE_REASONS: &[&str] = &["closed", "expired", "owner_exit", "error"];

/// The closed MVP event-type registry (Event Protocol §7). Unknown strings map
/// to [`RejectReason::UnknownEventType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    /// `room.created` — genesis; establishes room identity, admin, creator device.
    RoomCreated,
    /// `member.invited` — admin-issued capability-bound invite.
    MemberInvited,
    /// `member.joined` — proves the invite capability and binds the joiner device.
    MemberJoined,
    /// `member.left` — voluntary self-departure.
    MemberLeft,
    /// `member.removed` — admin removal / kick.
    MemberRemoved,
    /// `message.text` — a chat message.
    MessageText,
    /// `file.shared` — references a content-addressed blob.
    FileShared,
    /// `pipe.opened` — announces an authenticated TCP forward.
    PipeOpened,
    /// `pipe.closed` — closes a pipe.
    PipeClosed,
    /// `agent.status` — agent progress/status update.
    AgentStatus,
}

impl EventType {
    /// The registry string for this type.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RoomCreated => "room.created",
            Self::MemberInvited => "member.invited",
            Self::MemberJoined => "member.joined",
            Self::MemberLeft => "member.left",
            Self::MemberRemoved => "member.removed",
            Self::MessageText => "message.text",
            Self::FileShared => "file.shared",
            Self::PipeOpened => "pipe.opened",
            Self::PipeClosed => "pipe.closed",
            Self::AgentStatus => "agent.status",
        }
    }

    /// Whether an event of this type must resolve its `device_id` from
    /// membership state (Event Protocol §6 step 7 / spec scope item 11).
    ///
    /// `true` for the types that carry **no** self-contained `device_binding`
    /// (`message.text`, `file.shared`, `pipe.opened`, `pipe.closed`,
    /// `agent.status`, `member.invited`, `member.left`): their signing device
    /// must equal the device bound to `sender_id` in the membership view.
    /// `false` for the self-contained-binding types (`room.created`,
    /// `member.joined`, and `member.removed` whose binding is optional and
    /// verified statelessly when present), which the stateless layer already
    /// checks ([`verify_bindings`]).
    #[must_use]
    pub fn requires_membership_device_binding(&self) -> bool {
        matches!(
            self,
            Self::MessageText
                | Self::FileShared
                | Self::PipeOpened
                | Self::PipeClosed
                | Self::AgentStatus
                | Self::MemberInvited
                | Self::MemberLeft
        )
    }

    /// Parse a registry string, or `None` for an unknown type.
    #[must_use]
    pub fn from_registry(s: &str) -> Option<Self> {
        let ty = match s {
            "room.created" => Self::RoomCreated,
            "member.invited" => Self::MemberInvited,
            "member.joined" => Self::MemberJoined,
            "member.left" => Self::MemberLeft,
            "member.removed" => Self::MemberRemoved,
            "message.text" => Self::MessageText,
            "file.shared" => Self::FileShared,
            "pipe.opened" => Self::PipeOpened,
            "pipe.closed" => Self::PipeClosed,
            "agent.status" => Self::AgentStatus,
            _ => return None,
        };
        Some(ty)
    }
}

// ----------------------------------------------------------------------------
// Typed content
// ----------------------------------------------------------------------------

/// `room.created` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomCreated {
    /// Human room name.
    pub room_name: String,
    /// Nonce feeding `room_id` derivation (§5).
    pub room_nonce: [u8; SHORT_ID_LEN],
    /// Initial admin identities; MUST be exactly `[sender_id]` in MVP.
    pub admins: Vec<IdentityKey>,
    /// Creator's device binding.
    pub device_binding: DeviceBinding,
}

/// `member.invited` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInvited {
    /// Invite handle.
    pub invite_id: [u8; SHORT_ID_LEN],
    /// `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`.
    pub capability_hash: [u8; DIGEST_LEN],
    /// Invited role (`member` | `agent` | `admin`).
    pub role: String,
    /// The identity key this invite authorizes (key-bound).
    pub invitee_key: IdentityKey,
    /// Optional expiry (ms epoch).
    pub expires_at: Option<u64>,
    /// Optional non-authoritative human label.
    pub invitee_hint: Option<String>,
}

/// `member.joined` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberJoined {
    /// References a `member.invited.invite_id`.
    pub via_invite_id: [u8; SHORT_ID_LEN],
    /// Secret that recomputes the invite's `capability_hash`.
    pub capability_secret: [u8; SHORT_ID_LEN],
    /// Joined role (MUST equal the invite's role; checked statefully).
    pub role: String,
    /// Joiner's device binding.
    pub device_binding: DeviceBinding,
    /// Optional display name.
    pub display_name: Option<String>,
}

/// `member.left` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberLeft {
    /// Departing identity; MUST == `sender_id`.
    pub member_id: IdentityKey,
    /// Optional free-form reason.
    pub reason: Option<String>,
}

/// `member.removed` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRemoved {
    /// Removed identity; MUST != `sender_id`.
    pub member_id: IdentityKey,
    /// Admin identity; MUST == `sender_id`.
    pub removed_by: IdentityKey,
    /// Optional free-form reason.
    pub reason: Option<String>,
    /// Optional re-attestation of the admin's device. Verified when present
    /// (Event Protocol §7 member.removed schema lists no binding; §9 verifies
    /// "if present").
    pub device_binding: Option<DeviceBinding>,
}

/// `message.text` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageText {
    /// UTF-8 body, ≤ 16384 bytes.
    pub body: String,
    /// Optional format (`plain` | `markdown`).
    pub format: Option<String>,
    /// Optional reply target.
    pub in_reply_to: Option<EventId>,
    /// Optional mentioned identities.
    pub mentions: Option<Vec<IdentityKey>>,
}

/// `file.shared` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileShared {
    /// CLI handle.
    pub file_id: [u8; SHORT_ID_LEN],
    /// File name.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// BLAKE3-256 blob hash.
    pub blob_hash: HashRef,
    /// Optional blob format (`raw` | `hash_seq`).
    pub blob_format: Option<String>,
    /// Optional expected providers (`EndpointId`s).
    pub providers: Option<Vec<DeviceKey>>,
}

/// `pipe.opened` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeOpened {
    /// Pipe handle.
    pub pipe_id: [u8; SHORT_ID_LEN],
    /// Owner identity; MUST == `sender_id`.
    pub owner_id: IdentityKey,
    /// `EndpointId` to dial (== owner's `device_id`).
    pub owner_endpoint: DeviceKey,
    /// Transport kind; MUST == `tcp` in MVP.
    pub kind: String,
    /// Human label.
    pub label: String,
    /// Advisory target hint.
    pub target_hint: String,
    /// ALPN for the data stream.
    pub alpn: String,
    /// Identities authorized to connect (non-empty; no default-all).
    pub allowed_members: Vec<IdentityKey>,
    /// Optional expiry (ms epoch).
    pub expires_at: Option<u64>,
}

/// `pipe.closed` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeClosed {
    /// References an open `pipe.opened.pipe_id`.
    pub pipe_id: [u8; SHORT_ID_LEN],
    /// Optional reason (`closed` | `expired` | `owner_exit` | `error`).
    pub reason: Option<String>,
}

/// `agent.status` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    /// Free-form status label.
    pub status: String,
    /// Optional message.
    pub message: Option<String>,
    /// Optional related artifact ids (`file_id`s).
    pub related_artifact_ids: Option<Vec<[u8; SHORT_ID_LEN]>>,
    /// Optional progress percent (0..=100).
    pub progress_pct: Option<u64>,
}

/// Typed, strictly-validated event content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// See [`RoomCreated`].
    RoomCreated(RoomCreated),
    /// See [`MemberInvited`].
    MemberInvited(MemberInvited),
    /// See [`MemberJoined`].
    MemberJoined(MemberJoined),
    /// See [`MemberLeft`].
    MemberLeft(MemberLeft),
    /// See [`MemberRemoved`].
    MemberRemoved(MemberRemoved),
    /// See [`MessageText`].
    MessageText(MessageText),
    /// See [`FileShared`].
    FileShared(FileShared),
    /// See [`PipeOpened`].
    PipeOpened(PipeOpened),
    /// See [`PipeClosed`].
    PipeClosed(PipeClosed),
    /// See [`AgentStatus`].
    AgentStatus(AgentStatus),
}

impl Content {
    /// The event type of this content.
    #[must_use]
    pub fn event_type(&self) -> EventType {
        match self {
            Self::RoomCreated(_) => EventType::RoomCreated,
            Self::MemberInvited(_) => EventType::MemberInvited,
            Self::MemberJoined(_) => EventType::MemberJoined,
            Self::MemberLeft(_) => EventType::MemberLeft,
            Self::MemberRemoved(_) => EventType::MemberRemoved,
            Self::MessageText(_) => EventType::MessageText,
            Self::FileShared(_) => EventType::FileShared,
            Self::PipeOpened(_) => EventType::PipeOpened,
            Self::PipeClosed(_) => EventType::PipeClosed,
            Self::AgentStatus(_) => EventType::AgentStatus,
        }
    }

    /// Strictly parse a content map for `event_type` (structural checks only:
    /// known keys, required/optional, types, lengths, enums, binding shape).
    ///
    /// # Errors
    /// Returns [`RejectReason::InvalidContent`] on any structural violation.
    pub fn parse(event_type: EventType, value: &CborValue) -> Result<Self, RejectReason> {
        let mut f = Fields::new(value)?;
        let content = match event_type {
            EventType::RoomCreated => Self::RoomCreated(parse_room_created(&mut f)?),
            EventType::MemberInvited => Self::MemberInvited(parse_member_invited(&mut f)?),
            EventType::MemberJoined => Self::MemberJoined(parse_member_joined(&mut f)?),
            EventType::MemberLeft => Self::MemberLeft(parse_member_left(&mut f)?),
            EventType::MemberRemoved => Self::MemberRemoved(parse_member_removed(&mut f)?),
            EventType::MessageText => Self::MessageText(parse_message_text(&mut f)?),
            EventType::FileShared => Self::FileShared(parse_file_shared(&mut f)?),
            EventType::PipeOpened => Self::PipeOpened(parse_pipe_opened(&mut f)?),
            EventType::PipeClosed => Self::PipeClosed(parse_pipe_closed(&mut f)?),
            EventType::AgentStatus => Self::AgentStatus(parse_agent_status(&mut f)?),
        };
        // Reject any leftover (unknown) keys.
        f.finish()?;
        Ok(content)
    }

    /// Encode this content to its canonical CBOR map value. Optional fields are
    /// emitted only when present, so `parse` → `to_cbor` round-trips byte-exactly
    /// (the validator relies on this for the §6 step 4 re-canonicalization check).
    #[must_use]
    #[allow(clippy::too_many_lines)] // one arm per content type; splitting hurts readability
    pub fn to_cbor(&self) -> CborValue {
        match self {
            Self::RoomCreated(c) => {
                let mut m = vec![
                    text_entry("room_name", &c.room_name),
                    bytes_entry("room_nonce", &c.room_nonce),
                    ("admins".to_owned(), key_array(&c.admins)),
                    ("device_binding".to_owned(), c.device_binding.to_cbor()),
                ];
                sort_into_map(&mut m)
            }
            Self::MemberInvited(c) => {
                let mut m = vec![
                    bytes_entry("invite_id", &c.invite_id),
                    bytes_entry("capability_hash", &c.capability_hash),
                    text_entry("role", &c.role),
                    bytes_entry("invitee_key", c.invitee_key.as_bytes()),
                ];
                push_opt_uint(&mut m, "expires_at", c.expires_at);
                push_opt_text(&mut m, "invitee_hint", c.invitee_hint.as_deref());
                sort_into_map(&mut m)
            }
            Self::MemberJoined(c) => {
                let mut m = vec![
                    bytes_entry("via_invite_id", &c.via_invite_id),
                    bytes_entry("capability_secret", &c.capability_secret),
                    text_entry("role", &c.role),
                    ("device_binding".to_owned(), c.device_binding.to_cbor()),
                ];
                push_opt_text(&mut m, "display_name", c.display_name.as_deref());
                sort_into_map(&mut m)
            }
            Self::MemberLeft(c) => {
                let mut m = vec![bytes_entry("member_id", c.member_id.as_bytes())];
                push_opt_text(&mut m, "reason", c.reason.as_deref());
                sort_into_map(&mut m)
            }
            Self::MemberRemoved(c) => {
                let mut m = vec![
                    bytes_entry("member_id", c.member_id.as_bytes()),
                    bytes_entry("removed_by", c.removed_by.as_bytes()),
                ];
                push_opt_text(&mut m, "reason", c.reason.as_deref());
                if let Some(binding) = &c.device_binding {
                    m.push(("device_binding".to_owned(), binding.to_cbor()));
                }
                sort_into_map(&mut m)
            }
            Self::MessageText(c) => {
                let mut m = vec![text_entry("body", &c.body)];
                push_opt_text(&mut m, "format", c.format.as_deref());
                if let Some(id) = &c.in_reply_to {
                    m.push(bytes_entry("in_reply_to", id.as_bytes()));
                }
                if let Some(mentions) = &c.mentions {
                    m.push(("mentions".to_owned(), key_array(mentions)));
                }
                sort_into_map(&mut m)
            }
            Self::FileShared(c) => {
                let mut m = vec![
                    bytes_entry("file_id", &c.file_id),
                    text_entry("name", &c.name),
                    text_entry("mime_type", &c.mime_type),
                    ("size_bytes".to_owned(), CborValue::Uint(c.size_bytes)),
                    bytes_entry("blob_hash", c.blob_hash.as_bytes()),
                ];
                push_opt_text(&mut m, "blob_format", c.blob_format.as_deref());
                if let Some(providers) = &c.providers {
                    m.push(("providers".to_owned(), device_array(providers)));
                }
                sort_into_map(&mut m)
            }
            Self::PipeOpened(c) => {
                let mut m = vec![
                    bytes_entry("pipe_id", &c.pipe_id),
                    bytes_entry("owner_id", c.owner_id.as_bytes()),
                    bytes_entry("owner_endpoint", c.owner_endpoint.as_bytes()),
                    text_entry("kind", &c.kind),
                    text_entry("label", &c.label),
                    text_entry("target_hint", &c.target_hint),
                    text_entry("alpn", &c.alpn),
                    ("allowed_members".to_owned(), key_array(&c.allowed_members)),
                ];
                push_opt_uint(&mut m, "expires_at", c.expires_at);
                sort_into_map(&mut m)
            }
            Self::PipeClosed(c) => {
                let mut m = vec![bytes_entry("pipe_id", &c.pipe_id)];
                push_opt_text(&mut m, "reason", c.reason.as_deref());
                sort_into_map(&mut m)
            }
            Self::AgentStatus(c) => {
                let mut m = vec![text_entry("status", &c.status)];
                push_opt_text(&mut m, "message", c.message.as_deref());
                if let Some(ids) = &c.related_artifact_ids {
                    let arr = ids.iter().map(|id| CborValue::Bytes(id.to_vec())).collect();
                    m.push(("related_artifact_ids".to_owned(), CborValue::Array(arr)));
                }
                push_opt_uint(&mut m, "progress_pct", c.progress_pct);
                sort_into_map(&mut m)
            }
        }
    }
}

fn sort_into_map(entries: &mut Vec<(String, CborValue)>) -> CborValue {
    // Hold entries in canonical key order (length-first then bytewise — the same
    // order the strict decoder yields) so a built value equals a decoded one.
    // The encoder re-sorts on emit regardless, so the bytes are canonical either
    // way; this only keeps the in-memory `CborValue` equality-stable.
    entries.sort_by(|a, b| {
        a.0.len()
            .cmp(&b.0.len())
            .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
    });
    CborValue::Map(core::mem::take(entries))
}

fn text_entry(key: &str, value: &str) -> (String, CborValue) {
    (key.to_owned(), CborValue::Text(value.to_owned()))
}

fn bytes_entry(key: &str, value: &[u8]) -> (String, CborValue) {
    (key.to_owned(), CborValue::Bytes(value.to_vec()))
}

fn push_opt_text(entries: &mut Vec<(String, CborValue)>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        entries.push(text_entry(key, v));
    }
}

fn push_opt_uint(entries: &mut Vec<(String, CborValue)>, key: &str, value: Option<u64>) {
    if let Some(v) = value {
        entries.push((key.to_owned(), CborValue::Uint(v)));
    }
}

fn key_array(keys: &[IdentityKey]) -> CborValue {
    CborValue::Array(
        keys.iter()
            .map(|k| CborValue::Bytes(k.as_bytes().to_vec()))
            .collect(),
    )
}

fn device_array(keys: &[DeviceKey]) -> CborValue {
    CborValue::Array(
        keys.iter()
            .map(|k| CborValue::Bytes(k.as_bytes().to_vec()))
            .collect(),
    )
}

/// Recompute an invite capability hash (Event Protocol §7). Exposed for the
/// deferred membership layer to match a join's secret against an invite.
#[must_use]
pub fn capability_hash(
    room_id: &RoomId,
    invite_id: &[u8; SHORT_ID_LEN],
    secret: &[u8; SHORT_ID_LEN],
) -> [u8; DIGEST_LEN] {
    let mut hasher = Hasher::new();
    hasher.update(INVITE_CONTEXT);
    hasher.update(room_id.as_bytes());
    hasher.update(invite_id);
    hasher.update(secret);
    *hasher.finalize().as_bytes()
}

// ----------------------------------------------------------------------------
// Cross-field semantic checks against the envelope (bytes-local only).
// ----------------------------------------------------------------------------

/// Stateless per-type field rules that compare `content` to the envelope's
/// `sender_id` (Event Protocol §7). These are the §6 **step 5** content-validity
/// checks that need only the sender identity. Embedded device-binding signature
/// verification is a separate concern ([`verify_bindings`], §6 step 7), run
/// after room binding so it uses a validated `room_id`.
///
/// Deferred stateful rules (invite liveness, role/admin authorization) are
/// **not** here.
///
/// # Errors
/// Returns [`RejectReason::InvalidContent`] on a violation.
pub fn check_field_rules(content: &Content, sender_id: &IdentityKey) -> Result<(), RejectReason> {
    match content {
        Content::RoomCreated(c) => {
            // admins MUST be exactly [sender_id] in MVP.
            if c.admins.len() != 1 || &c.admins[0] != sender_id {
                return Err(RejectReason::InvalidContent);
            }
        }
        Content::MemberLeft(c) => {
            if &c.member_id != sender_id {
                return Err(RejectReason::InvalidContent);
            }
        }
        Content::MemberRemoved(c) => {
            if &c.removed_by != sender_id || &c.member_id == sender_id {
                return Err(RejectReason::InvalidContent);
            }
        }
        Content::PipeOpened(c) => {
            if &c.owner_id != sender_id {
                return Err(RejectReason::InvalidContent);
            }
        }
        Content::MemberInvited(_)
        | Content::MemberJoined(_)
        | Content::MessageText(_)
        | Content::FileShared(_)
        | Content::PipeClosed(_)
        | Content::AgentStatus(_) => {}
    }
    Ok(())
}

/// Verify the self-contained `device_binding` carried by `room.created`,
/// `member.joined`, and (when present) `member.removed` (Event Protocol §1, §6
/// step 7). The binding must attest exactly this event's `sender_id`/`device_id`
/// and verify under the validated `room_id`.
///
/// # Errors
/// Returns [`RejectReason::InvalidContent`] if a required binding is malformed,
/// mis-bound, or its signature does not verify.
pub fn verify_bindings(
    content: &Content,
    sender_id: &IdentityKey,
    device_id: &DeviceKey,
    room_id: &RoomId,
) -> Result<(), RejectReason> {
    match content {
        Content::RoomCreated(c) => check_binding(&c.device_binding, sender_id, device_id, room_id),
        Content::MemberJoined(c) => check_binding(&c.device_binding, sender_id, device_id, room_id),
        Content::MemberRemoved(c) => match &c.device_binding {
            Some(binding) => check_binding(binding, sender_id, device_id, room_id),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// A device binding embedded in content must attest exactly this event's
/// `sender_id`/`device_id` and verify under `room_id`.
fn check_binding(
    binding: &DeviceBinding,
    sender_id: &IdentityKey,
    device_id: &DeviceKey,
    room_id: &RoomId,
) -> Result<(), RejectReason> {
    if &binding.identity_key != sender_id || &binding.device_key != device_id {
        return Err(RejectReason::InvalidContent);
    }
    binding.verify(room_id)
}

// ----------------------------------------------------------------------------
// Per-type parsers
// ----------------------------------------------------------------------------

fn parse_room_created(f: &mut Fields<'_>) -> Result<RoomCreated, RejectReason> {
    Ok(RoomCreated {
        room_name: f.require_text("room_name")?.to_owned(),
        room_nonce: f.require_bytes::<SHORT_ID_LEN>("room_nonce")?,
        admins: f.require_key_array("admins")?,
        device_binding: DeviceBinding::from_cbor(f.require("device_binding")?)?,
    })
}

fn parse_member_invited(f: &mut Fields<'_>) -> Result<MemberInvited, RejectReason> {
    let invite_id = f.require_bytes::<SHORT_ID_LEN>("invite_id")?;
    let capability_hash = f.require_bytes::<DIGEST_LEN>("capability_hash")?;
    let role = f.require_enum("role", ROLES)?;
    let invitee_key = IdentityKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("invitee_key")?);
    let expires_at = f.opt_uint("expires_at")?;
    let invitee_hint = f.opt_text("invitee_hint")?.map(ToOwned::to_owned);
    Ok(MemberInvited {
        invite_id,
        capability_hash,
        role,
        invitee_key,
        expires_at,
        invitee_hint,
    })
}

fn parse_member_joined(f: &mut Fields<'_>) -> Result<MemberJoined, RejectReason> {
    let via_invite_id = f.require_bytes::<SHORT_ID_LEN>("via_invite_id")?;
    let capability_secret = f.require_bytes::<SHORT_ID_LEN>("capability_secret")?;
    let role = f.require_enum("role", ROLES)?;
    let device_binding = DeviceBinding::from_cbor(f.require("device_binding")?)?;
    let display_name = f.opt_text("display_name")?.map(ToOwned::to_owned);
    Ok(MemberJoined {
        via_invite_id,
        capability_secret,
        role,
        device_binding,
        display_name,
    })
}

fn parse_member_left(f: &mut Fields<'_>) -> Result<MemberLeft, RejectReason> {
    Ok(MemberLeft {
        member_id: IdentityKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("member_id")?),
        reason: f.opt_text("reason")?.map(ToOwned::to_owned),
    })
}

fn parse_member_removed(f: &mut Fields<'_>) -> Result<MemberRemoved, RejectReason> {
    let member_id = IdentityKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("member_id")?);
    let removed_by = IdentityKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("removed_by")?);
    let reason = f.opt_text("reason")?.map(ToOwned::to_owned);
    let device_binding = match f.opt("device_binding") {
        Some(v) => Some(DeviceBinding::from_cbor(v)?),
        None => None,
    };
    Ok(MemberRemoved {
        member_id,
        removed_by,
        reason,
        device_binding,
    })
}

fn parse_message_text(f: &mut Fields<'_>) -> Result<MessageText, RejectReason> {
    let body = f.require_text("body")?;
    if body.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(RejectReason::InvalidContent);
    }
    let body = body.to_owned();
    let format = f.opt_enum("format", MESSAGE_FORMATS)?;
    let in_reply_to = f
        .opt_bytes::<DIGEST_LEN>("in_reply_to")?
        .map(EventId::from_bytes);
    let mentions = f.opt_key_array("mentions")?;
    Ok(MessageText {
        body,
        format,
        in_reply_to,
        mentions,
    })
}

fn parse_file_shared(f: &mut Fields<'_>) -> Result<FileShared, RejectReason> {
    let file_id = f.require_bytes::<SHORT_ID_LEN>("file_id")?;

    let name = f.require_text("name")?;
    if name.is_empty() || name.len() > MAX_FILE_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(RejectReason::InvalidContent);
    }
    let name = name.to_owned();

    let mime_type = f.require_text("mime_type")?;
    if mime_type.is_empty()
        || mime_type.len() > MAX_MIME_TYPE_BYTES
        || !is_well_formed_mime(mime_type)
    {
        return Err(RejectReason::InvalidContent);
    }
    let mime_type = mime_type.to_owned();

    let size_bytes = f.require_uint("size_bytes")?;
    if size_bytes > MAX_SHARED_FILE_BYTES {
        return Err(RejectReason::InvalidContent);
    }

    let blob_hash = HashRef::from_bytes(f.require_bytes::<DIGEST_LEN>("blob_hash")?);
    let blob_format = f.opt_enum("blob_format", BLOB_FORMATS)?;

    let providers = f.opt_device_array("providers")?;
    if let Some(ps) = &providers {
        if ps.is_empty() || ps.len() > MAX_FILE_PROVIDERS {
            return Err(RejectReason::InvalidContent);
        }
    }

    Ok(FileShared {
        file_id,
        name,
        mime_type,
        size_bytes,
        blob_hash,
        blob_format,
        providers,
    })
}

/// Minimal MIME well-formedness: `type/subtype`, both non-empty, ASCII, no
/// whitespace or control chars, exactly one `/`. Deliberately permissive on the
/// subtype tail (parameters, `+suffix`) — strict RFC-6838 tokenization is out of
/// scope for MVP.
fn is_well_formed_mime(s: &str) -> bool {
    if !s.is_ascii() || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let mut parts = s.splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(t), Some(sub)) => !t.is_empty() && !sub.is_empty() && !sub.contains('/'),
        _ => false,
    }
}

fn parse_pipe_opened(f: &mut Fields<'_>) -> Result<PipeOpened, RejectReason> {
    let pipe_id = f.require_bytes::<SHORT_ID_LEN>("pipe_id")?;
    let owner_id = IdentityKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("owner_id")?);
    let owner_endpoint =
        DeviceKey::from_bytes(f.require_bytes::<PUBLIC_KEY_LEN>("owner_endpoint")?);
    let kind = f.require_text("kind")?.to_owned();
    if kind != "tcp" {
        return Err(RejectReason::InvalidContent);
    }
    let label = f.require_text("label")?.to_owned();
    let target_hint = f.require_text("target_hint")?.to_owned();
    let alpn = f.require_text("alpn")?.to_owned();
    let allowed_members = f.require_key_array("allowed_members")?;
    if allowed_members.is_empty() {
        return Err(RejectReason::InvalidContent);
    }
    let expires_at = f.opt_uint("expires_at")?;
    Ok(PipeOpened {
        pipe_id,
        owner_id,
        owner_endpoint,
        kind,
        label,
        target_hint,
        alpn,
        allowed_members,
        expires_at,
    })
}

fn parse_pipe_closed(f: &mut Fields<'_>) -> Result<PipeClosed, RejectReason> {
    Ok(PipeClosed {
        pipe_id: f.require_bytes::<SHORT_ID_LEN>("pipe_id")?,
        reason: f.opt_enum("reason", PIPE_CLOSE_REASONS)?,
    })
}

fn parse_agent_status(f: &mut Fields<'_>) -> Result<AgentStatus, RejectReason> {
    let status = f.require_text("status")?.to_owned();
    let message = f.opt_text("message")?.map(ToOwned::to_owned);
    let related_artifact_ids = f.opt_short_id_array("related_artifact_ids")?;
    let progress_pct = f.opt_uint("progress_pct")?;
    if let Some(pct) = progress_pct {
        if pct > 100 {
            return Err(RejectReason::InvalidContent);
        }
    }
    Ok(AgentStatus {
        status,
        message,
        related_artifact_ids,
        progress_pct,
    })
}

// ----------------------------------------------------------------------------
// Strict map-field reader: known keys consumed, unknown keys rejected.
// ----------------------------------------------------------------------------

struct Fields<'a> {
    entries: &'a [(String, CborValue)],
    consumed: Vec<bool>,
}

impl<'a> Fields<'a> {
    fn new(value: &'a CborValue) -> Result<Self, RejectReason> {
        let entries = value.as_map().ok_or(RejectReason::InvalidContent)?;
        Ok(Self {
            consumed: vec![false; entries.len()],
            entries,
        })
    }

    /// Look up an optional key, marking it consumed if present.
    fn opt(&mut self, key: &str) -> Option<&'a CborValue> {
        for (idx, (k, v)) in self.entries.iter().enumerate() {
            if k == key {
                self.consumed[idx] = true;
                return Some(v);
            }
        }
        None
    }

    /// Look up a required key.
    fn require(&mut self, key: &str) -> Result<&'a CborValue, RejectReason> {
        self.opt(key).ok_or(RejectReason::InvalidContent)
    }

    /// Error if any key was left unconsumed (i.e. an unknown content key).
    fn finish(self) -> Result<(), RejectReason> {
        if self.consumed.iter().all(|c| *c) {
            Ok(())
        } else {
            Err(RejectReason::InvalidContent)
        }
    }

    fn require_text(&mut self, key: &str) -> Result<&'a str, RejectReason> {
        self.require(key)?
            .as_text()
            .ok_or(RejectReason::InvalidContent)
    }

    fn opt_text(&mut self, key: &str) -> Result<Option<&'a str>, RejectReason> {
        match self.opt(key) {
            Some(v) => Ok(Some(v.as_text().ok_or(RejectReason::InvalidContent)?)),
            None => Ok(None),
        }
    }

    fn require_enum(&mut self, key: &str, allowed: &[&str]) -> Result<String, RejectReason> {
        let text = self.require_text(key)?;
        if allowed.contains(&text) {
            Ok(text.to_owned())
        } else {
            Err(RejectReason::InvalidContent)
        }
    }

    fn opt_enum(&mut self, key: &str, allowed: &[&str]) -> Result<Option<String>, RejectReason> {
        match self.opt_text(key)? {
            Some(text) if allowed.contains(&text) => Ok(Some(text.to_owned())),
            Some(_) => Err(RejectReason::InvalidContent),
            None => Ok(None),
        }
    }

    fn require_uint(&mut self, key: &str) -> Result<u64, RejectReason> {
        self.require(key)?
            .as_uint()
            .ok_or(RejectReason::InvalidContent)
    }

    fn opt_uint(&mut self, key: &str) -> Result<Option<u64>, RejectReason> {
        match self.opt(key) {
            Some(v) => Ok(Some(v.as_uint().ok_or(RejectReason::InvalidContent)?)),
            None => Ok(None),
        }
    }

    fn require_bytes<const N: usize>(&mut self, key: &str) -> Result<[u8; N], RejectReason> {
        fixed_bytes::<N>(self.require(key)?)
    }

    fn opt_bytes<const N: usize>(&mut self, key: &str) -> Result<Option<[u8; N]>, RejectReason> {
        match self.opt(key) {
            Some(v) => Ok(Some(fixed_bytes::<N>(v)?)),
            None => Ok(None),
        }
    }

    /// Require an array of `bstr[32]` parsed as identity keys.
    fn require_key_array(&mut self, key: &str) -> Result<Vec<IdentityKey>, RejectReason> {
        let items = self
            .require(key)?
            .as_array()
            .ok_or(RejectReason::InvalidContent)?;
        items
            .iter()
            .map(|v| Ok(IdentityKey::from_bytes(fixed_bytes::<PUBLIC_KEY_LEN>(v)?)))
            .collect()
    }

    /// Optional array of `bstr[32]` parsed as identity keys.
    fn opt_key_array(&mut self, key: &str) -> Result<Option<Vec<IdentityKey>>, RejectReason> {
        match self.opt(key) {
            Some(v) => {
                let items = v.as_array().ok_or(RejectReason::InvalidContent)?;
                let keys = items
                    .iter()
                    .map(|item| {
                        Ok(IdentityKey::from_bytes(fixed_bytes::<PUBLIC_KEY_LEN>(
                            item,
                        )?))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(keys))
            }
            None => Ok(None),
        }
    }

    /// Optional array of `bstr[32]` parsed as device keys (`EndpointId`s).
    fn opt_device_array(&mut self, key: &str) -> Result<Option<Vec<DeviceKey>>, RejectReason> {
        match self.opt(key) {
            Some(v) => {
                let items = v.as_array().ok_or(RejectReason::InvalidContent)?;
                let keys = items
                    .iter()
                    .map(|item| Ok(DeviceKey::from_bytes(fixed_bytes::<PUBLIC_KEY_LEN>(item)?)))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(keys))
            }
            None => Ok(None),
        }
    }

    /// Optional array of `bstr[16]` short ids.
    fn opt_short_id_array(
        &mut self,
        key: &str,
    ) -> Result<Option<Vec<[u8; SHORT_ID_LEN]>>, RejectReason> {
        match self.opt(key) {
            Some(v) => {
                let items = v.as_array().ok_or(RejectReason::InvalidContent)?;
                let ids = items
                    .iter()
                    .map(fixed_bytes::<SHORT_ID_LEN>)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(ids))
            }
            None => Ok(None),
        }
    }
}

/// Extract a fixed-length byte array from a CBOR byte string, or fail closed.
fn fixed_bytes<const N: usize>(value: &CborValue) -> Result<[u8; N], RejectReason> {
    let bytes = value.as_bytes().ok_or(RejectReason::InvalidContent)?;
    <[u8; N]>::try_from(bytes).map_err(|_| RejectReason::InvalidContent)
}

#[cfg(test)]
mod tests {
    use super::is_well_formed_mime;

    #[test]
    fn is_well_formed_mime_accepts_real_types() {
        for ok in [
            "text/plain",
            "application/pdf",
            "image/svg+xml",
            "application/vnd.api+json",
        ] {
            assert!(is_well_formed_mime(ok), "expected {ok:?} to be well-formed");
        }
    }

    #[test]
    fn is_well_formed_mime_rejects_malformed_strings() {
        for bad in [
            "",
            "plain",
            "/plain",
            "text/",
            "text//plain",
            "text/ plain",
            "tex t/plain",
            "text/plaín",
        ] {
            assert!(!is_well_formed_mime(bad), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn is_well_formed_mime_rejects_control_chars() {
        // A control char in a mime type is caught *only* by `is_well_formed_mime`
        // (unlike `name`, which has its own explicit `char::is_control` guard in
        // `parse_file_shared`). Cover both a whitespace control (`\t`, `\n`) and
        // DEL (`\u{7f}`), which is a control char that is NOT whitespace — so it
        // exercises the `is_control()` branch specifically, not just whitespace.
        for bad in [
            "text/pl\tain",
            "text/plain\n",
            "text/pl\u{7f}ain",
            "\u{7f}/plain",
        ] {
            assert!(
                !is_well_formed_mime(bad),
                "expected control-char mime {bad:?} to be rejected"
            );
        }
    }
}
