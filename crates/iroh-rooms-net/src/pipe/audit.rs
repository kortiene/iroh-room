//! The Live Pipe Plane audit sink (spec §6.6; PRD §13.2.7).
//!
//! Every open / connect / reject / accept / close / teardown is recorded locally
//! with a **stable, greppable reason string** (`pipe.opened`, `pipe.closed`,
//! `pipe.connect.accepted`, `pipe.connect.rejected:<cause>`, `pipe.torndown:<cause>`).
//! The reject/teardown causes map 1:1 to the core
//! [`DenyReason`](iroh_rooms_core::membership::DenyReason) plus `closed`. The strings
//! are pinned in a test so a parser-breaking silent rename is caught.

use iroh::EndpointId;

use super::hex16;

/// Why a pipe connection / live session was refused. Mirrors the core
/// [`DenyReason`](iroh_rooms_core::membership::DenyReason) and adds `Closed`
/// (the `pipe.closed`-known / unknown-pipe case the Pipe plane owns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeDenyCause {
    /// Stage 1: the device resolves to no known identity.
    UnknownDevice,
    /// Stage 1: the identity is not currently Active.
    NotActive,
    /// Stage 2: an Active member not in `allowed_members`.
    NotAllowed,
    /// Stage 2: the pipe owner is not currently Active.
    OwnerInactive,
    /// Stage 2: the pipe's `expires_at` has passed (the one wall-clock use).
    Expired,
    /// Stage 2: a `pipe.closed` is causally known, or the pipe is unknown locally.
    Closed,
}

impl PipeDenyCause {
    /// The stable lowercase reason suffix (`pipe.connect.rejected:<code>`).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownDevice => "unknown_device",
            Self::NotActive => "not_active",
            Self::NotAllowed => "not_allowed",
            Self::OwnerInactive => "owner_inactive",
            Self::Expired => "expired",
            Self::Closed => "closed",
        }
    }
}

impl From<iroh_rooms_core::membership::DenyReason> for PipeDenyCause {
    fn from(r: iroh_rooms_core::membership::DenyReason) -> Self {
        use iroh_rooms_core::membership::DenyReason as D;
        match r {
            D::UnknownDevice => Self::UnknownDevice,
            D::NotActive => Self::NotActive,
            D::NotAllowed => Self::NotAllowed,
            D::OwnerInactive => Self::OwnerInactive,
            D::Expired => Self::Expired,
            // `Unshared` is a blob-plane reason that cannot arise from the pipe
            // gate; map it to the closest fail-closed pipe cause.
            D::Unshared => Self::Closed,
        }
    }
}

/// A local audit sink for the Live Pipe Plane lifecycle (spec §6.6).
///
/// Implementations must be cheap and non-blocking — these are called inline on the
/// accept / teardown paths.
pub trait PipeAuditSink: Send + Sync + 'static {
    /// An owner published a `pipe.opened` (`pipe_id`, `allowed` count).
    fn opened(&self, pipe_id: &[u8; 16], allowed: usize);
    /// A `pipe.closed` was published (`pipe_id`, `reason`).
    fn closed(&self, pipe_id: &[u8; 16], reason: &str);
    /// A stream passed both gate stages and is now forwarding.
    fn connect_accepted(&self, device: EndpointId, pipe_id: &[u8; 16]);
    /// A connection / stream was refused at the gate.
    fn connect_rejected(
        &self,
        device: EndpointId,
        pipe_id: Option<&[u8; 16]>,
        cause: PipeDenyCause,
    );
    /// The teardown watcher severed a live session (revocation-on-learn).
    fn torndown(&self, device: EndpointId, pipe_id: &[u8; 16], cause: PipeDenyCause);
}

/// The default sink: structured `tracing` events with stable reason codes.
#[derive(Debug, Clone, Default)]
pub struct TracingPipeAudit;

impl PipeAuditSink for TracingPipeAudit {
    fn opened(&self, pipe_id: &[u8; 16], allowed: usize) {
        tracing::info!(reason = "pipe.opened", pipe = %hex16(pipe_id), allowed, "pipe exposed");
    }

