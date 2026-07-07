# ADR-0002: Invite Revocation And Bounded Ticket Risk For Production Beta

Status: Accepted for Phase 2.5 Production Beta
Date: 2026-07-07
Owners: Release owner, security reviewer

## Context

Iroh Rooms invite tickets are password-grade capabilities. A `roomtkt1...`
ticket carries the room id, invite id, capability secret, bound invitee identity
key, role, optional expiry, inviter identity, and discovery hints. The signed
room log stores only `member.invited.capability_hash`; the raw capability secret
travels out of band in the ticket and later appears on the `member.joined` event
as proof of possession.

The current schema has no native `invite.revoked` event. Adding one would be a
protocol and compatibility change: every peer would need to parse, validate,
fold, sync, display, and test a new admin-authored revocation type. That is the
right direction for GA, but it is not required to make the Phase 2.5 beta claim
truthful if the beta scope explicitly accepts a bounded leaked-ticket model.

## Decision

For Phase 2.5 Production Beta, Iroh Rooms accepts the current bounded-risk
model instead of implementing native invite revocation.

The release claim is:

> Iroh Rooms does not provide native ticket-specific invite revocation in
> Production Beta. A leaked invite ticket remains usable by the bound identity
> until it expires, is made inert by that subject's departure, or is superseded
> by a fresh post-departure invite. The blast radius is bounded by key binding,
> expiry, sticky departure, current-snapshot access gates, and removal-event
> reachability.

This is accepted only for the small-room, CLI-first, technical-user beta scope.
Production GA remains blocked unless native invite revocation is implemented or
the GA release explicitly re-accepts an equally narrow scope.

## Bounded Model

The beta model has these controls:

1. **Key-bound tickets only.** A ticket names exactly one invitee identity key.
   A holder cannot redeem it under a fresh key or use it as an open bearer
   invite.
2. **Capability hash on invite, secret on join.** The invite stores only
   `capability_hash = BLAKE3(INVITE_CONTEXT || room_id || invite_id || secret)`.
   A join must reveal a secret that recomputes the hash.
3. **Log-only expiry.** Expiry is deterministic: a join is valid only when
   `invite.expires_at` is absent or `join.created_at <= invite.expires_at`.
4. **Sticky departure.** A `member.left` or `member.removed` that causally
   follows an invite consumes that invite. A stale pre-departure invite cannot
   re-admit the subject; re-admission requires a fresh admin invite after the
   departure.
5. **Removed dominates.** Concurrent join and removal converge to `Removed`, so
   a raced redemption does not retain capabilities after removal reaches peers.
6. **Current-snapshot access gates.** Blob and pipe access is decided against
   the current local membership snapshot, not the author's old ancestor view.
7. **Pipe revocation-on-learn.** Live pipe sessions are torn down once the owner
   learns a removal, pipe close, or expiry.
8. **User-visible limitation.** Release notes, getting-started docs, and support
   guidance must say there is no native ticket-specific revocation.

## Security Consequences

Accepted for Production Beta:

- A leaked ticket can be redeemed by the bound identity until expiry if that
  identity has not left or been removed.
- The admin cannot revoke only the ticket while preserving the invited
  subject's future ability to join with that same authorization.
- If a ticket leak is suspected, the operational response is to stop sharing
  the ticket, let it expire, avoid issuing long-lived tickets, and remove the
  subject if the leaked ticket may already have been redeemed.
- Removal and access loss are effective once the enforcing peer learns the
  removal. Iroh Rooms does not guarantee immediate revocation against offline,
  partitioned, or withholding peers.

Not accepted:

- Open bearer tickets for production beta rooms.
- Marketing language that implies native invite revocation.
- Treating "do not leak tickets" as the only mitigation.
- Claiming immediate global revocation after removal.

## Required Evidence

The following evidence must stay green before a Production Beta label:

- Wrong-identity join rejection.
- Bad capability secret rejection.
- Expired invite rejection.
- Sticky departure tests for `member.left` and `member.removed`.
- Concurrent join versus kick convergence to `Removed`.
- Non-member and uninvited-agent rejection.
- Blob access denial for removed or non-member peers.
- Pipe access denial for unauthorized peers.
- Pipe teardown-on-learn after removal.
- Secret-free CLI output, error, diagnostic, and audit paths.

Current focused commands:

```bash
cargo test -p iroh-rooms-core --test membership_fold
cargo test -p iroh-rooms-cli --test join_cli
cargo test -p iroh-rooms-net --test join_e2e
cargo test -p iroh-rooms-net --test blob_e2e
cargo test -p iroh-rooms-net --test pipe_e2e p5_revocation_on_learn_tears_down_active_session
```

## Alternatives Considered

### Implement `invite.revoked` before Production Beta

Pros:

- Cleaner incident response for leaked tickets.
- Stronger security story and simpler release language.
- Better alignment with the long-term PRD security roadmap.

Cons:

- Requires a schema/protocol addition and compatibility policy.
- Requires new fold, sync, display, CLI, conformance, and migration tests.
- Increases beta schedule risk while the current bounded model already blocks
  fresh-key ban evasion and removed-member capabilities.

Decision: defer to GA planning unless the beta cohort requires stronger invite
controls sooner.

### Keep current behavior without a decision record

Pros:

- No engineering work.

Cons:

- Creates a silent security overclaim.
- Leaves support without a clear leaked-ticket procedure.
- Fails the P0.4 production-readiness bar.

Decision: rejected.

### Short-lived default expiry only

Pros:

- Reduces leak window without a protocol change.

Cons:

- Does not solve active leaks during the expiry window.
- Cannot help tickets already issued with long or absent expiry.
- Still needs the same public limitation language.

Decision: use expiry as a mitigation, not as the whole decision.

## Implementation Notes

This ADR is a release contract. The production preflight must verify:

- `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md` exists.
- `PRODUCTION-READINESS.md` links this ADR for P0.4 / PR-0004.
- `docs/security/threat-model.md` marks invite revocation as accepted for
  Production Beta under the bounded-risk model.
- `RELEASE-READINESS.md` carries the no-native-revocation limitation with this
  ADR reference.
- `docs/operations/data-handling.md` includes suspected ticket leak handling.

## Review Triggers

Revisit this ADR when any of the following occurs:

- The beta audience expands beyond technical small-room users.
- Users need long-lived invites by default.
- A ticket leak or near-miss occurs during beta.
- Multi-admin, multi-device, or public discovery enters scope.
- The release wants to remove the no-native-revocation caveat.
- A security review rejects the bounded model for the stated audience.

## Final Recommendation

Proceed with Production Beta only under this bounded leaked-ticket model, with
prominent user-facing limitations and operational guidance. Plan native
admin-authored invite revocation before GA unless the GA scope is deliberately
narrow enough to keep this beta posture.
