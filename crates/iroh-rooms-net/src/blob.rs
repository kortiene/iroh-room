//! [`BlobStore`] — the Blob Plane's durable local content store (IR-0202).
//!
//! A thin, persistent wrapper over an [`iroh_blobs`] filesystem store
//! ([`FsStore`]) rooted at `<home>/blobs/`. It is the **producer/import** half of
//! `iroh-rooms file share`: it content-addresses a local file into a
//! restart-surviving store (making this node a real provider of the blob) and
//! answers "does this node hold hash `H`?" for provider-status reads. The bytes are
//! never carried on the room log — only the `file.shared` **reference** is
//! (PRD §9.2). All `iroh_blobs` types are isolated behind this wrapper so a version
//! bump touches one file (spec R1); the raw `[u8; 32]` hash crosses the crate
//! boundary, never `iroh_blobs::Hash`.
//!
//! ## Confirmed `iroh-blobs 0.103.0` persistent-store API (spec §6.1 / Step 0)
//!
//! The spike only exercised the in-memory `MemStore`; the durable surface was
//! confirmed on the 0.103.0 source before coding:
//!
//! - **Open/create:** [`iroh_blobs::store::fs::FsStore::load`]`(root)` opens (or
//!   creates) a store rooted at `root` (it creates `root/blobs.db` + data dirs).
//!   `FsStore` derefs to the [`iroh_blobs::api::Store`] API.
//! - **Import by path (streamed copy):** `store.blobs().add_path(path)` returns an
//!   `AddProgress`; awaiting it yields a `TagInfo { hash, .. }` and creates a
//!   **persistent** tag so the content is durable (not GC-eligible). `add_path`
//!   uses `ImportMode::Copy`, so the store owns an independent copy — the original
//!   file may change or vanish afterwards.
//! - **Presence:** `store.blobs().has(hash)` returns `true` iff the store holds the
//!   *complete* blob (it maps `BlobStatus::Complete`), which survives restart.
//! - **Hash:** the store's content hash is BLAKE3-256 (`iroh_blobs::Hash`), the same
//!   digest the spike relied on; `Hash::as_bytes()` yields the raw `[u8; 32]`.
//!
//! ## Out of scope (the follow-up serve/fetch issue — spec §4.3)
//!
//! Serving these blobs to peers over the `iroh-blobs` ALPN with the spike's
//! two-gate ACL (`spike-blobs/src/net.rs::spawn_event_gate`), the consumer
//! `file fetch` with receiver-side BLAKE3 recompute, and any live broadcast of the
//! `file.shared` frame are **not** built here. This wrapper imports and records
//! local provider status; it never serves bytes.

use std::io::{self, Read};
use std::path::Path;

use iroh_blobs::store::fs::FsStore;
use iroh_blobs::Hash;

/// Buffer size for the streaming BLAKE3 recompute. Bounds worst-case memory
/// regardless of file size (the size cap is a policy bound, not a memory one).
/// Kept a stack-friendly 8 `KiB` so it stays well under clippy's stack-array bound.
const RECOMPUTE_CHUNK: usize = 8 * 1024;

/// The outcome of a successful [`BlobStore::import_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobImport {
    /// The imported content's BLAKE3-256 digest (raw 32 bytes). Equal to both the
    /// store's own import hash and the independent recompute (asserted equal).
    pub hash: [u8; 32],
    /// The number of content bytes imported.
    pub size_bytes: u64,
}

/// A durable, content-addressed local blob store (a persistent `iroh-blobs`
/// filesystem store). Holding a blob here **is** being a local provider of it.
pub struct BlobStore {
    store: FsStore,
}

impl BlobStore {
    /// Open (creating if absent) a durable blob store under `dir`.
    ///
    /// `dir` is created if it does not exist. Callers that need owner-only
    /// permissions on `dir` (the CLI roots this at the `0700` `<home>/blobs/`)
    /// should tighten them before/after; this wrapper does not loosen anything.
    ///
    /// # Errors
    /// [`BlobError::Open`] if the directory cannot be created or the store cannot
    /// be opened.
    pub async fn open(dir: &Path) -> Result<Self, BlobError> {
        // Defensive: `FsStore::load` creates the required directories, but ensuring
        // the root exists first keeps the failure mode a clear "could not create
        // dir" rather than a store-internal error.
        std::fs::create_dir_all(dir)
            .map_err(|e| BlobError::Open(format!("could not create {}: {e}", dir.display())))?;
        let store = FsStore::load(dir)
            .await
            .map_err(|e| BlobError::Open(e.to_string()))?;
        Ok(Self { store })
    }

