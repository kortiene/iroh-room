//! Named 32-byte identifiers and public-key principals (spec §6.1, issue #146).
//!
//! Two families of 32-byte newtypes:
//!
//! - **BLAKE3 hash IDs** ([`RoomId`], [`GovernanceEntryId`], [`ApprovalId`],
//!   [`ContentEventId`], [`SnapshotHash`], [`StateRoot`], [`MerkleRoot`]):
//!   display as the named form `blake3:<64-hex>` (lowercase). Parsing is
//!   case-insensitive for the hex but rejects a wrong prefix/length.
//! - **Public-key principals** ([`MemberId`]/[`PrincipalId`], [`DeviceId`]):
//!   raw Ed25519 public-key bytes; display as lowercase hex (no prefix).
//!
//! All newtypes are `Ord` over raw bytes for deterministic sorting in maps/sets.

use core::fmt;
use core::str::FromStr;

use crate::domain::{
    blake3_domain, COMMUNITY, CONTENT_EVENT, GOVERNANCE_CHECKPOINT, GOVERNANCE_ENTRY,
    REPLICA_RECEIPT, STREAM_CHECKPOINT,
};

/// Byte length of a BLAKE3-256 digest / Ed25519 public key.
pub const LEN: usize = 32;

/// The named-hash prefix for BLAKE3-256 digests (spec §6.1).
pub const BLAKE3_PREFIX: &str = "blake3:";

/// Error parsing a `blake3:<hex>` named-hash identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashParseError {
    /// The string did not start with the `blake3:` prefix.
    MissingPrefix,
    /// The hex body decoded to the wrong number of bytes.
    BadLength {
        /// Actual decoded byte length.
        actual: usize,
    },
    /// The hex body was not valid hex.
    BadHex,
}

impl fmt::Display for HashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => write!(f, "missing `{BLAKE3_PREFIX}` prefix"),
            Self::BadLength { actual } => write!(f, "expected {LEN} digest bytes, got {actual}"),
            Self::BadHex => f.write_str("invalid hex encoding"),
        }
    }
}
impl std::error::Error for HashParseError {}

/// Error parsing a hex-encoded public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParseError {
    /// Actual decoded byte length.
    pub actual: usize,
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected {LEN} key bytes, got {}", self.actual)
    }
}
impl std::error::Error for KeyParseError {}

macro_rules! named_hash_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// Wraps a raw 32-byte BLAKE3-256 digest. `Display`/`FromStr` use the named
        /// form `blake3:<64-hex>`.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; LEN]);

        impl $name {
            /// Wrap a raw 32-byte digest.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; LEN]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw 32 digest bytes (the on-wire form).
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; LEN] {
                &self.0
            }

            /// Render the named string form `blake3:<64-hex>` (lowercase).
            #[must_use]
            pub fn to_named_string(&self) -> String {
                format!("{BLAKE3_PREFIX}{}", hex::encode(self.0))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{BLAKE3_PREFIX}{}", hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({BLAKE3_PREFIX}{})", stringify!($name), hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = HashParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let body = s
                    .strip_prefix(BLAKE3_PREFIX)
                    .ok_or(HashParseError::MissingPrefix)?;
                let bytes = hex::decode(body).map_err(|_| HashParseError::BadHex)?;
                let actual = bytes.len();
                let arr =<[u8; LEN]>::try_from(bytes.as_slice())
                    .map_err(|_| HashParseError::BadLength { actual })?;
                Ok(Self(arr))
            }
        }
    };
}

