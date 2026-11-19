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

Build release artifacts only after the gates pass. The published artifacts are
the `release` workflow's, not a laptop build.
`scripts/build-release-artifacts.sh --version <VERSION>` is that same build run
locally, and it only ever produces the *rustc host triple*. Use it for local
dry-runs and smoke tests. It writes a `.sha256` and does not sign. Use
`--allow-dirty` only for local dry-runs, never for signed-off Production Beta
artifacts.

### Cutting a release

`crates/iroh-rooms-cli` must already be version `<VERSION>` on the commit you
tag. A non-dry run fails in `tag-guard`, before any build, unless `github.ref`
is `refs/tags/v<VERSION>` and `<VERSION>` is that crate's version. A
`workflow_dispatch` also fails unless the `tag` input equals `github.ref_name`.
Dispatch on the tag. Dispatching `--ref main` records the wrong source and the
guard rejects it.

Before `git tag` or `git push` of any `v*` tag, read the environment this
workflow requests and stop unless that environment already requires
`kortiene` and allows only `v*` tags. This endpoint is readable with
ordinary repository access. `sign-and-publish`'s token does not call it:
that token has `actions: read` and no administration scope.

```bash
name="$(awk '/^  sign-and-publish:/{job=1; next} job && /^  [^ ]/{job=0} job && /^    environment: /{sub(/^    environment: /,""); sub(/[ \t]*#.*/,""); gsub(/^[ \t"\047]+|[ \t"\047]+$/,""); print; exit}' .github/workflows/release.yml)"
: "${name:?}"
gh api "repos/kortiene/iroh-room/environments/${name}" --jq '{protection_rules, deployment_branch_policy}'
```

Stop unless that output shows both of these:

- `protection_rules` contains `"type": "required_reviewers"` and a reviewer
  `"login": "kortiene"`.
- `deployment_branch_policy` is
  `{"protected_branches": false, "custom_branch_policies": true}`.

`deployment_branch_policy` does not list the allowed names. Stop unless the
only deployment policy is the tag `v*`:

```bash
gh api "repos/kortiene/iroh-room/environments/${name}/deployment-branch-policies" --jq '.branch_policies[] | {name, type}'
```

`[]`, `"protection_rules": []`, or `"deployment_branch_policy": null` means
stop. Do not push the tag.

The repository ruleset `release-tags` (id 27471194, added 2026-11-17)
restricts creating, updating, deleting and force-pushing `refs/tags/v*`.

Its bypass list is the release owner's configuration: the Repository admin
role, as set by the release owner on 2026-11-17. A non-admin token cannot
read the bypass list, so this review did not verify it. With that setting,
only accounts with the admin role on this repository can create, move or
delete `v*` tags, and so can any token acting as one of them. Today that is
kortiene. Other accounts, GitHub App tokens and the workflow `GITHUB_TOKEN`
are rejected. Pushing the tag below is the release owner's step.

The publish job still peels `refs/tags/v<VERSION>` to this
run's `GITHUB_SHA` before any release write. That publish-time peel check
stays as defence in depth.

```bash
git tag v<VERSION> <COMMIT>
git push origin v<VERSION>
```

Pushing the tag starts the workflow (`on: push: tags: v*`). To run it again
from the same tag, dispatch on that ref and pass the tag name:

```bash
gh workflow run release.yml --ref v<VERSION> -f tag=v<VERSION>
```

Omit `dry_run`. `-f dry_run=true` skips `tag-guard`, signing, and every release
write. A dry run builds archives and `SHA256SUMS` as workflow artifacts and
does not create a GitHub release. It is not a practice release.

The workflow runs one tag ref at a time (`concurrency: release-${{ github.ref }}`,
`cancel-in-progress: false`). A second run waits. It does not cancel the first.

### Approve the release environment

`sign-and-publish` is the only job with `environment: release`, and the only
job with `contents: write`, `id-token: write`, `attestations: write`, or
`actions: read`. GitHub pauses that job until a required reviewer approves
the `release` environment deployment. Sekou is that reviewer. Nothing is
attested, signed, or uploaded before the approval. Build jobs stay
`contents: read` and have no `id-token`.

