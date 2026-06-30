//! The connect-accept admission gate (`PHASE-0-SPIKE.md` Membership & Ordering §5;
//! the issue's security note; spec §4.4 / §6).
//!
//! Admission is a **property of the transport** (ADR-1): the decision is made from
//! the QUIC/TLS-authenticated remote [`EndpointId`] (`device_id`) alone — never
//! from any self-asserted application field, and **before any event byte is
//! read**. The gate resolves `device_id → bound identity → Active?`, exactly the
//! `MembershipSnapshot` shape (§5), so the production re-point is a swap of the
//! two lookups for [`MembershipSnapshot`](iroh_rooms_core::membership::MembershipSnapshot),
//! not a reshape (the reusable-shape seam proven by `spike-blobs::acl::AuthContext`).

use std::collections::{HashMap, HashSet};

use iroh::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;

/// The decision the accept-gate makes from a proven remote [`EndpointId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// The device is bound to a currently-Active member: admit it.
    Admit {
        /// The membership identity (`sender_id`) the device is bound to.
        identity: IdentityKey,
    },
    /// Reject the connection (close before `accept_bi()`); no bytes are read.
    Reject(RejectCause),
}

/// Why a remote endpoint was refused — the PRD §16.3 reject vocabulary, carried
/// into the audit log and the dialer's connection-state surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCause {
    /// The `device_id` is not bound to any known identity (ban-evasion under a
    /// fresh key lands here). Default-deny.
    UnknownDevice,
    /// The device is bound, but its identity is not currently Active
    /// (Invited-only, Removed, or Left).
    NotActive,
    /// The local admin view is incomplete for this subject, so admission fails
    /// closed (§0/§5 fail-closed overlay). See [`Admission`] note on the overlay.
    FailClosed,
}

impl RejectCause {
    /// Stable lowercase reason string for the audit log (PRD §13.2 / §16.3).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownDevice => "unknown_device",
            Self::NotActive => "not_active",
            Self::FailClosed => "fail_closed",
        }
    }
}

/// Resolve a QUIC/TLS-authenticated remote `device_id` (== [`EndpointId`]) to an
/// [`AdmissionDecision`].
///
/// The production implementation reads a
/// [`MembershipSnapshot`](iroh_rooms_core::membership::MembershipSnapshot)
/// (device→identity reverse map + Active set + the §0/§5 fail-closed overlay).
/// The prototype uses [`AllowlistAdmission`], which has the same shape.
pub trait Admission: Send + Sync + 'static {
    /// Decide whether to admit a connection from `device`. Must be pure and fast:
    /// it runs inline on the accept path before any stream is accepted.
    fn authorize(&self, device: EndpointId) -> AdmissionDecision;
}

/// The fold-shaped prototype authorizer (spec D6).
///
/// Identical decision logic to the landed blob gate: an unbound device, or a
/// device bound to a non-Active identity, is **rejected** (fail-closed default).
/// The `fail_closed` set is the explicit, tested seam for the §0/§5 incompleteness
/// overlay (spec OQ-6) — production wiring fills it from
/// [`SyncEngine::fail_closed_subjects`](iroh_rooms_core::sync::SyncEngine::fail_closed_subjects);
/// the prototype leaves it empty but honours it when populated.
#[derive(Debug, Clone, Default)]
pub struct AllowlistAdmission {
    device_to_identity: HashMap<EndpointId, IdentityKey>,
    active: HashSet<IdentityKey>,
    fail_closed: HashSet<IdentityKey>,
}

impl AllowlistAdmission {
    /// An empty authorizer. Fail-closed: every device is rejected until bound and
    /// marked Active.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the validated device binding `device_id → sender_id` (§5). Returns
    /// `self` for fluent fixture construction.
    #[must_use]
    pub fn bind_device(mut self, device: EndpointId, identity: IdentityKey) -> Self {
        self.device_to_identity.insert(device, identity);
        self
    }

