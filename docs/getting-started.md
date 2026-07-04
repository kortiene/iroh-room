# Getting Started: the Iroh Rooms demo

See also: [`docs/protocol.md`](protocol.md) for the byte-level protocol contract (wire format,
signatures, membership fold, reason codes) if you're implementing or auditing a peer rather
than just running the demo.

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
>   As of issue #31 / IR-0206, inviting the agent uses the first-class `iroh-rooms agent
>   invite <ROOM_ID> <AGENT_ID>` noun — a thin, delegating wrapper over `room invite
>   --invitee <AGENT_ID> --role agent` (byte-identical `member.invited`, same ticket, same
>   error codes) — instead of the `--role agent` flag on `room invite`.
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
> - **Step 5** (`iroh-rooms file share | list | fetch`) is implemented and runnable as of
>   issue #29 / IR-0204, completing the Blob Plane #27 / IR-0202 started, with `file fetch`'s
>   failure states made honest and coded by issue #30 / IR-0205 (see
>   [Unavailable file](#unavailable-file) in Troubleshooting). Output blocks for all three
>   commands are reconciled against the shipped binary and show the actual format. `file
>   fetch` requires a provider to be online and serving — run `room tail` on the provider's
>   terminal, which now also serves the blobs it holds over the ACL-gated `iroh-blobs` ALPN.
> - **Step 6** (`iroh-rooms pipe expose | connect | close | list`) is implemented and runnable
>   as of issue #14 / IR-0010, reconciled to the PRD canonical surface by issue #23 / IR-0108.
>   Output blocks are reconciled against the shipped binary and show the actual format. One
>   format note: `--tcp` requires an IP address (`127.0.0.1:3000`, not `localhost:3000`).
>   `pipe close` now takes a bare `<PIPE_ID>` — the room is inferred from the local log; pass
>   `--room <ROOM_ID>` only to disambiguate a pipe id shared across rooms.
> - **Step 7** (`iroh-rooms agent status`) is implemented and runnable as of issue #33 /
>   IR-0208. Output blocks are reconciled against the shipped binary and show the actual
>   format. Posting is **not** role-gated — any active member may post, matching spike §7 — but
>   the demo uses the invited agent (Step 3) to demonstrate the intended workflow. The status,
>   an optional `--message`, an optional integer `--progress <0..100>`, and repeatable
>   `--artifact <FILE_ID>` references are all rendered by the **offline** `room tail --offline`
>   read; live-streaming rendering of `agent.status` is a known display gap (Step 4's live
>   `room tail` renders only `message.text` today).
> - **Issue #24 / IR-0109** adds the Phase 1A two-peer integration test suite
>   (`crates/iroh-rooms-cli/tests/two_peer_e2e.rs`). The CI tier (offline-backbone and
>   restart-persistence tests) runs automatically via `cargo test`. The full online tier —
>   membership convergence, live pipe, and authorized/unauthorized file fetch — is
>   `#[ignore]`-gated; run it locally with:
>   ```bash
>   cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1
>   ```
>   Every step in this guide that is runnable (Steps 1–6) is exercised by the suite.
> - **Issue #34 / IR-0209** adds the Phase 1B full-demo integration test suite
>   (`crates/iroh-rooms-cli/tests/full_demo_e2e.rs`) — the executable transcript of this whole
>   guide with the full three-party cast (two humans plus one agent), driving every command
>   this guide documents through the real binary. The CI tier (offline backbone,
>   restart-validation across every MVP event type, and the agent-posts-but-has-no-admin-
>   privilege pair) runs automatically via `cargo test`. The full online tier — three-way
>   membership convergence, a live agent status push, dual file fetch+verify, and the
>   authorized/unauthorized live-pipe pair — is `#[ignore]`-gated; run it locally with:
>   ```bash
>   cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1
>   ```
>   Every step in this guide (Steps 1–7, plus the optional restart check) is exercised by the
>   suite.
>
> General notes:
>
> - The data-directory override (`--data-dir` flag and `IROH_ROOMS_HOME` env var) is
>   confirmed by the shipped binary — use these exactly as documented.
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
# `agent invite` grants the agent role; omit --expires for a non-expiring invite. It is a
# thin wrapper over `room invite --invitee <AGENT_ID> --role agent` (same event, same ticket).
iroh-rooms agent invite <ROOM_ID> <AGENT_ID>
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

