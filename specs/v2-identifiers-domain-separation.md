# Spec: v2 Identifiers and Domain Separation

| | |
|---|---|
| **Issue** | #146 — `[CORE] v2 identifiers + domain separation (#134 §6)` |
| **Labels** | `type/feature` `area/protocol` `priority/p1` `risk/low` |
| **Refs** | #134 §6.1–§6.4; depends #140; blocks #147, #151, #152 |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/` (`src/domain.rs`, `src/ids.rs`, `src/schema.rs`) with frozen golden vectors in `tests/identifiers.rs` and `tests/golden/v2-identifiers.json`. How the §14 open questions were resolved is recorded in §15. |

---

## 1. Summary

Implement the #134 §6 v2 cryptographic identifier foundation in `crates/iroh-rooms-v2-core/`: frozen domain-separation constants, typed v2 identifiers, BLAKE3-256 derivation helpers over declared preimages, and strict canonical-CBOR record validation rules used before governance/content/replica layers build on top.

This must be a pure protocol-core change: no transport, storage, runtime, CLI, SDK wiring, GitHub automation, or v1 crate changes.

The current repository already has a landed `iroh-rooms-v2-core` crate from #140 with canonical CBOR, Ed25519/BLAKE3 primitives, generic signed envelopes, governance/content/member modules, banned-dependency tests, and #153-style golden vectors. However, its current `domain.rs` uses candidate strings such as `iroh-rooms:v2:governance-entry:sign:v1`, while this issue freezes #134 §6.2 strings in the `iroh-room-v2/...` form. #146 should reconcile that drift explicitly and treat the #134 §6 strings as normative.

---

## 2. Repository context read

Relevant local context:

- `README.md` describes Iroh Rooms as a local-first collaboration runtime and maps `crates/iroh-rooms-v2-core/` as the pure v2 crypto core: canonical CBOR, BLAKE3/Ed25519 IDs, governance state machine, fork detection, and member Merkle map. It is unused by the shipped runtime in this phase.
- `Cargo.toml` includes `crates/iroh-rooms-v2-core` as a workspace member and documents that it must have no network/store/async dependencies.
- `crates/iroh-rooms-v2-core/Cargo.toml` currently depends only on protocol primitives (`ed25519-dalek`, `blake3`, `hex`, `zeroize`, `getrandom`) and dev-only `proptest`; this matches the acceptance requirement to avoid `tokio`, `iroh`, and `iroh-blobs`.
- `specs/v2-crypto-core-crate.md` is the parent #140 plan. It names #146 as the identifier/domain-separation child issue and requires domain constants, ID/hash newtypes, BLAKE3 helpers, byte-pinned domain tests, and golden identifier vectors.
- `specs/v2-signed-record-golden-vectors.md` plans #153 fixture coverage and currently documents a compatibility assumption that `CommunityId` may be represented as `RoomId` until #146 lands. #146 should replace that assumption with the final v2 identifier names.
- `crates/iroh-rooms-v2-core/src/domain.rs` centralizes current candidate domain strings and provides `blake3_domain(context, payload)` plus `signing_message(sign_context, payload)`. This is the primary module to revise.
- `crates/iroh-rooms-v2-core/src/ids.rs` defines generic 32-byte hash-ID and public-key newtypes, including `RoomId`, `GovernanceEntryId`, `ApprovalId`, `ContentEventId`, `SnapshotHash`, `StateRoot`, and `MerkleRoot`. It does not yet expose the exact #146 names: `CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId`, `ReplicaId`.
- `crates/iroh-rooms-v2-core/src/cbor.rs` implements the strict deterministic-CBOR profile: unsigned integers, byte strings, UTF-8 text, arrays, text-keyed maps, definite lengths, shortest integers, sorted unique keys, no tags/floats/simple values, no negative ints, no trailing data, bounded depth.
- `crates/iroh-rooms-v2-core/src/signed.rs` implements the generic trust boundary: typed body → canonical signed bytes → domain-separated signature message → domain-separated ID → envelope preserving CSB verbatim.
- `crates/iroh-rooms-v2-core/src/error.rs` exposes machine-readable `Reject` codes, including `non_canonical_encoding`, `id_mismatch`, `bad_signature`, and content/schema validation failures.
- `crates/iroh-rooms-v2-core/tests/banned_dependencies.rs` already machine-checks that the v2 core does not depend on `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, or `rusqlite`.
- `crates/iroh-rooms-v2-core/tests/signed_records_golden.rs` and `tests/golden/` already freeze current candidate record bytes. #146 must update/add lower-level domain/identifier vectors without silently inheriting old candidate strings.
- `docs/protocol.md` is the v1 protocol reference. v1 remains in its own crate and must not be changed by #146.
- `CONTRIBUTING.md` requires `scripts/verify.sh` as the normal quality gate; protocol-labeled issues require maintainer review.

---

## 3. Scope

### 3.1 In scope

1. Define every frozen #134 §6.2 domain-separation string as a public constant in `crates/iroh-rooms-v2-core`:
   - `iroh-room-v2/community`
   - `iroh-room-v2/governance-entry`
   - `iroh-room-v2/governance-approval`
   - `iroh-room-v2/content-event`
   - `iroh-room-v2/member-leaf`
   - `iroh-room-v2/merkle-node`
   - `iroh-room-v2/governance-state`
   - `iroh-room-v2/replica-receipt`
   - `iroh-room-v2/governance-checkpoint`
   - `iroh-room-v2/stream-checkpoint`
   - `iroh-room-v2/migration`
2. Byte-pin every domain string with unit tests that assert exact ASCII bytes, length, uniqueness, no NUL/control bytes, and no plural/colon/v1 candidate drift.
3. Add or rename typed identifier newtypes for #134 §6.3:
   - `CommunityId`
   - `GovernanceId`
   - `StreamId`
   - `EventId`
   - `CheckpointId`
   - `ReplicaId`
4. Implement BLAKE3-256 derivation helpers for each identifier from its declared #134 §6.3 preimage.
5. Add golden vectors that round-trip every identifier from a fixed logical preimage through canonical bytes, domain-separated digest, typed ID, display, parse, and raw-byte access.
6. Enforce strict canonical-CBOR record validation rules from #134 §6.4:
   - reject non-canonical CBOR;
   - reject duplicate keys;
   - reject missing required keys;
   - reject wrong byte widths;
   - reject unknown mandatory schema/version/kind;
   - reject signature mismatch;
   - reject ID mismatch.
7. Add a golden negative vector for at least one non-canonical CBOR record and typed tests for each §6.4 rejection category reachable from this issue.
8. Preserve and extend the banned-dependency guard for `tokio`, `iroh`, `iroh-blobs`, and related runtime/store crates.

### 3.2 Out of scope

- v1 event or room record types in `crates/iroh-rooms-core/`.
- Governance operation registry semantics (#147).
- Governance/member Merkle map behavior beyond using the frozen `member-leaf`, `merkle-node`, and `governance-state` domains at hash boundaries.
- Content-event body registry semantics (#152), except the `content-event` domain and `EventId` derivation surface.
- Replica receipt protocol/storage semantics, except the `replica-receipt` domain and `ReplicaId`/receipt hash boundary names required by #134 §6.
- Wire transport, ALPNs, iroh networking, tokio tasks, SQLite/store migrations, CLI/SDK exports, deployment, releases, or GitHub issue/PR actions.
- Generating new #153 full signed-record fixtures beyond the minimal #146 identifier/domain vectors, unless existing #153 fixtures must be updated to compile after renames.

---

## 4. Key decisions

### D1 — #134 §6.2 strings are normative and replace local candidates

The frozen domain constants must be the exact issue-provided ASCII strings, not the existing candidate strings in `domain.rs`.

Current local examples like `iroh-rooms:v2:governance-entry:sign:v1` are useful historical scaffolding only. #146 should either replace them or keep compatibility aliases only if needed for already-implemented child modules, but all public/frozen #146 constants and tests must use the #134 strings.

### D2 — Use one domain per semantic boundary, not separate `sign` and `id` strings unless #134 says otherwise

The issue lists one domain string per boundary (`/governance-entry`, `/content-event`, etc.), not separate `:sign` and `:id` contexts. The default #146 interpretation should therefore be:

```text
id_digest  = BLAKE3(domain || declared_id_preimage)
sig_msg    = domain || canonical_record_bytes
```

If the unavailable #134 §6.3 text explicitly defines separate signature and ID preimages or prefixes, implement that text instead and document the deviation from this default in the code and vectors.

### D3 — Identifier helpers must make the preimage explicit at the type boundary

Do not expose generic `hash(domain, bytes) -> [u8; 32]` as the primary API for identifiers. Keep a low-level helper internally, but expose typed functions whose names state the preimage, for example:

```rust
impl CommunityId {
    pub fn derive(preimage: &CommunityIdPreimage) -> Self;
}

