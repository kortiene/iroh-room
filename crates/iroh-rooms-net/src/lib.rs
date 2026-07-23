//! `iroh-rooms-net` (IR-0005) — the real full-mesh direct-QUIC event-transport
//! adapter for the Room Event Plane.
//!
//! This is the **shipping** carrier the README and the
//! [`SyncTransport`](iroh_rooms_core::sync::SyncTransport) doc both name: the
//! concrete transport behind the landed, sans-IO
//! [`SyncEngine`](iroh_rooms_core::sync::SyncEngine). It proves the
//! `PHASE-0-SPIKE.md` ADR-1 path — full-mesh direct QUIC over the custom ALPN
//! [`EVENT_ALPN`](crate::alpn::EVENT_ALPN) — and the four issue acceptance
//! criteria:
//!
//! 1. **Exchange a signed `WireEvent`** over the ALPN — carried opaquely as a
//!    [`SyncMessage::Events`](iroh_rooms_core::sync::SyncMessage) frame on a
//!    per-peer bidi stream ([`frame`]).
//! 2. **Reject an unknown endpoint before any event byte** — the
//!    [`EventProtocolHandler`](crate::handler::EventProtocolHandler) closes the
//!    connection from the proven `device_id` ([`admission`]) **before**
//!    `accept_bi()`.
//! 3. **Distinguish connected / offline / unauthorized** — the observable
//!    [`PeerConnState`](crate::state::PeerConnState) model + change stream.
//! 4. **Basic reconnect** — the dial-with-backoff loop ([`peer`]) redials a
//!    dropped link.
//!
//! ## Layering
//!
//! The transport carries **opaque** frames; it never validates events or
//! re-implements ordering/membership — the engine owns all of that (spec N5). The
//! authorizer is a `device_id → identity → Active?` allowlist with the same shape
//! the membership fold produces, so the production re-point to
//! [`MembershipSnapshot`](iroh_rooms_core::membership::MembershipSnapshot) is a
//! swap of two lookups, not a reshape (spec D6).
//!
//! ## Entry points
//!
//! - [`NetTransport`] — the carrier; implements
//!   [`SyncTransport`](iroh_rooms_core::sync::SyncTransport) (G6).
//! - [`Node`] — a thin runtime pairing a transport with a `SyncEngine` and
//!   pumping them (used by the `net-smoke` binary and the loopback tests).
//!
//! **This is an implementation crate.** The supported SDK entry point is the
//! `iroh-rooms` façade (issue #36 / IR-0301), which re-exports this crate's
//! online-runtime surface behind its `experimental` feature. Depend on this
//! crate directly only for advanced use; every item here may change without
//! the façade's `0.x` stability policy.

pub mod admission;
pub mod alpn;
pub mod audit;
pub mod blob;
pub mod demo;
pub mod diag;
pub mod frame;
#[cfg(feature = "gossip_overlay")]
pub mod gossip;
pub mod handler;
pub mod manager;
pub mod node;
pub mod pipe;
pub mod state;
pub mod transport;

#[cfg(not(feature = "gossip_overlay"))]
const _: () = assert!(iroh_rooms_core::membership::MAX_ACTIVE_MEMBERS == 5);
#[cfg(feature = "gossip_overlay")]
const _: () = assert!(iroh_rooms_core::membership::MAX_ACTIVE_MEMBERS == 40);

mod peer;
mod queue;

pub use admission::{
    Admission, AdmissionDecision, AdmissionView, AllowlistAdmission, JoinBootstrapAdmission,
    RejectCause, SnapshotAdmission,
};
pub use alpn::{EVENT_ALPN, GOSSIP_ALPN};
pub use audit::{AuditSink, BlobDenyCause, TracingAudit};
pub use blob::{BlobAclView, BlobError, BlobImport, BlobStore, FetchOutcome};
pub use diag::{classify_remote_info, PathType};
pub use frame::{FrameError, MAX_FRAME_BYTES};
pub use handler::EventProtocolHandler;
pub use manager::{PeerManager, GOSSIP_BOOTSTRAP_SEEDS};
pub use node::{BlobServeConfig, BootstrapProof, Node, DEFAULT_TICK};
pub use pipe::{
    new_pipe_id, PipeAuditSink, PipeDenyCause, PipeError, PipeForwarder, PipeOutcome, PipeRegistry,
    PipeSessionInfo, TracingPipeAudit, PIPE_ALPN, PIPE_MAX_CONCURRENT_FORWARDS,
};
pub use state::{ConnEvent, OfflineReason, PeerConnState, PeerEntry, PeerTable};
pub use transport::{
    Inbound, InboundReceiver, NetConfig, NetMode, NetTransport, OutboundQueue, Shared,
    MAX_CONCURRENT_BIDI_STREAMS, RELAY_ONLY_TEST_BUILD,
};
