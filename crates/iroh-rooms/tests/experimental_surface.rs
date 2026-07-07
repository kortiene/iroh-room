//! Experimental-surface drift tripwire (issue #36 / IR-0301, spec §11 L3/L5, R3).
//!
//! The whole file is gated on `#![cfg(feature = "experimental")]`, so a
//! default-features build compiles it to an empty test binary — a direct
//! demonstration of the load-bearing feature gate (spec D4): the experimental
//! tier is not even *nameable* without the feature. Under
//! `--features experimental` (which `scripts/verify.sh` exercises via its
//! `--all-features` run), every `pub use` path in `iroh_rooms::experimental::*`
//! is referenced here, so any re-export drift (spec R3) becomes a compile error
//! caught in CI rather than by a downstream consumer.
//!
//! These checks are deliberately *offline*: they resolve paths, prove the trait
//! re-exports via their re-exported concrete impls, and exercise the two pure
//! helpers (`new_pipe_id`, `is_loopback_target`). Anything that dials a peer
//! belongs in the e2e tier, not here.

#![cfg(feature = "experimental")]

use std::net::SocketAddr;
use std::sync::Arc;

use iroh_rooms::experimental::{blob, pipe_runtime, session, store, sync};

/// Force a type path to resolve at compile time (the drift guard). `?Sized` so a
/// `dyn Trait` re-export (e.g. `dyn SyncTransport`) can be named too.
fn name_of<T: ?Sized>() -> &'static str {
    std::any::type_name::<T>()
}

/// Prove a type is re-exported *and* implements the (re-exported) `Admission`
/// trait — the bound makes the trait path load-bearing too.
fn admission_name<T: session::Admission>() -> &'static str {
    std::any::type_name::<T>()
}

/// Same, for the `AuditSink` trait re-export.
fn audit_name<T: session::AuditSink>() -> &'static str {
    std::any::type_name::<T>()
}

/// Same, for the `PipeAuditSink` trait re-export.
fn pipe_audit_name<T: pipe_runtime::PipeAuditSink>() -> &'static str {
    std::any::type_name::<T>()
}

#[test]
fn experimental_session_paths_resolve() {
    assert!(!name_of::<session::Node>().is_empty());
    assert!(!name_of::<session::NetConfig>().is_empty());
    assert!(!name_of::<session::NetMode>().is_empty());
    assert!(!name_of::<session::NetTransport>().is_empty());
    assert!(!name_of::<session::AdmissionDecision>().is_empty());
    assert!(!name_of::<session::AdmissionView>().is_empty());
    assert!(!name_of::<session::AllowlistAdmission>().is_empty());
    assert!(!name_of::<session::JoinBootstrapAdmission>().is_empty());
    assert!(!name_of::<session::SnapshotAdmission>().is_empty());
    assert!(!name_of::<session::BlobDenyCause>().is_empty());
    assert!(!name_of::<session::BlobServeConfig>().is_empty());
    assert!(!name_of::<session::ConnEvent>().is_empty());
    assert!(!name_of::<session::EventProtocolHandler>().is_empty());
    assert!(!name_of::<session::Inbound>().is_empty());
    assert!(!name_of::<session::Shared>().is_empty());
    assert!(!name_of::<session::PeerConnState>().is_empty());
    assert!(!name_of::<session::PeerEntry>().is_empty());
    assert!(!name_of::<session::PeerManager>().is_empty());
    assert!(!name_of::<session::PeerTable>().is_empty());
    assert!(!name_of::<session::RejectCause>().is_empty());
    assert!(!name_of::<session::OfflineReason>().is_empty());
    assert!(!name_of::<session::TracingAudit>().is_empty());

    // iroh transport identities re-exported verbatim (issue #87) — the paths must
    // resolve here; `iroh_transport_reexports_are_the_net_api_types` below proves
    // they are the *same* types `iroh-rooms-net` names, not just any resolvable one.
    assert!(!name_of::<session::EndpointAddr>().is_empty());
    assert!(!name_of::<session::EndpointId>().is_empty());
    assert!(!name_of::<session::SecretKey>().is_empty());
    assert!(!name_of::<session::Endpoint>().is_empty());

    // Trait re-exports, proven through their re-exported concrete impls.
    assert!(!admission_name::<session::AllowlistAdmission>().is_empty());
    assert!(!admission_name::<session::SnapshotAdmission>().is_empty());
    assert!(!audit_name::<session::TracingAudit>().is_empty());

    // Const re-exports.
    assert!(!session::EVENT_ALPN.is_empty());
    assert!(session::DEFAULT_TICK > core::time::Duration::ZERO);
}

