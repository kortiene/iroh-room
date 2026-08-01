//! R2 encrypted-content write-path conformance (#191 step 4, spec
//! `content-key-rotation.md` D2/D2a/D2b/D8 and the §5 golden-vector
//! discipline).
//!
//! What step 4 must prove:
//!
//! * the **normative D2 AAD** is pinned byte-for-byte against an independent
//!   Python/OpenSSL reference (`tests/golden/encrypted_aad_reference.py` —
//!   derived FIRST, Rust must match), along with the sealed ciphertext, the
//!   full CSB, the `event_id`, **and the Ed25519 signature** (spec §5);
//! * the AAD **binds**: transplanting the golden ciphertext onto any mutated
//!   signed prefix (room, sender, device, `created_at`, parents, inner type,
//!   epoch) or touching nonce/ciphertext fails the AEAD open, while the
//!   mutated event itself stays envelope-valid — unreadable, never invalid;
//! * the **floor gate** is two-sided and local-only: floor off refuses
//!   encrypted publishes (the step-3 posture, still covered by
//!   `encrypted_envelope.rs`), floor on accepts them and refuses *plaintext*
//!   content-class publishes instead, and membership events pass either way;
//! * **convergence is key-independent** (AC2/AC8): key holders, non-holders,
//!   and readers of a malicious inner body all reach identical digests; only
//!   key holders surface a body, and a malicious body surfaces nowhere;
//! * the engine's epoch key store **fails closed on conflict** (D5a): a
//!   conflicting key poisons the epoch — neither key is adopted.
//!
//! The golden cast shares the step-3 fixtures (`encrypted_envelope.rs`) —
//! identity/device seeds, room, timestamps, parent, `key_epoch` 7, room key
//! `00..1f`, nonce `a0..ab`, body "Hello room" — so this vector differs from
//! the step-3 placeholder-AAD pin **only** in the AAD and therefore the tag
//! bytes of the ciphertext. The step-3 pins are retained unchanged: they
//! freeze the envelope *encoding*; these freeze the *write path*.

use iroh_rooms_core::event::cbor;
use iroh_rooms_core::event::constants::{ENCRYPTED_NONCE_LEN, ENCRYPTED_SUITE_V1};
use iroh_rooms_core::event::content::{Content, EventType, MessageText};
use iroh_rooms_core::event::encrypted::{
    aad_for, build_content_encrypted, encrypted_content_aad, open_encrypted_content,
    UnreadableReason,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_crypto::RoomKey;

// --- Fixtures: the shared step-3 golden cast. --------------------------------

const ROOM_NONCE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];
const ROOM_CREATED_AT: u64 = 1_750_000_000_000;
const ENCRYPTED_CREATED_AT: u64 = 1_750_000_006_000;
const PARENT_ID: [u8; 32] = [0x11; 32];
const KEY_EPOCH: u64 = 7;
const GOLDEN_NONCE: [u8; ENCRYPTED_NONCE_LEN] = [
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
];

// --- Golden pins, derived FIRST by `tests/golden/encrypted_aad_reference.py`
// (an independent Python/OpenSSL/blake3 stack). Recompute & update only on an
// intentional consensus-byte change (such a change is breaking). -------------

const GOLDEN_ROOM_ID_HEX: &str = "43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3";
const GOLDEN_AAD_HEX: &str = "69726f682d726f6f6d733a636f6e74656e742d6161643a76318a01582043c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a358208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c58208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b39471636f6e74656e742e656e637279707465641b000001977420f37081582011111111111111111111111111111111111111111111111111111111111111116c6d6573736167652e746578740701";
const GOLDEN_PLAINTEXT_HEX: &str = "a264626f64796a48656c6c6f20726f6f6d66666f726d617465706c61696e";
const GOLDEN_CIPHERTEXT_HEX: &str =
    "447c1e4221b268f70709ebbc2708afb11dca3f7fe0da2318f97e4ae716c5b5ba71b0a897556b53c408243fe40879";
