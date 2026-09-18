//! What each column of the board says about one row.
//!
//! This used to live in the desktop's `src/table.rs`, on the reasoning that
//! severity is a presentation concern. With two front ends that reasoning
//! inverts: if the iPad decides for itself which blocker is the exceptional
//! one, or which glyph a dismissed review gets, the two products disagree
//! about the same pull request and a user finds the difference before a test
//! does. So the *decisions* live here — tone, order, glyph, wording — and each
//! front end only maps a [`Tone`] to its own colours and draws the strings.
//!
//! Everything here is a pure function of a [`BoardRow`]: no clock, no theme,
//! no widths. Eliding a long cell is still the front end's job, which is why
//! the primary phrase and the context facts are returned separately — the
//! caller shortens the tail and never the phrase that carries the exception.

use crate::board::strip_note_glyphs;
use crate::board::{Blocker, BoardRow, Category, Ci, ReviewState};

/// How alarming a cell is. Each front end maps this to its own palette; the
/// decision about which is which is made here so both make it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Exceptional blocker — merge conflict / CI failure / changes requested.
    /// It should interrupt the scan.
    Danger,
    /// Routine action needed — reviewers to assign, comments to resolve, a
    /// review still owed. Visible but peripheral, so a column of them never
    /// forms a wall of alarm.
    Warning,
    /// Merged-path good news — approved, or awaiting review after approval.
    Success,
    /// Completed but neutral — you already reviewed, nothing outstanding on
    /// you.
    Routine,
    /// Draft or inactive: no dot, dim text.
    Muted,
}

/// A Note cell decomposed for exception-first rendering: one emphasised
/// `primary` phrase, an optional inline `remedy`, and muted `context` facts so
/// no blocker disappears into the tooltip alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePresentation {
    pub tone: Tone,
    pub primary: String,
    pub remedy: Option<String>,
    pub context: Vec<String>,
    /// The full, glyph-stripped canonical note — always the hover text.
    pub tooltip: String,
}

/// Presentation priority — deliberately different from the canonical note
/// order: the most operationally urgent blocker is shown first and coloured.
fn blocker_rank(blocker: &Blocker) -> u8 {
    match blocker {
        Blocker::MergeConflict => 0,
        Blocker::CiFailing => 1,
        Blocker::ChangesRequested => 2,
        Blocker::UnresolvedComments(_) => 3,
        Blocker::NoReviewers { .. } => 4,
    }
}

/// Merge conflict / CI failure / changes-requested interrupt the scan (danger);
/// unresolved comments and missing reviewers are routine follow-up (warning).
fn is_exceptional(blocker: &Blocker) -> bool {
    matches!(
        blocker,
        Blocker::MergeConflict | Blocker::CiFailing | Blocker::ChangesRequested
    )
}

/// The blocker as the emphasised primary phrase (+ optional muted remedy).
fn blocker_primary(blocker: &Blocker) -> (String, Option<String>) {
    match blocker {
        Blocker::MergeConflict => ("merge conflict".into(), Some("rebase".into())),
        Blocker::CiFailing => ("CI failing".into(), None),
        Blocker::ChangesRequested => ("changes requested".into(), None),
        Blocker::UnresolvedComments(n) => (
            format!("resolve {n} comment{}", if *n == 1 { "" } else { "s" }),
            None,
        ),
        Blocker::NoReviewers { suggested } => {
            if suggested.is_empty() {
                ("assign reviewers".into(), None)
            } else {
                (format!("assign {}", suggested.join(" + ")), None)
            }
        }
    }
}

/// The blocker as a compact muted context fact (shown when a higher-priority
/// blocker is the primary), so it stays visible on the row, not only on hover.
fn blocker_context(blocker: &Blocker) -> String {
    match blocker {
        Blocker::MergeConflict => "merge conflict".into(),
        Blocker::CiFailing => "CI failing".into(),
        Blocker::ChangesRequested => "changes requested".into(),
        Blocker::UnresolvedComments(n) => format!("{n} unresolved"),
        Blocker::NoReviewers { .. } => "reviewers missing".into(),
    }
}

/// Split a note into a primary phrase and an optional " — " remedy, for the
/// review-queue exceptional notes ("CI red — maybe wait for green").
fn split_remedy(text: &str) -> (String, Option<String>) {
    match text.split_once(" — ") {
        Some((head, tail)) => (head.to_string(), Some(tail.to_string())),
        None => (text.to_string(), None),
    }
}

