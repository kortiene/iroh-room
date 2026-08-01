//! `SUITE_V1` golden vectors and negative tests (spec `content-key-rotation.md`
//! D3/D3a, §7 step 2: "golden vectors pin a known-answer wrap + content
//! encryption so two implementations must agree byte-for-byte").
//!
//! Every `GOLDEN_*` constant below was derived by an INDEPENDENT
//! implementation stack — OpenSSL (via Python `cryptography`) for Ed25519,
//! X25519, HKDF-SHA-256, and AES-256-GCM, plus a hand-rolled
//! Edwards→Montgomery birational map — by `tests/golden/suite_v1_reference.py`
//! in this directory. The Rust crate under test (`curve25519-dalek` +
//! `RustCrypto`) shares no code with that stack, so agreement here is a real
//! cross-implementation check, not a self-fulfilling snapshot.

use iroh_rooms_crypto::{
    ed25519_public_to_x25519, ed25519_seed_to_x25519, generate_nonce, open_content, seal_content,
    unwrap_room_key, wrap_room_key, wrap_room_key_with, CryptoError, EphemeralSecret, RoomKey,
    WrappedRoomKey, NONCE_LEN, SUITE_V1, TAG_LEN, WRAPPED_ROOM_KEY_LEN,
};

// --- fixed golden inputs (non-secret test patterns) ------------------------

/// `start, start+1, …` — the fixed byte patterns the reference script uses.
fn pattern<const N: usize>(start: u8) -> [u8; N] {
    core::array::from_fn(|i| start.wrapping_add(u8::try_from(i).expect("pattern index fits u8")))
}

fn room_key() -> RoomKey {
    RoomKey::from_bytes(pattern::<32>(0x00))
}
const RECIPIENT_SEED_START: u8 = 0x20;
const EPHEMERAL_SECRET_START: u8 = 0x40;
const ROOM_ID_START: u8 = 0x60;
const KEY_EPOCH: u64 = 7;
const WRAP_NONCE_START: u8 = 0x80;
const CONTENT_NONCE_START: u8 = 0x90;
const CONTENT_PLAINTEXT: &[u8] = b"iroh-rooms #191 SUITE_V1 content golden vector";
const CONTENT_AAD: &[u8] =
    b"iroh-rooms #191 golden AAD (schema|room|sender|device|type|created|prev|inner|epoch|suite)";

// --- pinned outputs from the independent reference implementation ----------

const GOLDEN_RECIPIENT_ED_PUB: &str =
    "29acbae141bccaf0b22e1a94d34d0bc7361e526d0bfe12c89794bc9322966dd7";
const GOLDEN_RECIPIENT_X_PUB: &str =
    "5730800ab340fcb18ce5111eda9d705f91388b41e4544cbd103ba5942db2233e";
const GOLDEN_RECIPIENT_X_SECRET: &str =
    "887af58a36202e05c4c1cfec5bf6c61fad66bca851536004074b31f1b56e4ac9";
const GOLDEN_EPHEMERAL_PUB: &str =
    "79a631eede1bf9c98f12032cdeadd0e7a079398fc786b88cc846ec89af85a51a";
const GOLDEN_WRAPPED_CT: &str =
    "8d43dcac931cbbe63336258b6359b76862e282d9fb90401c8f1c19d69e36d35ecc6bfa327e45ff5ffd7ff046c8c57b7e";
const GOLDEN_CONTENT_CT: &str =
    "9fb89a5715eb9190228f60005b40de7e394cfc5867e1c832ac7a2f989ad784c00825459056084e218704f424389dba79ff4e527a5f8019c9f27523a062bc";
/// Smallest canonical compressed-Edwards `y` that fails decompression
/// (`y = 2`), computed by the reference script's field math.
const GOLDEN_INVALID_ED_POINT: &str =
    "0200000000000000000000000000000000000000000000000000000000000000";

fn hex_array<const N: usize>(hex: &str) -> [u8; N] {
    let bytes = hex::decode(hex).expect("golden constant is valid hex");
    bytes.try_into().expect("golden constant has pinned length")
}

fn recipient_device() -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&pattern::<32>(RECIPIENT_SEED_START))
        .verifying_key()
        .to_bytes()
}

fn golden_wrapped() -> WrappedRoomKey {
    WrappedRoomKey {
        ephemeral_public: hex_array::<32>(GOLDEN_EPHEMERAL_PUB),
        nonce: pattern::<NONCE_LEN>(WRAP_NONCE_START),
        ciphertext: hex_array::<WRAPPED_ROOM_KEY_LEN>(GOLDEN_WRAPPED_CT),
    }
}

