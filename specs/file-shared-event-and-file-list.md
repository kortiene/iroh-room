# Spec: `file.shared` event validation & `file list` close-out (IR-0203)

| | |
|---|---|
| **Issue** | #28 — [IR-0203] Implement `file.shared` event and file list |
| **Parent** | #3 |
| **Labels** | type/feature, area/protocol, area/cli, area/blob, priority/p1, risk/medium |
| **Dependencies** | #27 — [IR-0202] File import into blob store (**landed**); #20 — [IR-0105] Signed message send/receive (**landed**) |
| **Traceability** | `PRD.v0.3.md` §11.2 (File Shared Event), §15.6 (Share File — AC1/AC2/AC3). `PHASE-0-SPIKE.md` Event Protocol §7 (`file.shared` schema), §4 (BLAKE3-256), §6 (stateless validation), §8 (reject taxonomy). |
| **Status** | Planning — spec only. No production code changed by this document. |
| **Type** | Feature close-out + protocol hardening (the residual of a scope that #27 largely pulled forward). |

---

## 1. Summary

Issue #28 asks to "emit a signed `file.shared` event for an imported artifact" and expose
`iroh-rooms file list <room-id>`. **Almost all of that mechanism already landed in #27**
(IR-0202), whose implementer deliberately pulled the event-authoring and `file list` surfaces
forward so that "local provider status is recorded" had an observable home (see
`specs/file-import-into-blob-store.md` §3.2 items 6–7 and §5.1). What #27 did **not** do — it
explicitly declared "Any change to the `file.shared` wire schema … out of scope" (that spec
§3.3) — is the one acceptance criterion #28 uniquely owns:

> **AC4 — Invalid file metadata is rejected.**

Today `file.shared` is the **only** content type whose stateless parser applies *no* semantic
bounds. `message.text` caps its body at `MAX_MESSAGE_BODY_BYTES`; `agent.status` bounds
`progress_pct ≤ 100`; `pipe.opened` rejects an empty `allowed_members`. But
`parse_file_shared` (`crates/iroh-rooms-core/src/event/content.rs:692`) accepts an **empty or
multi-megabyte `name`**, an **empty or malformed `mime_type`**, and — most importantly — a
`size_bytes` of **`u64::MAX`**. The `MAX_SHARED_FILE_BYTES` cap exists but is enforced only
CLI-side against the *local* file's on-disk length; it is **not** applied to the event content
at the trust boundary. A malicious or buggy peer can therefore author a `file.shared` claiming a
10-exabyte file with a garbage name, and every receiving node will validate it, fold it, and
surface it in `file list`.

This issue closes that gap: it graduates `file.shared` from *structurally parsed* to
*semantically validated*, adds the machine-checkable conformance vectors that make "invalid file
metadata is rejected" a first-class protocol guarantee, and adds the CLI/store tests that tie
"a file appears in `file list` **after validation**" to "an invalid `file.shared` never does."
It is a **pure tightening** of the accept predicate — no field is added, removed, renamed, or
re-encoded, so every already-valid event (and every golden vector) stays byte-identical and
still valid.

This document is detailed enough to execute without re-deriving scope.

---

## 2. Relationship to #27 — what is already landed (read first)

**Do not re-implement any of this.** The following all shipped in #27 and satisfy the
corresponding #28 scope items and ACs as noted:

| #28 scope / AC | Status | Where it lives |
|---|---|---|
| File ID generation | **Landed** | `cli/src/file.rs:159` — 16 CSPRNG bytes via `getrandom::fill`; handle `file_<hex>` (`file_handle`). |
| Name, MIME, size, blob hash | **Landed** | `Content::FileShared` (`core/src/event/content.rs:213`); authored by `build_file_shared` (`core/src/event/file.rs`). |
| "Provider endpoint" | **Landed** | `providers: Option<Vec<DeviceKey>>` — `DeviceKey == EndpointId` (content.rs:226 comment; spike §7 "EndpointIds expected to serve it; default `[device_id]`"). `file share` sets `providers = [self device]`. |
| AC1 — references blob hash, not file bytes | **Landed** | `build_file_shared` carries only the `blob_hash` digest (`core/src/event/file.rs:21` doc; PRD §9.2). Bytes live in `<home>/blobs/`, never on the log. |
| AC2 — event is signed and persisted | **Landed** | Signed under the device key, self-validated (`validate_wire_bytes`), fold-checked (`membership.ingest`), and persisted (`store.insert`) — `cli/src/file.rs:179–215`. |
| AC3 — file appears in `file list` after validation | **Landed (mechanism)** | `file list` (`cli/src/file.rs:254`) folds the room, reads `by_type(room, FileShared)`, and prints each row with provider status; `--json` array is stable. **#28 adds the explicit "after validation / invalid-never-listed" test.** |
| `iroh-rooms file list <room-id>` | **Landed** | `cli/src/file.rs:254` + `cli.rs` `FileAction::List`. |
| Event validation for `file.shared` (structural) | **Landed** | `parse_file_shared` enforces field presence, types, fixed lengths (`file_id[16]`, `blob_hash[32]`), `blob_format ∈ {raw, hash_seq}`, unknown-key rejection. |
| **AC4 — invalid file metadata is rejected (semantic bounds)** | **NOT DONE** | `parse_file_shared` has **no** bound on `name`/`mime_type` length or `size_bytes`. **This is #28's deliverable.** |

**Genuine residual owned by #28:**

1. **Semantic content validation** for `file.shared` (name/MIME/size/providers bounds) at the
   stateless trust boundary — AC4.
2. **Conformance vectors** proving each rejection (the §8 taxonomy realization for
   `file.shared`), plus a positive canonical vector.
3. **A store/CLI test** proving an invalid `file.shared` is never persisted and therefore never
   appears in `file list` — tying AC4 ↔ AC3.
4. Docs/README status update; no schema change.

> **Scoping note (surfaced as OQ-1):** because #27 pulled the event + list forward, #28 is a
> hardening/close-out issue, not greenfield. An alternative is to close #28 as "substantially
> delivered by #27" and file a small "harden `file.shared` validation" issue for the AC4
> residual. This spec assumes we keep #28 open and deliver the residual under it (recommended:
> AC4 is a real, unmet, security-relevant gap and #28 is its natural owner).

---

## 3. Background & repository state to read

- `crates/iroh-rooms-core/src/event/content.rs` — `FileShared` struct (`:213`),
  `parse_file_shared` (`:692`), the `Fields` helpers (`:770+`), and the enum tables
  (`MESSAGE_FORMATS`/`BLOB_FORMATS`, `:30`). This is the file the change lands in.
- `crates/iroh-rooms-core/src/event/constants.rs` — `MAX_MESSAGE_BODY_BYTES` (`:28`),
  `MAX_SHARED_FILE_BYTES` (`:37`), `SHORT_ID_LEN`/`DIGEST_LEN`. New caps go here.
- `parse_message_text` (`content.rs:673`) — the **pattern to mirror**: `if body.len() >
  MAX_MESSAGE_BODY_BYTES { return Err(RejectReason::InvalidContent); }`.
- `crates/iroh-rooms-core/tests/conformance/serialization.rs` — the invalid-content vector
  suite: `invalid_content_over_length_body_is_rejected` (`:515`),
  `invalid_content_message_text_bad_format_enum` (`:555`),
  `invalid_content_agent_status_pct_over_100` (`:568`). The new `file.shared` vectors are
  siblings of these, built with the local `parts_result`/`content` helper pattern.
- `crates/iroh-rooms-core/tests/conformance/taxonomy.rs` — the completeness gate.
  `RejectReason::InvalidContent` is **already in `COVERAGE`** (`:104`) and `DEFERRED` is empty
  (`:89`), so **no taxonomy change is required**; adding vectors only strengthens coverage.
- `crates/iroh-rooms-core/tests/conformance/mod.rs` — the §-indexed traceability table (add the
  new `file.shared` vectors under the §7 / content-validation rows).
- `crates/iroh-rooms-core/tests/conformance/fixtures.rs:397` — `file_shared(...)` fixture and the
  pinned `E_file` used by the golden log; `crates/iroh-rooms-core/tests/golden_vectors.rs:1136`
  and `PHASE-0-SPIKE.md:684` pin `E_file.blob_hash = dd101e8f…f313`. **These use a short valid
  name (`report.pdf`) and must remain valid** after the tightening (regression guard, §9).
- `crates/iroh-rooms-cli/src/file.rs` — `share`/`list`, `validate_share_name`/`validate_mime`
  (the *authoring-side* guards; landed). `tests/file_cli.rs` — the CLI suite to extend.
- Workspace lints are strict (`unsafe_code = "forbid"`, clippy `all` + `pedantic`);
  `scripts/verify.sh` is the real CI gate (fmt `--check`, clippy `-D warnings`, `--all-features`
  tests). `cargo test` passing is necessary but not sufficient.

---

## 4. Goal, scope, non-goals

### 4.1 Goal

Make `file.shared` metadata **bounded and well-formed at the stateless trust boundary**, so any
node — regardless of who authored the event or how it arrived — rejects malformed file metadata
with `RejectReason::InvalidContent`, and prove it with conformance vectors and a
"validated-only reaches `file list`" test. Close #28's AC1–AC4 with the residual work #27 left.

### 4.2 In scope

1. **`parse_file_shared` semantic bounds** (`content.rs`): non-empty and length-capped `name`
   and `mime_type` with no control characters; a minimal MIME well-formedness check; an
   **event-level `size_bytes` cap** (`≤ MAX_SHARED_FILE_BYTES`); a bounded, non-empty (when
   present) `providers` array. All returning `RejectReason::InvalidContent`.
2. **New constants** in `event/constants.rs`: `MAX_FILE_NAME_BYTES`, `MAX_MIME_TYPE_BYTES`,
   `MAX_FILE_PROVIDERS` (named, single-source, documented).
3. **Conformance vectors** in `serialization.rs`: one rejection vector per new rule + a positive
   canonical `file.shared` round-trip vector; update the `mod.rs` traceability table.
4. **Store/CLI test** (core `membership_store_e2e.rs` or `cli/tests/file_cli.rs`) proving an
   invalid `file.shared` is rejected by `validate_wire_bytes` and never appears in
   `by_type`/`file list` — AC4 ↔ AC3.
5. **Docs**: a README "Current Status" paragraph for IR-0203; keep `docs/getting-started.md`
   Step 5 truthful (it already documents `file share`/`file list`).

### 4.3 Out of scope / non-goals

- **Any new field or wire-format change to `file.shared`.** This is a validation *tightening*
  only; the CBOR encoding of every valid event is unchanged (golden vectors stay byte-stable).
- **The serve/fetch half** — `file fetch`, the `iroh-blobs` serve ALPN + two-gate ACL, live
  broadcast of the `file.shared` frame at share time, and honest "no-provider" language. This
  is the separately-tracked follow-up (PRD §15.6 AC4–AC6; #27 spec §4.3). #28 stays offline.
- **Receiver-side blob-byte verification on fetch** (BLAKE3 recompute against `blob_hash`) —
  part of the fetch follow-up. #28 validates *metadata*, not *bytes* (bytes aren't present at
  the event layer).
- **`hash_seq`/collection blobs**, blob GC/quotas/revocation — unchanged from #27's non-goals.
- **Rewriting `file share`/`file list`** — they are landed and correct; #28 adds tests and (at
  most) one defensive line (§6.4), not a rewrite.

