//! `pipe.closed` is attributed by its signed cleartext sender at query time
//! (Refs #429). The membership fold still accepts a close from any Active
//! member — plaintext, readable encrypted, or unreadable — so the DAG does not
//! depend on who holds the epoch key. `pipe_is_closed` counts it only when
//! that sender is the governing `pipe.opened`'s owner or the room admin.
//!
//! An unreadable close follows the same sender rule: the admin's closes every
//! pipe, an owner's closes only that owner's pipes, and anyone else's is
//! ignored. A pipe id with no governing `pipe.opened` keeps the previous
//! lookup (the connect gate already denies it).
//!
//! These tests fail on a build that treats every close, or every unreadable
//! close, as closing the named pipe.

#![cfg(all(feature = "sync", feature = "store"))]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::constants::{ENCRYPTED_NONCE_LEN, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, PipeClosed, PipeOpened,
    RoomCreated,
};
use iroh_rooms_core::event::encrypted::build_content_encrypted;
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine};
use iroh_rooms_crypto::RoomKey;

const ROOM_NONCE: [u8; 16] = [0xB4; 16];
const T0: u64 = 1_750_000_100_000;
const KEY_EPOCH: u64 = 4;
const PIPE_B: [u8; SHORT_ID_LEN] = [0xB0; 16];
const PIPE_B2: [u8; SHORT_ID_LEN] = [0xB2; 16];
const PIPE_C: [u8; SHORT_ID_LEN] = [0xC0; 16];
const NEVER_OPENED: [u8; SHORT_ID_LEN] = [0x00; 16];
const REMOTE: PeerId = PeerId::from_bytes([0xEE; 32]);

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

    fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }

    fn device(&self) -> iroh_rooms_core::event::keys::DeviceKey {
        self.dev.device_key()
    }
}

struct Cast {
    alice: Principal,
    bob: Principal,
    carol: Principal,
    room: RoomId,
}

fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

fn foreign_key() -> RoomKey {
    RoomKey::from_bytes([0xEE; 32])
}

fn unrelated_key() -> RoomKey {
    RoomKey::from_bytes([0x11; 32])
}

fn opening_key() -> RoomKey {
    RoomKey::from_bytes([0x42; 32])
}

fn engine() -> SyncEngine {
    let room = signed::derive_room_id(&Principal::new(0x01).identity(), &ROOM_NONCE, T0);
    let store = EventStore::open_in_memory().expect("in-memory store");
    SyncEngine::open(store, room, SyncConfig::default()).expect("open engine")
}

fn publish(engine: &mut SyncEngine, ev: &SignedEvent, dev: &SigningKey, what: &str) -> EventId {
    let id = ev.event_id();
    engine
        .publish(&seal(ev, dev))
        .unwrap_or_else(|e| panic!("{what} failed stateless validation: {e}"));
    assert!(
        engine.digest().expect("digest").event_ids.contains(&id),
        "{what} was not fold-accepted"
    );
    id
}

fn ingest(engine: &mut SyncEngine, bytes: &[u8], id: EventId, what: &str) {
    engine.ingest_frame(REMOTE, bytes);
    assert!(
        engine.digest().expect("digest").event_ids.contains(&id),
        "{what} was not fold-accepted"
    );
}

const LOG_UNREADABLE: &str = "pipe_close_unreadable";
const LOG_UNATTRIBUTED: &str = "pipe_close_unattributed";

fn close_log_lines(engine: &SyncEngine, label: &str) -> Vec<String> {
    let prefix = format!("{label} ");
    engine
        .logs()
        .iter()
        .filter(|line| line.starts_with(&prefix))
        .cloned()
        .collect()
}

fn close_log_line(label: &str, event: EventId, sender: IdentityKey) -> String {
    format!("{label} event={event} sender={sender}")
}

