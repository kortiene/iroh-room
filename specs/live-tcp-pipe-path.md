# Spec: Prototype Live TCP Pipe Path (Live Pipe Plane — authenticated TCP-over-QUIC forwarding)

| | |
|---|---|
| **Issue** | #14 — [IR-0010] Prototype live TCP pipe path |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/spike, type/security, area/pipe, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §9.3 (Live Pipe Plane), §13.2 (Pipe Security Requirements), §15.7 (Open Live Pipe), §10.6 (Pipe Authorization Lifecycle), §11.3 (`pipe.opened` event), §16 (CLI), §18.3/§18.5 (security risks). `PHASE-0-SPIKE.md` Membership & Ordering **§5** (pipe/blob authorization at connect-accept time + revocation-on-learn), Event Protocol §7 (`pipe.opened`/`pipe.closed` schema), §8 (rejection taxonomy), Spike Plan **Day 9** (Live Pipe confirmation), Test Vectors §17, Residual Risks #2 (removal bounded by reachability) and #4 (removed-member pollution). |
| **Status** | Landed — prototype in shipping crates as of issue #14 / IR-0010. |
| **Type** | Prototype landing in the **shipping** crates (mirrors how IR-0005 landed the event transport as a "prototype" in `iroh-rooms-net`, not a throwaway). Confirmation-grade, no production hardening beyond the §5 model. |

---

## 1. Summary

Confirm that the **Live Pipe Plane** can expose a local TCP service (e.g. a dev server on
`127.0.0.1:3000`) to an **explicitly authorized** room peer, over an authenticated
peer-to-peer QUIC tunnel, and **only** to that peer — proving the PRD's most differentiated
feature end to end.

