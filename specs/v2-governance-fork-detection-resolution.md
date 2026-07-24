# Spec: v2 Governance Fork Detection and `fork.resolve`

| | |
|---|---|
| **Issue** | #149 — `[CORE] v2 fork detection + fork.resolve (#134 §7.5)` |
| **Labels** | `type/feature` `area/protocol` `priority/p1` `risk/high` |
| **Refs** | #134 §7.5; depends on #147 and #148 |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/src/governance/log/{fork,machine}.rs`, with supporting changes in `authz.rs`, `records.rs`, `model.rs`, `operation.rs`, and `state.rs`, plus the end-to-end wire-bytes suite in `tests/v2_governance_fork_e2e.rs` and the corrected `tests/v2_governance_log_e2e.rs`. How the §18 open questions were resolved against what shipped is recorded in §19. |

---

## 1. Summary

Implement deterministic fork detection and recovery in the normative `governance::log` path of the pure v2 core crate.

The implementation must:

1. independently validate competing governance entries under the authorization rules established by #147 and #148;
2. enter an explicit `GovernanceForked` state when distinct authorization-valid entries occupy the same governance sequence on divergent branches;
3. retain every known competing branch head and the complete authenticated entry/approval evidence;
4. fail closed for all ordinary governance decisions while unresolved;
5. permit only a `fork.resolve` transition authorized by the recovery configuration at the last uncontested state;
6. require the resolution to name every locally known competing branch head and an already validated selected state;
7. accept exactly `W` or more distinct eligible recovery signatures and reject `W-1`;
8. append the resolution to the selected branch without discarding evidence from losing branches; and
9. never choose or auto-resolve a branch by lexicographic `GovernanceId` order.

The implementation remains pure and deterministic. It adds no network, storage, async, wall-clock, logging, publication-certificate, CLI, SDK, or operator-recovery UX behavior.

---

## 2. Repository context

### 2.1 Architectural boundary

`README.md` describes `crates/iroh-rooms-v2-core/` as the pure, currently unused v2 protocol core. Its canonical bytes, domains, identifiers, signatures, roots, and rejection codes are compatibility-sensitive. The implementation belongs under:

```text
crates/iroh-rooms-v2-core/src/governance/log/
```

Do not implement #149 by extending the older sibling modules under `src/governance/{model,authz,fold,fork,state_root}.rs`. Those modules use candidate `RoomId`/`GovernanceEntryId` types, legacy domains, per-author semantics, and a separate signed resolution envelope. The normative #147/#148 path uses `CommunityId`, exact-CSB-derived `GovernanceId`, the fourteen-operation registry, and `ValidatedGovernanceState`.

The core crate is constrained by `tests/banned_dependencies.rs`: no Iroh runtime, Tokio, SQLite, filesystem, or network dependency may be introduced.

### 2.2 Dependency #147

The landed governance-log foundation provides:

- canonical `GovernanceEntryBody` and `GovernanceApprovalBody` records;
- exact received CSB retention in `GovernanceEntry` and `GovernanceApproval`;
- exact-CSB-derived `VerifiedGovernanceEntry::id()`;
- deterministic operation application;
- six-component state-root computation;
- a closed operation registry containing `fork.resolve`; and
- a placeholder `ForkResolutionMarker` stored under `CommunityPolicy`.

The placeholder currently carries exactly two sorted evidence IDs, an opaque decision byte, and an advisory timestamp. It proves neither that a fork exists nor that the chosen state is valid, cannot list more than two heads, and does not preserve branch approvals.

### 2.3 Dependency #148

The landed authorization boundary provides:

```rust
validate_governance_entry(
    prev: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<(), Reject>

validate_and_apply_governance_entry(
    prev: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<ValidatedGovernanceState, Reject>
```

An ordinary candidate is valid only after checking the predecessor root/community, exact chain link, operation semantics, `W` distinct old administrators, and declared post-state root. Authorization reads the old state. The accepted-state wrapper is opaque but carries only one tip and no competing history.

#148 intentionally treats the placeholder `fork.resolve` as an ordinary administrator-authorized operation. #149 must replace that behavior at the fork-aware boundary: a resolution is recovery-authorized, not administrator-authorized.

### 2.4 Existing typed errors

`src/error.rs` already defines stable codes needed by this feature:

| Variant | Code | #149 use |
|---|---|---|
| `ForkDetected` | `fork_detected` | Observation outcome/typed notification when a valid second branch creates unresolved fork state. |
| `UnresolvedFork` | `unresolved_fork` | Any ordinary governance operation attempted while forked. |
| `InvalidForkResolution` | `invalid_fork_resolution` | Resolution payload/evidence/selection/state-machine mismatch. |
| `InsufficientAuthorization` | `insufficient_authorization` | Structurally valid resolution with fewer than `W` eligible recovery signers. |
| `MissingDependency` | `missing_dependency` | A named branch/predecessor is not yet available for validation. |
| `InvalidApproval`, `BadSignature` | existing codes | Approval binding or cryptographic failures before policy evaluation. |
| `StateRootMismatch` | `state_root_mismatch` | Selected or post-resolution state root mismatch. |

No new rejection variant is required unless implementation proves that `ForkDetected` cannot be represented cleanly as an observation outcome plus stable notification.

### 2.5 Verification convention

The repository quality gate is `scripts/verify.sh`, which runs formatting, workspace Clippy with warnings denied, all workspace tests, SDK doctests, and SDK example builds. Protocol/security changes require maintainer review.

---

## 3. Scope

### 3.1 In scope

- A pure predicate over authenticated, authorization-valid governance branch observations.
- Direct sibling detection: two distinct valid entries with the same sequence and same predecessor.
- Divergent-tip detection required by the issue acceptance text: two distinct valid entries at the same sequence with different predecessors.
- A fork-aware governance state machine with explicit linear and unresolved states.
- Fail-closed behavior for membership, role/administrator, device, recovery, replica, stream, invite, policy, migration, and other ordinary governance decisions.
- Recovery-threshold authorization of `fork.resolve`.
- Exact validation that a resolution names every locally known branch head and selects a validated state.
- Preservation of both/all competing entry signatures and approval signatures in typed audit evidence.
- Deterministic state reconstruction and continuation from the selected branch.
- Unit, property, integration, taxonomy, and compatibility tests.

### 3.2 Out of scope

