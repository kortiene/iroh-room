//! Frozen golden vectors for the #134 §6.3 v2 identifiers and the §6.2 frozen
//! domain-separation strings (issue #146).
//!
//! Each vector fixes a deterministic, public preimage (canonical-CBOR bytes of
//! a small descriptor), then asserts the full round-trip:
//!
//! 1. the fixture domain string equals the `domain.rs` constant byte-for-byte;
//! 2. the preimage bytes equal the typed-preimage builder output;
//! 3. `BLAKE3(domain || preimage)` equals the frozen `digest_hex`;
//! 4. the typed identifier wraps that exact digest;
//! 5. the typed identifier's display string equals the frozen `display`;
//! 6. parsing the display string returns the same typed identifier;
//! 7. recomputing under a *different* frozen domain yields a different digest
//!    (the domain-separation fence has teeth).
//!
//! Plus one negative canonical-CBOR vector: a non-canonical record (duplicate
//! map keys) is rejected at the CBOR layer before any identifier/schema work,
//! surfacing as `Reject::NonCanonicalEncoding`.
//!
//! Changing any frozen value below requires a fixture schema bump (see
//! `golden/README.md`). All preimages are deterministic public test data; no
//! secrets, identities, or network material are involved.

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::{self, CborError, CborValue};
use iroh_rooms_v2_core::domain::{self, ALL_V2_DOMAINS};
use iroh_rooms_v2_core::ids::{
    CheckpointId, CommunityId, EventId, GovernanceId, ReplicaId, StreamId,
};
use iroh_rooms_v2_core::schema::{FieldKind, FieldSpec, Schema};
use iroh_rooms_v2_core::Reject;

// Pulled in so a missing/malformed fixture fails the build.
const GOLDEN_JSON: &str = include_str!("golden/v2-identifiers.json");

// ============================================================================
// Frozen values — mirror of `golden/v2-identifiers.json`. ANY change here
// requires a schema-version bump (see `golden/README.md`).
// ============================================================================

// CommunityId: canonical CBOR of `{"epoch": 1, "name": "golden-community"}`.
const COMMUNITY_PREIMAGE_HEX: &str = "a2646e616d6570676f6c64656e2d636f6d6d756e6974796565706f636801";
const COMMUNITY_DIGEST_HEX: &str =
    "e593db90754f6d24cdfc13f59c0b2a709d541aecf5d62c05d8fd14bc9e6f5a4a";
const COMMUNITY_DISPLAY: &str =
    "blake3:e593db90754f6d24cdfc13f59c0b2a709d541aecf5d62c05d8fd14bc9e6f5a4a";
// Same preimage under a different frozen domain (governance-entry) — must differ.
const COMMUNITY_WRONGDOMAIN_HEX: &str =
    "024925d9375f8b8b35ab44c6dd269f2e7e64858e9c8739918f6781de6c62ef3b";

// GovernanceId: canonical CBOR of `{"epoch": 1000, "kind": "init_room", "seq": 1}`.
const GOVERNANCE_PREIMAGE_HEX: &str =
    "a36373657101646b696e6469696e69745f726f6f6d6565706f63681903e8";
const GOVERNANCE_DIGEST_HEX: &str =
    "518ce110d11f63da61cc335705e2063e0185f50ed20a5b5398925defb5bff8d9";
const GOVERNANCE_DISPLAY: &str =
    "blake3:518ce110d11f63da61cc335705e2063e0185f50ed20a5b5398925defb5bff8d9";

// StreamId: canonical CBOR of `{"name": "golden-stream", "seq": 1}`.
const STREAM_PREIMAGE_HEX: &str = "a26373657101646e616d656d676f6c64656e2d73747265616d";
const STREAM_DIGEST_HEX: &str = "78559e86ff09920b1d07cc4dcaa28d9b613d08df6c4f96d8b6e417d6fdf8ec60";
const STREAM_DISPLAY: &str =
    "blake3:78559e86ff09920b1d07cc4dcaa28d9b613d08df6c4f96d8b6e417d6fdf8ec60";

// EventId: canonical CBOR of `{"body": "hello golden v2", "kind": "message.text", "seq": 1}`.
const EVENT_PREIMAGE_HEX: &str =
    "a3637365710164626f64796f68656c6c6f20676f6c64656e207632646b696e646c6d6573736167652e74657874";
const EVENT_DIGEST_HEX: &str = "14ff4a5b86dbd7f13b6e40b6892eb14eeeeb55f3f721120647e2c1b4da10077c";
const EVENT_DISPLAY: &str =
    "blake3:14ff4a5b86dbd7f13b6e40b6892eb14eeeeb55f3f721120647e2c1b4da10077c";

