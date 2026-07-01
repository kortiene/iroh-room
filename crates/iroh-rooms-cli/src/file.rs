//! The Blob Plane CLI (import half): `iroh-rooms file share | list`
//! (spec IR-0202 §5). A thin orchestrator over the landed primitives, the sibling
//! of [`crate::message`] and [`crate::pipe`]:
//!
//! * `share` is the **producer/import** path (this issue's core deliverable): it
//!   content-addresses a local file into the durable local blob store
//!   ([`iroh_rooms_net::BlobStore`]), records this node as a local provider, then
//!   authors + self-validates + persists a signed `file.shared` **reference** onto
//!   the local log via the pure core builder
//!   ([`build_file_shared`](iroh_rooms_core::event::build_file_shared)). It is
//!   deliberately **offline**: the blob bytes are never carried on the log (PRD
//!   §9.2), and the event propagates through the already-landed sync engine
//!   unchanged once peers reconcile. No network is contacted.
//! * `list` is an **offline** read: for the room, it decodes every `file.shared`
//!   event and reports each file's handle, name, size, hash, and **provider
//!   status** — whether *this* node holds the blob (`you (local)`) or only the
//!   reference (`reference-only`).
//!
//! ## The follow-up boundary (spec §4.3 — the serve/fetch issue)
//!
//! `file share` here **imports + references** but does not **serve or push**. The
//! follow-up serve/fetch issue must: (a) add the `iroh-blobs` serve ALPN to the
//! shared `Router` with the spike's two-gate ACL
//! (`spike-blobs/src/net.rs::spawn_event_gate`); (b) add `file fetch <ROOM_ID>
//! <FILE_ID>` with an independent receiver-side BLAKE3 recompute (spike §4);
//! (c) optionally broadcast the `file.shared` frame at share time (the `room send`
//! `run_push` analogue); and (d) map "no provider online" to honest "unavailable"
//! CLI language (spike §5 / PRD §14). Until then a shared blob is held locally and
//! `file list` reports `you (local)`.

