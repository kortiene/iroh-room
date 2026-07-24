//! The canonical governance-log records: `GovernanceEntryBody`,
//! `GovernanceApprovalBody`, `GovernanceApproval`, and `GovernanceEntry`
//! (spec §5.2–§5.4, issue #147).
//!
//! Every record canonicalizes to closed deterministic CBOR and is signed under
//! the frozen #146 domains (`domain::GOVERNANCE_ENTRY`,
//! `domain::GOVERNANCE_APPROVAL`). The verification pipeline:
//!
//! 1. canonical decode of the exact received bytes;
//! 2. closed-schema validation (unknown keys → reject);
//! 3. `kind`/payload agreement + unknown-operation rejection;
//! 4. id recomputation + signature verification;
//! 5. approval sorting, duplicate-approver rejection, signature + binding
//!    verification.

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::ids::StateRoot;
use crate::ids::{CommunityId, GovernanceId, LEN};
use crate::keys::{verify, Signature};
use crate::PrincipalId;

use super::operation::GovernanceOperationPayload;
use super::GENESIS_SCHEMA_VERSION;

/// The canonical governance-log entry body (spec §5.2).
///
/// This is the post-genesis totally-ordered log record. `state_root` commits
/// to the state *after* applying `payload` to the previous state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceEntryBody {
    /// The community this entry belongs to.
    pub community_id: CommunityId,
    /// The 1-based entry sequence (`seq == 1` is the first post-genesis entry).
    pub seq: u64,
    /// The previous entry id (`None` only when `seq == 1`).
    pub prev: Option<GovernanceId>,
    /// Signed creation time (advisory; never a wall clock).
    pub created_at_ms: u64,
    /// The operation kind discriminant.
    pub kind: super::operation::GovernanceOperationKind,
    /// The typed operation payload (must agree with `kind`).
    pub payload: GovernanceOperationPayload,
    /// The state root after applying this operation.
    pub state_root: StateRoot,
}

impl GovernanceEntryBody {
    /// Canonical-CBOR encode this body.
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        let mut entries = vec![
            (
                "community_id".to_owned(),
                CborValue::Bytes(self.community_id.as_bytes().to_vec()),
            ),
            ("seq".to_owned(), CborValue::Uint(self.seq)),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
            (
                "kind".to_owned(),
                CborValue::Text(self.kind.as_str().to_owned()),
            ),
            ("payload".to_owned(), self.payload.to_cbor()),
            (
                "state_root".to_owned(),
                CborValue::Bytes(self.state_root.as_bytes().to_vec()),
            ),
        ];
        if let Some(prev) = self.prev {
            entries.push((
                "prev".to_owned(),
                CborValue::Bytes(prev.as_bytes().to_vec()),
            ));
        }
        CborValue::Map(entries)
    }

    /// Decode + strictly validate a canonically-decoded body (spec §5.4 step 2).
    ///
    /// # Errors
    /// - [`Reject::NonCanonicalEncoding`] — body is not a map or a known field
    ///   has the wrong shape.
    /// - [`Reject::UnknownRecordKind`] — the `kind` is outside the closed §7.3
    ///   registry (spec §7.3: unknown operations are rejected, not ignored).
    /// - [`Reject::InvalidContent`] — the `kind`/payload shapes disagree.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        super::reject_unknown_keys(
            entries,
            &[
                "community_id",
                "seq",
                "created_at_ms",
                "kind",
                "payload",
                "state_root",
                "prev",
            ],
            Reject::NonCanonicalEncoding,
        )?;
        let community_id = super::read_community_field(entries, "community_id")?;
        let seq = super::read_uint_field(entries, "seq")?;
        if seq == 0 {
            return Err(Reject::InvalidContent);
        }
        let created_at_ms = super::read_uint_field(entries, "created_at_ms")?;
        let kind_str = super::read_text_field(entries, "kind")?;
        let kind = super::operation::GovernanceOperationKind::parse(kind_str)?;
        let payload_val =
            super::opt_field(entries, "payload").ok_or(Reject::NonCanonicalEncoding)?;
        let payload = GovernanceOperationPayload::from_canonical(kind, payload_val)?;
        let state_root = super::read_state_root_field(entries, "state_root")?;
        let prev = match super::opt_field(entries, "prev") {
            Some(v) => {
                let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
                Some(GovernanceId::from_bytes(arr))
            }
            None => None,
        };
        // Chain invariant: prev is None iff seq == 1 (spec D5).
        if (seq == 1) != prev.is_none() {
            return Err(Reject::InvalidContent);
        }
        // The payload's own kind must agree with the declared kind.
        if payload.kind() != kind {
            return Err(Reject::InvalidContent);
        }
        Ok(Self {
            community_id,
            seq,
            prev,
            created_at_ms,
            kind,
            payload,
            state_root,
        })
    }
}

