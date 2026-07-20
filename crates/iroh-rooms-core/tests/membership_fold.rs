//! Focused membership-fold acceptance tests (IR-0008).
//!
//! These verify the issue's six Acceptance Criteria and the load-bearing risk
//! mitigations against the deterministic fold: admin-only authorization writes,
//! key-bound join capability, sticky departure (leave AND removal), concurrent
//! join-vs-kick convergence to Removed, the current-snapshot access split, and
//! `bound_device` enforcement. The exhaustive spike conformance-vector matrix
//! (§11/§13–§19) is layered on in the dedicated tests phase; this file proves the
//! engine behaves before that.
//!
//! Events are built with the same `SigningKey::from_seed` + `genesis` + `seal`
//! pattern as `e2e_lifecycle.rs`, then run through the real `validate_wire_bytes`
//! pipeline so the fold sees genuine `ValidatedEvent`s.

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, FileShared, MemberInvited, MemberJoined, MemberLeft,
    MemberRemoved, MessageText, PipeOpened, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::reject::{Flag, RejectReason};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::{
    blob_serve_allowed, pipe_connect_allowed, BlobDecision, DenyReason, Ingest, MembershipSnapshot,
    PipeDecision, Role, RoomMembership, Status, MAX_ACTIVE_MEMBERS,
};

// --------------------------------------------------------------------------
// Fixtures
// --------------------------------------------------------------------------

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

const NONCE: [u8; 16] = [0xaa; 16];
const T0: u64 = 1_750_000_000_000;

/// A principal: stable identity key + one bound device key.
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

fn seal(ev: &SignedEvent, dev_sk: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev_sk);
    WireEvent::seal(csb, sig).to_bytes()
}

fn validate(ev: &SignedEvent, dev_sk: &SigningKey, room_id: RoomId) -> ValidatedEvent {
    validate_wire_bytes(&seal(ev, dev_sk), &ValidationContext::for_room(room_id))
        .expect("event must be stateless-valid")
}

/// Build + validate the genesis; the signer becomes the immutable admin.
fn genesis(admin: &Principal) -> (ValidatedEvent, RoomId) {
    let room_id = signed::derive_room_id(&admin.identity(), &NONCE, T0);
    let binding = DeviceBinding::create(&room_id, &admin.id, admin.device());
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: binding,
        }),
    };
    let v = validate(&ev, &admin.dev, room_id);
    (v, room_id)
}

#[allow(clippy::too_many_arguments)]
fn invite(
    signer: &Principal,
    room_id: RoomId,
    prev: &[EventId],
    invite_id: [u8; 16],
    secret: [u8; 16],
    invitee: IdentityKey,
    role: &str,
    expires_at: Option<u64>,
    created_at: u64,
) -> ValidatedEvent {
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: signer.identity(),
        device_id: signer.device(),
        event_type: EventType::MemberInvited,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: cap_hash,
            role: role.to_owned(),
            invitee_key: invitee,
            expires_at,
            invitee_hint: None,
        }),
    };
    validate(&ev, &signer.dev, room_id)
}

fn join(
    joiner: &Principal,
    room_id: RoomId,
    prev: &[EventId],
    via_invite_id: [u8; 16],
    secret: [u8; 16],
    role: &str,
    created_at: u64,
) -> ValidatedEvent {
    let binding = DeviceBinding::create(&room_id, &joiner.id, joiner.device());
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: joiner.identity(),
        device_id: joiner.device(),
        event_type: EventType::MemberJoined,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberJoined(MemberJoined {
            via_invite_id,
            capability_secret: secret,
            role: role.to_owned(),
            device_binding: binding,
            display_name: None,
        }),
    };
    validate(&ev, &joiner.dev, room_id)
}

fn leave(member: &Principal, room_id: RoomId, prev: &[EventId], created_at: u64) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: member.identity(),
        device_id: member.device(),
        event_type: EventType::MemberLeft,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberLeft(MemberLeft {
            member_id: member.identity(),
            reason: None,
        }),
    };
    validate(&ev, &member.dev, room_id)
}

fn remove(
    signer: &Principal,
    room_id: RoomId,
    prev: &[EventId],
    target: IdentityKey,
    created_at: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: signer.identity(),
        device_id: signer.device(),
        event_type: EventType::MemberRemoved,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberRemoved(MemberRemoved {
            member_id: target,
            removed_by: signer.identity(),
            reason: None,
            device_binding: None,
        }),
    };
    validate(&ev, &signer.dev, room_id)
}

fn message(
    signer: &Principal,
    device: &SigningKey,
    room_id: RoomId,
    prev: &[EventId],
    body: &str,
    created_at: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: signer.identity(),
        device_id: device.device_key(),
        event_type: EventType::MessageText,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MessageText(MessageText {
            body: body.to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    validate(&ev, device, room_id)
}

fn file_shared(
    signer: &Principal,
    room_id: RoomId,
    prev: &[EventId],
    blob_hash: HashRef,
    created_at: u64,
) -> ValidatedEvent {
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: signer.identity(),
        device_id: signer.device(),
        event_type: EventType::FileShared,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::FileShared(FileShared {
            file_id: [0x01; 16],
            name: "f".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            size_bytes: 1,
            blob_hash,
            blob_format: None,
            providers: None,
        }),
    };
    validate(&ev, &signer.dev, room_id)
}

fn assert_accepted(ingest: &Ingest) {
    assert!(
        matches!(ingest, Ingest::Accepted { .. }),
        "expected Accepted, got {ingest:?}"
    );
}

fn assert_rejected(ingest: &Ingest, expected: &RejectReason) {
    match ingest {
        Ingest::Rejected { reason, .. } => assert_eq!(reason, expected, "wrong reject reason"),
        other => panic!("expected Rejected({expected:?}), got {other:?}"),
    }
}

fn id_of(ingest: &Ingest) -> EventId {
    match ingest {
        Ingest::Accepted { event_id, .. }
        | Ingest::Rejected { event_id, .. }
        | Ingest::Buffered { event_id, .. } => *event_id,
    }
}

// --------------------------------------------------------------------------
// AC1 — admin can invite and remove
// --------------------------------------------------------------------------

#[test]
fn admin_can_invite_and_remove() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let (gen_ev, room) = genesis(&alice);

    let mut m = RoomMembership::new(room);
    assert_accepted(&m.ingest(gen_ev.clone()));

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    assert_accepted(&m.ingest(inv.clone()));

    let rem = remove(&alice, room, &[inv.event_id], carol.identity(), T0 + 2);
    assert_accepted(&m.ingest(rem));

    let snap = m.snapshot();
    assert_eq!(snap.admin(), Some(&alice.identity()));
    assert_eq!(snap.role(&alice.identity()), Some(Role::Admin));
    assert_eq!(snap.status(&bob.identity()), Some(Status::Invited));
    assert_eq!(snap.status(&carol.identity()), Some(Status::Removed));

    // After Bob joins he is Active.
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 3,
    );
    assert_accepted(&m.ingest(jn));
    assert!(m.snapshot().is_active(&bob.identity()));
}

