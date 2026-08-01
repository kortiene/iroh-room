//! Phase R1 `content.encrypted` envelope conformance (#191 step 3, spec
//! `content-key-rotation.md` D2/D2a/D3/D8 and the §5 golden-vector
//! discipline).
//!
//! What R1 must prove:
//!
//! * the envelope parses **strictly** (exact key set, suite fail-closed, nonce
//!   length, D2a ciphertext bound, inner-type restrictions) with a
//!   key-independent verdict — every check here reads cleartext only;
//! * an encrypted event is **opaque-but-valid**: it validates, persists, and
//!   syncs like any content event (AC10 — no rejection, no room partition),
//!   while surfacing nothing of the sealed body;
//! * **writers are disabled**: `SyncEngine::publish` refuses a locally-
//!   authored envelope until the R2 compatibility floor (D8);
//! * the golden vector pins an encrypted event's exact CSB bytes and event id
//!   (mirroring `golden_vectors.rs`), with the ciphertext produced by the real
//!   step-2 crypto crate so the two crates are exercised together;
//! * the D9 fail-closed posture holds: an envelope wrapping `file.shared`
//!   contributes **no** blob hash to the serve allowlist without a key.
//!
//! The AAD used to seal the golden ciphertext is an explicitly NON-normative
//! placeholder: the D2 signed-prefix AAD is pinned by the R2 write path (§7
//! step 4), not by this reader-first phase. This file freezes the **envelope
//! encoding** (field names, types, order — the OQ-2 resolution), not the AAD.

use iroh_rooms_core::event::cbor::{self, CborValue};
use iroh_rooms_core::event::constants::{
    ENCRYPTED_NONCE_LEN, ENCRYPTED_SUITE_V1, ENCRYPTED_TAG_LEN,
    MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT, MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT,
};
use iroh_rooms_core::event::content::{Content, EncryptedContent, EventType, MessageText};
use iroh_rooms_core::event::ids::EventId;
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::reject::RejectReason;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;

// --- Fixtures: the golden cast of `tests/golden_vectors.rs`. -----------------

const ROOM_NONCE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];
const ROOM_CREATED_AT: u64 = 1_750_000_000_000;
const ENCRYPTED_CREATED_AT: u64 = 1_750_000_006_000;
/// Fixed placeholder causal parent (the fixture is a serialization vector, not
/// a live DAG event; a non-genesis event just needs non-empty `prev_events`).
const PARENT_ID: [u8; 32] = [0x11; 32];
/// Fixed golden wrap inputs: the step-2 vectors' room-key pattern and a
/// distinct nonce pattern.
const GOLDEN_NONCE: [u8; ENCRYPTED_NONCE_LEN] = [
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
];
/// Non-normative R1 placeholder AAD (see module docs).
const R1_PLACEHOLDER_AAD: &[u8] = b"iroh-rooms/#191-R1-golden-placeholder-aad";

// --- Golden pins (generated once from the fixtures above). -------------------

const GOLDEN_ENCRYPTED_CSB_HEX: &str = "a867636f6e74656e74a5656e6f6e63654ca0a1a2a3a4a5a6a7a8a9aaab65737569746501696b65795f65706f6368076a63697068657274657874582e447c1e4221b268f70709ebbc2708afb11dca3f7fe0da2318f97e4ae716c5b9dfe9d274b170fea7fc9a6ecf99e59a6a696e6e65725f747970656c6d6573736167652e7465787467726f6f6d5f6964582043c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3696465766963655f696458208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b3946973656e6465725f696458208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c6a637265617465645f61741b000001977420f3706a6576656e745f7479706571636f6e74656e742e656e637279707465646b707265765f6576656e747381582011111111111111111111111111111111111111111111111111111111111111116e736368656d615f76657273696f6e01";
const GOLDEN_ENCRYPTED_EVENT_ID: &str =
    "blake3:3afa261d0e748231ea7c99a04224735385f3c20f83bb009cef4308df14f3c98a";

