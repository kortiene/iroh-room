# Spec: v2 Governance Snapshot and Authority-Transition Proof

| | |
|---|---|
| **Issue** | #161 — `[SPEC] §25 #3: Governance snapshot encoding + admin-transition proof` |
| **Refs** | #134 §§6.2, 7.1–7.6, 14, 15, 22.1, 25 #3; #147–#151; #157; #160; ADR-0004; ADR-0008; ADR-0009 |
| **Status** | Proposed normative format. Accepted on merge; implementation, independent vectors, and Phase-C store/network wiring remain required before advertisement. |
| **Scope** | Pure specification: exact snapshot/checkpoint/proof/transfer formats, verification rules, limits, and fixtures. No runtime, store, or codec implementation. |

---

## 1. Decision summary

v2 bootstrap uses four new checkpoint-family objects plus one bounded wrapper
for the already-defined genesis evidence:

1. `GovernanceSnapshotV1`: the complete deterministic governance fold state at
   one accepted cursor, encoded as strict canonical CBOR;
2. `AuthorityTransitionManifestV1`: a checkpoint-bound proof of every
   administrator/recovery transition from genesis to that cursor;
3. `GovernanceCheckpointBodyV1`: the roots, counts, hashes, lengths, cursor, and
   transition-proof commitment approved by the derived current administrator
   quorum; and
4. `SnapshotTransferHeaderV1`: an unsigned, checkpoint-bound declaration of
   identity or Zstandard transfer encoding. Compression never changes snapshot
   identity.

`GenesisEvidenceV1` carries the existing exact `GenesisConfig` CSB and its
existing administrator signatures; it does not define a new genesis body.

The normative records use the landed `governance::log` model. Checkpoint IDs
and signatures retain the frozen `iroh-room-v2/governance-checkpoint` record
boundary; snapshot, manifest, anchor, and span-step hashes use the four additive
domains in §2.2. These records do **not** extend or reinterpret the legacy
candidate `governance::checkpoint::CheckpointBody`, its `RoomId`, its single-
signature `Envelope<SnapshotHash>`, or either legacy checkpoint/snapshot domain.

The compact proof mode omits ordinary governance entries. The final checkpoint
quorum therefore attests that the transition manifest is complete across those
spans. Entry signatures and approvals prove each listed transition under the
old administrator state; the derived final administrator set then verifies the
checkpoint. This ordering avoids trusting an administrator set merely because
the snapshot claims it.

If any accepted `fork.resolve` lies between genesis and the checkpoint, compact
linear mode is forbidden. The proof uses full-DAG mode and carries all
authenticated branch material needed to reproduce the existing recovery-
authorized resolution. A currently unresolved fork cannot produce an
authorization or compaction checkpoint.

---

## 2. Compatibility boundary

### 2.1 Legacy candidate remains byte-stable

The following are preserved exactly and have no alias in this format:

- `governance::checkpoint::CheckpointBody`;
- `Envelope<SnapshotHash>`;
- `iroh-rooms:v2:checkpoint:sign:v1`;
- `iroh-rooms:v2:snapshot-hash:v1`; and
- the frozen `governance-checkpoint-clean-state-v1` fixture and its negatives.

Those bytes demonstrate the earlier candidate core. They do not satisfy #134
§7.6: the candidate hash names the checkpoint-body CSB itself, not a snapshot
blob, and the candidate has no current-administrator quorum or transition proof.
An implementation MUST NOT accept a legacy candidate body as any record below,
even if individual fields happen to have equal values.

### 2.2 Normative domains and typed preimages

#134 §6.2 requires a distinct frozen domain for every hash/signature purpose
and calls its original eleven domains a minimum. This format retains the
existing checkpoint boundary and additively freezes four new boundaries:

```text
GOVERNANCE_CHECKPOINT          = "iroh-room-v2/governance-checkpoint"
GOVERNANCE_SNAPSHOT            = "iroh-room-v2/governance-snapshot"
GOVERNANCE_TRANSITION_MANIFEST = "iroh-room-v2/governance-transition-manifest"
GOVERNANCE_SPAN_ANCHOR         = "iroh-room-v2/governance-span-anchor"
GOVERNANCE_SPAN_STEP           = "iroh-room-v2/governance-span-step"
```

Checkpoint ID derivation and checkpoint signatures share the checkpoint record
boundary under #146's one-domain-per-semantic-record rule. Snapshot, manifest,
anchor, and step hashes are separate purposes and do not reuse it. Phase C must
add all four constants to the byte-pinned public domain inventory before the
format is advertised.

Every hashed canonical map has a required, closed `kind` field. The following
values are disjoint:

```text
governance.snapshot
governance.checkpoint
governance.transition_manifest
governance.span.anchor
governance.span.step
```

The distinct domains are the primary purpose separation. Closed `kind` values
add structural type/version defense and prevent a decoder from accepting one
family member as another.

Definitions:

```text
snapshot_blob_hash =
  BLAKE3(GOVERNANCE_SNAPSHOT || snapshot_csb)

transition_root =
  BLAKE3(GOVERNANCE_TRANSITION_MANIFEST || transition_manifest_csb)

checkpoint_id =
  BLAKE3(GOVERNANCE_CHECKPOINT || checkpoint_csb)

checkpoint_signature_message =
  GOVERNANCE_CHECKPOINT || checkpoint_csb
```

`checkpoint_id` is the normative `CheckpointId`. `snapshot_blob_hash` and
`transition_root` are raw 32-byte typed commitments under distinct domains;
neither is the legacy `SnapshotHash`.

### 2.3 Bounded genesis-evidence wrapper

`GenesisEvidenceV1` is an unsigned closed transfer map around the normative
#147 genesis bytes/signatures:

```text
GenesisEvidenceV1 = {
  kind:           "governance.genesis_evidence",
  signatures: [
    { signer: bstr[32], signature: bstr[64] }, ...
  ],
  genesis_csb:    bstr,
  format_version: 1
}
```

Canonical top-level key order is
`kind, signatures, genesis_csb, format_version`; signature-map order is
`signer, signature`. `genesis_csb` is exactly the closed
`GenesisConfig` format in
[`v2-governance-log-entry-approval-state-root.md`](v2-governance-log-entry-approval-state-root.md)
§5.1. It is strict-decoded and must re-encode byte-identically before use.

Signatures are strictly ascending by raw signer bytes and duplicate-free. Every
signer is one of the genesis administrators and verifies over
`COMMUNITY || genesis_csb`; at least the declared genesis administrator
threshold is required. The wrapper is not hashed state and cannot replace the
caller-pinned expected `CommunityId`.

---

## 3. Shared canonical profile and limits

Every CBOR object uses the repository's strict RFC 8949 core-deterministic
profile:

