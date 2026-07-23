# Spec: v2 Golden Vectors for Every Signed Record

| | |
|---|---|
| **Issue** | #153 — `[TEST] v2 golden vectors for every signed record` |
| **Labels** | `type/test` `area/protocol` `priority/p1` `risk/low` |
| **Refs** | #134 §1, #134 §6.4, #146, #147, #151, #152; `specs/v2-crypto-core-crate.md`; `specs/content-and-moderation-event-schemas.md` |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Primary test/fixture locations** | `crates/iroh-rooms-v2-core/tests/golden/` and `crates/iroh-rooms-v2-core/tests/signed_records_golden.rs` |
| **Status** | Planning only. Do not implement production code in this issue. |

---

## 1. Summary

Add a frozen v2 golden-vector fixture suite for every signed record and domain-separated hash boundary currently produced by the v2 crypto core. The fixture suite must pin byte-exact deterministic-CBOR encodings, BLAKE3 domain-separated identifiers, Ed25519 signatures, round-trip behavior, and typed rejection outcomes so v2 interoperability cannot be claimed while wire encodings or domain strings are still moving.

This is a **test-only** change. It must not alter production logic, protocol semantics, runtime wiring, networking, storage, SDK exports, or Git/GitHub state.

The suite must cover at least:

- `CommunityId` / `RoomId` derivation;
- `GovernanceEntry` / `GovernanceEntryBody` signed record;
- `GovernanceApproval` / `ApprovalBody` signed record;
- `GovernanceCheckpoint` / `CheckpointBody` signed record;
- `MemberRecord` / `MemberLeaf` canonical projection fixture;
- `ContentEvent` / `ContentEventBody` signed record;
- `SignedForkResolution` / `ForkResolutionBody` signed record (present in code with its own `FORK_RESOLVE_*` domains);
- every domain-separated hash used by those records;
- every #134 §6.4 rejection rule represented in the current v2 core taxonomy;
- encode → decode → re-encode byte identity for every fixture record.

---

## 2. Repository context read

Relevant local context:

- `README.md` states that `crates/iroh-rooms-v2-core/` is the pure v2 crypto core: canonical CBOR, BLAKE3/Ed25519 IDs, governance state machine, fork detection, and member Merkle map. It is unused by the shipped runtime in this phase.
- `specs/v2-crypto-core-crate.md` is the parent implementation plan for #140 and explicitly lists #153 as golden vectors for every signed record and every hash/root boundary. It requires deterministic fixtures, no network/store dependencies, and CI commands including `cargo test -p iroh-rooms-v2-core --all-targets --all-features`.
- `crates/iroh-rooms-v2-core/src/lib.rs` documents the trust boundary: typed body → deterministic-CBOR CSB → domain-separated signing message → domain-separated ID → envelope preserving CSB verbatim.
- `crates/iroh-rooms-v2-core/src/domain.rs` centralizes candidate domain strings from #146. Changing these strings is protocol-breaking and must break golden tests.
- `crates/iroh-rooms-v2-core/src/cbor.rs` implements the closed deterministic-CBOR profile and rejects indefinite lengths, non-shortest integers, tags/floats/simple values, negative integers, non-text keys, unsorted/duplicate map keys, invalid UTF-8, excessive depth, length overflow, and trailing data.
- `crates/iroh-rooms-v2-core/src/signed.rs` provides the generic signed-record envelope and `verify_envelope` path. It currently verifies canonicality, ID, signature, and body-specific validation.
- `crates/iroh-rooms-v2-core/src/error.rs` exposes the typed `Reject` taxonomy and stable `.code()` strings. The current public codes are:
  - `non_canonical_encoding`
  - `unknown_version`
  - `unknown_record_kind`
  - `unknown_content_kind`
  - `invalid_content`
  - `id_mismatch`
  - `bad_signature`
  - `wrong_domain`
  - `missing_dependency`
  - `insufficient_authorization`
  - `invalid_approval`
  - `fork_detected`
  - `unresolved_fork`
  - `invalid_fork_resolution`
  - `state_root_mismatch`
  - `snapshot_hash_mismatch`
  - `invalid_merkle_proof`
