# ADR-0008: Full-State Governance Snapshots with Checkpoint-Bound Authority Proofs

- **Date:** 2026-08-03
- **Status:** Proposed — accepted when this decision is merged
- **Owners:** Protocol lead, release owner
- **Issue:** #161 — `[SPEC] §25 #3: Governance snapshot encoding + admin-transition proof`
- **Related:** #134 §§7.6, 14, 15; #147–#151; #160; ADR-0004
- **Normative profile:** [`specs/v2-governance-snapshot-transition-proof.md`](../../specs/v2-governance-snapshot-transition-proof.md)

## Context

#134 requires a new client to verify genesis, a sparse chain of administrator
and recovery transitions, a quorum-approved governance checkpoint, the complete
snapshot, and the governance tail. It must not replay the complete governance
or content history merely to establish current membership.

The already-frozen `governance::checkpoint` candidate does not implement that
boundary. It uses the pre-#146 `RoomId`, `SnapshotHash`, and legacy domains; its
single signature covers an eight-field checkpoint body; and its so-called
snapshot hash is the hash of that checkpoint body. It has no snapshot blob,
blob length, member count, stream or replica commitment, current-administrator
quorum, or transition proof. The repository explicitly identifies
`governance::log` as the normative v2 governance path and the sibling modules as
candidate scaffolding.

Two less obvious gaps also matter:

1. A list containing only `admin.set` and `recovery.set` entries is not by itself
   a sparse proof. A governance entry names only its immediate predecessor. If
   ordinary entries are omitted, their absence, ancestry, and lack of another
   authority transition cannot be established from the listed hashes alone.
2. The compact member projection commits active devices only, while the fold
   retains revoked-device tombstones and rejects a later regrant of any retained
   device id. Two restart states can therefore have the same current member and
   state roots but make different future decisions. A restart snapshot must
   preserve and authenticate the full fold state, not only the compact roster.

Changing the frozen governance-entry body to add skip pointers would reopen all
entry IDs, approvals, signatures, and golden vectors. #161 can close the sparse
bootstrap gap without doing that: the final administrator quorum can approve a
checkpoint that commits the complete authority-transition manifest. Old-state
entry approvals establish each transition inductively; the final checkpoint
approval attests that the manifest is complete across the omitted ordinary
spans.

## Decision

Define a new, separately discriminated normative v2 checkpoint family over the
landed `governance::log` model. Preserve the legacy checkpoint decoder and
golden vector byte-for-byte, but give them no #134 §7.6 interoperability status.

The new family retains the frozen `iroh-room-v2/governance-checkpoint` boundary
for checkpoint IDs/signatures and additively freezes four distinct #134 §6.2
hash domains: `iroh-room-v2/governance-snapshot`,
`iroh-room-v2/governance-transition-manifest`,
`iroh-room-v2/governance-span-anchor`, and
`iroh-room-v2/governance-span-step`. #134 calls its original eleven domains a
minimum and requires distinct purposes; Phase C must extend #146's byte-pinned
inventory before advertising this format. Closed record-type discriminants
provide a second structural defense. None of these values is the legacy
`SnapshotHash`.

### Snapshot and checkpoint

The uncompressed snapshot is one strict canonical-CBOR map containing the
community id, accepted governance cursor, and all six normative governance
components. Its member array contains full `MemberRecord` values, including
revoked devices. The snapshot hash is over those exact uncompressed canonical
bytes. A checkpoint commits the snapshot hash and length, state/member/stream/
replica roots and counts, the governance cursor, and the transition-manifest
hash/count/length.

The checkpoint certificate carries a sorted, duplicate-free set of Ed25519
signatures over the exact checkpoint CSB. The verifier derives the current
administrator set from genesis plus the transition proof before counting those
signatures. A set merely claimed by the snapshot cannot authorize its own
checkpoint.

### Sparse authority proof

The sparse mode contains exactly every accepted `admin.set` and `recovery.set`
on the selected linear history, plus checkpoint-certified span commitments
covering the ordinary entries between them and from the final transition
through the checkpoint. Each transition witness retains the exact governance
entry and approval CSBs/signatures, its immediate predecessor CSB, and the
before/after six-component root records. The reduced verifier checks old-admin
authorization and recomputes the affected administrator or recovery component;
the checkpoint's derived current-administrator quorum authenticates manifest
completeness across the skipped spans.

This is a quorum-certified sparse authority proof, not a standalone Merkle
proof of every omitted entry. That distinction is deliberate and matches the
protocol's safety assumption: governance integrity holds while the applicable
administrator threshold is not compromised. A future history accumulator may
make skipped ancestry independently provable, but it is not needed for v2.0.

### Fork histories

