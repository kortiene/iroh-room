# Spec: Membership Fold Prototype

| | |
|---|---|
| **Issue** | #12 — [IR-0008] Implement membership fold prototype |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/feature, type/security, area/protocol, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §10.4 (Membership Validation), §10.5 (Invite Capability), §10.6 (Pipe Authorization), §13 (Security & Privacy) · `PHASE-0-SPIKE.md` Membership & Ordering §0, §1, §3 (full), §4, §5, §6, §7; Protocol Test Vectors §11, §13, §14, §15, §16, §17, §18, §19 |
| **Dependencies** | #6 — IR-0002 canonical signed event model (**landed**: `iroh-rooms-core::event`, `validate_wire_bytes` → `ValidatedEvent`, the deferred `MembershipOracle` trait + `RejectReason` variants). #8 — IR-0004 SQLite event store (**landed**: `iroh-rooms-core::store`, the `by_type` / `by_sender` / `heads` / `parents_of` query surface) is the **recommended substrate** but not a hard dependency (see D2). |
| **Status** | Implemented — landed (`iroh-rooms-core::membership`, issue #12 / IR-0008). |
| **Type** | Production code: a new **stateful** membership/authorization layer in `iroh-rooms-core`, downstream of the stateless validator. No transport/sync, no admin-tip advertisement, no CLI, no pipe/blob transport wiring. |

---

## 1. Summary

Implement the deterministic MVP **membership fold** and the **authorization checks** it powers
(`PHASE-0-SPIKE.md` Membership & Ordering §3, §5, §6, §7). This is the second stateful layer of the
Room Event Plane: it sits **downstream of the stateless validator** (#6) and turns a set of
`ValidatedEvent`s into (a) a per-event **log-validity verdict** and (b) a deterministic
**membership snapshot** that pipe/blob access decisions consult.

Four design choices (spike §0) make this tractable and are the load-bearing invariants this issue
implements:

1. **One immutable admin.** The genesis (`room.created`) signer is the sole authority for the whole
   MVP — the only writer of authorization state. There is never a second writer, so membership never
   needs Matrix-style multi-writer state resolution.
2. **Ancestor-stable validity.** Every event is judged against its **own fixed causal ancestors**,
   never the receiver's live state. The verdict is therefore identical on every peer and independent
   of arrival order — the property that makes "no permanent divergence" hold.
3. **Membership is a commutative causal fold.** Monotonic admin authorizations + a per-subject
   "causal heads, **Removed-dominates**" status rule + a deterministic **least-privilege** attribute
   merge is provably convergent and is a small, fully-specified state machine.
4. **Access control uses the current snapshot, not the ancestor view.** A log-valid event from a
   since-removed member is kept in the log but grants **zero capabilities** — the split that contains
   an equivocating "ignore-my-own-kick" member (spike §5).

The convergence guarantee this issue must deliver, in its **hedged** form (spike §0 — the
unqualified form is false and must not be claimed):

> **Any two honest peers that hold the IDENTICAL validated event set compute byte-identical
> membership state** — regardless of arrival order, restarts, or equivocation.

Out of scope here (sibling issues): transport/sync, backfill / anti-amplification (§4), admin-tip
**advertisement** and the cross-peer incompleteness detector / fail-closed-on-suspected-fork (§0),
the QUIC accept-handler wiring and tear-down-on-learn for pipes/blobs (§5 — the planes consume this
layer's snapshot), and CLI. This issue delivers the **fold + ancestor-stable authorization + the
snapshot and access-decision predicates** those layers call.

---

## 2. Background & current repository state

Read before implementing.

### 2.1 Source-of-truth docs

- **`PHASE-0-SPIKE.md` Membership & Ordering** is the normative design. The sections this issue
  implements verbatim:
  - **§0** — the honest convergence statement (set-completeness caveat). This issue implements the
    *same-set* guarantee; the cross-peer incompleteness detector (admin-tip advertisement,
    fail-closed) is the **sync** issue's, but this layer must expose the hooks it needs (the
    snapshot, the per-subject status, "is this decision removal-sensitive").
  - **§1** — identity/signing/envelope; **self-parent rule** (each author's events form a per-author
    hash chain; the admin chain defines `admin_seq`); **genesis-descent rule**.
  - **§3 (all subsections)** — the membership model: roles (§3.1), the five membership event types
    (§3.2), grow-only joins + per-subject departure status (§3.3), **the deterministic fold** (§3.4),
    the **authorization gate** (§3.5), leave/removal without key rotation (§3.6), **sticky departure
    / leave AND removal consume authorizations** (§3.7), and **deterministic concurrent-attribute
    resolution** — least-privilege + lowest-`event_id` (§3.8).
  - **§4** — three-stage pipeline; **stage 3 (semantic/authorization) is ancestor-based**.
  - **§5** — pipe/blob authorization at connect-accept time uses the **current snapshot**; the
    **log-validity vs access-control split**.
  - **§6** — invite tickets as **log-verifiable, key-bound capabilities**; capability-hash recompute;
    **log-only expiry** (no local clock); MVP limitations (no native revocation, single-subject).
  - **§7** — conflict/fork/equivocation resolution: the unifying principle.
- **Protocol Test Vectors** that become this issue's conformance tests: **§11** (concurrent
  invite/join vs kick → Removed), **§13** (non-member rejection), **§14** (non-admin admin action →
  `insufficient_role`), **§15** (stale/expired invite, bad capability), **§16** (blob serve gate),
  **§17** (pipe connect gate, `allowed_members ∩ Active`, no default-all), **§18** (concurrent
  attributes → least-privilege + lowest `event_id`), **§19** (leave-then-rejoin needs a fresh
  post-leave invite). Vectors' concrete `event_id`s for §18–§19 are generated by the harness; the
  **rule and required verdict are normative**.
- **`PRD.v0.3.md`:** §10.4 (creator = admin; only admin issues invites; join = valid ticket + signed
  `member.joined`), §10.5 (key-bound tickets, expiry from the start, revocation deferred), §10.6
  (pipes default to **explicit** allowed members, no default-all), §13.1–13.4 (membership checks,
  invite tickets as scoped capabilities, **no key rotation in MVP**).

### 2.2 Landed code this layer builds on (dependency #6, `crates/iroh-rooms-core/src/event/`)

- **`validate::ValidatedEvent { event_id: EventId, event: SignedEvent, wire: WireEvent, flags:
  Vec<Flag> }`** and **`validate_wire_bytes(bytes, &ValidationContext) -> Result<ValidatedEvent,
  RejectReason>`** — the **stateless** pipeline (Event Protocol §6 steps 1–6, 9 (structural), 10).
  This layer's input is a `ValidatedEvent` (stateless-valid); it adds steps 7–8 and the fold.
- **`reject::MembershipOracle`** — the **already-frozen trait** this issue must implement
  (`crates/iroh-rooms-core/src/event/reject.rs`):
  ```rust
  pub trait MembershipOracle {
      fn bound_device(&self, room_id: &RoomId, sender_id: &IdentityKey) -> Option<[u8; 32]>;
      fn authorize(&self, room_id: &RoomId, sender_id: &IdentityKey, event_type: &str)
          -> Result<(), RejectReason>;
  }
  ```
  Its doc already says: *"no implementation and no call site exist in this issue [#6] … The sibling
  membership/authorization issue provides the implementation and the wrapping
  `validate_with_membership` entry point."* **This is that issue.**
- **`reject::RejectReason`** deferred variants this issue is the first to **emit**: `UnboundDevice`,
  `NotAMember`, `InsufficientRole`, `ExpiredInvite`, `BadCapability` (the stateless layer defines but
  never produces them). Stable §8 codes: `unbound_device`, `not_a_member`, `insufficient_role`,
  `expired_invite`, `bad_capability`. The enum is `#[non_exhaustive]`.
- **`reject::Flag`** deferred variants this issue may attach: `Equivocation`, `FromRemovedMember`
  (`equivocation`, `from_removed_member`). Flags **never** change a verdict, the validated set,
  ordering, or any authorization/expiry decision.
- **`content`** typed structs (the fold reads these directly): `MemberInvited { invite_id,
  capability_hash, role, invitee_key, expires_at, invitee_hint }`, `MemberJoined { via_invite_id,
  capability_secret, role, device_binding, display_name }`, `MemberLeft { member_id, reason }`,
  `MemberRemoved { member_id, removed_by, reason, device_binding }`, `RoomCreated { …, admins,
  device_binding }`, `PipeOpened { pipe_id, owner_id, owner_endpoint, allowed_members, expires_at, …
  }`, `FileShared { blob_hash, … }`. **Stateless layer already enforces** the bytes-local cross-field
  rules (`content::check_field_rules`): `member.left.member_id == sender_id`, `member.removed`
  `removed_by == sender_id` and `member_id != sender_id`, `room.created.admins == [sender_id]`,
  `pipe.opened.owner_id == sender_id`. The fold need not re-check those; it adds the **stateful** gate
  (admin-signer, invite-liveness, capability) on top.
- **`content::capability_hash(room_id, invite_id, secret) -> [u8; 32]`** — already exposed
  *specifically for this layer* ("Exposed for the deferred membership layer to match a join's secret
  against an invite"). The join gate calls this.
- **`signed::SignedEvent`** (eight fields incl. `prev_events: Vec<EventId>`), **`ids::{EventId,
  RoomId}`** — `EventId` derives `Ord` over the **raw 32 digest bytes**, which is **exactly** the
  protocol's `event_id` tie-break (§2.1 / §3.8). Use `EventId`'s natural `Ord` for the least-privilege
  tie-break and timeline tie-break; no custom comparator needed.
- **`keys::{IdentityKey, DeviceKey}`** — distinct 32-byte newtypes; authorization is tracked against
  `IdentityKey` (`sender_id`), QUIC/ACL identity is the `DeviceKey` (`device_id` == `EndpointId`).

### 2.3 Landed store substrate (dependency #8, behind the `store` feature)

`store::EventStore` already provides the exact query surface the spike's §3.4 fold names: `by_type`
and `by_sender` (membership-fold inputs, ordered `(lamport, event_id)` with `NULL`-lamport last),
`heads`, `parents_of` / `children_of` / `missing_parents`, `admin_chain_tip`, `room_tail`. It also
derives and stores `lamport` and `admin_seq`. Its module doc states the fold and the `members` cache
are explicitly **out of its scope** and are *this* issue's: *"Out of scope here … the membership fold
and `members` cache … The store persists events and records dangling parent edges so those layers can
be built on a frozen substrate."* The `members`, `sync_state`, `trust_decisions` tables named in PRD
§12 are **derived caches rebuildable by re-folding `events`** — i.e. computed by this layer.

**There is no membership, authorization, or fold code in the workspace today.** This issue introduces
it.

### 2.4 Workspace conventions (must follow)

- `scripts/verify.sh` runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  **--all-features** -D warnings`, `cargo test --workspace --all-targets --all-features`. New code
  must be **pedantic-clean**.
- Root `Cargo.toml`: `unsafe_code = "forbid"`, Clippy `all` + `pedantic` at `warn`. **No panics on
  adversarial input** in non-test code (no `unwrap`/`expect`/slice on untrusted data).
- Error enums are **hand-rolled** (`Display` + `std::error::Error`); no `thiserror`. The protocol
  rejection type is the shared `RejectReason` — **reuse it**, do not introduce a parallel taxonomy.
- Fixtures are built with `SigningKey::from_seed(&[seed; 32])` and the `genesis()` / `seal()` helpers
  in `tests/e2e_lifecycle.rs`. Reuse that pattern for the fold tests.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A new **`membership`** module in `iroh-rooms-core` (not feature-gated — depends only on `event`
   types; see D1) implementing the deterministic fold and authorization.
2. **Ancestor-stable authorization** (§3.5 / §4 stage 3): per-event log-validity judged **only**
   against the event's transitive `prev_events` ancestors, producing an accept (joins the validated
   set) or a typed `RejectReason`.
3. The **authorization gate** for the five membership event types:
   - `member.invited(X)` / `member.removed(X)` — valid iff signer == **the single immutable admin**
     (genesis signer); `member.removed` additionally `X != admin` (already bytes-checked).
   - `member.left(X)` — valid iff signer == X (already bytes-checked); inert if X holds no
     authorization to consume.
   - `member.joined(X)` — valid iff signer == X **and** it causally descends from a **still-live,
     key-bound** admin authorization for X whose **capability secret matches** and whose **log-only
     expiry** has not passed and whose **role matches** (§3.5 / §6).
4. **Key-bound invite capability** verification (§6): recompute `capability_hash(room_id,
   via_invite_id, capability_secret)` and match the cited invite; **bearer/open tickets are excluded
   from MVP** (§6 path B) — a join under a key with no naming invite fails the gate (ban-evasion
   impossible).
5. The **membership fold** (§3.4) over the validated set: per subject X, collect authorized touching
   events → causal heads → **Removed-dominates** status (Removed > Active > Invited) → attributes.
6. **Sticky departure** (§3.7): a `member.removed(X)` **or** `member.left(X)` consumes every admin
   authorization for X causally prior to it; only a **fresh** invite causally **after** the departure
   (plus a join descending from it) can re-admit X.
7. **Deterministic concurrent-attribute resolution** (§3.8): on conflicting concurrent heads,
   **least-privilege** wins (`agent` < `member` < `admin`; capability scope = intersection), tie-broken
   by **lowest `event_id`** (bytewise). This is the one same-set divergence the bare status fold
   leaves open and MUST be closed.
8. A **`MembershipSnapshot`** value: the deterministic fold result over the local validated set —
   per-identity status + role + bound device, plus a device→identity reverse map for QUIC identity
   resolution (§5).
9. **Access-decision predicates** that consume the snapshot (§5): blob-serve gate and pipe-connect
   gate (`allowed_members ∩ Active`, no default-all), as **pure functions** the Blob/Pipe planes call.
   (The QUIC accept-handler wiring and tear-down-on-learn live in those planes; this issue provides
   the decision functions and the snapshot they evaluate.)
10. **Implement `event::reject::MembershipOracle`** for an ancestor-scoped view, and add the
    **`validate_with_membership(bytes, ctx, &impl MembershipOracle)`** wrapper that completes Event
    Protocol §6 steps 7–8 on top of the stateless `validate_wire_bytes` (the frozen surface named in
    the trait's doc).
11. **`bound_device` enforcement** (§6 step 7): events that do **not** carry a self-contained
    `device_binding` (`message.text`, `file.shared`, `pipe.*`, `agent.status`, `member.invited`,
    `member.left`) must use the `device_id` bound to their `sender_id` in the ancestor-view membership
    — else `unbound_device`.
12. Unit/integration tests mapping 1:1 to the issue Acceptance Criteria, the issue Test Plan, and the
    spike conformance vectors §11/§13/§14/§15/§16/§17/§18/§19.

### 3.2 Out of scope (sibling issues — do **not** implement here)

- **Transport / sync** and the `sync_state` cache (heads, parked-orphan set, recent-window cursor,
  highest known admin tip), backfill, **anti-amplification** (§4 stage 2 bounds), recent-history
  windowing. This layer assumes events are handed to it; it does *buffer* causally-incomplete events
  (a parent is missing) but does **not** fetch them.
- **Admin-tip advertisement and the cross-peer incompleteness detector** (§0): comparing admin tips
  across peers, detecting two distinct tips at the same `admin_seq`, **fail-closed-on-suspected-fork**.
  This issue delivers the *same-set* convergence and the *local* equivocation flag (one key authored
  two concurrent events); the room-wide detection + fail-closed policy is the sync issue's. This layer
  exposes the hook (per-subject "removal-sensitive" classification) it will use.
- **QUIC accept handlers, ALPN wiring, `iroh-blobs provider::events` config, tear-down-on-learn,
  `pipe.connect.rejected` audit emission** (§5) — the Pipe/Blob planes. This issue provides the
  **pure access-decision functions** and the snapshot; the planes wire them to live connections.
- **Key rotation, invite revocation lists, `max_uses` counters, open/bearer tickets** (§6 / PRD
  §13.4–13.5) — post-MVP. MVP invites are key-bound, single-subject, reusable-by-that-key-until-expiry.
- **CLI** (`room members`, `room kick`, …), the persisted `members` / `trust_decisions` tables, and
  exports. The fold is computed; persisting its result as a derived cache is a follow-up (the store
  already guarantees it is rebuildable).
- **Self-parent rule *enforcement* as a hard reject and full `admin_seq` re-derivation.** The store
  already derives `admin_seq`; this layer *reads* it where useful and detects equivocation as an
  advisory flag, but does not reject events for a missing self-parent (a non-fatal structural lint
  deferred with admin-tip work).

### 3.3 Why the split is safe

Per spike §0/§4, all membership/timeline state is a **pure function of the delivered validated set**.
Building the deterministic fold + ancestor-stable authorization first — over an in-memory set of
`ValidatedEvent`s — gives a self-contained, conformance-tested core whose output is identical on any
peer holding the same set. Sync (which *equalizes* the set) and the planes (which *consume* the
snapshot) bolt on without changing a single fold rule. The store already proved its derived caches
(incl. a future `members` cache) are rebuildable by re-folding `events`, so persistence is additive.

---

## 4. Domain model

```rust
/// A subject's current membership status (spike §3.4). Ordered so the
/// Removed-dominates rule is a `max`: Invited < Active < Removed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status { Invited, Active, Removed }

/// A participant role (spike §3.1). Ordered LEAST→MOST privileged so the
/// least-privilege merge (§3.8) is a `min`: Agent < Member < Admin.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role { Agent, Member, Admin }

/// The resolved state of one subject after the fold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub identity: IdentityKey,
    /// The device bound to this identity (from the join's / genesis device_binding).
    /// `None` for an Invited-only subject that has not joined.
    pub device: Option<DeviceKey>,
    pub status: Status,
    /// Resolved by the least-privilege + lowest-event_id merge (§3.8).
    pub role: Role,
}

/// The deterministic fold result over a validated event set — the value pipe/blob
/// access decisions consult (spike §5). A pure function of the set.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MembershipSnapshot {
    pub room_id: RoomId,
    pub admin: IdentityKey,                 // the immutable genesis signer
    members: BTreeMap<IdentityKey, Member>, // deterministic iteration order
    by_device: BTreeMap<DeviceKey, IdentityKey>, // QUIC EndpointId → identity (§5)
}

