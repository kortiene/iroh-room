# Spec: Room List Read CLI (IR-0307)

| | |
|---|---|
| **Issue** | #TBD — `[IR-0307] Implement room list read CLI` *(IR number to confirm against the tracker; next free after IR-0306)* |
| **Parent epic** | #4 (Phase 2 — Developer Preview epic) |
| **Labels** | `type/feature` `area/cli` `priority/p1` `risk/low` |
| **Dependencies** | #21 (IR-0106, offline room-read CLI — **landed**), #8 (IR-0004, SQLite event store — **landed**; provides `EventStore::room_ids`), #23 (IR-0108, pipe close room inference — **landed**; first consumer of `room_ids`) |
| **Traceability** | PRD `PRD.v0.3.md` §16 (CLI requirements — script-friendly output, honest availability), §17.2 (developer-experience metrics); Spike `PHASE-0-SPIKE.md` Membership & Ordering §2 (deterministic order), §3 (status/role lattices). External consumer: `docs/cockpit-backlog.md` UP-101 (Pi Rooms Cockpit — room discovery) |
| **Owning crate** | `crates/iroh-rooms-cli` (read-only projection over landed primitives). **No core/net change** — `EventStore::room_ids()` already exists (`store/mod.rs:206`) and is already consumed by `pipe close` room inference (`pipe.rs:582`). |

> **Status:** planning. This document is the build plan for another engineer/agent to execute.
> **Do not implement from this doc in the same change that writes it.** The compiled binary is
> the source of truth once landed.

---

## 1. Summary

Expose a **local room enumeration** so a user (or tool) can discover which rooms exist in a
data directory without already knowing their ids:

```bash
# Labeled lines (default):
iroh-rooms room list

# Single-line JSON array (script contract):
iroh-rooms room list --json
```

Today every room-scoped command requires a `<ROOM_ID>` the caller must have saved from
`room create` / `room join` output. The store can already enumerate rooms
(`EventStore::room_ids()`, ascending, de-duplicated, restart-deterministic — proven by
`store/tests.rs:694`–`:924`), and `pipe close` already uses it internally to infer a pipe's
room. What is missing is the **user-facing read surface**.

### 1.1 Why this issue is a thin read surface

Everything load-bearing is landed and conformance-tested:

- **`EventStore::room_ids()`** (`store/mod.rs:206`) — ascending, de-duplicated room ids from
  the derived cache; identical before/after `rebuild()` (restart determinism).
- **`message::fold_room`** — the shared re-validate-and-fold loop every offline read uses
  (`room members`, `room tail --offline`, `file list`, `pipe list`); yields the
  `MembershipSnapshot` (admin, members, roles, statuses).
- **`room.created` decode** — `Content::RoomCreated { room_name }` gives the display name.
- **`store.room_tail(room, 1)`** — the most recent causally-placed event, for a `last_event_at`
  freshness column.
- **CLI conventions** — labeled-line text + single-line JSON (`identity show --json`,
  `room members --json`, `file list --json`); IR-0110 coded errors; offline reads load no
  secret and require no membership.

> **Why `risk/low`.** No new crypto, no new validation rule, no authoring, no network, no
> schema change. This is a read-only projection of the already-validated local log, in the
> exact trust posture of the landed offline reads.

---

## 2. Background & current repository state

### 2.1 What exists

- `RoomAction` (`cli.rs:283`) has `Create | Members | Invite | Send | Tail | Join` — **no
  `List`**.
- `EventStore::room_ids()` (`store/mod.rs:206`): public, tested (empty store → empty vec;
  multi-room ascending de-duplicated; rebuild-stable).
- `pipe::resolve_pipe_room` (`pipe.rs:582`) already calls `store.room_ids()` — precedent for
  cross-room enumeration in the CLI.
- Offline read pattern (`room::members`, `room.rs`; `room::tail_offline`, `room.rs:297`):
  open store → `fold_room` (re-validate §6 + fold, loud failure on a corrupt row) → project →
  print text or single-line JSON.

### 2.2 The gap this issue closes

1. **No discovery.** A user with an existing `<home>` (e.g. after reinstalling a front-end, or
   driving the CLI from a tool) cannot enumerate rooms; external tools resort to reading
   `rooms.db` directly — brittle across schema bumps (v1→v2 already happened, IR-0201).
2. **No freshness signal.** Even knowing ids, there is no cheap "which room is active" read
   short of tailing each one.

### 2.3 Constraints

- **Determinism.** Given identical `rooms.db` bytes, byte-identical output (ascending room-id
  order — the `room_ids()` contract).
