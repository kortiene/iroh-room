//! The normative #134 §9.2 `ContentEventBody` (issue #152).
//!
//! This is the single accepted v2 content wire schema. It replaces the
//! provisional envelope preserved (test-only) in [`super::provisional`]. The
//! body is one canonical-CBOR map of exactly twelve keys; every key is required,
//! including `prev_device_event`, which uses explicit CBOR `null` for the first
//! device event rather than omission (spec §3.1).
//!
//! The exact canonical bytes (`body_csb`) are the cryptographic trust boundary:
//! the [`EventId`] is `BLAKE3(CONTENT_EVENT || body_csb)` and the Ed25519
//! signature is over `CONTENT_EVENT || body_csb`, both computed over those exact
//! bytes — never a re-serialization. The signature verifies under
//! `body.device_id` (NOT `author_id`); that envelope + verification path lives
//! in [`super::event`].
//!
//! This struct does NOT implement [`crate::signed::SignedBody`]: the signature
//! verification key is an in-body field (`device_id`), not an out-of-body
//! principal signer, so a concrete content path is used rather than weakening
//! the generic trait (spec §6 API invariants).

use crate::cbor::{self, CborValue};
use crate::content::registry::ContentKind;
use crate::error::Reject;
use crate::ids::{CommunityId, DeviceId, EventId, PrincipalId, StreamId, LEN};

/// The v2 content-event body version. `v` MUST equal this value (spec §9.2 /
/// §3.1). Any other value rejects as [`Reject::UnknownVersion`].
pub const CONTENT_EVENT_VERSION: u64 = 2;

/// Maximum number of `references` entries on a content event (spec #152 §3.6).
/// Validation accepts `0..=MAX_CONTENT_REFERENCES` and rejects any more.
pub const MAX_CONTENT_REFERENCES: usize = 8;

/// The exact twelve wire keys of a `ContentEventBody` (spec §9.2). Used to close
/// the top-level key set so an unknown key is rejected rather than ignored.
pub const TOP_LEVEL_KEYS: &[&str] = &[
    "v",
    "community_id",
    "stream_id",
    "author_id",
    "device_id",
    "device_seq",
    "prev_device_event",
    "auth_hint_seq",
    "created_at_ms",
    "kind",
    "references",
    "content",
];

/// The normative #134 §9.2 content-event body: one canonical-CBOR map of exactly
/// twelve keys. Every field is part of the signed/ID-bound trust boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentEventBody {
    /// Schema version (MUST be `2`).
    pub v: u64,
    /// The community this event belongs to (32-byte `CommunityId`).
    pub community_id: CommunityId,
    /// The content stream this event belongs to (required 32-byte `StreamId`).
    pub stream_id: StreamId,
    /// The authoring principal identity (32-byte `PrincipalId`). An authorization
    /// identity; NOT the signature verification key.
    pub author_id: PrincipalId,
    /// The device whose key signs this event (32-byte `DeviceId`). The ONLY key
    /// used for signature verification.
    pub device_id: DeviceId,
    /// Per-`(community_id, device_id)` sequence; the first event is `0`.
    pub device_seq: u64,
    /// The predecessor event id, or `None` (canonical CBOR `null`) for the first
    /// event (`device_seq == 0`).
    pub prev_device_event: Option<EventId>,
    /// Authorization hint cursor; strictly typed and signed, interpreted later.
    pub auth_hint_seq: u64,
    /// Creation timestamp (ms); signed data, not a trusted ordering input here.
    pub created_at_ms: u64,
    /// The registered content kind (closed registry; unknown rejects).
    pub kind: ContentKind,
    /// Ordered references; `0..=MAX_CONTENT_REFERENCES` exact-width `EventId`s.
    /// Caller-provided order is preserved (it is part of the signed bytes/ID).
    pub references: Vec<EventId>,
    /// The kind-specific content map (decoded canonical CBOR).
    pub content: CborValue,
}

