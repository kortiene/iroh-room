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
| — | Test plan: two local peers + ≥1 real-network run *if available* | ✅ | Loopback CI suite present (T1–T4). Real-NAT Gate A measured in S1/S2 and refreshed on 2026-07-07 local↔`demo1`; residual caveats remain in **Gate A**. |

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

## Gate A (real-network) — STATUS: MEASURED 2026-07-03/04 + refreshed 2026-07-07 (CONDITIONAL GO)

**A green loopback run is NOT Gate A.** Two real NAT environments were measured
(37 JSONs): **S1** home-broadband (Spectrum home-router NAT, wifi) ↔ Hetzner
public server behind a stateful ufw INPUT-DROP firewall (2026-07-03 + a
`--settle 30` #43 reconciliation pass); **S2** an iPhone cellular Personal Hotspot
(carrier CGNAT — the likely-symmetric environment) ↔ {Hetzner cloud;
home-broadband NAT} (2026-07-04). Different real networks, no VPN bridge, both
directions × {natural, relay-only}. Measured verdict: **a direct hole-punched
path establishes on every run in both environments** (native IPv6 throughout, plus
a punched IPv4 path even between the cellular CGNAT and the home NAT), and
**establishment + relay reachability PASS both directions** (incl. inbound to the
CGNAT client). nat-probe *labels* the runs `mixed` only because iroh 1.0.1 keeps
the relay addr Active as warm standby with no ConnectionType watcher — a classifier
artifact, unchanged at `--settle 30`, corroborated on S1 by the #43 SDK-daemon run.
**Residuals are non-connectivity:** forced-relay throughput over the cellular
uplink read 0.1–0.2 Mbit/s on 256 KiB samples (below the ≥1 Mbit/s target — a
larger-sample re-measure is owed), and the home-NAT→CGNAT reverse leg was not run.
Refresh: a 2026-07-07 operator-local ↔ `demo1` cloud run passed both directions,
natural and relay-only; relay-only measured 4.1 Mbit/s BtoA and 1.3 Mbit/s AtoB.
Findings block: `crates/spike-nat/NOTES.md` §6.
Tooling: a compile-time `relay-only-test` build seam (PR #107, off by default) now
supports a controlled relay-only run for the owed cellular re-measure and the
reverse leg — it is tooling, not evidence, and discharges neither residual.

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
     confirm a byte crosses `/iroh-rooms/pipe/1` over the real NAT — confirming
     the pipe ALPN over the measured Gate A carrier (see below).

Run ≥2 NAT scenarios incl. ≥1 likely-symmetric (CGNAT/mobile), both directions.
GO iff a path is established both directions within ≤10 s in every scenario via at
least relay fallback, with a direct hole-punched path in ≥1 non-symmetric scenario
and usable relay throughput (≥1 Mbit/s, RTT ≤ ~300 ms).

Measured results, 2026-07-03/04, both environments (full per-run table:
`crates/spike-nat/results/results.md`, 37 runs). S1 = home-NAT ↔ cloud
(non-symmetric); S2 = cellular CGNAT hotspot ↔ {cloud, home-NAT}
(likely-symmetric):

| env | scenario | direction | mode | established | settled path† | ttfb (ms) | rtt (ms) | tput (Mbit/s) |
|----|----------|-----------|------|-------------|--------------|-----------|----------|---------------|
| S1 | home-broadband↔hetzner | BtoA | natural (×3) | yes | mixed | 638–1005 | 113.5–126.6 | 0.7–3.8 |
| S1 | home-broadband↔hetzner | BtoA | relay-only | yes | relay | 1074 | 132.0 | 3.3 |
| S1 | home-broadband↔hetzner | AtoB | natural (×3) | yes | mixed | 976–1539 | 109.1–129.8 | 1.1–1.8 |
| S1 | home-broadband↔hetzner | AtoB | relay-only | yes | relay | 1141 | 144.1 | 1.2 |
| S1 | home-broadband↔hetzner | both | settle30 (×5) | yes | mixed | 753–1439 | 121.8–149.1 | — |
| S1 | home-broadband↔hetzner | both | natural (×10 @8MiB) | no* | none* | — | — | — |
| S2 | cgnat↔hetzner | AtoB | natural+settle30 (×4) | yes | mixed | 1121–1482 | 155–166 | — |
| S2 | cgnat↔hetzner | AtoB | relay-only | yes | relay | 1159 | 171.6 | 1.2 |
| S2 | cgnat↔hetzner | BtoA (inbound-to-CGNAT) | natural (×3) | yes | mixed | 1211–1683 | 179.9–180.9 | — |
| S2 | cgnat↔hetzner | BtoA | relay-only | yes | relay | 1207 | 297.8 | 0.2‡ |
| S2 | cgnat↔home-broadband | AtoB | natural+settle30 (×4) | yes | mixed | 403–650 | 96.9–131.3 | — |
| S2 | cgnat↔home-broadband | AtoB | relay-only | yes | relay | 648 | 113.2 | 0.1‡ |

† `mixed` = a direct addr Active **and** the relay addr Active (warm standby); a
direct hole-punched path is up on every established run (native IPv6 in all pairs,
plus a punched IPv4 path in S2 cgnat↔home). The `settle30` rows (`--settle 30`,
issue #43) confirm the relay never drops to Inactive even at a 30 s window, so
nat-probe's addr-set classifier structurally can't emit sole-`direct` — a label,
not a punch failure (corroborated on S1 by the #43 SDK-daemon traffic-path
evidence).
‡ forced-relay throughput bottlenecked by the cellular **uplink**, on 256 KiB
slow-start-dominated samples — below the ≥1 Mbit/s target; a larger-sample
re-measure is owed (natural S2 sessions used the healthy direct path).
\* failed only at the bulk-transfer stage: the probe's fixed 30 s per-op budget
vs 0.6–3.8 Mbit/s sustained on the auto-selected path; connect/TTFB/RTT
succeeded in every paired supplement run. Confirmation pass over this carrier:

2026-07-07 refresh using `root@demo1` as the cloud peer:

| scenario | direction | mode | established | settled path | ttfb (ms) | rtt (ms) | tput (Mbit/s) |
|----------|-----------|------|-------------|--------------|-----------|----------|---------------|
| operator-local↔demo1 | BtoA | natural | yes | mixed | 1116 | 108.7 | 5.3 |
| operator-local↔demo1 | BtoA | relay-only | yes | relay | 789 | 129.1 | 4.1 |
| operator-local↔demo1 | AtoB | natural | yes | mixed | 889 | 123.7 | 8.6 |
| operator-local↔demo1 | AtoB | relay-only | yes | relay | 1124 | 143.6 | 1.3 |

The refresh did not independently verify VPN/shared-LAN status, so it is a
current evidence refresh rather than a replacement for the S1/S2 coverage.
event ALPN ✓ both directions (signed genesis across the NAT in 1.06/1.08 s;
non-member rejected before any event bytes, `unknown_device`); pipe ALPN ✓
(HTTP round-trip through an authenticated `pipe expose`/`pipe connect` across
the real NAT — full CLI flow: identity → room → key-bound invite → cross-NAT
join → pipe).

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
- ~~Sync state restart durability (core prototype D7/OQ-3) — the orphan park,
  unconfirmed admin-tip suspicion, backfill token buckets, and equivocation audit
  trail were lost on process restart.~~ **Closed by IR-0201** (#26 / IR-0201):
  store schema v2 (`user_version = 2`) adds five derived-cache tables
  (`sync_state`, `sync_backfill_tokens`, `sync_parked`, `sync_parked_missing`,
  `trust_decisions`); `SyncEngine::open` restores them with fail-closed re-arming
  before the first access decision; per-mutation checkpoint hooks persist each state
  change transactionally inside the `Node` pump.
- Deterministic double-connect tie-break (D8 / OQ-4) — still a follow-up.
- **Gate A measured; keep refresh evidence current.** The measurement harness is
  landed (`crates/spike-nat` / `nat-probe`, IR-0012), S1/S2 are committed, and
  a 2026-07-07 local↔`demo1` refresh is recorded. Remaining work is sign-off on
  the cellular relay-throughput caveat and the home-NAT→CGNAT reverse leg.
- ~~ADR-1 mesh choice validated only on this landed loopback carrier, not
  measured against gossip.~~ **Closed by IR-0006** (#10): `spike-transport`
  measured full-mesh against a minimal `iroh-gossip` backend at N=2..5 and
  ratified ADR-1 (mesh admission-before-bytes corroborates T2 above; see
  `crates/spike-transport/NOTES.md` for the measured table and decision).
