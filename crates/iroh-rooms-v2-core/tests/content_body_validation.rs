#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::{self, CborValue};
use iroh_rooms_v2_core::content::{
    seal_content_event, validate_device_chain_link, verify_content_event, ContentEvent,
    ContentEventBody, ContentKind, VerifiedContentEvent, CONTENT_EVENT_VERSION,
    MAX_CONTENT_REFERENCES,
};
use iroh_rooms_v2_core::domain::{self, CONTENT_EVENT};
use iroh_rooms_v2_core::ids::{CommunityId, EventId, StreamId, LEN};
use iroh_rooms_v2_core::keys::{Signature, SigningKey};
use iroh_rooms_v2_core::Reject;

const GOLDEN_BODY_HEX: &str = "ac617602646b696e646c6d6573736167652e7465787467636f6e74656e74a264626f647974676f6c64656e20636f6e74656e74206576656e7466666f726d6174686d61726b646f776e69617574686f725f69645820d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737696465766963655f69645820a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f06973747265616d5f6964582071717171717171717171717171717171717171717171717171717171717171716a6465766963655f736571006a7265666572656e636573825820a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a15820a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a26c636f6d6d756e6974795f6964582070707070707070707070707070707070707070707070707070707070707070706d617574685f68696e745f736571076d637265617465645f61745f6d731b00000191a203220071707265765f6465766963655f6576656e74f6";
const GOLDEN_EVENT_ID_HEX: &str =
    "eee6da539856ebf3ff7906d6441e0e09bfdf853a7e9f6449f6d2e10b26ab3a3a";
const GOLDEN_SIGNATURE_HEX: &str = "6081998a852c3979879bb2e5a30e7e944826bdfa2805f311f3acd56b134f18f6abb8afb53767c79027cc3c624324e9dbc4057fce5c01202be9cba7055a79f502";

fn body_at(
    seq: u64,
    prev: Option<EventId>,
    author: &SigningKey,
    device: &SigningKey,
) -> ContentEventBody {
    ContentEventBody {
        v: CONTENT_EVENT_VERSION,
        community_id: CommunityId::from_bytes([0x70; LEN]),
        stream_id: StreamId::from_bytes([0x71; LEN]),
        author_id: author.member_id(),
        device_id: device.device_id(),
        device_seq: seq,
        prev_device_event: prev,
        auth_hint_seq: 7,
        created_at_ms: 1_725_000_000_000 + seq,
        kind: ContentKind::MessageText,
        references: Vec::new(),
        content: CborValue::Map(vec![
            (
                "body".to_owned(),
                CborValue::Text("golden content event".to_owned()),
            ),
            ("format".to_owned(), CborValue::Text("markdown".to_owned())),
        ]),
    }
}

fn golden_body() -> ContentEventBody {
    let author = SigningKey::from_seed(&[0x11; LEN]);
    let device = SigningKey::from_seed(&[0x22; LEN]);
    let mut body = body_at(0, None, &author, &device);
    body.references = vec![
        EventId::from_bytes([0xa1; LEN]),
        EventId::from_bytes([0xa2; LEN]),
    ];
    body
}

fn set_field(value: &mut CborValue, key: &str, replacement: CborValue) {
    let CborValue::Map(entries) = value else {
        panic!("body must be a map");
    };
    let field = entries
        .iter_mut()
        .find(|(name, _)| name == key)
        .expect("field must exist");
    field.1 = replacement;
}

// Seal + verify a successor body under its own device key, then link it against
// a verified predecessor. The full public trust boundary is exercised; the
// returned `Result` distinguishes chain rejection from acceptance.
fn seal_verify_link(
    succ: &ContentEventBody,
    key: &SigningKey,
    prev: &VerifiedContentEvent,
) -> Result<(), Reject> {
    let cur = verify_content_event(&seal_content_event(succ, key).unwrap())?;
    validate_device_chain_link(Some(prev), &cur)
}

