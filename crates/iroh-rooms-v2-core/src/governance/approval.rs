//! Governance approval record body (spec §6.3 / #147).
//!
//! An approval references a governance entry id, the approving principal, and
//! (optionally) the proposed state root the approver is attesting to. Approvals
//! are collected by the fold to satisfy the entry's approval policy (spec §6.4 /
//! [`super::authz`]).

use crate::cbor::CborValue;
use crate::domain;
use crate::error::Reject;
use crate::governance::model::{read_member_field, uint_field};
use crate::ids::{ApprovalId, GovernanceEntryId, StateRoot, LEN};
use crate::signed::{self, Envelope, SignedBody};

/// A governance approval body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalBody {
    /// The schema version (MUST be `2`).
    pub schema_version: u64,
    /// The room this approval belongs to.
    pub room_id: crate::ids::RoomId,
    /// The entry being approved.
    pub entry_id: GovernanceEntryId,
    /// The approving principal.
    pub approver: crate::MemberId,
    /// The proposed state root the approver attests to (optional; spec #147).
    pub proposed_state_root: Option<StateRoot>,
    /// Signed epoch (advisory).
    pub epoch: u64,
}

impl SignedBody for ApprovalBody {
    type Id = ApprovalId;
    const SIGN_CONTEXT: &'static [u8] = domain::GOVERNANCE_APPROVAL_SIGN;
    const ID_CONTEXT: &'static [u8] = domain::GOVERNANCE_APPROVAL_ID;

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
                "entry_id".to_owned(),
                CborValue::Bytes(self.entry_id.as_bytes().to_vec()),
            ),
            (
                "approver".to_owned(),
                CborValue::Bytes(self.approver.as_bytes().to_vec()),
            ),
            ("epoch".to_owned(), CborValue::Uint(self.epoch)),
        ];
        if let Some(root) = self.proposed_state_root {
            entries.push((
                "proposed_state_root".to_owned(),
                CborValue::Bytes(root.as_bytes().to_vec()),
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
                "entry_id",
                "approver",
                "epoch",
                "proposed_state_root",
            ],
        )?;
        let schema_version = uint_field(entries, "schema_version")?;
        if schema_version != crate::governance::model::SCHEMA_VERSION {
            return Err(Reject::UnknownVersion);
        }
        let room_id = read_id_field_local(entries, "room_id")?;
        let entry_id = read_id_field_local_into(entries, "entry_id")?;
        let approver = read_member_field(entries, "approver")?;
        let epoch = uint_field(entries, "epoch")?;
        let proposed_state_root = match signed::opt(entries, "proposed_state_root") {
            Some(v) => {
                let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
                let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
                Some(StateRoot::from_bytes(arr))
            }
            None => None,
        };
        Ok(Self {
            schema_version,
            room_id,
            entry_id,
            approver,
            proposed_state_root,
            epoch,
        })
    }

    fn id_from_csb(csb: &[u8]) -> Self::Id {
        ApprovalId::from_bytes(domain::blake3_domain(Self::ID_CONTEXT, csb))
    }
}

/// A signed approval envelope.
pub type SignedApproval = Envelope<ApprovalId>;

/// Decode + verify a signed approval end-to-end (spec D2 / §6.3).
///
/// # Errors
/// See [`signed::verify_envelope`]; additionally enforces `schema_version == 2`.
pub fn decode_verified(env: &SignedApproval) -> Result<ApprovalBody, Reject> {
    signed::verify_envelope::<ApprovalBody>(env)
}

fn read_id_field_local(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<crate::ids::RoomId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(crate::ids::RoomId::from_bytes(arr))
}

fn read_id_field_local_into(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<GovernanceEntryId, Reject> {
    let v = signed::field(entries, key).ok_or(Reject::NonCanonicalEncoding)?;
    let bytes = v.as_bytes().ok_or(Reject::NonCanonicalEncoding)?;
    let arr = <[u8; LEN]>::try_from(bytes).map_err(|_| Reject::NonCanonicalEncoding)?;
    Ok(GovernanceEntryId::from_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::model::SCHEMA_VERSION;
    use crate::keys::SigningKey;

    #[test]
    fn approval_seal_and_verify_round_trip() {
        let key = SigningKey::from_seed(&[0x70; LEN]);
        let body = ApprovalBody {
            schema_version: SCHEMA_VERSION,
            room_id: crate::ids::RoomId::from_bytes([0x31; LEN]),
            entry_id: GovernanceEntryId::from_bytes([0x32; LEN]),
            approver: key.member_id(),
            proposed_state_root: Some(StateRoot::from_bytes([0x33; LEN])),
            epoch: 9,
        };
        let env = signed::seal(&body, &key);
        let decoded = decode_verified(&env).expect("valid approval verifies");
        assert_eq!(decoded, body);
        assert_eq!(env.signer, key.member_id());
    }

    #[test]
    fn approval_wrong_signer_rejected() {
        let signer = SigningKey::from_seed(&[0x71; LEN]);
        let other = SigningKey::from_seed(&[0x72; LEN]);
        let body = ApprovalBody {
            schema_version: SCHEMA_VERSION,
            room_id: crate::ids::RoomId::from_bytes([0x31; LEN]),
            entry_id: GovernanceEntryId::from_bytes([0x32; LEN]),
            approver: signer.member_id(),
            proposed_state_root: None,
            epoch: 1,
        };
        let mut env = signed::seal(&body, &signer);
        env.signer = other.member_id();
        assert_eq!(decode_verified(&env).err(), Some(Reject::BadSignature));
    }

    #[test]
    fn approval_unknown_version_rejected() {
        let key = SigningKey::from_seed(&[0x73; LEN]);
        // Seal an approval body carrying the wrong schema version. The signature
        // verifies (it signs the actual CSB), but the version check in
        // `from_canonical` rejects with UnknownVersion.
        let body = ApprovalBody {
            schema_version: 99,
            room_id: crate::ids::RoomId::from_bytes([0x31; LEN]),
            entry_id: GovernanceEntryId::from_bytes([0x32; LEN]),
            approver: key.member_id(),
            proposed_state_root: None,
            epoch: 1,
        };
        let env = signed::seal(&body, &key);
        assert_eq!(decode_verified(&env).err(), Some(Reject::UnknownVersion));
    }
}
