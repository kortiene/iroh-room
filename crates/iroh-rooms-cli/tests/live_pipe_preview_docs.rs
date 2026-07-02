//! Structural + drift-guard conformance tests for `docs/live-pipe-preview.md` (IR-0305).
//!
//! These tests are deterministic and require no network, no binary execution, and no
//! external services. They read the Markdown guide (and the CLI/net *source* it quotes)
//! and assert two things:
//!
//! 1. **Acceptance-criteria coverage** — the guide documents the expose/connect/close
//!    flow, the authorized-preview proof, the two unauthorized cases, the availability +
//!    relay-fallback honesty, the neutral tunnel comparison, and the agent scenario
//!    (issue #40 ACs / spec §4, §8).
//! 2. **Doc/code drift guard** — every exact CLI output line, validation/error message,
//!    audit line, deny-cause code, and error-taxonomy code the guide quotes appears
//!    *verbatim in the emitting source*. This mechanizes the spec's "code wins" rule
//!    (spec §6 / Risk #1): if someone edits an output string in `pipe.rs` (or a code in
//!    `error.rs` / `audit.rs`) without updating the guide, or drops a documented line
//!    from the guide, this test fails and forces the two back into agreement.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/iroh-rooms-cli; workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{rel} must exist at {}", path.display()))
}

fn guide() -> String {
    read("docs/live-pipe-preview.md")
}

/// Collapse every run of whitespace (spaces, tabs, newlines) to a single space so a
/// `contains` check ignores Markdown line-wrapping and source indentation. Fragments
/// used against squished text must be single-spaced and must not straddle a Rust
/// backslash-newline string continuation in the source.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// GitHub-renderer heading-anchor slug: lowercase, drop every char that is not an
/// ASCII alphanumeric / hyphen / underscore, and map each space to a hyphen. Because
/// punctuation is *removed* while its surrounding spaces stay, a run like `" — "` or
/// `" & "` collapses to `--` — which is exactly why the guide links to
/// `#step-1--create-identities` and `#security-warning--close-flow-deep-dive`. This
/// matches GitHub for the ASCII+em-dash headings these two guides use, so a computed
/// slug equals the fragment a `[...](#slug)` link must target.
fn slugify(heading: &str) -> String {
    let mut out = String::new();
    for ch in heading.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch == ' ' {
            out.push('-');
        }
        // Any other char (`.`, `/`, `&`, `(`, `)`, the em-dash, …) is dropped.
    }
    out
}

/// The set of anchor slugs an ATX heading in `content` would expose, skipping fenced
/// code blocks so a shell comment like `# Terminal A — Alice` is never mistaken for a
/// heading.
fn heading_slugs(content: &str) -> std::collections::HashSet<String> {
    let mut slugs = std::collections::HashSet::new();
    let mut in_fence = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('#') {
            continue;
        }
        let hashes = line.chars().take_while(|&c| c == '#').count();
        let rest = &line[hashes..];
        if (1..=6).contains(&hashes) && rest.starts_with(' ') {
            slugs.insert(slugify(rest));
        }
    }
    slugs
}

/// Every markdown link *target* (the text between `](` and the next `)`), in document
/// order. These guides use no titled links, so the whole parenthesized run is the URL.
fn link_targets(content: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut i = 0;
    while let Some(pos) = content[i..].find("](") {
        let start = i + pos + 2;
        match content[start..].find(')') {
            Some(end) => {
                targets.push(content[start..start + end].to_string());
                i = start + end + 1;
            }
            None => break,
        }
    }
    targets
}

// ── Deliverable + discoverability (spec §4, §5) ───────────────────────────────

#[test]
fn guide_file_exists() {
    assert!(
        workspace_root().join("docs/live-pipe-preview.md").exists(),
        "the IR-0305 guide must exist at docs/live-pipe-preview.md (spec §4)"
    );
}

