//! The Blob Plane CLI: `iroh-rooms file share | list | fetch` (spec IR-0202 §5,
//! IR-0204 §5). A thin orchestrator over the landed primitives, the sibling of
//! [`crate::message`] and [`crate::pipe`]:
//!
//! * `share` is the **producer/import** path: it content-addresses a local file
//!   into the durable local blob store ([`iroh_rooms_net::BlobStore`]), records
//!   this node as a local provider, then authors + self-validates + persists a
//!   signed `file.shared` **reference** onto the local log via the pure core
//!   builder ([`build_file_shared`](iroh_rooms_core::event::build_file_shared)).
//!   It is deliberately **offline**: the blob bytes are never carried on the log
//!   (PRD §9.2), and the event propagates through the already-landed sync engine
//!   unchanged once peers reconcile. No network is contacted.
//! * `list` is an **offline** read: for the room, it decodes every `file.shared`
//!   event and reports each file's handle, name, size, hash, and **provider
//!   status** — whether *this* node holds the blob (`you (local)`) or only the
//!   reference (`reference-only`).
//! * `fetch` is the **consumer** path (IR-0204): resolve the `file.shared`
//!   reference (syncing it if absent), discover a provider from the reference's
//!   metadata, dial it as an authorized member over the ACL-gated blobs ALPN,
//!   transfer the blob, independently verify BLAKE3-256 against the declared
//!   hash, save the verified bytes, and print the saved path + verified hash. An
//!   unauthorized peer is denied at the provider's room-ACL path (`net::blob`'s
//!   two-gate serve, wired in via `room tail`'s [`iroh_rooms_net::BlobServeConfig`]);
//!   an unavailable provider is reported honestly within a bounded timeout.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use iroh::{EndpointAddr, EndpointId, SecretKey};
// The offline authoring half of `share` goes through the SDK façade (spec
// IR-0301 §5.4); the online engine/transport imports below stay direct
// `core`/`net` deps (the optional online-path migration).
use iroh_rooms::files::build_file_shared;
use iroh_rooms_core::event::constants::{MAX_SHARED_FILE_BYTES, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{Content, EventType, FileShared};
use iroh_rooms_core::event::ids::{HashRef, RoomId};
use iroh_rooms_core::event::keys::DeviceKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::Ingest;
use iroh_rooms_core::store::{EventStore, StoredEvent};
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    BlobError, BlobStore, FetchOutcome, NetConfig, Node, TracingAudit, DEFAULT_TICK,
};
use serde_json::json;

use crate::error::{CodedResultExt, ErrorCode};
use crate::message::{
    build_admission, build_dial_set, endpoint_id_of, fold_room, net_mode, parse_peers,
    select_heads, DB_FILE,
};
use crate::{clock, identity, paths};

/// Directory (under the data-directory home) rooting the durable blob store (spec
/// §4.1 / §5.5). Lives inside the `0700` home; `file share` tightens it to `0700`.
/// `pub(crate)` so `message::tail` can pass the same path to
/// [`iroh_rooms_net::BlobServeConfig`] (IR-0204 spec §6.6).
pub(crate) const BLOBS_DIR: &str = "blobs";
/// The only `blob_format` for MVP (spec §3.3 / §5.2 step 6); `hash_seq` is a
/// follow-up (spike NOTES.md §6).
const BLOB_FORMAT_RAW: &str = "raw";
/// Test-only seam (spec OQ-4): overrides [`MAX_SHARED_FILE_BYTES`] so the too-large
/// boundary can be exercised without a 100 MiB fixture. Not a user-facing knob — it
/// is intentionally absent from `--help` and the size error still names the
/// effective cap. Ignored if unset/unparseable.
const MAX_SHARE_BYTES_ENV: &str = "IROH_ROOMS_MAX_SHARE_BYTES";
/// Default per-provider connect+transfer timeout for `file fetch` (spec §5.1 /
/// OQ-4): larger than the 5s message/pipe default because a transfer can be up to
/// the 100 MiB [`MAX_SHARED_FILE_BYTES`] cap.
pub const DEFAULT_FETCH_TIMEOUT: &str = "30s";
/// How long `file fetch` waits for an absent `file.shared` reference to sync
/// before giving up (mirrors `pipe connect`'s `SYNC_WAIT`).
const SYNC_WAIT: Duration = Duration::from_secs(10);
/// Env var overriding the downloads directory `file fetch` saves to when `--out`
/// is omitted (spec §5.6).
const DOWNLOADS_ENV: &str = "IROH_ROOMS_DOWNLOADS";
/// Downloads directory name under `<home>/` when [`DOWNLOADS_ENV`] is unset.
const DOWNLOADS_DIR: &str = "downloads";

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
        crate::bail_coded!(
            crate::error::ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
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
    let validated = validate_wire_bytes(&wire.to_bytes(), &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built file.shared failed validation ({})",
                reason.code()
            )
        })
        .coded(crate::error::ErrorCode::Internal)?;
    let event_id = validated.event_id;
    match membership.ingest(validated.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => crate::bail_coded!(
            crate::error::ErrorCode::Internal,
            "internal error: freshly built file.shared was rejected by the fold ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => {
            crate::bail_coded!(
                crate::error::ErrorCode::Internal,
                "internal error: freshly built file.shared is causally incomplete"
            )
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
/// (spec §6.4). The provider line is always `you (local)` — this node just
/// imported the bytes; a peer fetches them once this node runs `room tail`
/// (IR-0204 §5.3, the "provider stays online" surface).
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
        "next: run `iroh-rooms room tail {}` to serve it, then peers can \
         `iroh-rooms file fetch {} {}`",
        summary.room_id,
        summary.room_id,
        file_handle(&summary.file_id)
    );
}

