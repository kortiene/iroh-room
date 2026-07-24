//! The canonical governance-log records: `GovernanceEntryBody`,
//! `GovernanceApprovalBody`, `GovernanceApproval`, and `GovernanceEntry`
//! (spec §5.2–§5.4, issue #147 / #178).
//!
//! Every record canonicalizes to closed deterministic CBOR and is signed under
//! the frozen #146 domains (`domain::GOVERNANCE_ENTRY`,
//! `domain::GOVERNANCE_APPROVAL`). The record wrappers (`GovernanceEntry`,
//! `GovernanceApproval`) own both the typed body and the **exact canonical
//! signed bytes (CSB)** they were constructed with, mirroring
//! `crate::signed::Envelope`: the body and CSB are private correlated fields
//! that cannot be desynchronized through safe public APIs.
//!
//! Retaining the verbatim CSB closes the trust-boundary gap (issue #178) left
//! by a typed decode that may normalize representation (e.g. `admin.set`
//! sorts and deduplicates `administrators`). The verification pipeline:
//!
//! 1. canonical decode of the exact received bytes (at construction);
//! 2. closed-schema validation (unknown keys → reject);
//! 3. `kind`/payload agreement + unknown-operation rejection;
//! 4. signature verification over the **retained** CSB (never a
//!    re-serialization of the typed body);
//! 5. post-signature semantic-canonicality check (typed re-encode == received);
//! 6. approval sorting (by retained-CSB approval hash), duplicate-approver
//!    rejection, signature + binding verification against the exact-CSB
//!    entry id.

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::ids::StateRoot;
use crate::ids::{CommunityId, GovernanceId, LEN};
use crate::keys::{verify, Signature};
use crate::PrincipalId;

use super::operation::GovernanceOperationPayload;
use super::GENESIS_SCHEMA_VERSION;

// ----------------------------------------------------------------------------
// #149: authenticated evidence (exact CSB + detached signatures) retained
// after successful verification so fork detection/audit can preserve signatures
// without trusting caller reconstruction (spec §6.1).
// ----------------------------------------------------------------------------

/// Authenticated evidence for a single verified approval (issue #149 §6.1).
///
/// Preserves the exact received canonical signed bytes (CSB) and the detached
/// Ed25519 signature that were verified by [`verify_approval_crypto`], so fork
/// audit evidence can re-expose them without re-deriving bytes from the typed
/// body. Construction occurs only inside [`verify_governance_entry`].
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedGovernanceApprovalEvidence {
    body: GovernanceApprovalBody,
    csb: Vec<u8>,
    signature: Signature,
}

impl VerifiedGovernanceApprovalEvidence {
    /// The verified approval body.
    #[must_use]
    pub fn body(&self) -> &GovernanceApprovalBody {
        &self.body
    }

    /// The exact canonical signed bytes (CSB) this approval was verified over.
    #[must_use]
    pub fn csb(&self) -> &[u8] {
        &self.csb
    }

    /// The detached Ed25519 signature over `domain::GOVERNANCE_APPROVAL || csb`.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }
}

impl std::fmt::Debug for VerifiedGovernanceApprovalEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedGovernanceApprovalEvidence")
            .field("body", &self.body)
            .field("csb_len", &self.csb.len())
            .field("signature", &self.signature)
            .finish()
    }
}

/// Authenticated evidence for a verified governance entry (issue #149 §6.1).
///
/// Bundles the exact-CSB-derived [`GovernanceId`], the typed body, the exact
/// retained entry CSB, the entry signer + signature, and the verified approval
/// evidence (each carrying its own exact CSB + signature). This is the
/// append-only audit material fork detection and resolution audit consume; it
/// is constructed only inside [`verify_governance_entry`] after every
/// cryptographic and canonical-encoding check has passed, so unauthenticated
/// bytes can never reach audit evidence.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedGovernanceEvidence {
    id: GovernanceId,
    body: GovernanceEntryBody,
    csb: Vec<u8>,
    signer: PrincipalId,
    signature: Signature,
    approvals: Vec<VerifiedGovernanceApprovalEvidence>,
}

impl AuthenticatedGovernanceEvidence {
    /// The exact-CSB-derived governance id (authenticated identity).
    #[must_use]
    pub fn id(&self) -> GovernanceId {
        self.id
    }

    /// The verified entry body.
    #[must_use]
    pub fn body(&self) -> &GovernanceEntryBody {
        &self.body
    }

    /// The exact canonical signed bytes (CSB) retained verbatim.
    #[must_use]
    pub fn csb(&self) -> &[u8] {
        &self.csb
    }

