//! Vectors §1–§7 — the byte-exact, stateless serialization/signature layer, plus
//! the ported stateless taxonomy cases (`unknown_schema_version`,
//! `unknown_event_type`, `invalid_content`, `too_many_parents`,
//! `not_genesis_descended`) that feed the §8 completeness gate.
//!
//! Every `event_id`/`room_id`/signature asserted here is a **Tier-1** value
//! independently reproduced by the spike (and already round-tripping in
//! `golden_vectors.rs`); a mismatch is a hard NO-GO.

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::cbor::{self, CborValue};
use iroh_rooms_core::event::content::{Content, EventType, RoomCreated};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::DeviceKey;
use iroh_rooms_core::event::reject::RejectReason;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;

use super::fixtures;

// ---------------------------------------------------------------------------
// Golden-event field map + tiny raw-CBOR emitter (for the non-canonical cases
// §2 that the shared canonical encoder cannot produce by construction).
// ---------------------------------------------------------------------------

/// The eight logical fields of the golden `message.text` event as `(key, value)`
/// entries, in §2 declaration order (NOT canonical order).
fn golden_entries() -> Vec<(String, CborValue)> {
    vec![
        ("schema_version".to_owned(), CborValue::Uint(1)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(fixtures::room_id().as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(fixtures::alice_id().as_bytes().to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(fixtures::alice_dev().as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("message.text".to_owned()),
        ),
        (
            "created_at".to_owned(),
            CborValue::Uint(fixtures::GOLDEN_CREATED_AT),
        ),
        ("prev_events".to_owned(), CborValue::Array(vec![])),
        (
            "content".to_owned(),
            CborValue::Map(vec![
                ("body".to_owned(), CborValue::Text("Hello room".to_owned())),
                ("format".to_owned(), CborValue::Text("plain".to_owned())),
            ]),
        ),
    ]
}

/// Entries sorted into the encoder's canonical key order (length-first, then
/// bytewise) — the same order [`cbor::encode`] emits.
fn canonical_ordered(mut entries: Vec<(String, CborValue)>) -> Vec<(String, CborValue)> {
    entries.sort_by(|a, b| {
        a.0.len()
            .cmp(&b.0.len())
            .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
    });
    entries
}

/// Convert an in-memory length to a CBOR head argument (mirrors `cbor.rs`).
fn len_arg(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// Write a CBOR item head (major type + shortest-form argument) — copied from the
/// production encoder so the emitted values match byte-for-byte.
fn write_head(major: u8, arg: u64, out: &mut Vec<u8>) {
    let high = major << 5;
    let be = arg.to_be_bytes();
    match arg {
        0..=0x17 => out.push(high | be[7]),
        0x18..=0xFF => {
            out.push(high | 0x18);
            out.push(be[7]);
        }
        0x100..=0xFFFF => {
            out.push(high | 0x19);
            out.extend_from_slice(&be[6..8]);
        }
        0x1_0000..=0xFFFF_FFFF => {
            out.push(high | 0x1A);
            out.extend_from_slice(&be[4..8]);
        }
        _ => {
            out.push(high | 0x1B);
            out.extend_from_slice(&be);
        }
    }
}

fn emit_key(key: &str, out: &mut Vec<u8>) {
    write_head(3, len_arg(key.len()), out);
    out.extend_from_slice(key.as_bytes());
}

/// Emit a map with entries in the GIVEN order (no canonical re-sort), each value
/// encoded canonically. The map head declares `entries.len()`.
fn emit_map_in_order(entries: &[(String, CborValue)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_head(5, len_arg(entries.len()), &mut out);
    for (k, v) in entries {
        emit_key(k, &mut out);
        out.extend_from_slice(&cbor::encode(v));
    }
    out
}

/// Seal signed bytes (signature by `alice_dev`, whose key is the golden
/// `device_id`) and run the stateless pipeline for room A.
fn validate_signed(signed: Vec<u8>) -> Result<ValidatedEvent, RejectReason> {
    let sig = signed::sign_csb(&signed, &fixtures::alice_dev_sk());
    let wire = WireEvent::seal(signed, sig);
    validate_wire_bytes(
        &wire.to_bytes(),
        &ValidationContext::for_room(fixtures::room_id()),
    )
}

// The five §2 non-canonical sub-cases (a–e), each isolating a single defect.

/// (a) top-level key order differs (declaration order ≠ canonical order).
fn noncanonical_a_key_order() -> Vec<u8> {
    emit_map_in_order(&golden_entries())
}

/// (b) an indefinite-length item (the top-level map made indefinite).
fn noncanonical_b_indefinite() -> Vec<u8> {
    let mut bytes = cbor::encode(&CborValue::Map(golden_entries())); // canonical `a8 …`
    bytes[0] = 0xbf; // map(*) indefinite
    bytes.push(0xff); // break
    bytes
}

/// (c) a non-shortest integer (`schema_version` in the 8-byte form of `1`).
fn noncanonical_c_non_shortest_int() -> Vec<u8> {
    let entries = canonical_ordered(golden_entries());
    let mut out = Vec::new();
    write_head(5, 8, &mut out);
    for (k, v) in &entries {
        emit_key(k, &mut out);
        if k == "schema_version" {
            out.push(0x1b); // uint, 8-byte argument (non-shortest for 1)
            out.extend_from_slice(&v.as_uint().expect("schema_version is a uint").to_be_bytes());
        } else {
            out.extend_from_slice(&cbor::encode(v));
        }
    }
    out
}

/// (d) a ninth top-level key (`"nonce"`) — canonical bytes that fail the
/// exact-eight-keys check.
fn noncanonical_d_ninth_key() -> Vec<u8> {
    let mut entries = golden_entries();
    entries.push(("nonce".to_owned(), CborValue::Uint(0)));
    cbor::encode(&CborValue::Map(entries)) // canonical 9-key map
}

/// (e) a duplicate map key (`schema_version` emitted twice, adjacent).
fn noncanonical_e_duplicate_key() -> Vec<u8> {
    let entries = canonical_ordered(golden_entries());
    let mut out = Vec::new();
    write_head(5, 9, &mut out); // 8 real entries + 1 duplicate
    for (k, v) in &entries {
        emit_key(k, &mut out);
        out.extend_from_slice(&cbor::encode(v));
    }
    // `schema_version` is last in canonical order; repeat it adjacently.
    let (k, v) = entries.last().expect("non-empty entries");
    emit_key(k, &mut out);
    out.extend_from_slice(&cbor::encode(v));
    out
}

// ---------------------------------------------------------------------------
// Helper for the ported stateless taxonomy cases (unknown version/type, content).
// ---------------------------------------------------------------------------

/// Build a canonically-encoded, `alice_dev`-signed event from raw top-level parts
/// and validate it for room A. Lets a single-defect negative case still pass the
/// id/canonical/signature steps and reach the version/type/content checks.
fn parts_result(
    schema_version: u64,
    event_type: &str,
    content: CborValue,
    prev_events: Vec<CborValue>,
) -> Result<ValidatedEvent, RejectReason> {
    let map = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(schema_version)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(fixtures::room_id().as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(fixtures::alice_id().as_bytes().to_vec()),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(fixtures::alice_dev().as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text(event_type.to_owned()),
        ),
        (
            "created_at".to_owned(),
            CborValue::Uint(fixtures::GOLDEN_CREATED_AT),
        ),
        ("prev_events".to_owned(), CborValue::Array(prev_events)),
        ("content".to_owned(), content),
    ]);
    validate_signed(cbor::encode(&map))
}

/// A 32-byte all-zero placeholder parent, to satisfy the non-genesis check.
fn dummy_prev() -> CborValue {
    CborValue::Bytes(vec![0u8; 32])
}

// ===========================================================================
// §1 — Canonical-serialization determinism.
//
// THEN: two builds (declaration order and scrambled order) produce byte-identical
// 242-byte CSB beginning `a867636f6e74656e74…01`, encoder-choice-independent.
// ===========================================================================

#[test]
fn vector_01_canonical_serialization_determinism() {
    let golden = fixtures::golden_event().to_csb();

    // 242 bytes, canonical prefix, and schema_version=1 as the last byte.
    assert_eq!(golden.len(), 242, "golden CSB must be exactly 242 bytes");
    assert!(
        hex::encode(&golden).starts_with("a867636f6e74656e74a264626f6479"),
        "unexpected CSB prefix: {}",
        hex::encode(&golden)
    );
    assert_eq!(
        golden.last(),
        Some(&0x01),
        "CSB must end with schema_version=1"
    );

    // Declaration order and a scrambled order both canonicalize to the same bytes.
    let decl = cbor::encode(&CborValue::Map(golden_entries()));
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
        (
            "created_at".to_owned(),
            CborValue::Uint(fixtures::GOLDEN_CREATED_AT),
        ),
        (
            "device_id".to_owned(),
            CborValue::Bytes(fixtures::alice_dev().as_bytes().to_vec()),
        ),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(fixtures::alice_id().as_bytes().to_vec()),
        ),
        (
            "room_id".to_owned(),
            CborValue::Bytes(fixtures::room_id().as_bytes().to_vec()),
        ),
    ]);
    assert_eq!(decl, golden, "declaration-order map must equal golden CSB");
    assert_eq!(
        cbor::encode(&scrambled),
        golden,
        "scrambled-order map must canonicalize to the identical CSB"
    );

    // Sanity: the local raw emitter reproduces the canonical CSB when fed the
    // canonical key order — proof the §2 emitter's only deviations are the ones
    // it deliberately injects.
    assert_eq!(
        emit_map_in_order(&canonical_ordered(golden_entries())),
        golden,
        "raw emitter must reproduce canonical CSB in canonical key order"
    );
}

// ===========================================================================
// §2 — Non-canonical encoding is rejected (all five sub-cases a–e).
// ===========================================================================

#[test]
fn vector_02_non_canonical_encoding_rejected() {
    let cases: [(&str, Vec<u8>); 5] = [
        ("a: top-level key order differs", noncanonical_a_key_order()),
        ("b: indefinite-length item", noncanonical_b_indefinite()),
        ("c: non-shortest integer", noncanonical_c_non_shortest_int()),
        ("d: ninth top-level key", noncanonical_d_ninth_key()),
        ("e: duplicate map key", noncanonical_e_duplicate_key()),
    ];
    for (label, signed) in cases {
        assert_eq!(
            validate_signed(signed),
            Err(RejectReason::NonCanonicalEncoding),
            "sub-case {label} must be rejected as non_canonical_encoding"
        );
    }
}

// ===========================================================================
// §3 — event_id == "blake3:" + hex(BLAKE3-256(CSB)), and `id` is recomputed.
// ===========================================================================

#[test]
fn vector_03_event_id_is_recomputed() {
    let csb = fixtures::golden_event().to_csb();
    assert_eq!(
        signed::event_id_from_bytes(&csb).to_named_string(),
        fixtures::GOLDEN_EVENT_ID
    );
    assert_eq!(
        fixtures::golden_event().event_id().to_named_string(),
        fixtures::GOLDEN_EVENT_ID
    );

    // A doctored advisory `id` over the exact golden bytes ⇒ id_mismatch (step 2,
    // before the genesis-descent check the empty-prev golden would otherwise hit).
    let sig = signed::sign_csb(&csb, &fixtures::alice_dev_sk());
    let mut wire = WireEvent::seal(csb, sig);
    wire.id = format!("blake3:{}", "00".repeat(32));
    assert_eq!(
        validate_wire_bytes(
            &wire.to_bytes(),
            &ValidationContext::for_room(fixtures::room_id())
        ),
        Err(RejectReason::IdMismatch)
    );
}

// ===========================================================================
// §4 — room_id derivation is recomputed and bound (genesis).
// ===========================================================================

#[test]
fn vector_04_room_id_derivation_bound() {
    // Positive: the derivation reproduces room_id_A.
    assert_eq!(
        hex::encode(fixtures::room_id().as_bytes()),
        fixtures::ROOM_ID_A_HEX
    );

    // Negative: a forged genesis whose envelope room_id ≠ the recomputed genesis
    // id (a vanity-id attempt) ⇒ room_id_mismatch.
    let alice_id = fixtures::alice_id_sk();
    let forged = RoomId::from_bytes([0xee; 32]);
    let binding = DeviceBinding::create(&forged, &alice_id, fixtures::alice_dev());
    let event = SignedEvent {
        schema_version: 1,
        room_id: forged,
        sender_id: fixtures::alice_id(),
        device_id: fixtures::alice_dev(),
        event_type: EventType::RoomCreated,
        created_at: fixtures::T_ROOM,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Vanity".to_owned(),
            room_nonce: fixtures::ROOM_NONCE,
            admins: vec![fixtures::alice_id()],
            device_binding: binding,
        }),
    };
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, &fixtures::alice_dev_sk());
    let bytes = WireEvent::seal(csb, sig).to_bytes();
    // Process it in its own (forged) room context so the failure is the
    // derived-≠-envelope check, not the processing-room check.
    assert_eq!(
        validate_wire_bytes(&bytes, &ValidationContext::for_room(forged)),
        Err(RejectReason::RoomIdMismatch)
    );
}