- unsigned integers only; no negative integers;
- definite-length byte/text strings, arrays, and text-keyed maps;
- shortest integer and length encodings;
- map keys in encoded-key order, unique;
- valid UTF-8 text;
- `null` only where this spec explicitly permits it;
- no tags, floats, booleans, undefined, other simple values, or indefinite
  lengths;
- maximum nesting depth 16; and
- one top-level object with no trailing bytes.

Typed decoding is closed-schema and non-normalizing. Received arrays that must
be sorted/unique are rejected when they are not; a decoder MUST NOT sort or
deduplicate them before authentication. After typed decode, re-encoding MUST be
byte-identical to the exact received canonical bytes.

Hard limits for format version 1:

| Item | Limit |
|---|---:|
| Genesis config CSB | 8,388,608 bytes |
| Genesis evidence wrapper CSB | 16,777,216 bytes |
| Checkpoint body CSB | 65,536 bytes |
| Checkpoint certificate CSB | 8,388,608 bytes |
| Snapshot transfer header CSB | 4,096 bytes |
| Encoded snapshot payload | 16,777,216 bytes |
| Uncompressed snapshot CSB | 16,777,216 bytes |
| Transition manifest CSB | 8,388,608 bytes |
| Transition witnesses (either mode) | 4,096 |
| Full-DAG evidence records | 65,535 |
| Embedded governance entry or approval CSB | 65,536 bytes each |
| Zstandard window | 8,388,608 bytes |
| ID/public-key width | 32 bytes |
| Ed25519 signature width | 64 bytes |

Lengths and counts are validated before proportional allocation. Values above a
limit fail explicitly; no implementation may truncate, silently skip a record,
or lower a quorum. The protocol cap is not the performance target: the complete
10,000-member bootstrap SHOULD remain below 5,242,880 uncompressed bytes,
excluding separately fetched optional profile blobs.

---

## 4. Governance snapshot

### 4.1 Exact top-level map

`GovernanceSnapshotV1` is this closed map:

| Key | Type | Rule |
|---|---|---|
| `kind` | text | exactly `governance.snapshot` |
| `format_version` | uint | exactly `1` |
| `schema_version` | uint | exactly `2` |
| `community_id` | bstr[32] | normative `CommunityId` |
| `governance_seq` | uint | accepted post-genesis count; genesis state is `0` |
| `governance_tip` | bstr[32] or null | null iff `governance_seq == 0`; otherwise exact accepted `GovernanceId` |
| `state_root` | bstr[32] | recomputed six-component `StateRoot` |
| `administrators` | map | exact `AdministratorState` CBOR |
| `recovery` | map | exact `RecoveryConfig` CBOR |
| `replicas` | array | full `ReplicaRecord`s, ascending raw `ReplicaId` |
| `members` | array | full `MemberRecord`s, ascending raw `PrincipalId` |
| `streams` | array | full `StreamRecord`s, ascending raw `StreamId` |
| `community_policy` | map | exact `CommunityPolicy` CBOR, including resolved-fork markers |

The canonical encoded key order is:

```text
kind, members, streams, recovery, replicas, state_root, community_id,
administrators, format_version, governance_seq, governance_tip,
schema_version, community_policy
```

`governance_tip` is always present; the genesis cursor uses canonical `null`.
This avoids two encodings for the same cursor.

### 4.2 Nested component values

The following closed schemas freeze the exact format-1 state encoding. Every
map is listed in canonical encoded-key order. Unknown keys, explicit `null`
outside §4.1, wrong widths, and values outside declared uint ranges reject.

#### 4.2.1 Administrator and recovery components

```text
AdministratorState = {
  threshold:      uint(0..65535),
  administrators: [bstr[32], ...]
}

RecoveryConfig = {
  threshold:     uint(0..65535),
  recovery_keys: [bstr[32], ...]
}
```

`AdministratorState.administrators` is nonempty, strictly ascending by raw
principal bytes, duplicate-free, and no longer than 65,535; its threshold is in
`1..=administrators.len()`.

`RecoveryConfig` deliberately preserves the current normative fold's wider
reachable state. `recovery_keys` remains in exact stored order and preserves
duplicates; `threshold == 0`, a threshold larger than the list/unique-key
count, and an empty list are valid snapshot state. A decoder MUST NOT invoke a
sort/dedup canonicalizer. Such a config simply fails closed if later consulted
to authorize `fork.resolve`. A successfully applied post-genesis
`recovery.set` happens to sort/deduplicate keys, but genesis can commit the
wider representation, so snapshot import cannot require that normalization.

#### 4.2.2 Replica component

```text
ReplicaDescriptor = {
  endpoint:   bstr,
  capability: uint(0..255),
  replica_id: bstr[32]
}

ReplicaRecord = {
  status:     "active" | "disabled",
  descriptor: ReplicaDescriptor
}
```

The top-level `replicas` array is strictly ascending and duplicate-free by
`descriptor.replica_id`. The descriptor id is the record's map key; endpoint
bytes are opaque and may be empty.

> **Additive #157 identity correction (2026-08-03):** format version 1 keeps this
> exact `bstr` field, permissive candidate semantics, byte-preserving decoder,
> and vectors. #157 adds a versioned operational resolver but does not decide
> whether layering it inside this intentionally opaque field preserves format-1
> semantics or requires a successor under §14. Stable advertising is blocked
> until the format owner records that ruling and pins the applicable additive
> profile/format vectors. Regardless of the ruling, low-level decoding cannot
> grant authority: activation fails for the entire active replica profile if any
> descriptor fails #157's exact key-eligibility, canonical endpoint resolution,
> active role-set disjointness, or retained-history cross-role rule.
> Implementations must never filter failed entries or derive/recompute a
> configured receipt quorum over a subset; format 1 contains no receipt-quorum
> field. See
> [`v2-replica-endpoint-identity.md`](v2-replica-endpoint-identity.md).

#### 4.2.3 Member/device component

```text
DeviceRecord = {
  status:    "active" | "revoked",
  binding:   bstr,
  device_id: bstr[32]
}

MemberRecord = {
  roles:       ["admin" | "agent" | "member", ...],
  status:      "active" | "revoked",
  devices:     [DeviceRecord, ...],
  profile?:    bstr,
  grant_seq:   uint,
  member_id:   bstr[32],
  revoke_seq?: uint
}
```

`roles` is nonempty and strictly ordered by its wire text (`admin`, `agent`,
`member`) with no duplicates. `devices` is strictly ascending and duplicate-
free by raw `device_id`; a device id occurs in at most one member record across
the entire snapshot. The top-level `members` array is strictly ascending and
duplicate-free by raw `member_id`.

