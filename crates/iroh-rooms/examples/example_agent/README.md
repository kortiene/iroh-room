# The example agent

A minimal, runnable Rust agent (issue #39 / IR-0304) that drives an Iroh Rooms room **through
the SDK** — the intended integration model — instead of shelling out to the `iroh-rooms`
binary. It is the runnable evolution of `../03_invite_and_join.rs` and
`../07_agent_status.rs`: it sets up its own local identity, joins a room by ticket, posts a
signed `agent.status` update, and (optionally) shares one artifact.

See `main.rs` for the annotated implementation; this file covers how to run it and how to
adapt it into your own agent.

## Integration model

Every room interaction in `main.rs` is a direct call into the `iroh-rooms` SDK's
`experimental` tier (`Node::spawn`, `node.publish(...)`, `node.snapshot()`, …) — not a
subprocess shelling out to the `iroh-rooms` CLI. That is the point of the example: it proves
the Rust SDK (`crates/iroh-rooms`, issue #36 / IR-0301) is sufficient, on its own, to build a
third-party agent integration.

## Run it from a clean checkout

Build once, with the online (`experimental`) tier enabled:

```bash
cargo build -p iroh-rooms --features experimental --example example_agent
```

The full demo needs **one admin (a human, running the real `iroh-rooms` binary) and this
example agent**, mirroring `docs/getting-started.md`'s three-terminal convention. Two
terminals are enough here (the admin, and the agent).

**Terminal A — Admin** (build the CLI once, then create identity + room):

```bash
cargo build --release
alias iroh-rooms="$PWD/target/release/iroh-rooms"
export IROH_ROOMS_HOME="$PWD/.demo/admin"
mkdir -p "$IROH_ROOMS_HOME"

iroh-rooms identity create --name "Admin"
iroh-rooms room create "Example Agent Room"
# copy the printed room id as <ROOM_ID>
```

**Terminal B — Agent** (print the agent's identity id so the admin can invite it):

```bash
cargo run -p iroh-rooms --features experimental --example example_agent -- identity
# copy the printed identity_id as <AGENT_ID>
```

**Terminal A — Admin** (invite the agent, then start hosting joins):

```bash
iroh-rooms agent invite <ROOM_ID> <AGENT_ID>
# copy the printed roomtkt1… token as <AGENT_TICKET>

iroh-rooms room tail <ROOM_ID> --accept-joins
# prints `listening: <ENDPOINT_ID>@<ip:port>` — copy that as <ADMIN_ADDR> and leave this running
```

**Terminal B — Agent** (redeem the ticket, join, and post a status):

```bash
cargo run -p iroh-rooms --features experimental --example example_agent -- join \
  --ticket <AGENT_TICKET> \
  --peer <ADMIN_ADDR> \
  --status running_tests \
  --message "Running integration tests" \
  --progress 40 \
  --loopback
```

`--loopback` forces the deterministic loopback/relay-disabled transport — the right choice on
one machine or in CI. Drop it to use the default real-network (discovery + relay) stack across
two machines.

**Terminal A — Admin** (confirm the signed status landed):

```bash
iroh-rooms room tail <ROOM_ID> --offline
# → event=blake3:… type=agent.status … from=<agent-id-8-hex> role=agent … state=running_tests …
```

That last line — a signed `agent.status` event, authored under the agent's own device key,
readable from the admin's local log — is the Test Plan this example (and its gated
integration test, `../../tests/example_agent_e2e.rs`) proves.

## Adaptation guide

Everything below is a line-referenced list of what a real agent integration changes:

- **What work you do, and what status you report.** `main.rs`'s `run_join` posts exactly the
  `--status`/`--message`/`--progress` you passed on the command line. Replace the CLI-arg
  plumbing with your agent's own work loop: do the work, then call `build_agent_status(...)` +
  `node.publish(...)` again for each update (see `../07_agent_status.rs` for a multi-post
  illustration). Posting `agent.status` is not role-gated — any active member may post it —
  but an `agent`-role principal is the documented convention (PRD §15.8).
- **Identity persistence.** `save_identity`/`load_identity` use a minimal two-line hex-seed
  file. A real integration should use whatever secret storage its deployment already has
  (a secrets manager, an OS keychain, …) — the only contract the SDK cares about is that you
  can reconstruct the same `SigningKey`s (`SigningKey::from_seed`) across runs. This
  deliberately does **not** reuse the CLI's `identity.json`/`identity.secret` layout: the SDK
  does not expose that persistence, and coupling to a CLI-internal format would be fragile. If
  you already have a human-created `iroh-rooms` identity you want the agent to use instead of
  generating a new one, run `iroh-rooms identity show` on that identity, and invite it — the
  SDK identity type is the same `SigningKey`/`IdentityKey` either way.
- **Publishing a result.** `share_artifact` imports a file, hashes it (BLAKE3-256, verified by
  the local blob store), and publishes a `file.shared` reference — pass `--artifact <PATH>` to
  exercise it. This short-lived example session does **not** itself serve the blob's bytes
  (importing + declaring the reference is enough to demonstrate "publish a result"). To
  actually serve fetches, bring the node up with `Node::spawn_room` and a `BlobServeConfig`
  instead of `Node::spawn` — see `crates/iroh-rooms-cli/src/message.rs`'s `tail` function for
  the reference wiring (admission cell, peer manager, blob ACL), or `../05_share_and_fetch_file.rs`
  for the SDK-level fetch call.
- **Exposing a live preview instead of a file.** This example does not expose a pipe (it needs
  a live local TCP service to be meaningful — out of scope for a minimal example). Adapt
  `../06_live_pipe.rs`'s `pipe_expose`/`pipe_connect` calls into the join flow if your agent's
  "result" is a running service rather than a file.
- **Multiple rooms / long-running daemon.** This example is intentionally single-room and
  exits after one status push (plus a flush grace). A production agent that stays online
  across many events should mirror `iroh-rooms room tail`'s long-running loop
  (`Node::spawn_room`, polling `node.room_tail(...)`/`node.conn_events()`) instead of the
  short-lived `Node::spawn` this example uses.

## Authorization posture

The only capability this agent holds is the room membership its invite ticket granted:

- **No central-service credentials.** Its identity is a locally generated Ed25519 keypair
  (`example_agent identity`); the only other input `join` needs is the room ticket. There is
  no account, no server login, no API key, anywhere in this flow.
- **Admission is seeded solely from the ticket's discovery hint.** On `join`, the example
  binds an `AllowlistAdmission` to *only* the admin's device (from `ticket.discovery`) — it
  will not dial or accept anyone else until the room's fold teaches it otherwise.
- **It joins at the ticket's role** (expected `agent`, the least-privileged role in the
  `Agent < Member < Admin` lattice) and authors only `member.joined`, `agent.status`, and
  (optionally) `file.shared` — never `member.invited`/`member.removed`, never anything implying
  admin authority.
- **Every event it authors is re-gated by the same fold check every peer in the room runs.**
  The agent is not implicitly trusted by virtue of being "an agent" — it can act only because
  an admin explicitly invited its specific identity key (`agent invite`).
- **Remove the agent, or let its invite expire, and it can do nothing further** — there is no
  standing credential beyond the room membership the (still-valid) ticket redemption produced.

## Availability honesty

Iroh Rooms is a local-first, best-effort peer-to-peer runtime — there is no central inbox and
no guaranteed offline delivery (PRD §14). This example follows the same honesty contract the
CLI does:

- `join` exits successfully once the `member.joined`/`agent.status` events are stored locally,
  even if delivery to the admin could not be confirmed live — the `stored: yes` /
  `delivered: …` lines it prints distinguish the two.
- `--artifact`'s provider-stays-online caveat: this example's short-lived session does not
  serve the shared blob's bytes past its own exit (see the adaptation guide above).