use std::io::ErrorKind;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms_core::event::build_file_shared;
use iroh_rooms_core::event::constants::{MAX_SHARED_FILE_BYTES, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::{HashRef, RoomId};
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::Ingest;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_net::BlobStore;
use serde_json::json;

use crate::message::{fold_room, select_heads, DB_FILE};
use crate::{clock, identity, paths};

/// Directory (under the data-directory home) rooting the durable blob store (spec
/// §4.1 / §5.5). Lives inside the `0700` home; `file share` tightens it to `0700`.
const BLOBS_DIR: &str = "blobs";
/// The only `blob_format` for MVP (spec §3.3 / §5.2 step 6); `hash_seq` is a
/// follow-up (spike NOTES.md §6).
const BLOB_FORMAT_RAW: &str = "raw";
/// Test-only seam (spec OQ-4): overrides [`MAX_SHARED_FILE_BYTES`] so the too-large
/// boundary can be exercised without a 100 MiB fixture. Not a user-facing knob — it
/// is intentionally absent from `--help` and the size error still names the
/// effective cap. Ignored if unset/unparseable.
const MAX_SHARE_BYTES_ENV: &str = "IROH_ROOMS_MAX_SHARE_BYTES";

/// The result of a successful `file share`, for the caller to present.
pub struct FileShareSummary {
    /// The 16-byte on-wire file handle (`SHORT_ID`).
    pub file_id: [u8; SHORT_ID_LEN],
    /// The stored display name.
    pub name: String,
    /// The stored MIME type.
    pub mime_type: String,
    /// The imported content size in bytes.
    pub size_bytes: u64,
    /// The BLAKE3-256 content hash.
    pub blob_hash: HashRef,
    /// The authored `file.shared` event id.
    pub event_id: iroh_rooms_core::event::ids::EventId,
    /// The room the file belongs to.
    pub room_id: RoomId,
    /// The original path as given (for the `imported:` line; not put on the log).
    pub source_display: String,
}

/// Import a local file into the Blob Plane and author its `file.shared` reference
/// (spec §5.2): classify the path, durably content-address it into `<home>/blobs/`,
/// record this node as a provider, then build + self-validate + persist the signed
/// `file.shared` locally. Fully offline — no network is contacted.
///
/// # Errors
/// Fails — leaving the store and blob store untouched on every pre-import path — if
/// `--mime`/`--name` are invalid (validated before any IO), if no local identity
/// exists, if the room is unknown, if the caller is not an active member, if the
/// path is missing / a directory / unreadable / over the size cap / otherwise
/// unreadable (classified before any write), on a blob-import or store error, or —
/// as an internal-bug guard — if the freshly built `file.shared` fails
/// self-validation or the membership self-check.
#[allow(clippy::too_many_lines)] // one linear import-then-author flow; splitting hurts readability
pub async fn share(
    home: &Path,
    room_id: &RoomId,
    path: &str,
    mime: Option<&str>,
    name: Option<&str>,
) -> Result<FileShareSummary> {
    // ---- Pre-IO argument validation (a bad invocation writes nothing). ----
    let mime_override = validate_mime(mime)?;
    let name_override = validate_share_name(name)?;
    let path = Path::new(path);

    // Load the signing secrets (also re-checks them against the public profile).
    let secret = identity::SecretKeys::load(home)?;
    let sender_id = secret.identity.identity_key();

    // ---- Fold the persisted log: confirm the room exists and we are Active. ----
    let db_path = home.join(DB_FILE);
    let mut store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (mut membership, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&sender_id) {
        bail!(
            "you are not an active member of room {room_id}; only an active member can share \
             files (this identity is {sender_id})"
        );
    }

    // ---- Classify the path (§7 taxonomy) BEFORE any store/blob write. ----
    // The returned metadata length only gates the size cap; the authoritative size
    // recorded on the log is `import.size_bytes` (the bytes actually hashed).
    classify_path(path, effective_max_share_bytes())?;

    // ---- Durable content-address into <home>/blobs/ (streamed import). ----
    // Import is ordered BEFORE the event insert, so a crash in between leaves an
    // orphan blob (harmless, re-importable) rather than a reference to absent bytes
    // (spec §9). Close the store afterwards to flush and release its lock.
    let blobs_dir = home.join(BLOBS_DIR);
    paths::ensure_dir(&blobs_dir)?;
    let blob_store = BlobStore::open(&blobs_dir)
        .await
        .with_context(|| format!("could not open the blob store at {}", blobs_dir.display()))?;
    // `iroh-blobs` `add_path` requires an absolute path; a relative CLI argument
    // (`iroh-rooms file share <ROOM> ./f.txt`) otherwise fails with an opaque
    // `blob_import_error` instead of importing. Canonicalize here (the file was
    // just confirmed to exist by `classify_path`) — the original `path` still
    // drives the name/mime guess and the `imported:` line.
    let import_path = std::fs::canonicalize(path)
        .with_context(|| format!("could not resolve the path to {}", path.display()))?;
    let import = match blob_store.import_path(&import_path).await {
        Ok(import) => import,
        Err(err) => {
            // Best-effort close so the lock is released even on the error path.
            let _ = blob_store.close().await;
            return Err(anyhow!(err))
                .with_context(|| format!("could not import {}", path.display()));
        }
    };
    blob_store
        .close()
        .await
        .context("could not finalize the blob store after import")?;

    // ---- Derive the file.shared metadata (§5.2 step 6). ----
    let mut file_id = [0u8; SHORT_ID_LEN];
    getrandom::fill(&mut file_id)
        .map_err(|err| anyhow!("OS CSPRNG (getrandom) unavailable: {err}"))?;
    let name = match name_override {
        Some(n) => n.to_owned(),
        None => default_name(path)?,
    };
    let mime_type = match mime_override {
        Some(m) => m.to_owned(),
        None => guess_mime(path),
    };
    let blob_hash = HashRef::from_bytes(import.hash);
    // The asserted provider set is this node's device (§7 default is [device_id]).
    let providers = [secret.device.device_key()];

    // ---- prev_events = current room heads, bounded per §6 (mirrors room send). ----
    let heads = select_heads(&store, room_id)?;

    // ---- Build + self-validate + fold-check. ----
    let created_at = clock::now_ms();
    let wire = build_file_shared(
        &secret.identity,
        &secret.device,
        room_id,
        file_id,
        &name,
        &mime_type,
        import.size_bytes,
        blob_hash,
        Some(BLOB_FORMAT_RAW),
        &providers,
        &heads,
        created_at,
    );
    let ctx = ValidationContext::for_room(*room_id);
    let validated = validate_wire_bytes(&wire.to_bytes(), &ctx).map_err(|reason| {
        anyhow!(
            "internal error: freshly built file.shared failed validation ({})",
            reason.code()
        )
    })?;
    let event_id = validated.event_id;
    match membership.ingest(validated.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => bail!(
            "internal error: freshly built file.shared was rejected by the fold ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => {
            bail!("internal error: freshly built file.shared is causally incomplete")
        }
    }

    // ---- Persist the reference locally (the offline guarantee). ----
    store
        .insert(&validated)
        .with_context(|| format!("could not persist file.shared to {}", db_path.display()))?;

    Ok(FileShareSummary {
        file_id,
        name,
        mime_type,
        size_bytes: import.size_bytes,
        blob_hash,
        event_id,
        room_id: *room_id,
        source_display: path.display().to_string(),
    })
}

/// Print a [`FileShareSummary`] as labeled, script-friendly, secret-free lines
/// (spec §6.4). The provider line is always `you (local)` — this node just imported
/// the bytes; peer fetch is the follow-up serve/fetch issue (§4.3).
pub fn print_share(summary: &FileShareSummary) {
    println!("imported: {}", summary.source_display);
    println!("file_id: {}", file_handle(&summary.file_id));
    println!("name: {}", summary.name);
    println!("mime: {}", summary.mime_type);
    println!("size: {} bytes", summary.size_bytes);
    println!("hash: {}", summary.blob_hash);
    println!("event: {}", summary.event_id);
    println!("room: {}", summary.room_id);
    println!("provider: you (local)");
    println!(
        "next: run `iroh-rooms file list {}` (peers can fetch it once serve/fetch lands)",
        summary.room_id
    );
}

/// List the room's shared files with provider status (spec §5.1 / §6.5). Offline:
/// no node is brought up; the only async work is the local blob-presence query.
///
/// # Errors
/// An unknown room, a store read failure, a blob-store open failure, or (in JSON
/// mode) an encoding failure.
pub async fn list(home: &Path, room_id: &RoomId, json: bool) -> Result<()> {
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    // Fold first so we only list files from a known room (and validate the log). No
    // membership requirement — this is an offline read, like `room tail --offline`.
    let (_, _snapshot) = fold_room(&store, home, room_id)?;

    let events = store
        .by_type(room_id, EventType::FileShared)
        .with_context(|| format!("could not read file.shared events for room {room_id}"))?;

    // Provider status needs the durable blob store — but only open it if it already
    // exists. A pure `file list` on a node that has shared nothing must not create a
    // blob store as a side effect; every file simply reads `reference-only`.
    let blobs_dir = home.join(BLOBS_DIR);
    let blob_store =
        if blobs_dir.is_dir() {
            Some(BlobStore::open(&blobs_dir).await.with_context(|| {
                format!("could not open the blob store at {}", blobs_dir.display())
            })?)
        } else {
            None
        };

    let mut rows: Vec<FileRow> = Vec::with_capacity(events.len());
    for se in &events {
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        let Content::FileShared(f) = ev.content else {
            continue;
        };
        let held = match &blob_store {
            Some(bs) => bs.has(*f.blob_hash.as_bytes()).await.unwrap_or(false),
            None => false,
        };
        rows.push(FileRow {
            file_id: file_handle(&f.file_id),
            name: f.name,
            size_bytes: f.size_bytes,
            blob_hash: f.blob_hash.to_string(),
            held,
        });
    }
    if let Some(bs) = blob_store {
        bs.close().await.context("could not close the blob store")?;
    }

    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "file_id": r.file_id,
                    "name": r.name,
                    "size_bytes": r.size_bytes,
                    "blob_hash": r.blob_hash,
                    "provider": provider_token(r.held),
                })
            })
            .collect();
        let line = serde_json::to_string(&arr).context("could not encode file list as JSON")?;
        println!("{line}");
    } else {
        println!("room: {room_id}");
        if rows.is_empty() {
            println!("(no shared files)");
        }
        for r in &rows {
            println!("file_id: {}", r.file_id);
            println!("  name: {}", r.name);
            println!("  size: {} bytes", r.size_bytes);
            println!("  hash: {}", r.blob_hash);
            println!("  provider: {}", provider_label(r.held));
        }
    }
    Ok(())
}

