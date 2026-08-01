//! Secret key material: the per-epoch room content key and the ephemeral
//! X25519 wrap secret (spec D3/D3a), plus CSPRNG nonce generation.
//!
//! Both secret types zeroize on drop and redact their `Debug` output; the
//! only way to read the bytes back out is an explicit accessor.

use curve25519_dalek::montgomery::MontgomeryPoint;
use zeroize::Zeroize;

use crate::{NONCE_LEN, ROOM_KEY_LEN};

/// A 32-byte symmetric room content key, `room_key[epoch]` (spec D3):
/// the AES-256-GCM key for every content event sealed in its epoch.
pub struct RoomKey([u8; ROOM_KEY_LEN]);

impl RoomKey {
    /// Generates a fresh room key from the OS CSPRNG (spec D3: 32 bytes
    /// from `getrandom`).
    ///
    /// # Panics
    ///
    /// Panics if the OS CSPRNG is unavailable (same posture as
    /// `iroh-rooms-v2-core`'s key generation: an environment without an OS
    /// entropy source must not silently produce keys).
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; ROOM_KEY_LEN];
        getrandom::fill(&mut bytes).expect("OS CSPRNG (getrandom) must be available");
        Self(bytes)
    }

    /// Wraps existing key bytes (e.g. an unwrapped distribution record).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; ROOM_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ROOM_KEY_LEN] {
        &self.0
    }
}

impl Clone for RoomKey {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl Drop for RoomKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl core::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RoomKey(<redacted>)")
    }
}

/// A fresh ephemeral X25519 secret for one wrap (spec D3a): compromising
/// the admin's long-term key later must not retroactively unwrap past
/// distributions, so every wrap uses a new one. Not `Clone` on purpose.
pub struct EphemeralSecret(pub(crate) [u8; 32]);

impl EphemeralSecret {
    /// Generates a fresh ephemeral secret from the OS CSPRNG.
    ///
    /// # Panics
    ///
    /// Panics if the OS CSPRNG is unavailable (see [`RoomKey::generate`]).
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS CSPRNG (getrandom) must be available");
        Self(bytes)
    }

    /// Builds an ephemeral secret from fixed bytes.
    ///
    /// Only for golden-vector tests and cross-implementation checks: a
    /// production wrap must use [`EphemeralSecret::generate`] — reusing an
    /// ephemeral secret forfeits the wrap channel's forward secrecy (D3a).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The X25519 public key for this secret (RFC 7748 clamped base-point
    /// multiplication); rides in the distribution record next to the
    /// wrapped key (spec D5).
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        MontgomeryPoint::mul_base_clamped(self.0).to_bytes()
    }
}

impl Drop for EphemeralSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl core::fmt::Debug for EphemeralSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("EphemeralSecret(<redacted>)")
    }
}

/// Generates a random 96-bit AEAD nonce from the OS CSPRNG.
///
/// For content events the caller owns the uniqueness contract: a nonce must
/// be unique per `(key_epoch, event)` (spec D3) — nonce reuse under the same
/// key is catastrophic for GCM.
///
/// # Panics
///
/// Panics if the OS CSPRNG is unavailable (see [`RoomKey::generate`]).
#[must_use]
pub fn generate_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).expect("OS CSPRNG (getrandom) must be available");
    nonce
}
