# v2 Governance Records: Verify Verbatim Received CSB

| Field | Value |
|---|---|
| Issue | #178 — `[CORE] v2 governance records: verify over verbatim received CSB` |
| Labels | `type/feature`, `area/protocol`, `priority/p2`, `risk/medium` |
| Owning crate | `crates/iroh-rooms-v2-core` |
| Owning module | `governance::log::records` |
| Related work | #134 §§5.3–5.4, #147, #149, #140 acceptance item 4 |
| Status | Implemented in `governance/log/records.rs` (+ `authz.rs` accepted-tip, `tests/v2_governance_log_e2e.rs`). No frozen wire byte/domain/ID/signature changed; §5.3/§5.4 of `v2-governance-log-entry-approval-state-root.md` are amended accordingly. |

## 1. Summary

The normative v2 governance-log records currently discard the canonical signed bytes (CSB) received from wire or storage. `GovernanceEntry` and `GovernanceApproval` retain only decoded typed bodies, and their verification functions reconstruct CSB from those bodies before checking signatures and deriving identifiers.

That is an invalid trust boundary whenever typed decoding changes representation. The concrete trigger is `admin.set`: decoding sorts and deduplicates `administrators`. A valid deterministic-CBOR entry containing `[B, A]` or `[A, A, B]` therefore decodes to a typed body containing `[A, B]`. The current receiver drops the original bytes and verifies a signature over the normalized re-encoding. The authenticated bytes, received bytes, and accepted `GovernanceId` can consequently differ.

Refactor the normative `governance::log` record wrappers to own both:

- the exact CSB supplied by the sender/receiver constructor; and
- the corresponding typed body used by governance validation and state application.

Typed `new` constructors encode once, sign that vector, and retain it. Received constructors retain the supplied vector byte-for-byte while decoding the typed body from it. Entry and approval signatures, governance IDs, approval sort hashes, approval binding, and accepted governance tips must use retained CSB. Typed re-encoding remains a post-signature semantic-canonicality check and must never be the signature preimage.

This is a Rust ownership/API and verification-flow change. It does not add fields to either signed CBOR body, define a new outer wire envelope, alter frozen domains, or change the v1 governance path.

## 2. Repository context

### 2.1 Release and protocol posture

- `README.md` identifies `iroh-rooms-v2-core` as a pure, unpublished crate unused by the shipped runtime. Its encodings, domains, signatures, IDs, roots, and rejection codes are frozen by golden vectors.
- `governance::log` is the normative v2 governance path. The sibling candidate modules under `governance::{model, approval, checkpoint, fork, ...}` are not migration targets.
- `scripts/verify.sh` is the required workspace gate. CI invokes it on pull requests and pushes to `main`.
- No runtime, database, storage format, network protocol, CLI, SDK, or deployment migration is required.

### 2.2 Current record shapes

`crates/iroh-rooms-v2-core/src/governance/log/records.rs` currently has:

```rust
pub struct GovernanceApproval {
    pub body: GovernanceApprovalBody,
    pub signature: Signature,
}

pub struct GovernanceEntry {
    pub body: GovernanceEntryBody,
    pub signer: PrincipalId,
    pub signature: Signature,
    pub approvals: Vec<GovernanceApproval>,
}
```

`GovernanceEntry::new` and `GovernanceApproval::new` encode and sign a body but discard the resulting CSB. Verification then encodes the typed body again:

- `verify_entry_crypto` calls `entry_csb(&entry.body)` before signature verification;
- `verify_approval_crypto` calls `approval_csb(&approval.body)` before signature verification;
- `verify_governance_entry` calls `entry_id(&entry.body)` for approval binding;
- approval sorting calls `approval_id(&approval.body)`;
- `validate_and_apply_governance_entry` calls `entry_id(entry.body())` when advancing the accepted tip;
- e2e receivers decode `WireEntry.csb`, construct a body-only `GovernanceEntry`, and discard the wire CSB.