#[test]
fn normative_body_matches_frozen_crypto_vector() {
    let device = SigningKey::from_seed(&[0x22; LEN]);
    let body = golden_body();
    let body_csb = body.encode_canonical();

    assert_eq!(hex::encode(&body_csb), GOLDEN_BODY_HEX);
    assert_eq!(CONTENT_EVENT, b"iroh-room-v2/content-event");

    let expected_id = hex::decode(GOLDEN_EVENT_ID_HEX).unwrap();
    let id = EventId::from_content_event_csb(&body_csb);
    assert_eq!(id.as_bytes().as_slice(), expected_id.as_slice());
    assert_eq!(
        domain::blake3_domain(b"iroh-room-v2/content-event", &body_csb).as_slice(),
        expected_id.as_slice()
    );

    let event = seal_content_event(&body, &device).unwrap();
    assert_eq!(event.body_csb, body_csb);
    assert_eq!(
        hex::encode(event.signature.as_bytes()),
        GOLDEN_SIGNATURE_HEX
    );

    let verified = verify_content_event(&event).unwrap();
    assert_eq!(verified.id(), id);
    assert_eq!(verified.body(), &body);
    assert_eq!(verified.body_csb(), body_csb);
}

#[test]
fn tampering_and_signature_changes_respect_exact_body_boundary() {
    let author = SigningKey::from_seed(&[0x11; LEN]);
    let device = SigningKey::from_seed(&[0x22; LEN]);
    let body = golden_body();
    let event = seal_content_event(&body, &device).unwrap();
    let original_id = EventId::from_content_event_csb(&event.body_csb);

    let mut tampered_body = body.clone();
    tampered_body.created_at_ms += 1;
    let tampered = ContentEvent::new(tampered_body.encode_canonical(), event.signature);
    assert_ne!(
        EventId::from_content_event_csb(&tampered.body_csb),
        original_id
    );
    assert_eq!(
        verify_content_event(&tampered).unwrap_err(),
        Reject::BadSignature
    );

    let resigned = seal_content_event(&body, &device).unwrap();
    assert_eq!(resigned.signature, event.signature);
    assert_eq!(
        EventId::from_content_event_csb(&resigned.body_csb),
        original_id
    );

    let mut signature_bytes = *event.signature.as_bytes();
    signature_bytes[0] ^= 1;
    let changed_signature = ContentEvent::new(
        event.body_csb.clone(),
        Signature::from_bytes(signature_bytes),
    );
    assert_eq!(
        EventId::from_content_event_csb(&changed_signature.body_csb),
        original_id
    );
    assert_eq!(
        verify_content_event(&changed_signature).unwrap_err(),
        Reject::BadSignature
    );

    let message = domain::signing_message(CONTENT_EVENT, &event.body_csb);
    let author_signed = ContentEvent::new(event.body_csb, author.sign(&message));
    assert_eq!(
        verify_content_event(&author_signed).unwrap_err(),
        Reject::BadSignature
    );
}

#[test]
fn references_enforce_cap_width_type_and_preserve_order() {
    for count in [0, 1, MAX_CONTENT_REFERENCES] {
        let mut body = golden_body();
        body.references = (0..count)
            .map(|index| EventId::from_bytes([u8::try_from(index).unwrap(); LEN]))
            .collect();
        let decoded = ContentEventBody::decode_from_csb(&body.encode_canonical()).unwrap();
        assert_eq!(decoded.references, body.references);
    }

    let mut body = golden_body();
    body.references = (0..=MAX_CONTENT_REFERENCES)
        .map(|index| EventId::from_bytes([u8::try_from(index).unwrap(); LEN]))
        .collect();
    assert_eq!(
        ContentEventBody::decode_from_csb(&body.encode_canonical()).unwrap_err(),
        Reject::InvalidContent
    );

    let duplicate = EventId::from_bytes([0xdd; LEN]);
    body.references = vec![
        EventId::from_bytes([0xee; LEN]),
        duplicate,
        duplicate,
        EventId::from_bytes([0xaa; LEN]),
    ];
    let decoded = ContentEventBody::decode_from_csb(&body.encode_canonical()).unwrap();
    assert_eq!(decoded.references, body.references);

    for invalid in [
        CborValue::Text("not-an-array".to_owned()),
        CborValue::Array(vec![CborValue::Uint(1)]),
        CborValue::Array(vec![CborValue::Bytes(vec![0; LEN - 1])]),
        CborValue::Array(vec![CborValue::Bytes(vec![0; LEN + 1])]),
    ] {
        let mut value = golden_body().to_cbor();
        set_field(&mut value, "references", invalid);
        assert_eq!(
            ContentEventBody::decode_from_csb(&cbor::encode(&value)).unwrap_err(),
            Reject::InvalidContent
        );
    }
}

