//! Non-e2e coverage for the in-session blob-import façade (issue #84 / IR-0308),
//! spec §7.2 / §7.1: both the fail-closed path (a `Node` spawned **without** a
//! `BlobServeConfig` owns no durable store, so both import methods must return the
//! coded [`BlobError::NotServing`] — AC4) *and* the delegation path (a `Node`
//! spawned **with** a `BlobServeConfig` reuses the live session's store handle and
//! returns a verified [`BlobImport`] — AC3/AC5, the `Some(blob_store)` arm the
//! `NotServing` tests can't reach).
//!
//! Both are deliberately the *most local* integration test possible: a single
//! in-process node on `NetMode::Loopback` that never dials or accepts a connection,
//! so no bytes cross a boundary — the serving node binds one loopback endpoint and
//! opens its own store, but nothing is served over the wire. The zero-disconnect /
//! serve-in-session / re-provide claims that *do* require a peer (AC1/AC2) are
//! proven over real loopback QUIC in the e2e tier (`blob_e2e.rs` and the headline
//! `blob_import_live_e2e`), not here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::demo::{self, Participant};
use iroh_rooms_net::{
    AdmissionView, BlobError, BlobImport, BlobServeConfig, NetConfig, NetMode, Node,
    SnapshotAdmission, TracingAudit, DEFAULT_TICK,
};
use tempfile::TempDir;

/// Spawn a minimal loopback [`Node::spawn`] — the **unmanaged** path, which never
/// opens a blob store, so `blob_store == None` and the import façade must fail
/// closed. No peers, no dialing: the node just binds one loopback endpoint.
///
/// Returns a boxed future so `Node::spawn`'s ~16 KB state machine is not inlined
/// into each caller (clippy `large_futures`).
fn spawn_non_serving_node() -> Pin<Box<dyn Future<Output = Node> + Send>> {
    Box::pin(async move {
        let host = Participant::new(0x84);
        let (room, _genesis_id, _genesis_bytes) = demo::genesis(&host);
        let store = EventStore::open_in_memory().expect("in-memory store");
        let engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        let cfg = NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        };
        Node::spawn(
            host.iroh_secret(),
            Arc::new(demo::allowlist(&[&host])),
            Arc::new(TracingAudit),
            engine,
            cfg,
            DEFAULT_TICK,
        )
        .await
        .expect("spawn non-serving node")
    })
}