    /// Import a file by path into the durable store (streamed copy, not fully
    /// buffered), returning the content hash and byte length.
    ///
    /// The store computes the content hash during import; this method
    /// **independently** recomputes BLAKE3-256 over the same file and asserts the
    /// two agree (spike NOTES.md §4 belt-and-suspenders — an internal-bug /
    /// concurrent-modification guard, not a trust boundary). The blob is durably
    /// persisted (a persistent tag protects it), so the node becomes a
    /// restart-surviving provider.
    ///
    /// # Errors
    /// [`BlobError::Import`] if the store import fails, [`BlobError::Read`] if the
    /// file cannot be re-read for the recompute, or [`BlobError::HashMismatch`] if
    /// the store hash and the independent recompute disagree.
    pub async fn import_path(&self, path: &Path) -> Result<BlobImport, BlobError> {
        // Durable, content-addressed import (Copy mode: the store owns a snapshot).
        let tag = self
            .store
            .blobs()
            .add_path(path)
            .await
            .map_err(|e| BlobError::Import(e.to_string()))?;
        let store_hash = *tag.hash.as_bytes();

        // Independent BLAKE3-256 recompute over the same file, on a blocking thread
        // so the async reactor is never stalled by file IO on a large blob.
        let path_buf = path.to_path_buf();
        let (computed, size_bytes) = tokio::task::spawn_blocking(move || blake3_file(&path_buf))
            .await
            .map_err(|e| BlobError::Read(format!("recompute task failed: {e}")))?
            .map_err(|e| BlobError::Read(e.to_string()))?;

        if computed != store_hash {
            return Err(BlobError::HashMismatch {
                store: hex::encode(store_hash),
                computed: hex::encode(computed),
            });
        }
        Ok(BlobImport {
            hash: store_hash,
            size_bytes,
        })
    }

    /// Whether this store currently holds the *complete* blob `hash` (⇒ this node
    /// is a local provider of it). Durable across restart.
    ///
    /// # Errors
    /// [`BlobError::Status`] if the presence query fails.
    pub async fn has(&self, hash: [u8; 32]) -> Result<bool, BlobError> {
        self.store
            .blobs()
            .has(Hash::from(hash))
            .await
            .map_err(|e| BlobError::Status(e.to_string()))
    }

    /// Flush and cleanly close the store, releasing its exclusive on-disk lock.
    ///
    /// The underlying `iroh-blobs` filesystem store holds an exclusive lock on its
    /// database while open; a subsequent open of the same directory (a later
    /// `file list` / `file share` in a fresh process) blocks until the previous
    /// handle releases it. Process exit releases it too, but calling this
    /// guarantees the imported blob is durably flushed and the lock is dropped
    /// before the command reports success — and lets the same process reopen the
    /// directory without deadlocking on the lock.
    ///
    /// # Errors
    /// [`BlobError::Close`] if the shutdown request fails.
    pub async fn close(self) -> Result<(), BlobError> {
        self.store
            .shutdown()
            .await
            .map_err(|e| BlobError::Close(e.to_string()))
    }
}

/// Stream a file through a BLAKE3 hasher with bounded memory, returning the
/// digest and the total byte count hashed.
fn blake3_file(path: &Path) -> io::Result<([u8; 32], u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; RECOMPUTE_CHUNK];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += u64::try_from(n).expect("a read count fits in u64");
    }
    Ok((*hasher.finalize().as_bytes(), total))
}

/// A Blob Plane local-store fault. Each `Display` carries a stable, greppable code
/// prefix (mirrors [`crate::FrameError`] / the pipe error taxonomy).
#[derive(Debug)]
#[non_exhaustive]
pub enum BlobError {
    /// The durable store could not be created/opened at the given directory.
    Open(String),
    /// The source file could not be read for the independent recompute.
    Read(String),
    /// Importing the blob into the store failed.
    Import(String),
    /// Cleanly shutting the store down (flush + release the lock) failed.
    Close(String),
    /// The store's import hash disagreed with the independent BLAKE3-256 recompute
    /// (an internal-bug or concurrent-modification guard).
    HashMismatch {
        /// The hash the store reported, lowercase hex.
        store: String,
        /// The independently recomputed hash, lowercase hex.
        computed: String,
    },
    /// Querying blob presence failed.
    Status(String),
}

impl core::fmt::Display for BlobError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "blob_store_open_error: {e}"),
            Self::Read(e) => write!(f, "blob_read_error: {e}"),
            Self::Import(e) => write!(f, "blob_import_error: {e}"),
            Self::Close(e) => write!(f, "blob_store_close_error: {e}"),
            Self::HashMismatch { store, computed } => write!(
                f,
                "blob_hash_mismatch: store import hash {store} != independent blake3 {computed}"
            ),
            Self::Status(e) => write!(f, "blob_status_error: {e}"),
        }
    }
}

impl std::error::Error for BlobError {}

#[cfg(test)]
mod tests {
    use super::{blake3_file, BlobError, BlobStore};
    use std::path::Path;
    use tempfile::TempDir;

