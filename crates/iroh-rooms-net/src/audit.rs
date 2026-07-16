//! The local admission/connection audit sink (PRD §13.2 pipe-style audit, §16.3
//! failure-mode distinction; spec §4.6 / §6).
//!
//! Every accept / reject / connect / disconnect is recorded locally with a
//! **stable reason string** (`peer.accepted`, `peer.rejected:unknown_device`, …),
//! so the reject vocabulary is exactly the PRD §16.3 offline-vs-unauthorized
//! distinction. It is a trait so the CLI can later persist it (a file/SQLite
//! sink); the default [`TracingAudit`] emits structured `tracing` events.

use iroh::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;

use crate::admission::RejectCause;

/// Why the blob-plane serve gate denied a connect or request (IR-0204 spec §7 —
/// the `blob.serve.rejected:<cause>` vocabulary, mirroring
/// [`PipeDenyCause`](crate::pipe::PipeDenyCause)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobDenyCause {
    /// Gate 1: the connecting device is not a current active member.
    NotActive,
    /// Gate 2: the hash is not referenced by any valid `file.shared` in the room.
    NotReferenced,
    /// A push request was refused — the blobs ALPN never accepts writes.
    PushDenied,
    /// An observe request was refused — no store enumeration over the blobs ALPN.
    ObserveDenied,
}

impl BlobDenyCause {
    /// Stable lowercase reason string for the audit log (spec §7).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotActive => "not_active",
            Self::NotReferenced => "not_referenced",
            Self::PushDenied => "push_denied",
            Self::ObserveDenied => "observe_denied",
        }
    }
}

