# ADR-0011: Governed Replica Replacement and Evidence-Driven Quarantine

- **Date:** 2026-08-03
- **Status:** Proposed — accepted when this decision is merged
- **Owners:** Protocol lead, release owner
- **Issue:** #159 — `[SPEC] §25 #5: Replica replacement, equivocation, recovery UX`
- **Related:** #134 §§5.2, 7.2–7.3, 10, 11, 13.4, 19, 25 #5; #155–#157; #161; ADR-0004; ADR-0007–ADR-0010
- **Normative profile:** [`specs/v2-replica-replacement-recovery.md`](../../specs/v2-replica-replacement-recovery.md)

## Context

#134 requires governance-controlled replicas, `W` matching persistence receipts,
and `W` replica signatures on stream checkpoints. It says to preserve two
conflicting checkpoint signatures as equivocation evidence and exclude the
signer according to governance policy, but it does not define the policy, the
replacement transition, or what an operator does.

The safe replacement shape is constrained by decisions already made:

- `ReplicaId` is the replica signing public key, so changing that key creates a
  new identity and receipt-sequence namespace.
- The predecessor and successor cannot both count as seats. Adding the new
  record and disabling the old one as two ordinary operations can temporarily
  inflate quorum weight or drop the active set below policy.
- A replacement must catch up and satisfy `local_sync_group_v1` readiness
  before it receives receipt or checkpoint weight.
- Receipts and checkpoint votes from different governance heads, replica
  component roots, classes, or old/new replica identities do not combine.
- Governance recovery keys resolve governance forks. They are not a shortcut
  around ordinary administrator authorization for replica operations.

The Phase-B candidate cannot express those constraints. It has only `active`
and `disabled` records, a three-field opaque descriptor, no authenticated `W`
or durability class, and a `replica.set` that upserts one map entry. Those bytes
and their vectors are frozen candidate evidence and cannot be silently
reinterpreted.

## Decision

Stable v2.0 uses a **governed staged lifecycle followed by one atomic full-set
transition**:

```text
post-genesis: provisioned -> staged -> active -> disabled (permanent)
                                  \-> disabled (abandoned/rejected)
genesis only: provisioned ----------------> active
```

`provisioned` is operator-local. `staged`, `active`, and `disabled` are
authenticated successor replica-policy states. A staged replica receives only
the bounded catch-up lane needed to verify and durably install public protocol
state. It cannot issue a persistence receipt, sign or propose a stream
checkpoint, serve as publication authority, or contribute to `R` or `W`. The
direct initial-active edge exists only in authenticated genesis; every later
identity stages first. A staged identity may be permanently disabled without
activation.

The successor `replica.set` profile replaces the complete sorted replica policy
and explicit receipt/checkpoint quorum in one predecessor-authorized operation.
It is not an extension of the candidate single-record payload. Stable genesis,
governance, state-root, and snapshot containers therefore use successor schema
versions and additive vectors; changed genesis bytes derive a new
`CommunityId`.

Same-identity crash recovery is fail-closed: every receipt, checkpoint,
prepare/cancel/handoff, fork-resolution dependency/control, evidence-intake,
incident/quarantine, and single-writer journal must recover as one exact
continuous state. The current lineage must have no unresolved fork-frontier
requirement, and only a governed active, locally non-quarantined identity may
resume issuance. Any uncertainty uses a fresh identity; a recovered quarantined
identity serves evidence/history only.

### Replacement procedure

1. Generate independent replica-signing and endpoint keys, validate the full
   descriptor, and reject current or historical cross-role key reuse.
2. An ordinary predecessor-state administrator quorum stages the proposed
   descriptor. The active set and `W` remain unchanged.
3. The candidate installs a verified governance snapshot, authority-transition
   proof, governance tail, historical replica-policy evidence, and the retained
   state of every governed stream through named certified checkpoints. It
   verifies all bodies and publication evidence and commits them under the
   governed durability class. Reconciliation equality without stable storage
   is not readiness.
4. The candidate signs a bounded readiness manifest naming the exact staging
   governance head, replica component root, descriptor, class, `W`, governance
   cursor, and per-stream checkpoint commitment. The manifest is an auditable
   assertion, not proof of honest storage or global freshness. Administrator
   approval remains the authority.
