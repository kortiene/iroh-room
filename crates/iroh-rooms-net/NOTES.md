# IR-0005 — Full-Mesh QUIC Event Transport: findings

Spike deliverable for `crates/iroh-rooms-net` (issue #9 / IR-0005), per spec
`specs/full-mesh-quic-event-transport.md` §13 and the issue Test Plan. This is the
first prototype slice of the **shipping** `iroh-rooms-net` adapter — the real
full-mesh direct-QUIC carrier behind the landed, sans-IO `SyncEngine`.

## Acceptance criteria → evidence

| # | Acceptance criterion | Status | Evidence |
|---|----------------------|--------|----------|
| 1 | Two peers exchange a signed `WireEvent` over the custom ALPN | ✅ | `tests/loopback.rs::t1_*`; `net-smoke` loopback run (genesis received over `/iroh-rooms/event/1` in ~59 ms). ALPN byte string pinned in `alpn.rs` test. |
| 2 | Unknown remote endpoint rejected **before** event bytes are accepted | ✅ | `tests/loopback.rs::t2_*` (dialer never reaches `Connected`; reject decided from `remote_id()` and the connection closed **before** `accept_bi()`); audit line `peer.rejected cause="unknown_device"`. |
| 3 | Connection state distinguishes connected / offline / unauthorized | ✅ | `tests/loopback.rs::t3_*`; `PeerConnState` + `ConnEvent` stream + `peer_states()`. |
| 4 | Basic reconnect behavior observed and documented | ✅ | `tests/loopback.rs::t4_*` (`Connected → Offline/Connecting → Connected`, then a post-reconnect event still arrives); see **Reconnect** below. |
| — | Security: admission via iroh-authenticated remote endpoint identity | ✅ | The gate authorizes on `Connection::remote_id()` **only**, reject-before-`accept_bi()` (T2; §6 of the spec). |
| — | Test plan: two local peers + ≥1 real-network run *if available* | ⚠️ partial | Loopback CI suite present (T1–T4). The real-NAT Gate-A run was **not** performed (no two-NAT rig during this phase); carried as a residual — see **Gate A**. |

`cargo test -p iroh-rooms-net`: 67 unit + 9 frame-codec integration
(`tests/frame.rs`) + 9 loopback integration (`tests/loopback.rs`, T1–T9) = **85
tests** pass; all suites run in <1 s with no relay and no real network
(deterministic for CI, spec D9/OQ-2).

## Pinned versions (re-confirmed against the resolved lockfile)

- `iroh = "=1.0.1"` (the pin proven by `spike-blobs`; resolves `iroh-base 1.0.1`,
  `noq 1.0.1`/`noq-proto 1.0.1` — the QUIC core — transitively). Zero 0.x crates on
  the event critical path (ADR-1 churn argument / D5).
- `tokio = "1"` (`rt-multi-thread`, `macros`, `time`, `sync`).
- `iroh-rooms-core` with `features = ["sync"]` (transitively enables `store`, so
  `SyncEngine` + `EventStore` are available to the driver/binary from one feature).

## Connection-state model (AC3)

`PeerConnState` (in `state.rs`) is the exact PRD §16.3 distinction plus a transient
dial state:

- `Connecting` — a dial is in progress.
- `Connected` — authenticated Active member, live bidi stream up.
- `Offline` — an authorized member with **no path right now** (dial failing / link
  dropped). We keep redialing.
- `Unauthorized` — a device that is **not** a bound Active member; refused
  regardless of reachability. Recorded both for inbound rejections (handler) and
  for an outbound dial whose remote refused us via the stable `REJECT_CODE` close.

Surfaced two ways: `NetTransport::peer_states()` (snapshot) and a
`tokio::sync::broadcast<ConnEvent>` change stream (`from → to` transitions). `set`
emits exactly one event per real transition (idempotent re-sets are no-ops), so the
engine driver's `on_connect`/`on_disconnect` fire once per link change.

## Reconnect behavior (AC4)

The per-peer dial loop (`peer.rs::dial_loop`) keeps an outbound link alive: on a
drop it marks the peer `Offline`, sleeps a bounded exponential backoff with
deterministic jitter (250 ms → 8 s, first redial fast), and redials. iroh handles
hole-punching/relay re-establishment beneath; the adapter only re-opens the bidi
stream and the engine replays `on_connect` (which re-pulls). T4 forces a link drop,
observes `Connected → Offline/Connecting → Connected`, and then publishes a **new**
event that still reaches the peer — proving the stream was genuinely re-established,
not just the state flag flipped.

A dial that reaches a **non-member** (proven `remote_id()` fails admission) records
`Unauthorized` and **stops** redialing — reachability is not the problem.

## Observed iroh 1.0.1 API (vs. the spec's recon sketch — R5)

Confirmed against the pinned source; no blocking divergences:

- **A1 confirmed**: `Connection::remote_id() -> EndpointId` returns the
  QUIC/TLS-authenticated remote id at/after `ProtocolHandler::accept`, with no
  application bytes read. Admission runs on it directly.
- `ProtocolHandler::accept(&self, Connection) -> Result<(), AcceptError>`;
  `Connection::{accept_bi, open_bi}().await -> (SendStream, RecvStream)`;
  `Connection::close(VarInt, &[u8])`; `closed()/close_reason() -> ConnectionError`.
- Streams (`SendStream::write_all/finish`, `RecvStream::read_exact`) and `VarInt`
  are re-exported at `iroh::endpoint::*` (from the `noq` QUIC core).
- `Endpoint::builder(presets::Minimal | presets::N0)`, `.secret_key()`,
  `.relay_mode(RelayMode::Disabled)`; `Endpoint::addr()` for the real-network
  dialable address; `bound_sockets()` for the loopback address.
- **Implementation correction to spec §4.3/§4.7**: those paragraphs say the reader
  hands bytes to `SyncEngine::ingest_frame`. The wire actually carries canonical-CBOR
  **`SyncMessage`** frames (`SyncMessage::encode()`, per D3/§4.3), so the receive
  path is `SyncMessage::decode()` + `SyncEngine::on_message()` (the engine driver
  does this; an undecodable frame is a logged drop). `ingest_frame` is the raw
  `WireEvent` entry point that `on_message` itself calls for `Events` frames. The
  transport still treats the body as opaque — the decode lives at the engine-driver
  boundary, not in the carrier.

## Key decisions / open questions resolved in this slice

- **OQ-1 (MSRV)**: per-crate — `iroh-rooms-net` does **not** inherit the workspace
  `rust-version = "1.80"` (the iroh 1.0 stack's MSRV is higher; the shipping crypto
  deps already force ≥1.85, so this only corrects the *declared* floor). Reconciling
  the workspace value is a follow-up.
- **OQ-3 (engine driver location)**: a thin `Node` runtime in this crate owns the
  `SyncEngine` and pumps it (one task; queries via a command channel). The CLI may
  later drive the engine manually against `NetTransport` directly — `NetTransport`
  implements `SyncTransport` and exposes the inbound stream + conn-event stream for
  exactly that.
- **OQ-4 (double-connect)**: the prototype **tolerates** both-dial double-connect —
  each bidi stream is full-duplex, ingest is idempotent, and the outbound table is
  last-writer-wins per device, so no frame destined to a connected peer is dropped.
  The loopback tests use single-direction dial (each pair = one full-duplex link), so
  they never exercise it. A deterministic lower-`EndpointId`-wins tie-break (D8) is a
  post-prototype follow-up.
- **OQ-6 (fail-closed overlay)**: `AllowlistAdmission` carries an explicit, tested
  `fail_closed` set (`RejectCause::FailClosed`) as the seam for the §0/§5
  incompleteness overlay; the prototype leaves it empty but honours it when
  populated. Production wiring fills it from `SyncEngine::fail_closed_subjects()`.
- **D6 (authorizer shape)**: `AllowlistAdmission` is `device → identity → Active?`,
  the same shape `MembershipSnapshot` produces (it already exposes
  `identity_of_device` + the Active set). The production re-point is a swap of those
  two lookups, not a reshape.

## Gate A (real-network) — STATUS: NOT YET RUN (measurement harness landed, IR-0012)

**A green loopback run is NOT Gate A.** The Gate-A NAT test (two physical machines
on different real NATs) has not been executed — it remains **owed before MVP go**.
What changed in IR-0012 (#43): the previously-missing rig now exists. A dedicated
substrate probe **`nat-probe`** (`crates/spike-nat`) and a full runbook +
GO/NO-GO rubric + results schema are landed and CI-proven; only the manual
two-host *execution* is outstanding.

Two complementary runs close Gate A (see `crates/spike-nat/NOTES.md` for the full
runbook):

1. **Substrate probe** — `nat-probe` measures the iroh substrate directly (bare
   `Endpoint` echo on `/iroh-rooms/nat-probe/1`): establishment, **path type read
   off iroh's `remote_info` active-addr set** (iroh 1.0.1 has no `ConnectionType`
   watcher — see `spike-nat/NOTES.md` §2), TTFB (direct + relay via `--relay-only`),
   RTT, and throughput, emitted as structured JSON.
2. **Confirmation pass over this carrier** — drives the *real shipping transport*
   across the same two hosts, no code change:
   - Event ALPN: `net-smoke listen --real` | `net-smoke dial <ENDPOINT_ID> --real`
     (both directions), recording establishment + time-to-first-event + path type
     (same `remote_info` method). This closes the IR-0005 Gate-A residual for the
     event plane.
   - Pipe ALPN: bring up `iroh-rooms pipe` across the two hosts (IR-0010) and
     confirm a byte crosses `/iroh-rooms/pipe/1` over the real NAT — closes the
     pipe ALPN's owed Gate A (see below).

Run ≥2 NAT scenarios incl. ≥1 likely-symmetric (CGNAT/mobile), both directions.
GO iff a path is established both directions within ≤10 s in every scenario via at
least relay fallback, with a direct hole-punched path in ≥1 non-symmetric scenario
and usable relay throughput (≥1 Mbit/s, RTT ≤ ~300 ms). Paste the rolled-up
`crates/spike-nat/results/results.md` table in place of the placeholder below.

| scenario | direction | established | path type | TTF event | RTT | throughput |
|----------|-----------|-------------|-----------|-----------|-----|------------|
| _(pending manual two-host run — harness: `crates/spike-nat`, `nat-probe`)_ | | | | | | |

### Loopback baseline (reference only — NOT Gate A)

From a `net-smoke` two-process loopback run (`RelayMode::Disabled`, same host):

| metric | value |
|--------|-------|
| connect (handshake) | ~21 ms |
| time-to-first-event (signed genesis over ALPN) | ~59 ms |
| non-member reject observed (dialer → `Unauthorized`) | ~43 ms |
| audit lines | `peer.accepted`, `peer.connected`, `peer.rejected cause="unknown_device"`, `peer.disconnected` |

## Confidentiality

Both direct and relay-fallback paths are QUIC/TLS between authenticated endpoints;
a relay forwards only ciphertext. The Gate-A notes must record path type but must
**not** imply relay = plaintext.

## Follow-ups (separate issues)

- ~~Wire the adapter into the `iroh-rooms` CLI binary + runtime (N1); remove the
  `demo` module's hardcoded seeds in favour of real identities + invite tickets
  (N3) and mDNS LAN discovery (N4).~~ **Closed** — landed across IR-0101–IR-0105
  (identity CLI, room creation, invite, join, messaging).
- ~~Re-point `Admission` to the live `MembershipSnapshot` + the fail-closed overlay
  (D6 / OQ-6).~~ **Closed by IR-0107** — `SnapshotAdmission` reads a shared
  `AdmissionView` cell that the pump refreshes on every fold change; a device removed
  mid-session begins being rejected within one tick.
- ~~Production hardening (N6); chain the blob/pipe ALPNs onto the shared `Router`.~~
  **Closed** — N6 (managed per-peer lifecycle) closed by IR-0107 (`PeerManager` +
  `spawn_room` + `RoomReconciler`); pipe/event ALPN chaining closed by IR-0010.
- ~~Roster-driven dial reconciliation (ADR-1 "per-room peer manager").~~ **Closed by
  IR-0107** — `PeerManager.reconcile` derives the desired outbound set from the live
  snapshot and starts/stops dial loops on membership change.
- Deterministic double-connect tie-break (D8 / OQ-4) — still a follow-up.
- **Run Gate A** and record the table above. The measurement harness is landed
  (`crates/spike-nat` / `nat-probe`, IR-0012) with a full runbook; what remains is
  the manual two-host execution (substrate probe + this carrier's confirmation
  pass) and pasting the results table here.