// --------------------------------------------------------------------------
// AC2 — non-admin invite/remove rejected (vector §14)
// --------------------------------------------------------------------------

#[test]
fn non_admin_invite_and_remove_rejected() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    // Bob joins as a member first.
    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob (a plain member) tries to invite and to remove.
    let bob_invite = invite(
        &bob,
        room,
        &[jn.event_id],
        [0x02; 16],
        [0x43; 16],
        carol.identity(),
        "member",
        None,
        T0 + 3,
    );
    assert_rejected(&m.ingest(bob_invite), &RejectReason::InsufficientRole);

    let bob_remove = remove(&bob, room, &[jn.event_id], carol.identity(), T0 + 4);
    assert_rejected(&m.ingest(bob_remove), &RejectReason::InsufficientRole);

    // But Bob may leave (self-departure is valid).
    let bob_leave = leave(&bob, room, &[jn.event_id], T0 + 5);
    assert_accepted(&m.ingest(bob_leave));

    assert_eq!(m.snapshot().status(&carol.identity()), None);
}

// --------------------------------------------------------------------------
// AC3 — join requires a valid key-bound invite capability (vector §15)
// --------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // five distinct capability-failure cases in one flow
fn join_requires_valid_capability() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let mal = Principal::new(0x30);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        Some(T0 + 1_000),
        T0 + 1,
    );
    m.ingest(inv.clone());

    // (a) correct secret → Accepted.
    let good = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    assert_accepted(&m.ingest(good));
    assert!(m.snapshot().is_active(&bob.identity()));

    // (b) wrong secret → bad_capability.
    let bob2 = Principal::new(0x11);
    let inv2 = invite(
        &alice,
        room,
        &[inv.event_id],
        [0x02; 16],
        [0x52; 16],
        bob2.identity(),
        "member",
        None,
        T0 + 3,
    );
    m.ingest(inv2.clone());
    let wrong = join(
        &bob2,
        room,
        &[inv2.event_id],
        [0x02; 16],
        [0x99; 16],
        "member",
        T0 + 4,
    );
    assert_rejected(&m.ingest(wrong), &RejectReason::BadCapability);

    // (c) no naming invite for the key (ban-evasion) → bad_capability.
    let no_invite = join(
        &mal,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 5,
    );
    assert_rejected(&m.ingest(no_invite), &RejectReason::BadCapability);

    // (d) expired (created_at past invite.expires_at) → expired_invite.
    let bob3 = Principal::new(0x12);
    let inv3 = invite(
        &alice,
        room,
        &[inv.event_id],
        [0x03; 16],
        [0x53; 16],
        bob3.identity(),
        "member",
        Some(T0 + 10),
        T0 + 6,
    );
    m.ingest(inv3.clone());
    let late = join(
        &bob3,
        room,
        &[inv3.event_id],
        [0x03; 16],
        [0x53; 16],
        "member",
        T0 + 1_000,
    );
    assert_rejected(&m.ingest(late), &RejectReason::ExpiredInvite);

    // (e) role mismatch (invited member, joins as admin) → insufficient_role.
    let bob4 = Principal::new(0x13);
    let inv4 = invite(
        &alice,
        room,
        &[inv.event_id],
        [0x04; 16],
        [0x54; 16],
        bob4.identity(),
        "member",
        None,
        T0 + 7,
    );
    m.ingest(inv4.clone());
    let mismatch = join(
        &bob4,
        room,
        &[inv4.event_id],
        [0x04; 16],
        [0x54; 16],
        "admin",
        T0 + 8,
    );
    assert_rejected(&m.ingest(mismatch), &RejectReason::InsufficientRole);
}

