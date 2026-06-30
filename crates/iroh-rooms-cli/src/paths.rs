//! Data-directory resolution and creation.
//!
//! Every `iroh-rooms` command operates against a single per-participant **data
//! directory** (the "home"). Identity files live here today; IR-0004's
//! `rooms.db` and all later room state share the same home (spec IR-0101 D3), so
//! the resolution rule defined here is the one every future subcommand reuses.
//!
//! Resolution precedence (highest first):
//!
//! 1. the `--data-dir <PATH>` global CLI flag,
//! 2. the `IROH_ROOMS_HOME` environment variable,
//! 3. the platform default (`directories`): `~/.local/share/iroh-rooms` (Linux,
//!    honoring `XDG_DATA_HOME`), `~/Library/Application Support/iroh-rooms`
//!    (macOS), `%APPDATA%\iroh-rooms` (Windows).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Environment variable selecting the data directory (precedence 2).
pub const HOME_ENV: &str = "IROH_ROOMS_HOME";

/// Resolve the data directory without creating it.
///
/// `flag` is the value of the global `--data-dir` option (precedence 1).
///
/// # Errors
/// Fails only when no directory could be resolved at all — i.e. neither the flag
/// nor `IROH_ROOMS_HOME` is set and the platform default home directory cannot be
/// determined.
pub fn data_dir(flag: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = flag {
        return Ok(dir.to_path_buf());
    }
    // An empty `IROH_ROOMS_HOME` is treated as unset rather than "the current
    // directory", which would be a surprising place to drop secret keys.
    if let Some(home) = std::env::var_os(HOME_ENV) {
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    platform_default()
}

fn platform_default() -> Result<PathBuf> {
    // Qualifier/organization are empty: the application segment alone yields the
    // conventional per-OS path documented above.
    let dirs = directories::ProjectDirs::from("", "", "iroh-rooms").context(
        "could not determine a platform data directory; \
         set IROH_ROOMS_HOME or pass --data-dir <PATH>",
    )?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Create the data-directory tree if absent and restrict it to owner-only access.
///
/// On Unix the directory is tightened to `0700`. On other platforms the mode bits
/// are a best-effort no-op (spec IR-0101 D6); the directory is still created.
///
/// # Errors
/// Fails if the directory tree cannot be created or (on Unix) its permissions
/// cannot be set — the latter fails closed so we never proceed with a
/// looser-than-`0700` home.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create data directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms)
            .with_context(|| format!("could not set 0700 permissions on {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── data_dir ────────────────────────────────────────────────────────────

    #[test]
    fn data_dir_returns_flag_value_when_provided() {
        let dir = tempdir().unwrap();
        let result = data_dir(Some(dir.path())).unwrap();
        assert_eq!(result, dir.path());
    }

    #[test]
    fn data_dir_flag_beats_env_var() {
        // The flag is the highest-precedence source; env var must be ignored.
        // (We test env-var precedence in integration tests to avoid thread-safety issues.)
        let dir = tempdir().unwrap();
        let result = data_dir(Some(dir.path())).unwrap();
        // Result must equal exactly the flag path, regardless of any ambient env var.
        assert_eq!(result, dir.path());
    }

    // ── ensure_dir ──────────────────────────────────────────────────────────

    #[test]
    fn ensure_dir_creates_directory_when_absent() {
        let parent = tempdir().unwrap();
        let new_dir = parent.path().join("iroh-rooms-test");
        assert!(!new_dir.exists());
        ensure_dir(&new_dir).unwrap();
        assert!(new_dir.is_dir());
    }

    #[test]
    fn ensure_dir_creates_nested_directories() {
        let parent = tempdir().unwrap();
        let nested = parent.path().join("a").join("b").join("c");
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = tempdir().unwrap();
        ensure_dir(dir.path()).unwrap();
        ensure_dir(dir.path()).unwrap(); // second call must not error
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_sets_0700_permissions() {
        use std::os::unix::fs::MetadataExt;
        let parent = tempdir().unwrap();
        let new_dir = parent.path().join("secure");
        ensure_dir(&new_dir).unwrap();
        let mode = std::fs::metadata(&new_dir).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "data directory must be owner-only (0700), got {mode:o}"
        );
    }
}
