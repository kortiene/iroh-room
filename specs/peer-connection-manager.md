# IR-0107 — Peer Connection Manager

- **Issue:** #22 ([IR-0107] Implement peer connection manager)
- **Parent:** #2
- **Labels:** type/feature, area/transport, area/cli, priority/p0, risk/high
- **Depends on:** #9 (IR-0005 full-mesh QUIC transport, landed), #11 (IR-0007 bounded recent-sync engine, landed)
- **Traceability:** `PRD.v0.3.md` §16 (CLI requirements, esp. §16.3), §18.1 (P2P reliability risk); `PHASE-0-SPIKE.md` ADR-1 *Consequences* ("build a per-room peer manager…"), Spike Plan Day 4.
- **Owning crates:** `crates/iroh-rooms-net` (the manager + connection-state model), `crates/iroh-rooms-cli` (connection-state output).
- **Status:** planning / spec only. **Do not implement from this document without a follow-up build task.**

---

## 1. Summary

The Room Event Plane already has a working carrier (`iroh-rooms-net`, IR-0005) and a
sans-IO sync engine (`iroh-rooms-core::sync`, IR-0007). What it does **not** have is a
single component that *owns the set of connections a room should maintain* and keeps
that set correct as the room's membership changes. Today the pieces are diffuse:

- `peer::dial_loop` maintains **one** outbound link with bounded backoff, but is spawned
  ad-hoc by `NetTransport::connect_to` and never stopped except on transport `Drop`.
- The CLI (`message::build_dial_set`) computes the dial set **once** at command start from
  a frozen membership snapshot and never re-derives it, even though `room tail` is a
  long-running session in which membership can change (a join lands, a member is removed).
- `admission::AllowlistAdmission` is a **frozen** snapshot bound into `Shared` at
  `NetTransport::bind` time; a member removed mid-session is still admitted.
- `PeerConnState` collapses *dial-failed*, *link-dropped*, and *unreachable* all into a
  single `Offline`, so the CLI cannot honour PRD §16.3's requirement to distinguish a
  reachable-but-failing transport from a peer that simply has no path.
- No CLI command or log line surfaces per-peer connection state to a human.

IR-0107 introduces an explicit, **room-scoped `PeerManager`** in `iroh-rooms-net` that:

