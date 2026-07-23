//! Ed25519 signing/verification and secret-key helpers (spec §4 D1 / D2, issue
//! #146).
//!
//! # Key model assumption (OQ-2)
//!
//! `#134 §6` was unavailable in this checkout (spec §13/§15). The crate assumes a
//! **single signing key per principal** (OQ-2): a [`SigningKey`] derives a
//! [`MemberId`] (principal identity) which also serves as the verifying key for
//! that principal's record signatures. [`DeviceId`] exists as a distinct type so
//! a future two-key model (v1-style identity + device) can split identity from
//! device without a breaking change; today [`SigningKey::device_id`] returns the
//! same bytes as [`SigningKey::member_id`].

use core::fmt;
use core::str::FromStr;

use ed25519_dalek::{Signature as DalekSignature, Signer, VerifyingKey};
use zeroize::Zeroizing;

use crate::ids::{DeviceId, MemberId, LEN};

/// Error parsing a hex-encoded 64-byte signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureParseError {
    /// Actual decoded byte length.
    pub actual: usize,
}

impl fmt::Display for SignatureParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected {SIGNATURE_LEN} signature bytes, got {}",
            self.actual
        )
    }
}
impl std::error::Error for SignatureParseError {}

/// A signature that did not verify under the supplied key. Carries no detail by
/// design — verification is constant-shape and reveals nothing about the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureError;

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ed25519 signature verification failed")
    }
}
impl std::error::Error for SignatureError {}

/// Length in bytes of an Ed25519 detached signature.
pub const SIGNATURE_LEN: usize = 64;

/// An Ed25519 detached signature (64 bytes).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Wrap raw signature bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 64 signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }

    fn to_dalek(self) -> DalekSignature {
        DalekSignature::from_bytes(&self.0)
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", hex::encode(self.0))
    }
}

impl FromStr for Signature {
    type Err = SignatureParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|_| SignatureParseError { actual: 0 })?;
        let actual = bytes.len();
        <[u8; SIGNATURE_LEN]>::try_from(bytes.as_slice())
            .map_err(|_| SignatureParseError { actual })
            .map(Self)
    }
}

/// Verify a detached Ed25519 signature under a principal's public key, using
/// strict verification (rejects non-canonical signatures and small-order
/// points). This is the single verification entry point used by all v2 signed
/// records (spec D2 step 5).
///
/// # Errors
/// Returns [`SignatureError`] if the key is not a valid point or the signature
/// does not verify.
pub fn verify(
    principal: &MemberId,
    message: &[u8],
    signature: &Signature,
) -> Result<(), SignatureError> {
    let vk = VerifyingKey::from_bytes(principal.as_bytes()).map_err(|_| SignatureError)?;
    vk.verify_strict(message, &signature.to_dalek())
        .map_err(|_| SignatureError)
}

/// A secret Ed25519 signing key. Its public half is exposed as a [`MemberId`]
/// (principal identity) and, optionally, a [`DeviceId`].
pub struct SigningKey {
    inner: ed25519_dalek::SigningKey,
}

impl SigningKey {
    /// Construct a signing key from a 32-byte secret seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; LEN]) -> Self {
        Self {
            inner: ed25519_dalek::SigningKey::from_bytes(seed),
        }
    }

    /// Generate a fresh signing key from the operating-system CSPRNG.
    ///
    /// # Panics
    /// Panics only if the OS CSPRNG is unavailable. On supported platforms that
    /// indicates a serious system fault; there is no safe fallback for key
    /// generation, so failing loudly is correct.
    #[must_use]
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; LEN]);
        getrandom::fill(seed.as_mut_slice()).expect("OS CSPRNG (getrandom) must be available");
        Self::from_seed(&seed)
    }

    /// The 32-byte secret seed backing this key, for secure persistence only.
    /// Returned inside a [`Zeroizing`] buffer so the copy is wiped on drop.
    #[must_use]
    pub fn to_seed(&self) -> Zeroizing<[u8; LEN]> {
        Zeroizing::new(self.inner.to_bytes())
    }

    /// This key's public half viewed as a [`MemberId`] (principal identity).
    #[must_use]
    pub fn member_id(&self) -> MemberId {
        MemberId::from_bytes(self.inner.verifying_key().to_bytes())
    }

    /// Alias for [`Self::member_id`] (the principal identity).
    #[must_use]
    pub fn public_key(&self) -> MemberId {
        self.member_id()
    }

    /// This key's public half viewed as a [`DeviceId`] (same bytes as the
    /// principal under the single-key model; OQ-2).
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        DeviceId::from_bytes(self.inner.verifying_key().to_bytes())
    }

    /// Sign `message`, producing a detached Ed25519 signature.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.inner.sign(message).to_bytes())
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_seed_is_deterministic_and_verifies() {
        let key = SigningKey::from_seed(&[0x42; LEN]);
        let msg = b"iroh-rooms:v2 round-trip";
        let sig = key.sign(msg);
        verify(&key.member_id(), msg, &sig).expect("self-signature verifies");
        // Wrong message must fail.
        assert!(verify(&key.member_id(), b"other", &sig).is_err());
    }

    #[test]
    fn wrong_key_rejects() {
        let a = SigningKey::from_seed(&[1; LEN]);
        let b = SigningKey::from_seed(&[2; LEN]);
        let sig = a.sign(b"msg");
        assert!(verify(&b.member_id(), b"msg", &sig).is_err());
    }

    #[test]
    fn to_seed_round_trips() {
        let key = SigningKey::from_seed(&[0x9a; LEN]);
        let restored = SigningKey::from_seed(&key.to_seed());
        assert_eq!(key.member_id(), restored.member_id());
    }

    #[test]
    fn signature_hex_round_trip() {
        let key = SigningKey::from_seed(&[3; LEN]);
        let sig = key.sign(b"hex");
        let s = sig.to_string();
        let parsed: Signature = s.parse().unwrap();
        assert_eq!(parsed, sig);
    }

    #[test]
    fn generate_produces_distinct_verifying_keys() {
        let a = SigningKey::generate();
        let b = SigningKey::generate();
        assert_ne!(a.member_id(), b.member_id());
        let sig = a.sign(b"msg");
        verify(&a.member_id(), b"msg", &sig).expect("self-signature verifies");
    }
}
