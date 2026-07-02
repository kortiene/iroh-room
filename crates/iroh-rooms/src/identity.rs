//! Participant and device keys, and the device-binding certificate that ties a
//! device to an identity within a room.
//!
//! Iroh Rooms distinguishes two key types per Event Protocol §1: an
//! [`IdentityKey`] (the stable principal, `sender_id`) and a [`DeviceKey`]
//! (the per-device signing key, `device_id` — byte-for-byte the iroh
//! `EndpointId`). Event signatures verify under the device key; membership
//! and authorization track the identity key.
//!
//! **MVP is one device per identity.** This surface intentionally exposes no
//! device-*set* or device-management API — multi-device is a post-MVP
//! capability (PRD §13.4/§13.5) and stays out of both the stable and
//! [`experimental`](crate::experimental) tiers (spec AC4).
//!
//! ```
//! use iroh_rooms::identity::SigningKey;
//!
//! // Generate a fresh secret key, then derive its two public faces.
//! let identity_secret = SigningKey::generate();
//! let device_secret = SigningKey::generate();
//!
//! let identity_key = identity_secret.identity_key();
//! let device_key = device_secret.device_key();
//! assert_ne!(identity_key.to_string(), device_key.to_string());
//! ```

pub use iroh_rooms_core::event::binding::DeviceBinding;
pub use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, Signature, SigningKey};