`fork.resolve` is not administrator-authorized and cannot appear in sparse
linear mode. When the selected checkpoint history includes `fork.resolve`,
proof mode is `full_dag`: the package contains every authenticated governance
record and losing-branch record needed to reproduce fork detection and
resolution from genesis through the checkpoint. The verifier uses an order-
independent, memoized dependency fold that reproduces the existing fork-aware
machine's branch-local validation and recovery rules; canonical wire sorting is
never treated as observation order. Recovery authority comes from the last common
uncontested ancestor. If that material is absent or exceeds a negotiated
transfer, the node must fetch and replay it before installing the snapshot.
There is no administrator-signature fallback.

Because the landed fold also permits marker-shaped policy values directly in
genesis and `policy.set`, a marker's presence alone does not select full-DAG
mode or prove recovery. The selected history's actual operation provenance
does.

No authorization checkpoint may be created or installed while governance is
currently forked. Conflicting or late branch evidence remains an online
cross-replica concern; a static package cannot prove that no branch was
withheld.

### Compression

Snapshot identity is independent of compression. The checkpoint always names
the uncompressed canonical bytes. Transfer encoding is declared in a bounded
header and is either `identity` or one RFC 8878 Zstandard frame. Zstandard uses
no dictionary, no skippable or concatenated frames, a declared content size,
and at most an 8 MiB window. Decompression is streamed into a fixed output cap;
length and hash are checked before CBOR decoding.

The protocol hard cap is 16 MiB for either encoded or decoded snapshot bytes.
The separate #134 target remains less than 5 MiB for the complete 10,000-member
bootstrap. #160's 1,019,725-byte result covered only projected member records,
so the final full-state fixture must rerun that target before wire advertising.

## Compatibility and implementation boundary

- The legacy `governance-checkpoint-clean-state-v1` fixture, decoder, legacy
  domains, and candidate `SnapshotHash` remain untouched.
- They are historical Phase-B candidate evidence, not aliases for the new
  checkpoint, snapshot, or transition records.
- The new maps use `format_version = 1`, `schema_version = 2`, `CommunityId`,
  `GovernanceId`, `StateRoot`, and `CheckpointId` from the normative log path.
- No implementation or new golden bytes land in this pure-spec decision. Phase C
  must add the four new domain constants and hash vectors, strict codecs,
  exact-CSB ownership, decompression guards, independent format vectors, and
  atomic store installation before advertising the format.
- This decision neither reads nor changes v1 `admin_seq`. In particular, it does
  not scope that community-global derived ordering/depth value; ADR-0006
  remains unchanged.

## Completeness boundary

A verified snapshot establishes current governance state at one exact
checkpoint and supplies the deterministic state from which to validate the
tail. Verification starts from a caller-supplied expected `CommunityId`; a
package-supplied, self-signed genesis cannot choose that trust anchor. The
snapshot does not prove that the checkpoint is the newest; freshness still
requires a persisted high-water mark, a separately pinned checkpoint, or live
quorum comparison.

It also does not prove historical authorization for content published before
the snapshot. The current v2 content `auth_hint_seq` is not such a proof, and a
latest-state roster cannot reconstruct every prior grant/revoke interval.
Governance entries, publication certificates, and historical-authorization
dependencies needed by retained content must not be compacted until that
separate schema is accepted and verified end to end.

## Alternatives rejected

### Extend the legacy checkpoint body in place

Rejected. It would silently reinterpret frozen legacy-domain bytes from the
wrong governance model and still would not supply a snapshot or sparse proof.

### List transition entry IDs without checkpoint binding

Rejected. Immediate predecessor hashes do not prove ancestry or the absence of
an omitted transition across a gap.

### Encode only the compact member projection

Rejected. It drops revoked-device tombstones that affect future `device.grant`
validity, so a snapshot restart could disagree with a full-history fold.

### Accept `fork.resolve` in the compact linear proof

Rejected. Resolution authority comes from the last common uncontested
ancestor's recovery set and requires branch evidence. Treating it as an ordinary
admin transition would authorize with the wrong principal set.

### Hash compressed bytes

Rejected. Compressor/version/settings differences would create different
snapshot identities for identical canonical state and encourage decode before
the authoritative content hash is known.

## Consequences and review triggers

- Phase C needs a full `GovernanceState` snapshot codec, not merely the member
  projection encoder.
- The final snapshot hash provides the missing checkpoint-approved commitment to
  revoked-device tombstones without changing frozen #147 entry/state-root bytes.
  The broader claim that the six-component state root alone commits every
  future-validation input remains a protocol debt and must be reviewed if any
  non-checkpoint state import is introduced.
- Snapshot installation is atomic only after proof, checkpoint quorum, decoded
  hash/length, semantic invariants, roots, counts, and cursor all agree.
- Revisit this decision if the full 10,000-member fixture misses the 5 MiB
  bootstrap target, a standalone history proof is required after admin-threshold
  compromise, or resolved-fork evidence makes the full-DAG fallback operationally
  unbounded.