5. An ordinary predecessor administrator quorum approves and commits a typed
   `replica.handoff.prepare` reservation. It changes no replica status or weight,
   but fixes the exact active-set transition, every applicable activation-
   readiness input, the cancellation alternative, and frontier-derivation
   rules. Until one child commits, every unrelated ordinary governance successor on that branch is
   invalid; recovery-authorized `fork.resolve` invokes a current-`W`, selected-
   administrator-approved fork-frontier reconciliation without changing
   `admin_seq` or granting recovery keys replica weight. If another resolution
   commits first, its fixed-size structural count/root rolls every prior
   unresolved reservation, closure, governance-retained statement/control
   commitment, and applicable exclusion into the one latest reconciliation.
   Exact-replay statements add signer-held roots; collection produces the final union root,
   and that latest child verifies its complete chunked proof and consumes all
   dependencies atomically.
6. `W` replicas eligible under the authenticated predecessor policy and the
   prepare's governance-carried control exclusions sign one exact-replay
   prepare-bound checkpoint-fence statement. Their canonical bundle derives the
   final frontier and prepared active-set `replica.set` child, which then
   obtains its predecessor administrator threshold. Requests without the committed approved prepare
   cannot fence a signer. The prepare and statements precede the child; the
   child commits the bundle ID and final terms, so no certificate identifier
   occurs in its own preimage. This
   intersects every predecessor checkpoint quorum and, on one selected,
   non-forked governance lineage, prevents disjoint old/new quorums from
   certifying different bodies at one generation without an objective
   individual checkpoint-double-vote or vote/frontier contradiction.
   Certificates under unresolved sibling outcomes instead invoke the fork-
   frontier control and are not automatically signer faults. If
   predecessor `W` is unavailable, the change remains uncommittable and any
   exposed fence stays closed pending the exact child/cancellation. A join or
   replacement candidate remains staged; pure leave has no staged candidate.
   Checkpoint service cannot bypass the handoff or lower `W`.
7. Committing that prepared and approved `replica.set` transition changes the
   old seat from `active` to `disabled` and the staged successor from `staged` to `active`,
   while committing the complete successor policy and explicit `W`. A join or
   planned leave uses the same complete-policy and handoff validation, with
   staged/readiness/new-identity checks applied only to activation deltas and
   disabled-signer cutovers applied only to removal deltas. Every resulting active set
   satisfies `3 <= R <= 7`, and `floor(R/2) + 1 <= W <= R`. Quorum intersection
   is required so two conflicting same-context certificates cannot be formed by
   disjoint signer sets without any individual equivocation. The #134 default
   remains the minimum majority; no failure path lowers `W` implicitly.
8. Every newly activated successor waits until the transition entry, new
   component root, endpoint binding, storage readiness, and receipt/checkpoint
   issuance fence are durably installed locally. Only then may it contribute
   new weight. The old replica stops current work once it observes the transition. Pending work
   completes entirely under the old context or is validated and certified
   again under the new context.

Each predecessor signer permits one in-flight handoff per active checkpoint-
policy lineage and returns exact bytes on replay. The committed prepare keys
signer state above child proposals and prevents unrelated ordinary governance
churn from staling a completed cancellation attachment. A discovered governance
fork instead supersedes losing fence statements through a current-`W`, ordinary-
admin-approved fork-frontier control before work resumes. Its signer statement
is outcome-neutral across the deterministic fresh-base and resume targets, so
competing target proposals cannot split first-arrival locks. Abandonment uses a
separately administrator-approved cancellation child derived from the prepare
and a predecessor-`W` checkpoint-control bundle; committing it advances the head and
makes sibling activation proposals stale on the selected branch. A detached
cancellation, timeout, or restart never reopens the old checkpoint policy.

Fold-time manifest and child validity depend only on authenticated bytes and
the governance lineage. The manifest becomes stale when an authenticated bound
staging context, descriptor, component/checkpoint root, class, policy, `W`, or
governance-carried disposition changes outside the permitted bridge. The exact
prepare is the ordinary bridge from that staging head: it commits the manifest
and changes no replica policy, so the prepare and activation's declared
successor changes do not stale their own predecessor-bound manifest. One or
more selected nested `fork.resolve` entries plus their mandatory rolled-up
current-`W`, selected-admin-approved fork-frontier reconciliation are the only
exceptional bridge, and only when the original staged head remains proven as
their ancestor and every semantic manifest input/root other than the named
chain's governance sequence/head progression remains unchanged; otherwise a
fresh manifest is required. The reconciliation entry becomes the prepare's
latest base.

