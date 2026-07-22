# Iroh Rooms Protocol Reference

This document is the **implementer reference** for the Iroh Rooms wire/signature/membership
contract at MVP `schema_version = 1`, exactly as shipped in `crates/iroh-rooms-core`. It exists
so an engineer or agent can build, re-implement, or audit an interoperable peer **without
reading the whole `PHASE-0-SPIKE.md`**.

It is **not**:

- The rationale for these design choices. That is `PHASE-0-SPIKE.md` — every section below
  cites the spike section it condenses; go there for the "why", the rejected alternatives, and
  the adversarial-review reasoning.
- A runnable walkthrough. That is [`docs/getting-started.md`](getting-started.md) — the
  end-to-end copy-pasteable CLI demo. This document owns the byte-level contract; that one
  owns the user journey. They cross-link; they do not duplicate.

## Normative source & precedence

**`PHASE-0-SPIKE.md` is authoritative** for every byte-level value in this document (context
strings, sizes, golden hashes, the verification algorithm). Where this document and the spike
disagree, **the spike wins** and the disagreement is a documentation bug — file it, do not
silently trust this page. The landed code (`crates/iroh-rooms-core/src/{event,membership}/`)
and the conformance suite (`crates/iroh-rooms-core/tests/`) are already reconciled against the
spike, so citing them here is safe; nothing in this document introduces a new rule, constant,
or event type that the spike and shipped code do not already define.

## How to read this doc

Six scope areas, each a self-contained reference section:

1. [Identity & key model](#1-identity--key-model) — the three keys and the device-binding certificate.
2. [Event envelope & wire format](#2-event-envelope--wire-format) — the eight signed fields and the `WireEvent` transport shape.
3. [Canonical serialization rules](#3-canonical-serialization-rules) — the deterministic-CBOR profile.
4. [Event ID & room ID derivation](#4-event-id--room-id-derivation) — the content-hash identifiers.
5. [Signature payload & verification](#5-signature-payload--verification) — the 11-step algorithm.
6. [MVP event-type registry](#6-mvp-event-type-registry) — the ten event types and their content schemas.

Plus [membership & ordering](#7-membership-fold--ordering-summary),
[connect-time authorization](#8-connect-time-authorization-blob--pipe),
[reason codes](#9-rejection--flag-reason-codes),
[test vectors](#10-test-vectors--linked-and-runnable),
[MVP limitations vs. roadmap](#11-mvp-limitations-vs-roadmap), and an
[appendix](#12-appendix--pointers) of cross-references.

Security-critical invariants are called out inline as:

> **Security invariant:** ...

---

## 1. Identity & key model

*Spike: Event Protocol §1. Code: `event/keys.rs`, `event/binding.rs`.*

Three keys participate in every event, all Ed25519 (RFC 8032):

| Concept | Field | Size | Definition |
|---|---|---|---|
| Participant identity | `sender_id` | 32 B pubkey | The stable principal (one per human/agent). **Authorization and membership are tracked against this key.** Never signs events directly in MVP. |
| Device key | `device_id` | 32 B pubkey | The per-device signing key. **`device_id` is byte-for-byte the iroh `EndpointId`** — the same secret is the iroh `Endpoint` secret, the event-signing key, and the blob/pipe-ACL node identity. |
| Room key | `room_id` | 32 B BLAKE3 digest | Cryptographic room identifier; derivation in [§4](#4-event-id--room-id-derivation). |

MVP invariant: exactly **one `device_id` per `sender_id`** (multi-device is deferred — [§11](#11-mvp-limitations-vs-roadmap)).

**Device binding.** Because events are signed by `device_id` but authorized against
`sender_id`, every device must be attested by its identity key:

```
BIND_CONTEXT = "iroh-rooms:device-binding:v1"                (ASCII, 28 bytes, no NUL)
binding_msg  = BIND_CONTEXT ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)
binding_sig  = Ed25519_sign(identity_secret, binding_msg)    // 64 B
```

A verifier accepts a device as belonging to a participant iff
`Ed25519_verify(sender_id, binding_msg, binding_sig)` succeeds. The binding travels in
`room.created` / `member.joined` / `member.removed` content as
`device_binding = { identity_key, device_key, sig }` (`DeviceBinding`, [§6](#6-mvp-event-type-registry)).

> **Security invariant:** the signature over an event verifies under `device_id`, **never**
> `sender_id`; authorization is judged against `sender_id`. The device binding is the *only*
> bridge between the two. Agents are ordinary principals — same keys, same binding rule; only
> `role` differs.

---

## 2. Event envelope & wire format

*Spike: Event Protocol §2, §3. Code: `event/wire.rs`, `event/signed.rs`.*

### The eight signed logical fields

These eight fields, and only these, are covered by the signature and the `event_id` hash:

| Field | CBOR type | Notes |
|---|---|---|
| `schema_version` | uint | MUST be `1`. Unknown versions rejected (`unknown_schema_version`). |
| `room_id` | bstr[32] | Binds the event to one room. |
| `sender_id` | bstr[32] | Participant identity public key. |
| `device_id` | bstr[32] | Signing device public key (= iroh `EndpointId`). |
| `event_type` | tstr | Registered type from [§6](#6-mvp-event-type-registry). Unknown types rejected. |
| `created_at` | uint | Milliseconds since Unix epoch (UTC). Wall-clock. |
| `prev_events` | array of bstr[32] | Causal parents (BLAKE3-256 digests). `[]` only for `room.created`; max 20 entries (`MAX_PREV_EVENTS`). |
| `content` | map | Event-type-specific payload; schemas in [§6](#6-mvp-event-type-registry). |

**There is no `lamport` field on the wire** — the Lamport clock is *derived* from signed
`prev_events` ([§7](#7-membership-fold--ordering-summary)), so it is a pure function of signed
data and there is nothing to lie about. `event_id` and `signature` are deliberately **not**
signed over (an event cannot commit to its own hash).

> **Security invariant:** `created_at` is **advisory/display only** — never trusted for causal
> ordering, authorization, or to mutate the validated set. The one narrow exception is the
> pipe-connect expiry check ([§8](#8-connect-time-authorization-blob--pipe)), which reads a
> *local* clock only to deny. A `created_at` far in the future raises the advisory `clock_skew`
> flag ([§9](#9-rejection--flag-reason-codes)) — it never rejects the event.

### The `WireEvent` transport/storage envelope

To eliminate re-serialization ambiguity, the canonical signed bytes (CSB, [§3](#3-canonical-serialization-rules))
are transported and persisted as an opaque byte string inside an outer deterministic-CBOR map:

```
WireEvent = {
  "v":      1,          // uint, transport envelope version (MUST be 1, `WIRE_VERSION`)
  "signed": bstr,       // == CSB, verbatim
  "sig":    bstr[64],   // Ed25519 signature, §5
  "id":     tstr        // "blake3:<hex>", advisory cache key, §4
}
```

> **Security invariant:** a receiver hashes and verifies **the exact `signed` bytes** — it
> never re-encodes to verify. It then CBOR-decodes `signed` for semantic checks, and separately
> re-canonicalizes the decoded object and rejects if it does not byte-equal `signed`
> ([§3](#3-canonical-serialization-rules)).

> **Security invariant:** `"id"` is a convenience cache key only. It **MUST be recomputed and
> checked, never trusted** — a doctored `"id"` on otherwise-valid `signed`/`sig` bytes is
> rejected with `id_mismatch` (Test Vector §3).

**Presentation rule:** 32-byte IDs (`sender_id`, `device_id`, `room_id`) are lowercase hex (64
chars) in CLI/JSON output. Hash-typed IDs (`event_id`, `prev_events` entries, `blob_hash`) use
the named form `blake3:<64-hex>`.

---

## 3. Canonical serialization rules

*Spike: Event Protocol §3. Code: `event/cbor.rs`.*

The signed-over bytes use **deterministic CBOR (RFC 8949 §4.2.1 Core Deterministic Encoding
Requirements)**. All seven rules MUST hold; a non-conforming encoding is rejected:

1. Map keys sorted in bytewise lexicographic order of their *encoded* form (for these short
   text keys, equivalently length-first then bytewise).
2. Shortest-form integer encoding.
3. Definite-length items only — no indefinite-length strings, arrays, or maps.
4. No duplicate map keys.
5. No CBOR tags. No floating-point values anywhere (use integers — e.g. `progress_pct` is `uint` percent).
6. Text strings are valid UTF-8.
7. The top-level signed object contains **exactly** the eight [§2](#2-event-envelope--wire-format)
   keys — no extras. Forward-compatible fields arrive only by bumping `schema_version`.

The resulting fixed canonical **top-level key order** (implementations MAY hardcode it):

```
content, room_id, device_id, sender_id, created_at, event_type, prev_events, schema_version
```

Call the resulting octet string **CSB** (canonical signed bytes).

> **Security invariant:** canonicality is enforced **independently of the signature**. A
> receiver re-canonicalizes the decoded `signed` bytes and rejects with `non_canonical_encoding`
> if they do not byte-equal the original — even if the signature over the non-canonical bytes
> happens to verify. This keeps event identity 1:1 with logical content regardless of which
> encoder produced the bytes (Test Vector §2).

**Golden anchor:** the reference `message.text` event (Test Vector §1) encodes to a **242-byte**
CSB beginning `a867636f6e74656e74…`. Two encoders (`ciborium` deterministic mode and
`cbor2.dumps(obj, canonical=True)`) — and two independent field orderings of the same logical
event — produce byte-identical output. See [§10](#10-test-vectors--linked-and-runnable).

---

## 4. Event ID & room ID derivation

*Spike: Event Protocol §4, §5. Code: `event/ids.rs`, `event/signed.rs`, `event/genesis.rs`.*

### Event ID

A **named content hash** of CSB — never a sender-chosen ULID/UUID/random value:

```
digest   = BLAKE3-256(CSB)                       // 32 bytes
event_id = "blake3:" ‖ lowercase_hex(digest)
```

Two `WireEvent`s with equal `event_id` are the same event; a receiver keeps the first
validly-signed copy and ignores duplicates ([§9](#9-rejection--flag-reason-codes)).

### Room ID

Derived once by the creator, recomputable by every peer from `room.created`:

```
ROOMID_CONTEXT = "iroh-rooms:room-id:v1"          (ASCII)
room_id = BLAKE3-256( ROOMID_CONTEXT
                      ‖ creator_sender_id(32)
                      ‖ room_nonce(16)             // from room.created content
                      ‖ created_at_be(8) )         // big-endian u64
```

A peer receiving `room.created` MUST recompute `room_id` and reject with `room_id_mismatch` if
it does not equal the envelope `room_id`.

> **Security invariant:** `room_id` is inside the signed bytes, so **cross-room replay is
> cryptographically impossible without re-signing**. Re-broadcasting a verbatim `WireEvent`
> from room A into room B fails `room_id_mismatch` at the room-binding check (verification step
> 6, [§5](#5-signature-payload--verification)); legitimately re-authoring "the same" event in
> room B requires a fresh signature over CSB carrying `room_id_B`, which changes the `event_id`
> too (Test Vector §7).

### Worked example (Test Vector §1/§3/§4/§7 golden values)

A `message.text` event by Alice (creator/admin), `room_id_A`, `content = {"body":"Hello
room","format":"plain"}`, `prev_events=[]`:

| Quantity | Value |
|---|---|
| `room_id_A` (creator seed `01`, nonce `000102…0e0f`, `created_at=1750000000000`) | `43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3` |
| CSB | 242 bytes, `a867636f6e74656e74…01` |
| `event_id` | `blake3:c389e251f9654902d26ea937b3e84a01bb5e5d578e394c95b6ade8b7144e85a1` |
| signature (over `EVENT_CONTEXT ‖ CSB`, key = `device_id`) | `98732ece…4f0f` |
| tampered `content.body` (`"Hello rooM"`) ⇒ `event_id` | `blake3:6267b72c066e30154b34d4430ce8fb735563c4500ff527d371bcc3de7f34c75c` (`id_mismatch` + `bad_signature`) |
| same logical event re-signed for `room_id_B = cad9174a1e34a847711e85968020a5cabaf9b35ed600d21457458f95c9c53494` ⇒ `event_id` | `blake3:81b6a82b4a1e22291102a80a454f935ddbc87d4e15d0de400d6eb6aadb85f057` |

This is a *serialization/signature* fixture (`prev_events=[]`); as a real `message.text` it
would separately fail `not_genesis_descended` (only `room.created` may have empty
`prev_events`). It tests encoding and identity derivation, not causal validity.

---

## 5. Signature payload & verification

*Spike: Event Protocol §6. Code: `event/signed.rs`, `event/validate.rs`.*

```
EVENT_CONTEXT = "iroh-rooms:event:v1"             (ASCII, 19 bytes, no NUL)
sig_msg   = EVENT_CONTEXT ‖ CSB
signature = Ed25519_sign(device_secret, sig_msg)  // 64 B
```

> **Security invariant:** the signature verifies under **`device_id`**, never `sender_id` —
> the classic "verify under the wrong field" bug. Verifying the golden signature under
> `sender_id` (the identity key) instead of `device_id` **fails** (Test Vector §5).

### The 11-step verification algorithm

In spec order — the first applicable failure rejects the event (reason code in brackets); see
the implementation note below for one execution-order refinement. Steps 1–6, 9, 10 are
**stateless** (need only the event's own bytes); steps 7–8 are **stateful** (need the
membership oracle — `crates/iroh-rooms-core/src/event/validate.rs`'s `validate_with_membership`
completes them over `membership::AncestorView`).

1. **Decode transport.** Parse `WireEvent`; reject `v != 1`, missing keys, non-canonical outer map. [`non_canonical_encoding`]
2. **Recompute id.** `id' = "blake3:" + hex(BLAKE3-256(signed))`; reject if `id' != id`. [`id_mismatch`]
3. **Verify signature.** `Ed25519_verify(device_id, EVENT_CONTEXT ‖ signed, sig)`. [`bad_signature`]
4. **Enforce canonicality.** `canonical_cbor(decode(signed)) == signed`, and exactly the eight §2 keys with correct types. [`non_canonical_encoding`]
5. **Version/type + strict content.** `schema_version == 1`; `event_type` registered; unknown content keys / missing / wrong-type / out-of-bound rejected. [`unknown_schema_version`, `unknown_event_type`, `invalid_content`]
6. **Room binding.** For `room.created`, recompute and check `room_id` ([§4](#4-event-id--room-id-derivation)); otherwise `room_id` must match the room being processed. [`room_id_mismatch`]
7. **Device binding** *(stateful)*. Resolve `sender_id`'s authorized device from validated membership state (or the event's own self-contained `device_binding` for the three carrying types); reject unless `device_id` matches. [`unbound_device`]
8. **Membership & role** *(stateful)*. `sender_id` must be a current member with a role permitting `event_type`, judged against the event's own ancestors. [`not_a_member`, `insufficient_role`, `expired_invite`, `bad_capability`]
9. **Causal structure.** `prev_events.len() <= 20`; genesis-descent (9a): non-genesis events MUST have non-empty `prev_events` and transitively reach genesis; `room.created` MUST have `prev_events == []`. Unknown-but-plausible parents are **buffered**, not rejected ([§7](#7-membership-fold--ordering-summary)). [`too_many_parents`, `not_genesis_descended`]
10. **Clock sanity (advisory only).** `created_at` more than `CLOCK_SKEW_FUTURE_MS` (300,000 ms) ahead of local time raises `clock_skew` but never rejects, drops, or reorders. [flag `clock_skew`, never a rejection]
11. **Dedup & persist.** Existing `event_id` ⇒ ignored (`duplicate`, not an error). Otherwise persist the verbatim `WireEvent`.

> **Implementation note:** the shipped `validate_wire_bytes` executes step 4 (canonicality)
> *before* step 3 (signature) — canonicalizing first means Ed25519 verification never runs over
> non-canonical bytes. The numbering above preserves the spike's normative §6 order; only the
> *execution* order of 3/4 differs, and it is a single-parser refinement, not a rule change. For
> an event that is both non-canonically encoded and badly signed, the reference implementation
> reports `non_canonical_encoding` rather than `bad_signature`; every other input's reason code
> is unaffected by which of the two runs first.

> **Implementation note (issue #143):** the sync engine runs a bounded in-memory event-id dedup
> cache *between steps 2 and 3* — after the id is recomputed from `wire.signed` (so steps 1–2
> still reject malformed envelopes and id mismatches first), but before any signature, content,
> membership, or store work. A cache hit on an id the local node has already durably persisted
> short-circuits the rest of the pipeline and is recorded in `SyncCounters::early_duplicates`
> (distinct from the post-store `duplicates` counter, which still covers cache misses, evictions,
> and the cap-0 rollback case). This is a local-only performance guardrail: it changes no wire,
> signature, or persistence rule. Correctness still rests on the store's primary-key idempotency
> (step 11); the cache is seeded only from ids the store has proven persisted, so a bad-signature
> first arrival cannot poison it and suppress a later valid copy. The capacity
> (`SyncConfig::early_event_id_dedup_cache_entries`, default 4096) bounds replay-flood memory;
> `0` disables the early path. The engine also batches consecutive fold-accepted events into one
> `SQLite` transaction per `SyncConfig::store_insert_batch_size` (default 32), preserving the
> ordered post-commit side effects and the #119 retry path on a failed batch.

> **Security invariant:** step 8 authorization is judged against the event's **own fixed
> causal ancestors**, not the receiver's live snapshot — this is what makes verdicts
> arrival-order-independent. Every honest peer reaches the identical verdict for the same event
> regardless of when or in what order it arrives ([§7](#7-membership-fold--ordering-summary)).

---

## 6. MVP event-type registry

*Spike: Event Protocol §7. Code: `event/{genesis,invite,join,left,removed,message,file,pipe,status,content}.rs`.*

Notation: `bstr[n]` = byte string of length n; `tstr` = UTF-8 text; `uint` = unsigned int;
`opt` = optional. `DeviceBinding = { identity_key: bstr[32], device_key: bstr[32], sig: bstr[64] }`
([§1](#1-identity--key-model)).

| `event_type` | Signer / role required | `prev_events` |
|---|---|---|
| `room.created` | Creator's device. No prior membership; creator becomes sole immutable admin. | `[]` (genesis, unique per room) |
| `member.invited` | The **admin**'s device. | room heads |
| `member.joined` | The **joining** member's device; capability-gated. | room heads (must descend from the referenced invite) |
| `member.left` | The member only (**voluntary self-leave**). | room heads |
| `member.removed` | The **admin**'s device (involuntary removal / kick). | room heads |
| `message.text` | Any current member. | room heads |
| `file.shared` | Any current member. | room heads |
| `pipe.opened` | Any current member (`owner_id == sender_id`). | room heads |
| `pipe.closed` | The pipe owner or the admin. | room heads |
| `agent.status` | Any current member (typically `role == "agent"`). | room heads |

> **Removal is two distinct types**, not one type with a `reason` discriminator:
> `member.left` (voluntary, signer == subject) and `member.removed` (admin kick, signer ==
> admin). This keeps the authorization gate trivial and precise.

> Strict content validation rejects unknown content keys, missing/wrong-typed/out-of-bound
> fields as `invalid_content`. Forward-compatible fields arrive only by bumping
> `schema_version` — never as a silently-ignored extra key.

> **v2.0 forward design (not shipping).** The `schema_version = 2` content-kind
> registry (the finer-grained successor of the table above), the stream-scoped
> moderation events `moderation.block` / `moderation.report` / `moderation.remove`,
> and the `unknown_content_kind` rejection that extends the "reject, never ignore"
> rule to the content-kind discriminant are specified in
> [`specs/content-and-moderation-event-schemas.md`](../specs/content-and-moderation-event-schemas.md)
> (issue #158, parent #134 §25 #4). It is a **design document only**: under the
> shipped code every v2.0 event is still rejected `unknown_schema_version` until
> the P-26 / Decision D-9 schema-evolution ADR
> (`docs/audits/feature-complete-audit-2026-07-02.md`) lands. No code, wire
> format, or compatibility promise here changes today.

### Content schemas (condensed; bounds are the `constants.rs` values, [§9](#9-rejection--flag-reason-codes))

**`room.created`**
`room_name: tstr` · `room_nonce: bstr[16]` · `admins: [bstr[32]]` (MUST be exactly `[sender_id]`
in MVP) · `device_binding: DeviceBinding` (`identity_key == sender_id`, `device_key == device_id`).

**`member.invited`**
`invite_id: bstr[16]` · `capability_hash: bstr[32]` · `role: tstr` (`member`\|`agent`\|`admin`) ·
`invitee_key: bstr[32]` (**REQUIRED** — key-bound, [§7](#7-membership-fold--ordering-summary)) ·
`expires_at: opt uint` · `invitee_hint: opt tstr` (non-authoritative label).

```
INVITE_CONTEXT   = "iroh-rooms:invite:v1"                          (ASCII)
capability_hash  = BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)
```

`secret` is ≥16 bytes and travels out-of-band in the invite ticket only, never on the wire —
a leaked `device_id` alone cannot forge a join.

**`member.joined`**
`via_invite_id: bstr[16]` · `capability_secret: bstr[16]` (recompute `capability_hash` with it
and match the cited invite's, else `bad_capability`) · `role: tstr` (MUST equal the invite's
role) · `device_binding: DeviceBinding` · `display_name: opt tstr`.

**`member.left`**
`member_id: bstr[32]` (MUST `== sender_id`) · `reason: opt tstr` (display only).

**`member.removed`**
`member_id: bstr[32]` (MUST `!= admin`) · `removed_by: bstr[32]` (MUST `== sender_id`) ·
`reason: opt tstr` · `device_binding: opt DeviceBinding` (re-attestation of the admin's own
device; verified when present, [§1](#1-identity--key-model)).

**`message.text`**
`body: tstr` (≤ `MAX_MESSAGE_BODY_BYTES` = 16,384 bytes) · `format: opt tstr` (`plain` default \|
`markdown`) · `in_reply_to: opt bstr[32]` · `mentions: opt [bstr[32]]`.

**`file.shared`**
`file_id: bstr[16]` · `name: tstr` (≤ `MAX_FILE_NAME_BYTES` = 255) ·
`mime_type: tstr` (≤ `MAX_MIME_TYPE_BYTES` = 255) · `size_bytes: uint` ·
`blob_hash: bstr[32]` (BLAKE3-256; verify fetched bytes against it) ·
`blob_format: opt tstr` (`raw` default \| `hash_seq`) ·
`providers: opt [bstr[32]]` (≤ `MAX_FILE_PROVIDERS` = 16, default `[device_id]`).

**`pipe.opened`**
`pipe_id: bstr[16]` · `owner_id: bstr[32]` (MUST `== sender_id`) · `owner_endpoint: bstr[32]`
(== owner's `device_id`) · `kind: tstr` (`"tcp"` only in MVP) · `label: tstr` ·
`target_hint: tstr` (advisory only) · `alpn: tstr` ·
`allowed_members: [bstr[32]]` (non-empty — **no default-all**) · `expires_at: opt uint`.

**`pipe.closed`**
`pipe_id: bstr[16]` (references an open `pipe.opened`) ·
`reason: opt tstr` (`"closed"`\|`"expired"`\|`"owner_exit"`\|`"error"`).

**`agent.status`**
`status: tstr` (≤ `MAX_STATUS_LABEL_BYTES` = 64, e.g. `"running"`\|`"blocked"`\|`"done"`) ·
`message: opt tstr` (≤ `MAX_STATUS_MESSAGE_BYTES` = 4,096) ·
`related_artifact_ids: opt [bstr[16]]` (≤ `MAX_ARTIFACT_REFS` = 16) ·
`progress_pct: opt uint` (0..=100, integer — no floats).

### Structural sizes (from `event/constants.rs`)

| Constant | Value |
|---|---|
| `PUBLIC_KEY_LEN` | 32 |
| `SIGNATURE_LEN` | 64 |
| `DIGEST_LEN` | 32 |
| `SHORT_ID_LEN` | 16 |
| `MAX_PREV_EVENTS` | 20 |
| `CLOCK_SKEW_FUTURE_MS` | 300,000 |
| `MAX_SHARED_FILE_BYTES` | 104,857,600 (100 MiB) |
| Crypto | Ed25519 (RFC 8032), BLAKE3-256, deterministic CBOR (RFC 8949 §4.2.1) |

---

## 7. Membership fold & ordering summary

*Spike: Membership & Ordering §0–§4, §7. Code: `membership/fold.rs`, `membership/model.rs`.*

This is a **summary**; the spike sections above hold the full derivation and adversarial
review. Read them if you need the "why", not just the "what".

### The convergence guarantee (hedged form only)

> **Any two honest peers that hold the IDENTICAL validated event set compute byte-identical
> membership state and timeline** — regardless of arrival order, restarts, or equivocation.

> **Security invariant:** the **unqualified** claim "equivocation cannot cause divergence" is
> **false and must not be used**. Convergence is *conditional on set-completeness*; the
> protocol mechanically guarantees only **ancestor-completeness** (you can always backfill
> `prev_events` to genesis), never **sibling/concurrent-completeness** (you cannot discover a
> withheld concurrent event — e.g. a removal held only by an offline peer — by following
> pointers alone).

### Derived Lamport clock and total order

```
lamport(genesis) = 0
lamport(e)       = 1 + max(lamport(p) for p in prev_events)
```

Recomputed by every peer from signed `prev_events` — not on the wire. **Total order** =
ascending `(lamport, event_id)`, `event_id` compared bytewise over its 32 raw digest bytes.
Since `a → b` (happens-before) implies `lamport(a) < lamport(b)`, the comparator is always a
linear extension of the causal DAG.

> **Security invariant:** timeline position carries **no trust**. An author can grind `content`
> so `BLAKE3(CSB)` sorts first/last, or pick `prev_events` to control their `lamport`. This
> never breaks convergence (every peer agrees on the pinned position) but **no logic or UI may
> attach meaning to position** ("first", "pinned", "top").

### The fold (per subject X)

1. Collect all **valid, authorized** events touching X (validity judged from each event's own
   ancestors — never the live snapshot).
2. Find the **causal heads** of that set.
3. **Status** (Removed dominates): any head is `member.removed`/`member.left` ⇒ **Removed**;
   else a head is `member.joined` ⇒ **Active**; else (invite only) ⇒ **Invited**.
4. **Attributes**: least-privilege merge — `role` resolves to the *least-privileged* value
   present (`agent < member < admin`); capability scope resolves to the *intersection*; ties
   broken by the **lowest `event_id`** (bytewise). This is the one rule that closes the
   otherwise-open same-set divergence for concurrent conflicting grants (Test Vector §18).

### Authorization gate & sticky departure

- `member.invited(X)`, `member.removed(X)`: valid iff signer `== admin` (and for removal,
  `X != admin`).
- `member.left(X)`: valid iff signer `== X`.
- `member.joined(X)`: valid iff signer is `X`'s device key **and** it causally descends from a
  **still-live** admin invite for `X` — one that has not been invalidated by a
  `member.removed(X)` or `member.left(X)` lying causally between the invite and the join.

> **Security invariant:** departure is **sticky** — any `member.removed(X)` or `member.left(X)`
> causally following an admin authorization **consumes** it. A stale pre-departure invite (or
> a replayed old join) can never resurrect a departed member; only a **new** admin invite,
> issued causally **after** the departure, plus a join descending from it, can re-admit them
> (Test Vector §19). Combined with key-bound invites ([§6](#6-mvp-event-type-registry)), ban
> evasion under a fresh key is impossible.

### Out-of-order delivery

Child-before-parent is **buffered and backfilled**, never rejected for "unknown parent". A
three-stage pipeline (structural/crypto → causal readiness with anti-amplification bounds
— signer pre-check, park caps, backfill quota — → semantic/authorization) guarantees no
permanent divergence for peers that eventually hold the same set.

### Snapshot & access predicates

The shipped `MembershipSnapshot` (`membership/model.rs`) exposes per-identity `status`/`role`/
bound `device`, and the access predicates `blob_serve_allowed` / `pipe_connect_allowed`
(`membership/access.rs`) consult the **current** snapshot, not the ancestor view —
[§8](#8-connect-time-authorization-blob--pipe) explains why that split matters.

---

## 8. Connect-time authorization (blob & pipe)

*Spike: Membership & Ordering §5. Code: `membership/access.rs`.*

Enforcement happens at QUIC connect-accept time, evaluated against the **current local
membership snapshot** (the fold over the whole local validated log), default-deny. If the
`admin_seq` incompleteness detector is tripped (a known-higher admin tip not yet backfilled, or
a same-`admin_seq` fork), the node **fails closed** on affected subjects and raises a CRITICAL
`equivocation` alert on the fork case.

**Blob serve gate.** Accept a connect only from a QUIC/TLS-proven identity that is currently
`Active`; serve a hash only if it is referenced by a valid, causally-visible `file.shared`
authored by an `Active` member; verify fetched bytes' BLAKE3-256 against `blob_hash`.

**Pipe connect gate.** Admit only if ALL hold against the local snapshot:
1. remote identity ∈ `Active` members,
2. remote identity ∈ the pipe's `allowed_members` (no default-all),
3. the governing `pipe.opened` was authored by its `Active` owner,
4. no `pipe.closed` for that `pipe_id` is causally known,
5. `expires_at` (if present) `>=` local wall clock (the boundary `now == expires_at` still
   admits; only `now > expires_at` denies).

> **Security invariant:** step 5 is the **one place** a local wall clock is consulted, and only
> to **deny** (fail-closed) — never to accept an otherwise-invalid connection.

**Revocation-on-learn.** Live connections are torn down as soon as the enforcing peer learns of
a removal, `pipe.closed`, or expiry. Exposure is bounded by *removal-event reachability*, not by
"briefly" — see [§11](#11-mvp-limitations-vs-roadmap).

> **Security invariant:** a since-removed member's log-valid events (chat, `file.shared`,
> `pipe.opened`) may still be *valid for log inclusion* (they were `Active` in their own
> ancestor view — the log is append-only) but grant **zero capabilities**, because access
> control uses the **current global snapshot**, not the ancestor view. This is what contains an
> equivocating, ignore-my-own-kick member: they can keep writing to their own fork but get no
> pipe/blob access and converge to `Removed` everywhere. The UI flags such events with the
> advisory `from_removed_member` ([§9](#9-rejection--flag-reason-codes)).

---

## 9. Rejection / flag reason codes

*Spike: Event Protocol §8. Code: `event/reject.rs`.*

The exact `.code()` spellings — do not invent variants:

### Rejections (15 — event dropped, never persisted or re-broadcast)

| Code | Meaning | Layer |
|---|---|---|
| `unknown_schema_version` | `schema_version != 1` | stateless |
| `unknown_event_type` | `event_type` not in [§6](#6-mvp-event-type-registry)'s registry | stateless |
| `non_canonical_encoding` | Non-canonical CBOR, wrong top-level key set, or malformed transport | stateless |
| `id_mismatch` | Advisory `id` != recomputed `event_id` | stateless |
| `bad_signature` | Ed25519 verification under `device_id` failed | stateless |
| `room_id_mismatch` | Recomputed genesis id, or the processing room, disagrees | stateless |
| `invalid_content` | Unknown content key, missing/wrong-type/out-of-bound field, bad embedded `device_binding` | stateless |
| `too_many_parents` | `prev_events.len() > MAX_PREV_EVENTS` (20) | stateless |
| `not_genesis_descended` | Non-genesis event with empty `prev_events`, or `room.created` with non-empty `prev_events` (full transitive check deferred) | stateless |
| `unbound_device` | `sender_id` has no device bound to it in membership state | deferred (membership) |
| `not_a_member` | `sender_id` is not a current member of the room | deferred (membership) |
| `insufficient_role` | `sender_id`'s role does not permit `event_type` | deferred (membership) |
| `expired_invite` | A cited invite was consumed (sticky departure) or its `expires_at` passed | deferred (membership) |
| `bad_capability` | A join's `capability_secret` did not reproduce the invite's hash, or no such invite exists in the ancestor view | deferred (membership) |
| `room_full` | Accepting a join would exceed the supported active-member ceiling | deferred (membership) |

### Ignored (not an error)

| Code | Meaning |
|---|---|
| `duplicate` | `event_id` already stored; the copy is ignored idempotently, state and timeline unchanged |

> **Observability note (issue #143):** `duplicate` is reported by the engine as **two disjoint
> counters**, not one. `SyncCounters::early_duplicates` counts replays the in-memory event-id
> cache caught before signature verification or any store work; `SyncCounters::duplicates` counts
> duplicates the store's primary-key idempotency arm returned after the full validation path
> (cache miss, eviction, or `SyncConfig::early_event_id_dedup_cache_entries == 0`). Their sum is
> the total duplicate count. Neither counter is an error and neither changes timeline state.

### Advisory flags (event still accepted, ordered, and persisted)

| Code | Meaning |
|---|---|
| `clock_skew` | `created_at` more than `CLOCK_SKEW_FUTURE_MS` ahead of local time |
| `equivocation` | The signer authored two mutually-concurrent events |
| `from_removed_member` | UI attribution: the author has since converged to `Removed` |

> **Security invariant:** advisory flags **never** affect the validated set, ordering, or any
> authorization/expiry verdict. A `clock_skew` event is still accepted, validated, ordered, and
> persisted exactly like any other (Test Vector §20). Likewise, `duplicate` is **ignored**
> (idempotent), never treated as an error.

### Surfacing to a CLI user

The `iroh-rooms` binary wraps `RejectReason::code()` / `Flag::code()` verbatim into its exit
taxonomy (`README.md` "Error codes"):

| Exit | Category | Example codes |
|---|---|---|
| `0` | — | success |
| `1` | Internal | `internal`, any uncoded failure |
| `2` | Usage | `invalid_room_id`, `invalid_argument`, `room_not_found`, ... |
| `3` | Auth | `not_a_member`, `unbound_device`, `insufficient_role`, `expired_invite`, `bad_capability`, `peer_unauthorized` |
| `4` | Integrity | `bad_signature`, `id_mismatch`, `non_canonical_encoding`, `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`, `not_genesis_descended`, `room_id_mismatch`, `hash_mismatch` |
| `5` | Ticket | `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`, ... |
| `6` | Connectivity | `no_admin_reachable`, `peer_offline`, `blob_unavailable` |

The split is **verbatim**: a `room join` failure and a `room tail` receive-path drop of the same
underlying event report the identical code. A `warning[clock_skew]` on `room tail` is never a
failure and never a non-zero exit.

---

## 10. Test vectors — linked and runnable

*Spike: Protocol Test Vectors §1–§20. Suite: `crates/iroh-rooms-core/tests/`.*

The 20 normative vectors are implemented one `#[test]` per vector, plus a broader
regression/negative corpus. Run them yourself:

```sh
# Full §-indexed conformance suite (82 tests as of this writing, incl. this doc's own drift gate)
cargo test -p iroh-rooms-core --test protocol_conformance --all-features

# Golden-value regression grab-bag (the original IR-0002 landing, kept as cheap insurance)
cargo test -p iroh-rooms-core --test golden_vectors

# Full project gate (fmt + clippy -D warnings + all tests, all features)
scripts/verify.sh
```

### Golden-value tiers

- **Tier 1 — independently reproduced (authoritative).** The cast public keys, the 242-byte
  golden CSB + its `event_id`, signature, `room_id_A`/`room_id_B`, the tampered id, and the
  cross-room re-signed id ([§4](#4-event-id--room-id-derivation)) are asserted against the exact
  spike hex. A mismatch here is a hard NO-GO.
- **Tier 2 — regenerated & pinned.** The multi-event fixture-log ids (`E_create … E_pipe`) were
  not independently reproduced in the spike (their content maps were never pinned there), so
  `tests/conformance/fixtures.rs` regenerates them from the landed content schema and pins the
  result as a regression tripwire.

### Vector → test function → module

| Vector | Test fn | Module |
|---|---|---|
| §1  canonical determinism | `vector_01_canonical_serialization_determinism` | `serialization` |
| §2  non-canonical rejected | `vector_02_non_canonical_encoding_rejected` | `serialization` |
| §3  `event_id` recomputed | `vector_03_event_id_is_recomputed` | `serialization` |
| §4  `room_id` bound (genesis) | `vector_04_room_id_derivation_bound` | `serialization` |
| §5  signature under device key | `vector_05_signature_under_device_key` | `serialization` |
| §6  tamper ⇒ id+sig fail | `vector_06_tampered_field_breaks_id_and_signature` | `serialization` |
| §7  cross-room replay | `vector_07_cross_room_replay_rejected` | `serialization` |
| §8  duplicate idempotency | `vector_08_duplicate_ignored_idempotently` | `idempotency_ordering` |
| §9  out-of-order buffering | `vector_09_child_before_parent_buffered` | `idempotency_ordering` |
| §10 total order | `vector_10_deterministic_total_order` | `idempotency_ordering` |
| §11 concurrent join/kick | `vector_11_concurrent_join_kick_removed` | `membership` |
| §12 equivocation | `vector_12_admin_equivocation_flagged` | `advisory` |
| §13 non-member rejected | `vector_13_non_member_event_rejected` | `membership` |
| §14 insufficient role | `vector_14_insufficient_role_rejected` | `membership` |
| §15 stale invite / bad cap | `vector_15_bad_capability_and_expired_invite` | `membership` |
| §16 blob serve gate | `vector_16_blob_serve_gate` | `membership` |
| §17 pipe connect gate | `vector_17_pipe_connect_gate` | `membership` |
| §18 concurrent attributes | `vector_18_concurrent_attributes_least_privilege` | `membership` |
| §19 leave then rejoin | `vector_19_leave_consumes_invite` | `membership` |
| §20 clock skew advisory | `vector_20_clock_skew_advisory_only` | `advisory` |

All modules live under `crates/iroh-rooms-core/tests/conformance/`
(`serialization.rs`, `idempotency_ordering.rs`, `membership.rs`, `advisory.rs`, `fixtures.rs`).

### Taxonomy completeness gate

`tests/conformance/taxonomy.rs`'s `every_reason_and_flag_is_covered_or_deferred` asserts every
`RejectReason` / `Flag` in [§9](#9-rejection--flag-reason-codes) is covered by a named vector or
an explicit (empty) `DEFERRED` list. A new reason/flag code cannot land without a vector, so this
document's §9 table cannot silently fall out of date without CI catching it first.

---

## 11. MVP limitations vs. roadmap

*Spike: §0, §3.6, §5, §6. PRD: §13.4/§13.5.*

| MVP limitation (`schema_version = 1`, today) | Long-term / roadmap (deferred) |
|---|---|
| **Single immutable admin** — the genesis signer, no co-admins, no transfer (spike §3.1, §7). | Multi-admin as a grow-only/quorum CRDT or a signed successor list (post-MVP). |
| **One device per identity** — multi-device out of scope (spike §1). | A generalized device set per identity. |
| **No key rotation.** Removal changes membership state but cannot cryptographically erase already-received data; enforcement is fail-closed-at-connect + tear-down-on-learn, **bounded by removal-event reachability**, not "briefly" (spike §3.6, §0). | Key rotation; member removal with rotation (PRD §13.5). |
| **Key-bound invites only** — open/bearer tickets are excluded because they defeat sticky-kick (a banned party could mint a join under a fresh key). No native revocation beyond removing the subject; `max_uses` is not convergently enforceable, so an invite is treated as single-subject, reusable until expiry (spike §6). | Admin-signed `cap_id` revocation events; first-key-binding for open tickets; invite revocation (PRD §13.5). |
| **Removed-member timeline pollution** — a removed member can keep authoring log-valid, zero-capability events by citing only pre-removal ancestors (spike §5). | UI hard-segregation of `from_removed_member` events from pickers/listings; an admin-signed tombstone (recommended, non-blocking). |
| **Segregated admin fork** is detectable only via admin-tip advertisement; a removal held only by offline/withholding peers is irreducible without an availability assumption (spike §0). | A stronger availability assumption or an always-on relay/witness set. |
| **Recent-history chat sync is count/time-bounded**, but the membership sub-DAG plus the full admin chain are **never windowed** — a hard invariant, not a limitation to fix (spike §0, §4). | N/A — this stays a hard invariant even as chat windowing policy evolves. |
| No full group E2EE ratchet, no perfect forward secrecy, no secure multi-device recovery, no anonymous credentials (PRD §13.4). | Security roadmap: device verification, encrypted local database, recovery phrase, secure backup, trust levels for agents, room-level pipe policies, storage encryption, security review before public beta (PRD §13.5). |

---

## 12. Appendix / pointers

| Doc section | Spike section | Implementing module(s) | Conformance test(s) |
|---|---|---|---|
| §1 Identity & key model | Event Protocol §1 | `event/{keys,binding}.rs` | `serialization` (binding checks) |
| §2 Envelope & wire format | Event Protocol §2–§3 | `event/{wire,signed}.rs` | §1–§3 |
| §3 Canonical serialization | Event Protocol §3 | `event/cbor.rs` | §1, §2 |
| §4 Event ID & room ID | Event Protocol §4–§5 | `event/{ids,genesis}.rs` | §3, §4, §7 |
| §5 Signature & verification | Event Protocol §6 | `event/{signed,validate}.rs` | §5, §6 |
| §6 Event-type registry | Event Protocol §7 | `event/{genesis,invite,join,left,removed,message,file,pipe,status,content}.rs` | `serialization` (`invalid_content_*` / `valid_*` vectors) |
| §7 Membership fold & ordering | Membership & Ordering §0–§4, §7 | `membership/{fold,model}.rs` | §10–§12, §18, §19 |
| §8 Connect-time authorization | Membership & Ordering §5 | `membership/access.rs` | §16, §17 |
| §9 Reason codes | Event Protocol §8 | `event/reject.rs` | `taxonomy.rs` |
| §10 Test vectors | Protocol Test Vectors §1–§20 | `tests/conformance/*`, `tests/golden_vectors.rs` | all |
| §11 MVP limitations | Spike §0, §3.6, §5, §6; PRD §13.4/§13.5 | — | — |

**Further reading:**

- [`PHASE-0-SPIKE.md`](../PHASE-0-SPIKE.md) — the full normative source, ADRs, and residual-risk analysis.
- [`PRD.v0.3.md`](../PRD.v0.3.md) §18.6 (Protocol Ambiguity Risk), §11 (product-level envelope view), §13/§14 (security & availability model).
- [`specs/content-and-moderation-event-schemas.md`](../specs/content-and-moderation-event-schemas.md) — v2.0 design (not shipping) for the content-kind registry, stream-scoped moderation events, and `unknown_content_kind` rejection; gated on the D-9 schema-evolution ADR (P-26).
- [`docs/getting-started.md`](getting-started.md) — the runnable end-to-end demo.
- `crates/iroh-rooms-core/` — the reference implementation.
- `crates/iroh-rooms-core/tests/conformance/` — the conformance suite.

> The PRD §11.1 envelope (JSON with a top-level `event_id`/`signature`) is a simplified,
> product-level illustration. This document follows the spike's normative eight-field signed
> envelope plus the `WireEvent` outer shape ([§2](#2-event-envelope--wire-format)); treat PRD
> §11 as documentation shape, not the wire contract.
