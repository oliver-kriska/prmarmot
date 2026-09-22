//! The sentences that describe the board rather than a pull request: the
//! header's count line and its explanation, the toolbar toggles' tooltips, the
//! blue marker's, the sync line, and the two duration phrasings.
//!
//! These are shared for the same reason [`crate::detail`] is: two front ends
//! that each write "1 needs you" by hand will eventually write two different
//! sentences, and the difference will be found by a user rather than by a test.
//!
//! Nothing here reads a clock. Elapsed seconds arrive as a parameter.

use crate::board::{BoardRow, Category, Mode};
use crate::github::access::AccessGaps;
use crate::layout::group_label;

/// Whether a row of this category counts toward the header's "need you":
/// My PRs' Needs action section, or the review queue's Requested from you and
/// Available to review sections. All open needs the row itself
/// ([`row_needs_you`]); from the category alone it counts only Requested from
/// you.
pub fn needs_you_here(mode: Mode, category: Category) -> bool {
    match mode {
        Mode::Authored => category == Category::Action,
        Mode::Review => matches!(category, Category::Todo | Category::Available),
        Mode::AllOpen => category == Category::Todo,
    }
}

/// Whether this row counts toward the header's "need you". In All open,
/// Needs action holds anyone's PRs, and a teammate's merge conflict is not
/// yours to act on: only your own PRs there count, plus Requested from you.
/// Your own are the rows with blockers, because someone else's PR carries
/// facts and never blockers. Every other view goes by the category.
pub fn row_needs_you(mode: Mode, row: &BoardRow) -> bool {
    match mode {
        Mode::AllOpen => {
            row.category == Category::Todo
                || (row.category == Category::Action && !row.blockers.is_empty())
        }
        _ => needs_you_here(mode, row.category),
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
    /// The badge, across My PRs and the review queue: your PRs that need
    /// action plus reviews requested from you, snoozed ones excluded.
    pub badge: usize,
    /// Both views have loaded, so `badge` is the whole count.
    pub badge_complete: bool,
    pub tracked_loaded: usize,
    pub tracked_total: usize,
    /// How many open PRs GitHub's search found, when the view asked (All
    /// open does; the other views count what they load).
    pub total: Option<u64>,
    /// A `label:` or `author:` filter went to GitHub with the search, so
    /// `total` counts the matches rather than every open PR.
    pub filtered: bool,
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

/// All open's count against what GitHub's search found: "60 of 759 open", or
/// "40 match" when a `label:` or `author:` filter went with the search. The
/// app's header and the CLI's status line both start with it.
pub fn all_open_count(loaded: usize, total: u64, filtered: bool) -> String {
    let found = if filtered { "match" } else { "open" };
    if loaded as u64 >= total {
        format!("{total} {found}")
    } else {
        format!("{loaded} of {total} {found}")
    }
}

/// The header's count line and the explanation behind it.
pub fn header_counts(c: &HeaderCounts, badge_name: BadgeName) -> (String, String) {
    let need_you = match c.need_you {
        0 => "nothing needs you".to_owned(),
        1 => "1 needs you".to_owned(),
        n => format!("{n} need you"),
    };
    let total = c.total.filter(|_| c.mode == Mode::AllOpen);
    let mut line = match total {
        Some(total) => all_open_count(c.loaded, total, c.filtered),
        None => format!("{} loaded", c.loaded),
    };
    if c.truncated && total.is_none() {
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

    let mut tip = match total {
        Some(total) => {
            let which = if c.filtered {
                "match the filter"
            } else {
                "are open in this repository"
            };
            format!("{} of the {total} PRs that {which} are loaded", c.loaded)
        }
        None => format!("{} PRs loaded in this view", c.loaded),
    };
    tip.push_str(match (c.truncated, total) {
        (true, Some(_)) => "; Load more fetches the next ones.",
        (true, None) => "; GitHub has more (Load more).",
        (false, _) => ".",
    });
    let sections = match c.mode {
        Mode::Authored => group_label(Mode::Authored, Category::Action, c.all_repos).to_owned(),
        Mode::Review => format!(
            "{} and Available to review",
            group_label(Mode::Review, Category::Todo, c.all_repos)
        ),
        Mode::AllOpen => format!(
            "{} and your own PRs under {}",
            group_label(Mode::AllOpen, Category::Todo, c.all_repos),
            group_label(Mode::AllOpen, Category::Action, c.all_repos)
        ),
    };
    tip.push_str(&format!(
        "\n{}: the PRs under {sections}, not counting snoozed ones.",
        upper_first(&need_you)
    ));
    tip.push_str(&format!(
        "\n{} shows {}: your PRs that need action plus reviews requested from \
         you, not counting snoozed ones.",
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

/// Why an organization's repositories can be missing after signing in with
/// PR Marmot's own OAuth app: an organization that restricts OAuth apps
/// hides them until an owner approves, and GitHub offers that request on the
/// page where you authorize. Both front ends show it on the one-time-code
/// screen. The GitHub CLI's login is exempt from that restriction, so the
/// sentence says nothing about it.
pub fn organization_approval_note() -> &'static str {
    "An organization that restricts OAuth apps hides its repositories from this sign-in until an \
     owner approves PR Marmot. GitHub offers the request next to the organization when you \
     authorize."
}

/// What the one-time-code screen says while PR Marmot waits for someone to
/// authorize it on GitHub. Both front ends show it as static text: the wait
/// is on the person, who is usually in a browser entering the code, not on
/// the app, so nothing on that screen needs to move to prove it is alive.
///
/// "Enter", not "type": on an iPad the code is pasted.
pub fn device_code_waiting_note(verification_uri: &str) -> String {
    format!(
        "Open {verification_uri}, sign in to GitHub, and enter the code. PR Marmot is waiting and \
         will continue on its own."
    )
}

/// What the token was not allowed to read on this board, in one line, or
/// `None` when it read everything. The rows still show; this says why some of
/// their CI reads "hidden" and what would show it.
pub fn access_notice(gaps: &AccessGaps) -> Option<String> {
    let pull_requests = |n: usize| match n {
        1 => "1 pull request".to_owned(),
        n => format!("{n} pull requests"),
    };
    let mut refused = Vec::new();
    if gaps.ci > 0 {
        refused.push(format!("CI on {}", pull_requests(gaps.ci)));
    }
    if gaps.teams > 0 {
        refused.push(format!(
            "which teams were asked to review {}",
            pull_requests(gaps.teams)
        ));
    }
    if gaps.other > 0 {
        refused.push(format!("some details of {}", pull_requests(gaps.other)));
    }
    if gaps.pull_requests > 0 {
        refused.push(format!("{} at all", pull_requests(gaps.pull_requests)));
    }
    let (last, rest) = refused.split_last()?;
    let what = if rest.is_empty() {
        last.clone()
    } else {
        format!("{} and {last}", rest.join(", "))
    };
    let remedy = if gaps.ci > 0 {
        " Fine-grained tokens never see check runs; a classic token with repo and read:org does."
    } else if gaps.teams > 0 {
        " A classic token with read:org sees them, as does a fine-grained token with the \
         organization's Members: read."
    } else {
        ""
    };
    Some(format!("This token can't read {what}.{remedy}"))
}

/// What a fine-grained token reaches, for the sign-in screen and the docs.
///
/// A fine-grained token reaches one resource owner, chosen when it is created:
/// with you as the owner it sees your repositories, and an organization's
/// private repositories need that organization as the owner. Nothing marks
/// their absence, which is the trap. "A token you own" would misread as "any
/// token you made", so the sentence uses GitHub's own term.
///
/// Its own sentence, not half of a paragraph: a front end that shows the two
/// kinds of token side by side would read a classic clause here as a mistake.
pub fn fine_grained_reach_note() -> &'static str {
    "A token whose resource owner is you cannot see an organization's private repositories — they do \
     not appear, with no error to explain it. To reach them, choose that organization as the resource \
     owner when you create the token."
}

/// What a classic token reaches, the companion to [`fine_grained_reach_note`].
///
/// The scopes are named by the surrounding screen, so the sentence does not
/// repeat them. CI is the one capability worth contrasting: the `gh` login and
/// a device-flow token read it too, so this does not claim to be the only kind
/// that can.
pub fn classic_reach_note() -> &'static str {
    "Reaches every organization you belong to, including their private repositories and ones that \
     restrict OAuth apps, and reads CI, which a fine-grained token cannot."
}

/// Shown under the header when a pasted token is carrying the board while the
/// GitHub CLI is signed in to the same host: the board may be missing an
/// organization's private repositories and would say nothing.
pub fn pasted_token_reach_notice() -> &'static str {
    "Using your pasted token; the gh login on this Mac would also reach organization private repositories."
}

/// What Load more added, once it lands. New rows join their sections rather
/// than the bottom of the list, so without this sentence nothing on screen
/// says where they went, or that a page brought none.
pub fn loaded_more_text(added: usize) -> String {
    match added {
        0 => "The next page added no PRs to this view".to_owned(),
        1 => "1 more PR loaded, sorted into its section".to_owned(),
        n => format!("{n} more PRs loaded, sorted into their sections"),
    }
}

/// Why All open cannot show while All repositories is selected: the view is
/// one repository's open PRs, and every repository's would be most of GitHub.
/// The desktop's disabled tab and the error both say it.
pub fn all_open_needs_repository() -> &'static str {
    "Pick a repository to see all of its open PRs."
}

/// All open's line under the filter when part of the filter could not go to
/// GitHub — free words, `is:stale`, `repo:` — and GitHub has more open PRs
/// than are loaded: those parts were checked against the loaded PRs only, so
/// an empty or short result is not the repository's answer. `None` when
/// every loaded PR is every PR, or the whole filter went to GitHub.
pub fn all_open_local_filter_notice(
    local_terms: &[String],
    loaded: usize,
    total: Option<u64>,
    can_load_more: bool,
) -> Option<String> {
    let total = total.filter(|&total| (loaded as u64) < total)?;
    let (last, rest) = local_terms.split_last()?;
    let terms = if rest.is_empty() {
        last.clone()
    } else {
        format!("{} and {last}", rest.join(", "))
    };
    let mut notice = format!("Only the {loaded} loaded PRs of {total} are checked for {terms}.");
    if can_load_more {
        notice.push_str(" Load more to check more.");
    }
    Some(notice)
}

/// The centered body copy shown before a queue's first rows ever arrive.
pub fn queue_loading_text(mode: Mode, all_repos: bool) -> &'static str {
    match (mode, all_repos) {
        (Mode::Authored, true) => "Loading pull requests involving you…",
        (Mode::Authored, false) => "Loading your open PRs…",
        (Mode::Review, _) => "Loading review queue…",
        (Mode::AllOpen, true) => all_open_needs_repository(),
        (Mode::AllOpen, false) => "Loading open PRs…",
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
        (Mode::AllOpen, true) => all_open_needs_repository(),
        (Mode::AllOpen, false) => "No open PRs in this repository",
    }
}