/// List the room's shared files with provider status (spec §5.1 / §6.5). Offline:
/// no node is brought up; the only async work is the local blob-presence query.
///
/// If a concurrent `room tail` provider holds the durable blob store's exclusive
/// lock on this home, provider status cannot be read; rather than deadlock, this
/// warns on stderr and reports each file's provider as `unknown` (spec §6.6 — the
/// provider-stays-online surface must coexist with listing).
///
/// # Errors
/// An unknown room, a store read failure, a blob-store open failure other than a
/// held lock, or (in JSON mode) an encoding failure.
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
    //
    // The store's on-disk lock is exclusive, so a concurrent `room tail` provider on
    // this home holds it open. Rather than block forever waiting for that lock,
    // `BlobStore::open` bounds the wait and returns `Locked`; degrade to `unknown`
    // provider status with a stderr warning so the listing still completes.
    let blobs_dir = home.join(BLOBS_DIR);
    let (blob_store, store_locked) = if blobs_dir.is_dir() {
        match BlobStore::open(&blobs_dir).await {
            Ok(bs) => (Some(bs), false),
            Err(BlobError::Locked(_)) => {
                eprintln!(
                    "warning: the blob store at {} is in use by another process (is \
                     `room tail` running on this home?); provider status is reported as \
                     unknown for this listing",
                    blobs_dir.display()
                );
                (None, true)
            }
            Err(e) => {
                return Err(anyhow!(e)).with_context(|| {
                    format!("could not open the blob store at {}", blobs_dir.display())
                });
            }
        }
    } else {
        (None, false)
    };

    let mut rows: Vec<FileRow> = Vec::with_capacity(events.len());
    for se in &events {
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        let Content::FileShared(f) = ev.content else {
            continue;
        };
        // `Some(true/false)` = definitively held / reference-only; `None` = unknown
        // because the store lock is held elsewhere (`store_locked`).
        let held = match &blob_store {
            Some(bs) => Some(bs.has(*f.blob_hash.as_bytes()).await.unwrap_or(false)),
            None if store_locked => None,
            None => Some(false),
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

/// One `file list` row. `held` is `Some(true)` when this node holds the blob,
/// `Some(false)` when it holds only the reference, and `None` when provider status
/// could not be determined (the store lock is held by a concurrent `room tail`).
struct FileRow {
    file_id: String,
    name: String,
    size_bytes: u64,
    blob_hash: String,
    held: Option<bool>,
}

// ---------------------------------------------------------------------------
// fetch (IR-0204 §5.2)
// ---------------------------------------------------------------------------

/// The result of a successful `file fetch`, for the caller to present.
pub struct FetchSummary {
    /// Where the verified bytes were saved.
    pub saved_path: PathBuf,
    /// The verified BLAKE3-256 hash (equal to the declared `blob_hash`).
    pub verified_hash: HashRef,
    /// The fetched content's size in bytes.
    pub size_bytes: u64,
    /// The provider the blob was fetched from.
    pub provider: EndpointId,
}

/// Tally of per-provider fetch outcomes across the loop below, used to classify
/// the terminal failure honestly when no provider served the bytes (spec IR-0205
/// §5.2 — the unauthorized-vs-unavailable split). Not a trust input; purely for
/// reporting which coded terminal state to render.
#[derive(Default)]
struct FetchTally {
    /// Provider reachable but refused the connection (an authorization wall).
    denied_at_connect: usize,
    /// Provider reachable, active, but not serving this hash (an availability gap).
    denied_per_hash: usize,
    /// Provider offline / timed out (an availability gap).
    unreachable: usize,
    /// Total non-fetch, non-hash-mismatch attempts tallied above.
    attempted: usize,
}

/// The honest terminal classification when no provider served the bytes (spec
/// IR-0205 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchFailure {
    /// Every reachable provider refused the connection — an authorization wall,
    /// not an availability gap.
    Unauthorized,
    /// At least one provider was unreachable or reachable-but-not-serving, and
    /// none authorized+served — the honest MVP-limitation state.
    Unavailable,
}

impl FetchTally {
    /// `Unauthorized` iff every attempted provider was `DeniedAtConnect` — the one
    /// outcome that is a pure authorization signal. Any availability gap in the mix
    /// (an unreachable or per-hash-denying provider) makes the honest headline
    /// "unavailable", since a holder may still come online later. An empty tally
    /// (no providers attempted) is also `Unavailable`.
    #[must_use]
    fn classify(&self) -> FetchFailure {
        if self.attempted > 0 && self.denied_at_connect == self.attempted {
            FetchFailure::Unauthorized
        } else {
            FetchFailure::Unavailable
        }
    }
}

/// Fetch a shared file from an available provider, verify its content hash, and
/// save it locally (spec §5.2): resolve the `file.shared` reference (syncing it
/// if absent), discover providers from its metadata, dial each in order over the
/// ACL-gated blobs ALPN, and require the assembled bytes' independent BLAKE3-256
/// equal the declared hash before saving.
///
/// # Errors
/// Fails — writing nothing to disk on every path — if the file id / `--out` is
/// invalid (pre-IO), no local identity exists, the room is unknown, the caller is
/// not an active member, the reference cannot be found (locally or after a bounded
/// sync wait), its `blob_format` is not `raw`/absent, every provider denies or is
/// unreachable, the assembled bytes fail the independent hash check (a hard stop —
/// never falls through to another provider), or the verified bytes cannot be saved.
#[allow(clippy::too_many_lines)] // one linear resolve-then-fetch-then-save flow; splitting hurts readability
pub async fn fetch(
    home: &Path,
    room_id: &RoomId,
    file_id_str: &str,
    out: Option<&str>,
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<FetchSummary> {
    // ---- Pre-IO argument validation (a bad invocation writes nothing). ----
    let file_id = parse_file_id(file_id_str).coded(ErrorCode::InvalidArgument)?;
    let peer_addrs = parse_peers(peers)?;

    // ---- Identity + membership (mirrors `pipe connect` / `room send`). ----
    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            ErrorCode::Reject(iroh_rooms_core::event::RejectReason::NotAMember),
            "you are not an active member of room {room_id}; only an active member can fetch \
             files (this identity is {self_id})"
        );
    }

    // ---- Resolve the file.shared reference locally first. ----
    let local_events = store
        .by_type(room_id, EventType::FileShared)
        .with_context(|| format!("could not read file.shared events for room {room_id}"))?;
    let mut file_ref = file_shared_in(&local_events, file_id);

    // ---- Bring up an ephemeral consumer node: dial active members so a missing
    // reference can sync and so providers see us as a connected member. ----
    let self_device = endpoint_id_of(secret.device.device_key())?;
    let admission = build_admission(&snapshot);
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);
    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        std::sync::Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;
    for addr in &dial_set {
        node.connect_to(addr.clone());
    }

    if file_ref.is_none() {
        file_ref = wait_for_file_shared(&node, file_id, SYNC_WAIT).await;
    }

    let Some((shared, author_device)) = file_ref else {
        let _ = node.shutdown().await;
        crate::bail_coded!(
            ErrorCode::NoSuchFile,
            "no such file {file_id_str} in room {room_id}"
        );
    };

    // ---- Format gate: only `raw`/absent is fetchable (spec §3.3). ----
    if let Some(format) = shared.blob_format.as_deref() {
        if format != BLOB_FORMAT_RAW {
            let _ = node.shutdown().await;
            crate::bail_coded!(
                ErrorCode::InvalidArgument,
                "file {file_id_str} uses blob_format={format}, which this version cannot fetch \
                 (raw only)"
            );
        }
    }

    // ---- Discover providers (spec §5.5): self is skipped. ----
    let providers = resolve_providers(&shared, author_device, self_device, &peer_addrs);
    if providers.is_empty() {
        let _ = node.shutdown().await;
        crate::bail_coded!(
            ErrorCode::BlobUnavailable,
            "file {file_id_str} is currently unavailable: no peer holding it is online. There \
             is no central inbox and no guaranteed offline delivery"
        );
    }

    // ---- Sequential per-provider fetch loop (spec §5.2 step 7). ----
    let declared = *shared.blob_hash.as_bytes();
    let mut fetched = None;
    let mut hash_mismatch: Option<String> = None;
    let mut tally = FetchTally::default();
    for provider_addr in &providers {
        let (outcome, data) = node
            .fetch_file(provider_addr.clone(), declared, declared, timeout)
            .await;
        match outcome {
            FetchOutcome::Fetched => {
                fetched = data.map(|b| (b, provider_addr.id));
                break;
            }
            FetchOutcome::DeniedAtConnect => {
                tally.denied_at_connect += 1;
                tally.attempted += 1;
                eprintln!(
                    "provider {} denied the connection (are you an active member?)",
                    short_endpoint(provider_addr.id)
                );
            }
            FetchOutcome::DeniedPerHash => {
                tally.denied_per_hash += 1;
                tally.attempted += 1;
                eprintln!(
                    "provider {} will not serve this hash",
                    short_endpoint(provider_addr.id)
                );
            }
            FetchOutcome::HashMismatch => {
                hash_mismatch = Some(data.map_or_else(
                    || "<unknown>".to_owned(),
                    |b| hex::encode(blake3::hash(&b).as_bytes()),
                ));
                break;
            }
            FetchOutcome::Unavailable => {
                tally.unreachable += 1;
                tally.attempted += 1;
                eprintln!("provider {} unreachable", short_endpoint(provider_addr.id));
            }
        }
    }

    if let Some(got) = hash_mismatch {
        let _ = node.shutdown().await;
        crate::bail_coded!(
            ErrorCode::HashMismatch,
            "integrity check FAILED: fetched bytes hash blake3:{got} but the reference declares \
             {}; refusing to save (the file reference or a provider may be corrupt — do not \
             trust this file)",
            shared.blob_hash
        );
    }

    let Some((data, provider_id)) = fetched else {
        let _ = node.shutdown().await;
        match tally.classify() {
            FetchFailure::Unauthorized => crate::bail_coded!(
                ErrorCode::PeerUnauthorized,
                "file {file_id_str} could not be fetched: every provider refused the \
                 connection — this identity ({self_id}) is not an active member from their view"
            ),
            FetchFailure::Unavailable => crate::bail_coded!(
                ErrorCode::BlobUnavailable,
                "file {file_id_str} is currently unavailable: no peer holding it is online. \
                 There is no central inbox and no guaranteed offline delivery"
            ),
        }
    };

    node.shutdown()
        .await
        .context("could not shut down cleanly")?;

    // ---- Save: sanitized name, atomic temp-then-rename (spec §5.6). ----
    let target = resolve_output_path(home, out, &shared.name, file_id)?;
    save_atomic(&target, &data)?;

    // ---- Recommended: become a provider too (spec §5.7 / OQ-5), best-effort. ----
    reprovide_best_effort(home, &target).await;

    let size_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    Ok(FetchSummary {
        saved_path: target,
        verified_hash: shared.blob_hash,
        size_bytes,
        provider: provider_id,
    })
}

