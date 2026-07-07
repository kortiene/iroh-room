# Iroh Rooms Threat Model

Status: Draft for Phase 2.5 Production Release Candidate.

This document covers the current CLI-first, small-room Iroh Rooms runtime and
the work required before a Production Beta label. It is not an external security
audit and does not claim production readiness by itself. It exists to make the
security posture explicit enough to drive implementation, review, and release
sign-off.

## Summary

Iroh Rooms protects a small private room by combining:

1. Local participant identity keys.
2. Per-device keys that are also iroh `EndpointId`s.
3. Signed, deterministic-CBOR room events.
4. A deterministic membership fold.
5. Connect-time authorization for peers, blobs, and live pipes.
6. Local-first storage without a central application server.

The strongest guarantees today are event authenticity, event integrity,
deterministic validation, key-bound joins, explicit pipe allowlists, verified
blob fetches, and fail-closed access checks for removed members once removal is
known.

The largest production risks are not in the event-signature core. They are:

1. Plaintext local storage.
2. No native invite revocation after ticket issue; accepted for Production
   Beta only under the bounded-risk model in
   `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`.
3. No key rotation or forward secrecy after member removal.
4. Local audit is persistent but not remote, tamper-evident, or automatically
   rotated; accepted for Production Beta in
   `docs/decisions/ADR-0003-persistent-audit-posture.md`.
5. Availability-dependent revocation propagation.
6. No independent security review yet.

Production Beta is acceptable only if these are either fixed or carried as
explicit, user-visible constraints in the release label and release notes.

## Scope

In scope:

- CLI identity, room, invite, join, message, file, pipe, and agent flows.
- `iroh-rooms-core` signed event model, membership fold, store, and sync engine.
- `iroh-rooms-net` transport admission, blob serve/fetch, and pipe runtime.
- `iroh-rooms` SDK stable and experimental facade insofar as it exposes the
  same runtime/security boundaries.
- Local filesystem state under the Iroh Rooms data directory.
- Real-network and relay-assisted peer connectivity as used by iroh.

Out of scope for this threat model:

- Desktop and mobile applications.
- Calls/WebRTC media.
- Large public rooms, public discovery, global usernames, spam prevention, and
  abuse reporting.
- Enterprise compliance controls.
- Full group E2EE ratchet, perfect forward secrecy, secure multi-device
  recovery, anonymous credentials, and metadata privacy beyond what iroh
  transport provides.

## Source References

- `PRD.v0.3.md` section 13: Security and Privacy Model.
- `PRD.v0.3.md` section 14: Availability Model.
- `docs/protocol.md`: implementer security invariants.
- `RELEASE-READINESS.md`: Developer Preview security warnings and known
  limitations.
- `PRODUCTION-READINESS.md`: Phase 2.5 production P0 gates.
- `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`:
  Production Beta invite revocation / bounded leaked-ticket decision.
- `crates/iroh-rooms-core/src/membership/access.rs`: blob and pipe access
  predicates.
- `crates/iroh-rooms-net/src/admission.rs`: transport admission gate.
- `crates/iroh-rooms-net/src/audit.rs`: admission/blob audit surface.
- `crates/iroh-rooms-cli/src/audit.rs`: CLI stderr plus `audit.ndjson` sink.
- `crates/iroh-rooms-cli/src/pipe.rs`: pipe CLI warning and pipe audit sink.
- `docs/decisions/ADR-0003-persistent-audit-posture.md`: Production Beta
  persistent local audit retention/privacy decision.

## Security Objectives

The production-scoped product must provide these guarantees:

1. Only valid room members can author log-valid room events.
2. A peer cannot forge another participant's events.
3. Event bytes cannot be modified without detection.
4. Cross-room replay is rejected.
5. A non-member cannot connect and send room event bytes.
6. Blob fetches are served only to active room members and only for hashes
   referenced by valid `file.shared` events.
7. Blob bytes are independently verified against the claimed BLAKE3 hash before
   save.
8. A live pipe is reachable only by explicitly allowed active members.
9. Removed members lose blob and pipe capability when the removal is learned.
10. Secrets are not printed in normal CLI output, error messages, diagnostics,
    or release artifacts.
