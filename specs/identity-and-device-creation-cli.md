# Spec: Identity and Device Creation CLI (IR-0101)

| | |
|---|---|
| **Issue** | #16 — `[IR-0101] Implement identity and device creation CLI` |
| **Parent epic** | #2 |
| **Labels** | `type/feature` `type/security` `area/protocol` `area/cli` `priority/p0` `risk/medium` |
| **Dependencies** | #6 (IR-0002, canonical signed event model — **landed**), #8 (IR-0004, SQLite event store — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.1, §16, §13.1; Spike `PHASE-0-SPIKE.md` Event Protocol §1 (Identity and Key Model) |
| **Owning crate** | `crates/iroh-rooms-cli` (app surface) + a small additive primitive in `crates/iroh-rooms-core::event::keys` |

> **Status:** implemented and landed (issue #16 / IR-0101). All steps in §10 are
> complete. `scripts/verify.sh` is green. The binary is the source of truth for any
> detail that diverges from this spec.

---

## 1. Summary

Add the first real subcommand group to the `iroh-rooms` binary: **local identity and
device creation**. Today the binary is a scaffold that prints a version string
(`crates/iroh-rooms-cli/src/main.rs`). This work makes the binary:

- `iroh-rooms identity create --name <name>` — generate a participant **identity**
  keypair (`sender_id`) and a **device** keypair (`device_id`), store them and the
  profile name locally under a per-host data directory with owner-only file
  permissions, and refuse to clobber an existing identity unless `--force` is given.
- `iroh-rooms identity show` — print the identity ID, device ID, and profile name in a
  **script-friendly** form, never touching or printing secret key material.

The cryptographic types already exist and are tested: `IdentityKey` / `DeviceKey` /
`SigningKey` in `iroh-rooms-core::event::keys` (issue #6). This issue is about (a) a
tiny additive **key-generation** primitive on `SigningKey`, and (b) the **CLI + on-disk
persistence + file-permission** layer around it. It does **not** create rooms, events,
or device-binding certificates — those are room-scoped and land in sibling issues.

This is the first node of the CLI command tree the PRD §16 surface hangs off; the data
directory and identity-loading helpers introduced here are reused by every later
command (`room create`, `room join`, `room send`, …), so the boundaries chosen here
matter beyond this issue.

---

## 2. Background & current repository state

**What exists (relevant landed work):**

- `iroh-rooms-core::event::keys` (issue #6 / IR-0002): `IdentityKey` and `DeviceKey`
  are distinct 32-byte Ed25519 public-key newtypes (`sender_id` vs `device_id`);
  `SigningKey` wraps an Ed25519 secret with `from_seed(&[u8;32])`, `public_bytes()`,
  `device_key()`, `identity_key()`, `sign()`. `SigningKey`'s `Debug` is **already
  redacted** (`SigningKey(<redacted>)`). `IdentityKey`/`DeviceKey` `Display` is
  **lowercase hex (64 chars)** — exactly the CLI presentation the spike mandates
  (Event Protocol §2: "human/CLI/JSON presentation is lowercase hex (64 chars)").
- `iroh-rooms-core::store` (issue #8 / IR-0004, `store` feature): `EventStore::open(path)`
  opens a SQLite DB at a filesystem path. Establishes that per-host room state lives in a
  data directory. The identity files specified here live in that **same** data directory,
  which is the substantive reason #8 is listed as a dependency (the home-resolution
  convention is shared). This issue does **not** open or write the store.
- `crates/iroh-rooms-cli`: a single `main.rs` that prints
  `iroh-rooms CLI scaffold v{PROTOCOL_VERSION}`. Its only dependency is
  `iroh-rooms-core`. No argument parser yet.
- `docs/getting-started.md` (issue #35) already **documents this exact command surface**
  ahead of implementation: `identity create --name "Alice"`, `identity show`, and a
  per-participant data directory selected via **`IROH_ROOMS_HOME`** (env) and/or
  **`--data-dir`** (flag). Its conformance test (`tests/docs_conformance.rs`) asserts the
  guide must mention `IROH_ROOMS_HOME` or `--data-dir`, that `identity show` prints an ID
  usable as `<BOB_ID>`/`<AGENT_ID>` (the `sender_id`), and that identity creation requires
  **no central account**. The guide flags the data-dir override name and exact flags as
  `[reconcile]` — **this issue is where they are reconciled, and the merged binary becomes
  the source of truth.**

**Spike facts that constrain the design (Event Protocol §1):**

- `sender_id` (identity) and `device_id` (device) are **distinct Ed25519 keys even in
  MVP**. Authorization/membership track `sender_id`; events are signed by `device_id`.
- MVP allows exactly **one `device_id` per `sender_id`** (multi-device out of scope) ⇒
  one identity + one device key per data directory.
- The `device_id`↔`sender_id` **binding certificate** is **room-scoped**
  (`binding_msg = BIND_CONTEXT ‖ room_id ‖ sender_id ‖ device_id`) and is produced inside
  `room.created` / `member.joined` / `member.removed` content — so it **cannot** be
  precomputed at identity-creation time (no room yet). Binding generation is therefore
  **out of scope** here; this issue only persists the two keypairs + name.
- An "agent" is an ordinary principal with its own `sender_id`/`device_id`; nothing
  identity-specific distinguishes it at creation time. The same `identity create` path
  serves humans and agents (the getting-started demo creates a `build-agent` identity the
  same way).

**Workspace conventions to honor:**

- Lints: `unsafe_code = "forbid"`, `clippy::all`+`clippy::pedantic = warn` (workspace).
  All new code must be clippy-pedantic clean.
- `scripts/verify.sh` runs fmt + clippy + tests `--all-features` across the workspace and
  is the CI gate.
- MSRV `rust-version = "1.80"`.
- Deps are added deliberately and documented with a comment explaining the pin/choice
  (see the heavily-commented `iroh-rooms-core/Cargo.toml`).

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. `SigningKey::generate()` additive primitive in `iroh-rooms-core::event::keys` (CSPRNG
   seed → `from_seed`), so secret generation lives behind the existing typed wrapper.
2. `iroh-rooms-cli` argument parser (clap) with an `identity` subcommand group:
   `identity create --name <name> [--force]` and `identity show [--json]`.
3. Data-directory resolution: `--data-dir <path>` flag > `IROH_ROOMS_HOME` env > platform
   default. Create the directory tree on first use with owner-only permissions.
4. On-disk identity persistence: a public profile file + a secret-key file, written with
   **0600** file mode and **0700** directory mode on Unix; best-effort elsewhere.
5. Overwrite protection: `create` refuses when an identity already exists, unless
   `--force` is supplied (atomic replace with a loud warning).
6. `identity show`: script-friendly output of `name`, `identity_id`, `device_id` (human
   labeled lines by default; `--json` for machine consumption). Never reads or prints
   secret material.
7. Tests: CLI integration tests against a temp `IROH_ROOMS_HOME` (Test Plan in §11).

### 3.2 Out of scope (sibling issues — do **not** implement here)

- Rooms, events, `room.created`, or any device-binding certificate generation
  (room-scoped; §1 of the spike). — later issues under epic #2.
- Opening/initializing the SQLite event store (`rooms.db`) — IR-0004 is landed but this
  command does not touch it.
- Invite tickets, joining, membership, networking/iroh transport.
- Key **rotation**, multi-device, recovery phrase, encrypted-at-rest storage — explicitly
  deferred by PRD §13.4 (not in MVP) and §13.5 (security roadmap).
- Interactive TUI prompts beyond a possible `--force` confirmation (see OQ-2).

### 3.3 Why the split is safe

Identity creation depends only on the Ed25519 primitives (landed, #6) and the filesystem.
The room-scoped binding and the event store are not needed to generate and persist a
keypair + name. Keeping persistence in the CLI crate (filesystem + OS permissions are app
concerns, not protocol concerns) means `iroh-rooms-core` stays free of `std::fs`/OS-perms
code and gains only one small, pure, well-tested function (`generate`).

---

## 4. Key design decisions

### D1 — Key generation lives in core as `SigningKey::generate()` (recommended)

Add one additive method to `iroh-rooms-core::event::keys`:

```rust
impl SigningKey {
    /// Generate a fresh signing key from the operating-system CSPRNG.
    #[must_use]
    pub fn generate() -> Self { /* fill 32 random bytes, from_seed, zeroize seed */ }
}
```

Implementation note: fill a `[u8; PUBLIC_KEY_LEN]` seed from the OS CSPRNG, call the
existing `from_seed`, then zeroize the seed buffer. Use the workspace-resolved
`getrandom` (already in `Cargo.lock`) for the entropy source rather than threading a
`rand`/`CryptoRng` generic through the public API — this keeps the signature
parameter-free and avoids coupling the public surface to an `ed25519-dalek`/`rand_core`
version. `getrandom` draws from the same OS CSPRNG `ed25519-dalek`'s own `generate` would.

- **Why core, not CLI:** secret bytes never leave the typed `SigningKey` wrapper. The CLI
  receives a `SigningKey`, never a raw seed it has to hand-roll RNG for. Mirrors the
  existing invariant that `from_seed` is the only constructor.
- **Alternative considered (rejected):** generate the seed in the CLI with `getrandom` and
  call `from_seed`. Works, but spreads secret-handling into the app layer and duplicates
  the zeroize discipline. Core is the natural home for a key-material primitive.
- This is the **only** change to `iroh-rooms-core`; it is purely additive and adds two
  small deps to that crate (`getrandom`, `zeroize`) — both already in the lockfile.

### D2 — Persistence lives in the CLI crate, not core

A new module set in `crates/iroh-rooms-cli/src/` (suggested: `paths.rs`, `identity.rs`,
`cli.rs`) owns data-directory resolution, file formats, and OS permissions. Rationale:
filesystem layout and Unix mode bits are application policy, not protocol. Later CLI
commands (`room`, `file`, `pipe`, `agent`) reuse `paths::data_dir()` and
`identity::load()` from the same crate. (If a future non-CLI consumer needs the on-disk
identity, promote these to a `core` module then — tracked as OQ-4, not now.)

### D3 — Data-directory resolution order (decide and document)

Resolution, highest precedence first:

1. `--data-dir <PATH>` global CLI flag.
2. `IROH_ROOMS_HOME` environment variable.
3. **Platform default** via the `directories` crate's project data dir:
   - Linux: `~/.local/share/iroh-rooms` (honors `XDG_DATA_HOME`)
   - macOS: `~/Library/Application Support/iroh-rooms`
   - Windows: `%APPDATA%\iroh-rooms`

This reconciles the getting-started guide's `[reconcile]` flag: the guide uses
`IROH_ROOMS_HOME` for per-participant isolation (e.g. `.demo/alice`) and `--data-dir` as
an equivalent; both are supported, and the platform default covers the no-config case.
The same resolved directory is where IR-0004's `rooms.db` will live, keeping one home per
participant across all planes.

- **Alternative considered:** a fixed `~/.iroh-rooms`. Rejected: ignores XDG/macOS/Windows
  conventions and still needs a home-dir crate. `directories` is the standard, maintained
  choice and gives correct per-OS paths.

### D4 — On-disk layout: split public profile from secret key material

Under the resolved data dir `<HOME>/` (created `0700` on Unix):

```text
<HOME>/identity.json     # public-ish profile, mode 0600
<HOME>/identity.secret   # the ONLY file holding secret seeds, mode 0600
```

`identity.json` (no secret bytes — safe for `identity show` to read):

```json
{
  "version": 1,
  "name": "Alice",
  "identity_id": "<64-hex sender_id public key>",
  "device_id":   "<64-hex device_id public key>",
  "created_at_ms": 1719772800000
}
```

`identity.secret` (the only secret-bearing file):

```json
{
  "version": 1,
  "identity_secret": "<64-hex 32-byte seed>",
  "device_secret":   "<64-hex 32-byte seed>"
}
```

- **Why split:** `identity show` and every future read-only command open only
  `identity.json` and never touch secret bytes — the common path can't accidentally load,
  log, or leak a seed. The secret file is opened only when an event must actually be
  signed (later issues).
- **Public ids stored redundantly** for a zero-crypto `show`, but on load they are
  **re-derived from the secret and checked** (cheap integrity guard, mirroring the event
  store's BLAKE3 re-derivation on insert). A mismatch is a hard error (tampered/corrupt
  identity) — see §8.
- **Format = JSON via serde** (`serde` + `serde_json`, added to the CLI crate). Rationale:
  stable, ubiquitous, lets `identity show --json` reuse the same `serde` types, and is
  trivially script-parseable. Both files carry a `version` field for forward-compat.
- **Alternative considered:** a single `identity.json` holding everything at `0600`.
  Simpler, but every read path then opens a secret-bearing file. Rejected in favor of the
  split for blast-radius reduction. (Single-file is the fallback if a reviewer prefers
  fewer files — see OQ-1.)

### D5 — Overwrite protection via exclusive create + `--force`

- `create` (no `--force`) writes the secret file with exclusive-create semantics
  (`OpenOptions::create_new(true)` — the OS guarantees failure if the path exists, no
  TOCTOU). If `identity.json` **or** `identity.secret` already exists, fail with an
  actionable message and a non-zero exit.
- `--force` performs an **atomic replace**: write each file to a sibling temp path
  (`*.tmp`) created `0600`, then `rename` over the target (rename preserves the temp's
  mode and is atomic on the same filesystem). Emit a prominent warning to stderr first:
  replacing the identity permanently discards the current keys and any room membership
  bound to them (local-first ⇒ no server copy to recover from).
- A **partial** prior state (one of the two files present) is detected: `create` without
  `--force` reports it as "an identity already exists (possibly incomplete)"; `--force`
  repairs by rewriting both.

### D6 — Owner-only permissions, created without a permissive window

- On Unix: directory `0700`; both files `0600`. Files are created **already restricted** —
  for the non-force path use `OpenOptions::new().write(true).create_new(true)
  .mode(0o600)`; for the force path the temp file is created with `.mode(0o600)` before
  any secret byte is written, then renamed. **Never** create world-readable then
  `chmod` — that leaves a window where the secret is exposed.
- On non-Unix ("where supported", per the issue): `create_new` still gives exclusivity;
  mode bits are a best-effort/no-op and the limitation is documented (§9). Use
  `#[cfg(unix)]` for the `PermissionsExt`/`mode` calls so the crate still builds on
  Windows.

### D7 — `identity show` output contract

Default (human + script-friendly: stable `key: value` lines, lowercase-hex ids, no
decoration, deterministic order):

```text
name: Alice
identity_id: 4f3c…(64 hex)
device_id: a91b…(64 hex)
```

`--json` (single line, machine-consumable; the public profile minus nothing secret):

```json
{"version":1,"name":"Alice","identity_id":"<hex>","device_id":"<hex>","created_at_ms":1719772800000}
```

- "Script-friendly identity and device IDs" (AC) is satisfied two ways: greppable
  `identity_id:` / `device_id:` lines, and `--json`. Both reuse `IdentityKey`/`DeviceKey`
  `Display` for the canonical lowercase-hex form.
- **Nice-to-have (OQ-3):** a `--id identity|device` selector printing just the raw hex
  (no label, no newline-noise) for `$(iroh-rooms identity show --id device)`. Cheap; can
  ship now or defer.
- `show` with no identity present prints an actionable error to stderr ("no identity in
  <HOME>; run `iroh-rooms identity create --name <name>`") and exits non-zero.

### D8 — Secret hygiene in memory

Read secret seeds into a zeroizing buffer (`zeroize::Zeroizing<[u8;32]>` or a `Zeroizing<Vec<u8>>`
from the hex decode) so they are wiped on drop; never place secret bytes in any `Debug`,
`Display`, `format!`, log, or error message. `SigningKey`'s `Debug` is already redacted;
do not add any code that prints its bytes. The serde struct holding secrets must not
derive `Debug` that exposes the seed (use a manual redacted `Debug` or no `Debug`).

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] <COMMAND>

Global options:
  --data-dir <PATH>   Data directory override (else $IROH_ROOMS_HOME, else platform default)

Commands:
  identity create --name <NAME> [--force]
  identity show [--json]
```

- Built with `clap` (derive). The top-level `--data-dir` is a global arg; `IROH_ROOMS_HOME`
  is read as the fallback (either via clap's `env` attribute on `--data-dir` or explicit
  resolution in `paths.rs` — explicit resolution is clearer given the 3-level precedence
  and is recommended).
- `--name <NAME>` is required for `create`. Validation: 1–64 UTF-8 bytes, reject empty and
  reject any control character (newline/CR/tab/etc.) to keep `show` output and future
  `display_name` event content clean (spike §7 `member.joined.display_name` is `opt tstr`;
  the local profile name seeds it later — bound reconciled in OQ-5).
- Keep `main.rs` thin: parse → dispatch to `cli::run()` → map errors to a stderr message +
  non-zero exit. Preserve room for future subcommands (`room`, `file`, `pipe`, `agent`).

---

## 6. Module/file plan (CLI crate)

```text
crates/iroh-rooms-cli/
  Cargo.toml          # + clap (derive), serde (derive), serde_json, directories; dev: assert_cmd, predicates, tempfile
  src/main.rs         # thin: build Cli, call cli::run, print error + exit code
  src/cli.rs          # clap structs/enums; run(cli) dispatch
  src/paths.rs        # data_dir resolution (flag > env > platform default); ensure_dir (0700)
  src/identity.rs     # Identity profile types, create(), load(), show formatting, file perms
  tests/identity_cli.rs   # integration tests (assert_cmd) against a temp IROH_ROOMS_HOME
```

Core change:

```text
crates/iroh-rooms-core/
  Cargo.toml          # + getrandom, zeroize (both already in lockfile)
  src/event/keys.rs   # + SigningKey::generate()
```

---

## 7. Dependencies to add

CLI crate (`iroh-rooms-cli/Cargo.toml`):

| Crate | Why |
|---|---|
| `clap` (`derive`) | Argument parsing for the subcommand tree (PRD §16 surface). |
| `serde` (`derive`) | Serialize/deserialize the identity profile + secret files and `--json` output. |
| `serde_json` | JSON encode/decode for the on-disk files and `--json`. |
| `directories` | Per-OS platform default data dir (D3). |
| `zeroize` | Wipe secret seed buffers in memory (already in lockfile). |
| `anyhow` (dev/prod) | Ergonomic error context in the binary (already in lockfile). |
| `assert_cmd`, `predicates` (dev) | Spawn the built binary and assert stdout/stderr/exit in tests. |
| `tempfile` (dev) | Temp `IROH_ROOMS_HOME` per test (already used in core tests). |

Core crate (`iroh-rooms-core/Cargo.toml`):

| Crate | Why |
|---|---|
| `getrandom` | OS CSPRNG seed for `SigningKey::generate()` (already in lockfile). |
| `zeroize` | Wipe the transient seed buffer in `generate()` (already in lockfile). |

Each addition gets a one-line comment explaining the choice, matching the house style in
`iroh-rooms-core/Cargo.toml`. Pin choices so `scripts/verify.sh --all-features` stays
reproducible. (`anyhow`, `getrandom`, `zeroize`, `tempfile` are already resolved
transitively, so they add no new third-party code to audit.)

---

## 8. Error model & observability

Distinct, actionable, non-zero-exit failures (no secret bytes in any message):

| Condition | Behavior |
|---|---|
| `create` when identity exists, no `--force` | stderr: "identity already exists at `<HOME>`; pass `--force` to replace (permanently discards current keys/membership)"; exit non-zero. |
| `create` partial prior state (one file present) | stderr names the incomplete state; suggests `--force` to repair. |
| `--name` invalid (empty / too long / control char) | stderr explains the bound; exit non-zero; nothing written. |
| Data dir not creatable / not writable | stderr with the path and OS error; exit non-zero. |
| Could not set required permissions (Unix) | stderr; **fail closed** — do not leave a secret file with looser-than-0600 perms. |
| `show` with no identity | stderr: "no identity in `<HOME>`; run `iroh-rooms identity create --name <name>`"; exit non-zero. |
| `identity.json` ids don't match re-derived public keys | stderr: "identity files are inconsistent or corrupt"; exit non-zero (tamper/corruption guard). |

Conventions: success → stdout + exit 0; all errors → stderr + non-zero exit. Use a single
generic non-zero code (1) for MVP; a dedicated "already exists" code is optional (OQ-2).
No telemetry, no logging framework — output is the only observability surface and it is
deliberately minimal and secret-free.

---

## 9. Security, privacy, reliability

- **Never log private keys** (issue security note): secrets exist only inside
  `SigningKey` / a `Zeroizing` buffer; no `Debug`/`Display`/`format!`/error path prints
  them; `identity show` reads only the public file. A regression test greps all command
  output for the secret hex and asserts absence (§11).
- **Owner-only by default** (issue security note): `0600` files, `0700` dir on Unix,
  created restricted (D6) so there is no world-readable window. Best-effort on Windows,
  documented.
- **At-rest threat model (MVP):** secrets are stored **plaintext** under owner-only perms.
  This protects against other local users but **not** against an attacker with the user's
  account or raw disk access. Encrypted-at-rest storage and recovery phrases are explicitly
  **out of MVP** (PRD §13.4) and on the roadmap (PRD §13.5). State this limitation in code
  comments and (optionally) a one-line `create` notice.
- **No central account** (PRD §15.1, asserted by the getting-started conformance test):
  creation is purely local; no network, no registration.
- **Reliability/atomicity:** exclusive create prevents clobbering; `--force` uses
  temp-file + atomic rename so a crash mid-write can't corrupt a live identity into a
  half-written state (worst case: a leftover `*.tmp`, ignored on next run / cleanable).
- **Determinism for tests:** all randomness is confined to `SigningKey::generate()`; every
  other behavior (paths, formatting, perms) is deterministic given `IROH_ROOMS_HOME`.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **Core:** add `SigningKey::generate()` to `event/keys.rs` (D1); add `getrandom` +
   `zeroize` to `iroh-rooms-core/Cargo.toml` with explanatory comments. Add a unit test:
   two `generate()` calls produce different keys; `identity_key() != device_key()` when
   two separate keys are generated; a generated key round-trips sign/verify.
2. **CLI deps:** add `clap`, `serde`, `serde_json`, `directories`, `zeroize` (and dev
   `assert_cmd`, `predicates`, `tempfile`) to `iroh-rooms-cli/Cargo.toml` with comments.
3. **`paths.rs`:** implement `data_dir(flag: Option<&Path>) -> PathBuf` with the D3
   precedence; `ensure_dir(&Path)` that creates the tree and sets `0700` on Unix.
4. **`identity.rs`:**
   - serde types for the public profile and the secret file (secret type: no leaking
     `Debug`; hold seeds in `Zeroizing`).
   - `create(home, name, force)`: validate name; generate two `SigningKey`s; derive
     `identity_id`/`device_id`; write `identity.secret` then `identity.json` using the
     exclusive-create (no force) or temp-write+rename (force) path from D5/D6; emit the
     `--force` warning. Return the public profile for printing.
   - `load(home) -> Profile`: read `identity.json`, re-derive public ids from
     `identity.secret` (or, for `show`, from the stored ids and verify against the secret
     only when needed) and verify consistency; map missing/partial/corrupt to typed errors.
   - `show` formatting: labeled lines (default) and `--json`.
5. **`cli.rs` + `main.rs`:** clap `Cli` with global `--data-dir`; `identity` subcommand
   (`create`/`show`); `run(cli)` dispatch; `main` maps `Err` → stderr + non-zero exit.
6. **Output:** `create` prints a short success summary (the new `identity_id`/`device_id`
   + next-step hint, e.g. "run `iroh-rooms identity show`") — actionable next steps per PRD
   §16 UX req 1. `show` prints per D7.
7. **Tests:** `tests/identity_cli.rs` per §11.
8. **Docs:** none required by the issue, but if any `[reconcile]` detail in
   `docs/getting-started.md` is now settled (data-dir override name, exact flags), note it
   so a follow-up doc PR can drop the `[reconcile]` marker. (Doc change itself is optional
   / separate.)
9. **Gate:** `scripts/verify.sh` green (fmt + clippy pedantic + tests `--all-features`).

---

## 11. Test strategy

CLI integration tests (`crates/iroh-rooms-cli/tests/identity_cli.rs`) using `assert_cmd`
to spawn the built binary, `predicates` for output assertions, and a fresh `tempfile`
directory exported as `IROH_ROOMS_HOME` per test (the issue's Test Plan: "CLI integration
test with temp home directory"). Required cases:

1. **Keypairs generated (AC1, AC2):** `identity create --name Alice` exits 0; both
   `identity.json` and `identity.secret` exist; `identity show` prints a 64-hex
   `identity_id` and a 64-hex `device_id`.
2. **Distinct keys (spike §1):** parsed `identity_id != device_id`.
3. **Profile name stored (AC3):** `show` reports `name: Alice` (and `--json` `"name":"Alice"`).
4. **Script-friendly output (AC4):** default output contains `identity_id:` and `device_id:`
   lines; `--json` parses as JSON with those fields; ids are **stable** across repeated
   `show` calls.
5. **No overwrite without force (AC5):** a second `create --name Bob` (no `--force`) exits
   non-zero, message mentions "exists"/"--force", and the on-disk `identity_id` is
   **unchanged** (assert the bytes did not move).
6. **Force replaces:** `create --name Bob --force` exits 0 and the `identity_id` **changes**.
7. **Permissions (Unix, `#[cfg(unix)]`):** `identity.secret` and `identity.json` are mode
   `0600`; the home dir is `0700`.
8. **Secrets never leak (security note):** capture stdout+stderr of `create` and `show`;
   read the secret seed from `identity.secret`; assert that hex string appears in **neither**
   stream.
9. **Isolation:** two different `IROH_ROOMS_HOME` dirs yield different identities; writing
   one does not affect the other.
10. **`show` with no identity:** non-zero exit, message suggests `identity create`.
11. **Name validation:** empty / >64 / control-char name → non-zero exit, nothing written.
12. **`--data-dir` precedence:** `--data-dir` overrides `IROH_ROOMS_HOME` (point them at two
    dirs and confirm which one gets the files).

Plus a core unit test for `SigningKey::generate()` (step 1). Run under
`scripts/verify.sh --all-features`.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | Secret leaks via logs/errors/Debug. | `Zeroizing` buffers; redacted/no `Debug` on secret types; `show` never opens the secret file; regression test #8 greps output for the seed. |
| R2 | World-readable window between create and chmod. | Create files already-`0600` (`create_new().mode()` / temp+rename), never chmod-after-write (D6). |
| R3 | Accidental identity clobber loses room access irrecoverably (local-first, no server copy). | Exclusive-create default; `--force` required + loud warning; test #5 asserts no-clobber. |
| R4 | Cross-platform perms gap (Windows). | `#[cfg(unix)]` mode code; document best-effort on non-Unix; exclusivity still holds via `create_new`. |
| R5 | New CLI deps (clap/serde/directories) bloat/conflict. | All are standard, MSRV-1.80-compatible; pin and comment; most secret-handling deps already in lockfile. |
| R6 | Data-dir override name diverges from the getting-started guide. | This issue **is** the reconciliation point; choose `--data-dir` + `IROH_ROOMS_HOME` (already what the guide documents) so the binary matches the guide; report any remaining divergence. |
| R7 | Corrupt/partial identity on disk wedges later commands. | Consistency re-derivation guard (§8); partial-state detection in `create`; `--force` repairs. |
| R8 | Profile-name bound diverges from future `member.joined.display_name`. | Pick a conservative 1–64-byte/no-control bound now; flag reconciliation in OQ-5. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Covered by |
|---|---|
| Identity keypair is generated. | D1 `SigningKey::generate()`; `create` derives `identity_id`; test #1. |
| Device keypair is generated. | Second `generate()` → `device_id`; tests #1, #2. |
| Profile name is stored locally. | `identity.json.name` (D4); test #3. |
| `identity show` prints script-friendly identity and device IDs. | D7 labeled lines + `--json`; tests #4. |
| Existing identity is not overwritten without explicit confirmation or force flag. | D5 exclusive-create + `--force`; tests #5, #6. |
| **Security note:** do not log private keys. | D8/§9; test #8. |
| **Security note:** owner-only default permissions. | D6/§9; test #7. |

---

## 14. Dependencies & sequencing

- **Hard prerequisite:** #6 (IR-0002) — landed. Provides `SigningKey`/`IdentityKey`/
  `DeviceKey`. `generate()` is added on top.
- **Soft prerequisite:** #8 (IR-0004) — landed. Shares the data-directory home; not opened
  here. Listed as a dependency so the home-resolution convention introduced here is
  consistent with where the store's `rooms.db` lives.
- **Unblocks:** `room create` / `room join` / `room send` / agent commands — they
  `identity::load()` the keypairs created here and (room create/join) produce the
  room-scoped device binding the spike §1 describes.
- The orchestrator handles all git/GitHub actions; no branch/PR work is part of this phase.

---

## 15. Assumptions

1. One identity (one `sender_id` + one `device_id`) per data directory — matches MVP's
   one-device-per-identity rule (spike §1). Multiple identities = multiple `--data-dir`/
   `IROH_ROOMS_HOME` homes (exactly how the getting-started demo runs Alice/Bob/Agent).
2. Plaintext-at-rest under `0600` is acceptable for MVP (PRD §13.4 excludes encrypted
   storage; §13.5 roadmaps it).
3. JSON is an acceptable on-disk format (vs TOML); both files carry a `version` field for
   forward migration.
4. The binary is invoked as `iroh-rooms` (per `[[bin]] name`); tests drive the compiled
   binary via `assert_cmd`.
5. `directories`/`getrandom`/`zeroize`/`clap`/`serde`/`serde_json` build on MSRV 1.80 and
   pass clippy-pedantic; pins are chosen accordingly.
6. "Agent" identities are created by the same `identity create` path (no agent-specific
   flag at creation; role is assigned later at invite time).

---

## 16. Open questions

- **OQ-1 (layout):** Two files (`identity.json` + `identity.secret`) vs one `0600`
  `identity.json` holding everything. Spec recommends the split (blast-radius); a reviewer
  may prefer the single file for simplicity.
- **OQ-2 (confirmation UX):** Is `--force` sufficient for "explicit confirmation," or
  should `create` also offer an interactive y/N prompt when stdin is a TTY (with
  `--force`/`--yes` to skip)? Spec defaults to `--force` only (non-interactive-friendly).
  Also: dedicated exit code for "already exists" vs generic non-zero?
- **OQ-3 (`--id` selector):** Ship the `identity show --id identity|device` raw-hex
  selector now, or defer? Cheap; recommended now for shell ergonomics.
- **OQ-4 (module home):** Keep identity persistence in the CLI crate, or promote a
  `core::identity` module if a non-CLI consumer appears? CLI-local for now.
- **OQ-5 (name bound):** Final profile-name length/charset bound and its relationship to
  the future `member.joined.display_name` (`opt tstr`, no explicit bound documented in the
  spike). Spec proposes 1–64 UTF-8 bytes, no control chars, pending reconciliation when
  membership events are wired.
- **OQ-6 (platform default):** Confirm `directories` data-dir paths are the desired
  defaults (esp. macOS `Application Support` vs a dotfile), or simplify to `~/.iroh-rooms`.
```
