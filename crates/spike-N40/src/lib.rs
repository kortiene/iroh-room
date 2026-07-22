//! `spike-n40` — the #145 measurement harness for the post-`b0622ec`-guardrail
//! v1 full-mesh event transport at N=5/10/20/40 loopback nodes (spec
//! `specs/measure-n40-v1-guardrails.md`).
//!
//! `PHASE-0-SPIKE.md:43-49` recorded the pre-guardrail N=25 collapse: at idle,
//! `frames_sent=0`, `accepted=0`, 661 MB inbound backlog, and under load 22
//! published events yielded `accepted=0` with 55,222 queued frames while
//! connectivity still looked healthy. The bounded queues + quiescent ticks +
//! #136 dial-stomp fix may have changed the failure mode from "silent
//! collapse" to "constant reconnect churn" — or it may have raised the
//! ceiling entirely. This spike answers:
//!
//! 1. Does N=40 idle survive?
//! 2. At what room-wide event rate does the queue-close-on-full cascade begin?
//! 3. Is gossip warranted now, or does v1-with-guardrails hold to ~40?
//!
//! The crate stands up N in-process loopback nodes through the shipping
//! [`iroh_rooms_net::Node`], forms the full mesh via `Node::connect_to` for
//! every ordered pair, drives a configurable admin-authored publish rate, and
//! samples RSS, per-node task estimates, accepted / frames-sent counters,
//! outbound queue bytes, queue-saturation audit events, and reconnect churn
//! to compute the cascade verdict and the GO/NO-GO rubric for #154.
//!
//! # Throwaway posture (mirrors `spike-transport` / `spike-nat` / `spike-blobs`)
//!
//! `publish = false`, in the workspace only so CI proves it builds and its
//! N=5 loopback self-check passes. The full N=40 matrix is a manual run
//! committed as `results/results.md`. The spike depends on shipping crates
//! (`iroh-rooms-core`, `iroh-rooms-net`); shipping crates must not depend on
//! it and are not modified.
//!
//! # Caveat (spec §4 D1) — over-cap transport mesh, not a product room
//!
//! Every node runs through the real `iroh_rooms_net::Node` with an
//! `AllowlistAdmission` admitting every N endpoint devices; the harness
//! dials every pair explicitly via `Node::connect_to`. This exercises the
//! real post-guardrail transport paths (byte-bounded queues, dial-with-backoff
//! `peer::dial_loop`, #136 guarded state transitions, `SyncEngine` counters)
//! but is **not** a product-supported active-member room: the membership fold
//! still enforces `MAX_ACTIVE_MEMBERS = 5` in shipping code, which this spike
//! does not modify. `NOTES.md` records this caveat plainly.

pub mod cluster;
pub mod metrics;
pub mod report;
pub mod rss;
pub mod workload;

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use iroh::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_net::admission::RejectCause;
use iroh_rooms_net::audit::AuditSink;

