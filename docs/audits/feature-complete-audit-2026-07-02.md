# Feature-Complete Audit — Iroh Rooms

**Date:** 2026-07-02 · **Repo:** kortiene/iroh-room @ `0e199d3` (main, clean) · **Scope:** `all` (v0.2 north-star vision ⊇ v1 ⊇ mvp)
**Verdict:** At **MVP scope the product is feature-complete on loopback evidence** (every sharp-workflow requirement `Done`, `verify.sh` + all six P0 online tiers re-run PASS during this audit); at **v1 scope it is ~93% Done** with six named, small gaps (UP-101…106 + taxonomy/audit residue); at **`all` scope every v0.2 vision pillar (calls, terminals, tasks, availability layer, QR, multi-device, desktop/mobile, bridges) is `Not Started` by design**. The single biggest obstacle is not code: **Gate A (real-NAT connectivity) has zero measured evidence** — the harness is CI-green but the manual two-host run was never executed, and it is the declared P0 blocking exit condition before any external preview.

**Sources reconciled:** PRD.v0.3.md + PRD.md + PHASE-0-SPIKE.md + PHASE-0-GO-NO-GO.md + docs/protocol.md + RELEASE-READINESS.md + 4 guides (intent) · 40/40 specs + docs/cockpit-backlog.md UP-* + 43/43 issues + 39/39 PRs + 4 milestones (plan) · 7 workspace crates, `scripts/verify.sh` and all six `--ignored` P0 online tiers **actually executed** (status).

Path shorthand: `core/` = `crates/iroh-rooms-core/src/`, `net/` = `crates/iroh-rooms-net/src/`, `cli/` = `crates/iroh-rooms-cli/src/`, `sdk/` = `crates/iroh-rooms/`. Test citations name the crate's `tests/` file.

---

## 1. Product understanding

**What Iroh Rooms is becoming:** a local-first, peer-to-peer collaboration **runtime** ("a private peer-to-peer workspace runtime for humans and agents", PRD.v0.3.md §1) built on iroh: small private rooms where humans, devices, and AI agents exchange signed events, share verified content-addressed files, and expose live local services through authenticated P2P pipes, with room data under participant control and no central application server. The differentiated feature is **Live Pipe** (PRD.v0.3.md §2).

**The three collaboration modes** (PRD.v0.3.md §2) — journeys and standing:

1. **Conversation** — identity → room create (signed genesis) → key-bound ticket invite → join bootstrap (`--accept-joins`) → signed `message.text` exchange → bounded recent sync → restart-durable local history. **Shipped end-to-end**; proven by `full_demo_e2e` (6/6 PASS this audit).
2. **Artifacts** — `file share` (durable blob import + signed `file.shared`) → ACL-gated serving over the iroh-blobs ALPN → `file fetch` with independent BLAKE3 recompute, hash-mismatch hard stop, honest coded unavailability. **Shipped end-to-end**; `blob_e2e`/`file_e2e` (deterministic tier) + `two_peer_e2e` (PASS).
3. **Live work** — `pipe expose --tcp 127.0.0.1:P --allow <MEMBER>` (loopback-only target, explicit per-member authz, SECURITY warning, signed `pipe.opened`) → authorized `pipe connect` over encrypted QUIC → close semantics (`closed`/`owner_exit`/SIGKILL-linger documented). **Shipped end-to-end**; `pipe_e2e` + `pipe_cli` tier (PASS).

**MVP boundary (PRD.v0.3.md §1, verbatim):** "Two humans and one agent can join a private room, exchange signed messages, share one verified artifact, expose a local web preview through an authenticated peer-to-peer pipe, and keep room data locally without a central application server." Cut-line priority (§8.3): Live Pipe survives last.

**Explicit non-goals (v0.3 §7.3, §13.4):** mobile/desktop apps, calls/WebRTC media, Matrix federation/API compat, group E2EE ratchet/PFS, multi-device identity, public rooms/discovery/usernames, push notifications, guaranteed offline delivery, full decentralized history reconciliation, room search, billing, enterprise console, app-store UX, key rotation, multi-device recovery, anonymous credentials, compliance, abuse reporting, spam protection, metadata privacy, post-leak ticket protection. §9.3: terminal sharing/Unix sockets/multiplexing "can come later". §9.1: no Matrix-style state resolution.

**Crate map (all statuses code-verified):** `iroh-rooms-core` — canonical-CBOR signed event model, 10-type registry, stateless `validate_wire_bytes`, membership fold + ancestor-view authz, SQLite store (schema v2), sans-IO sync engine (~13k lines of offline-deterministic tests) · `iroh-rooms-net` — full-mesh QUIC transport (ADR-1), PeerManager, sync carrier, two-gate blob ACL, Live Pipe plane, admission subsystem (10 always-green loopback-QUIC e2e files) · `iroh-rooms-cli` — full command tree, `error[code]:`/`next:` taxonomy, StderrAudit, diagnostics (21 test files, 5 with `#[ignore]` online tiers) · `iroh-rooms` — two-tier SDK façade (stable/experimental), 8 examples incl. the runnable example agent, `facade_e2e` in CI · spikes `spike-blobs` (superseded, retained), `spike-nat` (Gate-A harness, **measurement pending**), `spike-transport` (ADR-1 ratification, measured).

**Inference flags:** the "earliest-joiner roster staleness" characterization (§2 R-124 note) is inferred from the absence of a resident sync process plus the offline-fold members read; no in-repo doc states it.

## 2. Requirement traceability matrix

