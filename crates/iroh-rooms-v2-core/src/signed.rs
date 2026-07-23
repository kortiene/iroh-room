//! The generic signed-record envelope pattern and the canonical-signed-bytes
//! (CSB) / id / signature derivations (spec §4 D2, §6.3, issue #147).
//!
//! Every v2 signed record has, in order (D2):
//! 1. a logical body that serializes to [`CborValue`];
//! 2. `CSB = cbor::encode(body)` (the deterministic-CBOR bytes);
//! 3. a domain-separated signing message `SIGN_CONTEXT || CSB`;
//! 4. an id `BLAKE3(ID_CONTEXT || CSB)`;
//! 5. an envelope preserving `CSB` byte-for-byte.
//!
//! This module provides the shared envelope and the per-record-family concrete
//! wrappers via a single macro. Body structs live in their owning modules
//! (`governance::entry`, `governance::approval`, `content::body`, …) and
//! implement [`SignedBody`].

use crate::cbor::{self, CborValue};
use crate::domain;
use crate::error::Reject;
use crate::keys::{verify, Signature, SigningKey};
use crate::MemberId;

/// A logical record body that can be canonically serialized and validated.
///
/// Implementors encode themselves to the closed deterministic-CBOR profile and,
/// on decode, perform strict body-specific validation. The implementation owns
/// the exact canonical bytes; the envelope never re-serializes before signature
/// verification.
pub trait SignedBody {
    /// The id type for this record family.
    type Id: Copy + PartialEq + Eq + core::fmt::Debug;

    /// The signing-message domain-separation context (`*_SIGN`).
    const SIGN_CONTEXT: &'static [u8];
    /// The id-derivation domain-separation context (`*_ID`).
    const ID_CONTEXT: &'static [u8];

    /// Canonical-encode this body to the deterministic CBOR profile.
    fn to_cbor(&self) -> CborValue;

    /// Decode + strictly validate a canonically-decoded body (spec D2 step 5).
    ///
    /// # Errors
    /// Returns a [`Reject`] for any malformed or out-of-profile body.
    fn from_canonical(value: &CborValue) -> Result<Self, Reject>
    where
        Self: Sized;

    /// Construct this body's id from its id-context and canonical bytes.
    fn id_from_csb(csb: &[u8]) -> Self::Id;
}

/// The decoded envelope around a set of canonical signed bytes (spec §6.3).
///
/// `{ id, signed, sig, signer }`. `signed` is the CSB verbatim — preserved
/// byte-for-byte for storage and forwarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope<Id: Copy + PartialEq + Eq> {
    /// The domain-derived record id.
    pub id: Id,
    /// The canonical signed bytes (CSB), verbatim.
    pub signed: Vec<u8>,
    /// Ed25519 signature over `SIGN_CONTEXT || signed`.
    pub sig: Signature,
    /// The signing principal whose key must verify the signature.
    pub signer: MemberId,
}

/// Build the canonical signed bytes (CSB) for a body.
#[must_use]
pub fn to_csb<B: SignedBody>(body: &B) -> Vec<u8> {
    cbor::encode(&body.to_cbor())
}

/// Derive a record id from a body: `BLAKE3(B::ID_CONTEXT || CSB)` (D2 step 4).
#[must_use]
pub fn id_of<B: SignedBody>(body: &B) -> B::Id {
    let csb = to_csb(body);
    B::id_from_csb(&csb)
}

/// Sign a body's CSB with a secret key: `Ed25519_sign(secret, SIGN_CONTEXT || CSB)`.
#[must_use]
pub fn sign<B: SignedBody>(body: &B, secret: &SigningKey) -> Signature {
    let csb = to_csb(body);
    sign_csb::<B>(&csb, secret)
}

/// Sign already-canonical bytes with a secret key.
#[must_use]
pub fn sign_csb<B: SignedBody>(csb: &[u8], secret: &SigningKey) -> Signature {
    secret.sign(&domain::signing_message(B::SIGN_CONTEXT, csb))
}

/// Build a complete envelope from a body + secret, stamping the recomputed id.
#[must_use]
pub fn seal<B: SignedBody>(body: &B, secret: &SigningKey) -> Envelope<B::Id> {
    let csb = to_csb(body);
    let id = B::id_from_csb(&csb);
    let sig = sign_csb::<B>(&csb, secret);
    Envelope {
        id,
        signed: csb,
        sig,
        signer: secret.member_id(),
    }
}

