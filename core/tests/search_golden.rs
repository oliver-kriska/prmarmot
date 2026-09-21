//! Golden test for the board search grammar.
//!
//! The grammar moved out of the desktop app into `prmarmot_core::search` so
//! the iPad could not drift from it. This file is the contract that keeps them
//! together: every query in the matrix below is run against every row derived
//! from the same fixtures the parity goldens use, and the matching PR numbers
//! are compared with `tests/golden/search.json`.
//!
//! If this fails after a deliberate grammar change, regenerate with
//! `UPDATE_SEARCH_GOLDEN=1 cargo test -p prmarmot-core --test search_golden`
//! and read the diff before committing it. If it fails for any other reason,
//! fix the code, not the golden.

use chrono::{DateTime, Utc};
use prmarmot_core::board::{derive_rows, BoardConfig, BoardRow, IssueLinkRule, Mode};
use prmarmot_core::github::query::parse_search_response;
use prmarmot_core::search::{
    matches_filter, matches_search, take_filter_chips, with_filter, FilterChip, Qualifier,
    RemoteFilter, StaleRule,
};
use serde_json::{json, Map, Value};

const REPO: &str = "acme/widgets";
const ME: &str = "me";
const GOLDEN: &str = "tests/golden/search.json";

/// Fixed, so `is:stale` is a property of the fixture and not of the day the
/// suite runs. Chosen a little after the newest fixture timestamp.
fn stale_rule() -> StaleRule {
    StaleRule {
        now: DateTime::parse_from_rfc3339("2026-07-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        after_days: 3,
    }
}

/// Every shape of the grammar, in one list: free words, phrases, each
/// qualifier, case folding, quoting, partially typed terms, and combinations.
const QUERIES: &[&str] = &[
    "",
    "   ",
    "importer",
    "IMPORTER",
    "billing events",
    "\"drop legacy\"",
    "\"legacy drop\"",
    "#115",
    "label:bug",
    "label:Bug",
    "label:\"help wanted\"",
    "label:",
    "author:alice",
    "author:ALICE",
    "author:ali",
    "author:",
    "repo:acme/widgets",
    "repo:ACME/Widgets",
    "repo:widgets",
    "is:stale",
    "IS:Stale",
    "is:fresh",
    "is:",
    "label:bug is:stale",
    "author:alice repo:acme/widgets",
    "author:alice author:bob",
    "author:alice author:bob label:bug",
    "label:bug label:\"help wanted\"",
    "repo:acme/widgets repo:acme/other",
    "importer label:bug",
    "reviewer:alice",
    "\"label:bug\"",
    "label:\"help wanted",
];

fn config() -> BoardConfig {
    BoardConfig {
        issue_link: Some(
            IssueLinkRule::new("PROJ-[0-9]+", "https://tracker.example.test/issues/{id}").unwrap(),
        ),
        ..Default::default()
    }
}

fn load(path: &str) -> Value {
    let text = std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn rows_of(fixture: &str, mode: Mode) -> Vec<BoardRow> {
    let body = load(fixture);
    let (prs, _) = parse_search_response(&body).unwrap();
    derive_rows(&prs, mode, REPO, ME, &config())
}

/// `{query: {"authored": [numbers], "review": [numbers]}}`, plus the chip
/// round-trip, so both halves of the grammar are pinned in one file.
fn snapshot() -> Value {
    let authored = rows_of("tests/fixtures/authored_response.json", Mode::Authored);
    let review = rows_of("tests/fixtures/review_response.json", Mode::Review);
    let rule = stale_rule();

    let matched = |rows: &[BoardRow], query: &str| -> Vec<u64> {
        rows.iter()
            .filter(|row| matches_filter(row, query, rule))
            .map(|row| row.number)
            .collect()
    };

    let mut queries = Map::new();
    for query in QUERIES {
        // Enter turns finished qualifier terms into chips; the text left over
        // plus each chip's rendered term must together mean the same thing.
        let (chips, rest) = take_filter_chips(query, true);
        let rebuilt = chips
            .iter()
            .fold(rest.clone(), |text, chip| with_filter(&text, chip));
        queries.insert(
            (*query).to_owned(),
            json!({
                "authored": matched(&authored, query),
                "review": matched(&review, query),
                "chips": chips.iter().map(|chip| chip.term()).collect::<Vec<_>>(),
                "rest": rest,
                "rebuilt": rebuilt,
                "rebuiltAuthored": matched(&authored, &rebuilt),
            }),
        );
    }
    json!({
        "now": rule.now.to_rfc3339(),
        "staleAfterDays": rule.after_days,
        "authoredRows": authored.len(),
        "reviewRows": review.len(),
        "queries": Value::Object(queries),
    })
}

#[test]
fn search_grammar_matches_the_golden() {
    let ours = snapshot();
    let path = format!("{}/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_SEARCH_GOLDEN").is_some() {
        let mut text = serde_json::to_string_pretty(&ours).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("cannot read {GOLDEN}: {e}; regenerate with UPDATE_SEARCH_GOLDEN=1")
    }))
    .unwrap();

    let ours_queries = ours["queries"].as_object().unwrap();
    let golden_queries = golden["queries"].as_object().unwrap();
    for (query, expected) in golden_queries {
        let actual = ours_queries
            .get(query)
            .unwrap_or_else(|| panic!("query {query:?} is missing from the run"));
        assert_eq!(actual, expected, "query {query:?} drifted from the golden");
    }
    assert_eq!(
        ours_queries.len(),
        golden_queries.len(),
        "the query matrix changed; regenerate with UPDATE_SEARCH_GOLDEN=1"
    );
    assert_eq!(ours["now"], golden["now"]);
    assert_eq!(ours["authoredRows"], golden["authoredRows"]);
    assert_eq!(ours["reviewRows"], golden["reviewRows"]);
}

