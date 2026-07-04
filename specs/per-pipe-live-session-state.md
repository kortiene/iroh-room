# Per-pipe live-session state on the owner side (`Node::live_pipe_sessions_for` / `pipe_session_info`)

- **Issue:** #86 — `feat(pipe): expose per-pipe live-session state on the owner side (live_pipe_sessions is node-wide)`
- **Proposed work item:** IR-0309
- **Labels:** `enhancement`, `type/feature`
- **Status:** Landed — implemented per this build plan (issue #86 / IR-0309); see `README.md` and `crates/iroh-rooms/CHANGELOG.md` for the shipped user-facing docs.
- **Owning crates:** `iroh-rooms-net` (`Node` read methods + `PipeSessions` per-pipe accessors + a new `PipeSessionInfo` type), `iroh-rooms` (façade — two small re-export additions)
- **Filed by:** a real SDK consumer — **Bantaba**, a resident daemon + web UI on the developer-preview façade (`--features experimental`, rev `1d2f014`). Its Pipes panel lists every exposed pipe with its live state.

---

## 1. Problem statement

Owner-side live-session observability is **node-wide only**. `Node::live_pipe_sessions()`
returns `self.pipe_sessions.len()` — the total across *all* exposed pipes
(`crates/iroh-rooms-net/src/node.rs:856-861`):

```rust
/// The number of live pipe sessions currently being forwarded (observability /
/// tests for the teardown path).
#[must_use]
pub fn live_pipe_sessions(&self) -> usize {
    self.pipe_sessions.len()
}
```

The pipe plane's read bridge, `PipeQuery` (`crates/iroh-rooms-net/src/pipe/runtime.rs:21-28`),
answers **log-level** state per pipe — the governing `pipe.opened` (`Opened`) and whether a
`pipe.closed` is causally known (`IsClosed`) — but says **nothing about live forwarding
sessions per pipe**. There is no API that maps a `pipe_id` to "how many live sessions is
this specific pipe carrying, and to whom."

### Consumer impact (the honesty gap)

An owner exposing **more than one** pipe cannot truthfully render per-pipe "connected."
With two open pipes and `live_pipe_sessions() == 1`, there is no way to know *which* pipe
carries the session. Bantaba's honesty rule (never fabricate state) forces it to show
`connected` only when it is unambiguous — exactly one open pipe — and `false` otherwise.
That **under-reports real connections**: the moment a resident host exposes a second pipe,
every genuine connection on the first is rendered as `false`.

### Why this is exposure, not new tracking

The internal registry already records everything the panel needs. Each accepted+gated
stream registers a `SessionEntry { device, pipe_id, conn, abort }`
(`crates/iroh-rooms-net/src/pipe/sessions.rs:23-28`), and `PipeSessions::live()` already
projects it to a public `LiveSession { id, device, pipe_id }`
(`sessions.rs:31-39, 97-108`). The per-session `pipe_id` and connecting `device` are **sitting
in the table** — the feature is a read accessor over existing state, not a new subscription,
counter, or engine change.

### Goal

Expose per-pipe live-session state as `&self` reads on `Node`, reachable through the façade
at `iroh_rooms::experimental::session::Node`:

```rust
impl Node {
    /// Count of live forwarding sessions for one exposed pipe (the direct
    /// answer to "is *this* pipe connected, and by how many?").
    pub fn live_pipe_sessions_for(&self, pipe_id: [u8; 16]) -> usize;

    /// Per-session detail across all exposed pipes: which pipe, which peer
    /// device, and (optionally) since when. The Pipes-panel data source.
    pub fn pipe_session_info(&self) -> Vec<PipeSessionInfo>;
}
```

### Non-goals

- **Removing or changing `live_pipe_sessions()`.** The node-wide count stays as-is
  (back-compat; still the right primitive for whole-node teardown tests, e.g.
  `pipe_e2e.rs:289-298`). The new methods are additive.
- **A connector-side (consumer-side) session view.** This issue is owner-side only —
  the party that ran `pipe_expose`. The connector already gets `PipeForwarder` /
  `PipeOutcome` for its own link (`connector.rs`). Out of scope.
- **A push/subscription stream of session open/close events.** The panel polls a
  point-in-time snapshot, exactly like `peer_states()` / `live_pipe_sessions()` do today.
  A `broadcast`-style session-transition feed (à la `room_events`, issue #83) is a
  possible future refinement, noted in §13, not built here.
- **Resolving `device → identity` inside the API.** The table records the QUIC-proven
  `device` (`EndpointId`); mapping it to a human `IdentityKey` is a separate membership
  read the consumer already has (`Node::snapshot()`), and forcing it here would add a
  snapshot query on a pure-read path. Decision §11.4.
- **Exposing the internal teardown key (`LiveSession.id`) or QUIC/abort handles.** Those
  are owner-internal lifecycle handles; the public shape carries identity only. Decision §11.2.
- **Any CLI change.** The CLI does not consume `live_pipe_sessions()` today (grep: no
  reference in `crates/iroh-rooms-cli/src/pipe.rs`); `pipe expose` streams audit lines to
  stderr instead. This spec adds an SDK primitive only. (A `pipe status` CLI surface could
  later build on it — §13.)

---

## 2. Background — why the design is constrained (and simple)

Three facts about this codebase make this a small, low-risk change.

### 2.1 `PipeSessions` is `Arc`-held on `Node` and read `&self` with no pump routing

`Node` holds `pipe_sessions: Arc<PipeSessions>` directly (`node.rs:182`), the *same* `Arc`
it clones into the accept handler (`node.rs:335`) and the teardown watcher (`node.rs:424`).
`live_pipe_sessions()` already reads it on a plain `&self` method with **no** `Cmd`, no
oneshot, no pump hop (`node.rs:859-861`) — because the session table is an independent
`Mutex<HashMap<…>>` (`sessions.rs:43-46`), not part of the single-owner `SyncEngine`.

⇒ The new methods are the *same* shape: `&self` reads that lock the table, project, and
return. **No new `Cmd` variant, no `PipeQueryMsg` variant, no engine involvement, no `async`
required.** This is strictly simpler than the pipe plane's engine-backed reads
(`PipeQuery::pipe_opened`, which must marshal a oneshot to the pump), because live-session
state lives entirely in the table, off the engine.

### 2.2 `LiveSession` already carries per-session `pipe_id` + `device`

`PipeSessions::live()` returns `Vec<LiveSession>` where
`LiveSession { id, device, pipe_id }` (`sessions.rs:31-39, 97-108`). Everything the panel
needs — the governing `pipe_id` and the connecting `device` — is already projected out. The
only field the richer shape adds beyond what `LiveSession` holds is a **connected-at
timestamp** (`since`), which the table does not record today (§4 Step 1 adds it, and §11.3
scopes it as optional).

### 2.3 Teardown is decrement-correct *for free* — there is no separate counter

A per-pipe count derived by **filtering the one `HashMap` at call time** cannot drift from
teardown, because every removal path mutates that same map:

- splice finishes on its own → `deregister(id)` removes it (`sessions.rs:88-93`; scheduled at
  `handler.rs:169-173`),
- watcher revokes on membership/expiry change → `teardown(id)` removes it (`sessions.rs:113-122`;
  `watcher.rs:49`),
- owner `close` / owner exit → `teardown_pipe(pipe_id)` removes every entry for that pipe
  (`sessions.rs:126-136`; `Node::pipe_close` at `node.rs:809`).

So `count_for(pipe_id)` is always exactly `live().filter(pipe_id).len()` at the instant it is
called — the acceptance sketch's "teardown decrements the *right* counter" is satisfied
structurally, with no counter to keep in sync. (This is the same reason `live_pipe_sessions()`
== `len()` is always correct.)

---

## 3. Design overview

Add a thin, purpose-built read layer, bottom-up:

1. **`PipeSessionInfo`** — a new public, `Copy` struct in `sessions.rs`:
   `{ pipe_id: [u8; 16], device: EndpointId, since_ms: u64 }`. This is the consumer-facing
   shape; it deliberately omits `LiveSession`'s internal teardown `id`.
2. **`SessionEntry.since_ms`** — record the connected-at wall-clock ms at `register` time
   (via the module's existing `now_ms()`), so `PipeSessionInfo.since_ms` has a source.
3. **`PipeSessions::count_for(&self, pipe_id) -> usize`** and
   **`PipeSessions::info(&self) -> Vec<PipeSessionInfo>`** — the two accessors, computed under
   the existing `Mutex` (count filters without allocating; `info` projects entries).
4. **`Node::live_pipe_sessions_for(&self, pipe_id) -> usize`** and
   **`Node::pipe_session_info(&self) -> Vec<PipeSessionInfo>`** — the `&self` façade delegates.
5. **Re-exports** — `PipeSessionInfo` from the `iroh-rooms-net` crate root and from the
   façade's `experimental::pipe_runtime` (`Node` is already re-exported via
   `experimental::session`).

Everything below the `Node` methods is a pure, synchronous table read. The only mutation to
existing behavior is recording one extra `u64` per session at `register`.

```
Node::pipe_session_info()  ─┐
Node::live_pipe_sessions_for()─┤ &self, sync
                            ▼
        Arc<PipeSessions>  ── Mutex<HashMap<u64, SessionEntry>>
                            ▲                       │  info()/count_for()
   register(.., since_ms) ──┘   deregister/teardown/teardown_pipe (unchanged removal paths)
```

---

## 4. Detailed implementation steps

### Step 0 — Confirm the `now_ms()` clock-read discipline note

The pipe module's `now_ms()` (`crates/iroh-rooms-net/src/pipe/mod.rs:53-58`) is documented as
"the one place the Pipe plane reads a clock, and only to deny on expiry (fail-closed, §5)."
Recording a session-start timestamp is a **second** legitimate reason to read the clock.
Update that doc comment to say "reads a clock (to deny on expiry, and to stamp a live
session's start)" so the invariant note stays truthful. No behavioral change; documentation
honesty only. (If `since_ms` is dropped per §11.3, skip this step.)

### Step 1 — `SessionEntry.since_ms` + `register` records it (`sessions.rs`)

Add the field and stamp it inside `register` using the module clock, so **no caller signature
changes** and the handler (`handler.rs:165`) is untouched:

```rust
struct SessionEntry {
    device: EndpointId,
    pipe_id: [u8; 16],
    since_ms: u64,          // NEW — connected-at wall-clock ms
    conn: Connection,
    abort: AbortHandle,
}

// in register(...), when building the entry:
SessionEntry {
    device,
    pipe_id,
    since_ms: crate::pipe::now_ms(),   // NEW
    conn,
    abort,
}
```

`register`'s public signature is unchanged (`register(device, pipe_id, conn, abort) -> u64`,
`sessions.rs:63-84`), so the accept handler and every existing test keep compiling. The
timestamp is advisory display data — never used in any gate/teardown decision, so a clock
read that returns `0` (its saturating fallback) is harmless.

### Step 2 — `PipeSessionInfo` public struct (`sessions.rs`)

```rust
/// A live forwarding session as an owner-side consumer sees it (identity only —
/// no internal teardown key, no QUIC/abort handles). The data source for a
/// per-pipe "connected" indicator (issue #86).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeSessionInfo {
    /// The governing pipe this session forwards for.
    pub pipe_id: [u8; 16],
    /// The QUIC-proven device of the connecting peer. Resolve to an
    /// `IdentityKey` via `Node::snapshot()` if a human identity is needed.
    pub device: EndpointId,
    /// Connected-at wall-clock ms (advisory; owner's clock). `0` if the clock
    /// read failed at registration.
    pub since_ms: u64,
}
```

Kept distinct from `LiveSession` on purpose (Decision §11.2): `LiveSession` is the **watcher's**
internal view and carries the teardown `id`; `PipeSessionInfo` is the **consumer's** view and
must not leak that handle.

### Step 3 — `PipeSessions::count_for` + `info` (`sessions.rs`)

```rust
/// Count of live sessions for one pipe (issue #86). Filters the live table —
/// always exactly the sessions currently forwarding for `pipe_id`, so it is
/// decrement-correct with every teardown/deregister path (no separate counter).
#[must_use]
pub fn count_for(&self, pipe_id: &[u8; 16]) -> usize {
    self.sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .filter(|e| &e.pipe_id == pipe_id)
        .count()
}

/// Per-session identity across all pipes (issue #86) — the Pipes-panel source.
#[must_use]
pub fn info(&self) -> Vec<PipeSessionInfo> {
    self.sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .map(|e| PipeSessionInfo {
            pipe_id: e.pipe_id,
            device: e.device,
            since_ms: e.since_ms,
        })
        .collect()
}
```

Both mirror the existing lock/poison-recovery idiom of `len()` / `live()`
(`sessions.rs:97-108, 140-145`). `count_for` counts without allocating; `info` projects.
Iteration order of `HashMap` is unspecified — the consumer sorts for display if it needs a
stable order (noted in the doc + §6.3).

### Step 4 — `Node::live_pipe_sessions_for` + `pipe_session_info` (`node.rs`)

Immediately after `live_pipe_sessions()` (`node.rs:856-861`):

```rust
/// Count of live forwarding sessions for one exposed `pipe_id` (issue #86).
///
/// Unlike [`Node::live_pipe_sessions`] (node-wide across every exposed pipe),
/// this attributes sessions to a single pipe, so an owner exposing more than one
/// pipe can render an accurate per-pipe "connected" indicator. `0` for an
/// unknown / never-connected pipe.
#[must_use]
pub fn live_pipe_sessions_for(&self, pipe_id: [u8; 16]) -> usize {
    self.pipe_sessions.count_for(&pipe_id)
}

/// Per-session detail for every live forwarding session this node owns (issue
/// #86): `(pipe_id, connecting device, since)`. A point-in-time snapshot in
/// unspecified order — sort by `pipe_id`/`since_ms` for display. Resolve
/// `device` to a member identity via [`Node::snapshot`] if needed.
#[must_use]
pub fn pipe_session_info(&self) -> Vec<PipeSessionInfo> {
    self.pipe_sessions.info()
}
```

Add `PipeSessionInfo` to the existing `use crate::pipe::{…}` block (`node.rs:48-51`). This
import resolves through the `pipe` module re-export added in Step 5a below — do that first, or
this `use` will not compile.

### Step 5 — Crate-root + façade re-exports

- **Step 5a — `crates/iroh-rooms-net/src/pipe/mod.rs` (prerequisite).** The `pipe` module
  surfaces session types via an explicit `pub use sessions::{LiveSession, PipeSessions};`
  (`mod.rs:47`). Add the new type: `pub use sessions::{LiveSession, PipeSessionInfo,
  PipeSessions};`. Without this, `crate::pipe::PipeSessionInfo` does not resolve, so **both**
  the `node.rs` import (Step 4) and the crate-root re-export below fail to compile. (`sessions`
  is a `pub mod` at `mod.rs:32`, but the crate consistently names session types through this
  re-export, not `pipe::sessions::…`; follow that convention.)
- **`crates/iroh-rooms-net/src/lib.rs`** — add `PipeSessionInfo` to the `pub use pipe::{…}`
  block (`lib.rs:74-77`), alongside the other pipe types, so it is nameable at
  `iroh_rooms_net::PipeSessionInfo`. (`Node` is already re-exported at `lib.rs:73`; this line
  re-exports the Step 5a name, hence the ordering.)
- **`crates/iroh-rooms/src/experimental/pipe_runtime.rs`** — add `PipeSessionInfo` to the
  `pub use iroh_rooms_net::{…}` block (`pipe_runtime.rs:6-9`), so consumers name it at
  `iroh_rooms::experimental::pipe_runtime::PipeSessionInfo`. `Node` (which carries the two new
  methods) is already re-exported via `experimental::session` (`session.rs:21-26`), so no
  change is needed there beyond an optional mention in its module doc's method list.

### Step 6 — Docs

- Update the `Node::live_pipe_sessions()` doc (`node.rs:856-858`) to cross-reference the new
  per-pipe methods ("for a per-pipe count, see [`Node::live_pipe_sessions_for`]").
- Add `pipe_session_info` / `live_pipe_sessions_for` to the method list in
  `experimental::session`'s module doc (`session.rs:4-9`).
- If a per-pipe "connected" indicator is documented in the dev-preview live-pipe guide
  (`specs/dev-preview-live-pipe-guide.md`), add a one-line note that the SDK now answers it
  per pipe; otherwise leave guide untouched (out of scope).

---

## 5. API / data-model impact

| Surface | Change | Path |
|---|---|---|
| `Node::live_pipe_sessions_for` | **new** `&self, [u8;16] -> usize` | `node.rs` |
| `Node::pipe_session_info` | **new** `&self -> Vec<PipeSessionInfo>` | `node.rs` |
| `Node::live_pipe_sessions` | unchanged (doc cross-ref only) | `node.rs:856` |
| `PipeSessions::count_for` | **new** `&self, &[u8;16] -> usize` | `sessions.rs` |
| `PipeSessions::info` | **new** `&self -> Vec<PipeSessionInfo>` | `sessions.rs` |
| `PipeSessionInfo` | **new** public `Copy` struct | `sessions.rs` |
| `SessionEntry.since_ms` | **new** private field, stamped in `register` | `sessions.rs` |
| `pipe` module re-export | add `PipeSessionInfo` (prereq for the two below) | `pipe/mod.rs:47` |
| `iroh_rooms_net` root re-export | add `PipeSessionInfo` | `lib.rs:74` |
| `experimental::pipe_runtime` re-export | add `PipeSessionInfo` | `pipe_runtime.rs:6` |

No wire-format change, no event-schema change, no `SyncEngine` / pump change, no `Cmd` /
`PipeQueryMsg` variant. `EndpointId` is already a façade boundary type
(`Node::peer_states() -> Vec<(EndpointId, PeerConnState)>`, `node.rs:477`), so
`PipeSessionInfo` introduces no new externally-named dependency type.

---

## 6. Semantics, correctness & observability

### 6.1 Per-pipe attribution (the headline AC)

Two pipes A and B open; a connector attaches to A only. After the accept handler registers
the A session (`handler.rs:165`), the table holds one entry with `pipe_id == A`. Then:

- `live_pipe_sessions_for(A) == 1`, `live_pipe_sessions_for(B) == 0`,
- `live_pipe_sessions() == 1` (node-wide, unchanged),
- `pipe_session_info()` returns one `PipeSessionInfo { pipe_id: A, device: <connector>, .. }`.

Bantaba can now render A `connected` and B `idle` **truthfully**, regardless of how many pipes
are exposed — the honesty gap is closed.

### 6.2 Teardown decrements the right counter

Because both accessors read the live `HashMap` (§2.3), the moment any teardown path removes an
entry, the per-pipe count reflects it:

- owner `close A` → `teardown_pipe(&A)` empties A's entries → `live_pipe_sessions_for(A) == 0`,
  and node-wide drops by the same amount;
- watcher revokes (member removed / pipe expired) → `teardown(id)` → count drops on the next
  read;
- connector disconnects, splice ends → `deregister(id)` → count drops.

There is no independent counter to desync — see §2.3.

### 6.3 Snapshot semantics (point-in-time, unordered)

`pipe_session_info()` is a **point-in-time** read, exactly like `live_pipe_sessions()` /
`peer_states()`. It may change between two calls (a session opens/closes). `HashMap` iteration
order is unspecified, so the returned `Vec` is unordered; the doc directs consumers to sort by
`pipe_id`/`since_ms` for a stable panel. This matches the existing observability contract and
needs no locking guarantees beyond the table's own `Mutex`.

### 6.4 Consistency between the two methods

`live_pipe_sessions_for(p)` and `pipe_session_info().filter(pipe_id == p).count()` are computed
under separate lock acquisitions, so a session could open/close *between* two calls. Each call
is individually consistent (a single lock scope); the pair is not atomic. For a polling UI this
is fine and expected (same as calling `live_pipe_sessions()` then `peer_states()`). Documented,
not fixed — an atomic "all counts + all info in one lock" API is unnecessary complexity (§13).

### 6.5 Observability

These methods *are* the observability surface. No audit-line or `tracing` change is needed —
the existing `PipeAuditSink` accept/teardown lines (`audit.rs`) already narrate transitions;
this adds the *queryable state* the transitions move between. `since_ms` is advisory display
data only and never influences a gate/teardown decision.

---

## 7. Test strategy

All tests are deterministic. Unit tests are pure (no runtime); the e2e reuses the existing
loopback harness.

### 7.1 `PipeSessions` unit tests — `sessions.rs` `#[cfg(test)]`

Extend the existing module (`sessions.rs:154-178`). `register` needs `Connection`/`AbortHandle`
handles; follow whatever the existing session tests use, or gate the richer cases behind the
e2e if a bare `Connection` cannot be constructed in a unit test. At minimum, prove the
pure-logic contracts that don't need live handles:

- **`count_for` on an empty table is 0** for any `pipe_id`.
- **`info` on an empty table is empty.**
- If unit-constructing entries is feasible: **two sessions on distinct `pipe_id`s** →
  `count_for(A) == 1`, `count_for(B) == 1`, `info().len() == 2`, each `PipeSessionInfo`
  carries the right `pipe_id`/`device`; **`teardown_pipe(&A)`** → `count_for(A) == 0`,
  `count_for(B) == 1`, `info().len() == 1`.
- **`since_ms` is populated** (non-panicking; value is whatever `now_ms()` returned).

### 7.2 Headline e2e — `crates/iroh-rooms-net/tests/pipe_e2e.rs`

This is the acceptance oracle. The suite already has the full fixture set (Alice owner, Bob
allowed, echo server, `wait_pipe_opened`, `wait_sessions`, `loopback_addr`) — a new test
**`p7_per_pipe_session_attribution`** (or extend the table):

1. Alice exposes **two** pipes to Bob — pipe A (echo server 1) and pipe B (echo server 2) —
   yielding `pipe_a`, `pipe_b`.
2. Bob `wait_pipe_opened` for both, then `pipe_connect` to **pipe A only** and round-trips a
   byte (to force the session to register).
3. Assert:
   - `alice_node.live_pipe_sessions_for(pipe_a) == 1`,
   - `alice_node.live_pipe_sessions_for(pipe_b) == 0`,
   - `alice_node.live_pipe_sessions() == 1` (node-wide unchanged),
   - `alice_node.pipe_session_info()` has exactly one entry with `pipe_id == pipe_a` and
     `device == bob.endpoint_id()`; `since_ms` is set (non-zero under a real clock, or simply
     "present" to avoid clock flakiness).
   Use a small poll helper analogous to `wait_sessions` to avoid a fixed sleep.
4. **Teardown decrement:** Alice `pipe_close(pipe_a)` (or Bob drops the forwarder); poll until
   `live_pipe_sessions_for(pipe_a) == 0` and `live_pipe_sessions() == 0`, and
   `pipe_session_info()` is empty — while `pipe_b`'s count stayed `0` throughout.

Optionally a second connector on pipe B to assert both counts read `1` independently and total
`2`, strengthening "distinguishes the connected pipe from the idle one."

### 7.3 Façade surface tripwire — `crates/iroh-rooms/tests/experimental_surface.rs`

Add `assert!(!name_of::<pipe_runtime::PipeSessionInfo>().is_empty());` to the offline
path-resolution test, so re-export drift for the new type is a compile/CI error (spec R3
pattern, `experimental_surface.rs:8-16`). The two new `Node` methods are covered by the e2e;
optionally add a trivial offline call `session_node.pipe_session_info()` compiles/returns empty
on a freshly spawned pipe node if a cheap `Node` is already constructed there.

### 7.4 Gate

`scripts/verify.sh` must pass (`cargo test` **plus** `fmt --check` and `clippy -D warnings`
pedantic — memory: *verify.sh is the real CI gate*). Note the new `PipeSessionInfo` fields are
all `pub`, so no `dead_code`/`must_use` clippy friction; the accessors carry `#[must_use]`.

---

## 8. Acceptance criteria

- **AC1 — per-pipe count.** With two pipes exposed and a connector attached to exactly one,
  `live_pipe_sessions_for(connected) == 1` and `live_pipe_sessions_for(idle) == 0`, while
  `live_pipe_sessions()` remains the node-wide total. (§6.1, test §7.2.)
- **AC2 — per-session detail.** `pipe_session_info()` returns one `PipeSessionInfo` per live
  session, each carrying the correct governing `pipe_id` and connecting `device`; a two-pipe
  two-connector case returns two entries attributed to the right pipes. (§6.1, test §7.2.)
- **AC3 — teardown decrements the right counter.** Closing / revoking / disconnecting the
  session for one pipe drives *that* pipe's `live_pipe_sessions_for` to 0 (and the node-wide
  count down by the same amount) without touching the other pipe's count. (§6.2, test §7.2.)
