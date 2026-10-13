#!/usr/bin/env bash
# Print the GitHub release notes of one tag: its CHANGELOG.md section plus a link to the changelog.
#
#   scripts/release_notes.sh vX.Y.Z [repo]
#
# Used by .github/workflows/release.yml. `repo` (owner/name) defaults to $GITHUB_REPOSITORY.
# Fails when CHANGELOG.md has no `## [X.Y.Z]` section, or more than one.
set -euo pipefail
cd "$(dirname "$0")/.."

tag=${1:?usage: release_notes.sh vX.Y.Z [repo]}
repo=${2:-${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY or pass the repo}}
heading="## [${tag#v}]"

count=$(grep -cF -- "$heading" CHANGELOG.md || true)
if [[ $count != 1 ]]; then
    echo "release_notes.sh: expected one '$heading' section in CHANGELOG.md, found $count" >&2
    exit 1
fi

# The section body, without its heading and the blank lines around it.
awk -v h="$heading" '
    index($0, h) == 1 { on = 1; next }
    on && /^## / { exit }
    on { lines[++n] = $0 }
    END {
        first = 1; while (first <= n && lines[first] == "") first++
        while (n >= first && lines[n] == "") n--
        for (i = first; i <= n; i++) print lines[i]
    }' CHANGELOG.md

printf '\nFull changelog: [CHANGELOG.md](https://github.com/%s/blob/%s/CHANGELOG.md)\n' "$repo" "$tag"
