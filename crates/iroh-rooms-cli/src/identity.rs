//! Local identity persistence: create, load, and present the participant
//! identity (`sender_id`) and device (`device_id`) keypairs plus a profile name.
//!
//! On-disk layout under the resolved data directory (spec IR-0101 D4), both files
//! created owner-only (`0600` on Unix):
//!
//! ```text
//! <HOME>/identity.json     # public profile — safe for `identity show` to read
//! <HOME>/identity.secret   # the ONLY file holding secret seeds
//! ```
//!
//! The split keeps every read-only path (`identity show` and future commands)
//! away from the secret file: `show` opens only `identity.json` and can never
//! load, log, or leak a seed (spec D8 / §9).
//!
//! **At-rest threat model (MVP):** seeds are stored *plaintext* under owner-only
//! permissions. This protects against other local users but not against an
//! attacker with this account or raw disk access. Encrypted-at-rest storage and
//! recovery phrases are out of MVP (PRD §13.4) and on the roadmap (§13.5).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms_core::event::keys::SigningKey;
use zeroize::{Zeroize, Zeroizing};

/// File name of the public identity profile.
pub const IDENTITY_FILE: &str = "identity.json";
/// File name of the secret-bearing key file.
pub const SECRET_FILE: &str = "identity.secret";

/// On-disk format version of both `identity.json` and `identity.secret`.
const PROFILE_VERSION: u32 = 1;
/// Maximum profile-name length, in UTF-8 bytes (spec OQ-5; reconciled with the
/// future `member.joined.display_name` when membership events are wired).
const MAX_NAME_BYTES: usize = 64;
/// Length in bytes of an Ed25519 secret seed (the hex-encoded form is twice this).
const SEED_LEN: usize = 32;

/// The public identity profile, persisted as `identity.json` and printed by
/// `identity show`. Contains no secret bytes — safe to read, serialize, and log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// On-disk format version (forward-compat).
    pub version: u32,
    /// Human-chosen display name for this participant.
    pub name: String,
    /// `sender_id` public key, lowercase hex (64 chars).
    pub identity_id: String,
    /// `device_id` public key, lowercase hex (64 chars).
    pub device_id: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at_ms: u64,
}

impl Profile {
    /// Load the public profile from `<home>/identity.json`.
    ///
    /// Reads **only** the public file; the secret file is never opened.
    ///
    /// # Errors
    /// Returns an actionable error if no identity exists in `home`, if the file
    /// cannot be read, or if it is not valid `identity.json`.
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join(IDENTITY_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                crate::bail_coded!(
                    crate::error::ErrorCode::IdentityNotFound,
                    "no identity in {}",
                    home.display()
                );
            }
            Err(err) => {
                return Err(err).with_context(|| format!("could not read {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "identity files are inconsistent or corrupt: {}",
                path.display()
            )
        })
    }
}

/// The two secret signing keys backing the local identity, loaded for an
/// authoring command (`room create` and later flows).
///
/// The seeds live **only** inside the [`SigningKey`] wrappers (and the
/// `Zeroizing` buffers used to build them, which are wiped). There is
/// deliberately no `Debug`/`Display`/`Serialize`, so a stray `{:?}` or log call
/// cannot leak a seed (spec D4 / §9).
pub struct SecretKeys {
    /// Signs the device binding — authorizes `device_id` under `sender_id`.
    pub identity: SigningKey,
    /// Signs the event itself; the signature verifies under `device_id`.
    pub device: SigningKey,
}

/// On-disk shape of `identity.secret`. Holds the seed hex transiently; both
/// strings are zeroized before this struct is dropped. No derived `Debug`, so
/// the seeds cannot be printed by accident.
#[derive(serde::Deserialize)]
struct SecretFile {
    version: u32,
    identity_secret: String,
    device_secret: String,
}

