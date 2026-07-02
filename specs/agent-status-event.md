# Spec: Implement `agent.status` Event (IR-0208)

| | |
|---|---|
| **Issue** | #33 — `[IR-0208] Implement agent.status event` |
| **Parent epic** | #3 |
| **Labels** | `type/feature` `area/protocol` `area/cli` `area/agent` `priority/p1` `risk/medium` |
| **Dependencies** | #32 (IR-0207, agent invite flow — **landed**), #20 (IR-0105, signed message send/receive — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §11.4, §15.8 (AC3), §16; Spike `PHASE-0-SPIKE.md` Event Protocol §7 (`agent.status`), §8 (reject taxonomy), §3 rule 5 (no floats → integer percent) |
| **Owning crates** | `crates/iroh-rooms-cli` (the new `agent status` command + its offline-first/online-best-effort orchestration); small additive pieces in `crates/iroh-rooms-core::event` (a pure `build_agent_status` builder + tightened `parse_agent_status` bounds + new constants) and the `room.rs` tail display |

> **Status:** planned — this document is the build plan. Do **not** treat any code as landed
> beyond what §2.1 lists. The compiled binary is the source of truth.

---

## 1. Summary

Let an active room member (typically `role == "agent"`) post a signed `agent.status` event —
a short status label plus an optional human message, optional integer progress percent, and
optional references to already-shared files — into the room, and render it in the room tail.

```bash
# Post a status (offline-first: always persisted locally; best-effort pushed to online peers)
iroh-rooms agent status <ROOM_ID> "running_tests" \
  --message "Running integration tests" \
  --progress 40 \
  --artifact file_<32-hex>

# Read it back (offline, deterministic; renders all validated event types)
iroh-rooms room tail <ROOM_ID> --offline --json
```

**The bulk of the machinery is already landed and conformance-tested** (see §2.1): the
`agent.status` content type, its strict CBOR parse/emit, the stateless §6 pipeline, the
membership fold's ancestor-based authorization gate for non-membership writes, the sync
engine's receive path, and the offline/`--json` tail projection all already handle
`agent.status`. This issue is therefore **mostly integration**, with four small additive
pieces:

1. **Tighten content validation** in `parse_agent_status` (today only `progress_pct ≤ 100` is
   enforced) so the untrusted `status` / `message` / `related_artifact_ids` fields carry the
   same trust-boundary bounds the sibling types (`message.text`, `file.shared`) already have.
2. A **pure `build_agent_status` builder** in `iroh-rooms-core::event` — the byte-exact place an
   `agent.status` is assembled and signed (sibling of `build_message_text` / `build_file_shared`).
3. The **CLI command** `agent status <ROOM_ID> <STATUS>` and its send orchestration, reusing the
   landed `room send` offline-first/online-best-effort machinery verbatim.
4. **Extend the tail display** so an `agent.status` row surfaces `progress` and the referenced
   artifact ids (today it renders only `state` + `message`).

> **Why `risk/medium`.** No new crypto, no new authorization primitive, no new transport. The
> risk is (a) changing the strict parser's accept/reject surface for a type that already has
> committed golden/conformance vectors — a byte-format regression there is breaking; and
> (b) a second online CLI command re-driving the async `Node`. Both are contained by keeping
> the guaranteed core (author + local persist) fully offline and delegating every online detail
> to the landed `room send` path.

---

## 2. Background & current repository state

### 2.1 What already exists (landed — this builds on it)

`agent.status` was built into the **read / validate / fold / display** planes as part of the
full MVP event registry, ahead of a user-facing way to *author* one. All of the following is
committed on this branch:

- **Content type + strict parse/emit (`crates/iroh-rooms-core/src/event/content.rs`).**
  - `content::AgentStatus { status: String, message: Option<String>, related_artifact_ids: Option<Vec<[u8; SHORT_ID_LEN]>>, progress_pct: Option<u64> }` and `Content::AgentStatus(..)` (`content.rs:263`, `:298`).
  - `EventType::AgentStatus` registry string `"agent.status"` (`content.rs:59`, `:77`, `:119`).
  - `parse_agent_status` (`content.rs:789`) parses the four fields and **today enforces only** `progress_pct ≤ 100`. `status` is any non-empty-or-empty `tstr` with **no** length / control / emptiness bound; `message` has **no** length bound; `related_artifact_ids` uses `opt_short_id_array` (each element must be exactly `bstr[16]`, `content.rs:951`) but has **no count cap and no empty-array rejection**.
  - `to_cbor` for `AgentStatus` (`content.rs:440`) emits `status`, then optional `message`, `related_artifact_ids` (as an array of `bstr[16]`), `progress_pct` — following the §7 **omit-when-empty** discipline for the optionals (a `None` field is absent; the encoder re-sorts keys canonically).
  - `EventType::requires_membership_device_binding()` includes `AgentStatus` (`content.rs:93`): it carries **no** embedded `device_binding`; the signing device is resolved from the membership fold. `check_field_rules` has a no-op arm for `AgentStatus` (`content.rs:557`) — there is no `sender_id`-vs-content cross-field rule.
