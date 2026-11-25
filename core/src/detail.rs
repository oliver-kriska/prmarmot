//! What the Details panel says about one pull request.
//!
//! The desktop drew these lines and the iPad drew its own, which is exactly how
//! two front ends stop agreeing: a reordered line here, a different word for
//! `CHANGES_REQUESTED` there. The lines live here now, in the order they are
//! shown, so "the same facts in the same words" is a property of the code
//! rather than a promise in a review.
//!
//! Nothing here reads a clock or a time zone: `now` and `tz_offset_secs` are
//! parameters, the same rule `pickup` follows, so a golden test can pin the
//! wait and the local timestamp without pinning the machine it runs on.

use chrono::{DateTime, FixedOffset, Utc};

use crate::board::{strip_note_glyphs, BoardRow, CheckCounts, Mode, ReviewVerdict};
use crate::pickup::{wait_label, waiting_secs};
use crate::size::ChangeSize;

/// "Small · 42 changed lines in 3 files (+40 −2)".
pub fn size_text(size: ChangeSize) -> String {
    format!(
        "{} · {} (+{} −{})",
        size.band().label(),
        size.lines_and_files(),
        size.additions,
        size.deletions
    )
}

/// "1 failed · 12 passed · 3 skipped": each state that has a check, the ones
/// that want a look first. The CI column says one word; this says why.
pub fn checks_text(checks: CheckCounts) -> String {
    [
        (checks.failed, "failed"),
        (checks.running, "running"),
        (checks.passed, "passed"),
        (checks.cancelled, "cancelled"),
        (checks.skipped, "skipped"),
        (checks.neutral, "neutral"),
        (checks.stale, "stale"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, word)| format!("{count} {word}"))
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The open threads as a count, and the files they are on when the row knows:
/// "2 in src/app.rs, README.md"; "5+" when the count is a lower bound.
pub fn unresolved_text(row: &BoardRow) -> String {
    let count = if row.unresolved_capped {
        format!("{}+", row.unresolved)
    } else {
        row.unresolved.to_string()
    };
    if row.unresolved == 0 || row.unresolved_paths.is_empty() {
        count
    } else {
        format!("{count} in {}", row.unresolved_paths.join(", "))
    }
}

/// A review state in plain words ("changes requested"), never GitHub's enum.
pub fn review_state_words(state: &ReviewVerdict) -> String {
    match state {
        ReviewVerdict::Approved => "approved".into(),
        ReviewVerdict::ChangesRequested => "changes requested".into(),
        ReviewVerdict::Commented => "commented".into(),
        ReviewVerdict::Dismissed => "dismissed".into(),
        other => other.as_str().to_lowercase().replace('_', " "),
    }
}

/// Your standing review in plain words; `None` when there isn't one.
pub fn my_review_text(review: &ReviewVerdict) -> Option<&'static str> {
    match review {
        ReviewVerdict::Approved => Some("approved"),
        ReviewVerdict::ChangesRequested => Some("changes requested"),
        ReviewVerdict::Commented => Some("commented"),
        _ => None,
    }
}

/// Whether Details shows your standing review, so a `state:` match the
/// Review column does not show (the column leaves your own review out) is
/// explained. My PRs holds only your own PRs, which have no review of yours
/// to show. The review queue holds only other people's. All open holds both,
/// and the row does not say whose it is, so it shows only an approval or a
/// change request: GitHub lets nobody give either on their own PR, while a
/// reply in a thread there counts as a comment.
fn shows_your_review(mode: Mode, review: &ReviewVerdict) -> bool {
    match mode {
        Mode::Authored => false,
        Mode::Review => true,
        Mode::AllOpen => matches!(
            review,
            ReviewVerdict::Approved | ReviewVerdict::ChangesRequested
        ),
    }
}

/// What a line of the panel is, so a front end can draw one differently —
/// the labels as chips, the closing line muted — without reading its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    /// The Note, without its glyphs.
    Note,
    /// Author, CI and the open threads.
    Facts,
    /// The latest commit's checks counted by state.
    Checks,
    RequestedReviewers,
    Reviews,
    /// Your standing review, in the review queue.
    YourReview,
    /// How long it has waited for a reviewer, and since when.
    Waiting,
    Size,
    Labels,
    Issue,
    Stack,
}

