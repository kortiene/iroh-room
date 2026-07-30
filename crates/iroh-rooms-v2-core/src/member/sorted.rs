//! The #134 §8.2 sorted Merkle tree over member records (issue #151).
//!
//! This is the **normative** #151 member-Merkle construction. It replaces the
//! legacy 256-level sparse map in [`super::merkle`] for the member-projection
//! path. The two coexist while the old candidate golden vectors remain frozen:
//! new normative callers use this module exclusively.
//!
//! # Construction (#134 §8.2)
//!
//! - Leaves are member records, sorted by their **raw 32-byte identity-key
//!   bytes** (`PrincipalId`), not by display text, encoded CBOR, a BLAKE3 map
//!   key, insertion order, or governance arrival order.
//! - Leaf hash: `BLAKE3(b"iroh-room-v2/member-leaf" || canonical_member_record)`.
//!   There is no intermediate map-key hash or value hash.
//! - Parent hash: `BLAKE3(b"iroh-room-v2/merkle-node" || left_32 || right_32)`.
//!   Child order is significant.
//! - At every level, nodes are paired left-to-right. A trailing unpaired node
//!   is **promoted unchanged** to the next level — never duplicated, never
//!   hashed alone, never combined with an empty marker.
//! - One-leaf root is therefore the leaf hash itself; it is promoted unchanged
//!   until it is the root.
//! - Empty root: `BLAKE3(b"iroh-room-v2/merkle-node" || 0x40)`, where `0x40`
//!   is the deterministic-CBOR encoding of the empty byte string named by §8.2.
//!
//! # Incremental index (spec D7 / Candidate A)
//!
//! [`SortedMerkleMap`] stores sorted leaves plus fully materialized levels
//! (level 0 = leaf hashes; level L = the L-th pairing layer). Mutation reuses
//! the unaffected prefix and recomputes only the affected suffix:
//!
//! - **replace** (key present, rank unchanged): recompute the single leaf and
//!   its `O(log n)` path to the root — every other hash is reused.
//! - **insert** / **remove** (rank shifts every later leaf): every pair boundary
//!   at and after the change point shifts, so each level is recomputed from its
//!   first affected node onward. The prefix before the change point is reused
//!   at every level.
//!
//! The mutation path never calls the full-build oracle ([`rebuild_root`]); the
//! oracle exists only for fixtures and property tests. A `hashes_computed`
//! counter is exposed for test instrumentation so the "reuses unaffected work,
//! beats full rebuild" property can be measured (spec §3.4 #42 / §7.7).

use std::collections::BTreeMap;

use crate::error::Reject;
use crate::ids::{MerkleRoot, PrincipalId, LEN};

/// The frozen `member-leaf` domain (`#134 §6.2`).
pub const MEMBER_LEAF: &[u8] = crate::domain::MEMBER_LEAF;
/// The frozen `merkle-node` domain (`#134 §6.2`).
pub const MERKLE_NODE: &[u8] = crate::domain::MERKLE_NODE;

/// Maximum Merkle-tree height for a `u64` leaf count. A proof carries at most
/// one sibling per level, so a valid inclusion proof has at most this many
/// sibling steps (spec D8).
pub const MAX_LEVELS: usize = 64;

/// A raw 32-byte BLAKE3 digest used as a Merkle leaf/node hash.
pub type Hash = [u8; LEN];

fn validate_keyed_record(member_id: &PrincipalId, canonical: &[u8]) -> Result<(), Reject> {
    let record = super::projected::ProjectedMemberRecord::from_bytes(canonical)?;
    if record.member_id == *member_id {
        Ok(())
    } else {
        Err(Reject::InvalidContent)
    }
}

/// A stored leaf: the identity key (sort key), the canonical record bytes (the
/// leaf preimage, retained so incremental updates never re-encode unaffected
/// records), and the cached leaf hash.
#[derive(Clone, Debug)]
pub struct Leaf {
    /// The identity public key; the sort key for the tree.
    pub member_id: PrincipalId,
    /// The exact canonical-CBOR record bytes (the leaf preimage).
    pub canonical: Vec<u8>,
    /// `BLAKE3(MEMBER_LEAF || canonical)`.
    pub hash: Hash,
}

/// Leaf hash: `BLAKE3(MEMBER_LEAF || canonical_record)` (#134 §8.2).
#[must_use]
pub fn member_leaf_hash(canonical_record: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(MEMBER_LEAF);
    h.update(canonical_record);
    *h.finalize().as_bytes()
}

/// Parent hash: `BLAKE3(MERKLE_NODE || left || right)` (#134 §8.2). Child order
/// is significant.
#[must_use]
pub fn merkle_node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(MERKLE_NODE);
    h.update(left);
    h.update(right);
    *h.finalize().as_bytes()
}

/// The deterministic-CBOR encoding of the empty byte string named by §8.2.
pub const EMPTY_BSTR_BYTES: &[u8] = &[0x40];

/// The empty-tree root: `BLAKE3(MERKLE_NODE || 0x40)`.
#[must_use]
pub fn empty_member_root() -> MerkleRoot {
    MerkleRoot::from_bytes(empty_member_root_bytes())
}

/// The raw 32 bytes of the empty-tree root.
#[must_use]
pub fn empty_member_root_bytes() -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(MERKLE_NODE);
    h.update(EMPTY_BSTR_BYTES);
    *h.finalize().as_bytes()
}

// ----------------------------------------------------------------------------
// D6 full-build oracle (spec D6). Independent of the incremental index; used
// only for fixtures, property tests, and rebuild-equivalence checks.
// ----------------------------------------------------------------------------

