//! Fork/equivocation detection and the signed `fork.resolve` record (spec §4 D6,
//! #149).
//!
//! A **fork** is two accepted governance entries from the same author that occupy
//! the same per-author position: identical `seq`, or the same declared `parent`.
//! Detection is deterministic and **never picks a winner** — both conflicting
//! entries are carried as evidence, the author's authorization fails closed, and
//! state can only change through an authorized [`ForkResolutionBody`] (spec D6).
//!
//! # Action-set assumption (OQ-5)
//!
//! `#134 §7.5` exact `fork.resolve` enum is unavailable. This module implements a
//! concrete, safe resolution set — narrowed to only the transitions the
//! deterministic fold can actually enforce (spec D6 / #149):
//!
//! - `Accept { winner }` — keep one conflicting entry, drop the other; the fold
//!   re-derives state with the losing entry excluded;
//! - `Reject` — drop both conflicting entries; the fold re-derives state with
//!   both excluded.
//!
//! A `Supersede { new_state_root }` variant was intentionally **not** kept: the
//! fold's authoritative `state_root` is a pure function of the folded state, so
//! adopting an externally-supplied root verbatim would break that invariant and
//! let an authorized signer pin an arbitrary root divorced from the actual state.
//! The safe equivalent is `Reject` (drop both) followed by re-derivation.

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::governance::model::{reject_unknown_keys, uint_field, ForkEvidence};
use crate::ids::{GovernanceEntryId, LEN};
use crate::signed::{self, Envelope, SignedBody};

/// The `fork.resolve` action (spec D6 / OQ-5).
///
/// The set is deliberately narrowed to the transitions the fold can actually
/// enforce; see the module docs for why `Supersede` was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkResolveAction {
    /// Keep `winner` of the conflicting entries; the other is invalidated and
    /// its state effect is rolled back by re-deriving the fold without it.
    Accept {
        /// The winning entry id (must be one of the evidence pair).
        winner: GovernanceEntryId,
    },
    /// Drop both conflicting entries; the fold re-derives state without either.
    Reject,
}

/// The signed `fork.resolve` body (spec §6.3 / D6 / #149).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkResolutionBody {
    /// Schema version (MUST be `2`).
    pub schema_version: u64,
    /// The room.
    pub room_id: crate::ids::RoomId,
    /// The principal who authored (signed) this resolution. The envelope signer
    /// MUST equal this field (bound at [`decode_verified`]); authorization then
    /// gates the resolution on this signer being the admin (spec D6).
    pub signer: crate::MemberId,
    /// The fork being resolved (the two conflicting entry ids).
    pub evidence: [GovernanceEntryId; 2],
    /// The resolution action.
    pub action: ForkResolveAction,
    /// Signed epoch (advisory).
    pub epoch: u64,
}

impl SignedBody for ForkResolutionBody {
    type Id = crate::ids::SnapshotHash;
    const SIGN_CONTEXT: &'static [u8] = domain::FORK_RESOLVE_SIGN;
    const ID_CONTEXT: &'static [u8] = domain::FORK_RESOLVE_ID;

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
                "signer".to_owned(),
                CborValue::Bytes(self.signer.as_bytes().to_vec()),
            ),
            (
                "evidence".to_owned(),
                CborValue::Array(
                    self.evidence
                        .iter()
                        .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
            ("epoch".to_owned(), CborValue::Uint(self.epoch)),
        ];
        entries.push(("action".to_owned(), action_to_cbor(&self.action)));
        CborValue::Map(entries)
    }

    fn from_canonical(value: &CborValue) -> Result<Self, Reject> {
        let entries = value.as_map().ok_or(Reject::NonCanonicalEncoding)?;
        reject_unknown_keys(
            entries,
            &[
                "schema_version",
                "room_id",
                "signer",
                "evidence",
                "epoch",
                "action",
            ],
        )?;
        let schema_version = uint_field(entries, "schema_version")?;
        if schema_version != crate::governance::model::SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let room_id = read_room(entries)?;
        let signer = read_signer(entries)?;
        let evidence = read_evidence(entries)?;
        let epoch = uint_field(entries, "epoch")?;
        let action_val = signed::opt(entries, "action").ok_or(Reject::NonCanonicalEncoding)?;
        let action = action_from_cbor(action_val, &evidence)?;
        Ok(Self {
            schema_version,
            room_id,
            signer,
            evidence,
            action,
            epoch,
        })
    }

    fn id_from_csb(csb: &[u8]) -> Self::Id {
        crate::ids::SnapshotHash::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
    }
}

