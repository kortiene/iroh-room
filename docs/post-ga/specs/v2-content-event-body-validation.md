# Spec: v2 Content Event Body Validation

| | |
|---|---|
| **Issue** | #152 — `[CORE] v2 content event body validation (#134 §9.2)` |
| **Labels** | `type/feature` `area/protocol` `priority/p2` `risk/low` |
| **Depends on** | #146 — v2 identifiers and domain separation |
| **Refs** | #134 §9.2; #134 §6.4; `specs/v2-crypto-core-crate.md`; `specs/v2-identifiers-domain-separation.md` |
| **Owning crate** | `crates/iroh-rooms-v2-core/` |
| **Status** | Implemented in `crates/iroh-rooms-v2-core/src/content/{body,event,validate,registry}.rs`, with the shared canonical-CBOR `null` extension in `cbor.rs`, the device-key verifier in `keys.rs`, the `EventId::from_content_event_csb` derivation in `ids.rs`, and the focused acceptance suite in `tests/content_body_validation.rs`. The pre-#152 provisional schema is retained test-only as `content::provisional` to keep the frozen #153 golden vectors byte-stable. How the §16 open questions were resolved against what shipped is recorded in §17. |

---

## 1. Summary

Replace the provisional v2 content envelope with the strict #134 §9.2 `ContentEventBody`, preserve its exact canonical CBOR bytes as the cryptographic trust boundary, derive `EventId` with the frozen #146 content-event domain, verify one Ed25519 signature under the body's device key, and provide a pure per-device chain validator.

The normative logical body is:

```text
ContentEventBody = {
  "v":                  2,
  "community_id":       CommunityId,
  "stream_id":          StreamId,
  "author_id":          PrincipalId,
  "device_id":          DeviceId,
  "device_seq":         uint,
  "prev_device_event":  EventId | null,
  "auth_hint_seq":      uint,
  "created_at_ms":      uint,
  "kind":               registered tstr,
  "references":         [EventId; 0..=8],
  "content":            map
}
```

A `ContentEvent` carries the exact canonical body bytes and exactly one detached Ed25519 device signature. Its `EventId` is recomputed from the body bytes; it is not a third field in the logical wrapper described by the issue. A transport or caller that supplies an expected ID must compare it with the recomputed value. The signature is not in the ID preimage.

This work remains pure and unused by the shipped v1 runtime. It adds no transport, storage, async runtime, network lookup, publication certificate, device-cut, or merge behavior.

---

## 2. Repository context

### 2.1 Existing foundations to reuse

- `crates/iroh-rooms-v2-core/src/cbor.rs` owns the deterministic CBOR codec and exact-byte canonicality boundary. It already rejects indefinite lengths, non-shortest integers, duplicate/unsorted/non-text map keys, negative integers, tags, floats, invalid UTF-8, excessive depth, length overflow, and trailing bytes.
- `crates/iroh-rooms-v2-core/src/domain.rs` defines the frozen #146 domain `CONTENT_EVENT = b"iroh-room-v2/content-event"` and `BLAKE3(domain || payload)` / `domain || payload` helpers.
- `crates/iroh-rooms-v2-core/src/ids.rs` provides the normative `CommunityId`, `StreamId`, and `EventId`; `EventId::from_content_event_csb` already computes `BLAKE3(CONTENT_EVENT || csb)`.
- `crates/iroh-rooms-v2-core/src/keys.rs` uses strict Ed25519 verification, but its shared verifier currently accepts `MemberId`; content verification must verify under the distinct `DeviceId` represented by `body.device_id`.
- `crates/iroh-rooms-v2-core/src/signed.rs` establishes the exact-CSB envelope pattern and validates canonicality, ID, and signature without re-serializing received bytes.
- `crates/iroh-rooms-v2-core/src/content/registry.rs` and `content/validate.rs` contain the nearest local closed kind registry and strict kind-specific content validators.
- `crates/iroh-rooms-v2-core/src/governance/log/model.rs` already models members and their active/revoked devices. Authorization against those records is distinct from proving that a signature verifies.
- `crates/iroh-rooms-v2-core/src/error.rs` exposes stable typed rejection codes and does not log; callers own observability.
- `crates/iroh-rooms-v2-core/tests/golden/` freezes canonical bytes, domains, IDs, signatures, and rejection codes. Intentional changes require the documented fixture-schema/protocol-change discipline.
- `scripts/verify.sh` is the workspace CI gate invoked by `.github/workflows/verify.yml`.

### 2.2 Gaps in the current content implementation

The current `content/body.rs` is explicitly provisional. It uses `schema_version`, legacy `RoomId`, `author`, `version`, optional 16-byte `stream_id`, `body`, legacy `ContentEventId`, and legacy split `CONTENT_EVENT_SIGN` / `CONTENT_EVENT_ID` contexts. It does not contain `device_id`, `device_seq`, `prev_device_event`, `auth_hint_seq`, `created_at_ms`, or top-level `references`.

Additional gaps that #152 must close:

1. There is no per-device content chain validator.
2. The generic envelope records a principal signer outside the body instead of verifying under `body.device_id`.
3. The current optional field helpers can consume a present field of the wrong type and then treat it as absent.
4. Canonical-but-schema-invalid top-level fields are sometimes mislabeled `non_canonical_encoding` instead of `invalid_content`.
5. The current canonical CBOR value profile has no `null`, while §9.2 requires `prev_device_event = null` for the first device event.
6. The existing content golden vector freezes the provisional schema and legacy domains; it cannot silently become the normative §9.2 vector.

### 2.3 Compatibility boundary

`iroh-rooms-v2-core` is `publish = false` and unused by the shipped v1 runtime. #152 must not modify `crates/iroh-rooms-core`, v1 wire bytes, the CLI, SDK, network crates, persistence schemas, or release behavior. The normative v2 path may break the provisional v2 content API, but frozen fixture changes must be explicit and reviewable.

---

## 3. Requirements derived from #134 §9.2 and the issue

### 3.1 Exact record and strict §6.4 validation

The body is one canonical CBOR map containing exactly the twelve keys listed below. Every key is required, including `prev_device_event`, which uses explicit CBOR `null` for the first event rather than omission.

