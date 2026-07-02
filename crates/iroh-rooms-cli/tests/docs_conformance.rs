//! Structural conformance tests for `docs/getting-started.md`.
//!
//! These tests are deterministic and require no network, no binary execution, and no
//! external services.  They verify the issue #35 acceptance criteria purely by
//! reading the Markdown source.

use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is crates/iroh-rooms-cli; workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

fn guide() -> String {
    let path = workspace_root().join("docs/getting-started.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("docs/getting-started.md must exist at {}", path.display()))
}

fn readme() -> String {
    let path = workspace_root().join("README.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("README.md must exist at {}", path.display()))
}

// ── Deliverables ────────────────────────────────────────────────────────────

#[test]
fn guide_file_exists() {
    // Acceptance criterion: guide lives at docs/getting-started.md (spec §4).
    assert!(
        workspace_root().join("docs/getting-started.md").exists(),
        "docs/getting-started.md must exist"
    );
}

#[test]
fn readme_links_to_getting_started() {
    // Acceptance criterion: README links to the guide (spec §4).
    let content = readme();
    assert!(
        content.contains("docs/getting-started.md"),
        "README.md must contain a link to docs/getting-started.md"
    );
}

// ── Copy-pasteable commands with marked placeholders ─────────────────────────

#[test]
fn guide_defines_placeholder_legend() {
    // Acceptance criterion: commands are copy-pasteable with placeholders clearly marked
    // (spec §5.3 / issue AC).
    let content = guide();
    for placeholder in &[
        "<ROOM_ID>",
        "<BOB_TICKET>",
        "<FILE_ID>",
        "<PIPE_ID>",
        "<BOB_ID>",
        "<AGENT_ID>",
    ] {
        assert!(
            content.contains(placeholder),
            "guide must define placeholder {placeholder} in its legend"
        );
    }
}

#[test]
fn guide_covers_full_mvp_flow() {
    // Acceptance criterion: identity → room → invite/join → message → file → pipe →
    // agent status (issue AC / spec §6.4–§6.10).
    let content = guide();
    let required_commands = [
        "identity create",
        "room create",
        "room invite",
        "room join",
        "room send",
        "room tail",
        "file share",
        "file fetch",
        "pipe expose",
        "pipe connect",
        "agent status",
    ];
    for cmd in &required_commands {
        assert!(
            content.contains(cmd),
            "guide must document the `{cmd}` command (spec §6)"
        );
    }
}

// ── Failure modes documented with next actions ───────────────────────────────

#[test]
fn all_four_failure_modes_are_documented() {
    // Acceptance criterion: offline peer, unauthorized peer, invalid ticket,
    // unavailable file each appear in the guide (issue AC / spec §6.13).
    let content = guide();
    let lower = content.to_lowercase();
    for mode in &[
        "offline peer",
        "unauthorized peer",
        "invalid ticket",
        "unavailable file",
    ] {
        assert!(
            lower.contains(mode),
            "guide must document the '{mode}' failure mode"
        );
    }
}

#[test]
fn failure_modes_have_next_actions() {
    // Acceptance criterion: every failure mode ends with a concrete next action
    // (issue AC: "Failure modes are documented with next actions").
    let content = guide();
    let lower = content.to_lowercase();
    let count = lower.matches("next action").count();
    assert!(
        count >= 4,
        "guide must document a 'next action' for each of the four failure modes; \
         found {count} occurrence(s)"
    );
}

#[test]
fn troubleshooting_section_exists() {
    let content = guide();
    assert!(
        content.contains("Troubleshooting") || content.contains("troubleshooting"),
        "guide must contain a Troubleshooting section"
    );
}

// ── Availability model ───────────────────────────────────────────────────────

#[test]
fn availability_model_section_exists() {
    // Acceptance criterion: demo does not imply guaranteed offline delivery;
    // requires an explicit availability section (spec §6.12).
    let content = guide();
    assert!(
        content.contains("Availability model") || content.contains("availability model"),
        "guide must contain an Availability model section"
    );
}

#[test]
fn availability_model_states_no_guaranteed_offline_delivery() {
    // PRD §14 bullet 4 (spec §6.12 #4).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("no guaranteed offline delivery")
            || lower.contains("not guaranteed")
            || lower.contains("never guaranteed"),
        "availability model must state there is no guaranteed offline delivery"
    );
}

#[test]
fn availability_model_states_no_central_server() {
    // PRD §14 bullet 5 (spec §6.12 #5).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("no central") || lower.contains("no cloud inbox"),
        "availability model must state there is no central server / cloud inbox"
    );
}

#[test]
fn availability_model_covers_live_pipe_requires_both_online() {
    // PRD §14 bullet 3 (spec §6.12 #3).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("both peers"),
        "availability model must state live pipes require both peers online"
    );
}

#[test]
fn guide_does_not_imply_guaranteed_offline_delivery() {
    // Negative / regression test: no step may assert positive guaranteed delivery
    // (issue AC: "Demo does not imply guaranteed offline delivery").
    let content = guide();
    // Strip known-good negation patterns before checking for the bad phrase.
    let stripped = content
        .to_lowercase()
        .replace("no guaranteed offline delivery", "SAFE")
        .replace("not guaranteed", "SAFE")
        .replace("never guaranteed", "SAFE")
        .replace("does not guarantee", "SAFE")
        .replace("no guarantee", "SAFE")
        // "never as queued or guaranteed delivery" spans two lines in the guide
        .replace("never as\nqueued or guaranteed delivery", "SAFE")
        .replace("never as queued or guaranteed delivery", "SAFE");
    assert!(
        !stripped.contains("guaranteed delivery"),
        "guide must not claim delivery is guaranteed without negation"
    );
}

