# Implementer Protocol Documentation (IR-0302 / #37)

- **Issue:** #37 — [IR-0302] Write implementer protocol documentation
- **Labels:** `type/docs` `area/protocol` `area/dx` `priority/p1` `risk/medium`
- **Parent:** #4
- **Depends on:** #7 / IR-0003 (protocol conformance test vectors — landed), #34 / IR-0209 (full-demo e2e + `docs/getting-started.md` reconciliation — landed)
- **Traceability:** `PRD.v0.3.md` §18.6 (Protocol Ambiguity Risk); `PHASE-0-SPIKE.md` → *Event Protocol* (§1–§8), *Membership & Ordering Model* (§0–§9), *Protocol Test Vectors* (§1–§20).
- **Kind:** documentation-only deliverable. **No production code changes.** One new Markdown file plus small cross-link edits to existing docs.

---

## 1. Summary

Produce a single, concise, implementation-oriented protocol reference —
`docs/protocol.md` — that an engineer or agent can use to build (or re-implement, or
audit) an interoperable Iroh Rooms peer **without reading the whole `PHASE-0-SPIKE.md`**.

`PHASE-0-SPIKE.md` is ~980 lines of decision records, spike plans, residual-risk essays,
and normative protocol text interleaved. The normative wire/signature/membership contract
is buried inside it. IR-0302 extracts *only the normative contract* into a focused
reference: the wire format, canonical serialization rules, ID/signature derivations, the
membership fold, the reason-code taxonomy, and how to run the conformance vectors — each
item stated once, precisely, and cross-linked to (a) its authoritative spike section, (b)
the shipped code that implements it, and (c) the conformance test that pins it.

The spike stays the **authoritative normative source**; `docs/protocol.md` is the
**navigable implementer view** of it. Where they could drift, the doc points back to the
spike (byte-level golden values) and to the landed code/tests (which are already reconciled
against the spike) rather than restating derivations a reader might trust and get wrong.

This is a **docs consolidation + navigation** deliverable, not new protocol design. Every
fact in the target doc already exists in `PHASE-0-SPIKE.md` and is enforced by landed code
under `crates/iroh-rooms-core/`. Writing anything the spike/code does not already say is
out of scope and must instead be filed as a finding (see §10).

---

## 2. Goal and non-goals

### Goal
A new `docs/protocol.md` that:

- Fits the four ACs: concise & implementation-oriented; test vectors linked and runnable;
  security-critical invariants called out explicitly; MVP limitations distinguished from
  the long-term roadmap.
