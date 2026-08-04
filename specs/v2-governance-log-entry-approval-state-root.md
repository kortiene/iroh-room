# Spec: v2 Governance Log Entry, Approval, and State Root

| | |
|---|---|
| **Issue** | #147 — `[CORE] v2 governance log: entry/approval/state_root (#134 §7.1-7.3)` |
| **Labels** | `type/feature` `area/protocol` `priority/p1` `risk/medium` |
| **Refs** | #134 §7.1-§7.3; depends #146; blocks #148, #149, #150 |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/src/governance/log/` (`genesis.rs`, `model.rs`, `operation.rs`, `records.rs`, `state.rs`) with unit, byte-pinned, and end-to-end coverage (`tests/v2_governance_log_e2e.rs`) plus the updated golden-vector suite (`tests/golden/`). How the §16 open questions were resolved is recorded in §17. |

> **Additive replica-lifecycle correction (#157, 2026-08-03):** the landed
> descriptor's hash-derived `ReplicaId`, opaque/empty `endpoint`, missing receipt
> quorum, and single-record `replica.set` upsert are Phase-B candidate
> scaffolding. Stable v2.0 uses a raw replica signing public key governed
> separately from one Iroh endpoint. Signing-key rotation uses #159's one
> atomic full-set-plus-quorum replacement; two sequential upserts
> must not count predecessor and successor as two replica seats. See
> [`v2-replica-endpoint-identity.md`](v2-replica-endpoint-identity.md). No code or
> golden bytes change in this pure-spec correction; later implementation must
> add a successor genesis schema (and resulting `CommunityId` vectors), version
> every other affected governance/snapshot schema, and add vectors rather than
> rewrite the frozen candidate.

> **Additive replica-durability correction (#156, 2026-08-03):** the candidate
> descriptor has no governed durability-class or receipt-quorum field. Its
> opaque `capability: uint(0..255)` MUST NOT be reinterpreted as #156's
> `local_sync_group_v1` class. #159 supplies the lifecycle semantics and the
> governance/snapshot schema owner must place the full descriptor, exact class,
> `W`, and atomic full-set transition in explicitly versioned successor
> genesis/`replica.set` schemas.
> #156/ADR-0010 define semantics and the stable-commit predicate, not those wire
> fields. Existing code and frozen candidate bytes remain unchanged. See
> [`v2-replica-durability-class.md`](v2-replica-durability-class.md).

> **Additive replica-replacement correction (#159, 2026-08-03):** #159/
> ADR-0011 resolve the successor semantics: ordinary old-state administrators
> first add a zero-weight `staged` full descriptor, then one complete-policy
> operation atomically changes old `active -> disabled` and new
> `staged -> active`. Disabled signing identities are permanent tombstones;
> verified equivocation requires governed retirement, and rollback uncertainty
> requires a new identity. The successor operation carries intersecting-majority
> `W`, class, readiness/evidence commitments, canonical checkpoint-handoff terms
> for active-set changes, and complete state. A predecessor-admin-approved
> `replica.handoff.prepare` commits first, leaves replica policy unchanged, and
> permits only its prepared active-set `replica.set` or cancellation child
> derived from the exact intent.
> Eligible replicas produce one prepare-bound predecessor-`W` frontier bundle;
> a derived child then obtains predecessor-admin threshold approval, and neither
> input alone can fold the child. Abandonment is a separately approved/committed
> cancellation child derived from the same reservation. Unrelated **ordinary**
> governance cannot extend the reservation; recovery-authorized `fork.resolve`
> remains the exception and every accepted stable-v2 resolution structurally
> creates a current-`W`, selected-admin-approved
> `replica.handoff.fork_reconcile` requirement without consulting detached
> artifact observations. Its state carries a governance-derived control-signer-
> exclusion commitment and fixed-size **structural** count/root for the
> governance-derived unresolved set; verifier-local quarantine/readiness/
> evidence arrival is never a fold input. If another resolution commits first,
> it rolls every prior unresolved reservation, closure, governance-retained
> statement/control commitment, and applicable exclusion into the newest
> structural commitment. Each exact-replay signer statement adds only its own
> held-set count/root; collection unions those sets and verified supplemental
> leaves into the separate **final** count/root. One latest reconciliation child
> verifies that final bounded-chunk full-DAG proof and consumes all dependencies
> atomically; it either closes the selected reservation or becomes its
> latest base. A local readiness withdrawal stops service and drives governed
> cancellation/disablement but never arrival-order-reinterprets an exposed
> child. Recovery keys count toward neither administrator nor replica quorum,
> and `admin_seq` is unchanged. This
> prevents ordinary churn from staling the cancellation while avoiding an
> unapproved fence and handoff hash cycle. It is a new schema, not the landed
> one-record map upsert. Existing code, operation strings, roots,
> genesis/`CommunityId`, and golden bytes remain candidate evidence. See
> [`v2-replica-replacement-recovery.md`](v2-replica-replacement-recovery.md).

---

## 1. Summary

Implement the #134 §7.1-§7.3 governance-log foundation in the pure v2 core crate:

- `GenesisConfig` and non-recursive `CommunityId` derivation;
- `GovernanceEntryBody`, `GovernanceApproval`, and `GovernanceEntry` record types;
- deterministic approval sorting and duplicate-approval rejection/counting rules;
- `GovernanceStateRootRecord`, committing to the six #134 §7.1 components in fixed order under `iroh-room-v2/governance-state`;
- the closed #134 §7.3 operation registry;
- pure, deterministic `apply(old_state, op) -> new_state` functions for every registered operation;
- golden/unit tests for genesis threshold verification, every operation, state-root recomputation, unknown-operation rejection, and absence of async/network dependencies.

This is a pure protocol-core change only. It must not add networking, replicas/runtime code, `tokio`, `iroh`, storage, CLI/SDK wiring, GitHub automation, or v1 protocol behavior.

---

## 2. Repository context read

Relevant local context:

- `README.md` maps `crates/iroh-rooms-v2-core/` as the pure v2 crypto core: canonical CBOR, BLAKE3/Ed25519 identifiers, governance state machine, fork detection, and member Merkle map. It is unused by the shipped runtime in this phase.
- Root `Cargo.toml` includes `crates/iroh-rooms-v2-core` as a workspace member and documents the no-network/no-store/no-async constraint.
- `CONTRIBUTING.md` defines the standard quality gate as `scripts/verify.sh`; protocol-labeled work requires maintainer review.
- `specs/v2-crypto-core-crate.md` is the parent #140 plan and lists #147 as the child issue for governance entry bodies, approvals, entry IDs, approval IDs, and deterministic state-root computation.
- `specs/v2-identifiers-domain-separation.md` and `src/domain.rs` show #146 has landed the frozen #134 §6.2 domains, including `iroh-room-v2/community`, `iroh-room-v2/governance-entry`, `iroh-room-v2/governance-approval`, and `iroh-room-v2/governance-state`. Existing governance code still uses legacy candidate aliases such as `GOVERNANCE_ENTRY_SIGN`, `GOVERNANCE_ENTRY_ID`, and `GOVERNANCE_STATE_ROOT`; #147 should move new normative code to the frozen domains.
- `src/ids.rs` exposes #146 names (`CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId`, `ReplicaId`) plus older compatibility types (`RoomId`, `GovernanceEntryId`, `ApprovalId`, `StateRoot`). #147 should prefer the #146 names in public v2 governance APIs.
- `src/cbor.rs`, `src/schema.rs`, and `src/signed.rs` already provide deterministic CBOR, closed-schema validation helpers, and a generic signed-record trust boundary.
- `src/governance/model.rs`, `approval.rs`, `fold.rs`, `authz.rs`, and `state_root.rs` contain earlier candidate governance scaffolding (`InitRoom`, `AddMember`, etc.) that does not match the #134 §7.3 registry or the exact #147 entry-body shape. Treat it as scaffolding to replace/adapt, not as normative.
- `src/error.rs` already has typed rejects such as `unknown_record_kind`, `invalid_approval`, and `state_root_mismatch` that are close to #147 needs.
- `tests/banned_dependencies.rs` already checks the v2 core does not depend on `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, or `rusqlite`.
- `tests/signed_records_golden.rs` and `tests/golden/` currently pin older candidate governance bytes. #147 must update or add vectors deliberately; any protocol-shape drift should be explicit and reviewable.

---

## 3. Scope

### 3.1 In scope

1. Define `GenesisConfig` and threshold-verified genesis signing.
2. Derive `CommunityId` from `GenesisConfig` without including `community_id` in the preimage.
3. Define the canonical governance log records:
   - `GovernanceEntryBody`;
   - `GovernanceApproval`;
   - `GovernanceEntry`.
4. Define `GovernanceStateRootRecord` with exactly six components, in fixed order:
   1. administrators;
   2. recovery;
   3. replicas;
   4. members/devices/roles;
   5. stream manifest;
   6. community policy.
5. Implement the closed #134 §7.3 operation registry as typed Rust enums/payload structs:
   - `member.grant`
   - `member.revoke`
   - `device.grant`
   - `device.revoke`
   - `admin.set`
   - `recovery.set`
   - `replica.set`
   - `stream.create`
   - `stream.policy_set`
   - `stream.archive`
   - `invite.revoke`
   - `policy.set`
   - `fork.resolve`
   - `migration.accept`
6. Reject unknown operation kinds while decoding; never silently ignore them.
7. Add a pure apply function for every registered operation.
8. Verify declared `state_root` by recomputing state after applying the operation.
9. Unit/golden tests required by acceptance.
10. Preserve the pure-core dependency boundary: no `tokio`, `iroh`, store, replica runtime, or network code.

### 3.2 Out of scope

- Authorization policy for ordinary governance operations (#148), beyond genesis bootstrap threshold verification and cryptographic approval validation.
- Fork detection, branch choice, and conflict-resolution semantics (#149). `fork.resolve` is only parsed, validated structurally, and applied in a deterministic state-root-visible placeholder described below.
- Governance checkpoints and snapshots (#150), except that #147 exposes the current `state_root` for those later records.
- Network, replica transport, replica receipts, persistence, SQLite, CLI, SDK exports, deployment, release work, branch/PR/GitHub work.
- v1 protocol changes in `crates/iroh-rooms-core/`.

---

## 4. Key decisions

### D1 — Use `crates/iroh-rooms-v2-core` only

The owning code should live under `crates/iroh-rooms-v2-core/src/governance/`. Do not touch runtime/network/store crates. The existing v2 crate is already isolated and `publish = false`, which matches the issue.

### D2 — Prefer #146 frozen names and domains

Use these normative #146 domains for #147 code:

| Boundary | Domain constant | Bytes |
|---|---|---|
| Community ID | `domain::COMMUNITY` | `iroh-room-v2/community` |
| Governance entry ID/signing message | `domain::GOVERNANCE_ENTRY` | `iroh-room-v2/governance-entry` |
| Governance approval ID/signing message | `domain::GOVERNANCE_APPROVAL` | `iroh-room-v2/governance-approval` |
| State root | `domain::GOVERNANCE_STATE` | `iroh-room-v2/governance-state` |

Do not introduce new legacy `:sign:v1`/`:id:v1` strings for #147. If existing #153 candidate tests depend on old aliases, update the fixtures in the #147 change and document the intentional vector drift.

### D3 — Avoid recursive `CommunityId` derivation

`CommunityId` must be derived from canonical `GenesisConfig` bytes that do not contain `community_id` and do not contain records that themselves contain `community_id`.

Recommended derivation:

```text
genesis_config_csb = canonical_cbor(GenesisConfigBody)
community_id       = BLAKE3(domain::COMMUNITY || genesis_config_csb)
```

`GenesisConfigBody` may include deterministic bootstrap data such as genesis nonce, created time, admin threshold, admin keys, recovery config, initial replicas, and initial community policy, but it must not include `CommunityId`, governance entry IDs, governance approvals, or state roots.

### D4 — Separate bootstrap threshold verification from #148 authorization

Genesis threshold verification is in scope because acceptance requires it. Ordinary governance authorization is out of scope for #147 and belongs to #148.

#147 should therefore expose:

```rust
fn verify_genesis(config: &GenesisConfig, approvals: &[GenesisSignature]) -> Result<CommunityId, Reject>;
fn verify_entry_crypto(entry: &GovernanceEntry) -> Result<GovernanceEntryBody, Reject>;
fn verify_approval_crypto(approval: &GovernanceApproval) -> Result<GovernanceApprovalBody, Reject>;
fn apply_verified_entry(old: &GovernanceState, body: &GovernanceEntryBody) -> Result<GovernanceState, Reject>;
```

The later #148 layer can call these and then decide whether the signer/approvals are authorized.

### D5 — Entry order is a linear hash chain

Represent the totally ordered governance log as a sequence after genesis:

- `seq == 1` for the first governance entry after genesis;
- `prev == None` only when `seq == 1`;
- `seq > 1` requires `prev == Some(previous_governance_id)`;
- `GovernanceId` is derived from the entry body CSB under `domain::GOVERNANCE_ENTRY`;
- callers/folds reject skipped sequence numbers, wrong `prev`, or wrong `community_id` as structural log errors.

Fork detection beyond this single chain check remains #149.

### D6 — Approvals are sorted by canonical identity

`GovernanceEntry` must store approvals in deterministic order. Sort by:

1. `approver` raw 32-byte principal bytes;
2. approval ID raw bytes as a tie-breaker.

Reject duplicate approvers for a single entry as `invalid_approval`; do not double-count duplicates toward thresholds. Entry canonical bytes must not depend on the caller's original approval order.

### D7 — State root is the hash of a fixed-order six-component record

Define:

```rust
pub struct GovernanceStateRootRecord {
    pub administrators_root: [u8; 32],
    pub recovery_root: [u8; 32],
    pub replicas_root: [u8; 32],
    pub members_devices_roles_root: [u8; 32],
    pub stream_manifest_root: [u8; 32],
    pub community_policy_root: [u8; 32],
}
```

Canonicalize this as a fixed-order CBOR array or a closed map whose key order is byte-pinned by tests. Prefer a fixed-order array for direct correspondence to §7.1.

Recommended root computation:

```text
component_root_i = BLAKE3(domain::GOVERNANCE_STATE || canonical_cbor({ label, value }))
state_root       = BLAKE3(domain::GOVERNANCE_STATE || canonical_cbor(GovernanceStateRootRecord))
```

Including `label` in each component preimage prevents cross-component replay while still using the single §7.1 domain.

### D8 — `fork.resolve` is registry/apply-visible but fork handling stays deferred

Because #149 owns fork detection and resolution semantics, #147 should only:

- parse and validate `fork.resolve` as a known operation;
- apply it by inserting a deterministic `ForkResolutionMarker` into the community-policy component, so the operation has a state-root-visible pure transition;
- leave branch selection, evidence interpretation, and fork unblocking to #149.

If maintainers consider even this marker too much fork behavior, the fallback is a deterministic no-op apply with an explicit test proving no state change. The marker approach is preferred because acceptance asks every operation to have an `old_state -> new_state` apply function.

---

## 5. Proposed data model

### 5.1 `GenesisConfig`

Add `src/governance/genesis.rs` or a `genesis` section in `model.rs`:

```rust
pub struct GenesisConfig {
    pub schema_version: u64,
    pub created_at_ms: u64,
    pub genesis_nonce: [u8; 32],
    pub admin_threshold: u16,
    pub administrators: Vec<PrincipalId>,
    pub recovery: RecoveryConfig,
    pub replicas: Vec<ReplicaConfig>,
    pub community_policy: CommunityPolicy,
}
```

Validation rules:

- `schema_version == 2` unless #134 defines another v2 bootstrap version.
- `created_at_ms` is signed data only; do not compare to local wall clock.
- `admin_threshold > 0` and `admin_threshold <= unique(administrators).len()`.
- `administrators` sorted ascending, unique, non-empty.
- `replicas` sorted by `ReplicaId`, unique.
- all nested maps closed; unknown keys rejected.

Genesis signing:

```rust
pub struct GenesisSignature {
    pub signer: PrincipalId,
    pub signature: Signature,
}
```

`verify_genesis` computes the genesis CSB, verifies each signature over `domain::COMMUNITY || genesis_config_csb`, rejects duplicate or non-admin signers, and succeeds only when unique valid admin signatures meet `admin_threshold`. It returns the derived `CommunityId`.

### 5.2 `GovernanceEntryBody`

Replace/adapt the candidate `GovernanceEntryBody` shape to exactly match the issue:

```rust
pub struct GovernanceEntryBody {
    pub community_id: CommunityId,
    pub seq: u64,
    pub prev: Option<GovernanceId>,
    pub created_at_ms: u64,
    pub kind: GovernanceOperationKind,
    pub payload: GovernanceOperationPayload,
    pub state_root: StateRoot,
}
```

Notes:

- No `room_id`, `author`, `schema_version`, `epoch`, `parent`, or candidate action names in the canonical #147 body unless #134 explicitly requires them.
- The entry signer belongs in the signed record envelope, not in the body, unless #134 says signer is body data.
- `kind` and `payload` must agree. A `kind`/payload mismatch is `invalid_content`.
- `created_at_ms` is signed data only; no local wall-clock checks.
- `state_root` is the root after applying this operation to the previous state.

### 5.3 `GovernanceApproval`

Define approval body and signed approval record:

```rust
pub struct GovernanceApprovalBody {
    pub community_id: CommunityId,
    pub entry_id: GovernanceId,
    pub state_root: StateRoot,
    pub approver: PrincipalId,
    pub created_at_ms: u64,
}

pub struct GovernanceApproval {
    body: GovernanceApprovalBody,
    csb: Vec<u8>,
    signature: Signature,
}
```

> **Amended by #178** — `GovernanceApproval` owns the **exact canonical signed
> bytes (CSB)** it was constructed with alongside the typed body, mirroring
> `crate::signed::Envelope`. The fields are private correlated state exposed
> only through read-only accessors (`body()`, `csb()`, `signature()`); they
> cannot be desynchronized through safe public APIs. A typed `new(body, secret)`
> constructor encodes once and pins that vector; a trust-boundary
> `from_received_csb(csb, signature)` constructor canonical-decodes the supplied
> slice but retains it byte-for-byte. See `specs/v2-governance-records-verbatim-csb.md`.

Verification:

- approval sort hash is `BLAKE3(domain::GOVERNANCE_APPROVAL || csb)` computed over the **retained** CSB (`approval_id_from_csb`), not a re-encoding of the body.
- the signature message is `domain::GOVERNANCE_APPROVAL || csb` over the **retained** CSB; it is checked **before** the typed body is re-encoded (post-signature re-encode byte-equality is the canonicality check).
- `body.entry_id` must equal the enclosing entry's **exact-CSB-derived** ID.
- `body.community_id` and `body.state_root` must equal the entry body's declared values.
- approval signature failure (including an approver/signer mismatch) is `bad_signature`; an unbound or duplicate approval is `invalid_approval`.

### 5.4 `GovernanceEntry`

```rust
pub struct GovernanceEntry {
    body: GovernanceEntryBody,
    csb: Vec<u8>,
    signer: PrincipalId,
    signature: Signature,
    approvals: Vec<GovernanceApproval>,
}
```

> **Amended by #178** — like `GovernanceApproval`, `GovernanceEntry` owns the
> **exact entry-body CSB** it was constructed with as private correlated state
> (accessors `body()`, `csb()`, `signer()`, `signature()`, `approvals()`).
> `new(body, secret, approvals)` encodes once and pins the vector it signs;
> `from_received_csb(csb, signer, signature, approvals)` retains the supplied
> wire/storage bytes verbatim. Signer, signature, and approvals remain outside
> entry-body CSB. See `specs/v2-governance-records-verbatim-csb.md`.

Verification pipeline:

1. Canonical-decode the exact received bytes at construction (`cbor::decode_canonical`), forming the typed body.
2. Reject unknown fields, non-canonical bytes, wrong byte widths, and unknown operation kinds (at construction).
3. Verify the entry signature over `domain::GOVERNANCE_ENTRY || csb` using the **retained** CSB — never a re-serialization of the typed body.
4. Require the typed body to re-encode to exactly the retained CSB (semantic-canonicality round-trip, post-signature); a body whose typed decode normalizes representation (e.g. an unsorted/duplicate `admin.set` `administrators` array) is rejected here as `non_canonical_encoding`.
5. Derive the authenticated `GovernanceId` from the retained CSB under `domain::GOVERNANCE_ENTRY` (`entry_id_from_csb`).
6. Sort approvals canonically by `(approver bytes, retained-CSB approval hash)` and reject duplicate approvers.
7. Verify each approval signature over its retained CSB and its bindings to `community_id`, the exact-CSB entry ID, and `state_root`.
8. Apply the operation to `old_state` and recompute `state_root`; reject mismatch.
9. Return the verified body, signer, sorted approvals, and the exact-CSB identity (for #148 to authorize and for chain links to bind to).

---

## 6. Operation registry and apply functions

### 6.1 Registry type

Use closed enums:

```rust
pub enum GovernanceOperationKind {
    MemberGrant,
    MemberRevoke,
    DeviceGrant,
    DeviceRevoke,
    AdminSet,
    RecoverySet,
    ReplicaSet,
    StreamCreate,
    StreamPolicySet,
    StreamArchive,
    InviteRevoke,
    PolicySet,
    ForkResolve,
    MigrationAccept,
}

pub enum GovernanceOperationPayload {
    MemberGrant(MemberGrant),
    MemberRevoke(MemberRevoke),
    DeviceGrant(DeviceGrant),
    DeviceRevoke(DeviceRevoke),
    AdminSet(AdminSet),
    RecoverySet(RecoverySet),
    ReplicaSet(ReplicaSet),
    StreamCreate(StreamCreate),
    StreamPolicySet(StreamPolicySet),
    StreamArchive(StreamArchive),
    InviteRevoke(InviteRevoke),
    PolicySet(PolicySet),
    ForkResolve(ForkResolveMarker),
    MigrationAccept(MigrationAccept),
}
```

Wire strings are exactly the #134 §7.3 names. Do not accept aliases such as current `init_room`, `add_member`, `set_policy`, or `rotate_device` in the normative v2 path.

### 6.2 State shape

Recommended internal state:

```rust
pub struct GovernanceState {
    pub community_id: CommunityId,
    pub administrators: AdministratorState,
    pub recovery: RecoveryState,
    pub replicas: BTreeMap<ReplicaId, ReplicaRecord>,
    pub members: BTreeMap<PrincipalId, MemberRecord>,
    pub streams: BTreeMap<StreamId, StreamRecord>,
    pub policy: CommunityPolicy,
}
```

Use `BTreeMap`/sorted `Vec` everywhere roots depend on order. Store revoked invites, accepted migrations, and fork-resolution markers under `CommunityPolicy` unless #134 names a different component; this keeps the state-root record to the six §7.1 components.

### 6.3 Apply table

| Operation | Payload fields to validate | Pure state transition |
|---|---|---|
| `member.grant` | `member_id`, `role`, optional metadata | Insert or reactivate member; set role; preserve existing devices unless payload says otherwise. Root component: members/devices/roles. |
| `member.revoke` | `member_id`, optional reason/code | Mark member revoked/inactive; preserve tombstone for deterministic history. Root component: members/devices/roles. |
| `device.grant` | `member_id`, `device_id`, device role/caps if specified | Add device to member's sorted device set; reject if member absent unless #134 allows pre-grant. Root component: members/devices/roles. |
| `device.revoke` | `member_id`, `device_id` | Mark/remove device from member's active device set with tombstone if needed for deterministic replay. Root component: members/devices/roles. |
| `admin.set` | sorted admin principals, threshold | Replace administrator set and threshold; validate non-empty unique admins and `1 <= threshold <= admins.len()`. Root component: administrators. |
| `recovery.set` | recovery policy/config | Replace recovery component with canonical config. Root component: recovery. |
| `replica.set` | `replica_id`, endpoint/capability/status | Upsert or disable a Phase-B candidate replica record, sorted by `ReplicaId`. `capability` is not a durability class. Stable v2 uses #159's successor atomic full-set/class/`W` operation. Root component: replicas. |
| `stream.create` | `stream_id`, stream metadata, initial policy | Insert a new active stream; reject duplicate `stream_id` as invalid content unless #134 defines idempotency. Root component: stream manifest. |
| `stream.policy_set` | `stream_id`, stream policy | Replace the policy for an existing stream; reject missing stream unless #134 defines create-on-set. Root component: stream manifest. |
| `stream.archive` | `stream_id`, archived flag/time/reason | Mark an existing stream archived; keep in manifest. Root component: stream manifest. |
| `invite.revoke` | `invite_id` or invite commitment | Add invite to sorted revoked-invite set. Root component: community policy. |
| `policy.set` | community policy fields | Replace/patch community policy according to #134; canonicalize sorted sets. Root component: community policy. |
| `fork.resolve` | evidence IDs and decision marker | Record a deterministic marker under community policy only; #149 later interprets evidence and branch effects. Root component: community policy. |
| `migration.accept` | migration id/version/source | Add migration acceptance marker; reject duplicate if #134 requires single acceptance. Root component: community policy. |

Every apply function must be total over structurally valid payloads and must not read wall clocks, randomness, storage, network, or global state.

### 6.4 Apply API

Recommended APIs:

```rust
pub fn apply(old: &GovernanceState, op: &GovernanceOperationPayload) -> Result<GovernanceState, Reject>;
pub fn apply_member_grant(old: &GovernanceState, payload: &MemberGrant) -> Result<GovernanceState, Reject>;
// one named function per registry entry
```

Implementation notes:

- Clone-and-return is acceptable for this pure core; optimize later only if profiling demands it.
- Keep each operation in a small function so acceptance can have one unit test per operation.
- Structural failures should return `invalid_content` or a more specific typed reject if added.
- Unknown operation strings must fail before apply is called.

---

## 7. State-root computation

### 7.1 Component canonicalization

Add component-specific canonicalizers, for example:

```rust
fn administrators_component(state: &GovernanceState) -> CborValue;
fn recovery_component(state: &GovernanceState) -> CborValue;
fn replicas_component(state: &GovernanceState) -> CborValue;
fn members_devices_roles_component(state: &GovernanceState) -> CborValue;
fn stream_manifest_component(state: &GovernanceState) -> CborValue;
fn community_policy_component(state: &GovernanceState) -> CborValue;
```

Each component must:

- emit closed deterministic CBOR;
- sort maps/sets by raw identifier bytes;
- include tombstones/markers needed for deterministic future replay;
- omit no semantically relevant fields;
- reject or canonicalize duplicate logical records before hashing.

### 7.2 Root record

Compute:

```rust
pub fn component_root(label: &'static str, value: CborValue) -> [u8; 32];
pub fn governance_state_root_record(state: &GovernanceState) -> GovernanceStateRootRecord;
pub fn compute_state_root(state: &GovernanceState) -> StateRoot;
pub fn verify_state_root(state: &GovernanceState, expected: StateRoot) -> Result<(), Reject>;
```

The final hash uses `domain::GOVERNANCE_STATE`, not the legacy `GOVERNANCE_STATE_ROOT` alias.

### 7.3 Declared root check

When applying an entry:

```rust
let new_state = apply(old_state, &entry.body.payload)?;
let actual = compute_state_root(&new_state);
if actual != entry.body.state_root {
    return Err(Reject::StateRootMismatch);
}
```

This check is required for the golden vector acceptance criterion.

---

## 8. Error model

Prefer existing `Reject` variants where they fit:

| Condition | Reject code |
|---|---|
| non-canonical CBOR, trailing data, duplicate map key | `non_canonical_encoding` |
| wrong schema/version if version field remains present | `unknown_version` |
| unknown operation kind | `unknown_record_kind` or new `unknown_governance_operation` |
| known kind with wrong payload fields | `invalid_content` |
| entry ID mismatch | `id_mismatch` |
| entry/approval signature failure | `bad_signature` |
| approval not bound to entry/root/community or duplicate approver | `invalid_approval` |
| genesis threshold not met | `insufficient_authorization` or new `threshold_not_met` |
| declared root differs from recomputed root | `state_root_mismatch` |
| missing previous entry / sequence break in log fold | `missing_dependency` or `invalid_content` |

If adding new variants, update `error::all_codes()` and taxonomy tests in the same change.

No logging or metrics should be emitted from v2 core. Downstream runtime/CLI layers can add observability later by matching `.code()`.

---

## 9. Security, privacy, reliability, and performance

### Security

- Use deterministic canonical bytes as the trust boundary; never verify a signature over reserialized bytes. (#178 made this actually true for `GovernanceEntry`/`GovernanceApproval`: the records own the exact received CSB, and signatures/identity run over those bytes — see §5.3/§5.4 and `specs/v2-governance-records-verbatim-csb.md`.)
- Domain-separate genesis, entry, approval, and state-root hashes with the frozen #146 domains.
- Reject unknown operation kinds and unknown payload keys to avoid parser differentials.
- Do not count duplicate approvals twice.
- Do not use local wall clocks for validity; `created_at_ms` is signed data only.
- Do not log signatures, seeds, or private material.

### Privacy

- Golden vectors must use deterministic non-secret test seeds and synthetic IDs only.
- Do not include real names, endpoints, invite secrets, paths, network addresses, or user data in fixtures.

### Reliability

- All state is derived from sorted deterministic structures.
- Identical genesis config plus identical accepted governance-entry sequence must produce byte-identical state roots.
- Declared root mismatch must fail closed.

### Performance

- Clone-and-return apply functions are acceptable for the pure core and test-scale state.
- Use `BTreeMap`/sorted `Vec` for deterministic order rather than hash maps.
- Component hashing bounds recomputation cost and makes future checkpoint/snapshot work straightforward.

---

## 10. Implementation steps

1. **Add/reshape governance modules**
   - Add `governance/genesis.rs`, `operation.rs`, and `state.rs`, or equivalent sections in existing modules.
   - Keep public exports in `governance/mod.rs` explicit.

2. **Implement `GenesisConfig`**
   - Define body, canonical CBOR encode/decode, closed validation, and deterministic sorting of admins/replicas.
   - Implement `CommunityId::derive` from genesis CSB if an appropriate helper does not already exist.
   - Implement genesis signature verification against `admin_threshold`.

3. **Implement canonical entry/approval records**
   - Replace candidate `InitRoom`/`AddMember` entry shape in the normative path.
   - Use `CommunityId` and `GovernanceId`, not `RoomId`/`GovernanceEntryId`, for #147 public APIs.
   - Verify entry signature and approval signatures under frozen #146 domains.
   - Sort approvals and reject duplicates.

4. **Implement operation registry**
   - Add exact string conversions for all fourteen #134 §7.3 operation names.
   - Decode payloads through closed schemas.
   - Add an unknown-kind negative test.

5. **Implement `GovernanceState` six components**
   - Model administrators, recovery, replicas, members/devices/roles, stream manifest, and community policy.
   - Decide exact tombstone/marker representation for revocations, archives, fork-resolution markers, and migrations.

6. **Implement one pure apply function per operation**
   - Keep functions independent and directly testable.
   - Do not call authorization functions from #148.
   - Return typed `Reject` for structurally invalid transitions.

7. **Implement state-root record**
   - Canonicalize each component.
   - Compute component roots and final `StateRoot` under `domain::GOVERNANCE_STATE`.
   - Add `verify_state_root` for declared-root checks.

8. **Integrate fold-level validation**
   - Given old state and a verified entry, apply payload, recompute root, compare to `body.state_root`, and return new state.
   - Enforce `seq`/`prev` chain rules for the totally ordered log.

9. **Update tests and vectors**
   - Add unit tests for every apply function.
   - Add genesis threshold tests.
   - Add approval sorting/duplicate tests.
   - Add unknown-operation rejection.
   - Add golden vector for state-root recomputation.
   - Update any older signed-record golden vectors that intentionally drift from candidate domains/shapes.

10. **Run verification**
    - `cargo fmt --all --check`
    - `cargo test -p iroh-rooms-v2-core --all-targets --all-features`
    - `cargo test -p iroh-rooms-v2-core --test signed_records_golden --all-features`
    - `cargo test -p iroh-rooms-v2-core --test banned_dependencies --all-features`
    - `scripts/verify.sh` before maintainer review if time permits.

---

## 11. Acceptance criteria mapping

| Acceptance item | Spec coverage |
|---|---|
| Genesis signs and verifies under declared admin threshold | `GenesisConfig`, `GenesisSignature`, and `verify_genesis`; tests for threshold met, threshold not met, duplicate signer, non-admin signer. |
| Each §7.3 operation has pure apply function and unit test | Operation table and step 6; one test per registered operation. |
| `state_root` recomputation matches declared root on a golden vector | `verify_state_root` and golden vector under `tests/golden/`. |
| Unknown operation kind rejected, not ignored | Closed registry parse returning `unknown_record_kind`/specific unknown-op reject; negative test. |
| No `tokio` / `iroh` dependency | Existing banned-dependency guard extended/preserved. |

---

## 12. Test plan

### Unit tests

- `genesis_threshold_met_verifies`
- `genesis_threshold_not_met_rejected`
- `genesis_duplicate_admin_signature_counts_once`
- `genesis_non_admin_signature_rejected`
- `community_id_does_not_include_community_id_in_preimage`
- `approvals_are_sorted_canonically`
- `duplicate_approval_rejected`
- `approval_wrong_entry_or_root_rejected`
- `unknown_governance_operation_rejected`
- one `apply_<operation>_updates_expected_component` test for each of the fourteen registry entries;
- `declared_state_root_mismatch_rejected`.

### Golden vectors

Add or update crate-local fixtures under `crates/iroh-rooms-v2-core/tests/golden/`:

- `v2-governance-log.json` with:
  - deterministic genesis config;
  - derived community ID;
  - initial state root;
  - at least one signed entry and approval;
  - final six component roots;
  - final state root.
- Update `v2-signed-records.json` if entry/approval canonical bytes change from candidate scaffolding.

### Dependency tests

Keep `tests/banned_dependencies.rs` and ensure it still rejects at least:

- `tokio`
- `iroh`
- `iroh-blobs`
- `iroh-gossip`
- `rusqlite`

---

## 13. Rollout and rollback

Rollout is low operational risk because `iroh-rooms-v2-core` is pure, `publish = false`, and unused by the shipped v1 runtime in this phase.

Rollout plan:

1. Land the pure-core types and tests behind normal crate APIs.
2. Keep SDK/CLI/runtime exports unchanged.
3. Require maintainer review because this is protocol surface area.
4. Let #148, #149, and #150 consume the new APIs in separate issues.

Rollback plan:

- Revert the #147 pure-core changes and fixtures if the shape is wrong.
- If only vectors are wrong but the model is correct, regenerate vectors in a dedicated review with an explicit schema/vector bump note.
- Because no runtime wiring is included, rollback has no storage/network migration impact.

---

## 14. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---:|---:|---|
| Existing candidate governance scaffolding conflicts with #134 names | High | Medium | Treat current `InitRoom`/`AddMember` model as non-normative; migrate deliberately with vector updates. |
| Domain mismatch between old aliases and #146 frozen domains | High | High | Use `domain::GOVERNANCE_ENTRY`, `GOVERNANCE_APPROVAL`, and `GOVERNANCE_STATE`; tests pin exact bytes. |
| `GenesisConfig` preimage accidentally becomes recursive | Medium | High | Unit test that genesis CSB has no `community_id`; derive ID only after canonical genesis config is complete. |
| `fork.resolve` semantics overreach into #149 | Medium | Medium | Only record a deterministic marker or no-op; document that branch resolution is #149. |
| State-root component boundaries under-specified | Medium | High | Label component preimages and golden-vector every component root. |
| Approval duplicates counted twice | Medium | High | Sort approvals and dedup/reject by approver before threshold checks. |
| Later #148 authorization needs fields omitted by #147 | Medium | Medium | Keep signer, approvals, operation payload, and state roots available in typed verified records. |

---

## 15. Assumptions

1. #146 is the normative source for domain strings; #147 should not continue using old candidate `:sign:v1`/`:id:v1` strings for new protocol records.
2. `CommunityId` is the canonical v2 type, replacing `RoomId` in #147 public APIs.
3. Governance entries form a single linear log after genesis: `seq` plus `prev` identifies order.
4. `created_at_ms` is signed metadata and must not be validated against a local clock in the pure core.
5. Genesis threshold verification is in scope; ordinary operation authorization is #148.
6. `fork.resolve` must exist in the registry and have a pure apply function, but actual fork handling is #149.
7. Revocations, migration acceptances, and fork-resolution markers can live in the community-policy component unless #134 assigns them elsewhere.
8. The current v2 crate's dependency set is acceptable; new dependencies should not be needed.

---

## 16. Open questions

1. What exact fields does #134 §7.2 require in `GenesisConfig` besides administrators and threshold?
2. Does #134 define a separate signed genesis record type, or should genesis signatures be represented as a local `GenesisSignature` list over `GenesisConfig` CSB?
3. Does `GovernanceEntryBody.prev` use `None` for the first post-genesis entry, a zero hash, or a genesis-derived ID?
4. Does #134 require the entry signer inside `GovernanceEntryBody`, or only in the enclosing signed record?
5. Should unknown operation use existing `unknown_record_kind` or a new `unknown_governance_operation` reject code?
6. Are `fork.resolve` and `migration.accept` expected to mutate the community-policy component in #147, or should they be deterministic no-ops until #149/migration work?
7. Are component roots themselves mandated as raw hashes, Merkle roots, or canonical CBOR hashes in #134 §7.1?
8. Should duplicate approvals be rejected outright or accepted while counted once? This spec recommends rejection for stricter consensus behavior.

---

## 17. Implementation notes (post-landing)

#147 landed as a pure-protocol-core change confined to
`crates/iroh-rooms-v2-core/src/governance/log/` (`genesis.rs`, `model.rs`,
`operation.rs`, `records.rs`, `state.rs`). It satisfies every acceptance criterion
(genesis threshold verification, a pure `apply(old, op) -> new_state` function with
a unit test per §7.3 operation, byte-pinned state-root golden vectors, unknown-kind
rejection at the decode boundary, and no `tokio`/`iroh` dependency — guarded by
`tests/banned_dependencies.rs`). The §16 open questions were resolved as follows,
each pinned by focused tests:

1. **OQ-1 (`GenesisConfig` fields):** beyond administrators and threshold the body
   carries `schema_version` (must equal `2`), `created_at_ms`, `genesis_nonce`,
   `recovery` (`RecoveryConfig`), `replicas` (sorted unique by `ReplicaId`), and
   `community_policy`. None of these carry `community_id`, so derivation is
   non-recursive (D3). See `genesis.rs::GenesisConfig` and the
   `community_id_does_not_include_community_id_in_preimage` test.

2. **OQ-2 (signed genesis record):** genesis signatures are a local
   `GenesisSignature { signer, signature }` list verified over
   `domain::COMMUNITY || genesis_config_csb` — there is no separate signed genesis
   record type. `verify_genesis(config, signatures)` returns the derived
   `CommunityId` once unique valid admin signatures meet `admin_threshold`.

3. **OQ-3 (first-entry `prev`):** `prev == None` only when `seq == 1`; `seq > 1`
   requires `prev == Some(previous_governance_id)`. Enforced by the single-chain
   `check_chain_link` helper (D5); broader fork detection stays #149.

4. **OQ-4 (entry signer location):** the signer lives in the enclosing signed
   envelope `GovernanceEntry { body, signer, signature, approvals }`, **not** in
   `GovernanceEntryBody`. The body carries only `community_id`, `seq`, `prev`,
   `created_at_ms`, `kind`, `payload`, `state_root`.

5. **OQ-5 (unknown-operation reject code):** the existing `Reject::UnknownRecordKind`
   is reused (no new code). `GovernanceOperationKind::parse` returns it for any wire
   string outside the 14-entry §7.3 registry, and `decode_entry_csb` surfaces it
   before any signature or apply work — proven by the wire-level negative test
   `e2e_unknown_operation_kind_rejected_at_decode_boundary`.

6. **OQ-6 (`fork.resolve` / `migration.accept`):** both mutate the community-policy
   component (state-root-visible, not no-ops). `fork.resolve` appends a
   `ForkResolutionMarker` (D8 preferred approach); branch selection and evidence
   interpretation remain #149. `migration.accept` inserts into the migrations set and
   rejects duplicate acceptance. See `state.rs::apply_fork_resolve` /
   `apply_migration_accept`.

7. **OQ-7 (component-root form):** each component root is
   `BLAKE3(GOVERNANCE_STATE || canonical_cbor({label, value}))` — a label-separated
   hash of the component's canonical CBOR (the `label` in the preimage prevents
   cross-component replay, §7.2 / D7). The final `state_root` is
   `BLAKE3(GOVERNANCE_STATE || canonical_cbor(GovernanceStateRootRecord))` over the
   fixed-order six-byte-string array. `COMPONENT_LABELS` and the six-component count
   are byte-pinned by `state_root_record_has_six_distinct_component_labels`, and the
   genesis/post-`member.grant` roots are byte-pinned against silent drift.

8. **OQ-8 (duplicate approvals):** rejected outright (the stricter option this spec
   recommended). `verify_genesis` rejects a repeated signer with
   `Reject::InvalidApproval` rather than counting them toward the threshold, and the
   entry pipeline sorts approvals canonically and rejects duplicate approvers before
   any threshold work.

### Boundary against the candidate scaffolding

The normative `governance::log` module is **additive** to the earlier candidate
governance scaffolding (`super::model`, `super::approval`, …) that still carries the
frozen #153 signed-record golden vectors. That scaffolding remains the candidate
path; new normative code uses the frozen #146 domains
(`domain::GOVERNANCE_ENTRY`, `domain::GOVERNANCE_APPROVAL`,
`domain::GOVERNANCE_STATE`) and the #146 names (`CommunityId`, `GovernanceId`)
exclusively. A deliberate, reviewable migration to unify the two surfaces is deferred.

### Deferred scope (unchanged)

Authorization rules (#148), fork detection/branch choice beyond the single chain-link
check (#149), and checkpoints/snapshots (#150) remain out of scope, as do any
network/replica/storage code. `#147` only exposes the current `state_root` and the
typed verified records those later layers will consume.

### Amendment: verbatim-CSB trust boundary (#178, post-landing)

#178 closed a trust-boundary weakness flagged in PR #177 review: the original #147
verify path re-derived the CSB from the typed body before checking signatures and
deriving identity, so a body whose typed decode **normalizes** representation (the
`admin.set` `administrators` array is sorted and de-duplicated during decode) could be
accepted over bytes that differ from what was actually signed / from the accepted
`GovernanceId`. The correction, landed in `records.rs` + `authz.rs` + the e2e suite
(spec `v2-governance-records-verbatim-csb.md`):

- `GovernanceEntry` and `GovernanceApproval` now own the **exact CSB** they were
  constructed with as private correlated state (mirroring `crate::signed::Envelope`),
  exposed only through read-only accessors; body and CSB cannot be desynchronized after
  construction.
- Typed `new` constructors encode once and pin the vector they sign; trust-boundary
  `from_received_csb` constructors retain the supplied wire/storage bytes byte-for-byte.
- `verify_entry_crypto` / `verify_approval_crypto` verify the signature over the
  **retained** CSB *before* re-encoding the typed body; the post-signature re-encode
  equality is the semantic-canonicality check.
- Identity is derived from the retained CSB via `entry_id_from_csb` /
  `approval_id_from_csb`; `VerifiedGovernanceEntry::id()` exposes the authenticated
  identity, which the #148 accepted-tip (`authz.rs`) and chain links now bind to.

A normalizing `admin.set` carrying a signature valid only over the normalized bytes is
rejected as `bad_signature`; an exact signature over the normalizing bytes is rejected
post-signature as `non_canonical_encoding`. **No frozen wire byte, domain string, ID,
signature, or state-root vector changed** — the regression is covered by unit tests in
`records.rs` and the §9 verbatim-CSB e2e cases in `tests/v2_governance_log_e2e.rs`.
