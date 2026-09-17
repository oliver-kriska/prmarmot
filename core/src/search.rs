//! The board search grammar: free words plus `label:`, `author:`, `repo:` and
//! `is:stale`, ANDed together, matched against rows already loaded.
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

/// Case-insensitive AND search across the fields users scan in the board:
/// every free word must appear somewhere, and every qualifier (`label:`,
/// `author:`, `repo:`, `is:stale`) must match its field exactly. Filtering is
/// local; it must not trigger GitHub requests on each keystroke.
pub fn matches_filter(row: &BoardRow, query: &str, stale: StaleRule) -> bool {
    let terms = filter_terms(query);
    if terms.is_empty() {
        return true;
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
    terms.iter().all(|term| match term.qualifier {
        None => text.contains(&term.value.to_lowercase()),
        // `label:` alone is still being typed; it filters nothing yet.
        Some(qualifier) => term.value.is_empty() || qualifier.matches(row, &term.value, stale),
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
}