impl Zeroize for SecretFile {
    fn zeroize(&mut self) {
        self.identity_secret.zeroize();
        self.device_secret.zeroize();
    }
}

impl SecretKeys {
    /// Load the secret seeds from `<home>/identity.secret` and reconstruct the
    /// signing keys.
    ///
    /// After decoding, the keys are cross-checked against the public profile
    /// (`identity.json`): the derived public keys MUST equal the stored
    /// `identity_id` / `device_id`. A mismatch (tamper or partial corruption) is
    /// a hard error — the same guard the public side applies (#16 §8).
    ///
    /// # Errors
    /// Returns an actionable error if no identity exists in `home` (pointing at
    /// `iroh-rooms identity create`), if the secret file is unreadable, or if it
    /// is corrupt / inconsistent with the public profile. No secret bytes ever
    /// appear in an error message.
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join(SECRET_FILE);
        let mut bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                crate::bail_coded!(
                    crate::error::ErrorCode::IdentityNotFound,
                    "no identity in {}",
                    home.display()
                );
            }
            Err(err) => {
                return Err(err).with_context(|| format!("could not read {}", path.display()));
            }
        };

        // Parse, then wipe the raw file bytes (they hold the seed hex).
        let parsed: Result<SecretFile> = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "identity files are inconsistent or corrupt: {}",
                path.display()
            )
        });
        bytes.zeroize();
        let mut parsed = parsed?;

        let keys = Self::from_secret_file(&parsed).with_context(|| {
            format!(
                "identity files are inconsistent or corrupt: {}",
                path.display()
            )
        });
        parsed.zeroize();
        let keys = keys?;

        // Consistency guard: the secret seeds must reproduce the public profile.
        let profile = Profile::load(home)?;
        if keys.identity.identity_key().to_string() != profile.identity_id
            || keys.device.device_key().to_string() != profile.device_id
        {
            bail!(
                "identity files are inconsistent or corrupt: {} (secret keys do not match \
                 identity.json)",
                home.display()
            );
        }
        Ok(keys)
    }

    /// Decode both seeds into signing keys, validating the file version. Secret
    /// intermediates are held in `Zeroizing` and wiped on drop.
    fn from_secret_file(file: &SecretFile) -> Result<Self> {
        if file.version != PROFILE_VERSION {
            bail!("unsupported identity.secret version {}", file.version);
        }
        Ok(Self {
            identity: signing_key_from_seed_hex(&file.identity_secret)?,
            device: signing_key_from_seed_hex(&file.device_secret)?,
        })
    }
}

/// Decode a 32-byte secret seed from lowercase hex into a [`SigningKey`]. The
/// decoded seed bytes are held in a `Zeroizing` buffer and wiped before return;
/// no secret bytes appear in any error.
fn signing_key_from_seed_hex(seed_hex: &str) -> Result<SigningKey> {
    let mut raw = hex::decode(seed_hex).map_err(|_| anyhow!("secret seed is not valid hex"))?;
    let key = if let Ok(seed) = <[u8; SEED_LEN]>::try_from(raw.as_slice()) {
        let seed = Zeroizing::new(seed);
        SigningKey::from_seed(&seed)
    } else {
        raw.zeroize();
        bail!("secret seed has the wrong length (expected {SEED_LEN} bytes)");
    };
    raw.zeroize();
    Ok(key)
}

