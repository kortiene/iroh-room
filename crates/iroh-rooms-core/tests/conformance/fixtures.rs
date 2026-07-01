//! The single source of golden truth for every vector: the Cast, the Room, the
//! validated fixture-log DAG, and the capability fixtures
//! (`PHASE-0-SPIKE.md` Protocol Test Vectors, "Fixtures").
//!
//! Every vector derives from these identical, seed-derived bytes so the golden
//! ids are pinned in exactly one place. Two tiers of golden value live here
//! (spec §4.3):
//!
//! * **Tier 1 — independently reproduced.** Cast public keys, `room_id_A/B`, and
//!   the golden-serialization CSB/`event_id`/tampered/cross-room ids are asserted
//!   against the exact spike hex (`fixtures::tests`). These already round-trip in
//!   `golden_vectors.rs`; a mismatch here is a hard NO-GO.
//! * **Tier 2 — regenerated & pinned.** The fixture-log ids (`E_create … E_pipe`)
//!   were **not** independently reproduced by the spike — its Fixtures table lists
//!   placeholder hex whose content maps were never pinned (see the spike's
//!   "Caveat" under *Protocol Test Vectors*). We regenerate them from the landed
//!   content schema/builders and pin the produced values below as regression
//!   tripwires. Where a produced id differs from the spike's listed placeholder
//!   (expected), that is acceptable per the spike's own instruction — the *values
//!   and derivations* are locked by this suite from here on. Likewise the invite
//!   `capability_hash`es are regenerated from concrete `(invite_id, secret)` byte
//!   patterns and pinned.

#![allow(dead_code)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_panics_doc)]

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, FileShared, MemberLeft, MemberRemoved,
};
use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::event::{
    build_member_invited, build_member_joined, build_message_text, build_pipe_opened,
    build_room_created,
};

// ---------------------------------------------------------------------------
// Room parameters (spike Fixtures "Room").
// ---------------------------------------------------------------------------

/// The room's genesis `created_at` (spike Fixtures: `1750000000000`).
pub const T_ROOM: u64 = 1_750_000_000_000;

/// The golden serialization event's `created_at` (spike vector §1).
pub const GOLDEN_CREATED_AT: u64 = 1_750_000_005_000;

/// `room_nonce = 000102030405060708090a0b0c0d0e0f`.
pub const ROOM_NONCE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];

// ---------------------------------------------------------------------------
// Cast (spike Fixtures "Cast"): seed = byte repeated 32×; id then dev seed.
// ---------------------------------------------------------------------------

/// A signing key from the spike's `seed = byte repeated 32×` convention.
pub fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

pub fn alice_id_sk() -> SigningKey {
    sk(0x01)
}
pub fn alice_dev_sk() -> SigningKey {
    sk(0x02)
}
pub fn bob_id_sk() -> SigningKey {
    sk(0x03)
}
pub fn bob_dev_sk() -> SigningKey {
    sk(0x04)
}
pub fn carol_id_sk() -> SigningKey {
    sk(0x05)
}
pub fn carol_dev_sk() -> SigningKey {
    sk(0x06)
}
pub fn dave_id_sk() -> SigningKey {
    sk(0x07)
}
pub fn dave_dev_sk() -> SigningKey {
    sk(0x08)
}
pub fn mallory_id_sk() -> SigningKey {
    sk(0x09)
}
pub fn mallory_dev_sk() -> SigningKey {
    sk(0x0a)
}

pub fn alice_id() -> IdentityKey {
    alice_id_sk().identity_key()
}
pub fn alice_dev() -> DeviceKey {
    alice_dev_sk().device_key()
}
pub fn bob_id() -> IdentityKey {
    bob_id_sk().identity_key()
}
pub fn bob_dev() -> DeviceKey {
    bob_dev_sk().device_key()
}
pub fn carol_id() -> IdentityKey {
    carol_id_sk().identity_key()
}
pub fn carol_dev() -> DeviceKey {
    carol_dev_sk().device_key()
}
pub fn dave_id() -> IdentityKey {
    dave_id_sk().identity_key()
}
pub fn dave_dev() -> DeviceKey {
    dave_dev_sk().device_key()
}
pub fn mallory_id() -> IdentityKey {
    mallory_id_sk().identity_key()
}
pub fn mallory_dev() -> DeviceKey {
    mallory_dev_sk().device_key()
}

