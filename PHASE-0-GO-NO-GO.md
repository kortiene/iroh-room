# Phase 0 Go/No-Go Memo

| | |
|---|---|
| **Date** | 2026-07-02 |
| **Issue** | #15 — [IR-0011] Write Phase 0 go/no-go memo |
| **Scope** | ≤5-person private rooms, single device per identity, no key rotation |
| **Closes** | [`PHASE-0-SPIKE.md`](PHASE-0-SPIKE.md) Day 10 deliverable ("a 1-page Phase-0 findings memo: gate results, the two decisions with rationale, pinned versions, MVP GO/NO-GO recommendation") |
| **Traceability** | [`PRD.v0.3.md`](PRD.v0.3.md) §18 Key Risks, §19 Roadmap ("Phase 0 - Technical Spike") |

---

## 1. TL;DR / Recommendation

**CONDITIONAL GO.** Gates B, C, and D are green with measured or CI-reproducible
evidence; the Day 8 (Blob ACL) and Day 9 (Live Pipe) soft gates are green at
loopback. **Gate A (real-network NAT hole-punching) is now MEASURED across BOTH
required environments** (2026-07-03/04, 37 committed runs): **S1** home-NAT ↔
Hetzner public server (non-symmetric) and **S2** an iPhone cellular Personal
Hotspot (carrier CGNAT — the likely-symmetric environment) ↔ {cloud, home-NAT}.
Establishment passes both directions in every scenario (incl. inbound to the
CGNAT client), a direct hole-punched path is Active on every established run in
both environments (native IPv6 throughout, plus a punched IPv4 path even between
the cellular CGNAT and the home NAT), and the real shipping carrier crossed both
NATs (S1 event + pipe ALPN; S2 event ALPN from cellular). The direct-path result
is corroborated on S1 by the issue-#43 SDK-daemon run; nat-probe *labels* these
runs `mixed` only because iroh 1.0.1 keeps the relay addr Active as warm standby
with no ConnectionType watcher (unchanged at `--settle 30`), not a punch failure.
What keeps Gate E CONDITIONAL rather than GO is now narrow and non-connectivity:
forced-relay **throughput** over the constrained cellular uplink measured
0.1–0.2 Mbit/s on small 256 KiB samples (below the ≥1 Mbit/s target; natural S2
sessions used the healthy direct path), so a larger-sample cellular relay
re-measure — plus the one un-run home-NAT→CGNAT reverse leg — remain owed.

**ADR-1 (full-mesh direct QUIC) — confirmed by measurement.** **ADR-2
(hand-rolled SQLite signed log + bounded recent-sync) — confirmed**, with
iroh-docs parked for Phase 5.

MVP build work (Phase 1A/1B) may proceed on this foundation. The remaining exit
condition before declaring the substrate assumption fully proven: execute the
likely-symmetric scenario of the `spike-nat` runbook and commit the results.
See §7 for the full disposition and the NO-GO escalation branch.

---

## 2. Gate results (A–E)