| Wire key | Rust representation | CBOR representation | Validation |
|---|---|---|---|
| `v` | `u64` | uint | Must equal `2`; other values reject as `unknown_version`. |
| `community_id` | `CommunityId` | `bstr[32]` | Exact width; no legacy `RoomId`. |
| `stream_id` | `StreamId` | `bstr[32]` | Required and exact width; no provisional 16-byte stream ID. |
| `author_id` | `PrincipalId` | `bstr[32]` | Exact width. It is an authorization identity, not the signature verification key. |
| `device_id` | `DeviceId` | `bstr[32]` | Exact Ed25519 public-key width and the only key used for the event signature. |
| `device_seq` | `u64` | uint | First event is `0`; successor is predecessor sequence plus one. |
| `prev_device_event` | `Option<EventId>` | `null` or `bstr[32]` | `null` iff `device_seq == 0`; otherwise a predecessor ID is required. |
| `auth_hint_seq` | `u64` | uint | Strictly typed and signed; authorization interpretation is deferred unless §9.2 supplies more rules. |
| `created_at_ms` | `u64` | uint | Strictly typed and signed. No local-clock rejection or ordering use in this issue. |
| `kind` | `ContentKind` | tstr | Must be in the closed v2 content registry; unknown value rejects as `unknown_content_kind`. |
| `references` | `Vec<EventId>` | array of `bstr[32]` | Length `0..=8`; each element has exact width. A ninth element rejects. |
| `content` | `CborValue::Map` | map | Must be a map and pass the registered kind's strict schema validator. |

Strictness rules:

- malformed or non-canonical CBOR is `non_canonical_encoding`;
- this plan proposes `invalid_content` for a canonical map with an unknown key, missing key, wrong type, wrong ID width, invalid cross-field combination, over-cap array, or invalid known-kind content; confirm OQ-9/taxonomy policy because some existing candidate signed-record helpers currently classify schema errors as `non_canonical_encoding`;
- no unknown field is ignored;
- no default is inferred for a missing top-level field;
- a present optional kind-specific field with the wrong type is rejected, never treated as absent;
- `references` preserves caller-provided array order because that order is part of the signed bytes and ID; this does not assert semantic ordering. This issue does not sort, deduplicate, resolve, or merge references because §9.2 only supplies the cap;
- `created_at_ms` is data, not a trusted ordering or authorization input;
- body validation is deterministic and does not read a clock, database, network, or process-global state.

### 3.2 Canonical bytes and `null`

The exact received canonical body bytes are `body_csb`. IDs and signatures operate on those bytes, never on a re-serialization of the typed body.

To represent the required genesis predecessor, extend the closed CBOR model with only canonical `null` (`0xf6`):

- add `CborValue::Null`;
- encode it only as `0xf6`;
- decode `0xf6` as `Null`;
- continue rejecting booleans, undefined, all other simple values, and floats;
- extend CBOR round-trip and rejection tests so this narrow profile expansion cannot accidentally admit other simple values.

### 3.3 EventId recomputation

Use the #146 type and domain without aliases:

```text
body_csb = exact canonical CBOR bytes of ContentEventBody
EventId  = BLAKE3-256("iroh-room-v2/content-event" || body_csb)
```

The verifier must recompute and return `EventId` from the exact received `body_csb`. It must not hash a decoded/re-encoded body. `EventId::from_content_event_csb` documentation and regression tests must make clear that its input is body CSB, not wrapper bytes. Comparing an externally supplied claimed ID and returning `id_mismatch` is deferred until OQ-2 establishes such a field or caller contract.

The following are not in the ID preimage:

- the Ed25519 signature;
- any transport framing or envelope version;
- persistence metadata;
- receive time;
- validation state.

Consequently, changing a body field changes the hash preimage and should change the ID except with negligible BLAKE3 collision probability, while changing only the signature does not affect ID recomputation.

### 3.4 Signature verification

Subject to OQ-1, the planned signature contract is:

```text
signature_message = "iroh-room-v2/content-event" || body_csb
signature         = Ed25519_sign(device_secret, signature_message)
verification key  = body.device_id
```

Requirements:

- the event contains exactly one 64-byte detached Ed25519 signature;
- verification uses strict Ed25519 verification under `body.device_id`, never `author_id` and never a separately supplied principal signer;
- signature verification uses exact `body_csb` bytes;
- a canonical tamper that remains schema-valid and retains the old signature rejects as `bad_signature`; malformed CBOR or invalid schema may reject earlier under §7;
- signature verification proves control of `device_id`; it does not by itself prove that the device belongs to `author_id` or is currently authorized.

Device-to-author ownership and grant/revocation authorization require validated governance state. They should be exposed as a later stateful authorization stage or deferred requirement, not conflated with cryptographic signature failure.

### 3.5 Per-device chain validation

The issue normatively requires a per-device sequence and exact predecessor link. This plan proposes scoping it by `(community_id, device_id)`, permitting succession across streams, and requiring predecessor `author_id` continuity. Those context checks are security-oriented assumptions pending OQ-3/OQ-4; the mandatory minimum is the same device, exact predecessor ID, and exact sequence increment.

Intrinsic checks, possible from one body:

```text
if device_seq == 0:
    prev_device_event must be null
else:
    prev_device_event must be present
```

Successor checks, given the verified immediate predecessor:

```text
current.community_id      == previous.community_id
current.device_id         == previous.device_id
current.author_id         == previous.author_id
current.device_seq        == checked_add(previous.device_seq, 1)
current.prev_device_event == previous.event_id
```

Additional behavior:

- `u64::MAX` has no valid successor; checked addition must reject rather than wrap or panic;
- for a sequence-zero current event, predecessor event data is not consulted; callers should use `None`, but supplying unrelated context is API misuse rather than evidence that the current body is malformed;
- if a nonzero event's named predecessor is not supplied, return the OQ-9-confirmed defer/missing-dependency outcome, allowing a caller to buffer it without declaring its bytes malformed;
- if supplied predecessor data contradicts the required predecessor ID, device, or sequence, return the OQ-9-confirmed chain-invalid outcome; apply community/author checks only if OQ-3/OQ-4 confirm them;
- validating a five-event sequence means validating event 0 intrinsically, then events 1 through 4 against each immediately preceding verified event;
- event-set fork detection, competing successors, deduplication, persistence, and canonical branch selection are not introduced here. This issue validates a proposed predecessor/successor link only.

The chain validator must consume a verified object that retains `event_id`, `body_csb`, signature-verified device identity, and decoded body. It must not accept an arbitrary caller-constructed body as proof of a predecessor.

### 3.6 References cap

Define one public protocol constant in the content module:

```text
MAX_CONTENT_REFERENCES = 8
```

Validation accepts zero through eight references and rejects nine or more as `invalid_content`. Every entry must be a canonical 32-byte `EventId` value. Resolution, causal availability, same-stream constraints, duplicate policy, self-reference policy, and merge semantics are not stated by the issue and are therefore not invented by #152.

---

## 4. Scope

### 4.1 In scope

1. The exact v2 `ContentEventBody` record and canonical serializer/parser.
2. Closed top-level key validation and strict §6.4 type, width, version, kind, and content validation.
3. Narrow canonical-CBOR support for explicit `null` required by `prev_device_event`.
4. Normative #146 `CommunityId`, `StreamId`, `EventId`, and `domain::CONTENT_EVENT` use.
5. A concrete signed content-event envelope/verified representation that retains exact body CSB.
6. `EventId` recomputation and mismatch rejection.
7. One strict Ed25519 signature verification under `body.device_id`.
8. Intrinsic device sequence/predecessor checks and pure predecessor/successor link validation.
9. `references.len() <= 8` and reference element width validation.
10. Fixing strict optional-field parsing in the kind-specific validator where needed to uphold §6.4.
11. Unit, integration, golden, negative, and property tests for these boundaries.
12. Stable rejection taxonomy coverage and Rust API documentation.

### 4.2 Out of scope

- #134 §9.3 deterministic merge rules for conflicting event kinds, including any work assigned to #158.
- Publication certificates (Phase C).
- #134 §13.4 device-cut construction.
- Event-set fork resolution or selection among competing successors.
- Reference fetching, availability, causal resolution, deduplication, or semantic merge behavior.
- Interpretation of `auth_hint_seq` against governance history beyond strict field validation.
- Device ownership, active/revoked status, membership, role, stream authorization, or governance-fork authorization decisions.
- Checkpoints, replica receipts, store indexes, migrations, wire transport, ALPNs, networking, async runtime, clocks, logging, CLI/SDK exports, or deployment.
- Changes to the v1 protocol or shipped runtime.

---

## 5. Key design decisions

### D1 — Replace the provisional content record; do not support two normative v2 schemas

The §9.2 field names and #146 types are normative. `ContentEventBody` must use `v`, `community_id`, required 32-byte `stream_id`, `author_id`, device/chain fields, references, and `content`. Legacy `RoomId`, `ContentEventId`, 16-byte stream IDs, `version`, and split content sign/ID contexts must not remain an alternative accepted encoding.

A temporary source-compatibility alias may exist only if it cannot decode or emit legacy bytes. There must be one normative content wire schema.

### D2 — Preserve exact received body bytes

A verified event must retain `body_csb` verbatim. Re-encoding is useful as a canonicality assertion and for locally built events, but never as a substitute for bytes received and signed.

### D3 — Keep cryptographic verification separate from authorization

Signature verification uses the device public key inside the signed body. Device-to-author binding and device status use governance state and are deferred. This avoids reporting an ungranted device as a forged signature and keeps the pure cryptographic acceptance criteria testable without a governance store.

### D4 — Separate intrinsic and relational chain checks

`device_seq`/`prev_device_event` shape is a body invariant. Confirming predecessor ID, device, and sequence is a relational check requiring a supplied verified predecessor; community/author checks depend on OQ-3/OQ-4. Missing and contradictory state use the OQ-9-confirmed outcomes.

### D5 — Use existing rejection codes unless normative text requires a new one

Do not add a public `invalid_device_chain` code solely for convenience unless normative text requires it. The proposed mapping, which must be confirmed before vectors freeze, is:

| Failure | Code |
|---|---|
| Malformed/non-canonical CBOR | `non_canonical_encoding` |
| Unsupported `v` | `unknown_version` |
| Unknown `kind` | `unknown_content_kind` |
| Missing/unknown/wrong-type/wrong-width/over-cap field | `invalid_content` |
| Invalid first/successor relationship with supplied data | `invalid_content` |
| Claimed ID differs from body CSB hash | `id_mismatch` |
| Signature fails under body device key | `bad_signature` |
| Named predecessor not supplied | `missing_dependency` |
| Device not authorized for author/action in later stage | `insufficient_authorization` |

### D6 — Preserve reference byte order in a bounded array

The issue requires only a cap. Sorting or deduplicating would change signed bytes and invent semantics. Preserve the encoded array order because it affects bytes and ID, without claiming application-level order semantics; enforce only array shape, element width, and cap in #152.

### D7 — No direct logging in the core

Return `Reject` values. Downstream runtime code may count or audit stable `.code()` values, but this pure crate must not log content, IDs, keys, or signatures.

---

## 6. Proposed public API

Exact Rust naming may follow local conventions, but the API must preserve these distinctions:

```rust
pub const CONTENT_EVENT_VERSION: u64 = 2;
pub const MAX_CONTENT_REFERENCES: usize = 8;

pub struct ContentEventBody {
    pub v: u64,
    pub community_id: CommunityId,
    pub stream_id: StreamId,
    pub author_id: PrincipalId,
    pub device_id: DeviceId,
    pub device_seq: u64,
    pub prev_device_event: Option<EventId>,
    pub auth_hint_seq: u64,
    pub created_at_ms: u64,
    pub kind: ContentKind,
    pub references: Vec<EventId>,
    pub content: CborValue,
}

pub struct ContentEvent {
    pub body_csb: Vec<u8>,
    pub signature: Signature,
}

pub struct VerifiedContentEvent {
    id: EventId,
    body_csb: Vec<u8>,
    signature: Signature,
    body: ContentEventBody,
}

pub fn verify_content_event(
    event: &ContentEvent,
) -> Result<VerifiedContentEvent, Reject>;

pub fn seal_content_event(
    body: &ContentEventBody,
    key: &SigningKey,
) -> Result<ContentEvent, Reject>;

pub fn validate_device_chain_link(
    previous: Option<&VerifiedContentEvent>,
    current: &VerifiedContentEvent,
) -> Result<(), Reject>;
```

API invariants:

- callers cannot construct `VerifiedContentEvent` through public fields;
- `verify_content_event` is the only promotion path from untrusted event to verified event; it always recomputes and retains the ID;
- accessors may expose the decoded body, ID, and exact CSB read-only;
- local sealing performs full strict body validation before signing, and the `SigningKey`'s derived `DeviceId` must equal `body.device_id`; mismatch is rejected rather than silently rewriting the body;
- if the generic `SignedBody` abstraction cannot express an in-body device verification key and single frozen domain cleanly, implement a concrete content path rather than weakening the generic trait or retaining an out-of-body signer.

