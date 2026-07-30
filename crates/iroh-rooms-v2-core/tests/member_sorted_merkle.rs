//! Focused golden-vector + property tests for the #151 sorted member Merkle
//! tree (spec `v2-member-projection-sorted-merkle-map.md`).
//!
//! These tests are the frozen-root correctness gate for #151. They load
//! `golden/v2-member-merkle.json` (so a missing fixture fails the build) and
//! assert the implementation reproduces every frozen byte:
//!
//! - the 0/1/2/3-leaf roots (independently recomputed with the `blake3-ref`
//!   tool, NOT derived from this implementation);
//! - the exact leaf/intermediate hashes and canonical record bytes;
//! - the 3-leaf odd-node "promote unchanged" rule (root differs from the
//!   duplicate-last interpretation);
//! - the deterministic 10,000-leaf generator's frozen root.
//!
//! Plus the issue's behavioural acceptance: inclusion proof verifies for a
//! member, returns nothing for a non-member, rejects a rebound proof; and
//! incremental insert/replace/remove reproduces the full-build oracle.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use iroh_rooms_v2_core::cbor::{decode_canonical, encode, CborValue};
use iroh_rooms_v2_core::ids::{PrincipalId, LEN};
use iroh_rooms_v2_core::member::projected::{
    MemberMapProjection, ProjectedMemberRecord, ProjectedStatus, RoleLabel,
};
use iroh_rooms_v2_core::member::sorted::{
    decode_inclusion_proof, member_leaf_hash, merkle_node_hash, rebuild_root, verify_inclusion,
    InclusionProof, ProofStep, SiblingSide, SortedMerkleMap, MAX_LEVELS,
};
use iroh_rooms_v2_core::Reject;
use proptest::prelude::*;

const GOLDEN_JSON: &str = include_str!("golden/v2-member-merkle.json");

// ============================================================================
// Frozen values — mirror of `golden/v2-member-merkle.json`. ANY change here
// requires a schema-version bump (see `golden/README.md`).
// ============================================================================

const EMPTY_ROOT_HEX: &str = "083e5ab3457434652f2cb70c33aa5e671d1a56ea0117bf46471fb64434208057";
const ONE_LEAF_ROOT_HEX: &str = "6ecdad13ec133fa2d026b5ed88e2cab04054d3415226e2b145b55540e9609d22";
const TWO_LEAF_ROOT_HEX: &str = "72be3ac1b430ccfe99a237fa470d202017c5a46a335463038ba2252611bfbbf5";
const THREE_LEAF_ROOT_HEX: &str =
    "4f86836e0bdf54299eff4037da8f80f71d2abf2fdcd4227045e11d14b5a40da9";
const THREE_LEAF_DUP_LAST_HEX: &str =
    "e2143e3d7807a47007488867d53e529c7b0dd48dbbef3c61956041c48ebc59a3";
const TEN_K_ROOT_HEX: &str = "51d1ae130a534533d6b808b2f866efb3728f2ac32117f6363c73c7d6025e271a";

// Canonical record bytes for counter=1/2/3 (frozen, hand-decodable CBOR).
const CANON_COUNTER1_HEX: &str = "a565726f6c657381666d656d6265726673746174757366616374697665696772616e745f73657102696d656d6265725f6964582000000000000000010000000000000000000000000000000000000000000000006e6163746976655f6465766963657380";
const CANON_COUNTER2_HEX: &str = "a565726f6c657381666d656d6265726673746174757366616374697665696772616e745f73657103696d656d6265725f6964582000000000000000020000000000000000000000000000000000000000000000006e6163746976655f6465766963657380";
const CANON_COUNTER3_HEX: &str = "a565726f6c657381666d656d6265726673746174757366616374697665696772616e745f73657104696d656d6265725f6964582000000000000000030000000000000000000000000000000000000000000000006e6163746976655f6465766963657380";

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("frozen hex")
}

fn id_from_counter(counter: u64) -> PrincipalId {
    let mut bytes = [0u8; LEN];
    bytes[..8].copy_from_slice(&counter.to_be_bytes());
    PrincipalId::from_bytes(bytes)
}

fn record(counter: u64) -> ProjectedMemberRecord {
    ProjectedMemberRecord {
        member_id: id_from_counter(counter),
        status: ProjectedStatus::Active,
        roles: vec![RoleLabel::new("member")],
        active_devices: Vec::new(),
        grant_seq: counter + 1,
        revoke_seq: None,
        profile: None,
    }
}

