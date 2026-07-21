# Spec: Approach-To-Ceiling Warning

| | |
|---|---|
| **Issue** | #144 — `[CORE] Instrument approach-to-ceiling (warn above N active devices)` |
| **Labels** | `type/feature` `area/protocol` `priority/p2` `risk/low` |
| **Dependencies** | None |
| **Owning crates** | `crates/iroh-rooms-core` for capacity constants/helpers and unchanged hard rejection; `crates/iroh-rooms-net` for live snapshot crossing detection and audit/log emission; `crates/iroh-rooms-cli` for `room members --status` output and persistent local audit rendering. |
| **Status** | Planning only. Do not implement in the same change that creates this spec. |

---

## 1. Summary

Add operator-visible instrumentation for rooms approaching the fixed active-member ceiling:

1. Keep `MAX_ACTIVE_MEMBERS = 5` as a protocol invariant and keep `RejectReason::RoomFull` at the 6th active join.
2. Define a soft warning threshold derived from the cap, recommended as `MAX_ACTIVE_MEMBERS - 1` (currently `4`).
3. Emit a one-shot warning when the locally observed active-member count crosses from below the threshold to at/above it.
4. Add a capacity/headroom line to `iroh-rooms room members <ROOM_ID> --status`, for example:

```text
active: 4/5 (1 slot remaining)
```

This is observability only. It must not add new room events, change membership authorization, make the cap configurable, or raise the ceiling.

---

## 2. Repository Context Read

Relevant current state:

- `README.md` describes Iroh Rooms as a small trusted-group runtime and warns that rooms are intended for `≤5` members. Its current limitation text still says code does not enforce or warn on room size, which is stale relative to the hard cap now present.
- `docs/protocol.md` is the implementer reference for signed events, deterministic validation, membership fold, and rejection reason codes. Capacity observability must not change the wire format or signed event semantics.
- `docs/decisions/ADR-0003-persistent-audit-posture.md` accepts a local best-effort `audit.ndjson` posture for lifecycle/security callbacks and forbids logging secrets, tickets, message bodies, blob bytes, and local paths.
- `docs/security/threat-model.md` treats audit and diagnostic output as operational metadata that can reveal room relationships and must stay secret-free.
- `crates/iroh-rooms-core/src/membership/model.rs:14` defines `pub const MAX_ACTIVE_MEMBERS: usize = 5` and `MembershipSnapshot::active_member_count()`.
- `crates/iroh-rooms-core/src/membership/fold.rs:491-492` rejects a joining subject with `RejectReason::RoomFull` when the ancestor view already has `MAX_ACTIVE_MEMBERS` active members.
- `crates/iroh-rooms-core/src/membership/fold.rs:681` and `:784-806` also cap folded active members deterministically when concurrent joins would otherwise overfill the room.
- `crates/iroh-rooms-cli/src/message.rs:570-645` implements `room members --status` by folding the local store, bringing up a short-lived `Node`, then printing membership plus connection state.
- `crates/iroh-rooms-cli/src/message.rs:651-680` prints the current `--status` panel; it has no capacity/headroom line today.
- `crates/iroh-rooms-net/src/node.rs:121-157` has `RoomReconciler::maybe_reconcile`, which sees each live `SyncEngine` snapshot and already debounces membership changes before refreshing admission and peer dialing.
- `crates/iroh-rooms-net/src/audit.rs` defines `AuditSink` and the default `TracingAudit`; `crates/iroh-rooms-cli/src/audit.rs` adapts this to stderr plus persistent `audit.ndjson`.
- `scripts/verify.sh` is the full quality gate: format, clippy with `-D warnings`, workspace tests, SDK doctests, and examples.

---

## 3. Goals and Non-Goals

### Goals

- Warn operators before the 6th join fails due to the fixed room ceiling.
- Make the current active count and remaining slots visible in `room members --status`.
- Preserve script-friendly, secret-free CLI/audit output.
- Keep the hard cap behavior and protocol rejection taxonomy unchanged.
- Keep the design deterministic and low overhead.

### Non-goals

- Do not make `MAX_ACTIVE_MEMBERS` configurable.
- Do not raise the cap above 5.
- Do not add a signed room event for the warning.
- Do not change invite/join authorization, role resolution, or room membership semantics.
- Do not add database schema migrations.
- Do not emit warnings on every reconcile tick while the room remains above the threshold.

