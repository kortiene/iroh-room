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
use crate::event::content::{
    capability_hash, Content, EventType, FileShared, MemberInvited, MemberJoined, MemberRemoved,
    MessageText, RoomCreated,
};
use crate::event::distribution::build_member_removed_with_rotation;
use crate::event::ids::{EventId, HashRef, RoomId};
use crate::event::keys::{DeviceKey, IdentityKey, SigningKey};
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::{validate_wire_bytes, ValidationContext};
use crate::event::wire::WireEvent;
use crate::membership::{Role, Status};
use crate::store::EventStore;
use iroh_rooms_crypto::RoomKey;

use std::collections::BTreeSet;

use super::engine::{Completeness, Severity, SyncEngine};
use super::message::{KeyHistoryChunk, Outgoing, PeerId, SyncMessage};
use super::sim::SimNet;
use super::SyncConfig;
use crate::event::distribution::build_key_distribution_content;

const NONCE: [u8; 16] = [0xAA; 16];
const T0: u64 = 1_750_000_000_000;
const NODE_A: PeerId = PeerId::from_bytes([0xA1; 32]);
const NODE_B: PeerId = PeerId::from_bytes([0xB1; 32]);
const NODE_C: PeerId = PeerId::from_bytes([0xC3; 32]);

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

fn assert_counter_unchanged(engine: &SyncEngine, before: u64) {
    assert_eq!(
        engine.counters().membership_projection_recomputes,
        before,
        "content events must not refresh the cached membership projection"
    );
}

fn assert_counter_increased_by(engine: &SyncEngine, before: u64, delta: u64) {
    assert_eq!(
        engine.counters().membership_projection_recomputes,
        before + delta,
        "cached membership projection recompute counter changed by an unexpected amount"
    );
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

/// A `message.text` frame citing `prev`.
fn make_message(
    sender_id: &SigningKey,
    sender_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    body: &str,
    t: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: sender_id.identity_key(),
        device_id: sender_dev.device_key(),
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
    seal(&ev, sender_dev)
}

fn make_file_shared(
    sender_id: &SigningKey,
    sender_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    blob_hash: HashRef,
    t: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: sender_id.identity_key(),
        device_id: sender_dev.device_key(),
        event_type: EventType::FileShared,
        created_at: t,
        prev_events: vec![prev],
        content: Content::FileShared(FileShared {
            file_id: [0xf1; 16],
            name: "cached-projection.bin".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            size_bytes: 1,
            blob_hash,
            blob_format: None,
            providers: None,
        }),
    };
    seal(&ev, sender_dev)
}

#[allow(clippy::too_many_arguments)]
fn make_invite(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    invitee: IdentityKey,
    invite_id: [u8; 16],
    secret: [u8; 16],
    role: &str,
    t: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin_id.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: vec![prev],
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: capability_hash(&room, &invite_id, &secret),
            role: role.to_owned(),
            invitee_key: invitee,
            expires_at: None,
            invitee_hint: None,
        }),
    };
    seal(&ev, admin_dev)
}

#[allow(clippy::too_many_arguments)]
fn make_join(
    member_id: &SigningKey,
    member_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    invite_id: [u8; 16],
    secret: [u8; 16],
    role: &str,
    t: u64,
) -> Vec<u8> {
    let binding = DeviceBinding::create(&room, member_id, member_dev.device_key());
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: member_id.identity_key(),
        device_id: member_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: t,
        prev_events: vec![prev],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: secret,
            role: role.to_owned(),
            device_binding: binding,
            display_name: None,
        }),
    };
    seal(&ev, member_dev)
}

fn make_remove(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    room: RoomId,
    prev: EventId,
    target: IdentityKey,
    t: u64,
) -> Vec<u8> {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin_id.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MemberRemoved,
        created_at: t,
        prev_events: vec![prev],
        content: Content::MemberRemoved(MemberRemoved {
            member_id: target,
            removed_by: admin_id.identity_key(),
            reason: None,
            device_binding: None,
            rotation: None,
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

#[test]
fn terminal_events_confirms_only_ids_stored_in_the_room() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let terminal_frame = make_message(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        "terminal delivery",
        T0 + 1,
    );
    let terminal_id = frame_id(&terminal_frame, room);
    let missing_id = EventId::from_bytes([0xfe; 32]);
    let nonce = [0x6d; 16];
    assert!(!engine
        .store_mut()
        .contains_in_room(&room, &terminal_id)
        .expect("query terminal event before delivery"));

    let out = engine.on_message(
        NODE_A,
        SyncMessage::TerminalEvents {
            room_id: room,
            frames: vec![terminal_frame],
            ids: vec![missing_id, terminal_id],
            nonce,
        },
    );

    assert!(engine
        .store_mut()
        .contains_in_room(&room, &terminal_id)
        .expect("query terminal event after delivery"));
    assert_eq!(
        out,
        vec![Outgoing {
            peer: NODE_A,
            fanout: false,
            msg: SyncMessage::EventsConfirmed {
                room_id: room,
                ids: vec![terminal_id],
                nonce,
            },
        }]
    );
}

#[test]
fn content_message_publish_does_not_refresh_membership_projection() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let before = engine.counters().membership_projection_recomputes;

    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "content", T0 + 1);
    engine.publish(&msg).expect("publish message");

    assert_counter_unchanged(&engine, before);
    assert_eq!(engine.snapshot().active_member_count(), 1);
    assert_eq!(engine.snapshot().admin(), Some(&sk(1).identity_key()));
}

#[test]
fn file_shared_publish_updates_hashes_without_refreshing_membership_projection() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let before = engine.counters().membership_projection_recomputes;
    let blob_hash = HashRef::from_bytes([0x42; 32]);

    let share = make_file_shared(&sk(1), &sk(2), room, genesis_id, blob_hash, T0 + 1);
    engine.publish(&share).expect("publish file.shared");

    assert_counter_unchanged(&engine, before);
    assert!(engine
        .file_shared_hashes()
        .expect("hashes")
        .contains(blob_hash.as_bytes()));
    assert_eq!(engine.snapshot().active_member_count(), 1);
}

#[test]
fn member_joined_refreshes_cached_projection_and_updates_member_state() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x10);
    let member_dev = sk(0x11);
    let invite_id = [0x10; 16];
    let secret = [0x20; 16];
    let before = engine.counters().membership_projection_recomputes;

    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "agent",
        T0 + 1,
    );
    engine.publish(&invite).expect("publish invite");
    assert_counter_increased_by(&engine, before, 1);

    let invite_event_id = frame_id(&invite, room);
    let join = make_join(
        &member_id,
        &member_dev,
        room,
        invite_event_id,
        invite_id,
        secret,
        "agent",
        T0 + 2,
    );
    engine.publish(&join).expect("publish join");
    assert_counter_increased_by(&engine, before, 2);

    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.status(&member_id.identity_key()),
        Some(Status::Active)
    );
    assert_eq!(snapshot.role(&member_id.identity_key()), Some(Role::Agent));
    assert_eq!(
        snapshot
            .member(&member_id.identity_key())
            .and_then(|m| m.device),
        Some(member_dev.device_key())
    );
    assert_eq!(snapshot.active_member_count(), 2);
}

#[test]
fn member_removed_refreshes_cached_projection_and_removes_active_access() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x12);
    let member_dev = sk(0x13);
    let invite_id = [0x12; 16];
    let secret = [0x22; 16];
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "member",
        T0 + 1,
    );
    engine.publish(&invite).expect("publish invite");
    let invite_event_id = frame_id(&invite, room);
    let join = make_join(
        &member_id,
        &member_dev,
        room,
        invite_event_id,
        invite_id,
        secret,
        "member",
        T0 + 2,
    );
    engine.publish(&join).expect("publish join");
    let before_remove = engine.counters().membership_projection_recomputes;

    let join_event_id = frame_id(&join, room);
    let remove = make_remove(
        &sk(1),
        &sk(2),
        room,
        join_event_id,
        member_id.identity_key(),
        T0 + 3,
    );
    engine.publish(&remove).expect("publish remove");

    assert_counter_increased_by(&engine, before_remove, 1);
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.status(&member_id.identity_key()),
        Some(Status::Removed)
    );
    assert!(!snapshot.is_active(&member_id.identity_key()));
    assert!(!snapshot
        .active_members()
        .any(|m| m.device == Some(member_dev.device_key())));
    assert_eq!(snapshot.active_member_count(), 1);
}

#[test]
fn content_after_join_in_same_events_message_uses_refreshed_projection() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x14);
    let member_dev = sk(0x15);
    let invite_id = [0x14; 16];
    let secret = [0x24; 16];
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "member",
        T0 + 1,
    );
    engine.publish(&invite).expect("publish invite");
    let _ = engine.take_ingested();
    let before = engine.counters().membership_projection_recomputes;

    let join = make_join(
        &member_id,
        &member_dev,
        room,
        frame_id(&invite, room),
        invite_id,
        secret,
        "member",
        T0 + 2,
    );
    let join_id = frame_id(&join, room);
    let msg = make_message(&member_id, &member_dev, room, join_id, "same batch", T0 + 3);
    engine.on_message(
        NODE_A,
        SyncMessage::Events {
            room_id: room,
            frames: vec![join, msg.clone()],
        },
    );

    assert_counter_increased_by(&engine, before, 1);
    assert!(engine
        .take_ingested()
        .iter()
        .any(|se| se.event_id == frame_id(&msg, room)));
    assert!(!engine
        .logs()
        .iter()
        .any(|line| line.contains("anti_amplification_signer")
            || line.contains("reject.not_a_member")));
}

