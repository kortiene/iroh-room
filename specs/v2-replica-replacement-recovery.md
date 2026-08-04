# Spec: v2 Replica Replacement, Equivocation, and Recovery UX

| | |
|---|---|
| **Issue** | #159 — `[SPEC] §25 #5: Replica replacement, equivocation, recovery UX` |
| **Refs** | #134 §§5.2, 7.2–7.3, 10, 11, 13.4, 19, 25 #5; #155–#157; #161; ADR-0004; ADR-0007–ADR-0011 |
| **Status** | Proposed normative lifecycle and operator profile. Accepted on merge; Phase C codecs, runtime/store implementation, and independent vectors remain required before advertisement. |
| **Scope** | Pure specification. No v1 runtime, Phase-B candidate, CLI, store, governance codec, receipt, or stream-checkpoint implementation changes. |

---

## 1. Decision summary

Stable v2.0 uses this replica lifecycle:

```text
operator-local provisioned
  |                         \
  | ordinary post-genesis   \ genesis-only initial admission
  v                          v
governed staged ----------> governed active
  |              activation       |
  | abandonment/rejection         | leave/replacement
  +-------------------------------+----> governed disabled
                                         (permanent tombstone)
```

The direct `provisioned -> active` edge exists only while authenticated
successor genesis creates the initial valid `R/W` policy. Every post-genesis
identity is staged before activation. A staged candidate may instead terminate
at `disabled` without ever gaining weight; disabled remains permanent.

A signing-key replacement takes two replica-policy transitions plus one
governance-visible handoff reservation:

1. an ordinary predecessor-state administrator quorum stages the new full
   descriptor without changing active `R` or `W`;
2. after stable checkpoint-relative catch-up, an ordinary predecessor-state
   administrator quorum commits `replica.handoff.prepare`, which changes no
   replica-policy status or weight; then
3. after the prepared frontier bundle, one ordinary predecessor-state
   administrator quorum atomically disables the old seat and activates the
   staged successor in a complete successor replica policy.

The predecessor and successor never count as two seats. There is no
disable-first interval, no automatic quorum reduction, and no recovery-key
shortcut. Every active policy has `3 <= R <= 7` and an explicit
`floor(R/2) + 1 <= W <= R`. The default is the minimum intersecting majority;
a higher value is an explicit governed security/liveness choice.

Any transition that changes the active signer set first commits an ordinary
predecessor-administrator-approved `replica.handoff.prepare` reservation. It
leaves the replica policy unchanged, fixes the exact prepared active-set-
transition/cancellation intent, and blocks unrelated ordinary governance successors until one outcome
commits; `fork.resolve` and mandatory fork-frontier reconciliation remain the
explicit exception.
Replicas then produce one exact-replay checkpoint-fence statement for that
installed reservation. A predecessor-policy `W` bundle derives the final
frontier and the prepared `replica.set` or cancellation child; the chosen child obtains its
own predecessor administrator threshold. On one selected, non-forked governance
lineage, its durable fence prevents the old and new policies from certifying
different checkpoints at one generation without an objective signer fault:
either checkpoint double-voting or a checkpoint-vote/frontier contradiction.
Certificates emitted under unresolved sibling governance outcomes are fork
artifacts handled by §6.3.1, not automatically signer faults. Fewer than old `W`
cannot safely activate a changed signer set; this is #134's declared liveness
boundary.

The specific checkpoint-equivocation penalty is:

```text
verified conflicting signatures
  -> immediate local quarantine from new quorum decisions
  -> permanent administrator-governed disablement
  -> independently keyed staged/ready replacement when a seat is needed
```

There is no token or stake to slash. Evidence does not author governance by
itself, and review cannot restore the equivocating identity. `W` is never
silently lowered because a signer is quarantined.

Current `W` also certifies a per-stream retired-signer cutover during recovery or
replacement. One bounded signer incident then absorbs all later old-context
artifacts from that key; they cannot reopen historical completeness repeatedly.

Evidence verification first commits one size-bounded community incident barrier
with the durable quarantine. That barrier immediately excludes the signer and
makes every stale stream/certificate projection fail closed; it never waits for
an all-record transaction. Fixed-size per-stream and publication-certificate
markers are then materialized idempotently in resumable transactions capped at
both 256 incident-by-subject projection units and 1 MiB. Far-behind rows advance
through intermediate generations across capped transactions. A materialization
backlog cannot re-include the signer, lower `W`, or make stale completeness/
durability appear current.

An exact in-place crash recovery may preserve `ReplicaId` only when its
qualified store, signer, receipt high-water, per-stream checkpoint vote/
generation journals, authoritative incident barrier/quarantine/direct-trigger
state including trigger-subject saturation, and single-writer state recover
intact. Derived materialization progress
may instead be discarded and conservatively rebuilt behind that recovered
barrier. Restored or
uncertain receipt/checkpoint monotonic state, signing-key compromise,
receipt/checkpoint equivocation, or unrepairable durability breach requires a
fresh `ReplicaId`. Every disabled signing identity is permanently tombstoned;
endpoint-only rotation is the continuity mechanism.

The words MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, and MAY are normative.

---

## 2. Scope and terminology

### 2.1 In scope

This profile defines:

- governed join, leave, stage, activation, replacement, disablement, and
  convergence semantics;
- the semantic successor replica policy required to carry full descriptors,
  lifecycle state, durability class, and authenticated `W` atomically;
- checkpoint-relative stable catch-up and readiness requirements;
- exact semantic predicates for checkpoint and receipt equivocation;
- local quarantine, governed exclusion, and checkpoint recovery behavior;
- same-key crash recovery versus mandatory new-key replacement;
- historical evidence and non-compaction requirements;
- operator commands, outputs, failure codes, and audit record shapes; and
- Phase C implementation, vector, distributed-test, and independent-review
  gates.

### 2.2 Out of scope

This profile does not implement or freeze the final canonical bytes for:

- the successor genesis, replica policy, descriptor, governance operation,
  snapshot, readiness manifest, incident evidence, or historical-policy proof;
- replica receipts, publication certificates, stream-checkpoint bodies/votes/
  certificates, or their signature envelopes;
- the staged catch-up network handshake or service runtime;
- physical replica placement, trusted storage hardware, proof of retrievability,
  economic stake, or arbitrary-public-replica Byzantine consensus;
- exact-head publication progress during ordinary governance churn;
- private ticket/capability bootstrap beyond the staged authority described
  here; or
- any change to v1, its immutable room administrator, `admin_seq`, advisory
  fold-level divergence detector, or approved release records.

The public-artifact semantic requirements below are stable-wire inputs. Each
owning Phase C spec must assign exact canonical-CBOR fields, versions, bounds,
domains, and positive/negative vectors before interoperability advertising.
Requirements explicitly identified as local operational state—including the
evidence-intake slot, quarantine overlay, community incident barrier, checked
incident/subject/catalog generations, trigger-subject caps/saturation, direct-
trigger records/cumulative subject aggregates, materialization cursor/phase,
per-row projection markers and stale-pair index, bounded overflow audit metadata,
and transaction accounting—are not wire, governance-fold, state-root, or
snapshot inputs. Phase C
freezes those as local store-schema, crash, and property-test conformance; any
serialized fixtures for them MUST be labeled non-wire.

### 2.3 Terms

**Governed replica policy**
: The complete authenticated replica component at one governance head,
  including every current record, lifecycle status, full descriptor, governed
  durability class, explicit `W`, and the commitment needed to enforce retired
  role-key history.

**Seat**
: One `active` `ReplicaId` counted once toward active count `R` and potentially
  once toward a valid receipt or checkpoint quorum. Endpoint connections,
  staged or disabled records, processes, disks, and signatures do not add
  seats.

**Staged replica**
: A governance-named candidate that may authenticate only to a bounded
  catch-up/preflight lane. It has zero receipt, checkpoint, publication, and
  quorum authority.

**Readiness manifest**
: A bounded, candidate-signed assertion binding stable catch-up evidence and
  local storage readiness to one exact staged descriptor and predecessor
  policy. It informs administrator approval; it is not governance authority or
  cryptographic proof of honest storage.

**Activation**
: The one atomic full-policy transition that changes a staged record to active.
  Replacement activation also changes exactly the predecessor seat to disabled
  in the same post-state.

**Replica-policy handoff**
: A predecessor-policy `W` bundle of durably fenced statements over one
  committed `replica.handoff.prepare` intent. It derives exact per-stream
  checkpoint-frontier terms that a later administrator-approved child commits.
  It grants no governance authority; on one selected, non-forked governance
  lineage it prevents disjoint predecessor/successor quorums from certifying
  incompatible checkpoint bodies at one generation. Sibling-outcome
  certificates remain governance-fork artifacts handled by §6.3.1.

**Quarantine**
: A local fail-closed overlay derived from verified incident evidence. A
  quarantined signer remains in governed history until an approved transition,
  but contributes no new local receipt/checkpoint quorum decision. Quarantine
  never rewrites `W` or the governance state.

**Community incident barrier**
: A fixed-size durable local marker installed atomically with one verified
  signer incident and its quarantine. It carries a checked community incident
  generation and makes every operational stream/certificate projection from an
  older generation stale. Stale state is synchronously evaluated or reported
  unavailable/reconfirmation-required; it can never retain a prior green
  completeness/durability result. The barrier is not governance, a fold-time
  signer exclusion, a recovery certificate, or a retired-signer cutover.

**Incident materialization**
: The idempotent local projection of the current community incident generation
  and its barrier set into fixed-size per-stream recovery/cutover markers and
  publication-certificate eligibility/reconfirmation state. Work proceeds under
  projection-unit and byte caps with a durable resumable cursor.
  Materialization is derived operational state; its progress neither authorizes
  governance nor clears quarantine or recovery.

**Permanent tombstone**
: Authenticated history proving that a disabled `ReplicaId` may never be staged
  or active again in the community. Public verification material remains even
  after its secret is erased.

**Checkpoint anti-equivocation slot**
: The tuple `(CommunityId, StreamId, checkpoint_generation)`. One retained
  `ReplicaId` may sign at most one distinct checkpoint identity in a slot,
  across governance, retention, endpoint, and replica-component changes.

**Conflict-slot closure**
: A current-`W` frontier control that consumes the entire affected checkpoint
  slot and any authenticated conflict-dependent descendants, not merely artifact
  IDs known at proposal time, then allocates the exact recovery successor.

**Retired-signer cutover**
: A current-`W` per-stream control, carried by conflict recovery or the governed
  replacement/leave handoff, that makes every later artifact from one disabled
  signer under its superseded predecessor contexts bounded historical material
  rather than current completeness authority. An objective fault links it to the
  one signer incident; a planned retirement links it to transition provenance.

**Current operational decision**
: Local receipt/checkpoint authoring or quorum use, completeness display, or
  recovery service attempted after the node has verified incident evidence.
  It excludes the pure deterministic fold/validation of already exposed
  governance inputs. Historical signature and policy validity may still be
  displayed separately.

**Fold-time signer eligibility**
: The pure predicate used when a `W` attachment gates a governance child or
  state root. It derives only from the exact authenticated predecessor policy
  and an explicit governance-carried control-signer-exclusion commitment in the
  installed prepare/reconciliation reservation. Local quarantine, reachability,
  storage readiness, and evidence arrival order never change whether already
  exposed signed bytes count during fold. They do control what an honest local
  signer/collector will author or use operationally.

**Fork-reconciliation dependency commitments**
: Three phase-specific fixed-size `(dependency_count, dependency_root)` pairs.
  The resolution reservation/immutable plan carries the governance-derived
  **structural** set. Each exact-replay signer statement separately carries
  that signer's additional **held** set without claiming global completeness.
  Collection canonically unions structural, selected-signer-held, and verified
  supplemental leaves; only the resulting frontier/child carries the **final**
  complete pair. Leaves identify unresolved reservations/closures, selected or
  losing governance contexts/prepares, governance-carried control exclusions,
  and retained signer statement/control commitments. No signed body contains an
  inline list that grows with nested resolutions. Every pair verifies against
  its corresponding complete chunked proof under §6.3.1; a root is not
  permission to truncate or accept an unavailable dependency.

---

## 3. Successor governed replica policy

### 3.1 Compatibility boundary

The current Phase-B candidate is not this policy. It has:

```text
ReplicaStatus = active | disabled
ReplicaDescriptor = { replica_id, endpoint, capability }
ReplicaSet payload = { replica, status }
apply(replica.set) = one-record map insert/replacement
```

It has no authenticated `W`, durability class, staged state, readiness
commitment, transition reason, incident reference, or permanent-history
commitment. Its `ReplicaId` helper also hashes candidate descriptor bytes rather
than wrapping the stable raw Ed25519 key. Snapshot format 1 deliberately
preserves those records byte-for-byte.

Stable v2.0 therefore requires an explicitly versioned successor family. The
schema owner MUST NOT:

- add `staged` to the candidate enum in place;
- overload `capability` with `local_sync_group_v1`, `W`, or lifecycle flags;
- reinterpret a descriptor-hash identifier as a signing public key;
- change the candidate single-record payload while retaining its version;
- filter an invalid record and recompute a smaller active set or `W`; or
- rewrite any frozen candidate vector.

Successor genesis bytes produce a successor `CommunityId`; a candidate
community cannot retain its identifier through semantic reinterpretation.

### 3.2 Logical policy contents

The successor wire owner may choose names and compact encodings, but one
authenticated replica component MUST semantically contain:

```text
SuccessorReplicaPolicy {
  profile_version
  receipt_checkpoint_quorum: W
  records: sorted unique ReplicaId -> ReplicaLifecycleRecord
  retired_role_key_history_commitment
}

ReplicaLifecycleRecord {
  full_descriptor
  status: staged | active | disabled
  status_provenance
}
```

The only status-admission/transition edges are:

```text
genesis-only initial admission: provisioned -> active
post-genesis admission:         provisioned -> staged
successful activation:          staged -> active
abandonment/rejection:           staged -> disabled
leave/replacement:               active -> disabled
```

An endpoint-only update preserves status. `disabled` has no outgoing edge.

The full descriptor carries every #134 §11.2 meaning:

- raw validated replica signing public key (`ReplicaId`);
- a versioned endpoint profile resolving one validated Iroh `EndpointId`;
- discovery and relay hints;
- supported protocol versions and features;
- a region/operator label with no authorization meaning;
- governed durability class; and
- maximum retention commitment.

For the mandatory v2.0 receipt profile, every receipt-eligible active record
uses the exact `local_sync_group_v1` class. Unknown classes fail closed. Class
equality is exact; no "stronger counts as weaker" ordering is inferred.

`status_provenance` commits enough typed information to audit why a lifecycle
change occurred. At minimum it distinguishes:

```text
initial | join | planned_replacement | planned_leave | endpoint_update
endpoint_key_compromise | key_loss | key_compromise
sequence_rollback | checkpoint_journal_rollback | durability_breach
checkpoint_equivocation | checkpoint_frontier_equivocation
receipt_equivocation | operator_reconfiguration
stage_abandoned | readiness_failed | staged_authority_violation
```

An equivocation cause requires the authoritative incident-evidence identifier.
A rollback cause records the disposition of the old sequence namespace. A
planned transition does not fabricate cryptographic evidence.

The exact status-provenance encoding may live in the transition rather than
each current record, provided authenticated historical policy proofs retain it
and the current policy preserves the permanent tombstone.

### 3.3 Policy invariants

Every decoded successor policy is validated as one unit before it can grant any
networking or quorum authority:

1. records are canonical, sorted, and unique by raw `ReplicaId`;
2. each active or staged descriptor passes #157's exact replica-key, endpoint-
   resolution, current role-disjointness, and historical cross-role rules;
3. active `ReplicaId`s and active `EndpointId`s are separately unique and their
   complete sets are disjoint;
4. a key previously admitted in the other role is rejected, including after
   retirement;
5. a disabled `ReplicaId` is never staged or active again;
6. active count satisfies `3 <= R <= 7`;
7. `W` is explicit and satisfies `floor(R/2) + 1 <= W <= R`, so any two
   same-policy quorums intersect in at least one `ReplicaId`;
8. staged and disabled records add no seat or quorum weight;
9. every receipt-eligible active record supports the governed v2.0 class and
   negotiated stable protocol suite;
10. `W`, class, statuses, descriptors, and tombstone commitment all contribute
    to the replica component root;
11. a failure in any active or staged record rejects the complete operational
    profile—there is no filtering and quorum recomputation; and
12. unresolved or incompletely known governance makes the profile unusable for
    activation, receipt issuance, and checkpoint voting.

The protocol default is `W = floor(R/2) + 1`. The intersecting lower bound is a
safety rule: if `2W <= R`, disjoint signer sets could certify two conflicting
same-context checkpoints without any signer violating the single-vote rule,
leaving no §7 evidence predicate to identify a faulty seat. Tooling MUST warn
when a governed transition selects a higher non-default `W`, show the changed
liveness/compromise assumption, and require that value in the exact approved
post-state. No recovery code derives a lower value from reachability,
quarantine, or storage health.

### 3.4 Complete-set successor `replica.set`

The stable successor operation retains the semantic name `replica.set` but uses
a new governance schema/profile version. It carries or commits:

- exact predecessor governance head and replica component root;
- the complete sorted successor policy, not a patch interpreted against local
  state;
- the explicit predecessor and successor `R/W` values;
- a closed `transition_kind` identifying `stage | stage_abandon | join | leave |
  replace | endpoint_update | quorum_only`, plus the exact derived lifecycle
  deltas (`activate`/`disable`) where applicable;
- readiness-manifest identifiers for every newly activated replica;
- canonical checkpoint-handoff terms/commitment whenever the active `ReplicaId`
  set changes; the child commits the resulting prepare-bound predecessor-policy
  `W` certificate and its derived frontier;
- the matching committed `replica.handoff.prepare` reservation identifier for
  every active-set change;
- a canonical control-signer-exclusion commitment, empty for ordinary planned
  changes and incident-bound when authenticated transition evidence requires a
  retiring signer not count toward the handoff; this never lowers `W`;
- incident identifiers required by an equivocation cause; and
- the declared successor governance/state/component roots.

Before any such active-set `replica.set`, Phase C adds a typed
`replica.handoff.prepare` governance operation. Its ordinary predecessor-admin
approval commits the exact governance predecessor, readiness identifiers for
every staged record it will activate, applicable incident identifiers, complete
proposed successor policy, canonical handoff terms, and canonical cancellation
frontier terms. Folding it leaves the replica policy unchanged but
opens one governance-visible reservation for the active checkpoint-policy
lineage:

```text
ActiveHandoffReservation {
  original_prepare_id
  reservation_base_head = original_prepare_id
  active-set-transition/cancellation intent commitment
  control_signer_exclusion_commitment
}
```

From the current `reservation_base_head`, canonical derivation permits exactly two
successor operation kinds from a valid predecessor-`W` prepared-frontier bundle:
the prepared active-set `replica.set`, or a distinct typed
`replica.handoff.cancel` with unchanged replica policy and the checkpoint-
control attachment required by §6.3. Neither is encoded as a local flag.

The prepare's control-signer exclusion set is a canonical subset of active
predecessor `ReplicaId`s and is validated from its typed transition cause and
committed incident/rollback/compromise evidence. Ordinary planned join/leave/
rotation uses the empty set unless its cause independently requires exclusion.
Fold-time handoff eligibility is exactly `active predecessor policy - committed
control exclusions`; it never consults verifier-local quarantine, readiness,
reachability, or arrival order. Exclusion does not change configured `R` or `W`:
the remaining protocol-eligible set must still supply all `W` signatures.

While a reservation is open, validation rejects every unrelated **ordinary**
governance successor on that branch. The existing recovery-authorized
`fork.resolve` is the sole exception and follows §4.6; this exception does not
change, scope, or reinterpret `admin_seq`. A required
`replica.handoff.fork_reconcile` is allowed only from the resolution-created
reconciliation state; when it preserves a selected open prepare, its own entry
ID becomes that reservation's new `reservation_base_head`. Prepared
`replica.set` and cancellation children set `prev` to the latest base, repeat the
original prepare ID/intent, derive frontier fields from one valid `W` bundle
rather than caller input, and each requires the exact administrator threshold
from that base state.
An uncommitted sibling prepare cannot
solicit a signer fence. A committed sibling can remain hidden long enough to
collect one on its branch, so selecting a branch invokes §4.6's mandatory
fork-frontier reconciliation. Signer state is keyed above child proposals, so
competing children of one prepare cannot split it. This committed reservation
prevents first-arrival child locks and prevents a complete cancellation
attachment from becoming stale behind unrelated ordinary governance churn.

The exact successor policy is recomputed and validated before signature
counting or commit. Authorization uses distinct administrators and the threshold
from the exact predecessor state, following the ordinary governance rules. The
new administrator set, new replica policy, a staged or old replica signature,
endpoint possession, readiness witnesses, and recovery keys contribute zero to
that approval unless the same principal independently belongs to the old
administrator set and signs through the governance approval path.

A full-state payload prevents hidden intermediate semantics; it does not permit
an entry to erase historical records. Disabled records or an authenticated
append-only tombstone commitment remain part of the accepted post-state.

### 3.5 Permanent history and bounds

Current active count is bounded, but rotations can create unbounded historical
records. Until a successor authenticated key-history accumulator and proof
format are frozen, implementations MUST retain exact disabled descriptors and
their governance transitions and MUST NOT advertise compaction that discards
them.

A later accumulator may move old records out of the current snapshot only if
it provides deterministic inclusion/non-inclusion proofs for:

- every historical `ReplicaId` and `EndpointId` role assignment;
- permanent disabled-`ReplicaId` tombstones;
- the exact descriptor, status, class, `W`, governance head, and component root
  needed to validate every retained receipt/checkpoint; and
- every incident and transition still referenced by retained artifacts.

The accumulator is a Phase C schema/vector obligation, not permission to trust
a local database flag or lossy Bloom filter.

---

## 4. Lifecycle and authority

### 4.1 Provisioning

Before governance staging, the operator:

1. generates independent replica-signing and endpoint secrets using a CSPRNG;
2. validates canonical public keys and proves possession through separate
   purpose-bound preflight challenges;
3. builds the complete proposed descriptor, including class and retention
   commitment;
4. checks all locally available current/historical role-key and tombstone
   evidence;
5. provisions qualified storage and a sole receipt/checkpoint signer with
   atomic monotonic journals; and
6. records no secret, ticket capability, raw key material, or local path in
   plan output or audit logs.

Provisioning is not governance. A locally provisioned process receives no
community data or protocol authority merely because an administrator controls
the host.

### 4.2 Stage

An old-admin-quorum successor `replica.set` adds the validated descriptor with
status `staged`. It names the intended action (join or replacement target) and
leaves the active set and `W` unchanged.

After installing that governance state, active replicas may admit the exact
staged descriptor to a bounded catch-up lane. The lane requires the same two-key
live binding as an active replica, bound to the staged component root, but
authorizes only:

- public governance genesis/snapshot/checkpoint/transition-proof/tail evidence;
- historical replica policies, role-key history, and incident evidence needed
  for verification;
- stream checkpoint/certificate and retention evidence;
- RBSR and bounded event/publication-certificate transfer for retained state;
- declared archived-segment manifests and referenced protocol evidence; and
- readiness challenge/status exchange.

It does not authorize receipt issuance, checkpoint proposal/voting, ordinary
client publication handling, subscription authority, invites, secret
capabilities, or quorum weight. All message/body/queue limits and governance-
first backpressure priorities remain in force. A stage may be disabled without
activation.

Staging changes the authenticated replica component root even though active
`R/W` do not change. Receipts and votes under the pre-stage and staged roots do
not combine. Operators SHOULD drain or recertify pending work around the stage
transition and MUST surface that root change in the plan.

### 4.3 Planned join and leave

A join stages and readies a new replica, then atomically changes it to active.
The successor active count and explicit `W` pass §3.3. If `W` changes, the plan
and approvals show that change independently from reachability.

A planned leave atomically disables an active record only when the successor
policy still has `3 <= R <= 7` and `floor(R/2) + 1 <= W <= R`. If preserving
the intended fault posture requires a replacement, the replacement is staged
and readied first. A leave never deletes historical policy or artifacts and
never maps a different key onto the old `ReplicaId`.

### 4.4 Signing-key replacement

Signing-key rotation, key loss, compromise response, receipt/checkpoint-journal
rollback, and permanent storage failure use replacement:

1. stage a descriptor with a new independently generated `ReplicaId`;
2. complete §5 readiness;
3. build and freeze a `replica.handoff.prepare` reservation against the exact
   current staged governance head/root;
4. obtain the exact predecessor administrator threshold and commit that
   reservation, which changes no replica status or weight;
5. obtain §6.3's predecessor-policy `W` prepared checkpoint-frontier bundle,
   derive the exact active-set `replica.set` child from it, and obtain that
   child's predecessor administrator threshold;
6. transactionally commit/install the prepared and approved transition that changes old
   `active -> disabled` and new `staged -> active` in one successor policy; and
7. converge and fence issuance as in §6.

The old and new keys never coexist as active seats in a governed state. The new
key begins receipt sequence zero/initial allocation according to the eventual
receipt codec; it does not inherit or alias the old sequence. No old-key
governance approval, named consent, or old-to-new cross-signature authorizes the
successor. After loss/quarantine the old replica is not solicited and an honest
holder does not author. Fold-time validity of already exposed bytes follows only
the authenticated predecessor policy plus the prepare's committed exclusions.
An incident/loss-bound prepare excludes that signer deterministically; if
configured `W` then cannot be met, the replacement stays staged under the
declared liveness boundary. Local evidence arrival never changes fold. The old secret is erased after the
appropriate incident/planned-recovery window, while public evidence remains.

### 4.5 Endpoint-only update

An endpoint-only rotation preserves the active `ReplicaId` only when the exact
qualified store, signer, monotonic receipt state, checkpoint vote/generation
journals, and single-writer invariant remain continuous. The complete successor
policy changes the endpoint profile for that record and preserves status,
`ReplicaId`, class, and `W`.

The component root still changes. Only the new endpoint is authoritative at the
new head, and pending artifacts do not combine across roots. If endpoint
compromise may have reached the separate signer/store or continuity cannot be
shown, operators use signing-key replacement instead.

An endpoint-compromise transition uses the typed `endpoint_key_compromise`
cause through the same ordinary predecessor-administrator path. It grants no
separate emergency authority.

### 4.6 Governance fork ordering

No stage, staged abandonment, join, leave, endpoint update, quorum-only change,
exclusion, or replacement is accepted on an unresolved/incompletely known
governance fork. Recovery keys
first authorize `fork.resolve` under the existing recovery procedure. The
selected branch's ordinary administrators then authorize the replica operation
using its exact predecessor state.

An open handoff prepare does not reject that recovery operation. `fork.resolve`
is the only successor other than the prepared `replica.set` or cancellation
child allowed through the prepare's
ordinary-governance serialization rule. It retains v1/v2 governance DAG depth
and the complete carrier chain exactly as specified; in particular, this profile
MUST NOT scope `admin_seq`, discard an intermediate carrier, or make replica
state choose the governance branch.

Every accepted stable-v2 `fork.resolve` deterministically sets
`fork_frontier_reconciliation_required`. Folding this state transition depends
only on the authenticated governance DAG and exact resolved closure; it MUST NOT
inspect whether detached replica votes, fences, or controls have happened to be
observed. Absence of such an artifact is not provable. The requirement clears
only when a later governance-committed current-`W`, selected-admin-approved
§6.3.1 control names and covers that exact `fork.resolve` and closure. A snapshot
or replay may show it already cleared only by retaining and verifying that child.
If another recovery-authorized `fork.resolve` commits before that child, the new
resolution deterministically rolls every unresolved fork-frontier reservation
on its selected ancestry or losing closure into one new latest reservation. The
older requirements are not cleared: their IDs, exact closures, control-
exclusion provenance, and governance-retained statement/control commitments
enter §6.3.1's structural dependency set. Detached signer-held statements enter
only their signer's held-set root and collection's final union, never the fold-
time structural root. The new reservation carries its fixed-size structural
count/root, and the one latest committed child verifies the complete chunked
final proof and consumes the rolled-up requirements atomically.
This chaining never changes or scopes `admin_seq`.
This includes a fully closed activation on a losing branch: its later checkpoint
activity is not safe merely because its prepare is no longer open. Until the
post-resolution control commits, no ordinary governance successor, activation,
cancellation, receipt quorum decision, or checkpoint vote may proceed. If the
selected state still contains an open prepare, reconciliation supersedes every
losing reservation and establishes a fresh fence base for the selected prepare;
it never rewrites or reuses a losing signature. The selected prepare remains
blocked until that reconciliation commits; afterward its new reservation base
again permits only the prepared `replica.set` or cancellation child (or another
`fork.resolve`). Otherwise reconciliation supersedes the losing
reservations and resumes at the exact certified frontier. Recovery keys select
the governance branch; they
never supply administrator approval, replica weight, or checkpoint-frontier
authority.

Common-ancestor governance activity and any frontier already consumed by an
earlier retained **committed** current-`W` control are excluded from the new
control's frontier inputs, but an unresolved fork-frontier reservation or
detached statement on the selected ancestry is never excluded merely because it
became a common ancestor. None of these rules suppresses the structural trigger. Closed prepares outside the
contested suffix are not repeated in the losing-context list. Later artifacts
under a resolved losing governance context are bounded historical material after
the control and cannot reopen the selected checkpoint lineage.

Replica evidence never selects a governance branch. Recovery authority never
directly supplies replica quorum weight or bypasses readiness.

---

## 5. Stable catch-up and readiness

### 5.1 Readiness is checkpoint-relative

A candidate is not globally "caught up." Readiness is a bounded claim through
named governance and stream checkpoints plus an explicitly reported
uncheckpointed tail.

Before activation, the staged replica MUST durably install and verify:

1. externally anchored community genesis evidence;
2. the current #161 governance checkpoint, snapshot, authority-transition
   proof, and exact tail through the staging governance head;
3. every current and historical replica policy/key tombstone needed by the
   retained artifact set;
4. for every governed stream whose certified retention/archive policy still
   requires state—including archived streams—a named valid `W`-certified
   checkpoint at or after its required retention boundary;
5. the exact retained event set through each checkpoint, every event body,
   author/device predecessor required in the retained interval, and every
   publication/historical-authorization proof required to validate it;
6. referenced archived-segment manifests and protocol metadata covered by the
   checkpoint/retention policy; and
7. all of the above under the stable local commit and recovery rules of the
   governed durability class.

RBSR equality, `CompleteThroughCheckpoint`, network delivery, SQLite row
visibility, an unsynchronized WAL, and a replica's assertion alone do not prove
stable readiness. The candidate independently validates every signature, ID,
governance link, policy witness, retained-set root, and stable-store boundary.

A previously certified checkpoint may anchor repair when the current active set
has fewer than `W` reachable replicas. This does not reconstruct later lost
events or claim liveness that #134 does not provide. The operator output shows
the checkpoint age and tail uncertainty.

### 5.2 Uncheckpointed tail

Events after the named checkpoint remain an explicit tail. The candidate
reconciles and stably stores every tail artifact it can verify before activation
and reports the exact observed counts/ranges, but it MUST NOT label an
uncertified tail complete.

After activation:

- the replica may accept and durably receipt a newly validated event once the
  activation and issuance context are locally stable;
- it MUST NOT propose or vote on a stream checkpoint until its retained set and
  tail satisfy the final stream-checkpoint stable-retention predicate; and
- status remains `active_tail_reconciling` rather than `ready_complete` while
  any required stream has an unresolved tail or dependency.

This permits governance to restore future quorum without pretending that
unavailable historical data was recovered.

### 5.3 Readiness manifest semantics

The candidate signs a bounded readiness manifest under a new purpose distinct
from receipts, checkpoint votes, and endpoint binding. The final codec binds at
least:

```text
community_id
staged_replica_id
full_descriptor_hash
staging_governance_seq_and_head
staging_replica_component_root
governed_class_and_W
verified_governance_checkpoint_id
per_stream_checkpoint_vector_commitment
historical_replica_policy_high_water_or_commitment
receipt_namespace_disposition = fresh
checkpoint_vote_journal_disposition = fresh
storage_readiness_state
created_at_advisory
```

The per-stream commitment covers canonical sorted entries containing stream,
checkpoint ID/generation, retention generation/boundary, retained event count/
root, and explicit tail status. The format uses hard entry/byte caps and a
separate manifest or Merkle commitment if the complete vector would exceed the
control-frame limit. Exact limits, domain bytes, and proof shape remain Phase C
wire work.

The manifest proves only that the holder of the staged key made the signed
assertion over verified identifiers. It does not prove an honest fsync,
independent hardware, physical location, newest global state, or future
availability. Administrator quorum approval is the authorization decision and
MUST display the manifest digest and readiness exceptions.

Current active replicas MAY provide signed observations of the candidate's
checkpoint/root agreement, but those observations add no governance authority
and are not a mandatory live `W` gate for the candidate's storage-readiness
assertion. Separately, an activation that changes the active signer set requires
§6.3's predecessor-policy `W` checkpoint-frontier handoff. That handoff is a
cross-policy checkpoint-safety barrier, not an attestation that the candidate's
disk is honest. If old `W` cannot produce it, #134 guarantees no liveness and
activation waits.

### 5.4 Fold-time staleness and local readiness withdrawal

Fold-time manifest/child validity is a pure function of authenticated bytes and
the governance lineage. A readiness manifest is deterministically stale when
the accepted child predecessor no longer follows the manifest's bound staging
head/component root through only the permitted bridge, or when an authenticated,
governance-ordered input changed, including:

- a governance head outside the exact bridge or a changed replica component root;
- candidate descriptor, key, endpoint, class, or staged status;
- `W`, active policy, retention generation/boundary, or required stream set; or
- a governance-carried checkpoint/incident/readiness disposition that invalidates
  or supersedes a named input.

Detached/local observations—storage-readiness withdrawal, corruption, receipt-
state fault, checkpoint journal/fence uncertainty, newly observed checkpoint
evidence, staged-key misuse, or missing historical evidence—MUST NOT change
fold-time validity by arrival order. They immediately stop local candidate/
signer service, proposal, approval, and collection. Before an active-set child
commits, administrators make the withdrawal global by committing the prepared
cancellation (or disabling an unprepared staged record). If cancellation and a
prepared child both commit, ordinary fork resolution plus §6.3.1 selects the
lineage. If an already valid child folds first, the local not-ready/quarantine
overlay survives and the activated identity issues nothing while ordinary
governance disables/replaces it. No verifier retroactively reinterprets the
child bytes.

The exact `replica.handoff.prepare` named by the plan is the ordinary permitted
bridge between the manifest's staging head and the activation child. It commits
that manifest and every bound input while leaving the replica policy unchanged,
so its own reservation head does not stale the manifest. One or more nested
`fork.resolve` entries plus the mandatory rolled-up §4.6/§6.3 fork-frontier
reconciliation may remain in that bridge only when they select the prepared
state, prove the original staged head as their ancestor, and preserve every
manifest-bound semantic input/root other than the governance sequence/head
progression caused solely by the named prepare -> resolution(s) -> exact-closure
reconciliation chain. If any authenticated policy, component, checkpoint
vector, governance-carried disposition, or other semantic input changed, the selected
prepare can only be closed and a fresh manifest/plan is required. The proposed
activation's declared successor head, component root, status, active policy,
and explicit `W` changes are validated under §6 and likewise do not stale their
own predecessor-bound manifest. Any other independently accepted intervening
change does. The operator then refreshes catch-up and issues a new manifest. A
plan never silently retargets a later head or copies the old digest into a
changed entry.

---

## 6. Atomic transition and convergence

### 6.1 Active-set transition validation

Validation has a committed reservation phase and an attachment-gated active-set
transition phase. A `replica.handoff.prepare` may fold only when its exact
governance predecessor and administrator threshold verify and its committed
intent passes every check below that is decidable before frontier collection. It
changes no replica policy, but opens the one governance-visible reservation
described in §3.4. Protocol-eligible predecessor replicas then journal one
prepare-bound checkpoint-fence statement. A `W` bundle derives the exact
frontier and prepared `replica.set` child, which obtains its own predecessor
administrator threshold. Committing that child repeats every applicable check,
including the prepared-frontier bundle:

For an active-set change, a child or approvals without the installed prepare and
valid `W` bundle are not an acceptable/foldable `replica.set`. Network ingestion
may install the approved prepare as pending governance state, but MUST NOT
install or relay the prepared `replica.set` child as committed state until the required
bundle and child approval both verify. This boundary is a successor-schema
rule; it does not reinterpret candidate `replica.set`.

1. the exact governance predecessor, installed `replica.handoff.prepare`, and any
   §4.6-selected fork-resolution/frontier-reconciliation bridge head/state/
   component roots;
2. for **each activation delta**, a current staged record matches its prepare-
   committed readiness manifest and successor descriptor byte-for-byte at the semantic boundary,
   with only §5.4's exact prepare/fork-reconciliation bridge between the manifest
   and child; a pure leave has no staged-record or manifest requirement;
3. for **each activation delta**, the manifest signature, body/hash, bound
   context, and §5.4 governance-lineage staleness predicate verify; verifier-
   local live readiness/evidence is not a fold input, and a pure leave has none;
4. the complete successor policy and every §3.3 invariant;
5. the declared lifecycle deltas match the operation: a join activates staged
   record(s) and disables none; a pure leave disables active record(s) and
   activates none; a replacement disables exactly its named predecessor seat(s)
   and activates exactly its named staged successor(s);
6. no other record/status/descriptor/`W` change is hidden from the plan;
7. transition cause and required incident/rollback evidence are present;
8. every activated identity is distinct from every disabled/replaced identity,
   and no activated identity reuses a retired key; a pure leave has no new-
   identity check;
9. the post-state/component roots recompute exactly;
10. when active signer membership changes, §6.3's exact predecessor-policy
    prepared-frontier/handoff certificate verifies against the installed
    prepare, this child's derived terms, and successor policy;
11. the exact predecessor administrator threshold approves the governance
    child; and
12. the installed prepare has its own exact predecessor administrator threshold,
    commits this exact active-set transition and cancellation alternative, and has no
    unaccounted intervening/competing successor or losing reservation on the
    selected branch.

Failure is total. No partial record insert, disable, readiness reservation,
quorum update, or secret erasure occurs. On a non-forked lineage, a committed
prepare remains visibly open until its exact prepared `replica.set` or
cancellation child commits. After `fork.resolve`, only §6.3.1's selected-admin/
current-`W` control may explicitly rebase or close it.

### 6.2 Join, leave, and multi-record changes

The full-set operation can express a join, leave, replacement, or deliberate
`W`-only reconfiguration, but the operator plan lists every semantic delta. `R`
changes only as the exact result of named membership deltas; it is never an
independent caller-supplied field. A single entry MAY change multiple seats only
when the complete successor policy and readiness evidence for every activation
validate atomically. Tooling SHOULD default to one seat change per entry so
incident attribution and rollout remain reviewable.

A `W`-only change preserves every record, status, descriptor, and active
`ReplicaId`. It uses ordinary predecessor-administrator authorization, exact
successor-policy/root recomputation, §3.3 bounds, and §6.4's component-root
issuance fence. It has no staged candidate and does not open a prepared handoff:
the unchanged active signer set and majority bounds make every old/new quorum
intersect, while the cross-context single-vote rule exposes any contradictory
intersection signer. It is unrelated ordinary governance and therefore remains
invalid while an existing handoff prepare is open.

The operation MUST NOT use temporary active aliases, count a staged key, or
infer one physical machine from a one-for-one logical replacement.

### 6.3 Cross-policy checkpoint handoff

Majority quorums intersect inside one replica policy, but consecutive policies
whose active signer sets differ need not. With old `{A,B,C}` and new `{B,C,D}`
at `W=2`, old `{A,B}` and new `{C,D}` could otherwise certify different bodies
at one generation without any signer double-signing.

After the `replica.handoff.prepare` commits and before either allowed child can
commit, at least predecessor `W` distinct fold-time-eligible replicas produce a bounded
prepared checkpoint-fence bundle bound to:

```text
community_id
prepare entry id, predecessor head, and replica component root
exact active-set transition intent and successor policy/component root
predecessor and successor R/W
governance-carried control-signer-exclusion commitment
canonical per-retained-stream journal/frontier input commitment
active-set transition outcome:
  per-stream retired-signer cutovers for every disabled active signer
    (incident id for a fault; prepared transition provenance otherwise)
cancellation outcome:
  unchanged-policy resume frontier
  no planned-retirement cutover
  only independently required incident cutovers already valid in current state
```

There is no hash cycle. The prepare commits the exact active-set-transition/
cancellation intent, successor policy, stream set, bounds, and deterministic frontier-
derivation rule, then its normal CSB derives the prepare ID. Each signer signs
that prepare ID and intent, not a later child ID. A canonical sorted bundle of
`W` statements deterministically derives the common per-stream frontier,
cutovers, and certificate ID. The prepared `replica.set` or cancellation child commits that
certificate ID and derived terms before its own CSB derives the child ID. The
child's administrator approvals therefore cover the final frontier. Historical
policy evidence retains the prepare, statements/certificate, and chosen child.
A bundle or child for a different prepare, intent, signer statement, terms, or
successor root is invalid.

A fence request without the current `ActiveHandoffReservation`, its matching
administrator-approved original prepare, and exact latest base head is rejected
without changing any signer journal or fence. Each signer rechecks that the base
is the current selected head, governance is non-forked/reconciled, the committed
prepare/policy/frontier schema is valid, and its **own** signer/store journal is
healthy. It does not re-evaluate candidate live readiness: the one outcome-
neutral statement must remain collectable to derive cancellation after a
candidate withdrawal. An honest locally
quarantined or storage-unready signer refuses to author, and a collector that
knows that state does not solicit it. Fold-time counting, however, uses only the
authenticated predecessor policy and committed control exclusions: local state
cannot retroactively invalidate or validate an already exposed statement.
Staged/disabled replicas contribute zero because that status is authenticated;
a planned-retirement active replica counts unless the prepare's authenticated
exclusion set removes it. Thus an unauthorized or merely threshold-preapproved
proposal cannot turn into a checkpoint-service fence, and evidence-before versus
evidence-after delivery cannot fork governance apply/state roots.

Installing the prepare is serialized with local checkpoint voting. Old-context
work either stable-commits before the prepare wins that fence, or stops. For
each stream required by §5.1, a signer's one exact-replay
`PreparedCheckpointFence` statement binds the original prepare ID and current
reservation base, then exposes its durable vote-journal high-water,
commitments to retained votes, last uncontested certified checkpoint, retention
context, unresolved-slot dispositions, and every required retired-signer
cutover. It also authorizes the deterministic bundle rule to close the
contiguous range through the componentwise derived frontier under either
explicitly tagged prepared outcome. Before exposing the statement, the signer atomically persists it and
fences all later checkpoint votes under the prepared predecessor policy until
an allowed child commits.

The certificate canonically aggregates `W` such statements and derives the
minimal frontier that covers every included journal without a gap. A statement
with an inconsistent or omitted journal entry is invalid; unresolved
contradictions make collection fail rather than permitting a jump. Staged/new
replicas contribute zero. Different valid `W` bundles may produce sibling child
proposals, but signer reservations remain keyed to the one prepare and never to
those children. The ordinary administrator threshold selects the exact child;
two committed siblings are an ordinary governance fork, not a split signer
lock.

Prepared active-set-transition and cancellation terms are domain-separated
alternatives. Selecting the transition applies cutovers for every signer that
child disables; a pure join has none. Selecting cancellation leaves the entire
predecessor policy unchanged and MUST NOT apply any cutover solely planned by
the uncommitted transition; only an incident cutover independently required by
the already accepted current state may survive. An unselected alternative is
not a signed assertion about current state and cannot later serve as frontier-
equivocation evidence.

Because both an old checkpoint certificate and the prepared bundle use
intersecting predecessor quorums, an honest intersection signer cannot expose a
statement below a checkpoint it already signed. The `W` statements themselves
certify closure of unresolved predecessor slots through the derived frontier.
After any prepared active-set transition, the successor set starts at §7.1's
exact next generation.
The bundle is checkpoint-safety evidence only: it does not approve the child,
prove candidate storage, or add publication weight.

Fewer than predecessor `W` cannot safely change the active signer set. The
prepared transition remains uncommittable; a join/replacement candidate stays
staged, while a pure leave has no staged candidate. After any statement is
exposed, predecessor checkpoint service stays fenced pending the exact
transition, cancellation, or §6.3.1 path. Administrators MUST NOT bypass the
handoff or lower `W`. Signing a handoff irrevocably fences the old checkpoint
policy for that proposed transition. Each signer durably permits only one
in-flight prepare per `(CommunityId, active checkpoint-policy lineage)`—the
continuing signer/generation lineage, not merely one governance head. A sibling
or first-arriving uncommitted proposal cannot acquire any signer reservation.
Replaying the prepare request returns the exact stored statement/signature. The
outcome-neutral statement can feed either allowed child, so competing child
proposals cannot strand disjoint signer subsets. The reservation closes when an
prepared `replica.set`/cancellation child commits, or is superseded through §6.3.1 after
`fork.resolve`; no other path clears it.