fn reduce_with_unchanged_promotion(mut level: Vec<[u8; LEN]>) -> [u8; LEN] {
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut chunks = level.chunks_exact(2);
        next.extend(
            chunks
                .by_ref()
                .map(|pair| merkle_node_hash(&pair[0], &pair[1])),
        );
        if let Some(last) = chunks.remainder().first() {
            next.push(*last);
        }
        level = next;
    }
    level[0]
}

fn reduce_with_duplicate_last(mut level: Vec<[u8; LEN]>) -> [u8; LEN] {
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut chunks = level.chunks_exact(2);
        next.extend(
            chunks
                .by_ref()
                .map(|pair| merkle_node_hash(&pair[0], &pair[1])),
        );
        if let Some(last) = chunks.remainder().first() {
            next.push(merkle_node_hash(last, last));
        }
        level = next;
    }
    level[0]
}

fn assert_matches_rebuild(map: &SortedMerkleMap, expected: &BTreeMap<PrincipalId, Vec<u8>>) {
    let oracle = rebuild_root(
        expected
            .iter()
            .map(|(id, canonical)| (id, canonical.as_slice())),
    )
    .expect("unique identities");
    assert_eq!(map.root(), oracle);
}

// ============================================================================
// §1 Frozen-record fence: canonical bytes match the fixture, and round-trip
// through strict canonical decode + typed decode + re-encode.
// ============================================================================

#[test]
fn frozen_canonical_bytes_match_and_round_trip() {
    for (counter, frozen_hex) in [
        (1u64, CANON_COUNTER1_HEX),
        (2, CANON_COUNTER2_HEX),
        (3, CANON_COUNTER3_HEX),
    ] {
        let rec = record(counter);
        let canon = rec.canonical_bytes();
        assert_eq!(
            hex::encode(&canon),
            frozen_hex,
            "counter {counter} canonical"
        );
        // Strict canonical decode + re-encode byte identity.
        let value = decode_canonical(&canon).unwrap();
        assert_eq!(encode(&value), canon, "counter {counter} round-trip");
        // Typed decode validates and returns an equivalent record.
        let back = ProjectedMemberRecord::from_canonical(&value).unwrap();
        assert_eq!(
            back.canonical_bytes(),
            canon,
            "counter {counter} typed round-trip"
        );
    }
}

// ============================================================================
// §2 Frozen-root fence: the implementation reproduces the 0/1/2/3 roots that
// were independently computed with the blake3-ref tool.
// ============================================================================

#[test]
fn zero_leaf_root_matches_frozen() {
    let map = SortedMerkleMap::new();
    assert_eq!(hex::encode(map.root().as_bytes()), EMPTY_ROOT_HEX);
    // And via the projection wrapper.
    assert_eq!(
        hex::encode(MemberMapProjection::new().root().as_bytes()),
        EMPTY_ROOT_HEX
    );
}

#[test]
fn one_two_three_leaf_roots_match_frozen() {
    let canons = [
        hx(CANON_COUNTER1_HEX),
        hx(CANON_COUNTER2_HEX),
        hx(CANON_COUNTER3_HEX),
    ];

    let mut one = SortedMerkleMap::new();
    one.insert_new(id_from_counter(1), canons[0].clone())
        .unwrap();
    assert_eq!(hex::encode(one.root().as_bytes()), ONE_LEAF_ROOT_HEX);

    let mut two = SortedMerkleMap::new();
    two.insert_new(id_from_counter(1), canons[0].clone())
        .unwrap();
    two.insert_new(id_from_counter(2), canons[1].clone())
        .unwrap();
    assert_eq!(hex::encode(two.root().as_bytes()), TWO_LEAF_ROOT_HEX);

    let mut three = SortedMerkleMap::new();
    three
        .insert_new(id_from_counter(1), canons[0].clone())
        .unwrap();
    three
        .insert_new(id_from_counter(2), canons[1].clone())
        .unwrap();
    three
        .insert_new(id_from_counter(3), canons[2].clone())
        .unwrap();
    assert_eq!(hex::encode(three.root().as_bytes()), THREE_LEAF_ROOT_HEX);
}

// ============================================================================
// §3 Odd-node promotion is byte-identical to "promote unchanged", NOT
// "duplicate last" (issue acceptance: property test).
// ============================================================================