// ── Security framing ─────────────────────────────────────────────────────────

#[test]
fn guide_frames_tickets_as_secrets() {
    // Spec §6.6 / §7.5: tickets are key-bound capabilities; must be treated as passwords.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("password") || lower.contains("secret"),
        "guide must warn that invite tickets are secrets (treat like a password)"
    );
}

#[test]
fn pipe_step_includes_security_warning_and_allow_flag() {
    // PRD §16.2 / §13.2: pipe expose must display a prominent security warning
    // and the guide must show the --allow flag (spec §6.9).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("security") && lower.contains("pipe"),
        "guide must document the pipe security warning (PRD §16.2)"
    );
    assert!(
        content.contains("--allow"),
        "guide must show the --allow flag for pipe expose authorization"
    );
}

// ── Repeatability ─────────────────────────────────────────────────────────────

#[test]
fn guide_provides_reset_instructions() {
    // Test Plan: "Run the guide from a clean checkout and fresh local data directory."
    // Spec §6.14: reset / clean up section.
    let content = guide();
    assert!(
        content.contains("rm -rf .demo")
            || content.contains("Reset")
            || content.contains("clean up")
            || content.contains("Clean up"),
        "guide must include reset/cleanup instructions so the demo is repeatable"
    );
}

// ── Agent role ────────────────────────────────────────────────────────────────

#[test]
fn guide_explains_agent_requires_explicit_invite() {
    // Spec §6.10: agent could only post because it was explicitly invited (spike §3.5).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("agent") && (lower.contains("role") || lower.contains("invited")),
        "guide must explain that the agent requires an explicit invite"
    );
}

// ── Placeholder completeness ──────────────────────────────────────────────────

#[test]
fn guide_includes_agent_ticket_placeholder() {
    // Spec §5.3: <AGENT_TICKET> is a required entry in the placeholder legend.
    let content = guide();
    assert!(
        content.contains("<AGENT_TICKET>"),
        "guide must define the <AGENT_TICKET> placeholder in its legend (spec §5.3)"
    );
}

// ── Data-directory isolation ──────────────────────────────────────────────────

#[test]
fn guide_documents_iroh_rooms_home() {
    // Spec §5.1 (MUST): the guide must define how to point the CLI at a per-participant
    // data directory; expected to be IROH_ROOMS_HOME env var and/or --data-dir flag.
    let content = guide();
    assert!(
        content.contains("IROH_ROOMS_HOME") || content.contains("--data-dir"),
        "guide must document IROH_ROOMS_HOME (or --data-dir) for per-participant data isolation \
         (spec §5.1)"
    );
}

#[test]
fn guide_uses_three_isolated_demo_directories() {
    // Spec §5.1: three distinct data directories for Alice, Bob, and Agent on one host.
    let content = guide();
    assert!(
        content.contains(".demo/alice") && content.contains(".demo/bob"),
        "guide must demonstrate per-participant data directories (.demo/alice, .demo/bob)"
    );
}

// ── Additional required commands ──────────────────────────────────────────────

#[test]
fn guide_documents_room_members_command() {
    // Spec §6.5 / §6.6: `room members` is used after room creation and after invite/join
    // to verify convergence.
    let content = guide();
    assert!(
        content.contains("room members"),
        "guide must document `room members` to verify membership convergence (spec §6.5–§6.6)"
    );
}

#[test]
fn guide_documents_file_list_command() {
    // Spec §6.8: Bob uses `file list` before fetching to discover shared files.
    let content = guide();
    assert!(
        content.contains("file list"),
        "guide must document `file list` so Bob can discover shared files before fetching \
         (spec §6.8)"
    );
}

#[test]
fn guide_documents_pipe_close_command() {
    // Spec §6.9: Alice must explicitly close the pipe; the guide must show `pipe close`.
    let content = guide();
    assert!(
        content.contains("pipe close"),
        "guide must document `pipe close` so Alice can terminate the pipe (spec §6.9)"
    );
}

#[test]
fn guide_pipe_close_uses_bare_pipe_id_not_two_positionals() {
    // IR-0108 §5.2 reconcile: `pipe close` now takes `<PIPE_ID>` only — the old two-positional
    // form `pipe close <ROOM_ID> <PIPE_ID>` must NOT appear in the guide (it was never in the
    // PRD and diverged from the spec's own command table). The guide must show the canonical
    // single-positional form after the reconcile.
    let content = guide();
    // The guide must contain the bare `pipe close <PIPE_ID>` usage (any casing of placeholder).
    assert!(
        content.contains("pipe close <PIPE_ID>") || content.contains("pipe close $PIPE_ID"),
        "guide must document `pipe close <PIPE_ID>` (no ROOM_ID positional) per IR-0108 §5.2"
    );
    // The old two-positional form must not appear as a command example.
    assert!(
        !content.contains("pipe close <ROOM_ID> <PIPE_ID>")
            && !content.contains("pipe close $ROOM_ID $PIPE_ID"),
        "guide must not show the old `pipe close <ROOM_ID> <PIPE_ID>` two-positional form \
         (reconciled in IR-0108 §5.2)"
    );
}

