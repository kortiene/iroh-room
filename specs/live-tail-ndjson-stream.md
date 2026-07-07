# Spec: Live Tail NDJSON Stream (IR-0308)

| | |
|---|---|
| **Issue** | #TBD — `[IR-0308] Stream all validated event types from the live room tail as NDJSON` *(IR number to confirm against the tracker; next free after IR-0307)* |
| **Parent epic** | #4 (Phase 2 — Developer Preview epic) |
| **Labels** | `type/feature` `area/cli` `priority/p1` `risk/medium` |
| **Dependencies** | #20 (IR-0105, online `room tail` — **landed**), #21 (IR-0106, offline tail `--json` + `TailRow`/`content_fields` — **landed**), #22 (IR-0107, connection panel — **landed**), #33 (IR-0208, `agent.status` authoring + offline display — **landed**), #25 (IR-0110, `warning[<code>]` receive-path surface — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §16 (script-friendly output), §16.3 (connection trichotomy); Spike `PHASE-0-SPIKE.md` Membership & Ordering §2 (deterministic `(lamport, event_id)` order, §2.3 advisory `created_at`); `RELEASE-READINESS.md` "Known MVP limitations" → **Live-tail display gap** (this issue closes it, in JSON mode). External consumer: `docs/cockpit-backlog.md` UP-102 (Pi Rooms Cockpit — live feed without poll-diffing) |
| **Owning crate** | `crates/iroh-rooms-cli` only. `Node::room_tail` (`net/src/node.rs:549`) and `node.conn_events()` already surface everything needed; **no core/net change**. |

> **Status:** planning. This document is the build plan for another engineer/agent to execute.
> **Do not implement from this doc in the same change that writes it.** The compiled binary is
> the source of truth once landed.

---

## 1. Summary

Allow `--json` on the **online** `room tail` session, turning stdout into an NDJSON stream —
one JSON object per line — carrying **every newly-validated event type** (not just
`message.text`) plus structured session records (listening address, peer transitions, roster
summaries, receive-path warnings):

```bash
# Live NDJSON stream (long-running until Ctrl-C):
iroh-rooms room tail <ROOM_ID> --json [--peer <ADDR>]... [--limit <N>] [--accept-joins]

# Unchanged: offline one-shot JSON array
iroh-rooms room tail <ROOM_ID> --offline --json
```

