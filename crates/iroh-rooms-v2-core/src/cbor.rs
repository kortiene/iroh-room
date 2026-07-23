//! Purpose-built deterministic-CBOR codec for the v2 signed-record trust
//! boundary (spec §6.2 / D2).
//!
//! A small self-contained canonical-CBOR encoder plus a *strict* reader that
//! validates the RFC 8949 §4.2.1 core-deterministic profile **while parsing**.
//!
//! The supported value space is a **closed profile**: unsigned integers, byte
//! strings, text strings, arrays, and text-keyed maps. Everything else —
//! negative integers, tags, floats/simple values, indefinite-length items,
//! non-text map keys — is rejected as [`CborError`]. The decoder additionally
//! enforces shortest-form integers, definite lengths, strictly-ascending unique
//! map keys, valid UTF-8, bounded nesting depth, and no trailing data.
//!
//! A successful [`decode_canonical`] guarantees `encode(decode_canonical(b)) == b`,
//! which is the canonical-bytes trust boundary every v2 signature/id/root hashes
//! over.

use core::fmt;

/// Maximum CBOR nesting depth accepted by [`decode_canonical`]. A tight cap that
/// fails closed on adversarially-nested input without recursion blowup.
pub const MAX_DEPTH: usize = 16;

/// A decoded CBOR value, restricted to the closed deterministic profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborValue {
    /// Unsigned integer (major type 0).
    Uint(u64),
    /// Byte string (major type 2).
    Bytes(Vec<u8>),
    /// UTF-8 text string (major type 3).
    Text(String),
    /// Array (major type 4).
    Array(Vec<CborValue>),
    /// Map with text keys (major type 5), held in canonical key order.
    Map(Vec<(String, CborValue)>),
}

/// A non-canonical, malformed, or out-of-profile CBOR encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborError {
    /// Ran out of input bytes mid-item.
    UnexpectedEof,
    /// Bytes remained after a complete top-level item.
    TrailingData,
    /// An indefinite-length string/array/map was encountered.
    IndefiniteLength,
    /// An integer used a longer-than-necessary encoding.
    NonShortestInt,
    /// A negative integer (major type 1) — outside the profile.
    NegativeInteger,
    /// Additional-info values 28–30 are reserved.
    ReservedAdditionalInfo,
    /// A CBOR tag (major type 6) — disallowed.
    Tag,
    /// A float or simple value (major type 7) — disallowed.
    FloatOrSimple,
    /// A map key that was not a text string.
    NonTextMapKey,
    /// Map keys not in strictly-ascending canonical order.
    UnsortedMapKey,
    /// A duplicate map key.
    DuplicateMapKey,
    /// A text string that was not valid UTF-8.
    InvalidUtf8,
    /// Nesting deeper than the decoder's fixed depth cap.
    DepthExceeded,
    /// A declared length/count that cannot fit the platform or input.
    LengthOverflow,
}

impl fmt::Display for CborError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnexpectedEof => "unexpected end of input",
            Self::TrailingData => "trailing data after top-level item",
            Self::IndefiniteLength => "indefinite-length item",
            Self::NonShortestInt => "non-shortest integer encoding",
            Self::NegativeInteger => "negative integer outside profile",
            Self::ReservedAdditionalInfo => "reserved additional-info value",
            Self::Tag => "CBOR tag is disallowed",
            Self::FloatOrSimple => "float/simple value is disallowed",
            Self::NonTextMapKey => "map key is not a text string",
            Self::UnsortedMapKey => "map keys are not in canonical order",
            Self::DuplicateMapKey => "duplicate map key",
            Self::InvalidUtf8 => "text string is not valid UTF-8",
            Self::DepthExceeded => "nesting too deep",
            Self::LengthOverflow => "length/count overflow",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for CborError {}

// ----------------------------------------------------------------------------
// Encoder
// ----------------------------------------------------------------------------