named_hash_newtype! {
    /// A room/space cryptographic identity (spec §6.1).
    RoomId
}
named_hash_newtype! {
    /// A governance log entry's id: `BLAKE3(GOVERNANCE_ENTRY_ID || CSB)` (#147).
    GovernanceEntryId
}
named_hash_newtype! {
    /// A governance approval's id: `BLAKE3(GOVERNANCE_APPROVAL_ID || CSB)` (#147).
    ApprovalId
}
named_hash_newtype! {
    /// A content event's id: `BLAKE3(CONTENT_EVENT_ID || CSB)` (#152).
    ContentEventId
}
named_hash_newtype! {
    /// A snapshot hash: `BLAKE3(SNAPSHOT_HASH || canonical_snapshot)` (#150).
    SnapshotHash
}
named_hash_newtype! {
    /// The governance state root: `BLAKE3(GOVERNANCE_STATE_ROOT || state_cbor)` (#147).
    StateRoot
}
named_hash_newtype! {
    /// A sparse Merkle-map root (#151).
    MerkleRoot
}

// ----------------------------------------------------------------------------
// #134 §6.3 v2 identifier newtypes (issue #146). NORMATIVE.
//
// The exact §6.3 preimage byte layouts were unavailable in this checkout (spec
// §5.3 / §14 OQ-1). Per spec D2 the default derivation model is used and pinned
// by golden vectors, with each assumption documented at the derivation helper:
//
//   id_digest = BLAKE3(DOMAIN || declared_id_preimage)
//
// where `declared_id_preimage` is the canonical-CBOR bytes of the record or
// descriptor the identifier names. Presentation is the strict `blake3:<64
// lowercase hex>` form (spec D4): parsing rejects a missing prefix, wrong width,
// odd-length / invalid / uppercase hex, and surrounding whitespace.
// ----------------------------------------------------------------------------

/// Strict-parse error for the #134 §6.3 v2 identifier newtypes.
///
/// Mirrors [`HashParseError`] but adds [`Self::UppercaseHex`] and
/// [`Self::SurroundingWhitespace`] so canonical (lowercase, trim) presentation
/// is enforced for the frozen v2 types (spec D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictHashParseError {
    /// The string did not start with the `blake3:` prefix.
    MissingPrefix,
    /// The hex body decoded to the wrong number of bytes.
    BadLength {
        /// Actual decoded byte length.
        actual: usize,
    },
    /// The hex body was not valid (even-length) hex.
    BadHex,
    /// The hex body contained uppercase characters; canonical presentation is
    /// lowercase only (spec D4).
    UppercaseHex,
    /// The string had leading or trailing whitespace; canonical presentation
    /// carries none (spec D4).
    SurroundingWhitespace,
}

impl fmt::Display for StrictHashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => write!(f, "missing `{BLAKE3_PREFIX}` prefix"),
            Self::BadLength { actual } => {
                write!(f, "expected {LEN} digest bytes, got {actual}")
            }
            Self::BadHex => f.write_str("invalid hex encoding"),
            Self::UppercaseHex => f.write_str("uppercase hex; canonical form is lowercase"),
            Self::SurroundingWhitespace => f.write_str("surrounding whitespace"),
        }
    }
}
impl std::error::Error for StrictHashParseError {}

/// Decode a `blake3:`-prefixed hex body with the strict #146 presentation rules
/// (spec D4). Shared by every frozen v2 identifier's `FromStr`.
fn parse_strict_hex(prefixed: &str) -> Result<[u8; LEN], StrictHashParseError> {
    if prefixed != prefixed.trim() {
        return Err(StrictHashParseError::SurroundingWhitespace);
    }
    let body = prefixed
        .strip_prefix(BLAKE3_PREFIX)
        .ok_or(StrictHashParseError::MissingPrefix)?;
    // Reject uppercase before delegating to the (case-insensitive) decoder so
    // the canonical lowercase form is the only accepted presentation.
    if body.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(StrictHashParseError::UppercaseHex);
    }
    let bytes = hex::decode(body).map_err(|_| StrictHashParseError::BadHex)?;
    let actual = bytes.len();
    <[u8; LEN]>::try_from(bytes.as_slice()).map_err(|_| StrictHashParseError::BadLength { actual })
}

