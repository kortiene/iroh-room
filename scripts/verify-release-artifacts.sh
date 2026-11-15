#!/usr/bin/env bash
# Bash 3.2 (stock macOS) and newer. No associative arrays, mapfile, ${var,,},
# ${var^^}, &>>, negative substring offsets, namerefs, or empty-array
# expansion under `set -u`.
set -euo pipefail

# Verify one platform archive from an Iroh Rooms GitHub release.
#
# RB-REL-004 / RB-FC-004. The checks match what
# .github/workflows/release.yml uploads and what its sign-and-publish job
# verifies before it creates a draft.
#
# Usage:
#   scripts/verify-release-artifacts.sh [--dir DIR] [--commit SHA] [--offline] <tag> <triple>
#
# Environment (a flag overrides the matching variable):
#   IROH_ROOMS_RELEASE_DIR      use this directory instead of downloading
#   IROH_ROOMS_RELEASE_COMMIT   full 40-hex commit the caller expects
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
# The script peels refs/tags/<tag> the way the workflow does and always passes
# that commit as --source-digest and --certificate-github-workflow-sha.
# --commit is optional. When it is set, it must equal the peeled commit, and
# the archive name's 12-hex suffix must equal the first 12 hex of that commit.
#
# v0.1.0-rc.1 through v0.1.0-rc.5 are checksum-only. After `gh auth status`
# has succeeded, an attestation miss on those tags (or a lookup that truly
# finds no attestations) fails with that explanation. `gh` not authenticated
# exits 3 before any attestation check.
#
# Tools: GitHub CLI 2.68.0 or newer (`gh attestation verify` with
# --source-digest and --deny-self-hosted-runners). A checksum tool is
# `sha256sum`, or `shasum` when `sha256sum` is absent. cosign v3 or newer is
# required for a full pass. If the checksums and every `gh attestation verify`
# succeed and cosign is missing or older than v3, the script exits 3.
# --offline skips online checks and exits 1 with
# "PARTIAL: online checks skipped". It never exits 0.
#
# Exit codes:
#   0  checksums, attestations, and the cosign signature all passed
#   1  download, checksum, attestation, signature, or commit mismatch failed,
#      or --offline stopped after the local checks
#   2  usage error
#   3  a required tool is missing, gh is not authenticated, or gh is too old

REPO="kortiene/iroh-room"
ISSUER="https://token.actions.githubusercontent.com"

usage() {
  cat <<'EOF'
Usage:
  scripts/verify-release-artifacts.sh [--dir DIR] [--commit SHA] [--offline] <tag> <triple>

Verify one Iroh Rooms release archive: the per-archive SHA-256, the GitHub
build-provenance attestation, and the keyless cosign signature over SHA256SUMS.

  <tag>     Release tag, for example v0.1.0-rc.6
  <triple>  x86_64-apple-darwin, aarch64-apple-darwin,
            x86_64-unknown-linux-gnu, or aarch64-unknown-linux-gnu

Options:
  --dir DIR       Read assets from DIR instead of `gh release download`.
                  IROH_ROOMS_RELEASE_DIR is the same switch.
  --commit SHA    Optional full 40-hex commit. The script peels the tag and
                  uses that commit for --source-digest and
                  --certificate-github-workflow-sha. When --commit is set, it
                  must equal the peeled commit, and the archive name's 12-hex
                  suffix must equal the first 12 hex of that commit.
                  IROH_ROOMS_RELEASE_COMMIT is the same switch.
  --offline       Skip online checks (tag peel, gh attestation lookup, cosign).
                  Prints "PARTIAL: online checks skipped" and exits 1.
                  This is not a successful verification.
  -h, --help      Show this help.

Two different .sigstore.json files are published with a signed release:
  <archive>.sigstore.json   GitHub attestation bundle (gh attestation verify --bundle)
  SHA256SUMS.sigstore.json  cosign signature bundle (cosign verify-blob --bundle)
The SHA256SUMS attestation bundle is not uploaded. This script verifies that
attestation online with `gh attestation verify SHA256SUMS`.

The online path needs `gh auth login` (GitHub CLI 2.68.0 or newer) and
cosign v3 or newer. v0.1.0-rc.1 through v0.1.0-rc.5 have checksums and no
attestation. After authentication, this script fails on those tags at the
attestation step by design.

Exit codes: 0 passed, 1 verification failed (including --offline), 2 usage,
3 gh missing, too old, or not authenticated, or cosign missing/too old.
EOF
}

die() {
  local code="$1"
  shift
  printf 'error: %s\n' "$*" >&2
  exit "$code"
}

