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
use crate::event::constants::{DIGEST_LEN, SHORT_ID_LEN};
use crate::event::content::{
    member_key_distribution_to_cbor, parse_member_key_distribution_value, MemberKeyDistribution,
    WrappedKeyEntry,
};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::DeviceKey;

/// Maximum encoded size of one sync-message body (1 MiB) — the **shared wire
/// contract** with the transport's per-frame cap
/// (`iroh-rooms-net::frame::MAX_FRAME_BYTES`, spec D4): a conformant peer
/// closes the stream on any frame declared larger, and the net writer drops an
/// oversized locally-queued body rather than emitting it. The engine therefore
/// keeps every message it produces within this bound (bounded `have` claims,
/// byte-budgeted `Events` batches); the net crate pins equality in a test so
/// the two constants cannot drift apart silently.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Conservative fixed allowance for the CBOR envelope of one
/// [`Events`](SyncMessage::Events) message — map head + `"type"`/`"events"` +
/// `"room"` (32-byte id) + `"frames"` array head. Measured 60–62 bytes against
/// [`SyncMessage::encode`] for any batch up to 65 535 frames; 128 leaves margin.
/// Pinned by `events_overhead_fits_the_declared_allowances` so encoder drift
/// cannot silently break the engine's byte budgeting (issue #113).
pub(crate) const EVENTS_ENVELOPE_ALLOWANCE: usize = 128;

/// Conservative per-frame overhead inside an `Events` batch: the frame's CBOR
/// byte-string head (at most 5 bytes for any frame under 4 GiB).
pub(crate) const EVENTS_PER_FRAME_OVERHEAD: usize = 5;

/// Conservative fixed allowance for the CBOR envelope of one
/// [`KeyHistory`](SyncMessage::KeyHistory) message — map head + `"type"` /
/// `"room"` / `"chunks"` array head. Measured under 40 bytes; 128 leaves margin.
pub(crate) const KEY_HISTORY_ENVELOPE_ALLOWANCE: usize = 128;

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

/// One chunk of a chunked key-history transfer (spec D6). Mirrors the
/// `member.key_distribution` payload shape so a single chunk can be
/// authenticated and adopted by the same key-adoption path as a DAG event.
///
/// The chunk carries the original distribution event's `event_id` so the same
/// deterministic conflict-resolution rule (smallest event id wins) applies
/// whether the payload arrives over the DAG or over the key-history channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHistoryChunk {
    /// The original `member.key_distribution` event id that produced this chunk.
    pub distribution_event_id: EventId,
    /// The epoch this chunk distributes.
    pub epoch: u64,
    /// `BLAKE3(epoch_be8 || room_id || room_key)` — verified at adoption.
    pub key_commitment: [u8; DIGEST_LEN],
    /// Per-recipient wrapped keys, sorted by device id in canonical CBOR.
    pub wrapped_keys: Vec<(DeviceKey, WrappedKeyEntry)>,
}

impl KeyHistoryChunk {
    /// Convert to the equivalent `MemberKeyDistribution` content payload.
    #[must_use]
    fn to_distribution(&self) -> MemberKeyDistribution {
        MemberKeyDistribution {
            new_epoch: self.epoch,
            key_commitment: self.key_commitment,
            wrapped_keys: self.wrapped_keys.clone(),
        }
    }

    /// Build a chunk from a `MemberKeyDistribution` payload.
    #[must_use]
    fn from_distribution(event_id: EventId, d: MemberKeyDistribution) -> Self {
        Self {
            distribution_event_id: event_id,
            epoch: d.new_epoch,
            key_commitment: d.key_commitment,
            wrapped_keys: d.wrapped_keys,
        }
    }

    pub(crate) fn to_cbor(&self) -> CborValue {
        // Encode as a two-key map: the distribution payload plus the event id
        // that authenticates/conflict-resolves it. The encoder sorts canonically.
        let mut entries = vec![
            (
                "distribution_event_id".to_owned(),
                CborValue::Bytes(self.distribution_event_id.as_bytes().to_vec()),
            ),
            (
                "distribution".to_owned(),
                member_key_distribution_to_cbor(&self.to_distribution()),
            ),
        ];
        entries.sort_by(|a, b| {
            a.0.len()
                .cmp(&b.0.len())
                .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
        });
        CborValue::Map(entries)
    }

