# Spec: Harden CLI error handling and diagnostics (`iroh-rooms`)

Issue: **#38 / IR-0303** — Harden CLI error handling and diagnostics.
Parent epic: **#4**. Labels: `type/feature area/cli area/dx priority/p1 risk/medium`.

Traceability:
- PRD `PRD.v0.3.md` **§16** (CLI UX requirement 1: *"Commands should print actionable
  next steps"*; requirement 3: distinguish failure states; requirement 5: script-friendly
  output), **§17.2** (DX metrics — a developer can *"complete the full demo from docs
  without maintainer help"*: self-debugging), **§18.1** (P2P Reliability Risk mitigation:
  *"Clear connection state"* + *"Network diagnostics"*), **§18.5** (UX Risk mitigation:
  *"Clear CLI outputs"* + *"Hide networking details unless needed"*).
- PRD `PRD.v0.3.md` **§19 Phase 2 — Developer Preview**, deliverable 4 (*"Better error
  handling"*).
- Spike `PHASE-0-SPIKE.md` **§8** (Rejection / Flag Taxonomy — the stable reason codes).

Dependencies (both landed):
- **#25 / IR-0110** — CLI error taxonomy (`ErrorCode`/`ErrorCategory`/`CliError`,
  `error[<code>]:` render contract, category→exit scheme, `StderrAudit`, the README
  **Error codes** table, and the `docs_conformance.rs` code gate). See spec
  `specs/cli-error-taxonomy-for-critical-failures.md`.
- **#35 / IR-0210** — `docs/getting-started.md` (the Troubleshooting section with the four
  documented failure modes, each ending in a **Next action**).

> This is a **planning/spec document only**. No production code is written by this issue's
> planning phase. The implementation steps in §6 are for the executing engineer/agent.

---

## 1. Summary

The failure *taxonomy* landed with #25/IR-0110 (and was extended by #29/IR-0204 and
#30/IR-0205 for the file-fetch states): every terminal command failure already renders a
stable, machine-parseable `error[<code>]: <message>` line on stderr with a category exit
code, and every accepted-but-flagged receive-path event renders `warning[<code>]: …`. That
surface is correct and script-friendly. What IR-0303 hardens is the **human** half of the
same surface — the parts PRD §16/§17.2/§18 ask for but the taxonomy issue explicitly left
as a long tail:

1. **Actionable next steps everywhere (AC1).** Today the *next step* is present on the
   handful of failures IR-0205 wrote carefully (`no_admin_reachable`, `blob_unavailable`,
   the fetch splits) but **absent or bare on the majority** — four of six ticket-decode
   reasons, all three join rejections (`bad_capability`/`expired_invite`/
   `insufficient_role`), every "not an active member" wall, `peer_offline` on `pipe
   connect`, `no_such_file`/`permission_denied`/`file_too_large` on `file share`, and one of
   the two `room_not_found` sites. The next-step text is also **stylistically
   inconsistent** (some `;`-joined commands, some rhetorical questions, some bare) and
   sometimes **contradicts itself** (`room_not_found` has a hint via `fold_room` but not via
   `room::members`). IR-0303 makes a concrete, secret-free next step a **first-class,
   pinned property of each `ErrorCode`**, rendered uniformly.

2. **An error-code reference (AC2/AC4).** Turn the existing README **Error codes** table
   into a complete *reference* — every code, its category, its exit code, its meaning,
   **and its next action** — kept in lockstep with the emitter by the docs-conformance
   gate. No change to the machine surface (the `error[<code>]:` line and the exit code stay
   byte-for-byte stable, AC2).

3. **Optional verbose network diagnostics (AC1 self-debug / §18.1 / §18.5).** Add a
   `--verbose` mode to the connection-state commands that, *only when asked*, surfaces the
   network facts a developer needs to self-diagnose a P2P failure: local dialable address +
   **relay URL**, and per-peer **path classification** (direct / relay / mixed / none) read
   from iroh's `remote_info` active-address set. Hidden by default (§18.5 "hide networking
   details unless needed"); rendered on stderr so scripting output stays clean.

4. **Secret hygiene under the new surface (AC3).** The verbose dump and the new next-step
   text must never widen the existing secret-free guarantee: no private-key seed, no ticket
   token/capability secret, no message payload. This is enforced by the existing
   non-printable/`<redacted>` types plus a dedicated no-leak test over the verbose output.

The net code change is additive and CLI-centric: a `next_action()` method on the existing
`ErrorCode`, a second render line in `main.rs`, a small `PathType`/`remote_info`
classification ported from the `spike-nat` reference into `iroh-rooms-net`, a `--verbose`
flag on the status-bearing commands, and docs + conformance updates. **No protocol, schema,
gate, authorization, or exit-code-scheme change.**

---

## 2. Background & current repository state

### 2.1 What already landed (do not rebuild)

- **`crates/iroh-rooms-cli/src/error.rs`** — `ErrorCode` (`error.rs:33`, `#[non_exhaustive]`,
  wraps `RejectReason`/`TicketError`/`OfflineReason`), `ErrorCode::code()` (`:92`, stable
  `&'static str`), `category()`/`exit_code()` (`:116`/`:139`), `ErrorCategory` (`:160`,
  Internal=1 … Connectivity=6), `CliError::new(code, message)` (`:219`), `CodedResultExt`
  `.coded()`/`.with_coded()` (`:239`), `bail_coded!` (`:268`), `code_of()` (`:277`, walks the
  anyhow chain). Every code + category is pinned by unit tests (`error.rs:293`, `:380`).
- **`crates/iroh-rooms-cli/src/main.rs:32-37`** — the single terminal render point: coded →
  `eprintln!("error[{}]: {err:#}", code.code())` + `ExitCode::from(code.exit_code())`;
  uncoded → `eprintln!("error: {err:#}")` + exit 1. **No next-step line today.**
