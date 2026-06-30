//! The logical signed event (the eight §2 fields), canonical signed bytes
//! (CSB), and the BLAKE3 / Ed25519 derivations built on them.
//!
//! See `PHASE-0-SPIKE.md` Event Protocol §2 (fields), §3 (CSB), §4 (event id),
//! §5 (room id), §6 (signature). `event_id` and `signature` are deliberately
//! **not** part of [`SignedEvent`] — they are not signed-over. There is no
//! `lamport` field; the struct serializes to exactly `map(8)`.

use blake3::Hasher;

use super::cbor::{self, CborValue};
use super::constants::{
    DIGEST_LEN, EVENT_CONTEXT, PUBLIC_KEY_LEN, ROOMID_CONTEXT, SCHEMA_VERSION, SHORT_ID_LEN,
};
use super::content::{Content, EventType};
use super::ids::{EventId, RoomId};
use super::keys::{DeviceKey, IdentityKey, Signature, SigningKey};
use super::reject::RejectReason;

/// The eight signed logical fields of an event (Event Protocol §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedEvent {
    /// MUST be `1` (Event Protocol §2). Pins hash to BLAKE3-256, sig to Ed25519.
    pub schema_version: u64,
    /// The room this event belongs to.
    pub room_id: RoomId,
    /// Participant identity (`sender_id`).
    pub sender_id: IdentityKey,
    /// Signing device (`device_id`); the signature MUST verify under this key.
    pub device_id: DeviceKey,
    /// Registered event type (§7).
    pub event_type: EventType,
    /// Milliseconds since Unix epoch; advisory/display only.
    pub created_at: u64,
    /// Causal parents; `[]` only for `room.created`.
    pub prev_events: Vec<EventId>,
    /// Strictly-validated per-type content.
    pub content: Content,
}