- **Authorization gate (`crates/iroh-rooms-core/src/membership/fold.rs`).**
  - `Content::AgentStatus(_)` is gated by `gate_active_member` (`fold.rs:347`): the author must be `Active` in the event's **ancestor view** (computed from `prev_events`), exactly like `message.text` / `file.shared` / `pipe.*`. A non-member (or a since-removed member, evaluated against ancestors) is rejected `NotAMember`. This is **not** role-restricted — any active member may post; the "agent" framing is a CLI noun, not an authorization tier.
  - `gate_device_binding` (`fold.rs:389`) additionally requires the signer's `device_id` to equal the membership-bound device for that identity (`UnboundDevice` otherwise).
- **Stateless pipeline (`event/validate.rs`).** `validate_wire_bytes` / `validate_with_membership` already accept `agent.status` bytes (signature-under-`device_id`, canonical re-encode, id match, `not_genesis_descended` when `prev_events` is empty, `room_id` binding).
- **Sync engine (`crates/iroh-rooms-core/src/sync/engine.rs:1666`).** `AgentStatus` is a first-class type on the receive/store/`room_tail` passthrough — validated, fold-gated, deduped, persisted, and returned by `room_tail` like every other type.
- **Tail display (`crates/iroh-rooms-cli/src/room.rs`).** The offline `room tail` (text + `--json`) renders **all** validated event types via `content_summary` (`room.rs:397`) and `content_fields` (`room.rs:473`). The `AgentStatus` arms exist today but are **partial**: `content_summary` emits `state=<status>[ text=<message>]` (`room.rs:460`); `content_fields` emits `state` + optional `message` (`room.rs:529`). **Neither renders `progress_pct` nor `related_artifact_ids`.** Unit tests exist for the current partial shape (`room.rs` tests `content_fields_agent_status_*`, `content_summary_agent_status_*`).
- **Core tests already present.** `tests/golden_vectors.rs` (`agent_status_progress_pct_over_100_is_rejected`, `_at_zero_is_accepted`, `_at_100_is_accepted`, registry-string vector), `tests/conformance/serialization.rs` (`invalid_content_agent_status_pct_over_100`), `tests/e2e_lifecycle.rs` (`agent_status_update_sequence`).
- **CLI `agent` noun (`crates/iroh-rooms-cli/src/agent.rs`, `cli.rs`).** Only `agent invite` exists — a façade over `invite::invite` (`agent.rs:31`). `AgentAction` (`cli.rs:67`) has a single `Invite` variant; `dispatch_agent` (`cli.rs:422`) handles only it.
- **The `room send` online path (`crates/iroh-rooms-cli/src/message.rs`).** `send` (`message.rs:97`) is the offline-first / online-best-effort template: validate args pre-IO → load secrets → `fold_room` + `is_active` check → `select_heads` → build + `validate_wire_bytes` self-check → if peers online, `run_push` (engine `publish` persists + fans out) else local `insert`; **always** guarantees local persistence. All of its helpers are reusable `pub(crate)`: `fold_room` (`:716`), `select_heads` (`:751`), `build_admission` (`:655`), `build_dial_set` (`:676`), `run_push` (`:551`), `net_mode` (`:702`), `endpoint_id_of` (`:696`), `parse_peers` (`:946`), `parse_timeout` (`:921`), `SendSummary` (`:72`). The exit-code taxonomy is in `error.rs`.
- **File-id handle codec (`crates/iroh-rooms-cli/src/file.rs`).** `file share` / `file list` print a file id as `file_<32-hex>` (`file_handle`), and `parse_file_id` (`file.rs:677`) tolerantly parses `file_<32-hex>` **or** bare 32-hex into the same `[u8; SHORT_ID_LEN]` a `file.shared.file_id` uses. This is the exact value an `agent.status.related_artifact_ids` element must carry (AC4).

### 2.2 What is missing (this issue)