macro_rules! named_hash_newtype_strict {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// Wraps a raw 32-byte BLAKE3-256 digest. `Display` emits the canonical
        /// `blake3:<64 lowercase hex>` form; `FromStr` enforces it strictly
        /// (spec D4): lowercase, trimmed, exact width.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; LEN]);

        impl $name {
            /// Wrap a raw 32-byte digest. The digest is assumed to already be a
            /// BLAKE3 output under this identifier's domain.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; LEN]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw 32 digest bytes (the on-wire form).
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; LEN] {
                &self.0
            }

            /// Render the canonical string form `blake3:<64 lowercase hex>`.
            #[must_use]
            pub fn to_named_string(&self) -> String {
                format!("{BLAKE3_PREFIX}{}", hex::encode(self.0))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{BLAKE3_PREFIX}{}", hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({BLAKE3_PREFIX}{})", stringify!($name), hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = StrictHashParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                parse_strict_hex(s).map(Self)
            }
        }
    };
}

named_hash_newtype_strict! {
    /// A v2 community identity: `BLAKE3(COMMUNITY || community_descriptor_csb)`
    /// (spec §6.2 / §6.3; `#134` frozen domain `iroh-room-v2/community`).
    ///
    /// This is the canonical v2 name for what the legacy candidate layer called
    /// [`RoomId`]. The two are deliberately distinct types: `RoomId` is tied to
    /// the legacy `ROOM_ID` domain and remains only for the already-frozen
    /// signed-record golden vectors; new code uses `CommunityId`.
    CommunityId
}
named_hash_newtype_strict! {
    /// A v2 governance identity: `BLAKE3(GOVERNANCE_ENTRY || governance_entry_csb)`
    /// (spec §6.2 / §6.3; `#134` frozen domain `iroh-room-v2/governance-entry`).
    ///
    /// Assumption (OQ-3): `GovernanceId` identifies a single governance log
    /// entry. The governance *state* root uses the separate [`GOVERNANCE_STATE`]
    /// domain; this type does not represent the state root.
    GovernanceId
}
named_hash_newtype_strict! {
    /// A v2 content-stream identity (spec §6.2 / §6.3; `#134` frozen domain
    /// `iroh-room-v2/content-event`).
    ///
    /// Assumption (OQ-1): `#134 §6.3` did not list a dedicated stream domain, so
    /// streams derive under the `content-event` boundary from a canonical stream
    /// descriptor. If #134 later reserves a `stream` domain, add a dedicated
    /// derivation and migrate vectors.
    StreamId
}
named_hash_newtype_strict! {
    /// A v2 content-event identity: `BLAKE3(CONTENT_EVENT || content_event_csb)`
    /// (spec §6.2 / §6.3; `#134` frozen domain `iroh-room-v2/content-event`).
    ///
    /// Assumption (OQ-4): `EventId` is specifically a content-event id. The
    /// legacy [`ContentEventId`] remains for the frozen #153 vectors.
    EventId
}
named_hash_newtype_strict! {
    /// A v2 checkpoint identity (spec §6.2 / §6.3). One newtype covers both the
    /// governance-checkpoint and stream-checkpoint kinds via typed constructors
    /// that pin the domain at the call site.
    ///
    /// - governance kind: `BLAKE3(GOVERNANCE_CHECKPOINT || checkpoint_csb)`;
    /// - stream kind:     `BLAKE3(STREAM_CHECKPOINT || checkpoint_csb)`.
    ///
    /// Assumption (OQ-5): #134 does not split checkpoint ids into two types;
    /// the kind is a construction-time choice, not a type-level distinction.
    CheckpointId
}
named_hash_newtype_strict! {
    /// A v2 replica identity (spec §6.2 / §6.3; `#134` frozen domain
    /// `iroh-room-v2/replica-receipt`).
    ///
    /// Assumption (OQ-6): `ReplicaId` derives from a canonical replica
    /// descriptor under the `replica-receipt` domain. This issue exposes only
    /// the derivation surface; the receipt protocol is out of scope (#151).
    ReplicaId
}

