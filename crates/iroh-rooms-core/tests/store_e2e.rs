//! End-to-end integration tests for the `SQLite` event store (IR-0004).
//!
//! The unit tests in `src/store/tests.rs` cover the store's logic exhaustively
//! but use `open_in_memory()` and access private fields. These tests add two
//! boundaries absent from that suite, through the **public API only**:
//!
//! 1. **Validate → store pipeline** — `validate_wire_bytes` output feeding
//!    directly into `EventStore::insert_all`, exercised as a five-event room
//!    session with two participants.
//!
//! 2. **File-backed persistence** — events written to an on-disk `SQLite` file
//!    survive an `EventStore` drop + reopen with all derived state intact;
//!    `rebuild()` called on the reopened store is idempotent (spec D4).

#![cfg(feature = "store")]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MessageText, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;

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

fn validated_genesis(
    id: &SigningKey,
    dev: &SigningKey,
    nonce: [u8; 16],
) -> (ValidatedEvent, RoomId) {
    let id_key = id.identity_key();
    let dev_key = dev.device_key();
    let room = signed::derive_room_id(&id_key, &nonce, T0);
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
            room_name: "E2E Test Room".to_owned(),
            room_nonce: nonce,
            admins: vec![id_key],
            device_binding: binding,
        }),
    };
    let v = validate_wire_bytes(&seal(&ev, dev), &ctx(room)).expect("genesis valid");
    (v, room)
}

fn validated_invite(
    admin: &SigningKey,
    admin_dev: &SigningKey,
    invitee: &SigningKey,
    room: RoomId,
    prev: Vec<EventId>,
    t: u64,
) -> ValidatedEvent {
    let invite_id = [0x01u8; 16];
    let cap_secret = [0x42u8; 16];
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: prev,
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: capability_hash(&room, &invite_id, &cap_secret),
            role: "member".to_owned(),
            invitee_key: invitee.identity_key(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    validate_wire_bytes(&seal(&ev, admin_dev), &ctx(room)).expect("invite valid")
}

fn validated_join(
    member: &SigningKey,
    member_dev: &SigningKey,
    room: RoomId,
    prev: Vec<EventId>,
    t: u64,
) -> ValidatedEvent {
    let binding = DeviceBinding::create(&room, member, member_dev.device_key());
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: member.identity_key(),
        device_id: member_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: t,
        prev_events: prev,
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: [0x01; 16],
            capability_secret: [0x42; 16],
            role: "member".to_owned(),
            device_binding: binding,
            display_name: None,
        }),
    };
    validate_wire_bytes(&seal(&ev, member_dev), &ctx(room)).expect("join valid")
}