/// The published Cast public keys (spike Fixtures table), asserted in
/// `tests::seeds_reproduce_cast_public_keys` (Tier 1).
pub const CAST_HEX: &[(&str, &str)] = &[
    (
        "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c", // alice_id
        "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394", // alice_dev
    ),
    (
        "ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1", // bob_id
        "ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c", // bob_dev
    ),
    (
        "6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1", // carol_id
        "8a875fff1eb38451577acd5afee405456568dd7c89e090863a0557bc7af49f17", // carol_dev
    ),
    (
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c", // dave_id
        "1398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93ca", // dave_dev
    ),
    (
        "fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618", // mal_id
        "43a72e714401762df66b68c26dfbdf2682aaec9f2474eca4613e424a0fbafd3c", // mal_dev
    ),
];

// ---------------------------------------------------------------------------
// Room ids (Tier 1) and golden-serialization ids (Tier 1).
// ---------------------------------------------------------------------------

/// `room_id_A` — Alice, `ROOM_NONCE`, `T_ROOM` (spike: `43c19f2e…16a3`).
pub const ROOM_ID_A_HEX: &str = "43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3";
/// `room_id_B` — same creator/nonce, `T_ROOM + 1` (spike: `cad9174a…3494`).
pub const ROOM_ID_B_HEX: &str = "cad9174a1e34a847711e85968020a5cabaf9b35ed600d21457458f95c9c53494";

/// The golden `message.text` `event_id` (spike vector §1/§3).
pub const GOLDEN_EVENT_ID: &str =
    "blake3:c389e251f9654902d26ea937b3e84a01bb5e5d578e394c95b6ade8b7144e85a1";
/// The tampered-body id (spike vector §6).
pub const TAMPERED_EVENT_ID: &str =
    "blake3:6267b72c066e30154b34d4430ce8fb735563c4500ff527d371bcc3de7f34c75c";
/// The cross-room re-signed id (spike vector §7).
pub const CROSS_ROOM_EVENT_ID: &str =
    "blake3:81b6a82b4a1e22291102a80a454f935ddbc87d4e15d0de400d6eb6aadb85f057";

/// Room A: the room every fixture-log event belongs to.
pub fn room_id() -> RoomId {
    signed::derive_room_id(&alice_id(), &ROOM_NONCE, T_ROOM)
}

/// Room B: same creator/nonce, `created_at = T_ROOM + 1` (vector §7).
pub fn room_id_b() -> RoomId {
    signed::derive_room_id(&alice_id(), &ROOM_NONCE, T_ROOM + 1)
}

// ---------------------------------------------------------------------------
// Capability fixtures (spike Fixtures "Invite capabilities").
// ---------------------------------------------------------------------------

/// Bob's invite handle: `b0b0…b0b0`.
pub const BOB_INVITE_ID: [u8; 16] = [0xb0; 16];
/// Bob's capability secret: `5ec0…b0b0` (`5ec0` prefix, `b0` filler).
pub const BOB_SECRET: [u8; 16] = [
    0x5e, 0xc0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0,
];
/// Dave's invite handle: `da7e…da7e`.
pub const DAVE_INVITE_ID: [u8; 16] = [
    0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e, 0xda, 0x7e,
];
/// Dave's capability secret: `5ec0da7e…`.
pub const DAVE_SECRET: [u8; 16] = [
    0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda, 0x7e, 0x5e, 0xc0, 0xda, 0x7e,
];

/// `E_file.content.blob_hash` (spike Fixtures: `dd101e8f…f313`).
pub const BLOB_HASH: [u8; 32] = [
    0xdd, 0x10, 0x1e, 0x8f, 0x6f, 0xcf, 0x00, 0x5b, 0x1d, 0xd4, 0x78, 0x0c, 0x4f, 0x7b, 0x73, 0x6c,
    0x4f, 0x45, 0x6c, 0xe2, 0x92, 0xe5, 0x0a, 0x89, 0x6d, 0x1f, 0x40, 0xdf, 0x6d, 0xbe, 0xf3, 0x13,
];