#[test]
fn guide_documents_pipe_close_owner_exit_behavior() {
    // IR-0108 §5.5 / issue AC5: the guide must document that the pipe closes on owner process
    // exit (SIGINT/SIGTERM). This is the "closes on owner process exit" acceptance criterion.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("owner")
            && (lower.contains("ctrl-c")
                || lower.contains("sigint")
                || lower.contains("process exit")),
        "guide must document that the pipe closes on owner process exit (IR-0108 AC5 / §5.5)"
    );
}

#[test]
fn guide_documents_pipe_loopback_requirement() {
    // IR-0108 §5.2 / issue AC2 / §13.2.3: the guide must document that --tcp requires a
    // loopback address (security boundary — no exposing arbitrary network services).
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("loopback") || lower.contains("127.0.0.1"),
        "guide must document that --tcp requires a loopback address (IR-0108 §13.2.3 / AC2)"
    );
}

// ── Troubleshooting reason codes (spike §8) ───────────────────────────────────

#[test]
fn troubleshooting_documents_bad_capability_code() {
    // Spike §8 / spec §6.13: invalid ticket maps to reason code bad_capability.
    let content = guide();
    assert!(
        content.contains("bad_capability"),
        "troubleshooting section must document the 'bad_capability' reason code (spike §8)"
    );
}

#[test]
fn troubleshooting_documents_expired_invite_code() {
    // Spike §8 / spec §6.13: expired ticket maps to reason code expired_invite.
    let content = guide();
    assert!(
        content.contains("expired_invite"),
        "troubleshooting section must document the 'expired_invite' reason code (spike §8)"
    );
}

#[test]
fn troubleshooting_documents_pipe_connect_rejected_code() {
    // Spike §8 / §5 / spec §6.13: unauthorized pipe connect maps to pipe.connect.rejected.
    let content = guide();
    assert!(
        content.contains("pipe.connect.rejected"),
        "troubleshooting section must document 'pipe.connect.rejected' reason code (spike §8)"
    );
}

#[test]
fn troubleshooting_documents_bad_signature_code() {
    // Spike §8 / PRD §16.3 / spec §6.13: invalid signature maps to bad_signature.
    let content = guide();
    assert!(
        content.contains("bad_signature"),
        "troubleshooting section must document the 'bad_signature' reason code (spike §8)"
    );
}

#[test]
fn troubleshooting_documents_not_a_member_code() {
    // Spike §8 / PRD §16.3 / spec §6.13: non-member events map to not_a_member.
    let content = guide();
    assert!(
        content.contains("not_a_member"),
        "troubleshooting section must document the 'not_a_member' reason code (spike §8)"
    );
}

// ── Availability model — additional bullets ───────────────────────────────────

#[test]
fn availability_model_covers_file_requires_provider_online() {
    // PRD §14 bullet 2 / spec §6.12 #2: files are fetchable only when a peer holding the
    // file is online.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        (lower.contains("only") && lower.contains("online") && lower.contains("file"))
            || lower.contains("provider is online")
            || lower.contains("peer holding"),
        "availability model must state files are fetchable only when a provider peer is online \
         (PRD §14 bullet 2)"
    );
}

// ── Timing targets (PRD §17.2) ────────────────────────────────────────────────

#[test]
fn guide_documents_dx_timing_targets() {
    // PRD §17.2 / spec §6.1: first identity < 1 min, two-peer room < 3 min, pipe < 5 min.
    let content = guide();
    // The guide must name the timing targets; check for the distinctive minute values.
    assert!(
        (content.contains("1 minute") || content.contains("< 1"))
            && (content.contains("3 minute") || content.contains("< 3")),
        "guide must document PRD §17.2 DX timing targets (identity < 1 min, room < 3 min)"
    );
}

// ── Three-participant isolation ───────────────────────────────────────────────

#[test]
fn guide_documents_demo_agent_directory() {
    // Spec §5.1: the demo requires three directories — alice, bob, AND agent.
    // guide_uses_three_isolated_demo_directories only asserts alice and bob; this
    // asserts the third (agent) so all three participants are covered.
    let content = guide();
    assert!(
        content.contains(".demo/agent"),
        "guide must document the .demo/agent data directory for the agent participant (spec §5.1)"
    );
}

#[test]
fn guide_cleanup_removes_agent_directory() {
    // Spec §6.14 / test plan: reset must include every directory created for the demo,
    // including .demo/agent, so the demo is fully repeatable.
    let content = guide();
    assert!(
        content.contains(".demo/agent"),
        "guide reset/cleanup section must remove .demo/agent to leave a clean state"
    );
}

// ── MVP command surface ───────────────────────────────────────────────────────

#[test]
fn guide_documents_identity_show_command() {
    // Step 1: `identity show` is how participants copy <BOB_ID> and <AGENT_ID>
    // for later use; it belongs in the MVP flow alongside `identity create`.
    let content = guide();
    assert!(
        content.contains("identity show"),
        "guide must document `identity show` so participants can obtain their identity key"
    );
}