**Expected output** (reconciled to the binary; volatile bytes abbreviated as `…`):

```text
imported: ./hello.txt
file_id: file_…(32 lowercase hex chars)…
name: hello.txt
mime: text/plain
size: 17 bytes
hash: blake3:…(64 hex chars)…
event: blake3:…(64 hex chars)…
room: blake3:…(64 hex chars)…
provider: you (local)
next: run `iroh-rooms room tail blake3:…` to serve it, then peers can `iroh-rooms file fetch blake3:… file_…`
```

Copy the `file_…` value as `<FILE_ID>`. The `hash:` line is Alice's node's BLAKE3-256 content
hash of `hello.txt`; Bob will receive the same hash via the synced `file.shared` event and
verify it independently against the bytes he fetches.

**Command** (Terminal A — Alice) — confirm the file is held locally *before* going online:

```bash
# Substitute <ROOM_ID>. Run this before `room tail` below: the durable blob store
# takes an exclusive on-disk lock while a serving `room tail` session is up, so
# `file list` / `file share` on this same home must run before you start it (or
# after you stop it).
iroh-rooms file list <ROOM_ID>
```

**Expected output** (reconciled to the binary):

```text
room: blake3:…(64 hex chars)…
file_id: file_…(32 hex chars)…
  name: hello.txt
  size: 17 bytes
  hash: blake3:…(64 hex chars)…
  provider: you (local)
```

**Command** (Terminal A — Alice) — stay online and serve the blob (leave this running):

```bash
# Substitute <ROOM_ID>. This is the same `room tail` session used in Step 4 — it now
# also serves the blobs Alice holds over the ACL-gated `iroh-blobs` ALPN. It holds the
# blob store's lock for the whole session: a concurrent `file list` on this home reads
# provider status as `unknown (store in use)`, and a concurrent `file share` fails fast
# (a few seconds, not a hang) with a `blob_store_locked` error — stop `room tail` first
# to share another file from this home.
iroh-rooms room tail <ROOM_ID>
```

**Command** (Terminal B — Bob) — list shared files after syncing the event:

```bash
# Substitute <ROOM_ID>. Bob must have synced the file.shared event from Alice first
# (run `room tail <ROOM_ID>` while Alice is online, or check after a room send).
iroh-rooms file list <ROOM_ID>
```

**Expected output** — Bob's `file list` (reconciled to the binary; `reference-only` because
Bob has not yet imported the blob bytes):

```text
room: blake3:…(64 hex chars)…
file_id: file_…(32 hex chars)…
  name: hello.txt
  size: 17 bytes
  hash: blake3:…(64 hex chars)…
  provider: reference-only
```

`--json` emits a stable JSON array (`file_id`, `name`, `size_bytes`, `blob_hash`, `provider`):

```bash
iroh-rooms file list <ROOM_ID> --json
```

**Command** (Terminal B — Bob) — fetch the file from Alice:

```bash
# Substitute <ROOM_ID> and <FILE_ID> (the file_… value from Alice's `file share`).
iroh-rooms file fetch <ROOM_ID> <FILE_ID>
```

**Expected output** (reconciled to the binary; volatile bytes abbreviated as `…`):

```text
saved: /home/bob/.local/share/iroh-rooms/downloads/hello.txt
verified: blake3:…(64 hex chars)…
size: 17 bytes
provider: …(8 hex chars)…
```