1. Tightened `parse_agent_status` bounds + three new `constants.rs` entries.
2. `crates/iroh-rooms-core/src/event/status.rs` — `build_agent_status(..)` + `mod.rs` wiring/re-export.
3. `agent status` CLI command: `AgentAction::Status` (clap), `dispatch_agent` arm, `agent::status(..)` validator/wrapper, and `message::send_agent_status(..)` orchestration + `StatusSummary` + `print_status`.
4. `room.rs` `content_summary` / `content_fields` extension for `progress_pct` + `related_artifact_ids`.
5. Tests across all six layers (§11).

### 2.3 Authoritative schema (Spike §7, verbatim intent)

```
content = {
  "status":               tstr,          // "running" | "running_tests" | "blocked" | "error" | "done" | ...
  "message":          opt tstr,
  "related_artifact_ids": opt [ bstr[16], ... ],  // file_ids
  "progress_pct":     opt uint           // 0..=100 (integer; no floats)
}
```

- `status` is **free-form** text — the listed labels are examples (`| ...`), **not** a closed
  enum. Do not gate it to a fixed set.
- `progress_pct` is an **integer** percent 0..=100 (Spike §3 rule 5 forbids floats).
- `related_artifact_ids` are `file_id`s (16-byte short ids), advisory pointers (§4 D5).
- Signer/`prev_events` row (Spike §7 registry): "Any current member (typically `role == agent`)"; `prev_events` = room heads. No embedded `device_binding`.
- Spike §7 gives `agent.status` **no explicit "Validate:" line** — the extra bounds in §4 D1 are a deliberate trust-boundary hardening consistent with the module's stated "length/enum bounds are enforced" contract, not a spec-mandated rule.

---

## 3. Scope & non-goals

**In scope**
- `iroh-rooms agent status <ROOM_ID> <STATUS> [--message <TEXT>] [--progress <0..100>] [--artifact <FILE_ID> ...] [--peer ...] [--timeout <dur>] [--loopback]`.
- Content validation of the `agent.status` fields (§4 D1).
- Signed authoring, local persistence (the guarantee), best-effort online delivery.
- Rendering `agent.status` fully in the **offline** `room tail` (text + `--json`), including
  progress and artifact ids.

**Non-goals**
- **Live** (`room tail` streaming) rendering of `agent.status`. The live loop
  (`print_new_messages`, `message.rs:770`) intentionally renders only `message.text` today;
  extending it to `agent.status` is a nice-to-have display gap, deferred (§13 R4). The offline
  read is the authoritative display surface for this issue.
- Any agent runtime / orchestration / sandboxing — Spike explicitly: "`agent.status` is an
  ordinary event type; no orchestration/sandboxing."
- Role-gating status posting to `role == agent` (the gate is `gate_active_member`; any active
  member may post — matches Spike §7 "any current member").
- Guaranteed / queued offline delivery (PRD §14 availability model — best-effort only).
- Referential-integrity enforcement that a referenced `file_id` actually exists (§4 D5).
- Post-MVP `task.*` / `agent.output` event types (PRD §9.1 explicitly post-MVP).

---

## 4. Design decisions

**D1 — Tighten `parse_agent_status` to bound the untrusted fields.** Add, in
`parse_agent_status` (`content.rs:789`), mirroring `parse_file_shared` / `parse_message_text`:
- `status`: reject if empty, `len() > MAX_STATUS_LABEL_BYTES`, or contains any control char
  (`char::is_control`) — it is a short label rendered directly into the tail.
- `message`: reject if `len() > MAX_STATUS_MESSAGE_BYTES`. Newlines/Unicode are allowed (a human
  sentence), matching `message.text` body policy (control chars **not** rejected in `message`).
- `related_artifact_ids`: reject if the array is **empty** (must be omitted instead — §7
  omit-when-empty, exactly as `file.shared.providers` does at `content.rs:720`) or
  `len() > MAX_ARTIFACT_REFS`. Element length (`bstr[16]`) is already enforced by
  `opt_short_id_array`.
- `progress_pct ≤ 100` stays unchanged.
All violations return `RejectReason::InvalidContent`. This is the whole of "Validate status
event content" (issue Scope) at the trust boundary; the strictly AC-mandated checks are
`progress` (AC3, already present) and non-member rejection (AC2, in the fold).

**D2 — New constants (`constants.rs`), recommended values.**
- `MAX_STATUS_LABEL_BYTES: usize = 64` — short label; must accept the existing fixtures'
  `"running_tests"` (13 B). Tunable (§15 OQ1).