#[test]
fn join_rejected_when_room_is_full() {
    let alice = Principal::new(0x01);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    assert_accepted(&m.ingest(gen_ev.clone()));

    let mut prev = gen_ev.event_id;
    for i in 0..(MAX_ACTIVE_MEMBERS - 1) {
        let member = Principal::new(0x10 + u8::try_from(i).expect("small fixture index"));
        let invite_id = [0x10 + u8::try_from(i).expect("small fixture index"); 16];
        let secret = [0x40 + u8::try_from(i).expect("small fixture index"); 16];
        let inv = invite(
            &alice,
            room,
            &[prev],
            invite_id,
            secret,
            member.identity(),
            "member",
            None,
            T0 + 1 + u64::try_from(i).expect("small fixture index") * 2,
        );
        assert_accepted(&m.ingest(inv.clone()));
        let jn = join(
            &member,
            room,
            &[inv.event_id],
            invite_id,
            secret,
            "member",
            T0 + 2 + u64::try_from(i).expect("small fixture index") * 2,
        );
        assert_accepted(&m.ingest(jn.clone()));
        prev = jn.event_id;
    }
    assert_eq!(m.snapshot().active_member_count(), MAX_ACTIVE_MEMBERS);

    let extra = Principal::new(0x40);
    let extra_invite_id = [0xee; 16];
    let extra_secret = [0x99; 16];
    let inv = invite(
        &alice,
        room,
        &[prev],
        extra_invite_id,
        extra_secret,
        extra.identity(),
        "member",
        None,
        T0 + 100,
    );
    assert_accepted(&m.ingest(inv.clone()));
    let rejected = join(
        &extra,
        room,
        &[inv.event_id],
        extra_invite_id,
        extra_secret,
        "member",
        T0 + 101,
    );
    assert_rejected(&m.ingest(rejected), &RejectReason::RoomFull);
    assert_eq!(
        m.snapshot().status(&extra.identity()),
        Some(Status::Invited)
    );
}

// --------------------------------------------------------------------------
// AC4 — leave AND removal consume prior invitations (sticky, vector §19)
// --------------------------------------------------------------------------

fn rejoin_after_departure_is_blocked(departure_is_kick: bool) {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    let departure = if departure_is_kick {
        remove(&alice, room, &[jn.event_id], bob.identity(), T0 + 3)
    } else {
        leave(&bob, room, &[jn.event_id], T0 + 3)
    };
    m.ingest(departure.clone());
    assert_eq!(m.snapshot().status(&bob.identity()), Some(Status::Removed));

    // Re-cite the ORIGINAL invite, descending from the departure → consumed.
    let stale_rejoin = join(
        &bob,
        room,
        &[departure.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 4,
    );
    assert_rejected(&m.ingest(stale_rejoin), &RejectReason::ExpiredInvite);
    assert_eq!(m.snapshot().status(&bob.identity()), Some(Status::Removed));

    // A FRESH invite after the departure re-admits.
    let fresh = invite(
        &alice,
        room,
        &[departure.event_id],
        [0x02; 16],
        [0x77; 16],
        bob.identity(),
        "member",
        None,
        T0 + 5,
    );
    m.ingest(fresh.clone());
    let rejoin = join(
        &bob,
        room,
        &[fresh.event_id],
        [0x02; 16],
        [0x77; 16],
        "member",
        T0 + 6,
    );
    assert_accepted(&m.ingest(rejoin));
    assert!(m.snapshot().is_active(&bob.identity()));
}

#[test]
fn removal_consumes_invite() {
    rejoin_after_departure_is_blocked(true);
}

#[test]
fn leave_consumes_invite() {
    rejoin_after_departure_is_blocked(false);
}

// --------------------------------------------------------------------------
// AC5 — concurrent join vs kick converges to Removed (vector §11/§18)
// --------------------------------------------------------------------------

#[test]
fn concurrent_join_kick_converges_to_removed() {
    let alice = Principal::new(0x01);
    let dave = Principal::new(0x40);
    let (gen_ev, room) = genesis(&alice);

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        dave.identity(),
        "member",
        None,
        T0 + 1,
    );
    // Two concurrent children of the invite: Dave joins, Alice kicks.
    let join_dave = join(
        &dave,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    let kick_dave = remove(&alice, room, &[inv.event_id], dave.identity(), T0 + 2);

    let all = [gen_ev, inv, join_dave, kick_dave];

    // Feed the identical set in several orders; assert byte-identical snapshots.
    let orders: [[usize; 4]; 4] = [[0, 1, 2, 3], [0, 1, 3, 2], [3, 2, 1, 0], [2, 3, 1, 0]];
    let mut snapshots: Vec<MembershipSnapshot> = Vec::new();
    for order in orders {
        let mut m = RoomMembership::new(room);
        for &i in &order {
            m.ingest(all[i].clone());
        }
        let snap = m.snapshot();
        assert_eq!(
            snap.status(&dave.identity()),
            Some(Status::Removed),
            "Removed-dominates must hold for order {order:?}"
        );
        snapshots.push(snap);
    }
    for pair in snapshots.windows(2) {
        assert_eq!(
            pair[0], pair[1],
            "snapshots must be byte-identical across orders"
        );
    }
}

// --------------------------------------------------------------------------
// AC6 — current snapshot drives pipe/blob access (vectors §16/§17)
// --------------------------------------------------------------------------

#[test]
fn snapshot_drives_blob_and_pipe_access() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let dave = Principal::new(0x40);
    let mal = Principal::new(0x30);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    let mut prev = id_of(&m.ingest(gen_ev.clone()));

    // Bring Bob, Carol, Dave to Active; then remove Dave.
    for (i, p) in [&bob, &carol, &dave].into_iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let tag = 0x10 + i as u8;
        let iv = [tag; 16];
        let sec = [tag.wrapping_add(0x50); 16];
        let inv = invite(
            &alice,
            room,
            &[prev],
            iv,
            sec,
            p.identity(),
            "member",
            None,
            T0 + 1,
        );
        m.ingest(inv.clone());
        let jn = join(p, room, &[inv.event_id], iv, sec, "member", T0 + 2);
        prev = id_of(&m.ingest(jn));
    }
    let kick = remove(&alice, room, &[prev], dave.identity(), T0 + 10);
    prev = id_of(&m.ingest(kick));

    // Bob shares a file.
    let blob_ref = HashRef::from_bytes([0xbe; 32]);
    let share = file_shared(&bob, room, &[prev], blob_ref, T0 + 11);
    m.ingest(share);

    let snap = m.snapshot();
    assert_eq!(snap.status(&dave.identity()), Some(Status::Removed));

    // file_shares resolver: the blob was shared by Bob.
    let bob_id = bob.identity();
    let shares = move |h: &HashRef| -> Option<IdentityKey> {
        if *h == blob_ref {
            Some(bob_id)
        } else {
            None
        }
    };

    // Blob: Carol (Active) serves; Dave (Removed) and Mallory (unknown) rejected.
    assert_eq!(
        blob_serve_allowed(&snap, &carol.device(), &blob_ref, &shares),
        BlobDecision::Serve
    );
    assert_eq!(
        blob_serve_allowed(&snap, &dave.device(), &blob_ref, &shares),
        BlobDecision::Reject(DenyReason::NotActive)
    );
    assert_eq!(
        blob_serve_allowed(&snap, &mal.device(), &blob_ref, &shares),
        BlobDecision::Reject(DenyReason::UnknownDevice)
    );
    // An unreferenced hash → rejected even for an Active member.
    let other = HashRef::from_bytes([0xab; 32]);
    assert_eq!(
        blob_serve_allowed(&snap, &carol.device(), &other, &shares),
        BlobDecision::Reject(DenyReason::Unshared)
    );

    // Pipe owned by Bob, allowed {alice, bob}.
    let pipe = PipeOpened {
        pipe_id: [0x01; 16],
        owner_id: bob.identity(),
        owner_endpoint: bob.device(),
        kind: "tcp".to_owned(),
        label: "l".to_owned(),
        target_hint: "127.0.0.1".to_owned(),
        alpn: "iroh/rooms/1".to_owned(),
        allowed_members: vec![alice.identity(), bob.identity()],
        expires_at: None,
    };
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &pipe, None),
        PipeDecision::Accept
    );
    // Carol is Active but not in allowed_members (no default-all).
    assert_eq!(
        pipe_connect_allowed(&snap, &carol.device(), &pipe, None),
        PipeDecision::Reject(DenyReason::NotAllowed)
    );
    assert_eq!(
        pipe_connect_allowed(&snap, &dave.device(), &pipe, None),
        PipeDecision::Reject(DenyReason::NotActive)
    );
    assert_eq!(
        pipe_connect_allowed(&snap, &mal.device(), &pipe, None),
        PipeDecision::Reject(DenyReason::UnknownDevice)
    );
}