#[test]
fn buffered_membership_acceptance_refreshes_cached_projection() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x16);
    let member_dev = sk(0x17);
    let invite_id = [0x16; 16];
    let secret = [0x26; 16];
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "member",
        T0 + 1,
    );
    let join = make_join(
        &member_id,
        &member_dev,
        room,
        frame_id(&invite, room),
        invite_id,
        secret,
        "member",
        T0 + 2,
    );
    let before = engine.counters().membership_projection_recomputes;

    engine.ingest_frame(NODE_A, &join);
    assert_counter_unchanged(&engine, before);
    assert_eq!(engine.counters().parked, 1);

    engine.ingest_frame(NODE_A, &invite);

    assert_counter_increased_by(&engine, before, 1);
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.status(&member_id.identity_key()),
        Some(Status::Active)
    );
    assert_eq!(snapshot.active_member_count(), 2);
}

#[test]
fn duplicate_membership_event_does_not_refresh_cached_projection() {
    let cfg = SyncConfig {
        early_event_id_dedup_cache_entries: 0,
        ..SyncConfig::default()
    };
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let member_id = sk(0x18);
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        [0x18; 16],
        [0x28; 16],
        "member",
        T0 + 1,
    );
    engine.publish(&invite).expect("publish invite");
    let before_duplicate = engine.counters().membership_projection_recomputes;

    engine.ingest_frame(NODE_A, &invite);

    assert_counter_unchanged(&engine, before_duplicate);
}

#[test]
fn store_retry_success_does_not_refresh_membership_projection_again() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x19);
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        [0x19; 16],
        [0x29; 16],
        "member",
        T0 + 1,
    );
    let before = engine.counters().membership_projection_recomputes;
    engine.store_mut().fail_next_inserts(1);

    engine.publish(&invite).expect("publish invite");

    assert_counter_increased_by(&engine, before, 1);
    assert_eq!(engine.store_retry_len(), 1);
    let after_failed_publish = engine.counters().membership_projection_recomputes;

    engine.on_tick(T0 + 2);

    assert_counter_unchanged(&engine, after_failed_publish);
    assert_eq!(engine.store_retry_len(), 0);
}

#[test]
fn reopened_engine_uses_cached_projection_from_store_without_runtime_recompute() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let member_id = sk(0x1a);
    let member_dev = sk(0x1b);
    let invite_id = [0x1a; 16];
    let secret = [0x2a; 16];
    let invite = make_invite(
        &sk(1),
        &sk(2),
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "member",
        T0 + 1,
    );
    engine.publish(&invite).expect("publish invite");
    let join = make_join(
        &member_id,
        &member_dev,
        room,
        frame_id(&invite, room),
        invite_id,
        secret,
        "member",
        T0 + 2,
    );
    engine.publish(&join).expect("publish join");

    let store = engine.into_store();
    let reopened = SyncEngine::open(store, room, SyncConfig::default()).expect("reopen");

    assert_eq!(reopened.counters().membership_projection_recomputes, 0);
    assert_eq!(
        reopened.snapshot().status(&member_id.identity_key()),
        Some(Status::Active)
    );
}

#[test]
fn cached_snapshot_remains_available_when_completeness_changes_without_membership() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let msg = make_message(&sk(1), &sk(2), room, genesis_id, "admin tip", T0 + 1);
    let msg_id = frame_id(&msg, room);
    let before = engine.counters().membership_projection_recomputes;

    engine.on_message(
        NODE_A,
        SyncMessage::AdminTip {
            room_id: room,
            tip: Some((msg_id, 1)),
        },
    );

    assert_eq!(engine.completeness(), Completeness::AdminViewSuspect);
    assert_counter_unchanged(&engine, before);
    assert_eq!(engine.snapshot().active_member_count(), 1);
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
    assert_eq!(engine.trust_decisions(), [] as [_; 0]);

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
    assert_eq!(engine.trust_decisions(), [] as [_; 0]);
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

