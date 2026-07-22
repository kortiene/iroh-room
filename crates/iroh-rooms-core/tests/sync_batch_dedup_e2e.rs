//! End-to-end coverage for issue #143 — batched `SQLite` insertion + early
//! event-id dedup — exercised through the **public** engine API and the
//! deterministic [`SimNet`] multi-peer mesh.
//!
//! ## What boundary this crosses
//!
//! The per-criterion pinning lives in `src/sync/engine_tests.rs`, but those
//! tests are `#[cfg(test)]` in-crate because the deterministic fault injector
//! ([`EventStore::fail_next_inserts`]) and the write-transaction counter
//! ([`EventStore::write_tx_count`]) are `pub(crate)` + `#[cfg(test)]`-only —
//! integration tests compile the lib without them. This file adds the
//! **system-boundary** coverage the issue's acceptance criteria also call out,
//! using only the public surface:
//!
//! * **Client ↔ server fan-out amplification.** A published event fans out to
//!   every connected peer, and each accepting peer re-fans-out to every other
//!   connected peer. Without the early dedup cache every re-see would pay for
//!   an Ed25519 verification + a `SQLite` insert attempt; with it, the second
//!   arrival of an already-cached id is dropped before either runs (criterion
//!   #1, verified by the public [`SyncCounters::early_duplicates`] counter).
//! * **Wire → engine → store batching.** A single `Events` message carrying N
//!   frames must commit in ⌈N/batch⌉ transactions, not N (criterion #2). The
//!   raw transaction count is `pub(crate)`, but the public
//!   [`SyncCounters::store_insert_batches`] counter is the same observable: it
//!   bumps exactly once per non-empty `flush_store_batch`.
//! * **Config-driven rollback knobs.** `early_event_id_dedup_cache_entries: 0`
//!   must make a replay fall through to the existing idempotent store duplicate
//!   path (criterion #1's negative direction, pinning the supported rollback
//!   knob documented on [`SyncConfig::early_event_id_dedup_cache_entries`]).
//!
//! ## Coverage gaps left intentionally open
//!
//! * **`SQLITE_BUSY` fault injection** (criterion #3 — the #119 retry path
//!   recovers a transient burst) cannot be triggered from outside the crate
//!   without flaky real-process contention. The in-crate
//!   `engine_tests::failed_batch_defers_side_effects_and_recovers_on_tick` and
//!   `engine_tests::transient_busy_burst_recovers_across_multiple_ticks` pin
//!   that path against `fail_next_inserts`; this file asserts the weaker
//!   e2e-side observable that a converged mesh stays converged with batching
//!   on (no spurious `store_insert_failed`).
//! * **Per-event write-tx count** (criterion #2's literal "transactions, not
//!   N") is `pub(crate)`; the public `store_insert_batches` counter is the
//!   closest observable and is exactly what the engine's own docs cite.
//!
//! [`SimNet`]: iroh_rooms_core::sync::sim::SimNet
//! [`EventStore::fail_next_inserts`]: iroh_rooms_core::store::EventStore::fail_next_inserts
//! [`EventStore::write_tx_count`]: iroh_rooms_core::store::EventStore::write_tx_count
//! [`SyncCounters::early_duplicates`]: iroh_rooms_core::sync::SyncCounters::early_duplicates
//! [`SyncCounters::store_insert_batches`]: iroh_rooms_core::sync::SyncCounters::store_insert_batches
//! [`SyncConfig::early_event_id_dedup_cache_entries`]: iroh_rooms_core::sync::SyncConfig::early_event_id_dedup_cache_entries

#![cfg(feature = "sync")]
#![allow(clippy::similar_names)] // `bob`/`blob`-style short fixture names

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{Content, EventType, MessageText, RoomCreated};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::sim::SimNet;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine, SyncMessage};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xAA; 16];
const T0: u64 = 1_750_000_000_000;

const NODE_A: PeerId = PeerId::from_bytes([0xA1; 32]);
const NODE_B: PeerId = PeerId::from_bytes([0xB2; 32]);
const NODE_C: PeerId = PeerId::from_bytes([0xC3; 32]);

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// The validated `event_id` of a wire frame (decode transport, BLAKE3 the
/// canonical signed bytes) — used to identify events in fan-out / feed
/// assertions without re-running the full validator.
fn frame_event_id(bytes: &[u8]) -> EventId {
    let wire = WireEvent::decode(bytes).expect("decode wire frame");
    signed::event_id_from_bytes(&wire.signed)
}

/// A `room.created` frame whose admin is `admin_id`/`admin_dev`. Returns the
/// wire bytes plus the derived `RoomId`.
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
            room_name: "Issue143 E2E".to_owned(),
            room_nonce: NONCE,
            admins: vec![id_key],
            device_binding: binding,
        }),
    };
    (seal(&ev, admin_dev), room)
}