impl ContentEventBody {
    /// Canonical-encode this body to the deterministic CBOR profile (a single
    /// map of the twelve §9.2 keys, `prev_device_event` emitted as `null` when
    /// `None`). Map keys are emitted in canonical order by the codec.
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            ("v".to_owned(), CborValue::Uint(self.v)),
            (
                "community_id".to_owned(),
                CborValue::Bytes(self.community_id.as_bytes().to_vec()),
            ),
            (
                "stream_id".to_owned(),
                CborValue::Bytes(self.stream_id.as_bytes().to_vec()),
            ),
            (
                "author_id".to_owned(),
                CborValue::Bytes(self.author_id.as_bytes().to_vec()),
            ),
            (
                "device_id".to_owned(),
                CborValue::Bytes(self.device_id.as_bytes().to_vec()),
            ),
            ("device_seq".to_owned(), CborValue::Uint(self.device_seq)),
            (
                "prev_device_event".to_owned(),
                match &self.prev_device_event {
                    Some(id) => CborValue::Bytes(id.as_bytes().to_vec()),
                    None => CborValue::Null,
                },
            ),
            (
                "auth_hint_seq".to_owned(),
                CborValue::Uint(self.auth_hint_seq),
            ),
            (
                "created_at_ms".to_owned(),
                CborValue::Uint(self.created_at_ms),
            ),
            (
                "kind".to_owned(),
                CborValue::Text(self.kind.as_str().to_owned()),
            ),
            (
                "references".to_owned(),
                CborValue::Array(
                    self.references
                        .iter()
                        .map(|r| CborValue::Bytes(r.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            ("content".to_owned(), self.content.clone()),
        ])
    }

    /// Canonical-encode this body to its exact canonical signed bytes (CSB).
    #[must_use]
    pub fn encode_canonical(&self) -> Vec<u8> {
        cbor::encode(&self.to_cbor())
    }

    /// Strictly decode + validate a canonically-decoded body value (spec §6.4).
    ///
    /// This performs the full strict top-level and kind-specific validation but
    /// does NOT recompute an id or verify a signature (those live in
    /// [`super::event`]). It is the body-decode half of the trust boundary.
    ///
    /// # Errors
    /// - [`Reject::NonCanonicalEncoding`] — `value` is not a map (caller misuse;
    ///   the wire decoder already rejects non-canonical CBOR).
    /// - [`Reject::UnknownVersion`] — `v` is present and not `2`.
    /// - [`Reject::UnknownContentKind`] — `kind` is not in the closed registry.
    /// - [`Reject::InvalidContent`] — unknown/missing/wrong-type/wrong-width
    ///   field, over-cap `references`, invalid intrinsic chain shape, or invalid
    ///   kind-specific content.
    pub fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        // Close the top-level key set: any key outside the twelve rejects.
        reject_unknown_top_level_keys(entries)?;

        let v = require_uint(entries, "v")?;
        if v != CONTENT_EVENT_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let community_id = require_bstr32_id(entries, "community_id", CommunityId::from_bytes)?;
        let stream_id = require_bstr32_id(entries, "stream_id", StreamId::from_bytes)?;
        let author_id = require_bstr32_id(entries, "author_id", PrincipalId::from_bytes)?;
        let device_id = require_bstr32_id(entries, "device_id", DeviceId::from_bytes)?;
        let device_seq = require_uint(entries, "device_seq")?;
        let prev_device_event = require_prev_device_event(entries, "prev_device_event")?;
        let auth_hint_seq = require_uint(entries, "auth_hint_seq")?;
        let created_at_ms = require_uint(entries, "created_at_ms")?;
        // Kind check is the FIRST content-level check (§5 sub-step 5b): an
        // unknown kind rejects before any per-kind field parsing.
        let kind = ContentKind::from_wire(require_text(entries, "kind")?)?;
        let references = require_references(entries, "references")?;
        let content = require_content_map(entries, "content")?;

        // Intrinsic per-body chain shape (spec §3.5): a genesis event
        // (device_seq == 0) MUST carry a null predecessor; a successor MUST name
        // one. This is a body invariant, independent of any supplied predecessor
        // event data.
        let prev_is_null = prev_device_event.is_none();
        if device_seq == 0 {
            if !prev_is_null {
                return Err(Reject::InvalidContent);
            }
        } else if prev_is_null {
            return Err(Reject::InvalidContent);
        }

        let body = Self {
            v,
            community_id,
            stream_id,
            author_id,
            device_id,
            device_seq,
            prev_device_event,
            auth_hint_seq,
            created_at_ms,
            kind,
            references,
            content: content.clone(),
        };
        // Kind-specific strict content validation.
        super::validate::validate_content(body.kind, content, &body.author_id)?;
        Ok(body)
    }

