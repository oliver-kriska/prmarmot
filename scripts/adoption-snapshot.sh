#!/usr/bin/env bash
# Append one row per run to measurements/adoption.csv — no telemetry, only what GitHub
# already counts: stars, release-asset downloads, 14-day traffic, waitlist signals
# (unique commenters + 👍 reactors on the pinned waitlist Discussion) and the top Ideas
# by upvotes. Run weekly (cron/launchd) or by hand. Needs `gh` logged in as the owner
# (traffic requires push access).
set -euo pipefail
REPO="${PRMARMOT_REPO_SLUG:-oliver-kriska/prmarmot}"
OWNER="${REPO%/*}"; NAME="${REPO#*/}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HERE/../measurements/adoption.csv}"
# The waitlist discussion is matched by NUMBER, never by title: a renamed title would silently
# count zero, and a zero from a gate counter looks like "nobody signed up", not like a bug.
# scripts/discussions/setup.sh writes the number to measurements/waitlist-discussion when it posts.
NUMFILE="$HERE/../measurements/waitlist-discussion"
# The live thread is #8 (posted 2026-09-16); measurements/ is gitignored, so the number is also hardcoded.
WAITLIST_NUM="${PRMARMOT_WAITLIST_DISCUSSION:-$(cat "$NUMFILE" 2>/dev/null || echo 8)}"
if [[ -z "$WAITLIST_NUM" ]]; then
  echo "waitlist discussion number unknown: set PRMARMOT_WAITLIST_DISCUSSION or create $NUMFILE" >&2
  exit 2
fi

today=$(date -u +%F)
stars=$(gh api "repos/$REPO" --jq '.stargazers_count')
downloads=$(gh api "repos/$REPO/releases" --paginate --jq '[.[].assets[].download_count] | add // 0')
# Traffic needs push access; on any failure record an empty cell, never the error body.
views=$(gh api "repos/$REPO/traffic/views" --jq '.uniques' 2>/dev/null) || views=""
clones=$(gh api "repos/$REPO/traffic/clones" --jq '.uniques' 2>/dev/null) || clones=""
stranger_issues=$(gh api -X GET search/issues -f q="repo:$REPO is:issue -author:$OWNER" --jq '.total_count')

# Waitlist signals: unique humans who commented or 👍-reacted on discussion #N, owner excluded.
# Both connections are paginated (G1 triggers at 100, exactly where first:100 would truncate).
wl_query='query($o:String!,$n:String!,$num:Int!,$rc:String,$cc:String){ repository(owner:$o,name:$n){
  discussion(number:$num){ number title
    reactions(first:100, content:THUMBS_UP, after:$rc){ pageInfo{ hasNextPage endCursor } nodes{ user{ login } } }
    comments(first:100, after:$cc){ pageInfo{ hasNextPage endCursor } nodes{ author{ login } } } } } }'
logins=""; rc=""; cc=""; more=1; title=""
while (( more )); do
  page=$(gh api graphql -f query="$wl_query" -f o="$OWNER" -f n="$NAME" -F num="$WAITLIST_NUM" \
          ${rc:+-f rc="$rc"} ${cc:+-f cc="$cc"}) || { echo "GraphQL query for discussion #$WAITLIST_NUM failed" >&2; exit 1; }
  d=$(jq -c '.data.repository.discussion' <<<"$page")
  if [[ "$d" == "null" || -z "$d" ]]; then echo "discussion #$WAITLIST_NUM not found in $REPO" >&2; exit 1; fi
  title=$(jq -r '.title' <<<"$d")
  logins+=$'\n'$(jq -r '(.reactions.nodes[].user.login), (.comments.nodes[].author.login)' <<<"$d")
  rn=$(jq -r '.reactions.pageInfo.hasNextPage' <<<"$d"); cn=$(jq -r '.comments.pageInfo.hasNextPage' <<<"$d")
  more=0
  if [[ "$rn" == "true" ]]; then rc=$(jq -r '.reactions.pageInfo.endCursor' <<<"$d"); more=1; fi
  if [[ "$cn" == "true" ]]; then cc=$(jq -r '.comments.pageInfo.endCursor' <<<"$d"); more=1; fi
  # once a connection is exhausted keep its cursor at the last page so the loop only advances the other
done
# grep exits 1 on no matches; an empty waitlist is a legitimate 0, so use awk instead of grep here.
waitlist=$(printf '%s\n' "$logins" | awk -v o="$OWNER" 'NF && $0 != o' | sort -u | wc -l | tr -d ' ')
echo "waitlist discussion #$WAITLIST_NUM (\"$title\"): $waitlist unique humans" >&2

ideas=$(gh api graphql -f query='query($o:String!,$n:String!){ repository(owner:$o,name:$n){
  discussions(first:100, orderBy:{field:CREATED_AT, direction:ASC}){ nodes{ title upvoteCount category{ name } } } } }' \
  -f o="$OWNER" -f n="$NAME" --jq '[.data.repository.discussions.nodes[] | select(.category.name=="Ideas")]
  | sort_by(-.upvoteCount) | map("\(.title)=\(.upvoteCount)") | join("; ")') || { echo "Ideas query failed" >&2; exit 1; }

mkdir -p "$(dirname "$OUT")"
[[ -s "$OUT" ]] || echo "date,stars,release_downloads,views_14d_uniques,clones_14d_uniques,stranger_issues,waitlist_signals,ideas_by_upvotes" > "$OUT"
printf '%s,%s,%s,%s,%s,%s,%s,"%s"\n' "$today" "$stars" "$downloads" "$views" "$clones" "$stranger_issues" "$waitlist" "${ideas//\"/\"\"}" >> "$OUT"
tail -1 "$OUT"
