# ADR-0003: Persistent Local Audit Posture For Production Beta

Status: Accepted for Phase 2.5 Production Beta
Date: 2026-07-07
Owners: Release owner, security reviewer

## Context

Developer Preview relied heavily on stderr audit lines and in-process tracing
callbacks. That is useful while an operator is watching a terminal, but it is
not enough for production incident reconstruction. A user may need to answer
basic questions after a run has ended:

- Which peer was rejected before event bytes were accepted?
- Was a join-bootstrap peer admitted, blocked, or upgraded?
- Did a blob request fail because the peer was inactive, unauthorized, or the
  hash was not referenced?
- Did a pipe reject a connector or tear down a live session after removal?

The CLI now writes security and lifecycle callbacks to a local append-only
JSON-lines file at `<IROH_ROOMS_HOME>/audit.ndjson` through the explicit CLI
audit sink.

## Decision

For Phase 2.5 Production Beta, Iroh Rooms accepts a **persistent local audit**
posture:

> Iroh Rooms records local audit events to `audit.ndjson` for post-run incident
> reconstruction. The audit log is local, best-effort, append-oriented, and
> privacy-sensitive. It is not remote, centrally retained, tamper-evident,
> automatically rotated, or safe to paste publicly without redaction.

This is sufficient for the small-room, CLI-first beta scope. Production GA
should revisit retention, rotation, tamper evidence, export tooling, and
optional centralized collection if the support model expands.

## Audit Data Covered

The current local audit file records minimized operational metadata for:

- peer admission and rejection;
- peer connect, disconnect, offline, and deauthorization callbacks;
- event rejection counters and advisory flags;
- join-bootstrap admitted, blocked, and upgraded callbacks;
- blob serve accepted/rejected callbacks;
- pipe opened, closed, connect accepted/rejected, and torndown callbacks.

Expected fields include `ts_ms`, `event`, public peer/device identifiers,
identity identifiers where already public, pipe ids, denial causes, counts, and
short blob hash prefixes.

## Explicit Non-Goals

The Production Beta audit file is not:

- a security information and event management system;
- a remote audit trail;
- tamper-evident storage;
- an authoritative compliance record;
- a complete packet or message log;
- a substitute for signed room events;
- safe to attach to public issues without review.

## Privacy And Secret Handling

The audit sink must not record:

- identity secret seeds;
- device secret seeds;
- full invite tickets;
- invite capability secrets;
- message bodies;
- blob bytes;
- local filesystem paths;
- raw data-directory backups.

`audit.ndjson` may still reveal sensitive operational metadata: room
relationships, peer identifiers, pipe ids, timing, denial reasons, and blob hash
prefixes. Treat it as private support data.

## Retention And Permissions

For Production Beta:

- `audit.ndjson` is stored under the local data directory.
- On Unix, the file is created/tightened to `0600`; the containing data
  directory is owner-only where supported.
- Records are appended and flushed by the CLI sink.
- Retention and deletion are user-managed.
- There is no automatic rotation.
- There is no automatic upload.

Users who need to retain audit records should back up `audit.ndjson` with the
same care as `rooms.db`, but should redact it before sharing.

## Operational Guidance

Support and release docs must tell users:

1. Preserve `audit.ndjson` privately when investigating access, blob, pipe,
   join-bootstrap, or diagnostics incidents.
2. Do not attach raw `audit.ndjson` to public issues.
3. Redact peer ids, pipe ids, room ids, and timing if they reveal sensitive
   relationships.
4. Never include full tickets, identity secrets, device secrets, `rooms.db`,
   blobs, or full data-directory backups unless a private support path has been
   agreed.

## Required Evidence

The following evidence must stay green before a Production Beta label:

- CLI unit tests prove `PersistentAudit` appends valid NDJSON.
- CLI unit tests prove peer/blob callbacks persist expected event names and
  minimized fields.
- CLI unit tests prove pipe callbacks persist `pipe.opened`,
  `pipe.connect.rejected`, and `pipe.torndown`.
- Secret-free output tests remain green for ticket, identity, diagnostics,
  audit, and error paths.
- Release docs and bug templates warn users not to share raw audit logs or data
  directories publicly.

Focused commands:

```bash
cargo test -p iroh-rooms-cli audit::
cargo test -p iroh-rooms-cli pipe::tests::local_pipe_audit_persists_ndjson_events
cargo test -p iroh-rooms-cli --test diagnostics_cli
cargo test -p iroh-rooms-cli --test error_taxonomy
cargo test -p iroh-rooms-cli --test docs_conformance
```

## Alternatives Considered

### Keep stderr-only audit

Pros:

- Simple and already familiar to operators.
- No local file handling or privacy caveat.

Cons:

- Loses evidence after the terminal session ends.
- Weak incident reconstruction for unauthorized access attempts.
- Fails the production P0.6 requirement.

Decision: rejected.

### Centralized audit collection

Pros:

- Better support visibility.
- Stronger retention and correlation story.

Cons:

- Conflicts with local-first/no-central-application-server positioning.
- Introduces privacy, consent, authentication, retention, and breach risk.
- Too operationally heavy for the Phase 2.5 beta scope.

Decision: defer.

### Tamper-evident local audit chain

Pros:

- Stronger forensic integrity.
- Better long-term enterprise posture.

Cons:

- Does not prevent a compromised local account from deleting the file.
- Requires keying, rotation, export, and verification design.
- Adds complexity before the beta support model is proven.

Decision: defer to GA or enterprise planning.

## Implementation Notes

This ADR is a release contract. The production preflight must verify:

- `docs/decisions/ADR-0003-persistent-audit-posture.md` exists.
- `PRODUCTION-READINESS.md` links ADR-0003 for P0.6 / PR-0005.
- `docs/security/threat-model.md` marks persistent audit posture as accepted
  for Production Beta.
- `docs/operations/data-handling.md` documents audit handling and links this
  ADR.
- `.github/ISSUE_TEMPLATE/bug_report.md` warns against unredacted
  `audit.ndjson`.

## Review Triggers

Revisit this ADR when any of the following occurs:

- Production support needs remote diagnostic bundles.
- A beta incident needs stronger forensic integrity.
- Users ask for automatic audit rotation or retention policy.
- The product adds managed hosting, team administration, or enterprise support.
- A security review rejects local-only append-oriented audit as sufficient.

## Final Recommendation

Proceed with Production Beta using local `audit.ndjson` as a scoped operational
trail, with explicit privacy and retention caveats. Do not market it as
tamper-evident, centrally retained, or compliance-grade.