---

## 4. Design Decisions

### D1 — Derive the warning threshold from the hard cap

Add a public constant or pure helper in `iroh-rooms-core::membership`:

```rust
pub const ACTIVE_MEMBER_WARNING_THRESHOLD: usize = MAX_ACTIVE_MEMBERS - 1;
```

Use the derived value everywhere rather than hard-coding `4`. This keeps the implementation aligned with the protocol invariant without making it configurable.

Also add a small pure helper for headroom, either as a free function or `MembershipSnapshot` method:

```rust
headroom = MAX_ACTIVE_MEMBERS.saturating_sub(snapshot.active_member_count())
```

Recommended surface:

- `MembershipSnapshot::active_member_headroom() -> usize`
- `MembershipSnapshot::active_member_limit() -> usize` or direct use of `MAX_ACTIVE_MEMBERS`
- optional pure `active_member_warning_crossed(previous: Option<usize>, current: usize) -> bool`

### D2 — Keep the membership fold pure

Do not log inside `RoomMembership::snapshot()` or core fold methods. The fold is a pure, deterministic projection and is called in tests, CLI reads, sync, and admission. Side effects there would create spam and make tests/order harder to reason about.

Core should only expose constants/helpers and continue producing the same `RejectReason::RoomFull` at capacity.

### D3 — Emit warning from live snapshot observers

Use `iroh-rooms-net::RoomReconciler` as the primary live warning point because it already observes the current `SyncEngine` membership snapshot after local publish/inbound sync and already debounces membership-driven work.

Extend `RoomReconciler` with previous-count state:

```text
last_active_member_count: Option<usize>
```

On each `maybe_reconcile` call:

1. Read `snapshot.active_member_count()`.
2. If `last_active_member_count` is `Some(prev)` and `prev < ACTIVE_MEMBER_WARNING_THRESHOLD && current >= ACTIVE_MEMBER_WARNING_THRESHOLD`, emit the audit/log hook once.
3. Store `Some(current)`.
4. Continue the existing admission/blob/peer reconcile logic unchanged.

This gives one warning per below-to-at/above threshold crossing per running node, not per tick. If a room shrinks below threshold and later grows back to threshold, emit a new warning because that is a new crossing.

Open behavior to choose during implementation: whether the first observed snapshot should warn if it is already at/above threshold. The recommended default is **no initial warning** in `RoomReconciler` because no crossing was observed in this process; `room members --status` will still show the count explicitly. If product wants startup visibility, implement it intentionally and test it as "one warning per process startup above threshold", not accidentally.

### D4 — Extend audit/log vocabulary additively

Add a default no-op method to `iroh_rooms_net::AuditSink`, preserving existing implementors:

```rust
fn active_member_threshold_reached(
    &self,
    room_id: &RoomId,
    active: usize,
    max: usize,
    remaining: usize,
) {}
```

Recommended stable event name:

```text
room.active_members.near_cap
```

Recommended stderr warning code:

```text
warning[room_near_capacity]: room has 4/5 active members (1 slot remaining)
```

Recommended persistent audit fields:

```json
{
  "event": "room.active_members.near_cap",
  "room": "blake3:...",
  "active": 4,
  "max": 5,
  "remaining": 1,
  "threshold": 4
}
```

Do not include invite tickets, capability secrets, message bodies, blob bytes, local file paths, identity secret seeds, or device secret seeds.

Implementations:

- `iroh-rooms-net::TracingAudit`: `tracing::warn!` with `reason = "room.active_members.near_cap"`, `room`, `active`, `max`, and `remaining`.
- CLI `LocalAudit`: persist the NDJSON record and delegate to stderr.
- CLI `StderrAudit`: render one stable warning line with `warning[room_near_capacity]`.

### D5 — Add capacity/headroom to `room members --status`

Update `crates/iroh-rooms-cli/src/message.rs:print_members_status` to print an additive line after `admin:` and before `member:` rows:

```text
active: <active>/<max> (<remaining> slot remaining)
```

Pluralize `slot`/`slots` for readability, but tests should allow either the exact string at `1` or equivalent acceptance wording.

Example full panel shape:

```text
room: blake3:<room-id>
admin: <identity-id>
active: 4/5 (1 slot remaining)
member: <id> role=admin status=active conn=self (admin)
member: <id> role=member status=active conn=offline:unreachable
peers: 0 connected, 3 offline, 0 unauthorized
```

Keep `--verbose` diagnostics on stderr exactly as today. The new `active:` line belongs on stdout with the normal status panel.

### D6 — Documentation updates are part of implementation

Update README/operator docs that mention the small-room limitation. In particular, the README currently says "Nothing in code enforces or warns on room size"; replace that with current behavior: hard reject at 6 active members plus warning/status headroom near the ceiling.

No protocol doc change is required unless the team wants to mention the warning as non-normative operational observability. Do not imply the warning is part of the wire protocol.

---

## 5. Implementation Plan

### Step 1 — Core constants and helpers

Files:

- `crates/iroh-rooms-core/src/membership/model.rs`
- `crates/iroh-rooms-core/src/membership/mod.rs`
- optionally `crates/iroh-rooms/src/room.rs` facade re-export

Tasks:

1. Add `ACTIVE_MEMBER_WARNING_THRESHOLD` derived from `MAX_ACTIVE_MEMBERS`.
2. Add a headroom helper on `MembershipSnapshot` or a pure free function.
3. Add a pure crossing helper if it simplifies net tests:
   - `previous = Some(3), current = 4` returns true.
   - `previous = Some(4), current = 4` returns false.
   - `previous = Some(4), current = 5` returns false.
   - `previous = Some(3), current = 5` returns true.
   - `previous = Some(5), current = 3` returns false.
4. Re-export only if needed by CLI/net through existing public facade paths.

Do not change `MAX_ACTIVE_MEMBERS` or `RejectReason::RoomFull`.

### Step 2 — Add audit hook

Files:

- `crates/iroh-rooms-net/src/audit.rs`
- `crates/iroh-rooms-cli/src/audit.rs`

Tasks:

1. Import `RoomId` in net audit code.
2. Add `AuditSink::active_member_threshold_reached(...)` as a default no-op method.
3. Implement the method in `TracingAudit` with WARN level and stable reason `room.active_members.near_cap`.
4. Implement the method in CLI `LocalAudit`:
   - persist `room.active_members.near_cap` with `room`, `active`, `max`, `remaining`, `threshold`;
   - call `StderrAudit` for immediate operator feedback.
5. Implement the method in `StderrAudit` with `warning[room_near_capacity]` and the count/headroom text.
6. Add/extend CLI audit tests to assert the NDJSON event name and fields are present and no secret-like fields are emitted.

### Step 3 — Detect threshold crossings in the live room reconciler

File:

- `crates/iroh-rooms-net/src/node.rs`

Tasks:

1. Add fields to `RoomReconciler`:
   - `audit: Arc<dyn AuditSink>`
   - `last_active_member_count: Option<usize>`
2. Thread the existing node audit sink into `RoomReconciler` construction.
3. In `maybe_reconcile`, after `let snapshot = engine.snapshot()`, compute:
   - `active = snapshot.active_member_count()`
   - `max = MAX_ACTIVE_MEMBERS`
   - `remaining = max.saturating_sub(active)`
4. If the active count crosses the threshold from below to at/above, call `audit.active_member_threshold_reached(snapshot.room_id(), active, max, remaining)`.
5. Update `last_active_member_count` on every call after reading the snapshot.
6. Ensure the warning logic is independent of `membership_changed` early return so it tracks count transitions correctly, but still uses its own previous-count state to prevent tick spam.
7. Keep admission refresh, blob ACL refresh, and peer manager reconciliation behavior unchanged.

### Step 4 — Add status output line

File:

- `crates/iroh-rooms-cli/src/message.rs`

Tasks:

1. Add a small formatting helper, e.g. `active_capacity_line(active, max) -> String`, so unit tests do not need to capture stdout.
2. In `print_members_status`, after the `admin:` line, print the formatted capacity line using `snapshot.active_member_count()` and `MAX_ACTIVE_MEMBERS` or the new snapshot helper.
3. Keep the rest of the member rows and `peers:` summary unchanged.

### Step 5 — Documentation updates

Files likely affected:

- `README.md`
- optionally `docs/getting-started.md` if it shows `room members --status` sample output
- optionally release/readiness docs if they mention no cap enforcement

