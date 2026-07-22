//! The admin-authored signed-`WireEvent` workload (spec §6.5 / D5). Every load
//! event is a `message.text` authored by node 0's admin identity on a linear
//! `prev_events` chain, so we can keep >5 transport participants without
//! requiring >5 active membership authors (D1 caveat). The transport never
//! re-authorizes membership — that is the log layer's job — so a single admin
//! author is sufficient to fan out every load event through the mesh.
//!
//! Byte-deterministic (hence `event_id`-deterministic): the same
//! `Workload::build(...)` call produces byte-identical wires every run, so a
//! re-run's `accepted` / `frames_sent` / delivery-set comparisons are
//! attributable purely to transport behavior, not to payload drift.

use iroh_rooms_core::event::build_message_text;
use iroh_rooms_core::event::constants::SHORT_ID_LEN;
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, event_id_from_bytes, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;

/// A deterministic admin-authored `message.text` chain: `wires[0]` parents on
/// `genesis_id`; `wires[k]` parents on `wires[k-1]`'s id. Byte-identical on
/// every `Workload::build(...)` call with the same inputs.
///
/// Note: the admin signing keys are **not** held on the struct (the
/// `iroh_rooms_core::event::keys::SigningKey` is not `Clone`, and the wires
/// are pre-built during construction so no further signing is needed).
#[derive(Debug, Clone)]
pub struct Workload {
    /// The room the events are scoped to.
    pub room_id: RoomId,
    /// The admin author's identity key (`sender_id`).
    pub admin_identity: IdentityKey,
    /// The genesis id this chain parents on.
    pub genesis_id: EventId,
    /// The ordered signed event wires, each a `message.text`.
    pub wires: Vec<WireEvent>,
}

impl Workload {
    /// Build an admin-authored `message_count`-event chain parented on
    /// `genesis_id`, scoped to `room_id`, signed by the admin node 0.
    ///
    /// # Arguments
    /// * `room_id` - The room the events belong to.
    /// * `admin_identity_secret` - The admin's identity key seed source.
    /// * `admin_device_secret` - The admin's device signing key.
    /// * `genesis_id` - The genesis event id (the chain root).
    /// * `message_count` - How many `message.text` events to build.
    /// * `base_created_at` - The deterministic `created_at` for event 0; each
    ///   subsequent event advances by 1000 ms.
    /// * `body_prefix` - Body text prefix (each event's body is
    ///   `format!("{body_prefix} seq=<k>")`).
    #[must_use]
    pub fn build(
        room_id: RoomId,
        admin_identity_secret: &SigningKey,
        admin_device_secret: &SigningKey,
        genesis_id: EventId,
        message_count: usize,
        base_created_at: u64,
        body_prefix: &str,
    ) -> Self {
        let admin_identity = admin_identity_secret.identity_key();
        let mut prev = vec![genesis_id];
        let mut wires = Vec::with_capacity(message_count);
        for k in 0..message_count {
            let created_at = base_created_at + 1000 * (k as u64 + 1);
            let body = format!("{body_prefix} seq={k}");
            let wire = build_message_text(
                admin_identity_secret,
                admin_device_secret,
                &room_id,
                &body,
                None,
                None,
                &[],
                &prev,
                created_at,
            );
            prev = vec![event_id_from_bytes(&wire.signed)];
            wires.push(wire);
        }

        Self {
            room_id,
            admin_identity,
            genesis_id,
            wires,
        }
    }

    /// The recomputed `event_id`s of every wire in this workload, in order.
    #[must_use]
    pub fn event_ids(&self) -> Vec<EventId> {
        self.wires
            .iter()
            .map(|w| event_id_from_bytes(&w.signed))
            .collect()
    }
}

/// The fixed room nonce used by the harness when deriving a room id from a
/// seeded admin identity (spec §6.4 step 1 — deterministic inputs only).
#[must_use]
pub fn deterministic_room_nonce(seed_base: u64) -> [u8; SHORT_ID_LEN] {
    let mut nonce = [0u8; SHORT_ID_LEN];
    let bytes = seed_base.to_le_bytes();
    for chunk in nonce.chunks_mut(8) {
        let n = chunk.len().min(8);
        chunk[..n].copy_from_slice(&bytes[..n]);
    }
    nonce
}

