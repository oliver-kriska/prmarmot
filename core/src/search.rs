//! The board search grammar: free words plus `label:`, `author:`, `repo:` and
//! `is:stale`, matched against rows already loaded. Words, labels and
//! `is:stale` must all match; several authors, or several repositories, match
//! any one of them — a PR has one of each, and GitHub's search reads them the
//! same way, which All open relies on when it sends them there.
//!
//! It lives in core, not in a front end, because otherwise the desktop app and
//! the iPad would drift on what `label:"help wanted"` means. Drawing the chips
//! stays in each front end; deciding what they match is here.
//!
//! Nothing in this module reads the clock: `is:stale` takes a [`StaleRule`]
//! carrying `now`, the same rule `pickup` follows, which is what lets the
//! goldens pin the behaviour and lets iOS pass its own clock.

use chrono::{DateTime, Utc};

use crate::board::BoardRow;
use crate::pickup::is_stale;

/// A `key:value` search term. Each matches one field whole and ignoring
/// case; a new one is added here and in [`Qualifier::matches`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    Label,
    Author,
    Repo,
    /// `is:stale`: waited `stale_after_days` or longer for a reviewer.
    Is,
}

/// When the table is filtered, and how long a wait makes a PR stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleRule {
    pub now: DateTime<Utc>,
    pub after_days: u64,
}

impl StaleRule {
    pub fn is_stale(self, row: &BoardRow) -> bool {
        is_stale(row, self.now, self.after_days)
    }
}

impl Qualifier {
    const ALL: [Qualifier; 4] = [
        Qualifier::Label,
        Qualifier::Author,
        Qualifier::Repo,
        Qualifier::Is,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Qualifier::Label => "label",
            Qualifier::Author => "author",
            Qualifier::Repo => "repo",
            Qualifier::Is => "is",
        }
    }

    /// `label:bug`, or `label:"help wanted"` when the value needs quotes.
    fn term(self, value: &str) -> String {
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            format!("{}:\"{value}\"", self.key())
        } else {
            format!("{}:{value}", self.key())
        }
    }

    /// Whether `row`'s field is `value`, whole and ignoring case.
    fn matches(self, row: &BoardRow, value: &str, stale: StaleRule) -> bool {
        match self {
            Qualifier::Label => has_label(row, value),
            Qualifier::Author => row
                .author
                .as_deref()
                .is_some_and(|author| same_text(author, value)),
            Qualifier::Repo => same_text(&row.repo, value),
            Qualifier::Is => same_text(value, "stale") && stale.is_stale(row),
        }
    }
}

fn same_text(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A finished qualifier term: a chip in the search box, or what a click on a
/// label, author, or repository adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterChip {
    pub qualifier: Qualifier,
    pub value: String,
}

impl FilterChip {
    pub fn new(qualifier: Qualifier, value: impl Into<String>) -> Self {
        Self {
            qualifier,
            value: value.into(),
        }
    }

    /// The term as a user would type it.
    pub fn term(&self) -> String {
        self.qualifier.term(&self.value)
    }

    pub fn matches(&self, row: &BoardRow, stale: StaleRule) -> bool {
        self.qualifier.matches(row, &self.value, stale)
    }

    /// The same filter, ignoring case.
    pub fn same_as(&self, other: &FilterChip) -> bool {
        self.qualifier == other.qualifier && same_text(&self.value, &other.value)
    }
}

/// One search term: a free word or a qualifier, with its byte range in the
/// query so a pill can remove exactly the text it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterTerm {
    pub qualifier: Option<Qualifier>,
    /// The word, or the qualifier's value, without quotes.
    pub value: String,
    pub span: std::ops::Range<usize>,
}

