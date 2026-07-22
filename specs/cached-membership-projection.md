# Spec: Cached membership projection

**Issue:** #142 — `[CORE] Cached membership projection (incremental)`  
**Labels:** `type/feature`, `area/protocol`, `priority/p1`, `risk/medium`  
**Owning crate/modules:** `crates/iroh-rooms-core/src/membership/`, `crates/iroh-rooms-core/src/sync/`, `crates/iroh-rooms-net/src/node.rs`, `crates/iroh-rooms-net/src/manager.rs`, `crates/iroh-rooms-net/src/admission.rs`, `crates/iroh-rooms-net/src/blob/mod.rs`  
**Status:** implemented / landed. This document is now a **design record**; the
authoritative, up-to-date description of the shipped behavior lives in the code
doc-comments on `RoomMembership::membership_projection_generation` and
`SyncEngine::refresh_membership_projection_if_needed`, plus the CHANGELOG entry
under `Unreleased` in `crates/iroh-rooms/CHANGELOG.md`. Section 6 below maps
each issue acceptance criterion to where it is satisfied in the landed code.

---

## 1. Summary

The current runtime keeps the deterministic membership fold as the source of truth, but engine read paths still obtain a `MembershipSnapshot` by calling `RoomMembership::snapshot()`. That call re-folds the membership projection over the accepted membership event set each time a caller asks for it. The fold already excludes non-membership events from per-node ancestor sets, so the cost is not proportional to all content events, but a busy content stream still causes repeated projection work whenever the node reconciler, admission view, blob ACL view, digest, or anti-amplification signer check asks for the current snapshot.

Implement an **in-memory, engine-owned cached membership projection**:

1. `RoomMembership` remains the correctness authority for log-validity and fold semantics.
2. `SyncEngine` caches the current `MembershipSnapshot` derived from the fold.
3. The cache is recomputed only when the fold accepts a membership-affecting transition: `room.created`, `member.invited`, `member.joined`, `member.left`, `member.removed`, or any future event that changes device binding / membership attributes.
4. Content events such as `message.text` and `file.shared` must not trigger projection recomputation.
5. Engine/node read paths must consume the cached snapshot rather than calling `RoomMembership::snapshot()` directly.
6. Add instrumentation so tests can prove the recomputation behavior.

This is an in-memory performance improvement only. It must not change signed events, validation, authorization, sync messages, SQLite schema, or protocol behavior.

---

## 2. Repository context read for this spec

### 2.1 Product and protocol context

