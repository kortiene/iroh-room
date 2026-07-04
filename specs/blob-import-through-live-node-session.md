# Blob import through a live `Node` session (`Node::blob_import` / `blob_import_bytes`)

- **Issue:** #84 — `feat(blob): blob import through a live Node session (resident hosts must cycle the session to share a file)`
- **Proposed work item:** IR-0308
- **Labels:** `enhancement`, `type/feature`
- **Status:** Spec only — not implemented. This document is a build plan; no production code is written here.
- **Owning crates:** `iroh-rooms-net` (`Node` methods + `BlobStore::import_bytes` + `BlobError` variant), `iroh-rooms` (façade — rides existing re-exports)
- **Filed by:** a real SDK consumer — **Bantaba**, a resident daemon + web UI on the developer-preview façade (`--features experimental`, rev `1d2f014`).

---

## 1. Problem statement

A serving `Node` owns its durable blob store for the whole session. The store is
an `iroh-blobs` `FsStore` that takes an **exclusive** `redb` lock for the lifetime
of the opener (`crates/iroh-rooms-net/src/blob/mod.rs:57-67`): a second opener of
the same directory blocks, then fails with `BlobError::Locked`
(`blob/mod.rs:236`). So while a room session is live there is exactly **one** legal
opener of `<home>/blobs/` — the `Node` itself.

The façade exposes **no import path through the live node**. `Node` can serve
blobs (`spawn_room` with a `BlobServeConfig`, `node.rs:278-304`) and fetch them
(`Node::fetch_file`, `node.rs:868`), but the only way to *import* a new blob is
`BlobStore::open` → `import_path` → `close` (`blob/mod.rs:97/134/199`), which needs
its own exclusive lock. A live session already holds that lock.

