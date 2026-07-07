# Data Handling, Backup, Restore, And Export

Status: Phase 2.5 Production Beta policy draft.

This document defines how Iroh Rooms handles local data for a production-grade
post-MVP release. It is intentionally conservative: the current product is
local-first and does not upload room data to a central application server, but
local data is not encrypted at rest today. The Phase 2.5 storage decision record
is `docs/decisions/ADR-0001-local-storage-posture.md`; the persistent local
audit posture is accepted in
`docs/decisions/ADR-0003-persistent-audit-posture.md`.

## Summary

Iroh Rooms stores room state locally. That is a product feature, not a complete
security guarantee.

Current production posture:

- Room events are stored in local SQLite.
- Shared blobs/artifacts are stored in the local blob directory.
- Identity and device secrets are stored in local identity files.
- File permissions protect identity files on Unix.
- Network, blob, join-bootstrap, and pipe audit callbacks are appended to a
  local `audit.ndjson` file.
- `rooms.db` and blobs are plaintext at rest.
- There is no secure cloud backup, recovery phrase, or multi-device recovery.

Production Beta may proceed only with a scoped claim:

> Iroh Rooms keeps room data local by default. It does not protect local room
> history or blob contents from a compromised local user account, malware, or a
> device with filesystem access.

ADR-0001 accepts that scope for Production Beta only. Production GA should
either add storage encryption or keep the deployment model explicitly limited to
trusted local machines.

## Local Data Inventory

Data directory resolution is:

1. `--data-dir <PATH>`
2. `IROH_ROOMS_HOME`
3. Platform default

The current CLI stores data under this resolved home.

| Path | Contains | Sensitivity | Backup? | Notes |
| --- | --- | --- | --- | --- |
| `identity.json` | Public profile: name, identity ID, device ID | Low | Yes | Safe to inspect; no secret material. |
| `identity.secret` | Identity and device secret seeds | Critical | Optional, with care | Required to keep the same identity/device. Never paste into issues. |
| `rooms.db` | Signed room events, membership, messages, file refs, pipe events, sync state | High | Yes | Plaintext today. Authoritative room log lives here. |
| `blobs/` or blob store path | Shared/imported file bytes and provider state | High | Yes | Plaintext today; may contain sensitive artifacts. |
| `audit.ndjson` | Local peer/blob/join-bootstrap/pipe audit records | Medium | Optional | No secrets by design, but contains peer IDs, pipe IDs, timing, and access decisions. Redact before sharing. |
| Downloads/output files | Fetched artifacts saved by user | High | User-managed | Outside room store once saved. |
| stderr logs / terminal transcripts | Diagnostics, warning/error lines, peer IDs | Medium | No by default | Redact before sharing. |

## Data Classification

| Class | Examples | Handling rule |
| --- | --- | --- |
| Secret | identity secret seed, device secret seed, invite capability secret, full ticket token | Never log, paste, commit, or include in bug reports. |
| Sensitive room data | messages, `rooms.db`, blob contents, file names, agent status text | Treat as private project data. |
| Operational metadata | room ID, event IDs, device IDs, endpoint IDs, peer diagnostics, pipe IDs | Usually safe for maintainers but may reveal relationships/timing. Redact when unsure. |
| Public docs/config | README, PRD, protocol docs, test fixtures | Safe to share. |

## Audit Log Policy

`audit.ndjson` is a local append-only JSON-lines file created under the data
directory when networked CLI commands run. Its Production Beta posture is
accepted in `docs/decisions/ADR-0003-persistent-audit-posture.md`.

Current behavior:

- It records event names such as `peer.rejected`, `event.rejected`,
  `join.bootstrap.admitted`, `blob.serve.rejected`, `pipe.connect.rejected`,
  and `pipe.torndown`.
- It may include public identity IDs, endpoint IDs, pipe IDs, reason codes,
  counts, and shortened blob hash prefixes.
- It does not record identity secret seeds, device secret seeds, invite
  capability secrets, full invite tickets, blob bytes, message bodies, or local
  filesystem paths.
- On Unix the file is created/tightened to `0600`; the containing data
  directory is `0700`.
- Retention and rotation are user-managed today. There is no automatic upload,
  central collection, tamper-evident storage, or remote retention.

Operational rule: include `audit.ndjson` when incident reconstruction matters;
exclude or redact it when sharing a support bundle publicly.

## Backup Policy

Backups are user-managed today.

Minimum safe backup for continuity:

```text
<IROH_ROOMS_HOME>/identity.json
<IROH_ROOMS_HOME>/identity.secret
<IROH_ROOMS_HOME>/rooms.db
<IROH_ROOMS_HOME>/blobs/
```

Rules:

1. Stop long-running `iroh-rooms room tail`, `pipe expose`, and agent processes
   before copying the data directory.
2. Copy the whole directory, not individual files, unless you know the room does
   not reference local blobs.
3. Store backups in an encrypted location if they include `identity.secret`,
   `rooms.db`, or blobs.
4. Never attach backups to GitHub issues or support tickets.
5. Record the binary version used to create the backup.
6. Include `audit.ndjson` only when audit history is needed; redact it before
   sharing outside the trusted support channel.

Example local backup:

```bash
export IROH_ROOMS_HOME="$HOME/.local/share/iroh-rooms"
mkdir -p "$HOME/iroh-rooms-backups"
tar -czf "$HOME/iroh-rooms-backups/iroh-rooms-$(date +%Y%m%d-%H%M%S).tar.gz" \
  -C "$(dirname "$IROH_ROOMS_HOME")" \
  "$(basename "$IROH_ROOMS_HOME")"
```

