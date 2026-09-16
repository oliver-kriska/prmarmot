#!/usr/bin/env bash
# One-shot: turn on GitHub Discussions for oliver-kriska/prmarmot, set repo topics, and post the
# waitlist thread and the feature "Ideas" for upvoting. Idempotent: skips posts whose title
# already exists. Requires `gh` logged in as the repo owner.
#
#   scripts/discussions/setup.sh            # dry run: prints what it would do
#   scripts/discussions/setup.sh --apply    # enables Discussions, sets topics, posts
#
# Bodies live in scripts/discussions/posts/*.md; the first line `# Title` is the discussion
# title, the second `category: Ideas` picks the category, `pin: true` marks the waitlist thread
# (its number is written to measurements/waitlist-discussion for adoption-snapshot.sh; GitHub's
# GraphQL API has no pinDiscussion mutation as of 2026-09-16, so pin it in the web UI).
# Everything after the first blank line is the body.
set -euo pipefail

REPO="${PRMARMOT_REPO_SLUG:-oliver-kriska/prmarmot}"
OWNER="${REPO%/*}"; NAME="${REPO#*/}"
HERE="$(cd "$(dirname "$0")" && pwd)"
APPLY=0; [[ "${1:-}" == "--apply" ]] && APPLY=1
NUMFILE="$HERE/../../measurements/waitlist-discussion"; mkdir -p "$(dirname "$NUMFILE")"

say() { printf '%s\n' "$*" >&2; }

enabled=$(gh api "repos/$REPO" --jq '.has_discussions')
if [[ "$enabled" != "true" ]]; then
  if (( APPLY )); then
    gh api -X PATCH "repos/$REPO" -F has_discussions=true --silent
    say "enabled Discussions on $REPO"
  else
    say "would enable Discussions on $REPO"
  fi
fi

# Repo topics (Phase 3 hygiene) — set once, idempotent.
TOPICS="github pull-requests code-review rust gpui macos linux developer-tools cli claude-code"
have=$(gh api "repos/$REPO" --jq '.topics | join(" ")')
if [[ "$have" != *"pull-requests"* ]]; then
  if (( APPLY )); then
    args=(); for t in $TOPICS; do args+=(--add-topic "$t"); done
    gh repo edit "$REPO" "${args[@]}" >/dev/null && say "set topics: $TOPICS"
  else
    say "would set topics: $TOPICS"
  fi
fi

# Categories are created by GitHub when Discussions is enabled (Announcements, General,
# Ideas, Polls, Q&A, Show and tell); the API cannot create them.
meta=$(gh api graphql -f query='query($o:String!,$n:String!){ repository(owner:$o,name:$n){ id
  discussionCategories(first:20){ nodes{ id name } }
  discussions(first:100){ nodes{ title url } } } }' -f o="$OWNER" -f n="$NAME" 2>/dev/null || echo '{}')
repo_id=$(jq -r '.data.repository.id // empty' <<<"$meta")

record_number() {  # $1 = discussion url
  printf '%s\n' "${1##*/}" > "$NUMFILE"
  say "waitlist discussion #${1##*/} recorded in $NUMFILE (adoption-snapshot.sh reads it)"
}

for f in "$HERE"/posts/*.md; do
  title=$(sed -n '1s/^# //p' "$f")
  category=$(sed -n 's/^category: //p' "$f" | head -1)
  pin=$(sed -n 's/^pin: //p' "$f" | head -1)
  body=$(awk 'f{print} /^$/{f=1}' "$f")
  existing=$(jq -r --arg t "$title" '.data.repository.discussions.nodes[]? | select(.title==$t) | .url' <<<"$meta")
  if [[ -n "$existing" ]]; then
    say "exists: $title ($existing)"
    if [[ "$pin" == "true" ]] && (( APPLY )) && [[ ! -s "$NUMFILE" ]]; then record_number "$existing"; fi
    continue
  fi
  cat_id=$(jq -r --arg c "$category" '.data.repository.discussionCategories.nodes[]? | select(.name==$c) | .id' <<<"$meta")
  if (( ! APPLY )); then
    say "would post [$category${pin:+, waitlist}]: $title"; continue
  fi
  [[ -n "$repo_id" && -n "$cat_id" ]] || { say "missing repo/category id for '$title' (category '$category') — enable Discussions first"; exit 1; }
  url=$(gh api graphql -f query='mutation($r:ID!,$c:ID!,$t:String!,$b:String!){
      createDiscussion(input:{repositoryId:$r,categoryId:$c,title:$t,body:$b}){ discussion{ id url } } }' \
      -f r="$repo_id" -f c="$cat_id" -f t="$title" -f b="$body" --jq '.data.createDiscussion.discussion.url')
  say "posted: $url"
  if [[ "$pin" == "true" ]]; then
    record_number "$url"
    say "PIN IT MANUALLY: open $url → ··· menu → Pin discussion"
  fi
done