Cancellation is not a detached certificate that can later be replayed against
the same governance state. A valid prepared `W` bundle can instead derive a
`replica.handoff.cancel` child with unchanged replica policy and its exact next
frontier. Only after that immutable child has the predecessor administrator
threshold may it commit. Committing the child and its prepared checkpoint
control advances the governance head, closes the reservation, resumes
checkpoint service at the exact certified successor frontier, and makes any
sibling prepared active-set transition stale on the selected branch.
If the cancellation entry does not commit, the old policy stays fenced. A wall-
clock timeout, restart, detached control alone, or local rollback never reopens
it. Concurrent prepared-`replica.set`/cancellation commits are an ordinary governance fork
and use the existing fork-resolution procedure; neither branch is selected by
arrival order. While the prepare is open, every unrelated ordinary governance
successor is invalid. Only `fork.resolve` may intervene, and then §6.3.1
supersedes/rebases the affected fence before further work; ordinary churn cannot
stale a collected cancellation. Cancellation collection is itself exact-replay
idempotent and single-flight for the prepared handoff; it reuses the signers'
prepare-bound fence statements rather than acquiring child-specific locks.

#### 6.3.1 Fork-resolved frontier and losing activity

Folding every accepted stable-v2 `fork.resolve` creates a governance-visible
fork-frontier reconciliation reservation, even when no detached replica
artifact has yet been observed. It is keyed by the exact `fork.resolve` entry,
resolved closure commitment, selected checkpoint-policy lineage, and a fixed-
size structural dependency count/root derived from the complete authenticated
full DAG. That dependency set includes every prior unresolved reconciliation
reservation, closure, and governance-retained statement/control commitment on
the selected ancestry or losing closure. This applies whether a prepare is
open, closed, or absent. Common-ancestor governance activity and activity
already consumed by a retained committed control are excluded only from
frontier inputs; unresolved reservations/statements are not.
A reservation also carries one canonical control-signer-exclusion set derived at
fold solely from the selected authenticated state: the selected open prepare's
committed exclusions, any still-effective governance-carried incident
exclusions, and rolled-up prior reservation exclusions. It is explicitly empty
when none exist. A proposer, local quarantine, or newly observed detached
evidence cannot add/remove an ID. A nested resolution deterministically carries
forward exclusions still applicable to its selected policy; only authenticated
state transition makes one obsolete.
A previously committed control clears only its own exact resolution/closure; it
cannot pre-clear a later `fork.resolve`. This is not an active-set transition,
cancellation, or recovery-key checkpoint decision.

Against that exact reservation, each replica eligible under the exact selected
authenticated policy and governance-carried exclusion state produces one
exact-replay, **outcome-neutral** `ForkResolvedFenceStatement`. The statement is
keyed only to `(CommunityId, fork-frontier reconciliation reservation, selected
checkpoint-policy lineage)`: it does not name a reconciliation child or choose
a target. It commits the common structural count/root plus a fixed-size count/
root for every additional unresolved signer-journal dependency held by that
signer. Its signature authorizes the canonical bundle rule that unions those
committed inputs with independently verified supplemental leaves and derives
every valid target; it neither asserts a final complete root nor changes when a
later supplement appears. A canonical current-`W` bundle forms the sorted,
deduplicated union of the structural set, all selected signer dependency sets,
and any independently verified late supplemental leaves supplied during
collection. Supplemental leaves are bundle inputs; they never mutate an exact-
replayed stored statement or its held-set root. The bundle then reuses those
same statements to derive a target-tagged
`ForkResolvedFrontier` containing at least:

```text
community_id and fork.resolve entry id
resolved branch-head/closure commitment
structural dependency count/root
complete bundle dependency count/root
selected governance head, policy/component/class, and W
fold-time signer-set/control-exclusion commitment
selected open original prepare id/intent and latest base head, or none
selected branch's authenticated handoff/frontier chain
per-stream selected frontier and losing-statement journal high-waters
canonical contiguous dispositions through derived frontier f
target = fresh_selected_prepare_base | resume_selected_policy
```

The dependency proof is a canonical sequence of bounded leaves. A leaf contains
only typed IDs/digests, fixed-size commitments, and the disposition needed to
derive the frontier; referenced governance/control bodies remain separately
verified under their own bounds. Phase C freezes a domain-separated streaming
Merkle algorithm and exact leaf order with these stable-v2 maxima:

```text
MAX_FORK_DEPENDENCY_LEAF_BYTES = 4_096
MAX_FORK_DEPENDENCY_CHUNK_BYTES = 1_048_576
MAX_FORK_DEPENDENCY_LEAVES_PER_CHUNK = 4_096
dependency_count = uint64
dependency_root = 32 bytes
```

Each transport chunk satisfies both chunk limits. A verifier streams the whole
sequence with bounded memory, rejects a duplicate, noncanonical order, count/
root mismatch, oversized leaf/chunk, missing body, or unavailable suffix, and
accepts only after the final count/root matches. The total proof may grow with
retained governance history, but no signed/control body or individual transport
frame grows with it. Count overflow requires a versioned rollover and fails
closed. Implementations MUST NOT truncate, summarize a suffix without its
committed leaves, or treat a local time/resource abort as successful
verification.

Each current signer supplies the exact prior prepare and fork-frontier
reservation/statement leaves it holds, if any, and refuses a proposal whose
complete proof omits a losing or prior-unresolved reservation or journal
statement known locally. When the selected state has an open prepare whose
applicable authenticated bound inputs remain valid under §5.4, both tagged
targets are derivable from the same outcome-neutral statements: competing
target collections return the exact stored statement/signature and cannot
acquire signer-specific first-arrival locks. The selected administrators choose
the target by approving its derived child. Without a selected open prepare, or
when an authenticated manifest-bound semantic input/checkpoint vector changed
through governance, or a governance-carried disposition invalidated it,
`resume_selected_policy` is the only valid target and closes the prepare.

The committed bundle semantically supersedes every statement bound to each
reservation/prepare/context leaf committed by the dependency root and every
replica frontier under a committed losing governance context, including an
artifact learned later; the selected prepare intent itself remains open only
for `fresh_selected_prepare_base`. Exact retained statement/artifact leaves are
audit detail, while a context/closure leaf supplies the non-arrival-order range
coverage. A later governance branch outside the resolved closure reopens the
ordinary fork procedure and requires another resolution/control.

For each stream, derivation starts from the selected branch's authenticated
frontier and its complete valid cross-policy handoff chain. It then covers every
included losing-statement high-water and unresolved slot without gaps, choosing
the minimal `f` that satisfies those inputs and allocating only the checked
successor of `f`. A current signer already fenced to a losing prepare or prior
unresolved fork-frontier reservation may sign this special statement only after
verifying the exact latest `fork.resolve` and complete rolled-up reconciliation
reservation. Before release it atomically stable-commits every prior reservation/
statement commitment it holds, the new outcome-neutral statement,
nondecreasing frontier, and continued fence. This chains and supersedes the old
state; it does not alter old signed bytes or pretend they were bound to the
selected branch.

The selected branch's ordinary administrators approve a typed
`replica.handoff.fork_reconcile` child derived from the resolution and exact
`W` bundle. Its `prev` is exactly the accepted `fork.resolve`/reconciliation-
reservation head; another base is invalid. If
`target = resume_selected_policy`, committing both leaves the selected replica
policy unchanged, consumes/closes every selected or losing
`ActiveHandoffReservation` (including its original prepare) and every named
prior fork-frontier reservation, clears their fences, and resumes checkpoint
service only at the certified successor frontier.
If `target = fresh_selected_prepare_base`, commit leaves the selected prepare
open, consumes every named prior fork-frontier reservation, supersedes the losing
fences, installs the derived frontier, and sets the
reconciliation entry ID as its new `reservation_base_head`. Prepared
`replica.set`/cancellation children set `prev` to that latest base, repeat the original
prepare ID and intent, and never extend the old prepare head. Only after that
commit may a signer issue a fresh
`PreparedCheckpointFence` bound to `(original_prepare_id,
reservation_base_head = reconciliation_entry_id)`; no losing statement is
reused.

The reconciliation child is the only ordinary operation allowed while the
post-resolution requirement is set. Fewer than current `W`, missing branch/
statement evidence, a generation gap, or missing selected-administrator
approval leaves the community fail-closed. A recovery-key signature never
counts toward either quorum. A further recovery-authorized `fork.resolve` is
allowed and must roll this unresolved reservation forward as above. If prepared-
`replica.set`/cancellation siblings or further resolutions race, normal
governance-fork rules apply.

Every frontier-bearing signer action uses the same durable-before-release rule.
Before exposing a `PreparedCheckpointFence`, handoff/cancellation bundle
statement, `ForkResolvedFenceStatement`, abandoned-slot control,
`ConflictSlotFrontier`, or `RetiredSignerCutover` signature, the signer atomically
stable-commits the exact body and signature, the input and allocated generation/
frontier, every resulting issuance fence, the idempotency key, and the
applicable lineage/single-flight state. An exact
retry returns the stored bytes. Missing, rolled-back, or uncertain recovery of
any such record invokes §9.2; a signer never reconstructs it from a later
network view or signs a substitute control.

The exact entry-side terms commitment, detached handoff and checkpoint-control
bodies, cancellation governance operation, domains, signature ordering, stable-
commit ordering, stream-frontier commitment, retention attachment, and vectors
are Phase C stream-checkpoint/governance wire gates. Stable advertising is
forbidden until they exist.

### 6.4 Issuance fence

Receipt issuance, checkpoint proposal/voting, local readiness state, and every
replica-policy/component-root transition—stage, staged disablement, endpoint
update, quorum-only reconfiguration, or active-set change—share the governance
serialization fence. The only valid orders are:

```text
old-context receipt/vote stable-commits and is released
then transition commits
```

or:

```text
transition commits
then queued work revalidates and stable-commits under the new context
```

No old-context signature escapes after the transition wins the fence. No
newly active replica signs until the activation governance record, complete
policy, endpoint binding, class readiness, and new fence state are durably
installed. A committed stale-context receipt remains historical audit evidence
and is not re-emitted as current.

For checkpoint voting around an active-set change, the finer prepared-handoff
order is: all
released predecessor votes stable-commit; the prepare installs and pauses new
votes; each prepare-bound fence statement stable-commits before release; then
either the prepared `replica.set` child installs the derived successor frontier or the
cancellation child installs the derived resume frontier. A prepare or statement
cannot be rolled back to return to the first step.

### 6.5 Network and state convergence

After commit:

1. every replica/client verifies and transactionally installs the governance
   transition and complete successor policy;
2. connections close/rebind against the new component root and authoritative
   endpoints;
3. each process disabled by a removal delta stops receipts, checkpoint proposals/
   votes, and normal binding proofs after observing the new head, while it may
   serve bounded authenticated historical evidence according to policy; a pure
   join has no such process;
4. each replica activated by an activation delta completes §5.2 tail
   reconciliation and exposes typed per-stream readiness; a pure leave has no
   successor;
5. pending publication certificates finish entirely under the old exact
   context or collect a new matching set under the successor context; and
6. checkpoint construction uses only eligible current signers and the final
   stream-checkpoint stable-retention predicate.

Arrival order does not select a policy. A client on the old head may verify old
artifacts under retained evidence but cannot combine them with the new set.

### 6.6 Convergence completion

`converged` is not "every possible peer has observed the entry." For operator
status it means:

- the local node installed the exact successor governance state and policy;
- every activated successor, if any, installed that state, passed live two-key
  binding, has true storage/class readiness, and reports each required stream's
  checkpoint-relative/tail state;
- every removed signer, if any, is locally disabled for new issuance; and
- enough current eligible replicas are reachable to show whether `W` service
  is available.

The last item may be `quorum_unavailable`; convergence of authority does not
fabricate network liveness.

---

## 7. Equivocation evidence and quarantine

### 7.1 Checkpoint single-vote rule

The final stream-checkpoint owner MUST freeze one initial generation and exact,
checked successor allocation for each `(CommunityId, StreamId)`, plus a replica
vote whose signature binds the exact checkpoint identity/body. A valid body
uses exactly the successor of an authenticated certified checkpoint, handoff,
governed handoff cancellation, recovery, §8.2 conflict-slot control, or other
`W`-certified abandoned-slot frontier; an arbitrary forward jump is invalid
before evidence/quorum processing. For a retained `ReplicaId`,
generation does not reset when:

- governance head or replica component root changes;
- an endpoint rotates;
- retention generation/boundary changes;
- a proposal fails to reach a certificate;
- a checkpoint is superseded; or
- the replica is staged/active across an otherwise valid transition.

An honest replica signs at most one distinct checkpoint ID for one
`(CommunityId, StreamId, checkpoint_generation)` slot. Recertification or a
different proposal uses the next safely advanced generation. A failed proposal
does not permit a signer to choose another body in the same slot or jump ahead;
the checkpoint owner must freeze a `W`-certified abandon/next-frontier control
before progress. This makes conflicting signatures objective rather than an
inference from delivery order or local proposal state.

Before a checkpoint signature becomes observable, the signer atomically
stable-commits:

```text
(CommunityId, ReplicaId, StreamId, checkpoint_generation)
  -> exact checkpoint id, body/vote bytes, signature, and issuance context
```

with its per-stream vote high-water and idempotency state. A retry returns the
exact stored vote bytes. Crash recovery never signs a different body for a
stored slot. Missing, uncertain, or rolled-back vote-journal/high-water state
withdraws checkpoint and receipt readiness and requires permanent replacement,
just like uncertain receipt sequence state.

Successor arithmetic is checked. At the generation maximum, no wrap, reset,
larger sentinel, or same-version recovery is valid; checkpoint service fails
closed and a separately reviewed versioned rollover must be installed before
the maximum is reached. Because forward jumps without an authenticated frontier
are invalid, one malicious vote cannot jump to the maximum and block recovery.

Changing `ReplicaId` starts a different signer's duty, but the stream generation
itself remains monotonic.

### 7.2 Positive checkpoint-equivocation predicate

Two artifacts prove checkpoint equivocation by replica `Q` only when all of
these pass:

1. both exact checkpoint/vote records are within protocol bounds, strict-
   decode, canonical round-trip, and recompute their claimed checkpoint IDs;
2. the two checkpoint IDs differ;
3. both signatures verify strictly under the same raw `ReplicaId = Q` and the
   final checkpoint-vote domain;
4. both bodies name the same `CommunityId`, `StreamId`, and
   `checkpoint_generation`;
5. every other body invariant, predecessor/retention reference, and signature
   binding required before a replica could sign passes independently; and
6. authenticated historical governance/policy witnesses prove `Q` was active
   and checkpoint-eligible in each body's named governance/component/class
   context.

The two named governance heads or component roots may differ; generation is
non-reusable across those transitions. If either artifact was signed while `Q`
was staged, disabled, unknown, or policy-ineligible, that artifact is an
unauthorized signature and peer evidence, not proof that an eligible replica
violated the single-vote rule.

One replica signature pair is sufficient evidence of that replica's fault. A
`W` certificate is not required to quarantine the signer.

#### 7.2.1 Positive checkpoint-frontier-equivocation predicate

A cross-policy safety failure may expose one checkpoint vote and one false
frontier statement rather than two checkpoint votes. Replica `Q` also has an
objective fault when strict independent verification proves all of:

1. a valid `Q`-signed checkpoint vote/journal artifact under a context where
   authenticated historical policy made `Q` eligible;
2. a valid `Q`-signed `PreparedCheckpointFence`,
   `ForkResolvedFenceStatement`, handoff/cancellation, conflict-slot, or
   retired-signer-cutover statement under its exact eligible predecessor
   context;
3. both name the same community and stream; and
4. their signed semantics cannot both be true: the vote generation/ID lies
   beyond or conflicts with the statement's claimed journal frontier, its
   retained-vote commitment omits/differs from that exact vote, or the vote uses
   a predecessor policy/generation that the signed irrevocable fence closes.

This is `checkpoint_frontier_equivocation`. It needs no trusted wall-clock
ordering: the two signed protocol assertions are mutually exclusive. It uses
the same authoritative evidence, immediate quarantine, permanent governed
retirement, signer cutover, and operator severity as checkpoint double-voting.
A mere different local high-water report, malformed control, or statement from
an ineligible signer is not this proof.

### 7.3 Negative cases

None of these alone is checkpoint equivocation:

- duplicate delivery or alternate encoding of the same canonical body/ID;
- a lower generation received after a higher generation;
- different streams, communities, generations, or signers;
- a malformed body, bad signature, wrong key, unknown key, or unauthorized
  staged/disabled signer;
- a checkpoint vote and frontier statement with no signed semantic
  contradiction;
- a vote permitted by the selected cancellation outcome, or by a valid
  `ForkResolvedFrontier` that superseded the signer's losing reservation;
- different receipt acceptance/display times;
- an invalid event, checkpoint root mismatch, missing event, withholding,
  timeout, peer disconnect, or slow response;
- RBSR `view_equivocation`, range invalidity, or contradictory session
  summaries without two valid checkpoint signatures; or
- two administrator-signed #161 governance checkpoints, which belong to the
  governance snapshot/fork rules rather than this replica-vote policy.

Invalid and withholding behavior remains bounded operator evidence and may
change local peer selection. It cannot automatically permanently retire a
`ReplicaId` through this predicate. One stronger staged-candidate case is
handled separately: a valid receipt/checkpoint-domain signature from a key that
was only staged proves the holder violated zero-authority staging. It
withdraws local readiness, blocks proposal/approval/service for that candidate,
and requires an ordinary administrator-governed cancellation or staged disable/
re-provision with a fresh key, without relabeling the artifact as eligible-
replica equivocation. Its detached arrival does not retroactively invalidate an
already exposed governance child: if that child folds first, the local block
survives and governance immediately disables/replaces the identity.

### 7.4 Receipt equivocation

Receipt equivocation uses the already chosen sequence rule:

```text
same (CommunityId, ReplicaId, receipt_sequence)
different strict valid signed receipt bodies
historical policy proves the signer eligible in both named contexts
```

Lower-after-higher delivery is not sufficient. Two exact copies are not a
conflict. Cross-head or cross-root reuse of the same sequence still conflicts:
endpoint/governance transitions do not reset a retained `ReplicaId`'s receipt
namespace. A new replacement `ReplicaId` has a new namespace.

### 7.5 Authoritative incident evidence

The Phase C evidence owner freezes a bounded immutable core with at least:

```text
evidence_version
kind = checkpoint_equivocation | checkpoint_frontier_equivocation |
       receipt_equivocation
community_id
replica_id
conflict_slot_or_frontier_relation
artifact_a_exact_bytes_and_signature
artifact_b_exact_bytes_and_signature
canonical_sorted_artifact_ids
```

Canonical ordering is by artifact ID so receiving the pair in the opposite
order produces the same evidence identity. The evidence ID uses a dedicated
domain and the exact canonical immutable core; it is a computed value outside
its own preimage. Exact bytes remain a Phase C freeze/vector obligation.
Historical eligibility witnesses are verified attachments keyed by the
artifacts' named governance heads/component roots, not part of evidence
identity: #161 permits equivalent valid packages with different retained
approval/proof subsets.

The signer incident key is separately and deterministically derived from only
`(CommunityId, ReplicaId)`. The first verified objective fault installs one
primary evidence core; every later valid conflict kind, slot, frontier
contradiction, or old-context artifact for that signer attaches to the same
incident and cannot create another permanent proof-pair obligation.

Retention is hard-bounded per signer incident:

```text
MAX_INCIDENT_ARTIFACT_BODY_BYTES = 65_536
MAX_RETAINED_INCIDENT_ARTIFACT_BODIES = 8   # includes the primary pair
MAX_RETAINED_ELIGIBILITY_WITNESSES_PER_BODY = 1
MAX_INCIDENT_ELIGIBILITY_WITNESS_BYTES = 16_777_216
MAX_RETAINED_INCIDENT_WITNESS_BYTES = 33_554_432
MAX_RECORDED_OVERFLOW_OBSERVATIONS = 1_024
MAX_DIRECT_TRIGGER_SUBJECTS_PER_EVIDENCE = 2
MAX_RETAINED_DIRECT_TRIGGER_SUBJECTS_PER_SIGNER_INCIDENT = 8
```

The exact primary pair, its two required eligibility witnesses, and canonical
IDs are permanently retained. Up to six distinct supplementary bodies may be
kept for operator diagnosis within the body and aggregate witness-byte caps;
supplementary witnesses are optional after verification. After a cap is met, a
verified body's ID updates only a constant-size domain-separated rolling
`overflow_digest` and `overflow_count`; the supplementary body and witness
package are discarded. Exact deduplication is required only against the at-most-
eight retained body IDs. After discard there is intentionally no unbounded seen-
ID set: every subsequently verified overflow observation, including replay,
updates `H(domain || prior_digest || artifact_id)` and increments the count
until `MAX_RECORDED_OVERFLOW_OBSERVATIONS`, then digest/count freeze and an
`overflow_saturated` bit remains set. These fields are local arrival-order audit
metadata, not consensus identity, a unique-artifact count, or a membership
proof. Phase C may choose lower body/witness/overflow retention limits only
within the required primary-proof minima, but cannot raise those stable-v2
maxima without a versioned profile. The two direct-trigger constants are exact
stable-v2 profile values: lowering either could omit one subject from receipt
evidence or contradict the lifetime-cap contract and therefore requires a
versioned profile.

Trigger subjects are also hard-bounded. Checkpoint/checkpoint-frontier evidence
derives exactly one stream subject. Receipt evidence derives one subject from
each of its two signed bodies, then canonical-sorts and deduplicates that set, so
the result has one or two receipt/publication subjects with no reporter choice.
An incident retains at most eight `(subject, signer_incident)` trigger records/
subject-aggregate contributions. Existing retained subjects may still join
stronger fixed terms after the cap. New subjects are admitted in canonical order
only while capacity remains. If a verified candidate names any new subject that
does not fit, the same bounded transaction sets the incident's fixed
`direct_trigger_subjects_saturated` bit and allocates no record/aggregate for the
omitted subject. This loses exact per-subject precision, not safety: the
community barrier and keyed signer-incident index already put every subject that
`Q` could have authorized into conservative signer-incident mode. Such a stream
cannot become current without §8's current-`W` cutover/recovery, and a receipt/
publication subject cannot become current without Q-excluding reconfirmation or
recertification. Those final controls absorb later old-context `Q` artifacts;
the initial barrier—not the later saturation transition—already revoked any
generic post-barrier "Q-free" admission exception while `Q` could authorize.
Before trigger allocation, a candidate already covered by one of those certified
final controls updates only bounded audit/overflow metadata and never reopens
recovery. Otherwise an omitted subject remains conservative through the
existing barrier/index. Setting the
saturation bit neither allocates a new incident generation nor starts a scan;
later omitted subjects are already covered. Failure to persist its first
transition fails closed.

