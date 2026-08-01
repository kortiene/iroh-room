//! R2 encrypted-content write path (#191 step 4, spec `content-key-rotation.md`
//! D2/D2a/D2b/D3): the normative content AAD, byte-exact `content.encrypted`
//! assembly, and authenticated decrypt-on-read with the D2b strict
//! post-decryption parse.
//!
//! # The normative D2 AAD (consensus-critical)
//!
//! The AAD binds a sealed body to its exact event, room, type, epoch, and
//! suite so ciphertext cannot be transplanted (spec D2). Its byte layout is
//! pinned here (resolving spec §10 OQ-2) and frozen by the golden vectors in
//! `tests/encrypted_write_path.rs`:
//!
//! ```text
//! aad = ENCRYPTED_AAD_CONTEXT ‖ canonical-CBOR array [
//!     uint(schema_version), bytes32(room_id), bytes32(sender_id),
//!     bytes32(device_id), text(event_type), uint(created_at),
//!     array[bytes32(prev_event)…], text(inner_type), uint(key_epoch),
//!     uint(suite),
//! ]
//! ```
//!
//! A CBOR **array** (not the repo's canonical map, which would re-sort keys)
//! preserves the spec D2 field order, is self-delimiting for the
//! variable-length `prev_events`, and reuses the machine-checked canonical
//! encoder. The nonce is deliberately absent: it is an authenticated AES-GCM
//! cipher input already, so a flipped nonce fails the tag on its own. The D5
//! epoch key commitment is deliberately absent too: GCM's GHASH is not
//! key-committing regardless of AAD contents, and honest nodes cannot hold
//! divergent same-epoch keys once the D5a conflict rule fail-closes (the
//! commitment is verified at key *adoption*, §7 step 6). Note `key_epoch`
//! here is a CBOR uint, while the *wrap* KDF/AAD of `iroh-rooms-crypto` pins
//! 8-byte big-endian — two frozen conventions; do not "harmonize" them.
//!
//! # D2b — decryption never touches the DAG verdict
//!
//! [`open_encrypted_content`] is a **surfacing** helper: its failures mean
//! *unreadable*, never *invalid*. The accept/park/reject verdict for an
//! envelope is computed from cleartext alone (the strict envelope parser and
//! the fold); a node that cannot open a body drops it from display and the
//! key-aware authorization projections (§7 step 5) and nothing else. Callers
//! own the spec §5 "dropped, logged" obligation via the typed
//! [`UnreadableReason`].
//!
//! Like the sibling builders (`build_message_text`, …), the builder here is
//! deterministic in its inputs: the caller injects `prev_events`, `created_at`
//! **and the nonce**, so core stays clock- and RNG-free (production callers
//! draw the nonce from `iroh_rooms_crypto::generate_nonce()`; uniqueness per
//! `(key_epoch, event)` is their contract).
//!
//! Zeroization boundary: the encoded-plaintext buffers on both the seal and
//! open side are wiped on drop. The decoded `CborValue` tree and the returned
//! [`Content`] are deliberately outside that boundary — they *are* the
//! surfaced cleartext the caller asked for; the T28 secret is the epoch key
//! store, not this best-effort hygiene.

use iroh_rooms_crypto::{open_content, seal_content, CryptoError, RoomKey};
use zeroize::Zeroizing;

use super::cbor::{self, CborValue};
use super::constants::{ENCRYPTED_AAD_CONTEXT, ENCRYPTED_NONCE_LEN, ENCRYPTED_SUITE_V1};
use super::content::{check_field_rules, Content, EncryptedContent, EventType};
use super::ids::{EventId, RoomId};
use super::keys::{DeviceKey, IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Why a `content.encrypted` body could not be surfaced (spec D2b/§5).
///
/// Observability-only: a reason is never a DAG-verdict input, never travels to
/// peers, and never panics on peer bytes. Callers of
/// [`open_encrypted_content`] / the engine read surface own the spec §5
/// "dropped, logged" obligation with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnreadableReason {
    /// The event's content is not an encrypted envelope.
    NotEncrypted,
    /// The envelope names a suite this build does not implement. Fail-closed
    /// defense-in-depth only: the strict envelope parser already rejects any
    /// suite other than [`ENCRYPTED_SUITE_V1`] at validation, so a
    /// store-resident event can never reach this arm.
    UnsupportedSuite,
    /// AEAD authentication failed: wrong key, tampered ciphertext, or an AAD
    /// mismatch (a transplanted envelope).
    AeadFailure,
    /// The AEAD opened but the recovered bytes are not one canonical CBOR item.
    MalformedPlaintext,
    /// The recovered body failed the strict `Content::parse(inner_type)` or
    /// the sender-binding field rules — a malicious Active key holder's
    /// payload (spec D2b); the *envelope* verdict is unaffected.
    InnerInvalid,
    /// No key is held for the envelope's epoch (engine-constructed; never
    /// returned by [`open_encrypted_content`], which takes the key).
    NoEpochKey {
        /// The epoch the envelope names.
        epoch: u64,
    },
    /// Conflicting keys were offered for the envelope's epoch, so the epoch is
    /// poisoned and adopts neither (spec D5a fail-closed; engine-constructed).
    EpochConflicted {
        /// The conflicted epoch.
        epoch: u64,
    },
}