    /// The verified entry signer.
    #[must_use]
    pub fn signer(&self) -> PrincipalId {
        self.signer
    }

    /// The detached Ed25519 entry signature over `domain::GOVERNANCE_ENTRY || csb`.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// The verified approval evidence, canonically sorted and duplicate-free.
    #[must_use]
    pub fn approvals(&self) -> &[VerifiedGovernanceApprovalEvidence] {
        &self.approvals
    }
}

impl std::fmt::Debug for AuthenticatedGovernanceEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedGovernanceEvidence")
            .field("id", &self.id)
            .field("body", &self.body)
            .field("csb_len", &self.csb.len())
            .field("signer", &self.signer)
            .field("signature", &self.signature)
            .field("approvals_len", &self.approvals.len())
            .finish()
    }
}

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

/// Derive the [`GovernanceId`] of an entry from its **exact** canonical
/// signed bytes (spec D5 / issue #178): `BLAKE3(domain::GOVERNANCE_ENTRY ||
/// csb)`.
///
/// This is the byte-level identity used by the verify pipeline
/// ([`verify_governance_entry`]) and by [`VerifiedGovernanceEntry::id`].
/// Received or verified identity MUST go through this helper (or
/// [`VerifiedGovernanceEntry::id`]) on the retained CSB — never through a
/// re-encoding of the typed body.
#[must_use]
pub fn entry_id_from_csb(csb: &[u8]) -> GovernanceId {
    GovernanceId::from_governance_entry_csb(csb)
}

/// Derive the [`GovernanceId`] of an entry body (spec D5):
/// `BLAKE3(domain::GOVERNANCE_ENTRY || entry_csb)`.
///
/// Equivalent to [`entry_id_from_csb`] applied to one encoding of the body.
/// Use this only for canonical typed construction (a typed `new` constructor
/// pins that same encoding); received or verified identity must use
/// [`entry_id_from_csb`] on the retained CSB or [`VerifiedGovernanceEntry::id`].
#[must_use]
pub fn entry_id(body: &GovernanceEntryBody) -> GovernanceId {
    entry_id_from_csb(&entry_csb(body))
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

/// Derive the approval sort hash from its **exact** canonical signed bytes
/// (spec §5.3 / issue #178): `BLAKE3(domain::GOVERNANCE_APPROVAL || csb)`.
///
/// This is the byte-level hash used by [`verify_governance_entry`] when
/// sorting approvals, so canonical approval bytes never depend on caller
/// order. Sorting MUST go through this helper on the retained CSB — never
/// through a re-encoding of the typed body.
#[must_use]
pub fn approval_id_from_csb(csb: &[u8]) -> [u8; LEN] {
    domain::blake3_domain(domain::GOVERNANCE_APPROVAL, csb)
}

/// Derive the approval id (spec §5.3):
/// `BLAKE3(domain::GOVERNANCE_APPROVAL || approval_csb)`.
///
/// Equivalent to [`approval_id_from_csb`] applied to one encoding of the
/// body. Use this only for canonical typed construction; approval sorting
/// uses [`approval_id_from_csb`] on the retained CSB.
#[must_use]
pub fn approval_id(body: &GovernanceApprovalBody) -> [u8; LEN] {
    approval_id_from_csb(&approval_csb(body))
}

/// Decode + strictly validate an approval body from its canonical bytes.
///
/// # Errors
/// See [`GovernanceApprovalBody::from_canonical`] and
/// [`crate::cbor::decode_canonical`].
pub fn decode_approval_csb(csb: &[u8]) -> Result<GovernanceApprovalBody, Reject> {
    let value = crate::cbor::decode_canonical(csb)?;
    GovernanceApprovalBody::from_canonical(&value)
}

/// A signed governance approval (spec §5.3 / issue #178).
///
/// Owns both the typed approval body and the **exact canonical signed bytes
/// (CSB)** it was constructed with, mirroring `crate::signed::Envelope`. The
/// fields are private: body and CSB cannot be desynchronized through safe
/// public APIs. Signature verification
/// ([`verify_approval_crypto`]) runs over the retained CSB, never a
/// re-serialization of the typed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceApproval {
    /// The typed approval body.
    body: GovernanceApprovalBody,
    /// The exact canonical signed bytes of `body`, retained verbatim.
    csb: Vec<u8>,
    /// The detached Ed25519 signature over `domain::GOVERNANCE_APPROVAL ||
    /// csb`.
    signature: Signature,
}