/// Exception-first decomposition of an authored **Action** row: the highest
/// presentation-priority blocker becomes the primary (danger-coloured when it
/// is exceptional), and every remaining blocker becomes a muted context fact in
/// priority order — nothing is dropped.
fn action_presentation(row: &BoardRow, tooltip: String) -> NotePresentation {
    let mut ranked: Vec<&Blocker> = row.blockers.iter().collect();
    ranked.sort_by_key(|b| blocker_rank(b));
    let Some((primary_blocker, rest)) = ranked.split_first() else {
        // An Action row always carries >=1 blocker; degrade calmly if not.
        return NotePresentation {
            tone: Tone::Warning,
            primary: tooltip.clone(),
            remedy: None,
            context: Vec::new(),
            tooltip,
        };
    };
    let tone = if is_exceptional(primary_blocker) {
        Tone::Danger
    } else {
        Tone::Warning
    };
    let (primary, remedy) = blocker_primary(primary_blocker);
    let context = rest.iter().map(|b| blocker_context(b)).collect();
    NotePresentation {
        tone,
        primary,
        remedy,
        context,
        tooltip,
    }
}

/// A one-phrase note: the whole (stripped) note as the primary, no remedy or
/// context. Used for the calm await/done/draft/routine states.
fn plain_note(tone: Tone, tooltip: String) -> NotePresentation {
    NotePresentation {
        tone,
        primary: tooltip.clone(),
        remedy: None,
        context: Vec::new(),
        tooltip,
    }
}

/// Turn a row into its calm-then-exception Note presentation.
pub fn note_presentation(row: &BoardRow) -> NotePresentation {
    let tooltip = strip_note_glyphs(&row.note);
    match row.category {
        Category::Action => action_presentation(row, tooltip),
        // Review queue: a red health signal interrupts; otherwise it is a
        // routine "please review", warned but calm — never a wall of alarm.
        Category::Todo | Category::Available => {
            if row.ci == Ci::Fail || row.conflict {
                let (primary, remedy) = split_remedy(&tooltip);
                NotePresentation {
                    tone: Tone::Danger,
                    primary,
                    remedy,
                    context: Vec::new(),
                    tooltip,
                }
            } else {
                plain_note(Tone::Warning, tooltip)
            }
        }
        Category::Await => plain_note(Tone::Success, tooltip),
        // "You approved" is good news; "you commented / requested changes" is
        // neutral — the ball is on the author, nothing is wrong.
        Category::Done => {
            if row.my_review.as_deref() == Some("APPROVED") {
                plain_note(Tone::Success, tooltip)
            } else {
                plain_note(Tone::Routine, tooltip)
            }
        }
        Category::Draft => plain_note(Tone::Muted, tooltip),
    }
}

/// One reviewer's mark in the Review column: the glyph comes first so that the
/// state survives truncation, and it is never the only thing that says what
/// happened — [`ReviewCell::summary`] spells it out in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewMark {
    pub glyph: String,
    pub tone: Tone,
    pub login: String,
}

/// What the Review column shows for one row.
///
/// Completed reviews win over a pending request: a review supersedes the
/// invitation to give it. Merging the two into one column was what recovered
/// the width Title and Note needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewCell {
    /// Somebody has reviewed. `summary` is the aggregate state, already
    /// prefixed with the em dash the row shows ("— approved").
    Reviewed {
        marks: Vec<ReviewMark>,
        summary: String,
        hover: String,
    },
    /// Nobody has reviewed yet, but reviewers were asked.
    Requested {
        /// The logins, comma-separated, for the front end to elide.
        names: String,
        arrow: String,
        suffix: String,
        hover: String,
    },
    /// Nobody was asked. The one state that is worth saying out loud, because
    /// an empty cell reads as "not loaded".
    NotRequested { text: String },
}

/// Calm reviewer-state glyph and its tone. Rendered before the login: on
/// truncation the glyph is the information.
fn review_glyph(state: &str) -> (&'static str, Tone) {
    match state {
        "APPROVED" => ("✓", Tone::Success),
        "COMMENTED" => ("·", Tone::Muted),
        "CHANGES_REQUESTED" => ("±", Tone::Danger),
        // ✕, not "–": a bare dash reads as "nothing" — dismissed is an
        // invalidated review, which is information.
        "DISMISSED" => ("✕", Tone::Muted),
        _ => ("·", Tone::Muted),
    }
}

/// Aggregate review-state word for the merged Review column ("✓ alice —
/// approved"). Requested-but-unreviewed is handled separately.
fn review_state_word_aggregate(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::Changes => "changes requested",
        ReviewState::Commented => "commented",
        ReviewState::Waiting => "requested",
        ReviewState::None => "reviewed",
    }
}