    /// Write `bytes` to `<dir>/<name>` and return the path.
    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write fixture file");
        path
    }

    #[tokio::test]
    async fn import_hash_equals_independent_blake3_and_size_is_correct() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();

        let content = b"the quick brown fox jumps over the lazy dog";
        let path = write_file(tmp.path(), "fox.txt", content);

        let import = store.import_path(&path).await.unwrap();
        let expected = *blake3::hash(content).as_bytes();
        assert_eq!(
            import.hash, expected,
            "import hash must equal an independent BLAKE3-256 over the bytes"
        );
        assert_eq!(
            import.size_bytes,
            u64::try_from(content.len()).unwrap(),
            "size_bytes must equal the file length"
        );
    }

    #[tokio::test]
    async fn has_is_true_after_import_and_false_for_unrelated_hash() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();
        let path = write_file(tmp.path(), "data.bin", b"hold me");

        let import = store.import_path(&path).await.unwrap();
        assert!(
            store.has(import.hash).await.unwrap(),
            "the store must hold the blob it just imported"
        );
        assert!(
            !store.has([0x00; 32]).await.unwrap(),
            "the store must not claim to hold an unrelated hash"
        );
    }

    #[tokio::test]
    async fn empty_file_imports_and_is_held() {
        // A 0-byte file is a valid content-addressed blob (spec §7 allows it).
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();
        let path = write_file(tmp.path(), "empty", b"");

        let import = store.import_path(&path).await.unwrap();
        assert_eq!(import.size_bytes, 0);
        assert_eq!(import.hash, *blake3::hash(b"").as_bytes());
        assert!(store.has(import.hash).await.unwrap());
    }

    #[tokio::test]
    async fn provider_status_survives_reopen() {
        // Durability (AC3): a fresh store over the same dir still reports the blob.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("blobs");
        let path = write_file(tmp.path(), "durable.txt", b"persist across restart");

        let store = BlobStore::open(&dir).await.unwrap();
        let import = store.import_path(&path).await.unwrap();
        assert!(store.has(import.hash).await.unwrap());
        // Cleanly close so the exclusive lock is released — the same-process analogue
        // of the fresh-process `file list` after a `file share`.
        store.close().await.unwrap();

        let reopened = BlobStore::open(&dir).await.unwrap();
        assert!(
            reopened.has(import.hash).await.unwrap(),
            "a reopened store must still hold the blob (durable provider status)"
        );
        reopened.close().await.unwrap();
    }

    #[test]
    fn blake3_file_streams_large_input_across_chunks() {
        // A payload spanning multiple RECOMPUTE_CHUNK reads must hash identically to
        // a single-shot blake3 over the whole buffer, and count every byte.
        let tmp = TempDir::new().unwrap();
        let big = vec![0xABu8; super::RECOMPUTE_CHUNK * 3 + 7];
        let path = write_file(tmp.path(), "big.bin", &big);
        let (digest, size) = blake3_file(&path).unwrap();
        assert_eq!(digest, *blake3::hash(&big).as_bytes());
        assert_eq!(size, u64::try_from(big.len()).unwrap());
    }

    #[test]
    fn error_display_strings_carry_stable_codes() {
        assert!(BlobError::Open("x".into())
            .to_string()
            .starts_with("blob_store_open_error:"));
        assert!(BlobError::Read("x".into())
            .to_string()
            .starts_with("blob_read_error:"));
        assert!(BlobError::Import("x".into())
            .to_string()
            .starts_with("blob_import_error:"));
        assert!(BlobError::Close("x".into())
            .to_string()
            .starts_with("blob_store_close_error:"));
        assert!(BlobError::Status("x".into())
            .to_string()
            .starts_with("blob_status_error:"));
        let mismatch = BlobError::HashMismatch {
            store: "aa".into(),
            computed: "bb".into(),
        };
        assert!(mismatch.to_string().starts_with("blob_hash_mismatch:"));
    }

    #[test]
    fn blake3_file_empty_file_yields_known_digest() {
        // BLAKE3 of zero bytes has a well-known value; verify our streaming
        // implementation agrees and reports size = 0.
        let tmp = TempDir::new().unwrap();
        let path = write_file(tmp.path(), "empty.bin", b"");
        let (digest, size) = blake3_file(&path).unwrap();
        assert_eq!(size, 0, "empty file must have size 0");
        assert_eq!(
            digest,
            *blake3::hash(b"").as_bytes(),
            "empty file must hash to the well-known BLAKE3 empty digest"
        );
    }

    #[tokio::test]
    async fn import_nonexistent_path_returns_import_error() {
        // A missing source file must produce BlobError::Import (the FsStore
        // rejects it), not a panic or an unexpected variant.
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();
        let missing = tmp.path().join("does_not_exist.bin");
        let err = store.import_path(&missing).await.unwrap_err();
        assert!(
            matches!(err, BlobError::Import(_)),
            "expected BlobError::Import for a missing source path, got: {err}"
        );
    }
}
