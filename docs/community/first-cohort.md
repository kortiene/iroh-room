# First Builder Cohort

Status: draft operating plan  
Duration: 30 days  
Release target: `v0.1.0-rc.3` controlled Production Beta

## Purpose

The first cohort exists to answer one question:

> Can technical builders use Iroh Rooms to collaborate privately around local
> work without a central application server?

This is not a broad launch. It is a controlled usage loop with a small number
of builders who can tolerate beta friction and give precise feedback.

## Positioning

Use this one-line description:

> Private local-first rooms for humans and agents: share local previews,
> verified artifacts, and status directly with trusted peers.

Do not lead with "P2P chat." The valuable wedge is private local collaboration
without deploys or public tunnel URLs.

## Cohort Profile

Invite 10 builders manually.

Prioritize:

- people who already build with Rust, iroh, or local-first tools;
- developers who often share localhost previews;
- agent builders who want local artifacts and status in a trusted room;
- privacy-minded devtools users who understand beta caveats.

Avoid:

- users who need polished GUI onboarding;
- teams with compliance requirements;
- users who cannot build from source if no matching binary exists;
- users who expect always-on hosted delivery.

## Entry Criteria

Each participant should have:

- 20 to 30 minutes available;
- a trusted local machine;
- comfort running terminal commands;
- willingness to redact logs before sharing;
- one concrete thing to share: a local app, a file, or an agent artifact.

## Week 1: Prepare The Loop

Owner tasks:

- Confirm the GitHub release has the macOS artifact and checksum.
- Keep source-build instructions discoverable for non-macOS builders.
- Verify [`docs/community/demo-recipes.md`](demo-recipes.md) against the current
  CLI.
- Open GitHub Discussions or use issues only until there are repeated
  participant threads.
- Prepare a short terminal recording for the Live Pipe recipe.

Participant ask:

- Run the two-human room recipe.
- File one cohort feedback issue, even if the workflow succeeds.

## Week 2: Recruit Manually

Invite 10 named builders. Do not announce broadly.

Use the outreach scripts in [`docs/community/outreach.md`](outreach.md).

Expected output:

- 10 invites sent.
- 5 scheduled attempts.
- 3 completed attempts.
- 1 observed session where the maintainer does not guide every step.

## Week 3: Private Preview Jam

Run a lightweight jam:

- bring a local app, static preview, or artifact;
- create or join a private room;
- share a file or expose `localhost`;
- write down what failed or felt unclear.

Keep it small. Three to five people is enough.

## Week 4: Decide What The Community Actually Is

Review:

- which workflow people chose without prompting;
- where they got stuck;
- whether they understood the availability model;
- whether Live Pipe felt more useful than a public tunnel;
- whether agents were an actual pull or just a novelty.

Decision options:

1. Continue as a local-first builder community.
2. Narrow to "private localhost sharing" as the main wedge.
3. Narrow to "agent collaboration rooms" as the main wedge.
4. Stay in research/beta until stronger pull appears.

## Success Metrics

The first cohort succeeds if:

- 10 people outside maintainers launch the tool.
- 5 complete a room workflow with another participant.
- 3 complete Live Pipe or verified file sharing.
- 3 useful issues are filed.
- 1 person uses it on a real project without maintainer hand-holding.

## Stop Conditions

Pause community growth if:

- users repeatedly leak tickets or local data in public channels;
- onboarding requires maintainer intervention every time;
- the network model is too confusing to explain in one page;
- the only interest is "cool tech" with no actual workflow use;
- security caveats are being misunderstood or minimized.

## Discussion Categories

If GitHub Discussions are enabled, start with these categories:

- `show-and-tell`: local previews, artifacts, and demos people shared.
- `help`: setup and workflow support.
- `gate-a-network-reports`: real-network diagnostics and relay/direct results.
- `agent-workflows`: agent status, artifacts, and room participation.
- `ideas`: proposals that are not ready for issues.

Do not create a Discord until there are repeated discussions that would clearly
benefit from real-time coordination.