#[test]
fn five_event_device_chain_validates_through_public_boundary() {
    let author = SigningKey::from_seed(&[0x31; LEN]);
    let device = SigningKey::from_seed(&[0x32; LEN]);
    let mut verified = Vec::<VerifiedContentEvent>::new();
    let mut prev = None;

    for seq in 0..5 {
        let body = body_at(seq, prev, &author, &device);
        let event = seal_content_event(&body, &device).unwrap();
        let current = verify_content_event(&event).unwrap();
        validate_device_chain_link(verified.last(), &current).unwrap();
        prev = Some(current.id());
        verified.push(current);
    }

    assert_eq!(verified.len(), 5);
    assert_eq!(verified[0].device_seq(), 0);
    assert_eq!(verified[0].prev_device_event(), None);
    for (index, event) in verified.iter().enumerate().skip(1) {
        assert_eq!(event.device_seq(), u64::try_from(index).unwrap());
        assert_eq!(event.prev_device_event(), Some(verified[index - 1].id()));
    }
}

// ---------------------------------------------------------------------------
// Spec §9.3 crypto matrix: domain-separation replay fence through the real
// content-event device-key verification path. `tests/v2_identifiers_e2e.rs`
// pins cross-domain isolation for the generic principal-signed record path;
// this closes the same gap for the content-event path, which verifies under
// the in-body `device_id` over the frozen `CONTENT_EVENT` domain.
// ---------------------------------------------------------------------------

#[test]
fn foreign_domain_signature_does_not_replay_under_content_event() {
    let author = SigningKey::from_seed(&[0x11; LEN]);
    let device = SigningKey::from_seed(&[0x22; LEN]);
    let body = golden_body();
    let body_csb = body.encode_canonical();

    // A signature over the SAME body bytes but under a DIFFERENT frozen §6.2
    // domain must not verify under the content-event domain. This exercises the
    // domain-separation fence end to end through the real device-key verifier.
    let foreign_msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &body_csb);
    let replayed = ContentEvent::new(body_csb.clone(), device.sign(&foreign_msg));
    assert_eq!(
        verify_content_event(&replayed).unwrap_err(),
        Reject::BadSignature,
        "a signature over a foreign domain must not verify under content-event"
    );

    // The content-event domain prefix must be exactly the frozen #134 §6.2
    // string (regression fence against drift back to a legacy candidate).
    assert_eq!(CONTENT_EVENT, b"iroh-room-v2/content-event");
    assert_ne!(CONTENT_EVENT, domain::GOVERNANCE_ENTRY);

    // Sanity: the same body signed under the content-event domain verifies, and
    // the author key (not the device key) is still rejected (§9.3 wrong-key).
    let legit = seal_content_event(&body, &device).unwrap();
    verify_content_event(&legit).unwrap();
    let author_msg = domain::signing_message(CONTENT_EVENT, &body_csb);
    let author_signed = ContentEvent::new(body_csb, author.sign(&author_msg));
    assert_eq!(
        verify_content_event(&author_signed).unwrap_err(),
        Reject::BadSignature,
    );
}

// ---------------------------------------------------------------------------
// Spec §9.3 / §7 validation order: a wrong-width `device_id` is a SCHEMA fault
// (`InvalidContent`, before any crypto), while an exact-32-byte `device_id`
// that is not a valid Ed25519 point reaches the crypto layer (`BadSignature`).
// Pins the schema-before-signature ordering of `verify_content_event`.
// ---------------------------------------------------------------------------