The primary pair proves one community-wide fact: this signer is objectively
faulty. That retained fact is sufficient basis for current `W` to certify the
signer's conservative cutover on every retained stream; §8 does not require a
new permanently retained proof pair for each stream or slot. A stream that
observes a later verified conflict keeps one fixed recovery marker in its
ordinary per-stream state—signer incident ID, last uncontested frontier,
recovery/cutover disposition, and the incident overflow digest/count snapshot—
rather than another raw evidence body. The marker is not independent proof of
the discarded conflict; the retained primary pair plus a fresh current-`W`
control is the authority for conservative recovery. This bounds attacker-
chosen artifact storage while preserving one recovery obligation per governed
stream, not per received signature.
Each retained directly named marker comprises one fixed direct-trigger record
keyed by `(subject, signer_incident)` plus the subject's fixed cumulative trigger
aggregate, both committed by §7.6's bounded activation/update transaction before
any supplementary body or witness is discarded. Omitted subjects are covered by
the existing conservative barrier/index mode above. Neither path waits behind
the community scan. Recovery-semantic fields are fixed size: incident/primary-
evidence linkage and a tagged stream or receipt/publication obligation. A stream
obligation includes the earliest affected generation, conservative last-
uncontested frontier, and recovery/cutover disposition; a receipt/publication
obligation includes its exact subject and reconfirmation/eligibility
disposition. The local overflow digest/count snapshot is audit-only and is not
part of the cumulative aggregate, source revision, or operational cache key.

A bounded incident record links the signer incident and primary evidence IDs to
quarantine, the governed replacement, per-stream retired-signer cutovers, and
recovery state. It updates fixed fields instead of appending one durable row per
late artifact. Later proof enrichment or resolution never changes primary
evidence bytes/ID, and an attacker cannot grow retained bodies linearly or
quadratically by manufacturing more valid signatures under an already-faulted
key.

Evidence is durably stored before an operator is told it can be exported or a
quarantine can survive restart. The store retains the exact signed bytes, not a
reconstructed JSON summary. Evidence validation is pure and deterministic;
observation time, reporter identity, network path, or operator label is not part
of the equivocation predicate.

### 7.6 Immediate local response and bounded materialization

Immediately after pure evidence verification, before network-derived storage
work, the node closes the affected issuance/quorum fence and applies a volatile
quarantine. Issuance is serialized with incident activation: an in-flight
receipt/checkpoint decision either commits entirely before the barrier below or
aborts and revalidates against it. It cannot commit using a pre-barrier view.

The first authoritative write is one **size-bounded incident-activation
transaction**. Under §7.5's body/witness caps it durably commits:

- the evidence core/update and required eligibility witnesses;
- the fixed-size incident/overflow state;
- `Q`'s durable quarantine;
- a fixed-size `CommunityIncidentBarrier` naming the community, signer,
  incident, primary evidence, and a checked monotonic `incident_generation`,
  initially `materialization_pending`; and
- for §7.5's canonical set of one checkpoint subject or at most two receipt/
  publication subjects, each retained fixed direct-trigger record and fixed
  cumulative `DirectTriggerAggregate` plus checked per-subject
  `incident_projection_revision`. Every subject in the canonical primary set is
  atomically covered even when a body/witness will later be discarded.

It MUST NOT enumerate, rewrite, or lock every retained stream or publication
certificate. At most two record/aggregate joins occur for one evidence package.
Generation exhaustion fails closed pending a versioned local-store rollover; it
never wraps. Once the bounded activation transaction commits, the
barrier itself immediately:

1. records one deduplicated signer incident and marks `Q`
   `quarantined_equivocation` in local derived operational state;
2. excludes `Q` from every new local receipt/checkpoint quorum decision and
   refuses its new work while still permitting bounded evidence/history service;
3. keeps configured `W` unchanged and reports `quorum_unavailable` if the
   remaining eligible set cannot meet it;
4. logically marks the directly conflicting stream `CheckpointEquivocated`,
   links every directly named receipt/publication subject to the incident,
   puts every retained stream in at least conservative signer-incident recovery/
   cutover-required mode, and makes every Q-dependent current receipt/
   publication result reconfirmation-required unless its exact certificate still
   has `W` other eligible signers under §8.4; and
5. emits one minimized critical operator/audit notification and schedules the
   resumable projection below.

Those effects are authoritative even before any dependent row is rewritten.
Authoritative direct-trigger records are keyed by
`(subject, signer_incident)`. Multiple verified candidates for one key, and each
changed key into its subject's `DirectTriggerAggregate`, use the same fixed
commutative, associative, and idempotent **conservative join**. For a stream, the
join chooses the smaller affected generation and its fixed candidate frontier.
Equal generations with different frontiers, or any already conservative
candidate at that generation, produce a fixed
`conservative_frontier_unknown` sentinel and the signer-incident basis. Its
closed disposition order can only preserve or strengthen recovery/cutover. This
join compares fixed fields only; activation/update performs no ancestry walk or
unbounded proof scan. For a receipt/publication subject, the closed join table
may only preserve or strengthen reconfirmation/exclusion. The aggregate contains
no signer list: the keyed signer-incident index remains the signer authority.
The Phase C stream and receipt owners freeze these joins and their boundary
tests. Thus different delivery orders produce the same aggregate, and no later
artifact can move a recovery start forward or make a status greener.

Every stream/publication-certificate row records a checked
`incident_barrier_applied_through` generation and an
`incident_projection_revision_applied`; the subject's authoritative
`DirectTriggerAggregate` has the current monotonic
`incident_projection_revision` (zero when no trigger exists). A current
operational read or write at community generation `G` MUST NOT consume a row
unless its applied generation equals `G` and its applied revision equals the
aggregate's current source revision. A mismatch cannot report
`Durable`, `Complete`, a checkpoint base, or quorum evidence. The O(1)
authority is the community high-water plus its keyed signer-incident/quarantine
index, including each incident's `direct_trigger_subjects_saturated` precision
flag, not a fold of every historical barrier. A stale stream remains
conservatively recovery/cutover-required; a certificate recomputes its at-most-
`R` signer set against that index. Saturation does not weaken this broad mode or
create a second overlay. A read-through materializer may apply only a projection-
unit/byte-bounded prefix and may consume the row only after its generation and
subject revision reach the current values; otherwise it returns a typed
materialization-pending/reconfirmation state. Operational caches bind `G` and
the subject revision; barrier activation invalidates community caches in O(1),
while a later retained-subject aggregate update invalidates only its subject.
This local rule does not change fold-time
validation of already exposed governance bytes or the prepare's governance-
carried control exclusions.

One coalesced community materializer walks existing fixed-key stream and
publication-certificate indexes in canonical order toward the current community
generation; it creates no attacker-sized per-item work queue. The hard stable-
v2 local-store transaction bounds are:

```text
MAX_INCIDENT_MATERIALIZATION_PROJECTION_UNITS_PER_TX = 256
MAX_INCIDENT_MATERIALIZATION_BYTES_PER_TX = 1_048_576
```

