//! A pure protocol walkthrough with **no network**: create a room, invite a
//! member, author + validate a message, and fold the resulting membership.
//! Default features only. Run with:
//! `cargo run -p iroh-rooms --example offline_author_and_validate`
//!
//! This is the offline half of the `docs/getting-started.md` demo — every
//! step here also runs, byte-identically, inside the CLI's offline authoring
//! path (spec IR-0301 §5.4).

use iroh_rooms::events::{
    build_message_text, capability_hash, validate_wire_bytes, ValidationContext,
};
use iroh_rooms::identity::SigningKey;
use iroh_rooms::room::{
    build_member_invited, build_room_created, derive_room_id, RoomInviteTicket, RoomMembership,
};

fn main() {
    let admin_identity = SigningKey::generate();
    let admin_device = SigningKey::generate();
    let bob_identity = SigningKey::generate();

    let room_nonce = [0x42; 16];
    let t0 = 1_750_000_000_000;
    let room_id = derive_room_id(&admin_identity.identity_key(), &room_nonce, t0);

    let genesis = build_room_created(&admin_identity, &admin_device, "demo room", &room_nonce, t0);
    let ctx = ValidationContext::for_room(room_id);
    let v_genesis = validate_wire_bytes(&genesis.to_bytes(), &ctx).expect("genesis validates");

    // Invite Bob: the on-log `member.invited` carries only the capability
    // *hash*; the out-of-band `RoomInviteTicket` carries the *secret*.
    let invite_id = [0x01; 16];
    let capability_secret = [0x02; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &capability_secret);
    let invite = build_member_invited(
        &admin_identity,
        &admin_device,
        &room_id,
        &invite_id,
        &cap_hash,
        "member",
        &bob_identity.identity_key(),
        None,
        None,
        &[v_genesis.event_id],
        t0 + 1_000,
    );
    let v_invite = validate_wire_bytes(&invite.to_bytes(), &ctx).expect("invite validates");

    let ticket = RoomInviteTicket {
        room_id,
        invite_id,
        capability_secret,
        invitee_key: bob_identity.identity_key(),
        role: "member".to_owned(),
        expires_at: None,
        inviter_identity: admin_identity.identity_key(),
        discovery: vec![admin_device.device_key()],
    };
    // The ticket token is copy-paste text; `FromStr` decodes it back.
    let token = ticket.to_string();
    let round_tripped: RoomInviteTicket = token.parse().expect("ticket round-trips");
    assert_eq!(round_tripped.capability_hash(), cap_hash);

    // Author + validate a message from the admin (Bob has not joined yet in
    // this trimmed walkthrough — see `03_invite_and_join.rs` for the full
    // join step, which needs the online runtime).
    let message = build_message_text(
        &admin_identity,
        &admin_device,
        &room_id,
        "hello room",
        Some("plain"),
        None,
        &[],
        &[v_invite.event_id],
        t0 + 2_000,
    );
    let v_message = validate_wire_bytes(&message.to_bytes(), &ctx).expect("message validates");

    // Fold everything validated so far into a membership snapshot.
    let mut fold = RoomMembership::new(room_id);
    fold.ingest(v_genesis);
    fold.ingest(v_invite);
    fold.ingest(v_message);
    let snapshot = fold.snapshot();

    println!("room: {room_id}");
    println!(
        "admin active: {}",
        snapshot.is_active(&admin_identity.identity_key())
    );
    println!(
        "bob status: {:?}",
        snapshot.status(&bob_identity.identity_key())
    );
    println!("invite ticket: {token}");
}
