//! The blob-reference event side of the Blob Plane: authoring and validating
//! a `file.shared` reference.
//!
//! [`build_file_shared`] asserts a BLAKE3-256 [`HashRef`] pointing at a blob a
//! member holds, plus its metadata (name, MIME type, size, optional
//! providers). The *runtime* half — importing a file into the local blob
//! store, serving it over the ACL-gated blob ALPN, and verified fetch — is
//! [`crate::experimental::blob`]; this stable module covers only the
//! deterministic event side: authoring a reference and validating one
//! received.
//!
//! ```
//! use iroh_rooms::events::{validate_wire_bytes, Content, EventId, EventType, ValidationContext};
//! use iroh_rooms::files::{build_file_shared, HashRef};
//! use iroh_rooms::identity::SigningKey;
//! use iroh_rooms::room::RoomId;
//!
//! let sender_identity = SigningKey::generate();
//! let sender_device = SigningKey::generate();
//! let room_id = RoomId::from_bytes([0x11; 32]);
//! let parent = EventId::from_bytes([0x22; 32]);
//! let blob_hash = HashRef::from_bytes([0x33; 32]);
//!
//! let wire = build_file_shared(
//!     &sender_identity,
//!     &sender_device,
//!     &room_id,
//!     [0x44; 16],
//!     "notes.txt",
//!     "text/plain",
//!     42,
//!     blob_hash,
//!     None,
//!     &[],
//!     &[parent],
//!     1_750_000_000_000,
//! );
//!
//! let ctx = ValidationContext::for_room(room_id);
//! let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("file.shared validates");
//! assert_eq!(validated.event.event_type, EventType::FileShared);
//! assert!(matches!(validated.event.content, Content::FileShared(_)));
//! ```

pub use iroh_rooms_core::event::build_file_shared;
pub use iroh_rooms_core::event::content::FileShared;
pub use iroh_rooms_core::event::ids::HashRef;
