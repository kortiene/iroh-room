# ADR-0009: Separate Replica Signing Keys from Iroh Endpoint Keys

- **Date:** 2026-08-03
- **Status:** Proposed — accepted when this decision is merged
- **Owners:** Protocol lead, release owner
- **Issue:** #157 — `[SPEC] §25 #2: Replica key vs Iroh Endpoint ID rotation`
- **Related:** #134 §§6.3, 7.2–7.3, 10.3, 11.2, 13.4, 25 #2; #146; #159; #161; ADR-0004; ADR-0008
- **Normative profile:** [`specs/v2-replica-endpoint-identity.md`](../../specs/v2-replica-endpoint-identity.md)

> **Additive resolution note (ADR-0011, 2026-08-03):** the future-tense #159
> clauses below are now resolved semantically by ADR-0011: zero-weight staging,
> predecessor-admin-approved atomic full-policy replacement, permanent disabled
> signing identities, and a committed prepare plus predecessor-`W` checkpoint
> handoff. Every stable-v2 governance-fork closure additionally requires #159's
> outcome-neutral current-`W`, selected-admin-approved fork-frontier control; it
> grants recovery keys no replica or
> administrator weight and does not alter `admin_seq`. Nested controls retain
> governance-carried exclusions and distinct fixed-size structural, signer-held,
> and collected-final dependency commitments whose corresponding bounded-chunk
> full-DAG proofs survive replay. Named Phase C owners still must freeze and
> implement the exact successor descriptor,
> governance, handoff, evidence, and history bytes. This note preserves the
> original decision text while superseding its open #159 references.

## Context

#134 gives a community replica two cryptographic roles:

1. an application signing key, whose public key is `ReplicaId`, signs durable
   persistence receipts and replica-certified stream-checkpoint votes; and
2. an Iroh endpoint key, whose public key is `EndpointId`, authenticates the
   QUIC/TLS peer and supplies its discovery address.

#134 permits those public keys to be equal only when their lifecycle
requirements are compatible, but leaves that gate and the rotation procedure
open. The simple choice is attractive because Iroh already uses Ed25519 and one
secret can prove both roles. It also makes every transport-key exposure a
receipt/stream-checkpoint signing-key exposure, forces an operational endpoint
change to replace the replica's application identity, and prevents keeping the
application signer in a separately controlled service or hardware-backed key
store.

There is also a pre-wire-freeze implementation mismatch to resolve. #134 §6.3
defines `ReplicaId` as the configured replica signing public key. The landed
Phase-B candidate instead derives `ReplicaId` as
`BLAKE3(REPLICA_RECEIPT || canonical replica descriptor)` and pins that
assumption in its identifier vector. Its `ReplicaDescriptor.endpoint` is an
opaque byte string that may be empty. Those choices cannot verify a receipt
directly under `ReplicaId` and do not establish the identity relationship this
decision is meant to settle. The actual descriptor also contains `replica_id`,
so deriving that id from the complete descriptor would be self-referential; the
candidate vector uses a toy preimage rather than the landed descriptor schema.

## Decision

The v2.0 profile uses **two independently generated, separately rotatable
Ed25519 key pairs** for every replica.

- `ReplicaId` is exactly the raw 32-byte Ed25519 replica-signing public key. It
  is not a hash of a descriptor and is not derived under the replica-receipt
  domain.
- `ReplicaDescriptor.endpoint` remains opaque snapshot bytes until Phase C
  freezes a versioned profile that resolves canonically to exactly one raw
  32-byte Iroh `EndpointId` authorized at one governance state.
- Active replica-signing and endpoint key sets MUST be disjoint, including
  across records. Their secrets MUST NOT be copied from or deterministically
  derived from one shared seed.
- A key admitted in one role remains barred from the other role for that
  community after retirement; authenticated role-key history is retained for
  admission checks.
- Operators MUST NOT reuse either secret across community replica instances or
  roles, including when separate communities cannot detect the alias.
- The administrator-quorum-approved governance state is the durable
  `ReplicaId` → `EndpointId` authorization binding.
- A live connection proves the other half of that binding: Iroh TLS proves the
  endpoint key, then a fresh, domain-separated challenge on that connection is
  signed under `ReplicaId`. A client sends no publish, synchronization, or
  content payload until both proofs match the same active descriptor and exact
  replica component root.

