# Sanitized parity oracle derived from the review branch of the shell prototype.
# Keep its categorization behavior in sync with the prototype while passing
# tracker-specific values as fictional test arguments.
# Args: --arg repo owner/name --arg me login --arg issue_pattern regex
#       --arg issue_url_template URL-with-{id}
# A reviewer's standing review: the latest, except that a comment does not
# erase an earlier approval or change request (a later change request or
# dismissal still does). This deliberately diverges from the shell prototype,
# which takes the latest review of any kind; core/src/board.rs
# `standing_review` implements the same rule.
def standing:
  max_by(.submittedAt) as $last
  | ([.[] | select(.state != "COMMENTED")] | if length > 0 then max_by(.submittedAt) else null end) as $held
  | if $last.state == "COMMENTED" and $held != null
       and ($held.state == "APPROVED" or $held.state == "CHANGES_REQUESTED")
    then $held else $last end;
# Pickup age: when the PR started waiting for a reviewer (null when it is
# not waiting). A deliberate extension of the prototype; core/src/pickup.rs
# `pickup_since` implements the same rule. Timestamps are ISO-8601 UTC text,
# compared as text; logins and team slugs ignore ASCII case.
def requests: [.reviewRequests.nodes[].requestedReviewer | select(. != null)
  | if .login then {who: .login, team: false} elif .slug then {who: .slug, team: true} else empty end];
def request_time($r): [.timelineItems.nodes[]?
  | select(. != null and .__typename == "ReviewRequestedEvent" and .requestedReviewer != null)
  | select((if $r.team then .requestedReviewer.slug else .requestedReviewer.login end // "" | ascii_downcase)
           == ($r.who | ascii_downcase))
  | .createdAt] | max;
def ready_time: [.timelineItems.nodes[]? | select(. != null and .__typename == "ReadyForReviewEvent") | .createdAt] | max;
def pickup($start): [$start, ready_time] | max;
# Size band, also a deliberate extension; core/src/size.rs implements the
# same rule. null unless GitHub reported every count.
def size_band:
  if .additions == null or .deletions == null or .changedFiles == null then null
  else (.additions + .deletions) as $lines
    | if $lines > 400 or .changedFiles > 30 then "large"
      elif $lines <= 100 and .changedFiles <= 10 then "small"
      else "medium" end
  end;
[ .data.search.nodes[]
  | ([.labels.nodes[].name] | index("bug")) as $bug
  | ([.reviewThreads.nodes[] | select(.isResolved==false)] | length) as $unres
  | (.commits.nodes[0].commit.statusCheckRollup.state // "NONE") as $ci
  | (($ci=="FAILURE") or ($ci=="ERROR")) as $cifail
  | (.mergeable=="CONFLICTING") as $conflict
  | ([.reviews.nodes[] | select(.author.login==$me)]
      | (if length>0 then (standing.state) else "NONE" end)) as $mine
  | (if .isDraft then "draft"
     elif ($mine=="APPROVED" or $mine=="COMMENTED" or $mine=="CHANGES_REQUESTED") then "done"
     else "todo" end) as $cat
  | (if $cat != "todo" then null
     else . as $pr | requests as $rs
     # Asked by name: your latest request. Otherwise a requested team's latest.
     | (if ($rs | any(.team == false and (.who | ascii_downcase) == ($me | ascii_downcase)))
        then request_time({who: $me, team: false})
        else [$rs[] | select(.team) as $r | $pr | request_time($r)] | max end) as $asked
     | pickup($asked // .createdAt) end) as $waiting
  | ((.title | capture("(?<issue>" + $issue_pattern + ")").issue) // null) as $issue
  | {
      number: .number,
      url: "https://github.com/\($repo)/pull/\(.number)",
      title: (.title | gsub("^WIP\\s*";"") | gsub("^\\[" + $issue_pattern + "\\]\\s*";"")),
      issue: $issue,
      issueUrl: (if $issue then ($issue_url_template | gsub("\\{id\\}"; $issue)) else null end),
      author: .author.login,
      draft: .isDraft,
      category: $cat,
      bug: ($bug != null),
      ci: (if $ci=="SUCCESS" then "pass" elif $cifail then "fail" elif $ci=="NONE" then "none" else "running" end),
      conflict: $conflict,
      myReview: $mine,
      unresolved: $unres,
      createdAt: .createdAt,
      waitingSince: $waiting,
      sizeBand: size_band
    }
]
| sort_by( (if .category=="todo" then 0 elif .category=="done" then 1 else 2 end), .number )