Fixing only the signature call would therefore leave identity, approval binding, accepted tips, and future fork evidence derived from reserialized bodies.

### 2.3 Correct architectural precedent

`crate::signed::Envelope` owns a verbatim `signed: Vec<u8>`. `signed::verify_envelope` canonical-decodes that exact vector, checks its claimed ID, and verifies its signature without reserializing first. Candidate `decode_verified` APIs delegate to this path.

The normative governance-log implementation must copy the byte-ownership principle, not the candidate envelope type or its legacy IDs/domains. In particular:

- normative entries currently have no separately claimed ID in their outer test wire shape;
- normative approvals have neither an outer claimed ID nor a detached signer because the approver is in the body;
- `signed::verify_envelope` itself does not enforce typed re-encode equality; the golden helper separately checks `to_csb(decoded) == received`.

The generic `signed` module and candidate `decode_verified` behavior remain unchanged by this issue.

### 2.4 Normalization trigger

`GovernanceOperationPayload::from_canonical` handles `admin.set` by sorting and deduplicating `administrators`. Its encoder emits the in-memory order. All of these can therefore be valid deterministic CBOR but represent the same typed payload after decode:

- received `[A, B]`;
- received `[B, A]`;
- received `[A, A, B]`.

This distinction is semantic canonicality, not CBOR syntax canonicality. `cbor::decode_canonical` correctly accepts deterministic arrays without imposing domain-specific sort/dedup rules.

## 3. Scope

### 3.1 In scope

1. Make `GovernanceEntry` retain exact entry-body CSB alongside its typed body.
2. Make `GovernanceApproval` retain exact approval-body CSB alongside its typed body.
3. Add explicit received-CSB constructors that preserve supplied bytes.
4. Make typed `new` constructors encode once and pin the bytes they sign.
5. Verify entry and approval signatures over retained CSB.
6. Derive entry identity and approval sort hashes from retained CSB.
7. Carry the exact verified entry ID into approval binding and accepted-tip advancement.
8. Enforce typed re-encode equality only after exact-byte signature verification.
9. Update all normative governance-log unit, authorization, golden, and e2e call sites.
10. Add normalize-during-decode negative regression vectors.

### 3.2 Out of scope

- Any v1 `governance::*` change.
- Candidate governance-envelope migration or generic `signed::Envelope` refactoring.
- Changes to body maps, fields, omission rules, operation names, domain strings, ID/signature formulas, or state-root formulas.
- A new serialized outer envelope or claimed entry/approval ID field.
- Fork detection or branch selection; #149 may consume the verified exact identity later.
- Authorization threshold or state-transition changes.
- Tightening `admin.set` to reject unsorted/duplicate arrays. Its existing normalization is the required regression trigger.
- New dependencies, logging, metrics, networking, persistence, or async work.

## 4. Requirements

### 4.1 Record invariants

1. A normative entry record owns exactly the entry-body CSB it was constructed with.
2. A normative approval record owns exactly the approval-body CSB it was constructed with.
3. The typed body and CSB cannot be independently mutated through safe public APIs.
4. Received construction must never replace input CSB with `entry_csb(decoded_body)` or `approval_csb(decoded_body)`.
5. Signer, signature, and approvals remain outside entry-body CSB.
6. Signature remains outside approval-body CSB; the approver remains inside `GovernanceApprovalBody`.
7. Approvals remain outside entry identity/signature and may still be supplied independently when constructing an entry.

### 4.2 Cryptographic and identity requirements

