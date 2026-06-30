# Spec: SQLite Event Store Prototype

| | |
|---|---|
| **Issue** | #8 — [IR-0004] Build SQLite event store prototype |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/feature, area/protocol, area/storage, priority/p0, risk/medium |
| **Traceability** | `PRD.v0.3.md` §12 Local Storage · `PHASE-0-SPIKE.md` Event Protocol §6 step 11 (dedup & persist), Persistence note §9, ADR-2, Membership & Ordering §0/§2/§3.4 |
| **Dependencies** | #6 — IR-0002 canonical signed event model (**landed**: `iroh-rooms-core::event`, `validate_wire_bytes` → `ValidatedEvent`) |
| **Status** | Implemented — landed in `iroh-rooms-core::store` (feature: `store`, issue #8 / IR-0004) |
| **Type** | Production code: a new SQLite persistence layer for the `events` table + derived indexes + rebuild, plus tests. No membership fold, no sync/transport, no CLI wiring. |

---

## 1. Summary

Persist validated events locally in **SQLite**, keeping the append-only signed log as the
**single source of truth** (PRD §12, ADR-2). This issue builds the `events` table and the
derived indexes the rest of the Room Event Plane queries, with three load-bearing guarantees:

1. **Idempotent G-set persistence.** A validated event is stored **exactly once**, keyed by its
   `event_id`; a duplicate insert is **ignored without error** (Event Protocol §6 step 11 —
   `duplicate` is not a rejection; "first validly-signed copy wins; 1× or 1000× yields identical
   state").
2. **Verbatim byte preservation.** The exact `WireEvent` bytes are stored unchanged, so the store
   can re-broadcast and re-verify byte-for-byte (no re-serialization on the trust boundary).
3. **Derived-cache discipline.** Every column other than the raw bytes — `sender_id`, `device_id`,
   `event_type`, parent edges, derived `lamport`, derived `admin_seq` — and every secondary index
   is a **derived cache rebuildable from the stored events** (Persistence note §9). Rebuild
   determinism is what guarantees restart determinism for the membership fold and ordering.

The store sits **downstream of validation**: its input is a `ValidatedEvent` produced by the
already-landed `validate_wire_bytes` pipeline (#6). It does **not** re-decide validity, membership,
authorization, ordering, or sync. It provides the **query surface** those sibling layers consume
(room tail, parent lookup both directions, by-type / by-sender scans, DAG heads, admin-chain tip)
and the **rebuild** operation that re-folds the log.

---

## 2. Background & current repository state

Read before implementing:

- **`PRD.v0.3.md` §12 Local Storage** — SQLite for MVP; required tables include `events`,
  `members`, `sync_state`, `trust_decisions` (this issue builds **`events`** + its indexes only;
  the others are sibling-issue derived caches). Storage principles: local-first, **append-only
  events**, no central message/file DB, exportable events, user-controlled local deletion.
- **`PHASE-0-SPIKE.md`:**
  - **ADR-2** — "hand-roll a signed append-only log in SQLite"; SQLite is the single source of
    truth; ingest = verify signature → check membership → **dedup by `event_id`** → persist → fan
    out. Do **not** adopt iroh-docs / redb for MVP.
  - **Event Protocol §6 step 11** — "Dedup & persist. If `event_id` already stored, ignore
    (`duplicate`, not an error). Otherwise persist the verbatim `WireEvent`."
  - **Persistence note §9** — the normative shape of this issue: *"The SQLite `events` table stores
    raw signed bytes keyed by `event_id`, with indexed derived `lamport`, `prev_events`,
    `sender_id`, `event_type`, and (for admin events) derived `admin_seq`. `members`, `sync_state`
    … and `trust_decisions` … are derived caches rebuildable by re-folding `events` — guaranteeing
    restart determinism. The append-only log is the single source of truth."*
  - **Membership & Ordering §2 / §2.1** — `lamport` is **derived, not on the wire**
    (`lamport(genesis)=0`, `lamport(e)=1+max(lamport(p) for p in prev_events)`); total order is
    ascending **`(lamport, event_id)`**, `event_id` compared **bytewise over its 32 raw digest
    bytes**.
  - **Membership & Ordering §0** — derived **`admin_seq`** = length of the admin self-parent chain
    ending at an admin event; the **admin-chain tip** is the highest-`admin_seq` admin event.
  - **§3.4 the membership fold** — collects member-touching events and finds **causal heads**; this
    is the consumer of `by_type` / `by_sender` / `heads`.
  - **§4 out-of-order delivery** — events whose parents are missing are **buffered/backfilled, not
    rejected** (a sibling concern); the store must therefore tolerate **dangling parent
    references** without error.
- **Landed code (dependency #6), `crates/iroh-rooms-core/src/event/`:**
  - `validate::ValidatedEvent { event_id: EventId, event: SignedEvent, wire: WireEvent,
    flags: Vec<Flag> }` and `validate::validate_wire_bytes(bytes, ctx) -> Result<ValidatedEvent,
    RejectReason>` — **the store's input type**.
  - `wire::WireEvent { v, signed, sig, id }` with `to_bytes()` / `decode(bytes)` and
    `seal(signed, sig)`; `signed` is the CSB verbatim.
  - `signed::SignedEvent` (the eight fields: `schema_version, room_id, sender_id, device_id,
    event_type, created_at, prev_events, content`) with `decode(signed)` /
    `from_canonical_value`; `signed::event_id_from_bytes(&wire.signed) -> EventId`.
  - `ids::{EventId, RoomId, HashRef}` — newtypes over `[u8; 32]` with `as_bytes()` (raw on-wire
    form), `from_bytes`, `to_named_string()` (`blake3:<hex>`), `FromStr`. **`Ord` is bytewise over
    the raw 32 bytes** (derived), which is exactly the protocol's `event_id` tie-break order.
  - `content::{EventType, Content}` — closed event-type registry; `EventType::as_str()`.
  - `keys::{IdentityKey, DeviceKey}` — raw 32-byte Ed25519 public keys with `as_bytes()`.
- **Workspace facts:**
  - `crates/iroh-rooms-core/Cargo.toml` currently depends only on `ed25519-dalek`, `blake3`, `hex`.
    `lib.rs` already states the crate "owns the Room Event Plane, **persistence interfaces**, and
    shared domain types."
  - Strict lints: root `Cargo.toml` sets `unsafe_code = "forbid"`, Clippy `all` + `pedantic` at
    `warn`; `scripts/verify.sh` runs `cargo fmt --all --check`, `cargo clippy --workspace
    --all-targets --all-features -D warnings`, `cargo test --workspace --all-targets
    **--all-features**`. New code must be pedantic-clean; **`--all-features` means any new cargo
    feature is exercised by CI.**
  - Existing error enums are **hand-rolled** (`Display` + `std::error::Error`), no `thiserror`.
  - Heavier/experimental work is sometimes isolated into its own crate (`crates/spike-blobs`), but
    that one is explicitly throwaway; the store is a load-bearing MVP component.

**There is no existing store, `rusqlite`, or persistence code in the workspace.** This issue
introduces the first persistence engine.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A `rusqlite`-backed **`EventStore`** with `open(path)` / `open_in_memory()` and an idempotent
   schema migration (`CREATE TABLE IF NOT EXISTS`, `PRAGMA user_version` = 1).
2. The **`events`** table: authoritative `(event_id, wire)` columns + derived columns
   (`room_id, sender_id, device_id, event_type, created_at, lamport, admin_seq`).
3. The **`event_parents`** edge table modelling `prev_events` for parent/child lookup (both
   directions), tolerating **dangling** parent references (out-of-order arrival).
4. Secondary **indexes** supporting: room tail / total order, parent and reverse-parent lookup,
   by-type and by-sender scans (membership fold), by-device, and admin-chain tip.
5. **Idempotent insert** of a `ValidatedEvent`: stored exactly once by `event_id`; duplicate
   ignored, returning a typed `Inserted | Duplicate` outcome (never an error).
6. Derived **`lamport`** and **`admin_seq`** computation (genesis `lamport = 0`; `1 + max(parent
   lamports)` when causally complete; `NULL` while parents are missing).
7. A **query API** the sibling fold/sync/CLI layers consume (see §7).
8. A **`rebuild()`** operation that clears all derived state and recomputes it purely from the
   authoritative `(event_id, wire)` rows — the testable proof of "derived caches can be rebuilt
   from stored events / restart determinism."
9. A typed **`StoreError`** model and **no-panic** guarantee on any stored bytes.
10. Unit + integration tests mapping 1:1 to the issue Acceptance Criteria and Test Plan.

### 3.2 Out of scope (sibling issues under epic #1 — do **not** implement here)

- **Membership fold / authorization** (`members` table, §3.4–§3.8) — this issue provides the
  *queries* the fold runs (`by_type`, `by_sender`, `heads`), not the fold.
- **Sync / transport** and the `sync_state` cache (heads, parked-orphan set with per-author caps,
  recent-window cursor, highest known admin tip), backfill, anti-amplification (§4), admin-tip
  *advertisement* (§0).
- **Out-of-order buffering / orphan parking on disk** (§4). The store *persists* events and *records
  dangling parent edges* so buffering is possible later, but the buffering policy, per-author caps,
  and retry loop are the sync issue's.
- **`trust_decisions`** (equivocation alerts, fail-closed subjects).
- **Re-validation / signature re-verification on ingest.** The trust boundary is
  `validate_wire_bytes` (#6); the store assumes pre-validated input (see D5).
- **CLI wiring** (`iroh-rooms room tail`, etc.), exports, blobs/files/pipes/agents tables,
  GC/pinning, deletion UX. The CLI consumes this surface in a later issue.
- **Migration story across schema versions, fuzzing, production hardening** (spike non-goals).

### 3.3 Why the split is safe

Per ADR-2 and Persistence note §9, the `events` log is the single source of truth and every other
table is a pure function of it. Building `events` + its derived indexes + a deterministic `rebuild`
first gives the membership/sync layers a frozen, conformance-tested substrate they extend without
re-touching the byte storage or the dedup guarantee.

---

## 4. Key design decisions

### D1 — Where the store lives: a feature-gated `store` module in `iroh-rooms-core` (recommended)

Add the store as `crates/iroh-rooms-core/src/store/` behind a non-default cargo feature `store`
that enables the `rusqlite` dependency. Rationale:

- `lib.rs` already declares the core crate owns "persistence interfaces"; the store is tightly
  coupled to `ValidatedEvent` / `WireEvent` / `SignedEvent` and is **load-bearing MVP**, unlike the
  throwaway `spike-blobs` crate.
- A feature flag isolates the heavier `rusqlite` (vendored C SQLite) dependency so validate-only
  consumers keep a lean dep tree; **`scripts/verify.sh` runs `--all-features`, so CI still
  exercises the store** for fmt/clippy/test.
- Downstream crates opt in via `iroh-rooms-core = { features = ["store"] }`.

*Alternative considered:* a separate `crates/iroh-rooms-store` crate depending on `iroh-rooms-core`
(mirrors the `spike-blobs` isolation and keeps the core crate's dependency set pristine). Acceptable
and arguably cleaner layering; rejected as primary because the core crate explicitly claims
persistence and the extra crate boundary adds re-export churn for a prototype. **Surface this to the
maintainer (Open Q1); the schema and API below are identical either way.**

### D2 — SQLite crate: `rusqlite` with the `bundled` feature

Use `rusqlite` (synchronous; the core crate has no async runtime) with `features = ["bundled"]` so
SQLite is compiled from vendored source — a **hermetic, version-pinned** build with no system
`libsqlite3` dependency (reproducible CI). `rusqlite`'s FFI lives in `libsqlite3-sys`, a separate
crate, so the workspace `unsafe_code = "forbid"` lint (which applies to *our* code) is unaffected.
Pin a current `0.x` line and record it. *Build cost:* `bundled` compiles C once; CI runners have a C
toolchain. (Open Q6 if build time is a concern → consider the system-`libsqlite3` feature instead.)

### D3 — `event_id` stored as a raw 32-byte BLOB, not the `blake3:<hex>` text

Key `events.event_id` as **`BLOB` (32 raw digest bytes)**, the on-wire form (`EventId::as_bytes()`).
Rationale:

- **SQLite BLOB comparison is `memcmp`**, which is **exactly** the protocol's total-order tie-break
  ("`event_id` compared bytewise over its 32 raw digest bytes", §2.1). `ORDER BY lamport, event_id`
  therefore yields the canonical order with zero application-side sorting.
- Compact (32 B vs 71-char text) and joins cleanly with `event_parents` (raw-byte edges).
- The `blake3:<hex>` named form is **presentation only**; convert at the API/CLI boundary via
  `EventId::to_named_string()` / `FromStr`. (Text-hex would also preserve order, but BLOB matches
  the wire form and the §2.1 wording exactly.)

All key/id columns (`room_id`, `sender_id`, `device_id`, parent ids) follow the same raw-BLOB
convention.

### D4 — Authoritative vs derived columns, and `rebuild()` operates on the authoritative projection

`events.event_id` and `events.wire` are **authoritative**; *all* other `events` columns and the
*entire* `event_parents` table are a **derived cache**. `rebuild()` deletes the derived state and
recomputes it solely from the `(event_id, wire)` projection, re-decoding each `wire` with the landed
`WireEvent::decode` + `SignedEvent` decode. This makes the issue's "derived caches can be rebuilt
from stored events" criterion a **single, directly testable operation**: import only
`(event_id, wire)` pairs into a fresh database, run `rebuild()`, and assert the derived state is
byte-identical to the original (§8 rebuild test). Storing `room_id`, `sender_id`, etc. as columns is
purely a denormalized cache for query speed; it never adds information not present in `wire`.

### D5 — The store trusts pre-validated input; it does not re-run validation

Input is a `ValidatedEvent` (already past `validate_wire_bytes`). `insert` recomputes only the cheap
`event_id = BLAKE3(wire.signed)` to key the row and assert it matches `ValidatedEvent::event_id`
(integrity guard), but does **not** re-verify signatures or membership. `rebuild()` uses a
**structural** decode (shape/type only) to extract derived fields from trusted stored bytes; a
decode failure means **corruption**, surfaced as a typed `StoreError`, never a panic. An optional
strict `rebuild_verifying(ctx)` that re-runs full `validate_wire_bytes` is noted as a future
hardening hook (Open Q3), out of prototype scope.

### D6 — `prev_events` modelled as a separate edge table; parent refs may dangle

Model `prev_events` as `event_parents(child_id, parent_id, ordinal)` rather than a blob column, so
both **forward** (`parents_of`) and **reverse** (`children_of`) lookups are indexed — the reverse
direction is what lets the fold/sync re-process buffered children when a parent arrives. **Only
`child_id` carries a foreign key to `events`** (the child exists at insert time); **`parent_id` is
deliberately unconstrained** because out-of-order delivery (§4) means a parent may not be stored yet.
A dangling `parent_id` is normal, not an error; it is what `lamport = NULL` and future backfill keys
off of.

---

## 5. Schema (SQLite DDL, `user_version = 1`)

```sql
PRAGMA journal_mode = WAL;        -- multi-reader + single-writer for MVP
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

-- The append-only signed log. event_id + wire are AUTHORITATIVE (source of truth);
-- every other column is a DERIVED CACHE recomputable from `wire` (see D4).
CREATE TABLE IF NOT EXISTS events (
    event_id    BLOB    NOT NULL PRIMARY KEY,   -- raw 32-byte BLAKE3 digest (dedup key)
    wire        BLOB    NOT NULL,               -- verbatim WireEvent bytes (§6 step 11)
    -- ---- derived cache below this line ----
    room_id     BLOB    NOT NULL,               -- 32 bytes
    sender_id   BLOB    NOT NULL,               -- 32 bytes (identity / sender_id)
    device_id   BLOB    NOT NULL,               -- 32 bytes (signing device)
    event_type  TEXT    NOT NULL,               -- registry string, e.g. 'message.text'
    created_at  INTEGER NOT NULL,               -- ms epoch; ADVISORY/display only (never ordering)
    lamport     INTEGER,                        -- derived; NULL while causally incomplete
    admin_seq   INTEGER                         -- derived; non-NULL only for admin self-chain events
) STRICT;

-- prev_events DAG edges. child_id FK-constrained; parent_id intentionally NOT (may dangle, §4 / D6).
CREATE TABLE IF NOT EXISTS event_parents (
    child_id    BLOB    NOT NULL,
    parent_id   BLOB    NOT NULL,
    ordinal     INTEGER NOT NULL,               -- position within prev_events (preserves order)
    PRIMARY KEY (child_id, ordinal),
    FOREIGN KEY (child_id) REFERENCES events(event_id) ON DELETE CASCADE
) STRICT;

-- Room tail + canonical total order: (lamport, event_id) ascending. event_id BLOB compare == the
-- §2.1 bytewise tie-break, so this index *is* the canonical order with no app-side sort.
CREATE INDEX IF NOT EXISTS idx_events_room_order   ON events(room_id, lamport, event_id);
-- Membership fold inputs.
CREATE INDEX IF NOT EXISTS idx_events_room_type    ON events(room_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_room_sender  ON events(room_id, sender_id);
CREATE INDEX IF NOT EXISTS idx_events_room_device  ON events(room_id, device_id);
-- Reverse parent lookup (find children of a just-arrived parent).
CREATE INDEX IF NOT EXISTS idx_parents_parent      ON event_parents(parent_id);
-- Admin-chain tip (highest admin_seq); partial index keeps it small.
CREATE INDEX IF NOT EXISTS idx_events_admin_seq    ON events(room_id, admin_seq)
    WHERE admin_seq IS NOT NULL;
-- Advisory display-by-time (NOT used for ordering/security).
CREATE INDEX IF NOT EXISTS idx_events_room_created ON events(room_id, created_at);
```

Notes:
- `STRICT` tables enforce column typing (SQLite ≥ 3.37, satisfied by `bundled`).
- The forward `parents_of` lookup uses the `event_parents` primary key (`child_id, ordinal`); the
  `ordinal` column preserves the signed `prev_events` order for faithful reconstruction.
- `created_at` is indexed only for optional human display; **no query may order or authorize on it**
  (§2.3 / §6 step 10 — advisory only).

---

## 6. Data model (`ValidatedEvent` → columns)

| Column | Source (from `ValidatedEvent`) | Derivation |
|---|---|---|
| `event_id` | `event_id.as_bytes()` (assert `== BLAKE3(wire.signed)`) | authoritative key |
| `wire` | `wire.to_bytes()` | authoritative bytes (verbatim envelope) |
| `room_id` | `event.room_id.as_bytes()` | derived |
| `sender_id` | `event.sender_id.as_bytes()` | derived |
| `device_id` | `event.device_id.as_bytes()` | derived |
| `event_type` | `event.event_type.as_str()` | derived |
| `created_at` | `event.created_at` | derived (advisory) |
| `lamport` | computed | genesis (`room.created`, `prev_events == []`) ⇒ `0`; else `1 + max(parent lamports)` **iff every parent is present with a known lamport**, else `NULL` |
| `admin_seq` | computed | non-NULL **only** when `sender_id == room admin` (genesis creator identity, MVP single immutable admin): genesis ⇒ `0`; each subsequent admin-authored event along its self-parent chain ⇒ `prev admin_seq + 1`; needs the room's genesis present, else `NULL` (deferred until resolvable) |
| `event_parents` rows | `event.prev_events[i].as_bytes()` | one row per parent, `ordinal = i` |

**`lamport` / `admin_seq` computation policy (prototype):**
- **At insert:** compute eagerly when resolvable from already-stored rows (the common in-order
  case); otherwise store `NULL`. Inserting an event whose `event_id` is some stored event's missing
  parent SHOULD trigger recomputation of the now-resolvable descendants (a bounded forward pass over
  `children_of`), or callers may invoke `rebuild()`.
- **At rebuild:** recompute all `lamport`/`admin_seq` via a topological pass (Kahn over
  `event_parents`); events still missing a parent remain `NULL`. This is the authoritative,
  order-independent computation and the determinism oracle.
- `admin` identity resolution: read the genesis `room.created` for the `room_id` (its `sender_id`,
  with `admins == [sender_id]` in MVP). If genesis is absent (out-of-order), `admin_seq` stays
  `NULL` until it arrives. Full admin-tip advertisement semantics are a sibling concern (§0); this
  issue only persists the derived column + index.

---

## 7. Public API surface (`store` module)

Synchronous, hand-rolled error type, no panics on stored bytes. Sketch (names indicative):

```rust
pub struct EventStore { /* wraps rusqlite::Connection */ }

pub enum InsertOutcome { Inserted, Duplicate }   // never an Err for a known-duplicate

pub struct StoredEvent {                          // returned by reads
    pub event_id: EventId,
    pub wire: WireEvent,                          // verbatim, re-decoded from stored bytes
    pub room_id: RoomId,
    pub event_type: EventType,
    pub lamport: Option<u64>,
    pub admin_seq: Option<u64>,
    // sender_id / device_id / created_at available via wire/event as needed
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Self, StoreError>;
    pub fn open_in_memory() -> Result<Self, StoreError>;   // tests
    fn migrate(&self) -> Result<(), StoreError>;           // CREATE TABLE IF NOT EXISTS + user_version

    // ---- write path ----
    /// Idempotent persist of a pre-validated event (Event Protocol §6 step 11).
    /// Returns `Duplicate` (no-op) if `event_id` is already stored — never an error.
    pub fn insert(&mut self, ev: &ValidatedEvent) -> Result<InsertOutcome, StoreError>;
    /// Bulk insert in one transaction; returns counts {inserted, duplicate}.
    pub fn insert_all(&mut self, evs: &[ValidatedEvent]) -> Result<InsertStats, StoreError>;

    // ---- point/existence ----
    pub fn contains(&self, id: &EventId) -> Result<bool, StoreError>;
    pub fn get(&self, id: &EventId) -> Result<Option<StoredEvent>, StoreError>;
    pub fn count(&self, room: &RoomId) -> Result<u64, StoreError>;

    // ---- parent lookup (both directions) ----
    pub fn parents_of(&self, id: &EventId) -> Result<Vec<EventId>, StoreError>;   // ordered by ordinal
    pub fn children_of(&self, id: &EventId) -> Result<Vec<EventId>, StoreError>;  // reverse edge
    pub fn missing_parents(&self, id: &EventId) -> Result<Vec<EventId>, StoreError>; // dangling refs

    // ---- room tail / canonical order ----
    /// Most-recent `limit` causally-placed events in canonical (lamport, event_id) order.
    /// Events with NULL lamport (not yet causally complete) are excluded (§2.3).
    pub fn room_tail(&self, room: &RoomId, limit: u32) -> Result<Vec<StoredEvent>, StoreError>;

    // ---- membership-fold inputs ----
    pub fn by_type(&self, room: &RoomId, ty: EventType) -> Result<Vec<StoredEvent>, StoreError>;
    pub fn by_sender(&self, room: &RoomId, sender: &IdentityKey) -> Result<Vec<StoredEvent>, StoreError>;
    /// DAG heads in a room: events that are not the parent of any stored event (§3.4 causal heads).
    pub fn heads(&self, room: &RoomId) -> Result<Vec<EventId>, StoreError>;

    // ---- admin tip ----
    pub fn admin_chain_tip(&self, room: &RoomId) -> Result<Option<(EventId, u64)>, StoreError>;

    // ---- derived-cache maintenance ----
    /// Clear ALL derived state and recompute it from the authoritative (event_id, wire) rows.
    pub fn rebuild(&mut self) -> Result<(), StoreError>;
}
```

Guidance: cache prepared statements; do writes (and bulk reads where helpful) inside transactions;
`room_tail` is `... WHERE room_id = ? AND lamport IS NOT NULL ORDER BY lamport DESC, event_id DESC
LIMIT ?` then reversed to ascending for display, or an equivalent subquery.

---

## 8. Test strategy

All tests run under `scripts/verify.sh` (fmt + clippy `-D warnings` pedantic + test, `--all-features`
so the `store` feature is exercised). Build a fixture DAG by reusing the **`tests/e2e_lifecycle.rs`**
helper pattern (`SigningKey::from_seed`, `genesis()`, `seal()`) to produce real `ValidatedEvent`s:
genesis `room.created` → `member.invited` → `member.joined` → `message.text` chain, plus a
deliberate concurrent fork (two siblings sharing one `prev_events`) for tie-break and head tests.
Use `open_in_memory()` for unit/integration tests.

Maps 1:1 to the issue Acceptance Criteria and Test Plan:

1. **Persist exactly once by `event_id`** (AC1) — insert a validated event ⇒ `Inserted`; `count == 1`;
   `get(id)` returns a `StoredEvent` whose `wire.to_bytes()` **byte-equals** the input wire (verbatim
   preservation); the integrity assert `BLAKE3(wire.signed) == event_id` holds.
2. **Duplicate insert ignored without error** (AC2) — insert the same `ValidatedEvent` again ⇒
   `Ok(Duplicate)`, no `Err`; `count` unchanged; stored bytes unchanged (first copy wins, G-set).
   Insert 1× vs 1000× ⇒ identical final state (row count, bytes, derived columns).
3. **Query by room / type / sender** (Test Plan) — `by_type(room, MemberJoined)` returns exactly the
   join(s); `by_sender(room, alice)` returns Alice's events; results scoped to `room_id` (a second
   room's events never leak).
4. **Parent lookup** (AC3) — `parents_of(child)` returns the genesis id in `prev_events` order;
   `children_of(genesis)` returns the child (reverse edge); inserting a child **before** its parent
   stores a **dangling** edge (`missing_parents` non-empty, no error), and after the parent arrives
   `children_of(parent)` resolves it.
5. **Room tail / canonical order** (AC3) — `room_tail` returns events in ascending
   `(lamport, event_id)`; for the concurrent fork (equal `lamport`), the **lower raw-byte
   `event_id` sorts first** (assert the exact tie-break, mirroring spike Test Vector §10);
   `NULL`-lamport (causally incomplete) events are excluded from the tail.
6. **Membership-fold support** (AC3) — `heads(room)` returns the current DAG head(s); after appending
   a child, the head moves to the child; `by_type` over `member.*` returns the fold's input set.
7. **Rebuild from stored events** (AC4) — copy only the authoritative `(event_id, wire)` rows into a
   fresh `open_in_memory()` store, run `rebuild()`, and assert the **entire derived state is
   byte-identical** to the original: every derived column (`room_id, sender_id, device_id,
   event_type, created_at, lamport, admin_seq`), every `event_parents` row, and the results of
   `room_tail` / `by_type` / `heads` / `admin_chain_tip`. Insert the same set in **shuffled order**
   and assert `rebuild()` produces the identical derived state (order-independence / restart
   determinism).
8. **Derived `lamport` / `admin_seq`** — genesis `lamport == 0`; a 3-deep chain yields `0,1,2`;
   along the admin self-parent chain `admin_seq` increments `0,1,2…`; a non-admin author's events
   have `admin_seq == NULL`; an event with a missing parent has `lamport == NULL` until the parent
   is inserted/rebuilt.
9. **No panic on corrupt bytes** — store a row with deliberately truncated `wire`, run `rebuild()`
   ⇒ a typed `StoreError` (decode/integrity), never a panic, OOB, or unbounded allocation.

---

## 9. Error model & observability

- `StoreError` (hand-rolled, `Display` + `std::error::Error`, matching the crate's existing style):
  `Sqlite(rusqlite::Error)`, `Decode(RejectReason)` (a stored `wire` failed to decode during
  rebuild — corruption), `Integrity { … }` (recomputed `event_id` ≠ stored key, or a derived value
  disagrees), `Migration(String)`.
- **`duplicate` is success, not error** — surfaced as `InsertOutcome::Duplicate` so callers can
  count idempotent drops (debug-log; this is the metric the ingest path reports).
- **No panics on stored bytes** (mirrors event-core §10.3): rebuild decodes adversarial/corrupt
  bytes through the landed strict reader and maps failures to `StoreError`; no `unwrap`/`expect`/
  slicing on untrusted lengths in non-test code; `#![forbid(unsafe_code)]` already workspace-wide.
- Insert returns enough to log `{event_id, room_id, event_type, outcome}` for the local audit trail
  (PRD §16); flags are **not** persisted (advisory, recomputable — Open Q7).

---

## 10. Security, privacy, reliability, performance

- **Trust boundary already crossed.** The store persists only `ValidatedEvent`s; it never elevates
  trust. The cheap `event_id` re-derivation on insert guards against a caller passing mismatched
  `event_id`/`wire`.
- **Local-first / no central DB** (PRD §12) — a single local SQLite file per node; nothing leaves the
  device. WAL gives crash-consistency; the append-only log + deterministic `rebuild` give restart
  determinism (Persistence note §9).
- **Append-only.** No `UPDATE`/`DELETE` on the authoritative `(event_id, wire)` in normal operation
  (user-controlled deletion is a separate, later concern). Derived columns are rewritten only by
  `rebuild` / incremental recompute.
- **DoS / unbounded input.** `MAX_PREV_EVENTS = 20` already bounds edge fan-in per event (enforced by
  validation upstream); `room_tail`/scans take explicit `limit`s; rebuild is O(events + edges).
  Disk-park / anti-amplification for orphan events is the sync issue's (§4), not here.
- **Performance (prototype).** Single connection, prepared-statement cache, transactional bulk
  insert, WAL, the canonical-order composite index. Sufficient for MVP room sizes; multi-connection
  pooling / `Mutex<Connection>` sharing is noted, not built (Open Q8 covers multi-room layout).
- **Privacy.** `created_at` is wall-clock and attacker-chosen (§6 step 10) — stored for display,
  never trusted; no other PII beyond what the signed event already carries.

---

## 11. Implementation steps

1. **Dependency + feature (D1/D2).** Add `rusqlite = { version = "0.3x", features = ["bundled"] }`
   under a new `store` cargo feature in `crates/iroh-rooms-core/Cargo.toml`; gate the module with
   `#[cfg(feature = "store")]`. Confirm `scripts/verify.sh` (`--all-features`) builds + lints clean.
2. **Module scaffold.** `src/store/mod.rs` (public surface + docs linking PRD §12 / Persistence
   note §9), `schema.rs` (DDL constants + `migrate` + `user_version`), `error.rs` (`StoreError`),
   `model.rs` (`StoredEvent`, `InsertOutcome`, `InsertStats`).
3. **Open + migrate.** `EventStore::open` / `open_in_memory`; set pragmas; idempotent `migrate`.
4. **Insert path.** Decode-free column extraction from `ValidatedEvent`; `INSERT … ON CONFLICT
   DO NOTHING` for `events`; insert `event_parents` rows (`OR IGNORE`); compute `lamport`/`admin_seq`
   eagerly when resolvable; wrap in a transaction; return `Inserted | Duplicate` via `changes()`.
   Incremental forward recompute of now-resolvable descendants on parent arrival.
5. **Read/query API (§7).** Implement point lookups, `parents_of`/`children_of`/`missing_parents`,
   `room_tail`, `by_type`, `by_sender`, `heads`, `admin_chain_tip`; cache prepared statements.
6. **`rebuild()` (D4).** Clear derived columns + `event_parents`; scan `(event_id, wire)`; re-decode
   via `WireEvent::decode` + `SignedEvent` decode; repopulate columns + edges; topological
   (`lamport`, `admin_seq`) pass; integrity-assert recomputed `event_id == key`.
7. **Tests (§8).** Unit tests in-module (`#[cfg(test)]`); an integration test file
   `tests/event_store.rs` guarded with `#![cfg(feature = "store")]` reusing the e2e fixture builders.
8. **Docs.** Module-level rustdoc; update `README.md` "Remaining Room Event Plane targets" to mark
   the SQLite event store as landed when merged (doc-only, not in this spec change).

---

## 12. Risks & mitigations

- **R1 — `rusqlite` bundled build cost / C toolchain (LOW–MED).** Vendored SQLite compiles C in CI.
  *Mitigation:* `bundled` is hermetic and the GitHub runners have `cc`; if build time bites, switch to
  system `libsqlite3` (Open Q6). Feature-gating keeps it off the validate-only path.
- **R2 — `lamport`/`admin_seq` for causally-incomplete events (MED).** Missing parents ⇒ undefined
  derived order. *Mitigation:* `NULL` until resolvable; **no FK on `parent_id`** so dangling edges
  don't error; `rebuild` recomputes deterministically; `room_tail` excludes `NULL`-lamport events.
- **R3 — Derived/authoritative discipline drift (MED).** A query silently depending on a non-
  rebuildable value would break restart determinism. *Mitigation:* D4 + the rebuild byte-identity
  test (§8.7) is the guard; only `(event_id, wire)` is authoritative.
- **R4 — Tie-break ordering correctness (MED, convergence-critical).** `(lamport, event_id)` must be
  bytewise over raw digest bytes. *Mitigation:* `event_id` as `BLOB` so SQLite `memcmp` == §2.1; an
  explicit concurrent-fork tie-break test (§8.5) pins it.
- **R5 — Admin identity resolution (MED).** `admin_seq` needs the genesis admin; out-of-order genesis
  ⇒ unknown. *Mitigation:* `NULL` until genesis present; resolved on rebuild; full admin-tip
  semantics deferred to the sync issue.
- **R6 — Re-validation expectations (LOW).** Consumers might assume the store re-verifies signatures.
  *Mitigation:* document D5 clearly; offer the optional `rebuild_verifying` hook as future work.
- **R7 — Out-of-order / orphan policy creep (MED).** Buffering, per-author caps, backfill are §4
  sibling scope. *Mitigation:* the store *persists + records dangling edges* only; explicitly out of
  scope here (§3.2).

---

## 13. Acceptance criteria (issue) → coverage

- [x] **Valid event persists exactly once by `event_id`** — PK on `events.event_id`; §8.1.
- [x] **Duplicate insert is ignored without error** — `ON CONFLICT DO NOTHING` ⇒
  `InsertOutcome::Duplicate`; §8.2.
- [x] **Derived indexes support room tail, parent lookup, and membership fold** —
  `idx_events_room_order` (`room_tail`), `event_parents` + `idx_parents_parent`
  (`parents_of`/`children_of`), `idx_events_room_type`/`_sender` + `heads` (fold); §8.3–§8.6.
- [x] **Derived caches can be rebuilt from stored events** — `rebuild()` over the authoritative
  `(event_id, wire)` projection, byte-identical + order-independent; §8.7.

**Test Plan (issue):** insert, duplicate insert, query by room/type/sender, parent lookup, and
rebuild — all covered in §8.

---

## 14. Open questions

1. **Store location (D1):** feature-gated `store` module in `iroh-rooms-core` (recommended) vs a
   separate `crates/iroh-rooms-store` crate? Maintainer call; schema/API identical either way.
2. **`event_id` key form (D3):** raw `BLOB(32)` (recommended, matches §2.1 memcmp tie-break) vs
   `blake3:<hex>` `TEXT` (more human-readable in `sqlite3` shell)?
3. **Rebuild strictness (D5):** structural decode only (recommended for the prototype) vs an optional
   `rebuild_verifying(ctx)` that re-runs full `validate_wire_bytes` (needs room context)?
4. **`lamport`/`admin_seq` timing:** eager-on-insert-when-resolvable + rebuild fixpoint
   (recommended) vs compute-only-on-rebuild vs lazy-on-read?
5. **Orphan handling boundary:** confirm the store only *persists + records dangling edges*, leaving
   buffering / parked-set caps / backfill entirely to the sync issue (§4) — recommended.
6. **`rusqlite` version + features:** pin which `0.x`; `bundled` (recommended, hermetic) vs system
   `libsqlite3` if CI build time matters.
7. **Persist advisory `flags`?** `clock_skew` etc. are advisory and recomputable — recommend **not**
   storing them (out of scope); confirm.
8. **Multi-room layout:** single DB file with a `room_id` column (recommended; a node joins several
   rooms) vs one DB file per room?

## 15. Assumptions

- The store's input is a `ValidatedEvent` from the **landed** `validate_wire_bytes` (#6); validation
  is the trust boundary and is not repeated here.
- MVP has a **single immutable admin == the genesis `room.created` creator identity**; `admin_seq`
  derives from that self-parent chain. No key rotation, single device per identity.
- `lamport` and `admin_seq` are **derived** quantities (never on the wire); the golden CSB stays
  `map(8)` and no test vector changes from this issue.
- Synchronous `rusqlite` is acceptable — the core crate has no async runtime; the ingest path is not
  yet on a hot async loop.
- CI runners provide a C toolchain for `rusqlite` `bundled`.
- `members`, `sync_state`, `trust_decisions` (PRD §12) and the membership fold, ordering, sync, and
  CLI are **sibling issues**; this issue ships only the `events` table + derived indexes + parent
  edges + `rebuild`, plus the query surface those siblings consume.
```

