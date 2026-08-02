# ADR-0005: A Detected Admin Fork Is Advisory, Not Fail-Closed

Status: Accepted
Date: 2026-08-02
Owners: Release owner
Issue: #211 (successor to #191)

## Context

The sync engine derives an `admin_seq` for every admin-authored event and
declares a **fork** when it holds two distinct validated admin events at the same
`admin_seq` (`sync/engine.rs` `recompute_completeness`). On that verdict it set
`Completeness::AdminForkDetected`, recorded a CRITICAL `equivocation` trust
decision, and **fail-closed every non-admin member**.

That verdict was permanent. `admin_ids_by_seq` is insert-only, the store has no
delete path, and the map is re-seeded from the store on open — so the denial
survived restarts with no exit.

Two admin events sharing an `admin_seq` means only that they are **causally
concurrent**: neither cites the other. That is ordinary concurrency. It happens
whenever the admin publishes from a stale head — mint an invite, then send a chat
message before the first has been incorporated. Both derive the same seq and the
room bricks.

This is not hypothetical. Root-causing the N=40 gossip collapse (PR #190) showed
the harness accidentally produced exactly this collision and the shipped path
tore a 40-node cluster down within one 5s window. The gossip overlay was
exonerated; this fail-closed path was the cause. `crates/spike-N40/src/workload.rs`
still carries the workaround comment.

It is also reachable through shipped code rather than only through races:

- **A public API manufactures it.** `iroh-rooms-net` exports `pub mod demo`, whose
  `admin_message(host, room, genesis_id, n)` hard-codes
  `prev_events: vec![genesis_id]` (`demo.rs:122-144`). Its own doc comment frames
  it as a repeatable liveness probe, so the second call produces a second
  admin-authored event at `admin_seq = 1` and wedges the room.
- **Concurrent admin CLI processes.** There is no cross-process lock anywhere in
  `iroh-rooms-cli`, and concurrent `EventStore` connections on one database file
  are an explicitly supported, tested shape (issue #85, raised by a downstream
  consumer). Two admin commands each read `heads()` before either writes.
- **Any embedder that caches a parent id** instead of re-reading heads.
- **The admin wedges itself.** `publish()` routes local authoring through the same
  accept path as remote ingest, so this is not a remote-only condition.

### What the normative sources actually say

The permanence and the blast radius were never designed. Both were implementation
drift:

1. **Fail-closed was specified as temporary.** `PHASE-0-SPIKE.md:622`: "fail
   closed on the affected subjects **until reconciled**." `:664` repeats it:
   "fail closed on contested subjects **until reconciled**." The shipped verdict
   had no reconciled state.

2. **The scope was "affected subjects", not everyone.** Same lines. The
   implementation denies every non-admin, non-Removed member
   (`engine.rs` fail-closed set).

3. **The fold already resolves it.** `PHASE-0-SPIKE.md:663`: on an admin signing
   two branches, "the fold (Removed-dominates / least-privilege) converges all
   peers holding the same set to the same safety-biased result. We do NOT try to
   pick 'the real branch.'" Test Vector §18
   (`vector_18_concurrent_attributes_least_privilege`) pins this with two
   concurrent admin invites on genesis — the identical collision.

4. **The protocol reference classifies the signal as advisory.**
   `docs/protocol.md` §9 lists `equivocation` under "Advisory flags (event still
   accepted, ordered, and persisted)" with the bolded invariant that advisory
   flags "**never** affect the validated set, ordering, or any
   authorization/expiry verdict." Test Vector §12
   (`vector_12_admin_equivocation_flagged`) builds two concurrent admin **chat**
   messages and asserts both are accepted, advisory-only. §8 then contradicted §9
   by describing a fail-closed response.

5. **Ejecting the admin was explicitly out of scope.** `PHASE-0-SPIKE.md:665`:
   "MVP **detects and alerts**; it does not auto-eject the admin (the admin is
   the trust root; **a malicious admin compromises the room by definition**)."

### The detector fires at the wrong moment

The spec's fork detection is a **cross-partition** mechanism: two distinct admin
tips at the same `admin_seq` are "detectable across the room **even when no peer
holds both branches**" (`PHASE-0-SPIKE.md:664`), via tip advertisement.

The shipped detector requires the opposite. It fires only when a single node
**holds** both branches (`held.len() >= 2`, re-confirmed against the store). And
`handle_admin_tip` deliberately refuses to feed advertisements into the held
state, because doing so would let one peer forge a fork.

So the implemented detector fires precisely when the branches have been
reconciled — the condition the spec names as the moment fail-closed should
**end**.

## Decision

**A detected admin fork is advisory. It does not gate authorization.**

