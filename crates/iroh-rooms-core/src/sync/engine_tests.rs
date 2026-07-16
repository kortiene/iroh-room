//! Engine tests for the insert-failure recovery path (issue #119).
//!
//! `store_and_fanout` runs **after** the fold has committed an accept, so a
//! failed `store.insert` used to be a swallowed log line: the fold said
//! Accepted, the store lost the event, descendants persisted above a permanent
//! hole, and nothing told the operator. These tests pin the fix: the accepted
//! event is queued and retried on tick (healing the hole locally), fan-out /
//! counters / the push feed are deferred until the insert lands, and an
//! exhausted retry budget surfaces a CRITICAL `store_degraded`
//! [`TrustDecision`] that survives a restart.
//!
//! They live in-crate (like `store/tests.rs`) because the deterministic insert
//! fault injection ([`EventStore::fail_next_inserts`]) is `#[cfg(test)]`-only —
//! integration tests compile the lib without it.

use crate::event::binding::DeviceBinding;
use crate::event::content::{Content, EventType, MessageText, RoomCreated};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::SigningKey;
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::{validate_wire_bytes, ValidationContext};
use crate::event::wire::WireEvent;
use crate::store::EventStore;

use super::engine::{Severity, SyncEngine};
use super::message::{Outgoing, PeerId, SyncMessage};
use super::SyncConfig;

const NONCE: [u8; 16] = [0xAA; 16];
const T0: u64 = 1_750_000_000_000;
const NODE_A: PeerId = PeerId::from_bytes([0xA1; 32]);

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

fn frame_id(bytes: &[u8], room: RoomId) -> EventId {
    validate_wire_bytes(bytes, &ValidationContext::for_room(room))
        .expect("test frame must be stateless-valid")
        .event_id
}

/// A genesis frame for a fresh room whose admin is `sk(1)`/`sk(2)`.
fn make_genesis(admin_id: &SigningKey, admin_dev: &SigningKey) -> (Vec<u8>, RoomId) {
    let id_key = admin_id.identity_key();
    let dev_key = admin_dev.device_key();
    let room = signed::derive_room_id(&id_key, &NONCE, T0);
    let binding = DeviceBinding::create(&room, admin_id, dev_key);
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: id_key,
        device_id: dev_key,
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![id_key],
            device_binding: binding,
        }),
    };
    (seal(&ev, admin_dev), room)
}

/// An admin-authored `message.text` frame citing `prev`.
fn make_message(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    body: &str,
    t: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin_id.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: t,
        prev_events: vec![prev],
        content: Content::MessageText(MessageText {
            body: body.to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    seal(&ev, admin_dev)
}

/// A fresh engine over an in-memory store, seeded with a genesis. Returns the
/// engine plus the room and the genesis id.
fn seeded_engine(cfg: SyncConfig) -> (SyncEngine, RoomId, EventId) {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine = SyncEngine::open(store, room, cfg).expect("open");
    engine.publish(&genesis).expect("publish genesis");
    let genesis_id = frame_id(&genesis, room);
    (engine, room, genesis_id)
}

/// The event frames fanned out in `out`, flattened.
fn events_frames(out: &[Outgoing]) -> Vec<Vec<u8>> {
    out.iter()
        .filter_map(|o| match &o.msg {
            SyncMessage::Events { frames, .. } => Some(frames.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

// A failed insert defers — never drops — the accepted event: nothing is fanned
// out or fed to subscribers while the store lacks it, and the first tick's
// retry lands it with the full deferred bookkeeping (fan-out, counters, feed).
#[test]
fn failed_insert_is_retried_on_tick_and_recovers() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let _ = engine.on_connect(NODE_A);
    let _ = engine.take_ingested(); // drain the genesis

    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "hello", T0 + 1);
    let msg_id = frame_id(&msg, room);
    engine.store_mut().fail_next_inserts(1);
    let out = engine.publish(&msg).expect("publish is not an error path");

    // Deferred: fold-accepted but unstored — no fan-out, no feed, no accept count.
    assert!(
        events_frames(&out).is_empty(),
        "an unstored event must not fan out"
    );
    assert!(
        engine.take_ingested().is_empty(),
        "an unstored event must not reach the push feed"
    );
    assert_eq!(engine.store_retry_len(), 1, "the event must be queued");
    assert_eq!(engine.counters().store_insert_failed, 1);
    assert_eq!(engine.counters().accepted, 1, "genesis only");
    assert_eq!(engine.room_tail(100).expect("tail").len(), 1);
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("store insert failed")),
        "the failure must be logged (no silent degradation)"
    );

    // First tick: the retry lands and the deferred bookkeeping runs once.
    let out = engine.on_tick(T0 + 2);
    let fanned = events_frames(&out);
    assert!(
        fanned.iter().any(|f| frame_id(f, room) == msg_id),
        "the late-landing event must fan out to the connected peer"
    );
    assert_eq!(engine.store_retry_len(), 0);
    assert_eq!(engine.counters().accepted, 2);
    let fed: Vec<EventId> = engine
        .take_ingested()
        .iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(fed, vec![msg_id], "the feed gets the event exactly once");
    assert_eq!(engine.room_tail(100).expect("tail").len(), 2);
    assert!(
        engine.trust_decisions().is_empty(),
        "a recovered fault is not a trust decision"
    );
}

// The exact #119 shape: a descendant accepted-and-stored above the failed
// parent sits causally unplaced (NULL lamport, excluded from `room_tail`) until
// the local retry heals the hole — no peer involved.
#[test]
fn local_retry_heals_the_store_hole_under_descendants() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());

    let msg1 = make_message(&sk(1), &sk(2), room, genesis_id, "m1", T0 + 1);
    let msg1_id = frame_id(&msg1, room);
    let msg2 = make_message(&sk(1), &sk(2), room, msg1_id, "m2", T0 + 2);
    let msg2_id = frame_id(&msg2, room);

    engine.store_mut().fail_next_inserts(1);
    engine.publish(&msg1).expect("publish m1");
    engine.publish(&msg2).expect("publish m2");

    // The descendant fold-accepted (readiness checks the fold, not the store)
    // and persisted above the hole: stored but causally unplaced.
    let ids = engine.digest().expect("digest").event_ids;
    assert!(ids.contains(&msg2_id), "descendant is stored");
    assert!(!ids.contains(&msg1_id), "the hole itself is not");
    assert_eq!(
        engine.room_tail(100).expect("tail").len(),
        1,
        "a NULL-lamport descendant is excluded from the tail"
    );

    // The tick retry inserts the parent; the store's insert-time propagation
    // re-places the descendant.
    let _ = engine.on_tick(T0 + 3);
    assert_eq!(engine.store_retry_len(), 0);
    assert_eq!(
        engine.room_tail(100).expect("tail").len(),
        3,
        "healing the hole recomputes the descendant's lamport"
    );
}

