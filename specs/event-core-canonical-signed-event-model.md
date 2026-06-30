# Spec: Event-Core Canonical Signed Event Model

| | |
|---|---|
| **Issue** | #6 — [IR-0002] Implement event-core canonical signed event model |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/feature, type/security, area/protocol, priority/p0, risk/high |
| **Traceability** | `PRD.v0.3.md` §10.1–§10.3, §11.1, §18.6 · `PHASE-0-SPIKE.md` Event Protocol §1–§8, Test Vectors §1–§8, Spike Plan Gate B |
| **Status** | Implemented — merged via issue #6 |
| **Type** | Production code (new module in `iroh-rooms-core`) + tests. No CLI, transport, persistence, or membership-fold code. |

---

## 1. Summary

Implement the **single-event trust boundary** of the Room Event Plane: the canonical
signed event model. This is the load-bearing, byte-for-byte-correct core that every
other plane (messages, membership, files, pipes, agent status) rides on. It delivers:

- A typed **logical signed event** over the exact eight signed fields (`PHASE-0-SPIKE.md`
  Event Protocol §2).
- **Deterministic CBOR** canonical serialization (CSB — canonical signed bytes) per
  RFC 8949 §4.2.1 (§3).
- **BLAKE3-256** event-ID derivation as a named content hash (`blake3:<hex>`) (§4).
- **Room-ID** derivation/recomputation for `room.created` (§5).
- **Ed25519** signature verification under **`device_id`** (never `sender_id`) (§6).
- The **`WireEvent`** transport/storage envelope with verbatim signed-byte preservation (§3).
- **Strict schema + content validation**: known `schema_version`, registered `event_type`,
  per-type content schemas with **unknown-key rejection**, and the self-contained
  `device_binding` signature check (§7).
- A stable **rejection/flag taxonomy** for every outcome this layer can produce (§8).

This issue implements the **stateless** verification surface — every check in Event
Protocol §6 that depends **only on the event's own bytes**. It deliberately does **not**
implement membership-state resolution, role authorization, causal ordering, sync, the
SQLite store, or transport. Those are sibling issues under epic #1 (see §4 Out of Scope).

The deliverable is the acceptance oracle for the whole protocol: if these bytes are wrong,
everything downstream is wrong. This is why the issue is `priority/p0 risk/high` and why
the golden test vectors are normative.

---

## 2. Background & current repository state

Read before implementing:

- **`PHASE-0-SPIKE.md`** — the normative protocol contract. Specifically:
  - **Event Protocol §1** (identity/key model: `sender_id` vs `device_id`, `device_id` ==
    iroh `EndpointId`, the `device_binding` device certificate and `BIND_CONTEXT`).
  - **§2** (the eight signed logical fields and their CBOR types; `event_id`/`signature`
    are NOT signed-over; there is no `lamport` field on the wire — golden envelope is `map(8)`).
  - **§3** (deterministic-CBOR profile; the fixed canonical top-level key order; the
    `WireEvent` envelope `{v, signed, sig, id}`).
  - **§4** (event-ID = `"blake3:" + hex(BLAKE3-256(CSB))`; `id` is advisory, always recomputed).
  - **§5** (room-ID derivation `BLAKE3-256(ROOMID_CONTEXT ‖ sender_id ‖ room_nonce ‖ created_at_be)`).
  - **§6** (the 11-step verification algorithm; this issue implements the stateless subset).
  - **§7** (MVP event-type registry + per-type content schemas + `DeviceBinding`).
  - **§8** (rejection / flag taxonomy — the stable reason codes).
  - **Test Vectors §1–§8** (byte-exact golden vectors, independently reproduced in the spike).
  - **Spike Plan, Days 2–3 + Gate B** (the implementation order and GO/NO-GO bar this issue must clear).
- **`PRD.v0.3.md`** — §10.1 (event identity), §10.2 (canonical serialization), §10.3
  (signature model), §11.1 (event envelope example shape — *documentation* shape, not the
  signed wire form), §18.6 (protocol-ambiguity risk + its five mitigations).
- **`crates/iroh-rooms-core/src/lib.rs`**, **`Cargo.toml`**, **`scripts/verify.sh`**, **`CONTRIBUTING.md`**.

**Critical current-state facts:**

1. **`iroh-rooms-core` is a placeholder.** `src/lib.rs` exposes only `PROTOCOL_VERSION: u16 = 1`
   and a trivial test. The doc-comment already states the next milestone is "the signed event
   model described in `PHASE-0-SPIKE.md`." There are **no dependencies** in its `Cargo.toml`.
2. **The workspace lints are strict.** Root `Cargo.toml` sets `unsafe_code = "forbid"` and
   Clippy `all` + `pedantic` at `warn`; `scripts/verify.sh` runs `clippy … -D warnings`, so
   **all pedantic lints are effectively deny** in CI. New code must be pedantic-clean.
3. **`scripts/verify.sh` is the gate:** `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets --all-features -D warnings`, `cargo test --workspace --all-targets
   --all-features`. The new tests and benches must pass under it.
