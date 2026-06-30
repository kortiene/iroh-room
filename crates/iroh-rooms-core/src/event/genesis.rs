//! Pure genesis-event assembly: build and sign a `room.created` event
//! (Event Protocol §5/§6/§7).
//!
//! This is the single byte-exact place the genesis `room.created` is assembled
//! from a creator's keys. It is **deterministic** in its inputs — the caller
//! injects the `room_nonce` (a CSPRNG draw) and `created_at` (a clock read), so
//! this function is itself clock-/RNG-free and golden-testable (the only RNG in
//! `core` stays inside [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! The output is a sealed [`WireEvent`]; the caller validates it through
//! [`validate_wire_bytes`](super::validate::validate_wire_bytes), persists the
//! verbatim bytes, and re-derives `room_id` from the signed fields. Keeping the
//! assembly here (rather than in the CLI) gives it one conformance test and lets
//! later flows (`net`, future room commands) reuse exactly these bytes.

use super::binding::DeviceBinding;
use super::constants::{SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{Content, EventType, RoomCreated};
use super::keys::SigningKey;
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a genesis `room.created` event (Event Protocol §5/§6/§7).
///
/// The creator's `identity_secret` signs the device binding (authorizing the
/// device under `sender_id`); the `device_secret` signs the event itself (the
/// signature MUST verify under `device_id`, never `sender_id`). The room's
/// single immutable admin is the creator: `admins == [sender_id]`.
///
/// Pure and deterministic: with the same five inputs it yields byte-identical
/// output. `room_nonce` (CSPRNG) and `created_at` (clock) are injected by the
/// caller so this stays free of wall-clock and RNG.
#[must_use]
pub fn build_room_created(
    identity_secret: &SigningKey,
    device_secret: &SigningKey,
    room_name: &str,
    room_nonce: &[u8; SHORT_ID_LEN],
    created_at: u64,
) -> WireEvent {
    let sender_id = identity_secret.identity_key();
    let device_id = device_secret.device_key();
    let room_id = signed::derive_room_id(&sender_id, room_nonce, created_at);
    let device_binding = DeviceBinding::create(&room_id, identity_secret, device_id);

    let content = Content::RoomCreated(RoomCreated {
        room_name: room_name.to_owned(),
        room_nonce: *room_nonce,
        admins: vec![sender_id],
        device_binding,
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id,
        sender_id,
        device_id,
        event_type: EventType::RoomCreated,
        created_at,
        prev_events: Vec::new(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, device_secret);
    WireEvent::seal(csb, sig)
}

#[cfg(test)]
mod tests {
    use super::build_room_created;
    use crate::event::content::Content;
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};

    // The §5 golden vector (PHASE-0-SPIKE.md Protocol Test Vectors, "Room"):
    // creator identity seed 0x01×32, nonce 000102…0e0f, created_at 1750000000000.
    const NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    const GOLDEN_ROOM_ID_HEX: &str =
        "43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3";

    fn keys() -> (SigningKey, SigningKey) {
        // Identity seed 0x01 fixes the golden room_id; device seed 0x02 is the
        // golden device key. room_id depends only on the identity key.
        (
            SigningKey::from_seed(&[0x01; 32]),
            SigningKey::from_seed(&[0x02; 32]),
        )
    }

    #[test]
    fn builds_genesis_with_golden_room_id() {
        let (id, dev) = keys();
        let room_id = signed::derive_room_id(&id.identity_key(), &NONCE, CREATED_AT);
        assert_eq!(hex::encode(room_id.as_bytes()), GOLDEN_ROOM_ID_HEX);

        // The advisory wire id is the event id of the bytes the builder sealed.
        let wire = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("genesis must decode");
        assert_eq!(event.room_id, room_id);
    }

    #[test]
    fn built_genesis_passes_stateless_validation() {
        let (id, dev) = keys();
        let room_id = signed::derive_room_id(&id.identity_key(), &NONCE, CREATED_AT);
        let wire = build_room_created(&id, &dev, "MX-Loom Build Room", &NONCE, CREATED_AT);
        // The full §6 pipeline must accept our own event: recomputes room_id,
        // verifies the binding, verifies the signature under device_id, and
        // enforces prev_events == [].
        let validated =
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built genesis must validate");
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
    }

    #[test]
    fn admins_is_exactly_the_creator() {
        let (id, dev) = keys();
        let wire = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("genesis must decode");
        match event.content {
            Content::RoomCreated(c) => {
                assert_eq!(c.admins, vec![id.identity_key()]);
                assert!(event.prev_events.is_empty());
            }
            other => panic!("expected room.created, got {other:?}"),
        }
    }

    /// Different room names must produce different `event_id`s — the name is
    /// part of the canonical signed bytes (spec D1; sanity check).
    #[test]
    fn different_room_names_produce_different_event_ids() {
        let (id, dev) = keys();
        let wire_a = build_room_created(&id, &dev, "Room A", &NONCE, CREATED_AT);
        let wire_b = build_room_created(&id, &dev, "Room B", &NONCE, CREATED_AT);

        let ev_a = SignedEvent::decode(&wire_a.signed).expect("must decode");
        let ev_b = SignedEvent::decode(&wire_b.signed).expect("must decode");
        assert_ne!(
            ev_a.event_id(),
            ev_b.event_id(),
            "distinct room names must produce distinct event_ids"
        );
    }

    /// Different `room_nonce` values must derive different `room_id`s (§5
    /// uniqueness property — two creates in the same home produce distinct rooms).
    #[test]
    fn different_nonces_produce_different_room_ids() {
        let (id, dev) = keys();
        let mut nonce_b = NONCE;
        nonce_b[0] ^= 0xff; // flip first byte

        let wire_a = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);
        let wire_b = build_room_created(&id, &dev, "Room", &nonce_b, CREATED_AT);

        let ev_a = SignedEvent::decode(&wire_a.signed).expect("must decode");
        let ev_b = SignedEvent::decode(&wire_b.signed).expect("must decode");
        assert_ne!(
            ev_a.room_id, ev_b.room_id,
            "different nonces must derive different room_ids"
        );
    }

    /// Different creators must derive different `room_id`s for the same nonce
    /// and timestamp (spec §5: `room_id` binds to `creator_sender_id`).
    #[test]
    fn different_creators_produce_different_room_ids() {
        let id_a = SigningKey::from_seed(&[0x01; 32]);
        let id_b = SigningKey::from_seed(&[0x03; 32]);
        let dev = SigningKey::from_seed(&[0x02; 32]);

        let wire_a = build_room_created(&id_a, &dev, "Room", &NONCE, CREATED_AT);
        let wire_b = build_room_created(&id_b, &dev, "Room", &NONCE, CREATED_AT);

        let ev_a = SignedEvent::decode(&wire_a.signed).expect("must decode");
        let ev_b = SignedEvent::decode(&wire_b.signed).expect("must decode");
        assert_ne!(
            ev_a.room_id, ev_b.room_id,
            "different creators must produce different room_ids (room_id binds sender_id)"
        );
    }

    /// Validation with the **wrong** `room_id` context must be rejected (AC3:
    /// the validator recomputes `room_id` and refuses a mismatch).
    #[test]
    fn validation_rejects_genesis_presented_under_wrong_room_id() {
        let (id, dev) = keys();
        let wire = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);

        // A different nonce gives a different (wrong) room_id.
        let mut wrong_nonce = NONCE;
        wrong_nonce[0] ^= 0xff;
        let wrong_room_id = signed::derive_room_id(&id.identity_key(), &wrong_nonce, CREATED_AT);

        let result = validate_wire_bytes(
            &wire.to_bytes(),
            &ValidationContext::for_room(wrong_room_id),
        );
        assert!(
            result.is_err(),
            "validation must reject a genesis whose room_id doesn't match the context"
        );
    }

    /// The `created_at` field round-trips through encode → decode unchanged.
    #[test]
    fn created_at_round_trips_through_encode_decode() {
        let (id, dev) = keys();
        let wire = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        assert_eq!(
            event.created_at, CREATED_AT,
            "created_at must survive the encode/decode round-trip"
        );
    }

    /// The room name stored in content round-trips through encode → decode.
    #[test]
    fn room_name_round_trips_through_encode_decode() {
        let (id, dev) = keys();
        let name = "MX-Loom Build Room";
        let wire = build_room_created(&id, &dev, name, &NONCE, CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        match event.content {
            Content::RoomCreated(c) => {
                assert_eq!(c.room_name, name, "room_name must survive encode/decode");
                assert_eq!(c.room_nonce, NONCE, "room_nonce must survive encode/decode");
            }
            other => panic!("expected RoomCreated, got {other:?}"),
        }
    }

    /// Sender id in the signed event must match `identity_secret.identity_key()`
    /// and device id must match `device_secret.device_key()` — the right keys
    /// are used in the right roles (spec §9 R2).
    #[test]
    fn sender_id_and_device_id_are_derived_from_correct_keys() {
        let (id, dev) = keys();
        let wire = build_room_created(&id, &dev, "Room", &NONCE, CREATED_AT);
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        assert_eq!(
            event.sender_id,
            id.identity_key(),
            "sender_id must be derived from identity_secret"
        );
        assert_eq!(
            event.device_id,
            dev.device_key(),
            "device_id must be derived from device_secret"
        );
    }
}
