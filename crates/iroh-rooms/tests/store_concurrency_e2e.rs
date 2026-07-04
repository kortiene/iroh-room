//! Façade e2e coverage for issue #85 (`SQLite` `busy_timeout` + `IMMEDIATE`
//! writes for concurrent `EventStore` writers on a shared DB) — spec T6 of
//! `specs/sqlite-store-busy-timeout-concurrent-writers.md`.
//!
//! `iroh-rooms-core/src/store/tests.rs` already covers the fix exhaustively
//! (interleaved inserts, the read-then-write upgrade race, `BEGIN IMMEDIATE`
//! lock ordering, the fail-fast opt-out, and the `busy_timeout` pragma
//! values) — but those tests reach into the store's private `Connection`
//! field and live inside `iroh-rooms-core` itself. **Bantaba**, the consumer
//! that filed #85, only ever sees the public `iroh_rooms::experimental::store`
//! façade. This file proves the issue's acceptance sketch — "two `EventStore`
//! connections on one file-backed DB doing interleaved concurrent writes
//! pass without any `SQLITE_BUSY` reaching the caller" — holds through that
//! façade surface alone, and that the `open_with`/`StoreOptions` hook the
//! issue asks for is itself reachable from façade-only imports.
//!
//! Runs unmarked in CI, mirroring `facade_e2e.rs`'s own (non-`#[ignore]`)
//! tier: both tests use a small N (per the spec's §7 sizing guidance) so they
//! stay well under a second despite exercising real file-backed WAL
//! contention on two threads.

#![cfg(feature = "experimental")]

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use iroh_rooms::events::{
    build_message_text, validate_wire_bytes, EventId, ValidatedEvent, ValidationContext,
};
use iroh_rooms::experimental::store::{EventStore, StoreOptions};
use iroh_rooms::identity::SigningKey;
use iroh_rooms::room::{build_room_created, derive_room_id, RoomId};

const NONCE: [u8; 16] = [0x85; 16];
const T0: u64 = 1_750_000_000_000;

/// A deterministic (identity, device) signing-key pair, seeded from one byte
/// (mirrors `facade_e2e.rs::Principal`).
struct Principal {
    id: SigningKey,
    dev: SigningKey,
}

impl Principal {
    fn new(seed: u8) -> Self {
        Self {
            id: SigningKey::from_seed(&[seed; 32]),
            dev: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
        }
    }
}

/// Author + validate the room genesis, entirely through façade calls.
fn build_genesis(admin: &Principal) -> (ValidatedEvent, RoomId) {
    let room_id = derive_room_id(&admin.id.identity_key(), &NONCE, T0);
    let ctx = ValidationContext::for_room(room_id);
    let wire = build_room_created(&admin.id, &admin.dev, "Busy-Timeout E2E Room", &NONCE, T0);
    let validated =
        validate_wire_bytes(&wire.to_bytes(), &ctx).expect("genesis validates through the facade");
    (validated, room_id)
}

/// Author + validate one `message.text` citing `parent`, through façade calls.
fn build_message(
    admin: &Principal,
    room_id: RoomId,
    parent: EventId,
    body: &str,
    created_at: u64,
) -> ValidatedEvent {
    let ctx = ValidationContext::for_room(room_id);
    let wire = build_message_text(
        &admin.id,
        &admin.dev,
        &room_id,
        body,
        Some("plain"),
        None,
        &[],
        &[parent],
        created_at,
    );
    validate_wire_bytes(&wire.to_bytes(), &ctx).expect("message validates through the facade")
}

/// `N` distinct `message.text` events tagged `{tag}{i}`, all citing `parent`.
fn build_side(
    admin: &Principal,
    room_id: RoomId,
    parent: EventId,
    tag: char,
    first_t: u64,
) -> Vec<ValidatedEvent> {
    const N: usize = 4;
    (0..N)
        .map(|i| {
            let t = first_t + u64::try_from(i).unwrap();
            build_message(admin, room_id, parent, &format!("{tag}{i}"), t)
        })
        .collect()
}

