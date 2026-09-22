//! Sharing a board group outside the app: one section rendered as a titled,
//! linked list (or table) for pasting into chat, docs, and trackers.
//!
//! Rich editors (Slack, Teams, Google Docs, email, Notion, Linear, Jira) read
//! the `text/html` flavour; terminals and plain editors read `text/plain`.
//! `Markdown` and `Urls` deliberately carry no HTML: GitHub and Obsidian
//! convert an HTML flavour when one is present, which would mangle Markdown
//! (double-wrapped links, links dropped from table cells).
//!
//! No `@` mentions: GitHub logins are not chat handles, and on GitHub an
//! `@login` pings the person.

use crate::board::{strip_note_glyphs, BoardRow, Category, Mode, ReviewState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareFormat {
    /// Linked bullet list: HTML plus a readable plain-text fallback.
    List,
    /// Markdown list, plain text only.
    Markdown,
    /// HTML table plus a Markdown pipe-table fallback.
    Table,
    /// PR URLs, one per line.
    Urls,
}

/// What goes on the clipboard: an optional rich flavour and the plain text
/// every destination can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePayload {
    pub html: Option<String>,
    pub plain: String,
}

/// Render one group (`title`, e.g. "Awaiting review") in `format`. Rows keep
/// the order given; the caller passes them in board display order.
pub fn share_group(
    title: &str,
    rows: &[BoardRow],
    mode: Mode,
    format: ShareFormat,
) -> SharePayload {
    let group = Group::new(title, rows, mode);
    match format {
        ShareFormat::List => SharePayload {
            html: Some(group.html_list()),
            plain: group.plain_list(),
        },
        ShareFormat::Markdown => SharePayload {
            html: None,
            plain: group.markdown_list(),
        },
        ShareFormat::Table => SharePayload {
            html: Some(group.html_table()),
            plain: group.markdown_table(),
        },
        ShareFormat::Urls => SharePayload {
            html: None,
            plain: rows
                .iter()
                .map(|row| row.url.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        },
    }
}

/// The share-ready text of each row, computed once for every format.
struct Item<'a> {
    row: &'a BoardRow,
    reference: String,
    context: String,
}

struct Group<'a> {
    heading: String,
    /// Shown after the heading when every row is from the same repository;
    /// otherwise each reference carries its own `owner/repo`.
    repo: Option<&'a str>,
    items: Vec<Item<'a>>,
}

impl<'a> Group<'a> {
    fn new(title: &str, rows: &'a [BoardRow], mode: Mode) -> Self {
        let repo = rows
            .first()
            .map(|first| first.repo.as_str())
            .filter(|repo| rows.iter().all(|row| row.repo == *repo));
        let items = rows
            .iter()
            .map(|row| Item {
                row,
                reference: if repo.is_some() {
                    format!("#{}", row.number)
                } else {
                    format!("{}#{}", row.repo, row.number)
                },
                context: row_context(row, mode),
            })
            .collect();
        Self {
            heading: format!("{title} ({})", rows.len()),
            repo,
            items,
        }
    }

    fn has_issues(&self) -> bool {
        self.items.iter().any(|item| item.row.issue.is_some())
    }

    fn html_heading(&self) -> String {
        let mut out = format!("<p><b>{}</b>", escape_html(&self.heading));
        if let Some(repo) = self.repo {
            out.push_str(&format!(" · {}", escape_html(repo)));
        }
        out.push_str("</p>");
        out
    }

    fn plain_heading(&self, bold: bool) -> String {
        let heading = if bold {
            format!("**{}**", escape_markdown(&self.heading))
        } else {
            self.heading.clone()
        };
        match self.repo {
            Some(repo) if bold => format!("{heading} · {}", escape_markdown(repo)),
            Some(repo) => format!("{heading} · {repo}"),
            None => heading,
        }
    }

    // Charset meta: Cocoa's HTML importer assumes Latin-1 without it, turning
    // "—" and non-ASCII titles into mojibake (browsers write it on copy too).
    fn html_list(&self) -> String {
        let mut out = format!("<meta charset=\"utf-8\">{}<ul>", self.html_heading());
        for item in &self.items {
            out.push_str(&format!(
                "<li><a href=\"{}\">{} {}</a>",
                escape_html(&item.row.url),
                escape_html(&item.reference),
                escape_html(&item.row.title)
            ));
            if let Some(issue) = &item.row.issue {
                out.push_str(&format!(
                    " · {}",
                    html_issue(issue, item.row.issue_url.as_deref())
                ));
            }
            if !item.context.is_empty() {
                out.push_str(&format!(" — {}", escape_html(&item.context)));
            }
            out.push_str("</li>");
        }
        out.push_str("</ul>");
        out
    }

