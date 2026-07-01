//! Vectors §11, §13–§19 — the stateful fold/authorization and access-gate layer,
//! plus the two taxonomy outcomes the stateless module cannot reach:
//! `unbound_device` (a member signing with an unbound device) and the
//! `from_removed_member` advisory flag.
//!
//! Log-validity here is judged against each event's own causal ancestors (spec
//! D3); the §16/§17 access gates deliberately consult the **current** snapshot
//! (spec D6) — a since-removed member's log-valid event grants zero capability.

use iroh_rooms_core::event::content::Content;
use iroh_rooms_core::event::ids::HashRef;
use iroh_rooms_core::event::reject::{Flag, RejectReason};
use iroh_rooms_core::membership::{
    blob_serve_allowed, pipe_connect_allowed, BlobDecision, DenyReason, Ingest, PipeDecision, Role,
    RoomMembership, Status,
};

use super::fixtures;

const T: u64 = fixtures::T_ROOM;

/// Assert an ingest outcome is a rejection with the exact reason.
#[allow(clippy::needless_pass_by_value)] // by-value reads cleaner at the call sites
fn assert_rejected(outcome: &Ingest, reason: RejectReason) {
    assert!(
        matches!(outcome, Ingest::Rejected { reason: r, .. } if *r == reason),
        "expected Rejected({reason:?}); got {outcome:?}"
    );
}

// ===========================================================================
// §11 — Concurrent invite/join vs kick fork ⇒ identical membership (Removed)
//       on both arrival orders.
// ===========================================================================

#[test]
fn vector_11_concurrent_join_kick_removed() {
    let log = fixtures::log();
    let base = vec![
        log.e_create.clone(),
        log.e_inv_bob.clone(),
        log.e_join_bob.clone(),
        log.e_inv_carol.clone(),
        log.e_join_carol.clone(),
        log.e_msg_bob.clone(),
        log.e_inv_dave.clone(),
    ];

    // P1 receives A(join)-then-B(kick); P2 receives B-then-A.
    let mut p1 = RoomMembership::from_events(log.room_id, base.clone());
    p1.ingest(log.e_join_dave.clone());
    p1.ingest(log.e_kick_dave.clone());

    let mut p2 = RoomMembership::from_events(log.room_id, base);
    p2.ingest(log.e_kick_dave.clone());
    p2.ingest(log.e_join_dave.clone());

    let s1 = p1.snapshot();
    let s2 = p2.snapshot();
    assert_eq!(
        s1.status(&fixtures::dave_id()),
        Some(Status::Removed),
        "Removed-dominates: Dave must be Removed on P1"
    );
    assert_eq!(
        s2.status(&fixtures::dave_id()),
        Some(Status::Removed),
        "Removed-dominates: Dave must be Removed on P2"
    );
    assert_eq!(
        s1, s2,
        "both arrival orders must fold to the identical snapshot"
    );
}

// ===========================================================================
// §13 — Non-member event rejection (`not_a_member`).
// ===========================================================================

#[test]
fn vector_13_non_member_event_rejected() {
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

    // Mallory (never invited/joined) authors a well-formed message.text.
    let e_mal = fixtures::message(
        &fixtures::mallory_id_sk(),
        &fixtures::mallory_dev_sk(),
        "well-formed but unauthorized",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    );
    let outcome = m.ingest(e_mal);
    assert_rejected(&outcome, RejectReason::NotAMember);
}

// ===========================================================================
// §14 — Insufficient role (non-admin admin-only event); self-leave accepted.
// ===========================================================================

