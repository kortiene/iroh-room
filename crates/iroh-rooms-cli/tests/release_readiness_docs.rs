//! Structural + anti-drift conformance tests for the developer preview
//! release-readiness checklist (`RELEASE-READINESS.md`, issue #41 / IR-0306).
//!
//! These tests are deterministic and offline: no network, and the gate script
//! is only ever run along paths that terminate *before* its slow, online P0
//! tiers — `bash -n` (syntax-only, executes nothing) and its unknown-argument
//! usage path. The real exit-code wiring (AC4's actual verdict logic, faked
//! only at its two external-command boundaries) is exercised end to end by
//! `release_readiness_e2e.rs`, never here. They verify:
//!
//! * the checklist doc and the gate script both exist, in the expected shape;
//! * every required §6 section heading is present, in order;
//! * the "How to use" and "Sign-off" sections both tie a READY verdict to the
//!   gate script exiting `0` (AC4's mechanism, not an honor system);
//! * the doc's "P0 — gated online tiers" command table and the script's
//!   `ONLINE_TIERS` array name the exact same command set — a renamed/added
//!   online test that one side picks up but not the other fails here (the
//!   anti-drift guard called for by spec §12 / §16);
//! * the gate script is valid bash and honours its argument contract — an
//!   unknown flag is a stderr usage error with exit `2`, before any tier runs
//!   (proves the AC4 mechanism actually *runs*, not just that its text greps);
//! * the "P0 required tests" section enumerates all five AC1 areas (protocol,
//!   integration, pipe security, blob verification, agent flow);
//! * the "Known MVP limitations" section is non-empty and names the starred
//!   (Gate A, no-offline-delivery) items (AC2);
//! * the "Security warnings" section reproduces each warning spec §8 requires
//!   (the issue's type/security label);
//! * the "Release notes template" fenced block exists with its required
//!   placeholders (AC3).

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
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

fn doc() -> String {
    read("RELEASE-READINESS.md")
}

fn script() -> String {
    read("scripts/release-readiness.sh")
}

// ── Deliverables exist ───────────────────────────────────────────────────────

#[test]
fn doc_exists_and_is_non_empty() {
    let content = doc();
    assert!(
        content.len() > 500,
        "RELEASE-READINESS.md must exist and be non-trivial (D1)"
    );
}

#[test]
fn script_exists_and_is_executable() {
    let path = workspace_root().join("scripts/release-readiness.sh");
    assert!(
        path.exists(),
        "scripts/release-readiness.sh must exist (D2)"
    );
    let mode = std::fs::metadata(&path)
        .expect("scripts/release-readiness.sh must be readable")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "scripts/release-readiness.sh must be executable (0o111 bit); got mode {mode:o}"
    );
}

// ── Required section structure — spec §6 ─────────────────────────────────────

#[test]
fn doc_contains_all_required_sections_in_order() {
    let content = doc();
    let ordered_anchors = [
        "## How to use this checklist",
        "## Candidate build",
        "## P0 required tests",
        "## Pipe security review",
        "## Blob verification review",
        "## Agent flow review",
        "## Known MVP limitations",
        "## Security warnings",
        "## Dependency / churn review",
        "## Demo verification",
        "## Release notes template",
        "## Sign-off",
    ];
    let mut last_pos = 0usize;
    let mut last_anchor = "<start>";
    for anchor in ordered_anchors {
        let pos = content.find(anchor).unwrap_or_else(|| {
            panic!("spec §6: RELEASE-READINESS.md is missing the required section {anchor:?}")
        });
        assert!(
            pos >= last_pos,
            "spec §6: section {anchor:?} appears before {last_anchor:?}; required order is \
             {ordered_anchors:?}"
        );
        last_pos = pos;
        last_anchor = anchor;
    }
}

// Slice the doc from `start_anchor` (inclusive) to the end of the file.
fn section_to_end<'a>(content: &'a str, start_anchor: &str) -> &'a str {
    let start = content
        .find(start_anchor)
        .unwrap_or_else(|| panic!("missing section {start_anchor:?}"));
    &content[start..]
}