/// The canonical signed bytes (CSB) of an entry body.
#[must_use]
pub fn entry_csb(body: &GovernanceEntryBody) -> Vec<u8> {
    crate::cbor::encode(&body.to_cbor())
}

/// Derive the [`GovernanceId`] of an entry body (spec D5):
/// `BLAKE3(domain::GOVERNANCE_ENTRY || entry_csb)`.
#[must_use]
pub fn entry_id(body: &GovernanceEntryBody) -> GovernanceId {
    GovernanceId::from_governance_entry_csb(&entry_csb(body))
}

/// Decode + strictly validate an entry body from its canonical bytes.
///
/// # Errors
/// See [`GovernanceEntryBody::from_canonical`] and [`crate::cbor::decode_canonical`].
pub fn decode_entry_csb(csb: &[u8]) -> Result<GovernanceEntryBody, Reject> {
    let value = crate::cbor::decode_canonical(csb)?;
    GovernanceEntryBody::from_canonical(&value)
}

// ----------------------------------------------------------------------------
// GovernanceApproval
// ----------------------------------------------------------------------------

/// The governance approval body (spec §5.3). An approver attests to a specific
/// entry's resulting `state_root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceApprovalBody {
    /// The community this approval belongs to.
    pub community_id: CommunityId,
    /// The entry being approved.
    pub entry_id: GovernanceId,
    /// The state root the approver attests to.
    pub state_root: StateRoot,
    /// The approving principal.
    pub approver: PrincipalId,
    /// Signed creation time (advisory).
    pub created_at_ms: u64,
}

impl GovernanceApprovalBody {
    /// Canonical-CBOR encode this body.
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "community_id".to_owned(),
                CborValue::Bytes(self.community_id.as_bytes().to_vec()),
            ),
            (
                "entry_id".to_owned(),
                CborValue::Bytes(self.entry_id.as_bytes().to_vec()),
            ),
            (
                "state_root".to_owned(),
                CborValue::Bytes(self.state_root.as_bytes().to_vec()),
            ),
            (
                "approver".to_owned(),
                CborValue::Bytes(self.approver.as_bytes().to_vec()),
            ),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
        ])
    }

    /// Decode + strictly validate an approval body (spec §5.4).
    ///
    /// # Errors
    /// Returns [`Reject::NonCanonicalEncoding`] if the value is not a
    /// closed-schema map or a field has the wrong shape/width.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        super::reject_unknown_keys(
            entries,
            &[
                "community_id",
                "entry_id",
                "state_root",
                "approver",
                "created_at_ms",
            ],
            Reject::NonCanonicalEncoding,
        )?;
        let community_id = super::read_community_field(entries, "community_id")?;
        let entry_id = super::read_governance_field(entries, "entry_id")?;
        let state_root = super::read_state_root_field(entries, "state_root")?;
        let approver = super::read_principal_field(entries, "approver")?;
        let created_at_ms = super::read_uint_field(entries, "created_at_ms")?;
        Ok(Self {
            community_id,
            entry_id,
            state_root,
            approver,
            created_at_ms,
        })
    }
}

/// The canonical signed bytes of an approval body.
#[must_use]
pub fn approval_csb(body: &GovernanceApprovalBody) -> Vec<u8> {
    crate::cbor::encode(&body.to_cbor())
}

/// Derive the approval id (spec §5.3):
/// `BLAKE3(domain::GOVERNANCE_APPROVAL || approval_csb)`.
#[must_use]
pub fn approval_id(body: &GovernanceApprovalBody) -> [u8; LEN] {
    domain::blake3_domain(domain::GOVERNANCE_APPROVAL, &approval_csb(body))
}

