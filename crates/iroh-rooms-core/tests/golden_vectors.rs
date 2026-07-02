//! Focused golden-vector + acceptance-criteria conformance for the canonical
//! signed event model (`PHASE-0-SPIKE.md` Protocol Test Vectors §1–§7 and the
//! IR-0002 issue Test Plan).
//!
//! These pin the byte-exact, independently-reproduced golden values (cast keys
//! from seeds, 242-byte CSB, `event_id`, signature, `room_id_A/B`, tampered id)
//! and exercise each issue acceptance criterion through the public API. Direct
//! unit tests for the strict CBOR reader live in `cbor.rs`'s inline
//! `#[cfg(test)] mod tests`; property tests live in `tests/cbor_property.rs`
//! (spec `strict-cbor-reader-unit-property-fuzz-tests.md`, risk R1).

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::cbor::{self, CborValue};
use iroh_rooms_core::event::content::{Content, EventType, MessageText, RoomCreated};
use iroh_rooms_core::event::ids::{EventId, HashParseError, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::reject::{Flag, RejectReason};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;

// --- Fixtures (PHASE-0-SPIKE.md Protocol Test Vectors, "Cast" / "Room"). ------

const ALICE_ID_HEX: &str = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";
const ALICE_DEV_HEX: &str = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394";
const ROOM_NONCE_HEX: &str = "000102030405060708090a0b0c0d0e0f";
const ROOM_ID_A_HEX: &str = "43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3";
const ROOM_ID_B_HEX: &str = "cad9174a1e34a847711e85968020a5cabaf9b35ed600d21457458f95c9c53494";
const GOLDEN_EVENT_ID: &str =
    "blake3:c389e251f9654902d26ea937b3e84a01bb5e5d578e394c95b6ade8b7144e85a1";
const TAMPERED_EVENT_ID: &str =
    "blake3:6267b72c066e30154b34d4430ce8fb735563c4500ff527d371bcc3de7f34c75c";

const ROOM_CREATED_AT: u64 = 1_750_000_000_000;
const ROOM_B_CREATED_AT: u64 = 1_750_000_000_001;
const GOLDEN_CREATED_AT: u64 = 1_750_000_005_000;

fn arr32(h: &str) -> [u8; 32] {
    hex::decode(h).unwrap().try_into().unwrap()
}

fn arr16(h: &str) -> [u8; 16] {
    hex::decode(h).unwrap().try_into().unwrap()
}

fn alice_identity_secret() -> SigningKey {
    SigningKey::from_seed(&[0x01; 32])
}

fn alice_device_secret() -> SigningKey {
    SigningKey::from_seed(&[0x02; 32])
}

/// The golden `message.text` event (Test Vector §1). A serialization fixture
/// (`prev_events=[]`), not a live event.
fn golden_event() -> SignedEvent {
    SignedEvent {
        schema_version: 1,
        room_id: signed::derive_room_id(
            &IdentityKey::from_bytes(arr32(ALICE_ID_HEX)),
            &arr16(ROOM_NONCE_HEX),
            ROOM_CREATED_AT,
        ),
        sender_id: IdentityKey::from_bytes(arr32(ALICE_ID_HEX)),
        device_id: DeviceKey::from_bytes(arr32(ALICE_DEV_HEX)),
        event_type: EventType::MessageText,
        created_at: GOLDEN_CREATED_AT,
        prev_events: vec![],
        content: Content::MessageText(MessageText {
            body: "Hello room".to_owned(),
            format: Some("plain".to_owned()),
            in_reply_to: None,
            mentions: None,
        }),
    }
}

// --- Fixture sanity: seeds reproduce the published public keys. --------------

#[test]
fn seeds_reproduce_cast_public_keys() {
    assert_eq!(
        hex::encode(alice_identity_secret().identity_key().as_bytes()),
        ALICE_ID_HEX
    );
    assert_eq!(
        hex::encode(alice_device_secret().device_key().as_bytes()),
        ALICE_DEV_HEX
    );
}

// --- V1: canonical determinism (242-byte CSB, encoder/field-order independence).

#[test]
fn golden_csb_is_242_bytes_with_expected_prefix() {
    let csb = golden_event().to_csb();
    assert_eq!(csb.len(), 242, "golden CSB must be exactly 242 bytes");
    assert!(
        hex::encode(&csb).starts_with("a867636f6e74656e74a264626f6479"),
        "unexpected CSB prefix: {}",
        hex::encode(&csb)
    );
    assert_eq!(
        csb.last(),
        Some(&0x01),
        "CSB must end with schema_version=1"
    );
}

#[test]
fn scrambled_top_level_key_order_yields_identical_csb() {
    // Same eight logical fields, built in two different insertion orders, must
    // encode to byte-identical canonical CSB (Test Vector §1).
    let ev = golden_event();
    let decl_order = ev.to_csb();

    let scrambled = CborValue::Map(vec![
        ("prev_events".to_owned(), CborValue::Array(vec![])),
        (
            "content".to_owned(),
            CborValue::Map(vec![
                ("format".to_owned(), CborValue::Text("plain".to_owned())),
                ("body".to_owned(), CborValue::Text("Hello room".to_owned())),
            ]),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("message.text".to_owned()),
        ),
        ("schema_version".to_owned(), CborValue::Uint(1)),
        ("created_at".to_owned(), CborValue::Uint(GOLDEN_CREATED_AT)),
        (
            "device_id".to_owned(),
            CborValue::Bytes(arr32(ALICE_DEV_HEX).to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(arr32(ALICE_ID_HEX).to_vec()),
        ),
        (
            "room_id".to_owned(),
            CborValue::Bytes(arr32(ROOM_ID_A_HEX).to_vec()),
        ),
    ]);

    assert_eq!(cbor::encode(&scrambled), decl_order);
}

// --- V3: event id is BLAKE3-256(CSB), and `id` is recomputed, never trusted. --

#[test]
fn golden_event_id_matches() {
    let csb = golden_event().to_csb();
    assert_eq!(
        signed::event_id_from_bytes(&csb).to_named_string(),
        GOLDEN_EVENT_ID
    );
    assert_eq!(golden_event().event_id().to_named_string(), GOLDEN_EVENT_ID);
}

// --- V4 / V7: room_id derivation (genesis), and room B differs by created_at. -

#[test]
fn room_id_derivation_matches_golden() {
    let alice = IdentityKey::from_bytes(arr32(ALICE_ID_HEX));
    let nonce = arr16(ROOM_NONCE_HEX);
    assert_eq!(
        hex::encode(signed::derive_room_id(&alice, &nonce, ROOM_CREATED_AT).as_bytes()),
        ROOM_ID_A_HEX
    );
    assert_eq!(
        hex::encode(signed::derive_room_id(&alice, &nonce, ROOM_B_CREATED_AT).as_bytes()),
        ROOM_ID_B_HEX
    );
}

// --- V5: signature verifies under device_id, NOT sender_id (acceptance crit). -

#[test]
fn signature_verifies_under_device_id_not_sender_id() {
    let csb = golden_event().to_csb();
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    let msg = signed::event_signing_message(&csb);

    // Correct device key: accepts.
    let device = DeviceKey::from_bytes(arr32(ALICE_DEV_HEX));
    assert!(device.verify(&msg, &sig).is_ok());

    // The classic "verify under sender_id" bug: the identity key's bytes used as
    // a device key MUST fail.
    let sender_as_device = DeviceKey::from_bytes(arr32(ALICE_ID_HEX));
    assert!(sender_as_device.verify(&msg, &sig).is_err());

    // The deterministic Ed25519 signature reproduces the golden prefix/suffix.
    let sig_hex = hex::encode(sig.as_bytes());
    assert!(sig_hex.starts_with("98732ece"), "sig = {sig_hex}");
    assert!(sig_hex.ends_with("4f0f"), "sig = {sig_hex}");
}

// --- V6: tampered body changes the id (and would fail the signature). ---------

#[test]
fn tampered_body_changes_event_id() {
    let mut ev = golden_event();
    ev.content = Content::MessageText(MessageText {
        body: "Hello rooM".to_owned(),
        format: Some("plain".to_owned()),
        in_reply_to: None,
        mentions: None,
    });
    assert_eq!(ev.event_id().to_named_string(), TAMPERED_EVENT_ID);
}

// --- Acceptance: a valid genesis room.created round-trips through the validator.

/// Build a real, causally-structural `room.created` genesis and its `WireEvent`
/// bytes (regenerated per spec R2 — the golden serialization fixture is not a
/// live event).
fn genesis_wire_bytes() -> (Vec<u8>, Vec<u8>, iroh_rooms_core::event::ids::RoomId) {
    let id_secret = alice_identity_secret();
    let dev_secret = alice_device_secret();
    let alice_id = id_secret.identity_key();
    let alice_dev = dev_secret.device_key();
    let nonce = arr16(ROOM_NONCE_HEX);
    let room_id = signed::derive_room_id(&alice_id, &nonce, ROOM_CREATED_AT);
    let binding =
        iroh_rooms_core::event::binding::DeviceBinding::create(&room_id, &id_secret, alice_dev);
    let event = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id,
        device_id: alice_dev,
        event_type: EventType::RoomCreated,
        created_at: ROOM_CREATED_AT,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Test Room".to_owned(),
            room_nonce: nonce,
            admins: vec![alice_id],
            device_binding: binding,
        }),
    };
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, &dev_secret);
    let wire = WireEvent::seal(csb.clone(), sig);
    (wire.to_bytes(), csb, room_id)
}

