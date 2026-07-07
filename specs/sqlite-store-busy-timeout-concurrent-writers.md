# Spec: SQLite `busy_timeout` + `IMMEDIATE` write transactions for concurrent `EventStore` writers

**Issue:** #85 — `feat(store): set a SQLite busy_timeout (or expose a hook) for concurrent writers on a shared EventStore DB`
**Labels:** `enhancement`, `type/feature`
**Owning crate/module:** `iroh-rooms-core` → `src/store/` (`mod.rs`, `schema.rs`); re-exported through the façade `iroh-rooms` → `experimental::store`.
**Status:** planning / ready-to-implement. Do **not** implement from this document alone — it is the executable plan.

---

## 1. Summary

A real developer-preview consumer (**Bantaba**, a resident daemon on rev `1d2f014`) opens **two `EventStore` connections onto the same database file** — one for RPC-driven writes, one for a room session's sync pump. Under WAL, exactly one writer is allowed at a time; a colliding commit from the second connection surfaces to the caller as `SQLITE_BUSY` (`StoreError::Sqlite`) instead of briefly waiting. The consumer wrapped *their own* writes in a busy-retry loop, but the SDK's own write path — the sync engine's ingest (`EventStore::insert`, driven at `crates/iroh-rooms-core/src/sync/engine.rs:709`) — cannot be protected from outside. The fix has to live where the connection is configured.

This spec delivers three changes, in order of importance:

1. **`BEGIN IMMEDIATE` for every write transaction** (the load-bearing fix). Today all write methods use `Connection::transaction()`, which is `BEGIN DEFERRED`. A deferred transaction that reads before it writes (notably `append_trust_decision`, `store/mod.rs:726–731`) takes a read lock first, then tries to *upgrade* to a write lock; if another connection committed in between, SQLite returns `SQLITE_BUSY_SNAPSHOT` **immediately and does not invoke the busy handler** — no timeout can rescue it. Acquiring the write lock up front (`BEGIN IMMEDIATE`) makes the busy handler apply to the wait and removes the un-retryable upgrade path.
2. **An explicit, embedder-controllable `busy_timeout`** via a new `StoreOptions` hook (`EventStore::open_with` / `open_in_memory_with`), defaulting to `5000ms`. This satisfies both alternatives the issue floats (a set timeout *and* a hook).
3. **A test** — two `EventStore` connections on one file-backed DB doing interleaved concurrent writes, asserting no `SQLITE_BUSY` reaches the caller — plus a façade-level e2e mirroring the exact consumption path Bantaba uses.

### Critical framing correction (must be verified in-implementation)

The issue's premise is *"the SDK sets no `busy_timeout`, so a collision fails immediately."* That is **only half true against the pinned dependency.** `iroh-rooms-core` uses **`rusqlite = "0.37"` (bundled)** (`crates/iroh-rooms-core/Cargo.toml:59`), and rusqlite 0.37 calls `sqlite3_busy_timeout(db, 5000)` on **every** `Connection::open`/`open_in_memory` — SQLite's own C default is `0`, but rusqlite overrides it to 5000ms. So a *write-first* transaction (e.g. `insert`) already waits ~5s today via the library default. The failure Bantaba actually hit is dominated by the **read-then-write lock-upgrade deadlock**, which `busy_timeout` never covers. Two consequences for the implementer:

- **Do not treat the one-line `PRAGMA busy_timeout = 5000` as sufficient.** It changes nothing over rusqlite's existing default and does not fix the upgrade deadlock. Change #1 (`IMMEDIATE`) is the fix that removes the caller-visible `SQLITE_BUSY`.
- **The `busy_timeout: None` opt-out is not a no-op you can skip.** Because rusqlite pre-installs 5000ms, opting out must **explicitly clear it** by calling `conn.busy_timeout(Duration::ZERO)` (0 = restore fail-fast). A naive `if let Some(d) = opts.busy_timeout { conn.busy_timeout(d)?; }` would leave rusqlite's 5000ms in place and make the documented fail-fast opt-out silently false. See D3/AC3.