// --- cross-implementation golden vectors -----------------------------------

/// The Ed25519 identity and both X25519 conversions agree with the
/// independent stack (OpenSSL Ed25519; hand-rolled birational map; OpenSSL
/// X25519 base-point multiplication).
#[test]
fn golden_identity_and_conversions_match_reference() {
    assert_eq!(recipient_device(), hex_array::<32>(GOLDEN_RECIPIENT_ED_PUB));
    assert_eq!(
        ed25519_public_to_x25519(&recipient_device()).expect("valid device key"),
        hex_array::<32>(GOLDEN_RECIPIENT_X_PUB)
    );
    assert_eq!(
        *ed25519_seed_to_x25519(&pattern::<32>(RECIPIENT_SEED_START)),
        hex_array::<32>(GOLDEN_RECIPIENT_X_SECRET),
        "seed-side conversion must match the reference's SHA-512(seed) lower half"
    );
    assert_eq!(
        EphemeralSecret::from_bytes(pattern::<32>(EPHEMERAL_SECRET_START)).public_key(),
        hex_array::<32>(GOLDEN_EPHEMERAL_PUB)
    );
}

/// Fixed-input wrap produces exactly the reference bytes:
/// `(ephemeral_pub, nonce, ciphertext)` all pinned (spec §7 step 2).
#[test]
fn golden_wrap_vector_matches_reference() {
    let wrapped = wrap_room_key_with(
        &room_key(),
        &EphemeralSecret::from_bytes(pattern::<32>(EPHEMERAL_SECRET_START)),
        pattern::<NONCE_LEN>(WRAP_NONCE_START),
        &recipient_device(),
        &pattern::<32>(ROOM_ID_START),
        KEY_EPOCH,
    )
    .expect("golden wrap succeeds");
    assert_eq!(wrapped, golden_wrapped());
}

/// The reference implementation's wrapped bytes unwrap to the original room
/// key with only the recipient's Ed25519 seed (known-answer round-trip).
#[test]
fn golden_wrapped_bytes_unwrap_to_room_key() {
    let unwrapped = unwrap_room_key(
        SUITE_V1,
        &golden_wrapped(),
        &pattern::<32>(RECIPIENT_SEED_START),
        &pattern::<32>(ROOM_ID_START),
        KEY_EPOCH,
    )
    .expect("golden unwrap succeeds");
    assert_eq!(unwrapped.as_bytes(), room_key().as_bytes());
}

/// Fixed-input content seal produces exactly the reference ciphertext, and
/// the reference ciphertext opens back to the plaintext.
#[test]
fn golden_content_vector_matches_reference() {
    let nonce = pattern::<NONCE_LEN>(CONTENT_NONCE_START);
    let sealed = seal_content(&room_key(), &nonce, CONTENT_PLAINTEXT, CONTENT_AAD)
        .expect("golden seal succeeds");
    assert_eq!(sealed, hex::decode(GOLDEN_CONTENT_CT).expect("valid hex"));
    assert_eq!(sealed.len(), CONTENT_PLAINTEXT.len() + TAG_LEN);

    let opened = open_content(SUITE_V1, &room_key(), &nonce, &sealed, CONTENT_AAD)
        .expect("golden open succeeds");
    assert_eq!(opened, CONTENT_PLAINTEXT);
}

// --- determinism and random-path round-trips -------------------------------

/// Same inputs → same outputs across two calls (cross-implementation
/// determinism requirement).
#[test]
fn deterministic_wrap_and_seal_are_reproducible() {
    let wrap = |()| {
        wrap_room_key_with(
            &room_key(),
            &EphemeralSecret::from_bytes(pattern::<32>(EPHEMERAL_SECRET_START)),
            pattern::<NONCE_LEN>(WRAP_NONCE_START),
            &recipient_device(),
            &pattern::<32>(ROOM_ID_START),
            KEY_EPOCH,
        )
        .expect("wrap succeeds")
    };
    assert_eq!(wrap(()), wrap(()));

    let nonce = pattern::<NONCE_LEN>(CONTENT_NONCE_START);
    let seal =
        |()| seal_content(&room_key(), &nonce, CONTENT_PLAINTEXT, CONTENT_AAD).expect("seals");
    assert_eq!(seal(()), seal(()));
}