- `README.md` describes Iroh Rooms as a local-first collaboration runtime whose room state is derived from an append-only signed event log. Access to files and pipes comes from the current membership snapshot.
- `README.md` documents the active member ceiling (`MAX_ACTIVE_MEMBERS = 5`) and the room event plane as canonical signed events, membership, deterministic validation, local SQLite persistence, and bounded sync.
- `docs/protocol.md` states that membership and connect-time authorization are implemented in `crates/iroh-rooms-core/src/{event,membership}/`, and that blob/pipe access consults the current membership snapshot.
- `PHASE-0-SPIKE.md:50` identifies membership-node ancestor state as a cost in the v1 event fold and calls out amplification from membership projection work.
- `specs/membership-fold-prototype.md` defines the existing deterministic fold semantics and makes the same-set convergence guarantee load-bearing.
- `specs/peer-connection-manager.md` describes `PeerManager` and the node pump consuming live membership snapshots for outbound dial reconciliation and admission.
- `.github/workflows/verify.yml` and `scripts/verify.sh` define the local gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets --all-features`, SDK doctests, and example builds.

### 2.2 Current membership implementation

Relevant files:

- `crates/iroh-rooms-core/src/membership/mod.rs`
- `crates/iroh-rooms-core/src/membership/fold.rs`
- `crates/iroh-rooms-core/src/membership/model.rs`
- `crates/iroh-rooms-core/src/membership/access.rs`
- `crates/iroh-rooms-core/tests/membership_fold.rs`

Current behavior:

- `RoomMembership` stores an in-memory DAG of stateless-validated events and classifies them as `Pending`, `Accepted`, or `Rejected`.
- `Node::membership_ancestors` memoizes only transitive membership-relevant ancestors. Non-membership events inherit a membership view but do not enter that set.
- `RoomMembership::affects_membership` currently treats `RoomCreated`, `MemberInvited`, `MemberJoined`, `MemberLeft`, and `MemberRemoved` as membership events.
- `RoomMembership::snapshot()` builds a `MembershipSnapshot` by collecting accepted node ids and calling the private `fold(&accepted)` helper.
- `MembershipSnapshot` contains the room id, immutable admin, per-identity `Member` records, active status/role/device, and a device-to-identity reverse map.
- Access predicates (`blob_serve_allowed`, `pipe_connect_allowed`) rely on `MembershipSnapshot` and must not change behavior.
- Existing membership tests assert admin-only writes, key-bound joins, sticky departure, removed-dominates, current-snapshot access, bound-device enforcement, cap behavior, and convergence semantics. They should pass unchanged.

### 2.3 Current sync engine implementation

Relevant files:

- `crates/iroh-rooms-core/src/sync/engine.rs`
- `crates/iroh-rooms-core/src/sync/config.rs`
- `crates/iroh-rooms-core/src/sync/engine_tests.rs`
- `crates/iroh-rooms-core/tests/sync_*.rs`

Current behavior:

- `SyncEngine` owns an `EventStore` and a `RoomMembership` fold.
- `SyncEngine::open` rebuilds the fold from persisted events on startup by reading `room_tail`, re-validating stored bytes, and calling `RoomMembership::from_events`.
- `SyncEngine::publish`, `ingest_frame`, and `on_message(Events)` all eventually call `deliver`, which calls `self.fold.ingest(ev.clone())`.
- Accepted events are persisted through the existing batch / retry machinery. Store failures may leave the fold ahead of the store for the session, per the already-landed #119 behavior; this feature must preserve that behavior.
- `SyncEngine::snapshot()` currently returns `self.fold.snapshot()`.
- `SyncEngine::digest()` currently includes `self.fold.snapshot()`.
- `SyncEngine::signer_plausible()` currently calls `self.fold.snapshot()` for the anti-amplification pre-gate.
- `SyncCounters` already provides performance and correctness counters; this feature should add a projection recompute counter there.

### 2.4 Current network read paths

Relevant files:

- `crates/iroh-rooms-net/src/node.rs`
- `crates/iroh-rooms-net/src/manager.rs`
- `crates/iroh-rooms-net/src/admission.rs`
- `crates/iroh-rooms-net/src/blob/mod.rs`

Current behavior:

- `RoomReconciler::maybe_reconcile` calls `engine.snapshot()` on every reconcile opportunity, computes active count warnings, builds `AdmissionView::from_snapshot`, possibly builds `BlobAclView::from_snapshot`, and calls `PeerManager::reconcile(&snapshot)` when the membership-derived admission view changes.
- `PeerManager::desired_devices` derives active member devices from a `MembershipSnapshot` and excludes `self_device`.
- `AdmissionView::from_snapshot` converts a `MembershipSnapshot` plus fail-closed subjects into an admission lookup table.
- `BlobAclView::from_snapshot` converts active devices from the snapshot plus referenced file hashes into a blob ACL lookup table.
- These consumers should continue receiving a `MembershipSnapshot`, but it must come from the engine cache rather than a fresh fold recompute.

---

## 3. Goals, non-goals, and scope

### 3.1 In scope

1. Add an incremental membership-projection generation to `RoomMembership` so the engine can tell when any membership-affecting event has newly transitioned to `Accepted`.
2. Add an in-memory cached `MembershipSnapshot` to `SyncEngine`, rebuilt from the authoritative fold on startup and refreshed only when the fold's membership-projection generation changes.
3. Add instrumentation, preferably `SyncCounters::membership_projection_recomputes`, to prove when runtime cache refreshes occur.
4. Change `SyncEngine::snapshot`, `digest`, and internal snapshot reads such as `signer_plausible` to use the cached snapshot.
5. Ensure `RoomReconciler`, `PeerManager::desired_devices`, admission, and blob ACL wiring continue to receive snapshots via `engine.snapshot()` / the cached engine projection.
6. Add tests showing:
   - accepted `message.text` does not increment the recompute counter;
   - accepted `file.shared` does not increment the recompute counter;
   - accepted `member.joined` increments the counter and updates active members/devices/roles;
   - accepted `member.removed` increments the counter and removes active access;
   - parked membership events accepted by a later parent also refresh the cache;
   - duplicate membership events do not spuriously refresh the cache.
7. Preserve all existing membership, sync, admission, peer manager, and blob ACL behavior.

### 3.2 Out of scope

- SQLite schema changes or any new persisted cache table.
- Changes to signed event bytes, event type registry, canonical CBOR, signatures, `event_id`, or room id derivation.
- v2 governance projection or any Track 2 work (#147/#151).
- Changes to role semantics, invite capability semantics, room capacity semantics, fail-closed semantics, or access-denial taxonomy.
- Replacing `RoomMembership` with an incremental mutable membership state machine. The cache is an engine-level memoization of the existing fold output.
- Reworking offline CLI commands that explicitly fold the persisted store for one-shot local reads. The required cache is for the engine/runtime read paths.

---

## 4. Key design decisions

### D1 — Keep `RoomMembership` as the correctness authority

The cache must never become an independently maintained membership model. The authoritative rules remain in `RoomMembership::ingest`, `RoomMembership::fold`, and `MembershipSnapshot` construction.

The engine cache stores a clone of the fold output and refreshes it by calling `fold.snapshot()` when a membership version changes. This avoids subtle divergence between an incremental updater and the existing deterministic fold.

### D2 — Track fold membership changes with a monotonic generation

Add a private generation counter to `RoomMembership`:

```rust
pub struct RoomMembership {
    // existing fields
    membership_projection_generation: u64,
}
```

Add a public read-only accessor:

```rust
impl RoomMembership {
    #[must_use]
    pub fn membership_projection_generation(&self) -> u64 {
        self.membership_projection_generation
    }
}
```

Increment the generation only when a node transitions from `Pending` to `Accepted` and the event affects the membership projection.

The generation must **not** increment for:

- content events (`message.text`, `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status`);
- duplicates that return an existing accepted verdict;
- rejected events;
- buffered events that have not yet accepted.

The generation must increment for accepted:

- `room.created`;
- `member.invited`;
- `member.joined`;
- `member.left`;
- `member.removed`;
- any future event type that changes active status, role, or device binding.

This generation-based design is important because `RoomMembership::ingest(parent)` can classify previously buffered descendants in the same cascade. The engine cannot safely infer membership changes by looking only at the current event's `Content`; it must compare the fold's generation before and after ingestion.

### D3 — Treat `member.left` as membership-affecting

The issue text explicitly lists `RoomCreated`, `MemberInvited`, `MemberJoined`, `MemberRemoved`, and device binding changes. The current code and protocol also have `MemberLeft`, and the fold treats it as `Removed` status for active/access purposes.

This spec includes `member.left` in the invalidation set. Excluding it would leave the cached active member/device view stale after a voluntary leave and would regress admission, peer reconciliation, and ACL behavior.

### D4 — Cache at the engine, not in the store

Add an in-memory cache to `SyncEngine`, initialized during `SyncEngine::open` after `RoomMembership::from_events` has rebuilt the fold from persisted events.

Recommended shape:

```rust
struct MembershipProjectionCache {
    snapshot: MembershipSnapshot,
    fold_generation: u64,
}
```

Add fields to `SyncEngine`:

```rust
membership_projection: MembershipProjectionCache,
```

`SyncEngine::open` should:

1. rebuild `fold` from persisted events as it does today;
2. read `let fold_generation = fold.membership_projection_generation();`;
3. build `let snapshot = fold.snapshot();` once;
4. store both in `membership_projection`.

The runtime recompute counter should start at zero after `open`. Startup rebuild is expected and should not be interpreted as content-event-triggered recompute. Tests can still baseline counters after `open`.

### D5 — Refresh immediately after any ingest that changes the fold generation

Add a helper on `SyncEngine`:

```rust
fn refresh_membership_projection_if_needed(&mut self) {
    let generation = self.fold.membership_projection_generation();
    if generation == self.membership_projection.fold_generation {
        return;
    }
    self.membership_projection.snapshot = self.fold.snapshot();
    self.membership_projection.fold_generation = generation;
    self.counters.membership_projection_recomputes += 1;
}
```

Call this helper immediately after every `self.fold.ingest(...)` call, before any later operation can read the current snapshot:

- `deliver`, after `match self.fold.ingest(ev.clone())` obtains an `Ingest` result;
- `wake_park`, after `self.fold.ingest(ev.clone())` for parked frames;
- any future path that calls `RoomMembership::ingest` directly.

Immediate refresh is safer than a lazy refresh at entry-point boundaries because `on_message(Events)` can process multiple frames in one loop. A membership event early in the loop may make a later frame's signer plausible or alter admission-sensitive state before `finalize_delivery` runs.

### D6 — Preserve #119 fold/store divergence semantics

The current sync engine can accept an event into the fold before its SQLite insert succeeds. If the insert fails, the event is retried later and the fold remains ahead of the store for the session.

The cached snapshot must follow the fold, not the store, to preserve current behavior. Therefore:

- refresh the cache when the fold generation changes, even if the accepted membership event later enters store retry;
- do not refresh the cache again when a queued store retry eventually succeeds, because that does not change the fold;
- keep `store_degraded` behavior unchanged.

### D7 — Engine reads should clone the cached snapshot

Change `SyncEngine::snapshot()` from recomputing to cloning the cached value:

```rust
pub fn snapshot(&self) -> MembershipSnapshot {
    self.membership_projection.snapshot.clone()
}
```

Change other engine reads:

- `SyncEngine::digest()` should use `self.membership_projection.snapshot.clone()`.
- `SyncEngine::signer_plausible()` should consult `&self.membership_projection.snapshot` directly.
- Any new engine read path should avoid `self.fold.snapshot()` unless it is the cache-refresh helper.

After this change, production calls from `RoomReconciler::maybe_reconcile` remain source-compatible but no longer pay a fold recompute for unchanged content events.

### D8 — Do not hide fail-closed/completeness changes inside the membership cache

The cached projection covers membership state: active members, devices, roles, and the device-to-identity reverse map. It does not own the sync completeness overlay.

`AdmissionView::from_snapshot(&snapshot, &engine.fail_closed_subjects())` must continue to include fail-closed subjects from the engine. Completeness changes can occur from admin-tip processing without a membership event, so they must remain independently visible to admission reconciliation.

If a future implementation caches `AdmissionView` too, it must use a separate generation including both membership projection generation and fail-closed/completeness generation. That is not required for this issue.

### D9 — Keep blob referenced-hash invalidation separate

`file.shared` is a content event for membership projection purposes. It must not refresh the membership cache.

However, `RoomReconciler::maybe_reconcile` also updates `BlobAclView` when the set from `engine.file_shared_hashes()` changes. That behavior must remain: a `file.shared` can change blob ACL referenced hashes without changing active members/devices/roles.

The acceptance criterion only forbids membership recompute on `file.shared`; it does not forbid refreshing the blob ACL view when a new file reference is accepted.

---

## 5. Detailed implementation plan

### Step 1 — Add membership projection generation to `RoomMembership`

File: `crates/iroh-rooms-core/src/membership/fold.rs`

1. Add `membership_projection_generation: u64` to `RoomMembership`.
2. Initialize it to `0` in `RoomMembership::new`.
3. Add `pub fn membership_projection_generation(&self) -> u64`.
4. In the classification path, increment it only when a node transitions to `Verdict::Accepted` and `affects_membership(&node.event.event.content)` is true.
5. Use `saturating_add(1)` or `wrapping_add(1)` consistently. `saturating_add(1)` is preferable because this is instrumentation / invalidation state and should not wrap in a long-running process.
6. Do not change `Ingest` variants or existing public behavior unless necessary.

Implementation note: the increment belongs in the code that actually commits the final accepted verdict, not in `RoomMembership::ingest` before classification. This is what captures cascaded acceptance of previously buffered children.

### Step 2 — Add engine cache and counter

Files:

- `crates/iroh-rooms-core/src/sync/engine.rs`
- `crates/iroh-rooms-core/src/sync/config.rs` only if docs mention counters there; no config field is needed.

1. Add `membership_projection_recomputes: u64` to `SyncCounters`.
2. Add a private cache struct in `engine.rs` near other private engine helper structs:

   ```rust
   struct MembershipProjectionCache {
       snapshot: MembershipSnapshot,
       fold_generation: u64,
   }
   ```

3. Add `membership_projection: MembershipProjectionCache` to `SyncEngine`.
4. In `SyncEngine::open`, after `fold` is built, initialize the cache from one `fold.snapshot()` call and the fold generation.
5. Add `refresh_membership_projection_if_needed` as described in D5.
6. Ensure startup cache construction does not increment the runtime counter.

### Step 3 — Refresh after all fold ingest calls

File: `crates/iroh-rooms-core/src/sync/engine.rs`

1. In `deliver`, replace direct matching on `self.fold.ingest(...)` with:

   ```rust
   let ingest = self.fold.ingest(ev.clone());
   self.refresh_membership_projection_if_needed();
   match ingest { ... }
   ```

2. In `wake_park`, after each `self.fold.ingest(ev.clone())`, call the same refresh helper before matching / acting on the result.
3. Search for any other `fold.ingest` calls in `engine.rs` and add the helper after them.
4. Do not call the helper after store insert/retry paths that do not ingest into the fold.

### Step 4 — Serve engine read paths from the cache

File: `crates/iroh-rooms-core/src/sync/engine.rs`

1. Change `SyncEngine::snapshot()` to return `self.membership_projection.snapshot.clone()`.
2. Change `SyncEngine::digest()` to use the cached snapshot.
3. Change `signer_plausible` to use `&self.membership_projection.snapshot` instead of `self.fold.snapshot()`.
4. Search `engine.rs` for `fold.snapshot()` and ensure the only remaining production call is inside cache initialization / refresh. Tests may still use `RoomMembership` directly.
5. Consider adding a crate-private accessor for tests:

   ```rust
   #[cfg(test)]
   pub(crate) fn membership_projection_generation(&self) -> u64 { ... }
   ```

   This is optional if tests can use `counters().membership_projection_recomputes` and snapshot state.

### Step 5 — Keep net read paths source-compatible

Files:

- `crates/iroh-rooms-net/src/node.rs`
- `crates/iroh-rooms-net/src/manager.rs`
- `crates/iroh-rooms-net/src/admission.rs`
- `crates/iroh-rooms-net/src/blob/mod.rs`

The preferred implementation should require little or no code change in these files because `engine.snapshot()` keeps the same signature.

Verify the following paths receive cached snapshots:

1. `RoomReconciler::maybe_reconcile` calls `let snapshot = engine.snapshot();` and then uses it for:
   - active-member threshold warning;
   - `AdmissionView::from_snapshot`;
   - `BlobAclView::from_snapshot`;
   - `PeerManager::reconcile`.
2. `PeerManager::desired_devices` still accepts a `&MembershipSnapshot`; no behavior change.
3. `AdmissionView::from_snapshot` still receives fail-closed subjects separately.
4. `BlobAclView::from_snapshot` still receives referenced hashes separately.

If any path bypasses `engine.snapshot()` and calls `RoomMembership::snapshot()` directly, re-point it to the engine cache.

### Step 6 — Add regression tests for recompute behavior

Recommended location: `crates/iroh-rooms-core/src/sync/engine_tests.rs`, because it already has deterministic `SyncEngine` fixtures and access to crate-private helpers.

Add fixture helpers if needed for:

- admin genesis;
- admin-authored `message.text`;
- admin/member-authored `file.shared`;
- `member.invited`;
- `member.joined`;
- `member.removed`;
- optionally `member.left`.

Tests:

1. **Content message does not recompute**
   - Open and seed engine with genesis.
   - Drain/baseline counters after startup/genesis.
   - Publish an accepted `message.text`.
   - Assert `membership_projection_recomputes` unchanged.
   - Assert `engine.snapshot().active_member_count()` remains correct.

2. **File shared does not recompute**
   - Build a valid `file.shared` from an active member.
   - Publish it.
   - Assert `membership_projection_recomputes` unchanged.
   - Assert `engine.file_shared_hashes()` changed as expected, proving content still affects blob referenced-hash reads without touching membership projection.

3. **Member joined recomputes**
   - Publish admin invite.
   - Publish valid join.
   - Assert recompute counter increments for the invite and join, or at least increments across the membership sequence according to the exact baseline used.
   - Assert joined identity is `Active`, role is resolved, and device appears in `snapshot.active_members()`.

4. **Member removed recomputes and access changes**
   - Start with an active member.
   - Publish `member.removed` by admin.
   - Assert recompute counter increments.
   - Assert removed identity is not active.
   - Assert its device no longer appears in `snapshot.active_members()` and would be rejected by `AdmissionView::from_snapshot` / `PeerManager::desired_devices`.

5. **Content after join in same Events batch sees fresh cached projection**
   - Deliver an `Events` message containing a membership event followed by a causally valid content event from the newly active member.
   - Assert the content event is not dropped by `signer_plausible` due to a stale cache.
   - This protects the immediate-refresh requirement in D5.

6. **Buffered membership acceptance refreshes cache**
   - Deliver a `member.joined` or `member.removed` frame before one of its parents so it parks/buffers.
   - Deliver the missing parent so the fold cascades the buffered membership event to accepted.
   - Assert recompute counter increments when the buffered membership event becomes accepted.

7. **Duplicate membership event does not recompute**
   - Publish or ingest a membership event once and record the counter.
   - Re-ingest the same frame with early dedup disabled if necessary, or arrange a path that reaches `RoomMembership::ingest` with an already-known event.
   - Assert the counter does not increment a second time.

### Step 7 — Verify existing behavior

Run the smallest targeted tests while developing:

```bash
cargo test -p iroh-rooms-core membership_fold --all-features
cargo test -p iroh-rooms-core sync --all-features
cargo test -p iroh-rooms-net manager --all-features
cargo test -p iroh-rooms-net admission --all-features
```

Before final handoff, run the repository gate:

```bash
scripts/verify.sh
```

---

## 6. Acceptance criteria mapping

The implementation landed in `crates/iroh-rooms-core/src/{membership/fold.rs,
sync/engine.rs}` with regression coverage in
`crates/iroh-rooms-core/src/sync/engine_tests.rs`. Each issue acceptance is
satisfied as follows:

| Issue acceptance | Landed verification |
|---|---|
| A content-event publish (`message.text`, `file.shared`) does **not** trigger membership recompute, verified by counter/instrumentation | `SyncCounters::membership_projection_recomputes` is unchanged by a content publish; proven by `content_message_publish_does_not_refresh_membership_projection` and `file_shared_publish_updates_hashes_without_refreshing_membership_projection`. |
| `MemberJoined` / `MemberRemoved` invalidates and recomputes the projection | `RoomMembership::membership_projection_generation` bumps only on an accepted membership-affecting event; the engine's `refresh_membership_projection_if_needed` recomputes the cache; proven by `member_joined_refreshes_cached_projection_and_updates_member_state` and `member_removed_refreshes_cached_projection_and_removes_active_access`. |
| All existing membership tests pass unchanged | `RoomMembership` fold semantics and `MembershipSnapshot` shape are unchanged; only the additive generation counter + accessor were added. |
| No behavioral regression in `PeerManager::reconcile` or admission verdicts | `engine.snapshot()` keeps the same signature but clones the cached projection; net read paths still build `AdmissionView`, `BlobAclView`, and peer dial sets from a `MembershipSnapshot`. `cached_snapshot_remains_available_when_completeness_changes_without_membership` keeps the fail-closed overlay independent of the membership generation. |
| No on-disk schema change | The cache is in-memory only and is not persisted. |
| Cache rebuilt from store on startup | Built once in `SyncEngine::open` from `RoomMembership::from_events`; the runtime counter starts at zero and the startup build is not counted — proven by `reopened_engine_uses_cached_projection_from_store_without_runtime_recompute`. |

Additional landed regression coverage (beyond the issue's explicit acceptance
list): `content_after_join_in_same_events_message_uses_refreshed_projection`
(immediate-refresh requirement, D5),
`buffered_membership_acceptance_refreshes_cached_projection` (cascaded buffered
acceptance),
`duplicate_membership_event_does_not_refresh_cached_projection`, and
`store_retry_success_does_not_refresh_membership_projection_again` (the cache
follows the fold, not the store — D6).

---

## 7. Observability and instrumentation

Add `SyncCounters::membership_projection_recomputes`.

Semantics:

- Counts runtime cache refreshes after `SyncEngine::open` initialization.
- Increments exactly once per engine cache refresh caused by a fold membership generation change.
- Does not increment for content events that leave the fold membership generation unchanged.
- Does not increment when store retry succeeds without a new fold ingest.

Optional additional test-only accessors:

- `SyncEngine::membership_projection_generation()` returning cached generation.
- `RoomMembership::membership_projection_generation()` is public and can be asserted directly in membership unit tests.

Do not log per-refresh lines by default; a counter is enough and avoids noisy content-heavy rooms. Existing logs for rejection, store retry, admin-tip trust, and advisory flags remain unchanged.

---

## 8. Security, privacy, reliability, and performance considerations

### Security / authorization

- The cache must never authorize anything not authorized by `RoomMembership`. It is a memoized `MembershipSnapshot`, not a parallel policy engine.
- Admission must still default-deny unknown devices and inactive identities.
- The fail-closed subject overlay must remain separate from the membership cache so admin-tip suspicion still affects admission without requiring a membership event.
- A removed or left member must stop appearing as active immediately after the fold accepts the membership event and the cache refreshes.

### Privacy

- No new persisted data is introduced.
- No invite secrets, identity secrets, or device secrets are logged or cached beyond existing public-key membership state.
- Blob ACL referenced hashes continue to be derived from validated `file.shared` events; this feature does not broaden blob visibility.

### Reliability

- Startup rebuild remains deterministic: persisted accepted events rebuild `RoomMembership`, and one initial snapshot rebuilds the cache.
- Store retry / store degradation semantics are preserved. The cache follows the fold's accepted set, consistent with current session behavior.
- Immediate refresh after fold generation changes avoids stale-cache decisions within a multi-frame `Events` loop.

### Performance

- Content-heavy traffic no longer pays `RoomMembership::snapshot()` work on every `engine.snapshot()` / reconciler pass.
- Membership events still recompute the full membership snapshot from the fold. This is acceptable at the MVP room size and preserves deterministic correctness.
- Memory overhead is one `MembershipSnapshot` clone plus a generation integer.

---

## 9. Rollout and rollback plan

### Rollout

1. Land generation + cache + instrumentation behind no protocol flag; it is an internal performance change.
2. Run targeted tests and the full `scripts/verify.sh` gate.
3. Watch `membership_projection_recomputes` in tests / diagnostics to confirm content events do not increase the counter.

### Rollback

Because there is no on-disk schema or wire change, rollback is a code revert:

- remove the engine cache;
- restore `SyncEngine::snapshot()` and `digest()` to call `self.fold.snapshot()`;
- remove the counter and tests that assert cache behavior.

Existing stored rooms remain compatible.

---

## 10. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Cache becomes stale after a cascaded buffered membership event accepts | Admission / peer dial / ACL views may remain stale | Use `RoomMembership::membership_projection_generation()` and compare after every ingest, not just current event type. Add buffered-membership test. |
| Cache refresh delayed until end of `on_message(Events)` loop | Later frames in the same batch may be judged against stale signer/membership state | Refresh immediately after generation changes. Add same-batch join-then-content test. |
| `member.left` omitted from invalidation set | Voluntarily left members remain active in cached projection | Include `member.left` because current fold treats it as membership-affecting. |
| Runtime counter ambiguity with startup rebuild | Acceptance test may falsely read startup as content-triggered recompute | Counter starts after `open` cache initialization or tests baseline before content publish. |
| Fail-closed overlay accidentally coupled to membership generation | Admin-tip suspicion changes may not update admission | Keep fail-closed separate; `AdmissionView::from_snapshot` still receives `engine.fail_closed_subjects()`. |
| Duplicate membership replays spuriously recompute | Counter noise and avoidable work | Increment generation only on final transition from `Pending` to `Accepted`, not on duplicate `ingest` outcome. |

---

## 11. Assumptions

1. `RoomMembership::snapshot()` remains deterministic and is the only correct way to build a `MembershipSnapshot` from the fold.
2. The engine is the right cache owner because it already owns the live fold and is rebuilt from the store on startup.
3. The production long-running read paths named in the issue (`PeerManager::desired_devices`, admission, ACL views) consume snapshots through `SyncEngine` / `RoomReconciler` and do not need signature changes.
4. `member.left` is membership-affecting even though it is omitted from the issue's invalidation examples.
5. Current device binding changes are represented by membership events (`room.created`, `member.joined`, and removal/current status changes), not by a standalone event type. If a standalone binding event exists later, it must increment the same generation.
6. One full snapshot recompute per accepted membership transition is acceptable for the MVP ≤5-member ceiling; the requested optimization is to avoid recomputes on content-only activity.

---

## 12. Open questions

1. Should `membership_projection_recomputes` count the initial startup cache build? This spec recommends **no** for acceptance-test clarity, but a separate startup counter could be added if operators need it.
2. Should `MembershipProjectionCache` also cache derived `AdmissionView` or `BlobAclView`? This spec recommends **no** for the first implementation because fail-closed subjects and referenced blob hashes have independent invalidation sources.
3. Should the fold generation be public API on the SDK facade (`crates/iroh-rooms`) or remain core-only? This spec only requires it in `iroh-rooms-core`.
4. Should `RoomMembership::affects_membership` be exposed as `pub(crate)` / public for tests and future event classification, or should generation updates keep it private? Either is acceptable; avoid duplicating event-type lists across modules.