`fail_closed` is now driven by the *behind* condition alone — a known-higher
admin tip not yet backfilled. That is the case where information is genuinely
missing, where the "some unapplied admin event might remove someone" rationale
holds, and which already clears on catch-up or via the bounded attempt budget.

The fork state is retained and still recorded: `completeness()` still reports
`AdminForkDetected`, and the CRITICAL `equivocation` decision is still written to
the durable audit trail. **Detection is unchanged; only the response changed.**

### Why this does not weaken a real defence

- The admin chain defends **revocation timeliness** (threat-model **T18**,
  withheld removal), not admin honesty. That defence is the *behind* path, which
  is untouched. `stale_admin_tip_fails_closed_then_recovers` still passes.
- Against a malicious admin the denial bought nothing. The sole immutable admin
  can remove every member unilaterally; equivocating grants it no capability it
  lacks. The spike says as much.
- Where a node holds both branches, the fold has already merged them at least
  privilege. Denial adds no safety over a merge that is already safety-biased.
- The signal is undecidable. A benign stale-head publish and a deliberate fork
  are byte-identical. Permanent denial on an undecidable signal is a guaranteed
  outage traded for a speculative defence.
- The signal is not even reliable. `admin_seq` is **not stable**: `propagate_from`
  can raise an already-stored event's value when an ancestor backfills, while the
  engine caches the accept-time value in `admin_ids_by_seq` and never re-reads
  it. That is an independent *phantom*-fork vector, gating access on a stale
  cache.

## Alternatives Rejected

**Scope `admin_seq` to membership/governance types.** Issue #191 offered this,
and `PHASE-0-SPIKE.md:665` supports the underlying idea (fork severity should
depend on whether the forked events "touch membership/authz"). Rejected for now
as the primary fix because it is expensive and does not address the defect:

- `admin_seq` is a **persisted column**. Existing databases keep values computed
  under the old rule; nothing recomputes them on open (`EventStore::rebuild()`
  has zero production callers).
- Forcing a rebuild is O(all events), re-decodes and re-hashes every row, and
  hard-errors on any corrupt row.
- Bumping `user_version` past 2 makes the database **unopenable by rc.5** and by
  the pinned downstream `qualification/jeliya-*` build.
- `admin_seq` values are compared **across nodes**: `SyncMessage::AdminTip`
  carries `(event_id, admin_seq)` and `handle_admin_tip` compares the peer's seq
  against the local tip. Changing the derivation makes seqs non-comparable
  between versions, and an unpatched rc.5 peer could under-detect being behind —
  a fail-*open* direction.
- Most importantly it only narrows the collision class. Two concurrent admin
  *membership* events still collide, and Test Vector §18 says that case is normal.

Once a fork no longer denies anyone, a benign collision is harmless, so scoping
is no longer needed for availability. It remains worth doing later to cut alert
noise, as a coordinated change with a compatibility floor.

**Bounded/timed recovery from the fork.** Deny for N ticks, then clear. Rejected:
the fork verdict is already, by construction, the reconciled state, so a timer
would delay recovery without ever preventing anything.

## Consequences

- The demonstrated benign wedge is fixed, including for rooms **already wedged**:
  the state stops gating on the next `recompute_completeness`, with no migration
  and no operator action.
- Purely local. No wire change, no schema change, no stored-value change, no
  mixed-version risk. A patched node interoperates with rc.5 peers unchanged.
- A genuinely equivocating admin is no longer met with room-wide denial. This is
  a deliberate, documented reduction, justified above: it is alerting rather than
  enforcement, exactly as `PHASE-0-SPIKE.md:665` specifies.
- Regression coverage: `sync_smoke.rs::admin_stale_head_publish_does_not_wedge_the_room`
  pins the repro #191 asked for and never got. Before this change it failed with
  both non-admin members denied.

## Follow-Ups

1. **Severity predicate (spec gap).** `PHASE-0-SPIKE.md:665` requires CRITICAL
   only when "the forked events touch membership/authz", INFO otherwise. The
   engine raises CRITICAL unconditionally, so a stale-head chat publish still
   writes a CRITICAL audit line. Implementing it needs a new `Severity` variant —
   a public API change — so it is deferred.
2. **Phantom forks.** `admin_ids_by_seq` caches accept-time `admin_seq` values the
   store may later revise. Now cosmetic (it only mislabels an advisory alert), but
   it should be re-derived or invalidated.
3. **No production consumer.** `completeness()` and `trust_decisions()` have no
   production readers, so the CRITICAL alert reaches the durable audit trail but
   is surfaced nowhere in the CLI. The alert should be operator-visible now that
   it is the entire response.
4. **Cross-partition detection is unimplemented.** The spec's advertisement-based
   fork detection (`PHASE-0-SPIKE.md:664`) does not exist; only the
   holds-both-branches variant does. Implementing it safely means solving the
   forgery problem `handle_admin_tip` documents.
