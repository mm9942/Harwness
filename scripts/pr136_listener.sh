#!/bin/sh
# PR #136 cooperation listener (read-only polling of comments/reviews).
# Tracks only PR 136; appends new comments/reviews to the inbox file.
# Host mode required (gh needs network access to github.com).

INBOX="${1:-/tmp/pr136-inbox.txt}"
STATE="${2:-/tmp/pr136-seen.txt}"
REPO="mm9942/Harwness"
PR="136"

# Seed: mark every existing comment/review as already seen on first run.
seed() {
  gh api "repos/$REPO/pulls/$PR/comments" --paginate -q '.[].id' >>"$STATE" 2>/dev/null
  gh api "repos/$REPO/issues/$PR/comments" --paginate -q '.[].id' >>"$STATE" 2>/dev/null
  gh api "repos/$REPO/pulls/$PR/reviews" --paginate -q '.[].id' >>"$STATE" 2>/dev/null
  sort -u "$STATE" -o "$STATE"
}

[ -s "$STATE" ] || seed

pull_ids() { # $1: full endpoint path under repos/$REPO
  gh api "repos/$REPO/$1" --paginate -q '.[] | "\(.id)\t\(.user.login)\t\(.created_at)"' 2>/dev/null
}

body_of() { # $1: full endpoint path of a single item
  gh api "repos/$REPO/$1" -q '.body' 2>/dev/null
}

while :; do
  TMP=$(mktemp)
  pull_ids "pulls/$PR/comments" >"$TMP"
  pull_ids "issues/$PR/comments" >>"$TMP"
  pull_ids "pulls/$PR/reviews" >>"$TMP"
  while IFS="$(printf '\t')" read -r id who when; do
    [ -n "$id" ] || continue
    if ! grep -qx "$id" "$STATE" 2>/dev/null; then
      body=$(body_of "pulls/$PR/comments/$id")
      [ -n "$body" ] || body=$(body_of "issues/$PR/comments/$id")
      [ -n "$body" ] || body=$(body_of "pulls/$PR/reviews/$id")
      {
        echo "=== NEW id=$id by=$who at=$when ==="
        printf '%s\n\n' "$body"
      } >>"$INBOX"
      echo "$id" >>"$STATE"
    fi
  done <"$TMP"
  rm -f "$TMP"
  sort -u "$STATE" -o "$STATE"
  sleep 60
done
