# Spec: File fetch and verification (`iroh-rooms file fetch` + the blob serve plane)

| | |
|---|---|
| **Issue** | #29 — [IR-0204] Implement file fetch and verification |
| **Parent** | #3 |
| **Labels** | type/feature, type/security, area/cli, area/blob, priority/p1, risk/medium |
| **Dependencies** | #28 — [IR-0203] `file.shared` validation hardening (landed). #13 — [IR-0009] Prototype blob ACL path (landed: `crates/spike-blobs`; findings in `crates/spike-blobs/NOTES.md`). #26 — [IR-0201] Hardened recent-history sync (landed). Also builds directly on #27 — [IR-0202] File import into the blob store (landed: `iroh_rooms_net::BlobStore`, `iroh-rooms file share`). |
| **Traceability** | `PRD.v0.3.md` §15.6 (Share File — AC4/AC5/AC6: authorized fetch, content-hash verification, honest unavailable), §17.1 (DX timing targets), §9.2 (Blob Plane 6-step flow, steps 4–6), §14 (Availability Model). Spike: Blob Plane ACL confirmation (`crates/spike-blobs/NOTES.md`); `PHASE-0-SPIKE.md` Membership & Ordering §5 (blob serve gate), Event Protocol §7 (`file.shared`), §4 (BLAKE3-256). |
| **Status** | Planning — spec only. No production code changed by this document. |
| **Type** | Feature (the network/serve + consumer half of the Blob Plane: the follow-up IR-0202 §4.3 explicitly deferred). |

---

## 1. Summary

Implement the **serve + fetch** half of the Blob Plane, closing the file-sharing
journey that IR-0202 (#27) deliberately left open. Concretely:

1. **Serve** — wire the `iroh-blobs` serve ALPN onto the shipping node's shared
   `Router` with the spike's **two-gate ACL** (per-node membership admission at
   connect time + per-hash authorization at request time). An online node that holds
   a blob becomes a real provider of it to **active room members only**, for **only**
   the hashes referenced by a valid `file.shared`.
2. **Fetch** — a new consumer command
   `iroh-rooms file fetch <ROOM_ID> <FILE_ID>`: resolve the `file.shared` reference
   from the room log, discover an available provider from the event metadata, bring
   up a node, dial the provider's blob ALPN as an authorized member, transfer the
   blob, **independently recompute BLAKE3-256 and require it equals the declared
   `blob_hash`**, and save the verified bytes to an output path (or a configured
   downloads directory).
3. **Honest unavailability** — when no provider is online (or none holds the hash),
   the fetch fails **cleanly within a bounded timeout** with a distinct "unavailable"
   report, never a hang (PRD §14 / §18.2).

