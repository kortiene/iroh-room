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
covered that gap and is retired for `v0.1.0-rc.6`; see
[`v0.1.0-rc.6-production-beta-signoff.md`](../releases/v0.1.0-rc.6-production-beta-signoff.md)).
Verify the signed release with the script below and pin Actions run
[43835416665](https://github.com/kortiene/iroh-room/actions/runs/43835416665):

```bash
scripts/verify-release-artifacts.sh --commit bf353b78f63b08ff5e3f425273aa3e0983167679 v0.1.0-rc.6 <TRIPLE>
```

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