impl GovernanceApproval {
    /// Construct a signed approval from a typed body (spec D2). Encodes the
    /// body to its canonical CSB exactly once, signs that vector, and pins
    /// both alongside the signature.
    #[must_use]
    pub fn new(body: GovernanceApprovalBody, secret: &crate::keys::SigningKey) -> Self {
        let csb = approval_csb(&body);
        let msg = domain::signing_message(domain::GOVERNANCE_APPROVAL, &csb);
        Self {
            body,
            csb,
            signature: secret.sign(&msg),
        }
    }

    /// Construct an approval from its **exact received** canonical signed
    /// bytes — the trust-boundary constructor (issue #178). Canonical-decodes
    /// the supplied slice to form the typed body, but retains the supplied
    /// vector byte-for-byte: authentication later uses the retained bytes,
    /// never a re-encoding.
    ///
    /// No cryptographic verification is performed here. Body and CSB are
    /// correlated after construction; callers cannot independently mutate
    /// either through safe public APIs.
    ///
    /// # Errors
    /// Returns the existing strict-decode error (normally
    /// [`Reject::NonCanonicalEncoding`] or [`Reject::InvalidContent`]) when
    /// the slice is not a canonical, closed-schema approval body.
    pub fn from_received_csb(csb: Vec<u8>, signature: Signature) -> Result<Self, Reject> {
        let body = decode_approval_csb(&csb)?;
        Ok(Self {
            body,
            csb,
            signature,
        })
    }

    /// The typed approval body.
    #[must_use]
    pub fn body(&self) -> &GovernanceApprovalBody {
        &self.body
    }

    /// The exact canonical signed bytes (CSB) this approval was constructed
    /// with — verbatim, never re-derived from the typed body.
    #[must_use]
    pub fn csb(&self) -> &[u8] {
        &self.csb
    }

    /// The detached Ed25519 signature over `domain::GOVERNANCE_APPROVAL ||
    /// csb()`.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }
}

/// A signed governance-log entry with its approvals (spec §5.4 / issue #178).
///
/// Owns both the typed entry body and the **exact canonical signed bytes
/// (CSB)** it was constructed with, mirroring `crate::signed::Envelope`.
/// Signer, signature, and approvals remain outside entry-body CSB; the body
/// and CSB are private correlated fields that cannot be desynchronized
/// through safe public APIs. Signature verification
/// ([`verify_entry_crypto`]) runs over the retained CSB, never a
/// re-serialization of the typed body, and [`VerifiedGovernanceEntry::id`] is
/// derived from the retained CSB so chain links bind to the authenticated
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceEntry {
    /// The typed entry body.
    body: GovernanceEntryBody,
    /// The exact canonical signed bytes of `body`, retained verbatim.
    csb: Vec<u8>,
    /// The signing principal.
    signer: PrincipalId,
    /// The detached Ed25519 signature over `domain::GOVERNANCE_ENTRY || csb`.
    signature: Signature,
    /// Approvals collected for this entry (outside entry-body CSB). Verified
    /// and canonically sorted copies are returned from
    /// [`VerifiedGovernanceEntry::approvals`].
    approvals: Vec<GovernanceApproval>,
}
impl GovernanceEntry {
    /// Construct a signed entry (approvals supplied separately). Encodes the
    /// body to its canonical CSB exactly once, signs that vector, and pins
    /// both. Approvals are stored outside entry-body CSB.
    #[must_use]
    pub fn new(
        body: GovernanceEntryBody,
        secret: &crate::keys::SigningKey,
        approvals: Vec<GovernanceApproval>,
    ) -> Self {
        let csb = entry_csb(&body);
        let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, &csb);
        Self {
            body,
            csb,
            signer: secret.member_id(),
            signature: secret.sign(&msg),
            approvals,
        }
    }

    /// Construct an entry from its **exact received** canonical signed bytes
    /// — the trust-boundary constructor (issue #178). Canonical-decodes the
    /// supplied slice to form the typed body, but retains the supplied vector
    /// byte-for-byte: authentication later uses the retained bytes, never a
    /// re-encoding.
    ///
    /// No cryptographic verification is performed here. Body and CSB are
    /// correlated after construction; callers cannot independently mutate
    /// either through safe public APIs. Signer, signature, and approvals are
    /// detached from entry-body CSB and stored unchanged.
    ///
    /// # Errors
    /// Returns the existing strict-decode error (normally
    /// [`Reject::NonCanonicalEncoding`], [`Reject::UnknownRecordKind`], or
    /// [`Reject::InvalidContent`]) when the slice is not a canonical,
    /// closed-schema entry body.
    pub fn from_received_csb(
        csb: Vec<u8>,
        signer: PrincipalId,
        signature: Signature,
        approvals: Vec<GovernanceApproval>,
    ) -> Result<Self, Reject> {
        let body = decode_entry_csb(&csb)?;
        Ok(Self {
            body,
            csb,
            signer,
            signature,
            approvals,
        })
    }

    /// The typed entry body.
    #[must_use]
    pub fn body(&self) -> &GovernanceEntryBody {
        &self.body
    }

    /// The exact canonical signed bytes (CSB) this entry was constructed
    /// with — verbatim, never re-derived from the typed body.
    #[must_use]
    pub fn csb(&self) -> &[u8] {
        &self.csb
    }

    /// The signing principal.
    #[must_use]
    pub fn signer(&self) -> PrincipalId {
        self.signer
    }

    /// The detached Ed25519 signature over `domain::GOVERNANCE_ENTRY ||
    /// csb()`.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// The approvals attached to this entry (in caller order), all outside
    /// entry-body CSB.
    #[must_use]
    pub fn approvals(&self) -> &[GovernanceApproval] {
        &self.approvals
    }
}

