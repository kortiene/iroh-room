//! `spike-transport` — the IR-0006 Day-4 gossip-vs-full-mesh transport
//! comparison harness (`PHASE-0-SPIKE.md` ADR-1 / Spike Plan Day 4).
//!
//! `PHASE-0-SPIKE.md` ADR-1 *recommends* full-mesh direct QUIC over
//! `iroh-gossip` for the Room Event Plane on adversarial design review, not
//! measured code (Residual Risk 12). This crate builds **both** backends
//! minimally behind one trait ([`TransportBackend`]), drives the **same**
//! signed `WireEvent` payloads ([`workload`]) through each over N=2..5
//! in-process loopback nodes, and measures the five ADR-1 comparison
//! dimensions: propagation latency, reconnect behavior, late-join history gap,
//! auth/admission model, and implementation complexity. See `NOTES.md` for the
//! iroh-gossip API reconciliation, the measured comparison table, and the
//! decision memo.
//!
//! - [`mesh`] — the minimal full-mesh backend (spec §7.2).
//! - [`gossip`] — the minimal `iroh-gossip` backend (spec §7.3).
//! - [`workload`] — the shared deterministic signed-`WireEvent` workload (§7.5).
//! - [`report`] — the [`report::ComparisonResult`] record (§7.6/§7.9) plus its
//!   JSON and Markdown emitters.
//!
//! Like `spike-nat` / `spike-blobs`, this crate is a **throwaway spike**:
//! `publish = false`, isolated from the shipping crates' dependency tree, kept
//! in the workspace so CI proves it builds and its loopback self-check passes.
//! Unlike `spike-nat`, every claim this spike measures runs on deterministic
//! loopback — CI proves the *measured* claims too (spec §7.8).

pub mod gossip;
pub mod mesh;
pub mod report;
pub mod workload;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use iroh_rooms_core::event::EventId;

/// Which backend a [`TransportBackend`] implementation is — the results-table
/// label ("mesh" | "gossip").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// The minimal full-mesh direct-QUIC backend.
    Mesh,
    /// The minimal `iroh-gossip` backend.
    Gossip,
}

impl BackendKind {
    /// The lowercase label used in the results table and JSON.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Gossip => "gossip",
        }
    }
}

/// Verbatim signed-event bytes (`== WireEvent::to_bytes()`).
pub type WireBytes = Vec<u8>;

/// A backend-observed failure or liveness signal since the last
/// [`drain_events`](TransportBackend::drain_events) call — feeds the
/// "observed failure modes" column of the comparison (spec §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendEvent {
    /// gossip: `Event::Lagged` — the local consumer fell behind and messages
    /// were silently dropped ("resync needed" signal, no per-peer link to pull
    /// over).
    Lagged,
    /// mesh: an inbound connection from a non-member `EndpointId` was refused
    /// **before** `accept_bi()` — the admission-before-bytes evidence (AC3).
    AdmissionRejected,
    /// gossip: an interloper that knows only the 32-byte `TopicId` was
    /// admitted with **no** authentication check — the "open topic" evidence
    /// (AC3).
    InterloperAdmitted,
    /// mesh: a previously-established link to a peer dropped.
    LinkDropped,
}

/// A minimal event-carrier backend for the N≤5 comparison (spec §7.1).
/// Spike-local and async; **not** the shipping, sans-IO
/// `iroh_rooms_core::sync::SyncTransport` — this trait stands up real
/// endpoints and owns their I/O.
#[async_trait]
pub trait TransportBackend: Send + Sync {
    /// Human label for the results table ("mesh" | "gossip").
    fn kind(&self) -> BackendKind;

    /// Broadcast one verbatim signed `WireEvent` to the room.
    ///
    /// # Errors
    /// Returns an error if the backend cannot send (e.g. a build/serialize
    /// failure on the gossip side); a peer merely being offline is not an
    /// error here — it surfaces as a gap in that peer's `received_ids()`.
    async fn publish(&self, wire: WireBytes) -> anyhow::Result<()>;

    /// Every `event_id` this node currently holds: inserted when it authors an
    /// event via [`publish`](Self::publish) (so the publisher's own set
    /// converges too) or when it receives one from a peer, deduped by the
    /// recomputed id. This node's set for the set-equality oracle.
    fn received_ids(&self) -> BTreeSet<EventId>;

    /// Backend-observed failure signals since the last drain (gossip
    /// `Lagged`, admission rejections, interloper acceptances, link drops).
    fn drain_events(&self) -> Vec<BackendEvent>;
}

/// Stands up N nodes of one backend and drives the comparison scenarios
/// generically over [`TransportBackend`] (spec §7.1).
pub struct Cluster<B: TransportBackend> {
    nodes: Vec<Arc<B>>,
}

impl<B: TransportBackend> Cluster<B> {
    /// Wrap an already-bootstrapped set of nodes (bootstrapping — dial-all for
    /// mesh, single-topic subscribe for gossip — is backend-specific and lives
    /// in [`mesh`]/[`gossip`]).
    #[must_use]
    pub fn new(nodes: Vec<Arc<B>>) -> Self {
        Self { nodes }
    }

    /// Number of nodes in the cluster.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the cluster has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node at index `i`.
    #[must_use]
    pub fn node(&self, i: usize) -> &Arc<B> {
        &self.nodes[i]
    }

    /// Every node in the cluster.
    #[must_use]
    pub fn nodes(&self) -> &[Arc<B>] {
        &self.nodes
    }

    /// Broadcast `wire` from node `i`.
    ///
    /// # Errors
    /// Propagates the backend's publish error.
    pub async fn publish_from(&self, i: usize, wire: WireBytes) -> anyhow::Result<()> {
        self.nodes[i].publish(wire).await
    }

    /// Poll every node's `received_ids()` until each is a superset of
    /// `expected`, or `deadline` elapses — whichever comes first. Returns the
    /// final per-node sets (index-aligned with the cluster), so the caller can
    /// diff against `expected` for the exact set delta the failure-mode ledger
    /// records (spec §7.6).
    pub async fn await_convergence(
        &self,
        expected: &BTreeSet<EventId>,
        deadline: Duration,
    ) -> Vec<BTreeSet<EventId>> {
        const POLL_INTERVAL: Duration = Duration::from_millis(15);
        let start = Instant::now();
        loop {
            let sets: Vec<BTreeSet<EventId>> =
                self.nodes.iter().map(|n| n.received_ids()).collect();
            let converged = sets.iter().all(|s| expected.is_subset(s));
            if converged || start.elapsed() >= deadline {
                return sets;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendKind;

    #[test]
    fn backend_kind_labels() {
        assert_eq!(BackendKind::Mesh.label(), "mesh");
        assert_eq!(BackendKind::Gossip.label(), "gossip");
    }

    #[test]
    fn backend_kind_serde_matches_label_lowercase() {
        // The JSON artifact (`ComparisonResult`) relies on the serde encoding
        // being the same lowercase token as `label()`; keep the two in lockstep.
        for kind in [BackendKind::Mesh, BackendKind::Gossip] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.label()));
            let back: BackendKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kind);
        }
    }
}