/// Compile-only signature tripwire for `Node::room_events` (issue #83 / IR-0307):
/// locks the method's exact type so a future signature drift (wrong event type,
/// wrong channel kind) is a compile error here, not a downstream surprise.
#[test]
fn room_events_signature_is_locked() {
    let _: fn(&session::Node) -> tokio::sync::broadcast::Receiver<store::StoredEvent> =
        session::Node::room_events;
}

/// Compile-only signature tripwire for the in-session blob-import façade (issue #84
/// / IR-0308): pins the argument and awaited-output types of
/// `session::Node::blob_import` / `blob_import_bytes`, so a future drift (e.g.
/// returning `(HashRef, u64)` instead of `BlobImport`, or taking `PathBuf` instead
/// of `&Path`) is a compile error here rather than a downstream surprise.
///
/// Both are `async fn`s whose return future is opaque *and* generic over the input
/// lifetimes, so — unlike the sync `room_events` lock above — they cannot be named
/// as a single `fn(..) -> Fut` pointer. The pin is instead a never-called inner
/// `async fn` that binds each awaited result to its exact type; the lock is that
/// inner fn *compiling*, which is why the `#[test]` body itself is an intentional
/// no-op.
#[test]
fn blob_import_signatures_are_locked() {
    async fn lock(node: &session::Node, path: &std::path::Path, fetched: bytes::Bytes) {
        let by_path: Result<blob::BlobImport, blob::BlobError> = node.blob_import(path).await;
        let by_bytes: Result<blob::BlobImport, blob::BlobError> =
            node.blob_import_bytes(fetched).await;
        // Reference both pins so neither is an unused binding (this never runs).
        let _ = (by_path.is_ok(), by_bytes.is_ok());
    }
    // Name the fn so it is type-checked (the actual lock) without a dead-code
    // warning under `-D warnings`; it is never executed.
    let _ = lock;
}

/// Compile-only signature tripwire for the per-pipe live-session façade (issue #86
/// / IR-0309): pins the exact shapes of `Node::live_pipe_sessions_for` (a per-pipe
/// count keyed by `[u8; 16]`) and `Node::pipe_session_info` (the `PipeSessionInfo`
/// snapshot the Pipes panel renders), reached through `experimental::session::Node`.
///
/// Both are sync `&self` reads returning owned values, so — like `room_events`
/// above — they can be named as plain `fn(..) -> _` pointers; the lock is that this
/// binds. Using `pipe_runtime::PipeSessionInfo` in the `Vec` return type also ties
/// the façade re-export to the method's real return type, so a drift between them
/// (or a change to the count's key/return type) is a compile error here, not a
/// downstream surprise. Reaching the methods on `session::Node` is the AC5
/// façade-reach check, done offline (no node is spawned).
#[test]
fn pipe_session_methods_signatures_are_locked() {
    let _: fn(&session::Node, [u8; 16]) -> usize = session::Node::live_pipe_sessions_for;
    let _: fn(&session::Node) -> Vec<pipe_runtime::PipeSessionInfo> =
        session::Node::pipe_session_info;
}