Tasks:

1. Replace stale "nothing enforces or warns" text with hard-reject + warning/status behavior.
2. Document that the cap remains fixed at 5 and is not configurable.
3. Keep docs clear that audit/status output is local operational metadata.

---

## 6. Test Strategy

### Core tests

In `crates/iroh-rooms-core/src/membership/model.rs` or `fold.rs` tests:

- Threshold helper tests for below-to-threshold crossing and no repeated warning conditions.
- Headroom helper tests for active counts `0`, `4`, `5`, and defensive saturating behavior above `5`.
- Hard cap regression: build a room where admin plus four invitees are active, then attempt a fifth invitee join; assert `Ingest::Rejected { reason: RejectReason::RoomFull, .. }` for the 6th active member.
- Existing `RoomFull` behavior at `fold.rs:491-492` must remain unchanged.

### Net/audit tests

In `crates/iroh-rooms-net/src/node.rs` tests or a small helper module:

- A warning state observed counts `1 -> 2 -> 3 -> 4 -> 4 -> 5` emits exactly one audit hook.
- Counts `4 -> 4` on repeated ticks do not emit additional hooks.
- Counts `4 -> 3 -> 4` emit a second hook only on the second below-to-threshold crossing, if D3's per-crossing behavior is chosen.

In `crates/iroh-rooms-cli/src/audit.rs` tests:

- `LocalAudit` persists `room.active_members.near_cap` as valid NDJSON.
- The record contains `room`, `active`, `max`, `remaining`, and `threshold`.
- The record does not contain `ticket`, `capability_secret`, `identity.secret`, device seeds, message bodies, blob bytes, or local paths.

### CLI output tests

In `crates/iroh-rooms-cli/src/message.rs` unit tests or CLI integration tests:

- Formatting helper returns `active: 4/5 (1 slot remaining)` for `active=4, max=5`.
- Formatting helper returns `active: 5/5 (0 slots remaining)` for `active=5, max=5`.
- A `room members --status` run includes an `active:` line on stdout and keeps `diag:` lines stderr-only under `--verbose`.

Prefer a unit test for the formatter plus one existing/ignored live status integration test update, because constructing a 4-active-member live room through the CLI may be expensive. If a deterministic temp-store fixture can run `--status --loopback --timeout 1ms` without multiple live peers, add a non-ignored integration test that asserts the exact 4/5 line.

### Verification commands

Run the smallest focused checks during implementation, then the full gate:

```bash
cargo test -p iroh-rooms-core membership::
cargo test -p iroh-rooms-cli audit::
cargo test -p iroh-rooms-cli room_cli -- --nocapture
scripts/verify.sh
```

Adjust focused commands to actual test names once implemented.

---

## 7. Acceptance Criteria Mapping

| Acceptance item | Implementation evidence |
| --- | --- |
| A 4-active-member room emits a one-shot warning, not per-tick spam | `RoomReconciler` or extracted warning-state test shows one `room.active_members.near_cap` hook for `3 -> 4` and no repeats for unchanged `4` across ticks. CLI audit test proves the hook persists/renders through `LocalAudit`. |
| `room members --status` shows active count/headroom | CLI output/formatter test asserts `active: 4/5 (1 slot remaining)` or equivalent on stdout. |
| Hard reject at 6 still produces `RejectReason::RoomFull` | Core membership fold regression test asserts the 6th active join is rejected with `RejectReason::RoomFull`; no change to `fold.rs:491-492` semantics. |

---

## 8. Security, Privacy, Reliability, and Performance

### Security and privacy

- No new secrets are created or logged.
- Audit records contain room ID and public operational counts; this is metadata and should be treated like existing `audit.ndjson` records.
- Do not log invite tickets, capability secrets, identity/device secret seeds, message bodies, blob bytes, or local filesystem paths.
- No authorization is relaxed. The warning must be emitted after folding the same validated events that already drive admission.

### Reliability

- Warning state is in-process and best-effort, consistent with existing local audit posture.
- Persistent `audit.ndjson` write failures should follow existing `PersistentAudit` behavior: warn once about audit write failure and do not fail the room operation.
- Repeated reconcile ticks must not spam warnings.
- The hard cap remains the reliability guardrail for full-mesh small-room scaling.

### Performance

