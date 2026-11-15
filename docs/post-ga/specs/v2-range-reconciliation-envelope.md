# Spec: v2 Range Reconciliation and Spec-Owned Envelope

| | |
|---|---|
| **Issue** | #155 — `[SPEC] §25 #1: Range-reconciliation algorithm + spec-owned envelope` |
| **Refs** | #134 §§12.2–13.4 / §20.3 / §25 #1; #156; #159; ADR-0004; ADR-0007; ADR-0010–ADR-0011; Meyer 2022/2023 |
| **Status** | Proposed normative algorithm profile; provisional wire sketch. No implementation and no stable-wire claim until §10 is complete. |
| **Scope** | Pure specification. Phase C implementation and stream-checkpoint encoding remain separate work; later §25 decisions are consumed additively where relevant. |

> **Additive #159 lifecycle correction (2026-08-03):** RBSR equality is one
> input to a staged replica's checkpoint-relative catch-up; it grants no
> receipt/checkpoint weight until verified state is committed under the
> governed class and an old-admin-quorum complete-policy transition activates
> the candidate. RBSR `view_equivocation`, invalidity, withholding, and
> transport disagreement remain operator/session evidence, not checkpoint
> equivocation. Permanent quarantine/exclusion requires an objective eligible-
> signer same-slot checkpoint pair, same-sequence receipt pair, or mutually
> exclusive checkpoint-vote/frontier statement. RBSR equality or session
> evidence cannot clear a prepare fence or replace #159's current-`W`, selected-
> admin-approved fork-frontier reconciliation after `fork.resolve`. See
> [`v2-replica-replacement-recovery.md`](v2-replica-replacement-recovery.md).

---

## 1. Decision summary

v2 inventory discovery uses a bounded, directional profile of Meyer's
range-based set reconciliation (RBSR) over lexicographically sorted raw
32-byte `EventId`s. iroh-room owns the versioned network envelope. A dependency
MAY implement the ordered-inventory backend, but no dependency type,
discriminant, serialization, or document conflict rule is part of the wire.

One pass primarily answers this question:

> Which ids are present in responder view B and absent from initiator view A,
> for one exact checkpoint scope?

A client first initiates toward a checkpoint-serving replica. At every terminal
mismatching range it receives the responder's complete range inventory, so it
MUST also compare that listing to its local range when auditing checkpoint
completeness. That local audit identifies `A \\ B` without asking the replica to
initiate a reverse client pass. Two replicas that need the union of their fixed
views still run the pass in both directions, because each endpoint must learn
the remote ids it needs to fetch. The range phase discovers ids only;
`WantEvents` / `EventBatch` fetch bodies afterward.

The profile provisionally specifies:

- the set and stable-view boundary (§2);
- range semantics, rank-balanced splitting, and anchor behavior (§3);
- fingerprint suite 1 (§4), which freezes only when §10.6 is complete;
- logical envelope and pagination semantics (§5);
- hard per-frame/depth limits and negotiated cumulative budgets (§6); and
- the distinction between inventory coverage and checkpoint completeness (§7).

The exact canonical-CBOR bytes remain provisional until the freeze checklist in
§10 is complete. Implementations MUST NOT advertise wire version 1 as stable
before that point.

---

## 2. Set, scope, and stable views

### 2.1 Exact scope

Every reconciliation message is bound to exactly one tuple:

```text
Scope = (
  community_id:         CommunityId,  // raw 32 bytes
  stream_id:            StreamId,     // raw 32 bytes
  retention_generation: u64,
  checkpoint_id:        CheckpointId, // raw 32 bytes
)
```

There is no wildcard, zero/sentinel checkpoint, multi-stream batch, implicit
"latest", or mid-session scope change. In particular:

- `checkpoint_id` names the stream-checkpoint body/cut being reconciled; it is
  not the current governance `SnapshotHash` and this spec does not freeze #161;
- the selected set contains only retained events in that checkpoint cut;
- events after the checkpoint are an explicitly separate tail; and
- a new checkpoint or retention generation creates a new scope. A stored cursor
  for the old tuple MUST NOT be relabelled or continued against the new tuple.

The draft amendments in #134's comments discuss batching several stream scopes
in one deadline exchange. That draft is not ratified by ADR-0004. A transport
MAY co-schedule several independent single-scope frames, but wire version 1 does
not put `scopes[]` inside one reconciliation message.

### 2.2 Inventory value

For a scope, each endpoint exposes a **set**, not a multiset, of validated
`EventId` values it currently holds for the checkpoint cut. Duplicate ids are
removed before indexing. Document timestamps, authors, path prefixes, LWW
ordering, `ContentStatus`, and event bodies are not inventory fields.

The total order is unsigned lexicographic order over the raw 32 bytes. String
forms such as `blake3:<hex>` never appear in the binary envelope.

### 2.3 Fixed directional views

The initiator and responder each pin a read view before exchanging range
summaries. The view MUST remain logically immutable for the pass. Receiving an
id or body does not add it to the view being fingerprinted; newly accepted data
is visible only to a later pass.

The whole-set digest (§4.3) is the view id. Every request after the initial
`StreamSummary` names both view ids; the initial request necessarily omits the
not-yet-known responder id. A responder MAY reconstruct a view after a
disconnect rather than retain an in-memory snapshot, but it may resume only if
the reconstructed whole-set digest is byte-identical and the bounded session
lease/ticket state in §5.7 remains active. Otherwise it returns `view_changed`
or `session_expired`, and the initiator restarts from `All` with new summaries.

---

## 3. RBSR algorithm profile

### 3.1 Ranges

The wire has an explicit range union:

```text
Range = All
      | HalfOpen { start: EventId, end: EventId }
```

`All` denotes the complete set. `HalfOpen` follows clockwise order around the
32-byte key ring:

- `start < end`: `start <= id < end`;
- `start > end`: `id >= start || id < end` (wraparound);
- `start == end`: invalid. Whole-set meaning is represented only by `All`.

The canonical start point for ordering or paginating `All` is 32 zero bytes.
Range boundaries need not be members of either set. A child split boundary is a
member of the responder's view.

### 3.2 Range summary

```text
RangeSummary = {
  range:  Range,
  depth:  u16,
  count:  u64,
  digest: [u8; 32],
}
```

`count` and `digest` describe ids from one pinned view that fall in `range`.
`depth` is traversal metadata: `All` starts at zero and each split increments it
by exactly one. Counts are checked before any allocation or rank conversion.

### 3.3 Directional refinement

The initiator starts with its `All` summary. For each query, the responder
computes its summary for the exact same range and applies these cases in order:

1. **Equal.** Counts and digests match: the responder has no id to contribute
   in this range; mark the branch done.
2. **Responder empty.** Responder count is zero: mark the transfer branch done.
   If checkpoint auditing is enabled, every initiator id in the range is also
   recorded as local-only.
3. **Enumeration anchor.** Return `ids_available` when any is true:
   - responder count is at most `ANCHOR_MAX_IDS` (256);
   - initiator count is zero and its digest is the valid empty-range digest; or
   - `depth == MAX_RANGE_DEPTH` (64).
4. **Split.** Rank-split the responder range exactly as §3.4 and return the two
   child summaries.

The initiator compares each returned child to its own summary for that exact
range. Equal children finish; mismatching children enter the work queue.
`ids_available` is consumed through pull pagination (§5.6). The responder's
listing for a terminal range MUST be complete through the advertised remote
count. The initiator records responder-only ids and, when auditing checkpoint
completeness, MUST merge that listing with its own ordered range to record
local-only ids too. Once all branches and pages are consumed, the initiator has
the symmetric difference subject to the fingerprint assumption. That is an
inventory result, not completeness (§7).

For replica union, a second pass with the roles and session id reversed lets the
other replica learn the remote ids it must fetch. A checkpoint client does not
need a reverse pass merely to identify its own extras. A replica MUST NOT ingest
or publish a client-only id merely because an audit named it. Checkpoint
selection and normal body validation decide whether the client excludes,
quarantines, or separately treats that id as post-checkpoint tail.

### 3.4 Mandatory rank balance and progress

For a responder range containing `n >= 2` ids in clockwise order, choose the id
at zero-based rank `floor(n / 2)` as pivot `m`.