/// A signed fork-resolution envelope.
pub type SignedForkResolution = Envelope<crate::ids::SnapshotHash>;

/// Decode + verify a signed fork resolution (spec D2 / §6.3 / D6).
///
/// # Errors
/// See [`signed::verify_envelope`]; additionally enforces `schema_version == 2`,
/// that the `signer` field equals the verified envelope signer (else
/// [`Reject::InvalidForkResolution`]), and that `Accept.winner` is one of the
/// evidence pair (else [`Reject::InvalidForkResolution`]).
pub fn decode_verified(env: &SignedForkResolution) -> Result<ForkResolutionBody, Reject> {
    let body = signed::verify_envelope::<ForkResolutionBody>(env)?;
    // Bind the claimed signer to the key that actually signed the envelope, so a
    // resolution cannot claim authorship by anyone other than its signer (D6).
    if body.signer != env.signer {
        return Err(Reject::InvalidForkResolution);
    }
    // Cross-field rule: an Accept must pick one of the two evidence entries.
    if let ForkResolveAction::Accept { winner } = &body.action {
        if winner != &body.evidence[0] && winner != &body.evidence[1] {
            return Err(Reject::InvalidForkResolution);
        }
    }
    Ok(body)
}

/// Detect whether a newly-arrived entry forms a fork with an existing tip for
/// the same author. A **fork** is two entries from the same author occupying the
/// same per-author position: identical `seq` with different ids. (A new entry
/// that properly extends the tip — `seq == tip.seq + 1` — is never a fork.) Both
/// conflicting ids are returned in ascending order so the evidence is
/// deterministic and arrival-order-independent.
///
/// `new_parent` is accepted for API completeness but is not the fork signal: in
/// the per-author linear-chain model `seq` is the authoritative position.
#[must_use]
pub fn detect(
    author: crate::MemberId,
    new_id: GovernanceEntryId,
    new_seq: u64,
    _new_parent: Option<GovernanceEntryId>,
    existing_tip: Option<(u64, GovernanceEntryId)>,
) -> Option<ForkEvidence> {
    let (tip_seq, tip_id) = existing_tip?;
    // Same position, different entries → fork.
    if tip_seq != new_seq || tip_id == new_id {
        return None;
    }
    let mut pair = [tip_id, new_id];
    pair.sort();
    Some(ForkEvidence {
        author,
        conflicting: pair,
        seq: new_seq,
        resolved: false,
    })
}

fn read_room(entries: &[(String, CborValue)]) -> Result<crate::ids::RoomId, Reject> {
    let v = signed::field(entries, "room_id").ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(crate::ids::RoomId::from_bytes(arr))
}

fn read_signer(entries: &[(String, CborValue)]) -> Result<crate::MemberId, Reject> {
    let v = signed::field(entries, "signer").ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(crate::MemberId::from_bytes(arr))
}

fn read_evidence(entries: &[(String, CborValue)]) -> Result<[GovernanceEntryId; 2], Reject> {
    let v = signed::field(entries, "evidence").ok_or(Reject::NonCanonicalEncoding)?;
    let arr = v.as_array().ok_or(Reject::NonCanonicalEncoding)?;
    if arr.len() != 2 {
        return Err(Reject::InvalidForkResolution);
    }
    let mut out = [GovernanceEntryId::from_bytes([0; LEN]); 2];
    for (i, item) in arr.iter().enumerate() {
        let bytes = item.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
        let b = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
        out[i] = GovernanceEntryId::from_bytes(b);
    }
    Ok(out)
}

fn action_to_cbor(action: &ForkResolveAction) -> CborValue {
    match action {
        ForkResolveAction::Accept { winner } => CborValue::Map(vec![
            ("type".to_owned(), CborValue::Text("accept".to_owned())),
            (
                "winner".to_owned(),
                CborValue::Bytes(winner.as_bytes().to_vec()),
            ),
        ]),
        ForkResolveAction::Reject => CborValue::Map(vec![(
            "type".to_owned(),
            CborValue::Text("reject".to_owned()),
        )]),
    }
}

