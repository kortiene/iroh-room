//! Deterministic sparse Merkle map over 256-bit keys (spec §4 D7, issue #151).
//!
//! Hashing (all BLAKE3, domain-separated):
//!
//! - **map key**: `BLAKE3(MERKLE_KEY || logical_key_bytes)` (32 bytes);
//! - **leaf hash**: `BLAKE3(MEMBER_LEAF || key || value_hash)` (frozen `#134 §6.2`);
//! - **internal hash**: `BLAKE3(MERKLE_NODE || left_hash || right_hash)` (frozen `#134 §6.2`);
//! - **empty node at depth `d`**: `BLAKE3(MERKLE_EMPTY_CONTEXT || d_be)` where
//!   `d` is the depth from the root (root is depth 0; leaves are depth 255).
//!
//! The map is sparse: only set leaves are materialized; absent subtrees hash to
//! the precomputed depth-specific empty hash. Root computation, proof generation,
//! and proof verification are all `O(256)`.

use core::cmp::Ordering;

use crate::cbor::{self, CborValue};
use crate::domain::{self, MEMBER_LEAF, MERKLE_EMPTY, MERKLE_KEY, MERKLE_NODE};
use crate::error::Reject;
use crate::ids::{MerkleRoot, LEN};

/// Merkle tree depth: one level per key bit (256-bit keys → 256 levels).
pub const DEPTH: usize = 256;

/// A 256-bit Merkle key.
pub type Key = [u8; LEN];

/// A 256-bit Merkle hash.
pub type Hash = [u8; LEN];

/// The bit value of `key` at `depth` (depth 0 = root; the most-significant bit
/// of `key[0]` decides the root's left/right child).
fn bit(key: &Key, depth: usize) -> bool {
    let byte = depth / 8;
    let bit = 7 - (depth % 8);
    (key[byte] >> bit) & 1 == 1
}

/// The base empty hash at the leaf level (depth `DEPTH`):
/// `BLAKE3(MERKLE_EMPTY || DEPTH_be)` (D7).
fn empty_base() -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MERKLE_EMPTY);
    hasher.update(&(DEPTH as u64).to_be_bytes());
    *hasher.finalize().as_bytes()
}

/// The depth-indexed empty-subtree hashes, computed by the **recurrence**
/// `empty[d] = node_hash(empty[d+1], empty[d+1])` with `empty[DEPTH] = base`.
///
/// Using the recurrence (rather than a per-depth independent domain hash) is what
/// makes an empty subtree hash identical whether computed directly by the root
/// builder or reconstructed from sibling empties in a proof — the property every
/// Merkle proof must have.
#[must_use]
pub fn empty_table() -> Vec<Hash> {
    let mut table = vec![[0u8; LEN]; DEPTH + 1];
    table[DEPTH] = empty_base();
    for d in (0..DEPTH).rev() {
        table[d] = node_hash(&table[d + 1], &table[d + 1]);
    }
    table
}

/// The map key for a logical key: `BLAKE3(MERKLE_KEY || logical_key)` (D7).
#[must_use]
pub fn map_key(logical_key: &[u8]) -> Key {
    domain::blake3_domain(MERKLE_KEY, logical_key)
}

/// The leaf hash: `BLAKE3(MEMBER_LEAF || key || value_hash)` (D7; frozen `#134 §6.2`).
#[must_use]
pub fn leaf_hash(key: &Key, value_hash: &Hash) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MEMBER_LEAF);
    hasher.update(key);
    hasher.update(value_hash);
    *hasher.finalize().as_bytes()
}

/// The internal-node hash: `BLAKE3(MERKLE_NODE || left || right)` (D7; frozen `#134 §6.2`).
#[must_use]
pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MERKLE_NODE);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

/// The canonical value hash: `BLAKE3` over the deterministic-CBOR encoding of a
/// [`CborValue`]. Used so the Merkle map commits to canonical bytes, not in-
/// memory layout.
#[must_use]
pub fn value_hash(value: &CborValue) -> Hash {
    *blake3::hash(&cbor::encode(value)).as_bytes()
}

/// A sparse Merkle map. Leaves are held keyed by their 256-bit map key in
/// deterministic order. Insertions are stable; the root is computed on demand.
#[derive(Debug, Clone, Default)]
pub struct MerkleMap {
    leaves: std::collections::BTreeMap<Key, Hash>,
}

