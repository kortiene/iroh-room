//! Vector §20 (clock skew) and vector §12 (equivocation) — the advisory-flag
//! discipline: a flag NEVER changes the verdict, the validated set, ordering, or
//! any authorization/expiry decision (spike §6 step 10 / §8 / §9).

use iroh_rooms_core::event::reject::Flag;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::{Ingest, RoomMembership};

use super::fixtures;

const T: u64 = fixtures::T_ROOM;

// ===========================================================================
// §20 — Clock skew is advisory only and never affects the validated set.
// ===========================================================================

#[test]
fn vector_20_clock_skew_advisory_only() {
    // A fully valid member.joined (E_join_bob), evaluated under two clocks.
    let log = fixtures::log();
    let bytes = log.e_join_bob.wire.to_bytes();
    let created_at = log.e_join_bob.event.created_at;
    let room = log.room_id;

    // Peer P: a clock far behind ⇒ created_at is >300 s ahead ⇒ ClockSkew flag.
    let skewed = validate_wire_bytes(
        &bytes,
        &ValidationContext {
            expected_room: room,
            now_ms: Some(created_at - 301_000),
        },
    )
    .expect("a clock-skewed but valid event must still be accepted");

    // Peer Q: created_at within bounds ⇒ no flag.
    let in_bounds = validate_wire_bytes(
        &bytes,
        &ValidationContext {
            expected_room: room,
            now_ms: Some(created_at + 1_000),
        },
    )
    .expect("an in-bounds event must be accepted");

    // Same validated set from the same wire bytes: identical id, bytes, and event.
    assert_eq!(
        skewed.event_id, in_bounds.event_id,
        "the event_id must be clock-independent"
    );
    assert_eq!(
        skewed.signed_bytes(),
        in_bounds.signed_bytes(),
        "the signed bytes must be clock-independent"
    );
    assert_eq!(
        skewed.event, in_bounds.event,
        "the decoded event must be clock-independent"
    );

    // The ONLY difference is the advisory flag on P.
    assert_eq!(
        skewed.flags,
        vec![Flag::ClockSkew],
        "P must carry exactly the ClockSkew advisory flag"
    );
    assert!(
        in_bounds.flags.is_empty(),
        "Q must carry no flag; the event is in bounds"
    );
}

#[test]
fn clock_skew_threshold_boundary() {
    // The check is strict `created_at > now + CLOCK_SKEW_FUTURE_MS` (300_000 ms):
    // exactly at the threshold is NOT flagged; one ms over IS.
    let log = fixtures::log();
    let bytes = log.e_join_bob.wire.to_bytes();
    let created_at = log.e_join_bob.event.created_at;
    let room = log.room_id;

    let at_threshold = validate_wire_bytes(
        &bytes,
        &ValidationContext {
            expected_room: room,
            now_ms: Some(created_at - 300_000),
        },
    )
    .expect("must accept");
    assert!(
        at_threshold.flags.is_empty(),
        "no flag exactly at the threshold"
    );

    let over_threshold = validate_wire_bytes(
        &bytes,
        &ValidationContext {
            expected_room: room,
            now_ms: Some(created_at - 300_001),
        },
    )
    .expect("must accept");
    assert_eq!(
        over_threshold.flags,
        vec![Flag::ClockSkew],
        "one ms over the threshold must flag"
    );
}

// ===========================================================================
// §12 — Equivocation / fork detection (admin signs two concurrent events).
// ===========================================================================

#[test]
fn vector_12_admin_equivocation_flagged() {
    let log = fixtures::log();
    let mut m = RoomMembership::from_events(
        log.room_id,
        vec![
            log.e_create.clone(),
            log.e_inv_bob.clone(),
            log.e_join_bob.clone(),
            log.e_inv_carol.clone(),
            log.e_join_carol.clone(),
            log.e_msg_bob.clone(),
        ],
    );
    let before = m.tracked_event_count();

    // Alice (the admin) authors two message.text events on the SAME parent
    // [E_msg_bob] without either self-parenting the other — an equivocation.
    let e_eq_a = fixtures::message(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        "branch one",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    );
    let e_eq_b = fixtures::message(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        "branch two",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    );
    let (id_a, id_b) = (e_eq_a.event_id, e_eq_b.event_id);
    assert_ne!(id_a, id_b, "the two forked events must have distinct ids");

    // Both are crypto-valid and enter as concurrent siblings; the second reveals
    // the fork by the shared signer (`alice_dev`).
    let out_a = m.ingest(e_eq_a);
    assert!(
        matches!(out_a, Ingest::Accepted { .. }),
        "E_eq_a must be accepted; got {out_a:?}"
    );
    let out_b = m.ingest(e_eq_b);
    let Ingest::Accepted { flags, .. } = out_b else {
        panic!("E_eq_b must be accepted; got {out_b:?}");
    };
    assert!(
        flags.contains(&Flag::Equivocation),
        "the concurrent second event must raise the equivocation flag"
    );

    // Both are kept (advisory only — the flag never drops an event), and the
    // signer is the admin (⇒ CRITICAL severity in the spike taxonomy).
    assert_eq!(
        m.tracked_event_count(),
        before + 2,
        "both forked events must be retained"
    );
    assert_eq!(
        m.snapshot().admin(),
        Some(&fixtures::alice_id()),
        "the equivocating signer is the immutable admin (CRITICAL)"
    );

    // State stays deterministic: the same set folds identically regardless of the
    // order the fork was delivered in.
    let mut reverse = RoomMembership::from_events(
        log.room_id,
        vec![
            log.e_create.clone(),
            log.e_inv_bob.clone(),
            log.e_join_bob.clone(),
            log.e_inv_carol.clone(),
            log.e_join_carol.clone(),
            log.e_msg_bob.clone(),
        ],
    );
    reverse.ingest(fixtures::message(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        "branch two",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    ));
    reverse.ingest(fixtures::message(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        "branch one",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    ));
    assert_eq!(
        m.snapshot(),
        reverse.snapshot(),
        "the folded snapshot must be delivery-order independent"
    );
}