The deliverable is the pipe transport plane itself: a custom ALPN `/iroh-rooms/pipe/1`
registered on the **same shared `Router`** as the landed event ALPN ("one `Endpoint`,
multiple `.accept()` chains" — `transport.rs:220`), a **connect-accept gate** that composes
the already-landed pure predicate
[`pipe_connect_allowed`](../crates/iroh-rooms-core/src/membership/access.rs) (no default-all;
`allowed_members ∩ Active`), bidirectional **TCP↔QUIC splicing** to a loopback target, and
**tear-down-on-learn** when authorization is revoked or the pipe is closed.

Concretely the implementation must demonstrate, over two (or three) in-process `Node`s on a
loopback network plus a local echo TCP server:

1. **Authorized forward.** An allow-listed Active member connects through the pipe and
   round-trips bytes to the owner's local TCP service.
2. **Non-allowlisted member rejected.** An Active member *not* in `allowed_members` is denied
   (`not_allowed`); no bytes are forwarded.
3. **Non-member rejected.** An unknown / non-Active device is denied at the QUIC accept gate
   (`unknown_device` / `not_active`) before any handshake byte is read.
4. **Pipe closes on owner exit or explicit close.** Closing emits a signed `pipe.closed`, tears
   down live sessions, and makes subsequent connects fail (`closed`).
5. **Revocation-on-learn.** When the connected peer is removed and the removal reaches the
   owner, the owner immediately tears down the active session and denies reconnection.

This document is detailed enough to execute without re-deriving scope. Unlike the blob ACL
spike (`spike-blobs`, IR-0009), this path lands in the **shipping** crates, because the data
model (`PipeOpened`/`PipeClosed`), the access predicate (`pipe_connect_allowed`), and the
transport adapter (`iroh-rooms-net`) it needs are all **already landed** there — see §2 and
the decision in §4.1.

---

## 2. Background & current repository state

**Read before starting:**

- `PHASE-0-SPIKE.md`:
  - **Membership & Ordering §5** — the normative pipe authorization model. The **Pipe connect
    gate** accepts a connection for `pipe_id` only if ALL hold against the **local snapshot**:
    (1) remote identity ∈ Active members; (2) remote identity ∈ the `allowed_members` of the
    governing `pipe.opened` (no default-all — PRD §13.2); (3) that `pipe.opened` is authored by
    an Active member who is the pipe owner; (4) no `pipe.closed` for `pipe_id` is causally
    known; (5) `expires_at` (if present) > local wall clock — **the one place wall clock is
    consulted, and only to deny (fail-closed)**. Reject otherwise + write a local audit event
    (`pipe.connect.rejected`). **Revocation-on-learn:** long-lived pipe connections subscribe to
    membership changes; when the connected peer becomes Removed/Left, or `pipe.closed`/expiry
    fires, the enforcing node **immediately tears down** the live connection and logs it.
    Exposure is bounded to "until the enforcing peer learns of the change" (§0 / Residual #2).
  - **Event Protocol §7** — the `pipe.opened` / `pipe.closed` content schemas (reproduced in
    §6.1 below) and the §7 registry rows: `pipe.opened` signer = any current member with
    `owner_id == sender_id`; `pipe.closed` signer = the pipe owner **or** the admin.
  - **Event Protocol §8** — rejection/flag taxonomy (the audit reason vocabulary).
  - **Spike Plan Day 9** — the deliverable and the **soft GATE: GO iff the allowlisted forward
    works and a non-member is refused.** (Day 9 names `dumbpipe 0.39` as a library; §4.2 below
    explains why this landing hand-rolls the splice on iroh 1.0 core instead, consistent with
    the rest of the repo.)
  - **Test Vectors §17** — the pipe authorization scenario.
- `PRD.v0.3.md` §9.3 (Live Pipe Plane; "authenticated TCP forwarding between two peers" only —
  no terminal/Unix-socket/multiplex in MVP), §13.2 (the eight pipe security requirements),
  §15.7 (the `pipe expose/connect/close` journey + ACs), §10.6 (pipe authorization lifecycle:
  explicit allowed members, session id, close-on-exit, `pipe.opened`/`pipe.closed` audit
  events), §16 (CLI commands + "Pipe exposure should show a clear security warning").

**Current repo state — what is already landed and reused (this is why the path is "near-free"):**

- **Core data model (shipping, `iroh-rooms-core`).**
  - `event::content::PipeOpened` and `PipeClosed` structs, with strict deterministic-CBOR
    parse/encode and the §7 invariants enforced on validate (`owner_id == sender_id`,
    `kind == "tcp"`, `allowed_members` **non-empty**) —
    `crates/iroh-rooms-core/src/event/content.rs:230-260,711-736`.
  - `EventType::PipeOpened` / `PipeClosed` are registered, validated, ordered, stored, and
    synced by the landed validator / membership fold / store / `SyncEngine` (so all the
    *log-correctness* of pipe events is already conformance-tested — this issue adds the
    *transport plane*, not new event-validity logic).
  - `membership::access::pipe_connect_allowed(snapshot, connecting_device, pipe, now_ms)` —
    the **pure** gate predicate, already landed and unit-shaped to the model
    (`crates/iroh-rooms-core/src/membership/access.rs:110-134`). Returns
    `PipeDecision::Accept` or `Reject(DenyReason)` with reasons `UnknownDevice`, `NotActive`,
    `NotAllowed`, `OwnerInactive`, `Expired`. It consults the **current** `MembershipSnapshot`
    (not an ancestor view), exactly per §5.
  - `MembershipSnapshot::identity_of_device` + `is_active` — the device→identity reverse map +
    Active set the gate resolves against.
- **Transport adapter (shipping, `iroh-rooms-net`, IR-0005).** The full-mesh QUIC carrier the
  pipe plane extends:
  - `transport.rs` builds **one `Endpoint` + one `Router`**; the event ALPN is the first
    `.accept()` chain and the code already documents "the blob/pipe ALPNs chain `.accept()`
    here later (ADR-1)" (`transport.rs:220`). The pipe ALPN is the **second** chain.
  - The **admission gate** pattern: `admission::Admission` trait + `AllowlistAdmission`
    (device→identity→Active, fail-closed default, with the `fail_closed` overlay seam),
    enforced in `handler.rs` **before `accept_bi()`** (`unauthorized peer's first byte is never
    read`). The pipe handler reuses this for stage-1 admission (§5.3).
  - `audit::AuditSink` (`TracingAudit`) with stable greppable reason codes
    (`peer.rejected:<cause>`); extended with pipe lines in §6.6.
  - `node.rs` `Node`: a runtime pairing `NetTransport` with the `SyncEngine`, exposing
    `publish`, `snapshot`, `room_tail`, `conn_events`, `endpoint_addr`, `connect_to`,
    `shutdown`, and the `wait_*` test helpers. The pipe plane adds a sibling owner/connector
    surface (§6.5).
  - The **loopback test harness** (`tests/loopback.rs`, `tests/message_e2e.rs`,
    `demo::Participant`) — the exact AC-oracle pattern this issue's e2e suite copies
    (`NetMode::Loopback`, `RelayMode::Disabled`, every await `tokio::time::timeout`-bounded).
- **CLI (shipping, `iroh-rooms-cli`).** Identity, room create/members/invite, and the online
  `room send` / `room tail` commands are landed; the binary is the natural home for the user-
  facing `pipe expose/connect/close/list` subcommands (§6.5.3). There is **no** pipe scaffold
  yet (the README's "scaffold for file, pipe, agent" is aspirational).

**Dependency implication:** unlike IR-0009, the membership fold, event-core, store, and
transport **all exist**. The pipe plane is a thin, well-supported addition: an ALPN handler +
a forwarding splice + a teardown watcher + two core builders + CLI wiring. No fixtures /
stubs of the membership model are needed — the real `MembershipSnapshot` drives the gate.

---

## 3. Goal, scope, and non-goals

### 3.1 Goal (what "confirmed" means)

On a running owner/connector pair, prove that the Live Pipe Plane enforces the §5 pipe connect
gate end to end: **per-member admission** (proven `EndpointId` → identity → Active),
**explicit per-pipe authorization** (`allowed_members ∩ Active`, no default-all),
**bidirectional TCP forwarding** to a loopback target over the encrypted QUIC tunnel,
**explicit/owner-exit close** with a signed `pipe.closed`, and **tear-down-on-learn** when the
connected peer's authorization is revoked. Satisfy the Day-9 soft GATE (allowlisted forward
works; non-member refused) and all five issue ACs.

### 3.2 In scope

- A new `iroh-rooms-net` **`pipe` module** (and `/iroh-rooms/pipe/1` ALPN) registered on the
  shared `Router`.
- The **owner ("expose") side**: a `PipeRegistry` of open pipes, the pipe accept handler, the
  connect gate composing `pipe_connect_allowed` + `pipe.closed`-known + expiry, and TCP↔QUIC
  splicing to a **loopback** target.
- The **connector ("connect") side**: a **loopback-only** local TCP listener that dials the
  owner over the pipe ALPN, performs the pipe handshake, and splices local TCP ↔ QUIC.
- **Lifecycle events**: author + publish `pipe.opened` on expose and `pipe.closed` on
  explicit close / owner exit / expiry, via two new **pure core builders** (§6.2), fanned out
  over the existing event plane.
- **Tear-down-on-learn**: a watcher that re-evaluates each live session against the current
  snapshot + pipe status and tears down any that no longer pass the gate.
- **Local audit**: pipe-specific reason lines (`pipe.opened`, `pipe.closed`,
  `pipe.connect.accepted`, `pipe.connect.rejected:<cause>`, `pipe.torndown:<cause>`).
- **CLI `pipe` subcommands** (`expose`, `connect`, `close`, `list`) with the PRD §13.2/§16
  security warning and the exposed-target / allowed-member / pipe-id / close-command display.
- An **e2e loopback test suite** (the AC oracle, §8) plus a runnable demo binary or
  `pipe`-flavored extension of the existing `net-smoke`.

### 3.3 Out of scope / non-goals

- **Real-NAT / multi-machine** confirmation — that is Day-1/Day-10 (Gate A / Gate E) work, and
  the open Gate-A risk is inherited from the transport prototype (#9, `iroh-rooms-net/NOTES.md`).
  This issue proves the plane on loopback.
- **Terminal sharing, Unix-socket forwarding, multiplexed services, browser-native UX** —
  explicitly post-MVP (PRD §9.3). MVP is `kind == "tcp"` only.
- **Per-pipe revocation lists / key rotation** — none in MVP; removal/leave is the only
  revocation, enforced fail-closed + tear-down-on-learn, **bounded by removal-event
  reachability** (Residual Risk #2), not "briefly". Document, do not build.
- **`dumbpipe` as a dependency** — see §4.2 (hand-roll on iroh core instead).
- **Removed-member timeline pollution** mitigation (a removed member can still author a
  log-valid but capability-**zero** `pipe.opened`) — contained for safety by the gate
  (owner must be Active); UI segregation is a documented MVP limitation (Residual #4), not built
  here.
- **`room join` (#19)** — the e2e suite seeds membership via core event builders (as
  `message_e2e.rs` already does) to avoid the join-CLI dependency.
- **Multi-pipe stress / large-room** scale, fuzzing, formal threat model.

---

## 4. Key design decisions

### 4.1 D1 — Land in the shipping crates, not a throwaway `spike-pipe`

The Day-9 plan named a throwaway `spike-pipe` crate (parallel to `spike-blobs`). That made
sense when **no** protocol code existed. It no longer does: `PipeOpened`/`PipeClosed`,
`pipe_connect_allowed`, the membership fold, and the `iroh-rooms-net` carrier are all landed in
shipping crates, and the transport code already reserves the pipe ALPN seam. Spiking in an
isolated crate would mean **re-stubbing** the membership snapshot and the transport — exactly
the work the repo already did for real. Precedent: **IR-0005 ("prototype full-mesh QUIC event
transport") landed in shipping `iroh-rooms-net`**, not a throwaway, and "prototype" there meant
"first real cut, loopback-proven, real-NAT pending." This issue follows the same pattern.
*Consequence:* this spec **does** describe production-code changes (unlike the blob spike). The
quality bar is the workspace bar (`scripts/verify.sh`: fmt + clippy `pedantic` + tests,
`unsafe_code = forbid`).

### 4.2 D2 — Hand-roll the splice on iroh 1.0 core; do not add `dumbpipe`

`dumbpipe 0.39` is a 0.x crate; the entire repo is built on the principle "minimize 0.x crates
on the load-bearing path" (ADR-1/ADR-2 churn argument), and `iroh-rooms-net` deliberately uses
**only** iroh 1.0 stable core (`Endpoint`/`Router`/`ProtocolHandler`/`Connection`/`EndpointId`).
dumbpipe's model is also a single bearer-ALPN tunnel with no concept of a `MembershipSnapshot`
or per-pipe `allowed_members`, so its authorization would have to be bolted on anyway. The
forwarding itself is small: `conn.accept_bi()`/`open_bi()` give an ordered reliable byte stream,
and `tokio::io::copy_bidirectional` splices it to a `tokio::net::TcpStream`. We **read**
dumbpipe as a reference for the splice/`copy_bidirectional` shape, but add **zero** new
dependencies beyond `tokio`'s `net` feature. *(Open to override — OQ-1.)*

### 4.3 D3 — Two-stage gate: transport admission, then per-pipe authorization

Authorization is layered so that the cheapest, strongest check runs first and unauthorized
peers cost the least:

- **Stage 1 — transport admission (at QUIC `accept`, before any byte).** Reuse the landed
  `Admission` gate: resolve the proven `device_id → identity → Active?`. A non-member /
  non-Active device is closed **before `accept_bi()`** (identical to the event plane). This
  satisfies AC3 (non-member rejected) with no handshake.
- **Stage 2 — per-pipe authorization (after reading the pipe handshake).** Accept the bidi
  stream, read a tiny `PipeHello{ pipe_id }` control frame, look up the governing `pipe.opened`
  from the engine's validated set, and run `pipe_connect_allowed(snapshot, device, &pipe, now)`
  **plus** the `pipe.closed`-known check. An Active member not in `allowed_members` is rejected
  here (`not_allowed`, AC2). Only on `Accept` is a byte forwarded.

The `pipe_id` is application data, so it necessarily arrives *after* the QUIC handshake — but
it arrives on a control frame **before** any TCP payload is spliced, so "no bytes forwarded
before the gate passes" holds. The proven `device_id` (stage 1) is never re-derived from
self-asserted fields.

### 4.4 D4 — One QUIC connection per (connector→owner, pipe); one bidi stream per forwarded TCP connection

A connector's local listener may accept many concurrent TCP connections. Each maps to **one
QUIC bidi stream** (carrying its own `PipeHello` + spliced bytes) over a **single** reused QUIC
connection to the owner. The owner's accept handler loops `accept_bi()` and gates **each**
stream independently against the **current** snapshot — so revocation between streams is
enforced naturally for new streams, and the teardown watcher (§4.5) handles in-flight ones.
This matches the "many TCP conns over one tunnel" UX without a connection per TCP conn.

### 4.5 D5 — Tear-down-on-learn via snapshot re-evaluation on the anti-entropy tick

The owner holds, per live session, the connecting `device_id` + the governing `pipe_id`. A
lightweight **watcher** re-evaluates every live session on each anti-entropy tick (the existing
`Node` tick, default 250 ms): recompute `pipe_connect_allowed` against the **current** snapshot
and re-check `pipe.closed`-known / expiry; if a session no longer passes, close its QUIC
connection and audit `pipe.torndown:<cause>`. Poll-based teardown is the simplest correct
implementation and its latency bound (≤ one tick after the owner *learns* of the change) is
exactly the §5 / Residual-#2 guarantee ("bounded by reachability, then immediate"). A push-based
membership-change broadcast is noted as a refinement (OQ-3).

### 4.6 D6 — Loopback-only binds by default (PRD §13.2.3)

- **Owner `--tcp <target>`**: the forward target must be a **loopback** address
  (`127.0.0.0/8` / `::1`). A non-loopback target is **rejected** in the prototype (with a clear
  error); a future `--allow-non-loopback` escape hatch is out of scope.
- **Connector `--local <port>`**: the local listener binds **`127.0.0.1:<port>`** only, never
  `0.0.0.0`. This keeps the tunnel mouth private to the connector's host.

### 4.7 D7 — Pipe lifecycle events via pure core builders

`pipe.opened` / `pipe.closed` are authored through **new pure builders** in
`iroh-rooms-core::event` (`build_pipe_opened`, `build_pipe_closed`), siblings of the landed
`build_message_text` / `build_member_invited`: deterministic in their inputs (caller injects
`prev_events = heads`, `created_at`, and the CSPRNG `pipe_id`), clock-/RNG-free, golden-testable,
self-validated through the §6 pipeline, then `Node::publish`-ed so the engine persists and fans
them out. This keeps the byte-exact assembly point in `core` (where the conformance harness can
golden-test it), not in the net/CLI layer.

---

## 5. Architecture

```text
            OWNER (exposer)                                  CONNECTOR
   ┌───────────────────────────────┐               ┌──────────────────────────────┐
   │ iroh-rooms pipe expose        │               │ iroh-rooms pipe connect       │
   │  --tcp 127.0.0.1:3000         │               │  <room> <pipe-id> --local 3001│
   │                               │               │                              │
   │  build_pipe_opened ──publish──┼──event plane──┼─▶ engine: validate/fold/store │
   │  (pipe.opened fanned out)     │  (ALPN /event)│   learns owner_endpoint,alpn  │
   │                               │               │                              │
   │  Router .accept(/pipe/1)      │               │  127.0.0.1:3001 TCP listener  │
   │   stage1: Admission gate ◀────┼── QUIC /pipe/1┼── dial owner_endpoint         │
   │   stage2: PipeHello{pipe_id}  │   bidi stream │   send PipeHello{pipe_id}     │
   │     pipe_connect_allowed +    │◀═════════════▶│   splice localTCP ↔ QUIC      │
   │     closed/expiry check       │  spliced bytes│                              │
   │   on Accept: dial 127.0.0.1:3000              │                              │
   │     copy_bidirectional(QUIC,TCP)              │                              │
   │                               │               │                              │
   │  watcher (each tick):         │               │                              │
   │   re-eval live sessions vs    │               │                              │
   │   snapshot → teardown on      │               │                              │
   │   removal/close/expiry        │               │                              │
   │  on close/exit: build_pipe_closed ──publish──▶│                              │
   └───────────────────────────────┘               └──────────────────────────────┘
```

- **Identity unification:** the `EndpointId` proven at QUIC accept **is** the connector's
  `device_id`, the same key the membership fold binds to an identity — so the gate needs no
  application-level identity assertion.
- **Encryption:** every byte rides the QUIC/TLS tunnel between two authenticated endpoints
  (PRD §15.7.6 "traffic carried over an encrypted peer connection") — satisfied for free by
  iroh.
- **Access vs log-validity split (§5):** the gate uses the **current global snapshot**, so a
  since-removed owner's log-valid `pipe.opened` grants **zero** access (`OwnerInactive`), and a
  since-removed connector is torn down. Log inclusion of those events is unaffected.

---

## 6. Design detail & API surface

### 6.1 Event schemas (recap, Event Protocol §7 — already enforced by core)

```
pipe.opened.content = {
  "pipe_id":         bstr[16],
  "owner_id":        bstr[32],   // MUST == sender_id
  "owner_endpoint":  bstr[32],   // EndpointId to dial (== owner device_id)
  "kind":            "tcp",      // only value in MVP
  "label":           tstr,
  "target_hint":     tstr,       // advisory only (e.g. "localhost:3000")
  "alpn":            tstr,       // "/iroh-rooms/pipe/1"
  "allowed_members": [bstr[32], …],  // non-empty; no default-all
  "expires_at":  opt uint
}
pipe.closed.content = {
  "pipe_id": bstr[16],           // references an open pipe.opened.pipe_id
  "reason":  opt tstr            // "closed" | "expired" | "owner_exit" | "error"
}
```

`target_hint` is advisory and **never** trusted: the *real* forward target lives only in the
owner's local `PipeRegistry`, never on the log (a connector cannot redirect the owner's
forward). `allowed_members` is the trust input; the gate uses it directly.

### 6.2 New pure core builders (`iroh-rooms-core::event`)

```rust
// event/pipe.rs (new), re-exported from event::mod as build_pipe_opened / build_pipe_closed
pub fn build_pipe_opened(
    owner_identity_secret: &SigningKey,   // provides owner_id (== sender_id)
    owner_device_secret:  &SigningKey,    // signs; sig verifies under device_id
    room_id:              &RoomId,
    pipe_id:              [u8; SHORT_ID_LEN],
    owner_endpoint:       &DeviceKey,     // == owner device_id
    label:                &str,
    target_hint:          &str,
    alpn:                 &str,
    allowed_members:      &[IdentityKey], // MUST be non-empty
    expires_at:           Option<u64>,
    prev_events:          &[EventId],     // room heads, caller-injected
    created_at:           u64,            // clock read, caller-injected
) -> WireEvent;

pub fn build_pipe_closed(
    signer_identity_secret: &SigningKey,  // owner OR admin
    signer_device_secret:   &SigningKey,
    room_id:                &RoomId,
    pipe_id:                [u8; SHORT_ID_LEN],
    reason:                 Option<&str>,
    prev_events:            &[EventId],
    created_at:             u64,
) -> WireEvent;
```

Mirror `build_message_text` exactly: `kind` is hardcoded `"tcp"`; optional fields follow the §7
omit-when-empty rule; the function is pure and golden-testable (no RNG/clock inside). Self-
validate the produced `WireEvent` through `validate_wire_bytes` in tests.

### 6.3 The pipe ALPN (`iroh-rooms-net::pipe::alpn`)

```rust
pub const PIPE_ALPN: &[u8] = b"/iroh-rooms/pipe/1";   // 18 bytes; pin byte-for-byte in a test
```

Asserted byte-for-byte like `EVENT_ALPN` (it is a wire contract shared with every other
implementation). It matches `pipe.opened.content.alpn`.

### 6.4 The pipe control frame (`PipeHello`)

The first bytes on each accepted pipe bidi stream are a length-prefixed control frame (reuse
the `frame.rs` length-prefix helper):

```rust
struct PipeHello { v: u8 /* == 1 */, pipe_id: [u8; 16] }   // deterministic-CBOR or fixed 17 bytes
```

After `PipeHello`, the remainder of the stream is **raw spliced TCP bytes** (no further
framing). The owner reads exactly one `PipeHello`, gates, then either splices or sends a
1-byte reject + closes the stream with a stable code.

### 6.5 `iroh-rooms-net::pipe` module surface

```text
crates/iroh-rooms-net/src/pipe/
  mod.rs        # re-exports; PipeError
  alpn.rs       # PIPE_ALPN
  hello.rs      # PipeHello encode/decode (+ reject code)
  registry.rs   # PipeRegistry: pipe_id -> OpenPipe { PipeOpened, target: SocketAddr }
  handler.rs    # PipeProtocolHandler: ProtocolHandler for PIPE_ALPN (stage1+stage2 gate, splice)
  owner.rs      # expose(): register pipe + publish pipe.opened; close(): publish pipe.closed + teardown
  connector.rs  # connect(): local TCP listener -> dial owner -> hello -> splice
  watcher.rs    # tear-down-on-learn: re-eval live sessions each tick
  audit.rs      # PipeAuditSink (extends/peers with net::audit reason codes)
```

**6.5.1 Owner side.** `PipeRegistry` maps `pipe_id → OpenPipe { opened: PipeOpened, target:
SocketAddr }`. `expose(...)` validates the loopback target (D6), draws a CSPRNG `pipe_id`,
inserts into the registry, then `build_pipe_opened` + `Node::publish` (fanned out over the event
plane). `PipeProtocolHandler::accept(conn)`:

1. `device = conn.remote_id()`; run the landed `Admission` (stage 1). Reject → close before
   `accept_bi`, audit `pipe.connect.rejected:<unknown_device|not_active>`.
2. `loop { let (send,recv) = conn.accept_bi().await?; spawn(handle_stream) }`.
3. `handle_stream`: read `PipeHello{pipe_id}`; look up `OpenPipe` (engine: governing
   `pipe.opened` + `pipe.closed`-known); compute the gate:
   - `pipe.opened` missing / `pipe.closed` known → reject `closed`.
   - `pipe_connect_allowed(snapshot, device, &opened, now_ms)` → `Reject(reason)` → reject
     (`not_active|not_allowed|owner_inactive|expired`).
   - `Accept` → audit `pipe.connect.accepted`; `TcpStream::connect(target)`;
     `copy_bidirectional(quic_stream, tcp_stream)`; register the live session in the watcher's
     table keyed by `(device, pipe_id, conn handle)`.

**6.5.2 Connector side.** `connect(node, pipe_id, local_port)`:
1. Look up the `pipe.opened` for `pipe_id` from the node's validated set → `owner_endpoint`,
   `alpn`. (Error clearly if unknown — the connector must have synced the `pipe.opened`.)
2. Bind `TcpListener` on `127.0.0.1:local_port` (D6).
3. Dial `owner_endpoint` over `PIPE_ALPN` once (reuse the connection).
4. Per accepted local TCP conn: `conn.open_bi()`; write `PipeHello{pipe_id}`; splice
   `copy_bidirectional(tcp, quic_stream)`. On owner reject (stream closed with the reject code,
   or 1-byte reject) surface a clear `denied` outcome.

**6.5.3 CLI surface (`iroh-rooms-cli`, `iroh-rooms pipe …`).** Wire the net module:

| Command | Behavior |
|---|---|
| `pipe expose <ROOM_ID> --tcp <ADDR> --allow <IDENTITY_ID>… [--label <S>] [--expires <DUR>] [--peer <ADDR>]…` | Confirm caller is an Active member; reject non-loopback `--tcp` and empty `--allow`; print a **security warning** + exposed target + allowed members + `pipe_id` + the exact `pipe close <pipe-id>` command; bring up a `Node`, publish `pipe.opened`, run the accept handler + watcher until Ctrl-C; on exit publish `pipe.closed{reason:"owner_exit"}`. |
| `pipe connect <ROOM_ID> <PIPE_ID> --local <PORT> [--peer <ADDR>]…` | Resolve the `pipe.opened`; bind loopback listener; forward; print the local addr to use. |
| `pipe close <PIPE_ID>` | Publish `pipe.closed{reason:"closed"}` (owner or admin); tear down. |
| `pipe list <ROOM_ID>` | List open pipes (a `pipe.opened` with no causally-known `pipe.closed`) with owner / label / allowed / expiry. |

Distinguish failure modes per PRD §16.3 (`offline peer` vs `unauthorized` vs `invalid pipe`).
Output is script-friendly labeled lines, like the landed `room` commands.

> **Scope note (recommended split).** The **gating** deliverable for the issue ACs and the
> Day-9 GATE is the **net `pipe` module + the e2e suite (§8)** — that alone proves all five ACs
> (the e2e suite drives the owner/connector APIs directly, exactly as `loopback.rs` drives the
> transport without the CLI). The **CLI subcommands** complete the PRD §15.7 user journey and
> are in scope here, but MAY be split into an immediate follow-up issue if the net-layer
> confirmation must land first (precedent: IR-0005 landed net + loopback tests with **no** CLI;
> the CLI commands arrived in later issues). Recommend landing both together if schedule allows.

### 6.6 Observability / audit vocabulary (extends `net::audit`, stable strings)

| Reason string | Level | When |
|---|---|---|
| `pipe.opened` | INFO | owner published a `pipe.opened` (`pipe_id`, `allowed` count) |
| `pipe.closed` | INFO | `pipe.closed` published (`pipe_id`, `reason`) |
| `pipe.connect.accepted` | INFO | a stream passed both gate stages (`device`, `pipe_id`) |
| `pipe.connect.rejected:unknown_device` | WARN | stage 1: device bound to no identity |
| `pipe.connect.rejected:not_active` | WARN | stage 1: identity not Active |
| `pipe.connect.rejected:not_allowed` | WARN | stage 2: Active member not in `allowed_members` |
| `pipe.connect.rejected:owner_inactive` | WARN | stage 2: pipe owner not Active |
| `pipe.connect.rejected:expired` | WARN | stage 2: `expires_at` passed (the one wall-clock use) |
| `pipe.connect.rejected:closed` | WARN | stage 2: `pipe.closed` causally known / unknown pipe |
| `pipe.torndown:not_active` / `:not_allowed` / `:owner_inactive` / `:closed` / `:expired` | WARN | watcher tore down a live session (revocation-on-learn) |

The reject causes map 1:1 to the core `DenyReason` (`access.rs`) plus `closed`. These strings
are pinned in a test (a parser-breaking silent rename is caught).

---

## 7. Implementation steps

Work top to bottom; each step is independently observable.

1. **Core builders (`iroh-rooms-core`).** Add `event/pipe.rs` with `build_pipe_opened` /
   `build_pipe_closed` (§6.2); re-export from `event::mod`. Add unit + golden tests: assemble →
   `validate_wire_bytes` accepts; tamper (`owner_id != sender_id`, empty `allowed_members`,
   `kind != "tcp"`) → the existing strict parser rejects with the right `RejectReason`. No
   change to validator/fold (already handle these types). Keep `core` clock-/RNG-free.
2. **Thin engine/Node read passthroughs.** Add `SyncEngine::pipe_opened(pipe_id) ->
   Option<PipeOpened>` and `pipe_is_closed(pipe_id) -> bool` (read-only over the validated set,
   like `room_tail`), surfaced on `Node` via the pump `Cmd` pattern. These feed the gate's
   "governing `pipe.opened`" + "`pipe.closed`-known" lookups without a second store handle.
3. **Pipe ALPN + control frame (`net::pipe::alpn`, `hello.rs`).** Define `PIPE_ALPN` (pinned
   test) and `PipeHello` encode/decode + reject code; reuse the `frame.rs` length-prefix helper.
4. **`PipeRegistry` (`registry.rs`).** `pipe_id → OpenPipe { opened, target }`; loopback-target
   validation (D6) lives here. Insert on expose, remove on close.
5. **`PipeProtocolHandler` (`handler.rs`).** Stage-1 admission (reuse `Admission`) before
   `accept_bi`; per-stream stage-2 gate (`pipe_connect_allowed` + closed/expiry) reading
   `PipeHello`; on Accept, `copy_bidirectional` to the loopback `TcpStream`. Audit every
   decision. Register the handler as the **second** `.accept(PIPE_ALPN, …)` chain on the shared
   `Router` (extend `transport.rs:220`), so one `Endpoint` serves both ALPNs.
6. **Connector (`connector.rs`).** Loopback listener → dial owner over `PIPE_ALPN` →
   `PipeHello` → splice. Bounded connect timeout; classify `Forwarding | DeniedAtConnect |
   DeniedPerPipe | OwnerOffline`.
7. **Owner lifecycle (`owner.rs`).** `expose` (validate target, draw `pipe_id`, register,
   `build_pipe_opened` + publish) and `close` (`build_pipe_closed` + publish + teardown). Hook
   `Node::shutdown` / Ctrl-C to publish `pipe.closed{reason:"owner_exit"}` best-effort.
8. **Teardown watcher (`watcher.rs`).** A per-owner task that, each tick, re-evaluates every
   live session against the current snapshot + pipe status and closes any that no longer pass,
   auditing `pipe.torndown:<cause>`. Drive it off the existing `Node` tick (D5).
9. **CLI `pipe` subcommands (`iroh-rooms-cli`).** `expose/connect/close/list` (§6.5.3) with the
   security warning, loopback enforcement, and §16.3 failure-mode distinction. Integration tests
   via `assert_cmd` for arg validation (non-loopback `--tcp` rejected, empty `--allow` rejected,
   non-admin/non-owner `close` rejected) and secret hygiene (no key bytes in output).
10. **E2e suite + demo (§8).** `tests/pipe_e2e.rs` (the AC oracle) + a narrated demo (extend
    `net-smoke` or add a `pipe-smoke` bin). Run `scripts/verify.sh` green.

---

## 8. Test plan & acceptance-criteria mapping

E2e suite `crates/iroh-rooms-net/tests/pipe_e2e.rs`, modeled on `loopback.rs` /
`message_e2e.rs`: `NetMode::Loopback`, `RelayMode::Disabled`, membership seeded via core
builders (Alice admin/owner, Bob allowed member, Carol Active non-allowed, Mallory non-member),
every await `tokio::time::timeout`-bounded. A trivial in-test **echo TCP server** on
`127.0.0.1:0` is the owner's forward target; the connector binds `127.0.0.1:0` and the test
writes/reads through it.

| # | Scenario | Expected | AC |
|---|---|---|---|
| P1 | Bob (Active, in `allowed_members`) connects; test writes `ping` through the connector's local socket | bytes echo back `ping`; audit `pipe.connect.accepted`; gate `Accept` | **AC1** (authorized member forwards to the local TCP service) |
| P2 | Carol (Active, **not** in `allowed_members`) connects | rejected `not_allowed`; **zero** bytes forwarded; echo server sees no connection | **AC2** (non-allowlisted member rejected) |
| P3 | Mallory (unknown device / non-member) dials `PIPE_ALPN` | rejected at **stage 1** before any `PipeHello` is read; `unknown_device`/`not_active`; connection closed with the reject code | **AC3** (non-member rejected) |
| P4a | Owner runs `close` while Bob is forwarding | `pipe.closed` published; Bob's live session torn down (`pipe.torndown:closed`); a fresh Bob connect → `closed` | **AC4** (explicit close) |
| P4b | Owner `Node` shuts down while Bob is forwarding | live session drops; graceful path publishes `pipe.closed{reason:"owner_exit"}` | **AC4** (owner exit) |
| P5 | Bob forwarding; admin publishes `member.removed(Bob)`; removal reaches the owner | within ≤1 tick the owner tears down Bob's session (`pipe.torndown:not_active`); a fresh Bob connect → `not_active` | **AC5** (revocation-on-learn tears down an active session) |
| P6 | (gate unit, `access.rs` already covers; add a net-level assertion) expired pipe | `expired` reject; the one wall-clock consultation, deny-only | §5 expiry |

**Day-9 soft GATE:** P1 (allowlisted forward works) + P2/P3 (non-member/non-allowed refused) =
GO. **Issue ACs:** AC1→P1, AC2→P2, AC3→P3, AC4→P4a/P4b, AC5→P5. **Test Plan (issue):**
"authorized client, unauthorized client, close, and revocation path" = P1 / {P2,P3} / P4 / P5.

---

## 9. Security & privacy notes (PRD §13.2)

- **No default-all.** `allowed_members` is required non-empty (enforced by core on validate)
  and is the sole authorization input; the gate is `allowed_members ∩ Active` — a member not
  named is refused (P2). There is **no** "open to all room members" mode.
- **Loopback-only binds (D6).** The forward target must be loopback (non-loopback `--tcp`
  rejected); the connector's local listener binds `127.0.0.1` only. This is PRD §13.2.3.
- **Explicit, visible exposure.** `pipe expose` prints a clear security warning and shows the
  exposed target, allowed member(s), `pipe_id`, and the exact close command (PRD §13.2.4 /
  §16.2).
- **Close on exit + audit events.** `pipe.closed` is emitted on explicit close and best-effort
  on owner exit (PRD §13.2.5/§13.2.6); every open/connect/reject/close/teardown is locally
  audited with a stable reason (PRD §13.2.7).
- **Revocation is fail-closed + tear-down-on-learn, bounded by reachability.** A removed/left
  peer is denied new streams and has live sessions torn down within one tick of the owner
  learning of the change — **not** instantaneous globally (Residual Risk #2): exposure is
  bounded by removal-event reachability, which the spec documents, not hides.
- **Owner-Active gate contains removed-member `pipe.opened`.** A since-removed member can still
  author a log-valid `pipe.opened`, but it authorizes **nothing** because the gate requires the
  owner to be **currently Active** (`OwnerInactive`) — the §5 access-vs-log-validity split.
- **`target_hint` is never trusted.** The real forward target lives only in the owner's local
  registry; nothing on the log can redirect the owner's TCP connection.
- **No terminal/Unix-socket/multiplex** (PRD §13.2.8 / §9.3) — `kind == "tcp"` only.
- **Encrypted transport** is inherited from iroh QUIC/TLS (PRD §15.7.6).

---

## 10. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | iroh 1.0 `accept_bi`/`open_bi` stream semantics or `copy_bidirectional` half-close behavior differ from assumptions (e.g. one direction's EOF must propagate a FIN to splice cleanly). | Medium | Medium | Prototype the splice early (step 5/6); test half-close (echo server closes first) in P1; document the FIN/`finish()` handling. |
| R2 | Gate reads a **stale** snapshot (race between membership update and a new stream's stage-2 check). | Low | Medium | Stage 2 always reads the **current** `Node::snapshot()` at stream-accept time; the watcher (≤1 tick) covers in-flight sessions; default-deny on any lookup failure. |
| R3 | Teardown latency (tick poll) leaves a revoked session live for up to one tick. | Medium | Low | Accepted + documented (Residual #2: bounded by reachability anyway). Tick is 250 ms; OQ-3 offers a push refinement. |
| R4 | The connector lacks the `pipe.opened` (not yet synced) and cannot resolve `owner_endpoint`. | Medium | Low | Clear `unknown pipe / not yet synced` error; the e2e seeds the `pipe.opened` before connect; CLI hint to `room tail`/sync first. |
| R5 | `pipe_id` collision (CSPRNG 16 B) or two `pipe.opened` with the same id. | Very low | Low | 128-bit random id; registry rejects a duplicate locally; gate keys on the governing `pipe.opened` from the validated set. |
| R6 | Owner-exit `pipe.closed` is best-effort (process killed → no event). | Medium | Low | Connectors observe QUIC connection drop and stop forwarding regardless; the missing `pipe.closed` is a documented best-effort (a hard kill cannot emit). |
| R7 | Scope creep into CLI delays the gating net-layer confirmation. | Medium | Low | §6.5.3 split note: net module + e2e prove the ACs independently; CLI may follow. |
| R8 | Real-NAT behavior (hole-punch / relay for the pipe ALPN) unproven. | Medium (inherited) | Medium | Out of scope (Gate A / Day 10); inherits #9's open Gate-A risk; loopback is this issue's bound. |
| R9 | Strict workspace lints (`pedantic`, `unsafe_code=forbid`) on new async splice code. | Medium | Low | Keep clippy-clean; no `unsafe`; `tokio` `net` feature only. |

---

## 11. Assumptions

1. iroh 1.0 `Connection::open_bi`/`accept_bi` give ordered reliable byte streams suitable for
   raw TCP splicing via `tokio::io::copy_bidirectional`; `tokio` gains the `net` feature in
   `iroh-rooms-net`.
2. The proven `EndpointId` is available at the pipe handler's `accept(conn)` exactly as in the
   event handler (`conn.remote_id()`), so stage-1 admission needs no app data.
3. Seeding membership via core builders (as `message_e2e.rs` does) is acceptable for the e2e
   suite; `room join` (#19) is not a dependency.
4. Two/three in-process loopback `Node`s + an in-test echo server suffice to prove the ACs;
   real-NAT is Day-10's job.
5. The landed `pipe_connect_allowed` + `PipeOpened`/`PipeClosed` semantics are final for MVP
   (they match Event Protocol §7 / §5); this issue does not change them.
6. Reusing the landed `Admission`/`AllowlistAdmission` for stage 1 is correct (the pipe and
   event planes share the same membership snapshot and the same proven-identity model).
7. A snapshot-poll teardown (≤1 tick) meets the §5 "tear-down-on-learn" requirement (the model
   already bounds enforcement by reachability, not latency).

---

## 12. Open questions

- **OQ-1 (dumbpipe vs hand-roll).** D2 recommends hand-rolling on iroh core (zero new 0.x deps,
  consistent with `iroh-rooms-net`). Confirm no requirement forces `dumbpipe` (the Day-9 plan
  named it as a convenience, pre-dating the in-house transport). Recommend hand-roll.
- **OQ-2 (CLI in this issue vs follow-up).** Land `pipe expose/connect/close/list` here, or land
  the net module + e2e now and split the CLI to an immediate follow-up (IR-0005 precedent)?
  Recommend: land together if schedule allows; the net+e2e is the gating deliverable.
- **OQ-3 (teardown signal).** Poll the snapshot each tick (simple, ≤1-tick latency) vs a
  push-based membership-change broadcast on `Node`. Recommend poll for the prototype; note push
  as a refinement.
- **OQ-4 (stream multiplexing).** One QUIC connection + one bidi stream per forwarded TCP conn
  (D4) vs one QUIC connection per TCP conn. Recommend D4 (stream-per-conn); revisit if stream
  limits bite.
- **OQ-5 (connector authorization of the owner).** The connector dials `owner_endpoint` from the
  signed `pipe.opened`; should the connector also verify the owner is currently Active before
  dialing (belt-and-suspenders), or rely on the owner's gate? Recommend a connector-side Active
  check for a clearer error, but the owner's gate is authoritative.
- **OQ-6 (idle/half-open timeouts).** Should idle pipe streams be reaped on a timeout? Out of
  scope for the prototype; note as a hardening follow-up.

---

## 13. Definition of done

1. `build_pipe_opened` / `build_pipe_closed` land in `iroh-rooms-core` with golden + reject
   tests; `validate_wire_bytes` round-trips them.
2. `iroh-rooms-net::pipe` registers `/iroh-rooms/pipe/1` on the shared `Router`, forwards an
   authorized member's TCP traffic to a loopback target, and rejects non-members (stage 1) and
   non-allowed members (stage 2) with the §6.6 audit reasons.
3. `tests/pipe_e2e.rs` proves P1–P5 (AC1–AC5) and the Day-9 soft GATE; every await is
   timeout-bounded; the suite is deterministic on loopback (no relay/network).
4. Teardown-on-learn tears down a live session on removal / close / expiry within ≤1 tick and
   audits it (P5/P4a).
5. CLI `pipe expose/connect/close/list` land (or are split per OQ-2) with the §13.2 security
   warning, loopback enforcement, and §16.3 failure-mode distinction; secret hygiene holds.
6. `scripts/verify.sh` is green (fmt + clippy `pedantic` + tests; `unsafe_code = forbid`); no
   new 0.x dependency added (D2).
7. Findings (loopback-proven; real-NAT pending) feed the Day-10 Phase-0 memo and inherit the
   open Gate-A risk from `iroh-rooms-net/NOTES.md`.
```