// ===========================================================================
// §5 — Signature accept (device key) / reject (sender key).
// ===========================================================================

#[test]
fn vector_05_signature_under_device_key() {
    let csb = fixtures::golden_event().to_csb();
    let sig = signed::sign_csb(&csb, &fixtures::alice_dev_sk());
    let msg = signed::event_signing_message(&csb);

    // Correct device key accepts.
    assert!(fixtures::alice_dev().verify(&msg, &sig).is_ok());

    // The classic "verify under sender_id" bug: identity bytes as a device key fail.
    let sender_as_device = DeviceKey::from_bytes(*fixtures::alice_id().as_bytes());
    assert!(sender_as_device.verify(&msg, &sig).is_err());

    // Deterministic Ed25519 reproduces the golden signature prefix/suffix.
    let sig_hex = hex::encode(sig.as_bytes());
    assert!(sig_hex.starts_with("98732ece"), "sig = {sig_hex}");
    assert!(sig_hex.ends_with("4f0f"), "sig = {sig_hex}");
}

// ===========================================================================
// §6 — Tampered field ⇒ signature fails and identity changes.
// ===========================================================================

#[test]
fn vector_06_tampered_field_breaks_id_and_signature() {
    // (identity) the tampered body reproduces the pinned tampered id.
    let tampered_csb = fixtures::golden_event_tampered().to_csb();
    assert_eq!(
        signed::event_id_from_bytes(&tampered_csb).to_named_string(),
        fixtures::TAMPERED_EVENT_ID
    );

    let original_csb = fixtures::golden_event().to_csb();
    let original_sig = signed::sign_csb(&original_csb, &fixtures::alice_dev_sk());
    let ctx = ValidationContext::for_room(fixtures::room_id());

    // (a) tampered bytes carrying the ORIGINAL advisory id ⇒ id_mismatch (step 2).
    let original_id = signed::event_id_from_bytes(&original_csb).to_named_string();
    let mut wire_a = WireEvent::seal(tampered_csb.clone(), original_sig);
    wire_a.id = original_id;
    assert_eq!(
        validate_wire_bytes(&wire_a.to_bytes(), &ctx),
        Err(RejectReason::IdMismatch)
    );

    // (b) tampered bytes correctly re-hashed but carrying the ORIGINAL signature
    // ⇒ bad_signature (step 3), reached because the recomputed id now matches.
    let wire_b = WireEvent::seal(tampered_csb, original_sig);
    assert_eq!(
        validate_wire_bytes(&wire_b.to_bytes(), &ctx),
        Err(RejectReason::BadSignature)
    );
}