#[test]
fn guide_documents_agent_invite_command() {
    // Step 3 / spec §6.6: the agent must be invited via `agent invite`; it cannot
    // self-join without an explicit key-bound invite (spike §3.5).
    let content = guide();
    assert!(
        content.contains("agent invite"),
        "guide must document `agent invite` for explicitly inviting the agent (spec §6.6)"
    );
}

#[test]
fn guide_documents_pipe_list_command() {
    // Spec §6.9 / placeholder table: `pipe list` is how Bob discovers the pipe id if
    // he did not capture it from `pipe expose` output.
    let content = guide();
    assert!(
        content.contains("pipe list"),
        "guide must document `pipe list` so peers can discover active pipe ids (spec §6.9)"
    );
}

// ── Invite flags ──────────────────────────────────────────────────────────────

#[test]
fn guide_documents_invite_expires_flag() {
    // Spec §6.6: invites carry an expiry; the guide must show `--expires` so developers
    // know to set a finite lifetime and reduce the window for ticket abuse.
    let content = guide();
    assert!(
        content.contains("--expires"),
        "guide must show the --expires flag on room invite (spec §6.6)"
    );
}

// ── No-central-account claim ──────────────────────────────────────────────────

#[test]
fn guide_states_no_central_account_required() {
    // PRD §15.1 / spec §6.3: identity is created locally; the guide must confirm that
    // no central account or registration is required.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("no central account"),
        "guide must state that no central account is required for identity creation (PRD §15.1)"
    );
}

// ── Pipe security ─────────────────────────────────────────────────────────────

#[test]
fn guide_documents_pipe_expose_tcp_flag() {
    // PRD §16.2 / spec §6.9: `pipe expose` requires `--tcp <host:port>` to name the
    // local service being forwarded; the guide must show this flag.
    let content = guide();
    assert!(
        content.contains("--tcp"),
        "guide must document the --tcp flag for pipe expose (PRD §16.2)"
    );
}

// ── Timing targets — pipe ─────────────────────────────────────────────────────

#[test]
fn guide_documents_pipe_dx_timing_target() {
    // PRD §17.2 DX metrics: three timing milestones — identity < 1 min, room < 3 min,
    // pipe < 5 min.  guide_documents_dx_timing_targets only asserts the first two;
    // this asserts the third.
    let content = guide();
    assert!(
        content.contains("< 5") || content.contains("5 minute"),
        "guide must document the PRD §17.2 pipe timing target (< 5 minutes)"
    );
}

// ── Message delivery timing ───────────────────────────────────────────────────

#[test]
fn guide_documents_message_delivery_timing() {
    // PRD §17.1.3: messages must be delivered in < 2 s when both peers are online.
    // The guide must state this target so developers know what "good" latency looks like.
    let content = guide();
    assert!(
        content.contains("< 2 s") || content.contains("< 2s") || content.contains("2 second"),
        "guide must document the PRD §17.1.3 message delivery timing target (< 2 s)"
    );
}

// ── Ticket revocation policy ──────────────────────────────────────────────────

#[test]
fn guide_documents_no_native_ticket_revocation() {
    // Spec §6.6 / spike §6 "MVP limitations": native ticket revocation is not supported;
    // the only mitigation is removing the subject.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("no native revocation")
            || lower.contains("native revocation is not")
            || (lower.contains("revocation") && lower.contains("not")),
        "guide must document that native ticket revocation is not supported (spike §6 MVP \
         limitations)"
    );
}

// ── Pipe connect local binding ────────────────────────────────────────────────

#[test]
fn guide_documents_pipe_connect_local_flag() {
    // Spec §6.9 / PRD §16.2: Bob uses `pipe connect --local <port>` to bind a local
    // port for the forwarded connection; without this flag the step is incomplete and
    // the `curl` verification command would have no target.
    let content = guide();
    assert!(
        content.contains("--local"),
        "guide must document the --local flag for `pipe connect` so Bob can bind a local port \
         (spec §6.9)"
    );
}

// ── BLAKE3 / content-addressed verification ───────────────────────────────────

#[test]
fn guide_documents_blake3_hash_verification() {
    // Spec §6.8 / spike §5 blob gate: the file fetch step must mention BLAKE3 (the
    // hash algorithm used for content-addressed verification) so the developer knows
    // the integrity check is cryptographic, not just a byte count.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("blake3"),
        "guide must mention BLAKE3 in the file share/fetch step to confirm \
         cryptographic content verification (spec §6.8 / spike §5)"
    );
}

// ── Build prerequisite ────────────────────────────────────────────────────────

#[test]
fn guide_includes_build_command() {
    // Spec §6.2: the guide must include `cargo build --release` so a developer
    // arriving at a fresh checkout can produce the binary before any other step.
    let content = guide();
    assert!(
        content.contains("cargo build --release"),
        "guide must document `cargo build --release` in its prerequisites section (spec §6.2)"
    );
}

// ── Self-contained local service for pipe step ────────────────────────────────

#[test]
fn guide_documents_local_service_for_pipe_step() {
    // Spec §6.9: the pipe expose step must provide a self-contained local service
    // the reader can start with no extra install (e.g. `python3 -m http.server` or
    // an `nc` fallback) so the demo does not depend on the developer already running
    // a service.
    let content = guide();
    assert!(
        content.contains("http.server") || content.contains("python3") || content.contains("nc -l"),
        "guide must show a self-contained local service command for the pipe demo (spec §6.9)"
    );
}