/// Print a [`FetchSummary`] as labeled, script-friendly, secret-free lines (spec
/// §7 AC4): stdout stays clean for scripting; diagnostics go to stderr.
pub fn print_fetch(summary: &FetchSummary) {
    println!("saved: {}", summary.saved_path.display());
    println!("verified: {}", summary.verified_hash);
    println!("size: {} bytes", summary.size_bytes);
    println!("provider: {}", short_endpoint(summary.provider));
}

/// Parse a `file_<32-hex>` handle (or, tolerantly, bare 32-hex) into 16 bytes —
/// the inverse of `file_handle` (spec §5.1 / OQ-8). `pub(crate)` so other nouns
/// that accept a file-id handle (`agent status --artifact`) reuse this codec
/// verbatim rather than re-implementing it (spec IR-0208 D6).
pub(crate) fn parse_file_id(s: &str) -> Result<[u8; SHORT_ID_LEN]> {
    let trimmed = s.trim();
    let hex_part = trimmed.strip_prefix("file_").unwrap_or(trimmed);
    if hex_part.len() != 32 || !hex_part.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid file id {s:?} (expected file_<32-hex> or 32 hex chars)");
    }
    let mut out = [0u8; SHORT_ID_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_part[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow!("invalid file id {s:?}"))?;
    }
    Ok(out)
}

