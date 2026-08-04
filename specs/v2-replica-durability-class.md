# Spec: v2 Replica Receipt Durability Class

| | |
|---|---|
| **Issue** | #156 — `[SPEC] §25 #7: Durability class (fsync vs group commit vs replicated WAL)` |
| **Refs** | #134 §§3, 5.2, 10.2–10.4, 13.4, 15.2, 16.2, 19, 21–22, 25 #7; #155; #157; #159; #161; ADR-0004; ADR-0007–ADR-0010 |
| **Status** | Proposed normative durability profile. Accepted on merge; Phase C implementation, receipt/full-descriptor encoding, and independent vectors remain required before advertisement. |
| **Scope** | Pure specification. No runtime, store, receipt codec, schema, CLI, or operator-recovery implementation. |

---

## 1. Decision summary

The mandatory v2.0 receipt durability class is:

```text
local_sync_group_v1
```

It means that the signing replica committed one local copy to qualified
non-volatile storage behind a successful synchronization barrier before the
signed receipt became externally observable. The reference realization is a
bounded synchronized group commit, not a commit to SQLite's page cache and not
an acknowledgement from another replica.

The default group closes when the first limit is reached:

| Bound | Default |
|---|---:|
| Events | 32 |
| Canonical event plus required persistence metadata | 1 MiB |
| Age of oldest admitted item | 10 ms |

A writer MAY close a batch earlier. A one-event batch is valid. It MUST NOT
extend the 10 ms grouping window, exceed the count/byte caps, or retain an
unbounded durability queue.

`local_sync_group_v1` names a semantic class. The eventual receipt and governed
replica-descriptor codecs may encode that closed enum as text or an integer, but
Phase C must freeze one exact representation and vectors before any stable wire
claim. Unknown or unsupported classes fail closed.

---

## 2. Scope and terminology

### 2.1 In scope

This profile defines:

- the exact persistence boundary that must precede a replica receipt;
- the v2.0 default batching policy;
- the SQLite WAL reference configuration and equivalent-backend contract;
- crash, power-loss, retry, ambiguous-commit, and storage-fault behavior;
- how the class composes with `W`, governance, replica identity, retention, and
  stream-checkpoint signing;
- the distinction between durable reference metadata and referenced blob bytes;
  and
- the implementation, test, and observability gates for Phase C.

### 2.2 Out of scope

This profile does not define:

- the canonical `ReplicaReceiptBody`, `PublicationCertificate`, descriptor,
  genesis, or `replica.set` bytes;
- where `W` or the governed durability-class enum is encoded;
- #134's still-open publication-certificate head-agreement behavior during
  ordinary governance churn;
- replica catch-up, activation, replacement, equivocation penalty, recovery
  commands, or operator UX, which #159 owns;
- physical replica placement or cryptographic proof of independent disks,
  hosts, sites, operators, or power supplies;
- durability of referenced blob payload bytes unless a later explicit policy
  adds those bytes to the receipt's covered object;
- Byzantine storage attestation, trusted hardware, proof of retrievability, or
  remote erasure coding; or
- any v1 persistence or receipt behavior.

### 2.3 Terms

**Admitted item**
: A fully decoded and validated publication that has passed bounded queue
  admission for one exact issuance context. Admission is not a receipt.

**Issuance context**
: The tuple needed by the final receipt/publication profile. At minimum it
  contains the exact `CommunityId`, governance sequence/head, full replica
  component root used as `replica_set_hash`, active `ReplicaId`, governed
  durability class, and authorization/retention policy generation.

**Stable commit**
: An atomic transaction whose commit returned success only after the selected
  storage backend completed the synchronization barrier required by
  `local_sync_group_v1`.

**Issued receipt**
: The exact signed receipt bytes durably recorded under an issuance context and
  released by the serialized writer to an outbound application path after the
  stable commit. A signature held only inside the writer before commit is not
  issued and MUST NOT escape through logs, metrics, tracing, an HSM audit
  export, or another API.

**Qualified local storage**
: Persistent storage whose filesystem, VFS, controller, device, and deployment
  contract are expected to honor a successful synchronization barrier on the
  replica that signs. Memory databases, temporary filesystems, and an
  unqualified remote/network filesystem do not qualify.

**Replica failure**
: Loss or unavailability of one receipt-bearing replica failure domain. The
  class does not prove where that boundary lies.

---

## 3. Normative class contract

### 3.1 Receipt-eligibility predicate

A replica may expose a `local_sync_group_v1` receipt only when all of these are
true:

1. The signed event passed every decode, identifier, signature, device-chain,
   community/stream, current governance, role, policy, freshness, and bounded-
   park rule required by the final publication profile. Parking an unresolved
   event does not itself satisfy receipt eligibility.
2. Authenticated governance proves that the signing `ReplicaId` is active and
   quorum-eligible, and that `local_sync_group_v1` is the class governed for it,
   at the exact head/component root in the receipt.
3. Receipt creation is serialized against authorization-relevant governance
   and replica-component transitions as specified in §4.4.
