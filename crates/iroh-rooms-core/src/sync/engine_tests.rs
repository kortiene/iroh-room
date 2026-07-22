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
const NODE_B: PeerId = PeerId::from_bytes([0xB1; 32]);

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

// A transient SQLITE_BUSY burst that spans several retry ticks (but stays
// inside the attempt budget) recovers with the full deferred bookkeeping and
// raises no trust decision (issue #143 acceptance: "the store-retry path still
// recovers a transient SQLITE_BUSY burst"). The existing recovery tests all
// land on the very first retry tick; this pins the multi-tick-burst case.
#[test]
fn transient_busy_burst_recovers_across_multiple_ticks() {
    let cfg = SyncConfig {
        store_retry_attempts: 16,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.on_connect(NODE_A);
    let _ = engine.take_ingested(); // drain genesis + handshake

    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "bursty", T0 + 1);
    let msg_id = frame_id(&msg, room);

    // One initial publish failure plus two failed retry ticks: the burst spans
    // three consecutive insert attempts, all well inside the 16-attempt budget.
    engine.store_mut().fail_next_inserts(3);
    let out = engine.publish(&msg).expect("publish is not an error path");
    assert!(
        events_frames(&out).is_empty(),
        "an unstored event must not fan out"
    );
    assert_eq!(engine.store_retry_len(), 1, "queued for retry");

    // Tick 1 and tick 2 still hit the busy burst: the event stays queued, no
    // fan-out, no feed, and — crucially — no premature trust decision.
    let out = engine.on_tick(T0 + 2);
    assert!(events_frames(&out).is_empty(), "still busy: no fan-out");
    assert_eq!(engine.store_retry_len(), 1, "attempt 1: still queued");
    let out = engine.on_tick(T0 + 3);
    assert!(events_frames(&out).is_empty(), "still busy: no fan-out");
    assert_eq!(engine.store_retry_len(), 1, "attempt 2: still queued");
    assert!(
        engine.trust_decisions().is_empty(),
        "a still-recoverable burst is not yet a degradation"
    );
    assert_eq!(engine.counters().accepted, 1, "genesis only so far");

    // Tick 3: the burst has cleared (fault budget exhausted), so the retry lands
    // with the full deferred bookkeeping exactly once.
    let out = engine.on_tick(T0 + 4);
    assert!(
        events_frames(&out)
            .iter()
            .any(|f| frame_id(f, room) == msg_id),
        "the recovered event must fan out to the connected peer"
    );
    assert_eq!(engine.store_retry_len(), 0, "the queue drained on recovery");
    assert_eq!(engine.counters().store_insert_failed, 3, "three busy hits");
    assert_eq!(
        engine.counters().store_retry_dropped,
        0,
        "nothing abandoned"
    );
    assert_eq!(engine.counters().accepted, 2, "genesis + recovered event");
    let fed: Vec<EventId> = engine
        .take_ingested()
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(fed, vec![msg_id], "the feed gets the event exactly once");
    assert_eq!(engine.room_tail(100).expect("tail").len(), 2);
    assert!(
        engine.trust_decisions().is_empty(),
        "a recovered burst leaves no CRITICAL store_degraded trail"
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

// ---------------------------------------------------------------------------
// Issue #143 — batched SQLite insertion + early event-id dedup
//
// These tests pin the three acceptance criteria:
//   * a replay inside the cache window skips signature verification AND store
//     work (counted by `early_duplicates`);
//   * N consecutive accepted events commit in ⌈N/batch⌉ transactions;
//   * a failed batch defers all post-commit side effects and feeds the existing
//     #119 retry path.
// ---------------------------------------------------------------------------

/// Build a `WireEvent` envelope from already-signed bytes and a *raw* signature
/// — used by T1 to forge a deliberately-bad-sig replay over identical `signed`
/// bytes (same id) without going through [`crate::event::signed::sign_csb`].
fn seal_with_sig(signed_bytes: Vec<u8>, sig_bytes: [u8; 64]) -> Vec<u8> {
    use crate::event::keys::Signature;
    WireEvent::seal(signed_bytes, Signature::from_bytes(sig_bytes)).to_bytes()
}

// T1 — A replay inside the cache window skips both signature verification and
// any store transaction: the `early_duplicates` counter increments, no
// `reject.bad_signature` line is logged, and the store's write-tx count is
// unchanged.
#[test]
fn early_duplicate_skips_bad_signature_and_store() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let _ = engine.take_ingested(); // drain genesis

    // Build the valid signed bytes of a fresh message, accept+store it, and
    // confirm it landed in the cache.
    let admin_id = sk(1);
    let admin_dev = sk(2);
    let msg = make_message(&admin_id, &admin_dev, room, genesis_id, "first", T0 + 1);
    let msg_id = frame_id(&msg, room);
    let _ = engine.publish(&msg).expect("publish valid msg");
    let _ = engine.take_ingested();
    let early_before = engine.counters().early_duplicates;
    let rejected_before = engine.counters().rejected;
    engine.store_mut().reset_write_tx_count();

    // Forge a replay: identical `signed` bytes (so identical id), but a
    // signature that does NOT verify under the admin device key.
    let signed_bytes = crate::event::signed::SignedEvent::decode(
        &WireEvent::decode(&msg).expect("decode valid frame").signed,
    )
    .expect("decode signed")
    .to_csb();
    let bad_sig = [0u8; 64];
    let replay = seal_with_sig(signed_bytes, bad_sig);

    // Sanity: the replay's id matches the original (same signed bytes).
    assert_eq!(
        crate::event::signed::event_id_from_bytes(
            &WireEvent::decode(&replay).expect("decode replay").signed
        ),
        msg_id,
        "the replay must share the original's id"
    );

    let _ = engine.ingest_frame(NODE_A, &replay);

    assert_eq!(
        engine.counters().early_duplicates,
        early_before + 1,
        "the replay must hit the early dedup cache"
    );
    assert_eq!(
        engine.counters().rejected,
        rejected_before,
        "no reject.bad_signature — sig verification never ran"
    );
    assert!(
        !engine
            .logs()
            .iter()
            .any(|l| l.contains("reject.bad_signature")),
        "no bad_signature log line — sig verification never ran"
    );
    assert_eq!(
        engine.store_mut().write_tx_count(),
        0,
        "the store was not touched"
    );
    assert_eq!(
        engine.room_tail(100).expect("tail").len(),
        2,
        "room tail unchanged"
    );
}

// T2 — A bad-signature first arrival does NOT poison the dedup cache: the
// validly-signed copy with the same signed bytes is still accepted.
#[test]
fn invalid_first_arrival_cannot_poison_cache() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let _ = engine.take_ingested(); // drain genesis

    let admin_id = sk(1);
    let admin_dev = sk(2);
    let valid = make_message(&admin_id, &admin_dev, room, genesis_id, "first", T0 + 1);
    let signed_bytes = crate::event::signed::SignedEvent::decode(
        &WireEvent::decode(&valid).expect("decode valid").signed,
    )
    .expect("decode signed")
    .to_csb();
    // Same signed bytes ⇒ same id, but a deliberately invalid signature.
    let bad = seal_with_sig(signed_bytes, [0u8; 64]);

    let early_before = engine.counters().early_duplicates;
    engine.ingest_frame(NODE_A, &bad);
    assert_eq!(
        engine.counters().early_duplicates,
        early_before,
        "an invalid first arrival must not be cached"
    );
    assert!(
        engine.counters().rejected > 0,
        "the invalid sig was rejected, not silently dropped"
    );

    // The validly-signed copy with the same signed bytes must still land.
    let accepted_before = engine.counters().accepted;
    engine.publish(&valid).expect("publish valid");
    assert_eq!(engine.counters().accepted, accepted_before + 1);
}

