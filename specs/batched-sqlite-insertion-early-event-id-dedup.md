# Spec: Batched SQLite insertion + early event-id dedup

**Issue:** #143 — `[CORE] Batched SQLite insertion + early event-id dedup`  
**Labels:** `type/feature`, `area/storage`, `priority/p1`, `risk/low`  
**Owning crate/module:** `iroh-rooms-core` → `src/sync/` and `src/store/`  
**Status:** planning / ready-to-implement. Do **not** implement from this document alone.

---

## 1. Summary

The current sync ingest path validates every replayed event all the way through deterministic CBOR decoding and Ed25519 signature verification, then asks SQLite to reject duplicates via the idempotent store insert path. It also writes each accepted event in its own SQLite transaction, even when a peer delivers a consecutive run of accepted `Events` frames.

This issue should add two local-only performance guardrails without changing the event wire format, canonical CBOR, signatures, membership authorization, or SQLite schema:

1. **Early event-id duplicate rejection.** After a safe outer `WireEvent` parse and event-id derivation from `wire.signed`, check a bounded in-memory cache of recently persisted event ids before running signature validation or touching the store. A cache hit is ignored as `duplicate` and increments a dedicated counter.
2. **Batched store commits.** Consecutive fold-accepted events should be persisted with one SQLite transaction per configured batch using the existing store bulk-insert machinery. Fan-out, push-feed emission, counters, and advisory flags must still happen only after the batch commits.
3. **#119 retry semantics preserved.** If a batch insert fails, no event from that failed transaction may be fanned out or emitted to subscribers; every affected fold-accepted event must enter the bounded store-retry path and later receive the same deferred bookkeeping when it successfully lands.

The intended effect is:

- a replay inside the cache window avoids signature verification and DB work;
- `N` consecutive accepted events commit in `ceil(N / batch_size)` transactions rather than `N`;
- transient `SQLITE_BUSY`/write failures remain recoverable via the existing per-tick retry queue;
- observable ordering remains `insert commit -> fanout/feed`, with failure deferring all side effects.

---

## 2. Repository context read for this spec

### 2.1 Product and protocol context

- `README.md` describes Iroh Rooms as a local-first signed room-event log over SQLite. The room event plane is canonical signed events, membership, deterministic validation, local SQLite persistence, and bounded sync.
- `docs/protocol.md` §5 defines the validation pipeline. Step 11 is currently “Dedup & persist,” and `duplicate` is an ignored, idempotent outcome rather than an error.
- `docs/protocol.md` §4 states that `event_id` is `BLAKE3-256(CSB)` over the exact canonical signed bytes. The outer `WireEvent.id` field is advisory and must not be trusted as the source of truth.
- `docs/security/threat-model.md` lists strict event parsing, event-id recomputation, Ed25519 verification, deterministic membership fold, and idempotent duplicate handling as existing controls.
- `PHASE-0-SPIKE.md` records the original ingest shape as `verify signature -> check membership/authorization -> dedup by event_id -> persist -> fan out`, and issue #134 later calls out early duplicate rejection as a v1 guardrail.

### 2.2 Current store implementation

Relevant files:

- `crates/iroh-rooms-core/src/store/mod.rs`
- `crates/iroh-rooms-core/src/store/model.rs`
- `crates/iroh-rooms-core/src/store/schema.rs`
- `crates/iroh-rooms-core/src/store/tests.rs`

Current behavior:

- `EventStore::insert` (`store/mod.rs:208`) opens a write transaction with `BEGIN IMMEDIATE`, calls `insert_in_tx`, commits, and returns `InsertOutcome::{Inserted, Duplicate}`.
- `EventStore::insert_all` (`store/mod.rs:225`) already inserts many `ValidatedEvent`s in one transaction and returns `InsertStats { inserted, duplicate }`.
- `insert_in_tx` (`store/mod.rs:1005`) uses `INSERT ... ON CONFLICT(event_id) DO NOTHING`, so DB-level duplicate handling is idempotent.
- `insert_all` is currently adequate for store-only stats, but **not sufficient for the sync engine’s ordered side effects**, because it does not return the per-input `InsertOutcome` sequence needed to decide which events should fan out and enter the push feed.
- The schema already stores `event_id` as a raw 32-byte primary key and uses WAL + `BEGIN IMMEDIATE`; no schema migration is required for this issue.
- Store tests already cover duplicate idempotency and `insert_all` stats.

### 2.3 Current sync engine implementation

Relevant files:

- `crates/iroh-rooms-core/src/sync/engine.rs`
- `crates/iroh-rooms-core/src/sync/config.rs`
- `crates/iroh-rooms-core/src/sync/engine_tests.rs`
- `crates/iroh-rooms-core/src/sync/message.rs`

Current behavior:

- `SyncEngine::publish` (`engine.rs:479`) validates one local frame with `validate_wire_bytes`, then calls `deliver`.
- `SyncEngine::on_message` handles `SyncMessage::Events { frames }` by looping frames and calling `deliver_bytes` once per frame (`engine.rs:554`).
- `deliver_bytes` (`engine.rs:874`) currently runs `validate_wire_bytes` before any duplicate check.
- `deliver` folds each validated event through `RoomMembership::ingest`, then immediately calls `store_and_fanout` for accepted events (`engine.rs:889`).
- `store_and_fanout` (`engine.rs:937`) calls `self.store.insert(ev)` for exactly one accepted event, queues #119 retry on failure, and calls `apply_insert_outcome` on success.
- `apply_insert_outcome` (`engine.rs:966`) is the single correct place for post-commit effects: counters, cache invalidation, admin state, push feed, advisory flags, fan-out, and completeness recompute.
- #119 retry is implemented by `enqueue_store_retry` (`engine.rs:1031`) and `retry_store` (`engine.rs:1061`). It guarantees that a fold-accepted event whose store insert failed is retried later and not fanned out before it lands.
- Existing tests in `sync/engine_tests.rs` pin #119 recovery, retry exhaustion, retry queue cap behavior, and peer re-serve healing.

---

## 3. Goals, non-goals, and scope

### 3.1 In scope

1. Add a bounded per-engine event-id dedup cache checked before signature validation and before any store call.
2. Add observability for early duplicate drops via a dedicated `SyncCounters` field.
3. Add a configurable SQLite insert batch size to `SyncConfig`, defaulting to **32**.
4. Batch consecutive accepted events into one SQLite transaction, preserving per-event post-commit side effects in original delivery order.
5. Preserve #119 retry behavior for failed batches: no fanout/feed/counter acceptance until storage succeeds; queued retry remains bounded and eventually records `store_degraded` on exhaustion.
6. Extend store bulk insertion with an outcome-preserving API so the engine can apply side effects correctly.
7. Add deterministic tests for early dedup, batching transaction count, retry recovery, and ordering.

### 3.2 Out of scope

- v2 publication certificates, replica receipts, or any new trust material.
- Changes to canonical CBOR encoding, signed fields, `event_id` derivation, or Ed25519 verification rules.
- Changes to membership authorization, role checks, room capacity, or ancestor-view semantics.
- SQLite schema migration or on-disk format changes.
- Transport frame encoding changes.
- Connection pooling or async store work.

---

## 4. Key design decisions

### D1 — Derive the early cache key from `wire.signed`, never from advisory `wire.id`

The early path must perform only cheap, safe parsing before the cache check:

1. Decode the outer `WireEvent` with `WireEvent::decode(bytes)`.
2. Compute `event_id = signed::event_id_from_bytes(&wire.signed)`.
3. Use that recomputed id as the dedup-cache key.

Do **not** trust `wire.id` as the cache key. The protocol explicitly treats it as advisory. A malicious peer can write any string there, but cannot change `wire.signed` without changing the recomputed id.