4. One atomic stable commit contains every authoritative item in §3.2.
5. The commit result is unambiguously successful. If success is uncertain,
   recovery proves the exact signed receipt committed before any delivery.
6. The exact issuance context remains eligible through release to the outbound
   path. A stale committed receipt is preserved as audit evidence, not emitted
   as new current work.
7. The replica's storage readiness has not been withdrawn by an I/O, capacity,
   read-only, corruption, barrier, checkpoint, or sequence-state fault.

Queue insertion, fold acceptance, an in-memory retry record, database-row
visibility, a `NORMAL` WAL commit, a successful network send, receipt signing
before stable commit, another replica's acknowledgement, and later hope of a
checkpoint are each insufficient.

### 3.2 Atomic durable set

The stable transaction MUST contain, or reference state already committed under
the same stable-local predicate for:

1. **Event identity and bytes** — the exact signed event bytes, the recomputed
   `EventId`, community and stream scope, and authoritative storage locator.
2. **Required reference metadata** — every blob hash/reference and durable
   pin/ownership record required to preserve and later serve the event's signed
   reference graph.
3. **Authorization provenance** — the exact governance head/component/class
   context and every local authoritative record required by the final
   historical-authorization proof and retention rules. A derived projection or
   index may be rebuilt, but its authoritative inputs cannot exist only in
   memory.
4. **Exact signed receipt** — its immutable canonical body and signature. The
   body includes the eventual #134 §10.3 fields, notably the event, governance
   context, component root, `ReplicaId`, receipt sequence, acceptance time, and
   exact durability class.
5. **Monotonic sequence state** — the new per-`(CommunityId, ReplicaId)` receipt
   high-water and any allocation record required to prevent reuse after crash,
   restore, endpoint migration, or concurrent retry.
6. **Idempotency state** — a unique mapping from the final receipt's semantic
   key to its exact stored bytes, sufficient to coalesce concurrent duplicate
   requests and recover/retransmit without allocating a new sequence.

The transaction MAY include derived indexes and local telemetry, but their
presence does not enlarge the protocol guarantee. The transaction MUST NOT
commit an event while leaving its receipt/high-water/idempotency state to a
later unsynchronized write and still release that receipt.

### 3.3 Blob boundary

#134 §10.2 says to persist the event and required blob references. In this
profile, the covered object is the exact event plus its durable reference/pin
metadata. The event contains the expected blob hashes, so those hashes remain
auditable and independently verifiable.

`local_sync_group_v1` does **not** by itself assert that attachment/blob payload
bytes have reached stable storage, are replicated `W` times, or remain
retrievable. If a future publication policy requires payload durability before
event acknowledgement, it must define:

- the exact covered blob identities and lengths;
- the blob store's stable barrier;
- crash-safe ordering or a transaction protocol across the blob and event
  stores;
- retention and garbage-collection pins; and
- receipt/vector changes that make that stronger claim unambiguous.

Until then, implementations and user interfaces MUST NOT translate an event
publication certificate into "all referenced files are durably available."

### 3.4 Failure boundary and non-claims

After a receipt becomes observable, the covered durable set MUST survive:

- abrupt termination of the replica process;
- operating-system crash or hard reset; and
- loss and restoration of external power,

provided the same qualified storage returns and the OS/VFS/controller/device
honestly honored the successful synchronization request. Recovery may replay a
WAL; it must not require bytes that existed only in volatile process or kernel
state.

The class does not survive or prove:

- catastrophic loss of the receipt-bearing medium or every receipt-bearing
  failure domain;
- a storage stack that lies about flush completion;
- malicious operator deletion or software that bypasses the sole writer;
- undetected bit rot or corruption beyond the store's integrity/recovery
  mechanisms;
- independence of two configured `ReplicaId`s;
- availability after policy-authorized retention expiry; or
- newest-governance knowledge merely because the receipt names one valid head.

### 3.5 Governed class and live readiness

The class used for quorum is authorized by authenticated governance, not chosen
by a `ServerHello`, command-line flag, local database, or receipt signer. The
full descriptor/genesis/`replica.set` schema owner and #159 must encode it in an
explicitly versioned profile. The current candidate descriptor's opaque
`capability: uint(0..255)` MUST NOT be reinterpreted as a durability class.

The live replica also advertises whether it is presently ready to produce that
class. Readiness means at least:

- the configured backend and effective barrier settings passed startup checks;
- the sole-writer invariant is active;
- receipt high-water state is readable and not known to have rolled back;
- the database is writable and not reporting corruption, full-disk, quota, or
  barrier errors; and
- the active issuance context matches authenticated governance.

Readiness is operational status, not a cryptographic storage proof. It may
withhold work but cannot authorize a class, lower `W`, or validate a receipt.
A readiness transition to false stops new receipt and affected stream-
checkpoint signing immediately. Earlier honest receipts remain historical
artifacts; they are not rewritten.

---

## 4. Bounded group-commit protocol

### 4.1 Single writer and queue

