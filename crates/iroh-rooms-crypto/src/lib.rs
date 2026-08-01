//! Pure `SUITE_V1` content-key primitives for content-key rotation (issue
//! #191, spec `specs/content-key-rotation.md` D3/D3a, §7 step 2).
//!
//! # Crate invariants
//!
//! This crate is a **pure, deterministic, sans-IO** sibling of
//! `iroh-rooms-v2-core` (OQ-1 resolved: a new sibling crate, so v2-core's
//! "no payload encryption" purity tripwire stays intact). It owns only the
//! `SUITE_V1` cryptographic primitives and MUST NOT contain, transitively
//! depend on, or import:
//!
//! - network transports, ALPN constants, router accept loops (`iroh`,
//!   `iroh-blobs`, `iroh-gossip`);
//! - async runtimes (`tokio`/`tokio::spawn`);
//! - storage (`rusqlite`, `SQLite`, replicas, receipts);
//! - event/wire types from `iroh-rooms-core` — callers build AAD buffers;
//!   this crate operates on opaque bytes only.
//!
//! The `banned_dependencies` test machine-checks the absence of the banned
//! crates in the dependency tree, mirroring `iroh-rooms-v2-core`.
//!
//! # `SUITE_V1` (spec D3 — pinned, fail-closed)
//!
//! `suite = 0x01` means exactly, with no implementer choice:
//!
//! | Primitive | Pinned choice |
//! |---|---|
//! | Key agreement (wrap) | X25519 (RFC 7748); device Ed25519 keys converted to Montgomery; fresh ephemeral sender key per wrap (D3a) |
//! | Wrap KDF | HKDF-SHA-256 (RFC 5869), `salt = room_id`, `info = "iroh-rooms/content-key-wrap/v1" ‖ key_epoch ‖ recipient_device` |
//! | Wrap AEAD | AES-256-GCM (key 32 B, nonce 12 B, tag 16 B), `aad = room_id ‖ key_epoch ‖ recipient_device` |
//! | Content AEAD | AES-256-GCM, `key = room_key[epoch]`, nonce 12 B unique per `(key_epoch, event)`, tag 16 B, caller-built AAD (D2) |
//! | Content key | `room_key[epoch]` = 32 bytes from `getrandom` (CSPRNG) |
//! | Suite id | `0x01`; any other value is rejected fail-closed |
//!
//! **Byte-layout pins** (what the golden vectors in `tests/suite_v1.rs`
//! freeze, so two implementations agree byte-for-byte):
//!
//! - `key_epoch` is encoded as **8 bytes big-endian** (`u64::to_be_bytes`)
//!   everywhere it enters HKDF `info` or AEAD `aad`.
//! - `recipient_device` is the recipient's **32-byte Ed25519 public key**
//!   (the repo's `device_id`), not its Montgomery form.
//! - `room_id` is the repo's 32-byte room digest.
//! - The Ed25519→X25519 secret conversion is the RFC 8032 expansion: the
//!   lower 32 bytes of `SHA-512(seed)` (clamped inside X25519 itself); the
//!   public conversion is the Edwards→Montgomery birational map
//!   `u = (1 + y)/(1 − y)`.
//!
//! # What this crate does *not* do
//!
//! No event content is encrypted anywhere in the workspace yet: the
//! `Content::Encrypted` envelope, the rotation lifecycle, key distribution,
//! and decrypt-on-read are §7 steps 3–6 and live elsewhere. This crate also
//! does not choose nonces for content events — uniqueness per
//! `(key_epoch, event)` is the caller's contract (see [`seal_content`]).
//!
//! # Example
//!
//! ```
//! use iroh_rooms_crypto::{
//!     open_content, seal_content, unwrap_room_key, wrap_room_key, RoomKey, SUITE_V1,
//! };
//!
//! // Recipient device identity (Ed25519), as in `iroh-rooms-core`.
//! let seed = [0x2a; 32];
//! let device_public = ed25519_dalek::SigningKey::from_bytes(&seed)
//!     .verifying_key()
//!     .to_bytes();
//!
//! let room_id = [0x60; 32];
//! let key_epoch = 1;
//!
//! // Admin side: generate the epoch key and wrap it for the recipient.
//! let room_key = RoomKey::generate();
//! let wrapped = wrap_room_key(&room_key, &device_public, &room_id, key_epoch)?;
//!
//! // Recipient side: unwrap with the device seed.
//! let unwrapped = unwrap_room_key(SUITE_V1, &wrapped, &seed, &room_id, key_epoch)?;
//! assert_eq!(unwrapped.as_bytes(), room_key.as_bytes());
//!
//! // Content AEAD under the epoch key (AAD is built by the caller, D2).
//! let nonce = iroh_rooms_crypto::generate_nonce();
//! let aad = b"caller-built cleartext signed prefix";
//! let ciphertext = seal_content(&room_key, &nonce, b"hello", aad)?;
//! let plaintext = open_content(SUITE_V1, &unwrapped, &nonce, &ciphertext, aad)?;
//! assert_eq!(plaintext, b"hello");
//! # Ok::<(), iroh_rooms_crypto::CryptoError>(())
//! ```

#![forbid(unsafe_code)]

pub mod content;
pub mod convert;
pub mod error;
pub mod keys;
pub mod wrap;

pub use content::{open_content, seal_content};
pub use convert::{ed25519_public_to_x25519, ed25519_seed_to_x25519};
pub use error::CryptoError;
pub use keys::{generate_nonce, EphemeralSecret, RoomKey};
pub use wrap::{unwrap_room_key, wrap_room_key, wrap_room_key_with, WrappedRoomKey};

/// The single pinned cryptographic suite (spec D3). Any other suite id is
/// rejected fail-closed by [`ensure_suite`].
pub const SUITE_V1: u8 = 0x01;

/// Room content keys are 32 bytes (AES-256-GCM key size).
pub const ROOM_KEY_LEN: usize = 32;

/// AEAD nonces are 12 bytes (96-bit GCM nonce), for both wrap and content.
pub const NONCE_LEN: usize = 12;

/// AES-256-GCM authentication tag length.
pub const TAG_LEN: usize = 16;

/// A wrapped room key is exactly `ROOM_KEY_LEN + TAG_LEN` bytes: AEAD
/// preserves length apart from the tag.
pub const WRAPPED_ROOM_KEY_LEN: usize = ROOM_KEY_LEN + TAG_LEN;

/// HKDF `info` domain-separation prefix for the wrap KDF (spec D3, verbatim).
pub const WRAP_INFO_CONTEXT: &[u8] = b"iroh-rooms/content-key-wrap/v1";

/// Fail-closed suite gate (spec D3): accepts exactly [`SUITE_V1`].
///
/// # Errors
///
/// Returns [`CryptoError::UnsupportedSuite`] for any suite id other than
/// [`SUITE_V1`].
pub const fn ensure_suite(suite: u8) -> Result<(), CryptoError> {
    if suite == SUITE_V1 {
        Ok(())
    } else {
        Err(CryptoError::UnsupportedSuite(suite))
    }
}