A detached local readiness withdrawal, corruption report, quarantine, or
evidence-arrival order never reinterprets already exposed governance bytes. It
immediately stops local service/proposal/approval/collection and drives the
governed cancellation or disablement path. Signers collecting that prepared
cancellation re-evaluate their own signer/store journals, not the candidate's
live readiness, so withdrawal cannot strand an exposed fence. If a valid
activation child commits first, the local not-ready overlay survives and that
identity issues nothing until ordinary governance disables/replaces it. Neither
the reconciliation nor the old replica key, an old-to-new cross-signature,
endpoint possession, or an operator label authorizes the successor.

An active count or `W` change is always explicit. A non-default `W` is a
governed security choice, not a recovery tactic. If the remaining eligible
replicas cannot meet `W`, publication and checkpoint completeness stop with a
typed quorum-unavailable state while administrators stage and approve a safe
replacement.

### Equivocation policy

The specific penalty is **permanent exclusion and replacement**, not economic
slashing and not review-only handling.

Checkpoint equivocation exists only when two strict, independently verified
checkpoint bodies:

- have different checkpoint IDs;
- occupy the same frozen `(CommunityId, StreamId, checkpoint_generation)`
  anti-equivocation slot;
- carry valid signatures from the same `ReplicaId`; and
- have authenticated historical policy evidence showing that signer eligible
  in both named contexts.

The same penalty also applies to `checkpoint_frontier_equivocation`: one valid
eligible checkpoint vote and one valid same-signer prepare/fork-resolved fence,
handoff/cancellation, or cutover statement whose signed journal/frontier
assertions cannot both be true. This
captures the objective fault that a cross-policy quorum intersection can expose;
a malformed control or unsigned/local high-water disagreement does not.

The stream-checkpoint schema owner must make `checkpoint_generation` use an
exact authenticated successor allocation, checked for overflow, and non-reusable
for a retained `ReplicaId` across retention, governance, and replica-component
transitions. Each signer durably journals its exact vote, signature, generation
high-water, and idempotency state before release. Handoff, cancellation,
abandon, conflict-frontier, and cutover controls likewise persist their exact
bytes, frontier/allocation, fence, and idempotency state before release. A lower generation received
after a higher one, duplicate delivery, different generations, invalid
signatures, withholding, an invalid event, and RBSR `view_equivocation` are not
checkpoint equivocation.

On proof, an implementation preserves both exact signed records and their
historical policy evidence and immediately quarantines that `ReplicaId` from
all **new** receipt and checkpoint quorum decisions. It never lowers `W` or
rewrites the governed set locally. The operator-visible incident remains open
until an ordinary administrator-quorum transition permanently disables the old
identity and, when needed, activates an independently keyed ready replacement.
No old-key governance approval or named consent is required. The old replica
is not solicited locally after quarantine. Fold-time validity of an already
exposed predecessor-`W` statement is nevertheless derived only from the exact
authenticated policy and the committed prepare's control-signer-exclusion set,
never local evidence arrival order. An incident-bound prepare excludes the
faulted signer deterministically; if configured `W` then cannot be met, the
transition remains unavailable rather than bypassing or lowering the handoff.

Previously retained receipts and certificates remain historical artifacts
under their exact governing context; exclusion is prospective. A checkpoint in
the conflicting slot cannot support a current `CompleteThroughCheckpoint`
claim. If conflict reached certificate processing, the stream enters
`CheckpointEquivocated` until current `W` first certifies a conflict-slot
frontier/retired-signer cutover and then an exact-successor recovery checkpoint
commits a freshly verified retained-set root. A retained stream-specific pair
defines the exact conflict start; when later bodies exceed the hard incident
cap, the retained primary signer proof authorizes a conservative current-`W`
range closure instead of another permanent pair. Every handoff
that disables an active signer also carries per-stream cutovers. They make later
artifacts under covered predecessor contexts bounded historical/incident
material rather than a way to reopen completeness one slot at a time.
Governance excludes a signer; it does not choose which content root is true.