/// An admin-authored `message.text` frame citing `prev` in the same room.
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

/// A fresh in-memory engine for `room`, ready to ingest frames.
fn fresh_engine(room: RoomId, config: SyncConfig) -> SyncEngine {
    let store = EventStore::open_in_memory().expect("in-memory store");
    SyncEngine::open(store, room, config).expect("open engine")
}

// ---------------------------------------------------------------------------
// Criterion #1 — early event-id dedup across the fan-out amplification boundary
// ---------------------------------------------------------------------------

/// A published event in a three-peer full mesh is re-fanned-out by every
/// accepting peer to every other connected peer, so the second arrival at each
/// non-publishing peer is an early-dedup hit: it must skip signature
/// verification AND any store work, recorded in the public
/// [`SyncCounters::early_duplicates`] counter (issue #143 acceptance
/// criterion #1). The mesh must still converge.
///
/// Boundary crossed: `SyncEngine::publish` → fan-out → `SimNet` wire → peer
/// `ingest_frame` → `deliver_bytes` → early dedup cache.
///
/// [`SyncCounters::early_duplicates`]: iroh_rooms_core::sync::SyncCounters::early_duplicates
#[test]
fn mesh_fanout_amplification_hits_early_dedup() {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let genesis_id = frame_event_id(&genesis);

    let mut net = SimNet::new(room);
    net.add_peer(NODE_A, fresh_engine(room, SyncConfig::default()));
    net.add_peer(NODE_B, fresh_engine(room, SyncConfig::default()));
    net.add_peer(NODE_C, fresh_engine(room, SyncConfig::default()));

    // Full mesh first so every fan-out target is reachable.
    net.connect_all();
    // Drain handshake / membership-exchange side effects so the post-publish
    // counter deltas are attributable only to the event dissemination.
    net.run_to_quiescence();
    for peer in [NODE_A, NODE_B, NODE_C] {
        let _ = net.engine_mut(peer).take_ingested();
    }

    // Pre-seed A with genesis by direct publish so the message frame below has
    // a present parent and is fold-Accepted (not Buffered) at every peer.
    net.engine_mut(NODE_A)
        .publish(&genesis)
        .expect("publish genesis at A");
    net.run_to_quiescence();
    for peer in [NODE_A, NODE_B, NODE_C] {
        let _ = net.engine_mut(peer).take_ingested();
    }
    // Sanity: every peer holds genesis before the amplification scenario.
    net.assert_converged(&[NODE_A, NODE_B, NODE_C]);

    // Baseline counters immediately before the amplified publish.
    let early_before: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().early_duplicates)
        .sum();
    let accepted_before: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().accepted)
        .sum();
    let rejected_before: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().rejected)
        .sum();
    let store_failed_before: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().store_insert_failed)
        .sum();

    // The amplification scenario: A publishes one fresh admin-authored
    // message. A fans out to B and C. Each accepting peer (B, C) re-fans-out
    // to every OTHER connected peer — including each other — so the second
    // arrival at each of B and C must be an early-dedup hit.
    let msg = make_message(&admin_id, &admin_dev, room, genesis_id, "amplified", T0 + 1);
    let msg_id = frame_event_id(&msg);
    net.engine_mut(NODE_A)
        .publish(&msg)
        .expect("publish amplified message at A");
    net.run_to_quiescence();

    // Mesh-level convergence: every peer holds the message exactly once.
    net.assert_converged(&[NODE_A, NODE_B, NODE_C]);
    let mut saw_msg = 0;
    for peer in [NODE_A, NODE_B, NODE_C] {
        let tail: Vec<EventId> = net
            .engine(peer)
            .room_tail(100)
            .expect("tail")
            .into_iter()
            .map(|se| se.event_id)
            .collect();
        if tail.contains(&msg_id) {
            saw_msg += 1;
        }
    }
    assert_eq!(saw_msg, 3, "every peer holds the freshly-published message");

    // Criterion #1 — the public counter is non-zero: the second arrival at
    // each non-publishing peer (B and C) hit the early dedup cache before any
    // signature verification or store work ran.
    let early_after: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().early_duplicates)
        .sum();
    assert!(
        early_after > early_before,
        "fan-out amplification must produce at least one early-dedup hit, before={early_before} \
         after={early_after}",
    );

    // The message was accepted exactly once per peer (no duplicates slipped
    // past the cache to the store's `Duplicate` arm, and no peer accepted it
    // twice). `accepted` grows by 3 (one per peer) and `rejected` is flat
    // (a cache hit is silent — never counted as a reject).
    let accepted_after: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().accepted)
        .sum();
    assert_eq!(
        accepted_after,
        accepted_before + 3,
        "the message is accepted exactly once per peer"
    );
    let rejected_after: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().rejected)
        .sum();
    assert_eq!(
        rejected_after, rejected_before,
        "an early-dedup hit is silent — never a reject"
    );
    let store_failed_after: u64 = [NODE_A, NODE_B, NODE_C]
        .iter()
        .map(|p| net.engine(*p).counters().store_insert_failed)
        .sum();
    assert_eq!(
        store_failed_after, store_failed_before,
        "no spurious store failures with batching on (criterion #3 e2e observable)"
    );
}