1. Entry signature input is exactly `domain::GOVERNANCE_ENTRY || entry.csb()`.
2. Entry identity is exactly `GovernanceId::from_governance_entry_csb(entry.csb())`.
3. Approval signature input is exactly `domain::GOVERNANCE_APPROVAL || approval.csb()`.
4. Approval sort hash is exactly `BLAKE3(domain::GOVERNANCE_APPROVAL || approval.csb())`.
5. Approval binding compares `approval.body.entry_id` with the exact-CSB-derived verified entry ID.
6. Accepted governance tips use the verified exact entry ID, not `entry_id(verified.body())`.
7. `verify_entry_crypto`, `verify_entry_full`, `verify_governance_entry`, and `verify_approval_crypto` must not call typed-body CSB helpers before their respective signature checks.
8. No governance record-layer `IdMismatch` path is added because the current normative outer record does not carry a claimed ID. A future outer envelope may compare a claimed ID separately.

### 4.3 Decode and round-trip requirements

1. A received constructor canonical-decodes the exact input and constructs the typed body from that decode.
2. It retains the original bytes even when typed construction normalizes their meaning.
3. Verification signs/checks the retained bytes before any typed-body reserialization.
4. After a successful signature check, verification requires:

   ```text
   entry_csb(entry.body()) == entry.csb()
   approval_csb(approval.body()) == approval.csb()
   ```

5. A mismatch is `Reject::NonCanonicalEncoding` because the record violates the semantic canonical representation expected by the typed model.
6. Existing closed-schema validation, unknown-operation rejection, `seq`/`prev` checks, kind/payload agreement, approval binding, duplicate rejection, authorization, chain linking, and state-root checks remain unchanged.

### 4.4 Compatibility requirements

The implementation must not change:

- `GovernanceEntryBody` or `GovernanceApprovalBody` CBOR;
- `domain::GOVERNANCE_ENTRY`, `domain::GOVERNANCE_APPROVAL`, or `domain::GOVERNANCE_STATE`;
- `GovernanceId` or approval hash derivation;
- Ed25519 signing-message construction;
- state-root derivation;
- existing `Reject::code()` values;
- candidate signed-record golden vectors.

Public Rust source compatibility may change to enforce body/CSB invariants. All repository call sites must migrate atomically.

## 5. Detailed design

### 5.1 Record data model

Use explicit private CSB ownership rather than embedding the candidate `signed::Envelope`:

```rust
pub struct GovernanceApproval {
    body: GovernanceApprovalBody,
    csb: Vec<u8>,
    signature: Signature,
}

pub struct GovernanceEntry {
    body: GovernanceEntryBody,
    csb: Vec<u8>,
    signer: PrincipalId,
    signature: Signature,
    approvals: Vec<GovernanceApproval>,
}
```

Do not cache or accept a claimed ID in these raw records. Deriving an ID from the retained vector is cheap, avoids a second correlated field, and matches the current normative wire shape.

Provide read-only accessors as needed:

```rust
impl GovernanceEntry {
    pub fn body(&self) -> &GovernanceEntryBody;
    pub fn csb(&self) -> &[u8];
    pub fn signer(&self) -> PrincipalId;
    pub fn signature(&self) -> &Signature;
    pub fn approvals(&self) -> &[GovernanceApproval];
}

impl GovernanceApproval {
    pub fn body(&self) -> &GovernanceApprovalBody;
    pub fn csb(&self) -> &[u8];
    pub fn signature(&self) -> &Signature;
}
```

Keep the public surface minimal. If a production caller needs to replace or attach approvals after entry construction, provide a controlled `with_approvals` or equivalent that changes only the detached approval collection; do not expose body/CSB mutation.

### 5.2 Byte-level derivation helpers

Add helpers in `governance/log/records.rs`:

```rust
pub fn entry_id_from_csb(csb: &[u8]) -> GovernanceId;
pub fn approval_id_from_csb(csb: &[u8]) -> [u8; LEN];
```

Use the existing frozen formulas. Keep `entry_csb`, `approval_csb`, `entry_id(body)`, and `approval_id(body)` for canonical typed construction and compatibility. Make typed ID helpers encode once and delegate to byte-level helpers.

Rules for call sites:

- typed local construction may use `entry_id(body)`;
- received or verified identity must use retained CSB or `VerifiedGovernanceEntry::id()`;
- approval sorting must use `approval_id_from_csb(approval.csb())`.

### 5.3 Typed constructors

`GovernanceEntry::new(body, secret, approvals)`:

1. Compute `csb = entry_csb(&body)` exactly once.
2. Build the signing message from `&csb`.
3. Sign it.
4. Store `body`, the same `csb` vector, `secret.member_id()`, signature, and approvals.

`GovernanceApproval::new(body, secret)`:

1. Compute `csb = approval_csb(&body)` exactly once.
2. Build the signing message from `&csb`.
3. Sign it.
4. Store `body`, the same `csb` vector, and signature.

Preserve the current infallible approval constructor. If the key does not match `body.approver`, later verification returns `BadSignature`; do not rewrite the approver field.

### 5.4 Received constructors

Add public trust-boundary constructors:

```rust
pub fn GovernanceEntry::from_received_csb(
    csb: Vec<u8>,
    signer: PrincipalId,
    signature: Signature,
    approvals: Vec<GovernanceApproval>,
) -> Result<Self, Reject>;

pub fn GovernanceApproval::from_received_csb(
    csb: Vec<u8>,
    signature: Signature,
) -> Result<Self, Reject>;
```

Each constructor:

1. calls the existing strict decode path on the supplied slice;
2. stores the resulting typed body;
3. retains the supplied `Vec<u8>` unchanged;
4. stores detached fields unchanged;
5. performs no typed re-encoding;
6. does not claim that cryptographic verification has occurred.

An optional borrowed-slice convenience may copy the input, but the owned-vector form should be the primary wire/storage API to avoid unnecessary copies.

Malformed/non-deterministic CBOR and invalid typed schema fail in the constructor with the existing decode error. This two-phase API is intentional: typed decoding is needed to form these record types, and approvals need the decoded `approver` key before signature verification. Authentication still uses the retained bytes, never a re-encoding.

### 5.5 Entry verification

Refactor `verify_entry_crypto` so the effective pipeline is:

1. Obtain `received = entry.csb()`.
2. Build `domain::signing_message(domain::GOVERNANCE_ENTRY, received)`.
3. Verify with `entry.signer()`; return `BadSignature` on failure.
4. Encode `entry.body()` only now and compare it byte-for-byte with `received`.
5. Return `NonCanonicalEncoding` if typed re-encoding differs.
6. Return the verified body on success.

The received constructor has already canonical/typed-decoded `received`; the verifier may defensively decode retained bytes again if desired, provided it does not serialize before step 3 and checks consistency with the private stored body.

The primary negative must therefore return `BadSignature`: an altered received CSB paired with a signature valid only over normalized bytes fails at step 3 before the round-trip check.

### 5.6 Approval verification

Refactor `verify_approval_crypto` analogously:

1. Obtain `received = approval.csb()`.
2. Use `approval.body().approver` as the verification key.
3. Verify `domain::GOVERNANCE_APPROVAL || received`; return `BadSignature` on failure.
4. Re-encode the typed approval body only after signature success.
5. Require byte equality with `received`; return `NonCanonicalEncoding` on mismatch.
6. Return the verified approval body.

There is no known normalization-equivalent alternate encoding for the current approval body. Approval tests must still prove byte preservation and exact-byte signature use; do not invent an impossible normalization vector.

### 5.7 Full entry verification and exact identity

Extend the unforgeable verified wrapper:

```rust
pub struct VerifiedGovernanceEntry {
    id: GovernanceId,
    body: GovernanceEntryBody,
    signer: PrincipalId,
    approvals: Vec<GovernanceApprovalBody>,
}
```

Add `id(&self) -> GovernanceId`. Full verification must:

1. verify entry crypto over retained CSB;
2. derive `verified_entry_id = entry_id_from_csb(entry.csb())`;
3. clone and sort approvals by:

   ```text
   (approval.body().approver.as_bytes(),
    approval_id_from_csb(approval.csb()))
   ```

