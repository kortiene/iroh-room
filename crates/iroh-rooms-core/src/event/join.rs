//! Pure `member.joined` event assembly: build and sign a joiner's key-bound
//! redemption of an admin invite (Event Protocol §7).
//!
//! This is the single byte-exact place a `member.joined` event is assembled from
//! the joiner's keys, the sibling of [`build_room_created`](super::genesis::build_room_created)
//! and [`build_member_invited`](super::invite::build_member_invited). It is
//! **deterministic** in its inputs — the caller injects `via_invite_id`,
//! `capability_secret`, `role`, the already-built `device_binding`, `prev_events`,
//! and `created_at` — so this function is itself clock-/RNG-free and golden-testable
//! (the only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! Unlike `member.invited` (which carries only the `capability_hash`), the join
//! carries the raw `capability_secret` on the log by protocol (Spike §7): the join
//! *is* the proof of the capability. The membership fold's `gate_join` recomputes
//! `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ via_invite_id ‖ secret)` and matches it
//! against the on-log invite's `capability_hash`. The secret is therefore expected
//! to land on the log here — the key-binding gate keeps a replay under another key
//! inert.
//!
//! `member.joined` is a **self-contained-binding** type
//! (`requires_membership_device_binding == false`): its `device_binding` is verified
//! by the stateless layer ([`verify_bindings`](super::content::verify_bindings)) — it
//! is exactly the binding a first-time joiner's device needs, since the room has
//! never seen that device before. The event is signed by the joiner's **device**
//! secret; the signature MUST verify under `device_id`, never `sender_id`.

