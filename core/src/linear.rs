//! The Linear issue a PR is linked to, read from the comment Linear's GitHub
//! integration posts on it (its "linkback"):
//!
//! ```text
//! <!-- linear-linkback -->
//! <details>
//! <summary><a href="https://linear.app/acme/issue/ENG-123/fix-login">ENG-123 Fix login</a></summary>
//! …the issue's description…
//! ```
//!
//! Two bot accounts post it (`linear` and `linear-code`), and it is not always
//! the first comment, so a comment is recognised by its marker and by coming
//! from a bot, never by its position or its author's login.

use serde_json::{json, Value};

/// What marks Linear's linkback comment.
pub const LINKBACK_MARKER: &str = "<!-- linear-linkback -->";

/// A PR's Linear issue: its ID (`ENG-123`), its page and its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearTicket {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
}

/// The Linear issue in a PR node's comments ([`crate::github::query::TICKETS_QUERY`]):
/// the first comment from a bot that carries the linkback marker and a link to
/// a Linear issue.
pub fn linkback(node: &Value) -> Option<LinearTicket> {
    node.pointer("/comments/nodes")?
        .as_array()?
        .iter()
        .filter(|comment| {
            comment
                .pointer("/author/__typename")
                .and_then(Value::as_str)
                == Some("Bot")
        })
        .filter_map(|comment| comment.get("body").and_then(Value::as_str))
        .filter(|body| body.contains(LINKBACK_MARKER))
        .find_map(parse_linkback)
}

/// `node` with only what [`linkback`] reads: the linkback comment, cut to its
/// marker and its link to the issue, or no comment when it has none. A cache
/// keeps this instead of comments that can run to kilobytes each.
pub fn compact_node(node: &Value) -> Value {
    let comments: Vec<Value> = node
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|comment| {
            comment
                .pointer("/author/__typename")
                .and_then(Value::as_str)
                == Some("Bot")
        })
        .filter_map(|comment| {
            let body = comment.get("body").and_then(Value::as_str)?;
            if !body.contains(LINKBACK_MARKER) {
                return None;
            }
            let (link, _) = issue_link(body)?;
            Some(json!({
                "author": {"__typename": "Bot"},
                "body": format!("{LINKBACK_MARKER}\n<a {link}"),
            }))
        })
        .take(1)
        .collect();
    json!({
        "id": node.get("id").cloned().unwrap_or(Value::Null),
        "comments": {"nodes": comments},
    })
}

/// The first link to a Linear issue in a linkback body, and the issue's title
/// from the link's text. Linear wraps it in a `<summary>` when the issue has
/// a description and in a bare `<p>` when it has none; only the link counts.
fn parse_linkback(body: &str) -> Option<LinearTicket> {
    issue_link(body).map(|(_, ticket)| ticket)
}

/// [`parse_linkback`], with the link as written from `href` to `</a>`.
fn issue_link(body: &str) -> Option<(&str, LinearTicket)> {
    const HREF: &str = "href=\"";
    let mut rest = body;
    while let Some(start) = rest.find(HREF) {
        rest = &rest[start..];
        let url = &rest[HREF.len()..][..rest[HREF.len()..].find('"')?];
        let Some(id) = issue_id(url) else {
            rest = &rest[HREF.len()..];
            continue;
        };
        let end = rest.find("</a>")? + "</a>".len();
        let link = &rest[..end];
        let text = between(link, ">", "</a>").map(decode_entities);
        let title = text
            .as_deref()
            .map(|text| text.strip_prefix(&id).unwrap_or(text).trim().to_owned())
            .filter(|title| !title.is_empty());
        let ticket = LinearTicket {
            id,
            url: decode_entities(url),
            title,
        };
        return Some((link, ticket));
    }
    None
}

