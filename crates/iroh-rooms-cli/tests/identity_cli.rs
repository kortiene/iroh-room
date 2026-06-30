//! CLI integration tests for `identity create` and `identity show` (issue #16 / IR-0101).
//!
//! Each test gets its own temp directory via `--data-dir` so tests are fully
//! isolated even when run in parallel. `IROH_ROOMS_HOME` is removed from the
//! environment in all `--data-dir` tests to prevent interference.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Return a `Command` for `iroh-rooms` pointed at `home` via `--data-dir`.
/// `IROH_ROOMS_HOME` is explicitly cleared so the flag is the sole source.
fn cmd(home: &TempDir) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    c
}

// ── identity create ──────────────────────────────────────────────────────────

/// AC: identity keypair is generated and printed.
#[test]
fn identity_create_exits_zero_and_prints_ids() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success()
        .stdout(predicate::str::contains("identity_id:"))
        .stdout(predicate::str::contains("device_id:"));
}

/// AC: profile name is stored and echoed on create.
#[test]
fn identity_create_echoes_the_provided_name() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "DisplayName"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DisplayName"));
}

/// AC: `identity_id` and `device_id` in `create` output are distinct 64-char hex strings.
#[test]
fn identity_create_produces_distinct_64_char_hex_ids() {
    let home = TempDir::new().unwrap();
    let output = cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    let identity_id = extract_field(&stdout, "identity_id").expect("identity_id must appear");
    let device_id = extract_field(&stdout, "device_id").expect("device_id must appear");

    assert_eq!(identity_id.len(), 64, "identity_id must be 64 hex chars");
    assert_eq!(device_id.len(), 64, "device_id must be 64 hex chars");
    assert!(
        identity_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "identity_id must be lowercase hex"
    );
    assert!(
        device_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "device_id must be lowercase hex"
    );
    assert_ne!(
        identity_id, device_id,
        "sender_id and device_id must be distinct keys (spec §1)"
    );
}

/// AC: `identity show` hint printed after create.
#[test]
fn identity_create_suggests_identity_show() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success()
        .stdout(predicate::str::contains("identity show"));
}

// ── create guard (no force) ──────────────────────────────────────────────────

/// AC: existing identity is not overwritten without --force.
#[test]
fn identity_create_twice_without_force_fails() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "create", "--name", "Alice2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
}

/// Guard error must be actionable — mention the flag.
#[test]
fn identity_create_guard_error_is_actionable() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "create", "--name", "Dup"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
}

// ── create --force ───────────────────────────────────────────────────────────

/// AC: --force allows replacing an existing identity.
#[test]
fn identity_create_force_flag_succeeds_on_existing_identity() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "create", "--name", "Bob", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Bob"));
}

/// --force emits a warning to stderr so the user knows the overwrite happened.
#[test]
fn identity_create_force_warns_on_stderr() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "create", "--name", "Bob", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains("warning"));
}

// ── argument validation ──────────────────────────────────────────────────────

/// Missing --name must produce a non-zero exit.
#[test]
fn identity_create_without_name_arg_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home).args(["identity", "create"]).assert().failure();
}

/// Empty --name must produce a non-zero exit with a useful error.
#[test]
fn identity_create_with_empty_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", ""])
        .assert()
        .failure();
}

/// A name over 64 bytes must be rejected.
#[test]
fn identity_create_with_overlong_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    let long = "a".repeat(65);
    cmd(&home)
        .args(["identity", "create", "--name", &long])
        .assert()
        .failure();
}

/// A name containing a control character (newline) must be rejected.
#[test]
fn identity_create_with_control_char_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice\nEve"])
        .assert()
        .failure();
}

// ── identity show (text) ─────────────────────────────────────────────────────

/// AC: `identity show` prints script-friendly key: value lines.
#[test]
fn identity_show_prints_labeled_lines_for_all_fields() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("name:"))
        .stdout(predicate::str::contains("identity_id:"))
        .stdout(predicate::str::contains("device_id:"));
}

