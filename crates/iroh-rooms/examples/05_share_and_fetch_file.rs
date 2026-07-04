//! Step 5 of `docs/getting-started.md`: import a file into the local blob
//! store, author + publish its `file.shared` reference, and fetch a file a
//! peer already shared (mirrors `iroh-rooms file share` / `file fetch`).
//!
//! Requires: `--features experimental`. Compile-only in CI — actually serving
//! a blob needs a long-running provider session (`Node::spawn_room` with a
//! `BlobServeConfig`, as `room tail` runs); this example shows the
//! **consumer** side (`Node::fetch_file`, "a pure consumer call — this node
//! need not itself serve blobs"). Substitute `PROVIDER_ENDPOINT` and the
//! declared hash from the `file.shared` event you synced.

#[cfg(feature = "experimental")]
use std::path::Path;
#[cfg(feature = "experimental")]
use std::sync::Arc;
#[cfg(feature = "experimental")]
use std::time::Duration;

#[cfg(feature = "experimental")]
use iroh_rooms::events::HashRef;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::blob::{BlobStore, FetchOutcome};
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
use iroh_rooms::files::build_file_shared;
#[cfg(feature = "experimental")]
use iroh_rooms::identity::SigningKey;
#[cfg(feature = "experimental")]
use iroh_rooms::room::RoomId;

/// Substitute the room id printed by `02_create_room.rs` / `room create`.
#[cfg(feature = "experimental")]
const ROOM_ID: &str = "<PASTE_ROOM_ID_HERE>";
/// Substitute the provider's `EndpointId` (the peer whose `room tail` is
/// serving the file).
#[cfg(feature = "experimental")]
const PROVIDER_ENDPOINT: &str = "<PASTE_PROVIDER_ENDPOINT_ID_HERE>";

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let room_id: RoomId = ROOM_ID.parse()?;
    let sender_identity = SigningKey::generate();
    let sender_device = SigningKey::generate();

    // ---- Share: import + author (this half needs no network). ----
    let blobs_dir = std::env::temp_dir().join("iroh-rooms-sdk-example-blobs");
    let store = BlobStore::open(&blobs_dir).await?;
    // `import_path` requires an absolute path.
    let sample_path = std::env::temp_dir().join("iroh-rooms-sdk-example.txt");
    std::fs::write(&sample_path, b"hello from the Rust SDK example")?;
    let import = store
        .import_path(&std::fs::canonicalize(&sample_path)?)
        .await?;
    store.close().await?;

    let wire = build_file_shared(
        &sender_identity,
        &sender_device,
        &room_id,
        [0x01; 16],
        Path::new(&sample_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("shared-file"),
        "text/plain",
        import.size_bytes,
        HashRef::from_bytes(import.hash),
        None,
        &[],
        &[],
        now_ms(),
    );
    println!("file.shared event built ({} bytes)", import.size_bytes);
    // A real session publishes `wire` through a `Node` (as `04_send_message.rs`
    // shows) so the room learns about it and the sharer's `room tail` starts
    // serving the blob over the ACL-gated blob ALPN.
    let _ = wire;

    // ---- Fetch: verified retrieval from a peer who already has the blob. ----
    let fetcher_identity = SigningKey::generate();
    let fetcher_device = SigningKey::generate();
    let engine = SyncEngine::open(
        EventStore::open_in_memory()?,
        room_id,
        SyncConfig::default(),
    )
    .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let node = Node::spawn(
        SecretKey::from_bytes(&fetcher_device.to_seed()),
        Arc::new(AllowlistAdmission::new()),
        Arc::new(TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;
    let _ = fetcher_identity; // would sign the room-side `file.shared` if fetching triggered one

    let provider_id: EndpointId = PROVIDER_ENDPOINT.parse()?;
    let declared = import.hash; // the `blob_hash` from the synced `file.shared` event
    let (outcome, bytes) = node
        .fetch_file(
            EndpointAddr::new(provider_id),
            declared,
            declared,
            Duration::from_secs(10),
        )
        .await;
    match outcome {
        FetchOutcome::Fetched => println!("fetched {} bytes", bytes.map_or(0, |b| b.len())),
        other => println!("fetch did not complete: {other:?}"),
    }

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