This archive is sensitive. Encrypt it before moving it to shared storage.

## Restore Policy

Restore should be explicit and reversible.

Recommended restore flow:

1. Stop all Iroh Rooms processes.
2. Move the current data directory aside.
3. Restore the backup directory into the expected path.
4. Run read-only commands first:
   - `iroh-rooms identity show`
   - `iroh-rooms room members <ROOM_ID>`
   - `iroh-rooms room tail <ROOM_ID> --offline`
5. Only then resume networked commands.

Example:

```bash
export IROH_ROOMS_HOME="$HOME/.local/share/iroh-rooms"
mv "$IROH_ROOMS_HOME" "$IROH_ROOMS_HOME.before-restore"
mkdir -p "$(dirname "$IROH_ROOMS_HOME")"
tar -xzf ./iroh-rooms-backup.tar.gz -C "$(dirname "$IROH_ROOMS_HOME")"
iroh-rooms identity show
```

If restore fails, move the restored directory aside and put
`$IROH_ROOMS_HOME.before-restore` back.

## Export Policy

There is no polished production export command yet. Until one exists, export is
defined as a copy of the local data directory plus a note that the exported
archive contains sensitive data.

Production Beta requirement:

- Release notes must say that export is low-level and sensitive.
- Bug reports must not request raw exports by default.
- Any maintainer request for `rooms.db` or blobs must include a written
  sensitivity warning and a private transfer path.

Production GA recommendation:

- Add `iroh-rooms room export <ROOM_ID>` with a secret-free metadata option and
  an explicit `--include-secrets` or equivalent for full continuity export.

## Deletion Policy

There is no complete user-controlled deletion surface yet.

Current behavior:

- Users can delete local files/directories manually.
- Deleting local data does not delete copies held by other peers.
- Deleting a blob locally may make future fetches unavailable to peers relying
  on that provider.

Production Beta release notes must state:

> Deleting local data affects only this device. Iroh Rooms does not remotely
> delete copies already held by other peers.

Production GA recommendation:

- Add explicit local deletion commands.
- Add `file unpin` or blob retention policy.
- Add room archive/export/delete runbook.

## Storage Encryption Decision

Current decision: accepted for Production Beta in
`docs/decisions/ADR-0001-local-storage-posture.md`.

Production Beta stance:

> Production Beta may proceed with plaintext storage only if release notes,
> install docs, and first-run/onboarding language state the limitation before
> users create identities or rooms.

Production GA stance:

> Production GA should not claim local compromise resistance unless `rooms.db`,
> blobs, and identity secrets have an encryption and recovery model.

## Secret Handling Rules

Never share:

- `identity.secret`
- raw invite tickets
- invite capability secrets
- full terminal logs containing ticket output
- full `rooms.db` or blobs without an explicit private support path

Usually safe to share:

- `iroh-rooms --version`
- command line without tokens
- redacted `error[code]` and `warning[code]` lines
- event IDs and room IDs only when the room relationship itself is not sensitive

Redaction examples:

```text
ticket: roomtkt1<redacted>
identity.secret: <do not attach>
peer: <endpoint-id-prefix-only>
pipe_id: <first-8-hex-only>
```

## Suspected Invite Ticket Leak

Phase 2.5 Production Beta uses the bounded leaked-ticket model accepted in
`docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`. There is no
native ticket-specific revocation event.

If a ticket may have leaked:

1. Stop sharing the leaked ticket and do not paste it into issues, chat, logs, or
   support bundles.
2. Check whether the ticket has an expiry. If it was issued without expiry or
   with a long expiry, treat the exposure as active until the bound invitee has
   either joined and been reviewed, or the invitee has been removed.
3. If the bound identity should not join, keep the admin online long enough to
   publish `member.removed` for that identity if it appears or may already have
   joined.
4. Issue a fresh invite only to the intended identity, preferably with a short
   `--expires` window.
5. Ask peers to reconnect or sync so they learn any removal. Revocation of blob
   and pipe access is effective when the enforcing peer learns the removal.
6. Preserve `audit.ndjson` privately if incident reconstruction matters; redact
   ticket strings before sharing any excerpts.

Support response language:

> Invite tickets are password-grade capabilities. Production Beta does not have
> native ticket-specific revocation. The practical response is to stop using the
> leaked ticket, rely on expiry where present, remove the bound identity if it
> should not remain in the room, and issue a fresh short-lived invite.

## Upgrade And Rollback

Before upgrade:

1. Run `scripts/release-readiness.sh` on the candidate if building locally.
2. Back up the data directory.
3. Record the current binary version.

After upgrade:

1. Run `iroh-rooms identity show`.
2. Run `iroh-rooms room members <ROOM_ID>`.
3. Run `iroh-rooms room tail <ROOM_ID> --offline --limit 10`.

Rollback:

1. Stop all Iroh Rooms processes.
2. Restore the previous binary.
3. If the new binary migrated storage in a way the old binary cannot read,
   restore the pre-upgrade data backup.

Production Beta limitation:

- The current core compatibility fixture is documented in
  `docs/compatibility.md`. Each production candidate must still state whether
  it can read prior candidate data and whether rollback requires restoring the
  pre-upgrade data backup.

## Production Sign-Off Checklist

For each Production Beta candidate:

- [ ] Release notes state local storage is plaintext unless encryption has
      shipped.
- [ ] Backup and restore instructions were tested on the candidate.
- [ ] A sample backup excludes unrelated local files.
- [ ] Bug report template warns users not to attach secrets, `rooms.db`, or
      blobs.
- [ ] Compatibility policy says whether the candidate can read prior data.
- [ ] Rollback path is tested or a rollback limitation is disclosed.
