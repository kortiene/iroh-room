//! Pure `member.removed` event assembly: build and sign an admin's removal of a
//! member (Event Protocol §7).
//!
//! This is the single byte-exact place a `member.removed` event is assembled from
//! the admin's keys, the sibling of [`build_member_invited`](super::invite::build_member_invited)
//! and [`build_member_left`](super::left::build_member_left). It is
//! **deterministic** in its inputs — the caller injects the `subject`, `reason`,
//! the optional `device_binding`, `prev_events`, and `created_at` — so this
//! function is itself clock-/RNG-free and golden-testable (the only RNG in `core`
//! stays inside [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! The §7 removal rules require `removed_by == sender_id` (only the admin removes)
//! and `member_id != sender_id` (an admin does not self-remove; that is
//! `member.left`); this builder wires the admin's identity into `removed_by` so the
//! content is well-formed by construction. `member.removed` carries an **optional**
//! `device_binding` re-attesting the admin's device — verified statelessly when
//! present (Event Protocol §9). The event is signed by the admin's **device**
//! secret; the signature MUST verify under `device_id`, never `sender_id`.

use super::binding::DeviceBinding;
use super::constants::SCHEMA_VERSION;
use super::content::{Content, EventType, MemberRemoved};
use super::ids::{EventId, RoomId};
use super::keys::{IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign an admin-authored `member.removed` event (Event Protocol §7).
///
/// The `admin_identity_secret` provides `sender_id` (and `removed_by`, which MUST
/// equal it); the `admin_device_secret` signs the event (the signature MUST verify
/// under `device_id`). The two are passed separately, mirroring
/// [`build_member_invited`](super::invite::build_member_invited).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `subject` (the removed identity, which MUST differ from the admin), `reason`,
/// `device_binding`, `prev_events`, and `created_at` are injected by the caller so
/// this stays free of wall-clock and RNG. An absent `reason` / `device_binding`
/// follows the §7 omit-when-empty rule (omitted from the content map).
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_member_invited; each arg is a distinct signed field
pub fn build_member_removed(
    admin_identity_secret: &SigningKey,
    admin_device_secret: &SigningKey,
    room_id: &RoomId,
    subject: &IdentityKey,
    reason: Option<&str>,
    device_binding: Option<DeviceBinding>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = admin_identity_secret.identity_key();
    let device_id = admin_device_secret.device_key();

    let content = Content::MemberRemoved(MemberRemoved {
        member_id: *subject,
        removed_by: sender_id,
        reason: reason.map(ToOwned::to_owned),
        device_binding,
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberRemoved,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, admin_device_secret);
    WireEvent::seal(csb, sig)
}

#[cfg(test)]
mod tests {
    use super::build_member_removed;
    use crate::event::binding::DeviceBinding;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (mirrors invite.rs / join.rs). These are
    // implementation-pinned regression locks, not published conformance vectors.
    const ADMIN_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const ADMIN_DEVICE_SEED: [u8; 32] = [0x02; 32];
    // The removed subject's identity seed; its public key is the `member_id`.
    const SUBJECT_SEED: [u8; 32] = [0x04; 32];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    // Implementation-pinned regression lock: the event id our builder produces for
    // the fixtures below (no reason, no binding). Recompute & update only on an
    // intentional byte-format change to `member.removed` (such a change is breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "7d00d16d655fcdeec4c7b0ed8b5b32e8da2f45a926013b953584b8ec1ab4d06d";

    fn admin_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&ADMIN_IDENTITY_SEED),
            SigningKey::from_seed(&ADMIN_DEVICE_SEED),
        )
    }

    fn fixture_room_id() -> RoomId {
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        signed::derive_room_id(&admin.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    /// A non-empty `prev_events` (member.removed is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads at removal time.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct WireEventAndEvent {
        wire: WireEvent,
        event: SignedEvent,
    }

    fn build_fixture(reason: Option<&str>, with_binding: bool) -> WireEventAndEvent {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let binding = with_binding.then(|| DeviceBinding::create(&room_id, &id, dev.device_key()));
        let wire = build_member_removed(
            &id,
            &dev,
            &room_id,
            &subject,
            reason,
            binding,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("member.removed must decode");
        WireEventAndEvent { wire, event }
    }

    #[test]
    fn builder_is_deterministic() {
        let a = build_fixture(None, false);
        let b = build_fixture(None, false);
        assert_eq!(
            a.wire.to_bytes(),
            b.wire.to_bytes(),
            "same inputs must yield byte-identical output"
        );
    }

    #[test]
    fn content_round_trips_every_field() {
        let (admin_id, _dev) = admin_keys();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let f = build_fixture(Some("policy violation"), false);
        let Content::MemberRemoved(c) = f.event.content else {
            panic!("expected member.removed content");
        };
        assert_eq!(
            c.member_id, subject,
            "member_id must be the removed subject"
        );
        assert_eq!(
            c.removed_by,
            admin_id.identity_key(),
            "removed_by must be the admin sender"
        );
        assert_eq!(c.reason.as_deref(), Some("policy violation"));
        assert_eq!(c.device_binding, None);
        assert_eq!(f.event.prev_events, fixture_heads());
        assert_eq!(f.event.created_at, CREATED_AT);
    }

    #[test]
    fn absent_reason_and_binding_round_trip_as_none() {
        let f = build_fixture(None, false);
        let Content::MemberRemoved(c) = f.event.content else {
            panic!("expected member.removed content");
        };
        assert_eq!(c.reason, None);
        assert_eq!(c.device_binding, None);
    }

    #[test]
    fn built_removed_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture(None, false);
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built member.removed must validate");
        let (id, dev) = admin_keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::MemberRemoved
        );
    }

    #[test]
    fn built_removed_with_reason_and_binding_passes_stateless_validation() {
        // Exercises the optional reason + the optional self-contained device
        // binding path (verified statelessly when present, Event Protocol §9).
        let room_id = fixture_room_id();
        let f = build_fixture(Some("kicked"), true);
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("member.removed with reason + binding must pass stateless validation");
        let Content::MemberRemoved(c) = validated.event.content else {
            panic!("expected member.removed content");
        };
        assert_eq!(c.reason.as_deref(), Some("kicked"));
        assert!(c.device_binding.is_some(), "binding must round-trip");
    }

    #[test]
    fn signature_verifies_under_device_id() {
        let f = build_fixture(None, false);
        let msg = signed::event_signing_message(&f.wire.signed);
        f.event
            .device_id
            .verify(&msg, &f.wire.sig)
            .expect("signature must verify under device_id");
    }

    #[test]
    fn subject_differs_from_admin_sender() {
        // §7: member_id (subject) MUST differ from removed_by/sender_id.
        let f = build_fixture(None, false);
        let Content::MemberRemoved(c) = f.event.content else {
            panic!("expected member.removed content");
        };
        assert_ne!(
            c.member_id, c.removed_by,
            "an admin removal must target someone other than the admin"
        );
    }

    #[test]
    fn empty_prev_events_fails_stateless_validation() {
        // member.removed is not the genesis; empty prev_events must be rejected.
        let room_id = fixture_room_id();
        let (id, dev) = admin_keys();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let wire = build_member_removed(&id, &dev, &room_id, &subject, None, None, &[], CREATED_AT);
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id));
        assert!(
            result.is_err(),
            "member.removed with empty prev_events must fail validation (not_genesis_descended)"
        );
    }

    #[test]
    fn golden_event_id_is_stable() {
        let f = build_fixture(None, false);
        assert_eq!(
            hex::encode(f.event.event_id().as_bytes()),
            GOLDEN_EVENT_ID_HEX
        );
    }
}