The implementer must add a test (T5) that reads back `PRAGMA busy_timeout` after a bare `EventStore::open` to *pin the observed rusqlite default* — so the spec stays correct even if the vendored rusqlite version changes.

---

## 2. Background & current repository state

### 2.1 What the store does today

`EventStore` (`crates/iroh-rooms-core/src/store/mod.rs:63`) wraps a single `rusqlite::Connection`. It is `!Sync`; the doc comment already says "share across threads behind your own `Mutex`; multi-connection pooling is future work" (`store/mod.rs:58–65`). Bantaba's pattern — **two independent `EventStore` instances (two connections) on one file** — is exactly the supported way to get concurrency today, and is what this issue hardens.

Open path:

```
open(path)            -> Connection::open(path)          -> from_connection   (store/mod.rs:74–77)
open_in_memory()      -> Connection::open_in_memory()    -> from_connection   (store/mod.rs:83–86)
from_connection(conn) -> schema::apply_pragmas + migrate                      (store/mod.rs:88–92)
```

Pragmas applied on every open (`store/schema.rs:34–38`, via `apply_pragmas`, `schema.rs:146–150`):

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous  = NORMAL;
```

There is **no `busy_timeout` pragma and no hook** — but see §1's correction: rusqlite 0.37 installs a 5000ms handler regardless.

### 2.2 Every write path (all use `BEGIN DEFERRED` today)

`Connection::transaction()` opens a `DEFERRED` transaction. Write sites in `store/mod.rs`:

| Method | Line | Shape | Upgrade hazard? |
|---|---|---|---|
| `insert` | 107 | INSERT → `propagate_from` (writes) | No — first stmt is a write, lock taken at first statement |
| `insert_all` | 119 | loop of `insert_in_tx` | No — first stmt is a write |
| `rebuild` | 392 | SELECT (snapshot) → DELETE/UPDATE | **Yes** — reads `events` before writing |
| `save_sync_state` | 472 | upsert (write) | No — first stmt is a write |
| `save_backfill_tokens` | 534 | DELETE → INSERT | No — first stmt is a write |
| `upsert_parked` | 625 | INSERT/upsert → DELETE → INSERT | No — first stmt is a write |
| `append_trust_decision` | 726 | **SELECT `MAX(seq)+1` → INSERT** | **Yes** — reads then upgrades |
| `delete_parked` | 669 | single `conn.execute(DELETE …)` | N/A — autocommit single statement |

The two "Yes" rows are the ones that can hit `SQLITE_BUSY_SNAPSHOT` on the write-lock upgrade even *with* a busy_timeout, because SQLite deliberately does not run the busy handler when retrying could never succeed inside the same read snapshot. Even the "No" rows benefit from `IMMEDIATE`: it makes the lock-acquisition wait explicit and uniform, so behavior no longer depends on which statement happens to run first.

> Note on the sync engine ingest: `store_and_fanout` (`sync/engine.rs:700–715`) calls `self.store.insert(ev)` and, on error, only logs `"store insert failed"` and drops the event. Absorbing transient contention at the connection (busy_timeout) + removing the upgrade deadlock (IMMEDIATE) is precisely what protects this un-retryable-from-outside path. No app-level retry is added (see D5).

### 2.3 The consumption path (façade)

Bantaba consumes the store through `iroh_rooms::experimental::store`, which re-exports `EventStore, InsertOutcome, InsertStats, ParkedRow, StoreError, StoredEvent, SyncStateRow, TrustRow` from `iroh-rooms-core` (`crates/iroh-rooms/src/experimental/store.rs:5–8`). Any new public type (`StoreOptions`) and constructors (`open_with`, `open_in_memory_with`) **must** be added to that re-export or the consumer cannot reach them.

### 2.4 Existing tests & conventions

- Unit tests: `crates/iroh-rooms-core/src/store/tests.rs` (`#[cfg(test)] mod tests;`, `store/mod.rs:1310–1311`). Helpers build validated genesis/message events; `tempfile = "3"` is a dev-dependency for file-backed tests (`Cargo.toml`).
- Error taxonomy: `StoreError` (`store/error.rs`) — `Sqlite`, `Decode`, `Integrity`, `Migration`; `#[non_exhaustive]`; `From<rusqlite::Error>`.
- CI gate: `scripts/verify.sh` runs `--all-features` with `cargo fmt --check` and `clippy -D warnings` (pedantic). `cargo test` passing ≠ CI green — the store is `--all-features` so it is always fmt/clippy/test exercised.
- Spec-format convention (see `specs/sqlite-event-store-prototype.md`): numbered sections, `D#` decisions, `T#` tests, AC→coverage table.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. Change all `EventStore` write transactions from `DEFERRED` to `IMMEDIATE` via a single private `begin_write()` helper.
2. Add `StoreOptions { busy_timeout: Option<Duration> }` (default `Some(5000ms)`) and constructors `EventStore::open_with(path, opts)` and `EventStore::open_in_memory_with(opts)`.
3. Apply the busy_timeout **unconditionally** (`opts.busy_timeout.unwrap_or(Duration::ZERO)`) so `None` truly restores fail-fast.
4. Keep `open`/`open_in_memory` as thin wrappers over the `_with` forms using `StoreOptions::default()` — zero behavior change for existing callers except the DEFERRED→IMMEDIATE switch.
5. Re-export `StoreOptions` from `iroh_rooms::experimental::store`.
6. Tests: two-connection interleaved-write concurrency (core), fail-fast opt-out, a pin on the observed rusqlite default, and a façade e2e mirroring the acceptance sketch.
7. Doc updates on the pragma/hook and the `IMMEDIATE` guarantee.