- **`crates/iroh-rooms-cli/src/audit.rs`** — `StderrAudit` (`:22`) renders `warning[<code>]:`
  advisories, `note:` lines, and blob-serve audit lines directly to stderr (the CLI installs
  **no** `tracing` subscriber — project memory *CLI has no tracing subscriber*).
- **`crates/iroh-rooms-cli/src/message.rs`** — the live connection panel:
  `members_status()` (`:562`) / `print_members_status()` (`:638`) print
  `member: <id> role=<r> status=<s> conn=<conn>` (`:658`), where `conn` comes from
  `member_conn_field()` (`:672`: `self` / `n/a` / `offline reason=<reason>` / state label);
  `format_peer_line()` (`:1013`: `peer <id> device=<dev> state=<label>[ reason=<reason>]`),
  `roster_summary()` (`:1031`: `peers: N connected, M offline, K unauthorized`),
  `render_endpoint_addr()` (`:1145`: `<id>@<ip:port>,…` — **drops the relay URL today**).
- **README `### Error codes`** (`README.md:688`) — the category→exit table + the render
  contract + the wrap-don't-duplicate note. **No per-code next-action column.**
- **`docs/getting-started.md` `## Troubleshooting`** (`:869`) — four modes (offline peer,
  unauthorized peer, invalid ticket, unavailable file) plus adjacent cases, each with a
  **Next action**, and the "Stable error/warning lines and exit codes" subsection.
- **`crates/iroh-rooms-cli/tests/docs_conformance.rs`** — the docs gate:
  `all_four_failure_modes_are_documented` (`:100`), `failure_modes_have_next_actions`
  (`:119`, asserts ≥4 "next action" occurrences), `readme_documents_every_error_code`
  (`:1005`, over `ALL_ERROR_CODES` at `:930`), `readme_documents_every_exit_category`
  (`:1022`). Unit pins live in `error.rs`/`ticket.rs`.

### 2.2 The next-step gap inventory (AC1 target)

From an exhaustive sweep of the CLI message sites, the failures that currently give the user
**no actionable next step** (the AC1 work list):

| Failure | Site | Current text (abridged) | Gap |
| --- | --- | --- | --- |
| `ticket_bad_base32` | `core/ticket.rs:76` | "invite ticket is not valid base32" | no fix |
| `ticket_truncated` | `core/ticket.rs:78` | "invite ticket is truncated" | no fix |
| `ticket_unsupported_version` | `core/ticket.rs:80` | "unsupported invite-ticket version {v}" | no fix |
| `ticket_malformed` | `core/ticket.rs:84` | "invite ticket body is malformed" | no fix |
| `bad_capability` | `join.rs:419` | "…does not match the invite (bad_capability)" | no fix |
| `expired_invite` | `join.rs:421` | "this invite has expired (expired_invite)" | no fix |
| `insufficient_role` | `join.rs:423` | "the ticket's role does not match the invite …" | no fix |
| `not_a_member` walls | `message.rs:123`,`:272`,`:437`,`:580`; `file.rs:136`,`:496`; `pipe.rs:102`,`:240`,`:381`,`:392` | "you are not an active member of room … (this identity is …)" | states rule, no action |
| `peer_offline` (pipe) | `pipe.rs:305` | "the pipe owner is unreachable: …" | no fix (contrast `no_admin_reachable`) |
| `no_such_file` (share) | `file.rs:923`,`:965` | "no such file: {path}" | no fix |
| `permission_denied` | `file.rs:926`,`:958` | "permission denied reading {path}" | no fix |
| `file_too_large` | `file.rs:947` | "{path} is {len} bytes; exceeds the MVP share limit …" | no fix |
| `room_not_found` (3 sites, same code) | `room.rs:158`, `invite.rs:103`, `message.rs:892` | three *different* messages for one `ErrorCode::RoomNotFound`: `room.rs` bare "no room {} in {}"; `invite.rs` "…; run `iroh-rooms room create` first"; `message.rs` (`fold_room`) "…; run `iroh-rooms room create` or join an invite first" | **inconsistent** — same code, three next-step strings (one absent). Trimming the message + moving the step to `next_action()` collapses all three to one rendering. |
| `invalid_room_id` | `cli.rs:683` | "invalid room id (expected `blake3:<hex>`)" | format hint only, no command |

Failures that **already** carry a good next step (keep, but normalize the *phrasing* into
the new mechanism): `identity_not_found` (`identity.rs:140`), `wrong_identity`
(`join.rs:121`), `no_discovery_hint` (`join.rs:134`), `no_admin_reachable` (`join.rs:382`),
`blob_unavailable`/`peer_unauthorized`/`no_such_file`-on-fetch (`file.rs:564`/`:631`/`:541`),
`room_not_found`-via-`fold_room` (`message.rs:894`).

**One uncoded input-validation site (AC2 uniformity gap, not AC1).** `file fetch`'s
`--timeout` is parsed at `cli.rs:538` with a bare `?` — **missing** the
`.coded(ErrorCode::InvalidArgument)` that its four sibling timeout sites carry
(`cli.rs:479`/`:581`/`:617`/`:665`). A malformed `file fetch --timeout` therefore renders the
uncoded `error:` line and exits `1` instead of `error[invalid_argument]:` / exit `2`, breaking
the "every bad argument is `invalid_argument`/exit 2" contract scripts rely on. IR-0303 adds
the missing `.coded(ErrorCode::InvalidArgument)` (one-line fix, Step 3) and a regression
assertion so all five timeout sites are uniform.

### 2.3 The network-diagnostics surface (AC "verbose diagnostics" target)