11. Users are told plainly where data is local, when peers must be online, and
    which security properties are not provided.

## Assets

| Asset | Sensitivity | Notes |
| --- | --- | --- |
| Identity secret key | Critical | Stable participant principal; compromise impersonates the user. |
| Device secret key / iroh endpoint secret | Critical | Signs events and authenticates transport endpoint. |
| Invite ticket capability secret | High | Grants ability to produce a matching `member.joined` for the bound identity until expiry/consumption rules apply. |
| `rooms.db` | High | Plaintext room event log, membership, messages, file references, pipe events, and derived sync state. |
| Blob store | High | Shared files/artifacts; currently plaintext at rest. |
| Pipe target | High | Local loopback service exposed to a specific peer. |
| Audit/diagnostic output | Medium | Security-relevant, but can also reveal peer IDs, pipe IDs, and operational metadata. |
| Room metadata | Medium | Room ID, membership graph, device IDs, event timing, relay/direct diagnostics. |
| Release artifacts | Medium | If tampered, users may run malicious binaries. |

## Trust Boundaries

| Boundary | Trusted side | Untrusted side | Enforcement |
| --- | --- | --- | --- |
| CLI process to local filesystem | Local user account | Other local users/processes | File permissions today; storage encryption not implemented. |
| Event parser | Validated canonical bytes | Arbitrary peer bytes | Strict CBOR, event ID recompute, Ed25519 verify, content validation. |
| Transport admission | Active member device IDs | Unknown or inactive endpoints | QUIC/TLS remote `EndpointId` admission before event bytes. |
| Membership fold | Validated causal event set | Out-of-order, duplicate, or malicious events | Deterministic fold, ancestor-view authorization, buffering/backfill. |
| Blob serve | Active member requesting referenced hash | Non-member, removed member, unreferenced hash | Two-gate blob ACL plus hash verification. |
| Pipe connect | Explicitly allowed active member | Non-member, inactive member, non-allowlisted member | Current snapshot access predicate, no default-all. |
| Join bootstrap | Admin-hosted open invite window | Unknown first-time invitee | Provisional admission limited to membership bootstrap. |
| Relay/network | Encrypted transport | Relays and network observers | iroh transport encryption; metadata privacy not guaranteed. |

## Adversaries

| Adversary | Capability |
| --- | --- |
| Passive network observer | Observes timing, addresses, relay/direct behavior, and metadata visible outside encrypted payloads. |
| Malicious non-member | Dials endpoints, sends malformed frames, tries to join without valid ticket, requests blobs or pipes. |
| Malicious active member | Sends malformed or abusive validly signed events, withholds events, exposes risky pipes, shares misleading file metadata. |
| Removed member | Keeps old log data, may keep authoring on a stale fork, may try to connect before learning/removal propagates. |
| Stolen-ticket holder | Possesses an invite ticket secret and attempts to join or leak it. |
| Compromised device | Holds valid device secret for a member or agent. |
| Compromised local account | Reads or modifies plaintext local files under the data directory. |
| Malicious blob provider | Serves wrong bytes, refuses service, or serves only to selected peers. |
| Malicious pipe owner | Exposes a dangerous local service to an allowed peer or misrepresents target risk. |
| Supply-chain attacker | Attempts to tamper with dependencies, build artifacts, install instructions, or release checksums. |

## Existing Controls

### Protocol And Event Integrity

- Deterministic CBOR canonicality is enforced independently of signatures.
- `event_id` is recomputed from exact signed bytes.
- Signatures verify under `device_id`, not `sender_id`.
- Device bindings connect participant identity to device key.
- `room_id` is signed and cross-room replay is rejected.
- Unknown schema versions, event types, content keys, bad signatures, and
  malformed data are rejected before storage.
- Duplicate events are idempotent.
- Conformance tests cover the protocol rejection taxonomy.

### Membership And Authorization

- Room creator is the single immutable admin in current scope.
- Invites are key-bound to a named `invitee_key`.
- Joins must prove the invite capability secret.
- Invite expiry is deterministic and log-only: peers compare signed
  `join.created_at` to signed `invite.expires_at`.
- Departure is sticky; stale pre-departure invites cannot re-admit a member.
- Log validity uses ancestor-view authorization for deterministic convergence.
- Access capabilities use the current membership snapshot, so a since-removed
  member's old valid events do not grant blob or pipe access.