// --------------------------------------------------------------------------
// Non-member event rejection (vector §13)
// --------------------------------------------------------------------------

#[test]
fn non_member_message_rejected() {
    let alice = Principal::new(0x01);
    let mal = Principal::new(0x30);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let msg = message(
        &mal,
        &mal.dev,
        room,
        &[gen_ev.event_id],
        "intrusion",
        T0 + 1,
    );
    assert_rejected(&m.ingest(msg), &RejectReason::NotAMember);
    assert!(m.snapshot().member(&mal.identity()).is_none());
}

// --------------------------------------------------------------------------
// Log-validity vs access-control split (vector §16 + §5)
// --------------------------------------------------------------------------

#[test]
fn log_valid_share_denies_access_after_removal() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob shares while Active → log-valid (Accepted).
    let blob_ref = HashRef::from_bytes([0xbe; 32]);
    let share = file_shared(&bob, room, &[jn.event_id], blob_ref, T0 + 3);
    assert_accepted(&m.ingest(share.clone()));

    // Bob is later removed.
    let kick = remove(&alice, room, &[share.event_id], bob.identity(), T0 + 4);
    m.ingest(kick);

    // The share remains in the log, but access now denies (current snapshot).
    let snap = m.snapshot();
    let bob_id = bob.identity();
    let shares = move |h: &HashRef| -> Option<IdentityKey> { (*h == blob_ref).then_some(bob_id) };
    assert_eq!(
        blob_serve_allowed(&snap, &bob.device(), &blob_ref, &shares),
        BlobDecision::Reject(DenyReason::NotActive)
    );
    // Even a still-Active member cannot fetch a blob shared by a now-removed member.
    assert_eq!(
        blob_serve_allowed(&snap, &alice.device(), &blob_ref, &shares),
        BlobDecision::Reject(DenyReason::Unshared)
    );
}

// --------------------------------------------------------------------------
// bound_device enforcement
// --------------------------------------------------------------------------

#[test]
fn wrong_device_yields_unbound_device() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob authors a message under a DIFFERENT device key (not the bound one).
    let rogue = sk(0xee);
    let msg = message(&bob, &rogue, room, &[jn.event_id], "spoof", T0 + 3);
    assert_rejected(&m.ingest(msg), &RejectReason::UnboundDevice);
}

// --------------------------------------------------------------------------
// Determinism: shuffled bulk fold == incremental fold; idempotent replay
// --------------------------------------------------------------------------

#[test]
fn determinism_and_stale_replay() {
    let alice = Principal::new(0x01);
    let dave = Principal::new(0x40);
    let (gen_ev, room) = genesis(&alice);
    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        dave.identity(),
        "member",
        None,
        T0 + 1,
    );
    let join_dave = join(
        &dave,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    let kick_dave = remove(&alice, room, &[inv.event_id], dave.identity(), T0 + 2);
    let set = vec![
        gen_ev.clone(),
        inv.clone(),
        join_dave.clone(),
        kick_dave.clone(),
    ];

    // Incremental in a buffer-forcing order (children before parents).
    let mut incremental = RoomMembership::new(room);
    for ev in [&kick_dave, &join_dave, &inv, &gen_ev] {
        incremental.ingest(ev.clone());
    }

    // Bulk via from_events.
    let bulk = RoomMembership::from_events(room, set.clone());

    assert_eq!(incremental.snapshot(), bulk.snapshot());

    // Stale replay of the invite and join after kick does not resurrect Dave.
    let mut m = RoomMembership::from_events(room, set);
    assert_eq!(m.snapshot().status(&dave.identity()), Some(Status::Removed));
    m.ingest(inv);
    m.ingest(join_dave);
    assert_eq!(m.snapshot().status(&dave.identity()), Some(Status::Removed));
}

