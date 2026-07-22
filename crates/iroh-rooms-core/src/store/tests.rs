//! Focused unit tests for the `SQLite` event store, mapped to the issue Acceptance
//! Criteria (the broader integration suite lives in `tests/` per the test phase).

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, ErrorCode, TransactionBehavior};

use super::{EventStore, InsertOutcome};
use crate::event::binding::DeviceBinding;
use crate::event::content::{Content, EventType, MemberInvited, MessageText, RoomCreated};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::SigningKey;
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use crate::event::wire::WireEvent;

const NONCE: [u8; 16] = [0xaa; 16];
const T0: u64 = 1_750_000_000_000;

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

fn ctx(room: RoomId) -> ValidationContext {
    ValidationContext::for_room(room)
}

fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

fn genesis(id: &SigningKey, dev: &SigningKey) -> (ValidatedEvent, RoomId) {
    let id_key = id.identity_key();
    let dev_key = dev.device_key();
    let room = signed::derive_room_id(&id_key, &NONCE, T0);
    let binding = DeviceBinding::create(&room, id, dev_key);
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
    let v = validate_wire_bytes(&seal(&ev, dev), &ctx(room)).expect("genesis valid");
    (v, room)
}

fn message(
    id: &SigningKey,
    dev: &SigningKey,
    room: RoomId,
    prev: Vec<EventId>,
    body: &str,
    t: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: id.identity_key(),
        device_id: dev.device_key(),
        event_type: EventType::MessageText,
        created_at: t,
        prev_events: prev,
        content: Content::MessageText(MessageText {
            body: body.to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    validate_wire_bytes(&seal(&ev, dev), &ctx(room)).expect("message valid")
}

/// Admin-authored event (here `member.invited`) citing `prev`.
fn invite(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    invitee: &SigningKey,
    room: RoomId,
    prev: Vec<EventId>,
    t: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin_id.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: prev,
        content: Content::MemberInvited(MemberInvited {
            invite_id: [0x01; 16],
            capability_hash: [0x02; 32],
            role: "member".to_owned(),
            invitee_key: invitee.identity_key(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    validate_wire_bytes(&seal(&ev, admin_dev), &ctx(room)).expect("invite valid")
}

// AC1 — valid event persists exactly once, verbatim.
#[test]
fn insert_persists_once_verbatim() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let mut store = EventStore::open_in_memory().unwrap();

    assert_eq!(store.insert(&g).unwrap(), InsertOutcome::Inserted);
    assert_eq!(store.count(&room).unwrap(), 1);
    assert!(store.contains(&g.event_id).unwrap());

    let got = store.get(&g.event_id).unwrap().expect("present");
    assert_eq!(got.wire.to_bytes(), g.wire.to_bytes(), "verbatim bytes");
    assert_eq!(got.event_id, g.event_id);
    assert_eq!(got.lamport, Some(0), "genesis lamport");
    assert_eq!(got.admin_seq, Some(0), "genesis admin_seq");
}

#[test]
fn room_scoped_point_reads_never_match_a_foreign_room() {
    let (alice_id, alice_dev) = (sk(1), sk(2));
    let (bob_id, bob_dev) = (sk(3), sk(4));
    let (alice_genesis, alice_room) = genesis(&alice_id, &alice_dev);
    let (bob_genesis, bob_room) = genesis(&bob_id, &bob_dev);
    let mut store = EventStore::open_in_memory().unwrap();
    store.insert(&alice_genesis).unwrap();
    store.insert(&bob_genesis).unwrap();

    assert!(store
        .contains_in_room(&alice_room, &alice_genesis.event_id)
        .unwrap());
    assert!(store
        .get_in_room(&alice_room, &alice_genesis.event_id)
        .unwrap()
        .is_some());
    assert!(!store
        .contains_in_room(&alice_room, &bob_genesis.event_id)
        .unwrap());
    assert!(store
        .get_in_room(&alice_room, &bob_genesis.event_id)
        .unwrap()
        .is_none());
    assert!(store
        .get_in_room(&bob_room, &bob_genesis.event_id)
        .unwrap()
        .is_some());
}

// AC2 — duplicate insert is ignored without error; 1x == 1000x.
#[test]
fn duplicate_insert_is_idempotent() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let mut store = EventStore::open_in_memory().unwrap();

    assert_eq!(store.insert(&g).unwrap(), InsertOutcome::Inserted);
    let bytes = store.get(&g.event_id).unwrap().unwrap().wire.to_bytes();
    for _ in 0..1000 {
        assert_eq!(store.insert(&g).unwrap(), InsertOutcome::Duplicate);
    }
    assert_eq!(store.count(&room).unwrap(), 1);
    assert_eq!(
        store.get(&g.event_id).unwrap().unwrap().wire.to_bytes(),
        bytes
    );
}

// AC3 — parent lookup both directions, tolerating dangling refs (out-of-order).
#[test]
fn parent_lookup_tolerates_out_of_order() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m = message(&id, &dev, room, vec![g.event_id], "hi", T0 + 1);
    let mut store = EventStore::open_in_memory().unwrap();

    // Insert the child BEFORE its parent: edge dangles, lamport stays NULL.
    assert_eq!(store.insert(&m).unwrap(), InsertOutcome::Inserted);
    assert_eq!(store.parents_of(&m.event_id).unwrap(), vec![g.event_id]);
    assert_eq!(
        store.missing_parents(&m.event_id).unwrap(),
        vec![g.event_id]
    );
    assert_eq!(store.children_of(&g.event_id).unwrap(), vec![m.event_id]);
    assert_eq!(store.get(&m.event_id).unwrap().unwrap().lamport, None);

    // Parent arrives: dangling edge resolves, lamport propagates.
    assert_eq!(store.insert(&g).unwrap(), InsertOutcome::Inserted);
    assert!(store.missing_parents(&m.event_id).unwrap().is_empty());
    assert_eq!(store.get(&g.event_id).unwrap().unwrap().lamport, Some(0));
    assert_eq!(store.get(&m.event_id).unwrap().unwrap().lamport, Some(1));
}

// AC3 — room tail in canonical order; NULL-lamport events excluded.
#[test]
fn room_tail_is_canonical_and_excludes_incomplete() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m1 = message(&id, &dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&id, &dev, room, vec![m1.event_id], "two", T0 + 2);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), m1.clone(), m2.clone()])
        .unwrap();

    let tail: Vec<EventId> = store
        .room_tail(&room, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(tail, vec![g.event_id, m1.event_id, m2.event_id]);

    // An orphan (missing parent) has NULL lamport and is excluded from the tail.
    let orphan = message(
        &id,
        &dev,
        room,
        vec![EventId::from_bytes([0x77; 32])],
        "x",
        T0 + 3,
    );
    store.insert(&orphan).unwrap();
    let tail_ids: Vec<EventId> = store
        .room_tail(&room, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert!(!tail_ids.contains(&orphan.event_id));
    assert_eq!(tail_ids.len(), 3);
}

// AC3 — membership-fold inputs and DAG heads.
#[test]
fn by_type_and_heads_track_the_dag() {
    let (admin, admin_dev) = (sk(1), sk(2));
    let bob = sk(0x10);
    let (g, room) = genesis(&admin, &admin_dev);
    let inv = invite(&admin, &admin_dev, &bob, room, vec![g.event_id], T0 + 1);
    let mut store = EventStore::open_in_memory().unwrap();
    store.insert_all(&[g.clone(), inv.clone()]).unwrap();

    let invites: Vec<EventId> = store
        .by_type(&room, EventType::MemberInvited)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(invites, vec![inv.event_id]);

    // The head is the latest event; admin_seq follows the admin self-parent chain.
    assert_eq!(store.heads(&room).unwrap(), vec![inv.event_id]);
    assert_eq!(
        store.get(&inv.event_id).unwrap().unwrap().admin_seq,
        Some(1)
    );
    assert_eq!(
        store.admin_chain_tip(&room).unwrap(),
        Some((inv.event_id, 1))
    );

    // A non-admin author's event has no admin_seq.
    let bob_msg = message(&bob, &sk(0x11), room, vec![inv.event_id], "hello", T0 + 2);
    store.insert(&bob_msg).unwrap();
    assert_eq!(
        store.get(&bob_msg.event_id).unwrap().unwrap().admin_seq,
        None
    );
    assert_eq!(store.heads(&room).unwrap(), vec![bob_msg.event_id]);
}

// AC4 — derived caches rebuild purely from the authoritative (event_id, wire).
#[test]
fn rebuild_recovers_derived_state_from_authoritative_rows() {
    let (admin, admin_dev) = (sk(1), sk(2));
    let bob = sk(0x10);
    let (g, room) = genesis(&admin, &admin_dev);
    let inv = invite(&admin, &admin_dev, &bob, room, vec![g.event_id], T0 + 1);
    let m = message(&bob, &sk(0x11), room, vec![inv.event_id], "hi", T0 + 2);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), inv.clone(), m.clone()])
        .unwrap();

    let before: Vec<_> = [&g, &inv, &m]
        .iter()
        .map(|e| store.get(&e.event_id).unwrap().unwrap())
        .collect();

    // Corrupt every derived value and drop all edges, leaving only the
    // authoritative (event_id, wire) columns intact.
    store
        .conn
        .execute("UPDATE events SET lamport = 999, admin_seq = 999", [])
        .unwrap();
    store.conn.execute("DELETE FROM event_parents", []).unwrap();

    store.rebuild().unwrap();

    let after: Vec<_> = [&g, &inv, &m]
        .iter()
        .map(|e| store.get(&e.event_id).unwrap().unwrap())
        .collect();
    assert_eq!(before, after, "rebuild reproduces the derived cache");

    // Edges and lamport chain are restored.
    assert_eq!(store.parents_of(&m.event_id).unwrap(), vec![inv.event_id]);
    assert_eq!(store.get(&g.event_id).unwrap().unwrap().lamport, Some(0));
    assert_eq!(store.get(&inv.event_id).unwrap().unwrap().lamport, Some(1));
    assert_eq!(store.get(&m.event_id).unwrap().unwrap().lamport, Some(2));
    assert_eq!(
        store.admin_chain_tip(&room).unwrap(),
        Some((inv.event_id, 1))
    );
}

