//! Pure `file.shared` event assembly: build and sign a reference to a
//! content-addressed blob (Event Protocol §7).
//!
//! This is the single byte-exact place a `file.shared` event is assembled from a
//! member's keys — the sibling of [`build_message_text`](super::message::build_message_text)
//! and [`build_pipe_opened`](super::pipe::build_pipe_opened). It is
//! **deterministic** in its inputs: the caller injects the `file_id` (16 CSPRNG
//! bytes), the `blob_hash` (the BLAKE3-256 digest the durable blob store returned
//! for the imported content), the `prev_events` (the room heads), and `created_at`
//! (a clock read), so this function is itself clock-/RNG-free and golden-testable
//! (the only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! `file.shared` carries **no** embedded `device_binding`: it is a
//! membership-device-bound type (`requires_membership_device_binding == true`), so
//! the author's device is resolved from the membership fold rather than from the
//! event itself. The event is signed by the author's **device** secret; the
//! signature MUST verify under `device_id`, never `sender_id` — identical to
//! `message.text`.
//!
//! The blob **bytes** themselves are never carried on the log (PRD §9.2: blobs are
//! referenced by events, never inlined). This builder only assembles the signed
//! *reference*: the `file_id` handle, the human `name`/`mime_type`, the
//! `size_bytes`, the `blob_hash` digest, the `blob_format` (`raw` for MVP), and the
//! asserted `providers` set. Serving the bytes over the blobs ALPN is the follow-up
//! serve/fetch issue (spec IR-0202 §4.3).

