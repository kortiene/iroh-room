# Sharing a local preview over a Live Pipe

You have a local dev server — or an agent-generated preview build — and you want **one**
authorized peer to look at it, right now, without standing up a public tunnel, a cloud deploy,
or a VPN. Live Pipe forwards that local service directly to a peer you name explicitly: the
connection is peer-to-peer and authenticated, the forward target stays on loopback, and
authorization is a room-scoped, revocable decision you make per pipe, not an open port.

> **Read first.** This guide assumes you already have a room with two active members — follow
> [`getting-started.md`](./getting-started.md) [Steps 1–3](./getting-started.md#step-1--create-identities)
> to create identities, create a room, and invite/join a second participant — and that the
> `iroh-rooms` binary is on your `PATH` (see the
> [Prerequisites](./getting-started.md#prerequisites) there for the build/alias steps). This
> guide picks up from "you have a room with active members" and owns the preview task end to
> end.

---

## What Live Pipe is (and is not)

**Is:**

- Authenticated TCP forwarding between exactly two authorized peers.
- A loopback-only forward target — the pipe forwards a *local* service, it does not bind a
  public port.
- An explicit, per-member allow-list: you name who can connect, pipe by pipe.
- Room-scoped: only an active member of the room can expose, connect to, or close a pipe.
- Session-lived: the pipe exists only while its owner process is serving it.
- Locally audited: reject/teardown decisions print on the owner's terminal.

**Is not (MVP scope, not a shortcoming):**

- Terminal sharing.
- Unix-socket forwarding.
- Multiplexed services (one pipe forwards one target).
- A browser-native UX.
- A public URL of any kind.
- Offline or queued delivery — see [Availability & relay-fallback honesty](#availability--relay-fallback-honesty).

---

## Prerequisites

- A room where the previewer (**Reviewer** / Bob) and the presenter (**Presenter** / Alice, or
  an **Agent**) are both **active members** — see
  [getting-started.md Steps 1–3](./getting-started.md#step-1--create-identities).
- `python3` to stand up a throwaway local server, and `curl` to prove traffic flows. No Python?
  Use the same `nc` fallback as Step 6:
  `while true; do printf 'HTTP/1.1 200 OK\r\n\r\nhi\r\n' | nc -l 3000; done` (Linux and macOS ≥
  12 Monterey; older macOS `nc` needs `-l -p 3000` instead of `-l 3000`).
- On a LAN or in CI, where mDNS discovery isn't available, pass `--peer
  <PRESENTER_LISTENING_ADDR>` to `pipe connect` (the presenter's `listening:` address, printed
  by `pipe expose`) — the same `--peer` convention used throughout the demo.

---

## Scenario A — a local web server preview

This is the core **expose → connect → close** flow, and it is also the concrete proof that an
authorized peer can view a local preview.

### A1. Start a throwaway local server (Presenter)

```bash
python3 -m http.server 3000
```

Leave this running in its own shell; it serves the current directory on loopback port 3000.

### A2. Expose it to one reviewer (Presenter, new shell)

```bash
# Substitute <ROOM_ID> and <BOB_ID> (the reviewer's identity key from `identity show`).
# --tcp requires an IP address; use 127.0.0.1, not the hostname "localhost".
iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:3000 --allow <BOB_ID> --label web-preview
```

**Expected output** (the ⚠ SECURITY lines go to **stderr**; everything else goes to
**stdout**, so the trust decision stays visible even when stdout is redirected):

```text
⚠  SECURITY: exposing 127.0.0.1:3000 to 1 allowed member(s): 9f124ac1.
   Anyone allowed can reach 127.0.0.1:3000 through this pipe while it is open.
room: blake3:…(64 hex chars)…
target: 127.0.0.1:3000
label: web-preview
allow: 9f124ac1fc1e922c346bd9ff55666fe4ae3fb93f7606b78e97e9a4aab485768c
listening: <ENDPOINT_ID>@<ip:port>
tip: share this address with connectors via --peer
pipe_id: 09a73f56578cd313b647f1ca0df29ea0
connectors run: iroh-rooms pipe connect blake3:… 09a73f56578cd313b647f1ca0df29ea0 --local <PORT>
close it with: iroh-rooms pipe close 09a73f56578cd313b647f1ca0df29ea0
serving the pipe; press Ctrl-C to close it...
```

Copy the `pipe_id:` value (32 lowercase hex characters) as `<PIPE_ID>`. Useful flags, in brief
(full reference in [§ Quick CLI reference](#quick-cli-reference)): `--allow` (repeatable,
required), `--label`, `--expires <int>{s|m|h|d}`, `--peer`, `-v/--verbose`.

### A3. Connect and view the preview (Reviewer)

```bash
# Substitute <ROOM_ID> and <PIPE_ID> from the presenter's expose output (or `pipe list`).
iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001
```

**Expected output:**

```text
room: blake3:…
forwarding: 127.0.0.1:3001 -> pipe <PIPE_ID>
connect your client to 127.0.0.1:3001; press Ctrl-C to stop.
```

In another Reviewer shell, prove traffic flows over the authenticated P2P pipe:

```bash
curl http://localhost:3001
```

`curl` returns whatever the presenter's server serves (a directory listing). The connector also
prints a live status line per connection, `[pipe] connection forwarding`. **This is the proof
that an authorized peer can view a local preview** — the bytes travel presenter → pipe →
reviewer with no public hostname anywhere.

### A4. Close the pipe (Presenter)

```bash
# No room id needed: the room is inferred from the local log. Add --room only to disambiguate.
iroh-rooms pipe close <PIPE_ID>
```

**Expected output:** `closed pipe <PIPE_ID> in room blake3:…`. Then stop the `http.server` with
`Ctrl-C`.

There are three ways a pipe closes, and they behave differently — see
[Security warning & close flow](#security-warning--close-flow-deep-dive) for the full picture:

- explicit `pipe close` → publishes `pipe.closed{closed}`;
- presenter process exit via `Ctrl-C` (SIGINT) or `kill` (SIGTERM) → publishes
  `pipe.closed{owner_exit}`;
- a **hard kill (SIGKILL) or power loss** cannot publish anything: forwarding still stops the
  moment the presenter's endpoint dies, but the pipe shows **open** in `pipe list` until an
  owner or admin runs `pipe close`. This is a documented reachability bound (PRD §13.2), not a
  bug.

### A5. Verify the pipe is gone

```bash
iroh-rooms pipe list <ROOM_ID>
```

**Expected output:** `(no open pipes)` once the close has synced locally.

---

## Scenario B — an agent-generated preview

This repo ships **no dedicated example-agent binary**. An "agent" here is an ordinary principal
that holds its own identity key and was invited into the room with the agent role (see
[getting-started.md's "Invite and join the Agent"](./getting-started.md#invite-and-join-the-agent)).
That's documented honestly, not worked around — because the agent is a first-class principal
with its own identity, the mechanics are **identical to Scenario A**, run from the agent's
terminal:

```bash
# Agent terminal (the agent is an active member invited with --role agent).
python3 -m http.server 3000 --directory ./preview-build
iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:3000 --allow <REVIEWER_ID> --label agent-preview
```

The frame: an agent building a site produces a static preview directory (`./preview-build/`
above) and exposes it to a human reviewer for sign-off — no deploy, no public URL. The reviewer
connects exactly as in [A3](#a3-connect-and-view-the-preview-reviewer).

**Security point worth making explicit** (PRD §13.3): the agent can open a pipe **only because
it was explicitly invited and is an active member** — a non-member agent's `pipe expose` is
refused with `error[peer_unauthorized]` (exit `3`) before any dial, the same gate that applies
to any principal. The agent is first-class, not implicitly trusted.

One honest scoping note: this repo ships no turnkey "example agent" process. The scenario above
reuses the same CLI a human uses, driven from the agent's identity. If a dedicated example agent
lands later, it belongs here as a drop-in replacement for the manual `python3 -m http.server`
step.

---

## Security warning & close flow (deep dive)

- **Explicit authorization only.** `--allow` is required and repeatable; there is no
  default-all. Exposing to nobody is refused before any IO: `a pipe must name at least one
  --allow <IDENTITY_ID> (no default-all; PRD §13.2)`.
- **Loopback-only target.** A non-loopback `--tcp` is refused: `refusing to expose non-loopback
  target …: the pipe forward target must be a loopback address (127.0.0.0/8 or ::1)`. The pipe
  forwards a *local* service; it never binds a public port.
- **Visible trust decision.** The ⚠ SECURITY warning (stderr) names the exact target and each
  allowed member by a short id; the full ids are on the `allow:` stdout lines:

  ```text
  ⚠  SECURITY: exposing 127.0.0.1:3000 to 1 allowed member(s): 9f124ac1.
     Anyone allowed can reach 127.0.0.1:3000 through this pipe while it is open.
  ```

- **Least privilege / TTL.** Prefer `--expires <int>{s|m|h|d}` for short-lived previews, and
  `--allow` the single reviewer rather than the whole room.
- **Clean teardown.** The three close paths from [A4](#a4-close-the-pipe-presenter): explicit
  `pipe close` (`pipe.closed{closed}`), owner `Ctrl-C`/`kill` (`pipe.closed{owner_exit}`), and
  the SIGKILL/power-loss bound (forwarding stops, but the pipe stays listed as open until an
  owner/admin closes it). Closing emits a `pipe.closed` event that other members observe;
  `pipe list` reflects it once that event has synced.
- **Local audit.** Reject/teardown lines — `pipe.connect.rejected:<cause>` and
  `pipe.torndown:<cause>` — print on the presenter's **stderr** and are stable and greppable;
  `-v/--verbose` also logs each accepted connection. The CLI installs **no** `tracing`
  subscriber, so this stderr sink is the audit surface the operator actually sees.

---

## Unauthorized access behavior

Two distinct rejection cases, matching
[getting-started.md's Unauthorized peer section](./getting-started.md#unauthorized-peer):

1. **Room member, not in `--allow`.** The connect is rejected at connect time by the owner's
   gate. The **reviewer** sees a denied status line
   (`[pipe] denied by the owner (not authorized / closed)`); the **presenter** sees, on stderr:

   ```text
   pipe.connect.rejected:not_allowed peer=<ep-8-hex> pipe=<pipe-8-hex>
   ```

   No traffic flows.

2. **Not an active member of the room at all.** This caller is turned away **locally, before
   any dial**: `pipe connect` (and `pipe expose` / `pipe close`) report

   ```text
   error[peer_unauthorized]: you are not an active member of room …
   ```

   and exit `3`.

The presenter may also see other owner-side reject causes: `not_active`, `closed`, `expired`,
`unknown_device`, `owner_inactive`. In every case the next action is the same: the pipe owner
re-`expose`s with the correct `--allow`, or the room admin invites the peer first. See
[getting-started.md#unauthorized-peer](./getting-started.md#unauthorized-peer) for the full
reject-cause table.

---

## Availability & relay-fallback honesty

- **Both peers must be online for the entire session.** A pipe is a live stream, not a stored
  artifact — there is no cloud inbox and nothing is queued. If either peer drops, forwarding
  stops.
- **Connectivity is best-effort P2P with relay fallback.** The presenter and reviewer connect
  directly via NAT hole-punching when the network allows it; otherwise they **fall back to a
  relay** (PRD §18.1). Some networks will never permit a direct path — that's expected, not an
  error. A relay-relayed pipe still works, just with higher latency.
- **An offline owner fails loudly, not silently.** `pipe connect` is the one command in this
  guide that fails outright when its target is unreachable:

  ```text
  error[peer_offline]: the pipe owner is unreachable: …
  ```

  exit `6`. Retry once the owner is back online; nothing is queued in the meantime.
- **Hard-kill reachability bound.** As noted in [A4](#a4-close-the-pipe-presenter): a pipe can
  linger as "open" in `pipe list` after a SIGKILL or power loss, until an owner or admin runs
  `pipe close`. This is a documented limit, not a leak — no traffic flows, because the
  presenter's endpoint is dead.
- **No always-on infrastructure by default.** Optional user-owned infrastructure later (an
  always-on node, a self-hosted relay) can improve reachability, but it never owns the room.

The language above is deliberately unembellished: Live Pipe is honest about what it can and
cannot guarantee, rather than overselling reliability.

---

## Comparison vs. public tunnels

Public tunnels and cloud previews are a mature, well-understood category — this is not a
critique of that category, just a note on where it and Live Pipe solve different jobs. A public
tunnel is the right tool when you need a broadly reachable URL, or the reviewer isn't in your
room. Live Pipe targets a narrower job: private review, scoped to people or agents already
sharing a room.

| Dimension | Public tunnel / cloud preview | Live Pipe (this workflow) |
|---|---|---|
| Who can reach it | Anyone with the URL (often public by default) | Only the identities you `--allow`, who must be active room members |
| Where the URL lives | A third-party service issues a public hostname | No public hostname; addressed by room + pipe id, peer-to-peer |
| Auth model | Tunnel-provider account / link-holder | Room membership + explicit per-member allow-list, signed events |
| Data path | Traverses the provider's servers | Direct P2P when possible, else a relay; the loopback target never leaves your machine except through the authenticated pipe |
| Lifetime | Until you stop the tunnel / the link expires | Session-lived; closes on `pipe close`, owner exit, or `--expires` |
| Availability | Provider-hosted, works while their edge is up (even if you're offline) | Requires both peers online; no cloud inbox, no queued delivery |
| Audit | Provider dashboard/logs | Local, greppable audit lines on the owner's terminal |
| Best for | Sharing broadly, webhooks, demos to unknown parties | Private preview review with specific people/agents already in a room |

Choose a public tunnel when you need a URL anyone can open, or the reviewer isn't a room member.
Choose Live Pipe when the review is private, scoped to named peers, and you'd rather not put an
intermediate build on someone else's infrastructure.

---

## Quick CLI reference

- `pipe expose <ROOM_ID> --tcp <IP:PORT> --allow <ID>… [--label <L>] [--expires <int>{s|m|h|d}] [--peer <EP>[@ip:port]]… [-v]`
- `pipe connect <ROOM_ID> <PIPE_ID> --local <PORT> [--peer <EP>[@ip:port]]…`
- `pipe close <PIPE_ID> [--room <ROOM_ID>] [--peer …]`
- `pipe list <ROOM_ID>` (offline read)

---

## Troubleshooting cross-links

- [getting-started.md#offline-peer](./getting-started.md#offline-peer)
- [getting-started.md#unauthorized-peer](./getting-started.md#unauthorized-peer)
- [getting-started.md's "Stable error/warning lines and exit codes"](./getting-started.md#stable-errorwarning-lines-and-exit-codes)

The three pipe-relevant exit codes: `peer_unauthorized` = `3`, `peer_offline` = `6`, successful
runs = `0`.

---

## References

- `PRD.v0.3.md` §9.3 (Live Pipe Plane), §13.2 (Pipe Security), §16 (CLI), §17.3 (Product Success
  Metrics), §18 (Key Risks and Mitigations).
- [`docs/getting-started.md`](./getting-started.md) — Steps 1–3 for room setup, Step 6 for the
  in-demo minimal pipe flow.
- `PHASE-0-SPIKE.md` §5 (pipe/blob authorization), Gate A (NAT/relay).
- `specs/live-tcp-pipe-path.md`, `specs/authenticated-tcp-pipe-expose-connect-close.md`.
