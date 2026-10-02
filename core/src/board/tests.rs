use super::*;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Fetch a complete board flow. Authored mode preserves the legacy single
/// search; review mode combines requested and unrequested candidates in one
/// GraphQL request, with requested rows first and deduplicated.
fn fetch_board(
    transport: &dyn GithubTransport,
    mode: Mode,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Result<BoardFetch, GhError> {
    fetch_board_scoped(
        transport,
        mode,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
    )
}

/// Fetch a queue for either one repository or every repository involving the
/// authenticated user. Both variants retain the same one-operation contract.
fn fetch_board_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
) -> Result<BoardFetch, GhError> {
    fetch_board_scoped_with_tracked(transport, mode, scope, me, cfg, Tracked::default(), false)
}

/// [`fetch_board_scoped`] plus followed PRs: [`fetch_view`] without a filter.
fn fetch_board_scoped_with_tracked(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    tracked: Tracked<'_>,
    small_pages: bool,
) -> Result<BoardFetch, GhError> {
    fetch_view(
        transport,
        mode,
        scope,
        me,
        cfg,
        &RemoteFilter::default(),
        tracked,
        small_pages,
    )
}

struct FakeTransport(serde_json::Value);

impl GithubTransport for FakeTransport {
    fn graphql(
        &self,
        query: &str,
        variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        assert_eq!(query, with_scope_repository(REVIEW_SEARCH_QUERY).unwrap());
        assert_eq!(variables.len(), 5);
        assert!(variables.contains(&("who", "me")));
        assert!(variables.iter().any(|(key, _)| *key == "scopeOwner"));
        assert!(variables.iter().any(|(key, _)| *key == "scopeName"));
        Ok(self.0.clone())
    }
}

struct SequenceTransport(Mutex<VecDeque<Result<serde_json::Value, GhError>>>);

impl SequenceTransport {
    fn new(values: Vec<Result<serde_json::Value, GhError>>) -> Self {
        Self(Mutex::new(values.into()))
    }
}

impl GithubTransport for SequenceTransport {
    fn graphql(
        &self,
        _query: &str,
        _variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        self.0.lock().unwrap().pop_front().unwrap()
    }
}

struct GlobalReviewTransport;

impl GithubTransport for GlobalReviewTransport {
    fn graphql(
        &self,
        query: &str,
        variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        assert_eq!(query, REVIEW_SEARCH_QUERY);
        assert!(variables.contains(&(
            "requested",
            "is:pr is:open review-requested:me -author:me sort:updated-desc"
        )));
        assert!(variables.contains(&(
            "available",
            "is:pr is:open involves:me -author:me sort:updated-desc"
        )));
        Ok(json!({"data": {
            "requested": {"pageInfo":{"hasNextPage":false}, "nodes":[]},
            "available": {"pageInfo":{"hasNextPage":false}, "nodes":[]},
            "rateLimit": null
        }}))
    }
}

fn cfg() -> BoardConfig {
    BoardConfig {
        default_reviewers: vec!["alice".into(), "bob".into()],
        issue_link: Some(
            IssueLinkRule::new("PROJ-[0-9]+", "https://tracker.example.test/issues/{id}").unwrap(),
        ),
        ..Default::default()
    }
}

fn pr(v: serde_json::Value) -> RawPr {
    serde_json::from_value(v).unwrap()
}

fn base(number: u64) -> serde_json::Value {
    json!({
        "number": number,
        "title": "Some change",
        "isDraft": false,
        "reviewDecision": null,
        "mergeable": "MERGEABLE",
        "createdAt": "2026-07-20T10:00:00Z",
        "author": {"login": "me"},
        "labels": {"nodes": []},
        "reviewRequests": {"nodes": []},
        "reviews": {"nodes": []},
        "reviewThreads": {"nodes": []},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
    })
}

fn derive_one(v: serde_json::Value, mode: Mode) -> BoardRow {
    derive_rows(&[pr(v)], mode, "acme/widgets", "me", &cfg())
        .into_iter()
        .next()
        .unwrap()
}

#[test]
fn a_token_refused_checks_and_teams_still_gets_its_board() {
    let refused = |path: serde_json::Value| {
        json!({"type": "FORBIDDEN", "path": path,
                   "message": "Resource not accessible by personal access token"})
    };
    // The shape GitHub returned to a fine-grained token on 2026-09-18: the
    // head commit node refused and null, once per pull request.
    let mut first = base(1);
    first["commits"] = json!({"nodes": [null]});
    first["reviewRequests"] = json!({"totalCount": 1, "nodes": [{"requestedReviewer": null}]});
    let mut second = base(2);
    second["commits"] = json!({"nodes": [null]});
    let rollup = |i: u64| json!(["search", "nodes", i, "commits", "nodes", 0]);
    let body = json!({
        "data": {
            "search": {"pageInfo": {"hasNextPage": false}, "nodes": [first, second]},
            "rateLimit": null
        },
        "errors": [
            refused(rollup(0)),
            refused(rollup(1)),
            refused(json!(["search", "nodes", 0, "reviewRequests", "nodes", 0, "requestedReviewer"])),
        ]
    });

    let fetch = fetch_board_scoped(
        &SequenceTransport::new(vec![Ok(body)]),
        Mode::Authored,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
    )
    .expect("refused fields are not a failed refresh");

    assert_eq!(fetch.rows.len(), 2);
    assert!(fetch.rows.iter().all(|row| row.ci == Ci::Hidden));
    let asked_a_team = fetch.rows.iter().find(|row| row.number == 1).unwrap();
    assert_eq!(
        asked_a_team.requested,
        vec![crate::github::access::HIDDEN_TEAM]
    );
    assert!(
        !asked_a_team.note.contains("No reviewers"),
        "a team the token cannot see was still asked: {}",
        asked_a_team.note
    );
    assert_eq!(
        fetch.access,
        AccessGaps {
            ci: 2,
            teams: 1,
            other: 0,
            pull_requests: 0
        }
    );
    let notice = crate::status::access_notice(&fetch.access).unwrap();
    assert!(notice.starts_with("This token can't read CI on 2 pull requests"));
    assert!(!notice.contains('\n'));
}

#[test]
fn a_refused_search_still_fails_and_says_so_once() {
    let refused = json!({"type": "FORBIDDEN", "path": ["search"],
                             "message": "Resource not accessible by personal access token"});
    let body = json!({"data": null, "errors": [refused.clone(), refused]});
    let error = fetch_board_scoped(
        &SequenceTransport::new(vec![Ok(body)]),
        Mode::Authored,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "GraphQL errors: Resource not accessible by personal access token"
    );
}

#[test]
fn stripping_note_glyphs_keeps_separators_but_drops_the_neutral_lead() {
    assert_eq!(strip_note_glyphs("· draft"), "draft");
    assert_eq!(
        strip_note_glyphs("· draft (not ready)"),
        "draft (not ready)"
    );
    assert_eq!(
        strip_note_glyphs("🔴 draft · merge conflict"),
        "draft · merge conflict"
    );
    assert_eq!(
        strip_note_glyphs("new commits since your review · · draft (not ready)"),
        "new commits since your review · draft (not ready)"
    );
    assert_eq!(
        strip_note_glyphs("🔴 merge conflict — rebase · 🔴 CI failing"),
        "merge conflict — rebase · CI failing"
    );
}

#[test]
fn no_reviewers_is_action_with_assign_note() {
    let row = derive_one(base(1), Mode::Authored);
    assert_eq!(row.category, Category::Action);
    assert_eq!(row.review_state, ReviewState::None);
    assert_eq!(row.note, "⚠️ no reviewers — assign alice + bob");
}

#[test]
fn defaults_are_organization_and_tracker_agnostic() {
    let mut v = base(14);
    v["title"] = json!("[PROJ-1234] Fix the crash");
    let row = derive_rows(
        &[pr(v)],
        Mode::Authored,
        "acme/widgets",
        "me",
        &BoardConfig::default(),
    )
    .into_iter()
    .next()
    .unwrap();

    assert!(row.issue.is_none());
    assert!(row.issue_url.is_none());
    assert_eq!(row.title, "[PROJ-1234] Fix the crash");
    assert_eq!(row.note, "⚠️ no reviewers");
}

#[test]
fn labels_carry_through_and_bug_is_derived() {
    let mut v = base(9);
    v["labels"]["nodes"] = json!([
        {"name": "bug"}, {"name": "backend"}, {"name": "P1"}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.labels, vec!["bug", "backend", "P1"]);
    assert!(row.bug);
    let no_labels = derive_one(base(10), Mode::Authored);
    assert!(no_labels.labels.is_empty());
    assert!(!no_labels.bug);
}

#[test]
fn approved_is_await_mergeable() {
    let mut v = base(2);
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.category, Category::Await);
    assert_eq!(row.review_state, ReviewState::Approved);
    assert_eq!(row.note, "🟢 approved — mergeable");
}

fn approved(number: u64) -> serde_json::Value {
    let mut v = base(number);
    v["reviewDecision"] = json!("APPROVED");
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    v
}