`saved:` names the exact path the verified bytes were written to — the downloads directory
(`$IROH_ROOMS_DOWNLOADS`, else `<data-dir>/downloads/`) unless `--out <PATH>` names a file or
directory. `verified:` is Bob's own independent BLAKE3-256 recompute over the fetched bytes; it
must equal the `hash:` Alice printed at share time.

**What this proves / verify:** `file share` content-addresses the file into a durable local
blob store, records the BLAKE3-256 hash, and persists a signed `file.shared` event to the room
log — the hash is the offline guarantee (spike §4). `file fetch` resolves that reference,
discovers Alice as the provider, dials her over the ACL-gated `iroh-blobs` ALPN (only an active
room member reaches this far — an unauthorized peer is denied at the connect gate, PRD §15.6),
transfers the blob, and independently recomputes BLAKE3-256 over the received bytes before
saving — a `file.shared` that lied about its hash would be caught here, not just trusted.

> **Availability caveat (important):** file bytes are fetchable **only while a peer that holds
> the blob is online and running `room tail`** (the "provider stays online" surface). There is
> no always-on store; availability follows the providers — see the
> [unavailable file](#unavailable-file) troubleshooting case (PRD §9.2, §15.6 #6).

---

## Step 6 — Expose and connect a live pipe

*Demonstrates PRD §15.7 / §16.2 and spike §5: authenticated peer-to-peer TCP forwarding, explicit authorization.*

The Live Pipe is the most powerful — and riskiest — feature: it exposes a **local service**
to an authorized room peer. Authorization is explicit (`--allow <ID>`), the local bind
defaults to loopback, and the pipe closes when its owner process exits.

> For a task-focused guide with an agent scenario and a public-tunnel comparison, see
> [`live-pipe-preview.md`](./live-pipe-preview.md).

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
pipe_id: 09a73f56578cd313b647f1ca0df29ea0
connectors run: iroh-rooms pipe connect blake3:… 09a73f56578cd313b647f1ca0df29ea0 --local <PORT>
close it with: iroh-rooms pipe close 09a73f56578cd313b647f1ca0df29ea0
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
# Substitute <ROOM_ID>. --message, --progress, and --artifact are all optional.
iroh-rooms agent status <ROOM_ID> "running_tests" \
  --message "Running integration tests" \
  --progress 40
```

**Expected output:**

```text
status: blake3:<event-id-hex>
room:   <ROOM_ID>
from:   <AGENT_ID>
stored: yes
delivered: 0 (no peers online — stored locally only)
```

Alice or Bob then read it back with the offline tail (the display surface of record for this
step — see the note above about the live-tail gap):

```bash
# Substitute <ROOM_ID>.
iroh-rooms room tail <ROOM_ID> --offline
```

```text
event=blake3:<event-id-hex> type=agent.status lamport=<n> from=<agent-id-8-hex> role=agent status=active at=<iso8601>  state=running_tests text="Running integration tests" progress=40%
```

**What this proves / verify:** the `agent.status` event is signed by the **agent's own key**
(spike §7). The agent is a first-class participant but not implicitly trusted — it could only
post because it was explicitly invited in Step 3 (spike §3.5; PRD §13.3). A non-member's status
is rejected `not_a_member` — the same `gate_active_member` check every other content event uses.

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

  `pipe connect` is the one command in this list that actually fails when the
  target is offline: it prints the two-line shape (issue #25 / IR-0110 §5.7 / issue
  #38 / IR-0303) and exits `6` — the command-failure twin of the connection panel's
  `offline` state:

  ```text
  error[peer_offline]: the pipe owner is unreachable: …
  next: ask the owner to come online (run `room tail <ROOM_ID>`), then retry; or
  pass `--peer <owner-addr>`
  ```

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

  A caller who is not an **active member of the room at all** (not merely absent from a
  pipe's `--allow` list) is turned away locally, before any dial: `pipe expose` / `pipe
  connect` / `pipe close` report `error[peer_unauthorized]: you are not an active member of
  room …` and exit `3` (issue #25 / IR-0110 §5.7).

- **Next action:** have the pipe owner re-`expose` with the correct `--allow <ID>`, or have
  the admin invite the peer into the room first.

### Invalid ticket

- **Reproduce:** have Bob run `room join` with a garbled (corrupted copy-paste), expired, or
  wrong-identity ticket.
- **CLI reports:** a ticket that fails to **decode** (bad prefix, invalid base32, truncated,
  unsupported version, a bad checksum, or a malformed body) reports one of six stable
  `ticket_*` codes (issue #25 / IR-0110 AC3), exit `5` — never the raw token or secret, only
  the redacted reason:

  ```text
  error[ticket_bad_checksum]: invite ticket failed its checksum (corrupted on copy-paste?)
  next: check the whole ticket was copied (no truncation/whitespace); if it persists, ask
  the admin for a fresh `room invite`
  ```

  A ticket that decodes fine but whose capability secret or identity does not match the
  invite reports `bad_capability`; one past its `--expires` reports `expired_invite` — both
  fold-check failures, exit `3`:

  ```text
  error[bad_capability]: this ticket's secret or identity does not match the invite (bad_capability)
  next: ask the admin to re-issue the invite for your identity id
  ```

  A ticket bound to a different identity than Bob's own reports `error[wrong_identity]`
  (exit `3`) before any network dial; a ticket with no admin discovery hint and no `--peer`
  given reports `error[no_discovery_hint]` (exit `2`).

  This applies identically to an agent's ticket: the codec and `gate_join` never branch on
  role, so a corrupted, wrong-identity, wrong-secret, or expired ticket minted by `agent
  invite` is rejected with the exact same codes as one minted by `room invite` (issue #32 /
  IR-0207, which pins the parity with agent-flavored regression tests).

- **Next action:** ask the admin for a fresh `room invite` (re-issue, optionally with a longer
  `--expires`). There is no native revocation, so a re-issue is the fix.

### Admin not hosting joins

- **Reproduce:** run `room join` while the admin has no `room tail --accept-joins` session running.
- **CLI reports:** a bootstrap-timeout error — the joiner can reach the admin's endpoint but
  the connection is closed before bytes (the default admission gate rejects unknown devices).
  This is the *offline peer* scope item on the join path: `error[no_admin_reachable]`, exit
  `6` (issue #25 / IR-0110 §5.5) — distinct from any authorization rejection:

  ```text
  error[no_admin_reachable]: could not bootstrap the room membership within 10s
  next: ask the admin to run `room tail <ROOM_ID> --accept-joins`, then retry; or pass
  `--peer <admin-addr>`
  ```

- **Next action:** have the admin start `iroh-rooms room tail <ROOM_ID> --accept-joins` and
  retry `room join`. The `--accept-joins` flag opens the provisional-admission window; without
  it no join can bootstrap. Nothing was written locally; the retry is clean.

### Unavailable file

- **Reproduce:** with Alice (the only provider — she is not running `room tail`, or her process
  is stopped), have Bob run `file fetch`.
- **CLI reports** (PRD §9.2, §14, §15.6 #6) a coded, script-branchable line and exits `6`
  (Connectivity):

  ```text
  error[blob_unavailable]: file file_…2c is currently unavailable: no peer holding it is online.
  There is no central inbox and no guaranteed offline delivery
  next: ask a peer that holds the file to run `room tail <ROOM_ID>`, then retry `file fetch`
  ```

  The fetch fails within the bounded per-provider timeout (`--timeout`, default 30s) — never a
  hang. Nothing is written to the downloads directory.

  Two adjacent states are reported distinctly, never folded into `blob_unavailable`:

  - **Every reachable provider refuses the connection** (an authorization wall, not an
    availability gap — e.g. a provider whose ACL has not synced this identity in yet):

    ```text
    error[peer_unauthorized]: file file_…2c could not be fetched: every provider refused the
    connection — this identity (…) is not an active member from their view
    next: ask the admin to confirm your membership has synced, then retry
    ```

    exits `3` (Auth).

  - **Hash mismatch** — the assembled bytes' independently recomputed BLAKE3-256 does not match
    the reference's declared hash (a content-integrity failure, not an availability one):

    ```text
    error[hash_mismatch]: integrity check FAILED: fetched bytes hash blake3:… but the reference
    declares blake3:…; refusing to save (the file reference or a provider may be corrupt — do
    not trust this file)
    next: do not trust this file; the reference or a provider may be corrupt — ask for a
    fresh `file share`
    ```

    exits `4` (Integrity); nothing is saved.

- **Next action:** have a peer that holds the file run `iroh-rooms room tail <ROOM_ID>`, then
  retry `file fetch`. MVP has no pinning or always-on node — availability follows the providers.

### Adjacent cases the CLI also distinguishes

These share the "rejected & logged" model (PRD §16.3); one line each:

- **Invalid signature** — `bad_signature`: an event whose signature does not verify under its
  device key is dropped and logged; nothing is persisted. A running `room tail` prints
  `warning[bad_signature]: dropped an invalid inbound event; not stored, not re-broadcast`
  (issue #25 / IR-0110 §5.8); the session keeps running and still exits `0`. Next action: none
  required by you; the event never enters your log.
- **Non-member event** — `not_a_member`: an event from a key that is not an active member is
  rejected; `room tail` prints the same line shape as `warning[not_a_member]`, kept distinct
  from `bad_signature` (AC1) so a script can tell a forged signature from an unauthorized
  sender. Next action: the author must be invited and join before their events count.

For pipe activity specifically, the local audit log records **open / connect / reject / close**
events (spike §5, PRD §13.2) — inspect it to see exactly why a connection was refused.

### Stable error/warning lines and exit codes

Every reason code above (and a few more — `wrong_identity`, `no_discovery_hint`,
`no_admin_reachable`, `peer_offline`, `peer_unauthorized`, `blob_unavailable`,
`hash_mismatch`) is rendered on **stderr** in one pinned shape a script can grep or branch on
(issue #25 / IR-0110):

- A terminal command failure prints `error[<code>]: <message>` and exits with a small,
  documented category code (`2` = usage, `3` = auth, `4` = integrity, `5` = ticket,
  `6` = connectivity — see the README **Error codes** table for the full scheme). A failure
  the taxonomy has not adopted yet still prints `error: <message>` and exits `1`.
- A long-running session (`room tail`) prints `warning[<code>]: <message>` for a per-event
  advisory that never fails the command — most notably `warning[clock_skew]`, printed for an
  inbound event whose timestamp is far from local time. The event is still accepted, ordered,
  and displayed; the session still exits `0`.
- `room send` reaching zero reachable peers is availability, not failure: it always exits `0`.
- Every `error[<code>]:` line is immediately followed by a `next: <action>` stderr line naming
  the concrete next step (issue #38 / IR-0303) — the two-line shape shown throughout this
  section. It is a second, additive line; the `error[<code>]:` prefix and exit code above are
  unchanged, so a script matching `^error\[` or branching on `$?` still works.

### Verbose network diagnostics

`room members <ROOM_ID> --status --verbose` (`-v`) and `room tail <ROOM_ID> --verbose` append a
stderr-only `diag:` block (issue #38 / IR-0303, §18.1/§18.5) — hidden unless you ask for it, so
the default output is unchanged. It surfaces the network facts you need to self-diagnose a P2P
failure: your own dialable address(es) + home relay url, and each known peer's live path —
`direct` (a hole-punched UDP path — the good case), `relay` (relayed only — it works, just
slower / usually behind NAT), `mixed` (both currently active — not yet fully hole-punched), or
`none` (no active transport at all — always true for an `offline` or `unauthorized` peer, which
never renders as reachable):

```text
$ iroh-rooms room members <ROOM_ID> --status --verbose
room: blake3:…
admin: …
member: … role=admin status=active conn=self
member: … role=member status=active conn=connected
peers: 1 connected, 0 offline, 0 unauthorized
diag: local id=<endpoint_id> direct=192.168.1.20:45001 relay=none
diag: peer 9a0211bd device=7f3a2c1b state=connected path=direct relay=none
diag: transport connected=1 (direct=1 relay=0 mixed=0) offline=0 unauthorized=0
```

The `diag:` lines never contain a private key, a ticket secret, or a message payload — only
public identifiers (an `EndpointId`/`IdentityKey`), connection-state labels, IP socket
addresses, and relay URLs, the same secret-free guarantee every other error/warning line makes.
Path type is read directly from iroh's live transport state, never inferred from latency, and is
diagnostic only — it never feeds an authorization decision.

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

## Using it as a library

Everything this guide walks through with the `iroh-rooms` binary is also reachable as a Rust
library: `crates/iroh-rooms` is the public Rust SDK façade (issue #36 / IR-0301) — a curated,
stability-tiered re-export of the same `identity` / `room` / `events` / `files` / `pipes` surface
the CLI drives, split into a **stable** offline-protocol tier (on by default) and an
**experimental** online-runtime tier (`--features experimental`). See
`crates/iroh-rooms/src/lib.rs` for the crate docs and stability policy, and
`crates/iroh-rooms/examples/` for runnable examples mirroring this guide's steps — start with
`cargo run -p iroh-rooms --example offline_author_and_validate`.
`crates/iroh-rooms/examples/example_agent/` (issue #39 / IR-0304) is a minimal, runnable
example agent — the SDK-driven, adapt-me-as-a-template evolution of this guide's Step 3 (join)
and Step 7 (agent status) — with its own `README.md` covering the run flow and adaptation
points.

Driving the online tier (`Node::spawn`, `connect_to`, admission wiring) names the raw `iroh`
transport identities — `EndpointAddr`, `EndpointId`, `SecretKey` — but a consumer needs no
direct `iroh` dependency of its own to get them (issue #87): they're re-exported verbatim from
`iroh_rooms::experimental::session` (`EndpointId` is also re-exported from `experimental::blob`
and `experimental::pipe_runtime`, next to the blob/pipe APIs that name it, so a consumer working
with only one of those need not import `session` too):

```rust,ignore
use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};

let secret_key = SecretKey::from_bytes(&joiner_device.to_seed());
let admin_id: EndpointId = ADMIN_ENDPOINT.parse()?;
node.connect_to(EndpointAddr::new(admin_id));
```

These are the exact types `iroh-rooms-net`'s public `Node` API names — a same-type re-export,
not a wrapper — so nothing needs converting, and a consumer's own `Cargo.toml` carries no `iroh`
entry at all (see `crates/iroh-rooms-cli/Cargo.toml`, which dropped its direct `iroh` dependency
when it migrated to this re-export).

### Subscribe to room events

A long-running consumer (a resident daemon, a UI backend) can subscribe to a live push
stream of newly-ingested room events instead of polling `room_tail` (issue #83 / IR-0307):

```rust,ignore
use iroh_rooms::experimental::session::Node;
use tokio::sync::broadcast::error::RecvError;

let mut rx = node.room_events();
let mut seen = std::collections::HashSet::new();
loop {
    match rx.recv().await {
        Ok(ev) => {
            if seen.insert(ev.event_id) {
                handle(ev);
            }
        }
        Err(RecvError::Lagged(_)) => {
            // A slow subscriber missed some events — resync from the authoritative
            // tail, deduping against what's already been handled.
            for ev in node.room_tail(u32::MAX).await? {
                if seen.insert(ev.event_id) {
                    handle(ev);
                }
            }
        }
        Err(RecvError::Closed) => break,
    }
}
```

- **Exactly once per stored event.** Own publish, peer sync, and delayed park-promotion all
  route through the same insert choke point, so a duplicate re-see is never re-emitted.
- **Lossy on lag, not silent.** This is a bounded `tokio::sync::broadcast` (capacity
  `NetConfig::room_event_capacity`, default 256, identical in contract to `conn_events`). A
  subscriber that falls behind gets `RecvError::Lagged` and resyncs via `room_tail(u32::MAX)`
  deduped against a seen-set, as in the recipe above.
- **Not ordered by Lamport.** Emission order follows insertion order at the engine, which is
  not necessarily causal order across a park-promotion cascade (a child delivered before its
  parent). Sort by `StoredEvent.lamport` if a total order is needed.
- Out of scope: the CLI's offline authoring commands (`room`/`message`/`invite`/`file.rs`)
  insert directly through the store and never build a `Node`, so they never emit on this
  channel; a `room tail --follow` renderer built on this primitive is deferred.

### Share a file from a live session

The CLI's `file share` (Step 5) cycles the session (shutdown → open store → import →
close → respawn) because it never keeps a `Node` alive across commands. A resident
daemon that already holds a `Node` open (`spawn_room` with a `BlobServeConfig`) can
import straight through the live session instead — no session cycle, no peer
disconnects (issue #84 / IR-0308):

```rust,ignore
use iroh_rooms::experimental::blob::BlobImport;
use iroh_rooms::files::{build_file_shared, HashRef};

let BlobImport { hash, size_bytes } = node.blob_import(&path).await?;
let wire = build_file_shared(
    &identity_secret, &device_secret, &room_id, file_id, name, mime_type,
    size_bytes, HashRef::from_bytes(hash), None, &providers, &heads, now,
);
node.publish(wire.to_bytes()).await?;
```

Import **before** publish: publishing the `file.shared` first would briefly reference
a hash the store doesn't hold yet, so a racing fetcher could see a transient
`blob_unavailable`. The node must have opened its store with a `BlobServeConfig`
(`Node::spawn` alone has none) or `blob_import` returns `BlobError::NotServing`.

### Re-provide a fetched file

Symmetrically, a long-running consumer that just verified fetched bytes can become a
provider of them without restarting:

```rust,ignore
use iroh_rooms::experimental::blob::FetchOutcome;

let (outcome, bytes) = node.fetch_file(provider_addr, hash, hash, timeout).await;
if let (FetchOutcome::Fetched, Some(bytes)) = (outcome, bytes) {
    node.blob_import_bytes(bytes).await?;
    // The hash is already referenced by the file.shared this node fetched by, so it
    // serves the blob to other members immediately — no new file.shared needed.
}
```

Only re-provide on `FetchOutcome::Fetched`: a `HashMismatch` also returns `Some(bytes)` (the
transfer completed but the receiver's independent BLAKE3 recheck disagreed with the declared
hash), and importing those bytes would make this node serve content under a hash it does not
actually match.

## Next steps & references

- [`live-pipe-preview.md`](./live-pipe-preview.md) — a task-focused guide to the Live Pipe
  expose/connect/close workflow, including an agent-generated preview scenario and a neutral
  comparison against public tunnels.
- `PRD.v0.3.md` — product requirements and MVP scope (§6 demo, §14 availability, §15 journeys,
  §16 CLI, §17.2 DX metrics).
- `PHASE-0-SPIKE.md` — protocol design: §1 identity/keys, §5 pipe/blob authorization, §6
  invite capabilities, §7 event-type registry, §8 rejection/flag taxonomy.
- `CONTRIBUTING.md` — workflow, branch naming, and the `scripts/verify.sh` quality gate.
- `docs/sdk-coverage.md` — the Rust SDK façade's coverage audit against the CLI's own usage.
- Backlog: the Phase 0 epic and engineering slices live in
  [GitHub Issues](https://github.com/kortiene/iroh-room/issues).
