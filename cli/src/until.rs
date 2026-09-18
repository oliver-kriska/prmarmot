//! `watch --pr … --until`: what a follow can wait for, judged from the same
//! row facts and categories the board shows, so "mergeable" here means what
//! the app's Await group means plus the approval and green CI on top. A merge
//! ends every wait as met: whatever the caller waited for to act is moot.

use std::time::Duration;

use prmarmot_core::board::{BoardRow, Category, Ci, ReviewState, TrackedPrStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The status-check rollup is SUCCESS.
    CiPass,
    /// GitHub's review decision is APPROVED (without one: the latest reviews,
    /// the viewer's included, approve and none request changes).
    Approved,
    /// Approved, CI passing, no conflict, not a draft, nothing blocking.
    Mergeable,
    Merged,
}

impl Condition {
    pub fn key(self) -> &'static str {
        match self {
            Condition::CiPass => "ci-pass",
            Condition::Approved => "approved",
            Condition::Mergeable => "mergeable",
            Condition::Merged => "merged",
        }
    }

    pub fn parse(word: &str) -> Result<Self, String> {
        match word.trim().replace('_', "-").as_str() {
            "ci-pass" => Ok(Condition::CiPass),
            "approved" => Ok(Condition::Approved),
            "mergeable" => Ok(Condition::Mergeable),
            "merged" => Ok(Condition::Merged),
            _ => Err(format!(
                "unknown --until condition: {word} (use ci-pass, approved, mergeable, or merged)"
            )),
        }
    }
}

/// Why a condition can no longer be met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    CiFailed,
    ChangesRequested,
    /// Closed without merging.
    Closed,
    Inaccessible,
}

impl Blocked {
    pub fn key(self) -> &'static str {
        match self {
            Blocked::CiFailed => "ci_failed",
            Blocked::ChangesRequested => "changes_requested",
            Blocked::Closed => "closed",
            Blocked::Inaccessible => "inaccessible",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Blocked::CiFailed => "CI failed",
            Blocked::ChangesRequested => "changes requested",
            Blocked::Closed => "closed without merging",
            Blocked::Inaccessible => "no longer accessible",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Met,
    Blocked(Blocked),
    Pending,
}

/// How a wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Met(Condition),
    /// The PR merged, which meets any wait; lists the conditions asked for
    /// that were never seen (all but `merged`, since seeing one ends the wait).
    Merged(Vec<Condition>),
    /// Every condition asked for is blocked, each with its reason.
    Failed(Vec<(Condition, Blocked)>),
    TimedOut(Duration),
}

/// GitHub's review decision where branch protection sets one. Without it the
/// decision is null, so read the latest reviews instead: the board's
/// `review_state` (other people's) plus the viewer's own `my_review`, with a
/// change request from anyone outweighing approvals, as on the board.
fn approved(row: &BoardRow) -> bool {
    match row.review_decision.as_deref() {
        Some(decision) => decision == "APPROVED",
        None => {
            !changes_requested(row)
                && (row.review_state == ReviewState::Approved
                    || row.my_review.as_deref() == Some("APPROVED"))
        }
    }
}

fn changes_requested(row: &BoardRow) -> bool {
    match row.review_decision.as_deref() {
        Some(decision) => decision == "CHANGES_REQUESTED",
        None => {
            row.review_state == ReviewState::Changes
                || row.my_review.as_deref() == Some("CHANGES_REQUESTED")
        }
    }
}

/// A condition against the followed PR while it is open.
pub fn judge_open(condition: Condition, row: &BoardRow) -> Verdict {
    match condition {
        Condition::CiPass => match row.ci {
            Ci::Pass => Verdict::Met,
            Ci::Fail => Verdict::Blocked(Blocked::CiFailed),
            // No checks at all never turns green, and neither do checks the
            // token may not read; `--timeout` bounds that wait.
            Ci::Running | Ci::None | Ci::Hidden => Verdict::Pending,
        },
        Condition::Approved if approved(row) => Verdict::Met,
        Condition::Approved if changes_requested(row) => {
            Verdict::Blocked(Blocked::ChangesRequested)
        }
        Condition::Approved => Verdict::Pending,
        Condition::Mergeable if row.ci == Ci::Fail => Verdict::Blocked(Blocked::CiFailed),
        Condition::Mergeable if changes_requested(row) => {
            Verdict::Blocked(Blocked::ChangesRequested)
        }
        // Await is the board's "nothing blocks it": not a draft, no conflict,
        // CI not failing, no changes requested, no unresolved threads. A
        // mergeability GitHub is still computing is not a clean bill yet.
        Condition::Mergeable
            if row.category == Category::Await
                && approved(row)
                && row.ci == Ci::Pass
                && !row.mergeable_unknown =>
        {
            Verdict::Met
        }
        Condition::Mergeable | Condition::Merged => Verdict::Pending,
    }
}

