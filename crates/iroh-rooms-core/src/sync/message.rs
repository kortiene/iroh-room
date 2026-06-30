//! The sync wire protocol: [`SyncMessage`] frames, the [`PeerId`] transport
//! address, the engine's [`Outgoing`] output, and the bounded chat [`Window`]
//! (spec `bounded-recent-sync-prototype.md` §4.2/§4.3).
//!
//! Every message is a length-prefixable, deterministic-CBOR frame scoped to a
//! [`RoomId`]. All ids are the raw 32-byte values on the wire (hex presentation
//! lives at the CLI boundary). The codec reuses the event core's strict canonical
//! CBOR ([`crate::event::cbor`]) so encode/decode are byte-deterministic and a
//! peer cannot smuggle non-canonical framing past the validator boundary.

use std::collections::BTreeSet;

use crate::event::cbor::{self, CborValue};
use crate::event::constants::DIGEST_LEN;
use crate::event::ids::{EventId, RoomId};

/// A transport peer address: the remote device id (`device_id` == iroh
/// `EndpointId`). The engine fans out and directs pulls by this opaque id; it is
/// independent of the membership identity the device is bound to.
///
/// `Ord` is the bytewise order of the raw 32 bytes, giving the engine a stable
/// fan-out order (determinism guard, spec R4).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId([u8; DIGEST_LEN]);

impl PeerId {
    /// Wrap raw device-id bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 32 device-id bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }
}

impl core::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PeerId({})", hex::encode(self.0))
    }
}

impl core::fmt::Display for PeerId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

/// The bounded chat-history request window (spec §4.2 / PRD §10.7).
///
/// `max_count` is the **trustworthy** bound — it selects the last N events in the
/// canonical `(lamport, event_id)` order, which no peer can forge. `since_ms`
/// filters on the **advisory** `created_at` and MUST NOT gate completeness or
/// access (spec §2.3 / R8); a malicious peer can set any `created_at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Maximum number of chat events to return (trustworthy, canonical order).
    pub max_count: u32,
    /// Optional advisory lower bound on `created_at` (ms epoch). Advisory only.
    pub since_ms: Option<u64>,
}

/// Verbatim `WireEvent` bytes (`== WireEvent::to_bytes()`) carried in an
/// [`SyncMessage::Events`] response. Re-validated by the requester on receipt.
pub type WireBytes = Vec<u8>;

/// One frame of the bounded recent-sync protocol (spec §4.2).
///
/// `room_id` scopes every variant; the engine drops any frame whose `room_id`
/// does not match its own room. `have` lists are a server-side set-difference
/// **optimization** and never a trust input — the requester re-validates every
/// returned frame regardless.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncMessage {
    /// Admin-chain-tip advertisement: the highest admin-authored event the sender
    /// holds, as `(event_id, admin_seq)`, or `None` if it has no admin chain yet
    /// (spec §0 incompleteness detection).
    AdminTip {
        /// Room scope.
        room_id: RoomId,
        /// Advertised admin tip, or `None`.
        tip: Option<(EventId, u64)>,
    },
    /// The sender's DAG heads — a cheap set-difference hint (spec OQ-2).
    Heads {
        /// Room scope.
        room_id: RoomId,
        /// The sender's causal heads.
        heads: Vec<EventId>,
    },
    /// Pull specific events by id (the §4 backfill loop, driven by
    /// `Ingest::Buffered.missing`).
    WantEvents {
        /// Room scope.
        room_id: RoomId,
        /// Requested event ids.
        ids: Vec<EventId>,
    },
    /// Pull the **never-windowed** membership sub-DAG + full admin chain; `have`
    /// lets the responder send only the delta (spec §0 hard invariant).
    WantMembership {
        /// Room scope.
        room_id: RoomId,
        /// Authorization-class ids the requester already holds.
        have: Vec<EventId>,
    },
    /// Pull bounded recent chat history (spec §10.7).
    WantRecentChat {
        /// Room scope.
        room_id: RoomId,
        /// The bounded window (count trustworthy; time advisory).
        window: Window,
        /// Chat-class ids the requester already holds.
        have: Vec<EventId>,
    },
    /// A response carrying verbatim `WireEvent` frames (spec §6.4).
    Events {
        /// Room scope.
        room_id: RoomId,
        /// Verbatim `WireEvent` byte frames.
        frames: Vec<WireBytes>,
    },
    /// The responder does not hold these requested ids.
    NotFound {
        /// Room scope.
        room_id: RoomId,
        /// Ids the responder lacks.
        ids: Vec<EventId>,
    },
}