    /// Canonical-decode the exact `body_csb` bytes and strictly validate the
    /// resulting body. Malformed/non-canonical bytes reject as
    /// [`Reject::NonCanonicalEncoding`] before any schema check.
    ///
    /// # Errors
    /// See [`cbor::decode_canonical`] and [`Self::from_canonical`].
    pub fn decode_from_csb(csb: &[u8]) -> Result<Self, Reject> {
        let value = cbor::decode_canonical(csb)?;
        Self::from_canonical(&value)
    }
}

// ----------------------------------------------------------------------------
// Strict typed field helpers (spec §6.4 / §3.1).
//
// Unlike the provisional helpers, these distinguish absent, wrong-type, and
// wrong-width and map every schema error to `InvalidContent` (spec D5 mapping;
// gap #4 in §2.2). `v` and `kind` retain their dedicated codes.
// ----------------------------------------------------------------------------

/// Look up a required key, returning `InvalidContent` if absent.
fn lookup<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a CborValue, Reject> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or(Reject::InvalidContent)
}

fn require_uint(entries: &[(String, CborValue)], key: &str) -> Result<u64, Reject> {
    lookup(entries, key)?
        .as_uint()
        .ok_or(Reject::InvalidContent)
}

fn require_text<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a str, Reject> {
    lookup(entries, key)?
        .as_text()
        .ok_or(Reject::InvalidContent)
}

/// Read a required 32-byte identifier/key field. A present value of the wrong
/// type or wrong width rejects as `InvalidContent`; an invalid Ed25519 point is
/// not rejected here (it fails closed at signature verification).
fn require_bstr32_id<T>(
    entries: &[(String, CborValue)],
    key: &str,
    ctor: fn([u8; LEN]) -> T,
) -> Result<T, Reject> {
    let bytes = lookup(entries, key)?
        .as_bytes()
        .ok_or(Reject::InvalidContent)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
    Ok(ctor(arr))
}

/// Read `prev_device_event`: canonical `null` ⇒ `None`, otherwise an exact-width
/// `EventId`. Any other type (or wrong width) rejects as `InvalidContent`.
fn require_prev_device_event(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<Option<EventId>, Reject> {
    let v = lookup(entries, key)?;
    match v {
        CborValue::Null => Ok(None),
        CborValue::Bytes(b) => {
            let arr = <[u8; LEN]>::try_from(b.as_slice()).map_err(|_| Reject::InvalidContent)?;
            Ok(Some(EventId::from_bytes(arr)))
        }
        // Any other type is a schema error.
        _ => Err(Reject::InvalidContent),
    }
}

/// Read the `references` array: zero through `MAX_CONTENT_REFERENCES` exact-width
/// `EventId`s. A ninth element, a non-array, or a wrong-type/wrong-width entry
/// rejects as `InvalidContent`. Caller-provided order is preserved (spec §3.6/D6).
fn require_references(entries: &[(String, CborValue)], key: &str) -> Result<Vec<EventId>, Reject> {
    let arr = lookup(entries, key)?
        .as_array()
        .ok_or(Reject::InvalidContent)?;
    if arr.len() > MAX_CONTENT_REFERENCES {
        return Err(Reject::InvalidContent);
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let bytes = item.as_bytes().ok_or(Reject::InvalidContent)?;
        let id = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
        out.push(EventId::from_bytes(id));
    }
    Ok(out)
}

/// Read the `content` field; it MUST be a map. Per-kind schema validation runs
/// separately in [`super::validate::validate_content`].
fn require_content_map<'a>(
    entries: &'a [(String, CborValue)],
    key: &str,
) -> Result<&'a CborValue, Reject> {
    let v = lookup(entries, key)?;
    if v.as_map().is_none() {
        return Err(Reject::InvalidContent);
    }
    Ok(v)
}