impl MerkleMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of set leaves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Whether the map has no set leaves.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Insert a leaf, deriving its hash from a canonical-CBOR value.
    pub fn insert_value(&mut self, logical_key: &[u8], value: &CborValue) {
        let key = map_key(logical_key);
        let h = leaf_hash(&key, &value_hash(value));
        self.leaves.insert(key, h);
    }

    /// Insert a leaf whose hash is supplied directly (the value-hash boundary is
    /// the caller's responsibility). Used when the value is already canonicalized
    /// elsewhere (e.g. a member leaf).
    pub fn insert_hash(&mut self, key: Key, leaf: Hash) {
        self.leaves.insert(key, leaf);
    }

    /// Borrow a leaf hash by map key, if present.
    #[must_use]
    pub fn get(&self, key: &Key) -> Option<&Hash> {
        self.leaves.get(key)
    }

    /// The Merkle root: recursively combine set leaves with the empty-subtree
    /// hashes. `O(n * DEPTH)` for `n` leaves, where `n` is small (the member set).
    #[must_use]
    pub fn root(&self) -> MerkleRoot {
        let empty = empty_table();
        MerkleRoot::from_bytes(self.recurse_root(
            0,
            &self.leaves.keys().copied().collect::<Vec<_>>(),
            &empty,
        ))
    }

    /// Recursively compute the subtree root at `depth` over the set keys whose
    /// prefix matches the path so far.
    fn recurse_root(&self, depth: usize, keys: &[Key], empty: &[Hash]) -> Hash {
        // Base case: an empty bucket at any depth hashes to that depth's empty.
        if keys.is_empty() {
            return empty[depth.min(DEPTH)];
        }
        if depth == DEPTH {
            // Exactly one key remains at the leaf; its stored hash is the leaf.
            return self
                .leaves
                .get(&keys[0])
                .copied()
                .unwrap_or_else(|| empty[DEPTH]);
        }
        // Split keys by the bit at this depth.
        let mut left = Vec::new();
        let mut right = Vec::new();
        for k in keys {
            if bit(k, depth) {
                right.push(*k);
            } else {
                left.push(*k);
            }
        }
        let l = self.recurse_root(depth + 1, &left, empty);
        let r = self.recurse_root(depth + 1, &right, empty);
        node_hash(&l, &r)
    }

    /// Build an inclusion proof for `logical_key` (the proof commits to presence
    /// and the leaf hash). Returns `None` if the key is not set.
    #[must_use]
    pub fn prove_inclusion(&self, logical_key: &[u8]) -> Option<Proof> {
        let key = map_key(logical_key);
        let leaf = *self.leaves.get(&key)?;
        let empty = empty_table();
        let siblings = self.sibling_path(&key, &empty);
        Some(Proof {
            key,
            leaf: Some(leaf),
            siblings,
        })
    }

    /// Build an exclusion proof for `logical_key` (proves the key is absent by
    /// exhibiting the actual leaf occupying its path, or emptiness).
    #[must_use]
    pub fn prove_exclusion(&self, logical_key: &[u8]) -> Proof {
        let key = map_key(logical_key);
        let empty = empty_table();
        let siblings = self.sibling_path(&key, &empty);
        Proof {
            key,
            leaf: self.leaves.get(&key).copied(),
            siblings,
        }
    }

    /// The sibling hashes along `key`'s path, **root-to-leaf order** (index =
    /// depth): `siblings[d]` is the subtree hash on the opposite side of `key` at
    /// bit `d`, computed over keys that share `key`'s prefix bits `0..d-1`. This
    /// follows the tree's actual branching (MSB first, shallow→deep).
    fn sibling_path(&self, key: &Key, empty: &[Hash]) -> Vec<Hash> {
        let mut siblings: Vec<Hash> = Vec::with_capacity(DEPTH);
        let mut bucket: Vec<Key> = self.leaves.keys().copied().collect();
        for depth in 0..DEPTH {
            let kb = bit(key, depth);
            let (same, other): (Vec<Key>, Vec<Key>) =
                bucket.iter().copied().partition(|k| bit(k, depth) == kb);
            siblings.push(self.recurse_root(depth + 1, &other, empty));
            bucket = same;
        }
        siblings
    }
}

/// A Merkle proof (spec §6.5). Canonical-CBOR-serializable: `{ key, leaf, siblings }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// The searched 256-bit map key.
    pub key: Key,
    /// The leaf hash at `key`, if set (`None` for exclusion of an absent key).
    pub leaf: Option<Hash>,
    /// Sibling subtree hashes from leaf (depth DEPTH) up to depth 1.
    pub siblings: Vec<Hash>,
}

impl Proof {
    /// Verify this proof against an expected root. Reconstructs the root from the
    /// leaf + sibling path and checks it matches. An inclusion proof requires a
    /// present leaf; an exclusion proof may have `leaf == None`.
    ///
    /// # Errors
    /// Returns [`Reject::InvalidMerkleProof`] if the reconstructed root differs
    /// from `root`, or an inclusion proof has no leaf.
    pub fn verify(&self, root: &MerkleRoot, require_inclusion: bool) -> Result<(), Reject> {
        if self.siblings.len() != DEPTH {
            return Err(Reject::InvalidMerkleProof);
        }
        let empty = empty_table();
        let mut acc = match (self.leaf, require_inclusion) {
            (Some(h), _) => h,
            (None, true) => return Err(Reject::InvalidMerkleProof),
            (None, false) => empty[DEPTH],
        };
        // Reconstruct leaf→root: at each depth d (deepest first), combine `acc`
        // (key's subtree at depth d+1) with `siblings[d]` (the opposite side),
        // using bit d to pick the order.
        for depth in (0..DEPTH).rev() {
            let sibling = &self.siblings[depth];
            acc = if bit(&self.key, depth) {
                node_hash(sibling, &acc)
            } else {
                node_hash(&acc, sibling)
            };
        }
        if MerkleRoot::from_bytes(acc) == *root {
            Ok(())
        } else {
            Err(Reject::InvalidMerkleProof)
        }
    }

