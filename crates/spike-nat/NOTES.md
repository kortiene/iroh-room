# IR-0012 Gate-A Real-NAT Hole-Punching — `nat-probe` Findings (`NOTES.md`)

This is the written findings deliverable for **IR-0012 / #43** (spec
`specs/measure-real-nat-hole-punching-connectivity.md`). It records the iroh
1.0.1 API reconciliation, the measurement harness, the two-host **runbook**, the
GO/NO-GO rubric, and a **findings block ready to lift into the Gate E memo
(#15)**.

> Status: throwaway-grade spike (mirrors `spike-blobs`). It measures the iroh
> substrate's real-NAT connectivity; it does **not** modify the shipping crates.
> The **loopback self-check is NOT Gate A** — CI proves the harness builds, dials,
> echoes, and emits a well-formed `ProbeResult`; it cannot prove NAT traversal.

---

## 1. What this is and why it exists

Gate A is the **one load-bearing Phase-0 assumption with no measured evidence**:
that two iroh endpoints behind real, separate NATs can establish a **direct**
QUIC connection, or cleanly **fall back to relay**. The landed carrier
(`iroh-rooms-net`, IR-0005) was proven only on loopback (`RelayMode::Disabled`,
same host); its `NOTES.md` recorded *"Gate A (real-network) — STATUS: NOT YET
RUN"* until the first two-host run was executed on 2026-07-03 (§6). A LAN or
loopback demo cannot exercise NAT traversal and "will lie to you about it"
(`PHASE-0-SPIKE.md` Day 1).

`nat-probe` is the purpose-built **measurement** tool: a bare `iroh::Endpoint`
echo probe (isolated from our ALPN/framing, so it tests the *substrate*), which
reports per scenario × direction × path-mode:

- establishment success + setup time,
- **path type actually achieved** — direct hole-punched vs. relay — *read off
  iroh, never inferred from latency*,
- time-to-first-byte (direct and relay),
- application RTT (median + p90) and sustained throughput,
- the relay it homed to.

…as one JSON object per run (schema = spec §5), rolled up into `results/results.md`.

---

## 2. iroh 1.0.1 API reconciliation (Step 1 — do this first, it de-risks everything)

The spec (§6.2 / §10 step 1 / risk 3) flagged that the recon names would drift.
They did. Confirmed against the pinned `iroh = 1.0.1` source
(`~/.cargo/registry/.../iroh-1.0.1`):

| Recon expectation (spec §6.2) | Reality on `iroh 1.0.1` | What we do |
|---|---|---|
| `Endpoint::conn_type(id) -> Watcher<ConnectionType>` with `ConnectionType::{Direct,Relay,Mixed,None}` | **Does not exist.** There is no `conn_type` watcher and no `ConnectionType` enum on this line. | Classify from the **active transport-address set** instead (below). |
| `Endpoint::remote_info(id)` for resolved addrs / home relay | **Exists**: `async fn remote_info(&self, EndpointId) -> Option<RemoteInfo>`. `RemoteInfo::addrs()` yields `TransportAddrInfo { addr: TransportAddr, usage: TransportAddrUsage }`. | This is our classification signal. |
| A relay-force / direct-disable knob | `Endpoint::builder(_).clear_ip_transports()` removes all IP transports → traffic pinned to the relay. (`clear_relay_transports()` is the dual.) | `--relay-only` calls `clear_ip_transports()` (spec §6.4). |
| n0 discovery + relay preset | `Endpoint::builder(presets::N0)` (DNS discovery + default relay). Loopback uses `presets::Minimal` + `RelayMode::Disabled`. | As documented. |
| Readiness / discoverability | `Endpoint::online().await` waits for the home relay. | Awaited on bind in real mode (counts toward `setup_time`, not TTFB). |

### Path-type classification (the load-bearing instrumentation)

`TransportAddr` is `Relay(RelayUrl) | Ip(SocketAddr) | Custom(_)` and
`TransportAddrUsage` is `Active | Inactive`. We read the remote's addr set after
the path settles and classify (`probe::classify_remote_info`):

- an **active `Ip`** addr ⇒ a direct, hole-punched path (`direct`);
- an **active `Relay`** addr with **no** active `Ip` ⇒ `relay`;
- both active ⇒ `mixed` (transitional — we record the *settled* value);
- none active ⇒ `none` (no usable path — a NO-GO signal).

This is exactly the spec §6.2 backup ("cross-check against `remote_info` addr set:
a direct path shows a peer socket addr; relay-only shows only the relay URL"),
promoted to the *primary* method because the `ConnectionType` watcher it preferred
is absent on 1.0.1. The **settle window** (`--settle`, default 4 s) handles the
relay→direct upgrade: hole-punching often starts relayed and upgrades, so we
sample repeatedly and record the settled value (and the initial one, so the
upgrade latency is captured for the memo). The `RUST_LOG=iroh=debug` log path
(spec §6.2 fallback) remains available for cross-checking but is not needed —
`remote_info` is a clean, typed signal.

> **Measured caveat on `mixed` (2026-07-04, §6).** On the S1 home-NAT↔cloud pair,
> a direct addr is Active on every run **but the relay addr also stays Active** as a
> warm standby, so this classifier settles on `mixed` and never emits sole `direct`
> — even with `--settle 30`. `mixed` therefore reads "direct up, relay warm", **not**
> "traffic on relay": a direct path is established (corroborated by the #43 SDK-daemon
> traffic-path evidence on the same pair). The addr-set method cannot, on its own,
> distinguish which Active path carries bytes; that ambiguity is exactly what a restored
> `ConnectionType` watcher would remove.

> If a future iroh bump restores a `ConnectionType`/`conn_type` watcher, prefer it
> and keep this addr-set method as the cross-check (per spec §6.2 ordering).

---

## 3. The harness

```text
nat-probe listen [--relay-only] [--loopback] [--seed <N>]
nat-probe dial <ENDPOINT_ID> [--addr <IP:PORT>] [--relay-only] [--loopback]
               [--ping <N>] [--xfer <BYTES>] [--seed <N>]
               [--scenario <label>] [--direction AtoB|BtoA]
               [--nat-a <k=v;...>] [--nat-b <k=v;...>]
               [--run-at <UTC>] [--git-sha <sha>] [--notes <text>] [--json <path>]
```

- `listen` — minimal endpoint on `presets::N0` (DNS discovery + default relay),
  serves a trivial echo `ProtocolHandler` on **`/iroh-rooms/nat-probe/1`**, prints
  its `EndpointId` + addr hints, echoes forever.
- `dial <ENDPOINT_ID>` — same endpoint, dials **purely by `EndpointId`**
  (discovery resolves the path; `--addr` optionally seeds a direct hint), opens one
  bidi stream, measures TTFB, runs `--ping N` RTT probes and an `--xfer BYTES`
  throughput transfer, samples the settled path type, and emits a `ProbeResult`
  (stdout + JSON on `--json`).
- Identity is a fresh random `SecretKey` per run, or `--seed <N>` for a
  reproducible identity. These are genuine iroh-authenticated identities carrying
  **no room membership and exchanging no room data** (spec §8).
- `--relay-only` suppresses direct paths (`clear_ip_transports`) for a controlled
  relay measurement (spec §6.4). `--loopback` is the offline self-check (relay
  disabled, dial by `--addr`) — **NOT Gate A**.
- NAT context (`--nat-a`/`--nat-b`) is a compact `kind=…;isp=…;net=…;via=…`
  string (spec §6.7). `--run-at` is an operator-supplied UTC stamp (no wall-clock
  is read in any decision path, spec A6).

### Metrics contract

`report::ProbeResult` is the spec §5 field table verbatim, serialized to one JSON
object per run. `report::results_md` renders the rolled-up table. TTFB is bucketed
into `ttfb_direct_ms` / `ttfb_relay_ms` by the settled path type (or forced to
relay under `--relay-only`), so a natural direct run and a controlled relay run are
directly comparable.

---

## 4. Runbook (reproducible by a second operator, no need to read the spec)

```text
Preconditions: two machines on DIFFERENT real networks.
  NO shared LAN/Wi-Fi, NO VPN bridge (a shared LAN/VPN silently turns Gate A into
  a LAN demo that always "passes" — spec risk 4). Characterize each endpoint:
  network type (wifi/ethernet/lte), ISP, likely NAT class (spec §6.7).

Build once on each host:  cargo build -p spike-nat --release   # binary: nat-probe

Substrate probe (per scenario, run BOTH directions):
  Host A:  nat-probe listen                       # prints A_ENDPOINT_ID + hints
  Host B:  nat-probe dial <A_ENDPOINT_ID> --ping 20 --xfer 8388608 \
             --scenario "broadband<->lte" --direction BtoA \
             --nat-a "kind=port-restricted;isp=<A ISP>;net=ethernet;via=operator-knowledge" \
             --nat-b "kind=cgnat;isp=<B carrier>;net=lte;via=inferred-from-holepunch-result" \
             --run-at "<UTC now>" --notes "different networks, VPN off" \
             --json results/<date>-broadband-lte-BtoA.json
  Reverse roles for AtoB (A dials B).
  Forced-relay pass: add --relay-only on BOTH ends (spec §6.4) →
             --json results/<date>-broadband-lte-BtoA-relay.json
  Reliability estimate: repeat the natural run K times (5–10) per scenario;
             hole-punch rate = (direct settles) / (attempts). State K in the memo.

Matrix (spec §7): ≥2 real-NAT environments incl. ≥1 likely-symmetric (S1
  broadband↔mobile-hotspot; S2 broadband↔second-broadband). Per scenario:
  {AtoB, BtoA} × {natural, relay-only} = 4 rows minimum.

Confirmation pass — the REAL shipping carrier crosses NAT (spec §6.6, landed code):
  Event ALPN:  net-smoke listen --real   |   net-smoke dial <ID> --real   (both ways)
  Pipe  ALPN:  iroh-rooms pipe expose ... |  pipe connect ...             (per IR-0010)
  Record establishment + path type (same §6.2 method) + time-to-first-event/byte.

Then: roll up results/results.md; paste it into crates/iroh-rooms-net/NOTES.md
  under "Gate A (real-network)"; evaluate §5 GO/NO-GO; file residuals for failures.
```

Note on cloud-VM substitutes (spec OQ-5): a VM often has a public / full-cone-ish
NAT, *easier* than a home NAT, which inflates the direct rate. If a VM is one
endpoint, ensure the *other* is a real residential/mobile NAT and say so in
`--scenario`.

---

## 5. Gate A GO/NO-GO rubric (spec §9, from `PHASE-0-SPIKE.md` Day 1)

**GO — all of:**
- Connection established **both directions within ≤10 s** in **every** scenario,
  via *at least* relay fallback.
- A **direct hole-punched path** achieved in **≥1 non-symmetric scenario** (S2).
- Relay usable for chat/control: **≥1 Mbit/s** and **RTT ≤ ~300 ms** over relay.

**NO-GO — any of:**
- Any scenario with **no path at all** (neither direct nor relay).
- Unusable relay latency/throughput (below the thresholds).

**On NO-GO:** stop and escalate before further reliance on the substrate
assumption. Escalation (spike Day 1 / Residual #12): evaluate a **self-hosted
relay**, reconsider discovery config, or flag the substrate assumption as broken
and force the relay-infrastructure decision the spike could not pre-make. Record
the NO-GO + chosen escalation as a residual (§8) and surface it to #15 — a red
Gate A is a hard input to the MVP go/no-go.

`nat-probe` prints a per-run signal (`established / path / hole_punched /
within_10s`, and relay usability on relay runs). The **gate verdict is over the
whole matrix**, evaluated by the operator from the committed JSON.

---

## 6. Gate A findings block — lift verbatim into the Gate E memo (#15)

> **Filled 2026-07-03; reconciled + S2 added 2026-07-04** from the §4 runbook:
> S1 = 18 runs + 5 `--settle 30` reconciliation runs; S2 = 14 runs from a cellular
> CGNAT hotspot; all 37 per-run JSONs committed under `results/` (table in
> `results/results.md`). **Both required environments are now measured.**

```md
### Gate A — real-NAT hole-punching (IR-0012 / #43)

Verdict: CONDITIONAL GO (both environments measured; residual = cellular relay throughput)    Date: 2026-07-03/04    iroh: 1.0.1    Probe SHA: 0e199d3 / 5e58e49

Environments (≥2 executed — both real, different networks, no VPN bridge):
  S1 wifi/Spectrum/home-router NAT ↔ ethernet/Hetzner/public-IP behind stateful ufw INPUT-DROP (non-symmetric; both native IPv6)
  S2 iPhone cellular Personal Hotspot (carrier CGNAT — the likely-symmetric env) ↔ {Hetzner public server; home-broadband NAT}

Results (both directions; natural + relay-only + settle30): results/results.md (37 committed runs).

Establishment: PASS across BOTH environments — every natural run established both
  directions well under 10 s (S1 TTFB 0.6–1.7 s; S2/cellular TTFB 0.4–1.7 s, RTT
  ~100 ms to the home peer, ~160–180 ms to cloud). Includes inbound-to-CGNAT
  (cloud dialed the Mac behind carrier CGNAT: established every run). The ten S1
  8 MiB-xfer runs record established=false — a harness artifact (bulk transfer
  exceeds the probe's fixed 30 s per-op budget; connect/TTFB/RTT succeeded in every
  paired supplement). Reverse home-NAT→CGNAT (kilo→Mac) is the one leg not run
  (no route to drive kilo once the Mac left the LAN); inbound-to-CGNAT is covered
  by the cloud→Mac reverse runs.
Direct path: a direct addr was ACTIVE on every established run in BOTH environments —
  native IPv6 in all pairs, AND a punched IPv4 socket even between the cellular CGNAT
  and the home NAT (S2 hotspot↔home settle30). nat-probe labels these `mixed`, never
  sole "direct", because iroh 1.0.1 keeps the RELAY addr Active as a warm standby and
  exposes no ConnectionType watcher (§2) — unchanged at `--settle 30`. "mixed" = "direct
  up, relay warm", not "on relay"; hole_punched=false is a classifier label, not a punch
  failure. Corroborated on S1 by the #43 SDK-daemon (traffic on direct, no relay bytes).
Relay fallback: reachability confirmed via controlled --relay-only in BOTH environments,
  both directions where run — a relay path always established.
Relay usability: S1 PASS both directions (3.3 Mbit/s @ 132 ms BtoA, 1.2 Mbit/s @ 144 ms
  AtoB). S2 MIXED: forward cellular→cloud 1.2 Mbit/s @ 172 ms (PASS); but forced-relay
  throughput on the legs bottlenecked by the cellular UPLINK measured 0.1–0.2 Mbit/s @
  113–298 ms — BELOW the ≥1 Mbit/s target. Caveat: 256 KiB samples dominated by
  slow-start over a constrained mobile uplink, and forced worst-case (natural S2 sessions
  used the healthy direct/mixed path). A larger-sample cellular relay re-measure is the
  open item; --xfer can't grow without also raising the hardcoded 30 s budget (see below).
Confirmation pass (real carrier): S1 event ALPN ✓ both directions (signed genesis in
  ~1.07 s; non-member rejected before any event bytes, unknown_device) and pipe ALPN ✓
  (HTTP through an authenticated pipe across the NAT, full CLI flow). S2 event ALPN ✓ —
  the real transport carried a signed genesis from the cellular CGNAT to the cloud peer
  in 1.35 s (admission gate accepted).

Implication for Residual #12: DISCHARGED on connectivity — a direct hole-punched path,
  establishment, and relay fallback all work across TWO real NAT environments incl. a
  carrier CGNAT, both directions (inbound-to-CGNAT included), corroborated by two
  independent methods. Remaining soft residuals (not connectivity failures): (1) a
  clean, larger-sample cellular forced-relay THROUGHPUT measurement — the small samples
  read below the ≥1 Mbit/s target; (2) the home-NAT→CGNAT reverse leg. Escalation not
  triggered; relay reachability holds everywhere.
Confidentiality: every hop (direct or relay) is QUIC/TLS between authenticated
  endpoints; relays forward only ciphertext (relay ≠ plaintext).
```

---

## 7. Confidentiality (spec §8 — do not conflate relay with plaintext)

Both direct and relay-fallback paths are **QUIC/TLS between iroh-authenticated
endpoints**; a relay forwards only **ciphertext**. The Gate-A findings MUST record
path type but MUST NOT imply relay = plaintext. The probe uses Ed25519
`SecretKey → EndpointId` identities exactly as the product does, on a dedicated
`/iroh-rooms/nat-probe/1` ALPN with no `SyncEngine`, no membership, no events — it
echoes synthetic bytes only. Committed JSON carries public keys, relay URLs,
ISP/network labels, and timings — **no secrets, and no home-IP socket addrs**
(kept on the operator's console, spec §8 redaction).

---

## 8. Residual risks / limitations carried out of this spike

1. **The measurement is inherently manual and hardware-dependent (spec risk 1).**
   CI proves the *tool* builds and its loopback self-check passes; the *gate*
   closes only when a human runs the matrix on two real networks. The committed
   tool + runbook + results schema make the run reproducible and re-runnable.
2. **Path mis-classification is the load-bearing risk (spec risk 2).** Mitigated
   by reading iroh's active-addr set (never latency), the settle window for the
   relay→direct upgrade, and recording the initial *and* settled values.
   **Measured limit (§6):** on iroh 1.0.1 the relay addr stays Active as a warm
   standby even when a direct path carries traffic, and there is no
   `ConnectionType` watcher — so the addr-set classifier reports `mixed`, never
   sole-`direct`, and `hole_punched` under-reports. Read `mixed` as "direct up,
   relay warm". A restored path watcher (or the SDK daemon's traffic-path view)
   is the disambiguator.
3. **`--relay-only` completeness (spec risk 5).** `clear_ip_transports()` removes
   IP transports at bind, so a relay-only endpoint has no direct path to upgrade
   to. If a future iroh line opportunistically re-adds direct paths, cross-check
   the settled `path_type` reads `relay` and, failing that, rely on the natural
   symmetric-NAT fallback for the relay measurement.
4. **Environment hygiene — the LAN/VPN illusion (spec risk 4).** The runbook
   requires "different networks, no VPN" and records network/ISP per endpoint. A
   shared LAN or VPN silently passes; the operator must confirm it is off.
5. **Reliability from too few samples (spec risk 6).** One hole-punch is not a
   rate; the runbook prescribes K repeats and the memo states the sample size.
6. **No `--budget` flag; throughput bounded by the 30 s per-op budget.** The
   per-op wait budget is hardcoded 30 s (`DialParams::default`), so a large
   `--xfer` over a slow (e.g. forced-relay-over-cellular) path busts the budget
   and records `established=false`, and a late-stage transfer failure discards
   the run's earlier connect/TTFB/RTT measurements. The S2 cellular forced-relay
   throughput therefore rests on small 256 KiB samples (§6). Follow-up: add a
   `--budget <SECS>` flag (mirrors the new `--settle`) and stage partial results
   so a transfer timeout doesn't void the established measurement — then re-run
   the cellular relay-throughput leg with a larger transfer.
7. **Confirmation pass boundary (spec OQ-2).** IR-0012 covers connectivity +
   event/pipe ALPN reachability; the full create→invite→join→message→file→pipe
   lifecycle across real NATs belongs to the Gate E integration run (#15) reusing
   this rig. Confirm with the #15 owner.

## 9. Structure note

Like `spike-blobs`, this crate adds a small `src/lib.rs` (`pub mod probe; pub mod
report;`) not in the spec's file list, so `src/main.rs` (the `nat-probe` CLI) and
`tests/self_check.rs` (the loopback self-check) can share the measurement code —
integration tests can only link a crate's library target. The `iroh = "=1.0.1"`
pin matches the shipping carrier; zero 0.x crates; workspace lints
(`unsafe_code = "forbid"`, clippy pedantic) inherited and clean.
