//! The abstract transport boundary the sync engine is written against
//! (spec `bounded-recent-sync-prototype.md` D2).
//!
//! The engine itself is **sans-IO**: it returns [`Outgoing`] frames and never
//! touches a socket. This trait is the seam where a concrete carrier — the
//! in-memory [`SimNet`](super::sim::SimNet) on the deterministic conformance path,
//! or the real full-mesh iroh QUIC adapter (`crates/iroh-rooms-net`, D3/D9) off
//! it — actually delivers them. Keeping `iroh` behind this trait is what lets a
//! flaky network never make Gate D non-deterministic (spec §3.3 / R6).

use super::message::{Outgoing, PeerId};

/// A best-effort, per-peer-ordered frame carrier.
///
/// The real adapter implements this over `iroh::protocol::Router` + ALPN
/// `/iroh-rooms/event/1`, rejecting unknown `remote_endpoint_id`s at `accept()`
/// (ADR-1 native admission, spec D9). The deterministic harness implements it
/// in-memory. The engine drives a transport by handing it the [`Outgoing`]s its
/// entry points return.
pub trait SyncTransport {
    /// The currently-connected, authenticated peers (their `device_id`s).
    fn peers(&self) -> Vec<PeerId>;

    /// Enqueue an outbound frame to a peer. Best-effort; delivery is ordered per
    /// peer link but may be dropped if the link is down (the engine re-pulls on
    /// reconnect, spec §6.3).
    fn send(&mut self, out: Outgoing);
}
