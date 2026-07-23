//! Strict canonical-record schema validation (spec §5.4 / §6.4, issue #146).
//!
//! A small, closed-schema validator that runs over an *already canonically
//! decoded* [`CborValue`] map. Canonicality itself (non-canonical CBOR,
//! duplicate keys, unsorted keys, non-shortest integers, trailing data, …) is
//! enforced earlier by [`crate::cbor::decode_canonical`]; this layer enforces
//! the per-record-body rules from spec §6.4 that canonical CBOR alone cannot:
//!
//! - the record body must be a map;
//! - every required key is present;
//! - the schema is closed: no keys outside `required ∪ optional`;
//! - every present field matches its declared [`FieldKind`] (and exact byte
//!   width for [`FieldKind::BytesExact`]);
//! - declared `schema_version` / record-kind discriminants match a closed set.
//!
//! The validator is deliberately tiny: it is a reusable helper for the
//! per-body `SignedBody::from_canonical` implementations, not a dynamic schema
//! framework (spec §5.4: "behavior, not a framework"). It returns the typed
//! [`Reject`] codes recommended by spec §9 so downstream layers can map
//! failures without parsing display text.

use crate::cbor::CborValue;
use crate::error::Reject;

/// The declared kind of a record-body field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Unsigned integer (CBOR major type 0).
    Uint,
    /// UTF-8 text string (CBOR major type 3).
    Text,
    /// Byte string of an exact byte width (CBOR major type 2).
    BytesExact(usize),
    /// Array (CBOR major type 4); element types are validated by the caller.
    Array,
    /// Map (CBOR major type 5); entry types are validated by the caller.
    Map,
}

impl FieldKind {
    /// Check a decoded value against this kind, returning the width failure as
    /// [`Reject::InvalidContent`] (spec §6.4 "wrong byte widths").
    fn check(self, value: &CborValue) -> Result<(), Reject> {
        match self {
            Self::Uint => value.as_uint().map(|_| ()).ok_or(Reject::InvalidContent),
            Self::Text => value.as_text().map(|_| ()).ok_or(Reject::InvalidContent),
            Self::Array => value.as_array().map(|_| ()).ok_or(Reject::InvalidContent),
            Self::Map => value.as_map().map(|_| ()).ok_or(Reject::InvalidContent),
            Self::BytesExact(expected) => {
                let bytes = value.as_bytes().ok_or(Reject::InvalidContent)?;
                if bytes.len() == expected {
                    Ok(())
                } else {
                    Err(Reject::InvalidContent)
                }
            }
        }
    }
}

/// A single required or optional field of a record body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// The canonical CBOR text key.
    pub key: &'static str,
    /// The declared kind/value shape for this field.
    pub kind: FieldKind,
}

/// A closed record-body schema (spec §5.4).
///
/// `required` keys must be present; `optional` keys may be present; any other
/// key is rejected as unknown (closed schema, spec §6.4 "unknown mandatory
/// schema/version/kind" and the closed-registry rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schema<'a> {
    /// Human-readable schema name used in nothing machine-readable today; kept
    /// for diagnostics and future error-context wiring (the pure core never
    /// logs, so this is not emitted).
    pub name: &'static str,
    /// Keys that must be present and match their declared kind.
    pub required: &'a [FieldSpec],
    /// Keys that may be absent but, if present, must match their declared kind.
    pub optional: &'a [FieldSpec],
}

impl Schema<'_> {
    /// Look up a field spec by key in `required` ∪ `optional`.
    fn find(&self, key: &str) -> Option<FieldSpec> {
        self.required
            .iter()
            .chain(self.optional.iter())
            .find(|spec| spec.key == key)
            .copied()
    }

    /// Validate a decoded record body against this closed schema (spec §6.4).
    ///
    /// # Errors
    /// - [`Reject::InvalidContent`] — the body is not a map, a required key is
    ///   missing, an unknown key is present, or a present field has the wrong
    ///   kind/width.
    ///
    /// Canonicality, signature, and id checks are performed elsewhere in the
    /// trust-boundary pipeline ([`crate::signed::verify_envelope`]); this
    /// validator concerns itself only with body-shape rules.
    pub fn validate(&self, body: &CborValue) -> Result<(), Reject> {
        let entries = body.as_map().ok_or(Reject::InvalidContent)?;

        // Closed schema: every present key must be declared.
        for (key, value) in entries {
            // Unknown key → InvalidContent. Callers that need version/kind
            // discrimination do it before calling `validate` (see
            // `require_version` / `require_kind`), so an undeclared key here is
            // a shape failure (spec §9 mapping).
            let spec = self.find(key).ok_or(Reject::InvalidContent)?;
            spec.kind.check(value)?;
        }

        // Required keys must all be present. (Duplicates are already rejected by
        // `decode_canonical`, so a single presence check suffices.)
        for spec in self.required {
            let present = entries.iter().any(|(k, _)| k == spec.key);
            if !present {
                return Err(Reject::InvalidContent);
            }
        }
        Ok(())
    }
}

/// Assert a `schema_version` (or any closed uint discriminant) field equals one
/// of the accepted values, else [`Reject::UnknownVersion`] (spec §6.4 / §9).
///
/// # Errors
/// - [`Reject::InvalidContent`] — the key is missing or not a uint.
/// - [`Reject::UnknownVersion`] — the value is not in `accepted`.
pub fn require_version(body: &CborValue, key: &str, accepted: &[u64]) -> Result<(), Reject> {
    let entries = body.as_map().ok_or(Reject::InvalidContent)?;
    let value = entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or(Reject::InvalidContent)?;
    let v = value.as_uint().ok_or(Reject::InvalidContent)?;
    if accepted.contains(&v) {
        Ok(())
    } else {
        Err(Reject::UnknownVersion)
    }
}