- **State is queryable but under-surfaced.** `Node` exposes `peer_entries()`
  (`node.rs:476` → `PeerEntry { state, identity, offline_reason, last_change_ms }`),
  `peer_states()`, `conn_events()` (`:482`), `endpoint_addr()` (`:446`), `logs()` (`:565`,
  the no-subscriber `reject.<code>`/`flag.<code>` ring), and — crucially — `endpoint()`
  (`:792`, the raw iroh `Endpoint`). `PeerConnState::label()`/`OfflineReason::label()`
  (`state.rs:38`/`:77`) are the pinned strings tooling greps.
- **Direct-vs-relay classification does NOT exist in production.** There is no
  `remote_info` call, no `ConnectionType`/`RemoteInfo` usage, and no relay URL rendered
  anywhere in `iroh-rooms-net`/`-cli`. The **reference implementation exists only in the
  `spike-nat` crate** (not a dependency): `classify_remote_info(info: Option<&RemoteInfo>)
  -> (PathType, Option<String>)` (`crates/spike-nat/src/probe.rs:464`) iterates
  `info.addrs()`, tests `TransportAddrUsage::Active`, and maps `TransportAddr::Ip` ⇒ direct
  / `TransportAddr::Relay(url)` ⇒ relay; `PathType { Direct, Relay, Mixed, None }` with
  `label()` and `is_hole_punched()` (`crates/spike-nat/src/report.rs:62`). Project memory
  *iroh 1.0.1 has no ConnectionType watcher* confirms this is the only correct path in this
  iroh version.
- **The only verbosity precedent** is `pipe expose -v` (`cli.rs:209`), which gates
  per-connection accept chatter on stderr while stdout stays script-clean. IR-0303's
  `--verbose` mirrors that pattern.

### 2.4 Secret hygiene already in place (AC3 foundation)

- `SecretKeys` (`identity.rs:97`) has **no** `Debug`/`Display`/`Serialize`; `SecretFile`
  (`:108`) is `Zeroize` with no `Debug`; seeds live in `Zeroizing`; `SigningKey::Debug`
  prints `SigningKey(<redacted>)` (`core/event/keys.rs:292`).
- `RoomInviteTicket::Debug` masks `capability_secret` as `<redacted>` (`core/ticket.rs:262`)
  — but its **`Display` emits the full `roomtkt1…` token** (`:216`), so a diagnostic dump
  must **never** call `Display`/print a ticket. `TicketError`/`OfflineReason`/`PeerConnState`
  are all safe to render.
- Public-key newtypes `IdentityKey`/`DeviceKey`/`EndpointId` render hex — safe (public).

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

Make common CLI failures self-debuggable by an early developer without maintainer help
(§17.2): every common failure ends with a concrete, secret-free next step; the error-code
set is documented as a complete reference; and a `--verbose` mode exposes the network facts
(direct/relay path, relay URL, dialable address) needed to diagnose P2P reachability —
without ever leaking a secret and without changing the script-facing machine surface.

### 3.2 In scope

1. **`ErrorCode::next_action()` (AC1).** A pinned, secret-free `&'static str` next step per
   common code, rendered by `main.rs` as a second `next: …` line under `error[<code>]:`.
   Fill the §2.2 gaps and normalize the already-good hints into this one mechanism; fix the
   `room_not_found` inconsistency.
2. **Error-code reference (AC2/AC4).** Extend the README **Error codes** table with a
   *next action* column and a short per-code reference; keep the machine surface (code +
   exit) unchanged. Extend `docs_conformance.rs` to gate "every common code has a next
   action, documented".
3. **Verbose network diagnostics (§18.1/§18.5).** A `--verbose` flag on the connection-state
   commands (primary anchor: `room members --status --verbose`; also `room tail --verbose`)
   that appends a stderr diagnostics block: local id + dialable addrs + relay URL, and one
   line per peer with `state=`, `path=<direct|relay|mixed|none>`, and `relay=<url|none>`,
   read from `Endpoint::remote_info`. A ported `PathType` + `classify_remote_info` in
   `iroh-rooms-net`, and a `Node` accessor for per-peer path + local relay URL.
4. **Secret-hygiene enforcement (AC3).** A no-leak test asserting the verbose output and the
   next-step lines never contain a seed, a ticket token, or a capability secret; render only
   public keys, states, reasons, addresses, and relay URLs.
5. **Docs failure examples (AC4).** Extend the getting-started Troubleshooting section with
   (a) the two-line `error[…] / next:` shape on every example, (b) a "Verbose network
   diagnostics" subsection with sample output and a redaction note, and (c) the reference
   table. Keep the existing docs-conformance gates green.

### 3.3 Out of scope / non-goals (explicit)

- **No change to the taxonomy contract.** `ErrorCode::code()`, `category()`, `exit_code()`,
  the `error[<code>]:`/`warning[<code>]:` render prefixes, and the category→exit numbers
  stay byte-for-byte stable (AC2). IR-0303 only *adds* a `next:` line and a `--verbose`
  block.
- **No new protocol, schema, gate, or authorization behaviour.** Path classification is a
  read of iroh's transport state; it is diagnostic only and is **never** a trust input
  (mirrors the `OfflineReason` "no trust from labels" rule).