The projection-unit cap counts one unit whenever the materializer applies one
incident generation to one stream/certificate subject (or refreshes that
subject's cumulative direct-trigger aggregate); repeated logical mutations of
one backend row still count separately. The byte cap likewise sums the canonical/accounted
mutation bytes of every unit under one Phase C-frozen backend-independent rule,
not physical page/WAL overhead. Reaching either cap closes the batch. These are
local conformance limits, not public-wire fields.

A pass captures an exact target `incident_generation = G_target` and walks an
indexed, fixed-size two-dimensional cursor
`(next_required_incident_generation, subject_key)`; it does not materialize the
Cartesian product as a work queue. In the barrier phase, the stale-pair index
emits `(applied_through + 1, subject_key)` for a generation gap. In the direct-
refresh phase, a source-revision gap emits `(G_target, subject_key)` and one unit
applies the subject's complete fixed `DirectTriggerAggregate`, so it may advance
`incident_projection_revision_applied` directly to the aggregate revision
captured by that transaction. Either phase may rewind the cursor to its lowest
pair. Each barrier unit applies exactly one
retained barrier/signer incident to one subject's fixed-size cumulative derived
state. An ahead, malformed, or otherwise uncertain derived marker is reset
conservatively before this index is used. A row advances
`incident_barrier_applied_through` only across consecutively
applied generations; a direct-refresh unit advances only
`incident_projection_revision_applied`. Thus an imported/restored row far behind
may advance through intermediate `G_batch <= G_target` values across arbitrarily
many capped transactions and is never required to fold unbounded incident
history in one unit or transaction.

Each successful transaction compare-and-commits that the community generation
is still `G_target` and the observed catalog generation and covered subject-
aggregate revisions are unchanged, idempotently writes its bounded projection
units, and advances the two-dimensional cursor **in the same commit**. It stamps
only each covered row's consecutively processed `G_batch`, never a
newer generation whose incidents it did not apply. If the community generation
or a covered subject aggregate/revision changed, the batch aborts/recomputes and
the indexed lowest stale pair wins; it cannot overwrite a newer direct trigger. A
crash replays the last batch without changing the result. Only after a row
reaches `G_target` and its current subject revision does its cumulative
projection reflect the complete barrier set through that target; certificate
eligibility then reflects every signer quarantined in the keyed index through
that target. Until then the O(1) read gate above remains authoritative.

The cursor is a progress hint, never proof of completion. Membership in the
canonical stale-pair index is maintained atomically with row/catalog changes, so
`materialization_complete_through = G_target` requires only a bounded index-
emptiness query, not a retained-row scan. The completion transaction then
compare-and-commits that the index is still empty and both the observed incident
and catalog generations are unchanged. A newer incident's pending target
therefore monotonically wins over an older completion.

A genuinely new row created solely from post-barrier Q-free work MAY stamp the
current generation in its admission transaction only after bounded index checks
show that its exact context excludes every quarantined signer and §8's current-
`W` cutover/recovery or receipt recertification absorbs all applicable prior
contexts. Mere absence of a `Q` signature is insufficient. Later old-context
artifacts cannot invalidate that final control.
Imported, restored, or historically updated state remains stale and joins
bounded materialization even if its key sorts before the scan cursor. Multiple
signer incidents compose monotonically without lost updates. Catalog admission/
import, subject-content update, direct-trigger aggregate update, and deletion
commits advance a checked `catalog_generation`; the materializer's own derived
marker/cursor writes do not. Any commit that makes the indexed stale-pair
predicate non-empty atomically marks materialization pending at the current
target even when the community incident generation is unchanged. Admitting
stale state therefore reopens the coalesced scan at the indexed lowest stale
pair. The final no-stale check and
`materialization_complete_through = G_target` commit succeed
only if their observed incident and catalog generations are still current. No
transaction may cross either materialization cap while catching up generations.
Duplicate, supplementary, and overflow evidence for an existing signer incident
first applies §7.5's bounded deduplication/saturation rules, derives the canonical
one-or-two-subject set, and MUST serialize with all named subjects' issuance in
canonical order. One bounded transaction covers every subject atomically. For a
retained subject, the candidate joins its authoritative
`(subject, signer_incident)` record; any changed record then joins the subject's
cumulative aggregate. A changed pair record is authoritative even when the
aggregate was already more conservative. If and only if an aggregate changes,
the transaction increments its checked per-subject source revision and
`catalog_generation` and invalidates that subject's cache. Intermediate changes
to several pair records are therefore safely collapsed into one later fixed
aggregate refresh; no per-revision log is needed.

A new subject consumes one of the incident's eight lifetime slots. If no slot
remains, the transaction allocates no pair/aggregate and sets only that signer
incident's fixed `direct_trigger_subjects_saturated` bit. The existing broad
barrier/index mode already covers the omitted subject, so this transition
allocates no new `incident_generation`, cache invalidation, or materialization
scan; later omitted subjects are already conservative. Before consuming a slot
or changing a trigger, evidence under a certified retired-signer cutover/
superseded context follows §8.2 and updates only bounded audit/overflow metadata.
It cannot reopen recovery. An exact duplicate or other candidate whose pair/
aggregate joins are unchanged changes none of those fields; audit-only overflow
metadata may still advance under its separate cap. Once that metadata is also
unchanged/saturated, the candidate is a bounded no-op that schedules no work.
Required source/catalog revision exhaustion fails closed; no counter wraps. This
path does not restart a full scan. If a supplementary body is discarded, its
retained trigger/aggregate or the broad conservative barrier/index still covers
every named subject. Completion never clears the barrier, quarantine,
reconfirmation requirement, or current-`W` cutover/recovery obligation; late
historical imports still consult the retained incident index.

If the bounded activation transaction or an existing-incident authoritative
direct-trigger/saturation update fails, the volatile quarantine remains and the node enters
`IncidentPersistenceFailed`: all new receipt/checkpoint decisions for the
community stop until the evidence, incident barrier/quarantine, keyed index, and
required canonical trigger records/aggregates or saturation bit are durable. The
exact candidate remains in the intake slot when available. The node MUST NOT
continue using `Q` or forget any named subject merely because disk-full,
corruption, or an I/O fault prevented that bounded write. By contrast, only
failure of a later derived `incident_projection_revision_applied`
materialization batch with all
authoritative state healthy/readable sets `IncidentMaterializationBlocked`. The
unprocessed rows remain logically stale and fail closed, but already evaluated
Q-excluding work may continue when it meets unchanged `W` and every other
protocol rule. Batch failure never re-includes `Q`, clears the barrier, or
upgrades a stale status.
Loss, corruption, or unreadability of the evidence, quarantine, barrier/high-
water, keyed signer-incident index (including its saturation bit), or keyed
direct-trigger record/cumulative-
aggregate/source-revision high-water is an authoritative-persistence failure and
fail-stops the community.

Phase C must make those restart rules enforceable rather than trusting lost RAM.
Community creation preallocates a fixed-size `EvidenceIntakeSlot` in a
separately qualified metadata region with capacity reserved independently from
variable incident bodies for one maximum-size primary evidence package,
including its required witnesses and codec overhead. Intake is serialized and
bounded per community. After cheap frame bounds and canonical envelope decode,
but before objective/signature verification, the implementation atomically
stores the exact package bytes, claimed community/signer, digest, and
`candidate_pending` state in that slot. It verifies only the committed bytes.

An invalid candidate is durably cleared without quarantine. A valid candidate
first closes the volatile issuance fence; the slot is cleared only after either
the bounded new-incident activation or the bounded existing-incident trigger
set/saturation/change/no-op determination above is durably committed against all
authoritative keys. A crash can therefore leave an empty healthy slot plus a durable
pending/running/blocked barrier and
cursor; recovery resumes from validated progress or a conservative reset
without re-verifying a missing candidate or reopening issuance through stale
rows. If activation/update fails, the exact candidate remains in the slot as the
durable fail-stop source and the community stays `IncidentPersistenceFailed`.

Failure to stage a bounded candidate in the reserved slot is a storage-health
fault: the community stops new receipt/checkpoint decisions until the slot is
repaired and successfully exercised, but an unverified claimed signer is not
quarantined or retired. An unreadable non-empty slot, or failure to preserve a
verified candidate/incident barrier, fail-stops all v2 issuance for that
community. Recovery replays and reverifies a readable candidate and exactly
restores every authoritative evidence record, quarantine, barrier/high-water,
keyed signer-incident index including its saturation bit, and keyed direct-
trigger record/cumulative-
aggregate/source-revision high-water before recording operator acknowledgement
and clearing a storage-fault state. Materialization
cursors, phases, catalog generations, per-row applied markers, and stale-pair
index are derived progress. Recovery may use them only when store integrity and the atomic batch
relation to the recovered barrier are established. Missing, malformed, rolled-
back, ahead-of-barrier, or
otherwise uncertain progress is reset to the canonical beginning/lowest stale
key and rebuilt under the barrier; it is never trusted ahead and does not by
itself require a fresh `ReplicaId`. No global
"unclean issuance epoch" is set merely because the process is running, so an
ordinary power/process crash with an empty healthy slot and no stale barrier
state follows §9.1 and does not invent a suspect signer. Exact slot/barrier
bytes, conservative progress reset, batch atomicity, independent failure model,
and recovery bounds belong to the Phase C readiness/store owner. Disk-full or
I/O failure at each intake, activation, batch, cursor, and completion boundary
followed immediately by crash/restart is a mandatory fault test.

This does not mutate the governance component or erase historical signatures.
Nodes that have not received the evidence may continue under the governed set;
evidence dissemination and a prompt governance transition converge them. No
arrival-order or lexicographic root rule is introduced.

### 7.7 Governed penalty

The incident cannot be closed while `Q` remains staged/active or reactivatable.
Materialization progress is a separate local-store axis: `pending` or `blocked`
never permits skipping a recovery/cutover obligation, and `complete` neither
closes the incident nor supplies governance, current-`W` certification, or
operator acknowledgement.
Administrators MUST use an ordinary successor `replica.set` to:

- permanently disable `Q` with cause and incident ID;
- preserve its descriptor, tombstone, historical policy, and exact evidence;
- activate a separately keyed ready replacement when policy requires the seat;
- certify §8.2's per-stream retired-signer cutovers through recovery or the
  replacement handoff;
- commit the complete successor `R/W` policy, leaving `W` unchanged by default;
  and
- install the transition before clearing the operator action-required state.

There is no automatic governance author, old-key consent, grace-signature
period, appeal that reactivates the key, or token slash. Human review verifies
that the evidence and replacement plan are correct; it does not make an
objective signature conflict harmless.

---

## 8. Checkpoint conflict recovery

### 8.1 Affected completeness state

Once valid evidence exists, no checkpoint body in the conflicting slot may
support a new `CompleteThroughCheckpoint` claim, even if only one body had
previously accumulated a `W` certificate. Existing event signatures,
publication certificates, and stored bodies are not deleted or silently
rewritten. The stream reports:

```text
CheckpointEquivocated {
  basis = retained_stream_conflict | signer_incident_conservative,
  recovery_start_generation,
  retained_conflicting_checkpoint_ids,
  incident_overflow_digest_and_count,
  incident_id,
  last_uncontested_checkpoint,
  conflict_frontier = required | certified,
  retired_signer_cutover = required | certified,
  recovery = required | in_progress | certified
}
```

Previously displayed completeness becomes an explicit incident state rather
than staying green. Historical policy/signature validity remains inspectable;
the UI does not claim which conflicting retained-set root is true.
The durable community incident barrier supplies this logical state when the
per-stream row is absent or stale. Materialization only persists the derived
projection; no read, proposal, recovery base, or serving path may use the old
row while waiting for that projection.

### 8.2 Conflict-slot frontier and retired-signer cutover

Evidence can consist of two conflicting votes at generation `g` even though no
body reached a `W` checkpoint. In that case `g + 1` is not yet allocated by
§7.1. Before recovery, `W` eligible replicas from one exact current policy sign
a `ConflictSlotFrontier` control containing at least:

```text
community_id, stream_id
signer_incident_id and primary/supplementary evidence commitment
last authenticated predecessor frontier
basis = retained_stream_conflict | signer_incident_conservative
recovery_start_generation
through_generation = f
canonical contiguous slot-disposition commitment for recovery_start..=f
retired_signer_cutover
current governance/component/class/W context
```

Exact `retained_stream_conflict` mode is valid only when the exact stream-
specific pair **and currently verifiable historical-eligibility witnesses for
both bodies** are retained or otherwise available through the frozen historical-
policy proof. The control then re-verifies objective evidence at `g`, sets
`recovery_start_generation = g`, and accounts without gaps for every later
certified, abandoned, handed-off, or conflict-dependent slot through the latest
accepted frontier `f`. Retained bodies without both currently verifiable
eligibility proofs cannot select exact mode. When a later verified pair or its
witnesses were discarded/unavailable under §7.5's hard cap, the globally
retained primary pair still proves the signer incident. The conservative mode
instead starts at the exact successor of a **safe anchor** and closes every
possibly signer-influenced slot through `f`. A safe anchor is either before `Q`
first became checkpoint-eligible in the retained lineage, or is a later exact
current-`W` reconfirmation already bound to a Q-excluding cutover. An ordinary
checkpoint in a slot/context where `Q` could sign is not a safe anchor merely
because its particular certificate contains `W` other signers. The mode does
not assert that the discarded or witness-less per-stream body remains
independently provable.

For example, with `R=3,W=2`, Q's conflicting votes `X/Y` at `g` poison slot `g`
for current completeness even if the other replicas `B+C` certified body `Z` at
`g`. `Z` remains historically signature-valid, but recovery closes `g`; it
cannot be selected as the predecessor/safe anchor to skip that closure.

That conservative mode is available immediately for every retained stream
whose current checkpoint status depends on `Q`; it does not wait for a second
stream-specific conflict to arrive. Thus the primary incident deterministically
creates at most one reconfirmation/cutover obligation per governed stream, and
forgetting or discarding a later supplementary body cannot leave a Q-dependent
current completeness claim green.
Until a fixed per-stream marker materializes, §7.6's community barrier is the
implicit conservative obligation. Materializing it does not certify the
`ConflictSlotFrontier` or `RetiredSignerCutover`; the ordinary current-`W`
signatures below remain required.

In either mode the control cannot omit a slot inside its authenticated range,
skip to an operator-chosen generation, or consume state after `f`. Its `W`
signatures close the contiguous range and allocate exactly `f + 1` with checked
arithmetic. When the retained conflict was only two votes at `g` and the prior
frontier was `g - 1`, the control consumes `g`; the recovery checkpoint is
therefore exactly `g + 1`. Phase C MAY encode the control and recovery
certificate in one envelope, but validation preserves these two semantic steps
and binds the signer incident and applicable retained evidence.

The control also installs a per-stream `RetiredSignerCutover` for each proven
signer `Q`. It binds `Q`, the signer incident, the governance retirement/
quarantine boundary, all Q-eligible predecessor policy contexts through that
boundary, and the freshly verified current frontier. Once current `W` certifies
it, any later `Q` artifact naming one of those superseded contexts is historical
incident material only, regardless of observation time or claimed generation.
Artifacts at or below the frontier attach to the signer incident; an artifact
claiming a later generation under a superseded old context is rejected from the
current lineage and likewise only updates the bounded incident accumulator.
Neither can reopen completeness or force another recovery. This is a current-
state cutover, not a claim that the old signature was never cryptographically or
historically valid.

A replacement or leave handoff under §6.3 MUST carry equivalent per-stream
cutovers for every active signer it disables, including planned retirement and a
receipt-only incident with no checkpoint conflict. Until current `W` certifies a
faulted signer's cutover through either the conflict control or replacement
handoff, affected current completeness remains incident/unavailable. A newly
proven fault by a different still-eligible signer creates that signer's one
incident and penalty, but never reopens a range already closed for content
recovery.

### 8.3 Recovery checkpoint

The stream-checkpoint owner MUST version the body/certificate if necessary to
support a recovery checkpoint that:

1. names a valid current-`W` `ConflictSlotFrontier` from §8.2 and uses its exact
   checked successor, never an operator-chosen jump or wrapping value;
2. names the last uncontested predecessor according to the frozen checkpoint
   graph rules;
3. repeats the control's slot/range closure, signer cutover, and a canonical
   sorted, duplicate-free `supersedes` audit list of every individually retained
   conflicting checkpoint ID known when the recovery is proposed;
4. commits a freshly reconciled and independently validated retained-set root,
   retention generation/boundary, count, and any archive manifest;
5. is signed by at least `W` distinct eligible replicas from one exact current
   successor policy/component/class context; and
6. retains rather than compacts the primary incident proof and every exact
   supplementary artifact still required by §7.5's bounded retention policy.

The new replicas reconcile verified events and publication evidence; they do
not take a content-root choice from the administrator governance transition.
No lexicographic, earliest-seen, longest-count, or operator-selected root can
stand in for the new `W` certificate.

If fewer than `W` eligible replicas can complete that work, the stream remains
`CheckpointEquivocated`/`QuorumUnavailable`. This is the declared liveness
boundary, not permission to lower the quorum.

The range closure and signer cutover, not completeness of the then-known
`supersedes` list, make recovery final. Later old-context artifacts follow
§8.2, update only bounded signer-incident state, and do not replace the freshly
certified root. Newly discovered valid event material follows ordinary
reconciliation and may affect a later exact-successor checkpoint, never the
closed range by arrival order.

The local overflow digest/count/saturation snapshot may be displayed or
exported beside recovery status, but it is not in the recovery checkpoint,
frontier-control identity, or matching `W` terms and need not agree across
replicas.

If no checked successor generation exists, recovery fails closed with a typed
generation-exhausted state until a separately versioned rollover is governed
and implemented. It never resets or chooses a larger sentinel.

### 8.4 Receipt conflict recovery

Two conflicting receipts at one signer sequence remain preserved. Every current
durability/status query recomputes the referenced publication certificate after
excluding the quarantined signer. If at least `W` other distinct eligible
signers already remain in that exact certificate/context, current durability may
stay certified with the incident link. Otherwise it becomes:

```text
ReceiptEquivocated {
  incident_id,
  prior_certificate_id,
  current = reconfirmation_required | recertified,
}
```

`reconfirmation_required` is not `Durable`. Current work is re-certified with
`W` eligible signatures in one exact valid context when possible. The prior
certificate remains an auditable historical artifact linked to the incident;
the operator view does not pretend the later evidence proves when either old
signature was created.
This recomputation is logically required by the community incident barrier even
when a per-certificate row is below its generation. A bounded synchronous
evaluation may preserve certification with the incident link or persist
`reconfirmation_required`; failure to materialize returns the pending state,
never the stale pre-quarantine result.

Replacement starts a fresh sequence namespace. It does not choose one old
receipt, rewrite its sequence, or re-sign it under the new key.

---

## 9. Failure and recovery behavior

### 9.1 Same-identity in-place recovery

An active `ReplicaId` may resume after a process/OS/power crash without a
governance transition only when all of these are true:

1. the same qualified durable store recovers in place under #156's backend
   rules;
2. integrity/schema/WAL recovery finds the exact committed events, receipts,
   receipt idempotency rows, governance/policy evidence, and receipt high-water;
3. the signer secret was not lost, restored from an older image, exposed, or
   duplicated;
4. a sole-writer/external-signer fence proves no other instance could issue
   under the same key during recovery;
5. the exact current governance head/component/class context is installed,
   non-forked, and has no pending `fork_frontier_reconciliation_required` or
   other unresolved exact-closure reservation;
6. every per-stream checkpoint journal recovers the exact issued vote/body/
   signature, generation high-water, idempotency state, and any prepare,
   handoff, cancellation, `ForkResolvedFenceStatement`,
   `ForkResolvedFrontier`, nested resolution reservation/closure, structural or
   final dependency count/root and complete proof, reconciliation child, or
   issuance fence that became externally observable;
7. endpoint binding is current or follows the safe endpoint-only update without
   copying or rolling back either receipt or checkpoint monotonic state;
8. the reserved evidence-intake slot is healthy and either durably empty or has
   completed §7.6's replay/reverification and bounded incident-barrier
   activation; an empty slot does not imply that barrier materialization is
   complete; and
9. every authoritative incident evidence record, quarantine, barrier/high-
   water, keyed signer-incident index including trigger-subject saturation, and
   keyed direct-trigger record/
   cumulative-aggregate/source-revision high-water recovers exactly. Any
   materialization cursor/phase, catalog generation, per-row applied marker, or
   stale-pair index is either proven consistent
   with those records and its atomic commits or conservatively reset and rebuilt
   before it can report current. The identity is governed `active`,
   locally non-quarantined, free of a mandatory-
   retirement/issuance-uncertainty cause, and otherwise operationally eligible;
   any stale dependent row remains gated until bounded evaluation; and
10. all storage readiness checks pass before the first signature is exposed.

This is crash recovery of one continuous state, not reactivation of a disabled
identity. It does not create a new governance status or reset a sequence. A
quarantined or mandatory-retirement identity that recovers intact may serve only
the bounded evidence/history lane; it never resumes receipt/checkpoint signing.
Failure of signer/receipt/checkpoint/control continuity follows §9.2 and selects
a fresh `ReplicaId`. Failure solely to recover §9.1 item 9's authoritative
community incident state instead keeps the community
`IncidentPersistenceFailed` until exact repair or verified recovery; changing a
signer identity does not repair missing barrier/index/trigger obligations. If
both failures exist, replacement still does not clear the community incident
fail-stop. Loss of derived materialization progress alone takes neither path and
uses the conservative rebuild above.

### 9.2 Mandatory replacement after rollback uncertainty

Replacement is mandatory if any of these holds:

- a backup/snapshot may predate an issued receipt, checkpoint vote, handoff, or
  checkpoint-frontier control;
- the durable high-water is lower than, conflicts with, or cannot authenticate
  against retained exact receipt state;
- a committed receipt is missing its event, required reference metadata,
  idempotency key, governing evidence, or sequence allocation;
- a checkpoint vote journal, generation high-water, exact retry bytes,
  idempotency record, handoff, or issuance fence is missing, lower, conflicting,
  or cannot be authenticated against retained signed state;
- two writers or cloned signer/store images may have run;
- an external signing service may have retained/exposed an uncommitted or
  unrecorded signature;
- the replica signing secret was lost, compromised, or cannot be shown to be
  continuous; or
- repair would require guessing a counter or deleting conflicting evidence.

Scanning reachable peers cannot prove the maximum ever signed sequence: an
unseen client, offline peer, or attacker may hold a higher receipt. Choosing
`max(observed) + 1`, reserving a large receipt or generation gap, changing the
endpoint, or obtaining an administrator statement therefore does not make the
old namespace safe.
The old key is disabled permanently and a fresh `ReplicaId` starts a fresh
namespace.

### 9.3 Storage durability breach

If recovery finds an issued receipt without its required stable durable set,
the replica:

1. stops receipt issuance and affected checkpoint voting;
2. preserves the exact receipt/checkpoint artifacts, fault details, and all
   recoverable evidence;
3. reports `durability_breach` and storage readiness false;
4. repairs from independently verified replicas/checkpoints without claiming
   the prior receipt was never issued; and
5. resumes the same identity only if §9.1's exact continuous state—including
   receipt and checkpoint monotonic journals—can be re-established. Otherwise
   administrators replace it.

A storage fault alone is not cryptographic equivocation. It still justifies a
governed durability-breach replacement after operator review.

### 9.4 Key and endpoint compromise

Replica signing-key compromise permanently retires the `ReplicaId`, even when
no conflicting signature has yet been observed. The operator uses a typed
`key_compromise` cause, stages an independent successor, and treats every
newly presented old-key artifact conservatively. Cryptography cannot prove
whether an attacker signed before or after discovery.

Endpoint-key compromise alone may use endpoint-only rotation if separation of
the application signer/store is still credible. No convenience grace period
continues normal work on the compromised endpoint at the new head. If role
separation or state continuity is uncertain, replace both roles with a new
replica descriptor and `ReplicaId`.

### 9.5 Degraded and unavailable quorum

Quarantine, outage, stage, catch-up, storage-unready, and tail-reconciling
states are distinct. Only an active, non-quarantined, artifact-eligible replica
whose local class predicate passes can contribute to a new local quorum.

For default `R=3, W=2`:

- one ordinary offline replica leaves two eligible signers and reports
  degraded redundancy;
- one quarantined signer also leaves at most two eligible signers, but the
  incident remains critical and replacement-required;
- any second unavailable/unready signer makes new durable publication and
  checkpoint completeness unavailable; and
- a staged replacement does not restore service until activation and local
  readiness complete.

Individual exact receipts and content may still exist. The UI distinguishes
their presence from a `W` certificate.

### 9.6 Failure matrix

| Condition | Required behavior |
|---|---|
| Candidate descriptor invalid or key reused | Reject the complete stage plan; no partial record or catch-up authority. |
| Stage commits but candidate never becomes ready | Active `R/W` remain unchanged; disable the staged record through governance when abandoned. |
| An authenticated readiness-manifest input or head outside §5.4's exact prepare/fork/reconcile bridge changes | Mark `readiness_stale`; refresh catch-up/manifest and rebuild the plan. The exact bridge's head-only progression does not stale it. |
| Candidate crashes before activation | Recover its staged store or restage a new key; it still has zero quorum weight. |
| Staged candidate emits a valid receipt/checkpoint signature | Withdraw local readiness and block proposal/approval/service; cancel an open prepare or govern disable/re-provision. Do not arrival-order-invalidate already exposed governance bytes or count it as eligible-replica equivocation. |
| Prepare or prepared `replica.set` child lacks old-admin quorum | Reject; retain predecessor policy and any readiness evidence. |
| Request lacks a committed approved prepare | Reject without journal/fence mutation; child proposals cannot solicit signatures. |
| Different handoff already in flight on this lineage | Reject; the committed prepare owns the reservation, exact replay returns stored bytes, and only the prepared `replica.set`, cancellation, or post-fork frontier reconciliation supersedes the fence. |
| Unrelated ordinary governance successor targets an open prepare | Reject it; finish the derived prepared `replica.set` or cancellation child, or use the explicit `fork.resolve` path when governance is forked. |
| Active-set change lacks predecessor `W` checkpoint handoff | The transition is uncommittable; a join/replacement candidate remains staged, a pure leave has no candidate, and any exposed predecessor fence remains closed pending the exact transition/cancellation. Never bypass the handoff or lower `W`. |
| Detached cancellation arrives without its governance commit | Do not reopen checkpoint service; cancellation advances the governance head or has no effect. |
| Active-set transition commit is ambiguous locally | Recover governance/store transaction and compare exact entry/root before enabling any changed signer policy. |
| Old replica keeps signing after learning successor head | Reject from the new context, preserve as incident/operator evidence, and apply the exact equivocation predicate if a sequence/slot conflict exists. |
| New replica receives work before activation is stable | Reject/park within bounds; issue no receipt or checkpoint vote. |
| Fewer than `W` eligible replicas remain | `Pending`/`QuorumUnavailable`; never lower `W` or class. |
| Governance is forked/incomplete | `GovernanceForked`; recovery authority resolves governance without changing `admin_seq` semantics, then current `W` and selected ordinary admins reconcile the exact closure. |
| Any stable-v2 `fork.resolve` commits | Deterministically enter `fork_frontier_reconciliation_required` even when no detached replica artifact was observed; current `W` certifies §6.3.1 and selected administrators commit the reconciliation child before ordinary work. |
| Fork-frontier reconciliation cannot reach current `W` | Remain fail-closed at the resolved head; recovery keys do not count and no losing fence is locally cleared. |
| Structural/final fork dependency proof is unavailable, truncated, noncanonical, mismatched, or over a leaf/chunk bound | Reject the statement/frontier/child as applicable and remain fail-closed; never accept a prefix, structural-only proof at commit, or locally summarized suffix. |
| Checkpoint signatures conflict | Preserve exact proof, quarantine signer, require permanent replacement, and enter §8 recovery. |
| Checkpoint vote contradicts signer frontier/fence | Treat as objective frontier equivocation with the same quarantine, retirement, cutover, and evidence path. |
| Receipt signatures conflict | Preserve exact proof, quarantine signer, require permanent replacement, and recertify current work where possible. |
| Evidence intake, bounded incident-barrier activation, or authoritative direct-trigger/saturation update fails | Keep a verified candidate in the reserved intake slot when available, preserve volatile quarantine, and block the community through restart until slot/barrier/trigger repair or explicit verified recovery; never retire an unverified claimed signer. |
| Derived incident materialization batch fails with its authoritative state intact | Keep `Q` durably excluded and stale rows pending; resume bounded idempotent batches. Already evaluated Q-excluding work may continue only when unchanged `W` and every per-record recovery rule are satisfied. This row covers only per-row applied progress, never a source trigger. |
| Invalid/withholding/RBSR peer behavior only | Record bounded peer/operator evidence; do not call it checkpoint equivocation or auto-disable governance state. |
| Arbitrary/non-successor checkpoint generation | Reject before quorum/evidence processing; do not quarantine from the invalid jump alone. |
| Checkpoint generation is exhausted | Fail checkpoint/recovery service closed pending a separately versioned governed rollover; never wrap or reset. |
| Exact store, receipt high-water, and checkpoint journals recover in place | Same active identity may resume after all §9.1 checks and issuance fence. |
| Restored/uncertain/rolled-back receipt or checkpoint monotonic state | Stop signing and replace with a fresh identity; never guess a counter or generation. |
| Signing key is compromised | Permanent disable/replace; retirement is prospective and old evidence remains. |
| Endpoint key only is compromised | Rotate endpoint with continuous signer/store, or replace if separation is uncertain. |
| Lost events cannot be reconstructed | Report explicit retained/tail gaps; do not claim completeness or manufacture bodies. |
| Conflict control/cutover or recovery cannot reach `W` | Keep `CheckpointEquivocated`/`QuorumUnavailable`; preserve the primary proof and bounded incident state. |
| Conflicting votes occupy an uncertified generation | Current `W` certifies §8.2's incident-bound slot control, then recovery uses exactly the allocated successor. |
| Retired signer reveals another artifact under a cut-over predecessor context | Update only the bounded signer incident/digest; do not reopen completeness or allocate another recovery. |
| Prior receipt certificate loses quarantined weight | Recompute current eligibility; show `ReceiptEquivocated`/`reconfirmation_required` until exact-context eligible `W` recertifies. |

---

## 10. Operator workflow and CLI contract

### 10.1 Implementation boundary

The shipped CLI currently has no replica, governance-approval, incident, or
authoritative evidence command. The commands below are the required Phase C UX
shape, not claims about rc.5. They follow existing conventions:

- successful human/machine data goes to stdout;
- warnings and errors use stable lowercase codes on stderr;
- coded failures have coarse category exit status and a fixed secret-free
  `next:` line where a generic action exists;
- `--json` has a stable versioned object rather than parsing human columns; and
- mutating governance is plan/propose/approve/commit, never one opaque command.

### 10.2 Status and incident inspection

The minimum read-only surface is:

```text
iroh-rooms replica status <COMMUNITY_ID> [--json]
iroh-rooms replica incident show <INCIDENT_ID> [--json]
iroh-rooms replica incident export <INCIDENT_ID> --output <PATH>
iroh-rooms replica incident materialization status <COMMUNITY_ID> [--json]
iroh-rooms replica catch-up status <COMMUNITY_ID> <REPLICA_ID> [--json]
iroh-rooms replica handoff reconcile status \
  <FORK_RESOLVE_ENTRY_ID> [--wait <DURATION>] [--json]
iroh-rooms replica handoff reconcile dependencies \
  <FORK_RESOLVE_ENTRY_ID> [--cursor <CURSOR>] [--limit <1..256>] [--json]
```

`replica status` first displays one community-level summary, then per-replica
rows. Community-level state includes exact governance/component context, `R/W`
and quorum service, the serialized evidence-intake/incident-persistence state,
community incident generation/barriers and materialization state, and any
handoff/fork-reconciliation state; implementations MUST NOT scope an actual
community-wide issuance stop to one row or mislabel a per-record materialization
backlog as one. The combined surface displays:

- safe full/prefix `ReplicaId` and current endpoint identity separately;
- governed lifecycle state and local quarantine/eligibility overlay;
- exact governance head and replica component root;
- active `R`, configured `W`, locally eligible count, and reachability;
- governed durability class and typed storage readiness cause;
- receipt high-water disposition (`continuous`, `fresh`, `uncertain`,
  `retired`), never the secret;
- checkpoint-vote-journal disposition (`continuous`, `fresh`, `uncertain`,
  `retired`) plus bounded per-stream generation/fence summary;
- governance snapshot/tail and per-stream checkpoint-relative/tail progress;
- readiness-manifest/plan/incident identifiers where applicable;
- evidence-intake state (`healthy_empty | candidate_pending | unreadable |
  repair_required`), authoritative incident-persistence state, a claimed signer
  only after safe canonical envelope decode, and an exact replay/repair next
  action; an unverified claim is never displayed as a retired signer;
- retained artifact-body usage/overflow plus precise direct-trigger-subject
  usage against the lifetime cap and the
  `direct_trigger_subjects_saturated` precision flag;
- durable incident-barrier generation and materialization state (`pending |
  running | blocked | complete`), current bounded scan kind/two-dimensional
  cursor, processed projection-unit count, remaining unit count when known
  (otherwise `unknown`), last bounded failure, and automatic-resume or storage-
  repair next action;
- prepare/handoff/cancellation single-flight, signer-cutover, and receipt-
  reconfirmation states where applicable;
- fork-resolution/frontier-reconciliation state, selected original
  prepare ID, latest resolution/reservation base, selected target, resolved
  closure commitment, governance-carried control-exclusion commitment,
  structural dependency count/root and collection-phase final count/root
  (`pending` before collection), prior-unresolved reservation count, consumed/
  remaining outcome, exact current `R/W`, fold-time policy-
  eligible count, locally authorable/reachable/collected signer counts, selected-
  admin threshold/approval count, and exact next action; and
- convergence state and a concrete next action.

It MUST NOT collapse `staged`, `active_tail_reconciling`, storage-unready,
offline, quarantined, disabled, and quorum-unavailable into one "online" flag.
`reconcile dependencies` pages the canonical proof order and shows every prior
unresolved reservation ID, closure commitment, typed dependency, and consumed
outcome without placing an unbounded list in status or a signed body. Cursor
continuity, final count, and root are verified on every complete traversal.
Materialization status describes the coalesced community scan; it does not dump
or persist a separate per-item queue. There is no force-clear, cursor-skip, or
"mark complete" operator path.

Example human output:

```text
community: cmt_7b2c…
governance: seq 418 head gov_29af…
replica policy: root rps_81d4…  active 3  quorum 2  eligible 2

REPLICA       GOVERNED  LOCAL                    CLASS                RECEIPT STATE
rpk_01aa…     active    ready                    local_sync_group_v1  continuous
rpk_7f20…     active    ready                    local_sync_group_v1  continuous
rpk_43bd…     active    quarantined_equivocation local_sync_group_v1  retired
rpk_9ce0…     staged    checkpoint_caught_up     local_sync_group_v1  fresh

warning[replica_checkpoint_equivocation]: rpk_43bd… has verified conflicting signatures
next: run `iroh-rooms replica incident show <INCIDENT_ID>`, then create a replacement plan
```

`incident show` separates evidence validity, quarantine effect, governed
resolution, and stream recovery:

```text
incident: inc_6f18…
primary kind: checkpoint_equivocation
observed kinds: checkpoint_equivocation
status: verified_quarantined_replacement_required
replica: rpk_43bd…
slot: stream str_a4c1… generation 42
artifacts: chk_1170… chk_d93e…
retained artifact bodies: 2/8  overflow count: 0
direct trigger subjects: 1/8  saturated: no
governed policy at detection: R=3 W=2 root=rps_81d4…
local eligible after quarantine: 2
incident barrier: durable generation=7
materialization: running kind=streams batch_limit=256 remaining=unknown
replacement governance entry: none
retired-signer cutover: required (0/17 retained streams certified)
checkpoint recovery: required
receipt reconfirmation: not_applicable
```

### 10.3 Replica lifecycle and policy workflow

The minimum mutating workflow is:

```text
iroh-rooms replica stage plan <COMMUNITY_ID> --descriptor <FILE> \
  (--join | --replace <OLD_REPLICA_ID>) --out <PLAN>
iroh-rooms replica stage propose --plan <PLAN> --out <ENTRY>
iroh-rooms governance approve <ENTRY>
iroh-rooms governance commit <ENTRY>

iroh-rooms replica catch-up status <COMMUNITY_ID> <REPLICA_ID>

iroh-rooms replica replace plan <COMMUNITY_ID> \
  --old <REPLICA_ID> --new <STAGED_REPLICA_ID> \
  --cause <REPLACEMENT_CAUSE> [--w <SUCCESSOR_W>] \
  [--incident <INCIDENT_ID>] [--recovery-disposition <FILE>] --out <PLAN>
iroh-rooms replica handoff prepare propose --plan <PLAN> --out <PREPARE_ENTRY>
iroh-rooms governance approve <PREPARE_ENTRY>
iroh-rooms governance commit <PREPARE_ENTRY>
iroh-rooms replica handoff collect \
  --prepare <PREPARE_ENTRY_ID> --out <HANDOFF>
iroh-rooms replica replace propose \
  --plan <PLAN> --prepare <PREPARE_ENTRY_ID> \
  --handoff <HANDOFF> --out <ENTRY>
iroh-rooms governance approve <ENTRY>
iroh-rooms governance commit <ENTRY> --handoff <HANDOFF>
iroh-rooms replica replace status <ENTRY_ID> [--wait <DURATION>] [--json]

# pure join after the stage/catch-up steps above
iroh-rooms replica join plan <COMMUNITY_ID> \
  --new <STAGED_REPLICA_ID> [--w <SUCCESSOR_W>] --out <JOIN_PLAN>
iroh-rooms replica handoff prepare propose \
  --plan <JOIN_PLAN> --out <JOIN_PREPARE_ENTRY>
iroh-rooms governance approve <JOIN_PREPARE_ENTRY>
iroh-rooms governance commit <JOIN_PREPARE_ENTRY>
iroh-rooms replica handoff collect \
  --prepare <JOIN_PREPARE_ENTRY_ID> --out <JOIN_HANDOFF>
iroh-rooms replica join propose \
  --plan <JOIN_PLAN> --prepare <JOIN_PREPARE_ENTRY_ID> \
  --handoff <JOIN_HANDOFF> --out <JOIN_ENTRY>
iroh-rooms governance approve <JOIN_ENTRY>
iroh-rooms governance commit <JOIN_ENTRY> --handoff <JOIN_HANDOFF>
iroh-rooms replica join status <JOIN_ENTRY_ID> [--wait <DURATION>] [--json]

# pure planned leave: no staged/new replica or readiness manifest
iroh-rooms replica leave plan <COMMUNITY_ID> \
  --old <REPLICA_ID> --cause <LEAVE_CAUSE> [--w <SUCCESSOR_W>] \
  [--incident <INCIDENT_ID>] [--recovery-disposition <FILE>] \
  --out <LEAVE_PLAN>
iroh-rooms replica handoff prepare propose \
  --plan <LEAVE_PLAN> --out <LEAVE_PREPARE_ENTRY>
iroh-rooms governance approve <LEAVE_PREPARE_ENTRY>
iroh-rooms governance commit <LEAVE_PREPARE_ENTRY>
iroh-rooms replica handoff collect \
  --prepare <LEAVE_PREPARE_ENTRY_ID> --out <LEAVE_HANDOFF>
iroh-rooms replica leave propose \
  --plan <LEAVE_PLAN> --prepare <LEAVE_PREPARE_ENTRY_ID> \
  --handoff <LEAVE_HANDOFF> --out <LEAVE_ENTRY>
iroh-rooms governance approve <LEAVE_ENTRY>
iroh-rooms governance commit <LEAVE_ENTRY> --handoff <LEAVE_HANDOFF>
iroh-rooms replica leave status <LEAVE_ENTRY_ID> [--wait <DURATION>] [--json]

# staged-candidate abandonment; active set is unchanged, so no handoff
iroh-rooms replica stage abandon plan \
  <COMMUNITY_ID> <STAGED_REPLICA_ID> \
  --cause <stage_abandoned|readiness_failed|staged_authority_violation> \
  [--evidence <FILE>] --out <ABANDON_PLAN>
iroh-rooms replica stage abandon propose \
  --plan <ABANDON_PLAN> --out <ABANDON_ENTRY>
iroh-rooms governance approve <ABANDON_ENTRY>
iroh-rooms governance commit <ABANDON_ENTRY>
iroh-rooms replica stage abandon status \
  <ABANDON_ENTRY_ID> [--wait <DURATION>] [--json]

# endpoint-only update: active ReplicaId/store/signer continuity is preserved
iroh-rooms replica endpoint plan <COMMUNITY_ID> \
  --replica <REPLICA_ID> --endpoint-descriptor <FILE> \
  --cause <endpoint_update|endpoint_key_compromise> \
  --continuity-disposition <FILE> --out <ENDPOINT_PLAN>
iroh-rooms replica endpoint propose \
  --plan <ENDPOINT_PLAN> --out <ENDPOINT_ENTRY>
iroh-rooms governance approve <ENDPOINT_ENTRY>
iroh-rooms governance commit <ENDPOINT_ENTRY>
iroh-rooms replica endpoint status \
  <ENDPOINT_ENTRY_ID> [--wait <DURATION>] [--json]

# W-only complete-policy reconfiguration: active ReplicaId set is unchanged
iroh-rooms replica quorum plan <COMMUNITY_ID> \
  --w <SUCCESSOR_W> --out <QUORUM_PLAN>
iroh-rooms replica quorum propose \
  --plan <QUORUM_PLAN> --out <QUORUM_ENTRY>
iroh-rooms governance approve <QUORUM_ENTRY>
iroh-rooms governance commit <QUORUM_ENTRY>
iroh-rooms replica quorum status \
  <QUORUM_ENTRY_ID> [--wait <DURATION>] [--json]

# only when a prepared/fenced active-set transition is abandoned
iroh-rooms replica handoff cancel plan \
  --prepare <PREPARE_ENTRY_ID> [--entry <ENTRY_ID>] --out <CANCEL_PLAN>
iroh-rooms replica handoff cancel collect \
  --prepare <PREPARE_ENTRY_ID> --out <CHECKPOINT_CONTROL>
iroh-rooms replica handoff cancel propose \
  --plan <CANCEL_PLAN> --checkpoint-control <CHECKPOINT_CONTROL> \
  --out <CANCEL_ENTRY>
iroh-rooms governance approve <CANCEL_ENTRY>
iroh-rooms governance commit <CANCEL_ENTRY> \
  --checkpoint-control <CHECKPOINT_CONTROL>

# only after fork.resolve reports fork-frontier reconciliation required
iroh-rooms replica handoff reconcile plan \
  --resolution <FORK_RESOLVE_ENTRY_ID> \
  --target <resume_selected_policy|fresh_selected_prepare_base> \
  --structural-dependencies-out <STRUCTURAL_PROOF> --out <RECONCILE_PLAN>
iroh-rooms replica handoff reconcile collect \
  --plan <RECONCILE_PLAN> --structural-dependencies <STRUCTURAL_PROOF> \
  --dependencies-out <FINAL_DEPENDENCY_PROOF> --out <FORK_FRONTIER>
iroh-rooms replica handoff reconcile propose \
  --plan <RECONCILE_PLAN> --dependencies <FINAL_DEPENDENCY_PROOF> \
  --frontier <FORK_FRONTIER> \
  --out <RECONCILE_ENTRY>
iroh-rooms governance approve <RECONCILE_ENTRY>
iroh-rooms governance commit <RECONCILE_ENTRY> \
  --fork-frontier <FORK_FRONTIER> --dependencies <FINAL_DEPENDENCY_PROOF>
```

`REPLACEMENT_CAUSE` is the closed set `planned_replacement |
operator_reconfiguration | key_loss | key_compromise |
endpoint_key_compromise | sequence_rollback | checkpoint_journal_rollback |
durability_breach | checkpoint_equivocation |
checkpoint_frontier_equivocation | receipt_equivocation`. `LEAVE_CAUSE` is the
same set with `planned_leave` instead of `planned_replacement`. An equivocation
cause requires `--incident`; a loss/compromise/rollback/durability cause,
including replacement after uncertain endpoint/signer separation, requires the
typed `--recovery-disposition` package and its digest enters the immutable plan.
`staged_authority_violation` requires `--evidence`; no free-form reason is an
authenticated cause or audit field.

Endpoint planning proves the old/new endpoint-only delta and exact continuity
of the qualified store, replica signer, receipt high-water, checkpoint/control
journals, and sole-writer fence. If `endpoint_key_compromise` may have reached
the signer/store or any continuity item is uncertain, endpoint planning fails
and directs a replacement with that cause. Endpoint and `W`-only operations use
ordinary predecessor-administrator approval and §6.4's component-root issuance
fence, but no active-set handoff because the active `ReplicaId` set is unchanged.
They are unrelated ordinary governance and cannot extend an open prepare. The
quorum plan commits `transition_kind = quorum_only` and
`status_provenance = operator_reconfiguration`; endpoint planning commits
`transition_kind = endpoint_update` and the selected closed cause.

Stage planning freezes exactly one `join` or `replace(old ReplicaId)` intent.
The later join/replacement plan must match it and never retarget the staged key.
Omitting `--w` means “freeze the exact current `W` unchanged,” not “choose a
default.” If that value violates the successor `R/W` bounds, planning fails and
requires an explicit successor `--w`; every changed value is displayed and
approved. These rules also apply to deliberate higher-`W` reconfiguration.

Equivalent API/frontends may combine file transport ergonomics, but an active-
set change keeps these security stages distinct:

1. **plan** performs no governance mutation and prints every old/new policy
   delta, action kind, context root, `R/W`, key-role/tombstone check, every
   activation's readiness exception (none for pure leave), pending-certificate
   impact, and evidence digest;
2. **prepare** freezes and proposes the exact governance-visible reservation,
   active-set-transition/cancellation intent, canonical frontier-derivation
   rules, and plan identifiers against the governance predecessor (plus each
   applicable staging predecessor); its ordinary approval and
   commit change no replica policy but block unrelated ordinary governance
   successors, subject to the `fork.resolve`/§6.3.1 exception;
3. **handoff collect** obtains a predecessor-`W` prepared checkpoint-fence
   bundle and makes the old-policy issuance fence visible; it mutates no replica
   policy and counts as no administrator approval;
4. **propose and approve the active-set transition** derive the prepared
   `replica.set` child and final frontier only from the installed prepare and
   exact bundle, then show each administrator the exact entry ID, complete old/new
   fingerprints, `R/W`, typed cause, incident/readiness digests, and authorization
   threshold before signing;
5. **commit** re-verifies exact predecessor, signatures, state roots, every
   activation manifest (none for pure leave), required predecessor-`W` handoff/
   fence, and policy, then atomically installs or fails without partial state;
   and
6. **status** follows governed commit and quorum service for every delta;
   activation deltas additionally follow successor observation, endpoint
   binding, storage readiness, and tail reconciliation, while removal deltas
   follow disabled-signer observation/cutover and have no successor when the
   operation is a pure leave. Incident/checkpoint recovery remains separate.

Once `handoff collect` exposes any signature, abandoning the prepared intent or
one of its child proposals does not locally reopen checkpoint voting. The
installed prepare prevents ordinary governance churn from staling a derived
child. The cancellation workflow collects/reuses §6.3's prepare-bound statements,
derives a cancellation child and its exact `W` checkpoint control, obtains the
child's ordinary predecessor-admin approval, and commits both. That commit
advances the governance head; a
detached cancellation alone does nothing, and the sibling prepared active-set transition becomes
stale on the selected branch. Status remains
`handoff_fenced_pending_commit_or_cancel` until either exact child commits. A
timeout, process restart, or operator force flag is not a cancellation.

The reconciliation workflow is available only for §6.3.1's exact
post-`fork.resolve` state. Its immutable plan carries the resolved closure,
latest resolution, selected open original prepare/latest base or none, target,
exact current policy and `R/W`, governance-carried control-exclusion
commitment, structural dependency count/root, prior-unresolved reservation
count, fold-time policy-eligible count, locally authorable/reachable signer
counts, and selected-admin threshold/approval count. The separate structural
proof pages every governance-derived selected/losing context and prior
reservation/closure; the plan cannot inline, omit, or silently truncate it.
`fresh_selected_prepare_base` is rejected when there is no selected open
prepare or an authenticated manifest-bound semantic input changed through
governance; detached/local readiness withdrawal instead blocks service and
drives cancellation/disablement without changing fold validity. Collection
fully verifies the structural proof, obtains and displays the signer IDs/digest
of one current-`W` set of outcome-neutral `ForkResolvedFenceStatement`s, and
unions the structural leaves, each selected statement's committed held set, and
any independently verified late supplemental leaves. It emits a separate final
content-addressed dependency proof whose count/root exactly matches the derived
`ForkResolvedFrontier`. A supplemental leaf is a bundle input, never a mutation
of an exact-replayed stored statement or its held-set root. Competing target
collection exact-replays those statements rather than acquiring another lock.
The derived
`replica.handoff.fork_reconcile` child then receives the selected branch's
ordinary administrator threshold. `reconcile status` exposes the latest
resolution, dependency and closure commitments, canonical prior-unresolved
count (with paginated IDs), consumed/remaining outcome, remaining signer and
administrator counts, fail-closed cause, resulting frontier, and new
reservation base or closed-reservation outcome. Recovery keys perform neither
step.

No command self-approves merely because the local operator is an administrator.
Approval collection may be offline, but every signature binds the exact entry.
An independently changed staging head or semantic input outside §5.4's exact
prepare -> resolution(s) -> rolled-up reconciliation bridge makes the plan/prepare stale
rather than silently rebasing. The exact bridge's head-only progression uses its
governed latest-base rule. An installed prepare rejects unrelated ordinary
successors except that explicit fork-resolution/reconciliation path.
Authorization follows #148 exactly: the verified entry signer and approval-body
approvers form one distinct union intersected with the predecessor administrator
set. An administrator who authors the proposal and also attaches an approval
counts once, while an outsider author contributes no administrator weight.

### 10.4 Plan contents and safety checks

A plan is non-authoritative but content-addressed and contains at least:

- community and exact predecessor governance/component context;
- exact prepare reservation semantics and the two allowed child templates;
- complete old and proposed policies or canonical commitments to both;
- every record/status/descriptor delta;
- old/new active `R`, configured `W`, default/non-default warning, and locally
  eligible/reachable projections;
- readiness manifest IDs and per-stream checkpoint/tail summary;
- immutable canonical handoff terms/commitment and per-stream frontier summary,
  plus the explicit service-stall consequence if handoff cannot complete or
  must be cancelled;
- closed typed transition kind/cause and required incident or continuity-
  disposition evidence ID;
- for fork reconciliation, latest resolution/closure, control-exclusion, and
  structural dependency count/root plus its proof reference; the later
  collection artifact—not the immutable plan—adds the final union count/root
  and complete proof reference bound to `ForkResolvedFrontier`;
- high-water disposition and secret-erasure timing for retired identities;
- pending receipt/checkpoint context impact; and
- expected successor roots and the exact prepare-body/child-template
  commitments.

The content-addressed plan never changes after creation. Collected handoff
signature state and the detached certificate ID live in a separately content-
addressed handoff artifact/status record that binds the immutable plan and
prepare IDs; the later child commits that artifact ID and derived frontier.
The prepare entry ID, bundle/certificate ID, and derived child entry ID live in
their proposal/artifact/status records, never as fields retrofitted into the
immutable plan. Collection does not derive a new plan or mutate prepare bytes.

Plan generation rejects detectable key equality/reuse, a retired identity,
invalid descriptor, missing required readiness/evidence, invalid active count
or `W`, hidden delta, current governance fork, and insufficient historical
policy proof. It warns—but does not lie—about unavailable tail data, stale
checkpoints, reduced failure-domain confidence, and a non-default `W`.

### 10.5 Stable codes and next actions

Phase C adds and pins at least these codes:

| Code | Category | Meaning / fixed recovery intent |
|---|---|---|
| `replica_checkpoint_equivocation` | Integrity | Exact conflicting checkpoint signatures verified; inspect/export evidence and replace the signer. |
| `replica_checkpoint_frontier_equivocation` | Integrity | A signer vote contradicts its signed frontier/fence; inspect/export evidence and replace the signer. |
| `replica_receipt_equivocation` | Integrity | Exact receipt sequence was reused for different signed bodies; inspect/export evidence and replace the signer. |
| `replica_sequence_rollback` | Integrity | Receipt monotonic state is lower/uncertain; do not reset or guess, create a new `ReplicaId`. |
| `replica_checkpoint_journal_rollback` | Integrity | Checkpoint vote/generation/fence state is lower/uncertain; do not reset or guess, create a new `ReplicaId`. |
| `replica_durability_breach` | Integrity | Issued receipt lacks required durable state; stop signing and repair/replace. |
| `replica_quarantined` | Auth | The signer cannot contribute to a new local quorum. |
| `replica_evidence_intake_failed` | Internal | The reserved bounded intake slot is unavailable; block community issuance, repair/exercise the slot, and do not retire an unverified claimed signer. |
| `replica_incident_persistence_failed` | Internal | Verified evidence, quarantine, bounded community barrier/index including trigger-subject saturation, or an authoritative keyed direct-trigger record/cumulative aggregate/source-revision high-water is not durable/readable; all new receipt/checkpoint decisions for the community stay fail-closed. |
| `replica_incident_materialization_blocked` | Internal | Authoritative incident/trigger state is healthy but a bounded per-row-applied derived-state batch cannot advance; keep `Q` excluded, keep stale records pending, repair storage, and resume without force-clearing progress. |
| `replica_readiness_stale` | Integrity | An authenticated bound policy/readiness input or governance head outside §5.4's exact bridge changed; refresh catch-up and rebuild the plan. |
| `replica_not_ready` | Connectivity | Candidate has not passed all stable readiness predicates. |
| `replica_staged_signature` | Integrity | A staged key emitted an unauthorized receipt/checkpoint signature; withdraw local readiness/service, govern cancellation or disablement, and provision a fresh key without arrival-order-reinterpreting exposed governance bytes. |
| `replica_quorum_unavailable` | Connectivity | Fewer than configured `W` eligible replicas can serve; never lower `W` implicitly. |
| `replica_handoff_unavailable` | Connectivity | Predecessor `W` cannot certify the checkpoint frontier, so an active-set change cannot commit. |
| `replica_handoff_unapproved` | Auth | No matching predecessor-admin-approved prepare is installed; do not mutate a signer fence. |
| `replica_handoff_in_flight` | Connectivity | A committed prepare already owns this checkpoint-policy lineage; finish its prepared `replica.set`/cancellation child. |
| `replica_handoff_fenced` | Connectivity | A handoff signature closed old-policy voting; commit the exact entry or govern cancellation/next frontier. |
| `replica_fork_frontier_reconciliation_required` | Integrity | A stable-v2 `fork.resolve` committed; collect the exact-closure current-`W` fork frontier and commit the selected-admin reconciliation child. |
| `replica_fork_dependency_unavailable` | Integrity | The complete canonical structural/final dependency proof is missing, invalid, truncated, or over a leaf/chunk bound; remain fail-closed and repair/fetch the exact proof. |
| `replica_conflict_frontier_required` | Integrity | Incident-bound current-`W` slot consumption must allocate the recovery generation. |
| `replica_receipt_reconfirmation_required` | Connectivity | Quarantine leaves fewer than `W` eligible signers in the prior certificate; recertify exactly. |
| `replica_checkpoint_generation_exhausted` | Integrity | No checked successor generation exists; checkpoint service needs a versioned rollover. |
| `replica_policy_conflict` | Integrity | The plan predecessor or complete successor policy no longer matches. |
| `replica_retired` | Auth | Permanent tombstone forbids staging/reactivation; provision a fresh identity. |
| `replica_governance_forked` | Integrity | Resolve governance through recovery authority before replica operations. |

Categories are exactly the shipped coarse taxonomy and exit scheme: `Internal`
1, `Usage` 2, `Auth` 3, `Integrity` 4, `Ticket` 5, and `Connectivity` 6. Phase C
snapshot-tests every new fine-grained code/category pair. The fixed next action
for either monotonic-state rollback code is semantically:

```text
next: create a replacement ReplicaId; do not reset or guess a receipt counter or checkpoint generation
```

The fixed next action never interpolates keys, paths, endpoints, evidence, or
capabilities.

### 10.6 JSON and evidence export

JSON status includes a schema version and typed enums for governed status,
local eligibility, storage readiness, receipt high-water, checkpoint-vote-
journal/fence disposition, catch-up, convergence, incident, evidence-intake
slot, incident persistence, community incident generation/barrier,
precise trigger-subject count/cap/saturation, materialization phase/cursor/
counts/failure, fork-reconciliation/dependency-proof phase, and quorum service.
Fork fields distinguish structural from final count/root (the
final pair is `pending` before collection), fold-time policy eligibility from
local authorability/reachability, and consumed from unresolved reservations.
Machine fields use full public identifiers where explicitly requested; human
default output may use unambiguous prefixes.

Evidence export writes the exact authoritative canonical artifacts, signatures,
and historical policy witnesses needed to verify the primary proof. It includes
the capped supplementary IDs/bodies retained under §7.5 plus overflow digest/
count metadata, but never claims discarded overflow bodies are exportable. It:

- never exports a signing secret, invite/ticket capability, local database
  path, or unrelated content body;
- creates a new file with restrictive permissions and refuses accidental
  overwrite unless an explicit safe flag is specified;
- warns that public keys, community/stream identifiers, checkpoint roots, and
  timing metadata may still be sensitive; and
- verifies the exported package again before reporting success.

The export is evidence. Human JSON/terminal summaries and `audit.ndjson` are
not substitutes.

---

## 11. Audit and observability

### 11.1 Audit posture

Phase C extends the existing local audit posture. `<IROH_ROOMS_HOME>/audit.ndjson`
remains:

- append-oriented and flushed best-effort;
- local to the operator's data directory;
- permission-restricted where the platform supports it;
- neither remotely shipped, centrally complete, tamper-evident, authoritative,
  nor a compliance archive; and
- non-blocking for protocol safety—an audit write failure emits the existing
  warning but cannot turn invalid evidence into valid evidence.

Exact incident bytes live in the authoritative v2 evidence store. Audit records
carry minimized linkage only.

### 11.2 Event names

Pin at least these lowercase event names:

```text
replica.stage.proposed
replica.stage.committed
replica.stage.abandon_proposed
replica.stage.abandoned
replica.readiness.changed
replica.join.proposed
replica.join.committed
replica.join.converged
replica.leave.proposed
replica.leave.committed
replica.leave.converged
replica.replacement.proposed
replica.endpoint.proposed
replica.endpoint.committed
replica.endpoint.converged
replica.quorum.proposed
replica.quorum.committed
replica.quorum.converged
replica.handoff.prepared
replica.handoff.fenced
replica.handoff.cancelled
replica.handoff.fork_reconciled
replica.replacement.committed
replica.replacement.converged
replica.equivocation.detected
replica.quarantine.applied
replica.evidence_intake.failed
replica.incident.barrier_committed
replica.incident.persistence_failed
replica.incident.materialization_started
replica.incident.materialization_completed
replica.incident.materialization_blocked
replica.signer_cutover.certified
replica.sequence.rollback_detected
replica.checkpoint_journal.rollback_detected
replica.durability.breach_detected
replica.receipt.reconfirmation_required
replica.checkpoint.recovery_certified
replica.operation.failed
```

Failures name a stable cause code; they do not embed an arbitrary error chain
that could contain paths or network inputs.
Materialization events are aggregate phase transitions, not one audit row per
stream, certificate, or batch retry.

### 11.3 Common fields

Each applicable record contains:

```json
{
  "ts_ms": 1785792000000,
  "event": "replica.equivocation.detected",
  "severity": "critical",
  "community": "<safe pseudonymous id>",
  "replica": "<safe pseudonymous id>",
  "incident_id": "<digest>",
  "artifact_kind": "stream_checkpoint",
  "checkpoint_generation": 42,
  "artifact_ids": ["<id-a>", "<id-b>"],
  "governance_head": "<safe digest>",
  "replica_component_root": "<safe digest>",
  "configured_r": 3,
  "configured_w": 2,
  "eligible_after": 2,
  "action": "new_quorum_weight_withheld"
}
```

Transition records additionally carry a closed `transition_kind`, typed cause,
old/new policy roots, old/new `R/W`, subject/replacement safe IDs, readiness/
incident/continuity-disposition digest, governance entry ID, result, and
convergence state. Handoff/fork records also carry original prepare ID, latest
resolution/reservation base, resolved-closure commitment, control-exclusion
commitment, structural and final dependency count/root, prior-unresolved count,
and consumed/remaining outcome where applicable. Approval records, if logged,
use safe administrator identifiers and counts; they never include secret key
material or free-form causes.
Incident materialization phase records carry only the checked community
generation, phase, scan kind, opaque cursor digest, processed/remaining
projection-unit count (`unknown` allowed), configured projection-unit/byte caps,
and a stable bounded failure cause. They do not carry a stream/certificate list.

The record MUST NOT contain:

- raw signed bodies, signatures, event/content/blob bytes, or full evidence;
- private keys, seeds, tickets, invite secrets, capability tokens, or nonces;
- endpoint addresses/discovery hints, region/operator free text, or local paths;
- full arbitrary error messages from network/storage inputs; or
- a claim that audit persistence proves incident authenticity.

### 11.4 Metrics and status

Expose bounded metrics for:

- governed records by lifecycle state and locally eligible active count;
- configured `R/W`, reachable eligible count, and quorum-unavailable duration;
- staged catch-up bytes/items, governance cursor, per-stream checkpoint/tail
  lag, and readiness causes;
- `replica_transition_total{kind,outcome}` and
  `replica_transition_latency_seconds{kind,phase}` for the closed bounded kinds
  `stage`, `stage_abandon`, `join`, `leave`, `replace`, `endpoint_update`, and
  `quorum_only`;
- handoff single-flight/fenced/cancellation/fork-reconciliation state and
  duration;
- fork structural/final dependency counts, bounded chunk processing, and
  rejected proof counts by stable bounded cause;
- quarantine and incident counts by stable cause, not by high-cardinality raw
  identity;
- retained incident-body and precise trigger-subject cap usage, saturated-
  incident counts, overflow counts, and evidence-intake-slot state;
- incident-barrier generation and materialization phase, backlog when known,
  accounted batch projection units/bytes, retry/failure totals, and phase
  duration under closed low-cardinality labels;
- receipt/checkpoint-journal rollback and durability breach counts;
- current stream `CheckpointEquivocated`, conflict-frontier/cutover, and recovery
  state; and
- rejected stale-context receipts/votes, receipt reconfirmation, and
  recertification work.

Metrics labels are bounded and pseudonymous. Exact IDs and evidence remain in
explicit status/evidence APIs.

---

## 12. Historical authorization, retention, and privacy

### 12.1 Historical replica-policy witness

A verifier of a retained receipt or replica checkpoint needs more than the
signing public key. It must prove, at the artifact's exact named context:

- the community and governance head;
- the complete replica component root;
- the signer's descriptor and active/eligible status;
- configured `W` and governed durability class;
- the administrator-authorized transition path to that policy; and
- any prepare/base, handoff/cancellation/fork-reconciliation, signer cutover,
  incident, or recovery state that affects the current use of the artifact.

#161's compact administrator-transition manifest may omit ordinary
`replica.set` operations. Its current snapshot proves one current replica root,
not arbitrary historical roots. Phase C therefore freezes a historical
replica-policy witness or retains the exact governance entries/checkpoints/
snapshots needed to reproduce each policy. A latest snapshot never authorizes
an old receipt by itself.

### 12.2 Non-compaction set

Do not compact or garbage-collect:

- exact disabled/staged/active transition entries and administrator approvals
  needed for retained policy roots;
- prepare/base, handoff/cancellation/fork-reconciliation governance entries
  and checkpoint controls plus retired-signer cutovers needed for retained
  frontiers;
- historical descriptors, role-key assignments, and permanent tombstones;
- exact receipts, checkpoint votes/certificates, and their governing witnesses
  while referenced by retained content/status;
- each signer incident's primary objective proof pair and required witnesses,
  bounded incident state/overflow digest, quarantine/resolution/cutover linkage,
  authoritative community barrier/high-water and keyed signer-incident index
  including trigger-subject saturation,
  keyed direct-trigger records/cumulative subject aggregates/source-revision
  high-waters, and recovery checkpoints;
- governance-fork/recovery evidence required by those policies; or
- checkpoint/retention/publication evidence required to validate the candidate
  catch-up state.

A governance-authorized content-retention boundary may delete content according
to its own proof only after all evidence above that remains independently
required is retained. Incident closure is not permission to erase the primary
proof. Supplementary old-key bodies beyond §7.5's hard cap are explicitly not
in the non-compaction set: after strict verification updates the bounded digest/
count, their bodies and extra witness packages are discarded. Derived
materialization cursor/phase, per-row applied markers, and stale-pair index may
also be reset or reconstructed, but only conservatively behind the retained
barrier; compaction must never turn their absence into a current/green result.

### 12.3 Privacy

Replica descriptors are governance state, not secrets, but discovery hints,
operator labels, regions, checkpoint roots, event counts, and incident timing
can reveal deployment/community information. Implementations:

- disclose full descriptors/evidence only through authenticated authorized
  paths or explicit operator export;
- minimize default logs, metrics, and terminal prefixes;
- keep staged catch-up scoped to the named community and required retained
  state;
- never include invite/ticket secrets in descriptor, readiness, plan, incident,
  or audit records; and
- rate-limit evidence/catch-up requests to avoid using an incident as an
  amplification or history-exfiltration path.

Cryptographic evidence is shareable for independent verification only after
the operator accepts those metadata disclosures.

---

## 13. Security properties and non-claims

### 13.1 Properties

Assuming the governance administrator threshold remains honest and the final
codecs/crypto are correct:

- a staged candidate cannot add receipt/checkpoint weight before an approved
  atomic activation;
- a request lacking an installed predecessor-admin-approved prepare cannot fence
  predecessor checkpoint service; a child still needs ordinary approval to
  commit;
- a one-for-one replacement never counts old and new as two active seats and
  has no disable-first intermediate policy;
- no failure/recovery path silently lowers `W` or durability class;
- old/new-root receipts or votes cannot combine;
- a disabled signing identity cannot reactivate;
- an objective same-signer checkpoint/receipt conflict or vote/frontier
  contradiction causes fail-closed local containment and governed retirement;
- invalid, stale, out-of-order, or session-only behavior does not accidentally
  satisfy the equivocation predicate;
- receipt/checkpoint-journal rollback cannot be hidden by choosing a larger
  counter or generation;
- a current-`W` cutover prevents a compromised retired key from reopening
  covered historical completeness one slot at a time;
- one signer's valid-artifact flood cannot exceed the hard retained-body/
  witness or lifetime eight-subject precise-trigger cap; omitted subjects use
  fixed saturation metadata plus the already-active conservative barrier/index
  mode, never one durable row per artifact/subject;
- an evidence-intake/bounded-barrier-write failure survives restart in the
  reserved slot or storage-fault state and cannot fail open; an empty healthy
  slot plus a durable incomplete materialization resumes from validated progress
  or conservatively restarts its scan, while an ordinary crash with neither
  condition does not invent an incident;
- no incident-activation transaction grows with retained stream/certificate
  count, and a failed derived-state batch cannot re-include `Q` or make stale
  status green; and
- historical artifacts remain independently auditable under the exact policy
  that authorized them.

### 13.2 Non-claims

This profile does not prove:

- that a signed readiness manifest corresponds to honest stable storage;
- that distinct public keys run on distinct hosts, disks, networks, power
  domains, organizations, or operators;
- malicious intent, the moment a signing key was compromised, or which of two
  conflicting checkpoint roots is true;
- global completeness of incident discovery or an uncheckpointed tail;
- recovery of data whose last valid copies are lost;
- liveness with fewer than configured `W` eligible replicas;
- newest governance/checkpoint state from one unpinned peer;
- immunity to a compromised administrator quorum or `W` colluding replicas;
- indefinite retention beyond governed certified policy; or
- economic accountability, trusted hardware attestation, or Byzantine
  consensus among arbitrary public replicas.

Quarantine is prospective local containment. Ed25519 signatures do not carry a
trusted creation time, so permanent retirement cannot make a forged old-key
artifact cryptographically impossible.

### 13.3 Abuse resistance

Evidence import and staged catch-up are network-derived bounded paths. They
strict-decode and hash incrementally, cap record/list/proof/body sizes before
allocation, reject duplicate/unreferenced entries, bound decompression, apply
per-peer/community concurrency and byte budgets, and reserve governance/
revocation capacity. An invalid evidence flood never triggers repeated
governance proposals or unbounded audit output.

Signer-incident deduplication occurs only after an objective fault verifies;
unverified reporter-supplied IDs do not allocate permanent state, advance the
bounded overflow digest, or quarantine a signer. Evidence bodies beyond the hard
cap are verified incrementally and discarded after their ID updates the fixed-
size overflow metadata. Direct-trigger subjects consume lifetime, non-reusable
slots in the incident's eight-entry precise set; create/delete/reimport cycles
cannot free slots for attacker-chosen growth. Further subjects set one fixed
saturation bit and remain covered by conservative signer-incident mode.

The preallocated intake slot is reused serially, not allocated per reporter or
artifact. Implementations cap the number of served communities and total
reserved intake bytes; community creation/enablement fails before service when
its slot cannot be reserved and exercised. Per-peer admission and retry budgets
prevent a stream of invalid maximum-size candidates from monopolizing the slot.
The materializer uses one coalesced cursor over existing canonical indexes,
bounded projection-unit/byte working memory, capped retry/backoff, and aggregate
audit/metrics. New incidents advance the community high-water and coalesce with
the same scan; they do not allocate per-record queues. If evidence arrives faster
than projection, admission backpressure may delay further evidence processing
but cannot delay durable barrier activation for a candidate already accepted,
drop either subject of a receipt trigger set, lose saturation coverage, or make
a stale record usable.

---

## 14. Current repository compatibility

### 14.1 Phase-B pure core

`crates/iroh-rooms-v2-core` is `publish = false`, pure, and unused by the
shipping runtime. It contains no replica network/store runtime, receipt or
publication-certificate type, durability-class implementation, stable `W`,
replica-signed stream checkpoint, staged catch-up, readiness manifest,
incident evidence, or operator API.

The candidate code's `ReplicaStatus::{Active, Disabled}`, opaque
`ReplicaDescriptor`, one-record `ReplicaSet`, and total map-upsert apply path
cannot satisfy this spec. They remain historical scaffolding. This pure-spec
issue MUST NOT add misleading lifecycle methods to those types or claim that
their successful fold is a replacement implementation.

### 14.2 Frozen vectors and identifiers

Candidate golden material remains byte-identical and explicitly labeled:

- descriptor-hash-derived candidate `ReplicaId` is not relabeled as the raw
  stable signing key;
- snapshot format 1 keeps only `active|disabled`, opaque endpoint/capability,
  and no `W`/class;
- the candidate `replica.set` payload remains a single-record upsert;
- no current checkpoint fixture is called a replica stream checkpoint; and
- absence of receipt/readiness/evidence fixtures remains an honest gap.

Phase C adds successor genesis/community, descriptor/policy, state-root,
snapshot, governance operation, readiness, receipt, checkpoint, evidence,
history-proof, and rejection vectors. It never rewrites a frozen file value in
place or selects candidate semantics through stable negotiation.

### 14.3 v1 and admin divergence

The shipped v1 store uses behavior that is not a v2 receipt class, and its
session acknowledgements are not replica durability receipts. This spec does
not change them.

Replica checkpoint equivocation is also unrelated to v1 administrator
divergence. v1 `admin_seq` remains DAG depth across the complete carrier chain,
and ADR-0006's fold-level membership/authz divergence detector remains advisory.
It MUST NOT be scoped, repurposed as a replica sequence, turned into automatic
ejection, or made fail-closed by this work.

### 14.4 Release posture

This spec changes no rc.5 binary or qualification claim. It closes a v2 design
queue item and creates Phase C requirements. No public v2 interoperability
claim is permitted merely because the semantic decision merged.

---

## 15. Phase C implementation and verification gate

### 15.1 Required owners and formats

Before stable v2 advertising, named owners freeze and implement:

1. **Governance/genesis/snapshot owner** — successor full descriptor, lifecycle
   policy, explicit `W`, class, tombstone commitment, atomic full-state
   `replica.set`, committed handoff-prepare reservation/latest-base state and
   allowed-child serialization, governance-carried control-signer exclusions,
   durable pending fork-frontier/rolled-up dependency count/root and exact
   clearing state, approved-entry/handoff attachment boundary,
   governed handoff-cancellation and fork-frontier-reconciliation operations,
   fold-time readiness/local-overlay separation, roots, snapshot, new
   `CommunityId`, strict codecs, and vectors.
2. **Identity/handshake owner** — staged and active two-key binding, bounded
   catch-up authorization, current/historical role-key rejection, and endpoint
   rotation behavior.
3. **Readiness/store owner** — manifest/body/domain, stable catch-up commits,
   receipt high-water and checkpoint-vote-journal disposition, storage faults,
   authenticated fold-time staleness versus detached local service withdrawal,
   preallocated bounded evidence-intake slot, size-bounded incident-barrier
   activation, checked community generation, fixed direct-trigger record and
   cumulative subject aggregate, canonical one-or-two-subject derivation,
   lifetime eight-subject cap/saturation, projection-unit/byte-accounted
   materialization transactions, generation/revision-bound compare-and-commit,
   conservative cursor/catalog recovery, and no-dual-writer fence.
4. **Receipt/publication owner** — exact receipt sequence/conflict evidence,
   post-quarantine current-status/reconfirmation behavior, stale-generation read
   gate and bounded per-certificate materializer, publication-certificate
   behavior, and exact-head progress during churn.
5. **Stream-checkpoint owner** — body/vote/certificate, stable-retention signing
   predicate, exact checked generation succession, durable vote journal,
   prepared single-flight handoff/governed-cancellation fence, fork-resolved
   outcome-neutral `ForkResolvedFenceStatement`, target-tagged
   `ForkResolvedFrontier`, fixed-size structural/final dependency commitments,
   complete bounded-chunk proof and streaming verifier, durable-before-release
   journals for every frontier
   control, single-vote and vote/frontier fault rules, incident-bound conflict-
   slot controls, stale-generation conservative stream projection and bounded
   materializer, retired-signer cutovers, recovery checkpoint, and vectors.
6. **Evidence/history owner** — canonical incident/evidence package, historical
   replica-policy witnesses, one signer incident, fixed body/witness caps and
   overflow digest, fixed direct-trigger-subject cap/saturation, tombstone
   accumulator, prepare/fork-control/dependency-proof retention, resource
   limits, and independent verifier.
7. **CLI/API owner** — commands, plan/proposal files, stable error/JSON schemas,
   join/leave/replacement/staged-abandonment, endpoint update, quorum-only
   reconfiguration, handoff/cancellation/fork-reconciliation/reconfirmation
   workflow, evidence export, community barrier/materialization progress and
   recovery actions, aggregate audit events, bounded metrics/status schemas,
   redaction, and docs.

An owner may cover multiple rows, but no row disappears by implication.

### 15.2 Public codec and vector matrix

Add independent positive and negative vectors for public protocol artifacts:

- successor genesis/community identifier and complete replica-policy root;
- full descriptors, every lifecycle status and allowed edge (including genesis-
  only initial admission and terminal staged abandonment), explicit `R/W`, class,
  sorting, duplicate/current/historical role-key collisions, and permanent tombstones;
- stage, pure join, pure leave, endpoint update, `W`-only reconfiguration,
  replacement, staged
  abandonment, compromise, rollback,
  durability-breach, and all checkpoint-double-vote, checkpoint-frontier, and
  receipt objective fault causes;
- old-admin authorization, `W` boundaries, hidden policy delta, stale
  predecessor, partial update, governance-carried control-signer exclusions,
  verifier-local quarantine/readiness non-inputs, and recovery-key misuse;
- readiness manifest, per-stream commitment, wrong descriptor/head/root/class/
  `W`, stale retention/checkpoint, archived-stream omission, bad signature,
  staged-key artifact signature, and oversized inputs;
- predecessor-policy prepare/handoff/frontier/cancellation, unprepared or
  unapproved request with zero fence mutation, exact replay, bundle-derived
  children, competing-prepare fork, unrelated-successor rejection, insufficient/
  disjoint/quarantined signers, wrong proposed entry/policy, stale frontier,
  detached-cancellation replay, and governed cancellation;
- open-prepare `fork.resolve` exception, unconditional structural trigger for
  open/closed/absent-prepare forks even with no observed detached artifact,
  exact-control-only clearing, nested unresolved-resolution roll-up and exclusion
  carry-forward, original-prepare/latest-base linkage, outcome-neutral
  `ForkResolvedFenceStatement`, target-tagged `ForkResolvedFrontier`, exact
  statement replay across competing resume/fresh-base targets, selected/losing
  statement supersession, structural-proof versus collected final-union proof,
  signer-held and late supplemental leaf inclusion without changing stored
  statement bytes/root, final count/root binding, boundary-size leaves/chunks,
  duplicate/order/omission/mismatch/oversize/truncation/unavailable/count-
  overflow negatives, rejection of structural-only proof at propose/commit,
  late hidden siblings, and recovery/admin/replica authority separation;
- exact receipt sequence conflict and benign out-of-order delivery;
- exact checkpoint same-slot and vote/frontier conflicts, exact successor
  allocation, two-vote/no-certificate conflict-slot consumption, arbitrary
  generation jump/overflow, and every §7.3 non-conflict;
- canonical evidence ordering/ID, signer-incident deduplication, wrong historical
  eligibility, duplicate/malformed artifacts, and exact retained-body/witness
  hard-cap acceptance/rejection behavior;
- retained-proof and conservative signer-incident conflict-frontier modes,
  retained bodies with present versus missing/invalid eligibility witnesses,
  contiguous range accounting, retired-signer cutover, `supersedes`
  representation, old-context late artifacts above/below the frontier, and
  wrong-generation/root/context/certificate negatives; and
- historical policy inclusion/non-inclusion and forbidden compaction.

A second implementation reproduces canonical bytes, identifiers, signatures,
roots, evidence IDs, and all positive/negative public-artifact decisions.
`EvidenceIntakeSlot`, `CommunityIncidentBarrier`, quarantine/index generations,
trigger-subject caps/saturation, direct-trigger records/cumulative subject
aggregates/revisions, catalog generation, materialization cursor/phase, per-row
applied markers and stale-pair index, bounded overflow audit metadata, and
projection-unit/byte transaction accounting are
deliberately excluded from this public codec matrix.
They require local store-schema and crash/property conformance below; any byte
fixtures used by a backend are explicitly non-wire.

### 15.3 State-machine and property tests

Properties generate bounded policies and histories and prove:

- only allowed lifecycle edges occur and disabled never leaves disabled;
- only authenticated genesis admits `provisioned -> active`; post-genesis
  admission stages first, and `staged -> disabled` never gains weight;
- staged records contribute zero active count/weight;
- every accepted policy has valid `R/W`, key roles, class, and complete root;
- atomic replacement changes exactly the planned old/new seats with no
  observable intermediate policy;
- pure join requires staged/readiness inputs but no disabled predecessor, pure
  leave requires no staged/new identity or readiness manifest, and both use the
  common active-set prepare/handoff with exactly their planned deltas and no
  hidden seat/status change;
- endpoint and `W`-only changes preserve the active `ReplicaId` set, use the
  component-root issuance fence without a prepared handoff, and cannot extend
  an open prepare; `R` is always derived from exact membership rather than
  caller input;
- predecessor administrators, not successor/recovery/replica keys, authorize;
- `W-1` does not certify and `W` distinct eligible signers does;
- reachability/quarantine never changes configured `W`;
- context/root/class/identity mixing never certifies;
- a stale readiness manifest never activates;
- delivery order of local readiness withdrawal, quarantine/evidence, and an
  already exposed governance child never changes fold/apply/state roots; local
  authoring/service still stops, and only governance-ordered cancellation/
  disposition changes child validity;
- one committed prepare admits only its prepared `replica.set` or cancellation child derived
  from a valid prepared `W` bundle and rejects every unrelated ordinary
  successor until one commits, while `fork.resolve` triggers mandatory fork-
  frontier reconciliation without clearing a fence by recovery authority;
- every accepted stable-v2 `fork.resolve` structurally triggers fork-frontier
  reconciliation for open, closed, or absent prepares without consulting
  detached artifact observations; its signer statement is outcome-neutral and
  exact-replayed across both valid targets; it never rewrites/reuses a losing
  signature, advances only from the latest reservation base, and cannot commit
  with recovery-key weight;
- a later `fork.resolve` before reconciliation rolls every unresolved ancestor/
  losing reservation, statement, closure, and applicable exclusion into the new
  reservation; only the latest exact child consumes all of them;
- the collected final fork-dependency root is the canonical union of the
  structural proof, selected statements' committed held sets, and independently
  verified supplemental leaves; adding a supplement never changes exact-replay
  statement bytes, and propose/commit accepts only the complete proof whose
  count/root equals `ForkResolvedFrontier`;
- nested dependency depth never enlarges a signed/control body or one transport
  chunk; chunked and one-shot verification produce the same root, while any
  omitted, reordered, oversized, truncated, unavailable, or overflowing input
  fails closed;
- on one selected, non-forked governance lineage, consecutive active-signer
  policies cannot certify conflicting same-generation checkpoints across the
  predecessor-`W` handoff without an individual checkpoint double-vote or vote/
  frontier contradiction; sibling governance outcomes instead require §6.3.1;
- receipt/checkpoint rollback never resumes by counter or generation guessing;
- only an exact checked authenticated successor generation can certify;
- evidence pair order does not change evidence identity, and every proof for one
  signer maps to one signer incident;
- each positive equivocation pair quarantines exactly one signer;
- exact conflict-frontier mode is impossible without currently verifiable
  witnesses for both retained bodies; missing witnesses select conservative
  signer-incident mode;
- every negative §7.3 case leaves governance/quarantine unchanged;
- a non-empty/unreadable evidence-intake slot or incident storage fault never
  reopens issuance before replay or explicit recovery; an empty healthy slot
  with a recovered barrier resumes validated progress or conservatively restarts
  from the canonical beginning/lowest stale key, while one with no barrier
  follows ordinary crash recovery;
- incident activation is independent of retained record count; every
  materialization transaction stays within both caps, cursor advancement never
  precedes its covered projection units, each unit applies one incident to one
  subject, each batch stamps only consecutively processed intermediate
  generations at or below its captured target, and duplicate/reordered replay is
  idempotent;
- a row with more than 256 intervening incidents advances across multiple
  transactions, remains pending until it reaches the target generation/revision,
  and never requires one transaction to fold its complete incident history;
- a `G+1` incident racing a `G` batch or finalization makes the old transaction
  abort or commit only through `G`; the older completion cannot overwrite the
  newer pending target because both batch and completion compare incident and
  catalog generations;
- same-generation supplementary evidence that adds new subject coverage or a
  monotonically stronger disposition while racing a materializer, cache read,
  or target issuance and changes the cumulative aggregate increments the
  directly named source revision, aborts a stale merge, and invalidates that
  subject's cached green result without a new community generation or full scan;
- two conflicts for the same `(subject, signer_incident)` with the same coarse
  disposition but different affected generations/frontiers converge in both
  delivery orders to the same earliest or `conservative_frontier_unknown`
  recovery obligation; a changed aggregate increments the source revision,
  while a true no-change aggregate does not;
- when one subject has trigger records `A@G1` and `B@G2`, strengthening `B` and
  then `A` (and the reverse) produces one identical cumulative aggregate; a
  direct-refresh unit applies that whole aggregate at the captured source
  revision, so incident-generation cursor order cannot skip either change;
- duplicate/replayed evidence whose conservative trigger join is unchanged,
  including after overflow saturation, increments no revision/catalog
  generation, invalidates no cache, and schedules no projection work; weaker or
  reordered evidence cannot downgrade the retained trigger;
- a pair-record join dominated by an already more-conservative subject aggregate
  persists the exact authoritative pair change but increments no source/catalog
  generation, invalidates no cache, and schedules no projection work;
- checkpoint evidence derives exactly one trigger subject; receipt evidence with
  two distinct body subjects derives a canonical two-subject set and atomically
  updates both, while two equal subjects deduplicate to one and a fault between
  subject writes commits neither;
- lifetime direct-trigger cardinality stays at eight per signer incident across
  subject create/delete/reimport cycles: cap-minus-one and cap admit precise
  records, cap-plus-one sets only `direct_trigger_subjects_saturated`, and every
  omitted subject stays safe in the barrier's pre-existing conservative mode;
- after a current-`W` cutover/recovery or receipt recertification covers a
  retired signer/context, a flood across more than eight new old-context
  subjects changes only capped audit metadata and cannot allocate triggers,
  invalidate caches, or reopen completeness; separate signer incidents retain
  independent eight-subject caps;
- local overflow digest/count metadata obeys its exact retention/saturation cap,
  never enters a public recovery-control identity, and cannot turn replay into
  unbounded state, cache invalidation, or projection work;
- any row below the community incident generation is conservatively pending on
  every status, certification, checkpoint-base, reconciliation, and serving
  path; materialization failure cannot re-include a quarantined signer or
  restore a stale green result;
- genuinely new Q-free rows may stamp the current generation only in their
  admission transaction with the exact Q-excluding context and applicable §8
  cutover/recovery/recertification proof, while imports/restores and concurrent
  incidents cannot hide behind an advanced cursor;
- quarantine always recomputes current receipt-certificate eligibility; and
- recovery checkpoints cannot erase the primary conflict proof, and a later
  cut-over signer artifact cannot reopen any covered historical range.

### 15.4 Crash and distributed tests

The deterministic distributed matrix includes:

1. default `R=3,W=2` planned replacement with publications/checkpoints before,
   during, and after stage/handoff/activation, proving no mixed or disjoint-
   policy certificate;
2. genesis-only initial admission plus post-genesis stage catch-up from a
   certified checkpoint, process crash, resume, readiness refresh, pure join,
   and tail reconciliation, including `R=3,W=2 -> R=4,W=3`; a companion run
   proves staged abandonment gains no weight and a pure
   `R=4,W=3 -> R=3,W=2` leave needs no staged identity/manifest while still
   using the active-set prepare/handoff;
3. active signer/store crash with exact receipt high-water, checkpoint vote
   journal/fences, and WAL recovery followed by same-ID resume;
4. restored old backup and cloned writer, each preventing same-ID issuance and
   requiring replacement;
5. one offline replica, one quarantined replica, and two unavailable replicas,
   proving degraded versus quorum-unavailable output without `W` reduction;
6. endpoint-only rotation with continuous counter and no old-endpoint issuance
   after the new head, plus `W`-only reconfiguration with unchanged active
   identities and no prepared handoff, each proving the coarse component-root
   issuance fence;
7. default-`W` key compromise and replacement using the remaining predecessor
   quorum, with no failed-key signature or governance consent;
8. same-slot checkpoint conflicts delivered in both orders, across governance/
   component transitions, and after restart;
9. lower-generation/out-of-order, invalid-signature, withholding, and RBSR
   disagreement negatives that do not quarantine;
10. conflicting checkpoint recovery through an incident-bound current-`W`
    conflict-slot frontier and exact-successor certificate, including two votes
    at `g` with no certificate and recovery at exactly `g+1`;
11. unresolved governance fork blocking stage/activation until recovery then
    ordinary old-state administrator approval;
12. more retained streams/certificates than one backend transaction can hold,
    with evidence-intake, bounded barrier activation, and existing-incident
    authoritative direct-trigger/saturation updates faulted at every write
    boundary, crash after barrier commit/slot clear before the first batch, and
    faults before/after each capped projection-unit+byte batch/cursor/
    finalization commit. At least one restored row is behind by more incident
    generations than the unit cap.
    The run proves exact replay; atomic persistence of the canonical one- or
    two-subject trigger set before a supplementary body is discarded; no partial
    pair after a fault between two receipt subjects; empty-slot-plus-pending-
    barrier recovery; idempotent resume; concurrent new Q-free rows; stale
    imported/restored rows;
    a `G+1` activation racing both a `G` batch and `G` finalization, and a same-
    `G` supplementary direct trigger racing materialization/cache read/issuance,
    plus same-disposition earlier/different-frontier evidence in both orders,
    reversed incident-generation versus source-revision updates across two
    trigger records for one subject, and duplicate/saturated replays whose
    conservative trigger join is unchanged.
    It also covers cap-minus-one/cap/cap-plus-one admission, lifetime slot use
    across create/delete/reimport, a fault on the first saturation-bit write,
    post-cutover old-context flood without recovery reopening, and independent
    caps for multiple signer incidents. Loss or corruption of the derived stale-
    pair index forces a conservative rebuild and cannot prove completion.
    It proves intermediate-generation progress, target-generation compare-and-
    commit, subject-revision invalidation, multiple coalesced signer incidents,
    no transaction over either cap,
    Q-excluding `W` versus `W-1` behavior during partial progress, and barrier-
    intact materialization blocking separately from community-wide persistence
    failure and best-effort NDJSON failure;
13. restored vote-journal rollback, arbitrary generation jump, and generation
    exhaustion, each failing closed without same-key guessing or wrap;
14. staged-key unauthorized signing, candidate readiness withdrawal, and
    archived-stream catch-up omission delivered before and after an already
    exposed child, proving identical fold/state roots, immediate local service/
    approval withdrawal, governed cancel/disable, and no false eligible-
    equivocation case;
15. repeated requests lacking a committed prepare causing zero journal/fence
    mutation, exact prepare-request replay returning identical bytes, sibling
    preapprovals unable to split signer reservations, and competing child
    proposals reusing the one prepare-level reservation rather than acquiring
    per-child locks;
16. prepared/fenced replacement, pure join, and pure leave transitions abandoned
    through independently admin-approved, current-`W`, committed cancellation
    children—including prepare then candidate-readiness withdrawal—with an
    attempted unrelated ordinary governance successor rejected while each
    reservation is open, proving cancellation remains collectable, cannot stale,
    and a detached replay is ineffective;
17. an old checkpoint certificate plus contradictory handoff frontier plus new
    certificate, proving checkpoint-vote/frontier evidence quarantines the
    intersection signer;
18. retired-signer cutover followed by valid-looking artifacts across more than
    eight streams/slots and above the cutover frontier, proving conservative
    current-`W` recovery remains possible with no completeness reopen, no new
    signer incident, and hard-capped retained bodies, including `R=3,W=2` where
    Q conflicts at `g` while the other two certify a third body at `g` and that
    third body still cannot skip slot closure;
19. receipt equivocation on an already accepted certificate, proving current
    status downgrades below eligible `W` and clears only after exact-context
    reconfirmation;
20. crash after local signing but before send/release for checkpoint votes,
    handoffs, cancellation controls, abandoned-slot controls, conflict-slot
    frontiers, and retired-signer cutovers, proving exact retry after restart and
    mandatory replacement whenever the journal is uncertain;
21. a fork with no detached replica artifact observed, sibling prepares hidden
    until after partial and full `W` fences, plus
    activation/cancellation siblings derived from different valid `W` bundles
    and a closed losing activation that has emitted checkpoint votes, followed
    by `fork.resolve` selecting each branch/child in turn and a crash during
    fork-frontier reconciliation. The run proves that one outcome-neutral
    statement is exact-replayed across competing fresh-base/resume targets, the
    current-`W` control supersedes all losing frontier activity, advances the
    selected prepare's latest base or closes it, rejects recovery-key quorum
    substitution, and leaves no stranded fence or reusable generation. A nested
    run issues `ForkResolvedFenceStatement`s for resolution F1, commits F2 before
    F1's child, crashes, and proves F2's rolled-up statement/control atomically
    chains and consumes both reservations/closures with no `admin_seq` change.
    Signer-held and independently verified late supplemental leaves enter F2's
    final proof without changing exact-replayed statement bytes; structural-only
    or count/root-mismatched proof fails at propose/commit. Boundary-size and
    over-limit leaf/chunk cases prove fixed signed-body and frame bounds.

Crash tests use the qualified durable profile, abrupt process/OS/power-style
fault injection where available, and real persisted stores. In-memory or clean-
shutdown-only tests are not durability evidence.

### 15.5 Operator contract tests

Snapshot-test:

- human and JSON status for every lifecycle/readiness/quarantine/quorum state;
- complete immutable plan delta, committed prepare state, separate handoff
  artifact/status, fork-frontier-reconciliation workflow/status,
  endpoint-update and quorum-only no-handoff paths, unapproved/single-flight
  errors, governed cancellation, and stale-plan rejection;
- approval display of exact roots, `R/W`, closed typed cause, readiness/incident
  or continuity-disposition digest;
- every stable warning/error code, category, exit status, and fixed `next:`
  line;
- evidence export permissions, overwrite refusal, re-verification, and privacy
  warning, including bounded overflow metadata;
- receipt reconfirmation and evidence-intake-slot recovery states;
- community incident-barrier generation plus every pending/running/blocked/
  complete materialization phase, cursor/high-water/progress representation,
  unknown remaining count, fixed repair/resume action, and proof that completion
  does not clear quarantine/cutover/reconfirmation;
- precise direct-trigger-subject count/cap/saturation in human and JSON incident
  status, including cap-minus-one/cap/cap-plus-one snapshots;
- structural-pending versus final fork-dependency status, paginated prior-
  reservation/closure display, fold-time eligibility versus local
  authorability, and consumed nested-resolution outcomes;
- audit event names/fields plus exhaustive absence of secrets, raw bodies,
  signatures, endpoints, operator labels, and paths; and
- metric names, closed transition/phase/outcome/cause label enums, rejection of
  raw/high-cardinality identity labels, and bounded series cardinality.

---

## 16. Normative amendments to #134 and dependent profiles

### 16.1 Governance replica operations (§§7.2–7.3)

Read stable-v2 genesis and `replica.set` with this addition:

> The governed replica component uses an explicitly versioned complete policy
> containing full descriptors, lifecycle status, exact durability class,
> explicit receipt/checkpoint quorum `W`, and authenticated permanent role-key/
> disabled-identity history. `staged` grants only bounded catch-up authority and
> zero quorum weight. Activating a signing-key replacement atomically changes
> old `active -> disabled` and new `staged -> active` in one full-policy
> operation. Before it, a predecessor-admin-approved and committed
> `replica.handoff.prepare` leaves the replica policy unchanged, fixes the exact
> active-set-transition/cancellation intent, and rejects unrelated ordinary successors;
> recovery-authorized `fork.resolve` instead requires current-`W` fork-frontier
> reconciliation and never supplies replica/admin weight. Its deterministic
> fold state carries only authenticated governance inputs: a control-signer-
> exclusion commitment plus a fixed-size structural dependency count/root for
> every rolled-up unresolved reservation, closure, and governance-retained
> statement/control commitment. Collection adds signer-held/supplemental leaves
> without mutating exact-replayed statements and binds a separate final count/root into the
> frontier/child. Local readiness/quarantine arrival gates service but cannot
> reinterpret exposed bytes. One latest child verifies the complete
> bounded-chunk full-DAG proof and consumes all rolled-up controls atomically;
> snapshots/replay retain pending state until that exact child verifies. Only
> after a predecessor-`W` prepared-frontier bundle is formed does a derived
> prepared `replica.set` child obtain its own predecessor administrator threshold.
> Abandonment uses a separately admin-approved cancellation child derived from
> that prepare/bundle. Two single-record upserts are not a valid
> replacement. Recovery keys authorize governance-fork resolution only.

The Phase-B candidate operation/schema remains frozen and has no stable alias.

### 16.2 Publication and receipts (§10)

Read persistence receipt sequence and certificate rules with this addition:

> A retained `ReplicaId` has one non-resetting per-community receipt sequence
> across endpoint/governance/component changes. Two different strict valid
> signed receipt bodies at one sequence are equivocation evidence. Lower-after-
> higher delivery alone is not. Verified evidence quarantines the signer from
> new quorum decisions without lowering `W` and requires permanent governed
> replacement. A new `ReplicaId` starts a fresh namespace. Receipts never mix
> across governance heads, component roots, classes, or predecessor/successor
> identities. Current durability status after evidence recounts only eligible
> signers; an old certificate that falls below `W` is historical and reports
> `ReceiptEquivocated`/reconfirmation required until exact-context recertification.
> A size-bounded durable community incident barrier makes that logical downgrade
> immediate; per-certificate rows are only a bounded resumable materialization
> and stale rows cannot preserve a prior green result.

### 16.3 Replica topology/descriptors (§11)

Read replica descriptors with this addition:

> Descriptors live in the successor governed lifecycle `staged | active |
> disabled`. Staged descriptors authenticate only a bounded stable catch-up
> lane. Disabled signing identities are permanent tombstones. Full descriptor,
> status, class, `W`, and historical role-key commitment contribute to the
> authenticated policy; an invalid active/staged record fails the whole profile
> rather than being filtered. Stage, staged abandonment, join, leave, endpoint
> update, quorum-only reconfiguration, and replacement use complete
> predecessor-authorized policy transitions and are blocked by unresolved
> governance or an open unrelated handoff prepare.

### 16.4 Stream checkpoints (§13.4)

Read stream checkpoints with this addition:

> `checkpoint_generation` advances only by an exact authenticated checked
> successor per community/stream and does not reset across governance,
> retention, endpoint, or replica-component changes. Active-set changes require
> a predecessor-`W` checkpoint-frontier handoff/fence under a committed prepare.
> Each retained `ReplicaId` durably journals and signs at most one distinct
> checkpoint ID per generation. Every handoff/cancellation/abandon/conflict/
> cutover control likewise stable-commits exact bytes, generation/frontier,
> fences, and idempotency state before release.
> Two independently valid same-signer/same-slot signatures with historical
> eligibility proof are equivocation evidence. An affected slot cannot support
> current completeness until an incident-bound current-`W` conflict control—
> based on the retained stream pair or conservatively on the retained global
> signer incident—closes the affected range/allocates its exact successor and a recovery
> checkpoint commits a freshly validated root plus canonical sorted
> `supersedes` list of then-known IDs. A late retired-key artifact updates only
> the hard-bounded signer incident and does not reopen covered recovery. A
> same-signer checkpoint vote contradicting a signed frontier/fence is objective
> frontier equivocation with the same penalty. When conflicting votes occupy an
> uncertified generation, an incident-bound current-`W` control consumes that
> slot/descendants and allocates the exact recovery successor. Recovery or the
> replacement handoff installs a per-stream retired-signer cutover, so later
> artifacts under covered predecessor contexts update one bounded signer
> incident rather than reopening historical completeness. Governance excludes
> the signer; it does not choose the content root.
> A durable community incident barrier supplies the conservative current-state
> gate immediately; fixed per-stream markers materialize later under projection-
> unit/byte caps and never substitute for that `W` control or recovery
> checkpoint.

The stream-checkpoint owner must freeze the exact vote/certificate and recovery
wire before stable advertising.

### 16.5 Failure behavior (§19)

Replace/refine the replica rows semantically with:

| Failure | Required behavior |
|---|---|
| One of three replicas offline | Continue after two matching eligible receipts; report degraded redundancy. |
| Fewer than `W` eligible replicas | Queue/retain locally; do not claim durable publication or complete/recovered checkpoint; never lower `W`. |
| Replica sends invalid event or withholds | Reject/reconcile/record bounded operator evidence; do not call it checkpoint equivocation without two valid conflicting signatures. |
| Replica signs conflicting checkpoint slot | Preserve exact evidence, quarantine from new quorum decisions, permanently disable/replace through ordinary administrator governance, certify an incident-bound conflict frontier/cutover, then recover at its exact successor. |
| Replica reuses receipt sequence | Preserve exact evidence, quarantine, permanently disable/replace, and do not choose a larger counter. |
| Receipt or checkpoint-vote state rolls back or becomes uncertain | Stop same-key issuance and replace with a fresh `ReplicaId` unless exact continuous in-place recovery satisfies §9.1. |
| Predecessor `W` handoff is unavailable | Do not commit an active-signer-set change. A join/replacement candidate remains staged; a pure leave has no candidate. If a fence was exposed, predecessor service remains fenced pending the exact transition/cancellation. Never lower `W`. |
| Checkpoint-fence request lacks an installed administrator-approved prepare | Reject with no signer fence mutation; prepare-bound statements are single-flight and exact-replay idempotent. |
| Ordinary governance entry is unrelated to an open prepare | Reject it until the derived prepared `replica.set` or cancellation child commits. Permit recovery-authorized `fork.resolve`, then require §6.3.1's current-`W`, selected-admin-approved reconciliation before ordinary work. |
| Fenced prepared active-set transition is abandoned | On a non-forked lineage, resume only through the prepare's separately approved/committed cancellation child carrying current-`W` checkpoint control. After `fork.resolve`, only §6.3.1's exact-closure reconciliation may rebase or close it. |
| Fork dependency proof is incomplete or exceeds a per-leaf/chunk bound | Reject and remain fail-closed; the final child requires the exact complete proof matching `ForkResolvedFrontier`, never a structural-only or truncated substitute. |
| Evidence intake, bounded incident-barrier activation, or authoritative direct-trigger/saturation update fails | Retain the bounded candidate in the reserved slot when available, keep verified quarantine, and block restart issuance until slot/barrier/trigger repair, replay, or explicit verified recovery. |
| Derived incident materialization blocks with its authoritative state intact | Keep the signer excluded and stale rows pending, resume capped idempotent per-row-applied batches, and allow only already evaluated Q-excluding work that satisfies unchanged `W` and its per-record recovery rules. |
| Governance fork | Fail closed; recovery authority resolves the fork without changing `admin_seq`. Every accepted stable-v2 `fork.resolve` structurally requires §6.3.1 before ordinary governance, receipts, or checkpoint votes resume, even with no observed detached artifact. |

### 16.6 Dependent-spec reconciliation

Read the merged #155/#156/#157/#161 profiles together with these constraints:

- RBSR `view_equivocation`, invalidity, and withholding are session/operator
  evidence, not automatic checkpoint-equivocation proof.
- Stable catch-up includes verified RBSR/checkpoint state committed under
  `local_sync_group_v1`; neither equality alone nor a remote copy grants
  readiness/weight.
- Raw `ReplicaId`, separate endpoint identity, permanent cross-role history,
  atomic replacement, no aliases, and exact-context certificate rules remain
  unchanged.
- #161 governance checkpoints are administrator-signed and distinct from
  replica stream checkpoints. Snapshot format 1 remains byte-identical and
  cannot activate stable replicas because it lacks staged lifecycle, full
  descriptor, class, and `W`.
- #161's sparse transition proof can omit ordinary replica operations, so it is
  not the historical replica-policy witness required here.

---

## 17. Acceptance traceability

| Issue #159 acceptance | Resolution |
|---|---|
| Replica replacement documented end-to-end | §§3–6 define successor policy, old-admin stage, stable checkpoint-relative catch-up/readiness, committed prepare-bound predecessor-`W` handoff, atomic old-disable/new-activate, governed cancellation, issuance fence, state/network convergence, and tail handling. §§9–10 define failure recovery and operator execution. |
| Equivocation policy names a specific governance action | §§7–8 and ADR-0011 choose immediate evidence-driven local quarantine, a bounded durable community barrier plus resumable projection, mandatory permanent administrator-governed exclusion/replacement, and current-`W` signer cutover. Checkpoint double-vote, vote/frontier contradiction, and receipt predicates are objective; economic slashing and review-only reactivation are rejected. |
| Operator UX sketch | §§10–11 define community/per-replica status, incident/catch-up, barrier/materialization progress, immutable join/leave/replacement/staged-abandon/endpoint/quorum plan/propose/approve/handoff/commit/cancel/reconcile/status commands, paginated structural/final fork-dependency proofs, intake/receipt reconfirmation, human/JSON examples, exact shipped error categories, bounded evidence export, aggregate NDJSON events, redaction, and bounded metrics. |
| Joins/leaves/replacements preserve quorum safety | §§3.3–3.4 and 4–6 require a complete policy, intersecting majority `W`, zero-weight stage, administrator threshold before any handoff fence, single-flight predecessor-`W` frontier control, atomic activation, no hidden intermediate set, and no implicit lowering. |
| Recovery distinguishes crash from rollback/compromise | §9 permits same-ID issuance only for exact continuous receipt, checkpoint-vote, prepare/cancel/handoff/fork-control, evidence-intake, authoritative incident barrier/quarantine/high-water/index including trigger-subject saturation and keyed direct-trigger/cumulative-aggregate/source-revision high-water, and sole-writer state on a non-forked lineage with no pending closure; derived cursor/per-row applied progress is validated or conservatively rebuilt. It requires a permanently new identity for rollback uncertainty in issuance state, compromise, equivocation, or unrepairable durability breach. |
| False-positive equivocation avoided | §7 freezes same-signer checkpoint/receipt and mutually exclusive vote/frontier predicates, requires strict signatures plus historical eligibility, and enumerates delivery, invalidity, withholding, RBSR, governance-checkpoint, and non-contradictory frontier negatives. |
| Checkpoint conflict has a recovery state | §8 requires `CheckpointEquivocated`, an incident-bound current-`W` contiguous conflict control/cutover, and recovery at its exact successor. One bounded signer incident absorbs later covered old-context artifacts without reopening completeness; governance never selects content truth. |
| Phase-B/v1 compatibility explicit | §§3.1 and 14 preserve candidate schemas/vectors, require successor genesis/CommunityId, leave v1/admin_seq untouched, and make no rc.5 or implementation claim. |

Merging this pure-spec profile closes #134 §25 item 5's semantic queue item. It
does not by itself satisfy #134's public stable-wire gate: every §15 owner,
codec, vector, crash/distributed test, and independent implementation remains
required.

---

## References

- #134, *Proposal: iroh-room v2 architecture for large communities*.
- ADR-0004, *Accept the v2 Large-Community Architecture for Phase B*.
- ADR-0007, *Meyer RBSR with an iroh-room-Owned Bounded Envelope*.
- ADR-0008, *Full-State Governance Snapshots with Checkpoint-Bound Authority
  Proofs*.
- ADR-0009, *Separate Replica Signing Keys from Iroh Endpoint Keys*.
- ADR-0010, *Local Synchronized Group Commit for v2 Replica Receipts*.
- ADR-0011, *Governed Replica Replacement and Evidence-Driven Quarantine*.
- [`v2-range-reconciliation-envelope.md`](v2-range-reconciliation-envelope.md).
- [`v2-governance-snapshot-transition-proof.md`](v2-governance-snapshot-transition-proof.md).
- [`v2-replica-endpoint-identity.md`](v2-replica-endpoint-identity.md).
- [`v2-replica-durability-class.md`](v2-replica-durability-class.md).
