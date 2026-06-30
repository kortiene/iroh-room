# Spec: Bounded Recent Sync Prototype

| | |
|---|---|
| **Issue** | #11 — [IR-0007] Implement bounded recent sync prototype |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/spike, area/protocol, area/transport, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §7.2.10 (basic recent history sync), §10.7 (Sync Limits — bounded by count and/or time window; deep conflict resolution deferred), §15.5 (Sync Recent History journey + acceptance), §8.3 (cut-only-if-necessary), §13 (Security & Privacy) · `PHASE-0-SPIKE.md` ADR-1 (full-mesh QUIC transport), ADR-2 (hand-rolled SQLite log + bounded recent-sync pull), Membership & Ordering §0 (incompleteness detection, admin-tip, fail-closed), §4 (out-of-order delivery, three-stage pipeline, anti-amplification), §5 (fail-closed access), §8 (substrate mapping), §9 (persistence note: `sync_state`, `trust_decisions`) · Spike Plan **Day 6 — Recent-history sync hardening + causal layering (Gate D)** · Protocol Test Vectors §8 (duplicate idempotency), §9 (out-of-order buffer/backfill), §10 (deterministic order), §11/§12 (fork detection), §13 (non-member rejection) |
| **Dependencies** | #6 — IR-0002 canonical signed event model (**landed**: `iroh-rooms-core::event` — `validate_wire_bytes`/`validate_with_membership`, `WireEvent`, `ValidatedEvent`, `RejectReason`, `Flag`, `MembershipOracle`). #8 — IR-0004 SQLite event store (**landed**: `iroh-rooms-core::store` behind the `store` feature — `insert`, `room_tail`, `by_type`, `by_sender`, `heads`, `parents_of`/`children_of`/`missing_parents`, `admin_chain_tip`, `count`, `rebuild`). #12 — IR-0008 membership fold (**landed**: `iroh-rooms-core::membership` — `RoomMembership::{ingest,from_events,snapshot,ancestor_view}`, `Ingest::{Accepted,Rejected,Buffered}`, `MembershipSnapshot`, `pipe_connect_allowed`/`blob_serve_allowed`). |
| **Status** | Implemented — engine + SimNet landed in `iroh-rooms-core::sync` (feature: `sync`, issue #11 / IR-0007); real iroh QUIC adapter (`crates/iroh-rooms-net`, D3/D9) is the deferred follow-on slice (OQ-1). |
| **Type** | Spike. A new **transport-agnostic, deterministic sync engine** (`iroh-rooms-core::sync`, behind a new `sync` feature) over the landed store + fold, plus a deterministic in-memory multi-peer simulation harness that proves Gate D. The real iroh full-mesh QUIC adapter is specified at its trait boundary; its live-network wiring is isolated (see D2/D9) and is **not** on the deterministic conformance path. |

---

## 1. Summary

Prove the **bounded recent-sync path** (ADR-2) works for MVP-sized rooms (≤5 peers, full mesh)
**without** adopting full decentralized history reconciliation (PRD §7.3.14 / §10.7 — explicitly
deferred). This is the last remaining Room Event Plane target ("Full-mesh iroh QUIC event transport
and bounded recent sync", README *Current Status*) and corresponds to **Spike Plan Day 6 / Gate D**.

Everything below the wire is already landed and frozen: the byte-exact signed event model (#6), the
append-only SQLite store (#8), and the deterministic membership fold + ancestor-stable authorization
(#12). What is missing is the layer that **moves the opaque signed `WireEvent` set between peers and
reconciles it** so that an offline peer that reconnects converges to the expected event set, the
membership/admin chain is *always* fully reconciled (never windowed), missing parents are buffered
and backfilled rather than rejected, and set equality can be asserted after sync.

The five things this issue delivers (scope, verbatim from the issue, mapped to mechanisms):

1. **Pull missing events by ID** → the `WantEvents`/`Events` backfill loop driven by
   `RoomMembership::ingest → Ingest::Buffered { missing }` and `EventStore::missing_parents`
   (spike §4 stage 2).
2. **Pull bounded recent chat history** → the `WantRecentChat`/`Events` exchange bounded by **count**
   (trustworthy, via canonical `(lamport, event_id)` order) and optionally **time** (advisory only,
   per spike §2.3) — PRD §10.7.
3. **Always sync the membership sub-DAG + full admin chain without windowing** → the
   `WantMembership`/`Events` exchange that serves the union of all membership-type events and all
   admin-authored events, regardless of any chat window (spike §0, §4, §8 — a hard invariant).
4. **Exchange admin tips and detect suspected incompleteness** → the `AdminTip` advertisement plus
   the **incompleteness detector**: a known-higher admin tip not yet backfilled, or two distinct
   admin tips at the same `admin_seq` (a fork), trips **fail-closed** on removal-sensitive decisions
   and raises a CRITICAL `equivocation` alert on the fork case (spike §0, §7; the Security note).

The design choice that makes the spike provable: the sync engine is a **deterministic, sans-IO
protocol state machine** (it consumes inbound `SyncMessage`s + local state and emits outbound
`SyncMessage`s; it performs no I/O itself). A deterministic **in-memory simulation transport** drives
many peers through shuffled delivery, dropped/delayed frames, partitions, and reconnects, making
"event set equality can be asserted after sync" a precise, repeatable assertion. The real iroh
adapter is a thin pump behind the same trait and inherits Gate-A (Day 1) real-network validation; it
is deliberately kept off the conformance path so a flaky network can never make Gate D
non-deterministic.

The honest convergence claim this issue must respect (spike §0 — the unqualified form is **false**):

> Any two honest peers that end a sync round holding the **identical validated event set** compute a
> byte-identical membership snapshot and timeline. Sync's job is to **equalize the set**; for the
> **never-windowed membership/admin sub-DAG** equalization is unconditional, and for **chat** it is
> bounded to the requested window. Set-completeness in the *concurrent* dimension (a withheld
> removal tip, a segregated admin fork) cannot be self-certified by backfill — it is made
> **detectable** by admin-tip advertisement, on which the node **fails closed**.

---

## 2. Background & current repository state

Read before implementing.

### 2.1 Source-of-truth docs

- **`PHASE-0-SPIKE.md`** is normative. The sections this issue implements:
  - **ADR-1** — transport is **full-mesh direct QUIC over a custom ALPN** (`/iroh-rooms/event/1`).
    Reject unknown `remote_endpoint_id` at `accept()`. The pull RPC carries: *live event push*; a
    *backfill RPC* (by id; the **never-windowed** membership sub-DAG + full admin chain; the recent
    chat window); and **admin-tip exchange**. Gossip is NOT the system of record (optional liveness +
    admin-tip channel only).
  - **ADR-2** — sync is a **bounded recent-sync pull** over the mesh: "give me events after
    watermark / events whose IDs I lack," bounded by count/time for chat, **but the membership
    sub-DAG is never windowed**. Do **not** build Meyer range reconciliation (the deferred
    full-reconciliation primitive).
  - **Membership & Ordering §0** — the honest convergence guarantee + the three incompleteness
    mitigations: derived **`admin_seq` + admin-chain tip**, **admin-tip advertisement**,
    **fail-closed on suspected incompleteness**, and **never-window the membership sub-DAG**.
  - **§4** — the **three-stage ingest pipeline** and its **anti-amplification bounds** (signer
    pre-check, genesis-reachability-before-backfill, per-author parked-set cap + eviction, backfill
    rate-limit/quota, drop structurally-implausible lamport; survivors parked on disk and retried on
    reconnect).
  - **§5** — fail-closed access at connect-accept: enforcement uses the **current snapshot**, default
    deny, and **fails closed on the affected subjects** when the §0 detector trips.
  - **§7** — fork/equivocation: two distinct admin tips at the same `admin_seq` is the detectable
    signature of an admin self-fork even when no single peer initially holds both branches → CRITICAL
    `equivocation` + fail-closed.
  - **§8** — substrate mapping: the pull RPC requests *events by id*, the *never-windowed membership
    sub-DAG + full admin chain*, and the *recent chat window*, plus *admin-tip exchange*.
  - **§9** — persistence: `sync_state` (heads, parked-orphan set with per-author caps, recent-window
    cursor, highest known admin tip) and `trust_decisions` (equivocation alerts, fail-closed
    subjects) are **derived caches rebuildable by re-folding `events`** — restart determinism.
  - **Spike Plan Day 6 / GATE D** — the acceptance frame: *GO iff convergence is deterministic and
    arrival-order-independent under shuffled delivery and a mid-stream reconnect, and the
    anti-amplification bounds hold; NO-GO if ordering/buffering is nondeterministic or the
    parked-set/backfill is unbounded.*
- **Protocol Test Vectors** that become conformance tests here: **§8** (duplicate idempotency),
  **§9** (child-before-parent → buffered then accepted, no permanent divergence), **§10**
  (deterministic `(lamport, event_id)` order under reordering), **§11/§12** (concurrent fork →
  identical membership on same-set peers; fork detection), **§13** (non-member junk dropped early,
  denying backfill amplification).
- **`PRD.v0.3.md`:** §10.7 (bounded by count and/or time window; deep conflict resolution deferred),
  §15.5 (peer requests recent events; existing peer returns within sync bounds; receiver validates
  signature + membership, stores missing valid events, ignores duplicates, rejects+logs invalid),
  §7.3.14 / §8.3 (full reconciliation out of scope / cut-only-if-necessary), §13 (security).

### 2.2 Landed code this layer builds on

**Event core (#6, `crates/iroh-rooms-core/src/event/`, no feature gate):**
- `validate::validate_wire_bytes(bytes: &[u8], ctx: &ValidationContext) -> Result<ValidatedEvent, RejectReason>`
  — the **stateless** pipeline (Event Protocol §6 steps 1–6, 9 structural, 10). The sync engine runs
  this on every fetched/pushed frame **before** persisting or folding.
- `validate::validate_with_membership(...)` (re-exported at `event::validate_with_membership`) —
  completes steps 7–8 via the `MembershipOracle`. The fold already provides the oracle
  (`RoomMembership::ancestor_view`); the engine does not re-implement steps 7–8.
- `wire::WireEvent { v, signed, sig, id }` with `decode(&[u8])`, `to_bytes()`, and verbatim
  `signed` bytes — the on-wire/at-rest representation the sync messages carry.
- `reject::RejectReason` (stable §8 codes via `.code()`) and `reject::Flag`
  (`clock_skew`/`equivocation`/`from_removed_member`) — reused verbatim; **no new taxonomy**.
- `ids::{EventId, RoomId}` (`EventId` `Ord` is bytewise over the raw 32 digest bytes — the protocol
  tie-break), `keys::{IdentityKey, DeviceKey}`, `content::EventType`.

**Store (#8, `crates/iroh-rooms-core/src/store/`, behind `store` feature):**
- `EventStore::{open, open_in_memory, insert(&ValidatedEvent) -> InsertOutcome, insert_all, contains,
  get, count, parents_of, children_of, missing_parents, room_tail(room, limit), by_type(room, ty),
  by_sender(room, sender), heads(room), admin_chain_tip(room) -> Option<(EventId, u64)>, rebuild}`.
- `StoredEvent { event_id, wire, room_id, event_type, lamport, admin_seq }`,
  `InsertOutcome::{Inserted, Duplicate}`, `InsertStats`.
- The store already records **dangling parent edges** and recomputes `lamport`/`admin_seq` when a
  missing parent arrives (out-of-order tolerance); `room_tail` excludes `NULL`-lamport
  (causally-incomplete) events and orders by `(lamport DESC, event_id DESC)` → reversed to ascending
  canonical order — **exactly** the bounded-chat-window order the engine needs.

**Membership fold (#12, `crates/iroh-rooms-core/src/membership/`, no feature gate):**
- `RoomMembership::{new, from_events, ingest(ValidatedEvent) -> Ingest, snapshot() -> MembershipSnapshot,
  ancestor_view(&EventId) -> Option<AncestorView>, advisory_flags(&EventId)}`.
- `Ingest::{Accepted { event_id, flags }, Rejected { event_id, reason }, Buffered { event_id, missing }}`
  — **`Buffered.missing`** is the precise driver of the by-id backfill loop. `ingest` is idempotent
  on a re-seen id (the dedup/replay case) and **cascades** re-classification to descendants when a
  parent arrives (no separate fixpoint needed).
- `MembershipSnapshot` (`PartialEq + Eq`), `Member`, `Role`, `Status`,
  `pipe_connect_allowed(...) -> PipeDecision`, `blob_serve_allowed(...) -> BlobDecision`,
  `DenyReason` — the **current-snapshot** access boundary the §0 detector augments with fail-closed.

### 2.3 Workspace conventions (must follow)

- Edition 2021, `rust-version = 1.80`, `unsafe_code = "forbid"`, clippy `all` + `pedantic` = warn.
- `scripts/verify.sh` runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --all-features -D warnings`, and `cargo test --workspace --all-targets --all-features`. **Because
  CI passes `--all-features`, the new `sync` feature is always fmt/clippy/test-exercised.**
- Module/comment style: dense doc comments that cite the spike section being implemented; typed
  errors (no panics on peer-supplied bytes); deterministic ordering everywhere (`BTreeMap`/`BTreeSet`,
  bytewise `EventId` order).

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A **`sync` engine** (`iroh-rooms-core::sync`, new `sync` cargo feature ⇒ enables `store`) that, over
   an abstract peer link, implements:
   - **Live event push** (fan-out of newly-accepted events to connected peers).
   - **Backfill by id** (`WantEvents`/`Events`) wired to `Ingest::Buffered.missing` /
     `missing_parents`, **with the §4 anti-amplification bounds**.
   - **Never-windowed membership sub-DAG + full admin chain** pull (`WantMembership`/`Events`).
   - **Bounded recent chat** pull (`WantRecentChat`/`Events`) — count-bounded (trustworthy) and
     optionally time-bounded (advisory).
   - **Admin-tip advertisement + incompleteness detector + fail-closed** (`AdminTip`), exposing a
     `Completeness` predicate the access planes consult.
2. The **local ingest integration**: every inbound/fetched frame goes
   `validate_wire_bytes` → fold `ingest` (+ `validate_with_membership`) → on `Accepted`, `store.insert`
   + push to peers; on `Buffered`, anti-amplification-gated backfill; on `Rejected`, drop + log.
3. A **deterministic in-memory simulation harness** (`SimNet`) routing `Outgoing` frames between N
   `SyncEngine`s, with knobs for shuffle, delay, drop, partition, and disconnect/reconnect.
4. A **set-equality / convergence oracle** (`event_id_set`, `SyncDigest`) and the integration tests
   that assert it (the four acceptance criteria + the test-plan scenarios + the security fail-closed
   case).
5. The **iroh full-mesh QUIC adapter trait boundary** (`SyncTransport`/`PeerLink`) plus a documented
   thin adapter design (ALPN, accept-gate, length-prefixed framing). See D2/D9 for why the adapter's
   live-network test is isolated, not on the conformance path.

### 3.2 Out of scope (sibling issues / deferred — do **not** implement here)

- **Full decentralized history reconciliation / Meyer RBSR** (PRD §7.3.14, §10.7; ADR-2 — parked for
  Phase 5 / iroh-docs). This issue builds the *bounded* recent path only.
- **Key rotation / native invite revocation** (PRD §13.4) — the fold already encodes
  sticky-departure; sync does not add a revocation channel.
- **The QUIC accept-handler/transport for the Blob Plane and Live Pipe Plane** (sibling issues). This
  issue defines and exposes the `Completeness` fail-closed hook those planes consult; it does not
  wire pipe/blob sockets.
- **CLI wiring** (`room tail`, connection-state output, `iroh-rooms sync` UX) — sibling CLI issue
  (#34). The engine exposes the queries the CLI will render.
- **Multi-device per identity, multi-admin** (deferred protocol scope).
- **Process-restart durability of the in-flight orphan park** — the spike keeps the orphan park
  in-memory (survives transport reconnect, which the acceptance criteria require) and rebuilds steady
  state from the authoritative `events` table on open (spike §9). On-disk parking via a store schema
  v2 is a documented follow-on (D7, OQ-3).

### 3.3 Why the split is safe

The deterministic engine is a pure function of *(local store/fold state, inbound messages)* and the
landed layers below it are frozen and conformance-tested. Pulling the real iroh adapter out from
under the conformance path means Gate D is proven by repeatable simulation, while the adapter
inherits the orthogonal Gate-A real-network proof. Nothing about the engine's convergence depends on
iroh internals — exactly ADR-1's "ordering/membership semantics here are transport-agnostic and need
only core iroh 1.0."

---

## 4. Domain model

### 4.1 Event classes (the windowing boundary)

The single most important classification in this issue. Every registered `EventType` is exactly one
class:

| Class | Types | Sync rule |
|---|---|---|
| **Authorization (never windowed)** | `room.created`, `member.invited`, `member.joined`, `member.left`, `member.removed`, **plus every admin-authored event** (any event with a defined `admin_seq`) | **Always fully reconciled.** Served in full by `WantMembership`; never dropped by a chat window (spike §0/§4/§8). |
| **Chat (windowed)** | `message.text`, `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status` — **when not admin-authored** | Bounded by count and/or time (PRD §10.7). |

Notes:
- "Full admin chain" = all admin-authored events. By the **self-parent rule** (spike §1) the admin's
  events form a hash chain; `admin_seq` is its derived length. The store exposes the tip
  (`admin_chain_tip`); the whole chain is `by_sender(room, admin_identity)` (admin identity = the
  genesis `room.created` sender). An admin-authored *chat* event is therefore in the **authorization**
  class for sync purposes — withholding any admin event could hide a removal, so none may be windowed.
- The classification is computed locally from `StoredEvent.event_type` + `admin_seq`/genesis sender;
  it is never taken from the peer.

### 4.2 Sync messages (`SyncMessage`)

Length-prefixed deterministic-CBOR frames over a peer link. All ids are raw 32-byte values on the
wire (presentation is hex elsewhere). `room_id` scopes every message.

```text
SyncMessage =
  | AdminTip      { room_id, tip: Option<(EventId, admin_seq: u64)> }      // advertisement (§0)
  | Heads         { room_id, heads: Vec<EventId> }                         // DAG heads (set-diff hint)
  | WantEvents    { room_id, ids: Vec<EventId> }                           // pull by id (§4 backfill)
  | WantMembership{ room_id, have: Vec<EventId> }                          // never-windowed sub-DAG
  | WantRecentChat{ room_id, window: Window, have: Vec<EventId> }          // bounded chat (§10.7)
  | Events        { room_id, frames: Vec<WireBytes> }                      // response: verbatim WireEvent bytes
  | NotFound      { room_id, ids: Vec<EventId> }                           // responder lacks these ids

Window = { max_count: u32, since_ms: Option<u64> }                         // count trustworthy; time advisory
WireBytes = bstr                                                           // == WireEvent.to_bytes()
```

- `have` lists let the responder send only the delta (server-side set difference); it is an
  optimization, never a trust input — the requester re-validates every returned frame.
- `max_count` is the only **trustworthy** bound (canonical order). `since_ms` filters on advisory
  `created_at` and MUST NOT be relied on for completeness or security (spike §2.3 / Event Protocol
  §2). Default `max_count` and the responder's hard cap are §4.4 constants.

### 4.3 Engine outputs (`Outgoing`)

```text
Outgoing = { peer: PeerId, msg: SyncMessage }     // PeerId == remote device_id (= iroh EndpointId)
```

The engine **returns** `Vec<Outgoing>` from every entry point and performs no I/O. The harness/adapter
routes them. This is what makes the engine deterministic and replayable.

### 4.4 Anti-amplification configuration (`SyncConfig`)

Named constants with safe defaults (all tunable; chosen for ≤5-peer MVP rooms):

| Constant | Default | Rationale (spike §4) |
|---|---|---|
| `MAX_PARKED_PER_AUTHOR` | 64 | Cap the orphan park per author key; evict oldest on overflow. |
| `MAX_PARKED_TOTAL` | 1024 | Global park ceiling; protects memory. |
| `MAX_BACKFILL_FANOUT_IDS` | 256 | Max ids in one `WantEvents`; chunk larger needs. |
| `BACKFILL_TOKENS_PER_AUTHOR` | 32 / refill 8 per tick | Token bucket rate-limiting backfill **by requesting author**. |
| `MAX_BACKFILL_DEPTH` | 64 | Max consecutive missing-parent levels chased before a chain is treated as phantom. |
| `RESPONSE_MAX_FRAMES` | 512 | Cap frames per `Events` response; requester re-asks for the rest. |
| `CHAT_WINDOW_DEFAULT` | 200 | Default `Window.max_count` when a peer asks without one. |
| `CHAT_WINDOW_MAX` | 1000 | Responder's hard cap on `max_count` (PRD §10.7 "maximum events per sync"). |

The engine MUST `log()` whenever a bound drops/evicts/caps something — silent truncation reads as
"covered everything" when it did not (a Gate-D NO-GO condition is an *unbounded* park/backfill).

---

## 5. Key design decisions

### D1 — A deterministic, **sans-IO** sync engine in `iroh-rooms-core::sync` behind a `sync` feature

The engine is a pure state machine: `on_connect` / `on_message` / `on_tick` / `publish` each take the
current state + an input and return `Vec<Outgoing>`, mutating only the local store and fold. It does
**no** networking, no async, no clocks beyond an advisory `now_ms` passed in for token-bucket timing
and the (advisory) chat time window. Lives in core (like `store`/`membership`) behind a `sync` feature
that enables `store`. **Rationale:** matches the codebase's pure-core philosophy (event/store/fold are
all deterministic and conformance-tested); makes shuffle/drop/partition/reconnect scenarios
deterministically reproducible; keeps `iroh` and async runtimes out of the conformance path. **CI
`--all-features`** exercises it automatically.

### D2 — Transport is an abstract trait; the real iroh QUIC adapter is a thin, isolated pump

Define a minimal trait the engine is written against:

```rust
pub trait SyncTransport {
    /// Currently-connected authenticated peers (their device_ids).
    fn peers(&self) -> Vec<PeerId>;
    /// Enqueue an outbound frame to a peer (best-effort; ordered per peer link).
    fn send(&mut self, out: Outgoing);
}
```

The deterministic harness implements it in-memory (`SimNet`). The **real adapter** (D9) implements it
over `iroh::protocol::Router` + ALPN `/iroh-rooms/event/1`, rejecting unknown `remote_endpoint_id` at
`accept()` (ADR-1 native admission). **Rationale:** ADR-1's transport-agnostic guarantee; isolate the
churny async iroh stack so a flaky network can never make Gate D non-deterministic. The adapter's
real-network behavior is **Gate A (Day 1)**, not Gate D.

### D3 — `iroh` stays out of `iroh-rooms-core`; the adapter lives in a separate crate

Recommended: implement the real adapter in a **new `crates/iroh-rooms-net` crate** (or, second choice,
behind an `iroh-transport` feature in core). **Rationale:** core has deliberately kept `iroh` out
(only `ed25519-dalek` is pinned to iroh's version so `device_id` stays byte-equal to `EndpointId`);
the `spike-blobs` crate set the precedent of isolating an iroh-dependent spike from the shipping
crates' dep tree. Keeping `iroh` (and tokio/quinn) out of core preserves the lean, deterministic
conformance build. The engine ships and is gated on the sim transport; the adapter is a thin
follow-on slice that cannot regress the engine.

### D4 — Reuse the landed pipeline verbatim; the engine orchestrates, it does not re-decide

Every frame the engine ingests runs the **exact** landed path: `validate_wire_bytes` (stateless) →
`RoomMembership::ingest` (which, with `validate_with_membership` + `ancestor_view`, performs steps
7–8 against the event's own ancestors). The engine never re-implements validation, authorization,
ordering, the fold, or dedup. It adds only: *fetch* (backfill RPC), *anti-amplification gating*,
*persistence of accepted events*, *fan-out*, and the *admin-tip/fail-closed* layer. **Rationale:**
the trust boundary and convergence proof already live below; duplicating them invites divergence.

### D5 — The store holds exactly the **fold-accepted** set; pending orphans live in the engine park

`EventStore.insert` is called **only** for events the fold returns `Accepted`. Consequences: (a)
accepted events are always causally complete, so their `lamport` resolves immediately and `room_tail`
never hides them; (b) the `events` table **is** the convergent validated set, so `event_id_set` is a
clean set-equality oracle and rejected/junk frames never touch disk (Event Protocol §6: rejected ⇒
dropped, never persisted); (c) causally-incomplete-but-plausible frames are held in the engine's
in-memory **orphan park** (and in the fold's buffer) pending backfill, subject to §4.4 caps.
**Rationale:** keeps the authoritative table equal to the convergent set; avoids relying on the
store's dangling-edge tolerance for correctness (it remains a robustness backstop). On restart the
fold is rebuilt from `events` via `RoomMembership::from_events`; lost in-flight orphans are
re-fetched on reconnect.

### D6 — Admin-tip incompleteness detector + fail-closed, exposed as a `Completeness` predicate

Maintain, per room, `highest_known_admin_tip: Option<(EventId, admin_seq)>` (initialized from
`store.admin_chain_tip`, updated on every `AdminTip` received and every local accept). The detector
(spike §0/§7):
- **Behind (catch-up needed):** a peer advertises `admin_seq` > local and the higher tip is not yet
  backfilled ⇒ mark the room **admin-view-suspect**, issue `WantMembership`, and **fail closed on
  removal-sensitive decisions** for affected subjects until the local tip ≥ the advertised tip.
- **Fork (equivocation):** two **distinct** `event_id`s seen at the **same** `admin_seq` ⇒ raise a
  **CRITICAL `equivocation`** trust decision and **fail closed on contested subjects** until
  reconciled. (This is detectable across the room even when no single peer holds both branches.)

Expose:

```rust
pub enum Completeness { Complete, AdminViewSuspect, AdminForkDetected }
impl SyncEngine { pub fn completeness(&self) -> Completeness; }
```

The pipe/blob planes (sibling issues) call `pipe_connect_allowed`/`blob_serve_allowed` against the
**current snapshot** AND, when `completeness() != Complete`, **deny removal-sensitive access** for the
affected subjects (default-deny override). The engine surfaces the predicate + the affected-subject
set; the planes consume it. **Rationale:** the Security note — "fail closed when admin-tip
incompleteness is detected for removal-sensitive decisions" — is the load-bearing safety property of
this issue.

### D7 — `sync_state` / `trust_decisions` are derived caches; spike keeps them in-memory

Per spike §9 these are rebuildable from `events`. For the spike: keep them **in-memory** in the engine
(`highest_known_admin_tip`, orphan park + per-author counters, backfill token buckets, recent-window
cursor, fail-closed subject set, equivocation alerts), rebuilt on `SyncEngine::open` by re-folding
`events` and re-deriving the admin tip. **Rationale:** avoids touching the **frozen** store schema
(`user_version = 1`); satisfies "buffered/backfilled, not rejected" and "reconnect after missed
events" (which need transport-reconnect durability, not process-restart durability). A store **v2**
migration persisting `sync_state`/`trust_decisions` is a documented follow-on (OQ-3).

### D8 — Set-equality oracle: a small additive read-only store helper + an engine digest

Add a **read-only, additive** store method (no schema/`user_version` change):

```rust
impl EventStore { pub fn room_event_ids(&self, room: &RoomId) -> Result<BTreeSet<EventId>, StoreError>; }
```

and an engine-level `SyncDigest { event_ids: BTreeSet<EventId>, admin_tip: Option<(EventId,u64)>,
snapshot: MembershipSnapshot }`. Convergence is asserted as: for the **never-windowed sub-DAG** and
**snapshot**, `digest_a == digest_b` unconditionally; for **chat**, equality holds within matched
window parameters (a peer that requested the last N legitimately holds a subset). **Rationale:**
acceptance criterion 4 ("event set equality can be asserted after sync") needs a precise, honest
oracle that distinguishes the unconditional sub-DAG guarantee from the bounded chat guarantee.

### D9 — Real iroh adapter: ALPN, accept-gate, framing (specified; live test isolated)

`Router::builder(endpoint).accept(b"/iroh-rooms/event/1", handler)`. `ProtocolHandler::accept(conn)`
resolves `conn.remote_endpoint_id()` (the proven `device_id`) → bound `identity` via the
`MembershipSnapshot` device→identity map → **reject if not Active** before any frame is read (ADR-1
admission). Frames are length-prefixed (`u32` BE length + deterministic-CBOR `SyncMessage`) over one
bidi stream per peer; the adapter pumps inbound frames into `SyncEngine::on_message` and writes the
returned `Outgoing`s. The adapter's only test is a **feature-gated, `#[ignore]`-by-default**
two-endpoint loopback smoke test; real-NAT validation is Gate A. **Rationale:** keeps the heavy async
path provable-but-isolated.

---

## 6. The sync protocol (normative)

### 6.1 Local ingest of one frame (`ingest_frame`)

Given raw `WireEvent` bytes from peer `P` (push or backfill response):

1. **Stateless validate.** `validate_wire_bytes(bytes, ctx)`. On `Err(reason)` ⇒ drop, log
   `reason.code()`, return (PRD §15.5.6). On `Ok(ev)` continue.
2. **Fold ingest.** `fold.ingest(ev)` →
   - `Accepted { event_id, flags }` ⇒ `store.insert(&ev)` (idempotent; `Duplicate` is a no-op, §8
     vector), update `highest_known_admin_tip` if admin-authored, **fan out** the frame to all
     connected peers except `P`, wake any parked children that cited this id, and re-run the detector.
   - `Buffered { event_id, missing }` ⇒ run the **anti-amplification gate** (6.2); if it passes, park
     the frame and enqueue a rate-limited `WantEvents { ids: missing }` to `P` (and/or other peers);
     else drop early + log.
   - `Rejected { event_id, reason }` ⇒ drop, log `reason.code()` (e.g. `not_a_member`,
     `insufficient_role`); never persisted or re-broadcast.
3. **Idempotency.** Re-seen ids are no-ops at both the fold (`ingest` returns the existing outcome)
   and the store (`Duplicate`) — 1× or 1000× yields identical state (§8 vector).

### 6.2 Anti-amplification gate (spike §4 stage 2) — before parking/backfilling

A `Buffered` frame is parked + backfilled **only if all** hold; otherwise dropped early and logged:

1. **Signer pre-check.** The frame's `device_id` resolves (via the current snapshot's device→identity
   map, or a still-live invite for that key) to a plausible member/invitee. A frame from a key that
   is not even plausibly in the room is dropped — it never earns a backfill fan-out (§13 vector: a
   non-member's phantom-parent chain is dropped early).
2. **Genesis-reachability via validated ancestors.** Spend backfill only when the event is plausibly
   genesis-descended through already-validated ancestors; do not chase phantom-parent chains deeper
   than `MAX_BACKFILL_DEPTH`.
3. **Per-author park cap.** Parking respects `MAX_PARKED_PER_AUTHOR` and `MAX_PARKED_TOTAL` with
   oldest-first eviction (logged).
4. **Backfill quota.** `WantEvents` is rate-limited per requesting author via the token bucket
   (`BACKFILL_TOKENS_PER_AUTHOR`).
5. **Implausible-lamport drop.** Drop frames whose backfill chain would exceed the depth/size bounds
   (the structural proxy for "implausible derived lamport").

Survivors are parked **in memory** (D7) and **retried on every reconnect/tick** — never silently
discarded (spike §4).

### 6.3 Connect / reconnect handshake (`on_connect`)

On a new authenticated peer link (both directions symmetric):

1. Send `AdminTip { tip: store.admin_chain_tip(room) }` and `Heads { heads: store.heads(room) }`.
2. On receiving the peer's `AdminTip`/`Heads`, run the **detector** (D6) and compute what to pull:
   - `WantMembership { have: <local membership-sub-DAG ids> }` — **always** (never windowed).
   - `WantRecentChat { window: config.default_window, have: <local recent chat ids> }` — bounded.
   - `WantEvents { ids }` for any specific missing parents already known from the park.
3. Drain `Events` responses through `ingest_frame` (6.1); the `Buffered` path (6.2) issues further
   `WantEvents` until the pulled sub-DAG is causally complete (bounded).
4. After draining, re-exchange `AdminTip`; if still behind/forked ⇒ remain fail-closed + alert (D6).

### 6.4 Serving a pull (responder side)

- `WantEvents { ids }` ⇒ `Events { frames: ids.filter_map(store.get).map(wire.to_bytes) }`, capped at
  `RESPONSE_MAX_FRAMES`; ids not held ⇒ `NotFound`.
- `WantMembership { have }` ⇒ compute the **authorization-class** set (4.1: union of `by_type` over
  the five membership types + `by_sender(admin_identity)`), subtract `have`, return as `Events`
  (chunked). **Never** apply a window. This is the §0 hard invariant.
- `WantRecentChat { window, have }` ⇒ take `room_tail(room, min(window.max_count, CHAT_WINDOW_MAX))`,
  keep **chat-class** events, optionally filter `created_at >= since_ms` (advisory), subtract `have`,
  return as `Events` (chunked). Count is the trustworthy bound (canonical order); time is advisory.

### 6.5 Live push (`publish`)

A locally-authored frame (already `validate_wire_bytes`-valid) is ingested via 6.1; on `Accepted` it
fans out to all connected peers as `Events { frames: [bytes] }`. Steady-state propagation is therefore
just 6.1 + fan-out; dedup makes echoes harmless (§8 vector).

### 6.6 Ordering & convergence (inherited, asserted)

Ordering is **not** re-implemented: `store.room_tail` returns ascending `(lamport, event_id)` order;
the fold is order-independent. The engine's contribution is to *equalize the set*; once two peers hold
the same set, identical timeline + snapshot follow by the landed layers (spike §2/§3). The tests
assert this directly (set-equality oracle, D8).

---

## 7. Public API surface (`sync` module)

Indicative signatures (names may be refined in review; behavior is normative).

```rust
// crates/iroh-rooms-core/src/sync/mod.rs   (feature = "sync")

pub use config::SyncConfig;
pub use message::{SyncMessage, Window, Outgoing, PeerId, WireBytes};
pub use engine::{SyncEngine, SyncDigest, Completeness, TrustDecision};
pub use transport::SyncTransport;

pub struct SyncEngine { /* store, RoomMembership, in-memory sync_state, config */ }

impl SyncEngine {
    /// Open over an existing store for one room; rebuilds the fold from `events`
    /// (RoomMembership::from_events) and re-derives the admin tip (spike §9).
    pub fn open(store: EventStore, room_id: RoomId, config: SyncConfig) -> Result<Self, SyncError>;

    /// Ingest one inbound/fetched WireEvent frame (§6.1). Returns frames to send.
    pub fn ingest_frame(&mut self, from: PeerId, bytes: &[u8]) -> Vec<Outgoing>;

    /// A locally-authored, stateless-valid frame to publish (§6.5).
    pub fn publish(&mut self, bytes: &[u8]) -> Result<Vec<Outgoing>, SyncError>;

    /// A peer link came up (§6.3) — emit AdminTip/Heads + pulls.
    pub fn on_connect(&mut self, peer: PeerId) -> Vec<Outgoing>;
    /// A peer link went down — pause fan-out; keep the park for retry on reconnect.
    pub fn on_disconnect(&mut self, peer: PeerId);

    /// Handle one inbound control/data message (§6.4 responder + detector). 
    pub fn on_message(&mut self, from: PeerId, msg: SyncMessage) -> Vec<Outgoing>;

    /// Periodic tick: refill backfill tokens, retry the park, re-advertise tips.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<Outgoing>;

    // -- queries (CLI / planes / tests) --
    pub fn snapshot(&self) -> MembershipSnapshot;
    pub fn completeness(&self) -> Completeness;          // D6 fail-closed hook
    pub fn fail_closed_subjects(&self) -> Vec<IdentityKey>;
    pub fn digest(&self) -> Result<SyncDigest, SyncError>; // D8 set-equality oracle
    pub fn trust_decisions(&self) -> &[TrustDecision];   // equivocation alerts (CRITICAL on admin fork)
}
```

Additive, read-only store helper (no schema / `user_version` change):

```rust
// crates/iroh-rooms-core/src/store/mod.rs   (feature = "store")
impl EventStore {
    pub fn room_event_ids(&self, room: &RoomId) -> Result<std::collections::BTreeSet<EventId>, StoreError>;
}
```

Errors: a new `sync::SyncError` (wrapping `StoreError`, a frame-decode error, and config errors). It
is `#[non_exhaustive]`, `Display` carries a stable lowercase code; per-frame validation failures are
**not** errors of the engine — they are logged drops (PRD §15.5).

---

## 8. Test strategy

All deterministic tests run over `SimNet` (no network, no async, no wall clock except an injected
`now_ms`). The harness exposes: `step()` (deliver one queued frame), `run_to_quiescence()`,
`shuffle(seed)`, `delay`, `drop(p)`, `partition(set_a, set_b)`, `disconnect(peer)`,
`reconnect(peer)`. RNG is seeded and varied per test index (no `Math.random`/wall-clock).

### 8.1 Acceptance-criteria tests (issue)

| AC | Test |
|---|---|
| **AC1 — offline peer reconnects, gets expected recent set** | Build a known log (genesis → invites/joins → K chat events). Disconnect peer D after event j. Drive the room to event K. Reconnect D with `Window{max_count}`. Assert `D.digest().event_ids == expected(full_sub_dag ∪ last_max_count_chat)` and `D.snapshot() == online_peer.snapshot()`. |
| **AC2 — membership/admin fully reconciled even when chat bounded** | Same log, but D reconnects with a **tiny** `max_count` (e.g. 2). Assert the **authorization-class** id sets and `snapshot()` are **exactly equal** to an online peer, while the chat sets differ by exactly the windowed amount (the never-windowed invariant, spike §0/§4). |
| **AC3 — missing parents buffered + backfilled, not rejected** | Deliver a child frame before its parent (§9 vector). Assert `ingest_frame` yields `Buffered` (not `Rejected`), a `WantEvents` for the missing parent is emitted, and after the parent arrives both events are `Accepted` with `(lamport, event_id)` order identical to causal-order delivery. |
| **AC4 — event set equality assertable after sync** | The `SyncDigest`/`room_event_ids` oracle + a reusable `assert_converged(peers, room)` helper used by every multi-peer test (unconditional for sub-DAG+snapshot; window-matched for chat). |

### 8.2 Test-plan scenarios (issue "Test Plan")

- **Shuffled delivery (§10 vector):** N peers, every frame delivered in a seed-shuffled order across
  many seeds; assert all peers converge to one digest and one canonical `room_tail`.
- **Missing parents / out-of-order chains:** deliver a multi-level DAG fully reversed; assert
  buffering + bounded backfill resolves it to the causal-order result.
- **Reconnect after missed events:** the AC1/AC2 disconnect→catch-up path, plus a mid-stream
  reconnect while new events keep arriving.
- **Membership-event backfill:** disconnect a peer across an `member.invited`+`member.joined`+
  `member.removed` sequence; reconnect; assert the full membership sub-DAG reconciles and the
  snapshot converges (e.g. Dave → Removed on every peer).
- **Anti-amplification / phantom-parent DoS (§13 vector):** a non-member floods frames citing
  unknown parents; assert they are dropped at the signer pre-check (not parked, not fanned out), the
  park stays within `MAX_PARKED_*`, and backfill stays within the token budget.
- **Duplicate idempotency (§8 vector):** replay accepted frames 1×/1000×; assert identical state and
  no re-broadcast storm.

### 8.3 Security fail-closed tests (Security note + spike §0/§5/§7)

- **Stale admin tip ⇒ fail closed:** partition so peer X misses `member.removed(D)`; X receives an
  `AdminTip` with higher `admin_seq`; assert `X.completeness() == AdminViewSuspect`, `D ∈
  fail_closed_subjects()`, and a removal-sensitive decision for D is **denied** until X backfills the
  removal; after catch-up `completeness() == Complete` and the decision matches the converged
  snapshot.
- **Admin fork ⇒ CRITICAL + fail closed:** admin equivocates (two events at the same `admin_seq`,
  different `event_id`) delivered to different partitions; on cross-advertisement assert
  `completeness() == AdminForkDetected`, a **CRITICAL** `equivocation` `TrustDecision` naming both
  ids, and fail-closed on contested subjects (spike §7).

### 8.4 Determinism guards

- Same set, many ingest orders ⇒ byte-identical `SyncDigest` (the §0 same-set theorem, end-to-end
  through sync).
- `on_tick` with the same inputs ⇒ same `Outgoing`s (no hidden nondeterminism); `Outgoing` ordering
  is stable (`BTreeMap`/`Vec` discipline).

---

## 9. Error model & observability

- **Per-frame outcomes are logged, not errored** (PRD §15.5.6): every drop carries its stable
  `RejectReason::code()` / a sync drop reason (`anti_amplification_signer`, `park_evicted`,
  `backfill_rate_limited`, `phantom_parent_depth`). Accepts/duplicates/backfill requests are logged at
  debug.
- **Trust decisions** (`equivocation` CRITICAL on admin fork; `admin_view_suspect`) are first-class,
  queryable (`trust_decisions()`), and map to the §8 `equivocation` flag spelling. They feed the CLI
  audit surface (PRD §13.2, §16.3 "distinguish unauthorized peer").
- **Counters** (for the spike memo / Gate D evidence): events pushed/pulled, backfill rounds,
  parked/evicted counts, frames per `Events`, time-to-convergence (in harness steps), and (in the
  adapter) path-type direct-vs-relay (Gate A territory).
- `SyncError` is for engine-level faults (store failure, malformed control frame), never for a single
  invalid event.

---

## 10. Security, privacy, reliability, performance

- **Security — fail closed (the headline).** D6's detector + `Completeness` predicate is the issue's
  load-bearing safety property: a node that *might* be missing a removal denies removal-sensitive
  access until it proves otherwise (spike §0/§5/§7). The never-windowed membership invariant (4.1,
  §6.4) guarantees a catch-up *can* close the gap.
- **Admission.** The real adapter rejects non-member `EndpointId`s at `accept()` (ADR-1); the engine
  additionally drops non-member frames at the signer pre-check (6.2), so the sim path enforces the
  same boundary the network path does.
- **Privacy.** Sync moves only signed `WireEvent` bytes already destined for room members; no new data
  leaves the member set. `created_at`-based time windows are advisory and never gate access.
- **Reliability.** Out-of-order/duplicate/partition tolerance is proven by 8.2/8.3; the park + retry
  ensures no valid event is silently dropped (spike §4); bounded backfill prevents amplification
  collapse.
- **Performance.** O(n²) links at n≤5 is trivial (ADR-1). Backfill is delta-only via `have` sets;
  responses are chunked. `room_tail`/`by_type`/`by_sender`/`admin_chain_tip` are all index-backed
  (store schema indexes). Convergence cost scales with the *delta*, not whole history (the bounded
  point of ADR-2).

---

## 11. Implementation steps

1. **Add the `sync` feature** to `iroh-rooms-core/Cargo.toml` (`sync = ["store"]`); create
   `src/sync/{mod,message,config,engine,transport,sim}.rs`. Re-export the public surface (§7).
2. **`message.rs`** — `SyncMessage`, `Window`, `Outgoing`, `PeerId`, `WireBytes`; deterministic-CBOR
   encode/decode for `SyncMessage` (reuse the event core's CBOR discipline); length-prefixed framing
   helpers. Round-trip tests.
3. **`config.rs`** — `SyncConfig` with the §4.4 constants + defaults; validation.
4. **Additive store helper** — `EventStore::room_event_ids` (read-only; no schema change) + the
   authorization-class query helper (`by_type` union + `by_sender(admin)`); unit tests.
5. **`engine.rs` — local ingest (§6.1) + anti-amplification (§6.2).** `open` (rebuild fold from
   `events`), `ingest_frame`, `publish`, the orphan park (per-author cap + eviction), the
   `RejectReason`/drop logging. Wire `validate_wire_bytes` → `RoomMembership::ingest`/
   `validate_with_membership` → `store.insert` on accept.
6. **`engine.rs` — handshake + serving (§6.3/§6.4) + live push (§6.5).** `on_connect`,
   `on_disconnect`, `on_message` (responder + requester), `on_tick` (token refill, park retry,
   re-advertise), fan-out.
7. **`engine.rs` — admin-tip detector + fail-closed (§D6).** `highest_known_admin_tip`,
   `Completeness`, `fail_closed_subjects`, `trust_decisions` (CRITICAL on fork).
8. **`engine.rs` — convergence oracle (§D8).** `digest() -> SyncDigest`.
9. **`sim.rs` — `SimNet`** harness: route `Outgoing`→`on_message`, with shuffle/delay/drop/partition/
   disconnect/reconnect knobs; `assert_converged` helper.
10. **`tests/sync_convergence.rs`** — the §8 suite (AC1–AC4, the test-plan scenarios, the security
    fail-closed cases, the determinism guards). Reuse fixture-log builders from the membership/store
    e2e tests where possible.
11. **(Follow-on slice, D3/D9) `crates/iroh-rooms-net`** — the real iroh `SyncTransport` adapter
    (ALPN, accept-gate, framing pump) + a feature-gated `#[ignore]` loopback smoke test. Tracked
    separately so it cannot regress the engine; landing it in this PR vs a follow-on is OQ-1.
12. **Docs** — README *Current Status* update (mark IR-0007 path); a short **Gate-D memo** with the
    convergence + anti-amplification evidence (counters from §9).
13. **`scripts/verify.sh`** green across `--all-features`.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Over-claiming convergence** (the unqualified "equivocation can't diverge" — false). | State only the **hedged same-set** guarantee (§1); the detector + fail-closed (D6) handle the concurrent-incompleteness residual; tests assert the hedged form, not the false one. |
| R2 | **Chat windowing accidentally drops a membership/admin event** → a peer silently misses a removal. | The event-class split (4.1) is computed locally; `WantMembership` (6.4) **never** windows; AC2 test asserts exact sub-DAG equality under a tiny chat window. |
| R3 | **Backfill amplification / phantom-parent DoS** by a non-member. | The §6.2 anti-amplification gate (signer pre-check first, depth bound, per-author caps, token bucket); §13/anti-amplification tests; Gate D NO-GO if park/backfill is unbounded. |
| R4 | **Non-determinism** from map iteration / RNG / wall clock leaking into the engine. | `BTreeMap`/`BTreeSet`, bytewise `EventId` order, seeded harness RNG, `now_ms` injected and used only for advisory timing; the §8.4 determinism guards. |
| R5 | **Storing eventually-rejected junk** breaks the set-equality oracle. | D5 — persist only fold-`Accepted` events; pending orphans stay in the engine park, never in `events`. |
| R6 | **iroh churn / async flakiness** contaminating the conformance path. | D2/D3 — engine is sans-IO over a trait; iroh adapter is an isolated crate with an `#[ignore]` live test; Gate D runs entirely on `SimNet`. |
| R7 | **Restart loses in-flight orphans.** | Acceptable for the spike (D7): steady state rebuilds from `events`; orphans re-fetched on reconnect. On-disk park (store v2) is OQ-3. |
| R8 | **Time-window misuse for security** (`created_at` is forgeable, spike §2.3). | `since_ms` is advisory-only; the **count** bound (canonical order) is the trustworthy one; no access/expiry decision consults `created_at` except the single fail-closed pipe-expiry check (already in the fold). |

---

## 13. Acceptance criteria (issue) → coverage

| Issue acceptance criterion | Where satisfied |
|---|---|
| Offline peer reconnects and obtains the expected recent event set. | §6.3 handshake + §8.1 AC1 test. |
| Membership/admin chain is fully reconciled even when chat is bounded. | 4.1 event-class split + §6.4 `WantMembership` (never windowed) + §8.1 AC2 test. |
| Missing parent events are buffered and backfilled, not rejected prematurely. | §6.1 (`Buffered` path) + §6.2 backfill + §8.1 AC3 / §8.2 out-of-order tests. |
| Event set equality can be asserted after sync. | D8 `SyncDigest`/`room_event_ids` + `assert_converged` (§8.1 AC4, used throughout §8). |
| **Security:** fail closed when admin-tip incompleteness is detected for removal-sensitive decisions. | D6 detector + `Completeness`/`fail_closed_subjects` + §8.3 security tests. |
| **Test Plan:** shuffled delivery, missing parents, reconnect after missed events, membership backfill. | §8.2 scenarios (+ §8.3 fork/DoS). |
| **Gate D:** deterministic, arrival-order-independent convergence under shuffle + mid-stream reconnect; anti-amplification bounds hold. | §8.4 determinism guards + §8.2/§8.3; bounds enforced in §6.2 / §4.4. |

---

## 14. Open questions

- **OQ-1 — Does the real iroh adapter (`crates/iroh-rooms-net`, D3/D9) land in this issue or a
  follow-on?** Recommendation: land the **deterministic engine + `SimNet`** here (that is what proves
  Gate D); land the adapter as an immediately-following slice so the conformance path stays iroh-free.
  The issue's acceptance criteria + test plan are fully satisfied by the engine + sim harness.
- **OQ-2 — Should DAG-heads set-difference (`Heads`) be in the MVP handshake, or is admin-tip +
  membership/chat pulls enough?** Recommendation: include `Heads` as a cheap delta hint but keep the
  by-id backfill loop as the correctness path (heads are an optimization, not a trust input).
- **OQ-3 — Persist `sync_state`/`trust_decisions` (store v2) now, or keep them in-memory (D7)?**
  Recommendation: in-memory for the spike (rebuildable per spike §9); schedule the v2 migration with
  the CLI issue when process-restart durability of the park actually matters.
- **OQ-4 — Gossip-carried admin-tip advertisement (ADR-1 optional channel)?** Recommendation: keep
  admin-tip on the mesh pull RPC for the spike; the optional gossip channel is a post-MVP
  liveness/notify optimization off the critical path.
- **OQ-5 — Concrete window defaults (`CHAT_WINDOW_DEFAULT`/`_MAX`).** §4.4 proposes 200/1000; confirm
  against the PRD §10.7 "maximum events per sync" decision once a UX target exists.

## 15. Assumptions

- **A1** — The transport decision (ADR-1 full-mesh QUIC) and the sync-substrate decision (ADR-2
  hand-roll on stable core) are settled; this issue implements ADR-2's bounded path and ADR-1's pull
  RPC shape, and does **not** re-open Day-4/Day-5.
- **A2** — Rooms are ≤5 peers, single device per identity, single immutable admin, no key rotation
  (spike scope) — so the admin chain has one writer and `admin_seq` is a clean completeness signal.
- **A3** — The landed `event`/`store`/`membership` APIs are frozen; this issue only **adds** a
  read-only store helper (D8) and a new `sync` module/feature, touching no existing behavior or the
  store `user_version`.
- **A4** — "Reconnect" in the acceptance criteria means **transport** reconnect (the process stays
  up); process-restart durability of in-flight orphans is explicitly a follow-on (D7/OQ-3). Steady
  state is always rebuildable from the authoritative `events` table.
- **A5** — Deterministic conformance is proven over the in-memory `SimNet`; real-network behavior
  (hole-punching, relay fallback, throughput) is Gate A (Day 1), validated through the isolated iroh
  adapter, not here.
