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
use std::sync::{Arc, Mutex};

use iroh::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::membership::MembershipSnapshot;

/// The decision the accept-gate makes from a proven remote [`EndpointId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// The device is bound to a currently-Active member: admit it.
    Admit {
        /// The membership identity (`sender_id`) the device is bound to.
        identity: IdentityKey,
    },
    /// Admit the connection **provisionally** for the join bootstrap (IR-0104,
    /// Approach A): the device is **not** a known Active member, but the local node
    /// is hosting joins and an invite is open, so a first-time invitee is allowed to
    /// pull the (secret-free) membership sub-DAG and push a single `member.joined`.
    /// The connection is served membership events only and grants **no** membership
    /// by itself — every peer's `gate_join` remains the authorization authority. See
    /// [`JoinBootstrapAdmission`].
    AdmitProvisional,
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

/// An immutable admission decision table derived from a membership snapshot: the
/// `device_id → identity` reverse map, the Active identity set, and the §0/§5
/// fail-closed overlay. It carries exactly the three lookups
/// [`AllowlistAdmission`] holds; [`SnapshotAdmission`] swaps a whole new view in
/// atomically each time the fold changes, so admission tracks the **live** roster.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionView {
    device_to_identity: HashMap<EndpointId, IdentityKey>,
    active: HashSet<IdentityKey>,
    fail_closed: HashSet<IdentityKey>,
}

impl AdmissionView {
    /// An empty view — fail-closed: every device is rejected as `UnknownDevice`
    /// until the first snapshot is folded in.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build the view from the current membership snapshot plus the engine's
    /// fail-closed subject set (`SyncEngine::fail_closed_subjects`).
    ///
    /// **Every** member with a bound device is entered into the reverse map so a
    /// bound-but-inactive device (a removed/left member) resolves to `NotActive`
    /// rather than `UnknownDevice` — the same distinction the fold makes. Only
    /// `Active` identities go into the active set, so a since-removed member's
    /// device stops being admitted the moment the fold learns of the removal.
    #[must_use]
    pub fn from_snapshot(snapshot: &MembershipSnapshot, fail_closed: &[IdentityKey]) -> Self {
        let mut device_to_identity = HashMap::new();
        let mut active = HashSet::new();
        for m in snapshot.members() {
            if let Some(dev) = m.device {
                if let Ok(id) = EndpointId::from_bytes(dev.as_bytes()) {
                    device_to_identity.insert(id, m.identity);
                }
            }
            if snapshot.is_active(&m.identity) {
                active.insert(m.identity);
            }
        }
        Self {
            device_to_identity,
            active,
            fail_closed: fail_closed.iter().copied().collect(),
        }
    }

    /// The admission decision for `device` under this view — the **exact** decision
    /// order of [`AllowlistAdmission`] (`UnknownDevice` → `FailClosed` →
    /// `NotActive` → `Admit`), so reject-before-bytes and every admission test
    /// semantics are unchanged.
    #[must_use]
    fn decide(&self, device: EndpointId) -> AdmissionDecision {
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

/// Admission backed by the **live** membership snapshot (the IR-0005 NOTES D6/OQ-6
/// production re-point, now due — spec §4.4).
///
/// `authorize(device)` reads the current [`AdmissionView`] out of a shared cell on
/// every call, so a device removed mid-session begins being rejected as soon as the
/// pump swaps in the post-removal view. The read takes a short, non-blocking
/// critical section (`Mutex`, never held across an `.await`); at MVP room sizes
/// (N≤5) this is well below any contention that would justify a lock-free
/// `arc-swap` dependency (spec OQ-1). The pump is the **sole writer** of the cell.
#[derive(Clone)]
pub struct SnapshotAdmission {
    cell: Arc<Mutex<AdmissionView>>,
}

impl SnapshotAdmission {
    /// Wrap a shared admission cell. The caller keeps a clone of `cell` and swaps a
    /// fresh [`AdmissionView`] into it whenever the fold changes.
    #[must_use]
    pub fn new(cell: Arc<Mutex<AdmissionView>>) -> Self {
        Self { cell }
    }
}

impl std::fmt::Debug for SnapshotAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapshotAdmission")
    }
}

