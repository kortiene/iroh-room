//! `SUITE_V1` key wrap / unwrap (spec D3, D3a).
//!
//! One wrap protects one `room_key[epoch]` for one recipient device:
//!
//! ```text
//! shared   = X25519(ephemeral_secret, recipient_device_x25519_pub)
//! wrap_key = HKDF-SHA-256(ikm = shared, salt = room_id,
//!                         info = WRAP_INFO_CONTEXT ‖ key_epoch_be8 ‖ recipient_device)
//! wrapped  = AES-256-GCM(key = wrap_key, nonce = 12 B, plaintext = room_key,
//!                        aad = room_id ‖ key_epoch_be8 ‖ recipient_device)
//! record   = (ephemeral_pub, nonce, wrapped)
//! ```
//!
//! `key_epoch_be8` is the epoch as 8 bytes big-endian; `recipient_device` is
//! the recipient's 32-byte Ed25519 public key. Binding the epoch, room, and
//! recipient into both the KDF and the AAD means a wrapped key cannot be
//! replayed for another epoch, room, or device without the AEAD failing.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::convert::{ed25519_public_to_x25519, ed25519_seed_to_x25519};
use crate::error::CryptoError;
use crate::keys::{generate_nonce, EphemeralSecret, RoomKey};
use crate::{ensure_suite, NONCE_LEN, ROOM_KEY_LEN, WRAPPED_ROOM_KEY_LEN, WRAP_INFO_CONTEXT};

/// Length of the wrap KDF `info` buffer: context ‖ epoch (8 B BE) ‖ device (32 B).
const WRAP_INFO_LEN: usize = WRAP_INFO_CONTEXT.len() + 8 + 32;

/// Length of the wrap AAD buffer: room id (32 B) ‖ epoch (8 B BE) ‖ device (32 B).
const WRAP_AAD_LEN: usize = 32 + 8 + 32;

/// One recipient's wrapped `room_key[epoch]` — the per-recipient entry of a
/// `MemberKeyDistribution` record (spec D5). All fields are public wire data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedRoomKey {
    /// The admin's fresh ephemeral X25519 public key for this wrap (D3a).
    pub ephemeral_public: [u8; 32],
    /// The wrap AEAD nonce.
    pub nonce: [u8; NONCE_LEN],
    /// `AES-256-GCM(wrap_key, nonce, room_key, aad)` — always
    /// [`WRAPPED_ROOM_KEY_LEN`] bytes (32-byte key + 16-byte tag).
    pub ciphertext: [u8; WRAPPED_ROOM_KEY_LEN],
}

/// Wraps `room_key` for one recipient device with a fresh ephemeral X25519
/// secret and a fresh random nonce (spec D3a: one ephemeral per wrap).
///
/// `recipient_device` is the recipient's 32-byte Ed25519 public key
/// (`device_id`); `room_id` is the 32-byte room digest; `key_epoch` is the
/// epoch this key belongs to.
///
/// # Errors
///
/// - [`CryptoError::InvalidEd25519PublicKey`] /
///   [`CryptoError::WeakEd25519PublicKey`] if `recipient_device` is not a
///   usable Ed25519 key.
/// - [`CryptoError::NonContributoryKeyExchange`] if X25519 yields the
///   all-zero shared secret.
/// - [`CryptoError::AeadSealFailure`] if the AEAD rejects the seal
///   (unreachable for the fixed 32-byte plaintext, kept typed rather than
///   panicking).
///
/// # Panics
///
/// Panics if the OS CSPRNG is unavailable (see
/// [`RoomKey::generate`](crate::RoomKey::generate)).
pub fn wrap_room_key(
    room_key: &RoomKey,
    recipient_device: &[u8; 32],
    room_id: &[u8; 32],
    key_epoch: u64,
) -> Result<WrappedRoomKey, CryptoError> {
    let ephemeral = EphemeralSecret::generate();
    let nonce = generate_nonce();
    wrap_room_key_with(
        room_key,
        &ephemeral,
        nonce,
        recipient_device,
        room_id,
        key_epoch,
    )
}

