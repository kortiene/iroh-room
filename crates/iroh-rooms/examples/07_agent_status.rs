//! Step 7 of `docs/getting-started.md`: post an `agent.status` update
//! (mirrors `iroh-rooms agent status`). This is the seed for PRD §19 Phase 2
//! deliverable 8, "example agent" — the minimal shape a Rust agent integration
//! builds on: join a room (see `03_invite_and_join.rs`), then periodically
//! publish its own status.
//!
//! Requires: `--features experimental`. Compile-only in CI — substitute
//! `ROOM_ID` and `PEER_ENDPOINT`.

#[cfg(feature = "experimental")]
use std::sync::Arc;
#[cfg(feature = "experimental")]
use std::time::Duration;

#[cfg(feature = "experimental")]
use iroh::{EndpointAddr, EndpointId, SecretKey};
#[cfg(feature = "experimental")]
use iroh_rooms::events::build_agent_status;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::session::{
    AllowlistAdmission, NetConfig, Node, TracingAudit, DEFAULT_TICK,
};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::store::EventStore;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
#[cfg(feature = "experimental")]
use iroh_rooms::identity::SigningKey;
#[cfg(feature = "experimental")]
use iroh_rooms::room::RoomId;

/// Substitute the room id the agent was invited into (see
/// `iroh-rooms agent invite` / Step 3).
#[cfg(feature = "experimental")]
const ROOM_ID: &str = "<PASTE_ROOM_ID_HERE>";
/// Substitute a peer's `EndpointId` to publish through.
#[cfg(feature = "experimental")]
const PEER_ENDPOINT: &str = "<PASTE_PEER_ENDPOINT_ID_HERE>";

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let room_id: RoomId = ROOM_ID.parse()?;
    let peer_id: EndpointId = PEER_ENDPOINT.parse()?;

    // The agent's own identity/device keys — already joined via
    // `03_invite_and_join.rs`'s flow (`agent invite` + `room join`).
    let agent_identity = SigningKey::generate();
    let agent_device = SigningKey::generate();

    let engine = SyncEngine::open(
        EventStore::open_in_memory()?,
        room_id,
        SyncConfig::default(),
    )
    .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let node = Node::spawn(
        SecretKey::from_bytes(&agent_device.to_seed()),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;
    node.connect_to(EndpointAddr::new(peer_id));
    tokio::time::sleep(Duration::from_millis(500)).await; // let the dial settle

    // A minimal "example agent" loop: post progress, do work, post completion.
    for (status, message, progress) in [
        ("running_tests", Some("cargo test --workspace"), Some(10)),
        ("running_tests", Some("cargo test --workspace"), Some(80)),
        ("done", Some("all tests passed"), Some(100)),
    ] {
        let heads = node.heads().await?;
        let wire = build_agent_status(
            &agent_identity,
            &agent_device,
            &room_id,
            status,
            message,
            &[],
            progress,
            &heads,
            now_ms(),
        );
        node.publish(wire.to_bytes()).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await; // flush grace
    node.shutdown().await?;
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