is_checksum_only_tag() {
  case "$tag" in
    v0.1.0-rc.1 | v0.1.0-rc.2 | v0.1.0-rc.3 | v0.1.0-rc.4 | v0.1.0-rc.5)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

log_has_no_attestations() {
  local log="$1"
  if grep -F -q 'HTTP 404' "$log"; then
    return 0
  fi
  if grep -F -q 'no attestations found' "$log"; then
    return 0
  fi
  if grep -F -q 'No attestations found' "$log"; then
    return 0
  fi
  return 1
}

attest_failed() {
  local subject="$1"
  local log="$2"
  printf 'error: build-provenance attestation verification failed for %s.\n' "$subject" >&2
  if is_checksum_only_tag || log_has_no_attestations "$log"; then
    cat >&2 <<EOF
v0.1.0-rc.1 through v0.1.0-rc.5 were published with checksums and without
attestations. This script fails on those tags by design.
Releases built by the signing workflow, starting with the first release after
rc.5, publish a GitHub build-provenance attestation for each archive
(<archive>.sigstore.json) and a keyless cosign signature over SHA256SUMS
(SHA256SUMS.sigstore.json).
EOF
  fi
  rm -f "$log"
  exit 1
}

require_gh_auth() {
  if ! gh auth status >/dev/null 2>&1; then
    die 3 "gh not authenticated. Run \`gh auth login\`. The online attestation lookup reads GitHub as the logged-in user. This script did not check whether the release is unsigned."
  fi
}

require_gh_attest_flags() {
  local help_text
  if ! help_text="$(gh attestation verify --help 2>&1)"; then
    die 3 "this gh has no 'attestation verify' command. Install GitHub CLI 2.68.0 or newer (https://cli.github.com)."
  fi
  if ! printf '%s\n' "$help_text" | grep -F -q -- '--source-digest'; then
    die 3 "this gh attestation verify is missing --source-digest. Install GitHub CLI 2.68.0 or newer."
  fi
  if ! printf '%s\n' "$help_text" | grep -F -q -- '--deny-self-hosted-runners'; then
    die 3 "this gh attestation verify is missing --deny-self-hosted-runners. Install GitHub CLI 2.68.0 or newer."
  fi
}

# Count matching archives in $workdir. Sets $archive. Bash 3.2: no arrays.
pick_archive_in_dir() {
  local prefix="iroh-rooms-${version}-${triple}-"
  local path name rest suffix
  local count=0
  local chosen=""
  shopt -s nullglob
  for path in "$workdir"/"$prefix"*.tar.gz; do
    name="$(basename "$path")"
    rest="${name#"$prefix"}"
    suffix="${rest%.tar.gz}"
    if [[ "$rest" == "${suffix}.tar.gz" ]] && [[ "$suffix" =~ ^[0-9a-f]{12}$ ]]; then
      count=$((count + 1))
      chosen="$name"
    fi
  done
  shopt -u nullglob
  if [[ "$count" -eq 0 ]]; then
    die 1 "no archive iroh-rooms-${version}-${triple}-<12 hex>.tar.gz in ${workdir}"
  fi
  if [[ "$count" -ne 1 ]]; then
    die 1 "more than one archive for ${triple} in ${workdir}"
  fi
  archive="$chosen"
}

pick_archive_from_list() {
  local prefix="iroh-rooms-${version}-${triple}-"
  local name rest suffix
  local count=0
  local chosen=""
  while IFS= read -r name; do
    if [[ -z "$name" ]]; then
      continue
    fi
    rest="${name#"$prefix"}"
    if [[ "$rest" == "$name" ]]; then
      continue
    fi
    suffix="${rest%.tar.gz}"
    if [[ "$rest" == "${suffix}.tar.gz" ]] && [[ "$suffix" =~ ^[0-9a-f]{12}$ ]]; then
      count=$((count + 1))
      chosen="$name"
    fi
  done <<< "$asset_names"
  if [[ "$count" -eq 0 ]]; then
    die 1 "release ${tag} has no archive for ${triple}"
  fi
  if [[ "$count" -ne 1 ]]; then
    die 1 "release ${tag} has more than one archive for ${triple}"
  fi
  archive="$chosen"
}

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

download_one() {
  local name="$1"
  echo "downloading ${name}"
  if ! gh release download "$tag" --repo "$REPO" --dir "$workdir" --pattern "$name"; then
    die 1 "download failed for ${name}"
  fi
}

check_sum_file() {
  local sumfile="$1"
  if [[ "$checksum_tool" == "sha256sum" ]]; then
    sha256sum -c "$sumfile"
  else
    shasum -a 256 -c "$sumfile"
  fi
}