// T3 — When the dedup cache cap is small enough to evict the first arrival, a
// replay falls through to the existing idempotent store path and increments
// `duplicates` (not `early_duplicates`).
#[test]
fn cache_eviction_falls_back_to_store_idempotency() {
    let cfg = SyncConfig {
        early_event_id_dedup_cache_entries: 2,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.take_ingested(); // drain genesis

    let admin_id = sk(1);
    let admin_dev = sk(2);
    let m1 = make_message(&admin_id, &admin_dev, room, genesis_id, "one", T0 + 1);
    let m2 = make_message(&admin_id, &admin_dev, room, genesis_id, "two", T0 + 2);
    let m3 = make_message(&admin_id, &admin_dev, room, genesis_id, "three", T0 + 3);
    engine.publish(&m1).expect("m1");
    engine.publish(&m2).expect("m2");
    engine.publish(&m3).expect("m3");
    let _ = engine.take_ingested();

    let early_before = engine.counters().early_duplicates;
    let dups_before = engine.counters().duplicates;

    // m1 has been evicted (cap=2 holds {m2, m3}); its replay must run full
    // validation and hit the store's idempotent Duplicate arm.
    engine.ingest_frame(NODE_A, &m1);
    assert_eq!(
        engine.counters().early_duplicates,
        early_before,
        "evicted id must not hit the early cache"
    );
    assert_eq!(
        engine.counters().duplicates,
        dups_before + 1,
        "evicted id falls back to the existing store duplicate path"
    );
}

#[test]
fn disabled_dedup_cache_uses_store_duplicate_path() {
    let cfg = SyncConfig {
        early_event_id_dedup_cache_entries: 0,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.take_ingested();

    let admin_id = sk(1);
    let admin_dev = sk(2);
    let msg = make_message(&admin_id, &admin_dev, room, genesis_id, "first", T0 + 1);
    engine.publish(&msg).expect("publish valid msg");
    let _ = engine.take_ingested();

    let early_before = engine.counters().early_duplicates;
    let dups_before = engine.counters().duplicates;
    engine.store_mut().reset_write_tx_count();

    engine.ingest_frame(NODE_A, &msg);

    assert_eq!(engine.counters().early_duplicates, early_before);
    assert_eq!(engine.counters().duplicates, dups_before + 1);
    assert_eq!(engine.store_mut().write_tx_count(), 1);
}

// T4 — N consecutive accepted events in one Events message commit in
// ⌈N/batch⌉ transactions, not N (issue #143 batching acceptance).
#[test]
fn consecutive_accepted_events_commit_in_ceil_n_over_batch_transactions() {
    let cfg = SyncConfig {
        store_insert_batch_size: 4,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.take_ingested(); // drain genesis

    // Build a linear chain of 10 messages; deliver them in one Events message.
    let admin_id = sk(1);
    let admin_dev = sk(2);
    let mut prev = genesis_id;
    let mut frames = Vec::with_capacity(10);
    let mut expected_ids = Vec::with_capacity(10);
    for i in 1u64..=10 {
        let f = make_message(&admin_id, &admin_dev, room, prev, &format!("m{i}"), T0 + i);
        expected_ids.push(frame_id(&f, room));
        prev = expected_ids[expected_ids.len() - 1];
        frames.push(f);
    }

    engine.store_mut().reset_write_tx_count();
    let out = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames,
        },
    );
    // Discard handshake-style side effects; the test only cares about tx count.
    let _ = out;

    // ceil(10 / 4) = 3 batches → 3 write transactions.
    assert_eq!(
        engine.store_mut().write_tx_count(),
        3,
        "10 events / batch 4 → 3 transactions, not 10"
    );

    // All 10 events land and appear in canonical room-tail order.
    let tail: Vec<EventId> = engine
        .room_tail(100)
        .expect("tail")
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    for id in &expected_ids {
        assert!(tail.contains(id), "event {id} should be in the tail");
    }
    assert_eq!(engine.counters().accepted, 11, "genesis + 10 messages");
}

// T7 — A failed batch defers every side effect (no fan-out, no feed, no
// accept count) and enqueues each affected event on the #119 retry queue; the
// retry path on the next tick recovers with the full deferred bookkeeping.
#[test]
fn failed_batch_defers_side_effects_and_recovers_on_tick() {
    let cfg = SyncConfig {
        store_insert_batch_size: 4,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.on_connect(NODE_A);
    let _ = engine.on_connect(NODE_B);
    let _ = engine.take_ingested(); // drain genesis + handshake

    let admin_id = sk(1);
    let admin_dev = sk(2);
    let m1 = make_message(&admin_id, &admin_dev, room, genesis_id, "one", T0 + 1);
    let m2 = make_message(&admin_id, &admin_dev, room, genesis_id, "two", T0 + 2);
    let m1_id = frame_id(&m1, room);
    let m2_id = frame_id(&m2, room);

    // Inject one transaction-level failure for the next batch commit. With
    // batch size 4, the two events accumulate into a single flush that fails.
    engine.store_mut().fail_next_inserts(1);
    let out = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames: vec![m1.clone(), m2.clone()],
        },
    );

    // No fan-out, no feed, no accept.
    assert!(
        events_frames(&out).is_empty(),
        "an unstored batch must not fan out"
    );
    assert!(
        engine.take_ingested().is_empty(),
        "an unstored batch must not reach the feed"
    );
    assert_eq!(engine.store_retry_len(), 2, "both events queued for retry");
    assert_eq!(
        engine.counters().store_insert_failed,
        2,
        "failure count = batch size"
    );
    assert_eq!(engine.counters().accepted, 1, "genesis only");
    assert!(
        engine
            .logs()
            .iter()
            .any(|l| l.contains("store insert failed (batch)")),
        "the batch failure must be logged distinctly"
    );

    // Clear the fault; the tick's retry_store lands both events with their
    // deferred post-commit bookkeeping. Each event fans out to every peer
    // except its original sender (NODE_A), so NODE_B sees both.
    let out = engine.on_tick(T0 + 3);
    let fanned_to_b: Vec<EventId> = out
        .iter()
        .filter(|o| o.peer == NODE_B)
        .filter_map(|o| match &o.msg {
            SyncMessage::Events { frames, .. } => Some(frames.clone()),
            _ => None,
        })
        .flatten()
        .map(|f| frame_id(&f, room))
        .collect();
    assert!(fanned_to_b.contains(&m1_id) && fanned_to_b.contains(&m2_id));
    assert_eq!(engine.store_retry_len(), 0);
    assert_eq!(engine.counters().accepted, 3, "genesis + m1 + m2");
    let fed: Vec<EventId> = engine
        .take_ingested()
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    // The #119 per-event retry path iterates `store_retry` in BTreeMap
    // (bytewise EventId) order, not the original batch input order (spec D9:
    // "Keep retry insertion per-event initially"). Set-membership +
    // exactly-once is what holds here; strict input-order is the live batch
    // path's contract, pinned separately by `batch_preserves_fanout_and_feed_order`.
    let mut fed_sorted = fed.clone();
    fed_sorted.sort();
    let mut expected_sorted = vec![m1_id, m2_id];
    expected_sorted.sort();
    assert_eq!(
        fed_sorted, expected_sorted,
        "feed gets both events exactly once"
    );
    assert_eq!(engine.room_tail(100).expect("tail").len(), 3);
}

