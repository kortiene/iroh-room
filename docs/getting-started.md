# Getting Started: the Iroh Rooms demo

This guide walks you, from a clean checkout and a fresh local data directory, through the
full Iroh Rooms MVP flow on a single machine:

> create identity → create room → invite & join a second human → exchange signed messages →
> share & verify a file → expose & connect a live TCP pipe → post & read an agent status.

Iroh Rooms is a **local-first, peer-to-peer runtime**. There is **no central application
server and no guaranteed offline delivery**: peers exchange signed events directly, and your
room lives in your local store, not in someone else's cloud. Read the
[Availability model](#availability-model) section before you rely on any of this — it is the
honesty contract for what does and does not work when a peer is offline.

Rough timing targets (from `PRD.v0.3.md` §17.2), so you know what "good" feels like:

| Milestone | Target |
|---|---|
| First identity created | < 1 minute |
| First two-peer room joined | < 3 minutes |
| First live pipe connected | < 5 minutes |

---

## Status of this guide (read first)

> **Partially runnable — read this first.**
>
> - **Step 1** (`iroh-rooms identity create` / `iroh-rooms identity show`) is implemented
>   and runnable as of issue #16 / IR-0101. Output blocks are reconciled against the shipped
>   binary and show the actual format.
> - **Step 2** (`iroh-rooms room create` / `iroh-rooms room members`) is implemented and
>   runnable as of issue #17 / IR-0102. Output blocks are reconciled against the shipped
>   binary and show the actual format.
> - **Step 3** (`iroh-rooms room invite` + `iroh-rooms room join`) is implemented and
>   runnable as of issues #18 / IR-0103 (invite) and #19 / IR-0104 (join). Output blocks for
>   both commands are reconciled against the shipped binary and show the actual format. Note
>   that the admin must keep `room tail --accept-joins <ROOM_ID>` running while the joiner
>   executes `room join` — see the step instructions below.
> - **Step 4** (`iroh-rooms room send` / `iroh-rooms room tail`) is implemented and runnable
>   as of issue #20 / IR-0105. Both commands work against the shipped binary and the output
>   blocks below are reconciled to its actual format. The full two-human exchange is now
>   complete end-to-end once Step 3 join has been performed.
>   As of issue #21 / IR-0106, `room tail <ROOM_ID> --offline [--json] [--limit N]` provides
>   a deterministic, network-free one-shot read of the local log — all validated event types
>   in canonical `(lamport, event_id)` order, exits 0 — and `room members <ROOM_ID> --json`
>   emits the roster as a single-line JSON object. Departed members now show `status=left`
>   (voluntary) or `status=removed` (admin-removed) in both commands. See the
>   [Offline read](#offline-read-room-tail---offline) section in Step 4.
>   As of issue #22 / IR-0107, `room tail` also prints a per-peer connection-state line
>   (`peer … state=connected/offline/unauthorized [reason=…]`) and a roster summary
>   (`peers: N connected, M offline, K unauthorized`) each time a peer's state changes.
>   `room members <ROOM_ID> --status` is also available to query live connection state
>   from a short-lived node without keeping a session running.
> - **Step 6** (`iroh-rooms pipe expose | connect | close | list`) is implemented and runnable
>   as of issue #14 / IR-0010, reconciled to the PRD canonical surface by issue #23 / IR-0108.
>   Output blocks are reconciled against the shipped binary and show the actual format. One
>   format note: `--tcp` requires an IP address (`127.0.0.1:3000`, not `localhost:3000`).
>   `pipe close` now takes a bare `<PIPE_ID>` — the room is inferred from the local log; pass
>   `--room <ROOM_ID>` only to disambiguate a pipe id shared across rooms.
> - **Steps 5, 7** — `file` and `agent` are scaffold — the binary does not recognise them
>   yet. **Expected output** blocks for those steps are *illustrative* (consistent with
>   `PRD.v0.3.md` §16 but not yet captured from a real run).
>
> General notes:
>
> - The data-directory override (`--data-dir` flag and `IROH_ROOMS_HOME` env var) is
>   confirmed by the shipped binary — use these exactly as documented.
> - A few details for later commands are still pending and are flagged inline as
>   **[reconcile]**: the exact `agent invite`/join syntax.
> - **The merged binary is the source of truth.** If you are running against the real
>   binary and an output differs from any block in this guide, trust the binary and file
>   the divergence.


---

## Prerequisites

- **OS:** macOS or Linux (the current dev target).
- **Rust:** the workspace pins `rust-version = "1.80"`; install a toolchain ≥ 1.80 via
  [rustup](https://rustup.rs/).
- **git**, plus `python3` and `curl` for the live-pipe step (a `nc` alternative is noted there).

Clone and build a release binary:

```bash
git clone https://github.com/kortiene/iroh-room.git
cd iroh-room
cargo build --release
```

For readability, this guide invokes the CLI as `iroh-rooms`. The Cargo package is
`iroh-rooms-cli` but the produced binary is named `iroh-rooms`
(`crates/iroh-rooms-cli/Cargo.toml` → `[[bin]] name = "iroh-rooms"`). Alias the built
artifact so the commands below run verbatim:

```bash
alias iroh-rooms="$PWD/target/release/iroh-rooms"
```

> Equivalent without the alias: replace `iroh-rooms` with
> `cargo run --release -p iroh-rooms-cli --` everywhere. Pick **one** convention and use it
> consistently; this guide uses `iroh-rooms`.

---

## Placeholder legend

Placeholders are written as `<UPPER_SNAKE_IN_ANGLE_BRACKETS>`. Every command line that
contains one is preceded by a note saying where to copy the value from. Never paste a value
from this guide as if it were yours — produce your own from the command outputs.

| Placeholder | Meaning | Produced by |
|---|---|---|
| `<ROOM_ID>` | Room identifier | `room create` output |
| `<BOB_TICKET>` | Invite ticket for Bob | `room invite` output |
| `<AGENT_TICKET>` | Invite ticket / handle for the agent | `agent invite` output |
| `<BOB_ID>` | Bob's member identity key (hex) | Bob's `identity show` / `room members` |
| `<AGENT_ID>` | Agent's member identity key (hex) | Agent's `identity show` / `room members` |
| `<FILE_ID>` | File handle | `file share` / `file list` output |
| `<PIPE_ID>` | Pipe session id | `pipe expose` / `pipe list` output |

Tickets carry a secret. **Treat `<BOB_TICKET>` and `<AGENT_TICKET>` like passwords** — see
[Step 3](#step-3--invite-and-join).

---

## Set up the three participants

The demo needs **two humans (Alice, Bob) and one agent**, each a separate identity with its
own local store. On a single machine they must not share state, so each runs the CLI against
a **distinct data directory**, in **its own terminal**.

Point the CLI at a per-participant data directory with `IROH_ROOMS_HOME`. The confirmed
data-directory override options are: the `IROH_ROOMS_HOME` environment variable (used here)
and the `--data-dir <PATH>` global flag, which takes precedence over the env var when both
are set.

Open three terminals at the repo root and create one fresh data directory per participant:

```bash
mkdir -p .demo/alice .demo/bob .demo/agent
```

```bash
# Terminal A — Alice
alias iroh-rooms="$PWD/target/release/iroh-rooms"
export IROH_ROOMS_HOME="$PWD/.demo/alice"
```

```bash
# Terminal B — Bob
alias iroh-rooms="$PWD/target/release/iroh-rooms"
export IROH_ROOMS_HOME="$PWD/.demo/bob"
```

```bash
# Terminal C — Agent
alias iroh-rooms="$PWD/target/release/iroh-rooms"
export IROH_ROOMS_HOME="$PWD/.demo/agent"
```

`.demo/alice`, `.demo/bob`, and `.demo/agent` are the "fresh local data directories" the
[Reset / clean up](#reset--clean-up) section removes between runs. `IROH_ROOMS_HOME` and the
`.demo/*` paths are conventions for this guide — adjust them as you like.

Each labelled step below names the terminal that runs each command.

---

## Step 1 — Create identities

*Demonstrates PRD §15.1: local identity, no central account, stored locally.*

**Command** (run the matching line in each terminal):

```bash
# Terminal A — Alice
iroh-rooms identity create --name "Alice"
iroh-rooms identity show
```

```bash
# Terminal B — Bob
iroh-rooms identity create --name "Bob"
iroh-rooms identity show
```

```bash
# Terminal C — Agent (an agent is an ordinary principal with its own key)
iroh-rooms identity create --name "build-agent"
iroh-rooms identity show
```

**Expected output** (Bob's terminal; volatile bytes abbreviated as `…`):

`iroh-rooms identity create --name "Bob"`:

```text
created identity "Bob"
identity_id: 9f12…4ac1
device_id: 3b77…0e2a
next: run `iroh-rooms identity show`
```

`iroh-rooms identity show`:

```text
name: Bob
identity_id: 9f12…4ac1
device_id: 3b77…0e2a
```

**What this proves / verify:** each participant has an Ed25519 identity key plus a device key
(spike §1), generated locally with no central account, persisted under their
`IROH_ROOMS_HOME`. From `identity show`, **copy the `identity_id` value as `<BOB_ID>` and
the agent's `identity_id` as `<AGENT_ID>`** — you will authorize them by key later.

---

## Step 2 — Alice creates the room

*Demonstrates PRD §15.2: room id generated, creator is admin, `room.created` signed and stored.*

**Command** (Terminal A — Alice):

```bash
iroh-rooms room create "Getting Started Room"
```

Then list members:

```bash
# Substitute <ROOM_ID> from the room create output above.
iroh-rooms room members <ROOM_ID>
```

**Expected output** (`room create`):

```text
created room "Getting Started Room"
room_id: blake3:…(64 hex chars)…
admin: …(Alice's identity_id, 64 hex chars)…
next: run `iroh-rooms room members blake3:…`
```

**Expected output** (`room members <ROOM_ID>`):

```text
room: blake3:…(64 hex chars)…
admin: …(Alice's identity_id, 64 hex chars)…
member: …(Alice's identity_id)… role=admin status=active (admin)
```

**What this proves / verify:** **copy the full `blake3:…` value from the `room_id:` line
as `<ROOM_ID>`.** Alice is the single immutable admin (spike §3.1 — exactly the genesis
signer; no co-admins, no transfer); the genesis `room.created` event is signed by Alice's
device key and stored in Alice's local SQLite event log. The `members` command re-derives
the admin and membership entirely from the persisted event log — there is no separate
`rooms` table.

`--json` emits the same membership view as a single-line JSON object (stable field names,
parseable without brittle formatting — IR-0106):

```bash
iroh-rooms room members <ROOM_ID> --json
```

```text
{"room":"blake3:…","admin":"…(Alice's identity_id)…","members":[{"identity_id":"…","role":"admin","status":"active","is_admin":true}]}
```

---

## Step 3 — Invite and join

*Demonstrates PRD §15.3 and spike §6: scoped, key-bound, single-room invite capabilities.*

**Tickets are secrets.** An invite ticket is a **scoped, key-bound, single-room capability**:
it names the invitee's identity key and carries a secret out-of-band inside the ticket
string. Expiry is supported; **native revocation is not** (spike §6 "MVP limitations") — the
only way to undo an invite is to remove the subject. Anyone who gets the ticket before it
expires can attempt to join as the named key. Handle it like a password.

### Invite and join Bob

**Command** (Terminal A — Alice) — issue the invite:

```bash
# Substitute <ROOM_ID> from Step 2 and <BOB_ID> from Bob's `identity show` (Step 1).
# Invites are key-bound: --invitee names the exact identity allowed to redeem the ticket.
iroh-rooms room invite <ROOM_ID> --invitee <BOB_ID> --expires 24h
```

**Expected output**:

```text
invite_id: da7e…da7e
room: blake3:…(64 hex chars)…
invitee: 9f12…4ac1
role: member
expires: 2026-07-01T12:00:00Z (in 24h)
ticket:
  roomtkt1q…9z
warning: this ticket carries a secret — share it over a private channel and treat it like a password.
next: the invitee runs `iroh-rooms room join <ticket>`
```

Copy the `roomtkt1…` token as `<BOB_TICKET>`.

**Command** (Terminal A — Alice) — start hosting joins **before** Bob redeems the ticket:

```bash
# Substitute <ROOM_ID>. --accept-joins opens the join-bootstrap window while invites are
# open, letting invited peers pull the membership history and push their member.joined.
# Leave this running until all pending joins complete; stop it with Ctrl-C.
iroh-rooms room tail <ROOM_ID> --accept-joins
```

This prints a `listening:` address (same format as a plain `room tail`). On a LAN or in CI
copy that address and pass it to `room join` as `--peer`.

**Command** (Terminal B — Bob) — redeem the ticket while Alice's session is live:

```bash
# Substitute <BOB_TICKET> with the ticket Alice produced above.
# Add --peer <ALICE_LISTENING_ADDR> on a LAN / in CI (no discovery).
iroh-rooms room join <BOB_TICKET>
```

**Expected output** (reconciled to the binary):

```text
listening: <ENDPOINT_ID>@<ip:port>
joined: blake3:…(64 hex chars)…
room: blake3:…(64 hex chars)…
name: "Getting Started Room"
role: member
members: 2 active
next: run `iroh-rooms room members blake3:…` or `iroh-rooms room tail blake3:…`
```

### Invite and join the Agent

**Command** (Terminal A — Alice) — invite the agent by its identity key:

```bash
# Substitute <ROOM_ID> from Step 2 and <AGENT_ID> from the agent's identity show (Step 1).
# --role agent grants the agent role; omit --expires for a non-expiring invite.
iroh-rooms room invite <ROOM_ID> --invitee <AGENT_ID> --role agent
```

**Expected output**:

```text
invite_id: ab12…ab12
room: blake3:…(64 hex chars)…
invitee: 7c5e…d1a0
role: agent
expires: never
ticket:
  roomtkt1q…ag
warning: this ticket carries a secret — share it over a private channel and treat it like a password.
next: the invitee runs `iroh-rooms room join <ticket>`
```

Copy the `roomtkt1…` token as `<AGENT_TICKET>`.

If Alice's `room tail --accept-joins` session from the Bob-join step is still running, the
agent can join immediately. Otherwise restart it in Terminal A:

```bash
iroh-rooms room tail <ROOM_ID> --accept-joins
```

**Command** (Terminal C — Agent):

```bash
# Substitute <AGENT_TICKET> with the agent ticket Alice produced above.
# Add --peer <ALICE_LISTENING_ADDR> on a LAN / in CI (no discovery).
iroh-rooms room join <AGENT_TICKET>
```

### Verify membership

**Command** (run by Alice **and** Bob, to confirm both sides converged):

```bash
# Substitute <ROOM_ID>.
iroh-rooms room members <ROOM_ID>
```

**Expected output** (illustrative):

```text
Members of room_7Q3…f0:
  Alice        9a02…11bd   admin   active
  Bob          9f12…4ac1   member  active
  build-agent  7c5e…d1a0   agent   active
```

**What this proves / verify:** the member list, computed by folding each peer's local log
(spike §3.4), now shows Alice (admin), Bob (member), and the agent with `role = agent`. Both
Alice's and Bob's lists agree after sync. The `member.joined` event was authored by the joiner
itself (its own key + device binding), validated by `gate_join` on every peer against the
causal ancestors (including the naming invite and the capability secret), and stored locally
before Alice's session acknowledged it. The agent was admitted only through an explicit,
key-bound invite — it could not have joined otherwise (spike §3.5).

---

## Step 4 — Send and read messages

*Demonstrates PRD §15.4 / §17.1.3: signed messages, delivered in < 2 s when both peers are online.*

Keep **both peers online** for this step. Iroh Rooms does **not** guarantee offline delivery,
so this step never stages a "send while offline, receive later" flow.

**Command** (Terminal A — Alice) — start tailing first:

```bash
# Substitute <ROOM_ID>. This streams; leave it running (stop with Ctrl-C).
iroh-rooms room tail <ROOM_ID>
```

On startup `room tail` prints its own dialable address as a `listening:` line. On a real
network the peers find each other by iroh discovery, so you can ignore it. On a LAN or in CI
(no discovery), copy that address and pass it to the sender as `--peer` (and vice versa).

**Expected output** — Alice's `room tail` on startup, then Bob's connection and message
(reconciled to the binary; `<author>` is the sender's `member.joined` display name if
known, else a short identity id; `<identity-short>` / `<device-short>` are the first 8
chars of the respective key):

```text
listening: <ENDPOINT_ID>@<ip:port>
tip: share this address with the other peer via --peer
room: <ROOM_ID>
peer <identity-short> device=<device-short> state=connected
peers: 1 connected, 0 offline, 0 unauthorized
[2026-06-30T12:01:04Z] bob1a2b3c: I pushed the first prototype.
```

The two `peer …` / `peers: …` lines are the PRD §16.3 connection panel printed by the
peer connection manager (IR-0107). They appear on every state change (connect, drop,
unauthorized), so a long-running `room tail` session gives a live view of who is online.
To query connection state without a long-running session, use
`iroh-rooms room members <ROOM_ID> --status`.

**Command** (Terminal B — Bob) — send a message:

```bash
# Substitute <ROOM_ID>. Add --peer <ALICE_LISTENING_ADDR> on a LAN / in CI (no discovery).
iroh-rooms room send <ROOM_ID> "I pushed the first prototype."
```

**Expected output** — Bob's `room send` (reconciled to the binary):

```text
sent: <EVENT_ID>
room: <ROOM_ID>
from: <BOB_IDENTITY_ID>
stored: yes
delivered: 1 connected peer(s)
```

`room send` is **offline-first**: it always stores the message locally, then best-effort
pushes it to connected peers. With no peer online it still exits 0 and reports
`delivered: 0 (no peers online — stored locally only)` — there is no queue and no guaranteed
offline delivery (PRD §14). Optionally reverse it: Bob runs `room tail <ROOM_ID>` and Alice
runs `room send <ROOM_ID> "Nice — pulling it now."`

**What this proves / verify:** the message is a signed `message.text` event (spike §7),
delivered to the connected peer in under 2 seconds (PRD §17.1.3) and stored locally in
deterministic `(lamport, event_id)` timeline order. Duplicates are ignored; events with
invalid signatures or from non-members are rejected and logged — see
[Troubleshooting](#troubleshooting) for the reason codes.

---

### Offline read: `room tail --offline`

`room tail` with `--offline` is a **separate, fully offline mode** added in issue #21 /
IR-0106. It is a deterministic, network-free, one-shot projection of the local log — no
`Node`, no membership requirement, no secrets loaded. It renders **all** validated event
types (not just messages) and exits 0.

**Command** (any terminal, no peers needed):

```bash
# Substitute <ROOM_ID>. Reads rooms.db and exits.
iroh-rooms room tail <ROOM_ID> --offline
```

**Expected output** (illustrative; reconcile volatile bytes against your run):

```text
event=blake3:aa… type=room.created  lamport=0 from=alice9f8e role=admin  status=active  at=2026-06-30T12:00:00Z  name="Getting Started Room"
event=blake3:bb… type=member.invited lamport=1 from=alice9f8e role=admin  status=active  at=2026-06-30T12:00:05Z  invitee=bob1a2b3c role=member
event=blake3:cc… type=member.joined  lamport=2 from=bob1a2b3c role=member status=active  at=2026-06-30T12:00:40Z  role=member name="Bob"
event=blake3:dd… type=message.text   lamport=3 from=bob1a2b3c role=member status=active  at=2026-06-30T12:01:04Z  body=I pushed the first prototype.
```

The `event=`/`type=`/`lamport=`/`from=`/`role=`/`status=`/`at=` fields are a stable prefix
tests can parse; the trailing summary after `  ` is human context. Rows ordered by
`(lamport, event_id)` only — `created_at` is advisory display, never a trust input (spike
§2.3/§2.4). Two runs over the same `rooms.db` produce byte-identical output.

`--json` emits the same rows as a single JSON array, with stable field names and
type-specific content fields for structured test assertions:

```bash
iroh-rooms room tail <ROOM_ID> --offline --json
```

Departed members (`member.left` or `member.removed`) are shown with `status=left` or
`status=removed` respectively; `--limit N` restricts output to the N most-recent
causally-complete rows (default 200).

> **Note:** `--offline` is mutually exclusive with `--peer`, `--accept-joins`, and the
> online-session flags. `--json` requires `--offline` (the live session streams
> indefinitely; a JSON-array framing does not apply).

---

## Step 5 — Share and verify a file

*Demonstrates PRD §15.6 / §9.2 and spike §5: content-addressed blob, verified after fetch, honest availability.*

Create a small self-contained sample file (Terminal A — Alice):

```bash
printf 'hello from Alice\n' > hello.txt
```

**Command** (Terminal A — Alice) — share it into the room:

```bash
# Substitute <ROOM_ID>.
iroh-rooms file share <ROOM_ID> ./hello.txt
```

**Expected output** (illustrative):

```text
Imported ./hello.txt as content-addressed blob.
  file id: file_5kP…2c
  hash:    blake3:2f1a…9e   (17 bytes)

Shared file.shared event. Peers can fetch while you (a provider) are online.
```

**Command** (Terminal B — Bob) — list, then fetch:

```bash
# Substitute <ROOM_ID>.
iroh-rooms file list <ROOM_ID>
```

```bash
# Substitute <ROOM_ID> and <FILE_ID> from the file list / share output.
iroh-rooms file fetch <ROOM_ID> <FILE_ID>
```

**Expected output** — Bob's fetch (illustrative):

```text
Fetching file_5kP…2c from an available provider (Alice)…
Verified blake3:2f1a…9e — integrity OK. Saved to ./hello.txt
```

**What this proves / verify:** Bob's fetched bytes are content-verified against the declared
BLAKE3 hash before the file is accepted (spike §5 blob gate); the CLI confirms integrity.

> **Availability caveat (important):** the file is fetchable **only while a peer that holds it
> (here, Alice) is online**. There is no always-on store. If no provider is online, the CLI
> reports an unavailable state honestly rather than hanging — see the
> [unavailable file](#unavailable-file) troubleshooting case (PRD §9.2, §15.6 #6).

---

## Step 6 — Expose and connect a live pipe

*Demonstrates PRD §15.7 / §16.2 and spike §5: authenticated peer-to-peer TCP forwarding, explicit authorization.*

The Live Pipe is the most powerful — and riskiest — feature: it exposes a **local service**
to an authorized room peer. Authorization is explicit (`--allow <ID>`), the local bind
defaults to loopback, and the pipe closes when its owner process exits.

Start a throwaway local service to expose (Terminal A — Alice). This serves the current
directory on loopback port 3000; stop it later with `Ctrl-C`:

```bash
python3 -m http.server 3000
```

> No Python? A minimal stand-in: `while true; do printf 'HTTP/1.1 200 OK\r\n\r\nhi\r\n' | nc -l 3000; done`
> (Linux and macOS ≥ 12 Monterey. Older macOS `nc` needs `-l -p 3000` instead of `-l 3000`.)

**Command** (Terminal A — Alice, in a **new** shell so the server keeps running) — expose it
to Bob only:

```bash
# Substitute <ROOM_ID> and <BOB_ID> (Bob's identity key from Step 1).
# --tcp requires an IP address; use 127.0.0.1, not the hostname "localhost".
iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:3000 --allow <BOB_ID>
```

**Expected output** (`pipe expose`; the two security lines go to stderr, the rest to stdout):

```text
⚠  SECURITY: exposing 127.0.0.1:3000 to 1 allowed member(s): 9f124ac1.
   Anyone allowed can reach 127.0.0.1:3000 through this pipe while it is open.
room: blake3:…(64 hex chars)…
target: 127.0.0.1:3000
label: pipe
allow: 9f12…4ac1
listening: <ENDPOINT_ID>@<ip:port>
tip: share this address with connectors via --peer
pipe_id: 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f
connectors run: iroh-rooms pipe connect blake3:… 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f --local <PORT>
close it with: iroh-rooms pipe close 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f
serving the pipe; press Ctrl-C to close it...
```

The ⚠ SECURITY lines name the exposed target and each allowed member (short id); both go to
stderr, so they stay visible even when stdout is redirected. Rejected connect attempts are
also logged to Alice's stderr as `pipe.connect.rejected:<cause>`; pass `-v` to also log each
accepted connection.

Copy the `pipe_id:` value (32 lowercase hex chars) as `<PIPE_ID>`.

**Command** (Terminal B — Bob) — connect the pipe to a local port:

```bash
# Substitute <ROOM_ID> and <PIPE_ID> from Alice's pipe expose output (or `pipe list`).
iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001
```

**Command** (Terminal B — Bob, in another shell) — prove traffic flows over the pipe:

```bash
curl http://localhost:3001
```

**Expected output** — `curl` returns whatever Alice's `http.server` serves (a directory
listing), carried over the authenticated P2P pipe (illustrative):

```text
<!DOCTYPE html>
<html> … Directory listing for / … </html>
```

**Command** (Terminal A — Alice) — close the pipe when done:

```bash
# Substitute <PIPE_ID> (from Alice's `pipe expose` output or `pipe list`). No room id: the
# room is inferred from the local log — add `--room <ROOM_ID>` only to disambiguate.
iroh-rooms pipe close <PIPE_ID>
```

Then stop the `http.server` with `Ctrl-C` in its shell.

**What this proves / verify:** an **authorized** peer (Bob) connects and traffic flows over an
encrypted peer-to-peer connection; an **unauthorized** peer is rejected at connect time and the
rejection is logged on Alice's terminal as `pipe.connect.rejected:<cause>` (spike §5 — see
[unauthorized peer](#unauthorized-peer)). Closing emits a `pipe.closed` event; pipes also close
on owner **process exit** — a graceful `Ctrl-C` (SIGINT) or `kill` (SIGTERM) publishes
`pipe.closed{owner_exit}`. A hard kill (SIGKILL / power loss) cannot: forwarding still stops when
Alice's endpoint dies, but the pipe shows open in `pipe list` until an owner/admin `pipe close`
(PRD §13.2). **Both peers must be online** for a live pipe (PRD §14.3).

---

## Step 7 — Agent status

*Demonstrates PRD §15.8: the agent posts signed status with its own key, only because it was invited.*

**Command** (Terminal C — Agent):

```bash
# Substitute <ROOM_ID>.
iroh-rooms agent status <ROOM_ID> "Running integration tests…"
```

**Expected output** — Alice and Bob see it in `room tail` (illustrative):

```text
[12:05:18] build-agent (agent): Running integration tests…
```

**What this proves / verify:** the `agent.status` event is signed by the **agent's own key**
(spike §7). The agent is a first-class participant but not implicitly trusted — it could only
post because it was explicitly invited in Step 3 (spike §3.5; PRD §13.3).

---

## (Optional) Persistence & reconnect

*Demonstrates PRD §6 step 6 / §17.1.7: local history survives restart.*

Stop a participant and relaunch it; the local store still holds prior events:

```bash
# In any participant's terminal: stop any running `room tail` (Ctrl-C), then re-read history.
# Substitute <ROOM_ID>.
iroh-rooms room tail <ROOM_ID>
```

Prior messages, file shares, and agent statuses are replayed from the **local** log — they
were never on a server. Reconnect/sync is an **honest, recent-window** exchange between peers
that are online at the same time (spike §0/§4), **not** a guaranteed inbox: events authored
while you were offline arrive only if a peer that holds them is online when you reconnect.
This step is optional and kept out of the timed core flow.

---

## Availability model

This is the honesty contract. After following this guide you should be able to restate it in
one breath (PRD §17.2.5 measures exactly that). All five bullets are PRD §14:

1. **Messages** deliver when peers are online, or reconnect through peers that are available.
2. **Files** are fetchable **only** when at least one peer holding the file is online.
3. **Live pipes** require **both** peers to be online for the whole session.
4. There is **no cloud inbox** and **no guaranteed offline delivery**.
5. There is **no central application server by default.** Optional infrastructure later (a
   user-owned always-on node, a room archive peer, an optional relay) can improve reliability,
   but it **never owns your room**.

Nothing earlier in this guide contradicts these bullets: every send, fetch, and pipe step
keeps both peers online and frames offline behavior as "retry when a peer is back," never as
queued or guaranteed delivery.

---

## Troubleshooting

Each failure mode below lists **how to reproduce it in this demo**, **what the CLI reports**
(the stable reason code from spike §8 plus the human message), and **the next action**. The
reason codes are stable identifiers also written to the local audit log.

### Offline peer

- **Reproduce:** stop Alice's process, then from Bob run `room send`, `file fetch`, or
  `pipe connect`.
- **CLI reports:** in a running `room tail` session the connection panel fires on Alice's
  drop, printing a stable reason string (PRD §16.3 / IR-0107):

  ```text
  peer 9a0211bd device=7f3a2c1b state=offline reason=link_dropped
  peers: 0 connected, 1 offline, 0 unauthorized
  ```

  For one-shot commands (`room send`, `file fetch`) the failure is reported inline:

  ```text
  delivered: 0 (no peers online — stored locally only)
  ```

  `room members <ROOM_ID> --status` shows `conn=offline reason=unreachable` for
  Alice's row if she cannot be reached within the timeout.

- **Next action:** bring the peer back online and retry. Nothing is queued anywhere;
  delivery resumes only when both peers are online together. The `reason` field
  distinguishes a peer who went offline cleanly (`link_dropped`) from one that was
  never reachable (`unreachable`) or had a transport-level failure (`transport_error`).

### Unauthorized peer

- **Reproduce:** have a room member who is **not** in a pipe's `--allow` list run
  `pipe connect`; or have a non-member attempt any room action.
- **CLI reports:** `pipe.connect.rejected` for the pipe case; mesh `not_a_member` for a
  non-member; a blob request from a non-member returns `AbortReason::Permission`
  (spike §5, §8). The **pipe owner** (Alice) also sees the rejection on her `pipe expose`
  terminal (stderr), e.g. `pipe.connect.rejected:not_allowed peer=… pipe=…`. Illustrative:

  ```text
  Pipe connect rejected (pipe.connect.rejected): caller not in allowed_members.
  ```

- **Next action:** have the pipe owner re-`expose` with the correct `--allow <ID>`, or have
  the admin invite the peer into the room first.

### Invalid ticket

- **Reproduce:** have Bob run `room join` with a garbled or expired ticket.
- **CLI reports:** `bad_capability` for a malformed ticket or secret mismatch, `expired_invite`
  for an expired one (spike §8). Illustrative:

  ```text
  Join failed (bad_capability): this ticket's secret or identity does not match the invite (bad_capability)
  ```

- **Next action:** ask the admin for a fresh `room invite` (re-issue, optionally with a longer
  `--expires`). There is no native revocation, so a re-issue is the fix.

### Admin not hosting joins

- **Reproduce:** run `room join` while the admin has no `room tail --accept-joins` session running.
- **CLI reports:** a bootstrap-timeout error — the joiner can reach the admin's endpoint but
  the connection is closed before bytes (the default admission gate rejects unknown devices):

  ```text
  could not bootstrap the room membership within 10s; is the room admin online and accepting joins?
  Pass `--peer <admin-addr>` for a deterministic dial.
  ```

- **Next action:** have the admin start `iroh-rooms room tail <ROOM_ID> --accept-joins` and
  retry `room join`. The `--accept-joins` flag opens the provisional-admission window; without
  it no join can bootstrap. Nothing was written locally; the retry is clean.

### Unavailable file

- **Reproduce:** with Alice (the only provider) offline, have Bob run `file fetch`.
- **CLI reports:** an *unavailable / no-provider-online* state (PRD §9.2, §15.6 #6).
  Illustrative:

  ```text
  File file_5kP…2c is currently unavailable: no peer holding it is online.
  ```

- **Next action:** wait until a peer that holds the file is online, then retry. MVP has no
  pinning or always-on node — availability follows the providers.

### Adjacent cases the CLI also distinguishes

These share the "rejected & logged" model (PRD §16.3); one line each:

- **Invalid signature** — `bad_signature`: an event whose signature does not verify under its
  device key is dropped and logged; nothing is persisted. Next action: none required by you;
  the event never enters your log.
- **Non-member event** — `not_a_member`: an event from a key that is not an active member is
  rejected. Next action: the author must be invited and join before their events count.

For pipe activity specifically, the local audit log records **open / connect / reject / close**
events (spike §5, PRD §13.2) — inspect it to see exactly why a connection was refused.

---

## Reset / clean up

Return to a clean state so the demo is repeatable (and matches the Test Plan):

1. Close any open pipes (`iroh-rooms pipe close <PIPE_ID>`) and stop the `http.server`.
2. Stop any running `room tail` and the participant processes (`Ctrl-C` in each terminal).
3. Remove the per-participant data directories and the sample file:

```bash
rm -rf .demo/alice .demo/bob .demo/agent hello.txt
```

> **This deletes local identities and all room history.** Iroh Rooms is local-first: there is
> no server copy. After this, re-run from [Set up the three participants](#set-up-the-three-participants).

---

## (Optional) Two-machine variant

The canonical demo above runs on **one host** over local discovery (mDNS) and/or relay
fallback — the lowest-friction path and what the timing targets assume. To run Alice and Bob
on **two real machines**, give each its own checkout and `IROH_ROOMS_HOME`, and pass the
ticket between them out-of-band as usual. Direct connectivity is environment-dependent: NAT
hole-punching may succeed or the peers may fall back to a relay. See `PHASE-0-SPIKE.md`
Gate A (NAT/relay) for the connectivity model. This variant is not required to pass the demo.

---

## Next steps & references

- `PRD.v0.3.md` — product requirements and MVP scope (§6 demo, §14 availability, §15 journeys,
  §16 CLI, §17.2 DX metrics).
- `PHASE-0-SPIKE.md` — protocol design: §1 identity/keys, §5 pipe/blob authorization, §6
  invite capabilities, §7 event-type registry, §8 rejection/flag taxonomy.
- `CONTRIBUTING.md` — workflow, branch naming, and the `scripts/verify.sh` quality gate.
- Backlog: the Phase 0 epic and engineering slices live in
  [GitHub Issues](https://github.com/kortiene/iroh-room/issues).
