# Iroh Rooms Community

Iroh Rooms is building a small, technical community around private local-first
collaboration for humans and agents. The first community goal is not growth for
its own sake. It is to help real builders test one narrow workflow:

> Share a local app, file, or agent artifact privately with trusted peers,
> without deploying a central application server.

## Who This Is For

The first cohort is for builders who are comfortable with a CLI and willing to
file precise feedback.

Good fits:

- Rust and iroh builders.
- Local-first application developers.
- AI-agent builders working on CLI or local workflows.
- Devtools builders who share localhost previews.
- Privacy- and security-minded technical users.
- People who already use tunnels and want a narrower trusted-room model.

Poor fits for the first cohort:

- Non-technical end users.
- Large public communities.
- Enterprise compliance deployments.
- Users who need guaranteed offline delivery or hosted cloud availability.
- Users who need encrypted local storage against local machine compromise.

## What We Are Testing First

The first cohort focuses on three workflows:

1. Two humans create and join a private room.
2. One peer shares a verified file and another fetches it.
3. One peer exposes `localhost` privately with Live Pipe.

Optional stretch workflow:

4. An explicitly invited agent posts signed status and references an artifact.

See [`docs/community/demo-recipes.md`](docs/community/demo-recipes.md) for the
copy-paste recipes.

## Community Principles

- Small rooms over public feeds.
- Concrete demos over abstract platform talk.
- Private, trusted collaboration over default-public links.
- Honest limitations over polished claims.
- Reproducible feedback over vague reactions.

## Security And Privacy Rules

Treat Iroh Rooms cohort data as sensitive.

Do not post publicly:

- `identity.secret`
- full invite tickets (`roomtkt1...`)
- invite capability secrets
- `rooms.db`
- blob contents or private artifacts
- unredacted `audit.ndjson`
- full data-directory backups
- full terminal transcripts containing tickets or secrets

Invite tickets are password-grade capabilities. Share them only over a private
channel with the intended participant.

Local storage is plaintext for this beta. Use trusted local machines only.

## How To Participate

1. Read [`docs/releases/v0.1.0-rc.2-release-notes.md`](docs/releases/v0.1.0-rc.2-release-notes.md).
2. Run one recipe from [`docs/community/demo-recipes.md`](docs/community/demo-recipes.md).
3. File feedback with the cohort feedback issue template.
4. If the workflow worked, share what you used it for.
5. If it failed, include the command area, redacted output, OS, architecture,
   and network mode.

## What Counts As Success

The first community milestone is not stars or chat members.

Success means:

- 10 people outside the maintainers launch the tool.
- 5 people complete a room workflow with another participant.
- 3 people complete Live Pipe or verified file sharing.
- 3 useful issues are filed from real attempts.
- 1 person uses Iroh Rooms on a real project without maintainer hand-holding.

If those happen, the project has a real community seed.

## Useful Links

- [Getting started walkthrough](docs/getting-started.md)
- [Live Pipe preview guide](docs/live-pipe-preview.md)
- [First cohort plan](docs/community/first-cohort.md)
- [Demo recipes](docs/community/demo-recipes.md)
- [Outreach guide](docs/community/outreach.md)
- [Production Beta sign-off](docs/releases/v0.1.0-rc.2-production-beta-signoff.md)