// --------------------------------------------------------------------------
// Out-of-order buffering reports the missing parent
// --------------------------------------------------------------------------

#[test]
fn buffers_event_with_missing_parent() {
    let alice = Principal::new(0x01);
    let (gen_ev, room) = genesis(&alice);
    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        Principal::new(0x10).identity(),
        "member",
        None,
        T0 + 1,
    );

    let mut m = RoomMembership::new(room);
    // Ingest the invite before its genesis parent.
    match m.ingest(inv.clone()) {
        Ingest::Buffered { missing, .. } => assert_eq!(missing, vec![gen_ev.event_id]),
        other => panic!("expected Buffered, got {other:?}"),
    }
    // Genesis arrives → invite is re-evaluated and accepted.
    m.ingest(gen_ev);
    assert_accepted(&m.ingest(inv));
}

// --------------------------------------------------------------------------
// Status and Role ordering invariants (Removed-dominates / least-privilege)
// --------------------------------------------------------------------------

#[test]
fn status_and_role_ordering() {
    // Removed-dominates: Invited < Active < Removed.
    assert!(Status::Invited < Status::Active);
    assert!(Status::Active < Status::Removed);
    assert_eq!(Status::Active.max(Status::Removed), Status::Removed);
    assert_eq!(Status::Invited.max(Status::Removed), Status::Removed);
    assert_eq!(Status::Invited.max(Status::Active), Status::Active);

    // Least-privilege: Agent < Member < Admin.
    assert!(Role::Agent < Role::Member);
    assert!(Role::Member < Role::Admin);
    assert_eq!(Role::Member.min(Role::Agent), Role::Agent);
    assert_eq!(Role::Admin.min(Role::Member), Role::Member);
}

// --------------------------------------------------------------------------
// Empty fold: no admin, no members (from_events on the empty set)
// --------------------------------------------------------------------------

#[test]
fn empty_fold_has_no_admin_and_no_members() {
    let alice = Principal::new(0x01);
    let (_gen, room) = genesis(&alice);
    let snap = RoomMembership::from_events(room, []).snapshot();
    assert!(snap.admin().is_none());
    assert_eq!(snap.active_members().count(), 0);
    assert_eq!(snap.members().count(), 0);
}

// --------------------------------------------------------------------------
// Least-privilege role merge on concurrent invite heads (spec D5 step 4)
// --------------------------------------------------------------------------

#[test]
fn least_privilege_role_merge_on_concurrent_invites() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    // Two concurrent invites to Bob, each with a different role.
    let inv_member = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    let inv_agent = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x02; 16],
        [0x43; 16],
        bob.identity(),
        "agent",
        None,
        T0 + 2,
    );
    m.ingest(inv_member.clone());
    m.ingest(inv_agent.clone());

    let snap = m.snapshot();
    assert_eq!(snap.status(&bob.identity()), Some(Status::Invited));
    // Agent < Member: the least-privilege merge yields Agent.
    assert_eq!(snap.role(&bob.identity()), Some(Role::Agent));
}

// --------------------------------------------------------------------------
// Agent role: invite and join as "agent" (Role::Agent end-to-end)
// --------------------------------------------------------------------------

#[test]
fn agent_role_invite_and_join() {
    let alice = Principal::new(0x01);
    let agent = Principal::new(0x50);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        agent.identity(),
        "agent",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());

    let jn = join(
        &agent,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "agent",
        T0 + 2,
    );
    assert_accepted(&m.ingest(jn.clone()));

    let snap = m.snapshot();
    assert!(snap.is_active(&agent.identity()));
    assert_eq!(snap.role(&agent.identity()), Some(Role::Agent));

    // An Active agent can send messages.
    let msg = message(
        &agent,
        &agent.dev,
        room,
        &[jn.event_id],
        "status ok",
        T0 + 3,
    );
    assert_accepted(&m.ingest(msg));
}

// --------------------------------------------------------------------------
// Non-member self-departure is accepted but sets status to Removed
// (self-departure-always-valid rule, spec §6.2)
// --------------------------------------------------------------------------

#[test]
fn non_member_leave_accepted_and_sets_removed() {
    let alice = Principal::new(0x01);
    let mal = Principal::new(0x30);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    // Mallory is not a member. Self-departure is always valid (spec §6.2),
    // even with no prior membership events — the gate's MemberLeft arm returns Ok.
    let mal_leave = leave(&mal, room, &[gen_ev.event_id], T0 + 1);
    assert_accepted(&m.ingest(mal_leave));

    // The inert leave still sets the subject's status to Removed in the fold.
    assert_eq!(m.snapshot().status(&mal.identity()), Some(Status::Removed));
}

// --------------------------------------------------------------------------
// identity_of_device: reverse device→identity lookup on the snapshot
// --------------------------------------------------------------------------

#[test]
fn identity_of_device_lookup() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let outsider = Principal::new(0x99);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn);

    let snap = m.snapshot();
    // Admin device comes from genesis.
    assert_eq!(
        snap.identity_of_device(&alice.device()),
        Some(&alice.identity())
    );
    // Bob's device is bound by the join.
    assert_eq!(
        snap.identity_of_device(&bob.device()),
        Some(&bob.identity())
    );
    // An unregistered device returns None.
    assert_eq!(snap.identity_of_device(&outsider.device()), None);
}

// --------------------------------------------------------------------------
// active_members() and members() iterators on MembershipSnapshot
// --------------------------------------------------------------------------

