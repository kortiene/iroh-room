# Security Policy

Iroh Rooms is a pre-1.0 Production Beta for a small cohort of technical builders.
It has not had an independent security review
([`WAIVER-PBETA-0003`](docs/releases/v0.1.0-rc.5-production-beta-signoff.md)).
Treat it accordingly, and please report what you find.

## Supported Versions

Only the newest published release candidate receives security fixes.

| Version | Supported | Notes |
| --- | --- | --- |
| `v0.1.0-rc.5` | Yes | current candidate; the only release-owner-approved build since `rc.1` |
| `v0.1.0-rc.2` .. `v0.1.0-rc.4` | No | published, but their sign-offs were never completed; upgrade to `rc.5` |
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
- **Unsigned release artifacts.** Archives carry SHA-256 checksums but no project
  signature ([`WAIVER-PBETA-0004`](docs/releases/v0.1.0-rc.5-production-beta-signoff.md)).
  Verify checksums against the release page.
- **Transport metadata is not private.** Payloads are encrypted in transit;
  relay and path metadata are not hidden.
- **Content-key rotation is not reachable from the CLI.** The protocol and
  engine implement it, but the shipped CLI exposes no command to trigger it, so
  CLI-operated rooms store and exchange room content in plaintext form.

The full inventory, including threats that are only partially mitigated, is in
[`docs/security/threat-model.md`](docs/security/threat-model.md). Read it before
reporting — it is deliberately candid about what is unfinished.

## Scope

In scope: the protocol and event validation, membership and authorization, the
invite and ticket path, blob serve/fetch authorization, the live-pipe allow
list, the CLI, and the release artifacts published on this repository.

Out of scope: vulnerabilities in [iroh](https://github.com/n0-computer/iroh) and
other upstream dependencies (report those upstream, and tell us so we can pin or
patch), the spike crates under `crates/spike-*` (throwaway measurement
harnesses), and anything requiring an attacker to already control the victim's
local machine or data directory.

## Non-Security Bugs

For ordinary defects, use the
[bug report template](https://github.com/kortiene/iroh-room/issues/new/choose).
The same redaction rules apply.