/// Bob's regenerated capability hash. The spike listed `7ed58c86…d4a4`; that
/// value was an un-reproduced placeholder — its exact `(invite_id, secret)` bytes
/// were never pinned — so we regenerate from the concrete byte patterns above and
/// lock the produced value (spec §4.3 Tier-2 rule).
pub const BOB_CAP_HEX: &str = "1292c3a3a82ea95ed0300ed9cff6d4e9fb8c9ff87cfe407b64c03a826e1d3251";
/// Dave's regenerated capability hash. Spike listed `b944bc4d…447c` (an
/// un-reproduced placeholder); regenerated and pinned here.
pub const DAVE_CAP_HEX: &str = "546c6498202435429d0068345423b748d6d14627b74f0e495a36165cb26f2279";

pub fn bob_cap() -> [u8; 32] {
    capability_hash(&room_id(), &BOB_INVITE_ID, &BOB_SECRET)
}

pub fn dave_cap() -> [u8; 32] {
    capability_hash(&room_id(), &DAVE_INVITE_ID, &DAVE_SECRET)
}

// ---------------------------------------------------------------------------
// Tier-2 fixture-log golden ids (regenerated & pinned — see module doc).
// ---------------------------------------------------------------------------

pub const E_CREATE_ID: &str =
    "blake3:44b6e3ccf4df45cfdeb445734afb602f418f927eab7b6b5b059be23f25028a9d";
pub const E_INV_BOB_ID: &str =
    "blake3:ab579b0c84bee32c7df09e23d2fe14a5498b2c83ea268ad159aa896b8faad610";
pub const E_JOIN_BOB_ID: &str =
    "blake3:06567f76aeed1b1833ba48bf20350183e8d8691d8960a93ea2f667600bdf78df";
pub const E_INV_CAROL_ID: &str =
    "blake3:a1c7136d06786e45cbc1dd6f45b0e21226ead9ba5e3fb638b183e96845b4268f";
pub const E_JOIN_CAROL_ID: &str =
    "blake3:fa02adeaa622116e28fd5d7b253d5f257a6af2897024801406c66dc7a3353e8b";
pub const E_MSG_BOB_ID: &str =
    "blake3:78cd95cffc381ddf0a69111a82871ff20d719c60c8deabfa08f629f28aad1635";
pub const E_INV_DAVE_ID: &str =
    "blake3:0fcf3c35e7a400c35d5c6d82fd4164042878da59a61afd8288693920e1a508a3";
pub const E_JOIN_DAVE_ID: &str =
    "blake3:54e1a51c79283a935f44dc66b29dc8633887f99c8d31950d20b0caf43a35a7c6";
pub const E_KICK_DAVE_ID: &str =
    "blake3:73c0826ae10e2b4c055bbe417f541d7cff1afeb3776d8a3de0039884cf53de40";
pub const E_FILE_ID: &str =
    "blake3:7e5dbaa128515acf49a86a97261c588b1ae72ddee23f5b0710ece3076d4998c1";
pub const E_PIPE_ID: &str =
    "blake3:81244b596ab651ccdd144dc8e1fe5b6766eafa6fa0af56af3dac6fa03545079f";

// ---------------------------------------------------------------------------
// Low-level assembly helpers.
// ---------------------------------------------------------------------------

/// Validate a freshly-sealed `WireEvent` through the stateless §6 pipeline for
/// room A. Every fixture-log event is stateless-valid by construction.
fn validate(wire: &WireEvent) -> ValidatedEvent {
    validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id()))
        .expect("fixture event must pass stateless validation")
}

/// Seal + validate an assembled `SignedEvent` signed by `dev` (for the event
/// types that have no public builder: `member.removed`, `member.left`,
/// `file.shared`). Mirrors the `signed_bytes_with` helpers in `golden_vectors.rs`.
fn seal_validate(event: &SignedEvent, dev: &SigningKey) -> ValidatedEvent {
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    let wire = WireEvent::seal(csb, sig);
    validate(&wire)
}