/// The production wrap path draws a fresh ephemeral key and nonce every
/// time (D3a), and every wrap still unwraps to the same room key.
#[test]
fn random_wrap_path_is_fresh_and_round_trips() {
    let key = RoomKey::generate();
    let room_id = pattern::<32>(ROOM_ID_START);
    let first = wrap_room_key(&key, &recipient_device(), &room_id, KEY_EPOCH).expect("wraps");
    let second = wrap_room_key(&key, &recipient_device(), &room_id, KEY_EPOCH).expect("wraps");
    assert_ne!(
        first.ephemeral_public, second.ephemeral_public,
        "each wrap must use a fresh ephemeral key (D3a)"
    );
    assert_ne!(
        first.nonce, second.nonce,
        "each wrap must use a fresh nonce"
    );
    for wrapped in [&first, &second] {
        let unwrapped = unwrap_room_key(
            SUITE_V1,
            wrapped,
            &pattern::<32>(RECIPIENT_SEED_START),
            &room_id,
            KEY_EPOCH,
        )
        .expect("unwraps");
        assert_eq!(unwrapped.as_bytes(), key.as_bytes());
    }
}

/// Content round-trip under a random key and nonce.
#[test]
fn content_roundtrip_with_random_key() {
    let key = RoomKey::generate();
    let nonce = generate_nonce();
    let sealed = seal_content(&key, &nonce, CONTENT_PLAINTEXT, CONTENT_AAD).expect("seals");
    assert_eq!(sealed.len(), CONTENT_PLAINTEXT.len() + TAG_LEN);
    let opened = open_content(SUITE_V1, &key, &nonce, &sealed, CONTENT_AAD).expect("opens");
    assert_eq!(opened, CONTENT_PLAINTEXT);
}

// --- negative tests: typed errors, never panics ----------------------------

/// Wrong recipient, wrong epoch, wrong room, tampered ciphertext, tampered
/// nonce: all fail with the typed AEAD error (never a panic, spec §5).
#[test]
fn unwrap_rejects_wrong_inputs() {
    let seed = pattern::<32>(RECIPIENT_SEED_START);
    let room_id = pattern::<32>(ROOM_ID_START);

    let wrong_seed = pattern::<32>(0x55);
    assert_eq!(
        unwrap_room_key(
            SUITE_V1,
            &golden_wrapped(),
            &wrong_seed,
            &room_id,
            KEY_EPOCH
        )
        .expect_err("wrong recipient seed must fail authentication"),
        CryptoError::AeadOpenFailure
    );

    assert_eq!(
        unwrap_room_key(SUITE_V1, &golden_wrapped(), &seed, &room_id, KEY_EPOCH + 1)
            .expect_err("wrong epoch must fail (epoch is bound into KDF info and AAD)"),
        CryptoError::AeadOpenFailure
    );

    let wrong_room = pattern::<32>(0x77);
    assert_eq!(
        unwrap_room_key(SUITE_V1, &golden_wrapped(), &seed, &wrong_room, KEY_EPOCH)
            .expect_err("wrong room must fail (room_id is the HKDF salt and in the AAD)"),
        CryptoError::AeadOpenFailure
    );

    let mut tampered = golden_wrapped();
    tampered.ciphertext[0] ^= 0x01;
    assert_eq!(
        unwrap_room_key(SUITE_V1, &tampered, &seed, &room_id, KEY_EPOCH)
            .expect_err("tampered ciphertext must fail authentication"),
        CryptoError::AeadOpenFailure
    );

    let mut wrong_nonce = golden_wrapped();
    wrong_nonce.nonce[0] ^= 0x01;
    assert_eq!(
        unwrap_room_key(SUITE_V1, &wrong_nonce, &seed, &room_id, KEY_EPOCH)
            .expect_err("tampered nonce must fail authentication"),
        CryptoError::AeadOpenFailure
    );
}

/// Any suite id other than `SUITE_V1` is rejected fail-closed before any
/// cryptography runs (spec D3).
#[test]
fn foreign_suite_ids_are_rejected_fail_closed() {
    let seed = pattern::<32>(RECIPIENT_SEED_START);
    let room_id = pattern::<32>(ROOM_ID_START);
    for suite in [0x00, 0x02, 0xff] {
        assert_eq!(
            unwrap_room_key(suite, &golden_wrapped(), &seed, &room_id, KEY_EPOCH)
                .expect_err("must fail with a typed error"),
            CryptoError::UnsupportedSuite(suite)
        );
        assert_eq!(
            open_content(
                suite,
                &room_key(),
                &pattern::<NONCE_LEN>(CONTENT_NONCE_START),
                &hex::decode(GOLDEN_CONTENT_CT).expect("valid hex"),
                CONTENT_AAD,
            ),
            Err(CryptoError::UnsupportedSuite(suite))
        );
    }
    assert_eq!(iroh_rooms_crypto::ensure_suite(SUITE_V1), Ok(()));
}