// A failed parent insert can leave a persisted descendant with a NULL lamport.
// On restart that descendant is absent from `room_tail` and therefore from the
// rebuilt fold. Its id must not be seeded into the early cache: a peer replay is
// what restores it to the in-memory fold after the parent hole heals.
#[test]
fn restart_dedup_seed_excludes_causally_unplaced_rows() {
    let cfg = SyncConfig::default();
    let (mut engine, room, genesis_id) = seeded_engine(cfg);
    let admin_id = sk(1);
    let admin_dev = sk(2);
    let parent = make_message(
        &admin_id,
        &admin_dev,
        room,
        genesis_id,
        "missing parent",
        T0 + 1,
    );
    let parent_id = frame_id(&parent, room);
    let child = make_message(
        &admin_id,
        &admin_dev,
        room,
        parent_id,
        "stored child",
        T0 + 2,
    );

    engine.store_mut().fail_next_inserts(1);
    engine.publish(&parent).expect("fold accepts parent");
    engine.publish(&child).expect("fold accepts child");
    assert_eq!(
        engine.store_retry_len(),
        1,
        "the parent is still a store hole"
    );
    assert_eq!(
        engine.room_tail(100).expect("tail").len(),
        1,
        "the NULL-lamport child is not causally placed"
    );

    let store = engine.into_store();
    let mut reopened = SyncEngine::open(store, room, cfg).expect("reopen");
    assert_eq!(
        reopened.fold_tracked_len(),
        1,
        "only genesis was restored from the causal room tail"
    );
    let early_before = reopened.counters().early_duplicates;
    let duplicates_before = reopened.counters().duplicates;

    reopened.ingest_frame(NODE_A, &parent);
    assert_eq!(
        reopened.fold_tracked_len(),
        2,
        "the parent replay heals the hole"
    );
    reopened.ingest_frame(NODE_A, &child);

    assert_eq!(
        reopened.counters().early_duplicates,
        early_before,
        "an event omitted from the rebuilt fold must not be dropped early"
    );
    assert_eq!(
        reopened.counters().duplicates,
        duplicates_before + 1,
        "the stored child reaches the full fold + idempotent store path"
    );
    assert_eq!(
        reopened.fold_tracked_len(),
        3,
        "the child replay restores the event to the membership fold"
    );
    assert_eq!(reopened.room_tail(100).expect("tail").len(), 3);
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
// retry path preserves parent-before-child order, blocks the child behind a
// still-failing parent, then recovers with the full deferred bookkeeping.
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
    let m1_id = frame_id(&m1, room);
    // Make m2 a child whose id sorts *before* its parent. The old retry loop's
    // BTreeMap iteration would therefore announce m2 first; enqueue-order retry
    // must still land and announce the causal parent first.
    let (m2, m2_id) = (0u64..256)
        .find_map(|i| {
            let frame = make_message(
                &admin_id,
                &admin_dev,
                room,
                m1_id,
                &format!("child-{i}"),
                T0 + 2,
            );
            let id = frame_id(&frame, room);
            (id < m1_id).then_some((frame, id))
        })
        .expect("find a child id that sorts before its parent");

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

    // Fail the first retry too. Enqueue-order processing tries m1 first and
    // stops the pass, so m2 cannot land or be announced above the parent hole.
    // The old bytewise-id loop tried m2 first (its id is deliberately lower),
    // then continued to land and announce m1 in this tick.
    engine.store_mut().fail_next_inserts(1);
    let blocked = engine.on_tick(T0 + 3);
    assert!(
        events_frames(&blocked).is_empty(),
        "a failed parent retry blocks every later descendant"
    );
    assert!(
        engine.take_ingested().is_empty(),
        "the blocked retry pass has no feed side effects"
    );
    assert_eq!(engine.store_retry_len(), 2);
    assert_eq!(engine.room_tail(100).expect("tail").len(), 1);

    // The next tick lands both events with their deferred post-commit
    // bookkeeping. Each event fans out to every peer except its original
    // sender (NODE_A), so NODE_B sees both in causal/input order.
    let out = engine.on_tick(T0 + 4);
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
        fanned_to_b,
        vec![m1_id, m2_id],
        "retry fan-out preserves parent-before-child enqueue order"
    );
    assert_eq!(engine.store_retry_len(), 0);
    assert_eq!(engine.counters().accepted, 3, "genesis + m1 + m2");
    let fed: Vec<EventId> = engine
        .take_ingested()
        .into_iter()
        .map(|se| se.event_id)
        .collect();
    assert_eq!(
        fed,
        vec![m1_id, m2_id],
        "retry feed preserves parent-before-child enqueue order"
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

// ---------------------------------------------------------------------------
// Bounded tick-pull fan-out + the accept-path `fanout` marker (N=40
// hub-overload isolation: an unbounded pull fan-out lets a high-degree node
// summon up to N-1 overlapping serve batches per tick, saturating its inbound
// budgets; see `SyncConfig::pull_fanout_peers`).
// ---------------------------------------------------------------------------

fn peer(seed: u8) -> PeerId {
    PeerId::from_bytes([seed; 32])
}

/// With more peers than `pull_fanout_peers`, one tick pulls from exactly the
/// configured window while the (cheap, load-bearing) admin tip still reaches
/// every peer — and successive ticks rotate the window across the whole set.
#[test]
fn tick_pulls_are_bounded_to_a_rotating_window_and_sweep_all_peers() {
    let cfg = SyncConfig::default();
    let bound = cfg.pull_fanout_peers;
    let (mut engine, _room, _genesis) = seeded_engine(cfg);
    let peers: Vec<PeerId> = (0..6).map(|i| peer(0xC0 + i)).collect();
    for p in &peers {
        let _ = engine.on_connect(*p);
    }

    let mut pulled_union: std::collections::BTreeSet<PeerId> = std::collections::BTreeSet::new();
    for round in 0..peers.len() as u64 {
        // `on_connect` re-arms the pull flag without adding sync work, so every
        // tick in this loop emits pulls (a converged engine would otherwise
        // quiesce — that suppression is separate, older behavior).
        let _ = engine.on_connect(peers[0]);
        let out = engine.on_tick(T0 + round);

        let tips: std::collections::BTreeSet<PeerId> = out
            .iter()
            .filter(|o| matches!(o.msg, SyncMessage::AdminTip { .. }))
            .map(|o| o.peer)
            .collect();
        assert_eq!(
            tips.len(),
            peers.len(),
            "the admin tip must still reach every peer every tick"
        );

        let pulled: std::collections::BTreeSet<PeerId> = out
            .iter()
            .filter(|o| matches!(o.msg, SyncMessage::WantRecentChat { .. }))
            .map(|o| o.peer)
            .collect();
        assert_eq!(
            pulled.len(),
            bound,
            "each tick must pull from exactly `pull_fanout_peers` peers"
        );
        // Membership and chat pulls target the same window.
        let membership_pulled: std::collections::BTreeSet<PeerId> = out
            .iter()
            .filter(|o| matches!(o.msg, SyncMessage::WantMembership { .. }))
            .map(|o| o.peer)
            .collect();
        assert_eq!(
            membership_pulled, pulled,
            "membership and chat pulls must share the tick's window"
        );
        // No pull Outgoing is ever marked as fan-out.
        assert!(
            out.iter().all(|o| !o.fanout),
            "tick emissions are targeted, never fan-out"
        );
        pulled_union.extend(pulled);
    }
    assert_eq!(
        pulled_union,
        peers.iter().copied().collect(),
        "rotation must sweep every peer within one full cycle of ticks"
    );
}

/// Rooms with at most `pull_fanout_peers` peers keep the original behavior:
/// every peer is pulled from on every pull-emitting tick.
#[test]
fn tick_pulls_reach_all_peers_at_or_under_the_bound() {
    let cfg = SyncConfig::default();
    let bound = cfg.pull_fanout_peers;
    let (mut engine, _room, _genesis) = seeded_engine(cfg);
    #[allow(clippy::cast_possible_truncation)] // bound is 3 in the default config
    let peers: Vec<PeerId> = (0..bound as u8).map(|i| peer(0xD0 + i)).collect();
    for p in &peers {
        let _ = engine.on_connect(*p);
    }
    let _ = engine.on_connect(peers[0]);
    let out = engine.on_tick(T0);
    let pulled: std::collections::BTreeSet<PeerId> = out
        .iter()
        .filter(|o| matches!(o.msg, SyncMessage::WantRecentChat { .. }))
        .map(|o| o.peer)
        .collect();
    assert_eq!(
        pulled,
        peers.iter().copied().collect(),
        "at or under the bound, every peer is pulled from (pre-bound behavior)"
    );
}

/// The accept-path fan-out is the one emission marked `fanout: true`; a
/// transport with a broadcast plane rides it, and targeted serves must never
/// carry the marker. Pinned at the publish path, which shares the fan-out
/// site with inbound accepts.
#[test]
fn publish_fanout_is_marked_and_targeted_pulls_are_not() {
    let (mut engine, room, genesis_id) = seeded_engine(SyncConfig::default());
    let _ = engine.on_connect(NODE_A);
    let _ = engine.on_connect(NODE_B);

    let (admin_id, admin_dev) = (sk(1), sk(2));
    let msg = make_message(
        &admin_id,
        &admin_dev,
        room,
        genesis_id,
        "fanout-marker",
        T0 + 1,
    );
    let out = engine.publish(&msg).expect("publish");

    let events: Vec<&Outgoing> = out
        .iter()
        .filter(|o| matches!(o.msg, SyncMessage::Events { .. }))
        .collect();
    assert_eq!(events.len(), 2, "publish fans the event to both peers");
    assert!(
        events.iter().all(|o| o.fanout),
        "accept-path fan-out must be marked fanout: true"
    );
}

/// `pull_fanout_peers == 0` must be rejected at config validation: a zero
/// window would silence anti-entropy entirely.
#[test]
fn zero_pull_fanout_is_rejected() {
    let cfg = SyncConfig {
        pull_fanout_peers: 0,
        ..SyncConfig::default()
    };
    assert_eq!(cfg.validate(), Err("pull_fanout_zero"));
}

/// A SINGLE forced pull must sweep the full rotating cycle on its own: one
/// nudge arms `ceil(peers / window)` consecutive pull-emitting ticks, so the
/// peer holding a dropped frame is reached even when `has_pending_sync_work`
/// is false throughout. (The rotation test above re-arms every round, which
/// would mask a sweep that dies after one window.)
#[test]
fn single_forced_pull_sweeps_every_peer_then_quiesces() {
    let cfg = SyncConfig::default();
    let bound = cfg.pull_fanout_peers;
    let (mut engine, _room, _genesis) = seeded_engine(cfg);
    let peers: Vec<PeerId> = (0..7).map(|i| peer(0xE0 + i)).collect();
    for p in &peers {
        let _ = engine.on_connect(*p); // each arms the forced-pull flag
    }
    let sweep_ticks = peers.len().div_ceil(bound);

    let mut pulled_union: std::collections::BTreeSet<PeerId> = std::collections::BTreeSet::new();
    for round in 0..sweep_ticks as u64 {
        // No re-arming inside the loop: the single arm from `on_connect` must
        // carry the whole sweep.
        let out = engine.on_tick(T0 + round);
        let pulled: Vec<PeerId> = out
            .iter()
            .filter(|o| matches!(o.msg, SyncMessage::WantRecentChat { .. }))
            .map(|o| o.peer)
            .collect();
        assert!(
            !pulled.is_empty(),
            "tick {round} of the armed sweep must emit pulls"
        );
        pulled_union.extend(pulled);
    }
    assert_eq!(
        pulled_union,
        peers.iter().copied().collect(),
        "one forced sweep must reach every peer"
    );

    // Sweep exhausted, no pending sync work: the mesh quiesces.
    let after = engine.on_tick(T0 + 100);
    assert!(
        !after
            .iter()
            .any(|o| matches!(o.msg, SyncMessage::WantRecentChat { .. })),
        "after the sweep completes, ticks emit no pulls until re-armed"
    );
}

// ---------------------------------------------------------------------------
// Content-key rotation lifecycle (issue #191 step 6 / spec D4/D5/D6/D7)
// ---------------------------------------------------------------------------

/// The deterministic admin device seed used by `seeded_engine` (`sk(2)`).
const ADMIN_DEV_SEED: [u8; 32] = [0x02; 32];

/// Peer and on-disk epoch-key adoption, opt-in (Refs #429).
fn adoption_config(on: bool) -> SyncConfig {
    SyncConfig {
        experimental_room_key_adoption: on,
        ..SyncConfig::default()
    }
}

/// Like [`seeded_engine`] but opens the engine with the admin's device seed so
/// it can unwrap epoch keys from distribution payloads.
fn seeded_engine_with_admin_seed(cfg: SyncConfig) -> (SyncEngine, RoomId, EventId) {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let store = EventStore::open_in_memory().expect("store");
    let mut engine =
        SyncEngine::open_with_local_device(store, room, cfg, Some(ADMIN_DEV_SEED)).expect("open");
    engine.publish(&genesis).expect("publish genesis");
    let genesis_id = frame_id(&genesis, room);
    (engine, room, genesis_id)
}

/// Collect the device keys of every currently-Active member (the recipients for
/// a key distribution).
fn active_device_keys(engine: &SyncEngine) -> Vec<DeviceKey> {
    engine
        .snapshot()
        .active_members()
        .filter_map(|m| m.device)
        .collect()
}

#[test]
fn removal_with_rotation_adopts_epoch_key_for_remaining_member() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);
    let member_id = sk(3);
    let member_dev = sk(4);

    // Invite and join a second member so the room has an Active non-admin.
    let invite_id = [0x11; 16];
    let secret = [0x22; 16];
    let invite = make_invite(
        &admin_id,
        &admin_dev,
        room,
        genesis_id,
        member_id.identity_key(),
        invite_id,
        secret,
        "member",
        T0 + 1,
    );
    let invite_id_ev = frame_id(&invite, room);
    engine.publish(&invite).expect("publish invite");

    let join = make_join(
        &member_id,
        &member_dev,
        room,
        invite_id_ev,
        invite_id,
        secret,
        "member",
        T0 + 2,
    );
    engine.publish(&join).expect("publish join");
    assert_eq!(engine.snapshot().active_member_count(), 2);

    // Admin removes the member and rotates to epoch 2. The removed member is
    // excluded from recipients; only the admin's device remains.
    let new_key = RoomKey::generate();
    let recipients = active_device_keys(&engine);
    assert_eq!(recipients.len(), 2);
    let recipients: Vec<DeviceKey> = recipients
        .into_iter()
        .filter(|d| *d != member_dev.device_key())
        .collect();
    assert_eq!(recipients.len(), 1);

    let removal = build_member_removed_with_rotation(
        &admin_id,
        &admin_dev,
        &room,
        &member_id.identity_key(),
        Some(member_dev.device_key()),
        None,
        None,
        Some(2),
        Some(&new_key),
        &recipients,
        &[frame_id(&join, room)],
        T0 + 3,
    )
    .expect("build removal with rotation");
    engine
        .publish(&removal.to_bytes())
        .expect("publish removal");

    assert!(
        engine.has_room_key(2),
        "engine with admin seed must adopt epoch 2 key"
    );
    assert_eq!(
        engine.room_key(2).expect("epoch 2 key").as_bytes(),
        new_key.as_bytes(),
        "adopted key must match the one wrapped in the rotation payload"
    );
    assert_eq!(engine.snapshot().active_member_count(), 1);
}