Recommended helper:

```rust
fn prevalidate_event_id(bytes: &[u8]) -> Result<EventId, RejectReason> {
    let wire = WireEvent::decode(bytes)?;
    let event_id = signed::event_id_from_bytes(&wire.signed);
    if event_id.to_named_string() != wire.id {
        return Err(RejectReason::IdMismatch);
    }
    Ok(event_id)
}
```

This preserves the cheap `id_mismatch` rejection before the cache hit and avoids signature work. A same-`signed` replay with a bad signature may now be ignored as an early duplicate if the valid event is already in cache; that is acceptable because the first valid stored event already won, the replay cannot mutate signed content, and no state changes occur. Add a test for this explicitly.

### D2 — Only cache event ids after successful persistence

The cache must not be populated merely because a frame parsed or validated. Otherwise a bad-signature first arrival could poison the cache and suppress a later valid copy with the same signed bytes.

Populate the cache only when one of these happens:

- `apply_insert_outcome(InsertOutcome::Inserted, ...)` runs after a successful store commit;
- `apply_insert_outcome(InsertOutcome::Duplicate, ...)` runs after the store proves the id was already persisted but the hot cache missed;
- `SyncEngine::open` optionally seeds the cache from already persisted room ids, bounded by the configured cache capacity.

Do **not** cache fold-accepted events whose store insert failed and entered #119 retry. A peer re-serve must remain able to heal or supersede a pending retry, as pinned by `peer_reserve_clears_pending_retry_exactly_once` in `sync/engine_tests.rs`.

### D3 — Use a deterministic FIFO ring cache

Add a small deterministic cache to `SyncEngine`:

```rust
struct EventIdDedupCache {
    cap: usize,
    set: BTreeSet<EventId>,
    order: VecDeque<EventId>,
}
```

Semantics:

- `cap == 0` disables early dedup and is useful for rollback/bisecting.
- `contains(id)` is read-only and never touches SQLite.
- `insert(id)` is idempotent; if already present, do not duplicate it in `order`.
- When full, evict the oldest inserted id until `len < cap`, then insert the new id.
- Determinism matters for tests and simulation; avoid hash-map randomized iteration.

Default capacity: **4096 event ids**. This is much larger than the MVP room size and recent-sync windows, while memory remains modest.

### D4 — Add an outcome-preserving store batch API

The sync engine cannot use the current `EventStore::insert_all` return value directly because `InsertStats` loses the per-input outcome order. Ordered side effects require knowing, for each input event, whether it inserted or was a duplicate.

Add a new public or `pub(crate)` API in `store/mod.rs`:

```rust
pub fn insert_all_outcomes(
    &mut self,
    evs: &[ValidatedEvent],
) -> Result<Vec<InsertOutcome>, StoreError>
```

Implementation:

- Open one `BEGIN IMMEDIATE` write transaction.
- Iterate `evs` in input order and call existing `insert_in_tx(&tx, ev)`.
- Push each `InsertOutcome` into a `Vec` in the same order.
- Commit once at the end.
- If any insert returns an error, roll back the whole batch and return that error.

Then keep existing `insert_all` as the stats API by delegating to `insert_all_outcomes` and folding outcomes into `InsertStats`. This preserves existing store tests and callers while giving the engine the ordered result it needs.

### D5 — Add `store_insert_batch_size` to `SyncConfig`

Add:

```rust
pub store_insert_batch_size: usize,
pub early_event_id_dedup_cache_entries: usize,
```

Recommended defaults:

- `store_insert_batch_size: 32`
- `early_event_id_dedup_cache_entries: 4096`

Validation:

- `store_insert_batch_size == 0` is invalid, e.g. `store_batch_size_zero`.
- `store_insert_batch_size == 1` is the supported “disable batching” rollback knob.
- Consider rejecting values above `1024` with `store_batch_size_oversized` to avoid very long write transactions.
- `early_event_id_dedup_cache_entries == 0` is allowed and disables the early cache.
- Consider rejecting extremely large cache values above `1_000_000` to avoid accidental memory blowups.