For an active member, `revoke_seq` is absent. For a revoked member, it is
required, every device is revoked, and
`grant_seq < revoke_seq <= governance_seq`. An active member requires
`grant_seq <= governance_seq`; it may retain revoked devices from an earlier
membership interval. `profile` is omitted when absent, never encoded as null;
an explicitly present empty byte string remains distinct. A `DeviceRecord` has
no grant/revoke sequence fields: those cursors belong to its enclosing member.
Opaque binding bytes may be empty.

Device ids remain global tombstones even after revocation. Conversely, no new
cross-component invariant is introduced: an administrator need not have an
active member record or `admin` role, recovery principals need not be members,
and snapshot validation does not invent a minimum-surviving-member/admin rule.

#### 4.2.4 Stream component

```text
StreamPolicy = {
  access: uint(0..255)
}

StreamRecord = {
  policy:        StreamPolicy,
  archived:      uint(0..1),
  stream_id:     bstr[32],
  created_at_ms: uint
}
```

`archived` is canonical uint `0` or `1`, not a CBOR boolean. The top-level
`streams` array is strictly ascending and duplicate-free by raw `stream_id`;
creation time is signed advisory state and receives no wall-clock check.

#### 4.2.5 Community-policy component

```text
ResolvedForkMarker = {
  branch_heads:        [bstr[32], ...],
  created_at_ms:       uint,
  selected_head:       bstr[32],
  selected_state_root: bstr[32]
}

CommunityPolicy = {
  migrations:      [bstr[32], ...],
  fork_markers:    [ResolvedForkMarker, ...],
  revoked_invites: [bstr[32], ...]
}
```

Each marker has at least two strictly raw-byte-sorted unique branch heads and
contains `selected_head` exactly once. `migrations` and `revoked_invites` are
strictly raw-byte-sorted unique sets. `fork_markers` is nondecreasing by each
marker's exact canonical CBOR bytes, but duplicate marker bytes are preserved:
the current append-only fold can reach that state, so snapshot import MUST NOT
deduplicate it. Marker times are advisory, and no chronological ordering is
inferred from them.

### 4.3 Full member state is load-bearing

The snapshot MUST NOT substitute the #151 `ProjectedMemberRecord` array for
`members`. The compact projection includes only active devices, while the fold
retains revoked devices and rejects regrant of any already-bound device id. A
restart from the compact projection could therefore accept a transition that a
full-history replica rejects.

After decoding full members, the verifier derives the compact projection and
recomputes `member_root` for the checkpoint. The checkpoint-approved
`snapshot_blob_hash` additionally commits the revoked-device tombstones and all
other restart-relevant full-state bytes that are not distinguished by the
current six-component root.

### 4.4 Snapshot semantic validation

Before a snapshot may seed tail replay:

1. every nested record passes the exact closed schema and reachable-state rules
   in §4.2, including the recovery-key and duplicate-marker preservation
   exceptions;
2. outer identity maps, roles, devices, branch-head sets, revoked invites, and
   migrations satisfy only their explicitly declared order/uniqueness rules;
3. community/cursor fields match the checkpoint exactly;
4. the six component roots and final `state_root` recompute from the decoded
   state;
5. the projected member root and member-map count (including revoked member
   tombstones) match the checkpoint;
6. replica and stream component hashes match the checkpoint;
7. decoded administrators/recovery equal the authority state derived from the
   transition proof; and
8. exact re-encoding equals `snapshot_csb`.

`member_count` counts all keys in the committed member map, including revoked
member tombstones; it is not the active-member count.

---

## 5. Governance checkpoint and quorum certificate

### 5.1 Checkpoint body

`GovernanceCheckpointBodyV1` is this closed map:

| Key | Type | Rule |
|---|---|---|
| `kind` | text | exactly `governance.checkpoint` |
| `format_version` | uint | exactly `1` |
| `schema_version` | uint | exactly `2` |
| `community_id` | bstr[32] | exact snapshot community |
| `checkpoint_seq` | uint | nonzero, monotonically increasing per community |
| `governance_seq` | uint | exact snapshot/proof cursor |
| `governance_tip` | bstr[32] or null | null iff `governance_seq == 0` |
| `state_root` | bstr[32] | exact snapshot root |
| `member_root` | bstr[32] | #151 projected-member root |
| `member_count` | uint | exact member-map key count |
| `stream_manifest_root` | bstr[32] | exact stream component root |
| `replica_set_hash` | bstr[32] | exact replica component root |
| `snapshot_blob_hash` | bstr[32] | hash of uncompressed `snapshot_csb` |
| `snapshot_blob_len` | uint | exact uncompressed byte length |
| `transition_mode` | text | `sparse_linear` or `full_dag` |
| `transition_root` | bstr[32] | hash of exact transition-manifest CSB |
| `transition_proof_len` | uint | exact manifest byte length |
| `transition_count` | uint | exact `transitions` length; includes `fork.resolve` in full-DAG mode |

The canonical encoded key order is:

```text
kind, state_root, member_root, community_id, member_count, checkpoint_seq,
format_version, governance_seq, governance_tip, schema_version,
transition_mode, transition_root, replica_set_hash, transition_count,
snapshot_blob_len, snapshot_blob_hash, stream_manifest_root,
transition_proof_len
```

`stream_manifest_root` and `replica_set_hash` are the existing labeled
`GOVERNANCE_STATE` component roots, not new constructions. A checkpoint is
valid only for a linear, unforked accepted governance state. Current unresolved
fork evidence is a reason to reject checkpoint production/installation, not an
optional field that can be acknowledged and ignored.

`transition_mode` is `sparse_linear` exactly when the selected history contains
no `fork.resolve`; it is `full_dag` exactly when that history contains at least
one. Genesis therefore uses `sparse_linear`. This removes alternate proof-mode
encodings for the same accepted cursor.

`checkpoint_seq` is not v1 `admin_seq`, a governance-entry sequence, a branch
counter, or an authorization input. It is a caller/store high-water mark for
rollback and equivocation handling. Staleness/freshness policy remains a caller
and live-quorum concern.

### 5.2 Quorum certificate

The certificate is a closed canonical map:

```text
GovernanceCheckpointCertificateV1 = {
  signatures: [
    { signer: bstr[32], signature: bstr[64] }, ...
  ],
  checkpoint_id:  bstr[32],
  checkpoint_csb: bstr
}
```

Canonical top-level key order is `signatures, checkpoint_id, checkpoint_csb`;
each signature map orders `signer, signature`. Signatures are strictly sorted
by raw signer bytes and unique. Every signature verifies over
`GOVERNANCE_CHECKPOINT || checkpoint_csb`. The recomputed `CheckpointId` must
equal `checkpoint_id`.

Threshold verification occurs only after the transition manifest derives the
current administrator state. Count the intersection of distinct certificate
signers with that derived set and require at least its threshold. Snapshot-
claimed administrators, old administrators, duplicate signatures, outsiders,
and a signer appearing twice contribute no extra authority. A malformed
derived set or zero/overlarge threshold fails closed.