/// Verify an entry's signature over its **retained** canonical signed bytes,
/// then require the typed body to re-encode to those exact bytes (spec §5.4
/// step 4 / issue #178).
///
/// The signature is checked over `entry.csb()` — the exact bytes the record
/// was constructed with — using `entry.signer()`, **never** a re-serialization
/// of the typed body. Only after the signature check is the typed body
/// re-encoded and required to be byte-identical to the retained CSB. This
/// fences the trust-boundary gap where a body whose typed decode normalizes
/// its representation (e.g. an unsorted/duplicate `admin.set`
/// `administrators` array) could otherwise be accepted over bytes that
/// differ from what was signed.
///
/// Returns the validated body. Approval verification is separate
/// ([`verify_governance_entry`]).
///
/// # Errors
/// - [`Reject::BadSignature`] — signature does not verify under
///   `entry.signer()` against the retained CSB.
/// - [`Reject::NonCanonicalEncoding`] — typed re-encoding of the body
///   differs from the retained CSB (semantic canonicality).
pub fn verify_entry_crypto(entry: &GovernanceEntry) -> Result<GovernanceEntryBody, Reject> {
    let received = entry.csb();
    // Step 1: signature over the exact retained CSB (issue #178). No
    // typed-body serialization happens before this point.
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY, received);
    verify(&entry.signer(), &msg, entry.signature()).map_err(|_| Reject::BadSignature)?;
    // Step 2 (post-signature): the typed body must re-encode to exactly the
    // received CSB. The body was decoded from this CSB at construction; this
    // also fences any representation normalization during typed decode.
    let reencoded = entry_csb(entry.body());
    if reencoded.as_slice() != received {
        return Err(Reject::NonCanonicalEncoding);
    }
    Ok(entry.body().clone())
}

/// Verify an approval's signature over its **retained** canonical signed
/// bytes, then require the typed body to re-encode to those exact bytes
/// (spec §5.3 / §5.4 step 6 / issue #178).
///
/// The signature is checked over `approval.csb()` — the exact bytes the
/// record was constructed with — using the approver carried inside the body,
/// **never** a re-serialization. After the signature check the typed body is
/// re-encoded and required to be byte-identical to the retained CSB.
///
/// # Errors
/// - [`Reject::BadSignature`] — signature does not verify under
///   `body.approver` against the retained CSB.
/// - [`Reject::NonCanonicalEncoding`] — typed re-encoding of the body
///   differs from the retained CSB.
pub fn verify_approval_crypto(
    approval: &GovernanceApproval,
) -> Result<GovernanceApprovalBody, Reject> {
    let received = approval.csb();
    // The signer must be the claimed approver (spec §5.3).
    let approver = approval.body().approver;
    // Step 1: signature over the exact retained CSB (issue #178). No
    // typed-body serialization happens before this point.
    let msg = domain::signing_message(domain::GOVERNANCE_APPROVAL, received);
    verify(&approver, &msg, approval.signature()).map_err(|_| Reject::BadSignature)?;
    // Step 2 (post-signature): the typed body must re-encode to exactly the
    // received CSB.
    let reencoded = approval_csb(approval.body());
    if reencoded.as_slice() != received {
        return Err(Reject::NonCanonicalEncoding);
    }
    Ok(approval.body().clone())
}