/// How a wait ends once the followed PR has left the open state; `None`
/// without conditions (a plain follow).
pub fn settle_gone(conditions: &[Condition], status: TrackedPrStatus) -> Option<Outcome> {
    if conditions.is_empty() {
        return None;
    }
    let reason = match status {
        TrackedPrStatus::Merged => {
            let unseen = conditions
                .iter()
                .copied()
                .filter(|condition| *condition != Condition::Merged)
                .collect();
            return Some(Outcome::Merged(unseen));
        }
        TrackedPrStatus::Closed => Blocked::Closed,
        // An open PR always arrives with a row; without one it is not visible.
        TrackedPrStatus::Inaccessible | TrackedPrStatus::Open => Blocked::Inaccessible,
    };
    Some(Outcome::Failed(
        conditions
            .iter()
            .map(|condition| (*condition, reason))
            .collect(),
    ))
}

/// Any-of: the first condition that holds ends the wait; it fails only once
/// every condition is blocked. `None` keeps waiting.
pub fn settle(conditions: &[Condition], judge: impl Fn(Condition) -> Verdict) -> Option<Outcome> {
    let mut blocked = Vec::new();
    for &condition in conditions {
        match judge(condition) {
            Verdict::Met => return Some(Outcome::Met(condition)),
            Verdict::Blocked(reason) => blocked.push((condition, reason)),
            Verdict::Pending => {}
        }
    }
    (!conditions.is_empty() && blocked.len() == conditions.len())
        .then_some(Outcome::Failed(blocked))
}