- `All` splits into `[ZERO, m)` and `[m, ZERO)`.
- `[start, end)` splits into `[start, m)` and `[m, end)`.

Both children MUST be non-empty in the responder view. Their counts MUST sum to
the responder's parent count and differ by at most one. The child ranges MUST
cover the parent exactly, without overlap, and neither child may encode equal
endpoints.

A receiver validates range coverage, depth, count sum, and balance before
queueing either child. A claimed `1/(n-1)` split for `n > 3` is invalid even
though it would satisfy the looser recurse rule in Meyer's paper. Counts remain
untrusted statements about remote data, so these checks do not prove honesty;
the hard depth/work limits contain a lying peer.

Numeric midpoint splitting is forbidden. `EventId`s are sparse cryptographic
values; rank, not numeric distance, supplies the progress invariant.

### 3.5 Termination

For finite fixed views and an honest responder, every split reduces responder
cardinality by approximately half. A branch therefore reaches equality or an
enumeration anchor. At depth 64, enumeration is mandatory even for a larger
range, so the protocol does not depend on an unbounded recursion limit.

A responder that exhausts a cumulative budget returns `budget_exhausted`; it
does not return an empty response or success. The initiator persists its local
work queue and deterministic page cursors and may resume against the same two
view ids. A changed view restarts rather than pretending the old traversal still
applies.

### 3.6 Complexity contract

Let `nA`, `nB` be the view sizes and `delta = |A symmetric_difference B|`.
With binary balanced splits and the explicit-id anchor, honest-peer wire work
has Meyer's difference-sensitive shape:

```text
refinement-tree depth: O(log(max(2, nB))) before the depth cap
wire data:             O(min(delta * log(nA + nB), nA + nB))
largest frame:         O(1), concretely bounded by §6
request/response count: tree_batches + ids_pages, bounded by §6
```

`tree_batches` counts `Compare` requests after packing at most 64 ready probes
per request, including dependencies between tree levels. `ids_pages` is the sum
of `ceil(remote_anchor_count / requested_page_limit)` over enumerated terminal
ranges. Section 5.7 permits a byte-capped window of eight independent requests;
requests within one dependency wave or for different anchors may be pipelined,
while cursor-dependent pages for one anchor remain sequential. In particular,
local-empty versus a large responder anchors at depth zero but still needs
`ceil(nB / 1,024)` dependent page exchanges. Refinement-tree depth is therefore
not an RTT bound; total RTT waves depend on the frontier shape and window fill.

The wire-data expression is not the literal additive `delta + log(n)` byte
bound. #134's
"difference plus logarithmic range traversal" is interpreted as forbidding a
full-set scan or inventory when the difference is small, while permitting the
published RBSR `delta * log(n)` worst-case communication.

Local implementations MUST maintain a self-balancing, range-summarizable
order-statistic index with cached item count and §4 accumulator. Required
operation bounds are:

```text
index storage:                 O(n)
insert/delete one EventId:     O(log n)
range count/digest/rank pivot: O(log n)
enumerate a page:              O(log n + page_len)
```

Computing the initial `All` digest or a range digest by iterating every id is
non-conforming. The spec does not claim a tighter total CPU bound across every
round; the Phase C implementation must measure it under §10.5.

---

## 4. Fingerprint suite 1

### 4.1 Suite identifier and domains

```text
fingerprint_suite = 1
name = RBSR_RISTRETTO255_SHA512_BLAKE3_V1

RBSR_ELEMENT_DST =
  "iroh-room-v2-rbsr-eventid-v1_XMD:SHA-512_R255MAP_RO_"

RBSR_RANGE_DIGEST =
  "iroh-room-v2/rbsr-range-digest/v1"
```

Both domains are the exact displayed ASCII byte strings, with no trailing NUL.
`RBSR_ELEMENT_DST` is the application DST for this protocol, not an RFC suite
identifier.

The element mapping follows RFC 9380 Appendix B's `hash_to_ristretto255`,
instantiated with `expand_message_xmd(SHA-512)` and the exact ASCII
`RBSR_ELEMENT_DST`. In exact steps:

```text
uniform_bytes = expand_message_xmd(
    SHA-512,
    msg = EventId,
    DST = RBSR_ELEMENT_DST,
    len_in_bytes = 64,
)
P(EventId) = ristretto255_map(uniform_bytes)
```

`ristretto255_map` is exactly RFC 9496 §4.3.4 element derivation, and point
encoding is exactly RFC 9496 §4.3.2. Direct `SHA-512(EventId)`,
`SHA-512(DST || EventId)`, and Dalek's `RistrettoPoint::hash_from_bytes` are not
`expand_message_xmd` and are non-conforming, even though the last API is also
described as hashing to Ristretto.

These are reconciliation domains, not additions to #134's frozen identifier
derivation list. The implementation must nevertheless register and vector them
explicitly; silently reusing a signed-record or checkpoint domain is forbidden.

### 4.2 Cached accumulator

For raw event-id bytes `id`:

```text
P(id) = hash_to_ristretto255(msg = id, DST = RBSR_ELEMENT_DST)

accumulator(R) = sum(P(id) for id in the local set intersect R)
```

The sum is in the prime-order Ristretto255 group. The empty range uses the group
identity. The ordered index caches subtree count and point sum; point addition
is associative and commutative, so the self-balancing tree's shape is not part
of the fingerprint and attacker-chosen key order cannot unbalance the aggregate
definition.

Incoming wire data never supplies a point to add. An endpoint computes its own
accumulator and sends only the final 32-byte BLAKE3 digest below.

### 4.3 Range digest

Define fixed-width encodings:

```text
scope_bytes =
    community_id[32]
 || stream_id[32]
 || retention_generation.to_be_bytes()[8]
 || checkpoint_id[32]

range_bytes(All) = 0x00
range_bytes(HalfOpen { start, end }) = 0x01 || start[32] || end[32]

digest = BLAKE3_UNKEYED_256(
    RBSR_RANGE_DIGEST
 || wire_version.to_be_bytes()[2]       // 1
 || fingerprint_suite.to_be_bytes()[2]  // 1
 || scope_bytes
 || range_bytes(range)
 || count.to_be_bytes()[8]
 || ristretto255_encode(accumulator(range))[32]
)
```

`BLAKE3_UNKEYED_256` means BLAKE3's unkeyed hash mode with exactly 32 output
bytes. All concatenated components after the fixed domain have fixed length or
an explicit one-byte range tag, so the preimage is unambiguous. The whole-set
`digest(All)` is the pinned view id.

The range `count` is inside the digest and on the wire. This does not turn the
fingerprint into a proof: a peer can lie or withhold without solving a
collision. Under BLAKE3-256 collision resistance, it computationally binds the
count to an honestly computed digest rather than preventing substitution
unconditionally.

### 4.4 Security boundary

XOR of item hashes is forbidden for suite 1. Meyer's analysis shows that XOR
set collisions reduce to linear algebra even when the element hash itself is
secure. A pseudorandom Merkle treap is also not suite 1: an authorized author
can grind valid event bodies, and Meyer gives a chosen-input degenerate-tree
attack.

The selected point-sum construction is ECMH-style: it adds independently
hash-to-group-mapped set elements in a standardized prime-order group. It is not
an assertion that this profile instantiates every detail of the published ECMH
construction. Model `hash_to_ristretto255` as a random oracle into the
Ristretto255 group and assume discrete logarithms in that group are hard. Under
the standard elliptic-curve multiset-hash reduction, finding two distinct sets
with the same point sum is as hard as solving that discrete-log problem. The
outer digest separately assumes unkeyed BLAKE3-256 collision resistance. Suite
1 therefore targets `min(sqrt(l), 2^128)`, approximately 2^126 classical work,
where `l` is the Ristretto255 group order. These are computational assumptions,
not proof that digest equality implies set equality. Independent review and
cross-implementation vectors remain wire-freeze blockers (§10); this document
does not treat novelty in a spec as a substitute for cryptographic review.

A collision can make an unequal branch compare equal, omit ids, and cause an
availability or denial-of-service failure. By itself it cannot satisfy §7's
independently authenticated checkpoint count/root gate.

---

## 5. Spec-owned envelope

### 5.1 Framing and canonical profile

Each control message is framed as:

```text
u32 big-endian body length || canonical-CBOR body
```