/// One reviewer's state, in the words the hover text uses.
fn review_state_word(state: &str) -> &'static str {
    match state {
        "APPROVED" => "approved",
        "COMMENTED" => "commented",
        "CHANGES_REQUESTED" => "requested changes",
        "DISMISSED" => "dismissed",
        _ => "reviewed",
    }
}

/// The Review column for one row.
pub fn review_cell(row: &BoardRow) -> ReviewCell {
    if !row.reviews.is_empty() {
        let marks = row
            .reviews
            .iter()
            .map(|review| {
                let (glyph, tone) = review_glyph(&review.state);
                ReviewMark {
                    glyph: glyph.to_owned(),
                    tone,
                    login: review.login.clone().unwrap_or_else(|| "?".into()),
                }
            })
            .collect();
        let hover = row
            .reviews
            .iter()
            .map(|review| {
                format!(
                    "{} {}",
                    review.login.as_deref().unwrap_or("?"),
                    review_state_word(&review.state)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        return ReviewCell::Reviewed {
            marks,
            summary: format!("— {}", review_state_word_aggregate(row.review_state)),
            hover,
        };
    }
    if !row.requested.is_empty() {
        let names = row.requested.join(", ");
        return ReviewCell::Requested {
            hover: format!("requested: {names}"),
            names,
            arrow: "→".to_owned(),
            suffix: "— requested".to_owned(),
        };
    }
    ReviewCell::NotRequested {
        text: "Not requested".to_owned(),
    }
}

/// The CI column: the word, and how loud it is. `None` is an em dash rather
/// than an empty cell, because "no checks" and "not loaded" must not look the
/// same.
pub fn ci_cell(ci: Ci) -> (&'static str, Tone) {
    match ci {
        Ci::Pass => ("pass", Tone::Success),
        Ci::Fail => ("fail", Tone::Danger),
        Ci::Running => ("running", Tone::Warning),
        Ci::None => ("—", Tone::Muted),
    }
}

/// The line under a stack sub-header, and the hover text that explains why a
/// layer may be missing from the group.
pub fn stack_layer_hover(number: u64, position: Option<u64>, size: u64, base_ref: &str) -> String {
    let position = position
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".into());
    format!(
        "Stack #{number} · layer {position}/{size} · base {base_ref}. \
         Only matching PRs are shown; layers may be in other sections."
    )
}

/// The branch prefix drawn before a stacked row's title: `└─ 3/3` for the last
/// layer on screen, `├─ 2/3` for one with more below it.
pub fn stack_branch(position: Option<u64>, size: u64, last_in_group: bool) -> String {
    let branch = if last_in_group { "└─" } else { "├─" };
    let position = position
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".into());
    format!("{branch} {position}/{size}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::ReviewSummary;

    fn row() -> BoardRow {
        BoardRow {
            id: "PR_1".into(),
            repo: "demo-labs/atlas".into(),
            updated_at: None,
            head_oid: None,
            reviewed_oid: None,
            reviewed_at: None,
            number: 1,
            url: "https://github.com/demo-labs/atlas/pull/1".into(),
            title: "A change".into(),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: None,
            draft: false,
            category: Category::Action,
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
            note: "⚠ merge conflict".into(),
        }
    }

    #[test]
    fn the_worst_blocker_is_the_one_that_is_coloured() {
        let mut r = row();
        r.blockers = vec![
            Blocker::NoReviewers { suggested: vec![] },
            Blocker::MergeConflict,
            Blocker::UnresolvedComments(3),
        ];
        let p = note_presentation(&r);
        assert_eq!(p.tone, Tone::Danger);
        assert_eq!(p.primary, "merge conflict");
        assert_eq!(p.remedy.as_deref(), Some("rebase"));
        // Nothing is dropped: the rest trail as muted context, in priority
        // order rather than the order the note happened to list them.
        assert_eq!(p.context, vec!["3 unresolved", "reviewers missing"]);
    }

    #[test]
    fn a_routine_blocker_is_amber_not_red() {
        let mut r = row();
        r.blockers = vec![Blocker::UnresolvedComments(1)];
        let p = note_presentation(&r);
        assert_eq!(p.tone, Tone::Warning);
        assert_eq!(p.primary, "resolve 1 comment");
        assert!(p.context.is_empty());
    }

    #[test]
    fn suggested_reviewers_are_named_in_the_primary() {
        let mut r = row();
        r.blockers = vec![Blocker::NoReviewers {
            suggested: vec!["alice".into(), "bob".into()],
        }];
        assert_eq!(note_presentation(&r).primary, "assign alice + bob");
    }

    #[test]
    fn a_review_row_is_only_red_when_something_is_actually_broken() {
        let mut r = row();
        r.category = Category::Todo;
        r.note = "please review".into();
        assert_eq!(note_presentation(&r).tone, Tone::Warning);
        r.ci = Ci::Fail;
        assert_eq!(note_presentation(&r).tone, Tone::Danger);
        r.ci = Ci::Pass;
        r.conflict = true;
        assert_eq!(note_presentation(&r).tone, Tone::Danger);
    }

    #[test]
    fn your_own_approval_is_good_news_and_your_comment_is_neutral() {
        let mut r = row();
        r.category = Category::Done;
        r.my_review = Some("APPROVED".into());
        assert_eq!(note_presentation(&r).tone, Tone::Success);
        r.my_review = Some("COMMENTED".into());
        assert_eq!(note_presentation(&r).tone, Tone::Routine);
    }

    #[test]
    fn a_draft_is_dim_and_carries_no_dot() {
        let mut r = row();
        r.category = Category::Draft;
        r.note = "draft".into();
        assert_eq!(note_presentation(&r).tone, Tone::Muted);
    }

    #[test]
    fn the_note_shown_never_keeps_the_status_ball() {
        // The row already carries its status as a tone; repeating it as an
        // emoji is noise, and the desktop strips it the same way.
        let mut r = row();
        r.category = Category::Await;
        r.note = "🟢 ⏳ waiting on alice".into();
        let p = note_presentation(&r);
        assert_eq!(p.primary, "⏳ waiting on alice");
        assert_eq!(p.tooltip, "⏳ waiting on alice");
    }

    #[test]
    fn a_completed_review_supersedes_a_pending_request() {
        let mut r = row();
        r.requested = vec!["carol".into()];
        r.reviews = vec![ReviewSummary {
            login: Some("alice".into()),
            state: "APPROVED".into(),
            submitted_at: None,
        }];
        r.review_state = ReviewState::Approved;
        let ReviewCell::Reviewed {
            marks,
            summary,
            hover,
        } = review_cell(&r)
        else {
            panic!("expected a reviewed cell");
        };
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].glyph, "✓");
        assert_eq!(marks[0].tone, Tone::Success);
        assert_eq!(marks[0].login, "alice");
        assert_eq!(summary, "— approved");
        assert_eq!(hover, "alice approved");
    }

    #[test]
    fn a_dismissed_review_is_not_a_dash() {
        let mut r = row();
        r.reviews = vec![ReviewSummary {
            login: None,
            state: "DISMISSED".into(),
            submitted_at: None,
        }];
        let ReviewCell::Reviewed { marks, .. } = review_cell(&r) else {
            panic!("expected a reviewed cell");
        };
        assert_eq!(marks[0].glyph, "✕");
        assert_eq!(marks[0].login, "?", "a deleted account still reviewed");
    }

    #[test]
    fn who_was_asked_is_named_when_nobody_has_reviewed() {
        let mut r = row();
        r.requested = vec!["mkurkov".into(), "abs".into()];
        let ReviewCell::Requested {
            names,
            arrow,
            suffix,
            hover,
        } = review_cell(&r)
        else {
            panic!("expected a requested cell");
        };
        assert_eq!(names, "mkurkov, abs");
        assert_eq!(arrow, "→");
        assert_eq!(suffix, "— requested");
        assert_eq!(hover, "requested: mkurkov, abs");
    }

    #[test]
    fn nobody_asked_says_so_rather_than_leaving_a_hole() {
        assert_eq!(
            review_cell(&row()),
            ReviewCell::NotRequested {
                text: "Not requested".into()
            }
        );
    }

    #[test]
    fn no_checks_is_a_dash_because_an_empty_cell_reads_as_not_loaded() {
        assert_eq!(ci_cell(Ci::None), ("—", Tone::Muted));
        assert_eq!(ci_cell(Ci::Pass), ("pass", Tone::Success));
        assert_eq!(ci_cell(Ci::Fail), ("fail", Tone::Danger));
        assert_eq!(ci_cell(Ci::Running), ("running", Tone::Warning));
    }

    #[test]
    fn a_stacked_row_says_which_layer_it_is_and_what_is_missing() {
        assert_eq!(stack_branch(Some(2), 3, false), "├─ 2/3");
        assert_eq!(stack_branch(Some(3), 3, true), "└─ 3/3");
        assert_eq!(stack_branch(None, 3, true), "└─ ?/3");
        assert!(stack_layer_hover(11776, Some(2), 3, "main")
            .starts_with("Stack #11776 · layer 2/3 · base main."));
    }
}
