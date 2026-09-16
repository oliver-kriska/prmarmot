//! Pickup age: how long a PR has waited for a reviewer to pick it up, shared
//! by the app and the CLI. `tests/parity.rs` pins [`pickup_since`] to the jq
//! oracle in `scripts/prototype-jq/`, which implements the same rule:
//!
//! - Drafts wait for no one, and a PR someone already reviewed has been
//!   picked up (in the review queue, only your own review counts).
//! - Requested from you: since your latest review request if you were asked
//!   by name, otherwise since the latest request for a team that is still
//!   requested.
//! - Your PRs waiting on requested reviewers: since the longest-standing
//!   request, taking each requested reviewer's latest request.
//! - Available to review, and your PRs with no reviewer requested: since the
//!   PR opened.
//! - A request whose event is not in the fetched timeline window counts from
//!   when the PR opened, and every start moves up to the last "ready for
//!   review".
//!
//! Timestamps are GitHub's ISO-8601 UTC text and are compared as text; logins
//! and team slugs ignore ASCII case.

use chrono::{DateTime, Utc};

use crate::board::{BoardRow, Category, ReviewState};
use crate::github::query::{RawPr, RequestedReviewer, TimelineEvent};

/// A PR that has waited this many days is stale (`stale_after_days`).
pub const DEFAULT_STALE_AFTER_DAYS: u64 = 3;

/// One currently requested reviewer.
#[derive(Clone, Copy)]
enum Reviewer<'a> {
    User(&'a str),
    Team(&'a str),
}

impl<'a> Reviewer<'a> {
    fn of(requested: &'a RequestedReviewer) -> Option<Self> {
        match (&requested.login, &requested.slug) {
            (Some(login), _) => Some(Self::User(login)),
            (None, Some(slug)) => Some(Self::Team(slug)),
            (None, None) => None,
        }
    }

    fn matches(self, requested: &RequestedReviewer) -> bool {
        match self {
            Self::User(login) => requested
                .login
                .as_deref()
                .is_some_and(|other| other.eq_ignore_ascii_case(login)),
            Self::Team(slug) => requested
                .slug
                .as_deref()
                .is_some_and(|other| other.eq_ignore_ascii_case(slug)),
        }
    }
}

fn requests(pr: &RawPr) -> Vec<Reviewer<'_>> {
    pr.review_requests
        .nodes
        .iter()
        .filter_map(|node| node.requested_reviewer.as_ref())
        .filter_map(Reviewer::of)
        .collect()
}

fn events<'a>(pr: &'a RawPr, typename: &'a str) -> impl Iterator<Item = &'a TimelineEvent> {
    pr.timeline_items
        .nodes
        .iter()
        .flatten()
        .filter(move |event| event.typename == typename)
}

/// When `reviewer` was last asked, if that event was fetched.
fn requested_at<'a>(pr: &'a RawPr, reviewer: Reviewer<'_>) -> Option<&'a str> {
    events(pr, "ReviewRequestedEvent")
        .filter(|event| {
            event
                .requested_reviewer
                .as_ref()
                .is_some_and(|requested| reviewer.matches(requested))
        })
        .filter_map(|event| event.created_at.as_deref())
        .max()
}

fn ready_at(pr: &RawPr) -> Option<&str> {
    events(pr, "ReadyForReviewEvent")
        .filter_map(|event| event.created_at.as_deref())
        .max()
}

/// When `row` (derived from `pr`) started waiting for a reviewer, or `None`
/// when it is not waiting. `me` is the resolved login.
pub(crate) fn pickup_since(pr: &RawPr, row: &BoardRow, me: &str) -> Option<String> {
    if row.draft {
        return None;
    }
    let opened = pr.created_at.as_str();
    let start = match row.category {
        Category::Todo => {
            let requested = requests(pr);
            let by_name = requested
                .iter()
                .any(|r| matches!(r, Reviewer::User(login) if login.eq_ignore_ascii_case(me)));
            let asked = if by_name {
                requested_at(pr, Reviewer::User(me))
            } else {
                requested
                    .iter()
                    .filter(|r| matches!(r, Reviewer::Team(_)))
                    .filter_map(|&team| requested_at(pr, team))
                    .max()
            };
            asked.unwrap_or(opened)
        }
        Category::Available => opened,
        Category::Action | Category::Await => {
            let reviewed_someone_elses = row
                .author
                .as_deref()
                .is_some_and(|author| !author.eq_ignore_ascii_case(me))
                && matches!(
                    row.my_review.as_deref(),
                    Some("APPROVED" | "COMMENTED" | "CHANGES_REQUESTED")
                );
            match row.review_state {
                _ if reviewed_someone_elses => return None,
                ReviewState::Waiting => requests(pr)
                    .into_iter()
                    .map(|reviewer| requested_at(pr, reviewer).unwrap_or(opened))
                    .min()
                    .unwrap_or(opened),
                ReviewState::None => opened,
                ReviewState::Approved | ReviewState::Changes | ReviewState::Commented => {
                    return None
                }
            }
        }
        Category::Done | Category::Draft => return None,
    };
    Some(start.max(ready_at(pr).unwrap_or_default()).to_owned())
}

/// Seconds `row` has waited at `now`; `None` when it is not waiting.
pub fn waiting_secs(row: &BoardRow, now: DateTime<Utc>) -> Option<u64> {
    let since = DateTime::parse_from_rfc3339(row.waiting_since.as_deref()?).ok()?;
    Some(u64::try_from((now - since.with_timezone(&Utc)).num_seconds()).unwrap_or(0))
}

/// A compact wait: `<1h`, then whole hours below a day, then whole days.
pub fn wait_label(secs: u64) -> String {
    const HOUR: u64 = 3600;
    match secs {
        0..HOUR => "<1h".to_owned(),
        HOUR..86_400 => format!("{}h", secs / HOUR),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Whether `row` has waited `stale_after_days` or longer at `now`.
pub fn is_stale(row: &BoardRow, now: DateTime<Utc>, stale_after_days: u64) -> bool {
    waiting_secs(row, now).is_some_and(|secs| secs >= stale_after_days.saturating_mul(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_read_as_hours_then_days() {
        assert_eq!(wait_label(0), "<1h");
        assert_eq!(wait_label(3599), "<1h");
        assert_eq!(wait_label(3600), "1h");
        assert_eq!(wait_label(16 * 3600 + 59 * 60), "16h");
        assert_eq!(wait_label(86_399), "23h");
        assert_eq!(wait_label(86_400), "1d");
        assert_eq!(wait_label(3 * 86_400 + 86_399), "3d");
    }
}
