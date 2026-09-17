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

use crate::board::{strip_note_glyphs, BoardRow, Mode};
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

/// A review state in plain words ("changes requested"), never GitHub's enum.
pub fn review_state_words(state: &str) -> String {
    match state {
        "APPROVED" => "approved".into(),
        "CHANGES_REQUESTED" => "changes requested".into(),
        "COMMENTED" => "commented".into(),
        "DISMISSED" => "dismissed".into(),
        other => other.to_lowercase().replace('_', " "),
    }
}

/// Your standing review in plain words; `None` when there isn't one.
pub fn my_review_text(review: &str) -> Option<&'static str> {
    match review {
        "APPROVED" => Some("approved"),
        "CHANGES_REQUESTED" => Some("changes requested"),
        "COMMENTED" => Some("commented"),
        _ => None,
    }
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
    let mut lines = vec![
        strip_note_glyphs(&row.note),
        format!(
            "Author: {} · CI: {} · Unresolved threads: {}",
            row.author.as_deref().unwrap_or("unknown"),
            row.ci.as_str(),
            row.unresolved
        ),
        format!(
            "Requested reviewers: {}",
            if row.requested.is_empty() {
                "none".into()
            } else {
                row.requested.join(", ")
            }
        ),
        format!(
            "Reviews: {}",
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
            }
        ),
    ];
    // Your own PR has no review of yours to show.
    if let Some(review) = row
        .my_review
        .as_deref()
        .filter(|_| mode == Mode::Review)
        .and_then(my_review_text)
    {
        lines.push(format!("Your review: {review}"));
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
        lines.push(format!(
            "Waiting for a reviewer for {} (since {})",
            wait_label(secs),
            since.with_timezone(&zone).format("%Y-%m-%d %H:%M")
        ));
    }
    if let Some(size) = row.size {
        lines.push(format!("Size: {}", size_text(size)));
    }
    if !row.labels.is_empty() {
        lines.push(format!("Labels: {}", row.labels.join(", ")));
    }
    if let Some(issue) = &row.issue {
        lines.push(format!("Issue: {issue}"));
    }
    if let Some(stack) = &row.stack {
        lines.push(format!(
            "Stack #{} · Layer {} of {} · Base: {}",
            stack.number,
            stack
                .position
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            stack.size,
            stack.base_ref_name
        ));
    }
    lines.push("Details reflect the loaded snapshot; refresh restarts pagination.".into());
    lines
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
    use crate::board::{Category, Ci, ReviewState, ReviewSummary, StackInfo};
    use crate::size::ChangeSize;

    fn row() -> BoardRow {
        BoardRow {
            id: "PR_1".into(),
            repo: "acme/widgets".into(),
            updated_at: None,
            head_oid: None,
            reviewed_oid: None,
            reviewed_at: None,
            number: 42,
            url: "https://github.com/acme/widgets/pull/42".into(),
            title: "Fix login".into(),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: None,
            draft: false,
            category: Category::Todo,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Pass,
            conflict: false,
            mergeable_unknown: false,
            review_decision: None,
            review_state: ReviewState::Waiting,
            requested: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: "2026-09-01T10:00:00Z".into(),
            waiting_since: None,
            size: None,
            note: "🟡 ⏳ Waiting for your review".into(),
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
                "Author: alice · CI: pass · Unresolved threads: 0",
                "Requested reviewers: none",
                "Reviews: none",
                "Details reflect the loaded snapshot; refresh restarts pagination.",
            ]
        );
    }

    #[test]
    fn every_optional_line_appears_in_its_place() {
        let mut pr = row();
        pr.requested = vec!["bob".into(), "kim".into()];
        pr.reviews = vec![
            ReviewSummary {
                login: Some("bob".into()),
                state: "CHANGES_REQUESTED".into(),
                submitted_at: None,
            },
            ReviewSummary {
                login: None,
                state: "APPROVED".into(),
                submitted_at: None,
            },
        ];
        pr.my_review = Some("COMMENTED".into());
        pr.waiting_since = Some("2026-09-02T09:30:00Z".into());
        pr.size = Some(ChangeSize {
            additions: 40,
            deletions: 2,
            changed_files: 3,
        });
        pr.labels = vec!["backend".into(), "bug".into()];
        pr.issue = Some("ACME-7".into());
        pr.stack = Some(StackInfo {
            number: 70,
            size: 3,
            base_ref_name: "main".into(),
            position: Some(2),
        });

        assert_eq!(
            detail_lines(&pr, Mode::Review, now(), 0),
            vec![
                "⏳ Waiting for your review",
                "Author: alice · CI: pass · Unresolved threads: 0",
                "Requested reviewers: bob, kim",
                "Reviews: bob — changes requested, deleted user — approved",
                "Your review: commented",
                "Waiting for a reviewer for 3d (since 2026-09-02 09:30)",
                "Size: Small · 42 changed lines in 3 files (+40 −2)",
                "Labels: backend, bug",
                "Issue: ACME-7",
                "Stack #70 · Layer 2 of 3 · Base: main",
                "Details reflect the loaded snapshot; refresh restarts pagination.",
            ]
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