The certificate signatures do not enter `checkpoint_id`; adding a valid
signature to an otherwise identical certificate does not change the checkpoint
identity.

An administrator MUST sign only after locally validating the complete selected
governance history through the cursor and, in full-DAG mode, every required
branch-dependency record; recomputing the snapshot and every checkpoint field;
auditing the manifest against that evidence; and confirming that governance is
not currently forked. A signer does not delegate these checks to the package
producer; its signature is the completeness attestation that makes sparse spans
usable by a bootstrapper.

---

## 6. Snapshot transfer encoding

### 6.1 Framing and header

A snapshot transfer is:

```text
u32be(header_csb.len) || header_csb || encoded_payload
```

The four-byte header length is at most 4,096. `SnapshotTransferHeaderV1` is:

| Key | Type | Rule |
|---|---|---|
| `kind` | text | exactly `governance.snapshot.transfer` |
| `codec` | text | `identity` or `zstd` |
| `encoded_len` | uint | exact following payload length |
| `checkpoint_id` | bstr[32] | verified checkpoint |
| `format_version` | uint | exactly `1` |
| `uncompressed_len` | uint | exact checkpoint `snapshot_blob_len` |
| `snapshot_blob_hash` | bstr[32] | exact checkpoint commitment |

Canonical key order is:

```text
kind, codec, encoded_len, checkpoint_id, format_version,
uncompressed_len, snapshot_blob_hash
```

The payload consumes exactly `encoded_len` bytes; trailing bytes are invalid.
The header is transfer metadata, not signed state, so every identity/length/hash
field must match the already-verified checkpoint before decompression.

### 6.2 Identity codec

For `identity`, `encoded_len == uncompressed_len` and the payload itself is
`snapshot_csb`. The receiver still hashes before typed decode.

### 6.3 Zstandard codec

`zstd` is one RFC 8878 Zstandard frame with this profile:

- exactly one standard frame; reject skippable or concatenated frames and
  trailing data;
- `Dictionary_ID_Flag == 0`, so the dictionary-id field is absent, and the
  decoder is initialized without a dictionary;
- `Frame_Content_Size` is present and equals `uncompressed_len`;
- the effective window size is at most 8,388,608 bytes: use
  `Frame_Content_Size` for a single-segment frame and the decoded
  `Window_Descriptor` value otherwise;
- decoded output is streamed and aborted before it exceeds either the declared
  length or 16,777,216 bytes;
- RFC reserved and unused frame-header bits are zero;
- a frame checksum may be present and, when present, MUST be verified; and
- the checkpoint hash remains mandatory whether or not a frame checksum exists.

Compressor level, block choices, and checksum choice are not canonical. Two
valid compressed payloads may represent the same snapshot; golden fixtures pin
their decompressed bytes and hash, not one universally required compressor
output.

### 6.4 Receiver order

The receiver MUST perform these checks in order:

1. bound and strict-decode the header;
2. reject unsupported version/kind/codec;
3. compare header fields to the verified checkpoint;
4. bound `encoded_len`, then read exactly that payload;
5. stream identity/decompression with the output/window caps;
6. require exact uncompressed length;
7. compute and compare `snapshot_blob_hash`;
8. strict-decode canonical CBOR and require exact semantic re-encoding;
9. run every snapshot/root/count/authority check in §4.4; and
10. install snapshot, certificate, proof, and cursor atomically.

No decoded field is trusted before step 7.

---

## 7. Authority-transition manifest

### 7.1 Top-level map

`AuthorityTransitionManifestV1` is:

| Key | Type | Rule |
|---|---|---|
| `kind` | text | exactly `governance.transition_manifest` |
| `mode` | text | `sparse_linear` or `full_dag`; must equal checkpoint |
| `final_span` | map | last authority transition (or genesis) through checkpoint |
| `transitions` | array | exact selected-lineage `admin.set`, `recovery.set`, and (full-DAG only) `fork.resolve` records in predecessor order |
| `community_id` | bstr[32] | exact genesis/checkpoint community |
| `format_version` | uint | exactly `1` |
| `governance_seq` | uint | exact checkpoint cursor |
| `governance_tip` | bstr[32] or null | exact checkpoint cursor |
| `schema_version` | uint | exactly `2` |
| `full_dag_entries` | array, conditional | required only in `full_dag`; absent in `sparse_linear` |

Canonical key order is:

```text
kind, mode, final_span, transitions, community_id, format_version,
governance_seq, governance_tip, schema_version, full_dag_entries
```

All listed fields are required except `full_dag_entries`, which follows the
conditional rule above; an explicit empty `full_dag_entries` in sparse mode is
invalid. `governance_tip` remains present as `null` at genesis.
`transition_count` equals `transitions.len()`. Exact manifest length/hash must
match the checkpoint before the manifest can authorize anything.

### 7.2 Exact authenticated entry evidence

Each transition refers to received records without reserializing them:

```text
EntryEvidence = {
  approvals: [
    { csb: bstr, signature: bstr[64] }, ...
  ],
  entry_csb:       bstr,
  entry_signer:    bstr[32],
  entry_signature: bstr[64]
}
```

Canonical key order is `approvals, entry_csb, entry_signer, entry_signature`;
approval-evidence key order is `csb, signature`. Each embedded CSB is bounded
before allocation and must pass the existing exact-CSB decode, signature,
identity, approval-binding, approval-sort, duplicate-approver, and semantic
re-encode checks. `approvals` is strictly ordered by `(approver raw bytes,
BLAKE3(GOVERNANCE_APPROVAL || exact approval csb))`, exactly as in the
normative governance verifier; it is not reordered on receipt.

### 7.3 Transition witness

Each `transitions[]` element is:

```text
TransitionWitness = {
  span:            SpanCommitment,
  entry:           EntryEvidence,
  after_roots:     [bstr[32]; 6],
  before_roots:    [bstr[32]; 6],
  predecessor_csb: bstr | null
}
```

Canonical key order is:

```text
span, entry, after_roots, before_roots, predecessor_csb
```

The root arrays use the frozen order:

```text
administrators, recovery, replicas, members_devices_roles,
stream_manifest, community_policy
```

Hashing the canonical six-element array under `GOVERNANCE_STATE` must equal the
predecessor and transition entry state roots respectively.

For a sequence-1 transition, `predecessor_csb` is null and `before_roots` is the
verified genesis state-root record. Otherwise it is the exact canonical body
CSB of the immediate predecessor. Its derived ID equals the transition entry's
`prev`, its community and sequence are correct, and its declared state root
equals the hash of `before_roots`.

In `sparse_linear`, the transition entry kind is exactly `admin.set` or
`recovery.set`:

- form the distinct union of `entry_signer` and all approval-body approvers,
  intersect it with the derived **old** administrator set, and require at least
  that state's threshold; an outsider may author the entry when `W` old-admin
  approvals independently authorize it, but contributes zero;
