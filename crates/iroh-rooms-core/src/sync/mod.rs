//! The bounded recent-sync engine (IR-0007): a deterministic, **sans-IO** protocol
//! state machine that moves opaque signed [`WireEvent`](crate::event::WireEvent)s
//! between peers and reconciles them over the landed
//! [`store`](crate::store)/[`membership`](crate::membership) layers — proving the
//! ADR-2 bounded recent-sync path for MVP-sized rooms without adopting full
//! decentralized reconciliation (`PHASE-0-SPIKE.md` ADR-2 / Membership & Ordering
//! §0/§4; spec `bounded-recent-sync-prototype.md`).
//!
//! The five mechanisms this module delivers (spec §1):
//!
//! 1. **Pull missing events by id** — the `WantEvents`/`Events` backfill loop
//!    driven by [`Ingest::Buffered`](crate::membership::Ingest) /
//!    [`EventStore::missing_parents`](crate::store::EventStore::missing_parents),
//!    bounded by the §4 anti-amplification gate.
//! 2. **Pull bounded recent chat** — `WantRecentChat`/`Events`, count-bounded
//!    (trustworthy) and optionally time-bounded (advisory).
//! 3. **Always reconcile the membership sub-DAG + full admin chain** —
//!    `WantMembership`/`Events`, **never** windowed (the §0 hard invariant).
//! 4. **Exchange admin tips + detect incompleteness** — `AdminTip` plus the
//!    [`Completeness`] detector. A known-higher tip we have not backfilled ⇒
//!    [`AdminViewSuspect`](Completeness::AdminViewSuspect), which **fails
//!    affected subjects closed** until catch-up: an unapplied admin event could
//!    be a removal. A fold-level admin **divergence** — two causally concurrent
//!    admin membership writes with a conflicting authorization effect on one
//!    subject (issue #213 / ADR-0006) ⇒
//!    [`AdminForkDetected`](Completeness::AdminForkDetected) + a CRITICAL
//!    `equivocation` [`TrustDecision`], which is **advisory and denies nobody**
//!    (ADR-0005 / #211): a fork is only declared over branches we hold, so
//!    nothing is missing and the fold has already merged them at least
//!    privilege.
//! 5. **Assert set equality after sync** — the [`SyncDigest`] oracle (D8) and the
//!    [`sim::SimNet`] convergence harness.
//!
//! Everything below the wire is frozen and conformance-tested; the engine
//! orchestrates it and re-decides nothing (spec D4). See [`engine::SyncEngine`]
//! for the entry points and [`sim::SimNet`] for the deterministic Gate-D harness.

pub mod config;
pub mod engine;
#[cfg(test)]
mod engine_tests;
mod keys;
pub mod message;
pub mod sim;
pub mod transport;

pub use config::SyncConfig;
pub use engine::{
    Completeness, Severity, SyncCounters, SyncDigest, SyncEngine, SyncError, TrustDecision,
};
pub use message::{
    MessageError, Outgoing, PeerId, SyncMessage, Window, WireBytes, MAX_FRAME_BYTES,
};
pub use transport::SyncTransport;