/// Recompute the root from an unsorted iterator of canonical record bytes by
/// sorting leaf hashes by the supplied identity keys and reducing level-by-level
/// with the exact odd-node promotion rule (spec D6 / #134 §8.2).
///
/// `records` yields `(identity_key, canonical_record_bytes)` pairs. Duplicate
/// identity keys are rejected. This is the obviously-correct oracle; the
/// mutation path ([`SortedMerkleMap`]) must agree with it after every update.
///
/// # Errors
/// Returns [`Reject::InvalidContent`] on a duplicate identity key.
pub fn rebuild_root<'a, I>(records: I) -> Result<MerkleRoot, Reject>
where
    I: IntoIterator<Item = (&'a PrincipalId, &'a [u8])>,
{
    let mut keyed: Vec<(PrincipalId, Hash)> = records
        .into_iter()
        .map(|(id, canon)| {
            let record = super::projected::ProjectedMemberRecord::from_bytes(canon)?;
            if record.member_id != *id {
                return Err(Reject::InvalidContent);
            }
            Ok((*id, member_leaf_hash(canon)))
        })
        .collect::<Result<_, _>>()?;
    // Sort by raw identity-key bytes (spec §8.2). `PrincipalId::Ord` is bytewise
    // over raw bytes (ids.rs), so this is the raw-byte order the spec requires.
    keyed.sort_by_key(|(id, _)| *id);
    // Reject duplicate identity keys (spec §3.1 #6).
    for w in keyed.windows(2) {
        if w[0].0 == w[1].0 {
            return Err(Reject::InvalidContent);
        }
    }
    let hashes: Vec<Hash> = keyed.iter().map(|(_, h)| *h).collect();
    Ok(MerkleRoot::from_bytes(reduce_levels(&hashes)))
}

/// Reduce a sorted level-0 hash slice to the root, pairing left-to-right and
/// promoting a trailing unpaired node unchanged (spec D6). Empty input yields
/// the empty root.
fn reduce_levels(level: &[Hash]) -> Hash {
    if level.is_empty() {
        return empty_member_root_bytes();
    }
    let mut current: Vec<Hash> = level.to_vec();
    while current.len() > 1 {
        let mut next: Vec<Hash> = Vec::with_capacity(current.len() / 2 + 1);
        let mut i = 0;
        while i + 1 < current.len() {
            next.push(merkle_node_hash(&current[i], &current[i + 1]));
            i += 2;
        }
        // Odd trailing node: promote unchanged (NOT duplicated, NOT hashed
        // alone, NOT combined with an empty marker).
        if i < current.len() {
            next.push(current[i]);
        }
        current = next;
    }
    current[0]
}

/// Compute the §8.2 root directly from `(identity, canonical_record_bytes)`
/// pairs without re-validating the records. Sorts by raw identity-key bytes,
/// hashes each leaf with `member_leaf_hash`, and reduces with the unchanged
/// odd-node promotion rule. Byte-identical to [`rebuild_root`] for records
/// that already validate; total (never fails) for arbitrary canonical bytes.
/// Used by the governance state-root computation, which must stay total and
/// must not panic on arbitrary public [`crate::governance::log::GovernanceState`].
pub(crate) fn root_from_canonical<'a, I>(records: I) -> Hash
where
    I: IntoIterator<Item = (&'a PrincipalId, &'a [u8])>,
{
    let mut keyed: Vec<(PrincipalId, Hash)> = records
        .into_iter()
        .map(|(id, canon)| (*id, member_leaf_hash(canon)))
        .collect();
    keyed.sort_by_key(|(id, _)| *id);
    // Duplicate identities cannot occur for a keyed map (each id is unique);
    // retain the last so the reduction stays total on adversarial inputs.
    keyed.dedup_by_key(|(id, _)| *id);
    let hashes: Vec<Hash> = keyed.iter().map(|(_, h)| *h).collect();
    reduce_levels(&hashes)
}

// ----------------------------------------------------------------------------
// Inclusion proofs (spec §3.5 / D8).
// ----------------------------------------------------------------------------

/// Which side the sibling sits on relative to the leaf being proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiblingSide {
    /// The sibling is to the left; the proof recomputes `node(sibling, acc)`.
    Left,
    /// The sibling is to the right; the proof recomputes `node(acc, sibling)`.
    Right,
}

/// One sibling step in an inclusion proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProofStep {
    /// Sibling position relative to the prover's current accumulator.
    pub side: SiblingSide,
    /// The sibling's 32-byte hash at this level.
    pub hash: Hash,
}

/// A compact inclusion proof (spec D8). Canonical-CBOR-serializable as the
/// closed map `{"leaf_count": uint, "leaf_index": uint, "siblings": [...]}`.
///
/// `leaf_count` and `leaf_index` fully determine the level-by-level pairing, so
/// odd-node promotion is implicit and contributes no sibling step. The prover
/// is bound to the requested identity + canonical record at verification time
/// (the leaf hash is recomputed, never trusted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InclusionProof {
    /// Total number of leaves in the committed tree.
    pub leaf_count: u64,
    /// Zero-based sorted-rank index of the proven leaf.
    pub leaf_index: u64,
    /// One sibling per paired level, ordered from the leaf level upward.
    pub siblings: Vec<ProofStep>,
}

impl InclusionProof {
    /// Build the canonical-CBOR encoding (spec D8 frozen schema).
    #[must_use]
    pub fn to_cbor_value(&self) -> crate::cbor::CborValue {
        use crate::cbor::CborValue;
        let siblings = self
            .siblings
            .iter()
            .map(|step| {
                let side = match step.side {
                    SiblingSide::Left => "left",
                    SiblingSide::Right => "right",
                };
                CborValue::Map(vec![
                    ("side".to_owned(), CborValue::Text(side.to_owned())),
                    ("hash".to_owned(), CborValue::Bytes(step.hash.to_vec())),
                ])
            })
            .collect();
        CborValue::Map(vec![
            ("leaf_count".to_owned(), CborValue::Uint(self.leaf_count)),
            ("leaf_index".to_owned(), CborValue::Uint(self.leaf_index)),
            ("siblings".to_owned(), CborValue::Array(siblings)),
        ])
    }