// ===========================================================================
// §7 — Cross-room replay fails; re-signing in room B changes the id.
// ===========================================================================

#[test]
fn vector_07_cross_room_replay_rejected() {
    // The verbatim golden WireEvent (room_id_A inside CSB) replayed into room B.
    let csb = fixtures::golden_event().to_csb();
    let sig = signed::sign_csb(&csb, &fixtures::alice_dev_sk());
    let bytes = WireEvent::seal(csb, sig).to_bytes();
    assert_eq!(
        validate_wire_bytes(&bytes, &ValidationContext::for_room(fixtures::room_id_b())),
        Err(RejectReason::RoomIdMismatch)
    );

    // Legitimately authoring "the same message" in room B changes room_id inside
    // the CSB, which changes the event_id to the pinned cross-room value.
    let mut in_room_b = fixtures::golden_event();
    in_room_b.room_id = fixtures::room_id_b();
    assert_eq!(
        in_room_b.event_id().to_named_string(),
        fixtures::CROSS_ROOM_EVENT_ID
    );
}

// ===========================================================================
// Ported stateless taxonomy cases (feed the §8 completeness gate):
// unknown_schema_version, unknown_event_type, invalid_content,
// too_many_parents, not_genesis_descended.
// ===========================================================================

#[test]
fn unknown_schema_version_is_rejected() {
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    assert_eq!(
        parts_result(2, "message.text", content, vec![dummy_prev()]),
        Err(RejectReason::UnknownSchemaVersion)
    );
}