Consequence, for a resident daemon (unlike the CLI's short-lived process model):

- **`file share` requires a full session cycle.** To import a file the daemon must
  `shutdown node → open store → import → close store → respawn node → publish
  file.shared`. Every share makes connected peers observe a disconnect/reconnect
  (a `ConnEvent` churn on every peer's link).
- **Fetched bytes can't be re-provided in-session.** After `Node::fetch_file`
  returns verified bytes, a long-running consumer **cannot re-import them to become
  a provider**. The CLI's pattern — re-import on the next process start
  (`reprovide_best_effort`, `crates/iroh-rooms-cli/src/file.rs:852`) — has no analog
  in a process that never exits. Content availability is weakest exactly where an
  always-on daemon should strengthen it.

The capability already exists **inside** the session: `Node` holds the open
`BlobStore` (`node.rs:188`), and `BlobStore::import_path` is an `&self` method
(`blob/mod.rs:134`) — importing needs no second opener, no `&mut`, and no session
cycle. Only the façade is missing.

### Goal

Expose the import half of the node-owned store as two `&self` methods on `Node`:

```rust
impl Node {
    /// Import a file into the store this session already owns; returns the
    /// verified content ref. No session cycle, no second store open.
    pub async fn blob_import(&self, path: &Path) -> Result<BlobImport, BlobError>;
    /// Same, from memory — re-provide fetched bytes in-session.
    pub async fn blob_import_bytes(&self, bytes: Bytes) -> Result<BlobImport, BlobError>;
}
```

reachable through the façade at `iroh_rooms::experimental::session::Node`.

### Non-goals

- **A raw store handle (`Node::blob_store()`).** The issue floats a cloneable
  handle exposing the `BlobStore` import API. Rejected: it leaks `iroh_blobs` types
  across the crate boundary and breaks the module-isolation invariant (spec R1,
  `blob/mod.rs:10-13`). Two thin `&self` methods keep `iroh_blobs` behind the
  wrapper. (Key decision §11.1.)
- **Authoring `file.shared` inside `blob_import`.** Import returns the verified
  `BlobImport`; the consumer authors + publishes the reference via the existing
  `build_file_shared` + `Node::publish` (§6.3). Bundling them would duplicate the
  CLI's `share` orchestration inside `net` and couple two independent capabilities.
- **CLI changes.** `file share`/`file fetch` are short-lived processes that never
  keep a `Node` alive, so they don't have this problem (`file.rs:117`/`478`); their
  open→import→close and `reprovide_best_effort` flows stay as-is. This spec adds an
  SDK primitive only.
- **Removing / changing the exclusive-lock model or `BlobError::Locked`.** The lock
  is correct; this feature routes *around* it by reusing the one legal opener.
- **Auto-refresh of Gate 2 without a `file.shared`.** A freshly imported blob is
  served only once a `file.shared` referencing it exists (Gate 2, §6.4). Importing
  bytes with no reference is legal but unserveable until referenced — by design.

---

## 2. Background — why the design is constrained (and simple)

Three load-bearing facts about this codebase shape the design; together they make
this a small, low-risk change.

### 2.1 The `Node` already owns the open store as an `&self`-usable handle

For a blob-serving session, `spawn_inner` opens the store once and keeps it on the
node: `blob_store: Option<BlobStore>` (`node.rs:185-188`, set at `node.rs:439`). The
same open store is handed to `BlobsProtocol::new(&self.store, …)` via
`serve_handler` (`blob/mod.rs:183`); `FsStore` is a cheap `Clone` handle
(`blob/mod.rs:180-181`), so the serving protocol and the node's `BlobStore` are two
handles onto **one** store actor. Import (`import_path`) and presence (`has`) are
both `&self` (`blob/mod.rs:134`, `:169`) — only `close` consumes `self`
(`blob/mod.rs:199`). So a new `Node::blob_import(&self, …)` can import through the
already-open handle with **no** new `FsStore::load`, hence no lock contention.

### 2.2 The blob store is a separate actor from the single-owner engine

The `SyncEngine` is single-owner: exactly one pump task touches it, and every read
(`room_tail`, `snapshot`, `heads`) is routed through `Cmd`s over a channel so no
second `&mut SyncEngine` ever exists (`node.rs:1-17`). The **blob store is not part
of that** — it is an independent `iroh-blobs` actor with its own internal command
channel. `import_path`/`has` are already called `&self` off the pump today (CLI
`file.rs`, `blob_e2e.rs`), and `blob_store` is **not** moved into the pump — `Node`
keeps it directly (`node.rs:188`, destructured in `shutdown` at `node.rs:897`).

⇒ **Import needs no pump routing and adds no `Cmd` variant.** The engine stays
single-owner untouched. Concurrent `add` (from `blob_import`) and `get` (from the
serving `BlobsProtocol`) are serialized by the store actor — that is what the actor
model is for. This is strictly simpler than the sibling push-subscription work
(#83/IR-0307), which had to thread through the engine + pump.

### 2.3 Gate 2 already keys off `file.shared`, refreshed on every publish

The serve ACL is two gates (`blob/mod.rs:278-329`): Gate 1 = active member, Gate 2
= hash referenced by a `file.shared`. The pump's `RoomReconciler` refreshes the
`BlobAclView` from `engine.file_shared_hashes()` after every fold-mutating drive and
each tick — and it tracks referenced hashes **independently** of membership
precisely because a new `file.shared` is a content event that doesn't change
membership (`node.rs:100-153`, and the `member-message-ancestor-view-gate` /
`membership-snapshot-ignores-content-events` memories). So once the consumer
publishes the `file.shared` for a freshly imported blob, Gate 2 begins allowing that
hash on the next drive — no extra wiring needed here (§6.4).

---

## 3. Design overview

```
  resident daemon (façade consumer)                Node (already live, spawn_room + BlobServeConfig)
  ─────────────────────────────────                ────────────────────────────────────────────────
  share a local file:                              blob_store: Option<BlobStore>  ── one FsStore actor ──┐
    let imp = node.blob_import(&path).await?; ───►  BlobStore::import_path(&self)  (no new open, no lock) │
    let wire = build_file_shared(.. imp.hash ..);                                                          │
    node.publish(wire).await?;              ─────►  engine.publish → fanout to live peers                  │  BlobsProtocol
                                                    RoomReconciler refreshes Gate 2 (file_shared_hashes)   │  serves the
                                                                                                            │  same store
  re-provide fetched bytes:                                                                                 │  concurrently
    let (Fetched, Some(b)) = node.fetch_file(..);                                                           │  (actor
    node.blob_import_bytes(b).await?;         ───►  BlobStore::import_bytes(&self) (add_bytes + blake3)  ───┘  serializes)
    // hash already referenced by the file.shared we fetched by ⇒ Gate 2 already allows it ⇒ serveable now
```

- **No transport touched.** `blob_import*` never calls `bind`/`connect`/`shutdown`;
  the endpoint, router, engine pump, and every peer link stay up ⇒ zero `ConnEvent`
  churn (AC1).
- **No second opener.** Import rides the node's existing `BlobStore` handle ⇒ no
  `BlobError::Locked`, no cycle.
- **`file.shared` authoring stays the consumer's job**, via the already-public
  `build_file_shared` + `Node::publish`.

---

## 4. Detailed implementation steps

### Step 0 — Confirm the `iroh-blobs 0.103.0` `add_bytes` surface

Mirror the durable-store confirmation the module already did for `add_path`
(`blob/mod.rs:15-36`). Before coding `import_bytes`, confirm on the 0.103.0 source:

1. `store.blobs().add_bytes(bytes)` (or the equivalent) content-addresses in-memory
   bytes and returns a `TagInfo { hash, .. }` on await.
2. **Durability parity:** the resulting tag is **persistent** (not GC-eligible),
   exactly like `add_path`'s tag (`blob/mod.rs:24-27`). If `add_bytes`'s default is
   a temporary tag, use the persistent-tag variant (e.g. `add_bytes_with_opts` with a
   named/persistent tag) so a re-provided blob survives GC and restart. **This is a
   correctness requirement, not a nicety** (see Risk R2).

Record the confirmed call shape in the `import_bytes` doc comment, as the module
doc does for `add_path`.

### Step 1 — `BlobStore::import_bytes` (`crates/iroh-rooms-net/src/blob/mod.rs`)

Add a sibling to `import_path` (`blob/mod.rs:134`), same belt-and-suspenders shape:

```rust
/// Import in-memory bytes into the durable store, returning the content hash and
/// byte length. The in-session analog of [`BlobStore::import_path`] for
/// re-providing fetched bytes (issue #84 / IR-0308).
///
/// Like `import_path`, the store computes the content hash during import and this
/// method **independently** recomputes BLAKE3-256 over the same bytes and asserts
/// they agree (an internal-bug guard, not a trust boundary). The blob is durably
/// persisted (persistent tag), so this node becomes a restart-surviving provider.
///
/// # Errors
/// [`BlobError::Import`] if the store import fails, or [`BlobError::HashMismatch`]
/// if the store hash and the independent recompute disagree.
pub async fn import_bytes(&self, bytes: bytes::Bytes) -> Result<BlobImport, BlobError> {
    let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let computed = *blake3::hash(&bytes).as_bytes();      // in-memory: no spawn_blocking needed
    let tag = self
        .store
        .blobs()
        .add_bytes(bytes)                                 // Step 0: persistent-tag variant
        .await
        .map_err(|e| BlobError::Import(e.to_string()))?;
    let store_hash = *tag.hash.as_bytes();
    if computed != store_hash {
        return Err(BlobError::HashMismatch {
            store: hex::encode(store_hash),
            computed: hex::encode(computed),
        });
    }
    Ok(BlobImport { hash: store_hash, size_bytes })
}
```

Notes:
- `bytes` is hashed **before** the move into `add_bytes` (no re-read; the buffer is
  already resident). No `spawn_blocking` — hashing an in-RAM buffer up to the
  100 MiB share cap is fine off a blocking pool, and there is no file IO to offload.
  (If a reviewer prefers symmetry with `import_path`'s blocking hash, wrap the
  `blake3::hash` in `spawn_blocking` over a clone; call it out in the PR.)
- Reuse the existing `BlobImport` struct (`blob/mod.rs:70-77`) verbatim.

### Step 2 — `BlobError::NotServing` (`crates/iroh-rooms-net/src/blob/mod.rs`)

`blob_import*` is only meaningful on a session that opened a store (`spawn_room`
with `BlobServeConfig`). A node without one (`Node::spawn`, or `spawn_room` with
`blob = None`) has `blob_store == None`. Add a coded variant to the `#[non_exhaustive]`
`BlobError` enum (`blob/mod.rs:227-253`) so the failure is a stable, greppable code
like its siblings:

```rust
/// This session does not own a durable blob store, so it cannot import — the
/// node was spawned without a `BlobServeConfig` (issue #84). Distinct from
/// `Locked`: nothing is holding a store, there simply is none in this session.
NotServing,
```

and its `Display` arm (`blob/mod.rs:255-274`):

```rust
Self::NotServing => write!(
    f,
    "blob_not_serving: this session does not serve blobs; spawn the room with a \
     BlobServeConfig to import in-session"
),
```

Because `BlobError` is already re-exported (`lib.rs:68`, façade `experimental/blob.rs:4`),
this needs no further surface work.

### Step 3 — `Node::blob_import` + `Node::blob_import_bytes` (`crates/iroh-rooms-net/src/node.rs`)

Add two `&self` methods next to `fetch_file` (`node.rs:868`). They borrow the
node-owned handle and delegate — **no pump, no `Cmd`, no transport**:

```rust
/// Import a file into the durable store this session already owns (issue #84 /
/// IR-0308), returning the verified content ref. Unlike the CLI's
/// open→import→close, this reuses the live session's store handle: no second
/// `FsStore` open (so no `BlobError::Locked`) and **no session cycle** — the
/// endpoint, engine pump, and every peer link stay up (zero `ConnEvent` churn).
///
/// Pair with `build_file_shared` + [`Node::publish`] to announce the reference;
/// the serve ACL's Gate 2 begins allowing the new hash on the next fold drive.
///
/// # Errors
/// [`BlobError::NotServing`] if this session opened no blob store (spawned without
/// a `BlobServeConfig`); otherwise the `import_path` errors (`Import`, `Read`,
/// `HashMismatch`).
pub async fn blob_import(&self, path: &Path) -> Result<BlobImport, BlobError> {
    self.blob_store
        .as_ref()
        .ok_or(BlobError::NotServing)?
        .import_path(path)
        .await
}

/// [`Node::blob_import`] from in-memory bytes — re-provide fetched bytes
/// in-session so a long-running consumer becomes a provider without restarting.
/// After a `fetch_file` that returned `(FetchOutcome::Fetched, Some(bytes))`, the
/// bytes' hash is already the one the `file.shared` references, so once imported
/// this node serves it immediately (Gate 2 already allows the referenced hash).
///
/// # Errors
/// [`BlobError::NotServing`] if this session opened no blob store; otherwise the
/// `import_bytes` errors (`Import`, `HashMismatch`).
pub async fn blob_import_bytes(&self, bytes: Bytes) -> Result<BlobImport, BlobError> {
    self.blob_store
        .as_ref()
        .ok_or(BlobError::NotServing)?
        .import_bytes(bytes)
        .await
}
```

- `Path` (`std::path::Path`), `Bytes` (`bytes::Bytes`), `BlobImport`, and
  `BlobError` are already imported in `node.rs` (`node.rs:21`, `:26`, `:42`).
- **Concurrency:** `blob_store` is `&self`-shared; concurrent `blob_import` calls
  and the serving `BlobsProtocol` all funnel to the one store actor, which
  serializes them (§2.2). No `&mut`, no lock in `Node`.
- **Return type — decision:** return `BlobImport` (hash `[u8; 32]` + `size_bytes`),
  not the issue's `(HashRef, u64)`. `BlobImport` is the store wrapper's own return
  type, already re-exported through the façade (`experimental/blob.rs:4`); a
  consumer wraps `HashRef::from_bytes(imp.hash)` when authoring the `file.shared`.
  Keeps the two Node methods a pure pass-through and avoids importing the core
  `HashRef` into `net`'s blob wrapper. (Key decision §11.4.)

### Step 4 — Façade surface (`crates/iroh-rooms/src/experimental/session.rs`)

- `Node` is already re-exported (`session.rs:15-20`), so both methods ride along —
  **no new re-export**. Add `blob_import` / `blob_import_bytes` to the module-doc
  method list (`session.rs:6-8`).
- `BlobImport` and `BlobError` are already surfaced via `experimental/blob.rs:4`.
- **`Bytes` naming:** the façade does not re-export `bytes`; a consumer names
  `blob_import_bytes`'s argument via their own `bytes::Bytes` dependency (the fetched
  bytes come back as `Bytes` from `Node::fetch_file` already). Add a one-line
  pointer in the `blob.rs` / `session.rs` module doc, mirroring how `room_events`
  documents naming `tokio::sync::broadcast` (`session.rs:10-13`). **Do not**
  re-export `bytes` from the façade.

### Step 5 — Docs

- `README.md` "Current Status": add a per-issue changelog entry in the established
  pattern (cf. the #83 / #88 / #85 entries), e.g. *"In-session blob import (issue
  #84 / IR-0308): `Node::blob_import` / `blob_import_bytes` import into the live
  session's store — resident daemons share files and re-provide fetched bytes with
  **no** session cycle (zero peer disconnects)."*
- `docs/getting-started.md` (or `docs/sdk-coverage.md`): a short "Share a file from
  a live session" + "Re-provide a fetched file" snippet under the library section,
  showing `blob_import` → `build_file_shared` → `publish`, and `fetch_file` →
  `blob_import_bytes`. Note the daemon must `spawn_room` with a `BlobServeConfig`.
- `crates/iroh-rooms/CHANGELOG.md`: additive-API entry.
- `RELEASE-READINESS.md`: if it enumerates real-QUIC-loopback façade e2e tests, add
  the new one (§7.4). Prose-only; no test asserts the literal count.

---

## 5. API / data-model impact

| Surface | Change | Breaking? |
|---|---|---|
| `BlobStore::import_bytes(&self, Bytes) -> Result<BlobImport, BlobError>` | **new** | additive |
| `BlobError::NotServing` (variant on `#[non_exhaustive]` enum) | **new** | additive (enum is `#[non_exhaustive]`, `blob/mod.rs:228`) |
| `Node::blob_import(&self, &Path) -> Result<BlobImport, BlobError>` | **new** | additive |
| `Node::blob_import_bytes(&self, Bytes) -> Result<BlobImport, BlobError>` | **new** | additive |
| `iroh_rooms::experimental::session::Node` | two methods ride existing re-export | additive |
| `BlobImport` / `BlobError` façade re-exports | unchanged (already exported) | — |

- **No new event type, no store-schema change, no wire-format change.** `file.shared`
  authoring is unchanged (`build_file_shared`); this only produces the hash/size the
  consumer feeds into it.
- **No `Node` struct field / `pump` signature change** — unlike #83, the blob store
  is already on `Node` and off the pump (§2.2).
- **No validation/authorization change.** Import writes bytes into the local store;
  it grants no new serve permission. A blob is served only when Gate 1 (active
  member) **and** Gate 2 (referenced by a `file.shared`) both pass (§6.4), exactly
  as today.

---

## 6. Semantics, correctness & observability

### 6.1 Zero-disconnect guarantee (AC1)

`blob_import*` touch **only** the store actor. They never call `NetTransport::bind`,
`connect_to`, `disconnect_peer`, `Router::shutdown`, or drop the endpoint. The
event-plane QUIC links tracked by `ConnEvent` (`transport.rs`, surfaced via
`Node::conn_events`, `node.rs:491`) are entirely independent of the blob store, so
an import cannot produce a disconnect. Contrast the status quo: `shutdown()` calls
`Router::shutdown` and closes the endpoint (`node.rs:914-923`), which every peer
observes as a link drop. Removing the cycle removes the churn — provable by
subscribing to `conn_events` across an import + publish and asserting no
`Connected → Offline/Unauthorized` transition (§7.4, AC1).

### 6.2 In-session import is durable and content-addressed

`import_path`/`import_bytes` persist via a persistent `iroh-blobs` tag (Step 0
guarantees parity for bytes), so `has(hash)` is true immediately and survives
restart — identical durability to the CLI's open→import→close, minus the cycle. The
independent BLAKE3-256 recompute guard is preserved on both paths
(`blob/mod.rs:144-157` for path; Step 1 for bytes).

### 6.3 The share flow, with no cycle

The daemon sequences three already-public calls:

```rust
let imp  = node.blob_import(&abs_path).await?;                 // in-session import
let wire = build_file_shared(.., HashRef::from_bytes(imp.hash), imp.size_bytes, .. heads ..);
node.publish(wire.to_bytes()).await?;                          // fanout to live peers
```

**Ordering:** import **before** publish. After both, a peer fetch succeeds because
(a) the store `has()` the bytes (import) and (b) Gate 2 references the hash and the
peer has learned the reference (publish). Publishing the `file.shared` first would
briefly reference a hash the store doesn't hold yet — a transient `DeniedPerHash`/
`Unavailable` for a racing fetcher. The daemon controls the order; document it.

### 6.4 Gate 2 refresh — no extra wiring

`Node::publish` → `handle_cmd`'s `Cmd::Publish` arm → `room.maybe_reconcile(engine)`
(`node.rs:1103`), which recomputes `engine.file_shared_hashes()` and swaps a fresh
`BlobAclView` into the live cell (`node.rs:128-140`). So the just-published
`file.shared`'s hash enters Gate 2 on that same drive — the serve gate allows it
without any change here. **Re-provide case:** the fetched blob's hash is *already*
referenced by the `file.shared` the consumer fetched by, so after
`blob_import_bytes` the node serves it immediately — no new `file.shared`, no
reconcile needed (AC2).

### 6.5 `NotServing` is honest and coded

Calling `blob_import*` on a non-serving node (no `BlobServeConfig`) returns
`BlobError::NotServing` with a stable `blob_not_serving:` prefix, distinct from
`Locked` (something else holds a store) — there simply is no store in this session.
Mirrors the existing coded-error taxonomy (`blob/mod.rs:255-274`).

### 6.6 Observability

No new counters. The blob-plane audit sink already records serve decisions
(`BlobDenyCause`, `spawn_blob_gate`); an import is a local store write with nothing
to gate. Optionally a `tracing::debug!(hash, size)` in each `Node` method (off any
hot path). The consumer sees the result directly via the returned `BlobImport`.

---

## 7. Test strategy

Most coverage is cheap (store-only, no network); one headline e2e proves the
zero-disconnect + serve-in-session claims over real loopback QUIC. Mirror the
existing `blob/mod.rs` unit tests and `tests/blob_e2e.rs` harness.

### 7.1 `BlobStore::import_bytes` unit tests — `crates/iroh-rooms-net/src/blob/mod.rs` `#[cfg(test)]`

Mirror the `import_path` tests (`blob/mod.rs:346-508`):

1. `import_bytes_hash_equals_independent_blake3_and_size_is_correct` — import
   `b"the quick brown fox…"`; assert `hash == blake3::hash(content)` and
   `size_bytes == content.len()`.
2. `has_is_true_after_import_bytes` — `has(imp.hash)` true; `has([0;32])` false.
3. `empty_bytes_import_is_held` — 0-length `Bytes` imports, `size_bytes == 0`, hash
   is the well-known empty-BLAKE3 digest, `has` true.
4. `import_bytes_provider_status_survives_reopen` — import via bytes, `close()`,
   reopen the same dir, assert `has(hash)` still true (durability / persistent-tag —
   guards the Step 0 requirement; **fails loudly if `add_bytes` used a temp tag**).
5. `import_path_and_import_bytes_agree_on_hash` — write bytes to a file, import via
   both paths, assert identical `hash` (content-addressing is source-independent).

### 7.2 `Node` method tests — non-serving node returns `NotServing`

A `Node::spawn` node (no store) — no endpoint traffic needed:

1. `blob_import_on_non_serving_node_reports_not_serving` — assert
   `matches!(err, BlobError::NotServing)`.
2. `blob_import_bytes_on_non_serving_node_reports_not_serving` — same for the bytes
   method.

(These can spawn a minimal loopback `Node::spawn` and immediately call the methods;
no peer / no blob config.)

### 7.3 `BlobError` display test

Extend `error_display_strings_carry_stable_codes` (`blob/mod.rs:454`) with
`BlobError::NotServing` → starts with `blob_not_serving:`.

### 7.4 Headline e2e — `crates/iroh-rooms-net/tests/blob_import_live_e2e.rs` (new)

Reuse `blob_e2e.rs`'s `Principal` / `build_two_member_room` / `spawn_provider`
fixtures (a `spawn_room` node with `BlobServeConfig`). `#[ignore]` online tier if it
exceeds the default-CI budget; otherwise keep it always-green like `blob_e2e`.

**AC1 — share in-session, zero disconnects, peer fetches the new blob:**
`share_over_live_session_no_disconnect_peer_fetches`

1. Alice = `spawn_provider` (serving, empty store beyond genesis room). Bob = a
   second **serving** `spawn_room` node. Dial them together (`connect_to` /
   addr hints) and wait until Bob reaches `Connected` on Alice
   (`wait_for_state`).
2. Subscribe `let mut conn = alice.conn_events();` **before** the import.
3. Alice: `let imp = alice.blob_import(&abs_path).await?;` then build a `file.shared`
   referencing `imp.hash`/`imp.size_bytes` parented on current heads
   (`alice.heads()`), then `alice.publish(wire).await?`.
4. Assert **no disconnect**: drain `conn` (or poll `alice.peer_state(bob_id)`) and
   assert no `Connected → Offline/Unauthorized` transition occurred; Bob stays
   `Connected` throughout.
5. Bob (or a third member fetcher) `fetch_file(alice_addr, imp.hash, imp.hash, …)` →
   `FetchOutcome::Fetched`, bytes equal the original content — over the **same**
   uninterrupted session (Alice never cycled).

**AC2 — fetched bytes re-provided in-session make the fetcher a provider:**
`fetched_bytes_reprovided_in_session_serves_third_peer`

1. Three active members: Alice (origin provider, `spawn_provider` seeded with the
   blob + `file.shared`), Bob (serving `spawn_room`), Carol (serving `spawn_room`).
2. Bob `fetch_file(alice_addr, hash, hash, …)` → `Fetched`, `Some(bytes)`. Assert
   `bob.blob_import_bytes(bytes.clone()).await?.hash == hash`.
3. **Without restarting Bob**, Carol `fetch_file(bob_addr, hash, hash, …)` →
   `Fetched` — Bob now serves the blob it fetched. (Gate 1: Carol active; Gate 2:
   hash already referenced by the synced `file.shared`; store: `has(hash)` true from
   the re-import.) Optionally also assert Alice can be shut down first, proving Bob
   is a genuine independent provider.

### 7.5 Façade e2e — `crates/iroh-rooms/tests/facade_e2e.rs`

`blob_import_through_facade_reprovides` — using **façade-only imports**
(`iroh_rooms::experimental::session::Node`, `experimental::blob::BlobImport`), spawn
two serving nodes over real loopback QUIC, fetch a blob on one, `blob_import_bytes`
it, and assert a third façade node fetches it back. Proves the online tier works
through the public façade (the pattern `facade_e2e.rs` already establishes).

### 7.6 Surface tripwire — `crates/iroh-rooms/tests/experimental_surface.rs`

Compile-only fn-pointer signature locks:
```rust
let _: fn(&session::Node, &std::path::Path)
    -> /* impl Future of */ Result<blob::BlobImport, blob::BlobError> = session::Node::blob_import;
```
(async-fn pointers are matched via the same technique the existing surface tests
use; adapt to house style — the point is to pin the signature so a future refactor
that changes the return type fails to compile.)

### 7.7 Gate

Full `verify.sh` (`verify-sh-is-the-real-ci-gate`): `cargo fmt --check`, `clippy -D
warnings` (pedantic), all-features tests, `-p iroh-rooms --doc`, examples build. The
`#[ignore]` online e2e tiers run under the P0 online gate (`release-readiness.sh`,
`ir-0306-release-readiness-scope`), not default `verify.sh`.

---

## 8. Acceptance criteria

- **AC1 (share, no cycle, no churn).** With ≥1 peer connected, a `blob_import` +
  `file.shared` publish produces **zero** `ConnEvent` disconnects; the peer stays
  `Connected` and fetches the new blob from the same uninterrupted session (§7.4
  AC1).
- **AC2 (re-provide in-session).** After `fetch_file` returns verified bytes,
  `blob_import_bytes` makes the fetcher a provider *without restart*: a third member
  fetches the blob from the fetcher (§7.4 AC2, §7.5).
- **AC3 (no second opener / no lock).** `blob_import*` never call `FsStore::load`
  and never return `BlobError::Locked`; they reuse the node-owned handle (§2.1).
- **AC4 (honest non-serving error).** `blob_import*` on a node with no
  `BlobServeConfig` return `BlobError::NotServing` (coded `blob_not_serving:`),
  never a panic or a hang (§7.2/§7.3).
- **AC5 (durability + integrity).** Imported bytes persist (persistent tag, survive
  reopen) and pass the independent BLAKE3-256 recompute; a store/recompute
  disagreement is `HashMismatch` (§7.1.4/§7.1.1).
- **AC6 (façade).** Both methods are reachable and functional via
  `iroh_rooms::experimental::session::Node` over real loopback QUIC (§7.5/§7.6).
- **AC7 (no regression).** `verify.sh` green; existing serve/fetch/`conn_events`/
  `spawn_room` behavior unchanged (the ACL and shutdown paths are untouched).
- **AC8 (consumer win, informational).** Bantaba drops its shutdown→open→import→
  close→respawn cycle for `file share` and gains in-session re-provide.

---

## 9. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| **R1** | Concurrent `blob_import` (add) while `BlobsProtocol` serves (get) races on the store. | The `iroh-blobs` store is a single actor with an internal command queue; concurrent add/get are serialized by design (§2.2). Exercised by §7.4 AC2 (Bob imports while it may be serving). |
| **R2** | `add_bytes`'s default tag is **temporary**, so a re-provided blob is GC-eligible / doesn't survive restart — silently degrading availability, the exact thing the feature strengthens. | Step 0 confirms and, if needed, uses the persistent-tag variant; §7.1.4 (reopen-still-holds) is a regression tripwire that fails loudly if a temp tag slips in. |
| **R3** | A consumer publishes `file.shared` **before** `blob_import`, so a racing fetcher gets `DeniedPerHash`/`Unavailable`. | Document the import-before-publish order (§6.3); the returned `BlobImport` naturally sequences it (you need `imp.hash` to author the reference). |
| **R4** | Import on a non-serving node is a silent no-op or panic. | Explicit `BlobError::NotServing` coded error (Step 2), tested (§7.2). |
| **R5** | Large in-memory `blob_import_bytes` (up to the 100 MiB share cap) holds the buffer + hashes on the async reactor. | Bytes are already resident (came from `fetch_file`); hashing 100 MiB is bounded and off the network hot path. If profiling flags it, wrap the `blake3::hash` in `spawn_blocking` (OQ-3). No size cap is enforced here — that policy lives in `file.shared` authoring (`MAX_SHARED_FILE_BYTES`), see OQ-2. |
| **R6** | Adding `BlobError::NotServing` breaks an exhaustive external `match`. | `BlobError` is `#[non_exhaustive]` (`blob/mod.rs:228`) — external matches already need a wildcard arm; additive and safe. |
| **R7** | Someone expects `blob_import` to also publish the `file.shared` (bundle). | Explicit non-goal (§1); documented on the method that it pairs with `build_file_shared` + `publish`. Keeps capabilities orthogonal and matches the CLI's separation. |
| **R8** | `blob_store` is dropped on `shutdown`; a late `blob_import` after shutdown. | `blob_import*` are `&self`; after `shutdown(self)` the node is consumed and unusable — the borrow checker prevents calling them on a moved value. No runtime guard needed. |

---

## 10. Security / privacy / reliability / performance

- **Security/privacy:** No new trust or network surface. Import is a **local** store
  write; it grants no serve permission. Serving still requires Gate 1 (active
  member) **and** Gate 2 (referenced by a `file.shared`) — unchanged
  (`blob/mod.rs:278-329`). A node cannot become a provider of a blob no `file.shared`
  references (importing bytes with no reference leaves them unserveable, §6.4). No
  event validation/authorization path is touched.
- **Reliability:** Removing the session cycle removes a systemic failure mode — every
  `file share` no longer risks a reconnect storm across all peers. The store actor
  bounds concurrency; a failed import returns a coded error and leaves the session
  running (no cycle to get stuck mid-way). Import-before-insert ordering keeps the
  crash-safety property the CLI already documents (an orphan blob is harmless /
  re-importable; a reference to absent bytes is not — `file.rs:151-153`).
- **Performance:** One `add_path`/`add_bytes` + one BLAKE3 recompute per import, on
  the already-open store actor, off the network hot path. Eliminates the far more
  expensive status quo: an endpoint teardown + rebind + full re-handshake with every
  peer per share. Net large win for a resident daemon.
- **Migration/rollback:** Purely additive API; no schema/wire/event-format change.
  Rollback = revert; no data migration. Consumers adopt incrementally — the old
  cycle still works, they just stop needing it.

---

## 11. Key decisions

1. **Two thin `&self` methods on `Node`, not a raw `Node::blob_store()` handle.**
   Keeps `iroh_blobs` types behind the wrapper (isolation invariant R1,
   `blob/mod.rs:10-13`); a leaked `BlobStore`/`FsStore` handle would let a consumer
   `close()` the store out from under the live session.
2. **No pump routing / no `Cmd` variant.** The blob store is a separate actor from
   the single-owner engine and is already held directly by `Node` off the pump
   (§2.2). Import is a direct delegating `&self` call — strictly simpler than the
   #83 push-subscription plumbing.
3. **Reuse the already-open store handle; never re-open.** Re-opening is the whole
   problem (exclusive lock → `Locked`). The one legal opener is the `Node`; route
   through it.
4. **Return `BlobImport`, not `(HashRef, u64)`.** It's the wrapper's own return
   type, already façade-exported; the consumer wraps `HashRef::from_bytes` when
   authoring. Pure pass-through, no core-type import into `net`'s blob wrapper.
5. **`file.shared` authoring stays the consumer's job** via existing public
   `build_file_shared` + `Node::publish`. Import and reference-authoring are
   orthogonal capabilities; the reconciler already refreshes Gate 2 on publish.
6. **New coded `BlobError::NotServing`**, distinct from `Locked`, on the already
   `#[non_exhaustive]` enum — honest, greppable, non-breaking.

---

## 12. Assumptions

- **`iroh-blobs 0.103.0` exposes a bytes import** (`add_bytes` or equivalent)
  returning a `TagInfo { hash, .. }`, with a persistent-tag option matching
  `add_path`'s durability. Confirmed in Step 0 before coding (mirrors the module's
  existing `add_path` confirmation, `blob/mod.rs:15-36`). If not, `blob_import_bytes`
  falls back to a temp-file-then-`import_path` shim (OQ-4) — `blob_import` (path) is
  unaffected either way.
- **The store actor tolerates concurrent add + serve** (§2.2 / R1) — the actor model
  guarantees it; validated by the §7.4 AC2 test.
- **The re-provide consumer is a `spawn_room` session with a `BlobServeConfig`.**
  A pure `Node::spawn` fetcher (as in `blob_e2e.rs`) has no store and gets
  `NotServing` — correct: a non-serving node re-providing would be a no-op it can't
  serve anyway.
- **Developer-preview / pre-1.0 stability:** additive API + a new `#[non_exhaustive]`
  enum variant are acceptable.
- **Import-before-publish ordering** is the caller's contract (§6.3); the returned
  `BlobImport` makes it natural.

---

## 13. Open questions

- **OQ-1 (return type).** Return `BlobImport` (proposed) or the issue's
  `(HashRef, u64)` for a slightly more ergonomic `build_file_shared` call site?
  *Proposed: `BlobImport`* — consistent with `BlobStore::import_path`, already
  façade-exported; wrapping `HashRef::from_bytes` is one line.
- **OQ-2 (size cap).** Should `blob_import*` enforce `MAX_SHARED_FILE_BYTES`, or
  leave the cap to `file.shared` authoring (as `file share` does today via
  `classify_path`, `file.rs:917`)? *Proposed: no cap in `net`* — keep the store
  primitive policy-free; the cap is a share-policy concern the consumer/CLI applies.
- **OQ-3 (hashing off-reactor).** Wrap `import_bytes`'s `blake3::hash` in
  `spawn_blocking` for symmetry with `import_path`? *Proposed: defer* — in-RAM
  hashing up to 100 MiB is bounded and off the network hot path; revisit if profiled.
- **OQ-4 (`add_bytes` fallback).** If 0.103.0 lacks a clean persistent `add_bytes`,
  implement `blob_import_bytes` as write-temp-file → `import_path` → delete-temp?
  *Proposed: prefer native `add_bytes`; shim only if Step 0 finds no persistent
  variant.*
- **OQ-5 (convenience `share` helper).** Offer an optional
  `Node::share_file(path, name, mime, …) -> file.shared event id` that bundles
  import + author + publish (the daemon's whole share in one call)? *Proposed:
  defer* — bundling couples two orthogonal capabilities (non-goal §1); ship the
  primitives first, add the convenience wrapper if a consumer asks.
- **OQ-6 (CLI `file share --into <live-session>`).** Should the CLI eventually import
  through a running `room tail` via an IPC/handle instead of open→import→close?
  *Proposed: out of scope* — the CLI is short-lived and has no live session to
  import into; this is a daemon-only affordance.