#[test]
fn standalone_key_distribution_adopts_epoch_key() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);

    let new_key = RoomKey::generate();
    let distribution = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &new_key,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("build key distribution");
    engine
        .publish(&distribution.to_bytes())
        .expect("publish key distribution");

    assert!(
        engine.has_room_key(2),
        "standalone distribution must be adopted"
    );
    assert_eq!(
        engine.room_key(2).expect("epoch 2 key").as_bytes(),
        new_key.as_bytes()
    );
}

#[test]
fn conflicting_keys_for_same_epoch_poison_epoch() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);

    // Two distinct keys for the same epoch, both wrapped for the admin device.
    let key_a = RoomKey::from_bytes([0xAA; 32]);
    let key_b = RoomKey::from_bytes([0xBB; 32]);

    let dist_a = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_a,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("build distribution A");
    let dist_b = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_b,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 2,
    )
    .expect("build distribution B");

    engine.publish(&dist_a.to_bytes()).expect("publish A");
    engine.publish(&dist_b.to_bytes()).expect("publish B");

    // A second commitment poisons the epoch. Neither key stays usable.
    assert!(
        engine.is_room_key_poisoned(2),
        "a conflicting commitment must poison the epoch"
    );
    assert!(
        engine.room_key(2).is_none(),
        "neither conflicting key is held"
    );
}

#[test]
fn adopted_keys_survive_engine_restart() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);

    let new_key = RoomKey::from_bytes([0xCC; 32]);
    let distribution = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &new_key,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("build key distribution");
    engine
        .publish(&distribution.to_bytes())
        .expect("publish key distribution");
    assert!(engine.has_room_key(2));

    // Restart: store is durable, the in-memory key store is not.
    let store = engine.into_store();
    let reopened = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");

    assert!(
        reopened.has_room_key(2),
        "epoch 2 key must reload from persisted room_keys table"
    );
    assert_eq!(
        reopened.room_key(2).expect("epoch 2 key").as_bytes(),
        new_key.as_bytes()
    );
}

/// Two admin commitments poison the epoch in the session. The first key's row
/// can remain on disk, and open poisons the epoch again so that row is not
/// readable (Refs #429 #436).
#[test]
fn poisoned_epoch_resolution_is_fail_closed_on_reopen() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);

    // Two conflicting same-epoch (2) distributions, both wrapped for the admin
    // device. The deterministic resolution picks the candidate with the
    // lexicographically smallest event_id.
    let key_a = RoomKey::from_bytes([0xAA; 32]);
    let key_b = RoomKey::from_bytes([0xBB; 32]);
    let dist_a = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_a,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("build distribution A");
    let dist_b = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_b,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 2,
    )
    .expect("build distribution B");
    let id_a = frame_id(&dist_a.to_bytes(), room);

    engine.publish(&dist_a.to_bytes()).expect("publish A");
    engine.publish(&dist_b.to_bytes()).expect("publish B");
    assert!(engine.is_room_key_poisoned(2));
    assert!(engine.room_key(2).is_none());

    // The first adoption's row stays. Open must not treat it as the winner.
    let persisted = engine
        .store_mut()
        .load_room_keys(&room)
        .expect("load room keys")
        .into_iter()
        .find(|(epoch, _)| *epoch == 2)
        .map(|(_, (_, source_event_id))| source_event_id)
        .expect("epoch 2 key persisted");
    assert_eq!(persisted, id_a);

    // Restart: both commitments are in the store, so open poisons the epoch
    // and does not expose the persisted winner.
    let store = engine.into_store();
    let reopened = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");

    assert!(
        reopened.is_room_key_poisoned(2),
        "two stored commitments poison the epoch on open"
    );
    assert!(
        reopened.room_key(2).is_none(),
        "the persisted winner is not readable after reopen"
    );

    // The first adoption's row stays on disk. Open does not use it.
    let mut reopened = reopened;
    let persisted_after = reopened
        .store_mut()
        .load_room_keys(&room)
        .expect("load room keys")
        .into_iter()
        .find(|(epoch, _)| *epoch == 2)
        .map(|(_, (_, source_event_id))| source_event_id)
        .expect("epoch 2 key persisted after restart");
    assert_eq!(persisted_after, id_a);
}

/// A later conflicting key poisons the epoch. Order-independent selection of
/// a winner is #436, not this session.
#[test]
fn conflict_resolution_attributes_winner_to_minimum_source_id() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let admin_id = sk(1);
    let admin_dev = sk(2);

    // Keys: K = 0xAA offered under two ids (A, B), K' = 0xBB under C.
    let key_k = RoomKey::from_bytes([0xAA; 32]);
    let key_k_prime = RoomKey::from_bytes([0xBB; 32]);
    let dist_a = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_k,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("build distribution A");
    let dist_b = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_k,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 2,
    )
    .expect("build distribution B (same key as A, different id)");
    let dist_c = crate::event::distribution::build_member_key_distribution(
        &admin_id,
        &admin_dev,
        &room,
        2,
        &key_k_prime,
        &[admin_dev.device_key()],
        &[genesis_id],
        T0 + 3,
    )
    .expect("build distribution C (conflicting key)");
    engine.publish(&dist_a.to_bytes()).expect("publish A");
    engine.publish(&dist_b.to_bytes()).expect("publish B");
    assert!(engine.has_room_key(2), "the same key re-offered stays held");
    engine.publish(&dist_c.to_bytes()).expect("publish C");

    assert!(engine.is_room_key_poisoned(2));
    assert!(
        engine.room_key(2).is_none(),
        "the conflicting key poisons the epoch instead of selecting a winner"
    );
}

// ---------------------------------------------------------------------------
// KeyHistory adoption gate (Refs #429 #343 #286)
// ---------------------------------------------------------------------------

fn open_member(room: RoomId, seed_byte: u8, cfg: SyncConfig) -> SyncEngine {
    SyncEngine::open_with_local_device(
        EventStore::open_in_memory().expect("store"),
        room,
        cfg,
        Some([seed_byte; 32]),
    )
    .expect("open")
}

fn zero_event_id() -> EventId {
    EventId::from_bytes([0u8; 32])
}

fn history_chunk(
    room: &RoomId,
    epoch: u64,
    key: &RoomKey,
    event_id: EventId,
    recipient: DeviceKey,
) -> KeyHistoryChunk {
    let distribution =
        build_key_distribution_content(room, epoch, key, &[recipient]).expect("wrap chunk");
    KeyHistoryChunk {
        distribution_event_id: event_id,
        epoch,
        key_commitment: distribution.key_commitment,
        wrapped_keys: distribution.wrapped_keys,
    }
}

fn deliver_chunks(
    engine: &mut SyncEngine,
    from: PeerId,
    room: RoomId,
    chunks: Vec<KeyHistoryChunk>,
) {
    let _ = engine.on_message(
        from,
        SyncMessage::KeyHistory {
            room_id: room,
            chunks,
        },
    );
}