- `MAX_STATUS_MESSAGE_BYTES: usize = 4096` — a status note, not a chat transcript. (Reusing
  `MAX_MESSAGE_BODY_BYTES = 16384` is an acceptable alternative; §15 OQ1.)
- `MAX_ARTIFACT_REFS: usize = 16` — mirrors `MAX_FILE_PROVIDERS`.

**D3 — Pure builder, no validation inside it.** `build_agent_status` mirrors
`build_message_text` exactly: it is clock-/RNG-free (caller injects `prev_events` + `created_at`),
signs with the **device** secret (signature verifies under `device_id`, never `sender_id`),
maps an empty `message`/artifacts slice to `None` (omit-when-empty), and does **not** enforce
the D1 caps — the strict parser does, and the CLI pre-validates for friendly errors (the same
division of labor as `message.text`). A golden `event_id` regression lock pins the byte format.

**D4 — CLI split: thin `agent::status` validator over `message::send_agent_status` orchestration.**
Mirror `agent::invite` → `invite::invite`. `agent::status` owns **pre-IO argument validation**
(status/message caps, `progress ≤ 100` → `InvalidArgument`, artifact-handle parsing/dedup) and
then delegates the fold → heads → build → self-validate → persist → best-effort-push flow to a
new `message::send_agent_status`, which is `message::send` with the `agent.status` builder
substituted. This keeps a single online push implementation and puts the `progress > 100`
friendly check in exactly one place (`agent::status`).

**D5 — `related_artifact_ids` are advisory; no existence check.** Validation does **not** verify
a referenced `file_id` resolves to a known `file.shared` (that would need cross-event state the
Spike does not require). AC4 "*can* reference shared files *when present*" is satisfied by
accepting well-formed 16-byte ids and, on the display side, surfacing them so a reader can
correlate them with `file list`. Unknown/bogus ids persist verbatim as raw 16 bytes.

**D6 — Artifact input format = the file handle codec.** `--artifact` accepts `file_<32-hex>` or
bare 32-hex (reuse or exactly mirror `file::parse_file_id`, `file.rs:677`) — the string a user
copies from `file share` / `file list`. `agent::status` de-duplicates repeated `--artifact`
values (order-preserving) before building.

**D7 — Offline-first, online-best-effort (unchanged availability model).** `agent status`
**always** persists locally (AC1 "signed and persisted" is the guaranteed core); online
delivery to connected peers is best-effort and reported, exit 0 on partial/no delivery — byte
for byte the `room send` contract (PRD §14).

**D8 — Empty-array vs PRD example.** PRD §11.4 shows `"related_artifact_ids": []`. That JSON is
illustrative, not canonical CBOR. Under the §7 omit-when-empty rule the canonical encoding
**omits** an empty array; D1 therefore rejects a wire `[]`, and the CLI maps "no `--artifact`
flags" to omission. (Noted as an intentional PRD-vs-wire reconciliation, §15 OQ2.)

---

## 5. Data / API model

### 5.1 New public function (`iroh-rooms-core::event::status`)

```rust
/// Assemble and sign a member-authored `agent.status` event (Event Protocol §7).
/// Pure & deterministic; signs under the device secret. Empty `message`/`artifacts`
/// are omitted (None). Does NOT enforce the D1 caps (the strict parser does).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_agent_status(
    sender_identity_secret: &SigningKey,
    sender_device_secret: &SigningKey,
    room_id: &RoomId,
    status: &str,
    message: Option<&str>,
    related_artifact_ids: &[[u8; SHORT_ID_LEN]],
    progress_pct: Option<u64>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent
```
Re-export as `pub use status::build_agent_status;` in `event/mod.rs` (and add `pub mod status;`).

### 5.2 New CLI orchestration (`iroh-rooms-cli::message`)

```rust
pub struct StatusSummary {           // parallel to SendSummary
    pub event_id: EventId,
    pub room_id: RoomId,
    pub sender_id: IdentityKey,
    pub delivered: usize,
    pub attempted: usize,
}

/// Build + self-validate + persist (guaranteed) + best-effort push an agent.status.
/// Errors: NotAMember (exit 3) if not active; RoomNotFound; Internal on self-validate
/// failure. Peer-unreachable is NOT an error.
#[allow(clippy::too_many_arguments)]
pub async fn send_agent_status(
    home: &Path, room_id: &RoomId,
    status: &str, message: Option<&str>,
    progress_pct: Option<u64>, related_artifact_ids: &[[u8; SHORT_ID_LEN]],
    peers: &[String], timeout: Duration, loopback: bool,
) -> Result<StatusSummary>

pub fn print_status(summary: &StatusSummary) // labeled, script-friendly lines
```