/// A frame the engine wants sent to a peer. The engine performs no I/O; it
/// **returns** these and the harness/adapter routes them (spec §4.3 / D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    /// The destination peer (remote `device_id`).
    pub peer: PeerId,
    /// The message to deliver.
    pub msg: SyncMessage,
}

/// A `SyncMessage` failed to decode from peer-supplied bytes.
///
/// Per-frame decode failures are **logged drops at the engine boundary**, never a
/// reason to crash on peer bytes (spec §9 / typed-error discipline).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MessageError {
    /// The bytes were not canonical deterministic CBOR.
    NonCanonical,
    /// The CBOR was canonical but did not match a known message shape.
    BadShape,
}

impl core::fmt::Display for MessageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NonCanonical => "non_canonical_sync_frame",
            Self::BadShape => "bad_sync_frame_shape",
        })
    }
}

impl std::error::Error for MessageError {}

impl SyncMessage {
    /// The room this frame is scoped to.
    #[must_use]
    pub fn room_id(&self) -> &RoomId {
        match self {
            Self::AdminTip { room_id, .. }
            | Self::Heads { room_id, .. }
            | Self::WantEvents { room_id, .. }
            | Self::WantMembership { room_id, .. }
            | Self::WantRecentChat { room_id, .. }
            | Self::Events { room_id, .. }
            | Self::NotFound { room_id, .. } => room_id,
        }
    }

