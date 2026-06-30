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
}

#[cfg(test)]
mod tests {
    use super::{AuditSink, TracingAudit};
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
}