/// Split a search at whitespace outside double quotes. Quotes group words
/// (`label:"help wanted"`, `"two words"`) and are dropped from the value; an
/// unclosed quote runs to the end. A term is a qualifier only when its key
/// comes before any quote.
pub fn filter_terms(query: &str) -> Vec<FilterTerm> {
    let mut terms = Vec::new();
    let mut chars = query.char_indices().peekable();
    while let Some(&(start, ch)) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        let (mut end, mut value, mut quoted) = (start, String::new(), false);
        while let Some(&(ix, ch)) = chars.peek() {
            if ch.is_whitespace() && !quoted {
                break;
            }
            chars.next();
            end = ix + ch.len_utf8();
            if ch == '"' {
                quoted = !quoted;
            } else {
                value.push(ch);
            }
        }
        let text = &query[start..end];
        let qualifier = Qualifier::ALL.into_iter().find(|qualifier| {
            let key = qualifier.key();
            text.as_bytes().get(key.len()) == Some(&b':')
                && text
                    .get(..key.len())
                    .is_some_and(|k| k.eq_ignore_ascii_case(key))
        });
        if let Some(qualifier) = qualifier {
            // The key and colon are ASCII and precede any quote.
            value.drain(..=qualifier.key().len());
        }
        terms.push(FilterTerm {
            qualifier,
            value,
            span: start..end,
        });
    }
    terms
}

/// Whether the PR carries `label`, compared whole and case-insensitively.
pub fn has_label(row: &BoardRow, label: &str) -> bool {
    row.labels.iter().any(|name| same_text(name, label))
}

/// `query` with `chip`'s term appended, unless it has the same one.
pub fn with_filter(query: &str, chip: &FilterChip) -> String {
    if filter_terms(query).iter().any(|term| {
        term.qualifier
            .is_some_and(|q| chip.same_as(&FilterChip::new(q, term.value.as_str())))
    }) {
        return query.to_owned();
    }
    let term = chip.term();
    match query.trim_end() {
        "" => term,
        rest => format!("{rest} {term}"),
    }
}

/// Finished qualifier terms taken out of typed search text, for the search
/// box's chips, and the text left behind. `all` (Enter) takes every one;
/// while typing, only a term just ended by a space, so nothing is taken
/// mid-word or from the middle of the text.
pub fn take_filter_chips(text: &str, all: bool) -> (Vec<FilterChip>, String) {
    let terms = filter_terms(text);
    let finished = |term: &&FilterTerm| term.qualifier.is_some() && !term.value.is_empty();
    let taken: Vec<&FilterTerm> = if all {
        terms.iter().filter(finished).collect()
    } else {
        // An unclosed quote swallows the trailing space into the term.
        terms
            .last()
            .filter(finished)
            .filter(|term| term.span.end < text.len())
            .into_iter()
            .collect()
    };
    if taken.is_empty() {
        return (Vec::new(), text.to_owned());
    }
    let mut rest = text.to_owned();
    // Back to front, so earlier spans still point at the same text.
    for term in taken.iter().rev() {
        let before = rest[..term.span.start].trim_end();
        let after = rest[term.span.end..].trim_start();
        rest = if before.is_empty() || after.is_empty() {
            format!("{before}{after}")
        } else {
            format!("{before} {after}")
        };
    }
    // Typing goes on after the space that finished the term.
    if !all && !rest.is_empty() {
        rest.push(' ');
    }
    let chips = taken
        .iter()
        .filter_map(|term| {
            term.qualifier
                .map(|q| FilterChip::new(q, term.value.as_str()))
        })
        .collect();
    (chips, rest)
}

/// At most this many labels, and this many authors, are sent to GitHub. The
/// search box holds fewer chips than that; the bound keeps the query short
/// whatever a front end allows.
pub const MAX_REMOTE_TERMS: usize = 8;

/// The part of a filter that GitHub's search answers exactly: labels and
/// authors. All open loads a page of a repository that may hold a thousand
/// open PRs, so filtering only the loaded rows would state a confident wrong
/// answer; these go into the search itself, and the count and Load more then
/// cover the whole repository.
///
/// What GitHub does with them (measured 2026-09-21): qualifiers ignore case;
/// several `label:` must all match; several `author:` match any of them; an
/// unknown label or author matches nothing, with no error. An app's PRs carry
/// its bare login (`dependabot`) yet match only `author:app/dependabot`, so each
/// author is sent in both forms. The loaded rows are still filtered locally by
/// every chip, so the result means exactly what the chips say.
///
/// Free words, `repo:` and `is:stale` stay local: GitHub cannot answer them
/// the same way (`is:stale` is PR Marmot's own rule).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RemoteFilter {
    labels: Vec<String>,
    authors: Vec<String>,
}

