# Spec: Byte-bounded priority queues

**Issue:** #141 — `[NET] Byte-bounded priority queues (#134 §12.3)`  
**Labels:** `type/feature`, `area/transport`, `priority/p1`, `risk/medium`  
**Owning crate/module:** `crates/iroh-rooms-net` → `transport.rs`, `peer.rs`, `node.rs`; supporting tests in `crates/iroh-rooms-net` and possibly `crates/iroh-rooms-core` test helpers  
**Status:** planning / ready-to-implement. Do **not** implement from this document alone.

---

## 1. Summary

The network transport currently bounds the event-plane queues by **frame count**:

- `NetConfig::inbound_frame_capacity` default `256` (`crates/iroh-rooms-net/src/transport.rs:103-116`).
- `NetConfig::outbound_frame_capacity` default `256` (`crates/iroh-rooms-net/src/transport.rs:103-116`).
- Per-peer outbound queues are `tokio::sync::mpsc::channel::<Vec<u8>>(capacity)` with an `AtomicUsize` depth that counts frames (`peer.rs:53-60`, `transport.rs:135-148`).
- Inbound reader tasks enqueue `Inbound { peer, bytes }` onto a bounded `mpsc::Sender<Inbound>` (`peer.rs:96-111`, `transport.rs:577`).
- Saturation already follows the intended recovery shape: drop the frame, audit `transport.queue.saturated`, and close the peer link (`transport.rs:248-272`, `peer.rs:100-106`).

Issue #134 §12.3 requires byte budgets and priority classes instead: default **8 MiB queued per peer** and **2 MiB per subscribed stream per peer**, with priority ordering `governance > checkpoint > content > blob-hints`. This issue should harden the v1 event transport by enforcing those byte caps over encoded `SyncMessage` body bytes while preserving the existing audit and close-on-full recovery semantic.

This is a transport-queue hardening change only. It must not introduce the v2 wire protocol, change admission, or change `SyncMessage` semantics.

---

## 2. Repository context read for this spec

### 2.1 Product and protocol context