- **AC4 — node-wide method unchanged.** `live_pipe_sessions()` behavior and signature are
  byte-for-byte unchanged; existing tests that use it (`pipe_e2e.rs:289-298`) still pass. (§5.)
- **AC5 — façade reach.** `Node::pipe_session_info` / `live_pipe_sessions_for` are callable
  through `iroh_rooms::experimental::session::Node`, and `PipeSessionInfo` is nameable at
  `iroh_rooms::experimental::pipe_runtime::PipeSessionInfo`; the surface tripwire references it.
  (§5, test §7.3.)
- **AC6 — gate green.** `scripts/verify.sh` passes (fmt + clippy pedantic + tests). (§7.4.)

---

## 9. Risks & mitigations

| # | Risk | Likelihood | Mitigation |
|---|---|---|---|
| R1 | Unit-constructing a `SessionEntry` (needs a real `Connection`/`AbortHandle`) is impractical, so the pure `count_for`/`info` logic can't be unit-tested in isolation | med | Cover the empty-table cases as unit tests; prove the populated/decrement cases in the e2e (§7.2), which has real connections. The logic is a trivial filter/map — e2e coverage is sufficient. |
| R2 | Snapshot read races a concurrent open/close, so a poll sees a transient count | high (by design) | Documented as point-in-time (§6.3); identical to `live_pipe_sessions()`/`peer_states()` today. The teardown-decrement AC uses a poll-until helper, not a single read. |
| R3 | `since_ms` adds a clock read on the register hot path / a departure from the "one clock read" note | low | It's one `now_ms()` per session *open* (not per byte), advisory-only, never gates. Doc note updated (Step 0). Can be dropped entirely (§11.3) if the team prefers a zero-new-field change — the two count/info methods work without it (info would drop `since_ms`). |
| R4 | `HashMap` iteration order surprises a consumer expecting stable order | low | Doc directs sorting for display (§6.3, Step 4 doc). |
| R5 | Re-export drift (new type not reachable via façade) | low | Surface tripwire (§7.3) turns it into a CI compile error. |