// ---------------------------------------------------------------------------
// Criterion #2 — N consecutive accepted events commit in ⌈N/batch⌉ batches
// ---------------------------------------------------------------------------

/// A single `Events` message carrying N frames must commit in ⌈N/batch⌉
/// batches, observable through the public `store_insert_batches` counter
/// (issue #143 acceptance criterion #2). The raw transaction counter is
/// `pub(crate)`, but the engine bumps `store_insert_batches` exactly once per
/// non-empty `flush_store_batch`, so the public counter is the same
/// observable.
///
/// Boundary crossed: `SyncMessage::Events` → `on_message` → `deliver_bytes` ×
/// N → `flush_store_batch` → `EventStore::insert_all_outcomes`.
#[test]
fn events_message_commits_in_ceil_n_over_batch_batches() {
    // batch size 2: forces the cap-triggered mid-loop flush, exercising the
    // batching machinery rather than just the entry-point-boundary flush.
    let cfg = SyncConfig {
        store_insert_batch_size: 2,
        ..SyncConfig::default()
    };
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let genesis_id = frame_event_id(&genesis);

    let mut engine = fresh_engine(room, cfg);
    engine.publish(&genesis).expect("seed genesis");
    let _ = engine.take_ingested();
    let batches_before = engine.counters().store_insert_batches;
    let accepted_before = engine.counters().accepted;

    // Build a linear chain of 5 messages and deliver them in ONE Events
    // message. ceil(5 / 2) = 3 batches.
    let mut prev = genesis_id;
    let mut frames = Vec::with_capacity(5);
    let mut expected_ids = Vec::with_capacity(5);
    for i in 1u64..=5 {
        let f = make_message(&admin_id, &admin_dev, room, prev, &format!("m{i}"), T0 + i);
        expected_ids.push(frame_event_id(&f));
        prev = expected_ids[expected_ids.len() - 1];
        frames.push(f);
    }

    let _ = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames,
        },
    );

    assert_eq!(
        engine.counters().store_insert_batches,
        batches_before + 3,
        "5 events / batch 2 → 3 batches (ceil), not 5"
    );
    assert_eq!(
        engine.counters().accepted,
        accepted_before + 5,
        "all 5 events accepted"
    );

    // All 5 events are durable in the store and appear in the canonical tail.
    let tail: Vec<EventId> = engine
        .room_tail(100)
        .expect("tail")
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    for id in &expected_ids {
        assert!(tail.contains(id), "event {id} missing from tail");
    }
    // genesis + 5 messages.
    assert_eq!(tail.len(), 6);

    // No store failures and no parking — the batch path commits cleanly when
    // the store is healthy (the criterion #3 e2e observable).
    assert_eq!(
        engine.counters().store_insert_failed,
        0,
        "no spurious store failures"
    );
    assert_eq!(engine.parked_len(), 0, "no parked frames");
    assert_eq!(engine.store_retry_len(), 0, "no retry queue residue");
}

/// With the default batch size (32) the same 5-event burst fits in a single
/// batch: only the entry-point-boundary flush fires, so `store_insert_batches`
/// grows by exactly 1. This pins the "amortizes transaction overhead"
/// guarantee that motivates the criterion.
#[test]
fn events_message_fits_in_one_batch_under_default_size() {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let genesis_id = frame_event_id(&genesis);

    // Default config: store_insert_batch_size = 32.
    let mut engine = fresh_engine(room, SyncConfig::default());
    engine.publish(&genesis).expect("seed genesis");
    let _ = engine.take_ingested();
    let batches_before = engine.counters().store_insert_batches;

    let mut prev = genesis_id;
    let mut frames = Vec::with_capacity(5);
    for i in 1u64..=5 {
        let f = make_message(&admin_id, &admin_dev, room, prev, &format!("b{i}"), T0 + i);
        prev = frame_event_id(&f);
        frames.push(f);
    }
    let _ = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames,
        },
    );

    assert_eq!(
        engine.counters().store_insert_batches,
        batches_before + 1,
        "5 events under default batch=32 → exactly 1 batch"
    );
}

// ---------------------------------------------------------------------------
// Criterion #1 (negative direction) — disabling the early cache falls through
// to the store's idempotent Duplicate arm
// ---------------------------------------------------------------------------

