# Contributing to Iroh Rooms

Iroh Rooms uses [`ROADMAP.md`](ROADMAP.md) RB-1 as the authoritative source of
future scope and GitHub Issues as the traceable source of execution. Each
meaningful roadmap change should start from an issue with acceptance criteria
and traceability to an RB-1 row and its cited product, protocol, security, or
operations source. `PRD.v0.3.md` is current; `PRD.md` is historical v0.2 and
does not independently add scope.

## Local Setup

```bash
git clone git@github.com:kortiene/iroh-room.git
cd iroh-room
scripts/verify.sh
```

## Standard Workflow

1. Pick an issue from the current milestone and confirm its RB-1 row.
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
npm run issue -- 35 \
  --repo kortiene/iroh-room \
  --project-root /path/to/iroh-room \
  --runner claude \
  --dry-run
```

Recommended human-reviewed execution:

```bash
cd /path/to/switchyard/adw_sdlc
npm run issue -- 35 \
  --repo kortiene/iroh-room \
  --project-root /path/to/iroh-room \
  --runner claude
```

The `--project-root` flag is required when Switchyard is run from a separate
checkout. It tells the ADW kernel where to load this repository's `.adw` pack,
where to write `agents/{adw_id}` state, which worktree the agent may edit, and
where to execute `scripts/verify.sh`.

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
4. `cargo test -p iroh-rooms --doc` (the SDK façade's module doctests; `--all-targets` above
   does not run doctests)
5. `cargo build -p iroh-rooms --examples` (the façade's `examples/` under default/stable
   features; `--all-features` above already covers the `experimental`-feature build)

## Cutting a Developer Preview

`scripts/verify.sh` gates every pull request, but it does not run the flaky,
resource-heavy `#[ignore]`-gated online test tiers (real loopback processes
and sockets). Before cutting a developer preview build, additionally run:

```bash
scripts/release-readiness.sh
```

This runs `scripts/verify.sh` plus every P0 online tier and prints a single
`release-readiness: READY` / `NOT READY` verdict from real exit codes — a
preview cannot be marked ready while a P0 test is failing. Work through
[`RELEASE-READINESS.md`](RELEASE-READINESS.md) for the full checklist (pipe
security, blob verification, and agent flow review; known MVP limitations;
security warnings; dependency/churn review; demo verification against
`docs/getting-started.md`; and the release-notes template), and paste the
verdict line into its Sign-off section.

## Production Readiness

Do not treat a green Developer Preview gate as a production-grade release.
[`ROADMAP.md`](ROADMAP.md) governs roadmap scope and the rule that every
non-cohort deliverable completes before the external cohort.
[`PRODUCTION-READINESS.md`](PRODUCTION-READINESS.md) remains the detailed source
for security, local data handling, networking evidence, compatibility, release
operations, and candidate sign-off. A historical release waiver does not
complete an RB-1 row.

For production-candidate preparation, run:

```bash
scripts/production-readiness.sh
```

For fast local checks that do not run the full preview gate:

```bash
scripts/production-readiness.sh --offline-only
```

`--offline-only` is an iteration aid, not a production-ready verdict. It may
fail while production P0 artifacts are still missing. Manual P0 sign-offs in
`PRODUCTION-READINESS.md` remain required before any Production Beta or GA
label.

## Roadmap Baseline Changes

Do not add, remove, waive, or reclassify roadmap scope only through an issue
disposition, milestone edit, specification status, or release sign-off. A scope
change requires a reviewed RB-1.x amendment that:

1. names every affected stable row ID;
2. explains the reason and approval;
3. updates dependencies, completion evidence, hardware/external roles, status,
   and cohort-blocking flags; and
4. adds an entry to the amendment log in `ROADMAP.md`.

Closed-as-not-planned work and permanent waivers do not count as completion.
Frozen evidence and approved release records receive additive successors or
errata, never rewrites.