archive_suffix() {
  local prefix="iroh-rooms-${version}-${triple}-"
  local rest="${archive#"$prefix"}"
  printf '%s\n' "${rest%.tar.gz}"
}

# Peel refs/tags/<tag> the way release.yml does: a lightweight tag's object
# is the commit; an annotated tag is peeled once to its commit.
resolve_tag_commit() {
  local ref_type ref_sha peeled_type
  echo "peeling refs/tags/${tag}"
  if ! ref_type="$(gh api "repos/${REPO}/git/ref/tags/${tag}" --jq .object.type)"; then
    die 1 "could not read refs/tags/${tag}"
  fi
  if ! ref_sha="$(gh api "repos/${REPO}/git/ref/tags/${tag}" --jq .object.sha)"; then
    die 1 "could not read the object sha of refs/tags/${tag}"
  fi
  case "$ref_type" in
    commit)
      resolved_commit="$ref_sha"
      ;;
    tag)
      if ! peeled_type="$(gh api "repos/${REPO}/git/tags/${ref_sha}" --jq .object.type)"; then
        die 1 "could not peel annotated tag ${tag}"
      fi
      if ! resolved_commit="$(gh api "repos/${REPO}/git/tags/${ref_sha}" --jq .object.sha)"; then
        die 1 "could not read the commit annotated tag ${tag} peels to"
      fi
      if [[ "$peeled_type" != "commit" ]]; then
        die 1 "annotated tag ${tag} peels to ${peeled_type}, not a commit"
      fi
      ;;
    *)
      die 1 "refs/tags/${tag} points at ${ref_type}, not a commit or annotated tag"
      ;;
  esac
  if [[ ! "$resolved_commit" =~ ^[0-9a-f]{40}$ ]]; then
    die 1 "refs/tags/${tag} did not peel to a 40-hex commit"
  fi
}

# $1 subject, $2 bundle path or empty. Exits on failure.
run_attest() {
  local subject="$1"
  local bundle="${2:-}"
  local attest_log
  attest_log="$(mktemp)"
  if [[ -n "$bundle" ]]; then
    printf 'running: gh attestation verify %q --bundle %q --repo %q --cert-identity %q --source-ref %q --source-digest %q --deny-self-hosted-runners\n' \
      "$subject" "$bundle" "$REPO" "$identity" "$source_ref" "$resolved_commit"
    if gh attestation verify "$subject" \
      --bundle "$bundle" \
      --repo "$REPO" \
      --cert-identity "$identity" \
      --source-ref "$source_ref" \
      --source-digest "$resolved_commit" \
      --deny-self-hosted-runners >"$attest_log" 2>&1; then
      cat "$attest_log"
      rm -f "$attest_log"
      return 0
    fi
  else
    printf 'running: gh attestation verify %q --repo %q --cert-identity %q --source-ref %q --source-digest %q --deny-self-hosted-runners\n' \
      "$subject" "$REPO" "$identity" "$source_ref" "$resolved_commit"
    if gh attestation verify "$subject" \
      --repo "$REPO" \
      --cert-identity "$identity" \
      --source-ref "$source_ref" \
      --source-digest "$resolved_commit" \
      --deny-self-hosted-runners >"$attest_log" 2>&1; then
      cat "$attest_log"
      rm -f "$attest_log"
      return 0
    fi
  fi
  cat "$attest_log" >&2
  attest_failed "$subject" "$attest_log"
}

finish_offline() {
  printf 'PARTIAL: online checks skipped\n' >&2
  exit 1
}

dir="${IROH_ROOMS_RELEASE_DIR:-}"
given_commit="${IROH_ROOMS_RELEASE_COMMIT:-}"
offline=0
asset_names=""
archive=""
resolved_commit=""
owned_workdir=0
workdir=""