/// Create a new identity (and device) keypair under `home`, persisting both the
/// public profile and the secret seeds.
///
/// Without `force`, refuses to clobber an existing identity. With `force`,
/// atomically replaces it after printing a loud warning to stderr — the previous
/// keys are local-only and irrecoverable once replaced.
///
/// Returns the public [`Profile`] for the caller to print.
///
/// # Errors
/// Fails on an invalid name, when an identity already exists and `force` is not
/// set, or when the files cannot be written with the required permissions.
pub fn create(home: &Path, name: &str, force: bool) -> Result<Profile> {
    // Validate before touching the filesystem so a bad name writes nothing —
    // not even the home directory.
    validate_name(name)?;
    crate::paths::ensure_dir(home)?;

    let identity_path = home.join(IDENTITY_FILE);
    let secret_path = home.join(SECRET_FILE);

    if force {
        // Loud, explicit warning before overwriting irrecoverable local-only keys.
        eprintln!(
            "warning: --force replaces the identity at {}; the current keys and any room \
             membership bound to them are permanently discarded (local-first: there is no \
             server copy to recover from)",
            home.display()
        );
    } else {
        let identity_exists = identity_path.exists();
        let secret_exists = secret_path.exists();
        if identity_exists || secret_exists {
            let detail = if identity_exists && secret_exists {
                "an identity already exists"
            } else {
                "an identity already exists (incomplete: only one of identity.json / \
                 identity.secret is present)"
            };
            bail!(
                "{detail} at {}; pass --force to replace it (permanently discards the current \
                 keys and any room membership bound to them)",
                home.display()
            );
        }
    }

    // Generate both keypairs. Secret bytes live only inside the `SigningKey`
    // wrappers here and the `Zeroizing` seed buffers in `secret_file_contents`.
    let identity_key = SigningKey::generate();
    let device_key = SigningKey::generate();

    let profile = Profile {
        version: PROFILE_VERSION,
        name: name.to_owned(),
        identity_id: identity_key.identity_key().to_string(),
        device_id: device_key.device_key().to_string(),
        created_at_ms: crate::clock::now_ms(),
    };
    let profile_json = serde_json::to_vec(&profile).context("could not encode identity.json")?;

    // The only secret-bearing buffer; wiped before this function returns.
    let mut secret_json = secret_file_contents(&identity_key, &device_key);

    // Write the secret file first, then the public profile (spec D5). `force`
    // replaces atomically (temp + rename); the default path creates exclusively
    // so a concurrent create can never be clobbered.
    let write_result = if force {
        atomic_write_owner_only(&secret_path, secret_json.as_bytes())
            .and_then(|()| atomic_write_owner_only(&identity_path, &profile_json))
    } else {
        create_new_owner_only(&secret_path, secret_json.as_bytes())
            .and_then(|()| create_new_owner_only(&identity_path, &profile_json))
    };
    secret_json.zeroize();

    write_result
        .with_context(|| format!("could not write identity files to {}", home.display()))?;

    Ok(profile)
}

/// Print `show` output: labeled `key: value` lines by default (script-friendly,
/// deterministic order), or a single-line JSON object with `--json` (spec D7).
///
/// # Errors
/// Fails only if JSON encoding fails (it cannot, for this struct).
pub fn print_show(profile: &Profile, json: bool) -> Result<()> {
    if json {
        let line = serde_json::to_string(profile).context("could not encode identity as JSON")?;
        println!("{line}");
    } else {
        println!("name: {}", profile.name);
        println!("identity_id: {}", profile.identity_id);
        println!("device_id: {}", profile.device_id);
    }
    Ok(())
}

/// Validate a profile name: 1..=64 UTF-8 bytes, no control characters (so it
/// stays clean in `show` output and future `display_name` event content).
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name must not be empty");
    }
    let len = name.len();
    if len > MAX_NAME_BYTES {
        bail!("name must be at most {MAX_NAME_BYTES} bytes (got {len})");
    }
    if name.chars().any(char::is_control) {
        bail!("name must not contain control characters (newline, tab, etc.)");
    }
    Ok(())
}