// CheckpointId: canonical CBOR of `{"epoch": 2000, "seq": 1}`. Both kinds pin.
const CHECKPOINT_PREIMAGE_HEX: &str = "a263736571016565706f63681907d0";
const CHECKPOINT_GOV_DIGEST_HEX: &str =
    "9b14d6abe5645f6f3476a384ee9679f3ac0411c0dcb586a8a3345a0505aef138";
const CHECKPOINT_GOV_DISPLAY: &str =
    "blake3:9b14d6abe5645f6f3476a384ee9679f3ac0411c0dcb586a8a3345a0505aef138";
const CHECKPOINT_STREAM_DIGEST_HEX: &str =
    "c18eef16358a6058cf702314deeca21da045126589c520ac54a0601333f3a331";
const CHECKPOINT_STREAM_DISPLAY: &str =
    "blake3:c18eef16358a6058cf702314deeca21da045126589c520ac54a0601333f3a331";

// ReplicaId: canonical CBOR of `{"kind": "replica", "seq": 1}`.
const REPLICA_PREIMAGE_HEX: &str = "a26373657101646b696e64677265706c696361";
const REPLICA_DIGEST_HEX: &str = "0e389a8f70f6c6035cad205ca4f3bd680b7ea3e394a3b342f412b8bfe52944b0";
const REPLICA_DISPLAY: &str =
    "blake3:0e389a8f70f6c6035cad205ca4f3bd680b7ea3e394a3b342f412b8bfe52944b0";

// A frozen, one-fault non-canonical CBOR record: a 2-entry map with a
// duplicate text key "id" (`a2` map(2), `62 6964 01`, `62 6964 01`).
const NONCANONICAL_DUPLICATE_KEY_HEX: &str = "a26269640162696401";

// ============================================================================
// Helpers
// ============================================================================

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("frozen fixture hex must be valid lowercase")
}

/// Decode a frozen preimage, assert it is canonical CBOR that re-encodes to the
/// exact frozen bytes, and return the canonical bytes (the typed-preimage form).
fn frozen_preimage(hex: &str) -> Vec<u8> {
    let bytes = hx(hex);
    let value = cbor::decode_canonical(&bytes).expect("frozen preimage is canonical CBOR");
    assert_eq!(
        cbor::encode(&value),
        bytes,
        "preimage round-trip failed: encode(decode({hex})) != {hex}"
    );
    bytes
}

// ============================================================================
// §1 Domain-string fence — every frozen domain equals the fixture + constant.
// ============================================================================

#[test]
fn all_frozen_domains_match_fixture_and_constant() {
    for (name, constant) in ALL_V2_DOMAINS {
        let value_str = std::str::from_utf8(constant).expect("domain constants are ASCII");
        let pair = format!("\"{name}\": \"{value_str}\"");
        assert!(
            GOLDEN_JSON.contains(&pair),
            "frozen domain {name} constant ({value_str}) does not mirror the fixture"
        );
    }
    assert_eq!(ALL_V2_DOMAINS.len(), 11, "#134 §6.2 freezes eleven domains");
}

// ============================================================================
// §2 Identifier golden vectors — preimage → digest → display → parse.
// ============================================================================

#[test]
fn community_id_golden_round_trip() {
    let preimage = frozen_preimage(COMMUNITY_PREIMAGE_HEX);
    let id = CommunityId::derive(&preimage);
    // (1,4) typed identifier wraps the frozen digest.
    assert_eq!(hex::encode(id.as_bytes()), COMMUNITY_DIGEST_HEX);
    // (2) independent recompute from raw bytes via the low-level helper.
    let recomputed = domain::blake3_domain(domain::COMMUNITY, &preimage);
    assert_eq!(hex::encode(recomputed), COMMUNITY_DIGEST_HEX);
    // (3,5) display string + (6) parse round-trip.
    assert_eq!(id.to_string(), COMMUNITY_DISPLAY);
    let parsed: CommunityId = COMMUNITY_DISPLAY.parse().unwrap();
    assert_eq!(parsed.as_bytes(), id.as_bytes());
    // (7) wrong-domain divergence: same preimage under GOVERNANCE_ENTRY differs.
    let wrong = domain::blake3_domain(domain::GOVERNANCE_ENTRY, &preimage);
    assert_eq!(hex::encode(wrong), COMMUNITY_WRONGDOMAIN_HEX);
    assert_ne!(wrong, *id.as_bytes());
}