const GOLDEN_R2_CSB_HEX: &str = "a867636f6e74656e74a5656e6f6e63654ca0a1a2a3a4a5a6a7a8a9aaab65737569746501696b65795f65706f6368076a63697068657274657874582e447c1e4221b268f70709ebbc2708afb11dca3f7fe0da2318f97e4ae716c5b5ba71b0a897556b53c408243fe408796a696e6e65725f747970656c6d6573736167652e7465787467726f6f6d5f6964582043c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3696465766963655f696458208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b3946973656e6465725f696458208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c6a637265617465645f61741b000001977420f3706a6576656e745f7479706571636f6e74656e742e656e637279707465646b707265765f6576656e747381582011111111111111111111111111111111111111111111111111111111111111116e736368656d615f76657273696f6e01";
const GOLDEN_R2_EVENT_ID_HEX: &str =
    "37a69fbe56a101a1722096145fccbe4a786975d84276d516db4a37be549da7ab";
const GOLDEN_R2_SIGNATURE_HEX: &str = "0ac53823a9c15bdbf9c409c13a21a12c54eceb31230dc8fbf8b3caa08ebd28fd378900f52d7ad72339c9f77c9fd63a1d07f25a1fe9f815c8634627461ca96d0c";

fn alice_identity_secret() -> SigningKey {
    SigningKey::from_seed(&[0x01; 32])
}

fn alice_device_secret() -> SigningKey {
    SigningKey::from_seed(&[0x02; 32])
}

fn alice_identity() -> IdentityKey {
    alice_identity_secret().identity_key()
}

fn room_a() -> RoomId {
    signed::derive_room_id(&alice_identity(), &ROOM_NONCE, ROOM_CREATED_AT)
}

fn room_key() -> RoomKey {
    RoomKey::from_bytes(core::array::from_fn(|i| u8::try_from(i).expect("i < 32")))
}

fn golden_inner() -> Content {
    Content::MessageText(MessageText {
        body: "Hello room".to_owned(),
        format: Some("plain".to_owned()),
        in_reply_to: None,
        mentions: None,
    })
}

/// The golden R2 event, produced by the real writer.
fn golden_wire() -> WireEvent {
    build_content_encrypted(
        &alice_identity_secret(),
        &alice_device_secret(),
        &room_a(),
        &golden_inner(),
        KEY_EPOCH,
        &room_key(),
        &GOLDEN_NONCE,
        &[EventId::from_bytes(PARENT_ID)],
        ENCRYPTED_CREATED_AT,
    )
    .expect("the golden cast seals")
}

fn golden_event() -> SignedEvent {
    SignedEvent::decode(&golden_wire().signed).expect("golden event decodes")
}

// --- Golden vectors (spec §5: pin AAD, ciphertext, CSB, id, signature). ------

#[test]
fn golden_room_id_matches_reference() {
    assert_eq!(hex::encode(room_a().as_bytes()), GOLDEN_ROOM_ID_HEX);
}

#[test]
fn golden_aad_matches_reference() {
    let aad = encrypted_content_aad(
        1,
        &room_a(),
        &alice_identity(),
        &alice_device_secret().device_key(),
        ENCRYPTED_CREATED_AT,
        &[EventId::from_bytes(PARENT_ID)],
        EventType::MessageText,
        KEY_EPOCH,
        ENCRYPTED_SUITE_V1,
    );
    assert_eq!(
        hex::encode(&aad),
        GOLDEN_AAD_HEX,
        "the normative D2 AAD is frozen (OQ-2); an intentional change must \
         update the Python reference and this vector together"
    );
    // `aad_for` reads the identical bytes back out of the assembled event.
    assert_eq!(
        aad_for(&golden_event()).expect("encrypted event"),
        aad,
        "aad_for must rebuild the exact seal-time AAD from the event alone"
    );
}