// Slice the doc between `start_anchor` (inclusive) and the next occurrence of
// `next_anchor` after it (exclusive). Panics if either anchor is missing.
fn section<'a>(content: &'a str, start_anchor: &str, next_anchor: &str) -> &'a str {
    let rest = section_to_end(content, start_anchor);
    let end = rest[start_anchor.len()..]
        .find(next_anchor)
        .map_or(rest.len(), |p| p + start_anchor.len());
    &rest[..end]
}

// ── AC4 — READY is a gate-script exit code, not a checkbox ──────────────────

#[test]
fn how_to_use_and_sign_off_tie_ready_to_the_gate_script_exit_code() {
    let content = doc();
    let how_to_use = section(
        &content,
        "## How to use this checklist",
        "## Candidate build",
    );
    let sign_off = section_to_end(&content, "## Sign-off");

    for (name, text) in [("How to use", how_to_use), ("Sign-off", sign_off)] {
        assert!(
            text.contains("scripts/release-readiness.sh"),
            "AC4: the {name} section must name scripts/release-readiness.sh as the gate"
        );
        assert!(
            text.contains("exits `0`") || text.contains("exit `0`") || text.contains("exit 0"),
            "AC4: the {name} section must state that READY requires an exit-0 gate"
        );
    }
}

// ── Anti-drift — doc's online-tier table vs. the script's array ─────────────

// Extract every backtick-delimited span from `s`.
fn backtick_spans(s: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut idx = 0;
    while let Some(rel) = s[idx..].find('`') {
        let start = idx + rel + 1;
        match s[start..].find('`') {
            Some(end_rel) => {
                let end = start + end_rel;
                spans.push(s[start..end].to_string());
                idx = end + 1;
            }
            None => break,
        }
    }
    spans
}

fn doc_online_tier_commands() -> HashSet<String> {
    let content = doc();
    let table = section(
        &content,
        "### P0 — gated online tiers (loopback)",
        "### P1 — tracked, requires explicit acknowledgement",
    );
    backtick_spans(table)
        .into_iter()
        .filter(|s| s.starts_with("cargo test"))
        .collect()
}