- Is a **reference** (skim-and-jump), not a tutorial. `docs/getting-started.md` already
  owns the end-to-end runnable walkthrough (#34); `docs/protocol.md` owns the byte-level
  contract. They cross-link; they do not duplicate.
- Is self-contained enough that a reader never *has* to open `PHASE-0-SPIKE.md` to
  implement a conforming peer — but always *can*, via a section-precise pointer, when they
  want the full rationale or the adversarial-review reasoning.
- Passes the `writing-guidelines` review (voice/tone/clarity) and the project verify gate
  (docs are not compiled, but no code changes ⇒ `scripts/verify.sh` must stay green).

### Non-goals
- **No production code changes** under `crates/*/src/`. If the doc cannot be written
  accurately because the code diverges from the spike, that divergence is a **finding filed
  as a separate issue**, not a fix in this PR (§10).
- **No new protocol design.** No new event types, reason codes, constants, or serialization
  rules. The doc describes MVP `schema_version = 1` exactly as shipped.
- **Not a spike rewrite.** `PHASE-0-SPIKE.md` is not edited or replaced; it remains the
  normative source and the home of the decision records and residual-risk analysis.
- **Not the conformance suite.** The runnable vectors already landed in #7
  (`tests/protocol_conformance.rs`); this issue *links* them, it does not add tests.
- **No multi-file docs site / generator.** One Markdown file (plus optional appendix in the
  same file). No tooling, no doc build step.

---

## 3. Source-of-truth inventory (read these before writing)

The doc must be reconciled against these; every claim traces to one of them. Do **not**
paraphrase derivations from memory — copy the load-bearing values from the spike/code.

| Source | What it authoritatively gives you |
|---|---|
| `PHASE-0-SPIKE.md` *Event Protocol* §1–§8 | Key model, signed-field set, canonical CBOR profile, event-ID/room-ID derivations, signature payload, 11-step verification algorithm, the MVP event-type registry (per-type `content` schemas), and the §8 reason/flag taxonomy. **Normative.** |
| `PHASE-0-SPIKE.md` *Membership & Ordering* §0–§9 | Convergence guarantee (hedged form), derived Lamport clock + total order, roles, membership events, the deterministic fold (§3.4), the authorization gate (§3.5), sticky departure (§3.7), attribute merge (§3.8), out-of-order pipeline (§4), connect-time ACLs (§5), key-bound invites (§6), equivocation handling (§7). |
| `PHASE-0-SPIKE.md` *Protocol Test Vectors* §1–§20 + *Fixtures* | The byte-exact golden values (CSB, `event_id`, signatures, `room_id_A/B`, tampered id, cross-room id) and the 20 normative GIVEN/WHEN/THEN vectors. |
| `PRD.v0.3.md` §18.6 | The five protocol-ambiguity mitigations the doc must visibly satisfy (canonical serialization decided; signature payload documented; schemas versioned; test vectors created; invalid/unknown-critical fields rejected deterministically). Also §11 (Data Model, product-level envelope view) and §13/§14 (security & availability model). |
| `crates/iroh-rooms-core/src/event/reject.rs` | Shipped `RejectReason` (14) + `Flag` (3) with `.code()` strings — the exact spellings the doc's taxonomy table must match. |
| `crates/iroh-rooms-core/src/event/constants.rs` | Shipped structural limits and context strings the doc must cite verbatim (below). |
| `crates/iroh-rooms-core/src/event/{wire,cbor,ids,signed,validate}.rs` | The implementation of the envelope/codec/derivation/verification the doc summarizes; link, don't restate line-by-line. |
| `crates/iroh-rooms-core/src/membership/{fold,model,access}.rs` | The shipped fold, snapshot shape, and access predicates the fold-summary section describes. |
| `crates/iroh-rooms-core/tests/conformance/mod.rs` | The authoritative vector→test map and taxonomy-coverage note; the doc's "runnable vectors" section mirrors this table and links each row. |
| `README.md` "Error codes" table + `crates/iroh-rooms-cli/src/error.rs` | The CLI exit-category taxonomy that *wraps* `RejectReason::code()` — for the doc's note on how protocol codes surface to a CLI user. |
| `docs/getting-started.md` | The runnable end-to-end demo the doc cross-links to (do not duplicate its content). |

### Load-bearing constants to cite verbatim (from `constants.rs`)
The doc MUST use these exact values (a reader who hardcodes a wrong bound gets a
non-interoperable peer):

- Context strings (ASCII, no NUL): `EVENT_CONTEXT = "iroh-rooms:event:v1"`,
  `ROOMID_CONTEXT = "iroh-rooms:room-id:v1"`, `BIND_CONTEXT = "iroh-rooms:device-binding:v1"`,
  `INVITE_CONTEXT = "iroh-rooms:invite:v1"`.
- `SCHEMA_VERSION = 1`, `WIRE_VERSION = 1`.
- `MAX_PREV_EVENTS = 20`; `CLOCK_SKEW_FUTURE_MS = 300_000`.
- `MAX_MESSAGE_BODY_BYTES = 16_384`; `MAX_SHARED_FILE_BYTES = 104_857_600` (100 MiB);
  `MAX_FILE_NAME_BYTES = 255`; `MAX_MIME_TYPE_BYTES = 255`; `MAX_FILE_PROVIDERS = 16`;
  `MAX_STATUS_LABEL_BYTES = 64`; `MAX_STATUS_MESSAGE_BYTES = 4096`; `MAX_ARTIFACT_REFS = 16`.
- Sizes: `PUBLIC_KEY_LEN = 32`, `SIGNATURE_LEN = 64`, `DIGEST_LEN = 32`, `SHORT_ID_LEN = 16`.
- Crypto: Ed25519 (RFC 8032), BLAKE3-256, deterministic CBOR (RFC 8949 §4.2.1).

---

## 4. Deliverable: `docs/protocol.md` — required structure and content

Target length ~500–700 lines (reference density, not essay). Each section below lists the
**facts the section must contain** and the **pointers it must carry**. Write prose around
these; do not omit any listed fact and do not add un-sourced claims.

### 0. Front matter
- One-paragraph statement of what the doc is (implementer reference for MVP
  `schema_version = 1`) and what it is not (not the rationale — that's the spike; not the
  walkthrough — that's `docs/getting-started.md`).
- A "Normative source & precedence" note: **`PHASE-0-SPIKE.md` is authoritative** for
  byte-level values; where this doc and the spike disagree, the spike wins and the
  disagreement is a bug to file. The landed code and conformance suite are already
  reconciled to the spike.
- A short "How to read this doc" list of the six scope areas with in-page anchors.

### 1. Identity & key model  *(spike Event Protocol §1)*
- The three keys: `sender_id` (identity, 32 B Ed25519 — authorization/membership tracked
  against **this** key; never signs events in MVP), `device_id` (32 B Ed25519, **byte-for-byte
  the iroh `EndpointId`** — the key that signs events and is the transport/ACL identity),
  `room_id` (32 B BLAKE3 digest, §5 derivation).
- MVP invariant: exactly **one `device_id` per `sender_id`** (multi-device deferred).
- Device binding: `binding_msg = BIND_CONTEXT ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)`;
  `binding_sig = Ed25519_sign(identity_secret, binding_msg)`; accept a device iff it verifies
  under `sender_id`. Binding travels in `room.created` / `member.joined` / `member.removed`
  content (`device_binding = { identity_key, device_key, sig }`).
- **Security callout:** signature verifies under `device_id`; authorization is judged against
  `sender_id`; the binding is the only bridge between them. Agents are ordinary principals
  (same keys, same binding rule; only `role` differs).

### 2. Event envelope & wire format  *(spike §2, §3; code `event/wire.rs`, `signed.rs`)*
- The **eight signed logical fields** and only these are covered by the signature and the
  `event_id`: `schema_version`, `room_id`, `sender_id`, `device_id`, `event_type`,
  `created_at`, `prev_events`, `content`. Give the CBOR type and one-line meaning of each
  (table). State explicitly: **no `lamport` field on the wire** (it is derived); `event_id`
  and `signature` are **not** signed over.
- `created_at` is **advisory/display only** — never trusted for ordering, authorization, or
  to mutate the validated set. Flag this in-line; it is a repeat security invariant.
- `prev_events`: array of raw 32-byte BLAKE3 digests; `[]` **only** for `room.created`;
  every other event non-empty and transitively reaching genesis; **max 20** (`MAX_PREV_EVENTS`).
- The `WireEvent` transport/storage envelope: `{ "v": 1, "signed": bstr(==CSB verbatim),
  "sig": bstr[64], "id": tstr("blake3:<hex>") }`. State the two rules that make it safe: a
  receiver **hashes/verifies the exact `signed` bytes, never re-encodes**; `"id"` is an
  advisory cache key that MUST be recomputed and checked, never trusted.
- Presentation rule: 32-byte IDs are lowercase hex (64 chars) in CLI/JSON; hash-typed IDs use
  the `blake3:<64-hex>` named form.

### 3. Canonical serialization rules  *(spike §3; code `event/cbor.rs`)*
- Name the profile: **deterministic CBOR, RFC 8949 §4.2.1**. List all seven MUST rules
  verbatim in meaning (map keys sorted by encoded form / length-first then bytewise;
  shortest-form ints; definite-length only; no duplicate keys; no tags, no floats; UTF-8
  text; **exactly the eight top-level keys**, no extras).
- Give the **fixed canonical top-level key order** implementations may hardcode:
  `content, room_id, device_id, sender_id, created_at, event_type, prev_events, schema_version`.
- Define **CSB** (canonical signed bytes) as the resulting octet string.
- **Security callout:** canonicality is enforced *independently of signature* — a receiver
  re-canonicalizes the decoded `signed` and rejects if it does not byte-equal `signed`, so
  event identity stays 1:1 with logical content even if a non-canonical encoding's signature
  happens to verify. Non-conforming encoding ⇒ `non_canonical_encoding`.
- Anchor: the golden CSB is **242 bytes**, begins `a867636f6e74656e74…`; link vector §1.

### 4. Event ID & room ID derivation  *(spike §4, §5; code `event/ids.rs`, `genesis.rs`)*
- `event_id = "blake3:" ‖ lowercase_hex(BLAKE3-256(CSB))`. Named content hash — never a
  sender-chosen UUID/ULID/random. Equal `event_id` ⇒ same event (first valid copy wins).
- `room_id = BLAKE3-256(ROOMID_CONTEXT ‖ creator_sender_id(32) ‖ room_nonce(16) ‖
  created_at_be(8))`, recomputed from `room.created` and rejected (`room_id_mismatch`) if the
  envelope disagrees.
- Pin the golden anchors from the spike Fixtures so a reader can self-check:
  `room_id_A = 43c19f2e…16a3` (creator seed `01`, nonce `000102…0e0f`,
  `created_at = 1750000000000`); golden `event_id = blake3:c389e2…85a1`. Link vectors §3, §4.

### 5. Signature payload & verification  *(spike §6; code `event/signed.rs`, `validate.rs`)*
- Signature payload: `sig_msg = EVENT_CONTEXT ‖ CSB`; `signature = Ed25519_sign(device_secret,
  sig_msg)`; verifies under `device_id` (**never `sender_id`** — call out the "verify under the
  wrong field" bug explicitly, vector §5).
- Reproduce the **ordered 11-step verification algorithm** as a numbered checklist (decode
  transport → recompute id → verify sig → enforce canonicality/exactly-8-keys → version/type +
  strict content → room binding → device binding → membership & role → causal structure incl.
  genesis-descent (9a) → clock sanity (advisory only) → dedup & persist). For each step, note
  the reason code emitted on failure. Mark steps 7–8 as the **stateful** checks (need the
  membership oracle); steps 1–6, 9, 10 as the **stateless** subset.
- **Security callout:** step 8 authorization is judged against the event's **own fixed causal
  ancestors**, not the receiver's live snapshot — this is what makes verdicts
  arrival-order-independent (link the membership section).

### 6. MVP event-type registry  *(spike §7; code `event/{genesis,invite,join,left,removed,message,file,pipe,status}.rs`)*
- The registry table: each `event_type`, required signer/role, and `prev_events` rule
  (`room.created`, `member.invited`, `member.joined`, `member.left`, `member.removed`,
  `message.text`, `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status`).
- Per-type `content` schema, condensed to the field list + type + the bound that matters
  (cite the constants from §3 above: body ≤ 16384; file name/mime ≤ 255; providers ≤ 16;
  status label ≤ 64 / message ≤ 4096 / artifact refs ≤ 16; short ids = 16 B). Do not re-derive
  — copy from the spike §7 schemas and the shipped parsers.
- Callout: **removal is two distinct types** (`member.left` = voluntary self-leave, signer ==
  subject; `member.removed` = admin kick) — not one type with a discriminator.
- Callout: strict content validation rejects unknown content keys / missing / wrong-type /
  out-of-bound as `invalid_content`; forward-compatible fields arrive only by bumping
  `schema_version`.

### 7. Membership fold & ordering summary  *(spike Membership & Ordering §0–§4, §7; code `membership/fold.rs`, `model.rs`)*
- **Convergence guarantee — state the hedged form only:** *"Any two honest peers holding the
  identical validated event set compute byte-identical membership state and timeline."*
  Explicitly warn that the unqualified "equivocation cannot cause divergence" is **false**;
  convergence is conditional on set-completeness, and the protocol guarantees only
  ancestor-completeness, never concurrent/sibling-completeness.
- Derived **Lamport clock**: `lamport(genesis) = 0`; `lamport(e) = 1 + max(lamport(parents))`;
  recomputed from signed `prev_events`, not on the wire. **Total order** = ascending
  `(lamport, event_id)`, `event_id` compared bytewise over 32 raw digest bytes; always a
  linear extension of the DAG.
- **Security callout:** timeline position carries **no trust** — an author can grind `content`
  or pick `prev_events` to pin their event first/last; no logic or UI may attach meaning to
  position.
- **The fold (§3.4)**, per subject X: collect valid authorized events touching X; take causal
  heads; **Removed-dominates** (any head `member.removed`/`member.left` ⇒ Removed; else a
  `member.joined` head ⇒ Active; else invite-only ⇒ Invited); attributes by least-privilege
  merge (`agent < member < admin`, role → least-privileged; capability scope → intersection;
  tie-break by lowest `event_id`).
- **Authorization gate (§3.5)** and **sticky departure (§3.7)**: invite/remove require admin;
  leave requires self; a join must causally descend from a **still-live** invite for its key;
  any `member.removed` **or** `member.left` consumes prior authorizations, so re-admission
  needs a fresh post-departure invite. Single immutable admin = genesis signer.
- **Out-of-order handling (§4):** child-before-parent is **buffered and backfilled**, never
  rejected for "unknown parent"; note the anti-amplification bounds (signer pre-check, park
  caps, backfill quota) at a summary level.
- Point to the shipped `MembershipSnapshot` (per-identity status/role/bound device) and the
  access predicates `blob_serve_allowed` / `pipe_connect_allowed` (which consult the **current
  snapshot**, not the ancestor view). Keep this a *summary* — link the code for detail.

### 8. Connect-time authorization (blob & pipe)  *(spike Membership & Ordering §5)*
- Brief: enforcement is at QUIC connect-accept time against the **current local snapshot**,
  default-deny, fail-closed on suspected incompleteness (known-higher admin tip, or a
  same-`admin_seq` fork ⇒ CRITICAL `equivocation` + fail closed on affected subjects).
- Blob serve gate: connect only from an Active member's proven endpoint; serve a hash only if
  referenced by a valid causally-visible `file.shared`; verify fetched bytes' BLAKE3-256 ==
  `blob_hash`.
- Pipe connect gate: all of {remote ∈ Active, remote ∈ `allowed_members` (no default-all),
  `pipe.opened` by its Active owner, no known `pipe.closed`, unexpired}; the **one** place a
  local wall clock is read, and only to deny.
- **Security callout:** revocation-on-learn — live connections are torn down as soon as the
  enforcing peer learns of a removal/close/expiry; exposure is bounded by *removal-event
  reachability*, not by "briefly."

### 9. Rejection / flag reason codes  *(spike §8; code `event/reject.rs`)*
- A single table with the **exact `.code()` spellings** (do not invent):
  - **Rejections (14, event dropped):** `unknown_schema_version`, `unknown_event_type`,
    `non_canonical_encoding`, `id_mismatch`, `bad_signature`, `unbound_device`, `not_a_member`,
    `insufficient_role`, `room_id_mismatch`, `invalid_content`, `expired_invite`,
    `bad_capability`, `too_many_parents`, `not_genesis_descended`.
  - **Ignored (not an error):** `duplicate`.
  - **Advisory flags (event still accepted, ordered, persisted):** `clock_skew`,
    `equivocation`, `from_removed_member`.
- For each row: one-line meaning + which verification step / layer emits it (stateless vs
  deferred/membership), mirroring the `reject.rs` doc comments.
- **Security callout:** advisory flags **never** affect the validated set, ordering, or any
  authorization/expiry verdict. A `clock_skew` event is still accepted.
- Cross-reference how these surface to a CLI user: the `iroh-rooms` binary wraps
  `RejectReason::code()` into the exit-category taxonomy (`error[<code>]` / `warning[<code>]`,
  exit 0–6) — link `README.md` "Error codes" and note the split is verbatim (a `room join`
  failure and a `room tail` receive-path drop report the identical code).

### 10. Test vectors — linked and runnable  *(AC2; #7 conformance suite)*
- State that the 20 normative vectors (spike §1–§20) are implemented one-`#[test]`-per-vector.
- Give the **runnable commands** (verified against the repo):
  - Full conformance suite: `cargo test -p iroh-rooms-core --test protocol_conformance --all-features`
  - Golden-value regression grab-bag: `cargo test -p iroh-rooms-core --test golden_vectors`
  - Full gate (fmt + clippy + all tests): `scripts/verify.sh`
- Reproduce the **vector → test-fn → module** map (copy from
  `crates/iroh-rooms-core/tests/conformance/mod.rs`), each row linking the test file. Note the
  two golden tiers: Tier-1 independently-reproduced byte-exact values (hard NO-GO on mismatch)
  vs Tier-2 regenerated-and-pinned fixture-log ids.
- Note the **taxonomy completeness gate** (`conformance/taxonomy.rs`): every `RejectReason`/
  `Flag` is covered by a named vector or an explicit (empty) `DEFERRED` list, so a new code
  cannot land undocumented — the doc's §9 table therefore cannot silently fall out of date.

### 11. MVP limitations vs roadmap  *(AC4; spike §0, §3.6, §5, §6; PRD §13.4/§13.5)*
- A two-column table (or two clearly-labeled lists): **"MVP limitation (`schema_version = 1`,
  today)"** vs **"Long-term / roadmap (deferred)."** Populate from the spike, e.g.:
  - Single immutable admin (no co-admins / transfer) → multi-admin as a grow-only/quorum CRDT
    or signed successor list (post-MVP).
  - One device per identity → multi-device key set.
  - No key rotation; removal cannot cryptographically erase already-received data; enforcement
    is fail-closed-at-connect + tear-down-on-learn, **bounded by removal-event reachability**.
  - Key-bound invites only (open/bearer excluded because they defeat sticky-kick); no native
    revocation beyond removing the subject; `max_uses` not convergently enforceable → invite
    treated as single-key, reusable until expiry. Roadmap: admin-signed `cap_id` revocation +
    first-key-binding for open tickets.
  - Removed-member timeline pollution (a removed member can keep authoring log-valid,
    zero-capability events) → UI segregation / admin tombstone (recommended, non-blocking).
  - Segregated admin fork detectable only via admin-tip advertisement; residual = a removal
    held only by offline/withholding peers is irreducible without an availability assumption.
  - Recent-history chat sync is count-bounded; the membership sub-DAG + full admin chain are
    **never windowed** (a hard invariant).
- Each limitation must cite its spike section so the reader can get the full reasoning.

### 12. Appendix / pointers
- A compact table: doc section → spike section → implementing module(s) → conformance test.
- Links: `PHASE-0-SPIKE.md`, `PRD.v0.3.md` §18.6, `docs/getting-started.md`, the conformance
  suite, `crates/iroh-rooms-core/`.

---

## 5. Cross-link edits to existing docs (minimal)

- `README.md`: add one bullet under "Getting Started" (or a short "Protocol" line pointing at
  `docs/protocol.md`) so the implementer reference is discoverable next to
  `docs/getting-started.md`. Keep it to one or two lines; do not restructure the README.
- `docs/getting-started.md`: add a single "See also: `docs/protocol.md` for the byte-level
  protocol contract" pointer near its top. Do not move content between the two files.
- Optional: a pointer near the top of `PHASE-0-SPIKE.md`'s *Event Protocol* section noting
  that `docs/protocol.md` is the condensed implementer view. Only if it reads naturally;
  editing the spike is otherwise out of scope.

---

## 6. Implementation steps (ordered, for the writer)

1. **Read the sources in §3** — especially the three spike sections and `reject.rs` /
   `constants.rs` / `conformance/mod.rs`. Do not start writing from memory of this spec; open
   the spike and copy exact values.
2. **Scaffold `docs/protocol.md`** with the §4 section skeleton (headings + anchors + the
   pointer/appendix table stub).
3. **Fill the six scope sections** (envelope/wire; canonical serialization; ID & signature;
   event-type registry; membership fold; reason codes) using the §4 fact lists. For every
   numeric bound and context string, paste from `constants.rs`; for every golden value, paste
   from the spike Fixtures/vectors.
4. **Insert the security-invariant callouts inline** (AC3) using a consistent visual marker
   (e.g. a `> **Security invariant:**` blockquote) at each point listed in §4 and consolidated
   in the §7 checklist below. Verify all invariants in that checklist appear.
5. **Write the "Test vectors" section** (AC2): the runnable commands and the vector→test map
   copied from `conformance/mod.rs`, with working relative links to the test files.
6. **Write the "MVP limitations vs roadmap" section** (AC4) from §4.11, each row citing a
   spike section.
7. **Add the appendix pointer table** and the cross-link edits in §5.
8. **Reconcile:** walk the §8 verification checklist below — every fact matches the spike;
   every code string matches `reject.rs`; every command runs; every link resolves.
9. **Style pass:** run the `writing-guidelines` review over `docs/protocol.md`; tighten for
   concision (AC1 — it is a reference, not an essay).
10. **Verify green:** run `scripts/verify.sh` (no code changed, so it must stay green) and
    run the two `cargo test` commands the doc cites to confirm they exist and pass, so the doc
    ships only commands that actually work.

---

## 7. Security-critical invariants checklist (AC3)

The doc MUST call out each of these explicitly (this list is the AC3 acceptance oracle):

1. Signature verifies under `device_id`, **never** `sender_id`; authorization is against
   `sender_id`; the device binding is the only bridge.
2. Receivers hash/verify the **exact `signed` bytes** and never re-encode to verify.
3. `"id"` is advisory — always recomputed and checked, never trusted (`id_mismatch`).
4. Canonicality is enforced **independently of the signature**; non-canonical ⇒ reject even
   if the signature verifies.
5. `created_at` and any local wall clock are **advisory only** — never affect the validated
   set, ordering, authorization, or expiry (the one exception: pipe connect may read the local
   clock **only to deny** on expiry). `clock_skew` is a flag, never a reject.
6. Authorization verdicts are judged against an event's **own fixed causal ancestors**, giving
   arrival-order independence.
7. `room_id` is inside the signed bytes ⇒ cross-room replay is cryptographically impossible
   without re-signing (`room_id_mismatch`).
8. Departure is **sticky**: leave and removal both consume prior authorizations; re-admission
   requires a fresh post-departure invite. Key-bound invites make ban-evasion under a fresh
   key impossible.
9. Access control uses the **current snapshot**, not the ancestor view: a since-removed
   member's log-valid events grant **zero capabilities**; enforcement is fail-closed +
   tear-down-on-learn, bounded by removal-event reachability.
10. Convergence is stated **only in its hedged, set-conditional form**; the unqualified claim
    is flagged as false.
11. Timeline position carries no trust.
12. `duplicate` is ignored (idempotent), not an error; advisory flags never change state.

---

## 8. Verification / review plan (Test Plan: "review against conformance suite and spike")

Because this is a docs deliverable, "tests" are reconciliation checks, not new code:

- **Spike reconciliation:** for each of the six scope areas, diff the doc's stated rule
  against its authoritative spike section. Every derivation formula, context string, field
  set, and golden value matches character-for-character. Any mismatch is either a doc bug
  (fix here) or a code/spike divergence (file a finding, §10) — never silently smoothed over.
- **Code reconciliation:** every reason/flag code in the doc equals a `RejectReason::code()`
  / `Flag::code()` in `reject.rs`; every numeric bound equals a `constants.rs` value. (A cheap
  way to catch drift: grep the doc's code strings against `reject.rs`.)
- **Conformance reconciliation:** the doc's vector→test table equals the table in
  `conformance/mod.rs`; run `cargo test -p iroh-rooms-core --test protocol_conformance
  --all-features` and `--test golden_vectors` and confirm both pass, so the "runnable"
  claim is true at merge.
- **AC checklist:** confirm all four ACs (concise/impl-oriented; vectors linked & runnable;
  security invariants explicit — the §7 checklist; MVP-vs-roadmap distinguished).
- **Style gate:** `writing-guidelines` review pass.
- **CI gate:** `scripts/verify.sh` green (no code touched).

---

## 9. Acceptance criteria mapping

| Issue AC | How this spec satisfies it |
|---|---|
| Protocol docs are concise and implementation-oriented | Single reference file (§4), ~500–700 lines, fact-list-driven, reference (not tutorial) framing; the runnable walkthrough stays in `docs/getting-started.md`. Style gate in §6/§8. |
| Test vectors are linked and runnable | §4.10 reproduces the vector→test map from `conformance/mod.rs` with links and the two verified `cargo test` commands; §8 runs them at merge. |
| Security-critical invariants are called out explicitly | §4 inline `> **Security invariant:**` callouts, consolidated and acceptance-checked in the §7 checklist. |
| Docs distinguish MVP limitations from long-term roadmap | §4.11 dedicated two-column limitations-vs-roadmap section, each row citing its spike source. |

---

## 10. Risks & mitigations

- **R1 — Doc drifts from code/spike over time.** Mitigation: the doc points to code/tests for
  the volatile detail rather than restating it; the §9 taxonomy completeness gate (#7) already
  fails CI if a reason code is added without coverage, and the appendix pointer table makes the
  spike section the citable source. Consider (non-blocking) a follow-up that adds a
  `#[doc]`-style or CI grep check that the doc's code strings are a subset of
  `RejectReason::code()`.
- **R2 — Writer restates a derivation and gets a byte wrong**, producing a
  non-interoperable "reference." Mitigation: §3/§4 mandate copying exact values from the
  spike/`constants.rs`; §8 mandates character-for-character reconciliation and running the
  vectors.
- **R3 — Discovered spike↔code divergence.** If the shipped code contradicts the spike while
  writing (e.g. a bound or code string differs), do **not** paper over it: file a separate
  issue and document the as-shipped behavior with a note, per the #7 precedent (findings are
  filed, not silently fixed).
- **R4 — Scope creep into a spike rewrite or new design.** Mitigation: non-goals (§2) are
  explicit; the doc extracts and navigates, it does not add protocol.
- **R5 — Duplication with `docs/getting-started.md`.** Mitigation: strict division of labor
  (contract vs walkthrough) and cross-links (§5); reviewer checks no walkthrough content
  leaks into `protocol.md`.

---

## 11. Assumptions

1. **Target path is `docs/protocol.md`** — a single sibling of the existing
   `docs/getting-started.md`. (No `docs/` sub-tree or multi-file split; that would be
   scope creep for a p1 docs task.)
2. The doc covers **only MVP `schema_version = 1`** exactly as shipped in
   `crates/iroh-rooms-core`; future schema versions are named only in the roadmap section.
3. `PHASE-0-SPIKE.md` remains the normative source and is **not** rewritten; at most it gains
   a one-line pointer to the new doc.
4. The landed code and #7 conformance suite are already reconciled to the spike (per README
   and `conformance/mod.rs`), so citing them is safe; the doc's job is navigation + extraction,
   not re-verification of the protocol itself.
5. "Runnable" (AC2) means the vectors are executable via the two documented `cargo test`
   commands on the standard toolchain; no new harness is required.
6. This ADW phase does no git/GitHub work; the spec is the sole artifact of this run.

---

## 12. Open questions

1. **Doc home & naming.** `docs/protocol.md` vs `docs/protocol-reference.md` vs a
   `docs/protocol/` folder. Recommendation: single `docs/protocol.md` (assumption 1). Confirm
   with the maintainer if a docs sub-tree is preferred.
2. **Depth of the fold/ordering summary.** How much of Membership & Ordering §0–§9 belongs in
   `protocol.md` vs a pointer to the spike? Recommendation: summarize §0 (convergence),
   §2 (ordering), §3.4–§3.8 (fold/gate/attributes), §4 (out-of-order), §5 (connect ACLs) at
   reference depth and link the rest. Confirm the fold summary should stay a *summary*, not a
   second full spec.
3. **Include a worked byte-level example?** A single fully-worked "encode → CSB → `event_id`
   → sign → `WireEvent`" trace using the golden vector §1 values would be high-value for an
   implementer but adds length. Recommendation: include one compact worked example (it directly
   serves AC1's "implementation-oriented"); flag if length is a concern.
4. **CI guard for doc/code code-string drift (R1).** Worth a tiny follow-up test that greps
   `docs/protocol.md`'s reason codes against `RejectReason::code()`? Out of scope for this
   docs-only issue; note as a candidate follow-up.
5. **PRD §11 vs spike §7 envelope.** The PRD's product-level envelope view (§11.1) is a
   simplified subset of the spike's normative eight-field envelope. The doc follows the spike
   (normative). Confirm no reader is expected to treat PRD §11 as the wire contract.