/// A signed governance approval (spec §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceApproval {
    /// The approval body.
    pub body: GovernanceApprovalBody,
    /// The detached Ed25519 signature over `domain::GOVERNANCE_APPROVAL ||
    /// approval_csb`.
    pub signature: Signature,
}

impl GovernanceApproval {
    /// Construct a signed approval (spec D2).
    #[must_use]
    pub fn new(body: GovernanceApprovalBody, secret: &crate::keys::SigningKey) -> Self {
        let msg = domain::signing_message(domain::GOVERNANCE_APPROVAL, &approval_csb(&body));
        Self {
            body,
            signature: secret.sign(&msg),
        }
    }
}

/// A signed governance-log entry with its approvals (spec §5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceEntry {
    /// The entry body.
    pub body: GovernanceEntryBody,
    /// The signing principal.
    pub signer: PrincipalId,
    /// The detached Ed25519 signature over `domain::GOVERNANCE_ENTRY ||
    /// entry_csb`.
    pub signature: Signature,
    /// Approvals collected for this entry (verified + canonically sorted by
    /// [`verify_entry_crypto`]).
    pub approvals: Vec<GovernanceApproval>,
}

impl GovernanceEntry {
    /// Construct a signed entry (approvals supplied separately).
    #[must_use]
    pub fn new(
        body: GovernanceEntryBody,
        secret: &crate::keys::SigningKey,
        approvals: Vec<GovernanceApproval>,
    ) -> Self {
        let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &entry_csb(&body));
        Self {
            body,
            signer: secret.member_id(),
            signature: secret.sign(&msg),
            approvals,
        }
    }
}

/// Verify an entry's body closed-schema + signature (spec §5.4 step 4).
///
/// Returns the validated body. Approval verification is separate
/// ([`verify_entry_crypto`]).
///
/// # Errors
/// - [`Reject::NonCanonicalEncoding`] — malformed body.
/// - [`Reject::UnknownRecordKind`] — unknown operation kind.
/// - [`Reject::BadSignature`] — signature does not verify under `signer`.
pub fn verify_entry_crypto(entry: &GovernanceEntry) -> Result<GovernanceEntryBody, Reject> {
    let body = decode_entry_csb(&entry_csb(&entry.body))?;
    // The decoded body must be byte-identical to the in-memory body (defensive:
    // a caller may have hand-constructed an inconsistent body).
    if body != entry.body {
        return Err(Reject::NonCanonicalEncoding);
    }
    let csb = entry_csb(&entry.body);
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &csb);
    verify(&entry.signer, &msg, &entry.signature).map_err(|_| Reject::BadSignature)?;
    Ok(body)
}

/// Verify an approval's body + signature (spec §5.3 / §5.4 step 6).
///
/// # Errors
/// - [`Reject::NonCanonicalEncoding`] — malformed body.
/// - [`Reject::BadSignature`] — signature does not verify under `approver`.
pub fn verify_approval_crypto(
    approval: &GovernanceApproval,
) -> Result<GovernanceApprovalBody, Reject> {
    let value = crate::cbor::decode_canonical(&approval_csb(&approval.body))?;
    let body = GovernanceApprovalBody::from_canonical(&value)?;
    if body != approval.body {
        return Err(Reject::NonCanonicalEncoding);
    }
    // The signer must be the claimed approver (spec §5.3).
    let msg = domain::signing_message(domain::GOVERNANCE_APPROVAL, &approval_csb(&body));
    verify(&body.approver, &msg, &approval.signature).map_err(|_| Reject::BadSignature)?;
    Ok(body)
}

