//! The local admission/connection audit sink (PRD §13.2 pipe-style audit, §16.3
//! failure-mode distinction; spec §4.6 / §6).
//!
//! Every accept / reject / connect / disconnect is recorded locally with a
//! **stable reason string** (`peer.accepted`, `peer.rejected:unknown_device`, …),
//! so the reject vocabulary is exactly the PRD §16.3 offline-vs-unauthorized
//! distinction. It is a trait so the CLI can later persist it (a file/SQLite
//! sink); the default [`TracingAudit`] emits structured `tracing` events.

use iroh::EndpointId;
use iroh_rooms_core::event::ids::RoomId;
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

    /// A transport queue hit its configured capacity. Default: no-op.
    fn transport_queue_saturated(&self, _device: EndpointId, _queue: &'static str) {}

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

    /// The room approached its active-member ceiling (issue #144): a live
    /// observer (`RoomReconciler`) saw the active count cross from strictly
    /// below `ACTIVE_MEMBER_WARNING_THRESHOLD` to at/above it. Pure
    /// observability — never changes authorization, the hard
    /// `RejectReason::RoomFull` cap, or the signed event log.
    ///
    /// - `active` is the current active-member count.
    /// - `max` is the hard cap (`MAX_ACTIVE_MEMBERS`, currently `5`).
    /// - `remaining` is `max.saturating_sub(active)` (the headroom left).
    ///
    /// Stable reason code: `room.active_members.near_cap`. Emitters must keep
    /// this secret-free (room id + numeric counts only). Default: no-op, so
    /// existing sinks need not change.
    fn active_member_threshold_reached(
        &self,
        _room_id: &RoomId,
        _active: usize,
        _max: usize,
        _remaining: usize,
    ) {
    }

    /// A `SyncMessage::Events` body was broadcast on the room's gossip topic
    /// (issue #171 / spec §5.4). `bytes` is the encoded body length. Stable
    /// reason code: `gossip.broadcast`. Default: no-op, so existing sinks need
    /// not change. Only emitted when the `gossip_overlay` feature is on.
    fn gossip_broadcast(&self, _room_id: RoomId, _bytes: usize) {}

    /// A frame arrived over the gossip overlay from `device` (issue #171 /
    /// spec §5.4). `bytes` is the encoded body length. Stable reason code:
    /// `gossip.received`. Default: no-op, so existing sinks need not change.
    /// Only emitted when the `gossip_overlay` feature is on.
    fn gossip_received(&self, _device: EndpointId, _bytes: usize) {}

    /// The gossip receiver fell behind and dropped frames (issue #171 / spec
    /// §5.4, §4 D8). The recovery path is the engine's next anti-entropy pull
    /// (same shape as the mesh's "link dropped, re-pull" signal). Stable
    /// reason code: `gossip.lagged`. Default: no-op, so existing sinks need
    /// not change. Only emitted when the `gossip_overlay` feature is on.
    fn gossip_lagged(&self, _device: EndpointId) {}

    /// A gossip swarm neighbor came up for one of this node's subscribed
    /// topics (issue #171 / spec §5.4). Stable reason code:
    /// `gossip.neighbor_up`. Default: no-op, so existing sinks need not
    /// change. Only emitted when the `gossip_overlay` feature is on.
    fn gossip_neighbor_up(&self, _device: EndpointId) {}

    /// A gossip swarm neighbor went down for one of this node's subscribed
    /// topics (issue #171 / spec §5.4). Stable reason code:
    /// `gossip.neighbor_down`. Default: no-op, so existing sinks need not
    /// change. Only emitted when the `gossip_overlay` feature is on.
    fn gossip_neighbor_down(&self, _device: EndpointId) {}

    /// A `GOSSIP_ALPN` connection was rejected before any gossip byte was
    /// exchanged (issue #171 / spec §4 D2 / §5.4). The wrapper closes the
    /// connection at the same `REJECT_CODE` the event-plane gate uses, so the
    /// dialing side's reject-detection logic works for gossip redials too.
    /// Stable reason code: `gossip.topic_rejected:<cause>`. Default: no-op, so
    /// existing sinks need not change. Only emitted when the `gossip_overlay`
    /// feature is on.
    fn gossip_topic_rejected(&self, _device: EndpointId, _cause: RejectCause) {}
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

    fn transport_queue_saturated(&self, device: EndpointId, queue: &'static str) {
        tracing::warn!(
            reason = "transport.queue.saturated",
            queue,
            peer = %device,
            "bounded transport queue saturated; dropping frame and closing the peer link"
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

    fn active_member_threshold_reached(
        &self,
        room_id: &RoomId,
        active: usize,
        max: usize,
        remaining: usize,
    ) {
        // `room.active_members.near_cap` is the stable, greppable audit line for
        // the approach-to-ceiling warning (issue #144). WARN: an operator-relevant
        // signal that the next join may fail with `RejectReason::RoomFull`.
        tracing::warn!(
            reason = "room.active_members.near_cap",
            room = %room_id,
            active,
            max,
            remaining,
            threshold = iroh_rooms_core::membership::ACTIVE_MEMBER_WARNING_THRESHOLD,
            "room is approaching the active-member ceiling"
        );
    }

    fn gossip_broadcast(&self, room_id: RoomId, bytes: usize) {
        // `gossip.broadcast` — an Events frame fanned out over the gossip
        // overlay (issue #171 / spec §5.4). DEBUG: a hot-path event, one per
        // Events send.
        tracing::debug!(
            reason = "gossip.broadcast",
            room = %room_id,
            bytes,
            "broadcast an Events frame on the room gossip topic"
        );
    }

    fn gossip_received(&self, device: EndpointId, bytes: usize) {
        // `gossip.received` — a frame arrived over the overlay (issue #171).
        // DEBUG: hot-path, one per delivered gossip frame.
        tracing::debug!(
            reason = "gossip.received",
            peer = %device,
            bytes,
            "received an Events frame over the gossip overlay"
        );
    }

    fn gossip_lagged(&self, device: EndpointId) {
        // `gossip.lagged` — the receiver fell behind; anti-entropy will recover
        // (issue #171 / spec §4 D8). WARN: a delivery-gap signal.
        tracing::warn!(
            reason = "gossip.lagged",
            peer = %device,
            "gossip receiver lagged; anti-entropy pull will recover"
        );
    }

    fn gossip_neighbor_up(&self, device: EndpointId) {
        tracing::debug!(
            reason = "gossip.neighbor_up",
            peer = %device,
            "gossip swarm neighbor came up"
        );
    }

    fn gossip_neighbor_down(&self, device: EndpointId) {
        tracing::debug!(
            reason = "gossip.neighbor_down",
            peer = %device,
            "gossip swarm neighbor went down"
        );
    }

    fn gossip_topic_rejected(&self, device: EndpointId, cause: RejectCause) {
        // `gossip.topic_rejected:<cause>` mirrors `peer.rejected:<cause>` for
        // the GOSSIP_ALPN admission wrapper (issue #171 / spec §4 D2). WARN:
        // a security-relevant event — an unadmitted device attempted to join
        // the gossip plane.
        tracing::warn!(
            reason = "gossip.topic_rejected",
            cause = cause.code(),
            peer = %device,
            "rejected a GOSSIP_ALPN connection before any gossip byte"
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

    // ── gossip.* (issue #171 / spec §5.4) ────────────────────────────────────
    //
    // The gossip overlay audit events must be non-blocking and non-panicking on
    // every path through the sink, mirroring the existing admission/lifecycle
    // guarantees. They surface only when the `gossip_overlay` feature is on,
    // but the trait/impl contracts must hold regardless.

    #[test]
    fn tracing_audit_gossip_events_do_not_panic() {
        let audit = TracingAudit;
        let room = iroh_rooms_core::event::ids::RoomId::from_bytes([0x71; 32]);
        audit.gossip_broadcast(room, 1024);
        audit.gossip_received(device(8), 1024);
        audit.gossip_lagged(device(8));
        audit.gossip_neighbor_up(device(9));
        audit.gossip_neighbor_down(device(9));
        audit.gossip_topic_rejected(device(10), RejectCause::UnknownDevice);
        audit.gossip_topic_rejected(device(10), RejectCause::NotActive);
        audit.gossip_topic_rejected(device(10), RejectCause::FailClosed);
    }

    #[test]
    fn default_gossip_events_are_no_ops() {
        // A minimal sink relying on the trait default must compile and not
        // panic for the gossip events, so legacy sinks (the CLI's stderr
        // renderer, custom hosts) need no changes when the overlay is on.
        struct Minimal;
        impl AuditSink for Minimal {
            fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {}
            fn rejected(&self, _device: EndpointId, _cause: RejectCause) {}
            fn connected(&self, _device: EndpointId) {}
            fn disconnected(&self, _device: EndpointId) {}
        }
        let m = Minimal;
        let room = iroh_rooms_core::event::ids::RoomId::from_bytes([0x71; 32]);
        m.gossip_broadcast(room, 0);
        m.gossip_received(device(11), 0);
        m.gossip_lagged(device(11));
        m.gossip_neighbor_up(device(12));
        m.gossip_neighbor_down(device(12));
        m.gossip_topic_rejected(device(12), RejectCause::NotActive);
    }
}