#[test]
fn valid_genesis_is_accepted_and_preserves_signed_bytes() {
    let (bytes, csb, room_id) = genesis_wire_bytes();
    let ctx = ValidationContext::for_room(room_id);
    let validated = validate_wire_bytes(&bytes, &ctx).expect("genesis must validate");

    // Verbatim signed-byte preservation (acceptance criterion).
    assert_eq!(validated.signed_bytes(), csb.as_slice());
    // Recomputed id is the stable dedup key and re-hashes the verbatim bytes.
    assert_eq!(
        validated.event_id,
        signed::event_id_from_bytes(validated.signed_bytes())
    );
    assert_eq!(validated.event.event_type, EventType::RoomCreated);
    assert!(validated.flags.is_empty());
}

#[test]
fn doctored_advisory_id_is_rejected() {
    let (bytes, _, room_id) = genesis_wire_bytes();
    let mut wire = WireEvent::decode(&bytes).unwrap();
    wire.id = "blake3:0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    let ctx = ValidationContext::for_room(room_id);
    assert_eq!(
        validate_wire_bytes(&wire.to_bytes(), &ctx),
        Err(RejectReason::IdMismatch)
    );
}

#[test]
fn bad_signature_is_rejected() {
    let (bytes, csb, room_id) = genesis_wire_bytes();
    let mut wire = WireEvent::decode(&bytes).unwrap();
    // Replace the signature with a valid signature over DIFFERENT bytes; the id
    // (over `signed`) still matches, so we reach and fail the signature step.
    wire.sig = signed::sign_csb(b"not the csb", &alice_device_secret());
    let _ = csb;
    let ctx = ValidationContext::for_room(room_id);
    assert_eq!(
        validate_wire_bytes(&wire.to_bytes(), &ctx),
        Err(RejectReason::BadSignature)
    );
}