This closes the documented **live-tail display gap** (`RELEASE-READINESS.md`: "streaming
`room tail` renders only `message.text`; `agent.status`/`file.shared` rows show only in
`room tail --offline`") for machine consumers, and gives tools a real event stream instead of
poll-diffing the offline read.

### 1.1 Why this issue is presentation-only integration

Every load-bearing piece is landed:

- the display loop already polls the full timeline (`node.room_tail(limit)`,
  `message.rs:542`) — the `message.text` filter is applied only at print time
  (`print_new_messages`, `message.rs:1012`);
- the **row schema and type-specific fields already exist**: the offline `TailRow` +
  `content_fields` (`room.rs:256`, `room.rs:474`) render every event type, including
  `agent.status` (`state`/`message`/`progress`/`artifacts`) and `file.shared`
  (`file_name`/`size_bytes`/`blob_hash`);
- attribution inputs (fold snapshot, departure sets, display names) are already computed by
  the offline path (`display.rs`) and partially by the online path;
- peer transitions and roster summaries already stream as pinned text lines
  (`format_peer_line` / `roster_summary`, `message.rs:1086`/`:1106`);
- receive-path rejects already surface as `warning[<code>]` lines
  (`print_new_reject_warnings`, `message.rs:1061`).

> **Why `risk/medium`.** No protocol, schema, or authorization change — but this touches the
> long-running session loop (the demo's backbone), introduces a second output mode on a
> pinned-format command, and must keep the existing text mode **byte-identical** when `--json`
> is absent. The risk is regression of a heavily-documented surface, not correctness.

---

## 2. Background & current repository state

### 2.1 What exists

- **Online tail** (`message::tail`, `message.rs:422`): membership-gated, brings up a managed
  `Node` (blob-serving, join-bootstrap, connection panel), prints `listening:`/`room:` header
  lines, then a `tokio::select!` loop over Ctrl-C / `conn_events` / a poll ticker that calls
  `node.room_tail(limit)` and `node.logs()`.
- **Print-time filter:** `print_new_messages` skips every `StoredEvent` whose
  `event_type != MessageText` — the seen-set only ever admits messages.
- **Offline JSON contract** (IR-0106): `TailRow` fields `event_id`, `event_type`, `lamport`,
  `admin_seq?`, `created_at`, `at`, `from`, `display_name?`, `role`, `status`, plus flattened
  `content_fields` per type. Pinned by `tail_cli.rs`.
- **clap surface** (`cli.rs:384`): `json: bool` carries `requires = "offline"` — the exact
  gate this issue relaxes.
- **IR-0106 OQ-4** anticipated this: *"array now; if/when the online tail gains `--json`, use
  JSONL there."* This spec is that follow-up.

### 2.2 The gaps this issue closes

1. **Live sessions are message-only.** A running peer cannot observe joins, file shares,
   pipe opens, or agent statuses live; tools poll `room tail --offline --json` (wasteful,
   laggy, and racy against the store).
2. **Session facts are text-scrape-only.** `listening:`, peer transitions, and roster
   summaries have pinned but unstructured formats; consumers re-parse them with regexes.

### 2.3 Constraints

- **Text mode must not move.** Without `--json`, output stays byte-identical — the
  getting-started guide, `two_peer_e2e.rs`, and `full_demo_e2e.rs` pin it.
- **Offline `--json` must not move.** The single-array contract and `TailRow` field names are
  pinned by `tail_cli.rs`; the live stream must reuse — not fork — that schema.
- **stdout stays clean for scripting** (house rule since IR-0108): in `--json` mode stdout
  carries NDJSON only; stderr keeps its existing role (audit sinks, `warning[<code>]`,
  `diag:`).
- **Ordering honesty (spike §2).** Emission order is arrival/poll order; each event record
  carries `lamport`/`event_id` so a consumer can reconstruct canonical order. Position in the
  stream carries no trust (§2.4).

---

## 3. Design decisions

- **D1 — Relax the clap gate: `--json` valid with the online session; offline behavior
  unchanged.** Remove `requires = "offline"` from `Tail.json`; `--offline --json` keeps the
  landed single-array read. `--json` (online) selects NDJSON streaming. `--verbose` remains
  compatible (its `diag:` block is stderr-only by construction).

- **D2 — NDJSON framing: exactly one JSON object per stdout line, flushed per line.** No
  array wrapper, no partial lines. A consumer reads line-by-line for the life of the process
  (Ctrl-C ends the stream; exit code semantics unchanged).

- **D3 — Every record carries a `kind` discriminator; event records embed the offline
  `TailRow` schema verbatim.**

  | `kind` | Payload (beyond `kind`) | Replaces (text mode) |
  |---|---|---|
  | `"event"` | all `TailRow` fields incl. flattened `content_fields` | `[ts] author: body` (and the gap: all other types) |
  | `"listening"` | `addr` (the exact string after `listening: `) | `listening:` + `tip:` lines |
  | `"room"` | `room_id` | `room: <id>` header |
  | `"peer"` | `identity` (short), `device` (short), `state`, `reason?` | `peer … state=… [reason=…]` |
  | `"roster"` | `connected`, `offline`, `unauthorized` | `peers: N connected, …` |
  | `"warning"` | `code` | `warning[<code>]: …` (also still on stderr — see D6) |

  The offline array rows gain **no** `kind` field (their contract is pinned); schema equality
  is asserted field-by-field for the shared `TailRow` subset (§9).

- **D4 — Emit ALL validated event types; keep the seen-set by `event_id` across types.** The
  loop reuses `node.room_tail(limit)` unchanged; the print-time `MessageText` filter simply
  does not apply in NDJSON mode. Initial backlog (up to `--limit` historical rows) is emitted
  first as ordinary `"event"` records — consumers dedupe by `event_id` (they must anyway,
  across reconnects). No `historical` flag (OQ-2).

- **D5 — Attribution parity with the offline read.** `role`/`status`/`display_name` come from
  the same fold-snapshot + departure-set + display-name inputs the offline `TailRow` uses
  (`display.rs`). The online path already computes the snapshot and names at startup; refresh
  the departure sets and names when a relevant membership event arrives (cheap: membership
  events are rare and already trigger fold changes in the pump). A mid-session role change
  therefore updates attribution on subsequent rows — same "current view" semantics as the
  offline read (D3 of IR-0106).

- **D6 — Human text is suppressed on stdout in NDJSON mode; stderr is unchanged.** The
  `listening:`/`tip:`/`room:`/message/peer/roster stdout lines are replaced by their D3
  records. `warning[<code>]` continues to stderr **and** is mirrored as a `"warning"` record
  (a stream consumer should not need to merge two pipes for the common case); `diag:`
  (`--verbose`) and audit vocabulary (`pipe.*`, `blob.serve.*`, `reject.*` via the audit sink)
  stay stderr-only and unstructured (structured diagnostics are a separate follow-up —
  `docs/cockpit-backlog.md` UP-105).

- **D7 — Text-mode display gap is NOT closed here.** Rendering non-message events in the
  human text mode (the `RELEASE-READINESS.md` limitation as written) is the smaller cosmetic
  alternative (UP-103 in `docs/cockpit-backlog.md`) and stays a separate decision — this spec
  deliberately does not touch text-mode output (§2.3 constraint). OQ-1.

---

## 4. Architecture & data flow

```
message::tail(home, room_id, peers, limit, accept_joins, loopback, verbose, json)   [+json]
 → … unchanged bring-up: fold gate, admission, Node::spawn_room, blob serving …
 → if json:  emit {"kind":"listening",…}, {"kind":"room",…}      (else: landed text header)
 → loop (unchanged select!):
     conn event   → if json: {"kind":"peer",…} + {"kind":"roster",…}   (else: text lines)
     tick:
       node.room_tail(limit) → for each unseen StoredEvent (ALL types, D4):
           decode SignedEvent → build TailRow (shared with offline, D5)
           if json: println!(one line, flushed)                        (else: messages-only text)
       node.logs() → new reject entries:
           stderr warning[…] (unchanged) + if json: {"kind":"warning",…}   (D6)
 → Ctrl-C → shutdown (unchanged)
```

The only structural change in `message.rs` is print-dispatch: one `enum` of emitters (text vs
NDJSON) selected once at startup, so the loop body stays single-sourced.

---

## 5. Detailed implementation steps

1. **`cli.rs`** — on `RoomAction::Tail`, change `json` to plain `#[arg(long)]` (drop
   `requires = "offline"`); update the doc comment: *"Offline: emit a single JSON array.
   Online: stream one JSON object per line (NDJSON) for every validated event and session
   transition."* Thread `json` into `message::tail`.

2. **Share the row builder.** Extract the offline `TailRow` construction
   (`room.rs:297`'s per-event block: decode → attribution → `content_fields`) into a
   `pub(crate)` helper (e.g. `display::tail_row(...) -> TailRow`) consumed by both
   `room::tail_offline` and the new online emitter. `TailRow` moves to `display.rs` (or is
   re-exported) — **fields unchanged**.

3. **`message.rs`** — add the NDJSON emitter path per D3–D6: serialize
   `#[derive(Serialize)] #[serde(tag = "kind", rename_all = "snake_case")]` session records;
   `{"kind":"event", …TailRow flattened…}` via `#[serde(flatten)]`; per-line
   `println!` + flush; remove the `MessageText` filter in this mode; refresh departure
   sets/display names on membership-event arrival (D5).

4. **Docs reconciliation** — `docs/getting-started.md` (a short "machine consumers" block
   under Step 4 with a two-line NDJSON sample), `README.md` landed-feature paragraph,
   `RELEASE-READINESS.md` known-limitations entry updated: the live-tail display gap is
   closed **for JSON consumers**; the text-mode gap remains (D7) unless UP-103 also lands.

---

## 6. CLI surface & output (reference — reconcile against the binary)

```text
iroh-rooms [--data-dir <PATH>] room tail <ROOM_ID> --json [--peer <ADDR>]... [--limit <N>] [--accept-joins] [-v]
```

Illustrative stream (one object per line; wrapped here for readability):

```json
{"kind":"room","room_id":"blake3:aa…"}
{"kind":"listening","addr":"ed25519:9f2c…@192.168.1.20:45001"}
{"kind":"peer","identity":"bob1a2b3c","device":"7f3a2c1b","state":"connected"}
{"kind":"roster","connected":1,"offline":0,"unauthorized":0}
{"kind":"event","event_id":"blake3:dd…","event_type":"message.text","lamport":3,
 "created_at":1751284864000,"at":"2026-06-30T12:01:04Z","from":"bob1a2b3c",
 "role":"member","status":"active","body":"preview ready?","format":"plain"}
{"kind":"event","event_id":"blake3:ee…","event_type":"agent.status","lamport":4,
 "created_at":1751284892000,"at":"2026-06-30T12:01:32Z","from":"agent7d2e",
 "role":"agent","status":"active","state":"running_tests",
 "message":"Running integration tests","progress":40}
{"kind":"peer","identity":"bob1a2b3c","device":"7f3a2c1b","state":"offline","reason":"link_dropped"}
{"kind":"roster","connected":0,"offline":1,"unauthorized":0}
{"kind":"warning","code":"bad_signature"}
```

Without `--json`, output is byte-identical to today. With `--offline --json`, the landed
single-array read is byte-identical to today.

---

## 7. Error & observability model

- **Pre-bring-up failures unchanged** (coded `error[<code>]:` on stderr, non-zero exit): no
  identity, unknown room, not a member, bad flags. `--offline` still conflicts with the
  online flags; `--json` alone no longer implies anything about offline.
- **Malformed-stdout is impossible by construction:** every stdout write in NDJSON mode is a
  single `serde_json::to_string` + newline; no interleaved human text (D6).
- **Stream truncation is honest:** Ctrl-C / SIGTERM ends the stream mid-life by design; a
  consumer treats EOF as session end, not error. Serialization failure (cannot occur for
  these values) would be a context-wrapped internal error.
- **Warnings are dual-surfaced** (stderr line + `"warning"` record, D6) — additive, never a
  replacement; `^error\[`/`^warning\[` grep contracts hold.

---

## 8. Security, privacy, reliability, performance

- **No new trust surface.** Same validated rows, same fold attribution, same session gates
  (membership requirement, admission, join-bootstrap) as the landed online tail. NDJSON is a
  re-rendering, not a new data source.
- **Secret hygiene.** Records carry only what the text mode already shows (public ids,
  states, event content); no ticket, no seed, no capability secret can enter the stream —
  asserted by tests (§9). `body`/`message` are peer-authored content, exactly as today.
- **§16.4 honesty preserved structurally:** `"peer"` records carry `reason` only when
  `state == "offline"`; an unauthorized peer can never serialize as offline (the record is
  built from the same pinned `PeerConnState` labels).
- **Performance.** Same poll cadence and `room_tail(limit)` reads as today; serialization of
  MVP-sized rows is negligible; per-line flush keeps consumer latency at one poll tick.

---

## 9. Test strategy

- **Unit (emitters):** each `kind` record serializes with expected fields; `reason` present
  iff offline; `TailRow` flatten produces the same keys as the offline row for every content
  type (table-driven over `Content::*` fixtures, incl. `agent.status` with
  progress/artifacts and `file.shared`).
- **Schema-equality (the headline contract):** author one event of each type into a temp DB;
  render via `room tail --offline --json` (array) and via the online emitter's row builder;
  assert the shared field sets are identical key-by-key (guards against fork drift).
- **CLI (deterministic tier):** `--offline --json` byte-identical to pre-change goldens;
  text mode byte-identical (run existing `tail_cli.rs` unchanged); `--json` with
  `--offline`-conflicting flags still rejected.
- **Loopback e2e (`#[ignore]`-gated, mirrors `two_peer_e2e.rs` harness):** admin tails with
  `--json`; a second participant joins, sends a message, posts an `agent status --progress
  40`, shares a file. Assert the NDJSON stream contains, in arrival order: `"listening"`,
  `"room"`, a `"peer"`/`"roster"` pair, and `"event"` records for `member.joined`,
  `message.text`, `agent.status` (with `progress:40`), `file.shared` — each parseable
  line-by-line; no non-JSON bytes on stdout; no secret material in any line.
- **Warning mirror:** drive an invalid frame at the session (reuse `malformed_cbor_e2e`
  fixtures at the Node layer or the existing reject harness); assert the stderr
  `warning[<code>]` line **and** the `"warning"` record both appear.

---

## 10. Acceptance criteria → evidence

| # | Criterion | Satisfied by | Test |
|---|---|---|---|
| AC1 | Online `--json` streams one JSON object per line | D2 framing | §9 e2e (line-parse loop) |
| AC2 | All validated event types stream live | D4 (filter removed) | §9 e2e (four event kinds) |
| AC3 | Event records schema-match the offline `TailRow` | D3 + shared builder (Step 2) | §9 schema-equality |
| AC4 | Session facts stream structurally (listening/peer/roster/warning) | D3/D6 | §9 e2e + warning mirror |
| AC5 | Text mode and offline `--json` byte-identical when unchanged | D1/D7 + single-sourced loop | §9 CLI goldens |
| AC6 | No secret material on the stream | D6 + §8 hygiene | §9 e2e assertion |

---

## 11. Risks

- **R1 — Regression of the pinned text surface.** *Mitigation:* mode selected once; text
  path untouched; existing `tail_cli.rs`/e2e goldens run unchanged as the tripwire.
- **R2 — Schema fork between offline array rows and live records.** *Mitigation:* one shared
  row builder (Step 2) + the §9 schema-equality test — drift fails CI.
- **R3 — Consumers treat stream order as canonical order.** *Mitigation:* records carry
  `lamport`/`event_id`; docs state emission is arrival order and position carries no trust
  (spike §2.4).
- **R4 — Dual-surface warnings double-count.** *Mitigation:* documented as mirrors of one
  occurrence; the record carries only `code`, matching the stderr line 1:1.
- **R5 — stdout blocking on a slow consumer stalls the display loop.** *Mitigation:* same
  synchronous `println!` posture as today's text mode; the display loop is already advisory
  (the engine/pump own correctness). Document that a consumer must keep reading.

---

## 12. Open questions

- **OQ-1 — Also close the text-mode gap here (UP-103)?** Rendering all event types as text
  lines would change pinned human output the demo docs show. *Recommendation:* keep separate
  (D7); land NDJSON first, decide text rendering with the docs refresh. **Needs a scope call.**
- **OQ-2 — Mark historical backlog rows (`"historical":true`)?** *Recommendation:* no —
  consumers dedupe by `event_id` regardless (reconnects make dedupe mandatory anyway);
  additive field can come later without breaking readers. **Non-blocking.**
- **OQ-3 — Structured `diag:` in the stream when `--verbose`?** *Recommendation:* no — keep
  diagnostics stderr-only; structured diagnostics are their own follow-up (UP-105).
  **Non-blocking.**
- **OQ-4 — `kind` values as a pinned tooling contract?** *Recommendation:* yes — document the
  six kinds and their required fields in the getting-started block; additive-only evolution
  (new kinds may appear; consumers must skip unknown kinds). **Needs a docs call.**

---

## 13. Out of scope

- Text-mode rendering of non-message events (UP-103; D7).
- Structured diagnostics / counters (`diag:` JSON, pipe/blob/sync metrics — UP-105/UP-106 in
  `docs/cockpit-backlog.md`).
- Any change to `Node`, `SyncEngine`, the event schema, or the offline read contract.
- Follow/stream semantics for the offline reader (it remains a one-shot snapshot).
- Backpressure machinery beyond the documented keep-reading contract (R5).

---

## 14. File-change summary

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/src/cli.rs` | drop `requires = "offline"` on `Tail.json`; doc-comment update; thread `json` into `message::tail` |
| `crates/iroh-rooms-cli/src/display.rs` | **add** shared `tail_row` builder (moved from `room.rs` per-event block); `TailRow` home (fields unchanged) |
| `crates/iroh-rooms-cli/src/room.rs` | `tail_offline` consumes the shared builder (no output change) |
| `crates/iroh-rooms-cli/src/message.rs` | NDJSON emitter enum + session-record serde types; remove `MessageText` filter in JSON mode; membership-refresh for attribution (D5); warning mirror |
| `crates/iroh-rooms-cli/tests/tail_cli.rs` | schema-equality + goldens-unchanged cases |
| `crates/iroh-rooms-cli/tests/tail_stream_e2e.rs` | **new** — `#[ignore]`-gated loopback NDJSON e2e (§9) |
| `docs/getting-started.md` | "machine consumers" NDJSON block under Step 4 |
| `README.md` | landed-feature paragraph (house style) once implemented |
| `RELEASE-READINESS.md` | live-tail limitation entry updated (closed for JSON consumers; text gap remains unless UP-103 lands) |
