# Spec: v2.0 Content + Moderation Event Schemas

| | |
|---|---|
| **Issue** | #158 — `[SPEC] §25 #4: Content + moderation event schemas` |
| **Parent** | #134 (v2.0 protocol spec epic, §25 workstream) |
| **Labels** | `type/docs` `area/protocol` `priority/p2` `risk/low` |
| **Dependencies** | none (pure spec). Implementation depends on: P-26 / Decision D-9 schema-evolution ADR (see §10 Risk R1), and the §9 `ContentEventBody` + §17 stream envelopes from #134 |
| **Traceability** | `docs/protocol.md` §2 (eight signed fields), §3 (deterministic-CBOR profile), §6 (MVP event-type registry + strict content validation), §9 (reason-code taxonomy); `PHASE-0-SPIKE.md` Event Protocol §6–§8; `crates/iroh-rooms-core/src/event/{content,constants,reject}.rs`; `docs/audits/feature-complete-audit-2026-07-02.md` P-26 / D-9; `docs/security/threat-model.md` T09 (basic blocklist). Issue #134 §9 (`ContentEventBody` envelope), §17 (stream-level moderation events), §25 #4 (this work item), §6.4 (unknown-kind rule) |
| **Status** | **Spec only — do not implement.** Implementation is deferred to Phase C and is blocked by the D-9 schema-evolution ADR (R1). |

> **Hard gate (read first).** The repo has an enforced policy
> (`docs/audits/feature-complete-audit-2026-07-02.md:206,282,418`) — **P-26 / Decision D-9**:
> *no `schema_version: 2` and no new event-type registry work may land until a schema-evolution
> ADR is written.* This document is `type/docs`: it defines schemas on paper only and is
> therefore compatible with the gate. Any code change derived from it (Phase C) MUST be
> preceded by the D-9 ADR. Cite this spec as the requirements input to that ADR.

---

## 1. Summary