    /// Encode this proof as deterministic CBOR using the frozen D8 schema.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        crate::cbor::encode(&self.to_cbor_value())
    }
}

struct InclusionProofReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> InclusionProofReader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Reject> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(Reject::InvalidMerkleProof)?;
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or(Reject::InvalidMerkleProof)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_byte(&mut self) -> Result<u8, Reject> {
        Ok(self.take(1)?[0])
    }

    fn read_head(&mut self) -> Result<(u8, u64), Reject> {
        let initial = self.read_byte()?;
        let major = initial >> 5;
        let argument = match initial & 0x1f {
            value @ 0..=23 => u64::from(value),
            24 => {
                let value = u64::from(self.read_byte()?);
                if value <= 23 {
                    return Err(Reject::InvalidMerkleProof);
                }
                value
            }
            25 => {
                let bytes = self.take(2)?;
                let value = u64::from(u16::from_be_bytes([bytes[0], bytes[1]]));
                if u8::try_from(value).is_ok() {
                    return Err(Reject::InvalidMerkleProof);
                }
                value
            }
            26 => {
                let bytes = self.take(4)?;
                let value = u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                if u16::try_from(value).is_ok() {
                    return Err(Reject::InvalidMerkleProof);
                }
                value
            }
            27 => {
                let bytes = self.take(8)?;
                let value = u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                if u32::try_from(value).is_ok() {
                    return Err(Reject::InvalidMerkleProof);
                }
                value
            }
            _ => return Err(Reject::InvalidMerkleProof),
        };
        Ok((major, argument))
    }

    fn expect_head(&mut self, major: u8, argument: u64) -> Result<(), Reject> {
        if self.read_head()? == (major, argument) {
            Ok(())
        } else {
            Err(Reject::InvalidMerkleProof)
        }
    }

    fn expect_text(&mut self, expected: &[u8]) -> Result<(), Reject> {
        let length = u64::try_from(expected.len()).map_err(|_| Reject::InvalidMerkleProof)?;
        self.expect_head(3, length)?;
        if self.take(expected.len())? == expected {
            Ok(())
        } else {
            Err(Reject::InvalidMerkleProof)
        }
    }

    fn read_uint(&mut self) -> Result<u64, Reject> {
        let (major, value) = self.read_head()?;
        if major == 0 {
            Ok(value)
        } else {
            Err(Reject::InvalidMerkleProof)
        }
    }

    fn read_step(&mut self) -> Result<ProofStep, Reject> {
        self.expect_head(5, 2)?;
        self.expect_text(b"hash")?;
        self.expect_head(2, LEN as u64)?;
        let hash: Hash = self
            .take(LEN)?
            .try_into()
            .map_err(|_| Reject::InvalidMerkleProof)?;
        self.expect_text(b"side")?;
        let (major, length) = self.read_head()?;
        if major != 3 {
            return Err(Reject::InvalidMerkleProof);
        }
        let side = match length {
            4 if self.take(4)? == b"left" => SiblingSide::Left,
            5 if self.take(5)? == b"right" => SiblingSide::Right,
            _ => return Err(Reject::InvalidMerkleProof),
        };
        Ok(ProofStep { side, hash })
    }
}

/// Decode a deterministic-CBOR inclusion proof through a bounded streaming
/// parser and validate its complete Merkle path shape.
///
/// # Errors
/// Returns [`Reject::InvalidMerkleProof`] for every malformed, non-canonical, or
/// structurally invalid proof.
pub fn decode_inclusion_proof(input: &[u8]) -> Result<InclusionProof, Reject> {
    let mut reader = InclusionProofReader::new(input);
    reader.expect_head(5, 3)?;
    reader.expect_text(b"siblings")?;
    let (major, sibling_count) = reader.read_head()?;
    if major != 4 || sibling_count > MAX_LEVELS as u64 {
        return Err(Reject::InvalidMerkleProof);
    }
    let sibling_count = usize::try_from(sibling_count).map_err(|_| Reject::InvalidMerkleProof)?;
    let mut siblings = Vec::with_capacity(sibling_count);
    for _ in 0..sibling_count {
        siblings.push(reader.read_step()?);
    }
    reader.expect_text(b"leaf_count")?;
    let leaf_count = reader.read_uint()?;
    reader.expect_text(b"leaf_index")?;
    let leaf_index = reader.read_uint()?;
    if reader.position != input.len() {
        return Err(Reject::InvalidMerkleProof);
    }
    let proof = InclusionProof {
        leaf_count,
        leaf_index,
        siblings,
    };
    validate_inclusion_structure(&proof)?;
    Ok(proof)
}

/// The exact number of sibling steps an inclusion proof must carry for the
/// `(leaf_count, leaf_index)` path. At each level, a step is required iff the
/// prover is paired with a sibling (i.e. the prover's index is even and there
/// is a right neighbor, or the prover's index is odd); an unpaired trailing
/// node at the prover's position contributes no step (spec D8).
#[must_use]
pub fn expected_sibling_count(mut leaf_count: u64, mut leaf_index: u64) -> usize {
    if leaf_count == 0 {
        return 0;
    }
    let mut steps = 0usize;
    while leaf_count > 1 {
        let paired = if leaf_index % 2 == 1 {
            // Odd index: left sibling exists.
            true
        } else {
            // Even index: right sibling exists iff there is a right neighbor.
            leaf_index + 1 < leaf_count
        };
        if paired {
            steps += 1;
        }
        // Advance: count => ceil(count/2), index => floor(index/2).
        leaf_count = leaf_count / 2 + leaf_count % 2;
        leaf_index /= 2;
    }
    steps
}

