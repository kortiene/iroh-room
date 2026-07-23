# Spec: v2 Crypto Core Crate

| | |
|---|---|
| **Issue** | #140 — `[EPIC] v2 crypto core crate (crates/iroh-rooms-v2-core/)` |
| **Labels** | `type/epic` `area/protocol` `priority/p1` `risk/medium` |
| **Parent / refs** | Product decision record; #134 §6–§9; child issues #146–#153; deferred tracker #162; `crates/iroh-rooms-core/` sans-IO pattern |
| **Owning crate** | `crates/iroh-rooms-v2-core/` (new workspace member) |
| **Status** | Landed. Implemented as `crates/iroh-rooms-v2-core/` (Phase A — `publish = false`, unused pure infrastructure; a `cargo tree` banned-dependency guard plus taxonomy and golden-vector tests live in `tests/`). The independent governance-state-machine/fork audit and child-issue #146–#153 closure remain orchestrator-tracked (§10 / issue #140 acceptance). |

> This document is the implementation plan for the pure v2 cryptographic core. It landed as **unused, isolated infrastructure**: a pure crate with no network, store, or async dependencies, never published and not re-exported from the SDK in this phase. The plan still chooses no deployment model and performs no git or GitHub actions; runtime/store/network crates may wire to it through separate, later issues (§12).

---

## 1. Summary

Create a new workspace member, `crates/iroh-rooms-v2-core/`, that contains only deterministic protocol logic for the v2 foundation:

- canonical CBOR encoding/decoding;
- BLAKE3-based identifiers, state roots, snapshot hashes, and Merkle-map hashes;
- Ed25519 signing and verification of every v2 signed record;
- governance log record validation, approval validation, deterministic governance-state folding, and authorization checks;
- fork/equivocation detection plus the signed `fork.resolve` record model;
- checkpoint and snapshot-hash validation;
- member projection and deterministic Merkle map construction/proofs;
- content event body validation for #134 §9.2;
- golden vectors for every signed record and every hash/root boundary.

The crate must be pure. It must not contain network transports, ALPN constants, router accept loops, tokio tasks, SQLite/store schema, replica receipts, publication certificates, migration tooling, or group encryption. Later phases may consume this crate from runtime/store/network crates, but this crate must not depend on them.

---

## 2. Repository context read

The current repo has a v1 sans-IO-ish core at `crates/iroh-rooms-core/`, but that crate also contains optional `store` and `sync` features. Its useful patterns for v2 are:

- `src/event/cbor.rs`: small deterministic-CBOR encoder/strict reader, rejecting indefinite values, tags, floats, duplicate keys, non-shortest ints, unsorted maps, non-text map keys, invalid UTF-8, and trailing bytes.
- `src/event/{ids,signed,wire,validate}.rs`: named BLAKE3 IDs, canonical signed bytes, Ed25519 signing over domain-separated payloads, and typed validation errors.
- `src/membership/fold.rs`: arrival-order-independent fold from validated events, ancestor-stable authorization, advisory fork/equivocation flags, and deterministic snapshots.
- `tests/golden_vectors.rs` and `tests/conformance/`: seed-derived golden keys, pinned canonical bytes, event IDs, room IDs, signatures, rejection taxonomy, and stateful conformance vectors.
- Root `Cargo.toml`: workspace lints forbid unsafe code; new crates are added as workspace members. Existing verification is `scripts/verify.sh` (`cargo fmt`, clippy with all features/all targets, workspace tests, SDK docs/examples).

The v2 issue is deliberately narrower than the current v1 core: it asks for a pure crypto/deterministic-algorithm crate, not a store/sync/runtime crate.

---

## 3. Scope

### 3.1 In scope

This epic owns the implementation surface for these child issues:

1. **#146 — #134 §6 identifiers + domain separation**
   - Newtype identifiers and byte/string presentation.
   - Versioned domain-separation constants for every hash/signature/Merkle boundary.
   - Collision-resistant derivation helpers and rejection of wrong-domain use.
2. **#147 — #134 §7 governance log entry/approval/state_root**
   - Canonical governance log entry body.
   - Approval record body.
   - Entry ID, approval ID, signed bytes, signature verification.
   - Deterministic state-root computation after applying entries.
