# Measure Real-Network Gossip-Overlay Connectivity & Delivery (Gate A analog for the overlay) — #214

- **Issue:** #214 — [OVERLAY] Measure real-network gossip-overlay connectivity & delivery (Gate A analog for the overlay)
- **Labels:** `type/test` `area/transport` `priority/p1` `risk/medium`
- **Blocks:** #171 (Phase C `large_rooms` epic), #192 (gossip overlay — Step 7 / hub overload).
- **Inherits:** #43 / IR-0012 (`specs/measure-real-nat-hole-punching-connectivity.md`) — the Gate A rig, the IPv6/VPN trap discipline, and the path-watcher classification procedure.
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
- **Not** cellular. The maintainer fleet (assessed 2026-08-02) has **no cellular host**. **WAIVER-PBETA-0001** (forced-relay throughput) therefore stays a **permanent documented scope limit** (the relay is fine at 3.3–4.1 Mbit/s; the mobile uplink is the bottleneck), not a retirement target. Stated here so it is not re-litigated under this issue.
- **Not** a shipping-crate change. Measurement harness only; reuse `spike-nat` / `net-smoke` where possible. Any transport defect surfaced is filed as a new issue, not fixed here.

---

## 3. Fleet and topology (maintainer, 2026-08-02)

Three hosts: **datacenter/VPS**, **home broadband**, **CGNAT**. No Apple Silicon (Intel Mac only → covers `x86_64-apple-darwin`; `aarch64-apple-darwin` remains untested — macOS qualification is tracked separately).

| # | Pairing | Endpoint A | Endpoint B | Purpose |
|---|---|---|---|---|
| O1 | home broadband ↔ CGNAT | home broadband | CGNAT | Hardest overlay hole-punch cell; same topology as WAIVER-0002's missing BtoA row. **Primary.** |
| O2 | home broadband ↔ VPS | home broadband | datacenter/VPS | Relay-characterized baseline (VPS has global IPv6 — see §4). |
| O3 *(optional)* | CGNAT ↔ VPS | CGNAT | datacenter/VPS | Strengthens the reliability estimate. |

For each pairing run **both directions** (NAT symmetry is directional). The remaining host joins the gossip topic to exercise fan-out over NAT at n=3.

---

## 4. Traps that SILENTLY SELF-PASS (read before any run)

A run that ignores these produces **false PASSING** evidence. Inherited from the GA host-fleet assessment and `crates/spike-nat/NOTES.md`:

1. **IPv6 trap.** The VPS (and likely home broadband) has native global IPv6 → no NAT to traverse → "direct achieved" is guaranteed and meaningless. **Any direct-attempt run MUST NAT or disable IPv6 on the VPS/home leg**, or the row self-certifies. Record the IPv6 state (enabled/disabled/NAT'd) per endpoint in every committed result.
2. **VPN trap.** A shared LAN or VPN bridge (incl. Tailscale/WireGuard reach for SSH'ing the CGNAT host) silently converts the run into a LAN demo that always "passes," and the tool cannot detect it. **No mesh VPN joining two probe hosts.** Safe remote-access pattern: reverse-SSH to a **third** jump host carrying control only.
3. **Two relay-only seams, easily confused.** `nat-probe --relay-only` is a *runtime flag* on the probe. `relay-only-test` is a *cargo feature* that forces relay in the real shipping carrier (`crates/iroh-rooms-net/Cargo.toml`); it is **non-additive and `--all-features` turns it ON**. Never combine the feature with a real-network non-ignored test.

---

## 5. Measurement contract

Every (pairing, direction) run records the Gate A fields (#43 §5 — `scenario`, `direction`, `established`, `path_type`, `hole_punched`, `ttfb_ms`, `rtt_ms`, `throughput_mbit_s`, `setup_time_ms`, `relay_url`, provenance, `notes`) **plus** the overlay delivery rubric:

| Field | Definition |
|---|---|
| `overlay_installed` | bool — gossip topic subscribed and ≥1 mesh neighbor present over the `/iroh-rooms/event/1` ALPN. |
| `events_sent` / `events_received` | per-node counts; `delivery = received / sent` over the settled window. |
| `connectedness` | (established overlay links) / (expected) at settle. |
| `cascade` | bool — any Step 7 cascade trigger fired (queue saturation, reconnect churn, delivery/connectedness under threshold). |
| `path_type` | per link, read from iroh's connection-type watcher after settle (§6.2 of #43). **Never inferred from latency.** |

---

## 6. Acceptance criterion (the missing gate)

**PASS — all of:**
- Overlay installed AND ≥1 event delivered across the NAT in **both directions** in **every** pairing (O1 mandatory; O2 mandatory; O3 if run), via at least relay fallback, within **≤30 s** setup *(provisional — reflects `actor.subscribe` + mesh formation, slower than bare Gate A's 10 s; confirm against the first real run and pin the value here).*
- `delivery = 100%` at the fleet N (at n≤3 a drop is a defect, not a network artifact).
- `connectedness > 95%` AND **no cascade** at 1 event/s (the Step 7 thresholds, at the available N).
- `path_type` read from iroh's watcher on every link; the IPv6 trap (§4.1) explicitly guarded per endpoint.

**FAIL — any of:** no overlay path in any mandatory pairing; `delivery < 100%` at n≤3; a cascade at 1 event/s; `path_type` assumed rather than read from the watcher.

**On FAIL:** Phase C stays held; file a residual risk and surface it to #192. Do **not** re-run until the §4 trap checklist is re-confirmed (a FAIL is more often a trap than a defect).

---

## 7. Procedure, owner, artifact

- **Owner:** TBD (assign; this unblocks #192 Phase C).
- **Procedure:** extend `spike-nat` (or a new `spike-overlay`, or a `--overlay` mode on `net-smoke` / `spike-N40`'s real-network branch) to install the gossip topic over the shipping carrier and emit the §5 fields; run the §3 matrix both directions, natural + (where meaningful) `--relay-only`; commit per-run JSON + a rolled-up `results.md`. Reuse the Gate A runbook shape (#43 §10).
- **Artifact:**
  - `crates/spike-nat/results/<run_at_utc-date>-overlay-<pairing>-<direction>[-relay].json` — one per run, committed.
  - `crates/spike-nat/results/results-overlay.md` — rolled-up pairing × direction × path-type × delivery × connectedness × cascade table.
  - A Gate-A-analog findings block (verdict + implication for Phase C).
  - Flip `specs/gossip-overlay-events-fan-out.md:654` from "remains owed" to the dated result on a clean PASS.

---

## 8. Open questions

1. **Harness home.** Extend `spike-nat` to carry the gossip topic, or add a `--overlay` mode to `net-smoke` / `spike-N40`'s real-network branch? Recon, not a blocker for defining the AC — resolves at procedure time.
2. **Third-node value.** Is n=3 fan-out worth the VPS IPv6-trap overhead (§4.1), or keep n=2 (home↔CGNAT) clean and treat the VPS as relay-only? The hardest cell (home↔CGNAT) is n=2 regardless.
3. **Repeats K.** Hole-punch reliability needs K runs; state K (5–10) and the sample size in the findings rather than reporting one-shot success as a rate.

---

## 9. Gating

- **Blocks:** Phase C cap raise (`MAX_ACTIVE_MEMBERS` 5 → 20/40) in #171 / #192. Phase C does not land until this issue's AC is met OR an explicit residual re-waives it.
- **Inherits:** the Gate A rig, IPv6/VPN trap discipline, and path-watcher classification from #43 / IR-0012.
- **Does NOT block:** the current rc series or the dormant overlay ship state — the overlay is OFF in shipped binaries; this issue governs the *re-enable + cap-raise* decision, not a released behavior.
