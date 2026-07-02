# Write Phase 0 Go/No-Go Memo — IR-0011 / #15

| | |
|---|---|
| **Issue** | #15 — [IR-0011] Write Phase 0 go/no-go memo |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/docs, type/decision, area/dx, priority/p0, risk/medium |
| **Traceability** | `PRD.v0.3.md` §18 (Key Risks), §19 (Phase 0 roadmap) · `PHASE-0-SPIKE.md` Spike Plan Days 1–10, Gates A–E, "Residual Risks & Open Decisions", "Pinned Crate Versions & Churn Budget" |
| **Dependencies** | #6 IR-0002, #7 IR-0003, #8 IR-0004, #9 IR-0005, #10 IR-0006, #11 IR-0007, #12 IR-0008, #13 IR-0009, #14 IR-0010, #43 IR-0012 (all Phase 0 child issues — all landed; see §7 evidence matrix) |
| **Status** | Planned. This spec describes the memo to write; it does **not** write it and changes no code. |
| **Type** | Docs / decision deliverable: one new Markdown memo at the repo root. No production code, no test code. |

---

## 1. Summary

Phase 0 was a de-risking spike (`PHASE-0-SPIKE.md`) that ran the Room Event Plane
and its neighbours through five hard gates (A–E) and two architectural decision
records (ADR-1 transport, ADR-2 sync substrate). Every Phase 0 child issue
(#6–#14, #43) has now **landed**, and each carries measured or tested evidence in
its crate. Spike Plan **Day 10** calls for the closing artifact: *"a 1-page
Phase-0 findings memo: gate results, the two decisions with rationale, pinned
versions, MVP GO/NO-GO recommendation."* This issue is that artifact.

This spec tells an executing engineer/agent **exactly what to write, where, and
which measured outputs to cite**, so the memo can be assembled without
re-deriving the evidence. It pre-loads the current gate verdicts, the measured
numbers, the confirmed pinned versions, and the residual-risk/open-decision list
gathered from the child-issue crates as of this spec's authoring date. The one
load-bearing subtlety the memo must handle honestly: **Gate A (real-network NAT
hole-punching) has a landed harness but has not been executed on two real hosts**,
so it is the only gate without measured evidence. The memo's recommendation logic
(§6.7) is built around that fact.

Non-goal: this is not a re-litigation of ADR-1/ADR-2 (those decisions are made and
measured); it is the evidence roll-up and the explicit MVP go/no-go call.

---

## 2. Goal and non-goals

### Goal
- Produce a single, self-contained Phase 0 go/no-go memo that satisfies all four
  acceptance criteria of #15 (§8): documents Gate A–E status, confirms or revises
  ADR-1 and ADR-2 with measured evidence, gives every not-green gate a concrete
  mitigation/descope path, and states an explicit MVP go/no-go recommendation.
- Link each claim to a measured output or transcript in the child-issue crates
  (the #15 Test Plan: *"Review all Phase 0 child issues and link measured outputs
  or transcripts"*).

### Non-goals
- No new spikes, no code, no tests, no crate changes.
- No re-opening of ADR-1/ADR-2 — the memo records their status, it does not
  re-argue them.
- Not the Phase-5 sync-substrate decision (Residual #14, deferred by design).
- Not a production threat model or a shipping-line commitment for
  `iroh-blobs` 0.103 vs 0.35 (Residual #10, deferred to MVP time — the memo only
  records it as an accepted open decision).

---

## 3. Owning artifact and placement

- **New file:** `PHASE-0-GO-NO-GO.md` at the **repo root**, sitting beside
  `PHASE-0-SPIKE.md`, `PRD.md`, and `PRD.v0.3.md`. Rationale: `PHASE-0-SPIKE.md`
  (the document that *defines* Gate E's deliverable) lives at the root, so its
  closing memo belongs there for discoverability; all other top-level planning
  docs are root-level. (If a reviewer prefers `docs/`, that is a cosmetic move —
  see Open Question OQ-1.)
- **Cross-links to add:**
  - In the new memo: a top link back to `PHASE-0-SPIKE.md` (the plan it closes
    out) and to `PRD.v0.3.md` §19.
  - Recommended (optional, one line): a pointer from `PHASE-0-SPIKE.md`'s Day 10
    section to `PHASE-0-GO-NO-GO.md`. This is the only edit to an existing file and
    is optional; if the executing agent avoids touching `PHASE-0-SPIKE.md`, note
    that in the memo instead.
- **Length:** the spike calls it a "1-page memo," but the four ACs plus the
  evidence-linking Test Plan make that unrealistic; target **2–4 pages**. Keep
  each section tight and table-driven; put detail in the linked crate `NOTES.md`
  files rather than restating it.
- **House style:** match the existing spec/doc voice — a short status/traceability
  block up top, numbered sections, tables for status roll-ups, exact relative file
  paths as links. Use `blake3:<hex>`-style precision when quoting IDs.

---

## 4. Required memo structure

The memo MUST contain the following sections in this order. §6 gives the content
and the evidence to cite for each.

0. **Header** — title, date, scope one-liner (`≤5-person private rooms, single
   device per identity, no key rotation`), links to `PHASE-0-SPIKE.md` and
   `PRD.v0.3.md` §19.
1. **TL;DR / Recommendation** — the explicit MVP go/no-go up front (AC4).
2. **Gate results (A–E)** — status table + one paragraph per gate (AC1).
3. **Transport decision (ADR-1)** — confirmed/revised with measured evidence (AC2).
4. **Sync substrate decision (ADR-2)** — confirmed/revised with measured evidence (AC2).
5. **Pinned dependency observations** — confirmed versions + churn budget + the
   one stale-recon correction.
6. **Residual risks accepted for MVP** — the §Residual list, marked accepted or
   descoped.
7. **Failed / not-green gates → mitigation or descope** (AC3).
8. **MVP go/no-go recommendation** — restated with conditions and exit criteria (AC4).
9. **Evidence matrix** — issue → spec → crate → measured output (Test Plan).

---

## 5. Evidence baseline (verify before writing)

The verdicts in §6 are what the child-issue crates showed at this spec's authoring
date (2026-07-02). Evidence — especially Gate A — can change. **Before writing,
the executing agent MUST re-confirm the load-bearing items** and update the memo if
they have moved:

1. **Gate A status.** Read `crates/spike-nat/NOTES.md` §6 and
   `crates/spike-nat/results/results.md`, and `crates/iroh-rooms-net/NOTES.md` §"Gate A".
   Determine whether the real two-host run has since been executed. Today it reads
   *"Gate A (real-network) — STATUS: NOT YET RUN"* and the results table is a
   `_(pending manual two-host run)_` placeholder — i.e. **PENDING**, no measured
   numbers. If per-run JSON now exists under `crates/spike-nat/results/`, treat
   Gate A as measured and switch the recommendation from CONDITIONAL to
   unconditional per §6.7.
2. **Pinned versions.** Confirm `iroh`/`iroh-blobs`/`iroh-gossip` versions actually
   used are still `=1.0.1` / `=0.103.0` / `=0.101.0` (grep the crate `Cargo.toml`s
   and the child `NOTES.md` version-confirmation blocks). Do not copy the
   `PHASE-0-SPIKE.md` "Pinned Crate Versions" table verbatim — parts of it were
   automated recon (e.g. it originally said `iroh 1.0.0`, `iroh-blobs 0.97`);
   the crates confirmed `1.0.1` and `0.103.0`.
3. **Gate B/C/D test greenness.** Optionally run `bash scripts/verify.sh` (the real
   CI gate — `cargo test` alone is not enough; verify.sh also runs `fmt --check`
   and `clippy -D warnings`) to confirm the tested gates are still green at HEAD, or
   at minimum note the last landed commit for each child issue (§7).

---

## 6. Section-by-section content and evidence to cite

### 6.1 TL;DR / Recommendation
Lead with the explicit call (see §6.7 for the full logic). One short paragraph +
the gate status table (or a pointer to §6.2). State the two decisions in one line
each: *ADR-1 (full-mesh direct QUIC) — confirmed by measurement; ADR-2
(hand-rolled SQLite signed log + bounded recent-sync) — confirmed, with iroh-docs
parked for Phase 5.*

### 6.2 Gate results (A–E) — AC1

Emit this table (statuses as of authoring date — re-verify Gate A per §5):

| Gate | Scope | Status | Evidence |
|---|---|---|---|
| **A** | Real-network NAT hole-punching (Day 1) | **PENDING — not yet measured** | Harness landed (`spike-nat`, `nat-probe`, IR-0012/#43); CI loopback self-check green; **real two-host run not executed**, results table empty |
| **B** | Event Plane byte-level correctness (Days 2–3) | **GO (measured, CI-reproducible)** | `iroh-rooms-core` golden vectors + full §8 reject/flag taxonomy + strict-CBOR property tests |
| **C** | The two decisions D1+D2, and recent-sync converges on ≥1 path (Day 5) | **GO** | D1 measured (`spike-transport`); D2 hand-roll built and converges (`iroh-rooms-core/src/sync`); see caveat below |
| **D** | Sync convergence hardening — arrival-order-independent, bounded (Day 6) | **GO (measured in-sim/loopback)** | `iroh-rooms-core` `tests/sync_convergence.rs` + `tests/sync_restart.rs` |
| **E** | Integration smoke test across real NATs + MVP go/no-go (Day 10) | **CONDITIONAL** | All planes green at loopback (e2e/join/message/file/pipe tests); "across real NATs" clause blocked on Gate A |
| Day 8 (soft) | Blob Plane ACL | **GO** | `spike-blobs` 49 tests (38 unit + 11 integration), iroh-blobs 0.103.0 |
| Day 9 (soft) | Live Pipe Plane | **GO (loopback)** | `iroh-rooms-net/src/pipe/`, `tests/pipe_e2e.rs` (P1–P6); real-NAT folded into pending Gate A |

Then one paragraph per gate. Required content and evidence per gate:

- **Gate A — PENDING.** State plainly: the measurement harness (`nat-probe`),
  runbook, GO/NO-GO rubric, and results schema are landed and CI-green, but *"a
  green loopback run is NOT Gate A"* — CI cannot prove NAT traversal. No
  establishment-success / TTFB / path-type / RTT / throughput numbers exist yet.
  This is the only gate without measured evidence and drives the conditional
  recommendation. Cite `crates/spike-nat/NOTES.md`,
  `crates/spike-nat/results/results.md`, and `crates/iroh-rooms-net/NOTES.md`
  §"Gate A". Link the rubric thresholds (targets, not results): establish both
  directions ≤10 s in every scenario; a direct hole-punched path in ≥1
  non-symmetric scenario; relay usable ≥1 Mbit/s and RTT ≤ ~300 ms.
- **Gate B — GO.** Golden vector reproduces CSB, `event_id`, and signature
  byte-exactly; re-canonicalization stable; every §8 outcome exercised
  (`non_canonical_encoding`, `id_mismatch`, `bad_signature`, `unbound_device`,
  `room_id_mismatch`, `too_many_parents`, `unknown_schema_version`,
  `unknown_event_type`, `invalid_content`, `not_genesis_descended`,
  `not_a_member`, `insufficient_role`, plus advisory `clock_skew` and `duplicate`);
  taxonomy-completeness gate has an **empty** `DEFERRED` list. Cite
  `crates/iroh-rooms-core/tests/golden_vectors.rs`,
  `tests/protocol_conformance.rs` (+ `tests/conformance/`), `tests/cbor_property.rs`.
  Attribute to IR-0002/#6 (+ follow-up #45) and IR-0003/#7.
- **Gate C — GO, with one caveat to state explicitly.** D1 (transport) is measured
  and ratifies ADR-1 (§6.3). The recent-history-sync primitive demonstrably
  converges a reconnected peer on the hand-rolled path
  (`tests/sync_convergence.rs`: offline-peer membership backfill, byte-identical
  digest across 20 shuffle seeds). **Caveat:** D2 was *not* run as a head-to-head
  iroh-docs-vs-hand-roll benchmark — no `spike-sync` crate exists; the hand-roll
  was built directly per ADR-2 and its convergence measured. That is consistent
  with Gate C's GO criterion ("recent-history-sync primitive demonstrably works on
  at least one path"), which requires one working path, not a bake-off. Say so;
  do not overclaim a measured comparison that was not performed.
- **Gate D — GO.** Convergence is deterministic and arrival-order-independent
  under shuffled delivery and mid-stream reconnect; anti-amplification bounds
  (signer pre-check, per-author parked-set cap, backfill rate-limit) hold and
  survive restart. Cite `crates/iroh-rooms-core/tests/sync_convergence.rs`
  (reversed-causal-order, latecomer, 1000× idempotent replay, non-member flood
  guard, never-windowed invariant) and `tests/sync_restart.rs` (park survives
  restart, fail-closed re-armed, rate-limit not reset). Note these are
  in-sim/loopback, not real-NAT — the real-NAT convergence proof is the Gate E/A
  item.
- **Gate E — CONDITIONAL.** Every plane's end-to-end lifecycle is exercised at
  loopback (`iroh-rooms-core/tests/e2e_lifecycle.rs`; `iroh-rooms-net`
  `tests/join_e2e.rs`, `tests/message_e2e.rs`, `tests/file_e2e.rs`,
  `tests/pipe_e2e.rs`). The Day-10 criterion additionally requires *"full lifecycle
  converges across real NATs,"* which cannot be asserted until Gate A runs. Hence
  CONDITIONAL, not GO. Blob (soft) and Pipe (soft) gates are GO at loopback.

### 6.3 Transport decision — ADR-1 — AC2

State: **ADR-1 is CONFIRMED (ratified) by measurement, not revised.** Full-mesh
direct QUIC over ALPN `/iroh-rooms/event/1` remains the Room Event Plane transport;
`iroh-gossip` is not adopted for the load-bearing log (parked as an optional
off-critical-path liveness/admin-tip carrier). Cite the measured comparison from
`crates/spike-transport/NOTES.md` and `crates/spike-transport/results/results.md`
(IR-0006/#10, landed PR #69):
- Propagation latency N=2..5: mesh median 16 ms vs gossip median 16–17 ms —
  statistically indistinguishable, confirming *"gossip buys nothing at N≤5"*
  (the stronger form of the claim), not *"gossip is slower."* Both converge to
  full set equality; 0 lagged.
- Late-join gap: 11/11 on both backends; the difference is structural, not
  numeric — the mesh newcomer already holds an authenticated bidi link to every
  member, so backfill is one more frame; the gossip newcomer has no per-peer
  connection to attach a pull to.
- Admission/auth: mesh refuses an out-of-allowlist `EndpointId` **before**
  `accept_bi()`; gossip's open `TopicId` admits an interloper with no auth check
  (measured: interloper published an event a member received). Corroborated by
  `iroh-rooms-net` T2 (`tests/loopback.rs`: reject decided from `remote_id()`,
  connection closed before `accept_bi()`, audit `peer.rejected cause="unknown_device"`).
- Implementation complexity: mesh 405 LOC / 0 added 0.x crates; gossip 299 LOC but
  +1 direct 0.x crate — the ~100-line saving is a "false economy" (gossip has no
  slot for the admission gate / per-peer bookkeeping / frame codec you still need).
- **No measured surprise crossed the Day-4 flip trigger.**
- **Open Decision 13 resolved:** admin-tip rides the mesh `SyncMessage::AdminTip`
  control frame for MVP; gossip liveness topic measured (3–6 ms vs mesh 18–21 ms
  freshness) but **not adopted** — the gap is immaterial and a gossip tip is a
  weaker-trust unauthenticated hint. Record this decision as closed.

### 6.4 Sync substrate decision — ADR-2 — AC2

State: **ADR-2 is CONFIRMED — hand-roll a signed append-only log in SQLite with a
bounded recent-sync pull over the ADR-1 mesh; do not adopt iroh-docs for MVP.**
Evidence:
- The hand-rolled log/sync is built and its convergence measured: SQLite event
  store (IR-0004/#8, `iroh-rooms-core/src/store`), sans-IO recent-sync engine
  (IR-0007/#11, `src/sync`), membership causal fold (IR-0008/#12, `src/membership`),
  all landed. Convergence and arrival-order-independence proven in
  `tests/sync_convergence.rs`.
- **Honest caveat (repeat from Gate C):** iroh-docs was *not* built as a
  counterfactual (no `spike-sync`). ADR-2's rationale is that docs' unique value
  (whole-history RBSR) is exactly the MVP-deferred capability, its data model
  (LWW-KV) mismatches the `prev_events` causal DAG, and it drags three 0.x crates +
  redb onto the critical path. The memo confirms ADR-2 **on that design rationale
  plus a working, measured hand-roll**, and states that no head-to-head docs
  benchmark was run — this is a confirmation of the chosen path, not a measured
  refutation of docs.
- iroh-docs remains the **leading Phase-5 availability-layer candidate**
  (Residual/Open Decision #14), with the `m/`+`c/` prefix-free-key mapping ready.

### 6.5 Pinned dependency observations

Report the **confirmed** versions actually used (not the raw recon table). Cite the
per-crate `NOTES.md` version-confirmation blocks:

| Crate | Confirmed pin | Note |
|---|---|---|
| `iroh` | `=1.0.1` | stable 1.x core; recon originally said `1.0.0`, crates.io sparse index confirmed `1.0.1` |
| `iroh-base` | `=1.0.1` | stable 1.x |
| `iroh-gossip` | `=0.101.0` | builds against iroh 1.0.1 with zero API drift; spike table was correct here |
| `iroh-blobs` | `=0.103.0` | carries `provider::events` ACL; recon's earlier `0.97` was **stale** — corrected to `0.103.0` |
| `ed25519-dalek` | `=3.0.0-rc.0` | release *candidate*; must stay in lockstep with the version iroh 1.0 pins |
| `ciborium` | `^0.2` | verified deterministic-CBOR path in `event-core` (Gate B green) |
| `blake3` | `^1` | stable |

Churn-budget observation to record: only `iroh`/`iroh-base` carry a stability
guarantee; everything above is 0.x on a ~monthly breaking cadence. Budget
**~0.5–1 dev-day per minor bump**, plus a larger one-off if `ed25519-dalek` reaches
3.0 final or `iroh-blobs` graduates to production. ADR-1/ADR-2 deliberately keep the
event critical path on stable core only (0 added 0.x crates), which is the payoff.

### 6.6 Residual risks accepted for MVP

Reproduce the `PHASE-0-SPIKE.md` "Residual Risks & Open Decisions" list, each
marked **Accepted for MVP** or **Descope** with a one-line disposition. Do not
invent new risks; roll up the existing nine irreducible risks + open decisions:

1. **Conditional convergence** (deepest) — Accepted; detectability via admin-tip +
   never-windowed membership sub-DAG; fail-closed on suspected incompleteness.
2. **Removal/leave enforcement bounded by reachability** — Accepted; tear-down-on-learn.
3. **Blob-serve-to-any-Active + no per-blob revocation** — Accepted; key-bound
   invites close fresh-key ban-evasion.
4. **Removed-member timeline pollution (log-valid, capability-zero)** — Accepted;
   UI segregation recommended, not a protocol guarantee.
5. **Timeline position attacker-influenceable** — Accepted; ascribe no trust to order.
6. **`created_at` attacker-chosen/signed** — Accepted; kept out of all
   convergence/authorization decisions (advisory only).
7. **`max_uses` unenforceable** — Accepted; key-bound single-subject invites.
8. **Admin key compromise/loss unrecoverable — SINGLE LARGEST residual** — Accepted
   for MVP with documentation + out-of-band key custody; call this out prominently.
9. **DoS from causally-incomplete junk** — Accepted; §4 anti-amplification bounds
   reduce but do not eliminate; limits tunable, not proven.

Open decisions deferred to MVP time (record as open, not blocking):
- #10 `iroh-blobs` 0.103 (pre-production, has ACL) vs 0.35 (production, no ACL) —
  shipping-line call deferred; spike validated the 0.103 ACL path.
- #11 Cross-version schema skew — needs lock-step versioning or a forward-compat
  policy + shared vectors before a 2nd `schema_version` ships.
- #12 Gate A real-NAT measurement — **the one that gates the recommendation**;
  see §6.7. (D1 half discharged by IR-0006.)
- #13 Admin-tip carrier — **decided** (mesh AdminTip); move to "closed."
- #14 Phase-5 sync substrate (docs vs Meyer RBSR vs p2panda) — deferred by design.

### 6.7 Failed / not-green gates → mitigation, and the MVP go/no-go — AC3 + AC4

Only one gate is not green: **Gate A (PENDING)**. The memo MUST give it a concrete
disposition, and the overall recommendation follows from it. Prescribed logic
(the executing agent selects the branch matching the re-verified Gate A state
from §5):

- **If Gate A still PENDING (expected today) → recommend CONDITIONAL GO.**
  - *Rationale:* Gates B, C, D and the blob + pipe soft gates are green with
    measured or CI-reproducible evidence; ADR-1 is measured-ratified and ADR-2 is
    confirmed with a working, measured hand-roll; the full lifecycle converges at
    loopback. The MVP CLI build (Phase 1A/1B issues) has already proceeded on this
    foundation. The single un-discharged assumption is real-network hole-punching.
  - *Blocking exit condition (P0):* execute the `spike-nat` §4 runbook on two
    machines on different real networks (VPN off, ≥1 likely-symmetric/CGNAT case),
    both directions × {natural, relay-only}, commit the per-run JSON and regenerate
    `crates/spike-nat/results/results.md`, and confirm the Gate A rubric before
    declaring the substrate assumption proven / before any external preview.
  - *Mitigation if Gate A returns NO-GO:* this does **not** auto-fail the MVP —
    relay fallback is the PRD §18.1 mitigation and the rubric already accepts
    "at least relay fallback" for the non-direct case. A NO-GO on *all* paths
    (no direct and no usable relay) triggers the Residual #12 escalation:
    evaluate a self-hosted relay, reconsider discovery config, or flag the
    substrate assumption as broken. Spell out this branch explicitly.
- **If Gate A has since been run and is GO → recommend unconditional GO** and cite
  the committed results JSON + table.
- **If Gate A has been run and is NO-GO → recommend CONDITIONAL / descope**: MVP
  proceeds only with the relay-infrastructure decision made per Residual #12;
  name the failing scenario and the mitigation.

The recommendation MUST be a single explicit sentence at the top of the memo
(§6.1) and restated here with its condition and exit criteria. Ambiguity here
fails AC4.

### 6.8 Evidence matrix (Test Plan)

Include the table from §7 so every gate/decision links to a child issue, its spec,
its crate, and its measured output.

---

## 7. Evidence / traceability matrix (pre-gathered)

Child issues (all landed) → spec → crate → measured output to link. The executing
agent should paste this into the memo (§6.8), trimming columns to fit.

| Issue | IR | Spec (`specs/`) | Crate / code | Measured output to link | Feeds |
|---|---|---|---|---|---|
| #6 (+#45) | IR-0002 | `event-core-canonical-signed-event-model.md`, `strict-cbor-reader-unit-property-fuzz-tests.md` | `iroh-rooms-core/src/event/` | `tests/golden_vectors.rs`, `tests/cbor_property.rs` | Gate B |
| #7 | IR-0003 | `protocol-conformance-test-vectors.md` | `iroh-rooms-core/tests/conformance/` | `tests/protocol_conformance.rs` (empty `DEFERRED` list) | Gate B |
| #8 | IR-0004 | `sqlite-event-store-prototype.md` | `iroh-rooms-core/src/store/` | `tests/store_e2e.rs` | Gate C/D (ADR-2) |
| #9 | IR-0005 | `full-mesh-quic-event-transport.md` | `iroh-rooms-net/` | `NOTES.md`, `tests/loopback.rs` (T1/T2 admission) | ADR-1, Gate E |
| #10 | IR-0006 | `measure-gossip-vs-full-mesh-transport.md` | `spike-transport/` | `NOTES.md`, `results/results.md` | ADR-1 (Gate C/D1) |
| #11 | IR-0007 | `bounded-recent-sync-prototype.md` | `iroh-rooms-core/src/sync/` | `tests/sync_convergence.rs`, `tests/sync_restart.rs` | ADR-2, Gate C/D |
| #12 | IR-0008 | `membership-fold-prototype.md` | `iroh-rooms-core/src/membership/` | `tests/membership_fold.rs` | Gate D |
| #13 | IR-0009 | `prototype-blob-acl-path.md` | `spike-blobs/` | `NOTES.md`, `tests/blob_acl.rs` (49 tests) | Day 8 (soft) |
| #14 | IR-0010 | `live-tcp-pipe-path.md` | `iroh-rooms-net/src/pipe/` | `tests/pipe_e2e.rs` (P1–P6) | Day 9 (soft) |
| #43 | IR-0012 | `measure-real-nat-hole-punching-connectivity.md` | `spike-nat/` | `NOTES.md`, `results/results.md` **(pending)** | **Gate A** |

Note in the memo: IR-0007 and IR-0009 landed commits reference the IR number, not
the `#` issue number — mapping is via the spec headers.

---

## 8. Acceptance criteria (mapped to #15)

The memo is done when:

- [ ] **AC1 — Gate A–E status documented.** §6.2 table present with a status and a
      linked evidence source for each of A, B, C, D, E (and the two soft gates).
      Gate A is shown as PENDING (or updated per §5) with an explicit "no measured
      numbers yet" statement — no gate is left unaddressed.
- [ ] **AC2 — ADR-1 and ADR-2 confirmed or revised with measured evidence.** §6.3
      confirms ADR-1 citing `spike-transport` measured numbers; §6.4 confirms ADR-2
      citing the working hand-roll's convergence tests **and** explicitly states the
      D2 head-to-head docs benchmark was not run.
- [ ] **AC3 — every not-green gate has a concrete mitigation or descope.** §6.7
      gives Gate A a P0 runbook exit condition plus the relay-escalation branch;
      any other non-green gate (none expected) similarly dispositioned.
- [ ] **AC4 — MVP go/no-go recommendation is explicit.** A single unambiguous
      recommendation sentence appears in the TL;DR (§6.1) and is restated with
      conditions in §6.7.
- [ ] **Test Plan — Phase 0 child issues reviewed and measured outputs/transcripts
      linked.** §7 evidence matrix present; every link resolves to a real path in
      the repo.
- [ ] Memo lives at `PHASE-0-GO-NO-GO.md`, links to `PHASE-0-SPIKE.md` and
      `PRD.v0.3.md` §19, and passes a markdown-lint / dead-link sanity check
      (all cited paths exist).

---

## 9. Implementation steps (for the executing agent)

1. **Re-verify the evidence baseline (§5).** Read `spike-nat` NOTES + results and
   `iroh-rooms-net` NOTES §"Gate A" to fix Gate A's current state; confirm the
   pinned versions from the crate `Cargo.toml`s / NOTES; optionally run
   `bash scripts/verify.sh` and record greenness. Select the §6.7 recommendation
   branch accordingly.
2. **Create `PHASE-0-GO-NO-GO.md` at the repo root** with the §4 skeleton.
3. **Write the header + TL;DR (§6.1)** — put the explicit recommendation first.
4. **Fill the Gate results table and paragraphs (§6.2)** using the pre-gathered
   verdicts and evidence links; keep detail in the linked NOTES.
5. **Write ADR-1 (§6.3) and ADR-2 (§6.4)**, including the honest D2-not-benchmarked
   caveat in both Gate C and ADR-2.
6. **Write pinned-dependency observations (§6.5)** from the confirmed versions, not
   the raw recon table.
7. **Write residual risks + open decisions (§6.6)**, marking each accepted or
   descoped; call out Residual #8 (admin key) as the largest.
8. **Write the failed-gate mitigation + MVP go/no-go (§6.7)** with the exit
   condition and the relay-escalation branch.
9. **Paste the evidence matrix (§7 → §6.8)** and verify every linked path exists.
10. **Self-check against §8 ACs**; fix any dead links; confirm the recommendation
    sentence is present in both §6.1 and §6.7.
11. (Optional) add the one-line pointer from `PHASE-0-SPIKE.md` Day 10 to the memo;
    otherwise note its omission in the memo.

---

## 10. Risks

- **Overclaiming Gate A as green (highest risk).** The harness landing and a green
  CI loopback can read as "Gate A passed." It did not — CI cannot prove NAT
  traversal. The memo must state PENDING and gate the recommendation on it.
  Mitigation: §5 re-verification step + explicit CONDITIONAL language.
- **Overclaiming a D2 measured comparison.** No `spike-sync`/iroh-docs bake-off
  exists; ADR-2 is confirmed on design rationale + a working hand-roll. Stating a
  measured head-to-head would be false. Mitigation: the explicit caveat in §6.3/§6.4.
- **Stale version numbers.** Copying `PHASE-0-SPIKE.md`'s recon table would ship
  wrong pins (`iroh 1.0.0`, `iroh-blobs 0.97`). Mitigation: cite confirmed
  per-crate versions (§5.2, §6.5).
- **Evidence drift after this spec.** Verdicts are dated; the child issues can move.
  Mitigation: §5 re-verification before writing, and dating the memo.
- **Scope creep into re-deciding ADRs or writing the Phase-5 decision.** The memo
  records decisions; it does not re-open them. Mitigation: §2 non-goals.
- **Dead links.** Many cited paths; a typo breaks traceability (AC/Test Plan).
  Mitigation: step 9 path-existence check.

---

## 11. Assumptions

- The memo is a docs/decision artifact only; no code or tests change (consistent
  with the ADW "planning/spec only" instruction and #15's `type/docs` label).
- Gate A is still PENDING at write time (true as of 2026-07-02); if not, §6.7
  branches handle it.
- All ten Phase 0 child issues remain landed with the evidence described in §7;
  the executing agent spot-checks rather than re-runs every suite.
- Root placement (`PHASE-0-GO-NO-GO.md`) is acceptable; `docs/` is a cosmetic
  alternative (OQ-1).
- Confirmed pins are `iroh =1.0.1`, `iroh-blobs =0.103.0`, `iroh-gossip =0.101.0`
  unless §5.2 shows otherwise.

---

## 12. Open questions

- **OQ-1 — File location.** Root `PHASE-0-GO-NO-GO.md` (recommended, beside
  `PHASE-0-SPIKE.md`) vs `docs/phase-0-go-no-go.md`. Default: root.
- **OQ-2 — Should Gate A be executed before this memo, making it unconditional?**
  This spec assumes not (the harness owner runs the two-host measurement on their
  own schedule) and writes a CONDITIONAL GO with a P0 exit condition. If the
  reviewer wants an unconditional call, the real-NAT run (IR-0012 runbook) must
  complete first — that is a separate task, not part of writing the memo.
- **OQ-3 — Recommendation strength.** Is CONDITIONAL GO acceptable to
  stakeholders, or is an unconditional decision required now? The spec prescribes
  CONDITIONAL because it is the honest read of the evidence; escalate if a
  stronger call is demanded.
- **OQ-4 — Edit `PHASE-0-SPIKE.md`?** Whether to add the Day-10 back-pointer
  (one line) or leave the existing file untouched. Default: optional, note in memo
  if skipped.