### 3.2 Out of scope (do not build here)

- Connection **pooling** or making `EventStore` `Sync` (still "your own `Mutex`"; the two-connections model stands).
- A generic "set any pragma / install any busy handler callback" API. The hook is intentionally narrow: a duration (or opt-out). A closure-based `sqlite3_busy_handler` is rejected in D6.
- App-level retry loops inside the sync engine or CLI (D5).
- Changing `synchronous`, `journal_mode`, WAL autocheckpoint, or `wal_autocheckpoint` tuning.
- Schema/`user_version` change — there is **none**; this touches only connection configuration and transaction behavior (D7).

---

## 4. Key design decisions

### D1 — The real fix is `BEGIN IMMEDIATE`, not the pragma

Convert every write transaction to `TransactionBehavior::Immediate`. Rationale in §1 and §2.2: `busy_timeout` cannot rescue a `DEFERRED` transaction that reads before it writes and then loses the write-lock upgrade race (`SQLITE_BUSY_SNAPSHOT`, busy handler not invoked). `IMMEDIATE` takes the write lock at `BEGIN`, where the busy handler *does* apply, so a second writer waits (up to `busy_timeout`) instead of failing — for **all** write methods uniformly, independent of statement order. This is the change that makes the acceptance test (T1) pass; the pragma alone would not.

Implementation: a single private helper, used by every write method.

```rust
use rusqlite::TransactionBehavior;

impl EventStore {
    /// Begin a write transaction that grabs the write lock up front
    /// (`BEGIN IMMEDIATE`), so a colliding writer *waits* (bounded by the
    /// connection's `busy_timeout`) instead of failing with `SQLITE_BUSY`, and
    /// read-then-write bodies never hit the un-retryable lock-upgrade deadlock.
    fn begin_write(&mut self) -> Result<rusqlite::Transaction<'_>, StoreError> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }
}
```

Replace `let tx = self.conn.transaction()?;` at lines 107, 119, 392, 472, 534, 625, 726 with `let tx = self.begin_write()?;`. `delete_parked` (669) is a single autocommit statement — the busy handler already applies; leave it, or (optional, for uniformity) wrap it in `begin_write()` too. **Do not** change any read method.

