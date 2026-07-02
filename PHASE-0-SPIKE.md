# Iroh Rooms — Phase 0 Spike

**Status:** Phase 0 design + spike plan (pre-MVP). **Date:** 2026-06-26.
**Scope:** ≤5-person private rooms, single device per identity, no key rotation.
**Inputs:** PRD v0.3 (`/Users/sekou/TAC/iroh-room/PRD.v0.3.md`, current) and PRD v0.2 (`/Users/sekou/TAC/iroh-room/PRD.md`).

## Purpose

This document exists to **de-risk the one genuinely hard plane — the Room Event Plane — before committing to the MVP.** The Blob Plane (iroh-blobs / sendme-style serving) and the Live Pipe Plane (dumbpipe-style TCP-over-iroh) are near-free wins on top of a working event log; they get confirmation, not risk budget. The hard plane is the signed append-only event log plus membership/authorization-as-state plus "recent history sync." Everything else rides on it. This spike proves, in order of decreasing risk, that (1) the iroh substrate actually connects two peers across real NATs, (2) the signed event log is byte-for-byte correct and arrival-order-independent, and (3) the two architectural forks are resolved on measured evidence.

## TL;DR — the two decisions

- **ADR-1 — Event transport: adopt full-mesh direct QUIC over a custom ALPN; reject iroh-gossip for the load-bearing log.** At n≤5 a full mesh connects every pair directly, gives ordered reliable streams for free, and — decisively — lets a node reject an unknown remote `EndpointId` at `accept()` before any byte flows. Gossip's only real value (epidemic fan-out + partial-view membership) is zero at this size, and its open, unauthenticated topic actively fights the private-room model. Gossip is parked as an *optional best-effort liveness/notify signal* (presence, "new event available," admin-tip advertisement), never the system of record.
- **ADR-2 — Event log + sync: hand-roll a signed append-only log in SQLite with a bounded recent-sync pull protocol over the ADR-1 mesh; do NOT adopt iroh-docs for the MVP.** The one thing iroh-docs uniquely buys — efficient reconciliation of *fully divergent* histories — is exactly what the MVP defers (PRD §10.7/§15.5). Its data model (LWW key-value, wall-clock-µs tiebreak, no causal parents) mismatches the PRD's `prev_events` causal DAG, and it drags three pre-1.0 crates (docs, gossip, blobs) plus redb onto the critical path. iroh-docs is parked as the **leading candidate for the Phase-5 availability layer**, where real offline catch-up makes its reconciliation primitive worth the baggage.

Both decisions were adversarially verified against the actual mid-2026 stack (iroh 1.0.0; iroh-docs/iroh-gossip 0.101.0; iroh-blobs 0.103.0) and **hold**. The convergence model was red-teamed and **survives**, with a set of blocking pre-MVP fixes that are **applied inline** in the Membership & Ordering Model below (not appended as critique).

---

# Decision Records

## ADR-1: Event Transport — iroh-gossip Swarm vs. Full-Mesh Direct QUIC

