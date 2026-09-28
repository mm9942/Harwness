#!/usr/bin/env bash
# Commit one finished workflow wave onto its own branch and merge it into the
# integration branch that the main working tree is on.
# Usage: wave_commit.sh <wave> <message-file> <file>...
# - claude/r16/<wave> is cut from $BASE (default origin/dev; use
#   claude/r16-integration when an earlier wave already changed these files)
#   and gets exactly these files. An existing branch of that name is replaced.
# - The main tree (integration branch) merges that branch; the files' identical
#   uncommitted copies are stashed around the merge, other files stay untouched.
set -euo pipefail
REPO=${R16_REPO:-$(git rev-parse --show-toplevel)}
SCR=${R16_SCRATCH:?set R16_SCRATCH}
wave=$1; msgf=$2; shift 2
branch="claude/r16/$wave"
wt="$SCR/wt-$wave"
cd "$REPO"
git fetch -q origin dev
rm -rf "$wt"; git worktree prune
BASE=${BASE:-origin/dev}
git worktree add -q -B "$branch" "$wt" "$BASE"
for f in "$@"; do mkdir -p "$wt/$(dirname "$f")"; cp -p "$REPO/$f" "$wt/$f"; done
git -C "$wt" add -- "$@"
git -C "$wt" commit -q --no-verify -F "$msgf"
for i in 1 2 3 4; do git -C "$wt" push -q --force-with-lease -u origin "$branch" && break || sleep $((2**i)); done
git worktree remove --force "$wt"
# The stash stack is shared with every worktree: tag the entry, keep its SHA,
# restore with apply <sha> and drop only the entry that still carries the tag.
tag="wave-commit-$wave-$$"
git stash push -q --include-untracked -m "$tag" -- "$@"
sha=$(git stash list --format='%H %gs' | awk -v t="$tag" '$0 ~ t {print $1; exit}')
[ -n "$sha" ] || { echo "stash entry $tag not found" >&2; exit 1; }
drop_tagged() {
  ref=$(git stash list --format='%gd %gs' | awk -v t="$tag" '$0 ~ t {print $1; exit}')
  [ -n "$ref" ] && git stash drop -q "$ref"
}
if git merge -q --no-ff "$branch" -m "Merge $branch into claude/r16-integration

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HgUn9md8JpoG9JD2rB66D"; then
  drop_tagged
else
  git merge --abort; git stash apply -q "$sha" && drop_tagged; echo "MERGE FAILED for $branch" >&2; exit 1
fi
for i in 1 2 3 4; do git push -q -u origin claude/r16-integration && break || sleep $((2**i)); done
echo "wave $wave: $(git -C "$REPO" rev-parse --short "$branch") merged into $(git rev-parse --short HEAD)"