// ── Pipe auto-close on process exit ──────────────────────────────────────────

#[test]
fn guide_documents_pipe_auto_close_on_process_exit() {
    // PRD §13.2 / spec §6.9: pipes also close automatically when the owner process
    // exits; the guide must state this so developers know explicit `pipe close` is
    // not the only clean-up path and pipes don't linger after crashes.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("process exits") || lower.contains("process exit"),
        "guide must state that a pipe closes automatically when the owner process exits \
         (PRD §13.2)"
    );
}

// ── Cleanup warns about permanent data deletion ───────────────────────────────

#[test]
fn guide_cleanup_warns_about_identity_deletion() {
    // Spec §6.14 / test plan: the cleanup section must warn that removing `.demo/*`
    // permanently deletes local identities and room history because Iroh Rooms is
    // local-first — there is no server copy to restore from.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("delet") && lower.contains("identit"),
        "guide cleanup section must warn that deleting .demo/* removes local identities and \
         room history (no server copy — spec §6.14)"
    );
}

// ── Pipe expose targets loopback ──────────────────────────────────────────────

#[test]
fn guide_pipe_expose_uses_loopback_target() {
    // PRD §16.2 / spec §6.9: the canonical pipe expose command must bind to loopback
    // (`localhost`) — this is both the security default and the expected demo setup.
    // The guide must show this so readers understand the local service is not exposed
    // on a public interface.
    let content = guide();
    assert!(
        content.contains("localhost"),
        "guide must show `localhost` as the pipe expose target to confirm loopback bind \
         (PRD §16.2 / spec §6.9)"
    );
}

// ── Availability model — message delivery bullet ──────────────────────────────

#[test]
fn availability_model_covers_message_delivery_condition() {
    // PRD §14 bullet 1 / spec §6.12 #1: the availability model must state when
    // messages are delivered (peers online / reconnect through available peers).
    // Other bullets (files, pipes, no inbox, no server) are tested separately;
    // this covers the first bullet.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("message") && lower.contains("online"),
        "availability model must describe the condition under which messages are delivered \
         (PRD §14 bullet 1)"
    );
}

// ── Next steps references ─────────────────────────────────────────────────────

#[test]
fn guide_references_contributing_md() {
    // Spec §6.15: the next steps / references section must link to CONTRIBUTING.md
    // so contributors can find the workflow, branch-naming rules, and the
    // `scripts/verify.sh` quality gate.
    let content = guide();
    assert!(
        content.contains("CONTRIBUTING.md"),
        "guide must reference CONTRIBUTING.md in its next steps / references section \
         (spec §6.15)"
    );
}

#[test]
fn guide_references_phase_0_spike() {
    // Spec §6.15: the next steps section must link to PHASE-0-SPIKE.md so developers
    // can trace the protocol design (identity, keys, pipe/blob auth, invite capabilities,
    // rejection taxonomy) that underpins the demo steps.
    let content = guide();
    assert!(
        content.contains("PHASE-0-SPIKE.md"),
        "guide must reference PHASE-0-SPIKE.md in its next steps / references section \
         (spec §6.15)"
    );
}

// ── Single-host canonical path ────────────────────────────────────────────────

#[test]
fn guide_marks_two_machine_path_as_optional() {
    // Spec §5.2 / §3 (out of scope): multi-machine is explicitly out of scope as the
    // primary demo path.  The canonical demo runs on a single machine.  The guide may
    // mention two-machine as a variant but must not present it as required.
    let content = guide();
    let lower = content.to_lowercase();
    // The two-machine section should be labelled optional or as a variant.
    // Check for the presence of "optional" near "two" or "machine".
    let has_optional = lower.contains("optional");
    let has_two_machine = lower.contains("two-machine") || lower.contains("two machine");
    // Either it never mentions two-machine (it's truly out of scope) or it clearly
    // marks it as optional.
    assert!(
        !has_two_machine || has_optional,
        "if the guide mentions a two-machine path it must mark it as optional, not as the \
         primary demo (spec §5.2)"
    );
}

// ── Expected output blocks ────────────────────────────────────────────────────

#[test]
fn guide_has_expected_output_blocks_for_each_step() {
    // Spec §6.4–§6.10 (MUST): every major demo step must contain a labelled
    // "Expected output" block so the reader can verify they are on the right track.
    // Seven steps, at least one block each → minimum 7 occurrences.
    let content = guide();
    let count = content.matches("Expected output").count();
    assert!(
        count >= 7,
        "guide must include at least one 'Expected output' block per major step \
         (7 steps); found {count} occurrence(s) — spec §6.4–§6.10"
    );
}

// ── "What this proves" framing ────────────────────────────────────────────────

#[test]
fn guide_frames_each_step_with_what_this_proves() {
    // Spec §6.4–§6.10 (MUST): every step must end with a "What this proves / verify"
    // paragraph tying the outcome back to a PRD acceptance criterion.
    // Seven numbered steps → minimum 7 occurrences.
    let content = guide();
    let count = content.matches("What this proves").count();
    assert!(
        count >= 7,
        "guide must include a 'What this proves / verify' block per step \
         (7 required); found {count} occurrence(s) — spec §6.4–§6.10"
    );
}