/// Find the `file.shared` matching `file_id` in `events`, returning its content
/// plus the author's signing device (`file.shared.providers`' implicit default).
fn file_shared_in(
    events: &[StoredEvent],
    file_id: [u8; SHORT_ID_LEN],
) -> Option<(FileShared, DeviceKey)> {
    for se in events {
        if se.event_type != EventType::FileShared {
            continue;
        }
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        let Content::FileShared(f) = ev.content else {
            continue;
        };
        if f.file_id == file_id {
            return Some((f, ev.device_id));
        }
    }
    None
}

/// Wait (bounded) for `file_id`'s `file.shared` to sync into `node`'s validated
/// set, polling its timeline (mirrors `pipe connect`'s `pipe.opened` wait).
async fn wait_for_file_shared(
    node: &Node,
    file_id: [u8; SHORT_ID_LEN],
    timeout: Duration,
) -> Option<(FileShared, DeviceKey)> {
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(events) = node.room_tail(u32::MAX).await {
                if let Some(found) = file_shared_in(&events, file_id) {
                    return found;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .ok()
}

/// Resolve `file.shared.providers` (default `[author device]`) into dialable
/// addresses, in as-listed order, skipping `self_device` (spec §5.5 — fetching
/// from yourself is a no-op).
fn resolve_providers(
    shared: &FileShared,
    author_device: DeviceKey,
    self_device: EndpointId,
    peer_addrs: &[EndpointAddr],
) -> Vec<EndpointAddr> {
    let devices: Vec<DeviceKey> = match &shared.providers {
        Some(list) if !list.is_empty() => list.clone(),
        _ => vec![author_device],
    };
    devices
        .into_iter()
        .filter_map(|dev| EndpointId::from_bytes(dev.as_bytes()).ok())
        .filter(|id| *id != self_device)
        .map(|id| {
            peer_addrs
                .iter()
                .find(|a| a.id == id)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(id))
        })
        .collect()
}

/// Resolve the save target for a fetched file (spec §5.6): `--out <FILE>` (exact
/// path, refused if it already exists), `--out <DIR>` (existing dir or a value
/// ending in a path separator; joined with the sanitized name), or the configured
/// downloads directory when `--out` is omitted.
fn resolve_output_path(
    home: &Path,
    out: Option<&str>,
    name: &str,
    file_id: [u8; SHORT_ID_LEN],
) -> Result<PathBuf> {
    let safe_name = sanitize_name(name, file_id);
    let Some(spec) = out else {
        let downloads = downloads_dir(home);
        paths::ensure_dir(&downloads)?;
        return Ok(downloads.join(safe_name));
    };

    let path = Path::new(spec);
    let is_dir_target =
        path.is_dir() || spec.ends_with('/') || spec.ends_with(std::path::MAIN_SEPARATOR);
    if is_dir_target {
        std::fs::create_dir_all(path)
            .with_context(|| format!("could not create directory {}", path.display()))?;
        return Ok(path.join(safe_name));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create directory {}", parent.display()))?;
        }
    }
    if path.exists() {
        bail!(
            "refusing to overwrite existing file {} (pass a different --out)",
            path.display()
        );
    }
    Ok(path.to_path_buf())
}