/// A cryptographically verified governance entry (issue #148 D2 / #178).
///
/// Preserves the exact inputs the #148 authorization predicate needs — the
/// verified signer and the verified, canonically sorted approval bodies —
/// alongside the **exact-CSB-derived** governance id ([`Self::id`]). Fields
/// are private: the only way to construct one is
/// [`verify_governance_entry`], so policy code can never be handed an
/// "verified" identity that was not actually checked against a real Ed25519
/// signature over the retained bytes. Chain-linking and #149 fork evidence
/// must consume [`Self::id`] rather than re-deriving from the typed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGovernanceEntry {
    /// The full authenticated evidence bundle (exact CSB, signatures, and
    /// verified approvals) retained for #149 fork detection/audit.
    evidence: AuthenticatedGovernanceEvidence,
}

impl VerifiedGovernanceEntry {
    /// The exact-CSB-derived governance id of this verified entry (issue
    /// #178). Derived from the retained entry CSB only after entry
    /// crypto/round-trip success, so it is the authenticated identity.
    #[must_use]
    pub fn id(&self) -> GovernanceId {
        self.evidence.id
    }

    /// The verified entry body.
    #[must_use]
    pub fn body(&self) -> &GovernanceEntryBody {
        &self.evidence.body
    }

    /// The verified entry signer.
    #[must_use]
    pub fn signer(&self) -> PrincipalId {
        self.evidence.signer
    }

    /// The verified, canonically sorted, duplicate-free approval evidence.
    /// Each entry carries its exact retained CSB and detached signature
    /// (issue #149 §6.1) so fork audit preserves approval signatures without
    /// trusting caller reconstruction.
    #[must_use]
    pub fn approvals(&self) -> &[VerifiedGovernanceApprovalEvidence] {
        &self.evidence.approvals
    }

    /// The full authenticated evidence bundle (exact entry CSB, entry
    /// signature, and verified approval evidence) retained for #149 fork
    /// detection and audit (spec §6.1).
    #[must_use]
    pub fn authenticated_evidence(&self) -> &AuthenticatedGovernanceEvidence {
        &self.evidence
    }
}

/// Verify a full entry: body crypto over the retained CSB, entry signature,
/// exact-CSB identity derivation, approval sorting by retained-CSB approval
/// hash, duplicate-approver rejection, approval signatures, and approval
/// bindings against the exact-CSB identity (spec §5.4 pipeline / issue
/// #148 D2 / #178), returning the verified signer and approval bodies
/// alongside the body and the authenticated identity.
///
/// # Errors
/// - Any error from [`verify_entry_crypto`].
/// - [`Reject::BadSignature`] — an approval signature fails against its
///   retained CSB.
/// - [`Reject::NonCanonicalEncoding`] — an approval body re-encodes to bytes
///   other than its retained CSB.
/// - [`Reject::InvalidApproval`] — duplicate approver, or an approval is not
///   bound to the entry's `community_id`, exact-CSB entry id, or declared
///   `state_root`.
pub fn verify_governance_entry(entry: &GovernanceEntry) -> Result<VerifiedGovernanceEntry, Reject> {
    let body = verify_entry_crypto(entry)?;
    // Authenticated identity: derived from the retained CSB only after entry
    // crypto + round-trip success (issue #178).
    let verified_id = entry_id_from_csb(entry.csb());

    // Sort approvals canonically by (approver bytes, retained-CSB approval
    // hash) so the canonical bytes do not depend on caller order (spec D6 /
    // issue #178: the sort hash must use retained CSB).
    let mut approvals = entry.approvals.clone();
    approvals.sort_by(|a, b| {
        (
            a.body().approver.as_bytes(),
            approval_id_from_csb(a.csb()).as_slice(),
        )
            .cmp(&(
                b.body().approver.as_bytes(),
                approval_id_from_csb(b.csb()).as_slice(),
            ))
    });

    let mut seen = std::collections::BTreeSet::new();
    let mut approval_evidence = Vec::with_capacity(approvals.len());
    for approval in &approvals {
        let verified = verify_approval_crypto(approval)?;
        // Binding checks (spec §5.3): approval must reference this entry's
        // exact-CSB identity + community + declared root.
        if verified.community_id != body.community_id
            || verified.entry_id != verified_id
            || verified.state_root != body.state_root
        {
            return Err(Reject::InvalidApproval);
        }
        if !seen.insert(verified.approver) {
            // Duplicate approver for a single entry (spec D6 / §9).
            return Err(Reject::InvalidApproval);
        }
        // #149: retain the exact approval CSB + signature for audit evidence.
        approval_evidence.push(VerifiedGovernanceApprovalEvidence {
            body: verified,
            csb: approval.csb().to_vec(),
            signature: *approval.signature(),
        });
    }
    let evidence = AuthenticatedGovernanceEvidence {
        id: verified_id,
        body,
        csb: entry.csb().to_vec(),
        signer: entry.signer(),
        signature: *entry.signature(),
        approvals: approval_evidence,
    };
    Ok(VerifiedGovernanceEntry { evidence })
}