/// Genesis plus Bob and Carol joined. Alice is the admin and is not a pipe
/// owner in these tests, so an admin close and an owner close are different
/// senders.
fn seeded() -> (SyncEngine, Cast, EventId) {
    let alice = Principal::new(0x01);
    let bob = Principal::new(0x10);
    let carol = Principal::new(0x20);
    let room = signed::derive_room_id(&alice.identity(), &ROOM_NONCE, T0);
    let mut engine = engine();

    let genesis = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: alice.identity(),
        device_id: alice.device(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Pipe close attribution".to_owned(),
            room_nonce: ROOM_NONCE,
            admins: vec![alice.identity()],
            device_binding: DeviceBinding::create(&room, &alice.id, alice.device()),
        }),
    };
    let mut head = publish(&mut engine, &genesis, &alice.dev, "genesis");
    head = invite_and_join(&mut engine, &alice, &bob, head, 0x01, T0 + 1);
    head = invite_and_join(&mut engine, &alice, &carol, head, 0x02, T0 + 10);

    (
        engine,
        Cast {
            alice,
            bob,
            carol,
            room,
        },
        head,
    )
}

fn invite_and_join(
    engine: &mut SyncEngine,
    admin: &Principal,
    member: &Principal,
    prev: EventId,
    invite_seed: u8,
    created_at: u64,
) -> EventId {
    let room = signed::derive_room_id(&admin.identity(), &ROOM_NONCE, T0);
    let invite_id = [invite_seed; 16];
    let secret = [invite_seed.wrapping_add(0x40); 16];
    let invite = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: admin.identity(),
        device_id: admin.device(),
        event_type: EventType::MemberInvited,
        created_at,
        prev_events: vec![prev],
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: capability_hash(&room, &invite_id, &secret),
            role: "member".to_owned(),
            invitee_key: member.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite_event = publish(engine, &invite, &admin.dev, "member.invited");
    let join = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: member.identity(),
        device_id: member.device(),
        event_type: EventType::MemberJoined,
        created_at: created_at + 1,
        prev_events: vec![invite_event],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: secret,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room, &member.id, member.device()),
            display_name: None,
        }),
    };
    publish(engine, &join, &member.dev, "member.joined")
}

fn open_pipe(
    engine: &mut SyncEngine,
    owner: &Principal,
    room: RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    prev: EventId,
    created_at: u64,
    label: &str,
) -> EventId {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: owner.identity(),
        device_id: owner.device(),
        event_type: EventType::PipeOpened,
        created_at,
        prev_events: vec![prev],
        content: Content::PipeOpened(PipeOpened {
            pipe_id,
            owner_id: owner.identity(),
            owner_endpoint: owner.device(),
            kind: "tcp".to_owned(),
            label: label.to_owned(),
            target_hint: "localhost:9".to_owned(),
            alpn: "iroh-rooms/pipe/0".to_owned(),
            allowed_members: vec![owner.identity()],
            expires_at: None,
        }),
    };
    publish(engine, &ev, &owner.dev, "pipe.opened")
}

fn plaintext_close(
    engine: &mut SyncEngine,
    signer: &Principal,
    room: RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    prev: EventId,
    created_at: u64,
) -> EventId {
    let ev = SignedEvent {
        schema_version: 1,
        room_id: room,
        sender_id: signer.identity(),
        device_id: signer.device(),
        event_type: EventType::PipeClosed,
        created_at,
        prev_events: vec![prev],
        content: Content::PipeClosed(PipeClosed {
            pipe_id,
            reason: Some("closed".to_owned()),
        }),
    };
    publish(engine, &ev, &signer.dev, "pipe.closed")
}

fn encrypted_close_bytes(
    signer: &Principal,
    room: RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    key: &RoomKey,
    nonce_tag: u8,
    parent: EventId,
    created_at: u64,
) -> (EventId, Vec<u8>) {
    let mut nonce = [0xC0; ENCRYPTED_NONCE_LEN];
    nonce[ENCRYPTED_NONCE_LEN - 1] = nonce_tag;
    let wire = build_content_encrypted(
        &signer.id,
        &signer.dev,
        &room,
        &Content::PipeClosed(PipeClosed {
            pipe_id,
            reason: Some("closed".to_owned()),
        }),
        KEY_EPOCH,
        key,
        &nonce,
        &[parent],
        created_at,
    )
    .expect("seal pipe.closed");
    let id = signed::event_id_from_bytes(&wire.signed);
    (id, wire.to_bytes())
}

#[allow(clippy::too_many_arguments)] // one field per sealed input, same shape as the builder
fn ingest_encrypted_close(
    engine: &mut SyncEngine,
    signer: &Principal,
    room: RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    key: &RoomKey,
    nonce_tag: u8,
    parent: EventId,
    created_at: u64,
) -> EventId {
    let (id, bytes) =
        encrypted_close_bytes(signer, room, pipe_id, key, nonce_tag, parent, created_at);
    ingest(engine, &bytes, id, "encrypted pipe.closed");
    id
}