/// Type-identity regression guard for the iroh transport re-exports (issue #87),
/// done **offline** — the compile-time counterpart to `facade_e2e.rs`'s live guard.
///
/// The load-bearing correctness constraint of #87 (spec §3.2, R1) is that the
/// façade's re-exported `EndpointAddr` / `EndpointId` / `SecretKey` / `Endpoint`
/// are the *same* crate-instance types `iroh-rooms-net`'s public `Node` API names.
/// If the façade's `iroh` pin ever drifted from `-net`'s, Cargo would resolve two
/// `iroh` crates and these would silently become *different* types — the exact bug
/// the issue exists to kill.
///
/// Each `fn(..) -> _` binding coerces a real `Node`/`BlobAclView` method item into
/// a pointer typed with the *façade's* re-export. It compiles only if the façade
/// type unifies with the type the net method actually names, so any pin desync is a
/// compile error here (fast, no node spawned) rather than a downstream surprise.
#[test]
fn iroh_transport_reexports_are_the_net_api_types() {
    // Inner items first (clippy::items_after_statements). `SecretKey` is only named
    // by net's async `Node::spawn*`; an async fn's return future is unnameable, so —
    // like `blob_import_signatures_are_locked` above — we pin its `secret` parameter
    // through a never-called inner `async fn`: it compiles only if `session::SecretKey`
    // is the exact type `Node::spawn` (hence `-net`) names. Building the future
    // (without awaiting it) does no IO.
    async fn spawn_secret_lock(secret: session::SecretKey, engine: sync::SyncEngine) {
        let admission: Arc<dyn session::Admission> = Arc::new(session::AllowlistAdmission::new());
        let audit: Arc<dyn session::AuditSink> = Arc::new(session::TracingAudit);
        // Dropped, never awaited (and this fn is never called): construction alone
        // type-checks the `secret` position, which is the lock.
        drop(session::Node::spawn(
            secret,
            admission,
            audit,
            engine,
            session::NetConfig::default(),
            session::DEFAULT_TICK,
        ));
    }

    // The three `EndpointId` re-exports (session/blob/pipe_runtime) must be one and
    // the same type — a consumer mixing modules must not get incompatible ids. These
    // identity coercions compile only if all three paths name the same type, and the
    // `pipe_runtime` one ties the re-export to the real `PipeSessionInfo.device` field.
    fn session_id_is_blob_id(id: session::EndpointId) -> blob::EndpointId {
        id
    }
    fn session_id_is_pipe_id(id: session::EndpointId) -> pipe_runtime::EndpointId {
        id
    }
    fn pipe_session_device_is_pipe_endpoint_id(
        info: pipe_runtime::PipeSessionInfo,
    ) -> pipe_runtime::EndpointId {
        info.device
    }

    // `EndpointId` — named by `Node::id` (return) and `Node::disconnect_peer` (arg).
    let _: fn(&session::Node) -> session::EndpointId = session::Node::id;
    let _: fn(&session::Node, session::EndpointId) = session::Node::disconnect_peer;
    // `EndpointAddr` — named by `Node::connect_to` (arg).
    let _: fn(&session::Node, session::EndpointAddr) = session::Node::connect_to;
    // `Endpoint` — named by `Node::endpoint` (return).
    let _: fn(&session::Node) -> session::Endpoint = session::Node::endpoint;
    // The `blob` duplicate — named by `BlobAclView::is_active` (arg).
    let _: fn(&blob::BlobAclView, blob::EndpointId) -> bool = blob::BlobAclView::is_active;
    // Name each inner item so it is type-checked without a dead-code warning under
    // `-D warnings`; none is executed.
    let _ = spawn_secret_lock;
    let _ = session_id_is_blob_id;
    let _ = session_id_is_pipe_id;
    let _ = pipe_session_device_is_pipe_endpoint_id;
}

/// Exercise the exact inherent methods the reference CLI drives through these
/// re-exports (issue #87 §4.2) — `SecretKey::from_bytes`/`.public()`,
/// `EndpointId::from_bytes`/`from_str`/`Display`, `EndpointAddr::new`/
/// `.with_ip_addr`/`.id` — proving re-exporting the *types* is sufficient (no extra
/// iroh symbol is needed) and that they behave, all offline and deterministic.
#[test]
fn iroh_transport_reexports_construct_offline() {
    use std::str::FromStr;

    // `SecretKey::from_bytes(&seed).public()` is how the CLI turns a device seed into
    // a transport identity (join.rs / file.rs / message.rs), now through the facade.
    let id: session::EndpointId = session::SecretKey::from_bytes(&[7u8; 32]).public();
    let same: session::EndpointId = session::SecretKey::from_bytes(&[7u8; 32]).public();
    let other: session::EndpointId = session::SecretKey::from_bytes(&[8u8; 32]).public();
    assert_eq!(id, same, "same seed must derive the same EndpointId");
    assert_ne!(
        id, other,
        "different seeds must derive different EndpointIds"
    );

    // `EndpointId` Display + FromStr round-trip (message.rs parses `EndpointId::from_str`).
    let printed = id.to_string();
    let parsed = session::EndpointId::from_str(&printed)
        .expect("EndpointId round-trips via its string form");
    assert_eq!(parsed, id);

    // `EndpointId::from_bytes` on its own 32 bytes (file.rs `resolve_providers` path).
    let from_bytes = session::EndpointId::from_bytes(id.as_bytes())
        .expect("a valid public key's own bytes parse back");
    assert_eq!(from_bytes, id);

    // `EndpointAddr::new(id).with_ip_addr(sock)` + `.id` field — the dial hint the
    // CLI builds (join.rs / message.rs / pipe.rs), routed through the facade.
    let sock: SocketAddr = "127.0.0.1:45001".parse().expect("valid loopback socket");
    let addr = session::EndpointAddr::new(id).with_ip_addr(sock);
    assert_eq!(addr.id, id, "EndpointAddr carries the id it was built from");
}

