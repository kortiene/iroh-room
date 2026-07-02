//! Pure `agent.status` event assembly: build and sign a member-authored status
//! update (Event Protocol §7).
//!
//! This is the single byte-exact place an `agent.status` event is assembled from
//! a member's keys — the sibling of
//! [`build_message_text`](super::message::build_message_text) and
//! [`build_file_shared`](super::file::build_file_shared). It is **deterministic**
//! in its inputs — the caller injects the `prev_events` (the room heads) and
//! `created_at` (a clock read) — so this function is itself clock-/RNG-free and
//! golden-testable (the only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate)).
//!
//! `agent.status` carries **no** embedded `device_binding`: it is a
//! membership-device-bound type (`requires_membership_device_binding == true`), so
//! the author's device is resolved from the membership fold rather than from the
//! event itself. The event is signed by the author's **device** secret; the
//! signature MUST verify under `device_id`, never `sender_id` — identical to
//! `message.text` / `file.shared`.
//!
//! Despite the CLI noun (`agent status`), posting is **not** role-gated: any
//! active member may author one (Spike §7: "any current member, typically
//! `role == agent`"). Authorization is the ordinary `gate_active_member` check.
//!
//! The builder does **not** enforce the §7 length/count caps on `status` /
//! `message` / `related_artifact_ids` / `progress_pct`: those are enforced by the
//! strict content parser on decode/validate. Callers that want a friendly pre-IO
//! error validate the fields before building (the CLI does).