fn assert_logged_once_and_not_per_tick(
    engine: &mut SyncEngine,
    label: &str,
    event: EventId,
    sender: IdentityKey,
) {
    let expected = vec![close_log_line(label, event, sender)];
    assert_eq!(
        close_log_lines(engine, label),
        expected,
        "{label} is logged once at insert"
    );
    let _ = engine.pipe_is_closed(&PIPE_B).expect("closed?");
    let _ = engine.pipe_is_closed(&PIPE_C).expect("closed?");
    let _ = engine.on_tick(0);
    assert_eq!(
        close_log_lines(engine, label),
        expected,
        "querying pipe_is_closed or ticking must not log the close again"
    );
}

/// B-1. Carol is Active, not the owner, and not the admin. Her encrypted
/// `pipe.closed` for Bob's pipe leaves that pipe open whether this node holds
/// no epoch key or holds a key that cannot open the ciphertext.
#[test]
fn non_owner_unreadable_close_leaves_owners_pipe_open() {
    for hold_unrelated_key in [false, true] {
        let (mut engine, cast, head) = seeded();
        if hold_unrelated_key {
            engine
                .insert_room_key(KEY_EPOCH, &unrelated_key())
                .expect("unrelated key");
        }
        let opened = open_pipe(
            &mut engine,
            &cast.bob,
            cast.room,
            PIPE_B,
            head,
            T0 + 30,
            "bob",
        );
        assert!(!engine.pipe_is_closed(&PIPE_B).expect("closed?"));

        let (id, bytes) = encrypted_close_bytes(
            &cast.carol,
            cast.room,
            PIPE_B,
            &foreign_key(),
            u8::from(hold_unrelated_key),
            opened,
            T0 + 40,
        );
        ingest(&mut engine, &bytes, id, "carol unreadable close");

        assert!(
            !engine.pipe_is_closed(&PIPE_B).expect("closed?"),
            "hold_unrelated_key={hold_unrelated_key}: Carol's unreadable close must leave Bob's pipe open"
        );
        assert!(
            engine.pipe_is_closed(&NEVER_OPENED).expect("closed?"),
            "a pipe id with no pipe.opened keeps the previous lookup"
        );
        assert_logged_once_and_not_per_tick(&mut engine, LOG_UNREADABLE, id, cast.carol.identity());
        ingest(&mut engine, &bytes, id, "duplicate unreadable close");
        assert_eq!(
            close_log_lines(&engine, LOG_UNREADABLE),
            vec![close_log_line(LOG_UNREADABLE, id, cast.carol.identity())],
            "a duplicate insert must not log the close again"
        );
    }
}

/// B-2. Carol's plaintext `pipe.closed` for Bob's pipe does not close it.
#[test]
fn non_owner_plaintext_close_leaves_owners_pipe_open() {
    let (mut engine, cast, head) = seeded();
    let opened = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "bob",
    );
    let id = plaintext_close(&mut engine, &cast.carol, cast.room, PIPE_B, opened, T0 + 40);

    assert!(
        !engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "Carol's plaintext close must leave Bob's pipe open"
    );
    assert!(
        engine.pipe_opened(&PIPE_B).expect("lookup").is_some(),
        "the open stays known"
    );
    assert_logged_once_and_not_per_tick(&mut engine, LOG_UNATTRIBUTED, id, cast.carol.identity());
}

