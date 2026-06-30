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

pub mod admission;
pub mod alpn;
pub mod audit;
pub mod demo;
pub mod frame;
pub mod handler;
pub mod node;
pub mod state;
pub mod transport;

mod peer;

pub use admission::{Admission, AdmissionDecision, AllowlistAdmission, RejectCause};
pub use alpn::EVENT_ALPN;
pub use audit::{AuditSink, TracingAudit};
pub use frame::{FrameError, MAX_FRAME_BYTES};
pub use handler::EventProtocolHandler;
pub use node::{Node, DEFAULT_TICK};
pub use state::{ConnEvent, PeerConnState, PeerEntry, PeerTable};
pub use transport::{Inbound, NetConfig, NetMode, NetTransport, Shared};
