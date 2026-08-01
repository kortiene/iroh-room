//! Ed25519 → X25519 conversion for device keys (spec D3).
//!
//! The repo's device identities are Ed25519 (`device_id` = the 32-byte
//! verifying key). `SUITE_V1` wraps keys over X25519, so both halves convert:
//!
//! - **public:** the Edwards→Montgomery birational map `u = (1 + y)/(1 − y)`
//!   (`VerifyingKey::to_montgomery`);
//! - **secret:** the RFC 8032 expansion — the lower 32 bytes of
//!   `SHA-512(seed)` (`SigningKey::to_scalar_bytes`), clamped inside the
//!   X25519 scalar multiplication itself.
//!
//! Both directions are pinned by the golden vectors against an independent
//! implementation (`tests/golden/suite_v1_reference.py`).

use ed25519_dalek::{SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::error::CryptoError;

/// Converts an Ed25519 device public key to its X25519 (Montgomery) form.
///
/// Fail-closed hardening: small-order (weak) points are rejected here, before
/// any DH — a wrap to such a key would produce a predictable shared secret.
///
/// # Errors
///
/// - [`CryptoError::InvalidEd25519PublicKey`] if the bytes do not decompress
///   to a curve point.
/// - [`CryptoError::WeakEd25519PublicKey`] if the point is small-order.
pub fn ed25519_public_to_x25519(ed25519_public: &[u8; 32]) -> Result<[u8; 32], CryptoError> {
    let key = VerifyingKey::from_bytes(ed25519_public)
        .map_err(|_| CryptoError::InvalidEd25519PublicKey)?;
    if key.is_weak() {
        return Err(CryptoError::WeakEd25519PublicKey);
    }
    Ok(key.to_montgomery().to_bytes())
}

/// Converts an Ed25519 signing seed to the matching X25519 secret bytes.
///
/// The result is secret key material, returned under [`Zeroizing`] so it is
/// wiped on drop like every other secret in this crate; the crate's own
/// [`unwrap_room_key`](crate::unwrap_room_key) goes through this function.
#[must_use]
pub fn ed25519_seed_to_x25519(seed: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(SigningKey::from_bytes(seed).to_scalar_bytes())
}