- `crates/iroh-rooms-v2-core/tests/taxonomy.rs` currently checks that the taxonomy is named and stable, but it does not provide fixture-level negative vectors for every rejection path.
- `crates/iroh-rooms-v2-core/tests/room_lifecycle.rs` and `tests/governance_state_machine.rs` provide deterministic lifecycle coverage but do not freeze reviewable byte fixtures.
- `crates/iroh-rooms-v2-core/tests/golden/` and `fixtures/` do not currently exist in this checkout.

---

## 3. Scope

### 3.1 In scope

1. Add frozen fixture files under `crates/iroh-rooms-v2-core/tests/golden/`.
2. Add test code that loads those fixtures and asserts the current implementation exactly reproduces them.
3. Add positive golden vectors for every signed record type named in issue #153:
   - Community/room ID derivation vector;
   - Governance entry vector;
   - Governance approval vector;
   - Governance checkpoint vector;
   - Member record/projection vector;
   - Content event vector;
   - Fork-resolution vector.
4. Add negative vectors for every current #134 §6.4-style rejection rule represented by `Reject` codes and body validation checks.
5. Add round-trip vectors asserting `encode → decode → re-encode` produces identical bytes for every signed record and every standalone canonical object in the fixtures.
6. Add a documentation fixture declaring the vectors frozen and requiring an explicit schema-version bump for any intentional change.
7. Ensure a changed domain string or canonical-CBOR rule breaks at least one test.
8. Keep all fixtures deterministic and seed-derived; no entropy, secrets, network addresses, or real user data.

### 3.2 Out of scope

- Wire-transport golden vectors: no v2 ALPN or transport exists yet.
- Replica-receipt vectors: no receipt type exists in Track 2 scope.
- New production code or protocol behavior.
- Store, network, CLI, SDK, deployment, migration, GitHub, or release work.
- Closing or commenting on GitHub issues.

---

## 4. Key decisions

### D1 — Store fixtures under `crates/iroh-rooms-v2-core/tests/golden/`

Use the issue-preferred crate-local location because these fixtures belong to the v2 pure core and should run with `cargo test -p iroh-rooms-v2-core`. Do not place them at repository root unless a later cross-language conformance harness needs shared fixtures.

Recommended layout:

```text
crates/iroh-rooms-v2-core/tests/
  signed_records_golden.rs
  golden/
    README.md
    v2-signed-records.json
    positive/
      community-id.json
      governance-entry.json
      governance-approval.json
      governance-checkpoint.json
      member-record.json
      content-event.json
      fork-resolution.json
    negative/
      non-canonical-encoding.json
      unknown-version.json
      unknown-record-kind.json
      unknown-content-kind.json
      invalid-content.json
      id-mismatch.json
      bad-signature.json
      wrong-domain.json
      missing-dependency.json
      insufficient-authorization.json
      invalid-approval.json
      fork-detected.json
      unresolved-fork.json
      invalid-fork-resolution.json
      state-root-mismatch.json
      snapshot-hash-mismatch.json
      invalid-merkle-proof.json
```

A single aggregate JSON file is acceptable if implementers prefer fewer files, but each vector must still have a stable `name`, `record_type`, `expected`, and `frozen` marker.

### D2 — Use reviewable JSON carrying canonical bytes as lowercase hex

Fixture JSON should be hand-reviewable and language-neutral. Store all byte sequences as lowercase hex strings with no `0x` prefix. Store named IDs in their public presentation form where applicable (`blake3:<64-hex>`), and also store raw digest hex where a byte-exact hash input/output is being pinned.

Recommended per-vector schema:

```json
{
  "schema": "iroh-rooms-v2-golden-vectors/v1",
  "frozen": true,
  "requires_schema_bump_on_change": true,
  "vectors": [
    {
      "name": "governance-entry-init-room-v1",
      "issue": 153,
      "record_type": "governance_entry",
      "positive": true,
      "seed_keys": {
        "admin_seed_hex": "a0a0...",
        "member_seed_hex": "b0b0..."
      },
      "domains": {
        "sign": "iroh-rooms:v2:governance-entry:sign:v1",
        "id": "iroh-rooms:v2:governance-entry:id:v1"
      },
      "logical": { "...": "reviewable logical fields" },
      "canonical_cbor_hex": "...",
      "id": "blake3:...",
      "signer_hex": "...",
      "signature_hex": "...",
      "round_trip_canonical_cbor_hex": "...",
      "expected_result": "ok"
    }
  ]
}
```