/// `identity show` output must include the name that was set at create time.
#[test]
fn identity_show_includes_profile_name() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "SpecialName"])
        .assert()
        .success();

    cmd(&home)
        .args(["identity", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SpecialName"));
}

/// IDs emitted by `show` must match those emitted by `create`.
#[test]
fn identity_show_ids_match_create_output() {
    let home = TempDir::new().unwrap();

    let create_out = cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .output()
        .unwrap();
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let create_identity_id =
        extract_field(&create_stdout, "identity_id").expect("identity_id in create output");
    let create_device_id =
        extract_field(&create_stdout, "device_id").expect("device_id in create output");

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let show_identity_id =
        extract_field(&show_stdout, "identity_id").expect("identity_id in show output");
    let show_device_id =
        extract_field(&show_stdout, "device_id").expect("device_id in show output");

    assert_eq!(
        create_identity_id, show_identity_id,
        "identity_id must be stable across create and show"
    );
    assert_eq!(
        create_device_id, show_device_id,
        "device_id must be stable across create and show"
    );
}

// ── identity show --json ─────────────────────────────────────────────────────

/// AC: `--json` emits a single-line JSON object parseable by scripts.
#[test]
fn identity_show_json_is_valid_json() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let output = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json output must be valid JSON");
}

/// JSON output must contain `name`, `identity_id`, and `device_id` fields.
#[test]
fn identity_show_json_contains_required_fields() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "JsonAlice"])
        .assert()
        .success();

    let output = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();

    assert_eq!(v["name"].as_str().unwrap(), "JsonAlice");
    let id = v["identity_id"]
        .as_str()
        .expect("identity_id field must exist");
    let dev = v["device_id"].as_str().expect("device_id field must exist");
    assert_eq!(id.len(), 64, "identity_id must be 64 hex chars");
    assert_eq!(dev.len(), 64, "device_id must be 64 hex chars");
}

/// JSON output must include `version` = 1 (on-disk format version field).
#[test]
fn identity_show_json_version_field_is_1() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let output = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let version = v["version"]
        .as_u64()
        .expect("version field must be present and numeric");
    assert_eq!(version, 1, "on-disk format version must be 1");
}

/// JSON output must include a positive `created_at_ms` epoch timestamp.
#[test]
fn identity_show_json_created_at_ms_is_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let output = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let ts = v["created_at_ms"]
        .as_u64()
        .expect("created_at_ms field must be present and numeric");
    assert!(ts > 0, "created_at_ms must be a positive epoch timestamp");
}

/// JSON output must NOT contain secret-bearing field names.
#[test]
fn identity_show_json_does_not_expose_secret_fields() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let output = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("identity_secret"),
        "--json output must not contain identity_secret"
    );
    assert!(
        !stdout.contains("device_secret"),
        "--json output must not contain device_secret"
    );
}

// ── identity show without identity ──────────────────────────────────────────

/// AC: `show` with no identity gives a non-zero exit and a useful error message.
#[test]
fn identity_show_without_identity_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "show"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identity create"));
}

// ── data isolation ───────────────────────────────────────────────────────────

/// `--data-dir` must isolate identities: creating in dir A must not affect dir B.
#[test]
fn data_dir_flag_isolates_identities() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();

    cmd(&home_a)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    // home_b has no identity — show must fail.
    cmd(&home_b).args(["identity", "show"]).assert().failure();
}

/// `IROH_ROOMS_HOME` env var selects the data directory when no flag is given.
#[test]
fn iroh_rooms_home_env_var_sets_data_dir() {
    let home = TempDir::new().unwrap();

    Command::cargo_bin("iroh-rooms")
        .unwrap()
        .env("IROH_ROOMS_HOME", home.path())
        // No --data-dir; must use the env var.
        .args(["identity", "create", "--name", "EnvVarTest"])
        .assert()
        .success();

    // Verify the file was created in the env-var directory.
    assert!(
        home.path().join("identity.json").exists(),
        "identity.json must be created in the IROH_ROOMS_HOME directory"
    );
}

/// --force must work even when no prior identity exists in the data directory.
#[test]
fn identity_create_force_on_fresh_directory_succeeds() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Alice"))
        .stdout(predicate::str::contains("identity_id:"))
        .stdout(predicate::str::contains("device_id:"));
}

/// An empty `IROH_ROOMS_HOME` must be treated as unset; `--data-dir` must win.
#[test]
fn iroh_rooms_home_empty_string_is_ignored() {
    let flag_home = TempDir::new().unwrap();

    Command::cargo_bin("iroh-rooms")
        .unwrap()
        // Explicitly set env var to the empty string — must be treated as unset.
        .env("IROH_ROOMS_HOME", "")
        .arg("--data-dir")
        .arg(flag_home.path())
        .args(["identity", "create", "--name", "EmptyEnvTest"])
        .assert()
        .success();

    assert!(
        flag_home.path().join("identity.json").exists(),
        "identity.json must be written to --data-dir when IROH_ROOMS_HOME is empty"
    );
}