---

## 5. Design

### 5.1 The validation rules (the heart of #28)

Add these bounds to `parse_file_shared`, mirroring `parse_message_text` and
`parse_pipe_opened`. Every violation → `RejectReason::InvalidContent` (the taxonomy already
covers it; a single code is correct and matches how every other content rule reports).

| Field | Rule | Rationale |
|---|---|---|
| `name` | non-empty; `len() ≤ MAX_FILE_NAME_BYTES`; no `char::is_control` | Empty/huge/newline-laden names pollute `file list` and CBOR; symmetric with the CLI's `validate_share_name` (which only guards *local* authoring). |
| `mime_type` | non-empty; `len() ≤ MAX_MIME_TYPE_BYTES`; no `char::is_control`; **well-formed** (contains exactly one `/` separating a non-empty type and non-empty subtype; ASCII, no whitespace) | A MIME type is displayed and may drive a viewer; garbage/empty is invalid metadata. Keep the check *minimal* to avoid fighting the long tail (OQ-3 covers strict RFC-6838 tokens). |
| `size_bytes` | `≤ MAX_SHARED_FILE_BYTES` | **The load-bearing rule.** Without it a peer asserts an unbounded size; enforcing it at the trust boundary matches the CLI's local cap and the constant's stated intent (`constants.rs:30–37`). |
| `providers` (when present) | non-empty; `len() ≤ MAX_FILE_PROVIDERS` | An explicit empty array violates the §7 "omit-when-empty" canonical rule (the builder already omits it — `file.rs:74`); mirrors `pipe.opened`'s empty-`allowed_members` rejection. Bounds an unbounded array. |
| `file_id`, `blob_hash` | fixed length already enforced (`require_bytes::<16>` / `::<32>`) | No change. All-zero values are *accepted* (OQ-4: no security benefit to rejecting, false-positive risk). |
| `blob_format` | already `∈ {raw, hash_seq}` via `opt_enum` | No change. |

