//! Stable-surface conformance (issue #36 / IR-0301, spec §11 L1/L2/L4).
//!
//! These are *integration* tests: they see the façade only through its public
//! `pub` API, exactly as a third-party consumer would. That is deliberately
//! stronger than an in-crate unit test for an SDK boundary — a re-export that
//! is not actually reachable, or a path that has drifted (spec R3), fails to
//! compile here, so CI catches it before a consumer does.
//!
//! Every import below is from the **stable** tier
//! (`iroh_rooms::{identity, room, events, files, pipes}` + `prelude`) under
//! default features — no `experimental`. The centerpiece
//! (`offline_flow_uses_only_the_stable_facade`) reconstructs the CLI's offline
//! authoring path (create room → invite → author a `message.text` → fold
//! membership) using the façade alone, proving it is a sufficient superset of
//! that path (AC2 / spec §5.4).

#![allow(clippy::similar_names)] // admin_identity/admin_device etc. are intentionally parallel

use iroh_rooms::events::{
    build_agent_status, capability_hash, validate_wire_bytes, Content, EventId, EventType,
    ValidationContext, PROTOCOL_VERSION,
};
use iroh_rooms::files::{build_file_shared, HashRef};
use iroh_rooms::identity::{DeviceBinding, SigningKey};
use iroh_rooms::pipes::{build_pipe_closed, build_pipe_opened};
use iroh_rooms::room::{
    blob_serve_allowed, build_member_invited, build_member_removed, build_room_created,
    derive_room_id, pipe_connect_allowed, BlobDecision, DenyReason, Ingest, PipeDecision, Role,
    RoomId, RoomInviteTicket, RoomMembership, Status,
};

/// A fixed, deterministic wall-clock stamp so nothing in these tests depends on
/// the real clock (mirrors the golden-vector convention in `core`).
const CREATED_AT: u64 = 1_750_000_000_000;

// ── L1: the stable re-export path resolves and carries the right value ───────

#[test]
fn protocol_version_is_reexported_and_equals_one() {
    // A trivial tripwire: if the stable `events` re-export path ever drifts,
    // this line fails to compile; if the protocol constant ever moves off 1
    // without a deliberate bump, it fails at runtime.
    assert_eq!(PROTOCOL_VERSION, 1);
}

// ── L4: the offline authoring flow, through the façade alone ──────────────────