#[test]
fn an_approved_pr_is_mergeable_only_when_github_says_so() {
    let note = |status: &str, ci: &str| {
        let mut v = approved(2);
        v["mergeStateStatus"] = json!(status);
        v["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] = json!(ci);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Await, "{status} {ci}");
        row.note
    };
    assert_eq!(note("CLEAN", "SUCCESS"), "🟢 approved — mergeable");
    assert_eq!(note("HAS_HOOKS", "SUCCESS"), "🟢 approved — mergeable");
    // An approved PR with four checks still running is not mergeable yet.
    assert_eq!(note("UNSTABLE", "PENDING"), "🟢 approved — waiting for CI");
    assert_eq!(note("BLOCKED", "PENDING"), "🟢 approved — waiting for CI");
    assert_eq!(
        note("UNSTABLE", "SUCCESS"),
        "🟢 approved — checks not passing"
    );
    assert_eq!(
        note("BLOCKED", "SUCCESS"),
        "🟢 approved — blocked by branch rules"
    );
    assert_eq!(
        note("BEHIND", "SUCCESS"),
        "🟢 approved — branch out of date"
    );
    // Still computing: no claim either way.
    assert_eq!(note("UNKNOWN", "SUCCESS"), "🟢 approved");
    assert_eq!(note("DRAFT", "SUCCESS"), "🟢 approved");
    // Not reported (prototype fixtures): the prototype's wording.
    let row = derive_one(approved(2), Mode::Authored);
    assert!(row.merge_state.is_none());
    assert_eq!(row.note, "🟢 approved — mergeable");
}

#[test]
fn a_queued_pr_says_where_it_stands_in_the_merge_queue_not_mergeable() {
    let note = |state: &str, position: Option<u64>| {
        let mut v = approved(2);
        v["mergeStateStatus"] = json!("BLOCKED");
        v["mergeQueueEntry"] = json!({ "state": state, "position": position });
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Await, "{state}");
        (row.merge_queue, row.note)
    };
    // Queued: GitHub merges it in turn, so "press merge" would be wrong.
    let (queue, text) = note("QUEUED", Some(2));
    assert_eq!(
        queue,
        Some(MergeQueue {
            state: MergeQueueState::Queued,
            position: Some(2)
        })
    );
    assert_eq!(text, "🟢 approved — in merge queue, position 2");
    assert_eq!(
        note("AWAITING_CHECKS", Some(1)).1,
        "🟢 approved — in merge queue, position 1"
    );
    assert_eq!(
        note("MERGEABLE", Some(1)).1,
        "🟢 approved — in merge queue, position 1"
    );
    assert_eq!(note("LOCKED", Some(1)).1, "🟢 approved — merging");
    assert_eq!(
        note("UNMERGEABLE", None).1,
        "🟢 approved — merge queue couldn't merge it"
    );
    // A position GitHub did not give, or a state this build does not know.
    assert_eq!(note("QUEUED", None).1, "🟢 approved — in merge queue");
    let (queue, text) = note("SOMETHING_NEW", Some(3));
    assert_eq!(queue.map(|q| q.state), Some(MergeQueueState::Unknown));
    assert_eq!(text, "🟢 approved — in merge queue, position 3");

    // Not queued: null entry, and the merge-state wording as before.
    let mut v = approved(2);
    v["mergeStateStatus"] = json!("CLEAN");
    v["mergeQueueEntry"] = serde_json::Value::Null;
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.merge_queue, None);
    assert_eq!(row.note, "🟢 approved — mergeable");

    // The rebase tail still follows.
    let mut v = with_methods(approved(2), true, true, true);
    v["canBeRebased"] = json!(false);
    v["mergeQueueEntry"] = json!({ "state": "QUEUED", "position": 4 });
    assert_eq!(
        derive_one(v, Mode::Authored).note,
        "🟢 approved — in merge queue, position 4 · can't rebase"
    );
}
fn with_methods(
    mut v: serde_json::Value,
    merge: bool,
    squash: bool,
    rebase: bool,
) -> serde_json::Value {
    v["repository"] = json!({
        "nameWithOwner": "acme/widgets",
        "mergeCommitAllowed": merge,
        "squashMergeAllowed": squash,
        "rebaseMergeAllowed": rebase,
    });
    v
}

#[test]
fn a_branch_github_cannot_rebase_is_named_and_blocks_only_where_rebase_is_the_only_way() {
    let mut v = with_methods(approved(2), true, true, true);
    v["mergeStateStatus"] = json!("CLEAN");
    v["canBeRebased"] = json!(false);
    // A branch whose newest commit merges main, which rebase can't
    // replay; a merge commit or a squash still goes through.
    let row = derive_one(v.clone(), Mode::Authored);
    assert_eq!(row.category, Category::Await);
    assert!(row.cannot_rebase && !row.rebase_only);
    assert!(row.blockers.is_empty());
    assert_eq!(row.note, "🟢 approved — mergeable · can't rebase");

    let only_rebase = derive_one(with_methods(v.clone(), false, false, true), Mode::Authored);
    assert_eq!(only_rebase.category, Category::Action);
    assert_eq!(only_rebase.blockers, vec![Blocker::CannotRebase]);
    assert_eq!(only_rebase.note, "🔴 can't rebase — rebase locally");

    // Rebase merges not allowed: rebasing is not a way to merge here.
    let no_rebase = derive_one(with_methods(v.clone(), true, true, false), Mode::Authored);
    assert!(!no_rebase.cannot_rebase);
    assert_eq!(no_rebase.note, "🟢 approved — mergeable");

    // A conflict is the blocker; "can't rebase" would only repeat it.
    let mut conflicted = with_methods(v, false, false, true);
    conflicted["mergeable"] = json!("CONFLICTING");
    let conflicted = derive_one(conflicted, Mode::Authored);
    assert_eq!(conflicted.blockers, vec![Blocker::MergeConflict]);

    // Someone else's PR in a rebase-only repository says it as a fact.
    let mut theirs = with_methods(approved(3), false, false, true);
    theirs["author"] = json!({"login": "alice"});
    theirs["canBeRebased"] = json!(false);
    let theirs = derive_involving_rows(&[pr(theirs)], "acme/widgets", "me", &cfg())
        .pop()
        .unwrap();
    assert_eq!(theirs.category, Category::Action);
    assert_eq!(theirs.note, "alice's PR · can't rebase");
}