The application proof requires a purpose distinct from every receipt,
checkpoint, and other signature. The profile gives
`iroh-room-v2/replica-endpoint-binding` and a separate exporter label as
provisional examples; Phase C freezes their exact bytes. Its semantic inputs
include the community, exact replica component root, both public keys,
negotiated v2 ALPN, a fresh client nonce, and a 32-byte channel binding derived
locally from that Iroh connection's TLS exporter. A relayed signature from a
different connection therefore fails.
Governance head/sequence is local provenance, not a signed field, so a verified
linear descendant with the same replica component root can reuse the proof.

Cold bootstrap does not weaken the identity rule. Before binding, a bounded
lane may carry only public, self-authenticating governance evidence treated as
untrusted until verified from the ticket's external community/genesis anchor.
It carries no invite secret or proof and releases no private governance. Phase C
must separately solve private bootstrap with a ticket-pinned authenticated
current checkpoint/descriptor or a fully specified channel-bound capability
protocol before advertising private-community join.

Transport authentication never substitutes for an application signature. A
receipt or replica-certified stream checkpoint is verified under the
`ReplicaId` active in the exact governance/replica-set context named by that
artifact, regardless of which peer delivered it.

## Rotation semantics

### Endpoint-key rotation

Changing only the Iroh key preserves `ReplicaId` and its per-community receipt
sequence namespace. The operator provisions and preflights the new endpoint,
then an administrator-quorum-approved `replica.set` changes the endpoint bound
to the existing `ReplicaId`. At the new governance head, clients accept only a
TLS peer whose `EndpointId` equals that new descriptor and whose fresh binding
proof verifies under the unchanged `ReplicaId`.

Only one endpoint is authoritative for that replica at any one governance
state. A planned rollout may keep the old listener available only for bounded
public-bootstrap evidence used by clients still learning the governance update;
after observing the new head it issues no new receipt, replica vote, binding
proof, or normal work under the stale root. Old-root certification finishes
before cutover or is certified again under the new root. The replica signer and
its receipt-sequence storage must not roll back or split into two independent
writers during the move.

The endpoint change also changes the full replica component root. Receipts or
stream-checkpoint votes from the old and new roots cannot combine; pending work
must complete under the old root or be certified again under the new one.

### Replica-signing-key rotation

Changing the signing key creates a new `ReplicaId`; it is a replica replacement,
not an in-place mutation. #159 must make governance replace the old descriptor,
new descriptor, active set, and receipt quorum atomically. The predecessor and
successor cannot both count as independent seats in the governed one-for-one
replacement; this does not claim that governance can detect physical hosting.
Two sequential single-record upserts are not a safe rotation. The new key has a
new receipt-sequence namespace. Receipts from old and new replica-set hashes
cannot be combined into one publication certificate.

Historical receipts and replica-certified stream checkpoints remain
signature-valid under the old public key, but policy authorization additionally
requires retained authenticated evidence of the historical replica component,
active status, and quorum `W`. They are never re-signed. Retirement is
prospective: it cannot make a compromised old signature cryptographically
unforgeable or prove when an old-key signature was made. Persisted sequence
high-water marks, stream-checkpoint/equivocation evidence, the `W`-replica
quorum, and #159's recovery procedure contain that residual risk. An old key or
a cross-signature from it never authorizes its successor; only governance does.

## Why the identical-key gate fails

An identical-key profile would be permissible only if all of these remain true
for the deployment and protocol suite:

1. both roles use compatible algorithms, encodings, and independently reviewed
   domain boundaries;
2. the online transport secret receives the same custody, backup, rollback,
   access-control, and compromise treatment required for durable receipt and
   stream-checkpoint signing;
3. every transport and application rotation can occur atomically as one
   governance transition, with no need to preserve a stable `ReplicaId` or its
   receipt-sequence namespace across an endpoint move;
4. compromise, revocation, loss, or recovery of either role is always treated
   as compromise, revocation, loss, or recovery of both;
5. operators accept that transport software necessarily holds the complete
   application-signing authority and cannot isolate it behind a separate
   signer/HSM boundary; and
6. tooling prevents partial rotation and proves that old/new endpoints and
   counters cannot both issue conflicting current work.

v2.0 deliberately requires independent endpoint rotation and stable receipt
identity across ordinary host/network changes, so conditions 3–5 do not hold.
Equality is therefore rejected by strict descriptor validation rather than
left as an operator option.

## Compatibility and implementation boundary