- require the administrator and recovery components in `before_roots` to equal
  roots recomputed from the currently derived authority state;
- for `admin.set`, recompute the administrator component from the payload,
  require the other five component roots unchanged, and update only the derived
  administrator state after successful verification;
- for `recovery.set`, recompute the recovery component, require the other five
  roots unchanged, and update only the derived recovery state after successful
  verification; and
- require both authority components in `after_roots` to equal roots recomputed
  from the resulting derived authority state; and
- newly proposed administrators never authorize their own transition.

In `sparse_linear`, sequences are strictly increasing. The witness's span
begins at the preceding transition endpoint (or genesis) and ends at this exact
transition entry. Full-DAG transition selection/order is defined by §8.3.

### 7.4 Span commitment

A span is:

| Key | Type |
|---|---|
| `end_seq` | uint |
| `end_head` | bstr[32] or null |
| `start_seq` | uint |
| `start_head` | bstr[32] or null |
| `entry_count` | uint |
| `entries_root` | bstr[32] |
| `end_state_root` | bstr[32] |
| `start_state_root` | bstr[32] |

Canonical key order is the table order. Heads are null iff their sequence is
zero. Require `entry_count == end_seq - start_seq` using checked arithmetic.

The archival commitment is computed over the exact accepted entries in the
span:

```text
R0 = BLAKE3(GOVERNANCE_SPAN_ANCHOR || canonical_cbor({
  kind:             "governance.span.anchor",
  community_id:     C,
  start_seq:        s,
  start_head:       h_or_null,
  start_state_root: r
}))

Ri = BLAKE3(GOVERNANCE_SPAN_STEP || canonical_cbor({
  kind:       "governance.span.step",
  prior:      R(i-1),
  seq:        entry.seq,
  entry_id:   entry.id,
  state_root: entry.state_root
}))
```

Both preimages are closed maps. The anchor's canonical key order is
`kind, start_seq, start_head, community_id, start_state_root`; the step's is
`seq, kind, prior, entry_id, state_root`. Sequences are uints, `community_id`,
`prior`, `entry_id`, and roots are 32-byte byte strings, and a head is a
32-byte byte string or the explicitly allowed genesis `null`.

`entries_root` is the final `Ri`, or `R0` for an empty span. A full auditor can
recompute it from retained entries. A sparse bootstrapper cannot independently
recompute omitted steps; the derived current-administrator signatures on the
checkpoint authenticate the exact manifest and its completeness assertion.

The span sequence is exact and contiguous. The first transition span starts at
verified genesis. Each later transition span starts at the preceding transition
entry's sequence, id, and state root, and ends at the current transition entry's
sequence, id, and state root. `after_roots` hashes to that end root. If two
transitions are consecutive, the later witness's `predecessor_csb` is byte-
identical to the earlier witness's `entry_csb`. `final_span` starts at the last
transition endpoint (or genesis) and ends at the checkpoint's exact governance
sequence, tip, and state root. Any gap, overlap, alternate head, or boundary
mismatch is invalid.

The final span asserts that no later authority transition was omitted. In an
all-genesis checkpoint it is the empty genesis-to-genesis span.

### 7.5 Why this is not circular

Verification is tentative until the last step:

1. verified genesis supplies administrator/recovery state `A0`;
2. each exact transition is authorized by the currently derived old state and
   then updates it;
3. the final derived state `An` verifies the signatures over checkpoint CSB;
4. those signatures authenticate the checkpoint's manifest root/count/length;
5. the snapshot authority components must equal `An`.

No transition is authorized by a future set, and the untrusted snapshot is not
used to choose checkpoint signers. Omission/reordering/splicing changes the
derived state and/or transition root and cannot pass an honest final quorum.

This construction assumes the applicable administrator threshold does not
collude to certify a false manifest or a snapshot that is incomplete/not fold-
equivalent. In particular, revoked-device tombstone completeness is protected
by the quorum-signed snapshot hash, not independently by the six component
roots. This is the same threshold-honesty boundary used for administrator-
authorized governance; it is not forward-secure history after threshold key
compromise.

---

## 8. Resolved forks and full-DAG mode

Sparse mode MUST reject any `fork.resolve` transition on the selected history,
any competing quorum-valid branch known in the covered history, and any attempt
to treat a policy marker as recovery evidence. `ResolvedForkMarker` values are
also reachable directly from genesis or ordinary administrator-authorized
`policy.set`; those values remain valid sparse-mode state, but only as
checkpoint-attested policy bytes. They neither force full-DAG mode nor prove a
recovery-authorized branch selection. Proof mode is selected by operation
provenance, not by the mere presence of marker-shaped state.

### 8.1 Canonical evidence set

`full_dag_entries` contains the complete dependency closure of the checkpoint
tip: the selected lineage, every named winning/losing branch, every earlier
resolution reachable on any of those branches, and one exact retained
signature/approval evidence set for each entry. It has no sparse gap. The array
is strictly sorted by `(entry body seq, raw bytes of the GovernanceId derived
from exact entry CSB)` and duplicate-free by derived `GovernanceId`. This order
is wire canonicalization only and MUST NOT be fed as observation order to the
landed `GovernanceMachine` API.

After exact-CSB decode/crypto verification, index entries by derived
`GovernanceId`. Every entry targets the manifest community. A sequence-1 entry
has `prev` absent (`None`) under the landed #147 exact entry encoding; every
later entry's `prev` exists in the index and has sequence exactly one lower.
Missing parents, duplicate ids, and parent cycles reject before state
reconstruction.

### 8.2 Order-independent offline reconstruction

The full-DAG verifier performs this memoized dependency fold, not an arrival-
ordered replay:

1. For an ordinary entry, recursively reconstruct its predecessor state (or
   verified genesis), then run the exact #147/#148 branch-local entry,
   authorization, apply, and declared-root checks. A sibling is validated from
   its own predecessor snapshot; observing another sibling does not suppress
   branch-local validation.
2. For `fork.resolve`, recursively reconstruct every signed payload
   `branch_heads` state. Require at least two raw-byte-sorted unique heads, the
   signed selected head exactly once, and pairwise incomparability under the
   full dependency relation (no named head reaches another through `prev` or
   nested `fork.resolve.branch_heads` edges). Compute their unique last common
   uncontested ancestor from predecessor links alone.
   Branch-head edges prove that prior incident evidence was consumed; they are
   not state-parent edges and therefore do not participate in the LCA state.
3. Build the offline incident from exactly that signed head set. Require the
   selected root to equal the already reconstructed selected-head state,
   `prev == selected_head`, and `seq == selected_head.seq + 1` with checked
   arithmetic.
