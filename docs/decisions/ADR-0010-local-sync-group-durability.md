# ADR-0010: Local Synchronized Group Commit for v2 Replica Receipts

- **Date:** 2026-08-03
- **Status:** Proposed — accepted when this decision is merged
- **Owners:** Protocol lead, release owner
- **Issue:** #156 — `[SPEC] §25 #7: Durability class (fsync vs group commit vs replicated WAL)`
- **Related:** #134 §§3, 5.2, 10.2–10.4, 13.4, 15.2, 19, 21–22, 25 #7; #157; #159; ADR-0004; ADR-0009
- **Normative profile:** [`specs/v2-replica-durability-class.md`](../../specs/v2-replica-durability-class.md)

> **Additive resolution note (ADR-0011, 2026-08-03):** ADR-0011 resolves the
> future-tense #159 clauses below. Same-identity recovery is permitted only when
> the exact continuous receipt high-water, checkpoint vote/generation journals,
> signer/store, and single-writer state recover in place; uncertain rollback
> permanently requires a fresh identity. The continuous journal includes every
> prepare, cancellation, fork-resolved fence/frontier, conflict, and cutover
> signer action that can advance or fence a checkpoint lineage, including every
> pending structural, signer-held, and collected-final nested-resolution
> dependency count/root and corresponding proof needed to recover it. ADR-0011
> also defines zero-weight staging, governed replacement, equivocation
> quarantine, and operator semantics. Receipt status is logically downgraded at
> the bounded durable community-incident-barrier commit, before resumable per-
> certificate materialization. Authoritative evidence/barrier/quarantine high-
> water, keyed signer-incident index including trigger-subject saturation, and
> direct-trigger records/cumulative subject aggregates/source revisions are
> exact recovery state; derived cursor/per-certificate progress and stale-pair
> index are validated or conservatively reset and rebuilt.
> Named Phase C owners still freeze/
> implement their exact bytes. This note
> preserves the original decision text rather than rewriting it.

## Context

#134 requires a replica to persist an event and required blob references
"durably enough to satisfy its advertised durability class" before issuing a
receipt. The receipt carries that class, and a publication certificate requires
at least `W` matching receipts. The proposal deliberately leaves the class
undefined and asks whether v2.0 should use an fsync per event, group commit, or
replicated-WAL acknowledgement.

That undefined term is load-bearing. A transaction can be visible after an
application restart without being stable across an operating-system crash or
power loss. In particular, SQLite WAL mode with `synchronous=NORMAL` can return
from most commits without synchronizing the WAL. Treating that return as a
durability receipt would let a certificate attest bytes that every signer could
lose in one power event.

The shipped v1 store uses WAL plus `synchronous=NORMAL`. It is not a v2 receipt
implementation and remains unchanged by this decision. #134 separately requires
the v2 store to have a dedicated batched writer, so the v2 decision can choose a
different persistence profile without changing v1 or the frozen Phase-B core.

## Decision

The mandatory v2.0 durability class is **`local_sync_group_v1`**: one
replica-local, non-volatile copy made stable by a successful synchronization
barrier, using bounded group commit.

The class name is a normative semantic identifier. Its eventual canonical-CBOR
representation remains part of the Phase C receipt/descriptor format and MUST
be frozen in independent golden vectors before stable wire advertising.

### Receipt boundary

A replica may expose a `local_sync_group_v1` receipt only after one atomic local
transaction has durably committed:

- the exact signed event bytes and authoritative event identifier;
- every required blob hash/reference/pin record covered by #134 §10.2, but not
  the referenced blob payload unless a separate policy explicitly includes it;
- the exact signed receipt, including its governance head, replica component
  root, durability class, acceptance time, and sequence;
- the per-`(CommunityId, ReplicaId)` sequence high-water mark; and
- enough idempotency state to reproduce the same receipt body and sequence
  after restart.

Every governance, replica-set, authorization, and retention record needed to
justify that issuance must already satisfy the same stable-local predicate or be
committed in the transaction. Derived indexes may be rebuilt and need not add a
second durability boundary.

The writer serializes receipt creation and release against governance changes.
Under one exact issuance context it allocates the sequence, builds and signs the
receipt into writer-private memory, atomically stores that exact signed receipt
with the event and high-water state, waits for the stable commit, and only then
places the bytes on an outbound path. The signing service MUST NOT log, retain,
or expose an uncommitted signature; a signer that cannot provide that narrow
boundary needs a separately durable reservation/issuance protocol.