fn script_online_tier_commands() -> HashSet<String> {
    let content = script();
    let marker = "ONLINE_TIERS=(";
    let start = content
        .find(marker)
        .expect("scripts/release-readiness.sh must declare an ONLINE_TIERS array")
        + marker.len();
    let end = start
        + content[start..]
            .find("\n)")
            .expect("ONLINE_TIERS array must close with a lone ')' on its own line");
    content[start..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix('"')
                .and_then(|l| l.strip_suffix('"'))
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn doc_and_script_online_tier_command_sets_match_exactly() {
    // Spec §12 / §16 anti-drift guard: the doc's "gated online tiers" table and
    // the script's ONLINE_TIERS array must name the exact same commands. A
    // command added/renamed on one side without the other is a real drift bug
    // this test exists to catch.
    let doc_cmds = doc_online_tier_commands();
    let script_cmds = script_online_tier_commands();

    // Guard against a broken parser silently making this test vacuous.
    assert!(
        doc_cmds.len() >= 5,
        "expected several online-tier commands in RELEASE-READINESS.md; only parsed {} — the \
         table parser may be broken",
        doc_cmds.len()
    );
    assert!(
        script_cmds.len() >= 5,
        "expected several entries in ONLINE_TIERS; only parsed {} — the array parser may be \
         broken",
        script_cmds.len()
    );

    let only_in_doc: Vec<&String> = doc_cmds.difference(&script_cmds).collect();
    let only_in_script: Vec<&String> = script_cmds.difference(&doc_cmds).collect();
    assert!(
        only_in_doc.is_empty() && only_in_script.is_empty(),
        "RELEASE-READINESS.md's online-tier table and scripts/release-readiness.sh's \
         ONLINE_TIERS array have drifted apart.\nOnly in doc: {only_in_doc:?}\nOnly in script: \
         {only_in_script:?}"
    );
}

#[test]
fn script_gates_verdict_on_a_real_exit_code_not_a_flag() {
    // AC4: the script must compute READY only from command exit codes — not
    // print READY unconditionally, and must handle --skip-online by forcing a
    // non-ready, non-zero exit (spec §11 step 2).
    let content = script();
    assert!(
        content.contains("release-readiness: READY"),
        "script must print the exact `release-readiness: READY` verdict line on the all-green path"
    );
    assert!(
        content.contains("release-readiness: NOT READY"),
        "script must print a `release-readiness: NOT READY` verdict line when a P0 check fails"
    );
    assert!(
        content.contains("--skip-online")
            && content.contains("ONLINE TIER SKIPPED")
            && content.contains("NOT release-ready"),
        "script must handle --skip-online by printing a loud, non-suppressible SKIPPED line \
         (spec §11 step 2)"
    );
    assert!(
        content.contains("set -euo pipefail"),
        "script must fail fast/loud like scripts/verify.sh (spec §11)"
    );
}

// ── AC2 — known MVP limitations ──────────────────────────────────────────────

#[test]
fn known_limitations_section_is_non_empty_and_names_starred_items() {
    let content = doc();
    let section_text = section(&content, "## Known MVP limitations", "## Security warnings");
    assert!(
        section_text.trim().len() > "## Known MVP limitations".len() + 50,
        "AC2: 'Known MVP limitations' section must not be empty"
    );
    assert!(
        section_text.contains("Gate A"),
        "AC2: known limitations must mention the pending Gate A real-NAT run"
    );
    let lower = section_text.to_lowercase();
    assert!(
        lower.contains("no cloud inbox") || lower.contains("guaranteed offline delivery"),
        "AC2: known limitations must mention the no-cloud-inbox / no-offline-delivery limitation"
    );
}

// ── AC3 — release notes template ─────────────────────────────────────────────

#[test]
fn release_notes_template_exists_with_required_placeholders() {
    let content = doc();
    let after_heading = content
        .split_once("## Release notes template")
        .map(|(_, rest)| rest)
        .expect("AC3: 'Release notes template' section must exist");
    let fence_start = after_heading
        .find("```markdown")
        .expect("AC3: release notes template must be a fenced ```markdown block");
    let body_start = fence_start + "```markdown".len();
    let body_end = after_heading[body_start..]
        .find("```")
        .map(|p| body_start + p)
        .expect("AC3: release notes template fenced block must close with ```");
    let template = &after_heading[body_start..body_end];

    for placeholder in ["<VERSION>", "Known limitations", "P0 gate:"] {
        assert!(
            template.contains(placeholder),
            "AC3: release notes template must contain {placeholder:?}"
        );
    }
}

// ── Cross-links — spec §13 step 4 ────────────────────────────────────────────

#[test]
fn readme_and_contributing_link_to_the_release_gate() {
    let readme = read("README.md");
    assert!(
        readme.contains("RELEASE-READINESS.md") && readme.contains("scripts/release-readiness.sh"),
        "README.md must link to RELEASE-READINESS.md and scripts/release-readiness.sh"
    );
    let contributing = read("CONTRIBUTING.md");
    assert!(
        contributing.contains("RELEASE-READINESS.md"),
        "CONTRIBUTING.md must reference RELEASE-READINESS.md (a 'cutting a developer preview' step)"
    );
}

// ── AC4 — the gate script actually runs (not just greps) ────────────────────
//
// Every assertion above inspects the script's *text*; a script with a bash
// syntax error, or one that mishandled its arguments, would sail past all of
// them. These two invoke `bash` on the real file — but only along paths that
// terminate before `scripts/verify.sh` or any online tier runs, so they stay
// deterministic and network-free. The full P0 gate belongs to the e2e tier.

fn script_path() -> PathBuf {
    workspace_root().join("scripts/release-readiness.sh")
}

#[test]
fn script_is_syntactically_valid_bash() {
    // `bash -n` parses the whole script (including the online-tier loop and
    // summary code the arg-error path below never reaches) without executing a
    // single command — a syntax slip that all the text-grep tests miss.
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(script_path())
        .output()
        .expect("bash must be available to syntax-check scripts/release-readiness.sh");
    assert!(
        out.status.success(),
        "scripts/release-readiness.sh is not valid bash (`bash -n` failed):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn script_rejects_unknown_argument_with_usage_and_exit_2() {
    // Spec §11: the only accepted flag is --skip-online; anything else is a
    // usage error handled *before* verify.sh / the online tiers, so this is a
    // fast, deterministic exercise of the real script — proof the AC4 gate runs
    // and honours its contract, not merely that its source contains the words.
    let out = std::process::Command::new("bash")
        .arg(script_path())
        .arg("--definitely-not-a-flag")
        .output()
        .expect("bash must be available to run scripts/release-readiness.sh");

    assert_eq!(
        out.status.code(),
        Some(2),
        "an unknown argument must exit 2 (usage), aligned with clap's usage-exit convention; \
         got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage"),
        "the usage error must be printed to stderr; got stderr: {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "stdout must stay clean on a usage error (scripting contract); got stdout: {stdout:?}"
    );
    // A usage error must never masquerade as a READY verdict on either stream.
    assert!(
        !stdout.contains("release-readiness: READY")
            && !stderr.contains("release-readiness: READY"),
        "a usage error must never print a READY verdict"
    );
}

// ── AC1 — the checklist covers all five required test areas ──────────────────

#[test]
fn p0_required_tests_section_names_all_five_ac1_areas() {
    // AC1: "Checklist covers protocol tests, integration tests, pipe security,
    // blob verification, and agent flow." `doc_contains_all_required_sections_
    // in_order` proves the review *headings* exist (pipe/blob/agent), but
    // protocol and integration appear only as rows in the P0 tables — nothing
    // asserts they're named. This closes that AC1 gap.
    let content = doc();
    let section_text = section(&content, "## P0 required tests", "## Pipe security review");
    let lower = section_text.to_lowercase();
    for (area, needle) in [
        ("protocol tests", "protocol test"),
        ("integration tests", "integration"),
        ("pipe security", "pipe security"),
        ("blob verification", "blob verification"),
        ("agent flow", "agent flow"),
    ] {
        assert!(
            lower.contains(needle),
            "AC1: the 'P0 required tests' section must name {area} coverage (looked for {needle:?})"
        );
    }
}

// ── type/security label — the security-warnings section is substantive ───────

#[test]
fn security_warnings_section_names_the_required_warnings() {
    // The issue carries the type/security label; spec §8 enumerates the specific
    // warnings a preview must reproduce. Assert the section is non-empty and
    // names each, so a future edit can't quietly drop one (the same anti-drift
    // posture the known-limitations test takes for AC2).
    let content = doc();
    let section_text = section(
        &content,
        "## Security warnings",
        "## Dependency / churn review",
    );
    assert!(
        section_text.trim().len() > "## Security warnings".len() + 50,
        "type/security: the 'Security warnings' section must not be empty"
    );
    let lower = section_text.to_lowercase();
    for (warning, needle) in [
        ("pipe exposure", "pipe exposure"),
        ("ticket = password-grade capability", "password-grade"),
        ("loopback-only bind", "loopback-only"),
        (
            "agents are not implicitly trusted (least-privileged role)",
            "least-privileged",
        ),
        ("unencrypted local storage", "unencrypted"),
    ] {
        assert!(
            lower.contains(needle),
            "type/security: the 'Security warnings' section must reproduce the {warning:?} \
             warning (looked for {needle:?})"
        );
    }
}

// ── Online-tier invocation contract — spec §5.2 / §11 step 2 ─────────────────
//
// The set-equality test above proves the doc's table and the script's array
// name the *same* commands. These two prove those commands are *correct*: each
// runs the `#[ignore]`-gated tier, serially, against a test binary that still
// exists. A command that silently matched zero tests (e.g. a renamed binary, or
// a dropped `--ignored`) would sail past the set-equality check while making the
// release gate vacuous — exactly the failure the release gate must not have.

#[test]
fn every_online_tier_runs_ignored_tests_serially() {
    // Spec §5.2 / §11: the gated tiers exist to drive the `#[ignore]`-gated
    // online suites one at a time — they bind loopback sockets / spawn child
    // processes and must not race. Every entry must therefore carry both
    // `-- --ignored` (or nothing runs) and `--test-threads=1` (or they race).
    let cmds = script_online_tier_commands();
    assert!(
        cmds.len() >= 5,
        "sanity: parsed too few ONLINE_TIERS entries ({}) — the array parser may be broken",
        cmds.len()
    );
    for cmd in &cmds {
        assert!(
            cmd.starts_with("cargo test "),
            "each ONLINE_TIERS entry must be a `cargo test` invocation; got {cmd:?}"
        );
        assert!(
            cmd.contains(" -- --ignored"),
            "ONLINE_TIERS entry must run the #[ignore]-gated tier via `-- --ignored`, else it \
             matches zero online tests and the gate is vacuous; got {cmd:?}"
        );
        assert!(
            cmd.contains("--test-threads=1"),
            "ONLINE_TIERS entry must serialize with --test-threads=1 (loopback suites cannot \
             race); got {cmd:?}"
        );
    }
}

#[test]
fn online_tier_commands_reference_real_test_binaries() {
    // Each ONLINE_TIERS command targets one or more `--test <name>` binaries. A
    // renamed/deleted test file would leave the command matching zero tests and
    // "passing" at release time — a silent hole in the gate. Assert every
    // referenced binary still exists somewhere under crates/*/tests/.
    let crates_dir = workspace_root().join("crates");
    let mut stems: HashSet<String> = HashSet::new();
    for crate_entry in std::fs::read_dir(&crates_dir).expect("crates/ must exist") {
        let tests_dir = crate_entry
            .expect("readable crate dir entry")
            .path()
            .join("tests");
        let Ok(entries) = std::fs::read_dir(&tests_dir) else {
            continue; // crate has no tests/ dir
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    stems.insert(stem.to_string());
                }
            }
        }
    }
    assert!(
        stems.len() > 5,
        "sanity: expected to discover several integration-test binaries under crates/*/tests/, \
         found {} — the discovery walk may be broken",
        stems.len()
    );

    let mut referenced: Vec<String> = Vec::new();
    for cmd in script_online_tier_commands() {
        let tokens: Vec<&str> = cmd.split_whitespace().collect();
        for (i, tok) in tokens.iter().enumerate() {
            // Exact `--test` only; `--test-threads=1` is a distinct token and
            // must not be mistaken for a binary name.
            if *tok == "--test" {
                let name = tokens
                    .get(i + 1)
                    .unwrap_or_else(|| panic!("`--test` with no binary name in {cmd:?}"));
                referenced.push((*name).to_string());
            }
        }
    }
    assert!(
        referenced.len() >= 6,
        "sanity: expected several `--test` targets across ONLINE_TIERS, found {} — the token \
         scan may be broken",
        referenced.len()
    );
    for name in referenced {
        assert!(
            stems.contains(&name),
            "ONLINE_TIERS references `--test {name}` but no crates/*/tests/{name}.rs exists; a \
             renamed/deleted test binary makes that release tier silently match zero tests"
        );
    }
}