/// All open's empty body when GitHub answered the whole filter, or every open
/// PR is loaded, and nothing matched: a statement about the repository, not
/// about what happens to be loaded. `filter` is the search as the reader
/// wrote it.
pub fn all_open_no_match_text(filter: &str) -> String {
    format!("No open PRs in this repository match {filter}.")
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
                (Mode::AllOpen, _) => "Updating open PRs…",
            };
            format!("{verb} · synced {}", relative(secs))
        }
        (false, Some(secs)) => format!("synced {}", relative(secs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_reach_notes_stand_alone_one_per_kind_of_token() {
        let fine = fine_grained_reach_note();
        assert!(fine.contains("no error to explain it"), "{fine}");
        assert!(
            fine.contains("resource owner") && !fine.contains("you own"),
            "GitHub's term, not one that reads as any token you made: {fine}"
        );
        assert!(
            !fine.to_lowercase().contains("classic"),
            "a card for one kind of token must not describe the other: {fine}"
        );
        let classic = classic_reach_note();
        assert!(classic.contains("reads CI"), "{classic}");
        assert!(
            !classic.contains("only kind"),
            "the gh login and a device-flow token read CI too: {classic}"
        );
        for note in [fine, classic] {
            assert!(
                !note.contains('\n'),
                "one paragraph, wrapped by the front end"
            );
        }
        assert!(pasted_token_reach_notice().starts_with("Using your pasted token"));
    }

    #[test]
    fn the_device_code_waiting_note_names_the_page_and_says_it_continues() {
        let note = device_code_waiting_note("https://github.com/login/device");
        assert_eq!(
            note,
            "Open https://github.com/login/device, sign in to GitHub, and enter the code. PR \
             Marmot is waiting and will continue on its own."
        );
        assert!(
            !note.contains("type"),
            "the code is pasted on an iPad, not typed: {note}"
        );
        assert!(
            !note.contains('\n'),
            "one paragraph, wrapped by the front end"
        );
    }

    #[test]
    fn the_organization_approval_note_is_one_plain_sentence_pair() {
        assert_eq!(
            organization_approval_note(),
            "An organization that restricts OAuth apps hides its repositories from this sign-in \
             until an owner approves PR Marmot. GitHub offers the request next to the organization \
             when you authorize."
        );
        assert!(!organization_approval_note().contains("CLI"));
    }

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
            total: None,
            filtered: false,
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
             The Dock badge shows 3: your PRs that need action plus reviews requested \
             from you, not counting snoozed ones. Only the views loaded \
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
            total: None,
            filtered: false,
        };
        let (line, tip) = header_counts(&both_loaded(Mode::Review, review), BadgeName::Dock);
        assert_eq!(line, "5 loaded · 3 need you");
        assert_eq!(
            tip,
            "5 PRs loaded in this view.\n\
             3 need you: the PRs under Requested from you and Available to review, not \
             counting snoozed ones.\n\
             The Dock badge shows 3: your PRs that need action plus reviews requested \
             from you, not counting snoozed ones."
        );
        let (line, tip) = header_counts(&both_loaded(Mode::Authored, mine), BadgeName::Dock);
        assert_eq!(line, "5 loaded · 2 need you");
        assert!(tip.contains("2 need you: the PRs under Needs action, not counting"));
        assert!(tip.contains("The Dock badge shows 3: your PRs"));
        assert!(!tip.contains("so far"));
    }

    #[test]
    fn all_open_counts_against_what_github_found() {
        let all_open = |loaded, total, filtered, truncated| HeaderCounts {
            loaded,
            truncated,
            mode: Mode::AllOpen,
            all_repos: false,
            need_you: 1,
            badge: 4,
            badge_complete: true,
            tracked_loaded: 0,
            tracked_total: 0,
            total: Some(total),
            filtered,
        };
        let (line, tip) = header_counts(&all_open(60, 759, false, true), BadgeName::Dock);
        assert_eq!(line, "60 of 759 open · 1 needs you");
        assert_eq!(
            tip,
            "60 of the 759 PRs that are open in this repository are loaded; Load more \
             fetches the next ones.\n\
             1 needs you: the PRs under Requested from you and your own PRs under Needs action, \
             not counting snoozed ones.\n\
             The Dock badge shows 4: your PRs that need action plus reviews requested \
             from you, not counting snoozed ones."
        );
        let (line, tip) = header_counts(&all_open(12, 12, true, false), BadgeName::Dock);
        assert_eq!(line, "12 match · 1 needs you");
        assert!(tip.starts_with("12 of the 12 PRs that match the filter are loaded."));
        let mine = HeaderCounts {
            mode: Mode::Authored,
            ..all_open(60, 759, false, true)
        };
        assert_eq!(
            header_counts(&mine, BadgeName::Dock).0,
            "60 loaded · partial results · 1 needs you",
            "My PRs keeps its own line"
        );
    }

    #[test]
    fn load_more_says_how_many_joined_their_sections() {
        assert_eq!(
            loaded_more_text(60),
            "60 more PRs loaded, sorted into their sections"
        );
        assert_eq!(
            loaded_more_text(1),
            "1 more PR loaded, sorted into its section"
        );
        assert_eq!(
            loaded_more_text(0),
            "The next page added no PRs to this view"
        );
    }

    #[test]
    fn all_open_says_when_a_filter_only_looked_at_the_loaded_prs() {
        let terms = ["“login”".to_owned(), "is:stale".to_owned()];
        assert_eq!(
            all_open_local_filter_notice(&terms, 60, Some(759), true).as_deref(),
            Some("Only the 60 loaded PRs of 759 are checked for “login” and is:stale. Load more to check more.")
        );
        assert_eq!(
            all_open_local_filter_notice(&terms[..1], 300, Some(759), false).as_deref(),
            Some("Only the 300 loaded PRs of 759 are checked for “login”.")
        );
        assert_eq!(
            all_open_local_filter_notice(&terms, 59, Some(59), false),
            None
        );
        assert_eq!(all_open_local_filter_notice(&[], 60, Some(759), true), None);
    }

    #[test]
    fn the_ipad_says_app_icon_where_the_desktop_says_dock() {
        let (_, tip) = header_counts(&counts(3, true, (0, 0)), BadgeName::AppIcon);
        assert!(
            tip.contains("The app icon badge shows 3: your PRs"),
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

    #[test]
    fn a_token_refused_nothing_gets_no_notice() {
        assert_eq!(access_notice(&AccessGaps::default()), None);
    }

    #[test]
    fn what_a_token_was_refused_reads_as_one_line_with_the_remedy() {
        let ci_only = AccessGaps {
            ci: 24,
            ..Default::default()
        };
        assert_eq!(
            access_notice(&ci_only).as_deref(),
            Some(
                "This token can't read CI on 24 pull requests. Fine-grained tokens never see \
                 check runs; a classic token with repo and read:org does."
            )
        );
        let teams_only = AccessGaps {
            teams: 1,
            ..Default::default()
        };
        assert_eq!(
            access_notice(&teams_only).as_deref(),
            Some(
                "This token can't read which teams were asked to review 1 pull request. A \
                 classic token with read:org sees them, as does a fine-grained token with the \
                 organization's Members: read."
            )
        );
        let everything = AccessGaps {
            ci: 3,
            teams: 2,
            other: 1,
            pull_requests: 1,
        };
        assert_eq!(
            access_notice(&everything).as_deref(),
            Some(
                "This token can't read CI on 3 pull requests, which teams were asked to review \
                 2 pull requests, some details of 1 pull request and 1 pull request at all. \
                 Fine-grained tokens never see check runs; a classic token with repo and \
                 read:org does."
            )
        );
    }
}