- **Offline honesty (PRD §16).** A local read never invents peers or online state; it reports
  only what the log holds.
- **Additive only.** No change to any landed command, output block, or schema.

---

## 3. Design decisions

- **D1 — New `room list` subcommand; offline, synchronous, no identity, no membership.** The
  same trust posture as `room tail --offline` / `room members`: reading your own local log
  needs no key. No `Node`, no async runtime.

- **D2 — One row per `room_ids()` entry, enriched by a per-room fold.** For each room:
  `name` (genesis `room.created.room_name`), `admin` (fold snapshot; `null`/`<none>` if no
  genesis in scope), `members` (total roster size), `active` (count of `Status::Active`),
  `events` (`store.count(room)`), `last_event_at` (the `created_at` of `room_tail(room, 1)`'s
  row, ISO-8601; advisory/display-only per spike §2.3 — **never** an ordering claim). MVP
  rooms are small; a full fold per room is O(log size) and negligible (mirrors every landed
  offline read).

- **D3 — Corrupt room fails loudly, listing does not silently skip.** `fold_room`'s existing
  behavior: a stored event failing §6 re-validation is on-disk corruption and surfaces as an
  error naming the room — never displayed as truth, never silently omitted. (Degrade-with-
  warning alternative: OQ-2.)

- **D4 — Two output shapes, matching house style.**
  - **Text (default):** `pipe list`-style labeled block per room (id line + indented fields).
  - **`--json`:** a single-line JSON **array** of objects with stable lowercase-snake fields
    (the finite-snapshot contract, mirroring `room tail --offline --json`).