#[test]
fn cross_room_replay_is_rejected() {
    // The golden message.text carries room_id_A; processing it for room B must
    // fail at room binding (Test Vector §7).
    let ev = golden_event();
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    let bytes = WireEvent::seal(csb, sig).to_bytes();

    let room_b = signed::derive_room_id(
        &IdentityKey::from_bytes(arr32(ALICE_ID_HEX)),
        &arr16(ROOM_NONCE_HEX),
        ROOM_B_CREATED_AT,
    );
    assert_eq!(hex::encode(room_b.as_bytes()), ROOM_ID_B_HEX);
    assert_eq!(
        validate_wire_bytes(&bytes, &ValidationContext::for_room(room_b)),
        Err(RejectReason::RoomIdMismatch)
    );
}

#[test]
fn non_genesis_with_empty_prev_events_is_rejected() {
    // The same golden message.text, processed for its OWN room (room A), passes
    // room binding but fails the stateless genesis-descent structural check.
    let ev = golden_event();
    let room_a = ev.room_id;
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    let bytes = WireEvent::seal(csb, sig).to_bytes();
    assert_eq!(
        validate_wire_bytes(&bytes, &ValidationContext::for_room(room_a)),
        Err(RejectReason::NotGenesisDescended)
    );
}

// --- Acceptance: unknown schema/type/content-key are rejected. ----------------

/// Build canonically-encoded, validly-signed CSB from raw top-level parts, so we
/// can craft single-defect negative cases that still pass the id/canonical/sig
/// steps and exercise the version/type/content checks.
fn signed_bytes_with(
    schema_version: u64,
    event_type: &str,
    content: CborValue,
    prev_events: Vec<CborValue>,
) -> Vec<u8> {
    let room_id = signed::derive_room_id(
        &IdentityKey::from_bytes(arr32(ALICE_ID_HEX)),
        &arr16(ROOM_NONCE_HEX),
        ROOM_CREATED_AT,
    );
    let map = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(schema_version)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room_id.as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(arr32(ALICE_ID_HEX).to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(arr32(ALICE_DEV_HEX).to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text(event_type.to_owned()),
        ),
        ("created_at".to_owned(), CborValue::Uint(GOLDEN_CREATED_AT)),
        ("prev_events".to_owned(), CborValue::Array(prev_events)),
        ("content".to_owned(), content),
    ]);
    cbor::encode(&map)
}

fn seal_and_room(csb: Vec<u8>) -> (Vec<u8>, ValidationContext) {
    let room_id = signed::derive_room_id(
        &IdentityKey::from_bytes(arr32(ALICE_ID_HEX)),
        &arr16(ROOM_NONCE_HEX),
        ROOM_CREATED_AT,
    );
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    (
        WireEvent::seal(csb, sig).to_bytes(),
        ValidationContext::for_room(room_id),
    )
}

#[test]
fn unknown_schema_version_is_rejected() {
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    let csb = signed_bytes_with(2, "message.text", content, vec![]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::UnknownSchemaVersion)
    );
}