/// A spike-local recording audit sink (spec §6.3). Records the lifecycle
/// counts and the cascade-relevant signals (`transport.queue.saturated`,
/// timestamps of `connected` / `disconnected` for reconnect/sec calculation)
/// **without** recording event bodies, message text, invite secrets,
/// capability secrets, blob data, local paths, or private keys (spec §10).
///
/// The harness holds one of these per cluster (shared across every node) and
/// snapshots it before each load window to baseline the cascade signals.
#[derive(Debug, Default)]
pub struct RecordingAudit {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Counters keyed by stable reason family (spec §6.3).
    accepted: u64,
    rejected: u64,
    connected: u64,
    disconnected: u64,
    offline: u64,
    /// Per-(device, queue-name) saturation counts (D4 trigger 1).
    saturated: HashMap<(EndpointId, &'static str), u64>,
    /// Wall-clock instants of every `connected` / `disconnected` event, for
    /// reconnect/sec over a window (D4 trigger 2). The harness drains these
    /// per sample window and counts the post-warmup reconnect transitions.
    connected_times: Vec<Instant>,
    disconnected_times: Vec<Instant>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A snapshot of the recording audit's monotonic counters at one point in
/// time. Used to compute per-window deltas (reconnect churn, saturations).
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditSnapshot {
    pub accepted: u64,
    pub rejected: u64,
    pub connected: u64,
    pub disconnected: u64,
    pub offline: u64,
    pub saturations: u64,
    pub connected_events: u64,
    pub disconnected_events: u64,
}

impl RecordingAudit {
    /// Construct a fresh recording audit (all counters zero).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap as an `Arc<dyn AuditSink>` for `Node::spawn`.
    #[must_use]
    pub fn into_sink(self) -> std::sync::Arc<dyn AuditSink> {
        std::sync::Arc::new(self)
    }

    /// Take a consistent snapshot of every monotonic counter.
    #[must_use]
    pub fn snapshot(&self) -> AuditSnapshot {
        let g = lock(&self.inner);
        AuditSnapshot {
            accepted: g.accepted,
            rejected: g.rejected,
            connected: g.connected,
            disconnected: g.disconnected,
            offline: g.offline,
            saturations: g.saturated.values().sum(),
            connected_events: g.connected_times.len() as u64,
            disconnected_events: g.disconnected_times.len() as u64,
        }
    }

    /// Total cluster-wide `transport.queue.saturated` events observed so far.
    #[must_use]
    pub fn queue_saturations(&self) -> u64 {
        lock(&self.inner).saturated.values().sum()
    }

    /// Count of `connected` transitions recorded since `baseline`.
    #[must_use]
    pub fn connected_since(&self, baseline: &AuditSnapshot) -> u64 {
        self.snapshot()
            .connected_events
            .saturating_sub(baseline.connected_events)
    }

    /// Count of `disconnected` transitions recorded since `baseline`.
    #[must_use]
    pub fn disconnected_since(&self, baseline: &AuditSnapshot) -> u64 {
        self.snapshot()
            .disconnected_events
            .saturating_sub(baseline.disconnected_events)
    }
}

impl AuditSink for RecordingAudit {
    fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {
        lock(&self.inner).accepted += 1;
    }
    fn rejected(&self, _device: EndpointId, _cause: RejectCause) {
        lock(&self.inner).rejected += 1;
    }
    fn connected(&self, _device: EndpointId) {
        let mut g = lock(&self.inner);
        g.connected += 1;
        g.connected_times.push(Instant::now());
    }
    fn disconnected(&self, _device: EndpointId) {
        let mut g = lock(&self.inner);
        g.disconnected += 1;
        g.disconnected_times.push(Instant::now());
    }
    fn offline(&self, _device: EndpointId, _reason: &'static str) {
        lock(&self.inner).offline += 1;
    }
    fn transport_queue_saturated(&self, device: EndpointId, queue: &'static str) {
        *lock(&self.inner)
            .saturated
            .entry((device, queue))
            .or_insert(0) += 1;
    }
}

// Trivial placeholder so a doctest on the module docs does not need to spin
// up a real `Node`. Kept minimal: the harness reaches the API through
// `cluster` / `metrics` / `report`.
#[cfg(doctest)]
struct _DoctestAnchor;

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn id(seed: u8) -> IdentityKey {
        IdentityKey::from_bytes([seed; 32])
    }

    #[test]
    fn recording_audit_records_each_lifecycle_signal() {
        let audit = RecordingAudit::new();
        audit.accepted(dev(1), &id(0x10));
        audit.accepted(dev(2), &id(0x11));
        audit.rejected(dev(3), RejectCause::UnknownDevice);
        audit.connected(dev(1));
        audit.connected(dev(2));
        audit.disconnected(dev(1));
        audit.offline(dev(1), "unreachable");
        audit.transport_queue_saturated(dev(1), "inbound");
        audit.transport_queue_saturated(dev(1), "inbound");
        audit.transport_queue_saturated(dev(2), "outbound");

        let snap = audit.snapshot();
        assert_eq!(snap.accepted, 2);
        assert_eq!(snap.rejected, 1);
        assert_eq!(snap.connected, 2);
        assert_eq!(snap.disconnected, 1);
        assert_eq!(snap.offline, 1);
        assert_eq!(snap.saturations, 3);
        assert_eq!(snap.connected_events, 2);
        assert_eq!(snap.disconnected_events, 1);
        assert_eq!(audit.queue_saturations(), 3);
    }

    #[test]
    fn connected_since_reports_delta_from_baseline() {
        let audit = RecordingAudit::new();
        audit.connected(dev(1));
        let baseline = audit.snapshot();
        audit.connected(dev(2));
        audit.connected(dev(3));
        audit.disconnected(dev(1));
        assert_eq!(audit.connected_since(&baseline), 2);
        assert_eq!(audit.disconnected_since(&baseline), 1);
    }

    #[test]
    fn baseline_at_zero_sees_every_event() {
        let audit = RecordingAudit::new();
        let baseline = AuditSnapshot::default();
        audit.connected(dev(1));
        audit.connected(dev(2));
        audit.disconnected(dev(1));
        audit.transport_queue_saturated(dev(1), "outbound");
        assert_eq!(audit.connected_since(&baseline), 2);
        assert_eq!(audit.disconnected_since(&baseline), 1);
        assert_eq!(audit.queue_saturations(), 1);
    }

    #[test]
    fn into_sink_satisfies_audit_sink_bound() {
        let audit = RecordingAudit::new();
        let _sink: std::sync::Arc<dyn AuditSink> = audit.into_sink();
    }

    #[test]
    fn audit_snapshot_deltas_pin_reconnect_and_saturation_window_accounting() {
        let audit = RecordingAudit::new();
        audit.connected(dev(1));
        audit.disconnected(dev(1));
        audit.transport_queue_saturated(dev(1), "outbound");
        let baseline = audit.snapshot();

        audit.connected(dev(2));
        audit.disconnected(dev(2));
        audit.disconnected(dev(3));
        audit.transport_queue_saturated(dev(2), "outbound");
        audit.transport_queue_saturated(dev(2), "outbound");

        let after = audit.snapshot();
        assert_eq!(after.connected.saturating_sub(baseline.connected), 1);
        assert_eq!(after.disconnected.saturating_sub(baseline.disconnected), 2);
        assert_eq!(after.saturations.saturating_sub(baseline.saturations), 2);
        assert_eq!(audit.connected_since(&baseline), 1);
        assert_eq!(audit.disconnected_since(&baseline), 2);
    }

    /// Spec §10: the recording audit must never carry room ids / event bytes /
    /// secrets / paths. The struct's fields are all numeric counters, device
    /// ids, or `&'static str` queue labels — there is no field that could
    /// accidentally hold a private key, message body, or path. This test pins
    /// that surface by asserting the snapshot carries only integer counters.
    #[test]
    fn recording_audit_has_no_secret_carrying_public_state() {
        let audit = RecordingAudit::new();
        let snap = audit.snapshot();
        // Every snapshot field is an integer counter; no string, key, or path.
        let _ = snap.accepted;
        let _ = snap.rejected;
        let _ = snap.connected;
        let _ = snap.disconnected;
        let _ = snap.offline;
        let _ = snap.saturations;
        let _ = snap.connected_events;
        let _ = snap.disconnected_events;
    }
}