/// `--data-dir` takes precedence over `IROH_ROOMS_HOME`.
#[test]
fn data_dir_flag_beats_env_var() {
    let flag_home = TempDir::new().unwrap();
    let env_home = TempDir::new().unwrap();

    Command::cargo_bin("iroh-rooms")
        .unwrap()
        .env("IROH_ROOMS_HOME", env_home.path())
        .arg("--data-dir")
        .arg(flag_home.path())
        .args(["identity", "create", "--name", "FlagWins"])
        .assert()
        .success();

    assert!(
        flag_home.path().join("identity.json").exists(),
        "identity.json must be in the --data-dir directory"
    );
    assert!(
        !env_home.path().join("identity.json").exists(),
        "identity.json must NOT be in the IROH_ROOMS_HOME directory when --data-dir is set"
    );
}

// ── security: no secret material in output ───────────────────────────────────

/// The raw secret seeds stored in `identity.secret` must never appear in stdout or stderr.
#[test]
fn identity_create_does_not_log_secret_seeds() {
    let home = TempDir::new().unwrap();
    let output = cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let secret_path = home.path().join("identity.secret");
    let secret_raw = std::fs::read_to_string(&secret_path).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap();
    let device_seed = secret_v["device_secret"].as_str().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stdout.contains(identity_seed),
        "create stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(identity_seed),
        "create stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(device_seed),
        "create stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(device_seed),
        "create stderr must not contain the device secret seed"
    );
}

/// `identity show` must never expose the secret seeds (it reads only identity.json).
#[test]
fn identity_show_does_not_log_secret_seeds() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret")).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap();
    let device_seed = secret_v["device_secret"].as_str().unwrap();

    let output = cmd(&home).args(["identity", "show"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stdout.contains(identity_seed),
        "show stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(identity_seed),
        "show stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(device_seed),
        "show stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(device_seed),
        "show stderr must not contain the device secret seed"
    );
}

// ── guard: filesystem unchanged after rejection ──────────────────────────────

/// Spec §11 test #5: after a failed second `create` (no `--force`), both
/// `identity.json` and `identity.secret` must be byte-for-byte unchanged on disk.
/// Verifies that the guard does not partially write before aborting.
#[test]
fn identity_create_guard_leaves_identity_json_byte_identical() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let before = std::fs::read(home.path().join("identity.json")).unwrap();

    cmd(&home)
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .failure();

    let after = std::fs::read(home.path().join("identity.json")).unwrap();
    assert_eq!(
        before, after,
        "identity.json must not be modified when create is rejected by the overwrite guard"
    );
}

/// Companion to the above: `identity.secret` must also be untouched after a guard
/// rejection (no partial write of new secret seeds).
#[test]
fn identity_create_guard_leaves_secret_file_byte_identical() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let before = std::fs::read(home.path().join("identity.secret")).unwrap();

    cmd(&home)
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .failure();

    let after = std::fs::read(home.path().join("identity.secret")).unwrap();
    assert_eq!(
        before, after,
        "identity.secret must not be modified when create is rejected by the overwrite guard"
    );
}

// ── force: key rotation ───────────────────────────────────────────────────────

/// Spec §11 test #6: `--force` must generate a *fresh* `identity_id`, not reuse
/// the previous key. This is the core correctness property of force-replace.
#[test]
fn identity_create_force_rotates_identity_id() {
    let home = TempDir::new().unwrap();

    let first_out = cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .output()
        .unwrap();
    assert!(first_out.status.success());
    let first_stdout = String::from_utf8_lossy(&first_out.stdout);
    let first_id = extract_field(&first_stdout, "identity_id")
        .expect("identity_id in first create output")
        .to_owned();

    let second_out = cmd(&home)
        .args(["identity", "create", "--name", "Bob", "--force"])
        .output()
        .unwrap();
    assert!(second_out.status.success());
    let second_stdout = String::from_utf8_lossy(&second_out.stdout);
    let second_id =
        extract_field(&second_stdout, "identity_id").expect("identity_id in second create output");

    assert_ne!(
        first_id, second_id,
        "--force must generate a fresh identity_id, not reuse the previous key"
    );
}