    /// Mark an identity as currently Active (admin counts as Active here).
    #[must_use]
    pub fn set_active(mut self, identity: IdentityKey) -> Self {
        self.active.insert(identity);
        self
    }

    /// Mark an identity as fail-closed for removal-sensitive admission (§0/§5
    /// overlay seam). A fail-closed identity is rejected even while nominally
    /// Active, until the local admin view catches up.
    #[must_use]
    pub fn set_fail_closed(mut self, identity: IdentityKey) -> Self {
        self.fail_closed.insert(identity);
        self
    }

    /// Resolve a device to its bound identity, if any (the §5 reverse map).
    #[must_use]
    pub fn identity_of(&self, device: EndpointId) -> Option<&IdentityKey> {
        self.device_to_identity.get(&device)
    }
}

impl Admission for AllowlistAdmission {
    fn authorize(&self, device: EndpointId) -> AdmissionDecision {
        let Some(identity) = self.device_to_identity.get(&device) else {
            return AdmissionDecision::Reject(RejectCause::UnknownDevice);
        };
        if self.fail_closed.contains(identity) {
            return AdmissionDecision::Reject(RejectCause::FailClosed);
        }
        if !self.active.contains(identity) {
            return AdmissionDecision::Reject(RejectCause::NotActive);
        }
        AdmissionDecision::Admit {
            identity: *identity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Admission, AdmissionDecision, AllowlistAdmission, RejectCause};
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::keys::IdentityKey;

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn identity(seed: u8) -> IdentityKey {
        IdentityKey::from_bytes([seed; 32])
    }

    #[test]
    fn unbound_device_is_rejected_as_unknown() {
        let auth = AllowlistAdmission::new();
        assert_eq!(
            auth.authorize(device(9)),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
    }

    #[test]
    fn bound_but_inactive_device_is_rejected_as_not_active() {
        // Bound to an identity that was never set Active (Removed/Invited-only).
        let auth = AllowlistAdmission::new().bind_device(device(1), identity(0xA1));
        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Reject(RejectCause::NotActive)
        );
    }

    #[test]
    fn bound_and_active_device_is_admitted_to_its_identity() {
        let id = identity(0xA1);
        let auth = AllowlistAdmission::new()
            .bind_device(device(1), id)
            .set_active(id);
        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Admit { identity: id }
        );
    }

    #[test]
    fn fail_closed_identity_is_rejected_even_when_active() {
        let id = identity(0xA1);
        let auth = AllowlistAdmission::new()
            .bind_device(device(1), id)
            .set_active(id)
            .set_fail_closed(id);
        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
    }

    #[test]
    fn empty_authorizer_is_fail_closed_by_default() {
        let auth = AllowlistAdmission::new();
        assert!(matches!(
            auth.authorize(device(42)),
            AdmissionDecision::Reject(_)
        ));
        assert!(auth.identity_of(device(42)).is_none());
    }

    // --- Stable audit-log strings ---

    #[test]
    fn reject_cause_code_strings_are_stable() {
        // These strings appear verbatim in the audit log (PRD §13.2 / §16.3).
        // Changing them silently breaks log parsers and tooling.
        assert_eq!(RejectCause::UnknownDevice.code(), "unknown_device");
        assert_eq!(RejectCause::NotActive.code(), "not_active");
        assert_eq!(RejectCause::FailClosed.code(), "fail_closed");
    }

    // --- identity_of lookup ---

    #[test]
    fn identity_of_returns_none_for_unbound_device() {
        let auth = AllowlistAdmission::new();
        assert!(auth.identity_of(device(5)).is_none());
    }

    #[test]
    fn identity_of_returns_the_bound_identity() {
        let id = identity(0xAA);
        let auth = AllowlistAdmission::new().bind_device(device(5), id);
        assert_eq!(auth.identity_of(device(5)), Some(&id));
    }

    // --- Multi-device and multi-identity edge cases ---

