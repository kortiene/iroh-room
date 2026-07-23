# v2 Golden Vectors — Frozen Interoperability Fixtures

This directory holds the **frozen** golden-vector fixtures for the v2 crypto core.
There are two fixture sets, both frozen under the same change discipline:

- `v2-signed-records.json` — every signed record and domain-separated hash boundary
  produced by `iroh-rooms-v2-core` (issue #153, spec `v2-signed-record-golden-vectors.md`,
  refs #134 §1 / §6.4). Driven by `../signed_records_golden.rs`.
- `v2-identifiers.json` — the `#134 §6.3` v2 identifier derivations over the
  `#134 §6.2` frozen domain-separation strings, plus one golden non-canonical CBOR
  negative vector (issue #146, spec `v2-identifiers-domain-separation.md`, refs
  #134 §6.2 / §6.3 / §6.4). Driven by `../identifiers.rs`.

## These vectors are FROZEN

Any intentional change to:

- canonical CBOR bytes (`canonical_cbor_hex` / `*_csb_hex`),
- domain-separation strings (see `domain.rs`),
- derived identifiers / hashes / roots (`blake3:` named strings or raw digest hex),
- Ed25519 signatures, or
- typed rejection codes,

**requires an explicit schema-version bump** (`requires_schema_bump_on_change`) and a
protocol-change note. The `signed_records_golden.rs` test suite asserts the current
implementation reproduces these exact bytes; any silent drift fails CI.

If `#134` lands and renames `CommunityId` or splits a `Reject` code, update only via
an explicit schema bump (`schema` field below) while preserving or documentedly
changing the frozen byte/hash expectations.

### Change log

- **v3** — `#147` landed the normative v2 governance-log approval pipeline under
  `governance::log` (`verify_genesis` / `verify_entry_full`), which emits
  `Reject::InvalidApproval` for approvals not bound to their enclosing entry's
  `entry_id`, for duplicate approvers, and for non-admin genesis signers (spec
  `v2-governance-log-entry-approval-state-root.md` §8). The `invalid-approval`
  negative vector therefore transitions from `status = "blocked"` to
  `status = "active"`. **No frozen bytes** (CSBs/ids/signatures) change — only the
  reachability/placeholder metadata for this one `Reject` code. The candidate
  scaffolding vectors (schema v2) remain untouched as the frozen path until a
  deliberate migration. See `negative_invalid_approval` in
  `signed_records_golden.rs`; `BLOCKED_CODES` is now just `["wrong_domain"]`.
- **v2** — Migrated the Merkle leaf/internal-node hash boundaries from the legacy
  candidate strings (`iroh-rooms:v2:merkle:leaf|node:v1`) onto the frozen `#134 §6.2`
  domains `iroh-room-v2/member-leaf` and `iroh-room-v2/merkle-node`. This changes
  `LEAF_HASH`, `MEMBER_ROOT`, the governance `state_root`, and the governance
  approval/checkpoint CSBs/ids/signatures. No compatibility impact: `iroh-rooms-v2-core`
  is `publish = false` and unused this phase. The legacy strings survive as documented
  aliases in `domain.rs` (`MERKLE_LEAF`, `LEGACY_MERKLE_NODE`); the active Merkle
  computation no longer references them (PR #176 review resolution).

## Fixture format

- `v2-signed-records.json` — the single aggregate, hand-reviewable JSON file for the
  signed-record set (#153). Every byte sequence is lowercase hex with no `0x` prefix.
  Named hashes use the `blake3:<64-hex>` presentation form; raw digest hex is stored
  alongside where a byte-exact hash input/output is being pinned.
- `v2-identifiers.json` — the identifier-derivation set (#146). Same hex conventions;
  each vector carries its `domain`, `preimage_hex`, `digest_hex`, and `display`, and the
  negative vector carries `expected_reject_code`.
- The Rust tests in `../signed_records_golden.rs` and `../identifiers.rs` carry the same
  values as compile-time constants and are the authoritative checks; the JSON files are
  the human-reviewable mirrors (loaded via `include_str!`, so a missing fixture fails the
  build).

Per-vector schema marker (present on the aggregate and every vector):

```json
{
  "schema": "iroh-rooms-v2-golden-vectors/v2",
  "frozen": true,
  "requires_schema_bump_on_change": true
}
```

## Seeds are public, non-secret

All fixture signing keys are derived from deterministic public seeds (see
`seed_keys` / `*_seed_hex` in each vector):

| Role     | Seed bytes       |
|----------|------------------|
| admin    | `[0xa0; 32]`     |
| member   | `[0xb0; 32]`     |
| approver | `[0xc0; 32]`     |
| author   | `[0xd0; 32]`     |
| resolver | `[0xe0; 32]`     |

These are **public test seeds**, never secrets. No entropy, network addresses,
endpoint ids, ticket material, real user names, or private data appear anywhere in
these fixtures.

## Coverage

### Signed-record vectors (#153)

#### Positive vectors (one per signed record / hash boundary)

| Vector                       | Record type        | Pinned boundary                       |
|------------------------------|--------------------|---------------------------------------|
| `community-id-room-id-derivation-v1` | community id | `ROOM_ID` domain + derivation         |
| `governance-entry-init-room-v1`      | entry        | CSB, entry id, signature              |
| `governance-approval-add-member-v1`  | approval     | CSB, approval id, signature           |
| `governance-checkpoint-clean-state-v1` | checkpoint | CSB, snapshot hash, state/member root |
| `member-record-active-member-leaf-v1` | member leaf | leaf CSB, merkle key/value/leaf hash, member root |
| `content-event-message-text-v1`      | content      | CSB, content event id, signature      |
| `fork-resolution-accept-winner-v1`    | fork resolve | CSB, fork-resolve id, signature       |

Each positive vector asserts: CSB equality, strict decode, re-encode byte identity
(round-trip), domain-separated id equality, domain-separated signing message shape,
Ed25519 signature equality, and full `decode_verified` success.

#### Negative vectors (one per §6.4 rejection rule)

One vector per `Reject::code()`, asserting the implementation returns exactly that
typed reason. See the `negative` section of the JSON and the `negative_*` tests in
`signed_records_golden.rs`.

**Implementation gap (blocked, do not fake):** the code `wrong_domain` is
declared in `error::Reject` but is **not emitted by any current public path**
(verified by `rg "Reject::WrongDomain" src/`). Per spec §5 Step 6 / risk row, it
is recorded as a `status = "blocked"` vector entry rather than a fabricated vector.
The `negative_blocked_codes_have_no_reachable_vector` test documents this gap and
will force a real vector to be added when the code becomes reachable. (Until `#147`
landed the normative governance-log approval pipeline, `invalid_approval` shared
this blocked status; it now has a real vector — see change-log v3.)

### Identifier vectors (#146)

`v2-identifiers.json` + `../identifiers.rs` pin the `#134 §6.3` v2 identifiers
(`CommunityId`, `GovernanceId`, `StreamId`, `EventId`, `CheckpointId` governance + stream
kinds, `ReplicaId`) over the eleven `#134 §6.2` frozen domain strings. Each vector fixes a
deterministic public preimage and asserts: the fixture domain equals the `domain.rs`
constant byte-for-byte; `BLAKE3(domain || preimage)` equals the frozen digest; the typed id
wraps that digest; the display string round-trips through strict parse; and recomputing
under a different frozen domain yields a distinct digest. A completeness test pins all
eleven domains and proves their digests are mutually distinct.

The negative vector `non-canonical-duplicate-key-v1` feeds a duplicate-key CBOR map to the
strict decoder and asserts it rejects before any identifier/schema work, surfacing as
`Reject::NonCanonicalEncoding` (`non_canonical_encoding`). `identifiers.rs` extends this
with one-fault vectors for each canonical-CBOR fault class (trailing data, non-shortest int,
negative int, tag, float/simple, indefinite length, non-text map key, unsorted key,
truncated byte string) plus `§6.4` schema-rule negatives (missing required key, wrong-width
id field, unknown schema version).

## Out of scope

- Wire-transport golden vectors: no v2 ALPN or transport exists yet.
- Replica-receipt vectors: no receipt type exists in Track 2 scope.

These are intentionally absent; do not treat the omission as a coverage gap.