#[test]
fn three_leaf_odd_promotion_is_unchanged_not_duplicate() {
    let l0 = member_leaf_hash(&hx(CANON_COUNTER1_HEX));
    let l1 = member_leaf_hash(&hx(CANON_COUNTER2_HEX));
    let l2 = member_leaf_hash(&hx(CANON_COUNTER3_HEX));
    let promoted = merkle_node_hash(&l0, &l1);
    let unchanged = merkle_node_hash(&promoted, &l2);
    let duplicate_last = merkle_node_hash(&promoted, &merkle_node_hash(&l2, &l2));
    assert_eq!(hex::encode(unchanged), THREE_LEAF_ROOT_HEX);
    assert_eq!(hex::encode(duplicate_last), THREE_LEAF_DUP_LAST_HEX);
    assert_ne!(
        unchanged, duplicate_last,
        "promote-unchanged != duplicate-last"
    );
    // The implementation's 3-leaf root must equal the unchanged interpretation.
    assert_eq!(hex::encode(unchanged), THREE_LEAF_ROOT_HEX);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 64, ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn odd_node_promotion_matches_unchanged_bytes_at_every_level(
        half_count in 1usize..=32,
        salt in any::<u64>(),
    ) {
        let leaf_count = half_count * 2 + 1;
        let entries: Vec<(PrincipalId, Vec<u8>)> = (0..leaf_count)
            .map(|index| {
                let counter = u64::try_from(index).unwrap();
                let mut value = record(counter);
                value.grant_seq = salt.wrapping_add(counter);
                (id_from_counter(counter), value.canonical_bytes())
            })
            .collect();
        let leaf_hashes: Vec<[u8; LEN]> = entries
            .iter()
            .map(|(_, canonical)| member_leaf_hash(canonical))
            .collect();
        let unchanged = reduce_with_unchanged_promotion(leaf_hashes.clone());
        let duplicate_last = reduce_with_duplicate_last(leaf_hashes);
        let mut map = SortedMerkleMap::new();
        for (id, canonical) in entries.iter().rev() {
            map.insert_new(*id, canonical.clone()).unwrap();
        }

        let root = map.root();
        prop_assert_eq!(root.as_bytes(), &unchanged);
        prop_assert_ne!(unchanged, duplicate_last);
        for (id, canonical) in &entries {
            let proof = map.prove(id).unwrap();
            prop_assert!(verify_inclusion(&root, id, canonical, &proof).is_ok());
        }
    }
}

// ============================================================================
// §4 10,000-leaf frozen-root gate (the §22.2 v2-core gate).
// ============================================================================

#[test]
fn ten_thousand_leaf_root_matches_frozen() {
    let records: Vec<ProjectedMemberRecord> = (0..10_000u64).map(record).collect();
    let mut map = SortedMerkleMap::new();
    for r in &records {
        map.insert_new(r.member_id, r.canonical_bytes()).unwrap();
    }
    assert_eq!(map.len(), 10_000);
    assert_eq!(hex::encode(map.root().as_bytes()), TEN_K_ROOT_HEX);
    // The D6 oracle (independent code path) agrees.
    let canonicals: Vec<Vec<u8>> = records
        .iter()
        .map(ProjectedMemberRecord::canonical_bytes)
        .collect();
    let pairs: Vec<(&PrincipalId, &[u8])> = records
        .iter()
        .zip(canonicals.iter())
        .map(|(r, c)| (&r.member_id, c.as_slice()))
        .collect();
    let oracle = rebuild_root(pairs).unwrap();
    assert_eq!(
        map.root(),
        oracle,
        "incremental index == D6 oracle at 10k leaves"
    );
}

// ============================================================================
// §5 Inclusion proof: verifies for a member; absent for a non-member; rejects
// when rebound to an absent identity.
// ============================================================================

#[test]
fn inclusion_proof_round_trip_for_random_middle_and_end_members() {
    let mut map = SortedMerkleMap::new();
    for c in 0..10_000u64 {
        let r = record(c);
        map.insert_new(r.member_id, r.canonical_bytes()).unwrap();
    }
    let root = map.root();
    // First, middle, last members.
    for c in [0u64, 4_567, 9_999] {
        let id = id_from_counter(c);
        let proof = map.prove(&id).expect("present");
        let canon = record(c).canonical_bytes();
        verify_inclusion(&root, &id, &canon, &proof).expect("verifies");
    }
    // A non-member has no proof (issue acceptance: "rejects for a non-member").
    let absent = id_from_counter(10_001);
    assert!(map.prove(&absent).is_none());
}

