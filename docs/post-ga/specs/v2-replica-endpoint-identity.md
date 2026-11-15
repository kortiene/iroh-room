# Spec: v2 Replica Signing and Iroh Endpoint Identity

| | |
|---|---|
| **Issue** | #157 — `[SPEC] §25 #2: Replica key vs Iroh Endpoint ID rotation` |
| **Refs** | #134 §§6.2–6.4, 7.2–7.3, 10.2–10.3, 11.2, 13.4, 19, 25 #2; #146; #147; #156; #159; #161; ADR-0004; ADR-0008–ADR-0011 |
| **Status** | Proposed decision/lifecycle profile — normative when ADR-0009 and this profile merge; §5 is a non-wire Phase C handshake sketch |
| **Scope** | Pure specification. #159/ADR-0011 now define replacement, equivocation, and operator semantics; Phase C implementation and exact handshake/receipt/checkpoint/evidence codecs remain separate work. #156/ADR-0010 define receipt durability semantics; their codec/store implementation remains Phase C work. |

> **Additive #159 lifecycle resolution (2026-08-03):** authenticated genesis may
> admit its initial valid active set directly; every post-genesis signing
> identity follows governed `staged -> active -> disabled` or terminal
> `staged -> disabled`, where staged has a bounded catch-up lane and zero quorum
> weight and disabled is a permanent same-role tombstone. Replacement stages a
> new independent key, proves stable checkpoint-relative readiness, then
> atomically changes old
> `active -> disabled` and new `staged -> active` in one complete successor
> policy carrying exact class and `W`. Verified same-slot checkpoint or
> same-sequence receipt equivocation quarantines the signer from new quorum
> decisions and requires permanent governed replacement. Rollback uncertainty
> never guesses a counter. Fork resolution retains governance-carried control
> exclusions plus structural, signer-held, and collected-final fixed-size
> dependency roots whose corresponding bounded-chunk full-DAG proofs survive
> snapshot/replay; local readiness or quarantine arrival never rewrites exposed
> governance bytes. See
> [`v2-replica-replacement-recovery.md`](v2-replica-replacement-recovery.md).

---

## 1. Decision and safety boundary

Every v2.0 community replica has two independent Ed25519 key pairs:

| Role | Public identity | Authority |
|---|---|---|
| Replica application signer | `ReplicaId` | Signs persistence receipts, replica votes used to certify stream checkpoints, and the sketched live endpoint-binding proof |
| Iroh transport endpoint | `EndpointId` | Authenticates QUIC/TLS, discovery, relay/direct routing, and the peer carrying application messages |

The key pairs MUST be generated independently, MUST have different public-key
bytes, and MUST be separately rotatable. They MUST NOT be derived from one root
seed or stored as aliases for one secret.

The administrator-approved governance state authorizes the mapping between the
roles. A live session is usable only after it proves possession of both keys:
Iroh authenticates `EndpointId`; a fresh application challenge is signed under
`ReplicaId`. Neither proof alone grants both authorities.

This resolves #134 §25 item 2 in favor of **separate keys**. It also corrects
Phase B's candidate assumption: in stable v2.0, `ReplicaId` is the configured
signing public key, as #134 §6.3 states, not a descriptor hash.

These service credentials do not become member or governance credentials.
`ReplicaId`/`EndpointId` possession grants no `PrincipalId`, `DeviceId`, role,
administrator approval, or recovery authority; replica signatures cannot
approve governance. A person or operator holding multiple roles still uses the
role-specific verification rules and SHOULD use independent secrets.

The key words MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, and MAY are
normative.

---

## 2. Scope

### 2.1 In scope

- The exact semantic meaning and validation of `ReplicaId`, plus the required
  resolution invariant for `ReplicaDescriptor.endpoint`.
- The governance and live-session binding between `ReplicaId` and `EndpointId`.
- Which key verifies each replica artifact.
- Endpoint-key and replica-signing-key rotation semantics.
- The lifecycle requirements an identical-key profile would have to satisfy.
- The implementation and vector gates required before v2 wire advertising.

### 2.2 Out of scope

- Phase C network/store implementation and an exact handshake message map.
- Receipt, publication-certificate, or stream-checkpoint formats beyond the
  identity rules needed here.
- Replica catch-up, replacement ordering, equivocation penalties, and operator
  UX, specified separately by #159/ADR-0011.
- The receipt durability-class encoding and implementation. #156/ADR-0010 now
  specify the `local_sync_group_v1` semantic default.
- Replica fault-domain placement or proof that two configured replicas run on
  independent hardware.
- v1 identity semantics. v1 continues to use `device_id == EndpointId`.

---

## 3. Normative identity model

### 3.1 `ReplicaId`

`ReplicaId` is a raw Ed25519 verifying key:

```text
ReplicaId = replica_signing_public_key   // exactly 32 bytes
```

Stable-v2 genesis/`replica.set` validation and every replica-artifact verifier
MUST apply one exact key predicate. A key is eligible only when its 32 bytes
decompress as an Edwards25519 point, re-encoding that point yields the identical
canonical bytes, and the point is non-identity and in the prime-order subgroup
(torsion-free). This is a new admission predicate, stronger than merely parsing
with the current candidate or pinned Iroh helpers, and may require explicit
curve-point checks. Independently, every replica signature uses strict Ed25519
verification with canonical signature encoding and small-order rejection.
Decompression-only or ZIP-215-permissive admission is insufficient. The same
eligibility predicate applies to the `EndpointId` resolved in §3.2 even if the
pinned Iroh parser accepts a wider key set.

