//! Governance checkpoints and snapshot hash (spec §4 / §6 / #150).
//!
//! A checkpoint is a signed commitment to a folded state at a point in time:
//!
//! ```text
//! CheckpointBody = {
//!   schema_version, room_id, state_root, member_root,
//!   governance_tip, unresolved_forks, epoch, seq
//! }
//! snapshot_hash = BLAKE3(SNAPSHOT_HASH_CONTEXT || canonical_snapshot_bytes)
//! ```
//!
//! Validation recomputes the state root, member root, and snapshot hash from the
//! supplied state and rejects on any mismatch (spec §11: roots must commit to all
//! authorization-relevant state).

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::governance::model::{uint_field, ForkEvidence};
use crate::governance::state_root;
use crate::ids::{MerkleRoot, RoomId, SnapshotHash, StateRoot, LEN};
use crate::member::projection;
use crate::signed::{self, Envelope, SignedBody};
use crate::MemberId;

/// A signed governance checkpoint body (spec #150).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointBody {
    /// Schema version (MUST be `2`).
    pub schema_version: u64,
    /// The room.
    pub room_id: RoomId,
    /// The committed governance state root.
    pub state_root: StateRoot,
    /// The committed member Merkle root.
    pub member_root: MerkleRoot,
    /// The governance tip (accepted entry id) this checkpoint is taken at.
    pub governance_tip: Option<crate::ids::GovernanceEntryId>,
    /// Commitments to unresolved fork evidence at this checkpoint.
    pub unresolved_forks: Vec<[crate::ids::GovernanceEntryId; 2]>,
    /// Signed epoch (advisory; spec §11 no wall-clock authorization).
    pub epoch: u64,
    /// Monotonic checkpoint sequence (per the room; OQ-6).
    pub seq: u64,
}

impl SignedBody for CheckpointBody {
    type Id = SnapshotHash;
    const SIGN_CONTEXT: &'static [u8] = domain::CHECKPOINT_SIGN;
    const ID_CONTEXT: &'static [u8] = domain::SNAPSHOT_HASH;

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
                "state_root".to_owned(),
                CborValue::Bytes(self.state_root.as_bytes().to_vec()),
            ),
            (
                "member_root".to_owned(),
                CborValue::Bytes(self.member_root.as_bytes().to_vec()),
            ),
            ("epoch".to_owned(), CborValue::Uint(self.epoch)),
            ("seq".to_owned(), CborValue::Uint(self.seq)),
        ];
        if let Some(tip) = self.governance_tip {
            entries.push((
                "governance_tip".to_owned(),
                CborValue::Bytes(tip.as_bytes().to_vec()),
            ));
        }
        if !self.unresolved_forks.is_empty() {
            entries.push((
                "unresolved_forks".to_owned(),
                CborValue::Array(
                    self.unresolved_forks
                        .iter()
                        .map(|pair| {
                            CborValue::Array(
                                pair.iter()
                                    .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                                    .collect(),
                            )
                        })
                        .collect(),
                ),
            ));
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
                "state_root",
                "member_root",
                "epoch",
                "seq",
                "governance_tip",
                "unresolved_forks",
            ],
        )?;
        let schema_version = uint_field(entries, "schema_version")?;
        if schema_version != crate::governance::model::SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let room_id = read_room(entries)?;
        let state_root = read_root(entries, "state_root")?;
        let member_root = read_merkle(entries)?;
        let epoch = uint_field(entries, "epoch")?;
        let seq = uint_field(entries, "seq")?;
        let governance_tip = match signed::opt(entries, "governance_tip") {
            Some(v) => {
                let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
                Some(crate::ids::GovernanceEntryId::from_bytes(arr))
            }
            None => None,
        };
        let unresolved_forks = match signed::opt(entries, "unresolved_forks") {
            Some(v) => {
                let arr = v.as_array().ok_or(Reject::NonCanonicalEncoding)?;
                let mut out = Vec::with_capacity(arr.len());
                for pair_val in arr {
                    let pair = pair_val.as_array().ok_or(Reject::NonCanonicalEncoding)?;
                    if pair.len() != 2 {
                        return Err(Reject::InvalidContent);
                    }
                    let mut p = [crate::ids::GovernanceEntryId::from_bytes([0; LEN]); 2];
                    for (i, item) in pair.iter().enumerate() {
                        let bytes = item.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                        let b = <[u8; LEN]>::try_from(bytes)
                            .map_err(|_| Reject::NonCanonicalEncoding)?;
                        p[i] = crate::ids::GovernanceEntryId::from_bytes(b);
                    }
                    out.push(p);
                }
                out
            }
            None => Vec::new(),
        };
        Ok(Self {
            schema_version,
            room_id,
            state_root,
            member_root,
            governance_tip,
            unresolved_forks,
            epoch,
            seq,
        })
    }

    fn id_from_csb(csb: &[u8]) -> Self::Id {
        SnapshotHash::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
    }
}