#[test]
fn vector_14_insufficient_role_rejected() {
    let log = fixtures::log();
    let seed = || {
        RoomMembership::from_events(
            log.room_id,
            vec![
                log.e_create.clone(),
                log.e_inv_bob.clone(),
                log.e_join_bob.clone(),
                log.e_inv_carol.clone(),
                log.e_join_carol.clone(),
                log.e_msg_bob.clone(),
            ],
        )
    };
    let prev = &[log.e_msg_bob.event_id];

    // Bob (member, not admin) authors a member.invited ⇒ insufficient_role.
    let bob_invite = fixtures::invite(
        &fixtures::bob_id_sk(),
        &fixtures::bob_dev_sk(),
        &[0x14; 16],
        &[0x41; 16],
        "member",
        &fixtures::dave_id(),
        None,
        prev,
        T + 6_000,
    );
    assert_rejected(&seed().ingest(bob_invite), RejectReason::InsufficientRole);

    // Bob authors a member.removed of Carol ⇒ insufficient_role.
    let bob_kick = fixtures::member_removed(
        &fixtures::bob_id_sk(),
        &fixtures::bob_dev_sk(),
        &fixtures::carol_id(),
        prev,
        T + 6_000,
    );
    assert_rejected(&seed().ingest(bob_kick), RejectReason::InsufficientRole);

    // Positive control: Bob's own member.left validates (voluntary self-leave).
    let bob_left = fixtures::member_left(
        &fixtures::bob_id_sk(),
        &fixtures::bob_dev_sk(),
        prev,
        T + 6_000,
    );
    assert!(
        matches!(seed().ingest(bob_left), Ingest::Accepted { .. }),
        "a self member.left must be accepted"
    );
}

// ===========================================================================
// §15 — Stale/expired invite and bad capability.
// ===========================================================================

#[test]
fn vector_15_bad_capability_and_expired_invite() {
    let log = fixtures::log();
    let base = vec![
        log.e_create.clone(),
        log.e_inv_bob.clone(),
        log.e_join_bob.clone(),
        log.e_inv_carol.clone(),
        log.e_join_carol.clone(),
        log.e_msg_bob.clone(),
        log.e_inv_dave.clone(),
    ];

    // (a) A join citing E_inv_dave with the WRONG secret ⇒ bad_capability.
    let mut m_bad = RoomMembership::from_events(log.room_id, base.clone());
    let bad_join = fixtures::join(
        &fixtures::dave_id_sk(),
        &fixtures::dave_dev_sk(),
        &fixtures::DAVE_INVITE_ID,
        &[0xff; 16], // wrong secret — does not reproduce the capability hash
        "member",
        &[log.e_inv_dave.event_id],
        T + 8_000,
    );
    assert_rejected(&m_bad.ingest(bad_join), RejectReason::BadCapability);

    // (b) A rejoin after E_kick_dave, correct secret, citing the consumed invite
    //     ⇒ expired_invite (sticky departure, §3.7).
    let mut m_exp = RoomMembership::from_events(log.room_id, base);
    m_exp.ingest(log.e_kick_dave.clone());
    let stale_rejoin = fixtures::join(
        &fixtures::dave_id_sk(),
        &fixtures::dave_dev_sk(),
        &fixtures::DAVE_INVITE_ID,
        &fixtures::DAVE_SECRET, // correct secret this time
        "member",
        &[log.e_kick_dave.event_id], // descends from the removal that consumed it
        T + 9_000,
    );
    assert_rejected(&m_exp.ingest(stale_rejoin), RejectReason::ExpiredInvite);
}

// ===========================================================================
// §16 — Blob serve gate against the resolved membership set.
// ===========================================================================