The prefix is not part of the body limit. A declared control body over 65,536
bytes is rejected before allocation. The body is one definite-length,
text-keyed map in the repository's strict deterministic-CBOR profile:

- shortest unsigned integers; definite byte strings/arrays/maps;
- canonical unique text-map-key order;
- no negative integers, tags, floats, booleans, or indefinite lengths;
- optional fields are omitted, never encoded as `null`; and
- unknown, duplicate, or missing required fields are rejected.

Boolean semantics use unsigned enums (`0`/`1`) in this sketch. This avoids
silently widening the existing v2 closed CBOR value space. The precise map keys
and golden bytes are provisional until §10; their logical types and validation
rules below are the #155 contract.

### 5.2 Common fields

Every `StreamSummary`, `RangeQuery`, `RangeDigest`, `RangeIds`, and
`SessionControl` map contains the fields below, except for the one explicitly
omitted initial-summary field:

| Field | Type | Rule |
|---|---|---|
| `v` | u16 | Reconciliation wire version; exactly 1. |
| `kind` | u16 | `0=StreamSummary`, `1=RangeQuery`, `2=RangeDigest`, `3=RangeIds`, `4=SessionControl`. |
| `suite` | u16 | Exactly 1 for this profile. |
| `community_id` | bytes(32) | Raw `CommunityId`. |
| `stream_id` | bytes(32) | Raw `StreamId`. |
| `retention_generation` | u64 | Exact scope value. |
| `checkpoint_id` | bytes(32) | Raw stream `CheckpointId`. |
| `session_id` | bytes(16) | Initiator-generated uniformly with a CSPRNG; direction-specific. |
| `attempt` | u32 | Starts at zero; increments by exactly one through `ResumeAttempt` after `budget_exhausted`/`request_timeout`, or proactively with an empty window when the known remainder cannot fund the next atomic request. |
| `request_id` | u64 | Monotonic within the session; zero is the initial summary. |
| `initiator_view` | bytes(32) | Initiator `digest(All)`. |
| `responder_view` | bytes(32), optional | Responder `digest(All)`; omitted only from the first initiator summary, before it is known. |

After the responder summary, `responder_view` is required. A response echoes
the complete common scope, session, attempt, request, and view pair. Any
mismatch fails the request; fields are never inherited implicitly from the
connection.

For first-seen requests, `request_id` MUST increase strictly across attempts;
gaps are permitted and wraparound is not. A previously seen id is valid only as
the byte-identical retry described in §5.7.

The `session_id` correlates work and detects request-id reuse. It is not an
authority token. Authorization is established by the v2 session before any
private stream summary is served.

### 5.3 `StreamSummary`

The initiator sends request zero with its `All` summary and cumulative budget.
The responder pins its view and replies with both view ids, its `All` summary,
the budget it will enforce, and a root branch ticket.

```text
StreamSummaryRequest = {
  all:    RangeSummary, // range=All, depth=0; digest equals sender view id
  budget: Budget,
}

StreamSummaryResponse = {
  all:         RangeSummary, // responder All summary
  budget:      Budget,
  root_ticket: RangeTicket,  // purpose=compare; remote=all
}
```

An `All` summary with a nonzero depth, wrong view digest, or invalid empty digest
is rejected. Equal summaries permit the initiator to skip range descent, but
still yield only the candidate state in §7.

A branch ticket is:

```text
RangeTicket = {
  remote:  RangeSummary,
  purpose: u16,       // 0=compare, 1=ids
  auth:    bytes(32),
}
```

The responder generates `session_ticket_key` uniformly with a CSPRNG as a secret
32-byte key and retains it only for the bounded session lifetime in §5.7. Define
`BLAKE3_KEYED_256(key, input)` as BLAKE3 keyed-hash mode with exactly 32 output
bytes. The authenticator is:

```text
auth = BLAKE3_KEYED_256(
  key = session_ticket_key,
  input = "iroh-room-v2/rbsr-ticket/v1"
       || wire_version_be2 || fingerprint_suite_be2
       || scope_bytes || session_id
       || initiator_view || responder_view
       || purpose_be2 || range_bytes(remote.range) || remote.depth_be2
       || remote.count_be8 || remote.digest,
)
```

The domain is the exact ASCII bytes with no trailing NUL. `attempt` is
deliberately absent so a ticket remains valid across a budget resume in the same
session. The ticket key is never transmitted or logged and is destroyed on
session release; authenticators are compared in constant time. A ticket is
branch provenance and resource admission, not peer authority or evidence that
its summary is honest.

### 5.4 `RangeQuery`

`RangeQuery` has three operations:

```text
Compare = {
  op:     0,
  probes: [ { probe_id: u64, branch: RangeTicket,
              local: RangeSummary } ], // 1..=64
}

IdsPage = {
  op:       1,
  probe_id: u64,
  anchor:   RangeTicket, // purpose=ids
  cursor:   PageCursor?, // omitted for offset zero
  rebase:   ResumePoint?, // cross-session; mutually exclusive with cursor
  limit:    u16,      // 1..=1024
}

BranchRebase = {
  op:       2,
  probe_id: u64,
  resume:   BranchResumePoint,
}

PageCursor = {
  offset:  u64,      // number of remote range ids already returned
  last_id: EventId,  // id at rank offset-1 in clockwise range order
  auth:    bytes(32),
}

ResumePoint = {
  offset:  u64,
  last_id: EventId,
}

BranchResumePoint = {
  target: RangeSummary,
  path:   bytes(1..8), // one root-to-target bit per target depth
}
```

`Compare.local` belongs to the initiator view and MUST have the exact range and
depth in `branch.remote`. The ticket MUST authenticate with `purpose=compare`.
The responder recomputes that remote summary from its pinned view and requires
byte equality with the ticket before processing the probe. Root and child
tickets are the only valid branch provenance, so an initiator cannot introduce
an arbitrary overlapping range or forged depth. Probe ids are unique within one
`Compare` request and remain stable when those exact request bytes are retried.

`BranchRebase` reacquires one `purpose=compare` ticket after session rollover.
`target.depth` MUST be in `1..=64`; `path` has exactly
`ceil(target.depth / 8)` bytes, reads most-significant bit first, and has zero
unused low bits. Starting from the fresh session's `All` summary, the responder
recomputes the deterministic §3.4 split at every path bit. Every predecessor
MUST have more than 256 responder ids and depth below 64, so it was eligible to
issue children. The selected child at the final bit MUST be byte-identical to
`target`; otherwise the rebase is `invalid_range`. The responder then issues a
fresh-session branch ticket for that target. This walk is charged exactly as
§6.2 specifies and is bounded by the depth and request deadline.

The initiator persists each outstanding target's path with the traversal map;
left and right child tickets append zero and one respectively. A rebase proves
only that the target is a deterministic node of the byte-identical responder
view. It neither authenticates history from the old session nor establishes
set completeness, but it prevents the 16-attempt cap from forcing an unfinished
deep branch back through the same prefix forever. A root branch never needs a
rebase because every new `StreamSummary` supplies its ticket.

An `IdsPage` anchor MUST authenticate with `purpose=ids`. A first-page request
omits `cursor` and starts at offset zero. A later cursor authenticates, under the
same session key and exact anchor, the fixed domain
`"iroh-room-v2/rbsr-page/v1" || anchor.auth || offset_be8 || last_id` using
`BLAKE3_KEYED_256(session_ticket_key, ...)`. This cursor domain is also exact
ASCII with no trailing NUL. The
responder also verifies `1 <= offset < anchor.remote.count` and
that `last_id` is exactly the remote range id at rank `offset - 1`; this makes a
cursor independently checkable after attempt-level accounting is reset.
Pagination follows the clockwise ordering in §3.1 and does not depend on a
retained probe registry.

After a session expires or reaches its attempt cap, the initiator starts from
`All`, pins the byte-identical view pair, and reacquires an ids anchor whose
`remote` exactly equals its persisted anchor summary. For a non-root anchor it
uses `BranchRebase`, then one `Compare`, to obtain the fresh `purpose=ids`
ticket. It may then send a `ResumePoint` instead of the old
session-authenticated cursor. The responder
validates `1 <= offset < anchor.remote.count` and exact rank-`offset - 1`
`last_id` equality before returning the page and a cursor authenticated under
the new session. A conforming initiator uses `rebase` only when its persisted
cumulative count equals `offset`; a forged/skipping point can only deny that
initiator completeness because §7 still checks the checkpoint count/root. A
request with both `cursor` and `rebase` is invalid.