- **No `--json` error envelope.** Deferred to a later DX issue (was #25 OQ-5); the machine
  surface remains the `error[<code>]:` line + exit code. The verbose block is human-facing
  stderr text, not a stable JSON contract (flagged OQ-5).
- **No standalone `doctor` / room-less connectivity self-test in this issue.** A `--verbose`
  extension of the existing status commands is the MVP; a dedicated `iroh-rooms doctor`
  (relay reachability, STUN, no room required) is proposed as OQ-3 / a follow-up.
- **No global default-verbose or new `tracing` subscriber.** §18.5: networking details stay
  hidden unless `--verbose` is passed.
- **No latency/RTT-based inference.** Path type is read from the `remote_info` active-address
  set only (project memory *iroh 1.0.1 has no ConnectionType watcher*); RTT may be shown as
  a raw datum if available but is never used to *infer* direct vs relay.

---

## 4. Placement & dependencies

| Change | Crate / file | Kind |
| --- | --- | --- |
| `ErrorCode::next_action()` + pins | `iroh-rooms-cli/src/error.rs` | additive |
| `next:` render line | `iroh-rooms-cli/src/main.rs` | edit (one arm) |
| Fill next-step gaps / normalize hints / fix `room_not_found` | `iroh-rooms-cli/src/{join,file,pipe,message,room,cli,identity}.rs`, `iroh-rooms-core/src/ticket.rs` (only if ticket next-steps are surfaced CLI-side, see §5.1) | edits |
| `PathType` + `classify_remote_info` (ported from spike-nat) | **new** `iroh-rooms-net/src/diag.rs` (or `state.rs`) | additive |
| `Node::peer_paths()` / `Node::relay_url()` accessors | `iroh-rooms-net/src/node.rs` (+ `transport.rs`) | additive |
| `--verbose` flag + diagnostics block render | `iroh-rooms-cli/src/{cli,message}.rs` | edits |
| README **Error codes** reference (next-action column) + verbose docs | `README.md`, `docs/getting-started.md` | docs |
| Tests | `iroh-rooms-cli/tests/error_taxonomy.rs`, `.../docs_conformance.rs`, a new `.../diagnostics_cli.rs`, `iroh-rooms-net` unit tests | tests |

`next_action()` adds **no** external crate. The diagnostics classification depends on the
iroh types already used by `iroh-rooms-net` (`Endpoint`, and the `RemoteInfo` /
`TransportAddr` / `TransportAddrUsage` surface exercised by `spike-nat`). **`spike-nat` and
`iroh-rooms-net` pin the exact same iroh version — `iroh = "=1.0.1"`** (`spike-nat/Cargo.toml`
and `iroh-rooms-net/Cargo.toml`), so those types are guaranteed reachable and the port is a
*mechanical copy*, not a version-adaptation. (`RemoteInfo`/`TransportAddrUsage` come from
`iroh::endpoint`; `TransportAddr` from `iroh` — see `spike-nat/src/probe.rs:24-28`.)

---

## 5. Design

### 5.1 Actionable next steps (`ErrorCode::next_action`) — AC1

Add one method to the existing enum; do not change `code()`/`category()`/`exit_code()`:

```rust
// crates/iroh-rooms-cli/src/error.rs
impl ErrorCode {
    /// A stable, secret-free next step for a human — the "what do I do now" line
    /// (spec IR-0303 §5.1). `None` for codes where the call-site message already
    /// carries all the context there is (e.g. `internal`), or where no generic
    /// action applies. Must never interpolate a secret; the string is a fixed
    /// template, so runtime detail (paths, ids) stays in the `CliError` message.
    #[must_use]
    pub fn next_action(&self) -> Option<&'static str> {
        match self {
            Self::IdentityNotFound => Some("run `iroh-rooms identity create --name <name>` first"),
            Self::InvalidRoomId    => Some("copy the room id from `room create` / `room members` (form `blake3:<hex>`)"),
            Self::RoomNotFound     => Some("run `iroh-rooms room create <name>`, or join an invite ticket first"),
            Self::NoSuchFile       => Some("check the path; share a single existing file"),
            Self::PermissionDenied => Some("check the file's read permissions, or share a copy you can read"),
            Self::FileTooLarge     => Some("the MVP share limit is fixed; split or compress the file"),
            Self::NoDiscoveryHint  => Some("pass `--peer <admin-addr>` (the ticket carried no discovery hint)"),
            Self::NoAdminReachable => Some("ask the admin to run `room tail <ROOM_ID> --accept-joins`, then retry; or pass `--peer <admin-addr>`"),
            Self::PeerOffline(_)   => Some("ask the owner to come online (run `room tail <ROOM_ID>`), then retry; or pass `--peer <owner-addr>`"),
            Self::PeerUnauthorized => Some("ask the admin to confirm your membership has synced, then retry"),
            Self::WrongIdentity    => Some("ask the admin to re-issue the invite for your identity id (`identity show`)"),
            Self::BlobUnavailable  => Some("ask a peer that holds the file to run `room tail <ROOM_ID>`, then retry `file fetch`"),
            Self::HashMismatch     => Some("do not trust this file; the reference or a provider may be corrupt — ask for a fresh `file share`"),
            Self::Ticket(_)        => Some("check the whole ticket was copied (no truncation/whitespace); if it persists, ask the admin for a fresh `room invite`"),
            Self::Reject(r)        => reject_next_action(*r), // see below
            Self::InvalidArgument | Self::Internal => None,   // context is in the message
        }
    }
}

fn reject_next_action(r: RejectReason) -> Option<&'static str> {
    match r {
        RejectReason::ExpiredInvite   => Some("ask the admin for a fresh `room invite` (optionally with a longer `--expires`)"),
        RejectReason::BadCapability   => Some("ask the admin to re-issue the invite for your identity id"),
        RejectReason::InsufficientRole=> Some("ask the admin to invite you with the intended role"),
        RejectReason::NotAMember
        | RejectReason::UnboundDevice => Some("ask the admin to invite you and complete `room join` first"),
        _ => None, // structural/crypto rejects are receive-path advisories, not user-fixable
    }
}
```

**Render contract (pinned by tests), extends #25 §5.2:**

```rust
// main.rs — the Err arm
if let Some(code) = error::code_of(&err) {
    eprintln!("error[{}]: {err:#}", code.code());
    if let Some(next) = code.next_action() {
        eprintln!("next: {next}");           // second stderr line; script surface unchanged
    }
    ExitCode::from(code.exit_code())
} else {
    eprintln!("error: {err:#}");
    ExitCode::FAILURE
}
```

- The `next: …` line is **stderr**, on its own line, after `error[<code>]:`. Scripts that
  match `^error\[<code>\]` or branch on `$?` are unaffected (AC2). Only one `next:` line is
  ever emitted (the code-level action), removing the current duplication risk.
- **Message vs action split (the migration rule):** the `CliError` *message* states **what**
  failed and any runtime context (paths, ids, the offline `reason`); `next_action()` states
  **what to do**. Where a call-site message currently inlines a generic next step (e.g.
  `no_admin_reachable`, the IR-0205 fetch messages), **move that step into `next_action()`**
  and trim the message to the failure + context, so the surface is uniform. Context that
  cannot be a fixed template (a concrete `--peer <resolved-addr>`) stays in the message; the
  generic complement lives in `next_action()`.
- **`room_not_found` fix:** all three sites (`room.rs:158`, `invite.rs:103`, `message.rs:892`)
  already emit the same `ErrorCode::RoomNotFound` via `bail_coded!` but carry three *different*
  next-step strings (one bare). Trim each message to the failure + context ("no room {} in {}")
  and let the single `RoomNotFound.next_action()` supply the step, so all three render
  identically — killing the §2.2 inconsistency without touching the code or exit.
- **Secret-free by construction:** `next_action()` returns fixed `&'static str` templates —
  no interpolation — so it structurally cannot leak. Pinned by a unit test.

Why a method on `ErrorCode` rather than per-call-site prose: it makes "does this failure
tell the user what to do?" a **testable, centralized invariant** (a unit test asserts every
*user-actionable* code returns `Some`), it guarantees one consistent voice, and it lets the
docs-conformance gate prove the README reference matches the emitter.

