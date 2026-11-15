#!/usr/bin/env bash
set -euo pipefail

# Verify one platform archive from an Iroh Rooms GitHub release.
#
# RB-REL-004 / RB-FC-004. The checks match what
# .github/workflows/release.yml actually uploads and what its sign-and-publish
# job verifies before it creates a draft.
#
# Usage:
#   scripts/verify-release-artifacts.sh [--dir DIR] [--commit SHA] <tag> <triple>
#
# Environment (a flag overrides the matching variable):
#   IROH_ROOMS_RELEASE_DIR      use this directory instead of downloading
#   IROH_ROOMS_RELEASE_COMMIT   full 40-hex source digest (the run's GITHUB_SHA)
#
# <tag> is v<VER>, for example v0.1.0-rc.6.
# <triple> is one of x86_64-apple-darwin, aarch64-apple-darwin,
# x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu.
#
# Files, downloaded or already in --dir:
#   iroh-rooms-<VER>-<triple>-<commit12>.tar.gz
#   <archive>.sha256
#   <archive>.sigstore.json          GitHub build-provenance attestation bundle
#   SHA256SUMS
#   SHA256SUMS.sigstore.json         cosign keyless signature bundle
#
# <archive>.sigstore.json is a SLSA build-provenance bundle from actions/attest.
# Verify it with `gh attestation verify`, online and with --bundle.
# SHA256SUMS.sigstore.json is a cosign keyless signature from `cosign sign-blob`.
# Verify it with `cosign verify-blob`. Those two files are different kinds of
# bundle. The SHA256SUMS attestation bundle is not a release asset; verify that
# attestation online with `gh attestation verify SHA256SUMS`.
#
# v0.1.0-rc.1 through v0.1.0-rc.5 are checksum-only. This script fails on them
# at the attestation step by design.
#
# Tools: `gh` is required before any check, and it must provide
# `gh attestation verify`. A checksum tool is `sha256sum`, or
# `shasum` when `sha256sum` is absent (macOS). `cosign` is required for a full
# pass. If the checksums and every `gh attestation verify` succeed and `cosign`
# is not installed, the script exits 3 and says the signature was not checked.
# It does not exit 0 in that case. The online `gh` path needs network access
# and `gh auth login`.
#
# Exit codes:
#   0  checksums, attestations, and the cosign signature all passed
#   1  download, checksum, attestation, or signature failed
#   2  usage error
#   3  a required tool is missing

REPO="kortiene/iroh-room"
ISSUER="https://token.actions.githubusercontent.com"

usage() {
  cat <<'EOF'
Usage:
  scripts/verify-release-artifacts.sh [--dir DIR] [--commit SHA] <tag> <triple>

Verify one Iroh Rooms release archive: the per-archive SHA-256, the GitHub
build-provenance attestation, and the keyless cosign signature over SHA256SUMS.

  <tag>     Release tag, for example v0.1.0-rc.6
  <triple>  x86_64-apple-darwin, aarch64-apple-darwin,
            x86_64-unknown-linux-gnu, or aarch64-unknown-linux-gnu

Options:
  --dir DIR       Read assets from DIR instead of `gh release download`.
                  IROH_ROOMS_RELEASE_DIR is the same switch.
  --commit SHA    Full 40-hex source digest. Adds --source-digest to
                  `gh attestation verify` and --certificate-github-workflow-sha
                  to `cosign verify-blob`, matching the workflow self-verify.
                  IROH_ROOMS_RELEASE_COMMIT is the same switch.
  -h, --help      Show this help.

Two different .sigstore.json files are published with a signed release:
  <archive>.sigstore.json   GitHub attestation bundle (gh attestation verify --bundle)
  SHA256SUMS.sigstore.json  cosign signature bundle (cosign verify-blob --bundle)
The SHA256SUMS attestation bundle is not uploaded. This script verifies that
attestation online with `gh attestation verify SHA256SUMS`.

v0.1.0-rc.1 through v0.1.0-rc.5 have checksums and no attestation. The script
fails on those tags at the attestation step by design.

Exit codes: 0 passed, 1 verification failed, 2 usage, 3 required tool missing.
`gh` is required up front. If `cosign` is missing after the gh checks have
passed, the exit code is 3 and the signature was not verified.
EOF
}

die() {
  local code="$1"
  shift
  printf 'error: %s\n' "$*" >&2
  exit "$code"
}