impl UnreadableReason {
    /// Stable lowercase code for logs/audit surfaces (mirrors
    /// `RejectReason::code`).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotEncrypted => "not_encrypted",
            Self::UnsupportedSuite => "unsupported_suite",
            Self::AeadFailure => "aead_failure",
            Self::MalformedPlaintext => "malformed_plaintext",
            Self::InnerInvalid => "inner_invalid",
            Self::NoEpochKey { .. } => "no_epoch_key",
            Self::EpochConflicted { .. } => "epoch_conflicted",
        }
    }
}

impl core::fmt::Display for UnreadableReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoEpochKey { epoch } | Self::EpochConflicted { epoch } => {
                write!(f, "{} (epoch {epoch})", self.code())
            }
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for UnreadableReason {}

/// Why [`build_content_encrypted`] refused to assemble an event. A local
/// authoring error (friendly, pre-IO) — never a wire-validation verdict
/// (`RejectReason` is reserved for those).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SealError {
    /// The inner body is not one of the five encryptable content-class types
    /// (membership bodies stay plaintext per D1; envelopes do not nest).
    NotEncryptable(EventType),
    /// The canonical-CBOR plaintext exceeds the inner type's D2a cap.
    PlaintextTooLarge {
        /// Encoded plaintext length.
        len: usize,
        /// The inner type's `max_encrypted_plaintext_bytes` cap.
        cap: usize,
    },
    /// The AEAD refused to seal (unreachable behind the D2a cap; kept typed).
    SealFailure,
}

impl core::fmt::Display for SealError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotEncryptable(t) => {
                write!(f, "seal_not_encryptable: {}", t.as_str())
            }
            Self::PlaintextTooLarge { len, cap } => {
                write!(f, "seal_plaintext_too_large: {len} > {cap}")
            }
            Self::SealFailure => f.write_str("seal_failure"),
        }
    }
}

impl std::error::Error for SealError {}

/// Build the normative D2 AAD from its ten cleartext signed-prefix fields
/// (module docs pin the byte layout; the golden vectors freeze it).
///
/// Both sides of the AEAD call this: the builder with the fields it is about
/// to sign, the reader via [`aad_for`] with the fields the envelope carries.
#[must_use]
#[allow(clippy::too_many_arguments)] // one argument per bound AAD field, mirroring the builders
pub fn encrypted_content_aad(
    schema_version: u64,
    room_id: &RoomId,
    sender_id: &IdentityKey,
    device_id: &DeviceKey,
    created_at: u64,
    prev_events: &[EventId],
    inner_type: EventType,
    key_epoch: u64,
    suite: u8,
) -> Vec<u8> {
    let fields = CborValue::Array(vec![
        CborValue::Uint(schema_version),
        CborValue::Bytes(room_id.as_bytes().to_vec()),
        CborValue::Bytes(sender_id.as_bytes().to_vec()),
        CborValue::Bytes(device_id.as_bytes().to_vec()),
        CborValue::Text(EventType::ContentEncrypted.as_str().to_owned()),
        CborValue::Uint(created_at),
        CborValue::Array(
            prev_events
                .iter()
                .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                .collect(),
        ),
        CborValue::Text(inner_type.as_str().to_owned()),
        CborValue::Uint(key_epoch),
        CborValue::Uint(u64::from(suite)),
    ]);
    let mut aad = ENCRYPTED_AAD_CONTEXT.to_vec();
    aad.extend_from_slice(&cbor::encode(&fields));
    aad
}