// --- Typed derivation helpers (spec D3: preimage is explicit at the boundary).
//
// Each helper takes the *declared preimage* — the canonical-CBOR bytes of the
// record/descriptor the identifier names — so a caller cannot accidentally hash
// under the wrong domain or the wrong preimage shape. The low-level
// `domain::blake3_domain` performs `BLAKE3(domain || preimage)` (spec D2).

impl CommunityId {
    /// Derive a `CommunityId` from its declared preimage — the canonical-CBOR
    /// bytes of the community descriptor.
    #[must_use]
    pub fn derive(preimage: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(COMMUNITY, preimage))
    }
}

impl GovernanceId {
    /// Derive a `GovernanceId` from the canonical signed bytes (CSB) of a
    /// governance entry record.
    #[must_use]
    pub fn from_governance_entry_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(GOVERNANCE_ENTRY, csb))
    }
}

impl StreamId {
    /// Derive a `StreamId` from the canonical-CBOR bytes of a stream descriptor.
    #[must_use]
    pub fn from_stream_descriptor_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(CONTENT_EVENT, csb))
    }
}

impl EventId {
    /// Derive an `EventId` from the canonical signed bytes (CSB) of a content
    /// event record.
    #[must_use]
    pub fn from_content_event_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(CONTENT_EVENT, csb))
    }
}

impl CheckpointId {
    /// Derive a governance-kind `CheckpointId` from the canonical signed bytes
    /// of a governance checkpoint.
    #[must_use]
    pub fn from_governance_checkpoint_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(GOVERNANCE_CHECKPOINT, csb))
    }

    /// Derive a stream-kind `CheckpointId` from the canonical signed bytes of a
    /// stream checkpoint.
    #[must_use]
    pub fn from_stream_checkpoint_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(STREAM_CHECKPOINT, csb))
    }
}

impl ReplicaId {
    /// Derive a `ReplicaId` from the canonical-CBOR bytes of a replica
    /// descriptor (e.g. the material a replica receipt commits to).
    #[must_use]
    pub fn from_replica_descriptor_csb(csb: &[u8]) -> Self {
        Self::from_bytes(blake3_domain(REPLICA_RECEIPT, csb))
    }
}

macro_rules! public_key_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// `Ord` is the bytewise order of the raw public-key bytes (no protocol
        /// meaning; exists so deterministic `BTreeMap`s can key on principals).
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; LEN]);

        impl $name {
            /// Wrap raw public-key bytes. The bytes are not validated as a curve
            /// point here; an invalid point fails closed at verification time.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; LEN]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw 32 public-key bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; LEN] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = KeyParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let bytes = hex::decode(s).map_err(|_| KeyParseError { actual: 0 })?;
                let actual = bytes.len();
                <[u8; LEN]>::try_from(bytes.as_slice())
                    .map_err(|_| KeyParseError { actual })
                    .map(Self)
            }
        }
    };
}

public_key_newtype! {
    /// A principal's identity public key (`MemberId`, spec §6.1 / OQ-2).
    ///
    /// In the single-key v2 model (OQ-2 assumption), this is the Ed25519 public
    /// key that both identifies the principal and verifies their record
    /// signatures.
    MemberId
}
public_key_newtype! {
    /// A device signing public key (spec §6.1 / OQ-2).
    ///
    /// Distinct from [`MemberId`] so a future two-key model (OQ-2) can split
    /// identity from device without a breaking change; today the two are the
    /// same bytes.
    DeviceId
}