#[test]
fn action_note_combines_in_blocking_order() {
    let mut v = base(3);
    v["mergeable"] = json!("CONFLICTING");
    v["reviewDecision"] = json!("CHANGES_REQUESTED");
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
    v["reviewThreads"]["nodes"] = json!([
        {"isResolved": false}, {"isResolved": false}, {"isResolved": true}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.category, Category::Action);
    assert_eq!(
        row.note,
        "⚠️ no reviewers — assign alice + bob · 🔴 merge conflict — rebase · \
             ❌ CI failing · ✋ changes requested · 🟡 2 unresolved comments"
    );
}

#[test]
fn no_reviewer_blocker_carries_configured_reviewers() {
    let row = derive_one(base(1), Mode::Authored);
    assert_eq!(
        row.blockers,
        vec![Blocker::NoReviewers {
            suggested: vec!["alice".into(), "bob".into()]
        }]
    );
}

#[test]
fn reviewer_suggestions_prefer_the_repository_then_its_owner() {
    let mut cfg = cfg();
    cfg.repo_reviewers
        .insert("acme".into(), vec!["olga".into(), "oscar".into()]);
    cfg.repo_reviewers
        .insert("acme/widgets".into(), vec!["rita".into()]);
    cfg.repo_reviewers.insert("quiet/repo".into(), Vec::new());

    assert_eq!(cfg.suggested_reviewers("Acme/Widgets"), ["rita"]);
    assert_eq!(cfg.suggested_reviewers("acme/api"), ["olga", "oscar"]);
    assert_eq!(cfg.suggested_reviewers("other/repo"), ["alice", "bob"]);
    assert!(cfg.suggested_reviewers("quiet/repo").is_empty());

    let row = derive_rows(&[pr(base(1))], Mode::Authored, "acme/widgets", "me", &cfg)
        .into_iter()
        .next()
        .unwrap();
    assert!(row.note.contains("assign rita"), "{}", row.note);
}

#[test]
fn no_reviewer_blocker_is_empty_without_config() {
    let row = derive_rows(
        &[pr(base(1))],
        Mode::Authored,
        "acme/widgets",
        "me",
        &BoardConfig::default(),
    )
    .into_iter()
    .next()
    .unwrap();
    assert_eq!(
        row.blockers,
        vec![Blocker::NoReviewers { suggested: vec![] }]
    );
}

#[test]
fn kitchen_sink_row_lists_every_blocker_in_prototype_order() {
    let mut v = base(3);
    v["mergeable"] = json!("CONFLICTING");
    v["reviewDecision"] = json!("CHANGES_REQUESTED");
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
    v["reviewThreads"]["nodes"] = json!([
        {"isResolved": false}, {"isResolved": false}, {"isResolved": true}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(
        row.blockers,
        vec![
            Blocker::NoReviewers {
                suggested: vec!["alice".into(), "bob".into()]
            },
            Blocker::MergeConflict,
            Blocker::CiFailing,
            Blocker::ChangesRequested,
            Blocker::UnresolvedComments(2),
        ]
    );
    // And the generated note is byte-identical to the legacy wording.
    assert_eq!(
        row.note,
        "⚠️ no reviewers — assign alice + bob · 🔴 merge conflict — rebase · \
             ❌ CI failing · ✋ changes requested · 🟡 2 unresolved comments"
    );
}

#[test]
fn await_rows_have_no_blockers() {
    let mut v = base(2);
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.category, Category::Await);
    assert!(row.blockers.is_empty());
}

#[test]
fn approved_with_unresolved_is_still_action() {
    let mut v = base(4);
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "grace"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    v["reviewThreads"]["nodes"] = json!([{"isResolved": false}, {"isResolved": false}]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.category, Category::Action);
    assert_eq!(row.review_state, ReviewState::Approved);
    assert_eq!(row.note, "🟡 2 unresolved comments");

    let mut one = base(5);
    one["reviews"]["nodes"] = json!([
        {"author": {"login": "grace"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    one["reviewThreads"]["nodes"] = json!([{"isResolved": false}]);
    assert_eq!(
        derive_one(one, Mode::Authored).note,
        "🟡 1 unresolved comment"
    );
}

#[test]
fn bot_and_own_reviews_are_excluded() {
    let mut v = base(5);
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "github-actions"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:00:00Z"},
        {"author": {"login": "chatgpt-codex-connector"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:05:00Z"},
        {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:10:00Z"}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert!(row.reviews.is_empty());
    assert_eq!(row.review_state, ReviewState::None);
    assert_eq!(row.category, Category::Action);
}

#[test]
fn latest_review_per_author_wins() {
    let mut v = base(6);
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "eve"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:00:00Z"},
        {"author": {"login": "eve"}, "state": "APPROVED", "submittedAt": "2026-07-22T09:00:00Z"}
    ]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(
        row.reviews,
        vec![ReviewSummary {
            login: Some("eve".into()),
            state: "APPROVED".into(),
            submitted_at: Some("2026-07-22T09:00:00Z".into()),
        }]
    );
    assert_eq!(row.review_state, ReviewState::Approved);
}

#[test]
fn a_comment_does_not_erase_an_approval_or_a_change_request() {
    let mut v = base(60);
    let (t1, t2) = ("2026-07-21T09:00:00Z", "2026-07-22T09:00:00Z");
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "eve"}, "state": "APPROVED", "submittedAt": t1},
        {"author": {"login": "eve"}, "state": "COMMENTED", "submittedAt": t2},
        {"author": {"login": "fay"}, "state": "APPROVED", "submittedAt": t1},
        {"author": {"login": "fay"}, "state": "DISMISSED", "submittedAt": t2},
        {"author": {"login": "gus"}, "state": "DISMISSED", "submittedAt": t1},
        {"author": {"login": "gus"}, "state": "COMMENTED", "submittedAt": t2},
        {"author": {"login": "hal"}, "state": "CHANGES_REQUESTED", "submittedAt": t1},
        {"author": {"login": "hal"}, "state": "COMMENTED", "submittedAt": t2},
        {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": t1},
        {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": t2}
    ]);
    let row = derive_one(v.clone(), Mode::Authored);
    let standing: Vec<_> = row
        .reviews
        .iter()
        .map(|r| {
            (
                r.login.as_deref().unwrap(),
                r.state.as_str(),
                r.submitted_at.as_deref().unwrap(),
            )
        })
        .collect();
    // The standing review keeps its own time, so a later comment is not
    // reported as a new approval.
    assert_eq!(
        standing,
        vec![
            ("eve", "APPROVED", t1),
            ("fay", "DISMISSED", t2),
            ("gus", "COMMENTED", t2),
            ("hal", "CHANGES_REQUESTED", t1),
        ]
    );
    assert_eq!(row.review_state, ReviewState::Changes);
    // The viewer's own standing review is known in every mode.
    assert_eq!(row.my_review, Some(ReviewVerdict::Approved));
    let review = derive_one(v, Mode::Review);
    assert_eq!(review.my_review, Some(ReviewVerdict::Approved));
    assert_eq!(review.category, Category::Done);
    assert_eq!(review.note, "✅ you approved");
}

#[test]
fn team_slug_counts_as_requested_reviewer() {
    let mut v = base(7);
    v["reviewRequests"]["nodes"] = json!([
        {"requestedReviewer": {"__typename": "Team", "slug": "platform"}},
        {"requestedReviewer": null}
    ]);
    let row = derive_one(v.clone(), Mode::Authored);
    assert_eq!(row.requested, vec!["platform"]);
    assert_eq!(row.requested_teams, vec!["platform"]);
    assert_eq!(row.review_state, ReviewState::Waiting);
    assert_eq!(row.category, Category::Await);
    assert_eq!(
        row.note,
        "✅ awaiting review — team requested, nobody responded"
    );

    // A person asked by name is on the hook.
    let mut named = v.clone();
    named["reviewRequests"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"requestedReviewer": {"__typename": "User", "login": "alice"}}));
    let row = derive_one(named, Mode::Authored);
    assert_eq!(row.requested_teams, vec!["platform"]);
    assert_eq!(row.note, "✅ awaiting review");

    // Someone else's PR in the involving view says the same.
    let mut theirs = v.clone();
    theirs["author"] = json!({"login": "alice"});
    let rows = derive_involving_rows(&[pr(theirs)], "acme/widgets", "me", &cfg());
    assert_eq!(
        rows[0].note,
        "alice's PR · team requested, nobody responded"
    );

    // The review queue: asked only through the team, and nobody reviewed.
    v["author"] = json!({"login": "alice"});
    let row = derive_one(v.clone(), Mode::Review);
    assert_eq!(row.category, Category::Todo);
    assert_eq!(row.note, "🔵 team requested, nobody responded");
    let mut by_name = v.clone();
    by_name["reviewRequests"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"requestedReviewer": {"__typename": "User", "login": "Me"}}));
    assert_eq!(
        derive_one(by_name, Mode::Review).note,
        "🔵 needs your review"
    );
    let mut reviewed = v.clone();
    reviewed["reviews"]["nodes"] = json!([
        {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    assert_eq!(
        derive_one(reviewed, Mode::Review).note,
        "🔵 needs your review"
    );
}

#[test]
fn pickup_age_follows_the_row_not_the_prototype_view() {
    let mut v = base(12);
    v["author"] = json!({"login": "alice"});
    v["timelineItems"] = json!({"nodes": [
        {"__typename": "ReadyForReviewEvent", "createdAt": "2026-07-21T09:00:00Z"}
    ]});
    // Available to review: since opened, moved up to ready for review.
    let available = derive_review_row(
        &pr(v.clone()),
        "acme/widgets",
        "me",
        &cfg(),
        QueueProvenance::Available,
    );
    assert_eq!(available.category, Category::Available);
    assert_eq!(
        available.waiting_since.as_deref(),
        Some("2026-07-21T09:00:00Z")
    );
    // Once you reviewed it, it is not waiting on you.
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-07-22T10:00:00Z"}
    ]);
    let done = derive_review_row(
        &pr(v.clone()),
        "acme/widgets",
        "me",
        &cfg(),
        QueueProvenance::Available,
    );
    assert_eq!(done.category, Category::Done);
    assert_eq!(done.waiting_since, None);
    // Someone else's PR you reviewed has been picked up, although your
    // review does not count toward its review state.
    let rows = derive_involving_rows(&[pr(v.clone())], "acme/widgets", "me", &cfg());
    assert_eq!(rows[0].review_state, ReviewState::None);
    assert_eq!(rows[0].waiting_since, None);
    // Your own comment on your PR is not a pickup.
    v["author"] = json!({"login": "me"});
    let mine = derive_one(v, Mode::Authored);
    assert_eq!(mine.waiting_since.as_deref(), Some("2026-07-21T09:00:00Z"));

    let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T08:59:59Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        crate::pickup::waiting_secs(&mine, now),
        Some(3 * 86_400 - 1)
    );
    assert!(!crate::pickup::is_stale(&mine, now, 3));
    assert!(crate::pickup::is_stale(
        &mine,
        now + chrono::Duration::seconds(1),
        3
    ));
    assert!(crate::pickup::is_stale(&mine, now, 2));
    assert!(!crate::pickup::is_stale(&done, now, 0));
    // A clock behind GitHub's reads as no wait yet.
    let early = now - chrono::Duration::days(30);
    assert_eq!(crate::pickup::waiting_secs(&mine, early), Some(0));
}

#[test]
fn title_issue_extraction_and_stripping() {
    let mut v = base(8);
    v["title"] = json!("WIP [PROJ-1234] Fix the crash");
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.issue.as_deref(), Some("PROJ-1234"));
    assert_eq!(
        row.issue_url.as_deref(),
        Some("https://tracker.example.test/issues/PROJ-1234")
    );
    assert_eq!(row.title, "Fix the crash");
}

#[test]
fn a_matched_issue_id_is_percent_encoded_into_the_link() {
    let rule = IssueLinkRule::new(
        r"T-[a-z #?/]+[0-9]",
        "https://tracker.test/browse/{id}?view=1",
    )
    .unwrap();
    let cfg = BoardConfig {
        issue_link: Some(rule),
        ..cfg()
    };
    let mut v = base(9);
    v["title"] = json!("[T-a b#c?d/1] Odd ticket ids");
    let row = derive_rows(&[pr(v)], Mode::Authored, "acme/widgets", "me", &cfg)
        .into_iter()
        .next()
        .unwrap();
    // The ID reads as written; only the link escapes it.
    assert_eq!(row.issue.as_deref(), Some("T-a b#c?d/1"));
    assert_eq!(
        row.issue_url.as_deref(),
        Some("https://tracker.test/browse/T-a%20b%23c%3Fd%2F1?view=1")
    );
    assert_eq!(row.title, "Odd ticket ids");
    // Unreserved characters and non-ASCII (as UTF-8 bytes).
    assert_eq!(url_component("AZaz09-_.~"), "AZaz09-_.~");
    assert_eq!(url_component("é"), "%C3%A9");
}

#[test]
fn draft_notes_first_match_wins() {
    let mut v = base(9);
    v["isDraft"] = json!(true);
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
    assert_eq!(
        derive_one(v.clone(), Mode::Authored).note,
        "🔴 draft · CI failing"
    );
    v["mergeable"] = json!("CONFLICTING");
    assert_eq!(
        derive_one(v.clone(), Mode::Authored).note,
        "🔴 draft · merge conflict"
    );
    v["mergeable"] = json!("MERGEABLE");
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]);
    v["reviewThreads"]["nodes"] = json!([{"isResolved": false}]);
    assert_eq!(
        derive_one(v, Mode::Authored).note,
        "🟡 draft · 1 unresolved comment"
    );
}

