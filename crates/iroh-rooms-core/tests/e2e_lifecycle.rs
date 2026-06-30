//! End-to-end lifecycle tests for the canonical signed event model.
//!
//! These tests exercise the full `validate_wire_bytes` pipeline across
//! *sequences* of events, simulating how real room traffic flows: each
//! validated event's `event_id` feeds the next event's `prev_events`, the
//! capability hash output by an invite is verified by a join, and device
//! bindings for multiple principals are checked in context.
//!
//! None of these multi-step flows are in `golden_vectors.rs`, which tests
//! individual events in isolation.

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, AgentStatus, Content, EventType, FileShared, MemberInvited, MemberJoined,
    MemberLeft, MemberRemoved, MessageText, PipeClosed, PipeOpened, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, HashRef};
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;

// --------------------------------------------------------------------------
// Shared fixtures
// --------------------------------------------------------------------------

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

const NONCE: [u8; 16] = [0xaa; 16];
const T0: u64 = 1_750_000_000_000; // room genesis
const T1: u64 = T0 + 1_000;
const T2: u64 = T0 + 2_000;
const T3: u64 = T0 + 3_000;
const T4: u64 = T0 + 4_000;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Build a fully-valid `room.created` wire event and return `(bytes, room_id)`.
fn genesis(
    id_sk: &SigningKey,
    dev_sk: &SigningKey,
) -> (Vec<u8>, iroh_rooms_core::event::ids::RoomId) {
    let id_key = id_sk.identity_key();
    let dev_key = dev_sk.device_key();
    let room_id = signed::derive_room_id(&id_key, &NONCE, T0);
    let binding = DeviceBinding::create(&room_id, id_sk, dev_key);
    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: id_key,
        device_id: dev_key,
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Test Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![id_key],
            device_binding: binding,
        }),
    };
    (seal(&ev, dev_sk), room_id)
}

/// Encode + sign a `SignedEvent` to raw wire bytes.
fn seal(ev: &SignedEvent, dev_sk: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev_sk);
    WireEvent::seal(csb, sig).to_bytes()
}

fn ctx(room_id: iroh_rooms_core::event::ids::RoomId) -> ValidationContext {
    ValidationContext::for_room(room_id)
}

// --------------------------------------------------------------------------
// 1. Causal chain: genesis → message
// --------------------------------------------------------------------------

