# Spec: Make `JoinBootstrapAdmission`'s `accept_joins` dynamic (`iroh-rooms-net`)

Issue: **#88** — `feat(net)`: make `JoinBootstrapAdmission`'s `accept_joins` dynamic so
resident hosts can gate on pending invites without a session respawn.
Labels: `enhancement type/feature`.
Filed by an SDK consumer (**Bantaba**, a resident daemon on the developer-preview façade,
rev `1d2f014`).

Traceability:
- `PHASE-0-SPIKE.md` Membership & Ordering §5 (admission is a property of the transport);
  the IR-0104 join-bootstrap seam (Approach A) — the provisional overlay this issue makes
  live.
- `crates/iroh-rooms-net/src/admission.rs` — the existing `JoinBootstrapAdmission<A>` and its
  constructor doc, which already state the intended policy ("`accept_joins` should be set by
  the admin's join-hosting session **only** while at least one invite is open; the caller
  computes that policy") but only expose it as a **construction-time `bool`**.
- The live-cell precedent already in the same file: `SnapshotAdmission` reads an
  `Arc<Mutex<AdmissionView>>` on every `authorize()` so the accept gate tracks the live
  roster without a respawn. This issue applies the identical idea to the `accept_joins` knob.

> This is a **planning/spec document only**. No production code is written by this issue's
> planning phase. The implementation steps in §6 are for the executing engineer/agent.

---

## 1. Summary

`JoinBootstrapAdmission<A>` wraps an inner admission gate and, when `accept_joins` is set,
turns the single outcome "unknown device" into `AdmitProvisional` — letting a first-time
invitee pull the secret-free membership sub-DAG and push one `member.joined`. The knob is a
**`bool` fixed at construction** (`admission.rs:299`, `pub fn new(inner: A, accept_joins:
bool)`), and the whole admission chain is installed once at `Node::spawn` and lives for the
session's lifetime (`node.rs:207/305`, `Arc<dyn Admission>`).

For the **CLI** that is exactly right: join-hosting lives inside `room tail --accept-joins`,
an interactively-open command whose lifetime *is* the policy window, so
`message::tail` computes `host_joins` once and passes the fixed `bool`
(`message.rs:458-466`). For a **resident daemon** that holds a room session open
indefinitely, pending-invite state flips many times within one session's lifetime, and the
fixed `bool` forces one of two wrong options:

1. `accept_joins = true` for the whole session — the provisional bootstrap overlay serves the
   membership DAG to unknown devices around the clock, widening the pre-authorization
   metadata surface far beyond the moments an invite is actually pending.
2. `accept_joins = false` + respawn the session to flip it — every invite mint/redemption
   restarts the `Node`, which drops the endpoint and makes every connected peer observe a
   disconnect (`ConnEvent` churn).

Bantaba shipped a ~40-line hand-rolled `DynamicJoinBootstrap` (an `Admission` impl consulting
an `Arc<AtomicBool>`) to get option 3 — "serve bootstrap on exactly while ≥1 unredeemed
invite exists." Every long-running consumer will rediscover the same gap and re-implement the
same combinator.

This issue makes the documented policy expressible in the SDK without a respawn: a
**second, additive constructor**

```rust
impl<A: Admission> JoinBootstrapAdmission<A> {
    /// `accept_joins` is consulted per request instead of fixed at construction.
    pub fn new_dynamic(inner: A, accept_joins: Arc<AtomicBool>) -> Self;
}
```

`authorize()` reads the shared flag on each call, so a caller flips the join window on
(invite minted) and off (redeemed/expired) by writing the `AtomicBool` it already owns — no
node restart, no chain rebuild, and therefore no `ConnEvent` churn on connected members. The
change is **purely additive**: `new(inner, bool)` and every existing behaviour, test, and the
CLI path stay byte-for-byte unchanged; `new_dynamic` is observationally identical to `new`
for any fixed value of the flag, and flipping the flag is equivalent to what changing the
`bool` field in place would do. No new authorization surface: `gate_join` remains the
convergent membership authority on every peer, exactly as today.

---

## 2. Background & current repository state

### 2.1 The gate as it stands (`crates/iroh-rooms-net/src/admission.rs`)

- **Trait.** `Admission: Send + Sync + 'static` with one method
  `fn authorize(&self, device: EndpointId) -> AdmissionDecision` (`:73-77`). It runs inline
  on the accept path and must be "pure and fast."
- **Decision.** `AdmissionDecision::{Admit{identity}, AdmitProvisional, Reject(RejectCause)}`
  (`:21-37`). `AdmitProvisional` is documented as a **liveness + privacy** admit that grants
  no membership (`:27-34`).
- **`JoinBootstrapAdmission<A = AllowlistAdmission>`** (`:287-325`):
  - Fields: `inner: A`, `accept_joins: bool`. `#[derive(Debug, Clone)]`.
  - `new(inner, accept_joins: bool)` (`:299`) and `accepts_joins(&self) -> bool` (`:308`).
  - `authorize` (`:313-325`) changes **exactly one** outcome: an inner
    `Reject(RejectCause::UnknownDevice)` becomes `AdmitProvisional` **iff** `self.accept_joins`;
    every other inner verdict (`Admit`, `NotActive`, `FailClosed`, and unknown-with-window-closed)
    passes through verbatim.
- **Live-cell precedent in the same file.** `SnapshotAdmission` (`:229-256`) holds
  `Arc<Mutex<AdmissionView>>` and calls `.lock()…decide(device)` on every `authorize`, so a
  mid-session removal takes effect within a tick without a respawn — the direct analog of what
  this issue does for the boolean window. Its live-flip test is
  `snapshot_admission_live_flip_on_mid_session_removal` (`:616-634`).

### 2.2 Where the gate is installed and consulted

- **Installed once per session.** `Node::spawn`/`spawn_room`/`spawn_inner` all take
  `admission: Arc<dyn Admission>` and hand it to `NetTransport::bind` (`node.rs:305/357-365`);
  it lives for the node's lifetime. There is **no** API to swap the gate after spawn.
- **Consulted on the accept path only.** `EventProtocolHandler::accept`
  (`handler.rs:49-95`) resolves the QUIC/TLS-proven `device_id` and calls
  `self.shared.admission.authorize(device)` **once per inbound connection, before
  `accept_bi()`**. `Reject` → `conn.close(REJECT_CODE)`; `Admit` → serve; `AdmitProvisional`
  → mark provisional and serve membership only. **Nothing re-authorizes an already-established
  connection against `accept_joins`.** This is the structural fact that makes the AC's
  "connected peers observe no `ConnEvent` churn across the flips" hold: flipping the flag can
  only change the outcome of a *future* `accept()` for an *unknown* device; an Active member's
  live connection is governed by its `Admit` decision and is never re-evaluated because of the
  window flip. (Contrast: the respawn workaround drops the endpoint, severing every peer.)

### 2.3 How consumers construct the gate today

- **CLI (correct as-is, do not change).** `message::tail` computes
  `host_joins = accept_joins && hosting_joins_effective(&snapshot, &self_id)` once
  (`message.rs:458`), where `hosting_joins_effective` = "I am the room admin **and** ≥1 member
  is still `Invited`" (`message.rs:875-877`), then builds
  `JoinBootstrapAdmission::new(SnapshotAdmission::new(cell), host_joins)`
  (`message.rs:463-466`). The command's lifetime is the policy window, so a fixed `bool` is
  exactly right — **this issue leaves the CLI path untouched.**
- **Façade / SDK (the filer's surface).** `JoinBootstrapAdmission` (and `Admission`,
  `SnapshotAdmission`, `AdmissionView`, …) are re-exported from
  `iroh-rooms::experimental::session` (`crates/iroh-rooms/src/experimental/session.rs:10-15`)
  and catalogued in `docs/sdk-coverage.md:57`. A façade consumer builds its own gate and
  passes it to the online session; `new_dynamic` rides along on the already-exported type
  with **no new re-export required**.
- **e2e.** `crates/iroh-rooms-net/tests/join_e2e.rs` drives the full two-node join over
  `NetMode::Loopback` with `JoinBootstrapAdmission::new(…, accept_joins=true)` (`:57-60`, and
  the flow doc at `:6-11`). This is the natural home for the new mid-session-flip e2e (§8).

### 2.4 The workaround being upstreamed

Bantaba's `DynamicJoinBootstrap` is an `Admission` impl holding `Arc<AtomicBool>`, flipped by
its pending-invite bookkeeping. `new_dynamic` folds that combinator into the library so it
composes with the inner gate's overlay logic (Active/NotActive/FailClosed pass-through) that a
from-scratch impl would have to duplicate.

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

Let a long-running host express the already-documented "serve join bootstrap **only** while an
invite is pending" policy by flipping a live flag, with no session respawn and no
`ConnEvent` churn on connected members — while keeping the fixed-`bool` constructor and every
existing behaviour untouched.

### 3.2 In scope

1. **`JoinBootstrapAdmission::new_dynamic(inner, Arc<AtomicBool>)`** — the flag is read per
   `authorize()` call.
2. **`accepts_joins()` reads the live value** for the dynamic case (returns the flag's current
   state), staying exact for the fixed case.
3. **Doc update** on the type / constructors explaining the daemon use case, the live-cell
   read, the memory-ordering choice, and the "no churn because admission is accept-path only"
   guarantee; cross-link `SnapshotAdmission`.
4. **Tests** (unit + one ignored-tier e2e) covering the full decision matrix under a dynamic
   flag, the live on→off→on flip, member-connection insensitivity to the flag, `Clone`
   sharing the cell, composition over `SnapshotAdmission`, and the end-to-end
   mint-window-without-respawn + no-`ConnEvent`-churn AC.

### 3.3 Out of scope / non-goals (explicit)

- **No change to `new`, `authorize`'s decision logic, `AdmissionDecision`, `RejectCause`, or
  the `Admission` trait signature.** The dynamic path produces the identical decision for a
  given flag value; only the *source* of the boolean changes.
- **No new authorization or trust.** The provisional admit remains liveness+privacy only;
  `gate_join` is still the sole membership authority on every peer. Widening the window admits
  no one to membership — it only re-opens the secret-free bootstrap metadata surface.
- **No pending-invite tracking in the SDK.** The library accepts a flag; **the caller computes
  the policy** (mint→set true, redeem/expire→set false), exactly as the existing constructor
  doc already states. No invite-store, timer, or expiry logic is added here.
- **No CLI change.** `room tail --accept-joins` keeps the fixed `bool` (its command lifetime is
  the window). A future interactive/daemon CLI mode could adopt `new_dynamic`, but that is a
  separate issue.
- **No node-level "swap the admission gate" API.** The dynamism lives entirely inside the gate
  via the shared flag; `Node`/`NetTransport` are unchanged.
- **No new dependency.** `AtomicBool`/`Arc` are std; no `arc-swap`/`crossbeam`.
- **No `Fn() -> bool` generalization in this cut** (see OQ-1) — `Arc<AtomicBool>` matches the
  daemon use case and the upstreamed workaround exactly, is lock-free, and keeps
  `#[derive(Debug, Clone)]` intact.

---

## 4. Placement & dependencies

| Change | Crate / file | Kind |
| --- | --- | --- |
| Internal `accept_joins` field → policy source; `new_dynamic`; live `accepts_joins()` | `crates/iroh-rooms-net/src/admission.rs` | additive (one struct field reshaped, `new` unchanged) |
| Doc comments on the type + both constructors | `crates/iroh-rooms-net/src/admission.rs` | docs |
| Unit tests (dynamic matrix, live flip, member-insensitivity, clone, compose) | `crates/iroh-rooms-net/src/admission.rs` `#[cfg(test)]` | tests |
| Mid-session-flip + no-churn e2e | `crates/iroh-rooms-net/tests/join_e2e.rs` (ignored online tier) | tests |
| Note the new method in the SDK coverage doc row (optional, type already listed) | `docs/sdk-coverage.md` | docs |

No `lib.rs` re-export change (the type is already exported at `lib.rs:62-65`), no façade change
(already re-exported at `session.rs:12`), no `Cargo.toml` change. The only imports added to
`admission.rs` are `std::sync::atomic::{AtomicBool, Ordering}` (`Arc` is already imported).

---

## 5. Design

### 5.1 Represent the policy source as a small enum

Reshape the single `accept_joins: bool` field into a two-variant internal enum so the fixed
constructor keeps its exact, allocation-free, faithfully-`Debug` behaviour and the dynamic
constructor shares a live cell:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// Source of the join-bootstrap window flag: fixed at construction (`new`) or a
/// live shared cell the host flips as invites open/close (`new_dynamic`).
#[derive(Debug, Clone)]
enum AcceptJoins {
    Fixed(bool),
    Dynamic(Arc<AtomicBool>),
}

impl AcceptJoins {
    #[inline]
    fn get(&self) -> bool {
        match self {
            Self::Fixed(b) => *b,
            // Relaxed is sufficient: the flag is a standalone advisory boolean, not a
            // lock guarding other data — see §5.3.
            Self::Dynamic(cell) => cell.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct JoinBootstrapAdmission<A: Admission = AllowlistAdmission> {
    inner: A,
    accept_joins: AcceptJoins,
}

impl<A: Admission> JoinBootstrapAdmission<A> {
    /// (unchanged signature) Wrap `inner` with a **fixed** provisional window.
    #[must_use]
    pub fn new(inner: A, accept_joins: bool) -> Self {
        Self { inner, accept_joins: AcceptJoins::Fixed(accept_joins) }
    }

    /// Wrap `inner` with a **live** provisional window read per request. The caller
    /// keeps a clone of `accept_joins` and stores `true` while ≥1 invite is open,
    /// `false` otherwise — so bootstrap serving is on exactly while an invite is
    /// pending, with no session respawn (issue #88). Mirrors `SnapshotAdmission`'s
    /// shared-cell pattern; the caller is the sole writer.
    #[must_use]
    pub fn new_dynamic(inner: A, accept_joins: Arc<AtomicBool>) -> Self {
        Self { inner, accept_joins: AcceptJoins::Dynamic(accept_joins) }
    }

    /// Whether this gate **currently** admits first-time invitees provisionally
    /// (reads the live cell for a dynamic gate).
    #[must_use]
    pub fn accepts_joins(&self) -> bool {
        self.accept_joins.get()
    }
}

impl<A: Admission> Admission for JoinBootstrapAdmission<A> {
    fn authorize(&self, device: EndpointId) -> AdmissionDecision {
        match self.inner.authorize(device) {
            AdmissionDecision::Reject(RejectCause::UnknownDevice) if self.accept_joins.get() => {
                AdmissionDecision::AdmitProvisional
            }
            other => other,
        }
    }
}
```

Why the enum rather than "always store `Arc<AtomicBool>`":

- **Zero perturbation of the fixed path.** `new(inner, false)` stays a plain `bool` with no
  heap allocation and a faithful `Debug` (`Fixed(false)`), so existing snapshot/debug output
  and the hot path for non-hosting nodes are unchanged.
- **`#[derive(Debug, Clone)]` still holds.** `Arc<AtomicBool>` is both `Clone` and `Debug`, so
  the parent derives compile as-is — **no** manual `Debug` impl (unlike `SnapshotAdmission`,
  which needed one for its non-`Debug` field).
- **Room to grow.** A future `Fn() -> bool` variant (OQ-1) is an additive enum arm, not another
  breaking constructor churn.

The only observable change to `authorize`/`accepts_joins` is that the boolean is now read from
`self.accept_joins.get()` instead of a `bool` field — identical result for `Fixed`.

### 5.2 Caller pattern (documented, not shipped)

A resident host owns the flag and derives it from its own invite bookkeeping:

```rust
let window = Arc::new(AtomicBool::new(false)); // no pending invites at startup
let gate = JoinBootstrapAdmission::new_dynamic(SnapshotAdmission::new(cell), window.clone());
let node = Node::spawn(secret, Arc::new(gate), audit, engine, cfg).await?;
// … later, on invite mint:
window.store(true, Ordering::Relaxed);
// … on the last invite being redeemed or expiring:
window.store(false, Ordering::Relaxed);
```

The SDK computes no policy: it consumes the flag the caller already maintains — the same
contract the current constructor doc states, now expressible without a respawn.

### 5.3 Memory ordering

`Relaxed` on both load and store is sufficient and is the recommended default:

- The window flag is a **standalone advisory boolean**, not a lock protecting other shared
  state. `authorize` reads only the flag; it does not depend on any other memory being
  published in a happens-before relationship with the flag write.
- A briefly-stale read is **bounded and benign**. If a reader sees the old `true` just after
  the caller stored `false`, the worst case is one extra `AdmitProvisional` for an unknown
  device — which `gate_join` still rejects, granting no membership. If a reader sees the old
  `false` just after the caller stored `true`, the invitee's bootstrap is refused once and it
  retries. No corruption, no lost membership, no security regression in either direction.
- Correctness never depends on the flip being *instantaneously* visible — the existing
  fixed-`bool` gate already has no such guarantee across threads. A caller that wants a
  happens-before with its own invite-state writes may use `Release`/`Acquire`, but this is not
  required for the gate's correctness; the doc will note the default is `Relaxed` and why.

### 5.4 Why this satisfies "no `ConnEvent` churn" (the load-bearing AC)

The admission gate is consulted **once per inbound connection at accept time**
(`handler.rs:54`), and only its `UnknownDevice`-vs-`AdmitProvisional` branch depends on the
flag. Flipping the `AtomicBool`:

- does **not** rebuild the admission chain, respawn the `Node`, or touch the `Endpoint`;
- does **not** re-run `accept()` for any established connection;
- changes **only** the verdict a *future* `accept()` returns for an *unknown* device.

Therefore an Active member that is already connected (its verdict was `Admit`, independent of
the flag) sees no close, no redial, and emits no `ConnEvent` when the window flips — the exact
property the respawn workaround violates. This is asserted directly by the e2e in §8.

---

## 6. Implementation steps

Ordered so each step compiles and is independently testable.

### Step 1 — Introduce the policy-source enum and `new_dynamic`
- In `crates/iroh-rooms-net/src/admission.rs`, add `use std::sync::atomic::{AtomicBool,
  Ordering};`.
- Add the private `AcceptJoins` enum (`Fixed(bool)` / `Dynamic(Arc<AtomicBool>)`) with a
  `#[inline] fn get(&self) -> bool` using `Ordering::Relaxed` for the dynamic arm (§5.1).
- Change `JoinBootstrapAdmission`'s field to `accept_joins: AcceptJoins`; keep
  `#[derive(Debug, Clone)]`.
- Update `new` to wrap `AcceptJoins::Fixed(accept_joins)` (signature unchanged).
- Add `new_dynamic(inner: A, accept_joins: Arc<AtomicBool>) -> Self` → `AcceptJoins::Dynamic`.
- Update `accepts_joins` to return `self.accept_joins.get()`.
- Update `authorize` to gate on `self.accept_joins.get()` (logic otherwise identical).

### Step 2 — Doc comments
- Extend the `JoinBootstrapAdmission` type doc and both constructor docs (§5.1/§5.2): explain
  the resident-daemon motivation, the per-request live read, the `Relaxed` choice (§5.3), that
  the caller is the sole writer and owns the policy, and the "no churn because admission is
  accept-path only" guarantee (§5.4). Cross-link `SnapshotAdmission` as the sibling live-cell
  pattern.

### Step 3 — Unit tests (§8, in-module)
- Add the dynamic-matrix, live-flip, member-insensitivity, `accepts_joins`-tracks-flag,
  clone-shares-cell, and compose-over-`SnapshotAdmission` tests alongside the existing
  `JoinBootstrapAdmission` tests (`admission.rs:662-777`).

### Step 4 — e2e (ignored online tier)
- Add the mid-session-flip + no-`ConnEvent`-churn test to
  `crates/iroh-rooms-net/tests/join_e2e.rs`, reusing its two-node loopback harness and the
  bounded-await helpers, on the same `#[ignore]`/online tier as the existing join tests.

### Step 5 — Docs/coverage (optional)
- Optionally annotate the `JoinBootstrapAdmission` row in `docs/sdk-coverage.md:57` to mention
  the dynamic constructor; no table restructure needed (the type is already listed).

### Step 6 — Gate
- `scripts/verify.sh` is the CI gate (project memory *verify.sh is the real CI gate*): `cargo
  fmt --check`, `clippy -D warnings` (pedantic), and `--all-features` tests must be green. No
  new `#[allow]`; the new `AcceptJoins`/`new_dynamic`/`Dynamic` arm must be reached by the new
  tests so no `dead_code` allowance is needed.

---

## 7. Error model & observability

- **No new error surface.** `AdmissionDecision` and `RejectCause` are unchanged; the audit
  strings (`reject.<cause>` codes) are unaffected. A window flip changes only whether an
  unknown device is refused (`unknown_device`) or provisionally admitted — the same two
  outcomes the fixed `bool` already produces.
- **Observability of the flip is the caller's.** The library adds no logging on flip (an
  `authorize` runs on the hot accept path and must stay allocation-free and quiet — matching
  the existing gate). A daemon that wants to log window transitions does so where it writes the
  flag; the CLI's `report_accept_joins` (`message.rs:880`) remains the example of caller-side
  reporting.

## 8. Test strategy

All unit tests are network-free and deterministic, mirroring the existing
`JoinBootstrapAdmission` block (`admission.rs:662-777`). The e2e reuses the ignored online tier.

**Unit (in `admission.rs` `#[cfg(test)]`):**
1. **Dynamic matrix, flag=true.** `new_dynamic(AllowlistAdmission…, Arc::new(AtomicBool::new(
   true)))`: unknown → `AdmitProvisional`; Active member → `Admit`; bound-but-inactive →
   `Reject(NotActive)`; fail-closed → `Reject(FailClosed)`. (Byte-for-byte the `new(…, true)`
   matrix.)
2. **Dynamic matrix, flag=false.** Same construction with `false`: unknown →
   `Reject(UnknownDevice)`; Active → `Admit`; the other two unchanged. (Matches `new(…,
   false)`.)
3. **Live flip (the core AC at unit level).** Build with a shared `Arc<AtomicBool>` starting
   `false`. An unknown device → `Reject(UnknownDevice)`. `store(true)`; the **same** unknown
   device → `AdmitProvisional`. `store(false)`; → `Reject(UnknownDevice)` again. (Sibling of
   `snapshot_admission_live_flip_on_mid_session_removal`.)
4. **Member connection is insensitive to the flag.** An Active member → `Admit { identity }`
   with the flag `false` **and** after flipping to `true` — proving a window flip never
   changes a member's verdict (the unit analog of "no member churn").
5. **Sticky departure preserved under flips.** A bound-but-inactive device → `Reject(NotActive)`
   with the flag `false` and after flipping to `true` (re-opening the window must not resurrect
   a removed/left member — mirrors
   `bootstrap_unknown_device_not_active_member_stays_rejected_when_joins_closed`).
6. **`accepts_joins()` tracks the live flag.** Returns `false`, then `true` after `store(true)`,
   then `false` after `store(false)`.
7. **`Clone` shares the cell.** Clone the gate; flip via the original `Arc<AtomicBool>` handle;
   the **clone's** `authorize`/`accepts_joins` observes the new value. (This matters because
   `Node` installs the gate as `Arc<dyn Admission>` and the derive `Clone` must not detach the
   cell.)
8. **Composition over `SnapshotAdmission`.** `new_dynamic(SnapshotAdmission::new(cell), flag)`:
   an Active member in the view → `Admit`; an unknown device → `AdmitProvisional`/`Reject`
   tracking the flag (sibling of `join_bootstrap_wraps_snapshot_admission`).

**e2e (in `join_e2e.rs`, `#[ignore]` online tier):**
9. **Mint-window-without-respawn + no `ConnEvent` churn (the issue's acceptance sketch).**
   - Bring up an admin `Node` **once** with `new_dynamic(inner, window)` where `window` starts
     `false`, plus one already-`Active` member peer connected to it.
   - With the window `false`, a fresh joiner's bootstrap dial is refused (no provisional
     admit; the joiner does not converge to `Active`).
   - `window.store(true)` (simulating an invite mint) **without respawning the admin node**;
     the joiner now completes the bootstrap and both peers fold the joiner `Active` — reusing
     `join_e2e`'s existing `valid_join` convergence check.
   - `window.store(false)` (redemption/expiry) closes the window: a second unknown device is
     refused again.
   - **Drain the admin's `conn_events()` across the whole sequence and assert the pre-connected
     member produced no disconnect/reconnect `ConnEvent` attributable to the flips** — the
     direct AC. Keep every await bounded via the file's `wait_until_contains`/`timeout`
     helpers so a wiring bug fails fast rather than hanging CI.

---

## 9. Security, privacy, reliability, performance

- **No widened authorization.** The dynamic flag governs only the liveness+privacy provisional
  admit; `gate_join` remains the membership authority on every peer (`admission.rs:277-280`
  invariant). A daemon that keeps the flag `false` when no invite is pending gets a **strictly
  smaller** exposure window than the "always-true" option 1 today, which is the privacy win the
  issue targets: the membership-DAG metadata surface is open to unknown devices only while an
  invite is actually pending.
- **Fail-safe default.** A caller that never flips the flag (starts `false`) is exactly a
  quiescent, no-strangers node; a caller that leaves it `true` is exactly today's always-hosting
  behaviour. Neither is a regression.
- **Reliability / back-compat.** Purely additive: `new`, `authorize`'s decision logic,
  `accepts_joins`'s contract, the trait, and the CLI/e2e call sites are unchanged. Existing
  tests pass without edits.
- **Performance.** `authorize` gains one relaxed atomic load (or a `bool` copy for `Fixed`)
  on the accept path — cheaper than `SnapshotAdmission`'s existing per-call `Mutex` lock, and
  far below any accept-path cost. No allocation on the hot path; the single `Arc` allocation is
  one-time at construction.
- **Concurrency.** The caller is the sole writer of the flag; multiple `authorize` readers +
  one writer over an `AtomicBool` is data-race-free by construction (no lock, no poisoning).

## 10. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| A consumer assumes flipping the flag *tears down* in-flight provisional bootstraps | low | low | Document explicitly: the flag governs **new** accept decisions only; an in-flight single-join bootstrap completes as it does today. Semantics equal the fixed `bool` at any instant. |
| Relaxed ordering surprises a consumer expecting instant cross-thread visibility | low | low | §5.3 doc: the flip is eventually-visible and both stale directions are benign; offer `Release`/`Acquire` as a caller choice, not a correctness requirement. |
| `#[derive(Debug)]` leaks something via the new field | none | — | `AcceptJoins` Debug prints `Fixed(bool)` or `Dynamic` + an `AtomicBool` (a public boolean); no secret material. Existing types already derive `Debug`. |
| Scope creep into a `Node`-level "swap admission" API or SDK invite-tracking | med | low | Explicitly out of scope (§3.3); the dynamism is confined to the gate's shared flag. |
| CLI accidentally migrated and its one-shot semantics change | low | med | §3.3: CLI stays on `new`; no `message.rs`/`cli.rs` edits in this issue. |

## 11. Acceptance criteria (mapped to the issue)

- **AC — window opens on mint without a respawn.** A resident host built with `new_dynamic`
  and a `false` flag refuses join-bootstrap; `store(true)` opens the window and a joiner
  completes the bootstrap **without restarting the session**. Tests #3 (unit), #9 (e2e).
- **AC — window closes on redemption/expiry.** `store(false)` returns the gate to
  no-strangers; a subsequent unknown device is refused. Tests #3, #9.
- **AC — connected peers observe no `ConnEvent` churn across the flips.** Grounded structurally
  by admission being accept-path-only (§5.4); asserted by the drained-`conn_events` check in #9
  and the member-insensitivity unit test #4.
- **AC — no behavioural regression.** `new`, the decision matrix, `accepts_joins`, the trait,
  and the CLI path are unchanged; `new_dynamic` equals `new` for any fixed flag value. Tests
  #1, #2, #5, plus the untouched existing suite.

## 12. Assumptions

1. The executing agent may modify `crates/iroh-rooms-net/src/admission.rs` and
   `.../tests/join_e2e.rs` (and optionally `docs/sdk-coverage.md`); `scripts/verify.sh` is the
   gate.
2. `Arc<AtomicBool>` is the right first shape — it matches the daemon use case and the
   upstreamed `DynamicJoinBootstrap` workaround, is lock-free, and preserves the type's derives.
   A `Fn() -> bool` generalization is deferred (OQ-1).
3. The caller owns and computes the pending-invite policy (mint→true, redeem/expire→false); the
   SDK does not track invites — consistent with the existing constructor doc.
4. The CLI's one-shot `--accept-joins` semantics are correct as-is and are intentionally left
   on the fixed constructor.
5. `Relaxed` ordering is acceptable for the flag (§5.3); no consumer needs the flip to
   establish a happens-before over other shared state.

## 13. Open questions

- **OQ-1 (`Arc<AtomicBool>` vs `Fn() -> bool`).** Ship `new_dynamic(inner, Arc<AtomicBool>)`
  now (recommended: matches the workaround + daemon use case, lock-free, `Debug`/`Clone`-clean)
  vs also/instead a `new_dynamic_fn(inner, Arc<dyn Fn() -> bool + Send + Sync>)` for a computed
  predicate. Recommendation: `AtomicBool` first; add the `Fn` arm later only if a consumer needs
  a derived predicate — it is an additive `AcceptJoins` variant, not another breaking change.
- **OQ-2 (constructor name).** `new_dynamic` (issue's wording, recommended) vs
  `with_dynamic_window` / `new_live`. Recommendation: keep `new_dynamic`.
- **OQ-3 (ergonomic handle).** Keep the caller-supplies-`Arc<AtomicBool>` shape (recommended,
  mirrors `SnapshotAdmission::new(cell)`) vs return a `(Self, JoinWindowHandle)` pair with a
  typed `open()/close()` setter. Recommendation: keep the raw `Arc<AtomicBool>` for the MVP; a
  typed handle is a later ergonomics-only follow-up.
- **OQ-4 (memory ordering default).** `Relaxed` (recommended, §5.3) vs `SeqCst` for
  conservative intuition. Recommendation: `Relaxed`, documented; correctness does not depend on
  the stronger ordering.
- **OQ-5 (does the e2e's no-churn assertion belong on the online tier or can a Loopback-only
  variant be non-ignored?).** The existing join tests are `#[ignore]`-gated; recommend keeping
  the new e2e on the same tier and relying on the deterministic unit tests (#3/#4) for the core
  invariant in the default `cargo test` run.

## 14. Definition of done

- `JoinBootstrapAdmission::new_dynamic(inner, Arc<AtomicBool>)` landed; `new`, `authorize`'s
  decision logic, the trait, and the CLI/e2e call sites unchanged; `accepts_joins()` reads the
  live flag.
- The type/constructor docs explain the daemon motivation, the per-request live read, the
  `Relaxed` choice, and the "no churn because admission is accept-path only" guarantee, and
  cross-link `SnapshotAdmission`.
- Unit tests #1–#8 (dynamic matrix, live flip, member-insensitivity, sticky-departure,
  `accepts_joins`-tracks-flag, clone-shares-cell, compose) and the ignored-tier e2e #9
  (mint-without-respawn + no-`ConnEvent`-churn) are present and green.
- `scripts/verify.sh` is green (fmt, clippy `-D warnings` pedantic, `--all-features` tests). No
  new dependency, no re-export churn, no protocol/schema/gate/authorization change.
