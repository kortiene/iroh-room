//! Deterministic room fixtures shared by the `net-smoke` binary and the loopback
//! tests (prototype/demo scaffolding, mirroring `spike-blobs::roster`).
//!
//! This module hardcodes seeded identities and builds the minimal signed events a
//! transport demo needs (a room genesis + an admin message). It exists only so the
//! prototype can exercise the carrier without the real CLI identity/room layer
//! (spec N1/N3); it is **not** part of the production surface and will be dropped
//! when the CLI wires real identities and invite tickets.
//!
//! Identity alignment (Membership §1 / spec A2): a participant's iroh endpoint
//! secret is derived from the **same** 32-byte seed as its event-signing device
//! key, so `endpoint.id() == device_id == EndpointId` byte-for-byte — both go
//! through `ed25519_dalek::SigningKey::from_bytes`.

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{Content, EventType, MessageText, RoomCreated};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;

use crate::admission::AllowlistAdmission;

/// Fixed room nonce for the demo room.
const NONCE: [u8; 16] = [0xab; 16];
/// Fixed base timestamp (advisory `created_at`).
const T0: u64 = 1_750_000_000_000;

/// A demo participant: a stable identity key (`sender_id`) plus a distinct device
/// key (`device_id`), seeded deterministically.
pub struct Participant {
    id: SigningKey,
    dev: SigningKey,
}

impl Participant {
    /// Build a participant from a one-byte seed. The identity key uses `seed`; the
    /// device key uses `seed ^ 0x80` so the two keys differ (Event Protocol §1).
    #[must_use]
    pub fn new(seed: u8) -> Self {
        Self {
            id: SigningKey::from_seed(&[seed; 32]),
            dev: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
        }
    }

    /// The participant's identity key (`sender_id`).
    #[must_use]
    pub fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }

    /// The participant's device key (`device_id`), as the event layer sees it.
    #[must_use]
    pub fn device(&self) -> DeviceKey {
        self.dev.device_key()
    }

    /// The iroh endpoint secret for this participant (== the device signing key).
    #[must_use]
    pub fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev.to_seed())
    }

    /// The participant's authenticated transport identity (`EndpointId`). Equal,
    /// byte-for-byte, to [`device`](Self::device) (spec A2).
    #[must_use]
    pub fn endpoint_id(&self) -> EndpointId {
        self.iroh_secret().public()
    }
}

/// The demo room id, derived from the host's identity + the fixed nonce/time.
#[must_use]
pub fn room_id(host: &Participant) -> RoomId {
    signed::derive_room_id(&host.identity(), &NONCE, T0)
}

/// Seal a signed event into verbatim `WireEvent` bytes signed by `dev`.
fn wire(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// Build the room genesis (`RoomCreated`) authored by `host`. Returns the room id,
/// the genesis event id, and the verbatim `WireEvent` bytes to publish.
#[must_use]
pub fn genesis(host: &Participant) -> (RoomId, EventId, Vec<u8>) {
    let room = room_id(host);
    let event = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: host.identity(),
        device_id: host.device(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Net Smoke".to_owned(),
            room_nonce: NONCE,
            admins: vec![host.identity()],
            device_binding: DeviceBinding::create(&room, &host.id, host.device()),
        }),
    };
    let eid = event.event_id();
    (room, eid, wire(&event, &host.dev))
}

/// Build an admin-authored `MessageText` parented on the genesis (a second, live
/// event used to prove a re-established link actually carries data). Returns the
/// event id and the verbatim `WireEvent` bytes.
#[must_use]
pub fn admin_message(
    host: &Participant,
    room: RoomId,
    genesis_id: EventId,
    n: u8,
) -> (EventId, Vec<u8>) {
    let event = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: host.identity(),
        device_id: host.device(),
        event_type: EventType::MessageText,
        created_at: T0 + 1 + u64::from(n),
        prev_events: vec![genesis_id],
        content: Content::MessageText(MessageText {
            body: format!("hello {n}"),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let eid = event.event_id();
    (eid, wire(&event, &host.dev))
}

/// Build an [`AllowlistAdmission`](crate::admission::AllowlistAdmission) that binds
/// every listed participant's device → identity and marks each identity Active —
/// the same shape the membership fold produces (spec D6).
#[must_use]
pub fn allowlist(members: &[&Participant]) -> AllowlistAdmission {
    let mut auth = AllowlistAdmission::new();
    for m in members {
        auth = auth
            .bind_device(m.endpoint_id(), m.identity())
            .set_active(m.identity());
    }
    auth
}

#[cfg(test)]
mod tests {
    use super::Participant;
    use crate::admission::{Admission, AdmissionDecision, RejectCause};

    #[test]
    fn endpoint_id_equals_device_id_byte_for_byte() {
        // The load-bearing identity-unification invariant (Membership §1 / A2).
        let p = Participant::new(7);
        assert_eq!(
            p.endpoint_id().as_bytes(),
            p.device().as_bytes(),
            "iroh EndpointId must equal the event-layer device_id"
        );
    }

    #[test]
    fn identity_and_device_keys_differ() {
        let p = Participant::new(7);
        assert_ne!(p.identity().as_bytes(), p.device().as_bytes());
    }

    #[test]
    fn different_seeds_produce_distinct_participants() {
        let p1 = Participant::new(1);
        let p2 = Participant::new(2);
        assert_ne!(p1.endpoint_id(), p2.endpoint_id());
        assert_ne!(p1.identity(), p2.identity());
        assert_ne!(p1.device(), p2.device());
    }

    #[test]
    fn genesis_is_deterministic_same_host() {
        let host = Participant::new(5);
        let (r1, id1, bytes1) = super::genesis(&host);
        let (r2, id2, bytes2) = super::genesis(&host);
        assert_eq!(r1, r2, "room_id must be deterministic");
        assert_eq!(id1, id2, "genesis event_id must be deterministic");
        assert_eq!(bytes1, bytes2, "genesis wire bytes must be deterministic");
    }

    #[test]
    fn genesis_room_ids_differ_for_different_hosts() {
        let h1 = Participant::new(10);
        let h2 = Participant::new(11);
        let (r1, _, _) = super::genesis(&h1);
        let (r2, _, _) = super::genesis(&h2);
        assert_ne!(r1, r2, "different hosts must produce different room ids");
    }

    #[test]
    fn allowlist_admits_all_listed_participants() {
        let p1 = Participant::new(20);
        let p2 = Participant::new(21);
        let auth = super::allowlist(&[&p1, &p2]);
        // Both devices must be bound and Active.
        assert!(matches!(
            auth.authorize(p1.endpoint_id()),
            AdmissionDecision::Admit { .. }
        ));
        assert!(matches!(
            auth.authorize(p2.endpoint_id()),
            AdmissionDecision::Admit { .. }
        ));
    }

    #[test]
    fn allowlist_rejects_unlisted_participant() {
        let member = Participant::new(30);
        let stranger = Participant::new(99);
        let auth = super::allowlist(&[&member]);
        assert_eq!(
            auth.authorize(stranger.endpoint_id()),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
    }
}
