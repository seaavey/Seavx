#!/usr/bin/env bash
# The body of one GitHub release: commit count, line stats, the pull requests
# and the contributors. No issues section: this repository does not track
# releases through issues, so one would always be empty.
#
# Usage: TAG=v0.1.3 REPO=owner/name bash .github/scripts/release-notes.sh
set -euo pipefail

tag=${TAG:?TAG is required}
repo=${REPO:?REPO is required}

# A checkout can hold a branch of the same name as the tag: name the tag by ref
# so every range below means the release, not the branch.
tag_ref="refs/tags/${tag}"
prev=$(git describe --tags --abbrev=0 --match 'v*' "${tag_ref}^" 2>/dev/null || true)
if [ -n "$prev" ]; then
  range="${prev}..${tag_ref}"
  since="$prev"
else
  range="$tag_ref"
  since="the first release"
fi
commits=$(git rev-list --count "$range")

# An empty tree compares against everything the tag carries, so the first
# release still gets real line counts.
empty_tree=4b825dc642cb6eb9a060e54bf8d69288fbee4904
base=${prev:-$empty_tree}
shortstat=$(git diff --shortstat "$base" "$tag_ref")
files=$(printf '%s' "$shortstat" | sed -nE 's/.* ([0-9]+) files? changed.*/\1/p')
added=$(printf '%s' "$shortstat" | sed -nE 's/.* ([0-9]+) insertions?\(\+\).*/\1/p')
removed=$(printf '%s' "$shortstat" | sed -nE 's/.* ([0-9]+) deletions?\(-\).*/\1/p')
files=${files:-0}
added=${added:-0}
removed=${removed:-0}

echo "**${commits} commits** since ${since} · **+${added} / -${removed}** across ${files} files"
echo
echo "### Pull requests"
echo

# Any commit can belong to at most a handful of PRs, and a stack merge can leave
# the merged PR's head commit associated with the stack's parent only: take every
# PR a commit names, and every merged PR whose merge commit lands in the range.
# One API call per commit, capped so a huge range still finishes.
in_range() {
  git merge-base --is-ancestor "$1" "$tag_ref" 2>/dev/null || return 1
  [ -z "$prev" ] || ! git merge-base --is-ancestor "$1" "$prev" 2>/dev/null
}
prs=$(
  {
    for sha in $(git rev-list --max-count=300 "$range"); do
      gh api "repos/${repo}/commits/${sha}/pulls" \
        --jq '.[] | "\(.number)\t\(.title)\t\(.user.login)"' 2>/dev/null || true
    done
    while IFS=$'\t' read -r number title login merge; do
      if [ -n "$merge" ] && in_range "$merge"; then
        printf '%s\t%s\t%s\n' "$number" "$title" "$login"
      fi
    done < <(
      gh pr list --repo "$repo" --state merged --limit 200 \
        --json number,title,author,mergeCommit \
        --jq '.[] | "\(.number)\t\(.title)\t\(.author.login)\t\(.mergeCommit.oid // "")"' \
        2>/dev/null || true
    )
  } | awk -F '\t' 'NF && !seen[$1]++ { printf "- #%s %s (@%s)\n", $1, $2, $3 }' | sort -t'#' -k2 -n
)
echo "${prs:-- No pull requests in this range.}"
echo
echo "### Contributors"
echo

git shortlog -sne "$range" | awk -F '\t' '
  {
    name = $2
    sub(/ <.*/, "", name)
    count[name] += $1 + 0
  }
  END {
    for (name in count) {
      n = count[name]
      printf "- %s (%d commit%s)\n", name, n, (n == 1 ? "" : "s")
    }
  }
' | sort -t'(' -k2 -rn
