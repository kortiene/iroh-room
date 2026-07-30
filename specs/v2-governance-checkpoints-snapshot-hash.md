# Spec: v2 Governance Checkpoints and Snapshot Hash

| | |
|---|---|
| **Issue** | #150 — `[CORE] v2 governance checkpoints + snapshot hash (#134 §7.6)` |
| **Labels** | `type/feature` `area/protocol` `priority/p1` `risk/medium` |
| **Refs** | #134 §7.6; `specs/v2-crypto-core-crate.md` §3.1 #5 / §4 / §8 Step 9; depends on #147 (state root) and #151 (member root); pinned by #153 |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/src/governance/checkpoint.rs`, with root recomputation via `governance/state_root.rs` + `member/projection.rs`, and byte-pinned golden vectors in `tests/golden/v2-signed-records.json` driven by `tests/signed_records_golden.rs` (+ `governance/checkpoint.rs` unit tests). How the §5 open questions were resolved against what shipped is recorded in §7. |

---

## 1. Summary

A governance checkpoint is a signed commitment to a folded governance state at a
point in time. It binds four independently-recomputable roots — the governance
`state_root`, the member `member_root`, the committed `governance_tip`, and the
unresolved-fork evidence set — plus an advisory epoch/seq, into one record whose
identifier *is* the snapshot hash:

```text
CheckpointBody = {
  schema_version, room_id, state_root, member_root,
  governance_tip?, unresolved_forks?, epoch, seq
}
snapshot_hash = BLAKE3(SNAPSHOT_HASH || canonical_cbor(CheckpointBody))
```

The checkpoint's envelope id is its snapshot hash (so a checkpoint is self-
identifying). Validation is pure and fail-closed: a receiver recomputes the
member root from the supplied folded state, verifies the declared state root,
checks the unresolved-fork commitment, recomputes the snapshot hash, and rejects
on any mismatch. A checkpoint commits to *all* authorization-relevant state
(spec §11), so any semantically relevant change to members, governance tip, or
unresolved forks changes the snapshot hash.

This work is pure and unused by the shipped v1 runtime. It adds no transport,
storage, async runtime, or checkpoint *publication* (no replica receipts, no
certificate scheme) — those are later, separate issues.

---

## 2. Record model

### 2.1 Canonical body

```rust
pub struct CheckpointBody {
    pub schema_version: u64,            // MUST be 2 (SCHEMA_VERSION)
    pub room_id: RoomId,
    pub state_root: StateRoot,          // BLAKE3(GOVERNANCE_STATE_ROOT || …)
    pub member_root: MerkleRoot,        // §8.2 sorted-merkle root (#151)
    pub governance_tip: Option<GovernanceEntryId>,
    pub unresolved_forks: Vec<[GovernanceEntryId; 2]>,
    pub epoch: u64,                     // advisory signed epoch
    pub seq: u64,                       // per-room monotonic checkpoint seq
}
```

Canonical-CBOR encoding rules (strict profile, see `cbor.rs`):

- map keys are emitted in canonical sorted order;
- `governance_tip` is **omitted** when `None` (not emitted as `null`);
- `unresolved_forks` is **omitted** when empty (not emitted as `[]`).

The two omissions are load-bearing: because the snapshot hash is taken over the
exact canonical bytes, an explicit `unresolved_forks: []` (or an explicit
`governance_tip: null`) would hash differently from the omitted form. The decoder
rejects unknown keys, wrong schema versions, wrong-width id fields, and malformed
fork-evidence pairs (each pair must be exactly two 32-byte entries).

### 2.2 Signature and identifier

`CheckpointBody: SignedBody` with:

- `type Id = SnapshotHash`;
- `SIGN_CONTEXT = domain::CHECKPOINT_SIGN` (`iroh-rooms:v2:checkpoint:sign:v1` legacy alias);
- `ID_CONTEXT = domain::SNAPSHOT_HASH` (`iroh-rooms:v2:snapshot-hash:v1` legacy alias).

The signing message is `CHECKPOINT_SIGN || canonical_cbor(body)`; the Ed25519
signature verifies under the checkpoint signer's principal key. The id is
`BLAKE3(SNAPSHOT_HASH || canonical_cbor(body))` — the signature is **not** part
of the id preimage, and there is no claimed-id envelope field: a caller that
wants to pin an expected snapshot hash must compare it to the recomputed value.

```rust
pub type SignedCheckpoint = Envelope<SnapshotHash>;
```

---

## 3. Validation

Two entry points, in keeping with the crate's "stateless signed-record decode
vs. stateful fold validation" split:

- **`decode_verified(env)`** — verifies canonicality, id (snapshot hash), and
  Ed25519 signature over the exact retained CSB. Carries no folded state.
- **`validate_against_state(env, state)`** — the root-recompute gate:

  1. `decode_verified(env)`;
  2. recompute `(member_root, _projection)` from `state` via `projection::project`;
  3. `state_root::verify(state, member_root, body.state_root)` →
     [`Reject::StateRootMismatch`] on divergence;
  4. the declared `unresolved_forks` must equal the state's actual unresolved
     (`!resolved`) fork evidence pairs, else [`Reject::InvalidContent`];
  5. recompute `snapshot_hash(&body)` and compare to `env.id` →
     [`Reject::SnapshotHashMismatch`] on divergence.