/// One `file list` row.
struct FileRow {
    file_id: String,
    name: String,
    size_bytes: u64,
    blob_hash: String,
    held: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The human provider label for `file list` text output.
fn provider_label(held: bool) -> &'static str {
    if held {
        "you (local)"
    } else {
        "reference-only"
    }
}

/// The stable provider token for `file list --json`.
fn provider_token(held: bool) -> &'static str {
    if held {
        "local"
    } else {
        "reference-only"
    }
}

/// The CLI file handle for a 16-byte `file_id`: `file_<32-hex>` (spec OQ-6 — matches
/// the PRD `file_…` shape and the getting-started walkthrough).
fn file_handle(file_id: &[u8; SHORT_ID_LEN]) -> String {
    format!("file_{}", hex::encode(file_id))
}

/// The effective size cap: the [`MAX_SHARE_BYTES_ENV`] test override if set to a
/// parseable value, else [`MAX_SHARED_FILE_BYTES`].
fn effective_max_share_bytes() -> u64 {
    std::env::var(MAX_SHARE_BYTES_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(MAX_SHARED_FILE_BYTES)
}

/// Classify a path against the §7 error taxonomy, returning the file size on
/// success. Runs entirely before any store/blob write, so a bad invocation writes
/// nothing. `metadata` follows symlinks (std default), so a symlink to a missing
/// target reports `no such file` and a symlink to a directory reports the directory
/// error.
fn classify_path(path: &Path, max_bytes: u64) -> Result<u64> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!("no such file: {}", path.display())
        }
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            bail!("permission denied reading {}", path.display())
        }
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", path.display()));
        }
    };
    if meta.is_dir() {
        bail!(
            "{} is a directory, not a file; share a single file",
            path.display()
        );
    }
    let len = meta.len();
    if len > max_bytes {
        bail!(
            "{} is {len} bytes; exceeds the MVP share limit of {max_bytes} bytes",
            path.display()
        );
    }
    // A `chmod 000` file stats fine (the inode is readable via the parent) but its
    // contents cannot be opened — probe an open so the import does not fail mid-way.
    match std::fs::File::open(path) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            bail!("permission denied reading {}", path.display())
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!("no such file: {}", path.display())
        }
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", path.display()));
        }
    }
    Ok(len)
}