// Order-independence: shuffled insertion then rebuild yields the same state.
#[test]
fn rebuild_is_order_independent() {
    let (admin, admin_dev) = (sk(1), sk(2));
    let (g, room) = genesis(&admin, &admin_dev);
    let m1 = message(&admin, &admin_dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&admin, &admin_dev, room, vec![m1.event_id], "two", T0 + 2);

    let mut in_order = EventStore::open_in_memory().unwrap();
    in_order
        .insert_all(&[g.clone(), m1.clone(), m2.clone()])
        .unwrap();

    let mut shuffled = EventStore::open_in_memory().unwrap();
    shuffled
        .insert_all(&[m2.clone(), g.clone(), m1.clone()])
        .unwrap();
    shuffled.rebuild().unwrap();

    for e in [&g, &m1, &m2] {
        assert_eq!(
            in_order.get(&e.event_id).unwrap(),
            shuffled.get(&e.event_id).unwrap(),
        );
    }
}

// Test plan: query by sender — by_sender filters strictly to one identity.
#[test]
fn by_sender_filters_correctly() {
    let (admin, admin_dev) = (sk(1), sk(2));
    let bob = sk(0x10);
    let bob_dev = sk(0x11);
    let (g, room) = genesis(&admin, &admin_dev);
    let inv = invite(&admin, &admin_dev, &bob, room, vec![g.event_id], T0 + 1);
    let bob_msg = message(&bob, &bob_dev, room, vec![inv.event_id], "hi", T0 + 2);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), inv.clone(), bob_msg.clone()])
        .unwrap();

    let admin_events: Vec<EventId> = store
        .by_sender(&room, &admin.identity_key())
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert!(
        admin_events.contains(&g.event_id),
        "genesis missing from admin events"
    );
    assert!(
        admin_events.contains(&inv.event_id),
        "invite missing from admin events"
    );
    assert_eq!(admin_events.len(), 2, "admin events: genesis + invite");

    let bob_events: Vec<EventId> = store
        .by_sender(&room, &bob.identity_key())
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(
        bob_events,
        vec![bob_msg.event_id],
        "bob events: just his message"
    );
}

// insert_all returns accurate inserted/duplicate counts.
#[test]
fn insert_all_stats_counts_are_accurate() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m = message(&id, &dev, room, vec![g.event_id], "hi", T0 + 1);
    let mut store = EventStore::open_in_memory().unwrap();

    let stats = store.insert_all(&[g.clone(), m.clone()]).unwrap();
    assert_eq!(stats.inserted, 2, "first batch: two new events");
    assert_eq!(stats.duplicate, 0);

    let stats = store.insert_all(&[g.clone(), m.clone()]).unwrap();
    assert_eq!(stats.inserted, 0, "second batch: all duplicates");
    assert_eq!(stats.duplicate, 2);

    let m2 = message(&id, &dev, room, vec![m.event_id], "bye", T0 + 2);
    let stats = store.insert_all(&[g.clone(), m2.clone()]).unwrap();
    assert_eq!(stats.inserted, 1, "mixed: one new");
    assert_eq!(stats.duplicate, 1, "mixed: one dup");
}