#[test]
fn golden_write_path_matches_reference() {
    // The sealed plaintext is the inner body's canonical CBOR.
    assert_eq!(
        hex::encode(cbor::encode(&golden_inner().to_cbor())),
        GOLDEN_PLAINTEXT_HEX
    );

    let wire = golden_wire();
    assert_eq!(
        hex::encode(&wire.signed),
        GOLDEN_R2_CSB_HEX,
        "CSB is frozen"
    );
    assert_eq!(
        hex::encode(wire.sig.as_bytes()),
        GOLDEN_R2_SIGNATURE_HEX,
        "the Ed25519 signature is deterministic (RFC 8032) and frozen"
    );

    let event = golden_event();
    assert_eq!(
        hex::encode(event.event_id().as_bytes()),
        GOLDEN_R2_EVENT_ID_HEX
    );
    let Content::Encrypted(env) = &event.content else {
        panic!("golden event must carry the encrypted envelope");
    };
    assert_eq!(hex::encode(&env.ciphertext), GOLDEN_CIPHERTEXT_HEX);

    // End-to-end: the golden event passes the full stateless pipeline and
    // opens back to the golden body under the golden key.
    validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_a()))
        .expect("the golden event validates");
    assert_eq!(
        open_encrypted_content(&event, &room_key()).expect("opens"),
        golden_inner()
    );
}

#[test]
fn golden_ciphertext_differs_from_step3_only_in_the_tag() {
    // Same key, nonce, and plaintext as the step-3 placeholder-AAD golden
    // vector — AES-GCM's keystream is AAD-independent, so the body bytes
    // must be identical and ONLY the 16 tag bytes may differ. This pins the
    // relationship between the two vectors (and would catch an accidental
    // key/nonce/plaintext drift between the files).
    const STEP3_CIPHERTEXT_HEX: &str = "447c1e4221b268f70709ebbc2708afb11dca3f7fe0da2318f97e4ae716c5b9dfe9d274b170fea7fc9a6ecf99e59a";
    let step3 = STEP3_CIPHERTEXT_HEX;
    let step4 = GOLDEN_CIPHERTEXT_HEX;
    assert_eq!(step3.len(), step4.len());
    let body_hex = step4.len() - 32; // last 16 bytes (32 hex chars) are the tag
    assert_eq!(
        step3[..body_hex],
        step4[..body_hex],
        "identical (key, nonce, plaintext) must give identical GCM body bytes"
    );
    assert_ne!(
        step3[body_hex..],
        step4[body_hex..],
        "the normative AAD must change the tag"
    );
}

// --- AAD binding: the transplant matrix. -------------------------------------

/// Re-sign a mutated copy of the golden event with `device_secret` and assert
/// (a) the AEAD refuses the golden ciphertext under the mutated prefix, and
/// (b) the mutated event is still envelope-valid in `room` — the verdict is
/// key-independent; a transplant is unreadable, never invalid.
fn assert_transplant_unreadable(
    mutate: impl FnOnce(&mut SignedEvent),
    device_secret: &SigningKey,
    room: RoomId,
) {
    let mut event = golden_event();
    mutate(&mut event);
    assert_eq!(
        open_encrypted_content(&event, &room_key()).expect_err("must not open"),
        UnreadableReason::AeadFailure,
        "a transplanted prefix must fail the AEAD, not any later stage"
    );
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, device_secret);
    validate_wire_bytes(
        &WireEvent::seal(csb, sig).to_bytes(),
        &ValidationContext::for_room(room),
    )
    .expect("a transplanted envelope still validates (key-independent verdict)");
}

#[test]
fn transplanted_created_at_fails_the_aead() {
    assert_transplant_unreadable(|ev| ev.created_at += 1, &alice_device_secret(), room_a());
}

#[test]
fn transplanted_room_fails_the_aead() {
    let other_room = RoomId::from_bytes([0xFF; 32]);
    assert_transplant_unreadable(
        move |ev| ev.room_id = other_room,
        &alice_device_secret(),
        other_room,
    );
}