Implementations MUST NOT compute
`ReplicaId` by hashing a descriptor, receipt, endpoint, or any other preimage.
`iroh-room-v2/replica-receipt` remains the domain for replica receipt
authentication; it is not a `ReplicaId` derivation domain. #161's generic
format-1 snapshot decoder may preserve arbitrary `bstr[32]` candidate state,
but that value cannot become an operational signing identity until this
validation succeeds.

Equality, ordering, set membership, and descriptor lookup use the 32 raw
public-key bytes. Human-readable presentation is not signed and MUST NOT enter
any hash preimage.

The holder of the corresponding secret may sign only the replica application
purposes assigned by the negotiated suite. At minimum those are persistence
receipts, replica votes used to certify stream checkpoints, and the sketched
endpoint-binding proof in §5. Each frozen purpose uses its own domain and strict
canonical body.

### 3.2 Iroh `EndpointId` and endpoint material

The active replica descriptor's endpoint material must canonically resolve to
exactly one raw Iroh public key:

```text
resolve_endpoint_id(endpoint_profile_bytes) = EndpointId  // Ed25519, 32 bytes
```

#161 freezes the outer `ReplicaDescriptor.endpoint` field as an opaque `bstr`.
This issue does not silently reinterpret that field as a bare 32-byte value or
tighten the format-1 snapshot decoder. Phase C must give the bytes an explicit
profile/version and a canonical resolver before they can authorize networking.
For an active stable-v2 descriptor, an absent/unknown profile, malformed
encoding, zero or multiple endpoint identities, or an invalid Ed25519 key fails
operational eligibility.

#134 §11.2 also assigns discovery/relay hints to the full replica descriptor.
Hints, supported features, operator labels, and durability/retention metadata
are not endpoint identity and MUST NOT substitute for `EndpointId`. #159/
ADR-0011 supply their successor lifecycle semantics; the Phase C governance/
snapshot format owner must freeze the exact versioned full-descriptor fields.
#156/ADR-0010 supply `local_sync_group_v1` durability-class
semantics rather than wire encoding; the candidate `capability` integer is not
that class and MUST NOT be overloaded as one. #161 requires a new snapshot
format version for a field-schema or
field-semantic change. #157 does not decide whether a versioned resolver inside
the intentionally opaque `bstr` is an external operational profile or such a
semantic change. Before stable advertising, the governance/snapshot format
owner MUST record that compatibility ruling, freeze the endpoint-profile
discriminator and vectors, and add a successor snapshot format if the ruling
finds a semantic change. Format 1 is never silently tightened.

The authenticated endpoint of an accepted Iroh connection MUST equal the
resolved current descriptor `EndpointId` exactly. Phase C must obtain the peer
value from Iroh's authenticated connection API, not from an untrusted request
field, ticket cache, or address hint.

### 3.3 Descriptor invariant

For one governance state, an active descriptor means:

```text
ReplicaDescriptor {
  replica_id: ReplicaId,       // application verification key
  endpoint:   bstr,            // versioned profile resolving one EndpointId
  capability: uint(0..255)
}
```

The descriptor is committed by genesis or an administrator-quorum-approved
`replica.set` and by the replica component of the governance state root and
snapshot. A genesis descriptor or `replica.set` transition producing an
**active** record that violates this section rejects before it can affect
governance state. Opaque candidate bytes preserved by #161's low-level snapshot
codec never acquire networking or quorum authority from successful decoding.

A disable/replacement transition MUST be able to retire an opaque, malformed,
equal-key, or otherwise ineligible historical descriptor without first making
it valid. Disabled records preserve evidence but carry no live networking or
quorum authority. #159/ADR-0011 define permanent retirement/tombstone semantics;
the Phase C governance/snapshot owner freezes the exact operation and proof.

Within a community, active `ReplicaId`s are unique, resolved active
`EndpointId`s are unique, and the two complete active key sets are disjoint:

```text
set(active ReplicaId) ∩ set(active EndpointId) = ∅
```

This catches cross-record aliasing such as `ReplicaId_A == EndpointId_B`, which
would otherwise let B's transport-secret compromise forge A's replica
artifacts. One active `ReplicaId` maps to exactly one active `EndpointId` at one
governance head. This profile does not claim that distinct keys imply
independent hosts, operators, disks, or fault domains; deployment and
durability policy must make those guarantees separately.

Role separation is also permanent across accepted community history. A public
key once admitted as a `ReplicaId` MUST NOT later be admitted as an `EndpointId`
for that community, or conversely, even after retirement. Otherwise a retained
or stolen old-role secret would gain authority in the other role. Admission
therefore consults authenticated, non-compacted per-community role-key history,
including every resolvable active and disabled descriptor. #159/ADR-0011
require that retained history; the Phase C governance/snapshot schema owner
must freeze and implement its commitment before stable replacement admission.
This cross-role tombstone rule
composes with #159/ADR-0011's same-role rule: every disabled `ReplicaId` is a
permanent tombstone, including after planned rotation.

### 3.4 Key custody

The two secrets have separate lifecycle records:

- independent CSPRNG generation and provenance;
- separate access-control and backup/recovery policy;
- separate compromise and rotation triggers;
- no shared seed export, deterministic child derivation, or byte-for-byte copy;
- no secret reuse across community replica instances or between transport and
  application roles, even when protocol state cannot detect the other use;
- endpoint secret available only to the Iroh transport boundary;
- replica signer usable through a narrow signing interface and SHOULD be held
  in a separate process or hardware-backed service; and
- secure erasure of retired secrets while retaining public keys and signed
  historical evidence.

An implementation may colocate the two services on one machine. Colocation
does not relax distinct-key generation, access boundaries, or rotation rules.

---

## 4. Governance binding

Genesis and `replica.set` authorize a `ReplicaId` → `EndpointId` pair as part of
the replica component at one governance head. Genesis uses the administrator
set, threshold, and genesis signatures carried by its own verified config;
there is no predecessor state. Every post-genesis `replica.set` uses the
applicable administrator threshold evaluated under its authenticated
predecessor governance state. Possession of either replica secret does not
authorize a mapping change.

The protocol field named `replica_set_hash` is #161's exact **replica component
root**, over full active and disabled `ReplicaRecord`s. It is not an active-only
set hash. Stable use must derive active eligibility, status, uniqueness, and the
configured receipt quorum `W` from authenticated decoded governance state. The
candidate model does not encode `W`; #157 does not choose its location. #159/
ADR-0011 require explicit `W`, while the Phase C governance/snapshot schema
owner must freeze its representation and version every changed container.

A verifier consuming a descriptor MUST know all of:

- expected `CommunityId`;
- accepted governance sequence and `GovernanceId`;
- exact replica component root carried as `replica_set_hash`;
- authenticated active/disabled records and configured receipt quorum;
- active/disabled status;
- `ReplicaId`; and
- the `EndpointId` resolved from `endpoint`.

The verifier MUST reject a descriptor learned only from discovery, a replica
response, or a self-signed claim. Snapshot installation inherits the exact
proof and trust-anchor requirements in
`v2-governance-snapshot-transition-proof.md`; a snapshot cannot authorize its
own replica mapping. Low-level snapshot decode/hash reproduction is not
operational activation: if any active descriptor fails this profile, stable-v2
activation of the entire replica component fails closed. An implementation
MUST NOT silently filter the bad record or recompute quorum over a subset.

Governance approval establishes authorization, not availability or current key
possession. The live check in §5 supplies possession.

---

## 5. Live `ReplicaId` ↔ `EndpointId` possession proof

This section is the binding-mechanism sketch required by #157. It freezes
security properties and lifecycle inputs, not canonical map keys, message
framing, a wire version, or final domain/exporter labels. A dedicated Phase C
handshake spec must own those bytes and independent vectors before negotiation
advertises them.

### 5.1 Bounded cold-bootstrap lane

A new client cannot start with a current authenticated descriptor: #134 tickets
carry a `CommunityId`/genesis trust anchor and cached bootstrap descriptors,
then the client fetches governance state. Cached descriptors are routing hints,
not authority.

Before normal binding succeeds, an implementation MAY expose one bounded
bootstrap lane carrying only public, self-authenticating proof material needed
to authenticate current governance:

- genesis and its threshold signatures;
- administrator/recovery transition evidence;
- the administrator-certified governance checkpoint and snapshot transfer; and
- the governance tail.

Every received byte is an untrusted carrier object until independently verified
from the caller's expected `CommunityId` and external genesis trust anchor.
#161's size, decompression, allocation, proof, and installation gates apply.
The lane cannot publish, subscribe, reconcile content, fetch private blobs,
carry an invitation secret or proof, release private governance, or grant
replica quorum weight. A cached or stale endpoint is routing, not current
replica authority, so a capability MUST NOT be disclosed to it merely to escape
the bootstrap dependency.

If governance or the current descriptor is private, this public lane is
insufficient. Before stable private-community join can be advertised, Phase C
must separately specify either a ticket-pinned authenticated current checkpoint/
descriptor or a channel-bound capability protocol with explicit transcript,
privacy, replay, and redaction rules. #157 does not invent such a protocol or
claim that private bootstrap is solved.

After verifying current governance, the client selects the now-authoritative
descriptor and performs §5.2. If the bootstrap connection's authenticated
endpoint is stale or different, it closes that connection and dials the current
one. Withholding or corrupting bootstrap bytes can deny service but cannot
choose the trust anchor or authorize a replica.

### 5.2 Normal-session sequence

Before a connection may carry a publish request, reconciliation message,
subscription, stream-checkpoint transfer, content body, blob authorization, or
any other non-bootstrap/non-handshake application payload, the verifier MUST:

1. select an active descriptor from an authenticated governance state;
2. resolve and dial its `EndpointId` using the v2 ALPN;
3. read the peer identity from the authenticated Iroh connection and compare it
   byte-for-byte with the resolved descriptor endpoint;
4. generate a fresh unpredictable 32-byte client nonce;
5. derive a 32-byte TLS-exporter channel binding from that local connection;
6. request a bounded replica endpoint-binding proof on that same connection;
7. verify the response signature under the descriptor's `ReplicaId`; and
8. verify every context value in §5.3, including its independently derived
   channel binding, against local expected state.