// ── Terminal labels ───────────────────────────────────────────────────────────

#[test]
fn guide_labels_steps_with_terminal_identifiers() {
    // Spec §6.3 / §5.1: three-participant single-host demo requires per-terminal labels
    // so readers know which shell runs each command.
    let content = guide();
    assert!(
        content.contains("Terminal A")
            && content.contains("Terminal B")
            && content.contains("Terminal C"),
        "guide must label commands with 'Terminal A', 'Terminal B', 'Terminal C' \
         for the three-participant single-host demo (spec §6.3 / §5.1)"
    );
}

// ── Placeholder verbatim-paste warning ───────────────────────────────────────

#[test]
fn guide_warns_not_to_paste_placeholder_values_verbatim() {
    // Spec §5.3 / §7.2: placeholders are host-specific values the reader must produce
    // from their own command output — never copy from the guide as if they were generic.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("never paste")
            || lower.contains("do not paste")
            || lower.contains("produce your own"),
        "guide must warn readers not to paste placeholder values verbatim; \
         each must be produced from the reader's own command output (spec §5.3 / §7.2)"
    );
}

// ── PRD traceability ──────────────────────────────────────────────────────────

#[test]
fn guide_references_prd() {
    // Issue #35 traceability: the guide is drafted against PRD.v0.3.md §17.2 (DX metrics)
    // and §19 (Phase 1B).  A reference must appear so developers can trace acceptance
    // criteria back to the product requirements.
    let content = guide();
    assert!(
        content.contains("PRD.v0.3.md") || content.contains("PRD §"),
        "guide must reference PRD.v0.3.md (issue #35 traceability to §17.2 DX Metrics)"
    );
}

// ── Agent ticket used in join context ────────────────────────────────────────

#[test]
fn guide_uses_agent_ticket_in_room_join_command() {
    // Spec §6.6: after `agent invite` the agent must run `room join <AGENT_TICKET>`.
    // Verify the placeholder is used as a `room join` argument, not only in the legend.
    let content = guide();
    assert!(
        content.contains("room join <AGENT_TICKET>"),
        "guide must show `room join <AGENT_TICKET>` so the agent knows how to join \
         the room after receiving an invite (spec §6.6)"
    );
}

// ── Draft / status warning ────────────────────────────────────────────────────

#[test]
fn guide_includes_status_section_with_scaffold_disclaimer() {
    // The guide is drafted ahead of the CLI implementation (the binary is a scaffold).
    // It must include a prominent status/disclaimer section so readers understand that
    // commands are the intended surface and expected outputs are illustrative, not
    // captured from a live binary.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("status") && (lower.contains("scaffold") || lower.contains("illustrative")),
        "guide must include a status section disclaiming that the CLI is a scaffold \
         and expected outputs are illustrative (spec § 'Status of this guide')"
    );
}

// ── Alias command ─────────────────────────────────────────────────────────────

#[test]
fn guide_shows_alias_setup_for_binary() {
    // Spec §6.2: the guide must establish one consistent invocation convention for the
    // binary.  The preferred form is a shell alias so all commands read as `iroh-rooms …`.
    let content = guide();
    assert!(
        content.contains("alias iroh-rooms"),
        "guide must show the `alias iroh-rooms=…` setup so readers use one consistent \
         invocation convention throughout (spec §6.2)"
    );
}

// ── CLI error-taxonomy docs gate (issue #25 / IR-0110, spec §7 Step 7 / §8) ────
//
// AC4 ("codes stable enough for scripts/tests") is gated here: the README **Error
// codes** table is the documented script contract, so it must (a) exist, (b) name
// every stable code string the taxonomy emits — no undocumented codes — and (c)
// document the render contract (`error[<code>]:` / `warning[<code>]:`) and the
// category→exit scheme. This mirrors the core taxonomy-completeness gate but at the
// docs boundary (spec §8 "Docs conformance").
//
// The canonical code list below MUST stay in lockstep with the pinned `.code()`
// strings in `src/error.rs::codes_are_stable` and `src/ticket.rs::
// ticket_error_codes_are_stable`; those unit tests pin the emitter, this test pins
// the documentation of it. A new code that lands without a README row fails here.

/// Every stable `ErrorCode::code()` string the taxonomy renders (spec §5.1/§5.3),
/// grouped as in the README table. Kept identical to the strings pinned by the unit
/// tests in `src/error.rs` and `src/ticket.rs`.
const ALL_ERROR_CODES: &[&str] = &[
    // Internal (exit 1)
    "internal",
    // Usage (exit 2)
    "invalid_room_id",
    "invalid_argument",
    "no_such_file",
    "permission_denied",
    "file_too_large",
    "identity_not_found",
    "room_not_found",
    "no_discovery_hint",
    // Auth (exit 3): the five §8 authz rejects + two CLI-native twins
    "not_a_member",
    "unbound_device",
    "insufficient_role",
    "expired_invite",
    "bad_capability",
    "wrong_identity",
    "peer_unauthorized",
    // Integrity (exit 4): the crypto/structural §8 rejects
    "bad_signature",
    "id_mismatch",
    "non_canonical_encoding",
    "invalid_content",
    "unknown_schema_version",
    "unknown_event_type",
    "too_many_parents",
    "not_genesis_descended",
    "room_id_mismatch",
    "hash_mismatch",
    // Ticket (exit 5)
    "ticket_bad_prefix",
    "ticket_bad_base32",
    "ticket_truncated",
    "ticket_unsupported_version",
    "ticket_bad_checksum",
    "ticket_malformed",
    // Connectivity (exit 6)
    "no_admin_reachable",
    "peer_offline",
    "blob_unavailable",
];

