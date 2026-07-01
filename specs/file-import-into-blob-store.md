# Spec: File import into the blob store (`iroh-rooms file share` — import path)

| | |
|---|---|
| **Issue** | #27 — [IR-0202] Implement file import into blob store |
| **Parent** | #3 |
| **Labels** | type/feature, area/cli, area/blob, priority/p1, risk/medium |
| **Dependencies** | #13 — [IR-0009] Prototype blob ACL path (landed: `crates/spike-blobs`, findings in `crates/spike-blobs/NOTES.md`) |
| **Traceability** | `PRD.v0.3.md` §9.2 (Blob Plane / MVP file-sharing flow, steps 1–3), §11.2 (`file.shared` event), §15.6 (Share File — AC1/AC2/AC3). Spike: Blob Plane ACL confirmation (`crates/spike-blobs`). `PHASE-0-SPIKE.md` Event Protocol §7 (`file.shared` schema), §4 (BLAKE3-256 content hash). |
| **Status** | Planning — spec only. No production code changed by this document. |
| **Type** | Feature (first shipping Blob Plane code: the local producer/import half of `file share`). |

---

## 1. Summary

Implement the **local import path** of `iroh-rooms file share <ROOM_ID> <PATH>`: take a
local file, content-address it into a **durable local blob store**, record that this node is
a **local provider** of the blob, author the `file.shared` reference event so the blob is
**referenced by a room event**, and report path/permission failures clearly.