impl RemoteFilter {
    /// The chips GitHub can answer, lowercased, deduplicated, and sorted, so
    /// the same filter in another order or case is the same search.
    pub fn from_chips(chips: &[FilterChip]) -> Self {
        let mut labels: Vec<String> = chips
            .iter()
            .filter(|chip| chip.qualifier == Qualifier::Label)
            .filter_map(|chip| remote_label(&chip.value))
            .collect();
        let mut authors: Vec<String> = chips
            .iter()
            .filter(|chip| chip.qualifier == Qualifier::Author)
            .filter_map(|chip| remote_author(&chip.value))
            .collect();
        for values in [&mut labels, &mut authors] {
            values.sort();
            values.dedup();
            values.truncate(MAX_REMOTE_TERMS);
        }
        Self { labels, authors }
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.authors.is_empty()
    }

    /// The qualifiers to append to a search, each with a leading space, or
    /// nothing. Labels are always quoted; authors are logins and never need it.
    pub fn qualifiers(&self) -> String {
        let mut out = String::new();
        for label in &self.labels {
            out.push_str(&format!(" label:\"{label}\""));
        }
        for author in &self.authors {
            out.push_str(&format!(" author:{author} author:app/{author}"));
        }
        out
    }

    /// Whether GitHub answers `chip`, rather than only the loaded rows.
    pub fn sends(&self, chip: &FilterChip) -> bool {
        match chip.qualifier {
            Qualifier::Label => {
                remote_label(&chip.value).is_some_and(|label| self.labels.contains(&label))
            }
            Qualifier::Author => {
                remote_author(&chip.value).is_some_and(|author| self.authors.contains(&author))
            }
            Qualifier::Repo | Qualifier::Is => false,
        }
    }
}

/// A label GitHub can be asked for inside quotes: at most 50 characters (the
/// longest label GitHub allows), no quote or backslash, no control character.
fn remote_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= 50
        && !value
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control()))
    .then(|| value.to_lowercase())
}

/// A GitHub login: 1–39 letters, digits, or hyphens, not starting with one.
fn remote_author(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    .then(|| value.to_lowercase())
}

/// The active search terms that filter only the rows already loaded, as the
/// reader typed them: free words, and the chips GitHub does not answer. When
/// a view is truncated these can hide a match that is not loaded yet, and the
/// front end says so.
pub fn local_only_terms(text: &str, chips: &[FilterChip], remote: &RemoteFilter) -> Vec<String> {
    let mut terms: Vec<String> = chips
        .iter()
        .filter(|chip| !chip.value.is_empty() && !remote.sends(chip))
        .map(FilterChip::term)
        .collect();
    terms.extend(filter_terms(text).into_iter().filter_map(|term| {
        let chip = term
            .qualifier
            .map(|q| FilterChip::new(q, term.value.as_str()));
        match chip {
            // `label:` still being typed filters nothing yet.
            _ if term.value.is_empty() => None,
            Some(chip) if remote.sends(&chip) => None,
            Some(chip) => Some(chip.term()),
            None => Some(format!("“{}”", term.value)),
        }
    }));
    terms
}

/// Case-insensitive search across the fields users scan in the board: every
/// free word must appear somewhere, every `label:` and `is:stale` must match
/// its field exactly, and of several `author:` (or `repo:`) terms one must
/// match. Filtering is local; it must not trigger GitHub requests on each
/// keystroke.
pub fn matches_filter(row: &BoardRow, query: &str, stale: StaleRule) -> bool {
    let terms = filter_terms(query);
    let terms: Vec<(Option<Qualifier>, &str)> = terms
        .iter()
        .map(|term| (term.qualifier, term.value.as_str()))
        .collect();
    matches_terms(row, &terms, stale)
}