- Publication-certificate refusal; no publication-certificate model exists yet and Phase C owns it.
- Operator UX, CLI prompts, recovery workflows, or diagnostics (#159).
- Network discovery, branch fetch, anti-entropy, or proving global branch-set completeness.
- SQLite, NDJSON, durable incident storage, metrics, logs, alerts, or runtime audit sinks.
- Checkpoint/snapshot integration (#150), except that this spec states the data such integration must later consume.
- Member Merkle projection integration.
- Automatic administrator ejection or punishment.
- A hash/lexical branch winner, least-ID winner, first-seen winner, timestamp winner, or arrival-order winner.
- Git or GitHub operations.

---

## 4. Normative terminology and assumptions

### 4.1 Authorization-valid entry

An **authorization-valid ordinary entry** is a `VerifiedGovernanceEntry` that independently passes all five #148 rules against its declared, opaque validated predecessor snapshot.

A cryptographically valid entry is not sufficient. An entry with bad operation semantics, insufficient administrator quorum, the wrong predecessor, or a mismatched state root does not become fork evidence.

A **recovery-valid resolution entry** passes the structural, branch-set, selected-state, recovery-threshold, and post-state checks in §9. It does not use the ordinary administrator threshold.

### 4.2 Branch observation

A branch observation is opaque proof produced only by successful validation:

```rust
pub struct ValidatedGovernanceCandidate {
    entry: VerifiedGovernanceEntry,
    predecessor_tip: GovernanceTip,
    predecessor_root: StateRoot,
    resulting_state: GovernanceState,
    resulting_root: StateRoot,
    evidence: AuthenticatedGovernanceEvidence,
}
```

Fields remain private. Safe construction must validate the entry against a `ValidatedGovernanceState`; tests needing malformed values use crate-private test helpers only.

### 4.3 Fork

For two validated observations `A` and `B`, the pair is a fork when all are true:

```text
A.community_id == B.community_id
A.entry_id     != B.entry_id
A.seq          == B.seq
A and B are each authorization-valid against their declared predecessors
```

The predicate does not require the same author. Governance authorization is quorum-based; limiting detection to one signer would miss two incompatible entries authored by different principals but approved by valid quorums.

The direct #134 §7.5 case is the subset:

```text
A.prev == B.prev
```

The issue acceptance text also explicitly requires the divergent-predecessor case:

```text
A.prev != B.prev
```

Therefore the implementation uses the broader same-community, same-sequence, distinct-valid-ID predicate. When full lineages are available, the state machine records the last common ancestor as the stable recovery authority and the currently known descendant heads as branches.

Two branch heads at different sequences are also divergent if lineage comparison finds distinct valid entries at their first shared sequence. Detection compares lineages, not merely the latest head numbers, when merging prevalidated histories.

### 4.4 GovernanceForked

`GovernanceForked` means the core has authenticated enough evidence to prove that more than one authorization-valid state exists for one governance position or lineage. It is a protocol state, not merely an error string.

### 4.5 Known branch head

A **known branch head** is the latest independently validated entry retained on each locally observed divergent lineage. “Every known competing branch head” means exact equality with the state machine's canonical local head set at resolution-validation time. The core cannot prove that no withheld branch exists globally.

### 4.6 Selected state

The selected state is an existing, independently validated branch-head snapshot named by:

- `selected_head: GovernanceId`; and
- `selected_state_root: StateRoot`.

A resolution cannot inject an arbitrary state/root. The selected head must be one of the named heads, and the selected root must equal the state already validated at that head.

### 4.7 Recovery authority snapshot

Recovery authorization uses the recovery configuration from the last common, uncontested ancestor of all named branches. It never uses a `recovery.set` result from one contested branch. This prevents a branch from installing its own recovery keys and then authorizing its selection.

If no common ancestor or its state is unavailable, resolution returns `MissingDependency`; it must not guess an authority set.

### 4.8 Local completeness limitation

A resolution attests to the branch set known to the validating node and named in the signed payload. It does not establish global branch completeness. A later-arriving valid branch not covered by an accepted resolution re-enters `GovernanceForked` and requires a new recovery-authorized resolution.

---

## 5. Complete behavioral inventory

### 5.1 Detection behavior

1. Compare authenticated exact-CSB IDs through `VerifiedGovernanceEntry::id()`; never recompute IDs from typed bodies.
2. Independently validate both candidates, including authorization and post-state roots, before detecting a fork.
3. Detect distinct valid entries at the same sequence when they share a predecessor.
4. Detect distinct valid entries at the same sequence when they name different predecessors, provided both predecessor lineages are validated and belong to the same community.
5. When merging heads at unequal sequences, walk retained lineage to find the first differing sequence; represent current descendant heads in evidence.
6. Treat an identical ID replay as the same entry, not a fork.
7. Treat different approval attachment sets for the same entry ID as one entry. Merge non-conflicting verified approvals by authenticated approval identity for audit, without double-counting signers.
8. Do not create fork state if only one candidate is authorization-valid.
9. Do not compare or merge different communities.
10. Detect a fork even if competing operations produce equal state roots; distinct authorized decisions remain distinct audit events.
11. Detection is input-order independent.
12. Sorting IDs is permitted only for canonical evidence encoding and set comparison. Sorted position has no authorization meaning.
13. Detection does not mutate a validated predecessor or branch state.
14. A third or later valid competing branch extends the canonical branch set and audit evidence rather than replacing prior evidence.

### 5.2 State-machine behavior

1. The initial state is `Linear(validated_genesis_state)`.
2. A valid ordinary extension of the sole accepted head remains `Linear`.
3. Observation of a second valid divergent candidate transitions atomically to `GovernanceForked` and returns/surfaces `ForkDetected` with typed evidence.
4. `GovernanceForked` retains the last common ancestor, all validated branch snapshots, current known heads, all entry/approval evidence, and prior resolution evidence relevant to late forks.
5. A failed transition leaves the entire previous machine state byte-for-byte/equality unchanged.
6. While forked, no ordinary operation can advance, partially apply, alter a candidate root, or change the accepted tip.
7. Only a valid `fork.resolve` can leave `GovernanceForked`.
8. A successful resolution creates one new linear accepted snapshot whose predecessor is the selected head and whose state is the selected branch state plus the append-only resolution marker.
9. Losing branch states and approvals remain in audit evidence after resolution.
10. A duplicate or stale resolution cannot resolve the same fork twice.
11. A later valid branch omitted because it was previously unknown re-enters forked state; no old evidence is deleted.
12. State-machine decisions depend only on authenticated input records and retained validated state, never arrival time, wall clock, randomness, or map iteration order.

### 5.3 Fail-closed behavior

While `GovernanceForked`, every operation other than `fork.resolve` returns `Reject::UnresolvedFork` before ordinary administrator quorum or operation application is consulted.

| Operation | While forked |
|---|---|
| `member.grant` | Reject `UnresolvedFork` |
| `member.revoke` | Reject `UnresolvedFork` |
| `device.grant` | Reject `UnresolvedFork` |
| `device.revoke` | Reject `UnresolvedFork` |
| `admin.set` | Reject `UnresolvedFork` |
| `recovery.set` | Reject `UnresolvedFork` |
| `replica.set` | Reject `UnresolvedFork` |
| `stream.create` | Reject `UnresolvedFork` |
| `stream.policy_set` | Reject `UnresolvedFork` |
| `stream.archive` | Reject `UnresolvedFork` |
| `invite.revoke` | Reject `UnresolvedFork` |
| `policy.set` | Reject `UnresolvedFork` |
| `migration.accept` | Reject `UnresolvedFork` |
| `fork.resolve` | Run the dedicated validation path in §9 |

This blocks all membership, role, device, replica, and policy decisions required by #134 §7.5. It intentionally blocks more than the acceptance example rather than trying to infer which contested branch components happen to overlap.

The low-level `state::apply` function remains a pure transition helper and cannot itself know whether the caller is forked. Its documentation must continue to state that it is not a state-machine or authorization boundary. Normative receiver examples must route through the fork-aware API.

### 5.4 Recovery-threshold behavior

Let `R` be the sorted unique recovery-key set and `W` its threshold from the last common ancestor.

1. Require `R` nonempty.
2. Require `1 <= W <= len(R)`.
3. Build a set from the verified resolution entry signer plus every verified attached approval signer.
4. Intersect that set with `R`.
5. Count each eligible principal once.
6. The entry signer counts if and only if it is in `R`.
7. A signer who also supplies an approval counts once.
8. Outsider signatures remain cryptographically checked but contribute zero.
9. Administrator status alone contributes zero; a principal counts only if also in `R`.
10. Recovery keys installed only on a contested branch contribute zero.
11. `W-1` eligible signatures return `InsufficientAuthorization` and leave the fork unresolved.
12. Exactly `W` and any valid superset succeed, subject to all other checks.
13. Duplicate approvers remain rejected during verified-entry construction as `InvalidApproval`.
14. A malformed/disabled recovery configuration fails closed as `InsufficientAuthorization`; there is no administrator fallback.

The existing governance approval record is sufficient for recovery signatures because each approval cryptographically binds the community, exact resolution entry ID, and declared post-resolution state root. No second signature envelope or new signing domain is introduced unless #134 explicitly requires one.

### 5.5 Resolution behavior

A resolution must:

1. be a verified `GovernanceEntry` whose operation kind and payload are `fork.resolve`;
2. target the forked community;
3. carry a canonical sorted, duplicate-free list of at least two branch-head IDs;
4. name exactly every current known head, no fewer and no unrelated extras;
5. name one listed head as `selected_head`;
6. name the state root already validated for `selected_head` as `selected_state_root`;
7. use `seq == selected_head.seq + 1` with checked arithmetic;
8. use `prev == Some(selected_head)`;
9. satisfy the recovery threshold from the last common ancestor;
10. apply a deterministic resolution marker to the selected branch state;
11. declare the exact root after that marker is applied;
12. preserve all branch and resolution signatures in audit evidence; and
13. atomically return a linear state with the resolution entry as its tip.

A resolution does not “reject both” and does not accept an externally supplied arbitrary state. The selected state must already be represented by one validated branch head. A future protocol version may add a superseding-state record, but that requires separate state-validation and compatibility rules.

### 5.6 No lexical tie-break

The implementation must contain no comparison equivalent to choosing `min(entry_id)`, `max(entry_id)`, first sorted ID, first arrival, or timestamp as winner.

Canonical sorting is mandatory for deterministic evidence serialization, branch-set equality, and tests. Selection is always the explicit `selected_head` signed by at least `W` recovery principals. A test must deliberately arrange for the selected head to be lexically greater in one case and lexically smaller in another; neither fork may resolve before a valid recovery entry arrives.

### 5.7 Audit preservation

For every branch, preserve:

- exact-CSB-derived entry ID;
- exact entry-body CSB;
- entry signer and signature;
- sequence and predecessor;
- declared/resulting state root;
- exact approval CSB for every verified approval;
- approval signer and signature; and
- the validation authority snapshot identity/root used to establish quorum validity.

For the resolution, preserve the same record material plus:

- all named branch heads;
- selected head and selected state root;
- recovery authority root/config identity;
- distinct eligible recovery signer set; and
- post-resolution root/tip.

Evidence is append-only in the fork-aware wrapper. Resolution changes status but does not erase branch records or losing approvals. `policy.set` must not clear resolution markers, matching the existing append-only marker behavior.

The pure core exposes an audit-ready typed record and performs no persistence. A later runtime/store issue may serialize it. Callers must not reconstruct evidence from display strings or logs.

---

## 6. Proposed public data model

Names may be adjusted to repository style, but the trust boundaries and represented facts are normative.

### 6.1 Authenticated evidence

Extend the private data retained by `VerifiedGovernanceEntry` or add a private nested value so fork evidence can preserve signatures without trusting caller reconstruction:

```rust
pub struct VerifiedGovernanceApprovalEvidence {
    body: GovernanceApprovalBody,
    csb: Vec<u8>,
    signature: Signature,
}

pub struct AuthenticatedGovernanceEvidence {
    id: GovernanceId,
    body: GovernanceEntryBody,
    csb: Vec<u8>,
    signer: PrincipalId,
    signature: Signature,
    approvals: Vec<VerifiedGovernanceApprovalEvidence>,
}
```

Construction occurs only in `verify_governance_entry` after exact-CSB signature verification, canonical round-trip verification, approval verification, binding checks, sorting, and duplicate rejection. Expose immutable accessors. Do not add these detached signatures to entry-body CSB or change `GovernanceId` derivation.

### 6.2 Validated candidate

Refactor the private #148 `validate_candidate` helper to return an opaque reusable proof:

```rust
pub struct ValidatedGovernanceCandidate {
    predecessor: ValidatedGovernanceState,
    resulting: ValidatedGovernanceState,
    evidence: AuthenticatedGovernanceEvidence,
}

pub fn validate_governance_candidate(
    prev: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<ValidatedGovernanceCandidate, Reject>;
```

`validate_governance_entry` maps the candidate to `()`. The existing linear commit function may map it to `resulting`, preserving source compatibility. Fork-aware code consumes the opaque candidate rather than re-running partial checks.

For ordinary entries, this helper uses the #148 administrator threshold. `fork.resolve` must be rejected or delegated to the dedicated recovery validator when the machine is forked; it must not silently pass ordinary authorization.

### 6.3 Branch evidence

```rust
pub struct GovernanceBranchEvidence {
    pub head: GovernanceId,
    pub seq: u64,
    pub predecessor: Option<GovernanceId>,
    pub state_root: StateRoot,
    pub entry: AuthenticatedGovernanceEvidence,
}

pub struct GovernanceForkEvidence {
    pub community_id: CommunityId,
    pub stable_tip: GovernanceTip,
    pub stable_state_root: StateRoot,
    pub branches: Vec<GovernanceBranchEvidence>,
}
```

`branches` is sorted by raw head ID only for canonical representation. It contains at least two unique branches. If descendant histories are merged, preserve sufficient lineage evidence to prove their first divergence and retain each latest known head.

### 6.4 Fork state

```rust
pub enum GovernanceMachineState {
    Linear(LinearGovernanceState),
    GovernanceForked(GovernanceForkedState),
}

pub struct LinearGovernanceState {
    accepted: ValidatedGovernanceState,
    lineage: GovernanceLineage,
    audit: Vec<GovernanceForkAuditRecord>,
}

pub struct GovernanceForkedState {
    stable: ValidatedGovernanceState,
    branches: Vec<ValidatedGovernanceBranch>,
    evidence: GovernanceForkEvidence,
    prior_audit: Vec<GovernanceForkAuditRecord>,
}
```

All fields affecting trust remain private. Expose read-only status, accepted/stable snapshot, known heads, and evidence accessors.

The lineage store is pure in-memory protocol state, not a persistence mechanism. It must retain enough validated ancestry to identify a common ancestor when merging prevalidated branches. A runtime may rebuild it from authenticated records.

### 6.5 Observation result

Because fork detection must both retain evidence and notify the caller, do not model the transition only as `Err(ForkDetected)` and discard the new state:

```rust
pub enum GovernanceObservation {
    Advanced(GovernanceMachineState),
    ForkDetected {
        state: GovernanceMachineState,
        evidence: GovernanceForkEvidence,
    },
    Duplicate(GovernanceMachineState),
}
```

An equivalent `(new_state, event)` result is acceptable. The stable `fork_detected` code must remain available to downstream callers, but evidence retention takes precedence over an API shape that loses the transition on `Err`.

### 6.6 Resolution payload and committed marker

Replace the normative placeholder payload with a typed shape capable of satisfying the issue:

```rust
pub struct ForkResolve {
    pub branch_heads: Vec<GovernanceId>,
    pub selected_head: GovernanceId,
    pub selected_state_root: StateRoot,
    pub created_at_ms: u64,
}
```

Canonical rules:

- `branch_heads.len() >= 2`;
- sort ascending by raw ID and reject a received encoding whose semantic round trip is not byte-identical;
- no duplicates;
- `selected_head` occurs exactly once in `branch_heads`;
- `created_at_ms` is signed advisory data and is never checked against a local clock.

The state-root-visible marker should commit to the same facts:

```rust
pub struct ResolvedForkMarker {
    pub branch_heads: Vec<GovernanceId>,
    pub selected_head: GovernanceId,
    pub selected_state_root: StateRoot,
    pub resolution_entry: GovernanceId,
    pub created_at_ms: u64,
}
```

If including `resolution_entry` in the marker creates a self-reference through the entry's declared post-state root, omit that field from the marker and retain it only in the outer audit record. Do not solve self-reference by weakening ID or root verification.

The old `[GovernanceId; 2] + decision: u8` marker is insufficient and should not remain the normative `governance::log` resolution API. The older candidate `governance::fork` record and its frozen vector remain untouched unless a separate migration explicitly replaces that path.

---

## 7. Pure fork predicate

Expose a side-effect-free predicate over opaque validated candidates:

```rust
pub fn detect_governance_fork(
    left: &ValidatedGovernanceCandidate,
    right: &ValidatedGovernanceCandidate,
) -> Option<GovernanceForkEvidence>;
```

Normative algorithm:

```text
1. If communities differ, return None.
2. Read exact authenticated IDs from candidate evidence.
3. If IDs are equal, return None (duplicate observation).
4. If sequences differ, compare retained lineages:
   a. find the last common validated ancestor;
   b. find the first sequence at which valid IDs differ;
   c. if no divergence exists, return None (one lineage extends the other);
   d. otherwise emit evidence with the latest known head of each lineage.
5. If sequences are equal and IDs differ, the candidates conflict.
6. Resolve the last common ancestor from validated lineage metadata.
7. Canonically sort branch records for representation only.
8. Return evidence containing both complete authenticated records and approvals.
```

A convenience set form should detect and coalesce more than two branches:

```rust
pub fn detect_governance_forks<'a>(
    candidates: impl IntoIterator<Item = &'a ValidatedGovernanceCandidate>,
) -> Result<Option<GovernanceForkEvidence>, Reject>;
```

The set form groups by community/lineage, deduplicates identical IDs, computes maximal heads, and returns `MissingDependency` if it cannot prove ancestry required by the supplied set. It must produce byte-identical canonical evidence for every permutation of the same inputs.

---

## 8. Fork-aware state machine

### 8.1 Linear observation

For an ordinary entry received in `Linear` state:

1. Verify its exact received record and approvals.
2. Locate its declared predecessor in retained validated lineage.
3. If the predecessor is unavailable, return `MissingDependency` without mutation.
4. Validate it independently against that predecessor using #148.
5. If it exactly extends the sole accepted head, advance linearly.
6. If it duplicates a retained entry ID, return `Duplicate` and optionally merge additional verified approval evidence.
7. If it creates or reveals a divergent authorized lineage, construct canonical evidence and atomically enter `GovernanceForked`.
8. Do not auto-commit one competing state before evaluating known same-position candidates merely because it arrived first.

### 8.2 Forked observation

For an entry received in `GovernanceForked`:

- If it is an ordinary governance operation submitted as a new decision, return `UnresolvedFork` before application.
- If it is authenticated historical evidence needed to complete an already observed branch, a separate evidence-ingestion API may validate and append it without authorizing a new decision. This API must not change the selected/accepted state.
- If it is `fork.resolve`, execute §9.

Separating “observe authenticated historical branch evidence” from “authorize a new governance decision” avoids making recovery impossible while still enforcing fail-closed behavior.

### 8.3 Successful resolution

On successful validation:

1. Clone the selected branch's validated `GovernanceState`.
2. Insert the canonical append-only `ResolvedForkMarker` into community policy.
3. Verify the resulting root equals the resolution entry's declared `state_root`.
4. Create a new `ValidatedGovernanceState` with the resolution entry's exact-CSB ID as tip and `seq == selected_seq + 1`.
5. Construct `GovernanceForkAuditRecord` containing every losing and winning branch record/approval plus resolution signatures.
6. Return `Linear` with the new snapshot, lineage linking the resolution to `selected_head`, and prior plus new audit records.
7. Retain rejected branch lineage in audit evidence but do not treat it as accepted authorization state.

---

## 9. `fork.resolve` validation

Validation order is normative so failures are deterministic and tests can isolate each rule.

### Rule 1 — Machine state and record crypto

- Require current status `GovernanceForked`; otherwise return `InvalidForkResolution` for a stale/unsolicited resolution.
- Require a cryptographically verified entry and verified attached approvals. Preserve existing `BadSignature`, `NonCanonicalEncoding`, and `InvalidApproval` failures from the record layer.
- Require matching community and operation kind/payload; mismatches return `InvalidForkResolution` at this boundary.

### Rule 2 — Canonical complete branch set

- Require at least two sorted unique `branch_heads`.
- Require exact set equality with all current `GovernanceForkedState` heads.
- A locally unknown listed ID returns `MissingDependency` until its entry/lineage is obtained and validated.
- A known ID that is not a head of this fork, an omitted known head, a duplicate, or a stale former head returns `InvalidForkResolution`.

### Rule 3 — Selected state binding

- Require `selected_head` in the exact branch set.
- Load the already validated snapshot for that head.
- Require `selected_state_root` equal its committed root.
- Do not deserialize or trust a caller-supplied full `GovernanceState` as the selected state.
- Root mismatch returns `StateRootMismatch`.

### Rule 4 — Resolution chain link

- Require `seq == selected_head.seq.checked_add(1)`.
- Require `prev == Some(selected_head)`.
- Overflow or wrong link returns `InvalidForkResolution`.
- This is the only authorized continuation while forked.

### Rule 5 — Recovery authority

- Resolve the last common ancestor covering every named head.
- Verify that ancestor snapshot/root is retained and valid.
- Validate its recovery configuration invariants.
- Count the distinct union of the verified entry signer and verified approval signers intersected with that ancestor's recovery keys.
- Fewer than `W` returns `InsufficientAuthorization`.
- Ordinary administrator quorum is neither required nor sufficient unless those same principals are recovery keys.

### Rule 6 — Deterministic post-resolution root

- Apply only the resolution marker to the selected validated state.
- Require marker facts to match the payload exactly.
- Compute the six-component root and require equality with the signed declared `state_root`.
- Mismatch returns `StateRootMismatch`.

### Rule 7 — Atomic commit and audit

- Build the complete audit record before commit.
- Commit the new linear snapshot, marker, lineage, and audit record atomically in the returned value.
- Any failure leaves the prior `GovernanceForkedState` unchanged.

---

## 10. Error precedence

| Situation | Result |
|---|---|
| Malformed/noncanonical entry or approval bytes | Existing record-layer error |
| Bad entry/approval signature | `BadSignature` |
| Approval not bound to exact entry ID/community/root or duplicate approver | `InvalidApproval` |
| Ordinary operation while forked, including `member.grant` | `UnresolvedFork` |
| Valid second branch causes transition | `ForkDetected` notification plus retained `GovernanceForked` state |
| Resolution attempted while not forked | `InvalidForkResolution` |
| Resolution references data not locally available | `MissingDependency` |
| Resolution omits a known head, includes unrelated known evidence, is stale, duplicates heads, or selects a non-head | `InvalidForkResolution` |
| Selected stored state root does not match payload | `StateRootMismatch` |
| Recovery config malformed/disabled or eligible signers fewer than `W` | `InsufficientAuthorization` |
| Resolution post-apply root mismatch | `StateRootMismatch` |

Fork-state gating precedes ordinary operation semantics and ordinary administrator quorum. Therefore a malformed `member.grant` submitted while forked still returns `UnresolvedFork`; the state machine does not leak or act on branch-dependent validation.

Record-level crypto/canonical decoding occurs before state-machine gating because unauthenticated bytes must never enter evidence.

---

## 11. Audit evidence model

### 11.1 Record shape

```rust
pub struct GovernanceForkAuditRecord {
    pub community_id: CommunityId,
    pub stable_tip: GovernanceTip,
    pub stable_state_root: StateRoot,
    pub branches: Vec<GovernanceBranchEvidence>,
    pub status: GovernanceForkAuditStatus,
}

pub enum GovernanceForkAuditStatus {
    Unresolved,
    Resolved {
        resolution: AuthenticatedGovernanceEvidence,
        selected_head: GovernanceId,
        selected_state_root: StateRoot,
        eligible_recovery_signers: Vec<PrincipalId>,
        resulting_tip: GovernanceId,
        resulting_state_root: StateRoot,
    },
}
```

The implementation may use separate unresolved/resolved structs to make illegal states unrepresentable. In either design, branch evidence is immutable and shared or cloned into the resolved record.

### 11.2 Evidence integrity

- Every branch entry signature and approval signature is preserved exactly as received and verified.
- Evidence uses exact entry/approval CSB, never a typed re-encoding.
- Branch order and approval order are canonical for deterministic output.
- Approval deduplication uses authenticated approval identity; one principal cannot count twice.
- Both competing approvals remain accessible after selection and after later `policy.set` operations.
- The audit record may be large relative to a marker, so full evidence remains outside the six-component governance root; the marker commits branch IDs and selected state. Durable callers must persist the typed record alongside protocol records.
- Do not log key material beyond public principals and signatures already present in governance records. Never include secret keys.

### 11.3 Late evidence

If new valid branch evidence arrives after resolution:

1. preserve the prior resolved audit record;
2. prove that the new branch diverges at a position not covered by that resolution;
3. enter a new unresolved audit incident whose branch set includes the currently accepted descendant head and the late branch head; and
4. require a new exact-head-set recovery resolution.

No prior resolution or approval is deleted or rewritten.

---

## 12. Implementation plan

### Step 1 — Preserve authenticated evidence in verified records

Files:

- `crates/iroh-rooms-v2-core/src/governance/log/records.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/mod.rs`

Actions:

1. Add private authenticated entry/approval evidence types or equivalent fields to `VerifiedGovernanceEntry`.
2. Clone exact retained CSB and detached signatures only after successful verification.
3. Keep existing `id()`, `body()`, `signer()`, and `approvals()` behavior source-compatible.
4. Add immutable evidence accessors needed by fork code.
5. Test exact byte/signature retention, canonical approval order, duplicate rejection, and no typed-body ID re-derivation.

No entry body, approval body, signature message, ID derivation, or domain changes in this step.

### Step 2 — Expose opaque validated candidates

Files:

- `crates/iroh-rooms-v2-core/src/governance/log/authz.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/mod.rs`

Actions:

1. Refactor the existing private `validate_candidate` result into opaque `ValidatedGovernanceCandidate`.
2. Retain predecessor tip/root, resulting snapshot/root, and authenticated evidence.
3. Keep the five-rule order and old-administrator semantics unchanged for ordinary operations.
4. Reimplement existing validation/commit wrappers through the shared helper to prevent disagreement.
5. Prevent the ordinary helper from being the forked-state authorization path for `fork.resolve`.
6. Add regression tests proving no mutation on failure and preserving #148 behavior.

### Step 3 — Add normative fork module

Files:

- new `crates/iroh-rooms-v2-core/src/governance/log/fork.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/mod.rs`

Actions:

1. Define branch/fork evidence types with private trusted construction.
2. Implement pair and set detection predicates.
3. Use exact-CSB IDs and validated lineage metadata.
4. Canonically sort evidence while keeping selection absent.
5. Coalesce third and later branches.
6. Add direct-sibling, divergent-predecessor, unequal-head-lineage, duplicate, invalid-candidate, equal-root, and permutation tests.

Do not call the older `governance::fork::detect` implementation.

### Step 4 — Add fork-aware state machine

Files:

- new `crates/iroh-rooms-v2-core/src/governance/log/machine.rs`, or `fork.rs` if maintainers prefer one cohesive module
- `crates/iroh-rooms-v2-core/src/governance/log/mod.rs`

Actions:

1. Add opaque `GovernanceMachineState::{Linear, GovernanceForked}`.
2. Retain validated lineage sufficient for common-ancestor lookup.
3. Implement linear observation, duplicate handling, fork transition, and evidence-only ingestion.
4. Gate all fourteen operation kinds as specified.
5. Return `UnresolvedFork` for ordinary operations while unresolved.
6. Return fork state and typed evidence together on detection.
7. Preserve prior audit incidents across transitions.
8. Document existing `validate_and_apply_governance_entry` and `state::apply` as lower-level APIs that cannot enforce fork state alone.

### Step 5 — Replace the normative placeholder resolution payload

Files:

- `crates/iroh-rooms-v2-core/src/governance/log/model.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/operation.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/state.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/mod.rs`

Actions:

1. Replace `ForkResolutionMarker { evidence: [..; 2], decision: u8, .. }` in the normative log registry with typed `branch_heads`, `selected_head`, and `selected_state_root` fields.
2. Enforce closed-schema canonical decode and semantic round-trip rules.
3. Separate the signed operation payload from the state-root-visible resolved marker if needed to avoid self-reference.
4. Make marker insertion append-only, deterministic, and duplicate-safe.
5. Ensure `policy.set` preserves markers.
6. Do not alter the older candidate fork envelope or its frozen fixture in this issue.
7. Review all normative state-root tests affected by the new marker representation; any changed pinned vector requires the compatibility process in §15.

### Step 6 — Implement recovery authorization

Files:

- `crates/iroh-rooms-v2-core/src/governance/log/fork.rs` or `machine.rs`
- optionally a small shared threshold helper in `authz.rs`

Actions:

1. Add recovery-config invariant validation.
2. Resolve recovery authority from the last common ancestor.
3. Count distinct signer/approval principals through a `BTreeSet` intersection with recovery keys.
4. Do not count administrator-only or branch-installed keys.
5. Return `InsufficientAuthorization` for malformed config or `W-1`.
6. Test `W-1`, `W`, supersets, duplicates, overlap, outsiders, disabled config, and branch-modified recovery sets.

If threshold set logic is shared with #148, parameterize it by an immutable authority set rather than duplicating the older candidate quorum implementation, which has different semantics.

### Step 7 — Implement resolution validation and commit

Files:

- `crates/iroh-rooms-v2-core/src/governance/log/fork.rs` or `machine.rs`
- `crates/iroh-rooms-v2-core/src/governance/log/state.rs`

Actions:

1. Implement the seven rules in §9 in fixed order.
2. Require exact branch-set equality.
3. Bind selected head to its stored validated state/root.
4. Validate resolution `prev`/`seq` against selected head.
5. Apply marker to selected state only.
6. Verify declared post-state root.
7. Construct complete audit evidence before returning the next state.
8. Prove failed resolutions leave fork state unchanged.
9. Prove successful resolution allows a subsequent ordinary entry under the selected state's resulting administrator set.

### Step 8 — Integrate end-to-end wire tests

Files:

- new `crates/iroh-rooms-v2-core/tests/v2_governance_fork_e2e.rs`, or a clearly separated section in `v2_governance_log_e2e.rs`

Drive the public trust boundary:

```text
raw exact CSB + signatures
→ GovernanceEntry::from_received_csb
→ verify_governance_entry
→ validate candidate against retained predecessor
→ detect fork / enter GovernanceForked
→ reject member.grant with UnresolvedFork
→ receive recovery-signed fork.resolve
→ reject W-1, accept W
→ commit selected state and audit record
→ accept next ordinary governance entry
```

Use deterministic public test seeds and real Ed25519 signatures. Do not construct private verified wrappers directly from integration tests.

### Step 9 — Taxonomy and compatibility coverage

Files:

- `crates/iroh-rooms-v2-core/tests/taxonomy.rs`
- `crates/iroh-rooms-v2-core/tests/signed_records_golden.rs`
- `crates/iroh-rooms-v2-core/tests/golden/` if a normative vector is added

Actions:

1. Make `fork_detected`, `unresolved_fork`, and `invalid_fork_resolution` reachable through normative public paths.
2. Add one isolated negative test/vector per newly reachable code according to existing fixture policy.
3. Add a normative `fork.resolve` round-trip/signature/root vector without modifying the unrelated older candidate vector unless required.
4. Treat any existing frozen-byte drift as a schema-versioned protocol change, never an incidental fixture update.

### Step 10 — Documentation and verification

Files:

- module docs in `governance/log/mod.rs`, `authz.rs`, `state.rs`, and new modules
- relevant normative spec status notes after implementation

Actions:

1. Remove stale statements that fork handling remains entirely deferred.
2. Document the pure-core/runtime boundary and local-completeness limitation.
3. Document that canonical ID sorting is not winner selection.
4. Run focused crate tests during development.
5. Run `scripts/verify.sh` before completion.
6. Require protocol/security maintainer review due to `risk/high` and `area/protocol`.

---

## 13. Test strategy

### 13.1 Detection unit tests

- Two distinct quorum-valid siblings at the same sequence and same predecessor enter `GovernanceForked`.
- Two distinct quorum-valid entries at the same sequence and different validated predecessors enter `GovernanceForked`.
- Two unequal-height heads whose histories first differ at the same sequence enter `GovernanceForked` and retain latest heads.
- Same exact entry ID replay is `Duplicate`, not forked.
- Same entry ID with additional valid approvals merges audit evidence without creating a fork.
- One valid and one under-threshold/invalid candidate do not form fork evidence.
- Different communities do not form a fork.
- Distinct valid entries with equal resulting roots still fork.
- Input reversal and all set permutations produce equal canonical evidence.
- A third branch expands evidence; no earlier branch disappears.
- Evidence IDs equal `VerifiedGovernanceEntry::id()` from retained CSB.

### 13.2 Fail-closed tests

- While forked, a fully valid `member.grant` returns `UnresolvedFork`.
- While forked, a malformed `member.grant` also returns `UnresolvedFork` after record crypto but before operation application.
- Table-test every non-resolution operation kind for `UnresolvedFork`.
- No rejected operation mutates stable state, branch state, heads, roots, or audit evidence.
- A `fork.resolve` attempt reaches its dedicated validator rather than being blocked by the ordinary gate.

### 13.3 Recovery threshold tests

For recovery threshold `W`:

- zero signatures rejects;
- `W-1` distinct eligible signatures reject with `InsufficientAuthorization`;
- exactly `W` succeeds;
- more than `W` succeeds;
- signer plus own approval counts once;
- duplicate approvers are rejected before counting;
- outsider signatures contribute zero;
- administrators not in recovery set contribute zero;
- recovery principals need not be administrators;
- branch-proposed recovery keys cannot authorize resolution;
- common-ancestor recovery keys can authorize even if removed on one branch;
- empty, zero-threshold, over-threshold, duplicate, or noncanonical recovery configuration fails closed;
- approval input order does not affect the result.

### 13.4 Resolution validation tests

- Fewer than two heads rejects.
- Unsorted/duplicate heads reject through canonical validation.
- Omitted known head rejects `InvalidForkResolution`.
- Extra unrelated known head rejects `InvalidForkResolution`.
- Unknown head returns `MissingDependency`.
- Selected head not listed rejects.
- Selected root mismatch returns `StateRootMismatch`.
- Wrong `prev`, wrong `seq`, and sequence overflow reject.
- Resolution while linear rejects `InvalidForkResolution`.
- Duplicate/stale resolution rejects.
- Declared post-marker root mismatch returns `StateRootMismatch`.
- Failed resolution leaves fork state unchanged.
- Successful resolution tip is the exact-CSB ID of the resolution entry.
- Successful resolution applies selected state plus marker, not losing branch effects.
- A later ordinary operation validates against the resolved selected state.

### 13.5 No-tie-break tests

Construct branch IDs so byte order is intentional:

1. Observe lower-ID and higher-ID valid branches; assert state remains `GovernanceForked` with neither selected.
2. Reverse arrival order; assert identical unresolved evidence.
3. Recovery-select the higher ID and assert success.
4. In a separate fixture recovery-select the lower ID and assert success.
5. Search implementation/tests for any helper named or behaving as `winner = min/max/sorted[0]`; canonical sorting must only feed representation/set comparison.

### 13.6 Audit evidence tests

- Both competing entries' exact CSB, signatures, and all verified approvals are present before resolution.
- Both remain present after selecting either branch.
- Losing approvals remain cryptographically verifiable against their original exact CSB.
- Resolution signer/approval signatures and eligible recovery signer set are retained.
- A policy replacement cannot erase resolved markers or outer audit evidence.
- Canonical evidence encoding is stable across branch/approval arrival order.
- A third branch retains all three approval sets.
- Late branch evidence preserves the earlier resolved incident and creates a new unresolved incident.

### 13.7 Property tests

Generate bounded administrator/recovery sets, thresholds, branch operations, and arrival permutations to prove:

- fork predicate symmetry;
- evidence set permutation independence;
- no duplicate principal contributes more than one threshold unit;
- `count < W` always rejects and `count >= W` succeeds when all non-auth rules are valid;
- selected state is always one of the retained validated branch states;
- no failed transition mutates input state;
- resolution followed by rebuild from the same authenticated record/evidence set yields the same tip and state root;
- IDs influence canonical order but never selected state.

Bound generators to protocol record limits so tests do not normalize unbounded-memory behavior.

### 13.8 Integration and regression tests

- Preserve all #147 and #148 tests.
- Exercise forks involving `member.grant`, `admin.set`, and `recovery.set` to prove state-component-sensitive cases.
- Exercise a fork where branch authors differ but both have sufficient old-admin approval quorums.
- Exercise exact received CSB through the external integration-test crate.
- Exercise restart/rebuild conceptually by serializing authenticated source records, reconstructing verification/candidates, and comparing machine status/evidence/root.

---

## 14. Acceptance criteria traceability

| Issue acceptance | Required evidence |
|---|---|
| Two quorum-valid entries at the same sequence from different predecessors trigger `GovernanceForked` | Detection + state-machine integration test with two independently validated predecessor lineages and distinct exact-CSB IDs. The direct same-predecessor sibling case is tested separately. |
| While forked, `member.grant` is rejected with a typed reason | Public state-machine test expects `Reject::UnresolvedFork` and proves no mutation. |
| `fork.resolve` with `W` recovery signatures resolves; `W-1` does not | Real-signature wire tests expect `InsufficientAuthorization` for `W-1`, successful linear state for `W`, and unchanged fork state on failure. |
| Lexical event-ID tie-break is not used | Arrival-order tests remain unresolved; explicit recovery can select either lower or higher ID. |
| Both competing approvals are preserved in audit evidence | Before/after resolution tests compare exact approval CSB/signature bytes for both branches and reverify them. |

Additional completion criteria:

- A resolution must name exactly all locally known heads and a validated selected state/root.
- Ordinary admin quorum cannot substitute for recovery quorum.
- Resolution uses the last uncontested recovery configuration.
- No publication-certificate code is introduced.
- Full repository verification passes.

---

## 15. Security, reliability, performance, and compatibility

### 15.1 Security

| Risk | Mitigation |
|---|---|
| Hash order becomes authority policy | IDs are sorted only for canonical representation; explicit recovery-signed `selected_head` is mandatory. |
| One branch installs recovery keys and selects itself | Recovery keys and threshold come from last common uncontested ancestor. |
| Invalid entries manufacture denial-of-service fork state | Only independently crypto-, operation-, quorum-, chain-, and root-valid entries become evidence. |
| Caller fabricates “validated” evidence | Candidate/evidence/state constructors remain opaque. |
| Signer/approval double counting | Count a `BTreeSet` intersection; signer plus own approval counts once. |
| Resolution omits a known branch | Exact canonical head-set equality is required. |
| Arbitrary state injection | Selected root must match a retained validated branch snapshot; no caller-supplied state is adopted. |
| Evidence loss hides equivocation | Preserve exact entry and approval signatures append-only before and after resolution. |
| Disabled recovery accidentally authorizes | Empty/zero/impossible configs fail closed; no administrator fallback. |
| Unauthenticated evidence reaches audit | Evidence constructed only after exact-CSB and signature verification. |

### 15.2 Reliability and convergence

The implementation guarantees deterministic results for the same authenticated, validated input set. It does not guarantee that every peer has discovered every branch. Missing records return `MissingDependency`; later omitted branches reopen fork state rather than being silently discarded.

A recovery resolution is therefore safe but locally completeness-sensitive. Network availability and dissemination remain outside this pure-core issue.

### 15.3 Performance

- Use `BTreeMap`/`BTreeSet` for deterministic bounded set operations.
- Pair detection is constant aside from evidence cloning; set detection is `O(n log n)` plus lineage traversal.
- Avoid cloning full `GovernanceState` more than the existing validate/apply pipeline requires; share immutable audit blobs internally where practical.
- Apply protocol record/branch count and byte limits at decode/ingestion boundaries already owned by the crate. Do not add unbounded recursive ancestry traversal; use iterative traversal and cycle-impossible opaque validated lineage.
- Full signatures are intentionally retained for audit. Document and test bounded memory behavior.

### 15.4 Privacy

Fork evidence contains public identifiers, signed governance contents, and signatures already supplied to governance validation. The pure core must not emit these to logs or telemetry. Durable callers should follow the repository's local, privacy-sensitive audit posture.

### 15.5 Compatibility and migration

The v2 core is unpublished and not wired into the shipped runtime, so there is no deployed data migration in this issue. Nonetheless, protocol artifacts are compatibility-sensitive:

- Do not change frozen domains or exact-CSB ID derivation.
- Adding private evidence retention does not change wire bytes.
- The normative placeholder `fork.resolve` payload must change because it cannot name all heads or selected state. Treat this as an intentional pre-release protocol amendment, not an incidental refactor.
- Keep the older candidate `SignedForkResolution` and its frozen vector untouched unless a separate migration retires it.
- Add/update normative vectors deliberately. If any existing frozen CSB, signature, ID, state root, or rejection code changes, perform the repository-required schema-version bump and protocol-change note.
- Rollback is removal of the additive fork-aware modules and restoration of the placeholder normative payload before adoption. Once a new normative resolution vector is released, rollback requires preserving decode compatibility or another explicit schema transition.

---

## 16. Rollout and verification plan

1. Land record-evidence retention with no wire changes and run #147/#148 regressions.
2. Land opaque candidate and pure detection tests.
3. Land fork-aware state machine and fail-closed tests.
4. Land typed resolution payload and recovery authorization behind the new fork-aware API.
5. Add full external wire integration and compatibility vectors.
6. Run focused checks:

```bash
cargo fmt --all --check
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
cargo test -p iroh-rooms-v2-core --all-targets --all-features
```

7. Run the full repository gate:

```bash
scripts/verify.sh
```

8. Require manual protocol/security review before merge.
9. Do not claim runtime enforcement until a later integration issue routes runtime governance through this pure core.

---

## 17. Assumptions and resolved ambiguities

These are implementation assumptions derived from the issue text and current repository. They are normative for this plan unless authoritative #134 §7.5 text says otherwise.

1. **Same-predecessor vs different-predecessor wording:** direct siblings sharing a predecessor are the primary fork case, while the acceptance text explicitly requires same-sequence entries from different predecessors. The predicate covers both by treating distinct valid IDs at the same sequence in one community as conflicting and using lineage to establish a common ancestor.
2. **Quorum-valid means full validity:** both branches pass cryptography, exact chain validation against their own predecessors, operation semantics, old-state administrator quorum, and post-state-root verification.
3. **Recovery source:** use recovery configuration from the last common uncontested ancestor, not either contested branch.
4. **Signature container:** reuse verified entry signer plus governance approvals for recovery counting because those signatures bind the exact resolution entry/root. Do not add a parallel record unless #134 mandates it.
5. **Selected state:** select one already validated branch head and root. Reject-both and arbitrary superseding-state actions are not included.
6. **Threshold failure code:** use `InsufficientAuthorization` for a structurally valid `fork.resolve` with `W-1`; reserve `InvalidForkResolution` for malformed/stale/inconsistent resolution semantics.
7. **Completeness:** “all known” is local validated knowledge, not a proof that no withheld branch exists.
8. **Audit persistence:** the core preserves and returns authenticated evidence; durable storage is a later runtime concern.
9. **Publication refusal:** explicitly deferred; the state type should make future fail-closed publication integration possible without implementing certificates now.

---

## 18. Open questions for maintainer/protocol review

1. Does the authoritative #134 §7.5 text intend the broader same-sequence predicate used here, or should different-predecessor detection be expressed only through lineage divergence while direct fork identity remains same-predecessor?
2. Does #134 define a dedicated recovery-approval record/domain, or is reuse of `GovernanceApproval` intended?
3. Is selecting one validated branch the complete resolution action set, or must a future version support rejecting all branches or supplying a separately authenticated superseding state?
4. Should full fork audit evidence become checkpoint/root-visible in #150, and if so, should the checkpoint commit an evidence digest while the six-component governance root continues to commit only the compact marker?
5. What protocol-level branch-count/total-evidence byte limit should be pinned if existing generic record limits do not bound an adversarial number of independently quorum-valid branches?
6. When a late branch appears after a resolution, should the next resolution name the current accepted resolution head plus the late head, as proposed here, or reopen the original branch set and supersede the prior resolution through another explicit model?
7. Should a malformed recovery configuration be prevented at `recovery.set` time in a follow-up, rather than only failing closed when recovery is needed?

None of these questions permits lexical, timestamp, or arrival-order auto-resolution. If an authoritative answer changes wire shape, authority source, or action set, update this specification and compatibility vectors before implementation.

---

## 19. Implementation notes (post-landing)

#149 landed exactly as designed in §5–§11. The new modules
`governance/log/fork.rs` (the pure detection predicate + branch/fork evidence)
and `governance/log/machine.rs` (the `GovernanceMachine` state machine, the
`GovernanceLineage`, the recovery-threshold counter, the seven-rule
`fork.resolve` validator, and the `GovernanceForkAuditRecord`) sit on top of
supporting changes: `records.rs` adds `AuthenticatedGovernanceEvidence` /
`VerifiedGovernanceApprovalEvidence` (trusted construction only inside
`verify_governance_entry`), `authz.rs` exposes the opaque
`ValidatedGovernanceCandidate` and rejects `fork.resolve` from the ordinary
admin-threshold path, `operation.rs` adds the typed `ForkResolve` payload, and
`model.rs`/`state.rs` replace the placeholder marker with the state-root-visible
`ResolvedForkMarker`. No frozen wire byte, domain string, frozen golden vector,
or `Reject`-code set changed (§15.5); the older candidate `governance::fork`
path and its frozen `fork-resolution-accept-winner-v1` vector remain untouched.
The §18 open questions were resolved as follows, each pinned by a test unless
noted:

1. **OQ-1 (same- vs different-predecessor wording):** resolved toward the
   broader predicate this spec assumed. `detect_governance_fork` (§7) returns
   `Some` iff `community_id` matches, exact-CSB ids differ, and `seq` matches.
   The common ancestor is resolved from carried predecessor snapshots for the
   direct same-predecessor case, and — when the two predecessors themselves
   diverge — from the machine's retained `GovernanceLineage::common_ancestor`
   (the pure pair/set predicate returns `MissingDependency` there, since it
   cannot prove deeper ancestry from two candidates alone; §4.7 "never guess").
   Pinned by `same_predecessor_distinct_ids_at_same_seq_is_a_fork` +
   `same_predecessor_sibling_fork_enters_governance_forked` (direct case) and
   `different_predecessor_candidates_are_detected_as_a_fork` (divergent
   predecessor, lineage-resolved).

2. **OQ-2 (dedicated recovery-approval record/domain):** resolved as assumed —
   none introduced. Recovery signatures reuse the verified entry signer plus the
   `GovernanceApproval` set, which already bind community, exact resolution
   entry id, and declared post-resolution root. `count_eligible_recovery_signers`
   builds the union of signer + approval principals and intersects it with the
   last-common-ancestor recovery-key set. Pinned by
   `fork_resolve_with_w_signatures_resolves`,
   `fork_resolve_with_w_minus_one_signatures_is_insufficient`,
   `administrator_only_signers_cannot_authorize_resolution`, and
   `signer_also_approving_counts_once`.

3. **OQ-3 (selected-state action set):** resolved as assumed — selecting one
   already-validated branch head is the complete resolution action.
   `validate_and_commit_resolution` Rule 3 loads the retained snapshot for
   `selected_head` and requires `selected_state_root` to equal its committed
   root; reject-both and arbitrary superseding-state actions are not
   implemented. Pinned by `fork_resolve_selected_root_mismatch_is_state_root_mismatch`
   and `fork_resolve_unknown_selected_head_is_invalid_at_decode`.

4. **OQ-4 (audit evidence checkpoint/root-visible in #150):** not decided here
   — remains #150's call. As designed in §6.6/§11, the six-component governance
   root commits only the compact `ResolvedForkMarker` (branch heads + selected
   head/root); the full authenticated branch/resolution evidence lives in
   `GovernanceForkAuditRecord`, outside the state root (§11.2). Whether #150
   later adds an evidence digest to the checkpoint is deferred and would be a
   separate, schema-aware change.

5. **OQ-5 (branch-count/total-evidence byte limit):** not pinned beyond the
   record/decode limits the crate already owns. `branch_heads` is a
   `Vec<GovernanceId>` decoded through the strict canonical reader (sorted,
   unique, `>= 2`, `selected_head` present exactly once — §6.6); no additional
   adversarial branch-count cap was added. Pinning a protocol-level ceiling
   remains an explicit follow-up if generic record limits prove insufficient.

6. **OQ-6 (late branch after resolution):** resolved toward the proposed
   behavior. The audit chain is append-only (`GovernanceMachine::audit` always
   appends the current unresolved incident, and a successful resolution records
   a `Resolved` incident without deleting branch records), and every
   `fork.resolve` requires exact set-equality with the *current* known heads
   (Rule 2). A later valid branch not covered by an accepted resolution
   therefore re-enters `GovernanceForked` pairing the current accepted
   (resolution-descended) head with the late head, and requires a fresh
   recovery-authorized resolution (§5.2 item 11 / §11.3). No dedicated
   end-to-end late-branch unit test pins this yet; it follows from the
   append-only-audit + exact-set-equality invariants, and a targeted regression
   test is a recommended follow-up.

7. **OQ-7 (malformed recovery config at `recovery.set` time):** resolved as in
   #148 OQ-7 — not strengthened at `recovery.set`. That operation keeps its
   existing apply semantics; #149 only adds fail-closed validation at resolution
   time (`recovery_authorization_invariants_hold`: non-empty, sorted-unique, and
   `1 <= threshold <= len(R)`), with no administrator fallback (§5.4 item 14).
   Pinned by `disabled_recovery_config_fails_closed`.

### Additional resolutions against §6

- **Observation API shape (§6.5):** the implementation uses the explicitly
  allowed equivalent `(new_state, event)` form. `GovernanceMachine::observe`
  returns `Result<GovernanceObservation, Reject>` and assigns the successor
  state to `self` only on success, so a failed observation leaves the entire
  previous machine state byte-for-byte unchanged (§5.2 item 5).
  `GovernanceObservation::{Advanced, ForkDetected, Duplicate}` is the
  descriptor; the machine holds the authoritative state and retained evidence.

- **`fork.resolve` is not administrator-authorized (§2.3 / §6.2):** the
  ordinary `validate_governance_candidate` rejects a `ForkResolve` payload with
  `Reject::InvalidForkResolution`, so a resolution can never pass the admin-
  threshold gate. Only the machine's dedicated recovery validator
  (`observe_forked` → `validate_and_commit_resolution`) can authorize one.
  Pinned by `fork_resolve_while_linear_is_invalid`.

- **Authenticated evidence retention (§6.1):** `AuthenticatedGovernanceEvidence`
  (entry id + body + exact CSB + signer + signature + verified approval
  evidence) and `VerifiedGovernanceApprovalEvidence` (body + exact CSB +
  signature) are constructed only inside `verify_governance_entry` after every
  crypto/canonical/approval-binding check passes, so unauthenticated bytes can
  never reach audit evidence. `VerifiedGovernanceEntry::approvals()` now returns
  `&[VerifiedGovernanceApprovalEvidence]`; this is a source-level accessor change
  inside the unpublished crate only. Pinned by
  `evidence_preserves_both_branch_signatures_and_csbs`,
  `both_branch_approvals_preserved_in_audit_before_and_after_resolution`, and
  `e2e_both_branch_approvals_preserved_in_audit_over_wire`.

- **State-root-visible marker (§6.6):** the placeholder
  `ForkResolutionMarker { evidence: [..; 2], decision: u8 }` is replaced by
  `ResolvedForkMarker { branch_heads, selected_head, selected_state_root,
  created_at_ms }` under `CommunityPolicy::fork_markers`. The `resolution_entry`
  field is deliberately omitted to avoid a self-referential derivation through
  the post-marker state root (§6.6); the resolution entry id is retained only in
  the outer `GovernanceForkAuditStatus::Resolved` record. `policy.set` preserves
  markers (append-only). Pinned by `apply_fork_resolve_records_marker`.

- **No lexical tie-break (§5.6):** branch ids are sorted only for canonical
  representation and exact set-equality; selection is always the explicit
  `selected_head`. Pinned by `fork_is_not_auto_resolved_by_hash_order` (selects
  both the lexically larger and, in a separate fixture, the lexically smaller
  head) and `e2e_fork_not_auto_resolved_by_hash_order_over_wire`.

### Verification

`cargo test -p iroh-rooms-v2-core --all-targets --all-features` is green: the
`governance/log/fork.rs` detection unit tests, the `governance/log/machine.rs`
state-machine + seven-rule + recovery-threshold unit tests, the wire-bytes
lifecycle in `tests/v2_governance_fork_e2e.rs` (every issue acceptance criterion
driven from raw CSB + signatures through `GovernanceEntry::from_received_csb` →
`verify_governance_entry` → `GovernanceMachine::observe`), and the corrected
`fork.resolve` registry row in `tests/v2_governance_log_e2e.rs`. The existing
`taxonomy.rs`, `signed_records_golden.rs`, `identifiers.rs`, and
`banned_dependencies.rs` suites remain green and unchanged, because no frozen
wire byte, domain string, frozen golden vector, `Reject`-code set, or crate
dependency changed.

### Deferred scope (unchanged)

Publication-certificate refusal (Phase C — no publication-certificate model
exists yet), operator fork-recovery UX (#159), checkpoints/snapshots (#150),
network branch discovery/anti-entropy/global-completeness proofs, durable
incident storage/metrics/logs, and routing of the shipped v1 runtime's
governance through this pure core all remain separate later issues. The pure
core exposes audit-ready typed records and performs no persistence; a runtime
may rebuild `GovernanceLineage` from authenticated records (§6.4).
