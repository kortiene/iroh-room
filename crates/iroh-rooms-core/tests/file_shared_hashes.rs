//! Focused coverage for [`SyncEngine::file_shared_hashes`] (IR-0204 spec §5.3) —
//! the per-hash authorization source the Blob Plane serve gate consults (Gate 2).
//!
//! A provider serves a blob **only** if a valid `file.shared` in the room
//! references its hash, so this set must be *exactly* the blob hashes of the
//! room's `file.shared` events: deduplicated, and ignoring every other event type.
//! Getting it wrong is the spec R2 risk — serving an unreferenced hash (an
//! exfiltration path) or failing to serve a legitimately-shared one. These are the
//! always-green, network-free complement to the `blob_e2e` two-peer suite: they
//! pin the *source* the gate reads, without any transport.
//!
//! The events are seeded through the public `validate_wire_bytes → EventStore`
//! pipeline (the same path `iroh-rooms file share` uses), mirroring
//! `tests/store_e2e.rs`. `file_shared_hashes` reads them straight back off the
//! store by type, so the assertions are fully deterministic.

#![cfg(feature = "sync")]
#![allow(clippy::similar_names)] // alice_id / alice_dev are intentionally parallel

use std::collections::BTreeSet;

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_file_shared;
use iroh_rooms_core::event::content::{Content, EventType, MessageText, RoomCreated};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};

const T0: u64 = 1_750_000_000_000;
const NONCE: [u8; 16] = [0xab; 16];

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

/// Genesis authored by `alice` (an admin ⇒ active from the very first event, so
/// every `file.shared` she then authors is a valid content event). Returns the
/// validated event, the derived room id, and the genesis event id (a valid parent
/// for the shares below).
fn genesis(alice_id: &SigningKey, alice_dev: &SigningKey) -> (ValidatedEvent, RoomId, EventId) {
    let room = signed::derive_room_id(&alice_id.identity_key(), &NONCE, T0);
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Blob Gate Source".to_owned(),
            room_nonce: NONCE,
            admins: vec![alice_id.identity_key()],
            device_binding: DeviceBinding::create(&room, alice_id, alice_dev.device_key()),
        }),
    };
    let v = validate_wire_bytes(&seal(&ev, alice_dev), &ctx(room)).expect("genesis valid");
    let id = v.event_id;
    (v, room, id)
}

/// A `file.shared` authored by `alice`, referencing `blob_hash`, parented on `prev`
/// (built through the same pure builder `file share` uses).
fn file_shared(
    alice_id: &SigningKey,
    alice_dev: &SigningKey,
    room: RoomId,
    file_id: [u8; 16],
    blob_hash: [u8; 32],
    prev: &[EventId],
) -> ValidatedEvent {
    let wire = build_file_shared(
        alice_id,
        alice_dev,
        &room,
        file_id,
        "f.bin",
        "application/octet-stream",
        3,
        HashRef::from_bytes(blob_hash),
        Some("raw"),
        &[],
        prev,
        T0 + 1,
    );
    validate_wire_bytes(&wire.to_bytes(), &ctx(room)).expect("file.shared valid")
}

