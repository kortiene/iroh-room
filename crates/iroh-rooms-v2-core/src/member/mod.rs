//! Member projection and the sparse Merkle map (spec §6.5 / §4 D7 / #151).

pub mod merkle;
pub mod projection;

pub use merkle::{map_key, MerkleMap, Proof};
pub use projection::{project, MemberLeaf, MemberProjection};
