# Contributing to Iroh Rooms

Iroh Rooms uses GitHub Issues as the traceable source of work. Each meaningful
change should start from an issue with acceptance criteria and traceability back
to `PRD.v0.3.md` or `PHASE-0-SPIKE.md`.

## Local Setup

```bash
git clone git@github.com:kortiene/iroh-room.git
cd iroh-room
scripts/verify.sh
```

## Standard Workflow

1. Pick an issue from the current milestone.
2. Create a branch using the issue number and a short slug.
3. Make the smallest coherent change that satisfies the issue.
4. Run `scripts/verify.sh`.
5. Open a pull request that links the issue.

Example branch names:

```text
feat/5-rust-workspace-hygiene
protocol/6-event-core-model
test/7-protocol-vectors
```

## Switchyard / ADW Workflow

Switchyard can orchestrate contribution runs from GitHub Issues. Treat it as an
optional internal tool, not as an authority that bypasses maintainer review.

Recommended dry run:

```bash
cd /path/to/switchyard/adw_sdlc
npm run issue -- 5 --repo kortiene/iroh-room --runner claude --dry-run
```

Recommended human-reviewed execution:

```bash
cd /path/to/switchyard/adw_sdlc
npm run issue -- 5 --repo kortiene/iroh-room --runner claude
```

Important: Switchyard's current standalone kernel resolves its repository root
from the installed package location. If it does not load this repository's
`.adw/config.json` when run externally, use the dry-run output only and track a
Switchyard follow-up to support explicit external project roots before relying
on full automation.

## Safety Rules

Do not auto-merge changes for issues labeled:

- `priority/p0`
- `risk/high`
- `area/protocol`
- `area/transport`
- `area/pipe`
- `type/security`

For those issues, Switchyard may help produce a branch or PR, but a maintainer
must review the protocol, security, and operational impact before merge.

## Quality Gate

Every pull request should pass:

```bash
scripts/verify.sh
```

The gate currently runs:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test --workspace --all-targets --all-features`

