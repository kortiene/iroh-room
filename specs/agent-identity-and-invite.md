# Spec: Agent Identity as a First-Class Participant (IR-0206)

| | |
|---|---|
| **Issue** | #31 — `[IR-0206] Implement agent identity` |
| **Parent** | #3 |
| **Labels** | `type/feature` `type/security` `area/protocol` `area/agent` `priority/p1` `risk/medium` |
| **Dependencies** | #16 (IR-0101, identity/device CLI — **landed**), #18 (IR-0103, key-bound invite ticket — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.8, §13.3, §16; Spike `PHASE-0-SPIKE.md` Event Protocol §1 (Identity & Key Model), Membership & Ordering §3.1 (Roles), §3.5 (authorization gate) |
| **Owning crate** | `crates/iroh-rooms-cli` (app surface + tests). **`iroh-rooms-core` needs no change** — the agent role is already in the domain model. |
| **Status** | planning — this document is the build plan; no production code is modified by this phase. |

---

## 1. Summary

Represent an **agent** as a first-class room participant with its own identity
and device key, on exactly the same protocol substrate as a human. Concretely,
add the user-facing surface the PRD documents but the binary does not yet expose:

```text
iroh-rooms agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]
```

`agent invite` registers a **known agent identity** into a room as an `agent`-role
member. It is a **thin, delegating wrapper** over the already-landed key-bound
invite path (`invite::invite(..., role = "agent", ...)`, IR-0103): it draws no new
authorization decision, mints no new event type, and reuses the exact
`member.invited` builder, `capability_hash`, ticket codec, and admin gate. The one
observable difference from `room invite --invitee <ID> --role agent` is the
**noun and positional shape** — matching `PRD.v0.3.md` §15.8/§16
(`iroh-rooms agent invite <room-id> <agent-id>`) and making the agent a
discoverable, first-class CLI concept rather than a `--role` flag buried under
`room invite`.

**The important framing for the executing engineer:** *most of this issue is
already implemented.* The Event Protocol and membership fold treat an agent as an
ordinary principal distinguished only by its `role` string:

- `Role::Agent` is a landed enum variant (`membership::model`), ordered
  `Agent < Member < Admin` for the least-privilege merge (spike §3.8).
