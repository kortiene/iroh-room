//! End-to-end test: membership lifecycle persisted to a file-backed `SQLite` store,
//! reloaded after a drop+reopen, re-validated through the full
//! `validate_wire_bytes` pipeline, and folded into `RoomMembership`.
//!
//! ## What boundary this crosses
//!
//! `membership_fold.rs` is exhaustive but entirely in-memory: it feeds
//! `ValidatedEvent`s directly into the fold.  This file adds the **persistence
//! boundary**: the exact same event set is written to an on-disk `SQLite` store, the
//! store is dropped (fd closed, WAL checkpointed), reopened, the wire bytes are
//! reloaded, re-validated, and folded.  The resulting `MembershipSnapshot` must be
//! byte-identical to the one from the in-memory reference fold, and access-control
//! decisions must be consistent with both snapshots.
//!
//! The test also verifies the `rebuild()` round-trip: all derived state is wiped
//! and recomputed from the authoritative `(event_id, wire)` pairs, after which
//! `room_tail` still returns the complete set with correct lamport values.

#![cfg(feature = "store")]
// `bob`/`blob` and similar short fixture names trip the pedantic similar-names
// lint; the pairs are unambiguous in context, so allow it for this test module.
#![allow(clippy::similar_names)]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, FileShared, MemberInvited, MemberJoined, MemberRemoved,
    RoomCreated,
};
use iroh_rooms_core::event::ids::{HashRef, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::{
    blob_serve_allowed, BlobDecision, DenyReason, MembershipSnapshot, RoomMembership, Status,
};
use iroh_rooms_core::store::EventStore;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NONCE: [u8; 16] = [0xaa; 16];
const T0: u64 = 1_750_000_000_000;

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

struct Principal {
    id: SigningKey,
    dev: SigningKey,
}

impl Principal {
    fn new(seed: u8) -> Self {
        Self {
            id: sk(seed),
            dev: sk(seed.wrapping_add(0x80)),
        }
    }

    fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }

    fn device(&self) -> DeviceKey {
        self.dev.device_key()
    }
}

fn ctx(room: RoomId) -> ValidationContext {
    ValidationContext::for_room(room)
}

fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

fn validate(ev: &SignedEvent, dev: &SigningKey, room: RoomId) -> ValidatedEvent {
    validate_wire_bytes(&seal(ev, dev), &ctx(room)).expect("must be stateless-valid")
}

// ---------------------------------------------------------------------------
// Shared event chain
// ---------------------------------------------------------------------------

/// Build the five-event lifecycle: genesis → invite → join → `file_share` → remove.
///
/// Alice is the admin.  Bob is invited, joins, and is then removed.  Alice shares
/// a blob while both are active; it is the shared blob used for access-control
/// assertions.  Returns `(events_in_causal_order, room_id, blob_hash)`.
#[allow(clippy::too_many_lines)] // five fully-specified events inline for fixture clarity
fn build_lifecycle(alice: &Principal, bob: &Principal) -> (Vec<ValidatedEvent>, RoomId, HashRef) {
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let gen = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::RoomCreated,
            created_at: T0,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "Persist Test".to_owned(),
                room_nonce: NONCE,
                admins: vec![alice.identity()],
                device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
            }),
        },
        &alice.dev,
        room,
    );

    let inv_id: [u8; 16] = [0x01; 16];
    let inv_sec: [u8; 16] = [0x42; 16];
    let inv = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::MemberInvited,
            created_at: T0 + 1,
            prev_events: vec![gen.event_id],
            content: Content::MemberInvited(MemberInvited {
                invite_id: inv_id,
                capability_hash: capability_hash(&room, &inv_id, &inv_sec),
                role: "member".to_owned(),
                invitee_key: bob.identity(),
                expires_at: None,
                invitee_hint: None,
            }),
        },
        &alice.dev,
        room,
    );

    let jn = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: bob.identity(),
            device_id: bob.device(),
            event_type: EventType::MemberJoined,
            created_at: T0 + 2,
            prev_events: vec![inv.event_id],
            content: Content::MemberJoined(MemberJoined {
                via_invite_id: inv_id,
                capability_secret: inv_sec,
                role: "member".to_owned(),
                device_binding: DeviceBinding::create(&room, &bob.id, bob.device()),
                display_name: None,
            }),
        },
        &bob.dev,
        room,
    );

    let blob = HashRef::from_bytes([0xbe; 32]);
    let share = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::FileShared,
            created_at: T0 + 3,
            prev_events: vec![jn.event_id],
            content: Content::FileShared(FileShared {
                file_id: [0x01; 16],
                name: "data.bin".to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                size_bytes: 42,
                blob_hash: blob,
                blob_format: None,
                providers: None,
            }),
        },
        &alice.dev,
        room,
    );

    let kick = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::MemberRemoved,
            created_at: T0 + 4,
            prev_events: vec![share.event_id],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: bob.identity(),
                removed_by: alice.identity(),
                reason: None,
                device_binding: None,
            }),
        },
        &alice.dev,
        room,
    );

    (vec![gen, inv, jn, share, kick], room, blob)
}

