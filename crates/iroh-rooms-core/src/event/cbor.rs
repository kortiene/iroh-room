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

// Direct unit tests for the strict reader (risk R1 — spec
// `strict-cbor-reader-unit-property-fuzz-tests.md` §5). One crafted input per
// `CborError` variant asserting the exact variant, plus accept-path and
// boundary cases. Property/fuzz tests live separately in
// `tests/cbor_property.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    // Decode a hex fixture (spaces allowed for readability) into raw bytes.
    fn hx(s: &str) -> Vec<u8> {
        hex::decode(s.replace(' ', "")).expect("valid hex fixture")
    }

    // ---- One case per `CborError` variant (14). ----

    #[test]
    fn unexpected_eof_on_short_bytes_payload() {
        // bstr(3) declares 3 bytes; only 2 follow.
        assert_eq!(
            decode_canonical(&hx("43 01 02")),
            Err(CborError::UnexpectedEof)
        );
    }

    #[test]
    fn unexpected_eof_on_truncated_head() {
        // 24-form head with no following argument byte.
        assert_eq!(decode_canonical(&hx("18")), Err(CborError::UnexpectedEof));
    }

    #[test]
    fn unexpected_eof_guards_oversized_declared_length_without_allocating() {
        // bstr(32) declares 32 bytes; only 5 are present. `checked_len` must
        // reject this before any allocation of the declared length — the
        // concrete R1/DoS preallocation guard.
        let mut bytes = hx("58 20");
        bytes.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(decode_canonical(&bytes), Err(CborError::UnexpectedEof));
    }

    #[test]
    fn trailing_data_after_complete_item() {
        for input in ["00 00", "a0 00"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::TrailingData),
                "input {input}"
            );
        }
    }

    #[test]
    fn indefinite_length_is_rejected() {
        // Major-0 indefinite (invalid on its own), plus every indefinite
        // container form (bstr/tstr/array/map).
        for input in ["1f", "5f", "7f", "9f", "bf"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::IndefiniteLength),
                "input {input}"
            );
        }
    }

    #[test]
    fn non_shortest_int_at_every_width() {
        for input in [
            "18 17",                      // 1-byte form of 23 (fits immediate).
            "19 00 ff",                   // 2-byte form of 255 (fits 1-byte).
            "1a 00 00 ff ff",             // 4-byte form of 65535 (fits 2-byte).
            "1b 00 00 00 00 ff ff ff ff", // 8-byte form of 2^32-1 (fits 4-byte).
        ] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::NonShortestInt),
                "input {input}"
            );
        }
    }

    #[test]
    fn non_shortest_int_enforced_on_length_fields_too() {
        // bstr length 23 written in the 1+1 (24-form) instead of immediate —
        // proves shortest-form applies to lengths, not just integer values
        // (D4: `read_head` enforces this for every item head).
        assert_eq!(
            decode_canonical(&hx("58 17")),
            Err(CborError::NonShortestInt)
        );
    }

    #[test]
    fn negative_integer_is_rejected() {
        for input in ["20", "38 63"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::NegativeInteger),
                "input {input}"
            );
        }
    }

    #[test]
    fn reserved_additional_info_is_rejected() {
        for input in ["1c", "1d", "1e"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::ReservedAdditionalInfo),
                "input {input}"
            );
        }
    }

    #[test]
    fn tag_is_rejected_before_any_payload_is_read() {
        // Major-6 dispatches immediately (D4) — tag 0, no payload bytes needed.
        assert_eq!(decode_canonical(&hx("c0")), Err(CborError::Tag));
    }

    #[test]
    fn float_or_simple_is_rejected() {
        // Immediate simple values (false/true/null/undefined) and the 1-byte
        // form (info 24, value > 23). Deliberately NOT f9/fa/fb (float16/32/64),
        // whose payload could trip the shortest-form check first (D4).
        for input in ["f4", "f5", "f6", "f7", "f8 ff"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::FloatOrSimple),
                "input {input}"
            );
        }
    }

    #[test]
    fn non_text_map_key_is_rejected() {
        for input in ["a1 00 00", "a1 41 61 00"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::NonTextMapKey),
                "input {input}"
            );
        }
    }

    #[test]
    fn unsorted_map_key_is_rejected() {
        // "b" then "a" — plain descending order.
        assert_eq!(
            decode_canonical(&hx("a2 61 62 00 61 61 00")),
            Err(CborError::UnsortedMapKey)
        );
        // "aa" (len 2) then "b" (len 1) — violates length-first tiebreak even
        // though "aa" > "b" bytewise is irrelevant; length must sort first.
        assert_eq!(
            decode_canonical(&hx("a2 62 61 61 00 61 62 00")),
            Err(CborError::UnsortedMapKey)
        );
    }

    #[test]
    fn duplicate_map_key_is_rejected() {
        assert_eq!(
            decode_canonical(&hx("a2 61 61 00 61 61 00")),
            Err(CborError::DuplicateMapKey)
        );
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        for input in ["62 ff ff", "a1 62 ff ff 00"] {
            assert_eq!(
                decode_canonical(&hx(input)),
                Err(CborError::InvalidUtf8),
                "input {input}"
            );
        }
    }

    #[test]
    fn depth_exceeded_one_past_the_cap() {
        // MAX_DEPTH + 1 nested single-element arrays; the innermost leaf would
        // be read at depth MAX_DEPTH + 1 — one past the cap.
        let mut bytes = vec![0x81u8; MAX_DEPTH + 1];
        bytes.push(0x00);
        assert_eq!(decode_canonical(&bytes), Err(CborError::DepthExceeded));
    }

    #[test]
    fn length_overflow_on_huge_declared_count() {
        // map(*) count 2^63; `checked_len`'s `n.checked_mul(2)` overflows a
        // 64-bit `usize` before any allocation is attempted.
        assert_eq!(
            decode_canonical(&hx("bb 80 00 00 00 00 00 00 00")),
            Err(CborError::LengthOverflow)
        );
    }

    // ---- Accept paths + round-trip, for all five `CborValue` kinds. ----

    #[test]
    fn uint_boundaries_accept_and_round_trip() {
        let cases = [
            ("00", 0u64),
            ("17", 23),
            ("18 18", 24),
            ("18 ff", 255),
            ("19 01 00", 256),
            ("19 ff ff", 65_535),
            ("1a 00 01 00 00", 65_536),
            ("1b 00 00 00 01 00 00 00 00", 1u64 << 32),
        ];
        for (input, expected) in cases {
            let bytes = hx(input);
            let value = decode_canonical(&bytes).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(value, CborValue::Uint(expected), "input {input}");
            assert_eq!(encode(&value), bytes, "round-trip for {input}");
        }
    }

    #[test]
    fn bytes_accept_and_round_trip() {
        for (input, expected) in [("40", vec![]), ("43 01 02 03", vec![1u8, 2, 3])] {
            let bytes = hx(input);
            let value = decode_canonical(&bytes).unwrap();
            assert_eq!(value, CborValue::Bytes(expected));
            assert_eq!(encode(&value), bytes);
        }
    }

    #[test]
    fn text_accept_and_round_trip() {
        for (input, expected) in [("60", ""), ("65 68 65 6c 6c 6f", "hello")] {
            let bytes = hx(input);
            let value = decode_canonical(&bytes).unwrap();
            assert_eq!(value, CborValue::Text(expected.to_owned()));
            assert_eq!(encode(&value), bytes);
        }
    }

    #[test]
    fn array_accept_and_round_trip() {
        let empty = hx("80");
        assert_eq!(decode_canonical(&empty).unwrap(), CborValue::Array(vec![]));
        assert_eq!(encode(&CborValue::Array(vec![])), empty);

        let bytes = hx("82 01 02");
        let expected = CborValue::Array(vec![CborValue::Uint(1), CborValue::Uint(2)]);
        assert_eq!(decode_canonical(&bytes).unwrap(), expected);
        assert_eq!(encode(&expected), bytes);
    }

    #[test]
    fn map_accept_and_round_trip() {
        let empty = hx("a0");
        assert_eq!(decode_canonical(&empty).unwrap(), CborValue::Map(vec![]));
        assert_eq!(encode(&CborValue::Map(vec![])), empty);

        let one = hx("a1 61 61 01");
        let one_expected = CborValue::Map(vec![("a".to_owned(), CborValue::Uint(1))]);
        assert_eq!(decode_canonical(&one).unwrap(), one_expected);
        assert_eq!(encode(&one_expected), one);

        let two = hx("a2 61 61 01 61 62 02");
        let two_expected = CborValue::Map(vec![
            ("a".to_owned(), CborValue::Uint(1)),
            ("b".to_owned(), CborValue::Uint(2)),
        ]);
        assert_eq!(decode_canonical(&two).unwrap(), two_expected);
        assert_eq!(encode(&two_expected), two);
    }

    #[test]
    fn nesting_at_max_depth_is_accepted() {
        // The boundary counterpart to `depth_exceeded_one_past_the_cap`: exactly
        // MAX_DEPTH nested arrays, leaf read at depth MAX_DEPTH, must be accepted.
        let mut bytes = vec![0x81u8; MAX_DEPTH];
        bytes.push(0x00);
        let value = decode_canonical(&bytes).expect("MAX_DEPTH nesting must be accepted");
        assert_eq!(encode(&value), bytes);
    }

    // ---- Canonical map ordering (encoder side). ----

    #[test]
    fn encoder_sorts_scrambled_map_keys_into_canonical_order() {
        let scrambled = CborValue::Map(vec![
            ("b".to_owned(), CborValue::Uint(2)),
            ("a".to_owned(), CborValue::Uint(1)),
        ]);
        assert_eq!(encode(&scrambled), hx("a2 61 61 01 61 62 02"));
    }

    #[test]
    fn encoder_orders_shorter_key_before_longer_key() {
        // "z" (len 1) must sort before "aa" (len 2) under length-first order,
        // even though 'z' > 'a' bytewise.
        let value = CborValue::Map(vec![
            ("aa".to_owned(), CborValue::Uint(2)),
            ("z".to_owned(), CborValue::Uint(1)),
        ]);
        let decoded = decode_canonical(&encode(&value)).unwrap();
        assert_eq!(
            decoded,
            CborValue::Map(vec![
                ("z".to_owned(), CborValue::Uint(1)),
                ("aa".to_owned(), CborValue::Uint(2)),
            ])
        );
    }
}