fn validate_inclusion_structure(proof: &InclusionProof) -> Result<(), Reject> {
    if proof.leaf_count == 0
        || proof.leaf_index >= proof.leaf_count
        || proof.siblings.len() > MAX_LEVELS
        || proof.siblings.len() != expected_sibling_count(proof.leaf_count, proof.leaf_index)
    {
        return Err(Reject::InvalidMerkleProof);
    }
    let mut leaf_count = proof.leaf_count;
    let mut leaf_index = proof.leaf_index;
    let mut steps = proof.siblings.iter();
    while leaf_count > 1 {
        let expected_side = if leaf_index % 2 == 1 {
            Some(SiblingSide::Left)
        } else if leaf_index + 1 < leaf_count {
            Some(SiblingSide::Right)
        } else {
            None
        };
        if let Some(expected_side) = expected_side {
            if steps.next().map(|step| step.side) != Some(expected_side) {
                return Err(Reject::InvalidMerkleProof);
            }
        }
        leaf_count = leaf_count / 2 + leaf_count % 2;
        leaf_index /= 2;
    }
    if steps.next().is_some() {
        return Err(Reject::InvalidMerkleProof);
    }
    Ok(())
}

/// Verify an inclusion proof by recomputing the leaf from `canonical_record`
/// and reducing to the root with the supplied siblings (#134 §8.2 / spec §3.5).
///
/// # Errors
/// Returns [`Reject::InvalidMerkleProof`] for any malformed record, structural
/// fault, or root mismatch.
pub fn verify_inclusion(
    root: &MerkleRoot,
    member_id: &PrincipalId,
    canonical_record: &[u8],
    proof: &InclusionProof,
) -> Result<(), Reject> {
    let record = super::projected::ProjectedMemberRecord::from_bytes(canonical_record)
        .map_err(|_| Reject::InvalidMerkleProof)?;
    if record.member_id != *member_id {
        return Err(Reject::InvalidMerkleProof);
    }
    // Structural checks before any hashing (spec §3.5 #46).
    validate_inclusion_structure(proof)?;

    // Recompute the leaf — never trust a supplied leaf hash (spec §3.5 #44).
    let mut acc = member_leaf_hash(canonical_record);
    let mut leaf_count = proof.leaf_count;
    let mut leaf_index = proof.leaf_index;
    let mut step_iter = proof.siblings.iter();
    while leaf_count > 1 {
        let paired = if leaf_index % 2 == 1 {
            // Odd index: a left sibling must be supplied.
            true
        } else {
            // Even index: a right sibling exists iff there is a right neighbor.
            leaf_index + 1 < leaf_count
        };
        if paired {
            let step = step_iter.next().ok_or(Reject::InvalidMerkleProof)?;
            // The side must agree with the tree arithmetic (spec §3.5 #46).
            let expected_side = if leaf_index % 2 == 1 {
                SiblingSide::Left
            } else {
                SiblingSide::Right
            };
            if step.side != expected_side {
                return Err(Reject::InvalidMerkleProof);
            }
            acc = match step.side {
                SiblingSide::Left => merkle_node_hash(&step.hash, &acc),
                SiblingSide::Right => merkle_node_hash(&acc, &step.hash),
            };
        }
        // Advance: count => ceil(count/2), index => floor(index/2). An unpaired
        // node is promoted unchanged (no hash, no consumed step).
        leaf_count = leaf_count / 2 + leaf_count % 2;
        leaf_index /= 2;
    }
    // No trailing/extra steps allowed.
    if step_iter.next().is_some() {
        return Err(Reject::InvalidMerkleProof);
    }
    if MerkleRoot::from_bytes(acc) == *root {
        Ok(())
    } else {
        Err(Reject::InvalidMerkleProof)
    }
}

// ----------------------------------------------------------------------------
// SortedMerkleMap — the incremental index (spec D7 / Candidate A).
// ----------------------------------------------------------------------------

/// The sorted Merkle map over canonical member-record bytes (#134 §8.2).
///
/// Leaves are held sorted by raw `PrincipalId` bytes; the materialized levels
/// cache every node hash so mutation reuses unaffected subtrees. See the module
/// docs for the exact odd-node promotion rule and the incremental strategy.
#[derive(Clone, Debug)]
pub struct SortedMerkleMap {
    /// Sorted leaves (sort key = `PrincipalId` raw bytes).
    leaves: Vec<Leaf>,
    /// Materialized levels: `levels[0]` = leaf hashes, `levels[L]` = the L-th
    /// pairing layer. `levels.last()` is the single-node root layer. Empty when
    /// the tree is empty (root = [`empty_member_root`]).
    levels: Vec<Vec<Hash>>,
    /// O(log n) identity -> sorted-rank lookup.
    index: BTreeMap<PrincipalId, usize>,
    /// Instrumentation: total `merkle_node_hash` + `member_leaf_hash` calls
    /// performed by mutation since construction. Test-only; proves reuse.
    hashes_computed: u64,
}

