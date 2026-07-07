# Compatibility And Migration Policy

Status: Phase 2.5 Production Beta policy with v1 fixture gate implemented.

This document defines compatibility expectations for the Iroh Rooms protocol,
local store, CLI, and SDK. It is required before any Production Beta label.

## Summary

Iroh Rooms has a strong deterministic protocol core, but production-grade
post-MVP releases need version continuity:

- A room created by one production candidate should remain readable by the next.
- Event validation rules should not silently change.
- SQLite migrations should be explicit, tested, and recoverable.
- Stable SDK APIs should have a semver policy.
- Experimental runtime APIs should be allowed to change, but must be labeled.

Current state:

- Event `schema_version = 1`.
- `WireEvent` envelope version is `1`.
- Protocol conformance and golden vectors exist.
- Versioned v1 event fixtures exist at
  `crates/iroh-rooms-core/tests/fixtures/v1/events.txt`.
- A v1 SQLite schema fixture exists at
  `crates/iroh-rooms-core/tests/fixtures/v1/store_v1_schema.sql`.
- `cargo test -p iroh-rooms-core --features store --test compatibility`
  decodes the v1 wire bytes, checks event IDs/signatures, folds membership,
  imports into the current store, and migrates a v1 database fixture to the
  current schema.
- SQLite has migration tests for current schema evolution.
- The public SDK facade exists but `crates/iroh-rooms` is `publish = false`.
- The online runtime tier is feature-gated as `experimental`.

Production Beta can proceed with this posture if release notes state that the
SDK is source-distributed unless publication changes.

## Compatibility Surfaces

| Surface | Compatibility expectation | Current status |
| --- | --- | --- |
| Event wire format | Stable for `schema_version = 1`; unknown versions rejected | Strong conformance tests exist |
| `WireEvent` envelope | Versioned; receiver recomputes ID and verifies signed bytes | Strong conformance tests exist |
| Event content schemas | Unknown content keys rejected; additions require schema bump | Strong conformance tests exist |
| SQLite store | Migrations must be additive or explicitly incompatible | v1 schema fixture plus migration test implemented |
| CLI text output | Human-readable, not fully stable unless documented | Partial |
| CLI JSON output | Should be treated as machine-facing once documented | Partial |
| SDK stable tier | Should follow semver after publication | Draft |
| SDK experimental tier | May change between releases | Documented by feature gate |
| Network runtime behavior | Best-effort within availability model | Partial |

## Versioning Rules

### Protocol

Protocol-breaking changes require at least one of:

1. Event `schema_version` bump.
2. `WireEvent` envelope version bump.
3. A new ALPN/versioned transport path.

Never silently reinterpret existing `schema_version = 1` fields.

Allowed without schema bump:

- New tests.
- New docs.
- New CLI commands.
- New local indexes or derived caches.
- Stricter rejection of bytes that were already invalid under the documented
  schema.

Not allowed without schema bump:

- Adding required event content fields.
- Accepting previously unknown critical content keys.
- Changing signature payload.
- Changing event ID derivation.
- Changing room ID derivation.
- Changing role authorization semantics for existing event types.

### SQLite

Rules:

1. Migrations must run inside transactions.
2. Authoritative signed event bytes must remain recoverable.
3. Derived caches may be rebuilt from authoritative event bytes.
4. A future schema version must not be opened silently by an older binary.
5. Before migration, release notes must recommend backing up the data directory.

Production Beta requirement:

- Every migration must have a test that opens old data, migrates it, and proves
  existing room events still validate.

### CLI

Human text output can evolve, but release notes should call out changes that
break common scripts.

Machine-facing output should be JSON and should preserve documented fields
within a release class.

Recommended policy:

- Treat `--json` fields documented in guides as stable for Production Beta.
- Add fields instead of renaming or removing them.
- If a field must change, add a release note and compatibility test.

### SDK

Stable tier:

- Offline/deterministic protocol authoring and validation.
- Membership fold.
- Ticket codec.
- No IO, no network, no local store by default.

Experimental tier:

- Online runtime.
- Transport/session/sync/store/blob/pipe runtime.
- May change between production beta candidates.

Production Beta options:

1. Keep SDK source-only with `publish = false` and document that constraint.
2. Publish the stable tier with a semver policy and keep runtime APIs behind
   `experimental`.

Recommendation:

- Do not publish unstable online runtime APIs as stable.
- If publishing, publish only the stable tier contract first.

## Compatibility Fixtures

Production Beta fixtures live under:

```text
crates/iroh-rooms-core/tests/fixtures/v1/
```

Current fixture set:

1. `room.created`
2. `member.invited`
3. `member.joined`
4. `message.text`
5. `file.shared`
6. `pipe.opened`
7. `pipe.closed`
8. `agent.status`
9. A small complete room log.
10. A v1 SQLite schema fixture seeded from the same event bytes.

The first Production Beta cannot include a database created by a previous
Production Beta candidate because none exists yet. The v1 schema fixture is the
bootstrap upgrade fixture for this release class. Starting with the second
Production Beta candidate, the release owner must preserve a database fixture
created by the immediately previous candidate or explicitly disclose why upgrade
continuity is not claimed.

Each fixture should assert:

- Bytes still decode.
- Event IDs are unchanged.
- Signatures still verify.
- Membership fold result is unchanged.
- Store migration preserves validated events.

## Backward Compatibility Gate

For each production candidate:

```bash
scripts/verify.sh
scripts/release-readiness.sh
scripts/production-readiness.sh
```

Additionally, once fixtures exist:

```bash
cargo test -p iroh-rooms-core --features store --test compatibility
```

A separate CLI compatibility suite should be added once machine-facing JSON
surfaces are declared stable. Until then, CLI text remains human-facing and
release notes must call out script-breaking changes.

## Migration Failure Policy

If migration fails:

1. The binary must fail loudly.
2. It must not silently corrupt authoritative signed event bytes.
3. It must not delete the old store.
4. The user must be told to restore from backup or retry with the previous
   binary.

Production Beta release notes must say whether migration is expected for that
release.

## Rollback Policy

Rollback is supported only if:

- No irreversible store migration occurred, or
- The user restores a pre-upgrade backup.

Release notes must say one of:

```text
Rollback supported without data restore.
Rollback requires restoring the pre-upgrade data backup.
Rollback not supported for this candidate.
```

## Compatibility Sign-Off Questions

For every Production Beta candidate:

1. Did event `schema_version` change?
2. Did `WireEvent` version change?
3. Did any event content schema change?
4. Did SQLite `user_version` change?
5. Can the candidate read a data directory from the previous candidate?
6. Can the previous candidate read this candidate's data directory?
7. If not, are backup and rollback instructions explicit?
8. Did CLI JSON output change?
9. Did SDK stable API change?
10. Are experimental API changes called out?

## Current Verdict

The protocol core has strong compatibility foundations through conformance,
golden tests, and the v1 fixture gate. P0.7 is satisfied for a first Production
Beta candidate with two caveats: the SDK remains source-distributed while
`publish = false`, and a real previous-candidate database fixture can only be
added from the second Production Beta candidate onward.
