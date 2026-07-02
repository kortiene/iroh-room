//! Property-based robustness tests for the strict CBOR reader and the
//! `validate_wire_bytes` pipeline (spec
//! `strict-cbor-reader-unit-property-fuzz-tests.md` §6, risk R1). Complements
//! the direct unit tests in `cbor.rs`'s inline `#[cfg(test)] mod tests`.

use iroh_rooms_core::event::build_room_created;
use iroh_rooms_core::event::cbor::{self, CborValue};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// A bounded, recursive `CborValue` strategy: depth <= 8, collection sizes 0..8.
// ---------------------------------------------------------------------------

fn cbor_value_strategy() -> impl Strategy<Value = CborValue> {
    let leaf = prop_oneof![
        any::<u64>().prop_map(CborValue::Uint),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(CborValue::Bytes),
        "[a-zA-Z0-9]{0,8}".prop_map(CborValue::Text),
    ];
    leaf.prop_recursive(8, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(CborValue::Array),
            // A `BTreeMap` key collection yields unique keys, so re-encoding is a
            // clean round-trip with no duplicate-key ambiguity.
            prop::collection::btree_map("[a-zA-Z0-9]{0,8}", inner, 0..8)
                .prop_map(|m| CborValue::Map(m.into_iter().collect())),
        ]
    })
}

/// Canonical key order: length-first, then bytewise — mirrors `cbor::encode`'s
/// internal `encoded_key` comparison.
fn canonical_key_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// Recursively normalize a generated value into the form `decode_canonical`
/// would produce: map entries sorted into canonical key order. The generator
/// already yields unique keys (via `BTreeMap`), so this is a pure re-ordering.
fn canonicalize(value: &CborValue) -> CborValue {
    match value {
        CborValue::Uint(_) | CborValue::Bytes(_) | CborValue::Text(_) => value.clone(),
        CborValue::Array(items) => CborValue::Array(items.iter().map(canonicalize).collect()),
        CborValue::Map(entries) => {
            let mut sorted: Vec<(String, CborValue)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            sorted.sort_by(|a, b| canonical_key_order(&a.0, &b.0));
            CborValue::Map(sorted)
        }
    }
}

/// A valid, signed genesis `WireEvent` and the `RoomId` its own fields derive to
/// — so `ValidationContext::for_room(room_id)` accepts it. Returning the room id
/// lets the tamper-evidence test use a *genuinely valid* baseline, so a
/// rejection there is caused by the mutation, not by a mismatched room context.
fn valid_wire_event() -> (Vec<u8>, RoomId) {
    let identity = SigningKey::from_seed(&[0x11; 32]);
    let device = SigningKey::from_seed(&[0x22; 32]);
    let nonce = [0x33u8; 16];
    let created_at = 1_750_000_000_000;
    let wire = build_room_created(&identity, &device, "Property Test Room", &nonce, created_at);
    let room_id = signed::derive_room_id(&identity.identity_key(), &nonce, created_at);
    (wire.to_bytes(), room_id)
}

/// A valid, signed genesis `WireEvent`'s bytes, for the P4 mutation property.
fn valid_wire_bytes() -> Vec<u8> {
    valid_wire_event().0
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// P1 (decoder half) — robustness / no panic (spec §10.3's guarantee):
    /// `decode_canonical` must return `Ok | Err(CborError)` for every input,
    /// never panic, over arbitrary bytes.
    #[test]
    fn decode_canonical_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = cbor::decode_canonical(&bytes);
    }

    /// P2 — canonical round-trip (the R1 invariant): `encode` output re-decodes
    /// to the canonicalized value, and re-encoding that reproduces the same bytes.
    #[test]
    fn encode_decode_round_trips_to_canonical_form(value in cbor_value_strategy()) {
        let bytes = cbor::encode(&value);
        let decoded = cbor::decode_canonical(&bytes)
            .expect("encoder output must always be strictly decodable");
        prop_assert_eq!(&decoded, &canonicalize(&value));
        prop_assert_eq!(cbor::encode(&decoded), bytes);
    }

    /// P3 — encoder is a subset of the strict reader: `encode` can never emit
    /// bytes the strict reader rejects.
    #[test]
    fn encoder_output_is_always_accepted(value in cbor_value_strategy()) {
        let bytes = cbor::encode(&value);
        prop_assert!(cbor::decode_canonical(&bytes).is_ok());
    }
}

proptest! {
    // The full pipeline runs Ed25519 verification per case; keep the budget
    // well within a normal `cargo test` run (D2 / Rk4).
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// P1 (pipeline half) — `validate_wire_bytes` must return `Ok |
    /// Err(RejectReason)` for every input, never panic, over arbitrary bytes.
    #[test]
    fn validate_wire_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
        let _ = validate_wire_bytes(&bytes, &ctx);
    }

    /// P4 — mutation stays typed: flipping any single byte of a valid signed
    /// `WireEvent` must keep `validate_wire_bytes` panic-free and typed. (The
    /// weaker "no panic + typed" invariant is the gate here, per spec Open Q5 —
    /// it holds unconditionally, unlike a per-mutation `Err`-class assertion.)
    #[test]
    fn single_byte_mutation_of_valid_event_stays_typed(
        idx in 0..valid_wire_bytes().len(),
        xor in 1u8..=255,
    ) {
        let mut bytes = valid_wire_bytes();
        bytes[idx] ^= xor;
        let ctx = ValidationContext::for_room(RoomId::from_bytes([0u8; 32]));
        let _ = validate_wire_bytes(&bytes, &ctx);
    }
}

/// P4 (strengthened) — exhaustive tamper-evidence (spec Open Q5's stronger `Err`
/// invariant). A *genuinely valid* signed `WireEvent` validates, and flipping
/// ANY single bit of it is rejected by `validate_wire_bytes` — never accepted,
/// never a panic.
///
/// This is deterministic and covers every bit position (unlike the sampled P4
/// proptest above, which runs against a mismatched room context so its baseline
/// never fully validates). Every one-bit change is provably rejected: it either
/// breaks the canonical CBOR envelope (`NonCanonicalEncoding`), alters the
/// signed bytes so the recomputed id no longer matches (`IdMismatch`), corrupts
/// the advisory id string (`IdMismatch`), or corrupts the signature
/// (`BadSignature`). No single-bit change can forge a BLAKE3 id collision plus a
/// valid Ed25519 signature, so acceptance is impossible.
#[test]
fn every_single_bit_flip_of_valid_event_is_rejected() {
    let (original, room_id) = valid_wire_event();
    let ctx = ValidationContext::for_room(room_id);

    // The pristine event must validate — otherwise "every flip is rejected"
    // would be vacuous (an already-invalid baseline rejects trivially).
    assert!(
        validate_wire_bytes(&original, &ctx).is_ok(),
        "baseline fixture must be a genuinely valid event"
    );

    for (byte_idx, &orig) in original.iter().enumerate() {
        for bit in 0..8u8 {
            let mut mutated = original.clone();
            mutated[byte_idx] = orig ^ (1u8 << bit);
            assert!(
                validate_wire_bytes(&mutated, &ctx).is_err(),
                "single-bit tamper at byte {byte_idx} bit {bit} must be rejected"
            );
        }
    }
}