/// Admin alice (`sk(1)`/`sk(2)`), member bob (`sk(3)`/`sk(4)`), member carol
/// (`sk(5)`/`sk(6)`), converged. Returns carol's join id for parenting.
fn converge_trio(cfg: SyncConfig) -> (SimNet, RoomId, EventId) {
    let (admin_id, admin_dev) = (sk(1), sk(2));
    let (bob_id, bob_dev) = (sk(3), sk(4));
    let (carol_id, carol_dev) = (sk(5), sk(6));
    let (genesis, room) = make_genesis(&admin_id, &admin_dev);
    let mut net = SimNet::new(room);
    net.add_peer(NODE_A, open_member(room, 0x02, cfg));
    net.add_peer(NODE_B, open_member(room, 0x04, cfg));
    net.add_peer(NODE_C, open_member(room, 0x06, cfg));

    net.engine_mut(NODE_A).publish(&genesis).expect("genesis");
    let mut prev = frame_id(&genesis, room);

    let inv_bob_id = [0x11; 16];
    let inv_bob_sec = [0x21; 16];
    let inv_bob = make_invite(
        &admin_id,
        &admin_dev,
        room,
        prev,
        bob_id.identity_key(),
        inv_bob_id,
        inv_bob_sec,
        "member",
        T0 + 1,
    );
    prev = frame_id(&inv_bob, room);
    net.engine_mut(NODE_A)
        .publish(&inv_bob)
        .expect("invite bob");

    let join_bob = make_join(
        &bob_id,
        &bob_dev,
        room,
        prev,
        inv_bob_id,
        inv_bob_sec,
        "member",
        T0 + 2,
    );
    prev = frame_id(&join_bob, room);
    net.engine_mut(NODE_A).publish(&join_bob).expect("join bob");

    let inv_carol_id = [0x12; 16];
    let inv_carol_sec = [0x22; 16];
    let inv_carol = make_invite(
        &admin_id,
        &admin_dev,
        room,
        prev,
        carol_id.identity_key(),
        inv_carol_id,
        inv_carol_sec,
        "member",
        T0 + 3,
    );
    prev = frame_id(&inv_carol, room);
    net.engine_mut(NODE_A)
        .publish(&inv_carol)
        .expect("invite carol");

    let join_carol = make_join(
        &carol_id,
        &carol_dev,
        room,
        prev,
        inv_carol_id,
        inv_carol_sec,
        "member",
        T0 + 4,
    );
    let join_carol_id = frame_id(&join_carol, room);
    net.engine_mut(NODE_A)
        .publish(&join_carol)
        .expect("join carol");

    net.connect_all();
    net.run_to_quiescence();
    assert_eq!(
        net.engine(NODE_C).snapshot().active_member_count(),
        3,
        "admin, attacker, and victim are Active"
    );
    (net, room, join_carol_id)
}

fn persisted_epochs(engine: &mut SyncEngine, room: RoomId) -> Vec<u64> {
    engine
        .store_mut()
        .load_room_keys(&room)
        .expect("load room keys")
        .into_keys()
        .collect()
}

/// A-1. Flag off: a forged `KeyHistory` (zeros id at epoch E, then `u64::MAX`)
/// adopts nothing and writes no `room_keys` row. A later admin DAG distribution
/// is stored and its key is not adopted.
#[test]
#[allow(clippy::too_many_lines)]
fn key_adoption_a1_flag_off_ignores_forged_history_and_dag() {
    let (mut net, room, head) = converge_trio(adoption_config(false));
    let carol_dev = sk(6).device_key();

    // Epoch 0 equals the untouched watermark, so a bound chunk is not rejected
    // by the epoch guard. The flag itself has to drop it.
    let epoch0_key = RoomKey::from_bytes([0x10; 32]);
    let dist0 = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        0,
        &epoch0_key,
        &[carol_dev],
        &[head],
        T0 + 5,
    )
    .expect("epoch 0 distribution");
    let dist0_bytes = dist0.to_bytes();
    let dist0_id = frame_id(&dist0_bytes, room);
    net.engine_mut(NODE_A)
        .publish(&dist0_bytes)
        .expect("publish epoch 0");
    net.engine_mut(NODE_C).ingest_frame(NODE_A, &dist0_bytes);
    assert!(
        net.engine_mut(NODE_C)
            .store_mut()
            .get_in_room(&room, &dist0_id)
            .expect("get")
            .is_some(),
        "the epoch 0 distribution is stored"
    );
    assert!(!net.engine(NODE_C).has_room_key(0));
    deliver_chunks(
        net.engine_mut(NODE_C),
        NODE_B,
        room,
        vec![history_chunk(&room, 0, &epoch0_key, dist0_id, carol_dev)],
    );
    assert!(
        !net.engine(NODE_C).has_room_key(0),
        "flag off does not adopt a bound chunk"
    );

    let epoch_e: u64 = 3;
    let forged = history_chunk(
        &room,
        epoch_e,
        &RoomKey::from_bytes([0xEE; 32]),
        zero_event_id(),
        carol_dev,
    );
    let forged_max = history_chunk(
        &room,
        u64::MAX,
        &RoomKey::from_bytes([0xEF; 32]),
        zero_event_id(),
        carol_dev,
    );
    deliver_chunks(
        net.engine_mut(NODE_C),
        NODE_B,
        room,
        vec![forged, forged_max],
    );

    assert!(
        !net.engine(NODE_C).has_room_key(epoch_e),
        "flag off must not adopt the forged epoch"
    );
    assert!(!net.engine(NODE_C).has_room_key(u64::MAX));
    assert!(
        net.engine(NODE_C)
            .logs()
            .iter()
            .any(|line| line == "key_history_ignored n=2"),
        "one ignore log per KeyHistory message, got {:?}",
        net.engine(NODE_C).logs()
    );
    assert!(
        persisted_epochs(net.engine_mut(NODE_C), room).is_empty(),
        "forged chunks must not write a room_keys row"
    );

    let admin_key = RoomKey::from_bytes([0x44; 32]);
    let admin_epoch = 4;
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        admin_epoch,
        &admin_key,
        &[carol_dev],
        &[head],
        T0 + 5,
    )
    .expect("admin distribution");
    let bytes = dist.to_bytes();
    let later_id = frame_id(&bytes, room);
    net.engine_mut(NODE_A)
        .publish(&bytes)
        .expect("admin publishes the distribution");
    net.engine_mut(NODE_C).ingest_frame(NODE_A, &bytes);

    assert!(
        net.engine_mut(NODE_C)
            .store_mut()
            .get_in_room(&room, &later_id)
            .expect("get")
            .is_some(),
        "the admin distribution event is stored"
    );
    assert!(
        !net.engine(NODE_C).has_room_key(admin_epoch),
        "flag off stores the DAG distribution and does not adopt its key"
    );
    assert!(
        !net.engine(NODE_C).has_room_key(epoch_e),
        "the forged epoch is still absent after the DAG event"
    );
    assert!(
        persisted_epochs(net.engine_mut(NODE_C), room).is_empty(),
        "no room_keys row after the ignored DAG distribution"
    );
    assert_eq!(
        net.engine(NODE_C).snapshot().active_member_count(),
        3,
        "ignoring key material does not change the fold"
    );
}