Receipt equivocation is the separate existing predicate: the same
`(CommunityId, ReplicaId, receipt_sequence)` signed over two different receipt
bodies. It uses the same quarantine and replacement response. Mere out-of-order
receipt delivery is not equivocation.

### Recovery and retirement

A disabled `ReplicaId` is permanently tombstoned and can never become active or
staged again. Endpoint-only rotation is the continuity mechanism when the
replica signer, qualified store, receipt sequence, and single-writer invariant
remain intact. A signing-key change always uses a new `ReplicaId` and begins a
fresh sequence namespace.

The same `ReplicaId` may resume after an ordinary crash only when its exact
durable signer/store state recovers in place, integrity and class-readiness
checks pass, and no concurrent writer existed. The continuity set includes the
committed receipt high-water; every per-stream checkpoint vote/generation and
issuance-fence journal; every prepare, handoff, cancellation, fork-resolved
statement/frontier, nested-resolution dependency reservation, conflict, and
cutover record, including each structural/final count/root and complete proof;
and the evidence-intake/incident/quarantine fail-stop state. The
exact current governance context must be non-forked with no pending
`fork_frontier_reconciliation_required` or unresolved closure, and the governed
active identity must be locally non-quarantined, free of mandatory-retirement
causes, and otherwise operationally eligible. A quarantined or mandatory-
retirement identity may recover only the bounded evidence/history lane.
Restoring a possibly old backup, discovering a lower or uncertain high-water,
or losing any atomic receipt/checkpoint/control/idempotency state requires
replacement. Querying peers and choosing a larger-looking counter or generation
cannot prove that no unseen higher artifact was signed.

Compromise, checkpoint or receipt equivocation, sequence rollback, and an
unrepairable durability breach permanently retire the signing identity. A
planned signing-key rotation uses the same permanent retirement rule. Public
keys, descriptors, transition evidence, historical replica policies, and
signed artifacts remain available for verification after secret erasure.

### Authority and evidence

Stage, staged abandonment, join, leave, endpoint update, quorum-only
reconfiguration, exclusion, and replacement use the ordinary administrator
threshold derived from the exact predecessor governance state. If governance is
forked/incomplete or an unrelated handoff prepare is open, those ordinary
operations fail closed; recovery authority first resolves a governance fork
through `fork.resolve`. Every accepted stable-v2 resolution then
deterministically creates a fork-frontier reservation; folding cannot depend on
whether detached replica artifacts were observed. Current `W` supplies one
outcome-neutral fork-resolved fence bundle and the selected branch's
administrators commit its typed exact-closure reconciliation child before any
replica transition or checkpoint work. A later resolution rolls all earlier
unresolved reconciliation reservations, closures, governance-retained
statement/control commitments, and exclusions into its structural count/root.
Signer statements add their held-set roots and collection commits the final union root; only the one latest
child, after final complete chunked proof verification, clears and consumes all
dependencies atomically. Recovery authority never counts toward either quorum, and this
process never scopes or reinterprets `admin_seq`.

Cryptographic incident evidence is stored in an authoritative retained v2
evidence core containing the exact canonical signed artifacts, signatures, and
conflict slot. Its identity excludes historical eligibility-witness packages,
because equivalent valid governance packages may retain different approval/
proof subsets. One or more independently verified witness attachments prove
eligibility at the named heads/roots. One bounded signer incident links that
immutable primary evidence ID to the eventual replacement, per-stream cutovers,
and recovery without changing the proof or its ID. Incidents deduplicate by
community and signer. One canonical pair is retained; at most eight exact
artifact bodies and one witness per body are retained, while excess verified
artifacts update only a constant-size digest/count. The primary pair proves the
community-wide signer fault and is sufficient for current `W` to cut that
signer over conservatively on every retained stream; no unbounded per-stream
proof archive is implied. Verification
closes the local issuance fence before persistence. If the authoritative
evidence/quarantine commit fails, the community fails closed for new receipt and
checkpoint decisions until incident state is durably restored. A preallocated,
independently reserved, maximum-size evidence-intake slot stores each bounded
candidate package before verification. Invalid candidates clear it; verified
packages clear only after one authoritative transaction commits the incident,
quarantine, every affected per-stream recovery/cutover marker, and current
receipt/checkpoint reconfirmation state. A non-empty/unreadable slot
blocks restart issuance and enables exact replay, while an ordinary crash with
an empty healthy slot does not invent a suspect signer. The local
`audit.ndjson` file records only minimized incident metadata. It is best-effort,
local, and non-tamper-evident and is never the equivocation proof.