/// After `--force`, `identity show` must return the *new* `identity_id`, not the
/// old one that was replaced.
#[test]
fn identity_show_after_force_returns_new_identity_id() {
    let home = TempDir::new().unwrap();

    let first_out = cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .output()
        .unwrap();
    assert!(first_out.status.success());
    let first_stdout = String::from_utf8_lossy(&first_out.stdout);
    let first_id = extract_field(&first_stdout, "identity_id")
        .expect("identity_id in first create output")
        .to_owned();

    cmd(&home)
        .args(["identity", "create", "--name", "Bob", "--force"])
        .assert()
        .success();

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    assert!(show_out.status.success());
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let show_id = extract_field(&show_stdout, "identity_id")
        .expect("identity_id in show output after --force");

    assert_ne!(
        first_id, show_id,
        "`identity show` must return the new identity_id after --force, not the discarded key"
    );
}

// ── show stability ────────────────────────────────────────────────────────────

/// Spec §11 test #4: ids must be stable across repeated `identity show` calls.
/// The output of two consecutive `show` invocations must be byte-identical.
#[test]
fn identity_show_output_is_stable_across_repeated_calls() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let first = cmd(&home).args(["identity", "show"]).output().unwrap();
    let second = cmd(&home).args(["identity", "show"]).output().unwrap();

    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "`identity show` output must be byte-identical across repeated calls"
    );
}

// ── cross-format consistency ──────────────────────────────────────────────────

/// `identity show --json` must report the same `identity_id` and `device_id` as
/// the default text output. Catches format-specific serialisation divergence.
#[test]
fn identity_show_json_ids_match_text_output_ids() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let text_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    assert!(text_out.status.success());
    let text_stdout = String::from_utf8_lossy(&text_out.stdout);
    let text_identity_id =
        extract_field(&text_stdout, "identity_id").expect("identity_id in text output");
    let text_device_id =
        extract_field(&text_stdout, "device_id").expect("device_id in text output");

    let json_out = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let json_stdout = String::from_utf8_lossy(&json_out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(json_stdout.trim()).expect("--json output must be valid JSON");

    assert_eq!(
        v["identity_id"].as_str().unwrap(),
        text_identity_id,
        "identity_id must be identical between text and --json output"
    );
    assert_eq!(
        v["device_id"].as_str().unwrap(),
        text_device_id,
        "device_id must be identical between text and --json output"
    );
}

// ── file-system side-effects ──────────────────────────────────────────────────

/// An invalid `--name` must not create any files in the data directory.
/// (Spec §8: "Fails on an invalid name … nothing written.")
/// This is the CLI-level complement of the unit-level `create_invalid_name_writes_no_files`.
#[test]
fn identity_create_invalid_name_does_not_write_files() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", ""])
        .assert()
        .failure();

    assert!(
        !home.path().join("identity.json").exists(),
        "identity.json must not be created when --name is invalid"
    );
    assert!(
        !home.path().join("identity.secret").exists(),
        "identity.secret must not be created when --name is invalid"
    );
}

// ── Unix file permissions (CLI-level) ─────────────────────────────────────────

/// Spec §11 test #7: files written by the binary must have mode 0600 on Unix.
/// This is the CLI-level complement of the unit-level `create_files_have_0600_permissions`.
#[cfg(unix)]
#[test]
fn identity_create_via_cli_produces_0600_files() {
    use std::os::unix::fs::MetadataExt;
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    for name in &["identity.json", "identity.secret"] {
        let mode = std::fs::metadata(home.path().join(name)).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "{name} must be owner-read/write only (0600) when written by the CLI binary; \
             got {mode:o}"
        );
    }
}

/// Spec §11 test #7: the data directory must have mode 0700 on Unix.
/// `TempDir::new()` creates the dir with a broad mode; `ensure_dir` must tighten it to 0700.
#[cfg(unix)]
#[test]
fn identity_create_via_cli_produces_0700_data_dir() {
    use std::os::unix::fs::MetadataExt;
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();

    let mode = std::fs::metadata(home.path()).unwrap().mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "data directory must be owner-only (0700) after `identity create`; got {mode:o}"
    );
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract the value of a `key: value` line from CLI text output.
fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            return Some(rest.trim());
        }
    }
    None
}