3. **#148 — #134 §7.4 authorization rules**
   - Pure authorization engine over a supplied/folded governance state.
   - Default deny, typed failures, no wall-clock-only authorization.
4. **#149 — #134 §7.5 fork detection + `fork.resolve`**
   - Deterministic fork/equivocation detection.
   - `fork.resolve` signed record body, validation, and state effects.
5. **#150 — #134 §7.6 governance checkpoints + snapshot hash**
   - Checkpoint record shape.
   - Snapshot canonicalization and hash derivation.
   - Checkpoint validation against folded state.
6. **#151 — #134 §8.1–§8.2 member projection + Merkle map**
   - Deterministic member projection from governance state.
   - Sparse deterministic Merkle map, root, inclusion/exclusion proofs.
7. **#152 — #134 §9.2 content event body validation**
   - Strict validation of v2 content bodies and registered content kinds.
   - Body-only validation; no blob fetch, no stream transport, no encryption.
8. **#153 — Golden vectors for every signed record**
   - Byte-exact canonical CBOR, IDs, signatures, state roots, snapshot hashes, Merkle roots/proofs, acceptance/rejection outcomes.

### 3.2 Out of scope

Do not include:

- anything requiring `tokio::spawn`, async runtimes, `Router::accept`, iroh ALPN registration, iroh-gossip, iroh-blobs, or network handshakes;
- store schema, SQLite, replicas, publication certificates, replica receipts, receipt persistence, or migration tools;
- group encryption, key ratchets, payload encryption, or deployment-model commitments;
- CLI/SDK façade changes except later consumers adding dependencies in their own issues;
- automatic GitHub issue closure; the orchestrator handles issue status.

---

## 4. Key design decisions

### D1 — Make `iroh-rooms-v2-core` independent, not a wrapper around v1 core

The new crate should copy/adapt proven patterns from `iroh-rooms-core`, but it should not depend on `iroh-rooms-core` for production code. Rationale:

- v2 identifiers, record kinds, governance state roots, and Merkle roots are new protocol surfaces;
- depending on v1 risks accidental schema/version coupling;
- the acceptance criterion is a pure v2 foundation with maximum optionality.

Permitted dependencies should be minimal and protocol-oriented: `ed25519-dalek`, `blake3`, `hex`, `zeroize`/`getrandom` for key helpers if generation is exposed, and dev-only property/vector tooling. Avoid `tokio`, `iroh`, `iroh-blobs`, `iroh-gossip`, `rusqlite`, `serde` network formats, and general-purpose CBOR behavior that cannot enforce the deterministic profile.

### D2 — Canonical bytes are the trust boundary

Every signed v2 record must have:

1. a logical body struct;
2. canonical signed bytes (`CSB`) produced by the crate's deterministic-CBOR profile;
3. a domain-separated signing message;
4. an ID derived from the exact `CSB` bytes;
5. a wire/storage envelope preserving `CSB` verbatim.

Receivers must verify the exact bytes they received. They must never reserialize before signature verification except for canonicality checks after decoding.

### D3 — Domain separation must be complete and pinned by tests

Every cryptographic boundary must have an explicit context string. Candidate contexts, to be finalized by #146 and then frozen by vectors:

| Boundary | Candidate context |
|---|---|
| Governance entry signature | `iroh-rooms:v2:governance-entry:sign:v1` |
| Governance entry ID | `iroh-rooms:v2:governance-entry:id:v1` |
| Governance approval signature | `iroh-rooms:v2:governance-approval:sign:v1` |
| Governance approval ID | `iroh-rooms:v2:governance-approval:id:v1` |
| Content event signature | `iroh-rooms:v2:content-event:sign:v1` |
| Content event ID | `iroh-rooms:v2:content-event:id:v1` |
| Room/space ID derivation | `iroh-rooms:v2:room-id:v1` |
| State root | `iroh-rooms:v2:governance-state-root:v1` |
| Checkpoint signature | `iroh-rooms:v2:checkpoint:sign:v1` |
| Snapshot hash | `iroh-rooms:v2:snapshot-hash:v1` |
| Merkle empty node | `iroh-rooms:v2:merkle:empty:v1` |
| Merkle leaf | `iroh-rooms:v2:merkle:leaf:v1` |
| Merkle internal node | `iroh-rooms:v2:merkle:node:v1` |
| Merkle key | `iroh-rooms:v2:merkle:key:v1` |
| Fork resolution signature | `iroh-rooms:v2:fork-resolve:sign:v1` |

