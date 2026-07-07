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
- Invite tickets are password-grade capabilities.
- No native ticket-specific invite revocation if ADR-0002 remains in force.
- `audit.ndjson` is local best-effort audit, not remote, centrally retained,
  tamper-evident, or compliance-grade audit if ADR-0003 remains in force.
- Binary artifacts are checksummed but not project-signed unless signing is
  explicitly listed below.
- SDK package publication status: <source-only publish=false / published>.
- Rollback support: <without data restore / requires pre-upgrade backup /
  not supported>.

## Security And Privacy Notes

- Storage posture: <ADR-0001 scoped plaintext beta / encrypted / blocked>.
- Invite posture: <ADR-0002 bounded leaked-ticket model / native revocation>.
- Audit posture: <ADR-0003 local audit / stronger audit>.
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

Artifacts were built with:

```bash
scripts/build-release-artifacts.sh --version <VERSION>
```

| Artifact | Platform | SHA-256 |
| --- | --- | --- |
| `iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz` | `<TRIPLE>` | `<SHA256>` |

Project signing: <not signed / signed with ...>.

## Install

See `docs/operations/install-uninstall.md`.

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