fn alice_identity_secret() -> SigningKey {
    SigningKey::from_seed(&[0x01; 32])
}

fn alice_device_secret() -> SigningKey {
    SigningKey::from_seed(&[0x02; 32])
}

fn alice_identity() -> IdentityKey {
    alice_identity_secret().identity_key()
}

fn room_a() -> iroh_rooms_core::event::ids::RoomId {
    signed::derive_room_id(&alice_identity(), &ROOM_NONCE, ROOM_CREATED_AT)
}

/// The sealed golden body: the canonical CBOR of the golden `message.text`
/// content, sealed by the real step-2 crate under a fixed key and nonce.
fn golden_ciphertext() -> Vec<u8> {
    let room_key = iroh_rooms_crypto::RoomKey::from_bytes(core::array::from_fn(|i| {
        u8::try_from(i).expect("index < 32")
    }));
    let plaintext = cbor::encode(
        &Content::MessageText(MessageText {
            body: "Hello room".to_owned(),
            format: Some("plain".to_owned()),
            in_reply_to: None,
            mentions: None,
        })
        .to_cbor(),
    );
    iroh_rooms_crypto::seal_content(&room_key, &GOLDEN_NONCE, &plaintext, R1_PLACEHOLDER_AAD)
        .expect("golden seal succeeds")
}

fn golden_encrypted_event() -> SignedEvent {
    SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::ContentEncrypted,
        created_at: ENCRYPTED_CREATED_AT,
        prev_events: vec![EventId::from_bytes(PARENT_ID)],
        content: Content::Encrypted(EncryptedContent {
            inner_type: EventType::MessageText,
            key_epoch: 7,
            suite: ENCRYPTED_SUITE_V1,
            nonce: GOLDEN_NONCE,
            ciphertext: golden_ciphertext(),
        }),
    }
}

fn seal(ev: &SignedEvent) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    WireEvent::seal(csb, sig).to_bytes()
}

// --- Constant cross-pins with the step-2 crypto crate. -----------------------

/// The core envelope constants and the pure crypto crate's canonical
/// `SUITE_V1` constants can never drift (the doc comment on
/// `ENCRYPTED_SUITE_V1` promises exactly this test).
#[test]
fn envelope_constants_match_crypto_crate() {
    assert_eq!(ENCRYPTED_SUITE_V1, iroh_rooms_crypto::SUITE_V1);
    assert_eq!(ENCRYPTED_NONCE_LEN, iroh_rooms_crypto::NONCE_LEN);
    assert_eq!(ENCRYPTED_TAG_LEN, iroh_rooms_crypto::TAG_LEN);
}

// --- Golden vector (spec §5: pin exact bytes + id). --------------------------

/// The encrypted event's CSB bytes and event id are pinned exactly, and the
/// build is deterministic across two constructions.
#[test]
fn golden_encrypted_csb_and_event_id_are_pinned() {
    let ev = golden_encrypted_event();
    let csb = ev.to_csb();
    assert_eq!(
        hex::encode(&csb),
        GOLDEN_ENCRYPTED_CSB_HEX,
        "the content.encrypted wire encoding is frozen (OQ-2); an intentional \
         format change must update this vector"
    );
    assert_eq!(ev.event_id().to_named_string(), GOLDEN_ENCRYPTED_EVENT_ID);
    assert_eq!(
        golden_encrypted_event().to_csb(),
        csb,
        "deterministic build"
    );
}

/// AC10: the envelope is opaque-but-valid — it passes the full stateless
/// pipeline (canonicality, signature, room binding, causal structure), and the
/// decoded content round-trips byte-exactly through `to_cbor`.
#[test]
fn golden_encrypted_event_is_opaque_but_valid() {
    let bytes = seal(&golden_encrypted_event());
    let ctx = ValidationContext::for_room(room_a());
    let validated = validate_wire_bytes(&bytes, &ctx).expect("envelope must validate");
    assert_eq!(validated.event.event_type, EventType::ContentEncrypted);
    let Content::Encrypted(c) = &validated.event.content else {
        panic!("decoded content must be the encrypted envelope");
    };
    assert_eq!(c.inner_type, EventType::MessageText);
    assert_eq!(c.key_epoch, 7);
    assert_eq!(c.suite, ENCRYPTED_SUITE_V1);
    // Round-trip: the validator's re-canonicalization check already ran; assert
    // it explicitly for the new variant.
    assert_eq!(
        validated.event.to_csb(),
        validated.signed_bytes(),
        "parse → to_cbor must be byte-exact for content.encrypted"
    );
}

