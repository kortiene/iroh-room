//! The canonical signed event model — the single-event trust boundary of the
//! Room Event Plane (`PHASE-0-SPIKE.md` Event Protocol §1–§8).
//!
//! This module is the byte-for-byte-correct core every other plane rides on. It
//! implements the **stateless** verification surface — every check that depends
//! only on an event's own bytes:
//!
//! * [`keys`] — Ed25519 identity (`sender_id`) and device (`device_id`) keys,
//!   signatures, and a secret signing-key wrapper (§1).
//! * [`ids`] — `EventId` / `RoomId` / `HashRef` named BLAKE3 hashes (§4/§5).
//! * [`constants`] — domain-separation contexts and structural limits.
//! * [`cbor`] — a purpose-built deterministic-CBOR encoder + strict canonical
//!   reader for the canonical signed bytes (CSB) (§3, spec D1).
//! * [`signed`] — the eight-field [`signed::SignedEvent`], CSB, BLAKE3 event-id
//!   and room-id derivation, and Ed25519 signing (§2/§3/§4/§5/§6).
//! * [`wire`] — the [`wire::WireEvent`] envelope with verbatim signed-byte
//!   preservation (§3).
//! * [`content`] — the §7 event-type registry and strict per-type content
//!   validation (unknown-key rejection, length/enum bounds).
//! * [`binding`] — self-contained `device_binding` certificate verification (§1).
//! * [`genesis`] — pure assembly of a signed genesis [`content::RoomCreated`]
//!   event ([`genesis::build_room_created`], §5/§6/§7).
//! * [`invite`] — pure assembly of a signed admin [`content::MemberInvited`]
//!   event ([`invite::build_member_invited`], §7).
//! * [`join`] — pure assembly of a signed joiner [`content::MemberJoined`]
//!   event ([`join::build_member_joined`], §7).
//! * [`left`] — pure assembly of a signed [`content::MemberLeft`] self-departure
//!   event ([`left::build_member_left`], §7).
//! * [`removed`] — pure assembly of a signed admin [`content::MemberRemoved`]
//!   removal event ([`removed::build_member_removed`], §7).
//! * [`file`] — pure assembly of a signed member [`content::FileShared`] blob
//!   reference event ([`file::build_file_shared`], §7).
//! * [`validate`] — the stateless [`validate::validate_wire_bytes`] pipeline
//!   (§6) returning a [`validate::ValidatedEvent`] or a typed [`RejectReason`].
//! * [`reject`] — the [`RejectReason`] / [`Flag`] taxonomy (§8) and the deferred
//!   [`reject::MembershipOracle`] boundary (§6 steps 7–8).
//!
//! Out of scope here (sibling issues under epic #1): causal ordering, transitive
//! genesis-descent, sync/transport, and the `SQLite` store. The membership fold
//! and authorization layer lives in [`crate::membership`]. This layer defines the
//! bytes those layers consume.

pub mod binding;
pub mod cbor;
pub mod constants;
pub mod content;
pub mod file;
pub mod genesis;
pub mod ids;
pub mod invite;
pub mod join;
pub mod keys;
pub mod left;
pub mod message;
pub mod pipe;
pub mod reject;
pub mod removed;
pub mod signed;
pub mod validate;
pub mod wire;

// Convenience re-exports of the most-used types.
pub use binding::DeviceBinding;
pub use content::{capability_hash, Content, EventType};
pub use file::build_file_shared;
pub use genesis::build_room_created;
pub use ids::{EventId, HashRef, RoomId};
pub use invite::build_member_invited;
pub use join::build_member_joined;
pub use keys::{DeviceKey, IdentityKey, Signature, SigningKey};
pub use left::build_member_left;
pub use message::build_message_text;
pub use pipe::{build_pipe_closed, build_pipe_opened};
pub use reject::{Flag, MembershipOracle, RejectReason};
pub use removed::build_member_removed;
pub use signed::SignedEvent;
pub use validate::{
    validate_wire_bytes, validate_with_membership, ValidatedEvent, ValidationContext,
};
pub use wire::WireEvent;