### 5.5 `RangeDigest`

One `RangeDigest` responds to one `Compare` or `BranchRebase` request. A
`Compare` response has one result per probe in ascending `probe_id` order; a
`BranchRebase` response has exactly one result:

```text
DigestResult = {
  probe_id: u64,
  remote:   RangeSummary, // responder summary for the queried/rebased range
  outcome:  u16,          // 0=done, 1=split, 2=ids_available, 3=branch_rebased
  children: [RangeTicket; 2]?, // purpose=compare; split only
  anchor:   RangeTicket?,      // purpose=ids; ids_available only
  rebased:  RangeTicket?,      // purpose=compare; branch_rebased only
}
```

`branch_rebased` is valid only for `BranchRebase`; its `probe_id` equals the
request, and `remote`, `rebased.remote`, and `resume.target` are byte-identical.
`rebased` is a fresh `purpose=compare` ticket and all other optional result
fields are absent. Outcomes 0 through 2 are valid only for `Compare`; `rebased`
is then absent and every result's `probe_id` names exactly one request probe.

`done` is valid only when summaries match or the responder count is zero.
`split` is valid only when §3.4 and the anchor ordering in §3.3 permit it.
`ids_available` is valid only under an anchor condition. The initiator verifies
the outcome from counts/digests/depth; it never trusts the enum alone.

For every `Compare` probe, the result's `remote` MUST be byte-identical to that
request probe's `branch.remote`. Each child ticket's `remote` is the exact
child summary. The
initiator retains a bounded/persistent
`root-to-range path -> range + remote summary` traversal map; any later
different summary for the same path/range/view tuple is
`view_equivocation`, terminates the session, and grants no progress. An anchor
ticket repeats the exact parent summary with `purpose=ids`.

Results may not be silently omitted. If the responder cannot process every
probe within its declared budget, it returns the common typed
`budget_exhausted` error for the request and no partial `RangeDigest`.

### 5.6 `RangeIds`

```text
RangeIdsBody = {
  probe_id: u64,
  anchor:   RangeTicket, // exact query anchor
  offset:   u64,        // zero or exact query cursor offset
  limit:    u16,        // exact query limit
  ids:      [EventId],  // exactly min(limit, remote.count-offset)
  next:     PageCursor?,
}
```

The anchor and `limit` MUST equal the request. `offset` is zero when the request
omitted both cursor forms and otherwise equals the authenticated cursor or
rank-validated rebase offset. Ids are the
exact `min(limit, anchor.remote.count - offset)` remote range ids starting at
rank `offset`; they MUST be unique, in range, and strictly ordered. A non-final
page therefore contains exactly `limit` ids. `next` is present if and only if
`offset + ids.len < anchor.remote.count`; it carries that sum as its offset and
the page's last id. It is omitted if and only if the sum equals the anchored
remote count. Overflow, an early final page, and a page beyond the count are
invalid.

The initiator persists the anchor, next cursor, cumulative received count, and
last id. It requires the cursor offset to equal its cumulative count and enforces
cross-page order and uniqueness. A terminal branch receives completion credit
only when that cumulative count equals `anchor.remote.count`. The initiator
merges the complete remote listing with its local ordered range to record both
`B \\ A` and `A \\ B`; no id is accepted as an event body at this stage.

### 5.7 Idempotence, lifecycle, and typed stops

A duplicate `(session_id, attempt, request_id)` with byte-identical canonical
request bytes is an idempotent retry. Reusing that triple with different bytes
is `request_reuse`. A request from another scope, view pair, attempt, or
direction is not a retry.

Within the active attempt, the responder retains
`request_id -> canonical request hash + canonical response bytes` in a bounded
persistent spool, not an unbounded RAM queue. Unique response bytes are bounded
by that attempt's `Budget.control_bytes`. An admitted duplicate returns the
cached bytes and does not reapply semantic work, but retries are never free: each
duplicate consumes one of at most 8 retries for that request and also consumes
the session-wide replay limits of 128 duplicate requests and 8 MiB of duplicate
request-plus-response bodies. Exceeding any replay limit returns
`retry_limit_exceeded` if a correlated error fits, then terminates the session.
The opening response, most recent `SessionControl` response, and most recent
range-admission error use three fixed cache slots and consume the same replay
limits. The admission-error slot retains the rejected request hash and exact
error bytes; its byte-identical duplicate gets the cached error, while changed
bytes at that request id are `request_reuse`.

When `ResumeAttempt` succeeds, the previous attempt's spool is released and any
request carrying an older attempt receives `attempt_closed`; it is never
recomputed. If the active cache or ticket key is gone, the session is expired
and the initiator restarts from `All`; it never guesses whether work was charged.

The directional session state machine is:

```text
Unopened
  -- valid StreamSummary --> Active(attempt=0)
Active
  -- budget_exhausted/request_timeout --> DrainingAttempt
DrainingAttempt
  -- accepted window resolved --> AttemptStopped
Active
  -- session_rollover_required --> DrainingRollover
DrainingRollover
  -- accepted window resolved --> Expired
Active [window empty]/AttemptStopped
  -- ResumeAttempt(attempt+1, Budget) --> Active(attempt+1)
Active/AttemptStopped
  -- Finish [window empty] --> Finished
any nonterminal state
  -- Cancel --> DrainingCancel
DrainingCancel
  -- accepted window resolved; ack cached --> Cancelled
any nonterminal state
  -- idle/absolute lease or view loss --> DrainingExpire
DrainingExpire
  -- accepted window resolved --> Expired
```

If one request stops an attempt while other requests are outstanding, no new
request is admitted; every already-reserved request is completed or receives
its own timeout, and the state becomes `AttemptStopped` only after the window is
drained. Those completed responses retain normal progress credit.

Every draining state closes request admission immediately. `DrainingCancel`
lets already-reserved requests complete or reach their original 30-second
deadline, emits the `Cancel` acknowledgment only after the window drains, and
then releases state. On lease expiry, unfinished accepted work receives a
correlated `session_expired`; on view loss it receives `view_changed`. Already
completed response bytes remain replayable while the drain finishes, but no
new progress is computed. `DrainingExpire` releases the pinned view, ticket key,
replay spool, and reservations only after those bounded outcomes are serialized
or their connection is gone. Thus a terminal transition never frees state
beneath accepted work.

`DrainingRollover` uses the same bounded release order when a
session-lifetime resource cannot admit a request. Previously reserved range
requests finish; the first unreservable request and every later request already
present in the bounded receive window receive `session_rollover_required` and
no progress credit. After the window drains the session expires and the
initiator rebases persisted work under a new session. Attempt rollover cannot
reset a session-lifetime counter.

`SessionControl` has `action: u16` (`0=Finish`, `1=Cancel`,
`2=ResumeAttempt`), `ack: u16` (`0=request`, `1=response`), and a `budget` field
required only for a resume request/response. `Finish` is valid only with no
outstanding work and means the initiator has consumed its traversal; it is not a
completeness assertion. `Cancel` abandons it through the drain above.
`ResumeAttempt` is also valid proactively from `Active` when its request window
is empty; this lets an initiator whose known remainder cannot fund its next
atomic request roll over without deliberately sending an over-budget request.
It increments attempt by exactly one, negotiates a fresh attempt budget, and
reuses still-valid branch tickets/page cursors. Attempt 15 is the last allowed
attempt; further work starts a new session from `All` and uses
`BranchRebase`/`ResumePoint` for persisted work. A terminal acknowledgment is
retained in its fixed cache slot after the pinned view, ticket key, and bulk
replay spool are safely released.

The profile admits at most 16 attempts per session, at most eight outstanding
`RangeQuery` requests plus one outstanding `SessionControl` request per
directional session, at most one active direction per `(peer, scope)` pair, and
at most eight active directional sessions/pinned views per authenticated peer.
A reciprocal replica pass begins only after the first direction reaches
`Finished`, `Cancelled`, or `Expired`. The pair slot is
reserved before sending an opening request. If two opens race, the
lexicographically smaller raw `session_id` wins; both endpoints cancel/reject
the losing open as `direction_busy`. An exact id collision rejects both and each
side retries with a fresh random id. A deployment MUST set
a finite aggregate pinned-view cap and a finite aggregate replay-spool byte cap,
and MUST return `server_busy` before pinning/reserving when either is reached. An
admitted request MUST receive a response or
`request_timeout` within 30 seconds. A session expires after 120 seconds with no
fully received request and after 900 seconds absolutely. Disconnect does not
extend either lease. Expiry follows the bounded draining order above.

