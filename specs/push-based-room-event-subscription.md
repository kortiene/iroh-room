# Push-based subscription for newly ingested room events (`Node::room_events`)

- **Issue:** #83 — `feat(net): push-based subscription for newly ingested room events on Node`
- **Proposed work item:** IR-0307
- **Labels:** `enhancement`, `type/feature`
- **Status:** Landed — implemented per this build plan (issue #83 / IR-0307); see `README.md`, `crates/iroh-rooms/CHANGELOG.md`, and `docs/getting-started.md` for the shipped user-facing docs.
- **Owning crates:** `iroh-rooms-core` (engine drain), `iroh-rooms-net` (broadcast + pump), `iroh-rooms` (façade re-export)
- **Filed by:** a real SDK consumer — **Bantaba**, a resident daemon + web UI on the developer-preview façade (`--features experimental`).

---

## 1. Problem statement

`Node` exposes a push channel for **connection** events only:
`conn_events() -> broadcast::Receiver<ConnEvent>` (`crates/iroh-rooms-net/src/node.rs:482`).
For **room events**, every read is pull — `room_tail`, `wait_until_contains`,
`snapshot`, `store_contains`. `SyncEngine` is likewise pull-only. There is **no
way to be told "a new validated event was just ingested."**

A consumer that needs push semantics must poll. Bantaba polls `room_tail` every
~300 ms per open room and dedupes against a seen-set. Two structural costs:

1. **Latency floor** = the poll interval, for every message/status/file event in
   every open room.
2. **Full-tail rescan for correctness.** A late-arriving low-lamport event can
   land *outside* any bounded tail window, so a bounded poll (`room_tail(512)`)
   silently drops it from the push stream. Exactly-once forces a
   `room_tail(u32::MAX)` scan each cycle — O(size) per poll, forever growing.

### Goal

Add a single push API on `Node`:

```rust
/// Every event accepted into the store (local publish and remote ingest),
/// emitted exactly once, after validation + insert.
pub fn room_events(&self) -> broadcast::Receiver<StoredEvent>;
```

with documented lossy-on-lag semantics and a reconcile recipe, surfaced through
the façade at `iroh_rooms::experimental::session`.

### Non-goals

- **CLI `room tail --follow`.** Deferred. The CLI installs no tracing subscriber
  (see the `cli-has-no-tracing-subscriber` constraint); a `--follow` renderer is
  a separate presentation concern and is explicitly out of scope here. This spec
  only lands the SDK primitive that would make it trivial later.
- **CLI offline authoring paths.** `room/message/invite/file.rs` insert directly
  through `EventStore::insert`, bypassing `SyncEngine` entirely, and never build
  a `Node`. Those authoring commands are out of scope and do **not** emit on this
  channel (they have no live subscriber to serve).
- **Ordered/replay delivery, gap-fill, or a durable event log.** The channel is a
  lossy live tap (tokio `broadcast`), identical in contract to `conn_events`.
  Catch-up after a gap is the consumer's job via the documented reconcile recipe.
- **Filtering by room/type/author.** One undifferentiated stream of `StoredEvent`;
  a `Node` is already single-room, and consumers filter on `event_type` cheaply.

---

## 2. Background — why the design is constrained

Two load-bearing facts about this codebase shape the only clean design:

### 2.1 There is exactly one insert choke point

Every in-engine store write funnels through one arm:
`SyncEngine::store_and_fanout` → `InsertOutcome::Inserted`
(`crates/iroh-rooms-core/src/sync/engine.rs:702`, insert at `:709`, the
`Inserted` arm at `:722`). That single function is reached from **all three**
ingest paths:

- **Own publish** — `engine.publish()` → `deliver()` → `store_and_fanout()`.
- **Peer sync** — `engine.on_message()` (a `SyncMessage::Events` frame) →
  `deliver()` → `store_and_fanout()`.
- **Delayed park-promotion** — `wake_park()` (`engine.rs:754`, calls
  `store_and_fanout` at `:765`), which fires when a previously-orphaned frame's
  missing parent finally arrives. `wake_park` is driven from within
  `on_message`/`on_tick`/`publish`, so it needs no separate hook.

Because insert is idempotent, the sibling `InsertOutcome::Duplicate` arm
(`engine.rs:717`) gives **exactly-once for free**: a re-seen event takes the
`Duplicate` branch and never reaches the emit point. This is the crux of AC-1.

### 2.2 The core crate is sans-IO (no tokio)

`iroh-rooms-core` has no tokio dependency (`crates/iroh-rooms-core/Cargo.toml`).
A `tokio::sync::broadcast::Sender` therefore **cannot** live inside `SyncEngine`.
The established pattern for surfacing per-ingest signals out of the sans-IO
engine is `take_flags()` (`engine.rs:629`): the engine accumulates a
`pending_flags: Vec<&'static str>` (`engine.rs:250`) at the insert choke point,
and the tokio-aware net pump drains it after each engine drive
(`node.rs:981`). This spec mirrors that pattern exactly with a
`pending_ingested: Vec<StoredEvent>` accumulator + `take_ingested()` drain, and
the net-crate pump owns the `broadcast::Sender<StoredEvent>` — precisely how
`PeerTable` owns the `ConnEvent` sender (`transport.rs:257`).

---

## 3. Design overview

```
                    iroh-rooms-core (sans-IO)          iroh-rooms-net (tokio)
                    ─────────────────────────          ──────────────────────
 own publish ─┐
 peer sync   ─┼─► store_and_fanout()                    pump() drains after each
 park wake   ─┘     └─ InsertOutcome::Inserted:          engine drive:
                        store.get(id)? ─► push to           let evs = engine.take_ingested();
                        pending_ingested                    for ev in evs { let _ = tx.send(ev); }

                    take_ingested() ── drains ──►      broadcast::Sender<StoredEvent>
                    (std::mem::take)                    (cap = NetConfig.room_event_capacity)
                                                              │ subscribe()
                                                              ▼
                                              Node::room_events() -> broadcast::Receiver<StoredEvent>
                                                              │ (façade re-export)
                                                              ▼
                                        iroh_rooms::experimental::session::Node::room_events()
```

- Engine accumulates each freshly-**Inserted** `StoredEvent` in a
  `pending_ingested` vec at the single choke point.
- The pump calls `take_ingested()` after every engine drive that can insert
  (`publish`, `on_message`, `on_tick`) and forwards each into a
  `broadcast::Sender<StoredEvent>`.
- `Node` holds a clone of the `Sender`; `room_events()` returns
  `sender.subscribe()`.
- Façade re-exports `Node` (already done); the new method rides along.

**Where the `broadcast` sender lives — decision:** in the **net pump / `Node`**,
not in `Shared`/`PeerTable` and not in the engine.
- *Not the engine* — sans-IO, no tokio (§2.2).
- *Not `PeerTable`* — that table is peer-scoped; room events are engine/room
  scoped, and the drain happens in `node.rs` where the engine is driven. Putting
  it in `Shared` would force threading `room_event_capacity` through
  `NetTransport::bind` for no benefit. Keep the sender local to `spawn_inner`,
  clone into the pump, and store a clone on `Node`. (Considered and rejected:
  symmetry with `conn_events`' `PeerTable` home; rejected because room events are
  not a transport concern.)

---

## 4. Detailed implementation steps

### Step 1 — Engine: accumulate ingested events (`iroh-rooms-core`)

File: `crates/iroh-rooms-core/src/sync/engine.rs`

1. Add a field beside `pending_flags` (near `engine.rs:250`):
   ```rust
   /// Events accepted (InsertOutcome::Inserted) since the last take_ingested
   /// drain — the push-subscription feed (issue #83 / IR-0307). Mirrors
   /// `pending_flags`: the sans-IO engine cannot own a tokio broadcast sender,
   /// so it buffers and the net pump drains + fans out.
   pending_ingested: Vec<StoredEvent>,
   ```
2. Initialize it `Vec::new()` in the constructor (beside `pending_flags` at
   `engine.rs:299`).
3. In `store_and_fanout`, inside the `InsertOutcome::Inserted` arm
   (`engine.rs:722`), after `note_admin_event(id)` (so `lamport`/`admin_seq` are
   computed), materialize the stored projection and push it:
   ```rust
   // Push-subscription feed (issue #83): emit exactly once, only on a real
   // insert (the Duplicate arm never reaches here → exactly-once for free).
   match self.store.get(&id) {
       Ok(Some(stored)) => self.pending_ingested.push(stored),
       Ok(None) => self.log("room_events: inserted event vanished from store"),
       Err(e) => self.log(&format!("room_events: store.get failed: {e}")),
   }
   ```
   Rationale for `store.get`: `EventStore::insert` returns only `InsertOutcome`
   (`store/mod.rs:189`), and `StoredEvent.lamport`/`admin_seq` are **derived by
   the store on insert** — re-reading is the correct, simplest way to get the
   canonical projection. See Risk R1 for the perf note and OQ-3 for the
   avoid-the-read alternative.
4. Add the drain accessor beside `take_flags` (`engine.rs:629`):
   ```rust
   /// Drain the events accepted since the last call — the push-subscription
   /// feed for `Node::room_events` (issue #83 / IR-0307). Each freshly-Inserted
   /// event appears exactly once across own-publish, peer-sync, and delayed
   /// park-promotion; a duplicate re-see never lands here. Callers get exactly
   /// the events accepted since they last drained (destructive `mem::take`).
   ///
   /// NOTE: within a single drive, park-promotion appends in engine-iteration
   /// order, NOT causal/Lamport order (see §6.2). Set-membership + exactly-once
   /// hold; strict ordering does not.
   pub fn take_ingested(&mut self) -> Vec<StoredEvent> {
       std::mem::take(&mut self.pending_ingested)
   }
   ```

> **Import note:** `StoredEvent` is already imported into `engine.rs` (`:30`), so
> no new `use` is needed.

### Step 2 — NetConfig: add the channel capacity (`iroh-rooms-net`)

File: `crates/iroh-rooms-net/src/transport.rs`

1. Add a field to `NetConfig` (`transport.rs:51`), mirroring
   `conn_event_capacity` (`:55`):
   ```rust
   /// Ring capacity of the `Node::room_events` broadcast (issue #83). Lossy on
   /// lag exactly like `conn_event_capacity`; a slow subscriber gets `Lagged`.
   pub room_event_capacity: usize,
   ```
2. Default it to `256` in `impl Default for NetConfig` (`transport.rs:62`),
   matching `conn_event_capacity`.

> **Source-compat note (R6):** `NetConfig` is **not** `#[non_exhaustive]`, so
> adding a field is technically breaking for an external caller that constructs
> it with an exhaustive struct literal. This is consistent with how
> `conn_event_capacity` shipped and acceptable pre-1.0 / developer-preview.
> Optionally add `#[non_exhaustive]` in the same PR (see OQ-4) — call it out in
> the PR body either way.

### Step 3 — Pump: own the sender, drain + fan out (`iroh-rooms-net`)

File: `crates/iroh-rooms-net/src/node.rs`

1. **Create the channel before `cfg` is moved.** `cfg` is consumed by
   `NetTransport::bind` at `node.rs:361`, so read the capacity first. In
   `spawn_inner`, before the `NetTransport::bind` call (before `node.rs:357`):
   ```rust
   let (room_event_tx, _) = broadcast::channel::<StoredEvent>(cfg.room_event_capacity);
   ```
   (`broadcast` is already imported for `conn_events`; `StoredEvent` is already
   in scope in `node.rs` — see `Cmd::Tail` at `:73`.)
2. **Pass a clone into `pump`.** Add a `room_event_tx: broadcast::Sender<StoredEvent>`
   parameter to the `pump` fn signature (`node.rs:906`) and to the `tokio::spawn(pump(...))`
   call (`node.rs:400`). (`pump` already carries `#[allow(clippy::too_many_arguments)]`.)
3. **Store the sender on `Node`.** Add a `room_event_tx: broadcast::Sender<StoredEvent>`
   field to `struct Node` (`node.rs:171`) and set it in the returned `Self { .. }`
   (`node.rs:421`).
4. **Add a private drain helper** (near the other pump free-fns, e.g. beside
   `handle_conn_event`):
   ```rust
   /// Fan out every event the engine accepted since the last drain onto the
   /// `room_events` broadcast (issue #83 / IR-0307). A `send` error means no
   /// live subscriber — expected and ignored; a lagging subscriber is dropped
   /// frames on the receiver, not an error here.
   fn drain_room_events(engine: &mut SyncEngine, tx: &broadcast::Sender<StoredEvent>) {
       for ev in engine.take_ingested() {
           let _ = tx.send(ev);
       }
   }
   ```
5. **Call the drain after every engine drive that can insert** — the three
   accept paths of §2.1:
   - After `engine.on_message(...)` in the inbound arm — right after the existing
     `engine.take_flags()` drain (`node.rs:981`). *(peer sync + any park-promotion
     that message triggered)*
   - After `engine.on_tick(now_ms())` in the ticker arm (`node.rs:1015`).
     *(anti-entropy pull responses land as `on_message`, but `on_tick` also runs
     `wake_park`, so park-promotions surface here)*
   - After `engine.publish(&bytes)` in `handle_cmd`'s `Cmd::Publish` arm
     (`node.rs:1036`). *(own publish)*

   `on_connect`/`on_disconnect` (`handle_conn_event`, `node.rs:1099`) never
   insert, so they need no drain. Calling the helper there anyway would be a
   harmless no-op (`take_ingested` returns empty); **do not** add it — keep the
   drain co-located with the three real accept sites and comment why the conn arm
   is exempt.

   > **Note on the provisional-peer branch:** when an inbound message is dropped
   > for a provisional peer (`node.rs:956`), `engine.on_message` is **not**
   > called, so there is nothing to drain — correct, no change needed.

6. **Add `Node::room_events`** beside `conn_events` (`node.rs:480`):
   ```rust
   /// Subscribe to the live stream of events accepted into this room's store —
   /// every event validated + inserted via local publish OR remote sync, emitted
   /// exactly once after insert (issue #83 / IR-0307).
   ///
   /// # Semantics
   /// - **Exactly once per stored event.** A duplicate re-see (same `event_id`)
   ///   is idempotent and never re-emitted.
   /// - **Lossy on lag.** This is a bounded `broadcast` (capacity
   ///   `NetConfig::room_event_capacity`, default 256). A subscriber that falls
   ///   behind receives `RecvError::Lagged(n)` and MUST resync — the events it
   ///   missed are gone from this channel.
   /// - **Not ordered by Lamport.** Emission order follows insertion order at the
   ///   engine choke point. A park-promotion cascade emits the directly-accepted
   ///   trigger first, then its promoted descendants in engine-iteration order —
   ///   NOT causal order. Use `StoredEvent.lamport` if you need a total order.
   ///
   /// # Reconcile recipe (on `Lagged`)
   /// ```ignore
   /// let mut rx = node.room_events();
   /// let mut seen = HashSet::new();
   /// loop {
   ///     match rx.recv().await {
   ///         Ok(ev) => { if seen.insert(ev.event_id) { handle(ev); } }
   ///         Err(RecvError::Lagged(_)) => {
   ///             // Rebuild from the authoritative tail, dedupe against `seen`.
   ///             for ev in node.room_tail(u32::MAX).await? {
   ///                 if seen.insert(ev.event_id) { handle(ev); }
   ///             }
   ///         }
   ///         Err(RecvError::Closed) => break,
   ///     }
   /// }
   /// ```
   #[must_use]
   pub fn room_events(&self) -> broadcast::Receiver<StoredEvent> {
       self.room_event_tx.subscribe()
   }
   ```

### Step 4 — Façade: surface it (`iroh-rooms`)

File: `crates/iroh-rooms/src/experimental/session.rs`

- `Node` is already re-exported (`session.rs:10`) and `StoredEvent` via
  `experimental/store.rs:6`, so **no new re-export is required** — the method
  rides on the already-exported `Node`. Update the `session.rs` module doc method
  list (`session.rs:6-7`) to mention `room_events` alongside `conn_events`.
- Confirm `broadcast::Receiver` is reachable to façade consumers. The type is
  re-exported implicitly through the method signature; if consumers need to name
  it, document that they get it via `tokio::sync::broadcast` (add a one-line
  pointer in the module doc). Do **not** re-export tokio from the façade.

### Step 5 — Docs

- `README.md` "Current Status" (`README.md:36`): add a changelog entry following
  the established per-issue pattern (e.g. the issue #88 / #85 entries at
  `README.md:986`/`:1015`): *"Push-based room-event subscription (issue #83 /
  IR-0307): `Node::room_events()` streams each newly-ingested `StoredEvent`
  exactly once; consumers stop polling `room_tail`."*
- `docs/getting-started.md` / `docs/sdk-coverage.md`: add a "Subscribe to room
  events" subsection under "Using it as a library" with the reconcile recipe.
- `RELEASE-READINESS.md`: if it enumerates real-QUIC-loopback façade tests, bump
  the count to include the new façade e2e (Step 6.4). No test asserts the literal
  count, so this is prose-only.

---

## 5. API / data-model impact

| Surface | Change | Breaking? |
|---|---|---|
| `SyncEngine::take_ingested(&mut self) -> Vec<StoredEvent>` | **new** | additive |
| `SyncEngine.pending_ingested` (private field) | **new** | no (private) |
| `NetConfig.room_event_capacity: usize` | **new field**, default 256 | technically source-breaking (R6); consider `#[non_exhaustive]` |
| `Node::room_events(&self) -> broadcast::Receiver<StoredEvent>` | **new** | additive |
| `Node` struct + `pump` fn signature | **new field / param** (private) | no |
| `iroh_rooms::experimental::session::Node` | new method rides existing re-export | additive |

- **No new event type, no store-schema change, no wire-format change.**
  `StoredEvent` is reused verbatim (`store/model.rs:56`).
- **No validation/authorization change.** The channel emits strictly what the
  engine already accepted; every membership/capability/anti-amplification gate
  (`deliver`'s §6.2 pre-gate at `engine.rs:677`, the fold's `ingest`) runs
  unchanged upstream. Rejected/parked frames never reach the emit point.

---

## 6. Semantics, correctness & observability

### 6.1 Exactly-once (AC-1)

The emit sits *inside* the `InsertOutcome::Inserted` arm only. The `Duplicate`
arm (`engine.rs:717`) is the sole other outcome and does nothing. Since all three
ingest paths (own publish, peer sync, park-promotion) route through this one
function, and insert is idempotent by `event_id`, each stored event is emitted
exactly once regardless of how many times it arrives or from how many peers.

### 6.2 Ordering — a documented weakness, not a bug

**Emission order is insertion order, which is NOT causal/Lamport order within a
park-promotion cascade.** When a connecting parent finally lands, the membership
fold internally promotes all its buffered descendants inside one
`fold.ingest()`; `wake_park` (`engine.rs:754`) then re-ingests the parked frames
in engine-iteration (HashMap) order, and each already reads `Accepted`, so
`store_and_fanout` records them in that order — not parent-before-child. Only
**exactly-once + set-membership** hold across a cascade: the directly-accepted
*trigger* is recorded first, its promoted descendants follow unordered.

This is why the doc comment (Step 3.6) states "not ordered by Lamport" and points
consumers at `StoredEvent.lamport` for a total order. It is also why the e2e test
(Step 6.3) asserts **set + trigger-first**, never an exact `vec![A,B,C]` sequence
— an exact causal-sequence assertion would flake.

### 6.3 Lossy-on-lag (AC-2)

The `broadcast` channel drops the oldest buffered items for a slow subscriber and
surfaces `RecvError::Lagged(n)` — never silent loss. The reconcile recipe (Step
3.6) resyncs from `room_tail(u32::MAX)` deduped against a seen-set. This matches
the `conn_events` contract byte-for-byte; the pump's own conn-event consumer
already models `Lagged` handling (`node.rs:1008`).

### 6.4 Observability

- No new counters required; ingest is already counted
  (`counters.accepted`/`duplicates`). Optionally add a `tracing::trace!` in
  `drain_room_events` for the emitted count (off any hot decision path).
- The channel itself is the observability improvement: consumers get push
  visibility they previously had to poll for.

---

## 7. Test strategy

Mirror how `conn_events`/`take_flags` are tested — most coverage is cheap and
sans-network; only the two-node cross-the-wire proof is e2e-tier.

### 7.1 Core unit tests — `crates/iroh-rooms-core/tests/sync_smoke.rs`

Reuse the existing `build_log`/`fresh_engine`/`eid` harness. **Genesis must be
pre-published** so an admin-authored frame parked before its parent survives the
§6.2 signer pre-gate.

1. `take_ingested_emits_on_own_publish` — publish genesis + a chain; assert each
   accepted event appears once in `take_ingested()`, in publish order.
2. `take_ingested_skips_duplicates` — re-deliver an already-stored frame; assert
   `take_ingested()` is empty on the re-see (exactly-once).
3. `take_ingested_is_destructive` — two consecutive drains: the second returns
   only events accepted since the first (`mem::take` semantics).
4. `take_ingested_emits_on_park_promotion` — deliver a child before its parent
   (buffered/parked), then deliver the parent; assert the drain now contains
   **both** as a *set*, with the directly-accepted trigger first. **Do NOT**
   assert an exact causal `vec` (see §6.2).

### 7.2 Net-pump tests — `#[cfg(test)] mod` in `crates/iroh-rooms-net/src/node.rs`

Test the private `drain_room_events` helper directly — **no endpoint/QUIC/tokio
runtime needed** (`broadcast::channel` + `try_recv` are sync). Build a real
`SyncEngine` over `open_in_memory`, publish genesis + a chain of admin
`MemberInvited` (chained parents keep `admin_seq` monotonic → no fork; each is
Accepted → drains), feed a bare `broadcast::channel::<StoredEvent>`:

1. `drain_forwards_in_order_and_is_destructive` — drive + drain forwards each
   accepted event once; a second drain is empty.
2. `drain_no_subscriber_does_not_panic` — drop the receiver; `tx.send` erroring
   is ignored (R7).
3. `drain_lagged_then_recovers` — capacity 2, send 4; the receiver observes
   exactly `Lagged(2)` then the two most-recent events. (tokio `broadcast` does
   **not** round capacity → the count is exact.)

### 7.3 Net e2e — `crates/iroh-rooms-net/tests/` (`#[ignore]` online tier)

`room_events_two_node_out_of_order` — the AC's headline test. Two real loopback
QUIC nodes; **induce out-of-order delivery** (e.g. hold back a low-lamport parent
so the child parks on the receiver, then release the parent). Assert on the
receiver's `room_events` stream:
- every event appears **exactly once** (dedupe check over `event_id`),
- the set matches `room_tail(u32::MAX)`,
- ordering asserted as **set + trigger-first**, never an exact causal sequence.

### 7.4 Façade e2e — `crates/iroh-rooms/tests/facade_e2e.rs`

`room_events_delivers_published_message_through_the_facade` — using **façade-only
imports** (`iroh_rooms::experimental::session::Node`), spawn two nodes over real
loopback QUIC, publish a `message.text` on one, assert it arrives on the other's
`room_events()` receiver. Proves the online tier works through the public façade.

### 7.5 Surface tripwire — `crates/iroh-rooms/tests/experimental_surface.rs`

A compile-only fn-pointer signature assertion locking
`Node::room_events`'s type, e.g.:
```rust
let _: fn(&session::Node) -> tokio::sync::broadcast::Receiver<store::StoredEvent> =
    session::Node::room_events;
```

> **Test-wiring gotcha (verified):** the façade dev-dependency `tokio`
> (`crates/iroh-rooms/Cargo.toml:42`) currently enables only
> `["rt-multi-thread", "macros"]`. Naming `tokio::sync::broadcast::Receiver` in a
> test requires **adding the `"sync"` feature** to that dev-dep, or the surface
> test won't compile.

### 7.6 Gate

Run the full `verify.sh` gate (see the `verify-sh-is-the-real-ci-gate`
constraint): `cargo fmt --check`, `clippy -D warnings` (pedantic), all-features
tests, `-p iroh-rooms --doc`, examples build. The `#[ignore]` online e2e tiers
(7.3/7.4) run under the P0 online gate, not the default `verify.sh`.

---

## 8. Acceptance criteria

- **AC-1 (exactly-once, both paths).** Exactly one emission per stored event
  across own-publish and peer-sync, proven by the two-node e2e (7.3) with induced
  out-of-order delivery. Duplicates never re-emit.
- **AC-2 (lossy-not-silent).** A lagging subscriber receives `RecvError::Lagged`,
  not silent loss (7.2.3), and can resync via the documented recipe.
- **AC-3 (façade path).** `room_events` is reachable and functional via
  `iroh_rooms::experimental::session` over real loopback QUIC (7.4).
- **AC-4 (docs).** The lossy-on-lag contract + reconcile recipe are documented on
  the method and in `docs/`; ordering caveat (§6.2) stated.
- **AC-5 (no regression).** `verify.sh` green; existing `conn_events`,
  `take_flags`, publish/sync behavior unchanged.
- **AC-6 (polling retired, informational).** A daemon consumer (Bantaba) can drop
  its `room_tail` poll pump + seen-set entirely (the seen-set survives only as the
  `Lagged` reconcile fallback).

---

## 9. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| **R1** | Extra `store.get(&id)` per insert (§4 Step 1.3) adds one point-read to the hot ingest path. | The read is a single indexed `event_id` lookup on an already-open connection; ingest already does far more work (validate, fold, fanout). Accept for correctness; OQ-3 is the optimization if profiling ever flags it. |
| **R2** | Ordering surprises consumers expecting causal order (§6.2). | Documented explicitly on the method + spec; tests assert set/trigger-first, not sequence. Consumers sort by `StoredEvent.lamport` if needed. |
| **R3** | Unbounded memory if `pending_ingested` grows between drains. | The pump drains after **every** engine drive, so the buffer holds at most one drive's accepted events. It is not a durable log. |
| **R4** | A misbehaving/slow subscriber silently misses events. | By-design lossy `broadcast`; `Lagged` makes it observable; reconcile recipe restores exactly-once. Identical to `conn_events`. |
| **R5** | Emitting inside the engine choke point could reorder relative to fanout. | The push happens *after* `store.insert` and admin-note, in the same arm as fanout; no observable reordering of network fanout. It is buffered, not sent, inside the engine (sans-IO), so no await/reentrancy. |
| **R6** | `NetConfig` new field is source-breaking for an exhaustive external constructor. | Consistent with `conn_event_capacity`'s shipment; pre-1.0 dev-preview. Consider `#[non_exhaustive]` (OQ-4); call out in PR body. |
| **R7** | `tx.send` with no subscriber returns `Err` and could be mistaken for failure. | `broadcast::Sender::send` erroring only means "no live receiver" — explicitly ignored (`let _ =`), covered by test 7.2.2. |
| **R8** | CLI authoring paths bypass the engine and won't emit, surprising someone wiring `room tail --follow`. | Documented as a non-goal (§1); those commands never build a `Node`. `--follow` is deferred until the SDK primitive is consumed. |

---

## 10. Security / privacy / reliability / performance

- **Security/privacy:** No new trust surface. The channel emits only
  already-validated, already-authorized events (all membership/capability/
  anti-amplification gates run upstream in `deliver`/the fold). No event that
  fails validation, is parked, or is rejected ever reaches a subscriber. No new
  network exposure — this is an in-process observer.
- **Reliability:** Bounded ring buffer; back-pressure resolves as `Lagged`, never
  as unbounded growth or a stalled pump (the pump never awaits the broadcast
  send). A dead subscriber cannot wedge ingest.
- **Performance:** One extra indexed point-read per accepted insert (R1); a
  `Vec::push` + `mem::take` + N `broadcast::send` per drive. All off the network
  hot path and dwarfed by validation/fold cost. Removes the consumer's
  O(size)-per-poll `room_tail(u32::MAX)` scan — a net system-wide win.
- **Migration/rollback:** Purely additive API; no schema, wire, or event-format
  change. Rollback = revert; no data migration. A consumer can adopt
  incrementally (keep the poll pump as the `Lagged` fallback, then delete it).

---

## 11. Key decisions

1. **Mirror `take_flags`, not a new engine-owned channel.** The sans-IO core
   can't hold a tokio sender; the accumulate-in-core / drain-in-pump split is the
   established, proven pattern.
2. **Emit at the single insert choke point** (`store_and_fanout` `Inserted`
   arm) — captures all three ingest paths in one place and gets exactly-once free
   from the `Duplicate` arm.
3. **Broadcast sender lives on `Node`/pump, not `Shared`/`PeerTable` or the
   engine.** Room events are engine-scoped and drained where the engine is
   driven; no benefit to threading capacity through `NetTransport::bind`.
4. **Reuse `StoredEvent`** — no new type; consumers already know it from
   `room_tail`/`snapshot`.
5. **Ship the SDK primitive only; defer `room tail --follow`.** The CLI's
   no-tracing-subscriber constraint makes a follow renderer a separate concern.
6. **Document ordering honestly** — set + exactly-once are guaranteed; causal
   order is not. Tests assert accordingly.

---

## 12. Assumptions

- `EventStore::get` returns the fully-derived `StoredEvent` (with `lamport`/
  `admin_seq`) immediately after `insert` within the same engine — confirmed by
  `store/mod.rs:231` and the insert-then-index model.
- Default capacity `256` is adequate for the target daemon workload (matches
  `conn_event_capacity`); it's a `NetConfig` knob if not.
- The three drive sites (`publish`/`on_message`/`on_tick`) are the complete set
  of engine methods that can insert — verified: only `deliver` and `wake_park`
  reach `store_and_fanout`, and both are reachable only from those three.
- Developer-preview / pre-1.0 stability: an additive `NetConfig` field is
  acceptable (see R6).

---

## 13. Open questions

- **OQ-1:** Should `room_events` carry the `InsertOutcome`/a per-event source tag
  (own vs peer vs park-promotion)? *Proposed: no* — `StoredEvent` alone; add a
  richer envelope only if a consumer needs provenance.
- **OQ-2:** Do we want a `room_events_capacity()`/config accessor on `Node`, or is
  the `NetConfig` knob enough? *Proposed: NetConfig only.*
- **OQ-3:** Avoid the extra `store.get` by having `EventStore::insert` return the
  `StoredEvent` (or by constructing it from the `ValidatedEvent` + freshly-computed
  `lamport`)? *Proposed: defer* — keep `insert`'s signature stable; revisit only
  if R1 profiles hot.
- **OQ-4:** Add `#[non_exhaustive]` to `NetConfig` in this PR to prevent future
  field-additions from breaking? *Proposed: yes, if it doesn't ripple through
  existing constructors* — otherwise track separately.
- **OQ-5:** Land a minimal `room tail --follow` on top in the same PR, or strictly
  defer? *Proposed: defer* (non-goal §1); revisit once a consumer drives it.
