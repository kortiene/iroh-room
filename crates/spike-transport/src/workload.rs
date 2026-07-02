//! The shared signed-`WireEvent` workload (spec §7.5).
//!
//! The issue Test Plan requires "the same signed event payloads" through both
//! backends. This module reuses `iroh-rooms-core`'s public, deterministic event
//! builders with **seed-derived keys and injected clocks** — the same technique
//! the core conformance fixtures use — to produce a fixed ordered list of signed
//! `WireEvent`s: a `room.created` genesis followed by `M` `message.text` events,
//! each citing the previous event as its sole `prev_events` parent. No wall-clock
//! or RNG on this path, so the exact bytes (hence every `event_id`) are identical
//! on every run — the numbers this spike measures are CI-reproducible.
//!
//! The transport backends never authorize or fold these events (that is the log
//! layer's job, out of scope here, spec §2 non-goals); they move the verbatim
//! bytes and dedup by the recomputed `event_id`, so a single signing identity is
//! sufficient for the whole workload.

use iroh_rooms_core::event::constants::SHORT_ID_LEN;
use iroh_rooms_core::event::signed::{derive_room_id, event_id_from_bytes};
use iroh_rooms_core::event::{
    build_message_text, build_room_created, EventId, SigningKey, WireEvent,
};

/// Seed for the workload's sole signing identity (genesis creator + message author).
const IDENTITY_SEED: [u8; 32] = [0x51; 32];
/// Seed for the workload's sole device key.
const DEVICE_SEED: [u8; 32] = [0x52; 32];
/// Fixed room nonce (spec: deterministic inputs only).
const ROOM_NONCE: [u8; SHORT_ID_LEN] = [0x53; SHORT_ID_LEN];
/// Fixed genesis `created_at` (an injected clock reading, not a live one).
const GENESIS_CREATED_AT: u64 = 1_750_000_000_000;

/// A deterministic ordered `WireEvent` workload: `wires[0]` is the `room.created`
/// genesis, `wires[1..]` are `M` `message.text` events each citing the previous
/// event as its parent.
#[derive(Debug, Clone)]
pub struct Workload {
    /// The ordered signed events (genesis first).
    pub wires: Vec<WireEvent>,
}

impl Workload {
    /// Build the workload: one genesis plus `message_count` `message.text`
    /// events. Byte-identical (hence `event_id`-identical) on every call.
    #[must_use]
    pub fn build(message_count: usize) -> Self {
        let identity = SigningKey::from_seed(&IDENTITY_SEED);
        let device = SigningKey::from_seed(&DEVICE_SEED);
        let room_id = derive_room_id(&identity.identity_key(), &ROOM_NONCE, GENESIS_CREATED_AT);

        let genesis = build_room_created(
            &identity,
            &device,
            "spike-transport room",
            &ROOM_NONCE,
            GENESIS_CREATED_AT,
        );
        let mut prev = vec![event_id_from_bytes(&genesis.signed)];
        let mut wires = Vec::with_capacity(message_count + 1);
        wires.push(genesis);

        for i in 0..message_count {
            let created_at = GENESIS_CREATED_AT + 1000 * (i as u64 + 1);
            let body = format!("spike-transport message {i}");
            let wire = build_message_text(
                &identity,
                &device,
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

        Self { wires }
    }

    /// The recomputed `event_id`s of every event in the workload, in order
    /// (genesis first) — the published set the equality oracle compares
    /// `received_ids()` against.
    #[must_use]
    pub fn event_ids(&self) -> Vec<EventId> {
        self.wires
            .iter()
            .map(|w| event_id_from_bytes(&w.signed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Workload;
    use std::collections::BTreeSet;

    #[test]
    fn build_is_deterministic() {
        let a = Workload::build(5);
        let b = Workload::build(5);
        for (wa, wb) in a.wires.iter().zip(b.wires.iter()) {
            assert_eq!(
                wa.to_bytes(),
                wb.to_bytes(),
                "workload must be byte-identical run to run"
            );
        }
    }

    #[test]
    fn genesis_plus_message_count_events() {
        let w = Workload::build(4);
        assert_eq!(w.wires.len(), 5, "genesis + 4 messages");
        assert_eq!(w.event_ids().len(), 5);
    }

    #[test]
    fn zero_messages_is_genesis_only() {
        let w = Workload::build(0);
        assert_eq!(w.wires.len(), 1);
    }

    #[test]
    fn every_event_id_is_distinct() {
        let w = Workload::build(10);
        let ids: BTreeSet<_> = w.event_ids().into_iter().collect();
        assert_eq!(ids.len(), 11, "all 11 event ids must be distinct");
    }

    #[test]
    fn genesis_is_a_parentless_room_created() {
        use iroh_rooms_core::event::signed::SignedEvent;
        use iroh_rooms_core::event::Content;

        let w = Workload::build(2);
        let genesis = SignedEvent::decode(&w.wires[0].signed).expect("decode genesis");
        assert!(
            matches!(genesis.content, Content::RoomCreated(_)),
            "wires[0] must be the room.created genesis"
        );
        assert!(
            genesis.prev_events.is_empty(),
            "the genesis event cites no parents"
        );
    }

    #[test]
    fn smaller_workload_is_a_byte_prefix_of_a_larger_one() {
        // Every backend and every N=2..5 run is fed byte-identical payloads, so
        // any set difference is a transport property, not a payload one (spec
        // §7.5). A shorter run must be a strict byte-prefix of a longer one.
        let small = Workload::build(2);
        let large = Workload::build(5);
        assert!(small.wires.len() < large.wires.len());
        for (i, w) in small.wires.iter().enumerate() {
            assert_eq!(
                w.to_bytes(),
                large.wires[i].to_bytes(),
                "event {i} must be byte-identical regardless of workload size"
            );
        }
    }

    #[test]
    fn messages_form_a_linear_chain() {
        // Each message.text's prev_events is exactly the previous event's id.
        use iroh_rooms_core::event::signed::SignedEvent;
        use iroh_rooms_core::event::Content;

        let w = Workload::build(3);
        let ids = w.event_ids();
        for i in 1..w.wires.len() {
            let ev = SignedEvent::decode(&w.wires[i].signed).expect("decode");
            assert_eq!(
                ev.prev_events,
                vec![ids[i - 1]],
                "event {i} must cite event {}",
                i - 1
            );
            assert!(matches!(ev.content, Content::MessageText(_)));
        }
    }
}