#[test]
fn readme_links_to_the_guide() {
    // spec §5.2: the README docs index must advertise the guide.
    assert!(
        read("README.md").contains("docs/live-pipe-preview.md"),
        "README.md must link to docs/live-pipe-preview.md (spec §5.2)"
    );
}

#[test]
fn getting_started_links_to_the_guide() {
    // spec §5.1: getting-started.md must cross-link the standalone guide (Step 6 pointer
    // and/or the "Next steps" list) so the guide is discoverable from the demo.
    assert!(
        read("docs/getting-started.md").contains("live-pipe-preview.md"),
        "docs/getting-started.md must cross-link live-pipe-preview.md (spec §5.1)"
    );
}

// ── AC1: expose → connect → close flow shown clearly (spec §4.4) ───────────────

#[test]
fn guide_shows_expose_connect_close_flow() {
    let content = guide();
    for cmd in &["pipe expose", "pipe connect", "pipe close", "pipe list"] {
        assert!(
            content.contains(cmd),
            "guide must document the `{cmd}` command (AC1 / spec §4.4)"
        );
    }
    // The step headings must appear in narrative order (expose → connect → close), not
    // merely be present, so the "flow shown clearly" AC is real. Anchor on the A-step
    // titles rather than the bare command names, which also occur in earlier prose (e.g.
    // the `pipe connect` --peer hint in Prerequisites).
    let expose = content
        .find("Expose it to one reviewer")
        .expect("A2 expose heading present");
    let connect = content
        .find("Connect and view the preview")
        .expect("A3 connect heading present");
    let close = content
        .find("Close the pipe")
        .expect("A4 close heading present");
    assert!(
        expose < connect && connect < close,
        "guide must present the flow in expose → connect → close order (AC1)"
    );
}

// ── AC2: an authorized peer can view a local preview (spec §4.4 A3) ────────────

#[test]
fn guide_proves_authorized_peer_can_view_preview() {
    // The concrete proof is a `curl` against the forwarded loopback port that returns the
    // presenter's content, plus the connector's live forwarding status line.
    let content = guide();
    assert!(
        content.contains("curl") && content.contains("localhost:3001"),
        "guide must show `curl http://localhost:3001` as the authorized-view proof (AC2 / §4.4 A3)"
    );
    assert!(
        content.contains("[pipe] connection forwarding"),
        "guide must show the connector's live forwarding status line as the view proof (AC2)"
    );
}

// ── AC3: unauthorized access behavior documented — both distinct cases (§4.7) ──

#[test]
fn guide_documents_member_not_allowed_case() {
    // Case 1: a room member absent from --allow is rejected at connect time by the owner
    // gate, surfaced as a `pipe.connect.rejected:not_allowed` audit line.
    let content = guide();
    assert!(
        content.contains("pipe.connect.rejected:not_allowed"),
        "guide must document the member-not-allowed reject line (AC3 / §4.7 case 1)"
    );
}

#[test]
fn guide_documents_non_member_case_and_exit_3() {
    // Case 2: a non-member is turned away locally before any dial with peer_unauthorized,
    // exit 3.
    let content = squish(&guide());
    assert!(
        content.contains("error[peer_unauthorized]"),
        "guide must document the non-member `error[peer_unauthorized]` rejection (AC3 / §4.7 case 2)"
    );
    assert!(
        content.contains("`peer_unauthorized` = `3`"),
        "guide must state peer_unauthorized maps to exit 3 (AC3 / §4.11)"
    );
}

// ── AC4: availability + relay-fallback stated honestly (spec §4.8) ─────────────

#[test]
fn guide_states_both_peers_must_be_online() {
    let lower = squish(&guide()).to_lowercase();
    assert!(
        lower.contains("both peers must be online"),
        "guide must state a pipe needs both peers online (AC4 / §4.8)"
    );
}

