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
//! * [`validate`] — the stateless [`validate::validate_wire_bytes`] pipeline
//!   (§6) returning a [`validate::ValidatedEvent`] or a typed [`RejectReason`].
//! * [`reject`] — the [`RejectReason`] / [`Flag`] taxonomy (§8) and the deferred
//!   [`reject::MembershipOracle`] boundary (§6 steps 7–8).
//!
//! Out of scope here (sibling issues under epic #1): membership fold and
//! authorization, causal ordering, transitive genesis-descent, sync/transport,
//! and the `SQLite` store. This layer defines the bytes those layers consume.

pub mod binding;
pub mod cbor;
pub mod constants;
pub mod content;
pub mod ids;
pub mod keys;
pub mod reject;
pub mod signed;
pub mod validate;
pub mod wire;

// Convenience re-exports of the most-used types.
pub use binding::DeviceBinding;
pub use content::{Content, EventType};
pub use ids::{EventId, HashRef, RoomId};
pub use keys::{DeviceKey, IdentityKey, Signature, SigningKey};
pub use reject::{Flag, MembershipOracle, RejectReason};
pub use signed::SignedEvent;
pub use validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
pub use wire::WireEvent;
