# Installation, Uninstallation, And Rollback

Status: Phase 2.5 Production Beta runbook.

This runbook is the concrete install/remove/rollback surface referenced by
`docs/operations/release-operations.md` and Production Beta release notes.

## Supported Platforms

Production Beta supports:

- macOS developer machines.
- Linux developer machines.

Binary artifacts are supported only for the exact platform triples listed in
the release notes with SHA-256 checksums. Other users should build from source
or wait for a matching artifact.

Windows is not a Production Beta target unless a specific release candidate
adds and verifies a Windows artifact.

## Install From Source

Prerequisites:

- Rust toolchain compatible with the workspace and locked dependencies.
- `git`.

Install:

```bash
git clone https://github.com/kortiene/iroh-room.git
cd iroh-room
git checkout <RELEASE_TAG_OR_COMMIT>
cargo build -p iroh-rooms-cli --release
install -m 0755 target/release/iroh-rooms "$HOME/.local/bin/iroh-rooms"
iroh-rooms --version
```

If `$HOME/.local/bin` is not on `PATH`, either add it or choose another
user-writable install directory.

## Install From Binary Artifact

Before installing:

1. Download the archive and adjacent `.sha256` file from the release.
2. Verify the checksum.

macOS:

```bash
shasum -a 256 -c iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz.sha256
tar -xzf iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz
install -m 0755 iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>/iroh-rooms \
  "$HOME/.local/bin/iroh-rooms"
iroh-rooms --version
```

Linux:

```bash
sha256sum -c iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz.sha256
tar -xzf iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz
install -m 0755 iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>/iroh-rooms \
  "$HOME/.local/bin/iroh-rooms"
iroh-rooms --version
```

Production Beta artifacts are checksummed. They are not project-signed unless
the release notes explicitly say signing has shipped.

## Configure Local Data

The CLI uses a platform-default data directory unless `IROH_ROOMS_HOME` is set.
For repeatable beta testing, prefer an explicit directory:

```bash
export IROH_ROOMS_HOME="$HOME/.local/share/iroh-rooms-beta"
mkdir -p "$IROH_ROOMS_HOME"
```

The data directory can contain identity secrets, invite tickets in shell
history, `rooms.db`, blobs, and `audit.ndjson`. Treat it as sensitive local
state.

## Back Up Before Upgrade

Before installing a new Production Beta candidate:

```bash
export IROH_ROOMS_HOME="${IROH_ROOMS_HOME:-$HOME/.local/share/iroh-rooms-beta}"
tar -C "$(dirname "$IROH_ROOMS_HOME")" \
  -czf "iroh-rooms-backup-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" \
  "$(basename "$IROH_ROOMS_HOME")"
```

Do not attach this backup to public issues. It may contain private identities,
room logs, blob bytes, and audit events.

## Uninstall Binary

Remove the binary:

```bash
rm -f "$HOME/.local/bin/iroh-rooms"
```

If installed elsewhere, remove that path instead.

## Remove Local Data

This deletes local room state and identity secrets. Only run it when you have
exported or backed up anything you need.

```bash
rm -rf "$IROH_ROOMS_HOME"
```

If `IROH_ROOMS_HOME` was not set, locate the platform-default data directory
with the release notes or `iroh-rooms identity show` output before deleting.

## Rollback

Rollback requires:

1. The previous binary or source commit.
2. A pre-upgrade data backup.
3. Release notes that say whether the new candidate performed an incompatible
   migration.

Rollback steps:

```bash
# Stop all iroh-rooms commands first.
rm -f "$HOME/.local/bin/iroh-rooms"
install -m 0755 <PREVIOUS_IROH_ROOMS_BINARY> "$HOME/.local/bin/iroh-rooms"

# Restore data only if the release notes require it or the old binary cannot
# read the upgraded store.
rm -rf "$IROH_ROOMS_HOME"
tar -xzf <PRE_UPGRADE_BACKUP>.tar.gz -C "$(dirname "$IROH_ROOMS_HOME")"

iroh-rooms --version
iroh-rooms identity show
```

If rollback fails after a schema migration, stop using the data directory and
restore the backup again with the previous binary.