// --- Strict-parse negatives (all cleartext, all `invalid_content`). ----------

/// Build a validly-signed event whose content map is handcrafted, so a single
/// planted defect fails exactly at strict content validation.
fn envelope_bytes_with(entries: Vec<(&str, CborValue)>) -> Vec<u8> {
    let content = CborValue::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    );
    let map = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(1)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room_a().as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(alice_identity().as_bytes().to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(alice_device_secret().device_key().as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("content.encrypted".to_owned()),
        ),
        (
            "created_at".to_owned(),
            CborValue::Uint(ENCRYPTED_CREATED_AT),
        ),
        (
            "prev_events".to_owned(),
            CborValue::Array(vec![CborValue::Bytes(PARENT_ID.to_vec())]),
        ),
        ("content".to_owned(), content),
    ]);
    let csb = cbor::encode(&map);
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    WireEvent::seal(csb, sig).to_bytes()
}

fn base_entries() -> Vec<(&'static str, CborValue)> {
    vec![
        ("inner_type", CborValue::Text("message.text".to_owned())),
        ("key_epoch", CborValue::Uint(7)),
        ("suite", CborValue::Uint(u64::from(ENCRYPTED_SUITE_V1))),
        ("nonce", CborValue::Bytes(GOLDEN_NONCE.to_vec())),
        (
            "ciphertext",
            CborValue::Bytes(vec![0x5a; ENCRYPTED_TAG_LEN + 32]),
        ),
    ]
}

fn replace_entry(
    entries: Vec<(&'static str, CborValue)>,
    key: &'static str,
    value: &CborValue,
) -> Vec<(&'static str, CborValue)> {
    entries
        .into_iter()
        .map(|(k, v)| if k == key { (k, value.clone()) } else { (k, v) })
        .collect()
}

fn with_entry(key: &'static str, value: &CborValue) -> Vec<(&'static str, CborValue)> {
    replace_entry(base_entries(), key, value)
}

fn expect_invalid(entries: Vec<(&'static str, CborValue)>, what: &str) {
    let bytes = envelope_bytes_with(entries);
    assert_eq!(
        validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a())),
        Err(RejectReason::InvalidContent),
        "{what} must be rejected as invalid_content"
    );
}

/// The well-formed baseline the negatives mutate from is itself accepted.
#[test]
fn baseline_handcrafted_envelope_is_accepted() {
    let bytes = envelope_bytes_with(base_entries());
    validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a()))
        .expect("baseline envelope must validate");
}

/// D1 + D2: the inner type must be a registered, encryptable content type —
/// membership bodies, nested envelopes, and unknown strings all fail closed.
#[test]
fn invalid_inner_types_are_rejected() {
    for (inner, what) in [
        ("member.removed", "a membership inner type"),
        ("room.created", "the genesis inner type"),
        ("content.encrypted", "a nested envelope"),
        ("message.bogus", "an unknown inner type"),
    ] {
        expect_invalid(
            with_entry("inner_type", &CborValue::Text(inner.to_owned())),
            what,
        );
    }
}

/// D3: `suite` must be exactly `ENCRYPTED_SUITE_V1` — fail-closed.
#[test]
fn foreign_suite_ids_are_rejected_fail_closed() {
    for suite in [0u64, 2, 0xff, 0x100, u64::MAX] {
        expect_invalid(
            with_entry("suite", &CborValue::Uint(suite)),
            "a non-SUITE_V1 suite id",
        );
    }
}