#[test]
fn vector_16_blob_serve_gate() {
    let log = fixtures::log();
    let snapshot = RoomMembership::from_events(log.room_id, log.all()).snapshot();
    let blob = HashRef::from_bytes(fixtures::BLOB_HASH);

    // The blob is referenced by E_file, shared by (currently-Active) Bob.
    let shares = |h: &HashRef| {
        if *h == blob {
            Some(fixtures::bob_id())
        } else {
            None
        }
    };

    // Carol: Active member ⇒ served.
    assert_eq!(
        blob_serve_allowed(&snapshot, &fixtures::carol_dev(), &blob, &shares),
        BlobDecision::Serve,
        "Carol (Active) must be served"
    );
    // Dave: Removed (device bound via his join, but status Removed) ⇒ rejected.
    assert_eq!(
        blob_serve_allowed(&snapshot, &fixtures::dave_dev(), &blob, &shares),
        BlobDecision::Reject(DenyReason::NotActive),
        "Dave (Removed) must be rejected"
    );
    // Mallory: never a member, device resolves to no identity ⇒ rejected.
    assert_eq!(
        blob_serve_allowed(&snapshot, &fixtures::mallory_dev(), &blob, &shares),
        BlobDecision::Reject(DenyReason::UnknownDevice),
        "Mallory (non-member) must be rejected"
    );
}

// ===========================================================================
// §17 — Pipe connect gate enforces `allowed_members` ∩ Active (no default-all).
// ===========================================================================

#[test]
fn vector_17_pipe_connect_gate() {
    let log = fixtures::log();
    let snapshot = RoomMembership::from_events(log.room_id, log.all()).snapshot();
    let Content::PipeOpened(pipe) = &log.e_pipe.event.content else {
        panic!("E_pipe must carry pipe.opened content");
    };

    // Alice: Active, in allowed_members [alice, bob], owner (Bob) Active ⇒ accepted.
    assert_eq!(
        pipe_connect_allowed(&snapshot, &fixtures::alice_dev(), pipe, None),
        PipeDecision::Accept,
        "Alice must be accepted"
    );
    // Carol: Active but NOT in allowed_members ⇒ rejected (no default-all).
    assert_eq!(
        pipe_connect_allowed(&snapshot, &fixtures::carol_dev(), pipe, None),
        PipeDecision::Reject(DenyReason::NotAllowed),
        "Carol (Active, not allowed) must be rejected"
    );
    // Dave: Removed ⇒ rejected.
    assert_eq!(
        pipe_connect_allowed(&snapshot, &fixtures::dave_dev(), pipe, None),
        PipeDecision::Reject(DenyReason::NotActive),
        "Dave (Removed) must be rejected"
    );
    // Mallory: non-member device ⇒ rejected.
    assert_eq!(
        pipe_connect_allowed(&snapshot, &fixtures::mallory_dev(), pipe, None),
        PipeDecision::Reject(DenyReason::UnknownDevice),
        "Mallory (non-member) must be rejected"
    );
}

// ===========================================================================
// §18 — Concurrent membership attributes resolve to least privilege (agent).
// ===========================================================================

#[test]
fn vector_18_concurrent_attributes_least_privilege() {
    let log = fixtures::log();
    let g = log.e_create.clone();

    // Two concurrent admin invites for Dave on sibling branches [E_create], with
    // conflicting roles: one `member`, one `agent`.
    let inv_member = fixtures::invite(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        &[0x1a; 16],
        &[0x2a; 16],
        "member",
        &fixtures::dave_id(),
        None,
        &[g.event_id],
        T + 1_000,
    );
    let inv_agent = fixtures::invite(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        &[0x1b; 16],
        &[0x2b; 16],
        "agent",
        &fixtures::dave_id(),
        None,
        &[g.event_id],
        T + 1_000,
    );
    // Dave joins citing the `agent` invite (role must equal the cited invite's).
    let join = fixtures::join(
        &fixtures::dave_id_sk(),
        &fixtures::dave_dev_sk(),
        &[0x1b; 16],
        &[0x2b; 16],
        "agent",
        &[inv_agent.event_id],
        T + 2_000,
    );

    let snapshot =
        RoomMembership::from_events(log.room_id, vec![g, inv_member, inv_agent, join]).snapshot();
    assert_eq!(
        snapshot.role(&fixtures::dave_id()),
        Some(Role::Agent),
        "least-privilege merge must resolve Dave's role to agent"
    );
    assert_eq!(
        snapshot.status(&fixtures::dave_id()),
        Some(Status::Active),
        "Dave must still be Active after joining"
    );
}

