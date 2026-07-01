//! Pure `member.left` event assembly: build and sign a member's voluntary
//! self-departure (Event Protocol §7).
//!
//! This is the single byte-exact place a `member.left` event is assembled from a
//! member's keys, the sibling of [`build_member_invited`](super::invite::build_member_invited)
//! and [`build_member_joined`](super::join::build_member_joined). It is
//! **deterministic** in its inputs — the caller injects `reason`, `prev_events`,
//! and `created_at` — so this function is itself clock-/RNG-free and
//! golden-testable (the only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! `member.left` carries **no** embedded `device_binding`: it is a
//! membership-device-bound type (`requires_membership_device_binding == true`), so
//! the departing member's device is resolved from the membership fold rather than
//! from the event itself. The §7 self-departure rule requires
//! `member_id == sender_id`; this builder wires the sender's identity into
//! `member_id` so the content is well-formed by construction. The event is signed
//! by the member's **device** secret; the signature MUST verify under `device_id`,
//! never `sender_id`.

use super::constants::SCHEMA_VERSION;
use super::content::{Content, EventType, MemberLeft};
use super::ids::{EventId, RoomId};
use super::keys::SigningKey;
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a member-authored `member.left` event (Event Protocol §7).
///
/// The `sender_identity_secret` provides `sender_id` (and `member_id`, which MUST
/// equal it for a self-departure); the `sender_device_secret` signs the event (the
/// signature MUST verify under `device_id`). The two are passed separately,
/// mirroring [`build_member_invited`](super::invite::build_member_invited).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `reason`, `prev_events` (the room heads), and `created_at` (a clock read) are
/// injected by the caller so this stays free of wall-clock and RNG. An absent
/// `reason` follows the §7 omit-when-empty rule (omitted from the content map).
#[must_use]
pub fn build_member_left(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    reason: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = sender_identity_secret.identity_key();
    let device_id = sender_device_secret.device_key();

    let content = Content::MemberLeft(MemberLeft {
        member_id: sender_id,
        reason: reason.map(ToOwned::to_owned),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberLeft,
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
    use super::build_member_left;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (mirrors invite.rs / join.rs). These are
    // implementation-pinned regression locks, not published conformance vectors.
    const MEMBER_IDENTITY_SEED: [u8; 32] = [0x04; 32];
    const MEMBER_DEVICE_SEED: [u8; 32] = [0x05; 32];
    const ADMIN_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    // Implementation-pinned regression lock: the event id our builder produces for
    // the fixtures below. Recompute & update only on an intentional byte-format
    // change to `member.left` (such a change is breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "b2e066dd08ba152787e06ef0dfafa71488718bdd509f92c55183b2c558202bb2";

    fn member_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&MEMBER_IDENTITY_SEED),
            SigningKey::from_seed(&MEMBER_DEVICE_SEED),
        )
    }

    /// The room id is derived from the *admin's* identity (the genesis vector);
    /// the departing member cites it but does not own it.
    fn fixture_room_id() -> RoomId {
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        signed::derive_room_id(&admin.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    /// A non-empty `prev_events` (member.left is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads at departure time.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct WireEventAndEvent {
        wire: WireEvent,
        event: SignedEvent,
    }

    fn build_fixture(reason: Option<&str>) -> WireEventAndEvent {
        let (id, dev) = member_keys();
        let room_id = fixture_room_id();
        let wire = build_member_left(&id, &dev, &room_id, reason, &fixture_heads(), CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("member.left must decode");
        WireEventAndEvent { wire, event }
    }

    #[test]
    fn builder_is_deterministic() {
        let a = build_fixture(None);
        let b = build_fixture(None);
        assert_eq!(
            a.wire.to_bytes(),
            b.wire.to_bytes(),
            "same inputs must yield byte-identical output"
        );
    }

    #[test]
    fn content_round_trips_every_field() {
        let (id, _dev) = member_keys();
        let f = build_fixture(Some("moving on"));
        let Content::MemberLeft(c) = f.event.content else {
            panic!("expected member.left content");
        };
        assert_eq!(
            c.member_id,
            id.identity_key(),
            "member_id must equal the departing sender's identity"
        );
        assert_eq!(c.reason.as_deref(), Some("moving on"));
        assert_eq!(f.event.prev_events, fixture_heads());
        assert_eq!(f.event.created_at, CREATED_AT);
    }

    #[test]
    fn absent_reason_round_trips_as_none() {
        let f = build_fixture(None);
        let Content::MemberLeft(c) = f.event.content else {
            panic!("expected member.left content");
        };
        assert_eq!(c.reason, None);
    }

    #[test]
    fn built_left_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture(None);
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built member.left must validate");
        let (id, dev) = member_keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::MemberLeft
        );
    }

    #[test]
    fn built_left_with_reason_passes_stateless_validation() {
        // The optional-field code path in the content validator the no-reason
        // fixture skips.
        let room_id = fixture_room_id();
        let f = build_fixture(Some("done here"));
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("member.left with reason must pass stateless validation");
        let Content::MemberLeft(c) = validated.event.content else {
            panic!("expected member.left content");
        };
        assert_eq!(c.reason.as_deref(), Some("done here"));
    }

    #[test]
    fn signature_verifies_under_device_id() {
        let f = build_fixture(None);
        let msg = signed::event_signing_message(&f.wire.signed);
        f.event
            .device_id
            .verify(&msg, &f.wire.sig)
            .expect("signature must verify under device_id");
    }

    #[test]
    fn member_id_is_the_sender_not_the_admin() {
        // A self-departure names the sender as the subject; never the admin.
        let (member_id, _dev) = member_keys();
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        let f = build_fixture(None);
        let Content::MemberLeft(c) = f.event.content else {
            panic!("expected member.left content");
        };
        assert_eq!(c.member_id, member_id.identity_key());
        assert_ne!(c.member_id, admin.identity_key());
    }

    #[test]
    fn empty_prev_events_fails_stateless_validation() {
        // member.left is not the genesis; empty prev_events must be rejected.
        let room_id = fixture_room_id();
        let (id, dev) = member_keys();
        let wire = build_member_left(&id, &dev, &room_id, None, &[], CREATED_AT);
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id));
        assert!(
            result.is_err(),
            "member.left with empty prev_events must fail validation (not_genesis_descended)"
        );
    }

    #[test]
    fn golden_event_id_is_stable() {
        let f = build_fixture(None);
        assert_eq!(
            hex::encode(f.event.event_id().as_bytes()),
            GOLDEN_EVENT_ID_HEX
        );
    }
}
