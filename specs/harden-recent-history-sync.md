# Spec: Harden Recent History Sync

| | |
|---|---|
| **Issue** | #26 — [IR-0201] Harden recent history sync |
| **Parent** | #3 |
| **Labels** | type/feature, area/protocol, area/transport, area/storage, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §10.7 (Sync Limits — bounded by count/time, invalid rejected+logged, deep conflict deferred), §15.5 (Sync Recent History journey + acceptance), §17.1 items 7 ("Local history survives restart") & 8 ("Recent history sync works after reconnect for a small room"), §12 (Local Storage — `events`, `sync_state`, `trust_decisions` tables) · `PHASE-0-SPIKE.md` ADR-1 (full-mesh QUIC), ADR-2 (bounded recent-sync pull), Membership & Ordering §0 (incompleteness detection, admin-tip, fail-closed), §4 (out-of-order pipeline, anti-amplification), §5 (fail-closed access), §9 (`sync_state`/`trust_decisions` are derived caches rebuildable by re-folding `events`) |
| **Dependencies (all landed)** | #11 IR-0007 bounded recent-sync engine (`iroh-rooms-core::sync`), #12 IR-0008 membership fold (`iroh-rooms-core::membership`), #22 IR-0107 peer connection manager (`iroh-rooms-net::PeerManager` + `SnapshotAdmission`), #24 IR-0109 two-peer integration test (`iroh-rooms-cli/tests/two_peer_e2e.rs`) |
| **Status** | **Implemented** — store schema v2 and process-restart durability landed in `iroh-rooms-core::store` and `iroh-rooms-core::sync` (issue #26 / IR-0201). |
| **Type** | Production code: a **store schema v2** migration adding the `sync_state` / `trust_decisions` derived-cache tables PRD §12 names (realized as five physical tables — §4.2), plus wiring `SyncEngine` to persist and restore its in-flight sync state across a **process restart** (not merely a transport reconnect) — closing the prototype's deferred OQ-3/D7. Graduates the Phase-0 prototype (#11) to the MVP recent-history-sync implementation. |

---

## 1. Summary

The bounded recent-sync **engine** (`iroh-rooms-core::sync`, #11) and its **real QUIC carrier**
(`iroh-rooms-net`, #9/#22) are landed and conformance-tested: an offline peer reconnects and
converges, the membership sub-DAG is never windowed, missing parents are buffered and backfilled,
duplicates are idempotent, invalid frames are dropped, and admin-tip incompleteness fails closed.
What the prototype **explicitly deferred** (spec `bounded-recent-sync-prototype.md` D7 / OQ-3, and
`store/mod.rs:27`) is the one remaining MVP requirement this issue owns:

> **The engine's in-flight sync state lives only in memory and is reconstructed from the `events`
> table on `open`. A process restart therefore loses the orphan park, the backfill rate-limiter
> state, the unconfirmed higher-admin-tip suspicion, and the equivocation audit trail.**

For the *steady-state convergent set* this is harmless — the `events` table is the single source of
truth and `SyncEngine::open` re-folds it losslessly (`engine.rs:231`). But it leaves **four gaps**
that the issue's acceptance criteria and the PRD's success metrics (§17.1.7 "local history survives
restart", §17.1.8 "recent history sync works after reconnect") require closing:

1. **Fail-*open* on restart (security, headline).** A node that has raised
   `Completeness::AdminViewSuspect` from a peer's advertised higher admin tip is holding a
   removal-sensitive access gate **closed** (spec D6, `engine.rs:493/500`). On restart that
   suspicion — which is *not* a held event and so is *not* rebuilt from `events` — evaporates, and
   the gate silently reopens before catch-up. A reboot must never be a way to clear a fail-closed
   posture.
2. **Lost in-flight buffering.** Causally-incomplete-but-plausible frames parked awaiting backfill
   (`park`, `engine.rs`) are dropped on restart. The AC "missing parent buffering **and retry**" and
   "sync state survives restart" argue for the park (and its retry) surviving a crash, not only a
   transport reconnect (the prototype's A4 guarantee was transport-reconnect only).
3. **Reset rate-limiter (anti-amplification) state.** The per-author backfill token buckets
   (`tokens`) and the unconfirmed-tip attempt budget (`suspect_tip`) reset to full on restart, so a
   crash-loop could be used to bypass the §4 amplification bounds.
4. **Wiped equivocation audit trail.** `trust_decisions` (CRITICAL `equivocation` on an admin
   self-fork, `engine.rs:216/529`) is security-relevant evidence (PRD §13.2, §16.3) that currently
   does not survive a reboot.

This issue adds a **store schema v2** (`user_version = 2`) realizing the two derived-cache tables
PRD §12 names beyond `events` — `sync_state` and `trust_decisions` — as **five physical SQLite
tables** (`sync_state`, `sync_backfill_tokens`, `sync_parked`, `sync_parked_missing`,
`trust_decisions`; the park, its missing-parent edges, and the per-author token buckets are naturally
row-per-entry rather than columns on the single-row `sync_state`), and wires
`SyncEngine` to **checkpoint** its non-rebuildable state and **restore** it on `open`, while keeping
`events` authoritative and the fold/admin-tip/completeness **rebuilt** from it (no duplication, no
drift — Persistence note §9). It then adds an **IR-0201 integration suite** proving all five
acceptance criteria over a real on-disk store, including the two scenarios the landed tests do not
cover: **restart with in-flight state** and **over-the-wire invalid-event injection that is dropped,
logged, not stored, and not re-broadcast.**

**What this issue is NOT.** It does not re-open the transport (ADR-1) or sync-substrate (ADR-2)
decisions, does not build full decentralized reconciliation (PRD §7.3.14 / §10.7 — deferred), and
does not re-implement any of the landed engine mechanics. It is a **storage + durability +
integration-hardening** slice on a frozen engine.

---

## 2. Background & current repository state

Read before implementing.

### 2.1 What is already landed (do **not** rebuild)

| Concern | Landed in | Location | Reused / hardened here |
|---|---|---|---|
| Sans-IO `SyncEngine` (ingest/publish/on_connect/on_disconnect/on_message/on_tick) | #11 | `core/src/sync/engine.rs:231–405` | **Reused verbatim**; gains a persistence-restore path in `open` and a checkpoint hook. |
| Anti-amplification bounds (`SyncConfig`, 10 params + defaults) | #11 | `core/src/sync/config.rs:9–58` | Reused; `tokens`/`suspect_tip` state becomes restart-durable. |
| Orphan park (per-author + global caps, oldest-first eviction, depth track) | #11 | `engine.rs` park fields + eviction `~778–829` | **Persisted** (new `sync_parked` table); retry re-kicked on `open`. |
| Admin-tip incompleteness detector + `Completeness` + fail-closed subjects | #11 | `engine.rs:493/500`, completeness `~1137–1221` | **Suspicion persisted** so a restart cannot fail-open. |
| Equivocation trust decisions (CRITICAL on admin self-fork) | #11 | `engine.rs:216/529`, record `~1223` | **Persisted** as an append-only audit log (`trust_decisions` table). |
| `SyncDigest` convergence oracle + `room_event_ids` | #11/#8 | `engine.rs:508`, `store/mod.rs:177` | Reused as the set-equality oracle in the new integration tests. |
| Deterministic `SimNet` harness (shuffle/delay/drop/partition/reconnect) | #11 | `core/src/sync/sim.rs` | Reused; gains a **restart** helper (drop + re-`open` a peer over its store). |
| SQLite `EventStore` (`events` + `event_parents`, `user_version = 1`) | #8 | `core/src/store/{schema,mod}.rs` | **Migrated to v2** (additive tables only; `events` untouched). |
| Real QUIC carrier (`NetTransport`, EVENT_ALPN, admission-before-bytes) | #9 | `net/src/{transport,handler,alpn}.rs` | Reused verbatim. |
| `Node` pump driving the engine over real QUIC (on_connect/message/tick/disconnect @ 250 ms) | #9/#11 | `net/src/node.rs:739–920`, `DEFAULT_TICK` | Reused; checkpoint runs inside the single-owner pump. |
| `PeerManager` roster reconciliation + `SnapshotAdmission` live gate + dial-loop reconnect/backoff | #22 | `net/src/{manager,admission,peer}.rs` | Reused verbatim; the restart AC composes with it. |
| CLI two-peer e2e + `manager_e2e` stop→restart no-dup | #22/#24 | `net/tests/manager_e2e.rs:620`, `cli/tests/two_peer_e2e.rs` | Extended (restart-with-in-flight-state; invalid injection). |

**Existing sync test coverage (green in CI).** `core/tests/sync_smoke.rs` and
`core/tests/sync_convergence.rs` already prove, at the deterministic SimNet layer: offline reconnect
+ convergence, tiny-chat-window/never-windowed membership, child-before-parent buffering+backfill,
1000× duplicate idempotency, shuffled-delivery determinism across seeds, non-member flood dropped +
park bounded, stale-admin-tip fail-closed-then-recover, admin-fork CRITICAL equivocation, 5-peer
mesh. `net/tests/manager_e2e.rs::managed_room_reconnect_delivers_no_duplicates` proves a stop→restart
reconnect over **real loopback QUIC** — **but** the restarted peer is re-seeded with exactly the
events it held while quiescent (`manager_e2e.rs:759`), so it exercises only the *event-log* restart,
**not** a restart with **in-flight sync state** (a non-empty park, a raised suspicion, or a recorded
trust decision). That is precisely the AC5 gap.

### 2.2 The persistence gap, precisely

`SyncEngine` (`engine.rs:184–220`) holds these fields; the table classifies each by whether it is
**rebuildable from `events`** (and therefore needs no new storage) vs **genuinely transient**
(the subject of this issue):

| Engine field | Rebuildable from `events` on `open`? | Restart behavior today | Hardening |
|---|---|---|---|
| `fold: RoomMembership` | **Yes** — `from_events` (`engine.rs:248`) | Correctly restored | keep rebuilding; **do not** persist |
| `admin_ids_by_seq` (held admin events per seq) | **Yes** — `seed_admin_state` from held chain (`engine.rs:268`) | Correctly restored; a genuine held fork is re-detected | keep rebuilding; **do not** persist |
| `completeness` / `fail_closed` (the *derived* part) | **Yes** — `recompute_completeness` (`engine.rs:269`) | Re-derived from held state | keep rebuilding |
| `suspect_tip` (an advertised higher tip **not** yet held) | **No** — not an event | **Lost ⇒ AdminViewSuspect clears ⇒ fail-OPEN** | **persist** (`sync_state`) |
| `park` (causally-incomplete frames awaiting backfill) | **No** — not yet in `events` | **Lost**; refetched only if a peer re-pushes | **persist** (`sync_parked`) |
| `tokens` (per-author backfill budget) | **No** | Reset to full ⇒ amplification bound resets | **persist** (`sync_state`, coarse) |
| `trust_decisions` (equivocation audit) | Recomputable for *held* forks, **not** for cross-partition history | Wiped | **persist** append-only (`trust_decisions`) |
| `peers`, `counters`, `logs` | live/session | reset | not persisted (session-scoped) |

### 2.3 Workspace conventions (must follow)

- Edition 2021, `rust-version = 1.80`, `unsafe_code = "forbid"`, clippy `all` + `pedantic` = warn.
- **`scripts/verify.sh` is the real CI gate** (memory: *verify-sh-is-the-real-ci-gate*): `cargo fmt
  --all --check`, `cargo clippy --workspace --all-targets --all-features -D warnings` (pedantic), and
  `cargo test --workspace --all-targets --all-features`. Because CI passes `--all-features`, the
  `store` and `sync` features are always fmt/clippy/test-exercised. New code must be pedantic-clean.
- Hand-rolled error enums (`Display` + `std::error::Error`); reuse `StoreError` / `RejectReason` /
  `SyncError` — **no new taxonomy**. No panics on peer-supplied or stored bytes.
- Deterministic ordering everywhere (`BTreeMap`/`BTreeSet`, bytewise `EventId` order).
- The store's existing D4 discipline: `events.event_id` + `events.wire` are **authoritative**; every
  other table is a **derived cache rebuildable from them**. The five new tables are derived caches;
  a `rebuild()`-equivalent for them is "drop them and re-derive from `events` + reconnect".
- **CLI has no tracing subscriber** (memory: *cli-has-no-tracing-subscriber*): `TracingAudit` output
  is dropped on the CLI. Any AC that requires a drop/reject to be *observable* (AC3 "invalid events
  are … logged") must surface it through an explicit non-tracing sink (the engine's bounded
  `logs()` / `counters()` surface, or the net `AuditSink`), **not** a `tracing` log.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. **Store schema v2** (`user_version = 2`): a forward, additive migration adding **five** derived-cache
   tables — `sync_state`, `sync_backfill_tokens`, `sync_parked`, `sync_parked_missing`,
   `trust_decisions` — scoped by `room_id` (realizing the two PRD §12 names `sync_state` /
   `trust_decisions`). `events` / `event_parents` are untouched; a v1 database upgrades in place (the
   new tables are created empty).
2. **Restore on `open`:** `SyncEngine::open` (in addition to re-folding `events`) reloads the
   persisted park, the unconfirmed admin-tip suspicion, the rate-limiter state, and the trust-decision
   audit log, then re-derives `completeness` from **both** the held chain **and** the restored
   suspicion — so a fail-closed posture survives a restart.
3. **Checkpoint on mutation:** a persistence hook that writes the non-rebuildable state to SQLite as
   it changes (parked-frame insert/evict/wake, suspicion raise/clear, token consume/refill,
   trust-decision record), transactionally, so a crash loses at most one tick of state.
4. **Retry survives restart:** on `open`, re-issue `WantEvents` for the missing parents of every
   restored parked frame at the next `on_connect`/`on_tick`, so buffering **and retry** are durable.
5. **Bounds hold across restart:** the persisted park honors `MAX_PARKED_PER_AUTHOR` /
   `MAX_PARKED_TOTAL`; the persisted token buckets are not refilled *by* the restart. A crash-loop
   cannot exceed the steady-state amplification budget by more than one restart's worth.
6. **Observable rejection (AC3):** ensure the "invalid event rejected + logged" path is surfaced
   through a **non-tracing** sink so it is observable on the CLI and in tests (engine `logs()` /
   `counters().rejected` and/or the net `AuditSink`), given the CLI-has-no-subscriber constraint.
7. **IR-0201 integration suite** covering all five acceptance criteria over a **persistent on-disk**
   store, adding the two uncovered dimensions: **restart-with-in-flight-state** and
   **over-the-wire invalid-event injection**, plus the issue Test Plan's shuffled-delivery and
   set-equality assertions.
8. **Docs:** README *Current Status* (mark IR-0201 recent-history-sync as MVP-complete), a short
   Gate-D/restart memo, and `NOTES.md` closing the OQ-3/D7 follow-up.

### 3.2 Out of scope (deferred / sibling — do **not** implement here)

- **Full decentralized history reconciliation / Meyer RBSR** (PRD §7.3.14, §10.7; ADR-2) — parked
  for Phase 5. This issue hardens the *bounded* path only.
- **Key rotation / native invite revocation** (PRD §13.4) — the fold already encodes sticky
  departure; sync adds no revocation channel.
- **Gossip-carried admin-tip advertisement** (ADR-1 optional channel) — admin-tip stays on the mesh
  pull RPC (prototype OQ-4).
- **Multi-device per identity, multi-admin** — deferred protocol scope; `admin_seq` remains a single
  immutable admin chain (prototype A2).
- **Blob/File plane sync and Pipe lifecycle sync** — separate planes; this issue moves only signed
  `WireEvent`s of the Room Event Plane.
- **Multi-connection store pooling / async store** — `EventStore` stays synchronous, single
  connection behind the pump's single-owner discipline (store §10).
- **A CLI `room sync`/`room peers` noun** — the connection surface is IR-0107's; this issue only
  ensures the reject/park/trust observability surfaces are reachable.

### 3.3 Why the split is safe

The engine below the persistence hook is a **pure function of (local store/fold state, inbound
messages)** and is frozen + conformance-tested. Persistence is **additive and derived**: `events`
stays the single source of truth; the five new tables are caches that (a) make genuinely-transient
state durable and (b) can be dropped and re-derived by re-folding `events` + reconnecting. Restoring
them on `open` changes no convergence rule — it only prevents a restart from *losing* buffering or
*clearing* a fail-closed gate. The migration is forward-only and additive, so no existing behavior,
wire format, or `events` row is touched.

---

## 4. Domain model

### 4.1 The persistence boundary (the single most important classification)

Every piece of engine state is exactly one of:

- **Authoritative** — `events` / `event_parents` (unchanged). The convergent validated set.
- **Rebuildable derived** — fold, held admin chain, and the *derived* part of completeness/fail-closed.
  Recomputed from `events` on `open`. **Never** persisted (avoids drift; Persistence note §9).
- **Transient non-rebuildable** — park, unconfirmed suspicion, rate-limiter buckets, trust-decision
  audit. **Persisted** by this issue (the five v2 tables) and restored on `open`.

The correctness invariant the tests pin: **dropping the five v2 tables and re-opening must converge
to the same steady state via reconnect** (they are caches, not sources of truth) — *and* keeping them
must preserve a fail-closed posture and an in-flight park across a crash.

### 4.2 Schema v2 (SQLite DDL, `user_version = 2`)

Additive migration; run only when `user_version < 2`. `events` / `event_parents` DDL is unchanged.

```sql
-- Per-room sync cursor + rate-limiter + unconfirmed-admin-tip state (single row per room).
-- All columns are a DERIVED CACHE: droppable and re-derivable from `events` + reconnect.
CREATE TABLE IF NOT EXISTS sync_state (
    room_id             BLOB    NOT NULL PRIMARY KEY,      -- 32 bytes
    -- recent-chat cursor: the highest (lamport,event_id) we have served/pulled as "recent"
    chat_cursor_lamport INTEGER,                           -- advisory optimization; NULL = none yet
    chat_cursor_event   BLOB,                              -- 32 bytes; tie-break with lamport
    -- unconfirmed higher admin tip advertised by a peer but not yet backfilled (spec D6).
    -- Its presence is what keeps Completeness::AdminViewSuspect across a restart (anti fail-open).
    suspect_tip_event   BLOB,                              -- 32 bytes; NULL = no suspicion
    suspect_tip_seq     INTEGER,                           -- admin_seq of the suspicion
    suspect_tip_attempts INTEGER NOT NULL DEFAULT 0,       -- attempts spent (bounded by config)
    updated_at          INTEGER NOT NULL                   -- ms epoch; advisory/debug only
) STRICT;

-- Per-(room, author) backfill token bucket (anti-amplification; spec §4.4).
CREATE TABLE IF NOT EXISTS sync_backfill_tokens (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    author_id   BLOB    NOT NULL,                          -- 32 bytes (requesting/parked-frame author)
    tokens      INTEGER NOT NULL,                          -- current bucket level
    PRIMARY KEY (room_id, author_id)
) STRICT;

-- The orphan park: causally-incomplete-but-plausible frames awaiting backfill (spec §6.2).
-- Bounded by MAX_PARKED_PER_AUTHOR / MAX_PARKED_TOTAL; oldest-first eviction by park_seq.
CREATE TABLE IF NOT EXISTS sync_parked (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    event_id    BLOB    NOT NULL,                          -- 32 bytes (the parked frame's id)
    wire        BLOB    NOT NULL,                          -- verbatim WireEvent bytes (re-validated on load)
    author_id   BLOB    NOT NULL,                          -- 32 bytes (per-author cap key)
    park_seq    INTEGER NOT NULL,                          -- monotone arrival order (eviction key)
    depth       INTEGER NOT NULL DEFAULT 0,                -- backfill chain depth chased (<= MAX_BACKFILL_DEPTH)
    PRIMARY KEY (room_id, event_id)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_parked_room_seq    ON sync_parked(room_id, park_seq);
CREATE INDEX IF NOT EXISTS idx_parked_room_author ON sync_parked(room_id, author_id);
-- The missing parents each parked frame is waiting on (drives the WantEvents retry on open).
CREATE TABLE IF NOT EXISTS sync_parked_missing (
    room_id     BLOB    NOT NULL,
    event_id    BLOB    NOT NULL,                          -- the parked child
    missing_id  BLOB    NOT NULL,                          -- a parent it is waiting for
    PRIMARY KEY (room_id, event_id, missing_id),
    FOREIGN KEY (room_id, event_id) REFERENCES sync_parked(room_id, event_id) ON DELETE CASCADE
) STRICT;

-- Append-only equivocation / incompleteness audit trail (PRD §13.2, §16.3). Survives restart so a
-- reboot cannot erase a CRITICAL admin-fork alert. Recomputable for HELD forks; retained for history.
CREATE TABLE IF NOT EXISTS trust_decisions (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    seq         INTEGER NOT NULL,                          -- per-room monotone insertion order
    code        TEXT    NOT NULL,                          -- 'equivocation' | 'admin_view_suspect'
    severity    TEXT    NOT NULL,                          -- 'critical' | 'warning'
    admin_seq   INTEGER,                                   -- the contested admin_seq (if any)
    event_ids   BLOB    NOT NULL,                          -- CBOR array of the implicated raw ids
    created_at  INTEGER NOT NULL,                          -- ms epoch; advisory/debug only
    PRIMARY KEY (room_id, seq)
) STRICT;
```

Notes:
- **`STRICT` + raw-BLOB ids** match the store's D3 convention (`memcmp` == the protocol tie-break).
- **Only `wire` in `sync_parked` is load-bearing**; `author_id`/`depth`/`park_seq` are re-derivable
  from it + arrival but stored to avoid re-decoding on the hot eviction path. A parked `wire` is
  **re-validated with `validate_wire_bytes` on load** (never trusted blindly; corruption ⇒ drop that
  parked row + log, never a panic).
- **`suspect_tip_*` is the anti-fail-open field.** It is a peer's *claim*, not proof, so it is bounded
  by `suspect_tip_attempts <= max_unconfirmed_tip_attempts` (config, `config.rs:56`) exactly as the
  in-memory version is (a fabricated higher tip still cannot pin fail-closed forever — the bound just
  now spans restarts).
- `created_at`/`updated_at` are **advisory** (never ordering/security), matching the store's
  `created_at` discipline.

### 4.3 New/changed types (indicative)

```rust
// core/src/store/  (feature = "store") — additive; no change to existing rows.
pub struct ParkedRow { pub event_id: EventId, pub wire: WireEvent, pub author: IdentityKey,
                       pub park_seq: u64, pub depth: u32, pub missing: Vec<EventId> }
pub struct SyncStateRow { pub chat_cursor: Option<(u64, EventId)>,
                          pub suspect_tip: Option<(EventId, u64, u32)>, /* id, seq, attempts */ }
pub struct TrustRow { pub seq: u64, pub code: String, pub severity: Severity,
                      pub admin_seq: Option<u64>, pub event_ids: Vec<EventId>, pub created_at: u64 }

impl EventStore {
    // -- sync_state (single row per room) --
    pub fn load_sync_state(&self, room: &RoomId) -> Result<Option<SyncStateRow>, StoreError>;
    pub fn save_sync_state(&mut self, room: &RoomId, st: &SyncStateRow) -> Result<(), StoreError>;
    // -- backfill tokens --
    pub fn load_backfill_tokens(&self, room: &RoomId)
        -> Result<BTreeMap<IdentityKey, u32>, StoreError>;
    pub fn save_backfill_tokens(&mut self, room: &RoomId,
        tokens: &BTreeMap<IdentityKey, u32>) -> Result<(), StoreError>;
    // -- park --
    pub fn load_parked(&self, room: &RoomId) -> Result<Vec<ParkedRow>, StoreError>;
    pub fn upsert_parked(&mut self, room: &RoomId, row: &ParkedRow) -> Result<(), StoreError>;
    pub fn delete_parked(&mut self, room: &RoomId, id: &EventId) -> Result<(), StoreError>;
    // -- trust decisions (append-only) --
    pub fn load_trust_decisions(&self, room: &RoomId) -> Result<Vec<TrustRow>, StoreError>;
    pub fn append_trust_decision(&mut self, room: &RoomId, row: &TrustRow)
        -> Result<u64, StoreError>;   // returns assigned seq
}
```

No change to the existing `EventStore` read/write surface, `StoredEvent`, `InsertOutcome`, or the
`sync` public API (§7). The engine gains **private** persistence plumbing plus the restore branch in
`open`; the *public* `SyncEngine` surface is unchanged, so `iroh-rooms-net` and the CLI need no
signature changes (only the `EventStore` handed to `open` is now a v2 store — automatic via migration).

---

## 5. Key design decisions

### D1 — `events` stays authoritative; the five v2 tables are derived caches (no duplication)

Do **not** persist the fold, the held admin chain, or the *derived* completeness/fail-closed —
they are pure functions of `events` and are already rebuilt losslessly on `open` (`engine.rs:248/268/269`).
Persisting them would violate the store's D4 derived-cache discipline and risk the cache disagreeing
with the log. Persist **only** the genuinely non-rebuildable state (D2). **Rationale:** the honest
convergence claim rests on `events` being the single source of truth; the caches must be droppable.

### D2 — Persist exactly the four non-rebuildable things, and nothing else

`sync_parked` (+ `sync_parked_missing`), `sync_state.suspect_tip_*`, `sync_backfill_tokens`, and
`trust_decisions`. Everything else the engine holds is either rebuilt from `events` or is
session-scoped (`peers`, `counters`, `logs`). **Rationale:** minimizes the persisted surface (less to
keep consistent), and each of the four maps to a specific AC/security gap in §1.

### D3 — Restore-then-re-derive on `open`; the suspicion re-arms fail-closed

`open` order becomes: (1) re-fold `events` (unchanged); (2) `seed_admin_state` from the held chain
(unchanged); (3) **load** `sync_parked` (re-validate each `wire`; drop+log corrupt rows),
`sync_state` (suspicion + cursor + tokens), and `trust_decisions`; (4) `recompute_completeness`
**with the restored suspicion in hand**, so a persisted `suspect_tip` whose seq still exceeds the
held tip re-raises `AdminViewSuspect` and re-populates `fail_closed_subjects` **before any access
decision is served**. **Rationale:** closes the fail-open-on-restart hole (§1.1) — the headline
security property of this issue.

### D4 — Checkpoint synchronously inside the single-owner pump, transactionally, on mutation

The engine already mutates its state only under the `Node` pump's single-owner discipline
(`node.rs:739`). Add a private `persist_*` call at each mutation site (park insert/evict/wake,
suspicion raise/clear/attempt, token consume/refill, trust-decision record) wrapped in a short
transaction. Batch the per-tick token refill into one write. **Rationale:** the store is synchronous
and single-connection (store §10); the pump is the sole writer, so no locking is added; a crash loses
at most the current in-flight mutation (bounded by one tick). **Alternative considered:** a periodic
full-state flush on `on_tick` only — rejected because a crash between ticks could lose a park that a
peer will not re-push, defeating the durability goal; per-mutation write is cheap at N≤5.

### D5 — The parked `wire` is re-validated on load, never trusted; corruption is a logged drop

`load_parked` runs each row's `wire` back through `validate_wire_bytes` (stateless) before
re-inserting it into the in-memory park. A decode/validation failure means a corrupt or tampered park
row: drop that row (delete it), log a `park_corrupt` reason, continue. **Rationale:** the park holds
*unvalidated-by-membership* frames by design (they are causally incomplete), but they were
stateless-valid when parked; on reload we re-establish that floor and never panic on stored bytes
(store §10.3 no-panic guarantee).

### D6 — `trust_decisions` is append-only and additive to the live in-memory list

On `open`, load persisted rows into `trust_decisions: Vec<TrustDecision>` (`engine.rs:216`); on a new
detection, both push in memory and `append_trust_decision`. The audit trail therefore grows across
restarts and is queryable via the unchanged `trust_decisions()` accessor (`engine.rs:529`).
**Rationale:** a CRITICAL admin-fork alert is security evidence (PRD §13.2/§16.3) that must not be
erased by a reboot; append-only matches the log's ethos and needs no reconciliation.

### D7 — Schema v2 is forward-only and additive; a v1 DB upgrades in place

The migration creates the five new tables `IF NOT EXISTS` and stamps `user_version = 2`; it touches
no `events` row. The existing guard (`schema.rs:80`) that **rejects a *newer* `user_version`** than
the binary supports is retained, so an **old** binary opening a v2 DB fails closed with a typed
`StoreError::Migration` (documented; acceptable for MVP — no downgrade path). **Rationale:** additive
migration is the lowest-risk way to satisfy PRD §12's named tables without disturbing the frozen
`events` schema; the store already models `user_version` bumps (`schema.rs:77`).

### D8 — AC3 "rejected **and logged**" is surfaced through a non-tracing sink

Because the CLI installs no tracing subscriber (memory: *cli-has-no-tracing-subscriber*), the drop of
an invalid frame must be observable without `tracing`. The engine already records drops in its bounded
`logs()` (256-line ring) and `counters().rejected` (`engine.rs:541/535`); ensure every reject path
increments the counter and appends a stable-coded log line (`reject.<code>` using
`RejectReason::code()`), and expose these through the net `AuditSink` on the reject path so a CLI/host
can render "invalid event rejected: <code>" without a subscriber. **Rationale:** makes the AC
*testable and operator-visible* under the real CLI constraint, not merely "logged" into a dropped sink.

### D9 — Restart durability is proven at both layers: SimNet (deterministic) and Node (real QUIC)

Add a `SimNet::restart(peer)` helper (drop the engine, re-`open` over the *same* store) for a fast,
deterministic AC5 proof, **and** extend `manager_e2e` with a real-loopback restart that carries a
non-empty park / raised suspicion. **Rationale:** the deterministic layer proves the *logic* under
shuffle/partition; the Node layer proves the *wiring* (checkpoint fires under the pump, the v2 store
round-trips) — mirroring the prototype's SimNet-primary / adapter-isolated split (D6/OQ-1 there).

---

## 6. The hardening protocol (normative deltas over the landed engine)

Only the deltas are normative; everything else is the landed §6 of `bounded-recent-sync-prototype.md`.

### 6.1 `open` (restart restore) — extends `engine.rs:231`

```
open(store, room_id, config):
  validate config
  validated = re-fold(store.room_tail(room_id, MAX))        # unchanged (events authoritative)
  fold = RoomMembership::from_events(room_id, validated)     # unchanged
  seed_admin_state()                                          # unchanged (held chain)
  # --- NEW: restore transient state ---
  for row in store.load_parked(room_id):
      if validate_wire_bytes(row.wire).is_err(): store.delete_parked(row.id); log("park_corrupt"); continue
      park.insert(row)                                        # respect caps; evict oldest if over (log)
  st = store.load_sync_state(room_id)
  suspect_tip = st.suspect_tip                                # may be None
  tokens = store.load_backfill_tokens(room_id)               # do NOT refill here
  trust_decisions = store.load_trust_decisions(room_id)
  recompute_completeness()   # NOW with suspect_tip in hand -> may re-raise AdminViewSuspect + fail_closed
  return engine
```

**Invariant:** after `open`, if a persisted `suspect_tip.seq` still exceeds the held admin tip,
`completeness() == AdminViewSuspect` and `fail_closed_subjects()` is non-empty **before** any access
decision is served. A restart never transitions `AdminViewSuspect → Complete` without a real backfill.

### 6.2 Checkpoint points (each wrapped in a short transaction, inside the pump)

| Engine mutation | Persist action |
|---|---|
| Park a `Buffered` frame (§6.2) | `upsert_parked(row)` + its `sync_parked_missing` edges |
| Evict oldest on cap overflow | `delete_parked(evicted_id)` (and log `park_evicted`) |
| Wake a parked child when its parent arrives (Accept) | `delete_parked(child_id)` |
| Raise/refresh unconfirmed suspicion | `save_sync_state` (suspect_tip_* + attempts++) |
| Clear suspicion on catch-up | `save_sync_state` (suspect_tip = NULL) |
| Consume a backfill token / refill on tick | `save_backfill_tokens` (batched per tick) |
| Advance the recent-chat cursor | `save_sync_state` (chat_cursor_*) — advisory optimization |
| Record a trust decision (fork/suspect) | `append_trust_decision(row)` |

A checkpoint failure surfaces as `SyncError::Store` at the pump boundary (logged; the pump continues —
a persistence miss degrades durability, never correctness, because `events` remains authoritative and
reconnect re-pulls). Per-frame validation failures remain **logged drops**, never `SyncError`.

### 6.3 Retry after restart

On the first `on_connect`/`on_tick` after `open`, the restored park's `sync_parked_missing` ids drive
`WantEvents` exactly as a freshly-parked frame does (§6.2 backfill), gated by the **restored** token
buckets (not a fresh full budget). So "buffering **and retry**" is durable, and the anti-amplification
budget is not reset by the restart (§1.3).

### 6.4 Everything else is unchanged

Serving pulls (`WantMembership` never windowed; `WantRecentChat` count-bounded, time advisory),
ordering/convergence (inherited from store `room_tail` + order-independent fold), live push/fan-out,
the anti-amplification gate, and the `Completeness` detector logic are **byte-for-byte the landed
behavior**. This issue adds durability around them, not new protocol.

---

## 7. Public API surface

**Unchanged (frozen):** the entire `iroh-rooms-core::sync` public surface (`SyncEngine`,
`SyncConfig`, `SyncMessage`, `Completeness`, `TrustDecision`, `SyncDigest`, `SyncCounters`,
`SyncTransport`, `Outgoing`, `PeerId`, `Window`) and the entire `iroh-rooms-net` surface. Callers
(`Node`, CLI) require **no** signature changes.

**Added (additive, `store` feature):** the `EventStore` sync-cache methods in §4.3
(`load/save_sync_state`, `load/save_backfill_tokens`, `load/upsert/delete_parked`,
`load/append_trust_decision`) plus the `ParkedRow` / `SyncStateRow` / `TrustRow` DTOs. These are the
only new public items. Internally, `SyncEngine` gains private `persist_*`/`restore_*` helpers and the
restore branch in `open`; `SyncError` already carries `Store(StoreError)` for checkpoint failures
(`engine.rs:36`), so no new error variant is required.

**Test-only (additive):** `SimNet::restart(peer)` (drop + re-`open` over the same store).

---

## 8. Test strategy

All deterministic tests run over `SimNet` (no network/async/wall-clock beyond injected `now_ms`); the
Node-layer restart runs over real loopback QUIC (`RelayMode::Disabled`). Reuse the fixture-log
builders from `sync_smoke.rs` / `sync_convergence.rs` (`build_log`, `Principal`, `wire_bytes`) and the
`manager_e2e.rs` `spawn_room_node` harness. **Every new test asserts over a persistent on-disk store**
(`EventStore::open(tempfile)`) so the migration and round-trip are exercised, except the pure-logic
ones that may use `open_in_memory` for speed.

### 8.1 Acceptance-criteria tests (issue)

| AC | Test |
|---|---|
| **AC1 — offline peer reconnects, catches up within configured bounds** | Build a log (genesis → invites/joins → K chat). Disconnect D after event j; drive to K. Reconnect D with `Window{max_count}`. Assert `D.digest().event_ids == full_sub_dag ∪ last_max_count_chat`, `D.snapshot() == online.snapshot()`, and that D pulled **no more than** `max_count` chat events (the "configured bounds" clause) — via `counters()`/`room_tail` length. *(Extends `offline_peer_reconnects_and_converges` with the bound assertion + persistent store.)* |
| **AC2 — membership sub-DAG complete after sync** | Same log, D reconnects with a **tiny** `max_count` (2). Assert D's **authorization-class** id set (`membership_event_ids()`) and `snapshot()` are **exactly** equal to online, while chat differs by the windowed amount. *(Extends `tiny_chat_window_reconciles_membership_but_bounds_chat`.)* |
| **AC3 — invalid events rejected and logged** | (a) SimNet: inject a bad-signature frame and a non-member `message.text` into a peer; assert `ingest_frame` stores nothing (`room_event_ids` unchanged), fans out nothing, and `counters().rejected` + a stable `reject.<code>` line in `logs()` record it. (b) **Over-the-wire (new dimension):** a connected but non-member/removed sender pushes an invalid `Events` frame; assert the receiver drops it, the net `AuditSink` records the reject cause, and no duplicate/rebroadcast occurs. |
| **AC4 — duplicate events ignored** | Replay accepted frames 1×/1000× across a reconnect; assert identical `digest()` and **no** re-broadcast storm (`counters().frames_sent` bounded). *(Extends `idempotency_1000x_does_not_change_state` across a reconnect.)* |
| **AC5 — sync state survives restart (headline)** | Four sub-cases over a persistent store, each: mutate → `restart` → assert restored: **(i) park:** park a child-before-parent frame, restart, assert the park is reloaded and its `WantEvents` re-issued on reconnect, then the parent arrives and both `Accept` in canonical order; **(ii) fail-closed:** raise `AdminViewSuspect` from an advertised higher tip, restart, assert `completeness() == AdminViewSuspect` and `fail_closed_subjects()` non-empty **immediately after `open`, before any new message** (anti fail-open); **(iii) trust audit:** record a CRITICAL admin-fork `equivocation`, restart, assert `trust_decisions()` still contains it; **(iv) rate-limit:** exhaust a backfill token bucket, restart, assert the bucket is **not** refilled by the restart (amplification bound holds). |

### 8.2 Test-plan scenarios (issue "Test Plan")

- **Offline peer + reconnect + event-set equality** — AC1/AC5 above; `assert_converged` oracle.
- **Shuffled delivery** — reuse `shuffled_delivery_converges_across_seeds`, now with a mid-stream
  `restart` on one peer per seed; assert all peers converge to one digest and one `room_tail`.
- **Invalid event injection** — AC3, both SimNet and over-the-wire.
- **Restart durability matrix** — AC5 (i)–(iv).
- **Node-layer real-QUIC restart with in-flight state** — extend `manager_e2e`: a peer with a
  non-empty park (a parent deliberately withheld) is stopped and restarted; assert on reconnect it
  re-issues backfill from the *restored* park and converges to the byte-identical head set, with the
  admin's/others' store counts unchanged (no duplicate application), composing with the landed
  `PeerManager` reconnect.
- **Migration** — open a v1 store (only `events`/`event_parents`), run the v2 migration, assert the
  five tables exist and `user_version == 2` and existing `events` rows are byte-identical; assert an
  *old-binary* (`user_version` guard) rejects a v2 DB with `StoreError::Migration`.

### 8.3 Determinism & derived-cache guards

- **Cache-drop equivalence:** for any converged peer, dropping the five v2 tables and re-`open`ing
  then reconnecting yields the identical steady-state `digest()` — proving they are caches, not
  sources of truth (mirrors the store's rebuild-determinism oracle).
- **Restart determinism:** the same mutate→restart sequence yields byte-identical restored state
  across runs (`BTreeMap`/`BTreeSet` discipline; `park_seq`/`trust.seq` monotone, not clock-derived).
- **Checkpoint idempotency:** persisting the same park/suspicion twice is a no-op (`upsert`/PK).

### 8.4 Coverage guard

Every new test runs under `scripts/verify.sh` (`--all-features`, so `store`+`sync` are exercised;
pedantic-clean). The deterministic SimNet + migration tests are always-CI; the real-QUIC restart test
follows the `manager_e2e` tier (loopback, bounded waits) and stays green in CI or is `#[ignore]`-gated
with a documented command exactly as IR-0109/#24 established.

---

## 9. Error model & observability

- **Per-frame outcomes are logged drops, not engine errors** (PRD §15.5.6): every reject carries its
  stable `RejectReason::code()` / sync drop reason (`anti_amplification_signer`, `park_evicted`,
  `backfill_rate_limited`, `phantom_parent_depth`, and new: `park_corrupt`). All increment
  `counters()` and append a stable line to the bounded `logs()` ring (D8). AC3's "logged" is these,
  surfaced via a non-tracing sink.
- **Checkpoint faults are `SyncError::Store`** at the pump boundary — logged, non-fatal (a persistence
  miss degrades durability, never correctness, because `events` stays authoritative). Never raised for
  a single invalid event.
- **Trust decisions** (`equivocation` CRITICAL on admin fork; `admin_view_suspect`) are first-class,
  now **durable** (`trust_decisions` table), queryable via `trust_decisions()`, and feed the CLI audit
  surface (PRD §13.2, §16.3).
- **Restart-relevant counters** (Gate-D / durability evidence): parked-restored, park-corrupt-dropped,
  suspicion-restored, trust-decisions-restored, tokens-restored — added to `SyncCounters` for the
  restart memo.

---

## 10. Security, privacy, reliability, performance

- **Security — no fail-open on restart (headline).** Persisting the unconfirmed suspicion (D3) and
  re-deriving completeness with it in hand guarantees a reboot cannot clear a fail-closed removal-
  sensitive gate before catch-up. This is the load-bearing safety property of the issue (spec §0/§5/§7
  of the spike, now restart-durable).
- **Security — amplification bounds span restarts.** Persisted token buckets (D2/§6.3) prevent a
  crash-loop from resetting the §4 backfill budget; the parked `wire` is re-validated on load (D5), so
  a tampered park row cannot inject an unvalidated frame.
- **Privacy.** The five new tables hold only signed `WireEvent` bytes and ids already destined for
  room members, plus advisory counters; nothing new leaves the device. `created_at`/`updated_at` stay
  advisory. Local-first, single SQLite file per node (PRD §12) — unchanged.
- **Reliability.** Durable park + retry means a valid-but-early frame survives a crash, not only a
  transport reconnect (strengthens the prototype's A4). WAL + short transactions keep the caches
  crash-consistent; on any inconsistency they are safely droppable (re-derive from `events`).
- **Performance.** N≤5 rooms; per-mutation checkpoints are single-row upserts on indexed BLOB PKs,
  negligible against the 250 ms tick. Park/token/trust volumes are bounded by config. `open` adds one
  bounded scan of each new table (typically empty) to the existing full `events` re-fold — O(park +
  tokens + trust), all bounded. No hot-path regression (the pump already owns the single connection).

---

## 11. Implementation steps

1. **Schema v2 (`store/schema.rs`).** Bump `USER_VERSION` to 2; add the five `CREATE TABLE` (+ index)
   `IF NOT EXISTS` statements (§4.2) to the migration, guarded to run when `user_version < 2`; keep the
   newer-version rejection guard. Add a migration test (v1→v2 additive; `events` byte-stable;
   old-binary rejects v2).
2. **Store cache methods (`store/mod.rs`, `store/model.rs`).** Implement `load/save_sync_state`,
   `load/save_backfill_tokens`, `load/upsert/delete_parked` (+ `sync_parked_missing` edges),
   `load/append_trust_decision`, and the `ParkedRow`/`SyncStateRow`/`TrustRow` DTOs. Cache prepared
   statements; do writes in transactions. Unit-test each round-trip + corrupt-`wire` handling.
3. **Engine restore (`sync/engine.rs::open`).** Add the restore branch (§6.1): load park (re-validate
   each `wire`, drop+log corrupt), load `sync_state` (suspicion/cursor/tokens), load trust decisions;
   call `recompute_completeness` **after** the suspicion is restored. Add the restart-relevant
   counters.
4. **Engine checkpoint hooks (`sync/engine.rs`).** At each mutation site (§6.2) call a private
   `persist_*` on the owned `store` inside a short transaction; batch token refill per tick; surface a
   checkpoint failure as `SyncError::Store` at the entry-point return (logged, non-fatal). Ensure the
   restored park re-issues `WantEvents` on the next `on_connect`/`on_tick` (§6.3).
5. **AC3 observability (`sync/engine.rs` + `net/src/audit.rs`).** Ensure every reject increments
   `counters().rejected` and appends a `reject.<code>` `logs()` line; surface the reject cause through
   the net `AuditSink` on the receive path so it is observable without a tracing subscriber (D8).
6. **SimNet restart helper (`sync/sim.rs`).** Add `restart(peer)` = drop the engine and re-`open` over
   the same store handle.
7. **Tests.** `core/tests/sync_restart.rs` (AC5 matrix, §8.1/§8.2/§8.3, persistent store),
   extend `sync_convergence.rs` (shuffle+restart) and `store` tests (migration). Extend
   `net/tests/manager_e2e.rs` with the real-QUIC restart-with-in-flight-state case (§8.2), following
   the existing tier/`#[ignore]` discipline.
8. **Docs.** README *Current Status* (mark IR-0201 recent-history-sync MVP-complete); a short
   **restart/Gate-D durability memo** (counters evidence); update `crates/iroh-rooms-net/NOTES.md` and
   the prototype spec's OQ-3/D7 to "closed by IR-0201". Do **not** claim Gate A (real-NAT).
9. **`scripts/verify.sh` green across `--all-features`.**

Land steps 1–2 (store, fully unit-tested) before 3–4 (engine wiring) so the schema/round-trip is
reviewed in isolation from the durability semantics.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Derived-cache drift** — a persisted cache silently disagrees with `events`. | D1: `events` stays the only source of truth; the five tables are droppable; §8.3 cache-drop-equivalence test proves re-deriving from `events` converges to the same digest. |
| R2 | **Fail-open still possible** if `recompute_completeness` runs before the suspicion is restored. | D3 fixes the `open` order (restore suspicion *then* recompute); AC5(ii) asserts `AdminViewSuspect` holds **immediately after `open`, before any new message**. |
| R3 | **Tampered / corrupt park row** injects an unvalidated frame on load. | D5: re-`validate_wire_bytes` every parked `wire` on load; corrupt ⇒ delete + `park_corrupt` log; never a panic (store §10.3). |
| R4 | **Crash-loop amplification bypass** by resetting token buckets. | D2/§6.3: persist + restore token buckets; the restart does not refill; AC5(iv) asserts the bound holds. |
| R5 | **Checkpoint I/O on the hot path** regresses tick latency. | D4: single-row upserts on BLOB PKs, batched token refill; N≤5; negligible vs 250 ms tick; checkpoint failure is non-fatal (events authoritative). |
| R6 | **Migration breaks an existing v1 DB.** | D7: additive `IF NOT EXISTS` tables, `events` untouched; explicit v1→v2 migration test asserts byte-stable `events`; old-binary rejects v2 (documented, no downgrade). |
| R7 | **Over-claiming AC5** — "sync state survives restart" read as "everything survives". | Scope it honestly: the *convergent set* survives via `events` (already), the *non-rebuildable* state (park/suspicion/tokens/trust) survives via the v2 tables; session state (peers/counters/logs) is intentionally not persisted. Document the boundary (§4.1). |
| R8 | **Non-determinism** leaking into restored state (map order, clock). | `BTreeMap`/`BTreeSet`, bytewise `EventId`; monotone `park_seq`/`trust.seq` (not clock-derived); §8.3 restart-determinism guard. |
| R9 | **CLI can't observe AC3 rejections** (no tracing subscriber). | D8: surface rejects via `counters()`/`logs()` and the net `AuditSink`, not `tracing`; AC3 test asserts the observable signal. |

---

## 13. Acceptance criteria (issue) → coverage

| Issue acceptance criterion | Where satisfied |
|---|---|
| Peer offline through recent events reconnects and catches up within configured bounds. | Landed handshake/anti-entropy (§6, unchanged) + §8.1 AC1 (with the bound assertion, persistent store). |
| Membership sub-DAG is complete after sync. | Never-windowed `WantMembership` (landed) + §8.1 AC2 (`membership_event_ids` exact equality under a tiny chat window). |
| Invalid events are rejected and logged. | Landed reject path + D8 non-tracing observability + §8.1 AC3 (SimNet **and** over-the-wire injection). |
| Duplicate events are ignored. | Landed G-set dedup + §8.1 AC4 (1000× across a reconnect, no rebroadcast storm). |
| **Sync state survives restart.** | **D1–D7 persistence (v2 tables + restore-on-`open` + checkpoint) + §8.1 AC5 matrix (park, fail-closed, trust audit, rate-limit) + §8.2 real-QUIC restart.** |
| **Test Plan:** offline peer, reconnect, event-set equality, shuffled delivery, invalid injection. | §8.2 scenarios + `assert_converged` oracle + migration + Node-layer restart. |

---

## 14. Open questions

- **OQ-1 — Persist the recent-chat cursor, or recompute it each session?** The `chat_cursor_*` is a
  pure optimization (a reconnecting peer can re-scan `room_tail` cheaply at N≤5). Recommendation:
  include the column but treat it as best-effort (correctness never depends on it); drop it from v1 of
  the migration if it complicates review.
- **OQ-2 — Should a restored `suspect_tip` keep or reset its `attempts` budget?** Keeping it (persist
  the counter) is the stricter anti-fail-open choice and prevents a crash-loop from resetting the
  bound; resetting it is more forgiving of a legitimate reboot mid-catch-up. Recommendation: **keep**
  (persist `suspect_tip_attempts`), matching R4.
- **OQ-3 — Per-mutation checkpoint vs a coalesced write-behind on `on_tick`.** Per-mutation (D4) is
  simplest and crash-tight; a write-behind reduces I/O but widens the crash window. Recommendation:
  per-mutation for the park/suspicion/trust (durability-critical), batched for token refill.
- **OQ-4 — Does the real iroh adapter restart test land here or follow IR-0109's tier discipline?**
  Recommendation: SimNet + migration tests are always-CI here; the real-QUIC restart follows the
  `manager_e2e` tier (loopback, bounded, promotable out of `#[ignore]` later) — no new CI risk.
- **OQ-5 — Persist a bounded slice of `logs()` for a post-restart audit view?** The drop-log is
  session-scoped today. Recommendation: leave `logs()` in-memory (session diagnostics); the durable
  audit surface is `trust_decisions` (security events), which is sufficient for PRD §16.3.

## 15. Assumptions

- **A1** — The transport (ADR-1) and sync-substrate (ADR-2) decisions are settled; this issue is a
  storage/durability/integration slice on the **frozen** engine (#11) and carrier (#9/#22); it
  re-opens neither.
- **A2** — Rooms are ≤5 peers, single device per identity, single immutable admin, no key rotation
  (spike scope), so `admin_seq` is a clean completeness signal and per-mutation checkpoint cost is
  negligible.
- **A3** — The landed `event`/`store`/`membership`/`sync`/`net` public APIs are frozen; this issue
  **adds** store methods + v2 tables and an engine restore/checkpoint path, and changes no existing
  behavior, wire format, or `events` schema.
- **A4** — "Sync state survives restart" means a **process restart** (cold store) preserves the
  non-rebuildable transient state (park, suspicion, tokens, trust audit) via the v2 tables, and the
  convergent set via the authoritative `events` table; session-scoped state (connected peers, live
  counters, drop log) is intentionally not persisted.
- **A5** — Deterministic conformance is proven over `SimNet` + a persistent temp store; real-network
  behavior (hole-punching, relay fallback) remains Gate A (IR-0012), validated separately through the
  isolated iroh adapter, not here.
- **A6** — `EventStore` remains synchronous, single-connection, owned by the `Node` pump's
  single-writer discipline; no store pooling or async is introduced.