impl Admission for SnapshotAdmission {
    fn authorize(&self, device: EndpointId) -> AdmissionDecision {
        self.cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .decide(device)
    }
}

/// A provisional-aware admission gate for an admin hosting joins (IR-0104,
/// Approach A — the join bootstrap seam).
///
/// It wraps an inner [`AllowlistAdmission`] (which already admits Active members
/// and default-denies everyone else) and changes exactly **one** outcome: when
/// `accept_joins` is set, a genuinely **unknown** device — one bound to no member,
/// i.e. a first-time invitee whose device the room has never seen — is
/// [`AdmitProvisional`](AdmissionDecision::AdmitProvisional) instead of rejected, so
/// it can pull the secret-free membership sub-DAG and push its `member.joined`.
///
/// Every other outcome is the inner gate's verbatim:
/// * an **Active** member is admitted normally ([`Admit`](AdmissionDecision::Admit));
/// * a **bound-but-inactive** device (a removed/left member, or an invitee whose
///   device is already known) is still rejected with `NotActive` — sticky departure
///   and the single-join bootstrap are preserved;
/// * a **fail-closed** identity is still rejected.
///
/// With `accept_joins` unset (a non-hosting node, or one with no open invites) the
/// gate is byte-for-byte its inner [`AllowlistAdmission`], so a quiescent room
/// admits no strangers. The provisional admission is a **liveness + privacy**
/// mechanism, never an authorization one: `gate_join` still decides membership on
/// every peer, so a provisional peer that fails the capability/key/expiry/role gate
/// grants nothing anywhere.
///
/// Generic over the inner gate `A` so it can wrap either the frozen
/// [`AllowlistAdmission`] (fixtures/tests) or the live [`SnapshotAdmission`] (the
/// IR-0107 production re-point) without duplicating the overlay logic; the default
/// keeps the historical `JoinBootstrapAdmission` (over `AllowlistAdmission`) working
/// unchanged.
#[derive(Debug, Clone)]
pub struct JoinBootstrapAdmission<A: Admission = AllowlistAdmission> {
    inner: A,
    accept_joins: bool,
}

impl<A: Admission> JoinBootstrapAdmission<A> {
    /// Wrap `inner` with the provisional join-bootstrap overlay. `accept_joins`
    /// should be set by the admin's join-hosting session **only** while at least one
    /// invite is open; the caller computes that policy (caller-is-admin +
    /// pending-invite) and passes the result here.
    #[must_use]
    pub fn new(inner: A, accept_joins: bool) -> Self {
        Self {
            inner,
            accept_joins,
        }
    }

    /// Whether this gate currently admits first-time invitees provisionally.
    #[must_use]
    pub fn accepts_joins(&self) -> bool {
        self.accept_joins
    }
}

