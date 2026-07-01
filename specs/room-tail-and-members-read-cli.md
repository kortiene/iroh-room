# Spec: Room Tail and Members Read CLI (IR-0106)

| | |
|---|---|
| **Issue** | #21 — `[IR-0106] Implement room tail and members CLI` |
| **Parent epic** | #2 |
| **Labels** | `type/feature` `area/cli` `priority/p1` `risk/low` |
| **Dependencies** | #20 (IR-0105, signed message send/receive — **landed**), #17 (IR-0102, room create + `room members` — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.4 (send/history), §16 (CLI requirements — `room tail`/`room members`, script-friendly output, honest availability), §17.2 (developer-experience metrics); Spike `PHASE-0-SPIKE.md` Membership & Ordering §2 (deterministic `(lamport, event_id)` order, §2.3 advisory `created_at`, §2.4 position carries no trust), §3 (status/role lattices) |
| **Owning crate** | `crates/iroh-rooms-cli` (read-only orchestration over landed primitives). Optional small additive pure builders in `crates/iroh-rooms-core::event` **only** to make the removed/left test path deterministic (D8; see OQ-3). No production change to core store/fold/sync is required — the read plumbing already exists. |

> **Status:** planning. This document is the build plan for another engineer/agent to execute.
> **Do not implement from this doc in the same change that writes it.** The compiled binary is
> the source of truth once landed.

---

## 1. Summary

Expose **basic, offline, deterministic room read commands** for developer workflow and testing:

```bash
# Deterministic, script-friendly, network-free timeline read of the local log:
iroh-rooms room tail <ROOM_ID> --offline [--json] [--limit <N>]

# Admin / member / agent roster with clear removed-vs-left representation:
iroh-rooms room members <ROOM_ID> [--json]
```

Both commands must produce **stable, script-friendly output** parseable by tests without brittle
formatting, with **clear attribution for sender identity and agent/member role**, and must
represent **removed / left members** unambiguously.

### 1.1 Why this issue is mostly integration + a small read surface

The protocol- and ordering-critical machinery already exists and is conformance-tested:

- the SQLite store's canonical **`room_tail(room, limit)`** returns the most-recent causally-placed
  events **ascending by `(lamport, event_id)`** — exactly the §2 deterministic timeline
  (`store/mod.rs:251`);
- the deterministic **membership fold** resolves each identity's `role` (admin/member/agent) and
  `status` (active/invited/removed) (`membership/*`), already consumed offline by the landed
  `room::members` (`room.rs:142`);
- the stateless **§6 validator** (`validate_wire_bytes`) re-checks every stored event
  (`validate.rs`); and
- `EventType` / `Content` decode every stored event's type and payload for display
  (`event/content.rs`).