    /// Encode to canonical deterministic CBOR (the on-wire body, before any
    /// length prefix). Deterministic: the same message always yields the same
    /// bytes (determinism guard, spec §8.4).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let entries = match self {
            Self::AdminTip { room_id, tip } => vec![
                tag("admin_tip"),
                room_field(room_id),
                ("tip".to_owned(), opt_tip(tip.as_ref())),
            ],
            Self::Heads { room_id, heads } => vec![
                tag("heads"),
                room_field(room_id),
                ("heads".to_owned(), id_array(heads)),
            ],
            Self::WantEvents { room_id, ids } => vec![
                tag("want_events"),
                room_field(room_id),
                ("ids".to_owned(), id_array(ids)),
            ],
            Self::WantMembership { room_id, have } => vec![
                tag("want_membership"),
                room_field(room_id),
                ("have".to_owned(), id_array(have)),
            ],
            Self::WantRecentChat {
                room_id,
                window,
                have,
            } => vec![
                tag("want_recent_chat"),
                room_field(room_id),
                (
                    "max_count".to_owned(),
                    CborValue::Uint(u64::from(window.max_count)),
                ),
                ("since".to_owned(), opt_u64(window.since_ms)),
                ("have".to_owned(), id_array(have)),
            ],
            Self::Events { room_id, frames } => vec![
                tag("events"),
                room_field(room_id),
                (
                    "frames".to_owned(),
                    CborValue::Array(frames.iter().map(|f| CborValue::Bytes(f.clone())).collect()),
                ),
            ],
            Self::NotFound { room_id, ids } => vec![
                tag("not_found"),
                room_field(room_id),
                ("ids".to_owned(), id_array(ids)),
            ],
        };
        cbor::encode(&CborValue::Map(entries))
    }

    /// Decode a canonical CBOR message body.
    ///
    /// # Errors
    /// [`MessageError::NonCanonical`] if the bytes are not canonical CBOR, or
    /// [`MessageError::BadShape`] if they do not match a known message shape.
    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let value = cbor::decode_canonical(bytes).map_err(|_| MessageError::NonCanonical)?;
        let entries = value.as_map().ok_or(MessageError::BadShape)?;
        let ty = field(entries, "type")
            .and_then(CborValue::as_text)
            .ok_or(MessageError::BadShape)?;
        let room_id = field(entries, "room")
            .and_then(read_room)
            .ok_or(MessageError::BadShape)?;
        let msg = match ty {
            "admin_tip" => Self::AdminTip {
                room_id,
                tip: read_opt_tip(field(entries, "tip").ok_or(MessageError::BadShape)?)?,
            },
            "heads" => Self::Heads {
                room_id,
                heads: read_id_array(field(entries, "heads"))?,
            },
            "want_events" => Self::WantEvents {
                room_id,
                ids: read_id_array(field(entries, "ids"))?,
            },
            "want_membership" => Self::WantMembership {
                room_id,
                have: read_id_array(field(entries, "have"))?,
            },
            "want_recent_chat" => Self::WantRecentChat {
                room_id,
                window: Window {
                    max_count: field(entries, "max_count")
                        .and_then(CborValue::as_uint)
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or(MessageError::BadShape)?,
                    since_ms: read_opt_u64(field(entries, "since").ok_or(MessageError::BadShape)?)?,
                },
                have: read_id_array(field(entries, "have"))?,
            },
            "events" => Self::Events {
                room_id,
                frames: read_bytes_array(field(entries, "frames"))?,
            },
            "not_found" => Self::NotFound {
                room_id,
                ids: read_id_array(field(entries, "ids"))?,
            },
            _ => return Err(MessageError::BadShape),
        };
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// Encode helpers
// ---------------------------------------------------------------------------

fn tag(ty: &str) -> (String, CborValue) {
    ("type".to_owned(), CborValue::Text(ty.to_owned()))
}

fn room_field(room: &RoomId) -> (String, CborValue) {
    (
        "room".to_owned(),
        CborValue::Bytes(room.as_bytes().to_vec()),
    )
}

fn id_array(ids: &[EventId]) -> CborValue {
    CborValue::Array(
        ids.iter()
            .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
            .collect(),
    )
}

fn opt_tip(tip: Option<&(EventId, u64)>) -> CborValue {
    match tip {
        None => CborValue::Array(Vec::new()),
        Some((id, seq)) => CborValue::Array(vec![
            CborValue::Bytes(id.as_bytes().to_vec()),
            CborValue::Uint(*seq),
        ]),
    }
}

fn opt_u64(v: Option<u64>) -> CborValue {
    match v {
        None => CborValue::Array(Vec::new()),
        Some(n) => CborValue::Array(vec![CborValue::Uint(n)]),
    }
}

// ---------------------------------------------------------------------------
// Decode helpers
// ---------------------------------------------------------------------------

fn field<'a>(entries: &'a [(String, CborValue)], key: &str) -> Option<&'a CborValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn read_digest(value: &CborValue) -> Option<[u8; DIGEST_LEN]> {
    value
        .as_bytes()
        .and_then(|b| <[u8; DIGEST_LEN]>::try_from(b).ok())
}

fn read_room(value: &CborValue) -> Option<RoomId> {
    read_digest(value).map(RoomId::from_bytes)
}

fn read_id_array(value: Option<&CborValue>) -> Result<Vec<EventId>, MessageError> {
    let items = value
        .and_then(CborValue::as_array)
        .ok_or(MessageError::BadShape)?;
    items
        .iter()
        .map(|item| {
            read_digest(item)
                .map(EventId::from_bytes)
                .ok_or(MessageError::BadShape)
        })
        .collect()
}