impl MembershipSnapshot {
    pub fn status(&self, id: &IdentityKey) -> Status;        // default Invited? -> see note
    pub fn is_active(&self, id: &IdentityKey) -> bool;       // status == Active
    pub fn role(&self, id: &IdentityKey) -> Option<Role>;
    pub fn member(&self, id: &IdentityKey) -> Option<&Member>;
    /// Resolve a QUIC-authenticated device key to its bound identity (§5).
    pub fn identity_of_device(&self, dev: &DeviceKey) -> Option<&IdentityKey>;
    pub fn active_members(&self) -> impl Iterator<Item = &Member>;
}
```

> **Note (unknown subjects).** `status()` for an identity with **no** membership events returns a
> well-defined "not a member" answer. Model it explicitly — either `Status` gains the bottom case or
> `status()` returns `Option<Status>` and `is_active` is the only predicate the planes use. **Default
> deny**: any identity not resolvably `Active` is denied (§5). Pin the exact shape in D5 / Open Q2.

---

## 5. Key design decisions

### D1 — The fold is a new **non-feature-gated** `membership` module over in-memory `ValidatedEvent`s

Add `crates/iroh-rooms-core/src/membership/` and `pub mod membership;` in `lib.rs`. It depends only
on `event` types (`ValidatedEvent`, `SignedEvent`, `content::*`, `ids::*`, `keys::*`, `RejectReason`,
`Flag`, `MembershipOracle`) — **no `rusqlite`, no `store` feature**. Rationale:

- The issue's **only hard dependency is #6** (`ValidatedEvent`). The convergence theorem is stated
  over "the identical *validated event set*" — a pure in-memory fold is the most faithful, most
  testable embodiment, and lets the acceptance tests feed the *same set in two different orders* and
  assert byte-identical state with zero I/O.
- It matches the frozen `MembershipOracle` trait, which lives in `event` (no store dependency).
- The **store integration is a thin adapter** (D2): feed `store.by_type(...)` / `by_sender(...)` /
  the room's events into the same fold. Keeping the fold store-independent means `--all-features` and
  the default feature set both exercise it.

*Alternative considered:* put the fold behind the `store` feature and read directly from SQL. Rejected
— it would make the convergence core untestable without a database, couple two prototypes, and fight
the trait's location. The store stays the *substrate that supplies events*, not the fold engine.

### D2 — Input is a stream/set of `ValidatedEvent`; the store is an optional source

The fold's public entry accepts already-stateless-validated events:

```rust
RoomMembership::new(room_id, ctx) -> Self
RoomMembership::ingest(&mut self, ev: ValidatedEvent) -> Ingest   // one event, any order
RoomMembership::from_events(room_id, ctx, evs: impl IntoIterator<Item = ValidatedEvent>) -> Self
```

A trivial, separately-testable adapter builds the input from the store
(`store.by_type` ∪ `store.by_sender` ∪ everything in the room, or simply the full room scan), but
the fold itself never touches SQL. `ctx` carries the `RoomId` and the optional `now_ms` (used **only**
for the advisory clock-skew flag — never for expiry, which is log-only, §6).

### D3 — Ancestor-stable validity via an **ancestor-scoped oracle**, completing the frozen `MembershipOracle` surface

The crux. Each event's log-validity is decided against **only its transitive ancestors** (§4 stage
3). To honor this *and* the frozen `authorize(room, sender, event_type)` signature (which takes no
ancestor parameter), the **oracle instance is the ancestor view**:

1. Process events in a **causal (topological) order** — ascending `(lamport, event_id)`, which is a
   linear extension of the DAG (spike §2.2). Parents are classified before children.
2. For each new event `e`, build an `AncestorView<'_>` over `e`'s already-classified ancestors and
   call `validate_with_membership(e.wire.signed-derived bytes, ctx, &ancestor_view)` — or, equivalently
   for events already past `validate_wire_bytes`, run the **steps 7–8 tail** directly against the view.