/// Assert a record-kind text discriminant equals one of the accepted values,
/// else [`Reject::UnknownRecordKind`] (spec §6.4 / §9).
///
/// # Errors
/// - [`Reject::InvalidContent`] — the key is missing or not text.
/// - [`Reject::UnknownRecordKind`] — the value is not in `accepted`.
pub fn require_kind(body: &CborValue, key: &str, accepted: &[&str]) -> Result<(), Reject> {
    let entries = body.as_map().ok_or(Reject::InvalidContent)?;
    let value = entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or(Reject::InvalidContent)?;
    let v = value.as_text().ok_or(Reject::InvalidContent)?;
    if accepted.contains(&v) {
        Ok(())
    } else {
        Err(Reject::UnknownRecordKind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::{self, CborValue};

    /// Build a map value from sorted (canonical-order) text-keyed entries.
    fn map(entries: &[(&str, CborValue)]) -> CborValue {
        CborValue::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    // Minimal schema used across the positive/negative cases: a 32-byte id, a
    // uint epoch, an optional text note. Closed — no other keys accepted.
    const ID: FieldSpec = FieldSpec {
        key: "id",
        kind: FieldKind::BytesExact(32),
    };
    const EPOCH: FieldSpec = FieldSpec {
        key: "epoch",
        kind: FieldKind::Uint,
    };
    const NOTE: FieldSpec = FieldSpec {
        key: "note",
        kind: FieldKind::Text,
    };
    const SCHEMA: Schema<'static> = Schema {
        name: "test-record",
        required: &[ID, EPOCH],
        optional: &[NOTE],
    };

    #[test]
    fn accepts_minimal_required_set() {
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 32])),
        ]);
        assert_eq!(SCHEMA.validate(&body), Ok(()));
    }

    #[test]
    fn accepts_required_plus_optional() {
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 32])),
            ("note", CborValue::Text("hi".to_owned())),
        ]);
        assert_eq!(SCHEMA.validate(&body), Ok(()));
    }

    #[test]
    fn rejects_non_map_body() {
        assert_eq!(
            SCHEMA.validate(&CborValue::Uint(0)),
            Err(Reject::InvalidContent)
        );
    }

    #[test]
    fn rejects_missing_required_key() {
        // `id` absent.
        let body = map(&[("epoch", CborValue::Uint(7))]);
        assert_eq!(SCHEMA.validate(&body), Err(Reject::InvalidContent));
    }

    #[test]
    fn rejects_unknown_key_closed_schema() {
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 32])),
            ("sneaky", CborValue::Uint(1)),
        ]);
        assert_eq!(SCHEMA.validate(&body), Err(Reject::InvalidContent));
    }

    #[test]
    fn rejects_wrong_byte_width() {
        // 16-byte id where 32 is required.
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 16])),
        ]);
        assert_eq!(SCHEMA.validate(&body), Err(Reject::InvalidContent));
    }

    #[test]
    fn rejects_wrong_field_kind() {
        // epoch declared Uint, supplied as text.
        let body = map(&[
            ("epoch", CborValue::Text("seven".to_owned())),
            ("id", CborValue::Bytes(vec![0u8; 32])),
        ]);
        assert_eq!(SCHEMA.validate(&body), Err(Reject::InvalidContent));
    }

    #[test]
    fn require_version_accepts_known_and_rejects_unknown() {
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 32])),
            ("schema_version", CborValue::Uint(2)),
        ]);
        assert_eq!(require_version(&body, "schema_version", &[2]), Ok(()));
        assert_eq!(
            require_version(&body, "schema_version", &[1, 3]),
            Err(Reject::UnknownVersion)
        );
    }

    #[test]
    fn require_kind_accepts_known_and_rejects_unknown() {
        let body = map(&[
            ("epoch", CborValue::Uint(7)),
            ("id", CborValue::Bytes(vec![0u8; 32])),
            ("kind", CborValue::Text("governance".to_owned())),
        ]);
        assert_eq!(
            require_kind(&body, "kind", &["governance", "content"]),
            Ok(())
        );
        assert_eq!(
            require_kind(&body, "kind", &["content"]),
            Err(Reject::UnknownRecordKind)
        );
    }

    /// Canonical CBOR already rejects duplicate keys at the decode layer; this
    /// test pins that the §6.4 duplicate-key rule is enforced *before* this
    /// schema layer ever sees the value.
    #[test]
    fn duplicate_keys_rejected_at_cbor_layer_before_schema() {
        // Hand-encode a 2-entry map with the SAME text key "id" twice. The
        // strict decoder must reject the duplicate before `Schema::validate`
        // could ever observe it (spec §6.4 / §9 → NonCanonicalEncoding).
        let one_entry = cbor::encode(&map(&[("id", CborValue::Uint(1))]));
        let mut crafted = vec![0xa2]; // map head: 2 entries
        crafted.extend_from_slice(&one_entry[1..]); // first (id,1)
        crafted.extend_from_slice(&one_entry[1..]); // duplicate (id,1)
        assert_eq!(
            cbor::decode_canonical(&crafted),
            Err(cbor::CborError::DuplicateMapKey)
        );
    }
}