Failure at any step closes or quarantines the connection and emits a bounded
audit reason. A peer must not be provisionally authorized for application data.
Zero-RTT/replayed application data MUST NOT bypass this sequence.

Every peer claiming replica-only privileges performs this proof. On a
replica-to-replica session, each side proves its own governed pair before
replica-only work; an ordinary member client does not need a `ReplicaId` of its
own. The proof shows that the governed signer cooperated with this TLS channel,
not that the two secrets are physically colocated.

### 5.3 Illustrative signed context and channel binding

The Phase C wire should allocate distinct signature and exporter purposes, for
example:

```text
REPLICA_ENDPOINT_BINDING = "iroh-room-v2/replica-endpoint-binding"
REPLICA_ENDPOINT_EXPORTER = "iroh-room-v2/replica-endpoint-channel/v1"
```

The strings above are provisional reservations, not frozen wire constants in
this issue. An illustrative strict canonical body contains at least:

| Field | Rule |
|---|---|
| schema version | Exact version selected by the v2 handshake |
| community id | Expected `CommunityId` |
| replica component root | Exact #161 root carried as `replica_set_hash`, including active and disabled records |
| replica id | Descriptor `ReplicaId` |
| endpoint id | TLS-authenticated peer `EndpointId` |
| ALPN | Exact negotiated v2 protocol identity |
| client nonce | Exact fresh 32-byte request nonce |
| channel binding | Exact 32-byte TLS exporter derived locally on this connection |

Conceptually, both endpoints derive:

```text
channel_binding = TLS-Exporter(
  connection,
  label = REPLICA_ENDPOINT_EXPORTER,
  context = canonical(community_id, ALPN, client_nonce),
  length = 32
)
```

and the replica service signs:

```text
signature = Ed25519-Sign(
  replica_secret,
  REPLICA_ENDPOINT_BINDING || canonical(binding_proof_body)
)
```

The service component authorized to request this signature must derive the
exporter from its own Iroh `Connection`; it must never sign channel-binding
bytes supplied by the peer. The verifier independently derives and compares the
exporter. The nonce prevents replay, while the exporter prevents forwarding the
challenge to a legitimate signer over a different TLS connection. No second
detached endpoint-key signature is needed because TLS already proves that key.

The replica-signer boundary accepts no arbitrary preimage. It constructs or
validates the closed proof body from its locally authorized community,
descriptor/component root, authenticated connection, negotiated ALPN, and the
bounded peer nonce; a transport caller cannot choose identity/root fields or
turn it into a general signing oracle.

The final Phase C design may use an equivalent channel-bound transcript, but it
must preserve these anti-replay, anti-relay, community, ALPN, identity, and
replica-component-root properties. It must use a closed bounded envelope,
global and per-peer rate/concurrency limits, a bounded signer queue, a narrow
non-general-purpose signer API, timeout/cancellation, and typed overload
failure. Attacker-controlled lengths are checked before allocation or signer/
HSM work.

### 5.4 State changes, connection reuse, and freshness

The proof authorizes one replica component root selected from authenticated
governance. The governance head/sequence remains local provenance and is not a
signed transcript field, so an ordinary verified linear descendant with the
same `replica_set_hash` need not force another signer/HSM call. Any unresolved
governance fork fails closed. A changed replica component root has zero
permitted freshness lag under #134 §10.2 and requires a new proof before new
normal work.

If the resolved endpoint changes, the old TLS connection still proves the old
key and MUST be closed; endpoint rotation always requires a new connection. A
connection may be reused after re-proof only when its authenticated endpoint
still equals the new active descriptor, such as a signing-key-only transition
that retains the endpoint.

Binding proves possession and authorization relative to locally accepted
state. It does not prove that state is newest. Checkpoint/high-water freshness
still comes from a separately pinned state, persisted high-water mark, or live
independent quorum comparison as required by #161.

---

## 6. Artifact verification

### 6.1 Persistence receipts

A receipt is valid only if:

- authenticated historical governance evidence proves its `ReplicaId` was
  active and quorum-eligible at the receipt's exact governance head and
  `replica_set_hash` (the full replica component root);
- its signature verifies under that raw `ReplicaId` using the receipt domain;
- its sequence/body does not conflict with retained per-`(CommunityId,
  ReplicaId)` evidence; and
- every other receipt rule in #134 §10.3 and the eventual receipt format passes.

A producer MUST allocate receipt sequence numbers durably, strictly
increasingly, and without reuse for one `(CommunityId, ReplicaId)`. Two
different signed bodies at the same sequence are equivocation evidence. A lower
sequence received after a higher one is not invalid solely because transport or
history delivery was out of order; live freshness/high-water policy is a
separate check.

For the v2.0 default, the receipt's class MUST equal the
`local_sync_group_v1` class governed for that `ReplicaId` at the exact
head/component root. Before any receipt bytes escape, one bounded synchronized
group transaction atomically persists the exact event/reference metadata,
exact signed receipt, sequence high-water, and idempotency state behind a
qualified local barrier. The exact rules and crash boundary are in
[`v2-replica-durability-class.md`](v2-replica-durability-class.md). Queue
admission, an in-memory retry, SQLite `synchronous=NORMAL`, remote fanout, or an
RBSR match does not satisfy them.