#[test]
fn transplanted_sender_fails_the_aead() {
    let mallory = SigningKey::from_seed(&[0x66; 32]).identity_key();
    assert_transplant_unreadable(
        move |ev| ev.sender_id = mallory,
        &alice_device_secret(),
        room_a(),
    );
}

#[test]
fn transplanted_device_fails_the_aead() {
    let other_device = SigningKey::from_seed(&[0x67; 32]);
    let device_id = other_device.device_key();
    assert_transplant_unreadable(move |ev| ev.device_id = device_id, &other_device, room_a());
}

#[test]
fn transplanted_parents_fail_the_aead() {
    assert_transplant_unreadable(
        |ev| ev.prev_events = vec![EventId::from_bytes([0x12; 32])],
        &alice_device_secret(),
        room_a(),
    );
}

#[test]
fn transplanted_inner_type_fails_the_aead() {
    // Relabeling the sealed body as another registered content type must die
    // at the AEAD (the AAD binds inner_type) — long before any parse could be
    // confused by it.
    assert_transplant_unreadable(
        |ev| {
            let Content::Encrypted(env) = &mut ev.content else {
                panic!("golden event is encrypted");
            };
            env.inner_type = EventType::AgentStatus;
        },
        &alice_device_secret(),
        room_a(),
    );
}

#[test]
fn transplanted_epoch_fails_the_aead() {
    assert_transplant_unreadable(
        |ev| {
            let Content::Encrypted(env) = &mut ev.content else {
                panic!("golden event is encrypted");
            };
            env.key_epoch = KEY_EPOCH + 1;
        },
        &alice_device_secret(),
        room_a(),
    );
}

#[test]
fn flipped_nonce_fails_the_aead() {
    // The nonce is deliberately not in the AAD: it is an authenticated
    // AES-GCM cipher input already, and this proves it.
    let mut event = golden_event();
    let Content::Encrypted(env) = &mut event.content else {
        panic!("golden event is encrypted");
    };
    env.nonce[0] ^= 0x01;
    assert_eq!(
        open_encrypted_content(&event, &room_key()).expect_err("must not open"),
        UnreadableReason::AeadFailure
    );
}

#[test]
fn tampered_ciphertext_fails_the_aead() {
    for flip_at in [0, 45] {
        // First body byte and last tag byte.
        let mut event = golden_event();
        let Content::Encrypted(env) = &mut event.content else {
            panic!("golden event is encrypted");
        };
        env.ciphertext[flip_at] ^= 0x01;
        assert_eq!(
            open_encrypted_content(&event, &room_key()).expect_err("must not open"),
            UnreadableReason::AeadFailure,
            "flipping ciphertext byte {flip_at} must fail authentication"
        );
    }
}

#[test]
fn foreign_suite_is_refused_before_any_cryptography() {
    // Unreachable from the wire (the envelope parser pins suite == 0x01);
    // covered as defense-in-depth for direct callers of the open path.
    let mut event = golden_event();
    let Content::Encrypted(env) = &mut event.content else {
        panic!("golden event is encrypted");
    };
    env.suite = 0x02;
    assert_eq!(
        open_encrypted_content(&event, &room_key()).expect_err("must not open"),
        UnreadableReason::UnsupportedSuite
    );
}

// --- Engine behavior: the R2 floor gate, key store, and convergence. ---------