/// B-3. An unreadable admin close covers every pipe. An unreadable owner close
/// covers only that owner's pipes. A pipe id with no open is unchanged.
#[test]
fn unreadable_close_is_scoped_to_admin_or_that_owner() {
    let (mut engine, cast, head) = seeded();
    let bob_open = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "bob",
    );
    let bob_second = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B2,
        bob_open,
        T0 + 31,
        "bob-2",
    );
    let carol_open = open_pipe(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_C,
        bob_second,
        T0 + 32,
        "carol",
    );

    let bob_close = ingest_encrypted_close(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_C,
        &foreign_key(),
        1,
        carol_open,
        T0 + 40,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "Bob's unreadable close closes his own pipe"
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B2).expect("closed?"),
        "Bob's unreadable close closes every pipe he owns"
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_C).expect("closed?"),
        "Bob's unreadable close must not close Carol's pipe"
    );
    assert!(
        engine.pipe_is_closed(&NEVER_OPENED).expect("closed?"),
        "a pipe id with no pipe.opened keeps the previous lookup"
    );
    assert_eq!(
        close_log_lines(&engine, LOG_UNREADABLE),
        vec![close_log_line(
            LOG_UNREADABLE,
            bob_close,
            cast.bob.identity()
        )]
    );

    let alice_close = ingest_encrypted_close(
        &mut engine,
        &cast.alice,
        cast.room,
        PIPE_B,
        &foreign_key(),
        2,
        carol_open,
        T0 + 41,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B).expect("closed?")
            && engine.pipe_is_closed(&PIPE_B2).expect("closed?")
            && engine.pipe_is_closed(&PIPE_C).expect("closed?")
            && engine.pipe_is_closed(&NEVER_OPENED).expect("closed?"),
        "an unreadable close from the admin closes every pipe"
    );
    assert_eq!(
        close_log_lines(&engine, LOG_UNREADABLE),
        vec![
            close_log_line(LOG_UNREADABLE, bob_close, cast.bob.identity()),
            close_log_line(LOG_UNREADABLE, alice_close, cast.alice.identity()),
        ]
    );
    let _ = engine.pipe_is_closed(&PIPE_C);
    let _ = engine.on_tick(0);
    assert_eq!(close_log_lines(&engine, LOG_UNREADABLE).len(), 2);
}

/// B-3 continued. A readable encrypted close from a non-owner is ignored. A
/// readable encrypted close from the owner is honoured and is not logged.
#[test]
fn readable_non_owner_close_is_ignored_and_owner_close_is_honoured() {
    let (mut engine, cast, head) = seeded();
    engine
        .insert_room_key(KEY_EPOCH, &opening_key())
        .expect("key");
    let bob_open = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "bob",
    );
    let carol_open = open_pipe(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_C,
        bob_open,
        T0 + 31,
        "carol",
    );

    let carol_readable = ingest_encrypted_close(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_B,
        &opening_key(),
        3,
        carol_open,
        T0 + 40,
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "a readable close from a non-owner must leave the pipe open"
    );
    assert_eq!(
        close_log_lines(&engine, LOG_UNATTRIBUTED),
        vec![close_log_line(
            LOG_UNATTRIBUTED,
            carol_readable,
            cast.carol.identity()
        )]
    );

    let bob_readable = ingest_encrypted_close(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        &opening_key(),
        4,
        carol_open,
        T0 + 41,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "a readable close from the owner is honoured"
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_C).expect("closed?"),
        "the owner's readable close names his own pipe"
    );
    assert!(
        !close_log_lines(&engine, LOG_UNATTRIBUTED).contains(&close_log_line(
            LOG_UNATTRIBUTED,
            bob_readable,
            cast.bob.identity()
        )),
        "an owner close of his own pipe is attributed and is not logged"
    );
}

/// B-3 continued. A readable plaintext close from the admin or the owner is
/// honoured. A later unreadable close does not reopen that pipe (AC9).
#[test]
fn readable_admin_or_owner_close_is_honoured_and_does_not_reopen() {
    let (mut engine, cast, head) = seeded();
    let bob_open = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "bob",
    );
    let carol_open = open_pipe(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_C,
        bob_open,
        T0 + 31,
        "carol",
    );
    let admin_close = plaintext_close(
        &mut engine,
        &cast.alice,
        cast.room,
        PIPE_C,
        carol_open,
        T0 + 42,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_C).expect("closed?"),
        "a readable close from the admin is honoured"
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "the admin close names Carol's pipe"
    );
    assert!(
        close_log_lines(&engine, LOG_UNATTRIBUTED).is_empty()
            && close_log_lines(&engine, LOG_UNREADABLE).is_empty(),
        "an admin close is attributed; event {admin_close} must not be logged"
    );

    let owner_close = plaintext_close(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        carol_open,
        T0 + 43,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "a plaintext close from the owner is honoured"
    );
    assert!(
        !close_log_lines(&engine, LOG_UNATTRIBUTED).contains(&close_log_line(
            LOG_UNATTRIBUTED,
            owner_close,
            cast.bob.identity()
        )),
        "the owner's plaintext close is attributed"
    );
    ingest_encrypted_close(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_B,
        &foreign_key(),
        5,
        carol_open,
        T0 + 44,
    );
    assert!(
        engine.pipe_is_closed(&PIPE_B).expect("closed?"),
        "an unreadable close never reopens a pipe an authorized close already shut"
    );
}