impl Default for SortedMerkleMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SortedMerkleMap {
    /// An empty map. Its root is [`empty_member_root`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            levels: Vec::new(),
            index: BTreeMap::new(),
            hashes_computed: 0,
        }
    }

    /// Build a map from an iterator of `(identity, canonical_record_bytes)` by
    /// full-build (spec D6). Duplicate identity keys are rejected.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] on a duplicate identity key.
    pub fn from_records<'a, I>(records: I) -> Result<Self, Reject>
    where
        I: IntoIterator<Item = (PrincipalId, &'a [u8])>,
    {
        let records = records
            .into_iter()
            .map(|(member_id, canonical)| {
                validate_keyed_record(&member_id, canonical)?;
                Ok((member_id, canonical.to_vec()))
            })
            .collect::<Result<Vec<_>, Reject>>()?;
        Self::from_validated_records(records)
    }

    pub(crate) fn from_validated_records<I>(records: I) -> Result<Self, Reject>
    where
        I: IntoIterator<Item = (PrincipalId, Vec<u8>)>,
    {
        let mut leaves: Vec<Leaf> = records
            .into_iter()
            .map(|(member_id, canonical)| Leaf {
                member_id,
                hash: member_leaf_hash(&canonical),
                canonical,
            })
            .collect();
        leaves.sort_by_key(|leaf| leaf.member_id);
        if leaves
            .windows(2)
            .any(|window| window[0].member_id == window[1].member_id)
        {
            return Err(Reject::InvalidContent);
        }
        let index = leaves
            .iter()
            .enumerate()
            .map(|(position, leaf)| (leaf.member_id, position))
            .collect();
        let mut map = Self {
            leaves,
            levels: Vec::new(),
            index,
            hashes_computed: 0,
        };
        map.rebuild_levels();
        Ok(map)
    }

    /// Number of leaves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Test-only instrumentation: total leaf/node hashes computed by mutation
    /// since construction (spec §7.7 — proves the incremental path reuses work).
    #[must_use]
    pub fn hashes_computed(&self) -> u64 {
        self.hashes_computed
    }

    /// Reset the instrumentation counter.
    #[doc(hidden)]
    pub fn reset_hashes_computed(&mut self) {
        self.hashes_computed = 0;
    }

    /// The committed root.
    #[must_use]
    pub fn root(&self) -> MerkleRoot {
        if self.leaves.is_empty() {
            return empty_member_root();
        }
        MerkleRoot::from_bytes(self.levels_last_root())
    }

    /// Borrow the leaf for `id`, if present.
    #[must_use]
    pub fn get(&self, id: &PrincipalId) -> Option<&Leaf> {
        self.index.get(id).map(|&pos| &self.leaves[pos])
    }

    /// Borrow the sorted leaves (in raw-identity order).
    #[must_use]
    pub fn leaves(&self) -> &[Leaf] {
        &self.leaves
    }

    /// Insert a brand-new leaf. Rejects if `member_id` already exists (spec
    /// §3.1 #6 — duplicates must never be silently overwritten).
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the identity is already present.
    pub fn insert_new(&mut self, member_id: PrincipalId, canonical: Vec<u8>) -> Result<(), Reject> {
        validate_keyed_record(&member_id, &canonical)?;
        if self.index.contains_key(&member_id) {
            return Err(Reject::InvalidContent);
        }
        let hash = member_leaf_hash(&canonical);
        self.hashes_computed += 1;
        let pos = match self.index.range(..member_id).next_back() {
            Some((_, &p)) => p + 1,
            None => 0,
        };
        self.leaves.insert(
            pos,
            Leaf {
                member_id,
                canonical,
                hash,
            },
        );
        // Re-key every later index (their rank shifted by +1).
        for l in &mut self.leaves[pos + 1..] {
            if let Some(slot) = self.index.get_mut(&l.member_id) {
                *slot += 1;
            }
        }
        self.index.insert(member_id, pos);
        self.recompute_suffix(pos);
        Ok(())
    }

    /// Replace an existing leaf's canonical bytes (and thus its hash). The
    /// identity key is unchanged, so the rank is unchanged — only the `O(log n)`
    /// path to the root is recomputed.
    ///
    /// Returns the previous canonical bytes.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the identity is absent.
    pub fn replace_existing(
        &mut self,
        member_id: &PrincipalId,
        canonical: Vec<u8>,
    ) -> Result<Vec<u8>, Reject> {
        validate_keyed_record(member_id, &canonical)?;
        let &pos = self.index.get(member_id).ok_or(Reject::InvalidContent)?;
        let hash = member_leaf_hash(&canonical);
        self.hashes_computed += 1;
        let prev = std::mem::replace(&mut self.leaves[pos].canonical, canonical);
        self.leaves[pos].hash = hash;
        self.recompute_path(pos);
        Ok(prev)
    }

    /// Physically remove the leaf at `member_id`. Returns its canonical bytes.
    ///
    /// This is the low-level map deletion (spec §3.4 #36 / §3.4 #40) used by
    /// deterministic tests and the add/remove acceptance; member revocation in
    /// the projection layer is a value replacement (tombstone), not a physical
    /// removal.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidContent`] if the identity is absent.
    pub fn remove_existing(&mut self, member_id: &PrincipalId) -> Result<Vec<u8>, Reject> {
        let &pos = self.index.get(member_id).ok_or(Reject::InvalidContent)?;
        let removed = self.leaves.remove(pos);
        self.index.remove(member_id);
        // Re-key every later index (their rank shifted by -1).
        for l in &self.leaves[pos..] {
            if let Some(slot) = self.index.get_mut(&l.member_id) {
                *slot -= 1;
            }
        }
        if self.leaves.is_empty() {
            self.levels.clear();
        } else {
            self.recompute_suffix(pos.min(self.leaves.len()));
        }
        Ok(removed.canonical)
    }

    /// Build an inclusion proof for `member_id` (spec §3.5 / D8). Returns `None`
    /// for an absent identity — no proof is fabricated.
    #[must_use]
    pub fn prove(&self, member_id: &PrincipalId) -> Option<InclusionProof> {
        let &pos = self.index.get(member_id)?;
        let mut siblings = Vec::new();
        let mut index = u64::try_from(pos).ok()?;
        let mut count = u64::try_from(self.leaves.len()).ok()?;
        let mut level_idx = 0usize;
        while count > 1 {
            if index % 2 == 1 {
                // Left sibling at index - 1.
                let sib_idx = usize::try_from(index - 1).ok()?;
                let sib = self.level_node(level_idx, sib_idx);
                siblings.push(ProofStep {
                    side: SiblingSide::Left,
                    hash: sib,
                });
            } else if index + 1 < count {
                // Right sibling at index + 1.
                let sib_idx = usize::try_from(index + 1).ok()?;
                let sib = self.level_node(level_idx, sib_idx);
                siblings.push(ProofStep {
                    side: SiblingSide::Right,
                    hash: sib,
                });
            }
            // else: unpaired trailing node — promoted unchanged, no step.
            count = count / 2 + count % 2;
            index /= 2;
            level_idx += 1;
        }
        Some(InclusionProof {
            leaf_count: u64::try_from(self.leaves.len()).ok()?,
            leaf_index: u64::try_from(pos).ok()?,
            siblings,
        })
    }

    /// Read a node hash at `(level, index)`. Level 0 = leaves; level L = the
    /// L-th pairing layer. Falls back to the materialized level (rebuilt lazily
    /// by the proof path only when needed).
    fn level_node(&self, level: usize, index: usize) -> Hash {
        if level == 0 {
            return self.leaves[index].hash;
        }
        // Materialized levels exist for non-empty trees after a build/mutation.
        // `levels[0]` = leaves, so level L lives at `levels[L]`.
        self.levels
            .get(level)
            .map_or_else(|| self.compute_level_node(level, index), |lvl| lvl[index])
    }

    /// Recompute a single node hash at `(level, index)` on demand (used only by
    /// the proof path when materialized levels are absent — they are always
    /// present in this index, but defended for future internal-shape changes).
    fn compute_level_node(&self, level: usize, index: usize) -> Hash {
        if level == 0 {
            return self.leaves[index].hash;
        }
        let left = self.compute_level_node(level - 1, 2 * index);
        // Odd trailing promotion: if there is no right child, this node IS the
        // promoted left child unchanged.
        let right_idx = 2 * index + 1;
        let level_below_len = self.level_len(level - 1);
        if right_idx >= level_below_len {
            return left;
        }
        let right = self.compute_level_node(level - 1, right_idx);
        merkle_node_hash(&left, &right)
    }

    /// Number of nodes at `level` (level 0 = leaf count).
    fn level_len(&self, level: usize) -> usize {
        let mut n = self.leaves.len();
        for _ in 0..level {
            n = n / 2 + n % 2;
        }
        n
    }

    /// The root from the materialized top level.
    fn levels_last_root(&self) -> Hash {
        // Always present for non-empty trees; defensive fallback rebuilds.
        self.levels
            .last()
            .and_then(|top| top.first().copied())
            .unwrap_or_else(|| {
                let hashes: Vec<Hash> = self.leaves.iter().map(|l| l.hash).collect();
                reduce_levels(&hashes)
            })
    }
    fn rebuild_levels(&mut self) {
        self.levels.clear();
        if self.leaves.is_empty() {
            return;
        }
        self.levels
            .push(self.leaves.iter().map(|leaf| leaf.hash).collect());
        while self.levels.last().is_some_and(|level| level.len() > 1) {
            let current = self.levels.last().expect("non-empty level");
            let mut next = Vec::with_capacity(current.len().div_ceil(2));
            let mut pairs = current.chunks_exact(2);
            next.extend(
                pairs
                    .by_ref()
                    .map(|pair| merkle_node_hash(&pair[0], &pair[1])),
            );
            if let Some(last) = pairs.remainder().first() {
                next.push(*last);
            }
            self.levels.push(next);
        }
    }

    /// Recompute the leaf→root path for an in-place replace at rank `pos`
    /// (`O(log n)` hashes; every other hash reused).
    fn recompute_path(&mut self, pos: usize) {
        if self.leaves.is_empty() {
            self.levels.clear();
            return;
        }
        self.resize_levels();
        // Level 0: refresh the single leaf hash.
        self.levels[0][pos] = self.leaves[pos].hash;
        let mut index = pos;
        for level in 0..self.levels.len().saturating_sub(1) {
            let parent = index / 2;
            let left = 2 * parent;
            let right = 2 * parent + 1;
            let new = if right < self.levels[level].len() {
                self.hashes_computed += 1;
                merkle_node_hash(&self.levels[level][left], &self.levels[level][right])
            } else {
                // Odd trailing node at this level: promoted unchanged.
                self.levels[level][left]
            };
            self.levels[level + 1][parent] = new;
            index = parent;
        }
    }

    /// Recompute every level from the first affected position `pos` upward
    /// (suffix recomputation). The prefix `[0, pos)` at every level is reused.
    fn recompute_suffix(&mut self, pos: usize) {
        if self.leaves.is_empty() {
            self.levels.clear();
            return;
        }
        self.resize_levels();
        // Level 0: refresh leaf hashes from `pos`.
        for (i, leaf) in self.leaves[pos..].iter().enumerate() {
            self.levels[0][pos + i] = leaf.hash;
        }
        let mut start = pos;
        for level in 0..self.levels.len().saturating_sub(1) {
            let below_len = self.levels[level].len();
            // The first parent whose left/right child is at or after `start`.
            let parent_start = start / 2;
            for parent in parent_start..self.levels[level + 1].len() {
                let left = 2 * parent;
                let right = 2 * parent + 1;
                let new = if right < below_len {
                    self.hashes_computed += 1;
                    merkle_node_hash(&self.levels[level][left], &self.levels[level][right])
                } else {
                    // Odd trailing node: promoted unchanged.
                    self.levels[level][left]
                };
                self.levels[level + 1][parent] = new;
            }
            start = parent_start;
        }
    }

    /// Adjust each level's Vec length to match the current leaf count WITHOUT
    /// recomputing hashes (the suffix/path recompute does the hashing). Existing
    /// prefix hashes are preserved across resize; new/stale suffix slots are
    /// filled by the caller's recompute. This keeps a single end-insert to
    /// `O(log n)` rather than the `O(n)` a full rebuild would cost.
    fn resize_levels(&mut self) {
        let n = self.leaves.len();
        if n == 0 {
            self.levels.clear();
            return;
        }
        // Compute the target length of every level (level 0 = n nodes).
        let mut sizes: Vec<usize> = Vec::new();
        let mut m = n;
        loop {
            sizes.push(m);
            if m == 1 {
                break;
            }
            m = m / 2 + m % 2;
        }
        // Ensure the outer Vec has exactly `sizes.len()` levels.
        if self.levels.len() != sizes.len() {
            self.levels.resize_with(sizes.len(), Vec::new);
        }
        // Resize each level's Vec to its target length. Existing entries are
        // kept (the prefix is unchanged); grown slots are zero-filled and will
        // be overwritten by the recompute. Shrunk levels just drop the tail.
        for (level_vec, &target) in self.levels.iter_mut().zip(&sizes) {
            if level_vec.len() != target {
                level_vec.resize(target, [0u8; LEN]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> PrincipalId {
        PrincipalId::from_bytes([byte; LEN])
    }

    fn rec(byte: u8) -> Vec<u8> {
        super::super::projected::ProjectedMemberRecord::genesis_admin(
            id(byte),
            super::super::projected::RoleLabel::from(crate::governance::log::model::Role::Member),
        )
        .canonical_bytes()
    }

    #[test]
    fn empty_root_matches_independent_constant() {
        let root = empty_member_root();
        let recomputed = {
            let mut h = blake3::Hasher::new();
            h.update(MERKLE_NODE);
            h.update(&[0x40]);
            *h.finalize().as_bytes()
        };
        assert_eq!(root.as_bytes(), &recomputed);
    }

    #[test]
    fn one_leaf_root_is_leaf_hash() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        let root = map.root();
        let leaf = member_leaf_hash(&rec(0x02));
        assert_eq!(root.as_bytes(), &leaf);
    }

    #[test]
    fn two_leaf_root_is_node_of_two_leaves() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        let root = map.root();
        let l0 = member_leaf_hash(&rec(0x01));
        let l1 = member_leaf_hash(&rec(0x02));
        assert_eq!(root.as_bytes(), &merkle_node_hash(&l0, &l1));
    }

    #[test]
    fn three_leaf_root_promotes_third_unchanged() {
        // root = node(node(L0,L1), L2) — L2 is promoted unchanged, NOT duplicated.
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        map.insert_new(id(0x03), rec(0x03)).unwrap();
        let root = map.root();
        let l0 = member_leaf_hash(&rec(0x01));
        let l1 = member_leaf_hash(&rec(0x02));
        let l2 = member_leaf_hash(&rec(0x03));
        let promoted = merkle_node_hash(&l0, &l1);
        let unchanged_promotion = merkle_node_hash(&promoted, &l2);
        // The duplicate-last interpretation would hash l2 with itself first.
        let duplicate_last = merkle_node_hash(&promoted, &merkle_node_hash(&l2, &l2));
        assert_eq!(root.as_bytes(), &unchanged_promotion);
        assert_ne!(root.as_bytes(), &duplicate_last);
    }

    #[test]
    fn incremental_equals_full_rebuild_after_each_op() {
        let mut map = SortedMerkleMap::new();
        let mut oracle_records: Vec<(PrincipalId, Vec<u8>)> = Vec::new();
        for byte in [0x07u8, 0x02, 0x09, 0x01, 0x05, 0x08, 0x03] {
            map.insert_new(id(byte), rec(byte)).unwrap();
            oracle_records.push((id(byte), rec(byte)));
            let oracle =
                rebuild_root(oracle_records.iter().map(|(k, v)| (k, v.as_slice()))).unwrap();
            assert_eq!(map.root(), oracle, "insert byte {byte:#x}");
        }
        // Replace one.
        let mut replacement = super::super::projected::ProjectedMemberRecord::genesis_admin(
            id(0x05),
            super::super::projected::RoleLabel::from(crate::governance::log::model::Role::Member),
        );
        replacement.grant_seq = 5;
        let replacement = replacement.canonical_bytes();
        map.replace_existing(&id(0x05), replacement.clone())
            .unwrap();
        let pos = oracle_records
            .iter()
            .position(|(k, _)| *k == id(0x05))
            .unwrap();
        oracle_records[pos].1 = replacement;
        assert_eq!(
            map.root(),
            rebuild_root(oracle_records.iter().map(|(k, v)| (k, v.as_slice()))).unwrap()
        );
        // Remove one.
        map.remove_existing(&id(0x02)).unwrap();
        oracle_records.retain(|(k, _)| *k != id(0x02));
        assert_eq!(
            map.root(),
            rebuild_root(oracle_records.iter().map(|(k, v)| (k, v.as_slice()))).unwrap()
        );
    }

    #[test]
    fn prove_and_verify_round_trip() {
        let mut map = SortedMerkleMap::new();
        for byte in 0x01..=0x06u8 {
            map.insert_new(id(byte), rec(byte)).unwrap();
        }
        let root = map.root();
        for byte in 0x01..=0x06u8 {
            let proof = map.prove(&id(byte)).expect("present");
            verify_inclusion(&root, &id(byte), &rec(byte), &proof).expect("verifies");
        }
    }

    #[test]
    fn inclusion_proof_bytes_round_trip() {
        let mut map = SortedMerkleMap::new();
        for byte in 0x01..=0x06u8 {
            map.insert_new(id(byte), rec(byte)).unwrap();
        }
        for byte in 0x01..=0x06u8 {
            let proof = map.prove(&id(byte)).unwrap();
            let bytes = proof.canonical_bytes();
            let decoded = decode_inclusion_proof(&bytes).unwrap();
            assert_eq!(decoded, proof);
            assert_eq!(decoded.canonical_bytes(), bytes);
        }
    }

    #[test]
    fn proof_decoder_rejects_oversized_declared_counts_before_payload() {
        let oversized = [
            0xa3, 0x68, b's', b'i', b'b', b'l', b'i', b'n', b'g', b's', 0x9b, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff,
        ];
        assert_eq!(
            decode_inclusion_proof(&oversized),
            Err(Reject::InvalidMerkleProof)
        );

        let sixty_five = [
            0xa3, 0x68, b's', b'i', b'b', b'l', b'i', b'n', b'g', b's', 0x98, 0x41,
        ];
        assert_eq!(
            decode_inclusion_proof(&sixty_five),
            Err(Reject::InvalidMerkleProof)
        );
    }

    #[test]
    fn proof_decoder_rejects_wrong_hash_width() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        let mut bytes = map.prove(&id(0x01)).unwrap().canonical_bytes();
        let width = bytes
            .windows(2)
            .position(|window| window == [0x58, 0x20])
            .unwrap();
        bytes[width + 1] = 0x1f;
        assert_eq!(
            decode_inclusion_proof(&bytes),
            Err(Reject::InvalidMerkleProof)
        );
    }

    #[test]
    fn proof_decoder_rejects_unknown_and_malformed_fields() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        let bytes = map.prove(&id(0x01)).unwrap().canonical_bytes();

        let mut unknown_top_level = bytes.clone();
        let offset = unknown_top_level
            .windows(8)
            .position(|window| window == b"siblings")
            .unwrap();
        unknown_top_level[offset + 7] = b'x';

        let mut unknown_nested = bytes.clone();
        let offset = unknown_nested
            .windows(4)
            .position(|window| window == b"hash")
            .unwrap();
        unknown_nested[offset] = b'b';

        let mut malformed_side = bytes.clone();
        let offset = malformed_side
            .windows(5)
            .position(|window| window == b"right")
            .unwrap();
        malformed_side[offset] = b'R';

        let mut trailing = bytes.clone();
        trailing.push(0);

        let mut non_shortest_map = bytes.clone();
        non_shortest_map.splice(..1, [0xb8, 0x03]);

        for invalid in [
            unknown_top_level,
            unknown_nested,
            malformed_side,
            trailing,
            non_shortest_map,
        ] {
            assert_eq!(
                decode_inclusion_proof(&invalid),
                Err(Reject::InvalidMerkleProof)
            );
        }
    }

    #[test]
    fn proof_decoder_rejects_invalid_path_structure() {
        let step = ProofStep {
            side: SiblingSide::Left,
            hash: [0; LEN],
        };
        for invalid in [
            InclusionProof {
                leaf_count: 0,
                leaf_index: 0,
                siblings: Vec::new(),
            },
            InclusionProof {
                leaf_count: 1,
                leaf_index: 1,
                siblings: Vec::new(),
            },
            InclusionProof {
                leaf_count: 2,
                leaf_index: 0,
                siblings: vec![step],
            },
            InclusionProof {
                leaf_count: 2,
                leaf_index: 0,
                siblings: Vec::new(),
            },
        ] {
            assert_eq!(
                decode_inclusion_proof(&invalid.canonical_bytes()),
                Err(Reject::InvalidMerkleProof)
            );
        }
    }

    #[test]
    fn absent_member_has_no_proof() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        assert!(map.prove(&id(0xff)).is_none());
    }

    #[test]
    fn proof_rejected_for_wrong_record() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        map.insert_new(id(0x02), rec(0x02)).unwrap();
        let root = map.root();
        let proof = map.prove(&id(0x01)).unwrap();
        // Verify against the wrong canonical bytes.
        assert_eq!(
            verify_inclusion(&root, &id(0x02), &rec(0x02), &proof),
            Err(Reject::InvalidMerkleProof)
        );
    }

    #[test]
    fn proof_rejected_for_wrong_root() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        let proof = map.prove(&id(0x01)).unwrap();
        let bad_root = MerkleRoot::from_bytes([0xff; LEN]);
        assert_eq!(
            verify_inclusion(&bad_root, &id(0x01), &rec(0x01), &proof),
            Err(Reject::InvalidMerkleProof)
        );
    }

    #[test]
    fn duplicate_insert_rejected() {
        let mut map = SortedMerkleMap::new();
        map.insert_new(id(0x01), rec(0x01)).unwrap();
        assert_eq!(
            map.insert_new(id(0x01), rec(0x01)),
            Err(Reject::InvalidContent)
        );
    }

    #[test]
    fn expected_sibling_count_matches_three_leaf_promotion() {
        // Three-leaf tree: index 2 is promoted twice with no sibling step.
        assert_eq!(expected_sibling_count(3, 0), 2);
        assert_eq!(expected_sibling_count(3, 1), 2);
        assert_eq!(expected_sibling_count(3, 2), 1);
        // The promoted leaf 2 has exactly one sibling (the node(L0,L1)).
        let mut map = SortedMerkleMap::new();
        for b in 0x01..=0x03u8 {
            map.insert_new(id(b), rec(b)).unwrap();
        }
        let proof = map.prove(&id(0x03)).unwrap();
        assert_eq!(proof.siblings.len(), 1);
        assert_eq!(proof.siblings[0].side, SiblingSide::Left);
    }
}