---

## 7. Validation order

The proposed deterministic precedence is below; OQ-8 must confirm it before vectors freeze stable outcomes:

1. Accept the typed in-memory content wrapper containing exact body CSB and one typed 64-byte signature. Raw serialized outer-envelope field/length validation remains blocked on OQ-2.
2. Canonically decode exact `body_csb`; malformed or non-canonical bytes return `non_canonical_encoding`.
3. Recompute and retain `EventId::from_content_event_csb(body_csb)`; compare a claimed ID only after OQ-2 defines one.
4. Strictly decode the complete top-level body and registered kind-specific content, including version, kind, device key shape, references cap, and intrinsic chain shape. Schema errors return the confirmed mapped code before cryptographic verification.
5. Subject to OQ-1, verify `Ed25519(device_id, CONTENT_EVENT || body_csb, signature)`; failure returns `bad_signature`.
6. Promote to `VerifiedContentEvent`.
7. If requested, validate the link against a supplied verified predecessor; report the confirmed missing-dependency or chain-invalid outcome.
8. Leave governance/device ownership and other deferred authorization to a separate caller-provided stateful stage.

A tamper test that intends to assert `bad_signature` must produce canonical, schema-valid tampered bytes; otherwise an earlier canonicality or schema failure is correct.

---

## 8. Implementation plan

### Step 1 — Add narrow CBOR `null` support

1. Add `CborValue::Null` to `cbor.rs`.
2. Encode it as the single canonical byte `0xf6`.
3. Decode only `0xf6` from CBOR major type 7; retain rejection of booleans, undefined, floats, break, and unsupported simple values.
4. Update exhaustive matches, generators, round-trip tests, and negative canonical-CBOR tests.
5. Update codec documentation to state that `null` is in the shared value space, require every typed schema to reject it outside permitted fields, test non-shortest simple encoding `f8 16`, and regression-test existing record families.
6. Verify existing golden bytes remain unchanged except for the deliberately versioned content-event fixture.

### Step 2 — Replace the provisional body model

1. Rewrite `content/body.rs` around the twelve exact §9.2 keys.
2. Use normative #146 types: `CommunityId`, `StreamId`, `EventId`, `PrincipalId`, and `DeviceId`.
3. Remove the normative content path's dependence on `RoomId`, `ContentEventId`, `CONTENT_EVENT_SIGN`, `CONTENT_EVENT_ID`, body `version`, optional short stream IDs, and provisional outer `author`/`body` names. Do not delete compatibility definitions still required by other frozen candidate vectors.
4. Encode all twelve keys, representing no predecessor as explicit `CborValue::Null`.
5. Decode through strict typed field helpers that distinguish absent, wrong type, and wrong width, mapping canonical schema errors to the OQ-9-confirmed code.
6. Reject all unknown top-level keys and all missing required keys.
7. Enforce `v == 2`, the closed `ContentKind`, `content` map shape, references element widths/cap, and intrinsic sequence/predecessor shape. Use a dedicated references parser that accepts an empty array; do not reuse kind-specific bounded-array helpers that require non-empty input.

### Step 3 — Harden kind-specific strict parsing

1. Preserve the existing closed `ContentKind` registry and per-kind validators as the nearest local registry source.
2. Change all optional accessors—including text, uint, bytes, and arrays—from `Option<T>` to `Result<Option<T>, Reject>` or equivalent so a present wrong-typed value returns the OQ-9-confirmed schema error.
3. Continue rejecting unknown kind-specific keys and enforcing existing required fields, caps, enums, and stateless cross-field invariants.
4. Rename uses from provisional `body.body`/`body.author`/short `stream_id` to normative `content`/`author_id`/typed `StreamId` where applicable.
5. Do not implement §9.3 merge behavior while touching these validators.

### Step 4 — Add a concrete exact-byte content envelope

1. Define `ContentEvent` with exact body CSB and one typed signature. Defer raw serialized envelope decoding and claimed-ID comparison until OQ-2 fixes that envelope.
2. Define non-forgeable `VerifiedContentEvent` retaining the recomputed ID, exact CSB, signature, and decoded body.
3. Use `EventId::from_content_event_csb` and `domain::CONTENT_EVENT` for both hash and signature boundaries.
4. Add or adapt a strict verifier that accepts `DeviceId` directly and uses `ed25519_dalek::VerifyingKey::verify_strict`. Document that this creates the required structural boundary even though current `SigningKey` helpers expose identical member/device bytes for one key; full multi-key ownership remains governance authorization work.
5. Ensure the public verifier verifies under decoded `device_id`, not `author_id` or an envelope signer.
6. Add fallible `seal_content_event`: run full strict body validation, reject a signing key/body device mismatch, then encode and sign.
7. Keep signature bytes outside every EventId helper and preimage; update the helper documentation to define its input as exact canonical `ContentEventBody` bytes, not outer wrapper bytes.

### Step 5 — Add pure chain validation

1. Add intrinsic validation for `(device_seq == 0) == prev_device_event.is_none()`.
2. Add `validate_device_chain_link(previous, current)` over verified events.
3. For sequence zero, require a null predecessor ID and do not consult predecessor event data; document `None` as the caller contract without mapping an extraneous argument to malformed content.
4. For nonzero sequence, return the OQ-9-confirmed missing-dependency/defer outcome when the required predecessor is absent.
5. Compare predecessor ID, device, and checked sequence increment. Also compare community and author if OQ-3/OQ-4 confirm those proposed chain invariants.
6. Reject overflow and mismatches with the OQ-9-confirmed chain-invalid code.
7. Keep lookup/storage out of the API; callers supply the predecessor they resolved by `prev_device_event`.

### Step 6 — Add focused tests and frozen vectors

1. Add `tests/content_body_validation.rs` for record, crypto, chain, and references behavior.
2. Add deterministic unit tests near CBOR, body, key, and chain helpers.
3. Bump the golden fixture schema, replace the provisional content event in `tests/golden/v2-signed-records.json`, update mirror constants/assertions in `tests/signed_records_golden.rs`, and add the mandatory protocol-change note to `tests/golden/README.md`; prove unrelated vectors remain unchanged.
4. Build vectors from fixed non-secret seed bytes; do not call entropy-backed key generation. Independently recompute or externally review the frozen BLAKE3 digest rather than testing a helper only against its own output.
5. Extend taxonomy coverage if any reachable outcome changes.
6. Keep tests network-free, store-free, clock-free, and deterministic.

