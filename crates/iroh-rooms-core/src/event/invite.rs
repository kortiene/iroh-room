//! Pure `member.invited` event assembly: build and sign an admin-issued
//! key-bound invite (Event Protocol §7).
//!
//! This is the single byte-exact place a `member.invited` event is assembled
//! from the admin's keys, the sibling of [`build_room_created`](super::genesis::build_room_created).
//! It is **deterministic** in its inputs — the caller injects the `invite_id` and
//! the already-computed `capability_hash` (i.e. the CSPRNG `secret` draw lives in
//! the caller), plus `expires_at`, `prev_events`, and `created_at` — so this
//! function is itself clock-/RNG-free and golden-testable (the only RNG in `core`
//! stays inside [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! The capability **secret never reaches this builder**: the event content carries
//! only the `capability_hash` (Spike §6/§7, AC3). The secret travels exclusively
//! inside the out-of-band [`RoomInviteTicket`](crate::ticket::RoomInviteTicket).
//!
//! `member.invited` carries **no** embedded `device_binding`: it is a
//! membership-device-bound type (`requires_membership_device_binding == true`), so
//! the admin's device is resolved from the genesis binding by the membership fold.
//! The event is signed by the admin's **device** secret; the signature MUST verify
//! under `device_id`, never `sender_id`.