The endpoint that delivered a detached receipt is irrelevant to signature
validity. A TLS-authenticated endpoint cannot sign or validate a receipt merely
because governance binds it to the replica.

### 6.2 Publication certificates

Receipt quorum counting is by distinct active `ReplicaId`, never by endpoint,
connection, address, process, or signature count. Receipts from different
governance heads or replica component roots do not combine. Reusing one
endpoint for multiple connections cannot increase quorum weight. #157 does not
choose the still-missing authenticated representation of `W`; #159/ADR-0011
define its policy semantics and the Phase C governance/snapshot owner freezes
the bytes.

Class equality is also exact. Unknown classes fail closed; v2.0 defines no
ordering or "stronger counts as weaker" substitution. Hidden remote WAL copies,
shared storage, and internal mirroring add no quorum weight. Fewer than `W`
matching active signers remains `Pending`/`QuorumUnavailable` and never lowers
the class or `W`. A certificate is `W` authenticated durability assertions, not
cryptographic proof of independent disks or honest barrier execution.

### 6.3 Replica-certified stream checkpoints

Replica votes/signatures used to certify a stream checkpoint are verified under
the `ReplicaId`s active in the exact checkpoint governance/component context.
The live transport peer does not become checkpoint authority. #157 does not
choose whether replicas sign the checkpoint body or a vote envelope, nor its
exact preimage, domain, or certificate encoding; the stream-checkpoint owner
must freeze those bytes. The administrator-signed governance-checkpoint family
from #161 remains a different authority/record-shape boundary.

### 6.4 Historical authorization evidence

An old public key or descriptor proves only which key can verify a signature;
it does not prove that the key was active, what the replica component root was,
or which receipt quorum applied. Policy-valid historical receipt and
stream-checkpoint verification requires authenticated historical replica-set
evidence: a checkpoint/snapshot/log segment or future inclusion proof that
establishes the exact governance head, full replica component, and `W`.

#161's sparse authority proof omits ordinary `replica.set` operations and does
not by itself supply this historical policy proof. #159/ADR-0011 define the
retention semantics; the Phase C evidence/history and receipt/stream-checkpoint
owners must freeze the proof and compaction rules, including handoff/
cancellation controls, `ForkResolvedFenceStatement`/
`ForkResolvedFrontier`/`replica.handoff.fork_reconcile` history, governance-
carried control exclusions, every pending structural/signer-held/final nested-
resolution count/root and its corresponding dependency proof, and retired-
signer cutovers that affect current use.
Until then, a cryptographically correct old-key signature may be reported as
signature-valid, but not as policy-authorized. Implementations must retain or
fetch the governing evidence and must not compact it merely because the key is
retired.

---

## 7. Rotation

### 7.1 Endpoint-key rotation, stable `ReplicaId`

A planned endpoint rotation is:

1. generate a new independent Iroh secret;
2. provision the replica service so the existing replica signer can answer the
   proposed channel-bound challenge behind the new endpoint;
3. run an operator-local, non-authorizing proposed-descriptor proof of
   possession; the new endpoint cannot pass normal §5 while it is not active,
   and #159/ADR-0011 define the staging boundary while the Phase C identity/
   handshake owner freezes its exact mechanism;
4. accept an administrator-quorum-approved `replica.set` that changes only the
   endpoint for the existing `ReplicaId`;
5. at the new head, accept only the new endpoint and a new binding proof; and
6. retire and erase the old endpoint secret after the bounded rollout/recovery
   window.

The `ReplicaId`, historical signatures, and receipt-sequence namespace remain
unchanged. The signer/counter store MUST remain single-writer or otherwise
atomically monotonic across the move; copying an old database or signer state
must not roll back receipt sequence or any checkpoint vote/generation journal.

Changing endpoint material changes the full replica component root even though
`ReplicaId` and its sequence namespace remain stable. Receipts or
stream-checkpoint votes from the old and new roots cannot combine. Pending work
must complete under the old root or be re-certified under the new one.

Only one endpoint is authoritative at a given head. The old endpoint may remain
reachable briefly solely to serve the bounded public-bootstrap evidence needed
by stale clients to learn the governance update. Once local state observes the
new head, it MUST NOT issue a new receipt, replica vote, binding proof, or accept
normal work under the stale root. Old-root certification must finish before the
transition or be recertified under the new root. The client closes the old
connection and re-proves binding at the new endpoint after learning the state.

For an endpoint-key compromise, skip any convenience grace period: use #159/
ADR-0011's ordinary predecessor-admin-authorized endpoint-update or replacement
procedure with typed `endpoint_key_compromise` cause, stop trusting the old
mapping at the new head, rotate address/discovery material, and audit the
exposure. There is no separate emergency authority. The attacker still cannot
pass §5 at the new endpoint or sign replica artifacts without the separate
replica key.

### 7.2 Replica-signing-key rotation

Because `ReplicaId` is the public key, changing the signing key creates a new
replica identity. It MUST be represented as replica replacement:

1. generate an independent signing key and a distinct endpoint key as needed;
2. catch the replacement replica up and prove it ready without granting it
   receipt quorum weight, including `local_sync_group_v1` stable-store and
   monotonic-sequence readiness under #156/ADR-0010;
3. commit #159's predecessor-admin-approved prepare reservation, obtain its
   predecessor-`W` checkpoint-frontier bundle, approve the derived activation
   child, and atomically replace the old active set entry with the new `ReplicaId`
   without dropping below the required durability/quorum policy;