4. verify each approval over its retained CSB;
5. bind its community, `entry_id`, and state root to the verified entry body and `verified_entry_id`;
6. reject duplicate verified approvers;
7. return sorted approval bodies plus `verified_entry_id` in `VerifiedGovernanceEntry`.

Preserve current sort-before-verification behavior unless a separate protocol decision changes it. The sort hash must still use retained CSB. Entry crypto failure precedes all approval work; approval crypto/round-trip failure precedes approval binding and duplicate checks.

`verify_entry_full` remains a compatibility wrapper returning only the body, but its implementation must delegate to this exact-byte path. New identity-sensitive consumers must use `verify_governance_entry` and `VerifiedGovernanceEntry::id()`.

### 5.8 Authorization and chain consumers

Update `governance/log/authz.rs`:

- `validate_and_apply_governance_entry` sets `GovernanceTip::Entry.id` to `entry.id()`;
- test assertions compare the tip with `verified.id()`;
- typed helpers that create a new local approval may continue using `entry_id(&body)` because `GovernanceEntry::new` pins that same canonical encoding.

Update e2e fold helpers to return the ID from the fully verified entry, not `entry_id(&verified_body)`. This prevents future chain links and #149 fork evidence from silently reverting to normalized-body identity.

## 6. Error model and precedence

No new rejection variant or code is required.

| Condition | Expected result |
|---|---|
| malformed or non-deterministic received CBOR | Existing constructor decode error, normally `NonCanonicalEncoding` |
| unknown operation or invalid body content | Existing typed decode error |
| entry signature invalid for exact retained entry CSB | `BadSignature` |
| approval signature invalid for exact retained approval CSB | `BadSignature` |
| exact signature valid but typed body re-encodes differently | `NonCanonicalEncoding` |
| approval community, exact entry ID, or state-root mismatch | `InvalidApproval` |
| duplicate approver | `InvalidApproval` |
| later authorization or state-root failure | Existing `InsufficientAuthorization`, `StateRootMismatch`, or other current result |

Pin these precedence rules:

1. Received-constructor decode errors occur before a record can be verified.
2. Entry signature failure occurs before entry round-trip comparison and approval processing.
3. Approval signature failure occurs before approval round-trip comparison and binding.
4. For altered `admin.set` CSB with a normalized-only signature, the result is `BadSignature`.
5. For the same altered CSB signed over its exact bytes, signature verification succeeds and the post-signature result is `NonCanonicalEncoding`.
6. Do not add governance-record `IdMismatch` without an actual claimed-ID field.

Update stale `records.rs` documentation that says approval signature failure is `InvalidApproval`; current code and tests expose `BadSignature`.

## 7. Implementation plan

### Step 1 — Add exact-CSB helpers

In `governance/log/records.rs`:

1. Add `entry_id_from_csb` and `approval_id_from_csb`.
2. Make typed ID helpers delegate to them after one encoding.
3. Add unit assertions that canonical typed-body and byte-level derivations agree.
4. Keep all domains and return types unchanged.

### Step 2 — Refactor `GovernanceApproval`

1. Add private `csb: Vec<u8>`.
2. Make body/CSB correlated state private and add read-only accessors.
3. Update `new` to encode once and sign/store the same vector.
4. Add `from_received_csb` with exact byte retention.
5. Refactor `verify_approval_crypto` to verify retained bytes before re-encoding.
6. Update approval sorting to use `approval_id_from_csb(approval.csb())`.
7. Migrate negative tests that mutate `signature` to construct a received record with the alternate signature.

### Step 3 — Refactor `GovernanceEntry`