/// Deterministic wrap core: like [`wrap_room_key`] but with a caller-supplied
/// ephemeral secret and nonce.
///
/// This exists so golden vectors and cross-implementation tests can pin exact
/// output bytes. Production callers must use [`wrap_room_key`]: supplying a
/// reused ephemeral secret or nonce here forfeits the wrap channel's
/// forward secrecy and GCM's security.
///
/// # Errors
///
/// As [`wrap_room_key`].
pub fn wrap_room_key_with(
    room_key: &RoomKey,
    ephemeral: &EphemeralSecret,
    nonce: [u8; NONCE_LEN],
    recipient_device: &[u8; 32],
    room_id: &[u8; 32],
    key_epoch: u64,
) -> Result<WrappedRoomKey, CryptoError> {
    let recipient_x25519 = ed25519_public_to_x25519(recipient_device)?;
    let shared = Zeroizing::new(
        MontgomeryPoint(recipient_x25519)
            .mul_clamped(ephemeral.0)
            .to_bytes(),
    );
    let wrap_key = derive_wrap_key(&shared, room_id, key_epoch, recipient_device)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(wrap_key.as_slice()));
    let aad = wrap_aad(room_id, key_epoch, recipient_device);
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: room_key.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::AeadSealFailure)?;
    let ciphertext: [u8; WRAPPED_ROOM_KEY_LEN] = sealed
        .try_into()
        .map_err(|_| CryptoError::AeadSealFailure)?;

    Ok(WrappedRoomKey {
        ephemeral_public: ephemeral.public_key(),
        nonce,
        ciphertext,
    })
}

/// Unwraps a [`WrappedRoomKey`] with the recipient's Ed25519 signing seed,
/// recovering `room_key[key_epoch]`.
///
/// The recipient device public key (KDF/AAD input) is derived from the seed,
/// so it cannot disagree with the secret used for the DH. `suite` is the
/// suite id from the distribution's envelope; anything other than
/// [`SUITE_V1`](crate::SUITE_V1) is rejected fail-closed before any
/// cryptography runs.
///
/// # Errors
///
/// - [`CryptoError::UnsupportedSuite`] for a foreign suite id.
/// - [`CryptoError::NonContributoryKeyExchange`] if X25519 yields the
///   all-zero shared secret (small-order `ephemeral_public`).
/// - [`CryptoError::AeadOpenFailure`] if authentication fails: wrong
///   recipient, wrong room, wrong epoch, or tampered bytes. Never panics on
///   peer-supplied bytes.
pub fn unwrap_room_key(
    suite: u8,
    wrapped: &WrappedRoomKey,
    recipient_seed: &[u8; 32],
    room_id: &[u8; 32],
    key_epoch: u64,
) -> Result<RoomKey, CryptoError> {
    ensure_suite(suite)?;

    let recipient_device = SigningKey::from_bytes(recipient_seed)
        .verifying_key()
        .to_bytes();
    let x25519_secret = ed25519_seed_to_x25519(recipient_seed);
    let shared = Zeroizing::new(
        MontgomeryPoint(wrapped.ephemeral_public)
            .mul_clamped(*x25519_secret)
            .to_bytes(),
    );
    let wrap_key = derive_wrap_key(&shared, room_id, key_epoch, &recipient_device)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(wrap_key.as_slice()));
    let aad = wrap_aad(room_id, key_epoch, &recipient_device);
    let opened = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&wrapped.nonce),
                Payload {
                    msg: &wrapped.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::AeadOpenFailure)?,
    );
    let key: [u8; ROOM_KEY_LEN] = opened
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::AeadOpenFailure)?;
    Ok(RoomKey::from_bytes(key))
}

/// `HKDF-SHA-256(ikm = shared, salt = room_id, info = context ‖ epoch ‖ device)`
/// (spec D3 — no salt-less shortcut). Rejects the all-zero DH output first
/// (RFC 7748 §6.1 contributory check).
fn derive_wrap_key(
    shared: &[u8; 32],
    room_id: &[u8; 32],
    key_epoch: u64,
    recipient_device: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    const CONTEXT_LEN: usize = WRAP_INFO_CONTEXT.len();

    if shared == &[0u8; 32] {
        return Err(CryptoError::NonContributoryKeyExchange);
    }

    let mut info = [0u8; WRAP_INFO_LEN];
    info[..CONTEXT_LEN].copy_from_slice(WRAP_INFO_CONTEXT);
    info[CONTEXT_LEN..CONTEXT_LEN + 8].copy_from_slice(&key_epoch.to_be_bytes());
    info[CONTEXT_LEN + 8..].copy_from_slice(recipient_device);

    let mut wrap_key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(room_id), shared)
        .expand(&info, wrap_key.as_mut())
        // 32 bytes is far below the RFC 5869 bound of 255 * 32; this cannot
        // fail for this fixed output length.
        .expect("HKDF-SHA-256 expand to 32 bytes is always within bounds");
    Ok(wrap_key)
}

/// `aad = room_id ‖ key_epoch_be8 ‖ recipient_device` (spec D3).
fn wrap_aad(room_id: &[u8; 32], key_epoch: u64, recipient_device: &[u8; 32]) -> [u8; WRAP_AAD_LEN] {
    let mut aad = [0u8; WRAP_AAD_LEN];
    aad[..32].copy_from_slice(room_id);
    aad[32..40].copy_from_slice(&key_epoch.to_be_bytes());
    aad[40..].copy_from_slice(recipient_device);
    aad
}