#[test]
fn readme_has_error_codes_section() {
    // Definition of Done: a README **Error codes** table must exist.
    let content = readme();
    assert!(
        content.contains("### Error codes") || content.contains("## Error codes"),
        "README must contain an `Error codes` section documenting the CLI taxonomy (IR-0110)"
    );
}

#[test]
fn readme_documents_the_render_contract() {
    // Spec §5.2/OQ-6: the pinned `error[<code>]:` line, the `warning[<code>]:`
    // advisory line, and the uncoded-fallback + zero-exit-for-success rules are the
    // machine surface AC4 promises; they must be spelled out for script authors.
    let content = readme();
    assert!(
        content.contains("error[<code>]:"),
        "README must document the `error[<code>]: <message>` render contract"
    );
    assert!(
        content.contains("warning[<code>]:"),
        "README must document the `warning[<code>]: <message>` advisory contract"
    );
    assert!(
        content.contains("`error: <message>`") && content.contains("exits `1`"),
        "README must document the uncoded fallback (`error: <message>`, exit 1)"
    );
}

#[test]
fn readme_documents_every_error_code() {
    // AC4 no-undocumented-codes half: every stable code the CLI can emit appears in
    // the README table. A new code without a README row is a documentation drift bug.
    let content = readme();
    let mut missing: Vec<&str> = Vec::new();
    for code in ALL_ERROR_CODES {
        if !content.contains(code) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "README Error codes table is missing these emitted codes: {missing:?} \
         (add a row, or update `ALL_ERROR_CODES` if a code was intentionally removed)"
    );
}

#[test]
fn readme_documents_every_exit_category() {
    // Spec §5.3: the coarse category→exit scheme (2..=6) is the `$?` contract; each
    // named category must appear so a script author can map an exit code to a class.
    let content = readme();
    for category in &[
        "Internal",
        "Usage",
        "Auth",
        "Integrity",
        "Ticket",
        "Connectivity",
    ] {
        assert!(
            content.contains(category),
            "README Error codes table must name the `{category}` exit category (spec §5.3)"
        );
    }
    // The distinctive non-trivial exit numbers (3=Auth .. 6=Connectivity) must be
    // documented so `case $? in` branches are pinned, not guessed.
    for exit in &["`3`", "`4`", "`5`", "`6`"] {
        assert!(
            content.contains(exit),
            "README must document the exit code {exit} in the category scheme (spec §5.3)"
        );
    }
}

#[test]
fn readme_documents_blob_unavailable_as_emitted() {
    // Spec IR-0205 §5.5/§6 Step 4: `blob_unavailable` is now emitted by `file fetch`
    // (the honest no-online-provider terminal state), not merely reserved ahead of
    // that command. The table must no longer flag it reserved, or a reader would
    // wrongly believe no landed command produces it.
    let content = readme();
    assert!(
        content.contains("blob_unavailable"),
        "README must document the `blob_unavailable` code"
    );
    let lower = content.to_lowercase();
    assert!(
        !lower.contains("reserved"),
        "README Error codes section must not mark `blob_unavailable` as reserved — \
         `file fetch` (IR-0205) now emits it"
    );
}

#[test]
fn guide_documents_error_and_warning_line_contract() {
    // Spec §7 Step 7: the getting-started troubleshooting guide extends the reason-code
    // narrative with the stable `error[<code>]:` / `warning[<code>]:` line shapes and
    // the clock-skew advisory, so the guide and the binary agree.
    let content = guide();
    assert!(
        content.contains("error[<code>]:"),
        "guide must document the `error[<code>]: <message>` line shape (IR-0110 §7)"
    );
    assert!(
        content.contains("warning[<code>]:"),
        "guide must document the `warning[<code>]: <message>` advisory line shape (IR-0110 §7)"
    );
    assert!(
        content.contains("clock_skew"),
        "guide must document the `warning[clock_skew]` advisory (spec §5.9)"
    );
}

#[test]
fn guide_states_zero_peer_send_is_success_not_failure() {
    // Spec §5.5 / regression (test #16): the taxonomy must NOT turn a `room send` that
    // reaches zero peers into an error; the guide must keep stating it exits `0`.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("room send") && lower.contains("zero") && lower.contains("exits `0`"),
        "guide must state that `room send` reaching zero peers exits `0` (availability, \
         not failure; spec §5.5)"
    );
}

// ── IR-0303 (issue #38): next-action reference + verbose diagnostics docs gate ──
//
// AC1/AC2/AC4 add a *human* half to the taxonomy docs the #25 gates above do not
// cover: a per-code **Next action** reference in the README (the counterpart to
// `ErrorCode::next_action()`), the additive `next: <action>` render-line contract,
// and a **Verbose network diagnostics** section documenting the opt-in `--verbose`
// / `diag:` block. These gates keep the docs in lockstep with the emitter so a
// script/human reader can trust the reference (spec §5.2/§5.5, Step 6).