/// Encode a [`CborValue`] as canonical (deterministic) CBOR.
///
/// Map entries are emitted in canonical key order (length-first then bytewise
/// over the encoded key form) regardless of insertion order, so the output is
/// independent of how the value was built.
#[must_use]
pub fn encode(value: &CborValue) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &CborValue, out: &mut Vec<u8>) {
    match value {
        CborValue::Uint(n) => write_head(0, *n, out),
        CborValue::Bytes(b) => {
            write_head(2, len_arg(b.len()), out);
            out.extend_from_slice(b);
        }
        CborValue::Text(s) => {
            write_head(3, len_arg(s.len()), out);
            out.extend_from_slice(s.as_bytes());
        }
        CborValue::Array(items) => {
            write_head(4, len_arg(items.len()), out);
            for item in items {
                encode_into(item, out);
            }
        }
        CborValue::Map(entries) => {
            write_head(5, len_arg(entries.len()), out);
            let mut sorted: Vec<&(String, CborValue)> = entries.iter().collect();
            sorted.sort_by_key(|entry| encoded_key(&entry.0));
            for (k, v) in sorted {
                write_head(3, len_arg(k.len()), out);
                out.extend_from_slice(k.as_bytes());
                encode_into(v, out);
            }
        }
    }
}

/// Convert an in-memory length to the `u64` argument used in a CBOR head.
fn len_arg(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// The canonical encoded form of a text map key, used as the sort/compare key.
fn encoded_key(key: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(key.len() + 1);
    write_head(3, len_arg(key.len()), &mut out);
    out.extend_from_slice(key.as_bytes());
    out
}

/// Write a CBOR item head (major type + shortest-form argument).
fn write_head(major: u8, arg: u64, out: &mut Vec<u8>) {
    let high = major << 5;
    let be = arg.to_be_bytes();
    match arg {
        0..=0x17 => out.push(high | be[7]),
        0x18..=0xFF => {
            out.push(high | 0x18);
            out.push(be[7]);
        }
        0x100..=0xFFFF => {
            out.push(high | 0x19);
            out.extend_from_slice(&be[6..8]);
        }
        0x1_0000..=0xFFFF_FFFF => {
            out.push(high | 0x1A);
            out.extend_from_slice(&be[4..8]);
        }
        _ => {
            out.push(high | 0x1B);
            out.extend_from_slice(&be);
        }
    }
}

// ----------------------------------------------------------------------------
// Strict decoder
// ----------------------------------------------------------------------------

/// Decode exactly one canonical CBOR item from `input`, enforcing the full
/// deterministic profile and requiring the entire input is consumed.
///
/// # Errors
/// Returns a [`CborError`] for any malformed or non-canonical encoding. Never
/// panics, never allocates beyond input bounds, and is bounded in depth.
pub fn decode_canonical(input: &[u8]) -> Result<CborValue, CborError> {
    let mut reader = Reader { buf: input, pos: 0 };
    let value = reader.read_value(0)?;
    if reader.pos == input.len() {
        Ok(value)
    } else {
        Err(CborError::TrailingData)
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], CborError> {
        let end = self.pos.checked_add(n).ok_or(CborError::LengthOverflow)?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(CborError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, CborError> {
        Ok(self.take(1)?[0])
    }

    /// Read an item head and return `(major_type, argument)`, enforcing
    /// shortest-form integers and rejecting indefinite/reserved encodings.
    fn read_head(&mut self) -> Result<(u8, u64), CborError> {
        let initial = self.read_u8()?;
        let major = initial >> 5;
        let info = initial & 0x1f;
        let arg = match info {
            0..=23 => u64::from(info),
            24 => {
                let v = u64::from(self.read_u8()?);
                if v <= 23 {
                    return Err(CborError::NonShortestInt);
                }
                v
            }
            25 => {
                let bytes = self.take(2)?;
                let v = u64::from(u16::from_be_bytes([bytes[0], bytes[1]]));
                if u8::try_from(v).is_ok() {
                    return Err(CborError::NonShortestInt);
                }
                v
            }
            26 => {
                let bytes = self.take(4)?;
                let v = u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                if u16::try_from(v).is_ok() {
                    return Err(CborError::NonShortestInt);
                }
                v
            }
            27 => {
                let bytes = self.take(8)?;
                let v = u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                if u32::try_from(v).is_ok() {
                    return Err(CborError::NonShortestInt);
                }
                v
            }
            28..=30 => return Err(CborError::ReservedAdditionalInfo),
            _ => return Err(CborError::IndefiniteLength),
        };
        Ok((major, arg))
    }

    /// Convert a declared length/count to `usize`, rejecting values that cannot
    /// be backed by the remaining input (anti-DoS preallocation guard).
    fn checked_len(&self, arg: u64, min_bytes_each: usize) -> Result<usize, CborError> {
        let n = usize::try_from(arg).map_err(|_| CborError::LengthOverflow)?;
        let remaining = self.buf.len() - self.pos;
        let needed = n
            .checked_mul(min_bytes_each)
            .ok_or(CborError::LengthOverflow)?;
        if needed > remaining {
            return Err(CborError::UnexpectedEof);
        }
        Ok(n)
    }

    fn read_value(&mut self, depth: usize) -> Result<CborValue, CborError> {
        if depth > MAX_DEPTH {
            return Err(CborError::DepthExceeded);
        }
        let (major, arg) = self.read_head()?;
        match major {
            0 => Ok(CborValue::Uint(arg)),
            1 => Err(CborError::NegativeInteger),
            2 => {
                let n = self.checked_len(arg, 1)?;
                Ok(CborValue::Bytes(self.take(n)?.to_vec()))
            }
            3 => {
                let n = self.checked_len(arg, 1)?;
                let bytes = self.take(n)?;
                let text = core::str::from_utf8(bytes).map_err(|_| CborError::InvalidUtf8)?;
                Ok(CborValue::Text(text.to_owned()))
            }
            4 => {
                let n = self.checked_len(arg, 1)?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.read_value(depth + 1)?);
                }
                Ok(CborValue::Array(items))
            }
            5 => {
                let n = self.checked_len(arg, 2)?;
                let mut entries: Vec<(String, CborValue)> = Vec::with_capacity(n);
                let mut prev_key: Option<Vec<u8>> = None;
                for _ in 0..n {
                    let (k_major, k_arg) = self.read_head()?;
                    if k_major != 3 {
                        return Err(CborError::NonTextMapKey);
                    }
                    let klen = self.checked_len(k_arg, 1)?;
                    let kbytes = self.take(klen)?;
                    let key = core::str::from_utf8(kbytes)
                        .map_err(|_| CborError::InvalidUtf8)?
                        .to_owned();
                    let key_enc = encoded_key(&key);
                    if let Some(prev) = &prev_key {
                        match key_enc.cmp(prev) {
                            core::cmp::Ordering::Less => return Err(CborError::UnsortedMapKey),
                            core::cmp::Ordering::Equal => return Err(CborError::DuplicateMapKey),
                            core::cmp::Ordering::Greater => {}
                        }
                    }
                    let value = self.read_value(depth + 1)?;
                    entries.push((key, value));
                    prev_key = Some(key_enc);
                }
                Ok(CborValue::Map(entries))
            }
            6 => Err(CborError::Tag),
            _ => Err(CborError::FloatOrSimple),
        }
    }
}

// ----------------------------------------------------------------------------
// Convenience accessors used by the typed mapping layers.
// ----------------------------------------------------------------------------

impl CborValue {
    /// Borrow as a `u64`, if this is an unsigned integer.
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(n) => Some(*n),
            _ => None,
        }
    }

    /// Borrow as a byte slice, if this is a byte string.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// Borrow as a string slice, if this is a text string.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow as an array slice, if this is an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[CborValue]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Borrow as map entries, if this is a map.
    #[must_use]
    pub fn as_map(&self) -> Option<&[(String, CborValue)]> {
        match self {
            Self::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// Look up a text key in a map value, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&CborValue> {
        self.as_map()
            .and_then(|entries| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hx(s: &str) -> Vec<u8> {
        hex::decode(s.replace(' ', "")).expect("valid hex fixture")
    }

    #[test]
    fn unexpected_eof_and_trailing_data() {
        assert_eq!(
            decode_canonical(&hx("43 01 02")),
            Err(CborError::UnexpectedEof)
        );
        assert_eq!(decode_canonical(&hx("00 00")), Err(CborError::TrailingData));
    }

    #[test]
    fn rejects_non_profile_items() {
        for input in [
            "1f",                   // indefinite
            "20",                   // negative
            "1c",                   // reserved
            "c0",                   // tag
            "f4",                   // simple/float
            "a1 00 00",             // non-text map key
            "a2 61 62 00 61 61 00", // unsorted keys
            "a2 61 61 00 61 61 00", // duplicate key
            "62 ff ff",             // invalid utf-8
        ] {
            assert!(decode_canonical(&hx(input)).is_err(), "input {input}");
        }
    }

    #[test]
    fn rejects_non_shortest_int() {
        assert_eq!(
            decode_canonical(&hx("18 17")),
            Err(CborError::NonShortestInt)
        );
        assert_eq!(
            decode_canonical(&hx("58 17")),
            Err(CborError::NonShortestInt)
        );
    }

    #[test]
    fn round_trips_all_kinds() {
        // Keys in canonical (length-first, then bytewise) order: a, b, n, t.
        let value = CborValue::Map(vec![
            (
                "a".to_owned(),
                CborValue::Array(vec![CborValue::Uint(1), CborValue::Uint(2)]),
            ),
            ("b".to_owned(), CborValue::Bytes(vec![1, 2, 3])),
            ("n".to_owned(), CborValue::Uint(300)),
            ("t".to_owned(), CborValue::Text("hi".to_owned())),
        ]);
        let bytes = encode(&value);
        let back = decode_canonical(&bytes).expect("round-trip");
        assert_eq!(back, value);
        assert_eq!(encode(&back), bytes);
    }

    #[test]
    fn encoder_sorts_scrambled_keys() {
        let scrambled = CborValue::Map(vec![
            ("b".to_owned(), CborValue::Uint(2)),
            ("a".to_owned(), CborValue::Uint(1)),
        ]);
        assert_eq!(encode(&scrambled), hx("a2 61 61 01 61 62 02"));
    }

    #[test]
    fn depth_cap_enforced() {
        let mut bytes = vec![0x81u8; MAX_DEPTH + 1];
        bytes.push(0x00);
        assert_eq!(decode_canonical(&bytes), Err(CborError::DepthExceeded));
    }

    #[test]
    fn get_accessor() {
        let v = CborValue::Map(vec![("k".to_owned(), CborValue::Uint(7))]);
        assert_eq!(v.get("k").and_then(CborValue::as_uint), Some(7));
        assert!(v.get("missing").is_none());
    }

    // Property test: any in-profile value round-trips through encode/decode and
    // re-encodes byte-identically (spec §8 step 2 / §9 — the canonical-bytes trust
    // boundary). Bounded depth keeps generated values inside the profile.
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 256, ..proptest::test_runner::Config::default()
        })]
        #[test]
        fn uint_round_trips(n in 0u64..=u64::from(u32::MAX)) {
            let v = CborValue::Uint(n);
            let bytes = encode(&v);
            prop_assert_eq!(decode_canonical(&bytes).unwrap(), v);
            prop_assert_eq!(encode(&decode_canonical(&bytes).unwrap()), bytes);
        }

        #[test]
        fn text_bytes_array_round_trip(
            t in "[a-z]{0,64}",
            b in proptest::collection::vec(0u8..=255, 0..64),
            arr in proptest::collection::vec(0u64..100, 0..8)
        ) {
            let v = CborValue::Array(vec![
                CborValue::Text(t),
                CborValue::Bytes(b),
                CborValue::Array(arr.into_iter().map(CborValue::Uint).collect()),
            ]);
            let bytes = encode(&v);
            prop_assert_eq!(encode(&decode_canonical(&bytes).unwrap()), bytes);
        }
    }
}