#[test]
fn a_cancelled_check_is_not_a_failing_one() {
    let with_counts = |runs: serde_json::Value, statuses: serde_json::Value| {
        let mut v = base(11);
        v["reviewRequests"] =
            json!({"nodes": [{"requestedReviewer": {"__typename": "User", "login": "alice"}}]});
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {
            "state": "FAILURE",
            "contexts": {"checkRunCountsByState": runs, "statusContextCountsByState": statuses}
        }}}]);
        derive_one(v, Mode::Authored)
    };
    let counts = |pairs: &[(&str, u64)]| {
        json!(pairs
            .iter()
            .map(|(state, count)| json!({"state": state, "count": count}))
            .collect::<Vec<_>>())
    };

    // GitHub's rollup said FAILURE because one optional check was
    // cancelled; every check that ran passed.
    let row = with_counts(
        counts(&[("SUCCESS", 11), ("SKIPPED", 9), ("CANCELLED", 1)]),
        json!([]),
    );
    assert_eq!(row.ci, Ci::Pass);
    assert_eq!(row.category, Category::Await);
    assert!(!row.note.contains("CI failing"), "{}", row.note);
    assert_eq!(
        row.checks,
        Some(CheckCounts {
            passed: 11,
            skipped: 9,
            cancelled: 1,
            ..CheckCounts::default()
        })
    );

    // Something still running is running, not passing.
    let row = with_counts(counts(&[("CANCELLED", 1), ("IN_PROGRESS", 2)]), json!([]));
    assert_eq!(row.ci, Ci::Running);
    let row = with_counts(counts(&[("CANCELLED", 1)]), counts(&[("PENDING", 1)]));
    assert_eq!(row.ci, Ci::Running);

    // A real failure, a failing status, an unknown state, or nothing to go
    // on keeps GitHub's failure.
    for (runs, statuses) in [
        (counts(&[("CANCELLED", 1), ("FAILURE", 1)]), json!([])),
        (counts(&[("CANCELLED", 1), ("TIMED_OUT", 1)]), json!([])),
        (counts(&[("ACTION_REQUIRED", 1)]), json!([])),
        (counts(&[("CANCELLED", 1)]), counts(&[("ERROR", 1)])),
        (counts(&[("CANCELLED", 1), ("SOMETHING_NEW", 1)]), json!([])),
        (counts(&[("CANCELLED", 0), ("FAILURE", 0)]), json!([])),
        (json!(null), json!(null)),
    ] {
        let row = with_counts(runs.clone(), statuses.clone());
        assert_eq!(row.ci, Ci::Fail, "{runs} {statuses}");
        assert_eq!(row.category, Category::Action, "{runs} {statuses}");
        // The counts agree with the column: a failing check is counted as
        // failed, and so is a state this build does not know.
        if let Some(checks) = row.checks {
            assert!(checks.failed > 0, "{runs} {statuses}");
        }
    }
    let row = with_counts(
        counts(&[("CANCELLED", 1), ("SOMETHING_NEW", 2), ("STALE", 1)]),
        counts(&[("SUCCESS", 3), ("ERROR", 1)]),
    );
    assert_eq!(
        row.checks,
        Some(CheckCounts {
            failed: 3,
            passed: 3,
            cancelled: 1,
            stale: 1,
            ..CheckCounts::default()
        })
    );
    // Zero counts, or none, are no counts at all.
    assert_eq!(
        with_counts(counts(&[("CANCELLED", 0)]), json!(null)).checks,
        None
    );

    // Without counts (the prototype's query) the rollup alone decides.
    let mut v = base(12);
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
    let row = derive_one(v, Mode::Authored);
    assert_eq!(row.ci, Ci::Fail);
    assert_eq!(row.checks, None);
}