/// A signed checkpoint envelope. Its id is the snapshot hash.
pub type SignedCheckpoint = Envelope<SnapshotHash>;

/// The recomputed snapshot hash for a checkpoint body.
#[must_use]
pub fn snapshot_hash(body: &CheckpointBody) -> SnapshotHash {
    SnapshotHash::from_bytes(domain::blake3_domain(
        domain::SNAPSHOT_HASH,
        &crate::cbor::encode(&body.to_cbor()),
    ))
}

/// Decode + verify a signed checkpoint's signature/canonicality (spec D2). Does
/// NOT validate the roots against a folded state — use [`validate_against_state`]
/// for that.
///
/// # Errors
/// See [`signed::verify_envelope`].
pub fn decode_verified(env: &SignedCheckpoint) -> Result<CheckpointBody, Reject> {
    signed::verify_envelope::<CheckpointBody>(env)
}

/// Validate a checkpoint's roots by recomputing them from the supplied folded
/// state (spec #150 / §11).
///
/// # Errors
/// - [`Reject::StateRootMismatch`] — recomputed state root differs.
/// - [`Reject::SnapshotHashMismatch`] — the checkpoint's snapshot hash differs
///   from the recomputed one (carried in the envelope id).
/// - [`Reject::InvalidContent`] — the unresolved-forks commitment does not match
///   the state's actual unresolved forks.
pub fn validate_against_state(
    env: &SignedCheckpoint,
    state: &crate::governance::model::GovernanceState,
) -> Result<CheckpointBody, Reject> {
    let body = decode_verified(env)?;
    let (member_root, _projection) = projection::project(state);
    state_root::verify(state, &member_root, &body.state_root)?;
    // Unresolved-forks commitment must match the state's actual unresolved forks.
    let actual: Vec<_> = state
        .forks
        .iter()
        .filter(|f| !f.resolved)
        .map(|f: &ForkEvidence| f.conflicting)
        .collect();
    if actual != body.unresolved_forks {
        return Err(Reject::InvalidContent);
    }
    // Snapshot hash = envelope id; recompute and compare.
    let recomputed = snapshot_hash(&body);
    if recomputed != env.id {
        return Err(Reject::SnapshotHashMismatch);
    }
    Ok(body)
}

fn read_room(entries: &[(String, CborValue)]) -> Result<RoomId, Reject> {
    let v = signed::field(entries, "room_id").ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(RoomId::from_bytes(arr))
}

fn read_root(entries: &[(String, CborValue)], key: &str) -> Result<StateRoot, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(StateRoot::from_bytes(arr))
}

fn read_merkle(entries: &[(String, CborValue)]) -> Result<MerkleRoot, Reject> {
    let v = signed::field(entries, "member_root").ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(MerkleRoot::from_bytes(arr))
}

