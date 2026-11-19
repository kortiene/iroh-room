# Production Beta Release Notes Template

Copy this template for every Production Beta candidate. Known limitations must
stay above install/run instructions.

```markdown
# Iroh Rooms <VERSION> Production Beta

Release date: <YYYY-MM-DD>
Commit SHA: <FULL_SHA>
Release owner: <NAME>
Supported artifacts: <platform triples>

## Release Class

Production Beta for small private CLI-first rooms used by technical users.

## Supported Scope

This release supports:

- Identity creation.
- Private room creation.
- Key-bound room invites and joins.
- Signed room messages.
- Verified file/artifact sharing.
- Authenticated loopback-only live TCP pipes with explicit per-member allow
  lists.
- Explicitly invited agent participants and signed `agent.status` updates.

## Platforms And glibc Floor

The Linux glibc floor is 2.34. On both Linux triples the max `GLIBC_` symbol,
measured by
`readelf -V "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1`
(record field `glibc_method`), is `GLIBC_2.34`. Coverage for that floor is
RHEL 9, Debian 12, and Ubuntu 22.04 and newer. Ubuntu 20.04, Debian 11, and
RHEL 8 are not supported.

Lifecycle run
[44541017067](https://github.com/kortiene/iroh-room/actions/runs/44541017067)
is attempt 1, `workflow_dispatch` on `main` at
`ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26`. The run conclusion is `success`.
Prior tag `v0.1.0-rc.5`. Candidate tag `v0.1.0-rc.6`. The per-triple JSON is
the source of truth. `Run`, `Attempt`, and `SHA` are `github_run_id`,
`github_run_attempt`, and `github_sha`. `ImageOS` and `ImageVersion` are
`image_os` and `image_version`.

These four rows are GitHub-hosted runners (owner decision Q2). Each record
has `runner_hosted` `github-hosted`, `runner_environment` `github-hosted`,
`class` `evidence`, `evidence_grade` true, and `result` `PASS`.

| Runner | Triple | Class | Result | Run | Attempt | SHA | ImageOS | ImageVersion | glibc_max_symbol | ldd --version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `macos-15-intel` | `x86_64-apple-darwin` | `evidence` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `macos15` | `20260824.0482.1` | `n/a (Mach-O)` | `n/a` |
| `macos-15` | `aarch64-apple-darwin` | `evidence` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `macos15` | `20261118.0550.1` | `n/a (Mach-O)` | `n/a` |
| `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | `evidence` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `ubuntu24` | `20261111.370.1` | `GLIBC_2.34` | `ldd (Ubuntu GLIBC 2.39-0ubuntu8.9) 2.39` |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` | `evidence` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `ubuntu24-arm64` | `20261111.182.1` | `GLIBC_2.34` | `ldd (Ubuntu GLIBC 2.39-0ubuntu8.9) 2.39` |

Both 22.04 probe cells passed (`x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu`). `class` is `probe`. `ldd --version` reports
glibc 2.35. `evidence_grade` is false.

| Runner | Triple | Class | Result | Run | Attempt | SHA | ImageOS | ImageVersion | glibc_max_symbol | ldd --version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `probe` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `ubuntu22` | `20261111.360.2` | `GLIBC_2.34` | `ldd (Ubuntu GLIBC 2.35-0ubuntu3.15) 2.35` |
| `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | `probe` | `PASS` | `44541017067` | `1` | `ec4bc744f9a514ab4116bf20f1cf2fbc5ce47d26` | `ubuntu22-arm64` | `20261116.196.1` | `GLIBC_2.34` | `ldd (Ubuntu GLIBC 2.35-0ubuntu3.18) 2.35` |

A 22.04 pass shows the binary runs there; it does not prove the 2.34 floor itself.

## Known Limitations

State these before install/run commands:

- No central application server.
- No guaranteed offline message delivery.
- Local storage is plaintext unless this release explicitly says otherwise.
  The per-epoch room content keys are stored in the `room_keys` table **inside
  `rooms.db` itself** — there is no separate key store. Any copy, backup, or
  read of `rooms.db` therefore includes both the ciphertext and the keys
  needed to decrypt all covered history, so the database must be handled as a
  secret (protected only as strongly as the identity keys, ADR-0001 scope;
  threat-model T28).
- Invite tickets are password-grade capabilities.
- No native ticket-specific invite revocation if ADR-0002 remains in force.
- **Content-key rotation (issue #191) constraints:** rotation is admin-driven
  under a single immutable admin. An admin-offline removal/leave stalls
  forward secrecy until the admin returns (T29). A voluntary `member.left`
  revokes *access* immediately, but the departed member keeps its old epoch
  key and can keep decrypting new content until the admin explicitly authors
  and distributes a rotation that excludes it — **no automatic rotation fires
  on the admin's next event in v1**, so this window is open-ended until an
  operator rotates (T29). Metadata — event existence, type, timing, the
  causal DAG, and an often-effectively-exact body length — stays cleartext
  (T30); length-hiding padding is out of v1 scope. Rotation requires every
  member past the step-6 compatibility floor (reader-first rollout, spec
  `content-key-rotation.md` D8).
- `audit.ndjson` is local best-effort audit, not remote, centrally retained,
  tamper-evident, or compliance-grade audit if ADR-0003 remains in force.
- Binary artifacts for rc.1 through rc.5 are checksum-only
  (WAIVER-PBETA-0004, retired for v0.1.0-rc.6 when the release owner approved
  that sign-off on 2026-11-16). `v0.1.0-rc.6` is the first
  published signed release; verify it with `scripts/verify-release-artifacts.sh`.
  Pin run 43835416665 with
  `jq -r .dsseEnvelope.payload <archive>.sigstore.json | base64 -d | jq -r .predicate.runDetails.metadata.invocationId`,
  which must print
  `https://github.com/kortiene/iroh-room/actions/runs/43835416665/attempts/1`.
  This jq read only means something after `scripts/verify-release-artifacts.sh`
  has verified that same bundle, because reading the payload alone does not
  check its signature or provenance.
  An archive whose SHA-256 matches the rc.6 notes table is not an ungated
  run 43791394134 byte. State below whether this candidate is signed the
  same way, and name the workflow run a checker must pin.
- SDK package publication status: <source-only publish=false / published>.
- Rollback support: <without data restore / requires pre-upgrade backup /
  not supported>.

## Security And Privacy Notes

- Storage posture: <ADR-0001 scoped plaintext beta / encrypted / blocked>.
- Invite posture: <ADR-0002 bounded leaked-ticket model / native revocation>.
- Audit posture: <ADR-0003 local audit / stronger audit>.
- Content confidentiality posture: <content-key rotation enabled (T27
  Controlled for the malicious-reader case; residuals T28/T29/T30) / rotation
  not yet enabled — admission-based confidentiality only>.
- Security reporting path: <PRIVATE_CONTACT_OR_PROCESS>.

## Compatibility And Migration

- Event schema version: <VALUE>.
- WireEvent version: <VALUE>.
- SQLite user_version: <VALUE>.
- Compatibility fixture gate:
  `cargo test -p iroh-rooms-core --features store --test compatibility`
  <PASS/FAIL>.
- Previous-candidate data fixture: <not applicable first beta / path / waived>.

## Gate Output

Paste exact final lines:

```text
verify: <summary>
release-readiness: <READY/NOT READY>
production-readiness: <AUTOMATED PREFLIGHT PASS/FAIL>
```

## Artifacts And Checksums

Published artifacts come from the `release` workflow at tag `v<VERSION>`, one
native runner per triple. `scripts/build-release-artifacts.sh --version <VERSION>`
is the same build run locally and only produces the rustc host triple; it does
not sign anything.

| Artifact | Platform | SHA-256 |
| --- | --- | --- |
| `iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz` | `<TRIPLE>` | `<SHA256>` |

A release built by the signing workflow also attaches, for each archive, the
`.sha256` line and `<archive>.sigstore.json` (a GitHub build-provenance
attestation bundle, SLSA, verified with `gh attestation verify`), plus
`SHA256SUMS` and `SHA256SUMS.sigstore.json` (a cosign keyless signature bundle,
verified with `cosign verify-blob`). Those two `.sigstore.json` files are
different kinds of bundle. The `SHA256SUMS` attestation bundle is not
uploaded; `gh attestation verify SHA256SUMS` reads it from GitHub.

Project signing: <checksum-only, rc.5 and earlier / release built by the signing workflow, starting with the first release after rc.5>.

## Install

See `docs/operations/install-uninstall.md`.

For a release built by the signing workflow, verify before install:

```bash
scripts/verify-release-artifacts.sh v<VERSION> <TRIPLE>
```

rc.1 through rc.5 fail that script at the attestation step by design. On those
candidates, check the per-archive checksum and then install.

Minimum binary install:

```bash
shasum -a 256 -c iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz.sha256
tar -xzf iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz
install -m 0755 iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>/iroh-rooms \
  "$HOME/.local/bin/iroh-rooms"
iroh-rooms --version
```

Use `sha256sum -c` on Linux if `shasum` is unavailable.

## Uninstall

```bash
rm -f "$HOME/.local/bin/iroh-rooms"
```

To remove local data, follow `docs/operations/install-uninstall.md`; do not
delete local data unless you accept losing local room state and identity
secrets.

## Rollback

Rollback across this release is: <SUPPORTED/REQUIRES_BACKUP/NOT_SUPPORTED>.

Steps:

1. Stop all Iroh Rooms commands.
2. Reinstall the previous binary.
3. Restore the pre-upgrade data backup if this release changed storage in a way
   the previous binary cannot read.
4. Run `iroh-rooms identity show`.

## Sign-Off

Production sign-off record:
`docs/operations/release-operations.md` template filled at <PATH_OR_LINK>.
```