### D6 — Batch only consecutive accepted events, and flush at deterministic boundaries

Introduce an engine-local pending batch structure:

```rust
struct PendingStoreBatch {
    events: Vec<ValidatedEvent>,
    from: Vec<Option<PeerId>>,
}
```

Refactor delivery so accepted events are appended to this batch instead of immediately calling `store.insert`.

Flush the batch when:

- it reaches `config.store_insert_batch_size`;
- the engine is about to emit non-store side effects for a buffered/rejected frame that would otherwise overtake prior accepted fanout;
- the current `SyncMessage::Events` frame loop ends;
- `publish` or `ingest_frame` finishes its single-frame path;
- before returning from `wake_park` processing;
- before any tick/message handler returns `Outgoing` to the transport adapter.

This gives batching for consecutive accepted events while preserving the observable rule that earlier accepted events do not have later backfill/failure outputs leapfrog their post-commit fanout.

### D7 — Post-commit side effects stay centralized in `apply_insert_outcome`

Batch flush should not duplicate fanout/feed logic. It should call the same `apply_insert_outcome` helper that the direct and retry paths use today.

Recommended helper:

```rust
fn flush_store_batch(&mut self, batch: &mut PendingStoreBatch, out: &mut Vec<Outgoing>) {
    if batch.events.is_empty() {
        return;
    }

    self.force_next_tick_pull = true;
    match self.store.insert_all_outcomes(&batch.events) {
        Ok(outcomes) => {
            for ((ev, from), outcome) in batch
                .events
                .iter()
                .zip(batch.from.iter().copied())
                .zip(outcomes)
            {
                self.store_retry.remove(&ev.event_id);
                self.note_event_id_seen(ev.event_id);
                self.apply_insert_outcome(outcome, ev, from, out);
            }
        }
        Err(e) => {
            let n = batch.events.len() as u64;
            self.counters.store_insert_failed += n;
            self.log(&format!("store insert batch failed: {e}"));
            for (ev, from) in batch.events.iter().zip(batch.from.iter().copied()) {
                self.enqueue_store_retry(ev, from);
            }
        }
    }
    batch.clear();
}
```

`note_event_id_seen` should update the early dedup cache. It is safe to call for both `Inserted` and `Duplicate`: either way, the store has proven that the id is persisted.

### D8 — Batch failure is all-or-nothing and feeds #119 retry

Because `insert_all_outcomes` runs inside one SQLite transaction, a failure means no event in that batch is durably inserted. Therefore:

- do not call `apply_insert_outcome` for any event in a failed batch;
- do not increment `accepted` for any event in a failed batch;
- do not push to `pending_ingested`;
- do not fan out;
- do not add event ids to the early dedup cache;
- call `enqueue_store_retry` for each affected event in original order;
- count/log the failure clearly enough to distinguish batch failures from per-event retry failures.

This preserves #119’s core invariant: the fold may have accepted the event, but the node must not announce or serve it until the store lands it.

### D9 — Keep retry insertion per-event initially

Do not batch `retry_store` in the first implementation unless the main batching work is already stable. The existing retry loop is correct, tested, and bounded. It can continue calling `self.store.insert(&ev)` one queued event per id per tick.

A future optimization may batch retry attempts, but only if it preserves these existing tests:

- recovered retry gets exactly-once feed/fanout;
- exhausted retry records `store_degraded`;
- retry queue cap remains bounded;
- peer re-serve can clear a pending retry.

### D10 — No SQLite schema migration

No new tables or columns are required. The dedup cache is in-memory, and batching uses existing `events` / `event_parents` writes. `schema::USER_VERSION` should not change.

---

## 5. Detailed implementation plan

### Step 1 — Extend `SyncConfig`