#[cfg(all(feature = "sync", feature = "store"))]
mod engine {
    use super::*;
    use iroh_rooms_core::event::binding::DeviceBinding;
    use iroh_rooms_core::event::constants::MAX_MESSAGE_BODY_BYTES;
    use iroh_rooms_core::event::content::RoomCreated;
    use iroh_rooms_core::store::EventStore;
    use iroh_rooms_core::sync::sim::SimNet;
    use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine, SyncError};

    const WRITER: PeerId = PeerId::from_bytes([0xA1; 32]);
    const READER_WITH_KEY: PeerId = PeerId::from_bytes([0xB2; 32]);
    const READER_NO_KEY: PeerId = PeerId::from_bytes([0xC3; 32]);

    fn engine_with(config: SyncConfig) -> SyncEngine {
        let store = EventStore::open_in_memory().expect("in-memory store");
        SyncEngine::open(store, room_a(), config).expect("open engine")
    }

    fn floor_on() -> SyncConfig {
        SyncConfig {
            encrypted_content_writes: true,
            ..SyncConfig::default()
        }
    }

    fn genesis_event() -> SignedEvent {
        SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: alice_identity(),
            device_id: alice_device_secret().device_key(),
            event_type: EventType::RoomCreated,
            created_at: ROOM_CREATED_AT,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "R2 Write Path".to_owned(),
                room_nonce: ROOM_NONCE,
                admins: vec![alice_identity()],
                device_binding: DeviceBinding::create(
                    &room_a(),
                    &alice_identity_secret(),
                    alice_device_secret().device_key(),
                ),
            }),
        }
    }

    fn seal(ev: &SignedEvent) -> Vec<u8> {
        let csb = ev.to_csb();
        let sig = signed::sign_csb(&csb, &alice_device_secret());
        WireEvent::seal(csb, sig).to_bytes()
    }

    /// Build an encrypted event parented on the live genesis via the real
    /// writer (unlike the golden serialization fixture, DAG-valid here).
    fn live_encrypted(inner: &Content, created_at: u64) -> WireEvent {
        build_content_encrypted(
            &alice_identity_secret(),
            &alice_device_secret(),
            &room_a(),
            inner,
            KEY_EPOCH,
            &room_key(),
            &GOLDEN_NONCE,
            &[genesis_event().event_id()],
            created_at,
        )
        .expect("live seal succeeds")
    }

    /// The floor gate is two-sided: with `encrypted_content_writes` on, the
    /// engine accepts a locally-authored envelope, refuses a locally-authored
    /// plaintext content-class event, and keeps accepting membership events
    /// (D1 — those must stay plaintext).
    #[test]
    fn floor_on_gates_plaintext_and_admits_encrypted() {
        let mut engine = engine_with(floor_on());
        engine
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("fresh key");

        // Membership (genesis) publishes regardless of the floor.
        engine
            .publish(&seal(&genesis_event()))
            .expect("membership events are never encrypted (D1)");

        // A plaintext message is refused fail-closed.
        let plaintext_msg = SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: alice_identity(),
            device_id: alice_device_secret().device_key(),
            event_type: EventType::MessageText,
            created_at: ENCRYPTED_CREATED_AT,
            prev_events: vec![genesis_event().event_id()],
            content: golden_inner(),
        };
        let refused_id = plaintext_msg.event_id();
        let err = engine
            .publish(&seal(&plaintext_msg))
            .expect_err("plaintext content must be refused after the opt-in");
        assert!(
            matches!(err, SyncError::PlaintextWritesDisabled),
            "expected the plaintext-writes-disabled error, got: {err}"
        );
        assert!(
            !engine
                .digest()
                .expect("digest")
                .event_ids
                .contains(&refused_id),
            "a refused plaintext event must not be persisted"
        );

        // The encrypted equivalent publishes, sealed with the engine-held key.
        let epoch = KEY_EPOCH;
        let key = engine.room_key(epoch).expect("held key");
        let wire = build_content_encrypted(
            &alice_identity_secret(),
            &alice_device_secret(),
            &room_a(),
            &golden_inner(),
            epoch,
            key,
            &GOLDEN_NONCE,
            &[genesis_event().event_id()],
            ENCRYPTED_CREATED_AT,
        )
        .expect("seals with the engine-held key");
        let encrypted_id = signed::event_id_from_bytes(&wire.signed);
        engine
            .publish(&wire.to_bytes())
            .expect("the floor admits locally-authored encrypted content");
        assert!(
            engine
                .digest()
                .expect("digest")
                .event_ids
                .contains(&encrypted_id),
            "the encrypted event must be persisted"
        );
    }

    /// With the floor off (the default), the encrypted publish stays refused —
    /// the step-3 posture is unchanged for undeclared rooms.
    #[test]
    fn floor_off_still_refuses_encrypted_publish() {
        let mut engine = engine_with(SyncConfig::default());
        engine
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");
        let err = engine
            .publish(&live_encrypted(&golden_inner(), ENCRYPTED_CREATED_AT).to_bytes())
            .expect_err("floor off must refuse the envelope");
        assert!(matches!(err, SyncError::EncryptedWritesDisabled));
    }

    /// D5a fail-closed: a conflicting key poisons the epoch — the engine
    /// adopts *neither* key, reads become typed-unreadable, and further
    /// offers stay refused. Never arrival-order-dependent readability.
    #[test]
    fn conflicting_epoch_key_poisons_the_epoch() {
        let mut engine = engine_with(SyncConfig::default());
        engine
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("fresh key");
        assert!(engine.has_room_key(KEY_EPOCH));

        let err = engine
            .insert_room_key(KEY_EPOCH, RoomKey::from_bytes([0xBB; 32]))
            .expect_err("a conflicting key must be refused");
        assert!(
            matches!(err, SyncError::EpochKeyConflict { epoch } if epoch == KEY_EPOCH),
            "expected the epoch-key-conflict error, got: {err}"
        );
        assert!(
            !engine.has_room_key(KEY_EPOCH),
            "after a conflict the epoch must hold NEITHER key (D5a)"
        );
        engine
            .insert_room_key(KEY_EPOCH, room_key())
            .expect_err("a poisoned epoch refuses even the original key");

        assert_eq!(
            engine
                .read_content(&golden_event())
                .expect_err("unreadable"),
            UnreadableReason::EpochConflicted { epoch: KEY_EPOCH }
        );
    }

    /// AC1/AC2/AC8 mechanics: a writer, a key-holding reader, and a keyless
    /// reader converge to identical digests; the body surfaces only where the
    /// key is held; the keyless node reports the typed no-key reason.
    #[test]
    fn convergence_is_key_independent_and_surfacing_is_not() {
        let mut net = SimNet::new(room_a());
        net.add_peer(WRITER, engine_with(floor_on()));
        net.add_peer(READER_WITH_KEY, engine_with(SyncConfig::default()));
        net.add_peer(READER_NO_KEY, engine_with(SyncConfig::default()));

        net.engine_mut(WRITER)
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("writer key");
        net.engine_mut(READER_WITH_KEY)
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("reader key");

        let encrypted = live_encrypted(&golden_inner(), ENCRYPTED_CREATED_AT);
        let encrypted_id = signed::event_id_from_bytes(&encrypted.signed);
        net.engine_mut(WRITER)
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");
        net.engine_mut(WRITER)
            .publish(&encrypted.to_bytes())
            .expect("the floor admits the encrypted publish");

        net.connect_all();
        net.run_to_quiescence();
        net.assert_converged(&[WRITER, READER_WITH_KEY, READER_NO_KEY]);

        for node in [WRITER, READER_WITH_KEY, READER_NO_KEY] {
            assert!(
                net.engine(node)
                    .digest()
                    .expect("digest")
                    .event_ids
                    .contains(&encrypted_id),
                "{node:?} must hold the encrypted event"
            );
        }

        let stored_event = |net: &SimNet, node| {
            let tail = net.engine(node).room_tail(16).expect("tail");
            let se = tail
                .iter()
                .find(|se| se.event_type == EventType::ContentEncrypted)
                .expect("the encrypted event is in the tail");
            SignedEvent::decode(&se.wire.signed).expect("decodes")
        };

        for node in [WRITER, READER_WITH_KEY] {
            let ev = stored_event(&net, node);
            assert_eq!(
                net.engine(node).readable_content(&ev).expect("readable"),
                golden_inner(),
                "{node:?} holds the key and must surface the body"
            );
        }
        let ev = stored_event(&net, READER_NO_KEY);
        assert_eq!(
            net.engine(READER_NO_KEY)
                .read_content(&ev)
                .expect_err("keyless"),
            UnreadableReason::NoEpochKey { epoch: KEY_EPOCH },
            "the keyless reader reports the typed no-key reason"
        );
    }

    /// AC8: a malicious Active key holder seals a strictly-invalid inner body
    /// (over the §7 message cap, inside the D2a envelope cap). Every node —
    /// keyless or key-holding — accepts and persists the envelope with the
    /// same digest, and NO node surfaces the body: key holders fail the D2b
    /// strict parse, keyless nodes lack the key. Never a fold wedge, never a
    /// per-type validation bypass.
    #[test]
    fn malicious_inner_body_converges_and_surfaces_nowhere() {
        let oversized = Content::MessageText(MessageText {
            body: "a".repeat(MAX_MESSAGE_BODY_BYTES + 1),
            format: None,
            in_reply_to: None,
            mentions: None,
        });

        let mut net = SimNet::new(room_a());
        net.add_peer(WRITER, engine_with(floor_on()));
        net.add_peer(READER_WITH_KEY, engine_with(SyncConfig::default()));
        net.add_peer(READER_NO_KEY, engine_with(SyncConfig::default()));
        net.engine_mut(WRITER)
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("writer key");
        net.engine_mut(READER_WITH_KEY)
            .insert_room_key(KEY_EPOCH, room_key())
            .expect("reader key");

        let malicious = live_encrypted(&oversized, ENCRYPTED_CREATED_AT + 1);
        let malicious_id = signed::event_id_from_bytes(&malicious.signed);
        net.engine_mut(WRITER)
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");
        net.engine_mut(WRITER)
            .publish(&malicious.to_bytes())
            .expect("the envelope verdict is cleartext-only; it publishes");

        net.connect_all();
        net.run_to_quiescence();
        net.assert_converged(&[WRITER, READER_WITH_KEY, READER_NO_KEY]);

        for node in [WRITER, READER_WITH_KEY, READER_NO_KEY] {
            let engine = net.engine(node);
            assert!(
                engine
                    .digest()
                    .expect("digest")
                    .event_ids
                    .contains(&malicious_id),
                "{node:?} must accept and persist the envelope (AC8)"
            );
            let tail = engine.room_tail(16).expect("tail");
            let se = tail
                .iter()
                .find(|se| se.event_type == EventType::ContentEncrypted)
                .expect("envelope in tail");
            let ev = SignedEvent::decode(&se.wire.signed).expect("decodes");
            assert!(
                engine.readable_content(&ev).is_none(),
                "{node:?} must not surface the malicious body"
            );
        }

        // The two failure modes are distinguishable to the logging caller.
        let ev = |node| {
            let tail = net.engine(node).room_tail(16).expect("tail");
            let se = tail
                .iter()
                .find(|se| se.event_type == EventType::ContentEncrypted)
                .expect("envelope in tail")
                .clone();
            SignedEvent::decode(&se.wire.signed).expect("decodes")
        };
        assert_eq!(
            net.engine(READER_WITH_KEY)
                .read_content(&ev(READER_WITH_KEY))
                .expect_err("unreadable"),
            UnreadableReason::InnerInvalid,
            "a key holder fails the D2b strict parse"
        );
        assert_eq!(
            net.engine(READER_NO_KEY)
                .read_content(&ev(READER_NO_KEY))
                .expect_err("unreadable"),
            UnreadableReason::NoEpochKey { epoch: KEY_EPOCH },
            "a keyless node fails at the key store"
        );
    }
}