This is PRD §9.2 file-flow **steps 4–6** ("a peer wants the file" → "fetch from a
provider over the ACL-gated blob plane" → "verify the content hash, save locally").
It is the exact follow-up IR-0202 §4.3 named: *(a) add the `iroh-blobs` ALPN to the
shared `Router` with the two-gate ACL, (b) add `file fetch` with the independent
BLAKE3 recompute, (c) optionally broadcast the `file.shared` frame at share time,
(d) map "no provider online" to honest "unavailable" language.*

The blob-ACL spike (#13 / `crates/spike-blobs`) already proved the serve gate and the
verified fetch against `iroh-blobs 0.103.0` / `iroh 1.0.1` (the exact pins the shipping
`iroh-rooms-net` already carries). This issue lifts that spike into production, driven
by the **real** membership fold and the **real** `file.shared` reference — no new
authorization model, event schema, or crate version.

This document is detailed enough to execute without re-deriving scope.

---

## 2. Background & current repository state

**Read before starting:**

- `crates/spike-blobs/NOTES.md` and `crates/spike-blobs/src/net.rs` — the confirmed
  serve-gate wiring and the verified-fetch client. `spawn_event_gate` is the exact
  two-gate loop to lift (`EventMask { connected: Intercept, get: Intercept,
  get_many: Intercept, push: Disabled, observe: None }` over
  `EventSender::channel(cap, mask)` handed to `BlobsProtocol::new(&store,
  Some(events))`; `ClientConnected` → active-member check; `GetRequestReceived` /
  `GetManyRequestReceived` → referenced-hash check; else `AbortReason::Permission`).
  `fetch_and_verify` is the exact client (`iroh_blobs::get::request::get_blob(conn,
  hash).bytes()`, wrapped in a bounded `timeout`, then independent
  `blake3::hash(&bytes)` compared to the declared hash), and `classify_get_failure` /
  `classify_get_error` map the getter error to the `FetchOutcome`
  (`Fetched | DeniedAtConnect | DeniedPerHash | HashMismatch | Unavailable`) using
  `iroh_blobs::protocol::ERR_PERMISSION`.
- `crates/spike-blobs/src/acl.rs` — the `AuthContext` (fold-shaped:
  `device_to_identity`, `active_members`, `referenced_hashes`;
  `is_active(device)` / `is_referenced(hash)`). This is the shape the production gate
  sources from the real fold + the real `file.shared` set.
- `specs/prototype-blob-acl-path.md` §5 (the serve gate) and §5.5 (the decision
  matrix — the authoritative accept/deny/verify table this issue must reproduce with
  the real fold).
- `specs/file-import-into-blob-store.md` §4.3 (the serve/fetch follow-up boundary —
  this issue) and §5.5 (`BlobStore`).
- `PRD.v0.3.md` §9.2, §15.6, §14; `PHASE-0-SPIKE.md` Membership §5, Event Protocol §7.

**Current repo state (relevant, already landed):**

- **`iroh_rooms_net::BlobStore`** (`crates/iroh-rooms-net/src/blob.rs`) is the durable
  local content store: `open(dir)`, `import_path(path) -> BlobImport { hash, size_bytes }`,
  `has(hash) -> bool`, `close()`. It wraps a persistent `iroh_blobs::store::fs::FsStore`
  at `<home>/blobs/` and isolates **all** `iroh_blobs` types behind the wrapper (only
  the raw `[u8; 32]` hash crosses the boundary). It has **no serve path and no fetch/get
  path yet** — that is this issue. `iroh-rooms-net/Cargo.toml` **already depends on**
  `iroh-blobs = "=0.103.0"` and `iroh = "=1.0.1"` (no new dependency needed).
- **`file.shared` is a landed, conformance-tested event type**
  (`iroh_rooms_core::event::content::FileShared`): `file_id: [u8; 16]`, `name`,
  `mime_type`, `size_bytes: u64`, `blob_hash: HashRef` (BLAKE3-256, `HashRef::as_bytes()
  -> &[u8; 32]`), `blob_format: Option<String>`, `providers: Option<Vec<DeviceKey>>`.
  IR-0203 hardened its stateless validation, so **only a semantically valid
  `file.shared` can ever reach the store** (invalid ones can never be persisted or
  listed). `SHORT_ID_LEN = 16`, `MAX_SHARED_FILE_BYTES = 104_857_600` (100 MiB),
  `MAX_FILE_PROVIDERS = 16` (`event/constants.rs`).
- **`iroh-rooms file share` / `file list`** (`crates/iroh-rooms-cli/src/file.rs`) are
  the landed import/read half. `file share` sets `providers = [self device_id]` and
  the CLI handle format `file_<32-hex>` (`file_handle`). `file list` already computes
  provider status via `BlobStore::has`.
- **Node / transport / Router ALPN wiring** (`crates/iroh-rooms-net`): one
  `iroh::Endpoint` + one `Router` serves multiple planes. `NetTransport::bind` chains
  `EVENT_ALPN` (always) and `PIPE_ALPN` (optional handler) via successive
  `.accept(...)`. Adding blob serving is a **third** `.accept(iroh_blobs::ALPN, …)` on
  the same router — the established one-endpoint-many-planes pattern (spike serve
  proved `Router::builder(ep).accept(iroh_blobs::ALPN, blobs)`).
- **`Node`** owns the single-owner engine pump; it already exposes read seams routed
  through the pump (`snapshot`, `heads`, `room_tail`, and the `PipeQuery` per-request
  reads via `serve_pipe_query`). The managed `spawn_room` path keeps a live
  `AdmissionView` cell refreshed by the `RoomReconciler` on every fold change (the
  exact shape the blob serve gate's active-member predicate needs). `Node::endpoint()`
  exposes the iroh `Endpoint` for an out-of-band dial (used by `pipe_connect`).
- **The consumer command pattern is `pipe connect`** (`crates/iroh-rooms-cli/src/pipe.rs`):
  bring up an unmanaged `Node::spawn`, dial the room's active members, **wait for the
  governing announcement to sync** (`pipe.opened`, bounded `SYNC_WAIT = 10s`), resolve
  the owner's dialable address (`resolve_owner_addr`: a matching `--peer` else a bare
  `EndpointId`), then drive an out-of-band connect on the shared endpoint. `file fetch`
  is the same shape with a blob transfer + verify in place of the loopback splice.
- **Shared online helpers** (`crates/iroh-rooms-cli/src/message.rs`, re-used by every
  online command): `fold_room`, `select_heads`, `build_admission`, `build_dial_set`,
  `net_mode`, `parse_peers`, `render_endpoint_addr`, `endpoint_id_of`, `parse_timeout`.
  Reuse them; do not duplicate.
- **`crates/iroh-rooms-net/tests/file_e2e.rs`** already proves a `file.shared`
  **event** crosses the network (build → publish → QUIC loopback → fold → `room_tail`)
  and that an over-cap one is rejected on both send and receive. It does **not** move
  blob **bytes** — that gap is this issue.
- Workspace lints are strict (`unsafe_code = "forbid"`, clippy `all` + `pedantic`);
  `scripts/verify.sh` runs fmt `--check`, clippy `-D warnings`, and `--all-features`
  tests. It is the real CI gate — `cargo test` passing is necessary but not sufficient.
- The CLI installs **no `tracing` subscriber** (`cli-has-no-tracing-subscriber`
  memory): any operator-facing serve/fetch signal must go to stdout/stderr via an
  audit sink, not `tracing`.
- `FsStore` holds an **exclusive on-disk lock** while open (`fsstore-exclusive-lock-needs-shutdown`
  memory): never open `<home>/blobs/` twice concurrently in one process; call
  `close()`/`shutdown()` before a same-process reopen. `add_path` needs an **absolute**
  path (`blob-add-path-requires-absolute` memory).

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

Make a shared file **fetchable and verifiable** between two authorized peers:
`iroh-rooms file fetch <ROOM_ID> <FILE_ID>` retrieves the blob from an online,
authorized provider over the ACL-gated blob plane, **verifies its BLAKE3-256 against
the declared `blob_hash`**, saves it, and prints the saved path + verified hash — while
an unauthorized peer is denied at the room ACL path and an unavailable provider is
reported honestly.

### 3.2 In scope (this issue)

1. **Serve plane** — the `iroh-blobs` ALPN on the shared `Router`, gated by the
   two-gate ACL, sourced from the **real** membership fold (active members) and the
   **real** `file.shared` set (referenced hashes). Fail-closed on every lookup.
2. **Which sessions serve** — an online node that holds blobs serves them; the
   canonical provider-online surface is `room tail` (opt-in blob serving on the
   managed session, matching the getting-started demo where the provider keeps
   `room tail` running). See §5.3 / OQ-1.
3. **`file fetch <ROOM_ID> <FILE_ID>`** — the consumer command: active-member
   pre-check, resolve the `file.shared` reference (syncing it first if not local),
   discover a provider, dial + transfer + **independent BLAKE3 verify**, save,
   print saved path + verified hash.
4. **Provider discovery** from room-event metadata (`file.shared.providers`, default
   `[author device_id]`), resolved to a dialable address (`--peer` hint else discovery).
5. **Membership-gated fetch** — the fetcher must be an active member (pre-checked and
   cryptographically enforced by the provider's connect gate).
6. **Content verification** — receiver-side BLAKE3-256 recompute equals the declared
   `blob_hash`; a mismatch is rejected, reported, and **nothing corrupt is left on
   disk**.
7. **Save** to `--out <PATH>` or a configured downloads directory, with a **sanitized,
   traversal-safe** filename derived from the peer-supplied `name`.
8. **Honest unavailable** reporting within a bounded timeout.
9. A stable, greppable, secret-free audit + output surface; a two-peer integration
   test suite (valid / wrong hash / unavailable / unauthorized) + net/CLI unit tests.

### 3.3 Out of scope / non-goals

- **Any change to the `file.shared` wire schema** — landed and conformance-tested.
- **`hash_seq` / collection blobs.** Only `blob_format = "raw"` (single blob) is
  fetched; a `hash_seq` reference is rejected with a clear "unsupported format" error
  (the per-hash gate's child-hash authorization is a documented follow-up, spike
  NOTES §6 / OQ-6).
- **Per-blob revocation, blob GC / pinning / eviction, quotas** (Residual Risk #3).
- **A resumable / range fetch, parallel multi-provider racing, provider ranking.**
  MVP tries providers sequentially with a bounded per-attempt timeout.
- **An always-on / archive provider.** Availability follows the providers (PRD §14) —
  a file is fetchable only while a holder is online.
- **Broadcasting `file.shared` at share time (IR-0202 §4.3c)** is treated as an
  optional convenience (OQ-3); the reference already propagates via the landed sync
  engine, and `file fetch` syncs it on demand. Recommend deferring the share-time push
  to keep `file share` offline.
- **Fetching into a multi-file directory or a batch `file fetch --all`.**

---

## 4. Placement, dependencies, and the serve/fetch split

### 4.1 Where the code lives

```
crates/iroh-rooms-net/
  src/blob.rs               # EXTEND: keep the import wrapper; add the serve gate
                            #   (spawn_blob_gate over provider::events) + the verified
                            #   fetch client (fetch_blob -> FetchOutcome) + BlobAclView.
                            #   Split into a `blob/` module (mod.rs + serve.rs + fetch.rs)
                            #   if blob.rs grows unwieldy — all iroh_blobs types stay here.
  src/node.rs               # EXTEND: optional blob-serve plane on spawn_room/spawn;
                            #   a BlobQuery seam (is-referenced) routed through the pump;
                            #   Node::fetch_file(providers, hash, declared, timeout).
  src/transport.rs          # EXTEND: NetTransport::bind chains iroh_blobs::ALPN when a
                            #   blob-serve handler is supplied (mirrors the pipe handler).
  src/audit.rs              # EXTEND: AuditSink gains blob.serve.* vocabulary (see §7).
  src/lib.rs                # re-export the new public types (FetchOutcome, BlobFetch, …).
  tests/blob_e2e.rs         # NEW: two-peer serve/fetch (valid / wrong-hash / unauth /
                            #   unavailable) — the always-green Node-level backstop.

crates/iroh-rooms-cli/
  src/file.rs               # EXTEND: fetch(...) orchestration; find_file_shared;
                            #   resolve_providers; output-path resolution + name sanitize.
  src/cli.rs                # NEW `FileAction::Fetch { room_id, file_id, out, peers,
                            #   timeout, loopback }` + dispatch.
  src/message.rs (tail)     # EXTEND: room tail enables blob serving for the session.
  tests/file_cli.rs         # EXTEND: fetch CLI cases (args, output path, save+verify,
                            #   unsupported format, unavailable) + a gated online tier.
```

Rationale for the split (mirrors IR-0202 §4.1):

- **The blob store, serve gate, and fetch client are `iroh`/`iroh-blobs` concerns** →
  `iroh-rooms-net`, which already owns the transport + `BlobStore`. Keep every
  `iroh_blobs` type behind `net::blob` so a version bump touches one module.
- **Authorization is fold-derived** → the gate's active-member predicate reads the
  same `MembershipSnapshot`/`AdmissionView` the pump already maintains; the
  referenced-hash predicate reads the room's `file.shared` set from the store/engine.
  No authorization logic lives in the CLI.
- **The CLI orchestrates**: resolve the reference → discover a provider → drive
  `Node::fetch_file` → verify (done in net) → save + print. Same shape as `pipe connect`.

### 4.2 Dependencies

- **No new crate.** `iroh-blobs = "=0.103.0"` and `iroh = "=1.0.1"` are already
  production deps of `iroh-rooms-net` (added in IR-0202). Keep net clippy-clean under
  the workspace `pedantic` lints (do not relax them).

### 4.3 The serve/fetch boundary is *closed* here

IR-0202 imported + referenced but did not serve or fetch. This issue lands **both**
halves of the deferred network work (serve ALPN + `file fetch`), so after it merges the
getting-started Step 5 `file fetch` block becomes runnable and its "illustrative" note
is removed. The only Blob-Plane items still deferred are the explicit non-goals in §3.3
(`hash_seq`, revocation, GC, always-on providers), each tracked separately.

---

## 5. Design

### 5.1 Command surface

```text
iroh-rooms [--data-dir <PATH>] file fetch <ROOM_ID> <FILE_ID>
    [--out <PATH>] [--peer <ENDPOINT_ID>[@<ip:port>]]… [--timeout <DUR>] [--loopback]
```

- `<FILE_ID>` accepts the `file_<32-hex>` handle printed by `file share` / `file list`
  (and, tolerantly, bare 32-hex). Parsed by the inverse of `file_handle`.
- `--out <PATH>` — save target. If it names an existing directory (or ends in a path
  separator), the file is written as `<dir>/<sanitized name>`; otherwise it is the
  exact output file path. Omitted ⇒ the configured downloads directory (§5.6).
- `--peer` — repeatable dial hint (same parser as every online command,
  `message::parse_peers`), used for deterministic loopback/LAN addressing of the
  provider; without it, a bare `EndpointId` relies on discovery.
- `--timeout <DUR>` — the per-attempt bound for connect + transfer (`message::parse_timeout`,
  e.g. `30s`). Default larger than the message/pipe default because a transfer can be
  large; propose `--timeout 30s` (OQ-4).
- `--loopback` — hidden CI/test stack flag (`net_mode`), identical to the other online
  commands.

`file share` / `file list` are unchanged except: `file list`'s next-step hint and the
getting-started doc are updated to point at `file fetch` (no longer "once serve/fetch
lands").

### 5.2 Data flow for `file fetch` (happy path)

```
1. Parse ROOM_ID + FILE_ID + --out/--peer/--timeout (pre-IO). Bad args ⇒ exit non-zero.
2. Load signing secrets (identity + device). No identity ⇒ clear error.
3. Open <home>/rooms.db; fold the room; require the caller is an ACTIVE member
   (else the "only an active member can fetch files" error). Mirrors pipe connect.
4. Resolve the file.shared reference for FILE_ID from the local log (find_file_shared):
     - present locally ⇒ use it;
     - absent ⇒ bring up the node (step 6) and wait (bounded) for the reference to sync,
       exactly like `pipe connect` waits for `pipe.opened` (SYNC_WAIT). Still absent
       after the wait ⇒ "unknown file … (has it been shared / synced?)".
   Reject blob_format other than "raw"/absent with an "unsupported format" error.
5. Discover providers: read file.shared.providers (default: [event.device_id] when
   absent/empty), resolve each DeviceKey -> EndpointId, and build a dial address for
   each (a matching --peer else a bare EndpointId). This is the resolve_owner_addr
   pattern generalized to a list (resolve_providers).
6. Bring up an ephemeral unmanaged Node (Node::spawn), admission = build_admission(snapshot),
   dial the room's active members (build_dial_set) so the reference can sync and so the
   provider sees us as a connected member. (The provider's ACL is authoritative; our
   dial set just gives the fold something to converge on.)
7. For each candidate provider (in order), with a bounded per-attempt timeout:
     Node::fetch_file(provider_addr, blob_hash, declared=blob_hash, timeout) ->
       FetchOutcome + Option<bytes>.
     - Fetched         ⇒ break with the verified bytes.
     - DeniedAtConnect ⇒ record "denied by <provider> (not an active member?)", try next.
     - DeniedPerHash   ⇒ record "provider will not serve this hash", try next.
     - HashMismatch    ⇒ HARD FAIL immediately (integrity): report + exit non-zero,
                          write nothing (AC2). Do not silently try another provider —
                          a lying reference is a security signal, not a transient miss.
     - Unavailable     ⇒ record "unreachable", try next.
   If every provider is exhausted without a Fetched: report the honest aggregate
   ("file <id> is currently unavailable: no provider holding it is online") and exit
   non-zero. Nothing is written.
8. Save: resolve the output path (§5.6), write the verified bytes atomically
   (temp file in the same dir + rename), and OPTIONALLY import the bytes into the local
   <home>/blobs/ store so this node becomes a provider too (§5.7 / OQ-5).
9. Print the FetchSummary: saved path, verified blake3:<hex> (== declared), size,
   provider dialed. Exit 0.
```

Verification (step 7 "Fetched") is done **inside** `Node::fetch_file` / the net
`fetch_blob` client: after the transfer assembles the bytes, it recomputes
`blake3::hash(&bytes)` and compares to the declared hash; only an equal hash yields
`Fetched`, otherwise `HashMismatch`. `iroh-blobs` additionally performs **bao verified
streaming** against the requested hash during transfer, so tampered bytes for a given
hash are already rejected at the transport layer; the independent recompute is the
belt-and-suspenders AC that also catches a `file.shared` whose declared hash differs
from the bytes actually served (spike §5.3, NOTES §4).

### 5.3 The serve plane (two-gate ACL over the real fold)

The provider side is the spike's `spawn_event_gate`, re-pointed at production sources:

- **Gate 1 — per-node admission** (`ProviderMessage::ClientConnected`): accept iff the
  QUIC/TLS-authenticated `endpoint_id` resolves to an **active member**. The predicate
  reads the same live active-member view the managed pump already maintains (the
  `AdmissionView` cell refreshed by `RoomReconciler`), so a member removed mid-session
  stops being served within a tick — identical to the event-plane accept gate and the
  pipe gate. Fail-closed on an unknown/unauthenticated peer.
- **Gate 2 — per-hash authorization** (`ProviderMessage::GetRequestReceived` /
  `GetManyRequestReceived`): serve a hash only if it is **referenced by a valid
  `file.shared` in the room** (causally-visible; because IR-0203 guarantees only valid
  `file.shared` events persist, "referenced" = "a `file.shared` in the room store
  carries this `blob_hash`"). Else `AbortReason::Permission`. For `get_many`, every
  requested hash must be referenced.
- **Push / observe** stay denied (`push: Disabled`, `observe: None`) — a peer can
  never write to or enumerate the provider's store over the blobs ALPN.

**Source of the two predicates (the production re-point of the spike fixture).** A
small `BlobAclView` (the production analog of `spike-blobs::AuthContext`) holds:
`active_devices: HashSet<EndpointId>` and `referenced_hashes: HashSet<[u8;32]>`. The
pump's `RoomReconciler` computes it from the current snapshot + the room's `file.shared`
set and swaps it into a shared `Arc<Mutex<BlobAclView>>` cell on every fold change —
the same mechanism that already refreshes the `AdmissionView`. The gate loop reads the
cell per message. (Alternative: answer `is_referenced` per request via a pump-routed
`BlobQuery` read against the engine, mirroring `serve_pipe_query`; recommend the
refreshed cell for symmetry with admission, and note the per-request read as OQ-2.)

**Which node serves.** Blob serving is **opt-in** on a managed session via a new
`BlobServeConfig { blobs_dir: PathBuf }` passed to `Node::spawn_room`
(`None` ⇒ no blob plane, unchanged behavior). The Node opens the `BlobStore` for the
session lifetime, builds the `BlobsProtocol` from its store + the gate, and the
transport chains `iroh_blobs::ALPN` on the router. **`room tail` turns it on** (the
canonical "provider stays online" surface, matching getting-started Step 5 where Alice
runs `room tail` while Bob fetches). `file fetch`'s own node does **not** serve (it is
a pure consumer). See OQ-1 for a possible dedicated `file serve` command instead.

### 5.4 The fetch client (net)

Lift `spike-blobs::net::{fetch_and_verify, classify_get_failure, classify_get_error,
connection_denied_for_permission, FetchOutcome}` into `net::blob` essentially verbatim,
adapted to the production endpoint. Public shape:

```rust
/// Classified outcome of one fetch attempt against one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome { Fetched, DeniedAtConnect, DeniedPerHash, HashMismatch, Unavailable }

/// Fetch `fetch_hash` from `provider_addr` over the blobs ALPN, then require the
/// assembled bytes' BLAKE3-256 equals `declared_hash`. The whole attempt is bounded
/// by `timeout` so an offline provider yields `Unavailable`, never a hang.
pub async fn fetch_blob(
    endpoint: &iroh::Endpoint,
    provider_addr: iroh::EndpointAddr,
    fetch_hash: [u8; 32],
    declared_hash: [u8; 32],
    timeout: std::time::Duration,
) -> (FetchOutcome, Option<bytes::Bytes>);
```

`Node::fetch_file(provider_addr, hash, declared, timeout)` is a thin wrapper that dials
on the node's shared `Endpoint` (`Node::endpoint()`), exactly as `pipe_connect` drives
the connector on the shared endpoint. `fetch_hash` and `declared_hash` are separate
parameters (the CLI always passes both = `file.shared.blob_hash`); the split exists so
the AC2 test can drive a deliberate declared≠served mismatch deterministically
(§8, spike §5.3).

> **Step-0 gate (see §6.1):** confirm on `iroh-blobs 0.103.0` whether a
> **get-into-store** API exists (streaming the blob directly into the local `FsStore`,
> which both bounds memory and makes the fetcher a provider) vs. only the spike's
> `get_blob(conn, hash).bytes()` (whole blob buffered in memory). Either is acceptable
> for MVP (buffering is bounded by the 100 MiB cap), but prefer the store path if
> available. Whichever is chosen, the independent BLAKE3 recompute over the final bytes
> is required for AC.

### 5.5 Provider discovery

`file.shared.providers: Option<Vec<DeviceKey>>` names the asserted providers'
`device_id`s (== `EndpointId`s). Resolution (`resolve_providers`, the list
generalization of `pipe::resolve_owner_addr`):

- `providers = Some(non-empty)` ⇒ each `DeviceKey` → `EndpointId` → a dial address
  (a matching `--peer` for a deterministic loopback/LAN dial, else a bare `EndpointId`
  resolved by iroh discovery). Order is deterministic (as-listed).
- `providers` absent/empty ⇒ default to `[event.device_id]` (the author's signing
  device — the §7 default and the node that necessarily held the bytes at share time).
- Self is skipped (fetching from yourself is a no-op; if you already hold the blob,
  report it and short-circuit — you are already the provider).

The dial address of a provider is the same `EndpointAddr` shape the transport already
uses; the fetch dials the **blobs** ALPN on it (not the event ALPN).

### 5.6 Output path resolution + filename safety

- `--out <FILE>` (not a dir) ⇒ write exactly there (create parent dirs as needed;
  refuse to overwrite an existing file unless `--force`? — recommend **refuse without
  `--force`** to avoid clobbering, OQ-7).
- `--out <DIR>` (existing dir, or a value ending in `/`) ⇒ `<DIR>/<sanitized name>`.
- `--out` omitted ⇒ `<downloads>/<sanitized name>`, where `<downloads>` is
  `IROH_ROOMS_DOWNLOADS` (env) else `<home>/downloads/` (created `0700` on Unix via
  `paths::ensure_dir`). This is the "configured downloads path" the scope names.
- **`name` is peer-controlled** (`file.shared.name`), so the derived filename MUST be
  the sanitized **final path component only**: reject/strip any path separator, `..`,
  and NUL/control chars, and fall back to `file_<hex>` if the result is empty. This
  prevents a malicious `name` like `../../.ssh/authorized_keys` from escaping the
  target directory (a path-traversal guard — a security requirement, not a nicety).

### 5.7 Becoming a provider after fetch (recommended)

After a verified fetch, importing the bytes into the local `<home>/blobs/` store makes
this node a provider too (so `file list` then shows `you (local)` and the node can
re-serve). If Step 0 confirms a get-into-store API, the transfer already lands in the
store; otherwise `import_path`/`add_bytes` the verified bytes. Recommend doing this
(improves availability, matches the mesh model) but gate it behind the same size cap
and note it as OQ-5 (product may prefer fetch-only). The **on-disk lock** caveat
applies: the fetch node opens `<home>/blobs/` once; do not also open it elsewhere in
the same process.

---

## 6. Implementation steps

Work top to bottom; each step is independently reviewable and testable.

### 6.1 Step 0 — Confirm the `iroh-blobs 0.103.0` serve + get API (gates the rest)

On the confirmed `iroh-blobs 0.103.0` source (the spike already used it), re-confirm
and record in a `blob.rs` comment: (a) the `provider::events` types the serve gate uses
(`EventMask`, `EventSender::channel`, `ProviderMessage::{ClientConnected,
GetRequestReceived, GetManyRequestReceived, PushRequestReceived, ObserveRequestReceived,
Throttle}`, `AbortReason::Permission`, `ConnectMode`, `RequestMode`, `BlobsProtocol::new`,
`iroh_blobs::ALPN`) — all already exercised by `spike-blobs/src/net.rs`; (b) the getter
API (`iroh_blobs::get::request::get_blob`, `.bytes()`, `GetError::{iroh_error_code,
open, remote_read, remote_write}`, `iroh_blobs::protocol::ERR_PERMISSION`); (c) whether
a **get-into-`FsStore`** streaming API exists (preferred over buffering — §5.4). If the
provider-events connect hook exposes the proven `endpoint_id` (it did in the spike),
Gate 1 is exact; if not, gate at the router accept for the blobs ALPN and document
(spike R2 / OQ). This mirrors the spike's Step-0 discipline; it is confirmation, not
new recon.

### 6.2 Step 1 — Net: the serve gate + `BlobAclView`

In `net::blob`: add `spawn_blob_gate(acl: Arc<Mutex<BlobAclView>>) -> (EventSender,
JoinHandle<()>)` (lift `spike-blobs::spawn_event_gate`, reading the shared cell instead
of a fixed `AuthContext`), and `BlobAclView { active_devices, referenced_hashes }` with
`is_active(EndpointId)` / `is_referenced(&[u8;32])`, both fail-closed. Add a
`BlobStore::serve_handler(gate) -> BlobsProtocol` (or a `serve(store, gate)` free fn) so
`BlobsProtocol::new(&store, Some(events))` is built without leaking `iroh_blobs` past
`net::blob`. Unit-test the gate loop decisions against a seeded `BlobAclView` (active
vs. removed vs. unknown device; referenced vs. unreferenced hash; push/observe denied),
reusing the spike's test intent.

### 6.3 Step 2 — Net: the verified fetch client

In `net::blob`: add `FetchOutcome` + `fetch_blob(...)` + the error-classification
helpers, lifted from `spike-blobs::net`. Re-export `FetchOutcome` (and any public
`BlobFetch` summary) from `lib.rs`. Unit-test `classify_get_error` mapping
(open→DeniedAtConnect, remote_read/write→DeniedPerHash, else→Unavailable) with
synthesized errors where feasible; the live paths are covered by the e2e in Step 6.

### 6.4 Step 3 — Net: wire serving into the Node + the fetch method

- `transport.rs`: `NetTransport::bind` gains an optional `blobs_handler:
  Option<BlobsProtocol>` argument (or a small `BlobServe` bundle), chained as a third
  `.accept(iroh_blobs::ALPN, …)` when present — the exact pattern used for
  `pipe_handler`.
- `node.rs`: `Node::spawn_room` gains an optional `BlobServeConfig { blobs_dir }`.
  When set, the Node `BlobStore::open`s the dir, builds the `BlobAclView` cell, extends
  `RoomReconciler` to refresh `referenced_hashes` (from the room's `file.shared` set)
  and `active_devices` (from the snapshot) alongside the existing `AdmissionView`, and
  passes the blobs handler to `bind`. On shutdown, `BlobStore::close()` releases the
  lock. Add `Node::fetch_file(provider_addr, hash, declared, timeout)` driving
  `fetch_blob` on `self.transport.endpoint()`.
- `audit.rs`: add the `blob.serve.*` sink methods (§7) and call them from the gate loop
  (accept / reject:<cause>) so serving is observable without a subscriber.

### 6.5 Step 4 — CLI: `file fetch`

- `cli.rs`: add `FileAction::Fetch { room_id, file_id, out, peers, timeout, loopback }`
  and dispatch through the scoped `runtime()` (mirrors `file share`/`pipe connect`).
- `file.rs`: implement `fetch(...)` per §5.2 — reuse `identity::SecretKeys::load`,
  `fold_room`, active-member pre-check, `build_admission`, `build_dial_set`, `net_mode`,
  `parse_peers`, `parse_timeout`; add `parse_file_id` (inverse of `file_handle`),
  `find_file_shared(store, room, file_id)` (offline read, the `open_pipe` analog),
  `resolve_providers`, `resolve_output_path` + `sanitize_name`, and the per-provider
  fetch loop with the HashMismatch hard-fail. Print `FetchSummary`
  (`saved: <path>` / `verified: blake3:<hex>` / `size: N bytes` / `provider: <short>`).
  Secret-safe; nothing written on any failure path.

### 6.6 Step 5 — CLI: `room tail` serves blobs

Thread a `BlobServeConfig { blobs_dir: home.join("blobs") }` into `message::tail`'s
`Node::spawn_room` call so a running `room tail` provider serves the blobs it holds to
active members. Keep it strictly additive (no output change for non-blob rooms; the
blob plane is silent unless a fetch happens). `members_status`'s short-lived node need
not serve. Confirm the FsStore single-open-per-process invariant holds (`room tail`
opens `<home>/blobs/` once for the session).

### 6.7 Step 6 — Tests + docs

`tests/blob_e2e.rs` (§8) — the always-green Node-level two-peer suite. Extend
`file_cli.rs` with the fetch CLI cases and (optionally) a `#[ignore]`-gated online tier
in `two_peer_e2e.rs` mirroring the pipe tier. Update `docs/getting-started.md` Step 5
(remove the "not yet implemented" note; reconcile the `file fetch` output block to the
shipped binary; drop the "illustrative" caveat; keep the availability caveat) and the
"Unavailable file" troubleshooting case. Update `README.md` "Current Status" with the
landed serve/fetch half (repo convention). Run `scripts/verify.sh` — the real CI gate.

---

## 7. Error model & observability

**Fetch error taxonomy (AC-facing).** Every failure exits non-zero, writes nothing (or
removes a partial temp file), and prints one actionable line to stderr:

| Condition | Detection | Message (shape) |
|---|---|---|
| Bad args (file id / out / timeout) | pre-IO parse | the specific parse error |
| No identity | `SecretKeys::load` | reuse the identity-missing message |
| Unknown room | `fold_room` empty | reuse `fold_room`'s "no room …" message |
| Not an active member | membership fold | `you are not an active member of room <id>; only an active member can fetch files (this identity is <id>)` |
| Unknown file | `find_file_shared` empty after sync wait | `no such file <FILE_ID> in room <id>; has it been shared and synced? try running \`room tail\` first` |
| Unsupported format | `blob_format` not raw/absent | `file <FILE_ID> uses blob_format=<x>, which this version cannot fetch (raw only)` |
| **Hash mismatch** | receiver BLAKE3 ≠ declared | `integrity check FAILED: fetched bytes hash blake3:<got> but the reference declares blake3:<want>; refusing to save` **(AC2)** |
| Denied at connect | `FetchOutcome::DeniedAtConnect` | `provider <short> denied the connection (are you an active member?)` (per-provider, then aggregate) |
| Denied per-hash | `FetchOutcome::DeniedPerHash` | `provider <short> will not serve this hash` (per-provider) |
| Unavailable (all) | every provider `Unavailable`/denied | `file <FILE_ID> is currently unavailable: no provider holding it is online` **(honest unavailable; PRD §14/§18.2)** |
| Save failure | write/rename error | `could not save to <path>: <source>` (no partial file left) |

The whole fetch (per provider) is wrapped in a **bounded timeout** so unavailable fails
promptly (spike §5.4 / R6). `HashMismatch` is a **hard stop** — it does not fall through
to the next provider (a lying reference is a security event, not a transient miss).

**Serve-side audit vocabulary** (new `AuditSink` methods; stable, greppable,
secret-free — the CLI renders them to stderr via an audit sink since there is no
`tracing` subscriber):

- `blob.serve.accepted peer=<short> hash=<short>` — a gated fetch was served.
- `blob.serve.rejected:<cause> peer=<short> [hash=<short>]` — `<cause>` ∈
  `not_active` (Gate 1) / `not_referenced` (Gate 2) / `push_denied` / `observe_denied`.

This mirrors the pipe plane's `pipe.connect.rejected:<cause>` sink (IR-0108), so an
unauthorized fetch is rejected **and locally visible** on the provider's terminal.

**Fetch-side output** (AC4): on success, stable labeled lines —
`saved: <path>`, `verified: blake3:<hex>`, `size: <N> bytes`, `provider: <short>`.
stdout stays clean for scripting; diagnostics go to stderr.

---

## 8. Test strategy

Maps the issue Test Plan ("two-peer integration test with valid fetch, wrong hash,
unavailable provider, and unauthorized peer") + the four ACs.

**Net unit tests (`net::blob`):**
- Serve-gate decisions over a seeded `BlobAclView`: active vs. removed vs. unknown
  device (Gate 1); referenced vs. unreferenced hash (Gate 2); push/observe denied.
- `classify_get_error` mapping (open→DeniedAtConnect, remote_read/write→DeniedPerHash,
  else→Unavailable).

**Net two-peer integration (`tests/blob_e2e.rs`, loopback `NetMode::Loopback`, no relay,
built on the `file_e2e.rs` fixtures — a two-member room, Alice + Bob):**
- **Valid fetch (AC1):** Alice serves a blob she holds + its `file.shared`; Bob
  (active) fetches it, the transfer completes, the receiver BLAKE3 equals the declared
  hash ⇒ `Fetched`, bytes match the original.
- **Wrong hash (AC2):** drive `fetch_blob(fetch_hash = served, declared_hash =
  different)` so the receiver recompute fails ⇒ `HashMismatch`; assert no bytes are
  accepted as valid. (The declared≠served split makes this deterministic in a
  content-addressed store, per spike §5.3.)
- **Unauthorized peer (AC3):** a third identity that is **not** an active member
  (removed or never-joined) attempts the fetch ⇒ `DeniedAtConnect` (Gate 1); assert
  zero bytes transferred and the provider logs `blob.serve.rejected:not_active`.
  Additionally: an **active** member requesting a store hash **not** referenced by any
  `file.shared` ⇒ `DeniedPerHash` (Gate 2 — per-hash gating independent of node
  admission, spike matrix row 4).
- **Unavailable provider:** with the provider node shut down (or a hash never
  imported), the fetch returns `Unavailable` within the bounded timeout — no hang, no
  panic.

**CLI tests (`tests/file_cli.rs`, `assert_cmd` + per-test `IROH_ROOMS_HOME`):**
- **Arg + reference plumbing (offline):** `file fetch` with a bad file id / bad
  `--timeout` exits non-zero pre-IO; `file fetch` for an unknown file id in a known
  room reports "no such file" (offline, no node needed if the reference is absent and
  no `--peer` given — bounded, then the unknown-file error); non-member caller is
  refused with the active-member message.
- **Unsupported format:** a seeded `file.shared` with `blob_format = "hash_seq"` ⇒ the
  unsupported-format error, nothing written.
- **Output path + name safety (unit):** `resolve_output_path` (file vs. dir vs. default
  downloads) and `sanitize_name` (strips separators / `..` / control chars; falls back
  to `file_<hex>`; a traversal `name` cannot escape the target dir).
- **Gated online tier (optional, `#[ignore]`, loopback):** two child CLI processes —
  Alice `room tail` (serving) + Bob `file fetch` — proving the **CLI** end-to-end:
  Bob's fetch prints `saved:` + `verified:` and the saved file's bytes match Alice's
  original (AC1 + AC4); a second run with Bob **not** an active member is denied
  (AC3). Runs under the documented `-- --ignored --test-threads=1` command, mirroring
  the IR-0109 pipe tier.

All CI-tier tests are deterministic loopback (no discovery, no relay, no external
tools). The always-green backstop is `blob_e2e.rs` at the Node layer; the CLI gated
tier is the product-level proof on top (same tiering as IR-0109).

---

## 9. Security, privacy, reliability, performance

- **Authorization (two gates, fail-closed).** Gate 1 rejects any non-active peer at
  connect on the **cryptographically proven** `endpoint_id` — the room-ACL path an
  unauthorized peer cannot pass (AC3). Gate 2 restricts even an active member to
  **referenced** hashes, so the blobs ALPN can never be used to exfiltrate an arbitrary
  store hash. Both read the live fold, so a mid-session removal takes effect within a
  tick (same guarantee as the event/pipe planes). Residual Risk #3 stands and is
  documented: any active member may serve any referenced blob to any other active
  member; there is no per-blob revocation in MVP.
- **Content integrity (AC2).** The receiver independently recomputes BLAKE3-256 and
  refuses to save on mismatch; `iroh-blobs` bao verified streaming already rejects
  tampered bytes for a requested hash in transit. A `file.shared` that *declares* a
  hash different from the served bytes is caught by the receiver recompute. Nothing
  corrupt is ever written (atomic temp-then-rename; the temp file is removed on any
  failure).
- **Privacy / path safety.** The peer-supplied `name` is sanitized to a single safe
  basename before it touches the filesystem (path-traversal guard, §5.6). The
  downloads dir is `0700` on Unix. No secret key material is read for a fetch beyond
  the device signing key (used only to authenticate the QUIC connection) and none is
  printed. The fetch discloses this node's `device_id` to the provider (intrinsic to a
  connection) — no new disclosure beyond the event plane.
- **Reliability / honest availability.** Every fetch attempt is bounded; an offline or
  non-holding provider yields a distinct `Unavailable` outcome, mapped to honest CLI
  language (PRD §14). Providers are tried sequentially; exhausting them reports
  unavailable, never hangs. A provider that is online but whose fold has not yet synced
  the `file.shared` will deny per-hash until it does — a transient, retryable state
  reported as such.
- **Performance.** BLAKE3 is fast; the transfer is streamed by `iroh-blobs`. The 100 MiB
  `MAX_SHARED_FILE_BYTES` cap bounds worst-case transfer and (if the buffered getter is
  used) memory; prefer the get-into-store path (Step 0) for large blobs. The serve gate
  is a per-message set-membership check (O(1)).
- **Migration.** None. No `rooms.db` schema change (provider status stays derived from
  the blob store + the on-log `file.shared`, per IR-0202 §5.3). No `file.shared` wire
  change. No new crate/version.

---

## 10. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | The 0.103.0 serve/get API differs subtly from the spike's usage (get-into-store vs. buffered; connect-hook identity exposure). | Low | Medium | Step 0 re-confirms on the pinned source (the spike already exercised the serve gate + getter); isolate all `iroh_blobs` types behind `net::blob`. |
| R2 | Sourcing the referenced-hash set incorrectly (serving a hash whose `file.shared` is not causally visible / from a non-member). | Medium | Medium | IR-0203 guarantees only valid `file.shared` persist; compute `referenced_hashes` from the room store's `file.shared` set refreshed by the fold reconciler; unit-test Gate 2. Note per-hash provenance nuance vs. the spike simplification in NOTES. |
| R3 | Deciding "which node serves" wrong (e.g. only `file share`, which is offline) breaks the demo flow. | Medium | Medium | Serve on the long-running `room tail` session (the demo's provider-online surface); make it opt-in `BlobServeConfig`; document. OQ-1. |
| R4 | Content-addressing makes a "hash mismatch" hard to reach via honest transfer, so AC2 is under-tested. | Medium | Medium | Keep the `fetch_hash`/`declared_hash` split in the net client so the AC2 test drives a deterministic declared≠served mismatch (spike §5.3); assert nothing is saved. |
| R5 | Path traversal via a malicious `file.shared.name`. | Low | High | `sanitize_name` takes the final component only, strips `..`/separators/control chars, falls back to `file_<hex>`; unit-tested. |
| R6 | `FsStore` exclusive-lock deadlock if `room tail` and another same-process open collide. | Low | Medium | One open per process for `<home>/blobs/`; `close()` on shutdown; the fetch node and a serving `room tail` are separate processes. Covered by the existing memory note. |
| R7 | Fetch hangs on an unreachable provider. | Medium | Medium | Bounded per-attempt `timeout` around connect + transfer; distinct `Unavailable`. |
| R8 | Provider discovery fails when `providers` is stale/absent. | Medium | Low | Default to the author `device_id`; try `--peer` hints; sequential fallback; honest unavailable when exhausted. |
| R9 | `0.103.0` is maintainer-labeled pre-production (spike R4 / Residual Risk #10). | Medium | Medium | Same pin the spike + IR-0202 already ship; serve/get are the exact paths the spike exercised; flag any instability into the Residual-Risk-#10 shipping decision. |

---

## 11. Acceptance criteria

Maps issue #29 ACs + Test Plan.

- [ ] **AC1 — Authorized peer can fetch from an online provider.** With a provider
  online (serving via `room tail`) and holding the blob, an active member's
  `file fetch <ROOM_ID> <FILE_ID>` transfers the bytes over the ACL-gated blobs ALPN
  and saves them; the net `blob_e2e` valid-fetch test yields `FetchOutcome::Fetched`
  and byte-equality with the original.
- [ ] **AC2 — Hash mismatch is rejected and reported.** When the assembled bytes'
  BLAKE3-256 does not equal the declared `blob_hash`, the fetch fails with the
  integrity-check error, exits non-zero, and writes nothing to disk (`HashMismatch`).
- [ ] **AC3 — Unauthorized peer cannot fetch via the room ACL path.** A non-active peer
  is denied at the connect gate (`DeniedAtConnect`); the provider logs
  `blob.serve.rejected:not_active`; zero bytes transfer. Additionally an active member
  is denied an **unreferenced** hash at the per-hash gate (`DeniedPerHash`).
- [ ] **AC4 — CLI prints saved path and verified hash.** A successful fetch prints
  `saved: <path>` and `verified: blake3:<hex>` (equal to the declared hash), and the
  saved file's contents match the shared file.
- [ ] **AC5 — Honest unavailable.** With no provider online / holding the hash, the
  fetch fails cleanly within the bounded timeout with a distinct "unavailable" report
  (no hang, no panic) — the test-plan "unavailable provider" case.
- [ ] **AC6 — No regressions / gate green.** `scripts/verify.sh` passes (fmt `--check`,
  clippy `-D warnings` pedantic, `--all-features` tests); no `file.shared` wire change;
  no `rooms.db` schema change; no new crate/version; `getting-started.md` Step 5 and
  the README status are reconciled to the shipped behavior.

**Test-plan coverage:** valid fetch → AC1/AC4; wrong hash → AC2; unavailable provider →
AC5; unauthorized peer → AC3.

---

## 12. Assumptions

1. `iroh-blobs 0.103.0` exposes the `provider::events` serve gate and the
   `get_blob(...).bytes()` (and, ideally, a get-into-store) client the spike already
   exercised — confirmed in Step 0. The connect hook surfaces the proven `endpoint_id`
   (it did in the spike).
2. Serving on the long-running `room tail` session is the right "provider online"
   surface for MVP (matches the getting-started demo). A dedicated `file serve` command
   is an alternative (OQ-1).
3. `raw` single-blob is the only fetched `blob_format`; a `hash_seq` reference is
   rejected with a clear error (child-hash authorization is a follow-up).
4. Provider discovery from `file.shared.providers` (default `[author device_id]`) plus
   `--peer` hints / discovery is sufficient; sequential provider fallback is acceptable
   (no parallel racing / ranking for MVP).
5. Importing the verified bytes into the local store after fetch (making the fetcher a
   provider) is desirable but optional (OQ-5); fetch-only is acceptable if product
   prefers it.
6. A default downloads directory of `<home>/downloads/` (overridable by
   `IROH_ROOMS_DOWNLOADS`) is acceptable as the "configured downloads path".

## 13. Open questions

- **OQ-1 (serve surface):** Serve blobs from the managed `room tail` session
  (recommended, matches the demo) vs. a dedicated `iroh-rooms file serve <ROOM_ID>`
  long-running command vs. both? A dedicated command is more explicit but adds surface
  and diverges from the getting-started flow.
- **OQ-2 (Gate 2 source):** A fold-refreshed `referenced_hashes` cell (recommended, for
  symmetry with the admission cell) vs. a per-request pump-routed engine read (the
  `serve_pipe_query` analog)? The cell is simpler; the per-request read is always
  perfectly fresh.
- **OQ-3 (share-time broadcast):** Should `file share` best-effort push the
  `file.shared` frame to connected peers now (IR-0202 §4.3c), or is on-demand sync in
  `file fetch` (like `pipe connect`) sufficient? Recommend defer (keeps `file share`
  offline; the reference already propagates via sync).
- **OQ-4 (timeout default):** `file fetch --timeout` default — 30s proposed (larger
  than the 5s message/pipe default because a transfer can be large). Confirm against
  the PRD §17.1 timing targets.
- **OQ-5 (re-provide after fetch):** Import fetched bytes into `<home>/blobs/` so the
  fetcher becomes a provider (recommended) vs. fetch-only?
- **OQ-6 (`hash_seq`):** In or out of MVP? Recommended out — reject with a clear error;
  the per-hash gate's child-hash authorization is the follow-up (spike NOTES §6).
- **OQ-7 (overwrite policy):** Refuse to overwrite an existing `--out` file without
  `--force` (recommended) vs. overwrite silently vs. auto-suffix (`name (1)`)?
- **OQ-8 (`FILE_ID` acceptance):** Accept only the `file_<hex>` handle, or also bare
  32-hex? Recommend accept both, print the `file_<hex>` form.

## 14. Definition of done

1. An online node holding a blob serves it to active room members over the `iroh-blobs`
   ALPN through the two-gate ACL (Gate 1 active-member, Gate 2 referenced-hash,
   fail-closed), sourced from the real fold + the real `file.shared` set; `room tail`
   enables serving.
2. `iroh-rooms file fetch <ROOM_ID> <FILE_ID> [--out …] [--peer …] [--timeout …]`
   resolves the reference, discovers a provider, transfers the blob, independently
   verifies BLAKE3-256 against the declared hash, saves the verified bytes to a
   sanitized output path, and prints the saved path + verified hash (AC1/AC2/AC4).
3. An unauthorized peer is denied at the room ACL path and the denial is locally
   logged; an unavailable provider is reported honestly within a bounded timeout
   (AC3/AC5).
4. All `iroh_blobs` types stay isolated behind `net::blob`; the fetch client + serve
   gate are lifted from the spike with no new authorization model, event schema, or
   crate version.
5. `tests/blob_e2e.rs` (Node-level, always-green) + extended `file_cli.rs`
   (+ optional gated online tier) cover the four test-plan scenarios and all ACs;
   `getting-started.md` Step 5 and README status are reconciled; `scripts/verify.sh` is
   green (AC6).