fn action_from_cbor(
    value: &CborValue,
    evidence: &[GovernanceEntryId; 2],
) -> Result<ForkResolveAction, Reject> {
    let entries = value.as_map().ok_or(Reject::InvalidForkResolution)?;
    let ty = signed::field(entries, "type")
        .and_then(|v| v.as_text())
        .ok_or(Reject::InvalidForkResolution)?;
    match ty {
        "accept" => {
            reject_unknown_action_keys(entries, &["type", "winner"])?;
            let v = signed::field(entries, "winner").ok_or(Reject::InvalidForkResolution)?;
            let bytes = v.as_bytes().ok_or(Reject::InvalidForkResolution)?;
            let b = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::InvalidForkResolution)?;
            let winner = GovernanceEntryId::from_bytes(b);
            if winner != evidence[0] && winner != evidence[1] {
                return Err(Reject::InvalidForkResolution);
            }
            Ok(ForkResolveAction::Accept { winner })
        }
        "reject" => {
            reject_unknown_action_keys(entries, &["type"])?;
            Ok(ForkResolveAction::Reject)
        }
        _ => Err(Reject::InvalidForkResolution),
    }
}

/// Reject a nested action map carrying any key outside its known set (spec D8).
fn reject_unknown_action_keys(
    entries: &[(String, CborValue)],
    allowed: &[&str],
) -> Result<(), Reject> {
    for (k, _) in entries {
        if !allowed.contains(&k.as_str()) {
            return Err(Reject::InvalidForkResolution);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::SigningKey;

    #[test]
    fn detect_same_seq_fork() {
        let author = crate::MemberId::from_bytes([0x77; LEN]);
        let a = GovernanceEntryId::from_bytes([1; LEN]);
        let b = GovernanceEntryId::from_bytes([2; LEN]);
        // New entry at seq 3 while a tip at seq 3 exists → fork.
        let evidence = detect(author, b, 3, None, Some((3, a)));
        let evidence = evidence.expect("same-seq fork detected");
        assert_eq!(evidence.conflicting, [a, b]); // ascending order
        assert_eq!(evidence.seq, 3);
        assert!(!evidence.resolved);
    }

    #[test]
    fn no_fork_when_seq_advances() {
        let author = crate::MemberId::from_bytes([0x77; LEN]);
        let a = GovernanceEntryId::from_bytes([1; LEN]);
        let b = GovernanceEntryId::from_bytes([2; LEN]);
        assert!(detect(author, b, 4, Some(a), Some((3, a))).is_none());
    }

    #[test]
    fn fork_resolution_accept_round_trips() {
        let key = SigningKey::from_seed(&[0x79; LEN]);
        let e0 = GovernanceEntryId::from_bytes([10; LEN]);
        let e1 = GovernanceEntryId::from_bytes([20; LEN]);
        let body = ForkResolutionBody {
            schema_version: 2,
            room_id: crate::ids::RoomId::from_bytes([0x70; LEN]),
            signer: key.member_id(),
            evidence: [e0, e1],
            action: ForkResolveAction::Accept { winner: e0 },
            epoch: 5,
        };
        let env = signed::seal(&body, &key);
        let decoded = decode_verified(&env).expect("valid resolution verifies");
        assert_eq!(decoded, body);
    }

    #[test]
    fn fork_resolution_signer_mismatch_rejected() {
        let key = SigningKey::from_seed(&[0x79; LEN]);
        let e0 = GovernanceEntryId::from_bytes([10; LEN]);
        let e1 = GovernanceEntryId::from_bytes([20; LEN]);
        // The body claims a different signer than the key that seals it.
        let body = ForkResolutionBody {
            schema_version: 2,
            room_id: crate::ids::RoomId::from_bytes([0x70; LEN]),
            signer: crate::MemberId::from_bytes([0xbb; LEN]),
            evidence: [e0, e1],
            action: ForkResolveAction::Reject,
            epoch: 5,
        };
        let env = signed::seal(&body, &key);
        assert_eq!(
            decode_verified(&env).err(),
            Some(Reject::InvalidForkResolution)
        );
    }

    #[test]
    fn fork_resolution_accept_with_foreign_winner_rejected() {
        let key = SigningKey::from_seed(&[0x7a; LEN]);
        let e0 = GovernanceEntryId::from_bytes([10; LEN]);
        let e1 = GovernanceEntryId::from_bytes([20; LEN]);
        let foreign = GovernanceEntryId::from_bytes([30; LEN]);
        let body = ForkResolutionBody {
            schema_version: 2,
            room_id: crate::ids::RoomId::from_bytes([0x70; LEN]),
            signer: key.member_id(),
            evidence: [e0, e1],
            action: ForkResolveAction::Accept { winner: foreign },
            epoch: 5,
        };
        let env = signed::seal(&body, &key);
        assert_eq!(
            decode_verified(&env).err(),
            Some(Reject::InvalidForkResolution)
        );
    }
}
