//! The PROVISIONAL v2 content-event envelope, retained ONLY to keep the frozen
//! #153 signed-record golden vectors (`tests/golden/v2-signed-records.json`) and
//! the provisional `room_lifecycle` integration test byte-stable.
//!
//! This is NOT the normative #134 §9.2 schema. The normative schema lives in
//! [`super::body`] ([`super::ContentEventBody`]) and is the single accepted v2
//! content wire format. Per spec #152 §2.2/§3.1/D1, a compatibility alias may
//! exist only if it cannot be mistaken for the normative wire schema; this type
//! is therefore deliberately renamed (`ProvisionalContentEventBody`) and lives
//! behind a clearly-marked module so it can never be decoded as normative bytes.
//!
//! It uses the legacy `schema_version`/`room_id`/`author`/`version`/`body`
//! fields and the split `CONTENT_EVENT_SIGN`/`CONTENT_EVENT_ID` candidate
//! domains. It is `publish = false` and unused by the shipped v1 runtime; it
//! will be retired when the #153 vectors migrate to the normative §9.2 schema
//! under a deliberate fixture-schema bump.

use crate::cbor::CborValue;
use crate::content::registry::ContentKind;
use crate::domain;
use crate::error::Reject;
use crate::ids::{ContentEventId, LEN};
use crate::signed::{self, Envelope, SignedBody};

/// Length in bytes of a short opaque id (`stream_id`, etc.); matches v1.
pub const SHORT_ID_LEN: usize = 16;

/// The provisional content-kind body version this crate accepts (default when
/// `version` is absent; source: §4 D2 / D5 sub-step 5c).
pub const BODY_VERSION: u64 = 1;

/// The PROVISIONAL v2 content-event envelope (pre-#152 schema).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionalContentEventBody {
    /// Schema version (MUST be `2`).
    pub schema_version: u64,
    /// The room.
    pub room_id: crate::ids::RoomId,
    /// The signing author/principal.
    pub author: crate::MemberId,
    /// The registered content kind (§6.4 discriminant).
    pub kind: ContentKind,
    /// Per-kind body version (default 1).
    pub version: u64,
    /// Optional stream scope (`bstr[16]`); absent ⇒ room default stream.
    pub stream_id: Option<[u8; SHORT_ID_LEN]>,
    /// The kind-specific body map (decoded canonical CBOR).
    pub body: CborValue,
}

impl SignedBody for ProvisionalContentEventBody {
    type Id = ContentEventId;
    const SIGN_CONTEXT: &'static [u8] = domain::CONTENT_EVENT_SIGN;
    const ID_CONTEXT: &'static [u8] = domain::CONTENT_EVENT_ID;

    fn to_cbor(&self) -> CborValue {
        let mut entries = vec![
            (
                "schema_version".to_owned(),
                CborValue::Uint(self.schema_version),
            ),
            (
                "room_id".to_owned(),
                CborValue::Bytes(self.room_id.as_bytes().to_vec()),
            ),
            (
                "author".to_owned(),
                CborValue::Bytes(self.author.as_bytes().to_vec()),
            ),
            (
                "kind".to_owned(),
                CborValue::Text(self.kind.as_str().to_owned()),
            ),
            ("version".to_owned(), CborValue::Uint(self.version)),
            ("body".to_owned(), self.body.clone()),
        ];
        if let Some(sid) = self.stream_id {
            entries.push(("stream_id".to_owned(), CborValue::Bytes(sid.to_vec())));
        }
        CborValue::Map(entries)
    }

    fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        crate::governance::model::reject_unknown_keys(
            entries,
            &[
                "schema_version",
                "room_id",
                "author",
                "kind",
                "version",
                "body",
                "stream_id",
            ],
        )?;
        let schema_version = uint_field_local(entries, "schema_version")?;
        if schema_version != crate::governance::model::SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let room_id = read_id_local(entries, "room_id")?;
        let author = read_member_local(entries, "author")?;
        // Kind check is the FIRST body-level check (§5 sub-step 5b): an unknown
        // kind is rejected before any per-kind field parsing.
        let kind = ContentKind::from_wire(text_field_local(entries, "kind")?)?;
        let version = match signed::opt(entries, "version") {
            Some(v) => v.as_uint().ok_or(Reject::NonCanonicalEncoding)?,
            None => BODY_VERSION,
        };
        if version != BODY_VERSION {
            return Err(Reject::InvalidContent);
        }
        let stream_id = match signed::opt(entries, "stream_id") {
            Some(v) => {
                let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                let arr =
                    <[u8; SHORT_ID_LEN]>::try_from(bytes).map_err(|_| Reject::InvalidContent)?;
                Some(arr)
            }
            None => None,
        };
        let body = signed::field(entries, "body")
            .filter(|v| v.as_map().is_some())
            .ok_or(Reject::NonCanonicalEncoding)?
            .clone();
        Ok(Self {
            schema_version,
            room_id,
            author,
            kind,
            version,
            stream_id,
            body,
        })
    }

    fn id_from_csb(csb: &[u8]) -> Self::Id {
        ContentEventId::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
    }
}