Cost: `IMMEDIATE` acquires the write lock slightly earlier, marginally serializing writers that a deferred transaction might have let run further before colliding. For this store's short write critical sections this is negligible and is the correct trade for correctness. Read concurrency is unaffected (WAL readers never block on the write lock).

### D2 — `StoreOptions` hook, defaulting to 5000ms

```rust
use std::time::Duration;

/// Connection configuration for [`EventStore::open_with`].
///
/// `busy_timeout` controls how long a writer waits for a competing writer's
/// lock before failing with `SQLITE_BUSY`. `Some(d)` installs a `d` busy
/// timeout; `None` opts out (fail fast — see the note on the rusqlite default).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoreOptions {
    pub busy_timeout: Option<Duration>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self { busy_timeout: Some(Duration::from_millis(5000)) }
    }
}
```

- `#[non_exhaustive]` so future knobs (e.g. `synchronous`, autocheckpoint) are additive without a breaking change.
- Default 5000ms matches the issue's suggestion and rusqlite's own default, so `open()` behavior is unchanged.
- A builder is unnecessary for one field; `StoreOptions { busy_timeout: Some(...) }` (or `..Default::default()`) is ergonomic enough. Revisit if a second knob lands.

### D3 — Apply the timeout **unconditionally** (the opt-out must clear rusqlite's default)

In the shared constructor, always call `busy_timeout`, mapping `None → Duration::ZERO`:

```rust
fn from_connection_with(conn: Connection, opts: &StoreOptions) -> Result<Self, StoreError> {
    schema::apply_pragmas(&conn)?;
    // Unconditional: rusqlite 0.37 pre-installs a 5000ms timeout on open, so a
    // conditional set would make `busy_timeout: None` a silent no-op (still 5000).
    // `Duration::ZERO` clears the handler → genuine fail-fast opt-out.
    conn.busy_timeout(opts.busy_timeout.unwrap_or(Duration::ZERO))?;
    schema::migrate(&conn)?;
    Ok(Self { conn })
}
```

`rusqlite::Connection::busy_timeout(Duration)` maps to `sqlite3_busy_timeout`; `Duration::ZERO` → `sqlite3_busy_timeout(db, 0)` → handler cleared. Keep the comment; a future "simplify" pass that turns this back into `if let Some(d) = …` reintroduces the bug (see the linked memory / T5).

Order relative to `apply_pragmas`/`migrate`: set the timeout **before** `migrate` so the very first schema write also benefits, though in practice migration runs single-connection at open. Setting it via `conn.busy_timeout()` (not inside the `PRAGMAS` SQL batch) is what lets it be parameterized and cleared; leave `PRAGMAS` (schema.rs:34–38) unchanged.

### D4 — Public constructors: `_with` variants, old ones delegate

```rust
pub fn open(path: &Path) -> Result<Self, StoreError> {
    Self::open_with(path, &StoreOptions::default())
}
pub fn open_with(path: &Path, opts: &StoreOptions) -> Result<Self, StoreError> {
    Self::from_connection_with(Connection::open(path)?, opts)
}
pub fn open_in_memory() -> Result<Self, StoreError> {
    Self::open_in_memory_with(&StoreOptions::default())
}
pub fn open_in_memory_with(opts: &StoreOptions) -> Result<Self, StoreError> {
    Self::from_connection_with(Connection::open_in_memory()?, opts)
}
```

Retain `from_connection(conn)` as `from_connection_with(conn, &StoreOptions::default())` if any internal caller uses it (grep first — e.g. `sync/sim.rs:86` uses `open_in_memory`, which is unaffected). Take `opts` by reference (`&StoreOptions`) to avoid forcing a move/clone on the caller.

### D5 — No app-level retry in the engine or CLI

