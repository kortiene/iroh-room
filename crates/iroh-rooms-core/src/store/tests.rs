//! Focused unit tests for the `SQLite` event store, mapped to the issue Acceptance
//! Criteria (the broader integration suite lives in `tests/` per the test phase).

use rusqlite::params;

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