- `README.md` describes Iroh Rooms as a small-room, local-first runtime. The room event plane is canonical signed events, membership, deterministic validation, local SQLite persistence, and bounded sync.
- `README.md` documents `outbound_depth=<N>` in verbose diagnostics; today that value is a queue-depth counter. This issue intentionally changes it to queued bytes.
- `docs/protocol.md` states the transport carries canonical-CBOR `SyncMessage` frames and that validation stays at the engine boundary. The transport should keep treating frame bodies as opaque for correctness; priority classification may decode `SyncMessage` only to decide local queue placement.
- `docs/security/threat-model.md` identifies peer-supplied transport bytes as untrusted and admission-before-bytes as a key trust boundary. Queue hardening is an availability/DoS control and must not weaken the existing authorization boundary.
- `docs/operations/data-handling.md` says audit output is local and secret-free. New queue saturation detail must not include frame bodies, event contents, invite capability secrets, blob bytes, or filesystem paths.
- `CONTRIBUTING.md` lists `scripts/verify.sh` as the full quality gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets --all-features`, SDK doctests, and SDK examples.

### 2.2 Current transport implementation

Relevant files:

- `crates/iroh-rooms-net/src/transport.rs`
- `crates/iroh-rooms-net/src/peer.rs`
- `crates/iroh-rooms-net/src/node.rs`
- `crates/iroh-rooms-net/src/frame.rs`
- `crates/iroh-rooms-net/src/audit.rs`
- `crates/iroh-rooms-net/src/manager.rs`
- `crates/iroh-rooms-net/src/handler.rs`

Current behavior:

- `NetConfig` has frame-count capacities: `inbound_frame_capacity` and `outbound_frame_capacity`, defaulting to `256` each.
- `NetTransport::bind` builds the inbound sink with `mpsc::channel(cfg.inbound_frame_capacity.max(1))`.
- `peer::register_connection` builds a per-peer outbound channel with `mpsc::channel::<Vec<u8>>(shared.outbound_frame_capacity())` and installs an `OutboundQueue`.
- `Shared::route` encodes `Outgoing.msg` with `SyncMessage::encode()`, increments `queue.depth` by `1`, then `try_send`s the body. On `Full`, it decrements depth, audits `transport.queue.saturated` with queue `outbound`, and closes the connection.
- `writer_task` decrements frame depth by `1` when it receives a body and writes it via the existing length-prefixed frame codec.
- `reader_task` reads length-prefixed frame bodies via `read_frame`, then `try_send`s raw bytes to the pump. On inbound channel saturation, it audits queue `inbound`, closes the connection, and stops reading.
- `frame.rs` already caps a single frame body at `MAX_FRAME_BYTES = 1 MiB`, equal to `iroh_rooms_core::sync::MAX_FRAME_BYTES`. The new queue caps are larger aggregate caps and should not change the per-frame wire cap.
- `node.rs::pump` decodes `SyncMessage` at the engine-driver boundary, handles join-bootstrap provisional gating, calls `engine.on_message`, routes engine outputs through `route_all`, and updates peer reconciliation on membership changes.

### 2.3 Current sync-message families

`crates/iroh-rooms-core/src/sync/message.rs` defines these v1 `SyncMessage` variants:

- `AdminTip`
- `Heads`
- `WantEvents`
- `WantMembership`
- `WantRecentChat`
- `Events`
- `NotFound`
- `ProveCapability`

There is no v1 wire field for a stream id or for a v2 queue family. The v1 implementation therefore needs a deterministic internal mapping from existing `SyncMessage` variants to the #134 §12.2 family names and the #134 §12.3 priority classes.

### 2.4 Existing tests and diagnostics to preserve

- `transport.rs` unit tests pin `NetConfig` defaults, `Shared::route` behavior, close-on-full shape, and `outbound_queue_depths`.
- `peer.rs` unit tests pin `peer_id` identity and reconnect backoff.
- `loopback.rs`, `manager_e2e.rs`, and other `crates/iroh-rooms-net/tests/*.rs` cover real in-process QUIC behavior.
- `node.rs::outbound_queue_depths` and verbose CLI diagnostics surface outbound queue depth; after this issue the numeric unit is bytes.

---

## 3. Goals, non-goals, and scope

### 3.1 Goals

1. Bound outbound queued data by encoded body bytes, not frame count.
2. Bound inbound queued data by encoded body bytes, not frame count.
3. Enforce a default per-peer queued-byte cap of **8 MiB** for both inbound and outbound event-plane queues.
4. Enforce a default per-subscribed-stream queued-byte cap of **2 MiB**. For v1, treat each peer's event-plane sync stream as one logical subscribed stream because the wire has no stream id.
5. Add stable internal priority classes with effective ordering `governance > checkpoint > content > blob-hints`.
6. Ensure governance traffic can skip ahead of a saturated content queue, specifically for `AdminTip` and `WantMembership`.
7. Preserve the current saturation recovery shape: drop the frame that cannot be admitted, audit `transport.queue.saturated`, and close the link.
8. Keep the frame codec and `SyncMessage` wire protocol unchanged.
9. Verify that no unbounded channel remains on a network-derived path, or explicitly convert/justify any grep hits that are not network-derived.

### 3.2 Non-goals

- No v2 wire protocol work.
- No new `SyncMessage` variants, fields, or encoding changes.
- No admission, membership, join-bootstrap, blob ACL, or pipe authorization semantic changes.
- No change to `MAX_FRAME_BYTES` or the length-prefixed frame codec.
- No durable storage migration.
- No changes to event validation, event authorization, or the `SyncEngine` protocol model.
- No telemetry backend or remote metrics export.

---

## 4. Key design decisions

### D1 — Count encoded body bytes only

Queue accounting must charge `body.len()` where `body` is the encoded `SyncMessage` body returned by `SyncMessage::encode()` or read by `read_frame`.

Do not count:

- One unit per frame.
- The 4-byte length prefix.
- `Vec` capacity or allocator overhead.
- Decoded object heap size.

This directly satisfies the acceptance criterion that `OutboundQueue::depth()` reports bytes-in-queue.

### D2 — Replace mpsc frame-count queues with small custom byte-priority queues

Tokio `mpsc` capacity is item-count based, so the event-plane queue needs a small custom queue.

Recommended new private module:

```text
crates/iroh-rooms-net/src/queue.rs
```

Core shape:

```rust
pub(crate) const DEFAULT_PER_PEER_QUEUE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const DEFAULT_PER_STREAM_QUEUE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueuePriority {
    BlobHints = 0,
    Content = 1,
    Checkpoint = 2,
    Governance = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueFamily {
    Session,
    Governance,
    Subscription,
    Publication,
    Reconciliation,
    Checkpoint,
    BlobHints,
}

pub(crate) struct QueuedFrame {
    pub peer: Option<PeerId>,
    pub body: Vec<u8>,
    pub bytes: usize,
    pub family: QueueFamily,
    pub priority: QueuePriority,
}
```

Use `Mutex<QueueState>` plus `tokio::sync::Notify` for async consumers:

- `try_push(frame) -> Result<(), QueueSaturated>` is non-blocking and used from `SyncTransport::send`/reader tasks.
- `recv().await -> Option<QueuedFrame>` drains the highest non-empty priority first.
- `close()` wakes waiters and ends the receiver once the queue drains or is explicitly discarded.
- `depth_bytes() -> usize` returns current queued body bytes.

Keep the implementation dependency-free; the workspace already uses `tokio::sync`.

### D3 — V1 family-to-priority mapping is internal and deterministic

Map existing v1 `SyncMessage` variants to #134 §12.2 families and scheduler priorities as follows:

| `SyncMessage` variant | §12.2 family | Queue priority | Cap bucket |
| --- | --- | --- | --- |
| `AdminTip` | Governance | Governance | per-peer only |
| `WantMembership` | Governance | Governance | per-peer only |
| `Heads` | Checkpoint | Checkpoint | per-peer only |
| `ProveCapability` | Session | Checkpoint | per-peer only |
| `Events` | Publication | Content | per-stream + per-peer |
| `WantRecentChat` | Subscription | Content | per-stream + per-peer |
| `WantEvents` | Reconciliation | BlobHints | per-stream + per-peer |
| `NotFound` | Reconciliation / blob-hint-equivalent | BlobHints | per-stream + per-peer |
| undecodable inbound body | BlobHints | BlobHints | per-stream + per-peer |

Rationale:

- `AdminTip` and `WantMembership` are the acceptance-critical governance frames.
- `Heads` is the closest v1 equivalent of a checkpoint/tip signal.
- `ProveCapability` is session control and must not be starved behind content during join bootstrap. It is not governance, but it should sit above content.
- `Events` may contain chat, file, pipe, or membership events, but transport should not parse room event payloads just to classify queue priority. It remains publication/content at the transport layer.
- `WantEvents` and `NotFound` are reconciliation hints and are safe to treat as lowest priority in v1.

### D4 — The v1 per-stream cap is a logical event-stream cap

Issue #134 §12.3 says **2 MiB per subscribed stream per peer**. V1 has one event ALPN bidi stream per peer and no explicit stream id in `SyncMessage`.

Implement the v1-equivalent as:

- One logical stream bucket per peer: `StreamKey::EventV1(peer)`.
- `Publication`, `Subscription`, `Reconciliation`, and `BlobHints` frames charge both the peer cap and the logical stream cap.
- `Governance`, `Checkpoint`, and `Session` frames charge only the peer cap.

This means a peer can fill the 2 MiB content/reconciliation stream budget but still has peer-level headroom for `AdminTip`, `WantMembership`, `Heads`, and `ProveCapability` up to the 8 MiB per-peer cap.

### D5 — Prefer skip-ahead over eviction for v1

The required governance behavior can be satisfied without evicting already-queued content:

1. Content-like frames are limited by the 2 MiB stream bucket.
2. Governance frames bypass the stream bucket and charge only the 8 MiB peer bucket.
3. The writer drains higher priorities before lower priorities.

So when a test fills the content budget to exactly 2 MiB, a following `AdminTip` or `WantMembership` can still be admitted and will be popped before content. Do not implement lower-priority eviction unless a later Track 2/v2 design explicitly needs it.

If a frame cannot be admitted because its relevant cap is truly exhausted, keep the existing semantic: reject that incoming frame, audit saturation, and close the peer link.

### D6 — `NetConfig` should use byte-named fields

Replace wrong-unit frame-count fields with byte-named fields so callers cannot silently continue thinking in frame counts.

Recommended public shape:

```rust
pub struct NetConfig {
    pub mode: NetMode,
    pub conn_event_capacity: usize,
    pub room_event_capacity: usize,
    pub inbound_peer_queue_bytes: usize,
    pub outbound_peer_queue_bytes: usize,
    pub stream_queue_bytes: usize,
    pub pipe_query_capacity: usize,
}
```

Defaults:

```rust
inbound_peer_queue_bytes: 8 * 1024 * 1024,
outbound_peer_queue_bytes: 8 * 1024 * 1024,
stream_queue_bytes: 2 * 1024 * 1024,
pipe_query_capacity: MAX_CONCURRENT_BIDI_STREAMS as usize,
```

Notes:

- `conn_event_capacity` and `room_event_capacity` remain item-count capacities because they are local observer broadcast queues, not peer frame-body queues.
- Removing or renaming `inbound_frame_capacity` and `outbound_frame_capacity` is intentionally breaking inside the unpublished net crate/API surface. It forces callers and tests to confront the unit change.
- Clamp byte caps to at least `1` only at construction if preserving the current `max(1)` style; tests should pin defaults, not zero behavior.

### D7 — Audit vocabulary remains stable

Keep `AuditSink::transport_queue_saturated(device, queue)` and `TracingAudit`'s `reason = "transport.queue.saturated"` unchanged.

Allowed queue labels:

- `"outbound"`
- `"inbound"`

Do not log frame bodies, decoded message contents, invite capability secrets, or event payloads. If future observability adds counts, keep it to numeric bytes/caps and stable family/priority names.

### D8 — Remaining unbounded channels require grep review

A repository grep currently finds unbounded channels in `crates/iroh-rooms-net/src/node.rs`, `pipe/runtime.rs`, `pipe/gate.rs`, and `pipe/connector.rs`. The issue acceptance is specifically: no unbounded channel remains on a network-derived path.

Implementation should:

1. Remove `mpsc` from the inbound/outbound event-frame path entirely by using the byte-priority queues.
2. Convert `PipeQuery` plumbing (`pipe/runtime.rs`, `pipe/gate.rs`, `node.rs`) from `mpsc::UnboundedSender/Receiver` to bounded `mpsc::Sender/Receiver` because pipe ALPN streams can trigger gate queries from network-derived input. Use `NetConfig::pipe_query_capacity` or a fixed default at least `MAX_CONCURRENT_BIDI_STREAMS`.
3. Leave `Node::cmd_tx` as unbounded only if documented as a local API/control path, not a peer/network-derived byte path.
4. Leave `PipeForwarder::outcomes` unbounded only if documented as local-loopback outcome reporting guarded by `PIPE_MAX_CONCURRENT_FORWARDS`; otherwise convert it to a bounded channel in the same change.
5. Add a test or CI note with the grep command and an allowlist comment for any remaining `Unbounded*` use.

Recommended grep command for the implementation PR:

```bash
rg "unbounded_channel|UnboundedSender|UnboundedReceiver" crates/iroh-rooms-net/src
```

---

## 5. Detailed implementation plan

### Step 1 — Add the queue module

Create `crates/iroh-rooms-net/src/queue.rs` and add `mod queue;` in `lib.rs` or the crate root module list.

Implement:

- `DEFAULT_PER_PEER_QUEUE_BYTES`
- `DEFAULT_PER_STREAM_QUEUE_BYTES`
- `QueuePriority`
- `QueueFamily`
- `QueuedFrame`
- `QueueBudget`
- `BytePriorityQueue`
- `BytePriorityReceiver` if separating producer/consumer handles is cleaner
- `QueueSaturated`

Suggested queue state:

```rust
struct QueueState {
    closed: bool,
    peer_bytes: usize,
    stream_bytes: usize,
    by_priority: [VecDeque<QueuedFrame>; 4],
}
```

For inbound, either:

- use a multi-peer queue with `HashMap<PeerId, PeerBudgetState>`, or
- keep one queue instance per peer and a small global priority fan-in.

The simpler implementation is one multi-peer inbound queue:

```rust
struct InboundQueueState {
    closed: bool,
    peer_bytes: HashMap<PeerId, usize>,
    stream_bytes: HashMap<PeerId, usize>,
    by_priority: [VecDeque<Inbound>; 4],
}
```

On `recv`, subtract the frame's byte cost from the relevant counters before returning it, just as `writer_task` currently decrements depth before writing.

### Step 2 — Add family classification helpers

Add classification helpers close to the queue module or in `transport.rs` if private to transport:

```rust
pub(crate) fn classify_sync_message(msg: &SyncMessage) -> QueueFamily;
pub(crate) fn priority_for_family(family: QueueFamily) -> QueuePriority;
pub(crate) fn charges_stream_budget(family: QueueFamily) -> bool;
```

Outbound path can classify from `out.msg` before encoding or after encoding.

Inbound path should classify by attempting `SyncMessage::decode(&bytes)`:

- On success, classify the decoded message.
- On decode failure, classify as `QueueFamily::BlobHints` / `QueuePriority::BlobHints` and still enqueue if budget allows. The pump will decode again and drop as today. This preserves the existing malformed-frame behavior while bounding memory.

### Step 3 — Change `NetConfig` defaults and call sites

In `transport.rs`:

1. Replace `inbound_frame_capacity` with `inbound_peer_queue_bytes`.
2. Replace `outbound_frame_capacity` with `outbound_peer_queue_bytes`.
3. Add `stream_queue_bytes`.
4. Add `pipe_query_capacity` if converting `PipeQuery` in this issue.
5. Update default tests:

```rust
assert_eq!(NetConfig::default().inbound_peer_queue_bytes, 8 * 1024 * 1024);
assert_eq!(NetConfig::default().outbound_peer_queue_bytes, 8 * 1024 * 1024);
assert_eq!(NetConfig::default().stream_queue_bytes, 2 * 1024 * 1024);
```

Update known config literals such as `src/bin/net_smoke.rs` to use `..NetConfig::default()` and new field names where needed.

### Step 4 — Replace outbound `mpsc` with `OutboundQueue`

In `transport.rs`, change `OutboundQueue` from an `mpsc::Sender<Vec<u8>> + AtomicUsize` wrapper into a byte-priority queue handle.

Recommended public behavior:

```rust
impl OutboundQueue {
    pub(crate) fn new(peer_cap: usize, stream_cap: usize) -> (Self, OutboundQueueReceiver);
    pub(crate) fn try_push(&self, body: Vec<u8>, family: QueueFamily) -> Result<(), QueueSaturated>;
    pub fn depth(&self) -> usize; // bytes, not frames
    pub(crate) fn close(&self);
}
```

Then update:

- `Shared::register_link` to store the new `OutboundQueue`.
- `Shared::unregister` to call `queue.close()` before removing it, so the writer task wakes and exits.
- `Shared::outbound_queue_depths` to continue returning `Vec<(EndpointId, usize)>`, now documented as bytes.
- `Shared::route` to:
  1. convert `out.peer` to `EndpointId`, as today;
  2. classify `out.msg`;
  3. encode `out.msg`;
  4. call `queue.try_push(body, family)`;
  5. on saturation, audit `outbound` and close the connection.

Do not call `fetch_add(1)`/`fetch_sub(1)` anywhere; byte counters live inside the queue.

### Step 5 — Update `peer::register_connection` and `writer_task`

In `peer.rs`:

- Replace `let (tx, rx) = mpsc::channel::<Vec<u8>>(...)` with `let (queue, rx) = OutboundQueue::new(peer_cap, stream_cap)`.
- Register the queue in `Shared::register_link` as today.
- Spawn `writer_task(send, rx)`.
- Change `writer_task` to receive `QueuedFrame` or raw `Vec<u8>` from the queue receiver.
- The receiver should already subtract byte depth before yielding the body, so `writer_task` should not mutate counters.
- Keep the existing oversized-frame handling: if `write_frame` returns `FrameError::Oversized`, log and continue; for other write errors, break.

### Step 6 — Replace inbound `mpsc` with byte-bounded inbound queue

In `transport.rs`:

- Replace `pub(crate) inbound_tx: mpsc::Sender<Inbound>` on `Shared` with `pub(crate) inbound: InboundQueue` or a similar handle.
- Replace `NetTransport::inbound_rx: Option<mpsc::Receiver<Inbound>>` with the custom receiver type.
- In `NetTransport::bind`, construct the inbound queue with:
  - per-peer cap from `cfg.inbound_peer_queue_bytes.max(1)`,
  - stream cap from `cfg.stream_queue_bytes.max(1)`.
- Update `take_inbound` to return the custom receiver type.
- Update `node.rs::pump` signature from `mpsc::Receiver<Inbound>` to the custom receiver and keep the existing loop shape: `inbound = inbound_rx.recv()`.

In `peer.rs::reader_task`:

- After `read_frame` returns bytes, compute `bytes.len()` and classify priority from `SyncMessage::decode(&bytes)`.
- Call `shared.try_enqueue_inbound(device, Inbound { peer, bytes })`.
- On saturation, audit `inbound`, close the connection, and break, preserving current recovery semantics.
- On queue closed, break.

### Step 7 — Convert network-derived unbounded pipe query path if grep requires it

If implementing the acceptance literally, convert `PipeQuery` from unbounded to bounded:

- `pipe/runtime.rs`: change `PipeQuery { tx: mpsc::UnboundedSender<PipeQueryMsg> }` to `mpsc::Sender<PipeQueryMsg>`.
- Async query methods already await one-shot replies; make them use `self.tx.send(msg).await` and fail closed on send error.
- `node.rs`: create `let (pipe_query_tx, pipe_query_rx) = mpsc::channel::<PipeQueryMsg>(cfg.pipe_query_capacity.max(1));`.
- `pump` receives `mpsc::Receiver<PipeQueryMsg>`.
- Unit tests in `pipe/gate.rs` and `pipe/runtime.rs` should use bounded channels.

This is not a semantic change to pipe authorization; it only makes the control channel bounded.

### Step 8 — Update diagnostics and docs comments

- Update `Node::outbound_queue_depths` doc comments to say bytes.
- Update `NetTransport::outbound_queue_depths` doc comments to say bytes.
- Update README/CLI diagnostic wording only if existing text claims frames or count. `outbound_depth=<N>` can remain as field name, but any explanation should say bytes.
- Update comments in `transport.rs` and `peer.rs` that refer to frame capacity.

---

## 6. Test strategy

### 6.1 Unit tests for queue accounting

Add focused tests in `queue.rs` or `transport.rs`:

1. **Depth is bytes, not frames**
   - Enqueue bodies of lengths `10`, `20`, and `30`.
   - Assert `depth() == 60`.
   - Pop one frame and assert depth subtracts that body's length.

2. **Exact per-peer cap**
   - With peer cap `100`, enqueue bodies totaling exactly `100` bytes.
   - Assert all land.
   - Attempt one more 1-byte frame and assert `QueueSaturated`.

3. **Exact per-stream cap**
   - With stream cap `50`, enqueue content bodies totaling exactly `50` bytes.
   - Assert all land.
   - Attempt one more content frame and assert `QueueSaturated` even if peer cap has headroom.

4. **Governance skips saturated content stream**
   - With stream cap `50` and peer cap `200`, enqueue content bodies totaling `50` bytes.
   - Enqueue an `AdminTip` or `WantMembership` body.
   - Assert it lands.
   - Assert the next popped frame is the governance frame, not the older content frame.

5. **Priority ordering**
   - Enqueue one frame in each priority in low-to-high order.
   - Assert pop order is governance, checkpoint, content, blob-hints.

6. **Malformed inbound is bounded**
   - Enqueue an undecodable inbound body over a small stream cap.
   - Assert it is rejected by byte budget and never allocates beyond the declared body already read by `read_frame`.

### 6.2 Update existing `transport.rs` tests

Update current tests around `Shared::route`:

- `net_config_default_frame_queue_capacities_are_256` becomes a byte-defaults test.
- `route_delivers_encoded_frame_to_registered_peer` should receive from the new queue receiver.
- `route_closes_peer_when_outbound_queue_is_full` should use a deliberately tiny byte cap and assert depth in bytes.
- `outbound_queue_depths()` expected values must be encoded-byte sizes, not `1`.

### 6.3 Inbound reader tests

Add a test-only `AuditSink` that records saturation calls. Then test:

- A frame above the inbound peer cap triggers exactly one `transport_queue_saturated(device, "inbound")` and calls `close_connection` path if a fake/real connection handle exists.
- Filling the content stream cap does not prevent a later `WantMembership` from being accepted when peer cap has headroom.

If direct `reader_task` testing is difficult because it needs real iroh streams, keep pure queue coverage and add loopback integration with tiny caps.

### 6.4 Integration tests

Add or update `crates/iroh-rooms-net/tests/manager_e2e.rs` or `loopback.rs`:

1. **Tiny outbound content budget saturation**
   - Configure a tiny `stream_queue_bytes`.
   - Generate enough content/publication frames to saturate it.
   - Assert link close/reconnect behavior remains consistent with existing saturation tests.

2. **Governance survives content backlog**
   - Use a queue-level unit test as the primary pin because it is deterministic.
   - If feasible, add a loopback test with a deliberately stalled writer, fill content budget, then route `AdminTip`/`WantMembership` and assert it is received before content drains.

3. **No unbounded network path**
   - Add a lightweight grep-based check to developer notes or a unit test comment. Do not shell out from Rust tests unless the repo already has such tests.
   - The PR description should include the grep output and allowlist.

### 6.5 Verification commands

For the implementation task, run the smallest useful checks while iterating:

```bash
cargo test -p iroh-rooms-net queue
cargo test -p iroh-rooms-net transport
cargo test -p iroh-rooms-net --test loopback
```

Before claiming completion, run the repository quality gate:

```bash
scripts/verify.sh
```

Also run the grep check:

```bash
rg "unbounded_channel|UnboundedSender|UnboundedReceiver" crates/iroh-rooms-net/src
```

---

## 7. Acceptance criteria mapping

| Issue acceptance | Implementation evidence |
| --- | --- |
| OutboundQueue enforces bytes-in-queue, not frame-count; `depth()` reports bytes | `OutboundQueue::try_push` charges `body.len()`; `depth()` returns queued bytes; unit test enqueues varied body sizes and checks exact byte total. |
| A governance frame preempts or skips ahead of saturated content queue | Queue test fills `stream_queue_bytes` with content, then enqueues `AdminTip` or `WantMembership`; enqueue succeeds and pop order returns governance before content. |
| Default per-peer cap is 8 MiB; per-stream cap is 2 MiB or documented v1-equivalent | `NetConfig::default()` pins `inbound_peer_queue_bytes == 8 MiB`, `outbound_peer_queue_bytes == 8 MiB`, `stream_queue_bytes == 2 MiB`; docs/comments state v1 uses one logical event stream per peer. |
| Existing saturation tests still pass; new byte-budget tests pin limits | Update current `transport.rs` saturation tests and add queue byte-limit tests. Run `cargo test -p iroh-rooms-net` and full `scripts/verify.sh`. |
| No unbounded channel remains on a network-derived path | Event path no longer uses `mpsc`; pipe query path converted to bounded or grep-allowlisted with rationale; PR includes `rg` output. |

---

## 8. Operational and observability notes

- `transport.queue.saturated` remains the stable audit reason.
- The queue labels remain `inbound` and `outbound`.
- `outbound_depth` diagnostics now report bytes. This is an intentional unit change; update text that describes the field if any CLI docs mention frame counts.
- Byte caps are local config knobs; no migration or peer negotiation is required.
- Because this closes links on saturation, recovery remains reconnect/backfill through the existing sync engine.

---

## 9. Security, reliability, and performance implications

### Security

- Positive: a malicious or slow peer can no longer consume memory by sending many maximum-size frames under a frame-count budget.
- Positive: governance/checkpoint/session traffic is less vulnerable to content backlog starvation.
- Must preserve: admission-before-bytes in `handler.rs`; this issue starts only after a peer is admitted or provisionally admitted.
- Must avoid: logging message bodies or `ProveCapability.capability_secret` during queue classification or audit.

### Reliability

- Saturation still tears down the link, so reconnect/backfill remains the recovery model.
- A custom queue must wake consumers correctly on enqueue and close; missed notifications could stall sync.
- `Shared::unregister` must close the queue so writer tasks do not outlive removed peers.
- Inbound queue closure must cause the pump to exit cleanly during shutdown.

### Performance

- Queue operations are under a small `Mutex` and should be cheap for N≤5 rooms.
- Inbound priority classification decodes `SyncMessage` once before enqueue and the pump decodes again. This is acceptable for the hardening slice; avoid room-event payload parsing.
- Byte counters are `usize`; caps are tiny relative to 64-bit and acceptable on supported platforms. Use checked/saturating arithmetic in queue internals to avoid overflow on malformed tests.

---

## 10. Rollout and rollback plan

### Rollout

1. Land queue module and unit tests behind existing default behavior shape.
2. Update transport/peer/node wiring.
3. Update diagnostics docs/comments.
4. Run net crate tests, loopback tests, grep audit, then `scripts/verify.sh`.
5. Review with transport/security attention because this is `area/transport` and changes recovery behavior under pressure.

### Rollback

If byte-priority queues regress delivery or cause deadlocks:

- Revert the queue module and wiring as one coherent change.
- Restore `inbound_frame_capacity` and `outbound_frame_capacity` only as a temporary emergency rollback.
- Keep any independent bounded `PipeQuery` conversion if it is correct and low risk, or revert it separately if it complicates rollback.

No data migration rollback is needed.

---

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Custom async queue has a wakeup bug | Sync stalls under load | Keep implementation minimal; unit-test enqueue-before-wait, wait-before-enqueue, close-with-empty, close-with-buffered frames. |
| Priority mapping differs from #134 §12.2 intent | Wrong traffic starves | Keep mapping in one helper with tests; document v1 assumptions; escalate exact mapping as open question if #134 text is available. |
| Governance still blocked by peer cap if peer cap is full of non-content frames | Admin-tip delay | V1 content is capped at 2 MiB stream bucket, leaving peer headroom; add tests. Future eviction can be a Track 2 decision. |
| Double-decoding inbound frames costs CPU | Higher CPU under malicious input | Frame size remains capped at 1 MiB; N≤5; avoid parsing nested `WireEvent`s. |
| Closing link on low-priority saturation drops queued high-priority frames | Recovery churn | Saturation only occurs when a new frame cannot fit. Governance can fit when only content stream is full. Existing reconnect/backfill handles churn. |
| Public `NetConfig` field rename breaks downstream code | Compile failures | Crate is `publish = false`; break is intentional to correct units. Update SDK facade examples if needed. |
| Grep acceptance pulls pipe control channels into scope | More files touched | Convert `PipeQuery` to bounded in the same PR, but do not alter authorization decisions. |

---

## 12. Assumptions

- The local repository does not contain the full text of #134 §12.2/§12.3, so this spec derives v1 mapping from the issue text and current `SyncMessage` variants.
- V1 has one logical subscribed event stream per peer; there is no stream id on the wire.
- `Events` frames are treated as content/publication at the transport layer even if their contained room events are membership/governance events, because parsing room events in transport would blur the engine validation boundary.
- The current close-on-full behavior is desired for true budget exhaustion.
- `outbound_depth` changing units from frames to bytes is acceptable because the issue explicitly requires `depth()` to report bytes.

---

## 13. Open questions

1. Does #134 §12.2 intend `Session` traffic (`ProveCapability`) to be above governance, equal to governance, or between governance and checkpoint? This spec maps it to checkpoint priority to keep it above content without making it governance.
2. Should `NotFound` be `Reconciliation` with content priority rather than blob-hints/lowest priority? The current spec treats it as low-priority because it carries absence hints and can be regenerated.
3. Should future v2 parse event families inside `SyncMessage::Events` to prioritize membership events differently, or should that remain an engine-level concern?
4. Should saturation audit include byte counters/caps in a future additive `AuditSink` method? This spec keeps the trait unchanged to minimize surface churn.
5. Should `PipeForwarder::outcomes` be converted to bounded in this issue for a stricter interpretation of “network-derived,” or is it acceptable as local-loopback outcome reporting guarded by `PIPE_MAX_CONCURRENT_FORWARDS`?