### Step 7 — Documentation and verification

1. Update Rust docs and module exports to describe the §9.2 trust boundary and deferred authorization.
2. Remove stale content comments that describe the provisional #158 envelope as normative.
3. Run the focused core checks, then the full repository gate listed in §11.
4. Require protocol maintainer review because canonical bytes, domains, signatures, IDs, and frozen fixtures are interoperability boundaries.

---

## 9. Test strategy

### 9.1 Required acceptance tests

| Issue acceptance | Test design |
|---|---|
| EventId recomputes exactly from a golden body | Freeze the logical body, canonical body hex, content domain, and expected `EventId`; assert `EventId::from_content_event_csb(fixture_csb)` and full verification both equal the expected value. |
| Signature over tampered bytes is rejected | Flip/change one value while keeping the body canonical and schema-valid, retain the original signature, and assert `bad_signature` under the OQ-1-confirmed signature preimage. Separately assert that recomputing the ID yields the tampered body's new ID. |
| Five-event device sequence validates | Build deterministic verified events with sequences `0..=4`; event 0 has null predecessor and each successor names the prior ID. Validate all five and all four links. |
| Ninth reference rejected | Test arrays of lengths 0, 8, and 9; 0 and 8 pass, 9 returns `invalid_content`. |
| Signature is not part of EventId preimage | Derive the ID directly from body CSB, invoke deterministic signing again over the same key/message and assert the same ID (and expected same Ed25519 signature), then mutate/remove only wrapper signature bytes and assert ID recomputation is still unchanged while invalid signature bytes fail verification. |

### 9.2 Strict body matrix

Add positive and negative cases for:

- all twelve required fields present and valid;
- every required field omitted, one at a time;
- unknown top-level key;
- each scalar field with a wrong CBOR type;
- wrong 31/33-byte width for every 32-byte ID/key field;
- `v = 1`, `v = 3`, and wrong-type `v`;
- unknown content kind;
- non-map `content`;
- present wrong-typed optional field in each kind-specific accessor family;
- `device_seq = 0` with non-null predecessor;
- `device_seq > 0` with null predecessor;
- zero, one, eight, and nine references;
- wrong-type and wrong-width reference entries;
- canonical top-level non-map body bytes;
- canonical non-array `references`, empty-array references, and wrong-type/wrong-width entries;
- non-canonical body bytes, duplicate/unsorted keys, trailing data, overlong null (`f8 16`), and unsupported simple values;
- explicit canonical `null` accepted by the shared codec but accepted by this schema only for `prev_device_event` where permitted.

Schema-negative crypto-path tests must hand-build raw canonical CBOR and sign those intentionally invalid bytes where needed; typed constructors cannot create the malformed cases. If OQ-2 later adds a claimed ID, those fixtures must also supply the recomputed ID to reach schema/signature checks.

### 9.3 Crypto matrix

- exact golden body/domain/ID/signature success;
- if OQ-2 defines a claimed ID, claimed/recomputed ID mismatch;
- canonical schema-valid body-byte tamper with old signature;
- deliberately distinct deterministic author and device keys: verification under `author_id` fails while `device_id` succeeds (the current helper otherwise derives equal bytes for both views of one key);
- wrong device key;
- wrong-width device key is `invalid_content`, while exact-width bytes that are not a valid Ed25519 point reach `bad_signature`;
- malformed Ed25519 public key;
- wrong signature length only after OQ-2 defines a raw serialized envelope decoder; the typed `Signature` wrapper already guarantees 64 bytes;
- wrong domain signature;
- local sealing key does not match body device;
- signing the same body again leaves ID unchanged (and deterministically reproduces the Ed25519 signature); mutating or omitting wrapper signature bytes also leaves ID recomputation unchanged, although mutated signature bytes fail verification.

### 9.4 Chain matrix

- valid first event `(0, null)`;
- valid links `0→1→2→3→4`, with every event sealed, verified through the public trust boundary, then link-validated;
- nonzero event with predecessor unavailable returns the OQ-9-confirmed missing/defer outcome;
- sequence gap, repeated sequence, and decreasing sequence reject;
- wrong `prev_device_event` rejects;
- correct predecessor ID from another community rejects if OQ-3 confirms community-scoped chains;
- correct sequence from another device rejects;
- author changes on the same community/device chain reject if OQ-4 confirms author continuity;
- successor after `u64::MAX` rejects without panic/wrap;
- a caller-provided unverified body cannot be used as predecessor proof.

### 9.5 Property and regression tests

- canonical `encode(decode(bytes)) == bytes` including `Null`;
- generated valid reference lengths never exceed eight after validation;
- for a generated valid body, mutating any body field changes EventId with overwhelming cryptographic certainty, while changing only envelope signature never changes recomputation;
- existing non-content golden vectors remain byte-identical;
- banned-dependency test continues to pass.

---

## 10. Acceptance criteria

- [ ] `ContentEventBody` uses exactly the twelve #134 §9.2 fields and normative #146 identifier types.
- [ ] Top-level and registered kind-specific validation is strict: unknown, missing, wrong-type, wrong-width, and out-of-bound values are rejected rather than ignored/defaulted.
- [ ] Canonical explicit `null` is supported for the first event's `prev_device_event` without admitting other unsupported CBOR simple values.
- [ ] EventId is exactly `BLAKE3("iroh-room-v2/content-event" || exact_body_csb)` and matches a frozen golden body.
- [ ] The ID is recomputed from exact received canonical body bytes; if OQ-2 defines a claimed-ID field, a mismatch returns `id_mismatch`.
- [ ] After OQ-1 confirms the preimage, exactly one Ed25519 signature is verified over that message under `body.device_id`.
- [ ] Under the OQ-1-confirmed preimage, a signature retained across canonical, schema-valid tampered body bytes is rejected as `bad_signature`.
- [ ] Signature bytes, wrapper framing, and metadata are absent from the EventId preimage; mutating/removing only signature bytes leaves ID recomputation unchanged.
- [ ] First device event requires `device_seq = 0` and `prev_device_event = null`.
- [ ] A successor requires the same device, exact predecessor EventId, and `previous.device_seq + 1` using checked arithmetic; same-community and same-author checks become mandatory only if OQ-3/OQ-4 confirm them.
- [ ] A deterministic five-event device sequence (`0..=4`) validates end to end.
- [ ] `references` accepts at most eight exact-width EventIds and rejects a ninth.
- [ ] Missing predecessor state is distinguishable from contradictory supplied chain data using the OQ-9-confirmed outcomes.
- [ ] Frozen content vectors are deliberately versioned/documented; unrelated golden vectors do not drift.
- [ ] No §9.3 merge rule, publication certificate, device cut, network, store, runtime, or v1 behavior is added.
- [ ] Focused tests, formatting, Clippy, full core tests, banned-dependency checks, and `scripts/verify.sh` pass.

