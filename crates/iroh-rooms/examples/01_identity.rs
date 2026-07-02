//! Step 1 of `docs/getting-started.md`: create an identity + device key pair.
//!
//! Default features only — no network. Run with:
//! `cargo run -p iroh-rooms --example 01_identity`
//!
//! Iroh Rooms MVP is **one device per identity** (spec IR-0301 AC4) — there is
//! no device-*set* API to reach for here.

use iroh_rooms::identity::SigningKey;

fn main() {
    // Every participant (human or agent) holds two Ed25519 secrets: an
    // identity secret (the stable principal, `sender_id`) and a device secret
    // (`device_id`, the key that actually signs events). Persist both
    // secrets' seeds (`SigningKey::to_seed`) somewhere durable in a real
    // program — this example keeps them in memory only.
    let identity_secret = SigningKey::generate();
    let device_secret = SigningKey::generate();

    let identity_key = identity_secret.identity_key();
    let device_key = device_secret.device_key();

    println!("identity: {identity_key}");
    println!("device:   {device_key}");
}
