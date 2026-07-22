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
use crate::event::ids::{EventId, HashRef, RoomId};
use crate::event::keys::{IdentityKey, SigningKey};
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::{validate_wire_bytes, ValidationContext};
use crate::event::wire::WireEvent;
use crate::membership::{Role, Status};
use crate::store::EventStore;

use super::engine::{Completeness, Severity, SyncEngine};
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