This is a pure specification decision. It does not wire Phase C networking,
storage, receipt production, stream checkpoints, or operator commands. The
administrator-signed authority and record shape of #161 governance checkpoints
remain distinct and are never verified under `ReplicaId`; later versioned
descriptor/root/sample bytes may change as a consequence of this decision.

The decision invokes ADR-0004's review trigger for a §25 finding and supersedes
only the Phase-B candidate assumption that hashes a descriptor into
`ReplicaId`. Before any v2 format is advertised, a separate implementation pass
must:

- replace the hash-ID helper with a validated Ed25519 public-key newtype using
  the profile's canonical, non-identity, prime-subgroup admission predicate and
  strict signature verification;
- require stable-v2 genesis/`replica.set` and live authorization to resolve the
  versioned endpoint bytes to one valid 32-byte `EndpointId`, enforce disjoint
  active key sets, and reject cross-role reuse from retained community history;
- update strict governance validation, operational state checks, tests, and
  identifier/golden fixtures with an explicit record of the candidate-vector
  break;
- implement the bounded, TLS-exporter-bound challenge/response with a narrow
  rate-limited signer interface, reject application or 0-RTT payload before it
  succeeds, and close the private-bootstrap design before claiming that mode;
  and
- add independent positive and negative vectors for both key roles, wrong-key,
  wrong-community/component/ALPN, wrong-exporter/cross-connection, historical
  cross-role reuse, replayed-nonce, and rotation cases.

The two identity keys remain 32 bytes, but the landed three-field descriptor is
not the complete #134 §11.2 descriptor: it has no typed home for discovery/
relay hints, protocol features, operator label, or durability/retention data.
The current `CommunityId` hashes the complete `GenesisConfig`, including these
replica descriptors, so reconciliation changes genesis bytes and the derived
community id. It requires a successor genesis schema/vectors; a candidate
community id cannot survive an in-place semantic reinterpretation.
ADR-0004 also forbids reopening Phase-B wire semantics without an explicit
schema-version bump, and #161 requires a new format for a field-semantic change.
Phase C/#159 and the governance/snapshot schema owner must freeze a versioned
full replica descriptor and record whether the resolver inside #161's opaque
`endpoint` field is an external operational profile or a field-semantic change;
the latter requires an additive successor snapshot format. #156 supplies
durability-class semantics, not the encoding of `W`. #161's snapshot format-1
codec and vectors remain byte-identical candidate evidence and may preserve
opaque `endpoint` bytes; stable advertising is blocked until that ruling and
the applicable additive profile/format vectors exist.

The old descriptor-hash bytes MUST NOT be silently relabeled as a signing key
or cited as final interoperability evidence.

## Alternatives rejected

### Use the Iroh endpoint key for both roles

Rejected for v2.0. It is operationally simple, but couples unrelated rotations,
widens transport compromise into receipt/stream-checkpoint forgery, and prevents
separate signer custody.

### Derive both keys from one root secret

Rejected. Domain-separated derivation would produce distinct public keys but
retain one compromise and recovery domain, defeating the lifecycle separation.

### Keep descriptor-hash `ReplicaId` and add a signing key field

Rejected. It adds a third identifier and an unnecessary lookup while
contradicting #134's direct definition. The signing public key already is the
stable verification identity.

### Treat governance approval alone as proof of live key possession

Rejected. Governance authorizes the mapping, but a typo, stale deployment, or
stolen endpoint key could otherwise accept a connection without proving that
the configured replica signer is present. The fresh, TLS-channel-bound
application challenge and narrow signer boundary close that gap.

## Consequences and review triggers

- Replica deployments manage two keys and a small binding handshake.
- Ordinary endpoint rotation no longer invalidates receipt/stream-checkpoint
  identity or resets its sequence namespace.
- Signing-key rotation is intentionally visible as replica replacement and
  composes with #159 rather than creating a hidden alias chain.
- With the channel-bound proof and narrow signer boundary implemented, a stolen
  endpoint key alone can impersonate Iroh transport until governance rotates
  it, but cannot pass the application binding proof or forge replica artifacts.
  A stolen replica key alone can forge application signatures but cannot
  impersonate the configured Iroh endpoint.
- Revisit this ADR if Iroh changes its endpoint-key algorithm or authenticated
  peer-ID semantics, Phase C cannot obtain the TLS-proven remote `EndpointId`,
  hardware-backed signing makes the handshake latency unacceptable, or #159
  adopts a replacement model incompatible with the rotation invariants above.
