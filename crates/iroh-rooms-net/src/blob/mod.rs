//! [`BlobStore`] — the Blob Plane's durable local content store (IR-0202) — plus
//! the serve gate ([`serve`]) and verified fetch client ([`fetch`]) that close the
//! serve/fetch half of the Blob Plane (IR-0204).
//!
//! A thin, persistent wrapper over an [`iroh_blobs`] filesystem store
//! ([`FsStore`]) rooted at `<home>/blobs/`. It is the **producer/import** half of
//! `iroh-rooms file share`: it content-addresses a local file into a
//! restart-surviving store (making this node a real provider of the blob) and
//! answers "does this node hold hash `H`?" for provider-status reads. The bytes are
//! never carried on the room log — only the `file.shared` **reference** is
//! (PRD §9.2). All `iroh_blobs` types are isolated behind this wrapper so a version
//! bump touches one module (spec R1); the raw `[u8; 32]` hash crosses the crate
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
//! - **Import in-memory bytes (issue #84 / IR-0308, confirmed for `import_bytes`
//!   below):** `store.blobs().add_bytes(bytes)` returns the same `AddProgress` type
//!   as `add_path`; both share one `IntoFuture` impl whose default `.await` calls
//!   `with_tag()`, which creates a **persistent** tag (`Tags::create`) — durability
//!   parity with `add_path` confirmed directly on the 0.103.0 source, no temp-tag
//!   fallback needed.
//! - **Serve (IR-0204 §5.3/§6.1):** [`BlobsProtocol::new`]`(&store, Some(events))`
//!   builds the ALPN handler over a `&Store` — `FsStore: Deref<Target = Store>`, so
//!   passing `&self.store` here coerces automatically. [`BlobStore::serve_handler`]
//!   is the single place that constructs it, keeping `iroh_blobs::BlobsProtocol`
//!   behind this wrapper too.

use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::{BlobsProtocol, Hash};

pub mod fetch;
pub mod serve;

pub use fetch::{fetch_blob, fetch_blob_sized, FetchOutcome};
pub use serve::spawn_blob_gate;

/// Buffer size for the streaming BLAKE3 recompute. Bounds worst-case memory
/// regardless of file size (the size cap is a policy bound, not a memory one).
/// Kept a stack-friendly 8 `KiB` so it stays well under clippy's stack-array bound.
const RECOMPUTE_CHUNK: usize = 8 * 1024;

