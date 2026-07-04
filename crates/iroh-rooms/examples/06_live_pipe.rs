//! Step 6 of `docs/getting-started.md`: expose a local TCP service as a live
//! pipe, and connect to a peer's exposed pipe (mirrors `iroh-rooms pipe
//! expose` / `pipe connect`).
//!
//! Requires: `--features experimental`. Compile-only in CI — running it needs
//! a second live peer; substitute `ALLOWED_MEMBER`, `OWNER_ENDPOINT`, and
//! `PIPE_ID`.

#[cfg(feature = "experimental")]
use std::net::SocketAddr;
#[cfg(feature = "experimental")]
use std::sync::Arc;

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
/// Substitute the connecting peer's identity (the `--allow` value `pipe
/// expose` needs).
#[cfg(feature = "experimental")]
const ALLOWED_MEMBER: &str = "<PASTE_ALLOWED_MEMBER_IDENTITY_HERE>";
/// Substitute the pipe owner's `EndpointId` (printed by `pipe_expose`'s node,
/// or `pipe list`'s `owner` field) — only needed by the connector role below.
#[cfg(feature = "experimental")]
const OWNER_ENDPOINT: &str = "<PASTE_OWNER_ENDPOINT_ID_HERE>";
/// Substitute the `pipe_id` printed by `pipe_expose` — only needed by the
/// connector role below.
#[cfg(feature = "experimental")]
const PIPE_ID: &str = "<PASTE_PIPE_ID_HERE>";

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // These two roles run on two different peers in reality (mirroring
    // `docs/getting-started.md` Step 6's two terminals); they are shown
    // sequentially here purely to illustrate both halves of the API.
    expose_pipe().await?;
    connect_to_pipe().await?;
    Ok(())
}

/// The pipe owner: expose a local TCP service, then close it.
#[cfg(feature = "experimental")]
async fn expose_pipe() -> anyhow::Result<()> {
    let room_id: RoomId = ROOM_ID.parse()?;
    let allowed_member: iroh_rooms::identity::IdentityKey = ALLOWED_MEMBER.parse()?;
    let owner_identity = SigningKey::generate();
    let owner_device = SigningKey::generate();

    let engine = SyncEngine::open(
        EventStore::open_in_memory()?,
        room_id,
        SyncConfig::default(),
    )
    .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let node = Node::spawn(
        SecretKey::from_bytes(&owner_device.to_seed()),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;

    // Expose a local dev server on 127.0.0.1:8080 — `pipe_expose` rejects any
    // non-loopback target (D6) and any empty allow-list.
    let target: SocketAddr = "127.0.0.1:8080".parse()?;
    let pipe_id = node
        .pipe_expose(
            &owner_identity,
            &owner_device,
            &room_id,
            target,
            "dev-server",
            "127.0.0.1:8080",
            &[allowed_member],
            None,
            now_ms(),
        )
        .await?;
    println!("pipe exposed: {}", hex::encode(pipe_id));

    // Tear the pipe down when done.
    node.pipe_close(
        &owner_identity,
        &owner_device,
        &room_id,
        pipe_id,
        Some("closed"),
        now_ms(),
    )
    .await?;
    node.shutdown().await?;
    Ok(())
}

/// The connector: resolve a peer's synced `pipe.opened` and forward a local
/// loopback connection to it.
#[cfg(feature = "experimental")]
async fn connect_to_pipe() -> anyhow::Result<()> {
    let room_id: RoomId = ROOM_ID.parse()?;
    let connector_identity = SigningKey::generate();
    let connector_device = SigningKey::generate();

    let engine = SyncEngine::open(
        EventStore::open_in_memory()?,
        room_id,
        SyncConfig::default(),
    )
    .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let node = Node::spawn(
        SecretKey::from_bytes(&connector_device.to_seed()),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;
    let _ = connector_identity; // authenticated implicitly via the QUIC connection's device id

    let owner_id: EndpointId = OWNER_ENDPOINT.parse()?;
    let pipe_id: [u8; 16] = {
        let bytes = hex::decode(PIPE_ID)?;
        bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("PIPE_ID must decode to 16 bytes"))?
    };

    // 0 ⇒ let the OS assign the local loopback listener's port.
    let forwarder = node
        .pipe_connect(EndpointAddr::new(owner_id), pipe_id, 0)
        .await?;
    println!("pipe connected, forwarding {}", forwarder.local_addr());

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
