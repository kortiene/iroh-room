//! Step 4 of `docs/getting-started.md`: send a `message.text` over the
//! network (mirrors `iroh-rooms room send`).
//!
//! Requires: `--features experimental`. Compile-only in CI — running it needs
//! a second, already-running peer to publish to; substitute `PEER_ENDPOINT`.
//! This is a trimmed illustration of `crates/iroh-rooms-cli/src/message.rs`'s
//! `run_push`.
//!
//! The library emits `tracing` events but installs no subscriber (this repo's
//! "CLI has no tracing subscriber" note applies to the SDK too) — call
//! `tracing_subscriber::fmt::init()` yourself if you want to see them.

#[cfg(feature = "experimental")]
use std::sync::Arc;
#[cfg(feature = "experimental")]
use std::time::Duration;

#[cfg(feature = "experimental")]
use iroh_rooms::events::build_message_text;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::session::{
    AllowlistAdmission, EndpointAddr, EndpointId, NetConfig, Node, SecretKey, TracingAudit,
    DEFAULT_TICK,
};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::store::EventStore;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
#[cfg(feature = "experimental")]
use iroh_rooms::identity::SigningKey;
#[cfg(feature = "experimental")]
use iroh_rooms::room::RoomId;

/// Substitute the room id printed by `02_create_room.rs` / `room create`.
#[cfg(feature = "experimental")]
const ROOM_ID: &str = "<PASTE_ROOM_ID_HERE>";
/// Substitute the peer's `EndpointId` (printed as `listening: <id>`).
#[cfg(feature = "experimental")]
const PEER_ENDPOINT: &str = "<PASTE_PEER_ENDPOINT_ID_HERE>";

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let room_id: RoomId = ROOM_ID.parse()?;
    let peer_id: EndpointId = PEER_ENDPOINT.parse()?;

    // A short-lived `room send` reopens the local store already populated by
    // `room create` / a prior join; this example starts from an empty
    // in-memory store, so it publishes a message with no causal parent
    // recorded locally (a real session would read its own `heads()` first).
    let sender_identity = SigningKey::generate();
    let sender_device = SigningKey::generate();

    let store = EventStore::open_in_memory()?;
    let engine = SyncEngine::open(store, room_id, SyncConfig::default())
        .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let secret_key = SecretKey::from_bytes(&sender_device.to_seed());

    // Every peer we have not folded as `Active` yet is rejected by default;
    // a real `room send` builds this from the local membership snapshot
    // (`AllowlistAdmission::from_snapshot`-equivalent — see `room.rs`).
    let admission = AllowlistAdmission::new();

    let node = Node::spawn(
        secret_key,
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;

    node.connect_to(EndpointAddr::new(peer_id));
    tokio::time::sleep(Duration::from_millis(500)).await; // let the dial settle

    let heads = node.heads().await?;
    let wire = build_message_text(
        &sender_identity,
        &sender_device,
        &room_id,
        "hello from the Rust SDK",
        Some("plain"),
        None,
        &[],
        &heads,
        now_ms(),
    );
    node.publish(wire.to_bytes()).await?;

    tokio::time::sleep(Duration::from_millis(500)).await; // flush grace
    node.shutdown().await?;
    println!("sent to room {room_id}");
    Ok(())
}

#[cfg(feature = "experimental")]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(not(feature = "experimental"))]
fn main() {
    eprintln!("this example requires `--features experimental` (see the module doc header)");
}
