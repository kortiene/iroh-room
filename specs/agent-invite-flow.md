# Spec: Agent Invite Flow — end-to-end invite → join → capability verification (IR-0207)

| | |
|---|---|
| **Issue** | #32 — `[IR-0207] Implement agent invite flow` |
| **Parent** | #3 |
| **Labels** | `type/feature` `type/security` `area/cli` `area/agent` `priority/p1` `risk/medium` |
| **Dependencies** | #31 (IR-0206, agent identity + `agent invite` noun — **landed**), #18 (IR-0103, key-bound invite ticket — **landed**), #19 (IR-0104, room join by ticket — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.8, §16, §13.3; Spike `PHASE-0-SPIKE.md` Membership & Ordering §6 (key-bound invites), §3.5 (join gate), §3.8 (least-privilege merge) |
| **Owning crate** | `crates/iroh-rooms-cli` (integration tests + docs). **No production-code change is expected in any crate** — see §1 and §3.2. |
| **Status** | planning — this document is the build plan; no production code is modified by this phase. |

---

## 1. Summary (read this first)

**This issue is almost entirely satisfied by already-landed work, and its remaining
deliverable is a focused, flow-level *integration test* — not a feature.** The
executing engineer should read §2.2 (what already exists) and §2.3 (the one genuine
gap) before writing a line of code, and should **not** re-implement the `agent
invite` command or duplicate the IR-0206 test matrix.

The issue asks to "allow a room admin to invite an agent participant explicitly,"
with a key-bound `role = agent` invite and an agent join that uses **the same
capability verification as a human peer**. Every piece of that is landed:

- `iroh-rooms agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` is a shipped
  command (IR-0206 / #31, `crates/iroh-rooms-cli/src/agent.rs`) — a thin, delegating
  wrapper over the key-bound invite path (`invite::invite(.., "agent", ..)`,
  IR-0103). It mints a byte-identical `member.invited{role:"agent"}` to
  `room invite --invitee <ID> --role agent`.
- The **agent join** path is the *same* `room join <TICKET>` path a human uses
  (IR-0104 / #19). Capability verification (`gate_join`) is **role-agnostic**: it
  checks the key binding, the capability-secret → `capability_hash` match, expiry,
  and `join.role == invite.role` — with no branch on whether the role is `agent`.
  A bad-secret/expired/wrong-identity agent join is rejected by the *identical* code
  that rejects a human's.
- `Role::Agent` is a landed membership enum variant (`Agent < Member < Admin`
  least-privilege merge, spike §3.8); `room members` / `room members --json`
  render it.

So the four acceptance criteria are, at the protocol/CLI layer, **already true and
mostly already proven** (see the coverage audit in §2.2). The **one leg the landed
tests do not exercise through the agent surface** is IR-0207 AC3 — *"agent join is
rejected without valid capability"* / the Test Plan's *"bad ticket"* — which
IR-0206's `agent_e2e.rs` module doc **explicitly declines** to test at the wire
level ("AC3 … is deliberately **not** re-tested here"), on the correct reasoning
that capability verification is role-agnostic and already covered for the `member`
role in `iroh-rooms-net/tests/join_e2e.rs`.

**IR-0207's deliverable is therefore a cohesive, issue-traceable integration-test
suite for the *agent invite flow*** that (a) proves all four Test-Plan legs — admin
invite, agent join, non-admin rejection, **bad ticket / invalid capability** — end
to end through the `agent invite` → `room join` surface, and (b) closes the
capability-rejection gap for the agent path that IR-0206 deferred. It adds **no new
authorization logic, no new event type, and no new command**.

> **Recommendation / top open question (OQ1):** Because ~85% of this issue is
> delivered by IR-0206, confirm the intended framing before coding: IR-0207 is the
> *flow-level conformance proof* (this spec's plan), **not** a re-implementation. If
> the reviewer instead reads #32 as fully subsumed by #31, it can be closed as
> substantially-satisfied with a one-line pointer to the IR-0206 tests + the single
> `bad_capability` agent-flow test added here. This spec plans the former (the
> safer, testable interpretation). See §16 OQ1.

`agent status` (the agent authoring an `agent.status` event) remains a **sibling
issue** and is out of scope (§3.2).

---

## 2. Background & current repository state

### 2.1 The flow this issue is about

The end-to-end "agent invite flow" is four legs:

```text
1. admin:  iroh-rooms agent invite <ROOM_ID> <AGENT_ID>   → roomtkt1… ticket   (admin-only)
2. agent:  iroh-rooms room join <TICKET> --peer <ADMIN>   → member.joined{role:agent}, Active
3. non-admin: iroh-rooms agent invite …                   → rejected (insufficient_role)
4. bad ticket / no valid capability: room join <BAD>      → rejected, no membership granted
```

Legs 1–3 are the *invite* side (offline, admin-gated). Leg 2's happy path plus leg 4
are the *join* side — where the agent's capability is verified **exactly as a
human's is**. The dependency on #19 (room join), which IR-0206 did **not** list, is
the tell that IR-0207's center of gravity is the join/verification side.

### 2.2 What already exists — coverage audit (the load-bearing table)

| IR-0207 AC / Test-Plan leg | Landed enforcement | Landed test(s) | Gap? |
|---|---|---|---|
| **AC1** Admin can invite an agent by identity key | `agent::invite` → `invite::invite(.., "agent", ..)`, admin gate + key-bound `member.invited` (IR-0206/IR-0103) | `agent_cli.rs::agent_invite_exits_zero_with_agent_role`, `…_appears_in_members_as_agent`, `…_ticket_decodes_with_agent_role`; `agent.rs` unit tests | **None** |
| **AC2** Non-admin cannot invite an agent | admin-only gate in `invite::invite` (single immutable admin, spike §3.1) → `insufficient_role` | `agent_cli.rs::agent_invite_by_non_admin_is_rejected` (asserts store byte-unchanged) | **None** |
| **AC3** Agent join is **rejected without valid capability** | `gate_join` (role-agnostic): `BadCapability` / `ExpiredInvite` / `InsufficientRole`; CLI pre-check `wrong_identity`; ticket-codec `ticket_*` | Proven only for the **`member`** role: `net/tests/join_e2e.rs::bad_capability_secret_join_not_accepted`, `…::expired_invite_join_not_accepted`; generic ticket/identity: `join_cli.rs::{join_garbage_ticket…, join_truncated_token…, join_wrong_identity…}` | **YES — not proven through the *agent* invite flow** (see §2.3) |
| **AC4** Agent appears as role `agent` in `room members` | `Role::Agent` fold → `room members` / `--json` | `agent_cli.rs::agent_invite_appears_in_members_as_agent`; online `agent_e2e.rs::agent_joins_and_converges_with_agent_role` (`#[ignore]`) | **None** |
| **Test Plan** admin invite | as AC1 | as AC1 | None |
| **Test Plan** agent join (happy path) | `room join` (IR-0104), role-agnostic | `agent_e2e.rs::agent_joins_and_converges_with_agent_role` (gated) | None |
| **Test Plan** non-admin rejection | as AC2 | as AC2 | None |
| **Test Plan** **bad ticket** | ticket codec + `gate_join` + join pre-check | *member*-role / generic only (above) | **YES** |

**Landed files this issue builds on:**

- `crates/iroh-rooms-cli/src/agent.rs` — the `agent invite` wrapper (152 lines).
- `crates/iroh-rooms-cli/src/invite.rs` — `invite::invite(home, room, invitee, role, expires)`; the admin gate, `capability_hash`, ticket mint.
- `crates/iroh-rooms-cli/src/join.rs` — `join::join(...)`: ticket decode (fail-closed) + key-binding **pre-check** (`self_id != ticket.invitee_key` → `wrong_identity`, before any IO), build+self-validate+fold-check `member.joined`, and `join_reject_message(RejectReason)` mapping `BadCapability`/`ExpiredInvite`/`InsufficientRole` to secret-free messages.
- `crates/iroh-rooms-cli/tests/agent_cli.rs` — the IR-0206 offline AC matrix (invite → `status: invited`).
- `crates/iroh-rooms-cli/tests/agent_e2e.rs` — the IR-0206 online agent-join **happy path** (`#[ignore]`, loopback), whose doc **defers** the capability-rejection leg.
- `crates/iroh-rooms-net/tests/join_e2e.rs` — the online capability-rejection proofs (`bad_capability`, `expired_invite`) for the **member** role, at the Node layer.
- `crates/iroh-rooms-cli/tests/join_cli.rs` — offline, network-free join failures (garbage/truncated ticket, wrong identity), role-neutral.

### 2.3 The genuine gap this issue closes

The capability-verification leg (AC3 / "bad ticket") is proven for the **`member`**
role and via **role-neutral** ticket-decode tests, but **never asserted end-to-end
through the `agent invite` → agent `room join` surface**. Concretely, no test today:

1. Takes a real `roomtkt1…` ticket minted by **`agent invite`** and shows that a
   **structurally corrupted** copy of it is rejected (exit 5, `ticket_*`) with no
   membership granted.
2. Takes an **agent** ticket and shows that redeeming it under the **wrong
   identity** (not the bound `<AGENT_ID>`) is rejected pre-IO (exit 3,
   `wrong_identity`).
3. (Online) Shows an **agent** join with a **wrong capability secret** or an
   **expired** agent invite is rejected by `gate_join` (`bad_capability` /
   `expired_invite`) and the agent is **not** made `Active` on the admin — the exact
   `join_e2e.rs` proof, but for the `agent` role and the agent CLI surface.

Because `gate_join` is role-agnostic, this is a **conformance proof**, not a bug
fix — but the issue exists precisely to make that guarantee explicit for the agent,
the PRD's least-trusted principal (§13.3: "Agents are first-class participants but
should not be implicitly trusted"). Making it a named, agent-flavored test also
guards against a future refactor accidentally special-casing the `agent` role.

### 2.4 Spike / PRD facts that constrain the design

- **Key-bound invites only (spike §6).** `member.invited.invitee_key` is REQUIRED;
  the agent invite binds to `<AGENT_ID>` exactly as a human invite binds to
  `--invitee`. Open/bearer tickets are excluded from MVP (spike §6(B)) because they
  defeat sticky removal. `capability_hash = BLAKE3-256("iroh-rooms:invite:v1" ‖
  room_id ‖ invite_id ‖ secret)`, secret ≥16 bytes, out-of-band in the ticket only.
- **Join gate is capability-bound and role-checked (spike §3.5, §6, event schema).**
  A `member.joined` validates iff: the referenced invite exists, is unexpired, its
  `invitee_key == sender_id`; the recomputed `capability_hash` matches; `role ==
  invite.role`; the device binding verifies; and the authorizing invite is still
  live in the join's ancestor view (not consumed by a prior removal/leave). Any
  failure ⇒ the join is dropped, never persisted or re-broadcast (`bad_capability`,
  `expired_invite`, `insufficient_role`, …). **None of these branch on `agent`.**
- **Agents are ordinary principals, not implicitly trusted (PRD §13.3, spike §1).**
  Own identity; join only through explicit invite; events signed; no room access
  unless invited; least-privileged concrete role. IR-0207 proves items 1, 2, 4 for
  the agent explicitly; it adds no capability an agent did not already have.
- **Single immutable admin (spike §3.1).** Only the admin invites; an agent can
  never invite/remove. `agent invite` inherits IR-0103's admin gate.
- **Advisory-clock-free expiry (IR-0103/IR-0104).** Expiry is enforced by
  `gate_join` comparing `join.created_at` vs `invite.expires_at` on the log, not by
  a local wall clock — so the expired-invite test is deterministic (see
  `join_e2e.rs`, which injects `created_at`).

### 2.5 Workspace conventions to honor

- **Thin tests over landed behavior**; drive the **real binary** with `assert_cmd`
  for CLI legs, and the Node layer with `#[tokio::test]` for online capability legs,
  mirroring the sibling suites exactly.
- **Two-tier online testing** (see the two-peer suite): always-green, network-free
  CI tier + an `#[ignore]`-gated loopback tier run with
  `-- --ignored --test-threads=1`. The `--loopback` hidden flag = `RelayMode::Disabled`
  + `presets::Minimal`; no relay, no discovery, no central server.
- **IR-0110 error taxonomy** codes are the assertion surface: exit `2` usage, `3`
  auth (`insufficient_role`, `wrong_identity`, `bad_capability`, `expired_invite`),
  `5` ticket (`ticket_bad_checksum`, `ticket_truncated`, …). Assert on the **code**,
  not prose.
- **Secret hygiene**: the capability secret lives in `Zeroizing` in `join`/`invite`;
  no test may print or assert it into an error stream. Reuse the existing
  secret-not-in-output grep pattern.
- **`scripts/verify.sh`** (fmt + clippy `pedantic -D warnings` + tests
  `--all-features`) is the CI gate (`verify.sh is the real CI gate` memory);
  `unsafe_code = "forbid"`.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A **cohesive agent-invite-flow integration suite** (recommended: a new
   `crates/iroh-rooms-cli/tests/agent_invite_flow.rs`, or a documented extension of
   `agent_e2e.rs` — see D1) proving the four Test-Plan legs through the agent CLI
   surface:
   - **Leg 1 (admin invite):** `agent invite` by the admin mints an
     `agent`-role, key-bound ticket (thin re-assertion for flow completeness;
     defers to IR-0206 for the exhaustive matrix — D2).
   - **Leg 2 (agent join, happy path):** an invited agent redeems its ticket and
     converges to `role: agent, status: active` on both peers (online, gated).
   - **Leg 3 (non-admin rejection):** a non-admin `agent invite` fails
     `insufficient_role` (exit 3), store unchanged.
   - **Leg 4 (bad ticket / invalid capability) — the new coverage:**
     - **4a (offline, always-green):** a **corrupted** agent ticket → exit 5
       `ticket_*`, no membership; an agent ticket redeemed under the **wrong
       identity** → exit 3 `wrong_identity`, pre-IO, no membership.
     - **4b (online, gated):** an agent join with a **wrong capability secret** →
       `bad_capability`, and an **expired** agent invite → `expired_invite`; the
       agent is **not** `Active` on the admin afterward.
2. **Traceability**: each test names its IR-0207 AC / Test-Plan leg in a doc comment,
   and the module header carries the AC → test table (mirroring `agent_e2e.rs`).
3. **Docs touch-up (light)**: confirm `docs/getting-started.md` and
   `docs_conformance.rs` remain green; add, if useful, a one-line note that a
   bad/expired agent ticket is rejected identically to a human's (no `[reconcile]`
   markers are expected to remain — IR-0206 retired them). No behavioral doc change.

### 3.2 Out of scope (do **not** implement here)

- **Any production-code change** — no new command, event type, gate, admission
  logic, or ticket field. `agent invite`, `room join`, `gate_join`, the ticket
  codec, and `SnapshotAdmission` are all landed and role-agnostic. If the executor
  finds themselves editing a `src/*.rs` outside `#[cfg(test)]`, they have
  misread the scope — stop and re-read §2.
- **Re-doing the IR-0206 offline AC matrix** (`agent_cli.rs`) or the IR-0206 online
  happy-path convergence (`agent_e2e.rs`). Reference them; don't duplicate them.
- **`agent status`** (authoring an `agent.status` event) — sibling issue under #3;
  the content type exists but has no authoring command.
- **A distinct agent principal type / agent-specific identity creation / `--agent`
  flag** — rejected by the "same protocol model" invariant and the spike (see the
  IR-0206 spec D3).
- **New net-layer *agent-role* capability production code.** The capability check is
  role-agnostic; leg 4b is a **test** mirroring `join_e2e.rs`, optionally in the net
  crate (D3), not a code change.
- **Invite revocation, trust levels for agents, `max_uses`, room-level pipe policy**
  (PRD §13.5 roadmap — post-MVP).

### 3.3 Why a thin, test-only issue is safe and sufficient

The entire agent trust boundary — admin-only invitation, key-binding, capability
matching, expiry, role match, sticky departure, active-member gating, connect
admission — is **already enforced deterministically by the landed validator, fold,
join gate, and admission**, and every branch of it is role-agnostic (proven for the
`member` role in `join_e2e.rs`). A buggy test cannot grant an agent access the admin
did not sign, because the join is authorized solely by the on-log invite + the
ticket secret, re-checked by every peer. IR-0207's blast radius is therefore the CLI
crate's `tests/` directory (and optionally one net-crate test), plus at most a
documentation line.

---

## 4. Key design decisions

### D1 — Package the flow as one traceable suite (recommended: `tests/agent_invite_flow.rs`)

Create a **new** integration test file
`crates/iroh-rooms-cli/tests/agent_invite_flow.rs` that owns the IR-0207 four-leg
matrix, rather than scattering the new cases across `agent_cli.rs` /
`agent_e2e.rs` / `join_cli.rs`.

- **Why a new file:** IR-0207 is a distinct issue with a distinct Test Plan; a
  single file keyed to it gives a clean traceability anchor (grep `IR-0207`), houses
  the online + offline legs behind one module doc with the AC table, and avoids
  bloating the IR-0206 files whose docs already declare their own scope boundaries.
  It reuses the `ChildSession` / `one_shot` / `parse_listening` helpers — **factor
  them into a shared `tests/common/` module** if that avoids a third copy (they are
  currently duplicated between `agent_e2e.rs` and `two_peer_e2e.rs`); otherwise a
  local copy is acceptable and matches the existing pattern (see OQ3).
- **Alternative (acceptable): extend `agent_e2e.rs`.** Add leg 4b's two online
  rejection tests next to `agent_joins_and_converges_with_agent_role`, and leg 4a's
  offline tests to `agent_cli.rs`, updating both module docs to retract the
  "deliberately not re-tested" note. This is lower-friction but splits IR-0207 across
  two IR-0206 files. **Recommend D1's dedicated file** for traceability; either is
  correct. Decide in review (OQ2).

### D2 — Legs 1–3 are thin re-assertions; leg 4 is the substance

Legs 1 (admin invite), 2-happy (join converges), and 3 (non-admin) are already
exhaustively covered by IR-0206. In `agent_invite_flow.rs` they appear as **one
concise assertion each**, present so the suite reads as a complete flow and so a
reader sees all four Test-Plan legs in one place — explicitly deferring the
exhaustive edge matrix to the IR-0206 files via a doc pointer. The engineering
effort concentrates on **leg 4** (§2.3), the genuine new coverage.

- Rationale: duplicating IR-0206's ~20 offline assertions would add maintenance cost
  and zero guarantee. The flow suite's value is proving the *legs compose* and that
  the *rejection* legs hold for the agent, not re-litigating the invite matrix.

### D3 — Leg 4b (online capability rejection): CLI-surface tier, optional net mirror

The online capability-rejection cases (wrong secret, expired) are inherently online
(a live admin must run `gate_join` and refuse to store the join). Two placements:

1. **CLI-surface (recommended, primary):** in `agent_invite_flow.rs`'s gated tier,
   an agent redeems a tampered/expired agent ticket over loopback against a live
   `room tail --accept-joins`; assert the join command exits non-zero with the
   coded message (`bad_capability` / `expired_invite`) **and** that the admin's
   `room members --json` never shows the agent `active`. This proves the *whole
   agent flow*, CLI to fold, which is what "integration test … agent join … bad
   ticket" asks for.
2. **Net-layer mirror (optional):** an agent-role variant of
   `join_e2e.rs::bad_capability_secret_join_not_accepted` /
   `…::expired_invite_join_not_accepted`, built by flipping the fixture role to
   `agent`. Cheap (it's a parameter), deterministic, and lives beside the member
   proof. Add **only if** the reviewer wants the guarantee pinned at the Node layer
   independent of the CLI (OQ4); the CLI-surface tier already covers the AC.

**Constructing "expired" deterministically:** follow `join_e2e.rs` — inject the
invite's `expires_at` and the join's `created_at` so `created_at > expires_at`; do
not sleep or read a wall clock. For the CLI tier this means minting the agent invite
with a `--expires` in the far past is *not* directly possible (durations are
future-relative), so the expired case is best expressed at the net/fixture layer
(D3.2) or by driving `join`'s builder with an injected clock in a `#[cfg(test)]`
seam — prefer the net-layer mirror for the expired case specifically (see R3/OQ4).

### D4 — "Bad ticket" is a family; cover the deterministic, agent-flavored members

"Bad ticket" (Test Plan) spans four failure classes; IR-0207 covers the ones that
are **deterministic** and **meaningful through the agent flow**:

| Bad-ticket class | Code / exit | Where | Tier |
|---|---|---|---|
| Structurally corrupt (bad base32/checksum/prefix/truncated) | `ticket_*` / 5 | agent ticket, 1 char mutated → `room join` | offline, always-green |
| Valid structure, **wrong identity** (redeemer ≠ bound agent key) | `wrong_identity` / 3 | agent ticket redeemed by a 2nd identity | offline (pre-IO), always-green |
| Valid ticket, **wrong capability secret** | `bad_capability` / 3 | requires a live admin to reject | online (gated) / net mirror |
| Valid ticket, **expired** invite | `expired_invite` / 3 | log-clock comparison | net mirror (deterministic) |

The first two are always-green and network-free (the ticket codec + the join
pre-check run before any dial). The last two are the online `gate_join` proofs.

### D5 — Error model & IR-0110 codes reused verbatim

Every rejection asserts the **existing** taxonomy code — no new variant:

| Condition (agent flow) | Code / exit |
|---|---|
| Non-admin `agent invite` | `insufficient_role` / 3 |
| Corrupt agent ticket | `ticket_bad_checksum` / `ticket_truncated` / … / 5 |
| Agent ticket redeemed under wrong identity | `wrong_identity` / 3 (pre-IO) |
| Agent join, wrong capability secret | `bad_capability` / 3 |
| Agent join, expired invite | `expired_invite` / 3 |
| Agent join, role mismatch (defensive) | `insufficient_role` / 3 |

Assertions target `error[<code>]:` on stderr (or the `join_reject_message` text
which embeds the code, e.g. `(bad_capability)`), matching `error_taxonomy_e2e.rs`.

---

## 5. Test-surface (precise)

No new CLI surface. The suite drives the already-shipped commands:

```text
iroh-rooms [--data-dir <P>] identity create --name <NAME>
iroh-rooms [--data-dir <P>] room create <NAME>
iroh-rooms [--data-dir <P>] agent invite <ROOM_ID> <AGENT_ID> [--expires <DUR>]
iroh-rooms [--data-dir <P>] room tail <ROOM_ID> --accept-joins --loopback        # admin host (gated tier)
iroh-rooms [--data-dir <P>] room join <TICKET> --peer <ADDR> --loopback          # agent (gated tier)
iroh-rooms [--data-dir <P>] room join <BAD_OR_WRONG_IDENTITY_TICKET>             # offline reject legs
iroh-rooms [--data-dir <P>] room members <ROOM_ID> [--json]
```

Ticket corruption helper (offline leg 4a): decode the printed `roomtkt1…` token,
flip one character in the base32 body (or truncate it), and feed it to `room join`;
assert exit 5 with a `ticket_*` code and that no `rooms.db` membership row appears.

---

## 6. Module / file plan

| File | Change |
|---|---|
| `crates/iroh-rooms-cli/tests/agent_invite_flow.rs` | **new** — the IR-0207 four-leg matrix (D1). Always-green legs 1, 3, 4a; `#[ignore]`-gated online legs 2-happy, 4b. Module doc carries the AC → test table + pointers to the IR-0206 files for the exhaustive matrices. |
| `crates/iroh-rooms-net/tests/join_e2e.rs` | **optional** (D3.2/OQ4) — add `agent`-role variants of `bad_capability_secret_join_not_accepted` / `expired_invite_join_not_accepted` (fixture role flip). |
| `crates/iroh-rooms-cli/tests/agent_e2e.rs` | **doc-only, optional** — if D1's file is added, update the "AC3 … deliberately not re-tested here" note to point at `agent_invite_flow.rs` so the two docs stay consistent (R4). |
| `crates/iroh-rooms-cli/tests/common/mod.rs` | **optional** — extract shared `ChildSession` / `one_shot` / `parse_listening` helpers if it avoids a third copy (OQ3). |
| `docs/getting-started.md` | **at most a light touch** — confirm still green; optionally one line that a bad/expired agent ticket is rejected like a human's. No behavioral change. |
| `crates/*/src/**` (production) | **no change** (§3.2). |

No new dependency: the test dev-deps (`assert_cmd`, `predicates`, `tempfile`,
`serde_json`, `tokio`) are already present in the respective crates.

---

## 7. Dependencies to add

**None.** All primitives (`agent invite`, `room join`, `gate_join`, ticket codec,
`SnapshotAdmission`, `--loopback`) and all test dev-dependencies are landed.

---

## 8. Error model & observability

- **Delegation, not re-mapping.** The suite asserts the *existing* coded errors; it
  introduces no failure surface. A corrupt agent ticket surfaces the same
  `ticket_*` code as any corrupt ticket; a bad-secret agent join surfaces the same
  `bad_capability` as a bad-secret human join — that identity of codes **is** the
  "same capability verification as a human peer" acceptance criterion, asserted.
- **No secret leakage.** Leg 4 must assert (grep) that neither the capability secret
  nor any identity/device seed appears in stdout/stderr of a rejected join — reusing
  `join_cli.rs::join_wrong_identity_error_does_not_expose_secret_seeds`'s pattern.
  `join_reject_message` is already secret-free by construction; the test pins it.
- **Fold is the audit record.** After a rejected agent join, the admin's
  `room members --json` is the observable proof of non-membership (agent absent or
  still `invited`, never `active`) — the fold, not a log line, is the trust oracle.

---

## 9. Security, privacy, reliability

- **Explicit invite only / no implicit access (PRD §13.3 #2, #4; AC3).** An agent is
  `Active` only via an admin-signed, key-bound `member.invited` for its key **plus**
  a `member.joined` that cites it and proves the capability secret (`gate_join`). Leg
  4 proves every way a join *without* a valid capability fails: corrupt ticket
  (codec), wrong identity (key-binding pre-check), wrong secret (`bad_capability`),
  expired (`expired_invite`). None grants membership.
- **Not implicitly trusted (PRD §13.3).** `role = agent` is the least-privileged
  concrete role; the least-privilege merge (spike §3.8) means a concurrent
  `agent`/`member` grant for one key resolves to `agent`. This issue adds no
  privilege; it proves the agent is gated at least as tightly as a human.
- **Signed events (PRD §13.3 #3).** The agent's `member.joined` is signed by its
  device key and validated by the stateless pipeline — unchanged; the rejection legs
  never reach persistence.
- **Same protocol model (spike §1, PRD §13.3).** Capability verification is
  role-agnostic by construction; the agent-flavored tests *assert* that byte-for-byte
  sameness rather than assume it, guarding against a future accidental `agent`
  special-case (R5).
- **Determinism.** The offline legs are pure (no clock/RNG in the assertion path);
  the online legs use `--loopback` (no relay/discovery) and inject `created_at` for
  the expired case (no wall-clock read), so the gated tier is reproducible under
  `--test-threads=1`.
- **Ancestor-view caveat (memory).** When constructing any *negative membership*
  assertion, ensure the failure is for the intended reason (capability), not an
  incidental `NotAMember`/missing-parent (`member-message-ancestor-view-gate` and
  `membership-snapshot-ignores-content-events` memories). Leg 4's rejections are at
  the *join* gate (`gate_join`), which is capability-specific, so this is mainly a
  caution for any content-event negative added as color.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **Read §2.2 and §2.3.** Confirm you are writing tests, not production code, and
   that legs 1–3 already have exhaustive IR-0206 coverage you will *reference*, not
   duplicate.
2. **Scaffold `tests/agent_invite_flow.rs`** (D1): module doc with the AC → test
   table and pointers to `agent_cli.rs`, `agent_e2e.rs`, `join_cli.rs`,
   `join_e2e.rs`. Import/duplicate the `ChildSession` + one-shot helpers (or add
   `tests/common/`, OQ3).
3. **Leg 1 (offline, always-green):** admin `identity create` → `room create` →
   `agent invite <ROOM> <AGENT_ID>`; assert exit 0, `role: agent`, `roomtkt1…`
   ticket, and `room members` shows `role=agent status=invited`. One concise test
   (D2).
4. **Leg 3 (offline, always-green):** copy the room store into a non-admin home;
   `agent invite` there fails with `insufficient_role` (exit 3) and leaves
   `rooms.db` byte-unchanged. One concise test (mirror
   `agent_cli.rs::agent_invite_by_non_admin_is_rejected`).
5. **Leg 4a (offline, always-green) — the new coverage:**
   - *Corrupt ticket:* mint an agent ticket, mutate one base32 char (and, as a second
     case, truncate it); `room join <corrupt>` exits 5 with a `ticket_*` code; assert
     no membership row / store unchanged.
   - *Wrong identity:* mint an agent ticket bound to `<AGENT_ID>`, then attempt
     `room join <ticket>` from a **different** identity's home; assert pre-IO exit 3
     `wrong_identity`, the error names the bound key but leaks no secret, store
     unchanged. (Mirror `join_cli.rs::join_wrong_identity_*` but with an *agent*
     ticket.)
6. **Leg 2-happy (online, `#[ignore]` gated):** one convergence assertion — admin
   `room tail --accept-joins --loopback`, agent `room join --peer <ADDR> --loopback`,
   both rosters show the agent `role: agent, status: active`. (May simply *reference*
   `agent_e2e.rs::agent_joins_and_converges_with_agent_role` if it is judged
   sufficient — see D2; include a thin copy here only if the suite should be
   self-contained.)
7. **Leg 4b (online, `#[ignore]` gated) — the new coverage:**
   - *Wrong secret:* agent redeems a ticket whose capability secret has been swapped
     for a wrong value against a live admin; the join exits non-zero with
     `bad_capability`, and the admin's `room members --json` never shows the agent
     `active`. (CLI-surface, D3.1.)
   - *Expired:* prefer the **net-layer mirror** (D3.2) for determinism — an
     `agent`-role variant of `join_e2e.rs::expired_invite_join_not_accepted` with
     injected `expires_at`/`created_at`. If kept at the CLI tier, use the injected
     clock seam; do not sleep.
8. **Optional net mirror (OQ4):** add the `agent`-role `bad_capability` / `expired`
   variants to `join_e2e.rs` if the reviewer wants a Node-layer pin independent of
   the CLI.
9. **Doc consistency (R4):** if D1's file lands, update the `agent_e2e.rs`
   "deliberately not re-tested" note to point at `agent_invite_flow.rs`; run
   `docs_conformance.rs` to confirm the getting-started guide stays green.
10. **Gate:** `scripts/verify.sh` green (fmt + clippy pedantic + tests
    `--all-features`); run the gated tier locally with
    `cargo test -p iroh-rooms-cli --test agent_invite_flow -- --ignored --test-threads=1`
    (and the net mirror if added).

---

## 11. Test strategy (mapping to the issue Test Plan & ACs)

The issue Test Plan — *"Integration test for admin invite, agent join, non-admin
rejection, and bad ticket"* — maps 1:1 to the four legs:

**Admin invite (AC1)** — `agent_invite_flow.rs`, always-green:
- Admin `agent invite <ROOM> <AGENT_ID>` → exit 0, `role: agent`, `roomtkt1…`
  ticket, `room members` shows `role=agent status=invited`. (Exhaustive matrix:
  `agent_cli.rs`.)

**Agent join (AC4 + happy-path capability verification)** — gated:
- Invited agent redeems its ticket over loopback → both peers converge on
  `agent, active`. (Backstop: `agent_e2e.rs`.)

**Non-admin rejection (AC2 / AC3-invite side)** — always-green:
- Non-admin `agent invite` → `insufficient_role` (exit 3), store byte-unchanged.

**Bad ticket / no valid capability (AC3 — the new coverage)** — mixed tier:
- *Offline (always-green):* corrupt agent ticket → exit 5 `ticket_*`; wrong-identity
  agent ticket → exit 3 `wrong_identity`; both leave the store unchanged and leak no
  secret.
- *Online (gated) / net mirror:* wrong-secret agent join → `bad_capability`; expired
  agent invite → `expired_invite`; agent never `active` on the admin.

**Cross-cutting assertions:**
- **Code-identity (AC3 core):** the agent-flow rejection codes are *identical* to the
  human-flow codes (`bad_capability`/`expired_invite`/`wrong_identity`/`ticket_*`) —
  asserted by using the same taxonomy strings the member tests use. This is the
  literal "same capability verification as a human peer."
- **Secret hygiene:** no capability secret or identity/device seed in any rejected
  join's output (grep, reusing the `join_cli.rs` pattern).
- **Store integrity:** every rejected leg leaves `rooms.db` byte-unchanged (metadata
  length check, as in `agent_cli.rs`).

Run under `scripts/verify.sh --all-features`; the online tier uses the
`two_peer_e2e.rs` / `agent_e2e.rs` `#[ignore]` + `--test-threads=1` loopback
convention.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Redundancy** — IR-0207 re-does IR-0206, adding maintenance with no new guarantee. | §2.2 audit + D2: legs 1–3 are one concise assertion each and *reference* the IR-0206 matrices; effort concentrates on leg 4 (the genuine gap). OQ1 lets the reviewer close as duplicate if preferred. |
| R2 | Executor re-implements the `agent invite` command or edits a gate. | §1 + §3.2 state plainly: no production change. Step 1 is an explicit "you are writing tests" checkpoint; a `src/*.rs` edit outside `#[cfg(test)]` is a scope error. |
| R3 | The "expired" case is non-deterministic if built via `--expires` + sleep. | Build expiry via the net-layer fixture with injected `expires_at`/`created_at` (D3.2), exactly as `join_e2e.rs::expired_invite_join_not_accepted` does; never sleep or read a wall clock. |
| R4 | Adding `agent_invite_flow.rs` leaves `agent_e2e.rs`'s "not re-tested here" note stale/contradictory. | Step 9 updates that doc to point at the new file; the two module docs must agree on who owns the capability-rejection leg. |
| R5 | A future refactor special-cases the `agent` role in `gate_join`, silently diverging from the human path. | The agent-flavored `bad_capability`/`expired` tests (leg 4b / net mirror) fail if `agent` ever gets a different code path — this is a durable guard, and a core reason the "redundant-looking" test has value. |
| R6 | Gated online tests are flaky in CI (two loopback processes). | Follow the proven `agent_e2e.rs`/`two_peer_e2e.rs` harness verbatim (bounded waits, `--loopback`, `--test-threads=1`, `#[ignore]`); keep leg 4's *deterministic* assertions (4a, expired net-mirror) in the always-green tier so CI proves the core rejection without the network. |
| R7 | "Bad ticket" under-scoped (only one failure class tested). | D4 enumerates the four classes and covers all deterministic, agent-meaningful ones; the table documents which tier each lands in so nothing is silently dropped. |
| R8 | Helper duplication (`ChildSession` etc.) across three test files drifts. | OQ3: optionally factor into `tests/common/`; otherwise a local copy matches the current repo pattern and is acceptable. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Enforced by (landed) | IR-0207 test |
|---|---|---|
| Admin can invite an agent by identity key. | `agent invite` → key-bound `member.invited{role:agent}` (IR-0206/IR-0103). | Leg 1 (flow re-assertion); exhaustive: `agent_cli.rs`. |
| Non-admin cannot invite an agent. | Admin-only gate in `invite::invite` → `insufficient_role`. | Leg 3; backstop: `agent_cli.rs::agent_invite_by_non_admin_is_rejected`. |
| **Agent join is rejected without valid capability.** | Role-agnostic `gate_join` (`bad_capability`/`expired_invite`/`insufficient_role`) + join key-binding pre-check (`wrong_identity`) + ticket codec (`ticket_*`). | **Leg 4a (offline: corrupt ticket, wrong identity) + Leg 4b (online/net mirror: wrong secret, expired)** — the new agent-flow coverage. |
| Agent appears as role `agent` in `room members`. | `Role::Agent` fold → `room members`/`--json`. | Leg 2-happy (online converged roster); offline: `agent_cli.rs::agent_invite_appears_in_members_as_agent`. |

---

## 14. Dependencies & sequencing

- **Hard deps (all landed in `main`):** #31 (IR-0206 `agent invite` noun + agent
  identity), #18 (IR-0103 key-bound invite), #19 (IR-0104 room join + `gate_join`
  capability verification — the load-bearing dependency IR-0206 lacked).
- **Reuses (landed):** IR-0008 fold (`Role::Agent`, `gate_join`), IR-0107 admission,
  IR-0110 error taxonomy, the `--loopback` net mode and the two-peer/agent e2e
  harnesses.
- **Sibling (out of scope):** `agent status` authoring lands under the same `agent`
  noun; coordinate nothing here beyond leaving `AgentAction` extensible (already
  done in IR-0206).
- **Unblocks:** the PRD §19 / §17.1 two-humans-plus-one-agent workflow proof gains an
  explicit "the agent is gated exactly like a human" conformance artifact.
- The orchestrator handles all git/GitHub actions; no branch/PR work is part of this
  phase.

---

## 15. Assumptions

1. IR-0206 (#31), IR-0103 (#18), IR-0104 (#19) are landed in `main` (confirmed:
   README + git log; `agent.rs`, `agent_cli.rs`, `agent_e2e.rs`, `join.rs`,
   `join_e2e.rs` all present).
2. `gate_join` capability verification is **role-agnostic** — no branch on `agent`
   (confirmed: spike §3.5/§6 validation list has no role gate beyond `role ==
   invite.role`; `join_e2e.rs` proves the member path). IR-0207 asserts, not
   implements, this for the agent.
3. IR-0207's intended scope is a *flow-level integration/conformance test* closing
   the capability-rejection gap, **not** a re-implementation (see §1 / OQ1).
4. The online agent legs run in the same `#[ignore]`-gated loopback tier as
   `agent_e2e.rs`/`two_peer_e2e.rs`; the deterministic legs (4a, expired net-mirror)
   run always-green.
5. The "expired" case is expressed via injected log timestamps (net fixture), not a
   wall-clock/sleep.
6. No new IR-0110 taxonomy variant is needed; every rejection reuses an existing
   code.

---

## 16. Open questions

- **OQ1 — Is IR-0207 a distinct flow-test issue, or subsumed by IR-0206?**
  *Recommended:* treat it as the flow-level conformance proof that closes the
  agent-flow capability-rejection gap (this spec). *Alternative:* if the reviewer
  reads #32 as fully delivered by #31, close it as substantially-satisfied with a
  pointer to the IR-0206 tests plus the single new agent `bad_capability` test.
  Decide before coding — it changes whether §10 runs fully or shrinks to one test.
- **OQ2 — New `agent_invite_flow.rs` vs extending the IR-0206 files (D1)?**
  Recommended: the dedicated file for traceability; extending `agent_e2e.rs` +
  `agent_cli.rs` is an acceptable lower-friction alternative.
- **OQ3 — Factor the `ChildSession`/one-shot helpers into `tests/common/`?**
  They are duplicated across `agent_e2e.rs` and `two_peer_e2e.rs` today; a third
  copy is the current-pattern default, but a shared module would reduce drift.
  Recommend factoring iff it does not disturb the existing suites.
- **OQ4 — Add the Node-layer `agent`-role capability mirror in `join_e2e.rs` (D3.2)?**
  Recommended: yes for the *expired* case (deterministic there) and optionally for
  *wrong-secret*, to pin the guarantee below the CLI. The CLI tier alone satisfies
  the AC; the mirror is defense-in-depth against a future `agent` special-case (R5).
- **OQ5 — How exhaustively to re-assert legs 1–3 (D2)?** Recommended: one concise
  assertion each, deferring the edge matrix to the IR-0206 files by doc pointer, to
  avoid duplication. Confirm the reviewer is comfortable with the flow suite
  *referencing* rather than *repeating* IR-0206.
- **OQ6 — Any docs change?** Recommended: none beyond confirming green; optionally a
  one-line getting-started note that a bad/expired agent ticket is rejected like a
  human's. Confirm no `docs_conformance.rs` substring is disturbed.