/// The configured downloads directory: [`DOWNLOADS_ENV`] if set to a non-empty
/// value, else `<home>/downloads/`.
fn downloads_dir(home: &Path) -> PathBuf {
    std::env::var_os(DOWNLOADS_ENV)
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(DOWNLOADS_DIR), PathBuf::from)
}

/// Reduce a peer-supplied `file.shared.name` to a single safe basename (spec
/// §5.6 — a path-traversal guard): keep only the final path component, strip
/// control characters, and fall back to `file_<hex>` if the result would be
/// empty, `.`, or `..`. Prevents a malicious `name` like
/// `../../.ssh/authorized_keys` from escaping the target directory.
fn sanitize_name(name: &str, file_id: [u8; SHORT_ID_LEN]) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        format!("file_{}", hex::encode(file_id))
    } else {
        cleaned.to_owned()
    }
}

/// Write `bytes` to `target` atomically: a temp file in the same directory, then
/// rename (spec §5.2 step 8) — no partial/corrupt file is ever visible at
/// `target`, and the temp file is removed on any failure.
fn save_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let tmp = dir.join(format!(".{file_name}.part"));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, target));
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("could not save to {}", target.display()));
    }
    Ok(())
}

/// After a verified fetch, best-effort import the saved bytes into the local
/// `<home>/blobs/` store so this node becomes a provider too (spec §5.7 / OQ-5).
/// Never fails the fetch — the file is already safely saved at `target`
/// regardless of whether the re-provide import succeeds.
async fn reprovide_best_effort(home: &Path, target: &Path) {
    let blobs_dir = home.join(BLOBS_DIR);
    if paths::ensure_dir(&blobs_dir).is_err() {
        return;
    }
    let Ok(store) = BlobStore::open(&blobs_dir).await else {
        return;
    };
    if let Ok(abs) = std::fs::canonicalize(target) {
        let _ = store.import_path(&abs).await;
    }
    let _ = store.close().await;
}

/// A short, human-scannable prefix of an endpoint id for a diagnostic line.
fn short_endpoint(device: EndpointId) -> String {
    device.to_string().chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The human provider label for `file list` text output. `None` (store lock held
/// elsewhere) reads `unknown (store in use)`.
fn provider_label(held: Option<bool>) -> &'static str {
    match held {
        Some(true) => "you (local)",
        Some(false) => "reference-only",
        None => "unknown (store in use)",
    }
}

/// The stable provider token for `file list --json`. `None` (store lock held
/// elsewhere) reads `unknown`.
fn provider_token(held: Option<bool>) -> &'static str {
    match held {
        Some(true) => "local",
        Some(false) => "reference-only",
        None => "unknown",
    }
}

