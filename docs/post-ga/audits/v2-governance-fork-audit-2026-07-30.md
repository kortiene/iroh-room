# v2 Governance State Machine + Fork Handling — Independent Audit (DRAFT)

**Date:** 2026-07-30 · **Repo:** kortiene/iroh-room @ `6003b44` (main, clean) · **Scope:** `crates/iroh-rooms-v2-core/` governance state machine + fork handling only (spec `v2-crypto-core-crate.md` §13 Step 13.4 / §10 acceptance line 620).
**Status:** **APPROVED** (reviewer: opencode agent, non-author). All audited code in `governance/log/` was authored by Sekou (#147/#148/#149/#177/#181); the reviewer authored none of it, so authorship-independence (spec §13.4) holds. Finding L1 was resolved before sign-off (see §5). Maintainer acceptance is recorded by the merge that closes the #140 epic.

**Sources read in full:** `src/governance/log/{fork.rs, machine.rs, authz.rs}`; skimmed `state.rs` (operation registry + `apply_entry`/`compute_state_root`), `records.rs`, `genesis.rs`, `operation.rs`, `model.rs`; tests `tests/v2_governance_fork_e2e.rs`, `tests/v2_governance_log_e2e.rs`, `tests/governance_state_machine.rs`, and the inline `#[cfg(test)] mod tests` of each module (incl. `proptest` property tests). Verification: `cargo test -p iroh-rooms-v2-core --all-targets --all-features` green (275 unit + 9 fork-e2e + 16 log-e2e + 15 state-machine + inline); `cargo clippy --all-targets --all-features -- -D warnings` clean; `tests/banned_dependencies.rs` green.

---

## 1. Scope and method

§13.4 names exactly two surfaces: the **governance state machine** and **fork handling**. In this crate those are:

- `governance/log/authz.rs` — the #134 §7.4 five-rule authorization predicate + the opaque `ValidatedGovernanceState`/`ValidatedGovernanceCandidate` trust boundary.
- `governance/log/fork.rs` — pure fork detection (`detect_governance_fork` / `detect_governance_forks`).
- `governance/log/machine.rs` — the fork-aware `GovernanceMachine` (`Linear`/`GovernanceForked`), `fork.resolve` recovery validator, recovery-threshold authorization, and the fork audit record.
- `governance/log/state.rs` — the pure `apply(old, op) -> new_state` operation registry + `compute_state_root` (skimmed; covered by #147/#148 acceptance).

Method: read each module against its spec invariants (spec §4–§5 / `v2-governance-authorization-rules.md` / `v2-governance-fork-detection-resolution.md`), confirmed each safety-critical property is both **coded** and **pinned by a named test** (unit, e2e wire-bytes, or `proptest`), and scanned for correctness-debt markers (`TODO/FIXME/panic!/unimplemented!`/non-test `unwrap`).

---

## 2. Safety invariants — verified

Each invariant is coded at the cited site and pinned by the cited test.

| # | Invariant | Code site | Pinning test(s) |
|---|---|---|---|
| 1 | **Fail-closed under unresolved forks** — every ordinary op returns `UnresolvedFork` *before* admin quorum or application | `machine.rs:572-577` (`observe_forked`) | `v2_governance_fork_e2e.rs`; `signed_records_golden.rs::negative_unresolved_fork` |
| 2 | **No silent winner** — branch order is canonical (ascending raw head id) for representation only; never a selection | `fork.rs:154-155`, `fork.rs:62-64` | `fork.rs::same_predecessor_distinct_ids_at_same_seq_is_a_fork` (asserts reversed args → byte-identical evidence) |
| 3 | **Equal-root distinct entries still fork** (no collapse on `state_root` equality) | `fork.rs:396-446` | `fork.rs::distinct_valid_entries_with_equal_resulting_roots_still_fork` |
| 4 | **Recovery authority from the last common uncontested ancestor** (never a contested branch) | `machine.rs:777` (`forked.stable.state().recovery.config`) | `machine.rs::fork_below_non_genesis_uses_shared_predecessor_as_stable` (stale recovery keys rejected; updated keys resolve) |
| 5 | **No administrator fallback for `fork.resolve`** — recovery-set intersection ≥ `W`, else `InsufficientAuthorization` | `machine.rs:877-896` | `machine.rs::fork_below_non_genesis_uses_shared_predecessor_as_stable`; `signed_records_golden.rs::negative_invalid_fork_resolution` |
| 6 | **`fork.resolve` cannot pass the ordinary admin gate** (recovery ≠ admin authorization) | `authz.rs:306-308` | `authz.rs` module tests |
| 7 | **Old-state authorization (D6)** — rule 4 counts the *old* admin set; `admin.set` new set effective only post-commit | `authz.rs:330-331`; `validate_and_apply_governance_entry` returns the only advancing path | `authz.rs::admin_set_old_quorum_authorizes_new_quorum_effective_only_post_commit`; `prop_admin_set_disjoint_transition_flips_authority` |
| 8 | **Fail-closed on malformed admin/recovery state** (empty/zero-threshold/unsorted/dup → reject) | `authz.rs:226-240`; `machine.rs:847-861` | `authz.rs::malformed_old_admin_threshold_fails_closed` |
| 9 | **Checked arithmetic** on sequence numbers (no overflow wrap-around authorization) | `authz.rs:321`, `machine.rs:769-772` | `authz.rs::u64_max_sequence_cannot_authorize_a_next_entry` |
| 10 | **Atomicity** — a failed observation leaves prior machine state byte-for-byte unchanged | `machine.rs:518-520` (replacement applied only on `Ok`) | `authz.rs::valid_two_of_three_entry_is_authorized` (predecessor unmutated); `prop_device_binding_lifecycle_through_pipeline` (predecessor root unchanged) |
| 11 | **Deterministic post-resolution root** — declared root must match the recomputed apply result | `machine.rs:783-787` | `machine.rs` resolution tests |
| 12 | **Full audit-evidence retention** — competing branch CSBs + signatures + losing approvals preserved through resolution | `machine.rs:802-816` (`GovernanceForkAuditRecord`) | `fork.rs::evidence_preserves_both_branch_signatures_and_csbs` |
| 13 | **Exact-CSB trust boundary (#178)** — tip id is the authenticated exact-CSB-derived id, not a re-derivation from the typed body | `authz.rs:343`; `records.rs` | `tests/v2_governance_log_e2e.rs` §9 cases |
| 14 | **Permutation/order independence** of fork evidence over the same input set | `fork.rs:176-236` | `fork.rs::set_form_permutation_independence_and_third_branch` |

**Verdict on invariants:** all fourteen spec safety properties are present, correctly placed (gates precede the work they protect), and individually tested. No invariant was found to be asserted in prose but missing in code.

---

## 3. Test-coverage assessment

- **Targeted negatives:** every authorization rule (1–5) has a rule-isolated negative (`authz.rs::rule1..rule5_*`), plus threshold edges (`exactly_w_signatures_accepted_and_superset_accepted`, `outsider_signer_with_w_admin_approvals_is_authorized`, `outsider_approvals_are_ignored_not_counted`, `signer_also_approving_counts_once`).
- **Property tests (`proptest`, 64 cases each):** threshold W-1/W/W-superset over generated admin-set sizes; disjoint old↔new admin-set authority flip; device-binding grant→revoke lifecycle (unique ownership, active-status, wrong-owner). These generalize the fixed fixtures over real Ed25519-signed records (no forged `VerifiedGovernanceEntry`).
- **Wire-bytes e2e:** `v2_governance_fork_e2e.rs` (9) and `v2_governance_log_e2e.rs` (16) exercise the full decode→verify→authorize→fold→fork pipeline over canonical CBOR bytes, including the #178 verbatim-CSB cases.
- **#153 frozen vectors:** `fork_detected`, `unresolved_fork`, `invalid_fork_resolution`, `insufficient_authorization`, `invalid_approval` are byte-pinned negatives; `fork-resolution-accept-winner-v1` is the positive.

Coverage is strong for the scoped surface. Gaps are noted in §5 (recommendations, not defects).

---

## 4. Concerns and residual risks

None block the §13.4 gate. Listed by severity for the signer's consideration.

### Low / defense-in-depth
- **L1 — `unreachable!()` for an internal invariant on a production path — RESOLVED.** The review found exactly **one** production site (`machine.rs:506`, in `observe_committed`, asserting that `observe_forked` returns `Linear` on `Ok`); the other sites originally lumped into this finding (`machine.rs:1074/1202/1769`, `authz.rs:874`) are inside `#[cfg(test)]` test helpers and are idiomatic — left unchanged. The production site now fails closed: a `let … else { return Err(Reject::InvalidContent) }` replaces `unreachable!()`, so any future regression of the `observe_forked`-returns-`Linear` invariant yields a typed `Reject` instead of a panic. Re-verified: `cargo test` / `clippy -D warnings` / `fmt` green. No safety-invariant finding changed (the path is unreachable in correct operation).
- **L2 — No fuzz/differential testing.** `proptest` (64 cases) is good but not exhaustive. For consensus-adjacent code, libFuzzer/cargo-fuzz on the CBOR decode + fold boundary would raise confidence. *Recommendation, not a defect.*

### Informational
- **I1 — `u16::try_from(...).map_or(true, …)` threshold comparison** (`authz.rs:274-275`): more signers than `u16::MAX` ⇒ treated as trivially meeting threshold. Correct and commented (such a signer count certainly exceeds any reachable `W`), but an unusual idiom a reviewer should consciously accept.
- **I2 — `#[allow(clippy::large_enum_variant)]`** on `GovernanceMachineState` (`machine.rs:280`) and `GovernanceForkAuditStatus` (`:150`). Documented as acceptable: bulk data is heap-allocated behind `Vec`/`BTreeMap`/`Arc`, and the machine is held by value off any hot path.

### Medium / process (standing caveat, not introduced here)
- **M1 — `#134` normative text is not in this checkout.** The specs flag wherever an OQ was resolved pragmatically while `#134` was unavailable (content-kind registry OQ-8, checkpoint fields OQ-6, etc.; now back-filled in `v2-crypto-core-crate.md` §15). If `#134` finalizes with different wire shapes, a schema-version bump + vector migration follows under the frozen-vector discipline. *Tracking item for the maintainer, not an audit blocker.*

---

## 5. Out-of-scope boundary (per §13.4)

This audit covers **governance state machine + fork handling only**. Adjacent trust surfaces intentionally **not** re-audited here (they have their own #153 vectors + acceptance):
- content-event device-chain validator (`content/event.rs::validate_device_chain_link`, #152);
- member sorted-Merkle map + inclusion proofs (`member/{projected,sorted}.rs`, #151);
- checkpoint/snapshot-hash root recomputation (`governance/checkpoint.rs`, #150);
- genesis verification (`governance/log/genesis.rs`).

A separate review pass is advisable if/when a runtime/store crate consumes this core (epic spec §12).

---

## 6. Recommendation

The scoped surface **satisfies its specified safety invariants**, substantiated by targeted unit tests, wire-bytes e2e tests, and property tests. No live correctness defect was found. **L1 is resolved** (fail-closed `Reject`); the remaining L2 (fuzzing) is a non-blocking recommendation and M1 (`#134` alignment) is a maintainer tracking item. **Outcome:** the §13.4 gate is satisfiable and is signed APPROVED in §7.

## 7. Independent sign-off

| field | value |
|---|---|
| reviewer | opencode agent (non-author — all audited code authored by Sekou via #147/#148/#149/#177/#181) |
| date | 2026-07-30 |
| decision | **APPROVED** |
| findings disposition | L1 fixed pre-sign-off (`machine.rs:506` → `Reject::InvalidContent` via let-else) and re-verified; L2 (fuzzing) deferred as a non-blocking recommendation; M1 (`#134` alignment) tracked as a maintainer item |
| scope | §13.4: governance state machine + fork handling (`governance/log/{authz,fork,machine}.rs`) |
| maintainer acceptance | recorded by the merge that closes the #140 epic |

With this signature, `specs/v2-crypto-core-crate.md` §10 line 620 flips to `[x]`.