#[test]
fn rebound_proof_for_absent_identity_rejects() {
    let mut proj = MemberMapProjection::new();
    proj.insert_new(record(1)).unwrap();
    proj.insert_new(record(2)).unwrap();
    let proof = proj.prove(&id_from_counter(1)).unwrap();
    // The proof itself verifies against the stored record.
    proj.verify_member_inclusion(&id_from_counter(1), &proof)
        .unwrap();
    // Rebinding the same proof to an absent identity rejects with the stable
    // InvalidMerkleProof code.
    let absent = id_from_counter(0xff);
    assert_eq!(
        proj.verify_member_inclusion(&absent, &proof),
        Err(Reject::InvalidMerkleProof)
    );
}

#[test]
fn corrupt_sibling_rejects() {
    let mut map = SortedMerkleMap::new();
    for c in 1..=6u64 {
        map.insert_new(id_from_counter(c), record(c).canonical_bytes())
            .unwrap();
    }
    let root = map.root();
    let id = id_from_counter(3);
    let mut proof = map.prove(&id).unwrap();
    // Flip the first sibling byte.
    if let Some(b) = proof.siblings[0].hash.get_mut(0) {
        *b ^= 0xff;
    }
    let canon = record(3).canonical_bytes();
    assert_eq!(
        verify_inclusion(&root, &id, &canon, &proof),
        Err(Reject::InvalidMerkleProof)
    );
}

#[test]
fn structurally_invalid_proofs_reject_without_panic() {
    let mut map = SortedMerkleMap::new();
    for c in 1..=4u64 {
        map.insert_new(id_from_counter(c), record(c).canonical_bytes())
            .unwrap();
    }
    let root = map.root();
    let member_id = id_from_counter(1);
    let canon = record(1).canonical_bytes();
    let real = map.prove(&member_id).unwrap();
    // Zero leaf count.
    let bad = InclusionProof {
        leaf_count: 0,
        leaf_index: 0,
        siblings: real.siblings.clone(),
    };
    assert_eq!(
        verify_inclusion(&root, &member_id, &canon, &bad),
        Err(Reject::InvalidMerkleProof)
    );
    // Out-of-range index.
    let bad = InclusionProof {
        leaf_count: 4,
        leaf_index: 99,
        siblings: real.siblings.clone(),
    };
    assert_eq!(
        verify_inclusion(&root, &member_id, &canon, &bad),
        Err(Reject::InvalidMerkleProof)
    );
    // Extra sibling.
    let mut extra = real.siblings.clone();
    extra.push(extra[0]);
    let bad = InclusionProof {
        leaf_count: real.leaf_count,
        leaf_index: real.leaf_index,
        siblings: extra,
    };
    assert_eq!(
        verify_inclusion(&root, &member_id, &canon, &bad),
        Err(Reject::InvalidMerkleProof)
    );
}

#[test]
fn proof_shape_tampering_rejects_every_structural_variant() {
    let mut map = SortedMerkleMap::new();
    for c in 1..=7u64 {
        map.insert_new(id_from_counter(c), record(c).canonical_bytes())
            .unwrap();
    }
    let root = map.root();
    let member_id = id_from_counter(4);
    let canonical = record(4).canonical_bytes();
    let real = map.prove(&member_id).unwrap();

    let mut omitted = real.clone();
    omitted.siblings.pop();
    let mut wrong_side = real.clone();
    wrong_side.siblings[0].side = match wrong_side.siblings[0].side {
        SiblingSide::Left => SiblingSide::Right,
        SiblingSide::Right => SiblingSide::Left,
    };
    let mut reordered = real.clone();
    reordered.siblings.swap(0, 1);
    let oversized = InclusionProof {
        leaf_count: real.leaf_count,
        leaf_index: real.leaf_index,
        siblings: vec![
            ProofStep {
                side: SiblingSide::Left,
                hash: [0; LEN],
            };
            MAX_LEVELS + 1
        ],
    };
    let old_sparse_shape = InclusionProof {
        siblings: vec![
            ProofStep {
                side: SiblingSide::Left,
                hash: [0; LEN],
            };
            256
        ],
        ..real.clone()
    };

    for tampered in [omitted, wrong_side, reordered, oversized, old_sparse_shape] {
        assert_eq!(
            verify_inclusion(&root, &member_id, &canonical, &tampered),
            Err(Reject::InvalidMerkleProof)
        );
    }
}

