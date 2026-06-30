//! Authorization fixture for the blob serve gate (`PHASE-0-SPIKE.md` Membership
//! & Ordering §5).
//!
//! This is the **reusable shape**: the real membership causal fold (Day 7 /
//! IR-0007) is not built yet, so the spike feeds the gate from an in-memory
//! fixture that has the *same shapes* the fold will later produce:
//!
//! - `device_to_identity`: the validated device binding (`device_id` ==
//!   `EndpointId` → `sender_id`). The gate authorizes against the **identity**,
//!   not the raw device key, exactly like §5.
//! - `active_members`: the set of identities currently Active (admin counts as
//!   Active). Snapshot + fail-closed.
//! - `referenced_hashes`: hashes referenced by a valid `file.shared` authored by
//!   an Active member and causally visible. The spike *precomputes* this set
//!   from the fixture `file.shared` payload(s), stubbing provenance while
//!   preserving the gate's behavior (the thing under test). See `NOTES.md` §3.
//!
//! Swapping the fixture for the real fold is a re-point of these three
//! collections, not a reshape of the gate — that is the seam this spike proves.

use std::collections::{HashMap, HashSet};

use iroh::EndpointId;
use iroh_blobs::Hash;

/// Identity (`sender_id`) key. The real fold uses a signed identity key; the
/// spike uses a stable string label so the fixture is readable. The gate never
/// inspects its contents, only set-membership, so the type is a drop-in seam.
pub type IdentityKey = String;

/// The minimal, fold-shaped authorization context consulted by both gates.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    device_to_identity: HashMap<EndpointId, IdentityKey>,
    active_members: HashSet<IdentityKey>,
    referenced_hashes: HashSet<Hash>,
}

impl AuthContext {
    /// An empty context (no members, no referenced hashes). Fail-closed: every
    /// connect and every hash is denied until populated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the validated device binding `device_id -> sender_id`.
    pub fn bind_device(&mut self, device: EndpointId, identity: impl Into<IdentityKey>) {
        self.device_to_identity.insert(device, identity.into());
    }

    /// Mark an identity as Active (admin is just an Active member here).
    pub fn set_active(&mut self, identity: impl Into<IdentityKey>) {
        self.active_members.insert(identity.into());
    }

    /// Add a hash referenced by a valid `file.shared` (per-hash allowlist).
    pub fn add_referenced_hash(&mut self, hash: Hash) {
        self.referenced_hashes.insert(hash);
    }

    /// Resolve a connecting device to its bound identity, if any.
    #[must_use]
    pub fn identity_of(&self, device: EndpointId) -> Option<&IdentityKey> {
        self.device_to_identity.get(&device)
    }

    /// Gate 1 predicate: is the connecting device bound to an Active identity?
    ///
    /// Fail-closed: an unbound device (no known identity) is **not** active.
    #[must_use]
    pub fn is_active(&self, device: EndpointId) -> bool {
        self.identity_of(device)
            .is_some_and(|id| self.active_members.contains(id))
    }