#[allow(dead_code)]
fn _signer_marker(_: MemberId) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::fold::GovernanceFold;
    use crate::governance::model::{GovernanceAction, GovernanceEntryBody};
    use crate::ids::{DeviceId, GovernanceEntryId};
    use crate::keys::SigningKey;

    #[test]
    fn checkpoint_round_trips_and_validates() {
        let room = RoomId::from_bytes([0x90; LEN]);
        let admin = SigningKey::from_seed(&[0xa0; LEN]);
        let genesis = GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: admin.member_id(),
            seq: 1,
            parent: None,
            epoch: 1_000,
            action: GovernanceAction::InitRoom {
                admin: admin.member_id(),
                admin_device: admin.device_id(),
                room_name: "r".to_owned(),
            },
        };
        let outcome = GovernanceFold::new().entry(genesis).finish().unwrap();
        let checkpoint = CheckpointBody {
            schema_version: 2,
            room_id: room,
            state_root: outcome.state_root,
            member_root: outcome.member_root,
            governance_tip: Some(GovernanceEntryId::from_bytes([0; LEN])),
            unresolved_forks: Vec::new(),
            epoch: 1_001,
            seq: 1,
        };
        let env = signed::seal(&checkpoint, &admin);
        let decoded = validate_against_state(&env, &outcome.state).expect("checkpoint validates");
        assert_eq!(decoded.state_root, outcome.state_root);
    }

    #[test]
    fn checkpoint_with_wrong_state_root_rejected() {
        let room = RoomId::from_bytes([0x90; LEN]);
        let admin = SigningKey::from_seed(&[0xa0; LEN]);
        let genesis = GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: admin.member_id(),
            seq: 1,
            parent: None,
            epoch: 1_000,
            action: GovernanceAction::InitRoom {
                admin: admin.member_id(),
                admin_device: admin.device_id(),
                room_name: "r".to_owned(),
            },
        };
        let outcome = GovernanceFold::new().entry(genesis).finish().unwrap();
        let bad = CheckpointBody {
            schema_version: 2,
            room_id: room,
            state_root: StateRoot::from_bytes([0xff; LEN]),
            member_root: outcome.member_root,
            governance_tip: None,
            unresolved_forks: Vec::new(),
            epoch: 1,
            seq: 1,
        };
        let env = signed::seal(&bad, &admin);
        assert_eq!(
            validate_against_state(&env, &outcome.state).err(),
            Some(Reject::StateRootMismatch)
        );
    }

    #[test]
    fn member_state_change_changes_snapshot_hash() {
        let room = RoomId::from_bytes([0x90; LEN]);
        let mk = |state: &crate::governance::model::GovernanceState| {
            let body = CheckpointBody {
                schema_version: 2,
                room_id: room,
                state_root: state_root::compute(state, &projection::project(state).0),
                member_root: projection::project(state).0,
                governance_tip: None,
                unresolved_forks: Vec::new(),
                epoch: 1,
                seq: 1,
            };
            snapshot_hash(&body)
        };
        let admin = SigningKey::from_seed(&[0xa0; LEN]);
        let g = GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: admin.member_id(),
            seq: 1,
            parent: None,
            epoch: 1_000,
            action: GovernanceAction::InitRoom {
                admin: admin.member_id(),
                admin_device: admin.device_id(),
                room_name: "r".to_owned(),
            },
        };
        let before = mk(&GovernanceFold::new()
            .entry(g.clone())
            .finish()
            .unwrap()
            .state);
        let add = GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: admin.member_id(),
            seq: 2,
            parent: Some(crate::governance::fold::entry_id(&g)),
            epoch: 1_001,
            action: GovernanceAction::AddMember {
                member: MemberId::from_bytes([0xbe; LEN]),
                device: DeviceId::from_bytes([0; LEN]),
                role: crate::governance::model::Role::Member,
            },
        };
        let after = mk(&GovernanceFold::new()
            .entry(g)
            .entry(add)
            .finish()
            .unwrap()
            .state);
        assert_ne!(before, after);
    }
}