    fn from_cbor(value: &CborValue) -> Result<Self, MessageError> {
        let entries = value.as_map().ok_or(MessageError::BadShape)?;
        let event_id = field(entries, "distribution_event_id")
            .and_then(read_digest)
            .map(EventId::from_bytes)
            .ok_or(MessageError::BadShape)?;
        let distribution = match field(entries, "distribution") {
            Some(v) => {
                parse_member_key_distribution_value(v).map_err(|_| MessageError::BadShape)?
            }
            None => return Err(MessageError::BadShape),
        };
        Ok(Self::from_distribution(event_id, distribution))
    }
}

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
    ///
    /// Each `have` entry is an **ancestry claim** (issue #113): it asserts the
    /// requester holds that event *and its entire stored ancestry*, so the
    /// responder subtracts the claimed id plus every stored ancestor of it. A
    /// bounded claim (placed DAG heads + a recent-lamport slab + a rotating
    /// window, see [`SyncConfig::membership_have_max_ids`](super::SyncConfig))
    /// therefore covers an arbitrarily large held set in O(cap) ids — the frame
    /// no longer grows with room history. An old-style exhaustive claim over an
    /// intact store (every held id, itself causally closed) expands to exactly
    /// itself, so the semantics generalize the pre-#113 exact-set subtraction;
    /// the exception is an old requester with a store *hole*, whose claimed
    /// unplaced descendants now cover the missing ancestor (see CHANGELOG
    /// upgrade note).
    WantMembership {
        /// Room scope.
        room_id: RoomId,
        /// Ancestry claims: ids the requester holds **with complete ancestry**.
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
    /// A terminal event batch whose receipt must be confirmed before the sender
    /// physically closes a removed peer's link. On the wire this keeps the
    /// `events` tag and adds `ids`/`nonce`: older peers therefore still deliver
    /// the events (and merely omit the receipt), while newer transports can
    /// recognize and reserve bounded lifecycle capacity for the envelope.
    TerminalEvents {
        /// Room scope.
        room_id: RoomId,
        /// Verbatim event frames, ending with the target's removal event.
        frames: Vec<WireBytes>,
        /// Event ids whose durable presence must be confirmed.
        ids: Vec<EventId>,
        /// Request correlation nonce.
        nonce: [u8; SHORT_ID_LEN],
    },
    /// The subset of a [`TerminalEvents`](Self::TerminalEvents) request that is
    /// actually present in the responder's room-scoped durable store.
    EventsConfirmed {
        /// Room scope.
        room_id: RoomId,
        /// Requested event ids that are durably stored in this room.
        ids: Vec<EventId>,
        /// Correlation nonce copied verbatim from the request.
        nonce: [u8; SHORT_ID_LEN],
    },
    /// The responder does not hold these requested ids.
    NotFound {
        /// Room scope.
        room_id: RoomId,
        /// Ids the responder lacks.
        ids: Vec<EventId>,
    },
    /// A join-bootstrap **capability proof** (issue #112): a provisionally-admitted
    /// dialer proves it holds an invite by presenting the invite's `invite_id` and
    /// its `capability_secret`. The responder recomputes the invite
    /// `capability_hash` and matches it against an on-log `member.invited` before it
    /// will serve the never-windowed membership **closure** — which, since #111, can
    /// carry the chat that entered the membership ancestry. An uninvited dialer
    /// cannot produce a matching secret, so it never earns the closure.
    ///
    /// This is a bootstrap **privacy** gate only; the convergent `gate_join`
    /// authorization authority is unchanged and still runs on the actual join. The
    /// secret carried here is the same one the join later places on the log, and it
    /// travels only over the authenticated transport link to the admin who minted
    /// it — so it reveals nothing the responder does not already hold.
    ProveCapability {
        /// Room scope.
        room_id: RoomId,
        /// The invite the dialer claims (`member.invited.invite_id`).
        invite_id: [u8; SHORT_ID_LEN],
        /// The capability secret proving possession of that invite.
        capability_secret: [u8; SHORT_ID_LEN],
    },
    /// Pull the room's epoch key history that the requester lacks (spec D6).
    ///
    /// `have_epochs` is a trust input only in the sense that a requester cannot
    /// forge a key it does not hold; the responder still sends only signed
    /// `member.key_distribution` payload shapes, and the requester verifies the
    /// D5 commitment + unwraps with its own device secret before adoption.
    WantKeyHistory {
        /// Room scope.
        room_id: RoomId,
        /// Epochs the requester already holds keys for.
        have_epochs: BTreeSet<u64>,
    },
    /// A response carrying one or more bounded `KeyHistoryChunk`s (spec D6).
    ///
    /// Chunks are encoded with the same canonical shape as a
    /// `member.key_distribution` payload so the receiver can feed each chunk
    /// through the ordinary key-adoption path.
    KeyHistory {
        /// Room scope.
        room_id: RoomId,
        /// Bounded chunks. The encoder keeps the total message under
        /// [`MAX_FRAME_BYTES`].
        chunks: Vec<KeyHistoryChunk>,
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
    /// `true` only for the accept-path fan-out of a newly accepted event to
    /// every connected peer — the one emission that is a *broadcast* by
    /// nature. Targeted emissions (pull responses, backfill serves, bootstrap
    /// closures) stay `false`, so a transport with a broadcast plane (the
    /// gossip overlay) can ride it for fan-out without turning targeted
    /// serves into mesh-wide duplicate spam. Transports without a broadcast
    /// plane ignore the flag (every Outgoing rides the per-peer queue).
    pub fanout: bool,
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
            | Self::TerminalEvents { room_id, .. }
            | Self::EventsConfirmed { room_id, .. }
            | Self::NotFound { room_id, .. }
            | Self::ProveCapability { room_id, .. }
            | Self::WantKeyHistory { room_id, .. }
            | Self::KeyHistory { room_id, .. } => room_id,
        }
    }

    /// Encode to canonical deterministic CBOR (the on-wire body, before any
    /// length prefix). Deterministic: the same message always yields the same
    /// bytes (determinism guard, spec §8.4).
    #[allow(clippy::too_many_lines)] // one arm per sync message variant; encoding is flat
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
            Self::TerminalEvents {
                room_id,
                frames,
                ids,
                nonce,
            } => vec![
                // Keep the established tag for rolling-upgrade delivery. The
                // legacy decoder ignores unknown map keys and treats this as a
                // normal Events message; the current decoder recognizes the
                // paired receipt fields below.
                tag("events"),
                room_field(room_id),
                (
                    "frames".to_owned(),
                    CborValue::Array(frames.iter().map(|f| CborValue::Bytes(f.clone())).collect()),
                ),
                ("ids".to_owned(), id_array(ids)),
                ("nonce".to_owned(), CborValue::Bytes(nonce.to_vec())),
            ],
            Self::EventsConfirmed {
                room_id,
                ids,
                nonce,
            } => vec![
                tag("events_confirmed"),
                room_field(room_id),
                ("ids".to_owned(), id_array(ids)),
                ("nonce".to_owned(), CborValue::Bytes(nonce.to_vec())),
            ],
            Self::NotFound { room_id, ids } => vec![
                tag("not_found"),
                room_field(room_id),
                ("ids".to_owned(), id_array(ids)),
            ],
            Self::ProveCapability {
                room_id,
                invite_id,
                capability_secret,
            } => vec![
                tag("prove_capability"),
                room_field(room_id),
                ("invite_id".to_owned(), CborValue::Bytes(invite_id.to_vec())),
                (
                    "secret".to_owned(),
                    CborValue::Bytes(capability_secret.to_vec()),
                ),
            ],
            Self::WantKeyHistory {
                room_id,
                have_epochs,
            } => vec![
                tag("want_key_history"),
                room_field(room_id),
                ("have_epochs".to_owned(), uint_array(have_epochs)),
            ],
            Self::KeyHistory { room_id, chunks } => vec![
                tag("key_history"),
                room_field(room_id),
                (
                    "chunks".to_owned(),
                    CborValue::Array(chunks.iter().map(KeyHistoryChunk::to_cbor).collect()),
                ),
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
            "events" => {
                let frames = read_bytes_array(field(entries, "frames"))?;
                match (field(entries, "ids"), field(entries, "nonce")) {
                    (None, None) => Self::Events { room_id, frames },
                    (Some(ids), Some(nonce)) => Self::TerminalEvents {
                        room_id,
                        frames,
                        ids: read_id_array(Some(ids))?,
                        nonce: read_short_id(Some(nonce))?,
                    },
                    // A partially-specified receipt request is malformed, not
                    // an ordinary Events frame with ignorable metadata.
                    _ => return Err(MessageError::BadShape),
                }
            }
            "events_confirmed" => Self::EventsConfirmed {
                room_id,
                ids: read_id_array(field(entries, "ids"))?,
                nonce: read_short_id(field(entries, "nonce"))?,
            },
            "not_found" => Self::NotFound {
                room_id,
                ids: read_id_array(field(entries, "ids"))?,
            },
            "prove_capability" => Self::ProveCapability {
                room_id,
                invite_id: read_short_id(field(entries, "invite_id"))?,
                capability_secret: read_short_id(field(entries, "secret"))?,
            },
            "want_key_history" => Self::WantKeyHistory {
                room_id,
                have_epochs: read_uint_array(field(entries, "have_epochs"))?,
            },
            "key_history" => Self::KeyHistory {
                room_id,
                chunks: read_chunk_array(field(entries, "chunks"))?,
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

fn uint_array(values: &BTreeSet<u64>) -> CborValue {
    CborValue::Array(values.iter().copied().map(CborValue::Uint).collect())
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

fn read_short_id(value: Option<&CborValue>) -> Result<[u8; SHORT_ID_LEN], MessageError> {
    value
        .and_then(CborValue::as_bytes)
        .and_then(|b| <[u8; SHORT_ID_LEN]>::try_from(b).ok())
        .ok_or(MessageError::BadShape)
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

fn read_uint_array(value: Option<&CborValue>) -> Result<BTreeSet<u64>, MessageError> {
    let items = value
        .and_then(CborValue::as_array)
        .ok_or(MessageError::BadShape)?;
    items
        .iter()
        .map(|item| item.as_uint().ok_or(MessageError::BadShape))
        .collect()
}

fn read_chunk_array(value: Option<&CborValue>) -> Result<Vec<KeyHistoryChunk>, MessageError> {
    let items = value
        .and_then(CborValue::as_array)
        .ok_or(MessageError::BadShape)?;
    items.iter().map(KeyHistoryChunk::from_cbor).collect()
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

    fn device(b: u8) -> DeviceKey {
        DeviceKey::from_bytes([b; DIGEST_LEN])
    }

    fn chunk(event_byte: u8, epoch: u64) -> KeyHistoryChunk {
        KeyHistoryChunk {
            distribution_event_id: id(event_byte),
            epoch,
            key_commitment: [0xcc; DIGEST_LEN],
            wrapped_keys: vec![(
                device(0x22),
                WrappedKeyEntry {
                    ephemeral_public: [0xee; DIGEST_LEN],
                    nonce: [0x55; 12],
                    ciphertext: [0x66; 48],
                },
            )],
        }
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
        round_trip(&SyncMessage::TerminalEvents {
            room_id: room(),
            frames: vec![vec![0xca, 0xfe]],
            ids: vec![id(7), id(8)],
            nonce: [0x6d; SHORT_ID_LEN],
        });
        round_trip(&SyncMessage::EventsConfirmed {
            room_id: room(),
            ids: vec![id(7)],
            nonce: [0x6d; SHORT_ID_LEN],
        });
        round_trip(&SyncMessage::NotFound {
            room_id: room(),
            ids: vec![id(9)],
        });
        round_trip(&SyncMessage::ProveCapability {
            room_id: room(),
            invite_id: [0x3c; SHORT_ID_LEN],
            capability_secret: [0x5e; SHORT_ID_LEN],
        });
        round_trip(&SyncMessage::WantKeyHistory {
            room_id: room(),
            have_epochs: BTreeSet::from([1, 3, 5]),
        });
        round_trip(&SyncMessage::KeyHistory {
            room_id: room(),
            chunks: vec![chunk(0xaa, 2), chunk(0xbb, 4)],
        });
    }

    #[test]
    fn key_history_chunk_round_trips_distribution_shape() {
        let original = chunk(0x12, 7);
        let encoded = original.to_cbor();
        let decoded = KeyHistoryChunk::from_cbor(&encoded).expect("decode");
        assert_eq!(
            decoded.distribution_event_id,
            original.distribution_event_id
        );
        assert_eq!(decoded.epoch, original.epoch);
        assert_eq!(decoded.key_commitment, original.key_commitment);
        assert_eq!(decoded.wrapped_keys, original.wrapped_keys);
    }

    #[test]
    fn max_frame_bytes_is_one_mib() {
        // Pin the shared wire contract (issue #113): the net transport closes a
        // stream on any frame above this, so every engine-emitted body must fit.
        // The net crate pins equality against its framing constant.
        assert_eq!(MAX_FRAME_BYTES, 1_048_576);
    }

    #[test]
    fn events_overhead_fits_the_declared_allowances() {
        // The engine's publish guard and Events byte budgeting assume the
        // encoded envelope costs at most EVENTS_ENVELOPE_ALLOWANCE plus
        // EVENTS_PER_FRAME_OVERHEAD per frame (issue #113). Pin that against the
        // real encoder at several batch shapes so a future field added to the
        // Events encoding cannot silently under-budget and produce frames the
        // net writer drops.
        for (count, frame_len) in [(1usize, 0usize), (1, 1_048_000), (23, 100), (512, 2040)] {
            let frames = vec![vec![0xEE; frame_len]; count];
            let msg = SyncMessage::Events {
                room_id: room(),
                frames: frames.clone(),
            };
            let payload: usize = count * frame_len;
            let budgeted = payload + count * EVENTS_PER_FRAME_OVERHEAD + EVENTS_ENVELOPE_ALLOWANCE;
            let encoded = msg.encode().len();
            assert!(
                encoded <= budgeted,
                "{count} frames of {frame_len} B encode to {encoded} > budget {budgeted}"
            );
            let terminal_encoded = SyncMessage::TerminalEvents {
                room_id: room(),
                frames,
                ids: vec![id(1)],
                nonce: [0x6d; SHORT_ID_LEN],
            }
            .encode()
            .len();
            assert!(
                terminal_encoded <= budgeted,
                "terminal batch of {count} frames / {frame_len} B encodes to \
                 {terminal_encoded} > budget {budgeted}"
            );
        }
    }

    #[test]
    fn terminal_events_keep_the_legacy_events_projection() {
        let expected_frames = vec![vec![0xca, 0xfe]];
        let bytes = SyncMessage::TerminalEvents {
            room_id: room(),
            frames: expected_frames.clone(),
            ids: vec![id(7)],
            nonce: [0x6d; SHORT_ID_LEN],
        }
        .encode();
        let value = cbor::decode_canonical(&bytes).expect("canonical terminal envelope");
        let entries = value.as_map().expect("terminal envelope map");

        // This is the projection performed by the rc.4 decoder: it recognizes
        // the established tag and ignores additive map keys. Pinning it here
        // prevents a future wire-tag edit from silently breaking rolling
        // removal delivery.
        assert_eq!(
            field(entries, "type").and_then(CborValue::as_text),
            Some("events")
        );
        let legacy_projection = SyncMessage::Events {
            room_id: field(entries, "room")
                .and_then(read_room)
                .expect("legacy room"),
            frames: read_bytes_array(field(entries, "frames")).expect("legacy frames"),
        };
        assert_eq!(
            legacy_projection,
            SyncMessage::Events {
                room_id: room(),
                frames: expected_frames,
            }
        );
    }

    #[test]
    fn terminal_events_reject_partial_receipt_metadata() {
        let base = vec![
            tag("events"),
            room_field(&room()),
            (
                "frames".to_owned(),
                CborValue::Array(vec![CborValue::Bytes(vec![0xca, 0xfe])]),
            ),
        ];
        let mut ids_only = base.clone();
        ids_only.push(("ids".to_owned(), id_array(&[id(7)])));
        assert_eq!(
            SyncMessage::decode(&cbor::encode(&CborValue::Map(ids_only))),
            Err(MessageError::BadShape)
        );
        let mut nonce_only = base;
        nonce_only.push((
            "nonce".to_owned(),
            CborValue::Bytes(vec![0x6d; SHORT_ID_LEN]),
        ));
        assert_eq!(
            SyncMessage::decode(&cbor::encode(&CborValue::Map(nonce_only))),
            Err(MessageError::BadShape)
        );
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