#[test]
fn unknown_event_type_is_rejected() {
    assert_eq!(
        parts_result(
            1,
            "message.bogus",
            CborValue::Map(vec![]),
            vec![dummy_prev()]
        ),
        Err(RejectReason::UnknownEventType)
    );
}

#[test]
fn invalid_content_over_length_body_is_rejected() {
    // MAX_MESSAGE_BODY_BYTES is 16 384; 16 385 must be rejected as invalid_content.
    let long = "x".repeat(16_385);
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text(long))]);
    assert_eq!(
        parts_result(1, "message.text", content, vec![dummy_prev()]),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn invalid_content_unknown_key_is_rejected() {
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("hi".to_owned())),
        ("surprise".to_owned(), CborValue::Uint(1)),
    ]);
    assert_eq!(
        parts_result(1, "message.text", content, vec![dummy_prev()]),
        Err(RejectReason::InvalidContent)
    );
}

/// A structurally valid but semantically incorrect device-binding map.
/// The signature bytes are all-zero (invalid), but `check_field_rules` fails on
/// wrong `admins` BEFORE `verify_bindings` ever runs — so no valid sig is needed.
fn fake_device_binding() -> CborValue {
    CborValue::Map(vec![
        (
            "device_key".to_owned(),
            CborValue::Bytes(fixtures::alice_dev().as_bytes().to_vec()),
        ),
        (
            "identity_key".to_owned(),
            CborValue::Bytes(fixtures::alice_id().as_bytes().to_vec()),
        ),
        ("sig".to_owned(), CborValue::Bytes(vec![0u8; 64])),
    ])
}

