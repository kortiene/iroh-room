//! `SUITE_V1` content AEAD (spec D2/D3): AES-256-GCM under `room_key[epoch]`.
//!
//! These functions operate on opaque bytes plus an explicit caller-built AAD
//! buffer. The AAD for a real content event is the cleartext signed prefix
//! (`schema_version ‖ room_id ‖ sender_id ‖ device_id ‖ event_type ‖
//! created_at ‖ prev_events ‖ inner_type ‖ key_epoch ‖ suite`, spec D2) —
//! building it requires the event types in `iroh-rooms-core`, which this
//! crate deliberately does not import. Encoding that prefix and choosing a
//! nonce unique per `(key_epoch, event)` are the caller's contract (§7
//! steps 3–4).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::error::CryptoError;
use crate::keys::RoomKey;
use crate::{ensure_suite, NONCE_LEN};

/// Seals a content body: `AES-256-GCM(room_key, nonce, plaintext, aad)`.
///
/// The returned ciphertext is `plaintext.len() + 16` bytes (AEAD preserves
/// length apart from the tag — the D2a bound relies on this). The nonce must
/// be unique per `(key_epoch, event)`; reuse under one key is catastrophic
/// for GCM.
///
/// # Errors
///
/// Returns [`CryptoError::AeadSealFailure`] if the plaintext exceeds the
/// AES-GCM length bound (~64 GiB — unreachable behind the 1 MiB wire frame,
/// kept typed rather than panicking).
pub fn seal_content(
    room_key: &RoomKey,
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(room_key.as_bytes()));
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::AeadSealFailure)
}

/// Opens a sealed content body, authenticating it against `aad`.
///
/// `suite` is the envelope's suite id; anything other than
/// [`SUITE_V1`](crate::SUITE_V1) is rejected fail-closed before any
/// cryptography runs (spec D3).
///
/// # Errors
///
/// - [`CryptoError::UnsupportedSuite`] for a foreign suite id.
/// - [`CryptoError::AeadOpenFailure`] if authentication fails: wrong key
///   (missing epoch), wrong nonce/AAD, or tampered ciphertext. The caller
///   treats this as *unreadable*, never *invalid* (spec D2b/§5); this
///   function never panics on peer-supplied bytes.
pub fn open_content(
    suite: u8,
    room_key: &RoomKey,
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    ensure_suite(suite)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(room_key.as_bytes()));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::AeadOpenFailure)
}