### D3 — Canonical signed bytes are the primary frozen artifact

For signed records, tests must compare exact CSB bytes before comparing IDs or signatures. This ensures any deterministic-CBOR ordering, omission, integer width, string length, byte-string length, or field-name change is caught independently of cryptographic outputs.

Each signed-record positive vector must assert:

1. generated CSB equals fixture `canonical_cbor_hex`;
2. strict decode succeeds;
3. re-encoding the decoded value equals the exact same bytes;
4. domain-separated ID equals fixture ID;
5. domain-separated signing message is `sign_domain || csb`;
6. Ed25519 signature equals fixture signature;
7. full `decode_verified` / `verify_envelope` succeeds and returns the expected logical body.

### D4 — Pin all domain strings explicitly

Every fixture that hashes or signs must carry the exact domain strings it expects. Tests must assert the fixture value equals the constant in `src/domain.rs` before using it.

Minimum domains to pin:

| Boundary | Constant |
|---|---|
| Governance entry signature | `GOVERNANCE_ENTRY_SIGN` |
| Governance entry ID | `GOVERNANCE_ENTRY_ID` |
| Governance approval signature | `GOVERNANCE_APPROVAL_SIGN` |
| Governance approval ID | `GOVERNANCE_APPROVAL_ID` |
| Content event signature | `CONTENT_EVENT_SIGN` |
| Content event ID | `CONTENT_EVENT_ID` |
| Community/room ID derivation | `ROOM_ID` |
| Governance state root | `GOVERNANCE_STATE_ROOT` |
| Checkpoint signature | `CHECKPOINT_SIGN` |
| Snapshot hash | `SNAPSHOT_HASH` |
| Merkle empty node | `MERKLE_EMPTY` |
| Merkle leaf | `MERKLE_LEAF` |
| Merkle internal node | `MERKLE_NODE` |
| Merkle key | `MERKLE_KEY` |
| Fork resolution signature | `FORK_RESOLVE_SIGN` |
| Fork resolution ID | `FORK_RESOLVE_ID` |

This is the acceptance fence: changing a domain string must fail one or more golden tests.

### D5 — Treat `CommunityId` as current `RoomId` unless #146 renames it

The issue names `CommunityId derivation`; current code exposes `RoomId` and `domain::ROOM_ID`. Until #146/#134 lands a distinct `CommunityId` type, the positive vector should be named `community-id-room-id-derivation` and document the alias explicitly:

- logical name: `CommunityId`;
- current Rust type: `RoomId`;
- domain constant: `ROOM_ID`.

If #146 later introduces a dedicated `CommunityId`, update the vector names and test type while preserving the frozen byte/hash expectation or bumping schema version if semantics changed.

### D6 — MemberRecord is a projection/leaf vector, not a signed envelope, in current code

Issue #153 lists `MemberRecord` alongside signed record types. Current code has:

- `governance::model::MemberRecord` as folded state;
- `member::projection::MemberLeaf` as the canonical Merkle-projection object;
- no separate `SignedMemberRecord` envelope.

Therefore the implementation should include a `member-record` positive vector that pins:

- `MemberRecord` logical fields;
- projected `MemberLeaf` canonical CBOR bytes;
- Merkle key;
- value hash;
- leaf hash;
- resulting member root for a one-member projection;
- inclusion proof and round-trip proof verification.

If #151 later adds a separately signed member record, add a distinct `signed_member_record` vector and keep this projection vector as the Merkle boundary fixture.

### D7 — Negative vectors must assert typed reasons, not just failure

Every negative vector must specify `expected_reject_code`. Tests must compare against `Reject::code()` and fail if the implementation returns a different typed reason.

For layered validation where more than one failure could apply, the fixture must be built so exactly one primary failure is triggered. Example: for `bad_signature`, keep CSB canonical and ID correct, then flip one signature byte. For `id_mismatch`, keep signature valid over the CSB and alter only the envelope ID.

### D8 — Fixture generation must be deterministic and auditable

Use seed-derived keys only, e.g. `[0xa0; 32]` for admin, `[0xb0; 32]` for member, `[0xc0; 32]` for approver, `[0xd0; 32]` for content author. Store these as public test seeds in fixture metadata and document they are non-secret.

