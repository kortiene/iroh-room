//! The signed-event trust boundary: authoring, the wire envelope, and
//! stateless validation.
//!
//! Every Iroh Rooms event is a [`SignedEvent`] carried on the wire as a
//! [`WireEvent`] (verbatim signed-byte preservation) and admitted only through
//! [`validate_wire_bytes`] — the stateless §6 pipeline (decode, recompute
//! `event_id`, verify the [`identity::DeviceKey`](crate::identity::DeviceKey)
//! signature, strict per-type content validation, causal-structure bounds).
//! [`validate_with_membership`] completes the stateful membership/role steps
//! against a [`MembershipOracle`] (see [`crate::room`]).
//!
//! This module also carries the `message.text` and `agent.status` builders —
//! the two event types with no dedicated domain module. `room.*`,
//! `file.shared`, and `pipe.*` builders live in [`crate::room`],
//! [`crate::files`], and [`crate::pipes`] respectively, alongside their
//! validation-relevant content types.
//!
//! `event::cbor` (the raw deterministic-CBOR codec) is **not** re-exported —
//! it is an implementation detail; consumers operate on [`WireEvent`] /
//! [`validate_wire_bytes`], never raw CBOR bytes directly.
//!
//! ```
//! use iroh_rooms::events::{
//!     build_message_text, validate_wire_bytes, EventId, EventType, ValidationContext,
//! };
//! use iroh_rooms::identity::SigningKey;
//! use iroh_rooms::room::RoomId;
//!
//! let identity_secret = SigningKey::generate();
//! let device_secret = SigningKey::generate();
//! let room_id = RoomId::from_bytes([0x11; 32]);
//!
//! // A non-genesis event needs at least one causal parent; any id is
//! // structurally valid here since `validate_wire_bytes` checks shape, not
//! // whether the referenced ancestor is actually stored (that is the
//! // membership/store layers' job).
//! let parent = EventId::from_bytes([0x22; 32]);
//!
//! let wire = build_message_text(
//!     &identity_secret,
//!     &device_secret,
//!     &room_id,
//!     "hello room",
//!     Some("plain"),
//!     None,
//!     &[],
//!     &[parent],
//!     1_750_000_000_000,
//! );
//!
//! let ctx = ValidationContext::for_room(room_id);
//! let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("authored event validates");
//! assert_eq!(validated.event.event_type, EventType::MessageText);
//! ```

pub use iroh_rooms_core::event::content::{
    capability_hash, AgentStatus, Content, EventType, MessageText,
};
pub use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
pub use iroh_rooms_core::event::reject::{Flag, MembershipOracle, RejectReason};
pub use iroh_rooms_core::event::signed::SignedEvent;
pub use iroh_rooms_core::event::validate::{
    validate_wire_bytes, validate_with_membership, ValidatedEvent, ValidationContext,
};
pub use iroh_rooms_core::event::wire::WireEvent;
pub use iroh_rooms_core::event::{build_agent_status, build_message_text};
pub use iroh_rooms_core::PROTOCOL_VERSION;

/// The trust-boundary structural bounds (`MAX_*`) every content validator
/// enforces — e.g. `MAX_MESSAGE_BODY_BYTES`, `MAX_PREV_EVENTS`.
pub mod constants {
    pub use iroh_rooms_core::event::constants::{
        BIND_CONTEXT, CLOCK_SKEW_FUTURE_MS, DIGEST_LEN, ENCRYPTED_AAD_CONTEXT, ENCRYPTED_NONCE_LEN,
        ENCRYPTED_SUITE_V1, ENCRYPTED_TAG_LEN, EVENT_CONTEXT, INVITE_CONTEXT, MAX_ARTIFACT_REFS,
        MAX_ENCRYPTED_AGENT_STATUS_PLAINTEXT, MAX_ENCRYPTED_FILE_SHARED_PLAINTEXT,
        MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT, MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT,
        MAX_ENCRYPTED_PIPE_OPENED_PLAINTEXT, MAX_FILE_NAME_BYTES, MAX_FILE_PROVIDERS,
        MAX_MESSAGE_BODY_BYTES, MAX_MIME_TYPE_BYTES, MAX_PREV_EVENTS, MAX_SHARED_FILE_BYTES,
        MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, PUBLIC_KEY_LEN, ROOMID_CONTEXT,
        SCHEMA_VERSION, SHORT_ID_LEN, SIGNATURE_LEN, WIRE_VERSION, WRAPPED_KEY_CIPHERTEXT_LEN,
        WRAPPED_KEY_NONCE_LEN,
    };
}