/// A-2. Flag on: unbound chunks are rejected, the admin key for E survives the
/// all-zero tie-break, `u64::MAX` does not pin the watermark, a later admin
/// epoch is adopted, and a bound chunk from an honest server is adopted.
#[test]
#[allow(clippy::too_many_lines)]
fn key_adoption_a2_flag_on_rejects_unbound_and_adopts_bound() {
    let (mut net, room, head) = converge_trio(adoption_config(true));
    let carol_dev = sk(6).device_key();
    let epoch_e: u64 = 2;
    let admin_key = RoomKey::from_bytes([0x11; 32]);
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        epoch_e,
        &admin_key,
        &[sk(2).device_key(), carol_dev],
        &[head],
        T0 + 5,
    )
    .expect("epoch E distribution");
    let bytes = dist.to_bytes();
    let dist_id = frame_id(&bytes, room);
    net.engine_mut(NODE_A).publish(&bytes).expect("publish E");
    net.engine_mut(NODE_C).ingest_frame(NODE_A, &bytes);
    net.engine_mut(NODE_B).ingest_frame(NODE_A, &bytes);
    assert_eq!(
        net.engine(NODE_C)
            .room_key(epoch_e)
            .expect("carol holds E")
            .as_bytes(),
        admin_key.as_bytes()
    );
    assert!(
        !net.engine(NODE_B).has_room_key(epoch_e),
        "bob was not in the wrap set"
    );

    let evil = RoomKey::from_bytes([0xEE; 32]);
    let bob_join = net
        .engine(NODE_C)
        .room_tail(16)
        .expect("tail")
        .into_iter()
        .find(|se| se.event_type == EventType::MemberJoined)
        .expect("a join event")
        .event_id;
    let rejects = [
        history_chunk(&room, epoch_e, &evil, zero_event_id(), carol_dev),
        history_chunk(&room, epoch_e, &evil, bob_join, carol_dev),
        history_chunk(&room, epoch_e + 9, &evil, dist_id, carol_dev),
        history_chunk(&room, epoch_e, &evil, dist_id, carol_dev),
        history_chunk(&room, u64::MAX, &evil, zero_event_id(), carol_dev),
    ];
    for chunk in rejects {
        deliver_chunks(net.engine_mut(NODE_C), NODE_B, room, vec![chunk]);
    }

    assert_eq!(
        net.engine(NODE_C)
            .room_key(epoch_e)
            .expect("admin key kept")
            .as_bytes(),
        admin_key.as_bytes(),
        "unbound chunks must not win the all-zero tie-break"
    );
    assert!(!net.engine(NODE_C).is_room_key_poisoned(epoch_e));
    assert!(!net.engine(NODE_C).has_room_key(epoch_e + 9));
    assert!(!net.engine(NODE_C).has_room_key(u64::MAX));
    let unbound_logs = net
        .engine(NODE_C)
        .logs()
        .iter()
        .filter(|line| line.starts_with("key_history_unbound "))
        .count();
    assert_eq!(unbound_logs, 5, "each unbound chunk is logged and dropped");

    let max_key = RoomKey::from_bytes([0xFE; 32]);
    let max_dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        u64::MAX,
        &max_key,
        &[carol_dev],
        &[dist_id],
        T0 + 6,
    )
    .expect("max epoch distribution");
    let max_bytes = max_dist.to_bytes();
    let max_id = frame_id(&max_bytes, room);
    net.engine_mut(NODE_C).ingest_frame(NODE_A, &max_bytes);
    assert!(
        net.engine_mut(NODE_C)
            .store_mut()
            .get_in_room(&room, &max_id)
            .expect("get")
            .is_some(),
        "the u64::MAX distribution event is stored"
    );
    assert!(
        !net.engine(NODE_C).has_room_key(u64::MAX),
        "u64::MAX is rejected on the DAG path"
    );

    let next_key = RoomKey::from_bytes([0x33; 32]);
    let next_epoch = epoch_e + 1;
    let later_dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        next_epoch,
        &next_key,
        &[sk(2).device_key(), carol_dev],
        &[dist_id],
        T0 + 7,
    )
    .expect("epoch E+1");
    let next_bytes = later_dist.to_bytes();
    let next_id = frame_id(&next_bytes, room);
    net.engine_mut(NODE_A)
        .publish(&next_bytes)
        .expect("publish E+1");
    net.engine_mut(NODE_C).ingest_frame(NODE_A, &next_bytes);
    net.engine_mut(NODE_B).ingest_frame(NODE_A, &next_bytes);
    assert_eq!(
        net.engine(NODE_C)
            .room_key(next_epoch)
            .expect("E+1 adopted after the max-epoch attempt")
            .as_bytes(),
        next_key.as_bytes()
    );

    let bob_peer = PeerId::from_bytes(*sk(4).device_key().as_bytes());
    let served = net.engine_mut(NODE_A).on_message(
        bob_peer,
        SyncMessage::WantKeyHistory {
            room_id: room,
            have_epochs: BTreeSet::new(),
        },
    );
    let mut served_ids = BTreeSet::new();
    for outgoing in served {
        let SyncMessage::KeyHistory { chunks, .. } = &outgoing.msg else {
            panic!("honest server must answer with KeyHistory");
        };
        for chunk in chunks {
            assert_ne!(
                chunk.distribution_event_id,
                zero_event_id(),
                "served chunks carry the real source id"
            );
            served_ids.insert((chunk.epoch, chunk.distribution_event_id));
        }
        let _ = net.engine_mut(NODE_B).on_message(NODE_A, outgoing.msg);
    }
    assert!(
        served_ids.contains(&(epoch_e, dist_id)),
        "epoch E is served under its distribution event, got {served_ids:?}"
    );
    assert!(
        served_ids.contains(&(next_epoch, next_id)),
        "epoch E+1 is served under its distribution event, got {served_ids:?}"
    );
    assert_eq!(
        net.engine(NODE_B)
            .room_key(epoch_e)
            .expect("bob adopts E")
            .as_bytes(),
        admin_key.as_bytes()
    );
    assert_eq!(
        net.engine(NODE_B)
            .room_key(next_epoch)
            .expect("bob adopts E+1")
            .as_bytes(),
        next_key.as_bytes()
    );

    // Flag off stores the distribution without adopting it. Reopen with the
    // flag on re-seeds the watermark from that event, so the bound chunk
    // adopts without a later DAG distribution.
    let (mut cold, cold_room, cold_genesis) = seeded_engine_with_admin_seed(adoption_config(false));
    let ahead_key = RoomKey::from_bytes([0x55; 32]);
    let ahead = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &cold_room,
        5,
        &ahead_key,
        &[sk(2).device_key()],
        &[cold_genesis],
        T0 + 1,
    )
    .expect("ahead distribution");
    let ahead_bytes = ahead.to_bytes();
    let ahead_id = frame_id(&ahead_bytes, cold_room);
    cold.publish(&ahead_bytes).expect("publish ahead");
    assert!(
        !cold.has_room_key(5),
        "flag off does not adopt the distribution used to plant the event"
    );
    let store = cold.into_store();
    let mut cold = SyncEngine::open_with_local_device(
        store,
        cold_room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen flag on");
    assert!(
        !cold.has_room_key(5),
        "flag off wrote no room_keys row to reload"
    );
    let ahead_chunk = history_chunk(&cold_room, 5, &ahead_key, ahead_id, sk(2).device_key());
    deliver_chunks(&mut cold, NODE_B, cold_room, vec![ahead_chunk.clone()]);
    assert_eq!(
        cold.room_key(5)
            .expect("seeded watermark lets the bound chunk adopt")
            .as_bytes(),
        ahead_key.as_bytes()
    );
    assert!(
        !cold
            .logs()
            .iter()
            .any(|line| line.starts_with("key_history_epoch_rejected ")),
        "a chunk at the seeded watermark is not ahead, got {:?}",
        cold.logs()
    );
    let later_key = RoomKey::from_bytes([0x66; 32]);
    let later = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &cold_room,
        6,
        &later_key,
        &[sk(2).device_key()],
        &[ahead_id],
        T0 + 2,
    )
    .expect("later distribution");
    cold.publish(&later.to_bytes()).expect("publish epoch 6");
    assert!(
        cold.has_room_key(6),
        "the DAG distribution raises the watermark"
    );
    deliver_chunks(&mut cold, NODE_B, cold_room, vec![ahead_chunk]);
    assert_eq!(
        cold.room_key(5)
            .expect("historical chunk adopted once covered")
            .as_bytes(),
        ahead_key.as_bytes()
    );
}

/// A-3. Reload. Flag off loads nothing and keeps every row. Flag on skips an
/// unbound (all-zero source) row and loads a row whose source event binds.
#[test]
fn key_adoption_a3_reload_skips_unbound_rows() {
    let epoch_bound = 2u64;
    let bound_key = RoomKey::from_bytes([0x11; 32]);
    let planted_key = RoomKey::from_bytes([0x22; 32]);
    let planted_epoch = 1u64;

    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(false));
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        epoch_bound,
        &bound_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("distribution");
    let dist_id = frame_id(&dist.to_bytes(), room);
    engine.publish(&dist.to_bytes()).expect("publish");
    assert!(!engine.has_room_key(epoch_bound));
    engine
        .store_mut()
        .save_room_key(&room, planted_epoch, &planted_key, &zero_event_id())
        .expect("plant zeros row");
    engine
        .store_mut()
        .save_room_key(&room, epoch_bound, &bound_key, &dist_id)
        .expect("plant bound row");
    let store = engine.into_store();
    let mut off = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(false),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen flag off");
    assert!(!off.has_room_key(planted_epoch));
    assert!(!off.has_room_key(epoch_bound));
    assert!(
        off.logs()
            .iter()
            .any(|line| line == "room_keys_ignored n=2"),
        "flag off logs the ignored rows, got {:?}",
        off.logs()
    );
    let kept: Vec<u64> = persisted_epochs(&mut off, room);
    assert_eq!(kept, vec![planted_epoch, epoch_bound]);

    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        epoch_bound,
        &bound_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("distribution");
    engine.publish(&dist.to_bytes()).expect("publish");
    assert!(engine.has_room_key(epoch_bound));
    engine
        .store_mut()
        .save_room_key(&room, planted_epoch, &planted_key, &zero_event_id())
        .expect("plant zeros row");
    let store = engine.into_store();
    let mut on = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen flag on");
    assert!(
        !on.has_room_key(planted_epoch),
        "an all-zero source id is not reloaded"
    );
    assert_eq!(
        on.room_key(epoch_bound)
            .expect("bound row loads")
            .as_bytes(),
        bound_key.as_bytes()
    );
    assert!(
        on.logs()
            .iter()
            .any(|line| line.starts_with("room_key_unbound epoch=1 ")),
        "the skipped row is logged, got {:?}",
        on.logs()
    );
    let kept: Vec<u64> = persisted_epochs(&mut on, room);
    assert_eq!(
        kept,
        vec![planted_epoch, epoch_bound],
        "skipped rows are kept"
    );
}

/// A-4. Flag off does not answer `WantKeyHistory`. Flag on serves the real
/// source id and skips keys attributed to the all-zero id.
#[test]
fn key_adoption_a4_serving_follows_the_flag() {
    let (mut engine, room, _genesis) = seeded_engine_with_admin_seed(adoption_config(false));
    engine
        .insert_room_key(1, &RoomKey::from_bytes([0xAB; 32]))
        .expect("local key");
    assert!(engine.has_room_key(1));
    let admin_peer = PeerId::from_bytes(*sk(2).device_key().as_bytes());
    let out = engine.on_message(
        admin_peer,
        SyncMessage::WantKeyHistory {
            room_id: room,
            have_epochs: BTreeSet::new(),
        },
    );
    assert!(
        out.is_empty(),
        "flag off must not serve key history, got {out:?}"
    );

    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    engine
        .insert_room_key(1, &RoomKey::from_bytes([0xAB; 32]))
        .expect("zeros-id local key");
    let real_key = RoomKey::from_bytes([0xCD; 32]);
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &real_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("distribution");
    let dist_id = frame_id(&dist.to_bytes(), room);
    engine.publish(&dist.to_bytes()).expect("publish");
    assert!(engine.has_room_key(1));
    assert!(engine.has_room_key(2));
    let out = engine.on_message(
        admin_peer,
        SyncMessage::WantKeyHistory {
            room_id: room,
            have_epochs: BTreeSet::new(),
        },
    );
    assert_eq!(out.len(), 1, "flag on serves one KeyHistory message");
    let SyncMessage::KeyHistory { chunks, .. } = &out[0].msg else {
        panic!("expected KeyHistory, got {:?}", out[0].msg);
    };
    assert_eq!(chunks.len(), 1, "the zeros-id key is not served");
    assert_eq!(chunks[0].epoch, 2);
    assert_eq!(chunks[0].distribution_event_id, dist_id);
    assert_ne!(chunks[0].distribution_event_id, zero_event_id());
}