| Gate | Scope | Status | Evidence |
|---|---|---|---|
| **A** | Real-network NAT hole-punching (Day 1) | **MEASURED 2026-07-03/04, both environments — CONDITIONAL GO** | 37 committed runs (`crates/spike-nat/results/`): S1 home-NAT↔cloud + S2 cellular-CGNAT↔{cloud, home-NAT}. Establishment + direct hole-punch + relay reachability PASS both environments/directions (incl. inbound-to-CGNAT); carrier confirmation ✓ (S1 event+pipe, S2 event ALPN); direct corroborated by #43 SDK-daemon (`mixed` = relay warm-standby label, unchanged at `--settle 30`); residual: cellular forced-relay throughput 0.1–0.2 Mbit/s on small samples (< 1 Mbit/s target) + home-NAT→CGNAT reverse leg |
| **B** | Event Plane byte-level correctness (Days 2–3) | **GO (measured, CI-reproducible)** | `iroh-rooms-core` golden vectors + full `PHASE-0-SPIKE.md` §8 reject/flag taxonomy + strict-CBOR property tests |
| **C** | The two decisions D1+D2, and recent-sync converges on ≥1 path (Day 5) | **GO** | D1 measured (`spike-transport`); D2 hand-roll built and converges (`iroh-rooms-core/src/sync`); see caveat below |
| **D** | Sync convergence hardening — arrival-order-independent, bounded (Day 6) | **GO (measured in-sim/loopback)** | `iroh-rooms-core` `tests/sync_convergence.rs` + `tests/sync_restart.rs` |
| **E** | Integration smoke test across real NATs + MVP go/no-go (Day 10) | **CONDITIONAL** | All planes green at loopback (e2e/join/message/file/pipe tests); "across real NATs" clause blocked on Gate A |
| Day 8 (soft) | Blob Plane ACL | **GO** | `spike-blobs` 49 tests (38 unit + 11 integration), iroh-blobs 0.103.0 |
| Day 9 (soft) | Live Pipe Plane | **GO (loopback + real-NAT pipe confirmation, 2026-07-03)** | `iroh-rooms-net/src/pipe/`, `tests/pipe_e2e.rs` (P1–P6); an authenticated pipe carried HTTP across a real NAT in the Gate A confirmation pass |

### Gate A — MEASURED 2026-07-03/04, both environments — CONDITIONAL GO

Two real NAT environments were measured (37 committed JSONs): **S1** home-broadband
(Spectrum home-router NAT, wifi) ↔ Hetzner public server behind a stateful ufw
INPUT-DROP firewall (2026-07-03, 18 runs + 5 `--settle 30` #43 reconciliation
runs); **S2** an iPhone cellular Personal Hotspot (carrier CGNAT — the
likely-symmetric environment) ↔ {Hetzner public server; home-broadband NAT}
(2026-07-04, 14 runs). Different real networks, no VPN bridge (Tailscale off),
both directions × {natural, relay-only}.
**A green loopback run is NOT Gate A** — CI still only proves the tool works;
these numbers came from real networks. Against the rubric (establish both
directions ≤10 s in every scenario; a direct hole-punched path in ≥1
non-symmetric scenario; relay usable ≥1 Mbit/s and RTT ≤ ~300 ms):

- **Establishment: PASS in both environments** — every natural run established
  both directions in ≤2.7 s (S1 TTFB 0.6–1.7 s; S2/cellular TTFB 0.4–1.7 s),
  **including inbound to the CGNAT client** (the cloud peer dialed the Mac behind
  carrier CGNAT and established every run). The ten S1 8 MiB-transfer runs record
  `established=false` as a harness artifact: the bulk transfer exceeded the
  probe's fixed 30 s per-op budget; connect/TTFB/RTT succeeded in every paired
  supplement.
- **Direct hole-punch: PASS** — a direct addr was ACTIVE on every established run
  in **both** environments: native IPv6 throughout, plus a punched **IPv4** path
  even between the cellular CGNAT and the home NAT. nat-probe *labels* the runs
  `mixed`, never sole-`direct`, only because iroh 1.0.1 keeps the relay addr
  Active as warm standby and exposes no ConnectionType watcher (unchanged at
  `--settle 30`); `mixed` = "direct up, relay standby", not "on relay".
  Corroborated on S1 by the #43 SDK-daemon run (traffic on the direct path).
- **Relay usability: S1 PASS, S2 caveat** — S1 controlled `--relay-only`:
  3.3 Mbit/s @ 132 ms (BtoA), 1.2 Mbit/s @ 144 ms (AtoB). S2 forward
  cellular→cloud: 1.2 Mbit/s @ 172 ms (PASS); but forced-relay legs bottlenecked
  by the cellular **uplink** measured 0.1–0.2 Mbit/s @ 113–298 ms — below the
  ≥1 Mbit/s target. These are 256 KiB slow-start-dominated samples on a forced
  worst-case path (natural S2 sessions used the healthy direct path); a
  larger-sample cellular relay re-measure is owed.