dir="${IROH_ROOMS_RELEASE_DIR:-}"
commit="${IROH_ROOMS_RELEASE_COMMIT:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)
      [[ $# -ge 2 ]] || die 2 "--dir requires a path"
      dir="$2"
      shift 2
      ;;
    --commit)
      [[ $# -ge 2 ]] || die 2 "--commit requires a full 40-hex SHA"
      commit="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      die 2 "unknown option: $1"
      ;;
    *)
      break
      ;;
  esac
done

if [[ $# -ne 2 ]]; then
  usage >&2
  die 2 "expected <tag> <triple>"
fi

tag="$1"
triple="$2"

if [[ ! "$tag" =~ ^v[0-9A-Za-z][0-9A-Za-z.+-]*$ ]]; then
  die 2 "tag must look like v0.1.0-rc.6 (got ${tag})"
fi
version="${tag#v}"

case "$triple" in
  x86_64-apple-darwin | aarch64-apple-darwin | x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
    ;;
  *)
    die 2 "triple must be one of x86_64-apple-darwin, aarch64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu"
    ;;
esac

if [[ -n "$commit" && ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
  die 2 "--commit must be the full 40-hex source digest (the workflow's GITHUB_SHA)"
fi

if ! command -v gh >/dev/null 2>&1; then
  die 3 "gh is not installed. The attestation checks need it (https://cli.github.com)."
fi
if ! gh attestation verify --help >/dev/null 2>&1; then
  die 3 "this gh has no 'attestation verify' command. Install a current GitHub CLI that includes it (https://cli.github.com)."
fi
if command -v sha256sum >/dev/null 2>&1; then
  checksum_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  checksum_tool="shasum"
else
  die 3 "neither sha256sum nor shasum is installed"
fi

identity="https://github.com/${REPO}/.github/workflows/release.yml@refs/tags/${tag}"
source_ref="refs/tags/${tag}"

owned_workdir=0
workdir=""
cleanup() {
  if [[ "$owned_workdir" -eq 1 && -n "$workdir" ]]; then
    rm -rf "$workdir"
  fi
}
trap cleanup EXIT

have_asset() {
  local name="$1"
  local line
  while IFS= read -r line; do
    if [[ "$line" == "$name" ]]; then
      return 0
    fi
  done <<< "$asset_names"
  return 1
}

find_archive() {
  local prefix="iroh-rooms-${version}-${triple}-"
  local name rest commit12
  local -a found=()
  shopt -s nullglob
  for path in "$workdir"/"$prefix"*.tar.gz; do
    name="$(basename "$path")"
    rest="${name#"$prefix"}"
    commit12="${rest%.tar.gz}"
    if [[ "$rest" == "${commit12}.tar.gz" && "$commit12" =~ ^[0-9a-f]{12}$ ]]; then
      found+=("$name")
    fi
  done
  shopt -u nullglob
  if [[ "${#found[@]}" -eq 0 ]]; then
    die 1 "no archive iroh-rooms-${version}-${triple}-<12 hex>.tar.gz in ${workdir}"
  fi
  if [[ "${#found[@]}" -ne 1 ]]; then
    die 1 "more than one archive for ${triple} in ${workdir}"
  fi
  archive="${found[0]}"
}

check_sum_file() {
  local sumfile="$1"
  if [[ "$checksum_tool" == "sha256sum" ]]; then
    sha256sum -c "$sumfile"
  else
    shasum -a 256 -c "$sumfile"
  fi
}

# Extra args are appended (for example --bundle <path>). --source-digest is
# added only when the caller passed the expected commit.
run_attest() {
  local subject="$1"
  shift
  local -a cmd=(
    gh attestation verify "$subject"
    --repo "$REPO"
    --cert-identity "$identity"
    --source-ref "$source_ref"
    --deny-self-hosted-runners
  )
  if [[ -n "$commit" ]]; then
    cmd+=(--source-digest "$commit")
  fi
  if [[ $# -gt 0 ]]; then
    cmd+=("$@")
  fi
  printf 'running:'
  printf ' %q' "${cmd[@]}"
  printf '\n'
  "${cmd[@]}"
}

attest_failed() {
  local subject="$1"
  cat >&2 <<EOF
error: build-provenance attestation verification failed for ${subject}.
v0.1.0-rc.1 through v0.1.0-rc.5 were published with checksums and without
attestations. This script fails on those tags by design.
Releases built by the signing workflow, starting with the first release after
rc.5, publish a GitHub build-provenance attestation for each archive
(<archive>.sigstore.json) and a keyless cosign signature over SHA256SUMS
(SHA256SUMS.sigstore.json).
If this tag should already be signed, confirm \`gh auth status\`. The online
path reads attestations from the GitHub API.
EOF
  exit 1
}

if [[ -n "$dir" ]]; then
  [[ -d "$dir" ]] || die 1 "directory not found: ${dir}"
  workdir="$(cd "$dir" && pwd)"
  echo "using local assets in ${workdir}"
else
  workdir="$(mktemp -d)"
  owned_workdir=1
  echo "listing assets for ${tag}"
  if ! asset_names="$(gh release view "$tag" --repo "$REPO" --json assets --jq '.assets[].name')"; then
    die 1 "could not read assets for ${tag} on ${REPO}"
  fi
  find_archive_name_from_list() {
    local prefix="iroh-rooms-${version}-${triple}-"
    local name rest commit12
    local -a found=()
    while IFS= read -r name; do
      [[ -z "$name" ]] && continue
      [[ "$name" == "$prefix"*.tar.gz ]] || continue
      rest="${name#"$prefix"}"
      commit12="${rest%.tar.gz}"
      if [[ "$rest" == "${commit12}.tar.gz" && "$commit12" =~ ^[0-9a-f]{12}$ ]]; then
        found+=("$name")
      fi
    done <<< "$asset_names"
    if [[ "${#found[@]}" -eq 0 ]]; then
      die 1 "release ${tag} has no archive for ${triple}"
    fi
    if [[ "${#found[@]}" -ne 1 ]]; then
      die 1 "release ${tag} has more than one archive for ${triple}"
    fi
    archive="${found[0]}"
  }
  find_archive_name_from_list
  download_one() {
    local name="$1"
    echo "downloading ${name}"
    if ! gh release download "$tag" --repo "$REPO" --dir "$workdir" --pattern "$name"; then
      die 1 "download failed for ${name}"
    fi
  }
  download_one "$archive"
  if have_asset "${archive}.sha256"; then
    download_one "${archive}.sha256"
  fi
  if have_asset "${archive}.sigstore.json"; then
    download_one "${archive}.sigstore.json"
  fi
  if have_asset "SHA256SUMS"; then
    download_one "SHA256SUMS"
  fi
  if have_asset "SHA256SUMS.sigstore.json"; then
    download_one "SHA256SUMS.sigstore.json"
  fi
fi

if [[ -z "${archive:-}" ]]; then
  find_archive
fi

cd "$workdir"

if [[ ! -f "${archive}.sha256" ]]; then
  die 1 "missing ${archive}.sha256"
fi

sum_line="$(cat "${archive}.sha256")"
sum_hash="${sum_line%% *}"
sum_name="${sum_line#*  }"
if [[ ! "$sum_hash" =~ ^[0-9a-f]{64}$ || "$sum_name" != "$archive" ]]; then
  printf 'error: %s.sha256 is not a bare sha256 line for %s\n' "$archive" "$archive" >&2
  printf 'line: [%s]\n' "$sum_line" >&2
  exit 1
fi

echo "checking ${archive}.sha256 with ${checksum_tool}"
if ! check_sum_file "${archive}.sha256"; then
  die 1 "checksum failed for ${archive}"
fi

cat <<EOF
checksum ok.
${archive}.sigstore.json is a GitHub build-provenance attestation bundle (SLSA),
verified with gh attestation verify.
SHA256SUMS.sigstore.json is a cosign keyless signature bundle, verified with
cosign verify-blob. The SHA256SUMS attestation bundle is not a release asset;
gh attestation verify SHA256SUMS reads that attestation from GitHub.
EOF

echo "verifying build provenance for ${archive}"
if ! run_attest "$archive"; then
  attest_failed "$archive"
fi

if [[ ! -s "${archive}.sigstore.json" ]]; then
  die 1 "missing ${archive}.sigstore.json (the GitHub attestation bundle for this archive)"
fi
echo "verifying ${archive} against ${archive}.sigstore.json"
if ! run_attest "$archive" --bundle "${archive}.sigstore.json"; then
  attest_failed "${archive} (bundle ${archive}.sigstore.json)"
fi

if [[ ! -f SHA256SUMS ]]; then
  die 1 "SHA256SUMS is missing. Releases built by the signing workflow publish it next to the archives."
fi
if ! grep -Fxq "$sum_line" SHA256SUMS; then
  die 1 "SHA256SUMS does not contain the checksum line for ${archive}"
fi

echo "checking SHA256SUMS with ${checksum_tool}"
if [[ "$checksum_tool" == "sha256sum" ]]; then
  if ! sha256sum -c --ignore-missing SHA256SUMS; then
    die 1 "SHA256SUMS does not match ${archive}"
  fi
else
  one_sum="$(mktemp)"
  printf '%s\n' "$sum_line" > "$one_sum"
  if ! shasum -a 256 -c "$one_sum"; then
    rm -f "$one_sum"
    die 1 "SHA256SUMS does not match ${archive}"
  fi
  rm -f "$one_sum"
fi

echo "verifying build provenance for SHA256SUMS"
if ! run_attest SHA256SUMS; then
  attest_failed "SHA256SUMS"
fi

if [[ ! -s SHA256SUMS.sigstore.json ]]; then
  die 1 "missing SHA256SUMS.sigstore.json (the cosign keyless signature bundle, not an attestation bundle)"
fi

if ! command -v cosign >/dev/null 2>&1; then
  cat >&2 <<EOF
error: cosign is not installed, so the SHA256SUMS signature was not verified.
Checksums and gh attestation verify passed, including the online SHA256SUMS
attestation. Install cosign and re-run to check SHA256SUMS.sigstore.json.
A full pass requires cosign. This is not a successful verification.
EOF
  exit 3
fi

cosign_cmd=(
  cosign verify-blob SHA256SUMS
  --bundle SHA256SUMS.sigstore.json
  --certificate-identity "$identity"
  --certificate-oidc-issuer "$ISSUER"
)
if [[ -n "$commit" ]]; then
  cosign_cmd+=(--certificate-github-workflow-sha "$commit")
fi
printf 'running:'
printf ' %q' "${cosign_cmd[@]}"
printf '\n'
if ! "${cosign_cmd[@]}"; then
  die 1 "cosign verify-blob failed for SHA256SUMS"
fi

echo "verified ${tag} ${triple}: checksum, archive attestation, SHA256SUMS attestation, cosign signature"
