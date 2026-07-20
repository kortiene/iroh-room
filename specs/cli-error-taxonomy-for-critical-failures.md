# Spec: CLI error taxonomy for critical failure modes (`iroh-rooms`)

Issue: **#25 / IR-0110** — Add CLI error taxonomy for critical failure modes.
Parent epic: **#2**. Labels: `type/feature area/cli area/dx priority/p1 risk/medium`.

Traceability:
- PRD `PRD.v0.3.md` **§16** (CLI Requirements — UX requirement 3: *"Failed connection
  states should distinguish offline peer, unauthorized peer, unavailable blob,
  invalid ticket, and invalid signature"*) and **§18.5** (UX Risk — clear CLI outputs).
- Spike `PHASE-0-SPIKE.md` **§8** (Rejection / Flag Taxonomy) and the Membership &
  Ordering Model §5 (offline-vs-unauthorized, fail-closed access).

Dependencies (all landed): **#7** (protocol conformance / §8 taxonomy),
**#20** (`room send`/`room tail`), **#21** (offline room-read CLI), **#23**
(authenticated pipe reconciliation, stderr audit sink).

> This is a **planning/spec document only**. No production code is written by this
> issue's planning phase. The implementation steps in §6 are for the executing
> engineer/agent.

---

## 1. Summary

The CLI already computes rich, correct failure information at every layer — the
protocol validator, the membership fold, the transport admission gate, the pipe
gate, and the ticket codec each carry their **own** stable reason enums with pinned
string codes. What is missing is a **single, uniform, script-facing error surface**
on the `iroh-rooms` binary: today every failure is an `anyhow::Error` printed as a
bare `error: <prose>` line and the process **always exits `1`**, so a script or test
cannot reliably tell *invalid signature* from *unauthorized sender*, *offline peer*
from *rejected peer*, or *bad ticket* from *bad room id* without brittle substring
matching on human prose that was never contract-pinned.

This issue introduces a thin **CLI error taxonomy** that:

1. Defines a stable `ErrorCode` set that **reuses** the already-pinned core/net codes
   verbatim (`RejectReason::code()`, `PeerConnState`/`OfflineReason` labels, the pipe
   and admission causes) and adds the few CLI-native codes still missing.
2. Renders every terminal CLI failure as a **machine-parseable line**
   `error[<code>]: <human message>` on stderr, and maps each code to a **stable
   category exit code** so scripts can branch on `$?` without parsing text.
3. Guarantees the six scoped critical failure modes each produce a distinct,
   actionable, **secret-free** code + message: invalid signature, unauthorized peer,
   offline peer, invalid/expired ticket, unavailable-blob placeholder, and the
   clock-skew *advisory* (a warning, never a failure).

The net code change is small and additive: one new `error` module in
`crates/iroh-rooms-cli`, one new `TicketError::code()` in `crates/iroh-rooms-core`,
a rewire of `main.rs` error rendering, and per-command adoption of the code carrier.
No protocol, schema, gate, or authorization behaviour changes.

---

## 2. Background & current repository state

The distinguishing information the ACs demand **already exists** — it is just not
surfaced through one consistent CLI contract. Inventory of the taxonomies to unify:

### 2.1 Core (`crates/iroh-rooms-core`)

- **`event::reject::RejectReason`** — the §8 protocol rejection taxonomy. 15 variants,
  each with a stable `.code()` (`bad_signature`, `id_mismatch`, `non_canonical_encoding`,
  `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`,
  `not_genesis_descended`, `room_id_mismatch`, `unbound_device`, `not_a_member`,
  `insufficient_role`, `expired_invite`, `bad_capability`, `room_full`). `#[non_exhaustive]`,
  implements `Display`/`Error`. **Coverage is conformance-gated** (README: the
  taxonomy-completeness gate fails if a variant lands without a vector). This is the
  authoritative source for the *invalid signature* vs *unauthorized sender* split
  (AC1): `bad_signature` is the crypto layer; `not_a_member`/`unbound_device`/
  `insufficient_role` are the authorization layer.
- **`event::reject::Flag`** — advisory flags on *accepted* events: `clock_skew`,
  `equivocation`, `from_removed_member`, each with `.code()`. A flag **never** changes
  the verdict, set, order, or any authz/expiry decision (spike §6 step 10 / §8). This
  is the source for the *clock-skew advisory* scope item.
- **`ticket::TicketError`** — fail-closed ticket-decode failures: `BadPrefix`,
  `BadBase32`, `Truncated`, `UnsupportedVersion(u8)`, `BadChecksum`, `MalformedBody`.
  Has a redacted `Display` and `#[non_exhaustive]` but **no `.code()` method yet** —
  the one gap to fill in core. The ticket type's `Debug` is already secret-redacted.
- **`membership::DenyReason`** — access-decision denials the pipe/blob gates consult
  (mapped into the net pipe cause below).

### 2.2 Net (`crates/iroh-rooms-net`)

- **`state::PeerConnState`** (`.label()`: `connecting` / `connected` / `offline` /
  `unauthorized`) — the PRD §16.3 trichotomy: the *offline peer* vs *rejected
  (unauthorized) peer* distinction (AC2) is exactly this enum.
- **`state::OfflineReason`** (`.label()`: `never_dialed` / `unreachable` /
  `transport_error` / `link_dropped` / `deauthorized`) — the diagnostic refinement of
  `offline`. Never a trust input.
- **`admission::RejectCause`** (`.code()`: `unknown_device` / `not_active` /
  `fail_closed`) — connect-time admission rejects (before any event byte is read).
- **`pipe::audit::PipeDenyCause`** (`.code()`: `not_allowed` / `expired` / `closed`)
  — pipe stage-2 connect denials.
- **`audit::AuditSink`** — the trait the CLI already installs (IR-0108) so rejects are
  visible **without a `tracing` subscriber**. Relevant hooks already present:
  `rejected(device, RejectCause)`, `offline(device, reason)`, `event_rejected(device,
  count)`, `deauthorized(device)`. The engine's bounded `logs()` ring already records
  per-frame `reject.<code>` entries (IR-0201). **There is no `event_flagged` hook yet**
  — that is the additive surface for the clock-skew advisory (§5.9).

### 2.3 CLI (`crates/iroh-rooms-cli`)

- **`main.rs`** is the only place that renders a terminal error:
  ```rust
  Err(err) => { eprintln!("error: {err:#}"); ExitCode::FAILURE }
  ```
  `{err:#}` prints the full anyhow context chain. Exit code is **always `1`**; `clap`
  independently exits `2` on argument-parse errors and `0` on `--help`/`--version`.
- Command functions return `anyhow::Result<_>` and build messages ad hoc. Examples of
  the *inconsistency* this issue removes:
  - `join.rs::join_reject_message` embeds the §8 code **in prose**:
    `"this ticket's secret or identity does not match the invite (bad_capability)"`.
  - `file.rs::classify_path` uses free prose with **no code**: `"no such file: …"`,
    `"… is a directory, not a file"`, `"permission denied reading …"`.
  - `cli.rs::parse_room_id` → `"invalid room id (expected `blake3:<hex>`)"` (no code).
  - Ticket decode → `"could not decode invite ticket (expected a roomtkt1… token…)"`
    (no code; deliberately does **not** echo the token — a property to preserve).
- **`room send`** deliberately treats *reaching zero peers* as **success** (exit 0,
  stdout `delivered: 0 (no peers online — stored locally only)`), per the PRD §14
  availability model. This must **not** become an error under the new taxonomy.
- **`room members --status`** / **`room tail`** already render the offline-vs-
  unauthorized connection panel using `PeerConnState`/`OfflineReason` labels.
- **`tests/docs_conformance.rs`** pins documented behaviour against real CLI output —
  the natural home for a "documented error table matches emitted codes" gate.

### 2.4 The precedent to imitate

`file share`'s §7 error handling and the `PeerConnState`/`OfflineReason` **label
stability tests** (`state.rs`) are the house pattern: a small enum with a `.code()`/
`.label()` returning `&'static str`, plus an explicit test that pins every string
because *"tooling parses these strings"*. This spec extends that pattern up to the
process boundary.

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

Map protocol and network failures to actionable CLI errors with **stable codes** so a
human sees an actionable message and a script/test can branch deterministically on a
code string and/or an exit code — without leaking secrets.

### 3.2 In scope (this issue)

The six scoped critical failure modes, each with a distinct stable code, message, and
exit category:

1. **Invalid signature** — inbound event whose Ed25519 signature fails under
   `device_id` (`bad_signature`), kept distinct from authorization failures (AC1).
2. **Unauthorized peer / sender** — `unauthorized` connection state (AC2) and the
   authorization-layer rejects `not_a_member` / `unbound_device` / `insufficient_role`.
3. **Offline peer** — `offline` connection state with an `OfflineReason` refinement,
   kept distinct from `unauthorized` (AC2), and the connectivity command failures
   (`no_admin_reachable` on a join that never reaches the admin; a `pipe connect` that
   cannot reach the owner).
4. **Invalid or expired ticket** — every `TicketError` variant plus the CLI-native
   `wrong_identity` and `no_discovery_hint`, and the log-verdict `expired_invite`;
   each includes the reason **without leaking the ticket secret** (AC3).
5. **Unavailable blob placeholder for later phase** — a reserved `blob_unavailable`
   code + message defined now and mapped where `file list` already shows
   `reference-only`, so the serve/fetch issue only has to *emit* it.
6. **Clock skew advisory** — surface `Flag::ClockSkew` as a `warning[clock_skew]: …`
   line; **never** an error, never non-zero exit, never a gate.

Plus the framework that makes them uniform and stable (AC4):

- A CLI `ErrorCode` taxonomy + a `CliError` carrier that attaches a code to an
  `anyhow` chain.
- A pinned render contract `error[<code>]: <message>` and a `warning[<code>]: …`
  advisory contract, both on **stderr** (stdout stays clean for scripting).
- A stable **category → exit-code** mapping.
- Stability tests + a documented error-code table (README + docs troubleshooting),
  gated in `docs_conformance.rs`.

Adopting the taxonomy across the already-landed command surfaces that produce the six
modes: `room join`, `room send`, `room tail` (receive path), `room members --status`,
`pipe connect`, `pipe close`, `file share`/`file list`, and the shared `parse_room_id`
/ identity-load paths.

### 3.3 Out of scope / non-goals (explicit)

- **No new protocol, schema, gate, or authorization behaviour.** Codes are *surfaced*,
  not *invented*; verdicts are unchanged. The conformance gate on `RejectReason` stays
  authoritative — this issue must not rename or re-pin any §8 code.
- **No `file fetch` implementation.** `blob_unavailable` is defined and reserved; the
  actual fetch-time emission lands with the serve/fetch issue (README: "serve/fetch
  half … deliberately out of scope").
- **No `--json` error envelope** (structured error objects on stdout). The stable
  surface is the `error[<code>]:` stderr line + the exit code. A JSON error mode is a
  documented open question (§13), not a deliverable.
- **No change to the zero-peers-is-success semantics** of `room send` (PRD §14).
- **No `agent` command errors** — `agent invite`/`agent status` are unlanded.
- **No retro-fitting every non-critical prose error** to a code in this issue. The
  framework is established and the six critical modes are converted; the long tail
  (e.g. name-validation prose) can adopt `.coded(...)` incrementally and is tracked as
  cleanup, not gated here.

---

## 4. Placement & dependencies

### 4.1 Where the code lives

| Change | Crate / file | Kind |
| --- | --- | --- |
| `TicketError::code()` + stability test | `iroh-rooms-core/src/ticket.rs` | additive |
| `ErrorCode`, `ErrorCategory`, `CliError`, `.coded()` ext trait, `bail_coded!` | **new** `iroh-rooms-cli/src/error.rs` | additive |
| Terminal render + exit code | `iroh-rooms-cli/src/main.rs` | rewrite of one arm |
| `event_flagged` audit hook (clock-skew advisory) | `iroh-rooms-net/src/audit.rs` + pump call site | additive (default no-op) |
| Adopt `.coded(...)` on the six modes | `iroh-rooms-cli/src/{cli,join,message,pipe,file,identity}.rs` | edits |
| Error-code table | `README.md` + `docs/getting-started.md` (troubleshooting) | docs |
| Tests | `iroh-rooms-cli/tests/error_taxonomy.rs` (new) + `docs_conformance.rs` | tests |

`error.rs` depends only on `iroh_rooms_core::{event::RejectReason, ticket::TicketError}`
and `iroh_rooms_net::state::{PeerConnState, OfflineReason}` (already CLI deps) plus
`anyhow`. It introduces **no new external crate**.

### 4.2 Dependency boundary with the serve/fetch issue

`blob_unavailable` is a reserved code with a message and an exit category. `file
list` already computes `provider: reference-only`; this issue adds the code + message
constant and (optionally) a `file fetch` stub arm that fails with `blob_unavailable`
and a "not yet implemented / provider offline" message, so when serve/fetch lands it
swaps the stub for real emission with **zero taxonomy change**.

---

## 5. Design

### 5.1 The unified `ErrorCode` taxonomy (wrap, don't duplicate)

`ErrorCode` is a thin enum that **wraps** the existing pinned enums where they exist
and adds CLI-native variants for the rest. Wrapping (rather than re-listing all 14 §8
codes) keeps the core conformance gate the single source of truth and makes a new §8
code appear on the CLI automatically.

```rust
// crates/iroh-rooms-cli/src/error.rs
use iroh_rooms_core::event::RejectReason;
use iroh_rooms_core::ticket::TicketError;
use iroh_rooms_net::state::OfflineReason;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// A protocol §8 rejection (reuses `RejectReason::code()` verbatim). Covers
    /// `bad_signature` (AC1 integrity) and `not_a_member`/`unbound_device`/
    /// `insufficient_role`/`expired_invite`/`bad_capability` (AC1/AC3 authz), plus
    /// the structural/encoding rejects.
    Reject(RejectReason),
    /// A ticket decode failure (reuses `TicketError::code()` verbatim; AC3).
    Ticket(TicketError),

    // --- connectivity (AC2 / offline-peer scope) ---
    /// A join never reached the room admin within the timeout (offline/unreachable
    /// admin). Distinct from an authorization rejection.
    NoAdminReachable,
    /// A `pipe connect` could not reach the pipe owner (owner offline).
    PeerOffline(OfflineReason),
    /// A peer presented itself but is not a bound Active member (rejected, not
    /// offline). The command-failure twin of `PeerConnState::Unauthorized`.
    PeerUnauthorized,

    // --- ticket-adjacent, CLI-native (AC3) ---
    /// The local identity does not match the ticket's `invitee_key`.
    WrongIdentity,
    /// The ticket carries no admin discovery hint and no `--peer` was given.
    NoDiscoveryHint,

    // --- availability, reserved for the serve/fetch phase (scope item 5) ---
    /// No reachable provider holds the requested blob (placeholder; emitted by the
    /// future `file fetch`).
    BlobUnavailable,

    // --- input / environment ---
    InvalidRoomId,
    InvalidArgument,     // bad --role / --expires / --format / duration, etc.
    NoSuchFile,
    PermissionDenied,
    FileTooLarge,
    IdentityNotFound,    // secrets not present (run `identity create`)
    RoomNotFound,        // room id not in the local log

    /// Catch-all for an unexpected internal failure (should be rare; a bug signal).
    Internal,
}
```

`code()` returns a stable `&'static str`; the wrapped arms delegate:

```rust
impl ErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Reject(r) => r.code(),          // e.g. "bad_signature", "not_a_member"
            Self::Ticket(t) => t.code(),          // e.g. "ticket_bad_checksum" (new)
            Self::NoAdminReachable   => "no_admin_reachable",
            Self::PeerOffline(_)     => "peer_offline",   // reason in the message
            Self::PeerUnauthorized   => "peer_unauthorized",
            Self::WrongIdentity      => "wrong_identity",
            Self::NoDiscoveryHint    => "no_discovery_hint",
            Self::BlobUnavailable    => "blob_unavailable",
            Self::InvalidRoomId      => "invalid_room_id",
            Self::InvalidArgument    => "invalid_argument",
            Self::NoSuchFile         => "no_such_file",
            Self::PermissionDenied   => "permission_denied",
            Self::FileTooLarge       => "file_too_large",
            Self::IdentityNotFound   => "identity_not_found",
            Self::RoomNotFound       => "room_not_found",
            Self::Internal           => "internal",
        }
    }
    pub fn category(&self) -> ErrorCategory { /* §5.3 */ }
    pub fn exit_code(&self) -> u8 { self.category().exit_code() }
}
```

> **Note on `PeerOffline(_)` code:** the *code* is the stable coarse token
> `peer_offline`; the `OfflineReason` label (`unreachable`, `transport_error`, …) is
> carried in the human message (`peer_offline reason=unreachable`). This keeps the
> code set small and script-stable while preserving the diagnostic. The finer reason
> is already independently pinned by `OfflineReason::label`.

### 5.2 The `CliError` carrier + render contract

Command functions keep returning `anyhow::Result<_>` (preserving the context-chain
ergonomics) but attach an `ErrorCode` at the layer where the failure class is known:

```rust
pub struct CliError { pub code: ErrorCode, message: String }
impl std::fmt::Display / std::error::Error for CliError { /* message only */ }

/// Ergonomic attach — mirrors anyhow's `.context(...)`.
pub trait CodedResultExt<T> {
    fn coded(self, code: ErrorCode) -> anyhow::Result<T>;
    fn with_coded(self, f: impl FnOnce() -> ErrorCode) -> anyhow::Result<T>;
}
#[macro_export] macro_rules! bail_coded { ($code:expr, $($fmt:tt)*) => { /* … */ } }
```

`main.rs` walks the anyhow chain for the **outermost** `CliError`, extracts its code,
and renders a pinned line + a category exit code:

```rust
fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            match error::code_of(&err) {                 // walks err.chain()
                Some(code) => eprintln!("error[{}]: {err:#}", code.code()),
                None       => eprintln!("error: {err:#}"),  // uncoded → generic
            }
            ExitCode::from(error::code_of(&err).map_or(1, |c| c.exit_code()))
        }
    }
}
```

**Render contract (pinned by tests):**
- Coded failure → stderr line `error[<code>]: <message>[: <context>…]`, exit =
  category code.
- Uncoded failure → stderr line `error: <message>`, exit `1` (generic; a code-adoption
  gap, acceptable for the long tail).
- Advisory → stderr line `warning[<code>]: <message>`, exit unaffected.
- **stdout is never used for errors or warnings** — script-friendly output stays clean.
- No secret bytes, no raw ticket token, and no key material ever appears on either
  stream (§9).

### 5.3 Exit-code scheme (stable script contract)

A small, documented **category** scheme (not one code per variant — categories are the
stable script contract; the string code is the fine-grained one). Aligned with
`clap`'s existing exit `2` for usage.

| Exit | `ErrorCategory` | Meaning | Codes |
| ---: | --- | --- | --- |
| `0` | — | success (incl. `send` reaching zero peers) | — |
| `1` | `Internal` | unexpected / uncoded internal error | `internal`, any uncoded |
| `2` | `Usage` | bad input / environment (clap-aligned) | `invalid_room_id`, `invalid_argument`, `no_such_file`, `permission_denied`, `file_too_large`, `identity_not_found`, `room_not_found`, `no_discovery_hint` |
| `3` | `Auth` | authorization / capability denial | `not_a_member`, `unbound_device`, `insufficient_role`, `expired_invite`, `bad_capability`, `wrong_identity`, `peer_unauthorized` |
| `4` | `Integrity` | crypto / structural rejection | `bad_signature`, `id_mismatch`, `non_canonical_encoding`, `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`, `not_genesis_descended`, `room_id_mismatch` |
| `5` | `Ticket` | ticket decode failure | `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`, `ticket_unsupported_version`, `ticket_bad_checksum`, `ticket_malformed` |
| `6` | `Connectivity` | reachability / availability | `no_admin_reachable`, `peer_offline`, `blob_unavailable` |

The **string code is authoritative** for fine-grained branching; the exit code is the
coarse contract for `if ! iroh-rooms …; then case $? in …`. Both are pinned by tests.

> This category→exit table is the one genuinely new *contract* (a hard-to-reverse
> compatibility surface). It is presented as the recommended design; see §13 OQ-1 for
> the fallback (collapse all runtime failures to exit `1`, keep only the string code).

### 5.4 Two surfaces, one taxonomy

The six modes arrive on **two distinct surfaces**; both draw from the same `ErrorCode`:

- **(a) Terminal command failures** — the command aborts non-zero. Renders
  `error[<code>]: …`. Examples: `room join` rejected `bad_capability`; ticket
  `ticket_bad_checksum`; `pipe connect` to an offline owner `peer_offline`; `file
  share ./missing` `no_such_file`; `room send <bad-room>` `invalid_room_id`.
- **(b) Per-event receive-path advisories** — a *long-running* session
  (`room tail`, `room members --status`) keeps running while individual inbound events
  are dropped or flagged. These render `warning[<code>]: …` via the installed
  `AuditSink`, and are the surface where AC1's *bad_signature vs not_a_member* split is
  observable per event (the engine's `logs()` already records `reject.<code>`; the
  sink surfaces them without a `tracing` subscriber — see project memory *CLI has no
  tracing subscriber*).

The connection panel (AC2, offline-vs-unauthorized) is surface (b): it already prints
per-peer `state=<label> [reason=<reason>]`. This issue keeps those strings and ensures
they are pinned and cross-referenced to the taxonomy table, but does not restructure
the panel.

### 5.5 Mapping the six scoped modes

| Scope item | Trigger | Surface | Code | Exit |
| --- | --- | --- | --- | ---: |
| Invalid signature | inbound event, sig fails under `device_id` | (b) tail reject | `bad_signature` | — (warn) |
| Unauthorized sender | inbound event by non/removed member | (b) tail reject | `not_a_member` / `unbound_device` / `insufficient_role` | — (warn) |
| Unauthorized peer | connect by non-Active device | (b) conn panel + audit | `peer_unauthorized` (state `unauthorized`) | — |
| Offline peer | authorized member, no path | (b) conn panel | `peer_offline` + `reason=…` | — |
| Join can't reach admin | join timeout, no admin frame | (a) command | `no_admin_reachable` | 6 |
| Pipe owner offline | `pipe connect`, owner unreachable | (a) command | `peer_offline` | 6 |
| Invalid ticket | `TicketError` on decode | (a) command | `ticket_*` | 5 |
| Wrong identity for ticket | local id ≠ `invitee_key` | (a) command | `wrong_identity` | 3 |
| Expired invite | join fold-check `expired_invite` | (a) command | `expired_invite` | 3 |
| Bad capability | join fold-check `bad_capability` | (a) command | `bad_capability` | 3 |
| Unavailable blob | `file fetch` (future), no provider | (a) command | `blob_unavailable` | 6 |
| Clock skew | flagged accepted event | (b) advisory | `clock_skew` (`warning[…]`) | — |

### 5.6 Ticket errors without secret leakage (AC3)

- Add `TicketError::code()` in core returning stable strings prefixed `ticket_`:
  `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`,
  `ticket_unsupported_version`, `ticket_bad_checksum`, `ticket_malformed`. Add a
  `code`-stability unit test mirroring `OfflineReason::label` pinning.
- The CLI maps a decode failure to `ErrorCode::Ticket(e)` and renders
  `error[ticket_bad_checksum]: invite ticket failed its checksum (corrupted on
  copy-paste?)` — the reason is the existing redacted `Display`, which **contains no
  secret**.
- **Hard rule (tested):** the error path must never echo the raw ticket **token**
  (the base32 body embeds the capability secret) nor any decoded field. `join.rs`
  already avoids echoing the token — preserve that. The AC3 test constructs a ticket
  with a known secret, corrupts it, and asserts the secret's base32 substring does not
  appear anywhere on stderr.
- `wrong_identity` renders identity keys (public, safe) but not the ticket; the
  existing message already does this correctly.

### 5.7 Offline vs unauthorized (AC2)

Already structurally distinct via `PeerConnState`. This issue:
- Documents the two states in the taxonomy table and pins their labels
  (`offline`/`unauthorized`) + the `OfflineReason` labels as part of the taxonomy
  stability test set (they already have pinning tests in `state.rs`; add a
  cross-reference test in the CLI suite asserting `room members --status` never prints
  an `unauthorized` peer as `offline` — the PRD §16.4 honesty rule).
- Adds `ErrorCode::PeerUnauthorized` / `PeerOffline` for the *command-failure* twins so
  `pipe connect`'s "owner offline" vs "you are not allowed" report distinct codes.

### 5.8 Invalid signature vs unauthorized sender (AC1)

The split is inherent to `RejectReason`: `bad_signature` (crypto) vs
`not_a_member`/`unbound_device`/`insufficient_role` (authz). This issue ensures the
**CLI actually surfaces the specific code** rather than a generic "rejected":
- On the receive path, the `AuditSink::event_rejected` count is refined to carry (or be
  accompanied by) the per-frame `reject.<code>` already in the engine `logs()`, so
  `room tail` prints `warning[bad_signature]: dropped inbound event …` vs
  `warning[not_a_member]: dropped inbound event …`. (Implementation note: prefer
  reading the engine's bounded `logs()` reject entries over widening the sink
  signature; see §6 Step 5 and OQ-2.)
- The AC1 test drives two inbound events into a `room tail` session — one with a
  corrupted signature, one from a non-member key — and asserts the two distinct codes
  appear.

### 5.9 Clock-skew advisory

- `Flag::ClockSkew` is advisory. Add an additive `AuditSink::event_flagged(device,
  code: &'static str)` hook (default no-op, like `event_rejected`) and have the `Node`
  receive-path pump call it when a validated event carries a flag. The engine already
  computes flags in `validate_wire_bytes`; the pump forwards the flag code.
- The CLI's audit sink renders `warning[clock_skew]: an inbound event's timestamp is
  far from local time (advisory; not rejected)`. **Never** affects exit code, verdict,
  or ordering — a test asserts a clock-skewed-but-valid event is still rendered in the
  timeline and the session still exits `0`.
- `equivocation` / `from_removed_member` flags may reuse the same `event_flagged` hook
  for free (they render `warning[equivocation]` / `warning[from_removed_member]`), but
  only `clock_skew` is required by this issue's scope.

### 5.10 Unavailable-blob placeholder

- Define `ErrorCode::BlobUnavailable` + a message constant now. No `file fetch` command
  is added. Optionally scaffold a hidden/documented `file fetch` arm that returns
  `bail_coded!(ErrorCode::BlobUnavailable, "no reachable provider holds this file yet
  (serve/fetch not implemented; see #—)")` so the code is exercised by a test today and
  the serve/fetch issue only swaps the body. Decision flagged in OQ-3.

---

## 6. Implementation steps

Ordered so each step compiles and is independently testable.

### Step 0 — Core: `TicketError::code()` (gates the ticket ACs)
- Add `pub fn code(&self) -> &'static str` to `TicketError` with the six `ticket_*`
  strings. `UnsupportedVersion(_)` maps to the version-independent
  `ticket_unsupported_version`.
- Add `ticket_error_codes_are_stable` unit test pinning all six.
- Confirm `#[non_exhaustive]` remains and `Display` is unchanged (still redacted).

### Step 1 — CLI: the `error` module
- New `crates/iroh-rooms-cli/src/error.rs`: `ErrorCode`, `ErrorCategory` (+`exit_code()`),
  `CliError`, `CodedResultExt` (`.coded` / `.with_coded`), `bail_coded!`, and
  `code_of(&anyhow::Error) -> Option<ErrorCode>` (chain walk, outermost wins).
- `From<RejectReason>` / `From<TicketError>` for `ErrorCode` for terse mapping.
- Unit tests: `code()` strings pinned for every variant; `exit_code()` for every
  category; `code_of` finds a code through `.context(...)` layers and returns the
  outermost when nested.
- Register `mod error;` in `main.rs`.

### Step 2 — CLI: terminal render + exit codes
- Rewrite the `Err` arm of `main.rs` per §5.2. Keep the `{err:#}` context chain.
- Add integration assertions (in the new suite) that a coded failure prints
  `error[<code>]:` and returns the category exit code, and an uncoded failure still
  prints `error:` and exits `1`.

### Step 3 — CLI: adopt codes on input/environment paths
- `cli.rs::parse_room_id` → `.coded(ErrorCode::InvalidRoomId)`.
- `identity::SecretKeys::load` / `Profile::load` "not found" → `IdentityNotFound`
  (keep the "run `identity create`" hint).
- `room::members` / offline reads on an unknown room → `RoomNotFound`.
- `message::parse_timeout`, `--role`, `--format`, `--expires` parse failures →
  `InvalidArgument`.
- `file::classify_path`: `no_such_file` → `NoSuchFile`, directory/`not a file` →
  `InvalidArgument` (or a dedicated `not_a_file` — OQ-4), over-cap → `FileTooLarge`,
  permission → `PermissionDenied`.

### Step 4 — CLI: adopt codes on the ticket / join path
- Ticket decode in `join.rs` → `map_err(|e| CliError::from(e))` /
  `.coded(ErrorCode::Ticket(e))` preserving the redacted `Display`; **do not** echo the
  token.
- Wrong identity → `WrongIdentity`; empty dial set → `NoDiscoveryHint`.
- `join_reject_message` → return `ErrorCode` + message: `bad_capability`,
  `expired_invite`, `insufficient_role` map to `ErrorCode::Reject(reason)`; the
  catch-all keeps embedding `reason.code()`.
- Join timeout (no Active transition / never reached admin) → `NoAdminReachable`.

### Step 5 — CLI + Net: receive-path advisories (AC1 signature-vs-authz, clock skew)
- Net: add `AuditSink::event_flagged(device, code)` (default no-op) and call it from
  the `Node` receive pump for each flag on an accepted event.
- CLI: implement a CLI audit sink (extending the IR-0108 stderr sink) that renders
  `warning[<code>]: …` for `event_rejected`/reject-log entries (distinct
  `bad_signature` vs `not_a_member`) and for `event_flagged` (`clock_skew`).
- Decide the reject-code plumbing: read the engine's bounded `logs()` `reject.<code>`
  entries (preferred, no signature change) vs widen `event_rejected` to carry the code
  (OQ-2). Pin the choice in the sink's rustdoc.

### Step 6 — CLI: connectivity command failures
- `pipe connect` owner-unreachable → `PeerOffline(OfflineReason::Unreachable)`;
  not-allowed / non-member → `PeerUnauthorized`.
- Verify `room send` zero-peers path is untouched (still exit `0`, stdout advisory) —
  add a regression test so nobody "codes" it into a failure.

### Step 7 — Docs + conformance
- README: add an **Error codes** subsection with the §5.3 table (code, category, exit,
  meaning). `docs/getting-started.md`: extend the troubleshooting guide to map each of
  the six modes to its code and the fix.
- `tests/docs_conformance.rs`: add a check that every code named in the README table is
  produced by at least one CLI path (or listed as reserved, e.g. `blob_unavailable`),
  and that no emitted code is missing from the table — the AC4 "stable enough for
  scripts/tests" gate, mirroring the core taxonomy-completeness gate.

### Step 8 — Full taxonomy test suite
- New `tests/error_taxonomy.rs` (see §8).

---

## 7. Error model & observability

- **One rendering point.** All terminal errors flow through `main.rs`; all
  per-event advisories through the CLI audit sink. No `println!`-to-stderr scattering.
- **Codes are additive over pinned strings.** Wrapped arms (`Reject`, `Ticket`,
  `OfflineReason`) reuse the source enum's already-pinned string, so the CLI cannot
  drift from the protocol/net vocabulary.
- **Exit codes are coarse; string codes are fine.** Documented together.
- **Advisory ≠ error.** `warning[clock_skew]` and the connection panel lines never
  change `$?`. `room send` reaching zero peers is success.
- **No tracing subscriber required.** The CLI audit sink writes directly to stderr
  (project memory: *CLI has no tracing subscriber*), so rejects/flags are visible in a
  plain `iroh-rooms room tail` run.

---

## 8. Test strategy

Test plan (issue): *"CLI tests that assert error code and human-readable message for
each failure mode."* Concretely, in `crates/iroh-rooms-cli/tests/error_taxonomy.rs`
(via `assert_cmd`, network-free where possible) plus unit tests:

**Unit (fast, deterministic):**
1. `ErrorCode::code()` pinned for every variant; `exit_code()` pinned per category.
2. `TicketError::code()` pinned for all six.
3. `code_of` walks context layers; outermost code wins; uncoded → `None`.
4. Every `RejectReason` variant maps to a category via `From` (a match-exhaustiveness
   guard so a new §8 code forces a category decision).

**CLI integration — one assertion of (exit code, `error[<code>]:` line, message
substring) per mode:**
5. **AC1 integrity vs authz (surface a):** `room join` with a tampered/bad-capability
   ticket → `error[bad_capability]`, exit 3.
6. **AC1 receive path (surface b):** a two-peer/loopback `room tail` fed one
   bad-signature frame and one non-member frame → stderr contains
   `warning[bad_signature]` **and** `warning[not_a_member]`, distinct; session exits 0.
   (Reuse the `two_peer_e2e`/`join_e2e` harness pattern; gate `#[ignore]` if it needs
   two live processes.)
7. **AC2 offline vs unauthorized:** `room members --status` on a room with one
   unreachable member and one removed device → the removed device renders
   `unauthorized` (never `offline`), the unreachable one renders `offline reason=…`.
8. **AC2 command:** `pipe connect` to an offline owner → `error[peer_offline]`, exit 6;
   to a disallowed pipe → `error[peer_unauthorized]`, exit 3.
9. **AC3 ticket reasons:** malformed prefix → `error[ticket_bad_prefix]`; corrupted
   checksum → `error[ticket_bad_checksum]`; truncated → `error[ticket_truncated]`;
   wrong version token → `error[ticket_unsupported_version]`; each exit 5.
10. **AC3 no secret leak:** corrupt a real ticket carrying a known secret; assert the
    secret's base32 substring appears **nowhere** on stdout or stderr.
11. **Ticket wrong identity / no hint:** `error[wrong_identity]` (exit 3);
    `error[no_discovery_hint]` (exit 2).
12. **Join can't reach admin:** join with an unreachable `--peer` and short timeout →
    `error[no_admin_reachable]`, exit 6.
13. **Clock-skew advisory:** feed a clock-skewed-but-valid event → `warning[clock_skew]`
    printed, event still in the timeline, exit 0.
14. **Blob-unavailable placeholder:** the reserved code renders correctly (via the
    `file fetch` stub if adopted, else a direct unit assertion on the message).
15. **Input paths:** bad room id → `error[invalid_room_id]` exit 2; `file share
    ./missing` → `error[no_such_file]` exit 2; over-cap → `error[file_too_large]` exit
    2; missing identity → `error[identity_not_found]` exit 2.
16. **Regression:** `room send` with no reachable peers → exit **0**, stdout
    `delivered: 0 …`, **no** `error[`/`warning[` line.

**Docs conformance:** `docs_conformance.rs` — README error table ⇔ emitted codes
(no orphan codes, no undocumented codes), reserved codes explicitly listed.

`scripts/verify.sh` (fmt + clippy `-D warnings` pedantic + tests) is the gate — see
project memory *verify.sh is the real CI gate*. Keep all strings clippy-clean and
avoid `#[allow]` creep.

---

## 9. Security, privacy, reliability, performance

- **Secret hygiene (the load-bearing property).** The taxonomy must never widen the
  existing secret-free guarantee’s attack surface. Enforced: ticket errors carry the
  redacted `Display`, never the token; capability secrets stay in `Zeroizing` buffers;
  no key bytes reach any code path. Dedicated no-leak test (#10). This preserves the
  "no secret material ever reaches an error path (spec D8/§9)" comment in `main.rs`.
- **No trust from labels.** `OfflineReason`, connection labels, and advisory flags are
  diagnostic only — never authorization inputs (as already documented in `state.rs` and
  spike §6). Surfacing them changes no verdict.
- **Honest availability (PRD §16.4/§18.2).** An `unauthorized` peer is never shown as
  `offline`; zero-peer delivery is reported, not hidden or errored.
- **Reliability.** Additive `#[non_exhaustive]` enums + default-no-op audit hooks mean
  older call sites keep compiling; a new §8 code surfaces automatically and the
  `From<RejectReason>` match forces a category decision at compile time.
- **Performance.** Error/label formatting is off the hot path; the audit sink is
  already required to be cheap and non-blocking (`audit.rs`). No allocation added to
  steady-state send/receive.
- **Migration/back-compat.** The **exit-code scheme is a new contract** — pre-existing
  scripts only relied on `0` vs non-zero, and every failure remains non-zero, so no
  script that used exit `1`-as-"any error" breaks in the "was it an error" sense; only
  scripts that branch on the *specific* code gain determinism. The `error:` → `error[
  code]:` prefix change is the one output-format change; documented in the PR and
  covered by the docs-conformance gate.

---

## 10. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| Exit-code scheme becomes a compatibility burden | med | med | String code is authoritative; exit categories are coarse (7) and documented; OQ-1 fallback to flat exit `1` is one-line reversible. |
| Secret leaks via a new error path | low | high | Single render point; redacted `Display`; explicit no-leak test (#10); no token echo. |
| Renaming/duplicating a pinned §8 code drifts from the conformance gate | low | high | Wrap `RejectReason`/`TicketError` — never re-list; delegate `.code()`. |
| AC1 receive-path split needs two live processes (flaky CI) | med | low | Prefer the deterministic engine-`logs()` path; `#[ignore]`-gate any live-process test as `two_peer_e2e` does. |
| Long-tail prose errors stay uncoded → inconsistent surface | med | low | Framework + six modes gated now; remaining paths adopt `.coded()` incrementally; uncoded still renders `error:`/exit 1 gracefully. |
| Widening `AuditSink` breaks external impls | low | low | New hook is a defaulted no-op, matching the established pattern. |

---

## 11. Acceptance criteria

Mapped to the issue ACs:

- **AC1 — invalid signature vs unauthorized sender.** `bad_signature` (integrity, exit
  4 as a command failure / `warning[bad_signature]` on the receive path) is emitted
  distinctly from `not_a_member`/`unbound_device`/`insufficient_role` (auth, exit 3 /
  `warning[not_a_member]`). Tests #5, #6.
- **AC2 — offline vs rejected peer.** `offline` (+`reason=`) is rendered distinctly
  from `unauthorized`; command twins `peer_offline` (exit 6) vs `peer_unauthorized`
  (exit 3). An `unauthorized` peer is never labelled `offline`. Tests #7, #8.
- **AC3 — invalid ticket reasons without secret leak.** Every `TicketError` maps to a
  distinct `ticket_*` code + redacted reason; the raw token/secret never appears on any
  stream. Tests #9, #10, #11.
- **AC4 — codes stable for scripts/tests.** Every code has a pinned `.code()` and a
  category exit code; a README table is gated against emitted codes in
  `docs_conformance.rs`; `#[non_exhaustive]` + exhaustive `From` matches prevent silent
  drift. Tests #1–#4, #16, docs-conformance.

Plus: clock-skew renders as an advisory that never fails a command (test #13); the
`blob_unavailable` placeholder is defined and reserved (test #14); `room send` zero-peer
success is preserved (test #16).

---

## 12. Assumptions

1. The executing agent runs in a full dev checkout and may modify
   `crates/iroh-rooms-{core,cli,net}`, `README.md`, `docs/getting-started.md`, and add
   test files; `scripts/verify.sh` is the gate.
2. The six §16 scoped modes are the required set; retro-coding every remaining prose
   error is explicitly deferred (§3.3).
3. `RejectReason`/`Flag`/`PeerConnState`/`OfflineReason`/`RejectCause`/`PipeDenyCause`
   codes are stable and must be reused verbatim, not renamed.
4. `main.rs`'s exit code may be extended from a flat `FAILURE` to a category scheme
   without breaking any current consumer (only `0` vs non-zero was relied on).
5. `file fetch` is unlanded; `blob_unavailable` is a reserved placeholder this issue
   only defines, not fully wires.
6. The stderr audit-sink path from IR-0108/IR-0201 is the intended home for receive-path
   advisories (no `tracing` subscriber is added).

## 13. Open questions

- **OQ-1 (exit codes).** Adopt the 7-category exit-code scheme (§5.3, recommended) or
  keep a flat non-zero exit `1` and make the **string code** the sole machine surface?
  Recommendation: adopt categories — the AC asks for script-stable codes and `$?`
  branching is the idiomatic shell contract. Reversible if rejected.
- **OQ-2 (AC1 receive-path plumbing).** Surface the per-frame reject code by reading the
  engine's bounded `logs()` `reject.<code>` ring (no signature change, recommended) or
  by widening `AuditSink::event_rejected` to carry the code? Recommendation: read
  `logs()` to keep the sink trait stable.
- **OQ-3 (`file fetch` stub).** Scaffold a documented `file fetch` arm that emits
  `blob_unavailable` today (exercises the code, zero-change handoff to serve/fetch), or
  keep `blob_unavailable` a pure reserved constant tested only at unit level?
  Recommendation: scaffold the stub arm behind the existing `file` group so the code is
  live-tested.
- **OQ-4 (`not_a_file`).** Give directory/non-file share targets a dedicated
  `not_a_file` code, or fold them under `invalid_argument`? Recommendation: fold under
  `invalid_argument` (exit 2) to keep the set minimal; split later if a script needs it.
- **OQ-5 (`--json` errors).** Should a future `--json`/`--error-format=json` mode emit a
  structured `{ "error": { "code", "message", "exit" } }` object? Out of scope here;
  the `error[<code>]:` line + exit code is the contract. Flag for a later DX issue.
- **OQ-6 (prefix bikeshed).** `error[<code>]:` vs `error: <code>: …` vs `error(<code>)`.
  Recommendation: `error[<code>]:` — unambiguous, greppable, and visually distinct from
  the anyhow context that follows.

## 14. Definition of done

- `TicketError::code()` landed with stability test.
- `crates/iroh-rooms-cli/src/error.rs` provides `ErrorCode`, `ErrorCategory`,
  `CliError`, `.coded()`/`bail_coded!`, and `code_of`.
- `main.rs` renders `error[<code>]:` + category exit codes for coded failures and
  `error:`/exit 1 for uncoded ones.
- The six scoped modes each emit a distinct, secret-free code + message on the correct
  surface, with the exit code from §5.3.
- `AuditSink::event_flagged` added (default no-op) and the CLI sink renders
  `warning[clock_skew]` (+ reject advisories distinguishing `bad_signature` from
  `not_a_member`).
- README **Error codes** table + `getting-started.md` troubleshooting updates, gated in
  `docs_conformance.rs`.
- `tests/error_taxonomy.rs` covers every §8 test item; `scripts/verify.sh` is green
  (fmt, clippy `-D warnings` pedantic, tests).
- No protocol/schema/gate/authorization behaviour changed; no secret reachable on any
  error path.