The initiator MUST continuously drain responses whenever the range window is
nonempty and MUST NOT wait to fill all eight slots before reading. The responder MUST
begin draining accepted requests without waiting for a full window and MUST
release each request's reservation when its response is sent. At most 1 MiB of
`RangeQuery` request-plus-worst-case-response bodies may be reserved in flight
for a session (`8 * (65,536 + 65,536)`). A separate single-slot lifecycle lane
reserves at most 8,192 bytes for one `SessionControl` request plus response;
each of those canonical bodies is capped at 4,096 bytes. That lane lets
`Cancel` close admission even when all eight range slots are occupied, but it
cannot carry range work. Responses may arrive out of request-id order and are
correlated by that id. Direction serialization, mandatory draining, and these
byte caps remove the two-small-buffer deadlock while permitting
benchmark-critical pipelining.

The existing session `Error` family carries these reconciliation codes:

| Code | Meaning / recovery |
|---|---|
| `unsupported_reconciliation` | Unknown `v` or `suite`; negotiate another version or stop. |
| `scope_mismatch` | Any scope field differs; fatal for the request. |
| `view_changed` | A pinned view cannot be reproduced; restart from summaries. |
| `session_expired` | Lease, replay state, or ticket key is gone; restart from summaries. |
| `session_rollover_required` | A session-lifetime resource cannot admit the request. Persist and drain; after release, start from summaries and rebase. |
| `attempt_closed` | The named current attempt is stopping/no longer admits range work, or the request names an older attempt whose spool was released. It grants no progress: for the current attempt, drain then `ResumeAttempt`; for an older attempt, use the active attempt or restart. |
| `request_timeout` | Atomic request deadline elapsed; stop the attempt and resume with `attempt+1`. |
| `request_reuse` | Same id, different request bytes; fatal for the session. |
| `retry_limit_exceeded` | Per-request or session replay allowance ended; terminate without completion credit. |
| `invalid_ticket` | Branch or cursor authenticator/provenance failed; fatal for the session. |
| `view_equivocation` | Same pinned range was described differently; retain evidence and restart elsewhere. |
| `server_busy` | Finite pinned-view/session admission cap reached; retry another session/peer. |
| `direction_busy` | The peer/scope direction slot is occupied or lost the simultaneous-open tie-break; retry after it closes. |
| `invalid_range` | Bad range, depth, cursor, id order, or membership. |
| `invalid_split` | Children do not cover/balance/progress exactly. |
| `limit_exceeded` | A per-frame/collection ceiling or the eight-`RangeQuery` outstanding ceiling was exceeded. Reject oversized bodies before proportional allocation. A ninth range request gets the one-shot admission error and no progress; drain the existing window, then retry later with a new `request_id`. |
| `budget_exhausted` | Cumulative attempt budget ended; persist local work, then resume. Never success. |
| `invalid_response` | Any other response invariant failed; no completeness credit. |

After the common fields have been strictly decoded and trusted, an error response
echoes them. An oversized length prefix, malformed CBOR before those fields, or
another pre-correlation failure closes/rejects the reconciliation stream without
a correlated protocol `Error`; fields that were never parsed cannot be echoed.
An opening `server_busy`, `direction_busy`, or `unsupported_reconciliation`
response may omit `responder_view` under the same initial-summary exception as
§5.2. Error text is diagnostic and is not part of machine behavior.

### 5.8 Draft amendment fields

#134's review comments label their timing, sync-state precedence, and partition
choices D1/D2/D3 as settled by the maintainer. The same comments carry a broader
reviewed amendment containing `degraded` on `RangeDigest`, `RangeIds`, and
`EventBatch`, horizon fields on `StreamSummary`, and a device-cut manifest.
ADR-0004 accepted the architecture and phasing, not those proposed wire fields
or proof bytes. No merged release-owner record currently ratifies them. #155
therefore neither silently freezes nor rejects them. Before wire version 1
becomes stable, the maintainer must either:

1. accept them and add exact enum-encoded fields plus golden bytes; or
2. reject/defer them explicitly.

Adding an accepted field after stability requires a new wire version unless
version 1's final schema already includes it.

The device-cut boundary proof is additionally a functional prerequisite for
§7.2, not merely optional metadata: without an accepted encoding, a client
cannot complete a stream whose first retained event has a pruned or cross-stream
predecessor.

---

## 6. Resource bounds and backpressure

### 6.1 Hard per-message/profile ceilings

| Limit | Value | Required behavior |
|---|---:|---|
| Control-frame CBOR body | 65,536 bytes | Reject length prefix before body allocation. |
| `Compare.probes` | 64 | Reject the frame; never truncate. |
| `RangeIds.ids` / requested page | 1,024 | Reject/limit before allocating ids. |
| Enumeration anchor | 256 responder ids | Split above it unless another anchor rule applies. |
| Range depth | 64 | Enumerate at the cap; never recurse to 65. |
| Branch-rebase path | 64 bits / 8 bytes | Reject non-minimal length or nonzero unused bits. |
| CBOR nesting depth | 16 | Same closed-profile guard as v2 core. |
| Outstanding `RangeQuery` requests/session | 8 | A ninth triggers the one-shot overflow rule; initiator drains while the window is nonempty. |
| In-flight range reservation | 1 MiB/session | Count range-request bodies plus 65,536 bytes per pending response. |
| Lifecycle control lane | 1 request / 8,192 bytes | One 4,096-byte `SessionControl` request plus one 4,096-byte response; never range work. |
| Range admission-error lane | 1 response / 4,096 bytes | Correlate one rejected ninth `RangeQuery`; stop reading more range frames until it is sent. |
| Active directional sessions/peer | 8 | Reject before pinning a ninth view; only one direction per peer/scope. |
| Issued + reserved branch/anchor tickets/session | 524,288 | Count the root; reserve before work and roll the session before exceeding the cap. Bound the initiator traversal map equally. |

Every generated frame MUST fit the 64 KiB body cap even when a count ceiling
would otherwise permit it. An initiator may choose fewer probes or a lower page
limit. Once a request is accepted, the responder MUST return every probe result
or the single atomic error; it cannot choose fewer. Before stable wire, §10.3
must construct the maximum 64-probe all-split `RangeDigest`, with every accepted
amendment field, and prove it fits. If it does not, the probe ceiling MUST be
lowered before version 1 is advertised.

Ticket slots are a serialized session-lifetime counter, separate from attempt
budgets. The opening root consumes one. Admission reserves two slots per
`Compare` probe (the worst-case pair of child tickets), one for
`BranchRebase`, and zero for `IdsPage`; it refunds unused slots after the atomic
response is encoded. If the complete reservation would cross 524,288, the
responder performs no summary/rank work, returns
`session_rollover_required` for that and every later request already in the
bounded window, enters `DrainingRollover`, and lets previously reserved
requests finish against their slots. `ResumeAttempt` never resets this counter.
Thus concurrent all-split responses cannot overshoot the cap or be truncated
after work.

### 6.2 Cumulative `Budget`

The initiator offers a maximum attempt budget in `StreamSummary` or
`ResumeAttempt`; the responder returns the component-wise lower of that offer
and its local policy and enforces the returned budget for its work/responses:

```text
Budget = {
  requests:        u32, // 1..=4,096
  range_summaries: u32, // 193..=524,288
  returned_ids:    u32, // 1..=1,048,576
  control_bytes:   u64, // 131,072..=67,108,864 (64 MiB)
}
```

The counters are exact:

| Counter | First-seen semantic request cost |
|---|---|
| `requests` | One per `Compare`, `IdsPage`, or `BranchRebase`. Opening and `SessionControl` are lifecycle-limited instead. |
| `range_summaries` | Per `Compare` probe: one for the remote parent; a split adds one rank-pivot selection and two child summaries, for four total. `done`/`ids_available` cost one. A `BranchRebase` costs `1 + 3 * target.depth`: one fresh-root check plus one pivot selection and two child summaries at every path bit. Only responder operations count. |
| `returned_ids` | Number of ids encoded in `RangeIds`. Cursor rank verification and one-id lookahead do not count here but remain subject to the request deadline and abuse limits. |
| `control_bytes` | Canonical request-body plus response-body lengths for each first-seen semantic request; 4-byte prefixes are excluded. |

