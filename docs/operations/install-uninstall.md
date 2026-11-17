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

- Rust toolchain 1.91 or newer — the iroh 1.0 stack in the CLI's dependency
  graph declares `rust-version = "1.91"`. Must also be compatible with the
  locked dependencies.
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

`v0.1.0-rc.1` through `v0.1.0-rc.5` publish a per-archive `.sha256` and nothing
else. Those candidates are checksum-only
([`WAIVER-PBETA-0004`](../releases/v0.1.0-rc.5-production-beta-signoff.md)
covered that gap and is retired for `v0.1.0-rc.6`. The release owner approved
that sign-off on 2026-11-16. See
[`v0.1.0-rc.6-production-beta-signoff.md`](../releases/v0.1.0-rc.6-production-beta-signoff.md)).
Verify the signed release with the script below. Pin Actions run
[43835416665](https://github.com/kortiene/iroh-room/actions/runs/43835416665).
An archive whose SHA-256 matches the table in the rc.6 release notes is not
an ungated run 43791394134 byte; those digests do not overlap.

```bash
scripts/verify-release-artifacts.sh --commit bf353b78f63b08ff5e3f425273aa3e0983167679 v0.1.0-rc.6 <TRIPLE>
jq -r .dsseEnvelope.payload <archive>.sigstore.json | base64 -d | jq -r .predicate.runDetails.metadata.invocationId
```

The `jq` line must print
`https://github.com/kortiene/iroh-room/actions/runs/43835416665/attempts/1`.
This jq read only means something after `scripts/verify-release-artifacts.sh`
has verified that same bundle, because reading the payload alone does not check
its signature or provenance.

Releases built by the signing workflow, starting with the first release after
rc.5, publish the following assets for each of the four triples, plus the two
aggregate files:

| Asset | What it is |
| --- | --- |
| `iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz` | The archive. `<COMMIT>` is the first 12 hex digits of the tagged commit. |
| `<archive>.sha256` | One SHA-256 line, two spaces, the bare archive name. |
| `<archive>.sigstore.json` | GitHub build-provenance attestation bundle (SLSA). Verify with `gh attestation verify`. |
| `SHA256SUMS` | The four `.sha256` lines, sorted by filename. |
| `SHA256SUMS.sigstore.json` | Cosign keyless signature bundle over `SHA256SUMS`. Verify with `cosign verify-blob`. |

`<archive>.sigstore.json` and `SHA256SUMS.sigstore.json` are different kinds of
bundle. Pass the archive bundle to `gh attestation verify --bundle`. Pass
`SHA256SUMS.sigstore.json` to `cosign verify-blob --bundle`. The workflow also
attests `SHA256SUMS` itself, and that attestation bundle is not uploaded.
`gh attestation verify SHA256SUMS` reads it from GitHub.

The signer identity for both checks is the workflow on the tag:

```text
https://github.com/kortiene/iroh-room/.github/workflows/release.yml@refs/tags/v<VERSION>
```

The OIDC issuer is `https://token.actions.githubusercontent.com`. There is no
project-held signing key. macOS code signing and notarization are not part of
these artifacts; a browser download may still be quarantined by Gatekeeper.

Verify a signed release with the repository script. It peels
`refs/tags/v<VERSION>` and always passes that commit to `gh attestation verify
--source-digest` and to `cosign verify-blob --certificate-github-workflow-sha`.
`--commit` is optional: when you pass it, it must be that same 40-hex commit,
and the archive name's 12-hex suffix must be its first 12 digits.
`v0.1.0-rc.1` through `v0.1.0-rc.5` fail at the attestation step by design,
after `gh auth status` has succeeded. If `gh` is not logged in, the script
exits 3 and says `gh not authenticated` before it looks at attestations.

```bash
scripts/verify-release-artifacts.sh v<VERSION> <TRIPLE>
# assets already on disk:
scripts/verify-release-artifacts.sh --dir ./assets v<VERSION> <TRIPLE>
# also require the peeled commit to be this SHA:
scripts/verify-release-artifacts.sh --dir ./assets --commit <FULL_SHA> v<VERSION> <TRIPLE>
```

`<TRIPLE>` is `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`, or `aarch64-unknown-linux-gnu`.

The online path needs GitHub CLI **2.68.0 or newer**. That release added
`--source-digest` (and `--source-ref`); `--deny-self-hosted-runners` is older
and the script requires it too. It probes `gh attestation verify --help` and
exits 3 when either flag is missing. The signature check needs **cosign v3 or
newer** (`cosign verify-blob --bundle`). The release workflow installs cosign
v3.1.4. If `cosign` is missing or older after the `gh` checks have passed, the
script exits 3 and says the signature was not checked. Exit 0 means the
checksum, both archive attestation checks, the online `SHA256SUMS`
attestation, and `cosign verify-blob` all passed. `--offline` skips the tag
peel, the GitHub attestation lookup, and cosign, prints
`PARTIAL: online checks skipped`, and exits 1. It is not a successful
verification.

Fetch the assets with `gh` (this needs `gh auth login` on current GitHub CLI
builds for a private lookup; a public release also accepts the curl URLs
below):

```bash
gh release download v<VERSION> -R kortiene/iroh-room \
  -p 'iroh-rooms-<VERSION>-<TRIPLE>-*' \
  -p 'SHA256SUMS*'
```

`iroh-rooms-<VERSION>-<TRIPLE>-*` is the archive, its `.sha256`, and
`<archive>.sigstore.json`. `SHA256SUMS*` is `SHA256SUMS` and
`SHA256SUMS.sigstore.json`. `gh release download` fails the whole command when
a pattern matches nothing, which is what happens for `SHA256SUMS*` on rc.1
through rc.5. Those tags only have the archive and the `.sha256`.

No-login path. `curl` does not use `gh auth`. `gh attestation verify --bundle`
and `cosign verify-blob` read the files you downloaded. They do not call the
GitHub attestation API. The `SHA256SUMS` build-provenance attestation is not
one of those files, so this path does not check it. The online
`gh attestation verify` (no `--bundle`) does, and that command needs
`gh auth login`.

```bash
TAG=v<VERSION>
ARCHIVE=iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz
BASE="https://github.com/kortiene/iroh-room/releases/download/${TAG}"
curl -fsSL -o "$ARCHIVE" "${BASE}/${ARCHIVE}"
curl -fsSL -o "${ARCHIVE}.sha256" "${BASE}/${ARCHIVE}.sha256"
curl -fsSL -o "${ARCHIVE}.sigstore.json" "${BASE}/${ARCHIVE}.sigstore.json"
curl -fsSL -o SHA256SUMS "${BASE}/SHA256SUMS"
curl -fsSL -o SHA256SUMS.sigstore.json "${BASE}/SHA256SUMS.sigstore.json"
```

The same checks by hand, from a directory that contains the assets:

```bash
TAG=v<VERSION>
ARCHIVE=iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz
IDENTITY="https://github.com/kortiene/iroh-room/.github/workflows/release.yml@refs/tags/${TAG}"

# Linux. On macOS use: shasum -a 256 -c "${ARCHIVE}.sha256"
sha256sum -c "${ARCHIVE}.sha256"
sha256sum -c --ignore-missing SHA256SUMS

# Online. Needs `gh auth login` and GitHub CLI 2.68.0 or newer.
gh attestation verify "$ARCHIVE" \
  --repo kortiene/iroh-room \
  --cert-identity "$IDENTITY" \
  --source-ref "refs/tags/${TAG}" \
  --source-digest "<FULL_SHA>" \
  --deny-self-hosted-runners
# No-login. The bundle file is the attestation; this does not call the
# GitHub attestation API.
gh attestation verify "$ARCHIVE" \
  --bundle "${ARCHIVE}.sigstore.json" \
  --repo kortiene/iroh-room \
  --cert-identity "$IDENTITY" \
  --source-ref "refs/tags/${TAG}" \
  --source-digest "<FULL_SHA>" \
  --deny-self-hosted-runners
# Online only. The SHA256SUMS attestation bundle is not a release asset.
gh attestation verify SHA256SUMS \
  --repo kortiene/iroh-room \
  --cert-identity "$IDENTITY" \
  --source-ref "refs/tags/${TAG}" \
  --source-digest "<FULL_SHA>" \
  --deny-self-hosted-runners

cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "$IDENTITY" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-github-workflow-sha "<FULL_SHA>"
```

`<FULL_SHA>` is the commit `refs/tags/${TAG}` peels to. The script looks that
up itself. `shasum` has no `--ignore-missing`; on macOS check the per-archive
`.sha256`, or pass that one line to `shasum -a 256 -c`. The script does that
fallback itself.

Install after the checks succeed.

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
# Written to the current working directory. The archive contains
# identity.secret, so create it owner-only — a default umask would leave it
# group- and world-readable.
( umask 077
  tar -C "$(dirname "$IROH_ROOMS_HOME")" \
    -czf "iroh-rooms-backup-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" \
    "$(basename "$IROH_ROOMS_HOME")" )
```

Note the archive lands in the directory you ran the command from, and its name
carries a timestamp — record the exact filename; the restore and rollback steps
need it.

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
# read the upgraded store. Move the current directory aside rather than
# deleting it: if the backup turns out to be bad or incomplete, deleting first
# destroys the only remaining copy.
mv "$IROH_ROOMS_HOME" "$IROH_ROOMS_HOME.before-rollback"
tar -xzf <PRE_UPGRADE_BACKUP>.tar.gz -C "$(dirname "$IROH_ROOMS_HOME")"
# Once the rolled-back binary reads the restored data correctly, remove
# "$IROH_ROOMS_HOME.before-rollback".

iroh-rooms --version
iroh-rooms identity show
```

If rollback fails after a schema migration, stop using the data directory and
restore the backup again with the previous binary.

## Per-Platform Install, Upgrade, Rollback, And Uninstall

These steps are the operator procedure for the four release triples.
Verification flags, asset names, and the signer identity are in
[Install From Binary Artifact](#install-from-binary-artifact) above. The
backup block in [Back Up Before Upgrade](#back-up-before-upgrade) and the
move-aside restore in [Rollback](#rollback) stay as written.
`scripts/lifecycle-smoke.sh` `documented_backup()` copies that backup block
and cites it.

These commands need `pgrep` (to see whether `iroh-rooms` is running) and
`sqlite3` (`PRAGMA integrity_check` and `PRAGMA wal_checkpoint`).

Do the work in a directory outside the git checkout. `ASSET_DIR` and the
backup tar are created in the working directory. The tar contains
`identity.secret`. Inside the repository both would be untracked files.

```bash
WORK="$(mktemp -d "${TMPDIR:-/tmp}/iroh-rooms-install.XXXXXX")"
cd "$WORK"
```

### Release triples

| Triple | GitHub-hosted runner |
| --- | --- |
| `x86_64-apple-darwin` | `macos-15-intel` |
| `aarch64-apple-darwin` | `macos-15` |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |

`<TRIPLE>` in the commands below is one of those four. Download and install
only the archive whose name contains that triple.

### Install

Download with `curl` or `gh` only, using the commands in Install From Binary
Artifact. Do not use another downloader.

A signed release (`v0.1.0-rc.6` and later) is installed only through
`scripts/verify-release-artifacts.sh`. That command needs GitHub CLI 2.68.0 or
newer and cosign v3 or newer. If it exits non-zero, do not install. Without a
`gh` login the verifier exits 3 and does not check attestations.

`v0.1.0-rc.1` through `v0.1.0-rc.5` are checksum-only (the per-archive
`.sha256`). `scripts/verify-release-artifacts.sh` exits non-zero on those tags
by design (exit 3 when `gh` is not logged in, and non-zero after that when
the attestation is missing). That exit is not a pass and it is not permission
to install. For those tags the gate is `sha256sum -c` on Linux, or
`shasum -a 256 -c` on macOS. If that checksum command exits non-zero, do not
install.

```bash
ASSET_DIR="${ASSET_DIR:-$WORK/assets}"
PREFIX="${PREFIX:-$HOME/.local}"
mkdir -p "${PREFIX}/bin" "$ASSET_DIR"
ARCHIVE="iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz"
```

Download `$ARCHIVE` and its check assets into `$ASSET_DIR` with `curl` or `gh`
only. Those commands are in Install From Binary Artifact. Pass that same
directory as `--dir`. The verifier reads the files there. Omit `--dir` and it
downloads into a temporary directory, then deletes that directory on exit, so
there is no verified archive left to install. Unpack
`${ASSET_DIR}/${ARCHIVE}`, the archive the gate just checked.

Signed release (`v0.1.0-rc.6` and later). Install only when this exits 0:

`<FULL_SHA>` is the commit `refs/tags/v<VERSION>` peels to.

```bash
ASSET_DIR="${ASSET_DIR:-$WORK/assets}"
scripts/verify-release-artifacts.sh --dir "$ASSET_DIR" --commit "<FULL_SHA>" \
  "v<VERSION>" "<TRIPLE>" || exit 1
```

Checksum-only (`v0.1.0-rc.1` through `v0.1.0-rc.5`). Do not use the verifier
as the gate. It exits non-zero on these tags by design, and that exit is not
a pass. The `.sha256` names the archive with no directory, so the check runs
inside `$ASSET_DIR`. Install only when it exits 0. The `|| exit 1` after the
subshell stops the current shell, which closes an interactive shell:

```bash
ASSET_DIR="${ASSET_DIR:-$WORK/assets}"
ARCHIVE="iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz"
(
  cd "$ASSET_DIR" || exit 1
  sha256sum -c "${ARCHIVE}.sha256" || exit 1
  # macOS: shasum -a 256 -c "${ARCHIVE}.sha256" || exit 1
) || exit 1
```

Unpack and install only after the gate above exits 0:

```bash
ASSET_DIR="${ASSET_DIR:-$WORK/assets}"
PREFIX="${PREFIX:-$HOME/.local}"
mkdir -p "${PREFIX}/bin"
ARCHIVE="iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz"
tar -xzf "${ASSET_DIR}/${ARCHIVE}" -C "$ASSET_DIR"
inner="${ASSET_DIR}/${ARCHIVE%.tar.gz}"
tmp="${PREFIX}/bin/iroh-rooms.tmp.$$"
install -m 0755 "${inner}/iroh-rooms" "$tmp"
mv "$tmp" "${PREFIX}/bin/iroh-rooms"
```

The default directory is `$HOME/.local/bin`. Set `PREFIX` to override that
prefix. The binary is always installed at `$PREFIX/bin/iroh-rooms`.

`--version` must print `iroh-rooms` and the `version` field from
`RELEASE-MANIFEST.txt` in the unpacked archive. The commit appears only in
that manifest's `commit` field and in the archive name (the 12-hex suffix).
It does not appear in `--version`.

```bash
ASSET_DIR="${ASSET_DIR:-$WORK/assets}"
PREFIX="${PREFIX:-$HOME/.local}"
ARCHIVE="iroh-rooms-<VERSION>-<TRIPLE>-<COMMIT>.tar.gz"
inner="${ASSET_DIR}/${ARCHIVE%.tar.gz}"
awk -F ': ' '$1 == "version" { print "iroh-rooms " $2; exit }' \
  "${inner}/RELEASE-MANIFEST.txt"
"${PREFIX}/bin/iroh-rooms" --version
awk -F ': ' '$1 == "commit" { print $2; exit }' "${inner}/RELEASE-MANIFEST.txt"
```

The first two lines must be the same string. The `commit` value must equal
the archive name's 12-hex suffix.

### Upgrade

Confirm no `iroh-rooms` process is running. A read-only open can leave a
0-byte `rooms.db-wal`. `documented_backup` refuses while that file exists,
including when it is empty. `scripts/lifecycle-smoke.sh` runs
`PRAGMA wal_checkpoint(TRUNCATE)` first and still refuses if `rooms.db-wal`
remains. Stop `iroh-rooms`, run that checkpoint, and take the backup only
after the file is gone. If it is still there, do not back up and do not
delete the file by hand. Run the backup from `$WORK`, not from the
repository. Then take the documented backup, the `umask 077` tar in Back Up
Before Upgrade. Leave that block unchanged.

```bash
if pgrep -x iroh-rooms >/dev/null 2>&1; then
  printf 'stop iroh-rooms before the backup\n' >&2
  exit 1
fi
data="${IROH_ROOMS_HOME:-$HOME/.local/share/iroh-rooms-beta}"
sqlite3 "${data}/rooms.db" "PRAGMA wal_checkpoint(TRUNCATE);"
if [[ -e "${data}/rooms.db-wal" ]]; then
  printf '%s still exists after checkpoint\n' "${data}/rooms.db-wal" >&2
  exit 1
fi
```

Keep the binary you are replacing. Each maintenance command sets `PREFIX`
when it is unset, which is `$HOME/.local`. A custom install must export the
same `PREFIX` used at install time.

```bash
PREFIX="${PREFIX:-$HOME/.local}"
mv "${PREFIX}/bin/iroh-rooms" "${PREFIX}/bin/iroh-rooms.prev"
```

Install the candidate with the Install steps above (temporary name, then
`mv`). Then run:

```bash
PREFIX="${PREFIX:-$HOME/.local}"
"${PREFIX}/bin/iroh-rooms" identity show
"${PREFIX}/bin/iroh-rooms" room members "<ROOM_ID>"
"${PREFIX}/bin/iroh-rooms" room tail --offline "<ROOM_ID>"
sqlite3 "${IROH_ROOMS_HOME:-$HOME/.local/share/iroh-rooms-beta}/rooms.db" \
  'PRAGMA integrity_check;'
```

`identity show`, `room members`, and `room tail --offline` must still read the
room from before the upgrade. `PRAGMA integrity_check` must print `ok`.

### Rollback

**(a) Same `user_version`.** `v0.1.0-rc.5` and `v0.1.0-rc.6` have the same
`user_version` today. Reinstall the prior binary from `iroh-rooms.prev`. Do
not restore the data directory.

```bash
PREFIX="${PREFIX:-$HOME/.local}"
tmp="${PREFIX}/bin/iroh-rooms.tmp.$$"
install -m 0755 "${PREFIX}/bin/iroh-rooms.prev" "$tmp"
mv "$tmp" "${PREFIX}/bin/iroh-rooms"
"${PREFIX}/bin/iroh-rooms" --version
```

**(b) The candidate raised `user_version`.** The older binary refuses the
store and exits 1. stderr contains `error: could not open event store at <path>: …`
and the refusal from `crates/iroh-rooms-core/src/store/schema.rs`:

```text
database user_version {current} is newer than supported {USER_VERSION}; restore from backup or retry with the previous binary
```

`{current}` is the database `user_version`. `{USER_VERSION}` is the highest
version that binary supports. The full line contains

```text
error: could not open event store at <path>: store migration failed: database user_version {current} is newer than supported {USER_VERSION}; restore from backup or retry with the previous binary
```

Reinstall the prior binary with the rollback (a) commands so `PREFIX` is
honored. Then use only the data-restore lines from the Rollback section
above, which move the directory aside and untar the pre-upgrade backup:

```bash
mv "$IROH_ROOMS_HOME" "$IROH_ROOMS_HOME.before-rollback"
tar -xzf <PRE_UPGRADE_BACKUP>.tar.gz -C "$(dirname "$IROH_ROOMS_HOME")"
```

The `rm` and `install` lines in that older block hard-code
`$HOME/.local/bin`. Do not use them when `PREFIX` is custom. Keep the
move-aside restore for `v0.1.0-rc.1` through `v0.1.0-rc.6`. A refusal from
those builds is not evidence that the store was left untouched.

### Uninstall

Remove the binary. The data directory is left in place unless you remove it
with Remove Local Data.

```bash
PREFIX="${PREFIX:-$HOME/.local}"
rm -f "${PREFIX}/bin/iroh-rooms"
```

`iroh-rooms.prev`, when it is still on disk, is a second binary. Removing
either file does not change the data directory.

### Automated lifecycle check

```bash
scripts/lifecycle-smoke.sh <prior-tag> <candidate-tag> <triple>
```

`<triple>` is one of the four release triples. The script exits 0 only when
every step passes. It writes `lifecycle-<triple>.json` and renders
`lifecycle-<triple>.md` from that JSON. The record's `evidence_grade` is true
only when `class` is `evidence`, and false for every other class.

| `class` | When the script assigns it |
| --- | --- |
| `evidence` | All of `GITHUB_ACTIONS=true`, `GITHUB_REF=refs/heads/main`, `GITHUB_EVENT_NAME=workflow_dispatch`, `RUNNER_ENVIRONMENT=github-hosted`, `GITHUB_REPOSITORY=kortiene/iroh-room`, and `GITHUB_WORKFLOW_REF=kortiene/iroh-room/.github/workflows/lifecycle.yml@refs/heads/main`, with neither `--candidate-binary` nor `--probe`. |
| `probe` | Any other GitHub Actions run, including an evidence-shaped run lowered by `--probe`. |
| `unreleased` | `--candidate-binary` is set. The script refuses that flag and exits 2 when `GITHUB_ACTIONS=true`. |
| `local` | A published-asset run outside GitHub Actions. |

`--probe` can only lower the class. It never raises `local` or `unreleased`.

local / probe / unreleased: developer or diagnostic run; not GA evidence for #265, #293 or #347

The workflow file named by that `GITHUB_WORKFLOW_REF` is planned and is not in
this repository yet. See Lifecycle evidence in
`docs/operations/release-operations.md` for what counts as evidence.