// ── Deterministic tier is verify.sh (single source of truth) — spec §5.1 ─────

#[test]
fn deterministic_tier_delegates_to_verify_sh() {
    // Spec §5.1 / §11 step 1: the deterministic P0 set is *exactly*
    // scripts/verify.sh. The gate must invoke it (not re-list its commands), and
    // the doc's deterministic-tier section must point at it — so the fmt/clippy/
    // workspace-test set has one source of truth and cannot drift.
    assert!(
        script().contains("scripts/verify.sh"),
        "the gate script must run scripts/verify.sh as its deterministic P0 tier"
    );
    let content = doc();
    let deterministic = section(
        &content,
        "### P0 — deterministic",
        "### P0 — gated online tiers",
    );
    assert!(
        deterministic.contains("scripts/verify.sh"),
        "the doc's deterministic-tier section must name scripts/verify.sh as the single source \
         of truth (no re-enumerated command list to drift)"
    );
}

// ── AC2 honesty — the file-cap claim is pinned to the real source constant ───

#[test]
fn known_limitations_file_cap_matches_the_source_constant() {
    // The 'Known MVP limitations' section states MAX_SHARED_FILE_BYTES as a
    // known cap-vs-metric divergence. If the real constant changes, the doc's
    // number silently lies. Derive the value from source and assert the doc
    // cites that exact literal, so the two can never drift apart.
    let constants = read("crates/iroh-rooms-core/src/event/constants.rs");
    let after_name = constants
        .split_once("MAX_SHARED_FILE_BYTES")
        .map(|(_, rest)| rest)
        .expect("iroh-rooms-core must define MAX_SHARED_FILE_BYTES");
    let after_eq = after_name
        .split_once('=')
        .map(|(_, rest)| rest)
        .expect("MAX_SHARED_FILE_BYTES declaration must have a value");
    let literal: String = after_eq
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .collect();
    assert!(
        literal.replace('_', "").parse::<u64>().is_ok() && !literal.is_empty(),
        "failed to parse MAX_SHARED_FILE_BYTES numeric literal (got {literal:?})"
    );

    let content = doc();
    let limits = section(&content, "## Known MVP limitations", "## Security warnings");
    assert!(
        limits.contains(&literal),
        "AC2 honesty: 'Known MVP limitations' must cite the exact MAX_SHARED_FILE_BYTES value \
         ({literal} bytes) from crates/iroh-rooms-core/src/event/constants.rs; the source \
         constant changed without updating RELEASE-READINESS.md's file-cap divergence note"
    );
    assert!(
        limits.contains("MiB") || limits.contains("MB"),
        "AC2: the file-cap limitation must also give a human-readable cap (MiB/MB)"
    );
}

