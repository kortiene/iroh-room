# Changelog

All notable changes to the `iroh-rooms` SDK façade are documented here. See
`src/lib.rs` for the versioning policy: within `0.x`, the **stable** tier
changes only on a minor bump (with an entry here and a deprecation window
where feasible); the **experimental** tier may change on any release.

## Unreleased

- Fixed the three stacked defects behind the N=40 gossip-overlay load collapse
  (the reason rc.4 held #171/#173 out of shipped binaries), isolated by a
  seven-step single-variable experiment ladder on the corrected spike-N40
  harness (`crates/spike-N40/results/results-gossip.md`):
  - **iroh-gossip message-size kill-switch** (`iroh-rooms-net`):
    `spawn_gossip_actor` now sets `max_message_size` to the 1 MiB wire frame
    cap. The 4096-byte default was a whole-connection kill-switch — one
    broadcast of an `Events` frame over 4 KiB (our frames are byte-budgeted
    against the wire cap, not 4 KiB) errored the send loop and killed the
    gossip link to **every** neighbor at once, measured as cluster-wide
    gossip death at ~event 10 under a 5 events/s load. Only compiled under
    `gossip_overlay`, which shipped binaries do not enable.
  - **Fan-out/serve routing split** (`iroh-rooms-core` / `iroh-rooms-net`):
    `Outgoing` gains a `fanout` flag, set only by the engine's accept-path
    fan-out — the one emission that is a broadcast by nature. With a mesh
    installed, fan-out `Events` ride the gossip broadcast alone (a dual-path
    per-peer copy multiplied every event into the seed hubs by ~N-K and
    drowned them), while targeted serves (pull responses, backfill,
    bootstrap closures) never broadcast (per-requester batches would spam
    the mesh with duplicates) and keep the queue path's per-link FIFO.
    Transports without a mesh ignore the flag — no-gossip builds are
    byte-identical in behavior. **Experimental-tier API note:** consumers
    constructing `Outgoing` literals must now populate `fanout` (`false` for
    anything except an accept-path fan-out).
  - **Bounded anti-entropy pull fan-out** (`iroh-rooms-core`): `on_tick` now
    sends its `WantMembership` + `WantRecentChat` pulls to a deterministic
    rotating window of `SyncConfig::pull_fanout_peers` peers (default 3,
    validated non-zero) instead of every connected peer. A high-degree node
    that fell slightly behind previously summoned up to N-1 overlapping
    serve batches every tick — measured at N=40 x 5 events/s as the inbound
    budget saturation that closed hub links (#141's fail-closed recovery)
    and ignited a reconnect-churn cascade; the serve storm, not gossip and
    not fan-out duplication, was the invariant initiator across the
    experiment ladder. The rotation reuses the per-tick claim rotation, so
    consecutive ticks sweep the whole peer set: convergence is delayed by at
    most ceil(peers / bound) ticks, never lost. The tiny admin-tip
    advertisement still reaches every peer every tick (fork-detection
    latency is load-bearing). **This change is active in shipped binaries**
    (it does not depend on `gossip_overlay`): rooms at the shipped 5-member
    cap have at most 4 peers, so a behind node sweeps them in 2 ticks
    (~300 ms) instead of 1. **Mixed-version note (docs/compatibility.md):**
    pulls are ordinary requests and responders are unchanged, so mixed
    rc.4/next rooms interoperate with no coordinated upgrade; the only
    observable difference is that a many-peer node paces its pulls.
  - **Per-plane inbound accounting** (`iroh-rooms-net`): gossip-delivered
    frames now charge a separate ledger in the byte-bounded inbound sink,
    same caps as the event-plane ledger. With fan-out riding the broadcast,
    hub-scale gossip fan-in is real steady-state volume; on the shared
    ledger it could saturate the event-plane budget whose exhaustion makes
    the event-plane reader close the connection — converting gossip volume
    into link churn. Gossip-ledger saturation drops the gossip frame
    (audited, queue=`gossip`) and the link stays up; anti-entropy re-pulls
    cover the gap. Only compiled under `gossip_overlay`.

  With all four in place the corrected loopback matrix is clean at every
  measured cell — N=40 x 5 events/s: 300/300 delivery on every node, zero
  queue saturations, zero reconnect churn (previously 100/300, 28k
  saturations, 775 reconnects/s). The overlay and the 40-member cap remain
  **disabled in shipped binaries**: re-enablement still gates on real-network
  overlay evidence and the admin fail-closed recovery story.
- Made `member.removed` the removed device's receipt-gated final room fact.
  The sender now deauthorizes and detaches routing immediately, but keeps
  that exact connection generation alive for at most five seconds while a
  bounded terminal `events` envelope drains; a current peer acknowledges only
  after the removal is present in its room-scoped durable store, then physical
  teardown completes. Both envelope and receipt have fixed queue reserves, so
  saturated ordinary content cannot prevent current peers from enqueueing the
  lifecycle exchange. The
  receipt is device/room/event-id/nonce/generation bound, and no other frame
  from the revoked peer passes the post-removal admission gate. **Mixed-version
  behavior:** terminal envelopes retain the existing `type = "events"` wire tag
  and add fields that rc.4 decoders ignore, so a valid envelope remains
  wire-decodable as ordinary events and normally folds, but an rc.4 peer sends
  no receipt and has no terminal inbound-queue reserve. The upgraded sender
  therefore uses the five-second grace before closing; a saturated or stalled
  legacy recipient remains best-effort. No coordinated upgrade is required.
  Upgrade admins/senders first to remove the old immediate-close race; a room
  whose admin is still rc.4 retains the previous behavior until that admin
  upgrades.

## 0.1.0-rc.4 - 2026-07-30

- Landed a **bounded gossip overlay for `Events` fan-out** and a feature-gated
  40-member ceiling, but **kept both out of the shipped binaries** (issues #171
  and #173, `iroh-rooms-net` / `iroh-rooms-core`). The shipped CLI and this
  façade's `experimental` feature no longer enable `gossip_overlay`, so this
  release's runtime topology and active-member ceiling are **unchanged from
  rc.3**: full mesh, `MAX_ACTIVE_MEMBERS = 5`. The reason is the project's own
  gate. `specs/gossip-overlay-events-fan-out.md` Step 8 raises the cap "only
  after Step 7 passes", and Step 7's acceptance criterion is "no cascade at
  1 event/s, connectedness >95%, delivery >95% at every N". Re-running the
  matrix through the committed spike-N40 harness measures the opposite — a
  cascade at 1 event/s with both figures under 95%, at N=5 as well as N=40
  (`crates/spike-N40/results/results-gossip.md`). The overlay code stays
  compiled, tested under `--all-features`, and available to a consumer that
  opts in explicitly via `iroh-rooms-net/gossip_overlay`; it is simply not what
  a shipped binary runs. What the overlay does, for when it is re-enabled:
  `Shared::route` broadcasts `SyncMessage::Events` on a
  deterministic per-room `iroh-gossip` topic among admitted device keys whenever
  a mesh is installed for the room; every pull/query variant stays on the
  point-to-point per-peer queue, which relies on the per-link FIFO that gossip's
  epidemic delivery does not provide. Delivery is deliberately **dual-path** —
  an `Events` frame still lands on the destination peer's outbound queue when a
  live writer exists, and the engine's `event_id` G-set dedup makes the doubled
  delivery idempotent. The structural reject-before-bytes admission guarantee is
  preserved: `GossipProtocolHandler` gates `GOSSIP_ALPN` with the same
  `Arc<dyn Admission>` instance as the event plane and closes before the inner
  gossip handler runs (threat-model row T26). `PeerManager::desired_seeds` bounds
  each node to `GOSSIP_BOOTSTRAP_SEEDS` warm links instead of `N - 1`, and
  `MAX_ACTIVE_MEMBERS` moves 5 → 40 behind the `large_rooms` core feature, valid
  **only** when paired with `iroh-rooms-net/gossip_overlay`; a `const` assertion
  in `iroh-rooms-net` pins the pairing at compile time (no-gossip builds must see
  5, gossip builds must see 40), so a mismatched cap/topology build cannot link.
  That assertion is what makes holding the overlay back a safe one-line change
  rather than a risky one. No wire-format, canonical-CBOR, signature,
  membership-fold, authorization, or `SQLite` schema change; `GOSSIP_ALPN` is an
  additional ALPN, not a change to `EVENT_ALPN`, and it is not registered at all
  in a build without the feature. **Upgrade note — mixed rc.3/rc.4 rooms:**
  because the shipped topology and ceiling are unchanged, rc.3 and rc.4 peers
  interoperate normally and no coordinated upgrade is required. Members may
  upgrade one at a time.
- Stopped fanning out live events to unproven provisional dialers (issue #121,
  `iroh-rooms-net`): this closes the residual rc.3 shipped alongside the #112
  capability-proof gate. An unproven provisional dialer reaching `Connected` no
  longer enters the engine's peer set — its `engine.on_connect` is deferred until
  its capability proof verifies or its accepted join upgrades it to a member — so
  until then the engine never fans newly accepted events out to it, never
  advertises the admin tip or heads to it, and never pulls from it. Previously
  history was gated but chat published **while** an uninvited dialer stayed
  connected during an open `room tail --accept-joins` window was still pushed to
  it. Both orderings are handled: a proof processed first marks the device proven
  so the `Connected` transition handshakes immediately, and a `Connected`
  processed first parks the peer in the pump's deferred set, flushed on proof
  verification and on join upgrade, dropped on disconnect. The e2e regression
  seeds a worst-case attacker with a stale copy of the room log (so any leaked
  push would fold cleanly and be visible) and asserts it receives nothing, with a
  proven invitee connected alongside as positive control.
- Guarded link teardown with a per-device **connection generation** (issue #126,
  `iroh-rooms-net`): per-device provisional/proven marks, outbound writers, and
  peer-table state were mutated by per-connection accept/dial tasks carrying no
  link identity, so a superseded link's late close clobbered its successor. On a
  double-connect this was a join-bootstrap **gate bypass** — an unproven
  provisional dialer holding `conn1` opens `conn2` and closes `conn1`, whose
  `clear_provisional` races the pump between clearing the mark and unregistering
  `conn1`'s writer, leaving `conn2` live but no longer marked provisional and
  therefore served un-gated. Each registered link is now stamped with a monotonic
  per-device generation (`Shared::register_link`) and teardown is a
  compare-and-swap (`Shared::teardown_if_current`) that clears state only while
  the link is still the device's current generation; the bump and the provisional
  mark are set in one critical section under the generations lock. Generations
  are never reset or pruned per device, so an ABA stamp reuse cannot let a
  long-superseded teardown clobber a same-numbered successor. The manager's
  deauthorize path keeps its unconditional unregister — a forced roster teardown
  is correct regardless of generation.
- Stopped stale dial loops from stomping live links (issue #136,
  `iroh-rooms-net`): after a peer rebinds to a new UDP port under the same device
  key, the remote's stale-address dial loop redials the dead address indefinitely,
  and its unguarded backoff-tick writes overwrote the peer-table entry of a device
  whose newer **inbound** link was alive and carrying data — observed as a peer
  reading connected with path `none` for minutes while data flowed. #126 guarded
  established-link teardown but left four non-terminal dial-loop paths unguarded
  (top-of-iteration `Connecting`, the `AdmitProvisional` backoff, the
  `Established::Failed` stream-open error, and the primary defect, the
  failed-connect `set_offline(Unreachable)`). New `Shared::set_offline_if_no_link`
  / `set_connecting_if_no_link` perform check-and-set under the same generations
  lock `register_link` takes, reading liveness from the per-device outbound-queue
  map, so a concurrent link registration fully serializes against the guard. The
  audit calls on the failed-connect and `Established::Failed` arms are gated on
  the guard's return so no spurious offline audit fires while a live link carries
  data; the two terminal `Unauthorized` paths stay unguarded as deliberate
  terminal states.
- Retried failed store inserts instead of losing accepted events (issue #119,
  `iroh-rooms-core`): `store_and_fanout` logged and returned when `store.insert`
  failed, but the membership fold had already committed the `Accepted` verdict, so
  fold and store disagreed for the rest of the session and later descendants
  persisted above a permanently missing parent. The accepted `ValidatedEvent` is
  now queued and retried once per `on_tick`; a landed retry runs the full deferred
  accept bookkeeping through the new `apply_insert_outcome` shared with the direct
  path, and the store's insert-time lamport propagation re-places descendants
  stored above the hole, healing it locally with no peer involved. Nothing is
  fanned out or fed to subscribers until the event is actually servable. The retry
  is bounded on both axes (`store_retry_attempts` per event and
  `max_store_retry_total` queued events, both validated non-zero); exhaustion or
  overflow abandons the event with a logged, counted drop and a CRITICAL
  `store_degraded` `TrustDecision` that survives restart.
- Made three silent anti-amplification cliffs observable (`iroh-rooms-core`):
  exceeding `max_backfill_depth`, `max_parked_total`, or
  `max_unconfirmed_tip_attempts` permanently lost events or weakened a security
  property while every connectivity signal still read healthy — a measurement
  caught a node stranded 773.6 s reporting `peers=24, accepted=0` with flat RSS
  and nothing to point an operator at it. Each now records a CRITICAL
  `TrustDecision` on the same path #119 uses for `store_degraded`:
  `backfill_depth_exceeded` (a chain gap deeper than the chase bound, thereafter
  unrecoverable through backfill), `park_overflow` (oldest-first eviction of a
  parked frame), and `admin_tip_expired` — the most serious, because clearing the
  suspicion unconditionally on reaching zero attempts was a fail-**open** of the
  removal-sensitive access gate; it also bumps a new `suspect_tip_expired`
  counter. **No constant value changed** — raising these bounds would move the
  cliff rather than remove it. The two hot, floodable paths latch their CRITICAL
  record to the first occurrence per session and let the counter carry ongoing
  volume, so a fabricated deep chain or park flood cannot turn the audit sink into
  its own denial of service; the per-event log line still names every dropped id.
  All three codes round-trip `trust_row_to_decision`, so they survive a restart.
- Recorded capability-proof outcomes in the CLI's local audit sink (issue #122,
  `iroh-rooms-cli`): #112 added `bootstrap_capability_proven` /
  `bootstrap_capability_rejected` as default-no-op `AuditSink` hooks overridden
  only by `TracingAudit`, and the CLI installs no tracing subscriber — so in the
  shipped binary a proof accept or reject produced no audit record at all, when a
  *rejected* proof (someone probing an open join window with a bad or replayed
  invite secret) is exactly what the local trail exists for. `LocalAudit` now
  overrides both hooks with `join.bootstrap.capability_proven` /
  `join.bootstrap.capability_rejected` ndjson lines in the established
  `{ts_ms, event, peer}` shape carrying the public peer id only — never any part
  of the proof — with a line-contract test asserting no `invite_id` or secret
  appears in any spelling. `StderrAudit` renders the reject as the established
  `warning[<code>]:` line and the accept as an informational note.
- Enforced the active-member ceiling in the fold and made anti-entropy adaptive
  (issue #137, `iroh-rooms-core` / `iroh-rooms-net`): the documented ceiling was
  previously unenforced, so an oversized room silently collapsed rather than
  failing closed. The fold now returns a `room_full` reject — the 15th rejection
  reason, with error taxonomy and conformance tests updated — when a
  not-currently-active subject joins a room already at `MAX_ACTIVE_MEMBERS`, and
  sync anti-entropy ticks became adaptive so a converged mesh quiesces. This
  entry's original frame-count-bounded event-plane queues were superseded within
  this same unreleased window by the byte-bounded queues below (issue #141), and
  its `MAX_ACTIVE_MEMBERS = 5` ceiling by the gossip-backed 40 above (issue #173);
  the `room_full` reject and the adaptive tick are what reach rc.4.
- Converted the v1 event-plane queues from frame-count-bounded to
  **byte-bounded priority queues** (issue #141, `iroh-rooms-net` + a CLI
  diagnostic wording pass): the inbound sink and the per-peer outbound queue
  were previously `mpsc` channels bounded by frame count (default 256 each);
  they now charge encoded `SyncMessage` body bytes against the #134 §12.3
  budgets. **Experimental-tier `NetConfig` break:** `inbound_frame_capacity` and
  `outbound_frame_capacity` (default 256 each) are removed and replaced by
  `inbound_peer_queue_bytes` / `outbound_peer_queue_bytes` (default 8 MiB each),
  a new `stream_queue_bytes` (default 2 MiB per subscribed stream — v1 carries
  one logical event stream per peer, so this is the per-peer content bucket),
  and a new `pipe_query_capacity` (default `MAX_CONCURRENT_BIDI_STREAMS` = 128).
  Frames are classified by `SyncMessage` variant into four priorities,
  `governance > checkpoint > content > blob-hints` (`AdminTip` / `WantMembership`
  > `Heads` / `ProveCapability` > `Events` / `WantRecentChat` > `WantEvents` /
  `NotFound` / undecodable); governance, checkpoint, and session control charge
  the per-peer cap only, so a `AdminTip` / `WantMembership` / `Heads` /
  `ProveCapability` frame still lands when the content stream is saturated. The
  verbose CLI `outbound_depth=<N>` diagnostic and `OutboundQueue::depth()` now
  report **queued body bytes** (an intentional unit change from frames — the
  README diagnostic wording is updated to match). A new `InboundReceiver` type
  replaces `mpsc::Receiver<Inbound>` as `NetTransport::take_inbound`'s return,
  yielding frames in priority order; it is exported from `iroh-rooms-net` but,
  like the prior `mpsc::Receiver`, is not re-exported through this façade (a
  consumer driving `NetTransport` directly names it via the net crate). The
  Pipe plane's engine-query control channel (`PipeQuery`) is also now bounded
  (`pipe_query_capacity`), closing the last unbounded channel on a
  network-reachable ALPN path; saturation fails closed — `snapshot` /
  `pipe_opened` return `None` and `pipe_is_closed` returns `true`, the same
  outcome as a vanished pump — so authorization decisions never branch on queue
  state. The recovery shape on true budget exhaustion is unchanged: the
  offending frame is dropped, `transport.queue.saturated` is audited (queue
  `inbound` / `outbound`), and the peer link is closed so reconnect/backfill
  becomes the recovery path. No wire-format, canonical-CBOR, signature,
  membership, validation, authorization, admission, or `SyncMessage` protocol
  change; the v2 wire protocol's byte budgets remain deferred (this hardens v1
  independently). **Upgrade note: any consumer that explicitly set the old
  `inbound_frame_capacity` / `outbound_frame_capacity` fields must rename them to
  the new byte-named fields — `NetConfig::default()` carries the §12.3 budgets,
  so callers that only override `mode` (the CLI and all SDK examples) are
  unaffected.**
- Cached the membership projection at the sync engine (issue #142,
  `iroh-rooms-core`): an in-memory performance optimization with no wire,
  signature, validation, authorization, or `SQLite` schema change. `RoomMembership`
  remains the correctness authority; `SyncEngine` now memoizes the current
  `MembershipSnapshot` and refreshes it only when the fold's new
  `membership_projection_generation` advances — i.e. only when a
  membership-affecting event (`room.created`, `member.invited`,
  `member.joined`, `member.left`, `member.removed`) newly accepts. Content
  publishes (`message.text`, `file.shared`, …), duplicates, rejections, and
  still-buffered frames never bump it, so a busy content stream no longer pays a
  fold recompute on every `SyncEngine::snapshot()` / reconciler / digest /
  anti-amplification signer read. The cache is rebuilt once from the fold during
  `SyncEngine::open` (startup rebuild is intentionally **not** counted), follows
  the fold rather than the store to preserve the existing #119 fold/store
  divergence semantics, and refreshes immediately after every `fold.ingest` so a
  membership event early in a multi-frame `Events` loop is visible to later
  frames in the same batch. The re-exported `SyncCounters` gains one additive
  field — `membership_projection_recomputes: u64` — counting only runtime cache
  refreshes caused by a fold membership-generation change after `open`, so
  diagnostics/tests can prove a content-only publish leaves it unchanged. The
  fail-closed / admin-completeness overlay remains independent of the membership
  cache, and `BlobAclView` referenced-hash refresh on `file.shared` is unchanged.
  No behavioral change to admission, `PeerManager::reconcile`, or any access
  verdict.
- Made the approach to the active-member ceiling observable (issue #144,
  `iroh-rooms-core` / `iroh-rooms-net` / `iroh-rooms-cli`): no-gossip/full-mesh
  builds keep the original hard `MAX_ACTIVE_MEMBERS = 5` cap and its
  `RejectReason::RoomFull` reject, while the `large_rooms` core feature raises
  the cap to 40 only when paired with `iroh-rooms-net/gossip_overlay` — which
  the shipped CLI and the experimental SDK do **not** enable in this release, so
  shipped binaries observe the cap of 5. The re-exported `MembershipSnapshot` gains
  two additive, side-effect-free methods — `active_member_limit() -> usize`
  (returns the compiled `MAX_ACTIVE_MEMBERS`) and
  `active_member_headroom() -> usize` (`limit.saturating_sub(active_count)`) —
  so status/audit callers can render headroom without importing the constant
  separately. The online `AuditSink` trait gains a default-no-op
  `active_member_threshold_reached(room_id, active, max, remaining)` hook; the
  CLI's `room members <ROOM_ID> --status` prints an
  `active: <n>/<max> (<k> slots remaining)` line, and a live observer
  (`RoomReconciler`) emits a one-shot-per-crossing `room.active_members.near_cap`
  audit record (plus a `warning[room_near_capacity]:` stderr line) when the
  locally observed active count crosses from below `MAX_ACTIVE_MEMBERS - 1` to
  at/above it. Note: `ACTIVE_MEMBER_WARNING_THRESHOLD` and
  `active_member_warning_crossed` are added to `iroh_rooms_core::membership` but
  are not yet re-exported through this façade (tracked as SDK-coverage drift in
  `docs/sdk-coverage.md`).
- Added a bounded early event-id dedup cache and batched `SQLite` accepted-event
  commits to the sync engine (issue #143, `iroh-rooms-core`): two local-only
  performance guardrails from #134 §22.2. The engine now decodes the outer
  `WireEvent`, recomputes the id from `wire.signed` (never the advisory
  `wire.id`), and consults an in-memory FIFO cache of recently persisted ids
  *before* signature verification or any store work — a replay inside the cache
  window is a cheap no-op counted by a new `SyncCounters::early_duplicates`
  counter (distinct from the existing post-store `duplicates` counter, which
  still covers cache misses, evictions, and the cap-0 rollback case). Consecutive
  fold-accepted events are then persisted in one `BEGIN IMMEDIATE` transaction
  per `SyncConfig::store_insert_batch_size` (default 32; `1` is the supported
  disable-batching knob; `0` is invalid), so `N` consecutive accepted events
  commit in `⌈N/batch⌉` transactions rather than `N`. The cache is populated
  only after the store proves an id is persisted, so a bad-signature first
  arrival cannot poison it and suppress a later valid copy; the capacity
  (`SyncConfig::early_event_id_dedup_cache_entries`, default 4096; `0` disables)
  bounds replay-flood memory. The #119 retry path is preserved: a failed batch
  is all-or-nothing, every affected event enters the bounded retry queue with
  `store_insert_failed` incremented by the affected count and a distinct
  `store insert failed (batch)` log line, and no fan-out, push-feed emit, or
  accept counter runs until the insert lands. Post-commit side effects
  (`apply_insert_outcome`) remain centralized and are applied in input order on
  success, so insert-then-fanout ordering is unchanged. The shipped
  `EventStore::insert_all` stats API now delegates to a new public
  `EventStore::insert_all_outcomes` that returns the per-input `InsertOutcome`
  sequence the engine needs. No wire-format, canonical-CBOR, signature,
  membership, or `SQLite` schema change; the new state is in-memory only and the
  cache is seeded from persisted room ids on `SyncEngine::open`.

## 0.1.0-rc.3 - 2026-07-16

- Gated the join-bootstrap membership closure on a **capability proof** (issue
  #112, `iroh-rooms-core` / `iroh-rooms-net` / `iroh-rooms-cli`): since #111
  `WantMembership` serves the causal closure of the authorization class, which
  can carry chat that entered the membership ancestry — and while a join window
  (`room tail --accept-joins`) was open, any provisionally-admitted unknown
  device could send `WantMembership` and read that chat with no invite. A
  provisional peer must now present the new
  `ProveCapability { room_id, invite_id, capability_secret }` message; the
  responder recomputes the invite `capability_hash` against an on-log
  `member.invited` and serves the closure only after a valid proof. This is a
  bootstrap **privacy** gate only — the convergent `gate_join` remains the
  unchanged authorization authority on the actual join. The join CLI,
  `Node::spawn_join_bootstrap`, and the SDK examples (PR #120) present the
  proof automatically before the bootstrap pull, so genuine invitees join
  unchanged; the deterministic engine treats a forwarded or replayed proof as
  a no-op. Tracked residuals: outbound live fan-out to a still-connected
  unproven provisional dialer is not yet gated — history no longer leaks, but
  chat published while the join window is open and the dialer stays connected
  does (issue #121) — and proof outcomes surface only through the tracing
  audit hooks, not the CLI's `audit.ndjson` (issue #122). **Upgrade note: an
  rc.2 joiner never sends the proof, so an rc.3 admin serves it no provisional
  bootstrap and its `room join` times out — joiners must run rc.3 against an
  rc.3 admin.**
- Healed deep pure-chat gaps for a returning member (issue #114,
  `iroh-rooms-core`): a member returning across a >64-deep linear pure-chat
  gap accepted no new chat. Three stacked defects: the backfill chase
  re-requested parents that were already parked in flight (burning the
  per-author token budget on no-ops while the one gap-advancing request
  deterministically lost the token race), the tick retry re-derived missing
  parents from the `events` table (empty for a parked frame — a silent no-op),
  and a legitimate >64-deep single-author chain overflowed the depth and
  per-author park caps (evicting the middle of the chain made its still-parked
  children re-request the evicted parents — eviction thrash). Backfill now
  skips parents already in flight, `retry_park` drives from each parked
  frame's recorded `missing` set, and `max_parked_per_author` /
  `max_backfill_depth` are raised 64 → 1024 (per-author park equals the total
  park so one author's chain cascades in a single pass with unchanged maximum
  memory; the depth bound stays finite, so a phantom-parent chase is still
  dropped at a hard bound — the Gate-D bounded-backfill requirement holds,
  widened). Gaps deeper than the cap still degrade gracefully: bounded chase,
  membership always converges.
- Removed the membership-sync room-size ceiling (issue #113, `iroh-rooms-core`
  / `iroh-rooms-net`): the `WantMembership` requester claimed **every held
  event id** in `have` (required by #111's progress invariant), so at ~30k held
  events the request exceeded the 1 MiB wire frame cap, was dropped at the net
  writer, and membership anti-entropy to that peer silently stalled. The `have`
  entries are now bounded **ancestry claims** — the requester samples its held
  set (placed DAG heads, a recent-lamport slab, and a per-tick rotating window
  over older history; ≤ `membership_have_max_ids`, default 512, ~17 KiB), and
  the responder subtracts each claimed id *plus every stored ancestor of it*.
  An old-style exhaustive claim over an intact store is causally closed and
  expands to exactly itself, so rc.2 requesters are served identically (see the
  upgrade note for the store-hole exception). Claims never include
  causally-unplaced (`NULL`-lamport) rows, so a local store hole keeps being
  re-served until it heals; the rotating window guarantees a claim cannot stay
  pinned in peer-unknown territory (an offline suffix deeper than the whole
  budget anchors within at most `placed-events` ticks). `Events` responses are
  now **byte-budgeted**: a serve larger than one wire frame is split into
  consecutive under-cap messages instead of being dropped whole and re-served
  forever (previously reachable at any room size via ~64 near-16-KiB message
  bodies in the membership ancestry). `SyncEngine::publish` now refuses a
  locally-authored frame too large to ever deliver
  (`SyncError::OversizedFrame`), and the Gate-D `SimNet` enforces the frame cap
  at delivery so this failure class stays visible to the deterministic tests.
  **Upgrade note: a v0.1.0-rc.2 responder subtracts the new bounded claim as an
  exact id set, so a fresh bootstrap against an old responder hard-stalls once
  the joiner holds more than `membership_have_max_ids` + `response_max_frames`
  (~1k) events — every room member, especially the admin, must run a build with
  this fix for rooms past that size. Two rc.2 residuals this fix cannot reach:
  an rc.2 requester whose store has a hole (a swallowed insert error) claims
  the unplaced rows above it, so an upgraded responder covers — and never
  re-serves — the missing ancestor that rc.2-to-rc.2 exact-set subtraction
  would have healed; and an oversized event that entered an rc.2 log before
  the publish guard existed still re-serves-and-drops on every pull to that
  peer (now logged at the responder).**

## 0.1.0-rc.2 - 2026-07-15

- Fixed the join-after-conversation deadlock (PR #111, `iroh-rooms-core` /
  `iroh-rooms-net` / `iroh-rooms-cli`): once any non-admin chat existed in a
  room, no new participant could ever complete `room join` — the invite cites
  the current DAG heads (chat events after a conversation), the membership fold
  requires every `prev_events` parent before classifying, `WantMembership`
  served only the bare authorization class, and the admin drops `WantEvents`
  backfill from provisional peers, a circular deadlock ending in a 10s timeout.
  `WantMembership` now serves the **causal closure** of the authorization class
  (memoized, room-scoped), and the requester's `have` claims every held event
  id, giving guaranteed `ceil(closure/cap)`-round bootstrap progress under the
  512-frame response cap. The net writer now drops a locally-queued oversized
  frame instead of killing the peer stream, and `room join` distinguishes a new
  `membership_incomplete` error (admin responded, ancestry never completed —
  counted per-attempt) from `no_admin_reachable`. Known residuals tracked in
  issues #112 (provisional closure read without capability proof), #113
  (have-list frame ceiling ~30k events), #114 (offline-member deep-chat-gap
  wedge). **Upgrade note: a v0.1.0-rc.1 admin still serves the bare class, so
  joins minted after a conversation keep failing in mixed-version rooms — every
  room member, especially the admin, must run rc.2.**
- Hardened cross-room isolation in the sync engine (PR #106,
  `iroh-rooms-core`): every event-id lookup against the shared event store is
  now room-scoped. Because the store holds every room in one database and
  `event_id` is a globally-unique content hash, unscoped lookups let a row from
  another room be served to a peer via `WantEvents` (cross-room byte
  disclosure), satisfy a local causal dependency, or clear the fail-closed
  admin-tip suspect state. New room-scoped store methods (`contains_in_room` /
  `get_in_room` / `missing_parents_in_room`) close all three. Since `event_id`
  is a unique primary key the scoping is a pure narrowing — legitimate same-room
  sync is unchanged and the reads stay PK point lookups (perf-neutral). No
  façade API change; a behavioral security fix that flows through to any
  online-tier consumer. Regression-tested at both the store and sync-engine
  layers.
- Added a compile-time `relay-only-test` cargo feature (PR #107,
  `iroh-rooms-net` with a façade pass-through) and re-exported the
  `RELAY_ONLY_TEST_BUILD` build-flavor constant through `experimental::session`.
  With the feature on, a `RealNetwork` endpoint suppresses direct UDP transports
  (`clear_ip_transports()`) so all room, blob, and pipe traffic traverses the
  configured relay — a controlled seam for Gate-A relay-throughput
  verification. Off by default and compile-time only, so ordinary binaries
  cannot switch transport policy at runtime and default behavior is unchanged.
  Note: the feature is deliberately non-additive and is enabled by
  `--all-features`; it is dormant under `cargo test` today (no non-ignored
  `RealNetwork` test), but a future such test must gate the seam behind a
  runtime switch to avoid forcing relay-only in CI.

## 0.1.0-rc.1 - 2026-07-07

- Re-exported the online tier's `iroh` transport identities — `EndpointAddr`,
  `EndpointId`, `SecretKey`, `Endpoint` — from `experimental::session`
  (`EndpointId` also from `experimental::blob` and `experimental::pipe_runtime`,
  issue #87): closes the last gap in "a consumer imports only through
  `iroh_rooms::*`". Driving `Node::spawn`/`connect_to`/admission wiring
  previously required a consumer's own direct `iroh` dependency pinned
  byte-identical to `iroh-rooms-net`'s `=1.0.1` — a version-skew trap where
  two resolved `iroh` crates produce incompatible `EndpointAddr` types. `iroh`
  becomes a direct, `experimental`-gated optional dependency of the façade
  (pinned `=1.0.1` to match `-net` exactly, so Cargo unifies to one crate
  instance); a default-features build still cannot name any of these types.
  The reference CLI proves the claim: its direct `iroh` dependency is deleted
  entirely, with every `iroh::` path routed through the façade instead. Purely
  additive — a re-export + import-routing change, no new runtime behavior.
- Added `Node::live_pipe_sessions_for(pipe_id) -> usize` and
  `Node::pipe_session_info() -> Vec<PipeSessionInfo>` (issue #86 / IR-0309,
  `experimental::session` + `experimental::pipe_runtime`): per-pipe
  live-session observability on the owner side, so an owner exposing more
  than one pipe can tell which pipe carries a live forwarding session
  instead of only a node-wide total (`Node::live_pipe_sessions()`). Both are
  pure `&self` reads over the existing session table — no new tracking, no
  engine/pump involvement — and are decrement-correct on every teardown path
  with no separate counter to desync. `live_pipe_sessions()` is unchanged;
  purely additive.
- Added `Node::blob_import(&Path)` / `Node::blob_import_bytes(Bytes)` (issue #84 /
  IR-0308, `experimental::session` + `experimental::blob`): import a file, or
  re-provide in-memory bytes, into the live session's already-open blob store —
  no second `FsStore` open (so no `BlobError::Locked`), no session cycle, zero
  `ConnEvent` disconnects. Pair with `build_file_shared` + `Node::publish` to
  announce the reference. A node spawned without a `BlobServeConfig` returns
  the new `BlobError::NotServing`. Purely additive; existing `Node` methods and
  the exclusive-lock model are unchanged.
- Added `Node::room_events() -> broadcast::Receiver<StoredEvent>` (issue #83 /
  IR-0307, `experimental::session`): a live push stream of every event accepted
  into the room's store — own publish, peer sync, and delayed park-promotion
  all emit here exactly once, so a long-running consumer (e.g. a resident
  daemon driving a UI) no longer has to poll `room_tail`. Lossy on lag like
  `conn_events` (`RecvError::Lagged`, resync via `room_tail` + a seen-set —
  see the method's doc comment for the recipe). Purely additive; existing
  `Node` methods are unchanged.
- Added `examples/example_agent/` (issue #39 / IR-0304): a minimal, runnable
  example agent driven by real command-line arguments — the adapt-me-as-a-
  template evolution of `07_agent_status.rs` — plus a co-located `README.md`
  and a gated integration test. Docs-and-examples only; no SDK surface change.
- Added `JoinBootstrapAdmission::new_dynamic` (issue #88, `experimental::session`):
  the join-bootstrap window (`accept_joins`) can now be read from a shared
  `Arc<AtomicBool>` on every `authorize()` call instead of being fixed at
  construction, so a long-running host (e.g. a resident daemon) can gate
  provisional admission on pending invites without respawning its `Node`.
  Purely additive — `new` and its fixed-`bool` semantics are unchanged, and
  `new_dynamic` is observationally identical to `new` for any fixed flag
  value.

## 0.1.0 — initial surface (IR-0301)

Initial developer-preview release. Defines the SDK boundary:

- Five stable domain modules — `identity`, `room`, `events`, `files`, `pipes`
  — re-exporting the deterministic, conformance-tested protocol layer from
  `iroh-rooms-core` (event authoring/validation, the membership fold, the
  invite ticket codec).
- An `experimental` cargo feature gating the online runtime — `session`
  (transport/admission/connection state), `sync` (the sans-IO engine), `store`
  (the local event store), `blob` (import/serve/fetch), `pipe_runtime`
  (live-pipe forwarding) — re-exported from `iroh-rooms-net` /
  `iroh-rooms-core`.
- A `prelude` module glob-re-exporting the most-used stable types.
- `examples/` mirroring the `docs/getting-started.md` demo, plus doctests on
  every stable module.
- The CLI (`iroh-rooms-cli`) migrated its offline authoring path
  (`identity`, `room` create/members, `invite`, and the `build_*` call sites
  in `message`/`file`) to import through this façade — see
  `docs/sdk-coverage.md` for the full coverage audit.

No crates.io publication yet (`publish = false`); no stability guarantee on
the `experimental` tier.