#[test]
fn governance_id_golden_round_trip() {
    let preimage = frozen_preimage(GOVERNANCE_PREIMAGE_HEX);
    let id = GovernanceId::from_governance_entry_csb(&preimage);
    assert_eq!(hex::encode(id.as_bytes()), GOVERNANCE_DIGEST_HEX);
    assert_eq!(
        hex::encode(domain::blake3_domain(domain::GOVERNANCE_ENTRY, &preimage)),
        GOVERNANCE_DIGEST_HEX
    );
    assert_eq!(id.to_string(), GOVERNANCE_DISPLAY);
    let parsed: GovernanceId = GOVERNANCE_DISPLAY.parse().unwrap();
    assert_eq!(parsed.as_bytes(), id.as_bytes());
}

#[test]
fn stream_id_golden_round_trip() {
    let preimage = frozen_preimage(STREAM_PREIMAGE_HEX);
    let id = StreamId::from_stream_descriptor_csb(&preimage);
    assert_eq!(hex::encode(id.as_bytes()), STREAM_DIGEST_HEX);
    assert_eq!(
        hex::encode(domain::blake3_domain(domain::CONTENT_EVENT, &preimage)),
        STREAM_DIGEST_HEX
    );
    assert_eq!(id.to_string(), STREAM_DISPLAY);
    let parsed: StreamId = STREAM_DISPLAY.parse().unwrap();
    assert_eq!(parsed.as_bytes(), id.as_bytes());
}

#[test]
fn event_id_golden_round_trip() {
    let preimage = frozen_preimage(EVENT_PREIMAGE_HEX);
    let id = EventId::from_content_event_csb(&preimage);
    assert_eq!(hex::encode(id.as_bytes()), EVENT_DIGEST_HEX);
    assert_eq!(
        hex::encode(domain::blake3_domain(domain::CONTENT_EVENT, &preimage)),
        EVENT_DIGEST_HEX
    );
    assert_eq!(id.to_string(), EVENT_DISPLAY);
    let parsed: EventId = EVENT_DISPLAY.parse().unwrap();
    assert_eq!(parsed.as_bytes(), id.as_bytes());
}

#[test]
fn checkpoint_id_golden_round_trip_both_kinds() {
    let preimage = frozen_preimage(CHECKPOINT_PREIMAGE_HEX);
    let gov = CheckpointId::from_governance_checkpoint_csb(&preimage);
    let stream = CheckpointId::from_stream_checkpoint_csb(&preimage);
    assert_eq!(hex::encode(gov.as_bytes()), CHECKPOINT_GOV_DIGEST_HEX);
    assert_eq!(gov.to_string(), CHECKPOINT_GOV_DISPLAY);
    assert_eq!(
        hex::encode(domain::blake3_domain(
            domain::GOVERNANCE_CHECKPOINT,
            &preimage
        )),
        CHECKPOINT_GOV_DIGEST_HEX
    );
    let gov_parsed: CheckpointId = CHECKPOINT_GOV_DISPLAY.parse().unwrap();
    assert_eq!(gov_parsed.as_bytes(), gov.as_bytes());

    assert_eq!(hex::encode(stream.as_bytes()), CHECKPOINT_STREAM_DIGEST_HEX);
    assert_eq!(stream.to_string(), CHECKPOINT_STREAM_DISPLAY);
    assert_eq!(
        hex::encode(domain::blake3_domain(domain::STREAM_CHECKPOINT, &preimage)),
        CHECKPOINT_STREAM_DIGEST_HEX
    );
    let stream_parsed: CheckpointId = CHECKPOINT_STREAM_DISPLAY.parse().unwrap();
    assert_eq!(stream_parsed.as_bytes(), stream.as_bytes());

    // The two pinned domains must produce distinct digests.
    assert_ne!(gov.as_bytes(), stream.as_bytes());
}

#[test]
fn replica_id_golden_round_trip() {
    let preimage = frozen_preimage(REPLICA_PREIMAGE_HEX);
    let id = ReplicaId::from_replica_descriptor_csb(&preimage);
    assert_eq!(hex::encode(id.as_bytes()), REPLICA_DIGEST_HEX);
    assert_eq!(
        hex::encode(domain::blake3_domain(domain::REPLICA_RECEIPT, &preimage)),
        REPLICA_DIGEST_HEX
    );
    assert_eq!(id.to_string(), REPLICA_DISPLAY);
    let parsed: ReplicaId = REPLICA_DISPLAY.parse().unwrap();
    assert_eq!(parsed.as_bytes(), id.as_bytes());
}

// ============================================================================
// §3 Negative vector — non-canonical CBOR record is rejected (spec §6.4).
// ============================================================================