/// The nonce must be exactly `ENCRYPTED_NONCE_LEN` bytes.
#[test]
fn wrong_nonce_lengths_are_rejected() {
    for len in [0usize, ENCRYPTED_NONCE_LEN - 1, ENCRYPTED_NONCE_LEN + 1] {
        expect_invalid(
            with_entry("nonce", &CborValue::Bytes(vec![0xa0; len])),
            "a wrong-length nonce",
        );
    }
}

/// D2a: `ciphertext` is bounded — at least a bare tag, at most the inner
/// type's plaintext cap + tag. Both boundaries are exact.
#[test]
fn ciphertext_bounds_are_exact() {
    let cap = MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT + ENCRYPTED_TAG_LEN;

    // In-bounds extremes are accepted.
    for len in [ENCRYPTED_TAG_LEN, cap] {
        let bytes =
            envelope_bytes_with(with_entry("ciphertext", &CborValue::Bytes(vec![0x5a; len])));
        validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a()))
            .expect("in-bounds ciphertext must validate");
    }

    // One byte outside either bound fails closed.
    for len in [ENCRYPTED_TAG_LEN - 1, cap + 1] {
        expect_invalid(
            with_entry("ciphertext", &CborValue::Bytes(vec![0x5a; len])),
            "an out-of-bounds ciphertext",
        );
    }
}

/// The D2a cap dispatches **per inner type**: the same ciphertext length is
/// out-of-bounds for `pipe.closed` (cap 1,024) yet in-bounds for
/// `message.text` (cap 20,480). A regression collapsing
/// `EventType::max_encrypted_plaintext_bytes` to one constant fails here.
#[test]
fn ciphertext_cap_dispatches_per_inner_type() {
    let pipe_closed_cap = MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT + ENCRYPTED_TAG_LEN;
    let as_pipe_closed = |len: usize| {
        replace_entry(
            with_entry("inner_type", &CborValue::Text("pipe.closed".to_owned())),
            "ciphertext",
            &CborValue::Bytes(vec![0x5a; len]),
        )
    };

    // Exactly at the pipe.closed cap: accepted.
    let bytes = envelope_bytes_with(as_pipe_closed(pipe_closed_cap));
    validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a()))
        .expect("pipe.closed ciphertext at its own cap must validate");

    // One byte over the pipe.closed cap: rejected for pipe.closed...
    expect_invalid(
        as_pipe_closed(pipe_closed_cap + 1),
        "a ciphertext over the pipe.closed cap",
    );

    // ...while the identical length is fine under message.text's larger cap.
    let bytes = envelope_bytes_with(with_entry(
        "ciphertext",
        &CborValue::Bytes(vec![0x5a; pipe_closed_cap + 1]),
    ));
    validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a()))
        .expect("the same length must be in-bounds for message.text");
}

/// The strict key set: a missing required key and an unknown extra key are
/// both `invalid_content` (the closed-registry discipline of every other
/// content type applies to the envelope too).
#[test]
fn missing_and_unknown_envelope_keys_are_rejected() {
    for missing in ["inner_type", "key_epoch", "suite", "nonce", "ciphertext"] {
        let entries: Vec<_> = base_entries()
            .into_iter()
            .filter(|(k, _)| *k != missing)
            .collect();
        expect_invalid(entries, "an envelope missing a required key");
    }

    let mut extra = base_entries();
    extra.push(("surprise", CborValue::Uint(1)));
    expect_invalid(extra, "an envelope with an unknown key");
}

/// Wrong CBOR types for envelope fields fail closed.
#[test]
fn wrong_field_types_are_rejected() {
    expect_invalid(
        with_entry("inner_type", &CborValue::Uint(5)),
        "a non-text inner_type",
    );
    expect_invalid(
        with_entry("key_epoch", &CborValue::Text("7".to_owned())),
        "a non-uint key_epoch",
    );
    expect_invalid(
        with_entry("nonce", &CborValue::Text("aTvzC3rNL24=".to_owned())),
        "a non-bytes nonce",
    );
    expect_invalid(
        with_entry("ciphertext", &CborValue::Array(vec![])),
        "a non-bytes ciphertext",
    );
}