#[test]
fn guide_states_relay_fallback_honestly() {
    let lower = squish(&guide()).to_lowercase();
    assert!(
        lower.contains("relay fallback") || lower.contains("fall back to a relay"),
        "guide must state P2P with relay fallback (AC4 / §4.8)"
    );
    assert!(
        lower.contains("no cloud inbox") || lower.contains("nothing is queued"),
        "guide must state there is no queued/offline delivery (AC4 / §4.8)"
    );
}

#[test]
fn guide_documents_peer_offline_exit_6() {
    let content = squish(&guide());
    assert!(
        content.contains("error[peer_offline]"),
        "guide must document `error[peer_offline]` for an unreachable owner (AC4 / §4.8)"
    );
    assert!(
        content.contains("`peer_offline` = `6`"),
        "guide must state peer_offline maps to exit 6 (AC4 / §4.11)"
    );
}

// ── Scope: neutral public-tunnel comparison (spec §4.9) ───────────────────────

#[test]
fn guide_has_public_tunnel_comparison_table() {
    let content = guide();
    assert!(
        content.contains("| Dimension |") && content.contains("Public tunnel"),
        "guide must include the public-tunnel comparison table (scope / §4.9)"
    );
}

#[test]
fn guide_comparison_names_no_specific_vendors() {
    // spec §4.9 hard constraint / Risk #2: compare the *category*, never a named product,
    // and never disparage. A vendor name creeping in is a documented regression risk.
    let lower = guide().to_lowercase();
    for vendor in &[
        "ngrok",
        "cloudflare",
        "tailscale",
        "localtunnel",
        "serveo",
        "pagekite",
    ] {
        assert!(
            !lower.contains(vendor),
            "comparison must stay vendor-neutral; found a specific product name: {vendor:?} \
             (spec §4.9 / Risk #2)"
        );
    }
}

// ── Scope: agent-generated preview, documented honestly (spec §4.5) ───────────

#[test]
fn guide_documents_agent_scenario() {
    let content = guide();
    assert!(
        content.contains("Scenario B") && content.contains("--label agent-preview"),
        "guide must include the agent-generated preview scenario (scope / §4.5)"
    );
}

#[test]
fn guide_is_honest_that_no_example_agent_binary_ships() {
    // spec §4.5 / Risk #3: the guide must not imply a tool that does not exist — it must
    // state plainly that no example-agent binary ships and the scenario reuses the CLI.
    let lower = guide().to_lowercase();
    assert!(
        lower.contains("no dedicated example-agent binary")
            || lower.contains("ships no turnkey")
            || lower.contains("no example-agent binary"),
        "guide must state honestly that no example-agent binary ships (§4.5 / Risk #3)"
    );
}

#[test]
fn guide_agent_scenario_states_explicit_invite_gate() {
    // spec §4.5: the agent may open a pipe only because it was explicitly invited and is an
    // active member — first-class, not implicitly trusted.
    let lower = squish(&guide()).to_lowercase();
    assert!(
        lower.contains("explicitly invited") && lower.contains("active member"),
        "guide must state the agent's pipe access requires an explicit invite (§4.5)"
    );
}

// ── Scope: security warning + close-flow deep dive (spec §4.6) ─────────────────

#[test]
fn guide_documents_stderr_stdout_split() {
    // The ⚠ SECURITY warning goes to stderr, the summary to stdout, so the trust decision
    // survives stdout redirection — a documented property the guide must call out.
    let lower = guide().to_lowercase();
    assert!(
        lower.contains("stderr") && lower.contains("stdout"),
        "guide must document the stderr/stdout split of the expose output (§4.2 / §4.6)"
    );
}

#[test]
fn guide_documents_three_close_paths() {
    // Explicit close, owner-exit, and the SIGKILL/power-loss reachability bound.
    let content = guide();
    let lower = squish(&content).to_lowercase();
    assert!(
        content.contains("pipe.closed{closed}") && content.contains("pipe.closed{owner_exit}"),
        "guide must document the explicit-close and owner-exit close events (§4.4 A4 / §4.6)"
    );
    assert!(
        lower.contains("sigkill") && lower.contains("power loss"),
        "guide must document the hard-kill/power-loss reachability bound (§4.4 A4 / §4.8)"
    );
}