One dedicated writer owns all receipt-eligible mutations, sequence allocation,
stable commits, and issuance-state transitions for a store. No other writable
connection, maintenance task, migration, cache helper, or test hook may bypass
its durability configuration while the replica advertises readiness.

The writer's input queue is aggregate-byte-bounded and has explicit per-peer
and per-community admission limits. When full, the replica returns a typed
`overloaded` or storage-unavailable outcome before durability admission. It does
not accept an unbounded number of `Pending` publications and does not report
them as durable.

Validation and signature checks MAY run concurrently ahead of the writer, but
their result is bound to an immutable issuance context. The writer rechecks the
context fence; it never treats time spent in a queue as authorization.

### 4.2 Batch key and limits

A durability batch has this semantic key:

```text
(community_id,
 governance_seq,
 governance_head,
 replica_id,
 replica_component_root,
 durability_class,
 authorization_policy_generation)
```

The final publication owner may refine the policy-generation field, but it must
retain enough information to detect every transition that invalidates queued
authorization. A batch MUST NOT mix different keys. It MAY include multiple
streams under the same key if all stream-policy checks remain exact.

The batch closes at the first applicable boundary:

```text
event_count == 32 after admission
accounted_bytes == 1_048_576 after admission
next admission would exceed either hard cap
now - oldest_admission >= 10 ms
context fence changed
orderly shutdown requested
```

`accounted_bytes` includes canonical event bytes and the bounded authoritative
metadata/receipt rows the transaction will persist, not merely payload length.
The implementation records its exact accounting method. On a prospective-cap
boundary, the writer closes the current nonempty batch before considering the
item for a new one. An item that cannot fit an empty batch is rejected by the
applicable event/frame limit; it is never split into a receipt whose covered
state is incomplete.

### 4.3 Commit and release sequence

For each closed batch, the writer performs these steps in order:

1. Recheck that the batch key is the current eligible issuance context.
2. Begin one atomic write transaction and allocate a strictly increasing
   receipt sequence for every non-duplicate item.
3. Build and sign each exact receipt under #157's narrow application signer.
   Until commit succeeds, those signatures are writer-private and inaccessible
   to every network, logging, metrics, tracing, audit-export, and retry path.
4. Insert the complete §3.2 durable set, including exact signed receipts and the
   final sequence high-water/idempotency mappings.
5. Commit with the class's synchronization barrier.
6. Recheck/complete the serialization fence and release only the exact committed
   receipt bytes to the bounded outbound path.
7. Publish local acceptance/fanout/subscriber side effects only according to
   the final Phase C atomicity contract; none may imply a receipt before step 6.

Any unambiguous error before the stable commit rolls back the transaction and
exposes no receipt. An implementation whose external signing service retains
or exposes a pre-commit signature cannot use this one-barrier protocol; it
needs a separate durable reservation/issuance design and review.

A database API returning an indeterminate commit outcome is handled by §7. The
writer does not retry the transaction with newly allocated sequences until it
has recovered and queried the exact idempotency keys.

### 4.4 Governance and replica-transition serialization

Receipt issue and authorization-relevant governance transitions share an
explicit serialization fence. The implementation must establish one of two
orders:

```text
receipt stable-commits and is issued under context H
then transition H -> H' commits
```

or:

```text
transition H -> H' commits
then queued publication is revalidated and recertified under H'
```

It MUST NOT produce an interleaving in which the transition commits first but a
new receipt is then exposed under stale head/component/class `H`. Endpoint
rotation, signing-key replacement, receipt-quorum change, governed class
change, revocation, stream restriction, and a forked/incomplete governance view
all participate in this fence.

This rule does not resolve #134's broader certificate-head agreement problem:
honest replicas may still be at different valid heads. The receipt/publication-
certificate owner must specify how clients make progress without combining
incompatible contexts. #156 preserves exact context equality and does not
replace it with a head range or silent downgrade.

### 4.5 Duplicate, retry, and sequence behavior

Within one issuance context, concurrent or repeated publication of the same
`EventId` coalesces to one durable receipt record. After recovery, a duplicate
returns the exact stored receipt bytes and sequence only if the context remains
eligible for delivery. It does not allocate a fresh acceptance time or consume
another sequence.

The idempotency key is at least:

```text
(community_id,
 event_id,
 governance_head,
 replica_component_root,
 durability_class,
 replica_id)
```

A final receipt schema may require more fields; it MUST NOT require fewer if
doing so aliases semantically different receipts. The transport's bounded retry
nonce is not part of receipt identity or event validity.

A lower receipt sequence arriving after a higher one is not invalid solely due
to delivery order. Two different signed bodies from the same
`(CommunityId, ReplicaId)` at one sequence are equivocation evidence. Sequence
rollback or an unverifiable restored high-water makes the signer not ready. It
must not guess a larger counter, scan only locally available receipts, or reset
to zero; #159 restores authenticated state or activates a new `ReplicaId` and
namespace.

### 4.6 Shutdown and maintenance