#[test]
fn unknown_event_type_is_rejected() {
    let content = CborValue::Map(vec![]);
    let csb = signed_bytes_with(1, "message.bogus", content, vec![]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::UnknownEventType)
    );
}

#[test]
fn unknown_content_key_is_rejected() {
    // A `message.text` content map carrying an extra, unknown key.
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("hi".to_owned())),
        ("surprise".to_owned(), CborValue::Uint(1)),
    ]);
    let csb = signed_bytes_with(1, "message.text", content, vec![]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- Causal structure checks (stateless). -------------------------------------

/// Placeholder `prev_event` entry: a 32-byte all-zero byte string, used wherever
/// a non-empty `prev_events` list is needed to satisfy non-genesis checks without
/// crafting a real ancestor event.
fn dummy_prev_event() -> CborValue {
    CborValue::Bytes(vec![0u8; 32])
}

/// Build a fully valid, signed `room.created` wire event with configurable
/// `admins` and `prev_events` lists. Used by negative causal/cross-field tests.
fn room_created_wire(
    admins: Vec<CborValue>,
    prev_events: Vec<CborValue>,
) -> (Vec<u8>, ValidationContext) {
    let id_secret = alice_identity_secret();
    let dev_secret = alice_device_secret();
    let alice_id = id_secret.identity_key();
    let alice_dev = dev_secret.device_key();
    let nonce = arr16(ROOM_NONCE_HEX);
    let room_id = signed::derive_room_id(&alice_id, &nonce, ROOM_CREATED_AT);
    let binding = DeviceBinding::create(&room_id, &id_secret, alice_dev);
    let content = CborValue::Map(vec![
        (
            "room_name".to_owned(),
            CborValue::Text("Test Room".to_owned()),
        ),
        ("room_nonce".to_owned(), CborValue::Bytes(nonce.to_vec())),
        ("admins".to_owned(), CborValue::Array(admins)),
        ("device_binding".to_owned(), binding.to_cbor()),
    ]);
    let map = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(1)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room_id.as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(alice_id.as_bytes().to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(alice_dev.as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("room.created".to_owned()),
        ),
        ("created_at".to_owned(), CborValue::Uint(ROOM_CREATED_AT)),
        ("prev_events".to_owned(), CborValue::Array(prev_events)),
        ("content".to_owned(), content),
    ]);
    let csb = cbor::encode(&map);
    let sig = signed::sign_csb(&csb, &dev_secret);
    let wire = WireEvent::seal(csb, sig).to_bytes();
    (wire, ValidationContext::for_room(room_id))
}

#[test]
fn too_many_parents_is_rejected() {
    // MAX_PREV_EVENTS is 20; 21 entries must produce TooManyParents.
    let prev: Vec<CborValue> = (0..21).map(|_| dummy_prev_event()).collect();
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    let csb = signed_bytes_with(1, "message.text", content, prev);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::TooManyParents)
    );
}

#[test]
fn genesis_with_nonempty_prev_events_is_rejected() {
    // A room.created with prev_events != [] violates the genesis-descent invariant.
    let admins = vec![CborValue::Bytes(arr32(ALICE_ID_HEX).to_vec())];
    let (bytes, ctx) = room_created_wire(admins, vec![dummy_prev_event()]);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NotGenesisDescended)
    );
}

// --- Advisory clock-skew flag. ------------------------------------------------

#[test]
fn clock_skew_flag_is_advisory_only() {
    // An event whose created_at is >5 min ahead of `now_ms` receives the
    // ClockSkew flag but is NOT rejected — the event itself is fully accepted.
    let (bytes, _csb, room_id) = genesis_wire_bytes();
    let ctx = ValidationContext {
        expected_room: room_id,
        // Supply a 'now' 301 seconds behind created_at to exceed the threshold.
        now_ms: Some(ROOM_CREATED_AT.saturating_sub(301_000)),
    };
    let validated =
        validate_wire_bytes(&bytes, &ctx).expect("clock-skewed genesis must still be accepted");
    assert_eq!(validated.flags, vec![Flag::ClockSkew]);
    assert_eq!(validated.event.event_type, EventType::RoomCreated);
}

#[test]
fn no_clock_skew_flag_within_threshold() {
    // created_at just one second behind `now_ms` — no flag.
    let (bytes, _csb, room_id) = genesis_wire_bytes();
    let ctx = ValidationContext {
        expected_room: room_id,
        now_ms: Some(ROOM_CREATED_AT + 1_000),
    };
    let validated = validate_wire_bytes(&bytes, &ctx).expect("must accept");
    assert!(validated.flags.is_empty());
}

// --- Signature checks. --------------------------------------------------------