fn read_bytes_array(value: Option<&CborValue>) -> Result<Vec<WireBytes>, MessageError> {
    let items = value
        .and_then(CborValue::as_array)
        .ok_or(MessageError::BadShape)?;
    items
        .iter()
        .map(|item| {
            item.as_bytes()
                .map(<[u8]>::to_vec)
                .ok_or(MessageError::BadShape)
        })
        .collect()
}

fn read_opt_tip(value: &CborValue) -> Result<Option<(EventId, u64)>, MessageError> {
    let items = value.as_array().ok_or(MessageError::BadShape)?;
    match items {
        [] => Ok(None),
        [id, seq] => {
            let id = read_digest(id)
                .map(EventId::from_bytes)
                .ok_or(MessageError::BadShape)?;
            let seq = seq.as_uint().ok_or(MessageError::BadShape)?;
            Ok(Some((id, seq)))
        }
        _ => Err(MessageError::BadShape),
    }
}

fn read_opt_u64(value: &CborValue) -> Result<Option<u64>, MessageError> {
    let items = value.as_array().ok_or(MessageError::BadShape)?;
    match items {
        [] => Ok(None),
        [n] => Ok(Some(n.as_uint().ok_or(MessageError::BadShape)?)),
        _ => Err(MessageError::BadShape),
    }
}

/// Collect an iterator of ids into a deterministic [`BTreeSet`] (helper for the
/// engine's `have`/delta computations).
#[must_use]
pub(crate) fn id_set(ids: impl IntoIterator<Item = EventId>) -> BTreeSet<EventId> {
    ids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> EventId {
        EventId::from_bytes([b; DIGEST_LEN])
    }

    fn room() -> RoomId {
        RoomId::from_bytes([0x11; DIGEST_LEN])
    }

    fn round_trip(msg: &SyncMessage) {
        let bytes = msg.encode();
        let back = SyncMessage::decode(&bytes).expect("decode");
        assert_eq!(*msg, back, "round-trip must be identity");
        // Determinism: re-encoding the decoded value yields identical bytes.
        assert_eq!(bytes, back.encode(), "encode must be deterministic");
    }

    #[test]
    fn round_trips_every_variant() {
        round_trip(&SyncMessage::AdminTip {
            room_id: room(),
            tip: None,
        });
        round_trip(&SyncMessage::AdminTip {
            room_id: room(),
            tip: Some((id(0xaa), 7)),
        });
        round_trip(&SyncMessage::Heads {
            room_id: room(),
            heads: vec![id(1), id(2)],
        });
        round_trip(&SyncMessage::WantEvents {
            room_id: room(),
            ids: vec![id(3)],
        });
        round_trip(&SyncMessage::WantMembership {
            room_id: room(),
            have: vec![id(4), id(5)],
        });
        round_trip(&SyncMessage::WantRecentChat {
            room_id: room(),
            window: Window {
                max_count: 200,
                since_ms: Some(1_700_000_000_000),
            },
            have: vec![],
        });
        round_trip(&SyncMessage::WantRecentChat {
            room_id: room(),
            window: Window {
                max_count: 10,
                since_ms: None,
            },
            have: vec![id(6)],
        });
        round_trip(&SyncMessage::Events {
            room_id: room(),
            frames: vec![vec![0xde, 0xad], vec![0xbe, 0xef]],
        });
        round_trip(&SyncMessage::NotFound {
            room_id: room(),
            ids: vec![id(9)],
        });
    }

    #[test]
    fn rejects_unknown_type() {
        let bytes = cbor::encode(&CborValue::Map(vec![tag("nope"), room_field(&room())]));
        assert_eq!(SyncMessage::decode(&bytes), Err(MessageError::BadShape));
    }

    #[test]
    fn rejects_non_canonical() {
        assert_eq!(
            SyncMessage::decode(&[0xff, 0x00]),
            Err(MessageError::NonCanonical)
        );
    }
}
