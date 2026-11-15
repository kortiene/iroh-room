# ADR-0007: Meyer-Style Range Reconciliation Behind an iroh-room-Owned Envelope

- **Date:** 2026-08-03
- **Status:** Proposed — accepted when this decision is merged
- **Owners:** Protocol lead, release owner
- **Issue:** #155 — `[SPEC] §25 #1: Range-reconciliation algorithm + spec-owned envelope`
- **Related:** #134 §§12.2–13.4 / §25 #1, ADR-0004, #161
- **Normative profile:** [`specs/v2-range-reconciliation-envelope.md`](../../specs/v2-range-reconciliation-envelope.md)

## Context

#134 requires v2 to reconcile the lexicographically ordered `EventId` set for
one `(community_id, stream_id, retention_generation, checkpoint_id)` scope.
The work must be difference-sensitive, byte/CPU/depth bounded, resumable, and
incapable of turning an adversarial response into a completeness proof. #134's
phrase "cost proportional to set difference plus logarithmic range traversal"
does not say whether that bound is literally additive. Event bodies are fetched
only after missing ids are known and are then validated independently.

Aljoscha Meyer's range-based set reconciliation (RBSR) is a good algorithmic
shape for that job: compare a fingerprint over an ordered range, stop on a
match, and rank-split a mismatch until it is cheaper to enumerate ids. It does
not define a production wire format, stable-view binding, fragmentation,
budgets, replay handling, resumption, or an application completeness state.
Its correctness is also conditional on no feasible adversary finding a
fingerprint collision except with negligible probability.

`iroh-docs` 0.101.0 implements a related algorithm, but its current boundary is
not the v2 boundary:

- its `ranger` module, ranges, fields, and tuning configuration are private;
- its public protocol message is specialized to `SignedEntry`, including
  document timestamp/LWW and prefix-deletion semantics rather than a plain
  `EventId` set;
- its private Postcard envelope is capped at 1 GiB, rather than #134's 64 KiB
  control-frame body;
- range length and fingerprint queries scan the selected redb range; and
- it has no cumulative work budget or persisted resume cursor.

Those are point-in-time implementation facts, not a judgment that the upstream
crate can never become useful. They do mean that exposing its current messages
would make an unstable dependency encoding the v2 compatibility contract while
still failing #134 §13.2.

## Decision

Adopt **Meyer-style binary range refinement** as the v2 inventory-discovery
algorithm, behind a **versioned, canonical-CBOR envelope owned by iroh-room**.

The normative profile makes four deliberate refinements to the paper:

1. A pass is directional and reads two fixed views. Its primary transfer result
   is ids present in the responder's pinned view but absent locally. At each
   terminal mismatching range, the initiator can also audit its local ids against
   the responder's complete range listing and identify local-only ids. Replica
   union still uses two passes so that each replica learns what it must fetch.
   Neither side mutates the inventory being fingerprinted during a pass.
2. A mismatch is split at the responder's exact rank median. The two child
   counts must differ by at most one and must sum to the parent count. This
   balance rule is stronger than the paper's stated "at least two non-empty
   subranges" rule and is required for logarithmic honest-peer depth.
3. Explicit ids are pull-paginated. One request yields at most one bounded
   response, so neither endpoint waits while holding an unbounded response and
   the paper's two-small-buffer deadlock cannot arise.
4. Fingerprints use the `RBSR_RISTRETTO255_SHA512_BLAKE3_V1` suite: each
   `EventId` is mapped to a Ristretto255 group element with RFC 9380's
   `hash_to_ristretto255` instantiated with XMD/SHA-512, range elements are
   added, and the canonical sum is bound to scope/range/count with BLAKE3.
   This ECMH-style construction retains an associative cached aggregate without
   adopting the polynomial-time collision weakness of XOR-of-hashes. Its
   intended classical collision-work target is approximately 2^126 in the
   random-oracle/generic-group model, assuming hard Ristretto255 discrete logs
   and BLAKE3-256 collision resistance. It is bounded by Ristretto255's group
   order rather than strengthened by the final BLAKE3 digest.

The wire carries only 32-byte ids and range summaries. It never carries an
`iroh-docs` entry, author/timestamp ordering rule, prefix-deletion rule,
`ContentStatus`, or dependency enum discriminant. `iroh-docs` MAY later drive
the semantic interface if it exposes a generic, efficiently indexed, bounded
adapter; replacing or removing that backend MUST NOT change one v2 wire byte.

### Complexity amendment to #134

This decision explicitly reads #134's difference-plus-logarithmic language as a
shape requirement, not a literal `O(delta + log n)` promise. Binary RBSR has the
published worst-case communication bound `O(min(delta * log n, n))`. A literal
additive bound is not supplied by the selected algorithm and is withdrawn. The
replacement still forbids a full-inventory exchange when the symmetric
difference is small, preserves logarithmic honest-peer traversal depth, and is
measurable against #134's catch-up gate. Accepting this ADR accepts that
normative amendment; if the additive bound is required, this algorithm must be
rejected in favor of a different construction.

## Completeness boundary

An RBSR match or an empty traversal work queue means only **candidate inventory
coverage relative to two pinned views**. It is never `Current`, never proof of a
checkpoint, and never sufficient grounds for a replica signature.

A node may claim synchronization through checkpoint X only after it also:

1. verifies X and its required replica quorum certificate;
2. fetches and independently validates every missing body and publication
   certificate;
3. recomputes the exact §13.3 retained-event count and sorted event-set Merkle
   root and matches both to X; and
4. resolves every retained-interval device-chain dependency, either with the
   predecessor body or an authenticated boundary/cross-stream proof.