4. From the common-ancestor state, validate recovery-config invariants and
   count the distinct union of resolution entry signer plus approval-body
   approvers intersected with its recovery keys. Require its threshold; current
   administrators and keys installed only on a contested branch contribute
   zero.
5. Apply only the canonical resolution marker to the reconstructed selected-
   head state and require the exact declared post-state root. Memoize that state
   for descendants. There is no administrator fallback.

Dependency traversal covers both `prev` and every `fork.resolve.branch_heads`
edge and uses a three-state (unseen/visiting/done) cycle check. Starting at the
checkpoint `governance_tip`, compute that closure and require exact set equality
with `full_dag_entries`; an unreferenced extra is invalid just like a missing
entry. Genesis uses sparse-linear mode and therefore has no
`full_dag_entries` field.

This algorithm makes late evidence deterministic without encoding arrival
order. A resolution record's signed `branch_heads` declares the incident view
it resolved. Evidence absent from that set is valid as later evidence only when
a later resolution reachable from the final checkpoint closure consumes it as
a named branch or ancestor. Thus an earlier resolution displaced by a later
shorter-branch recovery remains in the losing-head dependency closure and is
validated there; it is not falsely inserted into the final selected lineage.

### 8.3 Selected lineage, transition index, and spans

Walk `prev` from the reconstructed checkpoint tip to genesis and reverse that
list. This is the final selected lineage. `transitions` contains exactly, and
only, each `admin.set`, `recovery.set`, and `fork.resolve` on that list, in that
predecessor order. Each witness's `entry` is byte-identical to its unique
`full_dag_entries` evidence. Authority changes and resolved markers on losing
branches remain in the dependency closure/audit evidence but do not update the
final-lineage authority accumulator or `transition_count`.

Because the complete selected lineage is present, recompute every transition
witness, predecessor, before/after root record, contiguous span, and
`final_span` from those entries. The reconstructed checkpoint tip/root and all
six snapshot components must match. This also handles a later resolution whose
sequence is lower than an earlier displaced resolution: only the later
resolution lies on the final selected lineage, while the earlier one remains
validated in its losing branch.

### 8.4 Completeness limit and fallback

The offline incident view proves the signed branch set represented by the
package; it cannot prove when a remote signer first learned of withheld
evidence. The checkpoint signers' complete-history audit in §5.2 attests that
the package assigns no known branch falsely. Live clients/replicas still compare
heads across independently configured completeness dependencies, and late valid
evidence reopens an incident under the normative fork policy.

If any required lineage/evidence is missing, oversized, cyclic, unused, or
unverifiable, return a missing/invalid proof outcome and fetch/replay governance
from the last trusted anchor. Do not install the snapshot and do not fall back
to administrator authorization. If the complete closure exceeds the format
cap, this checkpoint cannot serve as a snapshot-install boundary; full history
replay remains the fallback until a later format supplies a bounded resolution
proof.

---

## 9. End-to-end verification and installation

A bootstrap caller MUST supply an expected `CommunityId` from an independent
trust anchor, such as a verified invitation ticket, an existing local
configuration, or an operator-pinned value. The package cannot choose its own
trust anchor: a self-consistent, self-signed genesis for a different community
is not an authenticated bootstrap.

A client bootstraps in this order:

1. bound and strict-decode `GenesisEvidenceV1` and its exact `GenesisConfig`
   CSB, verify its threshold signatures, derive `CommunityId`, require equality
   with the caller's expected `CommunityId`, and compute genesis state/root;
2. bound and strict-decode the checkpoint certificate and its embedded
   checkpoint CSB, recompute `CheckpointId`, and validate all body invariants;
3. bound and hash the transition manifest, match the checkpoint's community,
   cursor, mode/hash/length/count, and process sparse-linear or full-DAG mode
   from the verified genesis;
4. verify every certificate signature and require the proof-derived current-
   administrator threshold;
5. receive, decompress, and hash the snapshot using §6;
6. strict-decode the full snapshot and run §4.4, including exact authority-state
   equality with the proof;
7. enforce the persisted checkpoint high-water mark: reject a lower
   `checkpoint_seq`, and at an equal checkpoint sequence require the same
   `checkpoint_id`; for a higher checkpoint sequence, require either (a) the
   same governance tip and equality of all state-deterministic body fields:
   `format_version`, `schema_version`, `community_id`, `governance_seq`,
   `governance_tip`, `state_root`, `member_root`, `member_count`,
   `stream_manifest_root`, `replica_set_hash`, `snapshot_blob_hash`,
   `snapshot_blob_len`, `transition_mode`, and `transition_count`; (b) exact
   retained/fetched governance entries proving the stored tip is an ancestor of
   the candidate selected tip; or (c)
   `full_dag` proof of an applied recovery-authorized resolution on the new
   selected lineage for which the stored governance tip lies in the dependency
   closure (including the root head itself) of a named non-selected head;
   compute that closure over both `prev` and nested
   `fork.resolve.branch_heads` edges exactly as in §8.2, so chained recovery
   supersessions remain valid; a sequence comparison alone proves neither
   extension nor recovery;
8. persist genesis anchor, proof, checkpoint certificate, snapshot bytes/hash,
   full decoded state, and cursor in one transaction; and
9. only after commit, replay and validate the governance tail using the ordinary
   exact-next-sequence/predecessor and authorization rules.

Any failure leaves the prior installed state/cursor authoritative. Temporary
files and reservations are released; partial output never becomes a trusted
snapshot. A fresh install into an empty store still records the exact proof and
certificate needed to audit its trust boundary.

`governance_seq` is not an unconditional rollback or advancement counter:
different branches can be shorter, equal, or longer. Sparse proof normally
cannot show that an arbitrary previously stored tip lies inside an omitted
ordinary span, so a returning client fetches and replays the exact bridge from
that tip before install. Recovery may instead select a shorter valid branch and
append `fork.resolve` at `selected_seq + 1`; stores retain the superseded
checkpoint/head and full recovery evidence while making the higher
`checkpoint_seq` package current. Administrator signatures alone can never
authorize a divergent cursor replacement.

The bridge in step 7(b) is not a new proof encoding. It consists of ordinary
exact-CSB governance records, signatures, and approvals validated from the
stored tip through the candidate tip under the normative log rules.

For the same governance tip, two honest packages can retain different valid
subsets or supersets of detached approval evidence. Their transition-manifest
CSBs—and therefore `transition_root` and `transition_proof_len`—may differ even
though every state-deterministic field listed in step 7(a) is equal. The new
manifest and certificate must still verify completely; proof commitment
difference alone in a higher-sequence re-checkpoint is not same-tip state
equivocation. At an equal `checkpoint_seq`, the exact-`checkpoint_id` rule still
applies.