The issue is explicit: the retry "has to live where the connection is configured." busy_timeout (waiting writer) + IMMEDIATE (no upgrade deadlock) fully cover the SDK's own ingest path, so `store_and_fanout` (`sync/engine.rs:709`) needs no change. Adding a bespoke retry loop there would duplicate SQLite's own busy handler and risk double-counting/side effects. If a *genuine* >5s contention timeout ever fires it still surfaces as `StoreError::Sqlite` and the event is dropped-then-re-synced — acceptable and unchanged from today.

### D6 — Reject a closure-based busy-handler hook

`sqlite3_busy_handler` with a custom callback would let the embedder implement arbitrary backoff, but: (a) it requires an `unsafe`/FFI callback shape that fights the crate's `unsafe_code = forbid` posture, (b) rusqlite's safe surface is `busy_timeout`/`busy_handler(Option<fn>)` and a duration covers every real need here, (c) a `Duration` opt-out already gives the "fail fast" extreme. Keep the hook a plain `Option<Duration>`.

### D7 — No schema / `user_version` bump

This changes connection pragmas and transaction *behavior* only. On-disk format, DDL, and `user_version = 2` are untouched (`schema.rs:27`). A database written by an old binary and a new binary are byte-identical; downgrade/upgrade need no migration. This keeps the change trivially reversible (D-rollout).

---

## 5. Public API surface (delta)

New/changed in `crates/iroh-rooms-core/src/store/mod.rs` and re-exported:

| Symbol | Kind | Notes |
|---|---|---|
| `StoreOptions` | new pub struct | `{ busy_timeout: Option<Duration> }`, `Default`, `Clone`, `Debug`, `#[non_exhaustive]` |
| `EventStore::open_with(&Path, &StoreOptions)` | new pub fn | file-backed with options |
| `EventStore::open_in_memory_with(&StoreOptions)` | new pub fn | in-memory with options (symmetry/tests) |
| `EventStore::open` / `open_in_memory` | unchanged sig | now delegate to `_with` w/ defaults |
| `EventStore::begin_write` | new **private** fn | `BEGIN IMMEDIATE` helper |

Re-export update — `crates/iroh-rooms/src/experimental/store.rs`:

```rust
pub use iroh_rooms_core::store::{
    EventStore, InsertOutcome, InsertStats, ParkedRow, StoreError, StoredEvent,
    StoreOptions, SyncStateRow, TrustRow,   // + StoreOptions
};
```

No change to `StoreError` — a busy timeout still surfaces as `StoreError::Sqlite(rusqlite::Error::SqliteFailure(.. code SQLITE_BUSY ..))`. (Optionally add a `StoreError::is_busy()` convenience predicate for consumers who still want to detect a genuine timeout; see OQ-2 — not required by the ACs.)

---

## 6. Error model & observability

- **Absorbed contention** (the common case): no error — the writer waits ≤ busy_timeout and commits. This is the whole point.
- **Genuine timeout** (>5s sustained contention): `StoreError::Sqlite`, extended code `SQLITE_BUSY` (5) or `SQLITE_BUSY_SNAPSHOT` (517). With IMMEDIATE, `SQLITE_BUSY_SNAPSHOT` should no longer originate from the write-lock upgrade path; a plain `SQLITE_BUSY` only after the full timeout elapses.
- **Fail-fast opt-out** (`busy_timeout: None`): immediate `SQLITE_BUSY` on collision — this is the *tested, documented* behavior (T3), used by callers who want to implement their own policy.
- Observability: the store has no tracing subscriber of its own (consistent with the rest of the CLI, which drops tracing output). No new logs are mandated. The sync engine's existing `"store insert failed: {e}"` log (`engine.rs:712`) remains the single breadcrumb for a genuine timeout; it now fires far less often. Do not add noisy per-retry logging (SQLite's handler is internal).

---

## 7. Security, privacy, reliability, performance