### 5.2 Error-code reference (AC2/AC4)

- Extend the README **Error codes** table (`README.md:698`) — keep the category→exit rows,
  and add a compact per-code reference block listing **code · category · exit · meaning ·
  next action**, drawn from `next_action()`. The `error[<code>]:` render contract and the
  exit numbers are documented verbatim (AC2: unchanged machine surface).
- The reference is the human counterpart to the `ALL_ERROR_CODES` array
  (`docs_conformance.rs:930`). Add a gate: for every code that is *user-actionable* (the
  `next_action()` = `Some` set), the README reference names a next action; and every code in
  `ALL_ERROR_CODES` still has a table row (the existing `readme_documents_every_error_code`
  stays green).

### 5.3 Verbose network diagnostics (§18.1 / §18.5)

**Flag surface.** Add `--verbose` (`-v`) to the connection-state commands, mirroring `pipe
expose -v`:
- `room members <ROOM_ID> --status --verbose` — the **primary** anchor (it already brings up
  a node and prints the connection panel; §2.3).
- `room tail <ROOM_ID> --verbose` — appends the diagnostics block to the live panel.

Default (no `--verbose`): output is exactly as today (§18.5 hide-unless-needed). `--verbose`
is stderr-only; stdout stays script-clean (AC2).

**Net-side classification (ported from `spike-nat`).** Add to `iroh-rooms-net`:

```rust
// crates/iroh-rooms-net/src/diag.rs  (or state.rs)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathType { Direct, Relay, Mixed, None }
impl PathType {
    pub fn label(self) -> &'static str { /* "direct"|"relay"|"mixed"|"none" */ }
    // Port verbatim from spike-nat `report.rs:62-93`: only a *pure* direct path counts as
    // hole-punched (`Mixed` still has an active relay path, so the source treats it as not
    // yet fully hole-punched). `label()` — not this helper — is what the `diag:` block renders.
    pub fn is_hole_punched(self) -> bool { matches!(self, Self::Direct) }
}

/// Classify a peer's live path from iroh's *active* transport-address set — never
/// inferred from latency (iroh 1.0.1 has no ConnectionType watcher). Ported from
/// spike-nat `classify_remote_info` (probe.rs:464 / report.rs:62).
pub fn classify_remote_info(info: Option<&RemoteInfo>) -> (PathType, Option<String>);
```

Add `Node` accessors (and mirror on `NetTransport`):
- `async fn peer_paths(&self) -> Vec<(EndpointId, PathType, Option<String>)>` — for each
  peer in `peer_entries()`, call `self.endpoint().remote_info(id).await` and classify.
- `fn relay_url(&self) -> Option<String>` — the node's home relay, from the local
  `endpoint_addr()`/endpoint (so `render_endpoint_addr` can finally surface `relay=`).

**Render (stable, greppable, stderr).** After the existing panel, under `--verbose`:

```text
diag: local id=<endpoint_id> direct=<ip:port,…|none> relay=<url|none>
diag: peer <short_id> device=<short> state=connected path=direct relay=none
diag: peer <short_id> device=<short> state=connected path=relay  relay=<url>
diag: transport connected=2 (direct=1 relay=1 mixed=0) offline=0 unauthorized=0
```

- `path=` uses `PathType::label()`; `state=` reuses `PeerConnState::label()`. The block is
  additive to (not a replacement for) the pinned `member:`/`peer …`/`peers:` lines.
- An `offline`/`unauthorized` peer shows `path=none` (no active transport) — consistent with
  the honesty rule (never render an unauthorized peer as reachable).
- Lines are prefixed `diag:` so they are trivially grep-separable from the panel and from
  `error[`/`warning[`.

### 5.4 Secret hygiene in diagnostics (AC3)

- The diagnostics block renders **only**: `EndpointId`/short device (public), `IdentityKey`
  (public), `PeerConnState`/`OfflineReason` labels, IP socket addrs, `PathType` label, and
  relay URLs. It **never** touches `SecretKeys`, a `SigningKey` seed, a `RoomInviteTicket`
  (whose `Display` would emit the token), or message bodies.
