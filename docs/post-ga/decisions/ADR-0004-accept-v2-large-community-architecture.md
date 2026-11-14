# ADR-0004: Accept the v2 Large-Community Architecture (#134)

Status: Accepted — ratified 2026-07-30
Date: 2026-07-30
Owners: Maintainer (Sekou), protocol lead
Refs: #134 (proposal), ADR-0001..0003 (unchanged), #140 (Phase B), #162 (deferred-work triggers)

## Context

#134 proposes a second-generation architecture for large, intermittently
connected communities — target **10,000 registered members / 2,000 concurrently
online devices / 1,000 content events/s across 100 streams**. It separates a
small, totally ordered, quorum-signed **governance plane** from many
subscription-driven **content streams** served by **3–7 durable community
replicas** that provide availability, fanout, offline delivery, checkpoints, and
persistence receipts without becoming the authority for event validity. v1
remains supported as a small-room profile (`/iroh-rooms/event/1`); v2 is a new
ALPN/schema (`/iroh-rooms/event/2`).

The proposal defines a five-phase implementation sequence (§23) and seven
"issues to resolve before wire freeze" (§25).

#134 had been **"Proposed specification, v0.1"** since introduction, leaving the
v2 track unable to move from design into committed implementation: the §25
wire-freeze specs/decisions (#155–#161) are explicitly gated on an accepted
baseline.

## Decision

**Accept #134 as the v2 large-community architecture of record.**

> `iroh-room` adopts #134's governance/content/replica separation as the v2
> architecture. v1 remains the small-room profile. **Phase C** (v2 network +
> storage: `/iroh-rooms/event/2`, replica receipts + `W`-quorum assembly, range
> reconciliation, stream checkpoints, snapshot startup + compaction) is the next
> implementation phase, gated on the §25 wire-freeze decisions.

Acceptance is for the **architecture and its phasing**, not a commitment to a v2
ship date. The §25 wire-freeze decisions remain open and MUST be resolved before
any v2 interoperability is claimed.

## Phase Status at Ratification

| Phase | Scope | Status |
|---|---|---|
| **A** | v1 guardrails (byte-bounded queues, adaptive sync, batched SQLite, cached projection, ceiling warning) | ✅ complete — closed milestone "v1 hardening + N=40 measurement" |
| **B** | v2 crypto core: IDs, governance state machine, fork handling, member Merkle, content-event validation, golden vectors (#140) | ✅ complete — implemented, golden-vectored, and **independently audited APPROVED** (`docs/audits/v2-governance-fork-audit-2026-07-30.md`) |
| **C** | v2 network + storage (ALPN, replica receipts/quorum, range reconciliation, checkpoints, snapshot startup) | ⬜ not started — next phase |
| **D** | production hardening (multi-replica failure/equivocation, threshold/recovery/rotation UX, abuse controls, migration tooling) | future |
| **E** | federation, per-stream replica groups, regional placement, mandatory group encryption | future (separate specs) |

## What This Unblocks

- The §25 wire-freeze specs/decisions can move from proposal to normative:
  **#155** (range-reconciliation), **#156** (durability class), **#157** (replica
  key vs Endpoint ID), **#159** (replica replacement/equivocation/recovery UX),
  **#160** (10k roster ≤5 MiB benchmark), **#161** (governance snapshot encoding
  + admin-transition proof). (#158 — content/moderation schemas — already closed.)
- **Phase C becomes eligible to schedule**: #162 gates replica infrastructure on
  three triggers, one of which is *"v2 crypto core (#140) complete,
  golden-vectored, and independently audited"* — that trigger fires with this
  ratification (Phase B + audit complete).
- The #140 crate (`iroh-rooms-v2-core`), its frozen golden vectors, and the audit
  become the foundation Phase C wires to (epic spec §12: runtime/store/network
  crates consume the pure core through separate issues).

## Recommended v2 Sequencing (Non-Normative)

1. **#160 first** — the 10k-roster ≤5 MiB benchmark is a P1 spike and the
   architecture de-risker: its result determines whether v2.0 needs proof-carrying
   light-client mode at all. It reuses the `iroh-rooms-v2-core` machinery.
2. **#155** — range-reconciliation algorithm + spec-owned envelope (the other P1;
   the sync crux of Phase C).
3. The four P2 decisions (#156/#157/#159/#161) → wire freeze → Phase C.

v1 / **M4 (Production Beta)** remains the parallel near-term track.

## Alternatives Considered

### Keep extending v1 instead of adopting v2
Pros: no new ALPN/store/replica surface; smaller blast radius.
Cons: v1's full-mesh topology is an architectural limit — `C(C−1)/2` logical
links and ~`12C(C−1)` periodic control messages/s before content (§2 of #134) —
not a tuning problem. It cannot reach the target profile.
Decision: **rejected**.

### Defer the decision until every §25 item + #160 land
Pros: commits only after all unknowns are resolved.
Cons: the §25 issues are themselves gated on an accepted baseline; indefinite
deferral freezes the whole v2 track. With Phase B + audit complete, the cost of
continued deferral now exceeds the cost of ratifying and resolving §25 against a
stable reference.
Decision: **rejected**.

### Adopt v2 in place (reinterpret v1 wire)
Pros: one protocol.
Cons: #134 §20 explicitly forbids silently translating live v2 governance/stream
semantics into v1; v1 signed bytes, IDs, and history must remain verifiable.
Decision: **rejected** — v2 is a new ALPN/schema, as specified.

## Review Triggers

Revisit this ADR if:

- **#160** misses badly enough that v2.0 must adopt proof-carrying light-client
  mode (changes the client model in §8.2/§14).
- Any §25 wire-freeze decision surfaces a flaw in the #134 architecture.
- A real deployment ask makes the v1 (M4) vs v2 sequencing urgent.
- The Phase C scope proves materially larger or smaller than §23 estimates.

## Final Recommendation

Ratify #134 as accepted. Sequence the v2 track as #160 → #155 → the remaining
§25 decisions → Phase C, while keeping M4 (v1 Production Beta) as the parallel
near-term track. Treat the #140 crate + audit as the frozen foundation; do not
re-open Phase B wire formats without an explicit schema-version bump.