---

## 10. Security / privacy / reliability / performance

- **Security / authz.** No new capability. The methods read state the owner already fully
  controls (its own live sessions); they change nothing about who may connect or forward.
  Only the owner process can call them (it's the process holding the `Node`). The proven
  `device` surfaced is the same `EndpointId` the owner already sees in audit lines and
  `peer_states()`.
- **Privacy.** `PipeSessionInfo` exposes only the connecting device and the pipe it uses —
  data the owner is authoritative for. No connector-side or third-party data. `device` is not
  resolved to an identity here (§11.4), so no incidental membership disclosure beyond what the
  owner already holds.
- **Reliability.** Pure `&self` reads under the existing `Mutex`, with the established
  poison-recovery idiom (`unwrap_or_else(PoisonError::into_inner)`). No new task, channel,
  lock, or failure mode. Cannot deadlock (single lock, no nested acquisition, no await under
  lock).
- **Performance.** `count_for` is an O(n) filter, `info` an O(n) map, over the live-session
  table — n is the number of *concurrent live sessions* on one node (small; bounded by
  connected peers × pipes). Off every hot path (called by a polling UI, not per forwarded
  byte). `live_pipe_sessions()` is already O(1) `len()` and stays that way.
- **Migration / rollback.** Purely additive API; nothing to migrate. Rollback = delete the
  new methods/type/field; no persisted state, no wire change.

---

## 11. Key decisions

1. **Two methods, not one.** `live_pipe_sessions_for` (the minimal count that directly answers
   the acceptance sketch) *and* `pipe_session_info` (the richer per-session view Bantaba's panel
   actually renders). The issue floats both shapes as non-binding; shipping both costs almost
   nothing (both are one-line delegations) and serves both the count-only and detail consumers.
2. **New `PipeSessionInfo`, not reuse `LiveSession`.** `LiveSession` is the watcher's internal
   projection and carries the teardown `id` (an internal lifecycle handle). Exposing it across
   the façade would leak that handle and couple the public API to watcher internals. A distinct
   consumer-facing struct keeps `LiveSession` free to change and the public shape identity-only.
3. **`&self` synchronous reads, no pump routing.** The session table is off the single-owner
   engine (§2.1), so — like `live_pipe_sessions()` — these need no `Cmd`/`PipeQueryMsg`/oneshot.
   This is deliberately *not* modeled on `PipeQuery::pipe_opened` (which must hit the engine).
4. **Surface `device`, not `identity`.** The table records the QUIC-proven `EndpointId`;
   resolving it to an `IdentityKey` needs a snapshot lookup the consumer can already do via
   `Node::snapshot()`. Keeping the API a pure table read (no engine query) matches the
   "exposure of existing state" framing and avoids a snapshot hop on a read path.
5. **Keep `live_pipe_sessions()` as-is.** It is the right primitive for whole-node teardown
   tests and cheap `len()`; the new methods are additive, with a doc cross-reference.

---

## 12. Assumptions

1. **The registry already holds per-session `pipe_id` + `device`** — verified:
   `SessionEntry { device, pipe_id, .. }` (`sessions.rs:23-28`), `LiveSession` projects both
   (`sessions.rs:97-108`). The feature is exposure, not new tracking (modulo `since_ms`).
2. **`Node` holds `Arc<PipeSessions>` directly and reads it `&self` off the pump** — verified:
   `node.rs:182`, `live_pipe_sessions()` at `node.rs:859-861`.
3. **`EndpointId` is already an accepted façade boundary type** — verified: `Node::peer_states`
   / `PeerEntry` / `ConnEvent` carry it and are re-exported through `experimental`.
4. **The consumer polls** (Bantaba's panel refreshes), so a point-in-time snapshot API — not a
   subscription — meets the need. Consistent with `live_pipe_sessions()` today.
5. **Loopback e2e harness suffices** — `pipe_e2e.rs` already spawns multi-node loopback
   sessions with pipes, so two-pipe attribution is exercisable there without new infra.

---

## 13. Open questions

1. **Include `since_ms`?** The acceptance sketch requires only per-pipe count + correct
   teardown; `since` is "or richer" and non-binding. Recommendation: **include it** — it is
   one advisory `u64` the panel wants ("connected since …") and costs one clock read at
   register. If the team prefers the absolute-minimum change, drop the `SessionEntry` field and
   Step 0/1, and `PipeSessionInfo` becomes `{ pipe_id, device }`. **Decision needed before
   implementation.**
2. **Also re-export `LiveSession` / `PipeSessions` through the façade?** Not required by this
   issue (the `Node` methods return `PipeSessionInfo`), and doing so would widen the
   experimental surface. Recommendation: **no** — keep them net-internal; revisit only if a
   consumer needs the watcher-level view.
3. **A `pipe status` CLI surface?** Out of scope here, but `pipe_session_info()` is the natural
   backing for a future `pipe status` / `pipe list --sessions` command. Track as a follow-up if
   product wants operator-facing per-pipe session visibility.
4. **A session-transition subscription (open/close feed)?** A `broadcast`-style stream (à la
   `room_events`, issue #83) would let a UI react without polling. Deferred as a refinement;
   the polled snapshot meets the filed need. Track separately if the UI needs push semantics.