Orderly shutdown stops admission, closes or aborts the current bounded batch,
waits for any stable commit to resolve, and persists no receipt that is absent
from the sequence/idempotency state. It need not wait for a WAL checkpoint after
a successful `FULL` commit.

Backup, compaction, migration, checkpoint, and integrity-check tasks coordinate
with the sole writer. A maintenance operation that weakens the barrier, swaps
storage, restores an older image, or cannot preserve the receipt high-water
withdraws readiness before it begins. Restoring event bytes alone is not enough
to resume the same signing identity.

---

## 5. Reference SQLite WAL profile

### 5.1 Required effective configuration

The reference Phase C implementation uses a file-backed SQLite database with:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
```

On macOS, when the VFS supports `F_FULLFSYNC`, it additionally uses and verifies:

```sql
PRAGMA fullfsync = ON;
```

`synchronous=EXTRA` is acceptable and has the same behavior as `FULL` in WAL
mode. The writer queries the effective `journal_mode`, `synchronous`, and
platform full-sync setting after configuration. A request that SQLite ignored,
a build/VFS without the claimed barrier, `journal_mode=memory`, or an
unexpected value leaves receipt readiness false.

The database, WAL, shared-memory file, and durable metadata reside on one
qualified persistent volume. `:memory:`, temporary storage, ephemeral
container layers without a qualified persistent-volume contract, and network
filesystems not explicitly qualified for SQLite WAL locking and stable barriers
cannot advertise this class.

### 5.2 Why `FULL`, and why no receipt-time checkpoint

SQLite documents that WAL plus `synchronous=NORMAL` performs no sync during
most transactions. Such a transaction remains atomic and survives an
application-process crash, but recent commits can roll back after an OS crash
or power loss.

With WAL plus `synchronous=FULL`, SQLite performs an additional WAL sync after
each transaction commit. The successful commit therefore supplies the barrier
for every event in the group. The durable commit record may remain in the WAL;
moving frames into the main database with `wal_checkpoint` is not a prerequisite
to receipt issuance.

The shipped v1 store's `WAL`/`NORMAL` setting is appropriate to its documented
throughput posture but does not implement `local_sync_group_v1`. Phase C needs a
separate v2 store/configuration. Changing the v1 pragma under this docs-only
issue would neither create receipt semantics nor preserve the intended scope.

### 5.3 Errors and checkpoint health

`SQLITE_FULL`, `SQLITE_IOERR`, read-only media, quota exhaustion, a failed sync,
unrecoverable `SQLITE_BUSY`/lock condition, malformed WAL, failed integrity
checks, and an ambiguous commit produce a typed storage/durability failure. The
replica:

- exposes no receipt for the affected transaction;
- withdraws live receipt readiness;
- stops signing stream checkpoints whose claimed set depends on the affected
  storage;
- preserves available fault/audit evidence; and
- requires the Phase C/#159 recovery rule before resuming.

A transient checkpoint failure does not retroactively erase a `FULL` WAL commit
that is known intact, but it is an operational storage fault that must be
surfaced and resolved. The implementation must not let unbounded WAL growth or
repeated checkpoint failures become a silent alternate durability mode.

### 5.4 Equivalent backends

Another backend may implement the class if it proves all of:

- one atomic transaction for the §3.2 durable set;
- a commit-time stable-storage barrier with the §3.4 crash boundary;
- exact recovery after process/OS/power interruption;
- monotonic non-reused receipt sequence state;
- idempotent exact signed-receipt recovery;
- bounded writer queue, batch count, bytes, and delay;
- fail-closed ambiguous-commit and I/O-error handling; and
- no hidden remote copy counted as an independent application receipt.

Backend names and APIs are not wire semantics. A backend migration while
retaining `ReplicaId` is safe only if the exact event/receipt/counter state and
barrier guarantee survive without rollback.

---

## 6. Receipts, certificates, checkpoints, and retention

### 6.1 Receipt verification

A verifier treats `durability_class` as a closed exact enum. A
`local_sync_group_v1` receipt is policy-eligible only if authenticated
historical governance proves that exact class for the active `ReplicaId` at the
receipt's exact governance head and component root.

There is no implicit ordering such as `replicated > local > process`. A receipt
using another or unknown class cannot count as `local_sync_group_v1`, even if a
peer claims it is stronger. A replica may use internal replication, mirrored
media, or more barriers and still truthfully advertise the baseline semantic
class; those internals add no quorum weight.

The signature is an authenticated assertion by the governed replica. It is not
a remote proof that the OS issued a barrier, the disk honored it, the bytes are
currently retrievable, or the signers occupy independent failure domains.
Violating the class is replica misconduct/storage failure handled by #159's
evidence and recovery policy.

### 6.2 Publication certificates and `W`

A `PublicationCertificate` counts at least `W` receipts only when they have:

- the same community, stream, and event IDs;
- the exact governance sequence/head required by the final publication profile;
- the same full replica component root;
- the exact `local_sync_group_v1` class; and
- distinct `ReplicaId`s proven active and eligible in that context.

Connections, endpoints, signatures, disks, hidden WAL followers, and duplicate
receipts do not add weight. Receipts from different heads, component roots, or
classes do not combine. A governed class change changes the full replica
component and creates a new certificate context.

Fewer than `W` valid matching receipts leaves the event `Pending` and, when
reachability is insufficient, reports `QuorumUnavailable`. A client or replica
MUST NOT lower `W`, mix classes, count a remote copy twice, or call one local
receipt `Durable` merely because that signer replicated internally.

With the #134 default `R=3, W=2`, a certificate normally represents two signed
local-stability assertions. Assuming honest independent storage behavior, loss
of either one still leaves the other copy. The certificate does not prove that
the two services were actually deployed on different failure domains.

### 6.3 Replica overlay and replicated WAL

When replica A forwards an accepted event to replica B, B obtains no durability
weight until B independently validates, stable-commits, and signs its own
receipt under B's `ReplicaId`. A's receipt cannot name B's storage as an unnamed
second copy, and B's later receipt cannot be counted both as A's backing and as
B's seat.

An unsynchronized remote WAL append is neither a local receipt nor a remote
receipt. A future replicated durability class would need exact authenticated
remote acknowledgements, local/remote stable barriers, recovery semantics,
failure-domain membership, deduplication against `W`, and its own class/receipt
version and vectors. It is not an extension bit on this class.

### 6.4 Stream checkpoints

`local_sync_group_v1` is the §10.2 receipt claim; a stream-checkpoint vote is not
itself a durability receipt. RBSR equality, `CompleteThroughCheckpoint`, and a
`W`-signed stream checkpoint prove different inventory/completeness statements
and MUST NOT be presented as this class.

A storage-unready replica refuses to propose or vote on a retained set as a
safety predicate. The still-unfrozen stream-checkpoint owner must define the
signer-side stable-retention predicate, checkpoint/vote atomicity, body,
certificate, cadence, proposer, and cut-proof bytes. This profile does not infer
that contract from RBSR state or silently require one per-event receipt for
every reconciled historical body.

### 6.5 Retention and compaction

The class means stable retention under the policy that authorized the receipt,
not storage forever. A receipt-bearing replica MUST NOT silently remove the
covered event/reference state before a governance-authorized, certified
retention boundary permits it. Compaction is ordered after the replacement
snapshot/checkpoint and retention proof are themselves durably installed.

After authorized expiry, the old receipt and certificate remain historical
evidence that the event was accepted and durably stored at publication time.
They do not claim that current replicas still serve it. Governance fork,
equivocation, historical-set/quorum evidence, and records required to validate
retained certificates follow the non-compaction rules in #134, #157, and #161.

### 6.6 Rotation and replacement

Endpoint-key rotation may preserve `ReplicaId` only if the qualified store,
exact receipts, and monotonic sequence namespace remain intact and single-
writer throughout the move. Copying an old database image and starting two
writers violates the class and #157.

Signing-key rotation creates a new `ReplicaId` and sequence namespace. A
replacement replica completes stable catch-up and passes class readiness before
#159 grants active quorum weight. If old sequence state is lost or rolled back,
the same `ReplicaId` cannot resume merely by choosing a higher-looking counter;
#159 restores authenticated state or performs replacement.

Receipts across the old/new component roots never combine. #156 does not define
the atomic `replica.set`, readiness proof wire, operator command, or recovery
approval that makes those steps happen.

---

## 7. Failure and recovery behavior

| Failure point or condition | Required behavior |
|---|---|
| Queue admission rejected | No sequence, transaction, signature, or receipt; return bounded `overloaded`/storage-unavailable outcome. |
| Process dies with an open/uncommitted batch | Transaction is absent after recovery; no signature escaped; client remains `Pending` and retries. |
| Sync/commit returns failure | Roll back the whole group; expose no receipt for any member; withdraw readiness when storage health is uncertain. |
| Commit result is ambiguous | Do not allocate/retry blindly. Recover the store and query exact idempotency keys; deliver only a proven committed signed receipt whose context remains eligible. |
| Stable commit succeeds, process dies before/during send | Exact signed receipt/event/sequence survive. Redeliver the same bytes only while its context is eligible; otherwise retain as audit and recertify. |
| Receipt send succeeds, response is lost | Same-context retry returns the exact stored receipt; no new sequence or acceptance time. |
| Signer fails before commit | Abort or remove the item; no receipt. Other prevalidated items may enter a newly formed valid batch. |
| Signer unavailable after a prior committed receipt | Stored exact bytes remain auditable/replayable subject to context; no new receipt is created until signer/readiness recovery. |
| Governance/context changes while queued | Close/abort the old batch, revalidate under the new context, and never mix the contexts. |
| Governance becomes forked/incomplete | Stop new receipts and affected checkpoint votes; already stored content remains available according to policy. |
| `ENOSPC`, quota, read-only, I/O, barrier, corruption, or persistent lock fault | Typed storage failure, readiness false, no silent fallback to `NORMAL`, memory, weaker class, or smaller `W`. |
| WAL is intact but main DB lacks recent frames after crash | Recover from the synchronized WAL; a receipt-time checkpoint was not required. |
| Restored backup has lower receipt high-water | Do not resume the same `ReplicaId`; invoke #159 recovery/replacement. |
| One of `R=3, W=2` receipt-bearing replicas is permanently lost | Remaining matching local receipt copy preserves availability if reachable; report degraded redundancy. |
| Fewer than `W` matching replicas are reachable | Individual receipts may exist, but publication is not `Durable`; report `Pending`/`QuorumUnavailable`. |
| Referenced blob payload is absent | Do not claim payload durability from this class; surface the separate blob-availability state. |
| Retention boundary authorizes deletion | Delete only through the checkpoint-aware transactional compaction rule; keep required historical evidence. |

Recovery MUST perform WAL/backend recovery and schema/integrity/readiness checks
before accepting new publication work. Finding an issued receipt whose event,
reference metadata, sequence state, or required governing evidence is absent is
a durability breach: stop signing, preserve the receipt and fault evidence, and
repair through #159 rather than silently reconstructing and claiming no fault.

---

## 8. Observability and operational contract

The Phase C implementation exports at least:

- durability queue bytes/items, aggregate and by community;
- group size in events and bytes;
- oldest-item wait and group-close reason (`count`, `bytes`, `time`, `context`,
  `shutdown`);
- transaction and synchronization-barrier latency, separately;
- receipt sign and post-commit release latency;
- committed, rolled-back, ambiguous, recovered, deduplicated, and stale-context
  receipt counts;
- current effective journal/synchronous/full-sync backend profile as bounded
  labels, never file paths or secrets;
- readiness state and stable typed cause for every withdrawal;
- WAL size, checkpoint age/result, disk free space, I/O/full/quota/corruption
  faults, and recovery duration;
- per-replica receipt high-water and rollback detection using a safe
  pseudonymous label; and
- time from first `PublishRequest` to `W`-receipt certificate, including class
  and direct/relay path in benchmark evidence.

Metrics never count queue insertion, fold acceptance, transaction begin,
receipt construction, signature generation, or socket write as a stable
receipt. The primary release counter is signed receipts released after stable
commit; its denominator and failure outcomes are explicit.

The exact CLI/operator presentation belongs to #159. That issue must consume
the typed readiness/storage/rollback states here and must not reduce them to an
unqualified "replica online" boolean.

---

## 9. Phase C implementation and conformance gates

No implementation may advertise `local_sync_group_v1` as stable until all of
these pass.

### 9.1 Codec and state-machine gates

1. The receipt/full-descriptor/genesis/`replica.set` formats encode the exact
   class and authenticated `W` in explicitly versioned containers.
2. Golden vectors pin positive receipts/certificates and unknown, wrong,
   mixed-class, mixed-head, mixed-root, duplicate-replica, and fewer-than-`W`
   failures. Two independent implementations reproduce the bytes/verdicts.
3. The store schema atomically binds exact event bytes, reference metadata,
   signed receipt, sequence high-water, and idempotency key.
4. Duplicate concurrent requests produce one sequence and exact receipt.
5. The governance serialization fence proves no stale-context receipt is
   released after a transition wins.
6. Receipt and stream-checkpoint paths refuse work while readiness is false.
7. In-memory/test stores cannot accidentally advertise the class.

### 9.2 Crash and fault matrix

Tests cover process termination:

- before transaction begin;
- after inserts but before commit;
- while the synchronization barrier is blocked;
- immediately after stable commit and before outbound release;
- during receipt transmission; and
- during orderly shutdown.

Every cut asserts the §7 outcome after reopen. Additional deterministic fault
injection covers full disk, quota, read-only transition, every relevant SQLite
I/O/sync error, persistent busy/lock timeout, corrupt WAL/database, checkpoint
failure, and ambiguous commit. A faulted group is all-or-none and yields no
receipt for an absent event.

Process-kill tests do not prove power-loss durability. Qualification also uses
a storage/VFS power-interruption or VM hard-reset harness that discards volatile
page cache and demonstrates WAL recovery without a pre-receipt checkpoint. The
test report identifies OS, filesystem, mount/storage type, SQLite/VFS version,
effective pragmas, and whether caches/barriers are virtualized. macOS evidence
asserts the intended full-sync setting explicitly.

### 9.3 Batching and load gates

Tests pin:

- closure at exactly 32 events, 1 MiB accounting, and 10 ms oldest-item age;
- early one-event flush without changing receipt semantics;
- no cross-context batch;
- aggregate queue/backpressure bounds under 2,000 concurrent publishers;
- one stable transaction/barrier amortized across a full group;
- no receipt release before that barrier completes;
- no starvation of governance/revocation work behind storage batches; and
- recorded p50/p95/p99 group wait, barrier, signer, one-receipt, and
  `W`-certificate latency.

The #134 replica-beta and large-beta workloads must meet their durable-publish
latency/throughput gates with this class enabled. A benchmark using
`synchronous=NORMAL`, clean shutdown only, an in-memory database, or a stubbed
barrier is not durability evidence.

### 9.4 Quorum and recovery gates

The distributed matrix includes:

- `R=3, W=2`: publish, obtain two matching receipts, permanently destroy one
  receipt-bearing replica, and retrieve the exact event from the survivor;
- fewer than `W`: never reach `Durable` and never lower class/quorum;
- a remote replica fanout/WAL copy that supplies no weight until its own stable
  receipt exists;
- distinct endpoint connections that cannot duplicate `ReplicaId` weight;
- endpoint rotation with intact store/high-water and no dual writer;
- restored old backup/high-water rollback that prevents same-key activation;
- signing-key replacement with a fresh namespace and no mixed-root
  certificate; and
- a receipt/event mismatch on recovery that becomes a visible durability breach
  and #159 recovery condition.

### 9.5 Remaining stable-wire blockers

Merging this pure-spec profile closes #156's semantic choice. It does not make
the publication wire ready. Stable advertising still waits for:

- the final receipt/publication-certificate schema and resolution of exact-head
  agreement under governance churn;
- the versioned full replica descriptor, governed class, `W`, and atomic
  replacement operation;
- the #157 endpoint resolver/binding implementation and successor genesis
  reconciliation;
- stream-checkpoint/cut/vote formats and their stable-local signing boundary;
- #159 replacement, rollback, equivocation, evidence, and operator recovery; and
- independent receipt/class/crash vectors and review.

---

## 10. Compatibility with current repository evidence

### 10.1 Phase-B candidate

`iroh-rooms-v2-core` is deliberately pure and contains no SQLite/store runtime,
receipt type, publication certificate, class enum, or receipt sequence table.
Its receipt domain constant is not an implementation of this policy. Existing
golden files correctly say replica-receipt vectors are absent from Track 2.

The candidate replica descriptor is exactly:

```text
{ endpoint: bstr, capability: uint(0..255), replica_id: bstr[32] }
```

It has no durability class, `W`, or full #134 §11.2 metadata. `capability` is not
silently overloaded. #157 already requires a successor genesis/full descriptor
and new `CommunityId`/fixture evidence. #156 adds semantic input to that future
version but does not decide its field numbers or encoding.

Frozen candidate vectors remain byte-identical and labeled as pre-stable
evidence. The Phase C implementation adds successor receipt/class/descriptor
vectors; it never rewrites existing files in place.

### 10.2 Current v1 store and teardown receipt

The shipped v1 event store uses one synchronous rusqlite connection per store,
`BEGIN IMMEDIATE` transactions, accepted-event transaction batching, WAL, and
`synchronous=NORMAL`. Those are useful implementation precedents for atomic
batching and contention, but do not satisfy the host/power-loss barrier here.

The v1 terminal-removal `EventsConfirmed` message is an unsigned, nonce-bound
row-presence confirmation. It has no `ReplicaId`, signature, governance head,
component root, sequence, class, or `W` semantics. It MUST NOT be relabeled as a
v2 persistence receipt or used as conformance evidence.

The current local publish path can retain a store failure in an in-memory retry
queue while its caller receives an accepted result. Phase C must not inherit
that API meaning for durable publication: only a verified `W`-receipt
certificate is `Durable`, and every storage failure remains a typed non-durable
outcome.

### 10.3 RBSR and snapshot evidence

RBSR discovers candidate inventory differences. Receiving, validating, or
matching an event through RBSR does not stable-commit it and cannot create a
receipt or make a replica #159-ready. A fetched body's transition from
validated or quarantined data to retained storage follows the separately
specified store policy; RBSR itself supplies none.

An atomically installed #161 governance snapshot is administrator-authorized
bootstrap state, not a replica persistence receipt. Its format-1 replica
component cannot authenticate a durability class or `W`; the activation gate
uses the successor governed profile. Governance-checkpoint administrator
signatures and replica receipt/checkpoint signatures remain separate
authorities.

---

## 11. Normative amendments to #134 and dependent profiles

### 11.1 Replica validation (§10.2 item 7)

Replace the operational meaning of item 7 with:

> Before exposing a `local_sync_group_v1` receipt, the active replica MUST use
> its dedicated bounded writer to atomically persist the exact event and
> required reference metadata, exact signed receipt, monotonic receipt-sequence
> high-water, idempotency mapping, and stable authorization provenance. The
> batch contains at most 32 events and 1 MiB of accounted bytes and closes no
> later than 10 ms after its oldest admission. Its commit succeeds only after a
> qualified local stable-storage synchronization barrier. Receipt bytes remain
> private until that commit and the serialized issuance context both succeed.
> A queue, page cache, SQLite `synchronous=NORMAL` commit, network send, remote
> WAL append, or later checkpoint is insufficient.

Parking in item 4 delays the remaining validation/receipt steps; it is not a
durability acknowledgement.

### 11.2 Persistence receipt and certificate (§10.3)

Add:

> v2.0's mandatory semantic durability class is
> `local_sync_group_v1`. A receipt's class must exactly equal the class governed
> for its active `ReplicaId` at the authenticated head and full replica
> component root. Unknown classes fail closed. A certificate counts `W`
> distinct `ReplicaId`s only when event, exact head, component root, and class
> match; it never orders/substitutes classes, counts hidden remote copies, or
> lowers `W`. A class/component transition creates a new certificate context.

The receipt/publication owner still must resolve exact-head agreement under
ordinary governance churn. This amendment does not change that open behavior.

### 11.3 Stream checkpoint signing (§13.4)

Add:

> `local_sync_group_v1` is a persistence-receipt class, not an RBSR or
> checkpoint-signature class. A storage-not-ready replica neither proposes nor
> votes. The stream-checkpoint format owner MUST separately define and vector
> the signer-side stable-retention and vote-atomicity predicate; inventory
> equality or volatile possession alone is insufficient.

The checkpoint owner must freeze the exact atomic vote boundary and bytes.

### 11.4 SQLite writer (§15.2)

Refine the dedicated batched-writer requirement:

> Receipt-eligible v2 writes use a sole dedicated writer with bounded
> aggregate queues and the §11.1 group limits. The SQLite reference is
> file-backed WAL with `synchronous=FULL` and verified platform full-sync
> behavior (`fullfsync=ON` on supporting macOS VFSes). A successful WAL commit
> is the stable receipt barrier; a receipt-time WAL checkpoint is not required.
> Other writable connections may not bypass the profile.

### 11.5 Failure behavior (§19)

Add or refine:

| Failure | Required behavior |
|---|---|
| Stable commit/barrier fails or is ambiguous | Issue no receipt; recover exact idempotency state before retry; withdraw readiness while storage health is uncertain. |
| Receipt sequence state rolls back | Do not resume the same `ReplicaId`; restore authenticated monotonic state or replace it through #159. |
| One of three replicas offline | Two matching `local_sync_group_v1` receipts may certify; report degraded redundancy and never infer physical independence from keys. |
| Fewer than `W` eligible/ready replicas | Keep publication `Pending`/`QuorumUnavailable`; never lower class or quorum. |
| Referenced blob payload unavailable | Preserve the durable event/reference and report blob availability separately; the baseline receipt does not cover payload bytes. |

---

## 12. Acceptance traceability

| Issue #156 acceptance | Resolution |
|---|---|
| Decision recorded with rationale and named default | ADR-0010 and §1 choose `local_sync_group_v1`: bounded synchronized local group commit. §§5–6 and ADR alternatives compare per-event sync, `NORMAL` group commit, replicated WAL, and page-cache acknowledgement. |
| §10.2 “durability class” has a concrete operational definition | §§3–5 define the exact durable set, stable barrier, writer/sequence/release algorithm, 32-event/1-MiB/10-ms bounds, governed readiness, SQLite profile, and equivalent-backend predicate. §11.1 supplies the amendment text. |
| Crash failure mode documented | §§3.4 and 7 distinguish pre-commit, in-commit, post-commit/pre-send, ambiguous result, process/OS/power loss, media loss, rollback, storage faults, and retention expiry. §9.2 defines the test matrix. |

---

## 13. Security properties and non-claims

If an honest replica follows this profile, an externally visible
`local_sync_group_v1` receipt cannot be created solely from volatile queue,
process, page-cache, or unsynchronized-WAL state. A failed group loses only
events for which no receipt escaped. Stable event, exact receipt, sequence, and
retry identity cross the crash boundary together.

With `W` honest receipt-bearing replicas on independent real failure domains, a
certificate limits loss from fewer than all of those copies failing. The
protocol authenticates the signers and their assertions; it does not remotely
measure their disks or prove independence. A malicious replica can lie about
barrier execution, and correlated storage can defeat the intended availability
assumption. #159's evidence/replacement policy and deployment qualification are
the containment layers; neither turns a signature into proof of storage.

This class does not authorize content, prove newest governance, guarantee blob
payload availability, prevent withholding, prevent operator deletion, survive
all-media loss, or promise storage past a certified retention boundary. Those
claims remain independently verified by governance, event signatures,
publication quorum, reconciliation/checkpoints, blob hashes/providers,
retention policy, and operational recovery.

---

## References

- #134, *Proposal: iroh-room v2 architecture for large communities*, especially
  §§10.2–10.4, 15.2, 19, 25 #7, and 26.
- [SQLite `PRAGMA synchronous`](https://www.sqlite.org/pragma.html#pragma_synchronous).
- [SQLite write-ahead logging](https://www.sqlite.org/wal.html).
- [SQLite atomic commit and storage assumptions](https://www.sqlite.org/atomiccommit.html).
- [SQLite database-corruption and sync caveats](https://www.sqlite.org/howtocorrupt.html).
- [`v2-replica-endpoint-identity.md`](v2-replica-endpoint-identity.md).
- [`v2-range-reconciliation-envelope.md`](v2-range-reconciliation-envelope.md).
- [`v2-governance-snapshot-transition-proof.md`](v2-governance-snapshot-transition-proof.md).