/// The distinctive next-action phrase each user-actionable code carries in the
/// README reference table, kept identical to the `&'static str` templates pinned by
/// `src/error.rs::next_action`. A missing row (or drifted wording) breaks the gate.
/// Structural/crypto codes and the context-only codes (`internal`/`invalid_argument`)
/// have no next action, so they are intentionally absent.
const NEXT_ACTION_PHRASES: &[(&str, &str)] = &[
    ("identity_not_found", "identity create"),
    ("invalid_room_id", "copy the room id"),
    ("room_not_found", "join an invite ticket first"),
    ("no_such_file", "check the path"),
    ("permission_denied", "read permissions"),
    ("file_too_large", "split or compress"),
    ("no_discovery_hint", "--peer <admin-addr>"),
    ("not_a_member", "complete `room join` first"),
    ("insufficient_role", "with the intended role"),
    ("expired_invite", "fresh `room invite`"),
    ("bad_capability", "re-issue the invite"),
    ("wrong_identity", "identity show"),
    ("peer_unauthorized", "membership has synced"),
    ("hash_mismatch", "do not trust this file"),
    ("no_admin_reachable", "--accept-joins"),
    ("peer_offline", "come online"),
    ("blob_unavailable", "retry `file fetch`"),
];

#[test]
fn readme_error_code_reference_names_a_next_action_per_actionable_code() {
    // AC1/AC2 (spec §5.2): the README carries a per-code reference table with a
    // **Next action** column, and every user-actionable code names its concrete
    // step there — the documented counterpart to the emitter.
    let content = readme();
    assert!(
        content.contains("| Code | Category | Exit | Meaning | Next action |"),
        "README must carry the IR-0303 per-code reference table with a `Next action` column"
    );
    let mut missing: Vec<&str> = Vec::new();
    for (code, phrase) in NEXT_ACTION_PHRASES {
        // Each code must have a documented next action naming the concrete step.
        if !content.contains(code) || !content.contains(phrase) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "README reference is missing a documented next action for: {missing:?} \
         (add/repair the row, or update NEXT_ACTION_PHRASES if the template changed)"
    );
}

#[test]
fn readme_documents_the_next_action_render_contract() {
    // AC2: the `next:` line is documented as *additive* — it never replaces the
    // machine `error[<code>]:` line, so a script matching `^error\[` or branching on
    // `$?` is unaffected. Pin that the README spells this out (spec §5.1 OQ-1).
    let content = readme();
    assert!(
        content.contains("next: <action>"),
        "README must document the `next: <action>` render line (IR-0303 §5.1)"
    );
    assert!(
        content.contains("additive"),
        "README must state the `next:` line is additive (does not change the machine surface)"
    );
}

#[test]
fn readme_and_guide_document_verbose_diagnostics_vocabulary() {
    // AC4 (spec §5.3/§5.5, Step 6c): both docs must document the opt-in `--verbose`
    // diagnostics surface with the exact greppable vocabulary the CLI renders — the
    // `diag:` prefix and the four `path=` classifications — so a reader can map the
    // output. Asserted over README and guide together.
    for (name, content) in [("README.md", readme()), ("getting-started.md", guide())] {
        for token in ["--verbose", "diag:", "path=direct", "relay="] {
            assert!(
                content.contains(token),
                "{name} must document the verbose-diagnostics token `{token}` (IR-0303 §5.3)"
            );
        }
        // The four path classifications must all be named so their meaning is documented.
        for label in ["direct", "relay", "mixed", "none"] {
            assert!(
                content.contains(label),
                "{name} must document the `{label}` path classification (IR-0303 §5.3)"
            );
        }
    }
}

#[test]
fn guide_has_a_verbose_network_diagnostics_section() {
    // AC4: the getting-started guide must carry a dedicated Verbose network
    // diagnostics subsection (spec §5.5) showing how to run it and reading the block.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("verbose network diagnostics"),
        "guide must contain a `Verbose network diagnostics` section (IR-0303 §5.5)"
    );
    assert!(
        content.contains("room members <ROOM_ID> --status --verbose")
            || content.contains("--status --verbose"),
        "guide must show how to run the verbose status command (IR-0303 §5.3)"
    );
}

#[test]
fn guide_states_verbose_diagnostics_leak_no_secrets_and_grant_no_trust() {
    // AC3 (docs half): the verbose section must state the `diag:` block carries no
    // secret (private key / ticket secret / payload) and is diagnostic-only — never
    // an authorization input (spec §5.4/§9). The no-leak invariant is enforced in
    // code by the e2e no-leak test; here we gate that the docs make the promise.
    let content = guide();
    let lower = content.to_lowercase();
    assert!(
        lower.contains("never contain a private key")
            || (lower.contains("diag:") && lower.contains("secret")),
        "guide's verbose section must state the diag: block leaks no private key/secret (AC3)"
    );
    assert!(
        lower.contains("never feeds an authorization decision")
            || lower.contains("diagnostic only")
            || lower.contains("never a trust input"),
        "guide must state the diagnostics are advisory only, never an authorization input"
    );
}