#[test]
fn active_members_and_members_iterators() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    let g = id_of(&m.ingest(gen_ev.clone()));

    // Carol stays Invited; Bob joins then is removed.
    let carol_inv = invite(
        &alice,
        room,
        &[g],
        [0x01; 16],
        [0x42; 16],
        carol.identity(),
        "member",
        None,
        T0 + 1,
    );
    let ci = id_of(&m.ingest(carol_inv));
    let bob_inv = invite(
        &alice,
        room,
        &[ci],
        [0x02; 16],
        [0x43; 16],
        bob.identity(),
        "member",
        None,
        T0 + 2,
    );
    m.ingest(bob_inv.clone());
    let jn = join(
        &bob,
        room,
        &[bob_inv.event_id],
        [0x02; 16],
        [0x43; 16],
        "member",
        T0 + 3,
    );
    let bj = id_of(&m.ingest(jn));
    m.ingest(remove(&alice, room, &[bj], bob.identity(), T0 + 4));

    let snap = m.snapshot();

    // Only Alice is Active (Bob removed, Carol invited).
    let active: Vec<_> = snap.active_members().collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].identity, alice.identity());

    // All three (Alice, Bob, Carol) appear in members().
    assert_eq!(snap.members().count(), 3);
}

// --------------------------------------------------------------------------
// Pipe expiry and owner-inactive denial paths in pipe_connect_allowed
// --------------------------------------------------------------------------

#[test]
fn pipe_expired_and_owner_inactive_denied() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    let snap = m.snapshot();

    // Bob owns a pipe that expires at T0 + 1_000. Alice is in allowed_members.
    let expiring_pipe = PipeOpened {
        pipe_id: [0x02; 16],
        owner_id: bob.identity(),
        owner_endpoint: bob.device(),
        kind: "tcp".to_owned(),
        label: "lab".to_owned(),
        target_hint: "127.0.0.1".to_owned(),
        alpn: "iroh/rooms/1".to_owned(),
        allowed_members: vec![alice.identity()],
        expires_at: Some(T0 + 1_000),
    };

    // Before expiry and with no clock → both accept.
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &expiring_pipe, Some(T0 + 999)),
        PipeDecision::Accept
    );
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &expiring_pipe, None),
        PipeDecision::Accept
    );

    // Past expiry → Expired.
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &expiring_pipe, Some(T0 + 1_001)),
        PipeDecision::Reject(DenyReason::Expired)
    );

    // Now remove Bob. Owner becomes Inactive → OwnerInactive (checked before expiry).
    let kick = remove(&alice, room, &[jn.event_id], bob.identity(), T0 + 10);
    m.ingest(kick);
    let snap2 = m.snapshot();

    assert_eq!(
        pipe_connect_allowed(&snap2, &alice.device(), &expiring_pipe, None),
        PipeDecision::Reject(DenyReason::OwnerInactive)
    );
}

// --------------------------------------------------------------------------
// advisory_flags: FromRemovedMember added after author is kicked (spec §9)
// --------------------------------------------------------------------------

#[test]
fn advisory_flags_from_removed_member() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob sends a message while Active.
    let msg = message(&bob, &bob.dev, room, &[jn.event_id], "hi", T0 + 3);
    let res = m.ingest(msg.clone());
    assert_accepted(&res);
    let msg_id = id_of(&res);

    // Before removal: no FromRemovedMember flag.
    assert!(!m.advisory_flags(&msg_id).contains(&Flag::FromRemovedMember));

    // Bob is kicked.
    let kick = remove(&alice, room, &[msg.event_id], bob.identity(), T0 + 4);
    m.ingest(kick);

    // After removal: advisory_flags adds FromRemovedMember retroactively.
    assert!(m.advisory_flags(&msg_id).contains(&Flag::FromRemovedMember));

    // Unknown event id returns an empty flag list.
    let unknown = EventId::from_bytes([0xff; 32]);
    assert!(m.advisory_flags(&unknown).is_empty());
}

// --------------------------------------------------------------------------
// Equivocation advisory flag on concurrent events from the same sender (§9)
// --------------------------------------------------------------------------

#[test]
fn equivocation_flag_on_concurrent_events() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Two concurrent messages from Bob: both cite jn as their single prev,
    // so neither is an ancestor of the other.
    let msg_a = message(&bob, &bob.dev, room, &[jn.event_id], "alpha", T0 + 3);
    let msg_b = message(&bob, &bob.dev, room, &[jn.event_id], "beta", T0 + 4);

    let res_a = m.ingest(msg_a);
    let res_b = m.ingest(msg_b);

    // Both are accepted.
    assert_accepted(&res_a);
    assert_accepted(&res_b);

    // msg_a was classified first: no concurrent peer existed yet → no Equivocation.
    let Ingest::Accepted { flags: flags_a, .. } = res_a else {
        panic!("expected Accepted");
    };
    assert!(
        !flags_a.contains(&Flag::Equivocation),
        "first event must not be tagged"
    );

    // msg_b classified second: msg_a is a concurrent accepted event → Equivocation.
    let Ingest::Accepted { flags: flags_b, .. } = res_b else {
        panic!("expected Accepted");
    };
    assert!(
        flags_b.contains(&Flag::Equivocation),
        "second concurrent event must be tagged"
    );
}

// --------------------------------------------------------------------------
// validate_with_membership via the ancestor view (frozen-surface entry)
// --------------------------------------------------------------------------

#[test]
fn validate_with_membership_uses_ancestor_view() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());
    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // A message by Bob descending from his join: re-validate via the join's view.
    let bob_msg = message(&bob, &bob.dev, room, &[jn.event_id], "hi", T0 + 3);
    let view = m.ancestor_view(&bob_msg.event_id);
    // Not yet ingested → no view.
    assert!(view.is_none());

    m.ingest(bob_msg.clone());
    let view = m
        .ancestor_view(&bob_msg.event_id)
        .expect("view after ingest");
    let bytes = bob_msg.wire.to_bytes();
    let revalidated = iroh_rooms_core::event::validate_with_membership(
        &bytes,
        &ValidationContext::for_room(room),
        &view,
    );
    assert!(
        revalidated.is_ok(),
        "active member's message must re-validate"
    );
}