### 5.3 New CLI validator/wrapper (`iroh-rooms-cli::agent`)

```rust
#[allow(clippy::too_many_arguments)]
pub async fn status(
    home: &Path, room_id: &RoomId,
    status: &str, message: Option<&str>, progress: Option<u64>, artifacts: &[String],
    peers: &[String], timeout: Duration, loopback: bool,
) -> Result<StatusSummary>
// pure helpers (unit-tested without IO):
fn validate_status(s: &str) -> Result<()>          // non-empty, <=cap, no control
fn validate_message(m: Option<&str>) -> Result<()> // <=cap
fn parse_artifacts(raw: &[String]) -> Result<Vec<[u8; SHORT_ID_LEN]>> // handle codec + dedup
// inline in `status`: `if progress > 100 => bail_coded!(InvalidArgument, ..)`
```

### 5.4 CLI surface (`cli.rs`)

```
AgentAction::Status {
    room_id: String,          // positional (blake3:<hex>)
    status: String,           // positional
    #[arg(long)] message: Option<String>,
    #[arg(long)] progress: Option<u64>,          // clap rejects non-int / negative → exit 2
    #[arg(long = "artifact")] artifacts: Vec<String>,   // repeatable
    #[arg(long = "peer")] peers: Vec<String>,
    #[arg(long, default_value = message::DEFAULT_SEND_TIMEOUT)] timeout: String,
    #[arg(long, hide = true)] loopback: bool,
}
```
`dispatch_agent` adds a `Status { .. }` arm: `parse_room_id` → `parse_timeout` →
`runtime()?.block_on(agent::status(..))` → `message::print_status(&summary)`, mirroring the
`RoomAction::Send` dispatch (`cli.rs:545`).

---

## 6. Implementation steps

1. **Constants** — add `MAX_STATUS_LABEL_BYTES`, `MAX_STATUS_MESSAGE_BYTES`, `MAX_ARTIFACT_REFS`
   to `crates/iroh-rooms-core/src/event/constants.rs` with doc comments citing §7 / this spec.
2. **Tighten the parser** — in `parse_agent_status` (`content.rs:789`) add the D1 checks
   (`status` empty/cap/control, `message` cap, artifacts empty/count). Import the new constants.
   Keep the existing `progress_pct ≤ 100` check. Do not touch `to_cbor` — the emit format is
   already correct and byte-stable.
3. **Builder** — add `crates/iroh-rooms-core/src/event/status.rs` with `build_agent_status`
   (copy `message.rs` structure: assemble `Content::AgentStatus`, `SignedEvent`, `to_csb`,
   `sign_csb`, `WireEvent::seal`; empty `message`/`artifacts` → `None`). Add `pub mod status;`
   + `pub use status::build_agent_status;` to `event/mod.rs` (and mention it in the module doc
   list alongside `file`/`message`).
4. **CLI orchestration** — add `StatusSummary`, `send_agent_status`, `print_status` to
   `crates/iroh-rooms-cli/src/message.rs`. Implement `send_agent_status` by cloning `send`'s
   body and swapping `build_message_text(..)` for `build_agent_status(..)`; reuse `fold_room`,
   `is_active` guard (→ `NotAMember`), `select_heads`, `validate_wire_bytes` self-check,
   `build_dial_set` / `build_admission` / `run_push` / local-`insert` guarantee unchanged. (If
   the duplication is objectionable, factor the shared tail — everything after "build a
   `WireEvent`" — into a private `push_or_persist(wire, ..) -> (delivered, attempted)` helper
   and have both `send` and `send_agent_status` call it. Optional; do not let the refactor
   change `room send` behavior or its tests.)
5. **CLI validator/wrapper** — add `status`, `validate_status`, `validate_message`,
   `parse_artifacts` to `crates/iroh-rooms-cli/src/agent.rs`. `status` validates
   (status/message/progress/artifacts) **before any IO**, then calls
   `message::send_agent_status`. Progress `> 100` → `bail_coded!(InvalidArgument, ..)`.
6. **CLI wiring** — add `AgentAction::Status { .. }` (clap) in `cli.rs`, extend `dispatch_agent`
   with a `Status` arm (§5.4). Keep the `agent invite` arm untouched.