### Transport Admission

- QUIC/TLS-authenticated `EndpointId` is checked before accepting event bytes.
- Unknown devices, inactive identities, and fail-closed subjects are rejected.
- Join bootstrap provisional admission is limited and does not itself grant
  membership.

### Blob Plane

- Fetch authorization requires active room membership.
- The requested hash must be referenced by a valid `file.shared` event.
- Fetched bytes are independently BLAKE3-verified before save.
- Hash mismatch is a hard integrity failure.
- Output filenames are sanitized before save.

### Live Pipe Plane

- `pipe expose` requires explicit `--allow`.
- There is no default all-member exposure.
- Non-loopback targets are refused.
- `pipe.opened` records allowed members and owner.
- `pipe.closed` can close a pipe; clean exit emits `owner_exit`.
- Unauthorized pipe attempts are rejected and audited to stderr plus
  `<IROH_ROOMS_HOME>/audit.ndjson`.
- Revocation-on-learn tears down active sessions once the enforcing node learns
  of removal or pipe closure.

### CLI Secret Hygiene

- `identity show` never reads or prints secret key material.
- Invite-ticket failures redact ticket/capability secret details.
- Error taxonomy tests check secret-free output paths.
- Pipe and ticket warnings exist in Developer Preview release checks.

## Threat Matrix

Severity uses:

- Critical: can compromise identity, unauthorized room access, arbitrary local
  service exposure, or silent data corruption.
- High: can leak sensitive room data, bypass intended access, or cause durable
  trust loss.
- Medium: meaningful privacy, availability, or auditability loss.
- Low: bounded usability or diagnostic issue.

Status uses:

- Controlled: sufficient for scoped production.
- Partial: meaningful controls exist, but Production Beta needs a decision or
  improvement.
- Open: production blocker unless explicitly accepted with a narrow release
  label.