Verification proves validity at the named cursor, not that it is globally
latest. A client establishes freshness through a ticket/directory-pinned
checkpoint, a persisted high-water mark, or live comparison with an
independently configured/pinned replica quorum. The replica set decoded from
the candidate snapshot cannot vouch that the same candidate is latest. Peer
count, transport connectivity, or one replica's assertion alone does not
advance the high-water mark.

---

## 10. Worked three-transition example

Use deterministic public keys sorted by raw bytes; the symbolic labels below
describe roles, not wire ordering.

Genesis has administrators `{A, B, C}`, threshold 2, and recovery keys
`{R1, R2, R3}`, threshold 2. The verified genesis cursor is `(seq=0, tip=null)`.

| Governance seq | Operation | Required old authority | Result |
|---:|---|---|---|
| 1–4 | ordinary member/stream operations | current admins | no authority record; first span continues |
| 5 | `admin.set` | two distinct of `{A,B,C}` | admins `{B,C,D}`, threshold 2 |
| 6–8 | ordinary operations | `{B,C,D}` | no authority record |
| 9 | `recovery.set` | two distinct of `{B,C,D}` | recovery `{R2,R3,R4}`, threshold 2 |
| 10–13 | ordinary operations | `{B,C,D}` | no authority record |
| 14 | `admin.set` | two distinct of `{B,C,D}` | admins `{C,D,E}`, threshold 2 |
| 15–20 | ordinary operations | `{C,D,E}` | final span to checkpoint |
| checkpoint | — | two distinct of `{C,D,E}` | cursor `(20, H20)` |

The manifest has three transition witnesses and four span commitments:
genesis→5, 5→9, 9→14, and final 14→20. Verification proceeds:

1. genesis signatures derive `{A,B,C}` / `{R1,R2,R3}`;
2. seq 5 verifies under `{A,B,C}`, then derives `{B,C,D}`;
3. seq 9 verifies under `{B,C,D}`, then derives recovery `{R2,R3,R4}`;
4. seq 14 verifies under `{B,C,D}`, then derives `{C,D,E}`;
5. two signatures from `{C,D,E}` authenticate the checkpoint and its exact
   three-record transition manifest; and
6. the snapshot must contain `{C,D,E}` / `{R2,R3,R4}`, cursor `(20,H20)`, and
   roots/counts matching the checkpoint.

Removing or moving the seq-9 record changes the manifest hash and derived
recovery state. Letting `{C,D,E}` approve seq 14 would fail because the old set
for that entry is `{B,C,D}`. A checkpoint signed by `{A,B,C}` after the final
transition is under the wrong derived authority and fails.

---

## 11. Compaction and historical authorization

A verified snapshot is a restart/authorization boundary for the current
governance state and its later governance tail. It is not, by itself, proof that
a retained content event was authorized at an earlier publication head.

The current content `auth_hint_seq` is signed advisory input, not a historical
membership proof. The latest member record also retains only the latest
grant/revoke interval, so it cannot reconstruct arbitrary earlier authority.
Until a separately accepted historical-authorization/publication-certificate
proof schema exists:

- do not validate pre-snapshot content against the latest snapshot as if it were
  historical state;
- retain governance entries, checkpoint/member proofs, publication
  certificates, and fork evidence needed by retained content;
- do not advance compaction past those dependencies; and
- treat the historical-authorization schema as a v2 stable-wire prerequisite,
  consistent with the reconciliation checklist.

Resolved-fork losing evidence, the full-DAG proof material needed to audit a
resolution, and the exact checkpoint/proof bytes are never discarded merely
because the selected snapshot installed successfully.

---

## 12. Fixture and conformance plan

### 12.1 Positive frozen vectors

Before advertising format version 1, freeze exact canonical bytes/hashes/
signatures for:

1. exact genesis-evidence wrapper plus genesis-only snapshot and checkpoint
   (`governance_tip = null`, zero transitions, empty final span), pinning the
   wrapper, checkpoint-certificate, and identity transfer-header CSBs;
2. a small snapshot with every component populated, including one revoked member
   and revoked device tombstone, absent and present optional member fields, an
   exact reachable unsorted/duplicate genesis recovery-key representation, and
   duplicate sorted genesis/`policy.set`-origin fork markers accepted in sparse
   mode without recovery provenance;
3. the complete three-transition example in §10, including entry and approval
   CSBs/IDs/signatures, all before/after root records, spans, manifest CSB/root,
   checkpoint body and certificate CSBs/ID/signatures, and snapshot
   CSB/hash/length;
4. a transition authored by an outsider with `W` valid old-administrator
   approvals, proving that only the authority-set intersection counts;
5. no-op `admin.set` and `recovery.set` entries, proving that transition
   inclusion/count is operation-kind based rather than value-change based;
6. `identity` and checksum-present/checksum-absent valid `zstd` transfers that
   produce the same snapshot bytes/hash, with every exact transfer-header CSB
   pinned;
7. one resolved-fork `full_dag` proof selecting the lexically larger head and
   one selecting the smaller head;
8. a multi-incident late-branch proof in which a later recovery selects a
   shorter lineage and an earlier displaced resolution remains only in the
   losing-head closure, not `transitions`; the same vector pins high-water
   acceptance when the stored tip is reached only through that nested losing
   resolution's dependency closure;
9. a same-tip higher-sequence re-checkpoint whose valid detached approval
   subset/superset changes the manifest commitment but not any
   state-deterministic field in §9 step 7(a); and
10. a full 10,000-member governance snapshot, reporting snapshot, proof, and
   total bootstrap bytes separately from #160's projected-member-only number.

The canonical, uncompressed objects must be independently reproduced by a
second implementation. The legacy candidate checkpoint fixture remains
byte-identical and is exercised separately.

### 12.2 Structural and cryptographic negatives

One-fault fixtures cover:

- every strict-CBOR failure class, unknown kind/version/key, wrong type/width,
  illegal null, unsorted/duplicate semantic arrays, and semantic re-encode drift;
- malformed/oversized genesis CSB or wrapper, unsorted/duplicate/non-admin
  genesis signer, and `W-1` genesis signatures;
- a self-consistent alternate genesis/package whose derived community differs
  from the caller-pinned `CommunityId`;
- wrong community/cursor/checkpoint/proof/snapshot binding;
- bad entry, approval, or checkpoint signatures;
- `W-1`, duplicate, old-set, and self-authorizing new-set checkpoint or
  transition authority; outsider signatures contribute zero, and a fixture
  with outsiders plus fewer than `W` eligible signers fails;
- omitted, extra, reordered, duplicated, or cross-community/branch-spliced
  transition;
- wrong transition predecessor CSB, before/after root record, unchanged
  component, span boundary/root/count, final span, proof count/length/root, or
  final derived authority;
- sparse mode containing selected-lineage `fork.resolve`, full-DAG mode over an
  otherwise linear/no-resolution selected history, incomplete branch set,
  wrong selected root, admin fallback, contested-branch recovery key, and
  unresolved-fork checkpoint production;