// ── Drift guard: exact CLI output lines the guide quotes still exist in pipe.rs ─
//
// Each anchor is a stable literal fragment that must appear (after whitespace squishing)
// in BOTH the guide's expected-output blocks AND `crates/iroh-rooms-cli/src/pipe.rs`. A
// fragment is chosen so it is single-spaced and does not straddle a `\`-continuation in
// the source. If either side drifts, this fails — enforcing spec §6 "code wins".
const PIPE_OUTPUT_ANCHORS: &[&str] = &[
    // expose — warning + summary + next-step hints
    "SECURITY: exposing",
    "allowed member(s):",
    "through this pipe while it is open.",
    "tip: share this address with connectors via --peer",
    "connectors run: iroh-rooms pipe connect",
    "close it with: iroh-rooms pipe close",
    "serving the pipe; press Ctrl-C to close it...",
    // connect — forwarding banner + live status lines
    "-> pipe",
    "connect your client to",
    "; press Ctrl-C to stop.",
    "[pipe] connection forwarding",
    "[pipe] denied by the owner (not authorized / closed)",
    // close + list
    "closed pipe",
    "(no open pipes)",
    // pre-IO validation refusals
    "--allow <IDENTITY_ID> (no default-all; PRD §13.2)",
    "refusing to expose non-loopback target",
    "loopback address (127.0.0.0/8 or ::1)",
    // owner-side audit lines
    "pipe.connect.rejected:",
    "pipe.torndown:",
    // coded-error messages surfaced in the guide
    "you are not an active member of room",
    "the pipe owner is unreachable:",
];

#[test]
fn guide_output_lines_match_pipe_source() {
    let guide = squish(&guide());
    let source = squish(&read("crates/iroh-rooms-cli/src/pipe.rs"));
    for &anchor in PIPE_OUTPUT_ANCHORS {
        assert!(
            source.contains(anchor),
            "pipe.rs no longer emits {anchor:?} — the guide quotes it; update the guide \
             (code wins, spec §6) or fix this anchor"
        );
        assert!(
            guide.contains(anchor),
            "the guide dropped the CLI output {anchor:?} that pipe.rs still emits; the guide \
             must document real output (spec §4.4 / §7.1)"
        );
    }
}

// ── Drift guard: deny-cause vocabulary matches the net audit enum ──────────────

const DENY_CAUSES: &[&str] = &[
    "not_allowed",
    "not_active",
    "closed",
    "expired",
    "unknown_device",
    "owner_inactive",
];

#[test]
fn guide_deny_causes_match_net_audit_source() {
    // The guide lists the owner-side reject/teardown causes; each must be a real code the
    // `PipeDenyCause::code()` mapping still emits (crates/iroh-rooms-net/src/pipe/audit.rs).
    let guide = guide();
    let audit_src = read("crates/iroh-rooms-net/src/pipe/audit.rs");
    for &cause in DENY_CAUSES {
        assert!(
            audit_src.contains(&format!("\"{cause}\"")),
            "PipeDenyCause no longer defines the code {cause:?}; the guide documents it (§4.6/§4.7)"
        );
        assert!(
            guide.contains(cause),
            "guide must document the deny cause {cause:?} that audit.rs emits (§4.6/§4.7)"
        );
    }
    // The less-obvious causes must be surfaced as an explicit, backticked list (not just
    // incidentally present as an English word), so the operator can recognize them.
    for cause in &["not_active", "unknown_device", "owner_inactive"] {
        assert!(
            guide.contains(&format!("`{cause}`")),
            "guide must name the reject cause `{cause}` explicitly in its cause list (§4.7)"
        );
    }
}

// ── Drift guard: pipe-relevant error-taxonomy codes match error.rs ────────────