// Security regression: insert must reject a mismatched (event_id, wire) pair.
#[test]
fn insert_rejects_mismatched_event_id() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let mut store = EventStore::open_in_memory().unwrap();

    let mut tampered = g.clone();
    tampered.event_id = EventId::from_bytes([0xff; 32]);
    let err = store.insert(&tampered).unwrap_err();
    assert!(
        matches!(err, super::StoreError::Integrity(_)),
        "expected Integrity error, got {err:?}"
    );
    // The rejected call must not have persisted anything.
    assert_eq!(store.count(&room).unwrap(), 0);
}

// Schema migration guard: a future user_version returns StoreError::Migration.
#[test]
fn migration_rejects_future_schema_version() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", super::schema::USER_VERSION + 1)
        .unwrap();
    let err = super::schema::migrate(&conn).unwrap_err();
    assert!(
        matches!(err, super::StoreError::Migration(_)),
        "expected Migration error, got {err:?}"
    );
}

// All point/existence queries on an empty store return empty / None / false.
#[test]
fn empty_store_queries_return_empty() {
    let store = EventStore::open_in_memory().unwrap();
    let unknown_room = RoomId::from_bytes([0xbb; 32]);
    let unknown_id = EventId::from_bytes([0xcc; 32]);
    let any_sender = sk(0x42);

    assert!(!store.contains(&unknown_id).unwrap());
    assert!(store.get(&unknown_id).unwrap().is_none());
    assert_eq!(store.count(&unknown_room).unwrap(), 0);
    assert!(store.heads(&unknown_room).unwrap().is_empty());
    assert!(store.admin_chain_tip(&unknown_room).unwrap().is_none());
    assert!(store.parents_of(&unknown_id).unwrap().is_empty());
    assert!(store.children_of(&unknown_id).unwrap().is_empty());
    assert!(store.missing_parents(&unknown_id).unwrap().is_empty());
    assert!(store.room_tail(&unknown_room, 10).unwrap().is_empty());
    assert!(store
        .by_type(&unknown_room, EventType::MessageText)
        .unwrap()
        .is_empty());
    assert!(store
        .by_sender(&unknown_room, &any_sender.identity_key())
        .unwrap()
        .is_empty());
}

// Diamond merge: lamport = max(parent lamports) + 1.
#[test]
fn diamond_merge_lamport_is_max_parent_plus_one() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let a = message(&id, &dev, room, vec![g.event_id], "left", T0 + 1);
    let b = message(&id, &dev, room, vec![g.event_id], "right", T0 + 2);
    let merge = message(
        &id,
        &dev,
        room,
        vec![a.event_id, b.event_id],
        "merge",
        T0 + 3,
    );
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), a.clone(), b.clone(), merge.clone()])
        .unwrap();

    assert_eq!(store.get(&g.event_id).unwrap().unwrap().lamport, Some(0));
    assert_eq!(store.get(&a.event_id).unwrap().unwrap().lamport, Some(1));
    assert_eq!(store.get(&b.event_id).unwrap().unwrap().lamport, Some(1));
    // max(1, 1) + 1 = 2
    assert_eq!(
        store.get(&merge.event_id).unwrap().unwrap().lamport,
        Some(2)
    );
}

// No panic on corrupt stored bytes: rebuild surfaces a typed error.
#[test]
fn rebuild_on_corrupt_bytes_errors_without_panicking() {
    let (id, dev) = (sk(1), sk(2));
    let (g, _room) = genesis(&id, &dev);
    let mut store = EventStore::open_in_memory().unwrap();
    store.insert(&g).unwrap();

    // Truncate the authoritative wire bytes.
    store
        .conn
        .execute(
            "UPDATE events SET wire = ?1 WHERE event_id = ?2",
            params![vec![0x00_u8, 0x01], &g.event_id.as_bytes()[..]],
        )
        .unwrap();

    let err = store.rebuild().unwrap_err();
    assert!(
        matches!(
            err,
            super::StoreError::Decode(_) | super::StoreError::Integrity(_)
        ),
        "expected typed decode/integrity error, got {err:?}"
    );
}

// room_tail limit parameter truncates to the N most-recent events.
#[test]
fn room_tail_limit_is_respected() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m1 = message(&id, &dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&id, &dev, room, vec![m1.event_id], "two", T0 + 2);
    let m3 = message(&id, &dev, room, vec![m2.event_id], "three", T0 + 3);
    let m4 = message(&id, &dev, room, vec![m3.event_id], "four", T0 + 4);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), m1.clone(), m2.clone(), m3.clone(), m4.clone()])
        .unwrap();

    // limit=0 returns nothing.
    assert!(store.room_tail(&room, 0).unwrap().is_empty());

    // limit=2 returns the two most-recent events in ascending canonical order.
    let tail: Vec<_> = store
        .room_tail(&room, 2)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0], m3.event_id, "second-most-recent first");
    assert_eq!(tail[1], m4.event_id, "most-recent last");

    // limit >= total returns all 5.
    assert_eq!(store.room_tail(&room, 100).unwrap().len(), 5);
}

// Events in different rooms never bleed into each other's query results.
#[test]
fn multi_room_queries_are_isolated() {
    let (admin1, dev1) = (sk(1), sk(2));
    let (admin2, dev2) = (sk(3), sk(4));
    let (g1, room1) = genesis(&admin1, &dev1);
    let (g2, room2) = genesis(&admin2, &dev2);
    let m1 = message(&admin1, &dev1, room1, vec![g1.event_id], "r1", T0 + 1);
    let m2 = message(&admin2, &dev2, room2, vec![g2.event_id], "r2", T0 + 1);

    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g1.clone(), m1.clone(), g2.clone(), m2.clone()])
        .unwrap();

    // count isolates by room.
    assert_eq!(store.count(&room1).unwrap(), 2);
    assert_eq!(store.count(&room2).unwrap(), 2);

    // room_tail does not include the other room's events.
    let tail1: Vec<_> = store
        .room_tail(&room1, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert!(tail1.contains(&g1.event_id) && tail1.contains(&m1.event_id));
    assert!(!tail1.contains(&g2.event_id) && !tail1.contains(&m2.event_id));

    // by_type is room-scoped.
    let created1 = store.by_type(&room1, EventType::RoomCreated).unwrap();
    assert_eq!(created1.len(), 1);
    assert_eq!(created1[0].event_id, g1.event_id);

    // heads are room-scoped.
    assert_eq!(store.heads(&room1).unwrap(), vec![m1.event_id]);
    assert_eq!(store.heads(&room2).unwrap(), vec![m2.event_id]);

    // admin_chain_tip is room-scoped; each room's tip comes from its own chain.
    let tip1 = store.admin_chain_tip(&room1).unwrap();
    let tip2 = store.admin_chain_tip(&room2).unwrap();
    assert!(tip1.is_some() && tip2.is_some());
    assert_ne!(tip1.unwrap().0, tip2.unwrap().0);
}