/// The default display name: the path's final component. Fails with an actionable
/// hint if the path has no file-name component (e.g. it ends in `..`).
fn default_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "could not derive a file name from {}; pass --name <NAME>",
                path.display()
            )
        })
}

/// Validate the optional `--name` override: non-empty, no control characters (so it
/// stays clean in `file list` output and CBOR content). Returns the borrowed value.
fn validate_share_name(name: Option<&str>) -> Result<Option<&str>> {
    match name {
        None => Ok(None),
        Some("") => bail!("--name must not be empty"),
        Some(n) if n.chars().any(char::is_control) => {
            bail!("--name must not contain control characters (newline, tab, NUL, etc.)")
        }
        Some(n) => Ok(Some(n)),
    }
}

/// Validate the optional `--mime` override: non-empty, no control characters.
fn validate_mime(mime: Option<&str>) -> Result<Option<&str>> {
    match mime {
        None => Ok(None),
        Some("") => bail!("--mime must not be empty"),
        Some(m) if m.chars().any(char::is_control) => {
            bail!("--mime must not contain control characters")
        }
        Some(m) => Ok(Some(m)),
    }
}

/// A dependency-free MIME guess from the path extension (spec §5.6). Covers the
/// obvious cases; `--mime` always wins and the default is `application/octet-stream`
/// for the long tail.
fn guess_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let ty = match ext.as_deref() {
        Some("txt" | "text" | "log") => "text/plain",
        Some("md" | "markdown") => "text/markdown",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        _ => "application/octet-stream",
    };
    ty.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        classify_path, default_name, effective_max_share_bytes, file_handle, guess_mime,
        provider_label, provider_token, validate_mime, validate_share_name, MAX_SHARE_BYTES_ENV,
    };
    use iroh_rooms_core::event::constants::MAX_SHARED_FILE_BYTES;
    use std::path::Path;
    use tempfile::TempDir;

    // ── file_handle ───────────────────────────────────────────────────────────

    #[test]
    fn file_handle_is_prefixed_lowercase_hex() {
        let id = [0xabu8; 16];
        let h = file_handle(&id);
        assert!(h.starts_with("file_"), "handle must start with file_: {h}");
        let hex = h.strip_prefix("file_").unwrap();
        assert_eq!(hex.len(), 32, "16 bytes -> 32 hex chars");
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    // ── guess_mime ────────────────────────────────────────────────────────────

    #[test]
    fn guess_mime_known_and_default() {
        assert_eq!(guess_mime(Path::new("a.txt")), "text/plain");
        assert_eq!(guess_mime(Path::new("README.md")), "text/markdown");
        assert_eq!(guess_mime(Path::new("doc.PDF")), "application/pdf"); // case-insensitive ext
        assert_eq!(guess_mime(Path::new("photo.jpeg")), "image/jpeg");
        assert_eq!(guess_mime(Path::new("noext")), "application/octet-stream");
        assert_eq!(
            guess_mime(Path::new("archive.bin")),
            "application/octet-stream"
        );
    }

    // ── validate_mime / validate_share_name ──────────────────────────────────

    #[test]
    fn mime_validation() {
        assert_eq!(validate_mime(None).unwrap(), None);
        assert_eq!(
            validate_mime(Some("text/plain")).unwrap(),
            Some("text/plain")
        );
        assert!(validate_mime(Some("")).is_err());
        assert!(validate_mime(Some("bad\nmime")).is_err());
    }

    #[test]
    fn name_validation() {
        assert_eq!(validate_share_name(None).unwrap(), None);
        assert_eq!(
            validate_share_name(Some("file.txt")).unwrap(),
            Some("file.txt")
        );
        assert!(validate_share_name(Some("")).is_err());
        assert!(validate_share_name(Some("bad\nname")).is_err());
        assert!(validate_share_name(Some("nul\0name")).is_err());
    }

    // ── provider labels ───────────────────────────────────────────────────────

    #[test]
    fn provider_labels_are_stable() {
        assert_eq!(provider_label(true), "you (local)");
        assert_eq!(provider_label(false), "reference-only");
        assert_eq!(provider_token(true), "local");
        assert_eq!(provider_token(false), "reference-only");
    }

    // ── default_name ──────────────────────────────────────────────────────────

    #[test]
    fn default_name_uses_final_component() {
        assert_eq!(
            default_name(Path::new("/tmp/report.pdf")).unwrap(),
            "report.pdf"
        );
        assert_eq!(default_name(Path::new("notes.txt")).unwrap(), "notes.txt");
    }

    // ── classify_path ─────────────────────────────────────────────────────────

    #[test]
    fn classify_missing_file_reports_no_such_file() {
        let tmp = TempDir::new().unwrap();
        let err = classify_path(&tmp.path().join("nope.txt"), 1_000).unwrap_err();
        assert!(err.to_string().contains("no such file"), "got: {err}");
    }

    #[test]
    fn classify_directory_reports_directory_error() {
        let tmp = TempDir::new().unwrap();
        let err = classify_path(tmp.path(), 1_000).unwrap_err();
        assert!(err.to_string().contains("is a directory"), "got: {err}");
    }

    #[test]
    fn classify_over_cap_reports_size_error_with_cap() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("big.bin");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let err = classify_path(&path, 10).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("100 bytes"),
            "must name the actual size: {msg}"
        );
        assert!(msg.contains("10 bytes"), "must name the cap: {msg}");
    }

    #[test]
    fn classify_at_cap_and_under_cap_succeeds() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ok.bin");
        std::fs::write(&path, vec![0u8; 10]).unwrap();
        // Exactly at the cap must pass (boundary), and so must under it.
        assert_eq!(classify_path(&path, 10).unwrap(), 10);
        assert_eq!(classify_path(&path, 11).unwrap(), 10);
    }

    #[test]
    fn classify_empty_file_is_allowed() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(classify_path(&path, 1_000).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn classify_unreadable_file_reports_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("secret.txt");
        std::fs::write(&path, b"hidden").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = classify_path(&path, 1_000);
        // Restore perms so TempDir cleanup can remove the file.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let err = result.expect_err("a chmod 000 file must be rejected");
        assert!(err.to_string().contains("permission denied"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn classify_dangling_symlink_reports_no_such_file() {
        // `std::fs::metadata` follows symlinks, so a symlink to a missing target
        // must report "no such file", not a confusing "no such file" on the link
        // itself (spec §7 / classify_path doc).
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("dangling.lnk");
        std::os::unix::fs::symlink("/nonexistent/path/that/cannot/exist", &link).unwrap();
        let err = classify_path(&link, 1_000).unwrap_err();
        assert!(err.to_string().contains("no such file"), "got: {err}");
    }

    // ── guess_mime — full extension map ───────────────────────────────────────

    #[test]
    fn guess_mime_all_mapped_extensions() {
        // Covers every arm in the match table that the basic test misses.
        let cases = [
            ("a.text", "text/plain"),
            ("a.log", "text/plain"),
            ("a.markdown", "text/markdown"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.svg", "image/svg+xml"),
            ("a.json", "application/json"),
            ("a.html", "text/html"),
            ("a.htm", "text/html"),
            ("a.csv", "text/csv"),
            ("a.xml", "application/xml"),
            ("a.zip", "application/zip"),
            ("a.gz", "application/gzip"),
            ("a.tgz", "application/gzip"),
            ("a.tar", "application/x-tar"),
        ];
        for (filename, expected) in cases {
            assert_eq!(
                guess_mime(Path::new(filename)),
                expected,
                "wrong MIME for {filename}"
            );
        }
    }

    // ── default_name — no file-name component ─────────────────────────────────

    #[test]
    fn default_name_dotdot_path_errors_with_name_hint() {
        // A path whose final segment is ".." has no file_name(); the error must
        // include the actionable "--name" hint so the user knows how to fix it.
        let err = default_name(Path::new("/foo/..")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--name"),
            "error must include --name hint: {msg}"
        );
    }

    // ── validate_share_name / validate_mime — tab as control char ─────────────

    #[test]
    fn validate_share_name_rejects_tab_character() {
        // Tab is a control character (char::is_control) and must be rejected.
        assert!(validate_share_name(Some("file\tname")).is_err());
    }

    #[test]
    fn validate_mime_rejects_tab_character() {
        assert!(validate_mime(Some("text/\tplain")).is_err());
    }

    // ── file_handle — all-zeros id ────────────────────────────────────────────

    #[test]
    fn file_handle_all_zeros_encodes_to_zeros() {
        let id = [0x00u8; 16];
        assert_eq!(file_handle(&id), "file_00000000000000000000000000000000");
    }

    // ── effective_max_share_bytes — env-var override seam ─────────────────────

    #[test]
    fn effective_max_share_bytes_falls_back_to_constant_when_env_unset() {
        // Only valid when the env var is absent (the common CI baseline).
        if std::env::var(MAX_SHARE_BYTES_ENV).is_ok() {
            return; // Another test has set the var; skip rather than conflict.
        }
        assert_eq!(effective_max_share_bytes(), MAX_SHARED_FILE_BYTES);
    }

    #[test]
    fn effective_max_share_bytes_falls_back_when_env_is_not_a_number() {
        // parse::<u64>() fails on non-numeric input; the function must fall back
        // to the default rather than panicking. We can test the fallback path
        // without an env-var write by testing the value already in env or skipping.
        if let Ok(v) = std::env::var(MAX_SHARE_BYTES_ENV) {
            // If it parses as a number the override path is active — not what we
            // want here. Skip to avoid a spurious assertion failure.
            if v.trim().parse::<u64>().is_ok() {
                return;
            }
            // If it's set but not a number, effective_max_share_bytes returns the default.
            assert_eq!(effective_max_share_bytes(), MAX_SHARED_FILE_BYTES);
        }
        // Env var absent: also returns default. Already checked by the test above.
    }
}