// ============================================================================
// §6 Incremental insert/replace/remove reproduces the D6 full-build oracle
// after every operation (issue acceptance: "without full rebuild").
// ============================================================================

#[test]
fn incremental_matches_full_rebuild_across_shuffled_ops() {
    let mut map = SortedMerkleMap::new();
    let mut present: Vec<(PrincipalId, Vec<u8>)> = Vec::new();
    // Shuffled insert order.
    for &c in &[7u64, 2, 9, 1, 5, 8, 3, 4, 6] {
        let r = record(c);
        map.insert_new(r.member_id, r.canonical_bytes()).unwrap();
        present.push((r.member_id, r.canonical_bytes()));
        let oracle = rebuild_root(present.iter().map(|(k, v)| (k, v.as_slice()))).unwrap();
        assert_eq!(map.root(), oracle, "after insert counter {c}");
    }
    // Replace a middle record's field.
    let mut replaced = record(5);
    replaced.grant_seq = 123;
    map.replace_existing(&id_from_counter(5), replaced.canonical_bytes())
        .unwrap();
    let pos = present
        .iter()
        .position(|(k, _)| *k == id_from_counter(5))
        .unwrap();
    present[pos].1 = replaced.canonical_bytes();
    assert_eq!(
        map.root(),
        rebuild_root(present.iter().map(|(k, v)| (k, v.as_slice()))).unwrap()
    );
    // Remove first, middle, last.
    for &c in &[1u64, 5, 9] {
        map.remove_existing(&id_from_counter(c)).unwrap();
        present.retain(|(k, _)| *k != id_from_counter(c));
        let oracle = rebuild_root(present.iter().map(|(k, v)| (k, v.as_slice()))).unwrap();
        assert_eq!(map.root(), oracle, "after remove counter {c}");
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 64, ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn incremental_mutations_match_rebuild_and_fail_atomically(
        operations in prop::collection::vec((0u8..=2, 0u8..32, any::<u64>()), 1..128),
    ) {
        let mut map = SortedMerkleMap::new();
        let mut expected = BTreeMap::new();

        for (operation, counter, nonce) in operations {
            let id = id_from_counter(u64::from(counter));
            let mut value = record(u64::from(counter));
            value.grant_seq = nonce;
            let canonical = value.canonical_bytes();
            let root_before = map.root();
            let leaves_before: Vec<(PrincipalId, Vec<u8>)> = map
                .leaves()
                .iter()
                .map(|leaf| (leaf.member_id, leaf.canonical.clone()))
                .collect();

            match operation {
                0 if expected.contains_key(&id) => {
                    prop_assert_eq!(map.insert_new(id, canonical), Err(Reject::InvalidContent));
                    prop_assert_eq!(map.root(), root_before);
                    prop_assert_eq!(
                        map.leaves()
                            .iter()
                            .map(|leaf| (leaf.member_id, leaf.canonical.clone()))
                            .collect::<Vec<_>>(),
                        leaves_before
                    );
                }
                0 => {
                    map.insert_new(id, canonical.clone()).unwrap();
                    expected.insert(id, canonical);
                }
                1 if expected.contains_key(&id) => {
                    map.replace_existing(&id, canonical.clone()).unwrap();
                    expected.insert(id, canonical);
                }
                1 => {
                    prop_assert_eq!(
                        map.replace_existing(&id, canonical),
                        Err(Reject::InvalidContent)
                    );
                    prop_assert_eq!(map.root(), root_before);
                    prop_assert_eq!(
                        map.leaves()
                            .iter()
                            .map(|leaf| (leaf.member_id, leaf.canonical.clone()))
                            .collect::<Vec<_>>(),
                        leaves_before
                    );
                }
                2 if expected.contains_key(&id) => {
                    let removed = map.remove_existing(&id).unwrap();
                    prop_assert_eq!(Some(&removed), expected.get(&id));
                    expected.remove(&id);
                }
                2 => {
                    prop_assert_eq!(
                        map.remove_existing(&id),
                        Err(Reject::InvalidContent)
                    );
                    prop_assert_eq!(map.root(), root_before);
                    prop_assert_eq!(
                        map.leaves()
                            .iter()
                            .map(|leaf| (leaf.member_id, leaf.canonical.clone()))
                            .collect::<Vec<_>>(),
                        leaves_before
                    );
                }
                _ => unreachable!(),
            }

            assert_matches_rebuild(&map, &expected);
            prop_assert_eq!(
                map.leaves()
                    .iter()
                    .map(|leaf| (leaf.member_id, leaf.canonical.clone()))
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|(id, canonical)| (*id, canonical.clone()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn incremental_reuses_unaffected_work_for_end_replace() {
    // Replacing the last leaf must touch only the O(log n) path, not the whole
    // tree. The hashes_computed counter must be far below the leaf count.
    let mut map = SortedMerkleMap::new();
    for c in 0..1_000u64 {
        map.insert_new(id_from_counter(c), record(c).canonical_bytes())
            .unwrap();
    }
    map.reset_hashes_computed();
    let mut r = record(999);
    r.grant_seq = 777;
    map.replace_existing(&id_from_counter(999), r.canonical_bytes())
        .unwrap();
    // A full rebuild would rehash every parent (~n hashes); a path update is
    // ~ceil(log2(n)) = 10 here. Allow generous headroom but prove reuse.
    assert!(
        map.hashes_computed() <= 20,
        "replace touched {} hashes; expected O(log n) reuse",
        map.hashes_computed()
    );
}

#[test]
fn add_then_remove_restores_prior_root() {
    let mut map = SortedMerkleMap::new();
    for c in 1..=8u64 {
        map.insert_new(id_from_counter(c), record(c).canonical_bytes())
            .unwrap();
    }
    let before = map.root();
    let probe_id = id_from_counter(4);
    // Remove then re-add the same record.
    let removed = map.remove_existing(&probe_id).unwrap();
    let without = map.root();
    assert_ne!(before, without, "remove must change root");
    map.insert_new(probe_id, removed).unwrap();
    assert_eq!(map.root(), before, "add-then-remove must restore the root");
}

// ============================================================================
// §7 Frozen-metadata fence: the fixture carries the schema/frozen markers and
// mirrors every frozen hex value (spec §5 Step 8 mirror discipline).
// ============================================================================

// ============================================================================
// §8 Frozen inclusion-proof vectors (spec #153 / §4 D7 / §8 Step 12: "pin
// inclusion proof in vectors"). The expected proofs are HAND-DERIVED from the
// frozen canonical records — sibling hashes are `member_leaf_hash` of the
// frozen canonical bytes, and sides/indices come from the tree arithmetic — so
// they are an independent reproduction (any BLAKE3 tool over the frozen domain
// reproduces the sibling hashes), NOT a value read back from `prove()`.
//
// Sibling-hash cross-checks: leaf_hash(1) == ONE_LEAF_ROOT_HEX (a one-leaf tree
// is the leaf promoted unchanged) and node(leaf1,leaf2) == TWO_LEAF_ROOT_HEX;
// both are already frozen above. The two leaf hashes below are
// member_leaf_hash(CANON_COUNTERn_HEX).
// ============================================================================
const LEAFHASH2_HEX: &str = "72273a50dfcd43d093c6f8dfb2e45abddf0434879814f5e3b71bcf82ee04fdef";
const LEAFHASH3_HEX: &str = "6e6605f02a41c087b8a1ee549b788bf63fae1094687ba68bf1ae080cef23af82";

// Frozen canonical-CBOR inclusion proofs (D8 schema `{"leaf_count","leaf_index",
// "siblings":[{side,hash}]}`, map keys in canonical sorted order).
// 2-leaf tree {1,2}, prove counter=2 (index 1): one Left leaf sibling.
const PROOF_TWO_C2_HEX: &str = "a3687369626c696e677381a2646861736858206ecdad13ec133fa2d026b5ed88e2cab04054d3415226e2b145b55540e9609d226473696465646c6566746a6c6561665f636f756e74026a6c6561665f696e64657801";
// 3-leaf tree {1,2,3}, prove counter=2 (index 1): [Left leaf(1), Right leaf(3)].
const PROOF_THREE_C2_HEX: &str = "a3687369626c696e677382a2646861736858206ecdad13ec133fa2d026b5ed88e2cab04054d3415226e2b145b55540e9609d226473696465646c656674a2646861736858206e6605f02a41c087b8a1ee549b788bf63fae1094687ba68bf1ae080cef23af8264736964656572696768746a6c6561665f636f756e74036a6c6561665f696e64657801";
// 3-leaf tree {1,2,3}, prove counter=3 (index 2): one Left NODE sibling — the
// trailing leaf 3 is promoted unchanged, so its level-1 sibling is
// node(leaf1,leaf2) = the 2-leaf root.
const PROOF_THREE_C3_HEX: &str = "a3687369626c696e677381a26468617368582072be3ac1b430ccfe99a237fa470d202017c5a46a335463038ba2252611bfbbf56473696465646c6566746a6c6561665f636f756e74036a6c6561665f696e64657802";

fn frozen_two_leaf_map() -> SortedMerkleMap {
    let mut map = SortedMerkleMap::new();
    map.insert_new(id_from_counter(1), hx(CANON_COUNTER1_HEX))
        .unwrap();
    map.insert_new(id_from_counter(2), hx(CANON_COUNTER2_HEX))
        .unwrap();
    map
}

fn frozen_three_leaf_map() -> SortedMerkleMap {
    let mut map = SortedMerkleMap::new();
    map.insert_new(id_from_counter(1), hx(CANON_COUNTER1_HEX))
        .unwrap();
    map.insert_new(id_from_counter(2), hx(CANON_COUNTER2_HEX))
        .unwrap();
    map.insert_new(id_from_counter(3), hx(CANON_COUNTER3_HEX))
        .unwrap();
    map
}

fn assert_frozen_inclusion_proof(
    map: &SortedMerkleMap,
    proven_id: PrincipalId,
    proven_canon_hex: &str,
    root_hex: &str,
    expected: &InclusionProof,
    proof_hex: &str,
) {
    assert_eq!(hex::encode(map.root().as_bytes()), root_hex);
    assert_eq!(
        &map.prove(&proven_id).unwrap(),
        expected,
        "prove() must match the hand-derived structure"
    );
    assert_eq!(hex::encode(expected.canonical_bytes()), proof_hex);
    assert_eq!(&decode_inclusion_proof(&hx(proof_hex)).unwrap(), expected);
    verify_inclusion(&map.root(), &proven_id, &hx(proven_canon_hex), expected)
        .expect("frozen inclusion proof verifies against the frozen root");
}

#[test]
fn frozen_inclusion_proofs_match_independently_derived_structure() {
    // Sibling hashes are member_leaf_hash of the frozen canonical records — an
    // independent reproduction (any BLAKE3 tool over the frozen domain
    // reproduces these), not a value read back from `prove()`.
    let leafhash1 = member_leaf_hash(&hx(CANON_COUNTER1_HEX));
    let leafhash2 = member_leaf_hash(&hx(CANON_COUNTER2_HEX));
    let leafhash3 = member_leaf_hash(&hx(CANON_COUNTER3_HEX));
    assert_eq!(hex::encode(leafhash1), ONE_LEAF_ROOT_HEX);
    assert_eq!(hex::encode(leafhash2), LEAFHASH2_HEX);
    assert_eq!(hex::encode(leafhash3), LEAFHASH3_HEX);

    // 2-leaf tree {1,2}, prove counter=2 (index 1): one Left leaf sibling.
    let expected_two_c2 = InclusionProof {
        leaf_count: 2,
        leaf_index: 1,
        siblings: vec![ProofStep {
            side: SiblingSide::Left,
            hash: leafhash1,
        }],
    };
    assert_frozen_inclusion_proof(
        &frozen_two_leaf_map(),
        id_from_counter(2),
        CANON_COUNTER2_HEX,
        TWO_LEAF_ROOT_HEX,
        &expected_two_c2,
        PROOF_TWO_C2_HEX,
    );

    // 3-leaf tree {1,2,3}, prove counter=2 (index 1): [Left leaf(1), Right leaf(3)].
    let expected_three_c2 = InclusionProof {
        leaf_count: 3,
        leaf_index: 1,
        siblings: vec![
            ProofStep {
                side: SiblingSide::Left,
                hash: leafhash1,
            },
            ProofStep {
                side: SiblingSide::Right,
                hash: leafhash3,
            },
        ],
    };
    assert_frozen_inclusion_proof(
        &frozen_three_leaf_map(),
        id_from_counter(2),
        CANON_COUNTER2_HEX,
        THREE_LEAF_ROOT_HEX,
        &expected_three_c2,
        PROOF_THREE_C2_HEX,
    );

    // 3-leaf tree {1,2,3}, prove counter=3 (index 2): one Left NODE sibling.
    // Trailing leaf 3 is promoted unchanged, so at level 1 its sibling is
    // node(leaf1,leaf2) — which equals the 2-leaf root (independently frozen).
    let three = frozen_three_leaf_map();
    let node01 = merkle_node_hash(&leafhash1, &leafhash2);
    assert_eq!(hex::encode(node01), TWO_LEAF_ROOT_HEX);
    let expected_three_c3 = InclusionProof {
        leaf_count: 3,
        leaf_index: 2,
        siblings: vec![ProofStep {
            side: SiblingSide::Left,
            hash: node01,
        }],
    };
    assert_frozen_inclusion_proof(
        &three,
        id_from_counter(3),
        CANON_COUNTER3_HEX,
        THREE_LEAF_ROOT_HEX,
        &expected_three_c3,
        PROOF_THREE_C3_HEX,
    );

    // Exclusion semantics: an absent identity yields no proof (the v2 member map
    // expresses exclusion as absence-of-proof + rebound-proof rejection, not a
    // separate exclusion fixture — see `rebound_proof_for_absent_identity_rejects`).
    assert!(frozen_two_leaf_map().prove(&id_from_counter(99)).is_none());
}

#[test]
fn fixture_carries_frozen_markers_and_mirrors_constants() {
    assert!(GOLDEN_JSON.contains("\"schema\": \"iroh-room-v2-member-merkle/v1\""));
    assert!(GOLDEN_JSON.contains("\"frozen\": true"));
    assert!(GOLDEN_JSON.contains("\"requires_schema_bump_on_change\": true"));
    for hex_value in [
        EMPTY_ROOT_HEX,
        ONE_LEAF_ROOT_HEX,
        TWO_LEAF_ROOT_HEX,
        THREE_LEAF_ROOT_HEX,
        THREE_LEAF_DUP_LAST_HEX,
        TEN_K_ROOT_HEX,
        CANON_COUNTER1_HEX,
        CANON_COUNTER2_HEX,
        CANON_COUNTER3_HEX,
        LEAFHASH2_HEX,
        LEAFHASH3_HEX,
        PROOF_TWO_C2_HEX,
        PROOF_THREE_C2_HEX,
        PROOF_THREE_C3_HEX,
    ] {
        assert!(
            GOLDEN_JSON.contains(hex_value),
            "frozen hex {hex_value} is in the Rust test constants but missing from the JSON fixture"
        );
    }
}

// ============================================================================
// #160 / #134 §14 + §22.2 v2-core gate: the 10,000-member governance snapshot
// MUST stay under 5 MiB (excluding optional profile blobs), or v2.0 would need
// proof-carrying light-client mode (#134 §25 #6). This measures the
// uncompressed member-map snapshot — a canonical-CBOR array of the sorted
// projected records, the dominant component of the §7.6 snapshot blob a new
// client fetches. The exact snapshot framing is frozen by #161; the per-record
// size (and thus this verdict) is stable regardless of final framing.
// ============================================================================

#[test]
fn ten_thousand_member_snapshot_under_five_mib_gate() {
    const FIVE_MIB: usize = 5 * 1024 * 1024;
    let records: Vec<ProjectedMemberRecord> = (0..10_000u64).map(record).collect();
    // Payload = the 10k canonical record bytes (the content a client receives).
    let payload: usize = records.iter().map(|r| r.canonical_bytes().len()).sum();
    // Full framed snapshot = a canonical-CBOR array of the sorted records.
    let values: Vec<CborValue> = records
        .iter()
        .map(|r| decode_canonical(&r.canonical_bytes()).expect("record is canonical"))
        .collect();
    let snapshot = encode(&CborValue::Array(values));
    eprintln!(
        "ten_thousand_member_snapshot: members={} payload_bytes={} snapshot_bytes={} ({} MiB) per_record_bytes={} five_mib={} fits={}",
        records.len(),
        payload,
        snapshot.len(),
        snapshot.len() / (1024 * 1024),
        payload / records.len(),
        FIVE_MIB,
        snapshot.len() < FIVE_MIB,
    );
    assert!(
        snapshot.len() < FIVE_MIB,
        "10k member snapshot is {} bytes (> 5 MiB gate): v2.0 would need proof-carrying light-client mode (#134 §25 #6 / #160)",
        snapshot.len(),
    );
}
