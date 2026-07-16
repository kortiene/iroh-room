//! **Experimental (unstable API).** The online runtime: a full-mesh direct-QUIC
//! transport, connection admission, and peer connection state.
//!
//! [`Node`] is the high-level entry point — a thin runtime pairing a
//! [`NetTransport`] with a sync engine and pumping them (`spawn` /
//! `spawn_room` / `publish` / `room_tail` / `snapshot` / `fetch_file` /
//! `blob_import` / `blob_import_bytes` / `pipe_expose` / `pipe_connect` /
//! `conn_events` / `room_events` / `live_pipe_sessions_for` /
//! `pipe_session_info` / `shutdown`, …). See its own docs in
//! `iroh-rooms-net` for the full method set.
//!
//! `room_events()` (issue #83 / IR-0307) returns a
//! `tokio::sync::broadcast::Receiver<StoredEvent>` — the façade does not
//! re-export `tokio`, so consumers name the type via their own `tokio::sync::broadcast`
//! dependency.
//!
//! `blob_import_bytes()` (issue #84 / IR-0308) takes a `bytes::Bytes` — the façade
//! does not re-export `bytes` either, so consumers name it via their own
//! `bytes::Bytes` dependency (the fetched bytes already come back as `Bytes` from
//! `Node::fetch_file`).
//!
//! The transport identities `EndpointAddr` / `EndpointId` / `SecretKey` (and
//! `Endpoint`) *are* re-exported here (issue #87): a consumer needs no direct
//! `iroh` dependency to drive the online API. These are re-exported *verbatim*
//! from the pinned `iroh` release `iroh-rooms-net` depends on — not wrapped —
//! so they are exactly the same types `iroh-rooms-net`'s public signatures
//! name; the re-exports track that pin and may change when it moves (the
//! experimental tier's usual "may change on any release" promise).

pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
pub use iroh_rooms_net::{
    Admission, AdmissionDecision, AdmissionView, AllowlistAdmission, AuditSink, BlobDenyCause,
    BlobServeConfig, BootstrapProof, ConnEvent, EventProtocolHandler, Inbound,
    JoinBootstrapAdmission, NetConfig, NetMode, NetTransport, Node, OfflineReason, PeerConnState,
    PeerEntry, PeerManager, PeerTable, RejectCause, Shared, SnapshotAdmission, TracingAudit,
    DEFAULT_TICK, EVENT_ALPN, RELAY_ONLY_TEST_BUILD,
};