// --------------------------------------------------------------------------
// Re-ingesting an already-rejected event is idempotent (no state mutation)
// --------------------------------------------------------------------------

#[test]
fn idempotent_reingest_rejected_event() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob (non-admin) tries to remove Carol → rejected.
    let bad_remove = remove(&bob, room, &[jn.event_id], carol.identity(), T0 + 3);
    let first = m.ingest(bad_remove.clone());
    assert_rejected(&first, &RejectReason::InsufficientRole);

    // Re-ingesting the same rejected event must return the same Rejected result.
    let second = m.ingest(bad_remove);
    assert_rejected(&second, &RejectReason::InsufficientRole);

    // Carol's status is None — neither rejected event mutated the fold.
    assert_eq!(m.snapshot().status(&carol.identity()), None);
    // Bob is still Active — the rejected remove did not change his status.
    assert!(m.snapshot().is_active(&bob.identity()));
}

// --------------------------------------------------------------------------
// Three-level buffering cascade: genesis → invite → join ingested in reverse
// --------------------------------------------------------------------------

#[test]
fn three_level_buffering_chain_resolves() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );

    let mut m = RoomMembership::new(room);

    // Deepest event first: join is missing its parent invite.
    match m.ingest(jn.clone()) {
        Ingest::Buffered { missing, .. } => assert_eq!(missing, vec![inv.event_id]),
        other => panic!("join should be Buffered, got {other:?}"),
    }
    // Middle event: invite is missing its parent genesis.
    match m.ingest(inv.clone()) {
        Ingest::Buffered { missing, .. } => assert_eq!(missing, vec![gen_ev.event_id]),
        other => panic!("invite should be Buffered, got {other:?}"),
    }
    // Root arrives: genesis accepted, then invite and join cascade to Accepted.
    assert_accepted(&m.ingest(gen_ev));

    // All three are now classified; Bob is Active.
    assert!(m.snapshot().is_active(&alice.identity()));
    assert_eq!(m.snapshot().status(&bob.identity()), Some(Status::Active));
}

// --------------------------------------------------------------------------
// ancestor_view for an ingested-but-rejected event returns Some
// (the ancestor scope reflects only that event's accepted parents)
// --------------------------------------------------------------------------

#[test]
fn ancestor_view_for_rejected_event_returns_some() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Bob (non-admin) tries to invite Carol → log-rejected.
    let bad_invite = invite(
        &bob,
        room,
        &[jn.event_id],
        [0x02; 16],
        [0x43; 16],
        carol.identity(),
        "member",
        None,
        T0 + 3,
    );
    let result = m.ingest(bad_invite.clone());
    assert_rejected(&result, &RejectReason::InsufficientRole);
    let rejected_id = id_of(&result);

    // ancestor_view must return Some even for a rejected event.
    let view = m
        .ancestor_view(&rejected_id)
        .expect("ancestor_view must be Some for an ingested-but-rejected event");

    // The snapshot inside reflects the rejected event's accepted ancestors:
    // alice = Admin/Active, bob = Active, carol = unknown (not in ancestor scope).
    let snap = view.snapshot();
    assert!(snap.is_active(&alice.identity()));
    assert!(snap.is_active(&bob.identity()));
    assert_eq!(snap.status(&carol.identity()), None);
}

// --------------------------------------------------------------------------
// pipe_connect_allowed: now_ms == expires_at is still accepted (strictly > check)
// --------------------------------------------------------------------------

#[test]
fn pipe_connect_at_exact_expiry_is_accepted() {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    let snap = m.snapshot();
    let expiry = T0 + 500;

    let pipe = PipeOpened {
        pipe_id: [0x04; 16],
        owner_id: bob.identity(),
        owner_endpoint: bob.device(),
        kind: "tcp".to_owned(),
        label: "lab".to_owned(),
        target_hint: "127.0.0.1".to_owned(),
        alpn: "iroh/rooms/1".to_owned(),
        allowed_members: vec![alice.identity()],
        expires_at: Some(expiry),
    };

    // At exactly expiry: `now > expiry` is false → Accept (strictly-greater check).
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &pipe, Some(expiry)),
        PipeDecision::Accept,
        "now_ms == expiry must still be accepted (check is now > expiry, not >=)"
    );
    // One millisecond after expiry → Expired.
    assert_eq!(
        pipe_connect_allowed(&snap, &alice.device(), &pipe, Some(expiry + 1)),
        PipeDecision::Reject(DenyReason::Expired),
        "now_ms > expiry must be rejected"
    );
}

// --------------------------------------------------------------------------
// AncestorView::authorize and ::bound_device return errors for wrong room_id
// --------------------------------------------------------------------------