#[test]
fn experimental_sync_paths_resolve() {
    assert!(!name_of::<sync::SyncEngine>().is_empty());
    assert!(!name_of::<sync::SyncConfig>().is_empty());
    assert!(!name_of::<sync::SyncMessage>().is_empty());
    assert!(!name_of::<sync::SyncError>().is_empty());
    assert!(!name_of::<sync::MessageError>().is_empty());
    assert!(!name_of::<sync::SyncCounters>().is_empty());
    assert!(!name_of::<sync::SyncDigest>().is_empty());
    assert!(!name_of::<sync::Completeness>().is_empty());
    assert!(!name_of::<sync::Severity>().is_empty());
    assert!(!name_of::<sync::TrustDecision>().is_empty());
    assert!(!name_of::<sync::Outgoing>().is_empty());
    assert!(!name_of::<sync::Window>().is_empty());
    assert!(!name_of::<sync::PeerId>().is_empty());
    // `WireBytes` is a type alias (`Vec<u8>`); it still resolves through the façade.
    assert!(!name_of::<sync::WireBytes>().is_empty());

    // `SyncConfig` is a plain value type — constructing its default is offline.
    assert!(!format!("{:?}", sync::SyncConfig::default()).is_empty());

    // The `SyncTransport` trait re-export is object-safe; naming `dyn` resolves
    // its path without needing a re-exported concrete impl.
    assert!(!name_of::<dyn sync::SyncTransport>().is_empty());
}

#[test]
fn experimental_store_paths_resolve() {
    assert!(!name_of::<store::EventStore>().is_empty());
    assert!(!name_of::<store::StoredEvent>().is_empty());
    assert!(!name_of::<store::StoreError>().is_empty());
    assert!(!name_of::<store::InsertOutcome>().is_empty());
    assert!(!name_of::<store::InsertStats>().is_empty());
    assert!(!name_of::<store::ParkedRow>().is_empty());
    assert!(!name_of::<store::SyncStateRow>().is_empty());
    assert!(!name_of::<store::TrustRow>().is_empty());
}

#[test]
fn experimental_blob_paths_resolve() {
    assert!(!name_of::<blob::BlobStore>().is_empty());
    assert!(!name_of::<blob::BlobAclView>().is_empty());
    assert!(!name_of::<blob::BlobError>().is_empty());
    assert!(!name_of::<blob::BlobImport>().is_empty());
    assert!(!name_of::<blob::FetchOutcome>().is_empty());

    // `EndpointId` is duplicated into `blob` (issue #87) so a blob-only consumer
    // need not reach into `session` — it is the type `BlobAclView::is_active` names.
    assert!(!name_of::<blob::EndpointId>().is_empty());
}

#[test]
fn experimental_pipe_runtime_paths_resolve() {
    assert!(!name_of::<pipe_runtime::PipeForwarder>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeRegistry>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeOutcome>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeError>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeDenyCause>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeSessionInfo>().is_empty());
    assert!(!name_of::<pipe_runtime::TracingPipeAudit>().is_empty());

    // Trait re-export, proven through `TracingPipeAudit`'s impl.
    assert!(!pipe_audit_name::<pipe_runtime::TracingPipeAudit>().is_empty());

    // Const re-export.
    assert!(!pipe_runtime::PIPE_ALPN.is_empty());

    // `EndpointId` is duplicated into `pipe_runtime` (issue #87) so a pipe-only
    // consumer need not reach into `session` — it is the type `PipeSessionInfo.device`
    // and the `PipeAuditSink` callbacks name.
    assert!(!name_of::<pipe_runtime::EndpointId>().is_empty());
}

#[test]
fn pipe_runtime_helpers_are_pure() {
    // `new_pipe_id` draws a fresh 16-byte id per call (CSPRNG, no network IO).
    let a = pipe_runtime::new_pipe_id();
    let b = pipe_runtime::new_pipe_id();
    assert_ne!(a, b, "each pipe id draw must be fresh");

    // `is_loopback_target` is a pure predicate over a socket address, routed
    // through the façade re-export path (a drift there fails this to compile).
    let loopback: SocketAddr = "127.0.0.1:8080".parse().expect("valid loopback addr");
    let public: SocketAddr = "93.184.216.34:80".parse().expect("valid public addr");
    assert!(
        pipe_runtime::is_loopback_target(&loopback),
        "127.0.0.1 must classify as loopback"
    );
    assert!(
        !pipe_runtime::is_loopback_target(&public),
        "a public address must not classify as loopback"
    );
}
