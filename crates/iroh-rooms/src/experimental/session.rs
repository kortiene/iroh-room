//! **Experimental (unstable API).** The online runtime: a full-mesh direct-QUIC
//! transport, connection admission, and peer connection state.
//!
//! [`Node`] is the high-level entry point — a thin runtime pairing a
//! [`NetTransport`] with a sync engine and pumping them (`spawn` /
//! `spawn_room` / `publish` / `room_tail` / `snapshot` / `fetch_file` /
//! `pipe_expose` / `pipe_connect` / `conn_events` / `shutdown`, …). See its
//! own docs in `iroh-rooms-net` for the full method set.

pub use iroh_rooms_net::{
    Admission, AdmissionDecision, AdmissionView, AllowlistAdmission, AuditSink, BlobDenyCause,
    BlobServeConfig, ConnEvent, EventProtocolHandler, Inbound, JoinBootstrapAdmission, NetConfig,
    NetMode, NetTransport, Node, OfflineReason, PeerConnState, PeerEntry, PeerManager, PeerTable,
    RejectCause, Shared, SnapshotAdmission, TracingAudit, DEFAULT_TICK, EVENT_ALPN,
};