    fn plain_list(&self) -> String {
        let mut out = self.plain_heading(false);
        out.push('\n');
        for item in &self.items {
            out.push_str(&format!("\n- {} {}", item.reference, item.row.title));
            if let Some(issue) = &item.row.issue {
                out.push_str(&format!(" · {issue}"));
            }
            if !item.context.is_empty() {
                out.push_str(&format!(" — {}", item.context));
            }
            out.push_str(&format!("\n  {}", item.row.url));
        }
        out
    }

    fn markdown_list(&self) -> String {
        let mut out = self.plain_heading(true);
        out.push('\n');
        for item in &self.items {
            out.push_str(&format!(
                "\n- [{} {}]({})",
                escape_markdown(&item.reference),
                escape_markdown(&item.row.title),
                markdown_url(&item.row.url)
            ));
            if let Some(issue) = &item.row.issue {
                out.push_str(&format!(
                    " · {}",
                    markdown_issue(issue, item.row.issue_url.as_deref())
                ));
            }
            if !item.context.is_empty() {
                out.push_str(&format!(" — {}", escape_markdown(&item.context)));
            }
        }
        out
    }

    fn html_table(&self) -> String {
        let issues = self.has_issues();
        let mut out = format!(
            "<meta charset=\"utf-8\">{}<table><thead><tr><th>PR</th><th>Title</th>{}<th>Status</th></tr></thead><tbody>",
            self.html_heading(),
            if issues { "<th>Issue</th>" } else { "" }
        );
        for item in &self.items {
            out.push_str(&format!(
                "<tr><td><a href=\"{}\">{}</a></td><td>{}</td>",
                escape_html(&item.row.url),
                escape_html(&item.reference),
                escape_html(&item.row.title)
            ));
            if issues {
                let cell = item
                    .row
                    .issue
                    .as_deref()
                    .map(|issue| html_issue(issue, item.row.issue_url.as_deref()))
                    .unwrap_or_default();
                out.push_str(&format!("<td>{cell}</td>"));
            }
            out.push_str(&format!("<td>{}</td></tr>", escape_html(&item.context)));
        }
        out.push_str("</tbody></table>");
        out
    }

    fn markdown_table(&self) -> String {
        let issues = self.has_issues();
        let mut out = self.plain_heading(true);
        out.push_str("\n\n");
        if issues {
            out.push_str("| PR | Title | Issue | Status |\n| --- | --- | --- | --- |");
        } else {
            out.push_str("| PR | Title | Status |\n| --- | --- | --- |");
        }
        for item in &self.items {
            out.push_str(&format!(
                "\n| [{}]({}) | {} |",
                escape_markdown(&item.reference),
                markdown_url(&item.row.url),
                escape_markdown(&item.row.title)
            ));
            if issues {
                let cell = item
                    .row
                    .issue
                    .as_deref()
                    .map(|issue| markdown_issue(issue, item.row.issue_url.as_deref()))
                    .unwrap_or_default();
                out.push_str(&format!(" {cell} |"));
            }
            out.push_str(&format!(" {} |", escape_markdown(&item.context)));
        }
        out
    }
}

/// One short phrase saying why the PR is in this group, without repeating the
/// group title: who it waits on (awaiting review), what blocks it (needs
/// action), or who wrote it (review queue, All open).
fn row_context(row: &BoardRow, mode: Mode) -> String {
    let note = strip_note_glyphs(&row.note);
    match (mode, row.category) {
        (Mode::Authored, Category::Await) => {
            let mut parts = Vec::new();
            if !row.reviews.is_empty() {
                let logins = row
                    .reviews
                    .iter()
                    .map(|review| review.login.as_deref().unwrap_or("deleted user"))
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("{} by {logins}", review_word(row.review_state)));
            }
            if !row.requested.is_empty() {
                parts.push(format!("waiting on {}", row.requested.join(", ")));
            }
            if parts.is_empty() {
                note
            } else {
                parts.join(" · ")
            }
        }
        (Mode::Authored, Category::Draft) => match note.strip_prefix("draft · ") {
            Some(problem) => problem.to_string(),
            None if note == "draft" => String::new(),
            None => note,
        },
        (Mode::Authored, _) => note,
        // All open's Note leaves the author to the table's Author column; a
        // copied group has no such column, so it says whose PR it is. One
        // asking you for a review reads as in the review queue.
        (Mode::AllOpen, category) if category != Category::Todo => {
            let by = format!("by {}", row.author.as_deref().unwrap_or("unknown author"));
            if note.is_empty() || note == "draft" {
                by
            } else {
                format!("{by} · {note}")
            }
        }
        (Mode::Review | Mode::AllOpen, _) => {
            let mut parts = vec![format!(
                "by {}",
                row.author.as_deref().unwrap_or("unknown author")
            )];
            // The group title already says these; anything else (CI red, new
            // commits since your review, your verdict) is worth sharing.
            if !matches!(
                note.as_str(),
                "" | "needs your review" | "available for review" | "draft (not ready)"
            ) {
                parts.push(note);
            }
            parts.join(" · ")
        }
    }
}