#[test]
fn invalid_content_message_text_bad_format_enum() {
    // format: "xml" is not in ["plain", "markdown"] ⇒ invalid_content.
    let content = CborValue::Map(vec![
        ("body".to_owned(), CborValue::Text("hi".to_owned())),
        ("format".to_owned(), CborValue::Text("xml".to_owned())),
    ]);
    assert_eq!(
        parts_result(1, "message.text", content, vec![dummy_prev()]),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn invalid_content_agent_status_pct_over_100() {
    // progress_pct: 101 exceeds the 0..=100 bound ⇒ invalid_content.
    // The spec taxonomy matrix (§7) explicitly lists pct>100 as an invalid_content case.
    let content = CborValue::Map(vec![
        ("progress_pct".to_owned(), CborValue::Uint(101)),
        ("status".to_owned(), CborValue::Text("running".to_owned())),
    ]);
    assert_eq!(
        parts_result(1, "agent.status", content, vec![dummy_prev()]),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn invalid_content_room_created_wrong_admins() {
    // A room.created event where admins = [bob_id] instead of [sender_id = alice_id]
    // ⇒ invalid_content via check_field_rules, before the room_id / binding checks.
    // The spike taxonomy matrix (§7) lists "empty admins" as an explicit case;
    // wrong-identity admins hits the same `check_field_rules` branch.
    let content = CborValue::Map(vec![
        (
            "admins".to_owned(),
            CborValue::Array(vec![CborValue::Bytes(
                fixtures::bob_id().as_bytes().to_vec(),
            )]),
        ),
        ("device_binding".to_owned(), fake_device_binding()),
        ("room_name".to_owned(), CborValue::Text("test".to_owned())),
        (
            "room_nonce".to_owned(),
            CborValue::Bytes(fixtures::ROOM_NONCE.to_vec()),
        ),
    ]);
    let map = CborValue::Map(vec![
        ("content".to_owned(), content),
        ("created_at".to_owned(), CborValue::Uint(fixtures::T_ROOM)),
        (
            "device_id".to_owned(),
            CborValue::Bytes(fixtures::alice_dev().as_bytes().to_vec()),
        ),
        (
            "event_type".to_owned(),
            CborValue::Text("room.created".to_owned()),
        ),
        ("prev_events".to_owned(), CborValue::Array(vec![])),
        (
            "room_id".to_owned(),
            CborValue::Bytes(fixtures::room_id().as_bytes().to_vec()),
        ),
        ("schema_version".to_owned(), CborValue::Uint(1)),
        (
            "sender_id".to_owned(),
            CborValue::Bytes(fixtures::alice_id().as_bytes().to_vec()),
        ),
    ]);
    assert_eq!(
        validate_signed(cbor::encode(&map)),
        Err(RejectReason::InvalidContent)
    );
}

#[test]
fn too_many_parents_is_rejected() {
    // MAX_PREV_EVENTS is 20; 21 entries ⇒ too_many_parents.
    let prev: Vec<CborValue> = (0..21).map(|_| dummy_prev()).collect();
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    assert_eq!(
        parts_result(1, "message.text", content, prev),
        Err(RejectReason::TooManyParents)
    );
}

#[test]
fn not_genesis_descended_empty_prev_is_rejected() {
    // A non-genesis message.text with empty prev_events ⇒ not_genesis_descended.
    let content = CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]);
    assert_eq!(
        parts_result(1, "message.text", content, vec![]),
        Err(RejectReason::NotGenesisDescended)
    );
}