/// Verify an envelope's canonicality, id, signature, domain, and body
/// validation in one pass (spec D2 / §6.3 "expose decoded body only after
/// verification").
///
/// # Errors
/// - [`Reject::NonCanonicalEncoding`] — the CSB is not canonical CBOR.
/// - [`Reject::IdMismatch`] — the envelope id differs from the recomputed id.
/// - [`Reject::BadSignature`] — the signature does not verify under `signer`.
/// - Body-specific rejects from [`SignedBody::from_canonical`].
pub fn verify_envelope<B: SignedBody>(env: &Envelope<B::Id>) -> Result<B, Reject> {
    // Canonical decode of the exact received bytes (never a re-serialization).
    let value = cbor::decode_canonical(&env.signed)?;
    // Id must match the domain-derived id of the exact CSB.
    let recomputed = B::id_from_csb(&env.signed);
    if recomputed != env.id {
        return Err(Reject::IdMismatch);
    }
    // Signature over the domain-separated signing message.
    let msg = domain::signing_message(B::SIGN_CONTEXT, &env.signed);
    verify(&env.signer, &msg, &env.sig).map_err(|_| Reject::BadSignature)?;
    // Strict body-specific validation.
    B::from_canonical(&value)
}

/// Read an optional field from a map, returning `None` when absent.
pub(crate) fn opt<'a>(entries: &'a [(String, CborValue)], key: &str) -> Option<&'a CborValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Read a required field from a map, returning `None` when absent.
pub(crate) fn field<'a>(entries: &'a [(String, CborValue)], key: &str) -> Option<&'a CborValue> {
    opt(entries, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::CborValue;
    use crate::domain;
    use crate::ids::{GovernanceEntryId, LEN};

    /// A minimal test body: a single-field map, to exercise the generic envelope
    /// path end-to-end without depending on the governance module.
    #[derive(Debug, PartialEq, Eq)]
    struct EchoBody {
        n: u64,
    }

    impl SignedBody for EchoBody {
        type Id = GovernanceEntryId;
        const SIGN_CONTEXT: &'static [u8] = domain::GOVERNANCE_ENTRY_SIGN;
        const ID_CONTEXT: &'static [u8] = domain::GOVERNANCE_ENTRY_ID;

        fn to_cbor(&self) -> CborValue {
            CborValue::Map(vec![("n".to_owned(), CborValue::Uint(self.n))])
        }

        fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
            let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
            if entries.len() != 1 {
                return Err(Reject::NonCanonicalEncoding);
            }
            let n = entries[0].1.as_uint().ok_or(Reject::NonCanonicalEncoding)?;
            Ok(Self { n })
        }

        fn id_from_csb(csb: &[u8]) -> Self::Id {
            GovernanceEntryId::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
        }
    }

    #[test]
    fn seal_and_verify_round_trip() {
        let key = SigningKey::from_seed(&[0x11; LEN]);
        let body = EchoBody { n: 42 };
        let env = seal(&body, &key);
        let decoded = verify_envelope::<EchoBody>(&env).expect("valid envelope verifies");
        assert_eq!(decoded.n, 42);
    }

    #[test]
    fn id_mismatch_rejected() {
        let key = SigningKey::from_seed(&[0x22; LEN]);
        let body = EchoBody { n: 1 };
        let mut env = seal(&body, &key);
        env.id = GovernanceEntryId::from_bytes([0xff; LEN]);
        assert_eq!(verify_envelope::<EchoBody>(&env), Err(Reject::IdMismatch));
    }

    #[test]
    fn bad_signature_rejected() {
        let key = SigningKey::from_seed(&[0x33; LEN]);
        let other = SigningKey::from_seed(&[0x44; LEN]);
        let body = EchoBody { n: 1 };
        let mut env = seal(&body, &key);
        // Re-sign with a different key but keep the original signer.
        env.sig = sign_csb::<EchoBody>(&env.signed, &other);
        assert_eq!(verify_envelope::<EchoBody>(&env), Err(Reject::BadSignature));
    }

    #[test]
    fn non_canonical_csb_rejected() {
        let key = SigningKey::from_seed(&[0x55; LEN]);
        let body = EchoBody { n: 1 };
        let mut env = seal(&body, &key);
        // Append a trailing byte — no longer a single canonical item.
        env.signed.push(0x00);
        assert_eq!(
            verify_envelope::<EchoBody>(&env).err(),
            Some(Reject::NonCanonicalEncoding)
        );
    }
}