7. **Display extension** — in `crates/iroh-rooms-cli/src/room.rs`:
   - `content_summary` `AgentStatus` arm (`room.rs:460`): append ` progress=<n>%` when
     `progress_pct` is `Some`, and ` artifacts=<count>` when `related_artifact_ids` is non-empty
     (keep the existing `state=` / `text=` prefix stable — the tail tests key off the stable
     `key=value` prefix, not this free-form tail).
   - `content_fields` `AgentStatus` arm (`room.rs:529`): additionally insert `progress` (uint)
     when set and `artifacts` (JSON array of lowercase-hex or `file_<hex>` handles) when
     non-empty. Keep `state` + `message` keys unchanged (existing tests assert them).
8. **Docs** — update `docs/getting-started.md` and the `agent` section of any CLI reference so
   `agent status` is documented next to `agent invite`; ensure `tests/docs_conformance.rs`
   (which checks documented commands exist) still passes.
9. **Tests** — §11.

---

## 7. Validation & authorization rules (summary)

| Rule | Where | Reject/behaviour |
|---|---|---|
| `status` present, non-empty, ≤ cap, no control chars | `parse_agent_status` (D1) | `InvalidContent` |
| `message` ≤ cap | `parse_agent_status` (D1) | `InvalidContent` |
| `related_artifact_ids`: non-empty-if-present, ≤ cap, each `bstr[16]` | `parse_agent_status` (D1) | `InvalidContent` |
| `progress_pct` integer 0..=100 | `parse_agent_status` (already present) | `InvalidContent` (>100); non-int caught by clap at CLI (exit 2) |
| Unknown content key | `Fields::finish` (already) | `InvalidContent` |
| Signed by device key; id/canonicality/room-binding | stateless §6 (already) | `BadSignature` / `IdMismatch` / `NonCanonicalEncoding` / `RoomIdMismatch` |
| `prev_events` non-empty (non-genesis) | stateless §6 (already) | `NotGenesisDescended` |
| **Author is an Active member (ancestor view)** | `gate_active_member` (already) | **`NotAMember`** (AC2) |
| Signer device == membership-bound device | `gate_device_binding` (already) | `UnboundDevice` |
| CLI: caller is Active before authoring | `send_agent_status` `is_active` guard | `NotAMember` (exit 3) |

Note the **ancestor-view** subtlety (see the "member message ancestor-view gate"): a
non-admin's `agent.status` must cite a `prev` whose ancestor membership snapshot already has the
author Active. The CLI's `select_heads` cites current room heads (post-join fold tips), so a
normally-joined agent satisfies this; hand-authored test fixtures citing `genesis` from a
non-admin identity are silently rejected `NotAMember`.

---

## 8. Error model & exit codes

Reuse the landed taxonomy (`crates/iroh-rooms-cli/src/error.rs`; category → exit in
`ErrorCategory::exit_code`):

| Condition | `ErrorCode` | Category | Exit |
|---|---|---|---|
| Success (persisted; delivered 0..n) | — | — | `0` |
| Empty/over-cap/control `status`; over-cap `--message`; bad `--artifact` handle; too many artifacts; `--progress > 100` | `InvalidArgument` | Usage | `2` |
| Non-integer / negative `--progress`, missing positional | (clap usage error) | — | `2` |
| Unknown room | `RoomNotFound` | Usage | `2` |
| Caller not an active member | `Reject(NotAMember)` | Auth | `3` |
| Freshly built event fails self-validation (bug guard) | `Internal` | Internal | `1` |

`print_status` prints labeled lines (mirror `print_send`, `message.rs:222`):
```
status: <event_id>
room:   <room_id>
from:   <sender_id>
stored: yes
delivered: <0 (no other members…) | 0 (no peers online…) | N connected peer(s)>
```

---

## 9. Observability

