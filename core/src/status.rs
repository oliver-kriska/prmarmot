//! The sentences that describe the board rather than a pull request: the
//! header's count line and its explanation, the toolbar toggles' tooltips, the
//! blue marker's, the sync line, and the two duration phrasings.
//!
//! These are shared for the same reason [`crate::detail`] is: two front ends
//! that each write "1 needs you" by hand will eventually write two different
//! sentences, and the difference will be found by a user rather than by a test.
//!
//! Nothing here reads a clock. Elapsed seconds arrive as a parameter.

use crate::board::{Category, Mode};
use crate::layout::group_label;

/// Whether a row of this view counts toward the header's "need you": My PRs'
/// Needs action section, or the review queue's Requested from you and
/// Available to review sections.
pub fn needs_you_here(mode: Mode, category: Category) -> bool {
    match mode {
        Mode::Authored => category == Category::Action,
        Mode::Review => matches!(category, Category::Todo | Category::Available),
    }
}

/// The numbers behind the header's count line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderCounts {
    pub loaded: usize,
    pub truncated: bool,
    pub mode: Mode,
    pub all_repos: bool,
    /// Rows of this view that need you ([`needs_you_here`]), snoozed ones
    /// excluded.
    pub need_you: usize,
    /// The badge, across both views: your PRs that need action plus reviews
    /// requested from you, snoozed ones excluded.
    pub badge: usize,
    /// Both views have loaded, so `badge` is the whole count.
    pub badge_complete: bool,
    pub tracked_loaded: usize,
    pub tracked_total: usize,
}

/// What the badge is called where the reader is looking: the desktop has a
/// Dock, the iPad has an app icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeName {
    Dock,
    AppIcon,
}

impl BadgeName {
    fn words(self) -> &'static str {
        match self {
            Self::Dock => "The Dock badge",
            Self::AppIcon => "The app icon badge",
        }
    }
}

/// The header's count line and the explanation behind it.
pub fn header_counts(c: &HeaderCounts, badge_name: BadgeName) -> (String, String) {
    let need_you = match c.need_you {
        0 => "nothing needs you".to_owned(),
        1 => "1 needs you".to_owned(),
        n => format!("{n} need you"),
    };
    let mut line = format!("{} loaded", c.loaded);
    if c.truncated {
        line.push_str(" · partial results");
    }
    line.push_str(&format!(" · {need_you}"));
    let tracked = match (c.tracked_loaded, c.tracked_total) {
        (_, 0) => None,
        (loaded, total) if loaded >= total => Some(format!("{total} watched/snoozed")),
        (loaded, total) => Some(format!("{loaded} of {total} watched/snoozed")),
    };
    if let Some(tracked) = &tracked {
        line.push_str(&format!(" · {tracked}"));
    }

    let mut tip = format!("{} PRs loaded in this view", c.loaded);
    tip.push_str(if c.truncated {
        "; GitHub has more (Load more)."
    } else {
        "."
    });
    let sections = match c.mode {
        Mode::Authored => group_label(Mode::Authored, Category::Action, c.all_repos).to_owned(),
        Mode::Review => format!(
            "{} and Available to review",
            group_label(Mode::Review, Category::Todo, c.all_repos)
        ),
    };
    tip.push_str(&format!(
        "\n{}: the PRs under {sections}, not counting snoozed ones.",
        upper_first(&need_you)
    ));
    tip.push_str(&format!(
        "\n{} shows {} across both views: your PRs that need action plus \
         reviews requested from you, not counting snoozed ones.",
        badge_name.words(),
        c.badge
    ));
    if !c.badge_complete {
        tip.push_str(" Only the views loaded since launch are counted so far.");
    }
    if let Some(tracked) = tracked {
        tip.push_str(&format!(
            "\n{}: watched and snoozed PRs, refreshed with this view (up to 50 each time).",
            upper_first(&tracked)
        ));
    }
    (line, tip)
}

pub fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The Changed toggle's tooltip.
pub fn changed_toggle_tooltip(on: bool, count: usize) -> String {
    match (on, count) {
        (true, _) => "Showing only PRs that changed since you looked. Click to show all.".into(),
        (false, 0) => "No loaded PRs changed since you looked".into(),
        (false, 1) => "Show only the PR that changed since you looked".into(),
        (false, n) => format!("Show only the {n} PRs that changed since you looked"),
    }
}

/// The Snoozed toggle's tooltip.
pub fn snoozed_toggle_tooltip(on: bool, count: usize) -> String {
    match (on, count) {
        (true, _) => "Collapse the snoozed PRs".into(),
        (false, 0) => "No snoozed PRs here".into(),
        (false, 1) => "Show the snoozed PR".into(),
        (false, n) => format!("Show the {n} snoozed PRs"),
    }
}