/// How long [`BlobStore::open`] waits to acquire the store's exclusive on-disk
/// lock before giving up with [`BlobError::Locked`].
///
/// The underlying `iroh-blobs` `FsStore` takes an exclusive `redb` lock while
/// open; a second open of the same directory (a concurrent `file list` /
/// `file share` while a `room tail` provider holds the store) **blocks
/// indefinitely** — it neither errors nor times out on its own. Bounding the
/// wait turns that silent deadlock into an actionable error. Kept comfortably
/// larger than a healthy open (milliseconds) so it never trips a legitimate
/// same-process reopen after a clean [`BlobStore::close`].
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// be opened; [`BlobError::Locked`] if the store's exclusive on-disk lock is
    /// not acquired within [`OPEN_TIMEOUT`] (another live process — typically a
    /// `room tail` provider — is holding the same store open).
    pub async fn open(dir: &Path) -> Result<Self, BlobError> {
        Self::open_with_timeout(dir, OPEN_TIMEOUT).await
    }

    /// [`BlobStore::open`] with an explicit lock-acquisition timeout (the seam the
    /// lock-contention test drives with a short bound; production uses
    /// [`OPEN_TIMEOUT`]).
    async fn open_with_timeout(dir: &Path, timeout: Duration) -> Result<Self, BlobError> {
        // Defensive: `FsStore::load` creates the required directories, but ensuring
        // the root exists first keeps the failure mode a clear "could not create
        // dir" rather than a store-internal error.
        std::fs::create_dir_all(dir)
            .map_err(|e| BlobError::Open(format!("could not create {}: {e}", dir.display())))?;
        // `FsStore::load` blocks forever if another process holds the exclusive
        // lock, so bound it: a timeout means the lock is held elsewhere, not that
        // the store is broken — surface that as `Locked` so callers can degrade.
        let store = tokio::time::timeout(timeout, FsStore::load(dir))
            .await
            .map_err(|_| BlobError::Locked(dir.display().to_string()))?
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

    /// Import in-memory bytes into the durable store, returning the content hash and
    /// byte length. The in-session analog of [`BlobStore::import_path`] for
    /// re-providing fetched bytes (issue #84 / IR-0308).
    ///
    /// Like `import_path`, the store computes the content hash during import and this
    /// method **independently** recomputes BLAKE3-256 over the same bytes and asserts
    /// they agree (an internal-bug guard, not a trust boundary). The blob is durably
    /// persisted via the same persistent-tag path `add_path` takes (module doc §
    /// "Confirmed `iroh-blobs 0.103.0`" above), so this node becomes a
    /// restart-surviving provider.
    ///
    /// # Errors
    /// [`BlobError::Import`] if the store import fails, or [`BlobError::HashMismatch`]
    /// if the store hash and the independent recompute disagree.
    pub async fn import_bytes(&self, bytes: Bytes) -> Result<BlobImport, BlobError> {
        let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        // In-memory: hashing a resident buffer (bounded by the share size cap) needs
        // no `spawn_blocking` — there is no file IO to offload off the reactor.
        let computed = *blake3::hash(&bytes).as_bytes();
        let tag = self
            .store
            .blobs()
            .add_bytes(bytes)
            .await
            .map_err(|e| BlobError::Import(e.to_string()))?;
        let store_hash = *tag.hash.as_bytes();
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

    /// Build the `iroh-blobs` protocol handler serving this store's blobs over the
    /// blobs ALPN, gated by `events` (the two-gate ACL from
    /// [`serve::spawn_blob_gate`]). Keeps `iroh_blobs::BlobsProtocol` behind this
    /// wrapper (spec R1); `FsStore` is a cheap, `Clone`-able handle, so the returned
    /// protocol keeps working after this call returns.
    #[must_use]
    pub fn serve_handler(&self, events: EventSender) -> BlobsProtocol {
        BlobsProtocol::new(&self.store, Some(events))
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
    /// The store's exclusive on-disk lock could not be acquired within the open
    /// timeout — another live process (typically a `room tail` provider) is
    /// holding the same store open. Distinct from [`BlobError::Open`] so callers
    /// (a concurrent `file list`) can degrade gracefully instead of failing hard.
    Locked(String),
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
    /// This session does not own a durable blob store, so it cannot import — the
    /// node was spawned without a `BlobServeConfig` (issue #84). Distinct from
    /// [`BlobError::Locked`]: nothing is holding a store, there simply is none in
    /// this session.
    NotServing,
}

impl core::fmt::Display for BlobError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "blob_store_open_error: {e}"),
            Self::Locked(dir) => write!(
                f,
                "blob_store_locked: another process is using the blob store at {dir} \
                 (is `room tail` running on this home?); try again once it exits"
            ),
            Self::Read(e) => write!(f, "blob_read_error: {e}"),
            Self::Import(e) => write!(f, "blob_import_error: {e}"),
            Self::Close(e) => write!(f, "blob_store_close_error: {e}"),
            Self::HashMismatch { store, computed } => write!(
                f,
                "blob_hash_mismatch: store import hash {store} != independent blake3 {computed}"
            ),
            Self::Status(e) => write!(f, "blob_status_error: {e}"),
            Self::NotServing => write!(
                f,
                "blob_not_serving: this session does not serve blobs; spawn the room with a \
                 BlobServeConfig to import in-session"
            ),
        }
    }
}

impl std::error::Error for BlobError {}

/// The gate's live view of authorization (IR-0204 spec §5.3): the two collections
/// [`serve::spawn_blob_gate`]'s two-gate ACL consults per message — the production
/// analog of `spike-blobs::acl::AuthContext`. Fail-closed: [`BlobAclView::empty`]
/// denies every connect and every hash until a real fold snapshot is folded in.
#[derive(Debug, Clone, Default)]
pub struct BlobAclView {
    active_devices: std::collections::HashSet<iroh::EndpointId>,
    referenced_hashes: std::collections::HashSet<[u8; 32]>,
}