cleanup() {
  if [[ "$owned_workdir" -eq 1 && -n "$workdir" ]]; then
    rm -rf "$workdir"
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)
      [[ $# -ge 2 ]] || die 2 "--dir requires a path"
      dir="$2"
      shift 2
      ;;
    --commit)
      [[ $# -ge 2 ]] || die 2 "--commit requires a full 40-hex SHA"
      given_commit="$2"
      shift 2
      ;;
    --offline)
      offline=1
      shift
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

if [[ -n "$given_commit" && ! "$given_commit" =~ ^[0-9a-f]{40}$ ]]; then
  die 2 "--commit must be the full 40-hex source digest (the workflow's GITHUB_SHA)"
fi

if [[ "$offline" -eq 0 ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    die 3 "gh is not installed. The attestation checks need GitHub CLI 2.68.0 or newer (https://cli.github.com)."
  fi
  require_gh_auth
  require_gh_attest_flags
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

if [[ -n "$dir" ]]; then
  [[ -d "$dir" ]] || die 1 "directory not found: ${dir}"
  workdir="$(cd "$dir" && pwd)"
  echo "using local assets in ${workdir}"
  pick_archive_in_dir
else
  if [[ "$offline" -eq 1 ]]; then
    die 2 "--offline needs --dir (or IROH_ROOMS_RELEASE_DIR); it does not download"
  fi
  workdir="$(mktemp -d)"
  owned_workdir=1
  echo "listing assets for ${tag}"
  if ! asset_names="$(gh release view "$tag" --repo "$REPO" --json assets --jq '.assets[].name')"; then
    die 1 "could not read assets for ${tag} on ${REPO}"
  fi
  pick_archive_from_list
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

suffix="$(archive_suffix)"
if [[ "$offline" -eq 1 ]]; then
  if [[ -n "$given_commit" ]]; then
    short="${given_commit:0:12}"
    if [[ "$short" != "$suffix" ]]; then
      die 1 "archive name ends in ${suffix}; --commit ${given_commit} starts with ${short}"
    fi
    echo "source commit ${given_commit} (offline; tag was not peeled)"
  fi
  echo "checksum ok."
  finish_offline
fi

resolve_tag_commit
short="${resolved_commit:0:12}"
if [[ -n "$given_commit" && "$given_commit" != "$resolved_commit" ]]; then
  die 1 "--commit ${given_commit} does not equal refs/tags/${tag}, which peels to ${resolved_commit}"
fi
if [[ "$short" != "$suffix" ]]; then
  die 1 "archive name ends in ${suffix}; refs/tags/${tag} peels to ${resolved_commit}"
fi
echo "source commit ${resolved_commit}"

cat <<EOF
checksum ok.
${archive}.sigstore.json is a GitHub build-provenance attestation bundle (SLSA),
verified with gh attestation verify.
SHA256SUMS.sigstore.json is a cosign keyless signature bundle, verified with
cosign verify-blob. The SHA256SUMS attestation bundle is not a release asset;
gh attestation verify SHA256SUMS reads that attestation from GitHub.
EOF

echo "verifying build provenance for ${archive}"
run_attest "$archive"

if [[ ! -s "${archive}.sigstore.json" ]]; then
  die 1 "missing ${archive}.sigstore.json (the GitHub attestation bundle for this archive)"
fi
echo "verifying ${archive} against ${archive}.sigstore.json"
run_attest "$archive" "${archive}.sigstore.json"

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
  printf '%s\n' "$sum_line" >"$one_sum"
  if ! shasum -a 256 -c "$one_sum"; then
    rm -f "$one_sum"
    die 1 "SHA256SUMS does not match ${archive}"
  fi
  rm -f "$one_sum"
fi

echo "verifying build provenance for SHA256SUMS"
run_attest "SHA256SUMS"

if [[ ! -s SHA256SUMS.sigstore.json ]]; then
  die 1 "missing SHA256SUMS.sigstore.json (the cosign keyless signature bundle, not an attestation bundle)"
fi

if ! command -v cosign >/dev/null 2>&1; then
  cat >&2 <<EOF
error: cosign is not installed, so the SHA256SUMS signature was not verified.
Checksums and gh attestation verify passed, including the online SHA256SUMS
attestation. Install cosign v3 or newer and re-run to check SHA256SUMS.sigstore.json.
A full pass requires cosign v3. This is not a successful verification.
EOF
  exit 3
fi

cosign_version="$(cosign version 2>&1 || true)"
if ! printf '%s\n' "$cosign_version" | grep -F -q 'v3.'; then
  die 3 "cosign is not v3 or newer, so SHA256SUMS.sigstore.json was not verified. Install cosign v3 or newer."
fi

printf 'running: cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json --certificate-identity %q --certificate-oidc-issuer %q --certificate-github-workflow-sha %q\n' \
  "$identity" "$ISSUER" "$resolved_commit"
if ! cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "$identity" \
  --certificate-oidc-issuer "$ISSUER" \
  --certificate-github-workflow-sha "$resolved_commit"; then
  die 1 "cosign verify-blob failed for SHA256SUMS"
fi

echo "verified ${tag} ${triple}: checksum, archive attestation, SHA256SUMS attestation, cosign signature (source commit ${resolved_commit})"