- **Carrier confirmation: PASS** — S1 event ALPN both directions (signed genesis
  in ~1.07 s; non-member rejected before any event bytes, `unknown_device`) and
  pipe ALPN (authenticated `pipe expose`/`pipe connect` carried HTTP across the
  NAT, full CLI flow). S2 event ALPN: the real transport carried a signed genesis
  from the cellular CGNAT to the cloud peer in 1.35 s.

Evidence: [`crates/spike-nat/NOTES.md`](crates/spike-nat/NOTES.md) §6 (findings
block), [`crates/spike-nat/results/results.md`](crates/spike-nat/results/results.md)
(37 committed runs), and
[`crates/iroh-rooms-net/NOTES.md`](crates/iroh-rooms-net/NOTES.md) §"Gate A
(real-network)". Both required environments are now measured; the remaining
residuals are non-connectivity — a larger-sample cellular forced-relay throughput
re-measure (small samples read below the ≥1 Mbit/s target) and the one un-run
home-NAT→CGNAT reverse leg — carried in §7.

### Gate B — GO

Golden vector reproduces CSB, `event_id`, and signature byte-exactly;
re-canonicalization stable; every `PHASE-0-SPIKE.md` §8 outcome exercised (`non_canonical_encoding`,
`id_mismatch`, `bad_signature`, `unbound_device`, `room_id_mismatch`,
`too_many_parents`, `unknown_schema_version`, `unknown_event_type`,
`invalid_content`, `not_genesis_descended`, `not_a_member`,
`insufficient_role`, plus advisory `clock_skew` and `duplicate`); the
taxonomy-completeness gate
(`crates/iroh-rooms-core/tests/conformance/taxonomy.rs`,
`every_reason_and_flag_is_covered_or_deferred`) enforces an **empty**
`DEFERRED` list — confirmed in source: `const DEFERRED: &[(&str, &str)] = &[]`.
Evidence:
[`crates/iroh-rooms-core/tests/golden_vectors.rs`](crates/iroh-rooms-core/tests/golden_vectors.rs),
[`tests/protocol_conformance.rs`](crates/iroh-rooms-core/tests/protocol_conformance.rs)
(+ [`tests/conformance/`](crates/iroh-rooms-core/tests/conformance/)),
[`tests/cbor_property.rs`](crates/iroh-rooms-core/tests/cbor_property.rs).
Attributed to IR-0002/#6 (+ follow-up #45) and IR-0003/#7.

### Gate C — GO, with one caveat