Merely reporting a missing predecessor is operator-visible `Partial` state, not
completion. Because device sequence is community-wide, an in-scope event may
reference another stream; retention may also prune the predecessor of the first
retained event. The exact authenticated boundary proof belongs to the still-
unfrozen stream-checkpoint/cut work, not this reconciliation envelope, and is a
stable-wire prerequisite.

This explicitly replaces #134 §13.4 client-claim item 4's earlier "resolving or
explicitly reporting" rule. Reporting remains mandatory operator visibility,
but no longer counts as resolution or permits a synchronization claim.

The uncheckpointed tail is outside X's reconciliation scope. A new
`checkpoint_id` or `retention_generation` does not silently retarget a cursor;
it creates a different scope and requires a new pass.

## Envelope ownership and stability

Every reconciliation control frame has a 4-byte big-endian body length followed
by one strict canonical-CBOR map. The body is capped at 64 KiB. The common map
binds protocol version, message kind, fingerprint suite, the complete scope,
session/request identity, and directional view digests. `StreamSummary`,
`RangeQuery`, `RangeDigest`, `RangeIds`, and `SessionControl` have
iroh-room-owned schemas. Session-authenticated branch tickets and page cursors,
plus rank-validated page and deterministic-ancestry branch rebases, make
provenance/resume bounded across session rollover. Mandatory draining leases
and finish/cancel transitions release pinned views only after accepted work is
resolved. `WantEvents` and `EventBatch` remain the separately validated
body-fetch phase.

The algorithm decision is made here. The wire sketch is intentionally marked
**provisional**, because #155 contains no implementation or frozen bytes. It
becomes stable only after all of the following land:

- strict typed encoders/decoders with allocation-before-validation tests;
- canonical bytes for every message and a complete multi-round transcript;
- fingerprint mapping, accumulator, range-digest, empty-set, and boundary
  vectors checked by two independent implementations;
- independent review of the fingerprint construction;
- limit, pagination, replay, resumption, wrong-scope, and changed-view vectors;
- integration tests proving that digest equality cannot bypass checkpoint
  count/root verification or event-body validation;
- resolution of any accepted #134 amendment that adds fields to these
  messages; and
- a frozen stream-checkpoint/device-cut proof that makes retention-boundary and
  cross-stream predecessor validation executable.

Until then, v2 negotiation MUST NOT advertise reconciliation wire version 1 as
stable, and no public v2 interoperability claim may rely on it. After those
criteria are met, any semantic or byte change requires a new reconciliation
wire version or fingerprint-suite id; an `iroh-docs` upgrade alone never does.

## Alternatives rejected

### Expose the current iroh-docs protocol directly

Rejected. It is private at the useful semantic boundary, document-specific,
has different resource limits, scans ranges for summaries, and does not provide
the required resume/budget contract. It remains an implementation reference and
possible future backend.

### XOR BLAKE3 item hashes, as current iroh-docs does

Rejected for the frozen v2 suite. A secure element hash does not make XOR a
collision-resistant set fingerprint; Meyer describes the polynomial-time
linear-algebra attack. Final checkpoint-root verification would contain the
failure to denial of service rather than false completeness, but a
chosen-input collision remains an avoidable, repeatable reconciliation failure.

### Conventional-hash Merkle treap

Rejected for the first suite. It gives conventional collision resistance and a
unique tree representation, but Meyer also gives a chosen-input construction
for a height-n treap. Authorized content authors control signed event inputs, so
the adversarial-input model applies.

### Full inventories or a fixed Merkle-radix walk

Full inventories violate the difference-sensitive requirement. A fixed
Merkle-radix protocol can be made secure and depth-bounded, and remains the
fallback if the selected accumulator fails review, but it adds a second
tree-shaped commitment and up to 256 prefix steps per 32-byte id. The selected
profile preserves rank-balanced RBSR's communication shape and a self-balancing
order-statistic index.

## Consequences

- Phase C needs a pure ordered-inventory interface: fixed-view range summary,
  rank select, and paged ids. It must not let the backend mutate the authoritative
  event store while comparing inventories.
- The maintained index caches a Ristretto point sum and count per subtree. A
  self-balancing tree keeps updates and summaries logarithmic independently of
  attacker-chosen key order.
- The first implementation adds SHA-512/Ristretto operations to the pure
  inventory layer. It does not add iroh, Tokio, storage, or `iroh-docs` to
  `iroh-rooms-v2-core`.
- Dense differences may still enumerate most ids. That is inherent in RBSR and
  is bounded by pagination and cumulative budgets rather than hidden behind a
  misleading constant-round claim.

## Review triggers

Revisit this decision if independent cryptographic review rejects the point-sum
fingerprint, the 1%-of-1M catch-up fixture misses #134's 60-second gate, the
maintained range index cannot meet difference-sensitive CPU bounds, or a stable
generic upstream substrate can satisfy the semantic interface without leaking
its wire.

## References

- Aljoscha Meyer, [*Range-Based Set Reconciliation*](https://arxiv.org/abs/2212.13567), arXiv:2212.13567v2 / SRDS 2023.
- Maitin-Shepard, Tibouchi, and Aranha, [*Elliptic Curve Multiset Hash*](https://arxiv.org/abs/1601.06502), arXiv:1601.06502.
- [RFC 9380, Appendix B: Hashing to ristretto255](https://www.rfc-editor.org/rfc/rfc9380.html#appendix-B).
- [RFC 9496: The ristretto255 and decaf448 Groups](https://www.rfc-editor.org/rfc/rfc9496.html).
- [`iroh-docs` 0.101.0 `ranger.rs`](https://github.com/n0-computer/iroh-docs/blob/v0.101.0/src/ranger.rs).
