# v2 Golden Vectors — Frozen Interoperability Fixtures

This directory holds the **frozen** golden-vector fixtures for the v2 crypto core.
There are three fixture sets, all frozen under the same change discipline:

- `v2-signed-records.json` — every signed record and domain-separated hash boundary
  produced by `iroh-rooms-v2-core` (issue #153, spec `v2-signed-record-golden-vectors.md`,
  refs #134 §1 / §6.4). Driven by `../signed_records_golden.rs`.
- `v2-identifiers.json` — the `#134 §6.3` v2 identifier derivations over the
  `#134 §6.2` frozen domain-separation strings, plus one golden non-canonical CBOR
  negative vector (issue #146, spec `v2-identifiers-domain-separation.md`, refs
  #134 §6.2 / §6.3 / §6.4). Driven by `../identifiers.rs`.
- `v2-member-merkle.json` — the #151 §8.1/§8.2 member-record and sorted-tree
  vectors for 0, 1, 2, 3, and 10,000 leaves, plus the #153 frozen
  inclusion-proof fixtures. Driven by `../member_sorted_merkle.rs`; release
  timing uses `cargo bench -p iroh-rooms-v2-core --bench member_merkle`.

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

- **v6** — `#153` closed the last frozen-vector gap for the member Merkle map:
  byte-exact **inclusion-proof fixtures** were missing (spec §4 D7 / §8 Step 12
  require "pin ... inclusion proof ... in vectors"; proofs were exercised only
  behaviourally before). Added three hand-derived, independently-reproducible
  canonical-CBOR inclusion proofs to `v2-member-merkle.json` (a leaf sibling on
  each side; a two-step path exercising both sides + odd-node promotion; and a
  node-hash sibling produced by trailing-leaf promotion), driven by the new
  `frozen_inclusion_proofs_match_independently_derived_structure` test in
  `member_sorted_merkle.rs`. Sibling hashes are `member_leaf_hash` of the
  already-frozen canonical records (cross-checked: `leaf_hash(1)` == the 1-leaf
  root; `node(leaf1,leaf2)` == the 2-leaf root). **No frozen root or leaf hash
  changes** — v6 only adds proof fixtures composed of already-frozen hashes.
  Exclusion remains expressed as absence-of-proof + rebound-proof rejection (no
  separate byte fixture).
- **v5** — `#152` landed the normative #134 §9.2 `ContentEventBody`
  (`content::body`) and the concrete exact-byte content-event envelope
  (`content::event`: `ContentEvent` / `VerifiedContentEvent` /
  `verify_content_event` / `seal_content_event` / `validate_device_chain_link`).
  The frozen #153 content-event golden vector (`content-event-message-text-v1`)
  is **deliberately preserved on the pre-#152 provisional schema** (now exposed
  as the test-only `content::provisional::ProvisionalContentEventBody`); its
  CSB/id/signature bytes are byte-identical to v4 and prove "unrelated golden
  vectors do not drift". The normative §9.2 schema is a NEW, separate path with
  its own focused tests; it is not dual-decodable as the provisional schema
  (spec #152 D1: one normative v2 content wire schema). No frozen bytes change
  in this bump; only a new normative path is added and the provisional type is
  renamed/relocated. The shared canonical-CBOR codec gained the single canonical
  `null` byte (`0xf6`) required by `prev_device_event`. The fixture-format
  `schema` marker deliberately stays `iroh-rooms-v2-golden-vectors/v2`: these
  change-log versions track landed protocol changes, and no bump to the
  fixture-format schema is required because no frozen byte/hash/signature vector
  changed in v3–v5.
- **v4** — `#178` made the normative `governance::log` record wrappers
  (`GovernanceEntry` / `GovernanceApproval`) own the **exact received canonical
  signed bytes (CSB)** alongside the typed body, mirroring `crate::signed::Envelope`,
  and verify signatures + identity over those retained bytes rather than a
  re-serialization of the typed body (spec
  `v2-governance-records-verbatim-csb.md`; amends #147 §5.3/§5.4). This closes the
  trust-boundary gap where a body whose typed decode normalizes representation (an
  unsorted/duplicate `admin.set` `administrators` array) could be accepted over bytes
  that differ from what was signed. **No frozen bytes change** — the regression
  coverage (a normalizing CSB with a normalized-only signature → `bad_signature`;
  an exact signature over the normalizing bytes → `non_canonical_encoding`) lives in
  the `records.rs` unit tests and the `tests/v2_governance_log_e2e.rs` §9 cases, not
  in these fixture files. See `v2-governance-log-entry-approval-state-root.md`
  §17 "Amendment: verbatim-CSB trust boundary".
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

> **Additive correction (#157, 2026-08-03):** the `ReplicaId` row in this frozen
> file records the Phase-B descriptor-hash candidate; that row is not final #134
> §6.3 signing-key evidence. Stable v2 uses the raw validated Ed25519 replica
> public key and a separate governed Iroh endpoint. The implementation pass must
> add a bumped successor fixture/schema and vectors rather than rewriting this
> file in place. See
> [`v2-replica-endpoint-identity.md`](../../../../specs/v2-replica-endpoint-identity.md).

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

> **Additive correction (#156, 2026-08-03):** ADR-0010 specifies
> `local_sync_group_v1` as the stable-v2 receipt durability semantics. That
> pure-spec decision does not add a receipt/class codec, publication
> certificate, store, or crash vector to this Phase-B fixture family. The Phase
> C implementation must add explicitly versioned successor receipt/class/
> descriptor vectors without rewriting these files. See
> [`specs/v2-replica-durability-class.md`](../../../../specs/v2-replica-durability-class.md).

> **Additive correction (#159, 2026-08-03):** ADR-0011 specifies governed
> zero-weight staging, atomic full-policy activation/replacement, permanent
> disabled signing identities, evidence-driven equivocation quarantine, and
> no same-key recovery after receipt or checkpoint-vote-journal rollback
> uncertainty. This frozen Phase-B family contains none of those public wire
> objects or local operational store state. Add successor genesis/policy/
> readiness/receipt/checkpoint/committed-prepare-
> handoff/cancellation/outcome-neutral-fork-fence/target-tagged-fork-frontier/
> reconciliation/control-signer-exclusion/frontier-evidence/cutover/incident-
> evidence/recovery-control vectors under a bumped schema; do not rewrite
> current identifiers, records, or negatives. Evidence-intake, quarantine/
> barrier generations, trigger-subject caps/saturation, direct-trigger records/
> cumulative subject aggregates/revisions, materialization cursor/per-row
> projections/stale-pair index, and transaction
> accounting belong to explicitly non-wire Phase C store/crash conformance
> rather than this public golden family. Fork-
> reconciliation fixtures include fixed-size structural,
> exact-replayed statement-held, and collected final-union dependency count/
> roots; the complete canonical bounded-chunk proof; F1-to-F2 roll-up; late
> supplemental leaves without statement mutation; boundary-size positives;
> structural-only propose/commit rejection; and omission/duplicate/order/root/
> count/oversized/truncated/unavailable/overflow negatives. See
> [`specs/v2-replica-replacement-recovery.md`](../../../../specs/v2-replica-replacement-recovery.md).