/// The blue marker's hover text: what changed, and how to clear it.
pub fn changed_marker_tooltip(changes: &[String]) -> String {
    let what = if changes.is_empty() {
        "Changed on GitHub".to_string()
    } else {
        changes.join(" · ")
    };
    format!("Changed since you last selected it: {what}. Select the PR to clear.")
}

/// "just now", "3m ago", "2h 15m ago".
pub fn relative(secs_ago: i64) -> String {
    match secs_ago.max(0) {
        0..=59 => "just now".to_string(),
        secs @ 60..=3599 => format!("{}m ago", secs / 60),
        secs => format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60),
    }
}

/// "45s", "7m", "1h 5m" — a wait ahead rather than a time behind.
pub fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// The centered body copy shown before a queue's first rows ever arrive.
pub fn queue_loading_text(mode: Mode, all_repos: bool) -> &'static str {
    match (mode, all_repos) {
        (Mode::Authored, true) => "Loading pull requests involving you…",
        (Mode::Authored, false) => "Loading your open PRs…",
        (Mode::Review, _) => "Loading review queue…",
    }
}

/// The centered body copy when a queue has loaded and holds nothing.
///
/// Each queue says its own "nothing here", so an empty review queue never
/// reads as "you have no PRs".
pub fn queue_empty_text(mode: Mode, all_repos: bool) -> &'static str {
    match (mode, all_repos) {
        (Mode::Authored, true) => "No open pull requests involve you",
        (Mode::Authored, false) => "You have no open PRs",
        (Mode::Review, _) => "No requested or available reviews in this result set",
    }
}

