//! Purpose-built deterministic-CBOR codec for the signed-event trust boundary.
//!
//! This module implements the spec's recommended **D1** strategy: a small,
//! self-contained canonical-CBOR encoder plus a *strict* reader that validates
//! the RFC 8949 §4.2.1 core-deterministic profile **while parsing**, rather than
//! trusting a general library's default ordering on the signature payload.
//!
//! The supported value space is deliberately a **closed profile** matching
//! Event Protocol §2/§3/§7: unsigned integers, byte strings, text strings,
//! arrays, and text-keyed maps. Everything else — negative integers, tags,
//! floats/simple values, indefinite-length items, non-text map keys — is
//! rejected as [`CborError`]. The decoder additionally enforces shortest-form
//! integers, definite lengths, strictly-ascending unique map keys, valid UTF-8,
//! bounded nesting depth, and no trailing data.
//!
//! Equivalently to "decode → re-encode → byte-equal", a successful
//! [`decode_canonical`] guarantees `encode(decode_canonical(b)) == b`; the
//! validator additionally re-checks this for defence in depth (Event Protocol
//! §6 step 4).

use core::fmt;

/// Maximum CBOR nesting depth accepted by [`decode_canonical`].
///
/// The real envelope/`signed`/content structures nest only a few levels; a
/// tight cap fails closed on adversarially-nested input without recursion blowup.
const MAX_DEPTH: usize = 16;

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
///
/// Every variant maps to the `non_canonical_encoding` rejection reason at the
/// validation layer (Event Protocol §6 step 1/4, §8).
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
/// Map entries are emitted in canonical key order (bytewise over the encoded key
/// form, i.e. length-first then bytewise) regardless of insertion order, so the
/// output is independent of how the value was built.
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
/// Our own values always fit; saturate rather than panic if they somehow do not.
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

/// Write a CBOR item head (major type + shortest-form argument), cast-free.
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
/// deterministic profile and requiring that the entire input is consumed.
///
/// # Errors
/// Returns a [`CborError`] for any malformed or non-canonical encoding. Never
/// panics, never allocates more than the input bounds, and is bounded in depth.
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
                // Non-shortest: a value that fits in the 1-byte form.
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
            // 31 = indefinite length.
            _ => return Err(CborError::IndefiniteLength),
        };
        Ok((major, arg))
    }

    /// Convert a declared length/count to `usize`, rejecting values that cannot
    /// possibly be backed by the remaining input (anti-DoS preallocation guard).
    fn checked_len(&self, arg: u64, min_bytes_each: usize) -> Result<usize, CborError> {
        let n = usize::try_from(arg).map_err(|_| CborError::LengthOverflow)?;
        let remaining = self.buf.len() - self.pos;
        // Each element needs at least `min_bytes_each` bytes; reject impossible
        // counts before allocating.
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
                // Each array element is at least one byte.
                let n = self.checked_len(arg, 1)?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.read_value(depth + 1)?);
                }
                Ok(CborValue::Array(items))
            }
            5 => {
                // Each entry is at least two bytes (1-byte key head + 1-byte value).
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
            // major == 7
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
}