// ── Traceability — the doc's local companion references resolve ──────────────

#[test]
fn doc_local_references_resolve_to_real_files() {
    // The checklist's traceability leans on companion files (the go/no-go memo,
    // the getting-started demo it dry-runs, the Gate A notes/results, the PRD,
    // the file-cap constant). A moved/renamed companion would rot the checklist
    // silently; assert each referenced local path both appears in the doc and
    // exists on disk.
    let content = doc();
    for rel in [
        "PHASE-0-GO-NO-GO.md",
        "docs/getting-started.md",
        "crates/iroh-rooms-net/NOTES.md",
        "crates/spike-nat/results/results.md",
        "PRD.v0.3.md",
        "crates/iroh-rooms-core/src/event/constants.rs",
    ] {
        assert!(
            content.contains(rel),
            "RELEASE-READINESS.md is expected to reference {rel}; if it was intentionally \
             dropped, update this test"
        );
        assert!(
            workspace_root().join(rel).exists(),
            "RELEASE-READINESS.md references {rel}, but that file does not exist (moved/renamed?)"
        );
    }
}

// ── AC3 + type/security — the release-notes template keeps its disclaimers ────

#[test]
fn release_notes_template_carries_the_preview_and_security_disclaimers() {
    // AC3 proves the template *exists*; this proves it stays honest. A preview's
    // release notes must not read as production-ready, so the "developer preview
    // / not for production / no security audit" disclaimers and the remaining
    // fill-in placeholders must survive edits.
    let content = doc();
    let after = content
        .split_once("## Release notes template")
        .map(|(_, rest)| rest)
        .expect("AC3: 'Release notes template' section must exist");
    let body_start = after
        .find("```markdown")
        .expect("AC3: release notes template must be a fenced ```markdown block")
        + "```markdown".len();
    let body_end = body_start
        + after[body_start..]
            .find("```")
            .expect("AC3: release notes template fenced block must close with ```");
    let template = &after[body_start..body_end];

    let lower = template.to_lowercase();
    for (what, needle) in [
        ("label the build a developer preview", "developer preview"),
        ("state it is not for production", "not for production"),
        ("state no security audit was performed", "no security audit"),
    ] {
        assert!(
            lower.contains(needle),
            "type/security: the release-notes template must {what} (looked for {needle:?})"
        );
    }
    for placeholder in ["<DATE>", "<PREV_VERSION>"] {
        assert!(
            template.contains(placeholder),
            "AC3: the release-notes template must keep the {placeholder} fill-in placeholder"
        );
    }
}