// Exhausting the bounded retry budget abandons the event and surfaces the
// degradation as a CRITICAL `store_degraded` decision — which survives a
// restart, exactly like the equivocation audit trail.
#[test]
fn exhausted_retry_budget_records_critical_store_degraded() {
    let cfg = SyncConfig {
        store_retry_attempts: 2,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);

    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "doomed", T0 + 1);
    let msg_id = frame_id(&msg, room);
    // 1 initial failure + 2 failed retries = the whole budget.
    engine.store_mut().fail_next_inserts(3);
    engine.publish(&msg).expect("publish");
    assert_eq!(engine.store_retry_len(), 1);

    let _ = engine.on_tick(T0 + 2);
    assert_eq!(engine.store_retry_len(), 1, "attempt 1 of 2: still queued");
    assert!(engine.trust_decisions().is_empty());

    let _ = engine.on_tick(T0 + 3);
    assert_eq!(engine.store_retry_len(), 0, "budget exhausted: abandoned");
    assert_eq!(engine.counters().store_insert_failed, 3);
    assert_eq!(engine.counters().store_retry_dropped, 1);
    let decisions = engine.trust_decisions();
    assert!(
        decisions.iter().any(|d| d.code == "store_degraded"
            && d.severity == Severity::Critical
            && d.event_ids == vec![msg_id]),
        "the operator must see a CRITICAL store_degraded decision; got {decisions:?}"
    );

    // A restart re-folds from `events` (clearing the fold/store divergence) and
    // must restore the audit decision from the durable trail.
    let store = engine.into_store();
    let reopened = SyncEngine::open(store, room, cfg).expect("reopen");
    assert!(
        reopened
            .trust_decisions()
            .iter()
            .any(|d| d.code == "store_degraded" && d.severity == Severity::Critical),
        "store_degraded must survive a restart"
    );
    assert_eq!(reopened.counters().trust_restored, 1);
    assert!(
        !reopened
            .digest()
            .expect("digest")
            .event_ids
            .contains(&msg_id),
        "the re-fold no longer claims the event the store lost"
    );
}

// A full retry queue never grows past its cap (Gate-D R4): the overflowing
// arrival is dropped straight to the CRITICAL decision, logged and counted.
#[test]
fn full_retry_queue_drops_overflow_straight_to_decision() {
    let cfg = SyncConfig {
        max_store_retry_total: 1,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);

    let msg1 = make_message(&sk(1), &sk(2), room, genesis_id, "m1", T0 + 1);
    let msg2 = make_message(&sk(1), &sk(2), room, genesis_id, "m2", T0 + 2);
    let msg2_id = frame_id(&msg2, room);
    engine.store_mut().fail_next_inserts(2);
    engine.publish(&msg1).expect("publish m1");
    engine.publish(&msg2).expect("publish m2");

    assert_eq!(engine.store_retry_len(), 1, "the cap holds");
    assert_eq!(engine.counters().store_retry_dropped, 1);
    assert!(
        engine
            .trust_decisions()
            .iter()
            .any(|d| d.code == "store_degraded" && d.event_ids == vec![msg2_id]),
        "the overflow victim is surfaced, not silently truncated"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("store_retry_total")),
        "the cap drop must be logged"
    );
}

// A peer re-serving a queued event (the #118 healing path) supersedes the
// local retry: the insert lands through the normal deliver path, the queue
// entry clears, and the exactly-once feed guarantee holds.
#[test]
fn peer_reserve_clears_pending_retry_exactly_once() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let _ = engine.take_ingested(); // drain the genesis

    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "again", T0 + 1);
    let msg_id = frame_id(&msg, room);
    engine.store_mut().fail_next_inserts(1);
    let _ = engine.ingest_frame(NODE_A, &msg);
    assert_eq!(engine.store_retry_len(), 1);

    // The peer re-serves the same frame; the store is healthy again.
    let _ = engine.ingest_frame(NODE_A, &msg);
    assert_eq!(
        engine.store_retry_len(),
        0,
        "the re-see supersedes the retry"
    );
    assert_eq!(engine.counters().accepted, 2, "genesis + the event, once");
    let fed: Vec<EventId> = engine
        .take_ingested()
        .iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(
        fed,
        vec![msg_id],
        "exactly-once push feed across the re-see"
    );
    assert_eq!(engine.room_tail(100).expect("tail").len(), 2);

    // And the tick after has nothing left to retry.
    let _ = engine.on_tick(T0 + 2);
    assert_eq!(engine.counters().store_retry_dropped, 0);
    assert!(engine.trust_decisions().is_empty());
}
