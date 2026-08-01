//! Typed failure surface of the `SUITE_V1` primitives.
//!
//! Every fallible operation in this crate returns [`CryptoError`]; nothing
//! panics on untrusted input (spec §5 discipline: AEAD failure and malformed
//! peer bytes are typed errors, never panics). Error variants deliberately
//! carry no key material.

use core::fmt;

/// Failure of a `SUITE_V1` primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The suite id is not [`SUITE_V1`](crate::SUITE_V1) — rejected
    /// fail-closed (spec D3).
    UnsupportedSuite(u8),
    /// The recipient bytes are not a valid Ed25519 public key
    /// (decompression failed).
    InvalidEd25519PublicKey,
    /// The recipient Ed25519 public key is a small-order (weak) point; a
    /// wrap to it would not bind to a real device key.
    WeakEd25519PublicKey,
    /// X25519 produced the all-zero shared secret (small-order peer point,
    /// RFC 7748 §6.1) — rejected fail-closed.
    NonContributoryKeyExchange,
    /// AEAD open failed: wrong key, nonce, AAD, epoch, or tampered
    /// ciphertext. Deliberately not more specific — AES-GCM cannot
    /// distinguish these, and a caller must treat them all as "unreadable".
    AeadOpenFailure,
    /// AEAD seal failed (plaintext exceeds the AES-GCM length bound).
    AeadSealFailure,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSuite(suite) => {
                write!(
                    f,
                    "unsupported cryptographic suite id {suite:#04x} (fail-closed)"
                )
            }
            Self::InvalidEd25519PublicKey => f.write_str("invalid Ed25519 public key"),
            Self::WeakEd25519PublicKey => {
                f.write_str("weak (small-order) Ed25519 public key rejected")
            }
            Self::NonContributoryKeyExchange => {
                f.write_str("X25519 produced the all-zero shared secret (fail-closed)")
            }
            Self::AeadOpenFailure => {
                f.write_str("AEAD open failed (wrong key/nonce/AAD or tampered ciphertext)")
            }
            Self::AeadSealFailure => f.write_str("AEAD seal failed (plaintext too large)"),
        }
    }
}

impl std::error::Error for CryptoError {}