/// With `early_event_id_dedup_cache_entries: 0` (the supported rollback knob
/// documented on [`SyncConfig`]), a replay inside the cache window must NOT
/// hit `early_duplicates`; it must fall through to the store's idempotent
/// `Duplicate` arm and bump the legacy `duplicates` counter instead. This is
/// the negative-direction pin for criterion #1: the cache is a guardrail, not
/// a correctness dependency.
///
/// Boundary crossed: `SyncConfig` → `SyncEngine::open` (cache disabled) →
/// `ingest_frame` → `validate_wire_bytes` → `EventStore::insert` (Duplicate).
///
/// [`SyncConfig`]: iroh_rooms_core::sync::SyncConfig
#[test]
fn disabled_early_dedup_falls_through_to_store_duplicate() {
    let cfg = SyncConfig {
        early_event_id_dedup_cache_entries: 0,
        ..SyncConfig::default()
    };
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);

    let mut engine = fresh_engine(room, cfg);
    engine.publish(&genesis).expect("publish genesis");
    let _ = engine.take_ingested();
    let early_before = engine.counters().early_duplicates;
    let dups_before = engine.counters().duplicates;
    let accepted_before = engine.counters().accepted;

    // Re-deliver genesis as if from a peer: with the cache disabled, the
    // replay must run the FULL validation + store path and hit the store's
    // Duplicate arm.
    let _ = engine.ingest_frame(NODE_A, &genesis);

    assert_eq!(
        engine.counters().early_duplicates,
        early_before,
        "early cache is disabled — no early-dedup hit"
    );
    assert_eq!(
        engine.counters().duplicates,
        dups_before + 1,
        "replay falls through to the store's idempotent Duplicate arm"
    );
    assert_eq!(
        engine.counters().accepted,
        accepted_before,
        "a duplicate re-see is never re-accepted"
    );
    assert!(
        engine.take_ingested().is_empty(),
        "exactly-once feed: a duplicate re-see is never re-emitted"
    );
}

// ---------------------------------------------------------------------------
// Criterion #4 — no regression in insert-then-fanout ordering under batching
// ---------------------------------------------------------------------------

/// With batching enabled (the default), a multi-event `Events` message still
/// preserves the insert-then-fanout contract: each accepted event reaches the
/// push feed in input order, and the resulting fan-out `Events` frames to a
/// third peer are in input order too (issue #143 acceptance criterion #4 — no
/// regression from the batched commit path).
///
/// Boundary crossed: `on_message` → batched `insert_all_outcomes` →
/// `take_ingested` (feed) + fan-out `Events` to a connected peer.
#[test]
fn batched_commit_preserves_fanout_and_feed_order() {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let genesis_id = frame_event_id(&genesis);

    let mut engine = fresh_engine(room, SyncConfig::default());
    // Connect a peer so the freshly-accepted events have a fan-out target.
    let _ = engine.on_connect(NODE_B);
    let _ = engine.take_ingested();
    engine.publish(&genesis).expect("seed genesis");
    let _ = engine.take_ingested();

    // Build a linear chain and deliver it in one Events message.
    let mut prev = genesis_id;
    let mut frames = Vec::with_capacity(4);
    let mut expected_ids = Vec::with_capacity(4);
    for i in 1u64..=4 {
        let f = make_message(&admin_id, &admin_dev, room, prev, &format!("o{i}"), T0 + i);
        expected_ids.push(frame_event_id(&f));
        prev = expected_ids[expected_ids.len() - 1];
        frames.push(f);
    }

    let out = engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames,
        },
    );

    // The Events message arrived from NODE_A, so the fan-out target is NODE_B
    // (every connected peer except the sender). Flatten the frames in the
    // order they appear in the outgoing Events messages.
    let fanned_to_b: Vec<EventId> = out
        .iter()
        .filter(|o| o.peer == NODE_B)
        .filter_map(|o| match &o.msg {
            SyncMessage::Events { frames, .. } => Some(frames.clone()),
            _ => None,
        })
        .flatten()
        .map(|f| frame_event_id(&f))
        .collect();
    assert_eq!(
        fanned_to_b, expected_ids,
        "fan-out frames to NODE_B are in input order"
    );

    // The push feed (`take_ingested`) also emits in input order.
    let fed: Vec<EventId> = engine
        .take_ingested()
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(fed, expected_ids, "feed emits in input order");

    // And the canonical tail agrees.
    let tail: Vec<EventId> = engine
        .room_tail(100)
        .expect("tail")
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    let mut full_expected = vec![genesis_id];
    full_expected.extend(expected_ids);
    assert_eq!(tail, full_expected, "tail in canonical order");
}