Do not use `SigningKey::generate()` in fixture generation. Do not include real room names, user names, network addresses, endpoint IDs, ticket material, or private data.

---

## 5. Implementation plan

### Step 1 — Add the golden fixture directory and frozen-vector documentation

1. Create `crates/iroh-rooms-v2-core/tests/golden/`.
2. Add `crates/iroh-rooms-v2-core/tests/golden/README.md` explaining:
   - vectors are frozen interoperability fixtures;
   - changing any vector requires an explicit v2 schema-version bump or a documented protocol-breaking decision;
   - fixtures use deterministic public test seeds only;
   - fixtures cover signed-record CSB, domain-separated IDs/hashes, signatures, rejection codes, and round trips;
   - transport and replica receipts are intentionally out of scope.
3. Do not modify production modules.

### Step 2 — Add a fixture loader test

1. Add `crates/iroh-rooms-v2-core/tests/signed_records_golden.rs`.
2. Use only dependencies already available to the crate. If `serde_json` is not currently present, choose one of these approaches:
   - preferred: add `serde_json` as a dev-dependency only if repository policy allows;
   - fallback: use `include_str!` plus minimal fixture parsing helpers, or split fixtures into Rust constants in the test file while keeping the documentation JSON checked in.
3. Load fixture files with `include_str!` / `include_bytes!` so missing fixtures fail at compile/test time.
4. Provide helpers:
   - `hex_to_bytes` with lowercase-only validation;
   - `assert_domain(name, fixture, constant)`;
   - `assert_csb_round_trip(csb_hex)`;
   - `assert_signature_hex`;
   - `assert_reject_code(result, expected_code)`.

### Step 3 — Create deterministic positive vectors

For each vector, construct the logical body using current public types, compute CSB via `signed::to_csb` or `cbor::encode`, and freeze the exact outputs in JSON.

#### 3.1 Community/Room ID derivation

Fixture name: `community-id-room-id-derivation-v1`.

Pin:

- domain string `ROOM_ID`;
- deterministic input tuple used by current derivation helper if one exists;
- if no public derivation helper exists, pin `domain::blake3_domain(domain::ROOM_ID, canonical_community_seed_payload)` as the current derivation boundary and mark the exact payload in the fixture;
- resulting `RoomId` / `CommunityId` named string.

Implementation note: current code may not expose a high-level `derive_room_id` helper. If so, the test should exercise the public domain helper and document that #146 may later add a type-level helper without changing the vector semantics.

#### 3.2 GovernanceEntry

Fixture name: `governance-entry-init-room-v1`.

Use an `InitRoom` entry with:

- `schema_version = 2`;
- fixed `room_id` from the community vector or `[0x70; 32]` if no derivation helper exists;
- admin key seed `[0xa0; 32]`;
- `seq = 1`;
- `parent = None`;
- fixed `epoch`;
- `room_name = "golden-room"`.

Pin CSB, ID, signer public key, signature, and decoded logical fields.

#### 3.3 GovernanceApproval

Fixture name: `governance-approval-add-member-v1`.

Use:

- approver key seed `[0xc0; 32]`;
- fixed entry ID from the governance entry or an add-member entry fixture;
- optional `proposed_state_root` set to a deterministic state root from folding the genesis/add-member state;
- fixed epoch.

Pin CSB, approval ID, signer, signature, proposed root, and verification result.

#### 3.4 GovernanceCheckpoint

Fixture name: `governance-checkpoint-clean-state-v1`.

Build a deterministic folded state:

1. admin genesis;
2. add one member;
3. project members;
4. compute `state_root` and `member_root`;
5. create `CheckpointBody` with no unresolved forks.

Pin:

- checkpoint CSB;
- checkpoint envelope ID / snapshot hash;
- signature;
- state root;
- member root;
- `unresolved_forks = []`;
- `validate_against_state` succeeds.

#### 3.5 MemberRecord / MemberLeaf

Fixture name: `member-record-active-member-leaf-v1`.

Pin:

- logical `MemberRecord` fields (`member_id`, role, status, device keys, governance cursor);
- projected `MemberLeaf` canonical CBOR hex;
- Merkle map key;
- value hash;
- leaf hash;
- one-member root;
- inclusion proof hex/shape;
- exclusion proof for one absent member, if proof API exposes a stable encoding.