/// The golden would be vacuous if no query matched anything, or if every query
/// matched everything.
#[test]
fn the_matrix_actually_discriminates() {
    let snapshot = snapshot();
    let queries = snapshot["queries"].as_object().unwrap();
    let total = snapshot["authoredRows"].as_u64().unwrap();
    let counts: Vec<u64> = queries
        .values()
        .map(|entry| entry["authored"].as_array().unwrap().len() as u64)
        .collect();
    assert!(counts.contains(&0), "no query excludes everything");
    assert!(counts.contains(&total), "no query matches everything");
    assert!(
        counts.iter().any(|&n| n > 0 && n < total),
        "no query matches a strict subset"
    );
}

/// All open sends `label:` and `author:` chips to GitHub and still filters
/// the rows it gets back locally, so the two readings must be one: the same
/// chips must find the same PRs in My PRs and in All open. GitHub's reading
/// is written out here independently — every label, any one author, case
/// ignored — and checked row by row against the local grammar on both
/// fixtures, next to the exact qualifiers sent.
#[test]
fn a_chip_set_means_what_all_open_asks_github_for() {
    let chips = [
        FilterChip::new(Qualifier::Label, "Bug"),
        FilterChip::new(Qualifier::Author, "ALICE"),
        FilterChip::new(Qualifier::Author, "bob"),
    ];
    assert_eq!(
        RemoteFilter::from_chips(&chips).qualifiers(),
        r#" label:"bug" author:alice author:app/alice author:bob author:app/bob"#
    );
    let github = |row: &BoardRow| {
        let label = row.labels.iter().any(|l| l.eq_ignore_ascii_case("bug"));
        let author = row
            .author
            .as_deref()
            .is_some_and(|a| ["alice", "bob"].iter().any(|b| a.eq_ignore_ascii_case(b)));
        label && author
    };
    let mut rows = rows_of("tests/fixtures/authored_response.json", Mode::Authored);
    rows.extend(rows_of("tests/fixtures/review_response.json", Mode::Review));
    let mut matched = 0;
    for row in &rows {
        let local = matches_search(row, "", &chips, stale_rule());
        assert_eq!(local, github(row), "#{} reads differently", row.number);
        matched += usize::from(local);
    }
    assert!(
        matched > 0 && matched < rows.len(),
        "the fixture must tell the readings apart: {matched} of {}",
        rows.len()
    );
}