Environment protection is the gate. The job's first step only detects a
missing approval. It calls
`GET /repos/{owner}/{repo}/actions/runs/{run_id}/approvals` and fails unless
one entry has `state` `approved`, names `RELEASE_ENVIRONMENT` (the same
string as `environment:`), and has `user.login` `kortiene` (`RELEASE_APPROVER`) with
`user.type` `User`. GitHub stores that approval before the job's steps
start, so the check runs in the same job. It needs `actions: read`. It does
not call the environments API. An environment with no required reviewer
does not pause the job, and this list is empty, so the step fails before
attest or sign. A re-run attempt still needs a fresh GitHub approval, but
the approvals list includes earlier attempts' approvals, so the step cannot
prove the row belongs to that attempt. It cannot tell a protected
environment from an unprotected one with the same name.

### Confirm the approval record

After the tag's `release` run, before publishing the draft, read the
environment name from `sign-and-publish`'s `environment:` and then the
approval record. A non-zero exit from the `jq -e` command means stop.

```bash
name="$(awk '/^  sign-and-publish:/{job=1; next} job && /^  [^ ]/{job=0} job && /^    environment: /{sub(/^    environment: /,""); sub(/[ \t]*#.*/,""); gsub(/^[ \t"\047]+|[ \t"\047]+$/,""); print; exit}' .github/workflows/release.yml)"
: "${name:?}"
gh api repos/kortiene/iroh-room/actions/runs/<run-id>/approvals \
  | jq -e --arg name "$name" 'any(.[]; .state=="approved" and .user.login=="kortiene" and .user.type=="User" and any(.environments[]?; .name==$name))'
```

`gh api --jq` cannot take `--arg`, so the filter is a separate `jq -e`.
Exit 0 means a matching approval exists. Any other exit means stop.

`/approvals` rows have `comment`, `environments`, `state`, and `user`.
A row has no review timestamp. `created_at` and `updated_at` on a nested
environment are that environment resource's times.

A deployment status's `creator` is the run's actor. Approver identity still
comes from the `/approvals` check above.

Environment approvals carry over across re-run attempts. The job-id filter
below is what ties this check to this attempt. It keeps statuses whose
`log_url` ends in `/job/<job_id>` for this attempt's `sign-and-publish` job
and drops statuses from earlier attempts.

Set `run_id`, `attempt`, and `sha` for this attempt. `name` is the
environment name read above. The block is a subshell, so
`set -euo pipefail` applies only there. A failing check does not close an
interactive shell. The subshell's exit status is the result. A non-zero
exit means do not publish. The earliest `waiting` status for this job must
be earlier than the first `queued` status created after that wait, and that
`queued` status must be at or before the job's `started_at`.

Deployment and status lists are requested with `per_page=100` and
`gh api --paginate`. Each page is its own JSON array. `jq -r '.[].id'`
reads every deployment page, and `jq -s 'add // []'` flattens every status
page, so a status that is not on the first page still reaches the job filter.

```bash
(
set -euo pipefail
: "${name:?}"
: "${run_id:?}"
: "${attempt:?}"
: "${sha:?}"
job_json="$(gh api "repos/kortiene/iroh-room/actions/runs/${run_id}/attempts/${attempt}/jobs")"
job_id="$(printf '%s\n' "$job_json" | jq -er '[.jobs[] | select(.name=="sign-and-publish")] | if length==1 then .[0].id else error("expected one sign-and-publish job") end')"
started_at="$(printf '%s\n' "$job_json" | jq -er '[.jobs[] | select(.name=="sign-and-publish")] | if length==1 then .[0].started_at else error("expected one sign-and-publish job") end')"
statuses="$(
  gh api --paginate "repos/kortiene/iroh-room/deployments?environment=${name}&sha=${sha}&per_page=100" \
    | jq -r '.[].id' \
    | while IFS= read -r dep_id; do
        gh api --paginate "repos/kortiene/iroh-room/deployments/${dep_id}/statuses?per_page=100" || exit 1
      done \
    | jq -s 'add // []'
)"
printf '%s\n' "$statuses" \
  | jq -e --arg job "$job_id" --arg started "$started_at" '
      [ .[] | select((.log_url // "") | endswith("/job/" + $job)) ] as $rows
      | ($rows | map(select(.state=="waiting")) | sort_by(.created_at) | first // null) as $waiting
      | if $waiting == null then false
        else
          ($rows
            | map(select(
                .state=="queued"
                and ((.created_at | fromdateiso8601) > ($waiting.created_at | fromdateiso8601))
              ))
            | sort_by(.created_at)
            | first // null) as $queued
          | if $queued == null then false
            else
              (($waiting.created_at | fromdateiso8601) < ($queued.created_at | fromdateiso8601))
              and (($queued.created_at | fromdateiso8601) <= ($started | fromdateiso8601))
            end
        end
    '
)
```

