//! Pure `message.text` event assembly: build and sign a chat message
//! (Event Protocol §7).
//!
//! This is the single byte-exact place a `message.text` event is assembled from a
//! member's keys, the sibling of [`build_member_invited`](super::invite::build_member_invited)
//! and [`build_room_created`](super::genesis::build_room_created). It is
//! **deterministic** in its inputs — the caller injects the `prev_events` (the
//! room heads) and `created_at` (a clock read) — so this function is itself
//! clock-/RNG-free and golden-testable (the only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! `message.text` carries **no** embedded `device_binding`: it is a
//! membership-device-bound type (`requires_membership_device_binding == true`), so
//! the author's device is resolved from the membership fold rather than from the
//! event itself. The event is signed by the author's **device** secret; the
//! signature MUST verify under `device_id`, never `sender_id`.
//!
//! The builder does **not** enforce the §7 body-length cap (≤ 16384 bytes): that
//! is enforced by the strict content parser on decode/validate. Callers that want
//! a friendly pre-IO error validate the body before building (the CLI does).

use super::constants::SCHEMA_VERSION;
use super::content::{Content, EventType, MessageText};
use super::ids::{EventId, RoomId};
use super::keys::{IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a member-authored `message.text` event (Event Protocol §7).
///
/// The `sender_identity_secret` provides `sender_id` (the authorizing membership
/// identity); the `sender_device_secret` signs the event (the signature MUST
/// verify under `device_id`). The two are passed separately, mirroring
/// [`build_member_invited`](super::invite::build_member_invited).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `prev_events` (the room heads) and `created_at` (a clock read) are injected by
/// the caller so this stays free of wall-clock and RNG. Optional fields follow the
/// §7 omit-when-empty rule: `format == None` is omitted (defaults to `plain` on
/// read) and an empty `mentions` slice is omitted entirely.
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_member_invited; each arg is a distinct signed field
pub fn build_message_text(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    body: &str,
    format: Option<&str>,
    in_reply_to: Option<EventId>,
    mentions: &[IdentityKey],
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = sender_identity_secret.identity_key();
    let device_id = sender_device_secret.device_key();

    let content = Content::MessageText(MessageText {
        body: body.to_owned(),
        format: format.map(ToOwned::to_owned),
        in_reply_to,
        mentions: (!mentions.is_empty()).then(|| mentions.to_vec()),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MessageText,
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
    use super::build_message_text;
    use crate::event::constants::MAX_MESSAGE_BODY_BYTES;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::{IdentityKey, Signature, SigningKey};
    use crate::event::reject::{MembershipOracle, RejectReason};
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{
        validate_wire_bytes, validate_with_membership, ValidationContext,
    };
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (spec §10 step 1). These are
    // implementation-pinned regression locks, not published conformance vectors:
    // the spike lists `E_msg_bob` ("hi all") but does not byte-pin its full content
    // map, so we assert the value our own implementation produces.
    const SENDER_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const SENDER_DEVICE_SEED: [u8; 32] = [0x02; 32];
    // A second identity, used as a mention target.
    const MENTION_SEED: [u8; 32] = [0x04; 32];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    const BODY: &str = "I pushed the first prototype.";
    // Implementation-pinned regression lock: the event id our builder produces for
    // the fixtures above. Recompute & update only on an intentional byte-format
    // change to `message.text` (such a change is breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "8b26d39801d00aa0c6f41b47bc70d33c9e2829a6e79abf6e6c811f2d0dd24881";

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

    /// A non-empty `prev_events` (message.text is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct Built {
        wire: WireEvent,
        event: SignedEvent,
    }

    fn build_body(body: &str) -> Built {
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let wire = build_message_text(
            &id,
            &dev,
            &room_id,
            body,
            None,
            None,
            &[],
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("message.text must decode");
        Built { wire, event }
    }

    fn build_fixture() -> Built {
        build_body(BODY)
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
        let room_id = fixture_room_id();
        let (id, dev) = keys();
        let reply = EventId::from_bytes([0xcd; 32]);
        let mention = SigningKey::from_seed(&MENTION_SEED).identity_key();
        let wire = build_message_text(
            &id,
            &dev,
            &room_id,
            "hi all",
            Some("markdown"),
            Some(reply),
            &[mention],
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        let Content::MessageText(c) = event.content else {
            panic!("expected message.text content");
        };
        assert_eq!(c.body, "hi all");
        assert_eq!(c.format.as_deref(), Some("markdown"));
        assert_eq!(c.in_reply_to, Some(reply));
        assert_eq!(c.mentions, Some(vec![mention]));
        assert_eq!(event.prev_events, fixture_heads());
        assert_eq!(event.created_at, CREATED_AT);
    }

    #[test]
    fn absent_optionals_round_trip_as_none() {
        let f = build_fixture();
        let Content::MessageText(c) = f.event.content else {
            panic!("expected message.text content");
        };
        assert_eq!(c.format, None);
        assert_eq!(c.in_reply_to, None);
        assert_eq!(c.mentions, None);
    }

    #[test]
    fn empty_mentions_slice_is_omitted() {
        // An empty mentions slice must omit the field entirely (None), not encode
        // an empty array — the §7 omit-when-empty rule the content parser enforces.
        let f = build_fixture();
        let Content::MessageText(c) = f.event.content else {
            panic!("expected message.text content");
        };
        assert_eq!(c.mentions, None);
    }

    #[test]
    fn built_message_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture();
        // The full §6 stateless pipeline must accept our own event.
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built message.text must validate");
        let (id, dev) = keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::MessageText
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
        // The signing key is the device key; verifying the same bytes under the
        // identity key (sender_id) must fail (spec §1/§6: never `sender_id`).
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
    fn body_at_cap_passes_stateless_validation() {
        // A body exactly at the §7 cap (16384 bytes) must validate.
        let room_id = fixture_room_id();
        let body = "a".repeat(MAX_MESSAGE_BODY_BYTES);
        let f = build_body(&body);
        validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("a body exactly at the cap must validate");
    }

    #[test]
    fn body_over_cap_is_rejected_by_validation() {
        // The builder does not enforce the cap; the stateless validator does. Build
        // the wire directly (the decode helper would itself reject the over-cap
        // content), then assert validation refuses it.
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let body = "a".repeat(MAX_MESSAGE_BODY_BYTES + 1);
        let wire = build_message_text(
            &id,
            &dev,
            &room_id,
            &body,
            None,
            None,
            &[],
            &fixture_heads(),
            CREATED_AT,
        );
        assert!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id)).is_err(),
            "a body one byte over the cap must be rejected by validate_wire_bytes"
        );
    }

    #[test]
    fn distinct_bodies_produce_distinct_event_ids() {
        let a = build_body("hello");
        let b = build_body("world");
        assert_ne!(
            a.event.event_id(),
            b.event.event_id(),
            "distinct bodies must produce distinct event_ids"
        );
    }

    #[test]
    fn tampered_body_breaks_id_and_signature() {
        // Spike vector 6: tampering any signed byte changes the id and breaks the
        // signature.
        let f = build_body("hello");
        let mut tampered = f.wire.signed.clone();
        *tampered.last_mut().expect("non-empty signed bytes") ^= 0x01;
        let new_id = signed::event_id_from_bytes(&tampered);
        assert_ne!(
            new_id.to_named_string(),
            f.wire.id,
            "a tampered byte must change the recomputed event id"
        );
        let msg = signed::event_signing_message(&tampered);
        assert!(
            f.event.device_id.verify(&msg, &f.wire.sig).is_err(),
            "the original signature must not verify over tampered bytes"
        );
    }

    // ── IR-0105 acceptance criteria ─────────────────────────────────────────

    #[test]
    fn tampered_signature_is_rejected_by_stateless_validator() {
        // AC3 — Invalid signatures are rejected.
        // Keep the signed bytes intact (so the id check passes) but zero the sig.
        // The stateless pipeline must detect this and return BadSignature.
        let room_id = fixture_room_id();
        let f = build_fixture();
        let tampered = WireEvent {
            sig: Signature::from_bytes([0x00; 64]),
            ..f.wire
        };
        let result =
            validate_wire_bytes(&tampered.to_bytes(), &ValidationContext::for_room(room_id));
        assert_eq!(
            result.unwrap_err(),
            RejectReason::BadSignature,
            "a zeroed signature over correct signed bytes must produce BadSignature"
        );
    }

    #[test]
    fn message_text_with_no_prev_events_is_rejected_as_not_genesis_descended() {
        // message.text is a non-genesis type and MUST cite at least one parent.
        // An empty prev_events slice must be rejected before any network IO.
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let wire = build_message_text(
            &id,
            &dev,
            &room_id,
            "orphan",
            None,
            None,
            &[],
            &[],
            CREATED_AT,
        );
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id));
        assert_eq!(
            result.unwrap_err(),
            RejectReason::NotGenesisDescended,
            "message.text with empty prev_events must be rejected as not_genesis_descended"
        );
    }

    #[test]
    fn message_text_for_wrong_room_context_is_rejected() {
        // A message built for room A validated in room B's context must return
        // RoomIdMismatch — the room binding check in validate_wire_bytes §6 step 6.
        let f = build_fixture();
        let other_room = RoomId::from_bytes([0xFF; 32]);
        let result =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(other_room));
        assert_eq!(
            result.unwrap_err(),
            RejectReason::RoomIdMismatch,
            "message.text validated in the wrong room context must return RoomIdMismatch"
        );
    }

    #[test]
    fn non_member_message_is_rejected_by_membership_oracle() {
        // AC4 — Non-member messages are rejected.
        // validate_with_membership delegates authorization to the MembershipOracle.
        // An oracle that denies the sender must cause the pipeline to return NotAMember.
        struct NonMemberOracle;
        impl MembershipOracle for NonMemberOracle {
            fn bound_device(&self, _: &RoomId, _: &IdentityKey) -> Option<[u8; 32]> {
                None
            }
            fn authorize(&self, _: &RoomId, _: &IdentityKey, _: &str) -> Result<(), RejectReason> {
                Err(RejectReason::NotAMember)
            }
        }

        let room_id = fixture_room_id();
        let f = build_fixture();
        let result = validate_with_membership(
            &f.wire.to_bytes(),
            &ValidationContext::for_room(room_id),
            &NonMemberOracle,
        );
        assert_eq!(
            result.unwrap_err(),
            RejectReason::NotAMember,
            "message.text from a non-member must be rejected with NotAMember"
        );
    }
}