/// Issue #85's acceptance sketch, reproduced using **only** façade imports:
/// two `EventStore` connections opened onto one file-backed DB (default
/// `StoreOptions`, i.e. the 5000ms `busy_timeout` + `BEGIN IMMEDIATE` writes),
/// each hammering `insert` from its own thread with no coordination beyond a
/// start barrier. No `insert` may return an error — a colliding writer must
/// wait, not surface `SQLITE_BUSY` to the caller — and a third connection
/// must read back exactly the union of both writers' events.
#[test]
fn two_facade_connections_interleaved_writes_never_surface_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.sqlite3");
    let admin = Principal::new(0x01);
    let (genesis, room_id) = build_genesis(&admin);

    // Seed the shared parent once, before the racing connections open.
    {
        let mut seed_store = EventStore::open(&path).expect("open seed store through the facade");
        seed_store.insert(&genesis).expect("seed the genesis event");
    }

    let events_a = build_side(&admin, room_id, genesis.event_id, 'a', T0 + 1);
    let events_b = build_side(&admin, room_id, genesis.event_id, 'b', T0 + 1_000);
    let expected: BTreeSet<EventId> = std::iter::once(genesis.event_id)
        .chain(events_a.iter().map(|e| e.event_id))
        .chain(events_b.iter().map(|e| e.event_id))
        .collect();

    let mut store_a = EventStore::open(&path).expect("open connection A through the facade");
    let mut store_b = EventStore::open(&path).expect("open connection B through the facade");
    let barrier = Arc::new(Barrier::new(2));

    let ba = Arc::clone(&barrier);
    let ta = thread::spawn(move || {
        ba.wait();
        for ev in &events_a {
            store_a
                .insert(ev)
                .expect("facade connection A: no SQLITE_BUSY may reach the caller");
        }
    });
    let bb = Arc::clone(&barrier);
    let tb = thread::spawn(move || {
        bb.wait();
        for ev in &events_b {
            store_b
                .insert(ev)
                .expect("facade connection B: no SQLITE_BUSY may reach the caller");
        }
    });
    ta.join().expect("connection A thread must not panic");
    tb.join().expect("connection B thread must not panic");

    // A third facade connection reads back exactly the union of both writers.
    let verify = EventStore::open(&path).expect("open verify connection through the facade");
    assert_eq!(
        verify
            .room_event_ids(&room_id)
            .expect("room_event_ids through the facade"),
        expected,
        "both connections' events must all be durably stored with none silently dropped"
    );
}

/// The issue's alternative suggested shape — an `open_with`/`StoreOptions`
/// hook the embedder can use to configure the busy handler — is itself
/// reachable using only façade imports. `StoreOptions` is `#[non_exhaustive]`
/// in the defining crate, so a struct literal (even `{ busy_timeout: ..,
/// ..Default::default() }`) does not compile from this crate — only
/// `StoreOptions::new` does, which is exactly why that constructor exists.
/// Two connections opened this way still complete interleaved concurrent
/// writes cleanly. `iroh-rooms-core`'s own suite already pins the exact
/// `busy_timeout` pragma values for the default/`None`/custom cases; this
/// only proves an SDK consumer can actually reach the hook end to end.
#[test]
fn open_with_store_options_hook_reachable_through_the_facade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.sqlite3");
    let admin = Principal::new(0x02);
    let (genesis, room_id) = build_genesis(&admin);

    let opts = StoreOptions::new(Some(Duration::from_secs(2)));
    {
        let mut seed_store =
            EventStore::open_with(&path, &opts).expect("open_with through the facade");
        seed_store.insert(&genesis).expect("seed the genesis event");
    }

    let events_a = build_side(&admin, room_id, genesis.event_id, 'a', T0 + 1);
    let events_b = build_side(&admin, room_id, genesis.event_id, 'b', T0 + 1_000);
    let n = events_a.len() + events_b.len();

    let mut store_a = EventStore::open_with(&path, &opts).expect("open_with connection A");
    let mut store_b = EventStore::open_with(&path, &opts).expect("open_with connection B");
    let barrier = Arc::new(Barrier::new(2));

    let ba = Arc::clone(&barrier);
    let ta = thread::spawn(move || {
        ba.wait();
        for ev in &events_a {
            store_a
                .insert(ev)
                .expect("open_with connection A must not surface SQLITE_BUSY");
        }
    });
    let bb = Arc::clone(&barrier);
    let tb = thread::spawn(move || {
        bb.wait();
        for ev in &events_b {
            store_b
                .insert(ev)
                .expect("open_with connection B must not surface SQLITE_BUSY");
        }
    });
    ta.join().expect("connection A thread must not panic");
    tb.join().expect("connection B thread must not panic");

    let verify = EventStore::open(&path).expect("verify connection through the facade");
    assert_eq!(
        verify.count(&room_id).expect("count through the facade"),
        1 + u64::try_from(n).unwrap(),
        "open_with-configured connections must durably store every event"
    );
}