use super::constants::{DIGEST_LEN, SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{Content, EventType, MemberInvited};
use super::ids::{EventId, RoomId};
use super::keys::{IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign an admin-issued `member.invited` event (Event Protocol §7).
///
/// The `admin_identity_secret` provides `sender_id` (the room's single immutable
/// admin); the `admin_device_secret` signs the event (the signature MUST verify
/// under `device_id`). The two are passed separately, mirroring
/// [`build_room_created`](super::genesis::build_room_created).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `invite_id`, `capability_hash` (the secret draw), `expires_at`, `prev_events`,
/// and `created_at` are injected by the caller so this stays free of wall-clock
/// and RNG. The builder does **not** generate the secret or the hash — that is the
/// caller's RNG concern — it accepts the already-computed `capability_hash`.
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_room_created; each arg is a distinct signed field
pub fn build_member_invited(
    admin_identity_secret: &SigningKey,
    admin_device_secret: &SigningKey,
    room_id: &RoomId,
    invite_id: &[u8; SHORT_ID_LEN],
    capability_hash: &[u8; DIGEST_LEN],
    role: &str,
    invitee_key: &IdentityKey,
    expires_at: Option<u64>,
    invitee_hint: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = admin_identity_secret.identity_key();
    let device_id = admin_device_secret.device_key();

    let content = Content::MemberInvited(MemberInvited {
        invite_id: *invite_id,
        capability_hash: *capability_hash,
        role: role.to_owned(),
        invitee_key: *invitee_key,
        expires_at,
        invitee_hint: invitee_hint.map(ToOwned::to_owned),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberInvited,
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
    use super::build_member_invited;
    use crate::event::capability_hash;
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};

    // Deterministic in-test fixtures (spec §10 step 1). These are
    // implementation-pinned regression locks, not published conformance vectors:
    // the spike specifies only the §7 derivation formula and example handles, not
    // a pinned capability-hash output, so we assert the value our own
    // implementation produces.
    const ADMIN_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const ADMIN_DEVICE_SEED: [u8; 32] = [0x02; 32];
    // Invitee ("Dave"-style) identity seed; its public key is the bound invitee.
    const INVITEE_SEED: [u8; 32] = [0x04; 32];
    // Spike-style handles: invite_id = da7e…, secret = 5ec0da7e…
    const INVITE_ID: [u8; 16] = [
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
    // Implementation-pinned regression lock: the event id our builder produces for
    // the fixtures above. Recompute & update only on an intentional byte-format
    // change to `member.invited` (such a change is breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "1ccc28743e5ff48d684b1394056cbaac8ab244ea18c82058c153b3d140de5c52";

    fn admin_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&ADMIN_IDENTITY_SEED),
            SigningKey::from_seed(&ADMIN_DEVICE_SEED),
        )
    }

    fn fixture_room_id() -> RoomId {
        let identity = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        signed::derive_room_id(&identity.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    /// A non-empty `prev_events` (member.invited is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    fn build_fixture(expires_at: Option<u64>) -> WireEventAndEvent {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let invitee = SigningKey::from_seed(&INVITEE_SEED).identity_key();
        let cap = capability_hash(&room_id, &INVITE_ID, &SECRET);
        let wire = build_member_invited(
            &id,
            &dev,
            &room_id,
            &INVITE_ID,
            &cap,
            "member",
            &invitee,
            expires_at,
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("member.invited must decode");
        WireEventAndEvent { wire, event }
    }

    use crate::event::wire::WireEvent;
    struct WireEventAndEvent {
        wire: WireEvent,
        event: SignedEvent,
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
        let room_id = fixture_room_id();
        let invitee = SigningKey::from_seed(&INVITEE_SEED).identity_key();
        let expires_at = CREATED_AT + 86_400_000;
        let (id, dev) = admin_keys();
        let cap = capability_hash(&room_id, &INVITE_ID, &SECRET);
        let wire = build_member_invited(
            &id,
            &dev,
            &room_id,
            &INVITE_ID,
            &cap,
            "agent",
            &invitee,
            Some(expires_at),
            Some("dave-laptop"),
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        let Content::MemberInvited(c) = event.content else {
            panic!("expected member.invited content");
        };
        assert_eq!(c.invite_id, INVITE_ID);
        assert_eq!(c.capability_hash, cap);
        assert_eq!(c.role, "agent");
        assert_eq!(c.invitee_key, invitee);
        assert_eq!(c.expires_at, Some(expires_at));
        assert_eq!(c.invitee_hint.as_deref(), Some("dave-laptop"));
        assert_eq!(event.prev_events, fixture_heads());
        assert_eq!(event.created_at, CREATED_AT);
    }

    #[test]
    fn absent_expiry_and_hint_round_trip_as_none() {
        let f = build_fixture(None);
        let Content::MemberInvited(c) = f.event.content else {
            panic!("expected member.invited content");
        };
        assert_eq!(c.expires_at, None);
        assert_eq!(c.invitee_hint, None);
    }

    #[test]
    fn capability_hash_is_the_secret_derivation_and_secret_is_absent() {
        let room_id = fixture_room_id();
        let f = build_fixture(None);
        let Content::MemberInvited(c) = f.event.content else {
            panic!("expected member.invited content");
        };
        // AC4 substrate: the on-event hash equals BLAKE3(ctx ‖ room ‖ invite ‖ secret).
        assert_eq!(
            c.capability_hash,
            capability_hash(&room_id, &INVITE_ID, &SECRET)
        );
        // AC3: the raw secret bytes appear in NO field of the signed event.
        let csb = f.wire.signed.clone();
        let needle = SECRET.to_vec();
        assert!(
            !csb.windows(needle.len()).any(|w| w == needle.as_slice()),
            "capability secret must never appear in the signed event bytes"
        );
    }

    #[test]
    fn built_invite_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture(None);
        // The full §6 stateless pipeline must accept our own event.
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built member.invited must validate");
        let (id, dev) = admin_keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::MemberInvited
        );
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
    fn golden_event_id_is_stable() {
        // Regression lock on the exact bytes (see GOLDEN_EVENT_ID_HEX note).
        let f = build_fixture(None);
        assert_eq!(
            hex::encode(f.event.event_id().as_bytes()),
            GOLDEN_EVENT_ID_HEX
        );
    }

    // ── AC2: invitee key binding ─────────────────────────────────────────────

    #[test]
    fn invitee_key_is_exactly_the_bound_key() {
        // AC2: the content's invitee_key must be exactly the input identity key,
        // not the admin's sender_id or device_id.
        let invitee = SigningKey::from_seed(&INVITEE_SEED).identity_key();
        let (admin_id, _admin_dev) = admin_keys();
        let f = build_fixture(None);
        let Content::MemberInvited(c) = f.event.content else {
            panic!("expected member.invited content");
        };
        assert_eq!(
            c.invitee_key, invitee,
            "invitee_key must equal the passed-in key"
        );
        assert_ne!(
            c.invitee_key,
            admin_id.identity_key(),
            "invitee_key must not be the admin's identity key"
        );
    }

    // ── expiry path through the stateless validator ──────────────────────────

    #[test]
    fn built_invite_with_expiry_passes_stateless_validation() {
        // Regression: an invite with a non-None expires_at must also pass the
        // full stateless pipeline (tests the optional-field code path in the
        // content validator that the no-expiry fixture skips).
        let room_id = fixture_room_id();
        let f = build_fixture(Some(CREATED_AT + 86_400_000));
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("invite with expiry must pass stateless validation");
        let Content::MemberInvited(c) = validated.event.content else {
            panic!("expected member.invited content");
        };
        assert_eq!(c.expires_at, Some(CREATED_AT + 86_400_000));
    }

    // ── capability_hash input isolation ──────────────────────────────────────

    #[test]
    fn capability_hash_input_isolation() {
        // The hash must change whenever any input changes; this rules out
        // length-extension and field-collision attacks.
        let room_id = fixture_room_id();
        let other_room = RoomId::from_bytes([0xff; 32]);
        let other_invite_id: [u8; 16] = [0xee; 16];
        let other_secret: [u8; 16] = [0xcc; 16];

        let base = capability_hash(&room_id, &INVITE_ID, &SECRET);
        assert_ne!(
            base,
            capability_hash(&other_room, &INVITE_ID, &SECRET),
            "different room_id must yield a different hash"
        );
        assert_ne!(
            base,
            capability_hash(&room_id, &other_invite_id, &SECRET),
            "different invite_id must yield a different hash"
        );
        assert_ne!(
            base,
            capability_hash(&room_id, &INVITE_ID, &other_secret),
            "different secret must yield a different hash"
        );
    }
}