Open question OQ-1 tracks whether these exact strings match the July 2026 decision record/#134 text.

> **Update (#146 landed):** #134 §6.2 freezes a different, normative set — the eleven
> `iroh-room-v2/<kind>` strings (e.g. `iroh-room-v2/community`, `iroh-room-v2/governance-entry`,
> `iroh-room-v2/content-event`, `iroh-room-v2/merkle-node`, …). These live in
> `crates/iroh-rooms-v2-core/src/domain.rs` and drive v2 **identifier derivation**; the
> candidate strings in the table above survive only as private compatibility aliases for the
> already-frozen #153 signed-record signing contexts, pending a follow-up reconciliation pass.
> See `specs/v2-identifiers-domain-separation.md` §15.

### D4 — Governance state is a deterministic pure fold

The crate exposes a fold that consumes a set or sequence of validated governance entries and returns:

- accepted entries;
- rejected entries with typed reasons;
- unresolved forks/evidence;
- current governance state;
- current `state_root`;
- current member projection and `member_root`.

For identical validated input sets and identical fork-resolution records, every implementation must produce byte-identical roots and projections. Arrival order must not affect the final result.

### D5 — Authorization is data-in/data-out, not ambient runtime policy

The crate should expose pure functions like:

```rust
fn authorize_governance_entry(state: &GovernanceState, entry: &GovernanceEntry) -> Result<(), AuthzError>;
fn authorize_content_body(state: &GovernanceState, author: PrincipalId, body: &ContentEventBody) -> Result<(), AuthzError>;
```

These functions may inspect signed timestamps/epochs in records if #134 requires them, but must not read a local wall clock. Any caller that wants clock policy must pass it explicitly as data.

### D6 — Forks are detected and represented; resolution is explicit

Fork detection must not silently choose a winner. The core should:

- detect same-author conflicting governance branches according to #134 §7.5;
- carry both conflicting IDs as evidence;
- mark affected authorization state as unresolved/fail-closed;
- accept state mutation only through a valid, authorized `fork.resolve` record;
- include fork evidence and resolution status in state-root/snapshot hashing so peers cannot disagree silently.

### D7 — Merkle map is deterministic and independent of storage layout

Implement a deterministic sparse Merkle map over 256-bit keys:

- map key: `BLAKE3(MERKLE_KEY_CONTEXT || logical_key_bytes)`;
- leaf hash: `BLAKE3(MERKLE_LEAF_CONTEXT || key || value_hash)`;
- internal hash: `BLAKE3(MERKLE_NODE_CONTEXT || left_hash || right_hash)`;
- empty hash: depth-specific `BLAKE3(MERKLE_EMPTY_CONTEXT || depth_be)` or one explicitly specified empty-root recurrence;
- proof format: canonical CBOR containing the searched key, optional leaf, sibling path, and root.

Pin empty roots, one-leaf root, two-leaf root, inclusion proof, and exclusion proof in vectors.

### D8 — Content body validation is strict and closed

Carry forward the v1 rule: unknown keys/kinds are rejected, never ignored. Use the v2 content-kind registry from #134 §9.2; if #134's final registry is unavailable in the codebase, use `specs/content-and-moderation-event-schemas.md` as the nearest local planning input and mark any mismatch as a blocker before implementation.

---

## 5. Target crate structure

Recommended layout:

```text
crates/iroh-rooms-v2-core/
  Cargo.toml
  src/
    lib.rs
    error.rs
    cbor.rs
    ids.rs
    domain.rs
    keys.rs
    signed.rs
    governance/
      mod.rs
      model.rs
      entry.rs
      approval.rs
      authz.rs
      fold.rs
      fork.rs
      checkpoint.rs
      state_root.rs
    member/
      mod.rs
      projection.rs
      merkle.rs
      proof.rs
    content/
      mod.rs
      body.rs
      registry.rs
      validate.rs
  tests/
    canonical_cbor.rs
    identifiers.rs
    signed_records_golden.rs
    governance_fold.rs
    governance_authz.rs
    fork_detection.rs
    checkpoints.rs
    member_merkle_map.rs
    content_body_validation.rs
    taxonomy.rs
    vectors/
      v2-golden-vectors.cbor.hex
      v2-golden-vectors.json
```

Notes:

- `lib.rs` should re-export stable core types only. Keep constructors and validation entry points documented.
- `error.rs` should contain public typed errors/rejections; avoid stringly-typed validation outcomes.
- Tests should be deterministic, network-free, and entropy-free except tests that explicitly exercise key generation.
- No module should import `tokio`, `iroh`, `iroh_blobs`, `iroh_gossip`, `rusqlite`, or runtime/store crates.

---

## 6. Public API shape

The exact Rust names can vary, but the crate should expose these concepts.

### 6.1 IDs and hashes

```rust
struct RoomId([u8; 32]);
struct GovernanceEntryId([u8; 32]);
struct ApprovalId([u8; 32]);
struct ContentEventId([u8; 32]);
struct SnapshotHash([u8; 32]);
struct StateRoot([u8; 32]);
struct MerkleRoot([u8; 32]);
struct MemberId([u8; 32]);
struct DeviceId([u8; 32]);
```

Requirements:

- lowercase `blake3:<64-hex>` display for BLAKE3 hash IDs, unless #134 specifies a different prefix;
- strict parsing with wrong-prefix/wrong-length/bad-hex errors;
- raw byte access for canonical CBOR;
- `Ord` over raw bytes for deterministic sorting.

### 6.2 Canonical CBOR

Expose a closed `CborValue` profile and encode/decode helpers:

```rust
fn encode(value: &CborValue) -> Vec<u8>;
fn decode_canonical(bytes: &[u8]) -> Result<CborValue, CborError>;
```

Validation profile:

- unsigned integers only;
- byte strings, UTF-8 text strings, arrays, text-keyed maps;
- definite lengths only;
- shortest integer encodings only;
- no duplicate map keys;
- maps sorted by encoded key bytes;
- no tags, floats, simple values, negative integers, indefinite strings/arrays/maps, trailing bytes;
- bounded nesting depth and bounded declared lengths.

### 6.3 Signed records

Use a generic pattern internally, with concrete public types:

```rust
struct SignedGovernanceEntry { id, signed, sig, signer }
struct SignedApproval { id, signed, sig, signer }
struct SignedCheckpoint { id, signed, sig, signer }
struct SignedForkResolution { id, signed, sig, signer }
struct SignedContentEvent { id, signed, sig, signer }
```

Each type should support:

- build from typed body + signing key;
- parse from wire bytes;
- verify canonicality, ID, signature, domain, and body-specific validation;
- expose decoded body only after verification.

### 6.4 Governance fold

```rust
struct GovernanceState { ... }
struct GovernanceFold { ... }
struct FoldOutcome { accepted, rejected, unresolved_forks, state_root }
```

Required behavior:

- deterministic over identical input;
- idempotent duplicate handling;
- clear separation of stateless signed-record validation and stateful authorization/fold validation;
- no persistence or network fetching;
- unresolved fork state is explicit and fail-closed for affected subjects.

### 6.5 Member projection and Merkle proofs

```rust
struct MemberProjection { members, root }
struct MemberLeaf { member_id, status, roles, device_keys, governance_cursor, ... }
struct MerkleMap { root, leaves }
struct MerkleProof { key, leaf, siblings }
```

Requirements:

- projection is computed only from accepted governance state;
- canonical leaf encoding is pinned by tests;
- member ordering is deterministic by member ID raw bytes;
- inclusion and exclusion proofs verify without access to full state;
- root changes on any semantically relevant member-state change.

---

## 7. Error model and observability

Because this crate is pure, it should not log directly. It should return structured errors that callers can log/audit.

Recommended top-level rejection categories:

| Error | Meaning |
|---|---|
| `NonCanonicalEncoding` | CBOR malformed or outside deterministic profile |
| `UnknownVersion` | unsupported schema/protocol version |
| `UnknownRecordKind` | record kind not in closed registry |
| `UnknownContentKind` | content body kind not in v2 registry |
| `InvalidContent` | missing/wrong/extra/out-of-bound body field |
| `IdMismatch` | envelope ID does not match domain-separated hash of signed bytes |
| `BadSignature` | Ed25519 verification failed under signer/device key |
| `WrongDomain` | bytes valid under another signed-record domain but not this one |
| `Duplicate` | same ID already applied; idempotent non-error outcome |
| `MissingDependency` | parent/entry/checkpoint dependency not supplied to pure fold |
| `InsufficientAuthorization` | signer/approval set cannot authorize action |
| `InvalidApproval` | approval references wrong entry/root, duplicate signer, bad signature, or stale state |
| `ForkDetected` | conflicting branches/evidence detected |
| `UnresolvedFork` | operation depends on unresolved fork state and must fail closed |
| `InvalidForkResolution` | malformed or unauthorized `fork.resolve` |
| `StateRootMismatch` | supplied root differs from recomputed state root |
| `SnapshotHashMismatch` | checkpoint/snapshot hash differs from recomputed hash |
| `InvalidMerkleProof` | proof does not verify against root |

Also expose machine-readable `.code()` strings so downstream CLI/runtime layers can map failures without parsing display text.

---

## 8. Implementation steps

### Step 1 — Create the crate skeleton

1. Add `crates/iroh-rooms-v2-core/` with `Cargo.toml`, `src/lib.rs`, and empty module files.
2. Add it to the root workspace members.
3. Set `publish = false` in the package manifest.
4. Use workspace edition, license, repository, rust-version, and lints.
5. Start with no default features unless a `std`/`alloc` split is explicitly needed later.
6. Add only approved pure dependencies.
7. Add a CI guard test or script check that `cargo tree -p iroh-rooms-v2-core` contains none of: `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, `rusqlite`.

### Step 2 — Port/adapt deterministic CBOR

1. Adapt the v1 strict CBOR module into `src/cbor.rs`.
2. Add tests for every rejected CBOR class:
   - indefinite lengths;
   - duplicate keys;
   - unsorted keys;
   - non-shortest integers;
   - tags/floats/simple values;
   - non-text map keys;
   - invalid UTF-8;
   - trailing bytes;
   - excessive nesting.
3. Add property tests: `decode_canonical(encode(value)) == value` and `encode(decode_canonical(bytes)) == bytes` for generated in-profile values.

### Step 3 — Implement identifiers and domain separation (#146)

1. Define domain constants in one module.
2. Implement ID/hash newtypes and parsing/display.
3. Implement domain-separated BLAKE3 helpers that require an explicit domain enum/constant.
4. Add compile-time tests for context string bytes and lengths.
5. Add golden tests for every identifier derivation in #146.

### Step 4 — Implement signing primitives

1. Define identity/device/signing-key wrappers if v2 keeps the v1 two-key model; otherwise match #134's final key model.
2. Implement Ed25519 signing over `DOMAIN || canonical_signed_bytes`.
3. Implement verification under the correct signer/device field only.
4. Add negative tests for verifying under the wrong key and wrong domain.
5. Keep key generation helpers optional; golden vectors should use seed-derived keys.

### Step 5 — Implement governance record model (#147)

1. Define typed governance entry body enum and canonical field order.
2. Define approval body enum/struct referencing entry ID, proposed state root, signer, and any approval scope required by #134.
3. Define state-root input as a canonical snapshot of accepted governance state, not process memory layout.
4. Implement `build_*`, `to_csb`, `id`, `sign`, `verify`, and `decode_verified` flows.
5. Add tests for:
   - valid entry;
   - invalid canonical bytes;
   - ID mismatch;
   - bad signature;
   - approval references wrong entry/root;
   - state-root mismatch.

### Step 6 — Implement authorization rules (#148)

1. Encode #134 §7.4 as explicit match arms over governance action kind.
2. Default deny every unknown/unsupported action.
3. Check signer role, required approvals, threshold/quorum policy, subject status, and fork state.
4. Ensure authorization uses only supplied/folded signed state, not wall clocks or external stores.
5. Add table-driven tests for every allowed and denied action.
6. Add a taxonomy-completeness test so every authz error appears in at least one vector.

### Step 7 — Implement governance fold and state roots (#147/#148)

1. Build a pure `GovernanceFold` that accepts verified entries and approvals.
2. Treat duplicates idempotently.
3. Apply entries only when dependencies/approvals are present and valid.
4. Compute `state_root` after each accepted state transition.
5. Keep rejected entries out of the state root, but make unresolved fork evidence part of the state representation if #134 says fail-closed state must be hash-visible.
6. Add order-independence tests by shuffling the same input set.
7. Add restart/rebuild-equivalence tests by folding from a serialized vector list.

### Step 8 — Implement fork detection and `fork.resolve` (#149)

1. Define fork evidence: conflicting entry IDs, signer, conflicting parent/tip/sequence fields, and affected subjects/scopes.
2. Detect same-author mutually incompatible governance branches according to #134 §7.5.
3. Return `ForkDetected` and mark affected decisions fail-closed until resolution.
4. Define `ForkResolutionBody` with:
   - fork evidence IDs;
   - resolution action (`accept`, `reject`, `supersede`, or exact #134 enum);
   - resulting state root;
   - required approvals.
5. Validate that a `fork.resolve` record references real unresolved evidence and is authorized.
6. Add vectors for:
   - no fork;
   - admin/governance signer fork;
   - segregated fork evidence received later;
   - unauthorized resolution rejected;
   - authorized resolution updates state deterministically;
   - same input plus resolution converges across shuffled arrival order.

### Step 9 — Implement checkpoints and snapshot hash (#150)

1. Define canonical snapshot representation:
   - protocol version;
   - room ID;
   - accepted governance tip(s);
   - state root;
   - member Merkle root;
   - unresolved fork commitments;
   - checkpoint sequence/epoch if #134 specifies one.
2. Define `snapshot_hash = BLAKE3(SNAPSHOT_CONTEXT || canonical_snapshot_bytes)`.
3. Define signed checkpoint body and verification.
4. Validate checkpoint roots by recomputing from supplied state.
5. Add tests for snapshot hash stability, changed member leaf changes root/hash, wrong root rejected, and old checkpoint replay behavior.

### Step 10 — Implement member projection and Merkle map (#151)

1. Define member statuses, role/capability representation, device bindings, and any governance cursor fields from #134 §8.1.
2. Project members from folded governance state in deterministic member-ID order.
3. Encode each `MemberLeaf` as canonical CBOR.
4. Implement sparse Merkle map root and proof generation/verification.
5. Add golden roots/proofs for:
   - empty map;
   - one member;
   - two members with divergent key prefixes;
   - member removal/status change;
   - inclusion proof;
   - exclusion proof;
   - malformed proof rejection.

### Step 11 — Implement content event body validation (#152)

1. Define `ContentEventBody` according to #134 §9.2. If field names are unavailable, block implementation until #134 text is available; do not guess wire names in code.
2. Implement a closed content-kind registry.
3. For each registered kind, validate:
   - exact key set;
   - required fields;
   - field types;
   - byte-length caps;
   - enum values;
   - sender/body cross-field invariants that are stateless.
4. Return stateful checks (e.g. target event exists, author owns edited event, stream exists) as deferred validation requirements if #134 separates stateless and stateful validation.
5. Add invalid corpus for unknown kind, unknown key, missing key, wrong type, over cap, bad enum, empty disallowed string, bad target length.

### Step 12 — Golden vectors for every signed record (#153)

1. Establish deterministic fixture keys from byte-repeated seeds or explicitly listed seed hex.
2. For every signed record, pin:
   - logical body fixture;
   - canonical signed bytes hex;
   - domain context;
   - ID;
   - signature;
   - decoded body;
   - expected verification result.
3. Include all record families:
   - governance entry;
   - governance approval;
   - fork resolution;
   - governance checkpoint;
   - content event;
   - member projection leaf;
   - Merkle proof fixtures;
   - state root and snapshot hash fixtures.
4. Store vectors under `tests/vectors/` in a reviewable format and assert the Rust implementation exactly matches them.
5. Add a doc table mapping #134 sections and child issues to vector names.

### Step 13 — Audit and documentation readiness

1. Document the crate-level invariants in `src/lib.rs`.
2. Add `README` only if project convention requires crate-local docs; otherwise keep documentation in this spec and Rust docs.
3. Prepare an audit packet after implementation:
   - this spec;
   - final #146–#153 specs/issues;
   - golden vector files;
   - `cargo tree` banned-dependency proof;
   - conformance test output;
   - state-machine/fork-handling review notes.
4. Acceptance requires independent audit of governance state machine and fork handling before #140 is considered done.

---

## 9. Test strategy

### Unit tests

- Canonical CBOR encoder/decoder edge cases.
- ID parsing and display.
- Domain constants and wrong-domain failures.
- Per-record signing/verification.
- Content body parser positive/negative cases.
- Merkle empty/leaf/internal hash functions.

### Integration/conformance tests

- Golden signed-record vectors.
- Governance fold order independence.
- Approval threshold/authorization matrix.
- Fork detection and resolution matrix.
- Checkpoint/snapshot recomputation.
- Member projection + Merkle proof verification.
- Taxonomy completeness: every public rejection code is covered.

### CI commands

Expected verification after implementation:

```bash
cargo fmt --all --check
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo tree -p iroh-rooms-v2-core
scripts/verify.sh
```

The `cargo tree` result must be inspected or machine-checked for banned dependencies.

---

## 10. Acceptance criteria

- [ ] `crates/iroh-rooms-v2-core/` exists and is a root workspace member.
- [ ] `crates/iroh-rooms-v2-core/Cargo.toml` has `publish = false`.
- [ ] The crate builds cleanly under workspace CI.
- [ ] The crate has no dependency on `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, `rusqlite`, or runtime/store crates.
- [x] #146 identifiers/domain separation implemented and pinned by vectors (eleven frozen `iroh-room-v2/...` domain strings; `CommunityId`/`GovernanceId`/`StreamId`/`EventId`/`CheckpointId`/`ReplicaId` newtypes with typed derivation helpers; golden vectors in `tests/identifiers.rs` + `tests/golden/v2-identifiers.json`; see `specs/v2-identifiers-domain-separation.md` §15).
- [ ] #147 governance entry/approval/state-root implemented and pinned by vectors.
- [ ] #148 authorization rules implemented with exhaustive allow/deny tests.
- [ ] #149 fork detection and `fork.resolve` implemented with fail-closed unresolved-fork behavior.
- [ ] #150 checkpoints and snapshot hashes implemented and pinned by vectors.
- [ ] #151 member projection and Merkle map/proofs implemented and pinned by vectors.
- [ ] #152 content event body validation implemented with strict unknown-kind/key rejection.
- [ ] #153 golden vectors exist for every signed record and every hash/root boundary.
- [ ] Taxonomy completeness test covers every public rejection/error code.
- [ ] Independent audit of governance state machine and fork handling is complete and recorded.
- [ ] All child issues #146–#153 are closed by the orchestrator after implementation/audit, not by this planning phase.

---

## 11. Security, privacy, reliability, and performance notes

### Security

- Domain separation is mandatory for every signature/hash/root boundary.
- Golden fixtures must use deterministic non-secret seeds only; never commit real keys.
- Strict canonical CBOR prevents signature malleability and parser differentials.
- Unknown kinds/keys must reject, not ignore.
- Authorization must fail closed under unresolved forks and unknown governance state.
- Snapshot hashes and state roots must commit to all state that affects authorization.

### Privacy

- The crate stores no data and performs no IO.
- Test vectors should avoid real user names, network addresses, or secrets.
- Public display helpers must not expose private key material.

### Reliability

- Pure folds and roots must be deterministic over identical inputs.
- Duplicates are idempotent.
- Missing dependencies are explicit outcomes, not panics.
- Rebuild from vector files must reproduce roots exactly.

### Performance

- Bound CBOR nesting and declared lengths.
- Avoid `O(n^2)` projection/fold behavior where straightforward maps/sets suffice.
- Merkle map proof generation should be `O(256)` for sparse 256-bit keys; projection build should be `O(n log n)` or better.
- Avoid unbounded allocation based on attacker-supplied lengths.

---

## 12. Rollout and rollback plan

### Rollout

1. Land the crate as unused pure infrastructure.
2. Keep it `publish = false` and not re-exported from the SDK initially.
3. Add only deterministic tests and dependency guards.
4. After audit and child issue closure, later phases may wire store/network/runtime crates to this crate through separate issues.

### Rollback

Because no production runtime consumes the crate at initial landing, rollback is simple: remove the workspace member and crate directory in a follow-up orchestrated change. No user data migration or network compatibility rollback is involved.

---

## 13. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| #134 details are not fully present in this checkout | Implementers may guess wire fields | Block code on missing normative text; keep guessed names only in this spec as candidates |
| Domain string drift after vectors freeze | Incompatible signatures/roots | Centralize contexts and pin exact bytes in #146/#153 vectors |
| Merkle-map ambiguity | Different implementations compute different roots | Specify empty/leaf/node/key hashing and pin roots/proofs |
| Governance authorization ambiguity | Safety-critical divergence | Table-driven #148 tests plus independent audit |
| Fork resolution accidentally picks winners | Unsafe convergence claims | Represent forks explicitly and require signed `fork.resolve` |
| Banned dependencies creep in transitively | Violates pure-core acceptance | Add `cargo tree` guard and review dependencies in CI |
| Over-reuse of v1 types | v2 inherits v1 schema assumptions | Keep crate independent; copy patterns, not protocol structs |
| Golden vectors generated only by implementation | Bugs become fixtures | Require independent reproduction before vectors are declared frozen |

---

## 14. Assumptions

1. The July 2026 decision record requires a new pure crate rather than extending `iroh-rooms-core`.
2. BLAKE3-256 and Ed25519 remain the v2 primitives unless #134/#146 says otherwise.
3. Deterministic CBOR remains the canonical format for signed bytes, roots, snapshots, and proofs.
4. `specs/content-and-moderation-event-schemas.md` is a useful local planning input for v2 content body validation, but #134 §9.2 is the final authority.
5. The crate may expose key generation helpers if they stay pure, but all vectors use deterministic seed-derived keys.
6. Store/network layers will pass complete record sets or dependencies into this crate; this crate will not fetch missing records.

---

## 15. Open questions

- **OQ-1:** ~~What are the exact domain-separation strings from #134/#146? The table in D3 is a candidate and must be reconciled before code lands.~~ **Resolved by #146.** #134 §6.2 freezes eleven `iroh-room-v2/<kind>` strings (see D3 update above and `specs/v2-identifiers-domain-separation.md`); the D3 candidate strings remain only as legacy compatibility aliases.
- **OQ-2:** What is the exact v2 key model: v1-style identity key plus device key, or a changed principal/device model?
- **OQ-3:** What exact governance actions and approval thresholds does #134 §7 define?
- **OQ-4:** Does `state_root` commit to unresolved fork evidence directly, or only to accepted/resolved governance state plus a separate fork set?
- **OQ-5:** What are the exact `fork.resolve` action enum values and authorization requirements?
- **OQ-6:** What fields are mandatory in governance checkpoints, and is checkpoint sequence global, per-branch, or per-author?
- **OQ-7:** Does #134 require a specific Merkle map construction, or may the implementation use the sparse BLAKE3 map described here?
- **OQ-8:** What is the authoritative #134 §9.2 content-kind/body registry if it differs from `specs/content-and-moderation-event-schemas.md`?
- **OQ-9:** What artifact should record the independent audit: a doc under `docs/audits/`, a spec appendix, or an orchestrator-managed issue attachment?