4. start a fresh receipt-sequence namespace for the new
   `(CommunityId, ReplicaId)`; and
5. retain the old public descriptor, authenticated governing-set evidence, and
   signed artifacts for historical verification, then erase the old secret.

The predecessor and successor MUST NOT both count as independent active quorum
seats in a governed one-for-one replacement. This is a state-transition rule,
not proof that the services occupy one physical machine. Two sequential
single-record upserts are unsafe: add-first temporarily inflates quorum weight,
while disable-first may drop the active set below policy. #159/ADR-0011 define
the required full-set/quorum transition as zero-weight staging followed by one
complete-policy old-admin-authorized old-disable/new-activate operation; its
exact codec remains Phase C work.

No mutable alias maps the old `ReplicaId` to a new signing key. The old signer,
an old-to-new cross-signature, endpoint possession, or operator metadata cannot
authorize the successor; only governance can. A compromised signing key MUST
never be reactivated. #159/ADR-0011 choose permanent tombstones for every
disabled signing identity, including safely planned retirement; exact
continuous in-place crash recovery is not retirement or reactivation.

A publication certificate cannot straddle the replica-component transition:
every counted receipt agrees on the exact governance head and component root.
Pending work must finish under the old set or be republished/re-certified under
the new set according to the eventual receipt protocol.

### 7.3 Combined rotation

If both secrets must change, governance performs the signing-key replacement
above with a descriptor containing the new endpoint. The transition is still
one atomic replacement, not an endpoint update followed by two quorum-visible
signing identities. Compromise response does not require possession of or a
signature from either old secret.

### 7.4 Fork, history, and compromise semantics

Rotation is prospective. Given authenticated historical set/quorum evidence, a
verifier uses the replica state active at an artifact's named governance head,
so retirement does not cryptographically invalidate correctly formed
historical receipts or replica votes and does not require re-signing them.

Conversely, an Ed25519 signature alone cannot prove that it was created before
a private key was compromised. An attacker holding a retired replica secret
may attempt to forge an old-head artifact. Implementations MUST retain
per-replica sequence high-water marks and signed stream-checkpoint/equivocation
evidence, MUST enforce same-head/component-root quorum agreement, and MUST
surface the incident to #159's recovery path. Those records can reveal reuse or
conflict but cannot stop a stolen signer from forging an unused higher
sequence. The intended `W > 1` quorum limits one stolen share; it does not
repair a quorum compromise or prove when a signature was made.

Competing rotations on unresolved governance branches are ordinary governance
fork evidence. New replica admission, receipt/quorum decisions, and
replica-certified stream checkpoints fail closed until `fork.resolve` selects
an authenticated branch. Receipts from different branch heads or replica
component roots never combine, and no lexicographic or arrival-order tie-break
chooses a rotation.

---

## 8. Identical-key lifecycle gate

v2.0 requires the complete active replica-signing and endpoint key sets to be
disjoint. A future version or algorithm suite may permit equality only if all
of these are demonstrated and golden-vectored:

1. **Primitive compatibility:** both roles use the same validated key type and
   encoding, and cross-protocol use has been reviewed with distinct signature
   domains.
2. **Custody equivalence:** the online transport key receives the same secret
   isolation, access control, backup, rollback protection, and audit treatment
   required of the receipt/stream-checkpoint signer.
3. **Atomic lifecycle:** creation, activation, rotation, revocation, loss, and
   recovery always affect both roles in one governance transition.
4. **Identity continuity:** the protocol and deployment require no stable
   replica identity or sequence namespace across an ordinary endpoint move.
5. **Compromise equivalence:** compromise of either role is always handled as
   compromise of both, with no claimed containment benefit.
6. **Operational support:** tooling prevents partial rotation and proves that
   old/new endpoints and counters cannot both issue conflicting current work.

The v2.0 requirements for separately rotating network identity while preserving
replica receipt identity fail items 3–5. Equality is therefore a protocol
validation error, not a deployment preference.

---

## 9. Compatibility and the Phase-B candidate

The current unused `iroh-rooms-v2-core` candidate differs from this profile:

| Candidate behavior | Normative v2.0 behavior |
|---|---|
| `ReplicaId::from_replica_descriptor_csb` hashes under `REPLICA_RECEIPT` | `ReplicaId` wraps and validates the raw replica Ed25519 public key |
| identifier golden vector pins a descriptor-hash digest | final vector pins public-key bytes and signature verification |
| descriptor `endpoint` is opaque `Vec<u8>` and may be empty | an active versioned endpoint profile canonically resolves exactly one eligible Iroh `EndpointId` |
| `CommunityId` commits the candidate `GenesisConfig` and replica descriptors | successor genesis bytes derive a new `CommunityId`; no in-place reinterpretation |
| no live two-key proof exists | Phase C must freeze a nonce- and TLS-exporter-bound `ReplicaId` proof before normal work |

The landed descriptor itself contains `replica_id`; deriving that id from the
complete canonical descriptor would therefore be self-referential. The
candidate identifier vector avoids the cycle only by hashing a toy descriptor
preimage that is not the actual governance descriptor schema.

