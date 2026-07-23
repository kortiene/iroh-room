//! `GenesisConfig`, genesis signatures, and non-recursive `CommunityId`
//! derivation (spec §7.2, D3, issue #147).
//!
//! `CommunityId` is derived from the canonical-CBOR bytes of a
//! `GenesisConfig` body that **does not** contain `community_id` (or any
//! record that itself contains it), avoiding the recursive derivation trap.
//! Genesis signatures are verified over `domain::COMMUNITY || genesis_csb`;
//! bootstrap threshold verification is in scope for #147 (ordinary operation
//! authorization is #148).

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::ids::{CommunityId, PrincipalId};
use crate::keys::{verify, Signature, SigningKey};

use super::model::{CommunityPolicy, RecoveryConfig, ReplicaDescriptor};

/// The genesis schema version this module accepts (spec §7.2 / D8).
pub const GENESIS_SCHEMA_VERSION: u64 = 2;

/// A genesis configuration body (spec §5.1). This is the preimage of
/// [`CommunityId`]; it MUST NOT contain `community_id`, governance entry ids,
/// approvals, or state roots (spec D3: non-recursive derivation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisConfig {
    /// Schema version (MUST be `2`).
    pub schema_version: u64,
    /// Signed creation time (advisory; never a wall clock — spec §11).
    pub created_at_ms: u64,
    /// Genesis nonce (deterministic per-community entropy).
    pub genesis_nonce: [u8; 32],
    /// The signature threshold required to authorize the genesis.
    pub admin_threshold: u16,
    /// The initial administrator principal set (canonicalized to sorted unique).
    pub administrators: Vec<PrincipalId>,
    /// The initial recovery configuration.
    pub recovery: RecoveryConfig,
    /// The initial replica descriptors (canonicalized to sorted unique by id).
    pub replicas: Vec<ReplicaDescriptor>,
    /// The initial community policy.
    pub community_policy: CommunityPolicy,
}

impl GenesisConfig {
    /// Canonical-CBOR encode this genesis body (the `GenesisConfigBody` of D3).
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "schema_version".to_owned(),
                CborValue::Uint(self.schema_version),
            ),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
            (
                "genesis_nonce".to_owned(),
                CborValue::Bytes(self.genesis_nonce.to_vec()),
            ),
            (
                "admin_threshold".to_owned(),
                CborValue::Uint(u64::from(self.admin_threshold)),
            ),
            (
                "administrators".to_owned(),
                CborValue::Array(
                    self.administrators
                        .iter()
                        .map(|p| CborValue::Bytes(p.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            ("recovery".to_owned(), self.recovery.to_cbor()),
            (
                "replicas".to_owned(),
                CborValue::Array(
                    self.replicas
                        .iter()
                        .map(ReplicaDescriptor::to_cbor)
                        .collect(),
                ),
            ),
            (
                "community_policy".to_owned(),
                self.community_policy.to_cbor(),
            ),
        ])
    }

    /// Decode a genesis body from canonical CBOR, enforcing the closed schema
    /// (spec D8: unknown keys MUST be rejected).
    ///
    /// # Errors
    /// Returns [`Reject::NonCanonicalEncoding`] for a malformed body or unknown
    /// key, and [`Reject::UnknownVersion`] if `schema_version != 2`.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        super::reject_unknown_keys(
            entries,
            &[
                "schema_version",
                "created_at_ms",
                "genesis_nonce",
                "admin_threshold",
                "administrators",
                "recovery",
                "replicas",
                "community_policy",
            ],
            Reject::NonCanonicalEncoding,
        )?;
        let schema_version = super::read_uint_field(entries, "schema_version")?;
        if schema_version != GENESIS_SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let created_at_ms = super::read_uint_field(entries, "created_at_ms")?;
        let genesis_nonce = super::read_fixed32_field(entries, "genesis_nonce")?;
        let admin_threshold = super::read_u16_field(entries, "admin_threshold")?;
        let mut administrators = super::read_principal_array(entries, "administrators")?;
        let recovery_val =
            super::opt_field(entries, "recovery").ok_or(Reject::NonCanonicalEncoding)?;
        let recovery = RecoveryConfig::from_canonical(recovery_val)?;
        let replicas_val =
            super::opt_field(entries, "replicas").ok_or(Reject::NonCanonicalEncoding)?;
        let replicas = super::read_replica_array(replicas_val)?;
        let policy_val =
            super::opt_field(entries, "community_policy").ok_or(Reject::NonCanonicalEncoding)?;
        let community_policy = CommunityPolicy::from_canonical(policy_val)?;
        // Canonicalize administrator + replica ordering before signing/deriving.
        administrators.sort();
        administrators.dedup();
        Ok(Self {
            schema_version,
            created_at_ms,
            genesis_nonce,
            admin_threshold,
            administrators,
            recovery,
            replicas,
            community_policy,
        })
    }

    /// Validate the genesis invariants (spec §5.1).
    ///
    /// # Errors
    /// Returns [`Reject::UnknownVersion`] if `schema_version != 2`, or
    /// [`Reject::InvalidContent`] if the administrator/replica sets or threshold
    /// violate the canonicalization rules.
    pub fn validate(&self) -> Result<(), Reject> {
        if self.schema_version != GENESIS_SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        if self.administrators.is_empty() {
            return Err(Reject::InvalidContent);
        }
        // Administrators must be sorted ascending + unique.
        let mut sorted = self.administrators.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.administrators.len() {
            return Err(Reject::InvalidContent);
        }
        if sorted != self.administrators {
            return Err(Reject::InvalidContent);
        }
        // Threshold range: 1..=unique admins (spec §5.1).
        if self.admin_threshold == 0 {
            return Err(Reject::InvalidContent);
        }
        let admin_count =
            u16::try_from(self.administrators.len()).map_err(|_| Reject::InvalidContent)?;
        if self.admin_threshold > admin_count {
            return Err(Reject::InvalidContent);
        }
        // Replicas must be sorted by ReplicaId + unique (spec §5.1).
        let mut sorted_replicas = self.replicas.clone();
        sorted_replicas.sort_by_key(|r| *r.replica_id.as_bytes());
        let mut deduped = sorted_replicas.clone();
        deduped.dedup_by_key(|r| *r.replica_id.as_bytes());
        if deduped.len() != self.replicas.len() {
            return Err(Reject::InvalidContent); // duplicate replica id
        }
        if sorted_replicas != self.replicas {
            return Err(Reject::InvalidContent); // not sorted by ReplicaId
        }
        Ok(())
    }
}