If a revocation, replica-set change, stream-policy restriction, or other
authorization-relevant transition wins the serialization fence first, the
queued event is revalidated or rejected. A batch never mixes community,
governance-head, `ReplicaId`, replica-component-root, or durability-class
contexts. If its context changes before stable commit and release complete, the
old receipt is not exposed and the work is revalidated under the new context.
An ambiguous commit result is not success: recovery must prove that the exact
signed receipt committed, and must still verify that its context is issuance-
eligible, before delivering it. A committed stale-context receipt remains audit
evidence and is not retransmitted as current work.

### Bounded group commit

The default writer closes a batch when the first of these limits is reached:

- 32 events;
- 1 MiB of canonical event plus required persistence-metadata bytes; or
- 10 milliseconds since the oldest item entered the batch.

The count and byte bounds are hard caps. If admitting the next item would exceed
either one, the writer closes the current nonempty batch first and considers the
item for a new batch; an item larger than the empty-batch byte cap is rejected.
The queue is byte bounded. A replica may flush earlier, including a one-event
batch while idle, but may not silently extend the 10 ms grouping window. A
storage stall is bounded by the publish request deadline and backpressure
rules, not by keeping an unbounded batch open.

One synchronized commit covers every item in a batch. An unambiguous commit
failure produces no receipt for any item; an indeterminate result follows the
recovery rule below. Grouping changes only the volatile set of still-`Pending`
events: once the shared barrier succeeds, every event in the batch has the same
post-receipt crash guarantee as an event committed with its own barrier.
Per-event synchronization is therefore not a stronger acknowledged-event
guarantee; it is a batch size of one with more barriers and a smaller pre-
acknowledgement failure cohort.

The reference SQLite profile is:

- one dedicated writer connection;
- a qualified local persistent filesystem, not `:memory:`, temporary storage,
  or an unqualified network filesystem;
- `journal_mode=WAL`;
- `synchronous=FULL` (`EXTRA` is equivalent in WAL mode);
- `fullfsync=ON` on macOS where the VFS exposes `F_FULLFSYNC`; and
- startup/readiness verification of the effective settings and storage health.

A successful `FULL` WAL commit is the receipt barrier; a WAL checkpoint is not
required before receipt release. `synchronous=NORMAL` or `OFF`, page-cache
visibility, a successful socket write, and an unsynchronized WAL append do not
satisfy the class. An equivalent backend may be used only if it provides the
same atomic stable-commit, recovery, and fault behavior.

The guarantee assumes that the OS, VFS, controller, and device honor a
successful synchronization request. A lying cache or failed medium is a replica
failure, not something an application signature can prevent.

### Quorum composition

Each `ReplicaId` signs only for its own local stable copy. A remote WAL append,
shared disk, backup, or another replica's receipt cannot substitute for that
commit. At least `W` distinct active `ReplicaId`s must issue matching
`local_sync_group_v1` receipts before the publication is `Durable`.

The resulting certificate contains `W` governed signed assertions of local
stable storage. If each signer followed this profile, those assertions
correspond to at least `W` local copies, subject to the deployment's real
failure domains. The signatures do not cryptographically prove honest barrier
execution or distinct machines, disks, sites, power supplies, or operators. If
at least one receipt-bearing copy remains intact and reachable, loss of the
other receipt-bearing replicas does not erase the acknowledged event. Loss of
every such failure domain, operator deletion, media corruption, and deletion
permitted by a later certified retention boundary remain outside the class.

The governed descriptor/policy authorizes the class a replica may claim. A live
readiness advertisement reports whether its current storage configuration is
healthy, but cannot expand that authority. A replica with an I/O, capacity,
read-only, corruption, barrier, or sequence-rollback fault withdraws receipt
readiness and emits no new receipt; it never silently downgrades the class or
`W`. #159 owns the operator-facing recovery and replacement workflow.

## Crash and recovery semantics

- **Before commit, or after an unambiguous rollback:** the whole transaction is
  absent after recovery; no receipt has escaped, and the client remains
  `Pending` and retries.
- **Crash during commit:** no receipt has escaped. Recovery treats the result as
  ambiguous and proves whether the whole signed-receipt transaction committed
  before deciding whether to redeliver or retry.
- **After stable commit, before or during transmission:** the exact signed
  receipt survives. A duplicate request returns those bytes without consuming
  another sequence only while that exact context remains issuance-eligible; a
  stale-context receipt is retained as audit evidence and recertified instead.
- **Ambiguous commit:** reopen/recover and look up the exact signed receipt.
  Deliver only if it is proven committed and its context remains eligible;
  otherwise treat it as absent or stale audit evidence.
- **Sequence-state rollback:** the old `ReplicaId` cannot resume receipt duty if
  its durable high-water cannot be proven. #159 must restore verified monotonic
  state or replace the replica with a new signing key/namespace.