fn validated_message(
    sender: &SigningKey,
    dev: &SigningKey,
    room: RoomId,
    prev: Vec<EventId>,
    body: &str,
    t: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: sender.identity_key(),
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

// ---------------------------------------------------------------------------
// Test 1: validate → store pipeline
// ---------------------------------------------------------------------------

/// Full validate → store pipeline: five events from two participants run
/// through `validate_wire_bytes` (event module) then `EventStore::insert_all`
/// (store module), verifying every query API through the public surface only.
///
/// Boundary crossed: `ValidatedEvent` output from the event subsystem feeding
/// the store subsystem.
#[test]
#[allow(clippy::too_many_lines)] // one cohesive end-to-end narrative; splitting fragments it
fn full_session_validate_then_store() {
    let (alice, alice_dev) = (sk(0x10), sk(0x11));
    let (bob, bob_dev) = (sk(0x20), sk(0x21));
    let nonce = [0x10u8; 16];

    // Build a five-event linear chain: genesis → invite → join → msg_a → msg_b.
    let (genesis, room) = validated_genesis(&alice, &alice_dev, nonce);
    let invite = validated_invite(
        &alice,
        &alice_dev,
        &bob,
        room,
        vec![genesis.event_id],
        T0 + 1_000,
    );
    let join = validated_join(&bob, &bob_dev, room, vec![invite.event_id], T0 + 2_000);
    let msg_a = validated_message(
        &alice,
        &alice_dev,
        room,
        vec![join.event_id],
        "Hello Bob",
        T0 + 3_000,
    );
    let msg_b = validated_message(
        &bob,
        &bob_dev,
        room,
        vec![msg_a.event_id],
        "Hey Alice",
        T0 + 4_000,
    );

    let mut store = EventStore::open_in_memory().unwrap();
    let stats = store
        .insert_all(&[
            genesis.clone(),
            invite.clone(),
            join.clone(),
            msg_a.clone(),
            msg_b.clone(),
        ])
        .unwrap();

    assert_eq!(stats.inserted, 5, "all five events stored");
    assert_eq!(stats.duplicate, 0);
    assert_eq!(store.count(&room).unwrap(), 5);

    // Lamport clock assigns 0..=4 along the linear chain.
    for (ev, expected) in [
        (&genesis, 0u64),
        (&invite, 1),
        (&join, 2),
        (&msg_a, 3),
        (&msg_b, 4),
    ] {
        assert_eq!(
            store.get(&ev.event_id).unwrap().unwrap().lamport,
            Some(expected),
            "lamport for {expected}",
        );
    }

    // Room tail returns all five events in ascending (lamport, event_id) order.
    let tail: Vec<EventId> = store
        .room_tail(&room, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(
        tail,
        vec![
            genesis.event_id,
            invite.event_id,
            join.event_id,
            msg_a.event_id,
            msg_b.event_id
        ],
        "room tail is in causal order",
    );

    // by_type: only the two MessageText events.
    let texts: Vec<EventId> = store
        .by_type(&room, EventType::MessageText)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(texts, vec![msg_a.event_id, msg_b.event_id]);

    // by_sender partitioned correctly between the two participants.
    let alice_events: Vec<EventId> = store
        .by_sender(&room, &alice.identity_key())
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(alice_events.len(), 3, "alice: genesis + invite + msg_a");

    let bob_events: Vec<EventId> = store
        .by_sender(&room, &bob.identity_key())
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(bob_events.len(), 2, "bob: join + msg_b");

    // Only the final event is a DAG head.
    assert_eq!(store.heads(&room).unwrap(), vec![msg_b.event_id]);

    // Admin chain: genesis(0) → invite(1). msg_a's parent is join (admin_seq=None),
    // so the admin chain breaks there; msg_a and msg_b both get admin_seq=None.
    assert_eq!(
        store.get(&genesis.event_id).unwrap().unwrap().admin_seq,
        Some(0)
    );
    assert_eq!(
        store.get(&invite.event_id).unwrap().unwrap().admin_seq,
        Some(1)
    );
    assert_eq!(store.get(&msg_a.event_id).unwrap().unwrap().admin_seq, None);
    let (tip_id, tip_seq) = store.admin_chain_tip(&room).unwrap().expect("tip present");
    assert_eq!((tip_id, tip_seq), (invite.event_id, 1));

    // Parent/child edges link correctly across the causal boundary.
    assert_eq!(
        store.parents_of(&invite.event_id).unwrap(),
        vec![genesis.event_id],
    );
    assert_eq!(
        store.children_of(&genesis.event_id).unwrap(),
        vec![invite.event_id],
    );
    assert!(store.missing_parents(&msg_b.event_id).unwrap().is_empty());

    // Verbatim wire bytes round-trip through the store.
    assert_eq!(
        store
            .get(&genesis.event_id)
            .unwrap()
            .unwrap()
            .wire
            .to_bytes(),
        genesis.wire.to_bytes(),
        "wire bytes stored verbatim",
    );
}

// ---------------------------------------------------------------------------
// Test 2: file-backed persistence across close + reopen
// ---------------------------------------------------------------------------

/// File-backed persistence: a chain of validated events written to an on-disk
/// `SQLite` file survives an `EventStore` drop + reopen with all derived state
/// (`lamport`, `admin_seq`, parent edges) intact and every query API returning
/// correct results — without calling `rebuild()`.
///
/// Boundary crossed: `EventStore` close → OS filesystem → `EventStore` reopen.
#[test]
fn file_backed_persistence_survives_close_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("events.db");

    let (alice, alice_dev) = (sk(0x30), sk(0x31));
    let (bob, bob_dev) = (sk(0x40), sk(0x41));
    let nonce = [0x30u8; 16];

    let (genesis, room) = validated_genesis(&alice, &alice_dev, nonce);
    let invite = validated_invite(
        &alice,
        &alice_dev,
        &bob,
        room,
        vec![genesis.event_id],
        T0 + 1_000,
    );
    let join = validated_join(&bob, &bob_dev, room, vec![invite.event_id], T0 + 2_000);
    let msg = validated_message(
        &alice,
        &alice_dev,
        room,
        vec![join.event_id],
        "Persisted!",
        T0 + 3_000,
    );

    // Write phase — store dropped at end of block, closing the connection.
    {
        let mut store = EventStore::open(&db_path).unwrap();
        store
            .insert_all(&[genesis.clone(), invite.clone(), join.clone(), msg.clone()])
            .unwrap();
        assert_eq!(store.count(&room).unwrap(), 4, "sanity before close");
    }

    // Read phase — reopen from the same file path.
    let store = EventStore::open(&db_path).unwrap();

    assert_eq!(store.count(&room).unwrap(), 4, "count survives reopen");
    assert!(store.contains(&genesis.event_id).unwrap());
    assert!(store.contains(&invite.event_id).unwrap());
    assert!(store.contains(&join.event_id).unwrap());
    assert!(store.contains(&msg.event_id).unwrap());

    // Derived lamport values persist.
    assert_eq!(
        store.get(&genesis.event_id).unwrap().unwrap().lamport,
        Some(0)
    );
    assert_eq!(
        store.get(&invite.event_id).unwrap().unwrap().lamport,
        Some(1)
    );
    assert_eq!(store.get(&join.event_id).unwrap().unwrap().lamport, Some(2));
    assert_eq!(store.get(&msg.event_id).unwrap().unwrap().lamport, Some(3));

    // admin_seq for admin-chain events persists.
    assert_eq!(
        store.get(&genesis.event_id).unwrap().unwrap().admin_seq,
        Some(0)
    );
    assert_eq!(
        store.get(&invite.event_id).unwrap().unwrap().admin_seq,
        Some(1)
    );

    // Verbatim wire bytes preserved through the file round-trip.
    assert_eq!(
        store
            .get(&genesis.event_id)
            .unwrap()
            .unwrap()
            .wire
            .to_bytes(),
        genesis.wire.to_bytes(),
        "genesis wire bytes verbatim after reopen",
    );

    // Query surface works normally after reopen.
    let tail: Vec<EventId> = store
        .room_tail(&room, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(tail.len(), 4, "tail returns all 4 events");
    assert_eq!(
        tail[0], genesis.event_id,
        "genesis first in canonical order"
    );
    assert_eq!(tail[3], msg.event_id, "msg last in canonical order");

    assert_eq!(store.heads(&room).unwrap(), vec![msg.event_id]);
    assert_eq!(
        store.parents_of(&invite.event_id).unwrap(),
        vec![genesis.event_id],
    );
    assert_eq!(
        store.admin_chain_tip(&room).unwrap(),
        Some((invite.event_id, 1)),
    );
}

// ---------------------------------------------------------------------------
// Test 4: out-of-order delivery spanning a close + reopen
// ---------------------------------------------------------------------------

/// Out-of-order delivery across a persistence boundary: a child event is stored
/// in session 1 (its parent doesn't exist yet, so `lamport` stays `NULL`), then
/// the store is closed and reopened for session 2, at which point the parent
/// arrives. The dangling edge recorded in session 1 must resolve and the lamport
/// clock must propagate after the parent arrives in session 2.
///
/// Boundary crossed: dangling `event_parents` edge written in session 1 →
/// OS filesystem → `EventStore` reopen → parent stored in session 2.
#[test]
fn file_backed_out_of_order_delivery_resolves_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ooo.db");

    let (alice, alice_dev) = (sk(0x70), sk(0x71));
    let nonce = [0x70u8; 16];

    let (genesis, room) = validated_genesis(&alice, &alice_dev, nonce);
    let msg = validated_message(
        &alice,
        &alice_dev,
        room,
        vec![genesis.event_id],
        "arrives before genesis",
        T0 + 1_000,
    );

    // Session 1: store only the child; genesis is not yet known.
    {
        let mut store = EventStore::open(&db_path).unwrap();
        store.insert(&msg).unwrap();
        // Parent missing → lamport stays NULL.
        assert_eq!(
            store.get(&msg.event_id).unwrap().unwrap().lamport,
            None,
            "lamport must be NULL while parent is missing",
        );
        assert_eq!(
            store.missing_parents(&msg.event_id).unwrap(),
            vec![genesis.event_id],
            "genesis listed as a missing parent",
        );
    } // store dropped — dangling edge persisted to disk

    // Session 2: reopen; the dangling edge must still be recorded.
    let mut store = EventStore::open(&db_path).unwrap();
    assert_eq!(
        store.missing_parents(&msg.event_id).unwrap(),
        vec![genesis.event_id],
        "dangling parent still missing after reopen",
    );

    // Deliver the parent in this session.
    store.insert(&genesis).unwrap();

    // Edge must resolve and lamport must propagate within the same session.
    assert!(
        store.missing_parents(&msg.event_id).unwrap().is_empty(),
        "no missing parents once genesis is stored",
    );
    assert_eq!(
        store.get(&genesis.event_id).unwrap().unwrap().lamport,
        Some(0),
    );
    assert_eq!(
        store.get(&msg.event_id).unwrap().unwrap().lamport,
        Some(1),
        "child lamport resolves to 1 after parent arrives in session 2",
    );
    assert_eq!(
        store.heads(&room).unwrap(),
        vec![msg.event_id],
        "message is the sole DAG head",
    );
    assert_eq!(store.count(&room).unwrap(), 2);
}

// ---------------------------------------------------------------------------
// Test 5: duplicate insert is idempotent across close + reopen
// ---------------------------------------------------------------------------

/// Idempotency across the persistence boundary: re-inserting an event that was
/// stored in a previous session returns `Duplicate`, leaves the event count
/// unchanged, and preserves the verbatim wire bytes.
///
/// Boundary crossed: event stored in session 1 → OS filesystem →
/// `EventStore` reopen → same event re-inserted in session 2.
#[test]
fn file_backed_duplicate_across_reopen_is_ignored() {
    use iroh_rooms_core::store::InsertOutcome;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dedup.db");

    let (alice, alice_dev) = (sk(0x80), sk(0x81));
    let nonce = [0x80u8; 16];

    let (genesis, room) = validated_genesis(&alice, &alice_dev, nonce);
    let wire_bytes_before: Vec<u8>;

    // Session 1: initial insert.
    {
        let mut store = EventStore::open(&db_path).unwrap();
        store.insert(&genesis).unwrap();
        assert_eq!(store.count(&room).unwrap(), 1);
        wire_bytes_before = store
            .get(&genesis.event_id)
            .unwrap()
            .unwrap()
            .wire
            .to_bytes();
    }

    // Session 2: reopen and re-insert the same event.
    let mut store = EventStore::open(&db_path).unwrap();
    let outcome = store.insert(&genesis).unwrap();
    assert_eq!(
        outcome,
        InsertOutcome::Duplicate,
        "re-insert of a persisted event must return Duplicate across reopen",
    );
    assert_eq!(
        store.count(&room).unwrap(),
        1,
        "count must not change after duplicate insert",
    );
    assert_eq!(
        store
            .get(&genesis.event_id)
            .unwrap()
            .unwrap()
            .wire
            .to_bytes(),
        wire_bytes_before,
        "wire bytes must be unchanged after duplicate insert across reopen",
    );
    // Derived state must also be intact after the no-op duplicate.
    assert_eq!(
        store.get(&genesis.event_id).unwrap().unwrap().lamport,
        Some(0),
        "lamport unchanged after duplicate across reopen",
    );
}

// ---------------------------------------------------------------------------
// Test 3: file-backed rebuild is idempotent after close + reopen
// ---------------------------------------------------------------------------

/// `rebuild()` called on a fully-consistent file-backed store after a close +
/// reopen produces the exact same derived state as the original insert, proving
/// restart-determinism (spec D4) over the filesystem boundary.
///
/// Boundary crossed: `EventStore` drop → reopen → `rebuild()`.
#[test]
fn file_backed_rebuild_is_idempotent_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rebuild.db");

    let (admin, admin_dev) = (sk(0x50), sk(0x51));
    let (bob, bob_dev) = (sk(0x60), sk(0x61));
    let nonce = [0x50u8; 16];

    let (genesis, room) = validated_genesis(&admin, &admin_dev, nonce);
    let invite = validated_invite(
        &admin,
        &admin_dev,
        &bob,
        room,
        vec![genesis.event_id],
        T0 + 1_000,
    );
    let join = validated_join(&bob, &bob_dev, room, vec![invite.event_id], T0 + 2_000);
    let msg1 = validated_message(&bob, &bob_dev, room, vec![join.event_id], "one", T0 + 3_000);
    let msg2 = validated_message(&bob, &bob_dev, room, vec![msg1.event_id], "two", T0 + 4_000);

    // Write phase — capture derived state immediately after insert.
    let (pre_genesis, pre_invite, pre_join, pre_msg1, pre_msg2);
    {
        let mut store = EventStore::open(&db_path).unwrap();
        store
            .insert_all(&[
                genesis.clone(),
                invite.clone(),
                join.clone(),
                msg1.clone(),
                msg2.clone(),
            ])
            .unwrap();
        pre_genesis = store.get(&genesis.event_id).unwrap().unwrap();
        pre_invite = store.get(&invite.event_id).unwrap().unwrap();
        pre_join = store.get(&join.event_id).unwrap().unwrap();
        pre_msg1 = store.get(&msg1.event_id).unwrap().unwrap();
        pre_msg2 = store.get(&msg2.event_id).unwrap().unwrap();
    } // store dropped

    // Reopen and rebuild — state must be byte-identical to the pre-close snapshot.
    let mut store = EventStore::open(&db_path).unwrap();
    store.rebuild().unwrap();

    assert_eq!(store.get(&genesis.event_id).unwrap().unwrap(), pre_genesis);
    assert_eq!(store.get(&invite.event_id).unwrap().unwrap(), pre_invite);
    assert_eq!(store.get(&join.event_id).unwrap().unwrap(), pre_join);
    assert_eq!(store.get(&msg1.event_id).unwrap().unwrap(), pre_msg1);
    assert_eq!(store.get(&msg2.event_id).unwrap().unwrap(), pre_msg2);

    // Parent edges and aggregate queries also intact after rebuild.
    assert_eq!(
        store.parents_of(&invite.event_id).unwrap(),
        vec![genesis.event_id],
    );
    assert_eq!(
        store.parents_of(&msg2.event_id).unwrap(),
        vec![msg1.event_id],
    );
    assert_eq!(store.heads(&room).unwrap(), vec![msg2.event_id]);
    assert_eq!(
        store.admin_chain_tip(&room).unwrap(),
        Some((invite.event_id, 1)),
    );
    assert_eq!(store.count(&room).unwrap(), 5);
}