/// Alias: a `PrincipalId` is the principal identity (spec §6.5 / D5 uses
/// `PrincipalId`; spec §6.1 names `MemberId`). They are the same concept.
pub type PrincipalId = MemberId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_id_round_trip() {
        let raw = [7u8; LEN];
        let id = GovernanceEntryId::from_bytes(raw);
        let s = id.to_named_string();
        assert_eq!(
            s,
            "blake3:0707070707070707070707070707070707070707070707070707070707070707"
        );
        let parsed: GovernanceEntryId = s.parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn hash_id_parse_errors() {
        assert_eq!(
            "deadbeef".parse::<RoomId>().unwrap_err(),
            HashParseError::MissingPrefix
        );
        // Even-length hex that decodes to the wrong byte count.
        assert_eq!(
            "blake3:abcd".parse::<RoomId>().unwrap_err(),
            HashParseError::BadLength { actual: 2 }
        );
        assert_eq!(
            "blake3:zz".parse::<RoomId>().unwrap_err(),
            HashParseError::BadHex
        );
    }

    #[test]
    fn member_id_hex_round_trip() {
        let raw = [0xab; LEN];
        let id = MemberId::from_bytes(raw);
        let s = id.to_string();
        assert_eq!(s.len(), 64);
        let parsed: MemberId = s.parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn ord_is_bytewise() {
        let a = MerkleRoot::from_bytes([0; LEN]);
        let b = MerkleRoot::from_bytes([1; LEN]);
        assert!(a < b);
    }

    // --- #146 frozen v2 identifier strict presentation + derivation ----------

    #[test]
    fn v2_identifier_display_is_lowercase_prefixed_hex() {
        let id = CommunityId::from_bytes([0xab; LEN]);
        assert_eq!(
            id.to_string(),
            "blake3:abababababababababababababababababababababababababababababababab"
        );
        // Debug carries the type name so the identifier family is unambiguous.
        assert!(format!("{id:?}").starts_with("CommunityId("));
    }

    #[test]
    fn v2_identifier_round_trip_display_parse() {
        let id = EventId::from_bytes([0x01; LEN]);
        let s = id.to_named_string();
        let parsed: EventId = s.parse().unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.as_bytes(), id.as_bytes());
    }

    #[test]
    fn v2_identifier_parse_rejects_malformed_presentation() {
        // Missing prefix.
        assert_eq!(
            "abab".parse::<CommunityId>().unwrap_err(),
            StrictHashParseError::MissingPrefix
        );
        // Wrong width (even-length hex, too short).
        assert_eq!(
            "blake3:abcd".parse::<CommunityId>().unwrap_err(),
            StrictHashParseError::BadLength { actual: 2 }
        );
        // Odd-length / invalid hex.
        assert_eq!(
            "blake3:zz".parse::<CommunityId>().unwrap_err(),
            StrictHashParseError::BadHex
        );
        // Uppercase hex is not canonical (spec D4).
        assert_eq!(
            "blake3:ABAB".parse::<CommunityId>().unwrap_err(),
            StrictHashParseError::UppercaseHex
        );
        // Surrounding whitespace is not canonical (spec D4).
        assert_eq!(
            " blake3:abababababababababababababababababababababababababababababababab "
                .parse::<CommunityId>()
                .unwrap_err(),
            StrictHashParseError::SurroundingWhitespace
        );
    }

    #[test]
    fn v2_identifier_derivation_picks_the_declared_domain() {
        // Same preimage under two different domains must yield two different
        // identifiers — the wrong-domain guard for the typed helpers.
        let preimage = b"frozen-v2-preimage";
        let community = CommunityId::derive(preimage);
        let governance = GovernanceId::from_governance_entry_csb(preimage);
        assert_ne!(community.as_bytes(), governance.as_bytes());
        // A typed helper must match a manual `BLAKE3(domain || preimage)`.
        assert_eq!(
            community.as_bytes(),
            &blake3_domain(crate::domain::COMMUNITY, preimage)
        );
    }

    #[test]
    fn checkpoint_id_has_two_pinned_domains() {
        let csb = b"checkpoint-csb";
        let gov = CheckpointId::from_governance_checkpoint_csb(csb);
        let stream = CheckpointId::from_stream_checkpoint_csb(csb);
        assert_ne!(gov.as_bytes(), stream.as_bytes());
        assert_eq!(
            gov.as_bytes(),
            &blake3_domain(crate::domain::GOVERNANCE_CHECKPOINT, csb)
        );
        assert_eq!(
            stream.as_bytes(),
            &blake3_domain(crate::domain::STREAM_CHECKPOINT, csb)
        );
    }
}