Reference implementation (drop into `parse_file_shared`, replacing the current body):

```rust
fn parse_file_shared(f: &mut Fields<'_>) -> Result<FileShared, RejectReason> {
    let file_id = f.require_bytes::<SHORT_ID_LEN>("file_id")?;

    let name = f.require_text("name")?;
    if name.is_empty()
        || name.len() > MAX_FILE_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(RejectReason::InvalidContent);
    }
    let name = name.to_owned();

    let mime_type = f.require_text("mime_type")?;
    if mime_type.is_empty()
        || mime_type.len() > MAX_MIME_TYPE_BYTES
        || !is_well_formed_mime(mime_type)
    {
        return Err(RejectReason::InvalidContent);
    }
    let mime_type = mime_type.to_owned();

    let size_bytes = f.require_uint("size_bytes")?;
    if size_bytes > MAX_SHARED_FILE_BYTES {
        return Err(RejectReason::InvalidContent);
    }

    let blob_hash = HashRef::from_bytes(f.require_bytes::<DIGEST_LEN>("blob_hash")?);
    let blob_format = f.opt_enum("blob_format", BLOB_FORMATS)?;

    let providers = f.opt_device_array("providers")?;
    if let Some(ps) = &providers {
        if ps.is_empty() || ps.len() > MAX_FILE_PROVIDERS {
            return Err(RejectReason::InvalidContent);
        }
    }

    Ok(FileShared { file_id, name, mime_type, size_bytes, blob_hash, blob_format, providers })
}

/// Minimal MIME well-formedness: `type/subtype`, both non-empty, ASCII, no
/// whitespace or control chars, exactly one `/`. Deliberately permissive on the
/// subtype tail (parameters, `+suffix`) — strict RFC-6838 tokenization is OQ-3.
fn is_well_formed_mime(s: &str) -> bool {
    if !s.is_ascii() || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let mut parts = s.splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(t), Some(sub)) => !t.is_empty() && !sub.is_empty() && !sub.contains('/'),
        _ => false,
    }
}
```