// ===========================================================================
// §19 — Voluntary leave then rejoin requires a fresh post-leave invite.
// ===========================================================================

#[test]
fn vector_19_leave_consumes_invite() {
    let log = fixtures::log();

    // Bob was invited (E_inv_bob) and joined (E_join_bob); now he leaves.
    let e_left_bob = fixtures::member_left(
        &fixtures::bob_id_sk(),
        &fixtures::bob_dev_sk(),
        &[log.e_join_bob.event_id],
        T + 3_000,
    );
    // A rejoin re-citing the ORIGINAL invite, descending from the leave.
    let rejoin = fixtures::join(
        &fixtures::bob_id_sk(),
        &fixtures::bob_dev_sk(),
        &fixtures::BOB_INVITE_ID,
        &fixtures::BOB_SECRET,
        "member",
        &[e_left_bob.event_id],
        T + 4_000,
    );

    let mut m = RoomMembership::from_events(
        log.room_id,
        vec![
            log.e_create.clone(),
            log.e_inv_bob.clone(),
            log.e_join_bob.clone(),
            e_left_bob,
        ],
    );
    // member.left consumed the cited authorization ⇒ expired_invite.
    assert_rejected(&m.ingest(rejoin), RejectReason::ExpiredInvite);
}

// ===========================================================================
// Taxonomy completion (not reachable statelessly):
//   `unbound_device` and the `from_removed_member` advisory flag.
// ===========================================================================

#[test]
fn unbound_device_is_rejected() {
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

    // Bob (Active, device bound to bob_dev) authors a message.text signed by a
    // DIFFERENT, unbound device ⇒ the membership-derived binding check fails.
    let rogue_dev = fixtures::sk(0xc0);
    let rogue_msg = fixtures::message(
        &fixtures::bob_id_sk(),
        &rogue_dev,
        "signed by an unbound device",
        &[log.e_msg_bob.event_id],
        T + 6_000,
    );
    assert_rejected(&m.ingest(rogue_msg), RejectReason::UnboundDevice);
}

#[test]
fn from_removed_member_flag_on_removed_author() {
    let log = fixtures::log();
    let g = log.e_create.clone();

    // Dave: invited → joined (Active) → posts a message → later removed.
    let inv = fixtures::invite(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        &fixtures::DAVE_INVITE_ID,
        &fixtures::DAVE_SECRET,
        "member",
        &fixtures::dave_id(),
        None,
        &[g.event_id],
        T + 1_000,
    );
    let join = fixtures::join(
        &fixtures::dave_id_sk(),
        &fixtures::dave_dev_sk(),
        &fixtures::DAVE_INVITE_ID,
        &fixtures::DAVE_SECRET,
        "member",
        &[inv.event_id],
        T + 2_000,
    );
    let dave_msg = fixtures::message(
        &fixtures::dave_id_sk(),
        &fixtures::dave_dev_sk(),
        "hi from Dave",
        &[join.event_id],
        T + 3_000,
    );
    let kick = fixtures::member_removed(
        &fixtures::alice_id_sk(),
        &fixtures::alice_dev_sk(),
        &fixtures::dave_id(),
        &[dave_msg.event_id],
        T + 4_000,
    );

    let dave_msg_id = dave_msg.event_id;
    let m = RoomMembership::from_events(log.room_id, vec![g, inv, join, dave_msg, kick]);

    // The message is log-valid (accepted), but its author is now Removed, so the
    // current-snapshot attribution flag is raised (advisory only — the verdict,
    // set, and ordering are unaffected).
    assert_eq!(
        m.snapshot().status(&fixtures::dave_id()),
        Some(Status::Removed),
        "Dave must fold to Removed"
    );
    assert!(
        m.advisory_flags(&dave_msg_id)
            .contains(&Flag::FromRemovedMember),
        "a log-valid event from a since-removed author must carry from_removed_member"
    );
}