The current `CommunityId` is also
`BLAKE3(COMMUNITY || canonical(GenesisConfig))`, and `GenesisConfig` contains
the complete replica descriptors. Replacing descriptor-hash IDs, adding the
stable endpoint profile, or adding the missing receipt quorum changes genesis
bytes and therefore the derived `CommunityId`. The implementation reconciliation
must add a successor genesis schema and vectors. A candidate community cannot
preserve its `CommunityId` through in-place reinterpretation; any migration
mechanism is a separate explicitly versioned protocol decision.

The candidate `replica.set` payload/apply path upserts one record and its
genesis model does not carry #134's receipt quorum. It therefore cannot express
the atomic signing-key replacement required by §7.2. #159/ADR-0011 require a
versioned successor full-policy-plus-quorum operation; the current single-record
upsert is not rotation support.

ADR-0004 accepted Phase B while explicitly keeping §25 decisions open and
names a §25 conflict as a review trigger. This profile invokes that trigger and
supersedes the candidate assumption only at this boundary.

This issue remains pure specification. It changes no code or golden bytes and
therefore needs no byte-fixture bump itself. The later implementation
reconciliation is a protocol change: ADR-0004's Phase-B freeze requires it to
bump the applicable fixture/schema/profile version, record the change, and
label the old value as pre-#157 candidate evidence. The absence of a public v2
deployment does not permit rewriting a frozen vector in place or silently
relabeling the descriptor hash as a signing key.

#161's format-1 snapshot field schema, decoder, and vectors are not changed
here: `replica_id` remains `bstr[32]`, and `endpoint` remains an opaque `bstr`
that the snapshot codec preserves exactly. A candidate snapshot containing
descriptor-hash IDs or unresolved endpoint bytes may still decode and reproduce
its state root, but it is not stable-v2 interoperability evidence and cannot
authorize a replica session. The later implementation MUST record the §3.2
compatibility ruling before advertising either a layered endpoint profile or an
additive successor snapshot format. In either case, the entire active replica
profile must pass §3 atomically before networking or quorum use; filtering
failed entries is forbidden. Changed snapshot/profile vectors are additive, and
the frozen format-1 evidence remains labeled as pre-#157 candidate evidence.

---

## 10. Phase C implementation and interoperability gate

No v2 implementation may advertise a stable replica/receipt/stream-checkpoint
format until all of these pass:

1. `ReplicaId` is a dedicated Ed25519 public-key type with no descriptor-hash
   constructor on a normative path and both key roles enforce §3.1's exact
   canonical/subgroup/strict-verification predicate.
2. Stable-v2 genesis/`replica.set` validation and operational descriptor
   resolution enforce unique/disjoint active key sets before networking or
   quorum use, reject cross-role reuse from authenticated community history,
   and still permit an invalid historical record to be disabled; #161's
   low-level format-1 decoder remains byte-preserving and stable activation
   never filters individual failures. Provisioning tooling also rejects
   detectable secret reuse across locally managed community instances.
3. Receipt and replica-vote verification looks up raw `ReplicaId` using
   authenticated historical governance/component/quorum evidence and never
   substitutes `EndpointId`; #161 governance checkpoints remain
   administrator-signed.
4. A dedicated Phase C handshake spec freezes a bounded closed-schema
   request/response with nonce and locally derived TLS-exporter channel binding,
   plus signer rate/concurrency/queue/overload behavior and public-bootstrap
   limits; private bootstrap has the separately authenticated design required
   by §5.1 before it is claimed.
5. Golden vectors cover canonical, weak, noncanonical-equivalent, small-order,
   identity, and decompression-failing keys; pairwise and cross-record key
   collisions; historical cross-role reuse; wrong endpoint/replica/community/
   component/ALPN; replayed nonce; wrong exporter; and a relayed proof from a
   different connection.
6. Rotation tests show endpoint-only rotation preserves `ReplicaId` and receipt
   sequence, while one atomic full-set signing-key replacement creates a new
   namespace, never counts predecessor and successor as two seats, and cannot
   mix certificates/votes across replica component roots.
7. Crash/restore tests prove endpoint rollout cannot roll back or fork the
   per-replica receipt counter, and atomically bind each exact signed receipt to
   its event/reference/idempotency state behind #156's stable barrier.
8. Receipt tests distinguish producer sequence reuse/equivocation from benign
   out-of-order delivery and do not overclaim compromise containment.
9. #159/ADR-0011 supply the full-policy lifecycle, atomic replacement,
   catch-up/equivocation, and operator semantics; the governance/snapshot schema
   owner freezes their exact descriptor, `W`, readiness/evidence, and history
   bytes. #156/ADR-0010 supply the exact `local_sync_group_v1` semantics and
   bounded group-commit/crash predicate. The format owner records §3.2's
   compatibility ruling, versions every affected container, and keeps changed
   vectors additive.
10. Historical replica-set/quorum and role-key evidence has a frozen proof and
    retention owner before compaction discards anything needed by retained
    artifacts, prepare/fork-frontier controls, or cross-role admission checks.
11. Candidate descriptor-hash fixtures are explicitly versioned/labeled and
    cannot be selected by stable-v2 negotiation; successor genesis vectors show
    the resulting `CommunityId` change rather than preserving it by fiat.
12. A second implementation reproduces public-key eligibility, the final
    channel-bound proof, receipt verification, and rotation vectors.

---

## 11. Normative amendments to #134 and dependent specs