// insert_all rolls back the whole batch when any event fails the integrity check.
#[test]
fn insert_all_rolls_back_entire_batch_on_error() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m = message(&id, &dev, room, vec![g.event_id], "hi", T0 + 1);

    let mut tampered = m.clone();
    tampered.event_id = EventId::from_bytes([0xff; 32]);
    let mut store = EventStore::open_in_memory().unwrap();
    // Valid genesis followed by an event with a mismatched event_id.
    let err = store.insert_all(&[g.clone(), tampered]).unwrap_err();
    assert!(
        matches!(err, super::StoreError::Integrity(_)),
        "expected Integrity error, got {err:?}"
    );
    // Transaction must have rolled back; nothing should be stored.
    assert_eq!(store.count(&room).unwrap(), 0);
    assert!(!store.contains(&g.event_id).unwrap());
}

// Issue #143 — `insert_all_outcomes` returns the per-input outcome in input
// order, the contract the sync engine needs to apply per-event post-commit side
// effects after a batched commit.
#[test]
fn insert_all_outcomes_returns_per_input_outcomes_in_order() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m = message(&id, &dev, room, vec![g.event_id], "hi", T0 + 1);

    let mut store = EventStore::open_in_memory().unwrap();
    // Seed the genesis so the second batch's first event is a Duplicate.
    store.insert(&g).unwrap();

    let m2 = message(&id, &dev, room, vec![m.event_id], "bye", T0 + 2);
    let outcomes = store.insert_all_outcomes(&[g.clone(), m2.clone()]).unwrap();
    assert_eq!(
        outcomes,
        vec![InsertOutcome::Duplicate, InsertOutcome::Inserted],
        "ordered outcomes: g was already stored (Duplicate), m2 is new (Inserted)"
    );
    assert_eq!(store.count(&room).unwrap(), 2);
}

// Issue #143 — `insert_all_outcomes` opens exactly one write transaction per
// call (the test-only `write_tx_count` is the batching acceptance oracle for
// the engine).
#[test]
fn insert_all_outcomes_uses_one_transaction_per_batch() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m1 = message(&id, &dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&id, &dev, room, vec![m1.event_id], "two", T0 + 2);
    let m3 = message(&id, &dev, room, vec![m2.event_id], "three", T0 + 3);

    let mut store = EventStore::open_in_memory().unwrap();
    store.reset_write_tx_count();
    let _ = store
        .insert_all_outcomes(&[g.clone(), m1.clone(), m2.clone(), m3.clone()])
        .unwrap();
    assert_eq!(
        store.write_tx_count(),
        1,
        "one batch → one BEGIN IMMEDIATE transaction"
    );
}

// Issue #143 — `insert_all` still returns accurate stats after the refactor to
// delegate to `insert_all_outcomes` (the API the engine used to call directly).
#[test]
fn insert_all_delegates_to_insert_all_outcomes() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m = message(&id, &dev, room, vec![g.event_id], "hi", T0 + 1);
    let mut store = EventStore::open_in_memory().unwrap();

    let stats = store.insert_all(&[g.clone(), m.clone()]).unwrap();
    assert_eq!(stats.inserted, 2);
    assert_eq!(stats.duplicate, 0);

    let stats = store.insert_all(&[g, m]).unwrap();
    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.duplicate, 2);
}

// Before a merge event arrives, concurrent forks each appear as a DAG head.
#[test]
fn heads_returns_multiple_concurrent_forks() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let a = message(&id, &dev, room, vec![g.event_id], "left", T0 + 1);
    let b = message(&id, &dev, room, vec![g.event_id], "right", T0 + 2);

    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), a.clone(), b.clone()])
        .unwrap();

    // Both fork tips are heads; genesis is not (it has children).
    let mut heads = store.heads(&room).unwrap();
    heads.sort_unstable();
    let mut expected = vec![a.event_id, b.event_id];
    expected.sort_unstable();
    assert_eq!(heads, expected, "both forks are heads before merge");

    // After inserting the merge event, only it is a head.
    let merge = message(
        &id,
        &dev,
        room,
        vec![a.event_id, b.event_id],
        "merge",
        T0 + 3,
    );
    store.insert(&merge).unwrap();
    assert_eq!(store.heads(&room).unwrap(), vec![merge.event_id]);
}

// by_type filters strictly to the requested type and excludes all others.
#[test]
fn by_type_excludes_other_event_types() {
    let (admin, admin_dev) = (sk(1), sk(2));
    let bob = sk(0x10);
    let (g, room) = genesis(&admin, &admin_dev);
    let inv = invite(&admin, &admin_dev, &bob, room, vec![g.event_id], T0 + 1);
    let msg = message(&admin, &admin_dev, room, vec![inv.event_id], "hi", T0 + 2);

    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), inv.clone(), msg.clone()])
        .unwrap();

    let texts: Vec<_> = store
        .by_type(&room, EventType::MessageText)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(
        texts,
        vec![msg.event_id],
        "by_type(MessageText) returns only messages"
    );

    let invites: Vec<_> = store
        .by_type(&room, EventType::MemberInvited)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(
        invites,
        vec![inv.event_id],
        "by_type(MemberInvited) returns only invites"
    );

    let created: Vec<_> = store
        .by_type(&room, EventType::RoomCreated)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(
        created,
        vec![g.event_id],
        "by_type(RoomCreated) returns only genesis"
    );

    // A type with no events in the room returns empty.
    assert!(store
        .by_type(&room, EventType::MemberLeft)
        .unwrap()
        .is_empty());
}

// ── room_ids() (spec IR-0108 §4.2 / §5.1) ────────────────────────────────────

// room_ids() returns an empty vec for an empty store (spec §4.2 substrate).
#[test]
fn room_ids_empty_store_returns_empty() {
    let store = EventStore::open_in_memory().unwrap();
    assert!(
        store.room_ids().unwrap().is_empty(),
        "room_ids() on an empty store must return an empty vec"
    );
}

// room_ids() returns the single room when exactly one room is present.
#[test]
fn room_ids_single_room_returns_that_room() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let mut store = EventStore::open_in_memory().unwrap();
    store.insert(&g).unwrap();

    let ids = store.room_ids().unwrap();
    assert_eq!(ids, vec![room]);
}

