//! The `WireEvent` transport/storage envelope (Event Protocol §3).
//!
//! ```text
//! WireEvent = { "v": 1, "signed": bstr, "sig": bstr[64], "id": tstr }
//! ```
//!
//! `signed` is the CSB verbatim — preserved byte-for-byte for storage and
//! forwarding. `id` is an advisory cache key, always recomputed and checked,
//! never trusted (§4/§6 step 2).
//!
//! Note on canonical key order: Event Protocol §3 item 1 mandates *encoded-form*
//! (length-first then bytewise) ordering, which for these keys is
//! `v, id, sig, signed`. (The §6.3 prose example "id, sig, signed, v" is pure
//! bytewise and contradicts the normative §3 rule; the outer envelope bytes are
//! **not** part of the byte-exact golden vectors. We follow the §3 rule via the
//! shared canonical encoder, so encode/decode are self-consistent.)

use super::cbor::{self, CborValue};
use super::constants::{SIGNATURE_LEN, WIRE_VERSION};
use super::ids::EventId;
use super::keys::Signature;
use super::reject::RejectReason;
use super::signed::event_id_from_bytes;

/// The decoded transport envelope around a set of canonical signed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEvent {
    /// Transport envelope version (MUST be `1`).
    pub v: u64,
    /// The canonical signed bytes (CSB), verbatim.
    pub signed: Vec<u8>,
    /// Ed25519 signature over `EVENT_CONTEXT ‖ signed`.
    pub sig: Signature,
    /// Advisory `blake3:<hex>` cache key; recomputed and checked, never trusted.
    pub id: String,
}

impl WireEvent {
    /// Build a `WireEvent` around already-signed bytes, stamping `v = 1` and the
    /// recomputed advisory `id`.
    #[must_use]
    pub fn seal(signed: Vec<u8>, sig: Signature) -> Self {
        let id = event_id_from_bytes(&signed).to_named_string();
        Self {
            v: WIRE_VERSION,
            signed,
            sig,
            id,
        }
    }

    /// The advisory `id` parsed as an [`EventId`], if it is a well-formed
    /// `blake3:<hex>` string. (The validator recomputes the true id regardless.)
    #[must_use]
    pub fn advisory_id(&self) -> Option<EventId> {
        self.id.parse().ok()
    }

    /// Encode to canonical CBOR.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let entries = vec![
            ("v".to_owned(), CborValue::Uint(self.v)),
            ("signed".to_owned(), CborValue::Bytes(self.signed.clone())),
            (
                "sig".to_owned(),
                CborValue::Bytes(self.sig.as_bytes().to_vec()),
            ),
            ("id".to_owned(), CborValue::Text(self.id.clone())),
        ];
        cbor::encode(&CborValue::Map(entries))
    }

    /// Decode and strictly validate a transport envelope (Event Protocol §6
    /// step 1): canonical outer map, exactly the four keys with correct types,
    /// and `v == 1`.
    ///
    /// # Errors
    /// Returns [`RejectReason::NonCanonicalEncoding`] for any non-canonical
    /// encoding, missing/extra/wrong-typed key, or `v != 1`.
    pub fn decode(bytes: &[u8]) -> Result<Self, RejectReason> {
        let value =
            cbor::decode_canonical(bytes).map_err(|_| RejectReason::NonCanonicalEncoding)?;
        let entries = value.as_map().ok_or(RejectReason::NonCanonicalEncoding)?;
        if entries.len() != 4 {
            return Err(RejectReason::NonCanonicalEncoding);
        }

        let mut v: Option<u64> = None;
        let mut signed: Option<Vec<u8>> = None;
        let mut sig: Option<Signature> = None;
        let mut id: Option<String> = None;

        for (key, val) in entries {
            match key.as_str() {
                "v" => v = Some(val.as_uint().ok_or(RejectReason::NonCanonicalEncoding)?),
                "signed" => {
                    signed = Some(
                        val.as_bytes()
                            .ok_or(RejectReason::NonCanonicalEncoding)?
                            .to_vec(),
                    );
                }
                "sig" => {
                    let bytes = val.as_bytes().ok_or(RejectReason::NonCanonicalEncoding)?;
                    let arr = <[u8; SIGNATURE_LEN]>::try_from(bytes)
                        .map_err(|_| RejectReason::NonCanonicalEncoding)?;
                    sig = Some(Signature::from_bytes(arr));
                }
                "id" => {
                    id = Some(
                        val.as_text()
                            .ok_or(RejectReason::NonCanonicalEncoding)?
                            .to_owned(),
                    );
                }
                _ => return Err(RejectReason::NonCanonicalEncoding),
            }
        }

        let v = v.ok_or(RejectReason::NonCanonicalEncoding)?;
        let signed = signed.ok_or(RejectReason::NonCanonicalEncoding)?;
        let sig = sig.ok_or(RejectReason::NonCanonicalEncoding)?;
        let id = id.ok_or(RejectReason::NonCanonicalEncoding)?;

        if v != WIRE_VERSION {
            return Err(RejectReason::NonCanonicalEncoding);
        }

        Ok(Self { v, signed, sig, id })
    }
}
