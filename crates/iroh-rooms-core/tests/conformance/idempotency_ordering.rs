//! Vectors §8–§10 — duplicate idempotency, out-of-order buffering, and the
//! deterministic total order.
//!
//! §8 is asserted at **both** the feature-independent fold layer (always run) and
//! the `store`-gated persistence layer (run under `--all-features`, i.e. in CI).
//! The Lamport clock the spike derives (§2.1) is recomputed here as a pure
//! function of the signed `prev_events` — no wire field, no store dependency — so
//! §9/§10 are byte-level tripwires independent of the `SQLite` cache.

use std::collections::BTreeMap;

use iroh_rooms_core::event::ids::EventId;
use iroh_rooms_core::event::validate::ValidatedEvent;
use iroh_rooms_core::membership::{Ingest, RoomMembership};

use super::fixtures;

/// The derived Lamport clock (`lamport(genesis)=0`, else `1 + max(parent
/// lamports)`), computed as a least fixpoint over the signed `prev_events`. A
/// pure function of the event set — identical on every peer, arrival-order
/// independent (spike Membership §2.1).
fn lamport_map(events: &[ValidatedEvent]) -> BTreeMap<EventId, u64> {
    let mut lam: BTreeMap<EventId, u64> = BTreeMap::new();
    loop {
        let mut changed = false;
        for e in events {
            if lam.contains_key(&e.event_id) {
                continue;
            }
            let prev = &e.event.prev_events;
            let next = if prev.is_empty() {
                Some(0)
            } else if prev.iter().all(|p| lam.contains_key(p)) {
                Some(prev.iter().map(|p| lam[p]).max().expect("non-empty") + 1)
            } else {
                None
            };
            if let Some(v) = next {
                lam.insert(e.event_id, v);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    lam
}

/// Sort a set into the canonical timeline `(lamport, event_id)` (`event_id`
/// bytewise, the §2.1 tie-break).
fn timeline(events: &[ValidatedEvent]) -> Vec<EventId> {
    let lam = lamport_map(events);
    let mut ids: Vec<EventId> = events.iter().map(|e| e.event_id).collect();
    ids.sort_by(|a, b| lam[a].cmp(&lam[b]).then_with(|| a.cmp(b)));
    ids
}

// ===========================================================================
// §8 — Duplicate / replay idempotency (1× ≡ 1000×).
// ===========================================================================

#[test]
fn vector_08_duplicate_ignored_idempotently() {
    // Fold layer (feature-independent): re-ingesting a known event is a no-op.
    let log = fixtures::log();
    let mut m = RoomMembership::from_events(log.room_id, log.prefix_through_join_carol());

    let first = m.ingest(log.e_msg_bob.clone());
    assert!(
        matches!(first, Ingest::Accepted { .. }),
        "E_msg_bob must be accepted on first ingest; got {first:?}"
    );
    let count_1x = m.tracked_event_count();
    let snapshot_1x = m.snapshot();

    // 1000× re-ingest of the identical event must not change the outcome or state.
    for _ in 0..1000 {
        let again = m.ingest(log.e_msg_bob.clone());
        assert_eq!(again, first, "re-ingest must return the identical outcome");
    }
    assert_eq!(
        m.tracked_event_count(),
        count_1x,
        "1× ≡ 1000×: the DAG must not grow on duplicate ingest"
    );
    assert_eq!(
        m.snapshot(),
        snapshot_1x,
        "1× ≡ 1000×: the folded snapshot must be unchanged"
    );
}

#[cfg(feature = "store")]
#[test]
fn vector_08_duplicate_ignored_idempotently_store() {
    use iroh_rooms_core::store::{EventStore, InsertOutcome};

    let log = fixtures::log();
    let chain = [
        log.e_create.clone(),
        log.e_inv_bob.clone(),
        log.e_join_bob.clone(),
        log.e_inv_carol.clone(),
        log.e_join_carol.clone(),
        log.e_msg_bob.clone(),
    ];

    let mut store = EventStore::open_in_memory().expect("in-memory store");
    store.insert_all(&chain).expect("insert chain");
    let count_before = store.count(&log.room_id).expect("count");
    let wire_before = store
        .get(&log.e_msg_bob.event_id)
        .expect("get")
        .expect("present")
        .wire
        .to_bytes();

    // Re-insert the identical event 1000×; every one is an ignored Duplicate.
    for _ in 0..1000 {
        assert_eq!(
            store.insert(&log.e_msg_bob).expect("re-insert"),
            InsertOutcome::Duplicate,
            "a byte-identical re-insert must be ignored as Duplicate"
        );
    }
    assert_eq!(
        store.count(&log.room_id).expect("count"),
        count_before,
        "count must not change after duplicate inserts"
    );
    assert_eq!(
        store
            .get(&log.e_msg_bob.event_id)
            .expect("get")
            .expect("present")
            .wire
            .to_bytes(),
        wire_before,
        "verbatim wire bytes must be unchanged after duplicate inserts"
    );
}

// ===========================================================================
// §9 — Out-of-order delivery: child before parent → buffered, then accepted.
// ===========================================================================

#[test]
fn vector_09_child_before_parent_buffered() {
    let log = fixtures::log();

    // A fresh peer holds only E_create … E_join_carol.
    let mut peer = RoomMembership::from_events(log.room_id, log.prefix_through_join_carol());

    // E_file (prev = [E_msg_bob]) delivered BEFORE E_msg_bob ⇒ buffered on the
    // missing parent, NOT rejected.
    let buffered = peer.ingest(log.e_file.clone());
    assert_eq!(
        buffered,
        Ingest::Buffered {
            event_id: log.e_file.event_id,
            missing: vec![log.e_msg_bob.event_id],
        },
        "child-before-parent must buffer on the missing parent"
    );

    // E_msg_bob arrives and validates (Bob Active in its ancestor view).
    assert!(
        matches!(peer.ingest(log.e_msg_bob.clone()), Ingest::Accepted { .. }),
        "E_msg_bob must be accepted once delivered"
    );

    // The buffered E_file is re-processed and now accepted.
    assert!(
        matches!(peer.ingest(log.e_file.clone()), Ingest::Accepted { .. }),
        "buffered E_file must be accepted after its parent arrives"
    );

    // Derived lamport checks: E_msg_bob = 5, E_file = 6 (§9 THEN).
    let accepted = [
        log.e_create.clone(),
        log.e_inv_bob.clone(),
        log.e_join_bob.clone(),
        log.e_inv_carol.clone(),
        log.e_join_carol.clone(),
        log.e_msg_bob.clone(),
        log.e_file.clone(),
    ];
    let lam = lamport_map(&accepted);
    assert_eq!(lam[&log.e_msg_bob.event_id], 5, "E_msg_bob lamport");
    assert_eq!(lam[&log.e_file.event_id], 6, "E_file lamport");

    // Final state is byte-identical to in-order delivery.
    let mut in_order = log.prefix_through_join_carol();
    in_order.push(log.e_msg_bob.clone());
    in_order.push(log.e_file.clone());
    let in_order_snapshot = RoomMembership::from_events(log.room_id, in_order).snapshot();
    assert_eq!(
        peer.snapshot(),
        in_order_snapshot,
        "out-of-order delivery must converge to the in-order snapshot"
    );
}

// ===========================================================================
// §10 — Deterministic total order (Lamport + event_id tie-break).
// ===========================================================================

#[test]
fn vector_10_deterministic_total_order() {
    let log = fixtures::log();
    let set = log.all();

    // Derived lamports match the spike Fixtures table exactly (the fork siblings
    // E_join_dave / E_kick_dave both derive lamport 7; E_file = 6, E_pipe = 7).
    let lam = lamport_map(&set);
    let expected_lamports = [
        (&log.e_create, 0u64),
        (&log.e_inv_bob, 1),
        (&log.e_join_bob, 2),
        (&log.e_inv_carol, 3),
        (&log.e_join_carol, 4),
        (&log.e_msg_bob, 5),
        (&log.e_inv_dave, 6),
        (&log.e_join_dave, 7),
        (&log.e_kick_dave, 7),
        (&log.e_file, 6),
        (&log.e_pipe, 7),
    ];
    for (ev, want) in expected_lamports {
        assert_eq!(lam[&ev.event_id], want, "derived lamport mismatch");
    }

    // The tie at lamport 7 is broken by bytewise event_id: E_join_dave < E_kick_dave.
    assert!(
        log.e_join_dave.event_id < log.e_kick_dave.event_id,
        "the join must sort before the kick at the lamport-7 tie"
    );

    // The pinned timeline: total order by (lamport, event_id).
    let expected_timeline = vec![
        log.e_create.event_id,
        log.e_inv_bob.event_id,
        log.e_join_bob.event_id,
        log.e_inv_carol.event_id,
        log.e_join_carol.event_id,
        log.e_msg_bob.event_id,
        log.e_inv_dave.event_id,  // lamport 6, id 0fcf…
        log.e_file.event_id,      // lamport 6, id 7e5d…
        log.e_join_dave.event_id, // lamport 7, id 54e1…
        log.e_kick_dave.event_id, // lamport 7, id 73c0…
        log.e_pipe.event_id,      // lamport 7, id 8124…
    ];
    assert_eq!(
        timeline(&set),
        expected_timeline,
        "pinned canonical timeline"
    );

    // Determinism: a shuffled input yields the byte-identical timeline.
    let mut shuffled = set.clone();
    shuffled.reverse();
    assert_eq!(
        timeline(&shuffled),
        expected_timeline,
        "the total order must be arrival-order independent"
    );
}