/// `ENG-123` from `https://linear.app/<workspace>/issue/ENG-123/<slug>`.
fn issue_id(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://linear.app/")?;
    let mut segments = path.split('/');
    let _workspace = segments.next()?;
    if segments.next()? != "issue" {
        return None;
    }
    let id = segments.next()?;
    let (team, number) = id.split_once('-')?;
    let valid = !team.is_empty()
        && team.chars().all(|c| c.is_ascii_alphanumeric())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit());
    valid.then(|| id.to_ascii_uppercase())
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// The HTML entities GitHub writes in a comment's link text and address.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(typename: &str, body: &str) -> Value {
        json!({"author": {"login": "someone", "__typename": typename}, "body": body})
    }

    /// The linkback `linear-code` posted on construct-mobile#822, third after
    /// a coverage report and a person's own "Linked to".
    const LINKBACK: &str = "<!-- linear-linkback -->\n<details>\n<summary><a href=\"https://linear.app/terabase/issue/IFR-479/remove-dev-and-dev-test-environment\">IFR-479 Remove dev &amp; dev-test environment</a></summary>\n<p>\n\nKeep Cognito and IoT Core in dev environment.\n</p>\n</details>\n<!-- linear-review-link -->\n<p><a href=\"https://linear.app/terabase/review/mob-000-chore\">Review in Linear</a></p>";

    #[test]
    fn the_linkback_is_found_by_its_marker_wherever_it_sits() {
        let node = json!({"id": "PR_1", "comments": {"nodes": [
            comment("Bot", "<!-- jest coverage report -->"),
            comment("User", "Linked to:  Linear: IFR-479"),
            comment("Bot", LINKBACK),
        ]}});
        assert_eq!(
            linkback(&node),
            Some(LinearTicket {
                id: "IFR-479".into(),
                url:
                    "https://linear.app/terabase/issue/IFR-479/remove-dev-and-dev-test-environment"
                        .into(),
                title: Some("Remove dev & dev-test environment".into()),
            })
        );
        // What a cache keeps reads the same, and is a fraction of the size.
        let compact = compact_node(&node);
        assert_eq!(linkback(&compact), linkback(&node));
        assert_eq!(compact["id"], "PR_1");
        assert!(compact.to_string().len() < LINKBACK.len());
    }

    /// An issue with no description: no `<details>`, the link in a bare `<p>`
    /// (construct-mobile, MOB-1580).
    #[test]
    fn a_linkback_without_a_description_reads_and_compacts_alike() {
        let body = "<!-- linear-linkback -->\n<p><a href=\"https://linear.app/terabase/issue/MOB-1580/customer-unable\">MOB-1580 Customer Unable to Edit</a></p>\n<!-- linear-review-link -->\n<p><a href=\"https://linear.app/terabase/review/mob-1580\">Review in Linear</a></p>";
        let node = json!({"id": "PR_2", "comments": {"nodes": [comment("Bot", body)]}});
        let ticket = linkback(&node).unwrap();
        assert_eq!(ticket.id, "MOB-1580");
        assert_eq!(ticket.title.as_deref(), Some("Customer Unable to Edit"));
        assert_eq!(linkback(&compact_node(&node)), Some(ticket));
    }

    #[test]
    fn only_a_bot_s_linkback_to_a_linear_issue_counts() {
        let pasted = json!({"comments": {"nodes": [comment("User", LINKBACK)]}});
        assert_eq!(linkback(&pasted), None, "a person quoting it");
        let review_only = json!({"comments": {"nodes": [comment(
            "Bot",
            "<!-- linear-linkback --><a href=\"https://linear.app/terabase/review/x\">Review</a>"
        )]}});
        assert_eq!(linkback(&review_only), None, "no issue link");
        assert_eq!(linkback(&json!({"comments": {"nodes": []}})), None);
        assert_eq!(compact_node(&pasted)["comments"]["nodes"], json!([]));
    }

    #[test]
    fn issue_ids_come_from_the_issue_path_only() {
        assert_eq!(
            issue_id("https://linear.app/acme/issue/eng-12/slug").as_deref(),
            Some("ENG-12")
        );
        assert_eq!(issue_id("https://linear.app/acme/project/ENG-12"), None);
        assert_eq!(issue_id("https://example.com/acme/issue/ENG-12"), None);
        assert_eq!(issue_id("https://linear.app/acme/issue/ENG-x"), None);
    }
}