- `member.invited.role` / `member.joined.role` already accept `"agent"` (the
  content parser's `ROLES = ["member","agent","admin"]`), and `room invite`
  already lets an admin mint an `agent` invite (`INVITABLE_ROLES`).
- The membership gate already denies any non-Active principal (agent included)
  from authoring room content (`gate_active_member` → `NotAMember`), and the net
  admission gate already rejects an un-invited device before bytes.
- An agent's identity is created by the **same** `identity create` command a human
  uses (spike §1: "an agent is an ordinary participant with its own key").

So the four acceptance criteria are, at the protocol layer, **already true**. This
issue's deliverables are therefore: (1) the `agent` CLI noun so the documented
surface exists and the agent is first-class in the UX; (2) **explicit,
issue-scoped test coverage** proving the four ACs end-to-end (agent identity,
agent role in membership state, no implicit access, one shared protocol model); and
(3) **reconciling the getting-started guide** (which currently invites the agent
via `room invite --role agent` and flags `agent invite` as `[reconcile]`).

`agent status` (the agent posting an `agent.status` event) is a **sibling issue**
and is **out of scope here** — the README already tracks it separately, and the
`agent.status` content type already exists but has no authoring command.

---

## 2. Background & current repository state

### 2.1 What already exists (landed work this builds on)

- **Identity & device CLI (IR-0101, #16).** `iroh-rooms identity create --name
  <NAME>` generates a participant `sender_id` + a device `device_id` from the OS
  CSPRNG and persists them under the data-directory home; `identity show`
  prints them. There is **no** agent-specific flag, and per the IR-0101 spec
  (Assumption 6) none is needed: "Agent identities are created by the same
  `identity create` path … role is assigned later at invite time." The demo
  already creates a `build-agent` identity this way
  (`docs/getting-started.md`).
- **Key-bound invite (IR-0103, #18).** `iroh-rooms room invite <ROOM_ID>
  --invitee <IDENTITY_ID> [--role member|agent] [--expires <DURATION>]` is the
  admin-only authoring command. `INVITABLE_ROLES = ["member","agent"]`
  (`cli/src/invite.rs`) — `agent` is a first-class accepted role today; `admin`
  is rejected. The orchestration (`invite::invite`) folds the log to confirm the
  caller is the single immutable admin, draws `invite_id` + capability secret,
  computes `capability_hash`, builds + self-validates + persists a signed
  `member.invited`, and emits a `roomtkt1…` ticket. **This is the exact function
  `agent invite` delegates to.**
- **Room join (IR-0104, #19).** `room join <TICKET>` redeems an invite (agent or
  human, identically) and produces a `member.joined` carrying `role`. `gate_join`
  requires `join.role == invite.role` (spike §3.5), so an agent invite yields an
  `agent`-role member and nothing else.
- **Membership fold (IR-0008, #12).** `Role::{Agent,Member,Admin}` with the
  least-privilege lattice (§3.8). The snapshot exposes each member's `role`;
  `room members` / `room members --json` already render it (`build-agent … agent
  active` in the demo). `gate_active_member` returns `NotAMember` for any subject
  not Active in the event's ancestor view — the AC3 enforcement point at the
  protocol layer.
- **Net admission (IR-0005/IR-0107).** `SnapshotAdmission` resolves a QUIC-proven
  `device_id → identity → Active?` and rejects a non-member before `accept_bi()` —
  the AC3 enforcement point at the transport layer. An agent device with no
  live invite/join is rejected exactly like any other non-member.
- **`agent.status` content type.** `Content::AgentStatus` / `EventType::AgentStatus`
  exist and are validated (`event::content`), gated as an active-member action in
  the fold. There is **no** command to author one yet (sibling issue).

### 2.2 The real gap this issue closes

1. **No `agent` CLI noun.** The binary exposes `identity`, `room`, `pipe`, `file`
   (`cli.rs`). `PRD.v0.3.md` §16 documents `iroh-rooms agent invite <room-id>
   <agent-id>` and `iroh-rooms agent status <room-id> "…"`; neither is recognized.
   The getting-started guide calls `agent` "scaffold — the binary does not
   recognise it yet" and marks the "exact `agent invite`/join syntax" as
   `[reconcile]`.
2. **No issue-scoped conformance tests** that specifically prove *the agent* is a
   first-class, non-implicitly-trusted principal (the four ACs). The behavior is
   covered incidentally by IR-0103/IR-0104/IR-0008 tests using human members;
   IR-0206 owns the explicit agent-role assertions.
3. **A docs `[reconcile]` marker** to retire once the noun lands.

### 2.3 Spike / PRD facts that constrain the design

- **Agents are ordinary principals (spike §1, §3.1).** "An agent is a member with
  `role = "agent"`. Same membership rules; cannot invite/remove; can open pipes
  only when room policy + explicit authorization allow (PRD §13.3)." There is **no
  distinct principal type** — the same `sender_id`/`device_id`/device-binding rule
  applies. This is the direct basis of AC4 ("same protocol model").
- **Not implicitly trusted (PRD §13.3).** "Agents are first-class participants but
  should not be implicitly trusted." MVP agent security: own identity; joins only
  through explicit invite; events signed; cannot access rooms unless invited;
  cannot open pipes unless authorized; artifacts content-addressed. Every one of
  these is already enforced for the `agent` role by landed code — this issue
  proves it, it does not re-implement it.
- **Single immutable admin (spike §3.1).** Only the admin may invite. An agent can
  never invite or remove. `agent invite` is therefore admin-only, inheriting
  IR-0103's admin gate verbatim.
- **Key-bound invites only (spike §6).** `member.invited.invitee_key` is required;
  `agent invite <ROOM_ID> <AGENT_ID>` binds to `<AGENT_ID>` exactly as
  `room invite --invitee` does. No open/bearer agent tickets.
- **No agent-specific identity creation.** Because identity is role-agnostic and
  room-scoped role assignment happens at invite time, there is deliberately no
  `agent create` / `identity create --agent`. Adding one would fork the identity
  model and contradict AC4. (Considered and rejected — see D3 / OQ2.)

### 2.4 Workspace conventions to honor

- **Thin CLI glue over landed core**; validate-before-persist; errors → stderr +
  non-zero exit with the IR-0110 error taxonomy codes; success → stdout + exit 0.
- **Secret hygiene**: the capability secret already lives in a `Zeroizing` buffer
  inside `invite::invite`; the wrapper adds no new secret handling.
- **Lints**: `unsafe_code = "forbid"`, clippy `pedantic` `-D warnings`.
- **`scripts/verify.sh`** (fmt + clippy pedantic + tests `--all-features`) is the CI
  gate (see the `verify.sh is the real CI gate` project memory).

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A new `agent` CLI command group with **`agent invite <ROOM_ID> <AGENT_ID>
   [--expires <DURATION>]`**, delegating to the landed `invite::invite(...,
   "agent", ...)` path and printing the same ticket summary.
2. Wiring: `Command::Agent { action: AgentAction }`, dispatcher, `mod agent;`, and
   a thin `cli/src/agent.rs`.
3. **Explicit conformance tests** for the four ACs:
   - agent identity is a distinct principal with its own `sender_id`/`device_id`
     (AC1);
   - an invited-then-joined agent appears in membership state with `role = agent`
     (AC2);
   - an **un-invited** agent cannot access the room — its content event is
     rejected `NotAMember` and its device is rejected by admission (AC3);
   - the same `member.invited`/`member.joined`/fold model represents a human and
     an agent, differing only by the `role` attribute (AC4).
4. **Docs reconciliation**: update `docs/getting-started.md` Step 3 to use
   `agent invite <ROOM_ID> <AGENT_ID>`, retire the `agent invite` `[reconcile]`
   marker, and keep the `docs_conformance.rs` `guide_documents_agent_invite_command`
   assertion green (now backed by a real command).

### 3.2 Out of scope (sibling issues — do **not** implement here)

- **`agent status`** (authoring an `agent.status` event). Tracked separately
  (README "agent status … tracked separately"); the content type exists but the
  posting command and its tail rendering are a sibling under #3. Leave room in the
  `AgentAction` enum for it (D2).
- **Any change to the authorization model, event schema, membership fold, or
  admission.** Agent authorization is already deterministic and landed; this issue
  adds a CLI façade + proofs only.
- **A distinct agent principal type, agent-specific identity creation, or an
  `--agent` flag on `identity create`.** Rejected by AC4 and the spike (D3/OQ2).
- **Agent pipe/blob policy beyond the landed gates** (PRD §13.3 #5/#6). Agents
  already open pipes only when explicitly `--allow`-ed and share files via the same
  content-addressed path; no agent-specific policy engine in MVP (PRD §13.5 #7/#8
  roadmap).
- **Invite revocation, trust levels, `max_uses`** (PRD §13.5 — post-MVP).

### 3.3 Why the split is safe

The entire trust boundary for an agent — admin-only invitation, key-binding,
capability matching, expiry, sticky departure, active-member gating, and connect
admission — is **already enforced deterministically by the landed validator, fold,
and admission**, and is exercised for the `agent` role by IR-0103/IR-0104/IR-0008
tests. `agent invite` produces a byte-identical `member.invited` to
`room invite --role agent`; a buggy wrapper cannot grant access an admin did not
sign, because the join is authorized solely by the on-log invite + the ticket
secret. This issue is a **surface + proof** layer, so its blast radius is the CLI
crate and one doc.

---

## 4. Key design decisions

### D1 — `agent invite` is a delegating wrapper, not new authorization (recommended)

Add `crates/iroh-rooms-cli/src/agent.rs`:

```rust
/// Register a known agent identity into a room as an `agent`-role member.
///
/// A thin wrapper over the landed key-bound invite path: it is exactly
/// `room invite <ROOM_ID> --invitee <AGENT_ID> --role agent [--expires …]`, given
/// the `agent` noun and positional shape PRD §16 documents. No new event type,
/// no new authorization — the admin gate, capability hash, ticket codec, and
/// `member.invited` builder are all reused verbatim.
///
/// # Errors
/// Propagates every failure of [`crate::invite::invite`] unchanged (not admin,
/// no such room, self-invite, bad expiry, bad agent id, no local identity, …).
pub fn invite(
    home: &Path,
    room_id: &RoomId,
    agent_id: &str,          // the agent's identity id (64-hex), positional
    expires: Option<&str>,
) -> Result<crate::invite::InviteSummary> {
    crate::invite::invite(home, room_id, agent_id, "agent", expires)
}
```

Dispatch prints the returned `InviteSummary` via the existing
`invite::print_invite` (the summary already shows `role: agent`), so output is
identical to `room invite --role agent` — one code path, one format, one test
surface. The wrapper hard-codes `role = "agent"`, so no `--role` argument is
exposed on `agent invite`.

- **Why a wrapper, not a copy:** guarantees the agent invite is byte-identical to
  a human agent-role invite (AC4) and that any future hardening of `invite::invite`
  (revocation, richer discovery hints) applies to agents for free. Zero duplicated
  crypto/persist logic.
- **Alternative considered (rejected):** don't add a command at all — declare
  agent invitation "already served by `room invite --role agent`" and ship only
  tests + docs. This is *technically sufficient for the ACs* but leaves the PRD
  §15.8/§16 `agent invite <room-id> <agent-id>` surface unimplemented, keeps the
  agent a second-class `--role` flag, and forces the getting-started `[reconcile]`
  marker to stay. Recommendation: add the noun (it is ~30 lines of glue) so the
  documented surface and the "first-class participant" UX both hold. Flagged as
  OQ1 for the reviewer.

### D2 — `agent` noun with an extensible action enum (room for `agent status`)

```rust
#[derive(Debug, Subcommand)]
enum AgentAction {
    /// Invite a known agent identity into a room as an `agent`-role member.
    Invite {
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
        /// The agent's identity id (64-char lowercase hex from its `identity show`).
        agent_id: String,
        /// Optional expiry as <int>{s|m|h|d}, e.g. 24h.
        #[arg(long)]
        expires: Option<String>,
    },
    // `Status { .. }` lands in the sibling agent-status issue under this same noun.
}
```

`agent_id` is **positional** (matches `PRD.v0.3.md` §16 `agent invite <room-id>
<agent-id>`), whereas `room invite` uses `--invitee`. Both parse the same 64-hex
identity key; the positional form is the PRD's documented ergonomics for the agent
path. Keeping the enum non-exhaustive-in-spirit (one arm now) reserves the noun so
the sibling `agent status` command slots in without a surface change.

### D3 — Agent identity creation stays `identity create` (no new command)

AC1 ("agent has its own identity and device key") is satisfied by the **existing**
`identity create` (IR-0101), because an agent is an ordinary principal (spike §1).
This spec deliberately adds **no** `agent create` / `identity create --agent`:

- The identity keypair is role-agnostic; role is a **room-scoped** attribute
  assigned at invite time (spike §3.1). A creation-time agent flag would have no
  protocol meaning and would contradict AC4 ("same protocol model").
- The getting-started demo already creates the agent with `identity create --name
  "build-agent"`; documenting that path as *the* agent-identity-creation path
  (with a one-line note) is the honest reconciliation of the issue's "agent
  identity creation **or** registration command" — the **registration** command is
  `agent invite`; the **creation** command is `identity create`.

Rejected alternative (OQ2): an `agent create` alias for `identity create` for
discoverability. It buys a nicer demo narrative at the cost of two ways to make one
thing and a risk of drift. Recommend against; if wanted, make it a pure alias with
zero behavioral difference.

### D4 — AC3 ("no implicit access") is proven at both enforcement layers

The unauthorized-access proof exercises the two places the system fails closed:

1. **Protocol layer (fold).** An `agent`-key content event (e.g. `message.text`
   or `agent.status`) with **no** live `member.invited`/`member.joined` for that
   key in its ancestor view folds to `Ingest::Rejected(NotAMember)`
   (`gate_active_member`). A core unit test asserts this directly.
2. **Transport layer (admission).** A node dialing with an un-invited agent
   `device_id` is rejected by `SnapshotAdmission` before any event byte is read.
   Covered by reusing the existing admission decision-matrix test pattern with an
   agent device, or a CLI-level test where an un-invited agent's `room send`
   reaches the room and is denied membership (`not_a_member`, IR-0110 exit `3`).

This mirrors the spike's "membership is the authorization source; enforcement at
connect-accept + ancestor-fold" (§5), and makes AC3 a two-layer assertion rather
than a single happy-path negative.

### D5 — Error model & IR-0110 codes reused verbatim

`agent invite` surfaces the **same** coded errors as `room invite` (the wrapper
propagates them unchanged):

| Condition | Code / exit (IR-0110) |
|---|---|
| Bad `<AGENT_ID>` (not 64-hex / non-curve-point) | `invalid_argument` / `2` |
| Bad `--expires` (empty, `0`, no suffix, overflow) | `invalid_argument` / `2` |
| No local identity | `identity_not_found` / `2` |
| Unknown `<ROOM_ID>` (no events in store) | `room_not_found` / `2` |
| Caller is not the room admin | `insufficient_role` / `3` (AC: agent can't be invited by a non-admin) |
| Self-invite (agent id == caller identity) | `invalid_argument` / `2` |

No new taxonomy variant is introduced. The wrapper must **not** intercept or
re-map these — it delegates so the codes stay identical between `agent invite` and
`room invite --role agent` (the "same protocol model" property extends to the error
surface).

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]
```

- `<ROOM_ID>` — positional, `blake3:<64-hex>` (parsed via the shared
  `parse_room_id`).
- `<AGENT_ID>` — positional, 64-hex identity key of the agent (from the agent's
  `identity show`).
- `--expires <DURATION>` — optional, `<int>{s|m|h|d}` (same grammar as
  `room invite`).
- Exit `0` + the ticket summary on stdout on success (`role: agent`); coded stderr
  error + non-zero exit on failure, store untouched on every pre-persist path.

Wiring in `cli.rs`:

```rust
enum Command {
    Identity { .. }, Room { .. }, Pipe { .. }, File { .. },
    /// Invite and (later) drive agent participants.
    Agent { #[command(subcommand)] action: AgentAction },
}
```

with a `dispatch_agent(home, action)` mirroring `dispatch_file`:

```rust
fn dispatch_agent(home: &Path, action: AgentAction) -> Result<()> {
    match action {
        AgentAction::Invite { room_id, agent_id, expires } => {
            let room_id = parse_room_id(&room_id)?;
            let summary = agent::invite(home, &room_id, &agent_id, expires.as_deref())?;
            invite::print_invite(&summary);
        }
    }
    Ok(())
}
```

No async runtime is needed (invite is an offline, local command, like
`room invite`).

---

## 6. Module / file plan

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/src/agent.rs` | **new** — `invite(home, room_id, agent_id, expires) -> Result<InviteSummary>` delegating to `invite::invite`; `#[cfg(test)]` unit tests. |
| `crates/iroh-rooms-cli/src/cli.rs` | add `Command::Agent`, `AgentAction::Invite`, `dispatch_agent`; declare/`use` `agent`. |
| `crates/iroh-rooms-cli/src/main.rs` (or lib wiring) | `mod agent;` (mirror `mod invite;`). |
| `crates/iroh-rooms-cli/tests/agent_cli.rs` | **new** — `assert_cmd` integration suite (AC1–AC4, §11). |
| `docs/getting-started.md` | Step 3 "Invite and join the Agent" → `agent invite <ROOM_ID> <AGENT_ID>`; retire the `agent invite` `[reconcile]` note (§10 step 6). |
| `crates/iroh-rooms-core/**` | **no change** — `Role::Agent`, `build_member_invited`, fold, admission all landed. |

No new dependency in any crate: `agent invite` reuses `invite`'s deps.

---

## 7. Dependencies to add

**None.** The wrapper reuses `clap` (already present), `invite::invite`, and the
core primitives. Dev-deps for the new test file (`assert_cmd`, `predicates`,
`tempfile`, `assert_fs`/`serde_json` as used by sibling suites) are already in
`iroh-rooms-cli/Cargo.toml`.

---

## 8. Error model & observability

- **Delegation, not re-mapping (D5).** Every error is the `invite::invite` error
  with its IR-0110 code intact; `agent invite` adds no new failure surface. A test
  asserts a non-admin caller gets `insufficient_role` (exit `3`) and an unknown
  room gets `room_not_found` (exit `2`) — the same as `room invite`.
- **Success output** is the shared `print_invite` summary (`invite_id`, `room`,
  `invitee` = the agent key, `role: agent`, `expires`, `ticket:`, the
  password-grade warning, and the `next: … room join <ticket>` hint) — so an
  operator sees the agent is being authorized *as an agent*, and the audit record
  is the persisted `member.invited` event itself (subsequently visible as
  `status = invited`, then `role = agent / active` after the agent joins, in
  `room members`).
- **No secret leakage.** The capability secret stays in `invite::invite`'s
  `Zeroizing` buffer and appears only inside the ticket token; the wrapper touches
  no secret bytes. The existing secret-not-in-output test pattern is repeated for
  `agent invite`.

---

## 9. Security, privacy, reliability

- **Own identity & device key (AC1 / PRD §13.3 #1).** The agent runs `identity
  create`, producing a distinct `sender_id`/`device_id` bound per room via the
  device-binding certificate in its `member.joined` (spike §1). No shared or
  derived agent key.
- **Explicit invite only (AC3 / PRD §13.3 #2, #4).** An agent becomes a member
  solely via an admin-signed, key-bound `member.invited` for its key plus a
  `member.joined` descending from it (`gate_join`). No implicit access: an
  un-invited agent is `NotAMember` at the fold and rejected at admission (D4).
  Removal/leave is sticky (spike §3.7) for agents identically.
- **Not implicitly trusted (PRD §13.3).** `role = agent` is the **least**
  privileged concrete role (`Agent < Member < Admin`); the least-privilege merge
  (§3.8) means a concurrent `agent`/`member` grant for the same key resolves to
  `agent`, never up-privileging. Agents cannot invite/remove (admin-only gate) and
  open pipes only when explicitly `--allow`-ed (landed pipe ACL). This spec adds no
  capability an agent did not already have.
- **Signed events (PRD §13.3 #3).** Every agent event is signed by the agent's
  device key and validated by the stateless pipeline — unchanged.
- **Same protocol model (AC4).** A human member and an agent member are the same
  `member.invited`/`member.joined`/`Member` shapes differing only in the `role`
  string. `agent invite` produces a byte-identical event to `room invite --role
  agent`; a test asserts field-for-field equivalence.
- **Reliability / determinism.** No new state, RNG, or clock is introduced beyond
  what `invite::invite` already uses; restart determinism and convergence are
  inherited. The one advisory note from prior work applies unchanged: an agent (or
  any) member message must cite a parent whose ancestor view has the author Active,
  or it is silently `NotAMember` (see the `member-message-ancestor-view-gate`
  project memory) — relevant when writing the AC3 negative test so it fails for the
  *intended* reason.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **CLI module.** Add `crates/iroh-rooms-cli/src/agent.rs` with `pub fn
   invite(home, room_id, agent_id, expires) -> Result<invite::InviteSummary>`
   delegating to `invite::invite(home, room_id, agent_id, "agent", expires)` (D1).
   Keep it wafer-thin; add a module doc comment stating it is a façade over the
   landed key-bound invite path and that agent authorization is unchanged.
2. **Wire the noun.** In `cli.rs`: add `Command::Agent { action: AgentAction }`,
   the `AgentAction::Invite { room_id, agent_id, expires }` subcommand (D2),
   `dispatch_agent`, and `mod agent;` in the crate root. Print via
   `invite::print_invite`.
3. **CLI unit tests** (`agent.rs` `#[cfg(test)]`): `agent::invite` by the admin
   returns a summary with `role == "agent"` and the persisted event decodes to
   `Content::MemberInvited { role: "agent", invitee_key == <AGENT_ID>, .. }`; a
   non-admin caller errors `insufficient_role`; self-invite and bad expiry error
   before IO.
4. **CLI integration tests** (`tests/agent_cli.rs`, `assert_cmd`): the AC matrix in
   §11, driving the real binary against temp `IROH_ROOMS_HOME`s.
5. **Core conformance test (AC3 negative).** Add (or extend a sibling fold test
   with) an explicit case: an `agent.status`/`message.text` signed by an
   agent key with **no** live invite in its ancestor view ⇒
   `Ingest::Rejected(NotAMember)`; and the mirror admission case (un-invited agent
   device rejected) using the landed admission-matrix harness. Locate in
   `iroh-rooms-core` (fold) and/or `iroh-rooms-net` (admission) per where the
   sibling patterns live.
6. **Docs reconciliation.** Update `docs/getting-started.md` Step 3 to call
   `iroh-rooms agent invite <ROOM_ID> <AGENT_ID>` (keep the illustrative ticket
   output, now with `role: agent`), remove `agent invite` from the `[reconcile]`
   list (line ~84), and adjust the Step 7 scaffold note so it only covers `agent
   status` (still a sibling). Verify `docs_conformance.rs`
   `guide_documents_agent_invite_command` and
   `guide_uses_agent_ticket_in_room_join_command` stay green.
7. **Gate.** `scripts/verify.sh` green (fmt + clippy pedantic + tests
   `--all-features`).

---

## 11. Test strategy

Mapping the issue Test Plan ("CLI/unit tests for agent identity, invite role, and
unauthorized access attempt") and the four ACs to concrete tests:

**AC1 — agent has its own identity & device key** (`agent_cli.rs`):
- Create an agent identity in its own `IROH_ROOMS_HOME` via `identity create
  --name build-agent`; assert `identity show` prints a 64-hex `identity_id` and a
  **distinct** 64-hex `device_id`. Assert it differs from a separately-created
  human identity (distinct principals). (Reuses IR-0101 guarantees; this test
  pins the *agent* case explicitly.)

**AC2 — agent role appears in membership state** (`agent_cli.rs`, delegating +
membership):
- Admin `room create`; admin `agent invite <ROOM> <AGENT_ID>` exits `0` with
  `role: agent` and a `roomtkt1…` ticket. Decode the persisted `member.invited`
  (offline `room tail --offline --json` or the store) and assert `role == "agent"`
  and `invitee_key == <AGENT_ID>`.
- Drive the agent `room join <AGENT_TICKET>` (loopback, `--peer`, `#[ignore]`-gated
  online tier like `two_peer_e2e.rs`), then `room members --json` shows the agent
  with `role: agent`, `status: active`. The offline half (invite → `status:
  invited`) runs in the always-green CI tier.

**AC3 — agent cannot access a room without explicit invite** (core fold unit +
CLI/net):
- **Fold (core):** an `agent`-key `message.text`/`agent.status` whose ancestor view
  lacks a live invite ⇒ `Ingest::Rejected(NotAMember)` (D4 layer 1). Construct the
  negative so it fails for membership, not for a missing parent (see §9 memory
  note).
- **CLI/net:** an un-invited agent attempting to participate is denied — either a
  `room send` from a non-member agent returning `not_a_member` (exit `3`), and/or
  the admission-matrix assertion that an un-invited agent `device_id` is rejected
  before bytes (reuse the landed `SnapshotAdmission` test harness).

**AC4 — human and agent share one protocol model** (core/CLI equivalence):
- Assert `agent invite <ROOM> <ID>` and `room invite <ROOM> --invitee <ID> --role
  agent` produce a `member.invited` that is **field-identical** except for
  RNG-drawn fields (`invite_id`, capability secret/hash) and `created_at` — same
  `event_type`, same `role: "agent"`, same `invitee_key`, same builder path. A
  focused test can call `agent::invite` and `invite::invite(.., "agent", ..)` with
  injected fixtures and compare the decoded content shapes.
- Assert the folded `Member` for an agent and for a human differ **only** in
  `role` (both carry `identity`, `device`, `status`) — the "same model" invariant.

**Delegation / error parity** (`agent.rs` unit + `agent_cli.rs`):
- Non-admin `agent invite` → `insufficient_role` (exit `3`); unknown room →
  `room_not_found` (exit `2`); self-invite / bad `<AGENT_ID>` / bad `--expires` →
  `invalid_argument` (exit `2`) before any IO; store unchanged on each.
- Output carries **no** secret-seed material (repeat the invite secret-leak grep).
- `--data-dir` / `IROH_ROOMS_HOME` isolation honored.

Run under `scripts/verify.sh --all-features`; the online agent-join tier follows the
`two_peer_e2e.rs` `#[ignore]` + `--test-threads=1` loopback convention.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | Duplicating invite logic in `agent invite` drifts from `room invite` (two agent paths). | Wrapper **delegates** to `invite::invite`; no copied crypto/persist; equivalence test (AC4) pins byte-identical content. |
| R2 | Reviewer reads the issue as needing a distinct agent principal/identity type. | §1/§2.3/D3 make the spike-grounded case that an agent is an ordinary principal (role-only); AC4 forbids a separate type. Flagged OQ2. |
| R3 | AC3 negative test passes for the wrong reason (missing parent, not non-membership). | Construct the ancestor view so the author is genuinely non-member with parents present (see the `member-message-ancestor-view-gate` memory); assert the exact `NotAMember` reason. |
| R4 | Docs reconciliation breaks `docs_conformance.rs`. | Keep the `agent invite` / `<AGENT_TICKET>` / `room join <AGENT_TICKET>` substrings the conformance test pins; run the suite in the gate (§10 step 6). |
| R5 | `agent status` (sibling) later needs a conflicting `agent` surface. | Reserve the `AgentAction` enum now (D2); coordinate the subcommand name with the sibling issue. |
| R6 | Positional `<AGENT_ID>` vs `room invite`'s `--invitee` confuses users. | Positional matches PRD §16 exactly; `agent invite --help` documents it; both accept the same 64-hex key. |
| R7 | Perception that IR-0206 "does nothing" because the protocol already supports agents. | The value is the documented surface + first-class UX + explicit four-AC proofs + docs reconciliation; §1 states this framing plainly so it is not mistaken for redundant work. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Where satisfied | Test |
|---|---|---|
| Agent has its own identity and device key. | `identity create` (IR-0101) — agent is an ordinary principal (D3, spike §1). | AC1 test: distinct `identity_id`/`device_id`, distinct from a human identity. |
| Agent role appears in membership state. | `Role::Agent` (landed) surfaced via `agent invite` → `member.invited{role:agent}` → fold → `room members`. | AC2 tests: persisted `role: "agent"`; `room members --json` shows `role: agent`. |
| Agent cannot access a room without explicit invite. | `gate_active_member` → `NotAMember` (fold) + `SnapshotAdmission` reject (net); both landed (D4). | AC3 tests: fold `NotAMember` for un-invited agent; admission/`room send` denial. |
| Human and agent identities represented through the same protocol model. | Identical `member.*` events + `Member` shape, differing only in `role` (D1/§9). | AC4 tests: `agent invite` vs `room invite --role agent` content equivalence; `Member` differs only by `role`. |

---

## 14. Dependencies & sequencing

- **Hard deps (landed):** #16 (IR-0101 identity/device CLI — agent identity
  creation) and #18 (IR-0103 key-bound invite — the delegated path). Both in
  `main`.
- **Reuses (landed):** IR-0008 fold (`Role::Agent`, `gate_active_member`), IR-0104
  join (`gate_join` role match), IR-0107 admission, IR-0110 error taxonomy.
- **Sibling (out of scope, coordinate surface):** the `agent status` authoring
  command lands under the same `agent` noun; reserve `AgentAction` here.
- **Unblocks / enables:** the getting-started demo's agent step becomes runnable
  end-to-end with the documented `agent invite` surface; the two-peer-plus-agent
  Phase-1A workflow (PRD §19 deliverable, §17.1 metric 2) gains a first-class agent
  command.
- The orchestrator handles all git/GitHub actions; no branch/PR work is part of
  this phase.

---

## 15. Assumptions

1. An agent is an ordinary principal distinguished only by `role = "agent"` (spike
   §1/§3.1); no distinct type, no agent-specific identity creation (D3).
2. Agent identity creation is the existing `identity create` path (IR-0101
   Assumption 6); the agent shares its `identity_id` out-of-band and the admin binds
   the invite to that key.
3. `agent invite` is admin-only (single immutable admin, spike §3.1), inheriting
   IR-0103's admin gate; a non-admin caller is rejected `insufficient_role`.
4. `INVITABLE_ROLES` already contains `agent`, so no core change is needed to mint
   an agent invite; the wrapper only fixes the noun and positional shape.
5. `agent status` is a separate issue; this spec reserves the `agent` noun but
   implements only `invite`.
6. The online agent-join assertions run in the same loopback `#[ignore]`-gated tier
   as `two_peer_e2e.rs`; the offline invite/fold assertions run in the always-green
   CI tier.

---

## 16. Open questions

- **OQ1 — Add the `agent` noun, or tests+docs only?** Recommended: add `agent
  invite` (D1) so PRD §15.8/§16 surface exists and the agent is first-class. The
  minimal alternative (declare `room invite --role agent` sufficient; ship only
  tests + docs) satisfies the ACs but leaves the documented command and the
  getting-started `[reconcile]` unaddressed. Decide before coding.
- **OQ2 — `agent create` alias?** Recommended against (two ways to create one
  identity, drift risk); `identity create` is the agent-identity path. If wanted,
  make it a pure zero-behavior alias.
- **OQ3 — Positional `<AGENT_ID>` vs `--agent`/`--invitee`.** Spec uses positional
  to match PRD §16 (`agent invite <room-id> <agent-id>`). Confirm the PRD form is
  the desired ergonomics vs mirroring `room invite --invitee`.
- **OQ4 — `agent invite` expiry & discovery hints.** MVP inherits `room invite`'s
  `--expires` and admin-`device_id` discovery hint verbatim. Confirm no
  agent-specific defaults (e.g. shorter default expiry for automated agents) are
  wanted; recommend none for MVP.
- **OQ5 — Coordinate with the `agent status` sibling** on the `AgentAction`
  subcommand naming and whether `agent invite` and `agent status` should share any
  online-session plumbing. Recommend keeping `invite` fully offline (as `room
  invite` is) and letting `agent status` own its own online path.
- **OQ6 — Docs conformance surface.** Confirm the getting-started Step 3 edit keeps
  every substring `docs_conformance.rs` pins (`agent invite`, `<AGENT_TICKET>`,
  `room join <AGENT_TICKET>`) so the guide stays green while becoming runnable.