#[test]
fn guide_error_codes_match_taxonomy_source() {
    // The guide renders `error[peer_unauthorized]` / `error[peer_offline]`; those code
    // strings must still be the ones ErrorCode::code() returns (src/error.rs).
    let guide = guide();
    let error_src = read("crates/iroh-rooms-cli/src/error.rs");
    for code in &["peer_unauthorized", "peer_offline"] {
        assert!(
            error_src.contains(&format!("\"{code}\"")),
            "ErrorCode::code() no longer emits {code:?}; the guide renders error[{code}] (§4.7/§4.8)"
        );
        assert!(
            guide.contains(&format!("error[{code}]")),
            "guide must render the coded line error[{code}] that the taxonomy emits (§4.7/§4.8)"
        );
    }
}

// ── Link integrity: in-page anchors resolve to real headings (spec §5.3) ───────
//
// The guide is heavily cross-linked (jump-to-step, jump-to-honesty-box). A link to a
// heading that was renamed — in this file or, below, in getting-started.md — renders
// as a dead click on GitHub with no build error to catch it. These two tests compute
// the GitHub anchor slug for every heading and assert every link target still hits one.

#[test]
fn guide_internal_anchor_links_resolve() {
    let content = guide();
    let slugs = heading_slugs(&content);
    let mut checked = 0;
    for target in link_targets(&content) {
        if let Some(anchor) = target.strip_prefix('#') {
            checked += 1;
            assert!(
                slugs.contains(anchor),
                "guide has a dangling in-page link [...](#{anchor}); no heading slugifies to it. \
                 Rename the link or restore the heading. Known slugs: {slugs:?}"
            );
        }
    }
    // Guard against the link syntax silently changing to something this test can't see:
    // the guide is known to carry several in-page jumps (§4.4 step refs, §4.8 box).
    assert!(
        checked >= 4,
        "expected the guide to contain in-page #anchor links (found {checked}); \
         did the link syntax change?"
    );
}

#[test]
fn guide_cross_links_to_getting_started_resolve() {
    // spec §3 single-source rule: the guide links *into* getting-started.md rather than
    // duplicating setup. Every `getting-started.md#anchor` it cites must be a real
    // heading there, or the "follow Steps 1–3 / see Unauthorized peer" pointers break.
    let content = guide();
    let gs_slugs = heading_slugs(&read("docs/getting-started.md"));
    let mut checked = 0;
    for target in link_targets(&content) {
        let Some((path, anchor)) = target.split_once('#') else {
            continue;
        };
        if !path.ends_with("getting-started.md") {
            continue;
        }
        checked += 1;
        assert!(
            gs_slugs.contains(anchor),
            "guide links getting-started.md#{anchor}, but that file has no heading slugifying \
             to it — a broken cross-link. Fix the guide or restore the heading (spec §3/§5)."
        );
    }
    assert!(
        checked >= 5,
        "expected several getting-started.md#anchor cross-links (found {checked}); \
         did the link syntax change?"
    );
}

// ── Negative: the hidden --loopback CI/test flag must never surface (spec §4.10) ─

#[test]
fn guide_never_surfaces_the_hidden_loopback_flag() {
    // spec §4.10: `--loopback` is a hidden CI/test flag; the operator-facing guide must
    // not surface it. Assert both sides so this stays a real invariant, not a stale
    // assumption: the flag is *still* hidden in cli.rs, AND absent from the guide. If a
    // future change un-hides the flag, this fails and forces a deliberate doc decision.
    let cli_src = squish(&read("crates/iroh-rooms-cli/src/cli.rs"));
    assert!(
        cli_src.contains("#[arg(long, hide = true)] loopback: bool"),
        "cli.rs no longer keeps --loopback hidden; reconsider whether the guide should \
         document it (spec §4.10)"
    );
    assert!(
        !guide().contains("--loopback"),
        "the guide must not surface the hidden --loopback CI/test flag (spec §4.10)"
    );
}