- **Storage fault:** return a typed storage/durability failure, keep the event
  non-durable, stop new receipt/checkpoint signing for the affected scope, and
  preserve evidence needed for repair.

`local_sync_group_v1` is the §10.2 receipt claim. RBSR completion, a
stream-checkpoint signature, and #161's administrator-signed governance
checkpoint are separate completeness/authority boundaries, not durability
receipts. Storage readiness is a safety precondition for a replica to vote on a
retained set; the still-unfrozen stream-checkpoint owner must define that vote's
exact stable-retention and atomicity semantics.

Receipt durability is bounded by the event's certified retention generation.
The certificate remains historical acceptance evidence after policy-authorized
expiry, but does not promise storage forever.

## Alternatives rejected

### Mandatory fsync per event

Rejected as the default. A stable group commit and a stable one-event commit
give the same guarantee after the receipt is visible. Per-event synchronization
multiplies barriers at the 1,000-event/s target and defeats the batched-writer
architecture without adding acknowledged-event durability. An implementation
may flush early or use one-event batches while still advertising the baseline
class.

### Group commit with SQLite `synchronous=NORMAL`

Rejected. It improves throughput, but most WAL commits do not issue a sync and
may roll back after an OS crash or power loss. Such an acknowledgement is only
process-crash durable and cannot back #134's `Durable` publication state.

### Replicated-WAL acknowledgement

Rejected as the v2.0 default. It makes one replica's receipt depend on overlay
availability and introduces another acknowledgement protocol, remote identity,
recovery rule, and failure-domain claim beneath the existing `W`-receipt layer.
It can also double-count one remote copy: once as hidden WAL backing and again as
that remote replica's own receipt. A remote copy may be an internal backup, but
it never increases quorum weight. A future replicated class needs a separately
versioned, authenticated, deduplicated protocol and security review.

### OS-page-cache acknowledgement

Rejected. It survives many process crashes but can vanish on the exact OS-crash
or power-loss boundary the class exists to cover.

## Compatibility and implementation boundary

This is a pure specification decision. It changes no v1 store setting, Phase-B
record, database schema, domain constant, or frozen vector. The current v1
`WAL`/`NORMAL` store MUST NOT be cited as implementation evidence for this
class.

Receipt verification is exact, not ordered by an invented strength lattice. The
class must equal the class governed for that `ReplicaId` at the authenticated
head/component root; unknown classes fail closed. A certificate counts `W`
distinct active `ReplicaId`s only when event, head, component root, and class all
match. Fewer than `W` stays `Pending`/`QuorumUnavailable`; implementations never
substitute a supposedly stronger class, lower `W`, or combine receipts across a
governed class transition. The still-open receipt/publication-certificate owner
must resolve #134's separate exact-head agreement problem under ordinary
governance churn; this durability decision does not silently change that rule.

The Phase-B candidate has no replica receipt type, no receipt quorum in genesis,
no durability-class field in its three-field replica descriptor, and no Phase C
store. #157 already requires a successor genesis/full-descriptor profile and
records the resulting `CommunityId` change. The schema owner and #159 must place
the governed class and `W`, version every affected container, and add successor
vectors rather than rewriting existing candidate bytes.

Before stable advertising, Phase C must add receipt/class encodings, exact
receipt/store schema, atomic sequence allocation, bounded writer behavior,
storage-readiness negotiation, crash/fault tests, and independently reproduced
golden vectors. No format may call `local_sync_group_v1` stable merely because
its transaction API returned success.

## Consequences and review triggers

- The default amortizes stable-storage barriers without weakening the meaning
  of a visible receipt.
- Receipt latency includes the bounded grouping window plus the real storage
  barrier and signer latency; the #134 p99 durable-publish gate measures all of
  it.
- Replica provisioning must qualify persistent storage and expose barrier,
  transaction, batch, queue, and readiness metrics.
- A replica cannot retain its signing identity after losing trustworthy receipt
  sequence state merely because its event bytes were recovered elsewhere.
- Revisit the decision if the 100/1,000-event/s Phase C workload misses its
  latency/throughput gate, an intended platform cannot provide a trustworthy
  local barrier, or a future threat model needs one receipt to prove multiple
  independently authenticated failure domains.

## References

- [SQLite `PRAGMA synchronous`](https://www.sqlite.org/pragma.html#pragma_synchronous)
- [SQLite write-ahead logging](https://www.sqlite.org/wal.html)
- [SQLite atomic commit and storage assumptions](https://www.sqlite.org/atomiccommit.html)
- [SQLite database-corruption and sync caveats](https://www.sqlite.org/howtocorrupt.html)