/// Verify a full entry: body crypto, entry signature, approval sorting,
/// duplicate-approver rejection, approval signatures, and approval bindings
/// (spec §5.4 pipeline).
///
/// # Errors
/// - Any error from [`verify_entry_crypto`].
/// - [`Reject::InvalidApproval`] — duplicate approver, an approval signature
///   fails, or an approval is not bound to the entry's `community_id`,
///   `entry_id`, or declared `state_root`.
pub fn verify_entry_full(entry: &GovernanceEntry) -> Result<GovernanceEntryBody, Reject> {
    let body = verify_entry_crypto(entry)?;
    let expected_id = entry_id(&entry.body);

    // Sort approvals canonically by (approver bytes, approval_id bytes) so the
    // canonical bytes do not depend on caller order (spec D6).
    let mut approvals = entry.approvals.clone();
    approvals.sort_by(|a, b| {
        (a.body.approver.as_bytes(), approval_id(&a.body).as_slice())
            .cmp(&(b.body.approver.as_bytes(), approval_id(&b.body).as_slice()))
    });

    let mut seen = std::collections::BTreeSet::new();
    for approval in &approvals {
        let verified = verify_approval_crypto(approval)?;
        // Binding checks (spec §5.3): approval must reference this entry + root.
        if verified.community_id != body.community_id
            || verified.entry_id != expected_id
            || verified.state_root != body.state_root
        {
            return Err(Reject::InvalidApproval);
        }
        if !seen.insert(verified.approver) {
            // Duplicate approver for a single entry (spec D6 / §9).
            return Err(Reject::InvalidApproval);
        }
    }
    Ok(body)
}