### 11.1 Identifier row (§6.3)

Replace the `ReplicaId` row semantically with:

> `ReplicaId` is the configured replica Ed25519 signing public key, encoded as
> exactly 32 raw bytes. It is not hashed or descriptor-derived. In the v2.0
> profile it MUST be independently generated from every active endpoint key;
> the complete active `ReplicaId` and `EndpointId` sets are disjoint, and a key
> admitted in either role is permanently barred from the other role in that
> community.

### 11.2 Genesis and `replica.set` (§§7.2–7.3)

For stable-v2 genesis, an active `replica.set`, and operational authorization,
resolve each descriptor's versioned `endpoint` profile as exactly one eligible
Iroh `EndpointId`. Genesis and predecessor-authorized governance bind that key
to `ReplicaId`. Invalid, duplicate, current cross-role-colliding, or historically
cross-role-reused keys fail the entire active profile; disabled evidence may
retain opaque historical bytes.

Changing only the endpoint rotates transport identity without changing replica
identity; changing `ReplicaId` is one atomic full-set replacement and follows
#159/ADR-0011. That profile supplies authenticated receipt-quorum and full-
descriptor semantics; the Phase C governance/snapshot schema owner freezes the
exact fields and operation. #156/ADR-0010 supply
`local_sync_group_v1` durability semantics, not those fields' encoding. This
issue leaves #161's format-1 decoder byte-identical and requires
the explicit §3.2 compatibility ruling plus any resulting successor format
before stable-v2 use. The successor genesis schema derives a new `CommunityId`;
it cannot reinterpret an existing candidate id in place.

### 11.3 Publication/stream-checkpoint verification (§§10.2–10.3, 13.4)

Replica artifacts are signed and quorum-counted by distinct raw `ReplicaId`s
proven active by authenticated historical governance at the artifact's named
head and replica component root. Iroh authentication protects the live path but
never replaces application verification. #157 assigns replica keys to the
votes used for stream-checkpoint certification but leaves the vote/certificate
wire to its owner. Before normal application data, a live replica claimant must
pass a nonce- and TLS-channel-bound proof with §5's bounded-bootstrap exception.

For persistence receipts, policy verification also requires exact agreement on
the governed durability class. The v2.0 class is `local_sync_group_v1`; its
stable local barrier and failure semantics are defined by #156/ADR-0010. It does
not turn transport identity, another replica's copy, or a checkpoint signature
into a receipt.

### 11.4 Phase C binding-purpose requirement (§6.2)

The final binding signature/exporter must use purposes distinct from receipts,
checkpoints, and every other signature/hash. This issue provisionally reserves:

```text
iroh-room-v2/replica-endpoint-binding
iroh-room-v2/replica-endpoint-channel/v1
```

Phase C, not #157, freezes their exact bytes when it freezes the transcript and
vectors. `iroh-room-v2/replica-receipt` continues to authenticate receipt
records and no longer serves any `ReplicaId` derivation.

---

## 12. Acceptance traceability

| Issue #157 acceptance | Resolution |
|---|---|
| Decision recorded | ADR-0009 and §§1, 8 select separate keys and record why |
| If separate, binding mechanism sketched | §§4–5 bind the keys through authenticated governance, Iroh TLS identity, and a nonce- plus TLS-exporter-bound replica proof; public bootstrap is bounded and private bootstrap remains an explicit Phase C gate |
| Rotation story documented | §7 distinguishes endpoint-only rotation from signing-key replacement and records historical/compromise semantics |
| Key-lifecycle requirements gate identical option | §8 gives the six mandatory conditions and explains why v2.0 fails them |
| Implementation remains Phase C | §§2.2, 9–10 leave code, exact handshake wire, and operator UX to their owning phases/issues |

---

## 13. Security properties and non-claims

This profile provides:

- independent compromise domains for transport and replica artifacts;
- governance-authorized mapping plus live possession of both configured keys;
- replay- and cross-connection-relay-resistant binding to one community,
  replica component, endpoint, and ALPN;
- stable receipt identity across endpoint rotation; and
- explicit signing-key replacement without hidden aliases.

It does not prove that configured replicas have independent operators, disks,
regions, or durability failure domains. It does not prevent denial of service
by a stolen endpoint key before governance rotation. It does not retroactively
invalidate signatures made or forged with a compromised replica key. It does
not prove that locally accepted governance is newest. It does not yet freeze
the handshake, full descriptor, receipt-quorum, or historical-set proof bytes.
It does not make endpoint authentication a substitute for member, governance,
receipt-quorum, stream-checkpoint, governance-checkpoint, or content-signature
validation.

---

## References

- #134, *Proposal: iroh-room v2 architecture for large communities*, especially
  §§6.3, 10.3, 25 #2, and 26.
- ADR-0004, *Accept the v2 Large-Community Architecture*.
- ADR-0009, *Separate Replica Signing Keys from Iroh Endpoint Keys*.
- `specs/v2-identifiers-domain-separation.md` (the pre-#157 candidate
  `ReplicaId` assumption and its correction).
- `specs/v2-governance-snapshot-transition-proof.md` (authenticated replica
  descriptor state at a checkpoint).
- Iroh 1.0.1 `EndpointId`/`SecretKey` and authenticated connection identity as
  pinned by the workspace dependency.