If `MerkleProof` does not yet expose canonical serialization, pin its logical fields and root-verification result, and add an open question to add proof-byte vectors when #151 exposes a stable proof wire shape.

#### 3.6 ContentEvent

Fixture name: `content-event-message-text-v1`.

Use:

- author key seed `[0xb0; 32]`;
- `schema_version = 2`;
- same fixed room ID;
- `kind = ContentKind::MessageText`;
- `version = 1`;
- `stream_id = None`;
- body map `{ "body": "hello golden v2" }`.

Pin CSB, content-event ID, signature, author, body validation result, and full decode result.

#### 3.7 SignedForkResolution

Fixture name: `fork-resolution-accept-winner-v1`.

`governance::fork::SignedForkResolution` (`Envelope<SnapshotHash>` over a
`ForkResolutionBody`) is a real signed-record type with its own domain
constants (`FORK_RESOLVE_SIGN`, `FORK_RESOLVE_ID`) and its own `decode_verified`
path. Issue #153 enumerates "each domain-separated hash", so the fork-resolve
signing/id boundaries require a positive vector even though the issue's prose
list did not name the type explicitly.

Build a deterministic accepted resolution:

1. construct two conflicting entries by the same author at the same
   `seq`/`parent` (the fork evidence pair);
2. resolver key seed `[0xe0; 32]`;
3. `ForkResolveAction::Accept { winner }` where `winner` is one of the evidence
   pair, using the exact evidence ordering the current `ForkResolutionBody`
   validation requires;
4. fixed epoch/room id consistent with the other vectors.

Pin:

- fork-resolution CSB;
- envelope ID (the `SnapshotHash`-typed fork-resolve id);
- resolver signer public key;
- signature;
- the evidence pair ids;
- `winner`;
- `decode_verified` succeeds and returns the expected `ForkResolutionBody`.

### Step 4 — Add round-trip equality tests

For every positive vector:

1. decode `canonical_cbor_hex` with `cbor::decode_canonical`;
2. re-encode with `cbor::encode`;
3. assert byte equality with the original fixture bytes;
4. for signed records, run `decode_verified` and then `to_csb(decoded_body)` and assert equality;
5. for envelopes, assert fixture ID/signature still verify after reconstructing the envelope from fixture fields.

This directly satisfies `encode → decode → re-encode produces identical bytes`.

### Step 5 — Add domain-string and canonical-CBOR fence tests

1. Add `all_domain_constants_match_golden_vectors`:
   - iterate all fixture domains;
   - compare to `domain.rs` constants;
   - fail on any byte/string drift.
2. Add `canonical_cbor_rules_are_fenced_by_vectors`:
   - assert representative fixture CSB begins with expected map/field encodings where stable;
   - assert decoded/re-encoded bytes match exactly;
   - include a negative non-canonical fixture for at least unsorted map keys and non-shortest integers.
3. Ensure changing map sort order, optional-field emission, integer shortest-form encoding, string encoding, or domain string breaks at least one test.

### Step 6 — Add negative vectors for typed rejections

Create one vector per current `Reject` code. Minimum suggested constructions:

| Expected code | Vector construction |
|---|---|
| `non_canonical_encoding` | A valid logical map encoded with unsorted keys, duplicate keys, trailing byte, or non-shortest integer. Keep it as raw `signed` bytes and assert strict decode / verify rejects before body checks. |
| `unknown_version` | Validly signed governance entry or content event with `schema_version = 99`; ID and signature match the bad CSB. |
| `unknown_record_kind` | Governance entry with unknown `kind` string and otherwise valid fields, or the smallest body path that returns `UnknownRecordKind`. |
| `unknown_content_kind` | Content event envelope with `kind = "message.unknown"`, valid CSB, ID, and signature. |
| `invalid_content` | Known content kind with missing required body key, wrong type, oversized string, bad role string, malformed checkpoint unresolved-fork pair, or invalid stream ID length. |
| `id_mismatch` | Valid signed envelope with only the envelope ID replaced by `[0xff; 32]`. |
| `bad_signature` | Valid CSB and ID, but signature from a different key or one flipped signature byte. |
| `wrong_domain` | Bytes/signature that verify under another signed-record domain but are presented to the wrong record verifier. If current code cannot emit `WrongDomain`, mark this as a required implementation/test gap: either add a wrong-domain detection path in the relevant issue or remove/defer the code with explicit rationale. |
| `missing_dependency` | Fold/checkpoint validation referencing an entry parent/root/dependency not supplied to the pure fold. |
| `insufficient_authorization` | Non-admin or removed member attempts admin-governance action; or removed/stranger member attempts content authorization. |
| `invalid_approval` | Duplicate approver, approval for wrong entry, stale proposed root, malformed approval, or insufficiently bound approval. |
| `fork_detected` | Same author signs two conflicting governance entries at same sequence/parent; assert the API path that reports `ForkDetected`. If fold records forks without returning this error, use the exact current fork detector function. |
| `unresolved_fork` | Attempt authorization for an author/scope with unresolved fork evidence. |
| `invalid_fork_resolution` | `fork.resolve` with wrong evidence count, nonmatching evidence where strict validation expects a match, or unauthorized signer. |
| `state_root_mismatch` | Checkpoint body with incorrect `state_root` but otherwise valid signature and snapshot hash over that body. |
| `snapshot_hash_mismatch` | Valid checkpoint body/signature with envelope ID replaced so recomputed snapshot hash differs. |
| `invalid_merkle_proof` | Alter one sibling/root/leaf in a valid inclusion or exclusion proof. |

Important: if a listed code currently has no reachable public path, do not fake the vector. Add an explicit failing `#[ignore]` or a TODO vector entry with `status = "blocked"`, and document the implementation gap. Acceptance for #153 requires closing those gaps before claiming done.

### Step 7 — Add taxonomy/vector completeness tests

1. Load all negative vectors and collect their `expected_reject_code` values.
2. Compare that set to `iroh_rooms_v2_core::error::all_codes()`.
3. Fail if any public code lacks a negative vector.
4. Fail if any negative vector names a code that is not in `all_codes()`.
5. Add a second completeness check for positive signed-record coverage:
   - `community_id`
   - `governance_entry`
   - `governance_approval`
   - `governance_checkpoint`
   - `member_record`
   - `content_event`
   - `fork_resolution`

### Step 8 — Add frozen-vector change discipline

1. In `tests/golden/README.md`, state: "These vectors are frozen. Any intentional change to canonical bytes, domain strings, IDs, signatures, roots, or rejection codes requires an explicit schema-version bump and a protocol-change note."
2. Add a fixture-level field:
   - `schema = "iroh-rooms-v2-golden-vectors/v1"`;
   - `frozen = true`;
   - `requires_schema_bump_on_change = true`.
3. Add a test that asserts these fields are present and true in every fixture file.
4. If the project later adopts a protocol ADR for schema bumps, update this README to point to that ADR.

### Step 9 — Verification commands

After implementing the tests/fixtures, run the smallest relevant gate first:

```bash
cargo fmt --all --check
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
```

If time permits, run the workspace gate from `README.md`:

```bash
scripts/verify.sh
```

This planning phase should not run implementation tests unless a later phase actually changes test files.

---

## 6. Acceptance criteria mapping

| Issue acceptance | Planned coverage |
|---|---|
| Every signed record type has at least one positive golden vector | §5 Step 3 fixtures for community/room ID, governance entry, approval, checkpoint, member record/projection, content event, and fork resolution (§3.7). |
| Every §6.4 rejection rule has a negative vector that fails with the typed reason | §5 Step 6 negative vectors plus §5 Step 7 completeness test against `Reject::all_codes()`. |
| Round-trip equality holds for every record | §5 Step 4 decode/re-encode equality for all CSB and standalone canonical objects. |
| Changing any domain string or canonical-CBOR rule breaks at least one vector | §5 Step 5 domain and canonical-CBOR fence tests. |
| Vectors are documented as frozen; any change requires explicit schema-version bump | §5 Step 1 and Step 8 README/fixture metadata/tests. |

---

## 7. Security, privacy, reliability, performance, and migration notes

### Security

- Golden vectors prevent silent domain-string drift and canonicalization drift.
- Fixtures must use deterministic public seeds only; never commit real keys or invite material.
- Negative vectors should isolate one failure at a time to prevent ambiguous parser behavior from hiding regressions.
- Unknown kinds and unknown keys must be rejected, never ignored.
- Domain separation must be checked before interoperability claims.