#[allow(dead_code)]
fn _version_marker() -> u64 {
    GENESIS_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::super::model::Role;
    use super::super::operation::{GovernanceOperationKind, MemberGrant};
    use super::*;
    use crate::ids::ReplicaId;
    use crate::keys::SigningKey;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; LEN])
    }

    fn sample_body() -> GovernanceEntryBody {
        let community = CommunityId::from_bytes([0x70; LEN]);
        GovernanceEntryBody {
            community_id: community,
            seq: 1,
            prev: None,
            created_at_ms: 1_000,
            kind: GovernanceOperationKind::MemberGrant,
            payload: GovernanceOperationPayload::MemberGrant(MemberGrant {
                member_id: PrincipalId::from_bytes([0xc0; LEN]),
                role: Role::Member,
            }),
            state_root: StateRoot::from_bytes([0x33; LEN]),
        }
    }

    #[test]
    fn entry_body_round_trips_canonical_cbor() {
        let body = sample_body();
        let csb = entry_csb(&body);
        let back = decode_entry_csb(&csb).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn entry_body_rejects_unknown_kind() {
        // Build a CSB with a bogus kind discriminant.
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            for (k, v) in entries.iter_mut() {
                if k == "kind" {
                    *v = CborValue::Text("init_room".to_owned());
                }
            }
        }
        let csb = crate::cbor::encode(&value);
        assert_eq!(
            decode_entry_csb(&csb).err(),
            Some(Reject::UnknownRecordKind)
        );
    }

    #[test]
    fn entry_body_rejects_unknown_top_level_key() {
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            entries.push(("zz_unknown".to_owned(), CborValue::Uint(1)));
        }
        let csb = crate::cbor::encode(&value);
        assert_eq!(
            decode_entry_csb(&csb).err(),
            Some(Reject::NonCanonicalEncoding)
        );
    }

    #[test]
    fn entry_sign_and_verify_round_trips() {
        let author = key(0xa0);
        let entry = GovernanceEntry::new(sample_body(), &author, Vec::new());
        let body = verify_entry_crypto(&entry).expect("entry verifies");
        assert_eq!(body, entry.body);
        assert_eq!(entry_id(&body), entry_id(&entry.body));
    }

    #[test]
    fn entry_bad_signature_rejected() {
        let author = key(0xa0);
        let other = key(0xa1);
        let mut entry = GovernanceEntry::new(sample_body(), &author, Vec::new());
        entry.signature = other.sign(&domain::signing_message(
            domain::GOVERNANCE_ENTRY,
            &entry_csb(&entry.body),
        ));
        assert_eq!(
            verify_entry_crypto(&entry).err(),
            Some(Reject::BadSignature)
        );
    }

    #[test]
    fn approvals_sorted_and_duplicates_rejected() {
        let author = key(0xa0);
        let approver = key(0xc0);
        let body = sample_body();
        let approval_body = GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: entry_id(&body),
            state_root: body.state_root,
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let approval = GovernanceApproval::new(approval_body, &approver);
        // Duplicate approval from the same approver must reject.
        let entry_dup = GovernanceEntry::new(
            body.clone(),
            &author,
            vec![approval.clone(), approval.clone()],
        );
        assert_eq!(
            verify_entry_full(&entry_dup).err(),
            Some(Reject::InvalidApproval)
        );
        // A single approval verifies, regardless of caller order.
        let entry_ok = GovernanceEntry::new(body, &author, vec![approval]);
        verify_entry_full(&entry_ok).expect("single approval verifies");
    }

    #[test]
    fn approval_wrong_entry_binding_rejected() {
        let author = key(0xa0);
        let approver = key(0xc0);
        let body = sample_body();
        let bad_body = GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: GovernanceId::from_bytes([0xee; LEN]), // wrong entry
            state_root: body.state_root,
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let approval = GovernanceApproval::new(bad_body, &approver);
        let entry = GovernanceEntry::new(body, &author, vec![approval]);
        assert_eq!(
            verify_entry_full(&entry).err(),
            Some(Reject::InvalidApproval)
        );
    }

    #[test]
    fn entry_with_multiple_approvals_verifies_regardless_of_order() {
        // Two distinct approvers, supplied out of canonical order, must verify:
        // `verify_entry_full` sorts them before checking (spec D6).
        let author = key(0xa0);
        let body = sample_body();
        let mk_approval = |signer: &SigningKey| {
            GovernanceApproval::new(
                GovernanceApprovalBody {
                    community_id: body.community_id,
                    entry_id: entry_id(&body),
                    state_root: body.state_root,
                    approver: signer.member_id(),
                    created_at_ms: 1_001,
                },
                signer,
            )
        };
        let a1 = mk_approval(&key(0xc0));
        let a2 = mk_approval(&key(0xc1));
        // Supply in one order...
        let entry_fwd = GovernanceEntry::new(body.clone(), &author, vec![a1.clone(), a2.clone()]);
        verify_entry_full(&entry_fwd).expect("forward order verifies");
        // ...and the reverse — result must be identical (order-independent).
        let entry_rev = GovernanceEntry::new(body, &author, vec![a2, a1]);
        verify_entry_full(&entry_rev).expect("reverse order verifies");
    }

    #[test]
    fn approval_with_bad_signature_rejected() {
        let author = key(0xa0);
        let approver = key(0xc0);
        let body = sample_body();
        let approval_body = GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: entry_id(&body),
            state_root: body.state_root,
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let mut approval = GovernanceApproval::new(approval_body, &approver);
        // Corrupt the signature with one from a different key.
        let other = key(0xc9);
        approval.signature = other.sign(&domain::signing_message(
            domain::GOVERNANCE_APPROVAL,
            &approval_csb(&approval.body),
        ));
        let entry = GovernanceEntry::new(body, &author, vec![approval]);
        assert_eq!(verify_entry_full(&entry).err(), Some(Reject::BadSignature));
    }

    #[test]
    fn approval_wrong_state_root_binding_rejected() {
        let author = key(0xa0);
        let approver = key(0xc0);
        let body = sample_body();
        let bad_body = GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: entry_id(&body),
            state_root: StateRoot::from_bytes([0x99; LEN]), // wrong root
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let approval = GovernanceApproval::new(bad_body, &approver);
        let entry = GovernanceEntry::new(body, &author, vec![approval]);
        assert_eq!(
            verify_entry_full(&entry).err(),
            Some(Reject::InvalidApproval)
        );
    }

    #[test]
    fn entry_body_rejects_seq_zero() {
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            for (k, v) in entries.iter_mut() {
                if k == "seq" {
                    *v = CborValue::Uint(0);
                }
            }
        }
        let csb = crate::cbor::encode(&value);
        assert_eq!(decode_entry_csb(&csb).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn entry_body_rejects_seq_one_with_prev() {
        // Chain invariant: prev is None iff seq == 1 (spec D5).
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            entries.push(("prev".to_owned(), CborValue::Bytes(vec![0x01; LEN])));
        }
        let csb = crate::cbor::encode(&value);
        assert_eq!(decode_entry_csb(&csb).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn approval_id_marker_compiles() {
        // Ensures the unused ReplicaId import stays meaningful for future
        // replica-bearing entry bodies.
        let _ = ReplicaId::from_bytes([0; LEN]);
        let body = sample_body();
        let id = approval_id(&GovernanceApprovalBody {
            community_id: body.community_id,
            entry_id: entry_id(&body),
            state_root: body.state_root,
            approver: PrincipalId::from_bytes([0; LEN]),
            created_at_ms: 1,
        });
        assert_eq!(id.len(), LEN);
    }
}
