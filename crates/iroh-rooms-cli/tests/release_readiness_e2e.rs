//! End-to-end coverage for the release-readiness gate script itself
//! (`scripts/release-readiness.sh`, issue #41 / IR-0306).
//!
//! `release_readiness_docs.rs` is deliberately text/structure-only — its own
//! module doc explains that it only runs the script along paths that
//! terminate *before* `scripts/verify.sh` or any online tier (`bash -n`, the
//! unknown-argument usage path), and defers "the full P0 gate" to "the e2e
//! tier". This file is that tier. It proves AC4 ("a preview cannot be marked
//! ready while a P0 check is failing") against the real exit-code wiring of
//! the real script, not just against the words in its source.
//!
//! Actually running the real deterministic tier (`scripts/verify.sh`) or the
//! real online tiers (the six `#[ignore]`-gated suites) from inside a test
//! would make every run of this suite as slow as a full release dry-run, and
//! would just duplicate coverage `scripts/verify.sh` and each online-tier test
//! file already provide on their own. So each test here copies the real
//! script into an isolated temp "repo root" and substitutes only its two
//! external-command boundaries:
//!
//! - the `scripts/verify.sh` invocation, with a tiny fake script that exits a
//!   chosen code;
//! - the `ONLINE_TIERS` array, with fast, controlled stand-ins (`"true"` /
//!   `"false"`) in place of the real `cargo test ... -- --ignored` commands.
//!
//! Every other line — the record/summary/verdict bookkeeping AC4 actually
//! rests on — runs unmodified. This is the spec §14 "forced-failure sanity
//! check" and "`--skip-online` sanity" test-plan items, automated instead of
//! left as a one-off manual PR-description demonstration.
//!
//! ## Running
//!
//! ```bash
//! cargo test -p iroh-rooms-cli --test release_readiness_e2e
//! ```
//!
//! No `--ignored` gate: every test here is fast and network-free.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/iroh-rooms-cli; workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

/// Copy the real gate script into a fresh temp "repo root", patching only its
/// two external-command boundaries, then return the temp dir (kept alive for
/// the caller) and the patched script's path.
fn patched_script(verify_exit_code: i32, online_tier_commands: &[&str]) -> (TempDir, PathBuf) {
    let real = std::fs::read_to_string(workspace_root().join("scripts/release-readiness.sh"))
        .expect("scripts/release-readiness.sh must exist");

    let dir = TempDir::new().expect("tempdir");
    let scripts_dir = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");

    let fake_verify = scripts_dir.join("fake-verify.sh");
    std::fs::write(
        &fake_verify,
        format!("#!/usr/bin/env bash\nexit {verify_exit_code}\n"),
    )
    .expect("write fake verify script");

    // Replace the ONLINE_TIERS array body, keeping the surrounding script
    // (including the "ONLINE_TIERS=(" opener and the closing ")") untouched.
    let marker = "ONLINE_TIERS=(";
    let start = real.find(marker).expect("ONLINE_TIERS array must exist") + marker.len();
    let close_rel = real[start..]
        .find("\n)")
        .expect("ONLINE_TIERS array must close with a lone ')' on its own line");
    let end = start + close_rel + "\n)".len();
    let mut replacement = String::from("\n");
    for cmd in online_tier_commands {
        let _ = writeln!(replacement, "  \"{cmd}\"");
    }
    replacement.push(')');
    let with_tiers = format!("{}{}{}", &real[..start], replacement, &real[end..]);

    let patched = with_tiers.replacen(
        "if scripts/verify.sh; then",
        &format!("if bash \"{}\"; then", fake_verify.display()),
        1,
    );
    assert_ne!(
        patched, with_tiers,
        "the scripts/verify.sh invocation must have been patched — the real script's wording \
         may have changed"
    );

    let script_path = scripts_dir.join("release-readiness.sh");
    std::fs::write(&script_path, patched).expect("write patched script");

    (dir, script_path)
}

fn run(script_path: &Path, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(script_path)
        .args(args)
        .output()
        .expect("bash must be available to run the patched script")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn all_p0_checks_passing_yields_ready_exit_zero() {
    let (_dir, script) = patched_script(0, &["true", "true", "true"]);
    let out = run(&script, &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "stdout:\n{text}");
    assert!(text.contains("-- verify.sh: PASS"), "stdout:\n{text}");
    assert!(text.contains("release-readiness: READY"), "stdout:\n{text}");
}

#[test]
fn a_failing_online_tier_yields_not_ready_and_names_it() {
    let (_dir, script) = patched_script(0, &["true", "false", "true"]);
    let out = run(&script, &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "stdout:\n{text}");
    assert!(
        text.contains("release-readiness: NOT READY (1 P0 checks failing: false)"),
        "the failing command must be named in the verdict line; stdout:\n{text}"
    );
    assert!(
        !text.contains("release-readiness: READY"),
        "stdout:\n{text}"
    );
}

#[test]
fn a_failing_deterministic_tier_yields_not_ready_even_if_every_online_tier_passes() {
    let (_dir, script) = patched_script(1, &["true", "true"]);
    let out = run(&script, &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "stdout:\n{text}");
    assert!(text.contains("-- verify.sh: FAIL"), "stdout:\n{text}");
    assert!(
        text.contains("release-readiness: NOT READY (1 P0 checks failing: verify.sh)"),
        "verify.sh's own failure must be named in the verdict even though every online tier \
         passed; stdout:\n{text}"
    );
}

#[test]
fn skip_online_forces_not_ready_even_when_everything_passes() {
    let (_dir, script) = patched_script(0, &["true"]);
    let out = run(&script, &["--skip-online"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "stdout:\n{text}");
    assert!(text.contains("-- verify.sh: PASS"), "stdout:\n{text}");
    assert!(
        text.contains("release-readiness: ONLINE TIER SKIPPED — NOT release-ready"),
        "stdout:\n{text}"
    );
    assert!(
        !text.contains("release-readiness: READY"),
        "stdout:\n{text}"
    );
}

#[test]
fn multiple_p0_failures_are_all_named_and_counted_in_verdict() {
    // A verdict that only surfaced the *first* failing P0 check would let a
    // maintainer fix one failure, re-read the line, and miss that a second
    // check was also red. AC4 requires every failing P0 to be named.
    let (_dir, script) = patched_script(1, &["true", "false"]);
    let out = run(&script, &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "stdout:\n{text}");
    assert!(text.contains("-- verify.sh: FAIL"), "stdout:\n{text}");
    assert!(
        text.contains("release-readiness: NOT READY (2 P0 checks failing: verify.sh; false)"),
        "both the failing deterministic tier and the failing online tier must be named, in \
         the order they were checked; stdout:\n{text}"
    );
}

#[test]
fn an_early_online_tier_failure_does_not_skip_the_remaining_tiers() {
    // The script runs under `set -euo pipefail`. If the online-tier loop body
    // were written without the `if (eval "$cmd"); then ... fi` guard, a
    // failing tier would abort the whole script instead of being recorded
    // and moving on — silently under-reporting which P0 checks are red.
    let (_dir, script) = patched_script(0, &["false", "echo ONLINE_TIER_2_RAN && true", "false"]);
    let out = run(&script, &[]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "stdout:\n{text}");
    assert!(
        text.contains("ONLINE_TIER_2_RAN"),
        "a failing online tier must not short-circuit the remaining tiers; stdout:\n{text}"
    );
    assert!(
        text.contains("release-readiness: NOT READY (2 P0 checks failing: false; false)"),
        "both failing online tiers must be named even though a tier between them ran and \
         passed; stdout:\n{text}"
    );
}