use super::constants::{SCHEMA_VERSION, SHORT_ID_LEN};
use super::content::{AgentStatus, Content, EventType};
use super::ids::{EventId, RoomId};
use super::keys::SigningKey;
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Assemble and sign a member-authored `agent.status` event (Event Protocol §7).
///
/// The `sender_identity_secret` provides `sender_id` (the authorizing membership
/// identity); the `sender_device_secret` signs the event (the signature MUST
/// verify under `device_id`). The two are passed separately, mirroring
/// [`build_message_text`](super::message::build_message_text).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `prev_events` (the room heads) and `created_at` (a clock read) are injected by
/// the caller so this stays free of wall-clock and RNG. `message == None` is
/// omitted; an empty `related_artifact_ids` slice is omitted entirely (`None`),
/// never encoded as an empty array (the §7 omit-when-empty rule).
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_message_text; each arg is a distinct signed field
pub fn build_agent_status(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    status: &str,
    message: Option<&str>,
    related_artifact_ids: &[[u8; SHORT_ID_LEN]],
    progress_pct: Option<u64>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent {
    let sender_id = sender_identity_secret.identity_key();
    let device_id = sender_device_secret.device_key();

    let content = Content::AgentStatus(AgentStatus {
        status: status.to_owned(),
        message: message.map(ToOwned::to_owned),
        related_artifact_ids: (!related_artifact_ids.is_empty())
            .then(|| related_artifact_ids.to_vec()),
        progress_pct,
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::AgentStatus,
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
    //! L1 — the pure `agent.status` builder (spec IR-0208 §11 L1). Mirrors the
    //! sibling `message.rs` builder suite: determinism, full-field round-trip,
    //! omit-when-empty, signature-under-`device_id` (never `sender_id`), the golden
    //! `event_id` regression lock, boundary accepts, and the stateless/authorization
    //! rejections (`NotGenesisDescended`, `RoomIdMismatch`, `NotAMember`). The
    //! builder itself enforces no D1 caps — the strict parser does — so an over-cap
    //! field is proven rejected by `validate_wire_bytes`, not by the builder.

    use super::build_agent_status;
    use crate::event::constants::{
        MAX_ARTIFACT_REFS, MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, SHORT_ID_LEN,
    };
    use crate::event::content::{Content, EventType};
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::{DeviceKey, IdentityKey, SigningKey};
    use crate::event::reject::{MembershipOracle, RejectReason};
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{
        validate_wire_bytes, validate_with_membership, ValidationContext,
    };
    use crate::event::wire::WireEvent;

    // Deterministic in-test fixtures (spec §11 L1). Implementation-pinned
    // regression locks, not published conformance vectors.
    const SENDER_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const SENDER_DEVICE_SEED: [u8; 32] = [0x02; 32];
    // Genesis golden inputs (event/genesis.rs vector) feed a real room_id — the same
    // nonce/created_at the message.rs builder suite pins, so the two golden ids are
    // independent, comparable regression locks over the shared envelope.
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;
    const STATUS: &str = "running_tests";
    // Implementation-pinned regression lock: the event id our builder produces for
    // the bare-status fixture above (no message/artifacts/progress). Recompute &
    // update only on an intentional byte-format change to `agent.status` (breaking).
    const GOLDEN_EVENT_ID_HEX: &str =
        "417cf970a3cffa69c8a31ee6e8266af2d1056384c0bde0cf24950a91c2dcacac";

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

    /// A non-empty `prev_events` (agent.status is a non-genesis event). One
    /// synthetic head id stands in for the room's DAG heads.
    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    struct Built {
        wire: WireEvent,
        event: SignedEvent,
    }

    /// Build a bare-status event (no optionals) with the pinned fixtures.
    fn build_bare(status: &str) -> Built {
        let (id, dev) = keys();
        let wire = build_agent_status(
            &id,
            &dev,
            &fixture_room_id(),
            status,
            None,
            &[],
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("agent.status must decode");
        Built { wire, event }
    }

    fn build_fixture() -> Built {
        build_bare(STATUS)
    }

    fn agent_status(event: SignedEvent) -> AgentStatusFields {
        let Content::AgentStatus(c) = event.content else {
            panic!("expected agent.status content");
        };
        AgentStatusFields {
            status: c.status,
            message: c.message,
            related_artifact_ids: c.related_artifact_ids,
            progress_pct: c.progress_pct,
        }
    }

    struct AgentStatusFields {
        status: String,
        message: Option<String>,
        related_artifact_ids: Option<Vec<[u8; SHORT_ID_LEN]>>,
        progress_pct: Option<u64>,
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
        let artifacts = [[0x11u8; SHORT_ID_LEN], [0x22u8; SHORT_ID_LEN]];
        let wire = build_agent_status(
            &id,
            &dev,
            &fixture_room_id(),
            "blocked",
            Some("waiting on review"),
            &artifacts,
            Some(40),
            &fixture_heads(),
            CREATED_AT,
        );
        let event = SignedEvent::decode(&wire.signed).expect("must decode");
        assert_eq!(event.prev_events, fixture_heads());
        assert_eq!(event.created_at, CREATED_AT);
        assert_eq!(event.event_type, EventType::AgentStatus);
        let c = agent_status(event);
        assert_eq!(c.status, "blocked");
        assert_eq!(c.message.as_deref(), Some("waiting on review"));
        assert_eq!(c.related_artifact_ids, Some(artifacts.to_vec()));
        assert_eq!(c.progress_pct, Some(40));
    }

    #[test]
    fn absent_optionals_round_trip_as_none() {
        let c = agent_status(build_fixture().event);
        assert_eq!(c.status, STATUS);
        assert_eq!(c.message, None);
        assert_eq!(c.related_artifact_ids, None);
        assert_eq!(c.progress_pct, None);
    }

    #[test]
    fn empty_message_and_artifacts_are_omitted() {
        // An empty `message`/artifacts slice must omit the field entirely (None),
        // not encode an empty string/array — the §7 omit-when-empty rule the strict
        // parser enforces (an empty artifact array is a hard InvalidContent reject).
        let (id, dev) = keys();
        let wire = build_agent_status(
            &id,
            &dev,
            &fixture_room_id(),
            STATUS,
            None,
            &[],
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        let c = agent_status(SignedEvent::decode(&wire.signed).expect("must decode"));
        assert_eq!(c.related_artifact_ids, None, "empty artifacts → None");
        assert_eq!(c.message, None, "absent message → None");
    }

    #[test]
    fn built_status_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let f = build_fixture();
        let validated =
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("freshly built agent.status must validate");
        let (id, dev) = keys();
        assert_eq!(validated.event.sender_id, id.identity_key());
        assert_eq!(validated.event.device_id, dev.device_key());
        assert_eq!(validated.event.event_type, EventType::AgentStatus);
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
        let f = build_fixture();
        let msg = signed::event_signing_message(&f.wire.signed);
        let sender_as_device = DeviceKey::from_bytes(*f.event.sender_id.as_bytes());
        assert!(
            sender_as_device.verify(&msg, &f.wire.sig).is_err(),
            "an agent.status signature must never verify under sender_id"
        );
        // And the two keys are genuinely distinct (guards a degenerate fixture).
        assert_ne!(
            f.event.sender_id.as_bytes(),
            f.event.device_id.as_bytes(),
            "identity and device keys must differ"
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
    fn distinct_statuses_produce_distinct_event_ids() {
        let a = build_bare("running");
        let b = build_bare("done");
        assert_ne!(
            a.event.event_id(),
            b.event.event_id(),
            "distinct status labels must produce distinct event_ids"
        );
    }

    #[test]
    fn tampered_byte_breaks_id_and_signature() {
        let f = build_fixture();
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

    // ── boundary accepts (the strict parser accepts exactly at the cap) ─────────

    #[test]
    fn status_at_cap_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let status = "a".repeat(MAX_STATUS_LABEL_BYTES);
        let f = build_bare(&status);
        validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("a status label exactly at the cap must validate");
    }

    #[test]
    fn message_at_cap_passes_stateless_validation() {
        let room_id = fixture_room_id();
        let (id, dev) = keys();
        let message = "m".repeat(MAX_STATUS_MESSAGE_BYTES);
        let wire = build_agent_status(
            &id,
            &dev,
            &room_id,
            STATUS,
            Some(&message),
            &[],
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("a message exactly at the cap must validate");
    }

    #[test]
    fn progress_zero_and_100_and_artifacts_at_cap_validate() {
        let room_id = fixture_room_id();
        let (id, dev) = keys();
        let artifacts: Vec<[u8; SHORT_ID_LEN]> = (0..MAX_ARTIFACT_REFS)
            .map(|i| {
                let mut a = [0u8; SHORT_ID_LEN];
                a[0] = u8::try_from(i).unwrap();
                a
            })
            .collect();
        for pct in [0u64, 100] {
            let wire = build_agent_status(
                &id,
                &dev,
                &room_id,
                STATUS,
                None,
                &artifacts,
                Some(pct),
                &fixture_heads(),
                CREATED_AT,
            );
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .unwrap_or_else(|e| {
                    panic!("progress={pct} with {MAX_ARTIFACT_REFS} artifacts must validate: {e:?}")
                });
        }
    }

    // ── the builder enforces no caps; the strict validator does ─────────────────

    #[test]
    fn status_over_cap_is_rejected_by_validation() {
        // The builder does not enforce the D1 cap; validate_wire_bytes must. Build
        // the wire directly (decoding an over-cap event would itself reject it via
        // the strict parser), then assert validation refuses the raw bytes.
        let room_id = fixture_room_id();
        let (id, dev) = keys();
        let wire = build_agent_status(
            &id,
            &dev,
            &room_id,
            &"a".repeat(MAX_STATUS_LABEL_BYTES + 1),
            None,
            &[],
            None,
            &fixture_heads(),
            CREATED_AT,
        );
        assert_eq!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .unwrap_err(),
            RejectReason::InvalidContent,
            "a status label one byte over the cap must be rejected as InvalidContent"
        );
    }

    #[test]
    fn progress_over_100_is_rejected_by_validation() {
        let room_id = fixture_room_id();
        let (id, dev) = keys();
        let wire = build_agent_status(
            &id,
            &dev,
            &room_id,
            STATUS,
            None,
            &[],
            Some(101),
            &fixture_heads(),
            CREATED_AT,
        );
        assert_eq!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .unwrap_err(),
            RejectReason::InvalidContent,
            "progress_pct=101 must be rejected as InvalidContent"
        );
    }

    // ── stateless / authorization rejections ────────────────────────────────────

    #[test]
    fn status_with_no_prev_events_is_rejected_as_not_genesis_descended() {
        // agent.status is a non-genesis type and MUST cite at least one parent.
        let (id, dev) = keys();
        let room_id = fixture_room_id();
        let wire = build_agent_status(
            &id,
            &dev,
            &room_id,
            STATUS,
            None,
            &[],
            None,
            &[],
            CREATED_AT,
        );
        assert_eq!(
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .unwrap_err(),
            RejectReason::NotGenesisDescended,
            "agent.status with empty prev_events must be rejected as not_genesis_descended"
        );
    }

    #[test]
    fn status_for_wrong_room_context_is_rejected() {
        let f = build_fixture();
        let other_room = RoomId::from_bytes([0xFF; 32]);
        assert_eq!(
            validate_wire_bytes(&f.wire.to_bytes(), &ValidationContext::for_room(other_room))
                .unwrap_err(),
            RejectReason::RoomIdMismatch,
            "agent.status validated in the wrong room context must return RoomIdMismatch"
        );
    }

    #[test]
    fn non_member_status_is_rejected_by_membership_oracle() {
        // AC2 — a non-member's status is rejected. validate_with_membership
        // delegates authorization to the MembershipOracle; a denying oracle must
        // cause the pipeline to return NotAMember.
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
            "agent.status from a non-member must be rejected with NotAMember"
        );
    }
}