/// The normative AAD for an already-assembled encrypted event, read entirely
/// from the event itself (no way to desynchronize envelope and prefix).
///
/// # Errors
/// [`UnreadableReason::NotEncrypted`] if the event's content is not an
/// encrypted envelope.
pub fn aad_for(event: &SignedEvent) -> Result<Vec<u8>, UnreadableReason> {
    let Content::Encrypted(env) = &event.content else {
        return Err(UnreadableReason::NotEncrypted);
    };
    Ok(encrypted_content_aad(
        event.schema_version,
        &event.room_id,
        &event.sender_id,
        &event.device_id,
        event.created_at,
        &event.prev_events,
        env.inner_type,
        env.key_epoch,
        env.suite,
    ))
}

/// Assemble and sign a `content.encrypted` event sealing `inner` under
/// `room_key[key_epoch]` (Event Protocol §7 + spec D2) — the R2 writer, the
/// sibling of [`build_message_text`](super::message::build_message_text).
///
/// Pure and deterministic: `prev_events`, `created_at`, and the `nonce` are
/// injected by the caller (see the module docs; nonce uniqueness per
/// `(key_epoch, event)` is the caller's contract). The signature verifies
/// under `device_id`; the sealed body is the canonical CBOR of `inner`,
/// bound to this exact signed prefix via the normative AAD.
///
/// # Errors
/// * [`SealError::NotEncryptable`] — `inner` is a membership body (D1) or an
///   envelope (no nesting).
/// * [`SealError::PlaintextTooLarge`] — the canonical plaintext exceeds the
///   inner type's D2a cap (the built envelope would fail strict parse).
/// * [`SealError::SealFailure`] — the AEAD refused (unreachable behind the cap).
#[allow(clippy::too_many_arguments)] // mirrors build_message_text; each arg is a distinct signed field
pub fn build_content_encrypted(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    inner: &Content,
    key_epoch: u64,
    room_key: &RoomKey,
    nonce: &[u8; ENCRYPTED_NONCE_LEN],
    prev_events: &[EventId],
    created_at: u64,
) -> Result<WireEvent, SealError> {
    let inner_type = inner.event_type();
    let Some(cap) = inner_type.max_encrypted_plaintext_bytes() else {
        return Err(SealError::NotEncryptable(inner_type));
    };
    let plaintext = Zeroizing::new(cbor::encode(&inner.to_cbor()));
    if plaintext.len() > cap {
        return Err(SealError::PlaintextTooLarge {
            len: plaintext.len(),
            cap,
        });
    }

    let sender_id = sender_identity_secret.identity_key();
    let device_id = sender_device_secret.device_key();
    let aad = encrypted_content_aad(
        super::constants::SCHEMA_VERSION,
        room_id,
        &sender_id,
        &device_id,
        created_at,
        prev_events,
        inner_type,
        key_epoch,
        ENCRYPTED_SUITE_V1,
    );
    let ciphertext =
        seal_content(room_key, nonce, &plaintext, &aad).map_err(|_| SealError::SealFailure)?;

    let event = SignedEvent {
        schema_version: super::constants::SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::ContentEncrypted,
        created_at,
        prev_events: prev_events.to_vec(),
        content: Content::Encrypted(EncryptedContent {
            inner_type,
            key_epoch,
            suite: ENCRYPTED_SUITE_V1,
            nonce: *nonce,
            ciphertext,
        }),
    };
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, sender_device_secret);
    Ok(WireEvent::seal(csb, sig))
}

/// Open an encrypted event's body with `room_key[env.key_epoch]` and run the
/// D2b post-decryption gate: strict `Content::parse(inner_type)` plus the
/// sender-binding field rules. Only a body that passes everything is returned
/// for surfacing / the key-aware authorization projections.
///
/// Every failure means *unreadable*, never *invalid*: the envelope's DAG
/// verdict is computed elsewhere from cleartext alone and is not this
/// function's concern. Never panics on peer bytes.
///
/// # Errors
/// [`UnreadableReason`] — see the variant docs; callers log the code (spec §5).
pub fn open_encrypted_content(
    event: &SignedEvent,
    room_key: &RoomKey,
) -> Result<Content, UnreadableReason> {
    let Content::Encrypted(env) = &event.content else {
        return Err(UnreadableReason::NotEncrypted);
    };
    let aad = aad_for(event)?;
    let plaintext = Zeroizing::new(
        open_content(env.suite, room_key, &env.nonce, &env.ciphertext, &aad).map_err(
            |e| match e {
                CryptoError::UnsupportedSuite(_) => UnreadableReason::UnsupportedSuite,
                _ => UnreadableReason::AeadFailure,
            },
        )?,
    );
    let value =
        cbor::decode_canonical(&plaintext).map_err(|_| UnreadableReason::MalformedPlaintext)?;
    let inner =
        Content::parse(env.inner_type, &value).map_err(|_| UnreadableReason::InnerInvalid)?;
    check_field_rules(&inner, &event.sender_id).map_err(|_| UnreadableReason::InnerInvalid)?;
    Ok(inner)
}