---

## 11. Verification commands

Run the smallest relevant checks first:

```bash
cargo fmt --all --check
cargo test -p iroh-rooms-v2-core --all-targets --all-features
cargo clippy -p iroh-rooms-v2-core --all-targets --all-features -- -D warnings
cargo tree -p iroh-rooms-v2-core
```

Inspect the dependency tree or rely on the existing guard to confirm that `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, `rusqlite`, and runtime/store crates remain absent.

Then run the repository gate:

```bash
scripts/verify.sh
```

No online, deployment, migration, or release test is required because the v2 core remains isolated and unused by the shipped runtime.

---

## 12. Security, privacy, reliability, and performance

### Security

- Exact-CSB hashing and signing prevents re-serialization ambiguity.
- Domain separation prevents cross-record signature/ID reuse.
- Verification under `device_id` avoids the classic wrong-key (`author_id`) bug.
- Community, stream, author, device, sequence, predecessor, authorization hint, timestamp, kind, references, and content are all signed and ID-bound.
- A valid device signature is not treated as proof of device ownership or authorization; that distinction must remain explicit.
- Strict field closure prevents downgrade/extension confusion.

### Privacy

The core introduces no telemetry or logging. Content, references, author/device identifiers, and signatures must not be logged by this crate. The body is not encrypted by this issue.

### Reliability

- Checked sequence arithmetic prevents overflow.
- Missing predecessor state is deferable rather than permanently rejected.
- Fixed reference and CBOR depth limits bound graph fan-out and nesting, but the current codec has no total body-byte/collection-size cap; validation remains linear in supplied input and a normative total-size limit should be adopted if #134 defines one.
- Pure validation makes replay deterministic across replicas.

### Performance

Event ID hashing and signature verification are linear in body size; chain-link validation is constant time once the predecessor is supplied. The references scan is bounded by eight. Total CSB size and general collection lengths are not currently bounded by the codec, so this plan does not claim constant memory. This issue must not add graph traversal or network/store lookup.

---

## 13. Rollout and rollback

There is no runtime rollout or data migration: the crate is unpublished and unused by v1. Land the body model, verifier, chain validator, and vectors atomically so no intermediate commit presents provisional bytes as normative.

Rollback is source-level reversion before v2 publication. After another implementation consumes the new frozen vectors, rollback or wire changes require an explicit schema/version change and protocol-change note; silently restoring provisional content bytes is not compatible.

---

## 14. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Signature preimage differs in the full #134 text | Implementations produce incompatible signatures | Confirm the assumption in OQ-1 before implementation; pin domain, message, key, and signature in a golden vector. |
| Required `null` broadens the CBOR profile accidentally | Unsupported simple values enter signed records | Admit only `0xf6`; add exhaustive negative tests for booleans, undefined, floats, break, and other simple values. |
| Provisional and normative schemas coexist | Two incompatible events are both described as v2 | Replace the normative content path; do not dual-decode; explicitly version fixture changes. |
| Verification uses author instead of device | Forged/incorrect identity binding | Make content verifier consume `DeviceId` and add wrong-key golden tests. |
| Signature verification is mistaken for authorization | Revoked or foreign device accepted by callers | Return a verified cryptographic object and document governance authorization as a separate mandatory consumer stage. |
| Chain scope is interpreted differently | Peers disagree across streams | Pin `(community_id, device_id)` and test cross-stream sequencing; confirm OQ-3 before implementation. |
| Arrival order causes permanent rejection | Valid out-of-order events are lost | Return `missing_dependency` for absent predecessor and let callers buffer/retry. |
| Optional wrong-type fields remain silently accepted | Strict §6.4 closure is violated | Replace optional field accessors with typed `Result<Option<T>, Reject>` and add one test per accessor family. |
| Golden fixture update hides unrelated drift | Interoperability regression | Assert unrelated fixtures unchanged and document the exact schema/domain migration in the golden README. |
| Reference semantics are invented prematurely | Later §9.3 semantics become incompatible | Enforce only exact type/width/order preservation and cap; leave resolution/merge policy out of scope. |

---

## 15. Assumptions

1. #146 is complete and its `CommunityId`, `StreamId`, `EventId`, and exact `iroh-room-v2/content-event` domain are normative for #152.
2. All identifier, principal, device, and event values listed in §9.2 are encoded as 32-byte CBOR byte strings.
3. The body map contains all twelve keys; `prev_device_event` uses explicit CBOR null rather than omission.
4. Subject to OQ-1, signature messages use the same frozen content-event domain as the EventId boundary: `CONTENT_EVENT || body_csb`; implementation is blocked if the normative text defines another preimage.
5. `device_seq` is a `u64` scoped per `(community_id, device_id)`, starts at zero, and crosses streams within that community.
6. A device remains associated with one `author_id` throughout a chain.
7. `auth_hint_seq` and `created_at_ms` are required unsigned signed fields, but this issue applies no external-state or wall-clock semantics to them.
8. The current eight-kind `ContentKind` registry and strict per-kind schemas are the nearest local registry input until the complete #134 table is available.
9. References are an ordered array. No uniqueness, sorting, self-reference, stream, or availability rule is assumed beyond the cap and element width.
10. This plan proposes that existing rejection codes are sufficient; OQ-9 must confirm that no taxonomy expansion is required for chain mismatch.

---

## 16. Open questions

These must be confirmed against the complete #134 §9.2 text before production implementation freezes bytes:

1. **Signature preimage:** Is the exact signature message `iroh-room-v2/content-event || canonical body`, with no length delimiter or separate signature domain? Implementation and crypto vectors are blocked until confirmed.
2. **Outer envelope:** Does the serialized `ContentEvent` carry an explicit claimed `EventId`, and what are its exact canonical field names/version? Raw envelope decoding and `id_mismatch` are blocked; body-ID recomputation itself is not.
3. **Chain scope:** Is `device_seq` per `(community_id, device_id)`, globally per device, or per stream? This plan proposes per community/device and permits cross-stream succession, but those context tests are blocked until confirmed.
4. **Author continuity:** Must a successor's `author_id` equal the predecessor's author, or is device ownership resolved solely from governance state? Do not mandate this link check until confirmed.
5. **`auth_hint_seq`:** Which governance cursor does it identify, and are stale, future, or unavailable hints accepted, deferred, or rejected by #152 versus a later authorization issue?
6. **References:** Are duplicates forbidden, is order semantic, must referenced events be ancestors/same-community/same-stream, and should missing references defer validation? This plan enforces only the supplied cap and width.
7. **Kind registry:** Does #134 §9.2's final registry exactly match the eight kinds currently sourced from `specs/content-and-moderation-event-schemas.md`?
8. **Validation precedence:** Does #134 mandate signature verification before full schema validation? This plan performs canonicality, optional expected-ID comparison, and complete schema validation before signature verification so the in-body device key is typed and trusted only as data.
9. **Taxonomy mapping:** Does the protocol define a dedicated chain code, and should canonical missing/unknown/wrong-type/width body fields be `invalid_content` rather than the `non_canonical_encoding` used by some current candidate helpers?
10. **Schema-evolution gate:** Must the repository's unresolved D-9/P-26 schema-evolution ADR land before changing this unpublished v2-only content fixture, or is isolated core research explicitly exempt?
11. **Total body size:** Does #134 impose a maximum canonical body byte length or general collection-length cap beyond `references <= 8`?

Questions 1–3 and 10 are implementation blockers, not follow-up polish. Resolve them or record an explicit approved exemption before changing frozen bytes; do not guess a second wire format in code.

---

## 17. Implementation notes (post-landing)

#152 landed exactly as designed in §5–§9. The normative `ContentEventBody` lives
in `content/body.rs` (the single accepted v2 content wire schema: one canonical-
CBOR map of exactly twelve keys, `prev_device_event` emitted as canonical `null`
for the genesis event). The concrete exact-byte envelope, the device-key
verifier, and the pure per-device chain validator live in `content/event.rs`
(`ContentEvent` / `VerifiedContentEvent` / `verify_content_event` /
`seal_content_event` / `validate_device_chain_link`). `content/validate.rs`
hardens the shared per-kind content validator (the strict `Fields` reader with
`Result<Option<T>, Reject>` optional accessors that reject a present wrong-typed
value), and `content/registry.rs` owns the closed eight-kind registry. The
shared canonical-CBOR codec gained only `CborValue::Null` (`0xf6`); `keys.rs`
adds `verify_device` (strict Ed25519 under `DeviceId`); `ids.rs` documents
`EventId::from_content_event_csb` as taking body CSB (never wrapper bytes, never
including the signature). The pre-#152 provisional schema is retained test-only
as `content::provisional::ProvisionalContentEventBody` so the frozen #153
content-event golden vector stays byte-identical; it cannot be decoded as
normative bytes (D1: one normative v2 content wire schema). No frozen wire byte,
domain string, or successful golden CSB/id/signature vector changed; the golden
`content-event-message-text-v1` vector is deliberately preserved on the
provisional schema and the v5 change-log entry in `tests/golden/README.md`
records the deliberate non-drift. The §16 open questions were resolved as
follows, each pinned by a test unless noted:

1. **OQ-1 (signature preimage):** confirmed as assumed. The signature message
   is exactly `iroh-room-v2/content-event || body_csb` with no length delimiter
   or separate signature domain — `verify_content_event` calls
   `domain::signing_message(CONTENT_EVENT, &event.body_csb)` and verifies under
   the in-body `device_id`. Pinned by the frozen golden vector
   (`normative_body_matches_frozen_crypto_vector`: the deterministic signature
   over the golden body reproduces `GOLDEN_SIGNATURE_HEX`) and by
   `foreign_domain_signature_does_not_replay_under_content_event` (a signature
   over the same body under `GOVERNANCE_ENTRY` does not verify under the
   content-event domain).

2. **OQ-2 (outer envelope / claimed ID):** not added. The serialized envelope
   is exactly `{ body_csb, signature }`; no claimed `EventId` field exists.
   `verify_content_event` recomputes and retains the ID from the exact body
   bytes but performs no claimed-vs-recomputed comparison (the comment at the
   recompute site documents this deferral). A future envelope-shape issue may
   add one; until then, a transport/caller that holds an expected ID compares it
   against `VerifiedContentEvent::id()` itself. Not separately test-pinned
   because there is no field to mismatch.

3. **OQ-3 (chain scope):** resolved as proposed — per `(community_id,
   device_id)`, permitting cross-stream succession. `validate_device_chain_link`
   requires `current.community_id == previous.community_id` and
   `current.device_id == previous.device_id`, with no `stream_id` check.
   Pinned by `chain_rejects_cross_community_author_device_and_sequence_gap`
   (the cross-community successor rejects) and by the five-event happy path
   (`five_event_device_chain_validates_through_public_boundary`, where every
   event shares one `stream_id` but the validator does not assert it).

4. **OQ-4 (author continuity):** resolved as proposed — the successor's
   `author_id` MUST equal the predecessor's. `validate_device_chain_link`
   includes `current.author_id == previous.author_id` in the relational check.
   Pinned by `chain_rejects_cross_community_author_device_and_sequence_gap`
   (the cross-author successor — same device, different `author_id` in the
   body — rejects as `InvalidContent`).

5. **OQ-5 (`auth_hint_seq`):** not interpreted. It is strictly typed (`u64`),
   required, signed, and ID-bound; no stale/future/unavailable semantics are
   applied. Resolution against governance history remains a later authorization
   issue. Covered by the strict-decode matrix (a missing or wrong-typed
   `auth_hint_seq` rejects as `InvalidContent`).

6. **OQ-6 (references semantics):** not invented. `require_references` enforces
   only array shape, exact 32-byte element width, and the `0..=8` cap; caller-
   provided order is preserved byte-for-byte (it is part of the signed bytes and
   the ID). No uniqueness, sorting, self-reference, stream, ancestor, or
   availability rule is applied. Pinned by
   `references_enforce_cap_width_type_and_preserve_order` (lengths 0/1/8 accept,
   9 rejects; wrong-type/wrong-width entries reject; a duplicate-preserving
   array round-trips unchanged) and by `ninth_reference_rejects`.

7. **OQ-7 (kind registry):** the existing eight-kind closed registry
   (`message.text`, `message.reaction`, `message.edited`, `file.shared`,
   `agent.status`, `moderation.block`, `moderation.report`,
   `moderation.remove`) is treated as the normative #134 §9.2 registry. An
   unknown kind rejects as `UnknownContentKind` before any per-kind field
   parsing. Pinned by `unknown_kind_rejected_at_envelope_decode` (provisional
   path) and by the `ContentKind::from_wire` unit test `unknown_kind_rejected`.

8. **OQ-8 (validation precedence):** resolved as proposed. `verify_content_event`
   applies canonical decode (`NonCanonicalEncoding`) → ID recompute → full
   strict body + kind-specific schema validation (`InvalidContent` /
   `UnknownVersion` / `UnknownContentKind`) → signature verification
   (`BadSignature`). The in-body `device_id` is therefore typed and trusted only
   as data before any cryptographic check. Pinned by
   `device_id_width_and_point_split_schema_and_crypto_layers`: a wrong-width
   (31/33-byte) `device_id` rejects at the schema layer as `InvalidContent`
   regardless of the retained signature, while an exact-32-byte `device_id`
   that is not a valid Ed25519 point reaches the crypto layer as `BadSignature`.

9. **OQ-9 (taxonomy mapping):** resolved as proposed — no dedicated chain code
   was added. Canonical missing/unknown/wrong-type/wrong-width/over-cap fields
   and invalid supplied chain data map to `InvalidContent`; malformed
   non-canonical CBOR maps to `NonCanonicalEncoding`; a named-but-unsupplied
   predecessor maps to `MissingDependency`; a signature that fails under
   `device_id` maps to `BadSignature`. Pinned by the strict-decode unit tests
   in `body.rs` (`missing_required_key_rejects`, `wrong_width_id_rejects`,
   `unknown_top_level_key_rejects`, `ninth_reference_rejects`,
   `intrinsic_chain_shape_rejects_mismatch`), the chain matrix in `event.rs`
   (`nonzero_event_without_predecessor_defers` → `MissingDependency`;
   `sequence_gap_rejects` / `wrong_predecessor_id_rejects` /
   `overflow_after_max_seq_rejects` → `InvalidContent`), and the crypto matrix
   (`tampered_body_bytes_reject_as_bad_signature`,
   `wrong_device_key_rejects_as_bad_signature`).

10. **OQ-10 (schema-evolution gate):** not a blocker. `iroh-rooms-v2-core` is
    `publish = false` and unused by the shipped v1 runtime, so the deliberate
    fixture change is an isolated core-research versioning act, not a wire-
    compatibility migration. The v5 entry in `tests/golden/README.md` records
    that no frozen byte/hash/signature vector changed and that the fixture-
    format `schema` marker deliberately stays
    `iroh-rooms-v2-golden-vectors/v2`. The `BLOCKED_CODES` set in
    `signed_records_golden.rs` is unchanged (`wrong_domain` remains the only
    blocked code).

11. **OQ-11 (total body size):** not added. No total canonical-body byte length
    or general collection-length cap was introduced beyond `references <= 8`
    and the existing CBOR depth bound. A normative total-size limit remains an
    explicit follow-up if #134 defines one.

### Additional resolutions against §6 / §7

- **`SignedBody` not implemented for `ContentEventBody` (§6 API invariants):**
  the normative content path uses a concrete envelope rather than the generic
  `signed::SignedBody` trait, because the verification key is the in-body
  `device_id`, not an out-of-body principal signer. `content::provisional`
  retains the trait-based path solely to keep the frozen #153 vector
  byte-stable. Documented at the top of `body.rs`.

- **Genesis predecessor shape (§3.5 / §7):** the intrinsic
  `(device_seq == 0) == prev_device_event.is_none()` check runs at body-decode
  time in `ContentEventBody::from_canonical`, before any crypto. A genesis
  event with a named predecessor, or a successor with a null predecessor,
  rejects as `InvalidContent` before `verify_content_event` reaches the
  signature. Pinned by `intrinsic_chain_shape_rejects_mismatch`.

- **`seal_content_event` key/body device match (§6 / §8 step 6):** local
  sealing runs full strict body validation, then rejects with `BadSignature` if
  `key.device_id() != body.device_id` — the body is never silently rewritten to
  match the key. Pinned by `seal_rejects_key_device_mismatch`.

- **Signature is not part of the ID preimage (§3.3):** `EventId` is derived
  from body CSB only. Re-signing the same body with the same deterministic key
  reproduces both the signature and the ID; mutating only the signature bytes
  leaves ID recomputation unchanged while failing verification. Pinned by
  `signature_is_not_part_of_event_id_preimage` (unit) and by the
  `tampering_and_signature_changes_respect_exact_body_boundary` golden test.

### Verification

`cargo test -p iroh-rooms-v2-core --all-targets --all-features` is green: the
`content/{body,event,validate,registry}.rs` unit tests, the `cbor.rs` canonical-
`null` round-trip and non-null-simple-value rejection tests, the `keys.rs`
device-verifier tests, the focused `tests/content_body_validation.rs` suite
(every issue acceptance criterion driven from raw CSB + signatures through
`seal_content_event` → `verify_content_event` → `validate_device_chain_link`),
and the existing `signed_records_golden.rs`, `identifiers.rs`, `taxonomy.rs`,
and `banned_dependencies.rs` suites, which remain green because no frozen wire
byte, domain string, frozen golden vector, `Reject`-code reachability, or crate
dependency changed.

### Deferred scope (unchanged)

#134 §9.3 deterministic merge rules for conflicting event kinds (possibly #158),
publication certificates (Phase C), #134 §13.4 device-cut construction,
event-set fork resolution or selection among competing successors, reference
fetching / availability / causal resolution / deduplication / semantic merge,
interpretation of `auth_hint_seq` against governance history, device ownership
/ active-revoked status / role / stream authorization (a separate governance
authorization stage), checkpoints / replica receipts / store indexes /
migrations / wire transport / ALPNs / networking / async runtime / clocks /
logging / CLI-SDK exports / deployment, and any change to the v1 protocol or
shipped runtime all remain separate later issues. Cryptographic signature
verification proves control of `device_id` only; it does not prove that the
device belongs to `author_id` or is currently authorized.