- Counting active members is already available from `MembershipSnapshot` and bounded by the small-room invariant.
- The added check is O(number of known members) if using `active_member_count()` directly and occurs on existing reconcile/tick paths. With `MAX_ACTIVE_MEMBERS = 5`, this is negligible.
- No database schema or backfill work is needed.

---

## 9. Rollout and Rollback

### Rollout

- Additive trait method has a default no-op, minimizing downstream breakage.
- CLI output gains one new `active:` line for `room members --status`; document this as an intentional operator-visible addition.
- Audit event vocabulary is additive.
- No migration required.

### Rollback

- If warning emission is noisy, disable only the `RoomReconciler` crossing hook while keeping `room members --status` headroom output.
- If the status line breaks consumers, it can be gated behind a documented output-version decision, but this is not recommended because the issue acceptance requires the count/headroom surface.
- Reverting the feature must not touch `MAX_ACTIVE_MEMBERS` or `RejectReason::RoomFull`.

---

## 10. Assumptions

- The soft threshold should be `N - 1`, so today `4` from cap `5`.
- "One-shot" means one warning per observed below-to-threshold crossing per running node/process, not one warning per reconcile tick and not necessarily once forever across restarts.
- `room members --status` is the required status surface; offline `room members` and `room members --json` can remain unchanged unless maintainers want broader capacity visibility.
- The warning belongs to local operational audit/log output, not the signed room event log.
- The 6th active member means admin plus five other active members would exceed the cap; the admin counts as one active member.

---

## 11. Open Questions

1. Should a node that starts while the room is already at `4/5` emit an immediate near-cap warning, or only after observing a live crossing from below? Recommendation: only crossing in the live watcher; rely on `room members --status` for current-state visibility.
2. Should offline `room members <ROOM_ID>` also show the `active:` line for consistency, or is the requested `--status` surface sufficient? Recommendation: keep scope to `--status` for this issue.
3. Should the persistent audit record include `room`? Recommendation: yes, because room ID is operational metadata already present in CLI output and needed for incident reconstruction, but confirm against privacy expectations.
4. Should the warning code be `room_near_capacity`, `active_members_near_cap`, or another existing taxonomy style? Recommendation: `room_near_capacity` for stderr and `room.active_members.near_cap` for audit/tracing.
5. If concurrent joins cause a snapshot to jump from `3` to `5`, should it warn once with `active=5, remaining=0`? Recommendation: yes, because the threshold was crossed.

---

## 12. Key Risks

| Risk | Mitigation |
| --- | --- |
| Warning spam on periodic reconcile ticks | Keep explicit previous-count state and test repeated above-threshold ticks. |
| Side effects leak into pure membership fold | Keep logging out of `iroh-rooms-core`; core only exports constants/helpers. |
| Trait extension breaks custom audit sinks | Add the new `AuditSink` method with a default no-op implementation. |
| Audit output leaks sensitive material | Log only room ID and numeric counts; add secret-hygiene tests. |
| Status output change surprises scripts | Add a stable `active:` line rather than modifying existing `member:` or `peers:` rows. Document the addition. |
| Hard cap behavior regresses while adding soft warning | Add a dedicated `RejectReason::RoomFull` regression test for the 6th active join. |

---

## 13. References

- Issue #144: `[CORE] Instrument approach-to-ceiling (warn above N active devices)`
- `crates/iroh-rooms-core/src/membership/model.rs:14` — `MAX_ACTIVE_MEMBERS = 5`
- `crates/iroh-rooms-core/src/membership/model.rs:164-165` — `active_member_count()`
- `crates/iroh-rooms-core/src/membership/fold.rs:491-492` — hard `RejectReason::RoomFull`
- `crates/iroh-rooms-core/src/membership/fold.rs:784-806` — deterministic active cap in folded snapshot
- `crates/iroh-rooms-cli/src/message.rs:560-680` — `room members --status` path and panel rendering
- `crates/iroh-rooms-net/src/node.rs:121-157` — live room reconcile hook point
- `crates/iroh-rooms-net/src/audit.rs` — audit trait and tracing sink
- `crates/iroh-rooms-cli/src/audit.rs` — stderr plus `audit.ndjson` local sink
- `docs/decisions/ADR-0003-persistent-audit-posture.md` — local audit policy
- `scripts/verify.sh` — full verification gate
