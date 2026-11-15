//! Room lifecycle authoring, the membership fold, and the invite ticket.
//!
//! A room's identity ([`RoomId`]) is deterministically derived from its
//! genesis signer, a nonce, and a timestamp ([`derive_room_id`]); the genesis
//! [`build_room_created`] event asserts it. [`RoomMembership`] folds a stream
//! of [`crate::events::ValidatedEvent`]s into a convergent
//! [`MembershipSnapshot`] — the **current**-state authorization boundary the
//! access predicates ([`blob_serve_allowed`], [`pipe_connect_allowed`])
//! consult, kept rigorously separate from the ancestor-scoped log-validity
//! verdict ([`Ingest`]) that admits an event onto the fold in the first
//! place. [`RoomInviteTicket`] is the out-of-band secret carrier that travels
//! alongside an on-log `member.invited` event.
//!
//! ```
//! use iroh_rooms::events::{validate_wire_bytes, ValidationContext};
//! use iroh_rooms::identity::SigningKey;
//! use iroh_rooms::room::{build_room_created, derive_room_id, RoomMembership};
//!
//! let admin_identity = SigningKey::generate();
//! let admin_device = SigningKey::generate();
//! let nonce = [0x42; 16];
//! let created_at = 1_750_000_000_000;
//!
//! // `derive_room_id` mirrors exactly what `build_room_created` computes
//! // internally, so callers can know a room's id before/while authoring its
//! // genesis event (spec D-room-id).
//! let room_id = derive_room_id(&admin_identity.identity_key(), &nonce, created_at);
//! let wire = build_room_created(&admin_identity, &admin_device, "demo room", &nonce, created_at);
//!
//! let ctx = ValidationContext::for_room(room_id);
//! let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("genesis validates");
//!
//! let mut fold = RoomMembership::new(room_id);
//! fold.ingest(validated);
//!
//! let snapshot = fold.snapshot();
//! assert!(snapshot.is_active(&admin_identity.identity_key()));
//! ```

pub use iroh_rooms_core::event::ids::RoomId; // also surfaced here for discoverability
pub use iroh_rooms_core::event::signed::derive_room_id;
pub use iroh_rooms_core::event::{
    build_member_invited, build_member_joined, build_member_left, build_member_removed,
    build_room_created,
};
pub use iroh_rooms_core::membership::{
    blob_serve_allowed, pipe_connect_allowed, AncestorView, BlobDecision, DenyReason, Ingest,
    Member, MembershipSnapshot, PipeDecision, Role, RoomMembership, Status,
};
pub use iroh_rooms_core::ticket::{RoomInviteTicket, TicketError};