#[test]
fn ancestor_view_wrong_room_id_fails_closed() {
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::event::reject::MembershipOracle;

    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        bob.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());
    let jn = join(
        &bob,
        room,
        &[inv.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(jn.clone());

    // Use a message Bob sends after joining; its ancestor scope includes the join,
    // so Bob is Active in that view.
    let msg = message(&bob, &bob.dev, room, &[jn.event_id], "hi", T0 + 3);
    m.ingest(msg.clone());
    let view = m
        .ancestor_view(&msg.event_id)
        .expect("view for accepted message");

    // A mismatched room_id: authorize must fail closed.
    let wrong_room = RoomId::from_bytes([0xff; 32]);
    assert_eq!(
        view.authorize(&wrong_room, &bob.identity(), "message.text"),
        Err(RejectReason::NotAMember),
        "authorize with wrong room_id must return NotAMember"
    );
    // bound_device with wrong room_id must return None.
    assert_eq!(
        view.bound_device(&wrong_room, &bob.identity()),
        None,
        "bound_device with wrong room_id must return None"
    );

    // The correct room_id: Bob is Active in his ancestor view → authorize succeeds.
    assert!(
        view.authorize(&room, &bob.identity(), "message.text")
            .is_ok(),
        "correct room_id + active member must authorize"
    );
    // bound_device with correct room_id returns Bob's device bytes.
    assert!(
        view.bound_device(&room, &bob.identity()).is_some(),
        "correct room_id must resolve Bob's bound device"
    );
}

// --------------------------------------------------------------------------
// Agent identity (IR-0206): an agent is an ordinary principal, admitted only by
// an explicit admin invite + join, and never implicitly trusted. These pin the
// four ACs at the protocol layer; the agent noun's CLI surface is a pure façade
// over the same events (see iroh-rooms-cli/tests/agent_cli.rs).
// --------------------------------------------------------------------------

/// AC3 (no implicit access, fold layer): an agent-key content event whose ancestor
/// view holds **no** live invite for that key is rejected `NotAMember`. The parent
/// is present (genesis), so the rejection is for non-membership, not a missing
/// parent (per the `member-message-ancestor-view-gate` guidance). The agent leaves
/// no trace in membership state.
#[test]
fn uninvited_agent_content_rejected_not_a_member() {
    let alice = Principal::new(0x01);
    let agent = Principal::new(0x50);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    // The agent — never invited — tries to post, citing the real genesis head.
    let msg = message(
        &agent,
        &agent.dev,
        room,
        &[gen_ev.event_id],
        "agent status: online",
        T0 + 1,
    );
    assert_rejected(&m.ingest(msg), &RejectReason::NotAMember);
    assert!(
        m.snapshot().member(&agent.identity()).is_none(),
        "an un-invited agent must not appear in membership state"
    );
}

/// AC3 (invite ≠ access): a live admin invite alone does **not** grant an agent the
/// right to author content. An agent that is `Invited` but has never `joined`
/// authors a message citing its own invite; the fold rejects it `NotAMember`
/// because the author is not `Active` in the ancestor view. The agent remains
/// `Invited` with role `Agent` and no bound device — proving the join step is a
/// hard gate, not a formality.
#[test]
fn invited_but_unjoined_agent_cannot_post() {
    let alice = Principal::new(0x01);
    let agent = Principal::new(0x50);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    let inv = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        agent.identity(),
        "agent",
        None,
        T0 + 1,
    );
    m.ingest(inv.clone());

    // Agent has an invite but never joined → still not Active.
    let snap = m.snapshot();
    assert_eq!(snap.status(&agent.identity()), Some(Status::Invited));
    assert_eq!(snap.role(&agent.identity()), Some(Role::Agent));
    assert_eq!(
        snap.member(&agent.identity()).and_then(|mem| mem.device),
        None,
        "an invited-not-joined agent has no bound device"
    );

    // Posting content while merely Invited is rejected for non-membership,
    // authorization failing before any device-binding check.
    let msg = message(
        &agent,
        &agent.dev,
        room,
        &[inv.event_id],
        "premature",
        T0 + 2,
    );
    assert_rejected(&m.ingest(msg), &RejectReason::NotAMember);
}

/// AC4 (one protocol model): a human member and an agent, each admitted through the
/// identical `member.invited` → `member.joined` path, resolve to `Member` records
/// that differ **only** in `role`. Both are `Active`, both carry their own identity
/// and a bound device — the agent is a first-class participant on the same
/// substrate, distinguished by the role string alone.
#[test]
fn agent_and_human_members_differ_only_by_role() {
    let alice = Principal::new(0x01);
    let human = Principal::new(0x10);
    let agent = Principal::new(0x50);
    let (gen_ev, room) = genesis(&alice);
    let mut m = RoomMembership::new(room);
    m.ingest(gen_ev.clone());

    // Admit the human as `member`.
    let inv_h = invite(
        &alice,
        room,
        &[gen_ev.event_id],
        [0x01; 16],
        [0x42; 16],
        human.identity(),
        "member",
        None,
        T0 + 1,
    );
    m.ingest(inv_h.clone());
    let join_h = join(
        &human,
        room,
        &[inv_h.event_id],
        [0x01; 16],
        [0x42; 16],
        "member",
        T0 + 2,
    );
    m.ingest(join_h.clone());

    // Admit the agent as `agent` through the identical path.
    let inv_a = invite(
        &alice,
        room,
        &[join_h.event_id],
        [0x02; 16],
        [0x43; 16],
        agent.identity(),
        "agent",
        None,
        T0 + 3,
    );
    m.ingest(inv_a.clone());
    let join_a = join(
        &agent,
        room,
        &[inv_a.event_id],
        [0x02; 16],
        [0x43; 16],
        "agent",
        T0 + 4,
    );
    m.ingest(join_a);

    let snap = m.snapshot();
    let human_member = snap.member(&human.identity()).expect("human is a member");
    let agent_member = snap.member(&agent.identity()).expect("agent is a member");

    // The role is the sole distinguishing attribute of the resolved records.
    assert_eq!(human_member.role, Role::Member);
    assert_eq!(agent_member.role, Role::Agent);
    assert_ne!(human_member.role, agent_member.role);

    // Every *other* structural field is the same kind for both: Active status and a
    // present, principal-specific bound device.
    assert_eq!(human_member.status, Status::Active);
    assert_eq!(agent_member.status, Status::Active);
    assert_eq!(human_member.status, agent_member.status);
    assert_eq!(human_member.device, Some(human.device()));
    assert_eq!(agent_member.device, Some(agent.device()));
    assert_eq!(human_member.identity, human.identity());
    assert_eq!(agent_member.identity, agent.identity());

    // Normalizing the role away, the two records become field-for-field identical
    // in shape (same status + both device-bound) — the "same protocol model" (AC4).
    assert!(
        human_member.status == agent_member.status
            && human_member.device.is_some()
            && agent_member.device.is_some(),
        "human and agent members share one model, differing only by role"
    );
}