/// Every line of the panel, in the order it is shown.
///
/// `tz_offset_secs` is the reader's offset from UTC, used only for the
/// "(since …)" timestamp — the one place the panel shows a wall clock.
pub fn detail_lines(
    row: &BoardRow,
    mode: Mode,
    now: DateTime<Utc>,
    tz_offset_secs: i32,
) -> Vec<String> {
    detail_items(row, mode, now, tz_offset_secs)
        .into_iter()
        .map(|item| item.text)
        .collect()
}

/// A page a Details line can open: a failed check's run, named as the
/// line names it, so a front end can make that word a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLink {
    /// The words of the line the page belongs to (a check's name).
    pub text: String,
    /// A web address ([`crate::board::is_web_url`]).
    pub url: String,
}

/// One line of the panel and what it is, with the pages it can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailItem {
    pub kind: DetailKind,
    pub text: String,
    /// The failed checks with a run page, in the line's order; empty on
    /// every other line.
    pub links: Vec<DetailLink>,
}

/// [`detail_lines`], each with what it is: [`detail_fields`] written out as
/// the sentences the bottom pane and the iPad show.
pub fn detail_items(
    row: &BoardRow,
    mode: Mode,
    now: DateTime<Utc>,
    tz_offset_secs: i32,
) -> Vec<DetailItem> {
    let fields = detail_fields(row, mode, now, tz_offset_secs);
    let mut lines: Vec<DetailItem> = Vec::with_capacity(fields.len());
    for field in fields {
        let text = match (field.kind, field.label) {
            (DetailKind::Waiting, _) => format!("Waiting for a reviewer for {}", field.value),
            (DetailKind::Stack, Some(label)) => format!("{label} {}", field.value),
            (_, Some(label)) => format!("{label}: {}", field.value),
            (_, None) => field.value,
        };
        // Author, CI and the open threads share one line.
        match lines.last_mut() {
            Some(facts) if facts.kind == DetailKind::Facts && field.kind == DetailKind::Facts => {
                facts.text.push_str(" · ");
                facts.text.push_str(&text);
            }
            _ => lines.push(DetailItem {
                kind: field.kind,
                text,
                links: field.links,
            }),
        }
    }
    lines
}

/// One fact of the Details panel as a label and a value, for a panel that
/// lays them out in two columns. The Note has no label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailField {
    pub kind: DetailKind,
    pub label: Option<&'static str>,
    pub value: String,
    /// How the PR stands — CI, the checks, open threads, the wait, the size
    /// — rather than who and where (author, reviewers, reviews, issue,
    /// stack). The desktop's right panel gives these their own column.
    pub standing: bool,
    /// The pages the value names that can be opened: on the Checks line,
    /// each failed check GitHub gave a run page for, in the value's order.
    pub links: Vec<DetailLink>,
}

/// The failed checks that have a run page, as the Checks line names them.
pub fn failed_check_links(row: &BoardRow) -> Vec<DetailLink> {
    row.failed_checks
        .iter()
        .filter_map(|check| {
            check.url.as_ref().map(|url| DetailLink {
                text: check.name.clone(),
                url: url.clone(),
            })
        })
        .collect()
}