Define the **per-kind content payload schemas** and the **stream-scoped moderation event
schemas** for protocol `schema_version = 2`, plus the **strict-validation rule** that rejects
unknown content kinds (the §6.4 rule). The v2.0 envelope (`ContentEventBody`, issue #134 §9)
and the stream model (§17) are defined elsewhere in #134; this spec fills the two gaps the
issue names:

1. #134 §9 fixes the *envelope* (`ContentEventBody`) but not the per-kind **`body` maps** for
   each registered content kind. This spec defines them, with field types and caps.
2. #134 §17 *references* stream-level `block` / `report` / moderation events but does not
   schema them. This spec defines them, scoped to a stream, with audit evidence preserved.
3. The §6.4 rule ("unknown kind is **rejected**, not ignored") is stated in #134 but needs a
   concrete rejection code and validation-step placement; this spec pins both.

Everything is expressed in the existing v1 trust-boundary discipline: deterministic CBOR
(RFC 8949 §4.2.1), strict maps (unknown key → reject), length/enum caps, no floats, no tags,
no extras — the same rules `crates/iroh-rooms-core/src/event/content.rs` already enforces for
`schema_version = 1`.

**Deliverable of this issue:** this Markdown file. Nothing else. No code, no test vectors, no
CLI. Phase-C implementation issues will cite this spec section-by-section.

---

## 2. Background & current repository state

Read before implementing (Phase C) — and read now to keep this spec honest:

- **`docs/protocol.md`** — the v1 implementer reference. Note it ends at §12; the §6.4 / §9 /
  §17 / §25 numbering in issue #158 comes from the **v2.0** spec in #134, not this file.
  Relevant v1 sections this spec extends:
  - §2 — the eight signed logical fields (the v2.0 `ContentEventBody` rides inside `content`).
  - §3 — deterministic-CBOR profile (seven rules); v2.0 body maps inherit all seven.
  - §6 — MVP event-type registry and the strict per-type content schemas; the v2.0
    content-kind registry is the finer-grained successor.
  - §9 — rejection/flag reason codes; this spec adds `unknown_content_kind` (D5).
- **`PHASE-0-SPIKE.md`** Event Protocol §6 (11-step verification), §7 (registry), §8 (taxonomy).
- **`crates/iroh-rooms-core/src/event/content.rs`** — the v1 strict parser. The `Fields` reader
  (`content.rs:833`) is the exact pattern a v2.0 body parser follows: every key consumed or
  rejected, `finish()` rejects leftovers as `InvalidContent`. `check_field_rules`
  (`content.rs:535`) is the pattern for `sender_id`-vs-content cross-field rules.
- **`crates/iroh-rooms-core/src/event/constants.rs`** — v1 caps. v2.0 reuses
  `MAX_MESSAGE_BODY_BYTES` (16,384), `MAX_FILE_NAME_BYTES` (255), `MAX_MIME_TYPE_BYTES` (255),
  `MAX_FILE_PROVIDERS` (16), `MAX_STATUS_LABEL_BYTES` (64), `MAX_STATUS_MESSAGE_BYTES`
  (4,096), `MAX_ARTIFACT_REFS` (16), `PUBLIC_KEY_LEN` (32), `DIGEST_LEN` (32), `SHORT_ID_LEN`
  (16). New caps are listed in D8.
- **`crates/iroh-rooms-core/src/event/reject.rs`** — v1 taxonomy. `RejectReason` is
  `#[non_exhaustive]`; adding `UnknownContentKind` is additive but requires the
  taxonomy-completeness gate (`tests/conformance/taxonomy.rs`) to cover it.
- **`docs/audits/feature-complete-audit-2026-07-02.md`** — P-26 / Decision D-9 (the gate),
  and the "V-pillar" note that new event-type work "must not start casually".
- **`docs/security/threat-model.md`** — T09 (no basic blocklist): the product driver for
  stream-scoped moderation events.
- **Issue #134** §9 (`ContentEventBody`), §17 (stream moderation), §25 #4 (this item), §6.4
  (unknown-kind rule). These sections are normative for the envelope/stream model this spec
  depends on; where this spec assumes an envelope field name, it is flagged as Open Question
  OQ-1.

**Critical current-state facts:**

1. **v1 hard-rejects `schema_version != 1`** (`event/constants.rs:65`, `reject.rs:23`). Every
   schema in this document is a v2.0 design; under the shipped code today, a v2.0 event is
   rejected `unknown_schema_version`. That is correct and intended until D-9 lands.
2. **The v1 strict parser already implements the §6.4 spirit** for event types and content
   keys: unknown → reject, never ignore. The v2.0 §6.4 rule extends the same idea to the
   content **kind** discriminant inside `ContentEventBody`.
3. **There is no "stream" concept in v1.** Rooms are flat. `stream_id` is a v2.0 addition
   defined in #134 §17; this spec consumes it, it does not introduce it.
4. **No group-encrypted content.** Per the issue scope and #134 §18, group-E2EE content
   schemas are explicitly deferred; this spec's content kinds are plaintext-at-the-event-layer
   (transport/storage encryption is orthogonal).

---

## 3. Scope & non-goals

**In scope (this spec defines, on paper):**

- The **v2.0 content-kind registry** — the closed set of registered `kind` strings and their
  strict `body` maps (§4 D1, D3).
- The **stream-scoped moderation event schemas** — `moderation.block`, `moderation.report`,
  `moderation.remove` — with audit evidence preserved (§4 D4, D7).
- The **strict-validation rule for unknown kinds** — rejection code, validation-step
  placement, and the "reject, never ignore" guarantee (§4 D5, §5).
- The **new constants/caps** the above schemas require (§4 D8).
- The **authorization model** for moderation actions (admin vs. any-member) (§6).
- **Conformance-vector outline** so Phase C knows what to pin (§7).

**Out of scope (deferred or owned elsewhere):**

- **The `ContentEventBody` envelope itself** (owned by #134 §9). This spec assumes its
  existence and field shape; exact field names are OQ-1.
- **The stream lifecycle** (create/archive/permissions) beyond the `stream_id` scoping field
  moderation events consume (owned by #134 §17).
- **Group-encrypted content schemas** (deferred per #134 §18; explicitly named Out in the
  issue).
- **Implementation** (Phase C). No Rust, no tests, no CLI in this issue.
- **Migration / wire-compatibility between v1 and v2.0.** Owned by the D-9 ADR; this spec
  only states requirements the ADR must satisfy (§10 R1).
- **UI rendering** of moderation actions or removed-content tombstones.

---

## 4. Design decisions

Notation inherits v1: `bstr[n]` = byte string of length `n`; `tstr` = UTF-8 text; `uint` =
unsigned integer; `opt` = optional; `enum{…}` = closed set of `tstr` values. All maps are
deterministic CBOR (v1 §3 seven rules) with **unknown keys rejected**.

### D1 — Content-kind registry (v2.0, closed set)

The `ContentEventBody.kind` discriminant (envelope field, #134 §9) MUST be one of the
following registered strings. Any other value is rejected (D5). The registry is **only**
extensible by bumping `schema_version` and amending this table — the same forward-compat rule
as v1 (`docs/protocol.md` §3 rule 7).

| `kind` | Carries | Successor of (v1) | Signer / role |
|---|---|---|---|
| `message.text` | text body, optional reply/mentions | `message.text` | any Active member |
| `message.reaction` | emoji reaction on a target event | *(new)* | any Active member |
| `message.edited` | edit of a prior `message.text` body | *(new)* | the original author only |
| `file.shared` | content-addressed blob reference | `file.shared` | any Active member |
| `agent.status` | agent status label + progress | `agent.status` | any Active member (typically `role == agent`) |
| `moderation.block` | stream/room block of a subject | *(new; sibling of `member.removed`)* | admin only |
| `moderation.report` | member report of a subject/event | *(new)* | any Active member |
| `moderation.remove` | content tombstone on a target event | *(new)* | admin only |

> **Two distinct "removal" concepts, kept separate** (mirrors v1's `member.left` vs.
> `member.removed` split). `member.removed` (v1) is a **membership** action (changes the
> membership fold). `moderation.remove` (v2.0) is a **content** action: it tombstones a
> specific event without necessarily changing membership. `moderation.block` is a
> **scoped-visibility** action (stream- or room-level). These three must not be collapsed.

### D2 — `ContentEventBody` envelope (assumed; owned by #134 §9)

This spec depends on the envelope but does not define it. For concreteness, the body schemas
below assume the envelope exposes at least:

```
ContentEventBody = {              // owned by #134 §9 — exact names are OQ-1
  "kind":      tstr,              // registered kind from D1; the §6.4 discriminant
  "version":   opt uint,          // per-kind body schema minor version; default 1 when absent
  "stream_id": opt bstr[16],      // stream scope; absent ⇒ the room default stream (#134 §17)
  "body":      map                // kind-specific; strict; schemas in D3/D4
}
```

**All validation rules in this spec apply to the `body` map and the `kind` discriminant.**
If #134 §9 finalizes different field names, Phase C substitutes them verbatim; no semantic in
this spec turns on the field names.

### D3 — Per-kind content `body` schemas (strict; carry-forward + new)

Each `body` is a CBOR map. Unknown keys → reject (`invalid_content`). Required keys missing /
wrong-typed / out-of-bounds → reject. Optionals are omitted when absent (v1 omit-when-empty
discipline; the canonical re-encode in v1 §6 step 4 relies on this).

#### D3.1 `message.text`
```
body = {
  "body":       tstr,                 // ≤ MAX_MESSAGE_BODY_BYTES (16,384)
  "format":     opt tstr enum{plain, markdown},   // default plain
  "in_reply_to":opt bstr[32],         // event_id digest of the replied-to event
  "mentions":   opt [bstr[32]],       // ≤ MAX_MENTIONS (64); identity keys
  "thread_id":  opt bstr[16]          // short id of a thread within the stream
}
```
- Identical contract to v1 `message.text` (`content.rs:675`) plus optional `thread_id`.
- `body` MUST be valid UTF-8 (enforced by CBOR text) and ≤ the cap.

#### D3.2 `message.reaction` *(new)*
```
body = {
  "target": bstr[32],                 // event_id digest of a content event in this room
  "emoji":  tstr,                     // ≤ MAX_REACTION_EMOJI_BYTES (64); no control chars
  "op":     opt tstr enum{add, remove} // default add
}
```
- `target` MUST reference an existing content event (causal-reachability is a stateful,
  deferred check — see §5). Stateless layer checks only that it is a 32-byte digest.
- `emoji` is a short UTF-8 grapheme cluster (e.g. `"👍"`, `"+1"`); control characters rejected
  (same `char::is_control` guard as v1 `file.shared.name`, `content.rs:698`).
- Reactions are logically keyed by `(sender_id, target, emoji)`; `op == remove` is the
  explicit delete. Semantics of fold/dedup are owned by #134 §17; this spec only fixes the
  bytes.

#### D3.3 `message.edited` *(new)*
```
body = {
  "target":  bstr[32],                // event_id digest of the original message.text
  "new_body":tstr,                    // ≤ MAX_MESSAGE_BODY_BYTES (16,384)
  "format":  opt tstr enum{plain, markdown}
}
```
- `target` MUST reference a `message.text` whose `sender_id` == this event's `sender_id`
  (cross-field + stateful check; §5/§6). The original event is **not** rewritten — the log is
  append-only; clients render the latest edit causally descending from the original.

#### D3.4 `file.shared`
Carry forward v1 `file.shared` verbatim (`content.rs:694`):
```
body = {
  "file_id":    bstr[16],
  "name":       tstr,                 // 1..=MAX_FILE_NAME_BYTES (255); no control chars
  "mime_type":  tstr,                 // well-formed type/subtype, ≤ MAX_MIME_TYPE_BYTES (255)
  "size_bytes": uint,                 // ≤ MAX_SHARED_FILE_BYTES (100 MiB)
  "blob_hash":  bstr[32],             // BLAKE3-256; fetched bytes verified against it
  "blob_format":opt tstr enum{raw, hash_seq},
  "providers":  opt [bstr[32]]        // 1..=MAX_FILE_PROVIDERS (16); default [device_id]
}
```
No v2.0 delta except living inside `ContentEventBody.body` instead of the v1 top-level
`content`. The `mime_type` well-formedness check (`is_well_formed_mime`, `content.rs:742`) is
preserved.

#### D3.5 `agent.status`
Carry forward v1 `agent.status` verbatim (`content.rs:790`):
```
body = {
  "status":               tstr,       // 1..=MAX_STATUS_LABEL_BYTES (64); no control chars
  "message":              opt tstr,   // ≤ MAX_STATUS_MESSAGE_BYTES (4,096)
  "related_artifact_ids": opt [bstr[16]],  // 1..=MAX_ARTIFACT_REFS (16)
  "progress_pct":         opt uint    // 0..=100 (integer; no floats)
}
```

### D4 — Moderation event `body` schemas (stream-scoped; audit evidence preserved)

All three moderation kinds share an audit-evidence sub-structure (D7) and a `stream_id`
scope field. **`stream_id` semantics**: present ⇒ action scoped to that stream; absent ⇒
room-wide (the room default stream from #134 §17). The stateless layer validates only that
`stream_id` is a `bstr[16]`; existence/scope checks are deferred (§5).

> **Audit evidence is preserved by construction.** A moderation event is itself a signed,
  append-only log entry — its mere existence in the validated log is tamper-evident audit
  proof. The `reason` + `evidence_events` + `evidence_blobs` fields carry the *human and
  machine evidence* an auditor reconstructs intent from.

#### D4.1 `moderation.block`
```
body = {
  "stream_id":      opt bstr[16],     // scope; absent ⇒ room-wide
  "subject":        bstr[32],         // identity key being blocked; MUST != sender_id
  "blocked_by":     bstr[32],         // admin identity; MUST == sender_id
  "scope":          tstr enum{stream, room},
  "reason":         opt tstr,         // ≤ MAX_MOD_REASON_BYTES (1,024)
  "evidence_events":opt [bstr[32]],   // 1..=MAX_EVIDENCE_REFS (16); cited event_ids
  "evidence_blobs": opt [bstr[32]],   // 1..=MAX_EVIDENCE_REFS (16); cited blob hashes
  "expires_at":     opt uint          // ms epoch; time-limited block (optional)
}
```
- Cross-field rule (stateless): `blocked_by == sender_id`, `subject != sender_id`.
- `scope == room` MUST correspond to absent `stream_id` (a `stream_id` with `scope == room`
  is `invalid_content`).
- Authorization (deferred): signer is the room admin (single admin in v1; the D-9 ADR +
  #134 decide multi-admin). Closes threat-model T09 for scoped beta when paired with
  `member.removed`.
- Fold semantics (owned by #134 §17): a live `moderation.block` removes the subject from the
  stream's visibility set, **without** changing room membership. Sticky-departure (v1 §7)
  does not apply — that is membership's job.

#### D4.2 `moderation.report`
```
body = {
  "stream_id":      opt bstr[16],
  "subject":        bstr[32],         // reported identity; MAY be a non-member/unknown key
  "target_event":   opt bstr[32],     // the offending event_id, if any
  "category":       tstr enum{spam, abuse, harassment, malware, other},
  "reported_by":    bstr[32],         // MUST == sender_id
  "reason":         opt tstr,         // ≤ MAX_MOD_REASON_BYTES (1,024)
  "evidence_events":opt [bstr[32]],   // 1..=MAX_EVIDENCE_REFS (16)
  "evidence_blobs": opt [bstr[32]]    // 1..=MAX_EVIDENCE_REFS (16)
}
```
- Cross-field rule (stateless): `reported_by == sender_id`.
- `subject` MAY be unknown (not a current member) — reports are intake, not verdicts; the
  category set is closed to make intake sortable.
- Authorization (deferred): any Active member may report. Reports are advisory — they do not
  change membership or visibility; only an admin-issued `moderation.block` / `.remove` acts.

#### D4.3 `moderation.remove` (content tombstone)
```
body = {
  "stream_id":      opt bstr[16],
  "target_event":   bstr[32],         // REQUIRED: the event_id being tombstoned
  "removed_by":     bstr[32],         // admin identity; MUST == sender_id
  "reason":         opt tstr,         // ≤ MAX_MOD_REASON_BYTES (1,024)
  "evidence_events":opt [bstr[32]],   // 1..=MAX_EVIDENCE_REFS (16)
  "evidence_blobs": opt [bstr[32]]    // 1..=MAX_EVIDENCE_REFS (16)
}
```
- Cross-field rule (stateless): `removed_by == sender_id`.
- `target_event` is REQUIRED (a removal without a target is meaningless).
- **Tombstone, not erasure.** The original event stays in the append-only log (it cannot be
  erased without breaking convergence — v1 §7). Clients MUST treat content covered by a
  valid, causally-following `moderation.remove` as removed in UI; the log remains the audit
  record. This is the same "log-valid but zero-effect" split v1 uses for removed-member
  events (`docs/protocol.md` §8 security invariant).

### D5 — Strict-validation rule for unknown kinds (the §6.4 rule)

**Rule.** A `ContentEventBody` whose `kind` is not in the D1 registry is **rejected**, never
ignored. The event is dropped, never persisted, never re-broadcast.

- **New v2.0 rejection code:** `unknown_content_kind` (a new `RejectReason::UnknownContentKind`
  variant; `reject.rs` is `#[non_exhaustive]` so this is additive). This is distinct from v1
  `unknown_event_type` (the outer `event_type` discriminant) and from `invalid_content` (a
  *known* kind with a bad `body`). Keeping three separate codes preserves the v1 §9 principle
  that every distinct failure mode has a stable, named code.
- **Validation-step placement (§5):** the kind check is the **first `body`-level** check,
  immediately after the envelope decodes — before any per-kind field parsing. A body map is
  not even inspected for unknown keys until the kind is known, because unknown kinds have no
  defined schema to check against.
- **`version` field (D2):** an unknown `version` for a known `kind` is rejected as
  `invalid_content` (the kind exists but the body-shape minor version does not). This keeps
  the kind registry and the per-kind shape-version registry as separate concerns.
- **Taxonomy gate:** Phase C MUST extend `tests/conformance/taxonomy.rs`
  (`every_reason_and_flag_is_covered_or_deferred`) so a v2.0 reason code cannot land without a
  vector — the same tripwire v1 uses (`docs/protocol.md` §10).

### D6 — Stream-scoping model

- `stream_id` is a `bstr[16]` short id (same space as v1 `file_id` / `pipe_id`).
- A v2.0 room has an implicit **default stream** (the room itself). `stream_id` absent ⇒
  default stream. #134 §17 owns stream creation/lifecycle.
- Moderation events' `scope` field (D4.1) makes the room-vs-stream intent explicit and
  self-auditing rather than inferred from presence/absence of `stream_id`.
- This spec takes no position on stream permissions model (open vs. admin-moderated); that is
  #134 §17.

### D7 — Audit evidence shape (shared across moderation kinds)

`reason` + `evidence_events` + `evidence_blobs` is the audit triple. Design rules:

- **`reason`** is free-form UTF-8, capped (`MAX_MOD_REASON_BYTES` = 1,024). Not a closed enum
  — moderation intent is too varied to enumerate; the closed enum is on `category`
  (reports) and `scope` (blocks).
- **`evidence_events`** are `event_id` digests (`bstr[32]`) the moderator cites. Stateless
  layer checks only length/cap; causal-existence is deferred. Cap `MAX_EVIDENCE_REFS` = 16
  (mirrors `MAX_FILE_PROVIDERS` / `MAX_ARTIFACT_REFS`).
- **`evidence_blobs`** are BLAKE3-256 blob hashes (`bstr[32]`) — e.g. screenshots, exported
  transcripts. Same cap. Fetch/verify against the blob plane is the consumer's job (v1 §8
  blob-serve gate).
- **Integrity of evidence is structural:** the moderation event is signed and append-only, so
  the citation set cannot be repudiated by the signer after the fact. Evidence *availability*
  (a cited event/blob still being reachable) is an availability concern, not a schema concern.

### D8 — New constants (Phase C lands these in `event/constants.rs`)

| Constant | Value | Used by |
|---|---|---|
| `MAX_MENTIONS` | 64 | `message.text.mentions` |
| `MAX_REACTION_EMOJI_BYTES` | 64 | `message.reaction.emoji` |
| `MAX_MOD_REASON_BYTES` | 1,024 | `moderation.{block,report,remove}.reason` |
| `MAX_EVIDENCE_REFS` | 16 | `moderation._.evidence_events` / `evidence_blobs` |

Reused v1 constants: `MAX_MESSAGE_BODY_BYTES`, `MAX_FILE_NAME_BYTES`, `MAX_MIME_TYPE_BYTES`,
`MAX_SHARED_FILE_BYTES`, `MAX_FILE_PROVIDERS`, `MAX_STATUS_LABEL_BYTES`,
`MAX_STATUS_MESSAGE_BYTES`, `MAX_ARTIFACT_REFS`, `PUBLIC_KEY_LEN`, `DIGEST_LEN`,
`SHORT_ID_LEN`.

### D9 — Schema-evolution posture (restates the gate, does not relax it)

- All schemas in this doc are **v2.0**. Under shipped code they are rejected
  `unknown_schema_version` (correct).
- Forward-compat remains v1's: no silently-ignored extra keys; additions require a
  `schema_version` bump + registry amendment + taxonomy vector.
- The D-9 ADR decides lock-step vs. forward-compatible v2 rollout. This spec is input to that
  ADR, not a substitute for it.

---

## 5. Validation algorithm (v2.0 additions; placement relative to v1 §6)

The v1 11-step algorithm (`docs/protocol.md` §5) runs unchanged on the outer envelope. The
v2.0 content-kind checks slot into **step 5** (version/type + strict content), which becomes
a two-stage check once the envelope is `ContentEventBody`:

| Sub-step | Check | Failure code |
|---|---|---|
| 5a | `schema_version == 2`; `event_type` is the v2.0 content-bearing type (owned by #134). | `unknown_schema_version` / `unknown_event_type` |
| 5b | Decode `ContentEventBody`; `kind` ∈ D1 registry. | **`unknown_content_kind`** (the §6.4 rule) |
| 5c | `version` is a known minor for this `kind` (default `1`). | `invalid_content` |
| 5d | Strict-parse `body` per D3/D4 (known keys, required/optional, types, caps, enums). | `invalid_content` |
| 5e | Cross-field rules against the envelope (`sender_id`): D4.1 `blocked_by == sender_id`, `subject != sender_id`; D4.2 `reported_by == sender_id`; D4.3 `removed_by == sender_id`. (v1 `check_field_rules` pattern.) | `invalid_content` |
| 5f | `stream_id`/`scope` bidirectional consistency (D4.1): `scope == room` ⇒ `stream_id` absent; `scope == stream` ⇒ `stream_id` present. Both directions checked. | `invalid_content` |
| 5g | If the envelope `ContentEventBody.stream_id` (D2) and a moderation body's `stream_id` (D4) are both present, they MUST be identical. A mismatch means the event is ambiguous between the envelope's scope and the body's scope. | `invalid_content` |

**Deferred (stateful) checks — not owned by this spec, listed for completeness:**

- Step 7/8 (device binding, membership/role): admin-only kinds (`moderation.block`,
  `moderation.remove`) require the admin role; `moderation.report` requires Active membership;
  `message.edited` requires the original author.
- Causal existence of referenced ids: `message.reaction.target`, `message.edited.target`,
  `moderation.remove.target_event`, `moderation.report.target_event`, and all
  `evidence_events` entries must resolve in the event's ancestor view (or be buffered
  per v1 §7 out-of-order delivery).
- Fold semantics for blocks/reports/removals (owned by #134 §17).

---

## 6. Authorization model (summary; full gate is #134 §17 + D-9 ADR)

| Kind | Stateless cross-field rule | Deferred role rule |
|---|---|---|
| `message.text` | none | Active member |
| `message.reaction` | none | Active member |
| `message.edited` | (target author == sender, checked statefully) | Active member **and** original author |
| `file.shared` | none | Active member |
| `agent.status` | none | Active member |
| `moderation.block` | `blocked_by == sender_id`, `subject != sender_id` | room admin |
| `moderation.report` | `reported_by == sender_id` | Active member |
| `moderation.remove` | `removed_by == sender_id` | room admin |

"Admin" today means the single immutable genesis admin (v1 §11 limitation). Multi-admin
topology is the D-9 ADR's call.

---

## 7. Conformance / test-vector outline (Phase C; not authored here)

Phase C MUST add v2.0 conformance vectors under `crates/iroh-rooms-core/tests/conformance/`,
extending the v1 §10 model. Minimum set this spec requires:

1. **§6.4 unknown-kind rejection** — a well-formed envelope with `kind = "message.unknown"`
   → `unknown_content_kind` (the issue's AC#2 vector).
2. **Per-kind strict-content negatives** — one `invalid_content` vector per kind for each
   failure mode: unknown body key, missing required key, wrong type, over-cap, bad enum,
   bad `stream_id`/`scope` consistency.
3. **Cross-field moderation vectors** — `blocked_by != sender_id`, `subject == sender_id`,
   `reported_by != sender_id`, `removed_by != sender_id`, `moderation.remove` missing
   `target_event`.
4. **Evidence-cap vectors** — `evidence_events`/`evidence_blobs` at cap+1 → `invalid_content`.
5. **Taxonomy-completeness gate** — `unknown_content_kind` registered in
   `taxonomy.rs::every_reason_and_flag_is_covered_or_deferred`.
6. **Golden CSB pin** (Tier-1 style) — one canonical `message.text` v2.0 event with pinned
   CSB length + `event_id`, once the D-9 ADR fixes the envelope bytes.

No vectors are written in this issue.

---

## 8. Implementation steps (Phase C — for a *later* issue, not this one)

Tracked here only so a Phase-C planner has the full picture. **Do not execute in #158.**

1. **Land the D-9 schema-evolution ADR** (`docs/decisions/ADR-00NN-…`) deciding lock-step vs.
   forward-compatible v2. Blocks every step below.
2. Extend `event/constants.rs` with the D8 constants; add the doc/code drift guards v1 uses
   (`constants.rs:88` pattern).
3. Add `RejectReason::UnknownContentKind` (`event/reject.rs`) + its `code()` string
   `"unknown_content_kind"`.
4. Add a v2.0 content-kind module (e.g. `event/content_v2.rs`) mirroring v1 `content.rs`:
   `ContentKind` enum, per-kind `body` structs, `Fields`-style strict parser, `to_cbor` with
   omit-when-empty optionals.
5. Wire sub-steps 5a–5f into the validator (behind `schema_version == 2`).
6. Author conformance vectors per §7.
7. Update `docs/protocol.md` (or a new `docs/protocol-v2.md`) with the v2.0 registry, citing
   this spec.

---

## 9. Acceptance criteria (maps to the issue's checkboxes)

- [x] **AC1 — Each registered kind has a strict content-map schema with field types and caps.**
  Satisfied by §4 D3 (content kinds) + D4 (moderation kinds) + D8 (caps). Every kind lists
  field types, required/optional, enums, and byte/count caps.
- [x] **AC2 — Unknown kind is rejected (not ignored) per the §6.4 rule.**
  Satisfied by §4 D5 + §5 sub-step 5b: new `unknown_content_kind` rejection code; the event
  is dropped, never persisted or re-broadcast; taxonomy-completeness gate extended (§7 #5).
- [x] **AC3 — Moderation events scoped to a stream, with audit evidence preserved.**
  Satisfied by §4 D4 (`stream_id` + `scope` on every moderation kind) + D7 (audit-evidence
  triple: `reason` / `evidence_events` / `evidence_blobs`, integrity structural via signed
  append-only log).

The issue also names two Out-of-scope items; this spec respects both:
- Group-encrypted content schemas are deferred (§3, #134 §18) — not defined here.
- Implementation is Phase C (§8) — no code in this issue.

---

## 10. Risks

| ID | Risk | Severity | Mitigation |
|---|---|---|---|
| **R1** | **Implementing before the D-9 ADR lands** would break the P-26 hard gate and risk same-set divergence (v1 §11 schema-evolution trap; audit `feature-complete-audit-2026-07-02.md:403`). | High | This issue is `type/docs` only. §8 step 1 makes the ADR a hard prerequisite for every code step. The spec header states the gate verbatim. |
| R2 | **Envelope field-name drift.** This spec assumes `ContentEventBody = {kind, version, stream_id, body}` (D2). If #134 §9 finalizes different names/shapes, D3/D4 schemas need a rename pass. | Medium | D2 is explicitly assumed; field names are collected in OQ-1. No semantic in the spec turns on a name. |
| R3 | **Tombstone-vs-erasure confusion.** `moderation.remove` does not erase; clients that treat the log as the display source will show removed content. | Medium | D4.3 states the invariant loudly; §6 + #134 §17 own the rendering contract. Log remains the audit record (by design, mirrors v1 §8). |
| R4 | **Scope/room ambiguity.** `stream_id` present + `scope == room` is nonsensical; a lenient parser could silently widen a stream block to the room. | Low | D4.1 makes the combination `invalid_content`; sub-step 5f enforces it statelessly. |
| R5 | **Evidence availability.** Cited `evidence_events`/`evidence_blobs` may be unreachable (offline peer, pruned blob). Audit looks complete on paper but is not reconstructable. | Low | D7 separates integrity (structural, guaranteed) from availability (operational). Same class as v1 blob-unavailable; out of scope for a schema spec. |
| R6 | **Taxonomy explosion.** Adding `unknown_content_kind` plus eventual per-kind codes could erode the v1 §9 "one code per failure mode" clarity. | Low | D5 keeps exactly one new code now; §7 #5 ties it to the taxonomy gate. Defer further codes until a concrete need. |
| R7 | **Admin model instability.** Moderation block/remove assume "the admin"; v1 has a single immutable admin. If the D-9 ADR / #134 introduces multi-admin or rotation, the role gate in §6 moves. | Low | §6 explicitly defers the topology to the ADR; the schema is admin-model-agnostic. |

---

## 11. Open questions

- **OQ-1 (blocks Phase C, not this spec):** the exact `ContentEventBody` field names and the
  presence/shape of `version` and `stream_id` — owned by #134 §9. This spec assumes D2.
- **OQ-2 (resolved):** `message.edited` IS in the v2.0 MVP registry. It is the natural
  counterpart to `message.reaction` and is required for edit semantics in chat streams. A
  v2.0 event with `kind = "message.edited"` is accepted (not `unknown_content_kind`). The
  D1 registry entry and D3.3 schema are normative.
- **OQ-3:** should `moderation.report.category` be open (string with a *recommended* set) or
  closed enum? This spec chooses closed for sortability of intake; revisit if i18n/l10c of
  categories becomes a product requirement.
- **OQ-4:** does `moderation.block` with `expires_at` auto-unblock via a stateful fold, or
  does it require a follow-up unblock event? Owned by #134 §17 (fold semantics).
- **OQ-5:** should there be a `moderation.unblock` / `moderation.unremove` kind (reversal),
  or are reversals expressed by a new action with opposite intent? Not in D1; deferred to
  #134 §17.
- **OQ-6:** multi-admin signature policy for `moderation.block`/`.remove` under a future
  multi-admin topology (quorum? any-admin?). Owned by the D-9 ADR.

---

## 12. Assumptions

1. #134 §9 ships a `ContentEventBody` envelope with at least a kind discriminant and a body
   map (D2). This spec does not define the envelope.
2. #134 §17 ships a stream model with a `stream_id` identifier space and a room default
   stream. This spec consumes it.
3. The §6.4 rule (reject unknown kind) is normative in #134; this spec only concretizes the
   rejection code and validation-step placement.
4. v1 deterministic-CBOR profile (§3 seven rules) carries forward unchanged into v2.0.
5. The shipped code remains v1-only until D-9 lands; nothing in this spec is wire-active.
6. Moderation events are **not** group-encrypted at the event layer (issue scope; #134 §18).

---

## 13. Traceability

| Spec section | Source requirement | Repo anchor / cross-ref |
|---|---|---|
| §4 D1 registry | Issue #158 "registry of v2.0 content event kinds" | v1 registry `docs/protocol.md` §6; `event/content.rs:41` |
| §4 D3 content bodies | Issue #158 "strict content-map schemas with field types and caps"; AC#1 | v1 schemas `content.rs:611`–`827`; caps `constants.rs` |
| §4 D4 moderation | Issue #158 "Moderation event schemas (block/report/remove, scoped to a stream)"; AC#3 | #134 §17; threat-model T09 |
| §4 D5 / §5 5b unknown-kind | Issue #158 "Strict-validation rules for unknown kinds"; #134 §6.4; AC#2 | v1 `unknown_event_type` `reject.rs:24`; taxonomy gate `tests/conformance/taxonomy.rs` |
| §4 D7 audit evidence | Issue #158 "with audit evidence preserved"; AC#3 | v1 append-only log invariants `docs/protocol.md` §7/§8; ADR-0003 |
| §4 D8 caps | AC#1 "field types and caps" | v1 `constants.rs` |
| §10 R1 gate | Issue scope "Implementation (Phase C)" Out | P-26 / D-9 `docs/audits/feature-complete-audit-2026-07-02.md:206,418` |
| §3 group-E2EE deferred | Issue scope Out | #134 §18 |