1. Derives the **desired connection set** from the live membership snapshot (active
   members' devices minus self) and **reconciles** the running dial-loop set against it —
   starting loops for newly-active members, aborting and tearing down loops for
   since-removed members.
2. Backs admission with the **live** membership snapshot so a since-removed device's
   inbound connection starts being rejected the moment the fold learns of the removal.
3. Enriches the connection-state model with an additive **offline reason** so the CLI can
   render *offline (unreachable)* vs *offline (transport error)* vs *unauthorized* — the
   PRD §16.3 / §18.1 distinction — without disturbing the load-bearing `PeerConnState`
   trichotomy that the rest of the codebase pins.
4. **Surfaces** that state to the operator: stable, greppable per-transition log lines
   plus a CLI connection-state view.
5. **Guarantees and tests** that a peer stop → restart cycle re-converges without
   duplicate event delivery, leaning on the landed engine dedup (G-set by `event_id`).

This is primarily an **integration + lifecycle** issue: it consolidates landed primitives
into a managed, roster-reactive whole and closes the three follow-ups the IR-0005 NOTES
explicitly deferred ("re-point `Admission` to the live `MembershipSnapshot`",
"production hardening (N6)", roster-driven dial reconciliation). It introduces little new
*correctness* logic; the trust boundary (validation, membership fold, dedup, admission
shape) is unchanged.

---

## 2. Background: what already exists (do not rebuild)

| Concern | Landed in | Location | Reused by IR-0107 |
|---|---|---|---|
| Per-peer dial-with-backoff loop | #9 | `net/src/peer.rs::dial_loop`, `backoff` | The manager spawns/aborts these; **backoff is not re-implemented**. |
| Connection-state model (`PeerConnState`, `ConnEvent`, `PeerTable`) | #9 | `net/src/state.rs` | Extended additively (offline reason), not replaced. |
| Accept-gate reject-before-bytes | #9 | `net/src/handler.rs`, `net/src/admission.rs` | Reused verbatim; admission source becomes live. |
| Admission shape `device→identity→Active?` + fail-closed overlay | #9 | `net/src/admission.rs` | New `SnapshotAdmission` impl over the live snapshot. |
| Carrier + `Router` + `Shared` routing tables | #9 | `net/src/transport.rs` | The manager holds `Arc<Shared>` + `Endpoint`. |
| Engine pump (single-owner engine, `on_connect`/`on_disconnect`/`on_tick`) | #9/#11 | `net/src/node.rs::pump` | The pump drives reconciliation on fold change. |
| Dedup by `event_id` (G-set), reconnect re-pull via `on_connect` handshake | #11 | core `sync`/`store` | The no-duplicate-delivery guarantee rests on this. |
| CLI dial-set + admission construction from a fold | #17/#20 | `cli/src/message.rs::build_dial_set`, `build_admission` (also consumed by `cli/src/pipe.rs` ×3 and mirrored in `cli/src/join.rs`) | Logic moves into net; **all three** CLI call sites delegate. |
| Loopback tests T1–T9 (exchange, reject-unknown, trichotomy, basic reconnect) | #9 | `net/tests/loopback.rs` | Superset multi-peer test added. |

**Key consequence:** the four acceptance criteria are *partially* satisfied by landed code.
The residual work per AC is spelled out in §9. Do not duplicate the dial loop, the backoff
schedule, the frame codec, the accept gate, or the dedup — wire them.

---

## 3. Goals and non-goals

### Goals
- G1. Maintain, for a live room session, exactly one managed outbound dial loop per active
  remote member device, derived from the membership snapshot and reconciled on change.
- G2. Reject unknown/removed inbound endpoints *continuously*, tracking the live snapshot
  (not a start-of-command freeze).
- G3. Track connection state keyed by **endpoint** (`device_id == EndpointId`) and by
  **identity** (`sender_id`), and surface the offline/unauthorized/transport-failure
  distinction (PRD §16.3, §18.1).
- G4. Reconnect with the existing bounded backoff; a stop→restart cycle re-converges
  without duplicate event delivery.
- G5. Make connection state visible to the operator via CLI output and/or stable logs.

### Non-goals (explicitly out of scope for IR-0107)
- N1. Relay-vs-direct path *selection* or a NAT diagnostics command — iroh owns path
  selection; Gate A (real-NAT confirmation) remains separately owed (IR-0005 NOTES).
  IR-0107 may *report* the path type iroh exposes but does not implement traversal.
- N2. The deterministic double-connect tie-break (D8/OQ-4) — still a follow-up; the
  manager keeps the landed last-writer-wins tolerance.
- N3. Gossip/presence liveness channel (ADR-1 parks it) — not introduced here.
- N4. Any change to the event/validation/membership trust boundary or the wire format.
- N5. The join-bootstrap provisional path (IR-0104) — preserved unchanged; the manager
  must not regress it (see §6.4).
- N6. Persisting connection state across process restarts — connection state is live and
  in-memory only; the *event log* persistence is unchanged and already landed.

---

## 4. Design

### 4.1 Component: `PeerManager` (new module `net/src/manager.rs`)

A room-scoped owner of the outbound connection set. It does **not** own the engine or the
`Router`; it holds handles to dial the endpoint and mutate the shared per-peer tables.

```rust
/// Room-scoped manager of the outbound dial set (ADR-1 "per-room peer manager").
/// Owns one dial-with-backoff loop per desired active-member device and keeps that
/// set reconciled against the live membership snapshot.
pub struct PeerManager {
    shared: Arc<Shared>,          // per-peer tables + admission source + audit
    endpoint: Endpoint,           // to spawn dial_loop
    self_device: EndpointId,      // never dial ourselves
    // device -> running dial loop (handle + the addr we are dialing)
    dials: Mutex<HashMap<EndpointId, DialEntry>>,
    // optional operator-supplied addressing hints (--peer), by device id
    addr_hints: HashMap<EndpointId, EndpointAddr>,
}

struct DialEntry {
    handle: JoinHandle<()>,
    addr: EndpointAddr,
}
```

Public surface (minimal):

- `PeerManager::new(shared, endpoint, self_device, addr_hints) -> Self`
- `fn reconcile(&self, snapshot: &MembershipSnapshot)` — the heart of the manager (§4.2).
- `fn shutdown(&self)` — abort every dial loop (idempotent; also runs on `Drop`).
- `fn desired_devices(snapshot, self_device) -> BTreeSet<EndpointId>` — pure helper,
  the promoted/renamed form of the CLI's `build_dial_set` device selection.

The manager **replaces** the current `NetTransport::dial_tasks: Vec<JoinHandle>` (a flat,
never-pruned list). `NetTransport` keeps `connect_to` for tests/tools but the room session
drives dialing through the manager.

### 4.2 Reconciliation algorithm (`reconcile`)

Given the current membership snapshot:

1. Compute `desired = active_members(snapshot).devices() − {self_device}`. For each device,
   resolve its `EndpointAddr`: an explicit `--peer` hint if one matches, else a bare
   `EndpointId` (iroh discovery resolves it in real-network mode). *(Reuse the exact
   selection logic in `message::build_dial_set`; move it into `manager.rs` and have the
   CLI delegate so there is one implementation.)*
2. **Start**: for each `device ∈ desired` with no live `DialEntry`, spawn
   `peer::dial_loop(shared, endpoint, addr)` and record the handle. Set the table to
   `Connecting` (first-sight `ConnEvent` fires) so the operator sees the attempt.
3. **Stop**: for each `device` with a live `DialEntry` but `device ∉ desired` (member
   removed/left), `handle.abort()`, remove the entry, `shared.close_connection(device)`
   (tear down any in-flight link so we stop serving a now-removed peer), `shared.unregister(device)`,
   and set the table state to a terminal `Offline{reason: Deauthorized}` **or** drop the
   entry from the table (decision D3, §7). Emit the `peer.deauthorized` audit line.
4. **Address change**: if a device is still desired but its resolved `EndpointAddr` hint
   changed, abort+respawn its loop with the new address. (Rare; only when `--peer` hints
   are updated. Safe to defer if hints are immutable per session — see D4.)

`reconcile` is **idempotent**: calling it with an unchanged snapshot is a no-op (no loop
churn, no spurious `ConnEvent`s). This matters because it is called on every fold change
and every tick fallback (§4.3).

### 4.3 Wiring reconciliation into the pump

The engine is single-owner (the `node.rs::pump` task). Membership changes are applied there
(`on_message`, `publish`, `on_connect` handshake results). The manager must reconcile when
the fold changes. Two viable triggers (D2):

- **(Recommended) Snapshot-diff on the existing tick.** The pump already runs `on_tick`
  every `DEFAULT_TICK` (250 ms). After the tick — and after any `on_message`/`publish`
  that could mutate membership — compute `engine.snapshot()`, and if its
  membership-relevant projection differs from the last reconciled one, call
  `manager.reconcile(&snapshot)` and refresh the admission cell (§4.4). A cheap change
  detector: compare `(sorted active (identity, device) pairs, fail_closed set)`.
  Rationale: no new event plumbing; bounded latency (≤ one tick) to react to a removal,
  which is acceptable for a P0 small-room MVP; reuses the anti-entropy cadence already
  driving reconnect catch-up.
- **(Alternative) Push on change.** Have the engine/pump emit a `MembershipChanged` signal
  the manager subscribes to. Lower latency, more plumbing, and the engine currently exposes
  no such event. Deferred; the tick-diff is sufficient and simpler.

The manager and the admission cell are created in `Node::spawn` and moved into the pump so
the single-owner discipline is preserved (no `&mut engine` escapes the pump).

### 4.4 Live admission: `SnapshotAdmission`

Replace the frozen `AllowlistAdmission` for room sessions with a snapshot-backed gate that
reads the *current* membership on every `authorize` call. This is the IR-0005 NOTES D6/OQ-6
re-point, now due.

```rust
/// Admission backed by the live membership snapshot (the IR-0005 D6 production re-point).
/// `authorize(device)` reads the *current* snapshot: device→identity reverse map,
/// Active set, and the §0/§5 fail-closed overlay.
pub struct SnapshotAdmission {
    cell: Arc<ArcSwap<AdmissionView>>, // lock-free read on the hot accept path
}

struct AdmissionView {
    device_to_identity: HashMap<DeviceKey, IdentityKey>,
    active: HashSet<IdentityKey>,
    fail_closed: HashSet<IdentityKey>,
}
```

- The pump updates `cell` from `engine.snapshot()` + `engine.fail_closed_subjects()` in the
  same place it triggers reconciliation (§4.3), so admission and the dial set never drift.
- `authorize` keeps the exact decision order of `AllowlistAdmission`
  (`UnknownDevice` → `FailClosed` → `NotActive` → `Admit{identity}`), so the accept handler,
  the reject-before-bytes guarantee, and every existing admission test semantics are
  unchanged. `AllowlistAdmission` is **retained** (fixtures/tests, and the fluent builder);
  `SnapshotAdmission` is additive.
- `JoinBootstrapAdmission` continues to wrap the inner gate (now `SnapshotAdmission`); its
  one-outcome override for the open-join window is unchanged (§6.4).
- Use `arc-swap` (already a transitively-available crate — confirm; else `Arc<Mutex<…>>`
  read under a short critical section is acceptable at N≤5). The accept path must stay
  allocation-light and non-blocking; do not hold a lock across `.await`.

> **Rationale for keeping `PeerConnState` a trichotomy but admission live:** the enum's
> labels are pinned by tests and appear in logs/CLI; expanding it is churn. Removal is
> expressed by (a) admission flipping the device to rejected and (b) the manager aborting
> its dial loop — both of which the existing state machine already renders as
> `Unauthorized`/`Offline`. The *reason* refinement (below) carries the diagnostic.

### 4.5 Connection state: distinguishing transport failure from offline

PRD §16.3 requires the CLI to distinguish *offline peer* from *unauthorized peer*, and
§18.1 calls for "clear connection state" and "network diagnostics." Today `Offline` conflates
"never had a path," "dial/connect failed," and "was connected, link dropped."

**Design (additive, low-risk):** keep `PeerConnState` as the four-value load-bearing enum,
and attach an `OfflineReason` diagnostic to `PeerEntry` (and, optionally, to `ConnEvent`):

```rust
/// Why a peer is not currently Connected (observability/diagnostics only; never a
/// trust input). Refines PeerConnState::Offline for PRD §16.3 / §18.1 output.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OfflineReason {
    #[default]
    NeverDialed,     // desired, no attempt completed yet
    Unreachable,     // endpoint.connect() failed — no path (the common "peer is offline")
    TransportError,  // connected at QUIC but stream open/handshake failed (TLS/ALPN/proto)
    LinkDropped,     // was Connected, the live link fell (transient)
    Deauthorized,    // removed from the room mid-session; loop stopped (terminal)
}
```

- `PeerEntry` gains `offline_reason: OfflineReason` and (optional) `last_error: Option<Cow<'static,str>>`
  for a short human string. `Connected`/`Connecting`/`Unauthorized` ignore it.
- `PeerTable::set` gains a reason-aware setter (e.g. `set_offline(device, reason)` and
  keep `set(...)` for the non-offline transitions), or extend `set` with an
  `Option<OfflineReason>`. Preserve the existing idempotent-no-event semantics: a re-set to
  the same `(state, reason)` emits nothing; a reason *change* while staying `Offline`
  should update the entry but — to keep `ConnEvent` a pure state-transition stream — SHOULD
  NOT emit a new `ConnEvent` unless we choose to add the reason to `ConnEvent` (D5).
- `dial_loop` is the natural source of the reason:
  - `endpoint.connect(...)` `Err` → `Offline{Unreachable}`.
  - `open_bi`/`establish_outbound` transient failure (not a REJECT close) → `Offline{TransportError}`.
  - `conn.closed()` after being `Up` (not a REJECT) → `Offline{LinkDropped}`.
  - REJECT close / admission `Reject` → `Unauthorized` (unchanged).
  - provisional-only dial verdict → `Offline{Unreachable}` (unchanged behaviour).
- The snapshot API grows an entry-returning form (e.g. `PeerTable::entries() -> Vec<(EndpointId, PeerEntry)>`)
  so the CLI can render state **and** reason **and** bound identity. `peer_states()` (state-only)
  stays for back-compat.

This gives the CLI a faithful §16.3 rendering ("offline — unreachable" vs "offline —
transport error: ALPN mismatch" vs "unauthorized") while the trust-relevant enum and its
pinned labels are untouched.

### 4.6 Tracking by endpoint AND identity

`PeerEntry.identity: Option<IdentityKey>` already records the bound identity on admit
(`state.rs`). IR-0107 makes this reliably populated for the manager's view:
- On a successful admit (dial or accept), identity is set (already the case).
- The CLI join key for display is the **identity** (`sender_id`), with the device shown as a
  secondary detail, because a member may have multiple devices (§Membership §1). The manager
  MAY group table entries by identity for the operator view, but the dial set and admission
  remain keyed by **device** (`EndpointId`) — the cryptographically-proven unit.
- Provide `PeerTable::identity_of(device)` and a reverse `devices_of(identity)` helper for
  the CLI grouping.

### 4.7 Reconnect without duplicate delivery (the guarantee)

This is a **property to preserve and prove**, not new machinery:

- On link re-establishment the manager's `dial_loop` re-registers the connection and flips
  the peer to `Connected`; the pump runs `engine.on_connect(peer)`, which re-issues the
  pull handshake (membership sub-DAG never-windowed + recent-chat window).
- The engine/store dedup by `event_id` (`InsertOutcome::Duplicate`, G-set) makes re-pulled
  or re-pushed already-known events **idempotent**: they are counted as duplicates and never
  re-applied, never re-fanned-out as new.
- The CLI `room tail` already keeps a `seen` set (`message.rs`) so a re-delivered event is
  not re-printed to the human timeline.
- **Deliverable:** an explicit multi-peer integration test (§8, `T-RC`) that asserts the
  store event-count and head set are **stable** across a stop→restart cycle — i.e. reconnect
  causes zero net new applications for already-seen events, and the restarted peer converges
  to the byte-identical head set.

---

## 5. Data / API model impact

- **New public types (net):** `PeerManager`, `OfflineReason`, `SnapshotAdmission` (+ its
  `AdmissionView` builder). Re-exported from `lib.rs`.
- **Changed types (net, additive):** `PeerEntry { + offline_reason, + last_error }`;
  `PeerTable` gains `set_offline`/entry-returning snapshot/`devices_of`; optionally
  `ConnEvent { + reason }` (D5).
- **`NetTransport`:** `dial_tasks: Vec<…>` replaced by ownership of a `PeerManager`
  (or the manager lives in `Node` and `NetTransport` exposes `shared()` + `endpoint()` as
  today). `connect_to`/`disconnect_peer` retained for tools/tests.
- **`Node`:** constructs the `PeerManager` + `SnapshotAdmission` cell; the pump reconciles
  on fold change; adds handles used by the CLI: `peer_entries()`, `conn_events()` (exists),
  and a `reconcile_now()` test hook.
- **No `iroh-rooms-core` changes required** beyond possibly exposing an existing
  `fail_closed_subjects()`/`snapshot()` (already present). The wire format, event schema,
  and validation are untouched.
- **CLI:** `message.rs` delegates dial-set/admission construction to net; new output surface
  (§6.3).

Backwards compatibility: all net changes are additive; `AllowlistAdmission` and
`peer_states()` remain. No persisted-schema or wire change → no migration.

---

## 6. CLI impact (PRD §16.3)

### 6.1 Where connection state is needed
- `room tail <ROOM_ID>` (long-running session): the operator wants to see who is
  connected/offline/unauthorized live.
- `room send <ROOM_ID> …` (short-lived, offline-first): already reports peers-reached count;
  enrich the failure summary with per-peer reason for peers it could not reach.
- A read of "who is reachable right now" without sending anything.

### 6.2 Reject a dedicated schema churn; prefer minimal surface
The PRD §16 command list does not include a `room peers` subcommand, and AC3 permits
"CLI commands **or** logs." Recommended surface (D6):

1. **Primary — `room tail` connection panel + stable logs.** On session start and on every
   `ConnEvent`, print a stable, greppable status line per peer, e.g.:
   ```
   peer <identity-short> device=<device-short> state=connected
   peer <identity-short> device=<device-short> state=offline reason=unreachable
   peer <identity-short> device=<device-short> state=unauthorized
   ```
   and a one-line roster summary (`peers: 2 connected, 1 offline, 0 unauthorized`) refreshed
   on change. These lines double as the §16.3 "clear connection state" output and as the
   greppable audit trail. Drive them from `node.conn_events()` + `node.peer_entries()`.
2. **Recommended companion — `room members <ROOM_ID> --status [--peer …] [--timeout …] [--loopback]`.**
   Today `room members` is purely local (folds the log). A `--status` flag brings up an
   ephemeral node, reconciles the dial set, waits up to `--timeout`, then prints each member
   with `role`, membership `status`, **and** live `conn` state + reason + bound device. This
   directly serves "distinguish offline peer, unauthorized peer" for a human, reusing the
   `room members` mental model rather than inventing a new noun.

Pick (1) as mandatory (satisfies AC3 with least surface) and (2) as the recommended
human-facing view. A standalone `room peers` subcommand is possible but adds a noun the PRD
did not list — treat as optional (open question OQ-2).

### 6.3 Output rules (PRD §16 UX requirements)
- Script-friendly, stable, lowercase reason strings (reuse `PeerConnState::label()` +
  new `OfflineReason::label()`), pinned by tests exactly like the existing labels.
- Failure states must be honest and explicit (§16.4): never render "offline" for an
  `unauthorized` peer, and never imply delivery to an offline peer.
- Reuse the existing audit vocabulary (`peer.connected`, `peer.disconnected`,
  `peer.rejected:<cause>`) and add `peer.deauthorized` for a mid-session roster removal and
  `peer.offline:<reason>` for the diagnostic transition.

### 6.4 Preserve the join bootstrap (IR-0104)
`room tail --accept-joins` must keep engaging `JoinBootstrapAdmission` over the (now live)
inner gate. The manager reconciler must **not** stop dialing / must not deauthorize a peer
that is only provisionally admitted, and `maybe_upgrade_provisional` must still fire on
upgrade-on-learn. Concretely: `desired_devices` is computed from *active* members only, so a
provisional (not-yet-active) inbound joiner is naturally *not* in the outbound dial set (the
joiner dials us), and the reconciler leaves inbound-accepted provisional peers alone. Add a
regression test.

---

## 7. Key decisions

- **D1. IR-0107 is a consolidation/lifecycle layer, not a rewrite.** Reuse dial loop,
  backoff, frame codec, accept gate, and dedup verbatim; add the manager, live admission,
  reason diagnostic, CLI surface, and the multi-peer test.
- **D2. Reconcile via snapshot-diff on the existing pump tick** (≤250 ms reaction), not a new
  membership-change event bus. Simpler, no new engine API; latency is acceptable for a P0
  small room. (Push-on-change is a later optimization.)
- **D3. A removed member's table entry becomes `Offline{Deauthorized}` (kept, not dropped)**
  so the CLI can show "left/removed — connection stopped" for one render cycle; it may be
  garbage-collected after. (Alternative: drop immediately — rejected because the operator
  loses the signal that we *stopped* dialing them.)
- **D4. `--peer` addressing hints are immutable for a session.** Address-change reconciliation
  (§4.2 step 4) is therefore a no-op in practice and may be omitted in v1; document it as a
  follow-up. Membership-driven start/stop is the load-bearing path.
- **D5. Do not add `reason` to `ConnEvent` in v1.** Keep `ConnEvent` a pure state-transition
  stream (idempotent per state). The CLI reads the reason from `peer_entries()` when it
  renders a transition. (Revisit if a consumer needs the reason atomically with the event.)
- **D6. Connection-state output = mandatory `room tail` panel/logs + recommended
  `room members --status`.** No new top-level subcommand required for AC3.
- **D7. Keep `PeerConnState` a four-value enum; carry the transport-failure distinction as an
  additive `OfflineReason`.** Avoids churn to the pinned label set and the engine driver's
  `handle_conn_event` match.
- **D8. `SnapshotAdmission` reads lock-free (`arc-swap`) on the accept hot path**; the pump is
  the sole writer. Preserve the exact decision order so reject-before-bytes and all admission
  tests are semantically unchanged.

---

## 8. Test strategy

Deterministic loopback (no relay, no real network), mirroring the existing `net/tests`
harness. New file `net/tests/manager_e2e.rs` (or extend `loopback.rs`).

Unit (in `manager.rs`, `state.rs`, `admission.rs`):
- `desired_devices` selection: excludes self, dedups multi-binding, excludes non-active,
  resolves `--peer` hint over bare id.
- `reconcile` is idempotent (unchanged snapshot ⇒ no new loops, no `ConnEvent`s).
- `reconcile` **start**: a newly-active member gets a dial loop and a `Connecting` first-sight.
- `reconcile` **stop**: a removed member's loop is aborted, connection closed/unregistered,
  entry → `Offline{Deauthorized}`, `peer.deauthorized` audited.
- `SnapshotAdmission`: same decision matrix as `AllowlistAdmission` (unknown/fail-closed/
  not-active/admit), plus a **live** flip: authorize=Admit, swap in a snapshot without the
  identity, authorize=Reject(NotActive) — proving mid-session removal takes effect.
- `OfflineReason` label strings pinned (stable tooling contract).

Integration — the issue Test Plan ("local multi-peer integration test with peer
start/stop/restart and unauthorized inbound connection"):
- **T-Known (AC1):** 3 active members A,B,C. Bring up all three; assert each dials the other
  two and reaches `Connected` (full mesh), and a published event fans out to both.
- **T-Reject (AC2):** a stranger (unbound device) and a **removed** member both dial A;
  assert both are `Unauthorized`/rejected *before bytes* — the removed one proving live
  admission (it was Active earlier in the same process, then removed).
- **T-Visible (AC3):** assert `peer_entries()`/CLI output shows the trichotomy **and** the
  offline reason (`unreachable` for a never-started peer vs `link_dropped` after a drop),
  and that the stable log/CLI lines render as specified in §6.2.
- **T-RC (AC4 — the headline):** A,B,C converge on N events; **stop** B (drop its node);
  A and C publish M more events; assert A/C store counts advance by exactly M (no duplicate
  application from B's absence/return churn); **restart** B; assert B converges to the
  identical `(event_count, head set, ordered timeline)` as A/C, and that **A's and C's store
  counts do not change** when B reconnects and re-syncs (reconnect delivers no duplicates).
  Also assert B does not re-print previously-seen events to its `room tail` timeline.
- **T-Bootstrap (regression):** with `--accept-joins`, a provisional joiner is admitted,
  pushes its `member.joined`, upgrades on learn, and the reconciler neither deauthorizes it
  nor double-dials it (IR-0104 preserved).

CLI tests (`cli/tests/`): `room tail` prints the connection panel/log lines with stable
reason strings; `room members --status` renders membership × connection state; script-friendly
format asserted with `assert_cmd`/snapshot.

Gate/CI: all suites run <~2 s, no relay, deterministic (spec D9/OQ-2). `scripts/verify.sh`
(fmt + clippy + tests) must pass. **Gate A (real-NAT)** remains separately owed and is
*not* discharged here (inherits the IR-0005 residual).

---

## 9. Acceptance criteria → verification

| # | Issue AC | Landed baseline | IR-0107 residual | Verified by |
|---|---|---|---|---|
| 1 | Known peers are dialed for a room | dial loop exists; CLI dials a frozen set | `PeerManager.reconcile` derives + maintains the set from the live snapshot | `manager_e2e::T-Known`, `reconcile` unit tests |
| 2 | Unknown inbound endpoints are rejected | accept-gate rejects before bytes (static snapshot) | `SnapshotAdmission` rejects **since-removed** devices live | `T-Reject`, `SnapshotAdmission` live-flip unit test |
| 3 | Connection state is visible to CLI commands or logs | `PeerConnState` + `ConnEvent` + `peer_states()` | `OfflineReason` diagnostic + `room tail` panel/logs (+ `room members --status`) | `T-Visible`, CLI tests, pinned label tests |
| 4 | Reconnect does not duplicate event delivery | engine dedup + `on_connect` re-pull + basic T4 | explicit stop/restart multi-peer no-duplicate assertion | `manager_e2e::T-RC` |

All four ACs must have a passing automated test; the multi-peer stop/start/restart +
unauthorized-inbound scenario (T-RC + T-Reject) is the issue's named Test Plan.

---

## 10. Risks and mitigations

- **R1 (risk/high — scope/overlap).** Much of this exists; the danger is re-implementing dial
  loops or the state model and diverging. *Mitigation:* §2 table pins what to reuse; the
  manager *composes* `dial_loop`, does not replace it.
- **R2 (roster-change race).** A member removed while a link is mid-flight, or a reconcile
  racing an accept. *Mitigation:* the pump is the single writer of both the admission cell and
  the reconcile trigger; admission is the authority (reject-before-bytes still holds even if a
  dial loop lags a tick); `reconcile` aborts + closes atomically per device under the `dials`
  lock.
- **R3 (live-admission correctness).** A subtle change to admission could weaken the trust
  boundary. *Mitigation:* `SnapshotAdmission` preserves the exact decision order; the full
  `AllowlistAdmission` matrix is re-run against it; `AllowlistAdmission` is retained for the
  existing tests.
- **R4 (dial-loop leak / churn).** Aborting/respawning loops on every tick would thrash.
  *Mitigation:* `reconcile` is idempotent and diffs against the running set; only genuine
  membership deltas start/stop a loop.
- **R5 (transport-failure classification accuracy).** Distinguishing "unreachable" from
  "transport error" depends on iroh's error surface. *Mitigation:* classify conservatively —
  `connect()` error ⇒ `Unreachable`; post-connect stream/handshake failure ⇒ `TransportError`;
  REJECT close ⇒ `Unauthorized`. The reason is diagnostic-only (never a trust input), so an
  occasional misclassification degrades a label, not correctness.
- **R6 (Gate A still owed).** Real-NAT reconnect behaviour is unproven on the loopback suite.
  *Mitigation:* explicitly out of scope (N1); carried as the standing IR-0005 residual;
  loopback proves the manager logic deterministically.
- **R7 (CLI surface creep).** Adding a connection panel risks noisy or unstable output.
  *Mitigation:* stable pinned reason strings; summary line + per-transition lines only;
  no new top-level subcommand (D6).

---

## 11. Implementation steps (for the build task)

1. **Promote dial-set selection into net.** Move `build_dial_set` device-selection +
   `build_admission` shape from `cli/src/message.rs` into `net/src/manager.rs` as
   `PeerManager::desired_devices` / an `AdmissionView` builder; have **every** current
   consumer delegate — `message.rs`, the three `pipe.rs` call sites, and the mirrored
   id-matching in `join.rs` (`build_dial_set`'s sibling) — so there is one implementation.
   Keep behaviour byte-identical; re-run the existing `message.rs`/`pipe.rs`/`join.rs` unit tests.
2. **Add `OfflineReason` + reason-aware `PeerTable` setters** (`state.rs`), additive; pin
   labels; keep idempotent-no-event semantics. Update `dial_loop` to pass the reason at each
   `Offline` set (`Unreachable`/`TransportError`/`LinkDropped`) and add `Deauthorized`.
3. **Implement `SnapshotAdmission`** (`admission.rs`) over an `Arc<ArcSwap<AdmissionView>>`;
   re-export; do **not** remove `AllowlistAdmission`. Add the live-flip unit test.
4. **Implement `PeerManager`** (`manager.rs`): `new`, `reconcile` (start/stop/idempotent),
   `shutdown`/`Drop`. Own the `dials` map; replace `NetTransport::dial_tasks`.
5. **Wire into `Node`/pump** (`node.rs`): construct manager + admission cell; on fold change
   (snapshot-diff after tick / after `on_message`/`publish`), `reconcile` + refresh admission;
   preserve provisional/join-bootstrap paths; add `peer_entries()` and a `reconcile_now()`
   test hook.
6. **CLI output** (`cli/src/message.rs`, `room.rs`, `cli.rs`): drive a `room tail` connection
   panel + stable per-transition lines from `conn_events()`/`peer_entries()`; add
   `room members --status`; add `peer.deauthorized` / `peer.offline:<reason>` audit lines.
7. **Tests:** unit (steps 2–4), `net/tests/manager_e2e.rs` (T-Known/T-Reject/T-Visible/T-RC/
   T-Bootstrap), CLI output tests. Run `scripts/verify.sh`.
8. **Docs:** update `crates/iroh-rooms-net/NOTES.md` (close the D6/OQ-6 + N6 + roster
   reconciliation follow-ups), the README "Current Status" section, and
   `docs/getting-started.md` connection-state/troubleshooting notes. Do **not** claim Gate A.

Land steps 1–4 (net internals, fully unit-tested) before 5–6 (integration + CLI) so review is
incremental and the trust-boundary changes (admission) are reviewed in isolation.

---

## 12. Open questions

- **OQ-1.** Is `arc-swap` acceptable as a (possibly new) dependency for the lock-free
  admission read, or should the accept path take a short `Mutex` (fine at N≤5)? Default:
  short `Mutex` if `arc-swap` is not already in the tree, revisit if profiling shows contention.
- **OQ-2.** Do we want a first-class `iroh-rooms room peers <ROOM_ID>` subcommand, or is the
  `room tail` panel + `room members --status` sufficient for MVP? (PRD §16 does not list a
  `peers` noun.) Default: no new subcommand.
- **OQ-3.** Reaction latency: is ≤1 tick (250 ms) to react to a membership removal acceptable,
  or do we need push-on-change now? Default: tick-diff is sufficient for MVP.
- **OQ-4.** Should `OfflineReason` be surfaced on `ConnEvent` (atomic with the transition) for
  future consumers, or only via the entry snapshot? Default: entry snapshot only (D5).
- **OQ-5.** Does the deterministic double-connect tie-break (D8/OQ-4 from IR-0005) need to land
  with the manager, or stay a separate follow-up? Default: separate follow-up (N2).
- **OQ-6.** How aggressively should `Offline{Deauthorized}` entries be garbage-collected from
  the table (immediately after one render, after a TTL, or never within a session)? Default:
  keep for the session; revisit if the table grows unbounded under churn.

---

## 13. Assumptions

- A1. `device_id == EndpointId == PeerId` (same 32 bytes) holds throughout (Membership §1),
  as the landed `peer::peer_id` and `endpoint_of` already assume.
- A2. The membership fold/snapshot is the single source of truth for "who is an active
  member," and `engine.snapshot()` / `engine.fail_closed_subjects()` are available to the pump
  (they are, per `node.rs`).
- A3. The engine's dedup (G-set by `event_id`) and `on_connect` re-pull already make reconnect
  idempotent; IR-0107 proves it, it does not add dedup.
- A4. Loopback determinism is sufficient to satisfy the issue Test Plan; the real-NAT Gate-A
  run is a separate, still-owed deliverable and not a blocker for landing IR-0107.
- A5. Small rooms only (N≤5, ceiling ~10–20) — O(n²) managed links are acceptable (ADR-1).
- A6. Connection state is in-memory and per-session; nothing about it is persisted or must
  survive a process restart.
```
