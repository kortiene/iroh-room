//! **Experimental (unstable API).** The bounded recent-sync engine: a
//! deterministic, sans-IO protocol state machine that moves opaque signed
//! [`WireEvent`](crate::events::WireEvent)s between peers over a
//! [`SyncTransport`].
//!
//! [`sync::sim`](iroh_rooms_core::sync::sim) (the in-memory deterministic test
//! harness) is intentionally **not** re-exported — it is a test-only tool, not
//! part of the SDK surface.

pub use iroh_rooms_core::sync::{
    Completeness, MessageError, Outgoing, PeerId, Severity, SyncConfig, SyncCounters, SyncDigest,
    SyncEngine, SyncError, SyncMessage, SyncTransport, TrustDecision, Window, WireBytes,
};