    /// Encode this proof as canonical CBOR (spec §6.5 / D7 proof format).
    #[must_use]
    pub fn to_cbor_value(&self) -> CborValue {
        let leaf = match self.leaf {
            Some(h) => CborValue::Bytes(h.to_vec()),
            None => CborValue::Array(vec![]),
        };
        CborValue::Map(vec![
            ("key".to_owned(), CborValue::Bytes(self.key.to_vec())),
            ("leaf".to_owned(), leaf),
            (
                "siblings".to_owned(),
                CborValue::Array(
                    self.siblings
                        .iter()
                        .map(|h| CborValue::Bytes(h.to_vec()))
                        .collect(),
                ),
            ),
        ])
    }
}

/// Lexicographic comparison helper for stable leaf ordering (already provided by
/// `BTreeMap<Key, _>`, exposed for tests/docs).
#[must_use]
pub fn cmp_key(a: &Key, b: &Key) -> Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(n: u64) -> CborValue {
        CborValue::Uint(n)
    }

    #[test]
    fn empty_map_root_is_depth_zero_empty_hash() {
        let map = MerkleMap::new();
        let empty = empty_table();
        assert_eq!(*map.root().as_bytes(), empty[0]);
    }

    #[test]
    fn one_leaf_root_round_trips_through_proof() {
        let mut map = MerkleMap::new();
        map.insert_value(b"alice", &v(1));
        let root = map.root();
        let proof = map.prove_inclusion(b"alice").expect("leaf set");
        proof.verify(&root, true).expect("inclusion proof verifies");
    }

    #[test]
    fn two_leaves_divergent_keys_have_stable_root() {
        let mut a = MerkleMap::new();
        a.insert_value(b"alice", &v(1));
        a.insert_value(b"bob", &v(2));
        let root_a = a.root();

        // Rebuild in a different insertion order — root must be identical.
        let mut b = MerkleMap::new();
        b.insert_value(b"bob", &v(2));
        b.insert_value(b"alice", &v(1));
        assert_eq!(b.root(), root_a, "root is insertion-order independent");

        // A different value changes the root.
        let mut c = MerkleMap::new();
        c.insert_value(b"alice", &v(9));
        c.insert_value(b"bob", &v(2));
        assert_ne!(c.root(), root_a, "value change changes root");
    }

    #[test]
    fn exclusion_proof_verifies_for_absent_key() {
        let mut map = MerkleMap::new();
        map.insert_value(b"alice", &v(1));
        let root = map.root();
        let proof = map.prove_exclusion(b"bob");
        assert!(proof.leaf.is_none(), "absent key has no leaf");
        proof
            .verify(&root, false)
            .expect("exclusion proof verifies");
        // Requiring inclusion on an exclusion proof must fail.
        assert_eq!(proof.verify(&root, true), Err(Reject::InvalidMerkleProof));
    }

    #[test]
    fn malformed_proof_rejected() {
        let mut map = MerkleMap::new();
        map.insert_value(b"alice", &v(1));
        let root = map.root();
        let mut proof = map.prove_inclusion(b"alice").expect("leaf set");
        // Corrupt one sibling.
        proof.siblings[0] = [0xff; LEN];
        assert_eq!(proof.verify(&root, true), Err(Reject::InvalidMerkleProof));
    }

    #[test]
    fn depth_specific_empty_hashes_differ() {
        // Empty hashes at distinct depths must differ (D7 depth-specificity),
        // and the recurrence empty[d] = node(empty[d+1], empty[d+1]) must hold.
        let empty = empty_table();
        assert_ne!(empty[0], empty[1]);
        assert_ne!(empty[1], empty[255]);
        assert_eq!(empty[0], node_hash(&empty[1], &empty[1]));
        assert_eq!(empty[254], node_hash(&empty[255], &empty[255]));
    }

    #[test]
    fn map_key_and_leaf_hash_are_domain_separated() {
        let k = map_key(b"x");
        let lh = leaf_hash(&k, &[0u8; LEN]);
        // Leaf hash != map key (different domains).
        assert_ne!(k, lh);
        // node_hash is symmetric in its inputs only by construction domain, so
        // swapping inputs yields a different hash (non-commutative commitment).
        let a = [1u8; LEN];
        let b = [2u8; LEN];
        assert_ne!(node_hash(&a, &b), node_hash(&b, &a));
    }
}