- **D5 — Empty store is success, not failure.** `(no rooms)` / `[]`, exit 0 — an empty home
  is a normal state (mirrors `file list`'s `(no shared files)`); the `room_not_found` code is
  for a *named* room that does not exist, which cannot occur here.

---

## 4. Architecture & data flow

```
open EventStore(<home>/rooms.db)                       [offline; missing db ⇒ empty listing]
 → rooms = store.room_ids()                            (ascending, de-duplicated)
 → for room in rooms:
       (_, snapshot) = message::fold_room(&store, home, &room)   [D2/D3: loud on corruption]
       name    = first room.created's room_name        (store.by_type(room, RoomCreated))
       admin   = snapshot.admin()
       members = snapshot.members().len()
       active  = snapshot.members().filter(Active).count()
       events  = store.count(room)
       last_at = store.room_tail(room, 1) → created_at → iso8601_utc   (advisory display)
 → print text blocks OR one JSON array line
 → exit 0
```

---

## 5. Detailed implementation steps

1. **`cli.rs`** — add to `RoomAction`:

   ```rust
   /// List every room known to the local store (offline read).
   List {
       /// Emit a single JSON array instead of labeled lines.
       #[arg(long)]
       json: bool,
   },
   ```

   Dispatch in `dispatch_room`: `RoomAction::List { json } => room::list(home, json)?`
   (fully synchronous — no `runtime()`).

2. **`room.rs`** — add `pub fn list(home: &Path, json: bool) -> Result<()>` implementing §4,
   plus a `RoomRow` serde struct (`room_id`, `name`, `admin: Option<String>`, `members`,
   `active`, `events`, `last_event_at: Option<String>`; skip-if-none on optionals). A missing
   `rooms.db` file renders the empty listing (do not create the db as a side effect).

3. **Docs reconciliation** — `docs/getting-started.md` (a short "which rooms do I have?"
   note near Step 2), `README.md` landed-feature paragraph (house style) once implemented.

---

## 6. CLI surface & output (reference — reconcile against the binary)

```text
iroh-rooms [--data-dir <PATH>] room list [--json]
```

Text (default):

```text
room_id: blake3:aa…
  name: "Build Room"
  admin: 9f8e…{64-hex}
  members: 3 (3 active)
  events: 42
  last_event_at: 2026-06-30T12:05:00Z
room_id: blake3:bb…
  name: "QA Room"
  admin: 9f8e…{64-hex}
  members: 2 (1 active)
  events: 7
  last_event_at: 2026-06-28T09:12:44Z
```

Empty store:

```text
(no rooms)
```

`--json` (single line; shown wrapped):

```json
[{"room_id":"blake3:aa…","name":"Build Room","admin":"…","members":3,"active":3,
  "events":42,"last_event_at":"2026-06-30T12:05:00Z"}]
```

---

## 7. Error & observability model

- **Success:** empty store → `(no rooms)` / `[]`, exit 0 (D5).
- **Failures (exit non-zero, nothing written — reads never write):** store open/read error →
  context-wrapped IO error (uncoded, exit 1); a stored event failing §6 re-validation →
  loud corruption error naming the room (D3); `--json` encode failure (cannot occur for this
  value) → context-wrapped.
- **Honesty:** no peers, no `listening:`, no connection state — a local read claims nothing
  about availability. `last_event_at` is advisory display only.

---

## 8. Security, privacy, reliability, performance

- **No new trust surface.** Read-only projection; re-validation catches tampered rows.
- **Secret hygiene.** No secret load (D1); tests assert no seed bytes in any output stream.
- **Performance.** O(rooms × log size) with MVP-sized rooms; no async runtime; fast startup.
- **Restart determinism.** Output is a pure function of `rooms.db` bytes; `room_ids()` is
  rebuild-stable (proven in `store/tests.rs:906`).

---

## 9. Test strategy

All tests: `assert_cmd` + per-test `--data-dir` temp home, `IROH_ROOMS_HOME` cleared, no
network (state built via landed `identity create` / `room create` / offline authoring).

- **Empty:** fresh home → `(no rooms)` text / `[]` JSON, exit 0; `rooms.db` not created.
- **Single room:** after `room create` → one block; `room_id`/`admin` equal the create
  output / `identity show`; `members: 1 (1 active)`; `name` round-trips.
- **Multiple rooms:** three creates → three blocks in ascending room-id order; byte-identical
  across repeated invocations (restart determinism).
- **JSON contract:** parses via `serde_json`; field names/types asserted; no dependence on
  whitespace.
- **Freshness:** author a message into one room → its `last_event_at` reflects the newest
  event's `created_at`; `events` increments.
- **Secret hygiene:** no seed bytes in stdout/stderr.

---

## 10. Acceptance criteria → evidence

| # | Criterion | Satisfied by | Test |
|---|---|---|---|
| AC1 | `room list` enumerates every local room deterministically | `room_ids()` ascending + per-room fold (D1/D2) | §9 multiple-rooms + determinism |
| AC2 | Empty store is success (`(no rooms)` / `[]`, exit 0) | D5 | §9 empty |
| AC3 | Output parseable without brittle formatting | JSON array + labeled-line text (D4) | §9 JSON contract |
| AC4 | No network, no identity load, no membership requirement | D1 (posture of `room tail --offline`) | §9 all (offline harness) |
| AC5 | Docs reconciled | Step 3 | docs conformance |

---

## 11. Risks

- **R1 — Per-room fold cost on large stores.** Post-MVP rooms could make listing slow.
  *Mitigation:* MVP scope is small rooms; a `--fast` ids-only mode is a cheap follow-up (OQ-1).
- **R2 — `name` collides with untrusted content.** Room names are peer-authored strings.
  *Mitigation:* already bounded + control-character-rejected at the trust boundary
  (`validate_room_name` / §7 content validation); render quoted.
- **R3 — Corruption in one room blocks listing all rooms (D3).** *Mitigation:* deliberate —
  consistent with every landed offline read; OQ-2 tracks degrade-with-warning.

---

## 12. Open questions

- **OQ-1 — ids-only fast path?** `room list --ids` skipping the fold (pure `room_ids()`).
  *Recommendation:* not now; add if listing cost ever matters. **Non-blocking.**
- **OQ-2 — Corrupt room: fail the whole listing (D3) or emit the row as
  `name: <corrupt>` + stderr warning?** *Recommendation:* fail loudly now (house
  consistency); revisit if mixed-health stores become a real workflow. **Needs a scope call.**
- **OQ-3 — Include `last_event_at` at all?** It is advisory-clock display and could mislead.
  *Recommendation:* include, labeled ISO-8601, consistent with `at=` in the offline tail;
  never used to order. **Non-blocking.**

---

## 13. Out of scope

- Any online/live state (connection counts, peer liveness) — `room members --status` owns that.
- Room deletion/archival, name editing, or any authoring surface.
- Cross-home aggregation (one data directory per invocation, as everywhere else).
- Changes to `EventStore` — `room_ids()` is used as-is.

---

## 14. File-change summary

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/src/cli.rs` | `RoomAction::List { json }` + synchronous dispatch |
| `crates/iroh-rooms-cli/src/room.rs` | **add** `list`, `RoomRow` (serde), per-room projection |
| `crates/iroh-rooms-cli/tests/room_cli.rs` | **add** §9 cases (empty / single / multi / JSON / freshness / hygiene) |
| `docs/getting-started.md` | short discovery note near Step 2 |
| `README.md` | landed-feature paragraph (house style) once implemented |