// ---------------------------------------------------------------------------
// Public per-type builders (reused by vectors that assemble ad-hoc events).
// ---------------------------------------------------------------------------

/// A validated admin `member.invited` (role, key-bound to `invitee`), via the
/// landed [`build_member_invited`].
#[allow(clippy::too_many_arguments)]
pub fn invite(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    invite_id: &[u8; 16],
    secret: &[u8; 16],
    role: &str,
    invitee: &IdentityKey,
    expires_at: Option<u64>,
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let cap = capability_hash(&room_id(), invite_id, secret);
    let wire = build_member_invited(
        admin_id,
        admin_dev,
        &room_id(),
        invite_id,
        &cap,
        role,
        invitee,
        expires_at,
        None,
        prev,
        created_at,
    );
    validate(&wire)
}

/// A validated `member.joined` citing `via_invite_id` with `secret`, via the
/// landed [`build_member_joined`] (self-contained device binding built here).
#[allow(clippy::too_many_arguments)]
pub fn join(
    invitee_id: &SigningKey,
    invitee_dev: &SigningKey,
    via_invite_id: &[u8; 16],
    secret: &[u8; 16],
    role: &str,
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let binding = DeviceBinding::create(&room_id(), invitee_id, invitee_dev.device_key());
    let wire = build_member_joined(
        invitee_id,
        invitee_dev,
        &room_id(),
        via_invite_id,
        secret,
        role,
        binding,
        None,
        prev,
        created_at,
    );
    validate(&wire)
}

/// A validated `message.text` authored by `(id, dev)`, via [`build_message_text`].
pub fn message(
    id: &SigningKey,
    dev: &SigningKey,
    body: &str,
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let wire = build_message_text(id, dev, &room_id(), body, None, None, &[], prev, created_at);
    validate(&wire)
}

/// A validated admin `member.removed` of `member_id` (local assembler — no
/// public builder exists for this type, per spec non-goal §2).
pub fn member_removed(
    admin_id: &SigningKey,
    admin_dev: &SigningKey,
    member_id: &IdentityKey,
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let event = SignedEvent {
        schema_version: 1,
        room_id: room_id(),
        sender_id: admin_id.identity_key(),
        device_id: admin_dev.device_key(),
        event_type: EventType::MemberRemoved,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberRemoved(MemberRemoved {
            member_id: *member_id,
            removed_by: admin_id.identity_key(),
            reason: None,
            device_binding: None,
        }),
    };
    seal_validate(&event, admin_dev)
}

/// A validated self `member.left` of `sender` (local assembler).
pub fn member_left(
    id: &SigningKey,
    dev: &SigningKey,
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let event = SignedEvent {
        schema_version: 1,
        room_id: room_id(),
        sender_id: id.identity_key(),
        device_id: dev.device_key(),
        event_type: EventType::MemberLeft,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::MemberLeft(MemberLeft {
            member_id: id.identity_key(),
            reason: None,
        }),
    };
    seal_validate(&event, dev)
}

/// A validated `file.shared` referencing `blob_hash`, authored by `(id, dev)`
/// (local assembler).
pub fn file_shared(
    id: &SigningKey,
    dev: &SigningKey,
    blob_hash: [u8; 32],
    prev: &[EventId],
    created_at: u64,
) -> ValidatedEvent {
    let event = SignedEvent {
        schema_version: 1,
        room_id: room_id(),
        sender_id: id.identity_key(),
        device_id: dev.device_key(),
        event_type: EventType::FileShared,
        created_at,
        prev_events: prev.to_vec(),
        content: Content::FileShared(FileShared {
            file_id: [0xf1; 16],
            name: "report.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 1024,
            blob_hash: HashRef::from_bytes(blob_hash),
            blob_format: None,
            providers: None,
        }),
    };
    seal_validate(&event, dev)
}

// ---------------------------------------------------------------------------
// The golden serialization event (spike vector §1) — a byte-level fixture, NOT
// a live event (`prev_events=[]`, so as a real `message.text` it is rejected).
// ---------------------------------------------------------------------------