// room_ids() returns all three rooms, ascending, de-duplicated — even with
// multiple events in one room (spec §4.2 DISTINCT / ORDER BY guarantee).
#[test]
fn room_ids_three_rooms_ascending_deduplicated() {
    let (id_a, dev_a) = (sk(0x31), sk(0x32));
    let (id_b, dev_b) = (sk(0x33), sk(0x34));
    let (id_c, dev_c) = (sk(0x35), sk(0x36));
    let (g_a, room_a) = genesis(&id_a, &dev_a);
    let (g_b, room_b) = genesis(&id_b, &dev_b);
    let (g_c, room_c) = genesis(&id_c, &dev_c);
    // A second event in room_a — room_ids() must still return room_a only once.
    let m_a = message(&id_a, &dev_a, room_a, vec![g_a.event_id], "extra", T0 + 1);

    let mut store = EventStore::open_in_memory().unwrap();
    // Insert interleaved to stress de-duplication and ordering.
    store.insert(&g_b).unwrap();
    store.insert(&g_a).unwrap();
    store.insert(&m_a).unwrap();
    store.insert(&g_c).unwrap();

    let ids = store.room_ids().unwrap();
    assert_eq!(ids.len(), 3, "expected exactly 3 distinct rooms");

    let mut expected = vec![room_a, room_b, room_c];
    expected.sort_unstable();
    let mut actual = ids;
    actual.sort_unstable();
    assert_eq!(actual, expected, "all three rooms must be present");
}

// ── schema v2 sync-cache round-trips (IR-0201) ───────────────────────────────

// The v2 migration stamps user_version = 2 and creates the five sync-cache tables.
#[test]
fn migration_v2_stamps_version_and_creates_sync_cache_tables() {
    let store = EventStore::open_in_memory().unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2, "store must migrate to user_version 2");
    for tbl in [
        "sync_state",
        "sync_backfill_tokens",
        "sync_parked",
        "sync_parked_missing",
        "trust_decisions",
    ] {
        let found: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![tbl],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "v2 table {tbl} must exist after migration");
    }
}

// sync_state round-trips the unconfirmed suspicion (Some/None) idempotently.
#[test]
fn sync_state_round_trips_suspect_tip() {
    use super::SyncStateRow;
    let room = RoomId::from_bytes([0x21; 32]);
    let mut store = EventStore::open_in_memory().unwrap();

    // No row yet.
    assert!(store.load_sync_state(&room).unwrap().is_none());

    // Persist a suspicion; read it back exactly.
    let tip = EventId::from_bytes([0x42; 32]);
    let row = SyncStateRow {
        chat_cursor: None,
        suspect_tip: Some((tip, 7, 5)),
    };
    store.save_sync_state(&room, &row).unwrap();
    assert_eq!(store.load_sync_state(&room).unwrap(), Some(row));

    // Upsert to cleared suspicion (idempotent single row).
    let cleared = SyncStateRow::default();
    store.save_sync_state(&room, &cleared).unwrap();
    assert_eq!(store.load_sync_state(&room).unwrap(), Some(cleared));
}

// backfill token buckets round-trip and are replaced wholesale on save.
#[test]
fn backfill_tokens_round_trip_and_replace() {
    use std::collections::BTreeMap;
    let room = RoomId::from_bytes([0x22; 32]);
    let alice = sk(0x51).identity_key();
    let bob = sk(0x52).identity_key();
    let mut store = EventStore::open_in_memory().unwrap();

    assert!(store.load_backfill_tokens(&room).unwrap().is_empty());

    let mut tokens = BTreeMap::new();
    tokens.insert(alice, 3);
    tokens.insert(bob, 0);
    store.save_backfill_tokens(&room, &tokens).unwrap();
    assert_eq!(store.load_backfill_tokens(&room).unwrap(), tokens);

    // A subsequent save replaces the whole set (depleted budget persists).
    let mut fewer = BTreeMap::new();
    fewer.insert(alice, 1);
    store.save_backfill_tokens(&room, &fewer).unwrap();
    assert_eq!(store.load_backfill_tokens(&room).unwrap(), fewer);
}

// parked frames round-trip (row + missing edges) and delete cascades the edges.
#[test]
fn parked_round_trip_upsert_load_delete_cascades() {
    use super::ParkedRow;
    let room = RoomId::from_bytes([0x23; 32]);
    let child = EventId::from_bytes([0x61; 32]);
    let parent = EventId::from_bytes([0x62; 32]);
    let author = sk(0x53).identity_key();
    let mut store = EventStore::open_in_memory().unwrap();

    let row = ParkedRow {
        event_id: child,
        wire: vec![0xde, 0xad, 0xbe, 0xef],
        author,
        park_seq: 9,
        depth: 2,
        missing: vec![parent],
    };
    store.upsert_parked(&room, &row).unwrap();
    assert_eq!(store.load_parked(&room).unwrap(), vec![row.clone()]);

    // Idempotent upsert (checkpoint replay is a no-op).
    store.upsert_parked(&room, &row).unwrap();
    assert_eq!(store.load_parked(&room).unwrap().len(), 1);

    // Delete removes the frame and cascades its missing edges.
    store.delete_parked(&room, &child).unwrap();
    assert!(store.load_parked(&room).unwrap().is_empty());
    let orphan_edges: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sync_parked_missing WHERE room_id = ?1",
            params![&room.as_bytes()[..]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphan_edges, 0, "missing edges must cascade on delete");
}

// trust decisions are append-only with monotone per-room seq and CBOR event_ids.
#[test]
fn trust_decisions_append_only_monotone_seq() {
    use super::TrustRow;
    let room = RoomId::from_bytes([0x24; 32]);
    let a = EventId::from_bytes([0x71; 32]);
    let b = EventId::from_bytes([0x72; 32]);
    let mut store = EventStore::open_in_memory().unwrap();

    assert!(store.load_trust_decisions(&room).unwrap().is_empty());

    let first = TrustRow {
        seq: 0,
        code: "equivocation".to_owned(),
        severity: "critical".to_owned(),
        admin_seq: Some(4),
        event_ids: vec![a, b],
        created_at: 0,
    };
    let seq0 = store.append_trust_decision(&room, &first).unwrap();
    assert_eq!(seq0, 0);

    let second = TrustRow {
        code: "admin_view_suspect".to_owned(),
        severity: "warning".to_owned(),
        admin_seq: Some(5),
        event_ids: vec![a],
        ..first.clone()
    };
    let seq1 = store.append_trust_decision(&room, &second).unwrap();
    assert_eq!(seq1, 1, "seq is per-room monotone");

    let loaded = store.load_trust_decisions(&room).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].code, "equivocation");
    assert_eq!(loaded[0].event_ids, vec![a, b]);
    assert_eq!(loaded[1].code, "admin_view_suspect");
    assert_eq!(loaded[1].seq, 1);
}

