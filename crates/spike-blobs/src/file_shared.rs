//! Minimal `file.shared` reference (Event Protocol §7), just enough for the spike.
//!
//! The provider **creates** a [`FileShared`] after importing a blob; the fetcher
//! **consumes** it to learn `blob_hash` + provider `EndpointId`(s) before fetching.
//! This is the "create/consume a `file.shared` reference" requirement (AC6).
//!
//! Deliberate spike simplifications (documented in `NOTES.md`):
//!
//! - **No signing / no canonical CBOR.** The real event-core (IR-0007) signs a
//!   canonical CBOR map; here we only round-trip the `content` payload via
//!   `ciborium`. Field order is fixed by the struct, which is deterministic
//!   enough to demonstrate create/consume, but is *not* the normative canonical
//!   encoding.
//! - `blob_hash` and provider ids are carried as CBOR byte strings (`bstr`) to
//!   match the §7 schema shape (`blob_hash bstr[32]`, `providers [EndpointId]`).
//!
//! This module is intentionally free of any `iroh` / `iroh-blobs` types so the
//! reference shape stays transport-agnostic; callers convert the raw 32-byte
//! values into `iroh_blobs::Hash` / `iroh::EndpointId`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// `blob_format` value for a single content-addressed blob.
pub const BLOB_FORMAT_RAW: &str = "raw";
/// `blob_format` value for a hash-sequence (collection) root. Out of scope for
/// this spike (default `raw`); kept as a named constant for the observation in
/// `NOTES.md` §6.
pub const BLOB_FORMAT_HASH_SEQ: &str = "hash_seq";

/// Length in bytes of a BLAKE3-256 hash / an `EndpointId` (ed25519 public key).
pub const HASH_LEN: usize = 32;

/// Errors decoding/encoding a [`FileShared`] payload.
#[derive(Debug)]
pub enum FileSharedError {
    /// `ciborium` failed to decode the CBOR payload.
    Decode(String),
    /// `ciborium` failed to encode the CBOR payload.
    Encode(String),
    /// A byte-string field did not have the expected 32-byte length.
    BadLength {
        /// Which field was malformed.
        field: &'static str,
        /// The length actually found.
        found: usize,
    },
}

impl std::fmt::Display for FileSharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "file.shared decode failed: {e}"),
            Self::Encode(e) => write!(f, "file.shared encode failed: {e}"),
            Self::BadLength { field, found } => {
                write!(
                    f,
                    "file.shared field `{field}` must be {HASH_LEN} bytes, got {found}"
                )
            }
        }
    }
}

impl std::error::Error for FileSharedError {}

/// The `content` map of a `file.shared` event (Event Protocol §7), reduced to the
/// fields this spike needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileShared {
    /// Stable per-file identifier.
    pub file_id: String,
    /// Human-readable file name.
    pub name: String,
    /// MIME type of the content.
    pub mime_type: String,
    /// Size of the content in bytes.
    pub size_bytes: u64,
    /// BLAKE3-256 of the content, as a CBOR byte string (`bstr[32]`).
    pub blob_hash: ByteBuf,
    /// `"raw"` (default) or `"hash_seq"`.
    pub blob_format: String,
    /// Provider `EndpointId`s, each a 32-byte CBOR byte string. Defaults to the
    /// importing device's id (§7: `providers` opt, default `[device_id]`).
    pub providers: Vec<ByteBuf>,
}

impl FileShared {
    /// Build a `raw`-format `file.shared` for a freshly imported blob.
    #[must_use]
    pub fn new_raw(
        file_id: impl Into<String>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        size_bytes: u64,
        blob_hash: [u8; HASH_LEN],
        providers: &[[u8; HASH_LEN]],
    ) -> Self {
        Self {
            file_id: file_id.into(),
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes,
            blob_hash: ByteBuf::from(blob_hash.to_vec()),
            blob_format: BLOB_FORMAT_RAW.to_string(),
            providers: providers
                .iter()
                .map(|p| ByteBuf::from(p.to_vec()))
                .collect(),
        }
    }

    /// Encode the payload as CBOR (the "create" half of create/consume).
    ///
    /// # Errors
    /// Returns [`FileSharedError::Encode`] if `ciborium` serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, FileSharedError> {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf)
            .map_err(|e| FileSharedError::Encode(e.to_string()))?;
        Ok(buf)
    }

    /// Decode a CBOR payload (the "consume" half of create/consume).
    ///
    /// # Errors
    /// Returns [`FileSharedError::Decode`] if the bytes are not a valid encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, FileSharedError> {
        ciborium::from_reader(bytes).map_err(|e| FileSharedError::Decode(e.to_string()))
    }

    /// The declared content hash as a fixed 32-byte array.
    ///
    /// # Errors
    /// Returns [`FileSharedError::BadLength`] if `blob_hash` is not 32 bytes.
    pub fn blob_hash_array(&self) -> Result<[u8; HASH_LEN], FileSharedError> {
        bytes_to_array(&self.blob_hash, "blob_hash")
    }

    /// The declared provider `EndpointId`s as fixed 32-byte arrays.
    ///
    /// # Errors
    /// Returns [`FileSharedError::BadLength`] if any provider id is not 32 bytes.
    pub fn provider_arrays(&self) -> Result<Vec<[u8; HASH_LEN]>, FileSharedError> {
        self.providers
            .iter()
            .map(|p| bytes_to_array(p, "providers"))
            .collect()
    }
}

