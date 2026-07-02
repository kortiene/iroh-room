//! Step 2 of `docs/getting-started.md`: an admin creates a room.
//!
//! Default features only — no network. Run with:
//! `cargo run -p iroh-rooms --example 02_create_room`

use iroh_rooms::events::{validate_wire_bytes, ValidationContext};
use iroh_rooms::identity::SigningKey;
use iroh_rooms::room::{build_room_created, derive_room_id};

fn main() {
    let admin_identity = SigningKey::generate();
    let admin_device = SigningKey::generate();

    // A CSPRNG nonce keeps two rooms created by the same admin at the same
    // millisecond distinct.
    let room_nonce = {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).expect("OS CSPRNG must be available");
        nonce
    };
    let created_at = now_ms();

    // `derive_room_id` mirrors exactly what `build_room_created` computes
    // internally, so the caller learns `room_id` up front — before or
    // alongside authoring the genesis event.
    let room_id = derive_room_id(&admin_identity.identity_key(), &room_nonce, created_at);
    let genesis = build_room_created(
        &admin_identity,
        &admin_device,
        "Getting Started Demo",
        &room_nonce,
        created_at,
    );

    // Self-check before persisting/publishing anywhere: the genesis event
    // must validate under the room id we just derived.
    let ctx = ValidationContext::for_room(room_id);
    let validated =
        validate_wire_bytes(&genesis.to_bytes(), &ctx).expect("freshly built genesis validates");

    println!("room: {room_id}");
    println!("genesis event: {}", validated.event_id);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