/// The canonical signed bytes (CSB) of a genesis config.
#[must_use]
pub fn genesis_config_csb(config: &GenesisConfig) -> Vec<u8> {
    crate::cbor::encode(&config.to_cbor())
}

/// Derive the [`CommunityId`] from a genesis config (spec D3).
///
/// `community_id = BLAKE3(domain::COMMUNITY || canonical_cbor(GenesisConfig))`.
/// The preimage never includes `community_id` itself (non-recursive).
#[must_use]
pub fn derive_community_id(config: &GenesisConfig) -> CommunityId {
    let csb = genesis_config_csb(config);
    CommunityId::from_bytes(domain::blake3_domain(domain::COMMUNITY, &csb))
}

/// A genesis signature over `domain::COMMUNITY || genesis_config_csb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisSignature {
    /// The signing principal (MUST be one of `config.administrators`).
    pub signer: PrincipalId,
    /// The detached Ed25519 signature.
    pub signature: Signature,
}

/// Sign a genesis config with a secret key, producing a [`GenesisSignature`]
/// over `domain::COMMUNITY || genesis_config_csb`.
#[must_use]
pub fn sign_genesis(config: &GenesisConfig, secret: &SigningKey) -> GenesisSignature {
    let csb = genesis_config_csb(config);
    let msg = domain::signing_message(domain::COMMUNITY, &csb);
    GenesisSignature {
        signer: secret.member_id(),
        signature: secret.sign(&msg),
    }
}