/// The parsed content body kind, pairing the discriminant with its decoded body.
pub type ProvisionalContentBodyKind = ContentKind;

/// A signed content-event envelope (provisional schema).
pub type SignedProvisionalContentEvent = Envelope<ContentEventId>;

/// Decode + verify a signed provisional content event end-to-end, including
/// body-only validation (spec D2 / #152 provisional path).
///
/// # Errors
/// See [`signed::verify_envelope`] and [`super::validate::validate_body`].
pub fn decode_verified(
    env: &SignedProvisionalContentEvent,
) -> Result<ProvisionalContentEventBody, Reject> {
    let body = signed::verify_envelope::<ProvisionalContentEventBody>(env)?;
    crate::content::validate::validate_body(&body)?;
    Ok(body)
}

fn uint_field_local(entries: &[(String, CborValue)], key: &str) -> Result<u64, Reject> {
    signed::field(entries, key)
        .and_then(super::super::cbor::CborValue::as_uint)
        .ok_or(Reject::NonCanonicalEncoding)
}

fn text_field_local<'a>(entries: &'a [(String, CborValue)], key: &str) -> Result<&'a str, Reject> {
    signed::field(entries, key)
        .and_then(|v| v.as_text())
        .ok_or(Reject::NonCanonicalEncoding)
}

fn read_id_local(entries: &[(String, CborValue)], key: &str) -> Result<crate::ids::RoomId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(crate::ids::RoomId::from_bytes(arr))
}

fn read_member_local(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<crate::MemberId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(crate::MemberId::from_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RoomId;
    use crate::keys::SigningKey;

    fn text_body(body: &str) -> CborValue {
        CborValue::Map(vec![("body".to_owned(), CborValue::Text(body.to_owned()))])
    }

    #[test]
    fn content_event_seal_verify_round_trip() {
        let key = SigningKey::from_seed(&[0x33; LEN]);
        let body = ProvisionalContentEventBody {
            schema_version: 2,
            room_id: RoomId::from_bytes([0x50; LEN]),
            author: key.member_id(),
            kind: ContentKind::MessageText,
            version: 1,
            stream_id: None,
            body: text_body("hello"),
        };
        let env = signed::seal(&body, &key);
        let decoded = decode_verified(&env).expect("valid content event verifies");
        assert_eq!(decoded, body);
    }

    #[test]
    fn unknown_kind_rejected_at_envelope_decode() {
        let key = SigningKey::from_seed(&[0x34; LEN]);
        // Hand-build a body with an unknown kind via raw CBOR.
        let raw = CborValue::Map(vec![
            ("schema_version".to_owned(), CborValue::Uint(2)),
            ("room_id".to_owned(), CborValue::Bytes(vec![0x50; LEN])),
            (
                "author".to_owned(),
                CborValue::Bytes(key.member_id().as_bytes().to_vec()),
            ),
            (
                "kind".to_owned(),
                CborValue::Text("message.unknown".to_owned()),
            ),
            ("version".to_owned(), CborValue::Uint(1)),
            ("body".to_owned(), CborValue::Map(vec![])),
        ]);
        let csb = crate::cbor::encode(&raw);
        // Sign using a known context just to produce a verifiable signature shape;
        // the kind check happens before any signature-domain concern at decode.
        let sig = key.sign(&domain::signing_message(
            ProvisionalContentEventBody::SIGN_CONTEXT,
            &csb,
        ));
        let env = Envelope {
            id: ContentEventId::from_bytes(domain::blake3_domain(
                ProvisionalContentEventBody::ID_CONTEXT,
                &csb,
            )),
            signed: csb,
            sig,
            signer: key.member_id(),
        };
        assert_eq!(
            decode_verified(&env).err(),
            Some(Reject::UnknownContentKind)
        );
    }
}
