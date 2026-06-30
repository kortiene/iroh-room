# Spec: Full-Mesh QUIC Event Transport Prototype

| | |
|---|---|
| **Issue** | #9 — [IR-0005] Prototype full-mesh QUIC event transport |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/spike, area/protocol, area/transport, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §9.1 (Room Event Plane — append-only signed event log), §16.3 (CLI must distinguish offline peer / unauthorized peer / …), §18.1 (P2P reliability risk → relay fallback, clear connection state), §13.1 (signed events, signature validation, room membership checks) · `PHASE-0-SPIKE.md` **ADR-1** (full-mesh direct QUIC over custom ALPN; reject unknown `remote_endpoint_id` at `accept()`; gossip parked), **Spike Plan Day 1 / Gate A** (real-network NAT connectivity — the headline test), **Spike Plan Day 4** (transport build for N≤5), Membership & Ordering §1 (`device_id` == iroh `EndpointId`), §5 (connect-accept admission against the membership snapshot), §8 (substrate mapping: the ALPN carries live push + pull/backfill RPC + admin-tip exchange) · Event Protocol §1 (`device_id` = `EndpointId`), §3/§6 (`WireEvent`, validation) |
| **Dependencies** | #6 — IR-0002 canonical signed event model (**landed**: `iroh-rooms-core::event` — `WireEvent`, `validate_wire_bytes`, `ValidatedEvent`, `RejectReason`). Soft/integration dependency on the **landed** `iroh-rooms-core::sync` (#11 / IR-0007): the sans-IO `SyncEngine` and the `SyncTransport` trait this adapter implements, and `SyncMessage`/`Outgoing`/`PeerId` framing this adapter carries. |
| **Status** | Implemented and landed — `crates/iroh-rooms-net` is a workspace member (issue #9 / IR-0005); all four acceptance criteria pass; **Gate A** (real-NAT run) is owed before MVP go. |
| **Type** | Spike (risk/high). First slice of `crates/iroh-rooms-net`: prove ADR-1 full-mesh direct QUIC carries a signed `WireEvent` over the custom ALPN, that admission is enforced from the iroh-authenticated `EndpointId` **before any event byte is read**, that connection state is observable (connected / offline / unauthorized), and that a dropped link reconnects. It is deliberately the I/O pump *behind* the already-landed deterministic engine; the engine and conformance path stay iroh-free. |

> **Status:** implemented and landed (issue #9 / IR-0005). All four acceptance
> criteria pass (T1–T9 in `tests/loopback.rs`; 85 tests total: 67 unit +
> 9 frame-codec integration `tests/frame.rs` + 9 loopback integration
> `tests/loopback.rs`); `scripts/verify.sh` is green. **Gate A** (real-NAT run
> on two physical machines on different NATs) was not performed during this phase;
> it remains **owed before MVP go** — see `crates/iroh-rooms-net/NOTES.md` §Gate A.
> The implementation and `NOTES.md` are the source of truth for any detail that
> diverges from this spec.

---

## 1. Summary

The Room Event Plane is a hand-rolled signed append-only event log (ADR-2, landed across #6/#8/#12/#11). Everything below the wire is done and frozen: the byte-exact `WireEvent` (#6), the SQLite store (#8), the membership fold (#12), and the deterministic, **sans-IO** `SyncEngine` + in-memory `SimNet` (#11). The one thing that is still simulated is the **wire**: the engine returns `Outgoing` frames and an in-memory harness routes them. This issue builds the **real carrier** — a full-mesh direct-QUIC adapter over iroh — and proves it on a real network.

Per **ADR-1**, the transport is full-mesh direct QUIC over the custom ALPN `/iroh-rooms/event/1`: at N≤5 every peer dials every other peer's `EndpointId`, frames are carried on per-peer bidirectional QUIC streams, and — decisively — a node **rejects an unknown remote `EndpointId` at `accept()` before any byte flows**. The gossip alternative was already decided against in ADR-1 (its open, unauthenticated topic fights the private-room model); **this spike does not re-open that comparison** — it builds and measures the chosen path.

The design seam already exists. `iroh-rooms-core::sync` defines:

```rust
pub trait SyncTransport {
    fn peers(&self) -> Vec<PeerId>;     // currently-connected authenticated device_ids
    fn send(&mut self, out: Outgoing);  // best-effort, per-peer-ordered enqueue
}
```

and the engine exposes a clean I/O boundary: `ingest_frame(from, bytes) -> Vec<Outgoing>`, `publish(bytes) -> Result<Vec<Outgoing>>`, `on_connect(peer) -> Vec<Outgoing>`, `on_disconnect(peer)`, `on_tick(now_ms) -> Vec<Outgoing>`. **This adapter is precisely the concrete `SyncTransport` + the pump that feeds those entry points.** "WireEvent push" is realized as a `SyncMessage::Events { frames: [wire_bytes] }` frame — the same length-prefixable canonical-CBOR frame the engine already speaks (`SyncMessage::encode`/`decode`), so the transport carries one opaque frame type and the validator boundary is unchanged.

What this issue delivers, mapped to the issue scope:

1. **Iroh endpoint setup** → an `iroh::Endpoint` keyed by the node's `device_id` secret (the same Ed25519 key that signs events and is the `EndpointId`), with n0 DNS discovery + relay for real-network runs and `RelayMode::Disabled` for loopback tests.
2. **Router custom ALPN `/iroh-rooms/event/1`** → `Router::builder(endpoint).accept(EVENT_ALPN, handler).spawn()` with a `ProtocolHandler` that gates admission.
3. **Per-peer bidirectional stream framing for `WireEvent` push** → a per-peer connection manager holding one logical bidi stream per peer and a length-prefixed frame codec carrying `SyncMessage` frames (live push = `Events`).
4. **Reject unknown `EndpointId` at accept time** → the `ProtocolHandler` resolves `Connection::remote_id()` (the QUIC/TLS-authenticated `device_id`) against an `Admission` authorizer and closes the connection **before** calling `accept_bi()` if the device is not a bound Active member; a local audit record is written.

The honest scope boundary (so this stays a Phase-0 spike, not the whole CLI): the adapter exposes an **async API and a small `net-smoke` binary**; full wiring into the `iroh-rooms` CLI binary (a runtime, room lifecycle, `room tail` output) is **out of scope** here and tracked separately. The adapter's authorizer is a `device_id → identity → Active?` allowlist with the **same shape** the membership fold (`MembershipSnapshot`) produces, so swapping the fixture for the real fold is a re-point, not a reshape (the reusable-shape pattern proven by `crates/spike-blobs`).

---

## 2. Background & current repository state

Read before implementing.

### 2.1 Source-of-truth docs (normative)

- **`PHASE-0-SPIKE.md` ADR-1** — *Adopt full-mesh direct QUIC over custom ALPN; reject gossip for the load-bearing log.* Key consequences this spec implements verbatim:
  - "Build a per-room peer manager that, on join, dials each member `EndpointId`, maintains one bidi stream per peer, frames events with a length prefix, and surfaces connect/disconnect for the CLI's connection-state output."
  - "Register the `/iroh-rooms/event/1` ALPN on the shared `Router` alongside the blob and pipe ALPNs (one `Endpoint`, multiple `.accept()` chains)."
  - "Reject unknown `remote_endpoint_id` at `accept`, log the rejection locally." The gate resolves `device_id → bound sender_id → Active?` via the validated device binding.
  - "Dial purely by `EndpointId` using default n0 DNS discovery + mDNS for LAN; the invite ticket carries the addressing hints. DHT discovery is not required."
  - The ALPN carries *live event push*, a *pull/backfill RPC*, and *admin-tip exchange* — i.e. the full `SyncMessage` set, not a bespoke event-only frame.
- **Spike Plan Day 1 / GATE A** (the headline real-network test) — two physical machines on *different real NATs* establish a usable iroh connection; measure path type (direct hole-punched vs relay fallback), time-to-first-byte, RTT, throughput, both dial directions. **GO** iff a path is established both directions within ≤10 s in every scenario via at least relay fallback, with a direct path in ≥1 non-symmetric scenario, and relay throughput usable for chat. This spec inherits Gate A as its "at least one real-network run if available" test-plan item.
- **Spike Plan Day 4** — build the transport for an N≤5 room carrying signed `WireEvent`s; observe propagation latency N=2..5, reconnect behavior, and confirm the transport gives neither ordering nor auth for the log (the log layer owns both). The gossip backend is *not* built here (ADR-1 already decided; building it would re-open a closed decision).
- **Membership & Ordering §1** — `device_id` is byte-for-byte the iroh `EndpointId`; transport identity, event-signing identity, and ACL identity are one key. Authorization/membership is tracked against the **identity** key (`sender_id`); the connection proves the **device** key; the gate resolves device → identity → Active.
- **Membership & Ordering §5** — connect-accept admission: proven `EndpointId` → bound identity → evaluate against the **current local membership snapshot**, default deny; fail closed when the §0 incompleteness detector trips.
- **Event Protocol §1/§3/§6** — `device_id == EndpointId`; the `WireEvent` envelope (`v`/`signed`/`sig`/`id`); receiver re-validates the exact `signed` bytes (never re-encodes). The transport carries `WireEvent` bytes **opaquely** and re-validation stays the consumer's job (engine calls `validate_wire_bytes`).

### 2.2 Landed code this adapter builds on

- **`iroh-rooms-core::event` (no feature):** `WireEvent` (`to_bytes`/`from_bytes`), `validate_wire_bytes`, `ValidatedEvent`, `RejectReason`. The adapter never re-validates; it hands raw inbound frame bytes to the engine, which validates.
- **`iroh-rooms-core::sync` (feature `sync`, landed #11):**
  - `SyncTransport` trait (`crates/iroh-rooms-core/src/sync/transport.rs`) — the seam this adapter implements. Its doc-comment already names "the real full-mesh iroh QUIC adapter (`crates/iroh-rooms-net`, D3/D9)" as the implementor and states the contract: best-effort, per-peer-ordered, "may be dropped if the link is down (the engine re-pulls on reconnect)."
  - `SyncMessage` (+ `encode`/`decode`, canonical CBOR, strict shape), `Outgoing { peer, msg }`, `PeerId([u8;32])` (== `device_id`), `Window`, `WireBytes`.
  - `SyncEngine` entry points: `ingest_frame(from: PeerId, bytes: &[u8]) -> Vec<Outgoing>`, `publish(bytes) -> Result<Vec<Outgoing>, SyncError>`, `on_connect(peer) -> Vec<Outgoing>`, `on_disconnect(peer)`, `on_tick(now_ms) -> Vec<Outgoing>`, `snapshot() -> MembershipSnapshot`, `completeness()`, `fail_closed_subjects()`.
- **`iroh-rooms-core::membership` (#12):** `MembershipSnapshot` provides the `device_id → identity` reverse map and the Active set — the production source for the `Admission` authorizer (§5 connect-accept). For the prototype, an in-memory fixture of the same shape is used.

### 2.3 Proven iroh patterns already in the repo

`crates/spike-blobs` (IR-0009, landed) already stands up the iroh 1.0 stack and is the **reference for endpoint/router setup, the authenticated-identity gate, and the loopback test rig**:

- Pinned `iroh = "=1.0.1"` (resolved against the crates.io index 2026-06-29; `iroh-base 1.0.1` transitively). Async via `tokio = { version = "1", features = ["rt-multi-thread","macros","time","sync"] }`. `tracing` + `tracing-subscriber` for observability.
- `Endpoint::builder(presets::Minimal).secret_key(secret).relay_mode(RelayMode::Disabled).bind().await` for a loopback-only endpoint; `Router::builder(endpoint).accept(ALPN, handler).spawn()`.
- The authenticated remote identity is `connection.remote_id()` surfaced as `EndpointId` (the spike-doc name `remote_node_id` was a recon alias — confirmed in `spike-blobs/NOTES.md` §2). Admission denies by closing / `AbortReason::Permission` against a `device_id → identity → Active` `AuthContext` fixture — the exact shape this adapter reuses for connect-accept.
- It deliberately does **not** inherit the workspace `rust-version` (the iroh 1.0 stack has a higher MSRV than the shipping crates declare) and sets `publish = false`.

**Divergence to note:** `spike-blobs` is a *throwaway* crate forbidden from the shipping tree. `iroh-rooms-net` is the **real, shipping** adapter (the README names it as the remaining target), so it is a workspace member the CLI will later depend on — see D1.

### 2.4 What does NOT exist yet

- No `crates/iroh-rooms-net`. No `iroh`/`tokio` dependency in any **shipping** crate (`core` is sans-IO, `cli` is synchronous). This issue introduces the first async runtime onto the shipping path — an explicit, flagged consequence (D1, D2, OQ-1).

---

## 3. Goals and non-goals

### 3.1 Goals (this issue)

- **G1.** Stand up an `iroh::Endpoint` + `Router` with ALPN `/iroh-rooms/event/1` and a `ProtocolHandler` that gates admission on the authenticated `EndpointId`.
- **G2.** A per-peer connection manager: dial members by `EndpointId`, hold one logical bidi stream per peer, length-prefix-frame `SyncMessage`s, and push a signed `WireEvent` (as `SyncMessage::Events`) to a connected peer.
- **G3.** Reject an unknown/unauthorized remote `EndpointId` **before any event byte is read or surfaced**, with a local audit record.
- **G4.** An observable per-peer connection-state model distinguishing **connected**, **offline**, and **unauthorized** (PRD §16.3), exposed as a queryable snapshot + a change event stream.
- **G5.** Basic reconnect: a dropped link redials with bounded backoff and re-establishes; transitions are observed and documented.
- **G6.** Implement `iroh_rooms_core::sync::SyncTransport` so the landed `SyncEngine` can drive the adapter unchanged (integration target).
- **G7.** A `net-smoke` binary + a documented manual procedure for the Gate-A real-network run; a `NOTES.md` findings deliverable.

### 3.2 Non-goals (explicitly deferred)

- **N1.** Wiring the adapter into the `iroh-rooms` CLI binary (room create/join/tail subcommands, a long-lived runtime). Tracked separately; this issue ships an async library + smoke binary only.
- **N2.** The gossip backend / ADR-1 re-comparison (decided). Admin-tip advertisement *over gossip* (off the critical path) is out of scope; admin-tip exchange *over the mesh ALPN* is already handled by the landed engine and merely transported here.
- **N3.** Bootstrap/discovery via real invite tickets (the ticket format, `room join <ticket>`); the prototype dials by raw `EndpointId` + `EndpointAddr` hints supplied directly.
- **N4.** mDNS LAN discovery (ADR-1 mentions it; not required for two-peer dial-by-id). Record as a follow-up.
- **N5.** Re-validating events inside the transport, or re-implementing ordering/membership — the engine owns all of that. The transport carries opaque frames.
- **N6.** Production hardening of the runtime (graceful shutdown beyond `Drop`, metrics export, connection pooling beyond one-per-peer, multi-device per identity).

---

## 4. Design

### 4.1 Crate & dependency layout (D1, D2)

New workspace member `crates/iroh-rooms-net`:

```toml
[package]
name = "iroh-rooms-net"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
# Does NOT inherit `rust-version`: the iroh 1.0 stack's MSRV exceeds the
# workspace-declared 1.80 (the shipping crypto deps already require >= 1.85).
# Set an explicit MSRV matching iroh 1.0.1, or omit to make no claim. See OQ-1.

[lints]
workspace = true            # inherits `unsafe_code = "forbid"` (our code only;
                            # iroh's internal unsafe lives in dependencies).

[dependencies]
iroh-rooms-core = { path = "../iroh-rooms-core", features = ["sync"] }
iroh = "=1.0.1"             # same pin proven by spike-blobs (index 2026-06-29)
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }
anyhow = "1"               # binary/edge error context only
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

[[bin]]
name = "net-smoke"
path = "src/bin/net_smoke.rs"

[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync", "test-util"] }
iroh-rooms-core = { path = "../iroh-rooms-core", features = ["sync", "store"] }
```

Add `"crates/iroh-rooms-net"` to the root `Cargo.toml` `[workspace].members`.

Rationale for "real crate, not throwaway": the seam (`SyncTransport`) and the README both already designate `crates/iroh-rooms-net` as the adapter; ADR-1 is decided, so this is not a comparison spike like `spike-blobs`. It is the first prototype *slice* of a shipping crate, gated by Gate A. (Alternative considered: a throwaway `spike-transport` — rejected, see D1.)

### 4.2 Module layout

```text
crates/iroh-rooms-net/
  src/
    lib.rs            // crate docs, re-exports (NetTransport, PeerConnState, Admission, ...)
    alpn.rs           // pub const EVENT_ALPN: &[u8] = b"/iroh-rooms/event/1";
    frame.rs          // length-prefixed framing codec (read_frame / write_frame)
    admission.rs      // Admission trait + AllowlistAdmission fixture (fold-shaped)
    state.rs          // PeerConnState, ConnEvent, the shared PeerTable
    handler.rs        // EventProtocolHandler: ProtocolHandler::accept + the gate
    transport.rs      // NetTransport: Endpoint+Router owner; impl SyncTransport
    peer.rs           // per-peer reader/writer tasks + dial-with-backoff loop
    audit.rs          // local audit sink (peer.accepted / peer.rejected / ...)
  src/bin/net_smoke.rs // two-mode CLI: --listen / --dial <addr>; Gate-A harness
  tests/
    loopback.rs       // in-process two-endpoint integration tests (CI, deterministic-ish)
  NOTES.md            // findings deliverable (AC mirror; Gate-A results table)
```

### 4.3 Wire framing (`frame.rs`)

Each bidi stream carries a sequence of length-prefixed frames. A frame body is a canonical-CBOR `SyncMessage` (`SyncMessage::encode()`), so the transport speaks exactly one frame type and reuses the landed, strict, byte-deterministic codec.

- **Prefix:** 4-byte big-endian `u32` body length, then the body bytes. Reject (close the stream, mark the link errored) if the declared length exceeds `MAX_FRAME_BYTES` (a guard against a peer claiming a huge frame). Choose `MAX_FRAME_BYTES` to comfortably exceed one `WireEvent` plus a bounded `Events` batch (e.g. 1 MiB for the prototype; document and revisit when batch sizing is tuned).
- **Read:** `read_frame(stream) -> Option<Vec<u8>>` reads the prefix, then exactly that many bytes; `None` on clean EOF. The bytes are handed **verbatim** to `SyncEngine::ingest_frame` — the transport does **not** decode/validate them (defense in depth: a malformed frame is the engine's logged drop, not a transport crash).
- **Write:** `write_frame(stream, &bytes)` writes prefix + body and flushes.

> Note: live "WireEvent push" is a `SyncMessage::Events { room_id, frames: vec![wire_bytes] }`. The transport neither knows nor cares that a frame is "a push" vs "a backfill response" — it is opaque `SyncMessage` bytes end-to-end. This keeps AC1 ("exchange a signed WireEvent") and the broader sync RPC on one carrier.

### 4.4 Admission (`admission.rs`) — the authenticated-identity gate (G3, Security)

```rust
/// The decision the accept-gate makes from a proven remote EndpointId.
pub enum AdmissionDecision {
    Admit { identity: IdentityKey },  // device bound to an Active member
    Reject(RejectCause),              // unknown device, or bound-but-not-Active
}

pub enum RejectCause { UnknownDevice, NotActive, FailClosed }

/// Resolve a QUIC/TLS-authenticated remote `device_id` (== EndpointId) to an
/// admission decision. Production impl reads a `MembershipSnapshot`
/// (device->identity reverse map + Active set + §0 fail-closed). The prototype
/// uses `AllowlistAdmission`, the same shape (cf. spike-blobs `AuthContext`).
pub trait Admission: Send + Sync + 'static {
    fn authorize(&self, device: EndpointId) -> AdmissionDecision;
}
```

- `AllowlistAdmission` (prototype): `device_to_identity: HashMap<EndpointId, IdentityKey>` + `active: HashSet<IdentityKey>`. **Fail-closed**: an unbound device, or a device bound to a non-Active identity, is rejected. Identical decision logic to the landed blob gate, so the production re-point is `MembershipSnapshot` → these two lookups (+ `completeness()`/`fail_closed_subjects()` for the §5 fail-closed overlay).
- **The gate runs on the proven identity only.** `EndpointId` is cryptographically authenticated by iroh's QUIC/TLS client-auth (`connection.remote_id()`); no application bytes are read to decide admission. This is the ADR-1 "admission is a property of the transport, not a bolt-on."

### 4.5 Connection state (`state.rs`) — observability (G4, PRD §16.3)

```rust
/// Per-peer connection state surfaced to the CLI/app (PRD §16.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerConnState {
    Connecting,    // dial in progress (transient; not in the AC trio but useful)
    Connected,     // authenticated Active member, live bidi stream up
    Offline,       // a member we expect, but no live connection (dial failing / unreachable)
    Unauthorized,  // a device that presented itself but is not a bound Active member (rejected)
}
```

- The adapter holds a shared `PeerTable` (`Arc<Mutex<HashMap<EndpointId, PeerEntry>>>` or `arc-swap` snapshot) mapping each known device to `{ state, identity: Option<IdentityKey>, last_change_ms }`.
- **`Unauthorized`** is recorded for inbound rejections (someone dialed us and failed the gate) and for outbound dials that complete the handshake to a device our `Admission` cannot authorize. It is sticky for observability until the roster changes; it never holds a live stream.
- **`Offline` vs `Unauthorized`** is the exact PRD §16.3 distinction: offline = authorized member, no path right now; unauthorized = a peer we will not talk to regardless of reachability.
- A `tokio::sync::broadcast<ConnEvent>` (`ConnEvent { device, from: PeerConnState, to: PeerConnState, ts_ms }`) lets a consumer (CLI `room members`, audit) observe transitions live; `NetTransport::peer_states() -> Vec<(EndpointId, PeerConnState)>` gives a point-in-time snapshot.
- Mapping to AC3: the loopback tests assert all three of `Connected` / `Offline` / `Unauthorized` are reached and observable.

### 4.6 The accept-gate handler (`handler.rs`) — G1, G3

`EventProtocolHandler` implements `iroh::protocol::ProtocolHandler`:

```text
async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
    let device = conn.remote_id();                  // proven EndpointId (device_id)
    match self.admission.authorize(device) {
        Reject(cause) => {
            self.audit.rejected(device, cause);     // local audit (PRD §13.2 / §16.3)
            self.state.set(device, Unauthorized);
            conn.close(REJECT_CODE, b"unauthorized");// NO accept_bi(): zero event bytes read
            return Ok(());                           // not an error from the router's POV
        }
        Admit { identity } => {
            self.state.set(device, Connected);
            self.audit.accepted(device, identity);
            self.register_inbound(conn, device).await // spawn reader/writer; engine.on_connect
        }
    }
}
```

- **AC2 is satisfied structurally:** admission is decided from `remote_id()` and the connection is closed on reject **before** `accept_bi()` is ever called — there is no code path where an unauthorized peer's frame reaches `read_frame`/the engine. A test asserts the inbound sink stays empty for a rejected dialer.
- The reject close code/reason is a stable application close (`REJECT_CODE`, `"unauthorized"`) so the *dialing* side can surface `Unauthorized` rather than a generic drop (feeds the dialer's §16.3 state).

### 4.7 Per-peer manager & reconnect (`peer.rs`, `transport.rs`) — G2, G5, G6

- **Mesh dial:** `NetTransport::connect_to(addr: EndpointAddr)` (and a `connect_all(roster)` helper) starts, per peer, a **dial loop** task: `endpoint.connect(addr, EVENT_ALPN)`; on success authorize the remote `remote_id()` (a dial can reach a non-member → `Unauthorized` + close), then `open_bi()`, register the peer `Connected`, call `engine.on_connect(peer)` and route the returned `Outgoing`s, and spawn the reader.
- **Full-mesh double-connect:** both peers may dial each other → up to two connections per pair. Deterministic tie-break: **the connection initiated by the numerically-lower `EndpointId` wins**; the loser is closed with a stable code. (Prototype-acceptable simplification; documented.)
- **Reader task (per peer):** loop `read_frame`; for each frame `engine.ingest_frame(peer, &bytes)` and route returned `Outgoing`s; surface the decoded inbound message to the consumer sink. On stream error/EOF: mark the peer `Offline`, `engine.on_disconnect(peer)`, and signal the dial loop to redial.
- **Writer task (per peer):** drains an `mpsc` of outbound frame bytes for that peer and `write_frame`s them. `SyncTransport::send` pushes here.
- **Reconnect:** the dial loop retries with **exponential backoff + jitter**, capped (e.g. 250 ms → 8 s, jittered), and never gives up while the peer is in the roster. iroh handles hole-punching/relay re-establishment beneath; the adapter only re-opens the bidi stream and replays `on_connect` (which re-pulls — the engine is built for "best-effort, re-pull on reconnect"). A loopback test forces a disconnect and asserts re-establishment + the `Offline → Connecting → Connected` transitions (AC4).

**`SyncTransport` impl (G6) — bridging async I/O to the sync trait:** the trait is synchronous and best-effort, which fits a non-blocking enqueue:

```rust
impl SyncTransport for NetTransport {
    fn peers(&self) -> Vec<PeerId> {
        // snapshot of the PeerTable: device_ids whose state == Connected
    }
    fn send(&mut self, out: Outgoing) {
        // non-blocking: look up the peer's outbound mpsc Sender and try_send the
        // encoded frame. If the peer is Offline (no live writer), DROP — the
        // engine re-pulls on reconnect (trait contract). Never blocks/awaits.
    }
}
```

The engine loop the consumer runs (in the smoke binary and tests): take the `Vec<Outgoing>` returned by any engine entry point and call `transport.send(out)` for each; feed inbound frames via `engine.ingest_frame`; tick periodically via `engine.on_tick(now_ms)`.

### 4.8 Endpoint setup (`transport.rs`) — G1

- `NetTransport::bind(secret: SecretKey, admission, cfg) -> Result<Self>`:
  - `Endpoint::builder(preset).secret_key(secret)` where `secret` is the node's `device_id` key (so `endpoint.id() == device_id == EndpointId`, unifying transport/signing/ACL identity, Membership §1).
  - **Loopback/test mode:** `presets::Minimal` + `RelayMode::Disabled` (offline over loopback, à la spike-blobs).
  - **Real-network mode:** the default preset with n0 DNS discovery + relay (`dns.iroh.link`) so dial-by-`EndpointId` works across NATs (Gate A).
  - `Router::builder(endpoint).accept(EVENT_ALPN, EventProtocolHandler::new(...)).spawn()`. (Designed so the future blob/pipe ALPNs chain `.accept()` on the same `Endpoint`/`Router` — ADR-1 "one `Endpoint`, multiple `.accept()` chains.")
- `NetTransport::endpoint_addr() -> EndpointAddr` exposes this node's dialable address (for the smoke binary / out-of-band exchange in lieu of a ticket, N3).

---

## 5. Implementation steps

Detailed enough to execute without re-deriving the design.

1. **Scaffold the crate.** Create `crates/iroh-rooms-net` with the `Cargo.toml` of §4.1; add it to the workspace members. Confirm `cargo build -p iroh-rooms-net` resolves `iroh = 1.0.1` (matching `spike-blobs`' lock) and `cargo metadata` shows `tokio` only on the net crate, not on `core`/`cli`. Decide the MSRV stance (OQ-1) and record it in a Cargo comment mirroring `spike-blobs`.
2. **`alpn.rs`.** `pub const EVENT_ALPN: &[u8] = b"/iroh-rooms/event/1";` with a doc-link to ADR-1. Add a test asserting the exact byte string (it is a wire contract — a typo is a silent interop break).
3. **`frame.rs`.** Implement `write_frame`/`read_frame` over `iroh::endpoint::{SendStream, RecvStream}` with the 4-byte BE length prefix and `MAX_FRAME_BYTES` guard. Unit tests: round-trip arbitrary bodies; oversized-prefix rejected; truncated stream → clean `None`/error (no panic).
4. **`admission.rs`.** Define `Admission`, `AdmissionDecision`, `RejectCause`, and `AllowlistAdmission`. Port the `device_id → identity → Active` logic and fail-closed defaults from `spike-blobs::acl::AuthContext`. Unit tests: unbound device rejected; bound-but-inactive rejected; bound+Active admitted.
5. **`state.rs`.** Define `PeerConnState`, `PeerEntry`, `ConnEvent`, the shared `PeerTable` with `set(device, state)` (emits a `ConnEvent` on change) and `snapshot()`. Unit tests: transition emits exactly one event; idempotent set is a no-op.
6. **`audit.rs`.** A minimal audit sink (`accepted`/`rejected`/`connected`/`disconnected`) writing structured `tracing` events with stable reason strings (`peer.accepted`, `peer.rejected:unknown_device`, …) for the local audit log (PRD §13.2 / §16.3). Keep it a trait so the CLI can later persist it.
7. **`handler.rs`.** Implement `EventProtocolHandler` + `ProtocolHandler::accept` exactly as §4.6: authorize `conn.remote_id()`, reject-before-`accept_bi`, register inbound on admit. Wire `on_connect` and the reader/writer spawn through `peer.rs`.
8. **`peer.rs`.** Implement the per-peer reader task (frame → `engine.ingest_frame` → route `Outgoing`; surface inbound to the consumer sink; error → `Offline` + `on_disconnect` + redial signal), writer task (drain outbound `mpsc` → `write_frame`), and the dial-with-backoff loop with the lower-`EndpointId`-wins tie-break.
9. **`transport.rs`.** Implement `NetTransport::bind`/`connect_to`/`connect_all`/`endpoint_addr`/`peer_states`/`conn_events()` and `impl SyncTransport` (non-blocking `send`, `Connected`-only `peers`). Hold the `Endpoint`, `Router`, `PeerTable`, per-peer outbound senders, and the inbound sink.
10. **`net_smoke.rs` binary.** Two modes: `--listen` prints this node's `EndpointAddr` (id + relay/direct hints) and waits; `--dial <addr-or-id>` dials, runs a tiny engine loop, `publish`es one signed `WireEvent`, and prints received frames + the connection-state timeline. A `--reject-demo` flag dials with a non-member secret to show the reject path. Used for both the loopback demo and the Gate-A two-machine run.
11. **`tests/loopback.rs`.** The CI integration suite (§7.2): two in-process endpoints over `RelayMode::Disabled`, `#[tokio::test]`. Covers AC1–AC4. Use a `tokio::time::timeout` around every await so a wiring bug fails fast instead of hanging CI.
12. **`NOTES.md`.** Write the findings deliverable: the AC checklist mirrored to evidence, the pinned versions (re-confirmed against the index), the connection-state model, the reconnect behavior observed, and the **Gate-A results table** (scenario × direction × path-type × RTT × throughput × setup-time) — or an explicit "real-network run not performed; loopback only" if no two-NAT rig is available (the test plan says *if available*).
13. **Verify.** `scripts/verify.sh` (fmt + clippy `-D warnings` `--all-features` + tests `--workspace`). Ensure the loopback tests are deterministic enough for CI (bounded timeouts, no real network, no relay); mark any genuinely network-dependent test `#[ignore]` with a doc comment on how to run it (OQ-2).

---

## 6. Security & privacy

- **Authenticated admission (the issue's security note).** The gate authorizes on `connection.remote_id()` — the QUIC/TLS-proven `EndpointId` (`device_id`), never on any self-asserted application field. ADR-1's whole point: admission is a transport property. **Reject happens before `accept_bi()`**, so an unauthorized peer's bytes are never read, never reach the engine, never touch the store (AC2).
- **Default deny / fail-closed.** `AllowlistAdmission` (and the production `MembershipSnapshot` re-point) deny any device that is unbound or bound to a non-Active identity. When the engine's `completeness()` is suspect or a subject is in `fail_closed_subjects()` (§0/§5), the production authorizer denies the affected subjects — carried as `RejectCause::FailClosed`. (Prototype may stub this overlay; it must be a TODO seam, not silently absent — flag in NOTES.)
- **No event egress to non-members.** Outbound frames are only ever written to `Connected` peers (authorized at accept/dial). `SyncTransport::send` to an `Offline`/`Unauthorized` peer is dropped, not buffered to a non-member.
- **Confidentiality is end-to-end regardless of path.** Both direct and relay-fallback paths are QUIC/TLS between authenticated endpoints; relays forward only ciphertext (PHASE-0-SPIKE §"Confidentiality mechanism"). The Gate-A notes should record path type but must not imply relay = plaintext.
- **Audit.** Every accept/reject/connect/disconnect is logged locally with a stable reason (PRD §13.2 pipe-style audit, §16.3 failure-mode distinction). Reject reasons are the §16.3 vocabulary (offline vs unauthorized).
- **The transport is not a trust boundary for content.** It carries opaque `WireEvent` bytes; signature/membership/ordering validation stays in the engine (`validate_wire_bytes` + fold). The transport must never be tempted to "trust" a frame because it came from a `Connected` peer — a connected member can still send a malformed or invalid event, which the engine drops. (Defense-in-depth: framing length guard + opaque pass-through.)

---

## 7. Test strategy

### 7.1 Unit tests (deterministic, no network)

- `frame.rs`: round-trip; oversized-length rejected; truncated read is a clean error.
- `admission.rs`: unbound/inactive rejected, active admitted; fail-closed default.
- `state.rs`: transition emits one `ConnEvent`; snapshot correctness.
- `alpn.rs`: exact ALPN byte string (wire contract).

### 7.2 Loopback integration tests (`tests/loopback.rs`, CI) — the AC oracle

Two in-process `NetTransport`s on `RelayMode::Disabled` over loopback, each with an `AllowlistAdmission` fixture, both driving a real `SyncEngine` over an in-memory/temp store (`store` feature in dev-deps). Every await wrapped in a bounded `tokio::time::timeout`.

- **T1 (AC1) — exchange a signed `WireEvent`.** A and B authorize each other; A `publish`es a valid `WireEvent` (built via the event-core signing path, or the #6 golden-vector fixture); the routed `Events` frame arrives at B; B's engine `ingest_frame` accepts it; assert B's store now `contains` the event id and the bytes round-trip byte-for-byte. Also assert a tampered byte → engine rejects (transport delivered it; engine is the gate).
- **T2 (AC2) — unknown endpoint rejected before bytes.** C binds a fresh secret **not** in B's allowlist and dials B. Assert: B's inbound sink receives **zero** frames from C; B emitted `peer.rejected:unknown_device`; C observes `Unauthorized` (its dial loop sees the stable reject close); B's store is unchanged. A counter/hook asserts `accept_bi` was never reached for C.
- **T3 (AC3) — connection-state trichotomy.** Drive A↔B to `Connected`; stop B's endpoint → A observes `Offline`; have C (non-member) dial A → A records `Unauthorized` for C. Assert `peer_states()` shows all three and the `ConnEvent` stream emitted each transition.
- **T4 (AC4) — basic reconnect.** With A↔B `Connected`, force-drop the connection (close B's side / drop the link); assert A transitions `Connected → Offline → Connecting → Connected` within the backoff window and a subsequently-`publish`ed event still reaches B (proving the stream was genuinely re-established, not just the state flag flipped).

### 7.3 Real-network run (Gate A; "if available")

The `net-smoke` binary on two physical machines on **different real NATs** (not the same LAN/VPN), default preset (n0 DNS + relay). Run ≥2 NAT scenarios incl. ≥1 likely-symmetric (CGNAT/mobile), both dial directions. Record per the Gate-A table: establishment success + time-to-first-byte, **path type achieved** (direct hole-punched vs relay — read off the path watcher, do not assume), RTT, throughput; confirm a signed `WireEvent` round-trips. **GO/NO-GO** per Gate A. If no two-NAT rig is available during Phase 0, document loopback-only and carry the real-network confirmation as a residual (the test plan permits "if available"; Gate A remains owed before MVP go).

### 7.4 What is intentionally not tested here

Ordering/convergence/anti-amplification (owned + already tested by the landed engine + `SimNet`, Gate D); blob/pipe ACLs (separate planes). This spec tests only the **carrier** and its **admission gate**.

---

## 8. Acceptance criteria → evidence

| # | Issue acceptance criterion | Evidence |
|---|---|---|
| 1 | Two peers exchange a signed `WireEvent` over the custom ALPN | Test **T1**; `net-smoke` two-machine run (§7.3). ALPN asserted in `alpn.rs` test. |
| 2 | Unknown remote endpoint rejected **before** event bytes are accepted | Test **T2**: reject decided from `remote_id()`, connection closed before `accept_bi()`; inbound sink empty; `accept_bi` never reached for the rejected dialer; audit `peer.rejected`. |
| 3 | Connection state distinguishes connected / offline / unauthorized | `PeerConnState` (§4.5); test **T3**; `peer_states()` + `ConnEvent` stream. |
| 4 | Basic reconnect behavior observed and documented | Test **T4** (state transitions + post-reconnect delivery); `NOTES.md` reconnect section. |
| — | Security: admission via iroh-authenticated remote endpoint identity | §6; the gate uses `connection.remote_id()` only, reject-before-bytes (T2). |
| — | Test plan: two local peers + ≥1 real-network run if available | §7.2 (loopback CI) + §7.3 (Gate-A manual). |

---

## 9. Key decisions

- **D1 — Build the real `crates/iroh-rooms-net`, not a throwaway `spike-transport`.** ADR-1 is decided and the README + `SyncTransport` doc already name this crate as the adapter; this is the first prototype slice of a shipping crate, not a comparison spike. *Trade:* it puts iroh/tokio on the shipping tree now (vs. deferring). *Alternative:* a throwaway `spike-transport` like `spike-blobs` — rejected because there is no second backend to compare and the seam is already production-shaped. (Reversible: the crate can stay `publish`-gated until the CLI wires it.)
- **D2 — iroh/tokio land on the net crate only; `core` stays sans-IO.** The deterministic conformance path (engine + `SimNet`) remains iroh-free (the explicit ADR-1/§11 invariant — "a flaky network never makes Gate D non-deterministic"). `core` and `cli` gain no async dep from this issue.
- **D3 — The transport carries `SyncMessage` frames; "WireEvent push" = `SyncMessage::Events`.** Reuses the landed strict canonical-CBOR codec and makes the adapter a drop-in `SyncTransport`, so the same wire serves live push + backfill + admin-tip (ADR-1 §8). *Alternative:* a bespoke event-only frame — rejected (would fork the codec and re-do framing when sync is wired).
- **D4 — Length-prefixed frames (4-byte BE) with a `MAX_FRAME_BYTES` guard** over per-peer bidi streams. Matches ADR-1 "frames events with a length prefix"; the guard denies a peer claiming an enormous frame.
- **D5 — Pin `iroh = "=1.0.1"`, async via `tokio`**, mirroring the index-confirmed `spike-blobs` pins; only iroh 1.0 **stable core** (`Endpoint`/`Router`/`ProtocolHandler`/`Connection`/`EndpointId`) is used — zero 0.x crates on the event critical path (ADR-1 churn argument).
- **D6 — `Admission` trait with a fold-shaped `AllowlistAdmission` fixture now, `MembershipSnapshot` later.** Same reusable-shape seam as `spike-blobs::AuthContext`; production swap is a re-point of two lookups, not a reshape.
- **D7 — Connection-state model is `Connecting/Connected/Offline/Unauthorized`**, exactly covering the PRD §16.3 offline-vs-unauthorized distinction plus a transient dial state, exposed as snapshot + change-event stream.
- **D8 — Full-mesh double-connect tie-break: lower `EndpointId` wins.** Deterministic, prototype-acceptable; documented (a more robust scheme is post-prototype).
- **D9 — Loopback (relay-disabled, in-process) is the CI test path; the real-NAT run is manual/`#[ignore]`.** Keeps CI deterministic; Gate A is run on a two-machine rig and recorded in `NOTES.md`.

---

## 10. Assumptions

- **A1.** `iroh 1.0.1` `Connection::remote_id()` returns the authenticated remote `EndpointId` at/after `ProtocolHandler::accept` without reading application bytes — as used by `spike-blobs` (`connection.remote_id()` → `ClientConnected.endpoint_id`). Verify exact API/timing on the pinned version during step 7.
- **A2.** `device_id` (event signing key) and the iroh `Endpoint` secret are the **same** Ed25519 key (Event Protocol §1), so `endpoint.id() == device_id == EndpointId`. The adapter is constructed from that one secret.
- **A3.** The landed `SyncEngine`/`SyncTransport`/`SyncMessage` API (`ingest_frame`, `on_connect`/`on_disconnect`, `Outgoing`, `PeerId == device_id`) is stable and sufficient to drive from an async pump; the adapter needs no engine changes. (If a minor engine hook is missing — e.g. an inbound-message observer — add it to `core` behind the `sync` feature; flag in OQ.)
- **A4.** Loopback over `RelayMode::Disabled` connects two in-process endpoints deterministically enough for CI (bounded by timeouts), as `spike-blobs` demonstrates for the blobs ALPN.
- **A5.** The effective CI toolchain is already ≥1.85 (the landed `ed25519-dalek 3.0.0-rc.0` requires it, per the CLI Cargo comment), so adopting the iroh 1.0 MSRV in this crate does not raise the real build floor — only the declared one (OQ-1).
- **A6.** N≤5 full mesh (≤10 links) — O(n²) is trivial here; no connection pooling/sharding needed (ADR-1 accepted scaling ceiling).

---

## 11. Open questions

- **OQ-1 (MSRV / workspace).** Should `iroh-rooms-net` set an explicit `rust-version` matching iroh 1.0's MSRV, or should the workspace `rust-version` be bumped from the now-inaccurate `1.80` to reflect reality (crypto deps already force ≥1.85)? Recommendation: set it per-crate now (like `spike-blobs`), open a follow-up to reconcile the workspace value. *Owner: maintainer.*
- **OQ-2 (CI vs real network).** Confirm the loopback tests are stable in CI under `RelayMode::Disabled`; decide the gating mechanism for the real-network test (`#[ignore]` + a documented env/flag, or a separate non-CI job). The `--all-features` CI gate must not require a network.
- **OQ-3 (engine driver location).** Where does the "take `Outgoing`s → `send`; tick; ingest" driver loop live — in `iroh-rooms-net` (a `run()` task that owns an engine), or left to the future CLI? Recommendation: a thin `run()` in the net crate for the prototype/smoke binary, with the CLI free to drive manually later. Does the engine need an inbound-message callback so the app sees decoded messages (vs. only the store)? (Ties to A3.)
- **OQ-4 (double-connect).** Is the lower-`EndpointId`-wins tie-break (D8) adequate, or should the prototype simply allow two half-open connections and let one idle out? Pick the simplest that doesn't drop frames; document.
- **OQ-5 (discovery for Gate A).** For the two-machine run, exchange `EndpointAddr` out-of-band (paste id + relay hint), or stand up minimal mDNS? Recommendation: out-of-band paste for the prototype (N3/N4); real tickets are a later issue.
- **OQ-6 (fail-closed overlay).** Should the prototype already wire the §0/§5 fail-closed overlay (`completeness()`/`fail_closed_subjects()`) into `Admission`, or stub it with a TODO seam? Recommendation: stub now, but make the seam explicit and tested so the MVP wiring is a fill-in, not a redesign.

---

## 12. Risks

- **R1 (Gate A — load-bearing).** Hole-punching may fail on a symmetric-NAT pair; relay fallback may be the only path. *Mitigation:* the spec requires recording path type honestly and treats relay-usable as Gate-A GO; a no-path scenario is a documented escalation (self-hosted relay decision), not silently green. A green loopback run must **not** be reported as Gate A.
- **R2 (async on the shipping tree).** First tokio/iroh dependency in a shipping crate widens the build/MSRV/dep surface. *Mitigation:* confined to `iroh-rooms-net`; `core`/`cli` untouched; pins mirror the already-vetted `spike-blobs` lock (D2/D5).
- **R3 (admission-before-bytes correctness).** A wiring slip that reads a frame before authorizing would defeat AC2/the security note. *Mitigation:* structural guarantee (reject closes before `accept_bi`) + an explicit T2 assertion that `accept_bi` is never reached for a rejected dialer.
- **R4 (test flakiness).** Network/async tests can hang or flake in CI. *Mitigation:* loopback + relay-disabled + bounded `timeout` on every await; real-network tests `#[ignore]` (D9/OQ-2).
- **R5 (iroh 1.0 API drift from the spike-doc recon).** Names/signatures may differ from this spec's sketch (as they did for blobs — `remote_node_id` → `endpoint_id`). *Mitigation:* treat `spike-blobs` (compiled against the same pin) as the API oracle; verify `remote_id()`/`ProtocolHandler`/`open_bi` signatures during steps 3/7 and record divergences in `NOTES.md`.
- **R6 (double-connect frame loss).** A naive both-dial mesh could drop frames on the closed connection. *Mitigation:* deterministic tie-break (D8) + the engine's re-pull-on-reconnect contract; T4 proves delivery survives a reconnect.
- **R7 (scope creep into the CLI / sync).** Easy to over-build into room lifecycle or re-implement sync. *Mitigation:* N1/N5 hard non-goals; the adapter is a carrier + gate only, validated by AC-scoped tests.

---

## 13. Rollout / sequencing

1. Land `crates/iroh-rooms-net` with unit + loopback tests green under `scripts/verify.sh` (`--all-features`, `-D warnings`).
2. Run the Gate-A two-machine procedure if a rig is available; record results in `NOTES.md` (or mark loopback-only + residual).
3. Update the README *Current Status* / *Remaining targets* to mark the full-mesh transport prototype landed (mirroring how #46/#47/#48/#49/#50 updated status), noting Gate A's standing.
4. Follow-ups (separate issues): CLI wiring + runtime (N1), real invite-ticket discovery + mDNS (N3/N4), `MembershipSnapshot` re-point of `Admission` (D6) and the fail-closed overlay (OQ-6), production hardening (N6), chaining the blob/pipe ALPNs onto the shared `Router`.

There is no production data migration and no rollback surface: this adds an isolated crate behind a stable seam; reverting is removing the crate from the workspace.
```