#[test]
fn non_canonical_cbor_record_is_rejected() {
    // A 2-entry map with the SAME text key "id" twice. The strict decoder
    // rejects the duplicate before any identifier/schema work. This is the
    // golden negative vector for `Reject::NonCanonicalEncoding`.
    let bytes = hx(NONCANONICAL_DUPLICATE_KEY_HEX);
    let err = cbor::decode_canonical(&bytes).unwrap_err();
    assert_eq!(err, CborError::DuplicateMapKey);
    // The crate's `Reject` taxonomy maps any CborError to NonCanonicalEncoding.
    let reject: Reject = err.into();
    assert_eq!(reject, Reject::NonCanonicalEncoding);
    assert_eq!(reject.code(), "non_canonical_encoding");
}

/// Extend the §6.4 "reject non-canonical" acceptance beyond the single
/// duplicate-key vector: each distinct fault class must decode to the specific
/// `CborError` *and* fold to `Reject::NonCanonicalEncoding` through the public
/// boundary. One byte-pinned vector per fault so a drift names the culprit.
#[test]
fn non_canonical_cbor_fault_classes_all_reject() {
    // (hex, expected CborError). Each is a one-fault, hand-encoded item.
    let cases: &[(&str, CborError)] = &[
        // Trailing byte after a complete top-level item (`00` then `00`).
        ("0000", CborError::TrailingData),
        // Non-shortest uint: 24 with a value <= 23 (`18 17`, i.e. 23 in 2 bytes).
        ("1817", CborError::NonShortestInt),
        // Negative integer (major type 1) — outside the closed profile.
        ("20", CborError::NegativeInteger),
        // CBOR tag (major type 6) — disallowed.
        ("c000", CborError::Tag),
        // Float / simple value (major type 7) — disallowed.
        ("f4", CborError::FloatOrSimple),
        // Indefinite-length array head.
        ("9f", CborError::IndefiniteLength),
        // Map with an integer (non-text) key: `a1 00 00`.
        ("a10000", CborError::NonTextMapKey),
        // Map keys not in canonical ascending order: keys "b" then "a".
        ("a2616200616100", CborError::UnsortedMapKey),
        // Truncated byte string: header claims 3 bytes, only 2 follow.
        ("430102", CborError::UnexpectedEof),
    ];
    for (hex, expected) in cases {
        let err = cbor::decode_canonical(&hx(hex)).unwrap_err();
        assert_eq!(err, *expected, "fault vector {hex}");
        // Every CborError folds to the single non-canonical rejection code.
        let reject: Reject = err.into();
        assert_eq!(reject, Reject::NonCanonicalEncoding, "fault vector {hex}");
        assert_eq!(reject.code(), "non_canonical_encoding");
    }
}

// ============================================================================
// §3b Domain-separation fence — every frozen domain is mutually distinct.
// ============================================================================

/// The whole point of §6.2 is that a hash/signature valid under one domain can
/// never be replayed under another. The existing golden vectors pin one pair
/// (community vs governance-entry); this proves the property holds for the FULL
/// frozen set: hashing one common preimage under all eleven domains yields
/// eleven distinct digests.
///
/// Note: `content-event` legitimately appears for both `StreamId` and `EventId`
/// (documented OQ-1 assumption), but that is one *domain* used by two id types,
/// not two domains — `ALL_V2_DOMAINS` still holds eleven unique strings, so
/// their digests are all distinct.
#[test]
fn all_frozen_domains_produce_distinct_digests() {
    let preimage = b"one-common-preimage-across-all-domains";
    let mut digests: Vec<[u8; 32]> = Vec::new();
    for (name, dom) in ALL_V2_DOMAINS {
        let digest = domain::blake3_domain(dom, preimage);
        assert!(
            !digests.contains(&digest),
            "domain {name} collided with an earlier frozen domain on the shared preimage"
        );
        digests.push(digest);
    }
    assert_eq!(digests.len(), 11, "#134 §6.2 freezes eleven domains");
}

/// Regression fence for the documented OQ-1 design choice: `StreamId` and
/// `EventId` intentionally derive under the SAME `content-event` domain, so an
/// identical preimage yields identical raw digests. If a future change gives
/// streams a dedicated domain (as the OQ-1 note anticipates), this test must be
/// updated in lockstep with new golden vectors — it exists so that change is
/// never silent.
#[test]
fn stream_and_event_ids_share_content_event_domain_by_design() {
    let preimage = b"shared-content-event-preimage";
    let stream = StreamId::from_stream_descriptor_csb(preimage);
    let event = EventId::from_content_event_csb(preimage);
    assert_eq!(
        stream.as_bytes(),
        event.as_bytes(),
        "OQ-1: stream and event ids share the content-event domain today"
    );
    // Both equal the manual derivation under the single shared frozen domain.
    let manual = domain::blake3_domain(domain::CONTENT_EVENT, preimage);
    assert_eq!(stream.as_bytes(), &manual);
    assert_eq!(event.as_bytes(), &manual);
}