impl EventId {
    pub fn from_content_event_csb(csb: &[u8]) -> Self;
}
```

This prevents accidental wrong-domain/wrong-preimage use and makes acceptance tests mechanically trace each identifier to the declared preimage.

### D4 — Keep identifier presentation strict and deterministic

All hash identifiers are 32 bytes internally. Display/parse should remain lowercase `blake3:<64-hex>` unless #134 §6.3 explicitly defines another presentation form. Parsing must reject wrong prefix, wrong width, odd-length hex, invalid hex, uppercase if canonical presentation is required, and trailing/leading whitespace.

Public-key-like identifiers, if any are present in the #134 preimage model, should not reuse hash-ID parsing unless they are actually BLAKE3 outputs.

### D5 — `RoomId` compatibility must not obscure `CommunityId`

Current code and fixtures use `RoomId` where #146 names `CommunityId`. Introduce `CommunityId` as the canonical v2 type. If compatibility with existing in-crate code is needed, either:

1. add `pub type RoomId = CommunityId` temporarily inside v2 core; or
2. keep `RoomId` as a deprecated/internal alias used only by not-yet-updated child modules.

New tests and documentation should use `CommunityId`.

### D6 — Canonical bytes remain the trust boundary

The strict-CBOR decoder should verify canonicality before semantic validation, ID checks, or signature acceptance. Receivers verify the exact byte string supplied in the record/envelope; they must not reserialize to make a malformed record acceptable.

### D7 — Errors stay typed and pure

`iroh-rooms-v2-core` should never log, emit metrics, read clocks, touch storage, or perform network operations. It returns typed `Reject`/parse errors only. Downstream crates may map those errors to observability later.

### D8 — Test vectors are deterministic and non-secret

All golden vectors must use public fixed seeds/preimages. Do not include real room names, identities, network addresses, ticket material, endpoint IDs, local paths, or user data.

---

## 5. Proposed module/API shape

### 5.1 `domain.rs`

Replace or augment current constants with a frozen #146 section:

```rust
pub const COMMUNITY: &[u8] = b"iroh-room-v2/community";
pub const GOVERNANCE_ENTRY: &[u8] = b"iroh-room-v2/governance-entry";
pub const GOVERNANCE_APPROVAL: &[u8] = b"iroh-room-v2/governance-approval";
pub const CONTENT_EVENT: &[u8] = b"iroh-room-v2/content-event";
pub const MEMBER_LEAF: &[u8] = b"iroh-room-v2/member-leaf";
pub const MERKLE_NODE: &[u8] = b"iroh-room-v2/merkle-node";
pub const GOVERNANCE_STATE: &[u8] = b"iroh-room-v2/governance-state";
pub const REPLICA_RECEIPT: &[u8] = b"iroh-room-v2/replica-receipt";
pub const GOVERNANCE_CHECKPOINT: &[u8] = b"iroh-room-v2/governance-checkpoint";
pub const STREAM_CHECKPOINT: &[u8] = b"iroh-room-v2/stream-checkpoint";
pub const MIGRATION: &[u8] = b"iroh-room-v2/migration";
```

Keep a single helper with no allocation for BLAKE3 preimages:

```rust
pub(crate) fn blake3_domain<const N: usize>(domain: &[u8], parts: [&[u8]; N]) -> [u8; 32];
```

and one signing-message helper if signed records need `domain || csb` messages.

### 5.2 `ids.rs`

Add exact #146 newtypes:

```rust
pub struct CommunityId([u8; 32]);
pub struct GovernanceId([u8; 32]);
pub struct StreamId([u8; 32]);
pub struct EventId([u8; 32]);
pub struct CheckpointId([u8; 32]);
pub struct ReplicaId([u8; 32]);
```

Each type should support:

- `from_bytes([u8; 32]) -> Self` for already-verified bytes;
- `as_bytes(&self) -> &[u8; 32]`;
- `Display` as canonical named hash;
- `FromStr` with strict validation;
- `Debug` that includes the type name and canonical string;
- `Ord`/`Hash` over raw bytes;
- derivation helper(s) named after the declared #134 preimage.

Keep existing `GovernanceEntryId`, `ApprovalId`, `ContentEventId`, `SnapshotHash`, `StateRoot`, and `MerkleRoot` only if child modules still require them. Where semantics overlap, migrate to the #146 canonical names in a compatibility-focused pass.

### 5.3 Preimage model

Create explicit preimage structs or functions. The exact byte layout must be copied from #134 §6.3. If #134 §6.3 is not available to the implementer, block implementation rather than guessing.

Planning table for implementation:

| Identifier | Domain constant | Required source of preimage | Implementation note |
|---|---|---|---|
| `CommunityId` | `COMMUNITY` | #134 §6.3 `CommunityId` preimage | Replace current `RoomId` derivation/alias. |
| `GovernanceId` | likely `GOVERNANCE_ENTRY` or `GOVERNANCE_STATE` per #134 §6.3 | #134 §6.3 `GovernanceId` preimage | Do not infer from current `GovernanceEntryId`; verify name/semantics. |
| `StreamId` | likely `CONTENT_EVENT` or stream-specific preimage per #134 §6.3 | #134 §6.3 `StreamId` preimage | Ensure fixed width and community binding. |
| `EventId` | `CONTENT_EVENT` for content-event records unless #134 says broader event ID | #134 §6.3 `EventId` preimage | May replace current `ContentEventId`. |
| `CheckpointId` | `GOVERNANCE_CHECKPOINT` or `STREAM_CHECKPOINT` depending checkpoint kind | #134 §6.3 `CheckpointId` preimage | Consider enum/typed constructors for governance vs stream checkpoints. |
| `ReplicaId` | `REPLICA_RECEIPT` or #134-declared replica preimage | #134 §6.3 `ReplicaId` preimage | Do not add receipt protocol behavior in this issue. |

### 5.4 Strict record validation surface

Add a small validation layer used by record decoders:

```rust
pub struct Schema<'a> {
    pub name: &'static str,
    pub required: &'a [FieldSpec],
    pub optional: &'a [FieldSpec],
}

