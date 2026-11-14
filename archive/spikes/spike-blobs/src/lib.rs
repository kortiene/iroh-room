//! `spike-blobs` (IR-0009) — a throwaway spike that confirms the Blob Plane can
//! enforce **room membership** (per-node) and **per-hash** authorization for
//! shared artifacts over real `iroh-blobs`.
//!
//! See `specs/prototype-blob-acl-path.md` for scope and `NOTES.md` (next to this
//! crate) for the findings (version decision + real ACL API observations).
//!
//! The library exposes the pieces the binary and the integration tests share:
//!
//! - [`acl`] — the fold-shaped [`acl::AuthContext`] authorization fixture.
//! - [`file_shared`] — the minimal `file.shared` reference (create/consume).
//! - [`net`] — the gated provider, the fetcher, and the [`net::FetchOutcome`]
//!   classification driving the §5.5 decision matrix.
//! - [`roster`] — deterministic Test Vector §16 identities.

pub mod acl;
pub mod file_shared;
pub mod net;

/// Deterministic identities and the Test Vector §16 authorization snapshot.
///
/// Roster (`PHASE-0-SPIKE.md` Test Vector §16): `{Alice: Active(admin), Bob:
/// Active, Carol: Active, Dave: Removed}`, with `Mallory` unknown. Each member's
/// `SecretKey` is derived from a fixed one-byte seed so the spike is reproducible
/// without persisting keys.
pub mod roster {
    use iroh::{EndpointId, SecretKey};
    use iroh_blobs::Hash;

    use crate::acl::AuthContext;

    /// Seed for Alice (Active admin).
    pub const ALICE: u8 = 1;
    /// Seed for Bob (Active; the `file.shared` author and blob provider).
    pub const BOB: u8 = 2;
    /// Seed for Carol (Active).
    pub const CAROL: u8 = 3;
    /// Seed for Dave (Removed — bound but not Active).
    pub const DAVE: u8 = 4;
    /// Seed for Mallory (unknown — never bound).
    pub const MALLORY: u8 = 5;

    /// The deterministic `SecretKey` for a roster seed.
    #[must_use]
    pub fn secret(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; 32])
    }

    /// The `EndpointId` (== `device_id`) for a roster seed.
    #[must_use]
    pub fn id(seed: u8) -> EndpointId {
        secret(seed).public()
    }

    /// Build the Test Vector §16 authorization snapshot, with `referenced` as the
    /// set of hashes carried by valid `file.shared` payloads.
    ///
    /// Alice/Bob/Carol are Active; Dave is bound but Removed; Mallory is absent.
    #[must_use]
    pub fn test_vector_auth(referenced: &[Hash]) -> AuthContext {
        let mut auth = AuthContext::new();
        auth.bind_device(id(ALICE), "alice");
        auth.bind_device(id(BOB), "bob");
        auth.bind_device(id(CAROL), "carol");
        auth.bind_device(id(DAVE), "dave"); // bound, but never set Active
        auth.set_active("alice");
        auth.set_active("bob");
        auth.set_active("carol");
        for hash in referenced {
            auth.add_referenced_hash(*hash);
        }
        auth
    }

    #[cfg(test)]
    mod tests {
        use super::{id, secret, test_vector_auth, ALICE, BOB, CAROL, DAVE, MALLORY};
        use iroh_blobs::Hash;

        #[test]
        fn roster_seeds_produce_distinct_ids() {
            let seeds = [ALICE, BOB, CAROL, DAVE, MALLORY];
            let ids: Vec<_> = seeds.iter().map(|&s| id(s)).collect();
            for i in 0..ids.len() {
                for j in (i + 1)..ids.len() {
                    assert_ne!(ids[i], ids[j], "seeds {i} and {j} produced the same id");
                }
            }
        }

        #[test]
        fn test_vector_auth_alice_bob_carol_are_active() {
            let auth = test_vector_auth(&[]);
            assert!(auth.is_active(id(ALICE)), "Alice (admin) must be Active");
            assert!(auth.is_active(id(BOB)), "Bob must be Active");
            assert!(auth.is_active(id(CAROL)), "Carol must be Active");
        }

        #[test]
        fn test_vector_auth_dave_bound_but_not_active() {
            let auth = test_vector_auth(&[]);
            assert!(
                !auth.is_active(id(DAVE)),
                "Dave (Removed) must not be Active"
            );
            // Dave has a device binding even though he is not Active.
            assert!(
                auth.identity_of(id(DAVE)).is_some(),
                "Dave must still have a device binding"
            );
        }

        #[test]
        fn test_vector_auth_mallory_has_no_binding() {
            let auth = test_vector_auth(&[]);
            assert!(!auth.is_active(id(MALLORY)), "Mallory must not be Active");
            assert!(
                auth.identity_of(id(MALLORY)).is_none(),
                "Mallory must have no device binding"
            );
        }

        #[test]
        fn test_vector_auth_referenced_hashes_populated() {
            let h1 = Hash::from([0x01u8; 32]);
            let h2 = Hash::from([0x02u8; 32]);
            let auth = test_vector_auth(&[h1, h2]);
            assert!(auth.is_referenced(&h1));
            assert!(auth.is_referenced(&h2));
            let h3 = Hash::from([0x03u8; 32]);
            assert!(!auth.is_referenced(&h3));
        }

        // --- Determinism and isolation tests ---

        #[test]
        fn secret_is_deterministic() {
            // Calling secret() twice with the same seed must yield the same key pair
            // (verified via the derived public identity, since SecretKey doesn't implement Eq).
            assert_eq!(
                secret(ALICE).public(),
                secret(ALICE).public(),
                "same seed must always produce the same key"
            );
        }

        #[test]
        fn id_matches_secret_public_for_all_seeds() {
            for seed in [ALICE, BOB, CAROL, DAVE, MALLORY] {
                assert_eq!(
                    id(seed),
                    secret(seed).public(),
                    "id({seed}) must equal secret({seed}).public()"
                );
            }
        }

        #[test]
        fn test_vector_auth_single_hash_not_contaminated_by_other_hashes() {
            // Only the hash passed to test_vector_auth is referenced; an arbitrary
            // second hash must not appear in the allowlist (hash isolation).
            let h1 = Hash::from([0x11u8; 32]);
            let h2 = Hash::from([0x22u8; 32]);
            let auth = test_vector_auth(&[h1]);
            assert!(auth.is_referenced(&h1), "passed hash must be referenced");
            assert!(
                !auth.is_referenced(&h2),
                "hash not passed to test_vector_auth must NOT be referenced (hash isolation)"
            );
        }
    }
}