/// Compatibility wrapper over [`verify_governance_entry`] that returns only
/// the verified body (pre-#148 signature). New callers should prefer
/// [`verify_governance_entry`] plus the `log::authz` validation pipeline,
/// which is the only path that can evaluate the #148 five-rule authorization
/// predicate (the signer/approval set discarded here is required by rule 4).
///
/// # Errors
/// See [`verify_governance_entry`].
pub fn verify_entry_full(entry: &GovernanceEntry) -> Result<GovernanceEntryBody, Reject> {
    verify_governance_entry(entry).map(|verified| verified.body().clone())
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
        assert_eq!(&body, entry.body());
        assert_eq!(entry_id(&body), entry_id(entry.body()));
        // The retained CSB is exactly the typed-body encoding, and the
        // exact-CSB identity matches the typed-body identity (issue #178).
        assert_eq!(entry.csb(), entry_csb(entry.body()));
        assert_eq!(entry_id_from_csb(entry.csb()), entry_id(entry.body()));
    }

    #[test]
    fn entry_bad_signature_rejected() {
        let author = key(0xa0);
        let other = key(0xa1);
        let entry = GovernanceEntry::new(sample_body(), &author, Vec::new());
        // A signature valid only under a different key, over the same retained
        // CSB. The record is reconstructed from the retained CSB with the
        // alternate signature (mirrors a wire peer presenting a bad
        // signature); the body/CSB cannot be mutated directly post-construction.
        let bad_sig = other.sign(&domain::signing_message(
            domain::GOVERNANCE_ENTRY,
            entry.csb(),
        ));
        let bad = GovernanceEntry::from_received_csb(
            entry.csb().to_vec(),
            entry.signer(),
            bad_sig,
            Vec::new(),
        )
        .expect("retained CSB decodes");
        assert_eq!(verify_entry_crypto(&bad).err(), Some(Reject::BadSignature));
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
    fn approval_constructors_pin_and_preserve_signed_bytes() {
        let approver = key(0xc0);
        let entry = sample_body();
        let body = GovernanceApprovalBody {
            community_id: entry.community_id,
            entry_id: entry_id(&entry),
            state_root: entry.state_root,
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let expected_csb = approval_csb(&body);
        let local = GovernanceApproval::new(body.clone(), &approver);
        assert_eq!(local.csb(), expected_csb);
        assert_eq!(verify_approval_crypto(&local), Ok(body.clone()));
        assert_eq!(approval_id_from_csb(local.csb()), approval_id(local.body()));

        let received =
            GovernanceApproval::from_received_csb(expected_csb.clone(), *local.signature())
                .expect("approval CSB decodes");
        assert_eq!(received.csb(), expected_csb);
        assert_eq!(verify_approval_crypto(&received), Ok(body));
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
        let approval = GovernanceApproval::new(approval_body, &approver);
        // Corrupt the signature with one from a different key, reconstructing
        // the record from its retained CSB (body/CSB cannot be mutated
        // directly post-construction).
        let other = key(0xc9);
        let bad_sig = other.sign(&domain::signing_message(
            domain::GOVERNANCE_APPROVAL,
            approval.csb(),
        ));
        let bad_approval = GovernanceApproval::from_received_csb(approval.csb().to_vec(), bad_sig)
            .expect("retained approval CSB decodes");
        let entry = GovernanceEntry::new(body, &author, vec![bad_approval]);
        assert_eq!(verify_entry_full(&entry).err(), Some(Reject::BadSignature));
    }

    #[test]
    fn approval_crypto_checks_retained_csb_before_typed_reencoding() {
        let approver = key(0xc0);
        let entry = sample_body();
        let received_body = GovernanceApprovalBody {
            community_id: entry.community_id,
            entry_id: entry_id(&entry),
            state_root: entry.state_root,
            approver: approver.member_id(),
            created_at_ms: 1_001,
        };
        let received_csb = approval_csb(&received_body);
        let mut reencoded_body = received_body;
        reencoded_body.created_at_ms = 1_002;
        let reencoded_csb = approval_csb(&reencoded_body);

        let reencoded_only_signature = approver.sign(&domain::signing_message(
            domain::GOVERNANCE_APPROVAL,
            &reencoded_csb,
        ));
        let reencoded_only = GovernanceApproval {
            body: reencoded_body.clone(),
            csb: received_csb.clone(),
            signature: reencoded_only_signature,
        };
        assert_eq!(
            verify_approval_crypto(&reencoded_only).err(),
            Some(Reject::BadSignature)
        );

        let exact_signature = approver.sign(&domain::signing_message(
            domain::GOVERNANCE_APPROVAL,
            &received_csb,
        ));
        let exact = GovernanceApproval {
            body: reencoded_body,
            csb: received_csb,
            signature: exact_signature,
        };
        assert_eq!(
            verify_approval_crypto(&exact).err(),
            Some(Reject::NonCanonicalEncoding)
        );
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

    // --- #148 D2: verified-entry wrapper -----------------------------------

    #[test]
    fn verify_governance_entry_and_verify_entry_full_agree_on_body() {
        let author = key(0xa0);
        let approver = key(0xc0);
        let body = sample_body();
        let approval = GovernanceApproval::new(
            GovernanceApprovalBody {
                community_id: body.community_id,
                entry_id: entry_id(&body),
                state_root: body.state_root,
                approver: approver.member_id(),
                created_at_ms: 1_001,
            },
            &approver,
        );
        let entry = GovernanceEntry::new(body.clone(), &author, vec![approval]);

        let verified = verify_governance_entry(&entry).expect("verifies");
        assert_eq!(verified.body(), &body);
        assert_eq!(verified.signer(), author.member_id());
        assert_eq!(verified.approvals().len(), 1);
        assert_eq!(
            verified.approvals()[0].body().approver,
            approver.member_id()
        );

        // The compatibility wrapper must agree exactly on the returned body.
        let compat_body = verify_entry_full(&entry).expect("verifies");
        assert_eq!(compat_body, *verified.body());
    }

    #[test]
    fn verify_governance_entry_rejects_same_failures_as_verify_entry_full() {
        let author = key(0xa0);
        let other = key(0xa1);
        let entry = GovernanceEntry::new(sample_body(), &author, Vec::new());
        // Reconstruct from the retained CSB with an alternate signature
        // (body/CSB cannot be mutated directly post-construction).
        let bad_sig = other.sign(&domain::signing_message(
            domain::GOVERNANCE_ENTRY,
            entry.csb(),
        ));
        let bad = GovernanceEntry::from_received_csb(
            entry.csb().to_vec(),
            entry.signer(),
            bad_sig,
            Vec::new(),
        )
        .expect("retained CSB decodes");
        assert_eq!(
            verify_governance_entry(&bad).err(),
            Some(Reject::BadSignature)
        );
        assert_eq!(verify_entry_full(&bad).err(), Some(Reject::BadSignature));
    }

    // --- #178: normalize-during-decode trust-boundary regression ------------
    //
    // `admin.set` sorts and deduplicates `administrators` during typed decode,
    // so several distinct deterministic CSBs decode to the same typed body.
    // Before #178, the verify path re-derived CSB from the typed body and so
    // accepted a signature valid only over the *normalized* bytes — breaking
    // the signed-record trust boundary. The record layer now retains the
    // exact received CSB; the signature check runs over those bytes, so a
    // normalized-only signature is rejected as `BadSignature`, and an exact
    // signature over semantically normalizing bytes is rejected post-signature
    // as `NonCanonicalEncoding`. This test constructs the altered CSB
    // directly (never via `entry_csb(decoded_body)`) so it fences the
    // vulnerability rather than re-deriving the bytes under test.

    /// Build a typed `admin.set` entry body over sorted unique `[A, B]`.
    fn admin_set_body(a: PrincipalId, b: PrincipalId) -> GovernanceEntryBody {
        use super::super::operation::AdminSet;
        GovernanceEntryBody {
            community_id: CommunityId::from_bytes([0x70; LEN]),
            seq: 1,
            prev: None,
            created_at_ms: 1_000,
            kind: super::super::operation::GovernanceOperationKind::AdminSet,
            payload: GovernanceOperationPayload::AdminSet(AdminSet {
                administrators: vec![a, b],
                threshold: 1,
            }),
            state_root: StateRoot::from_bytes([0x33; LEN]),
        }
    }

    /// Reorder the `payload.administrators` array inside an entry-body
    /// `CborValue` to `perm` (without touching any other field), then
    /// deterministically encode it.
    fn reencoded_admin_set_with_permutation(body: &GovernanceEntryBody, perm: &[usize]) -> Vec<u8> {
        let mut value = body.to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            for (k, v) in entries.iter_mut() {
                if k == "payload" {
                    if let CborValue::Map(ref mut payload_entries) = v {
                        for (pk, pv) in payload_entries.iter_mut() {
                            if pk == "administrators" {
                                if let CborValue::Array(ref mut admins) = pv {
                                    let original = admins.clone();
                                    admins.clear();
                                    for &idx in perm {
                                        admins.push(original[idx].clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        crate::cbor::encode(&value)
    }

    #[test]
    fn altered_normalizing_csb_with_normalized_only_signature_is_bad_signature() {
        // Deterministic principals A < B (ed25519 public-key bytes are not
        // seed-ordered; sort to guarantee A < B).
        let mut principals = [key(0x01).member_id(), key(0x02).member_id()];
        principals.sort();
        let [a, b] = principals;
        let body = admin_set_body(a, b);
        let author = key(0xa0);

        // Canonical normalized CSB and a signature valid only over it.
        let normalized_csb = entry_csb(&body);
        let normalized_sig = author.sign(&domain::signing_message(
            domain::GOVERNANCE_ENTRY,
            &normalized_csb,
        ));

        // Altered CSB: `administrators` permuted to `[B, A]` — valid
        // deterministic CBOR that decodes to the same typed body.
        let received_csb = reencoded_admin_set_with_permutation(&body, &[1, 0]);
        assert_ne!(
            received_csb, normalized_csb,
            "altered CSB must differ from the normalized CSB"
        );
        // Both CSBs decode to the same normalized typed body (sort/dedup).
        let from_normalized = decode_entry_csb(&normalized_csb).expect("normalized decodes");
        let from_received = decode_entry_csb(&received_csb).expect("altered decodes");
        assert_eq!(
            from_normalized, from_received,
            "both CSBs must normalize to the same typed body"
        );
        // Their exact-CSB identities must differ.
        assert_ne!(
            entry_id_from_csb(&received_csb),
            entry_id_from_csb(&normalized_csb),
            "exact-CSB identities must differ"
        );

        // Reconstruct the record from the ALTERED bytes carrying a signature
        // valid only over the NORMALIZED bytes. Before #178 this verified;
        // now the signature check runs over the retained altered CSB and
        // fails before any round-trip / binding work.
        let bad = GovernanceEntry::from_received_csb(
            received_csb,
            author.member_id(),
            normalized_sig,
            Vec::new(),
        )
        .expect("altered CSB decodes");
        assert_eq!(
            verify_entry_crypto(&bad).err(),
            Some(Reject::BadSignature),
            "a signature valid only over normalized bytes must not verify over altered received bytes"
        );
        assert_eq!(
            verify_entry_full(&bad).err(),
            Some(Reject::BadSignature),
            "the full pipeline must reject at the signature check too"
        );
    }

    #[test]
    fn altered_normalizing_csb_signed_over_exact_bytes_is_non_canonical_encoding() {
        let mut principals = [key(0x01).member_id(), key(0x02).member_id()];
        principals.sort();
        let [a, b] = principals;
        let body = admin_set_body(a, b);
        let author = key(0xa0);

        // Altered CSB with a duplicate administrator `[A, A, B]` — also
        // valid deterministic CBOR that normalizes to the same typed body.
        let received_csb = reencoded_admin_set_with_permutation(&body, &[0, 0, 1]);
        assert_ne!(
            received_csb,
            entry_csb(&body),
            "altered CSB must differ from the normalized CSB"
        );

        // A signature valid over the EXACT altered bytes — passes the
        // signature check, then fails the post-signature round-trip
        // (semantic canonicality), surfacing as `NonCanonicalEncoding`.
        let exact_sig = author.sign(&domain::signing_message(
            domain::GOVERNANCE_ENTRY,
            &received_csb,
        ));
        let signed = GovernanceEntry::from_received_csb(
            received_csb,
            author.member_id(),
            exact_sig,
            Vec::new(),
        )
        .expect("altered CSB decodes");
        assert_eq!(
            verify_entry_crypto(&signed).err(),
            Some(Reject::NonCanonicalEncoding),
            "an exact signature over semantically normalizing bytes must fail the post-signature round-trip"
        );
    }
}