So the read *data* is all landed. What is missing is a **CLI read surface** that is (a) offline and
deterministic (today's `room tail` is an online streaming receiver — see §2.2), (b) shows **all**
validated event types with attribution (today's tail shows only `message.text`), (c) offers a
robust structured (`--json`) output, and (d) distinguishes **left** from **removed** members.

> **Why `risk/low`.** No new crypto, no new validation rule, no new authoring path, no network. These
> are read-only projections of the already-validated local event log. The only genuinely new surface
> is presentation (text/JSON) and, optionally, two pure test-support builders in core (D8).

---

## 2. Background & current repository state

### 2.1 What exists (landed work this builds on)

- **`room members <ROOM_ID>` — landed offline read (#17 / IR-0102), extended (#22 / IR-0107).**
  - `room::members(home, room_id) -> MembersView` re-derives membership by re-validating and folding
    the persisted log; no `rooms`/`members` table (`room.rs:142`). `MembersView { room_id,
    admin_identity_id, members: Vec<MemberRow> }`; `MemberRow { identity_id, role, status, is_admin }`.
  - `room::print_members` prints labeled lines (`room.rs:190`):
    ```
    room: <room_id>
    admin: <identity_id | <none>>
    member: <identity_id> role=<admin|member|agent> status=<active|invited|removed>[ (admin)]
    ```
  - `role_str` / `status_str` map the fold enums to strings (`room.rs:222`, `:231`).
  - `room members <ROOM_ID> --status` (#22) additionally brings up an ephemeral node and appends a
    live `conn=` field per member (`message::members_status`, `message.rs:369`). **Online**; out of
    scope to change here beyond sharing the refined status helper (D5).
- **`room tail <ROOM_ID>` — landed ONLINE streaming receiver (#20 / IR-0105).**
  - `message::tail(home, room_id, peers, limit, accept_joins, loopback)` (`message.rs:243`): loads
    the device secret, **requires the caller to be an Active member**, brings up a `Node`, prints a
    `listening:` address, then **loops until Ctrl-C** rendering newly-arrived rows. It filters to
    **`message.text` only** (`print_new_messages`, `message.rs:755`) and also drives the §16.3
    connection panel. This is a live session — it never terminates on its own and needs the network,
    so it **cannot** back a deterministic snapshot test over a temp DB.
  - `--accept-joins` engages the join-bootstrap admission overlay (#19); `--peer`/`--limit`/`--loopback`
    are the online session's knobs. **These must keep working unchanged** (getting-started and the
    join handshake depend on them).
- **Store read surface (`store`, #8).** `room_tail(room, limit)` (canonical order, excludes
  `NULL`-lamport / causally-incomplete events, `store/mod.rs:251`); `by_type(room, ty)`
  (`:275`); `by_sender` (`:290`); `room_event_ids(room)` (`:177`); `get(id)` (`:147`);
  `count(room)` (`:157`). `StoredEvent { event_id, wire, room_id, event_type, lamport: Option<u64>,
  admin_seq: Option<u64> }` (`store/model.rs:54`).
- **Membership snapshot (`membership`, #12).** `MembershipSnapshot::{admin, member, members,
  active_members, role, status, is_active, identity_of_device}`; `Member { identity, device:
  Option<DeviceKey>, status, role }`; `Status { Invited < Active < Removed }` (a **max** lattice,
  `model.rs:19`); `Role { Agent < Member < Admin }` (`model.rs:35`).
- **Event decode (`event`, #6).** `SignedEvent::decode(&wire.signed)` → `sender_id`, `device_id`,
  `created_at`, `prev_events`, `content`. `Content::{RoomCreated, MemberInvited, MemberJoined,
  MemberLeft, MemberRemoved, MessageText, FileShared, PipeOpened, PipeClosed, AgentStatus}`
  (`content.rs:277`); `EventType::as_str()` yields the stable dotted names (`room.created`,
  `member.joined`, `message.text`, …, `content.rs:66`).
- **CLI conventions.** `serde_json` is already a CLI dependency and `identity show --json` prints a
  single-line JSON object (`identity.rs:298`). `extract_field` in tests parses `key: value` lines
  (`tests/room_cli.rs:56`). `iso8601_utc(ms)` renders advisory timestamps without `chrono`
  (`message.rs:954`). Integration tests use `assert_cmd` + per-test `--data-dir` temp homes with
  `IROH_ROOMS_HOME` cleared (`tests/room_cli.rs:30`).

### 2.2 The real gaps (this issue closes them)

1. **No offline, deterministic timeline read.** `room tail` is an online streaming session that
   requires membership + network and never exits — unusable for the issue's Test Plan ("CLI snapshot
   tests or structured-output tests using temp local database"). **D1/D2/D3.**
2. **`room tail` shows only messages, not "validated events".** AC1 says "shows *validated events* in
   deterministic order"; the current tail filters to `message.text`. The read must project **all**
   validated event types with attribution. **D2.**
3. **No structured (`--json`) output on either command.** AC4 wants output "parsed by tests without
   brittle formatting"; labeled lines are greppable but a JSON mode is the robust parse contract. **D6.**
4. **Left vs removed is indistinguishable.** The fold collapses `member.left` and `member.removed`
   into `Status::Removed` (sticky departure). AC3 requires representing removed/left "clearly when
   relevant". A **display-only** refinement derived from the log closes this without touching the
   security lattice. **D5.**
5. **No deterministic way to *produce* a removed/left member for tests.** There is no
   `build_member_left` / `build_member_removed` and no CLI command that authors them. AC3's test
   needs one. **D8 / OQ-3.**

### 2.3 Spike / PRD facts that constrain the design

- **Deterministic order (Membership §2).** The timeline is the validated, causally-complete set
  **ascending by `(lamport, event_id)`**; `lamport` is derived (`1 + max(parent lamports)`, genesis
  0); `event_id` ties break bytewise over the 32 raw digest bytes. `store::room_tail` already
  implements exactly this comparator. Causally-incomplete events (`NULL` lamport) are excluded until
  their parents arrive.
- **`created_at` is advisory/display-only (§2.3)** and **timeline position carries no trust (§2.4).**
  The read may *show* `created_at` for human context but MUST order by `(lamport, event_id)` only and
  MUST attach no "first/pinned" meaning to position.
- **Status & role lattices (§3).** `Removed` dominates `Active`/`Invited` (a since-removed member is
  authoritatively Removed even with concurrent Active events); role is the least-privilege merge.
  `member.left` (self-departure, `member_id == sender_id`) and `member.removed` (admin action,
  `member_id != sender_id`, `removed_by == sender_id`) **both** land the subject in `Removed`; the
  distinction between them is *not* in the folded snapshot (`content.rs:174`–`:196`).
- **Availability / honesty (PRD §16).** Output should be script-friendly; failure and availability
  states must be explicit, not hidden. A local read never invents peers or online state.
- **Local-first (PRD §16, §17.2).** Room state is derived from the append-only log; a read command
  is a pure projection of `<home>/rooms.db` and must survive restart by construction.

---

## 3. Design decisions

> Referenced as `Dn` throughout. Each is the smallest choice that satisfies the acceptance criteria
> while staying aligned with the landed architecture and **not breaking the online `room tail`**.

- **D1 — Add an `--offline` read mode to `room tail`; keep the online streaming session the default.**
  `iroh-rooms room tail <ROOM_ID> --offline` performs a **pure local-DB read**: open the store,
  render the validated timeline in canonical order, and **exit 0**. No network, no `Node`, no
  identity/secret load, no membership requirement (it is a local inspection tool, exactly like
  `room members`). Without `--offline`, behavior is unchanged (the #20 live receiver). `--offline`
  **conflicts with** the online-only flags `--peer`, `--accept-joins`, and `--loopback` (clap
  `conflicts_with_all`) so the two modes never mix. **Rationale:** the issue and PRD §16 name the
  command `room tail`; overloading it with an additive flag satisfies IR-0106 without a second noun
  and without regressing the documented live/join workflows. (Alternative spellings `--once` /
  `--no-follow`, and a distinct `room log` subcommand, are considered in OQ-1.)

- **D2 — Offline `room tail` renders ALL validated event types, not just messages.** AC1 says
  "validated events". Each stored, causally-complete event is projected with a stable attribution
  header plus a type-specific one-line summary. Event *type* is the stable dotted name
  (`EventType::as_str()`); this is what makes the timeline a real developer/testing read of the log,
  covering `room.created`, `member.invited/joined/left/removed`, `message.text`, `file.shared`,
  `pipe.opened/closed`, `agent.status`. **Rationale:** a "room tail" for developer workflow must show
  the log, not one event family; message-only tailing remains available via the online session.

- **D3 — Re-validate every stored event through §6 before display; take attribution from the fold.**
  Mirror `room::members` / `message::fold_room`: for each id in the room, `get` → `validate_wire_bytes`
  (fail loudly on a corrupt row), then fold the validated set into a `MembershipSnapshot`. Order the
  displayable rows by `store::room_tail(room, limit)`'s canonical `(lamport, event_id)`. Each row's
  `from`/`role`/`status` attribution is resolved from the **snapshot** (the current room view), so the
  reader sees who each sender *is now* (admin/member/agent, active/removed/left). **Rationale:** the
  word "validated" in AC1 is load-bearing; re-validation is the belt-and-suspenders pattern already
  used by every offline read in this crate, and small rooms make it free.

- **D4 — `room members` keeps its landed text output; add `--json`.** The existing labeled-line
  output already satisfies AC2 (admin/member/agent role + active/invited/removed status) and stays
  the default. Add `--json` for the robust structured contract (D6). **Rationale:** additive,
  non-breaking, matches `identity show --json`.

- **D5 — Distinguish left vs removed as a DISPLAY-ONLY refinement derived from the log.** Compute a
  **membership display state** per member: `active` / `invited` / `removed` (admin-removed) / `left`
  (voluntary). It is derived by taking the fold `Status`, and when `Status::Removed`, disambiguating
  via the log: a `member.removed` with `member_id == subject` ⇒ `removed`; else a `member.left` with
  `member_id == subject` ⇒ `left`; else `removed` (fallback). Admin-removal dominates a self-leave if
  both exist (the admin action is the authoritative statement). This is **presentational only** — the
  security lattice is unchanged; `left` and `removed` are the same zero-capability state. Expose it as
  the `status=` value in text and `"status"` in JSON, so departed members are represented clearly
  (AC3). Share one helper between offline `room members`, offline `room tail` attribution, and the
  online `--status` path (so the three never diverge). **Rationale:** AC3 explicitly names both; the
  fold cannot tell them apart, but the log can, and a read command is exactly where that projection
  belongs. (Alternative: keep `status=removed` and add a separate `departure=self|admin` field —
  OQ-2.)

- **D6 — Two output shapes: stable `key=value` text (default) and a JSON array (`--json`).**
  - **Text:** one line per row with a fixed, ordered `key=value` prefix that tests parse, followed by
    a free-form human summary tail that tests do **not** parse. This keeps output greppable and
    stable even as summaries evolve.
  - **JSON:** a single top-level JSON **array** (not JSONL) of objects, printed once — trivially
    consumed by `serde_json` / `jq`, and unambiguous for a finite offline snapshot. Field names are
    stable and lowercase-snake. **Rationale:** the JSON array is the "parse without brittle
    formatting" contract (AC4); the text prefix is the lightweight greppable path. (JSONL alternative:
    OQ-4.)

- **D7 — Offline reads require neither identity nor active membership.** They are pure inspections of
  the caller's own local `rooms.db` (the same trust posture as the landed offline `room members`,
  which loads no secret). This is what makes them usable in tests and in "developer workflow". The
  online `room tail` still requires membership (it dials peers and serves the room). **Rationale:**
  reading your own local log needs no key; requiring one would make the test/dev path pointlessly
  heavier and diverge from `room members`.

- **D8 — To make the removed/left path deterministically testable, add pure `build_member_left` and
  `build_member_removed` to `iroh-rooms-core::event`.** They mirror `build_member_invited` /
  `build_member_joined` exactly (clock-/RNG-free; caller injects `prev_events`, `created_at`, keys).
  This issue uses them **only in tests** to author a valid departure into a temp DB; they are the
  natural siblings the future `room leave` / `member remove` authoring issues will reuse. **This is
  the one place the read-only scope is stretched, and it is optional** — see OQ-3 for the "construct
  raw in the test helper instead" alternative. **Rationale:** without a producer, AC3 can only be
  tested by hand-assembling CBOR in the test, which is brittle; two tiny pure builders make the test
  chain (`create → invite → join → left|removed`) clean and deterministic.

---

## 4. Architecture & data flow

### 4.1 Offline `room tail --offline` (new; D1–D3, D5, D6)

```
open EventStore(<home>/rooms.db)                                   [offline]
 → ids = room_event_ids(room); if empty → actionable "no room" error
 → for id in ids: get → validate_wire_bytes(§6)  (corrupt row → loud error)   [D3]
 → snapshot = RoomMembership::from_events(room, validated).snapshot()          [attribution + D5]
 → rows = store.room_tail(room, limit)   (ascending (lamport, event_id))        [D2 order]
 → for se in rows:
       ev = SignedEvent::decode(se.wire.signed)
       from  = short_id(ev.sender_id)  (+ display_name if a local member.joined names it)
       role  = snapshot.role(ev.sender_id)      // admin|member|agent|unknown
       state = membership_display(snapshot, log, ev.sender_id)  // active|invited|removed|left  [D5]
       summary = type_specific(ev.content)      // body / file name / pipe id / status text / …
       emit text line OR push JSON object
 → if --json: print one JSON array; else: already streamed line-by-line
 → exit 0
```

Never opens a `Node`, never loads secrets, never requires membership (D7). `--limit` bounds the rows
(reusing the existing default, `DEFAULT_TAIL_LIMIT = 200`).

### 4.2 Offline `room members [--json]` (extend landed; D4–D6)

```
view = room::members(home, room_id)          // landed: fold the log (re-validate each event)
 → refine each row's status via membership_display(snapshot, log)   [D5]
 → if --json: print JSON array of member objects; else: landed labeled-line output (+ refined status)
```

`room::members` already returns the folded `MembersView`; D5 refines the `status` string and D6 adds
the JSON branch. The snapshot and the per-subject terminal departure event (from
`store.by_type(room, MemberLeft|MemberRemoved)`) are the only inputs.

### 4.3 Where each acceptance criterion is enforced

| Criterion | Enforced by | Status |
|---|---|---|
| AC1 `room tail` shows validated events in deterministic order | re-validate §6 (D3) + `store.room_tail` `(lamport, event_id)` order (D2) | landed data + new read |
| AC2 `room members` shows admin/member/agent status | fold `role`/`status` (landed) surfaced by `room::members` | landed (D4) |
| AC3 removed/left represented clearly | `membership_display` log-derived refinement (D5) | new (display-only) |
| AC4 parseable without brittle formatting | JSON array + stable `key=value` prefix (D6) | new |

---

## 5. Detailed implementation steps

### Step 1 — CLI arg surface (`cli.rs`)

Extend `RoomAction::Tail` and `RoomAction::Members`:

```rust
Tail {
    room_id: String,
    // NEW: offline deterministic read of the local log; exits 0.
    #[arg(long, conflicts_with_all = ["peers", "accept_joins", "loopback"])]
    offline: bool,
    // NEW: structured output (single JSON array). Valid in both modes? -> offline only (see below).
    #[arg(long)]
    json: bool,
    #[arg(long = "peer")] peers: Vec<String>,
    #[arg(long, default_value_t = crate::message::DEFAULT_TAIL_LIMIT)] limit: u32,
    #[arg(long = "accept-joins")] accept_joins: bool,
    #[arg(long, hide = true)] loopback: bool,
},
Members {
    room_id: String,
    // NEW:
    #[arg(long)] json: bool,
    #[arg(long)] status: bool,
    #[arg(long = "peer")] peers: Vec<String>,
    #[arg(long, default_value = crate::message::DEFAULT_SEND_TIMEOUT)] timeout: String,
    #[arg(long, hide = true)] loopback: bool,
},
```

- `--json` on `room tail` is meaningful only in `--offline` mode (the online session streams
  indefinitely; JSON-array framing does not apply). Enforce `--json` ⇒ requires `--offline` via clap
  `requires`, **or** accept `--json` in the online session as JSONL later (out of scope; note it).
  Recommended: `#[arg(long, requires = "offline")]` on tail's `json` to keep the contract crisp.
- `--json` + `--status` on `room members`: for this issue, implement `--json` for the **offline**
  roster; if `--status` is also set, either (a) reject the combination for now, or (b) include the
  live `conn` field in JSON. Recommended (a) — reject with an actionable message and track (b) as a
  follow-up, since `--status` is an online path owned by #22.

Dispatch in `dispatch_room` (`cli.rs:311`):

```rust
RoomAction::Tail { room_id, offline, json, peers, limit, accept_joins, loopback } => {
    let room_id = parse_room_id(&room_id)?;
    if offline {
        room::tail_offline(home, &room_id, limit, json)?;      // NEW, fully synchronous
    } else {
        runtime()?.block_on(message::tail(home, &room_id, &peers, limit, accept_joins, loopback))?;
    }
}
RoomAction::Members { room_id, json, status, peers, timeout, loopback } => {
    let room_id = parse_room_id(&room_id)?;
    if status {
        // unchanged online path (reject `--json` combo for now, or extend later)
        let timeout = message::parse_timeout(&timeout)?;
        runtime()?.block_on(message::members_status(home, &room_id, &peers, timeout, loopback))?;
    } else {
        let view = room::members(home, &room_id)?;
        if json { room::print_members_json(&view)?; } else { room::print_members(&view); }
    }
}
```

### Step 2 — Core-free shared display helper: `membership_display` (D5)

In `room.rs` (or a small `src/display.rs` shared by `room.rs` and `message.rs`), add:

```rust
/// Presentational membership state for a member row: `active` | `invited` | `removed` | `left`.
/// `left`/`removed` are the SAME zero-capability security state (the fold's `Status::Removed`);
/// the distinction is display-only, derived from the terminal departure event in the log.
pub(crate) enum MemberDisplayState { Active, Invited, Removed, Left }

/// Derive the display state from the folded `Status` plus, for a departed subject, the log:
/// a `member.removed` targeting the subject ⇒ Removed (admin action dominates);
/// else a `member.left` by the subject ⇒ Left; else Removed.
pub(crate) fn member_display_state(
    status: Status,
    subject: &IdentityKey,
    removed_ids: &BTreeSet<IdentityKey>,  // member.removed.member_id set (from store.by_type)
    left_ids: &BTreeSet<IdentityKey>,     // member.left.member_id set
) -> MemberDisplayState { /* match status; disambiguate Removed via the two sets */ }
```

- Build `removed_ids` / `left_ids` once per command by decoding `store.by_type(room,
  EventType::MemberRemoved)` and `EventType::MemberLeft` (small; membership sub-DAG only).
- Text label: `active|invited|removed|left`. JSON: same string in `"status"`.
- Update the online `message::status_label` / `member_conn_field` and the offline
  `room::status_str` to route departed members through this helper so all three surfaces agree.

### Step 3 — Offline tail: `room::tail_offline` (D1–D3, D6)

New `room::tail_offline(home, room_id, limit, json) -> Result<()>`:

1. Open store; `room_event_ids`; if empty → `bail!("no room {room_id} in {home}; run \`room create\` or \`room join\` first")` (mirror `members`).
2. Re-validate + fold into a `snapshot` (reuse the exact loop from `room::members` — factor a shared
   `fold_room_offline(&store, home, room_id) -> Result<(MembershipSnapshot, Vec<ValidatedEvent>)>` so
   `members`, `tail_offline`, and the message path share one implementation).
3. Build the `display_names` map from local `member.joined` events (reuse the helper currently in
   `message.rs:734`; move it to the shared module).
4. `rows = store.room_tail(room_id, limit)`.
5. For each `StoredEvent`, decode `SignedEvent`; assemble a `TailRow`:
   - `event_id` (`blake3:<hex>`), `event_type` (`ev.event_type.as_str()`), `lamport`
     (`se.lamport`), `admin_seq` (`se.admin_seq`), `created_at` (ms) + `at` (ISO-8601 UTC),
     `from` (short id), `display_name` (opt), `role` (`admin|member|agent|unknown`), `status`
     (D5 display state), and a `summary` string from `content_summary(&ev.content)`.
6. Emit:
   - **Text** (default), one line per row, stable prefix then free-form summary:
     ```
     event=<event_id> type=<type> lamport=<n> from=<short> role=<role> status=<state> at=<iso8601>  <summary>
     ```
   - **JSON** (`--json`): collect `Vec<TailRow>` and `println!("{}", serde_json::to_string(&rows)?)`
     (a single JSON array). Derive `serde::Serialize` on `TailRow` with stable field names; omit
     `None` fields with `#[serde(skip_serializing_if = "Option::is_none")]`.
7. Exit 0. (Empty timeline — e.g. a freshly created room whose only event is genesis — prints the
   genesis row; an all-incomplete log prints an empty array / no lines and exits 0.)

`content_summary(&Content)`:

| Type | Summary (human tail; not parsed by tests) |
|---|---|
| `room.created` | `name="<room name>"` |
| `member.invited` | `invitee=<short> role=<role>[ expires=<iso8601>]` |
| `member.joined` | `role=<role>[ name="<display_name>"]` |
| `member.left` | `[reason="<reason>"]` |
| `member.removed` | `subject=<short> by=<short>[ reason="<reason>"]` |
| `message.text` | `[format=<fmt>] body=<body>` (body last; may contain spaces — that is why the parsed fields are the prefix, not the body) |
| `file.shared` | `name="<name>" size=<bytes> hash=<short>` |
| `pipe.opened` | `pipe=<short>[ label="<label>"]` |
| `pipe.closed` | `pipe=<short>` |
| `agent.status` | `state=<state> text=<...>` |

> The JSON object SHOULD carry the type-specific fields as real keys (e.g. `body`, `format`,
> `in_reply_to`, `file_name`, `pipe_id`) so structured tests assert on them directly rather than on
> the summary string. Keep the set minimal and additive.

### Step 4 — `room members --json`: `room::print_members_json` (D4/D6)

Add `print_members_json(view: &MembersView) -> Result<()>`:

```json
{
  "room": "blake3:…",
  "admin": "…identity_id… | null",
  "members": [
    { "identity_id": "…", "role": "admin", "status": "active", "is_admin": true }
  ]
}
```

- `status` values: `active|invited|removed|left` (D5). Deterministic member order (the fold already
  yields identity-sorted `members()`).
- Print with `serde_json::to_string` (single line, mirrors `identity show --json`).

### Step 5 — Optional core builders for the removed/left test path (D8; gate on OQ-3)

If adopting D8, add to `crates/iroh-rooms-core/src/event/`:

- `build_member_left(sender_identity_secret, sender_device_secret, room_id, reason: Option<&str>,
  prev_events, created_at) -> WireEvent` — content `MemberLeft { member_id: sender_id, reason }`.
- `build_member_removed(admin_identity_secret, admin_device_secret, room_id, subject: IdentityKey,
  reason: Option<&str>, device_binding: Option<DeviceBinding>, prev_events, created_at) -> WireEvent`
  — content `MemberRemoved { member_id: subject, removed_by: admin_id, reason, device_binding }`.

Both mirror `build_member_invited` (`invite.rs:43`): assemble `SignedEvent`, `to_csb()`,
`sign_csb(&csb, device_secret)`, `WireEvent::seal`. Re-export from `event/mod.rs`. Golden/regression
tests as for the siblings. **If not adopting D8**, the test constructs these events raw via the
public `SignedEvent` + `signed::sign_csb` + `WireEvent::seal` path in a `tests/` helper.

### Step 6 — Docs reconciliation

- `docs/getting-started.md`: document `room tail <ROOM_ID> --offline [--json]` as the deterministic,
  network-free read (distinct from the live receiver) and `room members <ROOM_ID> --json`; add a
  short note that `left` vs `removed` is shown. Keep the existing live-tail / `--accept-joins` /
  `--status` sections intact.
- `tests/docs_conformance.rs`: extend structural assertions if it pins the tail/members blocks
  (mirrors the #17/#20/#22 docs commits). Keep it network-free.
- `README.md`: add the landed-feature paragraph in house style once implemented.

---

## 6. CLI surface & output (reference)

```text
iroh-rooms [--data-dir <PATH>] room tail    <ROOM_ID> --offline [--json] [--limit <N>]
iroh-rooms [--data-dir <PATH>] room tail    <ROOM_ID> [--peer <ADDR>]... [--limit <N>] [--accept-joins]   # unchanged online session
iroh-rooms [--data-dir <PATH>] room members <ROOM_ID> [--json]
iroh-rooms [--data-dir <PATH>] room members <ROOM_ID> --status [--peer <ADDR>]... [--timeout <DUR>]        # unchanged online session
```

Offline `room tail --offline` (text; illustrative — reconcile against the binary):

```text
event=blake3:aa… type=room.created  lamport=0 from=alice9f8e role=admin  status=active  at=2026-06-30T12:00:00Z  name="Build Room"
event=blake3:bb… type=member.invited lamport=1 from=alice9f8e role=admin  status=active  at=2026-06-30T12:00:05Z  invitee=bob1a2b3c role=member
event=blake3:cc… type=member.joined  lamport=2 from=bob1a2b3c role=member status=left    at=2026-06-30T12:00:40Z  role=member name="Bob"
event=blake3:dd… type=message.text   lamport=3 from=bob1a2b3c role=member status=left    at=2026-06-30T12:01:04Z  body=I pushed the first prototype.
event=blake3:ee… type=member.left    lamport=4 from=bob1a2b3c role=member status=left    at=2026-06-30T12:05:00Z
```

Offline `room tail --offline --json` (single JSON array; illustrative):

```json
[{"event_id":"blake3:aa…","event_type":"room.created","lamport":0,"admin_seq":0,"created_at":1751284800000,"from":"alice9f8e","role":"admin","status":"active","room_name":"Build Room"},
 {"event_id":"blake3:dd…","event_type":"message.text","lamport":3,"created_at":1751284864000,"from":"bob1a2b3c","role":"member","status":"left","body":"I pushed the first prototype.","format":"plain"}]
```

`room members --json`:

```json
{"room":"blake3:…","admin":"…","members":[{"identity_id":"…","role":"admin","status":"active","is_admin":true},{"identity_id":"…","role":"member","status":"left","is_admin":false}]}
```

---

## 7. Error & observability model

- **Actionable failures (exit non-zero, nothing written — reads never write):**
  - malformed `<ROOM_ID>` → `invalid room id (expected \`blake3:<hex>\`)` (shared `parse_room_id`);
  - unknown room (`room_event_ids` empty) → `no room <id> in <home>; run \`iroh-rooms room create\` or \`iroh-rooms room join\` first`;
  - store open/read error → context-wrapped IO error;
  - a stored event fails §6 re-validation → `stored event <id> failed re-validation (<code>)` (on-disk
    corruption — surfaced, never silently skipped);
  - incompatible flags (`--offline` with `--peer`/`--accept-joins`/`--loopback`; `--json` without
    `--offline` on tail; `--json` with `--status` on members) → clap error / actionable message.
- **Honest, no invented state.** An offline read reports only what is in the local log: it never
  prints peers, `listening:` addresses, or online/offline connection state (that is the online
  session's job). Availability is not hidden — it is simply *not claimed* by a local read.
- **Determinism.** Given identical `rooms.db` bytes, byte-identical output every run and on every
  peer holding the same validated set (§2 convergence). No wall-clock read in the read path
  (`created_at` comes from the events; `at=` is a pure function of the stored `created_at`).

---

## 8. Security, privacy, reliability, performance

- **No new trust surface.** Reads project the already-validated local log; no crypto, no new
  validation, no authoring, no network. Re-validation (D3) means a tampered on-disk row is caught and
  reported, not displayed as truth.
- **Secret hygiene.** Offline reads load **no** secret key material (D7); integration tests assert no
  seed bytes appear in stdout/stderr (mirroring `room_cli.rs` / `invite_cli.rs`), and the optional D8
  builders keep signing secrets in `Zeroizing` exactly like the landed builders.
- **Trust-free presentation (§2.4).** Rows are ordered by `(lamport, event_id)` only; the reader
  attaches no meaning to position and never orders by `created_at`. A since-removed member's rows are
  clearly tagged (`status=removed|left`) so a reader is not misled into treating them as current.
- **Privacy.** Purely local; nothing leaves the machine. `--json` is the same information as text, in
  a parseable shape.
- **Performance.** MVP rooms are small (≤5 members, bounded recent history). Full re-validation + fold
  per read is O(log size) and negligible; `store.room_tail`'s `LIMIT` bounds rows. No async runtime is
  started for the offline path (unlike the online commands), so startup is fast.

---

## 9. Test strategy

All CLI tests use `assert_cmd` + per-test `--data-dir` temp home with `IROH_ROOMS_HOME` cleared, and
build state with the landed `identity create` / `room create` / `room invite` / `room join` (or, for
departures, D8 builders / raw construction) — **no network** for any test here.

### 9.1 Offline `room tail --offline`

- **AC1 order:** create a room, author ≥2 `message.text` (via landed `room send` offline half, which
  persists locally without peers, or via a temp-DB seed), then `room tail --offline` — assert the
  `event=`/`lamport=` prefixes appear in ascending `(lamport, event_id)` order and are stable across
  repeated runs (byte-identical stdout).
- **AC1 "validated events":** assert non-message types appear — a freshly created room's
  `--offline` output contains a `type=room.created` row; after an invite, a `type=member.invited` row.
- **AC4 structured:** `room tail --offline --json` parses as a JSON array via `serde_json`; assert the
  expected `event_type` sequence and a known `body` field, with **no** dependence on whitespace or the
  free-form summary.
- **Attribution (Scope):** assert each row carries `from=`/`role=` and that a message from the admin
  reads `role=admin`.
- **Errors:** unknown room → non-zero + "no room"; malformed id → non-zero + "invalid room id";
  `--offline --peer <x>` → clap conflict error.
- **Restart determinism:** a second process invocation over the same `--data-dir` yields identical
  output (state comes from `rooms.db`).

### 9.2 `room members --json` (extends `tests/room_cli.rs`)

- **AC2:** freshly created room → JSON with one member, `role":"admin"`, `"status":"active"`,
  `"is_admin":true`; `admin` equals `identity show`'s `identity_id`.
- **AC4:** JSON parses via `serde_json`; existing text-output tests remain green (default unchanged).

### 9.3 Removed / left representation (AC3) — the headline new test

Build the chain `room.created (Alice, admin) → member.invited(Bob) → member.joined(Bob)` in a temp DB,
then author a departure and assert display:

- **Left:** append a `member.left` by Bob (`member_id == Bob`); assert `room members` /
  `room members --json` shows Bob `status=left` and `room tail --offline` tags Bob's rows
  `status=left`.
- **Removed:** in a separate room, append a `member.removed` by Alice targeting Bob; assert Bob shows
  `status=removed`.
- **Admin-removal dominates:** with both a `member.left` (Bob) and a later `member.removed` (Alice→Bob),
  assert Bob shows `status=removed` (D5 dominance rule).
- Departure events are produced via the D8 builders (preferred) or the raw `SignedEvent` +
  `sign_csb` + `WireEvent::seal` helper (OQ-3). Assert the departed member is **shown**, never
  silently omitted.

### 9.4 Core (only if D8 adopted)

- `build_member_left` / `build_member_removed`: determinism, content round-trip,
  `built_event_passes_stateless_validation`, `signature_verifies_under_device_id`, golden `event_id`
  regression lock (mirror `invite.rs` / `join.rs` tests).

---

## 10. Acceptance criteria → evidence

| # | Criterion | Satisfied by | Test |
|---|---|---|---|
| AC1 | `room tail` shows validated events in deterministic order | §6 re-validate (D3) + `store.room_tail` `(lamport, event_id)` order (D2); all event types projected | §9.1 order + validated-events + restart |
| AC2 | `room members` shows admin/member/agent status | landed fold `role`/`status` via `room::members` (D4) | §9.2 |
| AC3 | Removed/left members represented clearly when relevant | `membership_display` log-derived refinement `removed`/`left`, departed members always shown (D5) | §9.3 |
| AC4 | Output parseable by tests without brittle formatting | JSON array + stable `key=value` text prefix (D6) | §9.1 JSON, §9.2 JSON |

---

## 11. Risks

- **R1 — Overloading `room tail` (mode confusion).** Two behaviors under one command (`--offline`
  snapshot vs live session). *Mitigation:* `conflicts_with_all` so modes can't mix; distinct help
  text; docs clearly separate them. OQ-1 tracks the "distinct `room log` noun" alternative.
- **R2 — Left/removed distinction is display-derived, not folded.** A future fold change could shift
  the source of truth. *Mitigation:* D5 is explicitly presentational and documented as such; the
  security lattice (`Status::Removed`) is untouched; one shared helper prevents divergence across the
  three surfaces.
- **R3 — Message `body` in text output is free-form (spaces/newlines).** A naive parser could choke.
  *Mitigation:* tests parse the fixed `key=value` **prefix**, not the body; `--json` is the robust
  contract; document that the body is the trailing free-form field.
- **R4 — D8 stretches read-only scope.** Adding authoring builders in a read issue. *Mitigation:*
  builders are pure, test-only here, and are the natural siblings future issues need; OQ-3 offers the
  raw-construction alternative if the orchestrator wants zero core change.
- **R5 — Docs conformance drift.** getting-started/README pin CLI blocks. *Mitigation:* reconcile in
  the same change (Step 6), as prior CLI issues did.
- **R6 — `--json` + `--status` combination on `room members`.** The online status path is owned by
  #22. *Mitigation:* reject the combination for now (Step 1) with an actionable message; track JSON
  for `--status` as a follow-up.

---

## 12. Open questions

- **OQ-1 — Command shape: `room tail --offline` vs `--once`/`--no-follow` vs a distinct `room log`.**
  This spec chooses `--offline` on `room tail` (matches PRD §16 naming; additive). A dedicated
  `room log <ROOM_ID>` noun would avoid overloading but adds surface. *Recommendation:* `--offline`
  now; revisit `room log` if the two modes prove confusing. **Non-blocking; naming only.**
- **OQ-2 — Represent departure as a refined `status` (`left`) or a separate field
  (`status=removed departure=self`).** This spec folds it into `status` (D5) for a single clear field.
  A separate `departure=` field keeps `status` strictly the fold lattice. *Recommendation:* refined
  `status` for human clarity; expose the raw fold status too in JSON if consumers need it
  (`"fold_status":"removed"`). **Needs a light UX call.**
- **OQ-3 — Adopt the D8 core builders, or construct departure events raw in the test helper?**
  Builders are cleaner and reusable but touch core in a read issue; raw construction keeps #21
  strictly CLI-only. *Recommendation:* add the builders (small, pure, needed soon); fall back to raw
  if the orchestrator wants zero core diff. **Needs a scope call.**
- **OQ-4 — JSON array vs JSONL for `room tail --offline --json`.** Array is unambiguous for a finite
  snapshot and easy to assert on; JSONL is stream-friendly and would extend naturally to a future
  `--json` online tail. *Recommendation:* array now; if/when the online tail gains `--json`, use JSONL
  there. **Non-blocking.**
- **OQ-5 — Should offline `room tail` filter by type (e.g. `--type message.text`) or default to
  messages-only for parity with the live tail?** This spec defaults to **all** validated events
  (AC1 "validated events"). A `--type <t>` filter is an obvious, cheap follow-up. *Recommendation:*
  ship all-events now; add `--type` later if noise is a problem. **Non-blocking.**

---

## 13. Out of scope

- Any change to the **online** `room tail` streaming session or `room members --status` behavior
  beyond routing departed members through the shared display helper (D5).
- Authoring commands for departures (`room leave`, `member remove`) — the D8 builders are added (if
  adopted) but are **not** wired to any command in this issue.
- File sharing, live pipe reads, and `agent.status` authoring/display beyond rendering an
  `agent.status` row if one already exists in the log (those event families ship in sibling issues).
- Follow/stream semantics for the offline reader (it is a one-shot snapshot that exits); a `--follow`
  offline tail is a future option.
- Network/real-NAT concerns (offline reads never touch the network).

---

## 14. File-change summary

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/src/cli.rs` | `RoomAction::Tail` gains `--offline` (+ `--json`, `conflicts_with_all`); `RoomAction::Members` gains `--json`; dispatch offline reads synchronously |
| `crates/iroh-rooms-cli/src/room.rs` | **add** `tail_offline`, `print_members_json`, shared `fold_room_offline`, `content_summary`, `TailRow` (serde); refine `status` via `member_display_state` |
| `crates/iroh-rooms-cli/src/message.rs` | route `status_label`/`member_conn_field` and `display_names`/`iso8601_utc` through the shared display module (avoid duplication) |
| `crates/iroh-rooms-cli/src/display.rs` *(optional)* | **new** — shared `member_display_state`, `short_id`, `iso8601_utc`, `display_names` if factored out of `message.rs` |
| `crates/iroh-rooms-core/src/event/{left.rs,removed.rs}` *(optional, D8)* | **new** — pure `build_member_left` / `build_member_removed` + golden tests |
| `crates/iroh-rooms-core/src/event/mod.rs` *(optional, D8)* | `pub mod` + `pub use` the two builders |
| `crates/iroh-rooms-cli/tests/room_cli.rs` | **add** `room members --json` + AC2/AC4 cases |
| `crates/iroh-rooms-cli/tests/tail_cli.rs` | **new** — offline tail order/validated-events/JSON/errors/restart (§9.1) + removed/left (§9.3) |
| `docs/getting-started.md`, `crates/iroh-rooms-cli/tests/docs_conformance.rs` | document `--offline`/`--json`; reconcile blocks |
| `README.md` | landed-feature paragraph (house style) once implemented |
```