Admission serializes counter and ticket-slot updates across the request window.
After structural/ticket validation, one transaction evaluates session-lifetime
ticket capacity first and attempt budget second. If ticket capacity cannot fit,
`session_rollover_required` takes precedence even when the attempt budget is
also short, and no attempt counter is charged. Otherwise, before doing work,
the responder atomically reserves §6.1's worst-case ticket slots plus one
request, four summary
units per `Compare` probe, `1 + 3 * target.depth` summary units for
`BranchRebase`, or `limit` returned-id units for `IdsPage`, plus the exact
request body and a full 65,536-byte response body. It then executes and encodes
the complete response, commits the exact costs above, and refunds the unused
reservation. If the worst-case reservation does not fit, it returns
`budget_exhausted` before summary lookup, rank selection, page read, or response
allocation and releases the provisional ticket-slot reservation. Thus a
multi-probe request cannot burn earlier probes and then omit
the last result. The minimum 193 summary units fund the deepest legal branch
rebase; every allowed minimum budget also funds one atomic split or one one-id
page.

The initiator tracks the returned counters and MUST NOT send a window whose
combined worst-case reservations exceed the known remainder. The responder
reserves in receive order. If one request cannot reserve, it stops admitting new
requests for that attempt, returns `budget_exhausted` for that request, and
finishes requests whose reservations were already accepted. Every later request
already present in the bounded eight-frame receive window receives
`attempt_closed`; it has no progress credit. The responder admits no further
`RangeQuery` in that attempt; only `SessionControl` remains admissible. A
conforming initiator never creates this overcommitted window.

When the next desired atomic request does not fit the known remainder, a
conforming initiator first drains its window and sends proactive
`ResumeAttempt`; it never sends a sacrificial request merely to elicit
`budget_exhausted`. At attempt 15 it instead drains, persists, and ends the
session before rebasing the work under a fresh one.

Each attempt also has a separate 32,768-byte terminal-error reserve unavailable
to normal frames: one canonical error body of at most 4,096 bytes for every slot
in the eight-request window. Correlated `budget_exhausted`, `attempt_closed`,
`request_timeout`, `session_expired`, `session_rollover_required`, and
`view_changed` drain responses use that reserve. A separate single 4,096-byte
admission-error reserve correlates a rejected ninth `RangeQuery` with
`limit_exceeded`; the responder stops reading further range frames until that
error is sent, so repeated overflow cannot accumulate memory. Diagnostic text
is omitted or shortened to fit. Duplicate traffic
does not consume these exactly-once semantic counters, but it consumes every
arrival and retransmitted byte under §5.7's mandatory replay limits. All
arithmetic is checked.

The fixed ceilings and mandatory 30-second request deadline make one attempt
finite. Starting a new attempt does not erase session replay limits,
authenticated-peer rate limits, or deployment abuse accounting.

### 6.3 Initiator audit budget

Responder budgets do not bound work the initiator performs while applying a
response. Every initiator therefore enforces a separate, nonzero, finite
per-session audit budget no greater than:

```text
local_summary_ops = 524,288
local_ids_examined = 1,048,576
local_chunk_ids = 1,024
```

Computing one initiator `RangeSummary` consumes one `local_summary_ops` unit.
Reading/comparing one local id in a terminal responder-empty or enumerated range
consumes one `local_ids_examined` unit. Local enumeration is chunked at no more
than `local_chunk_ids`; a response handler MUST NOT turn a remote-empty claim
into an immediate full local scan. Validated child ranges partition their
parent, and completed local ranges are recorded idempotently, so a local id is
not rescanned merely because a response is retried.

Before applying work that exceeds the remaining budget, the initiator persists
its traversal plus a local audit cursor containing the exact scope/view pair,
range, root-to-range path, remote summary, local offset/count, and last local
id. It stops issuing range requests, continuously drains the current request
window, persists every valid response, and then sends `Cancel`. Later it starts
from `All` against the byte-identical views, uses `BranchRebase` when the cursor
is below the root, and continues only after the fresh ticket's target is
byte-identical. It does not relabel that cursor onto changed views. Exhausting
this budget is a visible local `audit_budget_exhausted` stop and grants no
branch or completion credit.

For checkpoint auditing, `IdsKnown` requires both remote pagination and every
local terminal-range cursor to be exhausted. A transfer-only caller may skip
local-only discovery, but it MUST NOT reuse that weaker result for a checkpoint
claim. These caps bound a lying responder that claims an enormous local range
is remote-empty; repeated-session abuse remains subject to §5.7's peer/session
limits and deployment accounting.

### 6.4 Queue boundary

The existing 8 MiB per-peer and 2 MiB per-subscribed-stream queue bounds still
apply. A 64 MiB cumulative per-attempt allowance is transmitted over time; it does
not authorize 64 MiB of queued memory. Range work is lower priority than
governance and checkpoint traffic. No network-derived path may use an unbounded
channel or retain every decoded page in RAM; discovered ids and resume work may
be streamed to a bounded/persistent local cursor.

---

## 7. Completeness and validation

### 7.1 State boundary

Recommended internal states are deliberately not a public wire enum:

```text
Searching
  -> IdsKnown
  -> BodiesPending
  -> CandidateThroughCheckpoint
  -> CompleteThroughCheckpoint
```

Finishing range work can reach at most `IdsKnown`. Receiving an empty digest
response, equal `All` fingerprint, final id page, or empty work queue MUST NOT
skip the later states.

### 7.2 Body fetch

For every missing id, `WantEvents` / `EventBatch` obtains the body and
publication evidence. Before it enters the retained set, the receiver
independently checks at least:

- strict canonical body bytes, recomputes the `EventId`, and requires it to
  equal the exact id requested from `RangeIds`;
- Ed25519 signature and device binding;
- community and stream equality to the reconciliation scope;
- governance authorization at certified publication time;
- publication certificate and retention/cut membership; and
- device sequence/predecessor rules, subject to the boundary rule below.

A well-formed `RangeIds` item grants no authority and does not make a malformed
body acceptable. A valid body returned for a different requested id is
`invalid_response`, gives no branch progress, and is never stored under the
requested id. `EventBatch` retains #134's separate 1 MiB / 256-event bounds.

Here and below, "stored" means admitted to the receiver's validated retained-
set path; it is not a persistence receipt. RBSR completion, body validation,
page-cache/database visibility, or remote possession cannot create
`local_sync_group_v1`. A receipt-producing replica must separately pass
#156/ADR-0010's bounded synchronized commit before exposing its own receipt, and
a #159 replacement cannot use volatile reconciled bytes as stable-catch-up
readiness evidence.

Device sequence is scoped to `(community_id, device_id)`, not to a stream. A
valid first retained event can therefore reference either an event in another
stream or an event legitimately pruned below this retention generation. Full
link validation is required when the predecessor lies inside the certified cut.
When it lies outside, admission requires an authenticated checkpoint-bound
boundary/cross-stream proof instead. Until the stream-checkpoint device-cut
manifest and that proof are frozen, such a body remains
`verified_dependency_incomplete` in bounded quarantine and MUST NOT enter the
retained set. This profile does not invent the proof encoding.

### 7.3 Checkpoint gate

`CandidateThroughCheckpoint` requires all discovered missing bodies to be
validated, dependency-complete under §7.2, and stored. A
`verified_dependency_incomplete` body leaves the stream `Partial` and cannot
reach this state. `CompleteThroughCheckpoint` additionally requires:

1. a verified stream checkpoint and at least `W` valid active-replica
   signatures over the identical checkpoint body;
2. exact local retained-event count equality;
3. exact local sorted event-set Merkle root equality under #134 §13.3; and
4. every retained-interval device-chain dependency resolved by an in-cut
   predecessor or authenticated boundary/cross-stream proof.

Missing dependencies MUST be reported to operators, but reporting is not a
substitute for resolution and grants no completion credit.

