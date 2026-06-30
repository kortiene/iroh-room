//! Ed25519 key material: identity keys (`sender_id`), device keys (`device_id`),
//! detached signatures, and a secret signing-key wrapper.
//!
//! See `PHASE-0-SPIKE.md` Event Protocol §1. Two **distinct** public keys
//! participate in every event:
//!
//! * [`IdentityKey`] — the stable principal (`sender_id`). Authorization and
//!   membership are tracked against this key. It signs only device-binding
//!   certificates, never events directly (MVP).
//! * [`DeviceKey`] — the per-device signing key (`device_id`), byte-for-byte the
//!   iroh `EndpointId`. **Event signatures MUST verify under this key, never
//!   `sender_id`.**
//!
//! The two are different Rust types on purpose (spec R3): the event-verification
//! entry point takes a [`DeviceKey`], so accidentally verifying under an
//! [`IdentityKey`] does not type-check.

use core::fmt;
use core::str::FromStr;

use ed25519_dalek::{Signature as DalekSignature, Signer, VerifyingKey};
use zeroize::Zeroizing;

use super::constants::{PUBLIC_KEY_LEN, SIGNATURE_LEN};

/// Error parsing a hex-encoded key or signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParseError {
    /// The hex string decoded to the wrong number of bytes.
    BadLength {
        /// Expected byte length.
        expected: usize,
        /// Actual decoded byte length.
        actual: usize,
    },
    /// The string was not valid lowercase/uppercase hex.
    BadHex,
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength { expected, actual } => {
                write!(f, "expected {expected} bytes, got {actual}")
            }
            Self::BadHex => f.write_str("invalid hex encoding"),
        }
    }
}

impl std::error::Error for KeyParseError {}

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

fn parse_fixed<const N: usize>(s: &str) -> Result<[u8; N], KeyParseError> {
    let bytes = hex::decode(s).map_err(|_| KeyParseError::BadHex)?;
    let actual = bytes.len();
    <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| KeyParseError::BadLength {
        expected: N,
        actual,
    })
}

macro_rules! public_key_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// `Ord` is the bytewise order of the raw public-key bytes. It carries no
        /// protocol meaning (timeline position is untrusted) and exists only so
        /// the membership layer can key deterministic `BTreeMap`s on identities
        /// and devices (spec §4) and apply the lowest-`event_id`-style stable
        /// tie-breaks over identities.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; PUBLIC_KEY_LEN]);

        impl $name {
            /// Wrap raw public-key bytes. The bytes are not validated as a curve
            /// point here; an invalid point fails closed at verification time.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; PUBLIC_KEY_LEN]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw 32 public-key bytes (as they appear on the wire).
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
                &self.0
            }

            /// Construct the underlying Ed25519 verifying key, or fail closed if
            /// the bytes are not a valid curve point.
            fn verifying_key(&self) -> Result<VerifyingKey, SignatureError> {
                VerifyingKey::from_bytes(&self.0).map_err(|_| SignatureError)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = KeyParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                parse_fixed::<PUBLIC_KEY_LEN>(s).map(Self)
            }
        }
    };
}

public_key_newtype! {
    /// The stable participant identity public key (`sender_id`, Event Protocol §1).
    IdentityKey
}

public_key_newtype! {
    /// The per-device signing public key (`device_id` == iroh `EndpointId`,
    /// Event Protocol §1). Event signatures verify under this key.
    DeviceKey
}

impl DeviceKey {
    /// Verify an event signature: `Ed25519_verify(device_id, message, signature)`.
    ///
    /// Uses strict verification (rejects non-canonical signatures and
    /// small-order points). This is the **only** event-signature verification
    /// entry point and it takes a [`DeviceKey`] by construction (spec R3).
    ///
    /// # Errors
    /// Returns [`SignatureError`] if the key is not a valid point or the
    /// signature does not verify.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), SignatureError> {
        let vk = self.verifying_key()?;
        vk.verify_strict(message, &signature.to_dalek())
            .map_err(|_| SignatureError)
    }
}

impl IdentityKey {
    /// Verify a device-binding certificate:
    /// `Ed25519_verify(sender_id, binding_msg, binding_sig)` (Event Protocol §1).
    ///
    /// # Errors
    /// Returns [`SignatureError`] if the key is not a valid point or the
    /// signature does not verify.
    pub fn verify_binding(
        &self,
        message: &[u8],
        signature: &Signature,
    ) -> Result<(), SignatureError> {
        let vk = self.verifying_key()?;
        vk.verify_strict(message, &signature.to_dalek())
            .map_err(|_| SignatureError)
    }
}

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
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_fixed::<SIGNATURE_LEN>(s).map(Self)
    }
}