Historical receipt or checkpoint authorization requires the exact replica
policy, `W`, class, status, and governance/component context that applied when
the artifact was signed. #161's compact authority proof can omit ordinary
replica operations, so relevant governance entries and witnesses cannot be
compacted until a separately frozen historical replica-policy proof preserves
that information. Permanent same-role and cross-role tombstones likewise need
an authenticated retained history or successor accumulator before old records
can be removed from snapshots.

## Compatibility and implementation boundary

This decision is pure specification and changes neither the shipped v1 runtime
nor the unused Phase-B candidate. In particular, it does not change v1
`admin_seq`, the fold-level advisory divergence detector, or any approved
release claim.

Phase C must add, with independent review and additive vectors:

- a successor full descriptor and replica-policy schema carrying lifecycle
  status, authenticated class, explicit `W`, and permanent tombstone history;
- successor genesis, committed prepare reservation/latest-base state,
  active-set-transition/handoff attachment, governed handoff cancellation,
  fork-resolved frontier/reconciliation control, governance-carried signer exclusions,
  fixed-size nested-dependency commitments plus complete chunked proofs,
  state-root, snapshot, and `CommunityId` fixtures;
- a bounded staged catch-up handshake and readiness-manifest format;
- replica receipt and stream-checkpoint vote/certificate formats, including
  their exact conflict slots, durable vote journal, cross-policy handoff/fence,
  checkpoint vote/frontier fault evidence, checked generation succession,
  conflict-slot controls, retired-signer cutovers, and recovery semantics;
- hard-bounded signer incident evidence, retired-signer cutovers, bounded
  evidence-intake slot, and historical replica-policy proofs;
- store, issuance-fence, crash/rollback, and convergence behavior; and
- the operator CLI/API, stable error codes, JSON output, and audit hooks.

Candidate `active|disabled`, descriptor-hash `ReplicaId`, opaque capability,
single-record `replica.set`, snapshot format 1, and golden bytes remain labeled
as pre-stable evidence. They are not upgraded by documentation or selected by
stable negotiation.

## Alternatives rejected

### Disable and add in two ordinary operations

Rejected. Disable-first can make the active set violate policy; add-first can
count predecessor and successor as independent seats. It also exposes
intermediate roots that pending receipts or votes cannot safely combine.

### Activate first and catch up afterward

Rejected. An online endpoint is not a durability or retained-set readiness
proof. Granting weight before stable catch-up lets an empty or rolled-back
replica sign quorum artifacts.

### Automatically rewrite governance when evidence appears

Rejected. Evidence deterministically removes the signer from local new-quorum
eligibility, but only governance changes the authenticated active policy. A
network peer cannot inject two artifacts and unilaterally author a governance
entry.

### Recovery-key-authorized replacement

Rejected. Recovery keys select a branch after a governance fork. Reusing them
for ordinary replica operations creates a second governance authority and
weakens predecessor-state administrator approval.

### Review-only or economic slashing

Review-only handling leaves a proven equivocator eligible indefinitely.
Economic slashing assumes a token or stake system v2 does not have. Permanent
governed exclusion plus replacement is concrete, auditable, and within the
existing authority model.

### Reactivate a planned-retired signing identity

Rejected. Proving that a retired signer never issued an unseen higher-sequence
artifact or ran concurrently is generally impossible after restore. Permanent
tombstones give one simple rule; endpoint rotation preserves identity when
continuity is actually required.

## Consequences

- Replacement takes two replica-policy transitions—stage, then atomic
  activation—plus the intervening committed governance handoff reservation, and
  may stop publication when fewer than `W` eligible replicas remain.
- Every replica-policy transition changes the component root, so pending work
  must finish or recertify under one exact context.
- Operators receive a deterministic recovery path and a concrete penalty, but
  signatures still do not prove honest storage, physical independence,
  malicious intent, or compromise time.
- Stable v2 wire advertising remains blocked on Phase C codecs, vectors, and
  independent implementation; the decision does not make rc.5 or v1 a v2
  replica implementation.