File: `crates/iroh-rooms-core/src/sync/config.rs`

1. Add fields:
   - `store_insert_batch_size: usize`
   - `early_event_id_dedup_cache_entries: usize`
2. Set defaults to `32` and `4096`.
3. Extend `validate` with the rules in D5.
4. Add unit tests for default validity, zero batch rejection, oversized batch rejection if implemented, and zero dedup-cache opt-out.

### Step 2 — Add the dedup cache type

File: `crates/iroh-rooms-core/src/sync/engine.rs`

1. Import `VecDeque` alongside existing `BTreeMap` / `BTreeSet` imports.
2. Add `EventIdDedupCache` near other private engine structs.
3. Add methods:
   - `new(cap: usize) -> Self`
   - `contains(&self, id: &EventId) -> bool`
   - `insert(&mut self, id: EventId)`
   - `len(&self) -> usize` for tests if useful
4. Add a `dedup_cache: EventIdDedupCache` field to `SyncEngine`.
5. Initialize it in `SyncEngine::open` from config.
6. Optionally seed it from `store.room_event_ids(&room_id)?`, bounded by capacity. If seeded from a `BTreeSet`, the order is deterministic but not recency-based; that is acceptable because the cache is a performance guardrail, not correctness state.

### Step 3 — Add early duplicate counters

File: `crates/iroh-rooms-core/src/sync/engine.rs`

Add fields to `SyncCounters`:

```rust
/// Event-id replays dropped by the in-memory cache before signature validation
/// or store work.
pub early_duplicates: u64,

/// Optional: accepted-event store batch commits that succeeded.
pub store_insert_batches: u64,
```

Only `early_duplicates` is required for the issue acceptance. `store_insert_batches` is useful but can be omitted if tests use a test-only `EventStore` transaction counter.

Keep the existing `duplicates` counter for duplicates observed after the full path, i.e. `InsertOutcome::Duplicate`. Do not merge the two counters unless all existing tests are updated deliberately.

### Step 4 — Add pre-validation id extraction

File: `crates/iroh-rooms-core/src/sync/engine.rs`

Add a private helper using `WireEvent::decode` and `signed::event_id_from_bytes`, per D1.

Update `deliver_bytes` so the flow becomes:

1. `prevalidate_event_id(bytes)`.
2. On `Err(reason)`: increment `rejected`, log `reject.<code>`, return.
3. If `dedup_cache.contains(&event_id)`: increment `early_duplicates`, optionally also log or treat as quiet duplicate, and return.
4. Otherwise run the existing `validate_wire_bytes(bytes, &ctx)`.
5. Continue into fold delivery.

Do not query SQLite on a cache miss. The existing fold/store duplicate paths remain the fallback for cache misses.

### Step 5 — Add outcome-preserving store batch insertion

File: `crates/iroh-rooms-core/src/store/mod.rs`

1. Add `insert_all_outcomes(&mut self, evs: &[ValidatedEvent]) -> Result<Vec<InsertOutcome>, StoreError>`.
2. Implement it with one `begin_write()` transaction and existing `insert_in_tx`.
3. Update `insert_all` to delegate to `insert_all_outcomes` and fold into `InsertStats`.
4. Keep existing `insert_all` behavior and tests intact.
5. Extend store tests with:
   - ordered outcomes for all-new, all-duplicate, and mixed batches;
   - rollback on injected failure if test fault injection is extended to batch paths;
   - transaction count if a test-only counter is added at the store layer.

### Step 6 — Extend test-only store fault/transaction instrumentation

Files: `store/mod.rs`, `store/tests.rs`, `sync/engine_tests.rs`

For deterministic acceptance tests, add test-only instrumentation behind `#[cfg(test)]`:

- `write_tx_count` incremented in `begin_write()`.
- `reset_write_tx_count()` / `write_tx_count()` crate-visible helpers.
- Extend `fail_next_inserts` so `insert_all_outcomes` can fail before touching the database, or add `fail_next_insert_batches` if clearer.