impl SignedEvent {
    /// Produce the canonical signed bytes (CSB) for this event (Event Protocol
    /// §3): the deterministic-CBOR encoding of the eight fields.
    #[must_use]
    pub fn to_csb(&self) -> Vec<u8> {
        let entries = vec![
            (
                "schema_version".to_owned(),
                CborValue::Uint(self.schema_version),
            ),
            (
                "room_id".to_owned(),
                CborValue::Bytes(self.room_id.as_bytes().to_vec()),
            ),
            (
                "sender_id".to_owned(),
                CborValue::Bytes(self.sender_id.as_bytes().to_vec()),
            ),
            (
                "device_id".to_owned(),
                CborValue::Bytes(self.device_id.as_bytes().to_vec()),
            ),
            (
                "event_type".to_owned(),
                CborValue::Text(self.event_type.as_str().to_owned()),
            ),
            ("created_at".to_owned(), CborValue::Uint(self.created_at)),
            (
                "prev_events".to_owned(),
                CborValue::Array(
                    self.prev_events
                        .iter()
                        .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            ("content".to_owned(), self.content.to_cbor()),
        ];
        cbor::encode(&CborValue::Map(entries))
    }

    /// The event id: `BLAKE3-256(CSB)` (Event Protocol §4).
    #[must_use]
    pub fn event_id(&self) -> EventId {
        event_id_from_bytes(&self.to_csb())
    }

    /// Decode and structurally validate CSB into a [`SignedEvent`].
    ///
    /// Performs Event Protocol §6 step 4 (canonical decode; exactly the eight
    /// keys with correct CBOR types) and step 5 (`schema_version == 1`;
    /// registered `event_type`; strict content validation). It does **not**
    /// perform signature, room-binding, device-binding, or causal-structure
    /// checks — those are the validator's job ([`super::validate`]).
    ///
    /// # Errors
    /// * [`RejectReason::NonCanonicalEncoding`] — not canonical CBOR, or not the
    ///   exact eight typed keys.
    /// * [`RejectReason::UnknownSchemaVersion`] — `schema_version != 1`.
    /// * [`RejectReason::UnknownEventType`] — type not in the §7 registry.
    /// * [`RejectReason::InvalidContent`] — strict content validation failed.
    pub fn decode(signed: &[u8]) -> Result<Self, RejectReason> {
        let value =
            cbor::decode_canonical(signed).map_err(|_| RejectReason::NonCanonicalEncoding)?;
        Self::from_canonical_value(&value)
    }

    /// Steps 4–5 on an already canonically-decoded value. Lets the validator
    /// verify the signature (step 3) between the canonical decode and content
    /// validation while reusing a single parse.
    ///
    /// # Errors
    /// As [`SignedEvent::decode`].
    pub fn from_canonical_value(value: &CborValue) -> Result<Self, RejectReason> {
        let entries = value.as_map().ok_or(RejectReason::NonCanonicalEncoding)?;
        if entries.len() != 8 {
            return Err(RejectReason::NonCanonicalEncoding);
        }

        // ---- Step 4: exact-eight keys with correct CBOR types. ----
        let mut schema_version: Option<u64> = None;
        let mut room_id: Option<RoomId> = None;
        let mut sender_id: Option<IdentityKey> = None;
        let mut device_id: Option<DeviceKey> = None;
        let mut event_type_str: Option<&str> = None;
        let mut created_at: Option<u64> = None;
        let mut prev_events: Option<Vec<EventId>> = None;
        let mut content_val: Option<&CborValue> = None;

        for (key, val) in entries {
            match key.as_str() {
                "schema_version" => {
                    schema_version = Some(val.as_uint().ok_or(RejectReason::NonCanonicalEncoding)?);
                }
                "room_id" => room_id = Some(RoomId::from_bytes(top_digest(val)?)),
                "sender_id" => sender_id = Some(IdentityKey::from_bytes(top_key(val)?)),
                "device_id" => device_id = Some(DeviceKey::from_bytes(top_key(val)?)),
                "event_type" => {
                    event_type_str = Some(val.as_text().ok_or(RejectReason::NonCanonicalEncoding)?);
                }
                "created_at" => {
                    created_at = Some(val.as_uint().ok_or(RejectReason::NonCanonicalEncoding)?);
                }
                "prev_events" => prev_events = Some(decode_prev_events(val)?),
                "content" => {
                    if val.as_map().is_none() {
                        return Err(RejectReason::NonCanonicalEncoding);
                    }
                    content_val = Some(val);
                }
                _ => return Err(RejectReason::NonCanonicalEncoding),
            }
        }

        let schema_version = schema_version.ok_or(RejectReason::NonCanonicalEncoding)?;
        let room_id = room_id.ok_or(RejectReason::NonCanonicalEncoding)?;
        let sender_id = sender_id.ok_or(RejectReason::NonCanonicalEncoding)?;
        let device_id = device_id.ok_or(RejectReason::NonCanonicalEncoding)?;
        let event_type_str = event_type_str.ok_or(RejectReason::NonCanonicalEncoding)?;
        let created_at = created_at.ok_or(RejectReason::NonCanonicalEncoding)?;
        let prev_events = prev_events.ok_or(RejectReason::NonCanonicalEncoding)?;
        let content_val = content_val.ok_or(RejectReason::NonCanonicalEncoding)?;

        // ---- Step 5: version / type / strict content. ----
        if schema_version != SCHEMA_VERSION {
            return Err(RejectReason::UnknownSchemaVersion);
        }
        let event_type =
            EventType::from_registry(event_type_str).ok_or(RejectReason::UnknownEventType)?;
        let content = Content::parse(event_type, content_val)?;

        Ok(Self {
            schema_version,
            room_id,
            sender_id,
            device_id,
            event_type,
            created_at,
            prev_events,
            content,
        })
    }
}

/// A top-level 32-byte digest field (`room_id`).
fn top_digest(val: &CborValue) -> Result<[u8; DIGEST_LEN], RejectReason> {
    let bytes = val.as_bytes().ok_or(RejectReason::NonCanonicalEncoding)?;
    <[u8; DIGEST_LEN]>::try_from(bytes).map_err(|_| RejectReason::NonCanonicalEncoding)
}

/// A top-level 32-byte public-key field (`sender_id` / `device_id`).
fn top_key(val: &CborValue) -> Result<[u8; PUBLIC_KEY_LEN], RejectReason> {
    let bytes = val.as_bytes().ok_or(RejectReason::NonCanonicalEncoding)?;
    <[u8; PUBLIC_KEY_LEN]>::try_from(bytes).map_err(|_| RejectReason::NonCanonicalEncoding)
}

/// Decode `prev_events` as an array of 32-byte digests. A wrong-typed entry is a
/// shape violation (`non_canonical_encoding`); the >20 count check is the
/// validator's `too_many_parents` (kept out of decoding so the reason is precise).
fn decode_prev_events(val: &CborValue) -> Result<Vec<EventId>, RejectReason> {
    let items = val.as_array().ok_or(RejectReason::NonCanonicalEncoding)?;
    items
        .iter()
        .map(|item| Ok(EventId::from_bytes(top_digest(item)?)))
        .collect()
}

/// Read just the `device_id` from a canonical signed value, for the early
/// signature check (Event Protocol §6 step 3) before full content validation. A
/// missing or mis-typed field is a shape violation.
///
/// # Errors
/// Returns [`RejectReason::NonCanonicalEncoding`] if the value is not a map or
/// has no well-formed `device_id`.
pub fn read_device_id(value: &CborValue) -> Result<DeviceKey, RejectReason> {
    let entries = value.as_map().ok_or(RejectReason::NonCanonicalEncoding)?;
    for (key, val) in entries {
        if key == "device_id" {
            return Ok(DeviceKey::from_bytes(top_key(val)?));
        }
    }
    Err(RejectReason::NonCanonicalEncoding)
}

/// Compute an event id directly from raw signed bytes: `BLAKE3-256(signed)`
/// (Event Protocol §4). The validator hashes the **exact** wire bytes, never a
/// re-encoding.
#[must_use]
pub fn event_id_from_bytes(signed: &[u8]) -> EventId {
    EventId::from_bytes(*blake3::hash(signed).as_bytes())
}

/// Derive a room id (Event Protocol §5):
/// `BLAKE3-256(ROOMID_CONTEXT ‖ sender_id ‖ room_nonce ‖ created_at_be)`.
#[must_use]
pub fn derive_room_id(
    creator_sender_id: &IdentityKey,
    room_nonce: &[u8; SHORT_ID_LEN],
    created_at: u64,
) -> RoomId {
    let mut hasher = Hasher::new();
    hasher.update(ROOMID_CONTEXT);
    hasher.update(creator_sender_id.as_bytes());
    hasher.update(room_nonce);
    hasher.update(&created_at.to_be_bytes());
    RoomId::from_bytes(*hasher.finalize().as_bytes())
}

/// Build the Ed25519 signing message: `EVENT_CONTEXT ‖ signed` (Event Protocol §6).
#[must_use]
pub fn event_signing_message(signed: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(EVENT_CONTEXT.len() + signed.len());
    msg.extend_from_slice(EVENT_CONTEXT);
    msg.extend_from_slice(signed);
    msg
}

/// Sign canonical signed bytes with a device secret key (Event Protocol §6):
/// `Ed25519_sign(device_secret, EVENT_CONTEXT ‖ CSB)`.
#[must_use]
pub fn sign_csb(csb: &[u8], device_secret: &SigningKey) -> Signature {
    device_secret.sign(&event_signing_message(csb))
}