The RBSR fingerprint is intentionally a different commitment from the
checkpoint Merkle root. A replica MUST NOT sign a checkpoint merely because an
RBSR view digest matched.

`local_sync_group_v1` is a §10.2 receipt assertion, not an RBSR completion state
or a stream-checkpoint signature. A storage-unready replica refuses checkpoint
votes as a safety predicate, but the still-unfrozen stream-checkpoint owner must
define the signer-side retained-storage and vote-atomicity contract. This spec
does not infer that contract from `CompleteThroughCheckpoint`.

If range work finishes but count/root comparison fails, report
`checkpoint_root_mismatch` and record the serving peer/view as non-proving. A
checkpoint-audit pass already identifies local-only ids at its terminal ranges;
if that audit was not retained, restart it from `All` against the same fixed
views rather than inventing a special unbounded inventory message. If the
mismatch remains, retry another certified replica. Never reinterpret the
mismatch as a new checkpoint or silently mark the local subset current.

### 7.4 Adversarial peer

A malicious responder can lie about counts, digests, splits, or ids, and can
withhold data without finding a fingerprint collision. Structural validation
and budgets bound the work; the checkpoint gate prevents false completeness.
Malformed responses provide no progress credit. Repeated invalidity/withholding
is operator evidence. #159/ADR-0011 permit bounded local peer selection and
operator-governed reconfiguration for that behavior but do not call it
checkpoint equivocation or automatically retire a `ReplicaId`; #155 itself
authorizes no removal behavior.

---

## 8. Backend interface

The Phase C adapter is semantic and fixed-view oriented. An illustrative shape
(not a frozen Rust API) is:

```rust
trait OrderedEventInventory {
    type View;

    fn pin(&self, scope: Scope) -> Result<Self::View, InventoryError>;
    fn all_summary(&self, view: &Self::View) -> Result<Summary, InventoryError>;
    fn range_summary(
        &self,
        view: &Self::View,
        range: Range,
    ) -> Result<Summary, InventoryError>;
    fn select(
        &self,
        view: &Self::View,
        range: Range,
        rank: u64,
    ) -> Result<EventId, InventoryError>;
    fn ids_from_offset(
        &self,
        view: &Self::View,
        range: Range,
        offset: u64,
        limit: u16,
    ) -> Result<Page<EventId>, InventoryError>;
}
```

The implementation may use an augmented B-tree or another self-balancing
order-statistic store, provided §3.6 and every byte/vector match. The adapter
must not expose document prefixes, LWW values, authors, timestamps, bodies, or
direct store mutation.

`iroh-docs` 0.101.0 is not a conforming adapter today: its useful ranger fields
are private, it specializes to `SignedEntry`, and its range summaries scan. A
future upstream release MAY be used only after an adapter conformance test shows
that upgrading or removing it changes no spec-owned message byte.

---

## 9. Normative amendments to #134 §§13.2 and 13.4

### 9.1 Range reconciliation (§13.2)

On acceptance, read #134 §13.2's mandatory behavior with this addition:

> v2 inventory discovery uses the directional, fixed-view Meyer-style RBSR
> profile in `specs/v2-range-reconciliation-envelope.md`. Ranges are explicit
> `All` or circular half-open intervals over raw lexicographically ordered
> `EventId`s; mismatch refinement is binary and exactly rank-balanced; explicit
> ids are pull-paginated. The fingerprint suite and all messages are owned and
> versioned by iroh-room. Dependency messages and document conflict semantics
> are never exposed on `/iroh-rooms/event/2`.
>
> A completed range pass is candidate inventory coverage only. A synchronization
> claim through checkpoint X additionally requires verification of X's quorum
> certificate, independent validation of every fetched body/publication
> certificate, exact retained count and §13.3 event-set-root equality, and
> resolution of every retained device predecessor by an in-cut body or an
> authenticated boundary/cross-stream proof. Reporting an unresolved dependency
> leaves the stream `Partial`. A fingerprint match,
> empty response, budget stop, or backend success flag is never completeness.
>
> Every pass is bound to one `(community_id, stream_id,
> retention_generation, checkpoint_id)` tuple and two immutable view digests.
> Branch tickets and page cursors are session-authenticated. Session rollover
> releases them; persisted work continues only after a fresh `All` exchange
> pins the byte-identical tuple/view pair and deterministic branch ancestry or
> page rank is revalidated. A changed view invalidates the persisted work.
> Control frames, ranges, ids, depth, cumulative work, retries, sessions, and
> queues are bounded as specified; limits never truncate silently.

### 9.2 Checkpoint client claim (§13.4)

Replace #134 §13.4 client-claim item 4:

> Resolve every missing device predecessor within the retained interval using
> either the validated in-cut predecessor body or an authenticated,
> checkpoint-bound retention-boundary/cross-stream proof. An unresolved
> predecessor MUST be reported and makes the stream `Partial`; reporting alone
> is not resolution and MUST NOT support a synchronization claim through X.

