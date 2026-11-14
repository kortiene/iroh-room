# Measure Real-Network Gossip-Overlay Connectivity & Delivery (Gate A analog for the overlay) — #214

- **Issue:** #214 — [OVERLAY] Measure real-network gossip-overlay connectivity & delivery (Gate A analog for the overlay)
- **Labels:** `type/test` `area/transport` `priority/p1` `risk/medium`
- **Blocks:** #171 (Phase C `large_rooms` epic), #192 (gossip overlay — Step 7 / hub overload).
- **Inherits:** #43 / IR-0012 (`specs/measure-real-nat-hole-punching-connectivity.md`) — the Gate A rig, the IPv6/VPN trap discipline, and the path-classification procedure (`remote_info` active-addrs; iroh 1.0.1 exposes no `ConnectionType` watcher).
- **Traceability:** `specs/gossip-overlay-events-fan-out.md` Step 7 status (line 654: "Phase C remains gated on real-network overlay evidence") → this spec defines that previously-undefined evidence.
- **Kind:** measurement / test deliverable (spec only — no production code under `crates/*/src/`). The durable outputs are a results file, an updated `NOTES.md` block, and a GO/NO-GO finding.

---

## 1. Summary

Phase C (the `large_rooms` cap raise, #171 / #192) is gated on **"real-network overlay evidence"** (`specs/gossip-overlay-events-fan-out.md:654,691`) that, before this spec, was **undefined**: no threshold, procedure, owner, results artifact, or issue. Nothing can pass an undefined test. This spec defines it.

It is the **Gate A analog for the gossip overlay** — a second delivery plane installed alongside the full mesh — **not** a scale re-test. Gate A (#43 / IR-0012) proved the *full-mesh* carrier crosses NAT at n=2. This spec proves the **gossip overlay** crosses NAT and delivers events at the size the available fleet supports.

---

## 2. Goal and non-goals

### Goal

Produce reproducible, dated evidence that the gossip overlay, installed over the shipping carrier, establishes across real NATs and delivers events per the Step 7 rubric, at fleet-available N.

### Non-goals

- **Not** an N=20/40 scale test over real networks. The cascade / connectedness / delivery mechanics at scale are loopback-backed (`crates/spike-N40/results/results-gossip.md`; Step 7 AC met at N=5/10/20/40 on 2026-07-31). Real-network runs cover NAT traversal + overlay-over-NAT delivery only.
- **Not** cellular. The maintainer fleet (assessed 2026-08-02) has **no cellular host**, so this issue produces no cellular evidence. **WAIVER-PBETA-0001** (cellular forced-relay throughput) is **out of scope for this issue** but is **not** retired by it: the waiver carries its own expiry ("next Production Beta candidate or before GA", `docs/releases/v0.1.0-rc.1-production-beta-signoff.md` Current Waivers) and its own release-gate remediation ("re-measured with a larger transfer or explicitly accepted as non-blocking", `PRODUCTION-READINESS.md` §P0.2). For accuracy, the waiver records both the healthy relay refresh (4.1 / 1.3 Mbit/s `demo1↔local`) **and** the below-target cellular forced-relay legs (0.1–0.2 Mbit/s on 256 KiB samples bottlenecked by the mobile uplink, `crates/spike-nat/NOTES.md` §relay-usability).
- **Not** a shipping-crate change. Measurement harness only; reuse `spike-nat` / `net-smoke` where possible. Any transport defect surfaced is filed as a new issue, not fixed here.

---

## 3. Fleet and topology (maintainer, 2026-08-02)

Three hosts: **datacenter/VPS**, **home broadband**, **CGNAT**. No Apple Silicon (Intel Mac only → covers `x86_64-apple-darwin`; `aarch64-apple-darwin` remains untested — macOS qualification is tracked separately).

| # | Pairing | Endpoint A | Endpoint B | Purpose | Status |
|---|---|---|---|---|---|
| O1 | home broadband ↔ CGNAT | home broadband | CGNAT | Hardest overlay hole-punch cell; same topology as WAIVER-0002's missing BtoA row. | **Mandatory, n=2** |
| O2 | home broadband ↔ VPS | home broadband | datacenter/VPS | Relay-characterized baseline (VPS has global IPv6 — see §4). | **Mandatory, n=2** |
| O3 | CGNAT ↔ VPS | CGNAT | datacenter/VPS | Strengthens the reliability estimate. | Optional, n=2 |
| X1 | O1 or O2 + the third host on the gossip topic | — | — | n=3 fan-out over NAT (exercises multi-hop delivery). | **Non-gating experiment** (§8) |

For each mandatory pairing run **both directions** (NAT symmetry is directional). **Mandatory cells are n=2** (publisher + subscriber); n=3 fan-out (X1) is an explicitly **non-gating** experiment, so the gate's verdict does not depend on an undefined topology or pay the VPS IPv6-trap overhead on the mandatory cells. `fleet N` in the §6 rubric therefore = **2** for the mandatory PASS/FAIL.

---

## 4. Traps that SILENTLY SELF-PASS (read before any run)

A run that ignores these produces **false PASSING** evidence. Inherited from the GA host-fleet assessment and `crates/spike-nat/NOTES.md`:

1. **IPv6 trap.** The VPS (and likely home broadband) has native global IPv6 → no NAT to traverse → "direct achieved" is guaranteed and meaningless. **Any direct-attempt run MUST NAT or disable IPv6 on the VPS/home leg**, or the row self-certifies. Record the IPv6 state (enabled/disabled/NAT'd) per endpoint in every committed result.
2. **VPN trap.** A shared LAN or VPN bridge (incl. Tailscale/WireGuard reach for SSH'ing the CGNAT host) silently converts the run into a LAN demo that always "passes," and the tool cannot detect it. **No mesh VPN joining two probe hosts.** Safe remote-access pattern: reverse-SSH to a **third** jump host carrying control only.
3. **Two relay-only seams, easily confused.** `nat-probe --relay-only` is a *runtime flag* on the bare substrate probe only. The **shipping carrier has no runtime relay-only switch** — it forces relay only when **built** with the `relay-only-test` cargo feature (`crates/iroh-rooms-net/src/transport.rs`: `#[cfg(feature = "relay-only-test")] builder.clear_ip_transports()`), which is **non-additive and `--all-features` turns ON**. Consequence for this issue: a relay-only **overlay** run is a **separate binary build** with `--features gossip_overlay,relay-only-test` (never `--all-features`), and that build provenance (features + git sha) MUST be recorded in the result. Never combine `relay-only-test` with a real-network non-ignored test.

---

## 5. Measurement contract

Every (pairing, direction) run records the Gate A fields (#43 §5 — `scenario`, `direction`, `established`, `path_type`, `hole_punched`, `ttfb_ms`, `rtt_ms`, `throughput_mbit_s`, `setup_time_ms`, `relay_url`, provenance, `notes`) **plus** the overlay delivery rubric. Definitions are pinned to the implementations in `spike-N40` / `spike-nat` so two harnesses produce identical rows:

| Field | Definition |
|---|---|
| `overlay_installed` | bool — the gossip topic is subscribed AND ≥1 neighbor is present on **`GOSSIP_ALPN` (`/iroh-rooms/gossip/1`)** (`crates/iroh-rooms-net/src/alpn.rs`). **NOT `EVENT_ALPN`** — counting a warm-seed event-plane connection here would let the gate self-pass without forming the overlay. |
| `published_events` | global count of events published by the leg's publisher. |
| `accepted_min` | min, across recipient nodes, of accepted unique events (matches `spike-N40/src/metrics.rs::delivery_is_healthy`). |
| `delivery` | `accepted_min / published_events` — per-recipient accepted ÷ global published. **NOT** a per-node `received/sent`, which is undefined for the publisher and can exceed 100% at n>2. |
| `connectedness` | `total_connected_peer_entries / expected_connected_peer_entries`, where `expected = N * min(N-1, GOSSIP_BOOTSTRAP_SEEDS)` (matches `spike-N40/src/metrics.rs::connectedness_is_healthy` / `cluster.rs`). Count peer entries on the **gossip neighbor set**, not the event-plane warm-seed set. |
| `CascadeVerdict` | the structured verdict from `spike-N40` (NOT a single bool): the four trigger outcomes — (1) any `transport.queue.saturated`, (2) reconnects/sec >1.0 for ≥2 consecutive 5s windows, (3) connectedness <95% for ≥10s, (4) per-window delivery <95% for ≥2 consecutive 5s windows unrecovered by end — **plus** the timestamped 5s sample windows / raw saturation & reconnect counters behind it. A bare `cascade=false` with no samples is not acceptable evidence. |
| `paths` | **array**, one entry per sampled overlay link keyed by `{local_endpoint_id, remote_endpoint_id}`, each with `path_type` and `relay_url`. The Gate A schema's scalar `path_type` (`spike-nat/src/report.rs`) serves a single remote and is insufficient at n≥2, where different links can be direct/relay/mixed concurrently. |
| `path_type` (per link) | read via the **`remote_info` active-addr classification** `spike-nat` already uses (`NOTES.md` §2: iroh 1.0.1 exposes **no `ConnectionType` watcher**, so active-addr presence → `direct`/`mixed`/`relay`/`none`). Never inferred from latency. The parent Gate A spec's "watcher" language was hedged as *to-confirm*; on 1.0.1 the `remote_info` fallback **is** the procedure. |

---

## 6. Acceptance criterion (the missing gate)

**Load window** (else a one-event run cannot exercise the cascade classifier): every rate leg uses the `spike-N40` matrix defaults — `--warmup-secs 10` then `--load-secs 60` at the target rate. The cascade triggers (§5 / `spike-N40/NOTES.md` §cascade) require ≥2 consecutive 5s samples / ≥10s low connectedness, so a shorter run structurally cannot evaluate them.

**Repeats** (else one-shot success self-passes): each mandatory (pairing, direction) cell is run **K=5** times. A cell PASSES only if all K repeats pass individually; a single failure MUST be recorded with its raw sample windows and surfaced to #192, and **≥2 failures in a cell fail that cell** (and thus the gate). State the sample size in the findings.

**PASS — all of:**
- Overlay installed AND `delivery = 100%` (`accepted_min == published_events`) across the NAT in **both directions** in **every mandatory pairing** (O1, O2), via at least relay fallback.
- `connectedness > 95%` (gossip-neighbor denominator, §5) AND **no cascade trigger** fires (per the structured `CascadeVerdict`, §5) across the 10s warmup + 60s load window at 1 event/s.
- `paths` array populated with a `remote_info`-classified `path_type` for **every** overlay link; the IPv6 trap (§4.1) explicitly guarded and the per-endpoint IPv6 state recorded.
- Setup time within `T_setup` (below).

**FAIL — any of:** no overlay path in any mandatory cell; `delivery < 100%`; any cascade trigger; a link missing a `remote_info`-classified `path_type`; ≥2 failed repeats in one cell.

**Setup-time threshold (not data-dependent):** the first real O1 run is explicitly designated **CALIBRATION** and cannot itself satisfy the AC; it exists to measure `actor.subscribe` + mesh-formation overhead and pin `T_setup` = Gate A's 10s baseline + the observed overlay overhead, recorded here before any further run. Only runs **after** `T_setup` is pinned count as evidence (a first run exceeding the eventual `T_setup` therefore cannot move the threshold — it sets it).

**On FAIL:** Phase C stays held; file a residual risk and surface it to #192. Do **not** re-run until the §4 trap checklist is re-confirmed (a FAIL is more often a trap than a defect).

---

## 7. Procedure, owner, artifact

- **Owner:** **Release owner** (the maintainer) — matching the convention used by the `WAIVER-PBETA` table in `docs/releases/v0.1.0-rc.1-production-beta-signoff.md` (every waiver is "Owner: Release owner"). The same role that owns the waivers owns their retirement evidence; this issue does not leave the owner undefined.
- **Procedure:** build a dedicated overlay harness (extend `spike-nat`, or add a `--overlay` mode to `spike-N40`'s real-network branch) that installs the gossip topic over the shipping carrier and emits the §5 fields; run the §3 matrix both directions × **K=5**, natural AND (for ≥1 cell) relay-only via a **separate `--features gossip_overlay,relay-only-test` build** (§4.3); record build provenance (features + git sha) in every JSON. Commit per-run JSON + a rolled-up `results.md`. Reuse the Gate A runbook shape (#43 §10).
- **Artifact:**
  - `crates/spike-nat/results/<run_at_utc-date>-overlay-<pairing>-<direction>[-relay].json` — one per run, committed.
  - `crates/spike-nat/results/results-overlay.md` — rolled-up pairing × direction × path-type × delivery × connectedness × cascade table.
  - A Gate-A-analog findings block (verdict + implication for Phase C).
  - Flip `specs/gossip-overlay-events-fan-out.md:654` from "remains owed" to the dated result on a clean PASS.

---

## 8. Open questions

1. **Harness home (open).** Extend `spike-nat` to carry the gossip topic, or add a `--overlay` mode to `spike-N40`'s real-network branch? Recon — resolves at procedure time; not a blocker for the AC.
2. ~~Third-node value~~ — **RESOLVED:** mandatory cells are n=2 (§3); n=3 fan-out (X1) is an explicitly non-gating experiment, so the verdict does not depend on an undefined topology or pay the VPS IPv6-trap overhead on the mandatory cells.
3. ~~Repeats K~~ — **RESOLVED: K=5, all-must-pass per cell (≥2 failures fail the cell); see §6.**

---

## 9. Gating

- **Blocks:** Phase C cap raise (`MAX_ACTIVE_MEMBERS` 5 → 20/40) in #171 / #192. Phase C does not land until this issue's AC is met OR an explicit residual re-waives it.
- **Inherits:** the Gate A rig, IPv6/VPN trap discipline, and path-classification procedure (`remote_info` active-addrs — iroh 1.0.1 exposes no `ConnectionType` watcher) from #43 / IR-0012.
- **Does NOT block:** the current rc series or the dormant overlay ship state — the overlay is OFF in shipped binaries; this issue governs the *re-enable + cap-raise* decision, not a released behavior.
