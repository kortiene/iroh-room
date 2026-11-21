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

## Known Limitations

State these before install/run commands:

- No central application server.
- No guaranteed offline message delivery.
- Local storage is plaintext unless this release explicitly says otherwise.
  CLI-operated rooms store room content in plaintext (ADR-0014 OD-1). The
  `room_keys` table inside `rooms.db` stays empty unless an embedder drives
  the experimental rotation API; if it does, those keys live in `rooms.db`
  and the database is a secret (ADR-0001; threat-model T28).
- Invite tickets are password-grade capabilities.
- No native ticket-specific invite revocation if ADR-0002 remains in force.
- **Content encryption is not a shipped CLI capability.** v0.1 ships room
  content encrypted in transit only (ADR-0014 OD-1). `room remove` and
  `room leave` do not rotate keys. Removed members lose access to new
  messages, files, and pipes once peers see the removal; they keep what they
  already had. The content-key rotation lifecycle remains in the protocol
  and the sync engine, reachable from `iroh_rooms::experimental::rotation`,
  not from the shipped CLI and not from the stable SDK. Do not describe an
  operator rotation command, a room opt-in, or forward secrecy for CLI
  rooms: none of those exist. For an embedder who drives that experimental
  API, the rc.5 constraints still apply: a single immutable admin, an
  admin-offline stall (T29), no automatic rotation on `member.left` (T29),
  cleartext metadata (T30), and a reader-first floor (spec
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
- Content confidentiality posture: plaintext room content, encrypted in
  transit only between admitted peers (ADR-0014 OD-1). Do not claim
  content-key rotation, a room opt-in, or forward secrecy for the shipped CLI.
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