- **Security/privacy:** none affected. No new data, no new surface reachable by a peer; `busy_timeout` is a local connection setting. A malicious peer cannot induce write contention beyond what ordinary sync traffic already does.
- **Reliability:** strictly improved. IMMEDIATE removes an un-retryable deadlock; busy_timeout absorbs transient collisions. Durability unchanged (`synchronous = NORMAL`, WAL). `events`/`event_id`+`wire` remain authoritative; a dropped write after a genuine timeout is recovered on the next sync round (no corruption, spec D5 of the store).
- **Performance:** IMMEDIATE marginally serializes concurrent writers (they take the write lock at BEGIN rather than at first write). Critical sections are short; the alternative is caller-visible failures. Readers are never blocked (WAL). The default 5000ms only bounds the *worst-case* wait; a healthy system rarely waits milliseconds.
- **Test-runtime caveat (from prior measurement):** file-backed WAL contention tests are *slow* when the losing thread's SQLite busy handler escalates its internal backoff sleeps (1→2→5→…→100ms). Keep the interleaved-write count small when the critical section is heavy: `insert` runs `propagate_from` (graph writes), so use **N≈4** iterations per connection there; a trivial critical section like `append_trust_decision` tolerates N≈100 at ~0.06s. Size T1 accordingly so it stays well under CI limits.

---

## 8. Implementation steps

Ordered; each step compiles. No production behavior beyond the store connection/transaction config changes.

1. **`store/mod.rs` imports & `StoreOptions`.** Add `use std::time::Duration;` and `use rusqlite::TransactionBehavior;`. Define `StoreOptions` (D2) with derives and `#[non_exhaustive]`. Add `pub use ... StoreOptions` to the `pub use model::{...}` region or a new `pub use` line near `store/mod.rs:49–50`.
2. **Constructors.** Add `open_with`, `open_in_memory_with` (D4); rewrite `open`/`open_in_memory` to delegate. Replace `from_connection` with `from_connection_with(conn, opts)` (D3) — the unconditional `conn.busy_timeout(...unwrap_or(ZERO))?`. Keep a `from_connection` shim only if an internal caller needs it (grep `from_connection`).
3. **`begin_write` helper + write-site swap.** Add `begin_write` (D1). Replace `self.conn.transaction()?` at lines 107, 119, 392, 472, 534, 625, 726 with `self.begin_write()?`. Optionally wrap `delete_parked` (669). Confirm no read method changed.
4. **Docs.** Update the `EventStore` struct doc (`store/mod.rs:58–65`) and the `PRAGMAS` doc (`schema.rs:29–38`) to state: WAL single-writer, IMMEDIATE write transactions, a default 5000ms busy_timeout, and the `open_with`/`StoreOptions` opt-out. Note that the shared-file / two-connection pattern is the supported concurrency model.
5. **Façade re-export.** Add `StoreOptions` to `crates/iroh-rooms/src/experimental/store.rs` (§5). Update its module doc line if it enumerates the surface.
6. **Tests** — see §9: core unit/concurrency tests in `store/tests.rs`, façade e2e in `crates/iroh-rooms/tests/store_concurrency_e2e.rs` (new).
7. **README / SDK coverage.** Add a one-line note under the store bullets (README `EventStore` section ~L581) that concurrent writers on a shared DB are supported via a default 5000ms busy_timeout + IMMEDIATE writes, configurable through `StoreOptions`. Optionally note in `docs/sdk-coverage.md`.
8. **Verify.** Run `scripts/verify.sh` (`--all-features`, fmt `--check`, clippy `-D warnings` pedantic) — not just `cargo test`. Run the ignored/e2e concurrency test explicitly if it is gated.

---

## 9. Test strategy

All tests use the existing `store/tests.rs` helpers (validated genesis/message builders) and `tempfile` for file-backed DBs. Map 1:1 to ACs (§10).