1. Add private `csb: Vec<u8>`.
2. Make body/CSB correlated state private and add read-only accessors.
3. Update `new` to encode once and sign/store the same vector.
4. Add `from_received_csb`.
5. Keep approvals outside entry CSB.
6. Refactor `verify_entry_crypto` to verify retained bytes before re-encoding.
7. Migrate direct field access and signature-mutation tests to accessors and constructors.

### Step 4 — Carry exact identity through verification

1. Add `id` and `id()` to `VerifiedGovernanceEntry`.
2. Derive it from `entry.csb()` only after entry crypto/round-trip success.
3. Use it for approval binding.
4. Sort approvals with exact-CSB approval hashes.
5. Preserve sorted, duplicate-free approval-body output.
6. Keep `verify_entry_full` as a body-only compatibility wrapper.

### Step 5 — Update authorization and raw-wire receivers

1. Replace `entry_id(entry.body())` in accepted-tip advancement with `entry.id()`.
2. Audit every normative `entry_id(body)` call; retain it only for canonical typed construction, never received/verified identity.
3. In `v2_governance_log_e2e.rs`, keep the existing `WireEntry { csb, signer, sig, approvals }` shape; do not add an ID field.
4. Change `receive_and_fold` and `receive_and_authorize` to call `GovernanceEntry::from_received_csb` directly.
5. Return/use the verified exact ID for next-link `prev` values.
6. Add corresponding received construction for any raw approval fixture.

### Step 6 — Update documentation and exports

1. Correct `records.rs` module documentation to describe ownership of exact received CSB.
2. Correct the stale statement that `verify_entry_crypto` sorts approvals.
3. Correct approval-signature error documentation to `BadSignature`.
4. Re-export only byte helpers or constructors that callers actually need.
5. Document that #149 can consume the authenticated ID later but fork logic is not implemented here.

## 8. Test strategy

### 8.1 Unit tests in `records.rs`

Add or migrate tests covering:

- typed entry construction pins `entry_csb(body)` and verifies successfully;
- typed approval construction pins `approval_csb(body)` and verifies successfully;
- received entry construction retains input bytes exactly;
- received approval construction retains input bytes exactly;
- entry signature checks retained CSB;
- approval signature checks retained CSB;
- exact-CSB entry ID helper agrees with canonical typed construction;
- exact-CSB approval sort hash agrees with canonical typed construction;
- approval binding uses the exact verified entry ID;
- `VerifiedGovernanceEntry::id()` exposes the exact-CSB identity;
- exact approval sorting remains deterministic across caller order;
- duplicate approvers remain `InvalidApproval`;
- existing bad-signature and binding tests still return their current codes;
- exact altered normalizing entry bytes with normalized-only signature return `BadSignature`;
- exact altered normalizing entry bytes signed directly return post-signature `NonCanonicalEncoding`.

### 8.2 Required normalize-during-decode construction

Build the regression without using `entry_csb(decoded_body)` for the record under test:

1. Choose deterministic principals `A < B` and public test keys.
2. Construct a valid typed `admin.set` body with sorted unique `[A, B]`.
3. Compute `normalized_csb = entry_csb(&body)` and sign it.
4. Independently edit the body `CborValue` so only `payload.administrators` becomes `[B, A]` or `[A, A, B]`.
5. Deterministically encode that value as `received_csb`.
6. Assert `received_csb != normalized_csb`.
7. Assert both CSBs decode to the same normalized typed body.
8. Assert `entry_id_from_csb(received_csb) != entry_id_from_csb(normalized_csb)`.
9. Construct the record with `from_received_csb(received_csb, signer, normalized_signature, ...)`.
10. Assert `verify_entry_crypto` and the full receiver path return `BadSignature`.
11. Sign `received_csb` itself, construct a second received record, and assert `NonCanonicalEncoding` after signature verification.
12. Cover both unsorted and duplicate arrays if practical; at least one form is mandatory.

The first case fails on the current implementation and directly fences the vulnerability. The second preserves the verbatim decode/re-encode guarantee.

### 8.3 E2E tests