- `full_dag_entries` in wrong `(seq, id)` order, duplicate id, missing
  predecessor or named branch dependency, unreferenced extra entry, parent or
  dependency cycle, and pairwise dependency-comparable incident heads;
- a stale re-resolution naming an earlier resolution together with a branch
  head already consumed in that resolution's dependency closure; a genuinely
  late descendant of the old losing head remains incomparable and is covered
  by the positive multi-incident vector;
- checkpoint/member/stream/replica roots or counts differing from decoded state;
- omission of a revoked device tombstone from the payload while the checkpoint
  and certificate remain fixed; projected member/state roots stay unchanged,
  but snapshot-hash verification fails;
- rollback below the persisted checkpoint high-water mark, same-sequence
  checkpoint equivocation, and governance cursor regression without the exact
  full-DAG recovery supersession required by §9;
- a higher checkpoint sequence with the same governance tip but a different
  state/root/count/snapshot commitment, transition mode, or transition count;
- a numerically higher governance sequence on a divergent, non-recovered head;
- positive recovery from a stored longer branch to a shorter selected branch,
  with a higher checkpoint sequence and the stored tip in the transitive
  dependency closure of a named non-selected head;
- a forged policy marker or mere dependency-closure presence outside such a
  final-lineage recovery supersession attempting to replace the stored tip;
- a latest-snapshot attempt to authorize older retained content without the
  required historical proof.

### 12.3 Compression and resource negatives

Cover exact boundary and boundary+1 for every §3 cap, plus:

- encoded/decompressed length mismatch, truncation, output overrun, and a
  high-ratio decompression bomb;
- Zstandard dictionary flag/decoder dictionary, missing/wrong frame content
  size, window over 8 MiB, nonzero reserved/unused bit, skippable frame,
  concatenated frames, trailing bytes, and bad present checksum;
- valid checksum-present and checksum-absent frames producing identical
  uncompressed bytes/hash;
- hash mismatch checked before CBOR decode;
- declared collection sizes rejected before proportional allocation; and
- failed installation leaving the previous transaction/cursor byte-identical.

### 12.4 Properties

Property tests generate bounded administrator/recovery sets, thresholds,
ordinary gaps, authority transitions, device tombstones, and arrival
permutations to prove:

- old-state threshold induction and no early new-set authority;
- manifest order/hash is deterministic for identical exact evidence, and fold
  state is identical across valid detached approval subsets/supersets;
- no omitted transition can pass an honest final checkpoint quorum;
- snapshot encode/decode preserves tail-validation behavior;
- member/state roots plus snapshot hash all reproduce;
- compressed representations do not affect snapshot identity; and
- failed proof/decode/install never advances trusted state.

---

## 13. Normative amendments to #134

### 13.1 Domain inventory (§6.2)

On acceptance, extend #134 §6.2's minimum domain inventory with the four exact
ASCII values in §2.2 for `GOVERNANCE_SNAPSHOT`,
`GOVERNANCE_TRANSITION_MANIFEST`, `GOVERNANCE_SPAN_ANCHOR`, and
`GOVERNANCE_SPAN_STEP`. They are distinct hash purposes. Existing
`GOVERNANCE_CHECKPOINT` continues to identify/authenticate the checkpoint
record boundary; none of the four new hashes reuse it.

### 13.2 Snapshot and transition proof (§7.6)

On acceptance, read #134 §7.6's snapshot and sparse-transition paragraphs with
this addition:

> A v2 governance snapshot, checkpoint, quorum certificate, transfer header,
> and authority-transition manifest use the exact version-1 formats in
> `specs/v2-governance-snapshot-transition-proof.md`. Snapshot identity is the
> domain-separated hash of the uncompressed strict canonical-CBOR full fold
> state, including revoked-device tombstones. Compression is an independently
> declared, bounded transfer representation and is never part of snapshot
> identity.
>
> A checkpoint commits the exact snapshot and transition-manifest hashes,
> lengths, counts, roots, and governance cursor. The verifier derives current
> administrators inductively from verified genesis and every accepted
> `admin.set`/`recovery.set` witness before counting signatures on the
> checkpoint. The current-administrator quorum attests that the manifest is
> complete across omitted ordinary governance spans. A sparse list not bound by
> that checkpoint quorum is not a transition proof.
>
> Histories containing `fork.resolve` use full-DAG proof mode and recovery
> authority from the last common uncontested ancestor; sparse-linear mode and
> administrator fallback are forbidden. No checkpoint may become an
> authorization or compaction boundary while governance is unresolved/forked.

This amendment freezes §25 item 3's format. It does not implement Phase C,
establish checkpoint freshness from a single peer, define historical content-
authorization proofs, or authorize compaction past retained dependencies.

---

## 14. Stable-wire and implementation gate

The schemas and semantics above are frozen when this spec is accepted. Any
field, omission/null rule, kind value, hash preimage, proof mode, or compression
semantic change requires a new format version and vectors.

Format version 1 MUST NOT be advertised until all are true:

1. strict exact-CSB codecs implement every object and rejection rule;
2. all four additive domains are exposed, byte-pinned, uniqueness-tested, and
   covered by independent hash vectors;
3. streaming identity/Zstandard transfer enforces all preallocation, window,
   frame, output, length, and hash gates;
4. checkpoint quorum and transition induction use the normative
   `governance::log` types and frozen domains;
5. sparse/full-DAG proofs and every §12 negative pass;
6. snapshot installation and rollback checks are transactional;
7. the complete 10,000-member fixture re-runs the less-than-5-MiB bootstrap
   target and records any reopening decision;
8. a second implementation reproduces uncompressed bytes/hashes/signatures for
   identical exact evidence;
9. historical-authorization dependencies have an accepted owner/schema before
   compaction claims they are disposable; and
10. the legacy candidate fixture/decoder remains byte-identical and cannot be
   confused with this family; and
11. the #157 compatibility ruling and applicable additive endpoint-profile/
    snapshot-format vectors exist before stable replica state is advertised;
    format 1 remains byte-identical, and no descriptor-hash value is relabeled
    as a signing public key.

No public v2 interoperability claim may treat the closed #150 candidate issue
or its legacy vector as satisfying this gate.

---

## References

- [RFC 8949](https://www.rfc-editor.org/rfc/rfc8949), *Concise Binary Object
  Representation (CBOR)*.
- [RFC 8878](https://www.rfc-editor.org/rfc/rfc8878), *Zstandard Compression and
  the `application/zstd` Media Type*.
- ADR-0008, *Full-State Governance Snapshots with Checkpoint-Bound Authority
  Proofs*.
- ADR-0009, *Separate Replica Signing Keys from Iroh Endpoint Keys*.
- [`v2-replica-endpoint-identity.md`](v2-replica-endpoint-identity.md).