fn review_word(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::Changes => "changes requested",
        ReviewState::Commented => "commented",
        ReviewState::Waiting | ReviewState::None => "reviewed",
    }
}

fn html_issue(issue: &str, url: Option<&str>) -> String {
    match url {
        Some(url) => format!(
            "<a href=\"{}\">{}</a>",
            escape_html(url),
            escape_html(issue)
        ),
        None => escape_html(issue),
    }
}

fn markdown_issue(issue: &str, url: Option<&str>) -> String {
    match url {
        Some(url) => format!("[{}]({})", escape_markdown(issue), markdown_url(url)),
        None => escape_markdown(issue),
    }
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Backslash-escape the punctuation that would otherwise format, link, or
/// split a table cell (GFM allows escaping any ASCII punctuation).
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Keep a link destination from ending early at a space or parenthesis.
fn markdown_url(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Ci, ReviewSummary};

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
            title: format!("PR {number}"),
            issue: None,
            issue_url: None,
            author: Some("dana".into()),
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
            review_state: ReviewState::None,
            requested: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: String::new(),
            waiting_since: None,
            size: None,
            note: String::new(),
        }
    }

    fn awaiting(number: u64, title: &str, issue: &str, requested: &[&str]) -> BoardRow {
        let mut r = row(number, Category::Await);
        r.title = title.into();
        r.issue = Some(issue.into());
        r.issue_url = Some(format!("https://linear.app/acme/issue/{issue}"));
        r.requested = requested.iter().map(|login| login.to_string()).collect();
        r.note = "✅ awaiting review".into();
        r
    }

    #[test]
    fn list_carries_html_and_readable_plain_text() {
        let rows = vec![
            awaiting(
                11780,
                "Tag AppSignal samples",
                "ENA-10003",
                &["mkurkov", "abs"],
            ),
            awaiting(11730, "Upgrade Vega", "ENA-9904", &["josefrichter"]),
        ];
        let payload = share_group("Awaiting review", &rows, Mode::Authored, ShareFormat::List);
        assert_eq!(
            payload.html.as_deref(),
            Some(
                "<meta charset=\"utf-8\"><p><b>Awaiting review (2)</b> · acme/widgets</p><ul>\
                 <li><a href=\"https://github.com/acme/widgets/pull/11780\">#11780 Tag AppSignal samples</a> · \
                 <a href=\"https://linear.app/acme/issue/ENA-10003\">ENA-10003</a> — waiting on mkurkov, abs</li>\
                 <li><a href=\"https://github.com/acme/widgets/pull/11730\">#11730 Upgrade Vega</a> · \
                 <a href=\"https://linear.app/acme/issue/ENA-9904\">ENA-9904</a> — waiting on josefrichter</li></ul>"
            )
        );
        assert_eq!(
            payload.plain,
            "Awaiting review (2) · acme/widgets\n\
             \n- #11780 Tag AppSignal samples · ENA-10003 — waiting on mkurkov, abs\
             \n  https://github.com/acme/widgets/pull/11780\
             \n- #11730 Upgrade Vega · ENA-9904 — waiting on josefrichter\
             \n  https://github.com/acme/widgets/pull/11730"
        );
    }

    #[test]
    fn markdown_is_plain_text_only_and_escaped() {
        let mut r = awaiting(7, "Fix [flaky] *retry* | keys_here", "ENA-1", &["jo_do"]);
        r.issue_url = Some("https://tracker.example/issue (1)".into());
        let payload = share_group(
            "Awaiting review",
            &[r],
            Mode::Authored,
            ShareFormat::Markdown,
        );
        assert_eq!(
            payload.html, None,
            "GitHub/Obsidian would convert an HTML flavour"
        );
        assert_eq!(
            payload.plain,
            "**Awaiting review (1)** · acme/widgets\n\
             \n- [#7 Fix \\[flaky\\] \\*retry\\* \\| keys\\_here](https://github.com/acme/widgets/pull/7) · \
             [ENA-1](https://tracker.example/issue%20%281%29) — waiting on jo\\_do"
        );
    }

    #[test]
    fn html_escapes_titles_and_urls() {
        let mut r = row(3, Category::Action);
        r.title = "Render <b> & \"quotes\"".into();
        r.url = "https://github.com/acme/widgets/pull/3?a=1&b=2".into();
        r.note = "❌ CI failing".into();
        let html = share_group("Needs action", &[r], Mode::Authored, ShareFormat::List)
            .html
            .unwrap();
        assert!(html.contains(
            "<a href=\"https://github.com/acme/widgets/pull/3?a=1&amp;b=2\">#3 Render &lt;b&gt; &amp; &quot;quotes&quot;</a> — CI failing</li>"
        ));
    }

    #[test]
    fn mixed_repositories_qualify_each_reference() {
        let a = row(1, Category::Action);
        let mut b = row(2, Category::Action);
        b.repo = "acme/gadgets".into();
        b.url = "https://github.com/acme/gadgets/pull/2".into();
        let payload = share_group(
            "Needs action",
            &[a, b],
            Mode::Authored,
            ShareFormat::Markdown,
        );
        assert_eq!(
            payload.plain,
            "**Needs action (2)**\n\
             \n- [acme/widgets#1 PR 1](https://github.com/acme/widgets/pull/1)\
             \n- [acme/gadgets#2 PR 2](https://github.com/acme/gadgets/pull/2)"
        );
    }

    #[test]
    fn table_drops_issue_column_when_no_row_has_one() {
        let mut r = row(5, Category::Action);
        r.note = "⚠️ no reviewers · 🟡 2 unresolved comments".into();
        let payload = share_group("Needs action", &[r], Mode::Authored, ShareFormat::Table);
        assert_eq!(
            payload.plain,
            "**Needs action (1)** · acme/widgets\n\n\
             | PR | Title | Status |\n| --- | --- | --- |\n\
             | [#5](https://github.com/acme/widgets/pull/5) | PR 5 | no reviewers · 2 unresolved comments |"
        );
        let html = payload.html.unwrap();
        assert!(html.contains("<tr><th>PR</th><th>Title</th><th>Status</th></tr>"));
        assert!(html.contains(
            "<tr><td><a href=\"https://github.com/acme/widgets/pull/5\">#5</a></td><td>PR 5</td><td>no reviewers · 2 unresolved comments</td></tr>"
        ));
    }

    #[test]
    fn table_leaves_issue_cell_empty_for_rows_without_one() {
        let rows = vec![
            awaiting(1, "One", "ENA-1", &["abs"]),
            row(2, Category::Await),
        ];
        let plain = share_group("Awaiting review", &rows, Mode::Authored, ShareFormat::Table).plain;
        assert!(plain.contains("| PR | Title | Issue | Status |"));
        assert!(plain.ends_with("| [#2](https://github.com/acme/widgets/pull/2) | PR 2 |  |  |"));
    }

    #[test]
    fn urls_are_one_per_line_without_html() {
        let payload = share_group(
            "Drafts",
            &[row(1, Category::Draft), row(2, Category::Draft)],
            Mode::Authored,
            ShareFormat::Urls,
        );
        assert_eq!(payload.html, None);
        assert_eq!(
            payload.plain,
            "https://github.com/acme/widgets/pull/1\nhttps://github.com/acme/widgets/pull/2"
        );
    }

    #[test]
    fn context_explains_the_row_without_repeating_the_group() {
        let mut approved = row(1, Category::Await);
        approved.review_state = ReviewState::Approved;
        approved.reviews = vec![ReviewSummary {
            login: Some("alice".into()),
            state: "APPROVED".into(),
            submitted_at: None,
        }];
        approved.requested = vec!["bob".into()];
        assert_eq!(
            row_context(&approved, Mode::Authored),
            "approved by alice · waiting on bob"
        );

        let mut unrequested = row(2, Category::Await);
        unrequested.note = "✅ awaiting review".into();
        assert_eq!(row_context(&unrequested, Mode::Authored), "awaiting review");

        let mut draft = row(3, Category::Draft);
        draft.note = "· draft".into();
        assert_eq!(row_context(&draft, Mode::Authored), "");
        draft.note = "🔴 draft · CI failing".into();
        assert_eq!(row_context(&draft, Mode::Authored), "CI failing");

        let mut todo = row(4, Category::Todo);
        todo.note = "🔵 needs your review".into();
        assert_eq!(row_context(&todo, Mode::Review), "by dana");
        todo.note = "⚠️ CI red — maybe wait for green".into();
        todo.author = None;
        assert_eq!(
            row_context(&todo, Mode::Review),
            "by unknown author · CI red — maybe wait for green"
        );

        // The group title already says "draft"; the neutral glyph must not leak.
        let mut review_draft = row(5, Category::Draft);
        review_draft.note = "· draft (not ready)".into();
        assert_eq!(row_context(&review_draft, Mode::Review), "by dana");

        // All open's Note does not name the author; a shared line must.
        let mut theirs = row(12, Category::Action);
        theirs.author = Some("dana".into());
        theirs.note = "merge conflict · CI failing".into();
        assert_eq!(
            row_context(&theirs, Mode::AllOpen),
            "by dana · merge conflict · CI failing"
        );
        theirs.category = Category::Draft;
        theirs.note = "draft".into();
        assert_eq!(row_context(&theirs, Mode::AllOpen), "by dana");
    }
}
