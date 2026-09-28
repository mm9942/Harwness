#!/usr/bin/env bash
# Commit a wave that ran in its own worktree (.worktrees/<wave>, branch
# claude/r16/<wave>), push it, merge it into claude/r16-integration in the
# main tree and remove the worktree.
# Usage: wt_wave_commit.sh <wave> <message-file> <file>...
# Only the named files are committed. Any other change in the worktree is
# foreign (an agent that ran in the wrong checkout, catalog P14): the script
# lists it and stops, so it never lands in this wave's commit.
set -euo pipefail
REPO=${R16_REPO:-$(git rev-parse --show-toplevel)}
wave=$1; msgf=$2; shift 2
[ $# -gt 0 ] || { echo "name the wave's files" >&2; exit 1; }
if [ "${MERGE_ONLY:-0}" = 1 ]; then
  wt="$REPO/.worktrees/$wave"; branch="claude/r16/$wave"
  dirty=$(cd "$REPO" && for f in "$@"; do git diff --quiet -- "$f" 2>/dev/null || echo "$f"; done)
  if [ -n "$dirty" ]; then echo "main tree has uncommitted changes in:" >&2; echo "$dirty" >&2; exit 1; fi
  cd "$REPO"
  git merge -q --no-ff "$branch" -m "Merge $branch into claude/r16-integration

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HgUn9md8JpoG9JD2rB66D"
  for i in 1 2 3 4; do git push -q -u origin claude/r16-integration && break || sleep $((2**i)); done
  git worktree remove --force "$wt"
  echo "wave $wave: $(git rev-parse --short "$branch") merged into $(git rev-parse --short HEAD)"; exit 0
fi
wt="$REPO/.worktrees/$wave"; branch="claude/r16/$wave"
foreign=$(git -C "$wt" status --porcelain --untracked-files=all | awk '{print $2}' | while read -r p; do
  keep=0; for f in "$@"; do [ "$p" = "$f" ] && keep=1; done; [ $keep = 1 ] || echo "$p"; done)
if [ -n "$foreign" ]; then
  echo "foreign changes in $wt (not part of this commit):" >&2; echo "$foreign" >&2
  # FOREIGN_OK=1: commit the named files anyway and keep the worktree, so the
  # foreign files stay there for reconciliation and running agents do not
  # write into a removed directory.
  [ "${FOREIGN_OK:-0}" = 1 ] || exit 1
  KEEP_WT=1
fi
# Refuse a merge that would run into uncommitted changes of the main tree.
if [ "${NO_MERGE:-0}" != 1 ]; then
  dirty=$(cd "$REPO" && for f in "$@"; do git diff --quiet -- "$f" 2>/dev/null || echo "$f"; done)
  if [ -n "$dirty" ]; then echo "main tree has uncommitted changes in:" >&2; echo "$dirty" >&2; exit 1; fi
fi
base=$(git -C "$wt" rev-parse HEAD)
git -C "$wt" add -- "$@"
git -C "$wt" commit -q --no-verify -F "$msgf"
# Immutable wave manifest (kit wave_manifest.py) on top of the content commit:
# WAVE_SCRIPT (the wave's .js with embedded args), optional RESULT (task
# output JSON), RUNS (space-separated run ids), NOTE.
if [ -n "${WAVE_SCRIPT:-}" ]; then
  MANIFEST=${MANIFEST:-$REPO/docs/planning/85-gap-hunt/kit/wave_manifest.py}
  [ -f "$MANIFEST" ] || MANIFEST=$REPO/.worktrees/kit-4/docs/planning/85-gap-hunt/kit/wave_manifest.py
  head=$(git -C "$wt" rev-parse HEAD)
  runs=(); for r in ${RUNS:-}; do runs+=(--run "$r"); done
  extra=(); [ -n "${RESULT:-}" ] && extra+=(--result "$RESULT"); [ -n "${NOTE:-}" ] && extra+=(--note "$NOTE")
  python3 "$MANIFEST" --repo "$wt" --wave "$wave" --branch "$branch" --base "$base" --head "$head" \
    --script "$WAVE_SCRIPT" "${runs[@]}" "${extra[@]}" "$@" >/dev/null
  git -C "$wt" add -- "docs/planning/85-gap-hunt/waves/$wave.json"
  git -C "$wt" commit -q --no-verify -m "R16 $wave: wave manifest

Base, branch, file set, finding ids, workflow runs and disposition of
$wave, pinned to its content commit ${head:0:7}.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HgUn9md8JpoG9JD2rB66D"
fi
for i in 1 2 3 4; do git -C "$wt" push -q -u origin "$branch" && break || sleep $((2**i)); done
# NO_MERGE=1: stop after the push (the branch waits for a file the main tree
# still has in flight); merge later with MERGE_ONLY=1.
if [ "${NO_MERGE:-0}" = 1 ]; then echo "wave $wave: $(git -C "$wt" rev-parse --short HEAD) pushed, merge deferred"; exit 0; fi
cd "$REPO"
git merge -q --no-ff "$branch" -m "Merge $branch into claude/r16-integration

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HgUn9md8JpoG9JD2rB66D"
for i in 1 2 3 4; do git push -q -u origin claude/r16-integration && break || sleep $((2**i)); done
if [ "${KEEP_WT:-0}" = 1 ]; then echo "worktree $wt kept"; else git worktree remove --force "$wt"; fi
echo "wave $wave: $(git rev-parse --short "$branch") merged into $(git rev-parse --short HEAD)"