/// [`matches_filter`] over the search box as a whole: its chips and the words
/// still typed, read as one search.
pub fn matches_search(row: &BoardRow, text: &str, chips: &[FilterChip], stale: StaleRule) -> bool {
    let typed = filter_terms(text);
    let terms: Vec<(Option<Qualifier>, &str)> = chips
        .iter()
        .map(|chip| (Some(chip.qualifier), chip.value.as_str()))
        .chain(
            typed
                .iter()
                .map(|term| (term.qualifier, term.value.as_str())),
        )
        .collect();
    matches_terms(row, &terms, stale)
}

fn matches_terms(row: &BoardRow, terms: &[(Option<Qualifier>, &str)], stale: StaleRule) -> bool {
    // `label:` alone is still being typed; it filters nothing yet.
    let terms: Vec<_> = terms
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .collect();
    if terms.is_empty() {
        return true;
    }
    let any_of = |qualifier: Qualifier| {
        let mut values = terms
            .iter()
            .filter(|(q, _)| *q == Some(qualifier))
            .peekable();
        values.peek().is_none() || values.any(|(_, value)| qualifier.matches(row, value, stale))
    };
    if !any_of(Qualifier::Author) || !any_of(Qualifier::Repo) {
        return false;
    }
    let text = format!(
        "#{} {} {} {} {} {} {}",
        row.number,
        row.repo,
        row.title,
        row.author.as_deref().unwrap_or_default(),
        row.labels.join(" "),
        row.issue.as_deref().unwrap_or_default(),
        row.note
    )
    .to_lowercase();
    terms.iter().all(|(qualifier, value)| match qualifier {
        None => text.contains(&value.to_lowercase()),
        Some(Qualifier::Author | Qualifier::Repo) => true,
        Some(qualifier) => qualifier.matches(row, value, stale),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Category, Ci, ReviewState};

    /// A minimal row; every test sets the fields it cares about.
    fn row(number: u64, category: Category) -> BoardRow {
        BoardRow {
            id: format!("PR_{number}"),
            repo: "acme/widgets".into(),
            updated_at: None,
            head_oid: None,
            reviewed_oid: None,
            reviewed_at: None,
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("Change number {number}"),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: None,
            draft: category == Category::Draft,
            category,
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
            note: "waiting on bob".into(),
        }
    }

    /// A fixed "now" so `is:stale` is deterministic.
    fn rule() -> StaleRule {
        StaleRule {
            now: DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            after_days: 3,
        }
    }

    fn filtered(row: &BoardRow, query: &str) -> bool {
        matches_filter(row, query, rule())
    }

    #[test]
    fn filter_matches_all_words_across_loaded_fields() {
        let mut r = row(42, Category::Action);
        r.title = "Improve mobile settings".into();
        r.author = Some("Alice".into());
        r.labels = vec!["mobile-dev".into()];
        r.issue = Some("APP-123".into());
        assert!(filtered(&r, "  ALICE mobile-dev app-123 #42 "));
        assert!(filtered(&r, ""));
        assert!(!filtered(&r, "alice desktop"));
    }

    #[test]
    fn label_terms_match_whole_label_names() {
        let mut r = row(42, Category::Action);
        r.title = "Fix login".into();
        r.labels = vec!["bugfix".into(), "Help Wanted".into()];
        // Free words still match inside labels; `label:` only whole names.
        assert!(filtered(&r, "bug"));
        assert!(!filtered(&r, "label:bug"));
        assert!(filtered(&r, "label:BUGFIX"));
        assert!(filtered(&r, "LABEL:bugfix"));
        assert!(!filtered(&r, "label:help"));
        assert!(filtered(&r, "label:\"help wanted\""));
        assert!(
            filtered(&r, "label:\"help wanted"),
            "an unclosed quote runs to the end"
        );
        // Several qualifiers must all hold, alongside free words.
        assert!(filtered(&r, "login label:bugfix label:\"Help Wanted\""));
        assert!(!filtered(&r, "login label:bugfix label:docs"));
        assert!(!filtered(&r, "logout label:bugfix"));
        // A quoted free phrase, and a key that is not a qualifier.
        assert!(filtered(&r, "\"fix login\""));
        assert!(!filtered(&r, "\"login fix\""));
        assert!(!filtered(&r, "\"label:bugfix\""));
        assert!(!filtered(&r, "reviewer:alice"));
        // `label:` alone is still being typed.
        assert!(filtered(&r, "label:"));
        assert!(filtered(&r, "login label:"));
        r.labels.clear();
        assert!(!filtered(&r, "label:bugfix"));
    }

    #[test]
    fn filter_terms_keep_spans_and_values() {
        let query = "  fix label:\"help wanted\" é Label:bug";
        let terms = filter_terms(query);
        let values: Vec<_> = terms
            .iter()
            .map(|term| {
                (
                    term.qualifier,
                    term.value.as_str(),
                    &query[term.span.clone()],
                )
            })
            .collect();
        assert_eq!(
            values,
            [
                (None, "fix", "fix"),
                (
                    Some(Qualifier::Label),
                    "help wanted",
                    "label:\"help wanted\""
                ),
                (None, "é", "é"),
                (Some(Qualifier::Label), "bug", "Label:bug"),
            ]
        );
    }

    #[test]
    fn author_and_repo_terms_match_whole_values() {
        let mut r = row(7, Category::Await);
        r.title = "Fix login".into();
        r.author = Some("Alice".into());
        r.labels = vec!["alice".into()];
        assert!(filtered(&r, "author:alice"));
        assert!(filtered(&r, "AUTHOR:\"ALICE\""));
        assert!(!filtered(&r, "author:ali"));
        assert!(filtered(&r, "repo:ACME/Widgets"));
        assert!(!filtered(&r, "repo:widgets"));
        assert!(filtered(&r, "login author:alice repo:acme/widgets"));
        assert!(!filtered(&r, "author:alice repo:acme/gadgets"));
        // Each key checks its own field only.
        assert!(filtered(&r, "label:alice"));
        r.labels.clear();
        assert!(!filtered(&r, "label:alice"));
        assert!(filtered(&r, "author:"), "still being typed");
        r.author = None;
        assert!(!filtered(&r, "author:alice"));
    }

    #[test]
    fn filter_terms_render_once_and_quote_when_needed() {
        let label = |value: &str| FilterChip::new(Qualifier::Label, value);
        let author = |value: &str| FilterChip::new(Qualifier::Author, value);
        assert_eq!(with_filter("", &label("bug")), "label:bug");
        assert_eq!(with_filter("login  ", &label("bug")), "login label:bug");
        assert_eq!(
            with_filter("login label:Bug", &label("bug")),
            "login label:Bug"
        );
        assert_eq!(
            with_filter("login", &label("help wanted")),
            "login label:\"help wanted\""
        );
        // The same value under another key is another filter.
        assert_eq!(
            with_filter("label:alice", &author("alice")),
            "label:alice author:alice"
        );
        assert_eq!(
            with_filter("AUTHOR:Alice", &author("alice")),
            "AUTHOR:Alice"
        );
        assert_eq!(
            FilterChip::new(Qualifier::Repo, "acme/widgets").term(),
            "repo:acme/widgets"
        );
        assert!(author("Alice").same_as(&author("alice")));
        assert!(!author("alice").same_as(&label("alice")));
        let mut r = row(1, Category::Await);
        r.labels = vec!["Help Wanted".into()];
        assert!(has_label(&r, "help wanted"));
        assert!(!has_label(&r, "help"));
    }

    #[test]
    fn typed_filter_terms_become_chips_once_finished() {
        let label = |value: &str| FilterChip::new(Qualifier::Label, value);
        // Still typing: nothing is taken.
        for text in [
            "label",
            "label:",
            "label:bu",
            "fix label:bug",
            "label:\"help ",
        ] {
            assert_eq!(
                take_filter_chips(text, false),
                (vec![], text.to_owned()),
                "{text}"
            );
        }
        // A space finishes the last term; the rest keeps its trailing space.
        assert_eq!(
            take_filter_chips("fix label:bug ", false),
            (vec![label("bug")], "fix ".to_owned())
        );
        assert_eq!(
            take_filter_chips("label:\"help wanted\" ", false),
            (vec![label("help wanted")], String::new())
        );
        assert_eq!(
            take_filter_chips("label:area:gpui ", false),
            (vec![label("area:gpui")], String::new())
        );
        // Only the last term, never one in the middle of the text.
        assert_eq!(
            take_filter_chips("label:docs fix ", false),
            (vec![], "label:docs fix ".to_owned())
        );
        // Enter takes them all, an unclosed quote included.
        assert_eq!(
            take_filter_chips("label:docs fix label: LABEL:\"help wanted", true),
            (
                vec![label("docs"), label("help wanted")],
                "fix label:".to_owned()
            )
        );
        // Every key becomes a chip the same way.
        assert_eq!(
            take_filter_chips("fix author:alice ", false),
            (
                vec![FilterChip::new(Qualifier::Author, "alice")],
                "fix ".to_owned()
            )
        );
        assert_eq!(
            take_filter_chips("repo:acme/widgets author:\"bob\" fix", true),
            (
                vec![
                    FilterChip::new(Qualifier::Repo, "acme/widgets"),
                    FilterChip::new(Qualifier::Author, "bob"),
                ],
                "fix".to_owned()
            )
        );
        assert_eq!(take_filter_chips("fix", true), (vec![], "fix".to_owned()));
    }

    #[test]
    fn is_stale_keeps_prs_that_waited_the_configured_days() {
        let mut r = row(8, Category::Todo);
        r.title = "Fix login".into();
        assert!(!filtered(&r, "is:stale"), "not waiting");
        r.waiting_since = Some("2026-09-12T12:00:01Z".into());
        assert!(!filtered(&r, "is:stale"));
        r.waiting_since = Some("2026-09-12T12:00:00Z".into());
        assert!(filtered(&r, "IS:Stale login"));
        assert!(!filtered(&r, "is:fresh"));
        assert!(filtered(&r, "is:"), "still being typed");
        let week = StaleRule {
            after_days: 7,
            ..rule()
        };
        assert!(!matches_filter(&r, "is:stale", week));
        let chip = FilterChip::new(Qualifier::Is, "stale");
        assert_eq!(chip.term(), "is:stale");
        assert!(chip.matches(&r, rule()) && !chip.matches(&r, week));
        assert_eq!(
            take_filter_chips("is:stale ", false),
            (vec![chip], String::new())
        );
    }

    #[test]
    fn all_open_sends_labels_quoted_and_authors_as_user_or_app() {
        let chips = [
            FilterChip::new(Qualifier::Label, "Help Wanted"),
            FilterChip::new(Qualifier::Label, "help wanted"),
            FilterChip::new(Qualifier::Author, "Dependabot"),
            FilterChip::new(Qualifier::Label, r#"has "quotes""#),
            FilterChip::new(Qualifier::Author, "not a login"),
            FilterChip::new(Qualifier::Repo, "acme/widgets"),
            FilterChip::new(Qualifier::Is, "stale"),
        ];
        let remote = RemoteFilter::from_chips(&chips);
        // A bot's login is `dependabot`, and GitHub finds its PRs only as
        // `app/dependabot`, so each author goes both ways.
        assert_eq!(
            remote.qualifiers(),
            r#" label:"help wanted" author:dependabot author:app/dependabot"#
        );
        assert_eq!(
            remote,
            RemoteFilter::from_chips(&[chips[2].clone(), chips[1].clone()]),
            "the same filter in another order or case is the same search"
        );
        assert_eq!(
            local_only_terms("login fix", &chips, &remote),
            [
                r#"label:"has "quotes"""#,
                r#"author:"not a login""#,
                "repo:acme/widgets",
                "is:stale",
                "“login”",
                "“fix”",
            ]
        );
        assert!(RemoteFilter::from_chips(&chips[5..]).is_empty());
    }

    #[test]
    fn all_open_sends_a_bounded_number_of_labels_and_says_which_stayed_local() {
        let chips: Vec<FilterChip> = (0..MAX_REMOTE_TERMS + 2)
            .map(|n| FilterChip::new(Qualifier::Label, format!("area-{n:02}")))
            .collect();
        let remote = RemoteFilter::from_chips(&chips);
        assert_eq!(
            remote.qualifiers().matches("label:").count(),
            MAX_REMOTE_TERMS
        );
        assert_eq!(
            local_only_terms("", &chips, &remote),
            [r#"label:area-08"#, r#"label:area-09"#]
        );
    }
}