    fn closed(&self, pipe_id: &[u8; 16], reason: &str) {
        tracing::info!(reason = "pipe.closed", pipe = %hex16(pipe_id), close_reason = reason, "pipe closed");
    }

    fn connect_accepted(&self, device: EndpointId, pipe_id: &[u8; 16]) {
        tracing::info!(
            reason = "pipe.connect.accepted",
            peer = %device,
            pipe = %hex16(pipe_id),
            "pipe stream forwarding"
        );
    }

    fn connect_rejected(
        &self,
        device: EndpointId,
        pipe_id: Option<&[u8; 16]>,
        cause: PipeDenyCause,
    ) {
        // `pipe.connect.rejected:<cause>` is the stable, greppable audit line. WARN
        // because a refused connect is security-relevant (PRD §16.3).
        tracing::warn!(
            reason = "pipe.connect.rejected",
            cause = cause.code(),
            peer = %device,
            pipe = pipe_id.map(hex16).unwrap_or_default(),
            "rejected pipe connection at the gate"
        );
    }

    fn torndown(&self, device: EndpointId, pipe_id: &[u8; 16], cause: PipeDenyCause) {
        tracing::warn!(
            reason = "pipe.torndown",
            cause = cause.code(),
            peer = %device,
            pipe = %hex16(pipe_id),
            "tore down a live pipe session (revocation-on-learn)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{PipeAuditSink, PipeDenyCause, TracingPipeAudit};
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::membership::DenyReason;

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn deny_cause_code_strings_are_stable() {
        // These appear verbatim in `pipe.connect.rejected:<cause>` / `pipe.torndown:<cause>`.
        // Changing them silently breaks audit parsers (spec §6.6).
        assert_eq!(PipeDenyCause::UnknownDevice.code(), "unknown_device");
        assert_eq!(PipeDenyCause::NotActive.code(), "not_active");
        assert_eq!(PipeDenyCause::NotAllowed.code(), "not_allowed");
        assert_eq!(PipeDenyCause::OwnerInactive.code(), "owner_inactive");
        assert_eq!(PipeDenyCause::Expired.code(), "expired");
        assert_eq!(PipeDenyCause::Closed.code(), "closed");
    }

    #[test]
    fn deny_reason_maps_one_to_one_to_pipe_cause() {
        assert_eq!(
            PipeDenyCause::from(DenyReason::UnknownDevice),
            PipeDenyCause::UnknownDevice
        );
        assert_eq!(
            PipeDenyCause::from(DenyReason::NotActive),
            PipeDenyCause::NotActive
        );
        assert_eq!(
            PipeDenyCause::from(DenyReason::NotAllowed),
            PipeDenyCause::NotAllowed
        );
        assert_eq!(
            PipeDenyCause::from(DenyReason::OwnerInactive),
            PipeDenyCause::OwnerInactive
        );
        assert_eq!(
            PipeDenyCause::from(DenyReason::Expired),
            PipeDenyCause::Expired
        );
    }

    #[test]
    fn deny_reason_unshared_maps_to_closed() {
        // `Unshared` is a blob-plane reason that cannot arise from the pipe gate.
        // The From impl explicitly maps it to `Closed` (the closest fail-closed
        // pipe cause) — pin that rather than rely on the silent default.
        assert_eq!(
            PipeDenyCause::from(DenyReason::Unshared),
            PipeDenyCause::Closed
        );
        assert_eq!(PipeDenyCause::Closed.code(), "closed");
    }

    #[test]
    fn tracing_pipe_audit_never_panics() {
        let a = TracingPipeAudit;
        a.opened(&[0x01; 16], 2);
        a.closed(&[0x01; 16], "closed");
        a.connect_accepted(device(1), &[0x02; 16]);
        a.connect_rejected(device(2), Some(&[0x03; 16]), PipeDenyCause::NotAllowed);
        a.connect_rejected(device(2), None, PipeDenyCause::UnknownDevice);
        a.torndown(device(3), &[0x04; 16], PipeDenyCause::NotActive);
    }
}
