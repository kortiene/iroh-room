# Spec: v2 Governance Authorization Rules

| | |
|---|---|
| **Issue** | #148 — `[CORE] v2 governance authorization rules (#134 §7.4)` |
| **Labels** | `type/feature` `area/protocol` `priority/p1` `risk/medium` |
| **Refs** | #134 §7.4; depends on #147; fork detection/recovery-threshold signing remain #149 |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/src/governance/log/authz.rs`, with supporting changes in `records.rs`, `state.rs`, and `mod.rs`, plus the updated end-to-end pipeline in `tests/v2_governance_log_e2e.rs`. How the §15 open questions were resolved is recorded in §16. |

---

## 1. Summary

Implement the five-rule authorization predicate for a post-genesis governance entry as a deterministic, side-effect-free API in the normative `governance::log` module:

```rust
pub fn validate_governance_entry(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<(), RejectionReason>;
```

The predicate evaluates, in order:

1. the predecessor is a previously validated state and its committed `state_root` equals a fresh root computation;
2. the candidate has the exact next sequence number and exact predecessor ID;
3. the operation is structurally and semantically valid when applied to state `n-1`;
4. the union of cryptographically verified entry signer and approval signers contains at least the old state's threshold of distinct old-state administrators;
5. deterministic apply to state `n-1` produces the candidate's declared post-state root.

Authorization always reads the old state. In particular, an `admin.set` proposal is signed under the old administrator set and old threshold; the proposed set and threshold become authoritative only after validation succeeds and the caller commits the returned transition. The implementation remains pure and adds no wall-clock, network, store, async, logging, or runtime dependency.

The issue text names `prev_state` and `entry` abstractly. The concrete wrapper types above are required because the existing `GovernanceState` does not retain the accepted predecessor ID/sequence and `verify_entry_full` currently discards the verified signer/approval set. Passing the existing bare types cannot implement all five rules without trusting caller-supplied facts.

---

## 2. Repository context and current behavior

### 2.1 Normative implementation boundary

The implementation belongs under `crates/iroh-rooms-v2-core/src/governance/log/`, which is the landed #147 model. Do not add #148 behavior to the older sibling `governance/authz.rs`: that candidate path uses a single-admin/action model and legacy domains and is not compatible with the normative fourteen-operation log.

Relevant current files:

- `governance/log/mod.rs` declares authorization as deferred and exports the normative records/state functions.
- `governance/log/records.rs` defines `GovernanceEntry`, its signer, approvals, and crypto verification. `verify_entry_full` verifies and sorts approvals internally but returns only `GovernanceEntryBody`, losing authorization inputs.
- `governance/log/state.rs` defines the six-component state, all fourteen pure apply functions, `compute_state_root`, `apply_verified_entry`, and `check_chain_link`.
- `governance/log/model.rs` defines the current administrator set/threshold, members, statuses, devices, and recovery state.
- `governance/log/operation.rs` defines the closed fourteen-operation registry.
- `error.rs` already exposes stable `Reject` codes suitable for the five rules.
- `tests/v2_governance_log_e2e.rs` exercises decode → crypto → chain → apply/root, but has no authorization stage.

The v2 core crate is `publish = false`, pure, and unused by the shipped v1 runtime. No migration or runtime rollout is part of this issue.

### 2.2 Existing strengths to preserve

- Entry signatures use the frozen `domain::GOVERNANCE_ENTRY` domain.
- Approval signatures use `domain::GOVERNANCE_APPROVAL` and verify directly under `body.approver`.
- Approvals bind community ID, entry ID, and declared post-state root.
- Duplicate approvers are rejected.
- `GovernanceOperationKind` is a closed enum with fourteen variants.
- Apply and state-root computation are deterministic and side-effect free.
- State roots commit to administrators/threshold, recovery, replicas, members/devices/roles, streams, and community policy.
- `created_at_ms` is advisory signed data; authorization does not consult a local clock.

### 2.3 Gaps #148 must close

1. There is no normative authorization module or predicate.
2. A verified record does not preserve its verified signer and sorted approval bodies.
3. A bare `GovernanceState` has neither predecessor sequence nor predecessor entry ID, so exact chain validation cannot be performed from the requested two arguments.
4. `apply_verified_entry` can currently apply a cryptographically valid but unauthorized operation.
5. Operation validity is incomplete around member/device status and recovery configuration.
6. `admin.set` replaces the administrator component but does not synchronize member roles; authorization must therefore use `state.administrators`, not member role, as the sole admin authority.
7. A current registry e2e table duplicates `device.revoke` and omits `device.grant`; it should be corrected while adding the authorization pipeline test.

---

## 3. Scope

### 3.1 In scope

1. A pure `validate_governance_entry(prev_state, entry)` predicate implementing all five rules in fixed order.
2. Typed verified-entry and validated-predecessor representations that carry every input needed by the predicate without re-verifying or trusting unverified identity claims.
3. Threshold counting over distinct old-state administrators.
4. Exact old-set authorization and post-commit new-set effectiveness for `admin.set`.
5. Operation validity under old state, including member/device ownership and status checks.
6. Unit, table-driven, integration, and property tests for the five rules, administrator transitions, threshold boundaries, and device-binding transitions.
7. Stable typed rejection behavior and taxonomy coverage.
8. Updating normative module exports and documentation.

