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
}
