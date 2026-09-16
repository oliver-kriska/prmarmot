# Sanitized parity oracle derived from the authored branch of the shell
# prototype. Keep its categorization behavior in sync with the prototype while
# passing tracker-specific values as fictional test arguments.
# Args: --arg repo owner/name --arg me login --arg issue_pattern regex
#       --arg issue_url_template URL-with-{id}
def bots: ["chatgpt-codex-connector","github-actions"];
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
[ .data.search.nodes[]
  | ([.labels.nodes[].name] | index("bug")) as $bug
  | ([.reviewThreads.nodes[] | select(.isResolved==false)] | length) as $unres
  | (.commits.nodes[0].commit.statusCheckRollup.state // "NONE") as $ci
  | ([.reviewRequests.nodes[].requestedReviewer | (.login // .slug) | select(.!=null)]) as $req
  | ([.reviews.nodes[]
        | select(.author.login as $a | ($a!=$me) and (bots|index($a)|not))]
      | group_by(.author.login)
      | map({login:.[0].author.login, state:(standing.state)})) as $rv
  | ($rv | map(select(.state=="APPROVED")) | length) as $appr
  | ($rv | any(.state=="COMMENTED")) as $cmt
  | ($rv | any(.state=="CHANGES_REQUESTED")) as $chg
  | (.mergeable=="CONFLICTING") as $conflict
  | (($ci=="FAILURE") or ($ci=="ERROR")) as $cifail
  | (if $chg then "changes" elif $appr>0 then "approved" elif $cmt then "commented"
     elif ($req|length)>0 then "waiting" else "none" end) as $rflag
  | (if .isDraft then "draft"
     elif ($cifail or $conflict or (.reviewDecision=="CHANGES_REQUESTED") or ($unres>0) or ($rflag=="none")) then "action"
     else "await" end) as $cat
  | ((.title | capture("(?<issue>" + $issue_pattern + ")").issue) // null) as $issue
  | {
      number: .number,
      url: "https://github.com/\($repo)/pull/\(.number)",
      title: (.title | gsub("^WIP\\s*";"") | gsub("^\\[" + $issue_pattern + "\\]\\s*";"")),
      issue: $issue,
      issueUrl: (if $issue then ($issue_url_template | gsub("\\{id\\}"; $issue)) else null end),
      draft: .isDraft,
      category: $cat,
      bug: ($bug != null),
      ci: (if $ci=="SUCCESS" then "pass" elif $cifail then "fail" elif $ci=="NONE" then "none" else "running" end),
      conflict: $conflict,
      reviewDecision: (.reviewDecision // null),
      reviewState: $rflag,
      requested: $req,
      reviews: $rv,
      unresolved: $unres
    }
]
| sort_by( (if .category=="action" then 0 elif .category=="await" then 1 else 2 end), (- .number) )