### 3.2 Out of scope

- Detecting two quorum-valid entries that share a predecessor, branch selection, equivocation evidence, or fail-closed fork state (#149).
- Recovery-threshold authorization, including special handling of `fork.resolve` (#149). In #148, `fork.resolve` is treated like every other ordinary operation and requires the current admin threshold; #149 may replace that rule deliberately.
- Network, store, replica receipt, checkpoint, CLI, SDK, or shipped runtime integration.
- New signed wire fields or changes to frozen canonical bytes/domains/golden signatures.
- A principal/device two-key protocol. The current entry envelope identifies a principal signer only.
- Wall-clock expiry or freshness checks.
- Git, branch, pull-request, or GitHub work.

---

## 4. Complete behavioral inventory

### 4.1 Required capabilities

- Validate a candidate against exactly one accepted predecessor snapshot.
- Reject a stale, fabricated, or root-inconsistent predecessor before considering the candidate.
- Reject skipped, repeated, or otherwise incorrect sequences.
- Reject missing, extra, or incorrect predecessor IDs, including the genesis boundary.
- Apply every registered operation to a clone of old state to establish rule 3 without mutating old state.
- Count each old-state administrator at most once across the entry signer and approvals.
- Count the signer as one threshold signature when the signer belongs to the old admin set.
- Do not count outsider signers or approvals.
- Do not count administrators newly proposed by the same `admin.set` entry.
- Do count an old administrator being removed by that entry.
- Compare the deterministic candidate state root to the signed declared root.
- Return success without committing or exposing partially applied state.
- Permit callers to apply/commit only after successful validation.

### 4.2 Error paths

| Rule | Condition | Result |
|---|---|---|
| 1 | predecessor was not produced by verified genesis or a prior successful validation/commit | Construction of `ValidatedGovernanceState` is unavailable/fails; no predicate call with an unvalidated predecessor is possible through safe public APIs. |
| 1 | recomputed predecessor root differs from its committed root | `Reject::StateRootMismatch` |
| 1 | candidate community differs from predecessor community | `Reject::InvalidContent` |
| 2 | `seq` is not exactly predecessor sequence + 1, including overflow | `Reject::InvalidContent` |
| 2 | candidate `prev` differs from the exact expected predecessor (including an extra `prev` at genesis) | `Reject::InvalidContent`, preserving `check_chain_link` compatibility |
| 3 | kind/payload mismatch, invalid subject, invalid threshold/set, absent target, invalid status transition, duplicate-only transition, or other apply failure | preserve `Reject::InvalidContent` |
| 4 | fewer than old threshold distinct old admins signed | `Reject::InsufficientAuthorization` |
| 4 | old admin set is empty, threshold is zero, threshold exceeds old admin count, or old admin list is noncanonical | `Reject::InsufficientAuthorization` (fail closed; such state must not authorize recovery by accident) |
| 4 | duplicate approval, bad approval signature, or wrong approval binding | rejected earlier by verified-entry construction as `Reject::InvalidApproval` or `Reject::BadSignature`; never passed to authorization |
| 5 | post-apply root differs from `entry.body.state_root` | `Reject::StateRootMismatch` |

`RejectionReason` should be a public type alias to the existing stable taxonomy:

```rust
pub type RejectionReason = crate::error::Reject;
```

Do not add a threshold-specific code in this issue. `insufficient_authorization` already covers signer/approval sets that cannot authorize an action. Preserve the current `check_chain_link` mapping of all sequence/predecessor violations to `InvalidContent`; do not create a second chain taxonomy solely for this work. A later entry with no `prev` is rejected even earlier by canonical entry decoding, so authorization-level rule-2 tests use a wrong predecessor ID, an extra genesis predecessor, or a wrong sequence.

### 4.3 Empty and boundary inputs

- Zero approvals: accepted only when the entry signer is an old administrator and old threshold is exactly one; otherwise rejected.
- Empty old admin set or threshold zero: always reject authorization.
- Threshold one: one distinct old-admin signature succeeds.
- Threshold equal to admin count: every old admin must sign.
- Threshold `W-1`: rejects; exactly `W`: accepts; more than `W`: accepts.
- Signer also supplies an approval: contributes one, not two. The approval may remain in the verified record, but threshold counting uses a set.
- Multiple approvals from one principal: rejected by crypto/binding verification before authorization.
- Approvals in any order: produce the same decision.
- Outsider signatures: remain cryptographically valid but contribute zero; they do not turn an otherwise sufficient old-admin quorum into an error.
- First post-genesis entry: expected `seq == 1` and `prev == None`.
- Later entry: expected sequence uses checked addition and `prev == Some(old_tip_id)`.
- Sequence at `u64::MAX`: no next entry can be authorized; checked addition rejects rather than wrapping or panicking.
- Candidate operation that is structurally valid but a no-op: follow the existing operation's semantics. Set/idempotent operations may succeed if already defined that way; duplicate `migration.accept`, duplicate stream create, and malformed fork marker remain invalid under existing apply rules.
- Wrong candidate community: reject before apply/quorum/root checks.
- Wrong predecessor root and wrong candidate post-root are both `state_root_mismatch`, but separate tests must isolate rule 1 and rule 5.
- The function must not panic for malformed in-memory states or thresholds.

### 4.4 Device-binding validity in #148

The issue requests property tests over device-binding validity, but the #147 signed entry contains no signing `DeviceId`; it contains only `signer: PrincipalId`. The current key model deliberately maps a principal and its device to the same bytes, while genesis administrators have no device records. Therefore #148 must not invent a signer-device check or alter frozen signed bytes.

For this issue, **device-binding validity means the rule-3 validity of `device.grant` and `device.revoke` ownership/status transitions**:

- `device.grant(member, device)` requires the member to exist and be active.
- A device ID must be bound to at most one member in the entire old state; granting a device already bound to another member is invalid.
- Granting an already-active device to the same member is invalid rather than silently replacing it.
- Regranting a revoked device is invalid unless a later protocol issue explicitly defines reactivation semantics.
- `device.revoke(member, device)` requires an existing active member and an active device bound to that member.
- Revoking an absent device, a device bound to another member, or an already-revoked device is invalid.
- These rules are tested over generated states and device/member pairs.

A future two-key envelope/device-signature issue must define signer device identity and schema-version handling before active-device signer authorization can be added.

### 4.5 Public interfaces to preserve

Keep the existing #147 public record and state APIs source-compatible unless a separate intentional deprecation is approved:

- `GovernanceEntry`, `GovernanceEntryBody`, `GovernanceApproval`, `GovernanceApprovalBody`
- `verify_entry_crypto`, `verify_approval_crypto`, `verify_entry_full`
- `GovernanceState`, `apply`, `apply_verified_entry`, `check_chain_link`, `compute_state_root`, `verify_state_root`
- all fourteen operation and payload types
- existing `Reject` variants and `.code()` strings

Add safer typed APIs rather than changing canonical record fields. Mark low-level `apply_verified_entry` documentation clearly: it checks community/apply/post-root only and is not an authorization boundary. Normative receiver examples and tests must use the new validation/commit pipeline.

---

## 5. Design decisions

### D1 — Implement in `governance::log::authz`

Add `crates/iroh-rooms-v2-core/src/governance/log/authz.rs` and export only explicit public items from `governance/log/mod.rs`. Do not reuse the older `governance::authz` model or its legacy types.

### D2 — Preserve verified authorization inputs

Introduce an immutable verified representation:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGovernanceEntry {
    body: GovernanceEntryBody,
    signer: PrincipalId,
    approvals: Vec<GovernanceApprovalBody>,
}

impl VerifiedGovernanceEntry {
    pub fn body(&self) -> &GovernanceEntryBody;
    pub fn signer(&self) -> PrincipalId;
    pub fn approvals(&self) -> &[GovernanceApprovalBody];
}
```

Change the internals of `verify_entry_full` to call a new function:

```rust
pub fn verify_governance_entry(
    entry: &GovernanceEntry,
) -> Result<VerifiedGovernanceEntry, Reject>;
```

`verify_governance_entry` performs the current entry crypto, approval signature, binding, sorting, and duplicate rejection and returns the sorted verified approval bodies. Keep `verify_entry_full` as a compatibility wrapper that maps the result to a cloned body.

Fields stay private so callers cannot construct “verified” identities without cryptographic verification. Tests that need fixtures must build and sign real records.

### D3 — Represent an accepted predecessor explicitly

Introduce an opaque validated snapshot/cursor:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedGovernanceState {
    state: GovernanceState,
    tip: GovernanceTip,
    committed_state_root: StateRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceTip {
    Genesis,
    Entry { seq: u64, id: GovernanceId },
}
```

Public read-only accessors expose the state, tip, and root. Safe constructors are:

```rust
pub fn validated_genesis_state(
    config: &GenesisConfig,
    signatures: &[GenesisSignature],
) -> Result<ValidatedGovernanceState, Reject>;

pub fn validate_and_apply_governance_entry(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<ValidatedGovernanceState, RejectionReason>;
```

`validated_genesis_state` calls `verify_genesis`, builds `GovernanceState::from_genesis`, and records its computed root. `validate_and_apply_governance_entry` calls the required unit-returning predicate, applies once more or reuses an internal candidate helper, and returns a new snapshot with the new tip and root. There is no public unchecked constructor.

This wrapper metadata is not a seventh governance state-root component. The committed root remains the exact frozen six-component root over `GovernanceState`; the wrapper is local validation evidence/cursor metadata.

### D4 — Evaluate the five rules in normative order

Use one internal helper to avoid validation/apply disagreement:

```rust
fn validate_candidate(
    prev_state: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<GovernanceState, RejectionReason>;
```

`validate_governance_entry` maps the returned candidate state to `()`. `validate_and_apply_governance_entry` wraps that returned candidate as the next accepted snapshot. `validate_candidate` executes:

1. recompute and verify predecessor root/community;
2. validate exact sequence and predecessor link;
3. call `apply(old, payload)` and retain the candidate state;
4. verify old-admin threshold;
5. compare `compute_state_root(candidate)` with the declared root.

No state is committed before rule 5 succeeds. This order also enables one-negative-rule tests to assert the intended rejection path.

### D5 — Count the distinct union of old-admin signers

Build a `BTreeSet<PrincipalId>` containing `entry.signer()` and every verified approval `approver`, then retain only identities found in `prev_state.state().administrators.administrators`. Compare the count to the old threshold.

Consequences:

- signer plus approval from the same admin counts once;
- approval order is irrelevant;
- outsider signatures do not count;
- newly proposed admins do not count for their own `admin.set`;
- removed old admins remain eligible for that transition;
- signer need not separately author an approval;
- an outsider entry signer is permitted if the approvals independently contain `W` distinct old admins, because §7.4 specifies a threshold of admins signed, not an additional signer-role rule.

Do not consult `members.role == Admin` for threshold membership. `AdministratorState` is the state-root-committed source of truth and may currently diverge from member role records after `admin.set`.

### D6 — New administrator state is effective only after commit

For `admin.set`, rule 3 validates the proposed set/threshold and computes a candidate state, but rule 4 reads only `prev_state.state().administrators`. A successful unit-returning validation does not mutate `prev_state`. Only `validate_and_apply_governance_entry` returns a new accepted snapshot. The next entry is authorized against that returned snapshot and therefore uses the new set/threshold.

Property tests must generate overlapping, disjoint, expanding, and shrinking old/new sets and prove this three-part invariant:

1. old quorum + new non-quorum authorizes the transition;
2. new quorum without old quorum cannot authorize the transition;
3. after commit, the same new quorum can authorize the next ordinary entry while an old-only quorum cannot unless those principals remain admins.

### D7 — Rule 3 owns state-dependent operation validity

Keep the closed enum match in `state::apply`; no fallback arm is possible. Strengthen only the semantic validity needed by #148 acceptance:

- validate old administrator-state invariants before quorum counting;
- retain existing admin-set checks (`non-empty`, sorted unique, `1 <= threshold <= len`);
- preserve the existing `recovery.set` apply semantics in #148; do not guess new recovery threshold/config validity rules while recovery signing is deferred and empty-recovery semantics are unresolved;
- enforce active member and unique ownership/status rules for device transitions from §4.4;
- preserve all existing missing-subject/duplicate operation failures;
- do not introduce admin survivability, last-device, or recovery-signing policy not stated by this issue.

If changing operation validity would alter an existing accepted vector, treat that as a protocol review item and add an explicit negative vector; canonical bytes and successful #147 root vectors need not change merely because invalid transitions now reject.

### D8 — Preserve frozen wire and root formats

No field is added to `GovernanceEntryBody`, `GovernanceApprovalBody`, or the state-root record. No domain string, canonical CBOR encoding, ID derivation, signature, or successful state-root vector changes. New wrapper types are in-memory only.

### D9 — Keep observability at the typed error boundary

The pure core emits no logs, metrics, traces, or diagnostics. Callers observe the stable `Reject::code()` value. Tests must ensure a rejection leaves the predecessor wrapper and its state/root unchanged.

---

## 6. Five-rule algorithm

Pseudocode:

```rust
fn validate_candidate(
    prev: &ValidatedGovernanceState,
    entry: &VerifiedGovernanceEntry,
) -> Result<GovernanceState, Reject> {
    let body = entry.body();
    let old = prev.state();

    // Rule 1: predecessor validity/root/community.
    verify_state_root(old, prev.committed_state_root())?;
    if body.community_id != old.community_id {
        return Err(Reject::InvalidContent);
    }

    // Rule 2: exact sequence and predecessor.
    let (expected_seq, expected_prev) = match prev.tip() {
        GovernanceTip::Genesis => (1, None),
        GovernanceTip::Entry { seq, id } => (
            seq.checked_add(1).ok_or(Reject::InvalidContent)?,
            Some(id),
        ),
    };
    check_exact_chain_link(body, expected_prev, expected_seq)?;

    // Rule 3: operation validity under old state.
    let candidate = apply(old, &body.payload)?;

    // Rule 4: W distinct old admins signed.
    verify_old_admin_threshold(&old.administrators, entry)?;

    // Rule 5: declared post-state root.
    verify_state_root(&candidate, &body.state_root)?;

    Ok(candidate)
}
```

`check_exact_chain_link` should reuse the existing `check_chain_link` behavior and return `InvalidContent` for every sequence/predecessor mismatch. Canonical decoding already rejects the unrepresentable `seq > 1` plus missing-`prev` shape before authorization. The algorithm must never inspect `created_at_ms`, system time, random state, storage, network state, or arrival order.

---

## 7. Rule-isolated negative tests

Acceptance requires a negative test for each rule where all other rules are valid.

| Rule | Test construction | Expected rejection |
|---|---|---|
| 1 | Start from a valid predecessor wrapper, clone/tamper its internal state through a crate-private test helper without updating its committed root; candidate has correct next link, valid op, old quorum, and post-root for the tampered state. | `StateRootMismatch` before candidate checks |
| 2 | Valid predecessor/root, valid op, sufficient old quorum, correct post-root; alter only `seq` or use a different predecessor ID, then re-sign entry/approvals. | `InvalidContent` |
| 3 | Valid predecessor/link/quorum; use `device.revoke` for an absent device (or stream policy for absent stream), and set any declared root because apply must fail first. | `InvalidContent` |
| 4 | Valid predecessor/link/op/post-root; attach exactly `W-1` distinct old-admin signatures. | `InsufficientAuthorization` |
| 5 | Valid predecessor/link/op/old quorum; declare a different post-root and sign/approve that exact wrong root so all crypto and approval bindings remain valid. | `StateRootMismatch` |

Also include a dedicated acceptance test for a valid candidate paired with a predecessor whose committed root is wrong. This is separate from the candidate post-root mismatch test.

Because verified-wrapper fields are private, test-only constructors for deliberate rule-1 corruption must remain under `#[cfg(test)]` or test inside the defining module; production callers must not be able to forge validated state.

---

## 8. Detailed test strategy

### 8.1 Unit tests

Add focused tests in `governance/log/authz.rs` or a sibling test module:

- one valid 2-of-3 ordinary entry;
- the five isolated negatives from §7;
- foreign-community rejection;
- genesis boundary (`seq=1`, no predecessor);
- later exact sequence/predecessor;
- skipped/repeated sequence, wrong predecessor ID, and extra predecessor at the genesis boundary; canonical decode separately covers a missing predecessor on later entries;
- `u64::MAX` sequence overflow rejection;
- empty approvals at threshold one with admin signer;
- empty approvals above threshold one;
- signer counted once when also an approver;
- outsider signer plus `W` old-admin approvals accepted;
- outsider approvals ignored;
- approval order independence;
- malformed old admin threshold fails closed;
- validation leaves predecessor unchanged;
- compatibility `verify_entry_full` and new `verify_governance_entry` agree on the body.

Crypto/binding tests remain in `records.rs`, including bad signature, wrong community, wrong entry ID, wrong root, and duplicate approver. Authorization tests consume only `VerifiedGovernanceEntry` so policy code never handles unverified approvals.

### 8.2 Property tests: threshold edges

Use existing dev dependency `proptest`; do not add a dependency. Generate unique admin sets of size `1..=16`, threshold `W in 1..=N`, a signer, and approval subsets. Assert:

- any `W-1` distinct old-admin signers reject;
- any exactly `W` distinct old-admin signers accept when all other rules are valid;
- any superset of a valid quorum accepts;
- duplicate signer/approval identity never increases the count;
- adding/removing outsiders never changes the result;
- permuting approvals never changes the result.

Sign real entry and approval records with deterministic generated seed material. Avoid constructing `VerifiedGovernanceEntry` directly.

### 8.3 Property tests: administrator transitions

Generate valid non-empty old/new admin sets and valid thresholds, including:

- identical sets with threshold decrease/increase;
- overlapping sets;
- disjoint sets;
- one-admin and full `N-of-N` sets;
- removal of one or all old admins from the proposal;
- addition of principals that were not members.

For every generated transition, assert the D6 old-before/new-after behavior and that validation alone does not mutate old state. Compute and sign each declared post-root from deterministic apply.

### 8.4 Property tests: device binding

Generate member maps and device IDs and assert:

- active member + fresh globally unbound device grant succeeds;
- absent or revoked member grant rejects;
- same-member active duplicate and revoked-device regrant reject;
- cross-member duplicate device binding rejects;
- active owner + active device revoke succeeds;
- absent member/device, wrong owner, revoked member, and already-revoked device reject;
- successful grant/revoke changes only the members/devices/roles component and post-root deterministically;
- rejected operations do not alter the predecessor state or root.

### 8.5 Operation matrix

Create a table containing exactly `GovernanceOperationKind::ALL`, not a manually counted duplicate-prone list. For each of all fourteen operations, provide:

- one structurally valid payload/state fixture authorized by exactly `W` old admins;
- one rule-4 denial with exactly `W-1` signatures while keeping operation/link/roots valid;
- where state-dependent, one rule-3 denial.

Correct `tests/v2_governance_log_e2e.rs` so `device.grant` appears once and `device.revoke` appears once. Update its receiver pipeline to:

```text
canonical decode
→ verified entry/approval crypto and bindings
→ validate_governance_entry against accepted predecessor
→ validate-and-apply/commit accepted snapshot
```

### 8.6 Taxonomy and golden vectors

- Ensure `insufficient_authorization` is exercised in `tests/taxonomy.rs`/golden negative metadata as required by the existing completeness policy.
- Do not change successful signed-record bytes, IDs, signatures, state roots, or domain strings.
- Add a deterministic authorization outcome vector only if the existing golden-vector convention requires every public rejection boundary; the vector should reference existing signed bytes rather than invent a new wire record.

### 8.7 Verification commands

After implementation, run:

```bash
cargo fmt --all --check
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo test -p iroh-rooms-v2-core --test v2_governance_log_e2e --all-features
cargo test -p iroh-rooms-v2-core --test signed_records_golden --all-features
cargo test -p iroh-rooms-v2-core --test taxonomy --all-features
cargo test -p iroh-rooms-v2-core --test banned_dependencies --all-features
cargo tree -p iroh-rooms-v2-core
scripts/verify.sh
```

Inspect `cargo tree` to confirm no network/store/async dependency entered the pure crate.

---

## 9. Implementation steps

1. **Add the normative authorization module**
   - Create `governance/log/authz.rs`.
   - Export `validate_governance_entry`, `validate_and_apply_governance_entry`, `validated_genesis_state`, `ValidatedGovernanceState`, `GovernanceTip`, and `RejectionReason` explicitly from `log/mod.rs`.
   - Update module docs to mark #148 implemented while retaining #149/#150 deferrals.

2. **Preserve verified signer/approval context**
   - Add opaque `VerifiedGovernanceEntry` in `records.rs`.
   - Refactor current `verify_entry_full` internals into `verify_governance_entry`.
   - Return canonically sorted verified approval bodies and the verified signer.
   - Keep `verify_entry_full` as a body-returning compatibility wrapper.
   - Add accessor and anti-forgery/compatibility tests.

3. **Add validated predecessor state**
   - Implement the opaque wrapper and tip metadata.
   - Implement verified-genesis construction through existing `verify_genesis`.
   - Keep cursor metadata outside the six-component root.
   - Add tests that genesis root/cursor construction is deterministic.

4. **Implement rules 1 and 2**
   - Recompute predecessor root every call.
   - Compare communities.
   - Derive exact expected sequence/predecessor from the opaque tip with checked arithmetic.
   - Reuse or narrowly adapt `check_chain_link`, preserving `InvalidContent` for sequence/predecessor mismatches.

5. **Complete rule-3 validity**
   - Reuse `apply` as the operation-validity authority.
   - Preserve existing `recovery.set` validity behavior; record stronger threshold/config rules as deferred until the normative recovery semantics are available.
   - Add active member, global uniqueness, ownership, and active-status validation for device operations.
   - Keep all operations pure and clone-and-return.

6. **Implement old-admin threshold counting**
   - Validate old `AdministratorState` invariants.
   - Count the distinct union of signer and approval approvers intersected with old admins.
   - Compare safely against the old threshold.
   - Do not use proposed admin state or member roles.

7. **Implement rule 5 and atomic accepted transition**
   - Compare the candidate state's computed root to the signed declaration.
   - Return `()` from the required predicate.
   - Return the newly wrapped state only from `validate_and_apply_governance_entry` after all rules pass.
   - Document `apply_verified_entry` as lower-level/non-authorizing.

8. **Add exhaustive deterministic tests**
   - Add one negative per rule and the complete operation matrix.
   - Fix the duplicate `device.revoke`/missing `device.grant` registry e2e case.
   - Update the e2e receiver pipeline to use verified entry and validated predecessor wrappers.

9. **Add property tests**
   - Implement threshold, admin-transition, and device-binding generators and properties from §8.
   - Keep case counts bounded and deterministic enough for CI.

10. **Update taxonomy/docs and verify**
    - Update Rust docs and parent spec acceptance only when implementation lands.
    - Preserve all frozen vector bytes unless a separately reviewed schema change is unavoidable.
    - Run the commands in §8.7.

---

## 10. Acceptance criteria mapping

| Acceptance item | Required evidence |
|---|---|
| Each of the five rules has a negative test | Five isolated tests in §7, with valid crypto and all non-target rules satisfied. |
| Admin-set transition uses old set and new set only post-commit | Generated D6 three-stage property plus deterministic disjoint-set regression test. |
| `W-1` rejected and `W` accepted | Threshold property across generated `N/W`, plus fixed 2-of-3 regression. |
| Predecessor `state_root` mismatch rejected | Rule-1 corrupted predecessor wrapper test returns `state_root_mismatch`. |
| Pure requested predicate | Public two-argument function returns `Result<(), RejectionReason>` and has no IO, clock, randomness, mutation, or global state. |
| Property tests cover device-binding validity | Generated owner/member/status grant/revoke properties from §8.4. |
| Distinct admins only | signer/approval overlap, duplicate, outsider, and permutation tests. |
| New set not used prematurely | new-only quorum rejects transition; old quorum succeeds; next committed entry flips authority. |
| No fork/recovery scope creep | no competing-entry detection; `fork.resolve` remains ordinary admin-threshold operation until #149. |
| Frozen protocol surfaces preserved | signed-record and state-root golden suites remain byte-identical. |

---

## 11. Security, privacy, reliability, and performance

### Security

- Opaque verified/validated wrappers prevent policy code from trusting unsigned approver identities or fabricated predecessor metadata.
- Approval identity is bound to its Ed25519 signature and to community/entry/post-root before threshold counting.
- Distinct-set counting prevents signer/approval double counting.
- Old-state evaluation prevents a proposed administrator set from authorizing itself.
- Invalid predecessor roots and declared post-roots fail closed.
- Unsupported operation strings remain rejected at decode; the closed Rust enum has no default allow path.
- No clock-based rule can diverge across replicas.

### Privacy

- The pure function stores and emits no data.
- Tests use deterministic synthetic seed keys and identifiers only.
- No signatures, private seeds, personal data, or network endpoints are logged.

### Reliability

- Identical predecessor wrapper and verified entry produce the same result.
- Rejections leave predecessor state unchanged.
- Checked sequence arithmetic prevents wraparound.
- `BTreeSet` gives deterministic counting independent of approval order.
- A rebuilt accepted chain reproduces identical state roots and authority transitions.

### Performance

- Root recomputation and clone/apply match the existing #147 cost model.
- Threshold counting is `O(A + S log S)` with small sorted admin/signature sets; no quadratic scan is necessary.
- Device global-uniqueness validation may scan members/devices; use a helper and consider an index only if profiling shows need. Do not add mutable caches that could diverge from state roots.
- Property generators must cap administrator/member/device counts to keep CI predictable.

---

## 12. Rollout, rollback, and migration

### Rollout

1. Land as unused, pure protocol infrastructure inside `iroh-rooms-v2-core`.
2. Keep the crate unpublished and absent from SDK/CLI/runtime exports.
3. Require protocol maintainer review of threshold semantics, error ordering, and wrapper construction.
4. Let later store/network work adopt the accepted-state pipeline separately.

### Rollback

- Revert the new in-memory wrappers, authz module, semantic validity checks, and tests together.
- Existing canonical wire records and successful state-root vectors remain unchanged, so rollback requires no data, storage, or network migration.
- If only policy semantics are disputed, keep the verified-entry wrapper (it closes a trust-boundary gap) but disable downstream use in a separately reviewed change; do not silently weaken threshold checks.

### Compatibility/migration impact

- No v1 or shipped runtime behavior changes.
- No database migration.
- No wire schema or version bump is expected.
- `verify_entry_full` remains source compatible.
- New consumers should prefer `verify_governance_entry` and the validated-state pipeline; direct `apply_verified_entry` remains a low-level compatibility API, not an authorization guarantee.

---

## 13. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Exact #134 §7.4 text is absent from the checkout | Threshold/signer/device interpretation may diverge from the decision record | Resolve open questions below with maintainers before code; encode the approved interpretation in property tests. |
| Bare requested types cannot carry predecessor proof or approvals | A nominal implementation could skip rules 1, 2, or 4 | Use opaque `ValidatedGovernanceState` and `VerifiedGovernanceEntry` while preserving the requested function name/shape. |
| Candidate and normative governance modules are confused | Wrong domains/model could be extended | Implement only under `governance::log`; use fully qualified imports in tests. |
| Signer is accidentally double-counted via approval | Threshold bypass | Count a `BTreeSet` union and property-test overlap. |
| Proposed admins authorize themselves | Governance takeover | Rule 4 reads old state only; disjoint-set property test. |
| Member role and administrator component diverge | Different nodes choose different authority | Define `AdministratorState` as sole threshold source; separately track model normalization if desired. |
| Device-signature semantics are guessed | Existing genesis becomes unusable or wire schema drifts | Limit #148 device tests to operation ownership/status; defer signer-device protocol changes. |
| Low-level apply remains callable without authz | A future consumer could bypass policy | Document it as non-authorizing and make all normative examples/tests use the accepted-state API; consider later visibility deprecation. |
| Chain-error taxonomy drifts from #147 | Compatibility drift | Preserve `InvalidContent` for exact-link failures and keep `MissingDependency` unchanged for other existing consumers. |
| Stronger invalid-transition checks alter accepted behavior | Consensus compatibility concern | Add focused negative tests and protocol review; do not change canonical successful vectors. |

---

## 14. Assumptions

1. #147's normative `governance::log` types and frozen #146 domains are the implementation foundation.
2. A governance threshold counts signatures from the entry signer and approval signers as one distinct union.
3. The entry signer need not be an admin if `W` distinct old admins supplied verified approvals; there is no unstated sixth signer-role rule.
4. Outsider signatures are ignored for quorum rather than rejected solely for being outsiders.
5. `AdministratorState`, not member roles, is the authoritative admin set.
6. All fourteen ordinary operations use the current admin threshold in #148.
7. Recovery-threshold authorization and special fork resolution remain #149.
8. Device-binding acceptance refers to device operation ownership/status because the current signed entry does not identify a signing device.
9. The predecessor root committed in `ValidatedGovernanceState` is local typed validation evidence and is not a new state-root component.
10. Existing `Reject` codes are sufficient; no new public error variant is needed.
11. The v2 core remains unused by the runtime, so no operational migration is required.

---

## 15. Open questions requiring confirmation before implementation

1. **Signer counting:** Does §7.4 count the entry signer toward `W`, or must every threshold participant create an explicit `GovernanceApproval`? This spec counts the signer once.
2. **Outsider signer:** May a non-admin author an entry that carries `W` valid old-admin approvals? This spec allows it because the stated rule is threshold-based.
3. **Predecessor API:** Does the unavailable decision record define a predecessor-state/cursor type? If so, use its exact fields instead of introducing different names, while retaining opacity and the two required facts (tip and committed root).
4. **Device binding:** Does §7.4 require entry signatures to name and validate an active signing device? If yes, #147's current envelope is insufficient and implementation must stop for a separately reviewed schema/version decision rather than infer device identity.
5. **Admin/member normalization:** Must `admin.set` also update member records/roles for added and removed admins? This spec avoids broadening #148 and uses `AdministratorState` exclusively, but a normative invariant may require a coordinated #147 correction.
6. **Admin survivability:** Must `member.revoke` or `device.revoke` reject transitions affecting current administrators or their last usable device? This is not derivable from the supplied issue and is not added here.
7. **Recovery config validity:** Is empty recovery (`threshold=0`, no keys) valid after genesis, and otherwise must `1 <= threshold <= keys.len()`? Confirm before strengthening `recovery.set`.
8. **No-op semantics:** Should already-applied set-like operations reject or remain idempotent? Preserve #147 behavior unless §7.4 explicitly says operation validity is stricter.
9. **Low-level API visibility:** May `apply_verified_entry` be deprecated or reduced to crate visibility in a later breaking release, or must it remain public indefinitely?

---

## 16. Implementation notes (post-landing)

#148 landed exactly as designed in §5–§6: `governance/log/authz.rs` adds the pure
`validate_governance_entry`/`validate_and_apply_governance_entry` pair over the
opaque `ValidatedGovernanceState`/`GovernanceTip` wrapper, `records.rs` adds
`VerifiedGovernanceEntry`/`verify_governance_entry` (with `verify_entry_full` kept
as a compatibility wrapper), and `state.rs` strengthens `apply_device_grant`/
`apply_device_revoke` with the §4.4 ownership/status rules. No frozen wire byte,
domain string, or successful state-root vector changed. The §15 open questions
were resolved as follows, each pinned by a test in `authz.rs` unless noted:

1. **OQ-1 (signer counting):** the entry signer counts toward `W` alongside
   approvers, as one distinct union (D5) — proven by
   `signer_also_approving_counts_once` (signer + self-approval still counts once)
   and the `prop_threshold_w_minus_one_rejects_w_accepts` property.

2. **OQ-2 (outsider signer):** allowed — an entry authored by a non-admin with `W`
   valid old-admin approvals is authorized
   (`outsider_signer_with_w_admin_approvals_is_authorized`), while outsider
   approvals alone never contribute to the threshold
   (`outsider_approvals_are_ignored_not_counted`).

3. **OQ-3 (predecessor API):** implemented exactly as proposed —
   `ValidatedGovernanceState` (opaque; constructible only via
   `validated_genesis_state` or a successful
   `validate_and_apply_governance_entry`) plus the `GovernanceTip::{Genesis,
   Entry { seq, id }}` cursor. No decision record surfaced different field names.

4. **OQ-4 (device binding):** resolved as anticipated — the current #147 entry
   envelope names only a principal signer, so #148 does not invent a
   signer-device check. "Device-binding validity" means the rule-3
   ownership/status validity of `device.grant`/`device.revoke` transitions
   (globally-unique active binding, no re-grant of an already-active or revoked
   device, revoke requires the claimed owner to hold an active device), covered
   by `device_grant_and_revoke_transitions_are_rule3_gated` and the
   `prop_device_binding_lifecycle_through_pipeline` property, plus the
   `state.rs` unit tests `apply_device_grant_rejects_absent_or_revoked_member`,
   `apply_device_grant_rejects_globally_duplicate_device_id`, and
   `apply_device_revoke_rejects_absent_wrong_owner_or_already_revoked`. A future
   two-key/device-signature protocol issue remains a prerequisite for any
   signer-device authorization rule.

5. **OQ-5 (admin/member normalization):** not broadened. Rule 4 reads
   `state.administrators` exclusively (never `members[..].role`); `admin.set`
   still does not synchronize member roles. Reconciling the two remains a
   separately-tracked #147 follow-up, not part of #148.

6. **OQ-6 (admin survivability):** not added. No rule rejects a `member.revoke`
   or `device.revoke` that would remove a current administrator's membership or
   last device; this policy is not derivable from the supplied issue text and is
   left for a future, explicitly-scoped change.

7. **OQ-7 (recovery config validity):** not strengthened. `recovery.set` keeps
   its pre-#148 apply semantics; #148 only adds the threshold/device rules the
   issue explicitly requested.

8. **OQ-8 (no-op semantics):** preserved. #148 did not change whether
   already-applied set-like operations reject or remain idempotent; existing
   #147 `apply` semantics are the sole authority for rule 3.

9. **OQ-9 (low-level API visibility):** `apply_verified_entry` remains public for
   source compatibility, now documented as **not an authorization boundary**
   (checks community/operation-validity/post-root only — rules 1's community
   check, 3, and 5, but never rule 2's chain link or rule 4's threshold).
   Normative callers must use `validate_governance_entry` /
   `validate_and_apply_governance_entry`; no visibility change was made in this
   issue.

### Verification

`cargo test -p iroh-rooms-v2-core --all-targets --all-features` covers the new
`authz.rs` unit and property tests, the corrected `tests/v2_governance_log_e2e.rs`
registry (each of the fourteen §7.3 operations, including a single `device.grant`
and single `device.revoke` row — the prior duplicate-`device.revoke`/missing-
`device.grant` table is fixed), and the existing `taxonomy.rs`,
`signed_records_golden.rs`, and `banned_dependencies.rs` suites, which remain
unaffected because no wire format, domain string, or dependency changed.

### Deferred scope (unchanged)

Fork detection (two quorum-valid entries sharing a predecessor) and
recovery-threshold signing remain #149; `fork.resolve` is authorized like any
other ordinary operation under the current admin threshold until #149
deliberately replaces that rule. Checkpoints/snapshots remain #150.