#[test]
fn wrong_device_key_signs_is_rejected() {
    // Sign the event with a completely unrelated key, present the correct device_id
    // in the signed bytes (so the id check passes), then watch the sig check fail.
    let (bytes, _csb, room_id) = genesis_wire_bytes();
    let mut wire = WireEvent::decode(&bytes).unwrap();
    let impostor = SigningKey::from_seed(&[0x99; 32]);
    wire.sig = signed::sign_csb(&wire.signed, &impostor);
    // Re-seal to keep the advisory id consistent so we don't hit IdMismatch first.
    let resealed = WireEvent::seal(wire.signed, wire.sig);
    let ctx = ValidationContext::for_room(room_id);
    assert_eq!(
        validate_wire_bytes(&resealed.to_bytes(), &ctx),
        Err(RejectReason::BadSignature)
    );
}

// --- Content validation: length limits, enum bounds, required fields. ----------

#[test]
fn message_text_body_too_long_is_rejected() {
    // MAX_MESSAGE_BODY_BYTES is 16 384; 16 385 must be rejected.
    let long_body = "x".repeat(16_385);
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text(long_body))]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn message_text_invalid_format_enum_is_rejected() {
    // "html" is not in the accepted format enum ("plain" | "markdown").
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("hi".to_owned())),
        ("format".to_owned(), CborValue::Text("html".to_owned())),
    ]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn message_text_missing_required_body_is_rejected() {
    // A message.text with no "body" key violates the required-field check.
    let content = CborValue::Map(vec![(
        "format".to_owned(),
        CborValue::Text("plain".to_owned()),
    )]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn agent_status_progress_pct_over_100_is_rejected() {
    // progress_pct is bounded to 0..=100.
    let content = CborValue::Map(vec![
        ("status".to_owned(), CborValue::Text("running".to_owned())),
        ("progress_pct".to_owned(), CborValue::Uint(101)),
    ]);
    let csb = signed_bytes_with(1, "agent.status", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- Cross-field rules. -------------------------------------------------------

#[test]
fn room_created_empty_admins_is_rejected() {
    // admins MUST be exactly [sender_id] in MVP; [] violates the cross-field rule.
    let (bytes, ctx) = room_created_wire(vec![], vec![]);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn member_left_wrong_member_id_is_rejected() {
    // member.left requires member_id == sender_id; a different key is rejected.
    let other_key = [0x42u8; 32];
    let content = CborValue::Map(vec![(
        "member_id".to_owned(),
        CborValue::Bytes(other_key.to_vec()),
    )]);
    let csb = signed_bytes_with(1, "member.left", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- Device-binding checks. ---------------------------------------------------

#[test]
fn device_binding_identity_key_mismatch_is_rejected() {
    // room.created where device_binding.identity_key != sender_id is rejected
    // at the verify_bindings step, even though the content itself is well-formed.
    let id_secret = alice_identity_secret();
    let dev_secret = alice_device_secret();
    let alice_id = id_secret.identity_key();
    let alice_dev = dev_secret.device_key();
    let nonce = arr16(ROOM_NONCE_HEX);
    let room_id = signed::derive_room_id(&alice_id, &nonce, ROOM_CREATED_AT);
    // Create the binding signed by a different identity key.
    let other_id_secret = SigningKey::from_seed(&[0x03; 32]);
    let binding = DeviceBinding::create(&room_id, &other_id_secret, alice_dev);
    // binding.identity_key != alice_id, so check_binding will return InvalidContent.
    let content = CborValue::Map(vec![
        (
            "room_name".to_owned(),
            CborValue::Text("Test Room".to_owned()),
        ),
        ("room_nonce".to_owned(), CborValue::Bytes(nonce.to_vec())),
        (
            "admins".to_owned(),
            CborValue::Array(vec![CborValue::Bytes(alice_id.as_bytes().to_vec())]),
        ),
        ("device_binding".to_owned(), binding.to_cbor()),
    ]);
    let map = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(1)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room_id.as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(alice_id.as_bytes().to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(alice_dev.as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("room.created".to_owned()),
        ),
        ("created_at".to_owned(), CborValue::Uint(ROOM_CREATED_AT)),
        ("prev_events".to_owned(), CborValue::Array(vec![])),
        ("content".to_owned(), content),
    ]);
    let csb = cbor::encode(&map);
    let sig = signed::sign_csb(&csb, &dev_secret);
    let wire = WireEvent::seal(csb, sig);
    let ctx = ValidationContext::for_room(room_id);
    assert_eq!(
        validate_wire_bytes(&wire.to_bytes(), &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- Wire-envelope checks. ----------------------------------------------------

#[test]
fn empty_wire_bytes_are_rejected() {
    let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
    assert_eq!(
        validate_wire_bytes(&[], &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

#[test]
fn garbage_wire_bytes_are_rejected() {
    let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
    assert_eq!(
        validate_wire_bytes(b"not cbor at all", &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

#[test]
fn wire_version_two_is_rejected() {
    // WireEvent with v=2 is rejected at the envelope-decode step.
    let entries = vec![
        ("v".to_owned(), CborValue::Uint(2)),
        ("signed".to_owned(), CborValue::Bytes(vec![0xa0])), // empty CBOR map
        ("sig".to_owned(), CborValue::Bytes(vec![0u8; 64])),
        ("id".to_owned(), CborValue::Text("blake3:00".to_owned())),
    ];
    let bytes = cbor::encode(&CborValue::Map(entries));
    let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

// --- EventId / named-hash parse and format. -----------------------------------

#[test]
fn event_id_named_string_roundtrips() {
    let id: EventId = GOLDEN_EVENT_ID.parse().expect("must parse");
    assert_eq!(id.to_named_string(), GOLDEN_EVENT_ID);
}

#[test]
fn event_id_bad_prefix_parse_fails() {
    let result = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        .parse::<EventId>();
    assert_eq!(result, Err(HashParseError::MissingPrefix));
}

#[test]
fn event_id_bad_length_parse_fails() {
    // 31 bytes of hex (62 chars) — one byte short.
    let short = format!("blake3:{}", "00".repeat(31));
    let result = short.parse::<EventId>();
    assert!(matches!(result, Err(HashParseError::BadLength { .. })));
}

// --- member.removed cross-field rules. ----------------------------------------

#[test]
fn member_removed_self_removal_is_rejected() {
    // member_id == sender_id is the "don't remove yourself" invariant.
    let alice = arr32(ALICE_ID_HEX);
    let content = CborValue::Map(vec![
        ("member_id".to_owned(), CborValue::Bytes(alice.to_vec())),
        ("removed_by".to_owned(), CborValue::Bytes(alice.to_vec())),
    ]);
    let csb = signed_bytes_with(1, "member.removed", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn member_removed_wrong_removed_by_is_rejected() {
    // removed_by != sender_id; the admin identity must match the envelope sender.
    let other = [0x99u8; 32];
    let content = CborValue::Map(vec![
        ("member_id".to_owned(), CborValue::Bytes(other.to_vec())),
        ("removed_by".to_owned(), CborValue::Bytes(other.to_vec())),
    ]);
    let csb = signed_bytes_with(1, "member.removed", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- pipe.opened content validation. ------------------------------------------

fn pipe_opened_content(
    owner_id: &[u8; 32],
    kind: &str,
    allowed_members: Vec<CborValue>,
) -> CborValue {
    CborValue::Map(vec![
        ("pipe_id".to_owned(), CborValue::Bytes(vec![0u8; 16])),
        ("owner_id".to_owned(), CborValue::Bytes(owner_id.to_vec())),
        (
            "owner_endpoint".to_owned(),
            CborValue::Bytes(arr32(ALICE_DEV_HEX).to_vec()),
        ),
        ("kind".to_owned(), CborValue::Text(kind.to_owned())),
        ("label".to_owned(), CborValue::Text("test".to_owned())),
        (
            "target_hint".to_owned(),
            CborValue::Text("127.0.0.1".to_owned()),
        ),
        ("alpn".to_owned(), CborValue::Text("proto".to_owned())),
        (
            "allowed_members".to_owned(),
            CborValue::Array(allowed_members),
        ),
    ])
}

#[test]
fn pipe_opened_owner_id_mismatch_is_rejected() {
    // owner_id != sender_id violates the cross-field rule.
    let other = [0x99u8; 32];
    let alice_id = arr32(ALICE_ID_HEX);
    let content = pipe_opened_content(&other, "tcp", vec![CborValue::Bytes(alice_id.to_vec())]);
    let csb = signed_bytes_with(1, "pipe.opened", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn pipe_opened_non_tcp_kind_is_rejected() {
    let alice_id = arr32(ALICE_ID_HEX);
    let content = pipe_opened_content(&alice_id, "udp", vec![CborValue::Bytes(alice_id.to_vec())]);
    let csb = signed_bytes_with(1, "pipe.opened", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn pipe_opened_empty_allowed_members_is_rejected() {
    let alice_id = arr32(ALICE_ID_HEX);
    let content = pipe_opened_content(&alice_id, "tcp", vec![]);
    let csb = signed_bytes_with(1, "pipe.opened", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- member.invited and pipe.closed enum validation. --------------------------

#[test]
fn member_invited_unknown_role_is_rejected() {
    let content = CborValue::Map(vec![
        ("invite_id".to_owned(), CborValue::Bytes(vec![0u8; 16])),
        (
            "capability_hash".to_owned(),
            CborValue::Bytes(vec![0u8; 32]),
        ),
        ("role".to_owned(), CborValue::Text("superadmin".to_owned())),
        (
            "invitee_key".to_owned(),
            CborValue::Bytes(arr32(ALICE_ID_HEX).to_vec()),
        ),
    ]);
    let csb = signed_bytes_with(1, "member.invited", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn pipe_closed_unknown_reason_is_rejected() {
    let content = CborValue::Map(vec![
        ("pipe_id".to_owned(), CborValue::Bytes(vec![0u8; 16])),
        ("reason".to_owned(), CborValue::Text("abandoned".to_owned())),
    ]);
    let csb = signed_bytes_with(1, "pipe.closed", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::InvalidContent)
    );
}

// --- Boundary values. ---------------------------------------------------------

#[test]
fn message_text_body_at_max_length_is_accepted() {
    // MAX_MESSAGE_BODY_BYTES = 16_384; exactly 16 384 bytes must be accepted.
    let max_body = "x".repeat(16_384);
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text(max_body))]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert!(validate_wire_bytes(&bytes, &ctx).is_ok());
}

#[test]
fn agent_status_progress_pct_at_zero_is_accepted() {
    let content = CborValue::Map(vec![
        ("status".to_owned(), CborValue::Text("running".to_owned())),
        ("progress_pct".to_owned(), CborValue::Uint(0)),
    ]);
    let csb = signed_bytes_with(1, "agent.status", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert!(validate_wire_bytes(&bytes, &ctx).is_ok());
}

#[test]
fn agent_status_progress_pct_at_100_is_accepted() {
    let content = CborValue::Map(vec![
        ("status".to_owned(), CborValue::Text("done".to_owned())),
        ("progress_pct".to_owned(), CborValue::Uint(100)),
    ]);
    let csb = signed_bytes_with(1, "agent.status", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    assert!(validate_wire_bytes(&bytes, &ctx).is_ok());
}

#[test]
fn clock_skew_exactly_at_threshold_is_not_flagged() {
    // created_at == now + CLOCK_SKEW_FUTURE_MS: the check is >, so no flag.
    let (bytes, _csb, room_id) = genesis_wire_bytes();
    let ctx = ValidationContext {
        expected_room: room_id,
        now_ms: Some(ROOM_CREATED_AT.saturating_sub(300_000)),
    };
    let validated = validate_wire_bytes(&bytes, &ctx).expect("must accept");
    assert!(validated.flags.is_empty(), "no flag at the exact threshold");
}

#[test]
fn clock_skew_one_ms_over_threshold_is_flagged() {
    // created_at == now + 300_001 ms → strictly greater than threshold → flag.
    let (bytes, _csb, room_id) = genesis_wire_bytes();
    let ctx = ValidationContext {
        expected_room: room_id,
        now_ms: Some(ROOM_CREATED_AT.saturating_sub(300_001)),
    };
    let validated = validate_wire_bytes(&bytes, &ctx).expect("must accept");
    assert_eq!(validated.flags, vec![Flag::ClockSkew]);
}

#[test]
fn exactly_max_prev_events_is_accepted() {
    // MAX_PREV_EVENTS = 20; exactly 20 entries must be accepted (21 is rejected above).
    let prev: Vec<CborValue> = (0..20).map(|_| dummy_prev_event()).collect();
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    let csb = signed_bytes_with(1, "message.text", content, prev);
    let (bytes, ctx) = seal_and_room(csb);
    assert!(validate_wire_bytes(&bytes, &ctx).is_ok());
}

// --- WireEvent structural: missing and extra keys. ----------------------------

#[test]
fn wire_missing_id_field_is_rejected() {
    // Only 3 keys — "id" is absent; the decoder expects exactly 4.
    let entries = vec![
        ("v".to_owned(), CborValue::Uint(1)),
        ("signed".to_owned(), CborValue::Bytes(vec![0xa0])),
        ("sig".to_owned(), CborValue::Bytes(vec![0u8; 64])),
    ];
    let bytes = cbor::encode(&CborValue::Map(entries));
    let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

#[test]
fn wire_extra_key_is_rejected() {
    // 5 keys instead of 4 — an unknown extra field.
    let entries = vec![
        ("v".to_owned(), CborValue::Uint(1)),
        ("signed".to_owned(), CborValue::Bytes(vec![0xa0])),
        ("sig".to_owned(), CborValue::Bytes(vec![0u8; 64])),
        ("id".to_owned(), CborValue::Text("blake3:00".to_owned())),
        ("extra".to_owned(), CborValue::Uint(0)),
    ];
    let bytes = cbor::encode(&CborValue::Map(entries));
    let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

// --- prev_events wrong-type and wrong-length entries. -------------------------

#[test]
fn prev_event_text_entry_is_rejected() {
    // A text string in prev_events violates the bytes-only shape constraint.
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    let csb = signed_bytes_with(
        1,
        "message.text",
        content,
        vec![CborValue::Text("not-bytes".to_owned())],
    );
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

#[test]
fn prev_event_short_bytes_is_rejected() {
    // 31-byte entry — one byte short of the required 32-byte EventId.
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    let csb = signed_bytes_with(
        1,
        "message.text",
        content,
        vec![CborValue::Bytes(vec![0u8; 31])],
    );
    let (bytes, ctx) = seal_and_room(csb);
    assert_eq!(
        validate_wire_bytes(&bytes, &ctx),
        Err(RejectReason::NonCanonicalEncoding)
    );
}

// --- message.text optional fields are accepted. --------------------------------

#[test]
fn message_text_markdown_format_is_accepted() {
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("**bold**".to_owned())),
        ("format".to_owned(), CborValue::Text("markdown".to_owned())),
    ]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    let validated = validate_wire_bytes(&bytes, &ctx).expect("markdown must be accepted");
    match &validated.event.content {
        Content::MessageText(m) => assert_eq!(m.format.as_deref(), Some("markdown")),
        _ => panic!("unexpected content variant"),
    }
}

#[test]
fn message_text_in_reply_to_is_accepted() {
    let reply_target = [0x42u8; 32];
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("reply".to_owned())),
        (
            "in_reply_to".to_owned(),
            CborValue::Bytes(reply_target.to_vec()),
        ),
    ]);
    let csb = signed_bytes_with(1, "message.text", content, vec![dummy_prev_event()]);
    let (bytes, ctx) = seal_and_room(csb);
    let validated = validate_wire_bytes(&bytes, &ctx).expect("in_reply_to must be accepted");
    match &validated.event.content {
        Content::MessageText(m) => {
            let id = m.in_reply_to.as_ref().expect("in_reply_to must be set");
            assert_eq!(id.as_bytes(), &reply_target);
        }
        _ => panic!("unexpected content variant"),
    }
}

// --- SignedEvent::decode direct API. ------------------------------------------

#[test]
fn signed_event_decode_valid_csb_succeeds() {
    // decode() performs structural + content validation but not sig/room/causal checks.
    let csb = golden_event().to_csb();
    let decoded = SignedEvent::decode(&csb).expect("valid CSB must decode");
    assert_eq!(decoded.event_type, EventType::MessageText);
    assert_eq!(decoded.schema_version, 1);
}

#[test]
fn signed_event_decode_garbage_fails() {
    assert!(matches!(
        SignedEvent::decode(b"not cbor"),
        Err(RejectReason::NonCanonicalEncoding)
    ));
}

// --- Stable code() strings for RejectReason and Flag (Event Protocol §8). ----

#[test]
fn reject_reason_code_strings_match_spec() {
    assert_eq!(
        RejectReason::UnknownSchemaVersion.code(),
        "unknown_schema_version"
    );
    assert_eq!(RejectReason::UnknownEventType.code(), "unknown_event_type");
    assert_eq!(
        RejectReason::NonCanonicalEncoding.code(),
        "non_canonical_encoding"
    );
    assert_eq!(RejectReason::IdMismatch.code(), "id_mismatch");
    assert_eq!(RejectReason::BadSignature.code(), "bad_signature");
    assert_eq!(RejectReason::RoomIdMismatch.code(), "room_id_mismatch");
    assert_eq!(RejectReason::InvalidContent.code(), "invalid_content");
    assert_eq!(RejectReason::TooManyParents.code(), "too_many_parents");
    assert_eq!(
        RejectReason::NotGenesisDescended.code(),
        "not_genesis_descended"
    );
}

#[test]
fn flag_code_strings_match_spec() {
    assert_eq!(Flag::ClockSkew.code(), "clock_skew");
    assert_eq!(Flag::Equivocation.code(), "equivocation");
    assert_eq!(Flag::FromRemovedMember.code(), "from_removed_member");
}

// --- EventType registry round-trip. -------------------------------------------

#[test]
fn all_event_types_round_trip_registry_string() {
    let types = [
        (EventType::RoomCreated, "room.created"),
        (EventType::MemberInvited, "member.invited"),
        (EventType::MemberJoined, "member.joined"),
        (EventType::MemberLeft, "member.left"),
        (EventType::MemberRemoved, "member.removed"),
        (EventType::MessageText, "message.text"),
        (EventType::FileShared, "file.shared"),
        (EventType::PipeOpened, "pipe.opened"),
        (EventType::PipeClosed, "pipe.closed"),
        (EventType::AgentStatus, "agent.status"),
    ];
    for (ty, s) in &types {
        assert_eq!(ty.as_str(), *s, "as_str mismatch for {s}");
        assert_eq!(
            EventType::from_registry(s),
            Some(*ty),
            "from_registry mismatch for {s}"
        );
    }
    assert_eq!(EventType::from_registry("room.bogus"), None);
}
