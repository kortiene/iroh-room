//! Pure `pipe.opened` / `pipe.closed` event assembly (Event Protocol §7; spec
//! D7).
//!
//! These are the single byte-exact places a pipe-lifecycle event is assembled
//! from a member's keys — the siblings of
//! [`build_message_text`](super::message::build_message_text) and
//! [`build_member_invited`](super::invite::build_member_invited). They are
//! **deterministic** in their inputs: the caller injects `prev_events` (the room
//! heads), `created_at` (a clock read), and the CSPRNG `pipe_id`, so the builders
//! are themselves clock-/RNG-free and golden-testable (the only RNG in `core`
//! stays inside [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! Neither event carries an embedded `device_binding`: both are
//! membership-device-bound types, so the author's device is resolved from the
//! membership fold rather than from the event itself. They are signed by the
//! author's **device** secret; the signature MUST verify under `device_id`, never
//! `sender_id` (Event Protocol §1).
//!
//! Field invariants the §7 strict content parser enforces on decode/validate
//! (`owner_id == sender_id`, `kind == "tcp"`, `allowed_members` non-empty) are not
//! re-checked here: a builder cannot violate `owner_id == sender_id` (it derives
//! `owner_id` from `owner_identity_secret`) and hardcodes `kind = "tcp"`; an empty
//! `allowed_members` is left for the validator to reject so callers get the one
//! canonical error. The CLI validates `allowed_members` before building for a
//! friendly pre-IO error.