/// The inline golden `message.text` event of vector §1 (`content =
/// {"body":"Hello room","format":"plain"}`, `prev_events=[]`).
pub fn golden_event() -> SignedEvent {
    use iroh_rooms_core::event::content::MessageText;
    SignedEvent {
        schema_version: 1,
        room_id: room_id(),
        sender_id: alice_id(),
        device_id: alice_dev(),
        event_type: EventType::MessageText,
        created_at: GOLDEN_CREATED_AT,
        prev_events: vec![],
        content: Content::MessageText(MessageText {
            body: "Hello room".to_owned(),
            format: Some("plain".to_owned()),
            in_reply_to: None,
            mentions: None,
        }),
    }
}

/// The golden event with a single byte of `content.body` flipped
/// (`"Hello room"` → `"Hello rooM"`) — vector §6.
pub fn golden_event_tampered() -> SignedEvent {
    use iroh_rooms_core::event::content::MessageText;
    let mut ev = golden_event();
    ev.content = Content::MessageText(MessageText {
        body: "Hello rooM".to_owned(),
        format: Some("plain".to_owned()),
        in_reply_to: None,
        mentions: None,
    });
    ev
}

// ---------------------------------------------------------------------------
// The validated fixture-log DAG (spike Fixtures "Fixture log").
// ---------------------------------------------------------------------------

/// The full validated fixture-log DAG: a linear chain plus the deliberate
/// concurrent `E_join_dave` / `E_kick_dave` fork, all derived from one canonical
/// source so every vector shares identical bytes.
pub struct FixtureLog {
    pub room_id: RoomId,
    pub e_create: ValidatedEvent,
    pub e_inv_bob: ValidatedEvent,
    pub e_join_bob: ValidatedEvent,
    pub e_inv_carol: ValidatedEvent,
    pub e_join_carol: ValidatedEvent,
    pub e_msg_bob: ValidatedEvent,
    pub e_inv_dave: ValidatedEvent,
    pub e_join_dave: ValidatedEvent,
    pub e_kick_dave: ValidatedEvent,
    pub e_file: ValidatedEvent,
    pub e_pipe: ValidatedEvent,
}

impl FixtureLog {
    /// The entire accepted set, in a fold-order-independent vector — the input
    /// to the current-snapshot access vectors (§16/§17) and the total-order
    /// vector (§10).
    pub fn all(&self) -> Vec<ValidatedEvent> {
        vec![
            self.e_create.clone(),
            self.e_inv_bob.clone(),
            self.e_join_bob.clone(),
            self.e_inv_carol.clone(),
            self.e_join_carol.clone(),
            self.e_msg_bob.clone(),
            self.e_inv_dave.clone(),
            self.e_join_dave.clone(),
            self.e_kick_dave.clone(),
            self.e_file.clone(),
            self.e_pipe.clone(),
        ]
    }

    /// The prefix a fresh peer holds in vector §9 (`E_create … E_join_carol`).
    pub fn prefix_through_join_carol(&self) -> Vec<ValidatedEvent> {
        vec![
            self.e_create.clone(),
            self.e_inv_bob.clone(),
            self.e_join_bob.clone(),
            self.e_inv_carol.clone(),
            self.e_join_carol.clone(),
        ]
    }
}