3. `AncestorView` implements `MembershipOracle`:
   - `bound_device(room, sender)` → the device bound to `sender` in the **ancestor-view** fold
     (genesis device for the admin; the join's device for a member). Used for §6 step 7 on the types
     that carry no self-contained binding.
   - `authorize(room, sender, event_type)` → the §6 step-8 gate evaluated in the ancestor view:
     membership-event gate (admin-only for invite/remove; self for left) for membership types; "sender
     is **Active** in the ancestor-view fold" for `message.text`/`file.shared`/`pipe.*`/`agent.status`.
4. **The join-capability check is membership-internal, not via the trait** (the trait signature
   carries no `content`, so it cannot see `via_invite_id`/`capability_secret`). `ingest` runs the full
   §3.5 join gate against the ancestor view using the decoded `MemberJoined` content.

`validate_with_membership` is added to `event::validate` (sibling of `validate_wire_bytes`, named in
the trait doc). It is store-independent and reusable by any caller that can supply a `MembershipOracle`.

> **Why ancestor-stable matters (the security property):** authorization judged against fixed
> ancestors yields the **same verdict on every peer regardless of arrival order**, so honest peers
> with the same set never permanently disagree (spike §4). Judging against *live* state would make
> validity arrival-order-dependent and break convergence — the bug class this design exists to avoid.

### D4 — Sticky departure is an **ancestor-reachability** predicate, not a timestamp

A `member.removed(X)`/`member.left(X)` **consumes** an admin authorization `inv` for X iff `inv` is a
causal ancestor of the departure (§3.7). A join `e` citing `inv` is authorized iff **no** departure of
X in `ancestors(e)` has `inv` in **its** ancestors. Concretely, the join gate is:

```
join e by X via (via_invite_id, capability_secret), role r_join, at created_at:
  A = ancestors(e)                                  # transitive prev_events, excluding e
  inv = the member.invited in A with invite_id == via_invite_id
        AND invitee_key == X AND sender == admin
        (else  -> BadCapability)                    # no naming invite => ban-evasion blocked (§6)
  if capability_hash(room_id, via_invite_id, capability_secret) != inv.capability_hash
        -> BadCapability
  if inv.expires_at is Some(t) AND e.created_at > t   # LOG-ONLY, both signed fields (§6)
        -> ExpiredInvite
  if r_join != inv.role
        -> InsufficientRole                          # join role must equal invite role
  if EXISTS d in A: (d is member.removed(X) or member.left(X)) AND inv in ancestors(d)
        -> ExpiredInvite                             # consumed by causally-prior departure (§3.7)
  else -> authorized
```

This is **symmetric for kick and voluntary leave** (both consume), which closes the
self-rejoin-after-leave hole (spike §3.7 / vector §19): a member cannot manufacture a join that
descends past their own `member.left` while reusing the old invite. Re-admission always needs a
**fresh** `member.invited(X)` causally **after** the departure.

### D5 — The fold: causal heads + Removed-dominates + least-privilege, all deterministic

For each subject X, over the **authorized** events touching X:

1. `touch(X)` = authorized `member.invited(invitee_key==X)` ∪ `member.joined(sender==X)` ∪
   `member.left(member==X)` ∪ `member.removed(member==X)`. The admin (genesis signer) is seeded
   `Active`/`Admin` and is never in any subject's removable set (the gate forbids `removed(admin)`).
2. **Causal heads** of `touch(X)` = events in the set with **no other event of the set among their
   causal descendants** (spike §3.4). Compute via the DAG: `h` is a head iff no `g ∈ touch(X)\{h}`
   has `h ∈ ancestors(g)`.
3. **Status** (Removed-dominates): if any head is `removed`/`left` → **Removed**; else if any head is
   `joined` → **Active**; else (invites only) → **Invited**. Equivalent to `max` over the heads'
   `Status` contributions with the `Invited < Active < Removed` ordering — which is why `Status` is
   `Ord`.
4. **Attributes** (§3.8): resolve `role` over the *heads that carry a role* (invites and joins) by
   **least-privilege** (`min` over `Role`, since `Agent < Member < Admin`); tie-break (equal
   privilege, differing other attributes) by **lowest `event_id`** (bytewise — `EventId`'s natural
   `Ord`). Capability scope (when scopes exist beyond `role`) resolves to the **intersection**; for
   MVP, `role` is the only concrete attribute, and the intersection hook is documented for forward
   compatibility.

The fold is **commutative and deterministic**: heads, the status `max`, and the attribute `min` /
`event_id` tie-break are all order-independent set operations ⇒ **identical membership on every peer
holding the same set, and across restarts** (the §3.4 / §0 guarantee). This is the property the
"concurrent join vs kick → Removed" acceptance test pins.

### D6 — Log-validity vs access-control split: the snapshot, not the ancestor view, gates access

Two distinct authorization contexts, kept rigorously separate (spike §5):

- **Log-validity** (D3) uses the **ancestor view** → decides whether an event enters the validated
  set. A chat/file/pipe event from a member who was Active *in its own ancestor view* is **valid for
  inclusion** even if that member is **now Removed** (the log is append-only).
- **Access control** uses the **current `MembershipSnapshot`** (the fold over the *whole local
  validated set*) → decides pipe/blob connect. A since-removed member's log-valid `file.shared` /
  `pipe.opened` grants **zero capabilities**.

This split is what contains an equivocating "ignore-my-own-kick" member: they may scribble on their
own fork (log-valid in ancestor view) but converge to **Removed** in every peer's snapshot and get no
access. Events authored by a now-Removed member are tagged with the advisory `Flag::FromRemovedMember`
for UI attribution; the flag changes **nothing** about validity or access.

### D7 — Access decisions are pure functions the planes call

Expose, as free functions / methods that take the snapshot + the relevant validated content:

```rust
/// Blob serve gate (spike §5 / vector §16). The connecting identity must be Active,
/// and the hash must be referenced by a valid `file.shared` from an Active member.
pub fn blob_serve_allowed(
    snapshot: &MembershipSnapshot,
    connecting: &DeviceKey,            // QUIC-authenticated EndpointId
    blob_hash: &HashRef,
    file_shares: &dyn Fn(&HashRef) -> Option<IdentityKey>, // resolves hash -> authoring identity
) -> BlobDecision;                     // Serve | Reject(Permission)

/// Pipe connect gate (spike §5 / vector §17). ALL must hold (no default-all, PRD §13.2):
/// remote identity Active; ∈ allowed_members; pipe owner Active; (caller supplies
/// no-pipe.closed and expiry checks — see note). `now_ms` consulted ONLY to deny.
pub fn pipe_connect_allowed(
    snapshot: &MembershipSnapshot,
    connecting: &DeviceKey,
    pipe: &PipeOpened,
    now_ms: Option<u64>,
) -> PipeDecision;                     // Accept | Reject(reason)
```

The membership layer owns the **identity-resolution + Active + allowed_members ∩ Active + owner-Active**
predicates. The **no-`pipe.closed`-causally-known** and live-connection **tear-down-on-learn** are the
Pipe plane's (it owns pipe lifecycle state); the function takes them as inputs or the plane composes
them. `expires_at` is the **one** place wall-clock is consulted and only to **deny** (fail-closed) —
mirroring the stateless layer's advisory-only clock handling.

### D8 — Reuse `RejectReason` / `Flag`; no new taxonomy

Emit the already-defined deferred variants: `InsufficientRole` (non-admin invite/remove; join role
mismatch), `BadCapability` (no naming invite / secret mismatch), `ExpiredInvite` (consumed by
departure, or log-only expiry passed), `NotAMember` (author not Active in ancestor view for a
non-membership event), `UnboundDevice` (device_id ≠ bound device). Attach `Equivocation` /
`FromRemovedMember` flags. Do **not** add a parallel membership-error enum; the fold's *outcome* type
(`Ingest::Accepted { flags } | Ingest::Rejected { reason }`) wraps `RejectReason`.

---

## 6. The membership fold algorithm (normative)

### 6.1 Pipeline placement (spike §4)

```
wire bytes ──validate_wire_bytes (#6, stateless: steps 1–6, 9-structural, 10)──▶ ValidatedEvent
ValidatedEvent ──RoomMembership::ingest──▶
   stage 2  causal readiness: if any prev_event is not yet ingested, BUFFER keyed by the
            missing parent(s); do NOT reject (out-of-order tolerance, §4). Re-process buffered
            children when a parent arrives. (Backfill/fetch is the sync issue's; the fold only buffers.)
   stage 3  semantic/authorization (ANCESTOR-BASED, D3): run the per-type gate against the
            ancestor view; on pass, add to the validated set and update the fold; on fail, drop +
            record the typed RejectReason.
```

`lamport` (derived, `genesis=0`, `1+max(parents)`) gives the processing order and is recomputed from
signed `prev_events` (or read from the store when present). The membership **sub-DAG is never
windowed** — genesis + the full admin chain + all `member.*` for relevant subjects are always retained
(a hard invariant the sync layer must honor; this layer assumes it).

### 6.2 Per-type authorization gate (stage 3)

| Event type | Gate (evaluated in the event's ancestor view) | Reject code on failure |
|---|---|---|
| `room.created` | exactly one genesis per room; `admins == [sender]` (bytes-checked); establishes admin + seeds creator `Active/Admin` | — (stateless handles structure) |
| `member.invited(X)` | `sender == admin` | `insufficient_role` |
| `member.removed(X)` | `sender == admin` **and** `X != admin` (bytes-checked) | `insufficient_role` |
| `member.left(X)` | `sender == X` (bytes-checked); inert if X has no live authorization | — (valid; may be a no-op) |
| `member.joined(X)` | the full §3.5/D4 join gate (live key-bound invite + capability + log-only expiry + role match) | `bad_capability` / `expired_invite` / `insufficient_role` |
| `message.text`, `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status` | `sender` is **Active** in ancestor view **and** `device_id == bound_device(sender)` | `not_a_member` / `unbound_device` |

A failing event is **dropped and recorded** (a protocol-violation audit entry); it never affects
state (spike §3.5). `pipe.opened.owner_id == sender` and the other bytes-local cross-field rules are
already enforced upstream by `content::check_field_rules`.

### 6.3 The fold (stage 3 output → snapshot)

After each accepted event, recompute (or incrementally update) the per-subject state per D5 and the
`by_device` reverse map. The snapshot is **always** the fold over the **entire** current validated
set, so it reflects "the current local membership snapshot" (§5) that access decisions consult.

### 6.4 Worked convergence example (acceptance: concurrent join vs kick → Removed)

Fixtures mirror spike Test Vectors (Dave invited then forked):

```
E_create (Alice genesis, admin)
  └─ E_inv_dave   (member.invited, Alice, invitee_key=dave, key-bound)
       ├─ E_join_dave (member.joined, Dave; valid — invite live in its ancestor view)   [branch A]
       └─ E_kick_dave (member.removed, Alice; valid — admin)                            [branch B]
```

`touch(dave) = {E_inv_dave, E_join_dave, E_kick_dave}`; causal heads = `{E_join_dave, E_kick_dave}`
(neither descends from the other). A head is `removed` ⇒ **Removed-dominates** ⇒ `status(dave) =
Removed` on **every** peer holding all three events, regardless of whether it saw join-then-kick or
kick-then-join. Dave gets zero capabilities (blob/pipe gates deny). Re-admission requires a fresh
`member.invited(dave)` causally **after** `E_kick_dave`; replaying the stale `E_inv_dave`/`E_join_dave`
cannot resurrect him (D4 sticky departure). This is the IR-0008 headline acceptance criterion and the
spike's vector §11/§18.

---

## 7. Public API surface (`membership` module)

Synchronous, no panics on stored/decoded bytes. Names indicative; keep them rustdoc-linked to the
spike sections.

```rust
pub enum Status { Invited, Active, Removed }
pub enum Role { Agent, Member, Admin }
pub struct Member { pub identity: IdentityKey, pub device: Option<DeviceKey>,
                    pub status: Status, pub role: Role }
pub struct MembershipSnapshot { /* §4 */ }

/// Outcome of ingesting one event into the fold.
pub enum Ingest {
    /// Accepted into the validated set; advisory flags (e.g. from_removed_member, equivocation).
    Accepted { event_id: EventId, flags: Vec<Flag> },
    /// Failed the stateful gate; dropped, never affects state.
    Rejected { event_id: EventId, reason: RejectReason },
    /// Causally incomplete (a prev_event is not yet ingested); buffered, not an error (§4).
    Buffered { event_id: EventId, missing: Vec<EventId> },
}

pub struct RoomMembership { /* DAG + per-event verdicts + current fold */ }

impl RoomMembership {
    pub fn new(room_id: RoomId) -> Self;
    /// Ingest one stateless-validated event (any order); updates the fold.
    pub fn ingest(&mut self, ev: ValidatedEvent) -> Ingest;
    /// Convenience: fold a whole set (used by tests and the store adapter).
    pub fn from_events(room_id: RoomId, evs: impl IntoIterator<Item = ValidatedEvent>) -> Self;

    /// The current deterministic fold over the local validated set (§5 / §3.4).
    pub fn snapshot(&self) -> MembershipSnapshot;
    /// The ancestor-scoped MembershipOracle for an already-ingested event (for re-validation).
    pub fn ancestor_view(&self, event_id: &EventId) -> Option<AncestorView<'_>>;
}

/// The §6 steps 7–8 wrapper completing the stateless validator (frozen-surface entry).
pub fn validate_with_membership(
    bytes: &[u8],
    ctx: &ValidationContext,
    oracle: &impl MembershipOracle,
) -> Result<ValidatedEvent, RejectReason>;   // added to event::validate

/// Access-decision predicates the Blob/Pipe planes call (§5, D7).
pub fn blob_serve_allowed(/* … */) -> BlobDecision;
pub fn pipe_connect_allowed(/* … */) -> PipeDecision;
```

`AncestorView<'_>` implements `event::reject::MembershipOracle` (D3).

---

## 8. Test strategy

All tests under `scripts/verify.sh` (fmt + clippy `-D warnings` pedantic + test, `--all-features`).
Build real `ValidatedEvent`s by reusing the **`tests/e2e_lifecycle.rs`** helpers (`sk(seed)`,
`genesis()`, `seal()`, `ctx()`); each event's validator-returned `event_id` feeds the next
`prev_events`. Unit tests in-module (`#[cfg(test)]`); an integration file
`tests/membership_fold.rs`. Determinism tests feed the **same set in shuffled order** and assert
byte-identical snapshots.

Mapping to the issue **Acceptance Criteria**, **Test Plan**, and the spike **conformance vectors**:

1. **Admin can invite and remove** (AC1) — genesis Alice; `member.invited(bob)` and
   `member.removed(carol)` by Alice both **Accepted**; snapshot reflects Bob `Invited`→(after join)
   `Active`, Carol `Removed`.
2. **Non-admin invite/remove rejected** (AC2 / vector §14) — Bob (member) authors `member.invited` and
   `member.removed(carol)` → both `Ingest::Rejected { insufficient_role }`; state unchanged. (A
   `member.left{member_id=bob}` by Bob, by contrast, is Accepted — self-leave.)
3. **Join requires a valid key-bound invite capability** (AC3 / vector §15) — (a) join citing a
   matching `(via_invite_id, capability_secret)` whose recompute equals the invite's
   `capability_hash` → Accepted, Bob `Active`; (b) wrong secret → `bad_capability`; (c) join under a
   key with **no naming invite** → `bad_capability` (ban-evasion blocked, §6); (d) `expires_at` in the
   past vs the join's signed `created_at` → `expired_invite`; (e) join `role` ≠ invite `role` →
   `insufficient_role`.
4. **Leave and removal consume prior invitations** (AC4 / vector §19) — after `member.left(bob)` (or
   `member.removed(bob)`), a new `member.joined` that **re-cites the original invite** but descends
   from the departure → `expired_invite`; a join descending from a **fresh post-departure invite** →
   Accepted. Assert symmetry for both leave and kick.
5. **Concurrent join/kick converges to Removed for identical event sets** (AC5 / vector §11/§18) —
   build the §6.4 fork; ingest in order **A-then-B** into one `RoomMembership` and **B-then-A** into
   another; assert **both** snapshots have `status(dave) == Removed`, byte-identical `Member`, and
   identical `role`. Repeat with several shuffles. **The convergence headline.**
6. **Current snapshot drives pipe/blob access** (AC6 / vectors §16/§17) — with snapshot `{Alice:
   Active/Admin, Bob: Active, Carol: Active, Dave: Removed}`:
   - `blob_serve_allowed` for Bob's `file.shared` hash: **Carol Serve, Dave Reject(Permission),
     Mallory Reject**; an unreferenced hash → Reject(Permission).
   - `pipe_connect_allowed` for Bob's `pipe.opened{allowed_members=[alice,bob]}`: **Alice Accept**,
     **Carol Reject** (Active but ∉ allowed_members — no default-all), **Dave Reject** (Removed),
     **Mallory Reject** (not a member).
7. **Stale invite replay** (Test Plan / vector §15) — replaying `E_inv_dave`/`E_join_dave` after
   `E_kick_dave` is in the set does not change `status(dave)` from `Removed` (idempotent + sticky).
8. **Non-member event rejection** (vector §13) — Mallory (never invited) authors a `message.text`
   citing a real parent → `Ingest::Rejected { not_a_member }`; dropped, snapshot unchanged.
9. **Ancestor-stable validity** (vector §11 reasoning) — a `member.joined` validated against an
   ancestor view in which its invite is live → Accepted; the **same** join validated in a view where a
   causally-prior departure consumed the invite → `expired_invite`. Verdict depends only on ancestors,
   not arrival order: ingest the join before vs after unrelated concurrent events → identical verdict.
10. **Log-validity vs access-control split** (vector §16/§17 + §5) — a `file.shared` authored by Bob
    while Active is **Accepted** (log-valid) and tagged `from_removed_member` **after** Bob is later
    removed; `blob_serve_allowed` for that hash then **denies** (zero capabilities), proving access
    uses the current snapshot, not the ancestor view.
11. **`bound_device` enforcement** — a `message.text` whose `sender_id` is Bob but `device_id` is a
    key never bound to Bob → `unbound_device`.
12. **Determinism / restart** — `from_events(set)` over a shuffled set and an incremental
    `ingest`-one-at-a-time sequence produce **identical** snapshots; an independent re-fold of the
    same set is byte-identical (mirrors the store's rebuild-determinism oracle).
13. **No panic on adversarial input** — malformed/contradictory but stateless-valid events (e.g. a
    join citing a non-existent invite id, a cycle attempt, 20 parents) yield typed `Rejected`/
    `Buffered`, never a panic, OOB, or unbounded recursion (ancestor walks are iterative + bounded).

---

## 9. Error model & observability

- **Reuse `RejectReason`** (D8). The fold's outcome is `Ingest::{Accepted, Rejected, Buffered}`;
  `Rejected` carries the typed `RejectReason` (`insufficient_role`, `bad_capability`,
  `expired_invite`, `not_a_member`, `unbound_device`). Each rejection is a **protocol-violation audit
  record** `{event_id, sender_id, event_type, reason}` (local only; PRD §16).
- **Flags are advisory** and never change verdict/state: `from_removed_member` (author later removed —
  UI attribution), `equivocation` (one key authored two mutually-concurrent events — local detection
  only; severity CRITICAL when the signer is the admin and the fork touches membership, INFO
  otherwise; **MVP detects/alerts, never auto-ejects**). The cross-peer same-`admin_seq` fork detection
  + fail-closed is explicitly the sync issue's (§3.2).
- **Buffered** (causally incomplete) is **not** an error — it mirrors the store's `NULL`-lamport
  tolerance; the event is retried when its parent arrives.
- **No panics** on adversarial/contradictory input; ancestor traversal is iterative with a visited-set
  (no unbounded recursion / no re-walk), and `prev_events` is already `≤ 20` (stateless `too_many_parents`).

---

## 10. Security, privacy, reliability, performance

- **This is a security-critical, `risk/high`, `type/security` layer.** It is the authorization
  boundary for who may write membership and who may access pipes/blobs. The dominant risks are
  *convergence* bugs (two honest peers with the same set disagreeing) and *authorization* bugs
  (a non-admin writing membership, a removed member retaining access, ban-evasion under a fresh key).
- **Ancestor-stable verdicts** (D3) are the structural defense against arrival-order-dependent
  authorization — the property that makes the same-set convergence theorem hold. Tests 5/9/12 guard it.
- **Key-bound invites only** (§6 path A): a join under a never-before-seen key has no naming invite and
  fails the gate ⇒ **ban-evasion under a fresh key is impossible** and "kick is sticky" holds. Open
  bearer tickets are **excluded from MVP** (they defeat sticky-kick); do not add them.
- **Sticky departure** (D4): leave **and** removal consume prior authorizations, closing both the
  stale-invite-replay and self-rejoin-after-leave holes.
- **Least-privilege attribute merge** (§3.8) prevents the one same-set divergence the bare status fold
  left open (two peers deriving different `role` ⇒ different agent-pipe decisions). Tie-break is the
  bytewise-lowest `event_id` — deterministic and grind-resistant in the sense that *every* peer agrees
  on the same (possibly attacker-pinned) value, which is sufficient for convergence (timeline position
  carries no trust, §2.4).
- **No key rotation in MVP** (PRD §13.4): removal changes *future* authorization but cannot erase what a
  removed member already received. Enforcement is **fail-closed at connect + tear-down-on-learn**,
  **bounded by removal-event reachability** (§0/§5) — documented honestly, not as "briefly."
- **Log-only expiry** (§6): ticket validity compares signed `invite.expires_at` vs signed
  `join.created_at` — **never** the local clock — so every peer computes the same verdict. The
  advisory clock-skew flag MUST NOT influence any expiry/authorization decision (vector §20).
- **Privacy / local-first:** the fold is pure local computation over already-stored signed events;
  nothing leaves the device. `created_at` is attacker-chosen and used only for display + log-only
  expiry, never ordering.
- **Performance (prototype):** ancestor reachability is the hot path. Cache per-event ancestor sets
  (or memoize reachability) so the fold is ~O(events × avg-fan-in) over MVP-sized rooms (≤5 people).
  Incremental `ingest` updates only the touched subject. Acceptable for MVP; a windowed/indexed
  re-fold against the store is a later optimization.

---

## 11. Implementation steps

1. **Module scaffold.** `crates/iroh-rooms-core/src/membership/mod.rs` + `pub mod membership;` in
   `lib.rs`. Submodules: `model.rs` (`Status`, `Role`, `Member`, `MembershipSnapshot`), `dag.rs`
   (in-memory DAG + ancestor reachability with memoization), `fold.rs` (`RoomMembership`, `ingest`,
   the per-subject fold), `authz.rs` (`AncestorView`, the per-type gate, the join gate), `access.rs`
   (`blob_serve_allowed`, `pipe_connect_allowed`, decisions). Module docs link the spike sections.
2. **DAG + ancestors.** Build `event_id → (ValidatedEvent, parents)`; iterative ancestor walk with a
   visited-set; memoize. Topological processing order = ascending `(lamport, event_id)` (compute
   `lamport` from `prev_events`; reuse the store's value when fed from the store adapter).
3. **`AncestorView` + `MembershipOracle` impl.** Ancestor-scoped `bound_device` + `authorize`
   (membership-event gate + Active-member gate). Add **`validate_with_membership`** to
   `event::validate` (steps 7–8 over `validate_wire_bytes`), update `event::validate` / `lib.rs`
   re-exports.
4. **Authorization gates (§6.2).** Membership-event gate (admin/self); the full **join gate** (D4:
   live key-bound invite via `content::capability_hash`, log-only expiry, role match, sticky-departure
   consumption); generic gate (`not_a_member` / `unbound_device`).
5. **The fold (D5).** Per-subject `touch(X)` → causal heads → Removed-dominates status →
   least-privilege + lowest-`event_id` attribute merge. Seed admin from genesis. Maintain `by_device`.
6. **Out-of-order buffering (§4 light).** Buffer events with missing parents keyed by missing id;
   re-process on parent arrival; never reject for incompleteness. (No fetch/backfill — sync's job.)
7. **Access decisions (D7).** `blob_serve_allowed`, `pipe_connect_allowed` over the snapshot;
   `expires_at` consulted only to deny.
8. **Flags.** Attach `from_removed_member` to events authored by a now-Removed member; detect
   local `equivocation` (one key, two mutually-concurrent events). Advisory only.
9. **Tests (§8).** In-module unit tests + `tests/membership_fold.rs`, reusing the e2e fixture
   builders; include the shuffle-order convergence tests and the access-gate vectors.
10. **Docs.** Module rustdoc; when merged, update `README.md` "Remaining Room Event Plane targets" to
    mark the membership fold as landed (doc-only follow-up, not in this spec change).

---

## 12. Risks & mitigations

- **R1 — Ancestor-stability violated (HIGH, convergence-critical).** Any authorization that
  accidentally reads live state instead of the event's ancestors breaks the same-set theorem.
  *Mitigation:* the `AncestorView` is the **only** authorization context for log-validity (D3); tests
  5/9/12 feed shuffled orders and assert identical verdicts/snapshots; never pass the live snapshot
  into the gate.
- **R2 — Bearer-ticket / fresh-key ban-evasion (HIGH, security).** An open (non-key-bound) ticket lets
  a banned party mint a join under a new key with empty removal history ⇒ `Active` everywhere.
  *Mitigation:* MVP is **key-bound only** (§6 path A); a join with no naming invite for its key fails
  the gate; bearer tickets explicitly excluded (D8 / §10). Pin a test (3c).
- **R3 — Sticky-departure asymmetry (HIGH).** If `member.left` did **not** consume invites, a member
  could self-rejoin past their own leave with the old invite. *Mitigation:* D4 treats leave **and**
  removal symmetrically as invite-consuming; vector §19 test (4) covers both.
- **R4 — Attribute-merge non-determinism (MED–HIGH, convergence-critical).** Two peers deriving
  different `role` from the same set ⇒ different agent-pipe decisions. *Mitigation:* §3.8
  least-privilege (`min` over `Role`) + lowest-`event_id` tie-break, both order-independent; test 5
  asserts identical `role` across shuffles.
- **R5 — Log-validity vs access-control conflation (HIGH).** Gating access on the ancestor view (or
  log-validity on the live snapshot) would either leak access to removed members or break convergence.
  *Mitigation:* D6 keeps the two contexts in separate code paths; test 10 pins the split.
- **R6 — Clock-dependent expiry (MED).** Consulting the local clock for ticket expiry would make log
  validity peer-local and diverge honest peers. *Mitigation:* §6 log-only comparison (signed
  `expires_at` vs signed `created_at`); the advisory clock-skew flag never influences a verdict.
- **R7 — Frozen-surface drift (MED).** `MembershipOracle::authorize(room, sender, event_type)` carries
  no `content`, so it cannot itself perform the join-capability check. *Mitigation:* keep the join
  gate membership-internal (D3 step 4); the trait covers only the generic membership/role gate. Surface
  whether the trait should gain a richer form (Open Q1).
- **R8 — Set-completeness over-claim (MED, correctness-of-claims).** Asserting unconditional
  convergence is false (§0). *Mitigation:* implement and test only the **same-set** guarantee; document
  the caveat; leave admin-tip detection / fail-closed to sync (§3.2).
- **R9 — Performance of ancestor reachability (LOW–MED).** Naïve repeated DAG walks are O(events²).
  *Mitigation:* memoize ancestor sets / reachability; incremental `ingest`. MVP rooms are tiny; revisit
  with a store-backed re-fold later.

---

## 13. Acceptance criteria (issue) → coverage

- [ ] **Admin can invite and remove** — admin-signer gate (§6.2); test 1.
- [ ] **Non-admin invite/remove is rejected** — `insufficient_role` gate; test 2 / vector §14.
- [ ] **Join requires a valid key-bound invite capability** — D4 join gate via
  `content::capability_hash`; test 3 / vector §15.
- [ ] **Leave and removal consume prior invitations** — D4 sticky departure (symmetric); test 4 /
  vector §19.
- [ ] **Concurrent join/kick converges to Removed for identical event sets** — D5 causal-heads +
  Removed-dominates; shuffle-order test 5 / vector §11/§18.
- [ ] **Current snapshot is used for pipe/blob access decisions** — D6/D7 snapshot + access predicates;
  test 6/10 / vectors §16/§17.

**Issue Test Plan** (invite/join/leave/remove, stale invite replay, concurrent join vs kick, non-admin
admin action, removed-member capability denial) — all covered in §8 (tests 1–10).

---

## 14. Open questions

1. **`MembershipOracle` shape.** The frozen `authorize(room, sender, event_type)` cannot perform the
   join-capability check (no `content`). Recommendation: keep the join gate membership-internal and use
   the trait only for the generic membership/role gate (D3). Confirm, or extend the trait with an
   event-scoped form before more callers bind to it.
2. **`Status` for unknown subjects.** `Status::{Invited,Active,Removed}` + `status()->Option<Status>`
   with **default-deny** (recommended), or add an explicit bottom `NotAMember` case? Affects the access
   predicates' signatures (§4 note).
3. **Where `validate_with_membership` lives.** In `event::validate` (sibling of `validate_wire_bytes`,
   matching the trait doc — recommended) vs the new `membership` module? The trait doc implies the
   former.
4. **Store integration timing.** Ship the pure in-memory fold now and a thin `store`-backed adapter as
   a follow-up (recommended), or include the adapter (and a persisted `members` derived cache) in this
   issue? The store already guarantees rebuildability, so persistence is additive.
5. **Capability scope beyond `role`.** MVP's only concrete attribute is `role`; §3.8 also names
   "capability scope = intersection." Confirm `role` is the sole MVP attribute and the
   scope-intersection is a documented forward hook (recommended), not built now.
6. **Equivocation flagging depth.** Local same-key-two-concurrent-events detection + `equivocation`
   flag (recommended for this issue) vs deferring all equivocation handling to the admin-tip/sync
   issue? The cross-peer same-`admin_seq` detection + fail-closed is firmly sync's (§3.2).
7. **`pipe_connect_allowed` composition.** Should this function take `no_pipe_closed` / `expires_at`
   as inputs (membership owns only identity/Active/allowed_members — recommended), or should the Pipe
   plane compose the membership predicate with its own lifecycle checks?

## 15. Assumptions

- Input events are `ValidatedEvent`s from the **landed** `validate_wire_bytes` (#6); the stateless
  trust boundary (signature, canonical bytes, id, room binding, self-contained device bindings,
  structural genesis-descent) is **not** repeated here.
- Exactly **one** `room.created` per room and **one immutable admin** (the genesis signer); multi-admin
  is out of MVP and would reintroduce multi-writer resolution.
- **No key rotation** in MVP (PRD §13.4): identities are stable, one device per identity; removal
  bounds *future* access, bounded by removal-event reachability.
- The **membership sub-DAG is never windowed** by sync (genesis + full admin chain + all relevant
  `member.*` are always present); this layer relies on that invariant for correct folds.
- The store (when used as the source) supplies events already ordered `(lamport, event_id)`; the
  in-memory fold recomputes the same order from signed `prev_events` and is order-independent regardless.