    #[test]
    fn two_devices_same_identity_both_admitted_when_active() {
        let id = identity(0xBB);
        let auth = AllowlistAdmission::new()
            .bind_device(device(1), id)
            .bind_device(device(2), id)
            .set_active(id);
        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Admit { identity: id }
        );
        assert_eq!(
            auth.authorize(device(2)),
            AdmissionDecision::Admit { identity: id }
        );
    }

    #[test]
    fn two_devices_same_identity_one_fail_closed_both_rejected() {
        // fail_closed is on the identity, so all devices bound to that identity
        // are rejected — not just the one directly marked fail_closed.
        let id = identity(0xCC);
        let auth = AllowlistAdmission::new()
            .bind_device(device(1), id)
            .bind_device(device(2), id)
            .set_active(id)
            .set_fail_closed(id);
        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
        assert_eq!(
            auth.authorize(device(2)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
    }

    #[test]
    fn independent_identities_do_not_affect_each_other() {
        let id_a = identity(0x0A);
        let id_b = identity(0x0B);
        let auth = AllowlistAdmission::new()
            .bind_device(device(10), id_a)
            .set_active(id_a)
            // id_b bound to device 11, but id_b is NOT set Active
            .bind_device(device(11), id_b);
        // device 10 (id_a, Active) → Admit
        assert_eq!(
            auth.authorize(device(10)),
            AdmissionDecision::Admit { identity: id_a }
        );
        // device 11 (id_b, not Active) → NotActive (not affected by id_a's Active status)
        assert_eq!(
            auth.authorize(device(11)),
            AdmissionDecision::Reject(RejectCause::NotActive)
        );
    }

    // --- bind_device overwrite: last binding wins ---

    #[test]
    fn rebinding_device_to_new_identity_last_binding_wins() {
        let id_a = identity(0xA0);
        let id_b = identity(0xB0);
        // Bind device(1) to id_a, then rebind to id_b; only id_b is Active.
        let auth = AllowlistAdmission::new()
            .bind_device(device(1), id_a)
            .bind_device(device(1), id_b) // overwrites id_a
            .set_active(id_b);

        assert_eq!(
            auth.authorize(device(1)),
            AdmissionDecision::Admit { identity: id_b },
            "the last bind_device call must win; the previous binding is overwritten"
        );
        assert_eq!(
            auth.identity_of(device(1)),
            Some(&id_b),
            "identity_of must reflect the most recent binding"
        );
    }

    #[test]
    fn rebinding_device_drops_access_for_original_identity() {
        let id_a = identity(0xA1);
        let id_b = identity(0xB1);
        // id_a is Active, but device is rebound to id_b which is NOT Active.
        let auth = AllowlistAdmission::new()
            .bind_device(device(2), id_a)
            .set_active(id_a)
            .bind_device(device(2), id_b); // overwrite: now bound to inactive id_b

        assert_eq!(
            auth.authorize(device(2)),
            AdmissionDecision::Reject(RejectCause::NotActive),
            "after rebind the original Active identity no longer grants access"
        );
    }

    // --- set_active is idempotent ---

    #[test]
    fn set_active_called_twice_still_admits() {
        let id = identity(0xCC);
        let auth = AllowlistAdmission::new()
            .bind_device(device(3), id)
            .set_active(id)
            .set_active(id); // calling twice must not cause issues
        assert_eq!(
            auth.authorize(device(3)),
            AdmissionDecision::Admit { identity: id }
        );
    }

    // --- set_fail_closed takes priority over set_active regardless of order ---

    #[test]
    fn fail_closed_after_active_still_rejects() {
        let id = identity(0xDD);
        // set_active first, then set_fail_closed (the normal "removal pending" path).
        let auth = AllowlistAdmission::new()
            .bind_device(device(4), id)
            .set_active(id)
            .set_fail_closed(id);
        assert_eq!(
            auth.authorize(device(4)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
    }
}
