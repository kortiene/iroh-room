//! Named content-hash identifiers: [`EventId`], [`RoomId`], [`HashRef`].
//!
//! Each is a raw 32-byte BLAKE3-256 digest (Event Protocol §4/§5). On the wire
//! and in `prev_events` the value is the raw 32 bytes; its human/CLI/JSON
//! presentation is the **named** form `blake3:<64-hex>` (lowercase). Parsing is
//! case-insensitive but rejects a wrong prefix or wrong length.

use core::fmt;
use core::str::FromStr;

use super::constants::DIGEST_LEN;

/// The named-hash prefix for BLAKE3-256 digests (Event Protocol §4).
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
            Self::BadLength { actual } => {
                write!(f, "expected {DIGEST_LEN} digest bytes, got {actual}")
            }
            Self::BadHex => f.write_str("invalid hex encoding"),
        }
    }
}

impl std::error::Error for HashParseError {}

macro_rules! named_hash_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// Wraps a raw 32-byte BLAKE3-256 digest. `Display`/`FromStr` use the
        /// named form `blake3:<64-hex>`.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; DIGEST_LEN]);

        impl $name {
            /// Wrap a raw 32-byte digest.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw 32 digest bytes (the on-wire form).
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
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
                let arr = <[u8; DIGEST_LEN]>::try_from(bytes.as_slice())
                    .map_err(|_| HashParseError::BadLength { actual })?;
                Ok(Self(arr))
            }
        }
    };
}

named_hash_newtype! {
    /// A signed event's identity: `BLAKE3-256(CSB)` (Event Protocol §4).
    EventId
}

named_hash_newtype! {
    /// A room's cryptographic identity (Event Protocol §5).
    RoomId
}

named_hash_newtype! {
    /// A generic named BLAKE3 reference (e.g. a blob hash in `file.shared`).
    HashRef
}
