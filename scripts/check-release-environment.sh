#!/usr/bin/env bash
# Bash 3.2 (stock macOS) and newer. No associative arrays, mapfile, ${var,,},
# ${var^^}, or namerefs.
#
# Fail unless the sign-and-publish job's `environment:` value equals its
# RELEASE_ENVIRONMENT value. The names themselves are not fixed here.
set -eu

workflow="${1:-.github/workflows/release.yml}"

if [[ ! -f "$workflow" ]]; then
  printf 'error: workflow not found: %s\n' "$workflow" >&2
  exit 1
fi

awk '
function trim(s) {
  sub(/[ \t]*#.*/, "", s)
  gsub(/^[ \t]+|[ \t]+$/, "", s)
  gsub(/^[ \t"\047]+|[ \t"\047]+$/, "", s)
  return s
}
/^  sign-and-publish:[[:space:]]*$/ { injob = 1; next }
injob && /^  [^ ]/ { injob = 0 }
injob && /^    environment:[[:space:]]+[^[:space:]]/ {
  line = $0
  sub(/^    environment:[[:space:]]+/, "", line)
  envname = trim(line)
  envcount++
  next
}
injob && /^    environment:[[:space:]]*$/ { wantname = 1; next }
injob && wantname && /^      name:[[:space:]]+[^[:space:]]/ {
  line = $0
  sub(/^      name:[[:space:]]+/, "", line)
  envname = trim(line)
  envcount++
  wantname = 0
  next
}
injob && /^      RELEASE_ENVIRONMENT:[[:space:]]+[^[:space:]]/ {
  line = $0
  sub(/^      RELEASE_ENVIRONMENT:[[:space:]]+/, "", line)
  rel = trim(line)
  relcount++
}
END {
  if (envcount != 1) {
    printf "error: sign-and-publish environment: count is %d\n", envcount > "/dev/stderr"
    exit 1
  }
  if (relcount != 1) {
    printf "error: RELEASE_ENVIRONMENT count is %d\n", relcount > "/dev/stderr"
    exit 1
  }
  if (envname != rel) {
    printf "error: sign-and-publish environment (%s) != RELEASE_ENVIRONMENT (%s)\n", envname, rel > "/dev/stderr"
    exit 1
  }
  if (envname == "") {
    printf "error: sign-and-publish environment name is empty\n" > "/dev/stderr"
    exit 1
  }
  printf "sign-and-publish environment and RELEASE_ENVIRONMENT are both %s\n", envname
}
' "$workflow"