#[test]
#[allow(clippy::too_many_lines)] // one linear create→invite→message→fold narrative; splitting fragments it
fn offline_flow_uses_only_the_stable_facade() {
    // 1. Identity: generate the admin's identity + device keys.
    let admin_identity = SigningKey::generate();
    let admin_device = SigningKey::generate();
    let admin_id = admin_identity.identity_key();

    // 2. Room: derive the id, author the genesis, and confirm `derive_room_id`
    //    matches what the genesis validates under.
    let nonce = [0x42u8; 16];
    let room_id = derive_room_id(&admin_id, &nonce, CREATED_AT);
    let genesis = build_room_created(
        &admin_identity,
        &admin_device,
        "demo room",
        &nonce,
        CREATED_AT,
    );

    let ctx = ValidationContext::for_room(room_id);
    let genesis_validated =
        validate_wire_bytes(&genesis.to_bytes(), &ctx).expect("genesis validates statelessly");
    assert_eq!(
        genesis_validated.event.event_type,
        EventType::RoomCreated,
        "the genesis is a room.created event"
    );
    let genesis_id: EventId = genesis_validated.event_id;

    // 3. Fold: the admin is Active + Admin after the genesis alone.
    let mut fold = RoomMembership::new(room_id);
    assert!(
        matches!(fold.ingest(genesis_validated), Ingest::Accepted { .. }),
        "the genesis is accepted into the fold"
    );
    let snap = fold.snapshot();
    assert_eq!(snap.admin(), Some(&admin_id), "the creator is the admin");
    assert!(snap.is_active(&admin_id), "the creator is active");
    assert_eq!(snap.role(&admin_id), Some(Role::Admin));
    assert_eq!(
        snap.members().count(),
        1,
        "a fresh room has exactly one member"
    );

    // 4. Invite: the admin invites a member, citing the genesis head.
    let invitee = SigningKey::generate();
    let invitee_key = invitee.identity_key();
    let invite_id = [0x07u8; 16];
    let secret = [0x09u8; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let invite = build_member_invited(
        &admin_identity,
        &admin_device,
        &room_id,
        &invite_id,
        &cap_hash,
        "member",
        &invitee_key,
        None,
        None,
        &[genesis_id],
        CREATED_AT + 1,
    );
    let invite_validated =
        validate_wire_bytes(&invite.to_bytes(), &ctx).expect("invite validates statelessly");
    assert!(
        matches!(fold.ingest(invite_validated), Ingest::Accepted { .. }),
        "the admin-signed invite is accepted by the fold"
    );

    // The invitee is now Invited (not yet Active) and holds the Member role.
    let snap = fold.snapshot();
    assert_eq!(
        snap.status(&invitee_key),
        Some(Status::Invited),
        "an invited-but-not-joined subject reads Invited"
    );
    assert!(
        !snap.is_active(&invitee_key),
        "an invite alone does not make the invitee active"
    );
    assert_eq!(snap.role(&invitee_key), Some(Role::Member));
    assert_eq!(snap.members().count(), 2, "admin + invited member");

    // 5. Message: the admin authors a message.text citing the current heads and
    //    it validates through the stable pipeline.
    let message = iroh_rooms::events::build_message_text(
        &admin_identity,
        &admin_device,
        &room_id,
        "hello room",
        Some("plain"),
        None,
        &[],
        &[genesis_id],
        CREATED_AT + 2,
    );
    let message_validated =
        validate_wire_bytes(&message.to_bytes(), &ctx).expect("message.text validates statelessly");
    assert_eq!(message_validated.event.event_type, EventType::MessageText);
    assert!(matches!(
        message_validated.event.content,
        Content::MessageText(_)
    ));
}

// ── L4/negative: the re-exported validator actually rejects bad bytes ─────────

#[test]
fn validate_wire_bytes_rejects_tampered_bytes() {
    let identity = SigningKey::generate();
    let device = SigningKey::generate();
    let nonce = [0x01u8; 16];
    let room_id = derive_room_id(&identity.identity_key(), &nonce, CREATED_AT);
    let genesis = build_room_created(&identity, &device, "room", &nonce, CREATED_AT);
    let ctx = ValidationContext::for_room(room_id);

    // Sanity: the untouched bytes validate.
    let bytes = genesis.to_bytes();
    assert!(validate_wire_bytes(&bytes, &ctx).is_ok());

    // Flip a byte in the middle: validation must fail closed (the façade
    // re-exports the *real* validator, not a permissive shim).
    let mut tampered = bytes;
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xFF;
    assert!(
        validate_wire_bytes(&tampered, &ctx).is_err(),
        "a corrupted wire event must not validate"
    );
}

// ── L2/stable: the prelude glob covers the common authoring case ──────────────

#[test]
fn prelude_glob_covers_common_authoring() {
    // `use prelude::*` must be enough to author events, and — by construction on
    // a default-features build — can never pull an experimental type into scope
    // (spec D6). The names exercised unqualified below (`build_message_text`,
    // `build_member_joined`, `WireEvent`, `IdentityKey`, `RejectReason`) are
    // *not* imported at the top of this file, so resolving them here proves they
    // come from the prelude glob and not from an explicit `use`.
    use iroh_rooms::prelude::*;

    let identity = SigningKey::generate();
    let device = SigningKey::generate();
    let room_id = RoomId::from_bytes([0x33u8; 32]);

    // Author a genesis and a message through prelude-only names.
    let genesis: WireEvent =
        build_room_created(&identity, &device, "prelude room", &[0u8; 16], CREATED_AT);
    assert!(
        !genesis.to_bytes().is_empty(),
        "the prelude must be able to author a room.created"
    );
    let parent = EventId::from_bytes([0x01u8; 32]);
    let message = build_message_text(
        &identity,
        &device,
        &room_id,
        "hi",
        Some("plain"),
        None,
        &[],
        &[parent],
        CREATED_AT,
    );
    assert!(!message.to_bytes().is_empty());

    // The remaining prelude-only staples resolve through the glob too. Each of
    // these names is absent from this file's top-level `use`, so referencing it
    // here is a genuine drift tripwire for the prelude (spec D6 / R3).
    let _: IdentityKey = identity.identity_key();
    assert!(!std::any::type_name_of_val(&build_member_joined).is_empty());
    assert!(!std::any::type_name::<RejectReason>().is_empty());
}

// ── stable: ticket round-trip + capability-hash agreement ─────────────────────

#[test]
fn invite_ticket_round_trips_and_capability_hash_agrees() {
    let admin = SigningKey::generate();
    let admin_device = SigningKey::generate();
    let invitee = SigningKey::generate();
    let room_id = derive_room_id(&admin.identity_key(), &[0x55u8; 16], CREATED_AT);
    let invite_id = [0x11u8; 16];
    let secret = [0x22u8; 16];

    let ticket = RoomInviteTicket {
        room_id,
        invite_id,
        capability_secret: secret,
        invitee_key: invitee.identity_key(),
        role: "member".to_owned(),
        expires_at: None,
        inviter_identity: admin.identity_key(),
        discovery: vec![admin_device.device_key()],
    };

    // The free `events::capability_hash` and the ticket's own recompute must
    // agree — the two stable re-exports describe the same protocol rule.
    assert_eq!(
        capability_hash(&room_id, &invite_id, &secret),
        ticket.capability_hash(),
        "the free capability_hash must match the ticket's recompute"
    );

    // Display → FromStr must be byte-exact (the stable ticket codec).
    let token = ticket.to_string();
    let parsed = token
        .parse::<RoomInviteTicket>()
        .expect("a freshly minted ticket must round-trip");
    assert_eq!(parsed, ticket, "the ticket must survive a text round-trip");
}

// ── stable: files + pipes event authoring/validation through the façade ───────

#[test]
fn files_facade_builds_and_validates_file_shared() {
    let sender = SigningKey::generate();
    let device = SigningKey::generate();
    let room_id = derive_room_id(&sender.identity_key(), &[0x66u8; 16], CREATED_AT);
    let parent = EventId::from_bytes([0x77u8; 32]);
    let blob_hash = HashRef::from_bytes([0x88u8; 32]);

    let wire = build_file_shared(
        &sender,
        &device,
        &room_id,
        [0x44u8; 16],
        "notes.txt",
        "text/plain",
        42,
        blob_hash,
        None,
        &[],
        &[parent],
        CREATED_AT,
    );
    let ctx = ValidationContext::for_room(room_id);
    let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("file.shared validates");
    assert_eq!(validated.event.event_type, EventType::FileShared);
    match validated.event.content {
        Content::FileShared(fs) => {
            assert_eq!(fs.name, "notes.txt");
            assert_eq!(fs.size_bytes, 42);
            assert_eq!(fs.blob_hash, blob_hash);
        }
        other => panic!("expected FileShared content, got {other:?}"),
    }
}

#[test]
fn pipes_facade_builds_and_validates_pipe_open_and_close() {
    let owner = SigningKey::generate();
    let owner_device = SigningKey::generate();
    let connector = SigningKey::generate();
    let room_id = derive_room_id(&owner.identity_key(), &[0x99u8; 16], CREATED_AT);
    let parent = EventId::from_bytes([0xaau8; 32]);
    let ctx = ValidationContext::for_room(room_id);

    let opened = build_pipe_opened(
        &owner,
        &owner_device,
        &room_id,
        [0x55u8; 16],
        &owner_device.device_key(),
        "dev-server",
        "127.0.0.1:8080",
        "iroh-rooms/pipe/1",
        &[connector.identity_key()],
        None,
        &[parent],
        CREATED_AT,
    );
    let opened_validated =
        validate_wire_bytes(&opened.to_bytes(), &ctx).expect("pipe.opened validates");
    assert_eq!(opened_validated.event.event_type, EventType::PipeOpened);
    assert!(matches!(
        opened_validated.event.content,
        Content::PipeOpened(_)
    ));

    let closed = build_pipe_closed(
        &owner,
        &owner_device,
        &room_id,
        [0x55u8; 16],
        Some("closed"),
        &[opened_validated.event_id],
        CREATED_AT + 1,
    );
    let closed_validated =
        validate_wire_bytes(&closed.to_bytes(), &ctx).expect("pipe.closed validates");
    assert_eq!(closed_validated.event.event_type, EventType::PipeClosed);
    assert!(matches!(
        closed_validated.event.content,
        Content::PipeClosed(_)
    ));
}

// ── stable: agent.status authoring/validation through the events façade ────────

#[test]
fn events_facade_builds_and_validates_agent_status() {
    // `build_agent_status` is the "example agent" seed (spec §2.3); it lives in
    // `events` (no dedicated domain module) and is otherwise unexercised through
    // the façade.
    let sender = SigningKey::generate();
    let device = SigningKey::generate();
    let room_id = derive_room_id(&sender.identity_key(), &[0xc0u8; 16], CREATED_AT);
    let ctx = ValidationContext::for_room(room_id);
    let parent = EventId::from_bytes([0xc1u8; 32]);

    let wire = build_agent_status(
        &sender,
        &device,
        &room_id,
        "working",
        Some("indexing the repo"),
        &[],
        Some(42),
        &[parent],
        CREATED_AT,
    );
    let validated =
        validate_wire_bytes(&wire.to_bytes(), &ctx).expect("agent.status validates statelessly");
    assert_eq!(validated.event.event_type, EventType::AgentStatus);
    match validated.event.content {
        Content::AgentStatus(status) => {
            assert_eq!(status.status, "working");
            assert_eq!(status.progress_pct, Some(42));
            assert_eq!(status.message.as_deref(), Some("indexing the repo"));
        }
        other => panic!("expected AgentStatus content, got {other:?}"),
    }
}

// ── stable: a re-exported `MAX_*` bound is the same limit the validator enforces ─

#[test]
fn constants_bound_is_enforced_by_the_reexported_validator() {
    // Ties the re-exported `events::constants` value to the re-exported validator:
    // a body at the bound validates, one byte over is rejected. A drift in either
    // re-export (spec R3) fails this test.
    use iroh_rooms::events::constants::MAX_MESSAGE_BODY_BYTES;

    let identity = SigningKey::generate();
    let device = SigningKey::generate();
    let room_id = derive_room_id(&identity.identity_key(), &[0xd0u8; 16], CREATED_AT);
    let ctx = ValidationContext::for_room(room_id);
    let parent = EventId::from_bytes([0xd1u8; 32]);

    // Exactly at the bound: validates. (ASCII, so byte length == char count.)
    let at_limit = "a".repeat(MAX_MESSAGE_BODY_BYTES);
    let ok = iroh_rooms::events::build_message_text(
        &identity,
        &device,
        &room_id,
        &at_limit,
        Some("plain"),
        None,
        &[],
        &[parent],
        CREATED_AT,
    );
    assert!(
        validate_wire_bytes(&ok.to_bytes(), &ctx).is_ok(),
        "a message body of exactly MAX_MESSAGE_BODY_BYTES must validate"
    );

    // One byte over: rejected as InvalidContent.
    let over_limit = "a".repeat(MAX_MESSAGE_BODY_BYTES + 1);
    let too_big = iroh_rooms::events::build_message_text(
        &identity,
        &device,
        &room_id,
        &over_limit,
        Some("plain"),
        None,
        &[],
        &[parent],
        CREATED_AT,
    );
    assert!(
        matches!(
            validate_wire_bytes(&too_big.to_bytes(), &ctx),
            Err(iroh_rooms::events::RejectReason::InvalidContent)
        ),
        "a message body over MAX_MESSAGE_BODY_BYTES must be rejected as InvalidContent"
    );
}

// ── stable: full membership lifecycle (invite → join → remove) via the façade ──

#[test]
fn membership_lifecycle_join_then_remove_through_the_facade() {
    // `build_member_joined` / `build_member_removed` and the Active/Removed fold
    // transitions they drive are re-exported but otherwise only reach `Invited`
    // in `offline_flow_uses_only_the_stable_facade`. This carries a non-admin
    // member all the way to Active and back out to Removed.
    let room = active_room();
    let member_id = room.member_identity.identity_key();

    // After genesis → invite → join, the non-admin member is Active + Member.
    let snap = room.fold.snapshot();
    assert!(
        snap.is_active(&member_id),
        "a valid join makes the invitee Active"
    );
    assert_eq!(snap.status(&member_id), Some(Status::Active));
    assert_eq!(snap.role(&member_id), Some(Role::Member));

    // The admin removes the member; the fold converges it to Removed.
    let mut fold = room.fold;
    let ctx = ValidationContext::for_room(room.room_id);
    let removed = build_member_removed(
        &room.admin_identity,
        &room.admin_device,
        &room.room_id,
        &member_id,
        Some("spam"),
        None,
        &room.heads,
        CREATED_AT + 3,
    );
    let removed_validated =
        validate_wire_bytes(&removed.to_bytes(), &ctx).expect("member.removed validates");
    assert!(
        matches!(fold.ingest(removed_validated), Ingest::Accepted { .. }),
        "an admin-signed removal is accepted by the fold"
    );

    let snap = fold.snapshot();
    assert_eq!(
        snap.status(&member_id),
        Some(Status::Removed),
        "an admin removal converges the member to Removed"
    );
    assert!(
        !snap.is_active(&member_id),
        "a removed member is no longer Active"
    );
}

// ── stable: the access predicates gate a snapshot folded through the façade ────

#[test]
fn access_predicates_gate_a_folded_snapshot_through_the_facade() {
    // `blob_serve_allowed` / `pipe_connect_allowed` (+ their decision/deny enums)
    // are the authorization boundary the spec calls central to the stable tier
    // (D3), yet were untested through the façade. Prove they are reachable and
    // enforce — not a permissive shim — against a real folded snapshot.
    let room = active_room();
    let snap = room.fold.snapshot();
    let member_id = room.member_identity.identity_key();
    let admin_id = room.admin_identity.identity_key();
    let member_dev = room.member_device.device_key();
    let stranger_dev = SigningKey::generate().device_key();

    // ── blob_serve_allowed ──
    let hash = HashRef::from_bytes([0xabu8; 32]);
    let shares_by_member =
        move |_h: &HashRef| -> Option<iroh_rooms::identity::IdentityKey> { Some(member_id) };
    let no_shares = |_h: &HashRef| -> Option<iroh_rooms::identity::IdentityKey> { None };

    // An Active member's device, blob shared by an Active member → Serve.
    assert_eq!(
        blob_serve_allowed(&snap, &member_dev, &hash, &shares_by_member),
        BlobDecision::Serve
    );
    // No share references the hash → Unshared (Active member, but nothing to serve).
    assert_eq!(
        blob_serve_allowed(&snap, &member_dev, &hash, &no_shares),
        BlobDecision::Reject(DenyReason::Unshared)
    );
    // A device that resolves to no known identity → UnknownDevice.
    assert_eq!(
        blob_serve_allowed(&snap, &stranger_dev, &hash, &shares_by_member),
        BlobDecision::Reject(DenyReason::UnknownDevice)
    );

    // ── pipe_connect_allowed ──
    let ctx = ValidationContext::for_room(room.room_id);

    // The admin (Active owner) exposes a pipe that lists the member.
    let allow_member = build_pipe_opened(
        &room.admin_identity,
        &room.admin_device,
        &room.room_id,
        [0x55u8; 16],
        &room.admin_device.device_key(),
        "dev-server",
        "127.0.0.1:8080",
        "iroh-rooms/pipe/1",
        &[member_id],
        None,
        &room.heads,
        CREATED_AT + 10,
    );
    let allow_validated =
        validate_wire_bytes(&allow_member.to_bytes(), &ctx).expect("pipe.opened validates");
    let Content::PipeOpened(pipe) = allow_validated.event.content else {
        panic!("expected pipe.opened content");
    };
    // The allowed Active member's device → Accept.
    assert_eq!(
        pipe_connect_allowed(&snap, &member_dev, &pipe, None),
        PipeDecision::Accept
    );
    // A stranger device → UnknownDevice.
    assert_eq!(
        pipe_connect_allowed(&snap, &stranger_dev, &pipe, None),
        PipeDecision::Reject(DenyReason::UnknownDevice)
    );

    // A pipe that does NOT list the member → NotAllowed (no default-all, PRD §13.2).
    let admin_only = build_pipe_opened(
        &room.admin_identity,
        &room.admin_device,
        &room.room_id,
        [0x56u8; 16],
        &room.admin_device.device_key(),
        "dev-server",
        "127.0.0.1:8080",
        "iroh-rooms/pipe/1",
        &[admin_id],
        None,
        &room.heads,
        CREATED_AT + 11,
    );
    let admin_only_validated =
        validate_wire_bytes(&admin_only.to_bytes(), &ctx).expect("pipe.opened validates");
    let Content::PipeOpened(admin_only_pipe) = admin_only_validated.event.content else {
        panic!("expected pipe.opened content");
    };
    assert_eq!(
        pipe_connect_allowed(&snap, &member_dev, &admin_only_pipe, None),
        PipeDecision::Reject(DenyReason::NotAllowed)
    );
}

// ── shared fixture ────────────────────────────────────────────────────────────

/// A room folded through the **stable façade alone** (genesis → invite → join)
/// to the point where a non-admin member is `Active` with a bound device — the
/// state the access predicates gate and the removal test builds on. Building it
/// with only `iroh_rooms::{identity, room, events}` doubles as a reachability
/// proof that the façade can reconstruct the authorization boundary end to end.
struct ActiveRoom {
    fold: RoomMembership,
    room_id: RoomId,
    admin_identity: SigningKey,
    admin_device: SigningKey,
    member_identity: SigningKey,
    member_device: SigningKey,
    /// The current fold heads (the join event) to cite as `prev_events`.
    heads: Vec<EventId>,
}

fn active_room() -> ActiveRoom {
    let admin_identity = SigningKey::generate();
    let admin_device = SigningKey::generate();
    let member_identity = SigningKey::generate();
    let member_device = SigningKey::generate();

    let nonce = [0x5au8; 16];
    let room_id = derive_room_id(&admin_identity.identity_key(), &nonce, CREATED_AT);
    let ctx = ValidationContext::for_room(room_id);
    let mut fold = RoomMembership::new(room_id);

    // Genesis.
    let genesis = build_room_created(&admin_identity, &admin_device, "room", &nonce, CREATED_AT);
    let genesis_validated =
        validate_wire_bytes(&genesis.to_bytes(), &ctx).expect("genesis validates");
    let genesis_id = genesis_validated.event_id;
    assert!(matches!(
        fold.ingest(genesis_validated),
        Ingest::Accepted { .. }
    ));

    // Invite.
    let invite_id = [0x07u8; 16];
    let secret = [0x09u8; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &secret);
    let invite = build_member_invited(
        &admin_identity,
        &admin_device,
        &room_id,
        &invite_id,
        &cap_hash,
        "member",
        &member_identity.identity_key(),
        None,
        None,
        &[genesis_id],
        CREATED_AT + 1,
    );
    let invite_validated = validate_wire_bytes(&invite.to_bytes(), &ctx).expect("invite validates");
    let invite_event_id = invite_validated.event_id;
    assert!(matches!(
        fold.ingest(invite_validated),
        Ingest::Accepted { .. }
    ));

    // Join — the invitee presents the matching secret + a device binding.
    let binding = DeviceBinding::create(&room_id, &member_identity, member_device.device_key());
    let join = iroh_rooms::room::build_member_joined(
        &member_identity,
        &member_device,
        &room_id,
        &invite_id,
        &secret,
        "member",
        binding,
        None,
        &[invite_event_id],
        CREATED_AT + 2,
    );
    let join_validated = validate_wire_bytes(&join.to_bytes(), &ctx).expect("join validates");
    let join_id = join_validated.event_id;
    assert!(
        matches!(fold.ingest(join_validated), Ingest::Accepted { .. }),
        "a valid join must be accepted so the member folds to Active"
    );

    ActiveRoom {
        fold,
        room_id,
        admin_identity,
        admin_device,
        member_identity,
        member_device,
        heads: vec![join_id],
    }
}
