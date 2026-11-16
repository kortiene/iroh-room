# Security Policy

Iroh Rooms is a pre-1.0 Production Beta for a small cohort of technical builders.
It has not had an independent security review
([`WAIVER-PBETA-0003`](docs/releases/v0.1.0-rc.6-production-beta-signoff.md)).
Treat it accordingly, and please report what you find.

**v0.1 GA will not have an independent security review either.** No outside
reviewer or firm examines the v0.1 GA candidate. It gets only an internal
structured self-review by the project, whose plan and evidence live in
[`docs/security/v0.1-self-review.md`](docs/security/v0.1-self-review.md),
alongside the [threat model](docs/security/threat-model.md). The maintainer
accepted this gap for v0.1 GA only
([`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md)
OD-2, roadmap amendment RB-1.3); an independent review remains required
before v0.2 or any public beta expansion.

## Supported Versions

Security fixes target the supported row below. The newest published tag is
`v0.1.0-rc.6`. It is signed, and it is not the supported build until its
Production Beta sign-off is approved.

| Version | Supported | Notes |
| --- | --- | --- |
| `v0.1.0-rc.5` | Yes | last release-owner-approved build. Supported until the rc.6 sign-off is approved |
| `v0.1.0-rc.6` | No | published 2026-11-16 as a signed prerelease (release id 446819408). Production Beta sign-off is still pending, so it is not the supported build yet. Verify with `scripts/verify-release-artifacts.sh --commit bf353b78f63b08ff5e3f425273aa3e0983167679`. Pin run 43835416665 with `jq -r .dsseEnvelope.payload <archive>.sigstore.json \| base64 -d \| jq -r .predicate.runDetails.metadata.invocationId`, which must print `https://github.com/kortiene/iroh-room/actions/runs/43835416665/attempts/1`. An archive whose SHA-256 matches the rc.6 notes table is not an ungated run 43791394134 byte |
| `v0.1.0-rc.2` .. `v0.1.0-rc.4` | No | published, but their sign-offs were never completed; upgrade to `rc.5` (last approved) |
| `v0.1.0-rc.1` | No | superseded |
| source builds off `main` | Best effort | report anyway, and name the commit |

There is no backport path. Fixes land on `main` and ship in the next candidate.

## Reporting A Vulnerability

**Do not open a public issue for a security report.**

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/kortiene/iroh-room/security/advisories/new).
That opens a private advisory visible only to you and the maintainers.

If GitHub is not usable for you, open a public issue that says only "requesting a
private security contact" with no technical detail, and a maintainer will
follow up.

Please include:

- the version or commit, your OS and architecture, and the network mode
- what an attacker gains, not only what misbehaves
- the smallest reproduction you have, ideally a test or a command sequence
- whether it needs room membership, an admin role, or only network position

Please do **not** include, exactly as in
[`COMMUNITY.md`](COMMUNITY.md#security-and-privacy-rules):

- `identity.secret`, full invite tickets (`roomtkt1...`), or capability secrets
- `rooms.db`, blob contents, unredacted `audit.ndjson`, or data-directory backups
- terminal transcripts containing any of the above

Redacted excerpts are enough. If a secret is genuinely load-bearing for the
repro, say so and a maintainer will arrange a channel for it.

## What To Expect

This is a single-maintainer project, so these are honest targets rather than
guarantees:

| Stage | Target |
| --- | --- |
| Acknowledgement | 5 business days |
| Initial assessment | 10 business days |
| Fix or documented mitigation | before the next release candidate, severity permitting |

There is no bug bounty. Reporters are credited in the release notes unless they
ask not to be.

We ask for coordinated disclosure: please give us 90 days, or until a fix ships,
whichever comes first. If a report turns out to describe an already-accepted
limitation (below), we will say so and close it rather than sit on it.

## Already-Known Limitations

These are documented, accepted beta postures, not vulnerabilities. Reports that
restate them will be closed with a pointer here — but a report showing one is
*worse than documented* is very welcome.

- **Plaintext local storage.** `rooms.db`, blobs, and identity material sit
  unencrypted in `IROH_ROOMS_HOME`; the data directory is a secret.
  ([`ADR-0001`](docs/decisions/ADR-0001-local-storage-posture.md))
- **No native invite revocation.** Invite tickets are password-grade bearer
  capabilities with a bounded-risk model.
  ([`ADR-0002`](docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md))
- **Persistent local audit.** `audit.ndjson` is retained locally and is not
  compliance-grade. ([`ADR-0003`](docs/decisions/ADR-0003-persistent-audit-posture.md))
- **Release artifact signing.** `v0.1.0-rc.1` through `v0.1.0-rc.5` carry
  SHA-256 checksums and no project signature. That was
  [`WAIVER-PBETA-0004`](docs/releases/v0.1.0-rc.5-production-beta-signoff.md).
  The waiver is eligible for retirement for
  [`v0.1.0-rc.6`](https://github.com/kortiene/iroh-room/releases/tag/v0.1.0-rc.6);
  retired when the release owner approves that sign-off. That release is
  signed. Verify it with
  [`scripts/verify-release-artifacts.sh`](scripts/verify-release-artifacts.sh)
  `--commit bf353b78f63b08ff5e3f425273aa3e0983167679 v0.1.0-rc.6 <TRIPLE>`.
  Pin Actions run
  [43835416665](https://github.com/kortiene/iroh-room/actions/runs/43835416665):

  ```bash
  jq -r .dsseEnvelope.payload <archive>.sigstore.json | base64 -d | jq -r .predicate.runDetails.metadata.invocationId
  ```

  That output must equal
  `https://github.com/kortiene/iroh-room/actions/runs/43835416665/attempts/1`.
  An archive whose SHA-256 matches the table in the rc.6 release notes is not
  an ungated run 43791394134 byte; those digests do not overlap.
  Evidence:
  [verification](https://github.com/kortiene/iroh-room/issues/248#issuecomment-6912690853)
  and the
  [publish record](https://github.com/kortiene/iroh-room/issues/248#issuecomment-6919904237).
  An earlier run, 43791394134, signed without approval. Its draft was deleted.
  Its attestations remain public with different digests.
  The signed release publishes a SLSA build-provenance attestation for each
  archive (`<archive>.sigstore.json`, checked with `gh attestation verify`)
  and a keyless cosign signature over `SHA256SUMS`
  (`SHA256SUMS.sigstore.json`, checked with `cosign verify-blob`). Those two
  `.sigstore.json` files are different bundles. The `SHA256SUMS` attestation
  bundle is not uploaded; `gh attestation verify SHA256SUMS` reads it from
  GitHub. The install steps are in
  [`docs/operations/install-uninstall.md`](docs/operations/install-uninstall.md).
  rc.1 through rc.5 fail that script at the attestation step by design.
- **Transport metadata is not private.** Payloads are encrypted in transit;
  relay and path metadata are not hidden.
- **Room content is not end-to-end encrypted.** v0.1 GA ships room content as
  plaintext, encrypted in transit only between admitted peers
  ([`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md)
  OD-1). The protocol and engine implement content-key rotation, but the
  shipped CLI has no rotation command and no way to turn on encrypted writes.
  `room remove` and `room leave` exist and do not rotate content keys: removed
  members lose access to new messages, files and pipes once peers see the
  removal; they keep what they already had. CLI-operated rooms store and
  exchange room content in plaintext form. End-to-end encryption is planned
  after GA (v0.2).

The full inventory, including threats that are only partially mitigated, is in
[`docs/security/threat-model.md`](docs/security/threat-model.md). Read it before
reporting — it is deliberately candid about what is unfinished.

## Scope

In scope: the protocol and event validation, membership and authorization, the
invite and ticket path, blob serve/fetch authorization, the live-pipe allow
list, the CLI, and the release artifacts published on this repository.

Out of scope: vulnerabilities in [iroh](https://github.com/n0-computer/iroh) and
other upstream dependencies (report those upstream, and tell us so we can pin or
patch), the spike crates under `archive/spikes/spike-*` (throwaway measurement
harnesses), and anything requiring an attacker to already control the victim's
local machine or data directory.

## Non-Security Bugs

For ordinary defects, use the
[bug report template](https://github.com/kortiene/iroh-room/issues/new/choose).
The same redaction rules apply.