/// A local audit sink for transport admission and connection lifecycle events.
///
/// Implementations must be cheap and non-blocking — these are called inline on
/// the accept/dial paths.
pub trait AuditSink: Send + Sync + 'static {
    /// A connection was admitted: `device` is bound to Active `identity`.
    fn accepted(&self, device: EndpointId, identity: &IdentityKey);
    /// A connection was rejected before any event byte was read.
    fn rejected(&self, device: EndpointId, cause: RejectCause);
    /// An admitted peer's bidi stream came up (live).
    fn connected(&self, device: EndpointId);
    /// A peer's link dropped (now offline; will be redialed if still a member).
    fn disconnected(&self, device: EndpointId);

    /// A peer moved to offline with a diagnostic reason (`unreachable` /
    /// `transport_error` / `link_dropped`) — the PRD §16.3 / §18.1 refinement of a
    /// generic offline. `reason` is the stable [`OfflineReason`](crate::state::OfflineReason)
    /// label. Default: no-op, so existing sinks need not change.
    fn offline(&self, _device: EndpointId, _reason: &'static str) {}

    /// One or more inbound sync frames from `device` were **rejected** by the
    /// engine before being stored or fanned out (invalid signature, non-member,
    /// bad capability, …) — the AC3 "invalid event rejected" signal
    /// (harden-recent-history-sync §D8). `count` is how many frames the
    /// just-processed inbound message rejected; the stable per-frame
    /// `reject.<code>` detail lives in the engine's bounded `logs()`. Surfaced
    /// here so a CLI/host observes rejections **without** a `tracing` subscriber
    /// (the CLI installs none). Default: no-op, so existing sinks need not change.
    fn event_rejected(&self, _device: EndpointId, _count: u64) {}

    /// A peer was **deauthorized** mid-session — removed from the room, so the
    /// managed dial loop was stopped and its link torn down (spec §4.2 step 3).
    /// Terminal: we will not redial a since-removed peer. Default: no-op.
    fn deauthorized(&self, _device: EndpointId) {}

    /// A validated, **accepted** inbound event from `device` carried an advisory
    /// flag (spec IR-0110 §5.9) — `code` is the stable
    /// [`Flag::code`](iroh_rooms_core::event::Flag::code) label (e.g.
    /// `clock_skew`). Flags never change the verdict, the validated set, ordering,
    /// or any authz/expiry decision — this is purely an observability signal for a
    /// CLI/host that installs no `tracing` subscriber. Default: no-op, so existing
    /// sinks need not change.
    fn event_flagged(&self, _device: EndpointId, _code: &'static str) {}

    /// A connection was admitted **provisionally** for the join bootstrap
    /// (IR-0104, Approach A): a not-yet-Active invitee allowed to pull the
    /// membership sub-DAG and push a single `member.joined`. Default: no-op, so
    /// existing sinks need not change.
    fn bootstrap_admitted(&self, _device: EndpointId) {}
    /// A provisional join-bootstrap connection was upgraded to a full member after
    /// its `member.joined` was accepted by the fold (upgrade-on-learn). Default:
    /// no-op.
    fn bootstrap_upgraded(&self, _device: EndpointId, _identity: &IdentityKey) {}
    /// A non-membership request from a provisional peer was dropped — a provisional
    /// connection is served the membership sub-DAG only (`kind` is the stable
    /// sync-message kind that was refused). Default: no-op.
    fn bootstrap_blocked(&self, _device: EndpointId, _kind: &'static str) {}
    /// A provisional join-bootstrap peer proved invite possession (issue #112): its
    /// `ProveCapability` matched an on-log invite, so it may now pull the membership
    /// closure. Default: no-op.
    fn bootstrap_capability_proven(&self, _device: EndpointId) {}
    /// A provisional join-bootstrap peer's `ProveCapability` did **not** match any
    /// on-log invite (issue #112): it stays gated out of the membership closure.
    /// Default: no-op.
    fn bootstrap_capability_rejected(&self, _device: EndpointId) {}

    /// A gated blob fetch was served: `peer` requested `hash` over the blobs ALPN
    /// and the two-gate ACL allowed it (IR-0204 spec §7 `blob.serve.accepted`).
    /// Default: no-op, so existing sinks need not change.
    fn blob_serve_accepted(&self, _peer: EndpointId, _hash: [u8; 32]) {}

    /// A blob-plane connect or request was denied (spec §7
    /// `blob.serve.rejected:<cause>`). `hash` is present only for a per-hash
    /// denial (Gate 2); absent for a connect denial (Gate 1) or a push/observe
    /// refusal. Default: no-op, so existing sinks need not change.
    fn blob_serve_rejected(
        &self,
        _peer: EndpointId,
        _cause: BlobDenyCause,
        _hash: Option<[u8; 32]>,
    ) {
    }
}

/// The default audit sink: structured `tracing` events with stable reason codes.
#[derive(Debug, Clone, Default)]
pub struct TracingAudit;

impl AuditSink for TracingAudit {
    fn accepted(&self, device: EndpointId, identity: &IdentityKey) {
        tracing::info!(
            reason = "peer.accepted",
            peer = %device,
            %identity,
            "admitted active member"
        );
    }

    fn rejected(&self, device: EndpointId, cause: RejectCause) {
        // `peer.rejected:<cause>` is the stable, greppable audit line. WARN level
        // because a rejected connect is a security-relevant event (PRD §16.3).
        tracing::warn!(
            reason = "peer.rejected",
            cause = cause.code(),
            peer = %device,
            "rejected connection before accepting any event bytes"
        );
    }

    fn connected(&self, device: EndpointId) {
        tracing::info!(reason = "peer.connected", peer = %device, "bidi stream up");
    }

    fn disconnected(&self, device: EndpointId) {
        tracing::info!(reason = "peer.disconnected", peer = %device, "link down");
    }

    fn offline(&self, device: EndpointId, cause: &'static str) {
        // `peer.offline:<reason>` is the stable, greppable §16.3 diagnostic line —
        // it distinguishes an unreachable peer from a transport error / link drop.
        tracing::info!(
            reason = "peer.offline",
            cause,
            peer = %device,
            "peer is offline"
        );
    }

    fn event_rejected(&self, device: EndpointId, count: u64) {
        // `event.rejected` is the stable, greppable AC3 audit line. WARN because a
        // dropped peer frame is a security-relevant event (PRD §16.3).
        tracing::warn!(
            reason = "event.rejected",
            count,
            peer = %device,
            "rejected invalid inbound sync frame(s); not stored, not re-broadcast"
        );
    }

    fn deauthorized(&self, device: EndpointId) {
        // `peer.deauthorized` is the stable, greppable audit line for a mid-session
        // roster removal (WARN: a security-relevant membership change).
        tracing::warn!(
            reason = "peer.deauthorized",
            peer = %device,
            "peer removed from the room mid-session; stopped dialing and tore down the link"
        );
    }

    fn event_flagged(&self, device: EndpointId, code: &'static str) {
        // `event.flagged:<code>` is the stable, greppable advisory line (spec
        // IR-0110 §5.9). INFO: a flag is never a rejection.
        tracing::info!(
            reason = "event.flagged",
            code,
            peer = %device,
            "accepted inbound event carried an advisory flag"
        );
    }

    fn bootstrap_admitted(&self, device: EndpointId) {
        // `join.bootstrap.admitted` is the stable, greppable audit line (IR-0104
        // §8). INFO: a provisional admit is an expected part of the join handshake.
        tracing::info!(
            reason = "join.bootstrap.admitted",
            peer = %device,
            "admitted a provisional join-bootstrap peer (membership sub-DAG only)"
        );
    }

    fn bootstrap_upgraded(&self, device: EndpointId, identity: &IdentityKey) {
        tracing::info!(
            reason = "join.bootstrap.upgraded",
            peer = %device,
            %identity,
            "provisional peer's join was accepted; upgraded to active member"
        );
    }

    fn bootstrap_blocked(&self, device: EndpointId, kind: &'static str) {
        // `join.bootstrap.blocked:<kind>` — a provisional peer asked for more than
        // the membership sub-DAG; we refuse and serve nothing (privacy mitigation).
        tracing::warn!(
            reason = "join.bootstrap.blocked",
            kind,
            peer = %device,
            "dropped a non-membership request from a provisional join-bootstrap peer"
        );
    }

    fn bootstrap_capability_proven(&self, device: EndpointId) {
        // `join.bootstrap.capability_proven` — the dialer proved invite possession
        // (issue #112); it may now pull the membership closure. INFO: an expected
        // step of a genuine join handshake.
        tracing::info!(
            reason = "join.bootstrap.capability_proven",
            peer = %device,
            "provisional peer proved invite possession; serving the membership closure"
        );
    }

    fn bootstrap_capability_rejected(&self, device: EndpointId) {
        // `join.bootstrap.capability_rejected` — a provisional peer's proof matched
        // no on-log invite (issue #112). WARN: a dialer with no invite tried to earn
        // the membership closure; it stays gated out.
        tracing::warn!(
            reason = "join.bootstrap.capability_rejected",
            peer = %device,
            "provisional peer presented an invalid capability proof; staying gated"
        );
    }

    fn blob_serve_accepted(&self, peer: EndpointId, hash: [u8; 32]) {
        // `blob.serve.accepted` is the stable, greppable audit line (spec §7).
        tracing::info!(
            reason = "blob.serve.accepted",
            peer = %peer,
            hash = %hex::encode(hash),
            "served a gated blob fetch"
        );
    }

    fn blob_serve_rejected(&self, peer: EndpointId, cause: BlobDenyCause, hash: Option<[u8; 32]>) {
        // `blob.serve.rejected:<cause>` is the stable, greppable audit line (spec
        // §7). WARN because a denied blob request is a security-relevant event.
        tracing::warn!(
            reason = "blob.serve.rejected",
            cause = cause.code(),
            peer = %peer,
            hash = hash.map(hex::encode),
            "rejected a blob-plane connect or request"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{AuditSink, BlobDenyCause, TracingAudit};
    use crate::admission::RejectCause;
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::keys::IdentityKey;

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn identity(seed: u8) -> IdentityKey {
        IdentityKey::from_bytes([seed; 32])
    }

    // TracingAudit must be non-blocking and non-panicking on all lifecycle events.
    // These tests pin that guarantee for every path through the audit sink.

    #[test]
    fn tracing_audit_accepted_does_not_panic() {
        let audit = TracingAudit;
        let id = identity(0x11);
        audit.accepted(device(1), &id);
    }

    #[test]
    fn tracing_audit_rejected_all_causes_do_not_panic() {
        let audit = TracingAudit;
        // Every RejectCause must be handled without panic (PRD §13.2 / §16.3).
        audit.rejected(device(2), RejectCause::UnknownDevice);
        audit.rejected(device(2), RejectCause::NotActive);
        audit.rejected(device(2), RejectCause::FailClosed);
    }

    #[test]
    fn tracing_audit_connected_does_not_panic() {
        TracingAudit.connected(device(3));
    }

    #[test]
    fn tracing_audit_event_rejected_does_not_panic() {
        // AC3 observability sink must be non-panicking for any reject count.
        TracingAudit.event_rejected(device(5), 1);
        TracingAudit.event_rejected(device(5), 0);
    }

    #[test]
    fn tracing_audit_event_flagged_does_not_panic() {
        // The clock-skew advisory sink (spec IR-0110 §5.9) must never panic.
        TracingAudit.event_flagged(device(6), "clock_skew");
    }

    #[test]
    fn default_event_flagged_is_a_no_op() {
        // A minimal sink relying on the trait default must compile and not panic.
        struct Minimal;
        impl AuditSink for Minimal {
            fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {}
            fn rejected(&self, _device: EndpointId, _cause: RejectCause) {}
            fn connected(&self, _device: EndpointId) {}
            fn disconnected(&self, _device: EndpointId) {}
        }
        Minimal.event_flagged(device(7), "clock_skew");
    }

    #[test]
    fn tracing_audit_disconnected_does_not_panic() {
        TracingAudit.disconnected(device(4));
    }

    #[test]
    fn tracing_audit_satisfies_send_sync_bound() {
        // AuditSink requires Send + Sync; verify TracingAudit satisfies the bound.
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<TracingAudit>();
    }

    #[test]
    fn tracing_audit_is_clone_and_default() {
        let a = TracingAudit;
        let _b = a.clone();
        let _ = TracingAudit;
    }

    // ── BlobDenyCause / blob.serve.* (IR-0204) ──────────────────────────────────

    #[test]
    fn blob_deny_cause_code_strings_are_stable() {
        // These strings appear verbatim in the audit log (spec §7); changing them
        // silently breaks log parsers and tooling.
        assert_eq!(BlobDenyCause::NotActive.code(), "not_active");
        assert_eq!(BlobDenyCause::NotReferenced.code(), "not_referenced");
        assert_eq!(BlobDenyCause::PushDenied.code(), "push_denied");
        assert_eq!(BlobDenyCause::ObserveDenied.code(), "observe_denied");
    }

    #[test]
    fn tracing_audit_blob_serve_accepted_does_not_panic() {
        TracingAudit.blob_serve_accepted(device(6), [0xAB; 32]);
    }

    #[test]
    fn tracing_audit_blob_serve_rejected_all_causes_do_not_panic() {
        let audit = TracingAudit;
        for cause in [
            BlobDenyCause::NotActive,
            BlobDenyCause::NotReferenced,
            BlobDenyCause::PushDenied,
            BlobDenyCause::ObserveDenied,
        ] {
            audit.blob_serve_rejected(device(7), cause, Some([0xCD; 32]));
            audit.blob_serve_rejected(device(7), cause, None);
        }
    }
}