- **T1 — Acceptance sketch: two connections, interleaved concurrent writes, no `SQLITE_BUSY` (core).** Create one `tempfile` DB path. Open **two** `EventStore` instances on it (`open` — default 5000ms). From two threads, interleave writes for a small N (N≈4 for `insert` of distinct valid events sharing a room genesis; the genesis is inserted once first so both connections have the parent). Join; assert **every** `insert`/write returned `Ok` (no `StoreError::Sqlite`/`SQLITE_BUSY` escaped). Cross-check both connections read back the union of events. Keep N small per §7.
- **T2 — Read-then-write path specifically (`append_trust_decision`) under contention.** Two connections; both loop `append_trust_decision` on the same room concurrently (N up to ~100, cheap critical section). Assert no `SQLITE_BUSY` and that the per-room `seq` values are a gap-free set (append-only monotonicity preserved under concurrency). This is the case IMMEDIATE fixes that a bare busy_timeout would not.
- **T3 — Fail-fast opt-out.** Open two connections with `StoreOptions { busy_timeout: None }`. Hold a write transaction open on connection A (begin a write, don't commit), then attempt a write on connection B; assert it returns `StoreError::Sqlite` with an `SQLITE_BUSY`-class code **promptly** (no ~5s stall). Proves `None` truly clears the handler (not the rusqlite default). *(Structure to avoid a real 5s wait; a held-open transaction gives a deterministic immediate collision.)*
- **T4 — Default path waits, then succeeds.** Mirror T3 but with default options and a short-lived held write on A (released on another thread after a few ms); B's write blocks then succeeds `Ok`. Confirms the default waits rather than fails.
- **T5 — Pin the observed rusqlite default (regression guard for the framing correction).** After a bare `EventStore::open`, read `PRAGMA busy_timeout` back; assert it equals 5000 (documents that rusqlite pre-installs it, so the D3 unconditional-clear logic is necessary). Then open with `busy_timeout: None`, read it back, assert `0`. This test fails loudly if a future rusqlite bump changes the default, prompting a docs/behavior review.
- **T6 — Façade e2e (`crates/iroh-rooms/tests/store_concurrency_e2e.rs`, new).** Reproduce T1's two-connections-interleaved-writes acceptance sketch using **only** façade imports (`iroh_rooms::experimental::store::{EventStore, StoreOptions, InsertOutcome, ...}`), proving the fix and the `StoreOptions` hook are reachable through the exact path Bantaba consumes. Also assert `open_with(path, &StoreOptions::default())` works via the façade. Follow the import discipline of `facade_e2e.rs`. Gate/ignore if it is heavier than the CI tier expects; run under the `--all-features`/e2e lane.

Non-goals for tests: no attempt to prove a specific wait *duration*; no flaky timing assertions beyond "prompt failure" (T3) vs "eventually succeeds" (T4) using held-open transactions rather than sleeps where possible.

---

## 10. Acceptance criteria → coverage

| # | Criterion (from issue) | Mechanism | Test |
|---|---|---|---|
| AC1 | Two `EventStore` connections on one file-backed DB doing interleaved concurrent writes pass with **no `SQLITE_BUSY` reaching the caller** | IMMEDIATE write txns (D1) + 5000ms default busy_timeout (D2/D3) | T1, T2, T6 |
| AC2 | A `busy_timeout` is set (issue's minimal ask) | Default `StoreOptions.busy_timeout = 5000ms`, applied unconditionally (D3) | T4, T5 |
| AC3 | Embedder can set/opt-out the handler (issue's `open_with` alternative) | `open_with` / `open_in_memory_with` + `StoreOptions`; `None` clears via `Duration::ZERO` | T3, T5 |
| AC4 | The SDK's own ingest write path (sync pump) is protected from outside | Fix lives at the connection (D1/D3); `store.insert` at `engine.rs:709` inherits it; no app retry (D5) | T1 (uses `insert`) |
| AC5 | Reachable through the consumed façade | `StoreOptions` + constructors re-exported (§5) | T6 |

---

## 11. Rollout / rollback

- **Rollout:** pure library change, no schema/`user_version` change (D7), no data migration, no config. Ships in the normal crate release; existing `open()` callers get IMMEDIATE writes + the (already-effective) 5000ms timeout transparently.
- **Rollback:** revert the commit. Because on-disk format is untouched, a DB touched by the new binary is fully readable/writable by the old one and vice versa. No forward/backward compatibility hazard.
- **Blast radius:** every `EventStore` writer now uses IMMEDIATE. The only behavioral difference for a single-connection user is that write transactions take the write lock at BEGIN — functionally invisible (a single connection never contends with itself across transactions).

---

## 12. Risks & mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Implementer sets only `PRAGMA busy_timeout` and skips IMMEDIATE, leaving the upgrade deadlock → AC1 still fails intermittently | Medium (the issue's own "smallest fix" invites this) | §1 framing correction is front-and-center; T2 specifically exercises the read-then-write path; D1 marked load-bearing |
| `busy_timeout: None` written as a conditional set → opt-out is a silent no-op (rusqlite's 5000ms persists) | Medium | D3 mandates unconditional `unwrap_or(ZERO)`; T5 asserts `PRAGMA busy_timeout == 0` for `None`; comment warns against "simplifying" it back |
| Concurrency test is slow/flaky due to escalating busy-handler backoff sleeps | Medium | §7 sizing guidance: N≈4 for heavy `insert`, N≈100 only for cheap `append_trust_decision`; prefer held-open-transaction determinism over sleeps in T3/T4 |
| IMMEDIATE reduces write parallelism enough to matter | Low | Critical sections are short; readers unaffected; correctness > marginal throughput; revisit only if profiling shows a hotspot |
| `StoreOptions` not added to façade re-export → consumer can't reach the hook (AC5) | Low | Step 5 + T6 force the façade path |
| Future rusqlite bump changes the default timeout, invalidating the framing | Low | T5 pins the observed default and fails loudly on change |
| Clippy pedantic / fmt failures block CI despite green `cargo test` | Medium | Step 8: run `scripts/verify.sh`, not just tests |

---

## 13. Open questions

- **OQ-1:** Should `EventStore` expose a `Sync`/pooled variant so a single instance can be shared instead of opening N connections? Out of scope here (§3.2); the two-connection model is what Bantaba uses and what this hardens. Track separately if pooling is desired.
- **OQ-2:** Add `StoreError::is_busy(&self) -> bool` for consumers who still want to detect a genuine post-timeout `SQLITE_BUSY`? Not required by the ACs (the point is they *don't* see it). Cheap and additive if desired; defer unless a consumer asks.
- **OQ-3:** Default of 5000ms — keep, or lower (e.g. 2000ms) so a genuinely stuck writer surfaces faster? Keeping 5000ms matches the issue and rusqlite's default and avoids a behavior change. Left at 5000ms; make it obvious it is tunable via `StoreOptions`.
- **OQ-4:** Should `delete_parked` (`store/mod.rs:669`, currently a bare `conn.execute`) be wrapped in `begin_write()` for uniformity, or left as an autocommit single statement (already busy-handler-covered)? Leaning "leave it"; flagged for the reviewer.

## 14. Assumptions

- The pinned **rusqlite 0.37 (bundled)** installs a 5000ms `busy_timeout` on open (verified in prior work; re-pinned by T5). If the vendored version differs, T5 catches it and D3's unconditional-clear logic keeps `None` correct regardless.
- Bantaba's contention is between **separate connections on one file** (as stated in the issue), not intra-connection — so WAL single-writer semantics and the connection-level busy handler are the right layer.
- WAL mode remains the journal mode (`schema.rs:35`); the fix assumes WAL's one-writer/many-reader model.
- CI runs `--all-features`, so the `store`-gated code and its tests are always exercised; the façade e2e runs in the same lane as `facade_e2e.rs`.
- No consumer relies on the current `DEFERRED` behavior (e.g. on a write transaction *not* taking the lock until the first write). This is an internal detail; grep confirms only `EventStore` opens these transactions.