/// The status line specific to the active queue. Keeps the "synced Xm ago"
/// anchor visible during a background refresh, so switching views feels like
/// navigation rather than a command re-run.
pub fn queue_sync_text(
    mode: Mode,
    all_repos: bool,
    syncing: bool,
    synced_secs_ago: Option<i64>,
) -> String {
    match (syncing, synced_secs_ago) {
        (_, None) => queue_loading_text(mode, all_repos).to_string(),
        (true, Some(secs)) => {
            let verb = match (mode, all_repos) {
                (Mode::Authored, true) => "Updating involving PRs…",
                (Mode::Authored, false) => "Updating your PRs…",
                (Mode::Review, _) => "Updating review queue…",
            };
            format!("{verb} · synced {}", relative(secs))
        }
        (false, Some(secs)) => format!("synced {}", relative(secs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(need_you: usize, complete: bool, tracked: (usize, usize)) -> HeaderCounts {
        HeaderCounts {
            loaded: 56,
            truncated: true,
            mode: Mode::Authored,
            all_repos: false,
            need_you,
            badge: need_you,
            badge_complete: complete,
            tracked_loaded: tracked.0,
            tracked_total: tracked.1,
        }
    }

    #[test]
    fn the_header_says_who_needs_you_in_plain_words() {
        let line = |c: HeaderCounts| header_counts(&c, BadgeName::Dock).0;
        assert_eq!(
            line(counts(0, true, (0, 0))),
            "56 loaded · partial results · nothing needs you"
        );
        assert_eq!(
            line(counts(1, false, (2, 2))),
            "56 loaded · partial results · 1 needs you · 2 watched/snoozed"
        );
        assert_eq!(
            line(counts(3, true, (50, 64))),
            "56 loaded · partial results · 3 need you · 50 of 64 watched/snoozed"
        );
        let (_, tip) = header_counts(&counts(3, false, (2, 2)), BadgeName::Dock);
        assert_eq!(
            tip,
            "56 PRs loaded in this view; GitHub has more (Load more).\n\
             3 need you: the PRs under Needs action, not counting snoozed ones.\n\
             The Dock badge shows 3 across both views: your PRs that need action plus \
             reviews requested from you, not counting snoozed ones. Only the views loaded \
             since launch are counted so far.\n\
             2 watched/snoozed: watched and snoozed PRs, refreshed with this view (up to 50 \
             each time)."
        );
    }

    /// Once both views have loaded, the badge is the total, but the header
    /// still counts only the view on screen.
    #[test]
    fn the_header_counts_this_view_even_when_the_badge_covers_both() {
        let count = |mode, categories: &[Category]| {
            categories
                .iter()
                .filter(|&&category| needs_you_here(mode, category))
                .count()
        };
        let mine = count(
            Mode::Authored,
            &[
                Category::Action,
                Category::Action,
                Category::Await,
                Category::Draft,
            ],
        );
        let review = count(
            Mode::Review,
            &[
                Category::Todo,
                Category::Available,
                Category::Available,
                Category::Done,
                Category::Draft,
            ],
        );
        assert_eq!((mine, review), (2, 3));
        let both_loaded = |mode, need_you| HeaderCounts {
            loaded: 5,
            truncated: false,
            mode,
            all_repos: true,
            need_you,
            badge: 3,
            badge_complete: true,
            tracked_loaded: 0,
            tracked_total: 0,
        };
        let (line, tip) = header_counts(&both_loaded(Mode::Review, review), BadgeName::Dock);
        assert_eq!(line, "5 loaded · 3 need you");
        assert_eq!(
            tip,
            "5 PRs loaded in this view.\n\
             3 need you: the PRs under Requested from you and Available to review, not \
             counting snoozed ones.\n\
             The Dock badge shows 3 across both views: your PRs that need action plus \
             reviews requested from you, not counting snoozed ones."
        );
        let (line, tip) = header_counts(&both_loaded(Mode::Authored, mine), BadgeName::Dock);
        assert_eq!(line, "5 loaded · 2 need you");
        assert!(tip.contains("2 need you: the PRs under Needs attention, not counting"));
        assert!(tip.contains("The Dock badge shows 3 across both views"));
        assert!(!tip.contains("so far"));
    }

    #[test]
    fn the_ipad_says_app_icon_where_the_desktop_says_dock() {
        let (_, tip) = header_counts(&counts(3, true, (0, 0)), BadgeName::AppIcon);
        assert!(
            tip.contains("The app icon badge shows 3 across both views"),
            "{tip}"
        );
        assert!(!tip.contains("Dock"));
    }

    #[test]
    fn the_toggles_say_what_they_will_do_and_how_many_it_is_about() {
        assert_eq!(
            changed_toggle_tooltip(false, 0),
            "No loaded PRs changed since you looked"
        );
        assert_eq!(
            changed_toggle_tooltip(false, 1),
            "Show only the PR that changed since you looked"
        );
        assert_eq!(
            changed_toggle_tooltip(false, 4),
            "Show only the 4 PRs that changed since you looked"
        );
        assert_eq!(
            changed_toggle_tooltip(true, 4),
            "Showing only PRs that changed since you looked. Click to show all."
        );
        assert_eq!(snoozed_toggle_tooltip(false, 0), "No snoozed PRs here");
        assert_eq!(snoozed_toggle_tooltip(false, 1), "Show the snoozed PR");
        assert_eq!(snoozed_toggle_tooltip(false, 3), "Show the 3 snoozed PRs");
        assert_eq!(snoozed_toggle_tooltip(true, 3), "Collapse the snoozed PRs");
    }

    #[test]
    fn the_marker_says_what_changed_and_how_to_clear_it() {
        assert_eq!(
            changed_marker_tooltip(&["New commits".into(), "CI passing → failing".into()]),
            "Changed since you last selected it: New commits · CI passing → failing. \
             Select the PR to clear."
        );
        assert_eq!(
            changed_marker_tooltip(&[]),
            "Changed since you last selected it: Changed on GitHub. Select the PR to clear."
        );
    }

    #[test]
    fn durations_read_as_words_in_both_directions() {
        assert_eq!(relative(0), "just now");
        assert_eq!(relative(59), "just now");
        assert_eq!(relative(180), "3m ago");
        assert_eq!(relative(8_100), "2h 15m ago");
        assert_eq!(relative(-5), "just now");
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(600), "10m");
        assert_eq!(human_duration(3_900), "1h 5m");
    }

    #[test]
    fn the_sync_line_keeps_the_anchor_while_it_refreshes() {
        assert_eq!(
            queue_sync_text(Mode::Authored, true, false, None),
            "Loading pull requests involving you…"
        );
        assert_eq!(
            queue_sync_text(Mode::Authored, false, true, None),
            "Loading your open PRs…"
        );
        assert_eq!(
            queue_sync_text(Mode::Review, true, true, Some(300)),
            "Updating review queue… · synced 5m ago"
        );
        assert_eq!(
            queue_sync_text(Mode::Authored, true, false, Some(120)),
            "synced 2m ago"
        );
    }

    #[test]
    fn an_empty_queue_says_which_queue_is_empty() {
        assert_eq!(
            queue_empty_text(Mode::Authored, true),
            "No open pull requests involve you"
        );
        assert_eq!(
            queue_empty_text(Mode::Authored, false),
            "You have no open PRs"
        );
        assert_eq!(
            queue_empty_text(Mode::Review, true),
            "No requested or available reviews in this result set",
            "an empty review queue must never read as having no PRs at all"
        );
        assert_eq!(
            queue_empty_text(Mode::Review, false),
            queue_empty_text(Mode::Review, true)
        );
    }
}