// T5 — A successful batch preserves fanout/feed order: outgoing `Events`
// frames and `take_ingested()` are both in input order.
#[test]
fn batch_preserves_fanout_and_feed_order() {
    let cfg = SyncConfig {
        store_insert_batch_size: 8,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let _ = engine.on_connect(NODE_A);
    let _ = engine.on_connect(NODE_B);
    let _ = engine.take_ingested();

    let admin_id = sk(1);
    let admin_dev = sk(2);
    let mut prev = genesis_id;
    let mut frames = Vec::new();
    let mut expected_ids = Vec::new();
    for i in 1u64..=5 {
        let f = make_message(&admin_id, &admin_dev, room, prev, &format!("m{i}"), T0 + i);
        let id = frame_id(&f, room);
        expected_ids.push(id);
        prev = id;
        frames.push(f);
    }

    let out = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames,
        },
    );

    // The events arrived from NODE_A, so they fan out to NODE_B in input order.
    let fanned_to_b: Vec<EventId> = out
        .iter()
        .filter(|o| o.peer == NODE_B)
        .filter_map(|o| match &o.msg {
            SyncMessage::Events { frames, .. } => Some(frames.clone()),
            _ => None,
        })
        .flatten()
        .map(|f| frame_id(&f, room))
        .collect();
    assert_eq!(
        fanned_to_b, expected_ids,
        "outgoing Events frames to NODE_B are in input order"
    );
    let fed: Vec<EventId> = engine
        .take_ingested()
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(fed, expected_ids, "feed emits in input order");
}
