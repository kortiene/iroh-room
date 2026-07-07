# ADR-0001: Local Storage Posture For Production Beta

Status: Accepted for Phase 2.5 Production Beta  
Date: 2026-07-07  
Owners: Release owner, security reviewer

## Context

Iroh Rooms is local-first. The CLI stores identity metadata, signing secrets,
room events, blob bytes, downloads, and audit records under the resolved data
directory (`--data-dir`, `IROH_ROOMS_HOME`, or the platform default).

The current implementation does not encrypt the room database, blobs, or audit
log at rest. Unix file permissions protect the data directory and identity
secrets, but that is not equivalent to encryption. A compromised local user
account, malware process, unencrypted disk backup, or operator with filesystem
access can read local room history and blob contents.

Production Beta needs a truthful security claim before it needs a heavier local
encryption subsystem. Shipping encryption prematurely would add key-management,
backup, restore, migration, and recovery risk while the product is still
validating its small-room beta scope.

## Decision

For Phase 2.5 Production Beta, Iroh Rooms will ship with a scoped local-storage
claim:

> Iroh Rooms keeps room data local by default, but it does not protect local
> room history, blob contents, or audit records from a compromised local user
> account, malware, unencrypted backup, or device with filesystem access.

Storage encryption is deliberately deferred for Production Beta. The beta is
acceptable only for trusted local machines and users who understand that local
room data is plaintext.

Production GA remains blocked unless one of these is true:

1. Local storage encryption is implemented with documented key management,
   backup, restore, migration, and recovery behavior.
2. The GA release scope is explicitly narrowed enough that plaintext local
   storage remains a deliberate product constraint, not an accidental security
   gap.

## Data Covered By This Decision

Plaintext or filesystem-readable local data includes:

- `rooms.db`: signed room events, messages, membership state, file references,
  pipe events, and sync state.
- `blobs/`: imported/shared file bytes and blob provider state.
- `audit.ndjson`: local peer/blob/join-bootstrap/pipe audit records.
- Downloads/output files saved by the user.

Secret-bearing data includes:

- `identity.secret`: identity and device secret seeds.
- Invite ticket strings and capability secrets when held transiently by the
  operator or process.

The decision does not permit logging or printing secrets. Secret hygiene remains
mandatory.

## Controls Required For Production Beta

The following controls are required before a Production Beta label:

- The release notes must state the plaintext local storage caveat before any
  install/run instructions.
- `docs/operations/data-handling.md` must document backup, restore, export,
  deletion, audit log handling, and sensitive support-bundle rules.
- Bug report templates must warn users not to attach `identity.secret`, full
  invite tickets, `rooms.db`, blobs, full data-directory backups, or unredacted
  audit logs.
- Identity secret files and the data directory must continue to use owner-only
  permissions where the platform supports them.
- Tests must continue to cover secret-free CLI output for identity, invite,
  diagnostics, audit, and error paths.
- `scripts/production-readiness.sh` must fail if this ADR or its required
  release/documentation references are missing.

## Security Consequences

Accepted for beta:

- A local attacker with filesystem access can read room history and blob bytes.
- Backups containing `identity.secret`, `rooms.db`, blobs, or `audit.ndjson`
  are sensitive and must be encrypted or privately handled.
- `audit.ndjson` improves incident reconstruction but is local, not
  tamper-evident, centrally retained, or safe to paste publicly unredacted.

Not accepted:

- Claiming local compromise resistance.
- Claiming encrypted room history or encrypted blob storage.
- Asking users to attach raw data directories to public issues.
- Treating filesystem permissions as a substitute for encryption in GA claims.

## Alternatives Considered

### Implement storage encryption before beta

Pros:

- Stronger local confidentiality claim.
- Better alignment with a broader production audience.

Cons:

- Requires key management, recovery, backup, migration, and support design.
- Increases risk of data loss during a beta that still needs product evidence.
- Does not solve compromised-process exfiltration while the app is running.

Decision: defer to GA planning unless beta feedback changes the audience or
risk profile.

### Rely on filesystem permissions without an explicit caveat

Pros:

- No engineering work.

Cons:

- Overclaims security by omission.
- Creates support and trust risk when users discover plaintext local data.

Decision: rejected.

### Narrow the product to non-sensitive demo data only

Pros:

- Avoids a security promise.
- Reduces support risk.

Cons:

- Undermines the purpose of a production beta for real small-room workflows.
- Still requires documentation because metadata and identities remain sensitive.

Decision: not the Phase 2.5 beta target.

## Implementation Notes

This ADR is a release contract, not only documentation. The production preflight
must verify:

- `docs/decisions/ADR-0001-local-storage-posture.md` exists.
- The ADR contains the scoped claim "trusted local machines".
- `RELEASE-READINESS.md` carries the unencrypted local storage limitation.
- `docs/operations/data-handling.md` documents plaintext storage and backup
  handling.
- `.github/ISSUE_TEMPLATE/bug_report.md` warns against attaching sensitive local
  data.

## Review Triggers

Revisit this ADR when any of the following occurs:

- A beta cohort includes non-technical users, regulated data, shared devices, or
  managed enterprise environments.
- Multi-device recovery, cloud backup, or mobile/desktop apps enter scope.
- The release wants to remove the plaintext local storage caveat.
- A security review rejects plaintext local storage for the stated audience.
- Support incidents show users misunderstand the local compromise model.

## Final Recommendation

Proceed with Production Beta only under the trusted-local-machine scope, with
prominent release notes and operational documentation. Do not market the beta as
protecting data from local compromise. Treat encrypted local storage as a
Production GA decision unless the beta audience requires it sooner.