/// The simplest causal chain: one `message.text` event whose `prev_events`
/// contains the validated genesis `event_id`.
#[test]
fn causal_chain_genesis_then_message() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v_genesis = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T1,
        prev_events: vec![v_genesis.event_id],
        content: Content::MessageText(MessageText {
            body: "Hello room".to_owned(),
            format: Some("plain".to_owned()),
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v_msg = validate_wire_bytes(&seal(&ev, &alice_dev), &ctx(room_id)).expect("message");

    assert_eq!(v_msg.event.prev_events[0], v_genesis.event_id);
    assert_eq!(v_msg.event.event_type, EventType::MessageText);
    assert!(v_msg.flags.is_empty());
}

/// Three-event chain: genesis → `msg_a` → `msg_b`.  Each event's `event_id`
/// (produced by the validator) feeds the next event's `prev_events`.
#[test]
fn causal_chain_three_events() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    let msg_a = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MessageText(MessageText {
            body: "first".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v1 = validate_wire_bytes(&seal(&msg_a, &alice_dev), &ctx(room_id)).expect("msg_a");

    let msg_b = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T2,
        prev_events: vec![v1.event_id],
        content: Content::MessageText(MessageText {
            body: "second".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v2 = validate_wire_bytes(&seal(&msg_b, &alice_dev), &ctx(room_id)).expect("msg_b");

    assert_eq!(v2.event.prev_events[0], v1.event_id);
    assert_ne!(v0.event_id, v1.event_id);
    assert_ne!(v1.event_id, v2.event_id);
}

// --------------------------------------------------------------------------
// 2. Invite / join lifecycle
// --------------------------------------------------------------------------

/// Alice creates a room, invites Bob with a derived capability, Bob joins.
/// Verifies that the invite's `capability_hash` is exactly reproduced from
/// the join's `capability_secret` — the key cross-event invariant.
#[test]
fn invite_join_lifecycle() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);
    let bob_dev = sk(0x11);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v_genesis = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Alice issues the invite.
    let invite_id: [u8; 16] = [0x01; 16];
    let cap_secret: [u8; 16] = [0x42; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &cap_secret);

    let invite_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T1,
        prev_events: vec![v_genesis.event_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: cap_hash,
            role: "member".to_owned(),
            invitee_key: bob_id.identity_key(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let v_invite =
        validate_wire_bytes(&seal(&invite_ev, &alice_dev), &ctx(room_id)).expect("invite");
    assert_eq!(v_invite.event.event_type, EventType::MemberInvited);

    // Bob joins with the matching secret.
    let bob_binding = DeviceBinding::create(&room_id, &bob_id, bob_dev.device_key());
    let join_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T2,
        prev_events: vec![v_invite.event_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: cap_secret,
            role: "member".to_owned(),
            device_binding: bob_binding,
            display_name: Some("Bob".to_owned()),
        }),
    };
    let v_join = validate_wire_bytes(&seal(&join_ev, &bob_dev), &ctx(room_id)).expect("join");
    assert_eq!(v_join.event.event_type, EventType::MemberJoined);

    // The invite's capability_hash must equal the value recomputed from the
    // join's (via_invite_id, capability_secret) pair.
    let Content::MemberJoined(j) = &v_join.event.content else {
        panic!("expected MemberJoined");
    };
    let recomputed = capability_hash(&room_id, &j.via_invite_id, &j.capability_secret);
    assert_eq!(
        recomputed, cap_hash,
        "capability_hash from invite must match recomputed value from join"
    );
}

// --------------------------------------------------------------------------
// 3. Two-identity room session
// --------------------------------------------------------------------------

/// Full session: genesis → invite → join → Alice message → Bob reply
/// (with `in_reply_to` set).  Every event references its predecessor's ID.
#[test]
fn two_identity_room_session() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);
    let bob_dev = sk(0x11);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Alice invites Bob.
    let invite_id: [u8; 16] = [0x05; 16];
    let secret: [u8; 16] = [0x99; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let invite = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: cap_hash,
            role: "member".to_owned(),
            invitee_key: bob_id.identity_key(),
            expires_at: None,
            invitee_hint: Some("bob@example.com".to_owned()),
        }),
    };
    let v1 = validate_wire_bytes(&seal(&invite, &alice_dev), &ctx(room_id)).expect("invite");

    // Bob joins.
    let bob_binding = DeviceBinding::create(&room_id, &bob_id, bob_dev.device_key());
    let join = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T2,
        prev_events: vec![v1.event_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: secret,
            role: "member".to_owned(),
            device_binding: bob_binding,
            display_name: None,
        }),
    };
    let v2 = validate_wire_bytes(&seal(&join, &bob_dev), &ctx(room_id)).expect("join");

    // Alice sends "Welcome, Bob!"
    let alice_msg = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T3,
        prev_events: vec![v2.event_id],
        content: Content::MessageText(MessageText {
            body: "Welcome, Bob!".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: Some(vec![bob_id.identity_key()]),
        }),
    };
    let v3 = validate_wire_bytes(&seal(&alice_msg, &alice_dev), &ctx(room_id)).expect("alice_msg");

    // Bob replies, citing Alice's message.
    let bob_msg = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T4,
        prev_events: vec![v3.event_id],
        content: Content::MessageText(MessageText {
            body: "Thanks Alice!".to_owned(),
            format: Some("markdown".to_owned()),
            in_reply_to: Some(v3.event_id),
            mentions: None,
        }),
    };
    let v4 = validate_wire_bytes(&seal(&bob_msg, &bob_dev), &ctx(room_id)).expect("bob_msg");

    // Check the causal chain end-to-end.
    assert_eq!(v4.event.prev_events[0], v3.event_id);
    let Content::MessageText(m) = &v4.event.content else {
        panic!("expected MessageText");
    };
    assert_eq!(m.in_reply_to, Some(v3.event_id));
    assert!(v4.flags.is_empty());
    // All five event IDs are distinct.
    let ids: Vec<EventId> = [
        v0.event_id,
        v1.event_id,
        v2.event_id,
        v3.event_id,
        v4.event_id,
    ]
    .to_vec();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 5, "all event IDs must be distinct");
}

// --------------------------------------------------------------------------
// 4. Member departure (member.left + member.removed)
// --------------------------------------------------------------------------

