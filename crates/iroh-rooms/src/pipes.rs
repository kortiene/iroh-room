//! The pipe-event side of the Live Pipe Plane: authoring and validating
//! `pipe.opened` / `pipe.closed`.
//!
//! [`build_pipe_opened`] asserts a member-exposed TCP target (ALPN, label,
//! allowed members, optional expiry); [`build_pipe_closed`] asserts its
//! closure. The *forwarding* runtime — the loopback splice that actually
//! connects a peer to the exposed target — is
//! [`crate::experimental::pipe_runtime`]; this stable module covers only the
//! deterministic event side.
//!
//! ```
//! use iroh_rooms::events::{validate_wire_bytes, Content, EventId, EventType, ValidationContext};
//! use iroh_rooms::identity::DeviceKey;
//! use iroh_rooms::identity::SigningKey;
//! use iroh_rooms::pipes::build_pipe_opened;
//! use iroh_rooms::room::RoomId;
//!
//! let owner_identity = SigningKey::generate();
//! let owner_device = SigningKey::generate();
//! let connector_identity = SigningKey::generate();
//! let room_id = RoomId::from_bytes([0x11; 32]);
//! let parent = EventId::from_bytes([0x22; 32]);
//! let owner_endpoint: DeviceKey = owner_device.device_key();
//!
//! let wire = build_pipe_opened(
//!     &owner_identity,
//!     &owner_device,
//!     &room_id,
//!     [0x55; 16],
//!     &owner_endpoint,
//!     "dev-server",
//!     "127.0.0.1:8080",
//!     "iroh-rooms/pipe/1",
//!     &[connector_identity.identity_key()],
//!     None,
//!     &[parent],
//!     1_750_000_000_000,
//! );
//!
//! let ctx = ValidationContext::for_room(room_id);
//! let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("pipe.opened validates");
//! assert_eq!(validated.event.event_type, EventType::PipeOpened);
//! assert!(matches!(validated.event.content, Content::PipeOpened(_)));
//! ```

pub use iroh_rooms_core::event::content::{PipeClosed, PipeOpened};
pub use iroh_rooms_core::event::{build_pipe_closed, build_pipe_opened};