4. **The golden vector in §1 is a *serialization* fixture, not a causally-valid event.** It is
   a `message.text` with `prev_events = []`, which a full live pipeline would reject at the
   genesis-descent structural check. It exists to pin CSB bytes, `event_id`, and the signature
   — test the crypto/serialization primitives against it directly (see §10.3).
5. **Fixture-log IDs are partly un-reproduced.** The spike independently re-derived the golden
   `event_id`, signature, `device_binding.sig`, `room_id_A/B`, the tampered-body id, and the
   cross-room re-signed id. It did **not** independently reproduce the multi-event fixture-log
   ids (`E_create … E_pipe`, `E_eq_a/b`, `E_mal`) — their full content maps are not pinned.
   The conformance harness must **regenerate** those from the final content schema before
   trusting them as golden (see §12 Risks).

---

## 3. Goals, non-goals, and scope

### 3.1 In scope (this issue)

The **stateless single-event model and validator**:

1. Domain newtypes: identity/device public keys, signatures, named-hash IDs
   (`EventId`, `RoomId`, `HashRef`), with hex / `blake3:<hex>` presentation.
2. The `SignedEvent` logical struct over the eight §2 fields.
3. Deterministic-CBOR encoder producing **CSB** (the canonical signed bytes).
4. A strict canonical-CBOR reader/validator enforcing the §3 profile on inbound bytes.
5. `event_id` derivation (§4) and `room_id` derivation/recompute (§5).
6. Ed25519 sign + verify over `EVENT_CONTEXT ‖ CSB`, verifying under `device_id`.
7. The `WireEvent` envelope `{v, signed, sig, id}` with **verbatim** `signed`-byte
   preservation for storage/forwarding, encode + decode + recompute-and-check `id`.
8. The §7 event-type registry and **strict per-type content validation** (typed content,
   required/optional keys, length bounds, enum checks, **unknown content keys rejected**).
9. Self-contained `device_binding` signature verification (§1) for the three content types
   that carry it (`room.created`, `member.joined`, `member.removed`).
10. **Stateless structural causal checks**: `prev_events` length ≤ 20; non-genesis events
    MUST have non-empty `prev_events`; `room.created` MUST have empty `prev_events`.
    (NOTE: full *transitive* genesis-descent reachability needs the DAG/store and is deferred.)
11. The §8 rejection-reason enum + advisory-flag enum, and the stateless `created_at`
    clock-skew **advisory** flag (§6 step 10) — flag only, never reject.
12. Golden-vector and negative-corpus conformance tests covering every §8 outcome this
    layer can produce.

### 3.2 Out of scope (sibling issues under epic #1 — do NOT implement here)

- **Membership fold / authorization gate** (§6 steps 7–8; §3.4–§3.8): resolving
  `sender_id → bound device_id` from membership state, role checks, `not_a_member` /
  `insufficient_role` / `expired_invite` / `bad_capability` verdicts. The validator exposes
  a typed hook/trait boundary for these but does not decide them.
- **Causal ordering** (derived Lamport clock, `(lamport, event_id)` total order).
- **Transitive genesis-descent** (§6 step 9a full reachability), **out-of-order buffering /
  backfill / anti-amplification** (§4 of the ordering model).
- **Sync / transport** (full-mesh QUIC, ALPN, admin-tip).
- **SQLite persistence** (`events`, `members`, `sync_state`). This issue defines the
  *bytes to be stored* (verbatim `WireEvent`) but writes no schema.
- **Equivocation detection / admin-tip advertisement.**
- **Dedup-and-persist** (§6 step 11): dedup needs a store; this issue guarantees only that a
  validated event yields a stable `event_id` key and the verbatim bytes to dedup on.

### 3.3 Why the split is safe

Event Protocol §0/§3.3 establishes that validity is **ancestor-stable** and the canonical
bytes are **transport-agnostic**. The stateless layer (this issue) is a pure function of the
event's own bytes; the stateful layer (siblings) is a pure function of the validated set.
Implementing the stateless core first gives a frozen, conformance-tested foundation the
stateful layers build on without re-touching serialization or crypto.

---

## 4. Key design decisions

### D1 — Purpose-built deterministic CBOR codec (recommended) vs. a general CBOR library

This is a **trust-boundary serializer**; ambiguity here is exactly the PRD §18.6 risk. The
spike (Day 2 known risk) flags that `ciborium` is not guaranteed to emit RFC 8949 §4.2.1
deterministic encoding by default, and §3 additionally requires a **strict inbound check**
(reject indefinite-length, non-shortest ints, unsorted/duplicate keys, tags, floats).

**Recommendation:** implement a small, purpose-built canonical-CBOR module that:
- **emits** canonical CSB for the eight known top-level fields and the closed set of content
  schemas (a fixed key order MAY be hardcoded — §3 permits this), and
- **validates** inbound CBOR against the §3 profile *while parsing* (a strict reader that
  rejects any non-canonical encoding), which is equivalent to "decode → re-encode →
  byte-equal" but yields a precise `non_canonical_encoding` rejection and avoids trusting a
  third-party library's determinism.