The existing `fail_next_inserts` tests should continue to pass. If reusing it for batches, document whether one failed batch decrements by one or by the number of events. Recommended: one failed `insert` call or one failed `insert_all_outcomes` call decrements by one, because it models a transient transaction-level `SQLITE_BUSY` burst.

### Step 7 — Refactor accepted delivery into a batch

File: `crates/iroh-rooms-core/src/sync/engine.rs`

1. Add `PendingStoreBatch` private struct.
2. Replace immediate `store_and_fanout` calls on the direct inbound path with `batch.push(ev, from)`.
3. Flush at the boundaries in D6.
4. Keep `store_and_fanout` available for retry or rewrite it as a single-event wrapper around `PendingStoreBatch` + `flush_store_batch`.
5. Ensure `apply_insert_outcome` remains the only function that performs post-commit inserted/duplicate effects.
6. Ensure batch flush clears any pending `store_retry` for an id that later succeeds via peer re-serve, matching the #119 behavior at `engine.rs:953-956`.

### Step 8 — Preserve `wake_park` behavior

`wake_park` currently calls `store_and_fanout` whenever a parked event becomes accepted. The safest implementation is:

1. After a successful direct batch flush, call `wake_park` as today.
2. Make `wake_park` use the same batching helper internally, so a cascade of parked accepted events can also batch.
3. Flush the parked batch before each `wake_park` loop iteration returns or before any outgoing messages are returned.

If this is too invasive, keep `wake_park` per-event for the first implementation and document that the batching guarantee applies to consecutive accepted frames from the current inbound/local publish path. The acceptance test for `N` consecutive accepted events should use direct `Events` frames, not parked cascades.

### Step 9 — Keep fanout/feed order stable

For each successful batch, process `outcomes` in the same order as input events:

1. remove matching retry entry;
2. update early dedup cache;
3. call `apply_insert_outcome(outcome, ev, from, out)`.

This preserves:

- original delivery order for `pending_ingested`;
- original fanout order in `out`;
- `InsertOutcome::Duplicate` behavior: count duplicate, no feed/fanout storm;
- `Inserted` behavior: feed exactly once, fan out to peers except `from`.

Do not sort batch entries by `event_id`, lamport, or anything else before applying side effects.

### Step 10 — Verification commands

Before calling the implementation complete, run the standard local gate documented in `CONTRIBUTING.md`:

```bash
scripts/verify.sh
```

During iteration, the smallest useful commands are:

```bash
cargo test -p iroh-rooms-core --all-features sync::engine_tests
cargo test -p iroh-rooms-core --all-features store::tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

---

## 6. Acceptance criteria mapping

| Issue acceptance | Implementation coverage | Test coverage |
| --- | --- | --- |
| A replayed event-id within the cache window is rejected without a signature check or DB hit, verified by a counter | `prevalidate_event_id` + `EventIdDedupCache` + `SyncCounters::early_duplicates` | `early_duplicate_skips_bad_signature_and_store` using a cached event replay with a deliberately bad `sig`; assert `early_duplicates += 1`, no `reject.bad_signature`, and no store transaction delta |
| `N` consecutive accepted events commit in `ceil(N/batch)` transactions, not `N` | `PendingStoreBatch` + `insert_all_outcomes` + `store_insert_batch_size` | With batch size 4 and 10 consecutive accepted frames, assert transaction delta is 3 |
| Store-retry path (#119) still recovers a transient `SQLITE_BUSY` burst | failed batch enqueues every affected event through `enqueue_store_retry`; `retry_store` unchanged | Inject one batch failure; assert no immediate fanout/feed, retry queue fills, first tick stores and applies deferred bookkeeping |
| No regression in `store_and_fanout` ordering: insert-then-fanout, deferred on failure | flush applies outcomes after commit in input order; failed batch applies no side effects | connect a second peer, ingest ordered events, assert outgoing frame ids and push-feed ids match input order; failed batch emits none until retry |

---

## 7. Test plan

### T1 — Early duplicate skips signature and store

1. Seed an engine and accept a valid message so its id enters the dedup cache.
2. Construct a replay with identical `wire.signed` and advisory `id`, but a deliberately invalid `sig`.
3. Ingest the replay.
4. Assert:
   - `counters.early_duplicates == 1`;
   - `counters.rejected` did not increase;
   - logs do not contain `reject.bad_signature`;
   - test-only store write transaction count did not increase;
   - room tail is unchanged.

This proves the duplicate path ran before signature verification and before store insertion.

### T2 — Invalid first arrival cannot poison the cache

1. Start with an empty cache.
2. Send a frame with valid `signed` bytes but bad signature.
3. Assert it is rejected as `bad_signature` and `early_duplicates` remains zero.
4. Send the validly signed copy.
5. Assert it is accepted and stored.

### T3 — Cache window eviction falls back to existing idempotency

1. Configure cache capacity 2.
2. Accept three events.
3. Replay the first event.
4. Assert it does not increment `early_duplicates`; it goes through full validation and is handled by the existing duplicate path (`counters.duplicates` / `InsertOutcome::Duplicate`).

### T4 — Batch transaction count

1. Configure `store_insert_batch_size = 4`.
2. Seed genesis, reset the test transaction counter.
3. Ingest 10 consecutive valid accepted events in one `SyncMessage::Events` message.
4. Assert exactly `ceil(10 / 4) = 3` write transactions.
5. Assert all 10 events appear in the room tail in canonical order.

### T5 — Batch preserves fanout/feed order

1. Connect two peers to the engine.
2. Ingest a batch from peer A that should fan out to peer B.
3. Assert outgoing `Events` to peer B contain the inserted frames in the same input order.
4. Assert `take_ingested()` returns stored events in the same input order.

### T6 — Mixed inserted/duplicate batch outcomes

At the store level:

1. Insert events `[g, m1]`.
2. Call `insert_all_outcomes(&[g, m2])`.
3. Assert outcomes are `[Duplicate, Inserted]`.
4. Assert `insert_all` still returns `InsertStats { inserted: 1, duplicate: 1 }` for the same shape.

At the engine level, if a cache miss permits an old duplicate into a batch, assert the duplicate does not fan out.

### T7 — Failed batch defers all side effects and retry recovers

1. Configure batch size > 1.
2. Inject one transaction-level store failure for the next batch.
3. Ingest multiple accepted events.
4. Assert:
   - no outgoing event fanout;
   - `take_ingested()` is empty;
   - retry queue length equals the number of affected events unless capped;
   - `store_insert_failed` increased by the affected event count.
5. Clear the fault and tick.
6. Assert retry lands the events and emits the same post-commit effects as the old #119 tests.

### T8 — Retry exhaustion and cap behavior still hold

Update existing #119 tests only as needed for new counters/config fields. They should still prove:

- retry budget exhaustion records `store_degraded`;
- retry queue cap drops overflow to `store_degraded`;
- peer re-serve clears a pending retry exactly once.

### T9 — Config validation

Add tests for:

- default config is valid;
- `store_insert_batch_size = 0` is invalid;
- `store_insert_batch_size = 1` is valid;
- dedup cache capacity 0 is valid and disables early dedup;
- oversized batch/cache values are rejected if upper bounds are implemented.

---

## 8. Error model and observability

- Early duplicate is an ignored outcome, not a `RejectReason`, matching protocol `duplicate` semantics.
- `SyncCounters::early_duplicates` is the primary acceptance/operational signal for the new fast path.
- Existing `SyncCounters::duplicates` remains the signal for duplicates discovered after full validation/fold/store.
- A malformed outer `WireEvent` or id mismatch still increments `rejected` and logs `reject.<code>` before cache lookup.
- A genuine store batch failure logs `store insert batch failed: ...`, increments `store_insert_failed` by the number of affected events, and queues retries.
- No new CLI error code is required.
- No secret material is logged.

---

## 9. Security, privacy, reliability, and performance

### Security

- The early path must never trust `wire.id`; it derives the id from `wire.signed`.
- The cache is populated only after a valid event has been durably stored, preventing invalid-frame cache poisoning.
- Ignoring a bad-signature replay of already-stored signed bytes is safe because the event content is already fixed by the signed bytes and the node makes no state change.
- The cache is bounded to avoid unbounded memory growth under replay floods.

### Privacy

No new persistent data or network-visible fields are introduced. The cache is in-memory and contains only event ids already present in the local room log.

### Reliability

- Failed batches are all-or-nothing due to the SQLite transaction.
- #119 retry remains the recovery mechanism for transient store failures.
- Batch size 1 provides a simple behavioral rollback knob for batching.
- Dedup cache size 0 provides a simple behavioral rollback knob for early dedup.

### Performance

- Hot duplicate replays avoid Ed25519 verification and SQLite insert attempts.
- Consecutive accepted events amortize SQLite transaction overhead by up to the configured batch size.
- Larger batches hold the SQLite write lock longer; default 32 is a conservative midpoint of the issue’s suggested 16/32 range.

---

## 10. Rollout and rollback

Rollout:

1. Land store outcome-preserving batch API with tests.
2. Land config fields and validation.
3. Land early dedup with tests.
4. Land engine batching with #119 regression tests.
5. Run `scripts/verify.sh`.

Rollback knobs without code revert:

- Set `store_insert_batch_size = 1` to restore one transaction per accepted event.
- Set `early_event_id_dedup_cache_entries = 0` to disable early dedup.

Full code rollback is low risk because there is no schema migration or wire-format change.

---

## 11. Assumptions

1. The batch target is the `SyncEngine` accepted-event path, not every store caller in the workspace.
2. A bounded in-memory cache is sufficient; duplicate correctness still rests on SQLite’s primary key and store idempotency.
3. Changing the observable handling of an already-cached same-`signed` replay with a bad `sig` from `bad_signature` to early `duplicate` is acceptable because the issue explicitly asks to avoid repeated signature work.
4. The default batch size should be 32 unless maintainers prefer 16 after measurement.
5. No SQLite `user_version` bump is needed.

---

## 12. Open questions

1. Should `early_duplicates` also increment the existing `duplicates` counter for a total duplicate count, or should the two counters remain separate?
2. Should the cache be seeded on `SyncEngine::open` from persisted event ids, or should it start empty and only cover events seen during the current process lifetime?
3. Should `store_insert_batches` be a production counter, or is test-only transaction counting enough for the acceptance criterion?
4. Should `retry_store` be batched in a follow-up issue after this change lands, or intentionally remain per-event for simplicity?
5. What maximum batch size should config validation permit: 512 to match `response_max_frames`, 1024 to match park/retry caps, or no explicit upper bound?

---

## 13. Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Cache poisoning by invalid frames | Valid event could be suppressed | Populate cache only after successful store commit/proven duplicate |
| Losing per-event side-effect order in a batch | Fanout/feed regressions | Use ordered `insert_all_outcomes`; call `apply_insert_outcome` in input order |
| Batch failure accidentally fans out unstored events | Store/fold divergence visible to peers | On batch error, enqueue retry for all events and apply no side effects |
| Large batch holds SQLite write lock too long | Latency for other writers | Default 32; validate upper bound; batch size 1 rollback |
| Existing tests rely on per-event insert counters | Test churn | Add/adjust test-only store transaction instrumentation deliberately |
| Early duplicate changes diagnostics for cached bad-signature replay | Fewer `bad_signature` logs for exact known ids | Document as intentional; malformed/id-mismatch outer frames still reject before cache |