// room_ids() returns the same result before and after rebuild (derived-state
// determinism; spec §4.2 "mirrors room_event_ids").
#[test]
fn room_ids_survives_rebuild_unchanged() {
    let (id_a, dev_a) = (sk(0x41), sk(0x42));
    let (id_b, dev_b) = (sk(0x43), sk(0x44));
    let (g_a, _room_a) = genesis(&id_a, &dev_a);
    let (g_b, _room_b) = genesis(&id_b, &dev_b);

    let mut store = EventStore::open_in_memory().unwrap();
    store.insert(&g_a).unwrap();
    store.insert(&g_b).unwrap();

    let before = store.room_ids().unwrap();
    store.rebuild().unwrap();
    let after = store.room_ids().unwrap();
    assert_eq!(
        before, after,
        "room_ids() must be identical before and after rebuild (restart determinism)"
    );
}

// v1→v2 migration is additive: existing `events` rows survive byte-for-byte;
// the five new v2 derived-cache tables are created empty (spec §D1/§D7).
#[test]
fn migration_v1_to_v2_is_additive_preserves_events() {
    use rusqlite::Connection;

    let (id, dev) = (sk(0x60), sk(0x61));
    let (genesis_ev, _room) = genesis(&id, &dev);
    let wire_bytes = genesis_ev.wire.to_bytes();

    // Simulate a v1 database: create only the v1 tables and insert one event.
    // No v2 tables exist yet; user_version = 1.
    let v1_ddl = "
        CREATE TABLE events (
            event_id    BLOB    NOT NULL PRIMARY KEY,
            wire        BLOB    NOT NULL,
            room_id     BLOB    NOT NULL,
            sender_id   BLOB    NOT NULL,
            device_id   BLOB    NOT NULL,
            event_type  TEXT    NOT NULL,
            created_at  INTEGER NOT NULL,
            lamport     INTEGER,
            admin_seq   INTEGER
        ) STRICT;
        CREATE TABLE event_parents (
            child_id    BLOB    NOT NULL,
            parent_id   BLOB    NOT NULL,
            ordinal     INTEGER NOT NULL,
            PRIMARY KEY (child_id, ordinal),
            FOREIGN KEY (child_id) REFERENCES events(event_id) ON DELETE CASCADE
        ) STRICT;
        CREATE INDEX idx_events_room_order ON events(room_id, lamport, event_id);
    ";
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(v1_ddl).unwrap();
    conn.execute(
        "INSERT INTO events(event_id, wire, room_id, sender_id, device_id,
                            event_type, created_at, lamport, admin_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            &genesis_ev.event_id.as_bytes()[..],
            &wire_bytes[..],
            &genesis_ev.event.room_id.as_bytes()[..],
            &genesis_ev.event.sender_id.as_bytes()[..],
            &genesis_ev.event.device_id.as_bytes()[..],
            genesis_ev.event.event_type.as_str(),
            i64::try_from(genesis_ev.event.created_at).unwrap(),
            0_i64,
            0_i64,
        ],
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 1_i64).unwrap();

    // Run the v1→v2 migration.
    super::schema::migrate(&conn).unwrap();

    // user_version must now be 2.
    let ver: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 2, "user_version must be 2 after migration");

    // The event must be present with byte-identical wire bytes.
    let wire_back: Vec<u8> = conn
        .query_row(
            "SELECT wire FROM events WHERE event_id = ?1",
            params![&genesis_ev.event_id.as_bytes()[..]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        wire_back, wire_bytes,
        "event wire bytes must survive migration byte-for-byte"
    );

    // The five v2 tables must exist and be empty (additive: no rows fabricated).
    for tbl in [
        "sync_state",
        "sync_backfill_tokens",
        "sync_parked",
        "sync_parked_missing",
        "trust_decisions",
    ] {
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![tbl],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            found, 1,
            "v2 table {tbl} must exist after migration from v1"
        );

        let row_count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {tbl}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            row_count, 0,
            "v2 table {tbl} must be empty (additive migration, no rows fabricated)"
        );
    }
}

// D8: room_event_ids is the set-equality oracle (spec `bounded-recent-sync-prototype.md` D8).
// The engine's digest() calls it to compare event sets after sync; this tests the method
// directly so a regression in the query is caught at the store layer, not only via SimNet.
#[test]
fn room_event_ids_returns_full_validated_set() {
    use std::collections::BTreeSet;
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m1 = message(&id, &dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&id, &dev, room, vec![m1.event_id], "two", T0 + 2);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), m1.clone(), m2.clone()])
        .unwrap();

    let ids = store.room_event_ids(&room).unwrap();
    assert_eq!(ids.len(), 3, "must contain all three stored events");
    assert!(ids.contains(&g.event_id), "genesis must be in the set");
    assert!(ids.contains(&m1.event_id), "m1 must be in the set");
    assert!(ids.contains(&m2.event_id), "m2 must be in the set");

    // The return type is BTreeSet for deterministic ordering (spec D8).
    let expected: BTreeSet<EventId> = [g.event_id, m1.event_id, m2.event_id].into_iter().collect();
    assert_eq!(ids, expected, "the set must equal exactly the inserted ids");

    // A room with no stored events returns an empty set.
    let other_room = RoomId::from_bytes([0x99; 32]);
    assert!(
        store.room_event_ids(&other_room).unwrap().is_empty(),
        "an unknown room must return an empty set"
    );
}

// Issue #113: recent_event_ids is the recent-lamport slab of the WantMembership
// ancestry claim — descending canonical order, causally-placed rows only.
#[test]
fn recent_event_ids_returns_placed_rows_newest_first() {
    let (id, dev) = (sk(1), sk(2));
    let (g, room) = genesis(&id, &dev);
    let m1 = message(&id, &dev, room, vec![g.event_id], "one", T0 + 1);
    let m2 = message(&id, &dev, room, vec![m1.event_id], "two", T0 + 2);
    // m3's parent m2 is withheld below: NULL lamport, must never be claimed.
    let m3 = message(&id, &dev, room, vec![m2.event_id], "three", T0 + 3);
    let mut store = EventStore::open_in_memory().unwrap();
    store
        .insert_all(&[g.clone(), m1.clone(), m3.clone()])
        .unwrap();

    let recent = store.recent_event_ids(&room, 10, 0).unwrap();
    assert_eq!(
        recent,
        vec![m1.event_id, g.event_id],
        "descending (lamport, event_id); the unplaced row is excluded"
    );

    // The limit truncates from the top (most recent first); the offset pages
    // downward (the claim's rotating window, #113).
    assert_eq!(
        store.recent_event_ids(&room, 1, 0).unwrap(),
        vec![m1.event_id]
    );
    assert_eq!(
        store.recent_event_ids(&room, 1, 1).unwrap(),
        vec![g.event_id]
    );
    assert!(
        store.recent_event_ids(&room, 1, 2).unwrap().is_empty(),
        "an offset past the placed set returns an empty page, no wrap"
    );

    // The unplaced row is invisible to the claim's head list too.
    assert_eq!(
        store.placed_heads(&room).unwrap(),
        vec![m1.event_id],
        "m3 heads the DAG but is unplaced; m1 is the placed frontier"
    );
    assert_eq!(store.placed_count(&room).unwrap(), 2);

    // Healing the hole places m3 (insert-time propagation) and it leads the slab.
    store.insert(&m2).unwrap();
    assert_eq!(
        store.recent_event_ids(&room, 2, 0).unwrap(),
        vec![m3.event_id, m2.event_id],
        "a healed descendant re-qualifies with its recomputed lamport"
    );
    assert_eq!(store.placed_heads(&room).unwrap(), vec![m3.event_id]);
    assert_eq!(store.placed_count(&room).unwrap(), 4);

    assert!(
        store
            .recent_event_ids(&RoomId::from_bytes([0x99; 32]), 4, 0)
            .unwrap()
            .is_empty(),
        "an unknown room must return an empty slab"
    );
}