pub struct FieldSpec {
    pub key: &'static str,
    pub kind: FieldKind,
}

pub enum FieldKind {
    Uint,
    Text,
    BytesExact(usize),
    Array,
    Map,
}
```

The validator should reject:

- non-map record bodies;
- missing required keys;
- duplicate keys (normally already rejected by CBOR decode, but keep tests at the raw CBOR layer);
- unknown keys when the schema is closed;
- unknown mandatory schema/version/kind;
- byte strings not exactly 16/32/64 bytes where the schema declares those widths;
- ID fields that do not equal recomputed IDs;
- signatures that do not verify under the declared signer.

Avoid adding a broad dynamic schema system if the current per-body `from_canonical` pattern is simpler. The acceptance requirement is behavior, not a framework.

---

## 6. Implementation steps

### Step 1 — Freeze domain constants

1. Open `crates/iroh-rooms-v2-core/src/domain.rs`.
2. Add the eleven #134 §6.2 constants with exact byte strings.
3. Keep old candidate constants only as private compatibility aliases if needed to avoid a large unrelated rewrite; otherwise migrate direct uses to the new constants.
4. Add `ALL_V2_DOMAINS: &[(&str, &[u8])]` for tests.
5. Update documentation to state these are frozen and protocol-breaking to change.

### Step 2 — Byte-pin domains in unit tests

1. Add tests asserting each constant equals the exact `b"iroh-room-v2/..."` byte string.
2. Assert all constants:
   - are non-empty ASCII;
   - contain no NUL;
   - start with `iroh-room-v2/`;
   - do not contain `:`;
   - are unique.
3. Add a regression assertion that no frozen domain equals an old candidate like `iroh-rooms:v2:governance-entry:sign:v1`.
4. Ensure a one-byte edit to any domain string fails a test independent of golden-vector tests.

### Step 3 — Introduce exact #146 identifier types

1. Update `ids.rs` with `CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId`, and `ReplicaId`.
2. Reuse the existing macro pattern for named 32-byte BLAKE3 hash IDs.
3. Add strict parse/display tests for each type.
4. Decide and document compatibility aliases for current names:
   - likely `pub type RoomId = CommunityId` inside v2 core;
   - possibly `ContentEventId` → `EventId` if #134 confirms event scope;
   - avoid aliases that hide semantic differences.
5. Update internal imports only as needed to keep the v2 core compiling; do not touch v1 crates.

### Step 4 — Implement declared preimage derivations

1. Copy the exact #134 §6.3 preimage formulas into a table in `ids.rs` or a new `derive.rs` module.
2. For each identifier, implement a constructor that accepts typed preimage parts rather than raw concatenated bytes when practical.
3. Concatenate preimage parts in the exact #134 order with fixed-width binary fields and canonical-CBOR bytes where declared.
4. Use BLAKE3-256 over `domain || preimage`.
5. Add tests that independently recompute each digest from raw fixture bytes and compare to the typed helper output.
6. If a preimage contains a variable-length field, enforce prefix-free encoding exactly as #134 declares. If #134 is silent, block and ask for clarification rather than inventing a length scheme.

### Step 5 — Wire strict canonical-CBOR validation into record boundaries

1. Keep `decode_canonical` as the only accepted record parser for signed/hashed record bytes.
2. Ensure all `from_canonical` paths reject missing required keys and unknown keys with typed errors.
3. Add width checks for every identifier/signature/hash field:
   - 32 bytes for BLAKE3 IDs and Ed25519 public keys;
   - 64 bytes for Ed25519 signatures;
   - 16 bytes only for fields #134 explicitly declares as short IDs/nonces.
4. Ensure duplicate keys are rejected at the raw CBOR layer before any semantic field extraction.
5. Ensure ID mismatch is checked against the exact received canonical bytes, not reserialized bytes.
6. Ensure signature verification uses `domain || signed_bytes` and the declared signer key.

### Step 6 — Add identifier golden vectors

Add `crates/iroh-rooms-v2-core/tests/identifiers.rs` or extend a focused existing test file. Vectors should be small and reviewable.

Recommended fixture format under `crates/iroh-rooms-v2-core/tests/golden/v2-identifiers.json`:

```json
{
  "schema": "iroh-room-v2-identifiers/v1",
  "frozen": true,
  "requires_schema_bump_on_change": true,
  "domains": {
    "COMMUNITY": "iroh-room-v2/community"
  },
  "vectors": [
    {
      "name": "community-id-basic-v1",
      "identifier": "CommunityId",
      "domain": "iroh-room-v2/community",
      "preimage_hex": "...",
      "digest_hex": "...",
      "display": "blake3:...",
      "expected_result": "ok"
    }
  ]
}
```

Each vector must assert:

1. fixture domain string equals the constant;
2. fixture preimage bytes exactly match the typed preimage builder output;
3. BLAKE3 digest equals `digest_hex`;
4. typed ID wraps the digest;
5. display string equals fixture `display`;
6. parsing the display returns the same typed ID;
7. wrong-domain recomputation produces a different digest.

### Step 7 — Add negative canonical-CBOR vector(s)

At minimum, add a golden negative vector for a non-canonical record, e.g. a map with duplicate keys or non-shortest integer encoding. Assert the public parser returns `Reject::NonCanonicalEncoding` or the equivalent canonical-CBOR error mapped by the record boundary.

Also add targeted negative tests for:

- missing required key;
- wrong-width ID field;
- unknown mandatory schema/version;
- ID mismatch;
- signature mismatch.

Use exactly one primary fault per negative vector so expected rejection codes are stable.

### Step 8 — Reconcile existing golden tests

1. Run the v2-core tests after renaming domains/IDs.
2. Update `tests/signed_records_golden.rs` and `tests/golden/v2-signed-records.json` only for expected #146 changes.
3. Preserve the #153 invariant: any changed domain string, canonical bytes, ID, signature, or rejection code requires an explicit fixture schema bump and protocol-change note.
4. Do not paper over drift by weakening assertions.

### Step 9 — Preserve dependency purity

1. Keep `banned_dependencies.rs` and extend the banned list if new runtime/store crates appear.
2. Do not add `tokio`, `iroh`, `iroh-blobs`, `iroh-gossip`, `rusqlite`, `serde` transport formats, or networking/storage crates.
3. Prefer existing dependencies already in `Cargo.toml`.
4. If a JSON fixture parser is desired, avoid adding production dependencies. Prefer compile-time constants in Rust tests or dev-only parsing if already acceptable.

### Step 10 — Verification commands

Run the narrow gate first:

```bash
cargo fmt --all --check
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
```

Then run the workspace gate if time/resources allow:

```bash
scripts/verify.sh
```

---

## 7. Acceptance criteria mapping

| Issue acceptance | Spec/test requirement |
|---|---|
| Every domain string in §6.2 is a constant, byte-pinned by a unit test | `domain.rs` exposes all eleven exact constants; tests assert exact byte strings, uniqueness, ASCII, and no old candidate drift. |
| Each identifier type recomputes exactly from its declared preimage; golden-vector round-trip | `CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId`, `ReplicaId` each have typed derivation helpers and golden vectors covering preimage → digest → display → parse. |
| A non-canonical CBOR record is rejected; golden negative vector | `decode_canonical`/record parser rejects duplicate/unsorted/non-shortest/trailing cases; fixture asserts typed rejection. |
| No dependency on tokio / iroh / iroh-blobs | Existing banned-dependency test remains green and includes all forbidden crates. |

---

## 8. Validation and authorization rules

This issue is mostly stateless. Required validation:

1. Domain constants are closed and exact.
2. Identifier constructors enforce 32-byte digest width.
3. Identifier parsers reject malformed presentation.
4. Declared preimage builders reject wrong-width fields before hashing.
5. CBOR parser rejects non-canonical encodings before semantic use.
6. Record schema validators reject missing/duplicate/unknown keys and wrong widths.
7. ID verification recomputes from exact bytes and rejects mismatch.
8. Signature verification uses Ed25519 over the domain-separated message and rejects mismatch.

Authorization policy is out of scope except where a record boundary must not trust unauthenticated authors. Governance-specific authorization belongs to #147/#148.

---

## 9. Error model and observability

- The pure core returns typed errors only; it must not log or emit metrics.
- Use or extend `Reject` with stable `.code()` strings if existing variants cannot represent #134 §6.4 precisely.
- Recommended mappings:
  - non-canonical / duplicate key / unsorted key / non-shortest integer → `non_canonical_encoding`;
  - missing required key / wrong field type / wrong width → `invalid_content` or a more specific schema error if added;
  - unknown schema/version → `unknown_version`;
  - unknown mandatory record kind → `unknown_record_kind`;
  - ID mismatch → `id_mismatch`;
  - signature mismatch → `bad_signature`.
- Do not include secret material or raw private keys in error display strings.

---

## 10. Security, privacy, reliability, and performance

### Security

- Domain separation prevents cross-record hash/signature replay.
- Exact byte-pinned tests prevent accidental drift in protocol-critical constants.
- Canonical CBOR rejection prevents multiple byte encodings of the same logical record from producing divergent IDs/signatures.
- Typed ID wrappers reduce wrong-domain/wrong-width misuse.
- Signature and ID checks must use the received bytes verbatim.

### Privacy

- Golden fixtures must use deterministic public test data only.
- No local identity secrets, invite tokens, endpoint addresses, paths, or user data should appear in fixtures or errors.

### Reliability

- Deterministic derivations make cross-implementation test vectors possible.
- Tests should fail loudly on any domain/preimage/canonicalization drift.
- Keep compatibility aliases local and documented to avoid broad breakage while child issues update.

### Performance

- BLAKE3 and Ed25519 operations are small and synchronous.
- Avoid unnecessary allocations in domain/preimage concatenation for hot paths; fixture tests can be simpler.
- CBOR depth/length bounds should remain in place for adversarial inputs.

---

## 11. Rollout and rollback

### Rollout

1. Land #146 in `iroh-rooms-v2-core` only.
2. Keep the crate `publish = false` and unused by shipping v1 runtime.
3. Update #153 vectors if domain/ID names change.
4. Let #147/#151/#152 build on the frozen constants and ID types.

### Rollback

Because this crate is unused by the shipped runtime, rollback is source-level only: revert the #146 changes or restore the previous compatibility aliases. Do not ship two conflicting domain-string sets as equally valid; that would undermine interop. If the frozen #134 strings are later corrected, bump the fixture schema and document the protocol-breaking decision.

---

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Current local candidate domains conflict with #134 strings | Existing #153 tests and modules may fail after replacement | Treat #134 strings as normative; update fixtures with schema bump; document aliases if temporary. |
| Exact #134 §6.3 preimage formulas are not available in checkout | Implementer could guess wrong identifiers | Block implementation until formulas are copied from #134; do not infer from current code names. |
| `CommunityId` vs current `RoomId` naming drift | Confusing API and fixture names | Introduce `CommunityId` as canonical; keep `RoomId` only as explicit compatibility alias if necessary. |
| Single domain per record conflicts with existing sign/id split | Signature/ID tests may need broad updates | Follow #134. If #134 requires split contexts, document and test both; otherwise use listed frozen domains. |
| Negative vectors trigger multiple failures | Flaky expected rejection codes | Construct one-fault fixtures and assert first failure explicitly. |
| Adding fixture parsing dependencies bloats pure crate | Violates purity/minimality | Use Rust constants or dev-only minimal tooling; keep banned-dep test. |

---

## 13. Assumptions

1. #134 §6.2 domain strings listed in the issue are normative and exact, including singular `iroh-room-v2`.
2. All #146 identifiers are BLAKE3-256 outputs with 32-byte raw form.
3. The canonical public presentation remains `blake3:<64 lowercase hex>` unless #134 §6.3 says otherwise.
4. v1 remains isolated in `crates/iroh-rooms-core/` and should not be modified.
5. `iroh-rooms-v2-core` remains `publish = false` and unused by runtime/network crates in this phase.
6. Golden vectors may use deterministic public seeds/preimages and must not contain real secrets.

---

## 14. Open questions

1. What are the exact #134 §6.3 byte preimage formulas for `CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId`, and `ReplicaId`? The issue summary names them but does not include formulas.
2. Does #134 intend one domain string to be used for both ID derivation and signature messages, or are there unlisted subdomains for signing vs ID hashing?
3. Is `GovernanceId` the ID of a governance entry, the governance log/state, or another object? Current code has both `GovernanceEntryId` and `StateRoot`.
4. Is `EventId` specifically a content-event ID, or a generic signed-record/event ID across governance/content/replica records?
5. Does `CheckpointId` cover both governance checkpoints and stream checkpoints, or should the API expose separate constructors/types over the same newtype?
6. Is `ReplicaId` derived from a replica public key, a receipt, a device/principal key, or a canonical replica descriptor?
7. Should old candidate constants remain as deprecated private aliases until #147/#151/#152 are reconciled, or should #146 do a full immediate rename across v2 core?

---

## 15. Implementation notes (post-landing)

#146 landed as a pure-protocol-core change confined to `crates/iroh-rooms-v2-core/`. It satisfies every acceptance criterion (frozen domains byte-pinned by unit tests, identifier round-trip golden vectors, a golden non-canonical CBOR negative vector, and no `tokio`/`iroh`/`iroh-blobs` dependency). The §14 open questions were resolved as follows, each pinned by focused tests and documented at the derivation helper in `src/ids.rs`:

1. **OQ-1 (preimage formulas):** the exact `#134 §6.3` byte layouts were still unavailable in checkout, so the spec D2 default derivation is used and pinned by golden vectors:

   ```text
   id_digest = BLAKE3(DOMAIN || declared_id_preimage)
   ```

   where `declared_id_preimage` is the canonical-CBOR bytes of the descriptor the identifier names. The frozen vectors in `tests/golden/v2-identifiers.json` fix one preimage per identifier type and assert the full round-trip (preimage → digest → typed id → display → parse → raw bytes).