/// A `message.text` authored by `alice` — a non-`file.shared` event that must never
/// contribute a referenced hash.
fn message(
    alice_id: &SigningKey,
    alice_dev: &SigningKey,
    room: RoomId,
    prev: &[EventId],
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T0 + 2,
        prev_events: prev.to_vec(),
        content: Content::MessageText(MessageText {
            body: "not a file".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    validate_wire_bytes(&seal(&ev, alice_dev), &ctx(room)).expect("message valid")
}

/// Seed `events` into a fresh in-memory store (genesis must be first) and open a
/// sync engine over it.
fn open_engine(events: &[ValidatedEvent], room: RoomId) -> SyncEngine {
    let mut store = EventStore::open_in_memory().expect("in-memory store");
    store.insert_all(events).expect("seed events");
    SyncEngine::open(store, room, SyncConfig::default()).expect("open engine")
}

// ---------------------------------------------------------------------------

#[test]
fn empty_room_has_no_referenced_hashes() {
    // A room with no shared files must reference no hashes — the gate serves
    // nothing (fail-closed by construction of the empty set).
    let (id, dev) = (sk(0x01), sk(0x81));
    let (g, room, _gid) = genesis(&id, &dev);
    let engine = open_engine(&[g], room);

    let hashes = engine.file_shared_hashes().expect("read referenced hashes");
    assert!(
        hashes.is_empty(),
        "a room with no file.shared references no hashes, got {hashes:?}"
    );
}

#[test]
fn single_file_shared_contributes_exactly_its_hash() {
    // The one servable hash is precisely the blob_hash of the one file.shared.
    let (id, dev) = (sk(0x02), sk(0x82));
    let (g, room, gid) = genesis(&id, &dev);
    let h1 = [0x11u8; 32];
    let fs = file_shared(&id, &dev, room, [0xA1; 16], h1, &[gid]);
    let engine = open_engine(&[g, fs], room);

    let hashes = engine.file_shared_hashes().expect("read referenced hashes");
    assert_eq!(
        hashes,
        BTreeSet::from([h1]),
        "the referenced set must equal exactly the shared file's blob hash"
    );
}

#[test]
fn distinct_hashes_are_all_referenced() {
    // Two files with different content ⇒ both hashes servable (Gate 2 authorizes
    // each independently).
    let (id, dev) = (sk(0x03), sk(0x83));
    let (g, room, gid) = genesis(&id, &dev);
    let h1 = [0x11u8; 32];
    let h2 = [0x22u8; 32];
    let fs1 = file_shared(&id, &dev, room, [0xA1; 16], h1, &[gid]);
    let fs2 = file_shared(&id, &dev, room, [0xA2; 16], h2, &[gid]);
    let engine = open_engine(&[g, fs1, fs2], room);

    let hashes = engine.file_shared_hashes().expect("read referenced hashes");
    assert_eq!(hashes, BTreeSet::from([h1, h2]));
    assert_eq!(hashes.len(), 2, "both distinct hashes must be present");
}

#[test]
fn duplicate_hash_across_two_shares_is_deduplicated() {
    // Two separate file.shared events (distinct file ids) pointing at the *same*
    // content must collapse to a single referenced hash — the gate authorizes the
    // hash, not the reference, so the set must not double-count it.
    let (id, dev) = (sk(0x04), sk(0x84));
    let (g, room, gid) = genesis(&id, &dev);
    let shared_hash = [0x33u8; 32];
    let fs_a = file_shared(&id, &dev, room, [0xA1; 16], shared_hash, &[gid]);
    let fs_b = file_shared(&id, &dev, room, [0xB2; 16], shared_hash, &[gid]);
    let engine = open_engine(&[g, fs_a, fs_b], room);

    let hashes = engine.file_shared_hashes().expect("read referenced hashes");
    assert_eq!(
        hashes,
        BTreeSet::from([shared_hash]),
        "a hash shared twice must appear once"
    );
    assert_eq!(hashes.len(), 1, "the duplicate must be deduplicated");
}

#[test]
fn non_file_shared_events_do_not_contribute_hashes() {
    // A chat message shares the room log with file.shared events; only the
    // file.shared hash may be servable. If message bodies ever leaked into the
    // referenced set, the gate would serve arbitrary store content.
    let (id, dev) = (sk(0x05), sk(0x85));
    let (g, room, gid) = genesis(&id, &dev);
    let msg = message(&id, &dev, room, &[gid]);
    let h1 = [0x44u8; 32];
    let fs = file_shared(&id, &dev, room, [0xA1; 16], h1, &[gid]);
    let engine = open_engine(&[g, msg, fs], room);

    let hashes = engine.file_shared_hashes().expect("read referenced hashes");
    assert_eq!(
        hashes,
        BTreeSet::from([h1]),
        "only file.shared hashes count; the message.text must be ignored"
    );
}