// --- Engine behavior: R1 writer gate + opaque sync (needs the sync engine). --

#[cfg(all(feature = "sync", feature = "store"))]
mod engine {
    use super::*;
    use iroh_rooms_core::event::binding::DeviceBinding;
    use iroh_rooms_core::event::content::{FileShared, RoomCreated};
    use iroh_rooms_core::event::ids::HashRef;
    use iroh_rooms_core::store::EventStore;
    use iroh_rooms_core::sync::sim::SimNet;
    use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine, SyncError};

    const NODE_A: PeerId = PeerId::from_bytes([0xA1; 32]);
    const NODE_B: PeerId = PeerId::from_bytes([0xB2; 32]);

    fn fresh_engine() -> SyncEngine {
        let store = EventStore::open_in_memory().expect("in-memory store");
        SyncEngine::open(store, room_a(), SyncConfig::default()).expect("open engine")
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
                room_name: "R1 Envelope".to_owned(),
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

    /// An encrypted event parented on the live genesis (unlike the golden
    /// serialization fixture, this one is DAG-valid in the engine).
    fn live_encrypted_event(inner_type: EventType, ciphertext: Vec<u8>) -> SignedEvent {
        SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: alice_identity(),
            device_id: alice_device_secret().device_key(),
            event_type: EventType::ContentEncrypted,
            created_at: ENCRYPTED_CREATED_AT,
            prev_events: vec![genesis_event().event_id()],
            content: Content::Encrypted(EncryptedContent {
                inner_type,
                key_epoch: 1,
                suite: ENCRYPTED_SUITE_V1,
                nonce: GOLDEN_NONCE,
                ciphertext,
            }),
        }
    }

    /// D8 phase R1: `publish` refuses a locally-authored envelope with the
    /// typed writers-disabled error — **before** any persist or fan-out: the
    /// refused event must not enter the validated set.
    #[test]
    fn publish_rejects_locally_authored_envelope() {
        let mut engine = fresh_engine();
        engine
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");

        let refused =
            live_encrypted_event(EventType::MessageText, vec![0x5a; ENCRYPTED_TAG_LEN + 32]);
        let refused_id = refused.event_id();
        let err = engine
            .publish(&seal(&refused))
            .expect_err("R1 must refuse a locally-authored content.encrypted");
        assert!(
            matches!(err, SyncError::EncryptedWritesDisabled),
            "expected the writers-disabled error, got: {err}"
        );

        // The gate must run before deliver(): nothing persisted, no fan-out.
        let digest = engine.digest().expect("digest");
        assert!(
            !digest.event_ids.contains(&refused_id),
            "a refused envelope must not be persisted"
        );
        assert_eq!(
            digest.event_ids.len(),
            1,
            "only the genesis may be in the validated set"
        );
    }

    /// AC10: a remote peer's encrypted event ingests as opaque-but-valid and
    /// syncs to another node without wedging convergence — the whole point of
    /// the reader-first phase.
    #[test]
    fn remote_envelope_ingests_and_syncs() {
        let encrypted = live_encrypted_event(EventType::MessageText, vec![0x5a; 64]);
        let encrypted_id = encrypted.event_id();

        let mut net = SimNet::new(room_a());
        net.add_peer(NODE_A, fresh_engine());
        net.add_peer(NODE_B, fresh_engine());

        // Node A learns the room from local publishes and the envelope from a
        // remote peer (the R1 gate applies to local authoring only).
        net.engine_mut(NODE_A)
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");
        net.engine_mut(NODE_A)
            .ingest_frame(NODE_B, &seal(&encrypted));

        net.connect(NODE_A, NODE_B);
        net.run_to_quiescence();
        net.assert_converged(&[NODE_A, NODE_B]);

        for node in [NODE_A, NODE_B] {
            let digest = net.engine(node).digest().expect("digest");
            assert!(
                digest.event_ids.contains(&encrypted_id),
                "the encrypted event must be in {node:?}'s validated set"
            );
        }
    }

    /// The envelope is **chat-class** (`is_chat_class`): it must travel the
    /// bounded recent-chat window like the bodies it wraps — otherwise R2
    /// writers' events would be invisible to R1 readers' window pulls,
    /// defeating reader-first. With a 1-event window and three chat siblings,
    /// the window serves only its canonical-order tail: the encrypted event
    /// must fill the slot and the plaintext siblings must NOT arrive by any
    /// other path. (Verified discriminating: dropping `ContentEncrypted` from
    /// `is_chat_class` fails this test, while the plain convergence test
    /// above still passes under that mutation.)
    #[test]
    #[allow(clippy::too_many_lines)] // fully-specified invite/join/chat cast inline for clarity
    fn envelope_travels_the_bounded_chat_window() {
        // Chat-class means non-admin-authored: alice (the admin) would stamp
        // admin_seq on her events and sync them via the never-windowed admin
        // chain. Bob, a plain member, authors the chat here.
        let bob_identity_secret = SigningKey::from_seed(&[0x10; 32]);
        let bob_device_secret = SigningKey::from_seed(&[0x90; 32]);
        let bob = bob_identity_secret.identity_key();

        let genesis = genesis_event();
        let invite_id = [0x01; 16];
        let invite_secret = [0x41; 16];
        let invite = SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: alice_identity(),
            device_id: alice_device_secret().device_key(),
            event_type: EventType::MemberInvited,
            created_at: ROOM_CREATED_AT + 1,
            prev_events: vec![genesis.event_id()],
            content: Content::MemberInvited(iroh_rooms_core::event::content::MemberInvited {
                invite_id,
                capability_hash: iroh_rooms_core::event::capability_hash(
                    &room_a(),
                    &invite_id,
                    &invite_secret,
                ),
                role: "member".to_owned(),
                invitee_key: bob,
                expires_at: None,
                invitee_hint: None,
            }),
        };
        let join = SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: bob,
            device_id: bob_device_secret.device_key(),
            event_type: EventType::MemberJoined,
            created_at: ROOM_CREATED_AT + 2,
            prev_events: vec![invite.event_id()],
            content: Content::MemberJoined(iroh_rooms_core::event::content::MemberJoined {
                via_invite_id: invite_id,
                capability_secret: invite_secret,
                role: "member".to_owned(),
                device_binding: DeviceBinding::create(
                    &room_a(),
                    &bob_identity_secret,
                    bob_device_secret.device_key(),
                ),
                display_name: None,
            }),
        };

        let bob_message = |body: &str, created_at: u64| SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: bob,
            device_id: bob_device_secret.device_key(),
            event_type: EventType::MessageText,
            created_at,
            prev_events: vec![join.event_id()],
            content: Content::MessageText(MessageText {
                body: body.to_owned(),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        let msg1 = bob_message("first plaintext", ENCRYPTED_CREATED_AT - 2);
        let msg2 = bob_message("second plaintext", ENCRYPTED_CREATED_AT - 1);

        let bob_encrypted = |fill: u8| SignedEvent {
            schema_version: 1,
            room_id: room_a(),
            sender_id: bob,
            device_id: bob_device_secret.device_key(),
            event_type: EventType::ContentEncrypted,
            created_at: ENCRYPTED_CREATED_AT,
            prev_events: vec![join.event_id()],
            content: Content::Encrypted(EncryptedContent {
                inner_type: EventType::MessageText,
                key_epoch: 1,
                suite: ENCRYPTED_SUITE_V1,
                nonce: GOLDEN_NONCE,
                ciphertext: vec![fill; 64],
            }),
        };
        // All three chat events are equal-lamport siblings of bob's join, so
        // the 1-event window serves the max **event id** among them. Event ids
        // are hashes, so pick the first ciphertext fill byte that makes the
        // envelope sort last — deterministic for fixed fixtures, and
        // self-adjusting if an encoding change ever reshuffles the ids.
        let encrypted = (0u8..=255)
            .map(bob_encrypted)
            .find(|ev| ev.event_id() > msg1.event_id() && ev.event_id() > msg2.event_id())
            .expect("some fill byte makes the envelope sort last among the siblings");

        let bob_seal = |ev: &SignedEvent| {
            let csb = ev.to_csb();
            let sig = signed::sign_csb(&csb, &bob_device_secret);
            WireEvent::seal(csb, sig).to_bytes()
        };

        let mut net = SimNet::new(room_a());
        net.add_peer(NODE_A, fresh_engine());
        let tight = iroh_rooms_core::sync::SyncConfig {
            chat_window_default: 1,
            ..iroh_rooms_core::sync::SyncConfig::default()
        };
        let store = EventStore::open_in_memory().expect("in-memory store");
        net.add_peer(
            NODE_B,
            SyncEngine::open(store, room_a(), tight).expect("open engine"),
        );

        net.engine_mut(NODE_A)
            .publish(&seal(&genesis))
            .expect("genesis publishes");
        net.engine_mut(NODE_A)
            .publish(&seal(&invite))
            .expect("invite publishes");
        for frame in [bob_seal(&join), bob_seal(&msg1), bob_seal(&msg2)] {
            net.engine_mut(NODE_A)
                .ingest_frame(PeerId::from_bytes([0xC3; 32]), &frame);
        }
        // The envelope enters via ingest too (the R1 gate blocks local publish).
        net.engine_mut(NODE_A)
            .ingest_frame(PeerId::from_bytes([0xC3; 32]), &bob_seal(&encrypted));

        // Sanity: node A accepted all of bob's events.
        let a = net.engine(NODE_A).digest().expect("digest");
        for (name, id) in [
            ("join", join.event_id()),
            ("msg1", msg1.event_id()),
            ("msg2", msg2.event_id()),
            ("encrypted", encrypted.event_id()),
        ] {
            assert!(a.event_ids.contains(&id), "node A must hold {name}");
        }

        net.connect(NODE_A, NODE_B);
        net.run_to_quiescence();

        let b = net.engine(NODE_B).digest().expect("digest");
        assert!(
            b.event_ids.contains(&encrypted.event_id()),
            "the encrypted event must arrive via the bounded chat window"
        );
        for (name, ev) in [("msg1", &msg1), ("msg2", &msg2)] {
            assert!(
                !b.event_ids.contains(&ev.event_id()),
                "{name} is outside the 1-event window and must not arrive"
            );
        }
    }

    /// D9 fail-closed posture: an envelope wrapping `file.shared` contributes
    /// **no** blob hash to the serve allowlist — a node without the key must
    /// not serve (or invent) the referenced blob.
    #[test]
    fn encrypted_file_share_contributes_no_blob_hash() {
        // A real plaintext file.shared body, sealed — so the only difference
        // from a hash-contributing event is the encryption.
        let plaintext_body = Content::FileShared(FileShared {
            file_id: [0x33; 16],
            name: "secret.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 1234,
            blob_hash: HashRef::from_bytes([0x44; 32]),
            blob_format: None,
            providers: None,
        });
        let room_key = iroh_rooms_crypto::RoomKey::generate();
        let sealed_body = iroh_rooms_crypto::seal_content(
            &room_key,
            &GOLDEN_NONCE,
            &cbor::encode(&plaintext_body.to_cbor()),
            R1_PLACEHOLDER_AAD,
        )
        .expect("seals");

        let mut engine = fresh_engine();
        engine
            .publish(&seal(&genesis_event()))
            .expect("genesis publishes");
        engine.ingest_frame(
            NODE_B,
            &seal(&live_encrypted_event(EventType::FileShared, sealed_body)),
        );

        let hashes = engine.file_shared_hashes().expect("read allowlist");
        assert!(
            hashes.is_empty(),
            "an unreadable file.shared must contribute nothing to the blob \
             serve allowlist (fail closed), got {hashes:?}"
        );
    }
}