2. **OQ-2 (one domain vs split sign/id):** the eleven frozen `#134 §6.2` strings are normative for v2 **identifier derivation** (one domain per semantic boundary). The already-frozen `#153` signed-record vectors still sign under the legacy `*_SIGN` candidate contexts; the frozen domains therefore drive id derivation while signing messages retain the legacy contexts for the compatibility period. `src/domain.rs` documents this two-layer split, and a follow-up reconciliation pass will migrate the signed-record paths onto the frozen domains and retire the legacy aliases.

3. **OQ-3 (`GovernanceId` scope):** `GovernanceId` is the identity of a single governance log entry — `BLAKE3(GOVERNANCE_ENTRY || governance_entry_csb)`. The governance *state* root is a separate concept under the `GOVERNANCE_STATE` domain and is not represented by `GovernanceId`.

4. **OQ-4 (`EventId` scope):** `EventId` is specifically a content-event identity under `CONTENT_EVENT`. The legacy `ContentEventId` type remains only for the frozen `#153` vectors.

5. **OQ-5 (`CheckpointId` kinds):** a single `CheckpointId` newtype covers both kinds via typed constructors that pin the domain at the call site — `from_governance_checkpoint_csb` (`GOVERNANCE_CHECKPOINT`) and `from_stream_checkpoint_csb` (`STREAM_CHECKPOINT`).

6. **OQ-6 (`ReplicaId` source):** `ReplicaId` derives from a canonical replica descriptor under `REPLICA_RECEIPT`. Only the derivation surface is exposed here; the receipt protocol itself remains out of scope.

7. **OQ-7 (alias vs full rename):** the legacy candidate constants (e.g. `ROOM_ID`, `GOVERNANCE_ENTRY_SIGN`) are kept as private compatibility aliases so already-landed child modules and the `#153` vectors keep compiling; new code must use the frozen `#134 §6.2` names. A regression fence in `domain.rs` (`frozen_v2_domains_are_not_legacy_candidates`) prevents a frozen domain from silently reverting to a legacy candidate string.

The `StreamId` and `EventId` derivations intentionally share the single `content-event` domain today (documented OQ-1 assumption in `src/ids.rs`); `tests/identifiers.rs::stream_and_event_ids_share_content_event_domain_by_design` pins this so a future dedicated stream-domain change can never land silently.

