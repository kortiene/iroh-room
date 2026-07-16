# Release Operations

Status: Phase 2.5 Production Beta policy draft.

This document defines the operational release process for production-grade
post-MVP Iroh Rooms builds. It complements `RELEASE-READINESS.md`, which gates
Developer Preview builds, and `PRODUCTION-READINESS.md`, which defines the
Phase 2.5 production bar.

## Release Classes

| Class | Meaning | Required gate |
| --- | --- | --- |
| Developer Preview | MVP workflow is demonstrably usable with known limitations | `scripts/release-readiness.sh` exits `0` |
| Production Beta | Scoped production claim for small technical teams | Preview gate plus production preflight plus manual P0 sign-offs |
| Production GA | Beta evidence supports broader production use within stated scope | Beta exit memo plus release owner approval |

Do not label a build Production Beta or GA if only the Developer Preview gate
has passed.

## Roles

| Role | Responsibility |
| --- | --- |
| Release owner | Owns candidate, checklist, final sign-off, release notes, rollback decision. |
| Security reviewer | Reviews threat model, local data posture, invite/access risks, and audit posture. |
| Protocol/storage reviewer | Reviews schema compatibility, event/protocol fixtures, and migration behavior. |
| Networking reviewer | Reviews Gate A state, diagnostics, relay/direct caveats, and availability wording. |
| Product/beta owner | Owns beta cohort, DX timings, feedback, and product validation memo. |

One person may hold multiple roles for a beta, but the security review should
not be self-approved by the same person who wrote the relevant changes.

## Candidate Metadata

Every production candidate must record:

- Version.
- Commit SHA.
- Build date.
- Rust toolchain.
- Platform(s) built and tested.
- Release owner.
- Security reviewer.
- Protocol/storage reviewer.
- Local storage ADR accepted:
  `docs/decisions/ADR-0001-local-storage-posture.md`.
- Networking reviewer.
- Whether the SDK is source-only or published.
- Whether local storage is plaintext or encrypted.
- Whether invite revocation is implemented or waived.

## Required Commands

Run from repository root:

```bash
scripts/verify.sh
scripts/release-readiness.sh
scripts/production-readiness.sh
```

`scripts/production-readiness.sh --offline-only` is allowed during iteration but
cannot produce a production-ready verdict.

Build release artifacts only after the gates pass. Pushing the release tag runs
the `release` workflow, which builds and attaches every supported triple:

```bash
git push origin v<VERSION>
```

To (re)build artifacts for a tag that already exists — for example after fixing
a release that shipped an incomplete artifact set:

```bash
gh workflow run release.yml --ref main -f tag=v<VERSION>
```

`scripts/build-release-artifacts.sh --version <VERSION>` is the same build run
locally, but it only ever produces the *rustc host triple*. Use it for local
dry-runs and smoke tests, not as the source of published artifacts: a release
cut by hand ships only the platform its release owner happened to build on.
Use `--allow-dirty` only for local dry-runs, never for signed-off Production
Beta artifacts.

## Production Beta Sign-Off Template

```markdown
# Iroh Rooms Production Beta Sign-Off

Version:
Commit SHA:
Date:
Release owner:
Security reviewer:
Protocol/storage reviewer:
Networking reviewer:
Platforms exercised:
Rust toolchain:

## Automated Gates

- `scripts/verify.sh`: <PASS/FAIL, paste summary>
- `scripts/release-readiness.sh`: <PASS/FAIL, paste `release-readiness: ...`>
- `scripts/production-readiness.sh`: <PASS/FAIL, paste `production-readiness: ...`>
- `release` workflow run for `v<VERSION>`:
  <PASS/FAIL, paste run URL, then one artifact name + SHA-256 per supported triple>

## Manual P0 Sign-Offs

- Threat model reviewed: <YES/NO, reviewer>
- Local storage posture accepted: <ADR-0001 scoped plaintext beta/encrypted/blocked>
- Invite revocation posture accepted:
  <ADR-0002 bounded-risk beta/implemented native revocation/blocked>
- Persistent audit posture accepted:
  <ADR-0003 local audit beta/implemented stronger audit/blocked>
- Compatibility and migration policy accepted: <YES/NO>
- Backup and restore tested: <YES/NO>
- Release artifacts versioned and checksummed: <YES/NO>
- Install/uninstall/rollback instructions published:
  <YES/NO, docs/operations/install-uninstall.md>
- Release notes include all known limitations before install/run steps: <YES/NO>
- Privacy-preserving bug report path exists: <YES/NO>

## Current Waivers

- <waiver id, owner, expiry, rationale>

## Verdict

<PRODUCTION BETA APPROVED / NOT APPROVED>
```