| ID | Threat | Severity | Current controls | Gap / production decision | Status |
| --- | --- | --- | --- | --- | --- |
| T01 | Forged event under another identity | Critical | Device signatures, binding, membership validation, conformance tests | Keep tests mandatory; no additional blocker | Controlled |
| T02 | Modified event bytes accepted | Critical | Event ID recompute, signature verify, canonical CBOR, strict content validation | Keep protocol fixtures stable across versions | Controlled |
| T03 | Cross-room replay | High | Signed `room_id`, room binding validation | Keep as compatibility fixture | Controlled |
| T04 | Unknown peer sends room events | High | Transport admission before event bytes | Keep admission tests in release gate | Controlled |
| T05 | Stolen invite ticket joins room | High | Key-bound invite, expiry, wrong-identity rejection | Accepted for Production Beta via ADR-0002 bounded-risk model; GA should implement native revocation or re-accept a narrow scope | Partial |
| T06 | Expired or stale invite reused | High | Expiry, sticky departure, join capability check | Keep user docs and release notes explicit: no native revocation; ask admin for a fresh invite | Controlled |
| T07 | Removed member keeps blob/pipe capability | Critical | Current-snapshot access gates, owner inactive checks, revocation-on-learn | Exposure is bounded by removal-event reachability; production notes must say this; consider always-on witness later | Partial |
| T08 | Removed member pollutes timeline after removal | Medium | Advisory `from_removed_member`; capabilities are zero | UI/listing hard segregation not complete; acceptable for beta only if disclosed | Partial |
| T09 | Repeat malicious identity has no basic blocklist | Medium | Admin can withhold invites or remove members | PRD names basic blocklist; no explicit blocklist surface found. Decide if member removal satisfies scoped beta or implement blocklist | Open |
| T10 | Compromised local account reads room data | Critical | Unix owner-only identity files; local-only storage; ADR-0001 scopes beta to trusted local machines | `rooms.db`, blobs, and `audit.ndjson` are plaintext. GA needs encryption or a narrower deployment claim | Partial |
| T11 | Local secret key leakage through CLI output | Critical | Secret-free output tests, redaction, `identity show` avoids secret file | Keep regression tests; extend bug template to request redacted logs only | Controlled |
| T12 | Malicious blob provider serves wrong file | High | Independent BLAKE3 verify; hash mismatch hard stop | Keep tests in release gate | Controlled |
| T13 | Active member fetches unreferenced blob | High | Per-hash ACL requires valid `file.shared` reference | Keep two-gate ACL tests | Controlled |
| T14 | Unauthorized peer connects to live pipe | Critical | Explicit allowlist, active-member check, owner active, no default-all, loopback target | Keep live pipe online tier release-blocking | Controlled |
| T15 | Pipe remains open after owner crash/SIGKILL | High | Clean exit emits `pipe.closed`; owner/admin can close later | SIGKILL leaves log open until explicit close. Require stale pipe cleanup policy or disclose beta limitation | Partial |
| T16 | Pipe owner exposes sensitive local service to allowed peer | High | Warning names target and allowed members; loopback-only target | Add stronger user guidance; production cannot prevent user-authorized exposure | Partial |
| T17 | Malicious peer sends malformed CBOR to crash node | High | Strict parser, malformed live tests, no-panic property tests | Keep fuzz/property coverage | Controlled |
| T18 | Withheld removal delays revocation | Critical | Admin-tip incompleteness detector and fail-closed where known | No guaranteed sibling completeness without availability assumption. Production claim must remain small-room/online-peer scoped | Partial |
| T19 | Relay/network metadata exposure | Medium | Encrypted transport payloads | Full metadata privacy is out of scope; disclose in production limitations | Partial |
| T20 | Local audit trail is lost, leaked, or over-trusted | High | CLI-local `audit.ndjson` plus stderr audit vocabulary for peer, blob, join-bootstrap, and pipe callbacks | Retention, redaction, and tamper-evidence limits must stay documented | Controlled |
| T21 | Release artifact tampering | Critical | No production release artifact process yet | Add checksums/signing and release ops doc before Production Beta | Open |
| T22 | Breaking schema or SDK changes strand users | High | Protocol docs, conformance tests, v1 compatibility fixtures, and v1 SQLite migration fixture | Preserve previous-candidate DB fixture once a Production Beta exists | Controlled for first Beta |
| T23 | Agent over-trust or implicit access | High | Agent has own identity, explicit invite, least-privileged role, normal file/pipe gates | Add agent-specific guidance for external integrators | Partial |
| T24 | Diagnostics leak secrets | High | Diagnostics are secret-free by design; tests cover seed leakage | Extend bug template and audit guidance | Partial |
| T25 | Availability misunderstood as guaranteed delivery | Medium | Docs and release notes state no cloud inbox/no guaranteed offline delivery | Beta must measure whether users understand this model | Partial |

## Production-Blocking Decisions

These decisions must be made before Production Beta.

### D01 Storage Encryption

Decision status: accepted for Production Beta in
`docs/decisions/ADR-0001-local-storage-posture.md`.

Production Beta is scoped to trusted local machines. `rooms.db`, blobs, and
`audit.ndjson` remain plaintext, and release notes must make that limitation
visible before install/run instructions. Production GA remains blocked unless
local storage encryption is implemented or the GA deployment model is narrowed
enough that plaintext local storage is still a deliberate, user-visible
constraint.

### D02 Invite Revocation

Decision status: accepted for Production Beta in
`docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`.

Production Beta uses the bounded leaked-ticket model: tickets are key-bound,
expiry-checked, and made inert by sticky departure, but there is no native
ticket-specific revocation event. Release notes must state that a leaked ticket
remains dangerous for the bound identity until expiry or departure consumption
rules apply. Production GA should implement a minimal admin-authored
`invite.revoked` or equivalent unless the GA scope explicitly re-accepts this
narrow beta posture.

### D03 Persistent Audit

Decision status: accepted for Production Beta in
`docs/decisions/ADR-0003-persistent-audit-posture.md`.

The CLI appends local audit records to `<IROH_ROOMS_HOME>/audit.ndjson`. The
audit file is local, append-oriented, and useful for post-run incident
reconstruction, but it is not remote, tamper-evident, centrally retained,
automatically rotated, or safe to paste unredacted into public issues.

### D04 Removal Reachability

Decision status: accepted for Production Beta in ADR-0002.