In `tests/v2_governance_log_e2e.rs`:

1. Migrate all wire receivers to `from_received_csb`.
2. Preserve positive genesis-to-multi-entry fold, operation registry, authorization, chain, and state-root coverage.
3. Add the normalize-during-decode negative through only public APIs.
4. Prove exact received bytes are retained by the reconstructed record.
5. Prove the normalized-only signature is rejected before authorization/state application.
6. Prove next-link identity in positive folds equals the exact wire-CSB-derived ID.

### 8.4 Golden regression

Add frozen normative `governance::log` regression data for:

- normalized canonical CSB;
- altered deterministic received CSB;
- signer;
- signature valid only over normalized CSB;
- both CSB-derived governance IDs;
- expected `bad_signature`;
- preferably, exact-altered-CSB signature with expected `non_canonical_encoding`.

Reconstruct `GovernanceEntry` from those frozen fields through `from_received_csb`; do not rebuild the record under test from the logical body.

The current aggregate signed-record positives are candidate envelopes and must remain untouched. Preferred fixture handling:

- add a dedicated normative governance-log regression fixture under `tests/golden/` and document it in `tests/golden/README.md`; or
- add an explicitly named regression section to the aggregate fixture if maintainers prefer one file.

If the aggregate fixture schema changes, bump its schema marker and add a change-log note. Any change to an existing frozen CSB, ID, signature, root, domain, or rejection code always requires the documented schema bump and protocol-change note. Never regenerate existing vectors to accommodate this API refactor.

### 8.5 Verification commands

Run focused checks, then the full gate:

```bash
cargo fmt --all --check
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
cargo test -p iroh-rooms-v2-core --lib governance::log::records --all-features
cargo test -p iroh-rooms-v2-core --test v2_governance_log_e2e --all-features
cargo test -p iroh-rooms-v2-core --test signed_records_golden --all-features
cargo test -p iroh-rooms-v2-core --test taxonomy --all-features
cargo test -p iroh-rooms-v2-core --test banned_dependencies --all-features
cargo test -p iroh-rooms-v2-core --all-targets --all-features
scripts/verify.sh
```

Mechanically review the golden diff after tests. Existing frozen values must be byte-for-byte unchanged.

## 9. Acceptance criteria

### 9.1 Issue acceptance mapping

| Issue acceptance item | Evidence |
|---|---|
| Normalizing body is verified against received bytes; normalized-only signature is rejected | Unit, golden, and public e2e cases construct altered CSB directly and return `BadSignature`. |
| `verify_entry_full`, `verify_entry_crypto`, and `verify_approval_crypto` never reserialize before signature check | Implementations use record `csb()` for signing messages; post-signature equality is separately tested. |
| Golden and e2e cover normalize-during-decode negative | Frozen normative regression and `v2_governance_log_e2e` public-boundary test. |
| `scripts/verify.sh` green; no frozen drift | Full gate output and golden diff review; schema process used only where required. |

### 9.2 Complete checklist

- [ ] Both normative record types retain verbatim CSB alongside typed bodies.
- [ ] Safe public APIs cannot desynchronize body and CSB.
- [ ] Typed constructors encode once and sign/store the same vector.
- [ ] Received constructors preserve supplied vectors byte-for-byte.
- [ ] Entry signatures verify against retained entry CSB.
- [ ] Approval signatures verify against retained approval CSB.
- [ ] Entry identity derives from retained entry CSB.
- [ ] Approval sorting hashes retained approval CSB.
- [ ] Approval binding uses the exact verified entry ID.
- [ ] Accepted governance tips and subsequent links use `VerifiedGovernanceEntry::id()`.
- [ ] Typed re-encoding occurs only after signature verification.
- [ ] Normalized-only signature over altered received bytes returns `BadSignature`.
- [ ] Exact signature over semantically normalizing received bytes returns `NonCanonicalEncoding`.
- [ ] Existing approval order, duplicate, binding, authorization, chain, and root behavior remains green.
- [ ] No body, outer wire, domain, formula, v1, or runtime behavior changes.
- [ ] Existing frozen vectors do not drift.
- [ ] Focused tests and `scripts/verify.sh` pass.