/// A sibling `pipe.opened` for `pipe_id` with the same parents and a lower
/// `event_id`, signed by `squatter`.
fn lower_sibling_open(
    engine: &mut SyncEngine,
    squatter: &Principal,
    room: RoomId,
    pipe_id: [u8; SHORT_ID_LEN],
    parent: EventId,
    below: EventId,
) -> EventId {
    let mut chosen = None;
    for offset in 0..10_000u64 {
        let ev = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: squatter.identity(),
            device_id: squatter.device(),
            event_type: EventType::PipeOpened,
            created_at: T0 + 1_000 + offset,
            prev_events: vec![parent],
            content: Content::PipeOpened(PipeOpened {
                pipe_id,
                owner_id: squatter.identity(),
                owner_endpoint: squatter.device(),
                kind: "tcp".to_owned(),
                label: "squat".to_owned(),
                target_hint: "localhost:9".to_owned(),
                alpn: "iroh-rooms/pipe/0".to_owned(),
                allowed_members: vec![squatter.identity()],
                expires_at: None,
            }),
        };
        if ev.event_id() < below {
            chosen = Some(ev);
            break;
        }
    }
    let ev = chosen.expect("a created_at offset yields a lower event id");
    publish(engine, &ev, &squatter.dev, "sibling squat open")
}

/// F-2. Carol's lower sibling open must not become the owner the close rule
/// uses, and her `pipe.closed` must not close Bob's pipe.
#[test]
fn squat_open_and_close_do_not_close_bobs_pipe() {
    let (mut engine, cast, head) = seeded();
    let bob_open = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "bob",
    );
    lower_sibling_open(&mut engine, &cast.carol, cast.room, PIPE_B, head, bob_open);
    plaintext_close(
        &mut engine,
        &cast.carol,
        cast.room,
        PIPE_B,
        bob_open,
        T0 + 2_000,
    );

    let governing = engine
        .pipe_opened_for_owner(&PIPE_B, &cast.bob.identity())
        .expect("lookup")
        .expect("bob's pipe is known");
    assert_eq!(
        governing.owner_id,
        cast.bob.identity(),
        "a lower sibling open from Carol must not govern Bob's pipe"
    );
    assert_eq!(governing.label, "bob");
    assert!(
        !engine
            .pipe_is_closed_for_owner(&PIPE_B, &cast.bob.identity())
            .expect("closed?"),
        "Carol's close must not close Bob's pipe by posing as its owner"
    );
    assert!(
        engine
            .pipe_opened_signers_conflict(&PIPE_B)
            .expect("signers"),
        "Bob and Carol both signed an open for this pipe id"
    );
}

/// F-3. A later `pipe.opened` from Bob for the same id does not replace his
/// earlier open. The lowest `(lamport, event_id)` among his own opens governs,
/// with or without Carol's sibling.
#[test]
fn bobs_later_reopen_does_not_replace_his_governing_open() {
    let (mut engine, cast, head) = seeded();
    let first = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        head,
        T0 + 30,
        "first",
    );
    let _later = open_pipe(
        &mut engine,
        &cast.bob,
        cast.room,
        PIPE_B,
        first,
        T0 + 31,
        "later",
    );
    assert!(
        !engine
            .pipe_opened_signers_conflict(&PIPE_B)
            .expect("signers"),
        "Bob's later re-open is the same signer and must still be dialable"
    );
    lower_sibling_open(&mut engine, &cast.carol, cast.room, PIPE_B, head, first);

    let scoped = engine
        .pipe_opened_for_owner(&PIPE_B, &cast.bob.identity())
        .expect("lookup")
        .expect("bob's pipe is known");
    assert_eq!(scoped.label, "first");
    assert_eq!(scoped.owner_id, cast.bob.identity());
    let unscoped = engine
        .pipe_opened(&PIPE_B)
        .expect("lookup")
        .expect("an open is known");
    // Carol's sibling can win the unscoped hint. The serving lookup does not.
    assert_eq!(unscoped.label, "squat");
    assert!(
        !engine
            .pipe_is_closed_for_owner(&PIPE_B, &cast.bob.identity())
            .expect("closed?"),
        "no close has been published"
    );
}