/// Restart seeding. An admin distribution at epoch 5 is stored but not
/// unwrapped. After reopen the watermark is 5, so a bound chunk adopts, and a
/// lower-epoch DAG distribution is a backward replay.
#[test]
fn key_adoption_restart_seeds_watermark_from_stored_distribution() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let epoch5_key = RoomKey::from_bytes([0x55; 32]);
    let other = sk(4).device_key();
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        5,
        &epoch5_key,
        &[other],
        &[genesis_id],
        T0 + 1,
    )
    .expect("distribution");
    let dist_bytes = dist.to_bytes();
    let dist_id = frame_id(&dist_bytes, room);
    engine.publish(&dist_bytes).expect("publish");
    assert!(
        !engine.has_room_key(5),
        "the distribution does not wrap the local device"
    );

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert!(!engine.has_room_key(5));
    deliver_chunks(
        &mut engine,
        NODE_B,
        room,
        vec![history_chunk(
            &room,
            5,
            &epoch5_key,
            dist_id,
            sk(2).device_key(),
        )],
    );
    assert_eq!(
        engine
            .room_key(5)
            .expect("bound chunk adopts at the seeded watermark")
            .as_bytes(),
        epoch5_key.as_bytes()
    );

    let lower_key = RoomKey::from_bytes([0x33; 32]);
    let lower = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        3,
        &lower_key,
        &[sk(2).device_key()],
        &[dist_id],
        T0 + 2,
    )
    .expect("lower distribution");
    engine
        .publish(&lower.to_bytes())
        .expect("publish lower epoch");
    assert!(
        !engine.has_room_key(3),
        "a lower epoch than the seeded watermark is a backward replay"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|line| line.starts_with("backward_epoch_rejected ")),
        "backward rejection is logged, got {:?}",
        engine.logs()
    );
}

/// A removal rotation is part of the same scan. With no other distribution,
/// it is what seeds the watermark.
#[test]
fn key_adoption_restart_seeds_watermark_from_removal_rotation() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let epoch_key = RoomKey::from_bytes([0x44; 32]);
    let subject = sk(9).identity_key();
    let removal = build_member_removed_with_rotation(
        &sk(1),
        &sk(2),
        &room,
        &subject,
        None,
        None,
        None,
        Some(5),
        Some(&epoch_key),
        &[sk(4).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("removal rotation");
    let bytes = removal.to_bytes();
    let removal_id = frame_id(&bytes, room);
    engine.publish(&bytes).expect("publish removal");
    assert!(!engine.has_room_key(5));

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    deliver_chunks(
        &mut engine,
        NODE_B,
        room,
        vec![history_chunk(
            &room,
            5,
            &epoch_key,
            removal_id,
            sk(2).device_key(),
        )],
    );
    assert_eq!(
        engine
            .room_key(5)
            .expect("removal rotation seeds the watermark")
            .as_bytes(),
        epoch_key.as_bytes()
    );
}

fn served_epochs(engine: &mut SyncEngine, room: RoomId) -> BTreeSet<u64> {
    let admin_peer = PeerId::from_bytes(*sk(2).device_key().as_bytes());
    let out = engine.on_message(
        admin_peer,
        SyncMessage::WantKeyHistory {
            room_id: room,
            have_epochs: BTreeSet::new(),
        },
    );
    let mut epochs = BTreeSet::new();
    for outgoing in out {
        if let SyncMessage::KeyHistory { chunks, .. } = outgoing.msg {
            for chunk in chunks {
                epochs.insert(chunk.epoch);
            }
        }
    }
    epochs
}

/// D1 wraps us and arrives first; D2 does not. The live engine poisons the
/// epoch. After reopen the reloaded row is not readable or served.
#[test]
fn key_adoption_reopen_multi_commitment_d1_first_is_poisoned() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let key_one = RoomKey::from_bytes([0x11; 32]);
    let first = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_one,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("D1");
    let first_bytes = first.to_bytes();
    let first_id = frame_id(&first_bytes, room);
    engine.publish(&first_bytes).expect("publish D1");
    assert!(engine.has_room_key(2));

    let key_two = RoomKey::from_bytes([0x22; 32]);
    let second = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_two,
        &[sk(4).device_key()],
        &[first_id],
        T0 + 2,
    )
    .expect("D2");
    engine.publish(&second.to_bytes()).expect("publish D2");
    assert!(engine.is_room_key_poisoned(2));
    assert!(engine.room_key(2).is_none());
    assert!(
        !served_epochs(&mut engine, room).contains(&2),
        "a poisoned epoch is not served"
    );

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert!(
        engine.is_room_key_poisoned(2),
        "reopen poisons an epoch with two admin commitments"
    );
    assert!(
        engine.room_key(2).is_none(),
        "the reloaded row must not be readable"
    );
    assert!(
        persisted_epochs(&mut engine, room).contains(&2),
        "the room_keys row is kept and not used"
    );
    assert!(
        !served_epochs(&mut engine, room).contains(&2),
        "the reloaded row must not be served"
    );
}

/// D2 (does not wrap us) arrives before D1. After reopen the epoch is poisoned
/// even though the live session may have resolved to a key.
#[test]
fn key_adoption_reopen_multi_commitment_d2_first_is_poisoned() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let key_two = RoomKey::from_bytes([0x22; 32]);
    let second = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_two,
        &[sk(4).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("D2");
    let second_bytes = second.to_bytes();
    let second_id = frame_id(&second_bytes, room);
    engine.publish(&second_bytes).expect("publish D2");
    assert!(!engine.has_room_key(2));

    let key_one = RoomKey::from_bytes([0x11; 32]);
    let first = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_one,
        &[sk(2).device_key()],
        &[second_id],
        T0 + 2,
    )
    .expect("D1");
    engine.publish(&first.to_bytes()).expect("publish D1");

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert!(engine.is_room_key_poisoned(2));
    assert!(engine.room_key(2).is_none());
    assert!(!served_epochs(&mut engine, room).contains(&2));
}

/// One admin commitment reloads and is served.
#[test]
fn key_adoption_reopen_single_commitment_reloads() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let key = RoomKey::from_bytes([0x11; 32]);
    let dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("distribution");
    engine.publish(&dist.to_bytes()).expect("publish");
    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert!(!engine.is_room_key_poisoned(2));
    assert_eq!(
        engine
            .room_key(2)
            .expect("single commitment reloads")
            .as_bytes(),
        key.as_bytes()
    );
    assert!(served_epochs(&mut engine, room).contains(&2));
}

/// A stored admin `u64::MAX` distribution must not pin the watermark across
/// a reopen, so a later epoch still adopts.
#[test]
fn key_adoption_restart_max_epoch_does_not_pin_watermark() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let max_key = RoomKey::from_bytes([0xFE; 32]);
    let max_dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        u64::MAX,
        &max_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("max distribution");
    let max_bytes = max_dist.to_bytes();
    let max_id = frame_id(&max_bytes, room);
    engine.publish(&max_bytes).expect("publish max");
    assert!(!engine.has_room_key(u64::MAX));

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    let later_key = RoomKey::from_bytes([0x33; 32]);
    let later = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &later_key,
        &[sk(2).device_key()],
        &[max_id],
        T0 + 2,
    )
    .expect("later distribution");
    engine.publish(&later.to_bytes()).expect("publish later");
    assert_eq!(
        engine
            .room_key(2)
            .expect("a later epoch adopts after a stored u64::MAX distribution")
            .as_bytes(),
        later_key.as_bytes()
    );
}

/// A non-admin distribution inserted past the fold does not raise the
/// watermark on reopen.
#[test]
fn key_adoption_restart_non_admin_does_not_raise_watermark() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let stranger_key = RoomKey::from_bytes([0x77; 32]);
    let wire = crate::event::distribution::build_member_key_distribution(
        &sk(9),
        &sk(10),
        &room,
        5,
        &stranger_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("non-admin distribution");
    let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room))
        .expect("stateless-valid");
    engine
        .store_mut()
        .insert(&validated)
        .expect("direct insert");

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    let admin_key = RoomKey::from_bytes([0x44; 32]);
    let admin_dist = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        4,
        &admin_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 2,
    )
    .expect("admin distribution");
    engine
        .publish(&admin_dist.to_bytes())
        .expect("publish lower epoch");
    assert_eq!(
        engine
            .room_key(4)
            .expect("non-admin history must not pin the watermark")
            .as_bytes(),
        admin_key.as_bytes()
    );
}