/// Bob joins and then voluntarily leaves; Alice subsequently removes Charlie.
/// Each departure event is validated through the full pipeline.
#[test]
fn member_departure_lifecycle() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);
    let bob_dev = sk(0x11);
    let charlie_id = sk(0x20);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Bob joins (capability check is deferred; stateless layer accepts it).
    let bob_binding = DeviceBinding::create(&room_id, &bob_id, bob_dev.device_key());
    let join = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: [0x00; 16],
            capability_secret: [0x00; 16],
            role: "member".to_owned(),
            device_binding: bob_binding,
            display_name: None,
        }),
    };
    let v1 = validate_wire_bytes(&seal(&join, &bob_dev), &ctx(room_id)).expect("join");

    // Bob leaves voluntarily.
    let left = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MemberLeft,
        created_at: T2,
        prev_events: vec![v1.event_id],
        content: Content::MemberLeft(MemberLeft {
            member_id: bob_id.identity_key(),
            reason: Some("bye".to_owned()),
        }),
    };
    let v2 = validate_wire_bytes(&seal(&left, &bob_dev), &ctx(room_id)).expect("member.left");
    assert_eq!(v2.event.event_type, EventType::MemberLeft);

    // Alice removes Charlie.
    let removed = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MemberRemoved,
        created_at: T3,
        prev_events: vec![v2.event_id],
        content: Content::MemberRemoved(MemberRemoved {
            member_id: charlie_id.identity_key(),
            removed_by: alice_id.identity_key(),
            reason: Some("spam".to_owned()),
            device_binding: None,
        }),
    };
    let v3 =
        validate_wire_bytes(&seal(&removed, &alice_dev), &ctx(room_id)).expect("member.removed");
    assert_eq!(v3.event.event_type, EventType::MemberRemoved);
    assert_eq!(v3.event.prev_events[0], v2.event_id);
}

// --------------------------------------------------------------------------
// 5. File-sharing and pipe lifecycle
// --------------------------------------------------------------------------

/// Alice shares a file, opens a pipe for Bob, then closes the pipe.
/// Exercises the three event types not covered by valid-path tests elsewhere.
#[test]
fn file_and_pipe_lifecycle() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Alice shares a file.
    let blob_hash = HashRef::from_bytes([0xbe; 32]);
    let file_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::FileShared,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::FileShared(FileShared {
            file_id: [0x01; 16],
            name: "report.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 12_345,
            blob_hash,
            blob_format: Some("raw".to_owned()),
            providers: Some(vec![alice_dev.device_key()]),
        }),
    };
    let v1 = validate_wire_bytes(&seal(&file_ev, &alice_dev), &ctx(room_id)).expect("file.shared");
    assert_eq!(v1.event.event_type, EventType::FileShared);

    // Alice opens a pipe for Bob.
    let pipe_id: [u8; 16] = [0x02; 16];
    let pipe_open = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::PipeOpened,
        created_at: T2,
        prev_events: vec![v1.event_id],
        content: Content::PipeOpened(PipeOpened {
            pipe_id,
            owner_id: alice_id.identity_key(),
            owner_endpoint: alice_dev.device_key(),
            kind: "tcp".to_owned(),
            label: "data-pipe".to_owned(),
            target_hint: "127.0.0.1".to_owned(),
            alpn: "iroh/rooms/1".to_owned(),
            allowed_members: vec![bob_id.identity_key()],
            expires_at: None,
        }),
    };
    let v2 =
        validate_wire_bytes(&seal(&pipe_open, &alice_dev), &ctx(room_id)).expect("pipe.opened");
    assert_eq!(v2.event.event_type, EventType::PipeOpened);

    // Alice closes the pipe.
    let pipe_close = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::PipeClosed,
        created_at: T3,
        prev_events: vec![v2.event_id],
        content: Content::PipeClosed(PipeClosed {
            pipe_id,
            reason: Some("closed".to_owned()),
        }),
    };
    let v3 =
        validate_wire_bytes(&seal(&pipe_close, &alice_dev), &ctx(room_id)).expect("pipe.closed");
    assert_eq!(v3.event.event_type, EventType::PipeClosed);
    assert_eq!(v3.event.prev_events[0], v2.event_id);
}

// --------------------------------------------------------------------------
// 6. Agent-status update sequence
// --------------------------------------------------------------------------