D1 (transport) is measured and ratifies ADR-1 (§3). The recent-history-sync
primitive demonstrably converges a reconnected peer on the hand-rolled path
([`tests/sync_convergence.rs`](crates/iroh-rooms-core/tests/sync_convergence.rs):
offline-peer membership backfill, byte-identical digest across 20 shuffle
seeds). **Caveat:** D2 was *not* run as a head-to-head iroh-docs-vs-hand-roll
benchmark — no `spike-sync` crate exists; the hand-roll was built directly per
ADR-2 and its convergence measured. That is consistent with Gate C's GO
criterion ("recent-history-sync primitive demonstrably works on at least one
path"), which requires one working path, not a bake-off. This memo does not
claim a measured comparison that was not performed.

### Gate D — GO

Convergence is deterministic and arrival-order-independent under shuffled
delivery and mid-stream reconnect; anti-amplification bounds (signer pre-check,
per-author parked-set cap, backfill rate-limit) hold and survive restart.
Evidence:
[`crates/iroh-rooms-core/tests/sync_convergence.rs`](crates/iroh-rooms-core/tests/sync_convergence.rs)
(reversed-causal-order, latecomer, 1000× idempotent replay, non-member flood
guard, never-windowed invariant) and
[`tests/sync_restart.rs`](crates/iroh-rooms-core/tests/sync_restart.rs) (park
survives restart, fail-closed re-armed, rate-limit not reset). These are
in-sim/loopback, not real-NAT — the real-NAT convergence proof is the Gate E/A
item.

### Gate E — CONDITIONAL

Every plane's end-to-end lifecycle is exercised at loopback
([`iroh-rooms-core/tests/e2e_lifecycle.rs`](crates/iroh-rooms-core/tests/e2e_lifecycle.rs);
`iroh-rooms-net`
[`tests/join_e2e.rs`](crates/iroh-rooms-net/tests/join_e2e.rs),
[`tests/message_e2e.rs`](crates/iroh-rooms-net/tests/message_e2e.rs),
[`tests/file_e2e.rs`](crates/iroh-rooms-net/tests/file_e2e.rs),
[`tests/pipe_e2e.rs`](crates/iroh-rooms-net/tests/pipe_e2e.rs)). The Day-10
criterion additionally requires "full lifecycle converges across real NATs,"
which cannot be asserted until Gate A runs. Hence CONDITIONAL, not GO. Blob
(soft) and Pipe (soft) gates are GO at loopback.

---

## 3. Transport decision — ADR-1

**ADR-1 is CONFIRMED (ratified) by measurement, not revised.** Full-mesh direct
QUIC over ALPN `/iroh-rooms/event/1` remains the Room Event Plane transport;
`iroh-gossip` is not adopted for the load-bearing log (parked as an optional
off-critical-path liveness/admin-tip carrier). Measured comparison from
[`crates/spike-transport/NOTES.md`](crates/spike-transport/NOTES.md) and
[`crates/spike-transport/results/results.md`](crates/spike-transport/results/results.md)
(IR-0006/#10, landed PR #69):

- **Propagation latency, N=2..5:** mesh median 16 ms vs gossip median 16–17 ms
  — statistically indistinguishable, confirming *"gossip buys nothing at
  N≤5"* (the stronger form of the claim), not *"gossip is slower."* Both
  converge to full set equality; 0 lagged events.
- **Late-join gap:** 11/11 on both backends; the difference is structural, not
  numeric — the mesh newcomer already holds an authenticated bidi link to
  every member, so backfill is one more frame; the gossip newcomer has no
  per-peer connection to attach a pull to.
- **Admission/auth:** mesh refuses an out-of-allowlist `EndpointId` **before**
  `accept_bi()`; gossip's open `TopicId` admits an interloper with no auth
  check (measured: interloper published an event a member received).
  Corroborated by `iroh-rooms-net` T2
  ([`tests/loopback.rs`](crates/iroh-rooms-net/tests/loopback.rs): reject
  decided from `remote_id()`, connection closed before `accept_bi()`, audit
  `peer.rejected cause="unknown_device"`).
- **Implementation complexity:** mesh 405 LOC / 0 added 0.x crates; gossip 299
  LOC but +1 direct 0.x crate — the ~100-line saving is a "false economy"
  (gossip has no slot for the admission gate / per-peer bookkeeping / frame
  codec you still need).
- **No measured surprise crossed the Day-4 flip trigger.**
- **Open Decision 13 resolved:** admin-tip rides the mesh
  `SyncMessage::AdminTip` control frame for MVP; gossip liveness topic
  measured (3–6 ms vs mesh 18–21 ms freshness) but **not adopted** — the gap
  is immaterial and a gossip tip is a weaker-trust unauthenticated hint. This
  decision is closed.

---

## 4. Sync substrate decision — ADR-2

**ADR-2 is CONFIRMED — hand-roll a signed append-only log in SQLite with a
bounded recent-sync pull over the ADR-1 mesh; do not adopt iroh-docs for
MVP.** Evidence:

- The hand-rolled log/sync is built and its convergence measured: SQLite
  event store (IR-0004/#8,
  [`iroh-rooms-core/src/store`](crates/iroh-rooms-core/src/store)), sans-IO
  recent-sync engine (IR-0007/#11,
  [`src/sync`](crates/iroh-rooms-core/src/sync)), membership causal fold
  (IR-0008/#12, [`src/membership`](crates/iroh-rooms-core/src/membership)),
  all landed. Convergence and arrival-order-independence proven in
  [`tests/sync_convergence.rs`](crates/iroh-rooms-core/tests/sync_convergence.rs).
- **Honest caveat (repeat from Gate C):** iroh-docs was *not* built as a
  counterfactual (no `spike-sync`). ADR-2's rationale is that docs' unique
  value (whole-history RBSR) is exactly the MVP-deferred capability, its data
  model (LWW-KV) mismatches the `prev_events` causal DAG, and it drags three
  0.x crates + redb onto the critical path. This memo confirms ADR-2 **on
  that design rationale plus a working, measured hand-roll**, and states that
  no head-to-head docs benchmark was run — this is a confirmation of the
  chosen path, not a measured refutation of docs.
- iroh-docs remains the **leading Phase-5 availability-layer candidate**
  (Residual/Open Decision #14, §6), with the `m/`+`c/` prefix-free-key
  mapping ready.

---

## 5. Pinned dependency observations

Confirmed versions actually used (re-verified against each crate's
`Cargo.toml` and `NOTES.md`, 2026-07-02 — not the raw
`PHASE-0-SPIKE.md` recon table):

| Crate | Confirmed pin | Note |
|---|---|---|
| `iroh` | `=1.0.1` | stable 1.x core; recon originally said `1.0.0`, crates.io sparse index confirmed `1.0.1` |
| `iroh-base` | `=1.0.1` | stable 1.x |
| `iroh-gossip` | `=0.101.0` | builds against iroh 1.0.1 with zero API drift; spike table was correct here |
| `iroh-blobs` | `=0.103.0` | carries `provider::events` ACL; recon's earlier `0.97` was **stale** — corrected to `0.103.0` |
| `ed25519-dalek` | `=3.0.0-rc.0` | release *candidate*; must stay in lockstep with the version iroh 1.0 pins |
| `ciborium` | `^0.2` | verified deterministic-CBOR path in `event-core` (Gate B green) |
| `blake3` | `^1` | stable |

Re-verification method: grepped `iroh`, `iroh-base`, `iroh-gossip`,
`iroh-blobs`, `ed25519-dalek`, `ciborium`, `blake3` across every
`crates/*/Cargo.toml` — all pins match the table above with no drift since
the child issues landed.

Churn-budget observation: only `iroh`/`iroh-base` carry a stability guarantee;
everything above is 0.x on a ~monthly breaking cadence. Budget **~0.5–1
dev-day per minor bump**, plus a larger one-off if `ed25519-dalek` reaches
3.0 final or `iroh-blobs` graduates to production. ADR-1/ADR-2 deliberately
keep the event critical path on stable core only (0 added 0.x crates), which
is the payoff.

---

## 6. Residual risks accepted for MVP

Rolled up from `PHASE-0-SPIKE.md` "Residual Risks & Open Decisions," each
marked accepted or descoped:

1. **Conditional convergence** (deepest) — **Accepted**; detectability via
   admin-tip + never-windowed membership sub-DAG; fail-closed on suspected
   incompleteness.
2. **Removal/leave enforcement bounded by reachability** — **Accepted**;
   tear-down-on-learn.
3. **Blob-serve-to-any-Active + no per-blob revocation** — **Accepted**;
   key-bound invites close fresh-key ban-evasion.
4. **Removed-member timeline pollution (log-valid, capability-zero)** —
   **Accepted**; UI segregation recommended, not a protocol guarantee.
5. **Timeline position attacker-influenceable** — **Accepted**; ascribe no
   trust to order.
6. **`created_at` attacker-chosen/signed** — **Accepted**; kept out of all
   convergence/authorization decisions (advisory only).
7. **`max_uses` unenforceable** — **Accepted**; key-bound single-subject
   invites.
8. **Admin key compromise/loss unrecoverable — SINGLE LARGEST residual** —
   **Accepted for MVP** with documentation + out-of-band key custody. This is
   the highest-severity accepted risk in this list: the genesis key is the
   sole immutable authority *and* the transport identity, with no
   detection/containment/recovery path in MVP.
9. **DoS from causally-incomplete junk** — **Accepted**; §4 anti-amplification
   bounds reduce but do not eliminate; limits tunable, not proven.

Open decisions deferred to MVP time (recorded as open, not blocking):

- **#10 `iroh-blobs` 0.103 (pre-production, has ACL) vs 0.35 (production, no
  ACL)** — shipping-line call deferred; the spike validated the 0.103 ACL
  path.
- **#11 Cross-version schema skew** — needs lock-step versioning or a
  forward-compat policy + shared vectors before a 2nd `schema_version` ships.
- **#12 Gate A real-NAT measurement** — **the one that gates this
  recommendation**; see §7. (D1 half discharged by IR-0006.)
- **#13 Admin-tip carrier** — **Closed** (decided: mesh `AdminTip`).
- **#14 Phase-5 sync substrate** (docs vs Meyer RBSR vs p2panda) — deferred
  by design; out of scope for this memo.

---

## 7. Failed / not-green gate → mitigation, and the MVP go/no-go

The one gate that was unmeasured, **Gate A, is now measured across BOTH required
environments (2026-07-03/04)**: `crates/spike-nat/results/` holds 37 per-run JSONs
and `crates/spike-nat/results/results.md` / `crates/iroh-rooms-net/NOTES.md`
§"Gate A" record the measured state. Across S1 (home-NAT ↔ cloud) and S2 (cellular
CGNAT ↔ {cloud, home-NAT}), a direct hole-punched path establishes on every run,
establishment passes both directions (incl. inbound-to-CGNAT), relay fallback is
reachable everywhere, and the real carrier crossed both NATs. Applying this memo's
source spec's branch logic (`specs/phase-0-go-no-go-memo.md` §6.7) with only
non-connectivity residuals open:

**→ Gate A measured (both environments): direct hole-punch + establishment +
relay reachability all GO; the only open items are a cellular relay-throughput
re-measure and one reverse leg → recommend CONDITIONAL GO (a hair from
unconditional).**

- **Rationale:** Gates B, C, D and the blob + pipe soft gates are green with
  measured or CI-reproducible evidence; ADR-1 is measured-ratified and ADR-2
  is confirmed with a working, measured hand-roll; the full lifecycle converges
  at loopback, and the substrate + carrier are now measured on two real NAT
  environments including a carrier CGNAT — with a direct hole-punched path on
  every run (corroborated on S1 by the #43 SDK-daemon; nat-probe's `mixed` label
  is the relay warm-standby classifier artifact, not a punch failure — unchanged
  at `--settle 30`). The connectivity assumption Gate A exists to test is
  discharged. What remains is a measurement-quality item, not a connectivity
  risk: forced-relay THROUGHPUT over the constrained cellular uplink read
  0.1–0.2 Mbit/s on 256 KiB slow-start-dominated samples (below the ≥1 Mbit/s
  target; natural S2 sessions used the healthy direct path).
- **Blocking exit condition (P0) — SATISFIED:** the P0 exit condition was to
  execute the `spike-nat` §4 runbook (`crates/spike-nat/NOTES.md`) on two machines
  across ≥2 real NAT environments (VPN off, ≥1 likely-symmetric/CGNAT case), both
  directions × {natural, relay-only}, and commit the per-run JSON. That is **done**
  (2026-07-03/04; 37 JSONs; `results.md` regenerated) — so the substrate assumption
  is no longer un-measured and external preview is unblocked. What remains before
  *unconditional* GO is non-blocking: re-measure S2 forced-relay throughput with a
  larger transfer (needs a `--budget` knob — the per-op budget is hardcoded 30 s,
  so a big transfer over a slow relay records `established=false`; a `nat-probe`
  follow-up), and run the one un-run home-NAT→CGNAT reverse leg.
- **Mitigation if a re-measure disappoints:** this does **not** auto-fail the
  MVP — relay fallback is the PRD §18.1 mitigation, the rubric accepts "at least
  relay fallback" for the non-direct case, relay reachability held on every S2
  run, and even a slow relay carries chat/control. A NO-GO on *all* paths (no
  direct and no usable relay) — not observed in either environment — would
  trigger the Residual #12 escalation: evaluate a self-hosted relay, reconsider
  discovery config, or flag the substrate assumption as broken.

**Recommendation (restated):** MVP build work may proceed on Gates B–D,
ADR-1/ADR-2, and the measured Gate A. Both required NAT environments are now
measured and the connectivity assumption is discharged; the only items before
this CONDITIONAL GO becomes unconditional are a larger-sample cellular
relay-throughput re-measure and the one home-NAT→CGNAT reverse leg. External
previews may proceed with the measured state disclosed (direct hole-punch +
establishment + relay reachability proven across a non-symmetric and a cellular
CGNAT environment; forced-relay throughput over the mobile uplink read below the
usability target on small samples).

The preview-cadence tracking of the above is mechanized by
[`RELEASE-READINESS.md`](RELEASE-READINESS.md) (IR-0306): its Sign-off records
Gate A's status per candidate build (P1 — measured 2026-07-03/04, both
environments; non-blocking for a *developer* preview — pointing at the same
`crates/spike-nat/results/results.md`), while `scripts/release-readiness.sh`
mechanizes the P0 test gate this memo's Gates B–E are drawn from.

---

## 8. Evidence / traceability matrix

| Issue | IR | Spec (`specs/`) | Crate / code | Measured output | Feeds |
|---|---|---|---|---|---|
| #6 (+#45) | IR-0002 | [`event-core-canonical-signed-event-model.md`](specs/event-core-canonical-signed-event-model.md), [`strict-cbor-reader-unit-property-fuzz-tests.md`](specs/strict-cbor-reader-unit-property-fuzz-tests.md) | [`iroh-rooms-core/src/event/`](crates/iroh-rooms-core/src/event/) | [`tests/golden_vectors.rs`](crates/iroh-rooms-core/tests/golden_vectors.rs), [`tests/cbor_property.rs`](crates/iroh-rooms-core/tests/cbor_property.rs) | Gate B |
| #7 | IR-0003 | [`protocol-conformance-test-vectors.md`](specs/protocol-conformance-test-vectors.md) | [`iroh-rooms-core/tests/conformance/`](crates/iroh-rooms-core/tests/conformance/) | [`tests/protocol_conformance.rs`](crates/iroh-rooms-core/tests/protocol_conformance.rs) (empty `DEFERRED` list) | Gate B |
| #8 | IR-0004 | [`sqlite-event-store-prototype.md`](specs/sqlite-event-store-prototype.md) | [`iroh-rooms-core/src/store/`](crates/iroh-rooms-core/src/store/) | [`tests/store_e2e.rs`](crates/iroh-rooms-core/tests/store_e2e.rs) | Gate C/D (ADR-2) |
| #9 | IR-0005 | [`full-mesh-quic-event-transport.md`](specs/full-mesh-quic-event-transport.md) | [`iroh-rooms-net/`](crates/iroh-rooms-net/) | [`NOTES.md`](crates/iroh-rooms-net/NOTES.md), [`tests/loopback.rs`](crates/iroh-rooms-net/tests/loopback.rs) (T1/T2 admission) | ADR-1, Gate E |
| #10 | IR-0006 | [`measure-gossip-vs-full-mesh-transport.md`](specs/measure-gossip-vs-full-mesh-transport.md) | [`spike-transport/`](crates/spike-transport/) | [`NOTES.md`](crates/spike-transport/NOTES.md), [`results/results.md`](crates/spike-transport/results/results.md) | ADR-1 (Gate C/D1) |
| #11 | IR-0007 | [`bounded-recent-sync-prototype.md`](specs/bounded-recent-sync-prototype.md) | [`iroh-rooms-core/src/sync/`](crates/iroh-rooms-core/src/sync/) | [`tests/sync_convergence.rs`](crates/iroh-rooms-core/tests/sync_convergence.rs), [`tests/sync_restart.rs`](crates/iroh-rooms-core/tests/sync_restart.rs) | ADR-2, Gate C/D |
| #12 | IR-0008 | [`membership-fold-prototype.md`](specs/membership-fold-prototype.md) | [`iroh-rooms-core/src/membership/`](crates/iroh-rooms-core/src/membership/) | [`tests/membership_fold.rs`](crates/iroh-rooms-core/tests/membership_fold.rs) | Gate D |
| #13 | IR-0009 | [`prototype-blob-acl-path.md`](specs/prototype-blob-acl-path.md) | [`spike-blobs/`](crates/spike-blobs/) | [`NOTES.md`](crates/spike-blobs/NOTES.md), [`tests/blob_acl.rs`](crates/spike-blobs/tests/blob_acl.rs) (49 tests) | Day 8 (soft) |
| #14 | IR-0010 | [`live-tcp-pipe-path.md`](specs/live-tcp-pipe-path.md) | [`iroh-rooms-net/src/pipe/`](crates/iroh-rooms-net/src/pipe/) | [`tests/pipe_e2e.rs`](crates/iroh-rooms-net/tests/pipe_e2e.rs) (P1–P6) | Day 9 (soft) |
| #43 | IR-0012 | [`measure-real-nat-hole-punching-connectivity.md`](specs/measure-real-nat-hole-punching-connectivity.md) | [`spike-nat/`](crates/spike-nat/) | [`NOTES.md`](crates/spike-nat/NOTES.md), [`results/results.md`](crates/spike-nat/results/results.md) **(pending)** | **Gate A** |

Note: IR-0007 and IR-0009 landed commits reference the IR number, not the `#`
issue number — mapping is via the spec headers.

Every path in this table was checked to exist in the repository at
2026-07-02 (this memo's authoring date). Each child issue landed via a
reviewed, CI-gated PR at the commit noted in `git log` (e.g. IR-0006/#10 at
`dd13b82`/PR #69, IR-0002/#6 at `a6408e4`/PR #68); `bash scripts/verify.sh`
(`cargo fmt --all --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `cargo test --workspace --all-targets
--all-features`) is the CI gate enforced at merge time for each — this is
the primary evidence of Gate B/C/D greenness, not a fresh re-run at memo-write
time.

---

## 9. Notes on this memo's process

- `PHASE-0-SPIKE.md` Day 10 was **not** edited to add a back-pointer to this
  memo (Open Question OQ-4 in the source spec); this memo is discoverable
  from the repo root beside `PHASE-0-SPIKE.md`, `PRD.md`, and `PRD.v0.3.md`.
- File location follows OQ-1's recommendation (root, not `docs/`).
- This memo records decisions already made (ADR-1, ADR-2) with measured
  evidence; it does not re-open them, and it does not make the Phase-5
  sync-substrate call (Residual #14) or the `iroh-blobs` 0.103-vs-0.35
  shipping-line call (Residual #10) — both remain open decisions deferred to
  MVP time.