This is PRD §9.2 file-sharing-flow **steps 1–3** ("user adds file locally" → "file becomes a
content-addressed blob" → "room creates `file.shared` event"). It is deliberately the
**producer-side, offline** slice. The **network** half — standing up the `iroh-blobs` serve
ALPN with the two-gate ACL, broadcasting the reference to connected peers, and the consumer
`file fetch` command (PRD §9.2 steps 4–6 / §15.6 AC4–AC6) — is **out of scope** and tracked
as the follow-up "file share serve + fetch" issue (see §4.3). The producer authored here is
forward-compatible: the `file.shared` it persists locally propagates through the already-landed
sync engine unchanged once peers reconcile, and the durably-imported blob is exactly what the
serve ALPN will later serve.

The blob-ACL spike (#13 / `crates/spike-blobs`) already confirmed the `iroh-blobs 0.103.0`
import + provider path against `iroh 1.0.1` (the pin the shipping `iroh-rooms-net` already
uses); this issue lifts the **import** portion of that spike into production, using a
**persistent** store instead of the spike's in-memory one.

This document is detailed enough to execute without re-deriving scope.

---

## 2. Background & current repository state

**Read before starting:**

- `crates/spike-blobs/NOTES.md` — the confirmed version decision (`iroh-blobs =0.103.0` on
  `iroh 1.0.1`), the real provider API, and (§4) the verified-streaming / independent-BLAKE3
  recompute discipline. `crates/spike-blobs/src/net.rs::Provider::import` is the exact import
  pattern to lift: `store.blobs().add_bytes(data)` → assert `tag.hash == blake3(data)`.
- `specs/prototype-blob-acl-path.md` — the parent spike spec; §5/§6 describe the serve gate
  that this issue does **not** build (it is the follow-up).
- `PRD.v0.3.md` §9.2 (blobs are referenced by events, never carried inline; the 6-step flow),
  §11.2 (`file.shared` shape), §15.6 (Share File user story + ACs).
- `PHASE-0-SPIKE.md` Event Protocol §7 (`file.shared` content schema) and §4 (BLAKE3-256).

**Current repo state (relevant, already landed):**

- **`file.shared` is a first-class, conformance-tested event type.** `Content::FileShared`
  and its strict parser/encoder live in `crates/iroh-rooms-core/src/event/content.rs`
  (`FileShared { file_id: [u8;16], name, mime_type, size_bytes: u64, blob_hash: HashRef,
  blob_format: Option<"raw"|"hash_seq">, providers: Option<Vec<DeviceKey>> }`). Validation,
  canonical CBOR round-trip, and the `blob_format` enum are all done. `file.shared`
  `requires_membership_device_binding() == true` (its signing device is resolved from the
  membership fold; it carries no self-contained binding — identical to `message.text`).
- **The offline read surface already renders `file.shared`.** `crates/iroh-rooms-cli/src/room.rs`
  (`content_summary` line ~432, `content_fields` line ~505) prints `name=… size=… hash=…` in
  `room tail --offline` text mode and `{file_name, size_bytes, blob_hash}` in `--json`. So an
  authored `file.shared` event is **immediately observable** with no display changes.
- **No `file` CLI subcommand and no `build_file_shared` core builder exist yet.** Every other
  content type has a pure builder re-exported from `event/mod.rs` (`build_message_text`,
  `build_member_*`, `build_room_created`, `build_pipe_*`); `file.shared` is the one gap.
- **No shipping crate depends on `iroh-blobs` yet.** Only the throwaway `crates/spike-blobs`
  does. This issue introduces `iroh-blobs =0.103.0` as a production dependency of
  `crates/iroh-rooms-net` (which already carries the `iroh 1.0.1` stack).
- **The producer command pattern is well-established.** `crates/iroh-rooms-cli/src/message.rs`
  (`room send`) is the template: pre-IO argument validation → load signing secrets → open
  `EventStore` → `fold_room` + active-member pre-check → `select_heads` for `prev_events` →
  build + `validate_wire_bytes` + `membership.ingest` self-check → persist. `file share` is the
  same flow with a blob-import step inserted and (for this issue) **no live push**.
- **Data-directory model.** `crates/iroh-rooms-cli/src/paths.rs` resolves `<home>`
  (`--data-dir` > `IROH_ROOMS_HOME` > platform default) and `ensure_dir` tightens it to `0700`
  on Unix. `rooms.db` is `<home>/rooms.db` (`message::DB_FILE`). New blob content will live
  under `<home>/blobs/`.
- **Store discipline.** `crates/iroh-rooms-core/src/store/` is `user_version = 2` with an
  additive, forward-only migration precedent (v1→v2 added the sync-cache tables; an older
  binary opening a newer DB fails closed with `StoreError::Migration`). Any schema change here
  must follow that precedent.
- Workspace lints are strict (`unsafe_code = "forbid"`, clippy `all` + `pedantic` = warn);
  `scripts/verify.sh` runs fmt `--check`, clippy `-D warnings`, and tests with `--all-features`.
  It is the real CI gate — `cargo test` passing is necessary but not sufficient.

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

`iroh-rooms file share <ROOM_ID> <PATH>` imports a local file into the Blob Plane so it can be
referenced by room events: the content is durably content-addressed locally, the node records
itself as a provider, a `file.shared` reference is authored onto the local log, and any
path/permission failure is reported clearly with a non-zero exit and no partial writes.

### 3.2 In scope (this issue)

1. A new `file` subcommand group and the `file share <ROOM_ID> <PATH>` command.
2. Active-member + known-room pre-check (mirrors `room send`).
3. Robust file read with a **size cap** and a clear error taxonomy (missing / directory /
   unreadable / too-large / other IO), classified **before** any store write.
4. **Content hashing**: BLAKE3-256 over the file bytes, cross-checked against the blob store's
   own import hash (belt-and-suspenders, per spike §4).
5. **Durable import** into a persistent local blob store at `<home>/blobs/` (the node becomes
   a real, restart-surviving provider of the blob).
6. A new pure core builder `build_file_shared` (`event/file.rs`), the sibling of
   `build_message_text`; **author, self-validate, fold-check, and persist locally** the signed
   `file.shared` event (offline-first — no broadcast in this issue).
7. **Local provider status** surfaced deterministically: `file share` prints it, and a small
   `file list <ROOM_ID>` offline read reports, per shared file, whether **this node holds the
   blob** (`provider: you (local)`) vs. reference-only.
8. Script-friendly, secret-safe output; a CLI integration + core unit test suite covering the
   issue's test plan.

### 3.3 Out of scope / non-goals (explicit — tracked as the follow-up serve/fetch issue)

- **The blobs serve ALPN + two-gate ACL** (per-node admission + per-hash authorization) wired
  into the live `Node`. The spike proved it (`spike-blobs/src/net.rs`); productionizing it is
  the follow-up. This issue imports and references; it does **not** serve bytes to peers.
- **`file fetch`** (the consumer side) and receiver-side content verification on fetch
  (PRD §15.6 AC5).
- **Broadcasting the `file.shared` frame** to connected peers at share time. The event is
  persisted locally now and propagates via the existing sync engine; a best-effort live push
  (the `room send` `run_push` analogue) is deferred to the serve issue so this slice stays
  fully offline and matches the issue's offline test plan.
- **`hash_seq` / collection blobs.** Default and only `blob_format` here is `"raw"`
  (single blob); `hash_seq` is a documented follow-up (spike NOTES §6).
- **Per-blob revocation, blob GC / pinning / eviction policy, quotas** (Residual Risk #3).
- **Multi-file / directory / recursive share, resumable import, dedup UX.**
- **Any change to the `file.shared` wire schema** — it is already landed and conformance-tested.

---

## 4. Placement, dependencies, and the follow-up boundary

### 4.1 Where the code lives

```
crates/iroh-rooms-core/
  src/event/file.rs         # NEW: pure `build_file_shared` builder (sibling of message.rs)
  src/event/mod.rs          # re-export build_file_shared
  src/event/constants.rs    # NEW const: MAX_SHARED_FILE_BYTES (+ file_id domain if needed)

crates/iroh-rooms-net/
  Cargo.toml                # NEW dep: iroh-blobs = "=0.103.0"
  src/blob.rs               # NEW: BlobStore wrapper over a persistent iroh-blobs FsStore
  src/lib.rs                # pub mod blob; re-export BlobStore + BlobImport

crates/iroh-rooms-cli/
  src/file.rs               # NEW: `file share` / `file list` orchestration (the thin driver)
  src/cli.rs                # NEW `File { action: FileAction }` subcommand + dispatch_file
  src/main.rs / lib wiring  # register the module
  tests/file_cli.rs         # NEW: CLI integration suite (the issue test plan)
```

Rationale for the split:

- **Content hashing + the `file.shared` builder are pure/sans-IO** → `iroh-rooms-core`
  (`build_file_shared` needs no `iroh`, mirroring every other builder). Core stays free of the
  async `iroh`/`iroh-blobs` stack.
- **The durable blob store is an iroh concern** → `iroh-rooms-net`, which already owns the
  `iroh 1.0.1` transport and will own the serve ALPN in the follow-up. Keeping the blob store
  next to the (future) provider avoids a second home for blob content.
- **The CLI orchestrates**: read+classify the file → `net::BlobStore` import → `core`
  build/validate/fold → `core::EventStore` persist → print. Same shape as `message.rs`.

### 4.2 Dependency to add (production)

- `iroh-blobs = "=0.103.0"` on `crates/iroh-rooms-net` — the exact line the spike confirmed
  (`spike-blobs/NOTES.md` §1) and which pins `iroh ^1.0` (resolves `1.0.1`, matching net's
  existing `iroh = "=1.0.1"`). No new iroh-version divergence. Keep net clippy-clean under the
  workspace `pedantic` lints (do not relax them).

### 4.3 The follow-up boundary (write it down)

`file share` in this issue **imports + references** but does not **serve or push**. Document
in code comments and the follow-up issue that the serve/fetch issue must:
(a) add the `iroh-blobs` ALPN to the shared `Router` with the spike's two-gate ACL
(`spike-blobs/src/net.rs::spawn_event_gate`), (b) add `file fetch <ROOM_ID> <FILE_ID>` with the
independent BLAKE3 recompute (spike §4 / AC4), (c) optionally add a best-effort broadcast of the
`file.shared` frame at share time, and (d) map the "no provider online" outcome to honest
"unavailable" CLI language (spike §5 / PRD §14).

---

## 5. Design

### 5.1 Command surface

```text
iroh-rooms [--data-dir <PATH>] file share <ROOM_ID> <PATH> [--mime <TYPE>] [--name <NAME>]
iroh-rooms [--data-dir <PATH>] file list  <ROOM_ID> [--json]
```

- `file share` — the import path (this issue's core deliverable). Synchronous from the user's
  view; internally runs the async blob import in a scoped Tokio runtime (the `iroh-blobs` store
  API is async), exactly as `room send`/`room tail` enter a scoped runtime. **No network is
  contacted** — the runtime is only for the local async store API.
  - `--mime <TYPE>` (optional) overrides the derived MIME type.
  - `--name <NAME>` (optional) overrides the stored display name (defaults to the path's file
    name). Useful when the on-disk name is not the intended shared name.
- `file list` — an **offline** read (no network, no membership requirement, mirrors
  `room tail --offline`): for the room, decode every `file.shared` event and print
  `file_id`, `name`, `size`, `hash`, and **provider status** (does the local blob store hold
  the hash?). `--json` emits a single JSON array. `file list` is included because the issue's
  "Local provider status is recorded" AC needs an observable surface and the getting-started
  walkthrough (`docs/getting-started.md` Step 5) already references `iroh-rooms file list`.

### 5.2 Data flow for `file share` (happy path)

```
1. Parse ROOM_ID; validate --mime/--name (pre-IO). Bad args ⇒ exit non-zero, nothing written.
2. Load signing secrets (identity + device). No identity ⇒ clear error.
3. Open <home>/rooms.db; fold the room; require caller is an ACTIVE member (else clear error).
4. Stat PATH and classify:
     - not found              ⇒ "no such file: <path>"
     - is a directory         ⇒ "<path> is a directory, not a file"
     - metadata len > cap     ⇒ "<path> is N bytes; exceeds the MVP share limit of M bytes"
     - unreadable (open fails, PermissionDenied) ⇒ "permission denied reading <path>"
   All classification happens BEFORE any store/blob write (a bad invocation writes nothing).
5. Import into the durable blob store at <home>/blobs/:
     - stream the file by path (add_path — do NOT read the whole file into memory);
     - obtain the store's Hash; independently recompute BLAKE3-256 and assert equality
       (spike §4 belt-and-suspenders — an internal-bug guard, not a trust boundary).
6. Derive metadata:
     - file_id  = 16 random bytes (CSPRNG), the on-wire SHORT_ID; CLI handle "file_<hex>".
     - name     = --name else PATH's file_name (validated: non-empty, no NUL/control chars).
     - mime     = --mime else guess_from_extension(PATH) else "application/octet-stream".
     - size     = metadata length (u64).
     - blob_hash= HashRef::from_bytes(hash) ; blob_format = "raw".
     - providers= Some(vec![ self device_id ])  (§7 default is [device_id]; make it explicit).
7. prev_events = current room heads (select_heads, bounded to MAX_PREV_EVENTS) — identical to
   room send.
8. build_file_shared(identity, device, room_id, file_id, name, mime, size, blob_hash,
     "raw", [self_device], prev_events, created_at) -> WireEvent.
9. validate_wire_bytes(...) self-check; membership.ingest(...) self-check (internal-error guard
   — an active member citing heads is always Accepted, per the ancestor-view rule; a Rejected/
   Buffered result is an internal bug, surfaced as such and nothing is persisted).
10. store.insert(validated)  — persist the file.shared event locally (offline guarantee).
11. Print the FileShareSummary (file_id, hash+size, event_id, provider=you (local), a next-step
    hint pointing at `file list` / the future `file fetch`).
```

### 5.3 Provider status — no new SQLite table (recommended)

"Local provider status is recorded" is satisfied **without** a bespoke metadata table:

- The **content** is durably held in the persistent blob store (`<home>/blobs/`), which
  survives restart. Holding the blob **is** being a provider.
- The **metadata** (`file_id`, `name`, `mime_type`, `size_bytes`, `blob_hash`, `blob_format`,
  `providers`) is the `file.shared` **event on the log** — the canonical, replicated record.
- **Provider status is derived**: for a given `file.shared`, this node is a `local` provider
  iff its blob store contains `blob_hash` (`BlobStore::has(hash)`), else it is `reference-only`
  (it knows the file exists but does not hold the bytes — the future "unavailable / fetch it"
  state). `file list` computes this per row.

This keeps `rooms.db` at `user_version = 2` (no migration), matches the repo's "the log is the
source of truth; everything else is derived" discipline, and avoids a divergent second metadata
store. The `providers` field on `file.shared` records the *asserted* provider set (this node's
`device_id`) on the log; the *actual* local-holding status is the blob-store membership check.

> **Alternative (Open Question OQ-2):** a dedicated additive `shared_files` table (schema v3,
> same forward-only pattern as v2) keyed by `(room_id, file_id)` storing `source_path` and an
> explicit `provider_state`. Only needed if the product wants to (a) remember the original
> on-disk path after import, or (b) support an import that does **not** author an event. Neither
> is required by the ACs; recommend deferring unless a follow-up needs it.

### 5.4 The `build_file_shared` core builder

New `crates/iroh-rooms-core/src/event/file.rs`, a byte-for-byte sibling of
`event/message.rs::build_message_text`: pure, clock-/RNG-free (the caller injects `file_id`,
`created_at`, and `prev_events`), signs the canonical `Content::FileShared` under the identity +
device keys, and returns a `WireEvent`. Re-export `build_file_shared` from `event/mod.rs`.
Golden-test its `event_id` against a fixed input vector (a regression tripwire, exactly like the
other builders' golden locks). No new validation logic — `Content::FileShared` parsing/encoding
and the `blob_format` enum already exist and are conformance-tested.

Signature (mirrors `build_message_text`):

```rust
#[allow(clippy::too_many_arguments)]
pub fn build_file_shared(
    identity: &SigningKey,
    device: &SigningKey,
    room_id: &RoomId,
    file_id: [u8; SHORT_ID_LEN],
    name: &str,
    mime_type: &str,
    size_bytes: u64,
    blob_hash: HashRef,
    blob_format: Option<&str>,        // Some("raw") here; None also valid (defaults raw on read)
    providers: &[DeviceKey],          // -> Some(vec) when non-empty, else None (omit)
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent
```

### 5.5 The `net::BlobStore` wrapper

A thin, persistent-store wrapper in `crates/iroh-rooms-net/src/blob.rs`:

```rust
pub struct BlobStore { /* holds an iroh-blobs persistent (fs) store rooted at <home>/blobs */ }

impl BlobStore {
    /// Open (creating if absent) a durable blob store under `dir`.
    pub async fn open(dir: &Path) -> Result<Self, BlobError>;

    /// Import a file by path (streamed, not fully buffered). Returns the content
    /// hash and byte length. Asserts the store hash == independent BLAKE3-256.
    pub async fn import_path(&self, path: &Path) -> Result<BlobImport, BlobError>;

    /// Whether this store currently holds `hash` (⇒ local provider). Durable across restart.
    pub async fn has(&self, hash: [u8; 32]) -> Result<bool, BlobError>;
}

pub struct BlobImport { pub hash: [u8; 32], pub size_bytes: u64 }
```

Return the raw `[u8; 32]` hash across the crate boundary (the CLI wraps it in `HashRef`); do not
leak `iroh_blobs::Hash` into the CLI signature. **Step 0 (§6.1) must confirm the exact 0.103.0
persistent-store type and its `add_path`/`has`/blob-presence API** — the spike only exercised
`MemStore`; the persistent `FsStore` API surface (and whether import is by path or bytes) must
be verified on docs.rs before coding, exactly as the spike's Step 0 did for the provider events
API.

### 5.6 MIME derivation (dependency-free)

Default `application/octet-stream`; a tiny built-in extension→type map covers the obvious cases
(`txt`→`text/plain`, `md`→`text/markdown`, `pdf`→`application/pdf`, `png`/`jpg`/`gif`,
`json`, `html`, `csv`, `zip`, …). `--mime` always wins. Avoid pulling a `mime_guess` crate for
MVP; the map is a handful of lines and the flag covers the long tail. (OQ-3.)

### 5.7 Size cap

`MAX_SHARED_FILE_BYTES` in `event/constants.rs` (or a CLI-local const if the team prefers policy
in the CLI). Proposed default **100 MiB** (`104_857_600`). Enforced against the file's
**metadata length before import** so an over-cap file is rejected without reading it. Streaming
import (`add_path`) means memory is not the binding constraint; the cap is a deliberate MVP
policy bound (protects the store and keeps `file list`/sync sane). Exact value is OQ-1.

---

## 6. Implementation steps

Work top to bottom; each step is independently reviewable.

### 6.1 Step 0 — Confirm the persistent `iroh-blobs` store API (gates the rest)

On docs.rs for **`iroh-blobs 0.103.0`**, confirm the persistent (filesystem) store type and its
API: how to `open`/create a store rooted at a directory, how to import a file (by path vs. by
bytes; prefer a streaming path import), how to query blob presence (`has`/`status`), and that
its content hash is BLAKE3-256 (it is — the spike relied on it). Record the exact type/method
names in a short comment in `blob.rs` (the spike's `MemStore` names may differ for `FsStore`).
If a path-streaming import is unavailable, fall back to buffered `add_bytes` **and** lower the
size cap accordingly, and note it. This mirrors the spike's Step-0 discipline (its own doc names
were recon that had to be confirmed).

### 6.2 Step 1 — Core: `build_file_shared` + constant

Add `event/file.rs` with `build_file_shared` (§5.4), re-export from `event/mod.rs`, add
`MAX_SHARED_FILE_BYTES` to `event/constants.rs`. Unit-test: determinism, all-field round-trip
through `validate_wire_bytes`, a golden `event_id` lock, empty-`providers` ⇒ field omitted,
`blob_format` `Some("raw")` vs `None`. No CLI/net dependency — this compiles and tests under
core alone.

### 6.3 Step 2 — Net: `BlobStore` wrapper

Add `iroh-blobs =0.103.0` to `iroh-rooms-net/Cargo.toml`; add `src/blob.rs` (§5.5); re-export
`BlobStore`/`BlobImport`/`BlobError` from `lib.rs`. Lift the spike's import+recompute assertion
(`spike-blobs/src/net.rs::Provider::import`) to the persistent store. Unit-test against a
`tempfile` dir: import a known-bytes file → assert the returned hash equals an independent
`blake3::hash`, `size_bytes` is correct, `has(hash)` is true afterwards and **false for an
unrelated hash**, and that a reopened store still reports `has(hash) == true` (durability).

### 6.4 Step 3 — CLI: `file` subcommand + `file share`

Add `File { action: FileAction }` to `cli.rs` with `FileAction::Share { room_id, path, mime,
name }` and `FileAction::List { room_id, json }`; add `dispatch_file` (mirrors `dispatch_pipe`).
Add `src/file.rs` implementing the §5.2 flow:

- Pre-IO validation of `--mime`/`--name`/ROOM_ID.
- `identity::SecretKeys::load` + `EventStore::open` + `message::fold_room` + active-member
  pre-check (reuse the exact helpers `room send` uses — do not duplicate).
- `classify_path(path)` → the §5.2 step-4 error taxonomy, returning a typed error mapped to an
  actionable `anyhow` message; runs before any store write.
- Scoped Tokio runtime (reuse `cli::runtime()`); `BlobStore::open(<home>/blobs)` +
  `import_path`.
- `select_heads` + `build_file_shared` + `validate_wire_bytes` + `membership.ingest` self-check
  + `store.insert`.
- Print `FileShareSummary` (labeled lines: `imported`, `file_id`, `hash`, `size`, `event`,
  `provider: you (local)`, next-step hint). Secret-safe (no key bytes anywhere).

### 6.5 Step 4 — CLI: `file list`

Offline read: `store.by_type(room, EventType::FileShared)` (or fold + scan), decode each event's
`Content::FileShared`, and print `file_id`, `name`, `size`, `hash`, and provider status computed
via `BlobStore::has(blob_hash)` (open the store read-only). `--json` emits a single array with
stable field names (`file_id`, `name`, `size_bytes`, `blob_hash`, `provider`). Reuse the room
display idioms (`short_hex`, hash `blake3:<hex>` form). Fully synchronous except the async
`has` check (scoped runtime, or a sync presence check if the confirmed API offers one).

### 6.6 Step 5 — Tests + docs

`tests/file_cli.rs` (§8) covering the issue test plan. Update `docs/getting-started.md` Step 5
only if the illustrative output drifts from the real output (the doc already describes this
command; keep it truthful — note that fetch/serve is the follow-up). Update `README.md` "Current
Status" with the landed import path if the repo convention is to do so (it is — every prior
issue added a status paragraph). Run `scripts/verify.sh` (fmt + clippy `-D warnings` +
`--all-features` tests) — the real CI gate.

---

## 7. Error model & observability

**Error taxonomy (AC4 — "reports path/permission errors clearly"):** every failure exits
non-zero, writes nothing to the store or blob store, and prints one actionable line to stderr:

| Condition | Detection | Message (shape) |
|---|---|---|
| Path missing | `symlink_metadata`/`metadata` → `NotFound` | `no such file: <path>` |
| Path is a directory | `metadata.is_dir()` | `<path> is a directory, not a file; share a single file` |
| Unreadable | open/read → `PermissionDenied` | `permission denied reading <path>` |
| Too large | `metadata.len() > MAX_SHARED_FILE_BYTES` | `<path> is <N> bytes; exceeds the MVP share limit of <M> bytes` |
| Other IO | residual `io::Error` | `could not read <path>: <source>` |
| Not an active member | membership fold | `you are not an active member of room <id>; only an active member can share files` |
| Unknown room | `fold_room` empty | reuse `fold_room`'s existing "no room …" message |
| No identity | `SecretKeys::load` | reuse the existing identity-missing message |

Empty (0-byte) files are **allowed** (a valid content-addressed blob; the spike's tests cover
zero-size round-trips) — optionally emit an advisory note but do not fail. Symlinks are followed
(std default); classification uses `metadata` (follows) so a symlink to a missing target reports
`no such file`.

**Observability:** the command prints a stable, greppable, secret-free summary on success and a
single actionable error on failure. No tracing subscriber is installed on the CLI (per the
`cli-has-no-tracing-subscriber` note), so any operator-facing signal must be on stdout/stderr,
not `tracing` — this command needs none beyond its printed summary.

---

## 8. Test strategy

Maps the issue Test Plan ("CLI test with small file, missing file, unreadable file, and hash
verification") plus the four ACs.

**Core unit tests (`event/file.rs`):**
- `build_file_shared` determinism (same inputs ⇒ same `event_id`); golden `event_id` lock.
- Full round-trip through `validate_wire_bytes` (self-validates like a real send).
- `providers` empty ⇒ omitted; `blob_format` `Some("raw")`/`None` both encode.

**Net unit tests (`blob.rs`):**
- Import known bytes → hash == independent BLAKE3; `size_bytes` correct.
- `has(hash)` true after import, false for an unrelated hash.
- Reopen the store over the same dir → `has(hash)` still true (**durability**).

**CLI integration tests (`tests/file_cli.rs`, `assert_cmd` + per-test `IROH_ROOMS_HOME`):**
- **Small file (AC1):** create identity + room; `file share <room> ./small.txt` exits 0, prints
  `file_id`/`hash`/`size`; the `file.shared` event appears in `room tail --offline`
  (`name=…`), and `file list` shows `provider: you (local)`.
- **Hash verification (AC2):** the printed `hash` equals `blake3:<hex>` of an independent BLAKE3
  over the file bytes, and equals the `blob_hash` shown by `room tail --offline --json`.
- **Missing file:** `file share <room> ./does-not-exist` exits non-zero, stderr contains
  `no such file`; the store has no new `file.shared` (assert via `room tail --offline`).
- **Unreadable file (Unix):** `chmod 000` a file → exits non-zero, stderr `permission denied`;
  no event persisted. (`#[cfg(unix)]`; document the skip on Windows.)
- **Directory path:** `file share <room> ./somedir` → clear "is a directory" error.
- **Too-large file (AC "up to MVP target size"):** with a low cap injected for the test (a
  hidden `--max-bytes`/`IROH_ROOMS_MAX_SHARE_BYTES` test seam, or a fixture just over the real
  cap), assert the too-large error and that a file **at** the cap succeeds — proving the
  boundary. (Decide the seam in review; OQ-4.)
- **Non-member:** a second identity that never joined → `file share` refused with the
  active-member message; nothing written.
- **Provider status persisted (AC3):** after `file share`, drop the process and run `file list`
  fresh (reopens the blob store) → still `provider: you (local)` (durability across restart).
- **Secret hygiene:** no `identity.secret` bytes / seed hex appear on stdout or stderr in any
  path (grep the captured output, mirroring the identity/room suites).

All CLI tests are **offline** (no `--peer`, no network, no relay) — consistent with the issue's
offline test plan and this issue's no-serve scope.

---

## 9. Security, privacy, reliability, performance

- **Authorization:** only an **active member** may author a `file.shared` (pre-checked; the fold
  would reject a non-member's event regardless — the `member-message-ancestor-view-gate` rule).
  The blob import itself is local and unprivileged, but gating the *reference* keeps the log
  clean. Note: this issue does **not** serve bytes, so no ACL is exercised on a network path yet
  — that is the follow-up (spike §5). Do not imply the blob is access-controlled to peers here;
  it is not served at all yet.
- **Content integrity:** BLAKE3-256 is computed and cross-checked against the store's own import
  hash; `blob_hash` on the log is that digest. Receiver-side verification on fetch is the
  follow-up (spike §4 / AC4). The `file.shared` reference is signed under the device key like
  every other event, so its metadata is tamper-evident.
- **Privacy:** the original **filesystem path is not put on the log** — only the (possibly
  overridden) file `name`. The `providers` field discloses this node's `device_id` as a provider
  (intended). The blob content lives under the `0700` `<home>/blobs/`; do not loosen those
  permissions. No secret key material is read for the import or printed anywhere.
- **Reliability / durability:** the blob store is persistent (`FsStore`), so provider status
  survives restart; the `file.shared` event is persisted transactionally via `EventStore::insert`
  (idempotent — re-sharing the same path re-imports to the same content hash and inserts a *new*
  `file.shared` with a fresh `file_id`, which is fine; the blob is deduped by hash in the store).
  A crash between blob import and event insert leaves an orphan blob in the store (harmless,
  re-importable) but no dangling event — order the import **before** the insert so the failure
  mode is "blob present, no reference" not "reference to an absent blob."
- **Performance:** stream the import by path (no full-file buffering); hashing is BLAKE3 (fast).
  The size cap bounds worst-case work. `file list` is a bounded per-room scan.
- **Migration:** none (recommended design adds no schema table; §5.3). If OQ-2 (sidecar table)
  is chosen, it must be an additive, forward-only v2→v3 migration with the old-binary-fails-closed
  guard, exactly like the v1→v2 sync-cache migration.

---

## 10. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | The persistent `iroh-blobs 0.103.0` store API differs from the spike's `MemStore` (type/methods for open/import/presence). | Medium | Medium | Step 0 confirms the real `FsStore` API on docs.rs before coding; isolate all `iroh-blobs` types behind `net::BlobStore` so a rename touches one file. |
| R2 | `0.103.0` is maintainer-labeled pre-production (spike R4). | Medium | Medium | Same pin the spike already validated for import; import is a narrow, well-exercised path; flag any instability into the Residual-Risk-#10 shipping decision. |
| R3 | Scope creep — implementers add serve/fetch/broadcast because the command is named `share`. | Medium | Medium | §3.3 and §4.3 draw the boundary explicitly; the offline test plan enforces it; the follow-up issue owns the network half. |
| R4 | "MVP target size" is unspecified in the PRD, so the cap is a guess. | High | Low | Propose 100 MiB as a named, single-source constant; make it trivially changeable; call it out as OQ-1 for product sign-off. |
| R5 | Adding `iroh-blobs` enlarges the net (and transitively CLI) build/dep tree and CI time. | Medium | Low | The `iroh 1.0` tree is already pulled in by net; `iroh-blobs` adds to it but shares the resolver. Keep it a normal (non-optional) dep of net only. |
| R6 | A `file.shared` referencing a blob peers cannot yet fetch (no serve) could confuse users. | Medium | Low | Output and `file list` say `provider: you (local)` and the next-step hint notes fetch is not wired yet; the availability model already tolerates reference-before-fetch. |
| R7 | Orphaned blobs accumulate (no GC) on repeated shares / failed inserts. | Low | Low | Out of scope (Residual Risk #3); dedup-by-hash bounds duplicates; document GC as a follow-up. |

---

## 11. Acceptance criteria

Maps issue #27 ACs + Test Plan to this issue.

- [ ] **AC1 — A file up to the MVP target size can be imported.** `file share <room> <path>`
  imports a small file successfully; a file **at** `MAX_SHARED_FILE_BYTES` succeeds and one
  **over** it is rejected with the size error (§7). Import streams by path (no full buffering).
- [ ] **AC2 — Content hash is computed and persisted.** BLAKE3-256 is computed, cross-checked
  against the store's import hash, and persisted as `file.shared.blob_hash` on the local log
  (observable via `room tail --offline`/`--json` and `file list`); the printed hash equals an
  independent recompute over the bytes.
- [ ] **AC3 — Local provider status is recorded.** After import the node durably holds the blob
  (`<home>/blobs/`, survives restart) and `file list` reports `provider: you (local)` for it,
  including after a fresh process start.
- [ ] **AC4 — Import failure reports path/permission errors clearly.** Missing, directory,
  unreadable (Unix `0600`→`000`), too-large, and other-IO paths each exit non-zero with a
  distinct, actionable message and leave the store + blob store untouched.
- [ ] **AC5 — `file.shared` metadata is prepared and referenced.** A signed `file.shared`
  carrying `file_id`, `name`, `mime_type`, `size_bytes`, `blob_hash`, `blob_format="raw"`, and
  `providers=[self device]` is authored, self-validated, fold-accepted, and persisted locally
  (the "prepare metadata needed for `file.shared`" scope item, realized as an on-log reference).
- [ ] **AC6 — No regressions / gate green.** `scripts/verify.sh` passes (fmt `--check`, clippy
  `-D warnings` pedantic, `--all-features` tests); no change to the `file.shared` wire schema;
  the follow-up serve/fetch boundary (§4.3) is documented in code + the follow-up issue.

**Test-plan coverage:** small file → AC1/AC5; hash verification → AC2; missing file & unreadable
file → AC4; provider persistence → AC3.

---

## 12. Assumptions

1. `iroh-blobs 0.103.0` exposes a persistent (filesystem) store with a directory-rooted open, a
   file/bytes import returning a BLAKE3-256 `Hash`, and a blob-presence query — to be confirmed
   in Step 0 (§6.1). The spike confirmed the *provider/import* path on `MemStore`; the durable
   store is the new surface.
2. Authoring the `file.shared` reference **locally now** (offline, no broadcast) is the right
   unit for "prepare metadata needed for `file.shared`" and satisfies the issue goal ("so it can
   be referenced by room events"), with propagation deferred to the existing sync engine. (If the
   product wants strictly import-with-no-event, see OQ-2 / OQ-5.)
3. An active-member pre-check is desired (consistent with `room send`/`pipe`), even though the
   fold would reject a non-member's event anyway.
4. The recommended no-new-table design (§5.3) is acceptable; the original on-disk path need not
   be remembered after import.
5. `raw` single-blob is the only `blob_format` for MVP; `hash_seq` is a follow-up.
6. Two isolated identities are unnecessary here (no fetch); all tests are single-process offline.

## 13. Open questions

- **OQ-1 (size cap):** What is the exact "MVP target size"? Proposed 100 MiB. Product sign-off
  needed; the PRD gives no number (the §11.2 example is a 204,800-byte PDF).
- **OQ-2 (metadata store):** Recommended design records metadata as the on-log `file.shared`
  event + blob-store presence (no schema change). Do we instead want an additive `shared_files`
  table (schema v3) to remember `source_path` / an explicit `provider_state`? Only if a follow-up
  needs the original path or an import-without-event mode.
- **OQ-3 (MIME):** Built-in extension map + `--mime` override + `application/octet-stream`
  default, or pull a `mime_guess`-style crate? Recommend the dependency-free map for MVP.
- **OQ-4 (size-cap test seam):** How to test the too-large boundary cheaply — a hidden
  `--max-bytes` flag, an `IROH_ROOMS_MAX_SHARE_BYTES` env override, or a real over-cap fixture?
  Recommend a hidden test-only override to keep the fixture small and CI fast.
- **OQ-5 (broadcast at share time):** Should `file share` best-effort push the `file.shared`
  frame to connected peers now (like `room send`), or is offline-persist-only correct for this
  issue with broadcast folded into the serve/fetch follow-up? Recommend defer (keeps this slice
  offline and matches the test plan).
- **OQ-6 (`file_id` presentation):** CLI handle format — `file_<32-hex>` (proposed; matches the
  PRD `"file_…"` shape and the getting-started mock) vs. bare hex (matches `pipe_id`). Pick one
  and keep it stable, since `file fetch`/`file list` will parse it.

## 14. Definition of done

1. `iroh-rooms file share <ROOM_ID> <PATH>` imports a file durably, computes+persists the
   BLAKE3-256 hash, authors+persists the `file.shared` reference locally, records local provider
   status, and reports path/permission errors clearly (AC1–AC5).
2. `file list <ROOM_ID>` reports each shared file with provider status (AC3 observability).
3. `build_file_shared` lands in core with a golden `event_id` lock; `net::BlobStore` wraps the
   persistent `iroh-blobs` store; the CLI orchestrates them without duplicating `room send`'s
   helpers.
4. `tests/file_cli.rs` + core/net unit tests cover the test plan and all ACs; `scripts/verify.sh`
   is green (AC6).
5. The serve-ALPN + `file fetch` + broadcast boundary (§4.3) is documented in code and captured
   as the follow-up issue.
</content>
</invoke>