use super::binding::DeviceBinding;
use super::constants::{SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{Content, EventType, MemberJoined};
use super::ids::{EventId, RoomId};
use super::keys::SigningKey;
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a joiner's `member.joined` event (Event Protocol §7).
///
/// The `invitee_identity_secret` provides `sender_id` (which MUST equal the bound
/// `member.invited.invitee_key`); the `invitee_device_secret` signs the event (the
/// signature MUST verify under `device_id`). The two are passed separately,
/// mirroring [`build_member_invited`](super::invite::build_member_invited).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `via_invite_id`, `capability_secret`, `role`, `device_binding`, `display_name`,
/// `prev_events`, and `created_at` are injected by the caller so this stays free of
/// wall-clock and RNG. The self-contained `device_binding` is supplied by the caller
/// (built via [`DeviceBinding::create`](super::binding::DeviceBinding::create)); the
/// stateless layer checks it attests exactly `(sender_id, device_id)` under
/// `room_id`.
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_member_invited; each arg is a distinct signed field
pub fn build_member_joined(
    invitee_identity_secret: &SigningKey,
    invitee_device_secret: &SigningKey,
    room_id: &RoomId,
    via_invite_id: &[u8; SHORT_ID_LEN],
    capability_secret: &[u8; SHORT_ID_LEN],
    role: &str,
    device_binding: DeviceBinding,
    display_name: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = invitee_identity_secret.identity_key();
    let device_id = invitee_device_secret.device_key();

    let content = Content::MemberJoined(MemberJoined {
        via_invite_id: *via_invite_id,
        capability_secret: *capability_secret,
        role: role.to_owned(),
        device_binding,
        display_name: display_name.map(ToOwned::to_owned),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberJoined,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, invitee_device_secret);
    WireEvent::seal(csb, sig)
}

#[cfg(test)]
mod tests {
    use super::build_member_joined;
    use crate::event::binding::DeviceBinding;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (spec §10 step 1). These are
    // implementation-pinned regression locks, not published conformance vectors.
    const INVITEE_IDENTITY_SEED: [u8; 32] = [0x04; 32];
    const INVITEE_DEVICE_SEED: [u8; 32] = [0x05; 32];
    const ADMIN_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    // Spike-style handles: invite_id = da7e…, secret = 5ec0da7e…
    const VIA_INVITE_ID: [u8; 16] = [
        0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda,
        0x7e,
    ];
    const SECRET: [u8; 16] = [
        0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda,
        0x7e,
    ];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;

    fn invitee_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&INVITEE_IDENTITY_SEED),
            SigningKey::from_seed(&INVITEE_DEVICE_SEED),
        )
    }

    /// The room id is derived from the *admin's* identity, matching the genesis
    /// vector — the joiner cites it but does not own it.
    fn fixture_room_id() -> RoomId {
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        signed::derive_room_id(&admin.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    /// A non-empty `prev_events` (member.joined is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads after the pull.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct WireEventAndEvent {
        wire: WireEvent,
        event: SignedEvent,
    }

    fn build_fixture(display_name: Option<&str>) -> WireEventAndEvent {
        let (id, dev) = invitee_keys();
        let room_id = fixture_room_id();
        let binding = DeviceBinding::create(&room_id, &id, dev.device_key());
        let wire = build_member_joined(
            &id,
            &dev,
            &room_id,
            &VIA_INVITE_ID,
            &SECRET,
            "member",
            binding,
            display_name,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("member.joined must decode");
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
        let (id, dev) = invitee_keys();
        let f = build_fixture(Some("dave-laptop"));
        let Content::MemberJoined(c) = f.event.content else {
            panic!("expected member.joined content");
        };
        assert_eq!(c.via_invite_id, VIA_INVITE_ID);
        assert_eq!(c.capability_secret, SECRET);
        assert_eq!(c.role, "member");
        assert_eq!(c.device_binding.identity_key, id.identity_key());
        assert_eq!(c.device_binding.device_key, dev.device_key());
        assert_eq!(c.display_name.as_deref(), Some("dave-laptop"));
        assert_eq!(f.event.prev_events, fixture_heads());
        assert_eq!(f.event.created_at, CREATED_AT);
    }

    #[test]
    fn absent_display_name_round_trips_as_none() {
        let f = build_fixture(None);
        let Content::MemberJoined(c) = f.event.content else {
            panic!("expected member.joined content");
        };
        assert_eq!(c.display_name, None);
    }

    #[test]
    fn built_join_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture(None);
        // The full §6 stateless pipeline (incl. the self-contained device-binding
        // check) must accept our own event.
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built member.joined must validate");
        let (id, dev) = invitee_keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::MemberJoined
        );
    }

    #[test]
    fn built_join_with_display_name_passes_stateless_validation() {
        // The optional-field code path in the content validator that the no-name
        // fixture skips.
        let room_id = fixture_room_id();
        let f = build_fixture(Some("dave-laptop"));
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("join with display_name must pass stateless validation");
        let Content::MemberJoined(c) = validated.event.content else {
            panic!("expected member.joined content");
        };
        assert_eq!(c.display_name.as_deref(), Some("dave-laptop"));
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
    fn sender_is_invitee_not_admin() {
        // The joiner authors its own join: sender_id is the invitee identity, never
        // the admin's.
        let (invitee_id, _dev) = invitee_keys();
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        let f = build_fixture(None);
        assert_eq!(f.event.sender_id, invitee_id.identity_key());
        assert_ne!(f.event.sender_id, admin.identity_key());
    }

    #[test]
    fn capability_secret_is_on_the_log_by_protocol() {
        // Unlike member.invited (hash only), the join carries the raw secret on the
        // signed bytes (Spike §7) — the join is the proof of the capability.
        let f = build_fixture(None);
        let needle = SECRET.to_vec();
        assert!(
            f.wire
                .signed
                .windows(needle.len())
                .any(|w| w == needle.as_slice()),
            "capability secret must appear in the join's signed bytes (on the log)"
        );
    }

    // ── security: AC2 key-binding enforcement ────────────────────────────────

    /// AC2 / device-binding: if the device binding attests a different device key
    /// than the one that signed the event (the join's `device_id`), the stateless
    /// binding check must reject it. A join cannot be "re-signed" under a new
    /// device while keeping the old device's binding.
    #[test]
    fn mismatched_device_binding_fails_stateless_validation() {
        let (id, dev) = invitee_keys();
        let other_dev = SigningKey::from_seed(&[0xFF; 32]);
        let room_id = fixture_room_id();
        // Binding attests (id, dev) — the original device.
        let binding = DeviceBinding::create(&room_id, &id, dev.device_key());
        // Sign with other_dev: device_id = other_dev.device_key().
        // binding.device_key (dev) ≠ event.device_id (other_dev) → InvalidContent.
        let wire = build_member_joined(
            &id,
            &other_dev,
            &room_id,
            &VIA_INVITE_ID,
            &SECRET,
            "member",
            binding,
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id));
        assert!(
            result.is_err(),
            "a join whose device_binding.device_key ≠ device_id must fail stateless validation"
        );
    }

    /// A `member.joined` with empty `prev_events` must be rejected: it is not the
    /// genesis event, so `not_genesis_descended` is the expected reason.
    #[test]
    fn join_with_empty_prev_events_fails_stateless_validation() {
        let room_id = fixture_room_id();
        let (id, dev) = invitee_keys();
        let binding = DeviceBinding::create(&room_id, &id, dev.device_key());
        let wire = build_member_joined(
            &id,
            &dev,
            &room_id,
            &VIA_INVITE_ID,
            &SECRET,
            "member",
            binding,
            None,
            &[], // no prev_events — violates the non-genesis invariant
            CREATED_AT,
        );
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id));
        assert!(
            result.is_err(),
            "member.joined with empty prev_events must fail validation (not_genesis_descended)"
        );
    }

    /// A join built for room A must not validate for room B (room-id mismatch).
    #[test]
    fn join_for_wrong_room_fails_stateless_validation() {
        use super::signed;
        let room_id_a = fixture_room_id();
        // A distinct room_id using a different admin key.
        let other_admin = SigningKey::from_seed(&[0x99; 32]);
        let room_id_b =
            signed::derive_room_id(&other_admin.identity_key(), &ROOM_NONCE, CREATED_AT);
        let (id, dev) = invitee_keys();
        let binding = DeviceBinding::create(&room_id_a, &id, dev.device_key());
        let wire = build_member_joined(
            &id,
            &dev,
            &room_id_a,
            &VIA_INVITE_ID,
            &SECRET,
            "member",
            binding,
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        // Validate for the wrong room — must fail with RoomIdMismatch.
        let result = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id_b));
        assert!(
            result.is_err(),
            "a join built for room A must fail validation for room B (room_id_mismatch)"
        );
    }

    // ── additional coverage ──────────────────────────────────────────────────

    /// Non-ASCII / UTF-8 display names (e.g. accented characters, emoji) must
    /// survive CBOR encode → stateless decode → content check. This exercises the
    /// CBOR text-string path with multi-byte sequences.
    #[test]
    fn built_join_with_unicode_display_name_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture(Some("álvaro ☕"));
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("join with unicode display_name must pass stateless validation");
        let Content::MemberJoined(c) = validated.event.content else {
            panic!("expected member.joined content");
        };
        assert_eq!(c.display_name.as_deref(), Some("álvaro ☕"));
    }

    /// A join with multiple `prev_events` must round-trip all entries and still
    /// pass stateless validation (non-genesis events may fan-in multiple heads).
    #[test]
    fn built_join_with_multiple_prev_events_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let heads = vec![
            EventId::from_bytes([0x01; 32]),
            EventId::from_bytes([0x02; 32]),
            EventId::from_bytes([0x03; 32]),
        ];
        let (id, dev) = invitee_keys();
        let binding = DeviceBinding::create(&room_id, &id, dev.device_key());
        let wire = build_member_joined(
            &id,
            &dev,
            &room_id,
            &VIA_INVITE_ID,
            &SECRET,
            "member",
            binding,
            None,
            &heads,
            CREATED_AT,
        );
        // All prev_events must be preserved in the decoded event.
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        assert_eq!(event.prev_events, heads, "all prev_events must round-trip");
        // And the full stateless pipeline must accept the event.
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("join with multiple prev_events must pass stateless validation");
    }

    /// The `agent` role string must round-trip correctly (the role field is a
    /// plain text string; the fold normalises it later — the builder must not alter it).
    #[test]
    fn built_join_with_agent_role_round_trips() {
        let room_id = fixture_room_id();
        let (id, dev) = invitee_keys();
        let binding = DeviceBinding::create(&room_id, &id, dev.device_key());
        let wire = build_member_joined(
            &id,
            &dev,
            &room_id,
            &VIA_INVITE_ID,
            &SECRET,
            "agent",
            binding,
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        let Content::MemberJoined(c) = event.content else {
            panic!("expected member.joined content");
        };
        assert_eq!(c.role, "agent", "agent role must round-trip unchanged");
    }
}