#[test]
fn review_mode_categories_and_notes() {
    // Not yet reviewed, green.
    let mut v = base(20);
    v["author"] = json!({"login": "alice"});
    let row = derive_one(v.clone(), Mode::Review);
    assert_eq!(row.category, Category::Todo);
    assert_eq!(row.my_review, Some(ReviewVerdict::NoReview));
    assert_eq!(row.note, "🔵 needs your review");

    // CI red beats conflicts.
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "ERROR"}}}]);
    v["mergeable"] = json!("CONFLICTING");
    assert_eq!(
        derive_one(v.clone(), Mode::Review).note,
        "⚠️ CI red — maybe wait for green"
    );

    // I approved → done.
    v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]);
    v["mergeable"] = json!("MERGEABLE");
    v["reviews"]["nodes"] = json!([
        {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    let row = derive_one(v.clone(), Mode::Review);
    assert_eq!(row.category, Category::Done);
    assert_eq!(row.note, "✅ you approved");

    // Draft trumps everything.
    v["isDraft"] = json!(true);
    assert_eq!(derive_one(v, Mode::Review).note, "· draft (not ready)");
}

#[test]
fn sort_orders_differ_by_mode() {
    let mk = |n: u64, draft: bool, reviewed: bool| {
        let mut v = base(n);
        v["isDraft"] = json!(draft);
        if reviewed {
            v["reviews"]["nodes"] = json!([
                {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
            ]);
        }
        pr(v)
    };
    // 10=action (no reviewers), 11=await (approved), 12=draft, 13=action
    let prs = vec![
        mk(10, false, false),
        mk(11, false, true),
        mk(12, true, false),
        mk(13, false, false),
    ];
    let authored = derive_rows(&prs, Mode::Authored, "acme/widgets", "me", &cfg());
    let order: Vec<u64> = authored.iter().map(|r| r.number).collect();
    assert_eq!(order, vec![13, 10, 11, 12]); // action desc, then await, then draft

    // Review mode: todo rows sort ascending, followed by rows I reviewed.
    // — for review-mode sorting use my own reviews instead:
    let mk_r = |n: u64, mine: bool| {
        let mut v = base(n);
        v["author"] = json!({"login": "alice"});
        if mine {
            v["reviews"]["nodes"] = json!([
                {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
            ]);
        }
        pr(v)
    };
    let prs = vec![mk_r(31, true), mk_r(30, false), mk_r(28, false)];
    let review = derive_rows(&prs, Mode::Review, "acme/widgets", "me", &cfg());
    let order: Vec<u64> = review.iter().map(|r| r.number).collect();
    assert_eq!(order, vec![28, 30, 31]); // todo asc, then done
}

#[test]
fn rows_are_bounded() {
    let prs: Vec<RawPr> = (0..100).map(|n| pr(base(n))).collect();
    let rows = derive_rows(&prs, Mode::Authored, "acme/widgets", "me", &cfg());
    assert_eq!(rows.len(), MAX_BOARD_ROWS);
}

#[test]
fn expanded_review_queue_filters_deduplicates_and_preserves_states() {
    let mut requested = base(10);
    requested["author"] = json!({"login": "alice"});
    requested["reviewRequests"] = json!({"totalCount": 1, "nodes": [
        {"requestedReviewer": {"__typename": "User", "login": "me"}}
    ]});
    requested["reviews"]["nodes"] = json!([
        {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-07-21T10:00:00Z"}
    ]);
    requested["labels"] = json!({"nodes": [{"name": "bug"}, {"name": "backend"}]});
    requested["stack"] = json!({"number": 70, "size": 3, "baseRefName": "main"});
    requested["stackEntry"] = json!({"position": 2});

    let mut duplicate = requested.clone();
    duplicate["title"] = json!("broad duplicate must lose");

    let mut available = base(11);
    available["author"] = json!({"login": "bob"});
    available["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
    available["labels"] = json!({"nodes": [{"name": "frontend"}]});
    available["stack"] = json!({"number": 70, "size": 3, "baseRefName": "main"});
    available["stackEntry"] = json!({"position": 3});

    let mut completed = base(12);
    completed["author"] = json!({"login": "carol"});
    completed["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
    completed["reviews"]["nodes"] = json!([{
        "author": {"login": "me"}, "state": "APPROVED",
        "submittedAt": "2026-07-21T10:00:00Z"
    }]);

    let mut draft = base(13);
    draft["author"] = json!({"login": "dave"});
    draft["isDraft"] = json!(true);
    draft["reviewRequests"] = json!({"totalCount": 0, "nodes": []});

    let mut own = base(14);
    own["reviewRequests"] = json!({"totalCount": 0, "nodes": []});

    let mut assigned_other = base(15);
    assigned_other["author"] = json!({"login": "eve"});
    assigned_other["reviewRequests"] = json!({
        "totalCount": 1,
        "nodes": [{"requestedReviewer": {"__typename": "User", "login": "other"}}]
    });

    let mut team_requested = base(16);
    team_requested["author"] = json!({"login": "frank"});
    team_requested["reviewRequests"] = json!({
        "totalCount": 1,
        "nodes": [{"requestedReviewer": {"__typename": "Team", "slug": "platform"}}]
    });

    let body = json!({
        "data": {
            "requested": {"pageInfo": {"hasNextPage": false}, "nodes": [requested]},
            "available": {"pageInfo": {"hasNextPage": false}, "nodes": [
                duplicate, available, completed, draft, own, assigned_other, team_requested
            ]},
            "rateLimit": null
        }
    });
    let fetched = fetch_board(
        &FakeTransport(body),
        Mode::Review,
        "acme/widgets",
        "me",
        &cfg(),
    )
    .unwrap();

    assert_eq!(
        fetched.rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        vec![10, 11, 12, 13]
    );
    assert_eq!(fetched.rows[0].category, Category::Todo);
    assert_eq!(
        fetched.rows[0].queue_provenance,
        Some(QueueProvenance::Requested)
    );
    assert_eq!(fetched.rows[0].requested, vec!["me"]);
    assert_eq!(fetched.rows[0].reviews.len(), 1);
    assert_eq!(fetched.rows[0].reviews[0].login.as_deref(), Some("bob"));
    assert_eq!(fetched.rows[0].reviews[0].state, ReviewVerdict::Commented);
    assert_eq!(fetched.rows[0].labels, vec!["bug", "backend"]);
    assert!(fetched.rows[0].bug);
    assert_eq!(fetched.rows[0].stack.as_ref().unwrap().position, Some(2));
    assert_eq!(fetched.rows[1].category, Category::Available);
    assert_eq!(fetched.rows[1].labels, vec!["frontend"]);
    assert!(!fetched.rows[1].bug);
    assert_eq!(fetched.rows[1].stack.as_ref().unwrap().number, 70);
    assert_eq!(fetched.rows[1].stack.as_ref().unwrap().position, Some(3));
    assert_eq!(fetched.rows[2].category, Category::Done);
    assert_eq!(fetched.rows[3].category, Category::Draft);
    assert!(!fetched.truncated);
}

#[test]
fn expanded_review_queue_surfaces_alias_truncation() {
    let body = json!({"data": {
        "requested": {"pageInfo": {"hasNextPage": true}, "nodes": []},
        "available": {"pageInfo": {"hasNextPage": false}, "nodes": []},
        "rateLimit": null
    }});
    let fetched = fetch_board(
        &FakeTransport(body),
        Mode::Review,
        "acme/widgets",
        "me",
        &cfg(),
    )
    .unwrap();
    assert!(fetched.truncated);
}

#[test]
fn global_review_keeps_available_candidates_involvement_scoped() {
    let fetched = fetch_board_scoped(
        &GlobalReviewTransport,
        Mode::Review,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
    )
    .unwrap();
    assert!(fetched.rows.is_empty());
    assert!(!fetched.truncated);
}

#[test]
fn paginated_review_deduplicates_with_requested_priority_and_skips_exhausted_alias() {
    let mut available_duplicate = base(20);
    available_duplicate["author"] = json!({"login": "alice"});
    available_duplicate["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
    let first = json!({"data": {
        "requested": {"pageInfo": {"hasNextPage": false, "endCursor": "r1"}, "nodes": []},
        "available": {"pageInfo": {"hasNextPage": true, "endCursor": "a1"}, "nodes": [available_duplicate]},
        "rateLimit": null
    }});
    let mut requested_duplicate = base(20);
    requested_duplicate["author"] = json!({"login": "alice"});
    requested_duplicate["reviewRequests"] = json!({"totalCount": 1, "nodes": []});
    // The only live alias is available; a requested-looking result there is
    // filtered, proving an exhausted requested alias was not refetched.
    let second = json!({"data": {
        "available": {"pageInfo": {"hasNextPage": false, "endCursor": "a2"}, "nodes": [requested_duplicate]},
        "rateLimit": null
    }});
    let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
    let initial = fetch_board(&transport, Mode::Review, "acme/widgets", "me", &cfg()).unwrap();
    let fetched = fetch_more_board(
        &transport,
        Mode::Review,
        "acme/widgets",
        "me",
        &cfg(),
        &initial,
    )
    .unwrap();
    assert_eq!(fetched.rows.len(), 1);
    assert_eq!(
        fetched.rows[0].queue_provenance,
        Some(QueueProvenance::Available)
    );
    assert!(!fetched.pagination.can_load_more(Mode::Review));
}

#[test]
fn requested_page_replaces_an_available_duplicate() {
    let mut broad = base(30);
    broad["author"] = json!({"login": "alice"});
    broad["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
    let first = json!({"data": {
        "requested": {"pageInfo": {"hasNextPage": true, "endCursor": "r1"}, "nodes": []},
        "available": {"pageInfo": {"hasNextPage": false, "endCursor": "a1"}, "nodes": [broad]},
        "rateLimit": null
    }});
    let mut requested = base(30);
    requested["author"] = json!({"login": "alice"});
    requested["reviewRequests"] = json!({"totalCount": 1, "nodes": []});
    let second = json!({"data": {
        "requested": {"pageInfo": {"hasNextPage": false, "endCursor": "r2"}, "nodes": [requested]},
        "rateLimit": null
    }});
    let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
    let initial = fetch_board(&transport, Mode::Review, "acme/widgets", "me", &cfg()).unwrap();
    let fetched = fetch_more_board(
        &transport,
        Mode::Review,
        "acme/widgets",
        "me",
        &cfg(),
        &initial,
    )
    .unwrap();
    assert_eq!(fetched.rows.len(), 1);
    assert_eq!(
        fetched.rows[0].queue_provenance,
        Some(QueueProvenance::Requested)
    );
}

#[test]
fn authored_pagination_advances_and_missing_cursor_is_terminal() {
    let first = json!({"data": {
        "search": {"pageInfo": {"hasNextPage": true, "endCursor": "p1"}, "nodes": [base(2)]},
        "rateLimit": null
    }});
    let second = json!({"data": {
        "search": {"pageInfo": {"hasNextPage": true, "endCursor": null}, "nodes": [base(1)]},
        "rateLimit": null
    }});
    let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
    let initial = fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
    let fetched = fetch_more_board(
        &transport,
        Mode::Authored,
        "acme/widgets",
        "me",
        &cfg(),
        &initial,
    )
    .unwrap();
    assert_eq!(
        fetched.rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert!(!fetched.pagination.can_load_more(Mode::Authored));
}

#[test]
fn pagination_stops_at_five_pages_and_errors_do_not_mutate_input() {
    let page = |cursor: &str| {
        json!({"data": {
            "search": {"pageInfo": {"hasNextPage": true, "endCursor": cursor}, "nodes": []},
            "rateLimit": null
        }})
    };
    let transport = SequenceTransport::new(vec![
        Ok(page("p1")),
        Ok(page("p2")),
        Ok(page("p3")),
        Ok(page("p4")),
        Ok(page("p5")),
    ]);
    let mut fetched =
        fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
    for _ in 0..4 {
        fetched = fetch_more_board(
            &transport,
            Mode::Authored,
            "acme/widgets",
            "me",
            &cfg(),
            &fetched,
        )
        .unwrap();
    }
    assert!(!fetched.pagination.can_load_more(Mode::Authored));
    assert!(fetched.pagination.page_limit_reached(Mode::Authored));

    let error_seed = SequenceTransport::new(vec![Ok(page("e1"))]);
    let before_error =
        fetch_board(&error_seed, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
    let error_transport = SequenceTransport::new(vec![Err(GhError::Network("offline".into()))]);
    let before = before_error.rows.len();
    assert!(fetch_more_board(
        &error_transport,
        Mode::Authored,
        "acme/widgets",
        "me",
        &cfg(),
        &before_error
    )
    .is_err());
    assert_eq!(before_error.rows.len(), before);
}

#[test]
fn stack_metadata_is_native_and_labels_do_not_infer_it() {
    let mut stacked = base(40);
    stacked["stack"] = json!({"number": 7, "size": 3, "baseRefName": "main"});
    stacked["stackEntry"] = json!({"position": 2});
    let row = derive_one(stacked, Mode::Authored);
    assert_eq!(
        row.stack,
        Some(StackInfo {
            number: 7,
            size: 3,
            base_ref_name: "main".into(),
            position: Some(2),
        })
    );

    let mut no_position = base(42);
    no_position["stack"] = json!({"number": 8, "size": 2, "baseRefName": "develop"});
    assert_eq!(
        derive_one(no_position, Mode::Authored)
            .stack
            .unwrap()
            .position,
        None
    );

    let mut label_only = base(41);
    label_only["labels"]["nodes"] = json!([{"name": "stack"}]);
    assert!(derive_one(label_only, Mode::Authored).stack.is_none());
}

#[test]
fn available_review_keeps_health_warning() {
    let mut pr: RawPr = serde_json::from_value(base(99)).unwrap();
    pr.mergeable = Some("CONFLICTING".into());
    let row = derive_review_row(
        &pr,
        "acme/widgets",
        "reviewer",
        &cfg(),
        QueueProvenance::Available,
    );
    assert_eq!(row.category, Category::Available);
    assert_eq!(row.note, "⚠️ has conflicts");
}

#[test]
fn semantic_observation_is_stable_across_scope_and_queue_derivations() {
    let mut value = base(120);
    value["id"] = json!("PR_shared");
    value["url"] = json!("https://github.com/acme/widgets/pull/120");
    value["repository"] = json!({"nameWithOwner": "acme/widgets"});
    value["author"] = json!({"login": "alice"});
    value["updatedAt"] = json!("2026-09-11T10:00:00Z");
    value["headRefOid"] = json!("head-2");
    value["reviewRequests"] = json!({
        "totalCount": 1,
        "nodes": [{"requestedReviewer": {"__typename": "User", "login": "me"}}]
    });
    value["reviews"]["nodes"] = json!([
        {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z"},
        {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-09-09T10:00:00Z"}
    ]);
    value["latestReview"] = json!({"nodes": [{
        "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z", "commit": {"oid": "head-1"}
    }]});
    let raw: RawPr = serde_json::from_value(value).unwrap();
    let involving = derive_involving_row(&raw, "", "me", &cfg());
    let review = derive_review_row(
        &raw,
        "acme/widgets",
        "me",
        &cfg(),
        QueueProvenance::Requested,
    );
    assert_ne!(involving.category, review.category);
    assert_eq!(
        crate::attention::Observation::from_row(&involving),
        crate::attention::Observation::from_row(&review)
    );
}

#[test]
fn review_note_reports_new_head_without_fabricating_commit_count() {
    let mut value = base(121);
    value["author"] = json!({"login": "alice"});
    value["headRefOid"] = json!("current-head");
    value["latestReview"] = json!({"nodes": [{
        "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z", "commit": {"oid": "reviewed-head"}
    }]});
    value["reviews"]["nodes"] = json!([
        {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z"}
    ]);
    let row = derive_one(value, Mode::Review);
    assert_eq!(row.note, "new commits since your review · ✅ you approved");
    assert!(!row.note.chars().any(|character| character.is_ascii_digit()));
}

#[test]
fn a_missing_repository_is_an_error_and_a_missing_tracked_pr_is_inaccessible() {
    struct Scoped;
    impl GithubTransport for Scoped {
        fn graphql(
            &self,
            _query: &str,
            _variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            unreachable!("tracked ids are always requested here")
        }
        fn graphql_with_ids(
            &self,
            query: &str,
            variables: &[(&str, &str)],
            ids: &[String],
        ) -> Result<serde_json::Value, GhError> {
            assert!(query.contains("scopeRepository: repository("));
            assert_eq!(ids, ["PR_gone"]);
            let found = variables.contains(&("scopeName", "widgets"));
            assert!(variables.contains(&("scopeOwner", "acme")));
            let mut errors = vec![json!({"type": "NOT_FOUND", "path": ["tracked", 0],
                    "message": "Could not resolve to a node with the global id of 'PR_gone'."})];
            if !found {
                errors.push(json!({"type": "NOT_FOUND", "path": ["scopeRepository"],
                        "message": "Could not resolve to a Repository with the name 'acme/nope'."}));
            }
            Ok(json!({
                "data": {
                    "scopeRepository": if found { json!({"nameWithOwner": "acme/widgets"}) } else { json!(null) },
                    "search": {"pageInfo": {"hasNextPage": false}, "nodes": []},
                    "tracked": [null],
                    "rateLimit": null
                },
                "errors": errors
            }))
        }
    }
    let tracked = ["PR_gone".to_owned()];
    let scope = BoardScope::Repository("acme/widgets".into());
    let fetched = fetch_board_scoped_with_tracked(
        &Scoped,
        Mode::Authored,
        &scope,
        "me",
        &cfg(),
        Tracked::rows(&tracked),
        false,
    )
    .unwrap();
    assert_eq!(fetched.tracked.len(), 1);
    assert_eq!(fetched.tracked[0].status, TrackedPrStatus::Inaccessible);

    let scope = BoardScope::Repository("acme/nope".into());
    let error = fetch_board_scoped_with_tracked(
        &Scoped,
        Mode::Review,
        &scope,
        "me",
        &cfg(),
        Tracked::rows(&tracked),
        false,
    )
    .unwrap_err();
    assert_eq!(error, GhError::RepositoryNotFound("acme/nope".into()));
    assert_eq!(
        error.to_string(),
        "repository acme/nope not found, or the gh account can't access it"
    );
}

#[test]
fn repository_identity_survives_alias_dedup_and_page_merges() {
    let make = |repo: &str, id: &str| {
        let mut v = base(42);
        v["id"] = json!(id);
        v["repository"] = json!({"nameWithOwner": repo});
        v["url"] = json!(format!("https://github.com/{repo}/pull/42"));
        v["author"] = json!({"login": "alice"});
        v["reviewRequests"] = json!({"totalCount":0,"nodes":[]});
        v
    };
    let a = make("acme/one", "PR_a");
    let b = make("acme/two", "PR_b");
    let body = json!({"data": {
        "requested": {"nodes": [a.clone()]},
        "available": {"nodes": [a.clone(), b.clone()]}
    }});
    let fetched = fetch_board(
        &FakeTransport(body),
        Mode::Review,
        "ignored/repo",
        "me",
        &cfg(),
    )
    .unwrap();
    assert_eq!(fetched.rows.len(), 2);
    assert_eq!(fetched.rows[0].repo, "acme/one");
    assert_eq!(fetched.rows[1].url, "https://github.com/acme/two/pull/42");
    let a: RawPr = serde_json::from_value(a).unwrap();
    let mut b: RawPr = serde_json::from_value(b).unwrap();
    let mut authored = derive_rows(
        std::slice::from_ref(&a),
        Mode::Authored,
        "ignored/repo",
        "me",
        &cfg(),
    );
    merge_authored(&mut authored, &[b.clone()], "ignored/repo", "me", &cfg());
    assert_eq!(authored.len(), 2);
    b.title = "changed title".into();
    merge_authored(&mut authored, &[b.clone()], "ignored/repo", "me", &cfg());
    assert_eq!(authored.len(), 2);
    assert_eq!(
        authored.iter().find(|r| r.id == "PR_b").unwrap().title,
        "changed title"
    );
    let mut review = fetched.rows;
    merge_review(&mut review, &[b], &[a], "ignored/repo", "me", &cfg());
    assert_eq!(review.len(), 2);
    assert!(review
        .iter()
        .all(|r| r.queue_provenance == Some(QueueProvenance::Requested)));
}

#[test]
fn observation_fields_use_canonical_data_and_latest_review_alias() {
    let mut v = base(42);
    v["id"] = json!("PR_42");
    v["repository"] = json!({"nameWithOwner": "acme/actual"});
    v["updatedAt"] = json!("2026-09-11T10:00:00Z");
    v["headRefOid"] = json!("new-head");
    v["latestReview"] = json!({"nodes": [{"state":"APPROVED", "submittedAt":"2026-09-10T10:00:00Z", "commit":{"oid":"reviewed-head"}}]});
    let row = derive_one(v.clone(), Mode::Review);
    assert_eq!(row.repo, "acme/actual");
    assert_eq!(row.url, "https://github.com/acme/actual/pull/42");
    assert_eq!(row.updated_at.as_deref(), Some("2026-09-11T10:00:00Z"));
    assert_eq!(row.head_oid.as_deref(), Some("new-head"));
    assert_eq!(row.reviewed_oid.as_deref(), Some("reviewed-head"));
    assert_eq!(row.reviewed_at.as_deref(), Some("2026-09-10T10:00:00Z"));
    v["latestReview"]["nodes"][0]["commit"] = json!(null);
    assert_eq!(derive_one(v.clone(), Mode::Review).reviewed_oid, None);
    v["latestReview"]["nodes"][0]["commit"] = json!({"oid":"dismissed-head"});
    v["latestReview"]["nodes"][0]["state"] = json!("DISMISSED");
    assert_eq!(derive_one(v, Mode::Review).reviewed_oid, None);
}

#[test]
fn authored_only_narrows_the_all_repositories_search_to_your_prs() {
    struct Searches(Mutex<Vec<String>>);
    impl GithubTransport for Searches {
        fn graphql(
            &self,
            _query: &str,
            variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            let q = variables.iter().find(|(key, _)| *key == "q").unwrap().1;
            self.0.lock().unwrap().push(q.to_owned());
            Ok(json!({"data": {
                "search": {"pageInfo": {"hasNextPage": false}, "nodes": []},
                "rateLimit": null
            }}))
        }
    }
    let searches = Searches(Mutex::new(Vec::new()));
    let all = BoardScope::AllRepositories;
    let mut config = cfg();
    fetch_board_scoped(&searches, Mode::Authored, &all, "me", &config).unwrap();
    config.authored_only = true;
    fetch_board_scoped(&searches, Mode::Authored, &all, "me", &config).unwrap();
    let repo = BoardScope::Repository("acme/widgets".into());
    fetch_board_scoped(&searches, Mode::Authored, &repo, "me", &config).unwrap();
    assert_eq!(
        *searches.0.lock().unwrap(),
        [
            "is:pr is:open involves:me sort:updated-desc",
            "is:pr is:open author:me sort:updated-desc",
            "repo:acme/widgets is:pr is:open author:me",
        ]
    );
}

/// Records every search string and answers with `pages` in turn.
struct AllOpenSearches {
    seen: Mutex<Vec<(String, Option<String>)>>,
    requested: Mutex<Vec<String>>,
    pages: Mutex<VecDeque<serde_json::Value>>,
}

impl AllOpenSearches {
    fn new(pages: Vec<serde_json::Value>) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            requested: Mutex::new(Vec::new()),
            pages: Mutex::new(pages.into()),
        }
    }

    fn answer(&self, variables: &[(&str, &str)]) -> serde_json::Value {
        let get = |name: &str| {
            variables
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        };
        if let Some(requested) = get("requested") {
            self.requested.lock().unwrap().push(requested);
        }
        self.seen
            .lock()
            .unwrap()
            .push((get("q").unwrap(), get("after")));
        self.pages.lock().unwrap().pop_front().unwrap()
    }
}

impl GithubTransport for AllOpenSearches {
    fn graphql(
        &self,
        query: &str,
        variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        assert!(
            query.contains("issueCount"),
            "the count comes with the page"
        );
        Ok(self.answer(variables))
    }
}

fn all_open_page(
    nodes: Vec<serde_json::Value>,
    count: u64,
    next: Option<&str>,
) -> serde_json::Value {
    json!({"data": {
        "scopeRepository": {"nameWithOwner": "acme/widgets"},
        "search": {
            "issueCount": count,
            "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            "nodes": nodes
        },
        "rateLimit": null
    }})
}

fn someone_elses(number: u64, author: &str, updated: &str) -> serde_json::Value {
    let mut v = base(number);
    v["id"] = json!(format!("PR_{number}"));
    v["author"] = json!({"login": author});
    v["updatedAt"] = json!(updated);
    v
}

#[test]
fn all_open_searches_one_repository_by_last_update_and_counts_what_is_not_loaded() {
    let transport = AllOpenSearches::new(vec![
        all_open_page(
            vec![someone_elses(7, "alice", "2026-09-20T10:00:00Z")],
            412,
            Some("c1"),
        ),
        all_open_page(
            vec![someone_elses(5, "bob", "2026-09-19T10:00:00Z")],
            412,
            None,
        ),
    ]);
    let first = fetch_all_open(
        &transport,
        "acme/widgets",
        "me",
        &cfg(),
        &RemoteFilter::default(),
        Tracked::default(),
        false,
    )
    .unwrap();
    assert_eq!(first.total, Some(412));
    assert!(first.truncated);
    assert!(first.pagination.can_load_more(Mode::AllOpen));

    let more = fetch_more_board(
        &transport,
        Mode::AllOpen,
        "acme/widgets",
        "me",
        &cfg(),
        &first,
    )
    .unwrap();
    assert_eq!(
        more.rows.iter().map(|row| row.number).collect::<Vec<_>>(),
        [7, 5],
        "a later page lands below the rows already shown"
    );
    assert!(!more.truncated);
    assert!(!more.pagination.can_load_more(Mode::AllOpen));
    let search = "repo:acme/widgets is:pr is:open sort:updated-desc";
    assert_eq!(
        *transport.seen.lock().unwrap(),
        [
            (search.to_owned(), None),
            (search.to_owned(), Some("c1".to_owned())),
        ]
    );
}

#[test]
fn all_open_sends_label_and_author_chips_to_github_and_pages_the_same_search() {
    use crate::search::{FilterChip, Qualifier};
    let filter = RemoteFilter::from_chips(&[
        FilterChip::new(Qualifier::Label, "Needs Review"),
        FilterChip::new(Qualifier::Author, "Alice"),
    ]);
    let transport = AllOpenSearches::new(vec![
        all_open_page(Vec::new(), 90, Some("c1")),
        all_open_page(Vec::new(), 90, None),
    ]);
    let first = fetch_all_open(
        &transport,
        "acme/widgets",
        "me",
        &cfg(),
        &filter,
        Tracked::default(),
        false,
    )
    .unwrap();
    // A filter change is a new search; the cursor it hands out belongs to
    // this one, so Load more repeats it word for word.
    fetch_more_board(
        &transport,
        Mode::AllOpen,
        "acme/widgets",
        "me",
        &cfg(),
        &first,
    )
    .unwrap();
    let search = "repo:acme/widgets is:pr is:open sort:updated-desc \
                      label:\"needs review\" author:alice author:app/alice";
    let seen = transport.seen.lock().unwrap();
    assert_eq!(seen[0].0, search);
    assert_eq!(seen[1], (search.to_owned(), Some("c1".to_owned())));
    assert_eq!(
        *transport.requested.lock().unwrap(),
        [search_string(Mode::Review, "acme/widgets", "me")],
        "page one asks the review queue's own question, unfiltered; Load more does not"
    );
}

#[test]
fn a_later_page_reads_review_requests_from_page_one() {
    let mut first = all_open_page(
        vec![someone_elses(7, "alice", "2026-09-20T10:00:00Z")],
        2,
        Some("c1"),
    );
    first["data"]["requested"] = json!({"nodes": [{"id": "PR_5"}]});
    let transport = AllOpenSearches::new(vec![
        first,
        all_open_page(
            vec![someone_elses(5, "bob", "2026-09-19T10:00:00Z")],
            2,
            None,
        ),
    ]);
    let page = fetch_all_open(
        &transport,
        "acme/widgets",
        "me",
        &cfg(),
        &RemoteFilter::default(),
        Tracked::default(),
        false,
    )
    .unwrap();
    let more = fetch_more_board(
        &transport,
        Mode::AllOpen,
        "acme/widgets",
        "me",
        &cfg(),
        &page,
    )
    .unwrap();
    let bobs = more.rows.iter().find(|row| row.number == 5).unwrap();
    assert_eq!(bobs.category, Category::Todo);
}

#[test]
fn all_open_asks_for_a_repository_rather_than_searching_everything() {
    let error = fetch_board_scoped(
        &SequenceTransport::new(Vec::new()),
        Mode::AllOpen,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
    )
    .unwrap_err();
    assert_eq!(error, GhError::NeedsRepository);
    assert_eq!(
        error.to_string(),
        "Pick a repository to see all of its open PRs."
    );
}

#[test]
fn all_open_rows_read_as_yours_as_asked_of_you_or_in_the_authors_name() {
    let mut mine = base(1);
    mine["id"] = json!("mine");
    mine["updatedAt"] = json!("2026-09-18T10:00:00Z");
    let mut asks_me = someone_elses(2, "alice", "2026-09-19T10:00:00Z");
    asks_me["reviewRequests"] = json!({"totalCount": 1, "nodes": [
        {"requestedReviewer": {"__typename": "User", "login": "Me"}}
    ]});
    let mut asks_a_team = someone_elses(3, "bob", "2026-09-20T10:00:00Z");
    asks_a_team["reviewRequests"] = json!({"totalCount": 1, "nodes": [
        {"requestedReviewer": {"__typename": "Team", "slug": "platform"}}
    ]});
    asks_a_team["mergeable"] = json!("CONFLICTING");
    let mut draft = someone_elses(4, "carol", "2026-09-21T10:00:00Z");
    draft["isDraft"] = json!(true);
    draft["reviewRequests"] = asks_me["reviewRequests"].clone();

    let prs = [pr(mine), pr(asks_me), pr(asks_a_team), pr(draft)];
    let rows = derive_rows(&prs, Mode::AllOpen, "acme/widgets", "me", &cfg());
    assert_eq!(
        rows.iter().map(|row| row.number).collect::<Vec<_>>(),
        [4, 3, 2, 1],
        "most recently updated first, as GitHub sorted them"
    );
    let by_number = |n: u64| rows.iter().find(|row| row.number == n).unwrap();
    assert_eq!(by_number(1).note, "⚠️ no reviewers — assign alice + bob");
    assert_eq!(by_number(2).category, Category::Todo);
    assert_eq!(by_number(2).note, "🔵 needs your review");
    let team = by_number(3);
    assert_ne!(
        team.category,
        Category::Todo,
        "unless the review queue's search says a team of yours was asked"
    );
    assert_eq!(
        team.note, "merge conflict",
        "the Author column already says whose it is"
    );
    assert!(
        team.blockers.is_empty(),
        "someone else's conflict is not your blocker"
    );
    assert_eq!(by_number(4).category, Category::Draft);
    let need_you = |rows: &[BoardRow]| {
        rows.iter()
            .filter(|row| crate::status::row_needs_you(row))
            .count()
    };
    assert_eq!(
        need_you(&rows),
        2,
        "your own PR with no reviewers and the review asked of you; bob's conflict is his"
    );

    // The review queue's `review-requested:` search counts your teams;
    // what it returns is "Requested from you" in All open too.
    let rows = derive_all_open_rows(&prs, "acme/widgets", "me", &cfg(), &["PR_3".into()]);
    let team = rows.iter().find(|row| row.number == 3).unwrap();
    assert_eq!(team.category, Category::Todo);
    assert_eq!(team.queue_provenance, Some(QueueProvenance::Requested));
    assert_eq!(
        need_you(&rows),
        3,
        "your own PR, the review asked of you by name, and your team's"
    );
}

/// Your own review is not in `review_state`, and GitHub drops your
/// request once you review, so a teammate's PR that only you reviewed
/// reads as nobody asked and nobody reviewed. It stays under Awaiting
/// review rather than Available to review ("no review yet").
#[test]
fn a_teammates_pr_you_reviewed_is_not_available_to_review() {
    let unasked = someone_elses(1, "alice", "2026-09-19T10:00:00Z");
    let mut reviewed = someone_elses(2, "alice", "2026-09-20T10:00:00Z");
    reviewed["reviews"]["nodes"] = json!([
        {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-20T09:00:00Z"}
    ]);
    let prs = [pr(unasked), pr(reviewed)];
    for (mode, rows) in [
        (
            Mode::AllOpen,
            derive_all_open_rows(&prs, "acme/widgets", "me", &cfg(), &[]),
        ),
        (
            Mode::Authored,
            derive_involving_rows(&prs, "acme/widgets", "me", &cfg()),
        ),
    ] {
        let by_number = |n: u64| rows.iter().find(|row| row.number == n).unwrap();
        let reviewed = by_number(2);
        assert_eq!(
            (reviewed.review_state, reviewed.my_review.clone()),
            (ReviewState::None, Some(ReviewVerdict::Approved)),
            "{mode:?}"
        );
        assert!(
            !crate::layout::is_available_section(mode, reviewed),
            "{mode:?}"
        );
        assert!(
            crate::layout::is_available_section(mode, by_number(1)),
            "{mode:?}"
        );
    }
}

#[test]
fn global_involving_rows_keep_own_notes_and_describe_other_authors() {
    let mut mine = base(1);
    mine["id"] = json!("mine");
    mine["repository"] = json!({"nameWithOwner":"acme/one"});
    let mut theirs = base(2);
    theirs["id"] = json!("theirs");
    theirs["repository"] = json!({"nameWithOwner":"acme/two"});
    theirs["author"] = json!({"login":"alice"});
    theirs["mergeable"] = json!("CONFLICTING");
    theirs["commits"]["nodes"] = json!([{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]);

    let rows = derive_involving_rows(&[pr(mine), pr(theirs)], "", "me", &BoardConfig::default());
    let mine = rows.iter().find(|row| row.id == "mine").unwrap();
    assert_eq!(mine.note, "⚠️ no reviewers");
    let theirs = rows.iter().find(|row| row.id == "theirs").unwrap();
    assert_eq!(theirs.repo, "acme/two");
    assert_eq!(theirs.note, "alice's PR · merge conflict · CI failing");
    assert!(!theirs.note.contains("rebase"));
    assert!(theirs.blockers.is_empty());
}

#[test]
fn unknown_mergeability_keeps_the_last_known_conflict_and_rederives() {
    let mut known = base(1);
    known["id"] = json!("mine");
    known["mergeable"] = json!("CONFLICTING");
    let conflicted = derive_one(known.clone(), Mode::Authored);
    assert!(conflicted.conflict && !conflicted.mergeable_unknown);

    let mut recomputing = known.clone();
    recomputing["mergeable"] = json!("UNKNOWN");
    let derive = |v: &serde_json::Value| {
        derive_rows(
            &[pr(v.clone())],
            Mode::Authored,
            "acme/widgets",
            "me",
            &cfg(),
        )
    };
    let mut rows = derive(&recomputing);
    assert!(rows[0].mergeable_unknown && !rows[0].conflict);
    let last = |id: &str| (id == "mine").then_some(true);
    assert_eq!(
        carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg()),
        1
    );
    assert!(rows[0].conflict);
    assert_eq!(rows[0].category, conflicted.category);
    assert_eq!(rows[0].blockers, conflicted.blockers);
    assert_eq!(rows[0].note, conflicted.note);

    // Last known mergeable, no memory, or a value GitHub did report: untouched.
    let mut rows = derive(&recomputing);
    assert_eq!(
        carry_forward_conflicts(&mut rows, |_| Some(false), Mode::Authored, "me", &cfg()),
        0
    );
    assert_eq!(
        carry_forward_conflicts(&mut rows, |_| None, Mode::Authored, "me", &cfg()),
        0
    );
    let mut clean = known.clone();
    clean["mergeable"] = json!("MERGEABLE");
    let mut rows = derive(&clean);
    assert_eq!(
        carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg()),
        0
    );
    assert!(!rows[0].conflict);
    let mut absent = known.clone();
    absent["mergeable"] = serde_json::Value::Null;
    assert!(derive(&absent)[0].mergeable_unknown);

    // Someone else's PR in the involving view keeps its status Note.
    let mut theirs = known.clone();
    theirs["author"] = json!({"login": "alice"});
    let expected = derive_involving_rows(&[pr(theirs.clone())], "acme/widgets", "me", &cfg());
    theirs["mergeable"] = json!("UNKNOWN");
    let mut rows = derive_involving_rows(&[pr(theirs)], "acme/widgets", "me", &cfg());
    carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg());
    assert_eq!(rows[0].note, expected[0].note);
    assert_eq!(rows[0].category, expected[0].category);
    assert!(rows[0].blockers.is_empty());

    // Review queue Notes mention the conflict again.
    let mut review = known.clone();
    review["author"] = json!({"login": "alice"});
    let expected = derive_one(review.clone(), Mode::Review);
    review["mergeable"] = json!("UNKNOWN");
    let mut rows = derive_rows(&[pr(review)], Mode::Review, "acme/widgets", "me", &cfg());
    carry_forward_conflicts(&mut rows, last, Mode::Review, "me", &cfg());
    assert_eq!(rows[0].note, expected.note);
}

#[test]
fn settings_comparison_tracks_reviewers_and_issue_rule_content() {
    let mut first = BoardConfig::default();
    let mut second = first.clone();
    assert_eq!(first, second);
    second.default_reviewers.push("alex".into());
    assert_ne!(first, second);
    first = second.clone();
    first.issue_link = Some(IssueLinkRule::new("DEMO-[0-9]+", "https://example.com/{id}").unwrap());
    second.issue_link =
        Some(IssueLinkRule::new("DEMO-[0-9]+", "https://example.com/{id}").unwrap());
    assert_eq!(first, second);
    second.issue_link =
        Some(IssueLinkRule::new("TASK-[0-9]+", "https://example.com/{id}").unwrap());
    assert_ne!(first, second);
    second.issue_link =
        Some(IssueLinkRule::new("DEMO-[0-9]+", "https://other.example/{id}").unwrap());
    assert_ne!(first, second);
}

/// Answers only searches of [`SMALL_PAGE_SIZE`] rows, the way GitHub
/// answered "Involving me" on 2026-09-29: a full page timed out.
struct GivesUpOnFullPages(Mutex<Vec<String>>);

impl GithubTransport for GivesUpOnFullPages {
    fn graphql(
        &self,
        query: &str,
        _variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        self.0.lock().unwrap().push(query.to_owned());
        if query.contains(&format!("first:{PAGE_SIZE}")) {
            return Err(GhError::Http {
                status: 502,
                message: "gh: HTTP 502".into(),
            });
        }
        Ok(json!({"data": {
            "search": {
                "issueCount": 101,
                "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
                "nodes": [base(1)]
            },
            "rateLimit": null
        }}))
    }
}

#[test]
fn a_page_github_gives_up_on_is_asked_again_with_fewer_rows_and_the_view_remembers() {
    let transport = GivesUpOnFullPages(Mutex::new(Vec::new()));
    let all = BoardScope::AllRepositories;
    let first = fetch_board_scoped_with_tracked(
        &transport,
        Mode::Authored,
        &all,
        "me",
        &cfg(),
        Tracked::default(),
        false,
    )
    .unwrap();
    assert!(first.pagination.small_pages());
    assert_eq!(first.rows.len(), 1);
    let asked = std::mem::take(&mut *transport.0.lock().unwrap());
    assert_eq!(asked.len(), 2, "one full page, then one small page");
    assert!(asked[0].contains("first:60") && asked[1].contains("first:30"));

    // The next refresh of the same view asks for the small page at once,
    // and so does Load more.
    let again = fetch_board_scoped_with_tracked(
        &transport,
        Mode::Authored,
        &all,
        "me",
        &cfg(),
        Tracked::default(),
        true,
    )
    .unwrap();
    assert!(again.pagination.small_pages());
    let more =
        fetch_more_board_scoped(&transport, Mode::Authored, &all, "me", &cfg(), &again).unwrap();
    assert!(more.pagination.small_pages());
    let asked = std::mem::take(&mut *transport.0.lock().unwrap());
    assert_eq!(asked.len(), 2);
    assert!(asked.iter().all(|query| query.contains("first:30")));
}

/// Records each request's tracked ids and query; gives up on full pages.
struct FollowsPrs(Mutex<Vec<(String, Vec<String>)>>);

impl GithubTransport for FollowsPrs {
    fn graphql(
        &self,
        query: &str,
        variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        self.graphql_with_ids(query, variables, &[])
    }

    fn graphql_with_ids(
        &self,
        query: &str,
        _variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<serde_json::Value, GhError> {
        self.0
            .lock()
            .unwrap()
            .push((query.to_owned(), ids.to_vec()));
        if query.contains(&format!("first:{PAGE_SIZE}")) {
            return Err(GhError::Http {
                status: 502,
                message: "gh: HTTP 502".into(),
            });
        }
        let status_count = query.matches("\"PR_").count();
        let mut followed = base(7);
        followed["id"] = json!(ids.first().cloned().unwrap_or_default());
        Ok(json!({"data": {
            "search": {"pageInfo": {"hasNextPage": false}, "nodes": [base(1)]},
            "tracked": ids.iter().map(|_| followed.clone()).collect::<Vec<_>>(),
            "trackedStatus": (0..status_count).map(|index| match index {
                0 => json!({"id": "PR_s0", "state": "MERGED", "merged": true}),
                1 => json!({"id": "PR_s1", "state": "CLOSED", "merged": false}),
                2 => json!(null),
                _ => json!({"id": "PR_sn", "state": "OPEN", "merged": false}),
            }).collect::<Vec<_>>(),
            "rateLimit": null
        }}))
    }
}

#[test]
fn followed_prs_on_the_board_are_asked_for_their_state_only() {
    let rows: Vec<String> = (0..20).map(|n| format!("PR_r{n}")).collect();
    let status: Vec<String> = ["PR_s0", "PR_s1", "PR_s2"].map(String::from).to_vec();
    let transport = FollowsPrs(Mutex::new(Vec::new()));
    let fetched = fetch_board_scoped_with_tracked(
        &transport,
        Mode::Authored,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
        Tracked {
            rows: &rows,
            status: &status,
        },
        false,
    )
    .unwrap();
    let asked = std::mem::take(&mut *transport.0.lock().unwrap());
    assert_eq!(asked.len(), 2, "a full page, then the 30-row retry");
    let (full_query, full_ids) = &asked[0];
    assert_eq!(full_ids, &rows, "the full page follows every row");
    assert!(full_query.contains("trackedStatus: nodes(ids:[\"PR_s0\",\"PR_s1\",\"PR_s2\"])"));
    assert!(full_query.contains("...TrackedPr"));

    // The retry keeps the first rows in full and asks the rest for their
    // state, after the ones that were state-only already.
    let (small_query, small_ids) = &asked[1];
    assert_eq!(small_ids, &rows[..SMALL_PAGE_TRACKED_ROWS]);
    let mut expected = status.clone();
    expected.extend_from_slice(&rows[SMALL_PAGE_TRACKED_ROWS..]);
    let listed = expected
        .iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(",");
    assert!(small_query.contains(&format!("trackedStatus: nodes(ids:[{listed}])")));

    // Full rows first, then the states in request order; a node GitHub
    // did not return reads as inaccessible.
    assert_eq!(fetched.tracked.len(), rows.len() + status.len());
    assert!(fetched.tracked[..SMALL_PAGE_TRACKED_ROWS]
        .iter()
        .all(|tracked| tracked.row.is_some()));
    let states: Vec<_> = fetched.tracked[SMALL_PAGE_TRACKED_ROWS..]
        .iter()
        .map(|tracked| {
            (
                tracked.pr_id.as_str(),
                tracked.status,
                tracked.row.is_some(),
            )
        })
        .collect();
    assert_eq!(states[0], ("PR_s0", TrackedPrStatus::Merged, false));
    assert_eq!(states[1], ("PR_s1", TrackedPrStatus::Closed, false));
    assert_eq!(states[2], ("PR_s2", TrackedPrStatus::Inaccessible, false));
    assert_eq!(states[3].1, TrackedPrStatus::Open);
}

#[test]
fn a_followed_id_that_is_not_a_node_id_is_refused_before_any_request() {
    let transport = FollowsPrs(Mutex::new(Vec::new()));
    let bad = ["PR_ok".to_owned(), "x\"]){ viewer { login } }".to_owned()];
    let error = fetch_board_scoped_with_tracked(
        &transport,
        Mode::Authored,
        &BoardScope::AllRepositories,
        "me",
        &cfg(),
        Tracked {
            rows: &[],
            status: &bad,
        },
        true,
    )
    .unwrap_err();
    assert!(matches!(error, GhError::Parse(_)), "{error:?}");
    assert!(transport.0.lock().unwrap().is_empty());
}

#[test]
fn only_github_giving_up_asks_again() {
    // Refused, signed out, or a network failure: one request, same error.
    for error in [
        GhError::NotAuthenticated,
        GhError::Network("could not resolve host".into()),
        GhError::Timeout("gh timed out after 60s — killed".into()),
    ] {
        let transport = SequenceTransport::new(vec![Err(error.clone())]);
        let fetched = fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg());
        assert!(fetched.is_err(), "{error:?}");
        assert!(transport.0.lock().unwrap().is_empty());
    }
    // A small page that also fails reports that failure; no third try.
    let transport = SequenceTransport::new(vec![
        Err(GhError::Http {
            status: 502,
            message: "gh: HTTP 502".into(),
        }),
        Err(GhError::GraphqlErrors(vec![
            "Something went wrong while executing your query. This may be the result of a timeout"
                .into(),
        ])),
    ]);
    let error = fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap_err();
    assert!(error.is_query_timeout());
    assert!(transport.0.lock().unwrap().is_empty());
}

#[test]
fn more_threads_than_the_window_make_the_count_a_lower_bound() {
    let mut v = base(4);
    v["reviewThreads"] = json!({
        "totalCount": 130,
        "nodes": [{"isResolved": false}, {"isResolved": true}, {"isResolved": false}]
    });
    let row = derive_one(v.clone(), Mode::Authored);
    assert_eq!(row.unresolved, 2);
    assert!(row.unresolved_capped);
    assert!(row.note.contains("2+ unresolved comments"), "{}", row.note);
    // Every thread read: the plain count, singular where it is one.
    v["reviewThreads"] = json!({"totalCount": 1, "nodes": [{"isResolved": false}]});
    let row = derive_one(v, Mode::Authored);
    assert!(!row.unresolved_capped);
    assert!(row.note.contains("1 unresolved comment"), "{}", row.note);
    assert!(!row.note.contains("comments"), "{}", row.note);
}

#[test]
fn what_became_of_tracked_prs_is_one_request_without_rows() {
    struct Status;
    impl GithubTransport for Status {
        fn graphql(
            &self,
            _query: &str,
            _variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            unreachable!("ids go through graphql_with_ids")
        }
        fn graphql_with_ids(
            &self,
            query: &str,
            variables: &[(&str, &str)],
            ids: &[String],
        ) -> Result<serde_json::Value, GhError> {
            assert_eq!(query, TRACKED_STATUS_QUERY);
            assert!(variables.is_empty());
            assert_eq!(ids.len(), 4);
            Ok(json!({"data": {
                "tracked": [
                    {"id": "A", "state": "MERGED", "merged": true},
                    {"id": "B", "state": "CLOSED", "merged": false},
                    {"id": "C", "state": "OPEN", "merged": false},
                    null
                ],
                "rateLimit": {"limit": 5000, "cost": 1, "remaining": 4999, "resetAt": "2026-09-29T12:00:00Z"}
            }}))
        }
    }
    let ids: Vec<String> = ["A", "B", "C", "D"].map(String::from).to_vec();
    let fetched = fetch_tracked_status(&Status, &ids).unwrap();
    let statuses: Vec<_> = fetched
        .tracked
        .iter()
        .map(|tracked| (tracked.pr_id.as_str(), tracked.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("A", TrackedPrStatus::Merged),
            ("B", TrackedPrStatus::Closed),
            ("C", TrackedPrStatus::Open),
            ("D", TrackedPrStatus::Inaccessible),
        ]
    );
    assert!(fetched.tracked.iter().all(|tracked| tracked.row.is_none()));
    assert_eq!(fetched.rate.map(|rate| rate.cost), Some(1));
    assert!(fetch_tracked_status(&Status, &[])
        .unwrap()
        .tracked
        .is_empty());
}