use super::constants::{SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{Content, EventType, FileShared};
use super::ids::{EventId, HashRef, RoomId};
use super::keys::{DeviceKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a member-authored `file.shared` event (Event Protocol §7).
///
/// The `sender_identity_secret` provides `sender_id` (the authorizing membership
/// identity); the `sender_device_secret` signs the event (the signature MUST
/// verify under `device_id`). The two are passed separately, mirroring
/// [`build_message_text`](super::message::build_message_text).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// The `file_id`, `blob_hash`, `prev_events` (the room heads) and `created_at` (a
/// clock read) are injected by the caller so this stays free of wall-clock and RNG.
/// Optional fields follow the §7 omit-when-empty rule: `blob_format == None` is
/// omitted (defaults to `raw` on read) and an empty `providers` slice is omitted
/// entirely (rather than encoding an empty array).
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_message_text; each arg is a distinct signed field
pub fn build_file_shared(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    file_id: [u8; SHORT_ID_LEN],
    name: &str,
    mime_type: &str,
    size_bytes: u64,
    blob_hash: HashRef,
    blob_format: Option<&str>,
    providers: &[DeviceKey],
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = sender_identity_secret.identity_key();
    let device_id = sender_device_secret.device_key();

    let content = Content::FileShared(FileShared {
        file_id,
        name: name.to_owned(),
        mime_type: mime_type.to_owned(),
        size_bytes,
        blob_hash,
        blob_format: blob_format.map(ToOwned::to_owned),
        providers: (!providers.is_empty()).then(|| providers.to_vec()),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::FileShared,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, sender_device_secret);
    WireEvent::seal(csb, sig)
}

#[cfg(test)]
mod tests {
    use super::build_file_shared;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, HashRef, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (mirrors `message.rs`). These are
    // implementation-pinned regression locks, not published conformance vectors.
    const SENDER_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const SENDER_DEVICE_SEED: [u8; 32] = [0x02; 32];
    // A second device, used to populate a non-empty `providers` set.
    const PROVIDER_DEVICE_SEED: [u8; 32] = [0x03; 32];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    const FILE_ID: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    const NAME: &str = "report.pdf";
    const MIME: &str = "application/pdf";
    const SIZE: u64 = 204_800;
    const BLOB_HASH: [u8; 32] = [0xbb; 32];
    // Implementation-pinned regression lock: the event id our builder produces for
    // the fixtures below. Recompute & update only on an intentional byte-format
    // change to `file.shared` (such a change is breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "4f3d5d0c12e64d8813c219792d7ad79ce251b10772a797a66eb3ff0d743818d1";

    fn keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&SENDER_IDENTITY_SEED),
            SigningKey::from_seed(&SENDER_DEVICE_SEED),
        )
    }

    fn fixture_room_id() -> RoomId {
        let identity = SigningKey::from_seed(&SENDER_IDENTITY_SEED);
        signed::derive_room_id(&identity.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    /// A non-empty `prev_events` (file.shared is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct Built {
        wire: WireEvent,
        event: SignedEvent,
    }

    /// Build with the default fixture (raw format, empty providers).
    fn build_fixture() -> Built {
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let wire = build_file_shared(
            &id,
            &dev,
            &room_id,
            FILE_ID,
            NAME,
            MIME,
            SIZE,
            HashRef::from_bytes(BLOB_HASH),
            Some("raw"),
            &[],
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("file.shared must decode");
        Built { wire, event }
    }

    #[test]
    fn builder_is_deterministic() {
        let a = build_fixture();
        let b = build_fixture();
        assert_eq!(
            a.wire.to_bytes(),
            b.wire.to_bytes(),
            "same inputs must yield byte-identical output"
        );
    }

    #[test]
    fn content_round_trips_every_field() {
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let provider = SigningKey::from_seed(&PROVIDER_DEVICE_SEED).device_key();
        let wire = build_file_shared(
            &id,
            &dev,
            &room_id,
            FILE_ID,
            NAME,
            MIME,
            SIZE,
            HashRef::from_bytes(BLOB_HASH),
            Some("raw"),
            &[provider],
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        let Content::FileShared(c) = event.content else {
            panic!("expected file.shared content");
        };
        assert_eq!(c.file_id, FILE_ID);
        assert_eq!(c.name, NAME);
        assert_eq!(c.mime_type, MIME);
        assert_eq!(c.size_bytes, SIZE);
        assert_eq!(c.blob_hash, HashRef::from_bytes(BLOB_HASH));
        assert_eq!(c.blob_format.as_deref(), Some("raw"));
        assert_eq!(c.providers, Some(vec![provider]));
        assert_eq!(event.prev_events, fixture_heads());
        assert_eq!(event.created_at, CREATED_AT);
    }

    #[test]
    fn empty_providers_slice_is_omitted() {
        // An empty providers slice must omit the field entirely (None), not encode
        // an empty array — the §7 omit-when-empty rule the content parser enforces.
        let f = build_fixture();
        let Content::FileShared(c) = f.event.content else {
            panic!("expected file.shared content");
        };
        assert_eq!(c.providers, None);
    }

    #[test]
    fn none_blob_format_is_omitted_and_valid() {
        // `blob_format == None` must be omitted (defaults to `raw` on read); the
        // event must still validate through the full stateless pipeline.
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let wire = build_file_shared(
            &id,
            &dev,
            &room_id,
            FILE_ID,
            NAME,
            MIME,
            SIZE,
            HashRef::from_bytes(BLOB_HASH),
            None,
            &[],
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        let Content::FileShared(c) = event.content else {
            panic!("expected file.shared content");
        };
        assert_eq!(c.blob_format, None);
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("a file.shared with no blob_format must validate");
    }

    #[test]
    fn built_file_shared_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture();
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built file.shared must validate");
        let (id, dev) = keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::FileShared
        );
    }

    #[test]
    fn signature_verifies_under_device_id() {
        let f = build_fixture();
        let msg = signed::event_signing_message(&f.wire.signed);
        f.event
            .device_id
            .verify(&msg, &f.wire.sig)
            .expect("signature must verify under device_id");
    }

    #[test]
    fn signature_does_not_verify_under_sender_id() {
        use crate::event::keys::DeviceKey;
        let f = build_fixture();
        let msg = signed::event_signing_message(&f.wire.signed);
        let sender_as_device = DeviceKey::from_bytes(*f.event.sender_id.as_bytes());
        assert!(
            sender_as_device.verify(&msg, &f.wire.sig).is_err(),
            "an event signature must never verify under sender_id"
        );
    }

    #[test]
    fn golden_event_id_is_stable() {
        // Regression lock on the exact bytes (see GOLDEN_EVENT_ID_HEX note).
        let f = build_fixture();
        assert_eq!(
            hex::encode(f.event.event_id().as_bytes()),
            GOLDEN_EVENT_ID_HEX
        );
    }

    #[test]
    fn distinct_hashes_produce_distinct_event_ids() {
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let heads = fixture_heads();
        let a = build_file_shared(
            &id,
            &dev,
            &room_id,
            FILE_ID,
            NAME,
            MIME,
            SIZE,
            HashRef::from_bytes([0x01; 32]),
            Some("raw"),
            &[],
            &heads,
            CREATED_AT,
        );
        let b = build_file_shared(
            &id,
            &dev,
            &room_id,
            FILE_ID,
            NAME,
            MIME,
            SIZE,
            HashRef::from_bytes([0x02; 32]),
            Some("raw"),
            &[],
            &heads,
            CREATED_AT,
        );
        let a_id = SignedEvent::decode(&a.signed).unwrap().event_id();
        let b_id = SignedEvent::decode(&b.signed).unwrap().event_id();
        assert_ne!(
            a_id, b_id,
            "distinct blob hashes must produce distinct event_ids"
        );
    }
}