## Release Notes Requirements

Release notes must include:

1. Release class: Developer Preview, Production Beta, or GA.
2. Exact supported scope.
3. What users can do.
4. What is explicitly not supported.
5. Storage posture: plaintext or encrypted.
6. Invite revocation posture, including ADR-0002 if native revocation is not
   implemented.
7. Audit posture, including ADR-0003 if audit remains local-only.
8. Availability model: no cloud inbox, no guaranteed offline delivery.
9. Gate output lines.
10. Platforms tested.
11. Install instructions.
12. Rollback instructions.
13. Security reporting path.

Known limitations must appear before install/run instructions.

Use `docs/operations/production-beta-release-notes-template.md` as the starting
point for every Production Beta candidate.

## Build Artifacts

Production Beta artifacts must be:

- Versioned.
- Built from a recorded commit SHA.
- Checksummed.
- Distributed with install and uninstall instructions.
- Built for every triple the release notes list as supported, and listed in the
  notes for every triple actually attached to the release. `install-uninstall.md`
  only supports triples the notes name with a checksum, so an unannounced
  artifact is an unsupported one.

The `release` workflow builds and checksums each triple on a native runner and
attaches both files to the tag's release. Verify a downloaded artifact with:

```bash
shasum -a 256 -c <artifact>.sha256
```

Production GA should add artifact signing. Production Beta can proceed with
checksums only if the release notes say artifacts are checksummed but not
cryptographically signed by the project.

## Installation And Uninstallation

The concrete runbook is `docs/operations/install-uninstall.md`.

Every release must document:

- Supported platforms.
- Required Rust/toolchain version if building from source.
- Binary install path if distributing binaries.
- How to set `IROH_ROOMS_HOME`.
- How to remove the binary.
- How to remove local data, with a warning that this deletes local room state
  and identity secrets.

## Rollback

The concrete rollback procedure is `docs/operations/install-uninstall.md`.

Rollback is only safe if:

1. The user has a pre-upgrade data backup.
2. The previous binary is available.
3. The new build did not perform an irreversible migration.

Release notes must say whether rollback across this version is supported.

If rollback is not supported:

- State that clearly.
- Require users to back up before upgrade.
- Explain how to restore the backup.

## Security Reporting

Production Beta must provide a private or controlled security report path.

At minimum:

- GitHub issue template for ordinary bugs with redaction guidance.
- Security contact or private disclosure path for secrets, data loss, auth
  bypass, or remote code execution concerns.
- Instructions not to attach `identity.secret`, full tickets, `rooms.db`, or
  blob contents to public issues.

## Beta Operations

Production Beta should run as a time-boxed cohort.

Recommended beta window:

- 2 weeks.
- 5-10 technical users.
- Real machines and real networks.
- At least one agent workflow.
- At least one live pipe workflow.
- At least one file/artifact sharing workflow.

Track:

- Time to first identity.
- Time to first two-peer room.
- Time to first live pipe.
- Failed connection causes.
- Support questions.
- Whether users understand the online-peer availability model.
- Whether Live Pipe is preferred over public tunnel workflows in at least one
  real use case.

## Stop-Ship Conditions

Any of these blocks Production Beta:

- Event forgery or signature verification bypass.
- Unauthorized pipe connection.
- Unauthorized blob fetch.
- Secret leakage in normal error output.
- Data corruption across upgrade.
- Irreversible migration without backup/restore warning.
- Known release artifact tampering.
- Security reviewer refuses sign-off.
- Release notes omit plaintext storage or ADR-0002 no-native-revocation caveats
  when those limitations still exist.

Any of these blocks Production GA unless explicitly waived:

- No accepted persistent audit retention/privacy posture.
- Compatibility fixtures fail, or a candidate with prior production data lacks
  previous-candidate upgrade evidence.
- No beta cohort evidence.
- No security review record.
- No rollback story.

## Post-Release Monitoring

For each Production Beta release:

1. Watch issues daily during the beta window.
2. Triage labels: `security`, `data-loss`, `networking`, `pipe`, `blob`,
   `compatibility`, `docs`, `release`.
3. Record every P0 incident in a release follow-up note.
4. Decide whether to patch, roll back, or update known limitations.

## Final Rule

A production release is an operational commitment, not just a passing test run.
If the release owner cannot explain the storage, revocation, audit,
compatibility, and rollback posture in the release notes, the build is not
production-ready.