#[test]
fn device_id_width_and_point_split_schema_and_crypto_layers() {
    let device = SigningKey::from_seed(&[0x22; LEN]);
    let body = golden_body();
    let sealed = seal_content_event(&body, &device).unwrap();

    // A wrong-width device_id (31 or 33 bytes) rejects at the schema layer
    // regardless of the retained signature.
    for bad_len in [LEN - 1, LEN + 1] {
        let mut value = body.to_cbor();
        set_field(
            &mut value,
            "device_id",
            CborValue::Bytes(vec![0u8; bad_len]),
        );
        let bad_csb = cbor::encode(&value);
        assert_eq!(
            verify_content_event(&ContentEvent::new(bad_csb, sealed.signature)).unwrap_err(),
            Reject::InvalidContent,
            "wrong-width ({bad_len}) device_id must reject at the schema layer"
        );
    }

    // An exact-32-byte device_id that is not a valid Ed25519 point passes the
    // schema width check and is rejected only at the crypto layer. The retained
    // golden signature was computed over different body bytes (and a different
    // verifying key), so verification cannot succeed.
    let mut value = body.to_cbor();
    set_field(&mut value, "device_id", CborValue::Bytes(vec![0u8; LEN]));
    let zero_dev_csb = cbor::encode(&value);
    assert_eq!(
        verify_content_event(&ContentEvent::new(zero_dev_csb, sealed.signature)).unwrap_err(),
        Reject::BadSignature,
        "exact-width non-point device_id must reach the crypto layer"
    );
}

// ---------------------------------------------------------------------------
// Spec §9.4 chain matrix: a successor must share community_id, device_id, and
// author_id with its verified predecessor and claim exactly
// `prev.device_seq + 1`. Each broken continuity invariant rejects as
// `InvalidContent` through the full seal -> verify -> link pipeline. These are
// the security invariants enforced by `validate_device_chain_link` that the
// five-event happy path does not exercise.
// ---------------------------------------------------------------------------

#[test]
fn chain_rejects_cross_community_author_device_and_sequence_gap() {
    let author = SigningKey::from_seed(&[0x51; LEN]);
    let device = SigningKey::from_seed(&[0x52; LEN]);
    let other_author = SigningKey::from_seed(&[0x53; LEN]);
    let other_device = SigningKey::from_seed(&[0x54; LEN]);

    // Verified genesis predecessor (device_seq = 0, null prev).
    let body0 = body_at(0, None, &author, &device);
    let v0 = verify_content_event(&seal_content_event(&body0, &device).unwrap()).unwrap();

    // 1. Cross-community successor: different community_id than the predecessor.
    let mut xcomm = body_at(1, Some(v0.id()), &author, &device);
    xcomm.community_id = CommunityId::from_bytes([0xcc; LEN]);
    assert_eq!(
        seal_verify_link(&xcomm, &device, &v0).unwrap_err(),
        Reject::InvalidContent,
        "cross-community successor must reject"
    );

    // 2. Cross-author successor: same device, different author_id in the body.
    let xauthor = body_at(1, Some(v0.id()), &other_author, &device);
    assert_eq!(
        seal_verify_link(&xauthor, &device, &v0).unwrap_err(),
        Reject::InvalidContent,
        "cross-author successor must reject"
    );

    // 3. Cross-device successor: sealed by a different device (its signature
    //    verifies under its own device_id) but naming v0 as predecessor.
    let xdevice = body_at(1, Some(v0.id()), &author, &other_device);
    assert_eq!(
        seal_verify_link(&xdevice, &other_device, &v0).unwrap_err(),
        Reject::InvalidContent,
        "cross-device successor must reject"
    );

    // 4. Sequence gap: claims device_seq 2 instead of predecessor.seq + 1 (= 1).
    let gap = body_at(2, Some(v0.id()), &author, &device);
    assert_eq!(
        seal_verify_link(&gap, &device, &v0).unwrap_err(),
        Reject::InvalidContent,
        "out-of-order sequence must reject"
    );

    // Sanity: a correctly-linked successor validates end to end.
    let good = body_at(1, Some(v0.id()), &author, &device);
    seal_verify_link(&good, &device, &v0).expect("valid successor link must accept");
}
