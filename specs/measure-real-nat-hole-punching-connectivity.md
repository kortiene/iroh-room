# Measure Real-NAT Hole-Punching Connectivity (Gate A) — IR-0012 / #43

- **Issue:** #43 — [IR-0012] Measure real-NAT hole-punching connectivity (Gate A)
- **Labels:** `type/test` `area/transport` `priority/p0` `risk/high`
- **Parent:** #1 (Phase 0 epic)
- **Feeds:** #15 (Gate E go/no-go memo) — the consumer of this deliverable
- **Related:** #9 / IR-0005 (full-mesh QUIC event transport — landed, carries the open Gate-A residual), #14 / IR-0010 (live TCP pipe ALPN — inherits the same residual), #22 / IR-0107 (peer connection manager)
- **Traceability:** `PHASE-0-SPIKE.md` → *Spike Plan* Day 1 (Gate A, real-network NAT connectivity) and *Residual Risks* open decision #12; `PRD.v0.3.md` §9.1 Transport, §18.1 Connectivity Risk.
- **Kind:** measurement / test deliverable. **No changes to production code under `crates/*/src/` of the shipping crates.** The measurement harness lands as a throwaway spike crate; the durable outputs are a results file, an updated `NOTES.md` Gate-A table, and a memo-ready findings artifact.

---

## 1. Summary

Gate A is the **one load-bearing Phase-0 assumption with no measured evidence**: that two iroh endpoints behind real, separate NATs can establish a direct QUIC connection, or cleanly fall back to relay. Every plane in the product rides on the Room Event Plane, which rides on the iroh substrate; a LAN or loopback demo cannot exercise NAT traversal and "will lie to you about it" (`PHASE-0-SPIKE.md` Day 1). The landed transport prototype (`crates/iroh-rooms-net`, IR-0005) is proven only on loopback (`RelayMode::Disabled`, same host) — its `NOTES.md` explicitly records **"Gate A (real-network) — STATUS: NOT YET RUN"** and carries it as a residual owed before MVP go.

This spec defines the work to **close Gate A empirically**:

1. A small **measurement harness** (`nat-probe`) that stands up two iroh endpoints on separate real NATs, attempts a direct connection via n0 DNS discovery + relay, and records — per scenario, per direction — establishment success, **path type actually achieved** (direct hole-punched vs. relay-fallback, *read off iroh's path/connection-type watcher, never assumed*), time-to-first-byte, RTT, and sustained throughput.
2. A **forced-relay mode** so relay fallback is confirmed to work (and its TTFB measured) even in scenarios where direct hole-punching happens to succeed.
3. A **confirmation pass** over the real shipping carrier (`net-smoke --real`, event ALPN; and the pipe ALPN) so we know Gate A holds for the actual product transport, not only a bare `Endpoint`.
4. A **structured, machine-readable results record** (JSON + a Markdown table) that drops directly into the `crates/iroh-rooms-net/NOTES.md` Gate-A table and into the Gate E go/no-go memo (#15).
5. A **GO/NO-GO evaluation** against the spike's Gate A thresholds, and, for any assumption that fails, a **residual-risk entry**.

This is inherently a **manual two-host measurement** (two physical machines on two different real networks). The deliverable is therefore a *tool + a runbook + a results artifact + a decision*, not a CI test. CI proves the harness builds and its offline/loopback self-check passes; it cannot prove NAT traversal.

---

## 2. Goal and non-goals

### Goal

Produce reproducible, dated evidence that answers, for at least two distinct real-NAT environments and both dial directions:

- Did a connection establish at all (direct or relay)? In how long?
- What **path type** did it achieve — direct hole-punched, or relay-fallback?
- What is the **time-to-first-byte** on the direct path and on the relay path?
- Does **relay fallback** work when direct hole-punching fails or is disabled?
- What **NAT types** were exercised (incl. ≥1 likely-symmetric / CGNAT / mobile case)?

…captured in a form that (a) feeds the Gate E memo (#15), (b) updates the `NOTES.md` Gate-A table in place, and (c) turns any failed assumption into a logged residual risk.

### Non-goals

- **Not** a code change to the shipping transport. The landed carrier (IR-0005/IR-0010) is used *as-is* for the confirmation pass; the measurement harness is a separate throwaway. If the run surfaces a transport defect, that is a **finding filed as a new issue**, not a fix in this PR.
- **Not** self-hosted relay deployment or discovery re-architecture. We measure the default n0 stack (`dns.iroh.link` + n0 relays). Choosing/deploying a self-hosted relay is an *escalation path* only triggered by a NO-GO (§9), out of scope here.
- **Not** production DoS/perf hardening, throughput tuning, or a formal NAT taxonomy study. We record enough to classify each environment and to pass/fail the gate.
- **Not** a scale test. Two endpoints (n=2), one bidi stream. The event-plane fan-out at n≤5 is a separate concern (IR-0107, already landed on loopback).
- **Not** exchanging any room data. The probe uses iroh-authenticated endpoint identities and echoes synthetic bytes only (§8).

---

## 3. Background — what already exists, and why it is not Gate A

Read this before building anything; the point is to *reuse* the landed carrier and add only the measurement gap.

- **`crates/iroh-rooms-net` (IR-0005)** — the shipping full-mesh QUIC carrier. `NetTransport` = an `iroh::Endpoint` keyed by `device_id` + a `Router` on ALPN `/iroh-rooms/event/1`. `Node::spawn` drives a real `SyncEngine`. It supports a real-network mode: `NetConfig { mode: NetMode::RealNetwork, .. }` uses the n0 DNS + relay stack; the default `NetMode::Loopback` sets `RelayMode::Disabled`.
- **`net-smoke` binary** (`crates/iroh-rooms-net/src/bin/net_smoke.rs`) — a two-mode harness (`listen` / `dial <ENDPOINT_ID>`) already wired for `--real`. It measures **time-to-first-event** (a signed genesis `WireEvent` over the ALPN) and prints connection-state transitions and endpoint addr hints. `NOTES.md` documents that a Gate-A run *can* be driven by it "with no code change needed."
- **Loopback baseline (reference only, NOT Gate A)** from `NOTES.md`: connect ≈21 ms, time-to-first-event ≈59 ms, non-member reject ≈43 ms — same host, `RelayMode::Disabled`. This proves framing/admission, not traversal.

**Why `net-smoke` alone is insufficient for Gate A.** It does not:

1. **Classify the path type.** It never reads iroh's connection-type / path watcher, so it cannot say *direct vs. relay* — and the spike is emphatic: "read off the path watcher, do not assume." A ~59 ms first-event tells you nothing about whether a hole was punched.
2. **Force or isolate relay.** There is no mode that disables direct paths, so "relay fallback works when direct fails" and "TTFB on the relay path" cannot be measured deterministically on a NAT pair that happens to hole-punch successfully.
3. **Measure RTT / throughput.** It measures one first-event latency, not sustained RTT or ≥1 Mbit/s relay throughput (a Gate A GO threshold).
4. **Emit a structured results record.** Output is human prose; there is no JSON/CSV row to feed the memo or the `NOTES.md` table.

So Gate A needs a purpose-built **measurement** harness. It is small, and most of it is instrumentation and reporting, not new networking.

---

## 4. Owning component and where the code lives

**Decision: build a dedicated throwaway spike crate `crates/spike-nat`, mirroring `crates/spike-blobs`.** Rationale:

- It matches the Spike Plan Day 1 literal deliverable — *"`spike-nat` binary + results table"* — and the repo's established throwaway-spike pattern (`spike-blobs` is isolated in the workspace so CI proves it builds but it never touches shipping crates).
- Gate A's load-bearing question is about the **iroh substrate itself** (does hole-punching work on real NATs), which is independent of our ALPN/framing. A bare `iroh::Endpoint` echo probe is exactly what Day 1 specifies ("minimal `iroh::Endpoint` … open one bidi stream; echo bytes") and isolates the substrate assumption from our code.
- It keeps the shipping crates' dependency tree and public surface untouched, honouring "no production code changes."

The **confirmation pass** (§6.6) then re-uses the *already-shipping* `net-smoke --real` and the pipe ALPN unchanged — no new code — to show the real carrier also crosses NAT. The one small allowed touch to a shipping crate, if measured to be necessary, is an **additive, read-only path-type print** in `net-smoke`'s real-network branch (a log line, no behavioural change); if we want to avoid even that, path type for the confirmation pass is read from `RUST_LOG=iroh=debug` traces instead (§6.2). Default: **no shipping-crate edit**; the probe crate carries all measurement code.

`crates/spike-nat` layout:

```text
crates/spike-nat/
  Cargo.toml            # publish = false; iroh = "=1.0.1"; tokio; anyhow; serde/serde_json; clap or hand-rolled args
  src/main.rs           # the `nat-probe` binary (listen | dial subcommands)
  src/probe.rs          # endpoint bring-up, path-watcher classification, metric capture
  src/report.rs         # ProbeResult struct + JSON/Markdown emitters
  NOTES.md              # findings deliverable (mirrors spike-blobs/NOTES.md): env table, results, GO/NO-GO, residuals
  results/              # committed run outputs: <date>-<scenario>.json + a rolled-up results.md
```

Add `crates/spike-nat` to the workspace `members` list with the same "throwaway spike, kept in workspace so CI proves it builds" comment `spike-blobs` carries. Pin `iroh = "=1.0.1"` (the proven pin) and keep zero 0.x crates.

---

## 5. Metrics and definitions (the measurement contract)

Every scenario × direction run records exactly these fields. Definitions are fixed here so two operators produce comparable rows.

| Field | Type | Definition / how measured |
|---|---|---|
| `scenario` | string | Human label of the NAT pairing, e.g. `home-broadband ↔ lte-hotspot`. |
| `direction` | enum `AtoB` \| `BtoA` | Which endpoint dialed. **Both directions are run** (NAT symmetry is directional). |
| `nat_a`, `nat_b` | object | Per-endpoint NAT characterization (§7): `{ kind: full-cone|restricted|port-restricted|symmetric|cgnat|unknown, isp, network_type: wifi|ethernet|lte|... , observed_via }`. |
| `established` | bool | A usable bidi stream carried the first echo byte within the wait budget. |
| `path_type` | enum `direct` \| `relay` \| `mixed` \| `none` | **Read from iroh's connection-type/path watcher after the path settles** (§6.2). Never inferred from latency. `mixed` = direct + relay both present (transitional); record the *settled* value after a short observation window. |
| `hole_punched` | bool | `path_type == direct` (a direct, hole-punched path was achieved). |
| `ttfb_ms` | int | Time from dial start to first application byte received over the stream. |
| `ttfb_direct_ms` | int \| null | TTFB on a run that settled `direct`. Null if that path was never achieved in this scenario. |
| `ttfb_relay_ms` | int \| null | TTFB on a run forced/observed as `relay` (§6.4). Null if relay never exercised. |
| `rtt_ms` | number | Median application-level round-trip over the stream (N echo pings), reported separately for direct and relay when both are exercised. |
| `throughput_mbit_s` | number | Sustained one-way throughput of a fixed-size echo transfer (e.g. 8 MiB), direct and relay separately. |
| `setup_time_ms` | int | Wall time from process start to `established` (includes discovery/relay resolution). |
| `relay_url` | string \| null | The relay the endpoint homed to (from endpoint/home-relay watcher), for relay runs. |
| `iroh_version`, `probe_git_sha`, `run_at_utc` | string | Provenance. `run_at_utc` supplied by the operator (do not read the clock inside deterministic code paths; this is a report field stamped at emit time). |
| `notes` | string | Free text: retries, anomalies, firewall config, VPN-off confirmation. |

A run emits one JSON object per (scenario, direction, path-mode). The rolled-up `results.md` renders the scenario × direction × path-type table the spike's Day 1 deliverable calls for.

---

## 6. Design

### 6.1 The `nat-probe` harness (substrate probe)

A single binary with two subcommands, matching `net-smoke`'s ergonomics so operators already familiar with it are at home:

```text
nat-probe listen  [--relay-only] [--json <path>]
nat-probe dial <ENDPOINT_ID> [--addr <IP:PORT>] [--relay-only] [--ping <N>] [--xfer <BYTES>] [--json <path>]
```

- `listen` — builds a minimal `iroh::Endpoint` with **n0 DNS discovery + default relay** (`presets::N0`, `RelayMode::Default`), registers a trivial echo `ProtocolHandler` on a probe ALPN (e.g. `/iroh-rooms/nat-probe/1`), prints its `EndpointId` and addr hints, and serves echo forever.
- `dial` — builds the same endpoint, dials the listener **purely by `EndpointId`** (discovery resolves the path; `--addr` optionally seeds a hint), opens one bidi stream, sends a marker byte and measures TTFB, then runs `--ping N` RTT probes and an `--xfer BYTES` throughput transfer. After the path settles it samples the connection-type watcher (§6.2), assembles a `ProbeResult`, prints it, and writes JSON if `--json` is given.
- The endpoint secret is a fresh random `SecretKey` per run (or a `--seed` for reproducibility). These are genuine iroh-authenticated identities — satisfying the security note (§8) — but carry **no room membership and exchange no room data**.

The wait budget is generous (e.g. 30 s) because real discovery + relay resolution + hole-punch can take seconds; the *measured* number is what matters, the budget only bounds a hang.

### 6.2 Path-type classification (the load-bearing instrumentation)

Direct-vs-relay MUST be read from iroh, not guessed. iroh exposes the live path/connection classification through a watcher on the endpoint (historically `Endpoint::conn_type(endpoint_id) -> Watcher<ConnectionType>` with `ConnectionType::{Direct, Relay, Mixed, None}`, plus `Endpoint::remote_info(endpoint_id)` for the resolved addrs and home relay). **Verify the exact API against the pinned `iroh = 1.0.1` source before implementing** — the crate's higher-level names have shifted across releases and `NOTES.md` already records that recon names needed reconciliation (e.g. `remote_node_id` → `endpoint_id`). Treat the watcher name as *to-confirm*, not settled.

Classification procedure:

1. After `established`, subscribe to the connection-type watcher for the remote `EndpointId`.
2. Observe for a short settle window (e.g. 3–5 s) — hole-punching often starts on relay and *upgrades* to direct, so an early sample reads `relay`/`mixed` and the settled sample reads `direct`. Record the **settled** value and also note the initial value (the upgrade latency is itself interesting for the memo).
3. Record `path_type` = settled `ConnectionType`. `hole_punched = (settled == Direct)`.

**Fallback if the watcher API is unavailable/awkward on 1.0.1:** run with `RUST_LOG=iroh=debug,iroh_net=debug` and parse the path/holepunch log lines, *and* cross-check against `Endpoint::remote_info` addr set (a direct path shows a peer socket addr; relay-only shows only the relay URL). The watcher is preferred; the log path is the documented backup so classification is never blocked on one API detail.

### 6.3 Metric capture

- **TTFB** — `Instant` at dial start → first echo byte read. (Deterministic, not clock-derived; `Instant` is monotonic and allowed.)
- **RTT** — `--ping N` application round-trips (write 1 byte, read 1 byte), report median + p90; run once per settled path type.
- **Throughput** — `--xfer BYTES` (default 8 MiB) one-way, timed; `bytes / seconds` → Mbit/s. Run on direct and on relay separately.
- **Setup time** — process start → `established`.
- **Relay URL / home relay** — from the endpoint/home-relay watcher for relay runs.

### 6.4 Forced-relay mode (confirming fallback)

Acceptance requires "relay fallback confirmed to work when direct hole-punching fails" **and** "TTFB measured for the relay path." On a NAT pair that hole-punches successfully we still need a deterministic relay measurement, so:

- `--relay-only` builds the endpoint with **direct paths disabled / relay forced** (the iroh knob that suppresses direct UDP path discovery so traffic is pinned to the relay — verify the exact config on 1.0.1: a relay-forced `RelayMode` and/or disabling direct addr discovery). Both endpoints must run `--relay-only` for a clean relay-only measurement.
- This gives a *guaranteed* relay path to measure `ttfb_relay_ms`, `rtt` (relay), and `throughput` (relay), independent of whether the natural path would have gone direct.
- **Additionally**, in ≥1 scenario, capture a *natural* relay fallback: a likely-symmetric/CGNAT pair (§7) where direct hole-punching genuinely fails and iroh falls back on its own — the settled `path_type` reads `relay` without `--relay-only`. This is the real-world confirmation; `--relay-only` is the controlled one. Record both.

### 6.5 Structured results record — the Gate-E-memo artifact

`ProbeResult` (serde) serializes to one JSON object per run (schema = the §5 field table). The harness also renders/updates:

- `crates/spike-nat/results/<run_at_utc-date>-<scenario>-<direction>[-relay].json` — one per run, committed.
- `crates/spike-nat/results/results.md` — the rolled-up scenario × direction × path-type × TTFB × RTT × throughput × setup-time table.
- The same table, pasted into **`crates/iroh-rooms-net/NOTES.md`** replacing the current `_(pending real-network rig)_` placeholder row under *"Gate A (real-network)"*.
- A short **Gate A findings block** (GO/NO-GO verdict + the two-decision implication for open decision #12) suitable for lifting verbatim into the Gate E memo (#15).

Emitting a stable JSON schema means the memo/table can be regenerated deterministically and a later re-run is directly comparable.

### 6.6 Confirmation pass over the real shipping carrier

The substrate probe answers "does iroh hole-punch." We also confirm the **product's actual transport** crosses NAT, using landed code unchanged:

1. **Event ALPN** — `net-smoke listen --real` on host A; `net-smoke dial <ENDPOINT_ID> --real` on host B (and reversed). Record establishment + time-to-first-*event* (signed genesis over `/iroh-rooms/event/1`). Path type via the §6.2 watcher/log method. This closes the IR-0005 Gate-A residual for the event plane.
2. **Pipe ALPN** — bring up the pipe path across the same two hosts (`iroh-rooms pipe expose` on one, `pipe connect` on the other, per IR-0010) with a trivial loopback echo service, and confirm a byte crosses `/iroh-rooms/pipe/1` over the real NAT. This closes the IR-0010 Gate-A residual (`crates/iroh-rooms-net/NOTES.md` "Gate A for the pipe ALPN is still owed").

The confirmation pass records the same §5 metrics where meaningful (establishment, path type, TTFB) but need not re-measure throughput/RTT if the substrate probe already characterized the identical pair — cross-reference instead.

### 6.7 NAT-type characterization

For each endpoint/environment, capture enough to name the NAT class and to justify "≥1 likely-symmetric" coverage:

- **Network type & ISP** — Wi-Fi/Ethernet/LTE; carrier; whether CGNAT is likely (mobile/LTE and many residential ISPs are).
- **Observed NAT behavior** — derive from the run itself (did direct succeed?) and, optionally, an external STUN-based NAT-type check or iroh's own net-report/relay diagnostics if exposed. Record `observed_via` = how the class was determined (`stun`, `iroh-net-report`, `inferred-from-holepunch-result`, `operator-knowledge`).
- **Explicit environment hygiene** — record that the two endpoints are on **different networks, not the same LAN/Wi-Fi, with no VPN bridge** (the spike's hard setup constraint). A VPN or common LAN silently converts the test into a LAN demo.

---

## 7. Measurement matrix

Minimum to satisfy the gate (the acceptance floor is "≥2 distinct real-NAT environments"; more is better for a reliability estimate):

| # | Scenario | Endpoint A | Endpoint B | Purpose |
|---|----------|------------|------------|---------|
| S1 | Broadband ↔ mobile hotspot | home/office broadband (Wi-Fi/Ethernet) | phone LTE hotspot (likely CGNAT/symmetric) | The likely-symmetric case; expect natural relay fallback, want relay usable. |
| S2 | Broadband ↔ second broadband | household/office A | different household / coffee-shop / cloud VM B | The likely-hole-punchable case; expect ≥1 direct path (satisfies "direct in ≥1 non-symmetric scenario"). |
| S3 *(optional, strengthens reliability estimate)* | Repeat S1/S2 | — | — | Repeat runs to estimate hole-punch success rate (spike Day 1 "repeat to estimate reliability"). |

For **each** scenario run **both directions** (A→B and B→A) and, per §6.4, at least one `--relay-only` pass plus the natural pass. That yields, per scenario: `{AtoB, BtoA} × {natural, relay-only}` = 4 rows minimum.

**Reliability estimate:** repeat the natural direct-attempt runs K times (e.g. 5–10) per scenario and report hole-punch success rate = (direct settles) / (attempts). One-shot success is not a reliability number; the memo should state the sample size.

---

## 8. Security and privacy

- The probe uses **iroh-authenticated endpoint identities** (Ed25519 `SecretKey` → `EndpointId`), exactly as the product does. Every hop (direct or relay) is QUIC/TLS between authenticated endpoints; relays forward only ciphertext.
- **No room data is exchanged.** The substrate probe echoes synthetic bytes on a dedicated `/iroh-rooms/nat-probe/1` ALPN with no `SyncEngine`, no membership, no events. The confirmation pass (§6.6) exchanges only a room *genesis* and a trivial pipe echo created for the test — no real room content.
- **Do not conflate relay with plaintext exposure.** Per `NOTES.md` "Confidentiality": the Gate-A notes MUST record path type but MUST NOT imply relay = plaintext. State the confidentiality property explicitly in the findings.
- Committed results/JSON contain `EndpointId`s (public keys), relay URLs, ISP/network-type labels, and timing — no secrets. Do **not** commit socket addresses that reveal a home IP if that is a privacy concern; the `remote_info` addr set may be redacted in the committed artifact (keep it in the operator's private log if needed for debugging), noting the redaction.

---

## 9. Gate A GO/NO-GO rubric (from `PHASE-0-SPIKE.md` Day 1)

Evaluate the collected results against the spike's hard thresholds:

**GO — all of:**
- Connection established **both directions within ≤10 s** in **every** scenario, via *at least* relay fallback.
- A **direct hole-punched path** achieved in **≥1 non-symmetric scenario** (S2).
- Relay throughput usable for chat/control: **≥1 Mbit/s** and **RTT ≤ ~300 ms** over relay.

**NO-GO — any of:**
- Any scenario with **no path at all** (neither direct nor relay).
- Unusable relay latency/throughput (below the thresholds).

**On NO-GO:** stop and escalate before further reliance on the substrate assumption. Escalation options (spike Day 1 / Residual #12): evaluate a **self-hosted relay**, reconsider discovery config, or flag the substrate assumption as broken and force the relay-infrastructure decision the spike could not pre-make. Record the NO-GO and the chosen escalation as a residual risk (§11) and surface it to the Gate E memo (#15) — a red Gate A is a hard input to the MVP go/no-go.

---

## 10. Implementation steps (executable by another engineer/agent)

1. **Confirm the iroh 1.0.1 path/relay API surface (do this first, it de-risks everything).**
   - Locate the pinned `iroh 1.0.1` source; confirm the connection-type watcher (name, `ConnectionType` variants), `remote_info`/addr accessor, home-relay watcher, and the relay-force / direct-disable config knobs. Record the confirmed names in `spike-nat/NOTES.md` (mirroring how IR-0005 `NOTES.md` reconciled the accept-gate API).
   - If the watcher is absent/awkward, confirm the `RUST_LOG=iroh=debug` log-line fallback and the `remote_info` addr-set cross-check (§6.2).
2. **Scaffold `crates/spike-nat`.** Add to workspace `members` with the throwaway-spike comment. `Cargo.toml`: `publish = false`, `iroh = "=1.0.1"`, `tokio` (`rt-multi-thread`, `macros`, `time`, `net`, `sync`), `anyhow`, `serde`/`serde_json`, args parsing. Zero 0.x crates. Inherit workspace lints (`unsafe_code = "forbid"`).
3. **Implement `src/probe.rs`.** Endpoint bring-up (`presets::N0`, default relay; `--relay-only` variant), the `/iroh-rooms/nat-probe/1` echo `ProtocolHandler`, dial-by-`EndpointId`, one bidi stream, and the metric capture (TTFB via monotonic `Instant`, `--ping` RTT, `--xfer` throughput). Implement §6.2 path-type settle-and-sample.
4. **Implement `src/report.rs`.** `ProbeResult` serde struct = the §5 field table; JSON emitter (`--json <path>`); `results.md` table renderer.
5. **Implement `src/main.rs`.** `listen` / `dial` subcommands and flags (`--relay-only`, `--addr`, `--ping`, `--xfer`, `--json`, `--seed`). Human-readable summary on stdout plus JSON on `--json`.
6. **Offline self-check (the only CI-runnable part).** A loopback/relay-disabled test that proves the harness builds, dials, echoes, and emits a well-formed `ProbeResult` (path_type will read `direct` on loopback — assert *shape*, not traversal). Mark clearly in `NOTES.md`: **"loopback self-check is NOT Gate A."** Wire into `scripts/verify.sh` coverage by virtue of being a workspace member.
7. **Write the runbook** in `spike-nat/NOTES.md` (§ below) so a second operator can reproduce without reading this spec.
8. **Execute the matrix (§7)** on real hardware across ≥2 environments incl. ≥1 likely-symmetric, both directions, natural + `--relay-only`, with K repeats for the reliability estimate. Commit the per-run JSON and the rolled-up `results.md`.
9. **Run the confirmation pass (§6.6)** with `net-smoke --real` (event ALPN) and the pipe ALPN across the same hosts. Record establishment + path type + TTFB.
10. **Update `crates/iroh-rooms-net/NOTES.md`** — replace the `_(pending real-network rig)_` placeholder row under "Gate A (real-network)" with the measured table; flip "STATUS: NOT YET RUN" to the dated result; update the IR-0010 pipe "Gate A owed" line if the confirmation pass covers it.
11. **Evaluate GO/NO-GO (§9)** and write the Gate A findings block (verdict + implication for Residual #12) in a form liftable into the Gate E memo (#15).
12. **Log residual risks (§11)** for any failed assumption (or, on a clean GO, record that decision #12's Gate-A dependency is discharged with the measured evidence).

### Runbook (goes in `spike-nat/NOTES.md`)

```text
Preconditions: two machines on DIFFERENT real networks. No shared LAN/Wi-Fi, no VPN bridge.
  Characterize each: network type, ISP, likely NAT class (§6.7).

Substrate probe (per scenario, do both directions):
  Host A:  nat-probe listen                          # prints ENDPOINT_ID + addr hints
  Host B:  nat-probe dial <A_ENDPOINT_ID> --ping 20 --xfer 8388608 --json results/<date>-<scen>-BtoA.json
  Reverse roles for AtoB.
  Repeat with --relay-only on BOTH ends for the forced-relay measurement.
  Repeat the natural run K times for the hole-punch reliability estimate.

Confirmation pass (real carrier):
  Event ALPN:  net-smoke listen --real   |   net-smoke dial <ID> --real     (both directions)
  Pipe  ALPN:  iroh-rooms pipe expose ... |  iroh-rooms pipe connect ...     (per IR-0010)

Record: establishment y/n, setup_time, path_type (from watcher/log — DO NOT ASSUME),
  ttfb (direct + relay), rtt (direct + relay), throughput (direct + relay), relay_url.
Evaluate against the Gate A GO/NO-GO rubric (§9). File residuals for any failure.
```

---

## 11. Acceptance criteria

Mapped to the issue's acceptance criteria:

- [ ] **Direct-connection success/failure is recorded across ≥2 distinct real-NAT environments** — the §7 matrix executed over ≥2 environments (incl. ≥1 likely-symmetric), both directions, with per-run JSON committed under `spike-nat/results/` and a rolled-up table.
- [ ] **Relay fallback is confirmed to work when direct hole-punching fails** — captured both as a controlled `--relay-only` run and as ≥1 natural fallback on a symmetric/CGNAT pair; the settled `path_type = relay` is read from iroh's watcher/logs, not assumed.
- [ ] **Time-to-first-byte is measured for both direct and relay paths** — `ttfb_direct_ms` and `ttfb_relay_ms` recorded; `--relay-only` guarantees a relay measurement even when the natural path goes direct.
- [ ] **Results captured in a form that feeds the Gate E memo (#15)** — structured `ProbeResult` JSON + `results.md` table + a Gate A findings block; the `crates/iroh-rooms-net/NOTES.md` Gate-A placeholder replaced with the measured table.
- [ ] **Any failing connectivity assumption is logged as a residual risk** — a NO-GO (or any degraded finding) recorded as a residual with its escalation, and surfaced to #15; on a clean GO, Residual #12's Gate-A dependency is explicitly marked discharged.
- [ ] **(Security note)** the probe uses iroh-authenticated identities and exchanges no room data; the findings state the confidentiality property (relay forwards ciphertext, ≠ plaintext).
- [ ] **The measurement harness builds and its loopback self-check passes in CI** (`scripts/verify.sh`), clearly labeled as *not* Gate A.

---

## 12. Risks

1. **The test is inherently manual and hardware-dependent.** CI cannot prove NAT traversal; the gate closes only when a human runs it on two real networks. *Mitigation:* commit the tool + runbook + results so the run is reproducible and re-runnable, and gate the *tooling* in CI even though the *measurement* is manual.
2. **Path type mis-classified (the load-bearing risk).** If we infer direct-vs-relay from latency instead of iroh's watcher, a relayed path with a nearby relay can masquerade as direct and produce a false GO. *Mitigation:* §6.2 mandates reading the watcher/logs; the settle-window handles relay→direct upgrade; cross-check against `remote_info` addrs.
3. **iroh 1.0.1 API drift.** The exact watcher / relay-force names may differ from recon. *Mitigation:* Step 1 confirms them against source before building; the `RUST_LOG` fallback keeps classification unblocked. Consistent with how IR-0005 reconciled the accept-gate API.
4. **Environment hygiene failure — the LAN/VPN illusion.** A shared LAN, common Wi-Fi, or an unnoticed VPN silently turns Gate A into a LAN demo that always "passes." *Mitigation:* the runbook records network type/ISP per endpoint and requires explicit "different networks, no VPN" confirmation; the spike calls this out as the thing a LAN demo lies about.
5. **Relay-force knob doesn't fully suppress direct paths on 1.0.1.** If `--relay-only` still opportunistically upgrades to direct, the "relay TTFB" is contaminated. *Mitigation:* verify the direct-disable config in Step 1; if unavailable, rely on the natural-fallback symmetric-NAT case for the relay measurement and document the limitation.
6. **Reliability estimate from too few samples.** A single successful hole-punch is not a reliability figure. *Mitigation:* K repeats per scenario; the memo states sample size and the estimated rate, not a binary.
7. **NO-GO forces an out-of-spike decision.** A red Gate A (no usable path on a real symmetric pair) forces the relay-infrastructure decision the spike explicitly cannot pre-make (Residual #12). *Mitigation:* §9 pre-defines the escalation; a NO-GO is a first-class, documented output feeding #15, not a blocked task.
8. **Privacy leakage in committed artifacts.** `remote_info` addrs / relay URLs can reveal a home IP. *Mitigation:* §8 redaction guidance for the committed JSON.

---

## 13. Assumptions

1. Two physical machines on two genuinely different real NATs are available to the operator (home/office broadband + mobile hotspot, or a cloud VM as one endpoint). This is the one prerequisite the spike names and IR-0005 `NOTES.md` records as previously missing ("no two-NAT rig during this phase").
2. The default n0 discovery + relay stack (`dns.iroh.link`, n0 relays) is reachable from both environments and is the stack under test — self-hosted relay is out of scope unless a NO-GO triggers escalation.
3. `iroh = 1.0.1` (the proven pin) exposes a usable connection-type/path watcher and a relay-force / direct-disable configuration; if a specific name differs, Step 1 reconciles it and the `RUST_LOG` fallback covers the gap.
4. A throwaway `crates/spike-nat` (mirroring `spike-blobs`) is the accepted home; the shipping crates remain unmodified except, at most, an additive read-only log line in `net-smoke`'s real branch (avoidable via `RUST_LOG`).
5. Gate A concerns n=2 traversal only; event-plane fan-out at n≤5 is covered elsewhere (IR-0107, loopback) and is not re-measured here.
6. Monotonic `Instant` timing and operator-supplied UTC stamps are acceptable provenance; no wall-clock is used in any decision path (consistent with the protocol's clock discipline).

## 14. Open questions

1. **Exact 1.0.1 watcher/relay-force API names** — resolved in Step 1 against source; flagged now because recon names have drifted before.
2. **Should the confirmation pass (§6.6) also exercise the full lifecycle** (create → invite → join → message → file → pipe across the NAT), or is that Gate E's Day-10 integration smoke test (#15) reusing this rig? *Proposed:* keep IR-0012 to connectivity + event/pipe ALPN reachability; the full lifecycle across real NATs belongs to the Gate E integration run, which reuses the Day-1 rig. Confirm the boundary with the #15 owner.
3. **Reliability-estimate sample size (K).** §7 suggests 5–10 repeats; the statistically-meaningful K for a hole-punch rate is a judgment call — state the chosen K and its limitation in the memo rather than over-claiming.
4. **Committed-artifact redaction policy** — whether to redact `remote_info` socket addrs (home IPs) from the committed JSON; §8 recommends redaction, but confirm against the project's privacy stance.
5. **Cloud VM as a substitute endpoint** — a cloud VM often has a public IP / full-cone-ish NAT, which is *easier* than a real home NAT and can inflate the direct-success rate. If a VM is used for one endpoint, ensure the *other* is a real residential/mobile NAT so the pairing still exercises traversal; note it in `scenario`.
```