- `next_action()` strings are fixed templates (no interpolation) — structurally secret-free.
- Enforced by a dedicated test (§8 #7): construct a session with a known secret seed and a
  known ticket secret, run `--status --verbose` and a coded ticket failure, and assert the
  seed hex and the ticket token base32 substring appear **nowhere** on stdout or stderr.
- The single render points (main.rs for `error`/`next`, the diagnostics helper for `diag:`)
  keep the guarantee auditable — no scattered `println!` leaking state.

### 5.5 Docs (AC4)

- `docs/getting-started.md` Troubleshooting: show every failure example in the two-line
  `error[<code>]: …` / `next: …` shape; keep the four **Next action** blocks (the gate at
  `docs_conformance.rs:119` requires ≥4). Add a **Verbose network diagnostics** subsection
  under Troubleshooting: how to run `room members --status --verbose`, annotated sample
  output, what `path=direct|relay|mixed|none` means for reachability (relay = it works but is
  slower / behind NAT; direct = hole-punched), and an explicit note that verbose output
  contains **no secrets**.
- README: the reference table (§5.2) + a one-line pointer to the verbose mode.

---

## 6. Implementation steps

Ordered so each step compiles and is independently testable.

### Step 1 — CLI: `ErrorCode::next_action()` + pins
- Add `next_action()` (§5.1) and `reject_next_action()` to `error.rs`. Return fixed
  `&'static str` templates only.
- Unit tests: (a) every *user-actionable* code returns `Some(non_empty)`; (b) `Internal` and
  `InvalidArgument` return `None`; (c) a "no next_action string contains a base32/hex
  secret-looking token" guard (belt-and-suspenders; they are literals).

### Step 2 — CLI: render the `next:` line
- Edit the `main.rs` `Err` arm per §5.1. Keep `{err:#}`.
- Integration assertion (in `error_taxonomy.rs`): a coded failure prints `error[<code>]:`
  **and** a following `next:` line; the exit code is unchanged; `error[` still starts the
  first stderr line so existing grep contracts hold.

### Step 3 — CLI: fill gaps, normalize hints, fix `room_not_found`
- Trim the IR-0205/join messages that inline a generic next step so the step now comes from
  `next_action()` (avoid the double line); keep runtime context (ids/paths/`--peer`) in the
  message.
- Point `room::members` (`room.rs:158`) at the shared coded `room_not_found` path
  (`message.rs:894` style) so both sites match.
- Ticket next steps: prefer surfacing them via `ErrorCode::Ticket(_).next_action()`
  (CLI-side, no core change). Do **not** edit `core/ticket.rs` `Display` (kept redacted and
  stable) unless a core change is explicitly chosen (OQ-4).
- Confirm the "not an active member" walls now end with the `not_a_member`/`unbound_device`
  next action.
- **Close the one uncoded input-validation site (AC2):** add `.coded(ErrorCode::InvalidArgument)`
  to `file fetch`'s timeout parse at `cli.rs:538` so a bad `--timeout` there emits
  `error[invalid_argument]:` / exit `2` like the other four timeout sites, instead of the
  uncoded `error:` / exit `1` it produces today.

### Step 4 — Net: port `PathType` + `classify_remote_info`; add accessors
- New `iroh-rooms-net/src/diag.rs`: `PathType` (+ `label`/`is_hole_punched`) and
  `classify_remote_info` ported from `spike-nat` (`probe.rs:464-496`, `report.rs:62-93`).
  Re-export from `lib.rs`. Both crates pin `iroh = "=1.0.1"`, so this is a **verbatim copy**
  (`RemoteInfo`/`TransportAddrUsage` from `iroh::endpoint`, `TransportAddr` from `iroh`) — no
  match-arm adaptation needed. Make the copied items `pub` (the spike's are `pub(crate)`).
- `Node::peer_paths()` and `Node::relay_url()` (§5.3), mirrored on `NetTransport`
  (endpoint access already exists: `NetTransport::endpoint()` `transport.rs:298`,
  `Node::endpoint()` `node.rs:791`). Keep them cheap and off any hot path.
- **Settle nuance:** shipping `RealNetwork` binds *without* calling `.online()` (unlike
  `spike-nat`), so `endpoint().remote_info(id)` and the home-relay URL can read `None` until a
  link settles. `members_status` already waits for links to settle before printing (§6), so
  the diagnostics read happens post-settle; still, render `path=none`/`relay=none` honestly
  when `remote_info` is `None` rather than blocking.
- Net unit tests: `PathType::label`/`is_hole_punched` pinned; `classify_remote_info` over a
  synthetic `RemoteInfo` (direct-only ⇒ Direct, relay-only ⇒ Relay, both ⇒ Mixed, empty ⇒
  None) — mirrors the spike's tests.

### Step 5 — CLI: `--verbose` flag + diagnostics block
- Add `--verbose`/`-v` to `RoomAction::Members` (guard: only meaningful with `--status`) and
  `RoomAction::Tail` in `cli.rs`; thread to `message::members_status`/`tail`.
- Render the `diag:` block (§5.3) after the panel, stderr-only, using `peer_paths()` +
  `relay_url()` + `render_endpoint_addr` (now including `relay=`). Reuse
  `short_device`/`short_id`/labels.

### Step 6 — Docs + conformance
- README reference table (§5.2); getting-started Troubleshooting two-line shape + **Verbose
  network diagnostics** subsection (§5.5).
- `docs_conformance.rs`: (a) keep `all_four_failure_modes` / `failure_modes_have_next_actions`
  / `readme_documents_every_error_code` green; (b) add a gate that the README reference names
  a next action for the user-actionable codes; (c) add a gate that the guide documents the
  `--verbose` / `diag:` / `path=` vocabulary.

### Step 7 — Tests
- New `tests/diagnostics_cli.rs` (see §8). Extend `error_taxonomy.rs` for the `next:` line.

### Step 8 — Gate
- `scripts/verify.sh` (fmt `--check`, clippy `-D warnings` pedantic, `--all-features` tests)
  is the CI gate (project memory *verify.sh is the real CI gate*). No new clippy `#[allow]`
  creep; no `#[allow(dead_code)]` on the newly-live path (`peer_paths`/`relay_url` must be
  reached by `--verbose`).

---

## 7. Error model & observability

- **One render point per surface.** Terminal `error[<code>]:` + `next:` flow through
  `main.rs`; per-event advisories through `StderrAudit`; the `diag:` block through the single
  verbose renderer. No scattered stderr writes.
- **Additive over pinned strings.** `next_action()` sits beside `code()`; the `diag:` block
  reuses `PeerConnState`/`OfflineReason`/`PathType` labels. The machine surface (code, exit,
  render prefix) is unchanged (AC2).
- **Diagnostics are advisory, never trust.** `PathType`/relay URL are read-only transport
  observations; they change no verdict, order, or authz decision (same rule as
  `OfflineReason`).
- **No tracing subscriber.** The `diag:` block writes directly to stderr (project memory
  *CLI has no tracing subscriber*); `--verbose` is the opt-in, matching `pipe expose -v`.

## 8. Test strategy

In `crates/iroh-rooms-cli/tests/` (via `assert_cmd`, network-free where possible) + unit
tests; the network splits stay on the existing `#[ignore]`-gated two-peer tier.

**Unit (fast, deterministic):**
1. `ErrorCode::next_action()`: every user-actionable code → `Some(non-empty)`; `Internal`/
   `InvalidArgument` → `None`; no next_action string contains a long base32/hex run.
2. Net `PathType::label`/`is_hole_punched` pinned; `classify_remote_info` over synthetic
   `RemoteInfo` for Direct/Relay/Mixed/None.

**CLI integration:**
3. **AC1 next step present:** representative coded failures each print `error[<code>]:`
   **and** a following `next:` line — `invalid_room_id` (bad room id), `identity_not_found`
   (no identity), `no_such_file`/`file_too_large` (`file share`), a ticket decode
   (`ticket_bad_checksum`), a join reject (`bad_capability` via the join harness / `#[ignore]`
   if it needs two processes), `room_not_found` via **both** `room members` and a send path
   (same message + next action — the consistency fix).
4. **AC2 machine surface intact:** the first stderr line still starts `error[<code>]:`; the
   exit code equals the §5.3/#25 category; a script grepping `^error\[` and branching on `$?`
   still works with the `next:` line present.
5. **Verbose diagnostics (loopback):** `room members --status --verbose --loopback` prints
   the panel **plus** `diag: local …`, `diag: peer …`, `diag: transport …`; `path=` uses a
   valid label; without `--verbose` **no** `diag:` line appears (§18.5 default-hidden).
6. **Verbose is stderr-only:** stdout under `--verbose` is unchanged from the non-verbose run
   (script-clean).
7. **AC3 no secret leak:** with a known identity seed and a known ticket secret, neither the
   `--status --verbose` output nor a corrupted-ticket `room join` failure contains the seed
   hex or the ticket token base32 substring on **either** stream.
8. **Regression:** `room send` with no reachable peers still exits `0` with the availability
   line and **no** `error[`/`next:`/`diag:` line.
9. **AC2 uniform bad-argument:** a malformed `--timeout` on **`file fetch`** now emits
   `error[invalid_argument]:` / exit `2` (the cli.rs:538 fix), matching the four sibling
   timeout sites — a regression guard against the uncoded-`error:`/exit-1 path it took before.

**Docs conformance:** README reference ⇔ emitted codes (existing gate) + next-action-present
gate + `--verbose`/`diag:` vocabulary gate; the four **Next action** blocks stay ≥4.

## 9. Security, privacy, reliability, performance

- **Secret hygiene (load-bearing).** Verbose output and next-step lines render only public
  identifiers/labels/addresses/relay URLs; `next_action()` is non-interpolating; the no-leak
  test (#7) guards it. Preserves the "no secret material reaches an error path" invariant
  (`main.rs`, spec D8/§9) and the ticket/`SecretKeys` redaction (§2.4).
- **No trust from diagnostics.** Path type and relay URL are observations, not authorization
  inputs; surfacing them changes no verdict (§18.1 "clear connection state" without weakening
  the gate).
- **Honest availability (§16.4/§18.2).** An `unauthorized` peer renders `path=none`, never as
  reachable; zero-peer `room send` stays success.
- **Reliability.** Additive `#[non_exhaustive]` enum method + additive net accessors; older
  call sites keep compiling; a new §8 reject reason flows into `reject_next_action`'s
  wildcard (`None`) until the table is extended (a compile-safe default).
- **Performance.** `next_action()` is a match returning a literal. `peer_paths()` issues one
  `remote_info` read per known peer, only under `--verbose`, off the send/receive hot path.
- **Back-compat.** The only output change is the *added* `next:` line and the opt-in `diag:`
  block; the `error[<code>]:` prefix and exit codes are unchanged, so #25's contract holds.

## 10. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| iroh `RemoteInfo`/`TransportAddr` API differs from spike-nat's pinned version | **low** | med | `spike-nat` and `iroh-rooms-net` pin the *identical* `iroh = "=1.0.1"`, so the types are the same — the port is a verbatim copy, not an adaptation. |
| `remote_info`/relay URL reads `None` before the link settles (shipping skips `.online()`) | med | low | Read diagnostics post-settle (`members_status` already waits); render `path=none`/`relay=none` honestly on `None` — never block or fabricate. |
| Double next-step (message *and* `next:`) after partial migration | med | low | §5.1 migration rule: `next:` is the only next-step line; Step 3 trims inline steps; test #3 asserts a single `next:`. |
| Secret leak via the new verbose/next surface | low | high | Non-interpolating `next_action`; render public-only; single render points; no-leak test #7; never call ticket `Display`. |
| `--verbose` noise breaks a script that reads stderr | low | low | Diagnostics are stderr-only and opt-in; default output byte-identical; `diag:` is grep-separable. |
| Renaming/duplicating a pinned code while editing messages | low | high | `next_action` wraps `RejectReason`/`TicketError`; never re-list a code; `code()` unchanged. |
| Scope creep into a full `doctor` subcommand | med | low | Explicitly OQ-3/out of scope; ship `--verbose` on existing commands only. |

## 11. Acceptance criteria (mapped to the issue)

- **AC1 — common failure states include actionable next steps.** `ErrorCode::next_action()`
  yields a concrete, secret-free step for every common code; `main.rs` renders `next: …`; the
  §2.2 gaps are filled and `room_not_found` is made consistent. Tests #1, #3.
- **AC2 — error codes remain script-friendly.** `code()`, `exit_code()`, and the
  `error[<code>]:` prefix are unchanged; the `next:` line and `--verbose` block are additive
  and stderr-only. Tests #4, #6.
- **AC3 — verbose diagnostics do not log private keys, ticket secrets, or sensitive
  payloads.** The `diag:` block and next-step lines render only public data; no-leak test #7;
  ticket `Display` is never called on the diagnostic path.
- **AC4 — docs include failure examples.** Getting-started Troubleshooting shows the two-line
  `error`/`next` shape and a **Verbose network diagnostics** subsection; README carries the
  error-code reference; docs-conformance gates the reference ⇔ emitter and the ≥4 Next
  actions.

## 12. Assumptions

1. The executing agent runs a full dev checkout and may modify `crates/iroh-rooms-{cli,net}`
   (and `core` only if the ticket next-step is chosen to live in core, OQ-4), `README.md`,
   `docs/getting-started.md`, and add tests; `scripts/verify.sh` is the gate.
2. #25/IR-0110's taxonomy (codes, categories, exits, render contract) and #35/IR-0210's
   getting-started doc are landed and are the foundation IR-0303 extends, not replaces.
3. The `spike-nat` `classify_remote_info`/`PathType` reference is correct for the iroh
   version in `iroh-rooms-net`; the port is an adaptation, not a fresh design.
4. `--verbose` on the status-bearing commands is a sufficient "network diagnostics" surface
   for the Developer Preview; a room-less `doctor` is a later nice-to-have.
5. The `next:` second line and opt-in `diag:` block are acceptable output additions (only
   `0`-vs-non-zero and the `error[<code>]:`/exit contract were relied on by scripts).

## 13. Open questions

- **OQ-1 (next-step render shape).** `next: <action>` on a second stderr line (recommended,
  matches the existing `next:` success convention) vs folding the action into the
  `error[<code>]:` message vs a `hint:` prefix. Recommendation: separate `next:` line — keeps
  the machine line clean and greppable.
- **OQ-2 (how aggressively to migrate rich messages).** Move *all* inline next-steps into
  `next_action()` for uniformity (recommended) vs only fill the bare gaps and leave the
  IR-0205 messages untouched (less churn, heterogeneous). Recommendation: migrate, gated by
  the single-`next:` test.
- **OQ-3 (standalone `doctor`).** Ship only `--verbose` on `room members --status`/`room
  tail` (recommended for this issue) vs also add `iroh-rooms doctor` (relay reachability +
  local addrs, no room required — better for "I can't connect to anything"). Recommendation:
  defer `doctor` to a follow-up; it reuses `PathType`/`relay_url` with zero taxonomy change.
- **OQ-4 (ticket next-steps: CLI vs core).** Surface ticket-decode next steps via
  `ErrorCode::Ticket(_).next_action()` in the CLI (recommended, no core edit, keeps
  `TicketError::Display` stable/redacted) vs add guidance to `core/ticket.rs`. Recommendation:
  CLI-side.
- **OQ-5 (`--json`/`--error-format=json`).** A structured error+diagnostics JSON envelope is
  still out of scope (was #25 OQ-5). Flag for a later DX issue; the `error[<code>]:` line +
  exit + `diag:` text remain the contract.
- **OQ-6 (verbose flag scope).** Per-command `--verbose` (recommended, mirrors `pipe expose
  -v`) vs a global `-v`. Recommendation: per-command to avoid clashing with `pipe expose`'s
  existing `-v` semantics and to keep the node-bearing commands the only ones that produce a
  `diag:` block.

## 14. Definition of done

- `ErrorCode::next_action()` landed with pins; `main.rs` renders the `next:` line; the §2.2
  gaps are filled and `room_not_found` is consistent across both sites.
- README **Error codes** reference (code · category · exit · meaning · next action) and the
  getting-started **Verbose network diagnostics** subsection landed; docs-conformance gates
  extended and green.
- `PathType` + `classify_remote_info` ported into `iroh-rooms-net`; `Node::peer_paths()` /
  `Node::relay_url()` added; `room members --status --verbose` and `room tail --verbose`
  render the `diag:` block (relay URL now surfaced), default output unchanged.
- No-leak test proves the verbose/next surface carries no seed, ticket token, or payload; the
  machine surface (code + exit + `error[<code>]:` prefix) is byte-for-byte unchanged.
- `tests/diagnostics_cli.rs` + `error_taxonomy.rs`/`docs_conformance.rs` extensions cover §8;
  `scripts/verify.sh` is green (fmt, clippy `-D warnings` pedantic, tests). No protocol,
  schema, gate, authorization, or exit-code-scheme change.