// ── issue #85: busy_timeout + IMMEDIATE writes for concurrent writers ─────────
//
// Bantaba opens two `EventStore` connections onto one file (RPC writes + a sync
// pump). Under WAL exactly one writer is allowed at a time; the fix makes a
// colliding writer *wait* (default 5000ms busy_timeout) or, for read-then-write
// bodies, take the write lock up front (`BEGIN IMMEDIATE`) so no un-retryable
// `SQLITE_BUSY` reaches the caller. These map 1:1 to the spec's T1–T5.

/// Read `PRAGMA busy_timeout` (milliseconds) off a store's connection.
fn busy_timeout_ms(store: &EventStore) -> i64 {
    store
        .conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap()
}

// T1 (AC1) — two connections on one file-backed DB, interleaved concurrent
// `insert`s, must all return Ok (no `SQLITE_BUSY` escapes) and converge on the
// union of events. `insert` runs `propagate_from` (a heavy critical section), so
// N is kept small per spec §7 to avoid escalating busy-handler backoff.
#[test]
fn two_connections_interleaved_inserts_never_surface_busy() {
    const N: usize = 4;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite3");
    let (admin, admin_dev) = (sk(1), sk(2));
    let (g, room) = genesis(&admin, &admin_dev);

    // Seed the shared parent once so both writers cite an existing genesis.
    {
        let mut store = EventStore::open(&path).unwrap();
        store.insert(&g).unwrap();
    }

    let build = |tag: char, first_t: u64| -> Vec<ValidatedEvent> {
        (0..N)
            .map(|i| {
                let t = first_t + u64::try_from(i).unwrap();
                message(
                    &admin,
                    &admin_dev,
                    room,
                    vec![g.event_id],
                    &format!("{tag}{i}"),
                    t,
                )
            })
            .collect()
    };
    let events_a = build('a', T0 + 1);
    let events_b = build('b', T0 + 1_000);

    let expected: BTreeSet<EventId> = std::iter::once(g.event_id)
        .chain(events_a.iter().map(|e| e.event_id))
        .chain(events_b.iter().map(|e| e.event_id))
        .collect();

    let mut store_a = EventStore::open(&path).unwrap();
    let mut store_b = EventStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let ba = Arc::clone(&barrier);
    let ta = thread::spawn(move || {
        ba.wait();
        for ev in &events_a {
            store_a
                .insert(ev)
                .expect("connection A: no SQLITE_BUSY may reach the caller");
        }
    });
    let bb = Arc::clone(&barrier);
    let tb = thread::spawn(move || {
        bb.wait();
        for ev in &events_b {
            store_b
                .insert(ev)
                .expect("connection B: no SQLITE_BUSY may reach the caller");
        }
    });
    ta.join().unwrap();
    tb.join().unwrap();

    // A third connection reads back exactly the union of both writers' events.
    let verify = EventStore::open(&path).unwrap();
    assert_eq!(verify.room_event_ids(&room).unwrap(), expected);
    assert_eq!(
        verify.count(&room).unwrap(),
        1 + 2 * u64::try_from(N).unwrap()
    );
}

// T2 (AC1) — the read-then-write path (`append_trust_decision`: SELECT MAX(seq)+1
// → INSERT) under contention. This is the case a bare `busy_timeout` cannot
// rescue (a DEFERRED transaction loses the write-lock *upgrade* race with
// `SQLITE_BUSY_SNAPSHOT`, and the busy handler is not invoked) — only `BEGIN
// IMMEDIATE` fixes it. Both connections hammer the same room; afterwards the
// per-room `seq` values must be a gap-free 0..2N set, proving append-only
// monotonicity survived the race with no lost/duplicated seq.
#[test]
fn concurrent_read_then_write_appends_are_gap_free() {
    use super::TrustRow;

    // Cheap critical section, so a larger N is safe (spec §7).
    const N: usize = 100;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite3");
    let room = RoomId::from_bytes([0x24; 32]);

    // Create the schema before the racers open their connections.
    EventStore::open(&path).unwrap();

    let barrier = Arc::new(Barrier::new(2));

    let (pa, ba) = (path.clone(), Arc::clone(&barrier));
    let a = thread::spawn(move || {
        let mut store = EventStore::open(&pa).unwrap();
        let row = TrustRow {
            seq: 0, // ignored: append assigns the next per-room seq
            code: "equivocation".to_owned(),
            severity: "critical".to_owned(),
            admin_seq: None,
            event_ids: vec![],
            created_at: 0,
        };
        ba.wait();
        for _ in 0..N {
            store
                .append_trust_decision(&room, &row)
                .expect("connection A read-then-write append must not surface SQLITE_BUSY");
        }
    });
    let (pb, bb) = (path.clone(), Arc::clone(&barrier));
    let b = thread::spawn(move || {
        let mut store = EventStore::open(&pb).unwrap();
        let row = TrustRow {
            seq: 0,
            code: "admin_view_suspect".to_owned(),
            severity: "warning".to_owned(),
            admin_seq: None,
            event_ids: vec![],
            created_at: 0,
        };
        bb.wait();
        for _ in 0..N {
            store
                .append_trust_decision(&room, &row)
                .expect("connection B read-then-write append must not surface SQLITE_BUSY");
        }
    });
    a.join().unwrap();
    b.join().unwrap();

    let store = EventStore::open(&path).unwrap();
    let seqs: Vec<u64> = store
        .load_trust_decisions(&room)
        .unwrap()
        .into_iter()
        .map(|d| d.seq)
        .collect();
    let expected: Vec<u64> = (0..2 * u64::try_from(N).unwrap()).collect();
    assert_eq!(
        seqs, expected,
        "per-room seq must be gap-free and unique under concurrent writers"
    );
}