/// A small-order ephemeral public key would force the all-zero shared
/// secret; the unwrap rejects it fail-closed (RFC 7748 §6.1).
#[test]
fn small_order_ephemeral_public_is_rejected() {
    let mut wrapped = golden_wrapped();
    wrapped.ephemeral_public = [0u8; 32];
    assert_eq!(
        unwrap_room_key(
            SUITE_V1,
            &wrapped,
            &pattern::<32>(RECIPIENT_SEED_START),
            &pattern::<32>(ROOM_ID_START),
            KEY_EPOCH,
        )
        .expect_err("must fail with a typed error"),
        CryptoError::NonContributoryKeyExchange
    );
}

/// Content AEAD negatives: tampered ciphertext, wrong AAD, wrong nonce,
/// wrong key, truncated/empty ciphertext — all typed errors, no panics.
#[test]
fn open_content_rejects_wrong_inputs() {
    let nonce = pattern::<NONCE_LEN>(CONTENT_NONCE_START);
    let sealed = hex::decode(GOLDEN_CONTENT_CT).expect("valid hex");

    let mut tampered = sealed.clone();
    tampered[0] ^= 0x01;
    assert_eq!(
        open_content(SUITE_V1, &room_key(), &nonce, &tampered, CONTENT_AAD),
        Err(CryptoError::AeadOpenFailure)
    );

    assert_eq!(
        open_content(SUITE_V1, &room_key(), &nonce, &sealed, b"different aad"),
        Err(CryptoError::AeadOpenFailure),
        "the AAD binds the ciphertext to its event prefix (D2)"
    );

    let wrong_nonce = pattern::<NONCE_LEN>(0xa0);
    assert_eq!(
        open_content(SUITE_V1, &room_key(), &wrong_nonce, &sealed, CONTENT_AAD),
        Err(CryptoError::AeadOpenFailure)
    );

    let wrong_key = RoomKey::from_bytes(pattern::<32>(0x99));
    assert_eq!(
        open_content(SUITE_V1, &wrong_key, &nonce, &sealed, CONTENT_AAD),
        Err(CryptoError::AeadOpenFailure),
        "a node without the epoch key must see unreadable, not a panic"
    );

    assert_eq!(
        open_content(
            SUITE_V1,
            &room_key(),
            &nonce,
            &sealed[..TAG_LEN],
            CONTENT_AAD
        ),
        Err(CryptoError::AeadOpenFailure),
        "truncated ciphertext must fail authentication"
    );
    assert_eq!(
        open_content(SUITE_V1, &room_key(), &nonce, b"", CONTENT_AAD),
        Err(CryptoError::AeadOpenFailure),
        "empty ciphertext (shorter than the tag) must be a typed error"
    );
}

/// Ed25519→X25519 conversion rejects non-points and small-order (weak)
/// points, and the wrap path refuses to wrap to them.
#[test]
fn conversion_rejects_invalid_and_weak_points() {
    let invalid = hex_array::<32>(GOLDEN_INVALID_ED_POINT);
    assert_eq!(
        ed25519_public_to_x25519(&invalid),
        Err(CryptoError::InvalidEd25519PublicKey)
    );

    // The Edwards identity point (y = 1) is valid but small-order.
    let mut identity = [0u8; 32];
    identity[0] = 0x01;
    assert_eq!(
        ed25519_public_to_x25519(&identity),
        Err(CryptoError::WeakEd25519PublicKey)
    );

    let room_id = pattern::<32>(ROOM_ID_START);
    assert_eq!(
        wrap_room_key(&room_key(), &invalid, &room_id, KEY_EPOCH),
        Err(CryptoError::InvalidEd25519PublicKey)
    );
    assert_eq!(
        wrap_room_key(&room_key(), &identity, &room_id, KEY_EPOCH),
        Err(CryptoError::WeakEd25519PublicKey)
    );
}

/// Secret types never leak their bytes through `Debug`.
#[test]
fn secret_debug_output_is_redacted() {
    let key = room_key();
    assert_eq!(format!("{key:?}"), "RoomKey(<redacted>)");
    let eph = EphemeralSecret::from_bytes(pattern::<32>(EPHEMERAL_SECRET_START));
    assert_eq!(format!("{eph:?}"), "EphemeralSecret(<redacted>)");
}