/// `90`, `90s`, `30m`, `2h`, `1h30m`.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let invalid = || format!("--timeout needs a duration such as 90s, 30m, or 2h, got: {value}");
    let mut total: u64 = 0;
    let mut digits = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let unit = match ch {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return Err(invalid()),
        };
        let amount: u64 = digits.parse().map_err(|_| invalid())?;
        total = amount
            .checked_mul(unit)
            .and_then(|secs| total.checked_add(secs))
            .ok_or_else(invalid)?;
        digits.clear();
    }
    if !digits.is_empty() {
        let secs: u64 = digits.parse().map_err(|_| invalid())?;
        total = total.checked_add(secs).ok_or_else(invalid)?;
    }
    if total == 0 {
        return Err(invalid());
    }
    Ok(Duration::from_secs(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::row;

    fn open(category: Category) -> BoardRow {
        row(1, category)
    }

    #[test]
    fn conditions_parse_by_key_and_reject_the_rest() {
        assert_eq!(Condition::parse("ci-pass"), Ok(Condition::CiPass));
        assert_eq!(Condition::parse("ci_pass"), Ok(Condition::CiPass));
        assert_eq!(Condition::parse("merged"), Ok(Condition::Merged));
        assert!(Condition::parse("green")
            .unwrap_err()
            .contains("unknown --until"));
        for condition in [
            Condition::CiPass,
            Condition::Approved,
            Condition::Mergeable,
            Condition::Merged,
        ] {
            assert_eq!(Condition::parse(condition.key()), Ok(condition));
        }
    }

    #[test]
    fn ci_pass_waits_through_running_and_no_checks_and_stops_on_failure() {
        let mut pr = open(Category::Await);
        assert_eq!(judge_open(Condition::CiPass, &pr), Verdict::Met);
        pr.ci = Ci::Running;
        assert_eq!(judge_open(Condition::CiPass, &pr), Verdict::Pending);
        pr.ci = Ci::None;
        assert_eq!(judge_open(Condition::CiPass, &pr), Verdict::Pending);
        pr.ci = Ci::Fail;
        assert_eq!(
            judge_open(Condition::CiPass, &pr),
            Verdict::Blocked(Blocked::CiFailed)
        );
        // Requested changes do not end a wait for CI.
        pr.ci = Ci::Running;
        pr.review_decision = Some("CHANGES_REQUESTED".into());
        assert_eq!(judge_open(Condition::CiPass, &pr), Verdict::Pending);
    }

    #[test]
    fn approved_follows_the_review_decision_or_the_latest_reviews() {
        let mut pr = open(Category::Await);
        pr.ci = Ci::Fail;
        pr.review_decision = Some("REVIEW_REQUIRED".into());
        pr.review_state = ReviewState::Approved; // one of two required approvals
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Pending);
        pr.review_decision = Some("APPROVED".into());
        // A red CI does not undo an approval.
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Met);
        pr.review_decision = Some("CHANGES_REQUESTED".into());
        assert_eq!(
            judge_open(Condition::Approved, &pr),
            Verdict::Blocked(Blocked::ChangesRequested)
        );

        // No branch protection: GitHub sends no decision.
        pr.review_decision = None;
        pr.review_state = ReviewState::Approved;
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Met);
        pr.review_state = ReviewState::Changes;
        assert_eq!(
            judge_open(Condition::Approved, &pr),
            Verdict::Blocked(Blocked::ChangesRequested)
        );
        pr.review_state = ReviewState::Waiting;
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Pending);

        // The viewer's own latest review counts too; any change request wins.
        pr.my_review = Some("APPROVED".into());
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Met);
        pr.review_state = ReviewState::Changes;
        assert_eq!(
            judge_open(Condition::Approved, &pr),
            Verdict::Blocked(Blocked::ChangesRequested)
        );
        pr.review_state = ReviewState::Approved;
        pr.my_review = Some("CHANGES_REQUESTED".into());
        assert_eq!(
            judge_open(Condition::Approved, &pr),
            Verdict::Blocked(Blocked::ChangesRequested)
        );
        pr.my_review = Some("DISMISSED".into());
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Met);
        // A decision, when there is one, is the whole answer.
        pr.review_decision = Some("REVIEW_REQUIRED".into());
        pr.my_review = Some("APPROVED".into());
        assert_eq!(judge_open(Condition::Approved, &pr), Verdict::Pending);
    }

    #[test]
    fn mergeable_needs_the_await_group_an_approval_green_ci_and_known_mergeability() {
        let mut pr = open(Category::Await);
        pr.review_decision = Some("APPROVED".into());
        assert_eq!(judge_open(Condition::Mergeable, &pr), Verdict::Met);

        let mut unknown = pr.clone();
        unknown.mergeable_unknown = true;
        let mut running = pr.clone();
        running.ci = Ci::Running;
        let mut unapproved = pr.clone();
        unapproved.review_decision = Some("REVIEW_REQUIRED".into());
        // Conflicts, drafts, and unresolved threads put a PR outside Await.
        let mut blocked = pr.clone();
        blocked.category = Category::Action;
        blocked.conflict = true;
        let mut draft = pr.clone();
        draft.category = Category::Draft;
        for waiting in [unknown, running, unapproved, blocked, draft] {
            assert_eq!(judge_open(Condition::Mergeable, &waiting), Verdict::Pending);
        }

        let mut red = pr.clone();
        red.category = Category::Action;
        red.ci = Ci::Fail;
        assert_eq!(
            judge_open(Condition::Mergeable, &red),
            Verdict::Blocked(Blocked::CiFailed)
        );
        let mut rejected = pr;
        rejected.category = Category::Action;
        rejected.review_decision = Some("CHANGES_REQUESTED".into());
        assert_eq!(
            judge_open(Condition::Mergeable, &rejected),
            Verdict::Blocked(Blocked::ChangesRequested)
        );
        assert_eq!(
            judge_open(Condition::Merged, &open(Category::Await)),
            Verdict::Pending
        );
    }

    #[test]
    fn a_pr_that_left_ends_every_wait() {
        use TrackedPrStatus::*;
        let asked = [Condition::CiPass, Condition::Merged, Condition::Approved];
        assert_eq!(
            settle_gone(&asked, Merged),
            Some(Outcome::Merged(vec![
                Condition::CiPass,
                Condition::Approved
            ]))
        );
        assert_eq!(
            settle_gone(&[Condition::Merged], Merged),
            Some(Outcome::Merged(Vec::new()))
        );
        assert_eq!(
            settle_gone(&asked[..2], Closed),
            Some(Outcome::Failed(vec![
                (Condition::CiPass, Blocked::Closed),
                (Condition::Merged, Blocked::Closed),
            ]))
        );
        assert_eq!(
            settle_gone(&[Condition::Approved], Inaccessible),
            Some(Outcome::Failed(vec![(
                Condition::Approved,
                Blocked::Inaccessible
            )]))
        );
        assert_eq!(settle_gone(&[], Merged), None);
    }

    #[test]
    fn any_condition_ends_the_wait_but_all_must_be_blocked_to_fail() {
        let verdicts = |ci, approved| {
            move |condition| match condition {
                Condition::CiPass => ci,
                _ => approved,
            }
        };
        let both = [Condition::Approved, Condition::CiPass];
        let blocked = Verdict::Blocked(Blocked::ChangesRequested);
        assert_eq!(settle(&both, verdicts(Verdict::Pending, blocked)), None);
        assert_eq!(
            settle(&both, verdicts(Verdict::Met, blocked)),
            Some(Outcome::Met(Condition::CiPass))
        );
        let red = Verdict::Blocked(Blocked::CiFailed);
        assert_eq!(
            settle(&both, verdicts(red, blocked)),
            Some(Outcome::Failed(vec![
                (Condition::Approved, Blocked::ChangesRequested),
                (Condition::CiPass, Blocked::CiFailed),
            ]))
        );
        assert_eq!(settle(&[], |_| Verdict::Met), None);
    }

    #[test]
    fn durations_take_seconds_minutes_and_hours() {
        for (text, secs) in [
            ("90", 90),
            ("90s", 90),
            ("30m", 1800),
            ("2h", 7200),
            ("1h30m", 5400),
            ("1m30", 90),
        ] {
            assert_eq!(
                parse_duration(text),
                Ok(Duration::from_secs(secs)),
                "{text}"
            );
        }
        for bad in [
            "",
            "0",
            "0m",
            "m",
            "5x",
            "1.5h",
            "-1m",
            "99999999999999999999h",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }
}