#[cfg(test)]
mod tests {
    use super::{build_content_encrypted, open_encrypted_content, SealError, UnreadableReason};
    use crate::event::constants::{ENCRYPTED_NONCE_LEN, MAX_MESSAGE_BODY_BYTES};
    use crate::event::content::{Content, MemberLeft, MessageText, PipeOpened};
    use crate::event::ids::EventId;
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_crypto::RoomKey;

    const IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const DEVICE_SEED: [u8; 32] = [0x02; 32];
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const ROOM_CREATED_AT: u64 = 1_750_000_000_000;
    const CREATED_AT: u64 = 1_750_000_006_000;
    const KEY_EPOCH: u64 = 7;
    const NONCE: [u8; ENCRYPTED_NONCE_LEN] = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
    ];

    fn keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&IDENTITY_SEED),
            SigningKey::from_seed(&DEVICE_SEED),
        )
    }

    fn room_id() -> crate::event::ids::RoomId {
        signed::derive_room_id(&keys().0.identity_key(), &ROOM_NONCE, ROOM_CREATED_AT)
    }

    fn room_key() -> RoomKey {
        RoomKey::from_bytes(core::array::from_fn(|i| u8::try_from(i).expect("i < 32")))
    }

    fn heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0x11; 32])]
    }

    fn message(body: &str) -> Content {
        Content::MessageText(MessageText {
            body: body.to_owned(),
            format: Some("plain".to_owned()),
            in_reply_to: None,
            mentions: None,
        })
    }

    fn build(inner: &Content) -> Result<crate::event::wire::WireEvent, SealError> {
        let (id, dev) = keys();
        build_content_encrypted(
            &id,
            &dev,
            &room_id(),
            inner,
            KEY_EPOCH,
            &room_key(),
            &NONCE,
            &heads(),
            CREATED_AT,
        )
    }

    #[test]
    fn builder_is_deterministic_and_validates() {
        let a = build(&message("Hello room")).expect("build");
        let b = build(&message("Hello room")).expect("build");
        assert_eq!(a.to_bytes(), b.to_bytes(), "same inputs, same bytes");
        validate_wire_bytes(&a.to_bytes(), &ValidationContext::for_room(room_id()))
            .expect("a freshly built envelope must pass stateless validation");
    }

    #[test]
    fn seal_open_round_trips_the_body() {
        let inner = message("Hello room");
        let wire = build(&inner).expect("build");
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let opened = open_encrypted_content(&event, &room_key()).expect("open");
        assert_eq!(opened, inner, "the opened body is the sealed body");
    }

    #[test]
    fn wrong_key_is_unreadable_not_invalid() {
        let wire = build(&message("Hello room")).expect("build");
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let wrong = RoomKey::from_bytes([0xEE; 32]);
        assert_eq!(
            open_encrypted_content(&event, &wrong).expect_err("must not open"),
            UnreadableReason::AeadFailure
        );
        // The envelope itself stays valid — the verdict is key-independent.
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id()))
            .expect("verdict must not depend on key possession");
    }

    #[test]
    fn membership_body_is_not_encryptable() {
        let inner = Content::MemberLeft(MemberLeft {
            member_id: keys().0.identity_key(),
            reason: None,
        });
        assert!(matches!(
            build(&inner).expect_err("membership must be refused"),
            SealError::NotEncryptable(t) if t.as_str() == "member.left"
        ));
    }

    #[test]
    fn over_cap_plaintext_is_refused_before_sealing() {
        // A body over the raw §7 cap but with total CBOR still under the D2a
        // envelope cap would seal; go clearly over the D2a cap instead.
        let inner = message(&"a".repeat(MAX_MESSAGE_BODY_BYTES + 8192));
        assert!(matches!(
            build(&inner).expect_err("over-cap plaintext must be refused"),
            SealError::PlaintextTooLarge { .. }
        ));
    }

    #[test]
    fn oversized_inner_body_opens_but_fails_strict_parse() {
        // Spec D2b / AC8: a malicious Active key holder seals a body over the
        // §7 message cap yet inside the D2a plaintext cap. The AEAD opens; the
        // strict inner parse must refuse; the envelope verdict is unchanged.
        let inner = message(&"a".repeat(MAX_MESSAGE_BODY_BYTES + 1));
        let wire = build(&inner).expect("within the D2a cap, sealing succeeds");
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id()))
            .expect("the envelope verdict is key-independent");
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("must not surface"),
            UnreadableReason::InnerInvalid
        );
    }

    #[test]
    fn foreign_owner_pipe_opened_is_unreadable() {
        // check_field_rules parity (D2b): an encrypted pipe.opened claiming a
        // foreign owner_id must not surface — the step-5 authorization reads
        // will trust what this function returns.
        let (id, _dev) = keys();
        let foreign = SigningKey::from_seed(&[0x77; 32]).identity_key();
        let inner = Content::PipeOpened(PipeOpened {
            pipe_id: [0x21; 16],
            owner_id: foreign,
            owner_endpoint: crate::event::keys::DeviceKey::from_bytes([0x22; 32]),
            kind: "tcp".to_owned(),
            label: "db".to_owned(),
            target_hint: "localhost:5432".to_owned(),
            alpn: "iroh-rooms/pipe/0".to_owned(),
            allowed_members: vec![id.identity_key()],
            expires_at: None,
        });
        let wire = build(&inner).expect("build");
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("must not surface"),
            UnreadableReason::InnerInvalid
        );
    }

    #[test]
    fn open_refuses_a_non_encrypted_event() {
        let (id, dev) = keys();
        let wire = crate::event::message::build_message_text(
            &id,
            &dev,
            &room_id(),
            "plaintext",
            None,
            None,
            &[],
            &heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("not an envelope"),
            UnreadableReason::NotEncrypted
        );
    }

    /// Seal arbitrary raw bytes (bypassing the builder) under the correct AAD
    /// for a hand-built envelope event — the cheapest payload a malicious
    /// Active key holder can produce.
    fn sealed_raw(plaintext: &[u8]) -> SignedEvent {
        let (id, dev) = keys();
        let sender_id = id.identity_key();
        let device_id = dev.device_key();
        let aad = super::encrypted_content_aad(
            crate::event::constants::SCHEMA_VERSION,
            &room_id(),
            &sender_id,
            &device_id,
            CREATED_AT,
            &heads(),
            crate::event::content::EventType::MessageText,
            KEY_EPOCH,
            crate::event::constants::ENCRYPTED_SUITE_V1,
        );
        let ciphertext = iroh_rooms_crypto::seal_content(&room_key(), &NONCE, plaintext, &aad)
            .expect("raw seal succeeds");
        SignedEvent {
            schema_version: crate::event::constants::SCHEMA_VERSION,
            room_id: room_id(),
            sender_id,
            device_id,
            event_type: crate::event::content::EventType::ContentEncrypted,
            created_at: CREATED_AT,
            prev_events: heads(),
            content: Content::Encrypted(crate::event::content::EncryptedContent {
                inner_type: crate::event::content::EventType::MessageText,
                key_epoch: KEY_EPOCH,
                suite: crate::event::constants::ENCRYPTED_SUITE_V1,
                nonce: NONCE,
                ciphertext,
            }),
        }
    }

    #[test]
    fn authentic_non_cbor_plaintext_is_malformed_not_a_panic() {
        // The AEAD opens (authentic key holder), but the recovered bytes are
        // not CBOR at all: the distinct `malformed_plaintext` code preserves
        // the audit split between "tampered/wrong key" and "authentic but
        // garbage from a key holder" (spec §5 "dropped, logged").
        let event = sealed_raw(&[0xFF]);
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("must not surface"),
            UnreadableReason::MalformedPlaintext
        );
    }

    #[test]
    fn authentic_trailing_garbage_plaintext_is_malformed() {
        // A valid CBOR item followed by trailing bytes is not ONE canonical
        // item; the strict decoder's full-consumption rule must refuse it.
        let mut plaintext = crate::event::cbor::encode(&message("Hello room").to_cbor());
        plaintext.push(0x00);
        let event = sealed_raw(&plaintext);
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("must not surface"),
            UnreadableReason::MalformedPlaintext
        );
    }
}
