# Spec: Agent Identity and the `agent` Command Group (IR-0206)

| | |
|---|---|
| **Issue** | #31 — `[IR-0206] Implement agent identity` |
| **Parent** | #3 |
| **Labels** | `type/feature` `type/security` `area/protocol` `area/agent` `priority/p1` `risk/medium` |
| **Dependencies** | #16 (IR-0101, identity + device CLI — **landed**), #18 (IR-0103, key-bound invite ticket — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.8, §13.3, §16; Spike `PHASE-0-SPIKE.md` Event Protocol §1 (Identity & Key Model), Membership & Ordering §3.1 (Roles) / §3.5 (auth gate) |
| **Owning crate** | `crates/iroh-rooms-cli` (new `agent` subcommand group; new `src/agent.rs`). **No `iroh-rooms-core` change is required.** |

> **Status:** planning/spec only. This document is the build plan for another
> engineer/agent to execute. No production code is modified by this phase.

---

## 1. Summary

Make agents **first-class room participants at the CLI surface** by adding the
`agent` command group the PRD promises (`PRD.v0.3.md` §15.8 / §16):

```text
iroh-rooms [--data-dir <PATH>] agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]
```

`agent invite` mints a **key-bound, agent-role** invite ticket for a named agent
identity — exactly the artifact `room invite --role agent` already produces — but
under a dedicated verb with a positional `<AGENT_ID>` that matches the PRD CLI
surface verbatim. The agent then joins with the **shared** `room join <ticket>`
flow (there is no separate `agent join`), and every downstream plane (membership
fold, access gates, transport admission, `room members` display) already treats
the `agent` role correctly.

### 1.1 Why this issue is small (the model already supports agents)

The Phase-0 spike deliberately models an **agent as an ordinary principal** — its
own `sender_id` + `device_id`, distinguished only by `role = "agent"` in the
membership state (Spike §1 final ¶, §3.1). As a consequence, almost every
acceptance criterion for this issue is **already satisfied by landed code**:

| Issue AC | Already satisfied by (landed) | Gap this issue closes |
|---|---|---|
| Agent has its own identity **and** device key. | `identity create` (#16/IR-0101) mints a distinct `sender_id`/`device_id` for any principal, human or agent; the getting-started demo creates `build-agent` this way. | **None** at the protocol level. This issue *documents* it and adds a test proving an agent identity is byte-identical in shape to a human one. |
| Agent role appears in membership state. | `Role::Agent`, the least-privilege merge, `room members` rendering `role=agent` (#12/IR-0008, #21/IR-0106). `role_str(Role::Agent) => "agent"`. | **None.** New test asserts `room members` shows `role=agent` after an `agent invite` + join. |
| Agent cannot access a room without explicit invite. | Key-bound `gate_join` + default-deny snapshot + transport admission (#12, #19/IR-0104, #22/IR-0107). An uninvited key has no membership event ⇒ `is_active == false`. | **None.** New test proves an uninvited agent is denied. |
| Human and agent identities use the **same protocol model**. | The event model, key model, invite, join, and fold are role-agnostic; `agent` is one enum value in a shared `role: tstr`. | **This is a design constraint to preserve, not build:** the new command must *not* fork a parallel identity/key path. |

So the **only real gap** is the missing `agent` CLI verb. Today `iroh-rooms agent
…` is unrecognized — the getting-started guide flags it as "scaffold — the binary
does not recognise it yet" (`docs/getting-started.md` line 67) and marks the exact
`agent invite` syntax as `[reconcile]` (line 84). This issue lands the verb and
reconciles the guide.

### 1.2 What is explicitly NOT in this issue

- **`agent status`** (posting `agent.status` events). The `agent.status` content
  type exists (`event::content::AgentStatus`) but the CLI command that authors it
  is **tracked separately** (README "agent status … tracked separately"). This
  issue introduces the `agent` command *group* but implements only `agent invite`;
  `agent status` is added by its own issue as a second `AgentAction` arm.
- **Any change to the identity/key/event/fold protocol.** AC4 requires humans and
  agents to share one model; this issue must not introduce an agent-specific key
  type, identity file, or membership path.

---

## 2. Background & current repository state

### 2.1 Landed work this builds on

- **Identity & device CLI (#16 / IR-0101).** `identity create --name <NAME>` mints
  a participant identity keypair (`sender_id`) **and** a device keypair
  (`device_id`) from the OS CSPRNG, persists them under the resolved data dir with
  owner-only perms, and refuses to clobber without `--force`. `identity show`
  prints both ids. **There is no agent-specific flag: agents are created by this
  same path** (spec IR-0101 §15 Assumption 6, §2 "An 'agent' is an ordinary
  principal"). The getting-started demo runs
  `identity create --name "build-agent"` in a third data dir (`.demo/agent`).
- **Key-bound invite (#18 / IR-0103).** `room invite <ROOM_ID> --invitee
  <IDENTITY_ID> [--role member|agent] [--expires <DURATION>]` already:
  - accepts `--role agent` (`INVITABLE_ROLES = &["member", "agent"]`, rejecting
    `admin`) in `crates/iroh-rooms-cli/src/invite.rs`;
  - confirms the caller is the single immutable admin via the fold (AC1);
  - draws `invite_id` + capability secret, computes `capability_hash`, builds +
    self-validates + fold-checks + persists an admin-signed `member.invited`
    carrying `role = "agent"`, and emits a `roomtkt1…` ticket carrying the role.
  - `invite::invite(home, room_id, invitee_hex, role, expires) -> InviteSummary`
    and `invite::print_invite(&summary)` are the exact functions `agent invite`
    will call.
- **Room join (#19 / IR-0104).** `room join <TICKET>` decodes the ticket
  (including `role`), assembles a `member.joined` with the same role, and the
  landed `gate_join` accepts it iff a still-live, key-bound, role-matching admin
  invite lies in its ancestors. Agent joins already work end-to-end; the
  getting-started demo has the agent run `room join <AGENT_TICKET>`.
- **Membership fold (#12 / IR-0008).** `Role::Agent` is the least-privileged role
  (`Agent < Member < Admin`), resolved by the min-lattice merge with lowest-
  `event_id` tie-break. `MembershipSnapshot::role/status/is_active` expose it;
  `room members` renders `role=agent` (`room::role_str`).
- **Transport admission (#22 / IR-0107, #9 / IR-0005).** `SnapshotAdmission`
  default-denies any device whose bound identity is not `Active`. An uninvited
  agent's device is rejected before bytes.

### 2.2 The gap (this issue closes it)

`crates/iroh-rooms-cli/src/cli.rs` has **no `Agent` arm** in its top-level
`Command` enum (only `Identity`, `Room`, `Pipe`, `File`). There is no
`src/agent.rs`. Running `iroh-rooms agent invite …` today produces a clap
"unrecognized subcommand" error. PRD §16 lists `iroh-rooms agent invite <room-id>
<agent-id>` and `iroh-rooms agent status <room-id> "…"` as MVP CLI commands; §15.8
("Add Agent Participant") makes `agent invite` a named user journey.

### 2.3 Spike / PRD facts that constrain the design

- **Agents are ordinary principals (Spike §1).** "Agents are ordinary
  participants: an agent has its own `sender_id`/`device_id` pair and the same
  binding rule applies (role distinguishes it, §7)." ⇒ **no separate key type, no
  separate identity file, no `agent.db`.** AC4 is a hard constraint.
- **Roles (Spike §3.1).** `agent` = "a member with `role = "agent"`. Same
  membership rules; cannot invite/remove; can open pipes only when room policy +
  explicit authorization allow (PRD 13.3)." In MVP there is **no room-policy
  mechanism**; pipe authorization is per-pipe `--allow`, identical for members and
  agents. So the `agent` role is, in MVP, *informational at the membership level*
  plus the least-privilege tie-break — it does **not** unlock or restrict any
  additional capability beyond what a `member` has, except the documented
  "not implicitly trusted / must be explicitly invited" posture.
- **Agent security (PRD §13.3).** Agent has its own identity (1); joins only
  through explicit invite (2); events are signed (3); cannot access rooms unless
  invited (4); cannot open pipes unless authorized (5); artifacts content-
  addressed & verified like user files (6). Items (1)–(4) are this issue's ACs;
  (5)–(6) are already true because agents reuse the member pipe/file paths.
- **Explicit agent invite (PRD §13.1 #8, §15.8 #2, §17.1.2).** "One agent identity
  can join or be configured as a room participant" is a technical success metric;
  the mechanism is the explicit, key-bound invite — never implicit access.
- **Key-bound only (Spike §6).** `member.invited.invitee_key` is required; open
  bearer tickets are excluded. `agent invite` therefore requires the agent's
  identity key, exactly like `room invite`.

### 2.4 Workspace conventions to honor

- Pure/deterministic core assemblers; the only RNG in `core` stays in
  `SigningKey::generate`. **This issue adds no core code**, so it inherits this for
  free by delegating to the landed `invite::invite`.
- Validate arguments **before any IO**; a bad invocation leaves the store
  untouched (already true of `invite::invite`).
- Errors → stderr + non-zero exit; success → stdout + exit 0.
- Secret hygiene: the capability secret lives in `Zeroizing` inside
  `invite::invite`; `agent invite` never touches secret bytes itself.
- `scripts/verify.sh` (fmt + clippy `-D warnings` pedantic + workspace tests
  `--all-features`) is the CI gate. New code must be clippy-pedantic clean.
- CLI doc-comments avoid backticks (they render literally in `--help`); follow the
  `#[allow(clippy::doc_markdown)]` pattern already used across `cli.rs`.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A new top-level `agent` subcommand group in `crates/iroh-rooms-cli/src/cli.rs`
   (`Command::Agent { action: AgentAction }`) with one arm:
   `AgentAction::Invite`.
2. `agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` — positional
   `<AGENT_ID>` (matching PRD §16), role forced to `agent`, delegating to the
   landed `invite::invite(home, &room_id, agent_id, "agent", expires)`.
3. A thin `crates/iroh-rooms-cli/src/agent.rs` module owning the `agent invite`
   orchestration wrapper and its output helper (or a documented decision to call
   `invite::invite` / `invite::print_invite` directly — see D2).
4. Reconcile `docs/getting-started.md`: replace the agent-invite step's
   `room invite … --role agent` workaround with `agent invite <ROOM_ID>
   <AGENT_ID>`, remove the "Step 7 agent is scaffold — not recognised yet" caveat
   **for the invite path**, and drop the `[reconcile]` marker for `agent invite`
   syntax. (The `agent status` step stays flagged as scaffold — separate issue.)
5. Tests (issue Test Plan): CLI/unit for **agent identity** (same shape as a
   human), **agent invite role** (on-log `role=agent`, ticket role `agent`,
   admin-only, no secret leak), and an **unauthorized access attempt** (uninvited
   agent denied).
6. Keep the docs-conformance suite green (it already asserts the guide documents
   `agent invite`, the `.demo/agent` dir, and the agent-requires-explicit-invite
   narrative — see §11).

### 3.2 Out of scope (sibling issues — do **not** implement here)

- **`agent status`** command (`agent.status` authoring). Separate issue; add the
  second `AgentAction` arm there. Leave a `// agent status: tracked separately`
  note but do **not** stub it.
- **A separate agent-identity-creation path** (`agent create`/`agent register`
  that mints keys). AC4 forbids a parallel model; identity creation stays the
  shared `identity create`. (`agent create` as a thin *alias* is an explicitly
  considered-and-deferred alternative — see OQ1 / §4 D3.)
- **`agent join`.** Agents redeem tickets with the shared `room join`.
- **Room-level agent policy / trust levels** (PRD §13.5 #7–#8) — post-MVP.
- **Any `iroh-rooms-core` change**: no new event type, role, key, or fold rule.
- **Network push of the invite.** `agent invite` is offline/local exactly like
  `room invite` (the persisted `member.invited` propagates via sync/net later).

### 3.3 Why the split is safe

The trust boundary (admin-only authoring, key-binding, capability matching,
expiry, sticky departure, default-deny access) is **already enforced
deterministically** by the landed stateless validator, membership fold, and
transport admission. `agent invite` produces the *same* well-formed, admin-signed
`member.invited{role="agent"}` event as `room invite --role agent`; it introduces
no new authorization surface. Even a buggy `agent` wrapper cannot grant access: a
join is authorized solely by the on-log invite + a secret recomputing the on-log
`capability_hash`, and access is gated on the current `Active` snapshot.

---

## 4. Key design decisions

### D1 — `agent invite` is an ergonomic wrapper over `invite::invite`, role fixed to `agent` (recommended)

`agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` calls the **landed**
`invite::invite(home, &room_id, agent_id_hex, "agent", expires)` and prints the
result. It adds no new authorization logic, no new event, no new ticket format.

- **Why:** maximizes reuse; guarantees an agent invite is byte-for-byte the same
  capability artifact as `room invite --role agent`, so both the fold and every
  peer treat them identically. Preserves AC4 (same protocol model) by
  construction. The only behavioral difference from `room invite` is the surface:
  positional `<AGENT_ID>` (matching PRD §16) and role pinned to `agent`.
- **Alternative (rejected):** duplicate the RNG/admin-gate/persist orchestration
  inside `agent.rs`. Rejected — it would fork the one secret-bearing path and lose
  the single tested invite pipeline.

### D2 — Placement: a thin `src/agent.rs`, or direct dispatch to `invite::*` (recommended: thin module)

Two viable shapes; pick one before coding:

- **(recommended) `crates/iroh-rooms-cli/src/agent.rs`** exposing
  `pub fn invite(home, room_id, agent_id_hex, expires) -> Result<InviteSummary>`
  that calls `crate::invite::invite(home, room_id, agent_id_hex, "agent",
  expires)`, plus `pub fn print_agent_invite(summary)` (which may just call
  `crate::invite::print_invite` with an agent-tailored `next:` hint). Rationale:
  a named home for future agent commands (`agent status` lands next door), and a
  place to hang agent-specific UX (e.g. a "you invited an agent; it is not
  implicitly trusted" note per PRD §13.3) without touching `invite.rs`.
- **(fallback) No new module:** the `cli.rs` `AgentAction::Invite` arm calls
  `invite::invite(home, &room_id, &agent_id, "agent", expires.as_deref())` and
  `invite::print_invite(&summary)` directly. Zero new files. Acceptable if a
  reviewer prefers minimal surface; loses the future-home benefit.

Either way, **no logic is copied** — the wrapper is a one-liner over the landed
function. Declare `mod agent;` in the CLI crate root (mirror `mod invite;`).

### D3 — Agent identity is created by the shared `identity create` (no `agent create`) — AC4

Do **not** add an agent-specific identity-minting command. `identity create
--name "build-agent"` already produces a distinct `sender_id`/`device_id` with the
same on-disk layout as any human identity; that *is* the agent's identity. This
directly realizes AC4 ("Human and agent identities are represented through the
same protocol model") and Spike §1.

- The spec's answer to the issue scope line "Agent identity creation **or**
  registration command" is: **the registration-into-a-room command is `agent
  invite`; identity creation is the shared `identity create`.** An agent becomes a
  room participant only by being invited (registered) and joining — never by a
  separate identity ceremony.
- **Alternative (deferred, OQ1):** `agent create --name <NAME>` as a thin *alias*
  of `identity create` for discoverability. Rejected by default because two verbs
  writing the same `identity.json`/`identity.secret` invites confusion about
  whether an "agent identity" is a different on-disk artifact (it is not), and
  risks diverging from the one-identity-per-home model. If added later it MUST be a
  pure alias with identical behavior and a note that the files are shared.

### D4 — Positional `<AGENT_ID>`, `--expires` optional; role is not a flag

Match PRD §16 exactly: `agent invite <ROOM_ID> <AGENT_ID>`. `<AGENT_ID>` is the
agent's 64-char lowercase-hex identity id (from the agent's `identity show`),
positional (not `--invitee`). `--expires <DURATION>` (`<int>{s|m|h|d}`) is
supported for parity with `room invite` (the landed `invite::invite` already
parses it); absent ⇒ non-expiring. There is **no** `--role` flag — the verb *is*
the role. This means `agent invite` cannot be used to issue a `member`/`admin`
invite (use `room invite` for those), which is the intended, least-surprising
behavior.

### D5 — Reuse every guard from `invite::invite`; document the agent-relevant ones

Because `agent invite` delegates, it inherits, for free:
- **Admin-only (AC1 of #18, supports AC "explicit invite").** Non-admin caller →
  "only the room admin can issue invites" error, store untouched.
- **Self-invite guard.** If `<AGENT_ID>` equals the caller's own identity → error
  (an admin inviting itself as an agent is meaningless).
- **Already-active warning.** Re-inviting an already-`Active` identity warns but
  proceeds (sticky departure makes a stale invite inert). Note (§9): re-inviting
  an already-joined *member* as an `agent` does **not** downgrade them post-join;
  the least-privilege merge only applies to *concurrent* invite heads. This is
  existing documented fold behavior, unchanged here.
- **Key-binding, capability-hash, expiry, secret hygiene, validate-before-persist.**

### D6 — Output: reuse `invite::print_invite`, optionally with an agent `next:` hint

`invite::print_invite` already prints `role: agent`, the bound `invitee:` key, the
expiry, the `roomtkt1…` ticket, and the password-grade warning. Reuse it. The one
optional tweak: an agent-tailored final hint, e.g.
`next: the agent runs \`iroh-rooms room join <ticket>\``, and (per PRD §13.3) a
one-line reminder that the agent is a first-class participant that is **not
implicitly trusted**. Keep stdout script-friendly (labeled lines); put any prose
reminder on stderr or as a trailing comment line so it does not break parsing.

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]
```

- `<ROOM_ID>` — positional, `blake3:<64-hex>` (parsed via the shared
  `cli::parse_room_id`).
- `<AGENT_ID>` — positional, 64-char lowercase-hex identity id (parsed by the
  landed `invite::parse_invitee` path, i.e. `IdentityKey::from_str`).
- `--expires <DURATION>` — optional, `<int>{s|m|h|d}` (e.g. `24h`, `7d`); absent ⇒
  no expiry.
- Exit `0` + ticket on stdout on success; non-zero + stderr message on any error,
  store left unmodified on all pre-persist failures.

Wire into `cli.rs`:

```rust
#[derive(Debug, Subcommand)]
enum Command {
    Identity { /* … */ },
    Room     { /* … */ },
    Pipe     { /* … */ },
    File     { /* … */ },
    /// Invite and manage agent participants (first-class, explicitly invited).
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
}

#[derive(Debug, Subcommand)]
enum AgentAction {
    /// Mint a key-bound, agent-role invite ticket for a known agent identity.
    Invite {
        #[allow(clippy::doc_markdown)]
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
        #[allow(clippy::doc_markdown)]
        /// The agent's identity id (64-char lowercase hex from `identity show`).
        agent_id: String,
        #[allow(clippy::doc_markdown)]
        /// Optional expiry as <int>{s|m|h|d}, e.g. 24h.
        #[arg(long)]
        expires: Option<String>,
    },
    // agent status: tracked separately (authors `agent.status`); not added here.
}
```

Dispatch (mirror `dispatch_room`):

```rust
Command::Agent { action } => dispatch_agent(&home, action)?,
// …
fn dispatch_agent(home: &std::path::Path, action: AgentAction) -> Result<()> {
    match action {
        AgentAction::Invite { room_id, agent_id, expires } => {
            let room_id = parse_room_id(&room_id)?;
            // Delegates to the landed invite path with role fixed to "agent";
            // validates agent_id/expires before any IO (store untouched on error).
            let summary = agent::invite(home, &room_id, &agent_id, expires.as_deref())?;
            agent::print_agent_invite(&summary);
        }
    }
    Ok(())
}
```

---

## 6. Module / file plan

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/src/cli.rs` | Add `Command::Agent { action: AgentAction }`, `enum AgentAction { Invite { … } }`, dispatch arm + `dispatch_agent`. Update the module-header surface doc-comment. |
| `crates/iroh-rooms-cli/src/agent.rs` | **new** (D2 recommended) — `pub fn invite(home, room_id, agent_id_hex, expires) -> Result<InviteSummary>` delegating to `invite::invite(.., "agent", ..)`; `pub fn print_agent_invite(&InviteSummary)`. `#[cfg(test)]` unit tests. |
| `crates/iroh-rooms-cli/src/main.rs` (or lib module list) | Declare `mod agent;` (mirror `mod invite;`). |
| `crates/iroh-rooms-cli/tests/agent_cli.rs` | **new** — `assert_cmd` integration suite (§11). |
| `docs/getting-started.md` | Reconcile the agent-invite step to `agent invite <ROOM_ID> <AGENT_ID>`; drop the invite-path scaffold caveat and the `agent invite` `[reconcile]` marker (§10 step 5). |

No `iroh-rooms-core` change. No new dependency (delegates to landed CLI code;
`invite.rs` already pulls `hex`, `getrandom`, `zeroize`, the core `ticket`/event
builders).

---

## 7. Dependencies to add

**None.** `agent invite` reuses `invite::invite`, which already depends on the core
event builder, `RoomInviteTicket`, the store, the fold, `getrandom`, and
`zeroize`. Dev-deps (`assert_cmd`, `predicates`, `tempfile`) are already present in
the CLI crate for the sibling integration suites.

---

## 8. Error model & observability

All errors are `anyhow` with actionable context (inherited from `invite::invite`);
nothing secret appears in any message. Distinct, non-zero-exit failures:

| Condition | Behavior (inherited unless noted) |
|---|---|
| Bad `<ROOM_ID>` | `parse_room_id` error before IO. |
| Bad `<AGENT_ID>` (not 64-hex / non-curve-point) | error before IO (`parse_invitee`). |
| Bad `--expires` (empty / `0` / no suffix / overflow) | error before IO. |
| No local identity | actionable error pointing at `identity create`. |
| Unknown `room_id` (no events in store) | "no room … run `room create`". |
| Caller is not the room admin | "only the room admin can issue invites"; **AC (explicit invite / admin-gated)**. |
| Self-invite (`<AGENT_ID>` == caller identity) | error before IO. |
| OS CSPRNG unavailable | error (getrandom mapping). |
| Built event fails fold self-check | internal-error guard; **not persisted**. |
| Store open/write failure | error with the db path; validate-before-insert avoids partial state. |

Observability: success prints the labeled summary + ticket; the persisted
`member.invited{role="agent"}` event **is** the audit record — `room members`
then shows the agent as `status=invited`, `role=agent`, an end-to-end sanity hook.
No secret is ever logged. (The CLI installs no tracing subscriber — consistent with
the `CLI has no tracing subscriber` project memory; stdout/stderr are the only
observability surfaces, which is sufficient here since this command is
synchronous/offline.)

---

## 9. Security, privacy, reliability

- **Explicit invite only, no implicit access (AC3 / PRD §13.3 #2,#4).** An agent is
  admitted only by an admin-signed, key-bound `member.invited{role="agent"}` and a
  `member.joined` descending from it. An uninvited agent key has **no** membership
  event ⇒ `snapshot.is_active(agent) == false` ⇒ default-deny at every plane
  (fold gate, blob/pipe gate, transport admission). The new suite proves this
  directly (§11 "unauthorized access attempt").
- **Same protocol model (AC4).** No agent-specific key, identity file, event type,
  or fold rule is introduced. An agent identity is a human identity with
  `role="agent"` assigned at invite time. A test asserts an agent's `identity
  show` output is structurally identical to a human's (both have a 64-hex
  `identity_id` and a distinct 64-hex `device_id`).
- **Own identity + device key (AC1).** Guaranteed by the shared `identity create`
  (#16): `identity_id != device_id`, both 32-byte Ed25519 keys (Spike §1).
- **Role in membership state (AC2).** The on-log `member.invited.role == "agent"`
  folds to `Role::Agent`; `room members` renders `role=agent`.
- **Not implicitly trusted (PRD §13.3).** The `agent` role is the least-privileged
  in the min-lattice; concurrent-attribute conflicts resolve toward `agent`. In
  MVP this is informational at the membership level (no extra capability is granted
  by the role); the substantive guarantee is "must be explicitly invited," which
  the key-bound gate enforces. Document that MVP has no agent-specific *runtime*
  restriction beyond a member's (pipe/file access is identical) — the difference is
  admission (invite) + display, not a distinct permission set. This is faithful to
  Spike §3.1 and avoids over-claiming.
- **Secret hygiene.** The capability secret lives in `Zeroizing` inside
  `invite::invite`; `agent invite` never handles secret bytes. The ticket token is
  the sole secret carrier; the AC3-of-#18 secret-absent-from-log guarantee is
  inherited. New test greps `agent invite` stdout/stderr for the on-log
  `capability_hash`/secret and asserts absence.
- **Reliability / restart determinism.** The invite persists as canonical wire
  bytes into the same append-only `rooms.db`; re-folding reproduces the `invited`
  agent. No derived-state divergence (Spike §9).
- **Ticket-leak threat (accepted MVP limitation).** As with any key-bound invite, a
  leaked agent ticket lets *the named agent key* join until expiry; there is no
  native revocation other than removing the subject (PRD §13.4 #10, §13.5 #1). The
  password-grade warning is printed (inherited from `print_invite`).

---

## 10. Implementation steps (for the executing engineer/agent)

1. **CLI enum + dispatch.** In `cli.rs`: add `Command::Agent { action: AgentAction
   }`; define `enum AgentAction { Invite { room_id, agent_id, expires } }` with the
   doc-comments from §5 (backtick-free, `#[allow(clippy::doc_markdown)]`); add the
   `Command::Agent` match arm calling `dispatch_agent`; implement `dispatch_agent`
   parsing `room_id` and delegating to `agent::invite` + `agent::print_agent_invite`.
   Update the `cli.rs` header surface doc-comment to list the new command.
2. **`agent.rs`.** Add the module (D2 recommended): `invite(home, room_id,
   agent_id_hex, expires)` → `crate::invite::invite(home, room_id, agent_id_hex,
   "agent", expires)`; `print_agent_invite(summary)` → `crate::invite::print_invite`
   (optionally with an agent-tailored `next:` line and the §13.3 not-implicitly-
   trusted note on stderr). Declare `mod agent;` in the crate root.
3. **Unit tests (`agent.rs` `#[cfg(test)]`).** Prove the wrapper pins role to
   `agent` and forwards args (e.g. a happy-path `invite` in a temp home whose
   persisted event decodes to `Content::MemberInvited { role: "agent", .. }` with
   `invitee_key == <AGENT_ID>`; a non-admin home errors "only … admin" and writes
   nothing; a bad `--expires` errors before IO).
4. **Integration tests (`tests/agent_cli.rs`, `assert_cmd`).** §11 cases.
5. **Docs reconcile (`docs/getting-started.md`).**
   - In the "Invite and join the Agent" step, replace
     `iroh-rooms room invite <ROOM_ID> --invitee <AGENT_ID> --role agent` with
     `iroh-rooms agent invite <ROOM_ID> <AGENT_ID>` (keep `--expires` note as
     optional). Update the expected-output block if the `next:` line changes.
   - In the reconciliation preamble: change the "Step 7 — `agent` is scaffold — the
     binary does not recognise it yet" note so it applies **only** to `agent
     status`; note that `agent invite` is implemented as of this issue (IR-0206).
   - Remove the `[reconcile]` marker for "the exact `agent invite`/join syntax"
     (line ~84) — the invite syntax is now settled; join is the shared `room join`.
   - Keep `.demo/agent` and the "agent requires explicit invite" narrative (the
     docs-conformance suite asserts them — do not remove).
6. **Verify.** `scripts/verify.sh` green (fmt, clippy `-D warnings` pedantic, all
   tests `--all-features`, including `docs_conformance` and the new `agent_cli`).

---

## 11. Test strategy

Mapping the issue Test Plan ("CLI/unit tests for agent identity, invite role, and
unauthorized access attempt") to concrete tests. All CLI integration tests use
`assert_cmd`, `predicates`, and a fresh `tempfile` `IROH_ROOMS_HOME` per test, with
`IROH_ROOMS_HOME` isolation (mirror the sibling suites).

### 11.1 Agent identity (AC1, AC4)

- **`agent_identity_has_distinct_identity_and_device_keys`:** `identity create
  --name build-agent` in a temp home exits 0; `identity show --json` yields a
  64-hex `identity_id` and a distinct 64-hex `device_id`
  (`identity_id != device_id`). (Proves an agent identity is created by the
  *shared* path and has its own device key.)
- **`agent_identity_is_structurally_identical_to_a_human_identity` (AC4):** create
  two identities (`alice`, `build-agent`) in two homes; assert both `identity show
  --json` outputs have the same set of fields (name/identity_id/device_id) and the
  same id shapes — i.e. nothing distinguishes an agent identity on disk or in
  `show`. The `agent` role is assigned only later, at invite time.

### 11.2 Agent invite role (AC2, admin-gate, secret hygiene)

- **`agent_invite_happy_path_mints_agent_role_ticket`:** in an admin home with a
  created room, `agent invite <ROOM_ID> <AGENT_ID>` exits 0; stdout has
  `role: agent`, `invitee: <AGENT_ID>`, `expires: never`, and a `roomtkt1…` line.
- **`agent_invite_persists_member_invited_with_agent_role`:** after the command,
  the persisted event decodes to `Content::MemberInvited { role: "agent",
  invitee_key == <AGENT_ID> }`, and `room members <ROOM_ID>` shows the agent as
  `status=invited`, `role=agent` (**AC2**). (Unit-level via `agent::invite`
  return + `room::members`, or CLI-level via `room members` stdout.)
- **`agent_invite_ticket_role_is_agent` (round-trip):** decode the emitted
  `roomtkt1…` via `RoomInviteTicket::from_str` and assert `ticket.role == "agent"`
  and `ticket.capability_hash()` equals the on-log `capability_hash`.
- **`agent_invite_requires_admin`:** a non-admin home (identity B holding only the
  genesis of A's room, or driven via two data-dirs) running `agent invite` exits
  non-zero with "only … admin" and leaves `rooms.db` unchanged.
- **`agent_invite_expires_encoded`:** `agent invite … --expires 24h` shows an
  absolute ISO-8601 expiry + `(in 24h)`; a bad `--expires` (`0h`, `5x`, empty)
  exits non-zero before IO.
- **`agent_invite_no_secret_in_output`:** capture stdout+stderr; read the on-log
  `capability_hash`; assert the secret material appears in neither stream (the only
  secret carrier is the ticket token). Mirror the `room invite` secret-leak test.
- **`agent_invite_self_invite_rejected`:** inviting the admin's own identity as an
  agent exits non-zero before IO.

### 11.3 Unauthorized access attempt (AC3 / PRD §13.3 #4)

Prove an agent that was **never invited** cannot access the room:

- **(fold/unit) `uninvited_agent_is_not_active`:** build a room log with genesis
  only (admin), fold it, and assert `snapshot.status(agent_id) == None` and
  `is_active(agent_id) == false` — an unknown/uninvited agent has zero
  capabilities. Then attempt a `member.joined` authored by the agent citing **no**
  invite (or a forged/absent capability) and assert the fold returns
  `Ingest::Rejected` (e.g. `NotAMember` / `BadCapability`), never `Accepted`.
- **(CLI, negative) `agent_join_without_invite_fails`:** an agent home attempting
  `room join` with a self-fabricated or wrong-key ticket exits non-zero (wrong
  identity is caught before network IO by the landed `room join` pre-check; a
  bad-secret / no-matching-invite ticket is rejected by `gate_join`). This
  exercises "cannot access a room without explicit invite" at the command surface.
- **(transport, optional/reuse)** The landed `net` admission suites already prove a
  non-member device is rejected before bytes; reference them rather than
  duplicating. Optionally add an `#[ignore]`-gated loopback case: an uninvited
  agent process dialing an admin's `room tail` session is refused
  (`peer … state=unauthorized`), mirroring the existing two-peer unauthorized-pipe
  proof. Keep any real-network path out of the always-green CI tier.

### 11.4 Docs conformance (keep green)

`tests/docs_conformance.rs` already asserts the guide documents `agent invite`
(`guide_documents_agent_invite_command`), the `.demo/agent` directory, the agent-
requires-explicit-invite narrative, and `room join <AGENT_TICKET>`. After the §10
step-5 reconcile, re-run the suite and confirm all pass; adjust the guide (not the
test) if any assertion drifts. **Do not weaken these assertions.**

Run everything under `scripts/verify.sh` (`--all-features`).

---

## 12. Risks & mitigations

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | Introducing a parallel agent identity/key path violates AC4 and forks the model. | high | D3: agents use the shared `identity create`; `agent invite` only *assigns* the role. No core change. A test asserts agent/human identities are structurally identical. |
| R2 | `agent invite` duplicates the invite orchestration and drifts from `room invite`. | medium | D1/D2: `agent invite` is a one-line delegate to `invite::invite(.., "agent", ..)`; no logic copied. |
| R3 | Over-claiming agent restrictions (e.g. "agents can't open pipes") that MVP doesn't enforce. | medium | §9: state plainly that in MVP the `agent` role adds no runtime restriction beyond a member's; the guarantee is *explicit invite* + display. Matches Spike §3.1 / PRD §13.3 exactly. |
| R4 | Docs-conformance breakage during the getting-started reconcile. | medium | §11.4: run `docs_conformance` after editing; keep `.demo/agent`, `agent invite`, and the explicit-invite narrative. Edit the guide, never the assertions. |
| R5 | `agent status` half-added (a stub `AgentAction::Status` that errors) confuses users. | low | §3.2: do **not** stub `agent status`; leave a comment noting the separate issue. The `agent` group ships with exactly one working arm. |
| R6 | Positional `<AGENT_ID>` vs `room invite`'s `--invitee` inconsistency surprises users. | low | Intentional PRD §16 fidelity; documented in `--help` and the guide. `agent invite` is the agent-ergonomic front door; `room invite --invitee` remains for general use. |
| R7 | Re-inviting an already-joined member as `agent` does not downgrade them (fold merges only concurrent heads). | low | §5/D5/§9: documented existing fold behavior; out of scope to change. `agent invite` warns on an already-active invitee (inherited). |
| R8 | Ticket leak grants an agent join. | medium (accepted) | Inherited key-bound + expiry + password-grade warning; documented MVP limitation (PRD §13.4 #10). No native revocation — remove the subject. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Where satisfied | Test |
|---|---|---|
| Agent has its own identity and device key. | Shared `identity create` (#16); distinct `sender_id`/`device_id` (Spike §1). | 11.1 `agent_identity_has_distinct_identity_and_device_keys`. |
| Agent role appears in membership state. | `member.invited{role="agent"}` → `Role::Agent` fold → `room members` `role=agent` (landed #12/#21). | 11.2 `agent_invite_persists_member_invited_with_agent_role`. |
| Agent cannot access a room without explicit invite. | Key-bound `gate_join` + default-deny snapshot + transport admission (landed #12/#19/#22). | 11.3 `uninvited_agent_is_not_active`, `agent_join_without_invite_fails`. |
| Human and agent identities via the same protocol model. | One identity/key/event/fold model; `agent` is one `role` value; no parallel path (D1/D3). | 11.1 `agent_identity_is_structurally_identical_to_a_human_identity`; the "no core change" invariant. |
| PRD §15.8 / §16 CLI surface (`agent invite <room-id> <agent-id>`). | New `agent` command group (D4/§5). | 11.2 happy-path; 11.4 docs-conformance `guide_documents_agent_invite_command`. |

---

## 14. Dependencies & sequencing

- **Hard deps (landed):** #16 (IR-0101, identity/device create + secret loader),
  #18 (IR-0103, key-bound invite: `invite::invite`, `RoomInviteTicket`,
  `member.invited` builder). Both in `main`.
- **Reuses (landed):** #17 (room create + store), #12 (fold + admin gate +
  `gate_join`), #19 (`room join`, agent joins already work), #21 (`room members`
  role display), #22/#9 (transport admission default-deny).
- **Enables (siblings, out of scope):** the `agent status` command (`agent.status`
  authoring) hangs off the `AgentAction` enum introduced here; PRD §17.1.2 ("one
  agent identity can join or be configured as a room participant") is demonstrated
  end-to-end once `agent invite` + `room join` are wired.
- **No dependency on live networking for the command itself:** `agent invite` is
  offline/local (like `room invite`); the persisted invite propagates via sync/net.
- The orchestrator handles all git/GitHub actions; no branch/PR work is part of
  this phase.

---

## 15. Assumptions

1. Agents are ordinary principals (Spike §1): one `sender_id` + one `device_id`
   per data-dir home, created by the shared `identity create`. An "agent identity"
   is not a distinct on-disk artifact.
2. The `member.invited` schema, `capability_hash`, `RoomInviteTicket`, admin/join
   fold gates, and `Role::Agent` semantics are **final** and unchanged by this
   issue (confirmed in `event/content.rs`, `event/invite.rs`, `ticket.rs`,
   `membership/`). `agent invite` only *produces* an agent-role invite via the
   landed path.
3. In MVP the `agent` role grants no additional or reduced *runtime* capability
   versus `member` (pipe/file access is identical); the enforced difference is
   admission-by-explicit-invite plus display (Spike §3.1, PRD §13.3). No room-
   policy engine exists yet.
4. `agent status` is owned by a separate issue and only *reserves* a slot in the
   `AgentAction` enum here.
5. The agent redeems its ticket via the shared `room join <ticket>`; no `agent
   join` verb is needed.
6. `<AGENT_ID>` positional (PRD §16) is the intended surface; role is fixed by the
   verb (no `--role`). `--expires` is a supported parity extension.

---

## 16. Open questions

- **OQ1 — `agent create` alias?** Should we add `agent create --name <NAME>` as a
  thin, discoverability-only alias of `identity create` (D3)? Recommend **no** for
  MVP (avoids implying agent identities are a distinct on-disk artifact and keeps
  one-identity-per-home clear); reconsider if user testing shows discoverability
  friction. If added, it MUST be a pure alias.
- **OQ2 — `agent.rs` module vs direct dispatch (D2).** Ship the thin `agent.rs`
  module (recommended, future home for `agent status`) or dispatch straight to
  `invite::*` from `cli.rs`? Decide before coding; behavior is identical.
- **OQ3 — Agent-tailored output (D6).** Reuse `invite::print_invite` verbatim, or
  add an agent `next:` hint + a §13.3 "not implicitly trusted" reminder? Recommend
  the small agent-tailored hint on stderr so stdout stays script-parseable.
- **OQ4 — `--expires` on `agent invite`.** PRD §16 shows `agent invite` **without**
  `--expires`. Keep the optional `--expires` for parity with `room invite`
  (recommended, zero cost) or omit it to match the PRD example exactly? Recommend
  keeping it and documenting it as an extension.
- **OQ5 — Unauthorized-access test depth.** Is the fold/unit + CLI-negative proof
  of AC3 sufficient, or should an `#[ignore]`-gated loopback "uninvited agent
  dial → unauthorized" case be added to the always-run suite's ignored tier
  (mirroring `two_peer_e2e`)? Recommend the fold/unit + CLI proof for CI, with the
  loopback case ignored-gated.
- **OQ6 — Getting-started demo flow.** After the reconcile, should the guide still
  show the agent joining via `room join <AGENT_TICKET>` (yes — shared join), and
  should the `agent status` step stay as the only remaining "scaffold" caveat
  (yes, until its own issue lands)?
```