impl BlobAclView {
    /// An empty view — fail-closed: every device and every hash is denied until
    /// populated.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build the view from the current membership snapshot's active members
    /// (Gate 1) and the room's referenced blob hashes (Gate 2, from
    /// [`iroh_rooms_core::sync::SyncEngine::file_shared_hashes`]).
    #[must_use]
    pub fn from_snapshot(
        snapshot: &iroh_rooms_core::membership::MembershipSnapshot,
        referenced_hashes: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Self {
        let mut active_devices = std::collections::HashSet::new();
        for m in snapshot.active_members() {
            if let Some(dev) = m.device {
                if let Ok(id) = iroh::EndpointId::from_bytes(dev.as_bytes()) {
                    active_devices.insert(id);
                }
            }
        }
        Self {
            active_devices,
            referenced_hashes: referenced_hashes.iter().copied().collect(),
        }
    }

    /// Gate 1 predicate: is `device` a currently active member's bound device?
    #[must_use]
    pub fn is_active(&self, device: iroh::EndpointId) -> bool {
        self.active_devices.contains(&device)
    }

    /// Gate 2 predicate: is `hash` referenced by a valid `file.shared` in the room?
    #[must_use]
    pub fn is_referenced(&self, hash: &[u8; 32]) -> bool {
        self.referenced_hashes.contains(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::{blake3_file, BlobAclView, BlobError, BlobStore};
    use bytes::Bytes;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::time::Duration;
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

    #[tokio::test]
    async fn open_on_a_held_store_reports_locked_within_the_timeout() {
        // The FsStore holds an exclusive on-disk lock; a second open of the same
        // dir blocks indefinitely (it never errors or times out on its own). `open`
        // bounds that wait and surfaces `Locked`, so a concurrent `file list` /
        // `file share` degrades instead of deadlocking. Uses a short timeout so the
        // test stays fast.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("blobs");
        let held = BlobStore::open(&dir).await.unwrap();

        match BlobStore::open_with_timeout(&dir, Duration::from_millis(300)).await {
            Err(BlobError::Locked(_)) => {}
            Err(other) => panic!("a held store must report Locked, not {other}"),
            Ok(_) => panic!("a second open while the store is held must not succeed"),
        }

        // Once the holder closes, the lock is released and a fresh open succeeds.
        held.close().await.unwrap();
        let reopened = BlobStore::open(&dir).await.unwrap();
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
        assert!(BlobError::Locked("/x/blobs".into())
            .to_string()
            .starts_with("blob_store_locked:"));
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
        assert!(BlobError::NotServing
            .to_string()
            .starts_with("blob_not_serving:"));
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

    // ── import_bytes (issue #84 / IR-0308) ───────────────────────────────────
    // The in-memory analog of import_path (re-provide fetched bytes in-session).
    // Mirrors the import_path tests above; spec §7.1.

    #[tokio::test]
    async fn import_bytes_hash_equals_independent_blake3_and_size_is_correct() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();

        let content = b"the quick brown fox jumps over the lazy dog";
        let import = store
            .import_bytes(Bytes::from_static(content))
            .await
            .unwrap();
        assert_eq!(
            import.hash,
            *blake3::hash(content).as_bytes(),
            "import_bytes hash must equal an independent BLAKE3-256 over the bytes"
        );
        assert_eq!(
            import.size_bytes,
            u64::try_from(content.len()).unwrap(),
            "size_bytes must equal the byte length"
        );
    }

    #[tokio::test]
    async fn has_is_true_after_import_bytes() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();

        let import = store
            .import_bytes(Bytes::from_static(b"re-provide me"))
            .await
            .unwrap();
        assert!(
            store.has(import.hash).await.unwrap(),
            "the store must hold the bytes it just imported (⇒ it is now a provider)"
        );
        assert!(
            !store.has([0x00; 32]).await.unwrap(),
            "the store must not claim to hold an unrelated hash"
        );
    }

    #[tokio::test]
    async fn empty_bytes_import_is_held() {
        // A 0-length payload is a valid content-addressed blob (parity with the
        // empty-file import above).
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();

        let import = store.import_bytes(Bytes::new()).await.unwrap();
        assert_eq!(import.size_bytes, 0);
        assert_eq!(
            import.hash,
            *blake3::hash(b"").as_bytes(),
            "empty bytes must hash to the well-known BLAKE3 empty digest"
        );
        assert!(store.has(import.hash).await.unwrap());
    }

    #[tokio::test]
    async fn import_bytes_provider_status_survives_reopen() {
        // Durability / persistent-tag (AC5 + Risk R2): a *temporary* tag would leave
        // the blob GC-eligible and this reopen would fail to find it. This is the
        // regression tripwire that fails loudly if `add_bytes` ever stops taking the
        // same persistent-tag path `add_path` does (spec Step 0).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("blobs");

        let store = BlobStore::open(&dir).await.unwrap();
        let import = store
            .import_bytes(Bytes::from_static(b"persist across restart"))
            .await
            .unwrap();
        assert!(store.has(import.hash).await.unwrap());
        // Cleanly close so the exclusive lock is released, then reopen the same dir.
        store.close().await.unwrap();

        let reopened = BlobStore::open(&dir).await.unwrap();
        assert!(
            reopened.has(import.hash).await.unwrap(),
            "a reopened store must still hold the byte-imported blob (persistent tag)"
        );
        reopened.close().await.unwrap();
    }

    #[tokio::test]
    async fn import_path_and_import_bytes_agree_on_hash() {
        // Content-addressing is source-independent: the same bytes imported by path
        // and from memory must produce the identical hash and size.
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::open(&tmp.path().join("blobs")).await.unwrap();

        let content = b"identical content, two import routes";
        let path = write_file(tmp.path(), "same.bin", content);

        let by_path = store.import_path(&path).await.unwrap();
        let by_bytes = store
            .import_bytes(Bytes::from_static(content))
            .await
            .unwrap();
        assert_eq!(
            by_path.hash, by_bytes.hash,
            "import_path and import_bytes must agree on the content hash"
        );
        assert_eq!(
            by_path.size_bytes, by_bytes.size_bytes,
            "both import routes must report the same size"
        );
    }

    // ── BlobAclView (IR-0204 §5.3) ───────────────────────────────────────────

    fn endpoint_id(seed: u8) -> iroh::EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn empty_view_is_fail_closed() {
        let view = BlobAclView::empty();
        assert!(!view.is_active(endpoint_id(1)));
        assert!(!view.is_referenced(&[0u8; 32]));
    }

    #[test]
    fn from_snapshot_active_member_device_is_active() {
        use iroh_rooms_core::event::binding::DeviceBinding;
        use iroh_rooms_core::event::content::{Content, EventType, RoomCreated};
        use iroh_rooms_core::event::keys::SigningKey;
        use iroh_rooms_core::event::signed::{self, SignedEvent};
        use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
        use iroh_rooms_core::event::wire::WireEvent;
        use iroh_rooms_core::membership::RoomMembership;

        let id_sk = SigningKey::from_seed(&[0x11; 32]);
        let dev_sk = SigningKey::from_seed(&[0x91; 32]);
        let nonce = [0x22u8; 16];
        let room_id = signed::derive_room_id(&id_sk.identity_key(), &nonce, 0);
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: id_sk.identity_key(),
            device_id: dev_sk.device_key(),
            event_type: EventType::RoomCreated,
            created_at: 0,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "acl-test".to_owned(),
                room_nonce: nonce,
                admins: vec![id_sk.identity_key()],
                device_binding: DeviceBinding::create(&room_id, &id_sk, dev_sk.device_key()),
            }),
        };
        let csb = ev.to_csb();
        let sig = signed::sign_csb(&csb, &dev_sk);
        let wire = WireEvent::seal(csb, sig);
        let validated =
            validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
                .expect("genesis must validate");
        let snapshot = RoomMembership::from_events(room_id, [validated]).snapshot();

        let referenced: BTreeSet<[u8; 32]> = [[0xAB; 32]].into_iter().collect();
        let view = BlobAclView::from_snapshot(&snapshot, &referenced);

        let admin_endpoint = iroh::EndpointId::from_bytes(dev_sk.device_key().as_bytes()).unwrap();
        assert!(
            view.is_active(admin_endpoint),
            "admin device must be active"
        );
        assert!(
            !view.is_active(endpoint_id(0xFF)),
            "unknown device must not be active"
        );
        assert!(view.is_referenced(&[0xAB; 32]));
        assert!(!view.is_referenced(&[0xCD; 32]));
    }
}