A passing check prints `true` and the subshell exits 0. Any other exit
means do not publish.

### What the publish job does

After approval the job, still before any release write:

1. Re-verifies the four archives, each `.sha256` line, and `SHA256SUMS`, and
   peels `refs/tags/v<VERSION>`. The tag must still peel to this run's
   `GITHUB_SHA`. An annotated tag is peeled to its commit.
2. Attests each archive and `SHA256SUMS` with `actions/attest` (SLSA build
   provenance, one bundle per file).
3. Keyless-signs `SHA256SUMS` with `cosign sign-blob --bundle SHA256SUMS.sigstore.json`.
   There is no project-held signing key. The signer is this workflow's GitHub
   OIDC identity at `refs/tags/v<VERSION>`.
4. Self-verifies, and fails the job if any check fails:
   - `gh attestation verify <archive>` online and again with
     `--bundle <archive>.sigstore.json`, both with `--cert-identity`
     `https://github.com/kortiene/iroh-room/.github/workflows/release.yml@refs/tags/v<VERSION>`,
     `--source-ref refs/tags/v<VERSION>`, `--source-digest` set to `GITHUB_SHA`,
     and `--deny-self-hosted-runners`.
   - The same online and `--bundle` checks for `SHA256SUMS`. The `SHA256SUMS`
     attestation bundle stays on the runner. It is not uploaded.
   - `cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json` with that
     same certificate identity, issuer
     `https://token.actions.githubusercontent.com`, and
     `--certificate-github-workflow-sha` set to `GITHUB_SHA`.

`<archive>.sigstore.json` is the GitHub attestation bundle. `SHA256SUMS.sigstore.json`
is the cosign signature bundle. They are not interchangeable.

A failed self-verify leaves no draft. The attestations and the Rekor entries
from `actions/attest` and `cosign sign-blob` are already public, and they stay
public. Re-running does not erase them.

On success the job creates a draft (`--draft --verify-tag`, and `--prerelease`
when the version contains `-`) and uploads exactly these 14 files:

- four `iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT12>.tar.gz`
- four `<archive>.sha256`
- four `<archive>.sigstore.json`
- `SHA256SUMS`
- `SHA256SUMS.sigstore.json`

The workflow then checks the release is still a draft and that the asset names
match that list. It never publishes the draft and it never passes `--clobber`.
Immutable releases are on, so a published release cannot be overwritten.

If a draft for the tag already exists and has any assets, the job refuses to
upload (`draft <tag> already has N assets`). A re-run after a partial upload
requires deleting that draft by hand first. Read the draft flag, and delete
only when it is still `true`:

```bash
gh release view v<VERSION> --json isDraft
gh release delete v<VERSION> --yes
```

Never pass `--cleanup-tag`. That flag deletes `refs/tags/v<VERSION>` along
with the release, and the next run would no longer be on the tag ref
`tag-guard` requires. Do not delete a release whose `isDraft` is `false`.
The job also refuses to modify a published release. Immutable releases are on.

### Before Sekou publishes the draft

Drafts stay unpublished until Sekou publishes them. Before publishing:

1. Peel `refs/tags/v<VERSION>` and compare that commit to the attestation's
   source digest and to the run's `GITHUB_SHA`. Those three have to be the
   same commit. The workflow peels the tag again immediately before upload;
   this check is the release owner's, on the draft he is about to publish.

   The `^{}` line is the peeled commit. A lightweight tag has no `^{}` line;
   its single SHA is the commit. When both lines are present, the `^{}` line
   wins:

```bash
git ls-remote origin 'refs/tags/v<VERSION>^{}' 'refs/tags/v<VERSION>'
```

   The source digest recorded in the attestation, from a downloaded archive
   (GitHub CLI 2.68.0 or newer, `gh auth login`):

```bash
gh attestation verify <archive> \
  --repo kortiene/iroh-room \
  --cert-identity "https://github.com/kortiene/iroh-room/.github/workflows/release.yml@refs/tags/v<VERSION>" \
  --source-ref "refs/tags/v<VERSION>" \
  --source-digest "<GITHUB_SHA>" \
  --deny-self-hosted-runners \
  --format json \
  --jq '.[].verificationResult.signature.certificate.sourceRepositoryDigest'
```
2. Download the draft assets (or point `--dir` at a directory that has them)
   and run, for each supported triple:

```bash
scripts/verify-release-artifacts.sh --commit <GITHUB_SHA> v<VERSION> <TRIPLE>
```

Publish the draft only after both checks succeed. Publishing is a GitHub
release action, outside this workflow. Immutable releases then reject a later
overwrite of that release.

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
  <PASS/FAIL, paste run URL, then one artifact name + SHA-256 per supported triple.
  For a release built by the signing workflow, also name each
  `<archive>.sigstore.json` and `SHA256SUMS.sigstore.json`, and paste the
  `scripts/verify-release-artifacts.sh` result.>

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
- For a release built by the signing workflow (the first release after rc.5,
  and later): `scripts/verify-release-artifacts.sh --commit <GITHUB_SHA>`
  passed for every triple, and `refs/tags/v<VERSION>` still peels to that
  SHA: <YES/NO / checksum-only candidate, WAIVER-PBETA-0004>
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