- Receive-path rejects (a peer's malformed/non-member `agent.status`) surface through the
  engine's bounded `logs()` as `reject.<code>` and are rendered by the running `room tail` as
  `warning[<code>]: …` (`print_new_reject_warnings`, `message.rs:816`) — no new plumbing.
- Blob/audit lines ride the existing `StderrAudit` sink; the CLI installs **no** tracing
  subscriber, so any new operator signal must be an explicit `eprintln!`/audit line, not a
  `tracing` call ("CLI has no tracing subscriber").

---

## 10. Security / privacy / reliability

- **No new trust surface.** Authorization is the same `gate_active_member` + device-binding gate
  as `message.text`; a non-member's status is dropped, never persisted or re-broadcast (AC2).
- **DoS / pollution containment.** D1's `status`/`message`/artifact caps stop an unbounded or
  control-char-laden field from a malicious peer polluting the tail or bloating the log — the
  reason the sibling types are bounded.
- **Signature integrity.** Signed under the device key; tampering any signed byte breaks both id
  and signature (covered by the stateless pipeline + builder golden test).
- **Availability honesty.** Local persistence is guaranteed; delivery is best-effort and
  reported truthfully (PRD §16 UX requirement: "availability limitations explicit, not hidden").
- **Privacy.** `related_artifact_ids` leak only 16-byte file handles already shared in-room; no
  new data egress.

---

## 11. Test plan

Layered so each check lives once (extends, does not duplicate, the §2.1 coverage). Follows the
existing per-layer split for `agent.status`.

**L1 — Core builder (`event/status.rs` `#[cfg(test)]`).** Determinism (same inputs → identical
bytes); full-field round-trip (status + message + 2 artifacts + progress); omit-when-empty
(empty message/artifacts → `None`); signature verifies under `device_id`, **not** `sender_id`;
built event passes `validate_wire_bytes`; boundary accepts (status at cap, progress 0 and 100,
artifacts at `MAX_ARTIFACT_REFS`); `NotGenesisDescended` on empty `prev_events`; `RoomIdMismatch`
in a foreign room context; `NotAMember` via a denying `MembershipOracle`; a **golden `event_id`
lock** on a pinned bare-status fixture (seeds `[0x01]`/`[0x02]`, nonce `00..0f`,
`created_at 1_750_000_000_000`, one synthetic head, `status "running_tests"`) — recompute only
on an intentional (breaking) byte-format change.

**L2 — Protocol parser bounds.** In `tests/conformance/serialization.rs`: reject empty /
over-cap / control-char `status`; over-cap `message`; **empty** / over-cap / wrong-element-length
`related_artifact_ids`; unknown content key; **plus boundary accepts** (status at cap, artifacts
at cap, message at cap). The existing `invalid_content_agent_status_pct_over_100` and the
`golden_vectors.rs` progress vectors stay green (confirm the new bounds accept the fixtures they
use).

**L3 — CLI validators (`agent.rs` `#[cfg(test)]`, pure, no IO).** `validate_status` (empty /
over-cap / control rejected; normal accepted), `validate_message` (over-cap rejected;
newlines/Unicode accepted), `parse_artifacts` (`file_<hex>` and bare-hex both parse to the same
16 bytes; dedup; bad handle rejected; > `MAX_ARTIFACT_REFS` rejected).

**L4 — CLI orchestration (`agent.rs` `#[tokio::test]`, offline, temp home, no peers → local
insert).** Drives `agent::status` → `message::send_agent_status` directly:
  - **AC1** — valid status persists a **signed** `agent.status` (reopen the store; assert one
    `AgentStatus` row signed under the device key ≠ identity key) and returns a `StatusSummary`
    with `delivered = attempted = 0`.
  - progress + artifacts round-trip (2 bogus artifact handles persist as raw 16 bytes — advisory,
    D5).
  - **AC3** — `--progress 101` (or any `> 100`) → `code_of() == InvalidArgument`, **nothing**
    written to the store.
  - **AC2** — copy a room log into a **non-member** home → `code_of() == Reject(NotAMember)`,
    nothing written.
These pin exact `ErrorCode`s the assert_cmd stderr substrings can't.

**L5 — CLI integration (`tests/agent_cli.rs`, assert_cmd, offline).** The room creator is an
active admin, so no invite/join dance is needed to post. Assert: valid status → exit 0 +
`stored: yes`; `--progress 101` → exit 2; non-member home → exit 3; **display in `room tail
--offline`** (text + `--json`) shows the row with `state`, `message`, `progress`, and the
artifact ids (AC4 present-case) — the Test-Plan's "display in room tail".

**L6 — Display (`room.rs` `#[cfg(test)]`).** Extend `content_fields_agent_status_*` /
`content_summary_agent_status_*`: `progress` present when set / absent when `None`; `artifacts`
array present when non-empty / absent when empty; existing `state`/`message` assertions
unchanged.

**L7 — Online e2e (deferred tier, `#[ignore]`, `tests/agent_e2e.rs`).** Two live loopback QUIC
processes: admin `room tail --accept-joins --loopback`, agent joins via an `agent invite`
ticket, then `agent status --peer --loopback` pushes to the still-online admin; assert
`delivered: 1 connected peer(s)` **and** that the admin's own offline `room tail --json`
durably persisted the `agent.status` row (state/progress/message/from). Does **not** assert live
`room tail` stdout rendering (that live gap is out of scope — §13 R4). Run:
`cargo test -p iroh-rooms-cli --test agent_e2e -- --ignored --test-threads=1`.

**Gate:** `./verify.sh` (fmt `--check` + clippy `-D warnings` pedantic + full test suite) is the
real CI gate — `cargo test` passing alone is not green.

---

## 12. Acceptance criteria → evidence

| Issue AC | Satisfied by | Test |
|---|---|---|
| Agent status event is signed and persisted | `build_agent_status` (device-signed) + `send_agent_status` local-persist guarantee | L1 (signature), L4 AC1, L5 |
| Non-member agent status is rejected | `gate_active_member` (receive) + `is_active` guard (author) | L1 (oracle), L4 AC2, L5 (exit 3) |
| Optional progress percent validates as integer 0..100 | `progress_pct ≤ 100` in parser + `> 100` check in `agent::status` + clap int parse | L2, L4 AC3, L5 (exit 2) |
| Related artifact IDs can reference shared files when present | `related_artifact_ids` accepted as 16-byte `file_id`s (D5/D6) + rendered in tail | L4 (round-trip), L5/L6 (display) |

---

## 13. Risks

- **R1 — Byte-format regression in the strict parser.** Tightening `parse_agent_status` changes
  its accept/reject surface for a type with committed golden/conformance vectors. *Mitigation:*
  choose caps that accept every existing fixture; run `golden_vectors.rs` +
  `conformance/serialization.rs` before/after; the builder golden `event_id` locks the emit
  bytes (which do **not** change).
- **R2 — `send` duplication drift.** `send_agent_status` mirrors `send`; the two can diverge.
  *Mitigation:* prefer the shared `push_or_persist` helper (step 4) so the online path has one
  implementation; if copied, keep them adjacent and cross-referenced.
- **R3 — Cap-value bikeshedding.** The D2 numbers are judgment calls (Spike gives no
  "Validate:" line). *Mitigation:* documented as OQ1; conservative, sibling-consistent defaults;
  trivially tunable in one place.
- **R4 — Live-tail display gap.** `agent.status` won't render in the streaming `room tail`
  (only offline). *Mitigation:* explicit non-goal (§3); offline read is authoritative; the L7
  e2e asserts durable persistence, not live stdout. Fast follow-up if needed.
- **R5 — Ancestor-view fixture trap.** Hand-authored non-admin `agent.status` citing `genesis`
  is silently dropped `NotAMember`. *Mitigation:* test fixtures cite fold tips / real heads;
  documented in §7.

## 14. Rollout / rollback

Additive and feature-gated by a new subcommand: existing commands and event handling are
untouched (the parser change only *tightens* an already-strict type). Rollback = revert the
commit; no migration, no persisted-schema change (`SCHEMA_VERSION` unchanged — `agent.status`
was already in the registry). Any `agent.status` authored before this ships (there is no author
path, so none exist) would be unaffected.

---

## 15. Open questions

- **OQ1 — Cap values.** Confirm `MAX_STATUS_LABEL_BYTES = 64`, `MAX_STATUS_MESSAGE_BYTES = 4096`,
  `MAX_ARTIFACT_REFS = 16`. Alternative: reuse `MAX_MESSAGE_BODY_BYTES (16384)` for the message.
  (Chosen defaults are recommended; flagged only because the Spike gives no explicit bound.)
- **OQ2 — Empty artifact array.** Confirm rejecting a wire `related_artifact_ids: []` (D8) rather
  than accepting it as the PRD §11.4 example's literal JSON suggests. Recommendation: reject
  (canonical omit-when-empty, consistent with `file.shared.providers`).
- **OQ3 — `--artifact` display form.** Render artifact ids in `--json` as bare 32-hex or as
  `file_<hex>` handles? Recommendation: `file_<hex>` handles, so they round-trip straight back
  into `file fetch` / another `agent status --artifact`.
- **OQ4 — Should `agent status` warn when `--artifact` references a `file_id` not present in the
  local log?** Recommendation: a non-fatal `note:` on stderr (advisory, exit 0), never a
  rejection (D5).

## 16. Assumptions

- The heavy lifting (content type, strict parse/emit, fold gate, sync passthrough, offline tail
  projection, exit-code taxonomy, file-handle codec, online push machinery) is **landed** exactly
  as §2.1 describes; this issue is integration + a bounded parser tightening + display polish.
- `agent status` posting is open to **any active member** (not role-gated to `agent`), matching
  Spike §7 and `gate_active_member`.
- The offline `room tail` (`--offline`, text + `--json`) is the display surface of record for
  this issue; live streaming rendering of `agent.status` is deferred.
- No GitHub/network actions are performed by this planning phase (orchestrator owns git/gh).