// T2 companion (AC1, deterministic) — a *direct*, race-free proof that
// `begin_write` opens a `BEGIN IMMEDIATE` transaction: the write lock is grabbed
// at BEGIN, before any statement runs. This is the load-bearing D1 fix (spec §1
// risk #1: "sets only the pragma and skips IMMEDIATE"). T2 exercises it under a
// thread race, which only *probabilistically* hits the upgrade deadlock; here we
// hold one connection's write transaction open (running no statement) and show a
// second, fail-fast connection cannot even BEGIN its own write. A `BEGIN
// DEFERRED` transaction would take no lock at BEGIN, so the second `begin_write`
// would succeed and the `unwrap_err` below would panic — i.e. this test fails
// loudly if `begin_write` is ever "simplified" back to `conn.transaction()`.
#[test]
fn begin_write_takes_the_write_lock_up_front() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite3");
    EventStore::open(&path).unwrap(); // create the schema + enable WAL

    let mut store_a = EventStore::open(&path).unwrap();
    // Fail-fast so a collision is observed immediately, not after the 5s default.
    let mut store_b =
        EventStore::open_with(&path, &super::StoreOptions { busy_timeout: None }).unwrap();

    // A opens a write transaction but runs NO statement. Under BEGIN IMMEDIATE the
    // single WAL write lock is already held at this point.
    let tx_a = store_a.begin_write().unwrap();

    // B therefore cannot even BEGIN its own write transaction — it collides on the
    // write lock at BEGIN (not at some later statement). With DEFERRED this call
    // would instead succeed, holding only a read snapshot.
    let err = store_b.begin_write().unwrap_err();
    match &err {
        super::StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => assert!(
            matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked),
            "expected a BUSY/LOCKED code at BEGIN, got {:?}",
            e.code
        ),
        other => panic!("expected a Sqlite busy failure at BEGIN, got {other:?}"),
    }

    // Once A releases the write lock, B can begin unobstructed — proving the
    // collision was the held lock, not a broken connection.
    drop(tx_a);
    let tx_b = store_b
        .begin_write()
        .expect("the write lock must be free once A's transaction ends");
    drop(tx_b);
}

// T3 (AC3) — `busy_timeout: None` truly clears rusqlite's pre-installed 5000ms
// handler: with the single WAL write lock held by another connection, a colliding
// write fails *promptly* with a BUSY-class error instead of stalling ~5s.
#[test]
fn busy_timeout_none_fails_fast_on_collision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite3");
    let (admin, admin_dev) = (sk(1), sk(2));
    let (g, _room) = genesis(&admin, &admin_dev);

    // Schema first, then the fail-fast writer.
    EventStore::open(&path).unwrap();
    let mut store_b =
        EventStore::open_with(&path, &super::StoreOptions { busy_timeout: None }).unwrap();

    // Hold the one WAL write lock on a separate connection (BEGIN IMMEDIATE + a
    // write) for the duration of the collision.
    let mut holder = Connection::open(&path).unwrap();
    let lock = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    lock.execute("DELETE FROM events WHERE 1 = 0", []).unwrap();

    let start = Instant::now();
    let err = store_b.insert(&g).unwrap_err();
    let elapsed = start.elapsed();

    match &err {
        super::StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => assert!(
            matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked),
            "expected a BUSY/LOCKED code, got {:?}",
            e.code
        ),
        other => panic!("expected a Sqlite busy failure, got {other:?}"),
    }
    // The opt-out must not wait out the 5000ms default it replaced.
    assert!(
        elapsed < Duration::from_secs(2),
        "fail-fast opt-out stalled ({elapsed:?}); did None clear the handler?"
    );
    drop(lock);
}

// T4 (AC2) — the default path *waits* for a briefly-held write lock and then
// commits, rather than failing. A holder thread takes the write lock, rendezvous
// with the writer, then releases after ~50ms; the default-timeout writer blocks
// and succeeds.
#[test]
fn default_busy_timeout_waits_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite3");
    let (admin, admin_dev) = (sk(1), sk(2));
    let (g, room) = genesis(&admin, &admin_dev);
    {
        let mut store = EventStore::open(&path).unwrap();
        store.insert(&g).unwrap(); // schema + the parent `msg` cites
    }
    let msg = message(&admin, &admin_dev, room, vec![g.event_id], "waited", T0 + 1);

    let mut store_b = EventStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let (ph, bh) = (path.clone(), Arc::clone(&barrier));
    let holder = thread::spawn(move || {
        let mut conn = Connection::open(&ph).unwrap();
        let lock = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        lock.execute("DELETE FROM events WHERE 1 = 0", []).unwrap();
        bh.wait(); // rendezvous: the write lock is now held
        thread::sleep(Duration::from_millis(50));
        lock.rollback().unwrap(); // release
    });

    barrier.wait(); // proceed only once the holder owns the lock
    let start = Instant::now();
    store_b
        .insert(&msg)
        .expect("default busy_timeout must wait for the lock, not fail");
    let waited = start.elapsed();
    holder.join().unwrap();

    assert!(
        store_b.contains(&msg.event_id).unwrap(),
        "the waited-for write must be committed"
    );
    // It waited (the lock was provably held first) but nowhere near the 5s cap.
    assert!(
        waited < Duration::from_secs(5),
        "write should have proceeded well before the timeout; waited {waited:?}"
    );
}

// T5 (AC2/AC3) — pin the observed rusqlite default and the opt-out/override
// behavior. rusqlite 0.37 pre-installs 5000ms on open, so the default must read
// back 5000, `None` must actively clear it to 0, and an explicit duration must
// be applied verbatim. Fails loudly if a future rusqlite bump changes the default
// or if `from_connection_with` is "simplified" back to a conditional set.
#[test]
fn busy_timeout_pragma_reflects_store_options() {
    let default = EventStore::open_in_memory().unwrap();
    assert_eq!(
        busy_timeout_ms(&default),
        5000,
        "default StoreOptions must yield a 5000ms busy_timeout"
    );

    let opt_out =
        EventStore::open_in_memory_with(&super::StoreOptions { busy_timeout: None }).unwrap();
    assert_eq!(
        busy_timeout_ms(&opt_out),
        0,
        "busy_timeout: None must clear the handler to 0 (genuine fail-fast)"
    );

    let custom = EventStore::open_in_memory_with(&super::StoreOptions {
        busy_timeout: Some(Duration::from_millis(1234)),
    })
    .unwrap();
    assert_eq!(
        busy_timeout_ms(&custom),
        1234,
        "an explicit busy_timeout must be applied verbatim"
    );
}