/// Reject any top-level key outside the twelve §9.2 keys (spec §3.1: no unknown
/// field is ignored). This closes the map so signature-malleability via injected
/// keys is impossible.
fn reject_unknown_top_level_keys(entries: &[(String, CborValue)]) -> Result<(), Reject> {
    for (k, _) in entries {
        if !TOP_LEVEL_KEYS.contains(&k.as_str()) {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_body() -> ContentEventBody {
        ContentEventBody {
            v: CONTENT_EVENT_VERSION,
            community_id: CommunityId::from_bytes([0x70; LEN]),
            stream_id: StreamId::from_bytes([0x71; LEN]),
            author_id: PrincipalId::from_bytes([0xa0; LEN]),
            device_id: DeviceId::from_bytes([0xa0; LEN]),
            device_seq: 0,
            prev_device_event: None,
            auth_hint_seq: 1,
            created_at_ms: 1_000,
            kind: ContentKind::MessageText,
            references: Vec::new(),
            content: CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]),
        }
    }

    fn id_with_byte(b: u8) -> EventId {
        EventId::from_bytes([b; LEN])
    }

    #[test]
    fn round_trips_through_canonical_cbor() {
        let body = sample_body();
        let csb = body.encode_canonical();
        let back = ContentEventBody::decode_from_csb(&csb).expect("round-trip");
        assert_eq!(back, body);
        // Exact-byte identity: re-encoding the decoded body reproduces the CSB.
        assert_eq!(back.encode_canonical(), csb);
    }

    #[test]
    fn genesis_uses_canonical_null_predecessor() {
        let body = sample_body();
        let csb = body.encode_canonical();
        // The `prev_device_event` value must be the single canonical null byte.
        let value = cbor::decode_canonical(&csb).unwrap();
        assert!(matches!(
            value.get("prev_device_event"),
            Some(CborValue::Null)
        ));
    }

    #[test]
    fn wrong_version_rejects() {
        let mut body = sample_body();
        body.v = 3;
        assert_eq!(
            ContentEventBody::decode_from_csb(&body.encode_canonical()).err(),
            Some(Reject::UnknownVersion)
        );
    }

    #[test]
    fn ninth_reference_rejects() {
        let mut body = sample_body();
        body.references = (0u8..9).map(id_with_byte).collect();
        assert_eq!(
            ContentEventBody::decode_from_csb(&body.encode_canonical()).err(),
            Some(Reject::InvalidContent)
        );
        // Zero and eight references are accepted.
        for n in [0usize, 8] {
            let mut b = sample_body();
            b.references = (0u8..).take(n).map(id_with_byte).collect();
            assert!(
                ContentEventBody::decode_from_csb(&b.encode_canonical()).is_ok(),
                "{n} references must be accepted"
            );
        }
    }

    #[test]
    fn intrinsic_chain_shape_rejects_mismatch() {
        // device_seq == 0 with a named predecessor rejects.
        let mut body = sample_body();
        body.prev_device_event = Some(EventId::from_bytes([0x99; LEN]));
        assert_eq!(
            ContentEventBody::decode_from_csb(&body.encode_canonical()).err(),
            Some(Reject::InvalidContent)
        );
        // device_seq > 0 with a null predecessor rejects.
        let mut body = sample_body();
        body.device_seq = 1;
        assert_eq!(
            ContentEventBody::decode_from_csb(&body.encode_canonical()).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn unknown_top_level_key_rejects() {
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            entries.push(("bogus".to_owned(), CborValue::Uint(1)));
        }
        let csb = cbor::encode(&value);
        assert_eq!(
            ContentEventBody::decode_from_csb(&csb).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn missing_required_key_rejects() {
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            entries.retain(|(k, _)| k != "created_at_ms");
        }
        let csb = cbor::encode(&value);
        assert_eq!(
            ContentEventBody::decode_from_csb(&csb).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn wrong_width_id_rejects() {
        let mut value = sample_body().to_cbor();
        if let CborValue::Map(ref mut entries) = value {
            for (k, v) in entries.iter_mut() {
                if k == "community_id" {
                    *v = CborValue::Bytes(vec![0u8; LEN - 1]);
                }
            }
        }
        let csb = cbor::encode(&value);
        assert_eq!(
            ContentEventBody::decode_from_csb(&csb).err(),
            Some(Reject::InvalidContent)
        );
    }
}