Coverage: **40/40 specs, 43/43 issues (all closed), 39/39 PRs (38 merged, #65 superseded by #70), 4/4 milestones, 12 docs (~8,000 lines) read in full; `verify.sh` and all 6 P0 online tiers executed at `0e199d3`.**

**Status tally (186 requirements):** Done **142** · In Progress **7** · Not Started **27** · Blocked **1** (Gate A) · Needs Clarification **9**.
**MVP-scope subset:** all Done **except** R-32 (NC, PRD table list), R-38 (NS, blocklist), P-23 (Blocked, Gate A), P-24 (InP, relay validation).

**Status legend:** Done = wired-in crate code + `verify.sh` green + (networked) online tier green · In Progress = partial code / spec-not-landed / proof pending · Not Started = no crate evidence (closed issues and 100% milestones deliberately ignored) · Blocked = named unmet dependency · Needs Clarification = ambiguous, doc-conflicted, or provable only by manual/human run.

### 2a. PRD v0.3 requirements (R-01…R-143; basis: Documented unless noted)

| ID | Requirement (condensed) | Scope | Backlog refs | Code evidence | Status |
|----|--------------------------|-------|--------------|---------------|--------|
| R-01 | Rooms of humans/devices/agents exchanging signed events | mvp | IR-0002/05/08 #6 #9 #12 | core/event/* + membership/fold.rs + net/transport.rs; full_demo_e2e PASS | Done |
| R-02 | Verified content-addressed file sharing | mvp | IR-0202/03/04 #27–29 | net/blob/{mod,serve,fetch}.rs; blob_e2e; two_peer tier PASS | Done |
| R-03 | Live local services via authenticated P2P pipes | mvp | IR-0010/0108 #14 #23 | net/pipe/*; cli/pipe.rs; pipe_e2e + pipe_cli tier PASS | Done |
| R-04 | Data under participant control, no central server | mvp | IR-0004 #8 | core/store (local SQLite); no server component in workspace | Done |
| R-05 | The sharp workflow (2 humans + 1 agent) | mvp | IR-0209 #34 | full_demo_e2e.rs 6/6 PASS (loopback); real-NAT unproven → P-23 | Done |
| R-06 | Ten-step MVP demo end-to-end | mvp | IR-0209/0210 #34 #35 | full_demo_e2e + docs_conformance.rs (guide pinned to binary) | Done |
| R-07 | History persists across restart | mvp | IR-0004/0201 #8 #26 | store_e2e.rs; full_demo_log_validates_after_restart PASS | Done |
| R-08 | Rust core library | mvp | IR-0002.. #6 | crates/iroh-rooms-core + façade | Done |
| R-09 | CLI-first product | mvp | IR-0101.. #16.. | crates/iroh-rooms-cli (cli.rs:37-427) | Done |
| R-10 | Integration tests for two-peer + agent workflow | mvp | IR-0109/0206/0209 | two_peer_e2e, agent_e2e, full_demo_e2e — all PASS | Done |
| R-11 | Append-only signed event log | mvp | IR-0004 | store/mod.rs:106-126 (idempotent, verbatim bytes) | Done |
| R-12 | MVP event-type registry | mvp | IR-0002 #6 | core/event/content.rs:41-62 — **10 types; PRD §9.1 lists 9, omits `member.removed`** (→ §3d) | Done |
| R-13 | Deterministic validation, no state resolution | mvp | IR-0002/08 | validate.rs:79-168 + fold determinism (membership_fold.rs:905) | Done |
| R-14 | Events reference blobs, never carry bytes | mvp | IR-0202 | content.rs file.shared (hash ref); blob bytes via iroh-blobs ALPN | Done |
| R-15 | Content-addressing + fetch-side verification | mvp | IR-0204 #29 | net/blob/fetch.rs:49-147 (independent recompute) | Done |
| R-16 | Honest unfetchable state | v1 | IR-0205 #30 | cli/file.rs:566-570,637-641 (blob_unavailable exit 6); file_cli.rs | Done |
| R-17 | TCP-only pipes in MVP | mvp | IR-0010 | content.rs kind="tcp" only; net/pipe TCP splice | Done |
| R-18 | Hash-derived event IDs | mvp | IR-0002 | signed.rs:229-231 (BLAKE3-256 of wire bytes); golden_vectors.rs:152 | Done |
| R-19 | Canonical encoding + versioned schemas | mvp | IR-0002 | cbor.rs (canonical profile); schema_version=1 enforced | Done |
| R-20 | Identity+device keys; device signs; membership binds | mvp | IR-0002/08 | keys.rs (distinct types); binding.rs; fold device binding | Done |
| R-21 | Creator is admin; only admin invites | mvp | IR-0008 | fold.rs:363-372 (gate_admin_action, genesis-admin only) | Done |
| R-22 | Join = valid ticket + signed member.joined | mvp | IR-0008/0104 | fold.rs:406-476 (gate_join); join_e2e.rs | Done |
| R-23 | Ticket contents (room, inviter, hints, expiry, secret) | mvp | IR-0103 #18 | core/ticket.rs:114-131 | Done |
| R-24 | Ticket expiration from the start | mvp | IR-0103 | ticket.rs expires_at + fold log-only expiry; join_e2e agent_expired_invite | Done |
| R-25 | Pipes: explicit allow, session ID, explicit/exit close | mvp | IR-0010/0108 | net/node.rs:672-800 (CSPRNG pipe_id, close paths) | Done |
| R-26 | Bounded sync; invalid rejected+logged | mvp | IR-0007/0201 | core/sync/config.rs:44-58; AuditSink.event_rejected (net/node.rs:960-978) | Done |
| R-27 | Event envelope fields | mvp | IR-0002 | signed.rs:22-39 (8 fields) + wire.rs:27-36 | Done |
| R-28 | file.shared content schema | mvp | IR-0203 #28 | content.rs:611-827 (file.shared parser + bounds) | Done |
| R-29 | pipe.opened content schema | mvp | IR-0010 | content.rs (pipe.opened parser) | Done |
| R-30 | agent.status content schema | mvp | IR-0208 #33 | content.rs (agent.status: label/message/progress/artifacts) | Done |
| R-31 | SQLite MVP store | mvp | IR-0004 #8 | store/schema.rs (STRICT tables, USER_VERSION=2) | Done |
| R-32 | PRD §12 required-table list (12 tables) | mvp | — | Shipped schema ≠ PRD list: events+event_parents+5 sync tables; identities/devices are JSON files; membership derived by fold (getting-started Step 2 documents the deviation; PRD never reconciled) | **Needs Clarification** (D-8) |
| R-33 | Local-first, append-only, no central DB | mvp | IR-0004 | store/schema.rs:42-141 (events authoritative, rest derived) | Done |
| R-34 | Events exportable | v1 | — **no spec/issue** | **Absent** (grep export/backup over cli+sdk: none) — open Q8 | **Not Started** (D-3) |
| R-35 | Blob pin / garbage-collect | v1 | — no spec/issue | Absent; import uses persistent tag = never GC-eligible (net/blob/mod.rs:25), no management surface | Not Started |
| R-36 | User-controlled local deletion | v1 | — no spec/issue | Absent (no deletion command in cli.rs) | Not Started |
| R-37 | Local-only storage by default | mvp | IR-0004 | No third-party upload path exists in any crate | Done |
| R-38 | **Basic blocklist (explicitly MVP, §13.1.11)** | **mvp** | — **no spec/issue** | **Absent** — repo-wide grep: only unrelated hits (agent-status label "blocked", comments) | **Not Started** (D-2) |
| R-39 | pipe expose requires explicit --allow | mvp | IR-0108 | cli/cli.rs (--allow required, no default-all); pipe.rs:67-84 | Done |
| R-40 | Pipe local bind loopback-only | mvp | IR-0010/0108 | pipe.rs:216 (connect binds loopback listener); expose target validated loopback | Done |
| R-41 | Pipe security warning (target, allowed, ID, close) | mvp | IR-0108 | pipe.rs:118-122; pipe_cli tier PASS (warning/stdout split) | Done |
| R-42 | Pipe closes on owner exit | mvp | IR-0108 | pipe.rs Ctrl-C → pipe.closed{owner_exit}; SIGKILL-linger documented (R-138) | Done |
| R-43 | pipe.closed on clean close | mvp | IR-0010 | node.rs pipe_close authors event; pipe_e2e | Done |
| R-44 | Local audit log for pipe open/connect/reject/close | mvp | IR-0108 | **Stderr lines only** (StderrPipeAudit pipe.rs:148-156); no persisted log; net/audit.rs:7-8 promises future file/SQLite sink | **In Progress** |
| R-45 | No terminal sharing in MVP | mvp | — | Absent by design (consistent) | Done |
| R-46 | Agent has own identity as distinct principal | mvp | IR-0206 #31 | agent.rs:5-12 (ordinary principal, role="agent") | Done |
| R-47 | Agent joins only via explicit invite | mvp | IR-0206/0207 | fold key-bound gate; agent_e2e PASS; agent_invite_flow tier | Done |
| R-48 | Agent events signed with agent's key | mvp | IR-0206 | Same signing path as humans (keys.rs); agent_e2e | Done |
| R-49 | Agent pipes only when authorized | mvp | IR-0209 | full_demo agent_presenter_pipe_forwards_bytes PASS | Done |
| R-50 | Agent artifacts verified like user files | mvp | IR-0304 | example_agent_shares_artifact… PASS (example_agent_e2e tier) | Done |
| R-51–R-58 | Security roadmap: invite revocation, key rotation, device verification, encrypted storage, recovery phrase, agent trust levels, room pipe policies, security review | v1 (roadmap) | — no specs | Absent (all eight; §13.5 "later version"); "no security audit" disclosed in release-notes template | Not Started ×8 |
| R-59 | Honest availability contract | v1 | IR-0205/0110 | cli coded surfaces (CLI-12 inventory); message.rs:375-406; join.rs:381 | Done |
| R-60 | "No central server by default" product language | v1 | IR-0210 | README/getting-started/PRD language present | Done |
| R-61 | identity create → local identity+device keypairs | mvp | IR-0101 #16 | cli identity create; identity_cli.rs | Done |
| R-62 | Profile name on identity | mvp | IR-0101 | --name flag | Done |
| R-63 | room create (ID, admin, log init, signed genesis) | mvp | IR-0102 #17 | cli/room.rs; room_cli.rs | Done |
| R-64 | Admin invite ticket (room-scoped, expiry, copyable) | mvp | IR-0103 #18 | invite.rs; ticket roomtkt1 text codec | Done |
| R-65 | Joiner appears in member list post-join | mvp | IR-0104 #19 | three_way_membership_converges PASS | Done |
| R-66 | Send: signed, broadcast, stored | mvp | IR-0105 #20 | cli/message.rs; message_e2e.rs | Done |
| R-67 | Duplicate IDs ignored | mvp | IR-0002/04 | store ON CONFLICT DO NOTHING (mod.rs:773); vector_08 | Done |
| R-68 | Invalid signatures rejected | mvp | IR-0002 | validate.rs:103-107; bit-flip property test | Done |
| R-69 | Non-member senders rejected | mvp | IR-0008 | fold gate_active_member; membership_fold.rs:781 | Done |
| R-70 | Recent-history sync ACs (request, bounds, validate, dedup, log) | mvp | IR-0007/0201 | core/sync/engine.rs; sync_smoke/convergence/restart suites | Done |
| R-71 | file share → blob store + file.shared event | mvp | IR-0202 #27 | cli/file.rs:138-195 (canonicalize → import → sign → close store) | Done |
| R-72 | Peers fetch from providers + verify | mvp | IR-0204 #29 | file.rs fetch loop; blob_e2e authorized fetch+verify | Done |
| R-73 | Expose TCP + authorize members + pipe.opened | mvp | IR-0010/0108 | node.rs:672-800 | Done |
| R-74 | Authorized connect / unauthorized rejected | mvp | IR-0010 | pipe_e2e p1/p2; two_peer unauthorized_pipe_connection_denied PASS | Done |
| R-75 | Pipe traffic encrypted | mvp | IR-0005 | QUIC/TLS end-to-end (ADR-1); relay forwards ciphertext only | Done |
| R-76 | Close pipe + pipe.closed | mvp | IR-0108 | pipe.rs close; pipe_e2e owner-close teardown | Done |
| R-77 | Invited agent posts agent.status | mvp | IR-0208 #33 | agent_status_delivers_online_and_persists_on_peer PASS | Done |
| R-78 | Agent shares artifacts via same primitives | mvp | IR-0304 | example_agent share_artifact path (main.rs:476) | Done |
| R-79 | identity create --name / identity show | mvp | IR-0101 | cli.rs; identity show --json | Done |
| R-80 | room create/invite/join/send/tail/members | mvp | IR-0102–0106 | cli.rs:282-427 (all six present) | Done |
| R-81 | file share/list/fetch | mvp | IR-0202/04 | cli.rs file tree | Done |
| R-82 | pipe expose/list/connect/close | mvp | IR-0108 | cli.rs pipe tree (list offline, no --json → UP-104) | Done |
| R-83 | agent invite / agent status | mvp | IR-0206/0208 | cli.rs agent tree | Done |
| R-84 | Commands print actionable next steps | v1 | IR-0303 #38 | next: templates error.rs:151-227 for coded paths; **uncoded paths have none** (invite admin-gate, self-invite, bad hex — invite.rs:87-129, pipe.rs:67-84) | **In Progress** |
| R-85 | Failures distinguish offline/unauthorized/unavailable/ticket/signature | v1 | IR-0110/0205 | error.rs:89-111 (peer_offline, peer_unauthorized, blob_unavailable, ticket_*, bad_signature) | Done |
| R-86 | Availability limits explicit in output | v1 | IR-0205 | "no peers online — stored locally only" etc. (message.rs:375-406) | Done |
| R-87 | Script-friendly output where possible | v1 | UP-101/102/104/105; IR-0307/0308 specs | --json on identity show / members / tail --offline / file list only; **absent on room create, pipe list, live tail; `room list` absent entirely** | **In Progress** |
| R-88 | <2s message delivery on normal network | v1 | — | No measurement mechanism; loopback tiers imply local speed; real-network unmeasured (Gate A adjacent) | Needs Clarification |
| R-89 | Files ≤25 MB shareable | v1 | IR-0203 | Cap MAX_SHARED_FILE_BYTES=100MiB (constants.rs) ⊇ 25MB; **divergence tracked, unresolved** (D-4); no size-scale perf test | Done (flagged) |
| R-90 | Interrupted fetch restarts cleanly | v1 | IR-0204 | Atomic temp-then-rename save (file.rs:650-656) → nothing partial persists; *resume* explicitly cut (§8.2.1) | Done (restart, not resume) |
| R-91 | 5-participant room usable | v1 | — | 3-node tests only (loopback.rs t6, full_demo 3-way); 5-peer proven at transport level in spike-transport only | Needs Clarification |
| R-92–R-95 | Human-timed DX metrics (identity <1min, room <3min, pipe <5min, unaided demo) | v1 | IR-0306 | Process defined (RELEASE-READINESS Demo verification) but **no recorded run in repo**; docs_conformance is a proxy for R-95 | Needs Clarification ×4 |
| R-96 | ≥80% users explain availability model | v1 | — | No measurement mechanism anywhere | Needs Clarification |
| R-97 | Relay fallback exists | v1 | IR-0005/0012 | Code: NetMode::RealNetwork = presets::N0 (transport.rs:237-249); **never exercised by any automated test; relay-only pass is part of the pending Gate A runbook** | **In Progress** (proof blocked on P-23) |
| R-98 | Clear connection state | v1 | IR-0107 #22 | members --status; OfflineReason (net/state.rs:54-68) | Done |
| R-99 | Network diagnostics | v1 | IR-0303 | net/diag.rs; --verbose diag: block; diagnostics_cli.rs | Done |
| R-100 | Protocol test vectors | v1 | IR-0003 #7 | tests/conformance/* (all 20 vectors + taxonomy gate) | Done |
| R-101 | Unknown/invalid critical fields rejected deterministically | v1 | IR-0002 | strict content parse, unknown-key rejection (content.rs:864-870) | Done |
| R-102 | Rust SDK (Phase 2) | v1 | IR-0301 #36 PR#79 | crates/iroh-rooms two-tier façade; facade_e2e in CI | Done |
| R-103 | Protocol documentation | v1 | IR-0302 #37 | docs/protocol.md + drift gate (conformance/docs_reference.rs) | Done |
| R-104 | Invite expiration hardening | v1 | IR-0103/0008 | Log-only expiry determinism (fold.rs:406-476) + expired_invite code + expiry e2e | Done |
| R-105 | Improved file availability status | v1 | IR-0202/0205 | file list provider status; coded fetch states; advanced status cut-first (§8.2.4) | Done |
| R-106 | Runnable example agent | v1 | IR-0304 #39 PR#81 | sdk/examples/example_agent/ (702 lines); e2e tier 3/3 PASS | Done |
| R-107 | Dev-preview workflow documented | v1 | IR-0305 #40 PR#80 | docs/live-pipe-preview.md + live_pipe_preview_docs.rs drift guard | Done |
| R-108 | Readiness = script exit code | v1 | IR-0306 #41 PR#82 | scripts/release-readiness.sh; release_readiness_e2e.rs | Done |
| R-109 | P0 deterministic gate = verify.sh | v1 | IR-0306 | **Executed this audit: PASS** (all 5 steps) | Done |
| R-110 | P0 online tiers release-blocking | v1 | IR-0306 | **Executed this audit: 6/6 PASS**; caveat: diagnostics_cli row runs 0 tests (→ §3d) | Done (flagged) |
| R-111 | Gate A tracked P1 + recorded at sign-off | v1 | IR-0306 | Script checks pending status (release-readiness.sh:83-89) | Done (tracking only) |
| R-112 | Known limitations stated up front | v1 | IR-0306 | RELEASE-READINESS Known MVP limitations + notes template | Done |
| R-113 | Two-gate blob ACL | v1 | IR-0204 | net/blob/serve.rs:46-162; blob_e2e 4-outcome matrix | Done |
| R-114 | BLAKE3 recompute, hard stop, safe basename | v1 | IR-0204/05 | fetch.rs recompute; hash_mismatch exit 4 refuses save (file.rs:620-627) | Done |
| R-115 | Agent least privilege + identity-binding guard | v1 | IR-0206/0304 | Role lattice min (model.rs:35-42); example_agent_rejects_ticket PASS | Done |
| R-116 | Ticket warning + secret never leaks | v1 | IR-0103/0110 | Zeroizing buffer (invite.rs:163-165); Debug redaction (ticket.rs:262-276); secret-scan test (error.rs:725) | Done |
| R-117 | Plaintext-storage disclosure (doc-level) | v1 | IR-0306 | Carried by checklist + notes template; no code warning (documented as such) | Done |
| R-118 | Dependency review process | v1 | IR-0306 | Checklist + pins re-verified (PHASE-0-GO-NO-GO §5) | Done |
| R-119 | Human dry-run per candidate build | v1 | IR-0306 | Process defined; no recorded dry-run in repo | Needs Clarification |
| R-120 | Release-notes template | v1 | IR-0306 | Present in RELEASE-READINESS.md | Done |
| R-121 | File-cap divergence recorded per build | v1 | IR-0306 | Checklist mandates; constant + docs state 100MiB | Done |
| R-122 | room tail --offline contract | v1 | IR-0106 #21 | cli/room.rs:395-546; tail_cli.rs (json validity, determinism, --limit) | Done |
| R-123 | room members --json + left/removed statuses | v1 | IR-0106 | room.rs fold render; room_cli.rs | Done |
| R-124 | Per-peer connection lines + members --status | v1 | IR-0107 #22 | message.rs:569-770; manager_e2e. Note (inferred): earliest joiner's roster stale until next network touch — no resident daemon | Done |
| R-125 | Send offline-first ("delivered: 0 … stored locally") | v1 | IR-0105 | message.rs:375-406 | Done |
| R-126 | Join bootstrap via --accept-joins; no_admin_reachable | mvp | IR-0104 | join.rs:381-382; node.rs:1142-1192 (provisional membership-only serving) | Done |
| R-127 | Two-line error contract + warning[code] advisories | v1 | IR-0110 | main.rs:32-45; error_taxonomy.rs (27 offline tests) | Done |
| R-128 | Stable redacted ticket-failure codes | v1 | IR-0110 | error.rs ticket_* (6 codes, exit 5); role-independent codec | Done |
| R-129 | fetch: blob_unavailable/peer_unauthorized/hash_mismatch | v1 | IR-0205 #30 | file.rs:620-641 classify(); file_cli offline + tiers PASS | Done |
| R-130 | --verbose diag contract (stderr, secret-free, advisory) | v1 | IR-0303 | message.rs:680-745; diagnostics_cli live tests (deterministic tier) | Done |
| R-131 | IROH_ROOMS_HOME + --data-dir precedence | v1 | IR-0101 | cli/paths.rs:11-50 (verified: flag > env > default) | Done |
| R-132 | agent invite = thin wrapper over room invite --role agent | v1 | IR-0206 | agent.rs:46-53; delegation-equivalence unit test | Done |
| R-133 | agent status flags + offline-tail render; not role-gated | v1 | IR-0208 | agent.rs:109-154; room.rs:534-546; **role-gating ambiguity vs PRD §15.8 → D-6** | Done |
| R-134 | Tail serves blobs; lock contention fails fast | v1 | IR-0204 | Store exclusive lock 5s → BlobError::Locked (blob/mod.rs:57-67); **`blob_store_locked` is a message prefix (mod.rs:261), not a taxonomy ErrorCode** (minor adoption gap → IR-0313) | Done (flagged) |
| R-135 | SDK façade shape (stable/experimental, examples, example agent) | v1 | IR-0301/0304 | sdk/src/lib.rs:19-113; Cargo.toml:26-38 | Done |
| R-136 | Pipe --allow required, no default-all | v1 | IR-0108 | cli.rs (required, repeatable); pipe.rs empty-allow refused | Done |
| R-137 | Non-loopback pipe target refused | v1 | IR-0108 | pipe.rs pre-IO validation; is_loopback_target (façade-promoted) | Done |
| R-138 | Three close semantics (closed/owner_exit/SIGKILL-linger) | v1 | IR-0305 | pipe.rs + docs; live_pipe_preview_docs.rs pins the doc | Done |
| R-139 | Pipe --expires TTL | v1 | IR-0108 | cli.rs --expires; expiry deny in access.rs:128-131 | Done |
| R-140 | Greppable pipe reject/teardown stderr lines | v1 | IR-0108 | pipe/audit.rs deny causes; StderrPipeAudit; pipe_e2e p10 | Done |
| R-141 | Non-member turned away pre-dial (exit 3) | v1 | IR-0110 | pipe.rs:101-107, 240-241 (coded peer_unauthorized prechecks) | Done |
| R-142 | CLI imports ⊆ façade (sdk-coverage audit) | v1 | IR-0301 | Cross-checked: zero mismatches; superset confirmed | Done |
| R-143 | cbor/sim deliberately absent from façade | v1 | IR-0301 | Verified absent (events.rs:18; experimental/sync.rs:6-8) | Done |

### 2b. Protocol requirements (P-01…P-28, from PHASE-0-SPIKE.md / docs/protocol.md / PHASE-0-GO-NO-GO.md)

| ID | Requirement (condensed) | Scope | Code evidence | Status |
|----|--------------------------|-------|---------------|--------|
| P-01 | Canonical deterministic CBOR (7 profile rules, fixed key order) | mvp | core/event/cbor.rs:114-292; conformance vectors 1-2; proptest | Done |
| P-02 | event_id = blake3(CSB), advisory wire id recomputed | mvp | signed.rs:229-231; validate.rs:87-90; golden event_id | Done |
| P-03 | Domain-separated Ed25519 under device_id | mvp | signed.rs:249-263; wrong-key vector §5 test | Done |
| P-04 | WireEvent envelope, verbatim persistence, re-canonicalize check | mvp | wire.rs:27-131; validate.rs:97-99; store verbatim | Done |
| P-05 | Three-key model + device binding (28-byte BIND_CONTEXT) | mvp | keys.rs; binding.rs:1-60; **spike doc says 27 bytes — arithmetic error in the normative source** (→ §3d) | Done |
| P-06 | room_id derivation + cross-room replay impossibility | mvp | signed.rs:236-247; vector §4/§7 tests | Done |
| P-07 | 11-step verification + 14-code taxonomy + 3 flags | mvp | validate.rs:79-168; reject.rs:21-122; taxonomy completeness gate (DEFERRED empty). Step-3/4 order deviation documented in protocol.md §5 | Done |
| P-08 | created_at advisory-only; log-only expiry; clock_skew flag | mvp | validate.rs:155-160; fold log-only expiry; vector §20 | Done |
| P-09 | DAG rules: ≤20 parents, genesis-descent, self-parent chains | mvp | validate.rs:142-152; fold ancestors; admin_seq | Done |
| P-10 | Derived Lamport + (lamport, event_id) total order; position carries no trust | mvp | store/mod.rs:275-288, 809-890; vector §10 | Done |
| P-11 | Membership fold: Removed-dominates, least-privilege, lowest-id tiebreak | mvp | fold.rs:659-701; model.rs:19-42; vector §11/§18 | Done |
| P-12 | Ancestor-view authorization; hedged convergence guarantee only | mvp | fold.rs:344-384 (gate_active_member); arrival-order determinism tests | Done |
| P-13 | Sticky departure (kick and leave both consume invites) | mvp | fold.rs:480-500; vectors §15/§19 | Done |
| P-14 | Key-bound log-verifiable invites (path A); no bearer tickets | mvp | content.rs:507-518 capability_hash; fold gate_join; bearer excluded (fold.rs:424-426) | Done |
| P-15 | Never-windowed membership sub-DAG + AdminTip mesh frame | mvp | engine.rs:195-202; message.rs:105-119 | Done |
| P-16 | Three-stage out-of-order pipeline + anti-amplification + restart-durable park | mvp | engine.rs:121-172; schema v2 tables; sync_restart.rs (17 tests) | Done |
| P-17 | admin_seq incompleteness detection + fail-closed + equivocation alerts | mvp | engine.rs:67-103; sync_convergence.rs fork tests | Done |
| P-18 | Connect-time authz (blob + pipe gates), revocation-on-learn, wall-clock deny-only | mvp | access.rs:84-134; net/pipe/gate.rs:39-67; watcher.rs; **code admits at now==expires_at (access.rs:129) — matches protocol.md §8, contradicts spike §5** (→ §3d) | Done (flagged) |
| P-19 | 10-type registry, signer/role gates, binding size bounds | mvp | content.rs:41-62, 325-342; constants.rs:25-62 | Done |
| P-20 | 20 normative conformance vectors, Tier-1 byte-exact | mvp | tests/conformance/mod.rs:22-45; golden_vectors.rs | Done |
| P-21 | ADR-1: full-mesh QUIC, admission before bytes, no gossip on log path | mvp | net/transport.rs:229-286; alpn.rs; no iroh-gossip dep in net; spike-transport ratification measured | Done |
| P-22 | ADR-2: hand-rolled SQLite log, derived caches, rebuild determinism | mvp | store/schema.rs; rebuild fixpoint (mod.rs:949-1163) | Done |
| P-23 | **Gate A: real-NAT two-host measurement before external preview** | mvp | Harness complete (spike-nat); **zero committed runs — results.md: "(pending manual two-host run)"; net/NOTES.md:115 "NOT YET RUN"** | **Blocked (manual run never executed — the audit's #1 blocker)** |
| P-24 | Relay fallback semantics (encrypted, accepted mitigation) | mvp | Delegated to iroh presets::N0 (transport.rs:244-248); loopback mode disables relay; **no automated relay test; relay-only pass = Gate A runbook item** | In Progress (blocked on P-23) |
| P-25 | Exact version pins + churn budget | mvp | iroh =1.0.1, blobs =0.103.0, dalek =3.0.0-rc.0 lockstep (Cargo.tomls verified; go-no-go §5) | Done |
| P-26 | Schema-evolution policy before any schema_version 2 | v1 | v=1 hard-enforced (wire.rs:81-131) ✓; **the policy/ADR itself does not exist** | In Progress (D-9) |
| P-27 | Phase-5 iroh-docs key-mapping (conditional) | all | Deferred by design (m/+c/ mapping documented, ADR-2) | Not Started (deferred) |
| P-28 | CLI wraps reason codes verbatim into exit taxonomy | mvp | error.rs:89-111, 281-290 (wrap-verbatim invariant unit-tested) | Done |

### 2c. v0.2 vision requirements (V-01…V-15; basis: Documented-v0.2; scope `all`)

| ID | Vision pillar | Code evidence | Status |
|----|---------------|---------------|--------|
| V-01 | Call Plane (WebRTC via iroh signaling; call.started/ended) | Absent (grep webrtc/call.*: none); v0.3 §19 Phase 6 | Not Started |
| V-02 | Terminal sessions + Unix-socket forwarding | Absent; TCP-only pipes shipped | Not Started |
| V-03 | Tasks/decisions as first-class events (task.created/updated) | Absent (registry is closed at 10 types); v0.3 cut-first §8.2.5 | Not Started |
| V-04 | Agent vocabulary beyond agent.status (output/error/artifact.shared/review.requested) + MX-Agent/MX-Loom integration | Absent; open Q10 (agent runtime authn) unanswered | Not Started |
| V-05 | Availability layer (always-on nodes, pinning, retention, storage peers) | Absent; ADR-2 parks iroh-docs as Phase-5 substrate | Not Started |
| V-06 | QR-code invites | Absent (text roomtkt1 only); v0.3 §8.2.7 cut-first, §19 Phase 4 | Not Started |
| V-07 | Multi-device identity | Absent; protocol structure "generalizes to a set later" (protocol.md §11) | Not Started |
| V-08 | Full decentralized history reconciliation | Absent; bounded recent sync only; P-27 mapping ready | Not Started |
| V-09 | Desktop app (Tauri) | Absent | Not Started |
| V-10 | Mobile app | Absent | Not Started |
| V-11 | Room portability/export/backup | Absent (ties R-34; open Q8) | Not Started |
| V-12 | Security roadmap (= R-51…R-58) | Absent (see 2a) | Not Started |
| V-13 | Self-hosted relay (and later SFU) | No CLI/config surface (grep relay in cli.rs: display only); iroh N0 preset hardwired | Not Started |
| V-14 | Matrix / Jitsi interop bridges | Absent | Not Started |
| V-15 | Richer blob plane (folders, resume, multi-provider, media) | **Partial**: provider sets (≤16) + fetch-loop + post-fetch re-provide shipped (file.rs:650-656); folders/resume/media absent (cut-first §8.2.1-3) | In Progress |

**Dependency edges (ordering the roadmap):** event model (P-01..P-10) → membership/authz (P-11..P-14) → transport (P-21) → {file plane (R-71/72), Live Pipe (R-73..76), sync (P-15..P-17)} — all landed. Open-work edges: UP-105 → UP-106; IR-0308 supersedes UP-103; Gate A → external preview → security review (R-58) → public beta; P-26 policy → any schema_version 2; V-05 availability layer → V-08 reconciliation (per ADR-2).

## 3. Gap analysis

### 3a. Requirements missing from the backlog (no spec, no issue, no code)

| Gap | Evidence of absence | Proposed action (§5) |
|---|---|---|
| **R-38 Basic blocklist — the only unshipped MVP-scoped requirement** | PRD.v0.3.md §13.1.11 lists it as MVP; repo-wide grep finds nothing; no spec/issue mentions it; absent from guides and RELEASE-READINESS | Decision D-2: build (IR-0315) or formally descope via PRD erratum |
| R-34/V-11 event export / backup story | No export surface; PRD §12 principle 5 + open question Q8 | Decision D-3 → spec IR-0314 |
| R-44 persistent audit sink | net/audit.rs:7-8 promises "a file/SQLite sink later"; only stderr/tracing sinks exist | Spec IR-0316 |
| Uncoded-error completeness (R-84 residue) | Non-admin invite refusal = bare `bail!` exit 1 (invite.rs:125-129), pinned as current behavior by full_demo_e2e.rs:1347; `blob_store_locked` message-prefix not an ErrorCode; fetch node's TracingAudit dropped (file.rs:527) | Spec IR-0313 (taxonomy batch) |
| UP-103/104/105/106 | Cockpit-backlog rows only — no spec, no issue, no code (byte counters confirmed absent: splice.rs counts nothing) | Specs IR-0309/0311/0312; UP-103 moot if IR-0308 lands |
| P-26 schema-evolution policy | Constraint enforced in code; the ADR/policy doc does not exist | Decision D-9 → ADR |
| R-91 five-participant usability test | Max 3-party product tests; 5-peer only in spike-transport | Test-only spec (optional) |

### 3b. Planned-not-landed (the real in-flight work — exactly two items)

| Spec / IR / UP | Requirement | Doc `Status:` | Actual code state | Verdict |
|---|---|---|---|---|
| `specs/room-list-read-cli.md` IR-0307 = UP-101 | `room list [--json]` | planning | **Not landed** — `RoomAction` has no `List` variant (cli.rs:282-427); `EventStore::room_ids()` primitive ready (store/mod.rs:206) | Build next (P0); **issue not yet opened — spec says "#TBD"** |
| `specs/live-tail-ndjson-stream.md` IR-0308 = UP-102 | Live `room tail --json` NDJSON, all event types | planning | **Not landed** — `--json` requires `--offline` (cli.rs:383-384); live tail renders message.text only (message.rs:1012-1046) | Build next (P0); issue not yet opened |

### 3c. Orphan code (shipped, but in no PRD section / spec)

- `net/demo.rs:1-13` — self-declared prototype scaffolding ("will be dropped when the CLI wires real identities") that was never dropped. **Cleanup candidate.**
- `crates/spike-blobs` — superseded by the shipped blob plane; its own NOTES.md §7.5 flags graduation/removal as a follow-up. Costs CI time on every PR. **Removal candidate.**
- `manager.rs:29-35` `DialEntry.addr` `#[allow(dead_code)]` — documented future no-op (hint-change detection). Benign, tracked.
- Everything else the code sweep surfaced as "orphan candidates" (access predicates, sync trust layer, anti-amplification, SimNet, admission subsystem, net_smoke, docs-conformance suites) **traces cleanly to P-11…P-24 or docs specs** — not orphans; scope creep verdict: none.
- `agents/` (578 files, 22 ADW run dirs): **gitignored and untracked** (`git ls-files agents` = 0) — local pipeline artifacts, not shipped; no action needed.

### 3d. Doc / marker drift

| Drift | Evidence | Verdict |
|---|---|---|
| **~24 of 40 specs carry "planning/planned/proposed/absent" Status fields for fully-landed work** (e.g. room-join-by-ticket, peer-connection-manager, initial-rust-sdk-surface [landed PR#79], example-agent [PR#81], dev-preview-release-readiness [PR#82]) | Spec extractor sweep vs code/tracker evidence | Spec `Status:` is authoring-status, not implementation-status. Batch-erratum + one-line convention note in CONTRIBUTING (§5) |
| IR-0307/IR-0308 specs say **"Issue #TBD"** — never opened on the tracker | tracker sweep: IR-0307/0308 absent from all 43 issues | Open the two issues (§5 script) |
| Milestones M0–M3 all GitHub-state **open** with 0 open issues each | milestones API | Close all four; create forward milestones (§5) |
| **BIND_CONTEXT length: spike says 27 bytes, protocol.md says 28; the string is 28** — the *normative-precedence* doc carries the arithmetic error | PHASE-0-SPIKE.md Event Protocol §1 vs docs/protocol.md §1 | Erratum (IR-0317) |
| **Pipe-expiry boundary: spike §5 denies at `now == expires_at`; protocol.md §8 admits; code admits** (`now > expiry` denies, access.rs:129) | Verified in code this audit | Erratum: fix the spike to match shipped behavior (IR-0317) |
| `member.removed` device_binding: spike self-contradicts (§1/§6 vs §7 schema); protocol.md resolves as optional | docsV02 sweep | Erratum (IR-0317) |
| `capability_secret`: schema pins bstr[16] exactly; prose says "≥16 bytes" | spike §7 vs Membership §6 | Erratum (IR-0317) |
| **PRD v0.3 §9.1 lists 9 MVP event types, omitting the shipped `member.removed`** (registry = 10, content.rs:41-62) | Verified: PRD.v0.3.md:220-232 | PRD erratum (IR-0317) |
| PRD §12 requires 12 named tables; shipped schema has events + event_parents + 5 sync tables, identities/devices as files, membership derived | schema.rs:42-141; getting-started Step 2 documents the deviation, PRD never updated | PRD erratum or ADR note (D-8) |
| 25 MB (PRD §17.1.9) vs 100 MiB shipped cap | constants.rs MAX_SHARED_FILE_BYTES; RELEASE-READINESS tracks per-build | Decision D-4, then erratum |
| **`diagnostics_cli` ONLINE_TIERS row runs 0 tests** — the file has no `#[ignore]` attributes; its 4 live tests run ungated inside verify.sh, so the release-readiness tier invocation is a structural no-op | Gate run detail: "0 passed; 4 filtered out"; release-readiness.sh:38 | Decision D-7: gate ≥1 test or drop the row (tier-table honesty) |
| Closed-but-unimplemented / open-but-shipped issues | **None found** — every closed IR maps to landed code; the only unshipped specs (IR-0307/0308) were never opened | No action |

### 3e. Blockers for feature completeness

- **P-23 Gate A (real-NAT)** — blocks: external preview (PHASE-0-GO-NO-GO §7 declares it a **P0 blocking exit condition**), validation of R-97/P-24 (relay fallback), realism of R-88. The harness, runbook, and GO/NO-GO rubric are complete; what's missing is ~half a day with two machines on different real networks. **This is the audit's #1 action.**
- **D-2 (R-38 blocklist)** — blocks calling the MVP scope closed on paper: it is the only PRD-MVP requirement with no implementation and no descope record.
- **P-26 policy** — blocks any future `schema_version: 2` (same-set divergence hazard, spike open decision #11).
- **R-58 security review** — blocks any public beta (PRD §13.5.10); currently disclosed as not performed.
- Open decision #10 (iroh-blobs 0.103 pre-production vs 0.35) — not a blocker today (0.103 shipped and pinned) but an unratified decision carried silently (D-5).

## 4. Technical readiness

**CI state (executed during this audit at `0e199d3`):** `scripts/verify.sh` **PASS** (fmt ✓, clippy pedantic -D warnings ✓, workspace tests ✓, SDK doctests 6/6 ✓, default-features examples build ✓) · **P0 online tiers 6/6 PASS**: two_peer_e2e 5/5 (11.8s), full_demo_e2e 6/6 (38.3s), pipe_cli 1/1 (4.9s), agent_e2e 2/2 (5.2s), example_agent_e2e 3/3 (128.3s), error_taxonomy_e2e 1/1 (34.0s) + diagnostics_cli (0 gated — see D-7).

| Area | Finding | Evidence | Severity |
|------|---------|----------|----------|
| Protocol & event model | No gap found. Canonical CBOR, golden vectors (byte-exact, independently reproduced ×3), taxonomy-completeness gate with empty DEFERRED list, docs-drift gate embedding protocol.md | core/event/*; tests/conformance/* | — |
| Membership & authz | No gap found. Ancestor-view gate confirmed at fold.rs:344-384; fold ignores content events for snapshots (by design — snapshot-equality assertions over content events are vacuous, don't cite them as coverage); access predicates use *current* snapshot (correct for connect-time gates) | fold.rs; access.rs; membership_fold.rs (32 tests) | — |
| Transport / NAT | **Gate A never run** (zero committed measurements); relay path has no automated test — direct-vs-relay is observed (diag.rs) never controlled; `clear_ip_transports` exists only in spike-nat | spike-nat/results/results.md:10; transport.rs:237-249 | **High** |
| Blob & file plane | No functional gap. Two-gate ACL, independent recompute, atomic saves, lock-contention fail-fast. Minor: `blob_store_locked` not a taxonomy code; fetch node audit uses TracingAudit (dropped) | net/blob/*; file.rs:527 | Low |
| Live Pipe | No functional gap. **No byte counters anywhere** (splice.rs counts nothing) — UP-106 blocked on this; session accounting is count-only | net/pipe/splice.rs:20-48 | Low (feature gap, not defect) |
| CLI surface | `room list` absent; live NDJSON tail absent (the two planned-not-landed items); `--json` coverage partial (4 of ~8 read surfaces); non-admin invite refusal uncoded exit 1 (both `room invite` and `agent invite`) | cli.rs:282-427; invite.rs:125-129 | Medium |
| SDK façade | No gap found. Tier discipline enforced by cfg; doctests in CI; facade_e2e proves online tier through façade-only imports; sdk-coverage.md cross-check: zero mismatches. Caveat: façade doesn't wrap iroh's transport identity primitives (documented, OQ5); `publish = false` (crates.io deferred, OQ3) | sdk/src/*; facade_e2e.rs | Low |
| Identity & crypto | No gap found. Distinct key types (wrong-key misuse doesn't type-check); dalek pinned lockstep with iroh; secrets zeroized; ticket Debug redaction; secret-scan test over all error templates | keys.rs; ticket.rs:262-276 | — |
| Test tiers | Unit + property (proptest) + conformance/golden + deterministic loopback-QUIC e2e + gated online tiers. **Fuzz targets absent** (spec title promised them; only proptest exists — no cargo-fuzz anywhere). Zero TODO/FIXME/unimplemented!/todo! in all of crates/ | gate agent grep (empty); cbor_property.rs | Low |
| CI gates | verify.yml runs verify.sh only on PR/push; the six online tiers run **only via manual release-readiness.sh** — a green CI does not attest them (by design, but a scheduled CI job could). diagnostics_cli tier is a no-op row (D-7). `--skip-online` can never yield READY ✓ | .github/workflows/verify.yml; release-readiness.sh:59-70 | Medium |
| Observability | **CLI installs no tracing subscriber** — TracingAudit/TracingPipeAudit output dropped; mitigated by StderrAudit on tail/members/send/join/pipe (not fetch); no persistent audit sink (promised in net/audit.rs:7-8); Node::logs bounded reject ring polled by tail | main.rs:1-48 (verified: no subscriber) | Medium |
| Security posture | No security review performed (disclosed); plaintext local storage (doc-disclosed, no code warning); residuals #1–#9 accepted and documented — #8 (admin-key compromise = total unrecoverable loss) is the named largest; branch-protection claims in CONTRIBUTING not machine-verifiable from checkout | PHASE-0-SPIKE Residual Risks; RELEASE-READINESS | Medium (accepted for preview; blocking for beta via R-58) |
| Spikes | spike-transport: claims CI-reproven, decision reflected in net (no iroh-gossip dep) ✓. spike-nat: harness ✓, measurement pending (Gate A). spike-blobs: superseded, removal flagged by its own NOTES §7.5, still costing CI on every PR | crates/spike-*/NOTES.md | Low |
| Dependencies | iroh =1.0.1 / iroh-blobs =0.103.0 (pre-production — open decision #10) / dalek =3.0.0-rc.0 lockstep; lockfile committed; spikes off the shipping dep tree | Cargo.tomls; go-no-go §5 | Medium (0.103 churn watch) |

## 5. Backlog restructuring proposal (NOT EXECUTED — review, then run)

**Specs to write** (repo convention: one spec → one IR → one PR):

| IR | Spec | Covers | Pri | Size |
|----|------|--------|-----|------|
| IR-0309 | `specs/pipe-list-json.md` | UP-104 `pipe list --json` (mirror `file list --json` pattern) | P1 | S |
| IR-0311 | `specs/structured-diagnostics-json.md` | UP-105 `--json` diag block (machine form of diag.rs output) | P2 | S |
| IR-0312 | `specs/runtime-counters.md` | UP-106 pipe byte counters (splice.rs), fetch progress, sync/backfill stats; depends IR-0311 | P3 | M |
| IR-0313 | `specs/error-taxonomy-completeness.md` | Code the non-admin invite refusal as Auth/3 (both surfaces; un-pin full_demo_e2e.rs:1347); adopt `blob_store_locked` as ErrorCode; StderrAudit on the fetch node (file.rs:527); next: lines for remaining bare paths | P1 | S |
| IR-0314 | `specs/room-export.md` | R-34/V-11 after decision D-3 (likely: `room export` = verbatim WireEvent NDJSON/CBOR dump + re-import validation) | P2 | M |
| IR-0315 | `specs/basic-blocklist.md` | R-38 after decision D-2 (or PRD erratum instead) | D-2 | M |
| IR-0316 | `specs/persistent-audit-sink.md` | R-44 residue: file/SQLite AuditSink + PipeAuditSink | P2 | M |
| IR-0317 | `specs/protocol-doc-errata.md` | Doc-only batch: BIND_CONTEXT 28B, expiry comparator (spike→match code), member.removed in PRD §9.1, PRD §12 table reconciliation, capability_secret prose, 25MB↔100MiB per D-4 | P1 | S |
| — | (no spec — runbook execution) | **Gate A manual two-host run**: execute crates/spike-nat/NOTES.md §4, commit per-run JSON + regenerated results.md, fill the §6 findings block, update PHASE-0-GO-NO-GO §2 and RELEASE-READINESS sign-off | **P0** | S |

UP-103 (text-mode live-tail display gap): **do not spec** — IR-0308 supersedes it; revisit only if NDJSON is rejected.

**Milestones:** close M0–M3 (all 0 open). Create **M4 "Preview Exit"** (exit: Gate A verdict committed; IR-0307/0308/0313/0317 landed; release-readiness READY on a recorded run) and **M5 "Cockpit-enabling reads"** (exit: IR-0309/0311 landed; IR-0312 optional). v0.2 vision pillars (V-*) stay unscheduled until a Phase-3+ PRD revision — do not open issues for them yet.

**Label taxonomy:** existing `type/*`, `area/*`, `priority/*`, `risk/*` conventions suffice; add `area/docs-errata` for IR-0317-class work.

```bash
# --- Issues to open (specs already exist for the first two) ---
gh issue create --repo kortiene/iroh-room \
  --title "[IR-0307] Implement room list read CLI" \
  --label "type/feature,area/cli,priority/p1,risk/low" --milestone "M4 - Preview Exit" \
  --body "Spec: specs/room-list-read-cli.md (complete build plan; update its 'Issue #TBD' header once this issue exists).
Offline \`room list\` + \`--json\` over EventStore::room_ids (store/mod.rs:206). External consumer: cockpit UP-101.
AC: per spec §10. Traceability: PRD.v0.3.md §16 (script-friendly output)."

gh issue create --repo kortiene/iroh-room \
  --title "[IR-0308] Stream all validated event types from live room tail as NDJSON" \
  --label "type/feature,area/cli,priority/p1,risk/low" --milestone "M4 - Preview Exit" \
  --body "Spec: specs/live-tail-ndjson-stream.md (update 'Issue #TBD'). Relax --json/--offline conflict (cli.rs:383-384);
NDJSON stream reusing offline TailRow schema; supersedes cockpit UP-103. Consumer: UP-102."

gh issue create --repo kortiene/iroh-room \
  --title "[IR-0313] Error-taxonomy completeness: code the admin-gate refusal, blob_store_locked, fetch audit sink" \
  --label "type/feature,area/cli,priority/p1,risk/low" --milestone "M4 - Preview Exit" \
  --body "1) Non-admin room/agent invite refusal → error[insufficient_role] exit 3 (today: bare exit 1, invite.rs:125-129; un-pin full_demo_e2e.rs:1347-1387).
2) blob_store_locked as ErrorCode (today: message prefix only, net/blob/mod.rs:261).
3) file fetch node: StderrAudit instead of dropped TracingAudit (file.rs:527).
4) next: lines for remaining bare paths (self-invite, bad --invitee hex, pipe pre-IO validations)."

gh issue create --repo kortiene/iroh-room \
  --title "[Gate A] Execute the real-NAT two-host runbook and commit results" \
  --label "type/measurement,area/net,priority/p0,risk/high" --milestone "M4 - Preview Exit" \
  --body "Run crates/spike-nat/NOTES.md §4 on two machines on different real networks (both directions × natural/relay-only).
Commit per-run JSON under crates/spike-nat/results/, regenerate results.md, fill NOTES.md §6 findings,
update PHASE-0-GO-NO-GO.md §2 Gate A row and the RELEASE-READINESS sign-off Gate-A field.
GO rubric: NOTES.md §5. This is the declared P0 blocking exit condition for any external preview."

gh issue create --repo kortiene/iroh-room \
  --title "[IR-0317] Protocol/PRD documentation errata batch" \
  --label "type/docs,area/docs-errata,priority/p1" --milestone "M4 - Preview Exit" \
  --body "Fix: BIND_CONTEXT 27→28 bytes (PHASE-0-SPIKE §1); pipe-expiry boundary comparator in spike §5 → match shipped now>expiry (access.rs:129, protocol.md §8 already correct); add member.removed to PRD.v0.3 §9.1 registry list; reconcile PRD §12 table list with shipped schema (schema.rs) or record as ADR; capability_secret bstr[16] vs '≥16 bytes' prose; record D-4 verdict on 25MB/100MiB."

# --- Tracker hygiene ---
gh api -X PATCH repos/kortiene/iroh-room/milestones/1 -f state=closed   # M0 (repeat for M1..M3 ids 2..4)
gh api -X POST repos/kortiene/iroh-room/milestones -f title="M4 - Preview Exit" \
  -f description="Gate A verdict + IR-0307/0308/0313/0317 + recorded READY run"
gh api -X POST repos/kortiene/iroh-room/milestones -f title="M5 - Cockpit-enabling reads" \
  -f description="IR-0309 pipe list --json, IR-0311 structured diag; IR-0312 optional"

# --- Spec Status errata (batch, doc-only; do NOT change spec bodies) ---
# For each landed spec still marked planning/planned/proposed (24 files — list in §3d),
# flip the Status line to: "landed — implemented in issue #N / IR-XXXX; this document is the build plan."
# Add one line to CONTRIBUTING.md: "A spec's Status: field records doc-authoring status;
# implementation status lives in the tracker and the binary."
```

## 6. Roadmap and decisions

**Definition of feature-complete, per scope:**

- **MVP** = today's shipped set **+** D-2 resolved (blocklist built or formally descoped) **+** R-32 reconciled (doc-only) **+** a committed Gate A verdict (GO, or NO-GO with the documented relay-only escalation accepted). Quality gates: verify.sh green ✓ (holds), 6 P0 online tiers green ✓ (holds), release-readiness READY on a recorded run (pending Gate A field).
- **v1 / developer-preview GA** = MVP-complete **+** IR-0307, IR-0308, IR-0313, IR-0317 **+** UP-104/105 (IR-0309/0311) **+** D-3 export decision executed or explicitly deferred in the PRD **+** R-92–R-95 human-timed metrics recorded once **+** persistent audit sink (IR-0316) or explicit deferral. R-51…R-58 remain roadmap (not v1-blocking) **except R-58 (security review), which blocks any public beta**.
- **`all` / v0.2 north star** = V-01…V-15; sequence per PRD v0.3 §19 Phases 3–6 (agent vocabulary → desktop+QR → availability layer → calls). Not schedulable from current state; requires a Phase-3 PRD revision first.

**Sequence:**
1. **Now (days):** Gate A run (P0, ~half a day, unblocks everything external) · open IR-0307/0308 issues and land them (specs are complete build plans) · IR-0313 taxonomy batch · IR-0317 errata · close M0–M3, open M4/M5.
2. **Next (1–2 weeks):** IR-0309 pipe-list JSON · IR-0311 structured diag · IR-0316 audit sink · decisions D-2/D-3/D-4/D-5 recorded · one recorded release-readiness READY run with human-timed metrics (R-92–95, R-119).
3. **Then:** IR-0312 counters (after IR-0311) · IR-0314 export (after D-3) · P-26 schema-evolution ADR (before any v2) · security review scheduling (R-58) gate to public beta · Phase-3 PRD revision to schedule V-pillars.

**Top 5 highest-leverage tasks:**
1. **Execute Gate A** — the only P0 blocker; converts "works on loopback" into "works between real peers", or triggers the documented relay-only escalation early instead of at launch.
2. **Land IR-0307 + IR-0308** — two small, fully-specced CLI reads that unblock the entire downstream Cockpit M2 plan and close R-87's biggest holes.
3. **IR-0313 taxonomy batch** — small change, closes the last uncoded failure paths (R-84), and un-pins a test that currently enshrines the wrong behavior.
4. **Tracker/spec-status hygiene** (§5 script) — makes the backlog legible to engineers and coding agents; today the tracker reads "everything done" while the real work list lives in two specs and a planning doc.
5. **IR-0317 doc errata** — protocol.md/spike are the interop contract; the BIND_CONTEXT and expiry-comparator errors will bite any independent implementer.

**Top 5 risks:**
1. **Gate A returns NO-GO late** (likelihood unknown — that's the point; impact high): product premise fails on real networks at first external demo. Mitigation: run now; escalation branch (self-hosted relay / discovery config) already documented in PHASE-0-GO-NO-GO §7.
2. **iroh-blobs 0.103 pre-production pin** (med/med): API/wire churn or unpatched issues on the blob path; open decision #10 never ratified. Mitigation: D-5 ADR + churn budget per release (R-118 already mandates review).
3. **Admin-key compromise/loss is total and unrecoverable** (low/high — residual #8, the spike's named largest): Mitigation: keep loudly disclosed; prioritize R-52 key rotation in the security roadmap when v1 hardening starts.
4. **Schema-evolution trap** (low/high): a second schema_version without the P-26 policy causes same-set divergence. Mitigation: D-9 ADR before any registry/schema change — including the V-03/V-04 event types, which is why V-pillar work must not start casually.
5. **Downstream text-scraping coupling** (certain-today/med): the Cockpit's parsers pin the CLI's text output; every text tweak breaks them. Mitigation: land the JSON surfaces (IR-0307/0308/0309/0311) before consumers harden.

**Decisions needed immediately:**

| ID | Decision | Why it blocks | Options | Recommendation |
|----|----------|---------------|---------|----------------|
| D-1 | Schedule the Gate A two-host run | Declared P0 exit condition; blocks external preview | run now / accept relay-only posture without data | **Run now** |
| D-2 | R-38 blocklist: build or descope | Only unshipped MVP-paper requirement | IR-0315 / PRD erratum ("member.removed + sticky departure is the MVP block mechanism") | **Descope via erratum** — removal semantics already cover the in-room case; revisit at V-12 |
| D-3 | Export/backup story (R-34, open Q8) | PRD principle with zero surface | verbatim-event dump cmd / document "copy rooms.db" / defer | Spec IR-0314 as verbatim WireEvent dump (cheap, protocol-aligned) |
| D-4 | 25 MB vs 100 MiB file cap | Tracked-per-build divergence, never resolved | change constant / amend PRD | Amend PRD to 100 MiB (code+docs already agree) |
| D-5 | Ratify iroh-blobs 0.103 (open decision #10) | Unratified pre-production dep on the blob path | ADR ratify / plan 0.35 fallback | ADR ratify with churn-watch clause |
| D-6 | agent.status role-gating (PRD §15.8 vs shipped any-active-member) | Spec ambiguity | gate to role=agent / keep open | Keep open (shipped behavior), record in PRD erratum |
| D-7 | diagnostics_cli tier is a 0-test no-op row | Release-readiness table honesty | gate ≥1 live diag test with #[ignore] / drop row | Gate the existing live test (1-line change) |
| D-8 | PRD §12 table list vs shipped schema | Doc/architecture contradiction | PRD erratum / ADR | PRD erratum pointing at schema.rs + fold-derivation rationale |
| D-9 | Schema-evolution policy (P-26) | Blocks any schema_version 2 and all V-03/V-04 event-type work | lock-step / forward-compat ADR | Write the ADR when scheduling the first registry change; hard rule until then: no v2 |

**Recommended next work for an engineer/agent, in order:** Gate A run → IR-0307 → IR-0308 → IR-0313 → IR-0317 → §5 hygiene script. Every item has a complete spec or a §5 issue body to start from.

## 7. Coverage appendix

- **Docs read (12, in full):** PRD.v0.3.md, PRD.md, PHASE-0-SPIKE.md, PHASE-0-GO-NO-GO.md, RELEASE-READINESS.md, README.md, CONTRIBUTING.md, docs/{protocol, getting-started, live-pipe-preview, sdk-coverage, cockpit-backlog}.md (~8,000 lines).
- **Specs:** 40/40, each fully read by a dedicated extractor (verbatim Status fields, ACs, landed-claims captured).
- **Tracker:** 43/43 issues (all closed; 4 epics), 39/39 PRs (38 merged; #65 superseded by #70), 4/4 milestones. IR coverage IR-0001–IR-0012, 0101–0110, 0201–0210, 0301–0306 all map issue↔PR↔code; IR-0307/0308 exist as specs only.
- **Code:** all 7 crates capability-mapped with file:line citations by three independent read-only agents; `verify.sh` and all six P0 online tiers executed (not inferred) at `0e199d3`; stub grep across crates/: zero hits.
- **Unread sources:** none material — external links in docs are reference-only (rustup, localhost examples); no Notion/Figma/Jira detected; `.adw/` prompt pack skimmed via CONTRIBUTING cross-check only.
- **Confidence notes:** (1) Status assignments for networked features rest on loopback tiers — real-network behavior is exactly the Gate A unknown; (2) five contested facts (blocklist absence, blob_store_locked, IROH_ROOMS_HOME, expiry comparator, PRD §9.1 list) were independently re-verified by direct grep during the audit; (3) absence claims for R-34/35/36, V-01/03/13 grep-verified; (4) branch-protection/process claims in CONTRIBUTING are not verifiable from a checkout; (5) an hour of human review is best spent on §2's Needs Clarification rows (R-88, R-91–96, R-119) — all are measurement/process items, not code disputes.