`is_well_formed_mime` is a private free function in `content.rs` (unit-testable in the module's
`#[cfg(test)]`). Keep it dependency-free (no `mime`/`mime_guess` crate — consistent with the
CLI's dependency-free `guess_mime`).

### 5.2 Constants

Add to `crates/iroh-rooms-core/src/event/constants.rs`, documented like `MAX_MESSAGE_BODY_BYTES`:

```rust
/// Maximum byte length of a `file.shared` display `name`. 255 matches the common
/// POSIX `NAME_MAX` and is far above any realistic file name; a longer name is
/// rejected as `invalid_content` at the stateless boundary.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// Maximum byte length of a `file.shared` `mime_type`. Registered media types are
/// short; 255 bounds the field without fighting long parameterized types.
pub const MAX_MIME_TYPE_BYTES: usize = 255;

/// Maximum number of asserted `providers` (`EndpointId`s) on a `file.shared`.
/// MVP rooms are ~3 members; 16 is a generous bound that blocks an unbounded array.
pub const MAX_FILE_PROVIDERS: usize = 16;
```

(`MAX_SHARED_FILE_BYTES` already exists and is reused for the `size_bytes` cap.)

Values are OQ-2 (product sign-off), but each is a named single-source constant so a change is a
one-line edit and cannot silently drift between the CLI cap and the event cap.

### 5.3 Why the stateless validator, not the CLI

AC4 ("invalid file metadata is rejected") is only meaningful at the **receiver** trust boundary.
The CLI's `validate_share_name`/`validate_mime` (landed in #27) guard the *author's own* input,
but they do nothing for a `file.shared` that arrives over the network from a peer — which is
exactly the untrusted path the protocol must defend. `validate_wire_bytes` is the one gate every
event crosses (local self-check on author, and on every inbound frame in the `iroh-rooms-net`
receive path via the sync engine). Putting the bounds in `parse_file_shared` means:

- The net receive path (`Node` pump → engine → `validate_wire_bytes`) rejects a malformed
  peer `file.shared` automatically, increments `counters().rejected`, and emits the
  `event_rejected` audit signal landed in IR-0201 — **no new net code**.
- `EventStore::insert` is only ever reached for validated events, so an invalid `file.shared`
  can never be persisted and therefore never appears in `file list` (AC3 ⟺ AC4).
- The CLI author path already self-validates (`file.rs:194`), so the tightening also protects
  the local author from an internal-bug malformed build (surfaced as the existing "internal
  error" guard).

### 5.4 `file list` — no functional change (one optional defensive line)

`file list` already lists validated events and `continue`s past any row it cannot decode
(`file.rs:281–285`). Because the store holds only validated events, every row already reflects
post-validation state. **Optional hardening (recommended, tiny):** after decoding, run the
already-parsed content through the same bounds (or simply rely on the fact that a stored event
was validated) — no change is strictly required. Recommend **not** adding re-validation to
`file list` (it would duplicate the trust boundary and could mask a store-integrity bug); instead
prove the invariant with the store test in §6.3. Keep `file list` as-is.

---

## 6. Implementation steps

Work top to bottom; each step is independently reviewable and compiles under `core` alone until
the CLI test step.

### 6.1 Step 1 — Constants

Add `MAX_FILE_NAME_BYTES`, `MAX_MIME_TYPE_BYTES`, `MAX_FILE_PROVIDERS` to
`event/constants.rs` (§5.2), each with a doc comment.

### 6.2 Step 2 — `parse_file_shared` bounds + `is_well_formed_mime`

Apply §5.1 to `content.rs`. Add module `#[cfg(test)]` unit tests for `is_well_formed_mime`
(accepts `text/plain`, `application/pdf`, `image/svg+xml`, `application/vnd.api+json`; rejects
``, `plain`, `/plain`, `text/`, `text//plain`, `text/ plain`, `tex t/plain`, non-ASCII). Confirm
the existing `file.rs` builder tests and `fixtures.rs` `E_file` still pass (short valid name/MIME
→ unaffected). Run `cargo test -p iroh-rooms-core`.

### 6.3 Step 3 — Conformance vectors (`serialization.rs` + `mod.rs`)

Add a small helper mirroring the message.text vectors — a `file_shared_content(overrides)` that
builds a canonical `Content::FileShared` CBOR map with all required fields valid, so each vector
perturbs exactly one field. Then one `#[test]` per rule:

- `invalid_content_file_shared_empty_name`
- `invalid_content_file_shared_over_length_name` (`"x".repeat(MAX_FILE_NAME_BYTES + 1)`)
- `invalid_content_file_shared_control_char_name` (`"a\nb"`)
- `invalid_content_file_shared_empty_mime`
- `invalid_content_file_shared_over_length_mime`
- `invalid_content_file_shared_malformed_mime` (`"notamime"`, and one `"text/"`)
- `invalid_content_file_shared_size_over_cap` (`MAX_SHARED_FILE_BYTES + 1`; also assert
  `u64::MAX` rejects)
- `invalid_content_file_shared_empty_providers_array`
- `invalid_content_file_shared_too_many_providers` (`MAX_FILE_PROVIDERS + 1` entries)
- `invalid_content_file_shared_unknown_key`
- `invalid_content_file_shared_wrong_length_file_id` / `_blob_hash` (structural; may already be
  implied — assert explicitly)
- `invalid_content_file_shared_bad_blob_format_enum` (`"tarball"`)
- **`valid_file_shared_round_trips`** — a positive canonical vector: a fully-valid map validates
  and re-encodes byte-identically (the boundary "at the cap succeeds" proof:
  `size_bytes == MAX_SHARED_FILE_BYTES`, name of exactly `MAX_FILE_NAME_BYTES`).

Each asserts `Err(RejectReason::InvalidContent)` (or `Ok` for the positive). Update the §7 /
content-validation rows of the traceability table in `conformance/mod.rs`. No `taxonomy.rs`
change (InvalidContent already covered; DEFERRED stays empty).

### 6.4 Step 4 — Store/CLI "validated-only" test (AC4 ↔ AC3)

Prefer a **core store test** (deterministic, network-free) in
`crates/iroh-rooms-core/tests/membership_store_e2e.rs` (it already builds `file.shared` fixtures,
`:171`): hand-craft an over-cap `file.shared` wire, assert `validate_wire_bytes` → `Err`, assert
it is *not* inserted, and assert `by_type(room, FileShared)` does not contain it while a
sibling valid `file.shared` does. This is the crisp AC4 ↔ AC3 proof.

Additionally, in `crates/iroh-rooms-cli/tests/file_cli.rs`, add
`shared_file_appears_in_list_after_validation`: `file share` a valid small file, then `file list`
(and `--json`) shows exactly that `file_id`/`name`/`blob_hash` with `provider: you (local)` —
making AC3's "after validation" explicit end-to-end. (The CLI cannot easily inject an invalid
event — the author path validates — so the invalid-rejection proof lives in the core test above;
note this in the test comment.)

### 6.5 Step 5 — Docs + gate

Add a README "Current Status" paragraph for IR-0203 (the validation-hardening close-out;
`file.shared` now bounded, conformance-vectored). Verify `docs/getting-started.md` Step 5 stays
truthful (no change expected — no user-visible behavior change for valid files). Run
`scripts/verify.sh` (fmt + clippy `-D warnings` pedantic + `--all-features` tests) — the real
gate.

---

## 7. Error model & observability

- Every new rejection is `RejectReason::InvalidContent` → `.code() == "invalid_content"`, the
  same typed reason the rest of the content taxonomy uses. No new `RejectReason` variant (would
  force a taxonomy-gate change for no benefit; the §8 taxonomy already treats content-shape
  violations as a single code).
- On the **author** path (`file share`), a malformed build trips the existing self-validation
  guard (`file.rs:194`, "internal error: freshly built file.shared failed validation") — but the
  CLI's own `--name`/`--mime` guards make this unreachable in practice; it stays a belt-and-
  suspenders internal-bug signal.
- On the **receive** path, a peer's malformed `file.shared` is dropped by `validate_wire_bytes`,
  counted in `counters().rejected`, appended to the bounded `logs()` ring as `reject.invalid_content`,
  and surfaced via the `event_rejected` audit sink (all landed in IR-0201). No CLI tracing
  subscriber is needed (per the `cli-has-no-tracing-subscriber` memory) — the audit sink already
  makes rejection observable. **No new observability code.**

---

## 8. Test strategy

Maps the issue Test Plan ("Unit tests for event content validation and CLI test for
share/list") to concrete tests.

**Core unit (`content.rs` `#[cfg(test)]`):** `is_well_formed_mime` accept/reject table.

**Core conformance (`serialization.rs`):** the §6.3 rejection vectors + the positive canonical
round-trip (the boundary-succeeds proof). These are the "unit tests for event content
validation" the Test Plan names, made §-traceable.

**Core store (`membership_store_e2e.rs`):** invalid `file.shared` rejected → not inserted → not
in `by_type` (AC4 ↔ AC3), sibling valid one present.

**CLI integration (`file_cli.rs`):** `shared_file_appears_in_list_after_validation` (share→list
happy path with provider status, text + `--json`). The landed #27 suite already covers small
file / missing / unreadable / directory / too-large / hash-verify / non-member / provider
persistence — do not duplicate; add only the "after validation" linkage.

**Regression guards (must stay green, prove the tightening is non-breaking):**
`event/file.rs` golden `event_id` lock, `golden_vectors.rs` `E_file`, `conformance/fixtures.rs`
`file_shared`, `e2e_lifecycle.rs`, `membership_fold.rs`. All use valid short metadata → unaffected.

All tests are offline (no `--peer`, no network) — consistent with the issue's offline test plan.

---

## 9. Security, privacy, reliability, performance, compatibility

- **Security (the point of this issue):** bounding `size_bytes` at the trust boundary removes an
  amplification/absurd-metadata vector (a peer asserting `u64::MAX`); bounding `name`/`mime`/
  `providers` removes log-bloat and display-injection vectors. Contained the same way every
  other content rule is — a pure reject-only predicate, no capability change.
- **Privacy:** unchanged. No new field; the filesystem path is still never on the log (only
  `name`).
- **Reliability:** unchanged runtime behavior for valid events; the store remains the source of
  truth; `file list` behavior for valid files is byte-identical.
- **Performance:** the checks are O(len) over already-in-memory strings; negligible.
- **Compatibility / migration (call out in review):** tightening the accept predicate is a
  **forward-compatibility consideration** — a newer node rejects a `file.shared` an older node
  would have accepted. In Phase-0/1 pre-MVP with no such events in the wild, this is safe; the
  chosen bounds sit far above any legitimate value, and **every existing fixture/golden stays
  valid** (§8 regression guards). No schema version bump and no store migration: `user_version`
  stays `2`; the change is in the stateless parser, not the store. If a previously-persisted
  over-cap event somehow existed, `EventStore::rebuild()`/re-validation would now drop it on
  re-read — acceptable and arguably desirable (it was invalid metadata). Note that
  `validate_wire_bytes` operates on the verbatim signed bytes, so a *validly-signed* older event
  with a legitimate short name is unaffected.

---

## 10. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | The MIME well-formedness check over-rejects a legitimate type (e.g. a rare parameterized type). | Medium | Low | Keep the rule minimal (`type/subtype`, non-empty, ASCII, no whitespace); strict RFC-6838 tokenization is explicitly OQ-3 and *not* done here. Unit-test the accept table with real-world types. |
| R2 | Tightening validation silently invalidates an existing golden/fixture. | Low | High | All fixtures use short valid metadata; §8 lists the exact regression guards to run. Verified in Step 2 before proceeding. |
| R3 | Perceived duplication with #27 → reviewer confusion about why #28 exists. | High | Low | §1–§2 state the overlap explicitly and pinpoint AC4 as the unmet residual; OQ-1 offers the "close as delivered" alternative for a PM call. |
| R4 | Chosen caps (name 255 / mime 255 / providers 16 / size 100 MiB) disagree with product intent. | Medium | Low | Named single-source constants; OQ-2 flags them for sign-off; a change is one line and cannot drift from the CLI cap (they share `MAX_SHARED_FILE_BYTES`). |
| R5 | Scope creep into serve/fetch/broadcast because the issue says "file.shared event". | Medium | Medium | §4.3 draws the boundary; offline test plan enforces it; the follow-up serve/fetch issue owns the network half. |
| R6 | Rejecting an explicit empty `providers` array breaks a hypothetical non-canonical author. | Low | Low | The landed builder already omits empty providers (`file.rs:74`); an explicit empty array is non-canonical per §7 — rejecting it is correct and symmetric with `pipe.opened`. |

---

## 11. Acceptance criteria

Maps issue #28 ACs to this issue's deliverables (noting which #27 already satisfied).

- [ ] **AC1 — `file.shared` references blob hash, not file bytes.** Already true (#27); guarded
  by the existing `build_file_shared` doc/tests and the positive conformance vector (bytes never
  appear in content).
- [ ] **AC2 — Event is signed and persisted.** Already true (#27); `file share` signs under the
  device key, self-validates, and persists. Covered by landed tests; unchanged.
- [ ] **AC3 — File appears in `file list` after validation.** `file list` shows a shared file
  post-validation; the new store test proves an **invalid** `file.shared` is rejected and never
  listed, and the new CLI test proves a valid one is listed with provider status (text + JSON).
- [ ] **AC4 — Invalid file metadata is rejected.** `parse_file_shared` rejects empty/over-long/
  control-char `name`, empty/over-long/malformed `mime_type`, over-cap `size_bytes`, and an
  empty/over-long `providers` array with `RejectReason::InvalidContent`; each is proven by a
  conformance vector; a fully-valid file at the boundary still validates.
- [ ] **AC5 — No regressions / gate green.** `scripts/verify.sh` passes (fmt `--check`, clippy
  `-D warnings` pedantic, `--all-features` tests); **no `file.shared` wire-schema change**; all
  golden/fixture regression guards (§8) stay byte-stable; `user_version` unchanged; the
  serve/fetch boundary (§4.3) remains documented and out of scope.

**Test-plan coverage:** "unit tests for event content validation" → §6.3 conformance vectors +
§6.2 `is_well_formed_mime` unit tests; "CLI test for share/list" → §6.4
`shared_file_appears_in_list_after_validation` (+ the landed #27 suite).

---

## 12. Assumptions

1. #27's producer/list surface (build_file_shared, `file share`, `file list`, file_id, provider
   field) is present on this branch and correct — verified by reading the landed source (§2). #28
   builds on it and does not re-implement it.
2. AC4 ("invalid file metadata is rejected") is genuinely unmet and is #28's core deliverable;
   the other ACs are satisfied by #27 and only need explicit tests. (OQ-1 offers the alternative
   of closing #28 as delivered.)
3. `RejectReason::InvalidContent` is the right (and only needed) code for all new rejections —
   consistent with every other content rule; no new taxonomy variant.
4. The proposed caps (name/mime 255, providers 16, size = `MAX_SHARED_FILE_BYTES` = 100 MiB) are
   acceptable pending sign-off (OQ-2); each is a named constant.
5. A minimal `type/subtype` MIME check is sufficient for MVP; strict RFC-6838 is deferred (OQ-3).
6. Tightening the validator is acceptable in Phase-0/1 (no in-the-wild `file.shared` events with
   out-of-bounds metadata to preserve); no schema/version bump or store migration is needed (§9).

## 13. Open questions

- **OQ-1 (issue framing):** Keep #28 open and deliver the AC4 residual under it (recommended), or
  close #28 as "substantially delivered by #27" and file a focused "harden `file.shared`
  validation" issue for AC4? Product/PM call; the *work* is identical either way.
- **OQ-2 (cap values):** Confirm `MAX_FILE_NAME_BYTES=255`, `MAX_MIME_TYPE_BYTES=255`,
  `MAX_FILE_PROVIDERS=16`, and reuse of `MAX_SHARED_FILE_BYTES` (100 MiB) as the event-level
  `size_bytes` cap. The PRD gives no numbers (its §11.2 example is a 204,800-byte PDF).
- **OQ-3 (MIME strictness):** Minimal `type/subtype` check (recommended) vs. full RFC-6838 token
  grammar (`restricted-name` charset, parameter parsing). Recommend minimal for MVP; the flag/
  guess paths already constrain locally-authored values.
- **OQ-4 (all-zero ids):** Should an all-zero `file_id` or all-zero `blob_hash` be rejected?
  Recommend **no** — a zero `file_id` is a valid handle and a zero `blob_hash` can't be verified
  without bytes; rejecting adds false-positive risk with no security gain.
- **OQ-5 (`file list` re-validation):** Should `file list` defensively re-validate each decoded
  event, or trust the store invariant (only validated events are persisted)? Recommend trust the
  invariant (proven by the §6.4 store test); re-validating would duplicate the boundary and could
  mask a store-integrity bug.
- **OQ-6 (empty providers):** Confirm rejecting an explicit empty `providers` array (canonical
  "omit-when-empty" enforcement, symmetric with `pipe.opened`). Recommend yes; the builder never
  emits one, so only a hand-crafted/non-canonical author is affected.

## 14. Definition of done

1. `parse_file_shared` enforces the §5.1 bounds; `is_well_formed_mime` and the three new
   constants land in core with unit tests.
2. `serialization.rs` has one rejection conformance vector per rule plus a positive canonical
   round-trip (boundary-succeeds) vector; `conformance/mod.rs` traceability table updated;
   `taxonomy.rs` unchanged and still green.
3. A core store test proves invalid `file.shared` → rejected → absent from `by_type`/`file list`
   while a valid sibling is present (AC4 ↔ AC3); a CLI test proves a valid share appears in
   `file list` (text + `--json`) with provider status.
4. All existing golden/fixture regression guards stay byte-stable; no schema/version change;
   `scripts/verify.sh` is green (AC5).
5. README "Current Status" gains an IR-0203 paragraph; `docs/getting-started.md` remains
   truthful; the serve/fetch boundary stays documented and out of scope.