/// An agent posts three status updates in a chain: running (0%) → running (50%)
/// → done (100%).  Each cites its predecessor.
#[test]
fn agent_status_update_sequence() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v_genesis = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    let steps: &[(&str, u64, u64)] = &[("running", 0, T1), ("running", 50, T2), ("done", 100, T3)];

    let mut prev_id = v_genesis.event_id;
    for (status, pct, t) in steps {
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: alice_id.identity_key(),
            device_id: alice_dev.device_key(),
            event_type: EventType::AgentStatus,
            created_at: *t,
            prev_events: vec![prev_id],
            content: Content::AgentStatus(AgentStatus {
                status: (*status).to_owned(),
                message: Some(format!("progress {pct}%")),
                related_artifact_ids: None,
                progress_pct: Some(*pct),
            }),
        };
        let validated = validate_wire_bytes(&seal(&ev, &alice_dev), &ctx(room_id)).expect(status);
        assert_eq!(validated.event.prev_events[0], prev_id);
        prev_id = validated.event_id;
    }
}

// --------------------------------------------------------------------------
// 7. Signed bytes preserved and re-hash to event_id across a chain
// --------------------------------------------------------------------------

/// For every event in the invite-join chain, the `signed_bytes()` returned by
/// the validator re-hash (BLAKE3-256) to the reported `event_id`.  This is the
/// byte-faithful storage invariant: the bytes that were verified are the bytes
/// that are stored, and re-hashing them yields the stable dedup key.
#[test]
fn signed_bytes_rehash_to_event_id_across_chain() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);
    let bob_dev = sk(0x11);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    let invite_id: [u8; 16] = [0xca; 16];
    let secret: [u8; 16] = [0xfe; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let invite = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id,
            capability_hash: cap_hash,
            role: "admin".to_owned(),
            invitee_key: bob_id.identity_key(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let v1 = validate_wire_bytes(&seal(&invite, &alice_dev), &ctx(room_id)).expect("invite");

    let bob_binding = DeviceBinding::create(&room_id, &bob_id, bob_dev.device_key());
    let join = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MemberJoined,
        created_at: T2,
        prev_events: vec![v1.event_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: invite_id,
            capability_secret: secret,
            role: "admin".to_owned(),
            device_binding: bob_binding,
            display_name: None,
        }),
    };
    let v2 = validate_wire_bytes(&seal(&join, &bob_dev), &ctx(room_id)).expect("join");

    for (label, v) in [("genesis", &v0), ("invite", &v1), ("join", &v2)] {
        let recomputed = signed::event_id_from_bytes(v.signed_bytes());
        assert_eq!(
            recomputed, v.event_id,
            "{label}: BLAKE3(signed_bytes()) must equal event_id"
        );
    }
}

// --------------------------------------------------------------------------
// 8. Cross-room replay is caught even in a multi-event context
// --------------------------------------------------------------------------

// --------------------------------------------------------------------------
// 9. DAG merge: concurrent events from two participants
// --------------------------------------------------------------------------