/// Build the deterministic room genesis `WireEvent` bytes authored by the
/// admin node 0, parented on nothing. Returns `(room_id, genesis_wire)`.
///
/// This is the spike-local mirror of `iroh_rooms_net::demo::genesis` but
/// driven off a `u64` seed so a re-run reproduces byte-identical bytes (the
/// demo module fixes `T0` and `NONCE` as constants, which is also deterministic
/// but does not let each scenario vary them to avoid room-id collisions across
/// scenarios in one process).
///
/// # Panics
///
/// Panics if the freshly-built genesis fails to decode (a programming error
/// — `build_room_created` produces canonical bytes the decoder accepts).
#[must_use]
pub fn build_genesis_for_admin(
    admin_identity_secret: &SigningKey,
    admin_device_secret: &SigningKey,
    room_name: &str,
    nonce: &[u8; SHORT_ID_LEN],
    created_at: u64,
) -> (RoomId, WireEvent) {
    let admin_identity = admin_identity_secret.identity_key();
    let room_id = signed::derive_room_id(&admin_identity, nonce, created_at);
    let wire = iroh_rooms_core::event::build_room_created(
        admin_identity_secret,
        admin_device_secret,
        room_name,
        nonce,
        created_at,
    );
    let derived = SignedEvent::decode(&wire.signed).expect("freshly-built genesis decodes");
    // `build_room_created` recomputes the same room_id internally; the assert
    // pins that invariant.
    assert_eq!(derived.room_id, room_id);
    (room_id, wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_rooms_core::event::Content;

    const IDENTITY_SEED: [u8; 32] = [0x70; 32];
    const DEVICE_SEED: [u8; 32] = [0x71; 32];
    const BASE_T: u64 = 1_770_000_000_000;

    fn admin_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&IDENTITY_SEED),
            SigningKey::from_seed(&DEVICE_SEED),
        )
    }

    #[test]
    fn build_is_deterministic_across_runs() {
        let nonce = deterministic_room_nonce(0xABCD);
        let (room, genesis) = {
            let (id, dev) = admin_keys();
            build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T)
        };
        let gid = event_id_from_bytes(&genesis.signed);

        let a = {
            let (id, dev) = admin_keys();
            Workload::build(room, &id, &dev, gid, 5, BASE_T, "n40 load")
        };
        let b = {
            let (id, dev) = admin_keys();
            Workload::build(room, &id, &dev, gid, 5, BASE_T, "n40 load")
        };
        for (wa, wb) in a.wires.iter().zip(b.wires.iter()) {
            assert_eq!(
                wa.to_bytes(),
                wb.to_bytes(),
                "workload must be byte-identical run to run"
            );
        }
    }

    #[test]
    fn chain_parents_each_event_on_the_previous_one() {
        let (id, dev) = admin_keys();
        let nonce = deterministic_room_nonce(0x1);
        let (room, genesis) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        let gid = event_id_from_bytes(&genesis.signed);

        let w = Workload::build(room, &id, &dev, gid, 4, BASE_T, "n40");
        let ids = w.event_ids();

        // Event 0 parents on the genesis; each subsequent event on the prior.
        let e0 = SignedEvent::decode(&w.wires[0].signed).expect("decode");
        assert_eq!(e0.prev_events, vec![gid]);
        for k in 1..w.wires.len() {
            let ev = SignedEvent::decode(&w.wires[k].signed).expect("decode");
            assert_eq!(ev.prev_events, vec![ids[k - 1]], "event {k} parent linkage");
        }
    }

    #[test]
    fn every_event_id_is_distinct() {
        let (id, dev) = admin_keys();
        let nonce = deterministic_room_nonce(0x2);
        let (room, genesis) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        let gid = event_id_from_bytes(&genesis.signed);
        let w = Workload::build(room, &id, &dev, gid, 8, BASE_T, "n40");
        let unique: std::collections::BTreeSet<_> = w.event_ids().into_iter().collect();
        assert_eq!(unique.len(), 8);
    }

    #[test]
    fn every_event_is_a_message_text_from_the_admin() {
        let (id, dev) = admin_keys();
        let nonce = deterministic_room_nonce(0x3);
        let (room, genesis) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        let gid = event_id_from_bytes(&genesis.signed);
        let admin_identity = id.identity_key();
        let admin_device = dev.device_key();
        let w = Workload::build(room, &id, &dev, gid, 3, BASE_T, "n40 load");
        for wire in &w.wires {
            let ev = SignedEvent::decode(&wire.signed).expect("decode");
            assert_eq!(ev.sender_id, admin_identity);
            assert_eq!(ev.device_id, admin_device);
            assert!(matches!(ev.content, Content::MessageText(_)));
        }
    }

    #[test]
    fn zero_messages_works() {
        let (id, dev) = admin_keys();
        let nonce = deterministic_room_nonce(0x4);
        let (room, genesis) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        let gid = event_id_from_bytes(&genesis.signed);
        let w = Workload::build(room, &id, &dev, gid, 0, BASE_T, "n40");
        assert!(w.wires.is_empty());
        assert!(w.event_ids().is_empty());
    }

    #[test]
    fn deterministic_room_nonce_is_seed_sensitive_and_deterministic() {
        assert_eq!(
            deterministic_room_nonce(0x1234),
            deterministic_room_nonce(0x1234)
        );
        assert_ne!(
            deterministic_room_nonce(0x1234),
            deterministic_room_nonce(0x5678)
        );
    }

    #[test]
    fn build_genesis_is_deterministic_same_admin() {
        let (id, dev) = admin_keys();
        let nonce = deterministic_room_nonce(0x5);
        let (r1, g1) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        let (r2, g2) = build_genesis_for_admin(&id, &dev, "n40", &nonce, BASE_T);
        assert_eq!(r1, r2);
        assert_eq!(g1.to_bytes(), g2.to_bytes());
    }
}