The `release` workflow builds each triple on a native runner. rc.1 through rc.5
attached the archive and its `.sha256` only. Those releases stay checksum-only;
their records are not rewritten. `WAIVER-PBETA-0004` is retired
for `v0.1.0-rc.6`, the first published signed release. The release owner
approved that sign-off on 2026-11-16. Verify it with
`scripts/verify-release-artifacts.sh --commit bf353b78f63b08ff5e3f425273aa3e0983167679 v0.1.0-rc.6 <TRIPLE>`
and pin run
[43835416665](https://github.com/kortiene/iroh-room/actions/runs/43835416665):

```bash
jq -r .dsseEnvelope.payload <archive>.sigstore.json | base64 -d | jq -r .predicate.runDetails.metadata.invocationId
```

That output must equal
`https://github.com/kortiene/iroh-room/actions/runs/43835416665/attempts/1`.
This jq read only means something after `scripts/verify-release-artifacts.sh`
has verified that same bundle, because reading the payload alone does not check
its signature or provenance.
An archive whose SHA-256 matches the table in the rc.6 release notes is not
an ungated run 43791394134 byte; those digests do not overlap. See
`docs/releases/v0.1.0-rc.6-release-notes.md`.

Releases built by the signing workflow, starting with the first release after
rc.5, attach the archive, the `.sha256`, and `<archive>.sigstore.json` for each
triple, plus `SHA256SUMS` and `SHA256SUMS.sigstore.json`. Verify those with
`scripts/verify-release-artifacts.sh`, or with the commands in
`docs/operations/install-uninstall.md`. A checksum-only check is:

```bash
shasum -a 256 -c <artifact>.sha256
```

That checksum still matters. It does not check who built the archive. The
attestation and the cosign signature do that, for releases the signing
workflow built.

## Downstream Source Pins (Qualification Tags)

A downstream consumer sometimes needs a fix that has landed on `main` but is not
yet in any release candidate, and pins this repository by exact git revision
rather than by tag. That is permitted, and it is **not** a release: such a
revision has no gate evidence, no release notes, no sign-off, and no support
commitment.

When it happens, the pin must be made visible rather than left as a bare SHA in
someone else's `Cargo.toml`:

- Tag the pinned revision `qualification/<consumer>-<consumer-version>-<short-sha>`
  and push the tag, so the revision is not garbage-collected and is greppable
  from this repo.
- Record the consumer, the revision, and the reason for the pin in the release
  notes of the **next** candidate that contains the same fixes.
- Migrate the consumer to that candidate once it is tagged, and retire the pin.
  A qualification tag is a bridge to the next candidate, never a destination.

A qualification tag must never be referenced by this project's own docs, install
instructions, or cohort material as an installable version.

Outstanding pins:

| Tag | Revision | Consumer | Status |
| --- | --- | --- | --- |
| `qualification/jeliya-v0.6.1-a5d98b70` | `a5d98b70` (rc.3 + 24 commits) | `jeliya` v0.6.1, which pins `iroh-rooms` by git rev with `features = ["experimental"]` | To migrate to `v0.1.0-rc.4`. The pin predates the #137 room-ceiling/backpressure fix, the #122 audit fix, and all of #136/#141-#144/#171/#173 |

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

## Lifecycle evidence

Install, upgrade, rollback, and uninstall evidence comes from GitHub-hosted
runners for all four release triples. That is maintainer decision 2026-11-19, item 8 of https://github.com/kortiene/iroh-room/issues/352#issuecomment-7012603384 (GitHub-hosted runners count as evidence). Each
triple uses the same GitHub-hosted runner image the release workflow builds on:

| Triple | GitHub-hosted runner |
| --- | --- |
| `x86_64-apple-darwin` | `macos-15-intel` |
| `aarch64-apple-darwin` | `macos-15` |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |

The operator procedure is `docs/operations/install-uninstall.md`. Every record
must disclose that the runner is GitHub-hosted, and must disclose the runner
image as `ImageOS` and `ImageVersion` (stored on the record as `image_os` and
`image_version`).

The record's `class` field is self-asserted. The script sets it from the
environment it sees, and it sets `evidence_grade` true only when `class` is
`evidence`. A record counts as evidence only when all of these hold:

- `class` is `evidence` and `evidence_grade` is true.
- `result` is `PASS`.
- `triple` is one of the four release triples in the table above. A probe
  cell does not count, including one from an ubuntu-22.04 runner.
- It is the artifact downloaded from a dispatch of the lifecycle workflow on
  `main`.
- Its run id, run attempt, and head SHA match the GitHub API for that run.

A self-asserted `class` of `evidence` is not enough on its own.

```bash
gh api "repos/kortiene/iroh-room/actions/runs/<run_id>/attempts/<attempt>" \
  --jq '{id, run_attempt, head_sha, event, head_branch, path}'
```

`id`, `run_attempt`, and `head_sha` must equal the record's `github_run_id`,
`github_run_attempt`, and `github_sha`. `event` must be `workflow_dispatch`,
`head_branch` must be `main`, and `path` must be
`.github/workflows/lifecycle.yml`.

Priors in `v0.1.0-rc.1` through `v0.1.0-rc.5` are recorded
`prior_verification: checksum-only`. They are never signature-verified.

`.github/workflows/lifecycle.yml` is in the repository. It is
`workflow_dispatch` only. It was dispatched on `main` and passed.
[Run 44541017067](https://github.com/kortiene/iroh-room/actions/runs/44541017067)
is attempt 1. Its head SHA is `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26`,
`head_branch` is `main`, `event` is `workflow_dispatch`, `path` is
`.github/workflows/lifecycle.yml`, and the conclusion is `success`. The four
evidence-class records are `PASS`. The matrix, including `ImageOS`,
`ImageVersion`, `glibc_max_symbol`, and `ldd --version`, is under glibc floor
in `docs/operations/install-uninstall.md`.

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
