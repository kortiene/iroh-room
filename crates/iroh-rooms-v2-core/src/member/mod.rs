//! Member projection and deterministic Merkle commitments (#134 §8 / #151).

pub mod merkle;
pub mod projected;
pub mod projection;
pub mod sorted;

/// Legacy sparse-tree compatibility surface for pre-#151 golden vectors.
pub mod legacy {
    pub use super::merkle::{map_key, MerkleMap, Proof};
    pub use super::projection::{project, MemberLeaf, MemberProjection};
}

pub use projected::{
    ActiveDevice, MemberMapProjection, ProjectedMemberRecord, ProjectedStatus, ProjectionUpdate,
    RoleLabel,
};
pub use sorted::{
    decode_inclusion_proof, empty_member_root, member_leaf_hash, merkle_node_hash,
    verify_inclusion, InclusionProof, ProofStep, SiblingSide, SortedMerkleMap,
};