/// Verify genesis signatures against the declared admin threshold and return
/// the derived [`CommunityId`] (spec §5.1 / D4).
///
/// # Errors
/// - [`Reject::InvalidContent`] — genesis config invariants violated.
/// - [`Reject::InvalidApproval`] — a signer is not an administrator, or the
///   same administrator signed twice.
/// - [`Reject::BadSignature`] — an Ed25519 signature does not verify.
/// - [`Reject::InsufficientAuthorization`] — fewer than `admin_threshold`
///   unique valid admin signatures were supplied.
pub fn verify_genesis(
    config: &GenesisConfig,
    signatures: &[GenesisSignature],
) -> Result<CommunityId, Reject> {
    config.validate()?;
    let csb = genesis_config_csb(config);
    let community_id = derive_community_id(config);
    let msg = domain::signing_message(domain::COMMUNITY, &csb);

    let mut seen = std::collections::BTreeSet::new();
    let mut valid_count: u16 = 0;
    for sig in signatures {
        if !config.administrators.contains(&sig.signer) {
            return Err(Reject::InvalidApproval);
        }
        if !seen.insert(sig.signer) {
            // Duplicate approver: reject, do not double-count (spec D6 / §9).
            return Err(Reject::InvalidApproval);
        }
        verify(&sig.signer, &msg, &sig.signature).map_err(|_| Reject::BadSignature)?;
        valid_count = valid_count.checked_add(1).ok_or(Reject::InvalidApproval)?;
    }
    if valid_count < config.admin_threshold {
        return Err(Reject::InsufficientAuthorization);
    }
    Ok(community_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ReplicaId, LEN};

    fn admin_key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; LEN])
    }

    fn minimal_config() -> GenesisConfig {
        GenesisConfig {
            schema_version: GENESIS_SCHEMA_VERSION,
            created_at_ms: 1_000,
            genesis_nonce: [0xab; LEN],
            admin_threshold: 1,
            administrators: vec![admin_key(0xa0).member_id()],
            recovery: RecoveryConfig::empty(),
            replicas: Vec::new(),
            community_policy: CommunityPolicy::empty(),
        }
    }

    #[test]
    fn genesis_threshold_met_verifies() {
        let cfg = minimal_config();
        let admin = admin_key(0xa0);
        let sig = sign_genesis(&cfg, &admin);
        let cid = verify_genesis(&cfg, std::slice::from_ref(&sig)).expect("threshold met");
        assert_eq!(cid, derive_community_id(&cfg));
    }

    #[test]
    fn genesis_threshold_not_met_rejected() {
        let cfg = minimal_config();
        // No signatures supplied → below threshold 1.
        assert_eq!(
            verify_genesis(&cfg, &[]).err(),
            Some(Reject::InsufficientAuthorization)
        );
    }

    #[test]
    fn genesis_duplicate_admin_signature_rejected() {
        let cfg = minimal_config();
        let admin = admin_key(0xa0);
        let sig = sign_genesis(&cfg, &admin);
        // Supplying the same admin signature twice must reject (not double-count).
        assert_eq!(
            verify_genesis(&cfg, &[sig.clone(), sig]).err(),
            Some(Reject::InvalidApproval)
        );
    }

    #[test]
    fn genesis_non_admin_signature_rejected() {
        let cfg = minimal_config();
        let stranger = admin_key(0xee);
        let sig = sign_genesis(&cfg, &stranger);
        assert_eq!(
            verify_genesis(&cfg, std::slice::from_ref(&sig)).err(),
            Some(Reject::InvalidApproval)
        );
    }

    #[test]
    fn genesis_bad_signature_rejected() {
        let cfg = minimal_config();
        let admin = admin_key(0xa0);
        let other = admin_key(0xa1);
        // Sign with `other` but claim `admin` as the signer.
        let real = sign_genesis(&cfg, &other);
        let forged = GenesisSignature {
            signer: admin.member_id(),
            signature: real.signature,
        };
        assert_eq!(
            verify_genesis(&cfg, std::slice::from_ref(&forged)).err(),
            Some(Reject::BadSignature)
        );
    }

    #[test]
    fn community_id_does_not_include_community_id_in_preimage() {
        // The genesis CSB preimage must not carry the derived community_id.
        let cfg = minimal_config();
        let csb = genesis_config_csb(&cfg);
        let value = crate::cbor::decode_canonical(&csb).unwrap();
        let entries = value.as_map().unwrap();
        assert!(
            !entries.iter().any(|(k, _)| k == "community_id"),
            "genesis preimage must be non-recursive (spec D3)"
        );
    }

    #[test]
    fn genesis_round_trips_through_canonical_cbor() {
        let mut cfg = minimal_config();
        cfg.replicas = vec![ReplicaDescriptor {
            replica_id: ReplicaId::from_bytes([0x11; LEN]),
            endpoint: vec![0xfe],
            capability: 2,
        }];
        let csb = genesis_config_csb(&cfg);
        let value = crate::cbor::decode_canonical(&csb).unwrap();
        let back = GenesisConfig::from_canonical(&value).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn genesis_rejects_unsorted_administrators() {
        let a = admin_key(0x01).member_id();
        let b = admin_key(0x02).member_id();
        // Build a guaranteed-descending order regardless of how the seed maps
        // to public-key bytes (ed25519 output is not seed-ordered).
        let mut ascending = vec![a, b];
        ascending.sort();
        let descending: Vec<PrincipalId> = ascending.iter().rev().copied().collect();
        // Guard against the trivial single-admin case (seeds are distinct).
        assert_eq!(descending.len(), 2);
        assert_ne!(descending, ascending, "test setup: vectors must differ");
        let mut cfg = minimal_config();
        cfg.administrators = descending;
        cfg.admin_threshold = 1;
        assert_eq!(cfg.validate().err(), Some(Reject::InvalidContent));
    }
}