- **Status:** **Adopted — ratified by measurement (IR-0006, #10).** Confirmed
  by adversarial verification at design time and, since, by the Day-4
  measured comparison below.

> **COMPLETE (IR-0006):** D1 measured — **ADR-1 ratified**: full-mesh direct
> QUIC remains the Room Event Plane transport; `iroh-gossip` is not adopted
> for the load-bearing log (parked as an optional off-critical-path
> liveness/admin-tip carrier, exactly as already specified below). All five
> ADR-1 comparison dimensions (propagation latency, reconnect behavior,
> late-join history gap, auth/admission model, implementation complexity)
> were measured at N=2..5 on deterministic loopback and confirm the
> recommendation; none were disproven. Findings and the measured comparison
> table are in `crates/spike-transport/NOTES.md`.

- **Scope:** Room Event Plane transport for ≤5-person private rooms (PRD v0.3 §9.1, §17.1.13).
- **Related:** ADR-2 (event log + sync).

### Context

The Room Event Plane is an append-only signed event log (PRD §9.1). Every plane rides on it: messages, membership, `file.shared`, `pipe.opened/closed`, `agent.status`. It is the one genuinely hard plane; the Blob and Live Pipe planes are near-free wins on top of it.

We must pick the wire that moves events between peers. Two candidates exist in the mid-2026 iroh stack:

1. **iroh-gossip 0.101** — HyParView partial-view membership + PlumTree epidemic broadcast over a 32-byte `TopicId`.
2. **Full-mesh direct QUIC** — `iroh::protocol::Router` with a custom ALPN (e.g. `/iroh-rooms/event/1`), each peer dialing every other peer's `EndpointId` and framing events over bidirectional QUIC streams.

Hard constraints from the ground truth:

- **iroh core (1.0.0) and iroh-base (1.0.x) are the only API-stable, wire-stable pieces.** Everything higher (gossip 0.101, docs 0.101, blobs 0.103) is 0.x: pinned to `iroh ^1` but making *no* API-stability promise, on a roughly monthly breaking cadence.
- Rooms are explicitly small: PRD §17.1.13 success metric is "a room with 5 participants remains usable." There is no large-swarm requirement anywhere in scope.
- The product is **private by default** (PRD §13): signed events, membership checks, no central server, agents not implicitly trusted.

### Options

#### Option A — iroh-gossip swarm

A single `Gossip` instance per node; `gossip.subscribe(TopicId, bootstrap_peers)` yields a `GossipTopic` split into `GossipSender`/`GossipReceiver`. Events arrive as `Event::Received { content, delivered_from }`; `Event::Lagged` signals dropped messages.

- **Admission / authorization:** *Weak by construction.* Anyone who learns the 32-byte `TopicId` can join the topic and publish; gossip has **no built-in authentication, no membership, no topic authorization**. The transport itself does not tell you who a peer is, and there is no per-peer connection state with which to even detect an interloper. Admission control and signed envelopes are 100% the application's job — exactly the "membership-as-state" problem the PRD already flags. The transport actively *fights* the private-room model.
- **Reliability:** Best-effort eventual delivery only. `Event::Lagged` means the local consumer fell behind and messages were silently dropped — treat it as a "resync needed" trigger.
- **Ordering:** None. No total order, no causal order. PlumTree dedups by message hash but imposes no sequence.
- **History:** None. A late joiner receives nothing sent before it joined — gossip cannot satisfy PRD §15.5 "recent history sync" by itself.
- **Build cost:** The `subscribe`/`broadcast` API is trivially small, but because gossip supplies *none* of ordering, history, or auth, you re-implement all three regardless. The cheap API is a false economy.
- **Churn exposure:** Puts a 0.x crate (gossip 0.101) on the load-bearing critical path, with breaking minor bumps.
- **Where it shines:** Epidemic fan-out and HyParView partial-view membership for *large* swarms (dozens to thousands of peers). None of that is in scope here.

#### Option B — Full-mesh direct QUIC (custom ALPN)

`Router::builder(endpoint).accept(b"/iroh-rooms/event/1", handler).spawn()`; a `ProtocolHandler::accept(&self, conn: Connection)` handles each inbound link. Each node dials every other member's `EndpointId` directly (≤4 outbound dials at 5 members) and frames events over `open_bi`/`accept_bi` reliable, ordered byte streams.

- **Admission / authorization:** *Strong and native.* iroh's TLS client-auth makes `Connection::remote_endpoint_id` a cryptographically proven identity. You reject a non-member at connect time, before any event byte flows — admission control is a property of the transport, not a bolt-on. This is the same identity used for blob ACLs and event-signing keys (the `device_id` of the Event Protocol), so room membership unifies across all three planes.
- **Reliability:** Each link is a reliable, ordered QUIC stream with NAT hole-punching and relay fallback (PRD §18.1). At ≤5 members every pair is directly connected, so there is no multi-hop best-effort layer to drop messages.
- **Ordering:** Per-link total order for free. (App-level causal/total ordering across the room is still your responsibility — see ADR-2 — but you start from ordered reliable links rather than an unordered firehose.)
- **History:** The same connection trivially carries a "send me events since X" request, so recent-history backfill (PRD §15.5) is just another framed message on the existing link — no separate mechanism.
- **Build cost:** You write the dial fan-out, a small per-peer connection manager, and a length-prefixed frame codec. Modest and well-trodden, and you get ordered reliable streams plus explicit authenticated membership for it.
- **Churn exposure:** **Only depends on iroh 1.0 stable core** (`Router`, `ProtocolHandler`, `Connection`, `EndpointId`) — the one part of the stack with a wire- and API-stability guarantee. Zero 0.x crates on the event critical path.
- **Cost:** O(n²) connections. At n=5 that is 10 links — trivial. It does not scale to large rooms (the explicit trade).

### Decision

**Adopt Option B: full-mesh direct QUIC over a custom ALPN as the Room Event Plane transport.**

Rationale, in priority order:

1. **The transport must enforce the product's core invariant — private, authenticated admission — and only mesh does so natively.** Gossip's open-topic model is the opposite of a private room.
2. **For ≤5 peers, gossip's only real value (epidemic fan-out + partial-view membership) is zero**, because a full mesh already connects everyone to everyone. We would pay gossip's costs (no ordering, no history, no auth, a 0.x dep) for benefits that do not apply.
3. **It minimizes 0.x churn on the load-bearing path.** "Minimize the number of pre-1.0 crates on the critical path" is a legitimate tie-breaker, and here it points the same way as admission control and reliability.
4. **It makes recent-history sync cheaper** (ADR-2): a backfill request is just a frame on a connection we already hold open and have already authenticated.

Gossip is **not** discarded forever. It remains an optional, *best-effort liveness/notify signal* in a later phase — and, per the convergence hardening below, is the natural carrier for **admin-tip / admin-sequence advertisement** so peers can detect an incomplete membership view (see Membership & Ordering §0, §7). When used for that, it sits off the critical path where its eventual-consistency semantics are acceptable.

### Confidentiality mechanism — precise statement (corrected)

It is **not** the case that "mesh keeps plaintext within the member set while gossip forwards plaintext through intermediaries." *Both* transports are end-to-end encrypted: every iroh hop (including every gossip neighbor link and every mesh relay-fallback path) is a QUIC/TLS connection between two authenticated endpoints, and iroh relay servers forward only encrypted packets they cannot read. The accurate objections to gossip for a private room are:

- **(a) Admission control.** The topic is open — anyone who learns the `TopicId` can join the swarm and *legitimately* receive plaintext, with no per-peer connection state to detect them.
- **(b) Forwarding visibility.** A relaying swarm member can read all traffic it forwards. That is fine *iff* the swarm equals the member set, but gossip gives you no guarantee or control of that membership.

Mesh fixes both by making `remote_endpoint_id` a cryptographically proven identity you authorize at `accept()`. The correct one-liner is: **"mesh enforces authenticated admission and per-peer visibility; gossip's open, unauthenticated topic and content-forwarding relays do not."**

### Consequences

- **Implementation:** Build a per-room peer manager that, on join, dials each member `EndpointId`, maintains one bidi stream per peer, frames events with a length prefix, and surfaces connect/disconnect for the CLI's connection-state output (PRD §16.3, §18.1). Register the `/iroh-rooms/event/1` ALPN on the shared `Router` alongside the blob and pipe ALPNs (one `Endpoint`, multiple `.accept()` chains).
- **Membership gate is real and early:** reject unknown `remote_endpoint_id` at `accept`, log the rejection locally (supports PRD §13.2 audit and §16.3 "distinguish unauthorized peer"). Because membership is tracked by *identity* key (`sender_id`) and the connection proves the *device* key (`device_id` = `EndpointId`), the gate resolves `device_id → bound sender_id → Active?` via the validated device binding (Event Protocol §1).
- **Bootstrap/discovery:** dial purely by `EndpointId` using default n0 DNS discovery + mDNS for LAN; the invite ticket carries the addressing hints (PRD §10.5). DHT discovery is not required.
- **We own ordering and history.** The transport gives ordered *per-link* delivery; room-wide ordering/dedup/backfill is the event-log layer's job (ADR-2). This is unavoidable with either option.
- **The mesh pull RPC must support a dedicated "membership sub-DAG" request** that is *never windowed* (see Membership & Ordering §0, §4), in addition to the windowed chat backfill, plus an **admin-tip exchange** ("what is your highest admin-chain event_id / admin_seq?").
- **Scaling ceiling is accepted:** O(n²) links is fine to ~10–20 members and degrades beyond that.

### What Would Flip This Decision

- Room size grows past ~10–20 members, or a large/public-room requirement appears.
- ADR-2 chooses iroh-docs (which pulls gossip in transitively and already carries the live event tail) — the transport question would then partly collapse into the docs decision.
- A high-fan-out, low-trust event stream becomes first-class (broadcast presence/telemetry to many semi-trusted observers) where best-effort epidemic delivery is acceptable.
- iroh-gossip reaches 1.0 with authenticated/authorized topics, removing both the churn and the open-topic objections.

---

## ADR-2: Event Log + Sync — Build on iroh-docs vs. Hand-Roll a Signed Event Log

- **Status:** Proposed (decision recommended) — **confirmed by adversarial verification against source.**
- **Scope:** Room Event Plane log semantics, persistence, and "recent history sync" (PRD v0.3 §9.1, §10.1–10.7, §11.1, §12, §15.5).
- **Related:** ADR-1 (event transport).

### Context

The Room Event Plane is "an append-only signed event log" (PRD §9.1) with a defined envelope: `event_id` (BLAKE3-derived), `room_id`, `sender_id`, `device_id`, `event_type`, `created_at`, **`prev_events` (a causal-parent DAG)**, `content`, and an Ed25519 `signature` (PRD §11.1). Membership and authorization events live in the *same* log — so membership is itself state derived from the log.

Two scope facts bound this decision:

- **PRD §15.5 / §7.3.14 / §8.3:** the MVP needs only **bounded recent-history sync** ("bounded by count and/or time window", §10.7). **Full decentralized history reconciliation is explicitly out of scope** and may even be cut. "Deep conflict resolution is deferred."
- **PRD §12:** local storage is **SQLite**, with named tables (`events`, `members`, `sync_state`, …) and append-only semantics.

The genuinely hard distributed-systems primitive — efficiently reconciling two divergent histories on reconnect — is exactly what iroh-docs hands you off the shelf. So the question is sharp: do we buy that primitive (and its baggage), or hand-roll, given that the MVP has deliberately *deferred the hard version of the problem*?

#### Option A — Build on iroh-docs 0.101

A namespace = a room. `doc.set_bytes(author, key, value)` writes an entry `(namespace, key, author) -> {BLAKE3 hash, size, timestamp}`; `doc.subscribe()` yields `LiveEvent::InsertRemote` for the live tail; reconnect triggers range-based set reconciliation (Meyer 2022 recursive fingerprint partitioning).

- **Sync (the prize):** Best-in-class. **Verified against source** (`ranger.rs`): it is genuine Meyer range-based set reconciliation — but it reconciles the **entire replica set**; there is **no native bounded/recent-only or time-windowed mode** (only validate/content-status callbacks and split tuning, which control message granularity, not scope). So docs does **not** give "bounded recent-history sync" off the shelf — it gives **full whole-history convergence**, which is *precisely the deferred full-reconciliation primitive* (PRD §10.7/§15.5 want bounded recent). This makes the rejection *stronger*: docs' unique value is exactly the capability the MVP defers.
- **Signing / multi-writer:** Entries are author-signed; multi-writer is native.
- **Persistence:** redb-backed fs store + rich query API.
- **Data-model mismatch (the catch):** Docs is a **last-write-wins key-value CRDT, not a causal DAG.** There is no `prev_events`, no causal-parent concept, no state resolution; it collapses to latest-per-key on read. To use it as an append-only event *set*, every event must get a globally-unique key so LWW never silently overwrites two distinct events. You get **set-convergence, not causal ordering** — and the PRD envelope is explicitly causal.
- **Prefix-deletion footgun (verified, `ranger.rs`):** docs implements **Willow prefix-based deletion** — an entry whose key is a prefix of another, with a not-greater timestamp, is *silently deleted*. Event keys must therefore be **fixed-length and prefix-free** (use the 32-byte `event_id` digest, **not** `author/seq` strings where `author/1` prefixes `author/12`), or distinct events can be dropped.
- **Timestamp / tiebreak (RESOLVED from source — earlier suspicion confirmed):** `src/sync.rs` `system_time_now()` is `SystemTime::now().duration_since(UNIX_EPOCH).as_micros() as u64` — a **wall-clock microsecond** timestamp, clock-skew-vulnerable, **NOT a Lamport/logical clock**. `validate_entry` rejects entries >10 min ahead of local wall clock (`MAX_TIMESTAMP_FUTURE_SHIFT`). LWW tiebreak (per the Willow spec in `ranger.rs`): higher timestamp wins, ties broken by greater BLAKE3 hash. With unique-key-per-event there are no same-key collisions, so neither the timestamp tiebreak nor prefix-deletion fires — confirming that adopting docs means deliberately **not** using its conflict semantics (it becomes a churny signed-blob set). **This item is now answered; it no longer needs re-verification later.**
- **Authorization (corrected precision):** The `NamespaceSecret` is an **all-or-nothing whole-document write capability**. Each entry is *author-signed*, so a namespace-secret holder writes under *their own* author key and **cannot forge another author's entries** — the real limitation is that there are **no per-member roles and no revocation**; removing a writer requires rotating the namespace. The membership/authorization-as-state layer is ours either way; docs removes none of the auth work the PRD requires.
- **Churn / footprint:** Docs is a meta-protocol that drags in **three 0.x crates** — gossip 0.101, blobs 0.103, redb — all pre-1.0, on a monthly breaking cadence, versions not in lockstep (blobs 0.103 vs gossip/docs 0.101). It introduces **redb as a second persistence engine alongside the PRD's SQLite**. (Note: these crates are actively maintained, *not* deprecated/sunset — they are simply separate pre-1.0 projects; `iroh-docs` is on record as "not yet ready for a 1.0.")

#### Option B — Hand-roll a signed event log + bounded recent-sync

Store events in SQLite (PRD §12). `event_id = blake3(canonical_bytes)`; sign canonical bytes with the device key (Event Protocol §6); validate signature + membership on ingest; dedup by `event_id`; keep `prev_events` as the causal DAG. Sync = a small pull protocol over the ADR-1 mesh: "give me events after watermark / events whose IDs I lack," bounded by count/time window for chat — but the **membership sub-DAG is never windowed** (§0/§4).

- **Data-model fit:** *Exact.* The PRD envelope (BLAKE3 id, `prev_events` DAG, membership/auth events in-log, device-key signatures) is expressed natively. No impedance mismatch.
- **Sync:** For the MVP-scoped problem — recent window, ≤5 peers, all directly connected via ADR-1 — a naive "set difference of recent event IDs" pull is tractable and testable. We do **not** need Meyer range reconciliation for the deferred full-reconciliation case.
- **Authorization:** Fully under our control, validated against the same `EndpointId` identity used by the mesh and blob ACLs (identity unification).
- **Ordering:** We define it (causal via `prev_events`, deterministic tiebreak we choose — no dependence on an unverified, wall-clock docs timestamp).
- **Persistence:** SQLite only, exactly as PRD §12 specifies.
- **Churn exposure:** **Only iroh 1.0 stable core** on the critical path. Zero 0.x crates for the log.
- **Build cost (the catch):** We own canonical serialization, the signed-envelope codec, dedup/validation, the DAG bookkeeping, and the pull protocol. The genuinely hard part — efficient reconciliation of large divergent histories — we **do not build for MVP** (out of scope) but also do not get for free if it later comes into scope.
- **Canonical-serialization sub-decision:** sign over deterministic bytes. The Event Protocol uses **deterministic CBOR (RFC 8949 §4.2.1)** over the eight signed fields — chosen over postcard because `content` is a heterogeneous per-type map and CBOR's deterministic map-key ordering is fully specified and language-neutral (important for non-Rust agents) and COSE-compatible. See Event Protocol §3.

### Decision

**Adopt Option B: hand-roll a signed append-only event log in SQLite, with a bounded recent-sync pull protocol over the ADR-1 mesh. Do not adopt iroh-docs for the MVP.**

Rationale, in priority order:

1. **The one thing iroh-docs uniquely buys — efficient reconciliation of divergent histories — is precisely the thing the MVP defers** (PRD §10.7, §15.5, §8.3), and it only offers it as *whole-history* reconciliation, not the bounded recent sync the MVP wants.
2. **The data models mismatch.** Docs is LWW-KV set-convergence; the PRD log is a `prev_events` causal DAG with membership-as-state. Using docs forces us to neutralize its own semantics (unique fixed-length prefix-free keys, ignore its timestamp tiebreak), at which point it is a churny signed-blob set.
3. **Authorization/membership is hand-built either way** (docs' write capability is namespace-coarse with no roles/revocation).
4. **Churn and footprint.** Docs puts gossip + blobs + redb (all 0.x) on the load-bearing path and adds redb alongside SQLite.
5. **It composes cleanly with ADR-1:** backfill is a frame on an already-authenticated mesh link, not a second sync subsystem.

This is a *deliberately scoped* hand-roll: we build the easy, MVP-sized sync and **avoid** reimplementing Meyer range reconciliation. iroh-docs is parked as the **leading candidate for the Phase-5 availability layer** (PRD §19 Phase 5 / §14), where real offline catch-up and divergent-history reconciliation become first-class, and where it will (hopefully) be closer to 1.0.

### Consequences

- **Schema work up front (PRD §10):** lock down event-ID derivation, canonical serialization, the exact signature payload, and membership-validation rules (all in the Event Protocol below). Produce protocol test vectors before broad implementation — ambiguity here is the §18.6 protocol-ambiguity risk.
- **SQLite is the single source of truth** (`events`, `members`, `sync_state` per PRD §12). Ingest path = verify signature → check membership/authorization → dedup by `event_id` → persist → fan out live to mesh peers.
- **Recent-sync protocol** is a bounded pull for chat, an unbounded pull for the membership sub-DAG, plus admin-tip exchange.
- **We carry the ordering/tiebreak spec ourselves** — a clear win over inheriting docs' unverified wall-clock tiebreak.
- **Blobs stay independent:** the Blob Plane uses iroh-blobs 0.103 directly; the log only references blob hashes (PRD §9.2). Hand-rolling the log does *not* pull blobs or docs into the log's critical path.
- **If docs is ever adopted (Phase 5):** store membership/authorization events in a **dedicated, always-fully-reconciled namespace** (key = `"m/" + event_id`) separate from windowed chat (key = `"c/" + event_id`); all keys are the **fixed-length 32-byte `event_id`** (prefix-free) to avoid Willow prefix-deletion. The "never-windowed membership" invariant maps cleanly to "fully reconcile namespace `m/`."
- **Accepted debt:** if full reconciliation later becomes near-term, we either build Meyer range reconciliation ourselves or migrate the log onto iroh-docs (or p2panda). Mitigate by keeping the event representation transport-agnostic (signed canonical bytes addressable by hash) so it can be re-homed without re-signing.
- **The previously-parked "verify docs timestamp semantics" spike task is DONE** (wall-clock µs, confirmed from `src/sync.rs`). The spike need only re-confirm if the docs version is bumped.

### What Would Flip This Decision

- Full / robust decentralized history reconciliation moves into MVP or early-post-MVP scope.
- The room log can honestly be reduced to a set of signed events keyed by hash with set-convergence (not causal) semantics — the data-model objection evaporates and docs becomes a strong fit.
- ADR-1 adopts gossip, or the Blob Plane already forces gossip/redb into the tree — the transitive 0.x cost of docs is largely already paid.
- iroh-docs reaches 1.0 (or adds per-member authorization + verified causal/Lamport ordering).
- SQLite proves inadequate and we want an embedded KV anyway — redb-via-docs stops being a *second* engine.
- We decide to adopt **p2panda** for causal logs + convergent access-control — a different third-party answer that would also displace the hand-roll, and the realistic alternative if causal-DAG + access-control-CRDT semantics become hard requirements.

---

# Event Protocol

> `docs/protocol.md` is the condensed implementer view of this section and Membership &
> Ordering below — read it first if you just need to build or audit a conforming peer; this
> document stays the normative source for byte-level values and the decision rationale.

This section defines the wire- and signature-level contract for every event in the Room Event Plane. It is normative: an event that does not satisfy these rules MUST be rejected. All multi-byte integers in derivations are big-endian unless stated otherwise. The golden test vector in Protocol Test Vectors §1 is authoritative for byte-level conformance (independently reproduced during this spike).

## 1. Identity and Key Model

Three keys participate in every event. All are Ed25519 (RFC 8032; `ed25519-dalek 3.0.0-rc.0` as pinned by `iroh 1.0`). A public key is exactly 32 bytes; a signature is exactly 64 bytes.

| Concept | Field | Type | Definition |
|---|---|---|---|
| Participant identity | `sender_id` | Ed25519 public key (32 B) | The stable principal. One per human/agent. Survives device changes. **Authorization and membership are tracked against this key.** Never signs events directly in MVP. |
| Device key | `device_id` | Ed25519 public key (32 B) | The per-device key that actually signs events. **`device_id` is byte-for-byte the iroh `EndpointId`** (`iroh_base::EndpointId == PublicKey`). The same secret is the iroh `Endpoint` secret, the event-signing key, and the blob-ACL/pipe-ACL node identity. |
| Room key | `room_id` | 32 B BLAKE3 digest | Cryptographic room identifier; derivation in §5. |

`sender_id` and `device_id` are **distinct keys even in MVP** (the golden vector has different values for each). MVP allows exactly **one `device_id` per `sender_id`** (multi-device out of scope); the structure generalizes to a set later.

**Binding `device_id` to `sender_id` (device certificate).** Because events are signed by `device_id` but authorized against `sender_id`, every device must be attested by its identity key. The attestation is a detached signature carried in `room.created`, `member.joined`, and `member.removed` content (field `device_binding`, §7):

```
BIND_CONTEXT = "iroh-rooms:device-binding:v1"      (ASCII, 27 bytes, no NUL)
binding_msg  = BIND_CONTEXT ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)
binding_sig  = Ed25519_sign(identity_secret, binding_msg)          // 64 B
```

A verifier accepts a device as belonging to a participant iff `Ed25519_verify(sender_id, binding_msg, binding_sig)` succeeds. Membership state stores the resulting map `sender_id → {device_id, binding_sig}`. Agents are ordinary participants: an agent has its own `sender_id`/`device_id` pair and the same binding rule applies (role distinguishes it, §7).

## 2. The Signed Event (logical fields)

These eight fields, and only these, are covered by the signature and the `event_id` hash. `event_id` and `signature` are deliberately **not** signed-over (an event cannot commit to its own hash, and the signature is an attestation over the hash preimage). **There is no `lamport` field on the wire** — the Lamport clock is a *derived* ordering quantity (Membership & Ordering §2), recomputed by every peer from the signed `prev_events`, so it is a pure function of signed data and there is nothing to lie about. The golden vector is `map(8)`.

| Field | CBOR type | Notes |
|---|---|---|
| `schema_version` | uint | MUST be `1`. Unknown versions rejected. Pins hash to BLAKE3-256, signature to Ed25519. |
| `room_id` | bstr[32] | The room this event belongs to. Binds the event to one room (replay into another room fails because `room_id` is inside the signed bytes). |
| `sender_id` | bstr[32] | Participant identity public key. |
| `device_id` | bstr[32] | Signing device public key (= iroh `EndpointId`). The signature MUST verify under this key. |
| `event_type` | tstr | Registered type from §7. Unknown types rejected (no critical-unknown passthrough in MVP). |
| `created_at` | uint | Milliseconds since Unix epoch (UTC). Wall-clock; **advisory/display only — never trusted for causal ordering, authorization, or to mutate the validated set** (use `prev_events`). |
| `prev_events` | array of bstr[32] | Causal parents: BLAKE3-256 digests of the events this one acknowledges (local DAG heads at creation). `[]` **only** for `room.created`; every other event MUST be non-empty and transitively reach genesis (§6 step 9a). Max 20 entries. |
| `content` | map | Event-type-specific payload; schemas in §7. |

`sender_id`, `device_id`, `room_id`, and each `prev_events` entry are raw 32-byte CBOR byte strings on the wire; their human/CLI/JSON presentation is lowercase hex (64 chars), and for hash-typed IDs the **named** form `blake3:<64-hex>` (§4).

## 3. Canonical Serialization

The signed-over bytes are produced with **deterministic CBOR (RFC 8949 §4.2.1 Core Deterministic Encoding Requirements).** Reference encoder: `ciborium` with deterministic map ordering, or `cbor2.dumps(obj, canonical=True)` (the latter was used to generate and this spike re-verified the golden vector).

**Deterministic profile (all MUST hold; a non-conforming encoding is rejected):**

1. Map keys sorted in bytewise lexicographic order of their *encoded* form (for these short text keys, equivalently length-first then bytewise).
2. Shortest-form integer encoding.
3. Definite-length items only — no indefinite-length strings, arrays, or maps.
4. No duplicate map keys.
5. No CBOR tags. No floating-point values anywhere (use integers; e.g. progress is `uint` percent).
6. Text strings are valid UTF-8.
7. The top-level signed object contains **exactly** the eight keys of §2 — no extra keys (forward-compatible fields arrive only by bumping `schema_version`).

Applying the profile, the canonical top-level key order is fixed and implementations MAY hardcode it for assertions:

```
content, room_id, device_id, sender_id, created_at, event_type, prev_events, schema_version
```

Call the resulting octet string **CSB** (canonical signed bytes).

**Wire / storage form (`WireEvent`).** To eliminate re-serialization ambiguity, CSB is transported and persisted as an opaque byte string inside an outer deterministic-CBOR map:

```
WireEvent = {
  "v":      1,          // uint, transport envelope version
  "signed": bstr,       // == CSB, verbatim
  "sig":    bstr[64],   // Ed25519 signature, §6
  "id":     tstr        // "blake3:<hex>", advisory cache key, §4
}
```

A receiver hashes and verifies **the exact `signed` bytes** — it never re-encodes to verify. It then CBOR-decodes `signed` for semantic checks, additionally re-canonicalizes the decoded object and rejects if it does not byte-equal `signed`. `"id"` is a convenience index only and MUST be recomputed and checked (§4), never trusted.

## 4. Event ID Derivation

The `event_id` is a **named content hash** of CSB — never a sender-chosen ULID/UUID/random value, so identity is verifiable and self-certifying.

```
digest   = BLAKE3-256(CSB)                       // 32 bytes
event_id = "blake3:" ‖ lowercase_hex(digest)     // e.g. "blake3:c389e2…85a1"
```

`prev_events` entries are the raw 32-byte `digest`; their `blake3:`-prefixed string form is presentation-only. Two `WireEvent`s with equal `event_id` are the same event; the receiver keeps the first validly-signed copy and ignores duplicates.

## 5. Room ID Derivation

`room_id` is derived once by the creator and recomputable by every peer from `room.created`:

```
ROOMID_CONTEXT = "iroh-rooms:room-id:v1"          (ASCII)
room_id = BLAKE3-256( ROOMID_CONTEXT
                      ‖ creator_sender_id(32)
                      ‖ room_nonce(16)             // from room.created content
                      ‖ created_at_be(8) )         // room.created created_at, big-endian u64
```

On receiving `room.created`, a peer MUST recompute `room_id` and reject the event if it does not equal the envelope `room_id`. This binds the room identity to its creator and a nonce without any central authority. (Verified this spike: creator seed `0x01×32`, nonce `000102…0e0f`, `created_at=1750000000000` ⇒ `43c19f2e…16a3`.)

## 6. Signature Payload and Verification

Signing is domain-separated to prevent cross-protocol signature reuse:

```
EVENT_CONTEXT = "iroh-rooms:event:v1"             (ASCII, 19 bytes, no NUL)
sig_msg   = EVENT_CONTEXT ‖ CSB
signature = Ed25519_sign(device_secret, sig_msg)  // 64 B; key = device_id
```

**Verification algorithm (in order; first failure rejects the event and is logged):**

1. **Decode transport.** Parse `WireEvent`. Reject if `v != 1`, required keys missing, or outer map non-canonical.
2. **Recompute id.** `id' = "blake3:" + hex(BLAKE3-256(signed))`. Reject if `id' != id`.
3. **Verify signature.** Decode `signed` to read `device_id`. Reject unless `Ed25519_verify(device_id, EVENT_CONTEXT ‖ signed, sig)` succeeds.
4. **Enforce canonicality.** Reject unless `canonical_cbor(decode(signed)) == signed`, and unless `signed` decodes to exactly the eight §2 keys with correct CBOR types.
5. **Version/type.** Reject unless `schema_version == 1` and `event_type` is in the §7 registry; validate `content` strictly (unknown content keys → reject).
6. **Room binding.** For `room.created`, recompute and check `room_id` (§5). For all other types, reject unless `room_id` matches the room being processed.
7. **Device binding.** Resolve `sender_id`'s authorized device from validated membership state; reject unless `device_id` is bound to `sender_id` with a `binding_sig` that verifies (§1). For `room.created`/`member.joined`/`member.removed`, the binding is taken from this event's `content.device_binding`.
8. **Membership & role.** Reject unless `sender_id` is a current member of `room_id` with a role permitting `event_type` (per-type rules, §7 + Membership & Ordering §3.5). `room.created` is exempt. **Note:** authorization for non-membership events is judged against the event's **own fixed causal ancestors** (Membership & Ordering §3.4–§3.5), not the receiver's live snapshot — this is what makes the verdict arrival-order-independent.
9. **Causal structure.** Reject if `prev_events` length > 20. **(9a, genesis-descent — explicit structural invariant)** Reject any non-genesis event whose `prev_events` is empty, and reject any non-genesis event whose transitive ancestors do not include the room's genesis `room.created`. `room.created` MUST have `prev_events == []`. Each parent SHOULD already be present; an event whose parents are merely *unknown* (not provably non-genesis-descending) is **buffered pending sync** rather than rejected (Membership & Ordering §4), subject to the anti-amplification bounds there.
10. **Clock sanity (ADVISORY ONLY).** If `created_at` is more than 300000 ms in the future relative to local time, **flag/alert** (reason `clock_skew`) but **do NOT reject, drop, reorder, or exclude the event from the validated set.** The clock check MUST NOT influence the validated set, ordering, or any authorization/expiry verdict — those depend only on signed data so that all honest peers converge. (This is the corrected behavior; an earlier draft's hard reject would have made log validity depend on each peer's local clock and caused permanent divergence.)
11. **Dedup & persist.** If `event_id` already stored, ignore (`duplicate`, not an error). Otherwise persist the verbatim `WireEvent`.

## 7. MVP Event-Type Registry

Notation: `bstr[n]` = byte string of length n; `tstr` = UTF-8 text; `uint` = unsigned int; `opt` = optional. `DeviceBinding = { "identity_key": bstr[32], "device_key": bstr[32], "sig": bstr[64] }` (§1). `HashRef`/`EventId` in content = bstr[32]. All `content` maps use the §3 deterministic-CBOR profile.

| `event_type` | Signer / role required | `prev_events` |
|---|---|---|
| `room.created` | Creator's device. No prior membership; creator becomes sole immutable admin. | `[]` (genesis, unique per room) |
| `member.invited` | The **admin**'s device. | room heads |
| `member.joined` | The **joining** member's device; capability-gated. | room heads (must include / descend from the referenced invite) |
| `member.left` | The member only (**voluntary self-leave**). | room heads |
| `member.removed` | The **admin**'s device (involuntary removal / kick). | room heads |
| `message.text` | Any current member. | room heads |
| `file.shared` | Any current member. | room heads |
| `pipe.opened` | Any current member (`owner_id == sender_id`). | room heads |
| `pipe.closed` | The pipe owner or the admin. | room heads |
| `agent.status` | Any current member (typically `role == "agent"`). | room heads |

> **Removal modeling (reconciled).** Voluntary departure and admin removal are **distinct event types** (`member.left` vs `member.removed`), not one type with a `reason` discriminator. This keeps the authorization gate trivial — `member.removed` ⇒ signer must be admin; `member.left` ⇒ signer must be self — and lets the convergence rules speak about them precisely.

**`room.created`** — establishes room identity, admin, and creator device.
```
content = {
  "room_name":      tstr,
  "room_nonce":     bstr[16],          // feeds room_id derivation (§5)
  "admins":         [ bstr[32], ... ], // initial admin identities; MUST be exactly [sender_id] in MVP (single immutable admin)
  "device_binding": DeviceBinding      // identity_key MUST == sender_id, device_key == device_id
}
```
Validate: `prev_events == []`; `room_id` recomputes (§5); `sender_id ∈ admins`; binding verifies.

**`member.invited`** — records an admin-issued, capability-bound invite (the secret travels out-of-band inside the iroh ticket, never on the log).
```
content = {
  "invite_id":       bstr[16],
  "capability_hash": bstr[32],          // = BLAKE3-256("iroh-rooms:invite:v1" ‖ room_id ‖ invite_id ‖ secret)
  "role":            tstr,              // "member" | "agent" | "admin"
  "invitee_key":     bstr[32],          // REQUIRED in MVP — the identity key this invite authorizes (key-bound, path-A). See Membership & Ordering §6 for why open/bearer invites are not removal-safe.
  "expires_at":  opt uint,             // ms epoch; absent ⇒ no expiry
  "invitee_hint":opt tstr              // human label only, non-authoritative
}
```
Validate: sender is admin; `role` in enum.

**`member.joined`** — proves possession of the invite capability and binds the joiner's device.
```
content = {
  "via_invite_id":     bstr[16],        // references a member.invited.invite_id
  "capability_secret": bstr[16],        // recompute capability_hash and match the invite
  "role":              tstr,            // MUST equal the invite's role
  "device_binding":    DeviceBinding,   // identity_key == sender_id, device_key == device_id
  "display_name":  opt tstr
}
```
Validate: referenced invite exists, unexpired, and its `invitee_key == sender_id`; `BLAKE3-256("iroh-rooms:invite:v1" ‖ room_id ‖ via_invite_id ‖ capability_secret) == invite.capability_hash`; `role == invite.role`; binding verifies; the authorizing invite is still live in this event's ancestor view (not consumed by a prior `member.left`/`member.removed` of this subject — Membership & Ordering §3.5/§3.7); `sender_id` not already joined.

**`member.left`** — voluntary departure only.
```
content = {
  "member_id":  bstr[32],               // departing identity; MUST == sender_id
  "reason":     opt tstr                // free-form, display only
}
```
Validate: `member_id == sender_id`. Consumes all prior admin authorizations for `member_id` (Membership & Ordering §3.7).

**`member.removed`** — admin-initiated removal / kick.
```
content = {
  "member_id":   bstr[32],              // removed identity; MUST != admin
  "removed_by":  bstr[32],              // admin identity; MUST == sender_id (the admin)
  "reason":  opt tstr
}
```
Validate: `sender_id` is the admin; `removed_by == sender_id`; `member_id != sender_id`. After acceptance, `member_id` loses authorization for new capabilities (no key rotation in MVP). Consumes all prior admin authorizations for `member_id` (sticky removal, §3.7).

**`message.text`**
```
content = {
  "body":        tstr,                  // UTF-8, ≤ 16384 bytes
  "format":  opt tstr,                  // "plain" (default) | "markdown"
  "in_reply_to": opt bstr[32],          // EventId
  "mentions":    opt [ bstr[32], ... ]  // member identities
}
```

**`file.shared`** — references a content-addressed blob (the Blob Plane carries the bytes).
```
content = {
  "file_id":     bstr[16],              // CLI handle for `file fetch`
  "name":        tstr,
  "mime_type":   tstr,
  "size_bytes":  uint,
  "blob_hash":   bstr[32],              // BLAKE3-256 iroh-blobs hash (named: blake3)
  "blob_format": opt tstr,             // "raw" (default) | "hash_seq"
  "providers":   opt [ bstr[32], ... ] // EndpointIds expected to serve it; default [device_id]
}
```
Verifier of fetched bytes: recompute BLAKE3-256 and require it equals `blob_hash`.

**`pipe.opened`** — announces an authenticated TCP forward (Live Pipe Plane).
```
content = {
  "pipe_id":          bstr[16],
  "owner_id":         bstr[32],         // MUST == sender_id
  "owner_endpoint":   bstr[32],         // EndpointId to dial (== owner's device_id)
  "kind":             tstr,             // "tcp" (only value in MVP)
  "label":            tstr,
  "target_hint":      tstr,             // advisory only
  "alpn":             tstr,             // ALPN for the data stream, e.g. "/iroh-rooms/pipe/1"
  "allowed_members":  [ bstr[32], ... ],// identities authorized to connect (non-empty; no default-all)
  "expires_at":   opt uint
}
```
Validate: `owner_id == sender_id`; `kind == "tcp"`; `allowed_members` non-empty.

**`pipe.closed`**
```
content = {
  "pipe_id": bstr[16],                  // references an open pipe.opened.pipe_id
  "reason":  opt tstr                   // "closed" | "expired" | "owner_exit" | "error"
}
```
Validate: referenced pipe is open; `sender_id` is its owner or the admin.

**`agent.status`**
```
content = {
  "status":               tstr,         // "running" | "running_tests" | "blocked" | "error" | "done" | ...
  "message":          opt tstr,
  "related_artifact_ids": opt [ bstr[16], ... ], // file_ids
  "progress_pct":     opt uint          // 0..=100 (integer; no floats)
}
```

## 8. Rejection / Flag Taxonomy

Every outcome maps to a stable reason code (for the local audit log and CLI failure-mode distinction, PRD §16):

- **Rejections (event dropped, never persisted or re-broadcast):** `unknown_schema_version`, `unknown_event_type`, `non_canonical_encoding`, `id_mismatch`, `bad_signature`, `unbound_device`, `not_a_member`, `insufficient_role`, `room_id_mismatch`, `invalid_content`, `expired_invite`, `bad_capability`, `too_many_parents`, `not_genesis_descended` (§6 step 9a).
- **Ignored (not an error):** `duplicate`.
- **Advisory flags (event is STILL accepted, validated, ordered, and persisted — the flag never affects convergence):** `clock_skew` (§6 step 10), `equivocation` (Membership & Ordering §7), `from_removed_member` (UI attribution, §5/§7).

---

# Membership & Ordering Model (Room Event Plane core)

Status: Phase 0 protocol design record, **hardened against adversarial review**. Resolves PRD v0.3 §10.1–10.4, 10.6, 10.7 and Open Questions 2 and 4. Scope: ≤5-person private rooms, single device per identity, no key rotation, hand-rolled signed append-only event DAG over full-mesh QUIC (core iroh 1.0). The same rules apply unchanged if the log is later carried by iroh-docs (§8).

## 0. Design goal, and the honest statement of what converges

The hard problem: the envelope carries `prev_events` (a causal DAG) and the SAME log carries membership/authorization events, which ARE state. We need honest peers to **converge** on (a) message order and (b) the membership/authz set, **deterministically**, without a general state-resolution algorithm.

Four design choices collapse the difficulty:

1. **One immutable admin.** The room creator (genesis signer) is the sole authority for the whole MVP. There is never a second writer to the authorization state — exactly the case that would force Matrix-style resolution. Multi-admin is deferred.
2. **Ordering is a single comparator.** A derived Lamport clock plus `event_id` tie-break gives a deterministic linear extension of the DAG. No state resolution for ordering.
3. **Validity is ancestor-stable.** Every event is judged against its own fixed causal ancestors, never the receiver's live state. So the verdict is identical on all peers and independent of arrival order.
4. **Membership is a commutative causal fold.** Monotonic admin authorizations + a per-subject "causal heads, Removed-dominates" rule + a deterministic attribute merge (§3.8) is provably convergent and is a tiny, fully specified state machine.

### The precise convergence guarantee (stated only in its hedged form)

> **Any two honest peers that hold the IDENTICAL validated event set compute byte-identical membership state and timeline** — regardless of arrival order, restarts, or equivocation.

This is the real theorem and it survives adversarial review. The **unqualified** claim "equivocation cannot cause divergence" is **false and must not be used**: convergence is *conditional on set-completeness*, and the protocol mechanically guarantees only **ancestor-completeness** (you can always follow `prev_events` back to genesis), **never sibling/concurrent-completeness** (you cannot, by following pointers, discover a concurrent event that nothing you hold references — e.g. a withheld or not-yet-arrived removal TIP, or a segregated admin fork delivered only to another partition). A peer cannot, by backfill alone, detect that its set is incomplete in the concurrent dimension.

### Detecting incompleteness (the mitigations, applied)

Because set-completeness cannot be self-certified by ancestor backfill, the design adds three mechanisms so incompleteness becomes *detectable* and the node *fails closed*:

- **Derived admin sequence + admin-chain tip.** Admin events self-parent (§1), so the admin's own events form a hash chain; define `admin_seq(e)` = the length of the admin self-parent chain ending at `e` (a *derived* quantity, like Lamport — **not** a wire field, so no test vector changes). The **admin-chain tip** a peer holds is its highest-`admin_seq` admin event.
- **Admin-tip advertisement (room-wide).** Peers advertise their admin-chain tip (`event_id` + `admin_seq`) to each other — over the mesh pull RPC and/or the optional gossip liveness channel (ADR-1). If any peer advertises a tip with `admin_seq` greater than mine, or a *different* `event_id` at the *same* `admin_seq`, my membership view is suspect.
- **Fail-closed on suspected incompleteness.** On (a) a known-higher admin tip I have not yet backfilled, or (b) two distinct admin tips at the same `admin_seq` (a fork), the node **fails closed on contested/removal-sensitive decisions** (denies new pipe/blob access for subjects whose status could be affected) and raises a CRITICAL `equivocation` alert in case (b). Two admin tips at the same `admin_seq` is the detectable signature of admin self-fork even when no single peer initially holds both branches.
- **Membership sub-DAG is never windowed.** Recent-history sync may bound *chat* by count/time, but the **complete authorization chain** — genesis plus all `member.invited/joined/left/removed` for currently-relevant subjects plus the full admin chain — MUST always be synced (a hard invariant on the sync layer; §4, §8).

The residual — a removal whose only holders are offline/partitioned/withholding — is **irreducible without an availability assumption**, and is documented as such: **removal/leave enforcement is bounded by the reachability of the removal event, not by "briefly."** Tear-down-on-learn (§5) fires only if and when the enforcing peer learns of the change.

## 1. Identity, signing, and the event envelope

- A **principal** (human or agent) is a 32-byte Ed25519 identity key (`sender_id`). The signing/device key (`device_id`) is a **distinct** 32-byte key bound to the identity via a device certificate (Event Protocol §1). MVP = one device per identity. The device key is the iroh `EndpointId`, so transport identity, event-signing identity, and ACL identity are one key (the device key), while authorization/membership is tracked against the identity key.
- **Canonical bytes / signature / event id:** exactly as Event Protocol §3/§4/§6 (deterministic CBOR over the eight signed fields; `event_id = BLAKE3-256(CSB)`; `signature = Ed25519_sign(device_key, EVENT_CONTEXT ‖ CSB)`; signature and id excluded from CSB). Distinct content ⇒ distinct id ⇒ an equivocating author's two events are two separately-tracked events.
- **`room_id`** is the §5 derivation (`BLAKE3-256(context ‖ creator_sender_id ‖ room_nonce ‖ created_at_be)`), recomputed from `room.created`. It binds the log to one root/admin.

**Self-parent rule.** Every non-genesis event by author K MUST list K's own most-recent prior event among `prev_events`. This makes each author's events a per-author hash chain (tamper-evident, per-author monotonic, and makes equivocation detectable as "same author, two concurrent events"). The admin's self-parent chain is what defines `admin_seq` (§0).

**Genesis-descent rule (explicit).** Every non-genesis event MUST have non-empty `prev_events` and MUST transitively reach the room's genesis `room.created`. This is enforced structurally (Event Protocol §6 step 9a), not merely implied by the membership fold, so that floating events and future event types cannot slip through.

## 2. Ordering of grow-only / chat events (PRD Open Q2)

### 2.1 Lamport clock (derived, not on the wire)
`lamport(genesis) = 0`; `lamport(e) = 1 + max(lamport(p) for p in prev_events)`. It is **recomputed by every peer from the signed `prev_events`** — there is no `lamport` field on the wire (the golden envelope is `map(8)`). Since `prev_events` is signed, the derived value is equally authenticated and there is nothing to lie about: an author cannot inflate/deflate causal depth without changing `prev_events` (and thus the `event_id`).

### 2.2 The total order
Sort the validated, causally-complete event set ascending by the pair **`(lamport, event_id)`**, `event_id` compared bytewise over its 32 raw digest bytes.

- If `a → b` (happens-before), then `lamport(a) < lamport(b)` strictly, so `a` always sorts before `b`. The comparator is therefore **always a linear extension of the `prev_events` DAG** — no separate topological pass needed.
- `event_id` breaks ties between causally-concurrent events deterministically.
- The order is a **pure function of the delivered validated set** ⇒ identical sets produce byte-identical timelines.

### 2.3 Stability and re-render
An event is placed only when **causally complete** (all ancestors present — §4). A late-arriving concurrent event with a smaller `event_id` at the same `lamport` can be inserted earlier; for chat this is acceptable (CLI re-renders / `room tail` reflows). `created_at` may be shown to humans but is never used to order or to make security decisions.

### 2.4 Timeline position carries no trust (documented hazard)
An author controls their own `lamport` (via `prev_events` selection: cite only genesis ⇒ `lamport 1`) and can grind `content` so `BLAKE3(CSB)` has many leading/trailing zero bytes, pinning their event first or last in every peer's canonical order. This **does not break convergence** (every peer agrees on the pinned position) but means **no logic or UI may attach trust or semantic meaning to timeline position** ("first"/"top"/"pinned"). Render with a receive-order band if a stable visual order is needed; treat position as unauthenticated.

## 3. Membership model (PRD 10.4, Open Q4)

Membership is **state derived by folding the validated, ordered log** through a small deterministic machine. It is "state", but with a single authority and a commutative, fully-specified rule set.

### 3.1 Roles (MVP)
- **admin** — exactly the genesis signer. Single, immutable, no transfer, no co-admins. Only the admin may invite/remove, so authorization never has two competing writers.
- **member** — an admitted principal. May send messages, share files, open/own pipes; may NOT invite or remove.
- **agent** — a member with `role = "agent"`. Same membership rules; cannot invite/remove; can open pipes only when room policy + explicit authorization allow (PRD 13.3).

### 3.2 Membership events
- `room.created` (genesis): empty `prev_events`; defines `room_id`, sets admin = signer, initial members = {creator: Active}. Unique root.
- `member.invited{invitee_key, role, capability_hash, …}`: admin-signed authorization. **Key-bound** (`invitee_key` required — see §6). Grow-only.
- `member.joined{via_invite_id, capability_secret, role, device_binding, …}`: subject-signed; admits the subject if backed by a still-live admin authorization for that subject's key (§3.5).
- `member.left{member_id == sender_id}`: subject-signed voluntary leave.
- `member.removed{member_id, removed_by == sender_id == admin}`: admin-signed kick.

### 3.3 Joins are grow-only; departure is per-subject status
Admissions form a grow-only set. Departure (leave or removal) is a per-subject status flag whose writer must be authorized (admin for `member.removed`, self for `member.left`). This is an OR-Set-like structure constrained so only specific signers may add/remove specific subjects.

### 3.4 The membership fold (deterministic)
Maintain `admin` (fixed) and, per subject X, a status + attributes computed as:

1. Collect all **valid, authorized** events touching X (validity per §3.5, judged from each event's own ancestors).
2. Find the **causal heads** of that set (events with no valid descendant within the set).
3. **Status:** any head is `member.removed`/`member.left` ⇒ **Removed**; else a head is `member.joined` ⇒ **Active**; else (invite only) ⇒ **Invited** (authorized but not yet a member; cannot send/connect).
4. **Attributes:** resolve `role`/`kind` and capability scope by the deterministic merge of §3.8.

`Removed` dominates concurrent heads. The fold is commutative and deterministic ⇒ identical membership on every peer with the same validated set (and across restarts).

### 3.5 Authorization gate (who may write membership)
Applied using only the event's fixed causal ancestors (ancestor-stable):
- `member.invited(X)`, `member.removed(X)`: valid iff signer == admin (and for removed, `X != admin`).
- `member.left(X)`: valid iff signer == X (`member_id == sender_id`).
- `member.joined(X)`: valid iff signer == X's device key AND it causally descends from a **still-live** admin authorization for X — a `member.invited` whose `invitee_key == X` that has NOT been invalidated by a `member.removed(X)` **or `member.left(X)`** lying causally between that authorization and the join.

Events failing the gate are dropped and logged as protocol violations; they never affect state.

### 3.6 Leave / removal without key rotation
MVP has no key rotation (PRD 13.4). Consequences, made explicit:
- Removal changes membership state (so the subject loses authorization for new pipe/blob connections) but cannot cryptographically erase what they already received.
- Enforcement is **fail-closed at connect + tear-down-on-learn** (§5): a removed subject is denied new connections and has live connections dropped as soon as the enforcing peer learns of the removal. **Exposure is bounded by removal-event reachability** (§0), not by "briefly."
- Re-admission after leave/kick always requires a fresh admin invite that causally follows the departure (§3.7).

### 3.7 Monotonic authority / sticky departure (leave AND removal consume authorizations)
An admin authorization for X is **consumed/invalidated by any `member.removed(X)` OR `member.left(X)` that causally follows it.** Therefore a stale pre-departure invite (or a replayed old join) can never resurrect a departed member; only a **new** admin invite causally **after** the departure, plus a join descending from it, can. This holds symmetrically for kick and voluntary leave: *"departure is sticky; re-invite is explicit and post-departure."* (Treating `member.left` as invite-consuming closes the self-rejoin-after-leave hole: a member cannot manufacture a join that descends past their own `member.left` while reusing the old invite.)

### 3.8 Deterministic concurrent-attribute resolution (safety-biased)
When a subject X has **concurrent membership heads with conflicting attributes** (e.g. the admin issued `invited(X, role=member)` and `invited(X, role=agent)` on sibling branches, or two different capability scopes), the fold MUST still produce a single deterministic result for every peer holding the same set:

1. **Least-privilege dominates.** Among the conflicting heads, the **narrowest** privilege wins: `role` resolves to the *least-privileged* value present (`agent` < `member` < `admin`); capability scope resolves to the *intersection* (narrowest) of the concurrent grants.
2. **Tie-break by event_id.** If privilege levels are equal, the head with the **lowest `event_id`** (bytewise) wins.

This is part of the fold spec (and a conformance vector, Test Vectors §18). Without it, two peers with the identical set could derive different `role`/`kind` and thus make different agent-pipe-authorization decisions — the one same-set divergence the model must not allow.

## 4. Out-of-order delivery — no permanent divergence (PRD 10.7)

Three-stage pipeline; order matters.

1. **Structural / crypto (context-free, immediate).** `schema_version` supported; required fields present; no unknown critical fields; `room_id` matches; `event_id == BLAKE3(CSB)`; signature verifies under `device_id`; `prev_events` ≤ 20; non-genesis ⇒ non-empty `prev_events`. Failures ⇒ reject + log.
2. **Causal readiness (with anti-amplification bounds).** If any id in `prev_events` is missing locally, the event goes to a **pending buffer** keyed by missing-parent ids and we **backfill those parents by id from peers** — we do NOT reject it. Once parents are present, recompute and check `lamport`, enforce genesis-descent (§6 step 9a), and enforce genesis uniqueness. **Anti-amplification (applied):** to deny non-members and removed members a free backfill/disk-park DoS (the pipeline reaches authorization only at stage 3),
   - run a **lightweight membership pre-check on the signer** before buffering — if the signer's `device_id` is not even plausibly a member/invitee in the *currently-known* state, drop early rather than buffer and fan out backfill;
   - **require an event to be reachable from genesis through already-validated ancestors** before spending backfill effort on its remaining missing parents (don't chase phantom-parent chains from unauthenticated authors);
   - **cap the parked set per author key with eviction**, and **rate-limit/quota backfill by requesting author**;
   - drop events with structurally implausible derived `lamport`.
   Events that pass these bounds but whose parents cannot be fetched in time are **parked on disk and retried on reconnect**, not silently discarded. Duplicates (same `event_id`) are ignored idempotently (G-set).
3. **Semantic / authorization (ancestor-based).** Run per-type rules (§3.5 for membership; "author is Active in the event's ancestor-view fold" for message/file/pipe/agent; `pipe.opened` by owner; etc.) using ONLY the event's causal ancestors. Insert into the validated set, recompute the membership fold, then re-process buffered children.

Why honest peers cannot **permanently** diverge (conditioned on equal sets, §0): authorization is evaluated against the event's own fixed ancestors, so every peer reaches the identical verdict regardless of arrival order; a message from X that arrives before X's `member.joined`/invite/genesis is buffered and backfilled, never dropped for "unknown sender"; validated set + deterministic fold + deterministic comparator ⇒ identical membership and timeline for identical sets. Divergence is only ever transient *and only vanishes as sync equalizes the delivered set* — which is exactly the set-completeness caveat of §0.

## 5. Pipe / blob authorization at connect-accept time (PRD 10.6, 13.2)

Membership is the authorization source; enforcement happens at the moment of connection over QUIC.

- **Proven identity.** The QUIC/TLS-authenticated remote `EndpointId` is the peer's Ed25519 **device** key; resolve it to the bound **identity** key via validated membership state, then evaluate membership against the identity key.
- **Snapshot + fail-closed.** Enforcement evaluates the **current local membership snapshot** (the deterministic fold over the local validated log). Default deny. **If §0's incompleteness detector is tripped (known-higher admin tip not yet backfilled, or a same-`admin_seq` fork), fail closed on the affected subjects** until reconciled.

**Blob serve gate (iroh-blobs `provider::events`).** Configure `EventMask` with `connected = Intercept` and `get`/`get_many = Intercept`.
- On `ClientConnected`: accept only if the connecting identity ∈ Active members; else reject.
- On `RequestReceived`: serve a hash only if it is referenced by a valid `file.shared` authored by an Active member and causally visible; otherwise return `AbortReason::Permission`.

**Pipe connect gate (custom ALPN `accept()`).** Accept a connection for `pipe_id` only if ALL hold against the local snapshot:
1. remote identity ∈ Active members,
2. remote identity ∈ the `allowed_members` of the governing `pipe.opened` (no default-all — PRD 13.2),
3. that `pipe.opened` is authored by an Active member who is the pipe owner,
4. no `pipe.closed` for `pipe_id` is causally known,
5. `expires_at` (if present) > local wall clock — the ONE place wall clock is consulted, and only to **deny** (fail-closed).

Reject otherwise and write a local audit event (`pipe.connect.rejected`).

**Revocation-on-learn (the no-rotation mitigation).** Long-lived pipe connections subscribe to membership changes. When the connected peer becomes `Removed`/`Left`, or `pipe.closed`/expiry fires, the enforcing node **immediately tears down** the live connection and logs it. Exposure is bounded to "until the enforcing peer learns of the change" (§0) — the best achievable without key rotation.

**Log-validity vs access-control split (important).** A chat/file/agent event by a since-removed member may still be *valid for log inclusion* (it was Active in its own ancestor view; the log is append-only) but grants **zero capabilities**, because access control uses the **current global snapshot**, not the ancestor view. The UI flags such events as "from a removed member" (advisory `from_removed_member`). This contains an equivocating, ignore-my-own-kick member: they can scribble on their own fork but get no pipe/blob access and converge to Removed everywhere.

**Removed-member timeline pollution (documented MVP limitation).** Because a removed member can keep authoring log-valid events by citing only pre-removal ancestors, they can inject unbounded, un-revokable timeline entries — including *misleading* `file.shared` (a hash they will not serve) and `pipe.opened` (authorizing nothing). This is contained for **safety** (zero capabilities) but is a UX hazard. Recommended (not blocking): UI hard-segregation/collapse of "from a removed member" events; exclude them from file/pipe pickers and "available files" listings; optionally an admin-signed tombstone to hide. Treat as a documented MVP limitation.

## 6. Invite tickets as log-verifiable capabilities (PRD 10.5)

A join must be verifiable from the log alone by peers who were never handed the ticket. **MVP uses key-bound (path-A) invites for any room where removal must be enforceable — which is all of them.**

- **(A) Named, key-bound pre-authorization (REQUIRED in MVP).** The admin posts `member.invited{invitee_key, capability_hash, role, expires_at?}` naming the invitee's identity key before/with issuing the ticket; the join validates by citing that invite and proving `sender_id == invite.invitee_key` plus the capability secret. Because the authorization is **bound to a specific key**, a removal of that key consumes it (§3.7) and **ban evasion under a fresh key is impossible** — a join under a never-before-seen key has no naming invite and fails the gate.
- **(B) Open bearer tickets — NOT used for removal-enforceable rooms.** An admin-signed capability not bound to any key authorizes *any* key, so removal of key X cannot consume an authorization that names no key, and the banned party (or anyone who saw the still-unexpired ticket) can mint a join under a brand-new key X′ with an empty removal history ⇒ `Active` everywhere. **This defeats "kick is sticky" and is therefore excluded from MVP.** If open tickets are ever reintroduced for UX, they MUST be made removal-safe by (i) binding the cap to the **first key that uses it**, recorded via an admin-acknowledged event, and rejecting subsequent keys, **and/or** (ii) adding **admin-signed `cap_id` revocation events** so a removal can also revoke the open cap — and it must be documented that **kicking while an open ticket is live requires ticket/cap rotation.**

**Capability hash.** `capability_hash = BLAKE3-256("iroh-rooms:invite:v1" ‖ room_id ‖ invite_id ‖ secret)`, secret ≥16 bytes traveling out-of-band in the iroh ticket only (a leaked `EndpointId` alone is insufficient).

**Expiry determinism (log-only, no local clock).** Validity rule: join valid iff `invite.expires_at` absent OR `join.created_at <= invite.expires_at`. Both fields are signed, so every peer computes the same verdict. A joiner could lie about `created_at` to beat expiry — the same trust class as a leaked ticket, explicitly out of scope (PRD 13.4). **The clock-skew sanity check is strictly advisory** (Event Protocol §6 step 10 / §8): it MAY flag/alert a join whose `created_at` is implausibly far from local time, but it **MUST NOT** reject the event, exclude it from the validated set, or influence any expiry/authorization verdict — otherwise log validity would depend on each peer's local clock and honest peers would permanently diverge.

**MVP limitations (documented):** key-bound tickets have no native revocation other than removing the subject; `max_uses` cannot be enforced convergently without a counter authority, so MVP treats a key-bound invite as single-subject (one `invitee_key`) and reusable by that key until expiry. Revocation lists (admin-signed) and key rotation are post-MVP (PRD 13.5).

## 7. Conflict / fork / equivocation resolution (the crux, PRD 10.7.3)

Unifying principle: **authorization is monotonic and signer-gated; per-subject status conflicts resolve by causal heads + Removed-dominates; attribute conflicts resolve by least-privilege + lowest-`event_id`; equivocation is detected, contained, and alerted but still resolves deterministically for peers holding the same set.**

- **Concurrent invite/join vs kick.** Both events validate; `Removed` dominates ⇒ X converges to Removed and gets no capabilities. Re-admission needs a fresh admin invite causally after the removal (§3.7). Deterministic everywhere.
- **Stale-invite replay after departure.** A join citing a superseded (removal- or leave-consumed) authorization is unauthorized (§3.5/§3.7) and rejected on every peer; only a post-departure invite enables rejoin.
- **Two admins.** Impossible in MVP — exactly one admin; any non-admin invite/removal fails the gate. (This is *why* the model is simple; multi-admin reintroduces multi-writer resolution and is deferred. Post-MVP path: model the admin set as its own grow-only/quorum CRDT or a signed successor list.)
- **Admin signing two branches on the same parents (equivocation/fork).** The two events get distinct `event_id`s and enter as concurrent siblings; the fold (Removed-dominates / least-privilege) converges all peers **holding the same set** to the same safety-biased result. We do NOT try to pick "the real branch."
- **Segregated admin fork (different branches delivered to different partitions).** This is the case where no single peer initially holds both branches, so the same-set guarantee is vacuously true while peers genuinely *disagree*. It is addressed by **admin-tip advertisement** (§0): two distinct admin tips at the same `admin_seq` are detectable across the room even when no peer holds both; on detection, raise a **CRITICAL `equivocation` alert** and **fail closed on contested subjects** until reconciled.
- **Detection + alert.** Any key that authors two mutually-concurrent events is forking (honest single-instance clients always self-parent, so they never fork). Raise a local `equivocation` alert naming the signer and both `event_id`s; **CRITICAL** when the signer is the admin and the forked events touch membership/authz, **INFO** otherwise (e.g. a member misconfigured to run one key on two machines). MVP **detects and alerts**; it does not auto-eject the admin (the admin is the trust root; a malicious admin compromises the room by definition).
- **Convergence guarantee (hedged form only).** All state is a pure function of the delivered validated set, so equivocation — even by the admin — yields **identical state on all peers holding the same set.** Equivocation can change *what* the converged state is; it can never make two honest peers **with the same set** disagree. It can, however, leave peers with *different* sets disagreeing until sync (and admin-tip detection) reconciles or fails them closed — see §0.

## 8. Mapping onto the substrate choices

- **Full-mesh QUIC (recommended).** Each peer dials every other (≤5). Custom ALPN `/iroh-rooms/event/1` carries: live event push; a pull/backfill RPC (request events by id, request the **never-windowed membership sub-DAG + full admin chain**, request the recent chat window); and **admin-tip exchange**. Ordering/membership semantics here are transport-agnostic and need only core iroh 1.0.
- **If iroh-docs is adopted (Phase 5).** Store each event under a **fixed-length, prefix-free** key, segregated by class: membership/authorization under `"m/" + event_id`, windowed chat under `"c/" + event_id`, value = the signed canonical bytes. Docs' range-based set-reconciliation becomes backfill; its live notifications drive the tail. This document still supplies the ordering comparator, ancestor-stable validity, the membership fold, and the attribute merge — none of which docs provides (LWW KV, not a causal DAG; wall-clock-µs tiebreak). The **never-windowed** invariant maps to "**fully reconcile namespace `m/`**"; chat windowing applies only to `c/`. Fixed-length hash keys avoid Willow **prefix-deletion** (an `author/seq` scheme would silently drop events).
- **iroh-gossip** is NOT used for the load-bearing log (no ordering/persistence/replay/auth). It MAY optionally provide a best-effort liveness/notify signal **and the admin-tip advertisement channel** (§0), with `Event::Lagged` treated as a "trigger a backfill" hint.

## 9. Persistence note (PRD 12)
The SQLite `events` table stores raw signed bytes keyed by `event_id`, with indexed derived `lamport`, `prev_events`, `sender_id`, `event_type`, and (for admin events) derived `admin_seq`. `members`, `sync_state` (heads, parked-orphan set with per-author caps, recent-window cursor, highest known admin tip), and `trust_decisions` (equivocation alerts, fail-closed subjects) are **derived caches** rebuildable by re-folding `events` — guaranteeing restart determinism. The append-only log is the single source of truth.

---

# Protocol Test Vectors

These vectors are normative conformance checks. Vectors 1–17 are **byte-exact and recomputable**: every key is derived from a non-secret seed (`seed = byte repeated 32×`), every `event_id`/signature was produced with deterministic CBOR (`cbor2 canonical=True`), BLAKE3-256, and Ed25519 (`ed25519-dalek`-compatible, RFC 8032). **This spike independently re-derived and confirmed** the golden `event_id` (`blake3:c389e2…85a1`), signature, `device_binding.sig`, `room_id` (`43c19f2e…16a3`), `room_id_B` (`cad9174a…3494`), the tampered-body id (`6267b72c…c75c`), and the cross-room re-signed id (`81b6a82b…f057`). An independent third reproduction during QA (Python `cbor2 canonical=True` + `blake3` + Ed25519/PyNaCl) re-confirmed every one of those exact values. **Caveat:** the multi-event *fixture-log* ids (`E_create`…`E_pipe`, `E_eq_a/b`, `E_mal`) were NOT independently reproduced — their full content maps are not pinned in this document, so the conformance harness MUST regenerate them from the final content schema before they are trusted as golden. Vectors 18–20 specify the **hardening rules** (member.left invite-consumption, concurrent-attribute resolution, advisory clock-skew); their concrete `event_id`s are generated by the conformance harness from the final content schema (the rule and required verdict are normative).

Each vector is `GIVEN` / `WHEN` / `THEN` (with the §8 reason code where applicable). An implementation conforms iff it produces every `THEN`.

## Fixtures

**Cast (Ed25519 public keys, hex).** Each principal has an identity key (`*_id`) and a device key (`*_dev`); the device key signs events and is the iroh `EndpointId`.

| Principal | seed (id / dev) | `sender_id` (identity, hex) | `device_id` (signing, hex) |
|---|---|---|---|
| Alice (creator/admin) | `01` / `02` | `8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c` | `8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394` |
| Bob (member) | `03` / `04` | `ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1` | `ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c` |
| Carol (member) | `05` / `06` | `6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1` | `8a875fff1eb38451577acd5afee405456568dd7c89e090863a0557bc7af49f17` |
| Dave (invited, then kicked) | `07` / `08` | `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c` | `1398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93ca` |
| Mallory (never a member) | `09` / `0a` | `fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618` | `43a72e714401762df66b68c26dfbdf2682aaec9f2474eca4613e424a0fbafd3c` |

**Room.** `room_nonce = 000102030405060708090a0b0c0d0e0f`, creator = Alice, `created_at = 1750000000000`. Per §5:
`room_id_A = BLAKE3-256("iroh-rooms:room-id:v1" ‖ alice_id ‖ room_nonce ‖ u64_be(1750000000000))` =
`43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3`.

**Fixture log (validated DAG).** A linear chain plus a deliberate concurrent fork. `lamport` is the *derived* ordering key (`lamport(e)=1+max(lamport(parents))`, `lamport(genesis)=0`), never on the wire (`map(8)`).

| Handle | type / signer | `prev_events` | `lamport` | `event_id` |
|---|---|---|---|---|
| `E_create` | `room.created` / Alice | `[]` | 0 | `blake3:67bc4068dfb90701b64af282d7413f18db8828c8d803811be51b7c7e9793f34d` |
| `E_inv_bob` | `member.invited` / Alice | `[E_create]` | 1 | `blake3:7c43f92a7a8e6306aa2126e9fbc213f03e1b8b36c5eb834d236b1943e442b540` |
| `E_join_bob` | `member.joined` / Bob | `[E_inv_bob]` | 2 | `blake3:a556b3d2cb5d666038cc1637b41d8a6744b647d0580b11c4ba5938859a2a1540` |
| `E_inv_carol` | `member.invited` / Alice | `[E_join_bob]` | 3 | `blake3:c48b1e978225ec6906ad09cc8c7599815afe1b09d98d13184f82eeae43340984` |
| `E_join_carol` | `member.joined` / Carol | `[E_inv_carol]` | 4 | `blake3:ea13e4f75583ad10914dfb13a099a76d8607e51c94876125e811ac7dc20e6ccc` |
| `E_msg_bob` | `message.text "hi all"` / Bob | `[E_join_carol]` | 5 | `blake3:7292b7624c5ad4465ab3bc4c99f0f5d6ea37220803689eafb1d32edcb1e1251b` |
| `E_inv_dave` | `member.invited` / Alice | `[E_msg_bob]` | 6 | `blake3:4a9c5310e9079aa4e58b012441e8a368078350fc6ee35d525df28d02098a1e7e` |
| `E_join_dave` | `member.joined` / Dave | `[E_inv_dave]` | 7 | `blake3:53c561a9b70775c1d222b8a258d23742366f982a78efcef68a75339af7f2215c` |
| `E_kick_dave` | `member.removed` / Alice | `[E_inv_dave]` | 7 | `blake3:b4f8a960f7220fd424007e4b7e9872b4c3caa8c545fef15419bae516de9dd2b8` |
| `E_file` | `file.shared` / Bob | `[E_msg_bob]` | 6 | `blake3:844342036aa0d7a0efd3dfe1b9f908ca4d752abc1c4c5efd509e308c3fbce3f2` |
| `E_pipe` | `pipe.opened` / Bob | `[E_file]` | 7 | `blake3:7f6c2d2867df970c59dfccaad259854fba5d520434797f82b8f6cdcd55d36593` |

Invite capabilities (`capability_hash = BLAKE3-256("iroh-rooms:invite:v1" ‖ room_id ‖ invite_id ‖ secret)`): Bob `invite_id=b0b0…b0b0` `secret=5ec0…b0b0` → `cap=7ed58c86a8863dc116e219c389b5edbbe8ee0220a518c3a4ec0e731fb2b2d4a4`; Dave `invite_id=da7e…da7e` `secret=5ec0da7e…da7e0` → `cap=b944bc4dac2c03bccb4723b647a8358106d6d6af195acd564600d8617ae6447c`. Each invite is key-bound (`invitee_key` = the respective identity key, §6). `E_file.content.blob_hash = dd101e8f6fcf005b1dd4780c4f7b736c4f456ce292e50a896d1f40df6dbef313`. `E_pipe.content.allowed_members = [alice_id, bob_id]`.

---

### 1. Canonical-serialization determinism (field-order / encoder independence)
GIVEN the logical `message.text` event — the **golden event**, specified inline in this vector (`sender=alice_id`, `device=alice_dev`, `room_id_A`, `created_at=1750000005000`, `prev_events=[]`, `content={"body":"Hello room","format":"plain"}`), constructed twice: once with the eight top-level keys in §2 declaration order and `content` as `{"body",…,"format"}`; once with keys scrambled (`prev_events, content{"format","body"}, event_type, schema_version, created_at, device_id, sender_id, room_id`).
WHEN each is encoded with the deterministic-CBOR profile (§3).
THEN both produce **byte-identical** CSB — the 242-byte octet string beginning `a867636f6e74656e74a264626f6479…01`, canonical top-level order `content, room_id, device_id, sender_id, created_at, event_type, prev_events, schema_version`, `content` keys `body` before `format`. Encoder choice (`ciborium` deterministic / `cbor2 canonical=True`) MUST not change a byte. **(Re-verified this spike: CSB = 242 bytes, hash matches.)**
> **Note (serialization oracle only):** this golden event uses `prev_events=[]`, so it is a *byte-level serialization/signature* fixture, **not** a semantically-valid live event — as a real `message.text` it would be rejected by §6 step 9a (only `room.created` may have empty `prev_events`). These vectors test encoding/signing, not causal validity.

### 2. Non-canonical encoding is rejected
GIVEN a `WireEvent` whose `signed` decodes to the §9 event but was encoded non-canonically: (a) top-level key order differs; (b) indefinite-length item; (c) non-shortest `created_at`; (d) a ninth top-level key (`"nonce"`); (e) duplicate map key.
WHEN a receiver runs §6 step 4.
THEN each is rejected with `non_canonical_encoding` (case d also fails the exact-eight-keys check). Dropped and logged, never persisted/re-broadcast. (Such an event's signature may still verify over its bytes — canonicality is enforced independently so identity stays 1:1 with logical content.)

### 3. `event_id == "blake3:" + hex(BLAKE3-256(CSB))`, and `id` is recomputed
GIVEN the golden CSB (vector 1, 242 bytes).
WHEN the receiver computes `id' = "blake3:" + lowercase_hex(BLAKE3-256(CSB))`.
THEN `id' == blake3:c389e251f9654902d26ea937b3e84a01bb5e5d578e394c95b6ade8b7144e85a1`.
AND GIVEN a `WireEvent` with those exact `signed` bytes but a doctored `"id"` (`blake3:0000…0000`): WHEN §6 step 2 runs, THEN rejected with `id_mismatch` (the `"id"` field is advisory, recomputed, never trusted).

### 4. `room_id` derivation is recomputed and bound (genesis)
GIVEN `E_create` with `content.room_nonce=000102…0e0f`, `sender_id=alice_id`, `created_at=1750000000000`.
WHEN a peer applies §5/§6 step 6: `room_id' = BLAKE3-256("iroh-rooms:room-id:v1" ‖ alice_id ‖ room_nonce ‖ u64_be(created_at))`.
THEN `room_id' == 43c19f2e…16a3 ==` envelope `room_id`; accepted.
AND GIVEN a forged `room.created` whose envelope `room_id` differs (vanity-id attempt): THEN rejected with `room_id_mismatch`.

### 5. Signature accept (correct device key) / reject (wrong key)
GIVEN the golden CSB (vector 1) and signature `98732ece…4f0f`.
WHEN verifying `Ed25519_verify(device_id, "iroh-rooms:event:v1" ‖ CSB, sig)` with `device_id = 8139770e…b394`.
THEN **succeeds**; accepted at §6 step 3.
AND WHEN verified using `sender_id = 8a88e3dd…6f5c` (identity key) as the public key — the classic "verify under the wrong field" bug — THEN **fails** → `bad_signature`. The signature MUST verify under `device_id`, never `sender_id`.

### 6. Tampered field → signature fails and identity changes
GIVEN the golden event (vector 1) with one byte of `content.body` flipped (`"Hello room"`→`"Hello rooM"`), else identical.
WHEN the receiver hashes/verifies the tampered CSB carrying the original signature.
THEN (a) recomputed id becomes `blake3:6267b72c066e30154b34d4430ce8fb735563c4500ff527d371bcc3de7f34c75c` (≠ original) ⇒ `id_mismatch` at step 2; (b) `Ed25519_verify` **fails** → `bad_signature` at step 3. Flipping any byte invalidates both id and signature. **(Re-verified this spike.)**

### 7. Cross-room replay fails because `room_id` is inside the signed bytes
GIVEN room B with `room_id_B = cad9174a1e34a847711e85968020a5cabaf9b35ed600d21457458f95c9c53494` (same creator/nonce, `created_at=1750000000001`), and an attacker re-broadcasting the verbatim golden `WireEvent` (CSB carries `room_id_A`) into room B.
WHEN room B's receiver runs §6 step 6.
THEN rejected with `room_id_mismatch` (`room_id_A ≠ room_id_B`).
AND to legitimately author "the same message" in room B the sender must change `room_id` inside CSB, which changes everything downstream: `event_id` becomes `blake3:81b6a82b4a1e22291102a80a454f935ddbc87d4e15d0de400d6eb6aadb85f057` and a fresh signature is required. **(Both re-verified this spike.)** Replay across rooms is cryptographically impossible without re-signing.

### 8. Duplicate / replay idempotency
GIVEN a peer that has validated and persisted `E_msg_bob` (`blake3:7292b762…251b`).
WHEN the identical `WireEvent` arrives again byte-for-byte (gossip echo, mesh duplication, malicious replay).
THEN §6 step 11 matches an existing `event_id`: the copy is **ignored** (`duplicate`, not an error); state, timeline unchanged; nothing re-broadcast. First validly-signed copy wins (G-set). 1× or 1000× yields identical state.

### 9. Out-of-order delivery: child before parent → buffered, then accepted (no permanent divergence)
GIVEN a fresh peer P holding only `E_create … E_join_carol` (Bob, Carol Active).
WHEN `E_file` (`prev_events=[E_msg_bob]`) is delivered **before** `E_msg_bob`.
THEN P does **not** reject `E_file`. Its parent `7292b762…251b` is unknown, so per §6 step 9 / pipeline stage 2 it is buffered keyed by the missing parent and backfill-by-id is issued (subject to the §4 anti-amplification bounds: Bob passes the signer pre-check, so buffering proceeds); not surfaced/ordered yet.
AND WHEN `E_msg_bob` arrives and validates (Bob Active in its ancestor view), THEN it is inserted, its derived lamport checked (`5 == 1 + lamport(E_join_carol=4)`), and buffered `E_file` re-processed and accepted (`lamport 6`). Final state on P is byte-identical to a peer that received them in causal order. (A parent that never arrives is parked on disk and retried on reconnect — never silently dropped.)

### 10. Deterministic total order (Lamport + `event_id` tie-break)
GIVEN the validated set including the concurrent siblings `E_join_dave` and `E_kick_dave`, both `prev_events=[E_inv_dave]`, both derived `lamport = 7`.
WHEN any honest peer sorts ascending by `(lamport, event_id)`, `event_id` bytewise over 32 raw digest bytes.
THEN every peer produces the byte-identical timeline. At the `lamport 7` tie: `53c561a9…` (`E_join_dave`) `< b4f8a960…` (`E_kick_dave`), so the join displays before the kick. `a → b` ⇒ `lamport(a) < lamport(b)` strictly, so the comparator is always a linear extension of the DAG; `created_at` is never consulted for ordering.

### 11. Concurrent invite/join vs kick fork → identical membership on all honest peers (holding the same set)
GIVEN Dave invited (`E_inv_dave`, admin-signed, key-bound to `dave_id`, `lamport 6`). Two concurrent branches on the **same parent** `[E_inv_dave]`: branch A `E_join_dave` (Dave proves the capability; valid — invite live in its ancestor view); branch B `E_kick_dave` (`member.removed{member_id=dave_id, removed_by=alice_id}`, admin-signed; valid). Neither is an ancestor of the other.
WHEN peer P1 receives A-then-B and P2 receives B-then-A, each running the §3.4 fold for Dave.
THEN both validate on both peers (validity judged against each event's own fixed ancestors). Causal heads for Dave = `{E_join_dave, E_kick_dave}`; per Removed-dominates, Dave = **Removed** on P1 and P2 identically. Dave gets zero capabilities (vectors 16–17). Re-admission requires a fresh `member.invited(dave_id)` causally **after** `E_kick_dave` (sticky removal, §3.7); a replay of stale `E_inv_dave`/`E_join_dave` cannot resurrect him. **(Conditioned on both peers holding both events — see §0; if `E_kick_dave` is withheld from a partition, that partition keeps Dave Active until it learns the kick, and admin-tip advertisement is what surfaces the gap.)**

### 12. Equivocation / fork detection (admin signs two events on the same parents)
GIVEN Alice authors two events on the same parent set `[E_msg_bob]` without either self-parenting the other: `E_eq_a` (`message.text "branch one"`) `= blake3:418f8bbe34520b3397ed69fb25a3f8738bedb3236d634bf15c9526d1d7f5014a` and `E_eq_b` (`message.text "branch two"`) `= blake3:5096c905c019be1e9e33e5f85b7f1570601b3c7b5ae5cace0faa194f288c7d43`. Both crypto-valid; enter as concurrent siblings.
WHEN a peer detects one key (`alice_dev`) authored two mutually-concurrent events (honest single-instance clients always self-parent and never fork).
THEN raise a local `equivocation` alert naming the signer and both `event_id`s, severity **CRITICAL** (admin signer); MVP detects/alerts but does **not** auto-eject. State stays deterministic for peers holding the same set (both events kept, distinct ids). Per §0, segregated forks (each branch delivered to a different partition) are surfaced by **admin-tip advertisement** detecting two distinct admin tips at the same `admin_seq`, which triggers CRITICAL + fail-closed on contested subjects even when no single peer holds both branches.

### 13. Non-member event rejection
GIVEN Mallory (`mal_id`, never invited/joined) authors a well-formed `message.text` `E_mal = blake3:80d2ea4578ca1d0ac772c2c0f817772ba4da6d6b5b0408f32c431344c6989e6a` (`prev_events=[E_msg_bob]`, signed by `mal_dev`, canonical, id matches).
WHEN a receiver runs verification: steps 1–6 pass; step 8 finds `mal_id` not Active in `room_id_A`.
THEN rejected with `not_a_member`. A crypto-valid signature is necessary but not sufficient. Dropped and logged. (The §4 signer pre-check also drops it early at buffering time if its parent is missing, denying backfill amplification.)

### 14. Insufficient role (non-admin attempts an admin-only event)
GIVEN Bob (member) authors a `member.invited` (or a `member.removed` of Carol), correctly signed and canonical.
WHEN the receiver runs step 8 against §3.5 (`member.invited`/`member.removed` valid iff signer == the single immutable admin = Alice).
THEN rejected with `insufficient_role`. Only Alice can invite or remove — the single-writer property that keeps membership free of state resolution. (A `member.left{member_id=bob_id}` signed by Bob, by contrast, validates — voluntary self-leave requires `member_id == sender_id`.)

### 15. Stale/expired invite and bad capability
GIVEN Dave attempts to (re)join after `E_kick_dave` by citing the original invite `E_inv_dave` (`member.joined{via_invite_id=da7e…da7e, capability_secret=5ec0da7e…da7e0}`).
WHEN the receiver recomputes `BLAKE3-256("iroh-rooms:invite:v1" ‖ room_id ‖ da7e…da7e ‖ secret)` and checks the authorization is still live in the join's ancestor view.
THEN (a) if the supplied secret does not reproduce `E_inv_dave.content.capability_hash = b944bc4d…447c`, rejected with `bad_capability`; (b) if it reproduces the hash but the authorizing invite was invalidated by a causally-prior `member.removed`/`member.left` (sticky departure, §3.7) or `expires_at` passed (log-only, signed-fields comparison), rejected with `expired_invite`. A valid rejoin requires a fresh admin invite causally **after** the departure. Both verdicts are computed from the log alone.

### 16. Blob serve gate evaluated against the resolved membership set
GIVEN Bob's `E_file` (`blob_hash = dd101e8f…f313`), validated, authored by Active Bob; provider runs `provider::events` with `EventMask{ connected: Intercept, get/get_many: Intercept }`; snapshot `{Alice: Active(admin), Bob: Active, Carol: Active, Dave: Removed}`.
WHEN Carol (`carol_dev`), Dave (`dave_dev`, removed), Mallory (`mal_dev`) dial to fetch `dd101e8f…f313`.
THEN on `ClientConnected` accept iff the QUIC-authenticated remote identity ∈ Active: **Carol accepted**, **Dave rejected**, **Mallory rejected**. On `RequestReceived` the hash is served only because referenced by a valid `file.shared` from an Active member and causally visible; an unreferenced hash → `AbortReason::Permission`. Fetched bytes accepted only if `BLAKE3-256(bytes) == dd101e8f…f313`.

### 17. Pipe connect gate enforces `allowed_members` ∩ Active (no default-all)
GIVEN Bob's `E_pipe` (`owner_id=bob_id`, `owner_endpoint=bob_dev`, `kind="tcp"`, `alpn="/iroh-rooms/pipe/1"`, `allowed_members=[alice_id, bob_id]`, no `pipe.closed`, no `expires_at`); snapshot as vector 16.
WHEN Alice, Carol, Dave, Mallory dial the pipe ALPN at Bob's endpoint.
THEN admit iff ALL hold: remote identity ∈ Active; ∈ `allowed_members`; the `pipe.opened` authored by its Active owner; no `pipe.closed` causally known; `expires_at` (if present) > local clock. Result: **Alice accepted**; **Carol rejected** (Active but not in `allowed_members` — no default-all, PRD 13.2); **Dave rejected** (Removed); **Mallory rejected** (not a member). Each rejection writes `pipe.connect.rejected`. AND if Dave was connected when his `member.removed` is learned, the owner **tears down** the live connection immediately (revocation-on-learn, bounded by reachability — §0/§5).

### 18. Concurrent membership attributes resolve deterministically (least-privilege + lowest `event_id`)
GIVEN the admin issues two **concurrent** `member.invited` for the same `invitee_key = dave_id` on sibling branches with conflicting `role`: one `role="member"`, one `role="agent"` (and/or two differing capability scopes), neither an ancestor of the other; Dave then joins.
WHEN two honest peers holding the **identical** set fold Dave's attributes per §3.8.
THEN both peers resolve Dave's `role` to the **least-privileged** value present (`agent`), and capability scope to the **intersection** (narrowest) of the concurrent grants; if privilege levels were equal, the head with the **lowest `event_id`** (bytewise) wins. Both peers therefore make the **same** agent-pipe-authorization decision. (This closes the only same-set divergence the bare status-fold left open. Concrete `event_id`s are produced by the harness from the final content schema; the verdict is normative.)

### 19. Voluntary leave then rejoin requires a fresh post-leave admin invite
GIVEN Bob was invited (`E_inv_bob`, key-bound) and joined (`E_join_bob`), then authors `E_left_bob` (`member.left{member_id=bob_id}`, valid self-leave). Later Bob authors a new `member.joined` `J'` that causally **descends** from `E_left_bob` but **re-cites the original invite `E_inv_bob`**.
WHEN a receiver applies §3.5/§3.7 (member.left consumes all prior admin authorizations for the subject).
THEN `J'` is **rejected** (`expired_invite` — the cited authorization was consumed by the causally-prior `member.left`). Bob can only return via a **fresh `member.invited(bob_id)` causally after `E_left_bob`**, plus a join descending from it — identical to the post-kick path. (This makes leave-consumption symmetric with removal-consumption; the verdict is computed from the log alone, so every peer agrees.)

### 20. Clock-skew is advisory only and never affects the validated set
GIVEN a fully valid `member.joined` (correct signature, canonical, capability matches, authorization live) whose signed `created_at` is, say, 10 minutes ahead of peer P's local clock and within bounds of peer Q's.
WHEN P (accurate clock) and Q evaluate §6 step 10.
THEN **both P and Q accept, validate, order, and persist the event identically.** P MAY raise an advisory `clock_skew` flag/alert, but it MUST NOT drop the event, exclude it from the validated set, reorder it, or alter any expiry/authorization verdict. P and Q therefore hold the **same validated set** from the same wire bytes (no clock-induced divergence). Ticket-expiry validity uses only signed `invite.expires_at` vs signed `join.created_at`, never the local clock.

---

# Spike Plan (Phase 0)

## Goal

De-risk the **one hard plane** — the Room Event Plane — before committing to the MVP, and resolve the two architectural forks. In order of decreasing risk:

1. **The substrate works at all on the real internet.** Two peers on *different NATed networks* (not a LAN) establish a usable iroh connection. If this fails, nothing downstream matters.
2. **The signed event log is byte-for-byte correct and arrival-order-independent.** The canonical-CBOR + BLAKE3 + Ed25519 contract round-trips against the golden vector, the §6 pipeline rejects/flags every class in the §8 taxonomy, and "recent history sync" (an offline peer reconnecting and converging) actually works.
3. **The two decisions are made on evidence:** (D1) gossip vs. full-mesh QUIC; (D2) iroh-docs vs. hand-rolled reconciliation.

The blob ACL and live-pipe demos are deliberately **last** (Week 2 confirmation, not Week 1 risk budget). Scope: 1–2 dev Rust team, ~10 working days. Week 1 is the critical path and carries all three hard gates.

## Week 1 — Risk burn-down

### Day 1 — Real-network NAT connectivity (Gate A, the headline test)

The **first gate on purpose.** Hole-punching success is the load-bearing assumption under the entire product, and it is the one thing a LAN demo will lie to you about.

- **Setup:** two physical machines on two *different real networks behind NAT* — e.g. A on home/office broadband, B on a phone-LTE hotspot or a second household/coffee-shop link. **Explicitly not the same LAN, not the same Wi-Fi, no VPN bridge.** Run ≥2 NAT-pairing scenarios, including ≥1 likely-symmetric case (CGNAT / mobile).
- **Build:** minimal `iroh::Endpoint` with default n0 DNS discovery + relay (`dns.iroh.link`); dial purely by `EndpointId`; open one bidi stream; echo bytes.
- **Measure (per scenario, both dial directions):** connection-establishment success + time-to-first-byte; **path type achieved** — direct (hole-punched) vs. relay-fallback (read off the path watcher, do not assume); sustained throughput and RTT, direct vs. relayed.
- **Deliverable:** `spike-nat` binary + results table (scenario × direction × path-type × RTT × throughput × setup-time).

**GATE A — GO/NO-GO (hard; if NO-GO, stop and escalate before any further spike work):**
- **GO:** connection established **both directions within ≤10 s** in **every** scenario via *at least* relay fallback; **direct hole-punched path in ≥1 non-symmetric scenario**; relayed throughput usable for chat/control (target ≥1 Mbit/s, RTT ≤ ~300 ms over relay).
- **NO-GO:** any scenario with no path (direct *or* relay), or unusable relay latency/throughput. Escalation: evaluate self-hosted relay, reconsider discovery config, or flag the substrate assumption as broken. Do **not** proceed on a green-LAN illusion.

### Days 2–3 — Event Plane byte-level correctness (Gate B)

Front-loaded because it is hand-rolled regardless of D1/D2, and a subtle canonicalization bug is the most expensive thing to find late.

- **Day 2:**
  - Implement Event Protocol §1–§6: deterministic-CBOR encoder for the eight signed fields, CSB production, `event_id`, `room_id` derivation, Ed25519 sign/verify over `EVENT_CONTEXT ‖ CSB`, and the `WireEvent` outer envelope.
  - **Pin the golden vector as a test fixture first.** The 242-byte CSB, `event_id` `blake3:c389e2…85a1`, signature, `device_binding.sig`, `room_id`, and the room-B/tampered ids are the acceptance oracle (**all independently reproduced during this spike with `cbor2 canonical=True` + `blake3` + Ed25519 — see Test Vectors**).
  - **Known risk to verify Day 2:** confirm whether `ciborium` emits RFC 8949 §4.2.1 deterministic encoding (map-key bytewise ordering, shortest-form ints, definite-length) *by default*, or whether we must hand-sort keys / hand-emit the map. If not guaranteed, decide between a thin canonical-emit wrapper vs. a fixed hardcoded top-level key order. The decoded-then-re-canonicalize-and-byte-equal check (§3) must pass.
- **Day 3:**
  - Implement the full §6 pipeline (steps 1–11) and map every outcome to a §8 code. **Note the corrected behaviors:** `clock_skew` is an **advisory flag, not a reject** (step 10); add the explicit **genesis-descent** structural check (step 9a, `not_genesis_descended`).
  - Build a **negative/edge corpus**: one crafted `WireEvent` per §8 rejection code (`non_canonical_encoding`, `id_mismatch`, `bad_signature`, `unbound_device`, `room_id_mismatch`, `too_many_parents` (>20), `unknown_schema_version`, `unknown_event_type`, `invalid_content`, `not_genesis_descended` (empty/floating `prev_events`), `not_a_member`, `insufficient_role`), plus the byte-flip and wrong-key (`sender_id`-as-verify-key) cases, plus the **advisory** cases (`clock_skew` accepted-with-flag, `duplicate` ignored-not-error).
- **Deliverables:** `event-core` crate; golden-vector conformance test (green); negative/advisory suite covering every §8 outcome.

**GATE B — GO/NO-GO:**
- **GO:** golden vector reproduces CSB, `event_id`, and signature **exactly**; re-canonicalization stable; every §8 outcome exercised and produces its stable code; clock-skew is accepted-with-flag (not dropped); genesis-descent rejects floating events; valid events persist verbatim and dedup by `event_id`.
- **NO-GO:** any byte mismatch against the golden vector, any negative case slipping through, or any clock-skew/canonicalization behavior that could make two peers disagree. (If the CBOR library cannot be made deterministic, that is a NO-GO until the encoder strategy is resolved.)

### Day 4 — Decision 1: gossip vs. full-mesh QUIC (transport)

- **Build both, minimally**, for an N≤5 room carrying signed `WireEvent`s:
  - **(a) Full-mesh:** `Router` + ALPN `/iroh-rooms/event/1`, `ProtocolHandler::accept`, per-peer bidi streams; dial every member's `EndpointId`.
  - **(b) Gossip:** `iroh-gossip` `TopicId`, `subscribe` → `GossipSender/GossipReceiver`, broadcast the same envelopes.
- **Measure / observe:** propagation latency N=2..5; code/complexity surface; reconnect behavior (gossip late-joiner receives **nothing** sent before join — confirm and quantify the gap the sync layer must fill); `Event::Lagged` as a "resync needed" trigger; that neither gives ordering/auth (the log layer owns both). **Also prototype the admin-tip advertisement** as a candidate gossip use (off the critical path).
- **Deliverable:** `spike-transport` with both backends behind a trait + a short comparison memo.

**Decision criterion (folds into Gate C):** default per this document is **full-mesh** (ordered, reliable, authenticated streams; HyParView/PlumTree buy nothing at N≤5; admission control is native). Gossip wins only if mesh dial/maintenance proves materially harder than expected; gossip may still be retained as an optional best-effort liveness/notify + admin-tip channel. Decision must cite measured numbers.

> **COMPLETE (IR-0006):** GATE GO — D1 measured, **ADR-1 ratified**: mesh
> converges to full set equality at N=2..5 with propagation latency
> statistically indistinguishable from gossip's (both 15–18 ms per-event,
> confirming "gossip buys nothing at N≤5" rather than "gossip is slower");
> gossip late-join gap confirmed at 11/11 (mesh's late-join gap is also
> 11/11 raw, but the same link trivially carries a backfill pull, per ADR-1
> §4); mesh admission-before-`accept_bi()` confirmed and corroborates
> `iroh-rooms-net` T2; gossip's open topic admits an interloper with no
> auth check. No measured surprise crossed the Day-4 flip trigger. Residual
> Open Decision 13 (admin-tip carrier) resolved: mesh `AdminTip` control
> frame only for MVP. Full findings and the measured table in
> `crates/spike-transport/NOTES.md`.

### Day 5 — Decision 2: iroh-docs vs. hand-rolled reconciliation (sync substrate)

The genuinely hard primitive is range-based set reconciliation on reconnect. Log *semantics* (causal DAG, `(lamport, event_id)` order, membership fold, attribute merge) are hand-rolled either way; D2 is purely **how the opaque signed `WireEvent` set converges.**

- **Build the iroh-docs probe:** namespace/author keys; `doc.set_bytes(author, key, value)` with **key = fixed-length 32-byte `event_id`, prefix-free** (the Willow prefix-deletion + LWW trap), value = `WireEvent` bytes; **two namespaces** `m/` (membership, never windowed) and `c/` (chat, windowed). Subscribe for `LiveEvent::InsertRemote` as the tail; force an offline peer, reconnect, confirm reconciliation backfills the missing set.
- **Confirm the gotchas against source (mostly already done):** timestamp tiebreak is **wall-clock µs, not Lamport** (confirmed from `src/sync.rs`; just re-confirm against the pinned version) and is irrelevant under unique keys; **prefix-deletion** requires fixed-length prefix-free keys; tombstone-delete vs. append-only; docs pulls in blobs+gossip+redb (three 0.x deps); RBSR reconciles the **whole** replica with **no native bounded mode** (so chat windowing must be done by namespace separation + download policy, not by docs itself).
- **Measure:** correctness + wall-time backfilling a peer that missed ~1000 events; convergence of two divergent histories on reconnect.
- **Deliverable:** `spike-sync` (docs path) + a hand-roll feasibility note (effort to reimplement Meyer-2022 RBSR over core iroh 1.0 only).

**GATE C — END OF WEEK 1, the two decisions (GO/NO-GO):**
- **GO:** D1 (mesh | gossip) and D2 (docs | hand-roll) each resolved with a one-paragraph written rationale **backed by Day-4/Day-5 measurements**, and the **recent-history-sync primitive demonstrably works on at least one path** (offline peer reconnects → set converges, verified by event count + `event_id` set equality, with the membership sub-DAG fully reconciled). Grounded default: **mesh transport + hand-rolled SQLite log/sync** (keeps the critical path on stable core; docs' unique value is the deferred full-reconciliation primitive).
  - **D2 tie-break:** choose iroh-docs **iff** its reconciliation backfills correctly under the two-namespace/prefix-free-key mapping *and* the team accepts tracking three churny 0.x crates + redb; otherwise hand-roll on stable core only and budget the reconciliation rebuild for Phase 5.
- **NO-GO:** neither sync path converges a reconnected peer, or a decision can't be justified — extend Week 1 by up to 2 days rather than carry an unresolved fork into Week 2.

## Week 2 — Confirm the easy planes, integrate, decide MVP

### Day 6 — Recent-history sync hardening + causal layering (Gate D)

- On the **chosen** D2 path, drive §6 step-9 buffering (with the §4 anti-amplification bounds): events whose parents are unknown are buffered/backfilled (not rejected), with a **signer pre-check, per-author parked-set cap, and backfill rate-limit** to deny non-members a DoS. Prove `(lamport, event_id)` ordering is stable regardless of arrival order; prove genesis-descent rejects floating events.
- **Deliverable:** reconnect/convergence test — a peer offline through ~1000 events rejoins and reaches identical ordered state (byte-identical head set) as peers that stayed online; shuffled-delivery test; phantom-parent DoS test (junk from a non-member is dropped early, not buffered/fanned-out).

**GATE D — GO/NO-GO:** **GO** iff convergence is deterministic and arrival-order-independent under shuffled delivery and a mid-stream reconnect, and the anti-amplification bounds hold; **NO-GO** if ordering/buffering is nondeterministic or the parked-set/backfill is unbounded.

### Day 7 — Membership causal fold + fail-closed access enforcement

- Implement the membership model: single immutable creator-admin; deterministic causal fold over `room.created`/`member.invited`/`member.joined`/`member.left`/`member.removed` (monotonic admin authorizations; **Removed-dominates** status tie-break; **least-privilege + lowest-`event_id`** attribute merge, §3.8). Validity judged against each event's **fixed causal ancestors**; current snapshot used only for **fail-closed** pipe/blob access with **tear-down-on-learn**.
- Implement **leave-AND-removal invite consumption** (§3.7), **key-bound (path-A) invites only** (§6), the §1 device-binding check, and the capability flow (secret out-of-band in the iroh ticket). Implement the **admin-tip / `admin_seq` derivation + advertisement** and the **fail-closed-on-suspected-incompleteness** path (§0).
- **Deliverable:** membership-fold unit tests incl. Test Vectors §11, §12, §15, §18, §19, §20; concurrent invite/remove; a removed member's subsequent events rejected for *capabilities* but (per the log-validity split) still log-includable when ancestor-valid; a self-rejoin-after-leave attempt rejected (§19); a clock-skewed-but-valid event accepted-with-flag (§20).

### Day 8 — Blob Plane ACL confirmation (near-free win)

- `iroh-blobs 0.103` `provider::events`: `EventMask` with `get`/`get_many = RequestMode::Intercept`, deny by `AbortReason::Permission` on `RequestReceived` when `request.hash` ∉ allowed set; `ConnectMode`/`ClientConnected` to allowlist by TLS-authenticated `endpoint_id` (= `device_id`, resolved to identity; the field is `ClientConnected.endpoint_id: Option<EndpointId>` in 0.103.0 — the spike-doc name `remote_node_id` was a recon alias). Wire `file.shared` (§7) → fetch → recompute BLAKE3 == `blob_hash`.
- **Production-readiness fork (record, don't decide yet):** 0.103 (modern events ACL, on iroh 1.0, officially pre-production) vs. 0.35 (maintainer-"production", on iroh 0.35, **no** events ACL). Default 0.103 to keep iroh 1.0 + per-hash/per-node gating.
- **Deliverable:** `spike-blobs` — member fetches, non-member denied at connect, wrong/unreferenced hash denied (Test Vectors §16).

**GATE (soft):** GO iff per-hash and per-node gating both deny correctly.

> **COMPLETE (IR-0009):** GATE GO — 49 tests pass (38 unit + 11 integration).
> Confirmed `iroh-blobs 0.103.0` / `iroh 1.0.1`. Full findings in
> `crates/spike-blobs/NOTES.md`.

### Day 9 — Live Pipe Plane confirmation (near-free win)

- Use `dumbpipe 0.39` **as a library** (bin+lib): TCP-over-iroh with custom ALPN `/iroh-rooms/pipe/1`; gate the connecting peer at the iroh layer by a `device_id` allowlist derived from `pipe.opened.allowed_members` (§7, no default-all). Confirm tear-down-on-learn when membership revokes mid-session.
- **Deliverable:** `spike-pipe` — authorized member tunnels `localhost:3000`; non-allowlisted peer rejected; revocation tears down a live session (Test Vectors §17).

**GATE (soft):** GO iff allowlisted forward works and non-member is refused.

### Day 10 — Integration smoke test + MVP go/no-go (Gate E)

- End-to-end across two real-NAT machines (reuse Day-1 rig): create room → invite (key-bound) → join (device binding + capability) → exchange `message.text` → `file.shared` fetch → `pipe.opened` forward → one peer offline while others advance → offline peer reconnects and converges (chat windowed, membership sub-DAG fully reconciled). All over the chosen transport + sync paths.
- **Deliverable:** integration transcript + a 1-page Phase-0 findings memo: gate results, the two decisions with rationale, pinned versions, MVP GO/NO-GO recommendation.

**GATE E — MVP GO/NO-GO:**
- **GO:** Gates A, B, C, D green; blob + pipe demos pass; full lifecycle converges across real NATs.
- **NO-GO / conditional:** any hard gate red, or convergence fails across real NATs → MVP descopes or the failing plane gets a dedicated follow-up spike.

## What this spike deliberately does NOT do

- **No Matrix-style state resolution.** The DAG + causal membership fold is the model; no general resolution algorithm.
- **No multi-device, no multi-admin, no key rotation, no revocation lists.** One `device_id` per `sender_id`; one immutable admin. Multi-admin reintroduces multi-writer resolution and is deferred.
- **No production hardening.** No self-hosted relay deployment, no DoS hardening beyond the §4 anti-amplification bounds and the §8 negative corpus, no persistence migration story, no fuzzing, no formal threat model.
- **No UI/CLI polish.** Spike binaries only.
- **No scale beyond ≤5 peers.** Large-swarm gossip, sharding, big-room membership excluded.
- **No `schema_version` evolution / forward-compat passthrough.** Only `schema_version = 1` and the §7 registry; unknown versions/types hard-rejected (see the cross-version residual risk).
- **No agent-specific runtime.** `agent.status` is an ordinary event type; no orchestration/sandboxing.
- **No final commitment to `iroh-blobs 0.103` vs `0.35` for shipping.**

---

# Residual Risks & Open Decisions

These are the items that **genuinely cannot be settled within the spike** — either irreducible without an assumption the MVP does not make, or explicit product/scoping decisions deferred to MVP time. The convergence model **survives** adversarial review with the §0–§7 fixes applied; what remains below is real residual exposure to document and accept (or descope) at MVP go/no-go.

## Irreducible properties of the MVP design

1. **Conditional convergence (the deepest residual).** Every strong guarantee is of the form "*peers holding the same validated set agree*." The protocol mechanically guarantees only **ancestor-completeness** (via `prev_events` backfill), never **sibling/concurrent-completeness**, and a peer cannot, by backfill alone, detect that its set is incomplete in the concurrent dimension. The admin-tip/`admin_seq` advertisement + never-windowed membership sub-DAG (§0) make incompleteness **detectable** and the node **fail closed**, but detection still depends on *some* honest peer holding the missing event and being reachable. This cannot be eliminated without a stronger availability/consensus assumption than the MVP makes.
2. **Removal/leave enforcement is bounded by removal-event reachability, not "briefly."** A removed/left subject retains full capabilities at any peer that lacks the departure event — potentially indefinitely if the only holders are offline/partitioned/withholding. Tear-down-on-learn fires only when the peer learns. Documented, not fixed, in MVP.
3. **Blob-serve-to-any-Active-member + no revocation.** A single Active member can serve any referenced blob to any other Active member; there is no per-blob revocation in MVP. Combined with (2), a not-yet-known removal means continued blob access until learned. Key-bound invites (§6) close the *fresh-key ban-evasion* exfiltration path; the reachability-bounded path remains.
4. **Removed-member timeline pollution is unbounded and un-revokable in MVP.** Via narrow ancestor views a removed member can keep injecting log-valid (but capability-zero) events, including *misleading* `file.shared`/`pipe.opened`. Contained for safety; UI segregation recommended (§5) but not a protocol guarantee.
5. **Timeline position is attacker-influenceable** (author controls `lamport` via `prev_events`, and can grind `event_id`). Ascribe no trust meaning to ordering or "first/pinned" position (§2.4).
6. **`created_at` is attacker-chosen and signed**, so ticket/pipe expiry can be beaten by lying about `created_at` (same trust class as a leaked ticket). Kept strictly out of any convergence-affecting decision (§6 step 10, §6 expiry rule); the clock-skew guard is advisory only.
7. **`max_uses` on tickets is unenforceable** without a counter authority; MVP key-bound invites are single-subject and reusable by that subject until expiry.
8. **Admin key compromise/loss is unrecoverable in MVP and total.** The genesis key is the sole immutable authority *and* the iroh `EndpointId` — one key for transport + signing + ACL. A stolen key lets an attacker invite/remove/equivocate as admin silently across all three planes; a lost key freezes membership forever. No detection/containment/recovery path in MVP. **This is the single largest operational/security residual.** Mitigation for MVP is documentation + out-of-band key custody; post-MVP: signed successor/rotation list or an admin-set CRDT (reintroduces multi-writer resolution, hence deferred), or an admin-signed recovery anchor.
9. **DoS surface from causally-incomplete junk.** The §4 anti-amplification bounds (signer pre-check, genesis-reachability-before-backfill, per-author parked-set caps, backfill rate-limits) reduce but do not eliminate the resource cost of processing structurally-valid events from unauthorized signers before the authorization stage. Hard limits are tunable, not proven.

## Open decisions deferred to MVP time (cannot be settled by the spike alone)

10. **`iroh-blobs 0.103` vs `0.35` for shipping.** 0.103 has the modern events ACL on iroh 1.0 but is maintainer-labeled pre-production; 0.35 is "production" but on iroh 0.35 with **no** events ACL. The spike validates the 0.103 ACL path; the production-line choice is an MVP-time call.
11. **Cross-version schema skew can cause same-set divergence.** "Unknown critical field ⇒ reject" means a peer on new software accepts an event an old-software peer rejects during any rollout — they then hold different validated sets from identical wire events. The MVP needs either strict lock-step versioning or an explicit forward-compat policy plus shared test vectors before any second schema version ships. Not resolvable while only `schema_version = 1` exists.
12. **D1/D2 are recommended, not measured-yet.** Both recommendations (mesh, hand-roll) are adversarially confirmed at the *design* level, but Gate A/C require the Day-1/4/5 measurements to confirm hole-punching success and that the chosen sync path actually converges a reconnected peer on real NATs. A NO-GO at Gate A (no usable path on a real symmetric-NAT pair) would force a relay-infrastructure decision the spike cannot pre-make.
    > **D1 half measured (IR-0006):** the transport decision is now
    > measurement-backed — see the ADR-1 annotation above and
    > `crates/spike-transport/NOTES.md`. D2 (iroh-docs vs. hand-roll) remains
    > recommended-but-not-measured pending Day 5.
13. ~~**`admin_seq` / admin-tip advertisement transport.** Whether admin-tip advertisement rides the mesh pull RPC, an optional gossip liveness channel, or both, is a Day-4 measurement-informed choice; the *mechanism* is specified (§0) but the *carrier* is open.~~ **Decided (IR-0006):** the mesh `SyncMessage::AdminTip` control frame on the existing authenticated link is sufficient for MVP; gossip's liveness-topic carrier is prototyped and measured (freshness 3–6 ms vs. mesh's 18–21 ms on a 2-node loopback probe) but not adopted — the freshness gap is immaterial at MVP scale and a gossip tip is, by construction, a weaker-trust unauthenticated hint that can only ever trigger a re-check on the authenticated mesh path. Revisit only if a much larger room makes mesh's O(n²) control-frame fanout costly. Rationale in `crates/spike-transport/NOTES.md` §5.
14. **Phase-5 sync substrate (iroh-docs vs. build Meyer RBSR vs. p2panda).** Deferred by design. The spike parks iroh-docs as the leading candidate (with the `m/`+`c/` prefix-free-key mapping ready) but the real decision arrives only when full divergent-history reconciliation enters scope.

---

# Pinned Crate Versions & Churn Budget

Pin a **single known-good set** (these are *not* in lockstep — note blobs `0.103` vs gossip/docs `0.101` despite the same 2026-06-15 release train). Treat anything above the core as 0.x churn. Commit a `Cargo.lock` and pin the **exact** versions (not `latest`).

> **Confirm before pinning:** the version numbers in this table were gathered by automated web recon (mid-2026) and were **not** verified against crates.io in this environment — confirm each on crates.io / docs.rs first. Re-check `ed25519-dalek 3.0.0-rc.0` specifically (this crate sat on the 2.x line for a long time) and that `iroh 1.0` actually pins that exact rc; and reconcile the `iroh-blobs` version (an earlier recon pass reported `0.97`, this one `0.103`).

| Crate | Pinned version | Stability | Role in spike |
|---|---|---|---|
| `iroh` | `=1.0.1` | **stable 1.x** (wire-proto + public API committed) | Endpoint, Router, ALPN `ProtocolHandler`, discovery, relay, `SecretKey::sign`. *(Confirmed `1.0.1` via crates.io sparse index 2026-06-29; original recon said `1.0.0`.)* |
| `iroh-base` | `=1.0.1` (`^1`) | **stable 1.x** | `EndpointId` (= `device_id`), `EndpointAddr`, `EndpointTicket`/`NodeTicket`, key types |
| `iroh-gossip` | `=0.101.0` | 0.x (no API-stability promise) | D1 candidate transport / optional liveness + admin-tip notify |
| `iroh-docs` | `=0.101.0` | 0.x (actively maintained, "not yet ready for 1.0", **not** deprecated) | D2 candidate sync substrate (whole-history RBSR); pulls blobs+gossip+redb |
| `iroh-blobs` | `=0.103.0` | 0.x, **maintainer-labeled pre-production** | Blob Plane ACL via `provider::events`; alt `=0.35.0` is "production" but on `iroh ^0.35` and lacks events ACL |
| `dumbpipe` | `=0.39.0` | 0.x | Live Pipe Plane (use as library; custom ALPN) |
| `ed25519-dalek` | `=3.0.0-rc.0` | **release candidate** (the version `iroh 1.0` pins) | Event signing; pin the **exact same rc** to avoid duplicate-`Signature`/`VerifyingKey` types |
| `ciborium` | `^0.2` | 0.x | Deterministic CBOR for CSB — **verify it satisfies RFC 8949 §4.2.1; add a canonical-emit wrapper / hardcoded key order if not** |
| `blake3` | `^1` | stable 1.x | `event_id` / `room_id` / capability / device-binding hashes |
| `redb` | (transitive via `iroh-docs`) | 0.x | Embedded KV persistence only if D2 = docs |

**Toolchain confirmation (this spike).** The golden and derived vectors were independently reproduced with `cbor2 (canonical=True)` + `blake3` + Ed25519 (PyNaCl, RFC 8032), confirming the `event_id`, signature, `device_binding.sig`, `room_id_A`, `room_id_B`, tampered-body id, and cross-room re-signed id all match. The Rust implementation MUST reproduce these exact bytes (`ciborium` must equal `cbor2 canonical=True` byte-for-byte).

**Churn-budget note.** Only `iroh` + `iroh-base` carry a stability guarantee; **every higher plane is 0.x on a roughly monthly breaking minor cadence** (blobs `0.97→0.99→0.101→0.103` across Nov 25–Jun 26), and `ed25519-dalek` is a release *candidate*. Budget **~0.5–1 dev-day per minor bump** for recurring upgrade work — plus a larger one-off if `ed25519-dalek` goes 3.0 final or `iroh-blobs` graduates its rewrite line to production. A legitimate tie-breaker for the D2 hand-roll is **"minimize 0.x crates on the load-bearing path"**: docs adds three (docs, gossip, blobs) + redb; the hand-roll keeps the event log on stable core only.
