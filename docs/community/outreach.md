# First Cohort Outreach

This document gives maintainers a practical way to invite the first 10 builders
without over-launching the project.

## The Ask

Ask for a 20 to 30 minute test of one workflow:

> Create a private room, invite one peer, then either share a verified file or
> expose a localhost preview with Live Pipe.

Do not ask people to "join the community" first. Ask them to try a concrete
workflow and tell us where it breaks.

## Who To Invite

Prioritize people who already have a reason to care:

- they build local-first apps;
- they use Rust, iroh, or peer-to-peer tools;
- they work on agent workflows that produce local artifacts;
- they share localhost previews during reviews;
- they dislike public tunnels for private review.

Avoid broad audiences until at least 5 people complete a recipe.

## Short DM

```text
I am running a small controlled beta for Iroh Rooms: private local-first rooms
for humans and agents.

The first test is simple: share a local app or file with a trusted peer without
deploying or creating a public tunnel.

Would you be willing to try a 20-minute workflow and tell me where it breaks?
```

## Technical DM

```text
Iroh Rooms is a CLI-first local collaboration runtime on iroh. It gives a small
trusted room signed messages, verified file sharing, and Live Pipe for private
localhost access.

I am looking for 10 technical builders to try the first controlled beta. The
use case is narrow: invite a peer, share a verified artifact, or expose
127.0.0.1:3000 only to that peer.

If you are open to it, I will send the release notes and a copy-paste recipe.
The main ask is honest feedback, especially where setup or networking fails.
```

## Local-First Angle

```text
I am testing a small local-first collaboration primitive: private rooms where
humans and agents can exchange signed messages, verified files, and private
localhost previews without a central app server.

It is still CLI-heavy, so I am only inviting technical builders for now. Want
to try one recipe and tell me whether the model makes sense?
```

## Agent Builder Angle

```text
I am testing Iroh Rooms as a private room for humans and agents. The agent is
not a bot in a SaaS workspace; it has its own room identity, joins by explicit
invite, and can post signed status or share artifacts.

The first beta is intentionally small. Want to try the agent status or private
preview workflow and tell me what feels missing?
```

## Follow-Up After A Successful Run

```text
Thanks for trying it. Two quick questions:

1. What did you expect to happen that did not?
2. Would you use this again for a real local preview, artifact, or agent
   workflow?

If yes, what would block you from using it next week?
```

## Follow-Up After A Failed Run

```text
Thanks for trying it. That failure is useful.

Can you file a cohort feedback issue with:

- OS and architecture
- command area: room / file / pipe / agent
- network mode: same machine / LAN / home NAT / cellular / relay-only
- redacted error output
- whether a peer process was online

Please do not include full tickets, identity secrets, rooms.db, blobs, or
unredacted audit logs.
```

## Public Post Draft

Use this only after several manual attempts succeed.

```text
Iroh Rooms v0.1.0-rc.2 is open for a small controlled beta.

It is for technical builders who want private local-first collaboration:
create a room, invite a peer, share a verified file, or expose localhost to a
specific trusted room member with Live Pipe.

This is not GA, not a hosted service, and not a public-room product. Local data
is plaintext. Invite tickets are password-grade capabilities. The current
artifact is checksummed but not project-signed.

If you want to test the first cohort workflow, start here:
COMMUNITY.md
docs/community/demo-recipes.md
```

## What To Track

Track in a simple table:

| Person | Segment | Workflow tried | Outcome | Follow-up issue | Would use again? |
| --- | --- | --- | --- | --- | --- |
|  | Rust / iroh / local-first / agent / devtools | room / file / pipe / agent | pass / fail / partial | link | yes / no / unclear |

The goal is not volume. The goal is to learn which workflow creates pull.