/// Spawn a **serving** [`Node::spawn_room`] with a `BlobServeConfig` rooted at
/// `blobs_dir` — the managed path that owns an open durable store for the whole
/// session (`blob_store == Some`). This is the `Node` a resident daemon runs; the
/// import façade must route through this already-open handle (no second `FsStore`
/// open, no lock) rather than fail closed. Still fully local: it binds one loopback
/// endpoint and never dials or accepts a peer connection.
fn spawn_serving_node(
    host: &Participant,
    blobs_dir: std::path::PathBuf,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let (room, _genesis_id, genesis_bytes) = demo::genesis(host);
        let store = EventStore::open_in_memory().expect("in-memory store");
        let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("open engine");
        engine.publish(&genesis_bytes).expect("seed genesis");
        let cell = Arc::new(Mutex::new(AdmissionView::empty()));
        let admission = Arc::new(SnapshotAdmission::new(cell.clone()));
        Node::spawn_room(
            host.iroh_secret(),
            admission,
            Arc::new(TracingAudit),
            engine,
            NetConfig {
                mode: NetMode::Loopback,
                ..NetConfig::default()
            },
            DEFAULT_TICK,
            Vec::new(), // no addr hints: this test never dials the event plane
            cell,
            Some(BlobServeConfig { blobs_dir }),
        )
        .await
        .expect("spawn_room with blob serving")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_import_on_non_serving_node_reports_not_serving() {
    let node = spawn_non_serving_node().await;

    // The `NotServing` guard fires before the store is ever touched, so the path
    // need not exist — this is a coded-refusal test, not an import test.
    let err = node
        .blob_import(std::path::Path::new("/nonexistent/whatever.bin"))
        .await
        .expect_err("a node with no BlobServeConfig cannot import");
    assert!(
        matches!(err, BlobError::NotServing),
        "expected BlobError::NotServing, got: {err}"
    );
    assert!(
        err.to_string().starts_with("blob_not_serving:"),
        "the failure must carry the stable/greppable code, got: {err}"
    );

    node.shutdown().await.expect("shutdown");
}

// ── Serving node: the delegation (`Some(blob_store)`) arm — spec §7.1 / AC3+AC5 ──
// A serving node reuses the store it already owns; import returns a verified ref
// without a second `FsStore` open (so never `Locked`) and without a session cycle.
// No peer is ever connected, so this stays in the non-e2e tier.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_import_on_serving_node_returns_verified_ref() {
    let host = Participant::new(0x84);
    let tmp = TempDir::new().expect("temp dir");
    // `blobs_dir` (the node-owned store) and the source file live in separate
    // subtrees so importing never touches the store's own directory. `TempDir`
    // paths are absolute (the `blob-add-path-requires-absolute` invariant).
    let node = spawn_serving_node(&host, tmp.path().join("blobs")).await;

    let content = b"the quick brown fox jumps over the lazy dog";
    let path = tmp.path().join("fox.txt");
    std::fs::write(&path, content).expect("write source file");

    let import: BlobImport = node
        .blob_import(&path)
        .await
        .expect("a serving node imports into the store it already owns");
    assert_eq!(
        import.hash,
        *blake3::hash(content).as_bytes(),
        "the returned hash must equal an independent BLAKE3-256 over the bytes"
    );
    assert_eq!(
        import.size_bytes,
        u64::try_from(content.len()).unwrap(),
        "size_bytes must equal the source length"
    );

    node.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_import_bytes_on_serving_node_returns_verified_ref() {
    // The re-provide path: fetched bytes handed straight back to the live session.
    let host = Participant::new(0x84);
    let tmp = TempDir::new().expect("temp dir");
    let node = spawn_serving_node(&host, tmp.path().join("blobs")).await;

    let content = b"fetched bytes re-provided in the same session";
    let import: BlobImport = node
        .blob_import_bytes(bytes::Bytes::from_static(content))
        .await
        .expect("a serving node re-provides fetched bytes in-session");
    assert_eq!(
        import.hash,
        *blake3::hash(content).as_bytes(),
        "the returned hash must equal an independent BLAKE3-256 over the bytes"
    );
    assert_eq!(
        import.size_bytes,
        u64::try_from(content.len()).unwrap(),
        "size_bytes must equal the byte length"
    );

    node.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_import_routes_agree_through_the_same_live_session() {
    // `blob_import` (path) and `blob_import_bytes` (memory) both funnel to the ONE
    // store the session owns (§2.1: two handles onto one actor), so importing the
    // same content by either route through the same node must yield an identical
    // content-addressed ref — proving both façade methods reach the same store and
    // that neither needs a fresh open (a second `FsStore::load` would `Locked`).
    let host = Participant::new(0x84);
    let tmp = TempDir::new().expect("temp dir");
    let node = spawn_serving_node(&host, tmp.path().join("blobs")).await;

    let content = b"identical content, two in-session import routes";
    let path = tmp.path().join("same.bin");
    std::fs::write(&path, content).expect("write source file");

    let by_path = node.blob_import(&path).await.expect("import by path");
    let by_bytes = node
        .blob_import_bytes(bytes::Bytes::from_static(content))
        .await
        .expect("import by bytes");
    assert_eq!(
        by_path.hash, by_bytes.hash,
        "both in-session import routes must agree on the content hash"
    );
    assert_eq!(
        by_path.size_bytes, by_bytes.size_bytes,
        "both in-session import routes must report the same size"
    );

    node.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blob_import_bytes_on_non_serving_node_reports_not_serving() {
    let node = spawn_non_serving_node().await;

    let err = node
        .blob_import_bytes(bytes::Bytes::from_static(
            b"fetched bytes, nowhere to put them",
        ))
        .await
        .expect_err("a node with no BlobServeConfig cannot re-provide bytes");
    assert!(
        matches!(err, BlobError::NotServing),
        "expected BlobError::NotServing, got: {err}"
    );
    assert!(
        err.to_string().starts_with("blob_not_serving:"),
        "the failure must carry the stable/greppable code, got: {err}"
    );

    node.shutdown().await.expect("shutdown");
}