    /// Gate 2 predicate: is this hash referenced by a valid `file.shared`?
    #[must_use]
    pub fn is_referenced(&self, hash: &Hash) -> bool {
        self.referenced_hashes.contains(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::AuthContext;
    use iroh::{EndpointId, SecretKey};
    use iroh_blobs::Hash;

    /// Deterministic `EndpointId` from a one-byte seed (test-only).
    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn fixture() -> (AuthContext, EndpointId, EndpointId, EndpointId, Hash) {
        // Mirror Test Vector §16: Alice (admin)/Bob/Carol Active, Dave Removed,
        // Mallory absent.
        let alice = endpoint_id(1);
        let carol = endpoint_id(3);
        let dave = endpoint_id(4);
        let referenced = Hash::from([0xAB; 32]);

        let mut auth = AuthContext::new();
        auth.bind_device(alice, "alice");
        auth.bind_device(carol, "carol");
        auth.bind_device(dave, "dave");
        auth.set_active("alice");
        auth.set_active("carol");
        // Dave is bound but NOT active (Removed).
        auth.add_referenced_hash(referenced);
        (auth, alice, carol, dave, referenced)
    }

    #[test]
    fn active_member_passes_connect_gate() {
        let (auth, _alice, carol, _dave, _h) = fixture();
        assert!(auth.is_active(carol));
    }

    #[test]
    fn removed_member_fails_connect_gate() {
        let (auth, _alice, _carol, dave, _h) = fixture();
        assert!(!auth.is_active(dave));
    }

    #[test]
    fn unknown_device_fails_connect_gate() {
        let (auth, ..) = fixture();
        let mallory = endpoint_id(5); // never bound
        assert!(!auth.is_active(mallory));
    }

    #[test]
    fn referenced_hash_passes_per_hash_gate() {
        let (auth, .., referenced) = fixture();
        assert!(auth.is_referenced(&referenced));
    }

    #[test]
    fn unreferenced_hash_fails_per_hash_gate() {
        let (auth, ..) = fixture();
        let other = Hash::from([0xCD; 32]);
        assert!(!auth.is_referenced(&other));
    }

    #[test]
    fn empty_context_is_fail_closed() {
        let auth = AuthContext::new();
        let device = endpoint_id(42);
        let hash = Hash::from([0u8; 32]);
        assert!(
            !auth.is_active(device),
            "empty context must deny all connects"
        );
        assert!(
            !auth.is_referenced(&hash),
            "empty context must deny all hashes"
        );
    }

    #[test]
    fn identity_of_returns_bound_identity() {
        let (auth, alice, ..) = fixture();
        assert_eq!(auth.identity_of(alice), Some(&"alice".to_string()));
    }

    #[test]
    fn identity_of_unknown_device_returns_none() {
        let (auth, ..) = fixture();
        let stranger = endpoint_id(99);
        assert!(auth.identity_of(stranger).is_none());
    }

    #[test]
    fn bound_but_inactive_identity_fails_connect_gate() {
        // Device has a device->identity binding but the identity is never set_active.
        let mut auth = AuthContext::new();
        let bob = endpoint_id(2);
        auth.bind_device(bob, "bob");
        assert!(!auth.is_active(bob));
    }

    #[test]
    fn multiple_referenced_hashes_all_pass() {
        let mut auth = AuthContext::new();
        let h1 = Hash::from([0x11; 32]);
        let h2 = Hash::from([0x22; 32]);
        auth.add_referenced_hash(h1);
        auth.add_referenced_hash(h2);
        assert!(auth.is_referenced(&h1));
        assert!(auth.is_referenced(&h2));
        let h3 = Hash::from([0x33; 32]);
        assert!(!auth.is_referenced(&h3));
    }

    #[test]
    fn adding_same_hash_twice_is_idempotent() {
        let mut auth = AuthContext::new();
        let h = Hash::from([0xAA; 32]);
        auth.add_referenced_hash(h);
        auth.add_referenced_hash(h);
        assert!(auth.is_referenced(&h));
    }

    // --- Device-rebinding security tests ---

    #[test]
    fn device_rebinding_to_active_identity_makes_device_active() {
        let mut auth = AuthContext::new();
        let device = endpoint_id(10);
        // Bind to an inactive identity first.
        auth.bind_device(device, "inactive_user");
        assert!(!auth.is_active(device));
        // Rebind the same device to an active identity.
        auth.bind_device(device, "alice");
        auth.set_active("alice");
        assert!(
            auth.is_active(device),
            "device must be active after rebind to active identity"
        );
    }

    #[test]
    fn device_rebinding_away_from_active_identity_revokes_access() {
        let mut auth = AuthContext::new();
        let device = endpoint_id(11);
        auth.bind_device(device, "alice");
        auth.set_active("alice");
        assert!(auth.is_active(device), "precondition: device starts active");
        // Rebind the device to an identity that is not active — access must be revoked.
        auth.bind_device(device, "dave_removed");
        assert!(
            !auth.is_active(device),
            "device must no longer be active after rebind to non-active identity"
        );
    }

    #[test]
    fn set_active_without_device_binding_grants_no_access() {
        // An identity can be in the active set without any device bound to it; in
        // that case no device can authenticate as that identity.
        let mut auth = AuthContext::new();
        let device = endpoint_id(12);
        auth.set_active("orphan_identity"); // active identity, but no device binding
        assert!(
            !auth.is_active(device),
            "unbound device must not be active even if the identity is in the active set"
        );
        assert!(auth.identity_of(device).is_none());
    }

    #[test]
    fn multiple_devices_bound_to_same_active_identity_are_all_active() {
        // Two different device keys (e.g., phone + laptop) can share an identity.
        let mut auth = AuthContext::new();
        let device_a = endpoint_id(20);
        let device_b = endpoint_id(21);
        auth.bind_device(device_a, "carol");
        auth.bind_device(device_b, "carol"); // second device, same identity
        auth.set_active("carol");
        assert!(auth.is_active(device_a), "first device must be active");
        assert!(
            auth.is_active(device_b),
            "second device must also be active"
        );
    }

    #[test]
    fn clone_produces_independent_copy() {
        let (auth, _alice, carol, _dave, _h) = fixture();
        let mut cloned = auth.clone();
        // Mutations on the clone must not affect the original.
        let stranger = endpoint_id(99);
        cloned.bind_device(stranger, "stranger");
        cloned.set_active("stranger");
        assert!(cloned.is_active(stranger));
        assert!(
            !auth.is_active(stranger),
            "original must not see mutations applied to the clone"
        );
        // Original state must be preserved.
        assert!(
            auth.is_active(carol),
            "original active members must be unchanged"
        );
    }
}