/// A secret Ed25519 signing key. Used to author events (sign over
/// `EVENT_CONTEXT ‖ CSB`) and to issue device-binding certificates.
///
/// The same secret backs the iroh `Endpoint` secret in the full system; here it
/// is just an Ed25519 key. Its public half is exposed as either a [`DeviceKey`]
/// or an [`IdentityKey`] depending on the role it plays — the caller chooses.
pub struct SigningKey {
    inner: ed25519_dalek::SigningKey,
}

impl SigningKey {
    /// Construct a signing key from a 32-byte secret seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; PUBLIC_KEY_LEN]) -> Self {
        Self {
            inner: ed25519_dalek::SigningKey::from_bytes(seed),
        }
    }

    /// Generate a fresh signing key from the operating-system CSPRNG.
    ///
    /// Fills a 32-byte seed from the OS RNG (`getrandom`, the same entropy source
    /// `ed25519-dalek`'s own `generate` draws from), constructs the key through
    /// the sole [`from_seed`](Self::from_seed) constructor, then zeroizes the
    /// transient seed buffer so no plaintext copy of the secret lingers in memory.
    ///
    /// # Panics
    /// Panics only if the OS CSPRNG is unavailable (`getrandom` returns an error).
    /// On supported platforms that indicates a serious system fault, and there is
    /// no safe fallback for cryptographic key generation, so failing loudly is the
    /// correct behavior.
    #[must_use]
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; PUBLIC_KEY_LEN]);
        getrandom::fill(seed.as_mut_slice()).expect("OS CSPRNG (getrandom) must be available");
        Self::from_seed(&seed)
    }

    /// The 32-byte secret seed backing this key, for **secure on-disk
    /// persistence only**.
    ///
    /// Returned inside a [`Zeroizing`] buffer so the copy is wiped on drop. This
    /// is the only way secret bytes leave the wrapper; callers MUST treat the
    /// result as secret — never log, `Display`, or otherwise expose it.
    #[must_use]
    pub fn to_seed(&self) -> Zeroizing<[u8; PUBLIC_KEY_LEN]> {
        Zeroizing::new(self.inner.to_bytes())
    }

    /// The raw public-key bytes of this signing key.
    #[must_use]
    pub fn public_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.inner.verifying_key().to_bytes()
    }

    /// This key's public half viewed as a [`DeviceKey`] (`device_id`).
    #[must_use]
    pub fn device_key(&self) -> DeviceKey {
        DeviceKey(self.public_bytes())
    }

    /// This key's public half viewed as an [`IdentityKey`] (`sender_id`).
    #[must_use]
    pub fn identity_key(&self) -> IdentityKey {
        IdentityKey(self.public_bytes())
    }

    /// Sign `message`, producing a detached Ed25519 signature.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.inner.sign(message).to_bytes())
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print secret bytes.
        f.write_str("SigningKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::{IdentityKey, SigningKey};
    use crate::event::constants::PUBLIC_KEY_LEN;

    #[test]
    fn generate_produces_distinct_keys() {
        let a = SigningKey::generate();
        let b = SigningKey::generate();
        // Two fresh CSPRNG draws must not collide.
        assert_ne!(a.public_bytes(), b.public_bytes());
        // A separately generated identity key and device key are distinct keys
        // (spike §1: `sender_id` and `device_id` are different Ed25519 keys).
        assert_ne!(
            a.identity_key(),
            IdentityKey::from_bytes(b.device_key().as_bytes().to_owned())
        );
    }

    #[test]
    fn generated_key_round_trips_sign_and_verify() {
        let key = SigningKey::generate();
        let message = b"iroh-rooms generate() round-trip";
        let sig = key.sign(message);
        // Event signatures verify under the device-key view of the public half.
        key.device_key()
            .verify(message, &sig)
            .expect("a freshly generated key must verify its own signature");
    }

    #[test]
    fn to_seed_round_trips_through_from_seed() {
        let key = SigningKey::generate();
        let seed = key.to_seed();
        assert_eq!(seed.len(), PUBLIC_KEY_LEN);
        // Reconstructing from the persisted seed yields the same public key —
        // this is exactly what the CLI relies on to store and reload an identity.
        let restored = SigningKey::from_seed(&seed);
        assert_eq!(key.public_bytes(), restored.public_bytes());
    }
}