// ---------------------------------------------------------------------------
// Snapshot assertion helper
// ---------------------------------------------------------------------------

/// Assert the expected membership state and access-control decisions on `snap`.
///
/// Post-kick state: Alice = Admin/Active, Bob = Removed.
/// The shared blob was authored by Alice while Active → Serve for Alice's device,
/// Reject(NotActive) for Bob's device (device lookup succeeds but status check
/// fails).
fn assert_expected_snapshot(
    snap: &MembershipSnapshot,
    alice: &Principal,
    bob: &Principal,
    blob: HashRef,
    label: &str,
) {
    assert_eq!(
        snap.admin(),
        Some(&alice.identity()),
        "{label}: admin must be Alice"
    );
    assert!(
        snap.is_active(&alice.identity()),
        "{label}: Alice must be Active"
    );
    assert_eq!(
        snap.status(&bob.identity()),
        Some(Status::Removed),
        "{label}: Bob must be Removed"
    );

    // Device → identity reverse lookup survives the round-trip.
    assert_eq!(
        snap.identity_of_device(&alice.device()),
        Some(&alice.identity()),
        "{label}: Alice's device lookup"
    );
    assert_eq!(
        snap.identity_of_device(&bob.device()),
        Some(&bob.identity()),
        "{label}: Bob's device lookup (retained from join even after removal)"
    );

    // The blob was shared by Alice; Alice is Active → Serve for Alice's device.
    let alice_id = alice.identity();
    let shares = move |h: &HashRef| -> Option<IdentityKey> { (*h == blob).then_some(alice_id) };

    assert_eq!(
        blob_serve_allowed(snap, &alice.device(), &blob, &shares),
        BlobDecision::Serve,
        "{label}: Alice (Active, sharer Active) must be Served"
    );
    // Bob's device is known, but Bob is Removed → NotActive.
    assert_eq!(
        blob_serve_allowed(snap, &bob.device(), &blob, &shares),
        BlobDecision::Reject(DenyReason::NotActive),
        "{label}: Bob (Removed) must be denied"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The membership fold computed from the original `ValidatedEvent`s must equal
/// the fold computed from the same events after a file-backed store round-trip
/// (insert → drop+reopen → reload → re-validate).  Access-control decisions on
/// both snapshots must be correct.
#[test]
fn membership_fold_survives_store_round_trip() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (events, room, blob) = build_lifecycle(&alice, &bob);

    // Reference: in-memory fold from the original ValidatedEvents.
    let reference = RoomMembership::from_events(room, events.clone());
    let ref_snap = reference.snapshot();
    assert_expected_snapshot(&ref_snap, &alice, &bob, blob, "in-memory reference");

    // Write the events to a file-backed store, then drop it.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("room.db");
    {
        let mut store = EventStore::open(&db_path).expect("open store");
        let stats = store.insert_all(&events).expect("insert_all");
        assert_eq!(stats.inserted, 5, "all five events stored");
        assert_eq!(stats.duplicate, 0, "no duplicates on first insert");
    }

    // Reopen the store and reload all events via room_tail.
    let reloaded: Vec<ValidatedEvent> = {
        let store = EventStore::open(&db_path).expect("reopen store");
        let stored = store.room_tail(&room, 100).expect("room_tail");
        assert_eq!(
            stored.len(),
            5,
            "all five events reloaded after reopen (lamport chain complete)"
        );
        // Re-validate each stored wire payload; this re-executes the full
        // stateless pipeline on the persisted bytes.
        stored
            .into_iter()
            .map(|s| {
                let bytes = s.wire.to_bytes();
                validate_wire_bytes(&bytes, &ctx(room)).expect("stored wire bytes must re-validate")
            })
            .collect()
    };

    // Fold the reloaded events; order from room_tail is canonical but the fold
    // is order-independent, so the result must be byte-identical to the reference.
    let from_store = RoomMembership::from_events(room, reloaded);
    let store_snap = from_store.snapshot();

    assert_eq!(
        ref_snap, store_snap,
        "snapshot from store round-trip must be byte-identical to the in-memory reference"
    );
    assert_expected_snapshot(&store_snap, &alice, &bob, blob, "store round-trip");
}

/// After `EventStore::rebuild()` wipes and recomputes all derived state, the
/// reloaded and re-validated events produce the same membership snapshot and
/// access-control decisions as the pre-rebuild reference.
#[test]
fn membership_fold_survives_rebuild() {
    let alice = Principal::new(0x02);
    let bob = Principal::new(0x11);
    let (events, room, blob) = build_lifecycle(&alice, &bob);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("room.db");

    // Insert, rebuild (wipes + recomputes derived state), then reload.
    let reloaded: Vec<ValidatedEvent> = {
        let mut store = EventStore::open(&db_path).expect("open store");
        store.insert_all(&events).expect("insert_all");

        // Wipe and recompute all derived state from the authoritative (event_id, wire) pairs.
        store.rebuild().expect("rebuild");

        let stored = store
            .room_tail(&room, 100)
            .expect("room_tail after rebuild");
        assert_eq!(
            stored.len(),
            5,
            "rebuild must restore all five events to room_tail"
        );
        stored
            .into_iter()
            .map(|s| {
                let bytes = s.wire.to_bytes();
                validate_wire_bytes(&bytes, &ctx(room))
                    .expect("post-rebuild wire bytes must re-validate")
            })
            .collect()
    };

    let from_rebuild = RoomMembership::from_events(room, reloaded);
    let rebuild_snap = from_rebuild.snapshot();
    assert_expected_snapshot(&rebuild_snap, &alice, &bob, blob, "after rebuild");
}

/// Inserting the same five events a second time is fully idempotent: all five
/// return `Duplicate`, and the fold computed afterwards is unchanged.
#[test]
fn idempotent_insert_does_not_corrupt_fold() {
    let alice = Principal::new(0x03);
    let bob = Principal::new(0x12);
    let (events, room, blob) = build_lifecycle(&alice, &bob);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("room.db");

    let mut store = EventStore::open(&db_path).expect("open store");
    store.insert_all(&events).expect("first insert");

    // Second insert of the identical set must return all duplicates.
    let dup_stats = store.insert_all(&events).expect("second insert");
    assert_eq!(dup_stats.inserted, 0, "no new rows on duplicate insert");
    assert_eq!(dup_stats.duplicate, 5, "all five are duplicates");

    let stored = store.room_tail(&room, 100).expect("room_tail");
    assert_eq!(
        stored.len(),
        5,
        "still exactly five rows after duplicate inserts"
    );

    let reloaded: Vec<ValidatedEvent> = stored
        .into_iter()
        .map(|s| {
            validate_wire_bytes(&s.wire.to_bytes(), &ctx(room))
                .expect("re-validate after duplicate insert")
        })
        .collect();

    let after_dup = RoomMembership::from_events(room, reloaded);
    let snap = after_dup.snapshot();
    assert_expected_snapshot(&snap, &alice, &bob, blob, "after duplicate inserts");
}

// ---------------------------------------------------------------------------
// Helpers for additional e2e scenarios
// ---------------------------------------------------------------------------

/// Build a concurrent-event lifecycle: genesis → invite → {concurrent join + kick}.
///
/// Both join and kick cite only the invite as their single parent, making them
/// causally concurrent. Removed-dominates means the fold must yield
/// `Status::Removed` for the invitee regardless of arrival order.
fn build_concurrent_lifecycle(
    alice: &Principal,
    dave: &Principal,
) -> (Vec<ValidatedEvent>, RoomId) {
    let room = signed::derive_room_id(&alice.identity(), &NONCE, T0);

    let gen = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::RoomCreated,
            created_at: T0,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "Concurrent Test".to_owned(),
                room_nonce: NONCE,
                admins: vec![alice.identity()],
                device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
            }),
        },
        &alice.dev,
        room,
    );

    let inv_id: [u8; 16] = [0x77; 16];
    let inv_sec: [u8; 16] = [0x88; 16];
    let inv = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::MemberInvited,
            created_at: T0 + 1,
            prev_events: vec![gen.event_id],
            content: Content::MemberInvited(MemberInvited {
                invite_id: inv_id,
                capability_hash: capability_hash(&room, &inv_id, &inv_sec),
                role: "member".to_owned(),
                invitee_key: dave.identity(),
                expires_at: None,
                invitee_hint: None,
            }),
        },
        &alice.dev,
        room,
    );

    // Concurrent children of the invite: dave joins, alice kicks simultaneously.
    let join_dave = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: dave.identity(),
            device_id: dave.device(),
            event_type: EventType::MemberJoined,
            created_at: T0 + 2,
            prev_events: vec![inv.event_id],
            content: Content::MemberJoined(MemberJoined {
                via_invite_id: inv_id,
                capability_secret: inv_sec,
                role: "member".to_owned(),
                device_binding: DeviceBinding::create(&room, &dave.id, dave.device()),
                display_name: None,
            }),
        },
        &dave.dev,
        room,
    );

    let kick_dave = validate(
        &SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: alice.identity(),
            device_id: alice.device(),
            event_type: EventType::MemberRemoved,
            created_at: T0 + 2,
            prev_events: vec![inv.event_id],
            content: Content::MemberRemoved(MemberRemoved {
                member_id: dave.identity(),
                removed_by: alice.identity(),
                reason: None,
                device_binding: None,
            }),
        },
        &alice.dev,
        room,
    );

    (vec![gen, inv, join_dave, kick_dave], room)
}