use super::constants::{SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{Content, EventType, PipeClosed, PipeOpened};
use super::ids::{EventId, RoomId};
use super::keys::{DeviceKey, IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign an owner-authored `pipe.opened` event (Event Protocol §7).
///
/// `owner_identity_secret` provides both `sender_id` and `owner_id` (they are the
/// same identity — a pipe is owned by its announcer); `owner_device_secret` signs
/// the event (the signature MUST verify under `device_id`). The two are passed
/// separately, mirroring [`build_member_invited`](super::invite::build_member_invited).
///
/// `kind` is hardcoded `"tcp"` (the only MVP transport, PRD §9.3). `allowed_members`
/// is the trust input and MUST be non-empty (no default-all, PRD §13.2); an empty
/// slice produces an event the strict validator rejects, not a panic. Optional
/// `expires_at` follows the §7 omit-when-empty rule.
///
/// Pure and deterministic: the same inputs yield byte-identical output.
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_member_invited; each arg is a distinct signed field
pub fn build_pipe_opened(
    owner_identity_secret: &SigningKey,
    owner_device_secret: &SigningKey,
    room_id: &RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    owner_endpoint: &DeviceKey,
    label: &str,
    target_hint: &str,
    alpn: &str,
    allowed_members: &[IdentityKey],
    expires_at: Option<u64>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let owner_id = owner_identity_secret.identity_key();
    let device_id = owner_device_secret.device_key();

    let content = Content::PipeOpened(PipeOpened {
        pipe_id,
        owner_id,
        owner_endpoint: *owner_endpoint,
        kind: "tcp".to_owned(),
        label: label.to_owned(),
        target_hint: target_hint.to_owned(),
        alpn: alpn.to_owned(),
        allowed_members: allowed_members.to_vec(),
        expires_at,
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id: owner_id,
        device_id,
        event_type: EventType::PipeOpened,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, owner_device_secret);
    WireEvent::seal(csb, sig)
}

/// Assemble and sign a `pipe.closed` event (Event Protocol §7).
///
/// The signer is the pipe owner **or** the room admin (the §7 registry rule, folded
/// downstream). `signer_identity_secret` provides `sender_id`; `signer_device_secret`
/// signs. `reason` (`closed` | `expired` | `owner_exit` | `error`) follows the §7
/// omit-when-empty rule. Pure and deterministic.
#[must_use]
pub fn build_pipe_closed(
    signer_identity_secret: &SigningKey,
    signer_device_secret: &SigningKey,
    room_id: &RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    reason: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = signer_identity_secret.identity_key();
    let device_id = signer_device_secret.device_key();

    let content = Content::PipeClosed(PipeClosed {
        pipe_id,
        reason: reason.map(ToOwned::to_owned),
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::PipeClosed,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };

    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, signer_device_secret);
    WireEvent::seal(csb, sig)
}

#[cfg(test)]
mod tests {
    use super::{build_pipe_closed, build_pipe_opened};
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::{IdentityKey, SigningKey};
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use crate::event::wire::WireEvent;

    const OWNER_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const OWNER_DEVICE_SEED: [u8; 32] = [0x02; 32];
    const ALLOWED_SEED: [u8; 32] = [0x10; 32];
    const ROOM_NONCE: [u8; 16] = [0xab; 16];
    const CREATED_AT: u64 = 1_750_000_000_000;
    const PIPE_ID: [u8; 16] = [0x33; 16];
    const ALPN: &str = "/iroh-rooms/pipe/1";

    fn owner_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&OWNER_IDENTITY_SEED),
            SigningKey::from_seed(&OWNER_DEVICE_SEED),
        )
    }

    fn fixture_room_id() -> RoomId {
        let identity = SigningKey::from_seed(&OWNER_IDENTITY_SEED);
        signed::derive_room_id(&identity.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    fn allowed() -> IdentityKey {
        SigningKey::from_seed(&ALLOWED_SEED).identity_key()
    }

    fn build_opened(allowed_members: &[IdentityKey], expires_at: Option<u64>) -> WireEvent {
        let (id, dev) = owner_keys();
        build_pipe_opened(
            &id,
            &dev,
            &fixture_room_id(),
            PIPE_ID,
            &dev.device_key(),
            "dev-server",
            "localhost:3000",
            ALPN,
            allowed_members,
            expires_at,
            &fixture_heads(),
            CREATED_AT,
        )
    }

    #[test]
    fn pipe_opened_builder_is_deterministic() {
        let a = build_opened(&[allowed()], None);
        let b = build_opened(&[allowed()], None);
        assert_eq!(
            a.to_bytes(),
            b.to_bytes(),
            "same inputs must yield byte-identical output"
        );
    }

    #[test]
    fn pipe_opened_content_round_trips_every_field() {
        let wire = build_opened(&[allowed()], Some(CREATED_AT + 3_600_000));
        let event = SignedEvent::decode(&wire.signed).expect("pipe.opened must decode");
        let Content::PipeOpened(c) = event.content else {
            panic!("expected pipe.opened content");
        };
        let (id, dev) = owner_keys();
        assert_eq!(c.pipe_id, PIPE_ID);
        assert_eq!(c.owner_id, id.identity_key());
        assert_eq!(c.owner_endpoint, dev.device_key());
        assert_eq!(c.kind, "tcp");
        assert_eq!(c.label, "dev-server");
        assert_eq!(c.target_hint, "localhost:3000");
        assert_eq!(c.alpn, ALPN);
        assert_eq!(c.allowed_members, vec![allowed()]);
        assert_eq!(c.expires_at, Some(CREATED_AT + 3_600_000));
    }

    #[test]
    fn pipe_opened_owner_id_equals_sender_id() {
        // The §7 invariant the strict validator enforces: a builder can never
        // violate it because owner_id is derived from the same identity secret.
        let wire = build_opened(&[allowed()], None);
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let Content::PipeOpened(c) = &event.content else {
            panic!("expected pipe.opened");
        };
        assert_eq!(c.owner_id, event.sender_id);
    }

    #[test]
    fn built_pipe_opened_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let wire = build_opened(&[allowed()], None);
        let validated =
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built pipe.opened must validate");
        assert_eq!(
            validated.event.event_type,
            crate::event::content::EventType::PipeOpened
        );
    }

    #[test]
    fn pipe_opened_with_empty_allowed_members_is_rejected_by_validation() {
        // No default-all (PRD §13.2): allowed_members must be non-empty. The
        // builder does not enforce it; the strict validator does.
        let room_id = fixture_room_id();
        let wire = build_opened(&[], None);
        assert!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id)).is_err(),
            "an empty allowed_members must be rejected by validate_wire_bytes"
        );
    }

    #[test]
    fn pipe_opened_expires_at_omitted_round_trips_as_none() {
        let wire = build_opened(&[allowed()], None);
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let Content::PipeOpened(c) = event.content else {
            panic!("expected pipe.opened");
        };
        assert_eq!(c.expires_at, None);
    }

    #[test]
    fn pipe_opened_signature_verifies_under_device_id_not_sender_id() {
        use crate::event::keys::DeviceKey;
        let wire = build_opened(&[allowed()], None);
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let msg = signed::event_signing_message(&wire.signed);
        event
            .device_id
            .verify(&msg, &wire.sig)
            .expect("signature must verify under device_id");
        let sender_as_device = DeviceKey::from_bytes(*event.sender_id.as_bytes());
        assert!(
            sender_as_device.verify(&msg, &wire.sig).is_err(),
            "a pipe.opened signature must never verify under sender_id"
        );
    }

    fn build_closed(reason: Option<&str>) -> WireEvent {
        let (id, dev) = owner_keys();
        build_pipe_closed(
            &id,
            &dev,
            &fixture_room_id(),
            PIPE_ID,
            reason,
            &fixture_heads(),
            CREATED_AT + 10,
        )
    }

    #[test]
    fn pipe_closed_builder_is_deterministic() {
        assert_eq!(
            build_closed(Some("closed")).to_bytes(),
            build_closed(Some("closed")).to_bytes()
        );
    }

    #[test]
    fn pipe_closed_content_round_trips() {
        let wire = build_closed(Some("owner_exit"));
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let Content::PipeClosed(c) = event.content else {
            panic!("expected pipe.closed content");
        };
        assert_eq!(c.pipe_id, PIPE_ID);
        assert_eq!(c.reason.as_deref(), Some("owner_exit"));
    }

    #[test]
    fn built_pipe_closed_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let wire = build_closed(Some("closed"));
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("freshly built pipe.closed must validate");
    }

    #[test]
    fn pipe_closed_absent_reason_round_trips_as_none() {
        let wire = build_closed(None);
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let Content::PipeClosed(c) = event.content else {
            panic!("expected pipe.closed");
        };
        assert_eq!(c.reason, None);
    }

    #[test]
    fn pipe_closed_invalid_reason_is_rejected_by_validation() {
        // `reason` is an enum (`closed|expired|owner_exit|error`); an out-of-set
        // value must be rejected by the strict content parser.
        let room_id = fixture_room_id();
        let wire = build_closed(Some("nonsense"));
        assert!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id)).is_err(),
            "an out-of-enum reason must be rejected"
        );
    }

    #[test]
    fn pipe_closed_all_four_valid_reason_strings_pass_validation() {
        // The spec defines exactly four valid reason values. `closed` and `owner_exit`
        // are exercised elsewhere; explicitly verify `expired` and `error` too so
        // a forgotten case in the content parser is caught here rather than at
        // runtime.
        let room_id = fixture_room_id();
        for reason in ["closed", "expired", "owner_exit", "error"] {
            let wire = build_closed(Some(reason));
            assert!(
                validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                    .is_ok(),
                "reason '{reason}' must be accepted by validate_wire_bytes"
            );
        }
    }

    #[test]
    fn pipe_opened_with_multiple_allowed_members_preserves_all_of_them() {
        let (id, dev) = owner_keys();
        let member_a = SigningKey::from_seed(&[0x10; 32]).identity_key();
        let member_b = SigningKey::from_seed(&[0x11; 32]).identity_key();
        let member_c = SigningKey::from_seed(&[0x12; 32]).identity_key();
        let allowed = [member_a, member_b, member_c];
        let wire = build_pipe_opened(
            &id,
            &dev,
            &fixture_room_id(),
            PIPE_ID,
            &dev.device_key(),
            "multi",
            "localhost:7000",
            ALPN,
            &allowed,
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("decode");
        let Content::PipeOpened(opened) = event.content else {
            panic!("expected pipe.opened");
        };
        assert_eq!(opened.allowed_members, allowed.to_vec());
    }
}