fn bytes_to_array(buf: &[u8], field: &'static str) -> Result<[u8; HASH_LEN], FileSharedError> {
    <[u8; HASH_LEN]>::try_from(buf).map_err(|_| FileSharedError::BadLength {
        field,
        found: buf.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::{FileShared, BLOB_FORMAT_RAW, HASH_LEN};

    fn sample() -> FileShared {
        let hash = [7u8; HASH_LEN];
        let provider = [9u8; HASH_LEN];
        FileShared::new_raw("file-1", "hello.txt", "text/plain", 11, hash, &[provider])
    }

    #[test]
    fn round_trips_through_cbor() {
        let original = sample();
        let bytes = original.encode().expect("encode");
        let decoded = FileShared::decode(&bytes).expect("decode");
        assert_eq!(original, decoded);
    }

    #[test]
    fn exposes_fixed_size_fields() {
        let fs = sample();
        assert_eq!(fs.blob_format, BLOB_FORMAT_RAW);
        assert_eq!(fs.blob_hash_array().expect("hash"), [7u8; HASH_LEN]);
        assert_eq!(
            fs.provider_arrays().expect("providers"),
            vec![[9u8; HASH_LEN]]
        );
    }

    #[test]
    fn rejects_wrong_length_hash() {
        let mut fs = sample();
        fs.blob_hash = serde_bytes::ByteBuf::from(vec![1u8; 31]);
        assert!(fs.blob_hash_array().is_err());
    }

    #[test]
    fn encoding_is_stable_for_equal_payloads() {
        // Field order is fixed by the struct, so equal payloads encode identically.
        assert_eq!(sample().encode().unwrap(), sample().encode().unwrap());
    }

    #[test]
    fn decode_rejects_garbage_bytes() {
        assert!(FileShared::decode(b"not valid cbor").is_err());
    }

    #[test]
    fn wrong_length_provider_id_fails() {
        let mut fs = sample();
        fs.providers = vec![serde_bytes::ByteBuf::from(vec![0u8; 31])];
        let err = fs.provider_arrays().unwrap_err();
        assert!(err.to_string().contains("providers"));
    }

    #[test]
    fn empty_providers_list_is_valid() {
        let fs = FileShared::new_raw("f", "n", "m", 0, [0u8; HASH_LEN], &[]);
        assert!(fs.providers.is_empty());
        let decoded = FileShared::decode(&fs.encode().expect("encode")).expect("decode");
        assert!(decoded.providers.is_empty());
    }

    #[test]
    fn multiple_providers_round_trip() {
        let p1 = [1u8; HASH_LEN];
        let p2 = [2u8; HASH_LEN];
        let fs = FileShared::new_raw("f", "n", "m", 10, [0u8; HASH_LEN], &[p1, p2]);
        let decoded = FileShared::decode(&fs.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.provider_arrays().expect("providers"), vec![p1, p2]);
    }

    #[test]
    fn error_display_messages_are_informative() {
        use super::FileSharedError;
        let decode_err = FileSharedError::Decode("some error".to_string());
        assert!(decode_err.to_string().contains("decode failed"));

        let len_err = FileSharedError::BadLength {
            field: "blob_hash",
            found: 16,
        };
        let msg = len_err.to_string();
        assert!(msg.contains("blob_hash"));
        assert!(msg.contains("16"));
        assert!(msg.contains("32")); // HASH_LEN
    }

    // --- Encode/decode stability and edge-case tests ---

    #[test]
    fn decode_then_reencode_matches_original_encoding() {
        // encode(decode(encode(x))) must equal encode(x): the CBOR encoding is stable
        // across a full decode/re-encode cycle (no information lost or reordered).
        let original = sample();
        let encoded = original.encode().expect("encode");
        let decoded = FileShared::decode(&encoded).expect("decode");
        let reencoded = decoded.encode().expect("re-encode");
        assert_eq!(
            encoded, reencoded,
            "re-encoding a decoded payload must be byte-identical"
        );
    }

    #[test]
    fn blob_format_hash_seq_constant_has_correct_value() {
        assert_eq!(super::BLOB_FORMAT_HASH_SEQ, "hash_seq");
    }

    #[test]
    fn zero_size_file_round_trips() {
        let fs = FileShared::new_raw(
            "empty-file",
            "empty.bin",
            "application/octet-stream",
            0,
            [0u8; HASH_LEN],
            &[[0u8; HASH_LEN]],
        );
        assert_eq!(fs.size_bytes, 0);
        let decoded = FileShared::decode(&fs.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.size_bytes, 0);
        assert_eq!(decoded.file_id, "empty-file");
    }

    #[test]
    fn truncated_cbor_decode_fails() {
        let encoded = sample().encode().expect("encode");
        // A payload cut in half must not decode successfully.
        let truncated = &encoded[..encoded.len() / 2];
        assert!(
            FileShared::decode(truncated).is_err(),
            "truncated CBOR must fail to decode"
        );
    }

    #[test]
    fn hash_seq_format_stored_and_round_trips() {
        let mut fs = FileShared::new_raw("f", "n", "m", 0, [0u8; HASH_LEN], &[]);
        fs.blob_format = super::BLOB_FORMAT_HASH_SEQ.to_string();
        let decoded = FileShared::decode(&fs.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.blob_format, super::BLOB_FORMAT_HASH_SEQ);
    }
}