/// Alice and Bob each post a message concurrently (both citing the same
/// genesis parent), then a third event cites both as `prev_events`.
/// The merge node must validate successfully with two causal parents.
#[test]
fn dag_merge_from_concurrent_branches() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let bob_id = sk(0x10);
    let bob_dev = sk(0x11);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Alice posts — cites genesis.
    let alice_msg = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MessageText(MessageText {
            body: "Alice concurrent".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v_alice =
        validate_wire_bytes(&seal(&alice_msg, &alice_dev), &ctx(room_id)).expect("alice msg");

    // Bob posts concurrently — also cites genesis, not Alice's message.
    let bob_msg = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: bob_id.identity_key(),
        device_id: bob_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MessageText(MessageText {
            body: "Bob concurrent".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v_bob = validate_wire_bytes(&seal(&bob_msg, &bob_dev), &ctx(room_id)).expect("bob msg");

    // Concurrent events have distinct ids.
    assert_ne!(v_alice.event_id, v_bob.event_id);

    // A merge event cites both concurrent events as parents.
    let merge = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T2,
        prev_events: vec![v_alice.event_id, v_bob.event_id],
        content: Content::MessageText(MessageText {
            body: "Merge".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let v_merge = validate_wire_bytes(&seal(&merge, &alice_dev), &ctx(room_id)).expect("merge");

    assert_eq!(v_merge.event.prev_events.len(), 2);
    assert!(v_merge.event.prev_events.contains(&v_alice.event_id));
    assert!(v_merge.event.prev_events.contains(&v_bob.event_id));
    // The merge event itself gets a fresh, distinct id.
    assert_ne!(v_merge.event_id, v_alice.event_id);
    assert_ne!(v_merge.event_id, v_bob.event_id);
}

// --------------------------------------------------------------------------
// 10. Byte-faithful forwarding: storage → re-broadcast round-trip
// --------------------------------------------------------------------------

/// Simulates the storage + forwarding contract: the `signed_bytes()` extracted
/// from a validated event can be re-sealed into a fresh `WireEvent` (recomputing
/// the advisory id) and re-validated to the same `event_id`.  Proves the
/// stateless layer hands persistence layers exactly the bytes they need to store
/// and re-broadcast.
#[test]
fn stored_signed_bytes_re_validate_to_same_event_id() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    let ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::AgentStatus,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::AgentStatus(AgentStatus {
            status: "running".to_owned(),
            message: Some("in progress".to_owned()),
            related_artifact_ids: None,
            progress_pct: Some(42),
        }),
    };
    let first =
        validate_wire_bytes(&seal(&ev, &alice_dev), &ctx(room_id)).expect("first validation");
    let original_id = first.event_id;

    // Extract the raw signed bytes and the verbatim signature, then re-seal.
    // This is what a forwarding relay or persistence layer would do.
    let signed_bytes = first.signed_bytes().to_vec();
    let sig = first.wire.sig;
    let rewrapped = iroh_rooms_core::event::wire::WireEvent::seal(signed_bytes, sig).to_bytes();

    let second =
        validate_wire_bytes(&rewrapped, &ctx(room_id)).expect("re-validation must succeed");

    assert_eq!(
        second.event_id, original_id,
        "event_id stable across re-seal"
    );
    assert_eq!(
        second.signed_bytes(),
        first.signed_bytes(),
        "signed bytes preserved verbatim"
    );
    assert_eq!(
        second.event, first.event,
        "decoded event identical after re-seal"
    );
}

// --------------------------------------------------------------------------
// 11. member.removed with an optional device_binding
// --------------------------------------------------------------------------

/// An admin removes a member AND includes the optional re-attestation device
/// binding (Event Protocol §7 member.removed schema lists it as optional;
/// §9 verifies it when present).  This exercises the branch in `verify_bindings`
/// that is not covered by the lifecycle test that always passes `device_binding:
/// None`.
#[test]
fn member_removed_with_optional_device_binding_accepted() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);
    let charlie_id = sk(0x20);

    let (genesis_bytes, room_id) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes, &ctx(room_id)).expect("genesis");

    // Alice removes Charlie, re-attesting her own device binding in the event.
    let alice_binding = DeviceBinding::create(&room_id, &alice_id, alice_dev.device_key());
    let removed = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MemberRemoved,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MemberRemoved(MemberRemoved {
            member_id: charlie_id.identity_key(),
            removed_by: alice_id.identity_key(),
            reason: Some("violation".to_owned()),
            device_binding: Some(alice_binding),
        }),
    };
    let validated = validate_wire_bytes(&seal(&removed, &alice_dev), &ctx(room_id))
        .expect("member.removed with device_binding must be accepted");

    assert_eq!(validated.event.event_type, EventType::MemberRemoved);
    assert!(validated.flags.is_empty());
    // Confirm the optional binding is preserved in the decoded event.
    let Content::MemberRemoved(c) = &validated.event.content else {
        panic!("expected MemberRemoved content");
    };
    assert!(c.device_binding.is_some(), "device_binding must be present");
}

/// An event built for room A is presented to the validator for room B.  The
/// cross-room check must fire regardless of whether the event is structurally
/// valid (has good `prev_events`, valid signature, etc.).
#[test]
fn cross_room_replay_blocked_in_multi_event_context() {
    let alice_id = sk(0x01);
    let alice_dev = sk(0x02);

    let (genesis_bytes_a, room_a) = genesis(&alice_id, &alice_dev);
    let v0 = validate_wire_bytes(&genesis_bytes_a, &ctx(room_a)).expect("room-A genesis");

    let msg_for_a = SignedEvent {
        schema_version: 1,
        room_id: room_a,
        sender_id: alice_id.identity_key(),
        device_id: alice_dev.device_key(),
        event_type: EventType::MessageText,
        created_at: T1,
        prev_events: vec![v0.event_id],
        content: Content::MessageText(MessageText {
            body: "room-A message".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let msg_bytes = seal(&msg_for_a, &alice_dev);

    // room_b: different nonce ⇒ different room_id.
    let nonce_b: [u8; 16] = [0xbb; 16];
    let room_b = signed::derive_room_id(&alice_id.identity_key(), &nonce_b, T0);

    let result = validate_wire_bytes(&msg_bytes, &ctx(room_b));
    assert_eq!(
        result,
        Err(iroh_rooms_core::event::reject::RejectReason::RoomIdMismatch)
    );
}