// ============================================================================
// §4 §6.4 rule coverage via the schema validator — one fault per vector.
// ============================================================================

const SCHEMA_ID: FieldSpec = FieldSpec {
    key: "id",
    kind: FieldKind::BytesExact(32),
};
const SCHEMA_EPOCH: FieldSpec = FieldSpec {
    key: "epoch",
    kind: FieldKind::Uint,
};
const SCHEMA_VERSION: FieldSpec = FieldSpec {
    key: "schema_version",
    kind: FieldKind::Uint,
};
const RECORD_SCHEMA: Schema<'static> = Schema {
    name: "v2-record",
    required: &[SCHEMA_ID, SCHEMA_EPOCH],
    optional: &[SCHEMA_VERSION],
};

fn map(pairs: &[(&str, CborValue)]) -> CborValue {
    CborValue::Map(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

#[test]
fn negative_missing_required_key() {
    // `id` absent → only `epoch` present.
    let body = map(&[("epoch", CborValue::Uint(1))]);
    assert_eq!(RECORD_SCHEMA.validate(&body), Err(Reject::InvalidContent));
}

#[test]
fn negative_wrong_width_id_field() {
    // 16-byte id where 32 is required.
    let body = map(&[
        ("epoch", CborValue::Uint(1)),
        ("id", CborValue::Bytes(vec![0u8; 16])),
    ]);
    assert_eq!(RECORD_SCHEMA.validate(&body), Err(Reject::InvalidContent));
}

#[test]
fn negative_unknown_mandatory_schema_version() {
    let body = map(&[
        ("epoch", CborValue::Uint(1)),
        ("id", CborValue::Bytes(vec![0u8; 32])),
        ("schema_version", CborValue::Uint(99)),
    ]);
    // The body shape is valid, but the declared schema_version is unknown.
    assert_eq!(RECORD_SCHEMA.validate(&body), Ok(()));
    assert_eq!(
        iroh_rooms_v2_core::schema::require_version(&body, "schema_version", &[1, 2]),
        Err(Reject::UnknownVersion)
    );
}

// ============================================================================
// §5 Frozen-metadata fence — the fixture carries the frozen markers and mirrors
// every frozen hex value above (spec §5 Step 8).
// ============================================================================

#[test]
fn fixture_carries_frozen_markers_and_mirrors_constants() {
    assert!(
        GOLDEN_JSON.contains("\"schema\": \"iroh-room-v2-identifiers/v1\""),
        "v2-identifiers fixture missing schema marker"
    );
    assert!(
        GOLDEN_JSON.contains("\"frozen\": true"),
        "v2-identifiers fixture missing frozen=true marker"
    );
    assert!(
        GOLDEN_JSON.contains("\"requires_schema_bump_on_change\": true"),
        "v2-identifiers fixture missing requires_schema_bump_on_change marker"
    );
    for hex_value in [
        COMMUNITY_PREIMAGE_HEX,
        COMMUNITY_DIGEST_HEX,
        COMMUNITY_DISPLAY,
        GOVERNANCE_PREIMAGE_HEX,
        GOVERNANCE_DIGEST_HEX,
        GOVERNANCE_DISPLAY,
        STREAM_PREIMAGE_HEX,
        STREAM_DIGEST_HEX,
        STREAM_DISPLAY,
        EVENT_PREIMAGE_HEX,
        EVENT_DIGEST_HEX,
        EVENT_DISPLAY,
        CHECKPOINT_PREIMAGE_HEX,
        CHECKPOINT_GOV_DIGEST_HEX,
        CHECKPOINT_GOV_DISPLAY,
        CHECKPOINT_STREAM_DIGEST_HEX,
        CHECKPOINT_STREAM_DISPLAY,
        REPLICA_PREIMAGE_HEX,
        REPLICA_DIGEST_HEX,
        REPLICA_DISPLAY,
        COMMUNITY_WRONGDOMAIN_HEX,
        NONCANONICAL_DUPLICATE_KEY_HEX,
    ] {
        assert!(
            GOLDEN_JSON.contains(hex_value),
            "frozen hex {hex_value} is in the Rust constants but missing from the JSON fixture — they must mirror"
        );
    }
}