/// Build the full fixture-log DAG deterministically from the canonical source.
#[allow(clippy::too_many_lines)] // one cohesive DAG assembly; splitting fragments it
pub fn log() -> FixtureLog {
    let (alice_id_k, alice_dev_k) = (alice_id_sk(), alice_dev_sk());
    let (bob_id_k, bob_dev_k) = (bob_id_sk(), bob_dev_sk());
    let (carol_id_k, carol_dev_k) = (carol_id_sk(), carol_dev_sk());
    let (dave_id_k, dave_dev_k) = (dave_id_sk(), dave_dev_sk());

    // E_create — genesis room.created by Alice.
    let e_create = validate(&build_room_created(
        &alice_id_k,
        &alice_dev_k,
        "Conformance Room",
        &ROOM_NONCE,
        T_ROOM,
    ));

    // E_inv_bob — admin invites Bob (key-bound).
    let e_inv_bob = invite(
        &alice_id_k,
        &alice_dev_k,
        &BOB_INVITE_ID,
        &BOB_SECRET,
        "member",
        &bob_id(),
        None,
        &[e_create.event_id],
        T_ROOM + 1_000,
    );

    // E_join_bob — Bob joins.
    let e_join_bob = join(
        &bob_id_k,
        &bob_dev_k,
        &BOB_INVITE_ID,
        &BOB_SECRET,
        "member",
        &[e_inv_bob.event_id],
        T_ROOM + 2_000,
    );

    // E_inv_carol — admin invites Carol (Carol reuses Bob's handle/secret; the
    // invite is key-bound to Carol so the capability is per-subject regardless).
    let e_inv_carol = invite(
        &alice_id_k,
        &alice_dev_k,
        &BOB_INVITE_ID,
        &BOB_SECRET,
        "member",
        &carol_id(),
        None,
        &[e_join_bob.event_id],
        T_ROOM + 3_000,
    );

    // E_join_carol — Carol joins.
    let e_join_carol = join(
        &carol_id_k,
        &carol_dev_k,
        &BOB_INVITE_ID,
        &BOB_SECRET,
        "member",
        &[e_inv_carol.event_id],
        T_ROOM + 4_000,
    );

    // E_msg_bob — Bob posts "hi all".
    let e_msg_bob = message(
        &bob_id_k,
        &bob_dev_k,
        "hi all",
        &[e_join_carol.event_id],
        T_ROOM + 5_000,
    );

    // E_inv_dave — admin invites Dave (key-bound).
    let e_inv_dave = invite(
        &alice_id_k,
        &alice_dev_k,
        &DAVE_INVITE_ID,
        &DAVE_SECRET,
        "member",
        &dave_id(),
        None,
        &[e_msg_bob.event_id],
        T_ROOM + 6_000,
    );

    // E_join_dave / E_kick_dave — the concurrent fork on parent [E_inv_dave].
    let e_join_dave = join(
        &dave_id_k,
        &dave_dev_k,
        &DAVE_INVITE_ID,
        &DAVE_SECRET,
        "member",
        &[e_inv_dave.event_id],
        T_ROOM + 7_000,
    );
    let e_kick_dave = member_removed(
        &alice_id_k,
        &alice_dev_k,
        &dave_id(),
        &[e_inv_dave.event_id],
        T_ROOM + 7_000,
    );

    // E_file — Bob shares a blob (parent [E_msg_bob]).
    let e_file = file_shared(
        &bob_id_k,
        &bob_dev_k,
        BLOB_HASH,
        &[e_msg_bob.event_id],
        T_ROOM + 6_000,
    );

    // E_pipe — Bob opens a pipe allowing [alice_id, bob_id] (parent [E_file]).
    let e_pipe = validate(&build_pipe_opened(
        &bob_id_k,
        &bob_dev_k,
        &room_id(),
        [0x9a; 16],
        &bob_dev(),
        "dev-server",
        "localhost:3000",
        "/iroh-rooms/pipe/1",
        &[alice_id(), bob_id()],
        None,
        &[e_file.event_id],
        T_ROOM + 7_000,
    ));

    FixtureLog {
        room_id: room_id(),
        e_create,
        e_inv_bob,
        e_join_bob,
        e_inv_carol,
        e_join_carol,
        e_msg_bob,
        e_inv_dave,
        e_join_dave,
        e_kick_dave,
        e_file,
        e_pipe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Tier 1: seeds reproduce the published Cast public keys. -------------

    #[test]
    fn seeds_reproduce_cast_public_keys() {
        let seeds = [
            (alice_id(), alice_dev()),
            (bob_id(), bob_dev()),
            (carol_id(), carol_dev()),
            (dave_id(), dave_dev()),
            (mallory_id(), mallory_dev()),
        ];
        for ((id, dev), (id_hex, dev_hex)) in seeds.iter().zip(CAST_HEX) {
            assert_eq!(&hex::encode(id.as_bytes()), id_hex, "identity key mismatch");
            assert_eq!(&hex::encode(dev.as_bytes()), dev_hex, "device key mismatch");
        }
    }

    // --- Tier 1: room ids reproduce the spike hex. ---------------------------

    #[test]
    fn room_ids_reproduce_spike_hex() {
        assert_eq!(hex::encode(room_id().as_bytes()), ROOM_ID_A_HEX);
        assert_eq!(hex::encode(room_id_b().as_bytes()), ROOM_ID_B_HEX);
    }

    // --- Tier 2: fixture-log ids are pinned (regression tripwire). -----------

    #[test]
    fn fixture_log_ids_are_pinned() {
        let log = log();
        let pinned = [
            (&log.e_create, E_CREATE_ID, "E_create"),
            (&log.e_inv_bob, E_INV_BOB_ID, "E_inv_bob"),
            (&log.e_join_bob, E_JOIN_BOB_ID, "E_join_bob"),
            (&log.e_inv_carol, E_INV_CAROL_ID, "E_inv_carol"),
            (&log.e_join_carol, E_JOIN_CAROL_ID, "E_join_carol"),
            (&log.e_msg_bob, E_MSG_BOB_ID, "E_msg_bob"),
            (&log.e_inv_dave, E_INV_DAVE_ID, "E_inv_dave"),
            (&log.e_join_dave, E_JOIN_DAVE_ID, "E_join_dave"),
            (&log.e_kick_dave, E_KICK_DAVE_ID, "E_kick_dave"),
            (&log.e_file, E_FILE_ID, "E_file"),
            (&log.e_pipe, E_PIPE_ID, "E_pipe"),
        ];
        for (ev, expected, name) in pinned {
            assert_eq!(
                ev.event_id.to_named_string(),
                expected,
                "regenerated {name} id drifted from its pinned Tier-2 golden"
            );
        }
    }

    // --- Capability fixtures (regenerated & pinned). -------------------------

    #[test]
    fn capability_hashes_are_pinned() {
        assert_eq!(hex::encode(bob_cap()), BOB_CAP_HEX, "Bob cap drifted");
        assert_eq!(hex::encode(dave_cap()), DAVE_CAP_HEX, "Dave cap drifted");
    }

    // --- Harvest helper: prints every Tier-2 golden for pinning. Run with
    //     `--ignored --nocapture` when regenerating; asserts nothing. --------
    #[test]
    #[ignore = "regeneration utility; run explicitly to reharvest Tier-2 goldens"]
    fn zzz_harvest_tier2_goldens() {
        let log = log();
        println!("E_CREATE_ID = {}", log.e_create.event_id.to_named_string());
        println!(
            "E_INV_BOB_ID = {}",
            log.e_inv_bob.event_id.to_named_string()
        );
        println!(
            "E_JOIN_BOB_ID = {}",
            log.e_join_bob.event_id.to_named_string()
        );
        println!(
            "E_INV_CAROL_ID = {}",
            log.e_inv_carol.event_id.to_named_string()
        );
        println!(
            "E_JOIN_CAROL_ID = {}",
            log.e_join_carol.event_id.to_named_string()
        );
        println!(
            "E_MSG_BOB_ID = {}",
            log.e_msg_bob.event_id.to_named_string()
        );
        println!(
            "E_INV_DAVE_ID = {}",
            log.e_inv_dave.event_id.to_named_string()
        );
        println!(
            "E_JOIN_DAVE_ID = {}",
            log.e_join_dave.event_id.to_named_string()
        );
        println!(
            "E_KICK_DAVE_ID = {}",
            log.e_kick_dave.event_id.to_named_string()
        );
        println!("E_FILE_ID = {}", log.e_file.event_id.to_named_string());
        println!("E_PIPE_ID = {}", log.e_pipe.event_id.to_named_string());
        println!("BOB_CAP = {}", hex::encode(bob_cap()));
        println!("DAVE_CAP = {}", hex::encode(dave_cap()));
    }
}