Production claims must say revocation is effective once the enforcing peer
learns the removal. Stronger guarantees need an availability layer or witness
node and remain out of scope for the first beta.

### D05 Blocklist

Decision needed: decide whether per-room member removal satisfies the MVP
"basic blocklist" requirement for scoped beta, or implement an explicit local
blocklist.

Recommendation: for Production Beta, use member removal plus no implicit joins
as the scoped blocklist only if release notes state there is no global identity
blocklist or abuse-reporting system.

### D06 SDK Publication And Stability

Decision needed: keep the SDK unpublished and scoped to source users, or publish
the stable tier with a semver policy.

Recommendation: Production Beta can keep `publish = false` if install docs use
source builds only. Production GA should either publish the stable tier or make
the source-build constraint explicit.

## Accepted Limitations For Production Beta

These can be accepted for a narrow Production Beta if release notes and onboarding
state them plainly:

- No cloud inbox and no guaranteed offline delivery.
- Files and pipes require an online provider/owner.
- No full group E2EE ratchet or perfect forward secrecy.
- No secure multi-device recovery.
- No full metadata privacy.
- Small rooms only.
- Single immutable admin.
- TCP-only pipes.
- No public room discovery.
- No enterprise compliance or abuse-reporting system.

These are not acceptable to hide behind general "beta" language. Users must see
them before relying on the system.

## Required Security Tests For Production Beta

The following tests already exist or should be added before sign-off.

Existing release-gated coverage:

- Protocol conformance and taxonomy coverage.
- Strict CBOR property/no-panic tests.
- Malformed CBOR over live connection.
- Wrong identity and corrupt ticket rejection.
- Expired invite rejection.
- Non-member event rejection.
- Blob hash mismatch hard stop.
- Blob active-member plus referenced-hash ACL.
- Unauthorized pipe rejection.
- Pipe revocation-on-learn and owner close behavior.
- Agent explicit invite and least-privileged role.
- Persistent audit sink tests for peer/blob and pipe callbacks.
- Secret-free CLI output paths.

Missing or not yet production-sufficient:

- Native invite revocation tests, if implemented for GA.
- Backup/restore tests that prove no secret material is accidentally exported.
- Previous-production-candidate database fixture for the second Beta candidate
  onward.
- Release artifact checksum/signature verification.
- Beta bug-report redaction guidance.

## Incident Response Expectations

Production Beta needs at minimum:

1. A privacy-preserving bug report template.
2. Instructions for collecting CLI output without secrets.
3. A policy for suspected ticket leak, anchored in ADR-0002 and
   `docs/operations/data-handling.md`.
4. A policy for suspected identity/device key compromise.
5. A policy for suspected malicious blob provider.
6. A policy for accidentally exposed pipe target.
7. A rollback and local data backup procedure.

Until these exist, a production release can fail operationally even if the core
protocol remains correct.

## Release Sign-Off Questions

The release owner must answer these for every Production Beta candidate:

1. Did `scripts/release-readiness.sh` exit `0`?
2. Did `scripts/production-readiness.sh` exit `0`?
3. Was the threat model reviewed by someone other than the implementer?
4. Are plaintext storage risks either fixed or explicitly accepted?
5. Are invite leakage and no-revocation risks accepted under ADR-0002, or fixed
   by native revocation?
6. Is the persistent local audit posture accepted for beta?
7. Are compatibility and migration rules documented?
8. Are release artifacts checksummed or signed?
9. Are known limitations present in release notes before install/run steps?
10. Is there a support path for security and data-loss reports?

## Current Verdict

Current state: not production-ready.

The protocol, membership, blob, and pipe controls are strong enough to justify
continued Production Release Candidate work. The project should not use a
Production Beta label until the open threats above have either shipped fixes or
explicit release-owner waivers backed by user-visible limitations.

The next security work should be:

1. Decide whether per-room removal satisfies the scoped beta blocklist
   requirement, or add an explicit blocklist.
2. Preserve previous-candidate compatibility evidence once a Production Beta
   candidate exists.
3. Add release artifact checksum/signature evidence.
4. Keep ADR-0002 visible in release notes and revisit native invite revocation
   before GA.
5. Keep ADR-0003 visible in release notes and revisit retention/rotation/tamper
   evidence before GA.