/// After reopen, a new distribution with a different commitment poisons the
/// epoch even when this device cannot unwrap it.
#[test]
fn key_adoption_restart_seeded_commitment_catches_equivocation() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let first_key = RoomKey::from_bytes([0x11; 32]);
    let first = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &first_key,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("first distribution");
    let first_bytes = first.to_bytes();
    let first_id = frame_id(&first_bytes, room);
    engine.publish(&first_bytes).expect("publish first");
    assert!(engine.has_room_key(2));

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert_eq!(
        engine.room_key(2).expect("bound row reloads").as_bytes(),
        first_key.as_bytes()
    );

    let second_key = RoomKey::from_bytes([0x22; 32]);
    let second = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &second_key,
        &[sk(4).device_key()],
        &[first_id],
        T0 + 2,
    )
    .expect("conflicting distribution");
    engine
        .publish(&second.to_bytes())
        .expect("publish conflict");
    assert!(
        engine.is_room_key_poisoned(2),
        "the seeded commitment must conflict with the new distribution"
    );
    assert!(!engine.has_room_key(2));
}

/// A non-admin distribution inserted past the fold does not bind a chunk.
#[test]
fn key_adoption_non_admin_store_row_does_not_bind() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let stranger_id = sk(9);
    let stranger_dev = sk(10);
    let planted = RoomKey::from_bytes([0x77; 32]);
    let wire = crate::event::distribution::build_member_key_distribution(
        &stranger_id,
        &stranger_dev,
        &room,
        0,
        &planted,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("non-admin distribution");
    let bytes = wire.to_bytes();
    let validated = validate_wire_bytes(&bytes, &ValidationContext::for_room(room))
        .expect("stateless validation does not apply the admin gate");
    engine
        .store_mut()
        .insert(&validated)
        .expect("direct insert");
    deliver_chunks(
        &mut engine,
        NODE_B,
        room,
        vec![history_chunk(
            &room,
            0,
            &planted,
            validated.event_id,
            sk(2).device_key(),
        )],
    );
    assert!(
        !engine.has_room_key(0),
        "a non-admin source event must not bind a chunk"
    );
    assert!(
        engine
            .logs()
            .iter()
            .any(|line| line.starts_with("key_history_unbound ")),
        "the unbound chunk is logged, got {:?}",
        engine.logs()
    );
}

/// After reopen, bound chunks for both commitments and a new admin
/// distribution must not install a key. The early return in
/// `adopt_key_distribution` is what keeps the epoch poisoned.
#[test]
fn key_adoption_reopen_chunks_and_distribution_stay_poisoned() {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let key_one = RoomKey::from_bytes([0x11; 32]);
    let key_two = RoomKey::from_bytes([0x22; 32]);
    let first = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_one,
        &[sk(2).device_key()],
        &[genesis_id],
        T0 + 1,
    )
    .expect("D1");
    let first_bytes = first.to_bytes();
    let first_id = frame_id(&first_bytes, room);
    engine.publish(&first_bytes).expect("publish D1");
    let second = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &key_two,
        &[sk(4).device_key()],
        &[first_id],
        T0 + 2,
    )
    .expect("D2");
    let second_bytes = second.to_bytes();
    let second_id = frame_id(&second_bytes, room);
    engine.publish(&second_bytes).expect("publish D2");

    let store = engine.into_store();
    let mut engine = SyncEngine::open_with_local_device(
        store,
        room,
        adoption_config(true),
        Some(ADMIN_DEV_SEED),
    )
    .expect("reopen");
    assert!(engine.is_room_key_poisoned(2));
    let local = sk(2).device_key();
    // One chunk at a time. A same-commitment chunk is the offer that
    // `resolve` would install if the poisoned-epoch return were skipped,
    // and the other chunk must not be able to hide that by re-poisoning
    // afterwards.
    for (key, id) in [(&key_one, first_id), (&key_two, second_id)] {
        deliver_chunks(
            &mut engine,
            NODE_B,
            room,
            vec![history_chunk(&room, 2, key, id, local)],
        );
        assert!(
            engine.is_room_key_poisoned(2),
            "a bound chunk must not clear the poison"
        );
        assert!(
            engine.room_key(2).is_none(),
            "a bound chunk must not install a key"
        );
    }
    let third_key = RoomKey::from_bytes([0x33; 32]);
    let third = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        &third_key,
        &[local],
        &[second_id],
        T0 + 3,
    )
    .expect("new distribution");
    engine.publish(&third.to_bytes()).expect("publish third");
    assert!(
        engine.is_room_key_poisoned(2),
        "chunks and a later distribution must not clear the poison"
    );
    assert!(engine.room_key(2).is_none());
    assert!(
        !served_epochs(&mut engine, room).contains(&2),
        "no key is served for the poisoned epoch"
    );
}

fn publish_pair(wrap_first: bool) -> (SyncEngine, RoomId, Vec<KeyHistoryChunk>) {
    let (mut engine, room, genesis_id) = seeded_engine_with_admin_seed(adoption_config(true));
    let key_wrap = RoomKey::from_bytes([0x11; 32]);
    let key_other = RoomKey::from_bytes([0x22; 32]);
    let local = sk(2).device_key();
    let other_dev = sk(4).device_key();
    let (first_key, first_for, second_key, second_for) = if wrap_first {
        (&key_wrap, local, &key_other, other_dev)
    } else {
        (&key_other, other_dev, &key_wrap, local)
    };
    let first = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        first_key,
        &[first_for],
        &[genesis_id],
        T0 + 1,
    )
    .expect("first distribution");
    let first_bytes = first.to_bytes();
    let first_id = frame_id(&first_bytes, room);
    engine.publish(&first_bytes).expect("publish first");
    let second = crate::event::distribution::build_member_key_distribution(
        &sk(1),
        &sk(2),
        &room,
        2,
        second_key,
        &[second_for],
        &[first_id],
        T0 + 2,
    )
    .expect("second distribution");
    let second_bytes = second.to_bytes();
    let second_id = frame_id(&second_bytes, room);
    engine.publish(&second_bytes).expect("publish second");
    let chunks = vec![
        history_chunk(&room, 2, first_key, first_id, local),
        history_chunk(&room, 2, second_key, second_id, local),
    ];
    (engine, room, chunks)
}

/// Live conflicts stay poisoned in both arrival orders, including after chunks.
#[test]
fn key_adoption_live_conflict_both_orders_stay_poisoned() {
    for wrap_first in [true, false] {
        let (mut engine, room, chunks) = publish_pair(wrap_first);
        assert!(
            engine.is_room_key_poisoned(2),
            "wrap_first={wrap_first}: conflict poisons before any chunk"
        );
        assert!(
            engine.room_key(2).is_none(),
            "wrap_first={wrap_first}: conflict does not install a key"
        );
        for chunk in chunks {
            deliver_chunks(&mut engine, NODE_B, room, vec![chunk]);
            assert!(
                engine.is_room_key_poisoned(2),
                "wrap_first={wrap_first}: a chunk must not clear the poison"
            );
            assert!(
                engine.room_key(2).is_none(),
                "wrap_first={wrap_first}: a chunk must not install a key"
            );
        }
        assert!(
            !served_epochs(&mut engine, room).contains(&2),
            "wrap_first={wrap_first}: no key is served"
        );
    }
}

/// A-5. No production call site in the CLI, net, or façade crates sets the flag.
#[test]
fn key_adoption_a5_shipped_crates_do_not_set_the_flag() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    for rel in [
        "../iroh-rooms-cli/src",
        "../iroh-rooms-net/src",
        "../iroh-rooms/src",
    ] {
        let dir = root.join(rel);
        assert!(dir.is_dir(), "missing {}", dir.display());
        collect_flag_sets(&dir, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "no non-test CLI, net, or façade source may set experimental_room_key_adoption. Found:\n{}",
        hits.join("\n")
    );
}

fn is_cfg_test_attr(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("#[") && trimmed.contains("cfg(test)")
}

#[test]
fn cfg_not_test_stays_in_the_production_scan() {
    assert!(!is_cfg_test_attr("#[cfg(not(test))]"));
    assert!(is_cfg_test_attr("#[cfg(test)]"));
    assert!(is_cfg_test_attr("    #[cfg(test)]"));
}

fn collect_flag_sets(dir: &std::path::Path, hits: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|_| panic!("read {}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_flag_sets(&path, hits);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
        let mut depth: usize = 0;
        let mut skip_until: Option<usize> = None;
        let mut cfg_test_pending = false;
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // `cfg(test)` exactly. `cfg(not(test))` contains "test" and must
            // stay in the production scan.
            if skip_until.is_none() && is_cfg_test_attr(trimmed) {
                cfg_test_pending = true;
            }
            if skip_until.is_none() && cfg_test_pending && trimmed.contains('{') {
                skip_until = Some(depth);
                cfg_test_pending = false;
            }
            let in_test = skip_until.is_some() || cfg_test_pending;
            if cfg_test_pending && trimmed.ends_with(';') && !trimmed.contains('{') {
                cfg_test_pending = false;
            }
            let code = trimmed.split("//").next().unwrap_or(trimmed);
            if !in_test && code.contains("experimental_room_key_adoption") {
                hits.push(format!("{}:{}: {}", path.display(), idx + 1, trimmed));
            }
            depth = depth.saturating_add(line.matches('{').count());
            depth = depth.saturating_sub(line.matches('}').count());
            if let Some(stop) = skip_until {
                if depth <= stop {
                    skip_until = None;
                }
            }
        }
    }
}