impl<A: Admission> Admission for JoinBootstrapAdmission<A> {
    fn authorize(&self, device: EndpointId) -> AdmissionDecision {
        match self.inner.authorize(device) {
            // An unknown device + an open join window ⇒ provisional bootstrap admit.
            AdmissionDecision::Reject(RejectCause::UnknownDevice) if self.accept_joins => {
                AdmissionDecision::AdmitProvisional
            }
            // Active member, bound-but-inactive, fail-closed, or unknown-with-no-open
            // -invites: the inner gate's verdict is authoritative and unchanged.
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Admission, AdmissionDecision, AdmissionView, AllowlistAdmission, JoinBootstrapAdmission,
        RejectCause, SnapshotAdmission,
    };
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::keys::IdentityKey;
    use std::sync::{Arc, Mutex};

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn identity(seed: u8) -> IdentityKey {
        IdentityKey::from_bytes([seed; 32])
    }

    /// Build an [`AdmissionView`] directly (same-module access to private fields)
    /// so the live-admission tests need no full membership fold.
    fn view(
        bindings: &[(EndpointId, IdentityKey)],
        active: &[IdentityKey],
        fc: &[IdentityKey],
    ) -> AdmissionView {
        AdmissionView {
            device_to_identity: bindings.iter().copied().collect(),
            active: active.iter().copied().collect(),
            fail_closed: fc.iter().copied().collect(),
        }
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

    // ── SnapshotAdmission (IR-0107) — the live-roster re-point ──────────────────

    #[test]
    fn snapshot_admission_matches_allowlist_decision_matrix() {
        // The four decision outcomes must match AllowlistAdmission exactly, so the
        // reject-before-bytes guarantee and every admission semantic is unchanged.
        let id_active = identity(0xA1);
        let id_inactive = identity(0xB2);
        let id_fc = identity(0xC3);
        let v = view(
            &[
                (device(1), id_active),
                (device(2), id_inactive),
                (device(3), id_fc),
            ],
            &[id_active, id_fc],
            &[id_fc],
        );
        let gate = SnapshotAdmission::new(Arc::new(Mutex::new(v)));

        // unknown device
        assert_eq!(
            gate.authorize(device(9)),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
        // bound but not active
        assert_eq!(
            gate.authorize(device(2)),
            AdmissionDecision::Reject(RejectCause::NotActive)
        );
        // fail-closed takes priority over active
        assert_eq!(
            gate.authorize(device(3)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
        // bound + active
        assert_eq!(
            gate.authorize(device(1)),
            AdmissionDecision::Admit {
                identity: id_active
            }
        );
    }

    #[test]
    fn snapshot_admission_live_flip_on_mid_session_removal() {
        // Admit, then swap in a view without the identity (a mid-session removal):
        // the very next authorize must reject — proving admission tracks the live
        // roster, not a start-of-command freeze (AC2).
        let id = identity(0xD4);
        let cell = Arc::new(Mutex::new(view(&[(device(5), id)], &[id], &[])));
        let gate = SnapshotAdmission::new(cell.clone());
        assert_eq!(
            gate.authorize(device(5)),
            AdmissionDecision::Admit { identity: id }
        );

        // The pump swaps in the post-removal view: device still bound, no longer active.
        *cell.lock().unwrap() = view(&[(device(5), id)], &[], &[]);
        assert_eq!(
            gate.authorize(device(5)),
            AdmissionDecision::Reject(RejectCause::NotActive)
        );
    }

    #[test]
    fn empty_view_is_fail_closed() {
        let gate = SnapshotAdmission::new(Arc::new(Mutex::new(AdmissionView::empty())));
        assert_eq!(
            gate.authorize(device(1)),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
    }

    #[test]
    fn join_bootstrap_wraps_snapshot_admission() {
        // The generic overlay must compose with the live gate: an unknown device in
        // an open join window is provisional; an Active member is admitted normally.
        let id = identity(0xE5);
        let cell = Arc::new(Mutex::new(view(&[(device(6), id)], &[id], &[])));
        let gate = JoinBootstrapAdmission::new(SnapshotAdmission::new(cell), true);
        assert_eq!(
            gate.authorize(device(6)),
            AdmissionDecision::Admit { identity: id }
        );
        assert_eq!(
            gate.authorize(device(99)),
            AdmissionDecision::AdmitProvisional
        );
    }

    // ── JoinBootstrapAdmission (IR-0104, Approach A) — the provisional overlay ──

    #[test]
    fn bootstrap_unknown_device_with_open_window_is_provisional() {
        // The first-time invitee: bound to no member, hosting joins ⇒ provisional.
        let gate = JoinBootstrapAdmission::new(AllowlistAdmission::new(), true);
        assert_eq!(
            gate.authorize(device(9)),
            AdmissionDecision::AdmitProvisional
        );
        assert!(gate.accepts_joins());
    }

    #[test]
    fn bootstrap_unknown_device_without_open_window_is_rejected() {
        // Not hosting joins (quiescent / non-admin) ⇒ a stranger is admitted nothing,
        // exactly the inner gate's verdict.
        let gate = JoinBootstrapAdmission::new(AllowlistAdmission::new(), false);
        assert_eq!(
            gate.authorize(device(9)),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
        assert!(!gate.accepts_joins());
    }

    #[test]
    fn bootstrap_active_member_is_admitted_normally() {
        // An Active member is admitted in full even while hosting joins — provisional
        // applies only to genuinely-unknown devices.
        let id = identity(0xA1);
        let inner = AllowlistAdmission::new()
            .bind_device(device(1), id)
            .set_active(id);
        let gate = JoinBootstrapAdmission::new(inner, true);
        assert_eq!(
            gate.authorize(device(1)),
            AdmissionDecision::Admit { identity: id }
        );
    }

    #[test]
    fn bootstrap_bound_but_inactive_device_is_still_rejected() {
        // A removed/left member (bound, not Active) is NOT provisional — sticky
        // departure and the single-join bootstrap are preserved.
        let id = identity(0xB2);
        let inner = AllowlistAdmission::new().bind_device(device(2), id);
        let gate = JoinBootstrapAdmission::new(inner, true);
        assert_eq!(
            gate.authorize(device(2)),
            AdmissionDecision::Reject(RejectCause::NotActive)
        );
    }

    #[test]
    fn bootstrap_fail_closed_identity_is_still_rejected() {
        let id = identity(0xC3);
        let inner = AllowlistAdmission::new()
            .bind_device(device(3), id)
            .set_active(id)
            .set_fail_closed(id);
        let gate = JoinBootstrapAdmission::new(inner, true);
        assert_eq!(
            gate.authorize(device(3)),
            AdmissionDecision::Reject(RejectCause::FailClosed)
        );
    }

    #[test]
    fn bootstrap_active_member_admitted_normally_when_not_accepting_joins() {
        // accept_joins=false + Active member: the "other => other" arm in
        // JoinBootstrapAdmission must fall through to the inner gate's Admit —
        // quiescing the join window must not block already-Active members.
        let id = identity(0xD4);
        let inner = AllowlistAdmission::new()
            .bind_device(device(6), id)
            .set_active(id);
        let gate = JoinBootstrapAdmission::new(inner, false);
        assert_eq!(
            gate.authorize(device(6)),
            AdmissionDecision::Admit { identity: id },
            "Active member must be admitted even when accept_joins is false"
        );
    }

    #[test]
    fn bootstrap_two_unknown_devices_both_admitted_provisionally() {
        // Two independent first-time invitees: both must receive AdmitProvisional.
        // The provisional path is not one-time or device-count-limited.
        let gate = JoinBootstrapAdmission::new(AllowlistAdmission::new(), true);
        assert_eq!(
            gate.authorize(device(20)),
            AdmissionDecision::AdmitProvisional,
            "first unknown device must be AdmitProvisional"
        );
        assert_eq!(
            gate.authorize(device(21)),
            AdmissionDecision::AdmitProvisional,
            "second independent unknown device must also be AdmitProvisional"
        );
    }

    #[test]
    fn bootstrap_unknown_device_not_active_member_stays_rejected_when_joins_closed() {
        // A device that was previously bound but is now not Active (e.g. Removed)
        // must still be rejected even if accept_joins is toggled. The "sticky
        // departure" guarantee must not be bypassed by a re-open of the join window.
        let id = identity(0xE5);
        let inner = AllowlistAdmission::new().bind_device(device(7), id);
        // id is bound but not Active (Removed / Invited-only).
        let gate = JoinBootstrapAdmission::new(inner, true);
        assert_eq!(
            gate.authorize(device(7)),
            AdmissionDecision::Reject(RejectCause::NotActive),
            "bound-but-inactive device must be rejected even with accept_joins=true"
        );
    }
}
