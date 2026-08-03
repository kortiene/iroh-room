# ADR-0006: Replace the same-`admin_seq` Fork Oracle with a Fold-Level Divergence Detector

- **Date:** 2026-08-02
- **Status:** Accepted
- **Supersedes:** the detection mechanism of [ADR-0005](ADR-0005-admin-fork-is-advisory-not-fail-closed.md) (the *response* — advisory, not fail-closed — is unchanged).
- **Issue:** #213 — [CORE] Replace the same-admin_seq fork oracle with a fold-level divergence detector.
- **Related:** #191 / #211 (the wedge ADR-0005 closed), conformance Vector 12 (`tests/conformance/advisory.rs`) and Vector 18 (`tests/conformance/membership.rs`).

## Context

ADR-0005 made a detected admin fork **advisory** (it denies nobody; `fail_closed` is driven by the `behind` condition alone). It deliberately left the *detector* alone. That detector — two distinct held admin events at one `admin_seq`, via the engine's `admin_ids_by_seq` map — is unsound in **both** directions:

1. **It over-fires on ordinary concurrency.** Two admin events at one `admin_seq` are merely *causally concurrent*, which an honest admin produces by publishing from a stale head. `admin_seq` is a **DAG depth**, not a chain length — `store/mod.rs::compute_admin` is `1 + max(defined parent admin_seqs)`. The self-parent MUST that would have made it a chain length (`PHASE-0-SPIKE.md:526`) was descoped twice and never landed. So a benign stale-head publish and a deliberate fork produced the identical signal.
2. **It misses the case it is named for.** Conformance Vector 12 parents *both* admin branches on a member-authored event; neither parent carries a defined `admin_seq`, so both branches derive `None` and the oracle never sees a collision. Any conflicting pair whose branches land on different seqs is likewise invisible.
3. **It manufactures forks with no concurrency at all.** `admin_ids_by_seq` caches the accept-time `admin_seq`, but `propagate_from` renumbers stored rows when an ancestor backfills (e.g. `3 → 10`). The stale `(old_seq → id)` entry then collides with a later legitimate event at `old_seq`. The map is insert-only, so it also grows unbounded.

## Decision

Retire same-`admin_seq` co-occurrence as the fork signature and replace it with a **fold-level divergence detector** (`RoomMembership::admin_divergence` in `membership/fold.rs`):

> A divergence is two **causally concurrent** admin-authored membership writes with a **different** authorization effect on the **same** subject.

The detector groups admin authorization writes (`MemberInvited`, `MemberRemoved`) by subject, takes the causal heads (concurrent subset) via the existing `touch_events` / `causal_heads` primitives, and fires iff ≥2 heads carry ≥2 distinct effects. The engine's `Completeness::AdminForkDetected` verdict and the CRITICAL `equivocation` trust decision are driven by it. `admin_ids_by_seq`, `note_admin_event`, `note_admin_id`, and `seed_admin_state` are **deleted outright**, which removes the phantom-fork vector (3) and the unbounded growth together.

**The boundary — "different effect only".** Same-effect concurrency (e.g. two concurrent removals of one member) does **not** fire: the fold resolves the subject to the identical state either way, so there is no conflict to arbitrate and no harm. The detector fires only on a real conflict the fold must arbitrate by least-privilege — e.g. an invite and a removal of one subject, or two invites of one subject with different roles. This is the line the issue confirms.

**Unchanged (the freeze).** `admin_seq` derivation, storage, the `RC3_DERIVED_CACHE` compatibility fixture, and the `SyncMessage::AdminTip` wire encoding are byte-identical. The verdict remains **advisory** per ADR-0005 — `fail_closed` still comes from `behind` alone; `AdminForkDetected` still denies nobody.

## Coverage boundary (what the detector sees, and what covers the rest)

`admin_divergence` reads admin authorization writes — the same surface the fold's `touch_events` matches, restricted to admin-authored membership events. The boundary the reviewer must attack:

| Content type | In scope? | Covered by |
|---|---|---|
| `MemberInvited` | **Yes** — effect distinguishes role (`invite:<role>`). | This detector (e.g. two concurrent invites of one subject with different roles — Vector 18's shape). |
| `MemberRemoved` | **Yes** — effect `remove`. | This detector (invite + remove of one subject conflicts; two removes are same-effect ⇒ no fire). |
| `MemberJoined` / `MemberLeft` | No — self-authored, not admin authorization writes. | Not an admin equivocation; same-effect by construction (one join / one leave per membership lifecycle). |
| `MessageText` / other non-membership | No — no membership effect. | The fold's structural `Flag::Equivocation` (`is_equivocation`) still flags any same-author concurrent events as a broad advisory signal; it never writes a CRITICAL row or a verdict. |
| `MemberKeyDistribution` | No — not a `touch_events` type. | The independent D5a epoch-poisoning path (`engine.rs` `epoch_commitments`), which adopts **neither** key on a same-epoch conflict. |
| `RoomCreated` | No — genesis only. | Unique by structure. |

The net effect: outside the four membership types, equivocation becomes **provably undetectable by this detector** rather than unreliably detected. That is a harder, honest boundary than the old oracle's blunt "any admin pair whose seqs collided".

## Alternatives Rejected

- **"Fix" it by scoping `admin_seq` to membership types.** Rejected — it *severs* the chain rather than thinning it. Derivation propagates only through parents that already carry a defined seq, so nulling admin chat orphans later admin events: the rc.3 fixture chain `E_CREATE(0)→E_MESSAGE_1(1)→E_MESSAGE_2(2)→E_MESSAGE_3(3)→E_INVITE_BOB(4)` (`compatibility.rs::RC3_DERIVED_CACHE`) would make the invite `None`, pin `admin_chain_tip` at genesis forever, and silently kill the T18 withheld-removal detector. This is the first thing #191 itself proposed; do not do it.
- **Keep the oracle and patch the misses.** Rejected — the phantom-fork vector and the unbounded `admin_ids_by_seq` growth are structural; patching collisions one-by-one preserves a detector whose core premise (`admin_seq` as a chain length) is false.
- **A broad "any concurrent same-author events" engine detector.** Rejected — it re-creates the over-fire: two admin chat messages would raise a CRITICAL row on every stale-head publish (the exact regression ADR-0005's advisory posture was built to tolerate). The structural `Flag::Equivocation` already serves this broad role at the fold, advisably and without a durable CRITICAL row.

## Consequences

- `AdminForkDetected` and the CRITICAL `equivocation` row fire only on a **genuine** conflicting-effect divergence — never on ordinary concurrency (pinned by `admin_stale_head_publish_does_not_wedge_the_room` and `same_effect_concurrent_admin_events_are_not_a_divergence`).
- A real cross-partition fork is still detected once the never-windowed membership backfill reconciles both branches onto the held fold (`cross_partition_admin_fork_detected_after_membership_backfill`, now built on a conflicting-role pair).
- A fabricated advertised tip still cannot forge a divergence — the detector reads the held fold only (`fabricated_admin_tip_at_held_seq_does_not_forge_a_fork`).
- The verdict's meaning changed (`Completeness::AdminForkDetected` now means "divergence", not "same-`admin_seq` collision"); the variant's Rustdoc was updated in the same commit. `TrustDecision.admin_seq` stays `u64` (its rustdoc already documents `0` = "concerns no admin event"); the divergence decision records `0` and carries the conflicting event ids.

## Follow-ups (carried from ADR-0005, still open)

- **Severity predicate** (ADR-0005 #1). The engine raises CRITICAL unconditionally; `PHASE-0-SPIKE.md:665` requires CRITICAL only when the forked events touch membership/authz, INFO otherwise. Needs a new `Severity` variant — a public API change.
- **Operator visibility** (ADR-0005 #3). `trust_decisions()` has no reader outside the engine; the alert reaches the durable table and is surfaced nowhere in the CLI.
- **Cross-partition detection** (ADR-0005 #4). Only the holds-both-branches variant exists; the advertisement-based variant that detects a fork "even when no peer holds both branches" remains unimplemented (`handle_admin_tip` deliberately refuses to feed advertisements into held state to prevent forgery).