A general decoder MAY back the strict reader (e.g. `ciborium` for the parse tree) **only if**
followed by the explicit profile checks; the canonical **encoder** must not depend on library
default ordering. The decoded-then-re-canonicalized bytes MUST byte-equal `signed` (§3).

*Alternative considered:* `ciborium` end-to-end with a canonical wrapper + post-hoc checks.
Acceptable, but puts library-default behavior on the trust boundary; prefer the purpose-built
codec for the signed bytes. **Pick one and pin it; document the choice in the module.**

### D2 — Ed25519 / BLAKE3 crate selection (pin to the iroh-compatible stack)

- **Ed25519:** use `ed25519-dalek` **pinned to the version iroh 1.0 uses** (`3.0.0-rc.0` per
  the spike). Rationale: `device_id` is byte-for-byte the iroh `EndpointId`
  (`iroh_base::EndpointId == ed25519 PublicKey`); matching the exact crate/version guarantees
  byte and behavior compatibility (incl. RFC 8032 verification semantics) when transport lands.
- **BLAKE3:** the `blake3` crate (BLAKE3-256, 32-byte output). The same hash the Blob Plane uses.
- **Hex:** `hex` (or `data-encoding`) for lowercase hex; no `0x`/uppercase on the wire.
- **Errors:** `thiserror` for the rejection enum (or a hand-rolled enum — keep deps minimal).
- Do **not** add `iroh` itself as a dependency in this issue; the key types are plain Ed25519.
  Document the `device_id == EndpointId` invariant in code so the transport issue wires it up.

### D3 — Verification API shape: a stateless validator returning a typed result

Expose a single entry point that performs the stateless subset of §6 and returns either a
validated, decoded event (carrying its verbatim `signed` bytes + recomputed `event_id`) or a
typed `RejectReason`, plus any advisory `Flag`s. The membership/role decision (steps 7–8) is
expressed as a **trait boundary** (e.g. `MembershipOracle`) that this issue defines but does
**not** implement — sibling issues provide the impl. This keeps the stateless core independent
and fully testable without state.

### D4 — `created_at` units and clock-skew are advisory only