/// Build the `identity.secret` file body, hex-encoding both seeds. The returned
/// `String` is secret-bearing — the caller must `zeroize` it after writing.
fn secret_file_contents(identity_key: &SigningKey, device_key: &SigningKey) -> String {
    let identity_seed = identity_key.to_seed();
    let device_seed = device_key.to_seed();
    // `.as_slice()` borrows the zeroizing buffer in place — no `Copy` of the seed.
    let mut identity_hex = hex::encode(identity_seed.as_slice());
    let mut device_hex = hex::encode(device_seed.as_slice());
    let contents = format!(
        "{{\"version\":{PROFILE_VERSION},\"identity_secret\":\"{identity_hex}\",\
         \"device_secret\":\"{device_hex}\"}}\n"
    );
    // Wipe the intermediate hex copies; `contents` is wiped by the caller.
    identity_hex.zeroize();
    device_hex.zeroize();
    contents
}

/// Create `path` exclusively (failing if it exists) with owner-only permissions,
/// and write `bytes`. On Unix the file is created already `0600` — never
/// world-readable-then-chmod (spec D6).
fn create_new_owner_only(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_new_owner_only(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Atomically replace `path` with `bytes`: write to a sibling `*.tmp` created
/// `0600`, then rename over the target (rename preserves the temp's mode and is
/// atomic on the same filesystem).
fn atomic_write_owner_only(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    // Clear a leftover temp from a previously interrupted run so the exclusive
    // create below succeeds.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    {
        let mut file = open_new_owner_only(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn open_new_owner_only(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── name validation ─────────────────────────────────────────────────────

    #[test]
    fn validate_name_rejects_empty_string() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_65_byte_ascii_string() {
        let too_long = "a".repeat(MAX_NAME_BYTES + 1);
        let err = validate_name(&too_long).unwrap_err();
        assert!(
            err.to_string().contains(&MAX_NAME_BYTES.to_string()),
            "error must mention the byte limit"
        );
    }

    #[test]
    fn validate_name_accepts_exactly_64_bytes() {
        let max = "a".repeat(MAX_NAME_BYTES);
        assert!(validate_name(&max).is_ok());
    }

    #[test]
    fn validate_name_rejects_newline() {
        assert!(validate_name("Alice\nEve").is_err());
    }

    #[test]
    fn validate_name_rejects_tab() {
        assert!(validate_name("Alice\tEve").is_err());
    }

    #[test]
    fn validate_name_accepts_unicode_within_byte_limit() {
        // "é" is 2 UTF-8 bytes — 30 copies = 60 bytes, under the 64-byte limit.
        let name = "é".repeat(30);
        assert!(validate_name(&name).is_ok());
    }

    #[test]
    fn validate_name_rejects_unicode_over_byte_limit() {
        // "é" = 2 bytes × 33 = 66 bytes — over the 64-byte limit.
        let name = "é".repeat(33);
        assert!(validate_name(&name).is_err());
    }

    #[test]
    fn validate_name_accepts_single_byte_name() {
        // Lower boundary: a one-byte non-control ASCII character must pass.
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn validate_name_rejects_nul_byte() {
        // NUL (\0) is a C0 control character; is_control() returns true for it.
        assert!(validate_name("\0").is_err());
    }

    // ── create happy path ───────────────────────────────────────────────────

    #[test]
    fn create_returns_profile_with_given_name() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        assert_eq!(profile.name, "Alice");
    }

    #[test]
    fn create_sets_profile_version_to_1() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        assert_eq!(profile.version, PROFILE_VERSION);
    }

    #[test]
    fn create_identity_and_device_ids_are_distinct() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        assert_ne!(
            profile.identity_id, profile.device_id,
            "sender_id and device_id must be distinct keys (spec §1)"
        );
    }

    #[test]
    fn create_ids_are_64_char_lowercase_hex() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        for (label, id) in [
            ("identity_id", &profile.identity_id),
            ("device_id", &profile.device_id),
        ] {
            assert_eq!(id.len(), 64, "{label} must be 64 hex chars (32 bytes)");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{label} must be lowercase hex"
            );
        }
    }

    #[test]
    fn create_writes_identity_json_and_secret_file() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        assert!(
            dir.path().join(IDENTITY_FILE).exists(),
            "identity.json must exist"
        );
        assert!(
            dir.path().join(SECRET_FILE).exists(),
            "identity.secret must exist"
        );
    }

    #[test]
    fn create_created_at_ms_is_nonzero() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        assert!(
            profile.created_at_ms > 0,
            "created_at_ms must be a real epoch timestamp"
        );
    }

    // ── create guard (no force) ─────────────────────────────────────────────

    #[test]
    fn create_guard_rejects_second_create_without_force() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        let err = create(dir.path(), "Alice2", false).unwrap_err();
        assert!(
            err.to_string().contains("--force"),
            "error must mention --force flag: {err}"
        );
    }

    #[test]
    fn create_guard_error_when_only_public_file_present() {
        let dir = tempdir().unwrap();
        crate::paths::ensure_dir(dir.path()).unwrap();
        // Simulate partial state: only identity.json written (e.g. interrupted run).
        std::fs::write(dir.path().join(IDENTITY_FILE), b"{}").unwrap();
        let err = create(dir.path(), "Alice", false).unwrap_err();
        assert!(
            err.to_string().contains("incomplete"),
            "error must mention incomplete state: {err}"
        );
    }

    #[test]
    fn create_guard_error_when_only_secret_file_present() {
        let dir = tempdir().unwrap();
        crate::paths::ensure_dir(dir.path()).unwrap();
        // Simulate partial state: only identity.secret written.
        std::fs::write(dir.path().join(SECRET_FILE), b"{}").unwrap();
        let err = create(dir.path(), "Alice", false).unwrap_err();
        assert!(
            err.to_string().contains("incomplete"),
            "error must mention incomplete state: {err}"
        );
    }

    #[test]
    fn create_invalid_name_writes_no_files() {
        let dir = tempdir().unwrap();
        // Empty name is invalid; nothing should be written.
        let _ = create(dir.path(), "", false);
        assert!(
            !dir.path().join(IDENTITY_FILE).exists(),
            "identity.json must not be written when name is invalid"
        );
        assert!(
            !dir.path().join(SECRET_FILE).exists(),
            "identity.secret must not be written when name is invalid"
        );
    }

    // ── create with force ───────────────────────────────────────────────────

    #[test]
    fn create_force_replaces_existing_identity() {
        let dir = tempdir().unwrap();
        let first = create(dir.path(), "Alice", false).unwrap();
        let second = create(dir.path(), "Bob", true).unwrap();
        assert_eq!(second.name, "Bob");
        // Force must generate fresh keys, not reuse the previous ones.
        assert_ne!(
            first.identity_id, second.identity_id,
            "force must generate a new identity key"
        );
    }

    #[test]
    fn create_force_succeeds_on_fresh_directory() {
        let dir = tempdir().unwrap();
        // --force on a clean directory must succeed (guard bypass is not required).
        let profile = create(dir.path(), "Alice", true).unwrap();
        assert_eq!(profile.name, "Alice");
        assert!(dir.path().join(IDENTITY_FILE).exists());
        assert!(dir.path().join(SECRET_FILE).exists());
    }

    #[test]
    fn create_force_leaves_no_tmp_residue() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        create(dir.path(), "Bob", true).unwrap();
        // atomic_write_owner_only must not leave leftover .tmp files after success.
        assert!(
            !dir.path().join(format!("{IDENTITY_FILE}.tmp")).exists(),
            "identity.json.tmp must not remain after a successful force create"
        );
        assert!(
            !dir.path().join(format!("{SECRET_FILE}.tmp")).exists(),
            "identity.secret.tmp must not remain after a successful force create"
        );
    }

    #[test]
    fn create_force_recovers_from_leftover_tmp_file() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        // Simulate a leftover .tmp from a previously interrupted force run.
        std::fs::write(dir.path().join(format!("{SECRET_FILE}.tmp")), b"stale").unwrap();
        // A subsequent force create must remove the stale .tmp and succeed.
        let profile = create(dir.path(), "Bob", true).unwrap();
        assert_eq!(profile.name, "Bob");
    }

    #[test]
    fn create_at_ms_nondecreasing_across_sequential_creates() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let a = create(dir_a.path(), "Alice", false).unwrap();
        let b = create(dir_b.path(), "Bob", false).unwrap();
        assert!(
            b.created_at_ms >= a.created_at_ms,
            "created_at_ms must be non-decreasing across sequential creates \
             (got a={}, b={})",
            a.created_at_ms,
            b.created_at_ms
        );
    }

    // ── Profile::load ───────────────────────────────────────────────────────

    #[test]
    fn load_missing_identity_returns_actionable_error() {
        let dir = tempdir().unwrap();
        let err = Profile::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("identity create") || msg.contains("no identity"),
            "error must hint at the create command: {msg}"
        );
    }

    #[test]
    fn load_roundtrips_profile_written_by_create() {
        let dir = tempdir().unwrap();
        let created = create(dir.path(), "Alice", false).unwrap();
        let loaded = Profile::load(dir.path()).unwrap();
        assert_eq!(loaded.name, created.name);
        assert_eq!(loaded.identity_id, created.identity_id);
        assert_eq!(loaded.device_id, created.device_id);
        assert_eq!(loaded.version, created.version);
    }

    #[test]
    fn load_rejects_corrupt_identity_json() {
        let dir = tempdir().unwrap();
        crate::paths::ensure_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join(IDENTITY_FILE), b"not valid json").unwrap();
        assert!(Profile::load(dir.path()).is_err());
    }

    // ── secret isolation ────────────────────────────────────────────────────

    #[test]
    fn identity_json_does_not_contain_secret_field_names() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        let json = std::fs::read_to_string(dir.path().join(IDENTITY_FILE)).unwrap();
        assert!(
            !json.contains("identity_secret"),
            "identity.json must not contain the secret seed field"
        );
        assert!(
            !json.contains("device_secret"),
            "identity.json must not contain the device secret field"
        );
    }

    #[test]
    fn secret_file_contains_both_seed_fields() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        let secret = std::fs::read_to_string(dir.path().join(SECRET_FILE)).unwrap();
        assert!(
            secret.contains("identity_secret"),
            "identity.secret must contain identity_secret field"
        );
        assert!(
            secret.contains("device_secret"),
            "identity.secret must contain device_secret field"
        );
    }

    // ── SecretKeys::load ────────────────────────────────────────────────────

    #[test]
    fn secret_keys_load_missing_secret_file_returns_actionable_error() {
        let dir = tempdir().unwrap();
        let err = SecretKeys::load(dir.path())
            .err()
            .expect("expected load to fail when no identity exists");
        let msg = err.to_string();
        assert!(
            msg.contains("identity create") || msg.contains("no identity"),
            "error must hint at the create command: {msg}"
        );
    }

    #[test]
    fn secret_keys_load_roundtrips_keys_written_by_create() {
        let dir = tempdir().unwrap();
        let profile = create(dir.path(), "Alice", false).unwrap();
        let secret = SecretKeys::load(dir.path()).unwrap();
        assert_eq!(
            secret.identity.identity_key().to_string(),
            profile.identity_id,
            "loaded identity key must reproduce the profile identity_id"
        );
        assert_eq!(
            secret.device.device_key().to_string(),
            profile.device_id,
            "loaded device key must reproduce the profile device_id"
        );
    }

    #[test]
    fn secret_keys_load_corrupt_json_returns_error() {
        let dir = tempdir().unwrap();
        crate::paths::ensure_dir(dir.path()).unwrap();
        // Write identity.json first so Profile::load won't short-circuit on
        // a missing file before we get to the secret-file parse error.
        create(dir.path(), "Alice", false).unwrap();
        // Overwrite the secret file with invalid JSON.
        std::fs::write(dir.path().join(SECRET_FILE), b"not valid json").unwrap();
        let err = SecretKeys::load(dir.path())
            .err()
            .expect("expected load to fail with corrupt JSON");
        let msg = err.to_string();
        assert!(
            msg.contains("inconsistent or corrupt") || msg.contains("not valid"),
            "corrupt JSON must produce an error: {msg}"
        );
    }

    #[test]
    fn secret_keys_load_invalid_hex_seed_returns_error() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        // Overwrite with a structurally valid JSON but non-hex seeds.
        std::fs::write(
            dir.path().join(SECRET_FILE),
            b"{\"version\":1,\"identity_secret\":\"gggg\",\"device_secret\":\"hhhh\"}\n",
        )
        .unwrap();
        let err = SecretKeys::load(dir.path())
            .err()
            .expect("expected load to fail with non-hex seeds");
        assert!(
            err.to_string().contains("inconsistent or corrupt") || err.to_string().contains("hex"),
            "bad hex must produce an error: {err}"
        );
    }

    #[test]
    fn secret_keys_load_wrong_seed_length_returns_error() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        // 31-byte seed (62 hex chars) — one byte short.
        let short_seed = "ab".repeat(31);
        let body = format!(
            "{{\"version\":1,\"identity_secret\":\"{short_seed}\",\
             \"device_secret\":\"{short_seed}\"}}\n"
        );
        std::fs::write(dir.path().join(SECRET_FILE), body.as_bytes()).unwrap();
        let err = SecretKeys::load(dir.path())
            .err()
            .expect("expected load to fail with too-short seed");
        assert!(
            err.to_string().contains("inconsistent or corrupt")
                || err.to_string().contains("wrong length"),
            "short seed must produce an error: {err}"
        );
    }

    #[test]
    fn secret_keys_load_mismatch_against_public_profile_returns_error() {
        // Write Alice's identity in dir_a, then overwrite dir_a's identity.json
        // with Bob's public keys while keeping Alice's secret file. The
        // consistency guard must catch the mismatch.
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        create(dir_a.path(), "Alice", false).unwrap();
        let profile_b = create(dir_b.path(), "Bob", false).unwrap();

        // Replace Alice's identity.json with Bob's public profile.
        let bob_json = serde_json::to_string(&profile_b).unwrap();
        std::fs::write(dir_a.path().join(IDENTITY_FILE), bob_json.as_bytes()).unwrap();

        // Alice's secret file + Bob's public profile — keys won't match.
        let err = SecretKeys::load(dir_a.path())
            .err()
            .expect("expected load to fail when secret/public keys mismatch");
        assert!(
            err.to_string().contains("inconsistent or corrupt"),
            "key mismatch must be caught by the consistency guard: {err}"
        );
    }

    #[test]
    fn secret_keys_load_unsupported_version_returns_error() {
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        let valid_seed = "01".repeat(32);
        let body = format!(
            "{{\"version\":99,\"identity_secret\":\"{valid_seed}\",\
             \"device_secret\":\"{valid_seed}\"}}\n"
        );
        std::fs::write(dir.path().join(SECRET_FILE), body.as_bytes()).unwrap();
        let err = SecretKeys::load(dir.path())
            .err()
            .expect("expected load to fail with unsupported version");
        assert!(
            err.to_string().contains("unsupported") || err.to_string().contains("inconsistent"),
            "unknown version must produce an error: {err}"
        );
    }

    // ── Unix file permissions ───────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn create_files_have_0600_permissions() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        for name in &[IDENTITY_FILE, SECRET_FILE] {
            let mode = std::fs::metadata(dir.path().join(name)).unwrap().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{name} must be owner-read/write only (0600), got {mode:o}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_home_dir_has_0700_permissions() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir().unwrap();
        create(dir.path(), "Alice", false).unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "home directory must be owner-only (0700), got {mode:o}"
        );
    }
}
