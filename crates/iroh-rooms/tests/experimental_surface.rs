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
}

#[test]
fn experimental_pipe_runtime_paths_resolve() {
    assert!(!name_of::<pipe_runtime::PipeForwarder>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeRegistry>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeOutcome>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeError>().is_empty());
    assert!(!name_of::<pipe_runtime::PipeDenyCause>().is_empty());
    assert!(!name_of::<pipe_runtime::TracingPipeAudit>().is_empty());

    // Trait re-export, proven through `TracingPipeAudit`'s impl.
    assert!(!pipe_audit_name::<pipe_runtime::TracingPipeAudit>().is_empty());

    // Const re-export.
    assert!(!pipe_runtime::PIPE_ALPN.is_empty());
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