### Privacy

- Use synthetic room names and messages only.
- Do not include endpoint addresses, network hints, user names, ticket strings, private keys, or real audit data.
- Test seeds are public and explicitly non-secret.

### Reliability

- Tests must be deterministic and not depend on wall clock, entropy, network, file system mutation beyond loading checked-in fixtures, or ordering of hash maps.
- Fixture loader should fail loudly on missing/invalid fixture fields.
- Round-trip tests protect against accidental reserialization differences.

### Performance

- Fixtures are small; test overhead should be negligible.
- Avoid property/fuzz-scale generation in this golden test file. Keep fuzz/property tests separate.

### Migration / compatibility

- This is a test-only change, so no user-data migration is required.
- Intentional fixture changes are protocol changes and require schema-version bump discipline.
- No runtime rollout/rollback is needed because `iroh-rooms-v2-core` is unused pure infrastructure in this phase.

---

## 8. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| #134/#146 final text differs from current candidate code | Frozen vectors may pin provisional names/domains | Mark current assumptions in fixture README; update only with explicit schema bump / protocol note. |
| Vectors are generated only from the implementation | Implementation bug becomes interoperability truth | Require reviewable logical fields, exact domains, and ideally independent regeneration before declaring interop readiness. |
| `WrongDomain` or other codes are not reachable today | Acceptance cannot be fully satisfied | Completeness test should expose the gap; do not fake vectors. Add a tracked implementation/test gap. |
| MemberRecord is not currently a signed envelope | Ambiguous issue wording | Pin current `MemberRecord`/`MemberLeaf` projection and Merkle boundary; add signed member-record vector only if #151 introduces one. |
| Adding JSON parser dev-dependency violates minimal dependency posture | Pure core dependency discipline weakens | Prefer dev-only dependency; if not acceptable, use Rust constants or minimal parser in tests. Banned-dependency guard must still pass. |
| Fixture churn during active v2 development | Golden tests become noisy | Declare fixtures frozen only after #146/#147/#151/#152 behavior is accepted; before that, keep changes explicit and reviewed. |

---

## 9. Assumptions

1. `CommunityId` in issue #153 maps to the current `RoomId` / `domain::ROOM_ID` boundary unless #146 introduces a distinct type.
2. `MemberRecord` maps to current `governance::model::MemberRecord` plus `member::projection::MemberLeaf` / Merkle root, because no `SignedMemberRecord` exists in this checkout.
3. The current `Reject` taxonomy is the local representation of #134 §6.4 rejection rules for this planning phase.
4. `crates/iroh-rooms-v2-core` remains pure and unused by runtime/store/network crates during this work.
5. Fixture keys are deterministic public test seeds and are not secrets.
6. Adding test-only fixture files and test code is allowed; production code changes are not.

---

## 10. Open questions

- **OQ-1:** Does #146 finalize a distinct `CommunityId` type or keep the current `RoomId` naming?
- **OQ-2:** Does #151 require a separately signed `MemberRecord`, or is the member projection/Merkle leaf the intended pinned record boundary?
- **OQ-3:** Which exact #134 §6.4 rejection rules are normative beyond the current `Reject` enum, and should any current `Reject` codes be split or renamed before vectors freeze?
- **OQ-4:** Should fixtures be JSON with a dev-only parser, CBOR/hex-only files, or Rust constants plus documentation JSON?
- **OQ-5:** Can `WrongDomain`, `MissingDependency`, and `InvalidApproval` all be exercised through current public APIs, or do they require small test-only helpers / production validation gaps to be resolved in dependency issues?
- **OQ-6:** Should an independent non-Rust implementation regenerate the vectors before an interop claim is allowed?
- **OQ-7:** The content-kind registry (`content/registry.rs`) is closed with 8 kinds (`message.text`, `message.reaction`, `message.edited`, `file.shared`, `agent.status`, `moderation.block`, `moderation.report`, `moderation.remove`). Should the positive content-event coverage pin one vector per registered kind (to fence every `body`-map schema), or is a single representative `message.text` vector sufficient for #153 with per-kind body vectors deferred to #152?