### 3.1 Error taxonomy mapping

| Outcome | `Reject` code |
|---|---|
| malformed / non-canonical CBOR | `non_canonical_encoding` |
| `schema_version != 2` | `unknown_version` |
| wrong-width id / malformed fork pair | `non_canonical_encoding` / `invalid_content` |
| bad Ed25519 signature | `bad_signature` |
| envelope id ≠ recomputed snapshot hash | `snapshot_hash_mismatch` |
| declared `state_root` ≠ recomputed | `state_root_mismatch` |
| declared `unresolved_forks` ≠ state's | `invalid_content` |

---

## 4. Frozen golden vectors (#153)

Pinned in `tests/golden/v2-signed-records.json` + `tests/signed_records_golden.rs`
(+ `governance/checkpoint.rs` unit tests). Deterministic public test seeds only.

- **Positive** `governance-checkpoint-clean-state-v1` — frozen CSB, snapshot-hash
  id, signature, declared `state_root`, and `member_root` over the canonical
  genesis + add-member fold; asserts CSB equality, strict decode round-trip,
  domain-separated snapshot-hash equality, signature equality, and full
  `validate_against_state` success.
- **Negative** `state-root-mismatch` — checkpoint body with `state_root` replaced
  by `[0xff; 32]`; `validate_against_state` rejects.
- **Negative** `snapshot-hash-mismatch` — the only public path that fires this
  code today: a CSB carrying an explicit empty `unresolved_forks: []` array
  diverges from the re-encoded body (which omits it), so the recomputed snapshot
  hash differs from the id pinned over the original CSB.
- **Unit** `member_state_change_changes_snapshot_hash` — any accepted member
  transition changes the snapshot hash (the checkpoint commits to member state).

Any drift in a frozen byte/hash requires an explicit schema-version bump (see
`tests/golden/README.md`).

---

## 5. Open questions for maintainer/protocol review

- **OQ-1:** Is checkpoint `seq` global-per-room, per-branch, or per-author?
- **OQ-2:** Does `snapshot_hash` commit to unresolved fork evidence directly, or
  only to accepted/resolved governance state plus a separate fork set?
- **OQ-3:** What fields are mandatory in #134 §7.6 checkpoints, and is `epoch`
  authoritative or advisory?
- **OQ-4:** Should a stale checkpoint (older `seq` than the receiver's current
  tip) be rejected at this layer, or is that a caller policy?

---

## 6. Verification

```bash
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo test -p iroh-rooms-v2-core --test signed_records_golden
cargo tree -p iroh-rooms-v2-core   # banned-dependency guard (no iroh/tokio/rusqlite)
```

The crate is `publish = false`, unused by the shipped v1 runtime, and has no
network/store/async dependencies.

---

## 7. Implementation notes (post-landing)

How the §5 open questions were resolved against what shipped in
`src/governance/checkpoint.rs`:

- **OQ-1 (seq scope):** resolved as **per-room monotonic**. `CheckpointBody.seq`
  is a per-room checkpoint sequence number; `epoch` is an advisory signed epoch
  carried from the governance log. No per-branch or per-author split is modeled
  at this layer. Both are checked only for presence + uint encoding; ordering
  policy is deferred to the caller (no wall-clock authorization, spec §11).

- **OQ-2 (fork commitment):** resolved as **commits directly**. The declared
  `unresolved_forks: Vec<[GovernanceEntryId; 2]>` is part of the hashed body and
  is validated against the folded state's actual unresolved evidence
  (`state.forks` with `!resolved`). Peers therefore cannot silently disagree
  about unresolved fork state; fail-closed authorization under forks (#149) is
  hash-visible. Resolved fork evidence is excluded from the commitment.

- **OQ-3 (mandatory fields):** resolved per the body in §2.1. All eight fields
  are mandatory in the typed struct; on the wire, `governance_tip` and
  `unresolved_forks` are omitted when empty/absent (§2.1 omissions). `epoch` is
  advisory (signed but not used for authorization). `schema_version` must equal
  the crate `SCHEMA_VERSION` (`2`) else `unknown_version`.

- **OQ-4 (stale checkpoints):** resolved as **caller policy**. This pure layer
  does not reject an older `seq`; it only proves the checkpoint's roots are
  consistent with a supplied folded state. Stale-tip rejection is a runtime/
  store concern for a later issue.

**Notes on the exact-byte trust boundary.** `SignedCheckpoint` is
`Envelope<SnapshotHash>`; the envelope retains the exact received CSB and verifies
signature + identity over those bytes (never a reserialization), mirroring
`crate::signed::Envelope`. The id is recomputed from the CSB, not trusted from a
claimed field. The snapshot-hash-mismatch negative (§4) is reachable precisely
because the decoder pins the id to the exact CSB while the body re-encode omits
empty `unresolved_forks` — a deliberate fail-closed surface that catches any
future canonicalization drift.

**Deferred scope.** No checkpoint publication, replica receipts, certificate
chains, or transport binding live here; a receiver must supply the folded state
to `validate_against_state`. Wiring checkpoints into a store/network crate is a
separate, later issue (epic spec §12).
