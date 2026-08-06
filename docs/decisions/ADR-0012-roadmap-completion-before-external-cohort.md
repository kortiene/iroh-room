# ADR-0012: Complete the Roadmap Before the External Cohort

- **Date:** 2026-08-06
- **Status:** Proposed until the merge that introduces this ADR; accepted by
  that merge
- **Decision owner:** Maintainer
- **Related:** `ROADMAP.md` RB-1, milestones #5/#8, issues #99–#103, #162,
  `PRD.v0.3.md`, `PRODUCTION-READINESS.md`

## Context

The repository has several overlapping descriptions of future work:

- the current PRD v0.3 product phases, security roadmap, availability options,
  and future call stages;
- the historical PRD v0.2;
- production, security, operations, SDK, platform, and release-readiness plans;
- the accepted v2 Phase C/D/E architecture and its specifications; and
- the cross-repository Cockpit backlog, whose table enumerates 36 items even
  though its summary footer says 34.

GitHub milestones and issues do not currently represent all of that work. Some
specification issues are closed even though their runtime, store, codec, vector,
or product work is not implemented. Historical release sign-offs also contain
waivers that were sufficient for a narrow candidate but do not complete the
future deliverable they waived.

Milestone #5, M4 Production Beta Learning Loop, was closed as **deferred** on
2026-08-06 before any external attempt occurred. Issue #99 remains historical
preparation evidence. Cohort issues #100–#103 moved to the undated milestone #8,
Final Milestone — External Validation After Roadmap Completion.

The project therefore needs one finite authority that answers what must be
complete before that final cohort and distinguishes work that can only happen
during or after the cohort.

## Decision

Adopt [`ROADMAP.md`](../../ROADMAP.md) RB-1 as the authoritative roadmap
completion contract.

The external builder cohort starts only after:

1. every cohort-blocking `PRE` row is complete;
2. every cohort-blocking `COND` row is explicitly resolved;
3. every recurring `FC` gate passes on the exact tagged roadmap-complete cohort
   candidate;
4. no cohort-blocking issue or advertised-but-unreachable capability remains;
   and
5. three named external participants are confirmed.

Only cohort-dependent work is excepted from that precondition: the participant
attempts, human/product evidence produced by them, cohort-discovered fixes, the
Discussions decision, candidate regeneration caused by those fixes, and the
final GA decision.

An independent security reviewer, network operator, hardware operator, replica
operator, call-test operator, release reviewer, or recovery-test operator is a
verification role, not a cohort participant. Those roles may and should operate
before the cohort.

## Scope governance

An explicit future deliverable defaults to included. `Optional`, a historical
waiver, `closed:not_planned`, stale issue closure, or lack of a current issue is
not a removal decision.

Every required RB-1 row must receive an issue in its owning repository after the
baseline is accepted. Scope changes require a reviewed RB-1.x amendment that:

- names every row added, removed, or reclassified;
- states the reason and approval;
- updates dependencies, evidence requirements, hardware/role requirements, and
  cohort-blocking flags; and
- adds an entry to the baseline amendment log.

`PRD.v0.3.md` remains the current product requirements document. `PRD.md` is the
historical v0.2 record and cannot silently compete with v0.3. Historical audits,
release records, and sign-offs remain immutable evidence; later corrections are
additive.

## Candidate and cohort semantics

The pre-cohort tag is a **roadmap-complete cohort candidate**, not an immutable
GA candidate. If cohort feedback causes a material security, protocol,
migration, platform, networking, or product change, the project cuts a new
candidate, reruns every affected final-candidate gate, and records whether the
30-day cohort window must restart or extend. GA is decided only after that work
and the cohort evidence are complete.

The v2 migration trigger that requires a v2 beta does not require the external
cohort. The project may cut an internal/protocol v2 beta after Phase C, use it to
validate migration and hardening, then complete v2 Phase D/E and the remaining
roadmap before the final cohort.

## Preservation requirements

RB-1 does not authorize historical rewrites:

- frozen v1/v2 signed artifacts, legacy domains, checkpoints, fork envelopes,
  compatibility fixtures, and approved release records stay immutable;
- corrections use additive successor schemas, vectors, or errata;
- `admin_seq` remains complete carrier-chain DAG depth, preserving T18 and
  conformance Vector 18; and
- divergence caching may key only on `membership_projection_generation`.

## Consequences

- The existing recommendation to complete Phase 2.5 and its cohort before
  Phase 3 no longer controls scheduling. `PRODUCTION-READINESS.md` remains an
  important source of gates and evidence, but RB-1 controls roadmap order.
- Cohort and human-product evidence no longer appear as circular prerequisites
  for product phases that must precede the cohort.
- Cellular and Apple Silicon evidence require acquisition or rental if the
  current fleet remains unchanged. A permanent waiver is not completion.
- The non-existent `pi-rooms-cockpit` repository must be created before its CPT
  issues can be materialized.
- The first technical decision after issue materialization is D-9. Phase C and
  new post-MVP event types depend on it.
- #192 lifecycle residuals precede authoritative #214 measurement so the
  evidence is not collected against a known unhealthy mesh lifecycle.
- #162 remains open after its explicit Trigger 3 record; that record does not
  itself start implementation or waive entry gates.

## Alternatives considered

### Treat existing GitHub milestones as the roadmap

Rejected. They omit PRD phases, Phase C/D/E implementation, production and
security residuals, several CLI/SDK surfaces, and the Cockpit backlog.

### Use PRD v0.3 alone

Rejected. It does not contain the accepted v2 architecture, detailed production
gates, current correctness findings, release/platform obligations, or Cockpit
work.

### Keep the cohort before later product phases

Rejected by maintainer decision. It would make the cohort begin before all
explicit future non-cohort work was complete and would preserve the current
circular roadmap.

### Allow permanent waivers to satisfy unavailable evidence

Rejected. Acquire or rent the missing hardware/role, implement a remedy, or
approve a versioned baseline amendment that changes scope.