## 10. Security, privacy, reliability, and performance

### Security

The exact representation received becomes the authenticated representation and the source of identity. Private body/CSB correlation prevents safe callers from recreating the original weakness after construction. Approval binding and accepted-tip updates must be part of the same change; a signature-only patch is incomplete.

### Privacy and observability

No new user data is introduced. The pure core continues to return typed `Reject` values without logs or metrics. Do not include CSB, signature, or key material in errors. Tests use only documented deterministic public seeds.

### Reliability

Retaining CSB supports forwarding, audit, and future fork evidence. Post-signature re-encode equality rejects signed representations that the typed model would silently normalize. The boundary remains correct even if another payload decoder later gains normalization behavior.

### Performance

Each record stores one CSB vector in addition to its typed body. Typed construction encodes once. Received construction owns the wire vector. Verification performs a post-signature encode for semantic equality. Approval sort hashing reads retained bytes rather than repeatedly encoding bodies. This bounded overhead is appropriate for the pure v2 core and requires no new dependency.

## 11. Rollout and rollback

Land record refactoring, verifier changes, identity propagation, call-site migration, and regressions as one coherent change. Partial rollout is unsafe because body-derived identity in approval binding or accepted tips would retain part of the vulnerability.

The v2 crate is unpublished and unused by the shipped runtime, so rollback is a code-and-test revert with no data migration. If any existing frozen value changes unexpectedly, stop and restore the prior frozen derivation rather than blessing drift.

## 12. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Entry signature fixed but tip/binding still uses typed-body ID | High | Carry exact ID in `VerifiedGovernanceEntry`; audit all `entry_id(body)` calls. |
| Approval path remains body-derived | High | Retain approval CSB; sign and sort-hash it directly. |
| Received constructor stores a re-encoding | High | Preserve input ownership and assert byte equality in unit/e2e tests. |
| Body and CSB can diverge after construction | High | Private correlated fields and read-only accessors. |
| Round-trip check runs before signature or is removed | Medium | Pin both `BadSignature` and post-signature `NonCanonicalEncoding` cases. |
| `admin.set` is tightened instead of fixing envelope boundary | High | Keep normalization behavior and independent altered-CSB regression. |
| Candidate IDs/domains leak into normative log | High | Use existing normative helpers/domains; do not wrap candidate `Envelope`. |
| Golden regeneration hides drift | High | Preserve old vectors; require explicit fixture policy for changes. |
| API migration prevents approval collection | Medium | Keep approvals detached from CSB and provide a controlled replacement/builder only if needed. |
| Additional memory allocation | Low | Own one vector per record and prefer consuming received constructors. |

## 13. Assumptions

1. The inline issue is the authoritative scope.
2. `governance::log` is the only target.
3. The current `WireEntry` shape reflects the available normative outer fields and has no claimed ID.
4. Entry and approval body CSB formulas and normative domains are frozen and correct.
5. `BadSignature` remains the public result for approval signature failure.
6. Signed received bytes that normalize to a different typed re-encoding remain invalid.
7. `admin.set` sort/dedup remains unchanged for this issue.
8. `VerifiedGovernanceEntry` needs the authenticated ID now; retaining a second owned CSB in the verified wrapper is not required because the source record already owns it.
9. No deployed v2 persistence or mixed-version migration exists.

## 14. Open questions

No question blocks the trust-boundary repair. One repository-convention decision remains for implementation review:

1. Should the new normative golden regression be a dedicated fixture or a new section in `v2-signed-records.json`? Prefer a dedicated fixture to avoid conflating normative `governance::log` records with candidate envelope positives; follow the golden README's schema/change-log policy if the aggregate is extended.