These amendments resolve §25 #1's algorithm and ownership choice and remove an
unsafe ambiguity at the body-validation boundary. They do not resolve
stream-checkpoint body encoding, concurrent checkpoint proposal policy, the
governance snapshot/admin-transition proof (#161), or replica
replacement/equivocation policy. #156/ADR-0010 separately resolve the receipt
durability-class semantics; RBSR supplies no shortcut around them.

---

## 10. Fixtures and wire-freeze criteria

### 10.1 Fingerprint vectors

Freeze exact, independently reproduced bytes for:

- the 64-byte `uniform_bytes` and encoded `P(EventId)` for ids `00..00`,
  `00..01`, and `ff..ff`;
- encoded identity, one-element, two-element, and reordered two-element
  accumulators (the last two sums must match);
- the complete BLAKE3 preimage and output for `digest(All)`, a normal half-open
  range, a wrapping range, and the empty digest under one fixed scope;
- a one-bit change in each scope field, boundary, count, suite, and accumulator;
  every change must change the digest; and
- RFC 9380/RFC 9496 mapping/encoding compatibility plus an independent
  implementation of the repository domain vectors whose XMD/Ristretto wrapper
  is not shared with the Rust implementation.

### 10.2 Range and algorithm vectors

Cover empty/equal sets, equal non-empty sets, one missing id on either side,
subset, disjoint, alternating dense difference, and boundaries containing
`00..00` / `ff..ff`. Pin normal/wrapping membership and reject equal endpoints.

For even and odd parent sizes, fixture the exact median, two child ranges, and
counts. Negative fixtures include overlap, gap, reversed child order, empty
child, wrong depth, count overflow/sum mismatch, and a `1/(n-1)` unbalanced
split.

### 10.3 Envelope and transcript vectors

Before stable wire, freeze canonical CBOR and 4-byte framing for every message
and error variant, then one complete multi-round transcript for:

- equal `All` summaries;
- a one-id difference that descends and pages;
- local empty / remote larger than 1,024 (multiple pages);
- local-only ids discovered by the forward checkpoint-audit pass;
- a wrapping-range page;
- two directional passes producing a replica union; and
- a disconnect after a page followed by a ticket/cursor resume;
- expiry during deep refinement followed by restart from `All`, a validated
  `BranchRebase`, and continued descent; and
- expiry during pagination followed by branch/anchor reacquisition and
  `ResumePoint` rebase.

Using a fixed, published non-secret `session_ticket_key`, freeze branch-ticket
and page-cursor authenticator preimages and expected 32-byte tags, successful
cross-attempt use, `Finish`/`Cancel`/`ResumeAttempt` exchanges, and the maximum
64-probe all-split `RangeDigest` with every accepted optional field. The last
fixture MUST remain within 65,536 body bytes.

Negative vectors cover non-canonical CBOR, unknown/duplicate/missing keys,
wrong widths, unsupported version/suite, wrong community/stream/checkpoint or
retention generation, altered view, request-id reuse with changed bytes,
unsolicited range/depth, forged or wrong-purpose ticket, changed summary for a
previously ticketed range, unsorted/duplicate/out-of-range ids, forged cursor,
wrong cursor rank/offset, short non-final page, early final page, cumulative
count underflow/overflow, mismatched-view/anchor page rebase, simultaneous cursor
and page rebase, non-minimal or forged branch path, nonzero unused path bits,
branch target mismatch, a path through an enumeration-terminal ancestor, and
response omission. Exercise the closed request/result union explicitly:
`branch_rebased` on `Compare`, outcomes 0 through 2 on `BranchRebase`, a missing
or extra `rebased`/`children`/`anchor`, and a mismatched or duplicate probe id
are all invalid.

### 10.4 Boundaries and adversarial outcomes

Test every §6 limit at exactly the boundary and boundary+1. Verify oversized
prefixes and collection lengths fail before proportional allocation. Exhaust
each cumulative budget independently and prove the outstanding work survives,
the whole-request reservation prevents partial work/results, the stop is
visible, and no completion state is emitted. Exhaust per-request and session
replay allowances, request/idle/absolute leases, attempts, active sessions, and
pinned-view admission; every case releases state and emits no completion.
Include a legal minimum-budget traversal whose page count exceeds 16 attempts;
it MUST cross a session boundary, rank-validate a rebase, and finish without
restarting the page offset at zero.

Separately, construct a legal minimum-budget traversal whose dependent
refinement path crosses the 16-attempt boundary before reaching an anchor. It
MUST start a new session, deterministically validate and re-ticket the persisted
branch, make forward refinement progress, and finish rather than replaying the
same root prefix forever. Exercise the same branch rebase for a persisted local
audit cursor.

Fill an eight-request window with maximum request/response reservations, reject
the ninth through the single admission-error reserve, and prove responses are
drained without waiting for the window to fill. With all eight range slots
occupied, admit `Cancel` through the separate lifecycle lane; the combined
1 MiB range, 8,192-byte control, 32,768-byte drain-error, and 4,096-byte
admission-error reservations MUST neither overlap nor grow. Overcommit the
remaining attempt budget within an eight-request window;
the first unreservable request and every later received slot MUST get its
bounded terminal error while earlier reservations finish. Cancel and trigger
view/lease expiry with an occupied window; state and reservations MUST remain
until the specified drain outcomes, then release exactly once. Exhaust
`local_summary_ops` and `local_ids_examined` independently,
including a lying root-level responder-empty claim over more than 1,048,576
local ids; local work pauses at the exact cap, resumes from its persisted cursor,
and cannot reach `IdsKnown` early.

Count the root ticket, reach exactly 524,288 issued/reserved ticket slots, and
reject boundary+1 before summary or rank work. Race multiple 64-probe all-split
requests near the boundary: serialized `2 * probes` reservations MUST prevent
overshoot, unused outcomes MUST refund slots, and the first request that cannot
reserve atomically MUST produce `session_rollover_required`, drain earlier
reservations, return the same error with no progress for every later received
slot, and rebase unfinished work under a fresh session without partial results.

Inject a false-equal digest, malformed split, peer withholding, invalid fetched
body, a valid body whose recomputed id differs from the requested id, valid body
for the wrong scope, unresolved retention-boundary/cross-stream predecessor, and
final count/root mismatch. Every case must remain below
`CompleteThroughCheckpoint`; subject to the independent signature and
checkpoint-Merkle collision assumptions, a forged reconciliation fingerprint
may reduce liveness but cannot establish false completeness.

Property tests cover range partition/coverage, balance/progress, pagination
without loss/duplication or early completion, branch provenance, transcript
summary consistency, bounded idempotent retries, lifecycle transitions,
bounded/out-of-order request windows, accumulator addition, and cursor
invalidation under arbitrary
checkpoint/retention/view/session changes.

### 10.5 Performance and implementation gates

On #134's reference workload — client offline 24 hours, missing 1% of 1,000,000
retained events, 100 Mbps and 50 ms RTT — reconcile, fetch, validate, and reach
the checkpoint gate in under 60 seconds. The gate runs the profile maxima of 64
probes per `Compare`, 1,024 ids per page, eight outstanding requests, the §6.2
maximum attempt budgets, and the §6.3 initiator audit ceilings; a serialized
one-request configuration is not the reference profile.

The workload is one client subscribed to one stream containing all 1,000,000
retained ids, one checkpoint-serving replica, no competing application traffic,
a direct (non-relay) path, a symmetric 100 Mbps bottleneck, 25 ms one-way delay,
and zero configured loss or jitter. The committed fixture generator constructs
1,000,000 canonical, valid signed bodies and publication certificates: 100
fixed test devices each publish sequences 0 through 9,999 to this stream, with
fixed keys and deterministic content bytes recorded in the manifest. It computes
the real `EventId` from each canonical body and then sorts those ids by raw
bytes. Run all three 10,000-id missing layouts over those sorted ranks:

1. **spaced:** sorted ranks whose zero-based rank is divisible by 100;
2. **clustered:** sorted ranks 495,000 through 504,999 inclusive; and
3. **scattered:** the 10,000 ranks with the lexicographically smallest
   `BLAKE3("iroh-room-v2/rbsr-perf-pick/v1" || rank.to_be_bytes()[8])` values.

Before any result is called a pass, the release owner MUST commit a versioned
fixture manifest that pins the exact event-body and publication-certificate
bytes and their individual/aggregate sizes, checkpoint/cut bytes, benchmark
CPU model and enabled core count, RAM, storage, OS/kernel, build profile and
commit, traffic-control commands, and hashes of all generated inputs. #134's
phrase "reference laptop" does not currently identify that hardware; #155
records the required fixture plan and makes the missing manifest a gate blocker
rather than silently choosing a machine.

For every layout, report **inventory discovery alone** and the **end-to-end
fetch/validation/checkpoint gate** separately. Record at least:

- range-summary operations and CPU time;
- control bytes, pages, and round trips;
- ids discovered versus ids transmitted;
- body/certificate bytes and validation time; and
- peak queued bytes and persisted cursor size.

The 60-second requirement applies to the end-to-end result for every layout;
range-only figures cannot substitute for it. The gate must demonstrate that the
initial `All` and subsequent range summaries use the maintained aggregate rather
than scanning the retained set.

### 10.6 Stable-wire checklist

Wire version 1 becomes stable only when all are true:

1. ADR-0007 and this normative profile are accepted.
2. Strict typed codecs and allocation guards implement the final schemas.
3. §§10.1–10.4 vectors pass in the Rust implementation and an independent
   vector generator/decoder.
4. The fingerprint construction receives independent cryptographic review.
5. Final stream-checkpoint/device-cut, stream-event-root domain, publication-
   certificate, replica-certificate, and historical-authorization schemas are
   accepted; the checkpoint count/root and §7.2 boundary-proof gates are then
   exercised end to end with no backend completion-boolean bypass. #161 defines
   the governance snapshot/admin-transition proof, and #159/ADR-0011 define
   replica replacement/equivocation semantics; #156/ADR-0010 define receipt
   durability, but the final
   receipt/class codec and stream-checkpoint storage predicate still need their
   explicit owners before Phase C claims this item.
6. The §10.5 performance gate passes with all budgets enabled.
7. The reviewed `degraded`/horizon/device-cut amendment fields are accepted into
   or explicitly excluded from version 1; the boundary proof itself cannot be
   excluded without a separately accepted replacement satisfying §7.2.
8. An `iroh-docs` version swap/removal differential test changes no encoded
   frame or transcript.
9. Version 1 and suite 1 are advertised only after the preceding items; later
   semantic/byte changes use a new version/suite and new vectors.

No public v2 interoperability claim is permitted until this checklist and the
remaining #134 §25 wire-freeze items are complete.

---

## 11. References

- Aljoscha Meyer, [*Range-Based Set Reconciliation*](https://arxiv.org/abs/2212.13567), arXiv:2212.13567v2 / SRDS 2023.
- Maitin-Shepard, Tibouchi, and Aranha, [*Elliptic Curve Multiset Hash*](https://arxiv.org/abs/1601.06502), arXiv:1601.06502.
- [RFC 9380, Appendix B: Hashing to ristretto255](https://www.rfc-editor.org/rfc/rfc9380.html#appendix-B).
- [RFC 9496: The ristretto255 and decaf448 Groups](https://www.rfc-editor.org/rfc/rfc9496.html).
- [`iroh-docs` 0.101.0 `ranger.rs`](https://github.com/n0-computer/iroh-docs/blob/v0.101.0/src/ranger.rs).
- [`iroh-docs` 0.101.0 network codec](https://github.com/n0-computer/iroh-docs/blob/v0.101.0/src/net/codec.rs).