// ---------------------------------------------------------------------------
// E2E: AC5 × persistence — concurrent join+kick convergence survives store
// ---------------------------------------------------------------------------

/// Concurrent join and kick are stored in reverse causal order; after a
/// file-backed store round-trip the fold still converges to `Removed`
/// (AC5 × persistence boundary).  This test stresses both the lamport cascade
/// (parents inserted after children) and the Removed-dominates rule on reload.
#[test]
fn concurrent_join_kick_converges_after_store_round_trip() {
    let alice = Principal::new(0x04);
    let dave = Principal::new(0x41);

    let (events, room) = build_concurrent_lifecycle(&alice, &dave);

    // Reference: in-memory fold must already yield Removed.
    let reference_snap = RoomMembership::from_events(room, events.clone()).snapshot();
    assert_eq!(
        reference_snap.status(&dave.identity()),
        Some(Status::Removed),
        "in-memory reference must be Removed before persistence"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("concurrent.db");

    // Insert in REVERSE causal order to exercise the lamport cascade.
    {
        let mut store = EventStore::open(&db_path).expect("open store");
        for ev in events.iter().rev() {
            store.insert(ev).expect("insert");
        }
    }

    // Reopen, reload via room_tail, re-validate, fold.
    let reloaded: Vec<ValidatedEvent> = {
        let store = EventStore::open(&db_path).expect("reopen store");
        let stored = store.room_tail(&room, 100).expect("room_tail");
        assert_eq!(
            stored.len(),
            4,
            "all four events must be causal-complete after reverse-order insert + reopen"
        );
        stored
            .into_iter()
            .map(|s| {
                validate_wire_bytes(&s.wire.to_bytes(), &ctx(room))
                    .expect("stored event must re-validate")
            })
            .collect()
    };

    let store_snap = RoomMembership::from_events(room, reloaded).snapshot();

    assert_eq!(
        store_snap.status(&dave.identity()),
        Some(Status::Removed),
        "Removed-dominates must hold after store round-trip"
    );
    assert_eq!(
        reference_snap, store_snap,
        "snapshot must be byte-identical to the in-memory reference"
    );
}

// ---------------------------------------------------------------------------
// E2E: multi-room isolation — two rooms interleaved in a shared store
// ---------------------------------------------------------------------------

/// Two rooms are written interleaved into a single `SQLite` file.  `room_tail`
/// must return exactly each room's own events; folds must not bleed across
/// rooms; access decisions are scoped to the room.
#[test]
fn multi_room_isolation_in_shared_store() {
    // Room A: alice (admin) invites and later removes bob.
    let alice = Principal::new(0x05);
    let bob = Principal::new(0x13);
    // Room B: carol (admin) invites and later removes dave.
    let carol = Principal::new(0x22);
    let dave = Principal::new(0x43);

    let (events_a, room_a, blob_a) = build_lifecycle(&alice, &bob);
    let (events_b, room_b, blob_b) = build_lifecycle(&carol, &dave);

    assert_ne!(
        room_a, room_b,
        "distinct admin keys must yield distinct room IDs"
    );
    assert_eq!(events_a.len(), 5);
    assert_eq!(events_b.len(), 5);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("shared.db");

    // Interleave: room_a[0], room_b[0], room_a[1], room_b[1], …
    {
        let mut store = EventStore::open(&db_path).expect("open shared store");
        for i in 0..5 {
            store.insert(&events_a[i]).expect("insert room_a");
            store.insert(&events_b[i]).expect("insert room_b");
        }
    }

    // Load and fold both rooms from the shared store.
    let store = EventStore::open(&db_path).expect("reopen shared store");

    let snap_a = {
        let stored = store.room_tail(&room_a, 100).expect("room_tail A");
        assert_eq!(stored.len(), 5, "room A must have exactly 5 events");
        let evs: Vec<ValidatedEvent> = stored
            .into_iter()
            .map(|s| {
                validate_wire_bytes(&s.wire.to_bytes(), &ctx(room_a))
                    .expect("room A event must re-validate")
            })
            .collect();
        RoomMembership::from_events(room_a, evs).snapshot()
    };

    let snap_b = {
        let stored = store.room_tail(&room_b, 100).expect("room_tail B");
        assert_eq!(stored.len(), 5, "room B must have exactly 5 events");
        let evs: Vec<ValidatedEvent> = stored
            .into_iter()
            .map(|s| {
                validate_wire_bytes(&s.wire.to_bytes(), &ctx(room_b))
                    .expect("room B event must re-validate")
            })
            .collect();
        RoomMembership::from_events(room_b, evs).snapshot()
    };

    // Room A membership: alice=Admin/Active, bob=Removed.
    assert_eq!(
        snap_a.admin(),
        Some(&alice.identity()),
        "room A admin must be Alice"
    );
    assert_eq!(
        snap_a.status(&bob.identity()),
        Some(Status::Removed),
        "Bob must be Removed in room A"
    );

    // Room B membership: carol=Admin/Active, dave=Removed.
    assert_eq!(
        snap_b.admin(),
        Some(&carol.identity()),
        "room B admin must be Carol"
    );
    assert_eq!(
        snap_b.status(&dave.identity()),
        Some(Status::Removed),
        "Dave must be Removed in room B"
    );

    // Cross-room isolation: room A's principals are unknown in room B.
    assert_eq!(
        snap_b.status(&alice.identity()),
        None,
        "Alice (room A admin) must be unknown in room B"
    );
    assert_eq!(
        snap_a.status(&carol.identity()),
        None,
        "Carol (room B admin) must be unknown in room A"
    );

    // Access decisions are room-scoped: Alice's device has no binding in room B.
    let alice_id = alice.identity();
    let shares_a = move |h: &HashRef| (*h == blob_a).then_some(alice_id);
    assert_eq!(
        blob_serve_allowed(&snap_a, &alice.device(), &blob_a, &shares_a),
        BlobDecision::Serve,
        "Alice's device must be served the blob in her own room"
    );

    let carol_id = carol.identity();
    let shares_b = move |h: &HashRef| (*h == blob_b).then_some(carol_id);
    assert_eq!(
        blob_serve_allowed(&snap_b, &alice.device(), &blob_b, &shares_b),
        BlobDecision::Reject(DenyReason::UnknownDevice),
        "Alice's device must be UnknownDevice in room B (no cross-room device leak)"
    );
}
