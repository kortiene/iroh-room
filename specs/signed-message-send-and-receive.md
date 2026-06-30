# Spec: Signed Message Send and Receive (IR-0105)

| | |
|---|---|
| **Issue** | #20 — `[IR-0105] Implement signed message send and receive` |
| **Parent epic** | #2 |
| **Labels** | `type/feature` `area/protocol` `area/transport` `area/cli` `priority/p0` `risk/high` |
| **Dependencies** | #17 (IR-0102, room create — **landed**), #18 (IR-0103, key-bound invite — **landed**), #9 (IR-0005, full-mesh QUIC event transport — **landed prototype**), #11 (IR-0007, bounded recent-sync engine — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.4, §11.1, §16; Spike `PHASE-0-SPIKE.md` Event Protocol §7 (`message.text`); Membership & Ordering §2 (ordering), §4 (out-of-order delivery) |
| **Owning crates** | `crates/iroh-rooms-cli` (the first **online** command surface + orchestration); one small additive `message.text` builder in `crates/iroh-rooms-core::event`; one small additive read passthrough in `crates/iroh-rooms-core::sync` + `crates/iroh-rooms-net::node` |

> **Status:** landed — implemented in issue #20 / IR-0105; this document is the build plan.
> The compiled binary is the source of truth.

---

## 1. Summary

Add signed chat to the `iroh-rooms` binary: author a `message.text` event, push it to
connected room peers over the landed QUIC transport, and — on the receiving side — validate,
deduplicate, persist, and display it in deterministic timeline order.

```bash
# Receiver (long-running; streams the timeline):
iroh-rooms room tail <ROOM_ID>

# Sender (one-shot):
iroh-rooms room send <ROOM_ID> "I pushed the first prototype."
```

The protocol- and convergence-critical machinery this needs **already exists and is
conformance-tested**:

- the `message.text` content type + strict validation (`iroh-rooms-core::event::content`, #6);
- the stateless §6 signature/canonicality/id pipeline (`validate_wire_bytes`, #6);
- the deterministic membership fold + ancestor-stable authorization gate
  (`iroh-rooms-core::membership`, #12);
- the idempotent SQLite event store with `(lamport, event_id)` canonical ordering
  (`iroh-rooms-core::store`, #8);
- the bounded recent-sync **engine** whose receive path already does *validate → fold-gate →
  dedup → persist → fan-out* (`iroh-rooms-core::sync::SyncEngine`, #11); and
- the full-mesh QUIC carrier with admission-before-bytes and a live `WireEvent` push
  (`crates/iroh-rooms-net`, #9).

Because of that, **every message-correctness acceptance criterion is satisfied by landed code**
(see §12.1). This issue is therefore mostly **integration**, with exactly three small additive
library pieces:

1. a **pure `build_message_text` builder** in `iroh-rooms-core::event` (the byte-exact place a
   `message.text` is assembled and signed — the sibling of `build_room_created` /
   `build_member_invited`);
2. a thin **read passthrough** so a running node can surface its room timeline for display
   (`SyncEngine::room_tail` → `Node::room_tail`); and
3. the **first online CLI wiring**: the `iroh-rooms` binary today is fully synchronous and has
   **no dependency on `iroh-rooms-net`, `tokio`, or the core `sync` feature**. This issue adds
   that runtime and the two commands (`room send`, `room tail`) that drive a `Node`.

> **Why `risk/high`.** This is the first command that leaves the local filesystem and talks to
> another machine. The risk is **not** protocol correctness (that is landed and tested) — it is
> the new surface: an async runtime in a previously-sync binary, peer addressing/discovery, the
> ephemeral-vs-long-running session model, and the honest "no guaranteed delivery" UX
> (PRD §14). The design below keeps the guaranteed core (author + persist) fully offline-capable
> and treats live delivery as best-effort, exactly as the availability model promises.

---

## 2. Background & current repository state

### 2.1 What exists (landed work this builds on)

All paths below are `pub` unless noted.

- **`message.text` content (`event::content`, #6).**
  - `content::MessageText { body: String, format: Option<String>, in_reply_to: Option<EventId>, mentions: Option<Vec<IdentityKey>> }` and `Content::MessageText(..)` (`event/content.rs:199`).
  - Strict parse enforces: `body` is UTF-8 and `body.len() <= MAX_MESSAGE_BODY_BYTES` (16384), `format ∈ {plain, markdown}`, `in_reply_to`/`mentions` well-typed, and **any unknown content key is rejected** (`parse_message_text`, `content.rs:673`; `MAX_MESSAGE_BODY_BYTES`, `constants.rs:28`).
  - `message.text` is a **membership-device-bound** type — `EventType::requires_membership_device_binding() == true` — so it carries **no** embedded `device_binding`; its signing device is resolved from membership state (`content.rs:92`).
- **Event assembly + signing (`event::signed` / `event::wire`, #6).**
  - `SignedEvent { schema_version, room_id, sender_id, device_id, event_type, created_at, prev_events, content }`, `to_csb()`, `event_id()` (`signed.rs:21`).
  - `signed::sign_csb(csb, device_secret) -> Signature` (`signed.rs:261`); `WireEvent::seal(csb, sig)` + `to_bytes()` (`wire.rs`).
- **Stateless validator (`event::validate`, #6).**
  - `validate_wire_bytes(bytes, &ValidationContext) -> Result<ValidatedEvent, RejectReason>` and `ValidationContext::for_room(RoomId)` (`validate.rs:79`, `validate.rs:41`). For `message.text` this checks: canonical CBOR, recomputed `event_id`, **signature under `device_id`**, strict content, `room_id == expected`, `prev_events` non-empty and `<= 20`. Membership/role (step 8) and the membership-derived device binding (step 7) are deferred to the fold.
- **Membership fold (`membership`, #12, always compiled).**
  - `RoomMembership::from_events(room_id, events)`, `ingest(ValidatedEvent) -> Ingest`, `snapshot() -> MembershipSnapshot` (`membership/fold.rs:127`, `:139`, `:171`).
  - The gate for `message.text` is `gate_active_member`: the author must be **Active in the event's ancestor view**, else `RejectReason::NotAMember` (`fold.rs:375`), followed by `gate_device_binding` (`fold.rs:389`).
  - `MembershipSnapshot::{admin, is_active, member, active_members, identity_of_device, members}` and `Member { identity, device: Option<DeviceKey>, status, role }` (`membership/model.rs:90`, `:156`).
- **SQLite store (`store`, #8, `store` feature).**
  - `EventStore::{open, insert (idempotent G-set), room_tail, heads, room_event_ids, get, contains}` (`store/mod.rs`). `room_tail(room, limit)` returns the most-recent causally-placed events **ascending by `(lamport, event_id)`** — exactly the §2 deterministic timeline (`store/mod.rs:251`). `heads(room)` returns the current DAG heads for `prev_events` selection (`store/mod.rs:312`).
- **Bounded recent-sync engine (`sync`, #11, `sync` feature, which also enables `store`).**
  - `SyncEngine::open(store, room_id, config) -> Result<Self, SyncError>` rebuilds the fold from the persisted log (`sync/engine.rs:231`).
  - `publish(&[u8]) -> Result<Vec<Outgoing>, SyncError>` — ingest a **locally-authored** stateless-valid frame, fold-gate it, persist on accept, and fan out as `SyncMessage::Events` to connected peers (`engine.rs:296`).
  - `ingest_frame` / `on_message(Events)` → `deliver_bytes` → `deliver`: the **receive path** that runs `validate_wire_bytes` (drops bad signatures / non-canonical as a logged drop, `engine.rs:493`), then `fold.ingest` (drops non-members with `not_a_member`, buffers causally-incomplete frames, `engine.rs:515`), then `store.insert` (idempotent — a `Duplicate` is counted and **not** re-broadcast, `engine.rs:546`), then fan-out to peers **except** the sender (`engine.rs:557`).
  - `on_connect` issues the handshake (admin-tip + heads + `WantMembership` + bounded `WantRecentChat`); `on_tick` re-pulls as anti-entropy (`engine.rs:306`, `:376`).
  - `Outgoing { peer, msg }`, `SyncMessage::Events { room_id, frames }`, `PeerId([u8;32]) == device_id` (`sync/message.rs`). `SyncTransport { peers(), send(Outgoing) }` (`sync/transport.rs:20`).
- **Full-mesh QUIC carrier (`crates/iroh-rooms-net`, #9).**
  - `NetTransport::bind(secret: iroh::SecretKey, admission: Arc<dyn Admission>, audit: Arc<dyn AuditSink>, cfg: NetConfig)` implements `SyncTransport`; the endpoint is keyed by the node's **device secret** so `endpoint.id() == device_id == EndpointId` (`transport.rs:188`, lib.rs).
  - `Node::spawn(secret, admission, audit, engine, cfg, tick) -> Result<Node>` — pairs the transport with the engine and runs a single **pump task** that owns the engine: inbound frame → `on_message`; connect/disconnect → `on_connect`/`on_disconnect`; tick → `on_tick` (`node.rs:62`, `:239`).
  - `Node` handle methods: `publish(Vec<u8>) -> Result<()>` (`node.rs:134`), `connect_to(EndpointAddr)` (`:102`), `endpoint_addr() -> Result<EndpointAddr>` (`:97`), `id() -> EndpointId` (`:89`), `peer_state`/`peer_states`/`conn_events` (`:113`–`:127`), `snapshot() -> MembershipSnapshot` (`:163`), `store_contains(EventId) -> Result<bool>` (`:149`), `wait_for_state` / `wait_until_contains` (`:187`, `:206`), `shutdown()` (`:223`).
  - `Admission { authorize(device: EndpointId) -> AdmissionDecision }` with `AdmissionDecision::{Admit { identity }, Reject(cause)}`; the prototype `AllowlistAdmission` exposes `bind_device(device, identity)` + `mark_active(identity)` (and `mark_fail_closed`) with the **same shape** the production `MembershipSnapshot` provides (`admission.rs`). `AuditSink` + `TracingAudit` (`audit.rs`). `NetConfig { mode: NetMode, conn_event_capacity }`, `NetMode::{Loopback, RealNetwork}` (`transport.rs:37`).
- **CLI scaffold (`crates/iroh-rooms-cli`, #16/#17/#18).**
  - `cli.rs`: `Cli { data_dir, command }`, `Command::Room { action: RoomAction }`, `run()` dispatch (`cli.rs`). `RoomAction::{Create, Members, Invite}` today.
  - `identity::SecretKeys::load(home) -> Result<SecretKeys>` with `identity` + `device: SigningKey` (`identity.rs:96`); `Profile::load` (public only).
  - `room.rs` / `invite.rs`: the offline authoring pattern this issue mirrors — load secrets → fold the persisted log → select `prev_events = heads` (truncated to `MAX_PREV_EVENTS`) → build → `validate_wire_bytes` self-check → `membership.ingest` self-check → `store.insert` (`invite.rs:141`–`:211`).
  - `clock::now_ms()` — the single advisory wall-clock read (`clock.rs:12`).
  - Integration tests use `assert_cmd` + per-test `--data-dir` temp homes (`tests/invite_cli.rs`).

### 2.2 The real gaps (this issue closes them)

1. **No `message.text` builder.** There is no byte-exact assembly point for a `message.text`
   (the `room.created` and `member.invited` builders exist; this one does not). **D1.**
2. **The CLI is offline-only.** `crates/iroh-rooms-cli/Cargo.toml` depends on `iroh-rooms-core`
   with only the `store` feature and has **no** `iroh-rooms-net` / `tokio` dependency, and `run()`
   is synchronous. Live send/receive needs the async runtime + the net adapter wired in. **D2/D3.**
3. **No way to read a running node's timeline for display.** The engine owns the store inside the
   pump task; `Node` exposes membership/contains/snapshot but **not** the room tail. A thin
   additive passthrough is needed so `room tail` can render. **D6.**

### 2.3 Spike facts that constrain the design

- **`message.text` content (Event Protocol §7).**
  ```
  content = {
    "body":        tstr,                 // UTF-8, ≤ 16384 bytes
    "format":  opt tstr,                 // "plain" (default) | "markdown"
    "in_reply_to": opt bstr[32],         // EventId
    "mentions":    opt [ bstr[32], ... ] // member identities
  }
  ```
  Authored by **any current member**; `prev_events` = room heads (§7 registry table).
- **Signing model (§1/§6).** Events are signed by the **device key** (`device_id` = iroh
  `EndpointId`) and authorized against the **identity key** (`sender_id`). The signature MUST
  verify under `device_id`, never `sender_id`.
- **Deterministic timeline (Membership & Ordering §2).** Order the validated, causally-complete
  set ascending by `(lamport, event_id)`, where `lamport` is **derived** (`1 + max(parent
  lamports)`, genesis = 0) and `event_id` is compared bytewise over its 32 raw digest bytes.
  `created_at` is **advisory/display-only** — never used to order or to make any security
  decision (§2.3). `store::room_tail` already implements exactly this comparator.
  **Timeline position carries no trust** (§2.4): an author can grind `content`/`prev_events` to
  pin their event first or last; the UI must attach no meaning to position.
- **Ancestor-stable validity (§4).** A `message.text` is judged Active/not against **its own
  causal ancestors**, so two peers with the identical validated set converge byte-for-byte
  regardless of arrival order. A message that arrives **before** the membership events that name
  its author is **buffered and backfilled**, never dropped for "unknown sender" (engine
  `deliver` → `on_buffered`; fold `Ingest::Buffered`).
- **Duplicate idempotency (§6 step 11 / §8 vector 8).** A re-seen `event_id` is **ignored** (not
  an error), state/timeline unchanged, nothing re-broadcast — `store.insert` returns
  `InsertOutcome::Duplicate`, the engine counts it and stops the fan-out.
- **Non-member rejection (§8 vector 13).** A well-formed, correctly-signed `message.text` from a
  key that is not Active is rejected with `not_a_member` at the fold gate — dropped, never
  persisted or re-broadcast.
- **Admission-before-bytes / availability (ADR-1, §5; PRD §14, §16.3).** The carrier rejects an
  unknown/`!Active` remote `EndpointId` **before** reading any event byte. Delivery is
  best-effort while peers are online; there is **no cloud inbox and no guaranteed offline
  delivery**.

---

## 3. Design decisions

> Decisions are referenced by `Dn` throughout. Each is the smallest choice that satisfies the
> acceptance criteria while staying aligned with the landed architecture.

- **D1 — Add a pure `build_message_text` to `iroh-rooms-core::event`.** A new
  `event/message.rs`, re-exported as `event::build_message_text`, assembles + signs a
  `message.text` deterministically from injected inputs (keys, `room_id`, body, optional
  `format`/`in_reply_to`/`mentions`, `prev_events`, `created_at`). It mirrors
  `build_member_invited` exactly (clock-/RNG-free; the caller injects the heads and clock). This
  is the single byte-exact assembly point, golden-tested in core, reused by the CLI and any
  future flow. **Rationale:** keep authoring byte-exact and tested in one place; the CLI stays a
  thin orchestrator (the established #17/#18 pattern).

- **D2 — The CLI gains an async runtime and depends on `iroh-rooms-net`.** Add `iroh-rooms-net`,
  bump the core dependency to the `sync` feature (which transitively enables `store`), and add
  `tokio` (multi-thread runtime). `run()` stays synchronous for the offline commands; the two
  online commands (`room send`, `room tail`) each enter a scoped `tokio` runtime
  (`#[tokio::main]`-style block or `Runtime::new()?.block_on(...)`). **Rationale:** the net crate
  is async-only and the README/`iroh-rooms-net/Cargo.toml` already name the CLI as the intended
  runtime host ("Keep `publish` gated until the CLI (N1) wires the adapter into a runtime").

- **D3 — `room send` is offline-first, online-best-effort.** It **always** builds, self-validates,
  and persists the `message.text` locally (works with no network — the §2.2 guaranteed core).
  It **then** best-effort: brings up an **ephemeral `Node`**, dials the room's other Active
  members, waits briefly for at least one link to reach `Connected`, `publish`es the frame,
  grants a short grace for the writer queues to flush, and shuts down. The exit message honestly
  reports how many connected peers it reached (possibly zero). **Rationale:** matches the docs
  (`room send` is a one-shot) and the availability model — no queue, no guaranteed delivery; the
  local persist is the only guarantee. The ephemeral model avoids introducing a daemon in this
  issue (see OQ-1 for a future `room serve`).

- **D4 — `room tail` is the long-running receiver/session.** It brings up a `Node`, dials the
  room's other Active members, accepts inbound connections (admission from the membership
  snapshot), lets the engine validate/dedup/persist inbound `message.text` frames, and renders
  the timeline — printing the existing tail once, then streaming newly-arrived events — until
  the user interrupts (Ctrl-C / SIGINT). **Rationale:** the docs use `room tail` as the streaming
  receiver; a long-running session is the natural home for the inbound transport and the live
  display.

- **D5 — The dial set is the membership snapshot's Active members' devices; addressing is iroh
  discovery with an explicit `--peer` override.** The peers to dial are
  `snapshot.active_members()` device ids minus this node's own device (`fold.rs` /
  `model.rs:156`). In `NetMode::RealNetwork`, iroh n0 DNS + mDNS resolve an `EndpointId` to an
  `EndpointAddr` (Spike §8 "dial purely by `EndpointId`"). For deterministic LAN/loopback tests
  (where discovery is unavailable), a repeatable `--peer <ENDPOINT_ADDR>` flag supplies addresses
  out-of-band, mirroring how the landed net loopback tests exchange `endpoint_addr()`. `room tail`
  prints its own dialable `endpoint_addr` on startup so a second terminal can pass it to
  `--peer`. **Rationale:** there is no persistent peer address book yet (OQ-2); discovery covers
  real use and `--peer` makes the two-peer CLI test deterministic and hermetic.

- **D6 — Surface the timeline for display via a thin additive passthrough.** Add
  `SyncEngine::room_tail(&self, limit: u32) -> Result<Vec<StoredEvent>, SyncError>` (a passthrough
  to `store.room_tail`) and a `Node::room_tail(limit) -> Result<Vec<StoredEvent>>` handle command
  (a new `Cmd::Tail`). `room tail` polls this on a short interval and prints events it has not
  shown yet, keyed by `event_id`. **Rationale:** the engine is single-owner inside the pump; a
  query command keeps one writer and avoids a second SQLite connection racing the engine's WAL
  writes. (Alternative considered: open a second read-only `EventStore` on the same WAL file —
  rejected for this issue to avoid cross-connection subtleties; see OQ-3.)

- **D7 — Build the carrier `Admission` from the current `MembershipSnapshot`.** Construct an
  `AllowlistAdmission`, and for each `snapshot.active_members()` call `bind_device(device,
  identity)` + `mark_active(identity)`. This is the production shape the net crate documents and
  needs no new core code. **Rationale:** reuse the landed prototype; the snapshot is the exact
  device→identity + Active source the admission gate wants.

- **D8 — `prev_events = store.heads(room)`, truncated deterministically to `MAX_PREV_EVENTS`.**
  Identical to the landed `invite.rs` head-selection (`invite.rs:141`). Heads are already
  ascending by `event_id`; if there are more than 20, cite the 20 lowest-id heads and note it.
  **Rationale:** consistency with the landed authoring path; the single-admin small-room MVP
  rarely exceeds one head. (The Spike §1 self-parent rule is **not** enforced by the landed
  validator/fold and is out of scope here; see OQ-4.)

- **D9 — Self-validate before persist, exactly like `invite`.** After building, run
  `validate_wire_bytes` and then `membership.ingest(...)` and require `Ingest::Accepted` before
  `store.insert`. A failure is an internal bug, surfaced as an error, never a silent persist of a
  message peers would reject. **Rationale:** the #17/#18 belt-and-suspenders pattern; guarantees
  the local store only ever holds peer-acceptable events.

- **D10 — Display format is identity-first and trust-free.** Render each timeline row as
  `[<created_at>] <author>: <body>`, where `<author>` is the member's `display_name` if known
  (from a `member.joined` in the local log) else a short `sender_id` (first 8 hex). Mark
  `from_removed_member` rows with a `(removed)` tag. `created_at` is shown for human context but
  **the ordering is `(lamport, event_id)`** from `room_tail`, never `created_at`. No "first/top"
  semantics (§2.4). **Rationale:** honest, deterministic display that never attaches trust to
  position or wall clock.

---

## 4. Architecture & data flow

### 4.1 Send path (`room send`)

```
load SecretKeys (identity + device)                         [offline]
 → open EventStore(<home>/rooms.db); fold persisted log     [offline]
 → assert caller is an Active member of the room            [offline]
 → prev_events = heads(room) (≤ MAX_PREV_EVENTS)            [offline]
 → build_message_text(identity, device, room_id, body, …, prev, now_ms())   [D1]
 → validate_wire_bytes + membership.ingest == Accepted      [D9 self-check]
 → store.insert (idempotent)                                [offline — the GUARANTEE]
 ───────────────────────────────────────────────────────── persisted; safe to exit
 → best-effort live push:                                   [online — D3]
     SyncEngine::open(store2, room_id, cfg)                 (2nd handle, read+publish)
     Node::spawn(device_secret→iroh::SecretKey, Admission(snapshot), audit, engine, RealNetwork)
     for peer in dial_set: Node::connect_to(addr)           [D5]
     wait_for_state(any peer → Connected, --timeout)        (else "0 peers online")
     Node::publish(wire.to_bytes())                         (engine fans out Events frame)
     short grace (flush writer queues) → Node::shutdown()
 → print: stored locally; delivered to N connected peer(s)
```

> The send path opens the store twice in sequence (first to persist offline, then handed to the
> engine for the push) — never two live handles at once. Simpler still: persist via the engine's
> own `publish` after the offline self-check. Either is acceptable; the implementer picks one and
> keeps a single live `EventStore` handle (SQLite `EventStore` is not `Sync`).

### 4.2 Receive path (`room tail`) — already implemented in the engine

```
Node::spawn(device_secret, Admission(snapshot), audit, SyncEngine::open(store, room_id, cfg), RealNetwork)
 → for peer in dial_set: Node::connect_to(addr)             [D5]
 → inbound link admitted iff remote device → identity is Active (admission-before-bytes, #9)
 → inbound SyncMessage::Events → engine.on_message → deliver_bytes:    [all landed, #11]
     validate_wire_bytes  → drop bad-signature / non-canonical (logged)   (AC: invalid sig)
     fold.ingest          → Accepted | Buffered (backfill) | Rejected     (AC: non-member, order)
     store.insert         → Inserted | Duplicate (ignored, not rebroadcast)(AC: duplicate)
     fan-out to peers except sender
 → display loop (D6): poll Node::room_tail(limit); print rows not yet shown, ordered (lamport,event_id)
 → until SIGINT → Node::shutdown()
```

**The entire validate/dedup/persist/order chain on the receive side is landed code.** This
issue contributes the wiring and the display loop, not the correctness logic.

### 4.3 Where each acceptance criterion is enforced

| Criterion | Enforced by | Status |
|---|---|---|
| Signed by device key | `build_message_text` signs CSB with `device_secret`; verified under `device_id` | D1 + landed validator |
| Duplicate ids ignored | `store.insert → Duplicate`; engine counts, no rebroadcast | landed (#8/#11) |
| Invalid signatures rejected | `validate_wire_bytes → bad_signature` (logged drop) | landed (#6/#11) |
| Non-member messages rejected | `fold.ingest → not_a_member` (drop) | landed (#12/#11) |
| Deterministic timeline order | `store.room_tail` `(lamport, event_id)` ascending | landed (#8) |

---

## 5. Detailed implementation steps

### Step 1 — Core: `event::build_message_text` (D1)

Create `crates/iroh-rooms-core/src/event/message.rs`:

```rust
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors build_member_invited; each arg is a signed field
pub fn build_message_text(
    sender_identity_secret: &SigningKey, // provides sender_id
    sender_device_secret: &SigningKey,   // signs the event (verify under device_id)
    room_id: &RoomId,
    body: &str,
    format: Option<&str>,                // None ⇒ omit (defaults to "plain" on read)
    in_reply_to: Option<EventId>,
    mentions: &[IdentityKey],            // empty ⇒ omit
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent
```

- Assemble `Content::MessageText(MessageText { body: body.to_owned(), format: format.map(ToOwned::to_owned), in_reply_to, mentions: (!mentions.is_empty()).then(|| mentions.to_vec()) })`.
- Build `SignedEvent { schema_version: SCHEMA_VERSION, room_id: *room_id, sender_id, device_id, event_type: EventType::MessageText, created_at, prev_events: prev_events.to_vec(), content }`, then `csb = to_csb()`, `sig = sign_csb(&csb, sender_device_secret)`, `WireEvent::seal(csb, sig)`.
- Declare `pub mod message;` in `event/mod.rs` and add `pub use message::build_message_text;`.
- **Do not** validate body length here (the strict content parser already enforces ≤16384 on
  decode/validate); the CLI validates **before** building for a friendly pre-IO error (Step 5).
- Golden/regression tests in `message.rs` mirroring `invite.rs`: determinism, content
  round-trip (incl. `in_reply_to`/`mentions`/`format`), `built_message_passes_stateless_validation`,
  `signature_verifies_under_device_id`, an implementation-pinned `GOLDEN_EVENT_ID_HEX`, and a
  body-at-cap (16384) and body-over-cap (rejected by `validate_wire_bytes`) case.

> The Spike fixture log lists `E_msg_bob` (`message.text "hi all"`) but its full content map is
> **not** byte-pinned in the spike (see Test Vectors caveat); regenerate the golden id from the
> final content schema rather than hard-coding `7292b762…`. The pinned conformance fact this
> issue **can** assert is that a tampered body changes the id and breaks the signature
> (Spike vectors 6).

### Step 2 — Core: `SyncEngine::room_tail` passthrough (D6)

In `crates/iroh-rooms-core/src/sync/engine.rs`, add:

```rust
/// The most-recent `limit` causally-placed events in canonical (lamport, event_id)
/// order — the deterministic display timeline (Membership §2).
///
/// # Errors
/// [`SyncError::Store`] on a store read failure.
pub fn room_tail(&self, limit: u32) -> Result<Vec<StoredEvent>, SyncError> {
    Ok(self.store.room_tail(&self.room_id, limit)?)
}
```

Additive, read-only, no schema change. (Reuses the landed `store.room_tail`.)

### Step 3 — Net: `Node::room_tail` handle (D6)

In `crates/iroh-rooms-net/src/node.rs`, add a `Cmd::Tail(u32, oneshot::Sender<Result<Vec<StoredEvent>, String>>)`, a `handle_cmd` arm calling `engine.room_tail(limit)`, and the async handle:

```rust
/// The current room timeline (most-recent `limit`, canonical order) for display.
pub async fn room_tail(&self, limit: u32) -> Result<Vec<StoredEvent>> { /* oneshot round-trip */ }
```

Re-export `StoredEvent` is already public from core (`store::StoredEvent`).

### Step 4 — CLI: dependencies & runtime (D2)

In `crates/iroh-rooms-cli/Cargo.toml`:

- Change `iroh-rooms-core` features from `["store"]` to `["sync"]` (transitively enables `store`).
- Add `iroh-rooms-net = { path = "../iroh-rooms-net" }`.
- Add `tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "signal", "sync"] }`.
- Add `iroh = "=1.0.1"` (for `SecretKey` / `EndpointAddr` / `EndpointId` types crossed at the CLI boundary). Keep the pin identical to `iroh-rooms-net`.
- Note the MSRV: `iroh-rooms-net` deliberately does not inherit the workspace `rust-version = 1.80` (the iroh 1.0 stack needs ≥1.85). The CLI now transitively requires the higher floor; document this in the manifest comment (the landed crypto deps already forced ≥1.85, so this raises only the declared floor — same situation noted in `iroh-rooms-net/Cargo.toml`).

### Step 5 — CLI: `room send` (D3/D5/D7/D8/D9)

New `crates/iroh-rooms-cli/src/message.rs` (sibling of `room.rs`/`invite.rs`).

1. **Pre-IO validation** (a bad invocation writes nothing): non-empty body; `body.len() <= MAX_MESSAGE_BODY_BYTES`; reject control characters other than `\n`/`\t` is **out of scope** (the protocol allows any UTF-8 body); optional `--format` ∈ {plain, markdown}; optional `--reply-to <EVENT_ID>` parse; `--timeout` parse (default e.g. 5s); repeatable `--peer <ENDPOINT_ADDR>`.
2. **Load secrets** (`SecretKeys::load`); **open store**; **fold** the persisted room log (re-validate each stored event, like `invite.rs`); confirm the room exists and the caller is **Active** (else actionable error: not a member / unknown room).
3. **`prev_events = heads(room)`**, truncate to `MAX_PREV_EVENTS` with the same note as `invite.rs` (D8).
4. **Build** via `build_message_text`, **self-validate** (`validate_wire_bytes` + `membership.ingest == Accepted`, D9), **persist** (`store.insert`). At this point the message is durably stored; everything below is best-effort.
5. **Best-effort live push** (D3):
   - Convert the device seed to an `iroh::SecretKey` (`SecretKeys::device` → `SigningKey::to_seed()` → `iroh::SecretKey::from_bytes(&seed)`; assert the resulting `EndpointId == device_id`).
   - Build `Admission` from the snapshot (D7); build the dial set = Active member devices minus self; resolve addresses from `--peer` flags, else rely on `NetMode::RealNetwork` discovery (D5).
   - `SyncEngine::open(store, room_id, SyncConfig::default())`; `Node::spawn(...)`.
   - `for addr in dial_set_addrs: node.connect_to(addr)`; `node.wait_for_state(peer, Connected, timeout)` for at least one peer (collect how many connect within `--timeout`).
   - `node.publish(wire.to_bytes()).await` (idempotent at the receiver; the engine fans out to connected peers).
   - Brief grace (e.g. 200–500 ms) so the per-peer writer queues flush, then `node.shutdown().await`.
6. **Output** (script-friendly labeled lines):
   ```
   sent: <event_id>
   room: <room_id>
   from: <sender_id>
   stored: yes
   delivered: 1 connected peer(s)         # or "0 (no peers online — stored locally only)"
   ```
   Exit non-zero only on a pre-persist failure (bad args, not a member, store error). A failure
   to reach peers is **not** an error (availability model) — it is reported, exit 0.

### Step 6 — CLI: `room tail` (D4/D5/D6/D10)

Add to `message.rs` (or a `tail.rs`):

1. Parse args: `room_id`, optional repeatable `--peer`, optional `--limit` (default e.g. 200 historical rows).
2. Load secrets; open store; fold; confirm room exists and caller is Active.
3. Build `iroh::SecretKey`, `Admission(snapshot)`, dial set (D5/D7).
4. `SyncEngine::open`; `Node::spawn(...)`. Print this node's dialable address:
   `listening: <endpoint_addr>` and `tip: pass this to the other peer as --peer` (so a second
   terminal can connect deterministically without discovery).
5. `for addr in dial_set_addrs ∪ --peer: node.connect_to(addr)`.
6. **Display loop:** keep a `BTreeSet<EventId>` of already-printed ids. On a short interval
   (e.g. 200 ms), call `node.room_tail(limit)`, print rows whose id is unseen in
   `(lamport, event_id)` order using the D10 format, mark them seen. (Print the historical tail
   on the first iteration.) Resolve `<author>` to a `display_name` from a local `member.joined`
   if present, else short `sender_id`.
7. Run until SIGINT (`tokio::signal::ctrl_c()`), then `node.shutdown().await` and exit 0.

> The poll-based display (D6) is deliberately simple and robust for the MVP prototype. A
> push-based "new event applied" broadcast from the engine/Node is a clean future optimization
> (OQ-3) but is not needed to satisfy the acceptance criteria.

### Step 7 — CLI: command surface & dispatch (D2)

In `cli.rs`, extend `RoomAction`:

```rust
/// Send a signed text message to the room and push it to connected peers.
Send {
    room_id: String,
    message: String,
    #[arg(long)] format: Option<String>,      // plain | markdown
    #[arg(long = "reply-to")] reply_to: Option<String>,  // event_id
    #[arg(long = "peer")] peers: Vec<String>, // repeatable EndpointAddr
    #[arg(long, default_value = "5s")] timeout: String,
},
/// Stream the room timeline, receiving and displaying signed messages live.
Tail {
    room_id: String,
    #[arg(long = "peer")] peers: Vec<String>,
    #[arg(long, default_value_t = 200)] limit: u32,
},
```

In `run()`, dispatch `Send`/`Tail` into a scoped tokio runtime (the rest of `run()` stays sync).
Parse `room_id` with the existing `RoomId::from_str` (`blake3:<hex>`), surfacing the same
"invalid room id" error as `members`/`invite`.

### Step 8 — Docs reconciliation

Update `docs/getting-started.md` Step 4 status note from "scaffold/illustrative" to "implemented"
and reconcile the `room tail` / `room send` output blocks against the shipped binary. Update the
`docs_conformance.rs` test fixtures if they assert on those blocks. (Mirrors the #17/#18 docs
reconciliation commits.)

---

## 6. CLI surface & output (reference)

```text
iroh-rooms [--data-dir <PATH>] room send <ROOM_ID> <MESSAGE>
           [--format plain|markdown] [--reply-to <EVENT_ID>] [--peer <ENDPOINT_ADDR>]... [--timeout <DUR>]
iroh-rooms [--data-dir <PATH>] room tail <ROOM_ID> [--peer <ENDPOINT_ADDR>]... [--limit <N>]
```

`room tail` example (illustrative; reconcile against the binary):

```text
listening: <endpoint_addr>
tip: share this address with the other peer via --peer
[2026-06-30T12:01:04Z] bob1a2b3c: I pushed the first prototype.
[2026-06-30T12:01:22Z] alice9f8e: Nice — pulling it now.
```

---

## 7. Error & observability model

- **Pre-persist failures (exit non-zero, nothing written):** invalid body (empty / >16384 bytes),
  bad `--format`/`--reply-to`/`--timeout`, no local identity, unknown room, caller not an Active
  member, store open/read error, or the internal-bug guard (freshly built message fails
  self-validation). Mapped to stderr + `ExitCode::FAILURE` by the existing `main.rs`.
- **Best-effort delivery (exit 0):** "0 peers online" is reported, not an error — there is no
  queue and no guaranteed delivery (PRD §14). `room send` prints the count of connected peers it
  pushed to.
- **Receive-side reason codes (stable, from Spike §8):** the engine logs each drop with its code
  — `bad_signature`, `non_canonical_encoding`, `id_mismatch`, `room_id_mismatch`,
  `invalid_content`, `not_a_member`, `unbound_device`, `too_many_parents`,
  `not_genesis_descended`; `duplicate` is counted, not an error; advisory flags `clock_skew`,
  `from_removed_member`, `equivocation` never affect the verdict. These already flow through
  `SyncEngine::logs()` / `counters()` and the net `AuditSink`. `room tail` SHOULD surface a brief
  one-line audit summary (or expose `--verbose` to print engine logs) so the documented
  Troubleshooting reason codes are observable. (Wiring this is light; if cut for scope, note it.)
- **Connection state (PRD §16.3):** the `Node` exposes `peer_state`/`conn_events` — connected /
  offline / unauthorized. `room send`'s "delivered: N" and any "peer unreachable" line derive from
  these.

---

## 8. Security, privacy, reliability, performance

- **Signature & authorization (unchanged trust boundary).** Messages are signed by `device_id`
  and authorized against `sender_id` via the ancestor-view fold. The CLI adds no new crypto and
  no new validation rule — it reuses the landed, conformance-tested pipeline. Self-validation
  before persist (D9) guarantees the local store never holds an unauthored/unacceptable event.
- **Admission before bytes (#9).** The receiver rejects a non-member's connection before reading
  any frame; a non-member's `message.text` bytes are never read off the wire. Even if a frame is
  read (from an Active-then-removed member on a shared link), the fold gate and the
  anti-amplification signer pre-check (`engine.deliver`) contain it; a since-removed author's
  log-valid messages grant **zero** capabilities and are tagged `from_removed_member` for the UI
  (Spike §5).
- **Secret hygiene.** Device/identity seeds stay inside `SigningKey`/`Zeroizing` and the
  `iroh::SecretKey`; they never appear in any output. The CLI integration tests assert no secret
  bytes leak to stdout/stderr (mirroring `invite_cli.rs`).
- **Privacy.** Every iroh hop is QUIC/TLS between authenticated endpoints; the room is private by
  membership. No central server sees content.
- **Reliability.** Best-effort live delivery; the engine's anti-entropy `on_tick` re-pull recovers
  a message dropped during a shuffled handshake once membership is established. Local persistence
  is the durable guarantee; offline catch-up beyond a connected peer's window is out of MVP scope
  (PRD §15.5).
- **Performance.** Small rooms (≤5). Body cap 16384 bytes; frame cap enforced by the net layer
  (`MAX_FRAME_BYTES`). The display poll interval (≈200 ms) is negligible against a single SQLite
  reader. Target: message delivered to a connected peer in <2 s (PRD §17.1.3) — well within the
  per-link QUIC push latency.

---

## 9. Test strategy

### 9.1 Unit / core (deterministic, no network)

- `event/message.rs` (D1): determinism, full content round-trip (`format`/`in_reply_to`/`mentions`),
  `built_message_passes_stateless_validation`, `signature_verifies_under_device_id`, golden
  `event_id` regression lock, body-at-cap accepted + body-over-cap rejected by
  `validate_wire_bytes`, tampered-body changes id & fails signature.
- `sync/engine.rs` (D6): `room_tail` returns the persisted set in `(lamport, event_id)` order
  (extend the existing engine tests).
- Existing landed coverage already proves the message ACs at the protocol layer and SHOULD be
  cited in the spec PR as the correctness backstop:
  - **Invalid signature rejected** — `validate.rs` / golden vectors (Spike vector 5/6).
  - **Duplicate ignored** — store idempotency tests + engine duplicate counter (Spike vector 8).
  - **Non-member rejected** — `membership/fold.rs` non-member gate + engine `deliver` drop
    (Spike vector 13).
  - **Deterministic order** — `store.room_tail` ordering tests + Spike vector 10.

### 9.2 CLI message authoring (Rust API + `assert_cmd`, offline)

- `message::send` (offline half) authors + persists a `message.text`: the event appears in
  `store.room_tail`; re-running `room members` is unchanged; a second identical send is idempotent
  at the store.
- Pre-IO gates: empty body, over-cap body, bad `--format`, bad `--reply-to`, unknown room, caller
  not a member — each exits non-zero and writes nothing new.
- Secret hygiene: no seed hex in `room send` output.

### 9.3 Two-peer CLI test (the issue's headline test)

A `tests/message_two_peer.rs` integration test spawns two `--data-dir` homes (Alice = admin,
Bob = invited+joined member, set up via the landed `room create`/`invite`/`join`*), then:

1. Start `room tail <ROOM_ID>` for Alice in `NetMode::Loopback`/LAN; capture her printed
   `listening:` address.
2. Run `room send <ROOM_ID> "hello" --peer <alice_addr>` as Bob.
3. Assert Alice's `room tail` prints Bob's message within a timeout, in the format of D10.
4. Assert **duplicate suppression**: Bob sends the same bytes twice (or the frame echoes); Alice's
   timeline shows it once. Assert **order**: two messages appear in `(lamport, event_id)` order.
5. Negative: a frame authored by a non-member key (constructed in-test) is dropped — Alice's
   timeline never shows it and the engine logs `not_a_member`.

> *Depends on `room join` (IR-0104, #19). If `room join` has not landed when this test is written,
> seed Bob's Active membership by constructing + persisting a valid `member.invited` +
> `member.joined` pair directly via the core builders into both homes (the fold accepts them), or
> gate the two-peer test behind a `#[ignore]` until #19 lands. **OQ-5.** The single-peer authoring
> tests (§9.2) and the unit tests (§9.1) cover the ACs independent of #19.*

For determinism the two-peer test uses explicit `--peer` addressing (no discovery) and the net
crate's `NetMode::Loopback`, exactly as the landed `iroh-rooms-net` loopback suite does.

---

## 10. Acceptance criteria → evidence

| # | Criterion | Satisfied by | Test |
|---|---|---|---|
| AC1 | Message event is signed by device key | `build_message_text` signs CSB with the device secret; `validate_wire_bytes` verifies under `device_id` (D1, D9) | §9.1 `signature_verifies_under_device_id`; §9.3 |
| AC2 | Duplicate event ids are ignored | `store.insert → Duplicate`; engine counts, no rebroadcast (landed) | §9.1 dup vector; §9.3 step 4 |
| AC3 | Invalid signatures are rejected | `validate_wire_bytes → bad_signature` drop in `engine.deliver_bytes` (landed) | §9.1 tampered-body; §9.3 step 5 |
| AC4 | Non-member messages are rejected | `fold.gate_active_member → not_a_member` drop (landed) | §9.1 non-member gate; §9.3 step 5 |
| AC5 | Message appears in deterministic timeline order | `store.room_tail` `(lamport, event_id)` ascending; `room tail` renders that order (D6, D10) | §9.1 order; §9.3 step 4 |

---

## 11. Risks

- **R1 — Async runtime in a previously-sync binary (high surface).** First `tokio` + `iroh-rooms-net`
  in the CLI; risk of runtime/lifetime bugs, MSRV drift (workspace 1.80 vs iroh ≥1.85), and build-time
  growth. *Mitigation:* scope the runtime to the two online commands; keep offline commands sync;
  document the MSRV bump (already implied by landed crypto deps).
- **R2 — Peer addressing/discovery is unproven on real NATs (Gate A still owed, #9 NOTES).** Discovery
  may not connect two peers across real networks yet. *Mitigation:* the deterministic test uses
  explicit `--peer` + loopback; real-network delivery inherits the open Gate-A risk from #9 and is not
  newly gated here. Document clearly.
- **R3 — Ephemeral `room send` may publish before a link is `Connected`,** fanning out to zero peers
  even when a peer is reachable. *Mitigation:* `wait_for_state(Connected, --timeout)` before
  `publish`; report the actual connected count; the local persist is unconditional.
- **R4 — Display via polling could miss or double-print under races.** *Mitigation:* dedup by
  `event_id` in a seen-set; `room_tail` is a consistent point-in-time read; ordering is the store's
  canonical comparator.
- **R5 — Two `EventStore` handles to one file.** SQLite `EventStore` is not `Sync`. *Mitigation:* keep
  exactly one live handle (hand the store to the engine; do offline persist before spawning, or
  persist via `engine.publish`). Never hold two write handles concurrently.
- **R6 — Timeline-position trust leak (§2.4).** A crafted message could pin itself first/last.
  *Mitigation:* D10 attaches no semantics to position; render in canonical order only; no "pinned".
- **R7 — Two-peer test depends on `room join` (#19).** *Mitigation:* OQ-5 — seed membership via core
  builders or gate the test until #19 lands; ACs are independently covered by unit + single-peer tests.

---

## 12. Open questions

- **OQ-1 — `room send` ephemeral node vs. a persistent `room serve` session?** This spec chooses
  ephemeral (D3) to avoid a daemon in this issue. A future `room serve` (a long-running node that
  `room send` talks to over a local socket) would make sends instant and delivery more reliable.
  *Recommendation:* ship ephemeral now; track `room serve` as a follow-up. **Needs product sign-off.**
- **OQ-2 — Peer address book / discovery.** No persistent store of peer `EndpointAddr`s exists. This
  spec relies on iroh discovery + an explicit `--peer` override (D5). Should learned peer addresses be
  cached locally (e.g. in `rooms.db` or a `peers.json`)? *Recommendation:* defer; revisit with #19/#9
  Gate-A.
- **OQ-3 — Display: poll vs. push.** D6 polls `Node::room_tail`. A push-based "event applied" broadcast
  from the engine would be lower-latency and cleaner. *Recommendation:* poll for MVP; add a broadcast
  later if latency matters.
- **OQ-4 — Self-parent rule (Spike §1).** The landed validator/fold does **not** enforce that a
  non-genesis event cite the author's own latest prior event. `build_message_text` follows the landed
  `invite.rs` head-selection (D8). Is enforcing self-parent (needed for clean equivocation detection /
  `admin_seq`) in scope anywhere? *Recommendation:* out of scope here; track separately.
- **OQ-5 — Does `room join` (#19) land before or after this issue?** The two-peer test needs a real
  second Active member. *Recommendation:* if #19 is not ready, seed membership via core builders or
  `#[ignore]` the two-peer test; do not block the authoring/unit ACs on #19.
- **OQ-6 — Audit surfacing in `room tail`.** Should `room tail` print receive-side drop reason codes
  (Troubleshooting UX) by default, behind `--verbose`, or not at all in this issue? *Recommendation:*
  expose a minimal `--verbose` that prints `SyncEngine::logs()`; keep the default quiet.

---

## 13. Out of scope

- `room join` itself (#19), file sharing, live pipes, agent status — sibling issues.
- Offline/queued delivery, full history reconciliation, multi-device (PRD §15.5, §13.4).
- The `MembershipSnapshot` re-point of the net admission gate from the prototype `AllowlistAdmission`
  to a live snapshot adapter beyond what D7 needs (the net crate tracks that re-point separately).
- Real-NAT delivery confirmation (#9 Gate A).
- Message edit/delete, reactions, threads beyond the `in_reply_to` field already in the schema.

---

## 14. File-change summary

| File | Change |
|---|---|
| `crates/iroh-rooms-core/src/event/message.rs` | **new** — `build_message_text` (D1) + tests |
| `crates/iroh-rooms-core/src/event/mod.rs` | add `pub mod message;` + `pub use message::build_message_text;` |
| `crates/iroh-rooms-core/src/sync/engine.rs` | **add** `room_tail(limit)` passthrough (D6) + test |
| `crates/iroh-rooms-net/src/node.rs` | **add** `Cmd::Tail` + `Node::room_tail(limit)` handle (D6) |
| `crates/iroh-rooms-cli/Cargo.toml` | core `["sync"]`; add `iroh-rooms-net`, `tokio`, `iroh` (D2) |
| `crates/iroh-rooms-cli/src/message.rs` | **new** — `room send` + `room tail` orchestration (D3–D10) |
| `crates/iroh-rooms-cli/src/cli.rs` | `RoomAction::{Send, Tail}` + async dispatch (D2) |
| `crates/iroh-rooms-cli/tests/message_cli.rs` | **new** — authoring + pre-IO gates + secret hygiene (§9.2) |
| `crates/iroh-rooms-cli/tests/message_two_peer.rs` | **new** — two-peer live send/receive (§9.3) |
| `docs/getting-started.md`, `tests/docs_conformance.rs` | reconcile Step 4 to "implemented" (Step 8) |
| `README.md` | add the landed-feature paragraph (house style) |