`created_at` is **milliseconds since Unix epoch** (uint), wall-clock, **display/advisory only**
— never trusted for ordering or authorization. The §6 step 10 clock-skew check (>300000 ms
ahead of local time) raises an advisory `clock_skew` **flag** and MUST NOT reject, drop,
reorder, or exclude the event. Because it reads local time, expose it as an **optional** check
the caller may invoke with a supplied "now"; the core pure validator stays time-independent so
it is deterministic and trivially testable. (This is the corrected behavior; an earlier draft's
hard reject would make log validity depend on each peer's clock — see §6 step 10 / §20.)

### D5 — Bytes-in / typed-out, and verbatim preservation

The validator hashes and verifies **the exact `signed` bytes** and never re-encodes to verify.
It then decodes for semantic checks and re-canonicalizes for the §3 byte-equality check. The
returned validated event **retains the original `signed` octets** (and the full `WireEvent`)
so the persistence and forwarding layers store/forward verbatim — satisfying acceptance
criterion "valid events preserve raw signed bytes."

---

## 5. Proposed module layout

New module tree under `crates/iroh-rooms-core/src/`:

```text
src/
  lib.rs            // re-export the event module; keep PROTOCOL_VERSION
  event/
    mod.rs          // public surface + module docs (links to PHASE-0-SPIKE.md §1–§8)
    keys.rs         // IdentityKey, DeviceKey (Ed25519 pubkeys), Signature, SecretKey wrapper
    ids.rs          // EventId, RoomId, HashRef (named BLAKE3 hashes) + hex / blake3: display
    constants.rs    // EVENT_CONTEXT, ROOMID_CONTEXT, BIND_CONTEXT, INVITE_CONTEXT, limits
    cbor.rs         // deterministic-CBOR encoder + strict canonical reader (D1)
    signed.rs       // SignedEvent (8 fields), CSB, event_id, room_id derivation, sign/verify
    wire.rs         // WireEvent {v, signed, sig, id}, encode/decode, recompute-id
    content.rs      // EventType registry + per-type content structs + strict validation
    binding.rs      // DeviceBinding + binding-message construction + verification (§1)
    validate.rs     // stateless verification pipeline (steps 1–6, structural 9, content)
    reject.rs       // RejectReason (§8), Flag (advisory), MembershipOracle trait boundary
  // tests:
  event/tests/      // OR crates/iroh-rooms-core/tests/ integration tests
    vectors.rs      // golden-vector conformance (Test Vectors §1–§8)
    negative.rs     // one crafted WireEvent per applicable §8 reject code
```

Test data: store the byte-exact fixtures from `PHASE-0-SPIKE.md` (cast keys, seeds, golden
CSB hex, signature, `room_id_A/B`, tampered/cross-room ids) as constants or a small
`testdata/` file. The golden values are the acceptance oracle — copy them exactly.

---

## 6. Data model (normative field mapping)

### 6.1 Keys & IDs (`keys.rs`, `ids.rs`)

- `IdentityKey([u8; 32])` — Ed25519 public key = `sender_id`. Never signs events in MVP.
- `DeviceKey([u8; 32])` — Ed25519 public key = `device_id` = iroh `EndpointId`. Verifies sigs.
- `Signature([u8; 64])` — Ed25519 detached signature.
- `EventId([u8; 32])` — raw BLAKE3-256 digest; `Display`/serialize as `blake3:<64-hex>`;
  `prev_events` entries are the raw 32 bytes (presentation is `blake3:`-prefixed only).
- `RoomId([u8; 32])`, `HashRef([u8; 32])` — same named-hash pattern (blob hashes, etc.).
- All hex output **lowercase**, exactly 64 chars; parse is case-insensitive but reject wrong
  length / bad prefix. Provide `FromStr`/`Display`, `PartialEq`/`Eq`/`Hash`, constant-time
  compare is **not** required for public keys/ids (public data), but signatures are verified
  via the crate's verify API, not manual compare.

### 6.2 `SignedEvent` (the eight signed fields — `signed.rs`)

| Field | Rust type | CBOR | Rule |
|---|---|---|---|
| `schema_version` | `u64` (validate `== 1`) | uint | MUST be `1`; else `unknown_schema_version`. |
| `room_id` | `RoomId` | bstr[32] | binds event to a room. |
| `sender_id` | `IdentityKey` | bstr[32] | participant identity. |
| `device_id` | `DeviceKey` | bstr[32] | signing key; signature MUST verify under this. |
| `event_type` | `EventType` | tstr | registered §7 type; else `unknown_event_type`. |
| `created_at` | `u64` | uint | ms since Unix epoch; advisory/display only. |
| `prev_events` | `Vec<EventId>` | array of bstr[32] | causal parents; ≤ 20; `[]` only for genesis. |
| `content` | `Content` (enum per type) | map | strict per-type schema (§7). |

`event_id` and `signature` are **not** part of `SignedEvent` (not signed-over). There is **no**
`lamport` field. The struct serializes to exactly `map(8)`.

### 6.3 `WireEvent` (`wire.rs`)

```text
WireEvent = {
  "v":      1,        // uint, transport envelope version (reject != 1)
  "signed": bstr,     // == CSB verbatim, preserved for storage/forwarding
  "sig":    bstr[64], // Ed25519 signature over EVENT_CONTEXT ‖ signed
  "id":     tstr      // "blake3:<hex>", advisory cache key — recomputed & checked, never trusted
}
```

The outer map is itself deterministic-CBOR (canonical key order `id, sig, signed, v` per the
§3 length-first-then-bytewise rule — verify and hardcode). Decoding rejects a non-canonical
outer map, missing keys, or `v != 1`.

### 6.4 Constants (`constants.rs`)

```text
EVENT_CONTEXT  = "iroh-rooms:event:v1"          // 19 bytes ASCII, no NUL
ROOMID_CONTEXT = "iroh-rooms:room-id:v1"
BIND_CONTEXT   = "iroh-rooms:device-binding:v1"  // 27 bytes ASCII
INVITE_CONTEXT = "iroh-rooms:invite:v1"          // used by content validation of capability_hash
MAX_PREV_EVENTS = 20
MAX_MESSAGE_BODY_BYTES = 16384
SCHEMA_VERSION = 1
WIRE_VERSION   = 1
CLOCK_SKEW_FUTURE_MS = 300_000
```

---

## 7. Derivations & crypto (normative)

### 7.1 CSB — canonical signed bytes (`cbor.rs` + `signed.rs`)

Encode the `SignedEvent` with the deterministic-CBOR profile (§3): map-keys sorted bytewise
by encoded form (length-first then bytewise for these short text keys), shortest-form ints,
definite-length only, no duplicate keys, no tags, **no floats**, valid UTF-8 text. The fixed
canonical top-level key order (MAY hardcode for assertions):

```text
content, room_id, device_id, sender_id, created_at, event_type, prev_events, schema_version
```

Golden oracle (Test Vector §1): the golden `message.text` event yields a **242-byte** CSB
beginning `a867636f6e74656e74a264626f6479…01`; encoder choice MUST NOT change a byte.

### 7.2 Event ID (§4)

```text
digest   = BLAKE3-256(CSB)                       // 32 bytes
event_id = "blake3:" ‖ lowercase_hex(digest)
```

Golden: CSB(242B) ⇒ `blake3:c389e251f9654902d26ea937b3e84a01bb5e5d578e394c95b6ade8b7144e85a1`
(Test Vector §3). Tampered-body (`"Hello room"`→`"Hello rooM"`) ⇒
`blake3:6267b72c066e30154b34d4430ce8fb735563c4500ff527d371bcc3de7f34c75c` (Test Vector §6).

### 7.3 Room ID (§5) — only for `room.created`

```text
room_id = BLAKE3-256( ROOMID_CONTEXT ‖ sender_id(32) ‖ room_nonce(16) ‖ created_at_be(8) )
```

`created_at_be` is the `room.created` `created_at` as big-endian u64. Golden (creator Alice
seed `01×32`, nonce `000102…0e0f`, `created_at=1750000000000`):
`room_id_A = 43c19f2e3d8e933a7a0ddbc7999c7c24a97bc5eeb52ddf9674bd3646723f16a3` (Test Vector §4).
Room B (`created_at=1750000000001`) ⇒ `cad9174a…3494` (Test Vector §7). On `room.created`, the
validator MUST recompute and reject `room_id_mismatch` if it differs from the envelope.

### 7.4 Signature (§6)

```text
sig_msg   = EVENT_CONTEXT ‖ CSB
signature = Ed25519_sign(device_secret, sig_msg)   // verify under device_id
```

Golden signature for the golden event: `98732ece…4f0f` (Test Vector §5), verifying under
`device_id = 8139770e…b394`. Verifying the same bytes under `sender_id = 8a88e3dd…6f5c`
(the "wrong field" bug) MUST **fail** → `bad_signature`.

### 7.5 Device binding (§1, `binding.rs`)

```text
binding_msg = BIND_CONTEXT ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)
accept iff Ed25519_verify(sender_id, binding_msg, binding_sig)
```

`DeviceBinding = { "identity_key": bstr[32], "device_key": bstr[32], "sig": bstr[64] }`. For
`room.created`/`member.joined`/`member.removed`, content validation MUST check
`identity_key == sender_id`, `device_key == device_id`, and that `sig` verifies — this is
self-contained crypto (no external state) and is in scope here. (Resolving a binding from
*membership state* for non-binding events is step 7 and is deferred.)

---

## 8. Stateless verification pipeline (`validate.rs`)

Implement the **stateless subset** of Event Protocol §6, in order; first failure rejects and
returns a `RejectReason`. Steps marked *(deferred)* are represented by a trait boundary and are
**not** decided in this issue.

1. **Decode transport.** Parse `WireEvent`. Reject `v != 1`, missing keys, non-canonical
   outer map → `non_canonical_encoding`.
2. **Recompute id.** `id' = "blake3:" + hex(BLAKE3-256(signed))`. Reject `id' != id` →
   `id_mismatch`. *(advisory `id` never trusted.)*
3. **Verify signature.** Decode `signed` enough to read `device_id`. Reject unless
   `Ed25519_verify(device_id, EVENT_CONTEXT ‖ signed, sig)` → `bad_signature`.
4. **Enforce canonicality + shape.** Reject unless `canonical_cbor(decode(signed)) == signed`
   **and** `signed` decodes to exactly the eight §2 keys with correct CBOR types →
   `non_canonical_encoding`.
5. **Version / type / content.** Reject `schema_version != 1` → `unknown_schema_version`;
   `event_type` not in §7 registry → `unknown_event_type`; strict content validation failure
   (unknown content key, missing required key, wrong type, length/enum violation, bad embedded
   `device_binding`) → `invalid_content` (or the specific code where §8 names one).
6. **Room binding.** For `room.created`, recompute `room_id` (§5); reject `room_id_mismatch`.
   For other types, reject unless `room_id` equals the room being processed (the caller supplies
   the expected `room_id`); mismatch → `room_id_mismatch`.
7. *(deferred — device binding from membership state.)* For binding-carrying types, the
   self-contained binding check runs in step 5; resolving a non-binding event's
   `sender_id → device_id` against membership state is a sibling issue. The validator returns a
   value the membership layer consumes.
8. *(deferred — membership & role.)* Exposed via the `MembershipOracle` trait boundary; not
   decided here. (`not_a_member` / `insufficient_role` are produced by the sibling layer.)
9. **Causal structure (stateless part).** Reject `prev_events.len() > 20` → `too_many_parents`.
   Reject a non-genesis event with empty `prev_events`, and require `room.created` to have
   empty `prev_events` → `not_genesis_descended`. *(Full transitive genesis-descent reachability
   is deferred — needs the DAG.)*
10. **Clock sanity (advisory, optional).** If a "now" is supplied and `created_at` is
    > `CLOCK_SKEW_FUTURE_MS` ahead, attach advisory `Flag::ClockSkew`. **Never** reject, drop,
    reorder, or exclude. The pure validator is time-independent.
11. *(deferred — dedup & persist.)* The validated result carries the `event_id` key and verbatim
    `WireEvent`; the store layer dedups and persists.

The function returns, on success, a `ValidatedEvent` holding the decoded `SignedEvent`, the
recomputed `EventId`, the verbatim `signed` bytes + full `WireEvent`, and any advisory `Flag`s.

---

## 9. Event-type registry & strict content validation (`content.rs`, §7)

Model `EventType` as a closed enum; unknown strings → `unknown_event_type`. Model each
`content` as a typed struct (or per-variant enum) and validate **strictly**: every key in the
inbound map must be a known key for that type, required keys present, optional keys typed,
and unknown keys → reject (`invalid_content`). The MVP registry (signer/role columns are the
deferred authorization layer's concern — record them as doc comments, do not enforce here):

- `room.created` — `{ room_name: tstr, room_nonce: bstr[16], admins: [bstr[32]…], device_binding: DeviceBinding }`.
  Stateless validate: `prev_events == []`; `room_id` recomputes (§5); `admins` exactly
  `[sender_id]` in MVP; binding verifies (`identity_key == sender_id`, `device_key == device_id`).
- `member.invited` — `{ invite_id: bstr[16], capability_hash: bstr[32], role: tstr∈{member,agent,admin}, invitee_key: bstr[32], expires_at?: uint, invitee_hint?: tstr }`.
  Stateless validate: `role` in enum; field shapes. (Admin-signer check deferred.)
- `member.joined` — `{ via_invite_id: bstr[16], capability_secret: bstr[16], role: tstr, device_binding: DeviceBinding, display_name?: tstr }`.
  Stateless validate: binding verifies; field shapes. (Invite-liveness/capability match deferred —
  needs ancestor view; but the capability-hash *recomputation primitive*
  `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ via_invite_id ‖ capability_secret)` is implemented here
  for the sibling layer to call.)
- `member.left` — `{ member_id: bstr[32], reason?: tstr }`. Stateless: `member_id == sender_id`.
- `member.removed` — `{ member_id: bstr[32], removed_by: bstr[32], reason?: tstr }`.
  Stateless: `removed_by == sender_id`; `member_id != sender_id`; binding verifies (if present).
- `message.text` — `{ body: tstr ≤16384 bytes, format?: tstr∈{plain,markdown}, in_reply_to?: bstr[32], mentions?: [bstr[32]…] }`.
- `file.shared` — `{ file_id: bstr[16], name: tstr, mime_type: tstr, size_bytes: uint, blob_hash: bstr[32], blob_format?: tstr∈{raw,hash_seq}, providers?: [bstr[32]…] }`.
- `pipe.opened` — `{ pipe_id: bstr[16], owner_id: bstr[32], owner_endpoint: bstr[32], kind: tstr=="tcp", label: tstr, target_hint: tstr, alpn: tstr, allowed_members: [bstr[32]…] non-empty, expires_at?: uint }`.
  Stateless: `owner_id == sender_id`; `kind == "tcp"`; `allowed_members` non-empty.
- `pipe.closed` — `{ pipe_id: bstr[16], reason?: tstr∈{closed,expired,owner_exit,error} }`.
- `agent.status` — `{ status: tstr, message?: tstr, related_artifact_ids?: [bstr[16]…], progress_pct?: uint 0..=100 }`.

Where a per-type rule needs ancestor/membership state (invite liveness, owner-is-active,
pipe-still-open, admin-signer), it is **deferred**; implement only the bytes-local checks above
and expose the primitives (capability-hash recompute, binding verify) the sibling layer needs.

---

## 10. Error model & observability

### 10.1 `RejectReason` (`reject.rs`, §8)

Implement a `RejectReason` enum covering the codes this stateless layer can emit:
`unknown_schema_version`, `unknown_event_type`, `non_canonical_encoding`, `id_mismatch`,
`bad_signature`, `room_id_mismatch`, `invalid_content`, `too_many_parents`,
`not_genesis_descended`. Reserve (document, do not emit here) the deferred codes
`unbound_device`, `not_a_member`, `insufficient_role`, `expired_invite`, `bad_capability` for
the sibling layer (define them in the same enum so the taxonomy is one type, or in a shared
enum the sibling extends — prefer one enum with doc-comments marking which layer emits each).

Each variant maps to a **stable string code** (exactly the §8 spelling) for the local audit log
/ CLI failure-mode distinction (PRD §16). `duplicate` is **ignored, not an error** (handled by
the store layer). Provide `Display` + the stable `code()` string.

### 10.2 Advisory `Flag`

`Flag::ClockSkew` (§6 step 10). Flags are returned alongside a **successful** validation; they
never change the verdict. (`equivocation`, `from_removed_member` are sibling-layer flags —
document but do not emit here.)

### 10.3 No panics on hostile input

The validator parses **adversarial** bytes. It MUST NOT panic, index-out-of-bounds, allocate
unbounded, or overflow on any input. Enforce: bounded decode (cap nesting/length up front),
`#![forbid(unsafe_code)]` (already workspace-wide), no `unwrap`/`expect`/`panic!`/slicing on
untrusted lengths in non-test code (Clippy pedantic + a targeted fuzz/property test, §11).

---

## 11. Test strategy

The verify gate runs fmt + clippy(`-D warnings`, pedantic) + tests across the workspace; all of
the below must pass under it.

### 11.1 Golden-vector conformance (`vectors.rs`) — the acceptance oracle

Map directly to Test Vectors §1–§8. Test the **primitives** against the golden values (the
golden event is a serialization fixture, not a live event — call CSB/id/sign/verify directly):

- **V1 — canonical determinism:** build the golden event with declaration-order keys and with
  scrambled keys; both encode to the **same 242-byte CSB**; assert the exact prefix and length,
  and the fixed top-level key order.
- **V2 — non-canonical rejected:** five crafted `signed` encodings (reordered keys,
  indefinite-length, non-shortest int, a ninth key, duplicate key) each → `non_canonical_encoding`.
- **V3 — id recompute + advisory id:** golden CSB ⇒ `blake3:c389e2…85a1`; a `WireEvent` with a
  doctored `id` (`blake3:0000…0000`) → `id_mismatch`.
- **V4 — room_id derivation:** `E_create` inputs ⇒ `room_id_A = 43c19f2e…16a3`; a forged
  envelope `room_id` → `room_id_mismatch`.
- **V5 — signature accept/reject by key:** verify under `device_id` **succeeds**; under
  `sender_id` **fails** → `bad_signature`. *(Acceptance criterion: "signature verifies under
  `device_id`, not `sender_id`.")*
- **V6 — tampered field:** flip one `body` byte ⇒ id becomes `6267b72c…c75c` (`id_mismatch`)
  **and** signature fails (`bad_signature`).
- **V7 — cross-room replay:** golden `WireEvent` (carries `room_id_A`) processed for room B →
  `room_id_mismatch`; legitimately re-authoring in room B changes `event_id` to `81b6a82b…f057`
  and needs a fresh signature.
- **V8 — duplicate idempotency:** *(store-layer; here just assert equal bytes ⇒ equal `event_id`,
  the stable dedup key.)*

Pin the byte-exact fixtures (cast keys/seeds, golden CSB hex, signature,
`device_binding.sig`, `room_id_A/B`, tampered + cross-room ids) as constants copied from
`PHASE-0-SPIKE.md`.

### 11.2 Issue acceptance-criteria tests (the issue's own Test Plan)

- **Valid event** → accepted, returns `ValidatedEvent` with verbatim `signed` bytes preserved
  byte-for-byte and recomputed `event_id`. *(Use a regenerated, causally-structural valid event
  — e.g. a genuine `room.created` genesis whose ids the harness regenerates — since the golden
  serialization fixture has empty `prev_events` and would hit `not_genesis_descended` as a
  `message.text`. See §12 R2.)*
- **Bad signature** → `bad_signature`.
- **Wrong verifying key** (`sender_id` used as verify key) → `bad_signature`.
- **Unknown schema version** (`schema_version = 2`) → `unknown_schema_version`.
- **Unknown event type** (`event_type = "message.bogus"`) → `unknown_event_type`.
- **ID mismatch** (doctored `id`) → `id_mismatch`.
- **Unknown content key** (extra key in a `message.text` content map) → `invalid_content`.
- **Preserve raw signed bytes**: assert the returned `signed` slice `== input signed`, and
  that re-hashing it reproduces the same `event_id` (round-trip through encode/forward).

### 11.3 Negative corpus (`negative.rs`)

One crafted `WireEvent` per applicable §8 reject code:
`non_canonical_encoding`, `id_mismatch`, `bad_signature`, `room_id_mismatch`,
`too_many_parents` (21 parents), `unknown_schema_version`, `unknown_event_type`,
`invalid_content` (unknown content key; wrong type; over-length body; bad enum;
`device_binding.sig` that fails), `not_genesis_descended` (non-genesis with empty
`prev_events`; `room.created` with non-empty `prev_events`). Plus the advisory cases:
`clock_skew` accepted-with-flag (supply a future `created_at`); equal-bytes ⇒ equal id.

### 11.4 Robustness (property / fuzz)

A property test (e.g. `proptest`/`arbitrary`) feeding random bytes to the `WireEvent` decoder
and the strict CBOR reader, asserting **no panic** and a typed `RejectReason` for all inputs
(§10.3). Optional `cargo-fuzz` target for the decoder if time allows. Round-trip property:
`decode(encode(x)) == x` and `canonical(encode(x)) == encode(x)` for valid `SignedEvent`s.

---

## 12. Risks & mitigations

- **R1 — CBOR determinism (HIGH, the headline risk; PRD §18.6).** A subtle canonicalization
  bug makes two peers disagree on bytes and is "the most expensive thing to find late" (spike).
  *Mitigation:* purpose-built canonical codec (D1) + the strict inbound profile check +
  decode→re-canonicalize→byte-equal + the 242-byte golden CSB as a hard gate. Confirm whether
  the chosen library emits §4.2.1 deterministic encoding by default; if not, hardcode the key
  order and hand-emit. **Gate B is NO-GO on any byte mismatch.**
- **R2 — Fixture-log ids not independently reproduced (MEDIUM).** `E_create … E_pipe`,
  `E_eq_a/b`, `E_mal` ids in the spike were NOT independently reproduced (content maps not
  pinned). *Mitigation:* the harness MUST regenerate them from the final content schema before
  trusting them; do not hardcode them as golden. Only the independently-verified values (golden
  `event_id`, signature, `device_binding.sig`, `room_id_A/B`, tampered id, cross-room id) are
  safe to pin verbatim.
- **R3 — Verify-under-wrong-key (HIGH, security).** Verifying under `sender_id` instead of
  `device_id` is an explicit, classic bug class. *Mitigation:* dedicated test V5 + a type-level
  guard (the verify API takes a `DeviceKey`, not a generic pubkey, so passing `sender_id`
  doesn't type-check in correct call sites).
- **R4 — Trusting advisory fields (HIGH, security; trust boundary).** Never trust the `id`
  field without recompute; never let `created_at`/clock-skew affect the verdict. *Mitigation:*
  steps 2 & 10 enforced + tests V3 and clock-skew-advisory.
- **R5 — Panic / DoS on hostile input (MEDIUM).** *Mitigation:* §10.3 + property/fuzz tests +
  bounded decode + workspace `forbid(unsafe_code)`.
- **R6 — ed25519-dalek pre-release pin (MEDIUM).** `3.0.0-rc.0` is a release candidate.
  *Mitigation:* pin the exact version iroh 1.0 resolves to so `device_id == EndpointId` stays
  byte-compatible; record the version and revisit when the transport issue adds `iroh`.
- **R7 — Scope creep into membership/ordering (MEDIUM).** The §6 algorithm interleaves stateless
  and stateful steps. *Mitigation:* the §8 pipeline here explicitly marks deferred steps and
  exposes them as trait boundaries; do not implement membership/role/ordering in this issue.

---

## 13. Acceptance criteria (issue + Gate B)

From the issue:

- [x] **Event ID is recomputed from exact signed bytes** — §6 step 2; tests V3, V6, §11.2.
- [x] **Signature verifies under `device_id`, not `sender_id`** — §6 step 3; test V5, §11.2.
- [x] **Unknown schema versions and unknown event types are rejected** — §6 step 5; §11.2/§11.3.
- [x] **Unknown content keys are rejected** — strict §7 content validation; §11.2/§11.3.
- [x] **Valid events preserve raw signed bytes for storage and forwarding** — verbatim `signed`
  retained on the validated result; §11.2.

From Gate B (the spike's GO bar this issue clears):

- [x] Golden vector reproduces CSB (242 B), `event_id`, and signature **exactly**.
- [x] Re-canonicalization is stable (decode→re-encode→byte-equal); non-canonical input rejected.
- [x] Every applicable §8 outcome is exercised and produces its stable code.
- [x] Clock-skew is accepted-with-flag, never dropped.
- [x] Genesis-descent structural check rejects floating events.
- [x] Valid events persist verbatim and dedup by `event_id` (this layer provides the stable key
  + bytes; the store layer dedups).
- [x] `scripts/verify.sh` is green (fmt, clippy `-D warnings` pedantic, tests).

---

## 14. Out-of-scope follow-ups (sibling issues under epic #1)

- Membership fold + authorization gate (§3.4–§3.8, §6 steps 7–8).
- Derived Lamport ordering + `(lamport, event_id)` total order.
- Out-of-order buffering / backfill / anti-amplification + transitive genesis-descent.
- SQLite event store (`events`, `members`, `sync_state`) + dedup-and-persist.
- Full-mesh QUIC transport (ALPN, admin-tip) + connect-time blob/pipe ACLs.
- Equivocation detection + admin-tip advertisement.

---

## 15. Open questions

1. **CBOR strategy (D1):** purpose-built canonical codec vs. `ciborium` + canonical wrapper —
   confirm during implementation whether `ciborium` is deterministic by default; if yes, the
   wrapper may suffice. Recommendation stands at purpose-built for the signed bytes.
2. **`ed25519-dalek` version (D2/R6):** pin `3.0.0-rc.0` now (matches iroh 1.0) or defer the
   exact pin to the transport issue and use a stable line meanwhile? Recommendation: pin now to
   the iroh-resolved version to lock byte compatibility.
3. **Single taxonomy enum vs. layered:** put all §8 codes (incl. deferred ones) in one
   `RejectReason` enum now, or keep this layer's subset and let the sibling extend? Recommend a
   single enum with doc-comments marking the emitting layer, for one stable taxonomy.
4. **`room_id` expectation source for non-`room.created` events:** the validator needs the
   "room being processed" (§6 step 6). Confirm it is passed in by the caller (room context) vs.
   resolved from a store — for this stateless layer, pass it in.
5. **Where the membership trait boundary lives:** define `MembershipOracle` in this crate now
   (empty/never impl) so the validator signature is stable, or introduce it with the membership
   issue? Recommend defining the trait now to freeze the validator's public surface.

## 16. Assumptions

- The spike's Event Protocol §1–§8 and the independently-reproduced golden values are
  authoritative and frozen for MVP; this issue copies them verbatim as the oracle.
- `schema_version` and `WireEvent.v` are both `1` for MVP; any other value is rejected
  (forward-compatible fields arrive only via a version bump).
- `device_id` is an Ed25519 public key byte-identical to the iroh `EndpointId`; no `iroh`
  dependency is added in this issue, but the key types are chosen to match.
- MVP is single-device-per-identity, single immutable admin, no key rotation — these constrain
  the deferred layers, not the stateless bytes, but are noted so content/doc comments are correct.
- This issue ships **library code + tests only**: no CLI wiring, no persistence schema, no
  network. The CLI/store/transport consume the public surface in later issues.
