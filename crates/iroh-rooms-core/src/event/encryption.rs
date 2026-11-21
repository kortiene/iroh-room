//! Encrypted-content suite parameters and plaintext caps.
//!
//! Always compiled: the `content.encrypted` parser bounds-checks envelopes
//! against these values in a default build. The stable façade does not
//! re-export them; they are reachable from `iroh_rooms::experimental::encryption`
//! behind the `experimental` feature.

use super::constants::MAX_MESSAGE_BODY_BYTES;

/// Domain separator for the normative `content.encrypted` AAD (spec
/// `content-key-rotation.md` D2, pinned by #191 step 4):
/// `aad = ENCRYPTED_AAD_CONTEXT ‖ canonical-CBOR array of the ten cleartext
/// signed-prefix fields` (see `event::encrypted::encrypted_content_aad`).
/// Colon-style like the other event-layer contexts in `constants`; the crypto crate's
/// slash-style `WRAP_INFO_CONTEXT` is a D3 suite literal, a deliberate split.
pub const ENCRYPTED_AAD_CONTEXT: &[u8] = b"iroh-rooms:content-aad:v1";

/// The single pinned cryptographic suite id for `content.encrypted` envelopes
/// (spec `content-key-rotation.md` D3, `SUITE_V1`). Any other `suite` value is
/// rejected at parse, fail-closed. Pinned equal to
/// `iroh_rooms_crypto::SUITE_V1` by a dev-dependency cross-check test so the
/// two crates can never drift.
pub const ENCRYPTED_SUITE_V1: u8 = 0x01;

/// AEAD nonce length of a `content.encrypted` envelope (AES-256-GCM, 96-bit —
/// spec D3). Cross-pinned to `iroh_rooms_crypto::NONCE_LEN`.
pub const ENCRYPTED_NONCE_LEN: usize = 12;

/// AEAD authentication-tag length (AES-256-GCM — spec D3). AEAD preserves
/// plaintext length apart from this tag, which is what makes the D2a
/// ciphertext bound (`plaintext cap + tag`) exact. Cross-pinned to
/// `iroh_rooms_crypto::TAG_LEN`.
pub const ENCRYPTED_TAG_LEN: usize = 16;

/// Nonce length of a wrapped room key (AES-256-GCM, 96-bit — spec D3).
/// Cross-pinned to `iroh_rooms_crypto::NONCE_LEN`.
pub const WRAPPED_KEY_NONCE_LEN: usize = 12;

/// Ciphertext length of a wrapped room key: 32-byte key + 16-byte tag
/// (spec D3). Cross-pinned to `iroh_rooms_crypto::WRAPPED_ROOM_KEY_LEN`.
pub const WRAPPED_KEY_CIPHERTEXT_LEN: usize = 48;

/// Maximum canonical-CBOR plaintext bytes for an encrypted `message.text`
/// body (spec D2a): the §7 body cap plus 4 KiB headroom for `format`,
/// `in_reply_to`, `mentions`, and map overhead.
///
/// Like [`MAX_ENCRYPTED_PIPE_OPENED_PLAINTEXT`], this is a deliberate **new,
/// tighter authoring bound**, not a restatement of plaintext validity:
/// plaintext `message.text` places no count cap on `mentions`, so a
/// max-length body with more than ~120 mentions is strictly valid in
/// plaintext yet cannot be sealed within this bound (the headroom covers
/// ~120 mentions; the v1 room ceiling is N≤40). The R2 writer must keep the
/// plaintext CBOR under this cap before sealing — the bound is fail-closed
/// only and never widens what a reader accepts.
pub const MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT: usize = MAX_MESSAGE_BODY_BYTES + 4096;

/// Maximum canonical-CBOR plaintext bytes for an encrypted `file.shared` body
/// (spec D2a): name (255) + mime (255) + 16 providers (~544) + fixed fields
/// ≈ 1.3 KiB, with ~3× headroom.
pub const MAX_ENCRYPTED_FILE_SHARED_PLAINTEXT: usize = 4096;

/// Maximum canonical-CBOR plaintext bytes for an encrypted `pipe.opened` body
/// (spec D2a). `label`/`target_hint`/`alpn` carry no per-field cap today, so
/// this is a deliberate new authoring bound for the encrypted form; the R2
/// writer must keep the plaintext under it before sealing.
pub const MAX_ENCRYPTED_PIPE_OPENED_PLAINTEXT: usize = 8192;

/// Maximum canonical-CBOR plaintext bytes for an encrypted `pipe.closed` body
/// (spec D2a): a 16-byte id plus an enum reason.
pub const MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT: usize = 1024;

/// Maximum canonical-CBOR plaintext bytes for an encrypted `agent.status`
/// body (spec D2a): status (64) + message (4096) + 16 artifact ids (~288)
/// with headroom.
pub const MAX_ENCRYPTED_AGENT_STATUS_PLAINTEXT: usize = 8192;