/// Every fact of the panel, in the order it is shown: the one source of
/// [`detail_items`], so the two layouts cannot say different things.
pub fn detail_fields(
    row: &BoardRow,
    mode: Mode,
    now: DateTime<Utc>,
    tz_offset_secs: i32,
) -> Vec<DetailField> {
    let field = |kind, label, value: String| DetailField {
        kind,
        label,
        value,
        standing: matches!(
            kind,
            DetailKind::Facts | DetailKind::Checks | DetailKind::Waiting | DetailKind::Size
        ),
        links: Vec::new(),
    };
    let mut fields = vec![
        field(DetailKind::Note, None, strip_note_glyphs(&row.note)),
        DetailField {
            // One of the facts, but it says who, not how the PR stands.
            standing: false,
            ..field(
                DetailKind::Facts,
                Some("Author"),
                row.author.as_deref().unwrap_or("unknown").to_owned(),
            )
        },
        field(DetailKind::Facts, Some("CI"), row.ci.as_str().to_owned()),
        field(
            DetailKind::Facts,
            Some("Unresolved comments"),
            unresolved_text(row),
        ),
    ];
    if let Some(checks) = row.checks {
        fields.push(DetailField {
            // The failed checks' names can open their runs.
            links: failed_check_links(row),
            ..field(
                DetailKind::Checks,
                Some("Checks"),
                match crate::board::failed_checks_text(row) {
                    Some(names) => format!("{} — failed: {names}", checks_text(checks)),
                    None => checks_text(checks),
                },
            )
        });
    }
    fields.extend([
        field(
            DetailKind::RequestedReviewers,
            Some("Requested reviewers"),
            if row.requested.is_empty() {
                "none".into()
            } else {
                row.requested.join(", ")
            },
        ),
        field(
            DetailKind::Reviews,
            Some("Reviews"),
            if row.reviews.is_empty() {
                "none".into()
            } else {
                row.reviews
                    .iter()
                    .map(|review| {
                        format!(
                            "{} — {}",
                            review.login.as_deref().unwrap_or("deleted user"),
                            review_state_words(&review.state)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
    ]);
    if let Some(review) = row
        .my_review
        .as_ref()
        .filter(|review| shows_your_review(mode, review))
        .and_then(my_review_text)
    {
        fields.push(field(
            DetailKind::YourReview,
            Some("Your review"),
            match crate::board::commits_since_review_text(row) {
                Some(since) => format!("{review} — {since}"),
                None => review.to_owned(),
            },
        ));
    }
    // Like the Note's "· 4d", with the start in the reader's time zone.
    let since = row
        .waiting_since
        .as_deref()
        .and_then(|since| DateTime::parse_from_rfc3339(since).ok());
    if let (Some(secs), Some(since)) = (waiting_secs(row, now), since) {
        let zone = FixedOffset::east_opt(tz_offset_secs).unwrap_or_else(|| {
            FixedOffset::east_opt(0).expect("UTC is always a valid fixed offset")
        });
        fields.push(field(
            DetailKind::Waiting,
            Some("Waiting"),
            format!(
                "{} (since {})",
                wait_label(secs),
                since.with_timezone(&zone).format("%Y-%m-%d %H:%M")
            ),
        ));
    }
    if let Some(size) = row.size {
        fields.push(field(DetailKind::Size, Some("Size"), size_text(size)));
    }
    if !row.labels.is_empty() {
        fields.push(field(
            DetailKind::Labels,
            Some("Labels"),
            row.labels.join(", "),
        ));
    }
    if let Some(issue) = &row.issue {
        fields.push(field(DetailKind::Issue, Some("Issue"), issue.clone()));
    }
    if let Some(parent) = row.base_pr.filter(|parent| {
        row.stack
            .as_ref()
            .and_then(|s| s.branch)
            .and_then(|b| b.parent)
            != Some(*parent)
    }) {
        fields.push(field(
            DetailKind::Stack,
            Some("Based on"),
            format!("#{parent}"),
        ));
    }
    if let Some(stack) = &row.stack {
        fields.push(field(
            DetailKind::Stack,
            Some(if stack.branch.is_some() {
                "Branch tree"
            } else {
                "Stack"
            }),
            if let Some(layer) = stack.branch {
                let relation = layer.parent.map_or_else(
                    || "Root among loaded PRs".into(),
                    |parent| format!("Based on #{parent}"),
                );
                format!(
                    "#{} · {relation} · Base: {}",
                    stack.number, stack.base_ref_name
                )
            } else {
                format!(
                    "#{} · Layer {} of {} · Base: {}",
                    stack.number,
                    stack
                        .position
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "?".into()),
                    stack.size,
                    stack.base_ref_name
                )
            },
        ));
    }
    fields
}

/// "Attention: changed · watched" — the Details panel's line about this PR's
/// marker and watch, under core's lines on the desktop and the iPad alike.
pub fn attention_line(changed: bool, watched: bool) -> String {
    format!(
        "Attention: {} · {}",
        if changed { "changed" } else { "acknowledged" },
        if watched { "watched" } else { "not watched" }
    )
}

/// Full, unelided snapshot details; no secondary network request or hidden cache.
pub fn detail_text(row: &BoardRow, mode: Mode, now: DateTime<Utc>, tz_offset_secs: i32) -> String {
    detail_lines(row, mode, now, tz_offset_secs).join("\n")
}

/// The Copy menu, in order: what a person is most likely to want first.
pub fn copy_items(
    row: &BoardRow,
    mode: Mode,
    now: DateTime<Utc>,
    tz_offset_secs: i32,
) -> Vec<(&'static str, String)> {
    vec![
        ("Copy PR URL", row.url.clone()),
        ("Copy PR number", format!("#{}", row.number)),
        ("Copy PR reference", format!("{}#{}", row.repo, row.number)),
        ("Copy title", row.title.clone()),
        (
            "Copy all details",
            format!(
                "{}#{} {}\n{}\n\n{}",
                row.repo,
                row.number,
                row.title,
                row.url,
                detail_text(row, mode, now, tz_offset_secs)
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{
        Category, CheckCounts, Ci, FailedCheck, ReviewState, ReviewSummary, StackInfo,
    };
    use crate::size::ChangeSize;

    fn row() -> BoardRow {
        BoardRow {
            id: "PR_1".into(),
            repo: "acme/widgets".into(),
            number: 42,
            url: "https://github.com/acme/widgets/pull/42".into(),
            title: "Fix login".into(),
            author: Some("alice".into()),
            category: Category::Todo,
            ci: Ci::Pass,
            review_state: ReviewState::Waiting,
            created_at: "2026-09-01T10:00:00Z".into(),
            note: "🟡 ⏳ Waiting for your review".into(),
            ..Default::default()
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_short_panel_is_the_note_and_three_facts_in_this_order() {
        assert_eq!(
            detail_lines(&row(), Mode::Review, now(), 0),
            vec![
                "⏳ Waiting for your review",
                "Author: alice · CI: pass · Unresolved comments: 0",
                "Requested reviewers: none",
                "Reviews: none",
            ]
        );
    }

    /// A row with every optional line present.
    fn full_row() -> BoardRow {
        let mut pr = row();
        pr.requested = vec!["bob".into(), "kim".into()];
        pr.reviews = vec![
            ReviewSummary {
                login: Some("bob".into()),
                state: "CHANGES_REQUESTED".into(),
                submitted_at: None,
                bot: false,
            },
            ReviewSummary {
                login: None,
                state: "APPROVED".into(),
                submitted_at: None,
                bot: false,
            },
        ];
        pr.my_review = Some("COMMENTED".into());
        pr.waiting_since = Some("2026-09-02T09:30:00Z".into());
        pr.size = Some(ChangeSize {
            additions: 40,
            deletions: 2,
            changed_files: 3,
        });
        pr.checks = Some(CheckCounts {
            passed: 11,
            cancelled: 1,
            skipped: 9,
            ..CheckCounts::default()
        });
        pr.labels = vec!["backend".into(), "bug".into()];
        pr.issue = Some("ACME-7".into());
        pr.stack = Some(StackInfo {
            number: 70,
            size: 3,
            base_ref_name: "main".into(),
            position: Some(2),
            branch: None,
        });
        pr
    }

    #[test]
    fn the_checks_and_threads_lines_carry_their_evidence() {
        let mut pr = full_row();
        pr.checks = Some(CheckCounts {
            failed: 2,
            passed: 11,
            ..CheckCounts::default()
        });
        pr.failed_checks = vec![
            FailedCheck {
                name: "lint".into(),
                url: None,
            },
            FailedCheck {
                name: "test (macOS)".into(),
                url: Some("https://ci.example.test/runs/9".into()),
            },
        ];
        pr.unresolved = 3;
        pr.unresolved_paths = vec!["src/app.rs".into(), "README.md".into()];
        let lines = detail_lines(&pr, Mode::Review, now(), 0);
        assert_eq!(
            lines[1],
            "Author: alice · CI: pass · Unresolved comments: 3 in src/app.rs, README.md"
        );
        assert_eq!(
            lines[2],
            "Checks: 2 failed · 11 passed — failed: lint, test (macOS)"
        );
        pr.unresolved_capped = true;
        assert_eq!(unresolved_text(&pr), "3+ in src/app.rs, README.md");
        pr.unresolved_paths.clear();
        assert_eq!(unresolved_text(&pr), "3+");
    }

    #[test]
    fn every_optional_line_appears_in_its_place() {
        assert_eq!(
            detail_lines(&full_row(), Mode::Review, now(), 0),
            vec![
                "⏳ Waiting for your review",
                "Author: alice · CI: pass · Unresolved comments: 0",
                "Checks: 11 passed · 1 cancelled · 9 skipped",
                "Requested reviewers: bob, kim",
                "Reviews: bob — changes requested, deleted user — approved",
                "Your review: commented",
                "Waiting for a reviewer for 3d (since 2026-09-02 09:30)",
                "Size: Small · 42 changed lines in 3 files (+40 −2)",
                "Labels: backend, bug",
                "Issue: ACME-7",
                "Stack #70 · Layer 2 of 3 · Base: main",
            ]
        );
    }

    /// The two-column panel reads the same facts as the sentences, split into
    /// a label and a value; the Note has no label.
    #[test]
    fn every_fact_is_a_label_and_a_value() {
        let fields = detail_fields(&full_row(), Mode::Review, now(), 0);
        let pairs: Vec<(Option<&str>, &str)> = fields
            .iter()
            .map(|field| (field.label, field.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (None, "⏳ Waiting for your review"),
                (Some("Author"), "alice"),
                (Some("CI"), "pass"),
                (Some("Unresolved comments"), "0"),
                (Some("Checks"), "11 passed · 1 cancelled · 9 skipped"),
                (Some("Requested reviewers"), "bob, kim"),
                (
                    Some("Reviews"),
                    "bob — changes requested, deleted user — approved"
                ),
                (Some("Your review"), "commented"),
                (Some("Waiting"), "3d (since 2026-09-02 09:30)"),
                (Some("Size"), "Small · 42 changed lines in 3 files (+40 −2)"),
                (Some("Labels"), "backend, bug"),
                (Some("Issue"), "ACME-7"),
                (Some("Stack"), "#70 · Layer 2 of 3 · Base: main"),
            ]
        );
    }

    #[test]
    fn the_author_says_who_and_the_other_facts_say_how_it_stands() {
        let standing: Vec<&str> = detail_fields(&full_row(), Mode::Review, now(), 0)
            .into_iter()
            .filter(|field| field.standing)
            .filter_map(|field| field.label)
            .collect();
        assert_eq!(
            standing,
            ["CI", "Unresolved comments", "Checks", "Waiting", "Size"]
        );
    }

    /// The failed checks that have a run page come with the Checks line,
    /// named as the line names them, so a front end can open their runs.
    #[test]
    fn the_checks_line_can_open_the_failed_runs() {
        let mut pr = full_row();
        pr.failed_checks = vec![
            FailedCheck {
                name: "lint".into(),
                url: Some("https://github.com/acme/widgets/actions/runs/1".into()),
            },
            FailedCheck {
                name: "test (macOS)".into(),
                url: None,
            },
        ];
        let checks = detail_fields(&pr, Mode::Review, now(), 0)
            .into_iter()
            .find(|field| field.kind == DetailKind::Checks)
            .unwrap();
        assert_eq!(
            checks.value,
            "11 passed · 1 cancelled · 9 skipped — failed: lint, test (macOS)"
        );
        assert_eq!(
            checks.links,
            vec![DetailLink {
                text: "lint".into(),
                url: "https://github.com/acme/widgets/actions/runs/1".into(),
            }],
            "only a check with a run page is a link"
        );
        let items = detail_items(&pr, Mode::Review, now(), 0);
        let line = items
            .iter()
            .find(|item| item.kind == DetailKind::Checks)
            .unwrap();
        assert_eq!(line.links, checks.links);
        assert!(
            items
                .iter()
                .filter(|item| item.kind != DetailKind::Checks)
                .all(|item| item.links.is_empty()),
            "no other line opens anything"
        );
    }

    #[test]
    fn each_line_says_what_it_is_in_the_order_it_is_shown() {
        let pr = full_row();
        let items = detail_items(&pr, Mode::Review, now(), 0);
        assert_eq!(
            items.iter().map(|item| item.kind).collect::<Vec<_>>(),
            vec![
                DetailKind::Note,
                DetailKind::Facts,
                DetailKind::Checks,
                DetailKind::RequestedReviewers,
                DetailKind::Reviews,
                DetailKind::YourReview,
                DetailKind::Waiting,
                DetailKind::Size,
                DetailKind::Labels,
                DetailKind::Issue,
                DetailKind::Stack,
            ]
        );
        assert_eq!(
            items.into_iter().map(|item| item.text).collect::<Vec<_>>(),
            detail_lines(&pr, Mode::Review, now(), 0)
        );
        // Without the optional lines, the kinds close up in the same order.
        assert_eq!(
            detail_items(&row(), Mode::Authored, now(), 0)
                .iter()
                .map(|item| item.kind)
                .collect::<Vec<_>>(),
            vec![
                DetailKind::Note,
                DetailKind::Facts,
                DetailKind::RequestedReviewers,
                DetailKind::Reviews,
            ]
        );
    }

    #[test]
    fn checks_name_each_state_that_has_one_failures_first() {
        assert_eq!(
            checks_text(CheckCounts {
                failed: 1,
                running: 2,
                passed: 12,
                cancelled: 1,
                skipped: 3,
                neutral: 1,
                stale: 1,
            }),
            "1 failed · 2 running · 12 passed · 1 cancelled · 3 skipped · 1 neutral · 1 stale"
        );
        assert_eq!(
            checks_text(CheckCounts {
                passed: 4,
                ..CheckCounts::default()
            }),
            "4 passed"
        );
    }

    #[test]
    fn the_attention_line_names_the_marker_and_the_watch() {
        assert_eq!(
            attention_line(true, false),
            "Attention: changed · not watched"
        );
        assert_eq!(
            attention_line(false, true),
            "Attention: acknowledged · watched"
        );
    }

    #[test]
    fn the_wall_clock_line_is_the_readers_time_zone_not_the_machines() {
        let mut pr = row();
        pr.waiting_since = Some("2026-09-02T09:30:00Z".into());
        let two_hours_east = detail_lines(&pr, Mode::Review, now(), 2 * 3600);
        assert!(
            two_hours_east
                .iter()
                .any(|line| line == "Waiting for a reviewer for 3d (since 2026-09-02 11:30)"),
            "{two_hours_east:?}"
        );
    }

    #[test]
    fn all_open_shows_your_approval_or_change_request() {
        let mut pr = row();
        pr.my_review = Some("APPROVED".into());
        let lines = detail_lines(&pr, Mode::AllOpen, now(), 0);
        assert!(
            lines.iter().any(|line| line == "Your review: approved"),
            "{lines:?}"
        );
        pr.my_review = Some("CHANGES_REQUESTED".into());
        let lines = detail_lines(&pr, Mode::AllOpen, now(), 0);
        assert!(lines
            .iter()
            .any(|line| line == "Your review: changes requested"));
        // A comment may be a reply on your own PR, which All open cannot tell.
        pr.my_review = Some("COMMENTED".into());
        let lines = detail_lines(&pr, Mode::AllOpen, now(), 0);
        assert!(!lines.iter().any(|line| line.starts_with("Your review")));
    }

    #[test]
    fn your_own_pr_never_shows_your_review() {
        let mut pr = row();
        pr.my_review = Some("APPROVED".into());
        let lines = detail_lines(&pr, Mode::Authored, now(), 0);
        assert!(!lines.iter().any(|line| line.starts_with("Your review")));
    }

    #[test]
    fn copying_everything_starts_with_the_reference_and_the_url() {
        let items = copy_items(&row(), Mode::Review, now(), 0);
        assert_eq!(
            items.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec![
                "Copy PR URL",
                "Copy PR number",
                "Copy PR reference",
                "Copy title",
                "Copy all details"
            ]
        );
        assert_eq!(items[2].1, "acme/widgets#42");
        assert!(items[4].1.starts_with(
            "acme/widgets#42 Fix login\nhttps://github.com/acme/widgets/pull/42\n\n⏳ Waiting"
        ));
    }
}