/// The CLI file handle for a 16-byte `file_id`: `file_<32-hex>` (spec OQ-6 — matches
/// the PRD `file_…` shape and the getting-started walkthrough). `pub(crate)` so
/// `agent.status` display (`room tail`) round-trips artifact ids through the same
/// handle form (spec IR-0208 OQ3).
pub(crate) fn file_handle(file_id: &[u8; SHORT_ID_LEN]) -> String {
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
    use crate::error::ErrorCode;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            crate::bail_coded!(ErrorCode::NoSuchFile, "no such file: {}", path.display())
        }
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            crate::bail_coded!(
                ErrorCode::PermissionDenied,
                "permission denied reading {}",
                path.display()
            )
        }
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", path.display()));
        }
    };
    if meta.is_dir() {
        // Folded under `invalid_argument` to keep the code set minimal (OQ-4); a
        // dedicated `not_a_file` code can split out later if a script needs it.
        crate::bail_coded!(
            ErrorCode::InvalidArgument,
            "{} is a directory, not a file; share a single file",
            path.display()
        );
    }
    let len = meta.len();
    if len > max_bytes {
        crate::bail_coded!(
            ErrorCode::FileTooLarge,
            "{} is {len} bytes; exceeds the MVP share limit of {max_bytes} bytes",
            path.display()
        );
    }
    // A `chmod 000` file stats fine (the inode is readable via the parent) but its
    // contents cannot be opened — probe an open so the import does not fail mid-way.
    match std::fs::File::open(path) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            crate::bail_coded!(
                ErrorCode::PermissionDenied,
                "permission denied reading {}",
                path.display()
            )
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            crate::bail_coded!(ErrorCode::NoSuchFile, "no such file: {}", path.display())
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
        classify_path, default_name, downloads_dir, effective_max_share_bytes, file_handle,
        guess_mime, parse_file_id, provider_label, provider_token, resolve_output_path,
        resolve_providers, sanitize_name, save_atomic, short_endpoint, validate_mime,
        validate_share_name, FetchFailure, FetchTally, DOWNLOADS_ENV, MAX_SHARE_BYTES_ENV,
    };
    use iroh::{EndpointAddr, EndpointId, SecretKey};
    use iroh_rooms_core::event::constants::MAX_SHARED_FILE_BYTES;
    use iroh_rooms_core::event::content::FileShared;
    use iroh_rooms_core::event::ids::HashRef;
    use iroh_rooms_core::event::keys::{DeviceKey, SigningKey};
    use std::net::SocketAddr;
    use std::path::Path;
    use tempfile::TempDir;

    // ── fetch fixtures ────────────────────────────────────────────────────────

    /// A valid device signing key from a one-byte seed (a real Ed25519 point, so
    /// `EndpointId::from_bytes` on its bytes succeeds — `resolve_providers` relies
    /// on that conversion).
    fn device(seed: u8) -> DeviceKey {
        SigningKey::from_seed(&[seed; 32]).device_key()
    }

    /// The `EndpointId` a `DeviceKey` resolves to (the identity resolution
    /// `resolve_providers` performs internally).
    fn endpoint_of(dev: DeviceKey) -> EndpointId {
        EndpointId::from_bytes(dev.as_bytes()).expect("device key is a valid point")
    }

    /// A minimal `file.shared` reference with the given (optional) provider list.
    fn file_shared_with(providers: Option<Vec<DeviceKey>>) -> FileShared {
        FileShared {
            file_id: [0x11; 16],
            name: "report.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 3,
            blob_hash: HashRef::from_bytes([0xAB; 32]),
            blob_format: Some("raw".to_owned()),
            providers,
        }
    }

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
        assert_eq!(provider_label(Some(true)), "you (local)");
        assert_eq!(provider_label(Some(false)), "reference-only");
        assert_eq!(provider_label(None), "unknown (store in use)");
        assert_eq!(provider_token(Some(true)), "local");
        assert_eq!(provider_token(Some(false)), "reference-only");
        assert_eq!(provider_token(None), "unknown");
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

    // ── parse_file_id (spec §5.1 / OQ-8) ──────────────────────────────────────

    #[test]
    fn parse_file_id_roundtrips_with_file_handle() {
        // The CLI prints `file_<hex>`; `file fetch` must accept exactly that form.
        let id = [0x3cu8; 16];
        let handle = file_handle(&id);
        assert_eq!(parse_file_id(&handle).unwrap(), id);
    }

    #[test]
    fn parse_file_id_accepts_bare_hex_and_trims_whitespace() {
        let id = [0xabu8; 16];
        let bare = hex::encode(id);
        assert_eq!(
            parse_file_id(&bare).unwrap(),
            id,
            "bare 32-hex is tolerated"
        );
        assert_eq!(
            parse_file_id(&format!("  {bare}\n")).unwrap(),
            id,
            "leading/trailing whitespace is trimmed"
        );
    }

    #[test]
    fn parse_file_id_accepts_uppercase_hex() {
        // is_ascii_hexdigit + from_str_radix(.,16) both accept uppercase; a user
        // pasting an upper-cased handle must still resolve to the same 16 bytes.
        let id = [0xabu8; 16];
        let upper = hex::encode_upper(id);
        assert_eq!(parse_file_id(&format!("file_{upper}")).unwrap(), id);
    }

    #[test]
    fn parse_file_id_rejects_malformed_input() {
        assert!(parse_file_id("").is_err(), "empty is invalid");
        assert!(
            parse_file_id("file_").is_err(),
            "prefix with no hex is invalid"
        );
        assert!(
            parse_file_id(&"a".repeat(31)).is_err(),
            "31 hex chars is the wrong length"
        );
        assert!(
            parse_file_id(&"a".repeat(33)).is_err(),
            "33 hex chars is the wrong length"
        );
        assert!(
            parse_file_id(&"g".repeat(32)).is_err(),
            "32 non-hex chars is invalid"
        );
    }

    // ── sanitize_name (spec §5.6 / R5 — the path-traversal guard) ─────────────

    #[test]
    fn sanitize_name_keeps_a_plain_basename() {
        assert_eq!(sanitize_name("report.pdf", [0u8; 16]), "report.pdf");
        assert_eq!(sanitize_name("  spaced.txt  ", [0u8; 16]), "spaced.txt");
    }

    #[test]
    fn sanitize_name_reduces_forward_slash_traversal_to_the_basename() {
        // A malicious `file.shared.name` must never escape the target directory.
        assert_eq!(sanitize_name("../../etc/passwd", [0u8; 16]), "passwd");
        assert_eq!(sanitize_name("/abs/path/to/x", [0u8; 16]), "x");
    }

    #[test]
    fn sanitize_name_reduces_backslash_traversal_to_the_basename() {
        assert_eq!(sanitize_name("..\\..\\secret.txt", [0u8; 16]), "secret.txt");
    }

    #[test]
    fn sanitize_name_strips_control_characters() {
        // NUL / newline / tab are control chars and must be stripped from the name
        // before it reaches the filesystem.
        assert_eq!(sanitize_name("na\nme\t.txt", [0u8; 16]), "name.txt");
    }

    #[test]
    fn sanitize_name_falls_back_to_file_hex_when_unsafe_or_empty() {
        let id = [0xCDu8; 16];
        let fallback = format!("file_{}", hex::encode(id));
        assert_eq!(sanitize_name("", id), fallback, "empty name");
        assert_eq!(sanitize_name(".", id), fallback, "current-dir name");
        assert_eq!(sanitize_name("..", id), fallback, "parent-dir name");
        assert_eq!(sanitize_name("dir/", id), fallback, "trailing separator");
        assert_eq!(sanitize_name("\0", id), fallback, "control-only name");
    }

    // ── resolve_output_path (spec §5.6) ───────────────────────────────────────

    #[test]
    fn resolve_output_path_explicit_file_uses_the_exact_path_and_creates_parents() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("nested").join("out.bin");
        let got = resolve_output_path(
            tmp.path(),
            Some(target.to_str().unwrap()),
            "ignored",
            [0u8; 16],
        )
        .unwrap();
        assert_eq!(got, target, "an explicit --out file is used verbatim");
        assert!(
            target.parent().unwrap().is_dir(),
            "missing parent dirs are created"
        );
    }

    #[test]
    fn resolve_output_path_refuses_to_overwrite_an_existing_file() {
        let tmp = TempDir::new().unwrap();
        let existing = tmp.path().join("keep.bin");
        std::fs::write(&existing, b"do not clobber").unwrap();
        let err = resolve_output_path(tmp.path(), Some(existing.to_str().unwrap()), "n", [0u8; 16])
            .unwrap_err();
        assert!(
            err.to_string().contains("refusing to overwrite"),
            "must not clobber an existing --out file: {err}"
        );
    }

    #[test]
    fn resolve_output_path_existing_dir_joins_the_sanitized_name() {
        let tmp = TempDir::new().unwrap();
        let got = resolve_output_path(
            tmp.path(),
            Some(tmp.path().to_str().unwrap()),
            "doc.txt",
            [0u8; 16],
        )
        .unwrap();
        assert_eq!(got, tmp.path().join("doc.txt"));
    }

    #[test]
    fn resolve_output_path_trailing_separator_is_treated_as_a_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("downloads");
        let spec = format!("{}/", dir.display());
        let got = resolve_output_path(tmp.path(), Some(&spec), "doc.txt", [0u8; 16]).unwrap();
        assert_eq!(got, dir.join("doc.txt"));
        assert!(
            dir.is_dir(),
            "a value ending in a separator creates the dir"
        );
    }

    #[test]
    fn resolve_output_path_dir_target_cannot_be_escaped_by_a_traversal_name() {
        // The security property AC/R5 protects: even a `../../../etc/passwd` name
        // resolves to a single component inside the requested directory.
        let tmp = TempDir::new().unwrap();
        let got = resolve_output_path(
            tmp.path(),
            Some(tmp.path().to_str().unwrap()),
            "../../../etc/passwd",
            [0u8; 16],
        )
        .unwrap();
        assert_eq!(got, tmp.path().join("passwd"));
        assert_eq!(
            got.parent().unwrap(),
            tmp.path(),
            "the resolved path must stay inside the target directory"
        );
    }

    #[test]
    fn resolve_output_path_omitted_uses_the_downloads_dir() {
        // Only meaningful when the downloads override env is unset (CI baseline).
        if std::env::var_os(DOWNLOADS_ENV).is_some() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let got = resolve_output_path(tmp.path(), None, "doc.txt", [0xABu8; 16]).unwrap();
        assert_eq!(got, tmp.path().join("downloads").join("doc.txt"));
        assert!(
            tmp.path().join("downloads").is_dir(),
            "the downloads dir is created on demand"
        );
    }

    #[test]
    fn downloads_dir_defaults_under_home_when_env_unset() {
        if std::env::var_os(DOWNLOADS_ENV).is_some() {
            return;
        }
        let home = Path::new("/some/home");
        assert_eq!(downloads_dir(home), home.join("downloads"));
    }

    // ── resolve_providers (spec §5.5) ─────────────────────────────────────────

    #[test]
    fn resolve_providers_defaults_to_the_author_when_absent() {
        let author = device(1);
        let got = resolve_providers(&file_shared_with(None), author, endpoint_of(device(2)), &[]);
        assert_eq!(
            got.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![endpoint_of(author)],
            "absent providers default to the author's device"
        );
    }

    #[test]
    fn resolve_providers_defaults_to_the_author_when_list_is_empty() {
        let author = device(1);
        let got = resolve_providers(
            &file_shared_with(Some(vec![])),
            author,
            endpoint_of(device(2)),
            &[],
        );
        assert_eq!(
            got.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![endpoint_of(author)],
            "an empty providers list falls back to the author"
        );
    }

    #[test]
    fn resolve_providers_skips_self() {
        // Fetching from yourself is a no-op; self must be filtered out of the list.
        let me = device(4);
        let got = resolve_providers(&file_shared_with(None), me, endpoint_of(me), &[]);
        assert!(got.is_empty(), "self is skipped, leaving no provider");
    }

    #[test]
    fn resolve_providers_preserves_order_and_drops_self_from_the_list() {
        let p1 = device(3);
        let me = device(4);
        let p2 = device(5);
        let shared = file_shared_with(Some(vec![p1, me, p2]));
        // author is unused because an explicit non-empty list is present.
        let got = resolve_providers(&shared, device(9), endpoint_of(me), &[]);
        assert_eq!(
            got.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![endpoint_of(p1), endpoint_of(p2)],
            "as-listed order is preserved and self is removed"
        );
    }

    #[test]
    fn resolve_providers_uses_a_matching_peer_hint_address() {
        // A `--peer <id>@<ip:port>` hint makes the dial deterministic; the matching
        // provider must resolve to that full address, not a bare EndpointId.
        let p = device(6);
        let pid = endpoint_of(p);
        let sock: SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let hint = EndpointAddr::new(pid).with_ip_addr(sock);
        let got = resolve_providers(
            &file_shared_with(Some(vec![p])),
            device(9),
            endpoint_of(device(7)),
            std::slice::from_ref(&hint),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, pid);
        assert!(
            got[0].ip_addrs().any(|a| *a == sock),
            "the matching --peer direct address must be carried through"
        );
    }

    #[test]
    fn resolve_providers_falls_back_to_a_bare_endpoint_without_a_hint() {
        let p = device(6);
        let pid = endpoint_of(p);
        let got = resolve_providers(
            &file_shared_with(Some(vec![p])),
            device(9),
            endpoint_of(device(7)),
            &[],
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, pid);
        assert_eq!(
            got[0].ip_addrs().count(),
            0,
            "with no hint the address is bare (discovery resolves it)"
        );
    }

    // ── FetchTally::classify (spec IR-0205 §5.2 — the AC2 unauthorized-vs-
    // unavailable split) ────────────────────────────────────────────────────────

    #[test]
    fn classify_all_denied_at_connect_is_unauthorized() {
        let tally = FetchTally {
            denied_at_connect: 2,
            attempted: 2,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unauthorized);
    }

    #[test]
    fn classify_all_unreachable_is_unavailable() {
        let tally = FetchTally {
            unreachable: 2,
            attempted: 2,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unavailable);
    }

    #[test]
    fn classify_mixed_denied_and_unreachable_is_unavailable() {
        // One pure authorization refusal plus one availability gap: any gap in the
        // mix keeps the honest headline "unavailable" (spec R1).
        let tally = FetchTally {
            denied_at_connect: 1,
            unreachable: 1,
            attempted: 2,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unavailable);
    }

    #[test]
    fn classify_all_denied_per_hash_is_unavailable() {
        let tally = FetchTally {
            denied_per_hash: 2,
            attempted: 2,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unavailable);
    }

    #[test]
    fn classify_zero_attempted_is_unavailable() {
        assert_eq!(FetchTally::default().classify(), FetchFailure::Unavailable);
    }

    #[test]
    fn classify_denied_connect_and_per_hash_mix_is_unavailable() {
        // A connection refusal (a pure authz wall) mixed with a per-hash denial
        // (an availability gap — the provider is up but has not synced/does not
        // hold the reference): since not *every* attempt was `DeniedAtConnect`,
        // the honest headline stays "unavailable" (spec R1 — any availability gap
        // in the mix wins). Distinct from the denied+unreachable mix above.
        let tally = FetchTally {
            denied_at_connect: 1,
            denied_per_hash: 1,
            attempted: 2,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unavailable);
    }

    #[test]
    fn classify_single_denied_at_connect_is_unauthorized() {
        // The smallest all-refused case: exactly one provider, reachable, refused
        // the connection — the pure authorization signal, so `Unauthorized` (never
        // "unavailable"). Guards the `attempted > 0 && denied_at_connect ==
        // attempted` boundary at attempted == 1.
        let tally = FetchTally {
            denied_at_connect: 1,
            attempted: 1,
            ..FetchTally::default()
        };
        assert_eq!(tally.classify(), FetchFailure::Unauthorized);
    }

    // ── save_atomic (spec §5.2 step 8) ────────────────────────────────────────

    #[test]
    fn save_atomic_writes_bytes_and_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("out.bin");
        save_atomic(&target, b"hello world").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello world");
        assert!(
            !tmp.path().join(".out.bin.part").exists(),
            "the temp part file must be renamed away, never left behind"
        );
    }

    #[test]
    fn save_atomic_replaces_an_existing_target_atomically() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("out.bin");
        std::fs::write(&target, b"stale").unwrap();
        save_atomic(&target, b"fresh").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
    }

    #[test]
    fn save_atomic_missing_parent_dir_errors_and_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("no-such-dir").join("out.bin");
        let err = save_atomic(&target, b"data").unwrap_err();
        assert!(
            err.to_string().contains("could not save to"),
            "a write failure is reported against the target: {err}"
        );
        assert!(!target.exists(), "nothing is left at the target on failure");
    }

    // ── short_endpoint ────────────────────────────────────────────────────────

    #[test]
    fn short_endpoint_is_an_eight_char_prefix() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let short = short_endpoint(id);
        assert_eq!(short.len(), 8, "diagnostic prefix is 8 chars");
        assert!(
            id.to_string().starts_with(&short),
            "the short form is a prefix of the full id"
        );
    }
}
